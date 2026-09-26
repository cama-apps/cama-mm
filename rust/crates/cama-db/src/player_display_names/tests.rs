use super::*;
use crate::test_support::copy_migrated_database;

fn fixture() -> (tempfile::TempDir, PlayerDisplayNameRepository) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("names.db");
    copy_migrated_database(&path).expect("migrate");
    (directory, PlayerDisplayNameRepository::new(&path))
}

fn name(discord_id: i64, guild_id: i64, display_name: &str) -> PlayerDisplayName {
    PlayerDisplayName {
        discord_id,
        guild_id,
        display_name: display_name.to_owned(),
    }
}

#[test]
fn fresh_database_has_no_stored_names() {
    let (_directory, repository) = fixture();
    assert_eq!(repository.load_all().expect("load"), Vec::new());
}

#[test]
fn names_are_scoped_per_guild_and_updated_in_place() {
    let (_directory, repository) = fixture();
    repository
        .record(
            &[name(7, 42, "Old Nick"), name(7, 99, "Other Server")],
            1_000,
        )
        .expect("first sighting");
    repository
        .record(&[name(7, 42, "New Nick")], 2_000)
        .expect("rename");

    assert_eq!(
        repository.load_all().expect("load"),
        vec![name(7, 42, "New Nick"), name(7, 99, "Other Server")]
    );
}

#[test]
fn an_older_sighting_never_replaces_a_newer_name() {
    let (_directory, repository) = fixture();
    repository
        .record(&[name(7, 42, "Newer")], 2_000)
        .expect("newer write");
    repository
        .record(&[name(7, 42, "Older")], 1_000)
        .expect("late older write");

    assert_eq!(
        repository.load_all().expect("load"),
        vec![name(7, 42, "Newer")]
    );
}

#[test]
fn recording_is_idempotent_on_retry() {
    let (_directory, repository) = fixture();
    let batch = [name(7, 42, "Seven"), name(8, 42, "Eight")];
    repository.record(&batch, 1_000).expect("first write");
    repository.record(&batch, 1_000).expect("retried write");

    assert_eq!(repository.load_all().expect("load"), batch.to_vec());
}
