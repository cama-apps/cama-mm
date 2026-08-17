use std::collections::VecDeque;

use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::dig_miner_runtime::{DigMinerAllocation, DigMinerAutoBuyUpdate};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const USER: i64 = -94_001;
const GUILD: i64 = 94_002;
const NOW: i64 = 1_900_000_000;

#[derive(Debug)]
struct ScriptedNameEntropy {
    units: VecDeque<f64>,
    indexes: VecDeque<usize>,
}

impl Default for ScriptedNameEntropy {
    fn default() -> Self {
        Self {
            units: VecDeque::from([0.0]),
            indexes: VecDeque::from([0, 0]),
        }
    }
}

impl TunnelNameEntropy for ScriptedNameEntropy {
    fn unit_f64(&mut self) -> f64 {
        self.units.pop_front().expect("scripted name roll")
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.indexes.pop_front().expect("scripted name index") % upper_exclusive
    }
}

fn fixture(balance: i64) -> NamedTempFile {
    let database = NamedTempFile::new().expect("miner app database");
    initialize_or_migrate(database.path()).expect("migrated miner app schema");
    let connection = Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python legacy foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES (?1,?2,'miner-app',?3)",
            params![USER, GUILD, balance],
        )
        .unwrap();
    database
}

fn scripted_profile(
    service: &DigMinerRuntimeService,
) -> Result<DigMinerProfile, DigMinerRuntimeError> {
    service.profile_with_entropy(USER, GUILD, NOW, &mut ScriptedNameEntropy::default())
}

#[test]
fn profile_requires_registration_and_creates_an_exact_first_dig_tunnel() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .profile(USER + 1, GUILD, NOW)
            .unwrap_err()
            .to_string(),
        "You need to register first. Use /player register."
    );
    let profile = scripted_profile(&service).unwrap();
    assert_eq!(profile.tunnel_name, "The Whispering Descent");
    assert_eq!(
        profile.stats,
        DigMinerStats {
            strength: 0,
            smarts: 0,
            stamina: 0,
            stat_points: 5,
            spent_points: 0,
            unspent_points: 5,
        }
    );
    assert_eq!(
        profile.auto_buy,
        DigMinerAutoBuy {
            torch: false,
            hard_hat: false
        }
    );
    assert_eq!(profile.awarded_bosses, Vec::<i64>::new());
    let state: (i64, Option<i64>) = Connection::open(database.path())
        .unwrap()
        .query_row(
            "SELECT total_digs,last_dig_at FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (0, None));
}

#[test]
fn backstory_sanitizes_mentions_whitespace_length_and_locks_once() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    let raw = format!("  Former\n cartographer @everyone  {}", "x".repeat(600));
    let profile = service
        .set_backstory_with_entropy(USER, GUILD, &raw, NOW, &mut ScriptedNameEntropy::default())
        .unwrap();
    assert!(
        profile
            .backstory
            .starts_with("Former cartographer (at)everyone ")
    );
    assert_eq!(
        profile.backstory.chars().count(),
        MINER_BACKSTORY_MAX_LENGTH
    );
    assert_eq!(
        service
            .set_backstory(USER, GUILD, "Actually a duke in exile.", NOW)
            .unwrap_err()
            .to_string(),
        "Your miner backstory is already set and cannot be changed."
    );
}

#[test]
fn blank_backstory_is_rejected_after_profile_creation() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .set_backstory_with_entropy(
                USER,
                GUILD,
                " \n\t ",
                NOW,
                &mut ScriptedNameEntropy::default(),
            )
            .unwrap_err()
            .to_string(),
        "Provide a backstory to lock in."
    );
    assert!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT miner_about FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stat_allocation_returns_exact_budget_and_effect_formulas() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    let profile = service
        .allocate_stats_with_entropy(
            USER,
            GUILD,
            DigMinerAllocation {
                strength: 2,
                smarts: 2,
                stamina: 1,
            },
            NOW,
            &mut ScriptedNameEntropy::default(),
        )
        .unwrap();
    assert_eq!(
        profile.stats,
        DigMinerStats {
            strength: 2,
            smarts: 2,
            stamina: 1,
            stat_points: 5,
            spent_points: 5,
            unspent_points: 0,
        }
    );
    assert_eq!(profile.effects.advance_min_bonus, 0);
    assert_eq!(profile.effects.advance_max_bonus, 1);
    assert!((profile.effects.cave_in_reduction - 0.04).abs() < 1e-12);
    assert!((profile.effects.cooldown_multiplier - 0.96).abs() < 1e-12);
    assert!((profile.effects.paid_cost_multiplier - 0.96).abs() < 1e-12);
}

#[test]
fn stat_allocation_errors_match_python_service_copy() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    scripted_profile(&service).unwrap();
    for (allocation, expected) in [
        (
            DigMinerAllocation {
                strength: -1,
                smarts: 0,
                stamina: 0,
            },
            "S stats cannot be negative.".to_owned(),
        ),
        (
            DigMinerAllocation {
                strength: 0,
                smarts: 0,
                stamina: 0,
            },
            "Spend at least one point.".to_owned(),
        ),
        (
            DigMinerAllocation {
                strength: 5,
                smarts: 1,
                stamina: 0,
            },
            "That spends 6 points, but you only have 5 unspent.".to_owned(),
        ),
    ] {
        assert_eq!(
            service
                .allocate_stats(USER, GUILD, allocation, NOW)
                .unwrap_err()
                .to_string(),
            expected
        );
    }
}

#[test]
fn auto_buy_partial_updates_match_profile_contract() {
    let database = fixture(100);
    let service = DigMinerRuntimeService::sqlite(database.path());
    let both = service
        .set_auto_buy_with_entropy(
            USER,
            GUILD,
            DigMinerAutoBuyUpdate {
                torch: Some(true),
                hard_hat: Some(true),
            },
            NOW,
            &mut ScriptedNameEntropy::default(),
        )
        .unwrap();
    assert_eq!(
        both.auto_buy,
        DigMinerAutoBuy {
            torch: true,
            hard_hat: true
        }
    );
    let torch_off = service
        .set_auto_buy(
            USER,
            GUILD,
            DigMinerAutoBuyUpdate {
                torch: Some(false),
                hard_hat: None,
            },
            NOW,
        )
        .unwrap();
    assert_eq!(
        torch_off.auto_buy,
        DigMinerAutoBuy {
            torch: false,
            hard_hat: true
        }
    );
    assert_eq!(
        service
            .set_auto_buy(
                USER,
                GUILD,
                DigMinerAutoBuyUpdate {
                    torch: None,
                    hard_hat: None,
                },
                NOW,
            )
            .unwrap_err()
            .to_string(),
        "Choose at least one auto-buy setting to update."
    );
}

#[test]
fn profile_decodes_current_prestige_awards_and_rejects_stale_or_legacy_ledgers() {
    let database = fixture(100);
    scripted_profile(&DigMinerRuntimeService::sqlite(database.path())).unwrap();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE tunnels SET prestige_level=2,stat_points=8,stat_boss_awards=?1
             WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"prestige_level":2,"awards":[75,25,75,"bad"]}"#,
                USER,
                GUILD
            ],
        )
        .unwrap();
    assert_eq!(
        DigMinerRuntimeService::sqlite(database.path())
            .profile(USER, GUILD, NOW)
            .unwrap()
            .awarded_bosses,
        [25, 75]
    );
    connection
        .execute(
            "UPDATE tunnels SET stat_boss_awards='[25,50]'
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    assert!(
        DigMinerRuntimeService::sqlite(database.path())
            .profile(USER, GUILD, NOW)
            .unwrap()
            .awarded_bosses
            .is_empty()
    );
}

#[test]
fn respec_returns_allocated_and_total_unspent_separately() {
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
    let result = DigMinerRuntimeService::sqlite(database.path())
        .respec(USER, GUILD, NOW)
        .unwrap();
    assert_eq!(result.cost, 50);
    assert_eq!(result.returned_points, 5);
    assert_eq!(result.stats.unspent_points, 12);
    assert_eq!(result.stats.stat_points, 12);
    assert_eq!(result.stats.spent_points, 0);
    assert_eq!(result.action_id, 1);
}

#[test]
fn respec_errors_match_python_and_preserve_state() {
    let database = fixture(49);
    let service = DigMinerRuntimeService::sqlite(database.path());
    assert_eq!(
        service.respec(USER, GUILD, NOW).unwrap_err().to_string(),
        "You don't have any allocated S points to reset."
    );
    service
        .allocate_stats_with_entropy(
            USER,
            GUILD,
            DigMinerAllocation {
                strength: 3,
                smarts: 2,
                stamina: 0,
            },
            NOW,
            &mut ScriptedNameEntropy::default(),
        )
        .unwrap();
    assert_eq!(
        service.respec(USER, GUILD, NOW).unwrap_err().to_string(),
        "You need 50 JC to respec your S points."
    );
    let profile = service.profile(USER, GUILD, NOW).unwrap();
    assert_eq!((profile.stats.strength, profile.stats.smarts), (3, 2));
}
