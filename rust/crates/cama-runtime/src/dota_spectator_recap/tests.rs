use super::*;
use crate::dota_live::{LiveMapBuilding, LiveMapFrame, LiveMapHero};
use cama_app::pet_assets::{RasterImage, Rgba};

fn picture() -> Vec<u8> {
    crate::dota_spectator_png::compress(&RasterImage::new(4, 4, Rgba(10, 20, 30, 255)).encode_png())
        .unwrap()
}

fn map_frame(game_time: i64) -> LiveMapFrame {
    LiveMapFrame {
        match_id: 999,
        game_time,
        radiant_score: Some(game_time / 15),
        dire_score: Some(1),
        radiant_net_worth: Some(20_000 + game_time * 50),
        dire_net_worth: Some(20_000),
        heroes: vec![LiveMapHero {
            hero_id: 1,
            radiant: true,
            x: Some(-4000.0 + game_time as f64 * 100.0),
            y: Some(1000.0),
            respawn_seconds: None,
            player_name: Some("Replay player".into()),
            ultimate_state: Some(3),
            ultimate_cooldown: None,
            kills: Some(game_time / 15),
            deaths: Some(0),
            assists: Some(2),
            level: Some(12),
            gold_per_min: Some(400),
            net_worth: Some(8000),
            items: vec![None; 6],
        }],
        buildings: vec![LiveMapBuilding {
            radiant: false,
            name: "mid tier 1 tower".into(),
            destroyed: game_time >= 30,
            x: None,
            y: None,
        }],
        roshan_respawn_seconds: Some(90 - game_time),
    }
}

#[tokio::test]
async fn capture_map_throttles_five_second_feed_and_keeps_recap_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("game.db");
    let mut captured = Vec::new();
    for clock in (0..=60).step_by(5) {
        if capture_map(database.clone(), 42, 7, map_frame(clock), clock)
            .await
            .unwrap()
        {
            captured.push(clock);
        }
    }
    assert_eq!(captured, vec![0, 15, 30, 45, 60]);

    let dir = directory(&database, 42, 7).unwrap();
    let archive = load(&dir).unwrap().unwrap();
    assert_eq!(archive.interval, CAPTURE_INTERVAL_SECONDS);
    assert_eq!(
        archive
            .frames
            .iter()
            .map(|frame| frame.clock)
            .collect::<Vec<_>>(),
        captured
    );
    assert!(
        archive
            .frames
            .windows(2)
            .all(|pair| pair[1].clock - pair[0].clock >= CAPTURE_INTERVAL_SECONDS)
    );
    assert!(archive.frames.len() <= MAX_FRAMES);
    assert!(archive.frames.iter().map(|frame| frame.bytes).sum::<u64>() <= MAX_ARCHIVE_BYTES);
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        archive.frames.len() + 1
    );

    let paths = archive
        .frames
        .iter()
        .map(|frame| dir.join(&frame.file))
        .collect::<Vec<_>>();
    let replay = dir.join("recap.gif");
    let info = encoder::encode(&paths, &replay).unwrap();
    assert_eq!(info.frame_count, captured.len());
    assert_eq!(info.bytes, fs::metadata(replay).unwrap().len());
    assert!(info.bytes < 8 * 1024 * 1024);
    assert_eq!(info.duration_cs, 300);

    // Decode the actual production GIF, including delta frames, rather than
    // trusting an encoder receipt. New scores, hero positions and destroyed
    // building markers must survive the compressed-PNG archive path.
    let mut decoder = gif::DecodeOptions::new()
        .read_info(fs::File::open(dir.join("recap.gif")).unwrap())
        .unwrap();
    assert_eq!((decoder.width(), decoder.height()), (1240, 704));
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().unwrap() {
        assert!(!frame.buffer.is_empty());
        delays.push(frame.delay);
    }
    assert_eq!(delays, vec![50, 50, 50, 50, 100]);
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
