use tempfile::NamedTempFile;

use crate::test_support::copy_migrated_database;

use super::*;

const GUILD: i64 = 42;
const NOW: i64 = 1_800_000_000;
const TOPIC_1: &str = "cama-000000000000000000000000000000000000000000000001";
const TOPIC_2: &str = "cama-000000000000000000000000000000000000000000000002";
const TOPIC_3: &str = "cama-000000000000000000000000000000000000000000000003";
const TOPIC_4: &str = "cama-000000000000000000000000000000000000000000000004";

fn repository() -> (NamedTempFile, PushNotificationRepository) {
    let file = NamedTempFile::new().expect("temporary push notification database");
    copy_migrated_database(file.path()).expect("migrate push notification database");
    let repository = PushNotificationRepository::new(file.path());
    (file, repository)
}

#[test]
fn missing_target_reads_as_none() {
    let (_file, repository) = repository();
    assert_eq!(repository.get_config(1, Some(GUILD)).unwrap(), None);
}

#[test]
fn set_target_enables_both_kinds_by_default() {
    let (_file, repository) = repository();
    repository.set_target(1, Some(GUILD), TOPIC_1, NOW).unwrap();
    let config = repository.get_config(1, Some(GUILD)).unwrap().unwrap();
    assert_eq!(config.target.topic, TOPIC_1);
    assert!(config.readycheck_enabled);
    assert!(config.match_started_enabled);
}

#[test]
fn set_target_rejects_non_generated_or_duplicate_topics() {
    let (_file, repository) = repository();
    assert!(
        repository
            .set_target(1, Some(GUILD), "guessable-topic", NOW)
            .is_err()
    );
    repository
        .set_target(1, Some(GUILD), TOPIC_1, NOW)
        .expect("seed generated topic");
    assert!(repository.set_target(2, Some(GUILD), TOPIC_1, NOW).is_err());
}

#[test]
fn set_target_again_replaces_topic_and_preserves_enabled_flags() {
    let (_file, repository) = repository();
    repository.set_target(1, Some(GUILD), TOPIC_1, NOW).unwrap();
    repository
        .set_enabled(
            1,
            Some(GUILD),
            PushNotificationKind::MatchStarted,
            false,
            NOW,
        )
        .unwrap();
    repository
        .set_target(1, Some(GUILD), TOPIC_2, NOW + 1)
        .unwrap();
    let config = repository.get_config(1, Some(GUILD)).unwrap().unwrap();
    assert_eq!(config.target.topic, TOPIC_2);
    assert!(!config.match_started_enabled);
}

#[test]
fn set_enabled_without_existing_target_reports_false() {
    let (_file, repository) = repository();
    let changed = repository
        .set_enabled(1, Some(GUILD), PushNotificationKind::Readycheck, false, NOW)
        .unwrap();
    assert!(!changed);
}

#[test]
fn set_enabled_toggles_one_kind_independently() {
    let (_file, repository) = repository();
    repository.set_target(1, Some(GUILD), TOPIC_1, NOW).unwrap();
    let changed = repository
        .set_enabled(
            1,
            Some(GUILD),
            PushNotificationKind::Readycheck,
            false,
            NOW + 1,
        )
        .unwrap();
    assert!(changed);
    let config = repository.get_config(1, Some(GUILD)).unwrap().unwrap();
    assert!(!config.readycheck_enabled);
    assert!(config.match_started_enabled);
}

#[test]
fn delete_target_removes_the_row() {
    let (_file, repository) = repository();
    repository.set_target(1, Some(GUILD), TOPIC_1, NOW).unwrap();
    assert!(repository.delete_target(1, Some(GUILD)).unwrap());
    assert_eq!(repository.get_config(1, Some(GUILD)).unwrap(), None);
    assert!(!repository.delete_target(1, Some(GUILD)).unwrap());
}

#[test]
fn enabled_targets_filters_by_kind_guild_and_discord_ids() {
    let (_file, repository) = repository();
    repository.set_target(1, Some(GUILD), TOPIC_1, NOW).unwrap();
    repository.set_target(2, Some(GUILD), TOPIC_2, NOW).unwrap();
    repository
        .set_enabled(
            2,
            Some(GUILD),
            PushNotificationKind::MatchStarted,
            false,
            NOW,
        )
        .unwrap();
    // Different guild: must not leak into guild-scoped results.
    repository.set_target(1, Some(99), TOPIC_3, NOW).unwrap();
    // Not in the requested discord_id list: must be excluded.
    repository.set_target(3, Some(GUILD), TOPIC_4, NOW).unwrap();

    let targets = repository
        .enabled_targets(Some(GUILD), &[1, 2], PushNotificationKind::MatchStarted)
        .unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, 1);
    assert_eq!(targets[0].1.topic, TOPIC_1);
}

#[test]
fn enabled_targets_with_empty_discord_ids_is_empty() {
    let (_file, repository) = repository();
    let targets = repository
        .enabled_targets(Some(GUILD), &[], PushNotificationKind::Readycheck)
        .unwrap();
    assert!(targets.is_empty());
}

#[test]
fn guild_id_normalizes_none_to_zero() {
    let (_file, repository) = repository();
    repository.set_target(1, None, TOPIC_1, NOW).unwrap();
    assert_eq!(
        repository
            .get_config(1, Some(0))
            .unwrap()
            .unwrap()
            .target
            .topic,
        TOPIC_1
    );
}
