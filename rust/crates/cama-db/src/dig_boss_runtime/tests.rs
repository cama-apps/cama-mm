use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::core_repositories::{NewPlayer, PlayerRepository};
use crate::schema_manager::initialize_or_migrate;

const PLAYER: i64 = -83_001;
const GUILD: i64 = 83_002;
const KEY: DigBossRuntimeKey = DigBossRuntimeKey {
    discord_id: PLAYER,
    guild_id: GUILD,
};

fn fixture() -> NamedTempFile {
    let database = NamedTempFile::new().expect("boss runtime database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(PLAYER, "boss-runtime", Some(GUILD)))
        .expect("player");
    let connection = Connection::open(database.path()).expect("open fixture");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("player balance");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              boss_attempts,luminosity,stat_points,tunnel_name,cheer_data)
             VALUES (?1,?2,24,24,0,?3,2,80,5,'Boss Runtime',?4)",
            params![
                PLAYER,
                GUILD,
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item":"whetstone"},"future_field":17}}"#,
                r#"[{"user_id":9,"expires_at":999999}]"#,
            ],
        )
        .expect("tunnel");
    database
}

fn victory_state(snapshot: &DigBossRuntimeSnapshot) -> DigBossRuntimeState {
    let mut state = DigBossRuntimeState::from(snapshot);
    state.depth = 25;
    state.max_depth = 25;
    state.balance = 165;
    state.boss_progress_json =
        Some(r#"{"25":{"boss_id":"grothak","status":"defeated","future_field":17}}"#.to_owned());
    state.boss_attempts = 0;
    state.last_dig_at = Some(1_700_000_000);
    state.cheer_data_json = None;
    state
}

fn victory_commit<'a>(
    snapshot: &'a DigBossRuntimeSnapshot,
    state: &'a DigBossRuntimeState,
) -> DigBossRuntimeCommit<'a> {
    DigBossRuntimeCommit {
        expected: snapshot,
        proposed: state,
        audit: Some(DigBossRuntimeAudit {
            action_type: "boss_fight",
            target_id: None,
            detail_json: r#"{"boss_id":"grothak","won":true,"gross_jc":100}"#,
            created_at: 1_700_000_000,
        }),
        echo: Some(DigBossRuntimeEcho {
            boss_id: "grothak",
            depth: 25,
            killer_id: PLAYER,
            weakened_until: 1_700_086_400,
        }),
        artifact: None,
        vanity_tax: 0,
    }
}

fn seed_queued_preparation(database: &NamedTempFile, item_type: &str) -> i64 {
    let connection = Connection::open(database.path()).expect("inventory DB");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,?3,1,100)",
            params![PLAYER, GUILD, item_type],
        )
        .expect("queued preparation");
    connection.last_insert_rowid()
}

#[test]
fn preparation_activation_consumes_exact_row_with_guarded_progress_update() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let selected_id = seed_queued_preparation(&database, "tempered_whetstone");
    let untouched_id = seed_queued_preparation(&database, "rescue_line");
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let progress = r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item_type":"tempered_whetstone","used":false},"future_field":17}}"#;
    assert_eq!(
        repository
            .activate_preparation(DigBossPreparationActivation {
                expected: &snapshot,
                inventory_item_id: selected_id,
                item_type: "tempered_whetstone",
                proposed_boss_progress_json: progress,
            })
            .expect("activation"),
        DigBossPreparationActivationOutcome::Applied
    );
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT boss_progress FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, String>(0),
            )
            .expect("boss progress"),
        progress
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory WHERE id=?1",
                [selected_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("selected count"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT item_type FROM dig_inventory WHERE id=?1",
                [untouched_id],
                |row| row.get::<_, String>(0),
            )
            .expect("untouched item"),
        "rescue_line"
    );
}

#[test]
fn preparation_activation_rolls_back_inventory_when_tunnel_update_fails() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let item_id = seed_queued_preparation(&database, "warding_salts");
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let before = snapshot.boss_progress_json.clone();
    Connection::open(database.path())
        .expect("trigger DB")
        .execute_batch(
            "CREATE TRIGGER fail_prep_progress BEFORE UPDATE OF boss_progress ON tunnels
             BEGIN SELECT RAISE(ABORT,'injected preparation failure'); END;",
        )
        .expect("failure trigger");
    assert!(
        repository
            .activate_preparation(DigBossPreparationActivation {
                expected: &snapshot,
                inventory_item_id: item_id,
                item_type: "warding_salts",
                proposed_boss_progress_json: r#"{"25":{"active_prep":{"item_type":"warding_salts","used":false}}}"#,
            })
            .is_err()
    );
    let after = repository.snapshot(KEY).expect("after").expect("player");
    assert_eq!(after.boss_progress_json, before);
    assert_eq!(
        Connection::open(database.path())
            .expect("verify DB")
            .query_row(
                "SELECT queued FROM dig_inventory WHERE id=?1",
                [item_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("queued row"),
        1
    );
}

#[test]
fn full_victory_commits_tunnel_balance_audit_ledger_and_echo_together() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let state = victory_state(&snapshot);
    let outcome = repository
        .commit(victory_commit(&snapshot, &state))
        .expect("victory");
    let DigBossRuntimeCommitOutcome::Applied(receipt) = outcome else {
        panic!("expected applied victory, got {outcome:?}");
    };
    assert_eq!(receipt.balance_after, 165);
    assert!(receipt.action_id.is_some());
    assert_eq!(receipt.artifact_database_id, None);

    let connection = Connection::open(database.path()).expect("verify DB");
    let row = connection
        .query_row(
            "SELECT t.depth,t.max_depth,t.boss_progress,
                    CAST(COALESCE(t.boss_attempts,0) AS INTEGER),
                    t.cheer_data,p.jopacoin_balance
               FROM tunnels t JOIN players p USING(discord_id,guild_id)
              WHERE t.discord_id=?1 AND t.guild_id=?2",
            params![PLAYER, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .expect("persisted victory");
    assert_eq!((row.0, row.1, row.3, row.4, row.5), (25, 25, 0, None, 165));
    assert!(row.2.contains("future_field"));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 AND action_type='boss_fight'
                    AND jc_delta=65",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("audit count"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE account_id=?1 AND guild_id=?2 AND source='dig' AND delta=65",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("ledger count"),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context count"),
        0
    );
    assert_eq!(
        repository
            .active_echo(GUILD, "grothak", 1_700_000_001)
            .expect("echo"),
        Some((25, PLAYER, 1_700_086_400))
    );
}

#[test]
fn pinnacle_victory_fuses_generated_relic_with_state_balance_and_audit() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let state = victory_state(&snapshot);
    let mut commit = victory_commit(&snapshot, &state);
    commit.artifact = Some(DigBossRuntimeArtifactGrant {
        artifact_id: "pinnacle:Crown:Scales and Tribute:hp_plus_1:boss_payout_5",
        found_at: 1_700_000_000,
        is_relic: true,
    });
    let DigBossRuntimeCommitOutcome::Applied(receipt) =
        repository.commit(commit).expect("pinnacle victory")
    else {
        panic!("expected applied pinnacle victory");
    };
    let artifact_database_id = receipt
        .artifact_database_id
        .expect("atomic artifact receipt");
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT artifact_id,is_relic,equipped FROM dig_artifacts WHERE id=?1",
                [artifact_database_id],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            )
            .expect("artifact row"),
        (
            "pinnacle:Crown:Scales and Tribute:hp_plus_1:boss_payout_5".to_owned(),
            1,
            0,
        )
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        165
    );
}

#[test]
fn pinnacle_relic_insert_failure_rolls_back_entire_victory() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let state = victory_state(&snapshot);
    Connection::open(database.path())
        .expect("trigger DB")
        .execute_batch(
            "CREATE TRIGGER fail_pinnacle_relic BEFORE INSERT ON dig_artifacts
             BEGIN SELECT RAISE(ABORT,'injected pinnacle relic failure'); END;",
        )
        .expect("failure trigger");
    let mut commit = victory_commit(&snapshot, &state);
    commit.artifact = Some(DigBossRuntimeArtifactGrant {
        artifact_id: "pinnacle:Crown:Scales and Tribute:hp_plus_1:boss_payout_5",
        found_at: 1_700_000_000,
        is_relic: true,
    });
    assert!(repository.commit(commit).is_err());
    assert_eq!(
        repository.snapshot(KEY).expect("after").expect("player"),
        snapshot
    );
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| row
                .get::<_, i64>(0))
            .expect("artifact count"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        0
    );
}

#[test]
fn vanity_tax_preserves_gross_and_tax_ledgers_while_audit_records_net_change() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let mut state = victory_state(&snapshot);
    state.balance = 155;
    let mut request = victory_commit(&snapshot, &state);
    request.vanity_tax = 10;

    let outcome = repository.commit(request).expect("taxed victory");
    let DigBossRuntimeCommitOutcome::Applied(receipt) = outcome else {
        panic!("expected applied victory, got {outcome:?}");
    };
    assert_eq!(receipt.balance_after, 155);

    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        155
    );
    let ledger_rows = connection
        .prepare(
            "SELECT source,delta FROM economy_ledger_entries
              WHERE account_id=?1 AND guild_id=?2
                AND source IN ('dig','vanity_tax')
              ORDER BY source",
        )
        .expect("ledger query")
        .query_map(params![PLAYER, GUILD], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("ledger rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("ledger values");
    assert_eq!(
        ledger_rows,
        vec![("dig".to_owned(), 65), ("vanity_tax".to_owned(), -10)]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jc_delta FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 AND action_type='boss_fight'",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("audit delta"),
        55
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context count"),
        0
    );
}

#[test]
fn stale_actor_snapshot_rolls_back_every_boss_side_effect() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let state = victory_state(&snapshot);
    Connection::open(database.path())
        .expect("racer")
        .execute(
            "UPDATE tunnels SET depth=23 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("race");
    assert_eq!(
        repository
            .commit(victory_commit(&snapshot, &state))
            .expect("conflict"),
        DigBossRuntimeCommitOutcome::Conflict
    );
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        100
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audits"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_boss_echoes", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("echoes"),
        0
    );
}

#[test]
fn audit_failure_rolls_back_actor_balance_tunnel_echo_and_ledger() {
    let database = fixture();
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.snapshot(KEY).expect("snapshot").expect("player");
    let state = victory_state(&snapshot);
    Connection::open(database.path())
        .expect("trigger DB")
        .execute_batch(
            "CREATE TRIGGER fail_boss_audit BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='boss_fight'
             BEGIN SELECT RAISE(ABORT,'injected boss audit failure'); END;",
        )
        .expect("failure trigger");
    assert!(
        repository
            .commit(victory_commit(&snapshot, &state))
            .is_err()
    );
    let persisted = repository
        .snapshot(KEY)
        .expect("persisted")
        .expect("player");
    assert_eq!(persisted.depth, 24);
    assert_eq!(persisted.balance, 100);
    assert_eq!(persisted.boss_attempts, 2);
    assert!(persisted.cheer_data_json.is_some());
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE account_id=?1 AND guild_id=?2 AND source='dig' AND delta=65",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("ledger"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_boss_echoes", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("echo"),
        0
    );
}

fn paused_duel() -> DigBossPausedDuelRow {
    DigBossPausedDuelRow {
        key: KEY,
        boss_id: "grothak".to_owned(),
        boundary: 25,
        mechanic_id: "grothak_earthquake".to_owned(),
        risk_tier: "bold".to_owned(),
        wager: 20,
        player_hp: 3,
        boss_hp: 4,
        round_num: 2,
        round_log_json: r#"[{"round":1}]"#.to_owned(),
        pending_prompt_json: Some(r#"{"title":"Brace"}"#.to_owned()),
        rng_state: "seed:17".to_owned(),
        status_effects_json:
            r#"{"gear_snapshot_ids":[1,2],"armor_snapshot_id":2,"pet_boss_assist":{"bonus_pct":12}}"#
                .to_owned(),
        echo_applied: true,
        echo_killer_id: Some(90),
        player_hit: 0.42,
        player_damage: 2,
        boss_hit: 0.45,
        boss_damage: 1,
        critical_chance: 0.15,
        critical_bonus: 1,
        created_at: 1_700_000_000,
        last_interaction_at: 1_700_000_001,
    }
}

#[test]
fn paused_duel_survives_repository_recreation_and_is_claimed_once() {
    let database = fixture();
    DigBossRuntimeRepository::new(database.path())
        .save_active_duel(&paused_duel())
        .expect("save duel");
    let recovered = DigBossRuntimeRepository::new(database.path())
        .active_duel(KEY)
        .expect("read duel")
        .expect("durable duel");
    assert_eq!(recovered, paused_duel());
    let repository = DigBossRuntimeRepository::new(database.path());
    assert_eq!(
        repository.claim_active_duel(KEY).expect("first claim"),
        Some(paused_duel())
    );
    assert_eq!(
        repository.claim_active_duel(KEY).expect("second claim"),
        None
    );
}

#[test]
fn resolution_wear_ticks_snapshot_loadout_and_loss_armor_after_swap() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("gear DB");
    for (slot, durability) in [("weapon", 3), ("armor", 2)] {
        connection
            .execute(
                "INSERT INTO dig_gear
                 (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
                 VALUES (?1,?2,?3,1,?4,1,100,'fixture')",
                params![PLAYER, GUILD, slot, durability],
            )
            .expect("snapshot gear");
    }
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository.gear_snapshot(KEY).expect("gear snapshot");
    assert_eq!(snapshot.gear_ids.len(), 2);
    connection
        .execute(
            "UPDATE dig_gear SET equipped=0 WHERE slot='armor' AND discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("unequip old armor");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',2,9,1,101,'swap')",
            params![PLAYER, GUILD],
        )
        .expect("swap armor");
    let swapped_id = connection.last_insert_rowid();
    let receipt = repository
        .tick_gear_after_resolution(KEY, &snapshot, false)
        .expect("loss wear");
    assert_eq!(receipt.broken_ids, vec![snapshot.armor_id.expect("armor")]);
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [snapshot.gear_ids[0]],
                |row| row.get::<_, i64>(0),
            )
            .expect("weapon durability"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [snapshot.armor_id.expect("armor")],
                |row| row.get::<_, i64>(0),
            )
            .expect("old armor durability"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [swapped_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("new armor durability"),
        9
    );
}

#[test]
fn base_lantern_is_claimed_once_while_great_lantern_remains_persistent() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("inventory DB");
    for item in ["lantern", "lantern", "great_lantern"] {
        connection
            .execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,?3,0,100)",
                params![PLAYER, GUILD, item],
            )
            .expect("inventory item");
    }
    let repository = DigBossRuntimeRepository::new(database.path());
    let (lanterns, great) = repository.lantern_state(KEY).expect("lantern state");
    assert_eq!(lanterns.len(), 2);
    assert!(great);
    assert!(repository.consume_lantern(KEY, lanterns[0]).expect("claim"));
    assert!(!repository.consume_lantern(KEY, lanterns[0]).expect("retry"));
    let (remaining, great) = repository.lantern_state(KEY).expect("remaining state");
    assert_eq!(remaining, vec![lanterns[1]]);
    assert!(great);
}

#[test]
fn cheer_commit_guards_both_tunnels_and_creates_cheerer_atomically() {
    let database = fixture();
    let cheerer = PLAYER - 1;
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(cheerer, "cheerer", Some(GUILD)))
        .expect("cheerer player");
    let repository = DigBossRuntimeRepository::new(database.path());
    let snapshot = repository
        .cheer_snapshot(cheerer, PLAYER, GUILD)
        .expect("cheer snapshot")
        .expect("target");
    assert!(!snapshot.cheerer_tunnel_exists);
    let encoded = r#"[{"cheerer_id":-83002,"expires_at":1700003600}]"#;
    assert_eq!(
        repository
            .commit_cheer(DigBossCheerCommit {
                expected: &snapshot,
                proposed_target_cheer_data_json: encoded,
                cheerer_last_cheer_at: 1_700_000_000,
                create_cheerer_tunnel_name: "Atomic Cheer",
            })
            .expect("commit cheer"),
        DigBossCheerCommitOutcome::Applied
    );
    assert_eq!(
        repository
            .commit_cheer(DigBossCheerCommit {
                expected: &snapshot,
                proposed_target_cheer_data_json: encoded,
                cheerer_last_cheer_at: 1_700_000_000,
                create_cheerer_tunnel_name: "Atomic Cheer",
            })
            .expect("stale retry"),
        DigBossCheerCommitOutcome::Conflict
    );
    let connection = Connection::open(database.path()).expect("verify cheer DB");
    let (name, last_cheer): (String, i64) = connection
        .query_row(
            "SELECT tunnel_name,last_cheer_at FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![cheerer, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("cheerer tunnel");
    assert_eq!(name, "Atomic Cheer");
    assert_eq!(last_cheer, 1_700_000_000);
    assert_eq!(
        connection
            .query_row(
                "SELECT cheer_data FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, String>(0),
            )
            .expect("target cheer data"),
        encoded
    );
}
