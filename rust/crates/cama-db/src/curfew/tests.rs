use cama_domain::curfew::CurfewWindow;
use rusqlite::params;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const GUILD: i64 = 0;
const OTHER_GUILD: i64 = 999;

fn fixture() -> (tempfile::TempDir, CurfewRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("curfew.db");
    initialize_or_migrate(&path).expect("migrate");
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
