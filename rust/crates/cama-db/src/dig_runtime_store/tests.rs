use rusqlite::Connection;
use tempfile::NamedTempFile;

use super::*;
use crate::test_support::copy_migrated_database;

const USER: i64 = 91_001;
const GUILD: i64 = 91_002;

fn fixture() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary database");
    copy_migrated_database(database.path()).expect("migrated schema");
    let connection = Connection::open(database.path()).expect("fixture connection");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match runtime foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES (?1,?2,'runtime-store',100)",
            rusqlite::params![USER, GUILD],
        )
        .expect("player");
    database
}

fn open(database: &NamedTempFile) -> Connection {
    let connection = Connection::open(database.path()).expect("connection");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("pragma");
    connection
}

#[test]
fn tunnel_roundtrips_through_insert_and_full_select() {
    let database = fixture();
    let connection = open(&database);
    assert_eq!(tunnel(&connection, USER, GUILD).expect("select"), None);

    let mut row = DigRuntimeTunnel::new(USER, GUILD, 1_700_000_000);
    row.depth = 12;
    row.max_depth = 20;
    row.total_digs = 3;
    row.last_dig_at = Some(1_699_999_000);
    row.auto_buy_torch = true;
    row.mutations = Some("[\"gills\"]".to_owned());
    assert_eq!(insert_tunnel(&connection, &row).expect("insert"), 1);

    let loaded = tunnel(&connection, USER, GUILD)
        .expect("select")
        .expect("tunnel row");
    assert_eq!(loaded, row);
}

#[test]
fn update_tunnel_cas_requires_the_expected_version() {
    let database = fixture();
    let connection = open(&database);
    let mut row = DigRuntimeTunnel::new(USER, GUILD, 0);
    insert_tunnel(&connection, &row).expect("insert");

    row.depth = 5;
    row.total_digs = 1;
    row.last_dig_at = Some(9);
    assert_eq!(
        update_tunnel_cas(&connection, &row, Some(99), Some(99), Some(99)).expect("stale cas"),
        0
    );
    assert_eq!(
        update_tunnel_cas(&connection, &row, Some(0), Some(0), None).expect("cas"),
        1
    );
    let loaded = tunnel(&connection, USER, GUILD)
        .expect("select")
        .expect("tunnel row");
    assert_eq!(loaded.depth, 5);
    assert_eq!(loaded.last_dig_at, Some(9));
}

#[test]
fn player_balance_helpers_apply_the_exact_cas_semantics() {
    let database = fixture();
    let connection = open(&database);
    assert_eq!(
        player_balance(&connection, USER, GUILD).expect("balance"),
        Some(100)
    );
    assert!(player_exists(&connection, USER, GUILD).expect("exists"));
    assert_eq!(
        update_player_balance_cas(&connection, 90, USER, GUILD, 99).expect("stale cas"),
        0
    );
    assert_eq!(
        update_player_balance_cas(&connection, 90, USER, GUILD, 100).expect("cas"),
        1
    );
    assert_eq!(
        update_player_balance_coalesce_cas(&connection, 80, USER, GUILD, 90).expect("cas"),
        1
    );
    assert_eq!(
        debit_player_balance_if_sufficient(&connection, 1_000, USER, GUILD).expect("guarded"),
        0
    );
    assert_eq!(
        debit_player_balance(&connection, 30, USER, GUILD).expect("debit"),
        1
    );
    assert_eq!(
        player_balance(&connection, USER, GUILD).expect("balance"),
        Some(50)
    );
}

#[test]
fn dig_action_insert_returns_the_row_id_and_detail_updates_are_scoped() {
    let database = fixture();
    let connection = open(&database);
    let action_id = insert_dig_action(
        &connection,
        GUILD,
        USER,
        None,
        "dig",
        0,
        1,
        7,
        "{\"kind\":\"test\"}",
        1_700_000_100,
    )
    .expect("insert");
    assert!(action_id > 0);
    assert_eq!(
        dig_action_detail(&connection, action_id).expect("detail"),
        Some(Some("{\"kind\":\"test\"}".to_owned()))
    );
    assert!(
        dig_action_exists_for_actor(&connection, action_id, USER, GUILD).expect("actor exists")
    );
    assert!(
        !dig_action_exists_for_actor(&connection, action_id, USER + 1, GUILD).expect("wrong actor")
    );
    assert!(
        !dig_action_exists_for_actor(&connection, action_id, USER, GUILD + 1).expect("wrong guild")
    );
    assert_eq!(
        update_dig_action_detail_for_actor(&connection, "{}", action_id, USER + 1, GUILD)
            .expect("scoped update"),
        0
    );
    assert_eq!(
        update_dig_action_detail_for_actor(&connection, "{}", action_id, USER, GUILD)
            .expect("scoped update"),
        1
    );
    assert_eq!(
        dig_action_details_for_delivery(&connection, Some(GUILD), Some(USER), 10).expect("pending"),
        vec![Some("{}".to_owned())]
    );
}

#[test]
fn ledger_context_is_installed_and_cleared() {
    let database = fixture();
    let connection = open(&database);
    set_ledger_context(
        &connection,
        "dig",
        USER,
        "event",
        "test",
        "dig paid cost",
        "{}",
    )
    .expect("set");
    let row: (String, i64) = connection
        .query_row(
            "SELECT source, actor_id FROM economy_ledger_context WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("context row");
    assert_eq!(row, ("dig".to_owned(), USER));
    clear_ledger_context(&connection).expect("clear");
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(count, 0);
}

#[test]
fn inventory_artifact_and_gear_sync_upsert_by_row_id() {
    let database = fixture();
    let connection = open(&database);
    sync_inventory(
        &connection,
        &[DigRuntimeInventoryItem {
            id: 0,
            item_type: "torch".to_owned(),
            queued: true,
        }],
        USER,
        GUILD,
        1_700_000_000,
    )
    .expect("insert path");
    let items = inventory(&connection, USER, GUILD).expect("inventory");
    assert_eq!(items.len(), 1);
    assert!(items[0].queued);
    assert!(inventory_item_exists(&connection, items[0].id, USER, GUILD).expect("exists"));

    let mut updated = items[0].clone();
    updated.queued = false;
    sync_inventory(&connection, &[updated], USER, GUILD, 1_700_000_001).expect("update path");
    let items = inventory(&connection, USER, GUILD).expect("inventory");
    assert_eq!(items.len(), 1);
    assert!(!items[0].queued);
    assert_eq!(
        delete_inventory_item(&connection, items[0].id, USER, GUILD).expect("delete"),
        1
    );

    sync_artifacts(
        &connection,
        &[DigRuntimeArtifact {
            id: 0,
            artifact_id: "old_boot".to_owned(),
            is_relic: false,
            equipped: false,
        }],
        USER,
        GUILD,
        1_700_000_000,
    )
    .expect("artifact insert");
    assert_eq!(
        artifacts(&connection, USER, GUILD)
            .expect("artifacts")
            .len(),
        1
    );

    sync_gear(
        &connection,
        &[DigRuntimeGear {
            id: 0,
            slot: "armor".to_owned(),
            tier: 1,
            durability: 10,
            equipped: true,
            acquired_at: 0,
            source: "shop".to_owned(),
            item_id: None,
        }],
        USER,
        GUILD,
        1_700_000_000,
    )
    .expect("gear insert");
    let gear_rows = gear(&connection, USER, GUILD).expect("gear");
    assert_eq!(gear_rows.len(), 1);
    // A zero acquisition timestamp is replaced by `now` on insert.
    assert_eq!(gear_rows[0].acquired_at, 1_700_000_000);

    assert_eq!(
        insert_starter_weapon(&connection, USER, GUILD, 2, 1_700_000_002).expect("starter"),
        1
    );
    assert_eq!(
        insert_starter_weapon(&connection, USER, GUILD, 2, 1_700_000_003).expect("duplicate"),
        0
    );
}

#[test]
fn leaderboard_projections_keep_their_ordering() {
    let database = fixture();
    let connection = open(&database);
    for (offset, depth, prestige, best_run) in [(0, 10, 0, 5), (1, 30, 0, 0), (2, 20, 1, 9)] {
        let mut row = DigRuntimeTunnel::new(USER + offset, GUILD, 0);
        row.depth = depth;
        row.prestige_level = prestige;
        row.best_run_score = best_run;
        insert_tunnel(&connection, &row).expect("insert");
    }

    let by_depth = top_tunnel_depth_rows(&connection, GUILD).expect("depth rows");
    assert_eq!(
        by_depth.iter().map(|row| row.1).collect::<Vec<_>>(),
        vec![30, 20, 10]
    );
    let leaderboard = leaderboard_tunnel_rows(&connection, GUILD).expect("leaderboard");
    assert_eq!(
        leaderboard
            .iter()
            .map(|row| row.discord_id)
            .collect::<Vec<_>>(),
        vec![USER + 2, USER + 1, USER]
    );
    assert_eq!(
        tunnel_rank_ids(&connection, GUILD).expect("ranks"),
        vec![USER + 2, USER + 1, USER]
    );
    let fame = hall_of_fame_entry_rows(&connection, GUILD).expect("fame");
    assert_eq!(
        fame.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![USER + 2, USER]
    );
    let fame_runtime = hall_of_fame_depth_rows(&connection, GUILD).expect("fame runtime");
    assert_eq!(
        fame_runtime.iter().map(|row| row.1).collect::<Vec<_>>(),
        vec![USER + 2, USER]
    );
    assert_eq!(
        guild_tunnel_stat_rows(&connection, GUILD)
            .expect("stat rows")
            .len(),
        3
    );
}

#[test]
fn slow_drip_claim_cas_and_insert_race_semantics() {
    let database = fixture();
    let connection = open(&database);
    assert_eq!(
        update_slow_drip_claim_cas(&connection, 5, 100, USER, GUILD, "2026-09-01", 0, 0)
            .expect("missing row"),
        0
    );
    assert_eq!(
        insert_slow_drip_claim(&connection, USER, GUILD, "2026-09-01", 5, 100).expect("insert"),
        1
    );
    assert_eq!(
        insert_slow_drip_claim(&connection, USER, GUILD, "2026-09-01", 9, 300)
            .expect("conflict insert"),
        0
    );
    assert_eq!(
        update_slow_drip_claim_cas(&connection, 5, 200, USER, GUILD, "2026-09-01", 5, 100)
            .expect("cas"),
        1
    );
}
