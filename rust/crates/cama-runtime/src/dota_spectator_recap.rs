//! Bounded on-disk screenshot archive and optional post-summary recap outbox.
//! Recaps never participate in settlement and never publish into a live channel.
mod encoder;
mod png;

use crate::{
    InteractionAttachment, InteractionEmbed, InteractionResponse,
    discord_transport::{DiscordMessage, DiscordTransport},
};
use cama_db::{core_repositories::MatchRepository, match_runtime::PendingMatchRepository};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_FRAMES: usize = 240;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
const RETENTION: i64 = 24 * 60 * 60;
// All file mutations are short blocking operations. Encoding releases this lock.
static FILES: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Screenshot {
    clock: i64,
    file: String,
    bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    match_id: i64,
    channel_id: u64,
    summary_message_id: u64,
    queued_at: i64,
    next_attempt: i64,
    delivered: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Archive {
    guild_id: i64,
    pending_id: i64,
    valve_id: u64,
    updated_at: i64,
    interval: i64,
    frames: Vec<Screenshot>,
    job: Option<Job>,
}
fn root(database: &Path) -> PathBuf {
    database.with_extension("spectator-recaps")
}
fn directory(database: &Path, guild: i64, pending: i64) -> Result<PathBuf, String> {
    if guild <= 0 || pending <= 0 {
        return Err("invalid recap identity".into());
    }
    Ok(root(database).join(format!("{guild}-{pending}")))
}
fn load(dir: &Path) -> Result<Option<Archive>, String> {
    let path = dir.join("manifest.json");
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.len() > 128 * 1024 {
        return Err("recap manifest too large".into());
    }
    let archive: Archive = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if archive.guild_id <= 0
        || archive.pending_id <= 0
        || archive.valve_id == 0
        || archive.frames.len() > MAX_FRAMES
        || archive.interval < 15
        || archive.frames.iter().any(|frame| {
            frame.file != format!("frame-{}.png", frame.clock) || frame.bytes > MAX_PNG_BYTES as u64
        })
        || archive
            .frames
            .windows(2)
            .any(|pair| pair[0].clock >= pair[1].clock)
        || dir.file_name().and_then(|s| s.to_str())
            != Some(&format!("{}-{}", archive.guild_id, archive.pending_id))
    {
        return Err("invalid recap manifest".into());
    }
    Ok(Some(archive))
}
fn save(dir: &Path, archive: &Archive) -> Result<(), String> {
    let bytes = serde_json::to_vec(archive).map_err(|e| e.to_string())?;
    fs::write(dir.join("manifest.tmp"), bytes).map_err(|e| e.to_string())?;
    fs::rename(dir.join("manifest.tmp"), dir.join("manifest.json")).map_err(|e| e.to_string())
}
fn thin(archive: &mut Archive) -> Vec<String> {
    let last = archive.frames.len().saturating_sub(1);
    let mut removed = Vec::new();
    archive.frames = archive
        .frames
        .drain(..)
        .enumerate()
        .filter_map(|(index, frame)| {
            if index % 2 == 0 || index == last {
                Some(frame)
            } else {
                removed.push(frame.file);
                None
            }
        })
        .collect();
    archive.interval = archive.interval.saturating_mul(2);
    removed
}

/// Store only fresh, policy-qualified PNGs already rendered for live delivery.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn capture(
    database: PathBuf,
    guild: i64,
    pending: i64,
    valve: u64,
    clock: i64,
    png: Vec<u8>,
    now: i64,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        capture_sync(&database, guild, pending, valve, clock, &png, now)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[allow(clippy::too_many_arguments)]
fn capture_sync(
    database: &Path,
    guild: i64,
    pending: i64,
    valve: u64,
    clock: i64,
    png: &[u8],
    now: i64,
) -> Result<(), String> {
    if valve == 0 || png.len() > MAX_PNG_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("invalid or oversized recap screenshot".into());
    }
    let png = png::compress(png)?;
    let _lock = FILES.lock().map_err(|e| e.to_string())?;
    let dir = directory(database, guild, pending)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut archive = load(&dir)?.unwrap_or(Archive {
        guild_id: guild,
        pending_id: pending,
        valve_id: valve,
        updated_at: now,
        interval: 15,
        frames: Vec::new(),
        job: None,
    });
    if archive.valve_id != valve {
        return Err("recap Valve match identity changed".into());
    }
    if archive.job.is_some() {
        return Ok(());
    }
    if archive
        .frames
        .last()
        .is_some_and(|last| clock <= last.clock)
    {
        return Ok(());
    }
    // Keep the latest endpoint even between coarse samples after thinning.
    let mut removed = Vec::new();
    if archive.frames.len() >= 2
        && archive.frames[archive.frames.len() - 1].clock
            - archive.frames[archive.frames.len() - 2].clock
            < archive.interval
    {
        removed.push(archive.frames.pop().expect("two frames").file);
    }
    let file = format!("frame-{clock}.png");
    fs::write(dir.join("frame.tmp"), &png).map_err(|e| e.to_string())?;
    fs::rename(dir.join("frame.tmp"), dir.join(&file)).map_err(|e| e.to_string())?;
    archive.frames.push(Screenshot {
        clock,
        file,
        bytes: png.len() as u64,
    });
    while archive.frames.len() > MAX_FRAMES
        || archive.frames.iter().map(|f| f.bytes).sum::<u64>() > MAX_ARCHIVE_BYTES
    {
        removed.extend(thin(&mut archive));
    }
    archive.updated_at = now;
    save(&dir, &archive)?;
    for file in removed {
        let _ = fs::remove_file(dir.join(file));
    }
    Ok(())
}

/// Called only after the enriched summary message was acknowledged by Discord.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn queue_after_summary(
    database: PathBuf,
    guild: i64,
    pending: i64,
    match_id: i64,
    valve_id: Option<i64>,
    channel_id: u64,
    summary_message_id: u64,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let _lock = FILES.lock().map_err(|e| e.to_string())?;
        let dir = directory(&database, guild, pending)?;
        let Some(mut archive) = load(&dir)? else {
            return Ok(());
        };
        if match_id <= 0
            || channel_id == 0
            || summary_message_id == 0
            || valve_id.and_then(|id| u64::try_from(id).ok()) != Some(archive.valve_id)
        {
            return Err("recap summary identity mismatch".into());
        }
        if let Some(job) = &archive.job {
            return if job.match_id == match_id && job.channel_id == channel_id {
                Ok(())
            } else {
                Err("recap summary destination changed".into())
            };
        }
        if archive.frames.len() < 2 {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        archive.job = Some(Job {
            match_id,
            channel_id,
            summary_message_id,
            queued_at: now,
            next_attempt: now,
            delivered: None,
        });
        archive.updated_at = now;
        save(&dir, &archive)
    })
    .await
    .map_err(|e| e.to_string())?
}
fn delivery_key(archive: &Archive, job: &Job) -> String {
    let hash = Sha256::digest(format!(
        "recap:{}:{}:{}",
        archive.guild_id, job.match_id, archive.valve_id
    ));
    format!(
        "r{}",
        hash[..12]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn canonical_ready(database: &Path, archive: &Archive, job: &Job) -> Result<bool, String> {
    let matches = MatchRepository::new(database);
    if matches
        .match_id_for_pending_match(archive.guild_id, archive.pending_id)
        .map_err(|e| e.to_string())?
        != Some(job.match_id)
    {
        return Ok(false);
    }
    let recorded = matches
        .get_match(job.match_id, Some(archive.guild_id))
        .map_err(|e| e.to_string())?;
    Ok(recorded.is_some_and(|record| {
        record.valve_match_id.and_then(|id| u64::try_from(id).ok()) == Some(archive.valve_id)
    }) && PendingMatchRepository::new(database)
        .pending_match(archive.guild_id, archive.pending_id)
        .map_err(|e| e.to_string())?
        .is_none())
}
fn ready_jobs(
    database: &Path,
    guilds: &[i64],
    now: i64,
) -> Result<Vec<(PathBuf, Archive)>, String> {
    let _lock = FILES.lock().map_err(|e| e.to_string())?;
    let entries = match fs::read_dir(root(database)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut oldest: Option<(PathBuf, Archive)> = None;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let archive = match load(&dir) {
            Ok(Some(archive)) => archive,
            _ => {
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|t| t.as_secs() as i64);
                if modified.is_some_and(|at| now.saturating_sub(at) >= RETENTION) {
                    fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
                }
                continue;
            }
        };
        if now.saturating_sub(archive.updated_at) >= RETENTION {
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        } else {
            for file in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let file = file.map_err(|e| e.to_string())?;
                let name = file.file_name().to_string_lossy().into_owned();
                if name.starts_with("frame-")
                    && name.ends_with(".png")
                    && !archive.frames.iter().any(|f| f.file == name)
                {
                    fs::remove_file(file.path()).map_err(|e| e.to_string())?;
                }
            }
            if guilds.contains(&archive.guild_id)
                && archive
                    .job
                    .as_ref()
                    .is_some_and(|job| job.delivered.is_none() && job.next_attempt <= now)
                && oldest.as_ref().is_none_or(|(_, saved)| {
                    archive.job.as_ref().map(|j| j.queued_at)
                        < saved.job.as_ref().map(|j| j.queued_at)
                })
            {
                oldest = Some((dir, archive));
            }
        }
    }
    // At most one encoder/upload per worker tick, oldest queued work first.
    Ok(oldest.into_iter().collect())
}
fn checkpoint(
    dir: &Path,
    expected: &Archive,
    now: i64,
    delivered: Option<u64>,
) -> Result<(), String> {
    let _lock = FILES.lock().map_err(|e| e.to_string())?;
    let Some(mut archive) = load(dir)? else {
        return Err("recap archive disappeared".into());
    };
    if archive
        .job
        .as_ref()
        .map(|j| (j.match_id, j.channel_id, j.summary_message_id))
        != expected
            .job
            .as_ref()
            .map(|j| (j.match_id, j.channel_id, j.summary_message_id))
    {
        return Err("recap job changed".into());
    }
    let job = archive.job.as_mut().ok_or("missing recap job")?;
    job.next_attempt = now.saturating_add(60);
    if let Some(id) = delivered {
        job.delivered = Some(id);
    }
    save(dir, &archive)?;
    if delivered.is_some() {
        for frame in &archive.frames {
            let _ = fs::remove_file(dir.join(&frame.file));
        }
        let _ = fs::remove_file(dir.join("recap.gif"));
    }
    Ok(())
}

pub(crate) async fn tick(
    database: &Path,
    guilds: &[i64],
    discord: &dyn DiscordTransport,
    now: i64,
) -> Result<(), String> {
    let db = database.to_owned();
    let allowed = guilds.to_vec();
    let jobs = tokio::task::spawn_blocking(move || ready_jobs(&db, &allowed, now))
        .await
        .map_err(|e| e.to_string())??;
    for (dir, archive) in jobs {
        // Persist retry backoff before any external action, including lost replies.
        let path = dir.clone();
        let expected = archive.clone();
        tokio::task::spawn_blocking(move || checkpoint(&path, &expected, now, None))
            .await
            .map_err(|e| e.to_string())??;
        if let Err(error) = deliver(database, &dir, &archive, discord, now).await {
            tracing::warn!(%error, guild_id=archive.guild_id, pending_match_id=archive.pending_id, "optional map recap will retry");
        }
    }
    Ok(())
}
async fn deliver(
    database: &Path,
    dir: &Path,
    archive: &Archive,
    discord: &dyn DiscordTransport,
    now: i64,
) -> Result<(), String> {
    let job = archive.job.as_ref().ok_or("missing recap job")?;
    let db = database.to_owned();
    let saved = archive.clone();
    let queued = job.clone();
    if !tokio::task::spawn_blocking(move || canonical_ready(&db, &saved, &queued))
        .await
        .map_err(|e| e.to_string())??
    {
        return Ok(());
    }
    if discord
        .fetch_message(job.channel_id, job.summary_message_id)
        .await?
        .is_none()
    {
        return Err("recorded summary is no longer available".into());
    }
    let key = delivery_key(archive, job);
    let delivered = discord
        .find_message_by_delivery_key(job.channel_id, &key, job.queued_at.saturating_sub(1), 500)
        .await?;
    let message_id = if let Some(receipt) = delivered {
        receipt.message_id
    } else {
        let paths = archive
            .frames
            .iter()
            .map(|f| dir.join(&f.file))
            .collect::<Vec<_>>();
        let output = dir.join("recap.gif");
        let (bytes, info) = tokio::task::spawn_blocking(move || {
            if output.exists() {
                fs::remove_file(&output).map_err(|e| e.to_string())?;
            }
            let info = encoder::encode(&paths, &output)?;
            tracing::debug!(
                frames = info.frame_count,
                bytes = info.bytes,
                duration_cs = info.duration_cs,
                "encoded optional map recap"
            );
            let bytes = fs::read(&output).map_err(|e| e.to_string())?;
            Ok::<_, String>((bytes, info))
        })
        .await
        .map_err(|e| e.to_string())??;
        let first = archive.frames.first().ok_or("empty recap")?.clock;
        let last = archive.frames.last().ok_or("empty recap")?.clock;
        let filename = format!("match-{}-recap.gif", job.match_id);
        let embed = InteractionEmbed::titled(format!("Match #{} · Map recap", job.match_id))
            .image(format!("attachment://{filename}"));
        let content = format!(
            "Captured {}:{:02}–{}:{:02} · {:.1}s recap",
            first.div_euclid(60),
            first.rem_euclid(60),
            last.div_euclid(60),
            last.rem_euclid(60),
            f64::from(info.duration_cs) / 100.0
        );
        // Encoding can be slow. Recheck the recorded identity and completion
        // after it, so a correction/reopened pending match fails closed.
        let db = database.to_owned();
        let saved = archive.clone();
        let queued = job.clone();
        if !tokio::task::spawn_blocking(move || canonical_ready(&db, &saved, &queued))
            .await
            .map_err(|e| e.to_string())??
        {
            return Ok(());
        }
        if discord
            .fetch_message(job.channel_id, job.summary_message_id)
            .await?
            .is_none()
        {
            return Err("recorded summary disappeared before recap publication".into());
        }
        discord
            .send_message_with_delivery_key(
                job.channel_id,
                &key,
                DiscordMessage::silent(
                    InteractionResponse::message(content)
                        .embed(embed)
                        .attachment(InteractionAttachment::bytes(filename, bytes)),
                ),
            )
            .await?
            .message_id
    };
    let path = dir.to_owned();
    let expected = archive.clone();
    tokio::task::spawn_blocking(move || checkpoint(&path, &expected, now, Some(message_id)))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests;
