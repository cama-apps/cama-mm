use super::*;
use cama_app::pet_assets::{RasterImage, Rgba};

fn picture() -> Vec<u8> {
    RasterImage::new(4, 4, Rgba(10, 20, 30, 255)).encode_png()
}

#[test]
fn archive_bounds_long_matches_and_preserves_endpoints_without_ram_frames() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("game.db");
    let png = picture();
    for index in 0..1000 {
        capture_sync(&database, 42, 7, 999, index * 15, &png, index * 15).unwrap();
    }
    let dir = directory(&database, 42, 7).unwrap();
    let archive = load(&dir).unwrap().unwrap();
    assert!(archive.frames.len() <= MAX_FRAMES);
    assert_eq!(archive.frames.first().unwrap().clock, 0);
    assert_eq!(archive.frames.last().unwrap().clock, 999 * 15);
    assert!(archive.interval >= 60);
    assert!(
        archive.frames[..archive.frames.len() - 1]
            .windows(2)
            .all(|pair| pair[1].clock - pair[0].clock >= archive.interval)
    );
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        archive.frames.len() + 1
    );
    capture_sync(&database, 42, 7, 999, 5, &png, 20_000).unwrap();
    assert_eq!(
        load(&dir).unwrap().unwrap().frames.len(),
        archive.frames.len()
    );
    assert!(capture_sync(&database, 42, 7, 888, 20_000, &png, 20_000).is_err());
    assert!(!root(&temp.path().join("other.db")).exists());
}
#[tokio::test]
async fn queue_requires_matching_capture_and_is_durable_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("game.db");
    let now = chrono::Utc::now().timestamp();
    queue_after_summary(database.clone(), 42, 7, 8, Some(999), 123, 456)
        .await
        .unwrap();
    assert!(!root(&database).exists());
    capture_sync(&database, 42, 7, 999, 0, &picture(), now).unwrap();
    queue_after_summary(database.clone(), 42, 7, 8, Some(999), 123, 456)
        .await
        .unwrap();
    let dir = directory(&database, 42, 7).unwrap();
    assert!(load(&dir).unwrap().unwrap().job.is_none());
    capture_sync(&database, 42, 7, 999, 15, &picture(), now).unwrap();
    assert!(
        queue_after_summary(database.clone(), 42, 7, 8, Some(888), 123, 456)
            .await
            .is_err()
    );
    queue_after_summary(database.clone(), 42, 7, 8, Some(999), 123, 456)
        .await
        .unwrap();
    queue_after_summary(database.clone(), 42, 7, 8, Some(999), 123, 789)
        .await
        .unwrap();
    let archive = load(&dir).unwrap().unwrap();
    assert_eq!(archive.job.as_ref().unwrap().summary_message_id, 456);
    capture_sync(&database, 42, 7, 999, 30, &picture(), now).unwrap();
    assert_eq!(load(&dir).unwrap().unwrap().frames.len(), 2);
    assert!(ready_jobs(&database, &[43], now + 1).unwrap().is_empty());
    assert_eq!(ready_jobs(&database, &[42], now + 1).unwrap().len(), 1);
    checkpoint(&dir, &archive, now, None).unwrap();
    assert!(ready_jobs(&database, &[42], now + 59).unwrap().is_empty());
    assert_eq!(ready_jobs(&database, &[42], now + 60).unwrap().len(), 1);
    checkpoint(&dir, &archive, now + 60, Some(777)).unwrap();
    assert!(ready_jobs(&database, &[42], now + 120).unwrap().is_empty());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    assert_eq!(
        load(&dir).unwrap().unwrap().job.unwrap().delivered,
        Some(777)
    );
}
#[test]
fn expiration_and_orphan_cleanup_are_bounded_and_preserve_current_frames() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("game.db");
    capture_sync(&database, 42, 7, 999, 0, &picture(), 100).unwrap();
    let dir = directory(&database, 42, 7).unwrap();
    fs::write(dir.join("frame-999.png"), picture()).unwrap();
    ready_jobs(&database, &[42], 101).unwrap();
    assert!(!dir.join("frame-999.png").exists());
    assert!(dir.join("frame-0.png").exists());
    ready_jobs(&database, &[42], 100 + RETENTION).unwrap();
    assert!(!dir.exists());
}
#[test]
fn malformed_manifest_cannot_escape_archive_paths() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("game.db");
    capture_sync(&database, 42, 7, 999, 0, &picture(), 100).unwrap();
    let dir = directory(&database, 42, 7).unwrap();
    let mut archive = load(&dir).unwrap().unwrap();
    archive.frames[0].file = "../../elsewhere.png".into();
    save(&dir, &archive).unwrap();
    assert!(load(&dir).is_err());
    assert!(directory(&database, -1, 7).is_err());
}
