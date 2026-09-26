use super::*;
use crate::test_support::initialize_test_database;

#[test]
fn recorded_names_survive_a_restart_and_stay_per_guild() {
    let database = tempfile::NamedTempFile::new().expect("archive database");
    initialize_test_database(database.path()).expect("archive schema");
    let archive = PlayerNameArchive::load(database.path()).expect("empty archive");
    archive.record(
        42,
        [
            (7, "Seven".to_owned()),
            (697_288_295_876_526_160, "Offline Player".to_owned()),
        ],
    );
    archive.record(99, [(7, "Seven Elsewhere".to_owned())]);

    let restarted = PlayerNameArchive::load(database.path()).expect("stored archive");
    assert_eq!(
        restarted.names(42, &[7, 697_288_295_876_526_160, 8]),
        DiscordGuildMemberRenderNames::from([
            (7, "Seven".to_owned()),
            (697_288_295_876_526_160, "Offline Player".to_owned()),
        ])
    );
    assert_eq!(
        restarted.names(99, &[7]),
        DiscordGuildMemberRenderNames::from([(7, "Seven Elsewhere".to_owned())])
    );
}

#[test]
fn renames_replace_the_stored_name_and_unreadable_names_are_ignored() {
    let database = tempfile::NamedTempFile::new().expect("archive database");
    initialize_test_database(database.path()).expect("archive schema");
    let archive = PlayerNameArchive::load(database.path()).expect("empty archive");
    archive.record(42, [(7, "Old Nick".to_owned())]);
    archive.record(42, [(7, "New Nick".to_owned())]);
    for unreadable in ["", "  ", "697288295876526160", "<@697288295876526160>"] {
        archive.record(42, [(7, unreadable.to_owned())]);
    }

    let expected = DiscordGuildMemberRenderNames::from([(7, "New Nick".to_owned())]);
    assert_eq!(archive.names(42, &[7]), expected);
    assert_eq!(
        PlayerNameArchive::load(database.path())
            .expect("stored archive")
            .names(42, &[7]),
        expected
    );
}
