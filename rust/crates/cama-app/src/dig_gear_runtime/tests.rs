use std::sync::{Arc, Barrier};
use std::thread;

use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_domain::dig_gear::GearSlot;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const USER: i64 = 82_001;
const GUILD: i64 = 82_002;

fn fixture(balance: i64, depth: i64, prestige: i64) -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary DB");
    initialize_or_migrate(database.path()).expect("canonical schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "gear runtime miner", Some(GUILD)))
        .expect("player");
    let connection = Connection::open(database.path()).expect("fixture DB");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![balance, USER, GUILD],
        )
        .expect("balance");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,pickaxe_tier,prestige_level,
              relic_trim_notice)
             VALUES (?1,?2,?3,?3,1,?4,1)",
            params![USER, GUILD, depth, prestige],
        )
        .expect("tunnel");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',1,20,1,10,'fixture'),
                    (?1,?2,'weapon',2,8,0,11,'fixture'),
                    (?1,?2,'armor',2,5,0,12,'fixture'),
                    (?1,?2,'boots',1,19,0,13,'fixture')",
            params![USER, GUILD],
        )
        .expect("gear");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',10,1,0),
                    (?1,?2,'lucky_geode',11,1,0),
                    (?1,?2,'ordinary_art',12,0,0)",
            params![USER, GUILD],
        )
        .expect("artifacts");
    database
}

#[test]
fn panel_projects_every_owned_piece_relic_cap_and_trim_notice() {
    let database = fixture(500, 100, 1);
    let panel = DigGearRuntimeService::sqlite(database.path())
        .panel(USER, GUILD)
        .expect("panel")
        .expect("tunnel");
    assert_eq!(panel.pieces.len(), 4);
    assert_eq!(panel.relics.len(), 2);
    assert_eq!(panel.relic_cap, 2);
    assert_eq!(panel.balance, 500);
    assert!(panel.trim_notice);
    assert!(
        panel
            .pieces
            .iter()
            .any(|piece| { piece.slot == GearSlot::Weapon && piece.tier == 1 && piece.equipped })
    );
}

#[test]
fn equip_swap_is_atomic_and_duplicate_retry_is_not_reaudited() {
    let database = fixture(500, 100, 1);
    let service = DigGearRuntimeService::sqlite(database.path());
    let target = service
        .panel(USER, GUILD)
        .expect("panel")
        .unwrap()
        .pieces
        .into_iter()
        .find(|piece| piece.slot == GearSlot::Weapon && piece.tier == 2)
        .expect("target");
    let first = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::EquipGear { gear_id: target.id },
            200,
        )
        .expect("equip");
    assert!(first.success);
    assert_eq!(first.slot, Some(GearSlot::Weapon));
    assert!(first.panel.as_ref().is_some_and(|panel| {
        panel
            .pieces
            .iter()
            .filter(|piece| piece.equipped && piece.slot == GearSlot::Weapon)
            .count()
            == 1
    }));
    let retry = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::EquipGear { gear_id: target.id },
            201,
        )
        .expect("retry");
    assert!(!retry.success);
    assert!(
        retry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("already equipped"))
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("audit DB")
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        1
    );
}

#[test]
fn broken_piece_cannot_be_equipped() {
    let database = fixture(500, 100, 1);
    let connection = Connection::open(database.path()).expect("break DB");
    let armor_id: i64 = connection
        .query_row(
            "SELECT id FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 AND slot='armor'",
            params![USER, GUILD],
            |row| row.get(0),
        )
        .expect("armor");
    connection
        .execute("UPDATE dig_gear SET durability=0 WHERE id=?1", [armor_id])
        .expect("break armor");
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::EquipGear { gear_id: armor_id },
            202,
        )
        .expect("outcome");
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|error| error.contains("broken"))
    );
}

#[test]
fn one_piece_repair_uses_canonical_prorated_cost_and_full_durability() {
    let database = fixture(500, 100, 1);
    let panel = DigGearRuntimeService::sqlite(database.path())
        .panel(USER, GUILD)
        .expect("panel")
        .unwrap();
    let armor = panel
        .pieces
        .iter()
        .find(|piece| piece.slot == GearSlot::Armor)
        .expect("armor");
    let expected_cost = cama_domain::dig_gear::repair_cost(
        armor.slot,
        armor.tier,
        armor.item_id.as_deref(),
        armor.durability,
        Some(armor.max_durability),
    );
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::RepairGear { gear_id: armor.id },
            203,
        )
        .expect("repair");
    assert!(outcome.success);
    assert_eq!(outcome.cost, expected_cost);
    assert_eq!(outcome.balance_after, 500 - expected_cost);
    assert!(outcome.panel.as_ref().is_some_and(|panel| {
        panel
            .pieces
            .iter()
            .find(|piece| piece.id == armor.id)
            .is_some_and(|piece| piece.durability == piece.max_durability)
    }));
}

#[test]
fn repair_all_is_one_bill_and_repairs_every_damaged_piece() {
    let database = fixture(500, 100, 1);
    let service = DigGearRuntimeService::sqlite(database.path());
    let before = service.panel(USER, GUILD).expect("panel").unwrap();
    let expected_cost = before
        .pieces
        .iter()
        .filter(|piece| piece.durability < piece.max_durability)
        .map(|piece| {
            cama_domain::dig_gear::repair_cost(
                piece.slot,
                piece.tier,
                piece.item_id.as_deref(),
                piece.durability,
                Some(piece.max_durability),
            )
        })
        .sum::<i64>();
    let outcome = service
        .execute(USER, GUILD, DigGearRuntimeAction::RepairAll, 204)
        .expect("repair all");
    assert!(outcome.success);
    assert_eq!(outcome.cost, expected_cost);
    assert_eq!(outcome.repaired, 3);
    assert!(outcome.panel.as_ref().is_some_and(|panel| {
        panel
            .pieces
            .iter()
            .all(|piece| piece.durability == piece.max_durability)
    }));
}

#[test]
fn repair_all_insufficient_balance_changes_nothing() {
    let database = fixture(0, 100, 1);
    let service = DigGearRuntimeService::sqlite(database.path());
    let before = service.panel(USER, GUILD).expect("before").unwrap();
    let outcome = service
        .execute(USER, GUILD, DigGearRuntimeAction::RepairAll, 205)
        .expect("outcome");
    assert!(!outcome.success);
    assert_eq!(service.panel(USER, GUILD).expect("after").unwrap(), before);
}

#[test]
fn gear_purchase_enforces_depth_prestige_and_balance_then_inserts() {
    let database = fixture(1_000, 100, 1);
    let service = DigGearRuntimeService::sqlite(database.path());
    let gated = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyGear {
                slot: "armor".to_owned(),
                tier: 5,
            },
            206,
        )
        .expect("gated buy");
    assert!(!gated.success);
    assert!(
        gated
            .error
            .as_deref()
            .is_some_and(|error| error.contains("depth 150"))
    );
    let bought = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyGear {
                slot: "armor".to_owned(),
                tier: 3,
            },
            207,
        )
        .expect("buy");
    assert!(bought.success);
    assert_eq!(bought.cost, 180);
    assert_eq!(bought.balance_after, 820);
    assert!(bought.panel.as_ref().is_some_and(|panel| {
        panel
            .pieces
            .iter()
            .any(|piece| piece.slot == GearSlot::Armor && piece.tier == 3)
    }));
}

#[test]
fn relic_cap_duplicate_and_non_relic_rules_are_domain_authoritative() {
    let database = fixture(500, 100, 0);
    let service = DigGearRuntimeService::sqlite(database.path());
    let panel = service.panel(USER, GUILD).expect("panel").unwrap();
    let first = panel.relics[0].row_id;
    let second = panel.relics[1].row_id;
    assert!(
        service
            .execute(
                USER,
                GUILD,
                DigGearRuntimeAction::EquipRelic {
                    artifact_row_id: first,
                },
                208,
            )
            .expect("first equip")
            .success
    );
    let capped = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::EquipRelic {
                artifact_row_id: second,
            },
            209,
        )
        .expect("cap");
    assert!(!capped.success);
    assert!(
        capped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cap (1)"))
    );
    let ordinary_id: i64 = Connection::open(database.path())
        .expect("artifact DB")
        .query_row(
            "SELECT id FROM dig_artifacts WHERE discord_id=?1 AND guild_id=?2 AND is_relic=0",
            params![USER, GUILD],
            |row| row.get(0),
        )
        .expect("ordinary artifact");
    let ordinary = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::EquipRelic {
                artifact_row_id: ordinary_id,
            },
            210,
        )
        .expect("ordinary");
    assert!(!ordinary.success);
    assert!(
        ordinary
            .error
            .as_deref()
            .is_some_and(|error| error.contains("isn't a relic"))
    );
}

#[test]
fn trim_notice_is_one_shot_and_audited_once() {
    let database = fixture(500, 100, 1);
    let service = DigGearRuntimeService::sqlite(database.path());
    let first = service
        .execute(USER, GUILD, DigGearRuntimeAction::PopTrimNotice, 211)
        .expect("first");
    assert!(first.success);
    assert!(!first.panel.expect("panel").trim_notice);
    let retry = service
        .execute(USER, GUILD, DigGearRuntimeAction::PopTrimNotice, 212)
        .expect("retry");
    assert!(!retry.success);
    assert_eq!(
        Connection::open(database.path())
            .expect("audit DB")
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("count"),
        1
    );
}

#[test]
fn missing_tunnel_returns_typed_rejection_without_writes() {
    let database = NamedTempFile::new().expect("temporary DB");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "no tunnel", Some(GUILD)))
        .expect("player");
    let service = DigGearRuntimeService::sqlite(database.path());
    assert!(service.panel(USER, GUILD).expect("panel").is_none());
    let outcome = service
        .execute(USER, GUILD, DigGearRuntimeAction::RepairAll, 213)
        .expect("outcome");
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|error| error.contains("tunnel"))
    );
}

fn set_owned_weapon(database: &NamedTempFile, tier: i64, durability: i64) {
    let connection = Connection::open(database.path()).expect("weapon DB");
    connection
        .execute(
            "UPDATE tunnels SET pickaxe_tier=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![tier, USER, GUILD],
        )
        .expect("legacy pickaxe tier");
    connection
        .execute(
            "UPDATE dig_gear SET equipped=0
              WHERE discord_id=?1 AND guild_id=?2 AND slot='weapon'",
            params![USER, GUILD],
        )
        .expect("unequip weapons");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',?3,?4,1,99,'fixture')",
            params![USER, GUILD, tier, durability],
        )
        .expect("owned weapon");
}

#[test]
fn shop_projects_all_consumables_descriptions_and_every_paid_gear_row() {
    let database = fixture(10_000, 275, 5);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("registered player");
    assert_eq!(shop.consumables.len(), 13);
    assert!(
        shop.consumables.iter().all(|item| {
            item.price > 0 && !item.name.is_empty() && !item.description.is_empty()
        })
    );
    for slot in [GearSlot::Armor, GearSlot::Boots, GearSlot::Amulet] {
        assert_eq!(
            shop.gear_for_sale
                .iter()
                .filter(|item| item.slot == slot)
                .map(|item| item.tier)
                .collect::<Vec<_>>(),
            (1_u8..=7).collect::<Vec<_>>()
        );
    }
}

#[test]
fn shop_succeeds_before_first_dig_and_filters_owned_pickaxe_rungs() {
    let database = NamedTempFile::new().expect("temporary DB");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "shopper", Some(GUILD)))
        .expect("player");
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("registered player");
    assert!(!shop.has_tunnel);
    assert_eq!(shop.pickaxe_upgrades.len(), 7);
    assert_eq!(shop.pickaxe_upgrades[0].tier, 1);
}

#[test]
fn broken_high_tier_weapon_is_owned_for_shop_progression() {
    let database = fixture(10_000, 300, 5);
    set_owned_weapon(&database, 7, 0);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("registered player");
    assert_eq!(shop.owned_pickaxe_tier, 7);
    assert!(shop.pickaxe_upgrades.is_empty());
}

fn assert_shop_player_state(depth: i64, tier: i64, expected_upgrade_tiers: &[u8]) {
    let database = fixture(10_000, depth, 5);
    set_owned_weapon(&database, tier, 20);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("registered player");
    assert!(shop.has_tunnel);
    assert_eq!(shop.owned_pickaxe_tier, tier as u8);
    assert_eq!(
        shop.pickaxe_upgrades
            .iter()
            .map(|upgrade| upgrade.tier)
            .collect::<Vec<_>>(),
        expected_upgrade_tiers
    );
    assert!(!shop.consumables.is_empty());
    assert!(shop.consumables.iter().all(|item| item.price > 0));
    assert!(
        shop.gear_for_sale.iter().all(|item| {
            item.price > 0 && item.depth_required >= 0 && item.prestige_required >= 0
        })
    );
}

#[test]
fn shop_succeeds_for_fresh_tier_zero_player_state() {
    assert_shop_player_state(0, 0, &[1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn shop_succeeds_for_mid_depth_tier_three_player_state() {
    assert_shop_player_state(80, 3, &[4, 5, 6, 7]);
}

#[test]
fn shop_succeeds_for_deep_tier_seven_player_state() {
    assert_shop_player_state(300, 7, &[]);
}

#[test]
fn buy_next_pickaxe_tier_succeeds() {
    let database = fixture(10_000, 30, 0);
    set_owned_weapon(&database, 0, 20);
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 1 },
            300,
        )
        .expect("buy");
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.slot, Some(GearSlot::Weapon));
}

#[test]
fn pickaxe_purchase_debits_exactly_and_updates_legacy_and_equipped_weapon() {
    let database = fixture(10_000, 25, 0);
    set_owned_weapon(&database, 0, 20);
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 1 },
            301,
        )
        .expect("buy");
    assert_eq!(outcome.cost, 15);
    assert_eq!(outcome.balance_after, 9_985);
    let connection = Connection::open(database.path()).expect("verify DB");
    let state = connection
        .query_row(
            "SELECT p.jopacoin_balance,t.pickaxe_tier,
                    (SELECT tier FROM dig_gear
                      WHERE discord_id=p.discord_id AND guild_id=p.guild_id
                        AND slot='weapon' AND equipped=1)
               FROM players p JOIN tunnels t
                 ON t.discord_id=p.discord_id AND t.guild_id=p.guild_id
              WHERE p.discord_id=?1 AND p.guild_id=?2",
            params![USER, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("purchase state");
    assert_eq!(state, (9_985, 1, 1));
}

#[test]
fn insufficient_pickaxe_funds_reject_without_any_state_change() {
    let database = fixture(14, 25, 0);
    set_owned_weapon(&database, 0, 20);
    let service = DigGearRuntimeService::sqlite(database.path());
    let before = service.panel(USER, GUILD).expect("before").unwrap();
    let outcome = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 1 },
            302,
        )
        .expect("buy");
    assert!(!outcome.success);
    assert_eq!(
        outcome.error.as_deref(),
        Some("Costs 15 JC but you only have 14 JC.")
    );
    assert_eq!(service.panel(USER, GUILD).expect("after").unwrap(), before);
}

#[test]
fn skip_pickaxe_tier_names_the_required_next_rung() {
    let database = fixture(10_000, 80, 0);
    set_owned_weapon(&database, 0, 20);
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 3 },
            303,
        )
        .expect("buy");
    assert!(!outcome.success);
    assert_eq!(
        outcome.error.as_deref(),
        Some("Buy Stone first — pickaxes upgrade one tier at a time.")
    );
}

#[test]
fn current_or_lower_pickaxe_tier_is_rejected() {
    let database = fixture(10_000, 80, 0);
    set_owned_weapon(&database, 2, 20);
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 2 },
            304,
        )
        .expect("buy");
    assert_eq!(
        outcome.error.as_deref(),
        Some("You already have that pickaxe tier or higher.")
    );
}

#[test]
fn broken_high_tier_pickaxe_cannot_be_replaced_by_lower_tier() {
    let database = fixture(10_000, 300, 5);
    set_owned_weapon(&database, 7, 0);
    let service = DigGearRuntimeService::sqlite(database.path());
    let before = service.panel(USER, GUILD).expect("before").unwrap();
    let outcome = service
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 1 },
            305,
        )
        .expect("buy");
    assert_eq!(
        outcome.error.as_deref(),
        Some("You already have that pickaxe tier or higher.")
    );
    assert_eq!(service.panel(USER, GUILD).expect("after").unwrap(), before);
}

#[test]
fn pickaxe_purchase_without_tunnel_is_rejected() {
    let database = NamedTempFile::new().expect("temporary DB");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(USER, "no tunnel", Some(GUILD)))
        .expect("player");
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 1 },
            306,
        )
        .expect("buy");
    assert_eq!(outcome.error.as_deref(), Some("You don't have a tunnel."));
}

#[test]
fn pickaxe_purchase_uses_all_time_depth_after_prestige_reset() {
    let database = fixture(10_000, 0, 1);
    set_owned_weapon(&database, 3, 20);
    Connection::open(database.path())
        .expect("max depth DB")
        .execute(
            "UPDATE tunnels SET max_depth=100 WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .expect("max depth");
    let outcome = DigGearRuntimeService::sqlite(database.path())
        .execute(
            USER,
            GUILD,
            DigGearRuntimeAction::BuyPickaxe { tier: 4 },
            307,
        )
        .expect("buy");
    assert!(outcome.success, "{outcome:?}");
}

#[test]
fn pickaxe_audit_failure_rolls_back_wallet_legacy_tier_and_equipped_row() {
    let database = fixture(10_000, 25, 0);
    set_owned_weapon(&database, 0, 20);
    Connection::open(database.path())
        .expect("trigger DB")
        .execute_batch(
            "CREATE TRIGGER fail_pickaxe_audit BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='pickaxe_buy'
             BEGIN SELECT RAISE(ABORT,'injected pickaxe audit failure'); END;",
        )
        .expect("failure trigger");
    let service = DigGearRuntimeService::sqlite(database.path());
    let before = service.panel(USER, GUILD).expect("before").unwrap();
    assert!(
        service
            .execute(
                USER,
                GUILD,
                DigGearRuntimeAction::BuyPickaxe { tier: 1 },
                308,
            )
            .is_err()
    );
    assert_eq!(service.panel(USER, GUILD).expect("after").unwrap(), before);
    assert_eq!(
        Connection::open(database.path())
            .expect("tunnel DB")
            .query_row(
                "SELECT pickaxe_tier FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("legacy tier"),
        0
    );
}

#[test]
fn concurrent_pickaxe_buys_admit_exactly_one_upgrade_and_debit() {
    let database = fixture(100, 25, 0);
    set_owned_weapon(&database, 0, 20);
    let path = database.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                DigGearRuntimeService::sqlite(path)
                    .execute(
                        USER,
                        GUILD,
                        DigGearRuntimeAction::BuyPickaxe { tier: 1 },
                        309 + i64::from(index),
                    )
                    .expect("concurrent buy")
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.success).count(), 1);
    let connection = Connection::open(database.path()).expect("verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        85
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 AND action_type='pickaxe_buy'",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("audit count"),
        1
    );
}

#[test]
fn test_buy_autocomplete_exposes_new_sinks_whetstone() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");
    let choices = shop_autocomplete(&shop, "whetstone");
    assert!(
        choices.iter().any(|choice| {
            choice.value == "tempered_whetstone" && !choice.name.trim().is_empty()
        })
    );
}

#[test]
fn test_buy_autocomplete_exposes_new_sinks_warding() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");
    let choices = shop_autocomplete(&shop, "warding");
    assert!(
        choices
            .iter()
            .any(|choice| { choice.value == "warding_salts" && !choice.name.trim().is_empty() })
    );
}

#[test]
fn test_buy_autocomplete_exposes_new_sinks_rescue() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");
    let choices = shop_autocomplete(&shop, "rescue");
    assert!(
        choices
            .iter()
            .any(|choice| { choice.value == "rescue_line" && !choice.name.trim().is_empty() })
    );
}

#[test]
fn test_buy_autocomplete_exposes_new_sinks_amulet() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");
    let values = shop_autocomplete(&shop, "amulet")
        .into_iter()
        .map(|choice| choice.value)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        values.is_superset(
            &(4..=7)
                .map(|tier| format!("amulet:{tier}"))
                .collect::<std::collections::BTreeSet<_>>()
        )
    );
}

#[test]
fn test_buy_autocomplete_exposes_new_sinks_boots() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");
    let values = shop_autocomplete(&shop, "boots")
        .into_iter()
        .map(|choice| choice.value)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        values.is_superset(
            &(4..=7)
                .map(|tier| format!("boots:{tier}"))
                .collect::<std::collections::BTreeSet<_>>()
        )
    );
}

#[test]
fn test_every_shop_row_is_reachable_through_filtered_autocomplete() {
    let database = fixture(500, 100, 1);
    let shop = DigGearRuntimeService::sqlite(database.path())
        .shop(USER, GUILD)
        .expect("shop")
        .expect("tunnel");

    for item in &shop.consumables {
        assert!(
            shop_autocomplete(&shop, &item.name)
                .iter()
                .any(|choice| choice.value == item.id),
            "missing consumable {}",
            item.id
        );
    }
    for item in &shop.pickaxe_upgrades {
        let value = format!("weapon:{}", item.tier);
        assert!(
            shop_autocomplete(&shop, &item.name)
                .iter()
                .any(|choice| choice.value == value),
            "missing pickaxe {value}"
        );
    }
    for item in &shop.gear_for_sale {
        let value = format!("{}:{}", item.slot.as_str(), item.tier);
        assert!(
            shop_autocomplete(&shop, &item.name)
                .iter()
                .any(|choice| choice.value == value),
            "missing gear {value}"
        );
    }
}
