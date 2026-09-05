use cama_domain::curfew::{CurfewMode, CurfewWindow};
use rusqlite::params;

use super::*;
use crate::test_support::copy_migrated_database;

const GUILD: i64 = 0;
const OTHER_GUILD: i64 = 999;

fn fixture() -> (tempfile::TempDir, CurfewRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("curfew.db");
    copy_migrated_database(&path).expect("migrate");
    (directory, CurfewRepository::new(&path))
}

fn insert_player(path: &std::path::Path, discord_id: i64, guild_id: i64) {
    let connection = crate::open_runtime_connection(path).expect("open");
    connection
        .execute(
            "INSERT INTO players(discord_id, guild_id, discord_username) VALUES (?1, ?2, ?3)",
            params![discord_id, guild_id, format!("player-{discord_id}")],
        )
        .expect("insert player");
}

fn window(
    discord_id: i64,
    guild_id: i64,
    name: &str,
    start_hour: u32,
    end_hour: u32,
) -> CurfewWindow {
    CurfewWindow {
        discord_id,
        guild_id,
        name: name.to_owned(),
        start_hour,
        start_minute: 0,
        end_hour,
        end_minute: 0,
        timezone: None,
        days: None,
        mode: CurfewMode::Default,
    }
}

#[test]
fn test_list_for_player_starts_empty() {
    let (_dir, repository) = fixture();
    assert_eq!(repository.list_for_player(1, GUILD).unwrap(), Vec::new());
}

#[test]
fn test_add_and_read_window() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();

    let windows = repository.list_for_player(1, GUILD).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].name, "work");
    assert_eq!(windows[0].start_hour, 9);
    assert_eq!(windows[0].end_hour, 17);
    assert_eq!(windows[0].timezone, None);
}

#[test]
fn test_multiple_named_windows_per_player() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    repository
        .add_or_replace(&window(1, GUILD, "sleep", 22, 6))
        .unwrap();

    let windows = repository.list_for_player(1, GUILD).unwrap();
    let names: Vec<&str> = windows.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, vec!["sleep", "work"]);
}

#[test]
fn test_add_or_replace_overwrites_same_name() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    let mut updated = window(1, GUILD, "work", 8, 16);
    updated.start_minute = 30;
    updated.timezone = Some("America/Chicago".to_owned());
    repository.add_or_replace(&updated).unwrap();

    let windows = repository.list_for_player(1, GUILD).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].start_hour, 8);
    assert_eq!(windows[0].start_minute, 30);
    assert_eq!(windows[0].timezone.as_deref(), Some("America/Chicago"));
}

#[test]
fn test_remove_existing_window_returns_true() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();

    assert!(repository.remove(1, GUILD, "work").unwrap());
    assert_eq!(repository.list_for_player(1, GUILD).unwrap(), Vec::new());
}

#[test]
fn test_remove_missing_window_returns_false() {
    let (_dir, repository) = fixture();
    assert!(!repository.remove(1, GUILD, "nonexistent").unwrap());
}

#[test]
fn test_windows_are_guild_scoped() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();

    assert_eq!(
        repository.list_for_player(1, OTHER_GUILD).unwrap(),
        Vec::new()
    );
    assert_eq!(repository.list_for_player(1, GUILD).unwrap().len(), 1);
}

#[test]
fn test_list_for_players_bulk_fetch() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    repository
        .add_or_replace(&window(2, GUILD, "sleep", 22, 6))
        .unwrap();
    // Player 3 has no windows and should be absent from the result.

    let result = repository.list_for_players(&[1, 2, 3], GUILD).unwrap();

    assert_eq!(result.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(result[&1][0].name, "work");
    assert_eq!(result[&2][0].name, "sleep");
}

#[test]
fn test_list_for_players_empty_input() {
    let (_dir, repository) = fixture();
    assert!(repository.list_for_players(&[], GUILD).unwrap().is_empty());
}

#[test]
fn test_general_timezone_reads_players_table() {
    let (dir, repository) = fixture();
    let path = dir.path().join("curfew.db");
    insert_player(&path, 1, GUILD);
    let connection = crate::open_runtime_connection(&path).unwrap();
    connection
        .execute(
            "UPDATE players SET timezone = 'Asia/Tokyo' WHERE discord_id = 1 AND guild_id = 0",
            [],
        )
        .unwrap();

    assert_eq!(
        repository.general_timezone(1, GUILD).unwrap().as_deref(),
        Some("Asia/Tokyo")
    );
}

#[test]
fn test_general_timezone_none_when_unset() {
    let (dir, repository) = fixture();
    insert_player(&dir.path().join("curfew.db"), 1, GUILD);
    assert_eq!(repository.general_timezone(1, GUILD).unwrap(), None);
}

#[test]
fn test_player_exists() {
    let (dir, repository) = fixture();
    insert_player(&dir.path().join("curfew.db"), 1, GUILD);
    assert!(repository.player_exists(1, GUILD).unwrap());
    assert!(!repository.player_exists(999, GUILD).unwrap());
}

#[test]
fn test_days_round_trips_and_defaults_to_none() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    let mut weekend = window(1, GUILD, "weekend_sleep", 22, 6);
    weekend.days = Some(
        cama_domain::curfew::weekday_bit(chrono::Weekday::Fri)
            | cama_domain::curfew::weekday_bit(chrono::Weekday::Sat),
    );
    repository.add_or_replace(&weekend).unwrap();

    let windows = repository.list_for_player(1, GUILD).unwrap();
    let work = windows.iter().find(|w| w.name == "work").unwrap();
    assert_eq!(work.days, None);
    let weekend = windows.iter().find(|w| w.name == "weekend_sleep").unwrap();
    assert_eq!(
        weekend.days,
        Some(
            cama_domain::curfew::weekday_bit(chrono::Weekday::Fri)
                | cama_domain::curfew::weekday_bit(chrono::Weekday::Sat)
        )
    );
}

#[test]
fn test_out_of_range_day_mask_degrades_to_every_day_instead_of_truncating() {
    let (directory, repository) = fixture();
    let path = directory.path().join("curfew.db");
    repository
        .add_or_replace(&window(1, GUILD, "corrupt", 22, 6))
        .unwrap();
    // 256 truncates to 0 under `as u8` — a mask that matches no weekday, so
    // the window would silently never fire again while still rendering like
    // an every-day one. 0 is equally invalid.
    for stored in [256_i64, 0, -1, 128] {
        let connection = crate::open_runtime_connection(&path).expect("open");
        connection
            .execute(
                "UPDATE player_curfew_windows SET days = ?1 WHERE discord_id = 1 AND name = 'corrupt'",
                params![stored],
            )
            .expect("corrupt the mask");
        drop(connection);

        let windows = repository.list_for_player(1, GUILD).unwrap();
        let corrupt = windows.iter().find(|w| w.name == "corrupt").unwrap();
        assert_eq!(
            corrupt.days, None,
            "stored day mask {stored} should fall back to the every-day default"
        );
    }
}

#[test]
fn test_mode_round_trips_and_defaults() {
    let (_dir, repository) = fixture();
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    let mut informational_window = window(1, GUILD, "night", 22, 6);
    informational_window.mode = CurfewMode::Informational;
    repository.add_or_replace(&informational_window).unwrap();

    let windows = repository.list_for_player(1, GUILD).unwrap();
    let work = windows.iter().find(|w| w.name == "work").unwrap();
    assert_eq!(work.mode, CurfewMode::Default);
    let night = windows.iter().find(|w| w.name == "night").unwrap();
    assert_eq!(night.mode, CurfewMode::Informational);
}

#[test]
fn test_get_window_returns_none_when_missing() {
    let (_dir, repository) = fixture();
    assert_eq!(repository.get_window(1, GUILD, "work").unwrap(), None);
    repository
        .add_or_replace(&window(1, GUILD, "work", 9, 17))
        .unwrap();
    assert_eq!(
        repository
            .get_window(1, GUILD, "work")
            .unwrap()
            .unwrap()
            .name,
        "work"
    );
}

#[test]
fn test_stage_pending_upsert_leaves_live_window_untouched() {
    let (_dir, repository) = fixture();
    let mut strict = window(1, GUILD, "work", 9, 17);
    strict.mode = CurfewMode::Strict;
    repository.add_or_replace(&strict).unwrap();

    let mut edited = window(1, GUILD, "work", 8, 16);
    edited.mode = CurfewMode::Strict;
    let effective_at = chrono::Utc::now() + chrono::Duration::hours(6);
    repository
        .stage_pending_upsert(&edited, effective_at)
        .unwrap();

    // The live row is unchanged — today's enforcement still uses the old times.
    let live = repository.get_window(1, GUILD, "work").unwrap().unwrap();
    assert_eq!(live.start_hour, 9);

    let pending = repository
        .pending_change_for(1, GUILD, "work")
        .unwrap()
        .unwrap();
    assert_eq!(pending.window.unwrap().start_hour, 8);
    assert_eq!(pending.effective_at, effective_at);
}

#[test]
fn test_apply_due_pending_changes_commits_upsert_and_clears_pending_row() {
    let (_dir, repository) = fixture();
    let mut strict = window(1, GUILD, "work", 9, 17);
    strict.mode = CurfewMode::Strict;
    repository.add_or_replace(&strict).unwrap();
    let mut edited = window(1, GUILD, "work", 8, 16);
    edited.mode = CurfewMode::Strict;
    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    repository.stage_pending_upsert(&edited, past).unwrap();

    let applied = repository
        .apply_due_pending_changes(chrono::Utc::now())
        .unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].window.as_ref().unwrap().start_hour, 8);

    let live = repository.get_window(1, GUILD, "work").unwrap().unwrap();
    assert_eq!(live.start_hour, 8);
    assert!(
        repository
            .pending_change_for(1, GUILD, "work")
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_apply_due_pending_changes_skips_changes_not_yet_effective() {
    let (_dir, repository) = fixture();
    let mut strict = window(1, GUILD, "work", 9, 17);
    strict.mode = CurfewMode::Strict;
    repository.add_or_replace(&strict).unwrap();
    let future = chrono::Utc::now() + chrono::Duration::hours(6);
    repository
        .stage_pending_upsert(&window(1, GUILD, "work", 8, 16), future)
        .unwrap();

    let applied = repository
        .apply_due_pending_changes(chrono::Utc::now())
        .unwrap();
    assert!(applied.is_empty());
    assert_eq!(
        repository
            .get_window(1, GUILD, "work")
            .unwrap()
            .unwrap()
            .start_hour,
        9
    );
}

#[test]
fn test_apply_due_pending_changes_commits_delete() {
    let (_dir, repository) = fixture();
    let mut strict = window(1, GUILD, "work", 9, 17);
    strict.mode = CurfewMode::Strict;
    repository.add_or_replace(&strict).unwrap();
    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    repository
        .stage_pending_delete(1, GUILD, "work", past)
        .unwrap();

    let applied = repository
        .apply_due_pending_changes(chrono::Utc::now())
        .unwrap();
    assert_eq!(applied.len(), 1);
    assert!(applied[0].window.is_none());
    assert!(repository.get_window(1, GUILD, "work").unwrap().is_none());
}

#[test]
fn test_is_covered_is_false_until_coverage_is_recorded() {
    let (dir, repository) = fixture();
    let path = dir.path().join("curfew.db");
    insert_player(&path, 1, GUILD);

    assert!(
        !repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );
    repository
        .record_coverage(1, GUILD, "night", "2026-01-01")
        .unwrap();
    assert!(
        repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );
    assert!(
        !repository
            .is_covered(1, GUILD, "night", "2026-01-02")
            .unwrap(),
        "coverage is per local calendar date"
    );
    assert!(
        !repository
            .is_covered(1, GUILD, "other", "2026-01-01")
            .unwrap(),
        "coverage is per window"
    );
}

#[test]
fn test_clear_coverage_removes_a_recorded_confirmation() {
    let (dir, repository) = fixture();
    let path = dir.path().join("curfew.db");
    insert_player(&path, 1, GUILD);
    repository
        .record_coverage(1, GUILD, "night", "2026-01-01")
        .unwrap();
    repository.clear_coverage(&[1], GUILD).unwrap();
    assert!(
        !repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );
}

#[test]
fn test_record_coverage_is_idempotent_and_moves_to_the_latest_date() {
    let (dir, repository) = fixture();
    let path = dir.path().join("curfew.db");
    insert_player(&path, 1, GUILD);
    repository
        .record_coverage(1, GUILD, "night", "2026-01-01")
        .unwrap();
    repository
        .record_coverage(1, GUILD, "night", "2026-01-01")
        .unwrap();
    assert!(
        repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );

    repository
        .record_coverage(1, GUILD, "night", "2026-01-02")
        .unwrap();
    assert!(
        !repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );
    assert!(
        repository
            .is_covered(1, GUILD, "night", "2026-01-02")
            .unwrap()
    );
}

#[test]
fn test_clear_coverage_is_scoped_to_the_guild_and_players_given() {
    let (dir, repository) = fixture();
    let path = dir.path().join("curfew.db");
    insert_player(&path, 1, GUILD);
    insert_player(&path, 2, GUILD);
    insert_player(&path, 1, OTHER_GUILD);
    for (discord_id, guild_id) in [(1, GUILD), (2, GUILD), (1, OTHER_GUILD)] {
        repository
            .record_coverage(discord_id, guild_id, "night", "2026-01-01")
            .unwrap();
    }

    repository.clear_coverage(&[1], GUILD).unwrap();

    assert!(
        !repository
            .is_covered(1, GUILD, "night", "2026-01-01")
            .unwrap()
    );
    assert!(
        repository
            .is_covered(2, GUILD, "night", "2026-01-01")
            .unwrap()
    );
    assert!(
        repository
            .is_covered(1, OTHER_GUILD, "night", "2026-01-01")
            .unwrap(),
        "coverage in another guild is untouched"
    );
}
