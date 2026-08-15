use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const USER: i64 = -93_001;
const GUILD: i64 = 93_002;
const NOW: i64 = 1_900_000_000;

fn fixture(balance: i64) -> NamedTempFile {
    let database = NamedTempFile::new().expect("miner runtime database");
    initialize_or_migrate(database.path()).expect("migrated miner schema");
    let connection = Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python legacy foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES (?1,?2,'miner-runtime',?3)",
            params![USER, GUILD, balance],
        )
        .unwrap();
    database
}

fn repository(database: &NamedTempFile) -> DigMinerRuntimeRepository {
    DigMinerRuntimeRepository::new(database.path())
}

fn ensure(database: &NamedTempFile) -> DigMinerSnapshot {
    repository(database)
        .ensure_tunnel(USER, GUILD, "The Whispering Descent", NOW)
        .unwrap()
        .snapshot
}

#[test]
fn ensure_profile_tunnel_requires_player_and_is_idempotent() {
    let database = fixture(100);
    let repository = repository(&database);
    let missing = repository
        .ensure_tunnel(USER + 1, GUILD, "Missing Hollow", NOW)
        .unwrap();
    assert_eq!(missing.status, DigMinerMutationStatus::MissingPlayer);
    assert!(missing.snapshot.tunnel.is_none());

    let first = repository
        .ensure_tunnel(USER, GUILD, "The Whispering Descent", NOW)
        .unwrap();
    let second = repository
        .ensure_tunnel(USER, GUILD, "Ignored Race Name", NOW + 1)
        .unwrap();
    assert_eq!(first.status, DigMinerMutationStatus::Applied);
    assert_eq!(first.snapshot, second.snapshot);
    let tunnel = first.snapshot.tunnel.unwrap();
    assert_eq!(tunnel.tunnel_name, "The Whispering Descent");
    assert_eq!(tunnel.stat_points, 5);
    assert!(!tunnel.auto_buy_torch && !tunnel.auto_buy_hard_hat);
}

#[test]
fn backstory_lock_is_cas_guarded_and_only_one_concurrent_writer_wins() {
    let database = fixture(100);
    ensure(&database);
    let path = database.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["First story", "Second story"].map(|story| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            DigMinerRuntimeRepository::new(path).lock_backstory(USER, GUILD, "", story)
        })
    });
    barrier.wait();
    let statuses = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap().status)
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::Applied)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::Locked)
            .count(),
        1
    );
    assert!(
        ["First story", "Second story"].contains(
            &repository(&database)
                .snapshot(USER, GUILD)
                .unwrap()
                .tunnel
                .unwrap()
                .miner_about
                .as_str()
        )
    );
}

#[test]
fn allocation_uses_total_budget_without_decrementing_stat_points() {
    let database = fixture(100);
    let expected = ensure(&database).tunnel.unwrap();
    let first = repository(&database)
        .allocate_stats(
            USER,
            GUILD,
            &expected,
            DigMinerAllocation {
                strength: 2,
                smarts: 2,
                stamina: 1,
            },
        )
        .unwrap();
    assert_eq!(first.status, DigMinerMutationStatus::Applied);
    let tunnel = first.snapshot.tunnel.unwrap();
    assert_eq!(
        (
            tunnel.stat_strength,
            tunnel.stat_smarts,
            tunnel.stat_stamina,
            tunnel.stat_points,
        ),
        (2, 2, 1, 5)
    );
    assert_eq!(
        repository(&database)
            .allocate_stats(
                USER,
                GUILD,
                &tunnel,
                DigMinerAllocation {
                    strength: 1,
                    smarts: 0,
                    stamina: 0,
                },
            )
            .unwrap()
            .status,
        DigMinerMutationStatus::InsufficientPoints
    );
}

#[test]
fn stale_or_concurrent_allocation_cannot_overspend() {
    let database = fixture(100);
    let expected = Arc::new(ensure(&database).tunnel.unwrap());
    let path = database.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let expected = Arc::clone(&expected);
            thread::spawn(move || {
                barrier.wait();
                DigMinerRuntimeRepository::new(path).allocate_stats(
                    USER,
                    GUILD,
                    &expected,
                    DigMinerAllocation {
                        strength: 5,
                        smarts: 0,
                        stamina: 0,
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let statuses = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap().status)
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::Applied)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::Conflict)
            .count(),
        1
    );
    let tunnel = repository(&database)
        .snapshot(USER, GUILD)
        .unwrap()
        .tunnel
        .unwrap();
    assert_eq!((tunnel.stat_strength, tunnel.stat_points), (5, 5));
}

#[test]
fn auto_buy_partial_updates_preserve_the_other_setting() {
    let database = fixture(100);
    ensure(&database);
    let first = repository(&database)
        .set_auto_buy(
            USER,
            GUILD,
            DigMinerAutoBuyUpdate {
                torch: Some(true),
                hard_hat: Some(true),
            },
        )
        .unwrap();
    assert_eq!(first.status, DigMinerMutationStatus::Applied);
    let second = repository(&database)
        .set_auto_buy(
            USER,
            GUILD,
            DigMinerAutoBuyUpdate {
                torch: Some(false),
                hard_hat: None,
            },
        )
        .unwrap();
    let tunnel = second.snapshot.tunnel.unwrap();
    assert!(!tunnel.auto_buy_torch && tunnel.auto_buy_hard_hat);
}

#[test]
fn respec_debits_wallet_resets_stats_and_writes_ledger_and_audit_atomically() {
    let database = fixture(100);
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,stat_strength,stat_smarts,
                 stat_stamina,stat_points)
             VALUES (?1,?2,'Deep Ledger',2,1,2,12)",
            params![USER, GUILD],
        )
        .unwrap();
    let outcome = repository(&database).respec(USER, GUILD, 50, NOW).unwrap();
    assert_eq!(outcome.status, DigMinerMutationStatus::Applied);
    assert_eq!(outcome.returned_points, 5);
    let tunnel = outcome.snapshot.tunnel.unwrap();
    assert_eq!(
        (
            tunnel.stat_strength,
            tunnel.stat_smarts,
            tunnel.stat_stamina,
            tunnel.stat_points,
        ),
        (0, 0, 0, 12)
    );
    assert_eq!(outcome.snapshot.balance, Some(50));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 AND delta=-50
                   AND source='dig' AND related_type='miner_respec'",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    let audit: (i64, i64, String) = connection
        .query_row(
            "SELECT jc_delta,created_at,detail FROM dig_actions
             WHERE id=?1 AND action_type='miner_respec'",
            [outcome.action_id.unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((audit.0, audit.1), (-50, NOW));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&audit.2).unwrap()["returned_points"],
        5
    );
}

#[test]
fn empty_and_insufficient_respecs_never_charge() {
    let database = fixture(49);
    ensure(&database);
    assert_eq!(
        repository(&database)
            .respec(USER, GUILD, 50, NOW)
            .unwrap()
            .status,
        DigMinerMutationStatus::NoAllocatedPoints
    );
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET stat_strength=3,stat_smarts=2
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    assert_eq!(
        repository(&database)
            .respec(USER, GUILD, 50, NOW)
            .unwrap()
            .status,
        DigMinerMutationStatus::InsufficientBalance
    );
    assert_eq!(
        repository(&database).snapshot(USER, GUILD).unwrap().balance,
        Some(49)
    );
}

#[test]
fn injected_respec_failure_rolls_back_wallet_stats_ledger_and_audit() {
    let database = fixture(100);
    let tunnel = ensure(&database).tunnel.unwrap();
    repository(&database)
        .allocate_stats(
            USER,
            GUILD,
            &tunnel,
            DigMinerAllocation {
                strength: 5,
                smarts: 0,
                stamina: 0,
            },
        )
        .unwrap();
    assert!(matches!(
        repository(&database).respec_inner(USER, GUILD, 50, NOW, true),
        Err(DigMinerRuntimeRepositoryError::InjectedFailure)
    ));
    let snapshot = repository(&database).snapshot(USER, GUILD).unwrap();
    assert_eq!(snapshot.balance, Some(100));
    assert_eq!(snapshot.tunnel.unwrap().stat_strength, 5);
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='miner_respec'",
                [],
                |row| row.get::<_, i64>(0),
            )
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
fn concurrent_respec_charges_and_resets_exactly_once() {
    let database = fixture(100);
    let tunnel = ensure(&database).tunnel.unwrap();
    repository(&database)
        .allocate_stats(
            USER,
            GUILD,
            &tunnel,
            DigMinerAllocation {
                strength: 5,
                smarts: 0,
                stamina: 0,
            },
        )
        .unwrap();
    let path = database.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                DigMinerRuntimeRepository::new(path).respec(USER, GUILD, 50, NOW)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let statuses = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap().status)
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::Applied)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == DigMinerMutationStatus::NoAllocatedPoints)
            .count(),
        1
    );
    let snapshot = repository(&database).snapshot(USER, GUILD).unwrap();
    assert_eq!(snapshot.balance, Some(50));
    assert_eq!(snapshot.tunnel.unwrap().stat_strength, 0);
}
