use super::*;

use rusqlite::Connection;
use tempfile::NamedTempFile;

fn repository() -> (NamedTempFile, VanityTaxRepository) {
    let database = NamedTempFile::new().expect("create disposable database");
    let connection = Connection::open(database.path()).expect("open disposable database");
    connection
        .execute_batch(
            "CREATE TABLE vanity_tax_enforcements (
                 guild_id INTEGER NOT NULL,
                 discord_id INTEGER NOT NULL,
                 enforced_by INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (guild_id, discord_id)
             );",
        )
        .expect("create fixture-only Python-compatible table");
    drop(connection);
    let repository = VanityTaxRepository::new(database.path());
    (database, repository)
}

#[test]
fn fixture_get_is_guild_scoped_and_ordered() {
    let (_database, repository) = repository();
    repository
        .set_vanity_tax_enforcement_at(Some(7), 20, true, 1, 100)
        .expect("set first enforcement");
    repository
        .set_vanity_tax_enforcement_at(Some(7), -5, true, 1, 101)
        .expect("set signed enforcement");
    repository
        .set_vanity_tax_enforcement_at(Some(8), 99, true, 1, 102)
        .expect("set other-guild enforcement");

    assert_eq!(
        repository
            .get_vanity_tax_enforcements(Some(7))
            .expect("load enforcements"),
        BTreeSet::from([-5, 20])
    );
}

#[test]
fn fixture_upsert_updates_actor_and_timestamp() {
    let (database, repository) = repository();
    repository
        .set_vanity_tax_enforcement_at(Some(7), 20, true, 1, 100)
        .expect("insert enforcement");
    repository
        .set_vanity_tax_enforcement_at(Some(7), 20, true, 2, 200)
        .expect("update enforcement");

    let connection = Connection::open(database.path()).expect("inspect fixture");
    let row = connection
        .query_row(
            "SELECT enforced_by, created_at FROM vanity_tax_enforcements
             WHERE guild_id=7 AND discord_id=20",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("load enforcement metadata");
    assert_eq!(row, (2, 200));
}

#[test]
fn fixture_clear_returns_authoritative_set() {
    let (_database, repository) = repository();
    repository
        .set_vanity_tax_enforcement_at(None, 20, true, 1, 100)
        .expect("insert first enforcement");
    repository
        .set_vanity_tax_enforcement_at(None, 30, true, 1, 100)
        .expect("insert second enforcement");

    let current = repository
        .set_vanity_tax_enforcement_at(None, 20, false, 2, 200)
        .expect("clear enforcement");
    assert_eq!(current, BTreeSet::from([30]));
}
