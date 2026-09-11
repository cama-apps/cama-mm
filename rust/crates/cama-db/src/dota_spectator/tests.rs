use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::NamedTempFile;

use super::*;

fn fixture() -> (NamedTempFile, DotaSpectatorRepository) {
    let file = NamedTempFile::new().unwrap();
    crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
    let repository = DotaSpectatorRepository::new(file.path());
    (file, repository)
}

#[test]
fn subscriptions_are_idempotent_scoped_and_survive_repository_restart() {
    let (file, repository) = fixture();
    repository.update_subscription(1, 2, 3, true, 100).unwrap();
    repository.update_subscription(1, 2, 3, true, 101).unwrap();
    repository.update_subscription(1, 2, 4, true, 101).unwrap();
    repository.update_subscription(2, 2, 3, true, 102).unwrap();
    repository.update_subscription(1, 3, 3, true, 102).unwrap();
    let reopened = DotaSpectatorRepository::new(file.path());
    assert_eq!(reopened.subscribers(1, 2).unwrap(), vec![3, 4]);
    reopened.update_subscription(1, 2, 3, false, 103).unwrap();
    reopened.update_subscription(1, 2, 3, false, 104).unwrap();
    assert_eq!(reopened.subscribers(1, 2).unwrap(), vec![4]);
    assert_eq!(reopened.subscribers(2, 2).unwrap(), vec![3]);
    assert_eq!(reopened.subscribers(1, 3).unwrap(), vec![3]);
    assert!(reopened.subscribers(9, 9).unwrap().is_empty());
    assert!(reopened.update_subscription(1, 2, 0, true, 1).is_err());
    assert!(reopened.update_subscription(0, 2, 1, true, 1).is_err());
    assert!(reopened.subscribers(1, -2).is_err());
}

#[test]
fn subscription_cap_allows_retries_and_removal_without_affecting_other_messages() {
    let (_file, repository) = fixture();
    for user_id in 1..=80 {
        repository
            .update_subscription(1, 2, user_id, true, 100)
            .unwrap();
    }
    repository.update_subscription(1, 2, 1, true, 101).unwrap();
    assert!(matches!(
        repository.update_subscription(1, 2, 81, true, 101),
        Err(DotaSpectatorRepositoryError::SubscriberLimit)
    ));
    assert_eq!(repository.subscribers(1, 2).unwrap().len(), 80);
    repository.update_subscription(1, 3, 81, true, 101).unwrap();
    repository.update_subscription(2, 2, 81, true, 101).unwrap();
    repository.update_subscription(1, 2, 1, false, 102).unwrap();
    repository.update_subscription(1, 2, 81, true, 102).unwrap();
    assert_eq!(
        repository.subscribers(1, 2).unwrap(),
        (2..=81).collect::<Vec<_>>()
    );
}

#[test]
fn first_resource_identity_and_intent_survive_retry_restart_and_pending_cleanup() {
    let (file, repository) = fixture();
    let first = repository
        .create_or_get(
            1,
            2,
            "owner-original",
            json!({"viewers":[7],"outbox":["create"]}),
            100,
        )
        .unwrap();
    let retried = repository
        .create_or_get(1, 2, "owner-retry", json!({}), 200)
        .unwrap();
    assert_eq!(first, retried);
    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute(
            "INSERT INTO pending_matches(pending_match_id,guild_id,payload) VALUES(2,1,'{}')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM pending_matches WHERE pending_match_id=2 AND guild_id=1",
            [],
        )
        .unwrap();
    drop(connection);
    let reopened = DotaSpectatorRepository::new(file.path());
    assert_eq!(reopened.get(1, 2).unwrap(), Some(first));
    assert!(reopened.get(2, 2).unwrap().is_none());
    let other = reopened
        .create_or_get(2, 2, "other-guild", json!({}), 50)
        .unwrap();
    assert_eq!(reopened.list().unwrap()[0], other);
}

#[test]
fn optimistic_updates_and_cleanup_reject_stale_revisions_and_changed_ownership() {
    let (_file, repository) = fixture();
    let mut record = repository
        .create_or_get(1, 2, "owner", json!({"intent":"create"}), 100)
        .unwrap();
    record.channel_id = Some(999);
    record.payload = json!({"viewers":[5,6],"detector":{"last_sequence":3},"outbox":[{"id":"a"}],"future":{"keep":true}});
    record.expires_at = Some(1_000);
    assert!(repository.save(&record, 0, 200).unwrap());
    assert!(!repository.save(&record, 0, 201).unwrap());
    assert!(!repository.delete(1, 2, 0).unwrap());
    assert!(!repository.delete(2, 2, 1).unwrap());
    let saved = repository.get(1, 2).unwrap().unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.updated_at, 200);
    assert_eq!(saved.payload, record.payload);
    assert_eq!(saved.channel_id, Some(999));
    assert_eq!(saved.expires_at, Some(1_000));
    record.marker = "replacement-owner".into();
    assert!(!repository.save(&record, 1, 202).unwrap());
    assert_eq!(repository.get(1, 2).unwrap(), Some(saved));
    assert!(repository.delete(1, 2, 1).unwrap());
    assert!(!repository.delete(1, 2, 1).unwrap());
    assert!(repository.get(1, 2).unwrap().is_none());
}

#[test]
fn concurrent_creators_observe_the_same_committed_ownership_marker() {
    let (_file, repository) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|index| {
            let repository = repository.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                repository
                    .create_or_get(
                        1,
                        2,
                        &format!("owner-{index}"),
                        json!({"creator":index}),
                        100,
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let records = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0], records[1]);
}

#[test]
fn expired_records_remain_available_for_cleanup_and_listing_is_bounded() {
    let (file, repository) = fixture();
    let mut connection = Connection::open(file.path()).unwrap();
    let transaction = connection.transaction().unwrap();
    for pending in 1..=1_001 {
        transaction.execute("INSERT INTO dota_spectators(guild_id,pending_match_id,marker,payload,expires_at,updated_at) VALUES(1,?1,'owner','{}',0,?1)", [pending]).unwrap();
    }
    transaction.commit().unwrap();
    let first_batch = repository.list().unwrap();
    assert_eq!(first_batch.len(), 1_000);
    assert_eq!(first_batch[0].pending_match_id, 1);
    assert_eq!(first_batch[999].pending_match_id, 1_000);
    assert_eq!(first_batch[0].expires_at, Some(0));
    assert!(repository.save(&first_batch[0], 0, 2_000).unwrap());
    assert_eq!(
        repository.list().unwrap().last().unwrap().pending_match_id,
        1_001
    );
}

#[test]
fn invalid_identifiers_payloads_and_revisions_fail_without_writes() {
    let (_file, repository) = fixture();
    assert!(
        repository
            .create_or_get(0, 2, "owner", json!({}), 0)
            .is_err()
    );
    assert!(
        repository
            .create_or_get(1, -2, "owner", json!({}), 0)
            .is_err()
    );
    assert!(repository.create_or_get(1, 2, " ", json!({}), 0).is_err());
    assert!(
        repository
            .create_or_get(1, 2, &"a".repeat(201), json!({}), 0)
            .is_err()
    );
    assert!(
        repository
            .create_or_get(1, 2, "owner", json!([]), 0)
            .is_err()
    );
    assert!(repository.get(1, 2).unwrap().is_none());
    let mut record = repository
        .create_or_get(1, 2, "owner", json!({}), 0)
        .unwrap();
    record.channel_id = Some(0);
    assert!(repository.save(&record, 0, 1).is_err());
    record.channel_id = None;
    record.expires_at = Some(-1);
    assert!(repository.save(&record, 0, 1).is_err());
    record.expires_at = None;
    record.payload = Value::Null;
    assert!(repository.save(&record, 0, 1).is_err());
    record.payload = json!({});
    assert!(repository.save(&record, -1, 1).is_err());
    assert!(repository.save(&record, i64::MAX, 1).is_err());
    assert_eq!(repository.get(1, 2).unwrap().unwrap().revision, 0);
}

#[test]
fn canonical_schema_rejects_invalid_payload_and_scoped_identity_direct_writes() {
    let (file, _repository) = fixture();
    let connection = Connection::open(file.path()).unwrap();
    for (guild, payload) in [(0, "{}"), (1, "[]"), (1, "broken")] {
        assert!(connection.execute("INSERT INTO dota_spectators(guild_id,pending_match_id,marker,payload,updated_at) VALUES(?1,1,'owner',?2,0)", params![guild, payload]).is_err());
    }
}
