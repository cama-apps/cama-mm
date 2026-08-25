//! Disk-backed cache for the images attached to classic Dota trivia questions.
//!
//! The path mapping, ten-second HTTP timeout, eager warm, and remote-image
//! fallback mirror `services/trivia_image_cache.py`. Callers perform these
//! blocking operations on a worker thread.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::Client;
use thiserror::Error;

use crate::trivia_questions::TriviaCatalog;

pub const PRODUCTION_TRIVIA_CACHE_PATH: &str = ".cache/trivia";
pub const STEAM_IMAGE_CACHE_ROOT_ENV: &str = "STEAM_IMAGE_CACHE_ROOT";
pub const DEFAULT_STEAM_IMAGE_CACHE_ROOT: &str = ".cache";

/// Resolve the shared root for Steam-hosted static images. Deployments can
/// point this at a durable volume while local runs retain the historical
/// `.cache` location.
#[must_use]
pub fn steam_image_cache_root_from(value: Option<OsString>) -> PathBuf {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STEAM_IMAGE_CACHE_ROOT))
}

#[must_use]
pub fn production_steam_image_cache_root() -> PathBuf {
    steam_image_cache_root_from(std::env::var_os(STEAM_IMAGE_CACHE_ROOT_ENV))
}

#[must_use]
pub fn production_trivia_cache_path() -> PathBuf {
    production_steam_image_cache_root().join("trivia")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedTriviaImage {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TriviaCacheWarmReport {
    pub newly_cached: usize,
    pub total_urls: usize,
}

#[derive(Debug, Error)]
pub enum TriviaImageCacheError {
    #[error("could not construct trivia image HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("trivia image cache lock was poisoned")]
    Lock,
    #[error("trivia image URL {url:?} could not be downloaded: {source}")]
    Download {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("trivia image cache I/O failed at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A process-wide cache instance. The mutex closes the same-file race between
/// READY warming and an action-time fallback download.
#[derive(Debug)]
pub struct TriviaImageCache {
    root: PathBuf,
    writes: Mutex<()>,
}

impl TriviaImageCache {
    pub fn production() -> Result<Self, TriviaImageCacheError> {
        Self::new(production_trivia_cache_path())
    }

    pub fn new(root: impl AsRef<Path>) -> Result<Self, TriviaImageCacheError> {
        Ok(Self {
            root: root.as_ref().to_path_buf(),
            writes: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn path_for_url(&self, url: &str) -> PathBuf {
        let path = url
            .split_once("://")
            .map_or(url, |(_, remainder)| remainder)
            .split_once('/')
            .map_or("", |(_, path)| path)
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        let parts = path.split('/').collect::<Vec<_>>();
        if let Some(index) = parts.iter().position(|part| *part == "dota_react") {
            let relative = parts[index + 1..]
                .iter()
                .filter(|part| !part.is_empty() && **part != "." && **part != "..")
                .collect::<PathBuf>();
            if relative.file_name().is_some() {
                return self.root.join(relative);
            }
        }

        let hash = md5_hex(url.as_bytes());
        let extension = Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .map_or_else(|| ".png".to_owned(), |extension| format!(".{extension}"));
        self.root.join("other").join(format!("{hash}{extension}"))
    }

    /// Return an attachment, downloading once when the warm has not populated
    /// this URL yet. A caller may deliberately swallow an error and retain the
    /// question's remote thumbnail URL, matching Python.
    pub fn get_or_download(&self, url: &str) -> Result<CachedTriviaImage, TriviaImageCacheError> {
        let path = self.path_for_url(url);
        if !path.exists() {
            let client = http_client()?;
            self.ensure_cached(url, &path, &client)?;
        }
        let bytes = fs::read(&path).map_err(|source| TriviaImageCacheError::Io {
            path: path.clone(),
            source,
        })?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("trivia.png")
            .to_owned();
        Ok(CachedTriviaImage { filename, bytes })
    }

    pub fn warm_catalog(
        &self,
        catalog: &TriviaCatalog,
    ) -> Result<TriviaCacheWarmReport, TriviaImageCacheError> {
        let urls = catalog
            .heroes
            .iter()
            .filter_map(|hero| hero.image_url.as_deref())
            .chain(
                catalog
                    .abilities
                    .iter()
                    .filter_map(|ability| ability.icon_url.as_deref()),
            )
            .chain(
                catalog
                    .items
                    .iter()
                    .filter_map(|item| item.icon_url.as_deref()),
            )
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let mut newly_cached = 0;
        let client = http_client()?;
        for url in &urls {
            let path = self.path_for_url(url);
            if !path.exists() && self.ensure_cached(url, &path, &client).is_ok() {
                newly_cached += 1;
            }
        }
        Ok(TriviaCacheWarmReport {
            newly_cached,
            total_urls: urls.len(),
        })
    }

    fn ensure_cached(
        &self,
        url: &str,
        path: &Path,
        client: &Client,
    ) -> Result<(), TriviaImageCacheError> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| TriviaImageCacheError::Lock)?;
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| TriviaImageCacheError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let bytes = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::bytes)
            .map_err(|source| TriviaImageCacheError::Download {
                url: url.to_owned(),
                source,
            })?;
        fs::write(path, &bytes).map_err(|source| TriviaImageCacheError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn http_client() -> Result<Client, TriviaImageCacheError> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(TriviaImageCacheError::Client)
}

/// Python uses MD5 solely as a deterministic filename for non-Steam URLs.
/// Keeping a tiny implementation here avoids broadening the runtime dependency
/// surface for a non-cryptographic cache key.
fn md5_hex(input: &[u8]) -> String {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const CONSTANTS: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_le_bytes());

    let mut digest = [0x6745_2301_u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (word, bytes) in words.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [mut a, mut b, mut c, mut d] = digest;
        for index in 0..64 {
            let (function, word_index) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let rotated = a
                .wrapping_add(function)
                .wrapping_add(CONSTANTS[index])
                .wrapping_add(words[word_index])
                .rotate_left(SHIFTS[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        digest[0] = digest[0].wrapping_add(a);
        digest[1] = digest[1].wrapping_add(b);
        digest[2] = digest[2].wrapping_add(c);
        digest[3] = digest[3].wrapping_add(d);
    }

    let bytes = digest.into_iter().flat_map(u32::to_le_bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

#[cfg(test)]
#[path = "trivia_image_cache/tests.rs"]
mod tests;
