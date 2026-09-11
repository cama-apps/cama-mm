//! Bounded metadata-only download and private postgame graph decoding.
//! No game-server connection or replay download is involved.
mod crypto;

use std::io::Read;
use std::time::Duration;

use serde_json::{Value, json};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_dota2::dota_match_metadata::{
    CDOTAMatchMetadataFile, CDOTAMatchPrivateMetadata,
};
use thiserror::Error;

use crate::{DotaSteamClient, MatchMetadataLocation};

const MAX_DOWNLOAD: usize = 4 * 1024 * 1024;
const MAX_METADATA: usize = 16 * 1024 * 1024;
const MAX_PRIVATE: usize = 64 * 1024 * 1024;
const MAX_SAMPLES: usize = 4096;

/// Errors intentionally omit download URLs, keys, and response bodies.
#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("metadata coordinates are unavailable")]
    Unavailable,
    #[error("metadata request failed")]
    Request,
    #[error("metadata exceeds its size limit")]
    TooLarge,
    #[error("metadata has an unsupported or invalid compressed stream")]
    Compression,
    #[error("metadata is not a valid protobuf")]
    Protobuf,
    #[error("metadata belongs to a different match")]
    WrongMatch,
    #[error("metadata private block could not be decoded with the supplied key")]
    PrivateBlock,
    #[error("metadata has no valid win-probability graph")]
    InvalidGraph,
}

/// Valve's ordered percentages. Their sample-to-clock mapping is unverified.
#[derive(Clone, Debug, PartialEq)]
pub struct WinProbabilityGraph {
    pub match_id: u64,
    pub values: Vec<f32>,
}

impl WinProbabilityGraph {
    pub fn statistics_value(&self) -> Value {
        json!({"match_id":self.match_id,"values":self.values,
            "axis":"sample_index","unit":"percent","side":"radiant",
            "source":"valve_metadata"})
    }
}

impl DotaSteamClient {
    /// Optional enrichment; callers should impose their overall settlement budget.
    pub async fn postgame_win_probability(
        &self,
        match_id: u64,
    ) -> Result<WinProbabilityGraph, MetadataError> {
        let location = self
            .match_metadata_location(match_id)
            .await
            .map_err(|_| MetadataError::Unavailable)?;
        fetch_win_probability(&location).await
    }
}

fn metadata_url(location: &MatchMetadataLocation) -> Result<String, MetadataError> {
    let cluster = location
        .cluster
        .filter(|cluster| *cluster > 0 && *cluster <= 999)
        .ok_or(MetadataError::Unavailable)?;
    let salt = location.replay_salt.ok_or(MetadataError::Unavailable)?;
    if location.match_id == 0 || location.private_metadata_key().is_none() {
        return Err(MetadataError::Unavailable);
    }
    // Numeric components only. No caller-supplied hosts, paths, or redirects.
    Ok(format!(
        "https://replay{cluster}.valve.net/570/{}_{salt}.meta.bz2",
        location.match_id
    ))
}

pub async fn fetch_win_probability(
    location: &MatchMetadataLocation,
) -> Result<WinProbabilityGraph, MetadataError> {
    let url = metadata_url(location)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|_| MetadataError::Request)?;
    let response = client.get(&url).send().await;
    // Valve's verified replay CDN currently fails TLS for some clusters.
    // Only this exact numeric Valve URL may fall back to HTTP. The key is
    // never included in either request; redirects remain disabled.
    let mut response = match response {
        Ok(response) => response,
        Err(_) => client
            .get(url.replacen("https://", "http://", 1))
            .send()
            .await
            .map_err(|_| MetadataError::Request)?,
    };
    if !response.status().is_success() {
        return Err(MetadataError::Request);
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_DOWNLOAD as u64)
    {
        return Err(MetadataError::TooLarge);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| MetadataError::Request)? {
        if data.len().saturating_add(chunk.len()) > MAX_DOWNLOAD {
            return Err(MetadataError::TooLarge);
        }
        data.extend_from_slice(&chunk);
    }
    let match_id = location.match_id;
    let key = location
        .private_metadata_key()
        .ok_or(MetadataError::Unavailable)?;
    tokio::task::spawn_blocking(move || {
        let metadata = decompress(&data, MAX_METADATA)?;
        decode_win_probability(&metadata, match_id, key)
    })
    .await
    .map_err(|_| MetadataError::PrivateBlock)?
}

fn bounded_read(reader: impl Read, limit: usize) -> Result<Vec<u8>, MetadataError> {
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| MetadataError::Compression)?;
    if output.len() > limit {
        return Err(MetadataError::TooLarge);
    }
    Ok(output)
}

fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, MetadataError> {
    if data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder =
            zstd::stream::read::Decoder::new(data).map_err(|_| MetadataError::Compression)?;
        decoder
            .window_log_max(26)
            .map_err(|_| MetadataError::Compression)?;
        bounded_read(decoder.single_frame(), limit)
    } else if data.len() >= 4 && &data[..3] == b"BZh" && (b'1'..=b'9').contains(&data[3]) {
        bounded_read(bzip2::read::BzDecoder::new(data), limit)
    } else {
        Err(MetadataError::Compression)
    }
}

/// Decode an already decompressed metadata file using only its GC-issued key.
/// Exposed to offline diagnostics; it performs no network or filesystem work.
pub fn decode_win_probability(
    data: &[u8],
    match_id: u64,
    key: u32,
) -> Result<WinProbabilityGraph, MetadataError> {
    if data.len() > MAX_METADATA {
        return Err(MetadataError::TooLarge);
    }
    let metadata =
        CDOTAMatchMetadataFile::parse_from_bytes(data).map_err(|_| MetadataError::Protobuf)?;
    if match_id == 0 || metadata.match_id != Some(match_id) {
        return Err(MetadataError::WrongMatch);
    }
    let encrypted = metadata
        .private_metadata
        .as_deref()
        .ok_or(MetadataError::PrivateBlock)?;
    let decrypted = crypto::decrypt(encrypted, key).map_err(|_| MetadataError::PrivateBlock)?;
    let private_data = decompress(
        decrypted.get(4..).ok_or(MetadataError::PrivateBlock)?,
        MAX_PRIVATE,
    )?;
    let private = CDOTAMatchPrivateMetadata::parse_from_bytes(&private_data)
        .map_err(|_| MetadataError::Protobuf)?;
    let values = private.graph_win_probability;
    if values.is_empty()
        || values.len() > MAX_SAMPLES
        || values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v))
    {
        return Err(MetadataError::InvalidGraph);
    }
    Ok(WinProbabilityGraph { match_id, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/published_match_8010031109.meta");
    // Public demonstration key from the Apache-2.0 source repository main.py.
    const PUBLISHED_KEY: u32 = 1812757917;

    #[test]
    fn published_fixture_decodes_and_rejects_wrong_match_or_key() {
        let graph = decode_win_probability(FIXTURE, 8010031109, PUBLISHED_KEY).unwrap();
        assert_eq!(graph.values.len(), 46);
        assert!((graph.values[0] - 47.628925).abs() < 0.001);
        assert!((graph.values[45] - 98.95983).abs() < 0.001);
        assert_eq!(graph.statistics_value()["axis"], "sample_index");
        assert!(matches!(
            decode_win_probability(FIXTURE, 1, PUBLISHED_KEY),
            Err(MetadataError::WrongMatch)
        ));
        assert!(decode_win_probability(FIXTURE, 8010031109, PUBLISHED_KEY ^ 1).is_err());
    }

    #[test]
    fn both_compression_formats_preserve_data_and_enforce_output_limit() {
        let zstd = zstd::stream::encode_all(FIXTURE, 1).unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        bz.write_all(FIXTURE).unwrap();
        for compressed in [zstd, bz.finish().unwrap()] {
            assert_eq!(decompress(&compressed, MAX_METADATA).unwrap(), FIXTURE);
            assert!(matches!(
                decompress(&compressed, 32),
                Err(MetadataError::TooLarge)
            ));
        }
        assert!(decompress(b"not compressed", MAX_METADATA).is_err());
        assert!(matches!(
            bounded_read(&b"12345"[..], 4),
            Err(MetadataError::TooLarge)
        ));
    }
}
