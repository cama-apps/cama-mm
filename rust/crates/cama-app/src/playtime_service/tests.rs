use cama_db::registration_repository::RegistrationRepository;
use cama_db::schema_manager::initialize_or_migrate;

use super::*;

const GUILD: i64 = 0;

fn fixture() -> (tempfile::TempDir, PlaytimeService, RegistrationRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("playtime.db");
    initialize_or_migrate(&path).expect("migrate");
    let repository = RegistrationRepository::new(&path);
    (
        directory,
        PlaytimeService::new(repository.clone()),
        repository,
    )
}

fn test_connection(dir: &tempfile::TempDir) -> rusqlite::Connection {
    let path = dir.path().join("playtime.db");
    let connection = rusqlite::Connection::open(&path).expect("open");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    connection
}

fn insert_player(dir: &tempfile::TempDir, discord_id: i64, timezone: Option<&str>) {
    test_connection(dir)
        .execute(
            "INSERT INTO players(discord_id, guild_id, discord_username, timezone) VALUES (?1, 0, ?2, ?3)",
            rusqlite::params![discord_id, format!("player-{discord_id}"), timezone],
        )
        .expect("insert player");
}

#[test]
fn test_set_dota_play_hours_persists_sorted_and_deduped() {
    let (dir, service, repository) = fixture();
    insert_player(&dir, 1, None);

    service
        .set_play_hours(1, Some(GUILD), &[20, 18, 19, 18])
        .expect("set play hours");

    assert_eq!(
        repository.dota_play_hours_of(1, Some(GUILD)).unwrap(),
        Some(vec![18, 19, 20])
    );
}

#[test]
fn test_set_dota_play_hours_rejects_out_of_range() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 2, None);

    let error = service.set_play_hours(2, Some(GUILD), &[24]).unwrap_err();
    assert!(matches!(error, PlaytimeServiceError::InvalidHour));
}

#[test]
fn test_set_dota_play_hours_rejects_empty() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 3, None);

    let error = service.set_play_hours(3, Some(GUILD), &[]).unwrap_err();
    assert!(matches!(error, PlaytimeServiceError::EmptyHours));
}

#[test]
fn test_set_dota_play_hours_unregistered_player_raises() {
    let (_dir, service, _repository) = fixture();

    let error = service.set_play_hours(999, Some(GUILD), &[18]).unwrap_err();
    assert!(matches!(error, PlaytimeServiceError::PlayerNotRegistered));
}

#[test]
fn test_clear_dota_play_hours() {
    let (dir, service, repository) = fixture();
    insert_player(&dir, 4, None);
    service.set_play_hours(4, Some(GUILD), &[18]).unwrap();

    service.clear_play_hours(4, Some(GUILD)).expect("clear");

    assert_eq!(repository.dota_play_hours_of(4, Some(GUILD)).unwrap(), None);
}

#[test]
fn test_get_dota_play_hours_info() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 5, None);

    assert_eq!(service.play_hours_info(5, Some(GUILD)).unwrap(), None);

    service.set_play_hours(5, Some(GUILD), &[18, 19]).unwrap();

    assert_eq!(
        service.play_hours_info(5, Some(GUILD)).unwrap(),
        Some(vec![18, 19])
    );
}

#[test]
fn test_get_popular_play_hours_aggregates_registered_players() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 6, Some("America/New_York"));
    insert_player(&dir, 7, Some("America/New_York"));
    insert_player(&dir, 8, Some("America/New_York")); // never sets hours
    service.set_play_hours(6, Some(GUILD), &[20, 21]).unwrap();
    service.set_play_hours(7, Some(GUILD), &[20]).unwrap();

    let counts = service
        .popular_play_hours(Some(GUILD), Some("America/New_York"))
        .unwrap();

    assert_eq!(counts[20], 2);
    assert_eq!(counts[21], 1);
    assert_eq!(counts.iter().sum::<u32>(), 3);
}

#[test]
fn test_get_popular_play_hours_defaults_reporting_timezone() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 9, Some("America/New_York"));
    service.set_play_hours(9, Some(GUILD), &[12]).unwrap();

    let counts = service.popular_play_hours(Some(GUILD), None).unwrap();

    assert_eq!(counts[12], 1);
}
