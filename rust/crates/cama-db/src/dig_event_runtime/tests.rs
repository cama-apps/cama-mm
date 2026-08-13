use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::{Connection, params};
use serde_json::Value as JsonValue;
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = -81_001;
const GUILD: i64 = 12_345;

struct Fixture {
    database: NamedTempFile,
    repository: DigEventRuntimeRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     jopacoin_balance INTEGER NOT NULL DEFAULT 3,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     depth INTEGER NOT NULL DEFAULT 0,
                     luminosity INTEGER NOT NULL DEFAULT 100,
                     prestige_level INTEGER NOT NULL DEFAULT 0,
                     prestige_perks TEXT NOT NULL DEFAULT '[]',
                     boss_progress TEXT NOT NULL DEFAULT '{}',
                     streak_days INTEGER NOT NULL DEFAULT 0,
                     temp_buffs TEXT,
                     temp_curses TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_inventory (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     item_type TEXT NOT NULL,
                     queued INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE dig_artifacts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     artifact_id TEXT NOT NULL,
                     found_at INTEGER NOT NULL,
                     is_relic INTEGER NOT NULL DEFAULT 0,
                     equipped INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE dig_gear (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     slot TEXT NOT NULL,
                     tier INTEGER NOT NULL,
                     durability INTEGER NOT NULL,
                     equipped INTEGER NOT NULL DEFAULT 0,
                     acquired_at INTEGER NOT NULL,
                     source TEXT NOT NULL DEFAULT 'shop',
                     item_id TEXT
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     actor_id INTEGER NOT NULL,
                     target_id INTEGER,
                     action_type TEXT NOT NULL,
                     depth_before INTEGER NOT NULL,
                     depth_after INTEGER NOT NULL,
                     jc_delta INTEGER NOT NULL DEFAULT 0,
                     detail TEXT,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE bets (
                     bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     bet_time INTEGER NOT NULL
                 );
                 CREATE TABLE dig_quests (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     active_quest_id TEXT,
                     active_quest_step INTEGER,
                     completed_quests TEXT NOT NULL DEFAULT '[]',
                     last_updated_at INTEGER,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_guild_modifiers (
                     guild_id INTEGER NOT NULL,
                     modifier_id TEXT NOT NULL,
                     expires_at INTEGER NOT NULL,
                     payload_json TEXT NOT NULL DEFAULT '{}',
                     created_at INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (guild_id, modifier_id)
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TABLE economy_ledger_entries (
                     ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     account_id INTEGER,
                     delta INTEGER NOT NULL,
                     balance_before INTEGER,
                     balance_after INTEGER,
                     source TEXT NOT NULL,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_id, delta, balance_before,
                         balance_after, source, actor_id, related_type,
                         related_id, reason, metadata
                     ) VALUES (
                         NEW.guild_id,
                         NEW.discord_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         OLD.jopacoin_balance,
                         NEW.jopacoin_balance,
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                  'balance_update'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigEventRuntimeRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("fixture connection")
    }

    fn seed(&self, discord_id: i64, guild_id: i64, balance: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, 'event-test', ?3)",
                params![discord_id, guild_id, balance],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, streak_days,
                     temp_buffs, temp_curses
                 ) VALUES (?1, ?2, 50, 11, ?3, ?4)",
                params![
                    discord_id,
                    guild_id,
                    r#"{"id":"old_buff","digs_remaining":2}"#,
                    r#"{"id":"old_curse","digs_remaining":3}"#,
                ],
            )
            .expect("seed tunnel");
    }
}

fn key(discord_id: i64, guild_id: Option<i64>) -> DigEventActorKey {
    DigEventActorKey {
        discord_id,
        guild_id,
    }
}

fn gear_request<'a>(
    expected: &'a DigEventActorSnapshot,
    event_key: &'a str,
) -> AtomicDigEventSettlement<'a> {
    AtomicDigEventSettlement {
        expected,
        event_key,
        event_id: "collapsed_armory",
        choice: "risky",
        depth_after: 54,
        streak_days_after: 8,
        buff_mutation: EventJsonMutation::Replace(r#"{"id":"armory_focus","digs_remaining":3}"#),
        curse_mutation: EventJsonMutation::Clear,
        balance_delta: 13,
        reward: Some(DigEventReward::Gear {
            item_id: "glassbreaker_pick",
            slot: "weapon",
            tier: 3,
            durability: 8,
            source: "event:collapsed_armory",
        }),
        inventory_capacity: 5,
        detail_json: r#"{"succeeded":true,"advance":4,"jc":13}"#,
        created_at: 2_000_000_000,
    }
}

#[test]
fn pending_event_is_guarded_by_action_actor_guild_and_json_identity() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO dig_actions (
                 guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at
             ) VALUES (?1, ?2, NULL, 'dig', 49, 50, 1, ?3, 100)",
            params![
                GUILD,
                ACTOR,
                r#"{"event":"underground_stream","paid":false}"#
            ],
        )
        .expect("seed event Dig action");
    let action_id = connection.last_insert_rowid();

    assert_eq!(
        fixture
            .repository
            .pending_event(action_id, ACTOR, Some(GUILD))
            .expect("pending event"),
        Some(DigPendingEvent {
            action_id,
            actor_id: ACTOR,
            guild_id: GUILD,
            event_id: "underground_stream".to_owned(),
            created_at: 100,
        })
    );
    assert_eq!(
        fixture
            .repository
            .pending_event(action_id, ACTOR + 1, Some(GUILD))
            .expect("wrong actor lookup"),
        None
    );
    assert_eq!(
        fixture
            .repository
            .pending_event(action_id, ACTOR, Some(GUILD + 1))
            .expect("wrong guild lookup"),
        None
    );
}

#[test]
fn atomic_tunnel_update_can_grant_unique_gear() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .expect("snapshot")
        .expect("actor state");

    let outcome = fixture
        .repository
        .settle_actor_atomic(gear_request(&expected, "interaction:100"))
        .expect("settlement");
    let DigEventSettlementOutcome::Applied(receipt) = outcome else {
        panic!("unexpected outcome: {outcome:?}");
    };
    assert!(receipt.applied_now);
    assert_eq!(receipt.balance_before, 20);
    assert_eq!(receipt.balance_after, 33);
    assert_eq!(receipt.depth_before, 50);
    assert_eq!(receipt.depth_after, 54);
    assert_eq!(receipt.reward_row_id, Some(1));

    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        33
    );
    let tunnel = connection
        .query_row(
            "SELECT depth, streak_days, temp_buffs, temp_curses FROM tunnels
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![ACTOR, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(tunnel.0, 54);
    assert_eq!(tunnel.1, 8);
    assert_eq!(
        tunnel.2.as_deref(),
        Some(r#"{"id":"armory_focus","digs_remaining":3}"#)
    );
    assert_eq!(tunnel.3, None);

    let gear = connection
        .query_row(
            "SELECT slot, tier, durability, equipped, source, item_id
               FROM dig_gear WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        gear,
        (
            "weapon".to_owned(),
            3,
            8,
            0,
            "event:collapsed_armory".to_owned(),
            "glassbreaker_pick".to_owned(),
        )
    );

    let ledger = connection
        .query_row(
            "SELECT delta, source, actor_id, related_type, related_id, reason, metadata
               FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(ledger.0, 13);
    assert_eq!(ledger.1, "dig");
    assert_eq!(ledger.2, ACTOR);
    assert_eq!(ledger.3, "event");
    assert_eq!(ledger.4, "collapsed_armory");
    assert_eq!(ledger.5, "dig event credit");
    let metadata: JsonValue = serde_json::from_str(&ledger.6).unwrap();
    assert_eq!(metadata["event_key"], "interaction:100");
    assert_eq!(metadata["reward_row_id"], 1);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn duplicate_delivery_returns_original_receipt_without_second_write() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    let request = gear_request(&expected, "interaction:duplicate");
    let first = fixture
        .repository
        .settle_actor_atomic(request)
        .expect("first settlement");
    let second = fixture
        .repository
        .settle_actor_atomic(request)
        .expect("duplicate settlement");
    let DigEventSettlementOutcome::Applied(first) = first else {
        panic!("first settlement not applied");
    };
    let DigEventSettlementOutcome::Applied(second) = second else {
        panic!("duplicate settlement not admitted");
    };
    assert!(first.applied_now);
    assert!(!second.applied_now);
    assert_eq!(first.action_id, second.action_id);
    assert_eq!(first.reward_row_id, second.reward_row_id);

    let connection = fixture.connection();
    for table in ["dig_actions", "dig_gear", "economy_ledger_entries"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(
            connection
                .query_row(&sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1,
            "{table} was written more than once"
        );
    }
}

#[test]
fn duplicate_delivery_returns_first_receipt_when_environment_changed() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    let request = gear_request(&expected, "interaction:collision");
    fixture
        .repository
        .settle_actor_atomic(request)
        .expect("first settlement");
    let mut changed = request;
    changed.balance_delta = 14;
    let outcome = fixture
        .repository
        .settle_actor_atomic(changed)
        .expect("first committed receipt wins");
    assert!(matches!(
        outcome,
        DigEventSettlementOutcome::Applied(receipt)
            if !receipt.applied_now && receipt.jc_delta == 13
    ));
}

#[test]
fn reused_event_key_for_different_event_or_choice_is_rejected() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    let request = gear_request(&expected, "interaction:collision");
    fixture
        .repository
        .settle_actor_atomic(request)
        .expect("first settlement");
    let mut changed_event = request;
    changed_event.event_id = "different_event";
    assert!(matches!(
        fixture.repository.settle_actor_atomic(changed_event),
        Err(DigEventRuntimeRepositoryError::IdempotencyConflict)
    ));
    let mut changed_choice = request;
    changed_choice.choice = "safe";
    assert!(matches!(
        fixture.repository.settle_actor_atomic(changed_choice),
        Err(DigEventRuntimeRepositoryError::IdempotencyConflict)
    ));
}

#[test]
fn stale_snapshot_rejects_every_actor_side_effect() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance = 21
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![ACTOR, GUILD],
        )
        .unwrap();

    assert_eq!(
        fixture
            .repository
            .settle_actor_atomic(gear_request(&expected, "interaction:stale"))
            .unwrap(),
        DigEventSettlementOutcome::Conflict
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_gear", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn atomic_unique_gear_grant_rolls_back_with_event_failure() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_event_audit
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'event'
             BEGIN
                 SELECT RAISE(ABORT, 'forced event audit failure');
             END;",
        )
        .unwrap();

    assert!(matches!(
        fixture
            .repository
            .settle_actor_atomic(gear_request(&expected, "interaction:rollback")),
        Err(DigEventRuntimeRepositoryError::Sqlite(_))
    ));
    assert_eq!(
        fixture
            .repository
            .actor_snapshot(key(ACTOR, Some(GUILD)))
            .unwrap()
            .unwrap(),
        expected
    );
    let connection = fixture.connection();
    for table in [
        "dig_gear",
        "dig_actions",
        "economy_ledger_entries",
        "economy_ledger_context",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(
            connection
                .query_row(&sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "{table} escaped rollback"
        );
    }
}

#[test]
fn repository_persists_unique_item_identity_and_custom_durability() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .expect("snapshot")
        .expect("actor state");

    let outcome = fixture
        .repository
        .settle_actor_atomic(gear_request(&expected, "interaction:identity"))
        .expect("gear settlement");
    let DigEventSettlementOutcome::Applied(receipt) = outcome else {
        panic!("unexpected outcome");
    };
    let reward_id = receipt.reward_row_id.expect("gear reward row");
    let row = fixture
        .connection()
        .query_row(
            "SELECT item_id, slot, durability, source
               FROM dig_gear WHERE id=?1",
            params![reward_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .expect("persisted unique gear");
    assert_eq!(
        row,
        (
            "glassbreaker_pick".to_owned(),
            "weapon".to_owned(),
            8,
            "event:collapsed_armory".to_owned(),
        )
    );
}

#[test]
fn concurrent_duplicate_event_key_commits_once() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = Arc::new(
        fixture
            .repository
            .actor_snapshot(key(ACTOR, Some(GUILD)))
            .unwrap()
            .unwrap(),
    );
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let expected = Arc::clone(&expected);
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .settle_actor_atomic(gear_request(expected.as_ref(), "interaction:concurrent"))
                    .expect("concurrent settlement")
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("settlement thread"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                DigEventSettlementOutcome::Applied(receipt) if receipt.applied_now
            ))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                DigEventSettlementOutcome::Applied(receipt) if !receipt.applied_now
            ))
            .count(),
        1
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn consumable_and_artifact_rewards_use_the_same_atomic_boundary() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    fixture.seed(ACTOR - 1, GUILD, 30);

    let first = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    let mut consumable = gear_request(&first, "interaction:consumable");
    consumable.reward = Some(DigEventReward::Consumable {
        item_type: "rescue_line",
    });
    fixture
        .repository
        .settle_actor_atomic(consumable)
        .expect("consumable settlement");

    let second = fixture
        .repository
        .actor_snapshot(key(ACTOR - 1, Some(GUILD)))
        .unwrap()
        .unwrap();
    let mut artifact = gear_request(&second, "interaction:artifact");
    artifact.reward = Some(DigEventReward::Artifact {
        artifact_id: "miner_s_lullaby",
        is_relic: false,
    });
    fixture
        .repository
        .settle_actor_atomic(artifact)
        .expect("artifact settlement");

    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT item_type FROM dig_inventory WHERE discord_id = ?1",
                [ACTOR],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "rescue_line"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT artifact_id FROM dig_artifacts WHERE discord_id = ?1",
                [ACTOR - 1],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "miner_s_lullaby"
    );
}

#[test]
fn missing_player_and_tunnel_are_reported_without_writes() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let expected = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();

    fixture
        .connection()
        .execute(
            "DELETE FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![ACTOR, GUILD],
        )
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .settle_actor_atomic(gear_request(&expected, "interaction:no-player"))
            .unwrap(),
        DigEventSettlementOutcome::MissingPlayer
    );

    let missing = DigEventActorSnapshot {
        key: key(ACTOR - 5, Some(GUILD)),
        depth: 0,
        luminosity: 100,
        prestige_level: 0,
        prestige_perks_json: "[]".to_owned(),
        boss_progress_json: "{}".to_owned(),
        streak_days: 0,
        temp_buff_json: None,
        temp_curse_json: None,
        balance: 0,
        inventory_count: 0,
        owned_gear: BTreeSet::new(),
        equipped_gear: BTreeSet::new(),
        owned_artifacts: BTreeSet::new(),
        equipped_relics: BTreeSet::new(),
    };
    assert_eq!(
        fixture
            .repository
            .settle_actor_atomic(gear_request(&missing, "interaction:no-tunnel"))
            .unwrap(),
        DigEventSettlementOutcome::MissingTunnel
    );
}

#[test]
fn grant_splash_is_victim_atomic_ledgered_and_idempotent() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    fixture.seed(ACTOR - 1, GUILD, 30);
    let request = DigSplashGrantRequest {
        guild_id: Some(GUILD),
        digger_id: ACTOR,
        victim_id: ACTOR - 1,
        event_name: "Io Tether Pact",
        strategy: "random_active",
        event_key: "interaction:grant:victim",
        amount: 7,
        gross_jc: 10,
        detail_json: r#"{"penalty_requested":10,"penalty_scaled":7}"#,
        created_at: 2_000_000_001,
    };
    let first = fixture
        .repository
        .settle_splash_grant_atomic(request)
        .expect("grant");
    let duplicate = fixture
        .repository
        .settle_splash_grant_atomic(request)
        .expect("duplicate grant");
    assert!(first.applied_now);
    assert!(!duplicate.applied_now);
    assert_eq!(first.victim_action_id, duplicate.victim_action_id);
    assert_eq!(first.victim_balance_after, Some(37));
    assert_eq!(duplicate.victim_balance_after, Some(37));

    let connection = fixture.connection();
    let ledger = connection
        .query_row(
            "SELECT delta, source, actor_id, related_type, related_id, reason
               FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        ledger,
        (
            7,
            "dig".to_owned(),
            ACTOR,
            "splash_victim".to_owned(),
            "Io Tether Pact".to_owned(),
            "dig splash victim grant".to_owned(),
        )
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='splash_victim'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn hostile_splash_audit_records_burn_or_paired_steal_once() {
    let fixture = Fixture::new();
    let burn = DigHostileSplashAuditRequest {
        guild_id: Some(GUILD),
        digger_id: ACTOR,
        victim_id: ACTOR - 1,
        event_name: "Gas Tax",
        strategy: "richest_n",
        mode: "burn",
        event_key: "interaction:burn:victim",
        amount: 11,
        detail_json: r#"{"penalty_requested":8,"penalty_scaled":11}"#,
        created_at: 2_000_000_002,
    };
    let burn_first = fixture
        .repository
        .record_hostile_splash_audit(burn)
        .expect("burn audit");
    let burn_duplicate = fixture
        .repository
        .record_hostile_splash_audit(burn)
        .expect("duplicate burn audit");
    assert!(burn_first.applied_now);
    assert!(!burn_duplicate.applied_now);
    assert_eq!(burn_first.digger_action_id, None);

    let steal = DigHostileSplashAuditRequest {
        mode: "steal",
        event_key: "interaction:steal:victim",
        ..burn
    };
    let steal_first = fixture
        .repository
        .record_hostile_splash_audit(steal)
        .expect("steal audit");
    let steal_duplicate = fixture
        .repository
        .record_hostile_splash_audit(steal)
        .expect("duplicate steal audit");
    assert!(steal_first.applied_now);
    assert!(!steal_duplicate.applied_now);
    assert!(steal_first.digger_action_id.is_some());
    assert_eq!(
        steal_first.digger_action_id,
        steal_duplicate.digger_action_id
    );

    let connection = fixture.connection();
    let rows = connection
        .prepare(
            "SELECT actor_id, action_type, jc_delta
               FROM dig_actions ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (ACTOR - 1, "splash_victim".to_owned(), -11),
            (ACTOR - 1, "splash_victim".to_owned(), -11),
            (ACTOR, "splash_thief".to_owned(), 11),
        ]
    );
}

#[test]
fn failed_grant_audit_rolls_back_credit_and_ledger() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    fixture.seed(ACTOR - 1, GUILD, 30);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_splash_audit
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'splash_victim'
             BEGIN
                 SELECT RAISE(ABORT, 'forced splash audit failure');
             END;",
        )
        .unwrap();
    let result = fixture
        .repository
        .settle_splash_grant_atomic(DigSplashGrantRequest {
            guild_id: Some(GUILD),
            digger_id: ACTOR,
            victim_id: ACTOR - 1,
            event_name: "Io Tether Pact",
            strategy: "random_active",
            event_key: "interaction:grant:rollback",
            amount: 7,
            gross_jc: 10,
            detail_json: "{}",
            created_at: 2_000_000_003,
        });
    assert!(matches!(
        result,
        Err(DigEventRuntimeRepositoryError::Sqlite(_))
    ));
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR - 1, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        30
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn actor_snapshot_includes_every_event_policy_input() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE tunnels SET luminosity=0, prestige_level=9,
                    prestige_perks=?1, boss_progress=?2
              WHERE discord_id=?3 AND guild_id=?4",
            params![
                r#"["veteran_miner","veteran_miner","tunnel_mastery"]"#,
                r#"{"25":{"status":"defeated"},"50":"active"}"#,
                ACTOR,
                GUILD
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_gear (
                 discord_id,guild_id,slot,tier,durability,equipped,
                 acquired_at,source,item_id
             ) VALUES (?1,?2,'ring',3,14,1,1,'event','surveyors_loop'),
                      (?1,?2,'ring',3,0,1,1,'event','ruinwager_signet')",
            params![ACTOR, GUILD],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts (
                 discord_id,guild_id,artifact_id,found_at,is_relic,equipped
             ) VALUES (?1,?2,'diviners_knot',1,1,1),
                      (?1,?2,'black_wax_seal',1,1,0)",
            params![ACTOR, GUILD],
        )
        .unwrap();
    drop(connection);

    let snapshot = fixture
        .repository
        .actor_snapshot(key(ACTOR, Some(GUILD)))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.luminosity, 0);
    assert_eq!(snapshot.prestige_level, 9);
    assert_eq!(
        snapshot.prestige_perks_json,
        r#"["veteran_miner","veteran_miner","tunnel_mastery"]"#
    );
    assert!(snapshot.boss_progress_json.contains("defeated"));
    assert!(snapshot.owned_gear.contains("ruinwager_signet"));
    assert!(snapshot.equipped_gear.contains("surveyors_loop"));
    assert!(!snapshot.equipped_gear.contains("ruinwager_signet"));
    assert!(snapshot.owned_artifacts.contains("black_wax_seal"));
    assert!(snapshot.equipped_relics.contains("diviners_knot"));
    assert!(!snapshot.equipped_relics.contains("black_wax_seal"));
}

static PLAIN_INVENTORY_QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static PLAIN_OWNED_GEAR_QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static PLAIN_OWNED_ARTIFACT_QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);

fn trace_plain_reward_queries(event: TraceEvent<'_>) {
    let TraceEvent::Stmt(_, sql) = event else {
        return;
    };
    if sql.contains("SELECT COUNT(*) FROM dig_inventory") {
        PLAIN_INVENTORY_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    if sql.contains("SELECT item_id FROM dig_gear") && !sql.contains("equipped = 1") {
        PLAIN_OWNED_GEAR_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    if sql.contains("SELECT artifact_id FROM dig_artifacts") && !sql.contains("is_relic = 1") {
        PLAIN_OWNED_ARTIFACT_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn plain_event_skips_unneeded_reward_inventory_queries() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    PLAIN_INVENTORY_QUERY_COUNT.store(0, Ordering::Relaxed);
    PLAIN_OWNED_GEAR_QUERY_COUNT.store(0, Ordering::Relaxed);
    PLAIN_OWNED_ARTIFACT_QUERY_COUNT.store(0, Ordering::Relaxed);

    let connection = fixture.connection();
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT,
        Some(trace_plain_reward_queries),
    );
    let snapshot = query_actor_snapshot(
        &connection,
        key(ACTOR, Some(GUILD)),
        DigEventRewardQueryPlan::default(),
    )
    .expect("policy snapshot")
    .expect("seeded actor");
    assert!(snapshot.owned_gear.is_empty());
    assert!(snapshot.owned_artifacts.is_empty());
    assert_eq!(snapshot.inventory_count, 0);
    connection.trace_v2(TraceEventCodes::empty(), None);

    assert_eq!(
        PLAIN_INVENTORY_QUERY_COUNT.load(Ordering::Relaxed),
        0,
        "plain event policy snapshot must not count inventory"
    );
    assert_eq!(
        PLAIN_OWNED_GEAR_QUERY_COUNT.load(Ordering::Relaxed),
        0,
        "plain event policy snapshot must not read owned gear"
    );
    assert_eq!(
        PLAIN_OWNED_ARTIFACT_QUERY_COUNT.load(Ordering::Relaxed),
        0,
        "plain event policy snapshot must not read owned artifacts"
    );
}

static ARTIFACT_QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);

fn trace_artifact_reward_queries(event: TraceEvent<'_>) {
    let TraceEvent::Stmt(_, sql) = event else {
        return;
    };
    if sql.contains("SELECT artifact_id FROM dig_artifacts") && !sql.contains("is_relic = 1") {
        ARTIFACT_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn artifact_reward_pool_uses_one_owned_artifact_query() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    fixture
        .connection()
        .execute(
            "INSERT INTO dig_artifacts (
                 discord_id, guild_id, artifact_id, found_at, is_relic, equipped
             ) VALUES (?1, ?2, 'miner_s_lullaby', 1, 0, 0),
                      (?1, ?2, 'map_of_the_first_descent', 2, 0, 0)",
            params![ACTOR, GUILD],
        )
        .expect("seed owned artifact pool");
    ARTIFACT_QUERY_COUNT.store(0, Ordering::Relaxed);

    let connection = fixture.connection();
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT,
        Some(trace_artifact_reward_queries),
    );
    let reward = query_reward_snapshot(
        &connection,
        key(ACTOR, Some(GUILD)),
        DigEventRewardQueryPlan {
            owned_artifacts: true,
            ..DigEventRewardQueryPlan::default()
        },
    )
    .expect("artifact reward snapshot");
    connection.trace_v2(TraceEventCodes::empty(), None);

    assert_eq!(reward.owned_artifacts.len(), 2);
    assert_eq!(
        ARTIFACT_QUERY_COUNT.load(Ordering::Relaxed),
        1,
        "artifact reward admission uses one owned-set query"
    );
}

#[test]
fn quest_progress_is_fail_soft_ordered_and_idempotent() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let now = 2_000_000_000;
    fixture
        .connection()
        .execute(
            "INSERT INTO bets (guild_id,discord_id,bet_time) VALUES (?1,?2,?3)",
            params![GUILD, ACTOR, now - 60],
        )
        .unwrap();
    let actor = key(ACTOR, Some(GUILD));
    assert!(
        fixture
            .repository
            .quest_snapshot(actor, now)
            .unwrap()
            .recent_bet
    );

    let start = DigEventQuestMutation::SetActive {
        quest_id: "bolas_pact",
        next_step: 2,
    };
    assert!(
        fixture
            .repository
            .apply_quest_mutation(actor, start, now)
            .unwrap()
            .applied_now
    );
    assert!(
        !fixture
            .repository
            .apply_quest_mutation(actor, start, now)
            .unwrap()
            .applied_now
    );
    let advanced = fixture
        .repository
        .apply_quest_mutation(
            actor,
            DigEventQuestMutation::SetActive {
                quest_id: "bolas_pact",
                next_step: 3,
            },
            now + 1,
        )
        .unwrap();
    assert_eq!(advanced.state.active_quest_step, Some(3));
    assert!(matches!(
        fixture.repository.apply_quest_mutation(
            actor,
            DigEventQuestMutation::SetActive {
                quest_id: "different_arc",
                next_step: 2,
            },
            now + 2,
        ),
        Err(DigEventRuntimeRepositoryError::ActiveQuestConflict)
    ));

    let complete = DigEventQuestMutation::Complete {
        quest_id: "bolas_pact",
    };
    let completed = fixture
        .repository
        .apply_quest_mutation(actor, complete, now + 3)
        .unwrap();
    assert!(completed.applied_now);
    assert_eq!(completed.state.active_quest_id, None);
    assert!(completed.state.completed_quest_ids.contains("bolas_pact"));
    assert!(
        !fixture
            .repository
            .apply_quest_mutation(actor, complete, now + 4)
            .unwrap()
            .applied_now
    );
}

#[test]
fn guild_modifier_retry_does_not_extend_twice() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let request = DigEventGuildModifierRequest {
        guild_id: Some(GUILD),
        actor_id: ACTOR,
        modifier_id: "helltide_active",
        duration_seconds: 600,
        payload_json: r#"{"tax_per_dig":5}"#,
        event_key: "interaction:modifier:1",
        now: 2_000_000_000,
    };
    let first = fixture.repository.set_guild_modifier(request).unwrap();
    let duplicate = fixture.repository.set_guild_modifier(request).unwrap();
    assert!(first.applied_now);
    assert!(!duplicate.applied_now);
    assert_eq!(first.expires_at, 2_000_000_600);
    assert_eq!(duplicate.expires_at, first.expires_at);

    let second = fixture
        .repository
        .set_guild_modifier(DigEventGuildModifierRequest {
            event_key: "interaction:modifier:2",
            now: 2_000_000_010,
            ..request
        })
        .unwrap();
    assert_eq!(second.expires_at, 2_000_001_200);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='event_modifier'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert!(matches!(
        fixture
            .repository
            .set_guild_modifier(DigEventGuildModifierRequest {
                modifier_id: "different_modifier",
                ..request
            }),
        Err(DigEventRuntimeRepositoryError::IdempotencyConflict)
    ));
}

#[test]
fn quest_finale_jc_and_relic_followups_are_atomic_and_replayable() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 20);
    let actor = key(ACTOR, Some(GUILD));
    let jc = DigEventFinaleJcRequest {
        key: actor,
        quest_id: "aghanim_arc",
        event_key: "interaction:finale:jc",
        gross_jc: 100,
        net_jc: 62,
        detail_json: r#"{"reward_multiplier":0.65,"blue_tax":3}"#,
        now: 2_000_000_000,
    };
    let first = fixture.repository.settle_finale_jc_atomic(jc).unwrap();
    let duplicate = fixture.repository.settle_finale_jc_atomic(jc).unwrap();
    assert!(first.applied_now);
    assert!(!duplicate.applied_now);
    assert_eq!(first.balance_after, Some(82));
    assert_eq!(duplicate.balance_after, first.balance_after);
    assert_eq!(duplicate.action_id, first.action_id);

    let relic = DigEventFinaleRelicRequest {
        key: actor,
        quest_id: "bolas_pact",
        event_key: "interaction:finale:relic",
        artifact_id: "pinnacle:Bolas:the Pact:crit:hp",
        detail_json: r#"{"relic_name":"Bolas of the Pact"}"#,
        now: 2_000_000_001,
    };
    let relic_first = fixture.repository.grant_finale_relic_atomic(relic).unwrap();
    let relic_duplicate = fixture.repository.grant_finale_relic_atomic(relic).unwrap();
    assert!(relic_first.applied_now);
    assert!(!relic_duplicate.applied_now);
    assert_eq!(relic_first.reward_row_id, Some(1));
    assert_eq!(relic_duplicate.reward_row_id, Some(1));

    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE action_type IN ('quest_finale_jc','quest_finale_relic')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    let ledger = connection
        .query_row(
            "SELECT delta,source,related_type,related_id,reason
               FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        ledger,
        (
            62,
            "dig".to_owned(),
            "quest_finale".to_owned(),
            "aghanim_arc".to_owned(),
            "dig quest finale credit".to_owned(),
        )
    );
}
