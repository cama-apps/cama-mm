use tempfile::NamedTempFile;

use crate::test_support::copy_migrated_database;

use super::*;

const GUILD: i64 = 42;
const NOW: i64 = 1_800_000_000;

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
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-1", NOW)
        .unwrap();
    let config = repository.get_config(1, Some(GUILD)).unwrap().unwrap();
    assert_eq!(config.target.server, "https://ntfy.sh");
    assert_eq!(config.target.topic, "topic-1");
    assert!(config.readycheck_enabled);
    assert!(config.lobby_enabled);
}

#[test]
fn set_target_again_replaces_topic_and_resets_enabled_flags() {
    let (_file, repository) = repository();
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-1", NOW)
        .unwrap();
    repository
        .set_enabled(1, Some(GUILD), PushNotificationKind::Lobby, false, NOW)
        .unwrap();
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-2", NOW + 1)
        .unwrap();
    let config = repository.get_config(1, Some(GUILD)).unwrap().unwrap();
    assert_eq!(config.target.topic, "topic-2");
    assert!(config.lobby_enabled);
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
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-1", NOW)
        .unwrap();
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
    assert!(config.lobby_enabled);
}

#[test]
fn delete_target_removes_the_row() {
    let (_file, repository) = repository();
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-1", NOW)
        .unwrap();
    assert!(repository.delete_target(1, Some(GUILD)).unwrap());
    assert_eq!(repository.get_config(1, Some(GUILD)).unwrap(), None);
    assert!(!repository.delete_target(1, Some(GUILD)).unwrap());
}

#[test]
fn enabled_targets_filters_by_kind_guild_and_discord_ids() {
    let (_file, repository) = repository();
    repository
        .set_target(1, Some(GUILD), "https://ntfy.sh", "topic-1", NOW)
        .unwrap();
    repository
        .set_target(2, Some(GUILD), "https://ntfy.sh", "topic-2", NOW)
        .unwrap();
    repository
        .set_enabled(2, Some(GUILD), PushNotificationKind::Lobby, false, NOW)
        .unwrap();
    // Different guild: must not leak into guild-scoped results.
    repository
        .set_target(1, Some(99), "https://ntfy.sh", "topic-other-guild", NOW)
        .unwrap();
    // Not in the requested discord_id list: must be excluded.
    repository
        .set_target(3, Some(GUILD), "https://ntfy.sh", "topic-3", NOW)
        .unwrap();

    let targets = repository
        .enabled_targets(Some(GUILD), &[1, 2], PushNotificationKind::Lobby)
        .unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, 1);
    assert_eq!(targets[0].1.topic, "topic-1");
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
    repository
        .set_target(1, None, "https://ntfy.sh", "topic-dm", NOW)
        .unwrap();
    assert_eq!(
        repository
            .get_config(1, Some(0))
            .unwrap()
            .unwrap()
            .target
            .topic,
        "topic-dm"
    );
}
