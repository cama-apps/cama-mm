//! Offline diagnostic only: decode a downloaded, already decompressed .meta
//! file using the exact private metadata key returned by the GC for that match.
//! Usage: decode_match_metadata MATCH_ID FILE.meta COORDINATES.json OUTPUT.json
//! Uses the same bounded Rust bzip2/zstd decoder as production.
//! No Steam connection, HTTP requests, or key discovery.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Value, json};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_dota2::dota_match_metadata::CDOTAMatchMetadataFile;

type Error = Box<dyn std::error::Error>;
const MAX_META_BYTES: usize = 16 * 1024 * 1024;
const MAX_COORDINATE_BYTES: usize = 1024 * 1024;
const MAX_GRAPH_SAMPLES: usize = 100_000;

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, Error> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit as u64 {
        return Err("input is not a regular file within the diagnostic size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("input exceeded the diagnostic size limit while reading".into());
    }
    Ok(bytes)
}

fn coordinates_for_match(coordinates: &Value, match_id: u64) -> Result<&Value, Error> {
    let candidates: Vec<&Value> = match coordinates {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => vec![coordinates],
        _ => return Err("coordinates must be an object or array of probe records".into()),
    };
    let matching: Vec<_> = candidates
        .into_iter()
        .filter(|entry| entry.get("match_id").and_then(Value::as_u64) == Some(match_id))
        .collect();
    if matching.len() != 1 {
        return Err("coordinates must contain exactly one entry for the requested match".into());
    }
    Ok(matching[0])
}

fn validate_graph(graph: &[f32]) -> Result<(), Error> {
    if graph.is_empty() || graph.len() > MAX_GRAPH_SAMPLES {
        return Err("private metadata has no graph or exceeds the graph sample limit".into());
    }
    if graph
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=100.0).contains(value))
    {
        return Err("private metadata graph contains invalid raw probability values".into());
    }
    Ok(())
}

#[cfg(unix)]
fn private_output(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_output(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::other(
        "run this diagnostic on the isolated Linux test host",
    ))
}

fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(
            "usage: decode_match_metadata MATCH_ID FILE.meta COORDINATES.json OUTPUT.json".into(),
        );
    }
    let match_id: u64 = args[0]
        .to_str()
        .ok_or("match ID must be UTF-8 digits")?
        .parse()?;
    if match_id == 0 {
        return Err("match ID must be positive".into());
    }
    let metadata = CDOTAMatchMetadataFile::parse_from_bytes(&read_bounded(
        Path::new(&args[1]),
        MAX_META_BYTES,
    )?)?;
    if metadata.match_id != Some(match_id) {
        return Err("metadata file does not match the requested Valve match ID".into());
    }
    let coordinates: Value =
        serde_json::from_slice(&read_bounded(Path::new(&args[2]), MAX_COORDINATE_BYTES)?)?;
    let coordinates = coordinates_for_match(&coordinates, match_id)?;
    let key = coordinates
        .get("private_metadata_key")
        .and_then(Value::as_u64)
        .and_then(|key| u32::try_from(key).ok())
        .ok_or("matching coordinates have no valid u32 private metadata key")?;
    let graph =
        cama_steam::metadata::decode_win_probability(&metadata.write_to_bytes()?, match_id, key)?;
    validate_graph(&graph.values)?;
    let public_counts = metadata
        .metadata
        .as_ref()
        .map(|public| {
            public
                .teams
                .iter()
                .map(|team| {
                    json!({
                        "dota_team": team.dota_team,
                        "graph_experience_samples": team.graph_experience.len(),
                        "graph_gold_earned_samples": team.graph_gold_earned.len(),
                        "graph_net_worth_samples": team.graph_net_worth.len(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let result = json!({
        "match_id": match_id,
        "metadata_version": metadata.version,
        "duration_seconds": coordinates.get("duration").and_then(Value::as_u64),
        "raw_graph_values": graph.values,
        "sample_index": (0..graph.values.len()).collect::<Vec<_>>(),
        "public_team_graph_sample_counts": public_counts,
        "sample_timing": "unverified; indices are not timestamps",
    });
    let mut output = private_output(Path::new(&args[3]))?;
    serde_json::to_writer(&mut output, &result)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!(
        "match_id={match_id} decoded {} raw graph samples; private output written",
        graph.values.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_require_one_exact_match_and_preserve_unsigned_keys() {
        let coordinates = json!([{"match_id":1,"private_metadata_key":u32::MAX},{"match_id":2,"private_metadata_key":5}]);
        assert_eq!(
            coordinates_for_match(&coordinates, 1).unwrap()["private_metadata_key"],
            json!(u32::MAX)
        );
        assert!(coordinates_for_match(&coordinates, 3).is_err());
        assert!(coordinates_for_match(&json!([{"match_id":1},{"match_id":1}]), 1).is_err());
    }

    #[test]
    fn graph_validation_rejects_nonfinite_and_out_of_range_without_rescaling() {
        assert!(validate_graph(&[0.0, 41.35, 98.95, 100.0]).is_ok());
        for graph in [&[][..], &[f32::NAN], &[f32::INFINITY], &[-0.1], &[100.1]] {
            assert!(validate_graph(graph).is_err());
        }
    }
}
