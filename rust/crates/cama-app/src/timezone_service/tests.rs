use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::registration_repository::RegistrationRepository;

use super::*;

const GUILD: i64 = 0;

fn fixture() -> (tempfile::TempDir, TimezoneService, RegistrationRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("timezone.db");
    initialize_or_migrate(&path).expect("migrate");
    let repository = RegistrationRepository::new(&path);
    (
        directory,
        TimezoneService::new(repository.clone()),
        repository,
    )
}

fn test_connection(dir: &tempfile::TempDir) -> rusqlite::Connection {
    let path = dir.path().join("timezone.db");
    let connection = rusqlite::Connection::open(&path).expect("open");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    connection
}

fn insert_player(dir: &tempfile::TempDir, discord_id: i64) {
    test_connection(dir)
        .execute(
            "INSERT INTO players(discord_id, guild_id, discord_username) VALUES (?1, 0, ?2)",
            rusqlite::params![discord_id, format!("player-{discord_id}")],
        )
        .expect("insert player");
}

#[test]
fn test_set_timezone_persists() {
    let (dir, service, repository) = fixture();
    insert_player(&dir, 1);

    service
        .set_timezone(1, Some(GUILD), "Asia/Tokyo")
        .expect("set timezone");

    assert_eq!(
        repository.timezone_of(1, Some(GUILD)).unwrap(),
        Some("Asia/Tokyo".to_owned())
    );
}

#[test]
fn test_set_timezone_rejects_unknown_timezone() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 2);

    let error = service
        .set_timezone(2, Some(GUILD), "Not/AZone")
        .unwrap_err();
    assert!(matches!(error, TimezoneServiceError::InvalidTimezone(_)));
}

#[test]
fn test_set_timezone_unregistered_player_raises() {
    let (_dir, service, _repository) = fixture();

    let error = service
        .set_timezone(999, Some(GUILD), "Asia/Tokyo")
        .unwrap_err();
    assert!(matches!(error, TimezoneServiceError::PlayerNotRegistered));
}

#[test]
fn test_get_timezone_info_before_any_set() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 3);

    assert_eq!(service.timezone_info(3, Some(GUILD)).unwrap(), None);
}

#[test]
fn test_get_timezone_info_after_set() {
    let (dir, service, _repository) = fixture();
    insert_player(&dir, 4);
    service.set_timezone(4, Some(GUILD), "Asia/Tokyo").unwrap();

    assert_eq!(
        service.timezone_info(4, Some(GUILD)).unwrap(),
        Some("Asia/Tokyo".to_owned())
    );
}
