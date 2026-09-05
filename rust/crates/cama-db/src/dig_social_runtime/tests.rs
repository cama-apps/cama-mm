use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::test_support::copy_migrated_database;

const HELPER: i64 = -81_001;
const TARGET: i64 = -81_002;
const GUILD: i64 = 81_003;
const NOW: i64 = 1_900_000_000;

fn fixture() -> NamedTempFile {
    let database = NamedTempFile::new().expect("social database");
    copy_migrated_database(database.path()).expect("migrated social schema");
    let connection = Connection::open(database.path()).expect("social fixture DB");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy single-column foreign keys");
    for (id, name, balance) in [(HELPER, "helper", 100), (TARGET, "target", 200)] {
        connection
            .execute(
                "INSERT INTO players
                    (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![id, GUILD, name, balance],
            )
            .expect("player fixture");
    }
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                 boss_progress,stat_stamina)
             VALUES (?1,?2,'Target Descent',24,24,0,
                     '{\"25\":\"active\"}',0)",
            params![TARGET, GUILD],
        )
        .expect("target tunnel");
    database
}

fn help_request<'a>(snapshot: &'a DigHelpSnapshot) -> AtomicDigHelpSettlement<'a> {
    AtomicDigHelpSettlement {
        expected: snapshot,
        new_target_depth: 24,
        helper_last_dig_at: NOW,
        helper_reward: 1,
        create_helper_tunnel_name: Some("The Whispering Descent"),
        detail_json: r#"{"target_id":-81002,"advance":0,"target_depth_before":24,"target_depth_after":24,"mentor_bonus":false,"gross_jc":1,"reward_multiplier":0.65}"#,
        created_at: NOW,
    }
}

fn sabotage_fixture() -> NamedTempFile {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("sabotage fixture DB");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                 pickaxe_tier,boss_progress)
             VALUES (?1,?2,'Actor Hollow',40,40,0,2,'{}')",
            params![HELPER, GUILD],
        )
        .expect("actor tunnel");
    connection
        .execute(
            "UPDATE tunnels SET depth=150,max_depth=150,trap_active=0,
                                insured_until=?1,reinforced_until=?2
             WHERE discord_id=?3 AND guild_id=?4",
            params![NOW + 60, NOW + 60, TARGET, GUILD],
        )
        .expect("target sabotage state");
    database
}

#[test]
fn sabotage_snapshot_includes_revisions_relics_weapon_and_recent_targets() {
    let database = sabotage_fixture();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO dig_gear
                (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',4,11,1,?3,'fixture')",
            params![HELPER, GUILD, NOW - 10],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'obsidian_shield',?3,1,1),
                    (?1,?2,'vendetta_coin',?3,1,1)",
            params![TARGET, GUILD, NOW - 10],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,?3,'sabotage',0,0,0,?4,?5)",
            params![
                GUILD,
                HELPER,
                TARGET,
                format!(r#"{{"target_id":{TARGET}}}"#),
                NOW - 100
            ],
        )
        .unwrap();

    let snapshot = DigSocialRuntimeRepository::new(database.path())
        .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 1_000)
        .unwrap();
    assert_eq!(snapshot.actor_balance, Some(100));
    let actor = snapshot.actor_tunnel.unwrap();
    assert_eq!(actor.depth, 40);
    assert_eq!(actor.equipped_weapon_tier, Some(4));
    assert_eq!(actor.equipped_weapon_durability, Some(11));
    let target = snapshot.target_tunnel.unwrap();
    assert_eq!(target.tunnel_name, "Target Descent");
    assert_eq!(target.insured_until, Some(NOW + 60));
    assert_eq!(
        snapshot.target_equipped_relic_ids,
        ["obsidian_shield", "vendetta_coin"]
    );
    assert_eq!(
        snapshot.recent_sabotages,
        [DigRecentSabotage {
            target_id: TARGET,
            created_at: NOW - 100,
        }]
    );
}

#[test]
fn sabotage_hit_commits_wallet_depths_revenge_ledger_and_audit_together() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 7 * 86_400)
        .unwrap();
    let detail = format!(
        r#"{{"target_id":{TARGET},"damage":4,"cost":30,"trap_triggered":false,"attacker_block_reward":5}}"#
    );
    let receipt = match repository
        .atomic_sabotage(
            AtomicDigSabotageSettlement {
                expected: &snapshot,
                target_depth_delta: -4,
                actor_jc_cost: 30,
                target_jc_credit: 0,
                actor_depth_delta: 5,
                clear_target_trap: false,
                revenge: Some(DigSabotageRevenge {
                    target_id: HELPER,
                    kind: "discount",
                    until: NOW + 6 * 3_600,
                }),
                action_type: "sabotage",
                detail_json: &detail,
                created_at: NOW,
            },
            &ProfitDeductionPolicy::none(),
        )
        .unwrap()
    {
        DigSabotageSettlementOutcome::Applied(receipt) => receipt,
        outcome => panic!("unexpected sabotage outcome: {outcome:?}"),
    };
    assert_eq!(receipt.actor_balance_after, 70);
    assert_eq!(receipt.actor_depth_after, Some(45));
    assert_eq!(receipt.target_depth_after, 146);
    let connection = Connection::open(database.path()).unwrap();
    let target: (i64, i64, String, i64) = connection
        .query_row(
            "SELECT depth,revenge_target,revenge_type,revenge_until FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(target, (146, HELPER, "discount".to_owned(), NOW + 21_600));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_id=?2 AND delta=-30
                   AND source='dig' AND related_type='sabotage'",
                params![GUILD, HELPER],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    let action: (String, i64, i64) = connection
        .query_row(
            "SELECT action_type,depth_before,depth_after FROM dig_actions WHERE id=?1",
            [receipt.action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(action, ("sabotage".to_owned(), 0, 0));
}

#[test]
fn sabotage_trap_branch_can_charge_double_and_clears_everything_atomically() {
    let database = sabotage_fixture();
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET trap_active=1 WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 100)
        .unwrap();
    let detail = format!(
        r#"{{"target_id":{TARGET},"trap_triggered":true,"jc_lost":60,"blocks_lost":3,"victim_tip":25}}"#
    );
    let receipt = match repository
        .atomic_sabotage(
            AtomicDigSabotageSettlement {
                expected: &snapshot,
                target_depth_delta: 0,
                actor_jc_cost: 60,
                target_jc_credit: 55,
                actor_depth_delta: -3,
                clear_target_trap: true,
                revenge: None,
                action_type: "sabotage",
                detail_json: &detail,
                created_at: NOW,
            },
            &ProfitDeductionPolicy::none(),
        )
        .unwrap()
    {
        DigSabotageSettlementOutcome::Applied(receipt) => receipt,
        outcome => panic!("unexpected trap outcome: {outcome:?}"),
    };
    assert_eq!(receipt.actor_balance_after, 40);
    assert_eq!(receipt.target_balance_after, Some(255));
    assert_eq!(receipt.actor_depth_after, Some(37));
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT trap_active FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn stale_and_injected_sabotage_settlements_leave_no_partial_writes() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 100)
        .unwrap();
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET depth=149 WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    let detail = format!(r#"{{"target_id":{TARGET},"damage":4,"cost":30}}"#);
    let request = AtomicDigSabotageSettlement {
        expected: &snapshot,
        target_depth_delta: -4,
        actor_jc_cost: 30,
        target_jc_credit: 0,
        actor_depth_delta: 5,
        clear_target_trap: false,
        revenge: None,
        action_type: "sabotage",
        detail_json: &detail,
        created_at: NOW,
    };
    assert_eq!(
        repository
            .atomic_sabotage(request.clone(), &ProfitDeductionPolicy::none())
            .unwrap(),
        DigSabotageSettlementOutcome::Conflict
    );
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET depth=150 WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    assert!(matches!(
        repository.atomic_sabotage_inner(request, &ProfitDeductionPolicy::none(), true),
        Err(DigSocialRuntimeRepositoryError::InjectedFailure)
    ));
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='sabotage'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn concurrent_sabotage_delivery_commits_exactly_once() {
    let database = sabotage_fixture();
    let path = database.path().to_path_buf();
    let snapshot = Arc::new(
        DigSocialRuntimeRepository::new(&path)
            .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 7 * 86_400)
            .expect("sabotage snapshot"),
    );
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let snapshot = Arc::clone(&snapshot);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let detail = format!(
                    r#"{{"target_id":{TARGET},"damage":4,"cost":30,"trap_triggered":false,"attacker_block_reward":5}}"#
                );
                barrier.wait();
                DigSocialRuntimeRepository::new(path).atomic_sabotage(
                    AtomicDigSabotageSettlement {
                        expected: &snapshot,
                        target_depth_delta: -4,
                        actor_jc_cost: 30,
                        target_jc_credit: 0,
                        actor_depth_delta: 5,
                        clear_target_trap: false,
                        revenge: None,
                        action_type: "sabotage",
                        detail_json: &detail,
                        created_at: NOW,
                    },
                    &ProfitDeductionPolicy::none(),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("sabotage thread")
                .expect("sabotage outcome")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigSabotageSettlementOutcome::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigSabotageSettlementOutcome::Conflict))
            .count(),
        1
    );
    let connection = Connection::open(database.path()).expect("verify sabotage race");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        70
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        146
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='sabotage'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn sabotage_mana_steal_keeps_its_own_commit_boundary() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let detail = format!(r#"{{"target_id":{TARGET},"damage":5,"amount":12}}"#);
    assert_eq!(
        repository
            .credit_sabotage_mana_steal(HELPER, TARGET, GUILD, 12, &detail)
            .unwrap(),
        Some(112)
    );
}

#[test]
fn vendetta_reflect_settles_the_debit_credit_and_audit_together() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let detail = format!(r#"{{"target_id":{TARGET},"damage":5}}"#);
    let receipt = repository
        .atomic_vendetta_reflect(DigVendettaReflectSettlement {
            actor_id: HELPER,
            target_id: TARGET,
            guild_id: GUILD,
            reflect: 2,
            bonus: 3,
            reflect_detail_json: &detail,
            bonus_detail_json: &detail,
            created_at: NOW,
        })
        .unwrap();

    assert_eq!(receipt.reflected, 2);
    assert_eq!(receipt.bonus_credited, 3);
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT actor_id FROM dig_actions
                 WHERE id=?1 AND action_type='vendetta_reflect' AND target_id IS NULL",
                [receipt.action_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        TARGET
    );
    assert_eq!(balance(&connection, HELPER), 98);
    assert_eq!(balance(&connection, TARGET), 203);
}

#[test]
fn a_failed_vendetta_bonus_rolls_the_reflect_debit_back() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let detail = format!(r#"{{"target_id":{TARGET},"damage":5}}"#);
    let error = repository
        .atomic_vendetta_reflect_inner(
            DigVendettaReflectSettlement {
                actor_id: HELPER,
                target_id: TARGET,
                guild_id: GUILD,
                reflect: 2,
                bonus: 3,
                reflect_detail_json: &detail,
                bonus_detail_json: &detail,
                created_at: NOW,
            },
            true,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        DigSocialRuntimeRepositoryError::InjectedFailure
    ));
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        balance(&connection, HELPER),
        100,
        "the attacker keeps the reflect the defender was never credited"
    );
    assert_eq!(balance(&connection, TARGET), 200);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='vendetta_reflect'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "a rolled-back reflect leaves no audit row"
    );
}

#[test]
fn a_broke_attacker_reflects_nothing_and_the_audit_records_it() {
    let database = sabotage_fixture();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=1 WHERE discord_id=?1 AND guild_id=?2",
            params![HELPER, GUILD],
        )
        .unwrap();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let detail = format!(r#"{{"target_id":{TARGET},"damage":5}}"#);
    let receipt = repository
        .atomic_vendetta_reflect(DigVendettaReflectSettlement {
            actor_id: HELPER,
            target_id: TARGET,
            guild_id: GUILD,
            reflect: 20,
            bonus: 3,
            reflect_detail_json: &detail,
            bonus_detail_json: &detail,
            created_at: NOW,
        })
        .unwrap();

    assert_eq!(receipt.reflected, 0, "the balance floor blocks the debit");
    assert_eq!(receipt.bonus_credited, 3);
    assert_eq!(balance(&connection, HELPER), 1);
    let audit: String = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            [receipt.action_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        audit.contains(r#""reflected":0"#),
        "the audit reports what moved, got {audit}"
    );
}

#[test]
fn a_missing_defender_reports_no_vendetta_bonus() {
    let database = sabotage_fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let detail = format!(r#"{{"target_id":{TARGET},"damage":5}}"#);
    let receipt = repository
        .atomic_vendetta_reflect(DigVendettaReflectSettlement {
            actor_id: HELPER,
            target_id: TARGET,
            guild_id: GUILD + 1,
            reflect: 2,
            bonus: 3,
            reflect_detail_json: &detail,
            bonus_detail_json: &detail,
            created_at: NOW,
        })
        .unwrap();

    assert_eq!(receipt.reflected, 0);
    assert_eq!(
        receipt.bonus_credited, 0,
        "an uncreditable bonus must not be reported as paid"
    );
}

fn balance(connection: &Connection, discord_id: i64) -> i64 {
    connection
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, GUILD],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn migrated_snapshot_includes_relics_and_full_help_revision() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("snapshot fixture");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                 stat_stamina,mutations,injury_state,temp_curses)
             VALUES (?1,?2,'Helper Hollow',10,10,123,4,
                     '[\"restless\"]','{\"type\":\"slower_cooldown\"}',
                     '{\"digs_remaining\":2}')",
            params![HELPER, GUILD],
        )
        .expect("helper tunnel");
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mycelium_link',?3,1,1),
                    (?1,?2,'mentors_lantern',?3,1,1)",
            params![HELPER, GUILD, NOW],
        )
        .expect("helper relics");

    let snapshot = DigSocialRuntimeRepository::new(database.path())
        .help_snapshot(HELPER, TARGET, GUILD)
        .expect("help snapshot");
    let helper = snapshot.helper_tunnel.expect("helper tunnel snapshot");
    assert_eq!(helper.last_dig_at, Some(123));
    assert_eq!(helper.stat_stamina, 4);
    assert_eq!(helper.mutations_json.as_deref(), Some("[\"restless\"]"));
    assert_eq!(
        snapshot.helper_equipped_relic_ids,
        ["mycelium_link", "mentors_lantern"]
    );
    assert_eq!(
        snapshot.target_tunnel.expect("target tunnel").tunnel_name,
        "Target Descent"
    );
}

#[test]
fn help_creates_helper_and_commits_target_cooldown_wallet_ledger_and_audit() {
    let database = fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .help_snapshot(HELPER, TARGET, GUILD)
        .expect("help snapshot");
    let receipt = match repository
        .atomic_help(help_request(&snapshot), &ProfitDeductionPolicy::none())
        .expect("help settlement")
    {
        DigHelpSettlementOutcome::Applied(receipt) => receipt,
        outcome => panic!("unexpected help outcome: {outcome:?}"),
    };
    assert_eq!(receipt.target_depth_after, 24);
    assert_eq!(receipt.helper_balance_after, 101);

    let connection = Connection::open(database.path()).expect("verify help");
    let helper: (String, i64, i64) = connection
        .query_row(
            "SELECT tunnel_name,last_dig_at,
                    (SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2)
             FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![HELPER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("helper state");
    assert_eq!(helper, ("The Whispering Descent".to_owned(), NOW, 101));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_id=?2 AND delta=1
                   AND source='dig' AND related_type='help'",
                params![GUILD, HELPER],
                |row| row.get::<_, i64>(0),
            )
            .expect("help ledger"),
        1
    );
    let action: (i64, i64, i64, String) = connection
        .query_row(
            "SELECT actor_id,target_id,jc_delta,detail FROM dig_actions WHERE id=?1",
            [receipt.action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("help action");
    assert_eq!((action.0, action.1, action.2), (HELPER, TARGET, 1));
    assert!(action.3.contains("target_depth_before"));
}

#[test]
fn stale_help_revision_rolls_back_target_and_wallet() {
    let database = fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .help_snapshot(HELPER, TARGET, GUILD)
        .expect("help snapshot");
    Connection::open(database.path())
        .expect("conflict fixture")
        .execute(
            "UPDATE tunnels SET depth=23 WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .expect("change target");
    assert_eq!(
        repository
            .atomic_help(help_request(&snapshot), &ProfitDeductionPolicy::none())
            .expect("help conflict"),
        DigHelpSettlementOutcome::Conflict
    );
    let connection = Connection::open(database.path()).expect("verify conflict");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
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
fn concurrent_help_delivery_commits_exactly_once() {
    let database = fixture();
    let path = database.path().to_path_buf();
    let snapshot = Arc::new(
        DigSocialRuntimeRepository::new(&path)
            .help_snapshot(HELPER, TARGET, GUILD)
            .expect("help snapshot"),
    );
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let snapshot = Arc::clone(&snapshot);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                DigSocialRuntimeRepository::new(path)
                    .atomic_help(help_request(&snapshot), &ProfitDeductionPolicy::none())
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("help thread").expect("help outcome"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigHelpSettlementOutcome::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigHelpSettlementOutcome::Conflict))
            .count(),
        1
    );
    let connection = Connection::open(database.path()).expect("verify concurrent help");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        101
    );
}

#[test]
fn injected_help_failure_rolls_back_every_write() {
    let database = fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    let snapshot = repository
        .help_snapshot(HELPER, TARGET, GUILD)
        .expect("help snapshot");
    assert!(matches!(
        repository.atomic_help_inner(
            help_request(&snapshot),
            &ProfitDeductionPolicy::none(),
            true
        ),
        Err(DigSocialRuntimeRepositoryError::InjectedFailure)
    ));
    let connection = Connection::open(database.path()).expect("verify help rollback");
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        24
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

fn add_artifact(database: &NamedTempFile, artifact_id: &str, is_relic: bool) -> i64 {
    let connection = Connection::open(database.path()).expect("artifact fixture");
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,?3,?4,?5,1)",
            params![HELPER, GUILD, artifact_id, NOW - 10, is_relic],
        )
        .expect("artifact row");
    connection.last_insert_rowid()
}

#[test]
fn gift_transfers_one_relic_and_unequips_all_giver_copies() {
    let database = fixture();
    let source_id = add_artifact(&database, "mole_claws", true);
    add_artifact(&database, "mole_claws", true);
    let outcome = DigSocialRuntimeRepository::new(database.path())
        .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
        .expect("gift relic");
    let DigGiftOutcome::Applied(receipt) = outcome else {
        panic!("unexpected gift outcome: {outcome:?}");
    };
    assert_eq!(receipt.source_row_id, source_id);
    assert_eq!(receipt.artifact_id, "mole_claws");
    let connection = Connection::open(database.path()).expect("verify gift");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='mole_claws' AND equipped=1",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                 WHERE id=?1 AND discord_id=?2 AND guild_id=?3 AND equipped=0",
                params![receipt.receiver_row_id, TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn gift_validates_self_nonrelic_missing_and_receiver_tunnel() {
    let database = fixture();
    let repository = DigSocialRuntimeRepository::new(database.path());
    assert_eq!(
        repository
            .gift_relic(HELPER, HELPER, GUILD, "mole_claws", NOW)
            .unwrap(),
        DigGiftOutcome::SelfGift
    );
    assert_eq!(
        repository
            .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
            .unwrap(),
        DigGiftOutcome::MissingRelic
    );
    add_artifact(&database, "plain_rock", false);
    assert_eq!(
        repository
            .gift_relic(HELPER, TARGET, GUILD, "plain_rock", NOW)
            .unwrap(),
        DigGiftOutcome::NonRelic
    );
    let connection = Connection::open(database.path()).expect("receiver fixture");
    connection
        .execute(
            "DELETE FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    add_artifact(&database, "mole_claws", true);
    assert_eq!(
        repository
            .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
            .unwrap(),
        DigGiftOutcome::MissingReceiverTunnel
    );
}

#[test]
fn concurrent_gift_delivery_transfers_exactly_once() {
    let database = fixture();
    add_artifact(&database, "mole_claws", true);
    let path = database.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                DigSocialRuntimeRepository::new(path).gift_relic(
                    HELPER,
                    TARGET,
                    GUILD,
                    "mole_claws",
                    NOW,
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("gift thread").expect("gift outcome"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigGiftOutcome::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DigGiftOutcome::MissingRelic))
            .count(),
        1
    );
    let connection = Connection::open(database.path()).expect("verify gift race");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts WHERE artifact_id='mole_claws'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn injected_gift_failure_preserves_source_and_creates_no_receiver_copy() {
    let database = fixture();
    add_artifact(&database, "mole_claws", true);
    let repository = DigSocialRuntimeRepository::new(database.path());
    assert!(matches!(
        repository.gift_relic_inner(HELPER, TARGET, GUILD, "mole_claws", NOW, true),
        Err(DigSocialRuntimeRepositoryError::InjectedFailure)
    ));
    let connection = Connection::open(database.path()).expect("verify gift rollback");
    assert_eq!(
        connection
            .query_row(
                "SELECT discord_id,equipped FROM dig_artifacts WHERE artifact_id='mole_claws'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (HELPER, 1)
    );
}

static NO_ROSTER: BTreeSet<i64> = BTreeSet::new();

/// (penalty games remaining, vanity-taxable, JC withheld from a 120 JC credit)
/// at 5% per outstanding game and a flat 10% vanity share.
const WITHHOLDING_CASES: [(i64, bool, i64); 4] =
    [(1, false, 6), (6, false, 36), (0, true, 12), (0, false, 0)];

fn penalized(database: &NamedTempFile, discord_id: i64, remaining: i64) {
    Connection::open(database.path())
        .expect("penalty DB")
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,?3,1)",
            params![discord_id, GUILD, remaining],
        )
        .expect("penalty row");
}

fn vanity_roster(taxed: bool, discord_id: i64) -> BTreeSet<i64> {
    if taxed {
        BTreeSet::from([discord_id])
    } else {
        BTreeSet::new()
    }
}

fn withholding_policy(vanity: &BTreeSet<i64>) -> ProfitDeductionPolicy<'_> {
    ProfitDeductionPolicy {
        bankruptcy: Some(BankruptcyPenaltyPolicy {
            rate_per_game: 0.05,
        }),
        vanity_tax_rate: 0.10,
        vanity_taxable_ids: vanity,
        low_priority_tax_rate: 0.25,
        low_priority_taxable_ids: &NO_ROSTER,
    }
}

/// Every negative ledger row attributed to `related_type` for one player.
fn withheld_rows(
    connection: &Connection,
    discord_id: i64,
    related_type: &str,
) -> Vec<(String, String, i64)> {
    let mut statement = connection
        .prepare(
            "SELECT source,reason,delta FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND related_type=?3 AND delta<0
             ORDER BY rowid",
        )
        .unwrap();
    statement
        .query_map(params![GUILD, discord_id, related_type], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn expected_withheld_rows(
    reason: &str,
    remaining: i64,
    vanity_taxed: bool,
    withheld: i64,
) -> Vec<(String, String, i64)> {
    if remaining > 0 {
        vec![(
            "dig".to_owned(),
            format!("{reason} bankruptcy penalty"),
            -withheld,
        )]
    } else if vanity_taxed {
        vec![(
            "vanity_tax".to_owned(),
            "vanity tax on JC profit".to_owned(),
            -withheld,
        )]
    } else {
        Vec::new()
    }
}

#[test]
fn help_reward_withholds_bankruptcy_and_vanity_shares() {
    for (remaining, vanity_taxed, withheld) in WITHHOLDING_CASES {
        let database = fixture();
        if remaining > 0 {
            penalized(&database, HELPER, remaining);
        }
        let vanity = vanity_roster(vanity_taxed, HELPER);
        let repository = DigSocialRuntimeRepository::new(database.path());
        let snapshot = repository.help_snapshot(HELPER, TARGET, GUILD).unwrap();
        let receipt = match repository
            .atomic_help(
                AtomicDigHelpSettlement {
                    helper_reward: 120,
                    ..help_request(&snapshot)
                },
                &withholding_policy(&vanity),
            )
            .unwrap()
        {
            DigHelpSettlementOutcome::Applied(receipt) => receipt,
            outcome => panic!("unexpected help outcome: {outcome:?}"),
        };
        assert_eq!(receipt.helper_deductions.profit, 120);
        assert_eq!(
            receipt.helper_deductions.total(),
            withheld,
            "remaining={remaining} vanity={vanity_taxed}"
        );
        assert_eq!(receipt.helper_balance_after, 100 + 120 - withheld);
        let connection = Connection::open(database.path()).unwrap();
        assert_eq!(balance(&connection, HELPER), 100 + 120 - withheld);
        assert_eq!(
            withheld_rows(&connection, HELPER, "help"),
            expected_withheld_rows("dig help reward", remaining, vanity_taxed, withheld)
        );
    }
}

#[test]
fn help_target_bonus_withholds_bankruptcy_and_vanity_shares() {
    for (remaining, vanity_taxed, withheld) in WITHHOLDING_CASES {
        let database = fixture();
        if remaining > 0 {
            penalized(&database, TARGET, remaining);
        }
        let vanity = vanity_roster(vanity_taxed, TARGET);
        let receipt = DigSocialRuntimeRepository::new(database.path())
            .credit_help_target_bonus(
                HELPER,
                TARGET,
                GUILD,
                120,
                r#"{"bonus":120}"#,
                &withholding_policy(&vanity),
            )
            .unwrap()
            .expect("target player exists");
        assert_eq!(receipt.deductions.profit, 120);
        assert_eq!(
            receipt.deductions.total(),
            withheld,
            "remaining={remaining} vanity={vanity_taxed}"
        );
        assert_eq!(receipt.balance_after, 200 + 120 - withheld);
        let connection = Connection::open(database.path()).unwrap();
        assert_eq!(balance(&connection, TARGET), 200 + 120 - withheld);
        assert_eq!(
            withheld_rows(&connection, TARGET, "mentor_bonus"),
            expected_withheld_rows("dig mentor target bonus", remaining, vanity_taxed, withheld)
        );
    }
}

#[test]
fn sabotage_target_credit_withholds_bankruptcy_and_vanity_shares() {
    for (remaining, vanity_taxed, withheld) in WITHHOLDING_CASES {
        let database = sabotage_fixture();
        if remaining > 0 {
            penalized(&database, TARGET, remaining);
        }
        let vanity = vanity_roster(vanity_taxed, TARGET);
        let repository = DigSocialRuntimeRepository::new(database.path());
        let snapshot = repository
            .sabotage_snapshot(HELPER, TARGET, GUILD, NOW - 7 * 86_400)
            .unwrap();
        let detail = format!(
            r#"{{"target_id":{TARGET},"trap_triggered":true,"jc_lost":10,"victim_tip":120}}"#
        );
        let receipt = match repository
            .atomic_sabotage(
                AtomicDigSabotageSettlement {
                    expected: &snapshot,
                    target_depth_delta: 0,
                    actor_jc_cost: 10,
                    target_jc_credit: 120,
                    actor_depth_delta: 0,
                    clear_target_trap: false,
                    revenge: None,
                    action_type: "sabotage",
                    detail_json: &detail,
                    created_at: NOW,
                },
                &withholding_policy(&vanity),
            )
            .unwrap()
        {
            DigSabotageSettlementOutcome::Applied(receipt) => receipt,
            outcome => panic!("unexpected sabotage outcome: {outcome:?}"),
        };
        assert_eq!(receipt.target_deductions.profit, 120);
        assert_eq!(
            receipt.target_deductions.total(),
            withheld,
            "remaining={remaining} vanity={vanity_taxed}"
        );
        assert_eq!(receipt.actor_balance_after, 90);
        assert_eq!(receipt.target_balance_after, Some(200 + 120 - withheld));
        let connection = Connection::open(database.path()).unwrap();
        assert_eq!(balance(&connection, TARGET), 200 + 120 - withheld);
        assert_eq!(
            withheld_rows(&connection, TARGET, "sabotage"),
            expected_withheld_rows(
                "dig sabotage target credit",
                remaining,
                vanity_taxed,
                withheld
            )
        );
    }
}
