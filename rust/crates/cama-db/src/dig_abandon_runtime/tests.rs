use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::test_support::copy_migrated_database;

const USER: i64 = 83_001;
const GUILD: i64 = 83_002;

type PreservedAbandonState = (
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    i64,
);

fn fixture(depth: i64) -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary database");
    copy_migrated_database(database.path()).expect("migrated schema");
    let connection = Connection::open(database.path()).expect("fixture connection");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES (?1,?2,'abandoner',100)",
            params![USER, GUILD],
        )
        .expect("player");
    connection
        .execute(
            "INSERT INTO tunnels (
                discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                last_dig_at,streak_days,pickaxe_tier,prestige_level,
                boss_progress,boss_attempts,injury_state,cheer_data,
                pinnacle_boss_id,pinnacle_phase,pinnacle_hp_remaining,
                pinnacle_last_engaged_at,route_state
             ) VALUES (
                ?1,?2,?3,444,99,777,123456,8,4,3,
                '{\"25\":\"defeated\"}',7,'{\"id\":\"sprain\"}',
                '{\"bonus\":2}','forgotten_king',2,18,123400,
                '{\"selected\":\"old_supports\"}'
             )",
            params![USER, GUILD, depth],
        )
        .expect("tunnel");
    database
}

fn request<'a>(
    snapshot: DigAbandonSnapshot,
    boss_progress_json: &'a str,
    detail_json: &'a str,
    now: i64,
) -> AtomicDigAbandonSettlement<'a> {
    AtomicDigAbandonSettlement {
        expected: snapshot,
        refund: 13,
        boss_progress_json,
        detail_json,
        created_at: now,
    }
}

#[test]
fn migrated_snapshot_keeps_depth_prestige_and_balance_typed() {
    let database = fixture(100);
    let snapshot = DigAbandonRuntimeRepository::new(database.path())
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .expect("snapshot");
    assert_eq!(snapshot.depth, 100);
    assert_eq!(snapshot.prestige_level, 3);
    assert_eq!(snapshot.balance, 100);
}

#[test]
fn atomic_settlement_matches_partial_reset_wallet_ledger_and_audit() {
    let database = fixture(100);
    let repository = DigAbandonRuntimeRepository::new(database.path());
    let snapshot = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    let detail = r#"{"depth":100,"gross_jc":10,"refund":13,"reward_multiplier":0.65}"#;
    let outcome = repository
        .atomic_settle(request(
            snapshot,
            r#"{"25":"active","50":"active"}"#,
            detail,
            200_000,
        ))
        .unwrap();
    let DigAbandonSettlementOutcome::Applied(receipt) = outcome else {
        panic!("expected applied settlement: {outcome:?}");
    };
    assert_eq!(receipt.depth_lost, 100);
    assert_eq!(receipt.prestige_level, 3);
    assert_eq!((receipt.balance_before, receipt.balance_after), (100, 113));

    let connection = Connection::open(database.path()).unwrap();
    let state: PreservedAbandonState = connection
        .query_row(
            "SELECT depth,max_depth,total_digs,total_jc_earned,pickaxe_tier,
                        prestige_level,streak_days,injury_state,route_state,
                        (SELECT jopacoin_balance FROM players
                          WHERE discord_id=t.discord_id AND guild_id=t.guild_id)
                   FROM tunnels t WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
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
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(state, (0, 444, 99, 777, 4, 3, 0, None, None, 113));
    let action: (String, i64, String) = connection
        .query_row(
            "SELECT action_type,jc_delta,detail FROM dig_actions WHERE id=?1",
            [receipt.audit_action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(action, ("abandon".to_owned(), 13, detail.to_owned()));
    let ledger: (i64, String, String, String, String) = connection
        .query_row(
            "SELECT delta,source,related_type,reason,metadata
               FROM economy_ledger_entries
              WHERE account_type='player' AND account_id=?1
              ORDER BY ledger_id DESC LIMIT 1",
            [USER],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        ledger,
        (
            13,
            "dig".to_owned(),
            "abandon".to_owned(),
            "dig abandon credit".to_owned(),
            detail.to_owned(),
        )
    );
}

#[test]
fn cooldown_includes_the_exact_twenty_four_hour_boundary() {
    let database = fixture(100);
    let repository = DigAbandonRuntimeRepository::new(database.path());
    let first = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    assert!(matches!(
        repository
            .atomic_settle(request(first, "{}", "{}", 100_000))
            .unwrap(),
        DigAbandonSettlementOutcome::Applied(_)
    ));
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE tunnels SET depth=100 WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    let exact_boundary = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        repository
            .atomic_settle(request(
                exact_boundary,
                "{}",
                "{}",
                100_000 + ABANDON_COOLDOWN_SECONDS,
            ))
            .unwrap(),
        DigAbandonSettlementOutcome::Cooldown
    );
    assert!(matches!(
        repository
            .atomic_settle(request(
                exact_boundary,
                "{}",
                "{}",
                100_001 + ABANDON_COOLDOWN_SECONDS,
            ))
            .unwrap(),
        DigAbandonSettlementOutcome::Applied(_)
    ));
}

#[test]
fn concurrent_delivery_credits_and_audits_exactly_once() {
    let database = fixture(100);
    let path = Arc::new(database.path().to_path_buf());
    let repository = DigAbandonRuntimeRepository::new(path.as_ref());
    let snapshot = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                barrier.wait();
                DigAbandonRuntimeRepository::new(path.as_ref())
                    .atomic_settle(request(snapshot, "{}", "{}", 300_000))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigAbandonSettlementOutcome::Applied(_)))
            .count(),
        1
    );
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        113
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2 AND action_type='abandon'",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn stale_depth_conflict_leaves_wallet_and_audit_untouched() {
    let database = fixture(100);
    let repository = DigAbandonRuntimeRepository::new(database.path());
    let snapshot = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET depth=101 WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    assert_eq!(
        repository
            .atomic_settle(request(snapshot, "{}", "{}", 400_000))
            .unwrap(),
        DigAbandonSettlementOutcome::Conflict
    );
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn injected_reset_failure_rolls_back_wallet_ledger_and_audit() {
    let database = fixture(100);
    let repository = DigAbandonRuntimeRepository::new(database.path());
    let snapshot = repository
        .snapshot(DigAbandonKey {
            discord_id: USER,
            guild_id: GUILD,
        })
        .unwrap()
        .unwrap();
    Connection::open(database.path())
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_abandon_reset
             BEFORE UPDATE OF depth ON tunnels
             WHEN NEW.depth=0
             BEGIN SELECT RAISE(ABORT, 'injected abandon reset failure'); END;",
        )
        .unwrap();
    assert!(
        repository
            .atomic_settle(request(snapshot, "{}", "{}", 500_000))
            .is_err()
    );
    let connection = Connection::open(database.path()).unwrap();
    let (depth, balance, actions, ledger_delta): (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT t.depth,p.jopacoin_balance,
                    (SELECT COUNT(*) FROM dig_actions),
                    (SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                      WHERE account_type='player' AND account_id=t.discord_id)
               FROM tunnels t JOIN players p
                 ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
              WHERE t.discord_id=?1 AND t.guild_id=?2",
            params![USER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((depth, balance, actions), (100, 100, 0));
    assert_eq!(
        ledger_delta, 100,
        "only the fixture player-insert entry remains"
    );
}
