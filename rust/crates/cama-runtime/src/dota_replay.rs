//! Bounded replay archival, independent from match settlement.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ArchivedReplay {
    pub match_id: String,
    pub filename: String,
    pub bytes: u64,
    pub sha256: String,
    pub archived_at: i64,
}

#[derive(Clone)]
pub struct ReplayArchive {
    pub directory: PathBuf,
    pub maximum_bytes: u64,
    pub retention_days: u64,
}

impl ReplayArchive {
    pub async fn download(
        &self,
        match_id: u64,
        cluster: u32,
        salt: u32,
    ) -> Result<ArchivedReplay, String> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.download_blocking(match_id, cluster, salt))
            .await
            .map_err(|_| "replay archive task failed".to_owned())?
    }

    fn download_blocking(
        &self,
        match_id: u64,
        cluster: u32,
        salt: u32,
    ) -> Result<ArchivedReplay, String> {
        if match_id == 0 || cluster == 0 || salt == 0 {
            return Err("replay metadata is incomplete".to_owned());
        }
        fs::create_dir_all(&self.directory).map_err(|_| "cannot create replay directory")?;
        self.prune()?;
        let filename = format!("{match_id}.dem.bz2");
        let path = self.directory.join(&filename);
        if path.exists() {
            return inspect_existing(&path, match_id, self.maximum_bytes);
        }
        // URL parts are numeric GC metadata. Never follow redirects to an
        // arbitrary host or accept a user-supplied replay URL.
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot construct replay client")?;
        let response = client
            .get(replay_url(match_id, cluster, salt))
            .send()
            .map_err(|_| "Valve replay download unavailable")?;
        if !response.status().is_success() {
            return Err(format!(
                "Valve replay CDN returned {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.maximum_bytes)
        {
            return Err("replay exceeds archive size limit".to_owned());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)
            .map_err(|_| "cannot create replay staging file")?;
        let mut reader = response.take(self.maximum_bytes + 1);
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut first: Vec<u8> = Vec::new();
        let mut buffer = [0_u8; 65536];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| "replay download interrupted")?;
            if count == 0 {
                break;
            }
            size += count as u64;
            if size > self.maximum_bytes {
                return Err("replay exceeds archive size limit".to_owned());
            }
            first.extend(
                buffer[..count]
                    .iter()
                    .copied()
                    .take(3_usize.saturating_sub(first.len())),
            );
            hash.update(&buffer[..count]);
            temporary
                .write_all(&buffer[..count])
                .map_err(|_| "cannot write replay archive")?;
        }
        if first != b"BZh" {
            return Err("replay CDN did not return a bzip2 replay".to_owned());
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| "cannot sync replay archive")?;
        temporary
            .persist_noclobber(&path)
            .map_err(|_| "cannot finalize replay archive")?;
        let archive = ArchivedReplay {
            match_id: match_id.to_string(),
            filename,
            bytes: size,
            sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
            archived_at: chrono::Utc::now().timestamp(),
        };
        Ok(archive)
    }

    fn prune(&self) -> Result<(), String> {
        if self.retention_days == 0 {
            return Ok(());
        }
        let max_age = Duration::from_secs(self.retention_days.saturating_mul(86400));
        for entry in fs::read_dir(&self.directory).map_err(|_| "cannot list replay archives")? {
            let entry = entry.map_err(|_| "cannot inspect replay archive")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_archive_name(name)
                || !entry
                    .file_type()
                    .map_err(|_| "cannot inspect replay file")?
                    .is_file()
            {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|time| SystemTime::now().duration_since(time).ok())
                .is_some_and(|age| age > max_age);
            if old {
                fs::remove_file(entry.path()).map_err(|_| "cannot prune expired replay")?;
            }
        }
        Ok(())
    }
}

fn is_archive_name(name: &str) -> bool {
    name.strip_suffix(".dem.bz2")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn replay_url(match_id: u64, cluster: u32, salt: u32) -> String {
    // Valve's replay delivery uses HTTP, including the Perfect World clusters;
    // see odota/core svc/util/utility.ts buildReplayUrl. The file is never
    // executed and its content is not used as evidence for match settlement.
    let domain = if matches!(cluster, 413 | 415 | 417) {
        "dota2.com.cn"
    } else {
        "valve.net"
    };
    format!("http://replay{cluster}.{domain}/570/{match_id}_{salt}.dem.bz2")
}

fn inspect_existing(path: &Path, match_id: u64, limit: u64) -> Result<ArchivedReplay, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "cannot inspect stored replay")?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("stored replay is not a bounded regular file".to_owned());
    }
    let mut file = File::open(path).map_err(|_| "cannot open stored replay")?;
    let mut signature = [0; 3];
    file.read_exact(&mut signature)
        .map_err(|_| "stored replay is truncated")?;
    if signature != *b"BZh" {
        return Err("stored replay has invalid compression header".to_owned());
    }
    let mut hash = Sha256::new();
    hash.update(signature);
    let mut buffer = [0; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "cannot read stored replay")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(ArchivedReplay {
        match_id: match_id.to_string(),
        filename: format!("{match_id}.dem.bz2"),
        bytes: metadata.len(),
        sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        archived_at: metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|t| t.as_secs() as i64)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_recovers_a_completed_archive_without_network_and_rejects_invalid_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("123.dem.bz2");
        fs::write(&path, b"BZh9fixture").unwrap();
        let archive = ReplayArchive {
            directory: directory.path().into(),
            maximum_bytes: 100,
            retention_days: 0,
        };
        let saved = archive.download_blocking(123, 1, 2).unwrap();
        assert_eq!(saved.bytes, 11);
        assert_eq!(saved.sha256.len(), 64);
        fs::write(&path, b"error page").unwrap();
        assert!(archive.download_blocking(123, 1, 2).is_err());
        assert!(!is_archive_name("../123.dem.bz2"));
        assert!(!is_archive_name("notes.txt"));
        assert_eq!(
            replay_url(123, 413, 456),
            "http://replay413.dota2.com.cn/570/123_456.dem.bz2"
        );
    }
}
