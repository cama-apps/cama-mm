use std::collections::BTreeSet;

use crate::test_support::copy_migrated_database;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const NOW: i64 = 1_700_000_000;

struct Fixture {
    database: NamedTempFile,
    repository: DigPrestigeRuntimeRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        copy_migrated_database(database.path()).expect("canonical migrated schema");
        let repository = DigPrestigeRuntimeRepository::new(database.path());
        let fixture = Self {
            database,
            repository,
        };
        fixture.seed();
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
    }

    fn seed(&self) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players
                    (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'prestiger', 500)",
                params![ACTOR, GUILD],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, total_digs,
                     total_jc_earned, last_dig_at, luminosity, pickaxe_tier,
                     prestige_level, prestige_perks, boss_progress,
                     boss_attempts, best_run_score, current_run_jc,
                     current_run_artifacts, current_run_events,
                     total_prestige_score, streak_days, stat_boss_awards,
                     pinnacle_boss_id, pinnacle_phase, pinnacle_hp_remaining,
                     pinnacle_last_engaged_at, route_state
                 ) VALUES (
                     ?1, ?2, 300, 300, 88, 900, 123, 17, 4,
                     0, '[]', '{\"25\":\"defeated\"}',
                     3, 10, 50, 3, 7, 20, 4, '{\"awards\":[25]}',
                     'forgotten_king', 2, 9, 456, '{\"route\":\"left\"}'
                 )",
                params![ACTOR, GUILD],
            )
            .expect("seed tunnel");
        connection
            .execute(
                "INSERT INTO dig_artifacts
                    (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
                 VALUES (?1, ?2, 'owned_relic', 10, 1, 0)",
                params![ACTOR, GUILD],
            )
            .expect("seed artifact");
    }

    fn snapshot(&self) -> DigPrestigeSnapshot {
        self.repository
            .snapshot(DigPrestigeKey {
                discord_id: ACTOR,
                guild_id: GUILD,
            })
            .expect("snapshot read")
            .expect("seeded snapshot")
    }
}

fn settlement<'a>(snapshot: &'a DigPrestigeSnapshot) -> AtomicDigPrestigeSettlement<'a> {
    AtomicDigPrestigeSettlement {
        expected: snapshot,
        next_prestige_level: 1,
        prestige_perks_json: r#"["advance_boost"]"#,
        boss_progress_json: r#"{"25":"active","50":"active"}"#,
        mutations_json: None,
        stat_boss_awards_json: r#"{"prestige_level":1,"awards":[]}"#,
        best_run_score: 800,
        total_prestige_score: 810,
        balance_delta: 650,
        relic_artifact_id: Some("new_relic"),
        created_at: NOW,
    }
}

#[test]
fn migrated_snapshot_reads_run_wallet_and_owned_artifacts() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.depth, 300);
    assert_eq!(snapshot.current_run_jc, 50);
    assert_eq!(snapshot.current_run_artifacts, 3);
    assert_eq!(snapshot.current_run_events, 7);
    assert_eq!(snapshot.balance, 500);
    assert_eq!(
        snapshot.owned_artifact_ids,
        BTreeSet::from(["owned_relic".to_owned()])
    );
}

#[test]
fn atomic_settlement_resets_exact_fields_and_commits_grant_and_relic() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let outcome = fixture
        .repository
        .atomic_settle(settlement(&snapshot))
        .expect("prestige settlement");
    let DigPrestigeSettlementOutcome::Applied(receipt) = outcome else {
        panic!("expected applied prestige, got {outcome:?}");
    };
    assert_eq!(receipt.balance_before, 500);
    assert_eq!(receipt.balance_after, 1_150);
    assert!(receipt.relic_row_id.is_some());

    let connection = fixture.connection();
    let state: (i64, i64, i64, i64, i64, String, String, Option<String>) = connection
        .query_row(
            "SELECT CAST(depth AS INTEGER), CAST(prestige_level AS INTEGER),
                    CAST(boss_attempts AS INTEGER), CAST(current_run_jc AS INTEGER),
                    current_run_events, prestige_perks, stat_boss_awards,
                    pinnacle_boss_id
               FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .expect("settled tunnel");
    assert_eq!(state.0, 0);
    assert_eq!(state.1, 1);
    assert_eq!(state.2, 0);
    assert_eq!(state.3, 0);
    assert_eq!(state.4, 0);
    assert_eq!(state.5, r#"["advance_boost"]"#);
    assert_eq!(state.6, r#"{"prestige_level":1,"awards":[]}"#);
    assert_eq!(state.7, None);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='new_relic'",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("relic count"),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("cleared ledger context"),
        0
    );
    let ledger: (i64, String, String, String) = connection
        .query_row(
            "SELECT delta, source, related_type, reason
               FROM economy_ledger_entries ORDER BY ledger_id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("prestige ledger entry");
    assert_eq!(
        ledger,
        (
            650,
            "dig".to_owned(),
            "dig_action".to_owned(),
            "dig payout".to_owned()
        )
    );
}

#[test]
fn stale_prestige_claim_rolls_back_second_grant_and_relic() {
    let fixture = Fixture::new();
    let stale = fixture.snapshot();
    assert!(matches!(
        fixture
            .repository
            .atomic_settle(settlement(&stale))
            .unwrap(),
        DigPrestigeSettlementOutcome::Applied(_)
    ));
    assert_eq!(
        fixture
            .repository
            .atomic_settle(settlement(&stale))
            .unwrap(),
        DigPrestigeSettlementOutcome::Conflict
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1_150
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='new_relic'",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn tunnel_update_failure_rolls_back_wallet_ledger_and_relic() {
    let fixture = Fixture::new();
    let ledger_entries_before = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("baseline ledger count");
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_prestige_reset
             BEFORE UPDATE ON tunnels
             BEGIN SELECT RAISE(ABORT, 'injected prestige reset failure'); END;",
        )
        .expect("failure trigger");
    let snapshot = fixture.snapshot();
    assert!(
        fixture
            .repository
            .atomic_settle(settlement(&snapshot))
            .is_err()
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        500
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts WHERE artifact_id='new_relic'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        ledger_entries_before
    );
}

#[test]
fn audit_append_remains_a_separate_post_commit_operation() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    fixture
        .repository
        .atomic_settle(settlement(&snapshot))
        .unwrap();
    let action_id = fixture
        .repository
        .append_audit(
            snapshot.key,
            650,
            r#"{"level":1,"perk":"advance_boost"}"#,
            NOW,
        )
        .expect("post-commit audit");
    assert!(action_id > 0);
    let action: (String, i64, String) = fixture
        .connection()
        .query_row(
            "SELECT action_type, jc_delta, detail FROM dig_actions WHERE id=?1",
            [action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("prestige audit");
    assert_eq!(
        action,
        (
            "prestige".to_owned(),
            650,
            r#"{"level":1,"perk":"advance_boost"}"#.to_owned()
        )
    );
}
