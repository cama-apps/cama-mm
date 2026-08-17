use std::path::Path;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

fn fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary relic-rework database");
    Connection::open(file.path())
        .expect("open fixture")
        .execute_batch(
            "CREATE TABLE tunnels (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 prestige_level INTEGER NOT NULL DEFAULT 0,
                 cavein_free_streak INTEGER NOT NULL DEFAULT 0,
                 relic_trim_notice INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE dig_artifacts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 artifact_id TEXT NOT NULL,
                 found_at INTEGER NOT NULL,
                 is_relic INTEGER NOT NULL DEFAULT 0,
                 equipped INTEGER NOT NULL DEFAULT 0
             );",
        )
        .expect("create disposable post-migration schema");
    file
}

fn add_tunnel(path: &Path, discord_id: i64, prestige: i64) {
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO tunnels (discord_id, guild_id, prestige_level)
             VALUES (?1, 0, ?2)",
            params![discord_id, prestige],
        )
        .expect("insert tunnel");
}

fn add_equipped_relics(path: &Path, discord_id: i64, relic_ids: &[&str]) -> Vec<i64> {
    let connection = Connection::open(path).expect("open fixture");
    relic_ids
        .iter()
        .enumerate()
        .map(|(index, relic_id)| {
            connection
                .execute(
                    "INSERT INTO dig_artifacts (
                         discord_id, guild_id, artifact_id, found_at, is_relic, equipped
                     ) VALUES (?1, 0, ?2, ?3, 1, 1)",
                    params![discord_id, relic_id, index as i64 + 1],
                )
                .expect("insert equipped relic");
            connection.last_insert_rowid()
        })
        .collect()
}

fn equipped_ids(path: &Path, discord_id: i64) -> Vec<i64> {
    let connection = Connection::open(path).expect("open fixture");
    let mut statement = connection
        .prepare(
            "SELECT id FROM dig_artifacts
             WHERE discord_id = ?1 AND guild_id = 0 AND equipped = 1
             ORDER BY id",
        )
        .expect("prepare equipped relics");
    statement
        .query_map([discord_id], |row| row.get(0))
        .expect("query equipped relics")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect equipped relics")
}

fn trim_notice(path: &Path, discord_id: i64) -> i64 {
    Connection::open(path)
        .expect("open fixture")
        .query_row(
            "SELECT relic_trim_notice FROM tunnels
             WHERE discord_id = ?1 AND guild_id = 0",
            [discord_id],
            |row| row.get(0),
        )
        .expect("trim notice")
}

#[test]
fn test_relic_cap_migration_trims_to_six_newest() {
    let file = fixture();
    add_tunnel(file.path(), 1, 9);
    let over_ids = add_equipped_relics(
        file.path(),
        1,
        &[
            "midas_splinter",
            "lucky_seam",
            "prospectors_streak",
            "first_light",
            "berserkers_mark",
            "gamblers_edge",
            "deaths_door",
            "mole_claws",
        ],
    );
    add_tunnel(file.path(), 2, 3);
    add_equipped_relics(
        file.path(),
        2,
        &["crystal_compass", "magma_heart", "echo_stone"],
    );
    let report = DigRelicReworkRepository::new(file.path())
        .repair_relic_loadout_cap()
        .expect("repair relic cap");
    assert_eq!(report.flagged_tunnels, 1);
    assert_eq!(report.unequipped_relics, 2);
    assert_eq!(equipped_ids(file.path(), 1), over_ids[2..]);
    assert_eq!(trim_notice(file.path(), 1), 1);
    assert_eq!(equipped_ids(file.path(), 2).len(), 3);
    assert_eq!(trim_notice(file.path(), 2), 0);
}

#[test]
fn test_relic_cap_migration_leaves_exactly_six_untouched() {
    let file = fixture();
    add_tunnel(file.path(), 5, 5);
    let ids = add_equipped_relics(
        file.path(),
        5,
        &[
            "mole_claws",
            "magma_heart",
            "crystal_compass",
            "echo_stone",
            "spore_cloak",
            "frozen_clock",
        ],
    );
    let report = DigRelicReworkRepository::new(file.path())
        .repair_relic_loadout_cap()
        .expect("repair exact-cap relics");
    assert_eq!(report, RelicCapRepairReport::default());
    assert_eq!(equipped_ids(file.path(), 5), ids);
    assert_eq!(trim_notice(file.path(), 5), 0);
}
