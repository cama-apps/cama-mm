use std::path::Path;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42;
const PLAYER: i64 = 7;

fn fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary Dig sweep database");
    Connection::open(file.path())
        .expect("open fixture")
        .execute_batch(
            "CREATE TABLE players (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 jopacoin_balance INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE tunnels (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 depth INTEGER NOT NULL DEFAULT 0,
                 prestige_level INTEGER NOT NULL DEFAULT 0,
                 boss_progress TEXT DEFAULT '{}',
                 current_run_jc INTEGER NOT NULL DEFAULT 0,
                 current_run_artifacts INTEGER NOT NULL DEFAULT 0,
                 current_run_events INTEGER NOT NULL DEFAULT 0,
                 luminosity INTEGER NOT NULL DEFAULT 100,
                 lantern_stub_date TEXT,
                 pinnacle_boss_id TEXT,
                 pinnacle_phase INTEGER NOT NULL DEFAULT 0,
                 pinnacle_hp_remaining INTEGER,
                 pinnacle_last_engaged_at INTEGER,
                 hard_hat_charges INTEGER NOT NULL DEFAULT 0,
                 grappling_hook_charges INTEGER NOT NULL DEFAULT 0,
                 sonar_skip_pending INTEGER NOT NULL DEFAULT 0,
                 reinforced_until INTEGER,
                 void_bait_digs INTEGER NOT NULL DEFAULT 0,
                 trap_active INTEGER NOT NULL DEFAULT 0,
                 revenge_target INTEGER,
                 revenge_type TEXT,
                 revenge_until INTEGER,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE dig_artifacts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 artifact_id TEXT NOT NULL,
                 found_at INTEGER NOT NULL,
                 is_relic INTEGER NOT NULL DEFAULT 0,
                 equipped INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE dig_inventory (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 item_type TEXT NOT NULL,
                 queued INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE dig_actions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL,
                 actor_id INTEGER NOT NULL,
                 target_id INTEGER,
                 action_type TEXT NOT NULL,
                 depth_before INTEGER NOT NULL,
                 depth_after INTEGER NOT NULL,
                 jc_delta INTEGER NOT NULL DEFAULT 0,
                 detail TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE dig_active_duels (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 boss_id TEXT NOT NULL,
                 tier INTEGER NOT NULL,
                 mechanic_id TEXT NOT NULL,
                 risk_tier TEXT NOT NULL,
                 wager INTEGER NOT NULL,
                 player_hp INTEGER NOT NULL,
                 boss_hp INTEGER NOT NULL,
                 round_num INTEGER NOT NULL,
                 round_log TEXT NOT NULL DEFAULT '[]',
                 pending_prompt TEXT,
                 rng_state TEXT NOT NULL,
                 status_effects TEXT NOT NULL DEFAULT '{}',
                 echo_applied INTEGER NOT NULL DEFAULT 0,
                 echo_killer_id INTEGER,
                 player_hit REAL NOT NULL,
                 player_dmg INTEGER NOT NULL,
                 boss_hit REAL NOT NULL,
                 boss_dmg INTEGER NOT NULL,
                 crit_chance REAL NOT NULL DEFAULT 0,
                 crit_bonus INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 last_interaction_at INTEGER NOT NULL,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE economy_ledger_context (
                 id INTEGER PRIMARY KEY CHECK(id = 1),
                 source TEXT,
                 actor_id INTEGER,
                 related_type TEXT,
                 related_id TEXT,
                 reason TEXT,
                 metadata TEXT
             );",
        )
        .expect("create fixture schema");
    file
}

fn repository(path: &Path) -> DigSweepRepository {
    DigSweepRepository::new(path)
}

fn add_player(path: &Path, discord_id: i64, balance: i64) {
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO players (discord_id, guild_id, jopacoin_balance)
             VALUES (?1, ?2, ?3)",
            params![discord_id, GUILD, balance],
        )
        .expect("insert player");
}

fn add_tunnel(path: &Path, depth: i64) {
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO tunnels (discord_id, guild_id, depth)
             VALUES (?1, ?2, ?3)",
            params![PLAYER, GUILD, depth],
        )
        .expect("insert tunnel");
}

fn key() -> TunnelKey {
    TunnelKey {
        discord_id: PLAYER,
        guild_id: Some(GUILD),
    }
}

fn balance(path: &Path, discord_id: i64) -> i64 {
    Connection::open(path)
        .expect("open fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, GUILD],
            |row| row.get(0),
        )
        .expect("balance")
}

fn columns(path: &Path) -> ConsumableColumns {
    Connection::open(path)
        .expect("open fixture")
        .query_row(
            "SELECT depth, hard_hat_charges, grappling_hook_charges,
                    sonar_skip_pending, reinforced_until, void_bait_digs
             FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![PLAYER, GUILD],
            |row| {
                Ok(ConsumableColumns {
                    depth: row.get(0)?,
                    hard_hat_charges: row.get(1)?,
                    grappling_hook_charges: row.get(2)?,
                    sonar_skip_pending: row.get::<_, i64>(3)? != 0,
                    reinforced_until: row.get(4)?,
                    void_bait_digs: row.get(5)?,
                })
            },
        )
        .expect("consumable columns")
}

#[test]
fn test_double_prestige_credits_once() {
    let file = fixture();
    add_player(file.path(), PLAYER, 10);
    add_tunnel(file.path(), 350);
    let repository = repository(file.path());
    let request = PrestigeClaimRequest {
        key: key(),
        expected_depth: 350,
        expected_prestige_level: 0,
        reward: 650,
        relic_id: "prestige_relic",
        found_at: 1_000,
    };
    assert!(matches!(
        repository
            .claim_prestige_atomic(request)
            .expect("first claim"),
        PrestigeClaimOutcome::Claimed {
            prestige_level: 1,
            balance_after: 660,
            ..
        }
    ));
    assert_eq!(
        repository
            .claim_prestige_atomic(request)
            .expect("stale second claim"),
        PrestigeClaimOutcome::Conflict
    );
    assert_eq!(balance(file.path(), PLAYER), 660);
    let relics: i64 = Connection::open(file.path())
        .expect("open fixture")
        .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| row.get(0))
        .expect("relic count");
    assert_eq!(relics, 1);
}

#[test]
fn test_trap_branch_applies_all_writes_together() {
    let file = fixture();
    add_player(file.path(), 1, 100);
    add_player(file.path(), 2, 20);
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id, guild_id, depth, trap_active)
             VALUES (1, ?1, 50, 0)",
            [GUILD],
        )
        .expect("insert actor tunnel");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id, guild_id, depth, trap_active)
             VALUES (2, ?1, 30, 1)",
            [GUILD],
        )
        .expect("insert target tunnel");
    drop(connection);

    let outcome = repository(file.path())
        .atomic_sabotage(SabotageRequest {
            guild_id: Some(GUILD),
            actor_id: 1,
            target_id: 2,
            actor_jc_cost: 10,
            target_jc_credit: 5,
            target_depth_delta: 0,
            actor_depth_delta: -3,
            clear_target_trap: true,
            action_type: "sabotage",
            detail_json: Some(
                r#"{"target_id":2,"trap_triggered":true,"jc_lost":10,"blocks_lost":3}"#,
            ),
            created_at: 1_700_000_000,
        })
        .expect("atomic trap settlement");

    assert_eq!(outcome.actor_balance_after, 90);
    assert_eq!(outcome.target_balance_after, 25);
    assert_eq!(outcome.actor_depth, 47);
    assert_eq!(outcome.target_depth, 30);
    assert!(!outcome.target_trap_active);
    let action_count: i64 = Connection::open(file.path())
        .expect("open fixture")
        .query_row(
            "SELECT COUNT(*) FROM dig_actions
             WHERE actor_id = 1 AND action_type = 'sabotage'",
            [],
            |row| row.get(0),
        )
        .expect("sabotage audit count");
    assert_eq!(action_count, 1);
}

#[test]
fn test_abandon_clears_pinnacle_state() {
    let file = fixture();
    add_player(file.path(), PLAYER, 20);
    add_tunnel(file.path(), 349);
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "UPDATE tunnels SET pinnacle_phase = 2, pinnacle_boss_id = 'deep_one',
                                pinnacle_hp_remaining = 88
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![PLAYER, GUILD],
        )
        .expect("prime pinnacle");
    assert_eq!(
        repository(file.path())
            .abandon_atomic(key(), 349, 34)
            .expect("abandon"),
        AbandonOutcome::Abandoned {
            refund: 34,
            balance_after: 54
        }
    );
    let state: (i64, i64, Option<String>) = Connection::open(file.path())
        .expect("open fixture")
        .query_row(
            "SELECT depth, pinnacle_phase, pinnacle_boss_id FROM tunnels",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("reset tunnel");
    assert_eq!(state, (0, 0, None));
}

#[test]
fn test_lock_preserves_active_prep() {
    let file = fixture();
    add_tunnel(file.path(), 100);
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "UPDATE tunnels SET boss_progress =
             '{\"100\":{\"status\":\"active\",\"active_prep\":{\"item\":\"warding_salts\"}}}'",
            [],
        )
        .expect("prime progress");
    let progress = repository(file.path())
        .merge_locked_boss(key(), 100, "crystal_guardian")
        .expect("merge lock")
        .expect("tunnel");
    let values: (String, String) = Connection::open(file.path())
        .expect("open fixture")
        .query_row(
            "SELECT json_extract(?1, '$.\"100\".active_prep.item'),
                    json_extract(?1, '$.\"100\".boss_id')",
            [progress],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read merged JSON");
    assert_eq!(
        values,
        ("warding_salts".to_owned(), "crystal_guardian".to_owned())
    );
}

#[test]
fn test_repeat_blocked_attempt_costs_and_logs() {
    let file = fixture();
    add_player(file.path(), PLAYER, 100);
    add_player(file.path(), 8, 50);
    let outcome = repository(file.path())
        .record_blocked_attempt_atomic(BlockedAttemptRequest {
            guild_id: Some(GUILD),
            actor_id: PLAYER,
            target_id: 8,
            cost: 20,
            requested_tip: 0,
            action_type: "sabotage_blocked",
            detail_json: "{\"duplicate_block\":true,\"cost\":20}",
            depth: 100,
            created_at: 500,
        })
        .expect("blocked attempt");
    assert_eq!(outcome.actor_balance_after, 80);
    assert_eq!(outcome.target_balance_after, 50);
    let detail: String = Connection::open(file.path())
        .expect("open fixture")
        .query_row(
            "SELECT detail FROM dig_actions WHERE id = ?1",
            [outcome.action_id],
            |row| row.get(0),
        )
        .expect("audit detail");
    assert!(detail.contains("duplicate_block"));
}

#[test]
fn test_restore_is_a_noop_on_same_day_retry() {
    let file = fixture();
    add_tunnel(file.path(), 5);
    Connection::open(file.path())
        .expect("open fixture")
        .execute("UPDATE tunnels SET luminosity = 50", [])
        .expect("prime luminosity");
    let repository = repository(file.path());
    assert_eq!(
        repository
            .restore_lantern_once_atomic(key(), "2026-08-11", 5)
            .expect("first restore"),
        LanternRestoreOutcome::Restored { luminosity: 55 }
    );
    assert_eq!(
        repository
            .restore_lantern_once_atomic(key(), "2026-08-11", 5)
            .expect("retry"),
        LanternRestoreOutcome::AlreadyRestored { luminosity: 55 }
    );
}

#[test]
fn test_failed_dig_keeps_neither_the_item_nor_its_charges() {
    let file = fixture();
    add_tunnel(file.path(), 10);
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO dig_inventory
                 (discord_id, guild_id, item_type, queued, created_at)
             VALUES (?1, ?2, 'hard_hat', 1, 1)",
            params![PLAYER, GUILD],
        )
        .expect("queue hard hat");
    // Resolve the row id on a fresh connection rather than relying on connection-local state.
    let item_id: i64 = Connection::open(file.path())
        .expect("open fixture")
        .query_row("SELECT id FROM dig_inventory", [], |row| row.get(0))
        .expect("item id");
    let expected = columns(file.path());
    let mut next = expected;
    next.depth = 11;
    next.hard_hat_charges = 3;
    let error = repository(file.path())
        .commit_consumable_dig_atomic(CommitConsumableDigRequest {
            key: key(),
            expected,
            next,
            consumed_item_ids: &[item_id + 99],
        })
        .expect_err("missing item forces rollback");
    assert!(matches!(
        error,
        DigSweepRepositoryError::MissingQueuedItem(_)
    ));
    assert_eq!(columns(file.path()), expected);
    let inventory_count: i64 = Connection::open(file.path())
        .expect("open fixture")
        .query_row("SELECT COUNT(*) FROM dig_inventory", [], |row| row.get(0))
        .expect("inventory count");
    assert_eq!(inventory_count, 1);
}

#[test]
fn test_sonar_pulse_bought_while_one_is_pending_is_not_destroyed() {
    let file = fixture();
    add_tunnel(file.path(), 20);
    Connection::open(file.path())
        .expect("open fixture")
        .execute_batch(
            "UPDATE tunnels SET sonar_skip_pending = 1;
             INSERT INTO dig_inventory
                 (discord_id, guild_id, item_type, queued, created_at)
             VALUES (7, 42, 'sonar_pulse', 1, 1);",
        )
        .expect("prime sonar");
    let item_id: i64 = Connection::open(file.path())
        .expect("open fixture")
        .query_row("SELECT id FROM dig_inventory", [], |row| row.get(0))
        .expect("item id");
    let expected = columns(file.path());
    let mut next = expected;
    next.depth += 1;
    // The old pending skip was consumed, then the newly queued pulse re-arms it.
    next.sonar_skip_pending = true;
    assert_eq!(
        repository(file.path())
            .commit_consumable_dig_atomic(CommitConsumableDigRequest {
                key: key(),
                expected,
                next,
                consumed_item_ids: &[item_id],
            })
            .expect("atomic dig"),
        CommitConsumableDigOutcome::Committed(next)
    );
    assert!(columns(file.path()).sonar_skip_pending);
}

#[test]
fn test_stale_duel_is_claimed_atomically_not_read_then_cleared() {
    let file = fixture();
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO dig_active_duels (
                 discord_id, guild_id, boss_id, tier, mechanic_id, risk_tier,
                 wager, player_hp, boss_hp, round_num, rng_state, player_hit,
                 player_dmg, boss_hit, boss_dmg, created_at, last_interaction_at)
             VALUES (?1, ?2, 'warden', 1, 'choice', 'normal', 25,
                     100, 100, 2, 'seed', 0.8, 5, 0.7, 4, 1, 2)",
            params![PLAYER, GUILD],
        )
        .expect("insert duel");
    let repository = repository(file.path());
    assert_eq!(
        repository
            .claim_active_duel_atomic(key())
            .expect("first claim")
            .expect("duel")
            .wager,
        25
    );
    assert_eq!(
        repository.claim_active_duel_atomic(key()).expect("retry"),
        None
    );
}

#[test]
fn test_ward_block_is_still_audited() {
    let file = fixture();
    add_player(file.path(), PLAYER, 100);
    add_player(file.path(), 8, 10);
    let outcome = repository(file.path())
        .record_blocked_attempt_atomic(BlockedAttemptRequest {
            guild_id: Some(GUILD),
            actor_id: PLAYER,
            target_id: 8,
            cost: 20,
            requested_tip: 10,
            action_type: "sabotage_ward_blocked",
            detail_json: "{\"target_id\":8,\"protection_source\":\"ward\"}",
            depth: 75,
            created_at: 501,
        })
        .expect("ward audit");
    assert_eq!(outcome.actor_balance_after, 80);
    assert_eq!(outcome.target_balance_after, 20);
    let audit: (String, String) = Connection::open(file.path())
        .expect("open fixture")
        .query_row(
            "SELECT action_type, detail FROM dig_actions WHERE id = ?1",
            [outcome.action_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("audit row");
    assert_eq!(audit.0, "sabotage_ward_blocked");
    assert!(audit.1.contains("\"protection_source\":\"ward\""));
}
