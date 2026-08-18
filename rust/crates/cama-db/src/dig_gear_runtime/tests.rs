use std::sync::{Arc, Barrier};
use std::thread;

use cama_domain::dig_gear::{Artifact, GearPiece, GearSlot};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::core_repositories::{NewPlayer, PlayerRepository};
use crate::test_support::copy_migrated_database;

const USER: i64 = 81_001;
const GUILD: i64 = 81_002;

fn fixture() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary DB");
    copy_migrated_database(database.path()).expect("canonical schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "gear miner", Some(GUILD)))
        .expect("player");
    let connection = Connection::open(database.path()).expect("open fixture");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=500 WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .expect("balance");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,pickaxe_tier,prestige_level)
             VALUES (?1,?2,100,100,1,1)",
            params![USER, GUILD],
        )
        .expect("tunnel");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',1,20,1,10,'fixture'),
                    (?1,?2,'weapon',2,10,0,11,'fixture'),
                    (?1,?2,'armor',2,5,0,12,'fixture')",
            params![USER, GUILD],
        )
        .expect("gear");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',10,1,0)",
            params![USER, GUILD],
        )
        .expect("relic");
    database
}

fn state(snapshot: &DigGearRuntimeSnapshot) -> DigGearRuntimeState {
    DigGearRuntimeState {
        tunnel: snapshot.tunnel.clone(),
        balance: snapshot.balance,
        gear: snapshot.gear.clone(),
        artifacts: snapshot.artifacts.clone(),
    }
}

#[test]
fn slot_swap_relic_equip_balance_and_audit_commit_atomically() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    for piece in &mut proposed.gear {
        if piece.slot == GearSlot::Weapon {
            piece.equipped = piece.tier == 2;
        }
    }
    proposed.artifacts[0].equipped = true;
    proposed.balance -= 25;
    let outcome = repository
        .commit(DigGearRuntimeCommit {
            expected: &expected,
            proposed: &proposed,
            action_type: "gear_fixture",
            detail_json: r#"{"cost":25}"#,
            now: 123,
        })
        .expect("commit");
    let DigGearRuntimeCommitOutcome::Applied(receipt) = outcome else {
        panic!("expected applied outcome")
    };
    assert_eq!(receipt.balance_after, 475);
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert_eq!(after.balance, 475);
    assert!(
        after
            .gear
            .iter()
            .any(|piece| piece.slot == GearSlot::Weapon && piece.tier == 2 && piece.equipped)
    );
    assert!(after.artifacts[0].equipped);
    let connection = Connection::open(database.path()).expect("audit DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT action_type FROM dig_actions WHERE id=?1",
                [receipt.action_id],
                |row| row.get::<_, String>(0)
            )
            .expect("audit"),
        "gear_fixture"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| row
                .get::<_, i64>(0))
            .expect("cleared context"),
        0
    );
}

#[test]
fn stale_snapshot_rolls_back_balance_gear_relic_and_audit() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    proposed.balance -= 20;
    proposed.gear[0].durability = 20;
    proposed.artifacts[0].equipped = true;
    Connection::open(database.path())
        .expect("race DB")
        .execute(
            "UPDATE dig_gear SET durability=4 WHERE id=?1",
            [expected.gear[0].id],
        )
        .expect("racing write");
    assert_eq!(
        repository
            .commit(DigGearRuntimeCommit {
                expected: &expected,
                proposed: &proposed,
                action_type: "gear_repair",
                detail_json: "{}",
                now: 124,
            })
            .expect("conflict"),
        DigGearRuntimeCommitOutcome::Conflict
    );
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert_eq!(after.balance, 500);
    assert!(!after.artifacts[0].equipped);
    assert_eq!(after.gear[0].durability, 4);
    assert!(
        Connection::open(database.path())
            .expect("audit DB")
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .is_ok_and(|count| count == 0)
    );
}

#[test]
fn audit_failure_rolls_back_every_visible_effect() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    proposed.balance -= 25;
    proposed.gear[2].durability = 20;
    Connection::open(database.path())
        .expect("trigger DB")
        .execute_batch(
            "CREATE TRIGGER fail_gear_audit BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='gear_repair'
             BEGIN SELECT RAISE(ABORT,'injected gear audit failure'); END;",
        )
        .expect("failure trigger");
    assert!(
        repository
            .commit(DigGearRuntimeCommit {
                expected: &expected,
                proposed: &proposed,
                action_type: "gear_repair",
                detail_json: "{}",
                now: 125,
            })
            .is_err()
    );
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert_eq!(after.balance, expected.balance);
    assert_eq!(after.gear, expected.gear);
}

#[test]
fn new_shop_row_uses_guarded_repository_sequence() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    proposed.balance -= 180;
    proposed.gear.push(GearPiece {
        id: expected.next_gear_id,
        discord_id: USER,
        guild_id: GUILD,
        slot: GearSlot::Armor,
        tier: 3,
        durability: 20,
        equipped: false,
        acquired_at: expected.clock,
        source: "shop".to_owned(),
        item_id: None,
    });
    assert!(matches!(
        repository
            .commit(DigGearRuntimeCommit {
                expected: &expected,
                proposed: &proposed,
                action_type: "gear_buy",
                detail_json: r#"{"cost":180}"#,
                now: 126,
            })
            .expect("buy"),
        DigGearRuntimeCommitOutcome::Applied(_)
    ));
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert!(
        after
            .gear
            .iter()
            .any(|piece| piece.id == expected.next_gear_id)
    );
    assert_eq!(after.balance, 320);
}

#[test]
fn duplicate_equipped_slot_is_rejected_before_sql() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    proposed
        .gear
        .iter_mut()
        .filter(|piece| piece.slot == GearSlot::Weapon)
        .for_each(|piece| piece.equipped = true);
    assert!(matches!(
        repository.commit(DigGearRuntimeCommit {
            expected: &expected,
            proposed: &proposed,
            action_type: "gear_equip",
            detail_json: "{}",
            now: 127,
        }),
        Err(DigGearRuntimeRepositoryError::InvalidState(_))
    ));
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert!(same_persisted_snapshot(&after, &expected));
}

#[test]
fn concurrent_repair_snapshots_admit_exactly_one_commit() {
    let database = fixture();
    let path = database.path().to_path_buf();
    let repository = DigGearRuntimeRepository::new(&path);
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let expected = expected.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let repository = DigGearRuntimeRepository::new(path);
                let mut proposed = state(&expected);
                proposed.balance -= 10;
                proposed.gear[2].durability = 20;
                barrier.wait();
                repository
                    .commit(DigGearRuntimeCommit {
                        expected: &expected,
                        proposed: &proposed,
                        action_type: "gear_repair",
                        detail_json: "{}",
                        now: 128,
                    })
                    .expect("concurrent outcome")
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigGearRuntimeCommitOutcome::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == DigGearRuntimeCommitOutcome::Conflict)
            .count(),
        1
    );
    let after = repository.snapshot(USER, GUILD).expect("after").unwrap();
    assert_eq!(after.balance, 490);
    assert_eq!(after.gear[2].durability, 20);
}

#[test]
fn proposed_artifact_identity_cannot_be_rewritten() {
    let database = fixture();
    let repository = DigGearRuntimeRepository::new(database.path());
    let expected = repository.snapshot(USER, GUILD).expect("snapshot").unwrap();
    let mut proposed = state(&expected);
    proposed.artifacts[0] = Artifact {
        artifact_id: "different".to_owned(),
        ..proposed.artifacts[0].clone()
    };
    assert!(matches!(
        repository.commit(DigGearRuntimeCommit {
            expected: &expected,
            proposed: &proposed,
            action_type: "relic_equip",
            detail_json: "{}",
            now: 129,
        }),
        Err(DigGearRuntimeRepositoryError::InvalidState(_))
    ));
}

#[test]
fn shop_snapshot_supports_registered_player_before_first_dig() {
    let database = NamedTempFile::new().expect("temporary DB");
    copy_migrated_database(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "shopper", Some(GUILD)))
        .expect("player");
    let snapshot = DigGearRuntimeRepository::new(database.path())
        .shop_snapshot(USER, GUILD)
        .expect("shop snapshot")
        .expect("registered player");
    assert_eq!(snapshot.balance, 3);
    assert_eq!(snapshot.tunnel, None);
    assert_eq!(snapshot.equipped_weapon_tier, None);
    assert_eq!(snapshot.inventory_count, 0);
}

#[test]
fn shop_snapshot_counts_inventory_and_keeps_broken_owned_weapon_tier() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("shop fixture");
    connection
        .execute(
            "UPDATE dig_gear SET equipped=(tier=2),durability=0
              WHERE discord_id=?1 AND guild_id=?2 AND slot='weapon'",
            params![USER, GUILD],
        )
        .expect("equip broken weapon");
    connection
        .execute(
            "INSERT INTO dig_inventory(discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'torch',0,10), (?1,?2,'dynamite',1,11)",
            params![USER, GUILD],
        )
        .expect("inventory");
    let snapshot = DigGearRuntimeRepository::new(database.path())
        .shop_snapshot(USER, GUILD)
        .expect("shop snapshot")
        .expect("registered player");
    assert_eq!(snapshot.equipped_weapon_tier, Some(2));
    assert_eq!(
        snapshot.tunnel.as_ref().map(|tunnel| tunnel.pickaxe_tier),
        Some(1)
    );
    assert_eq!(snapshot.inventory_count, 2);
}
