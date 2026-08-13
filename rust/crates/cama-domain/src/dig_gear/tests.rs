use super::*;

const PLAYER: i64 = 111;
const GUILD: i64 = 0;

struct FixedRng {
    roll: f64,
    slot: GearSlot,
    random_calls: usize,
    choice_calls: usize,
}

impl FixedRng {
    fn new(roll: f64, slot: GearSlot) -> Self {
        Self {
            roll,
            slot,
            random_calls: 0,
            choice_calls: 0,
        }
    }
}

impl GearRng for FixedRng {
    fn random(&mut self) -> f64 {
        self.random_calls += 1;
        self.roll
    }

    fn choose_slot(&mut self) -> GearSlot {
        self.choice_calls += 1;
        self.slot
    }
}

fn regular_piece(slot: GearSlot, tier: u8, durability: i32) -> GearPiece {
    GearPiece {
        id: 999,
        discord_id: PLAYER,
        guild_id: GUILD,
        slot,
        tier,
        durability,
        equipped: true,
        acquired_at: 1,
        source: "test".to_owned(),
        item_id: None,
    }
}

fn unique_piece(definition: &UniqueGearDefinition, durability: i32) -> GearPiece {
    GearPiece {
        id: 999,
        discord_id: PLAYER,
        guild_id: GUILD,
        slot: definition.slot,
        tier: definition.reference_tier,
        durability,
        equipped: true,
        acquired_at: 1,
        source: "event:test".to_owned(),
        item_id: Some(definition.item_id.to_owned()),
    }
}

fn assert_close(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-9, "{left} != {right}");
}

fn embed_field<'a>(embed: &'a GearEmbed, name: &str) -> &'a EmbedField {
    embed
        .fields
        .iter()
        .find(|field| field.name == name)
        .expect("field exists")
}

#[test]
fn test_drop_gate_returns_none_for_unmapped_boundary() {
    let mut service = GearService::fixture();
    let mut rng = FixedRng::new(0.0, GearSlot::Armor);
    for _ in 0..20 {
        assert_eq!(service.maybe_drop_gear(PLAYER, GUILD, 25, &mut rng), None);
    }
    assert_eq!(rng.random_calls, 0, "the depth gate precedes RNG");
}

#[test]
fn test_drop_gate_returns_none_when_roll_misses() {
    let mut service = GearService::fixture();
    let mut rng = FixedRng::new(GEAR_BOSS_DROP_RATE + 0.01, GearSlot::Armor);
    assert_eq!(service.maybe_drop_gear(PLAYER, GUILD, 100, &mut rng), None);
    assert_eq!((rng.random_calls, rng.choice_calls), (1, 0));
}

#[test]
fn test_drop_tier_each_mapped_boundary_drops_correct_tier() {
    let mut service = GearService::fixture();
    for (boundary, expected_tier) in [(100, 4), (150, 5), (200, 6), (275, 7)] {
        let mut rng = FixedRng::new(0.0, GearSlot::Boots);
        let drop = service
            .maybe_drop_gear(PLAYER, GUILD, boundary, &mut rng)
            .expect("forced drop");
        assert_eq!(drop.tier, expected_tier);
        assert_eq!(drop.slot, GearSlot::Boots);
        assert_eq!(drop.name, BOOTS_TIERS[usize::from(expected_tier)].name);
    }
}

#[test]
fn test_drop_persists_creates_dig_gear_row() {
    let mut service = GearService::fixture();
    let mut rng = FixedRng::new(0.0, GearSlot::Armor);
    let drop = service
        .maybe_drop_gear(PLAYER, GUILD, 100, &mut rng)
        .expect("forced drop");
    let row = service
        .store
        .gear_by_id(drop.gear_id)
        .expect("drop persisted");
    assert_eq!(row.slot, GearSlot::Armor);
    assert_eq!(row.source, "boss_drop");
    assert!(!row.equipped);
}

#[test]
fn test_starter_weapon_create_tunnel_adds_equipped_starter_weapon() {
    let mut store = GearStore::new();
    let starter_id = store.create_tunnel(PLAYER, GUILD);
    let weapon = store.equipped_gear(PLAYER, GUILD)[&GearSlot::Weapon];
    assert_eq!(weapon.id, starter_id);
    assert_eq!(weapon.tier, 0);
    assert_eq!(weapon.durability, GEAR_MAX_DURABILITY);
    assert_eq!(weapon.source, "starter");
}

#[test]
fn test_drop_rate_in_band_over_1000_rolls() {
    let mut service = GearService::fixture();
    let mut rng = LcgRng::new(42);
    let hits = (0..1_000)
        .filter(|_| {
            service
                .maybe_drop_gear(PLAYER, GUILD, 100, &mut rng)
                .is_some()
        })
        .count();
    let rate = hits as f64 / 1_000.0;
    assert!((rate - GEAR_BOSS_DROP_RATE).abs() < 0.02, "observed {rate}");
}

#[test]
fn test_repository_crud_add_then_get() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 2, None, "shop", None);
    let owned = store.gear(PLAYER, GUILD);
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].id, id);
    assert_eq!(owned[0].slot, GearSlot::Armor);
    assert_eq!(owned[0].tier, 2);
    assert!(!owned[0].equipped);
    assert_eq!(owned[0].source, "shop");
}

#[test]
fn test_repository_crud_add_uses_max_durability_by_default() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 1, None, "shop", None);
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 20);
}

#[test]
fn test_repository_crud_add_respects_explicit_durability() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 1, Some(7), "shop", None);
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 7);
}

#[test]
fn test_repository_crud_get_gear_orders_by_slot_then_tier_desc() {
    let mut store = GearStore::new();
    store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    store.add_gear(PLAYER, GUILD, GearSlot::Armor, 3, None, "shop", None);
    store.add_gear(PLAYER, GUILD, GearSlot::Boots, 2, None, "shop", None);
    let rows = store.gear(PLAYER, GUILD);
    assert_eq!(
        rows.iter().map(|row| row.slot).collect::<Vec<_>>(),
        [GearSlot::Armor, GearSlot::Armor, GearSlot::Boots]
    );
    assert_eq!(
        rows.iter().map(|row| row.tier).collect::<Vec<_>>(),
        [3, 1, 2]
    );
}

#[test]
fn test_repository_crud_get_gear_isolated_by_player_and_guild() {
    let mut store = GearStore::new();
    store.add_gear(111, 0, GearSlot::Armor, 1, None, "shop", None);
    store.add_gear(222, 0, GearSlot::Armor, 1, None, "shop", None);
    store.add_gear(111, 999, GearSlot::Armor, 1, None, "shop", None);
    assert_eq!(store.gear(111, 0).len(), 1);
    assert_eq!(store.gear(222, 0).len(), 1);
    assert_eq!(store.gear(111, 999).len(), 1);
}

#[test]
fn test_repository_equip_marks_one_piece_equipped() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Armor)
        .expect("equip");
    assert_eq!(store.equipped_gear(PLAYER, GUILD)[&GearSlot::Armor].id, id);
}

#[test]
fn test_repository_equipping_second_piece_swaps_first_atomically() {
    let mut store = GearStore::new();
    let first = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    let second = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 3, None, "shop", None);
    store
        .equip_gear(first, PLAYER, GUILD, GearSlot::Armor)
        .expect("first");
    store
        .equip_gear(second, PLAYER, GUILD, GearSlot::Armor)
        .expect("swap");
    assert_eq!(
        store.equipped_gear(PLAYER, GUILD)[&GearSlot::Armor].id,
        second
    );
    assert!(
        !store
            .gear_by_id(first)
            .expect("first remains owned")
            .equipped
    );
}

#[test]
fn test_repository_partial_unique_index_blocks_manual_double_equip() {
    let mut store = GearStore::new();
    let first = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 1, None, "shop", None);
    let second = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 2, None, "shop", None);
    store
        .equip_gear(first, PLAYER, GUILD, GearSlot::Boots)
        .expect("equip");
    assert_eq!(
        store.manual_set_equipped(second, true),
        Err(StoreError::UniqueEquippedSlot)
    );
    assert!(!store.gear_by_id(second).expect("second").equipped);
}

#[test]
fn test_repository_unequip_clears_equipped_flag() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Armor)
        .expect("equip");
    store.unequip_gear(id);
    assert!(store.equipped_gear(PLAYER, GUILD).is_empty());
}

#[test]
fn test_repository_get_equipped_gear_keys_by_slot() {
    let mut store = GearStore::new();
    for (slot, tier) in [
        (GearSlot::Armor, 1),
        (GearSlot::Boots, 2),
        (GearSlot::Weapon, 3),
    ] {
        let id = store.add_gear(PLAYER, GUILD, slot, tier, None, "shop", None);
        store.equip_gear(id, PLAYER, GUILD, slot).expect("equip");
    }
    assert_eq!(
        store
            .equipped_gear(PLAYER, GUILD)
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([GearSlot::Armor, GearSlot::Boots, GearSlot::Weapon])
    );
}

#[test]
fn test_repository_durability_tick_decrements_only_equipped() {
    let mut store = GearStore::new();
    let equipped = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    let spare = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 3, None, "shop", None);
    store
        .equip_gear(equipped, PLAYER, GUILD, GearSlot::Armor)
        .expect("equip");
    assert!(store.tick_gear_durability(PLAYER, GUILD).is_empty());
    assert_eq!(store.gear_by_id(equipped).expect("equipped").durability, 19);
    assert_eq!(store.gear_by_id(spare).expect("spare").durability, 20);
}

#[test]
fn test_repository_durability_tick_to_zero_returns_broken_ids_and_stays_equipped() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 2, Some(1), "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Boots)
        .expect("equip");
    assert_eq!(store.tick_gear_durability(PLAYER, GUILD), [id]);
    let broken = store.equipped_gear(PLAYER, GUILD)[&GearSlot::Boots];
    assert_eq!(broken.durability, 0);
    assert!(broken.equipped);
}

#[test]
fn test_repository_durability_tick_floors_at_zero() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 2, Some(1), "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Boots)
        .expect("equip");
    store.tick_gear_durability(PLAYER, GUILD);
    store.tick_gear_durability(PLAYER, GUILD);
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 0);
}

#[test]
fn test_repository_repair_resets_durability() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, Some(3), "shop", None);
    store.repair_gear(id, 20);
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 20);
}

#[test]
fn test_repository_repair_does_not_auto_equip() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, Some(0), "shop", None);
    store.repair_gear(id, 20);
    assert!(!store.gear_by_id(id).expect("piece").equipped);
}

#[test]
fn test_repository_tick_by_id_ticks_specific_ids_only() {
    let mut store = GearStore::new();
    let first = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    let second = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 2, None, "shop", None);
    let untouched = store.add_gear(PLAYER, GUILD, GearSlot::Weapon, 3, None, "shop", None);
    assert!(store.tick_gear_durability_ids(&[first, second]).is_empty());
    assert_eq!(store.gear_by_id(first).expect("first").durability, 19);
    assert_eq!(store.gear_by_id(second).expect("second").durability, 19);
    assert_eq!(store.gear_by_id(untouched).expect("third").durability, 20);
}

#[test]
fn test_repository_tick_by_id_ticks_unequipped_pieces_too() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    assert!(store.tick_gear_durability_ids(&[id]).is_empty());
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 19);
}

#[test]
fn test_repository_tick_by_id_breaks_at_zero_and_returns_id() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, Some(1), "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Armor)
        .expect("equip");
    assert_eq!(store.tick_gear_durability_ids(&[id]), [id]);
    let row = store.gear_by_id(id).expect("piece");
    assert_eq!(row.durability, 0);
    assert!(row.equipped);
}

#[test]
fn test_repository_tick_by_id_empty_list_is_a_noop() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, None, "shop", None);
    assert!(store.tick_gear_durability_ids(&[]).is_empty());
    assert_eq!(store.gear_by_id(id).expect("piece").durability, 20);
}

#[test]
fn test_service_equip_owned_armor() {
    let mut service = GearService::fixture();
    let bought = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("purchase transaction");
    let result = service.equip_gear(PLAYER, GUILD, bought.gear_id.expect("id"));
    assert!(result.success);
    assert_eq!(result.slot, Some(GearSlot::Armor));
}

#[test]
fn test_service_equip_rejects_broken() {
    let mut service = GearService::fixture();
    let id = service
        .store
        .add_gear(PLAYER, GUILD, GearSlot::Boots, 2, Some(0), "shop", None);
    let result = service.equip_gear(PLAYER, GUILD, id);
    assert!(!result.success);
    assert!(
        result
            .error
            .expect("error")
            .to_lowercase()
            .contains("broken")
    );
}

#[test]
fn test_service_equip_rejects_someone_elses_gear() {
    let mut service = GearService::fixture();
    let id = service
        .store
        .add_gear(222, GUILD, GearSlot::Armor, 1, None, "shop", None);
    let result = service.equip_gear(PLAYER, GUILD, id);
    assert!(!result.success);
    assert!(result.error.expect("error").contains("doesn't belong"));
}

#[test]
fn test_service_equip_already_equipped_is_a_no_op_error() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    assert!(service.equip_gear(PLAYER, GUILD, id).success);
    let again = service.equip_gear(PLAYER, GUILD, id);
    assert!(!again.success);
    assert!(again.error.expect("error").contains("already equipped"));
}

#[test]
fn test_service_unequip_works_for_owner() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    service.equip_gear(PLAYER, GUILD, id);
    assert!(service.unequip_gear(PLAYER, GUILD, id).success);
    assert!(
        !service
            .store
            .equipped_gear(PLAYER, GUILD)
            .contains_key(&GearSlot::Armor)
    );
}

#[test]
fn test_ring_loadout_contributes_authored_combat_tradeoff() {
    let loadout = GearLoadout {
        ring: Some(unique_piece(&RED_THREAD_BAND, 14)),
        ..GearLoadout::default()
    };
    assert_eq!(loadout.combat_modifiers().player_hp_bonus, -1);
}

#[test]
fn test_ring_loadout_broken_ring_contributes_no_combat_modifiers() {
    let loadout = GearLoadout {
        ring: Some(unique_piece(&RED_THREAD_BAND, 0)),
        ..GearLoadout::default()
    };
    assert_eq!(loadout.combat_modifiers().player_hp_bonus, 0);
}

#[test]
fn test_unique_gear_registry_matches_python_sidegrades() {
    let expected = [
        ("glassbreaker_pick", GearSlot::Weapon, 8),
        ("needle_pick", GearSlot::Weapon, 16),
        ("briarplate", GearSlot::Armor, 14),
        ("nullweave_mantle", GearSlot::Armor, 12),
        ("springheel_boots", GearSlot::Boots, 14),
        ("anchor_boots", GearSlot::Boots, 16),
        ("loaded_die", GearSlot::Amulet, 12),
        ("blood_locket", GearSlot::Amulet, 14),
        ("surveyors_loop", GearSlot::Ring, 14),
        ("ruinwager_signet", GearSlot::Ring, 14),
        ("red_thread_band", GearSlot::Ring, 14),
    ];
    for (item_id, slot, durability) in expected {
        let definition = unique_gear(item_id).expect("approved unique gear");
        assert_eq!(definition.item_id, item_id);
        assert_eq!(definition.slot, slot);
        assert_eq!(definition.reference_tier, 3);
        assert_eq!(definition.repair_value, 200);
        assert_eq!(definition.max_durability, durability);
        assert!(!definition.effect_summary.is_empty());
    }
    assert_eq!(
        unique_gear("briarplate")
            .expect("Briarplate is authored")
            .player_hp_bonus,
        1
    );
    assert_eq!(
        unique_gear("briarplate")
            .expect("Briarplate is authored")
            .effect_id,
        Some("reflect_first_hit")
    );
    assert_eq!(
        unique_gear("loaded_die")
            .expect("Loaded Die is authored")
            .crit_bonus,
        1
    );
    assert_close(
        unique_gear("loaded_die")
            .expect("Loaded Die is authored")
            .crit_chance,
        0.10,
    );
    assert_eq!(
        unique_gear("blood_locket")
            .expect("Blood Locket is authored")
            .effect_id,
        Some("heal_first_crit")
    );
    assert_eq!(
        unique_gear("ruinwager_signet")
            .expect("Ruinwager Signet is authored")
            .effect_id,
        Some("risky_event_edge")
    );
}

#[test]
fn test_service_repair_charges_ten_percent_prorated_by_damage() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.repair_gear(id, 5);
    let before = service.store.balance(PLAYER, GUILD);
    let result = service.repair_gear(PLAYER, GUILD, id);
    assert!(result.success);
    assert_eq!(result.cost, 14);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - 14);
    assert_eq!(service.store.gear_by_id(id).expect("piece").durability, 20);
}

#[test]
fn test_service_repair_refuses_when_full() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    let result = service.repair_gear(PLAYER, GUILD, id);
    assert!(!result.success);
    assert!(result.error.expect("error").contains("full durability"));
}

#[test]
fn test_service_repair_all_sums_costs() {
    let mut service = GearService::fixture();
    let armor = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("armor")
        .gear_id
        .expect("id");
    let boots = service
        .buy_gear(PLAYER, GUILD, "boots", 2)
        .expect("boots")
        .gear_id
        .expect("id");
    service.store.repair_gear(armor, 5);
    service.store.repair_gear(boots, 7);
    let before = service.store.balance(PLAYER, GUILD);
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert!(result.success);
    assert_eq!((result.repaired, result.cost), (2, 7));
    assert_eq!(service.store.balance(PLAYER, GUILD), before - 7);
}

#[test]
fn test_service_repair_all_batches_durability_updates() {
    let mut service = GearService::fixture();
    let mut ids = Vec::new();
    for slot in ["armor", "boots", "amulet"] {
        let id = service
            .buy_gear(PLAYER, GUILD, slot, 1)
            .expect("buy")
            .gear_id
            .expect("id");
        service.store.repair_gear(id, 1);
        ids.push(id);
    }
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert!(result.success);
    assert_eq!(service.store.bulk_repair_calls, 1);
    assert!(
        ids.into_iter()
            .all(|id| service.store.gear_by_id(id).expect("piece").durability == 20)
    );
}

#[test]
fn test_service_repair_all_with_nothing_damaged_errors() {
    let mut service = GearService::fixture();
    service.buy_gear(PLAYER, GUILD, "armor", 1).expect("buy");
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("Nothing to repair."));
}

#[test]
fn test_service_repair_all_repairs_damaged_unequipped_pieces() {
    let mut service = GearService::fixture();
    let armor = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    let boots = service
        .buy_gear(PLAYER, GUILD, "boots", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.repair_gear(armor, 5);
    service.store.repair_gear(boots, 5);
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert!(result.success);
    assert_eq!(result.repaired, 2);
    assert_eq!(
        service.store.gear_by_id(armor).expect("armor").durability,
        20
    );
    assert_eq!(
        service.store.gear_by_id(boots).expect("boots").durability,
        20
    );
}

#[test]
fn test_service_compute_repair_all_cost_matches_actual_debit() {
    let mut service = GearService::fixture();
    let armor = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    let boots = service
        .buy_gear(PLAYER, GUILD, "boots", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.repair_gear(armor, 5);
    service.store.repair_gear(boots, 5);
    let preview = service.compute_repair_all_cost(PLAYER, GUILD);
    let before = service.store.balance(PLAYER, GUILD);
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert_eq!(preview, result.cost);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - result.cost);
}

#[test]
fn test_service_relic_cap_at_prestige_zero_is_one_slot() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .prestige_level = 0;
    let first = service
        .store
        .add_artifact(PLAYER, GUILD, "mole_claws", true);
    let second = service
        .store
        .add_artifact(PLAYER, GUILD, "magma_heart", true);
    assert!(service.equip_relic(PLAYER, GUILD, first).success);
    let rejected = service.equip_relic(PLAYER, GUILD, second);
    assert!(!rejected.success);
    assert!(rejected.error.expect("error").contains("cap (1)"));
}

#[test]
fn test_service_relic_cap_prestige_two_allows_three_relics() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .prestige_level = 2;
    let ids = ["mole_claws", "magma_heart", "crystal_compass", "echo_stone"]
        .map(|id| service.store.add_artifact(PLAYER, GUILD, id, true));
    assert!(
        ids[..3]
            .iter()
            .all(|id| service.equip_relic(PLAYER, GUILD, *id).success)
    );
    let rejected = service.equip_relic(PLAYER, GUILD, ids[3]);
    assert!(!rejected.success);
    assert!(rejected.error.expect("error").contains("cap (3)"));
}

#[test]
fn test_service_relic_cap_unequip_then_re_equip_works() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .prestige_level = 0;
    let first = service
        .store
        .add_artifact(PLAYER, GUILD, "mole_claws", true);
    let second = service
        .store
        .add_artifact(PLAYER, GUILD, "magma_heart", true);
    service.equip_relic(PLAYER, GUILD, first);
    service.unequip_relic(PLAYER, GUILD, first);
    assert!(service.equip_relic(PLAYER, GUILD, second).success);
}

#[test]
fn test_service_relic_cap_ceiling_holds_at_six_for_high_prestige() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .prestige_level = 10;
    let ids = [
        "mole_claws",
        "magma_heart",
        "crystal_compass",
        "echo_stone",
        "spore_cloak",
        "frozen_clock",
        "root_network",
    ]
    .map(|id| service.store.add_artifact(PLAYER, GUILD, id, true));
    assert!(
        ids[..6]
            .iter()
            .all(|id| service.equip_relic(PLAYER, GUILD, *id).success)
    );
    let rejected = service.equip_relic(PLAYER, GUILD, ids[6]);
    assert!(!rejected.success);
    assert!(rejected.error.expect("error").contains("cap (6)"));
}

#[test]
fn test_relic_cap_rollout_get_loadout_exposes_relic_cap() {
    assert_eq!(GearService::relic_slot_cap(2), 3);
    assert_eq!(GearService::relic_slot_cap(20), 6);
}

#[test]
fn test_relic_cap_rollout_pop_relic_trim_notice_is_one_shot() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .relic_trim_notice = true;
    assert!(service.pop_relic_trim_notice(PLAYER, GUILD));
    assert!(!service.pop_relic_trim_notice(PLAYER, GUILD));
    assert!(
        !service
            .store
            .tunnel(PLAYER, GUILD)
            .expect("tunnel")
            .relic_trim_notice
    );
}

#[test]
fn test_relic_cap_rollout_prospectors_streak_increments_on_safe_dig() {
    let mut service = GearService::fixture();
    let relic = service
        .store
        .add_artifact(PLAYER, GUILD, "prospectors_streak", true);
    service.equip_relic(PLAYER, GUILD, relic);
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .cavein_free_streak = 4;
    service.apply_dig_outcome(PLAYER, GUILD, false);
    assert_eq!(
        service
            .store
            .tunnel(PLAYER, GUILD)
            .expect("tunnel")
            .cavein_free_streak,
        5
    );
}

#[test]
fn test_relic_cap_rollout_prospectors_streak_resets_on_cave_in() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .cavein_free_streak = 7;
    service.apply_dig_outcome(PLAYER, GUILD, true);
    assert_eq!(
        service
            .store
            .tunnel(PLAYER, GUILD)
            .expect("tunnel")
            .cavein_free_streak,
        0
    );
}

#[test]
fn test_relic_cap_rollout_dig_core_apply_outcome_increments_streak_on_safe_dig() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .cavein_free_streak = 4;
    service.apply_dig_outcome(PLAYER, GUILD, false);
    assert_eq!(
        service
            .store
            .tunnel(PLAYER, GUILD)
            .expect("tunnel")
            .cavein_free_streak,
        5
    );
}

#[test]
fn test_relic_cap_rollout_dig_core_apply_outcome_resets_streak_on_cave_in() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .cavein_free_streak = 7;
    service.apply_dig_outcome(PLAYER, GUILD, true);
    assert_eq!(
        service
            .store
            .tunnel(PLAYER, GUILD)
            .expect("tunnel")
            .cavein_free_streak,
        0
    );
}

#[test]
fn test_relic_cap_embed_over_cap_warning_renders() {
    let loadout = GearLoadout {
        relics: vec!["mole_claws".to_owned(); 8],
        ..GearLoadout::default()
    };
    let embed = build_gear_embed(&loadout, 6, 0, 0);
    let field = embed
        .fields
        .iter()
        .find(|field| field.name.starts_with("Relics"))
        .expect("relic field");
    assert!(field.name.contains("8/6"));
    assert!(field.value.contains("Over cap"));
    assert!(field.value.contains("unequip 2"));
}

#[test]
fn test_relic_cap_embed_unique_gear_effect_renders() {
    let loadout = GearLoadout {
        weapon: Some(unique_piece(&GLASSBREAKER_PICK, 8)),
        ..GearLoadout::default()
    };
    let embed = build_gear_embed(&loadout, 3, 0, 0);
    let weapon = embed_field(&embed, "Weapon");
    assert!(weapon.value.contains("+2 boss damage"));
    assert!(weapon.value.contains("-8% hit chance"));
}

#[test]
fn test_relic_cap_embed_ring_slot_renders_with_effect_and_durability() {
    let loadout = GearLoadout {
        ring: Some(unique_piece(&SURVEYORS_LOOP, 9)),
        ..GearLoadout::default()
    };
    let embed = build_gear_embed(&loadout, 3, 0, 0);
    let ring = embed_field(&embed, "Ring");
    assert!(ring.value.contains("Surveyor's Loop"));
    assert!(ring.value.contains("9/14"));
    assert!(ring.value.contains(
        "Safe event successes gain +1 block; risky and desperate choices lose 5% success chance."
    ));
}

#[test]
fn test_apply_gear_to_combat_empty_loadout_returns_unchanged_stats() {
    let base = CombatStats::bold();
    let result = apply_gear_to_combat(base, &GearLoadout::default());
    assert_eq!(result.player_hp, base.player_hp);
    assert_eq!(result.player_dmg, base.player_dmg);
    assert_close(result.player_hit, base.player_hit);
    assert_close(result.boss_hit, base.boss_hit);
}

#[test]
fn test_apply_gear_to_combat_full_loadout_applies_each_axis() {
    let base = CombatStats::bold();
    let loadout = GearLoadout {
        weapon: Some(regular_piece(GearSlot::Weapon, 7, 20)),
        armor: Some(regular_piece(GearSlot::Armor, 7, 20)),
        boots: Some(regular_piece(GearSlot::Boots, 7, 20)),
        ..GearLoadout::default()
    };
    let result = apply_gear_to_combat(base, &loadout);
    assert_eq!(result.player_dmg, base.player_dmg + 2);
    assert_eq!(result.player_hp, base.player_hp + 4);
    assert_close(result.player_hit, base.player_hit + 0.07);
    assert_close(result.boss_hit, base.boss_hit - 0.13);
}

#[test]
fn test_apply_gear_to_combat_drop_depth_map_uses_renumbered_tiers() {
    let actual = [100, 150, 200, 275].map(drop_tier_at_depth);
    assert_eq!(actual, [Some(4), Some(5), Some(6), Some(7)]);
    assert_eq!(drop_tier_at_depth(75), None);
}

#[test]
fn test_apply_gear_to_combat_armor_hp_progression_is_monotonic_and_meaningful() {
    assert_eq!(
        ARMOR_TIERS.each_ref().map(|tier| tier.player_hp_bonus),
        [0, 0, 1, 2, 3, 3, 3, 4]
    );
    assert!(ARMOR_TIERS[3].player_hp_bonus >= 2);
}

#[test]
fn test_apply_gear_to_combat_weapon_dmg_progression_pins_smoothed_curve() {
    assert_eq!(
        WEAPON_TIERS.each_ref().map(|tier| tier.player_dmg),
        [0, 0, 0, 1, 1, 1, 2, 2]
    );
    assert!(WEAPON_TIERS[3].player_dmg >= 1);
    assert!(WEAPON_TIERS[6].player_dmg >= 2);
}

#[test]
fn test_apply_gear_to_combat_player_hit_clamps_to_ceiling() {
    let base = CombatStats {
        player_hit: 0.99,
        ..CombatStats::bold()
    };
    let loadout = GearLoadout {
        weapon: Some(regular_piece(GearSlot::Weapon, 7, 20)),
        ..GearLoadout::default()
    };
    assert!(apply_gear_to_combat(base, &loadout).player_hit <= PLAYER_HIT_CEILING);
}

#[test]
fn test_apply_gear_to_combat_boss_hit_floors_at_005() {
    let base = CombatStats {
        boss_hit: 0.05,
        ..CombatStats::bold()
    };
    let loadout = GearLoadout {
        boots: Some(regular_piece(GearSlot::Boots, 7, 20)),
        ..GearLoadout::default()
    };
    assert!(apply_gear_to_combat(base, &loadout).boss_hit >= 0.05);
}

#[test]
fn test_amulet_tiers_table_has_eight_tiers_and_is_wired() {
    assert_eq!(AMULET_TIERS.len(), 8);
    assert!(std::ptr::eq(
        tier_table(GearSlot::Amulet).expect("amulet table"),
        &AMULET_TIERS
    ));
}

#[test]
fn test_amulet_tiers_crit_values_match_spec() {
    let chances = AMULET_TIERS.each_ref().map(|tier| tier.crit_chance);
    let bonuses = AMULET_TIERS.each_ref().map(|tier| tier.crit_bonus);
    assert_eq!(chances, [0.00, 0.02, 0.04, 0.06, 0.07, 0.08, 0.09, 0.10]);
    assert_eq!(bonuses, [0, 0, 0, 0, 1, 1, 1, 1]);
}

#[test]
fn test_amulet_tiers_shop_prices_are_monotonic() {
    let prices = AMULET_TIERS.each_ref().map(|tier| tier.shop_price);
    assert!(prices.windows(2).all(|window| window[0] <= window[1]));
    assert_eq!(prices[0], 0);
}

#[test]
fn test_amulet_tiers_high_tiers_are_prestige_gated() {
    assert_eq!(AMULET_TIERS[4].prestige_required, 1);
    assert_eq!(AMULET_TIERS[7].prestige_required, 5);
}

#[test]
fn test_amulet_combat_adds_crit_to_cautious_base() {
    let base = CombatStats::cautious();
    assert_close(base.crit_chance, 0.0);
    let loadout = GearLoadout {
        amulet: Some(regular_piece(GearSlot::Amulet, 7, 20)),
        ..GearLoadout::default()
    };
    let result = apply_gear_to_combat(base, &loadout);
    assert_close(result.crit_chance, 0.10);
    assert_eq!(result.crit_bonus, 1);
}

#[test]
fn test_amulet_combat_crit_stacks_additively_with_bold() {
    let base = CombatStats::bold();
    let loadout = GearLoadout {
        amulet: Some(regular_piece(GearSlot::Amulet, 7, 20)),
        ..GearLoadout::default()
    };
    let result = apply_gear_to_combat(base, &loadout);
    assert_close(result.crit_chance, base.crit_chance + 0.10);
    assert_eq!(result.crit_bonus, base.crit_bonus + 1);
}

#[test]
fn test_amulet_combat_no_amulet_leaves_risk_tier_crit_untouched() {
    let base = CombatStats::reckless();
    let result = apply_gear_to_combat(base, &GearLoadout::default());
    assert_close(result.crit_chance, base.crit_chance);
    assert_eq!(result.crit_bonus, base.crit_bonus);
}

#[test]
fn test_amulet_buy_and_equip_buy_low_tier_amulet_succeeds() {
    let mut service = GearService::fixture();
    let result = service
        .buy_gear(PLAYER, GUILD, "amulet", 1)
        .expect("transaction");
    assert!(result.success);
    assert!(
        service
            .store
            .gear(PLAYER, GUILD)
            .iter()
            .any(|piece| piece.slot == GearSlot::Amulet && piece.tier == 1)
    );
}

#[test]
fn test_amulet_buy_and_equip_equip_amulet_shows_in_loadout() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "amulet", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    service.equip_gear(PLAYER, GUILD, id);
    let amulet = service
        .loadout(PLAYER, GUILD)
        .amulet
        .expect("equipped amulet");
    assert_eq!(amulet.slot, GearSlot::Amulet);
}

#[test]
fn test_active_pickaxe_tier_falls_back_to_legacy_column() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .pickaxe_tier = 4;
    let weapon_id = service.store.equipped_gear(PLAYER, GUILD)[&GearSlot::Weapon].id;
    service.store.unequip_gear(weapon_id);
    assert_eq!(service.active_pickaxe_tier(PLAYER, GUILD), 4);
}

#[test]
fn test_active_pickaxe_tier_equipped_weapon_takes_priority() {
    let mut service = GearService::fixture();
    service
        .store
        .tunnel_mut(PLAYER, GUILD)
        .expect("tunnel")
        .pickaxe_tier = 2;
    let id = service
        .store
        .add_gear(PLAYER, GUILD, GearSlot::Weapon, 5, None, "shop", None);
    service
        .store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Weapon)
        .expect("equip");
    assert_eq!(service.active_pickaxe_tier(PLAYER, GUILD), 5);
}

#[test]
fn test_buy_gear_succeeds_with_depth_and_funds() {
    let mut service = GearService::fixture();
    let before = service.store.balance(PLAYER, GUILD);
    let result = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("transaction");
    assert!(result.success);
    assert_eq!(result.cost, 20);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - 20);
    assert!(
        service
            .store
            .gear(PLAYER, GUILD)
            .iter()
            .any(|piece| piece.slot == GearSlot::Armor && piece.tier == 1)
    );
}

#[test]
fn test_buy_gear_upper_tier_after_persistent_progression_gate() {
    let mut service = GearService::fixture();
    let tunnel = service.store.tunnel_mut(PLAYER, GUILD).expect("tunnel");
    tunnel.depth = 0;
    tunnel.max_depth = 150;
    tunnel.prestige_level = 2;
    let before = service.store.balance(PLAYER, GUILD);
    let result = service
        .buy_gear(PLAYER, GUILD, "armor", 5)
        .expect("transaction");
    assert!(result.success);
    assert_eq!(result.cost, 525);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - 525);
}

#[test]
fn test_buy_gear_upper_tier_still_requires_current_prestige() {
    let mut service = GearService::fixture();
    let tunnel = service.store.tunnel_mut(PLAYER, GUILD).expect("tunnel");
    tunnel.depth = 150;
    tunnel.max_depth = 150;
    tunnel.prestige_level = 1;
    let result = service
        .buy_gear(PLAYER, GUILD, "armor", 5)
        .expect("transaction");
    assert!(!result.success);
    assert!(
        result
            .error
            .expect("error")
            .to_lowercase()
            .contains("prestige 2")
    );
}

#[test]
fn test_buy_gear_refuses_when_underdepth() {
    let mut service = GearService::fixture();
    let tunnel = service.store.tunnel_mut(PLAYER, GUILD).expect("tunnel");
    tunnel.depth = 10;
    tunnel.max_depth = 10;
    let result = service
        .buy_gear(PLAYER, GUILD, "armor", 2)
        .expect("transaction");
    assert!(!result.success);
    assert!(
        result
            .error
            .expect("error")
            .to_lowercase()
            .contains("depth")
    );
}

#[test]
fn test_buy_gear_refuses_when_broke() {
    let mut service = GearService::fixture();
    service.store.set_balance(PLAYER, GUILD, 0);
    let result = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("transaction");
    assert!(!result.success);
    assert!(result.error.expect("error").contains("JC"));
}

#[test]
fn test_buy_gear_rolls_back_debit_when_gear_insert_fails() {
    let mut service = GearService::fixture();
    service.store.fail_next_shop_insert = true;
    let before = service.store.balance(PLAYER, GUILD);
    assert_eq!(
        service.buy_gear(PLAYER, GUILD, "armor", 1),
        Err(StoreError::ForcedInsertFailure)
    );
    assert_eq!(service.store.balance(PLAYER, GUILD), before);
    assert!(
        !service
            .store
            .gear(PLAYER, GUILD)
            .iter()
            .any(|piece| piece.slot == GearSlot::Armor)
    );
}

#[test]
fn test_atomic_debit_repair_succeeds_when_just_funded() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.set_balance(PLAYER, GUILD, 28);
    service.store.repair_gear(id, 5);
    let result = service.repair_gear(PLAYER, GUILD, id);
    assert!(result.success);
    assert_eq!(service.store.balance(PLAYER, GUILD), 14);
}

#[test]
fn test_atomic_debit_repair_does_not_charge_on_insufficient_balance() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.set_balance(PLAYER, GUILD, 5);
    service.store.repair_gear(id, 5);
    let result = service.repair_gear(PLAYER, GUILD, id);
    assert!(!result.success);
    assert_eq!(service.store.balance(PLAYER, GUILD), 5);
    assert_eq!(service.store.gear_by_id(id).expect("piece").durability, 5);
}

#[test]
fn test_try_debit_succeeds_when_funded() {
    let mut service = GearService::fixture();
    let before = service.store.balance(PLAYER, GUILD);
    assert!(service.store.try_debit(PLAYER, GUILD, 100));
    assert_eq!(service.store.balance(PLAYER, GUILD), before - 100);
}

#[test]
fn test_try_debit_fails_when_short_and_does_not_charge() {
    let mut service = GearService::fixture();
    let before = service.store.balance(PLAYER, GUILD);
    assert!(!service.store.try_debit(PLAYER, GUILD, before + 1));
    assert_eq!(service.store.balance(PLAYER, GUILD), before);
}

#[test]
fn test_try_debit_zero_amount_is_a_noop_success() {
    let mut service = GearService::fixture();
    let before = service.store.balance(PLAYER, GUILD);
    assert!(service.store.try_debit(PLAYER, GUILD, 0));
    assert_eq!(service.store.balance(PLAYER, GUILD), before);
}

#[test]
fn test_relic_label_plain_known_relic() {
    assert_eq!(format_relic_label("frozen_clock", false), "Frozen Clock");
}

#[test]
fn test_relic_label_plain_known_relic_with_stats_flag_unchanged() {
    assert_eq!(format_relic_label("frozen_clock", true), "Frozen Clock");
}

#[test]
fn test_relic_label_pinnacle_without_stats() {
    let id = "pinnacle:Pickaxe:Patience:boss_hit_minus:hp_plus_1";
    assert_eq!(
        format_relic_label(id, false),
        "Pickaxe of Feints and Scales"
    );
}

#[test]
fn test_relic_label_pinnacle_with_stats() {
    let id = "pinnacle:Pickaxe:Patience:boss_hit_minus:hp_plus_1";
    assert_eq!(
        format_relic_label(id, true),
        "Pickaxe of Feints and Scales (Bosses miss more often, Tougher skin)"
    );
}

#[test]
fn test_relic_label_unknown_plain_id_returns_raw() {
    assert_eq!(
        format_relic_label("not_a_real_relic", false),
        "not_a_real_relic"
    );
}

#[test]
fn test_relic_label_pinnacle_skips_unknown_stat_id() {
    let id = "pinnacle:Pickaxe:Patience:bogus_stat:hp_plus_1";
    assert_eq!(
        format_relic_label(id, true),
        "Pickaxe of Scales (Tougher skin)"
    );
}

#[test]
fn test_relic_label_pinnacle_legacy_suffix_rederived_from_stats() {
    let first = "pinnacle:Pickaxe:Patience:scout_free:durability_minus";
    let second = "pinnacle:Pickaxe:Patience:boss_hit_minus:hp_plus_1";
    assert_eq!(
        format_relic_label(first, false),
        "Pickaxe of Patience and Scouting"
    );
    assert_eq!(
        format_relic_label(second, false),
        "Pickaxe of Feints and Scales"
    );
    assert_ne!(
        format_relic_label(first, false),
        format_relic_label(second, false)
    );
}

#[test]
fn test_relic_label_pinnacle_name_is_order_independent() {
    let first = "pinnacle:Heart:Echoes:boss_payout_5:boss_hp_minus_10";
    let second = "pinnacle:Heart:Ruin:boss_hp_minus_10:boss_payout_5";
    assert_eq!(
        format_relic_label(first, false),
        "Heart of Tribute and Withering"
    );
    assert_eq!(
        format_relic_label(first, false),
        format_relic_label(second, false)
    );
}

#[test]
fn test_relic_label_pinnacle_quest_suffix_preserved() {
    let id = "pinnacle:Cloak of the Necropolis:Long Silence:boss_payout_5:jc_plus_5";
    assert_eq!(
        format_relic_label(id, false),
        "Cloak of the Necropolis of Long Silence"
    );
}

#[test]
fn test_relic_label_pinnacle_stat_titles_cover_pool() {
    let titles = PINNACLE_STATS.map(|(_, _, title)| title);
    assert!(titles.iter().all(|title| !title.is_empty()));
    assert_eq!(
        titles.into_iter().collect::<BTreeSet<_>>().len(),
        PINNACLE_STATS.len()
    );
}

#[test]
fn test_relic_label_quest_relic_suffixes_avoid_legacy_pool() {
    let pool = PINNACLE_RELIC_SUFFIX_POOL
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert!(
        QUEST_RELIC_SUFFIXES
            .into_iter()
            .all(|suffix| !pool.contains(suffix))
    );
}

#[test]
fn test_broken_gear_tick_to_zero_preserves_equipped_and_reports_transition_once() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Armor, 1, Some(1), "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Armor)
        .expect("equip");
    assert_eq!(store.tick_gear_durability(PLAYER, GUILD), [id]);
    assert!(store.gear_by_id(id).expect("piece").equipped);
    assert!(store.tick_gear_durability(PLAYER, GUILD).is_empty());
}

#[test]
fn test_broken_gear_snapshot_tick_to_zero_preserves_current_equipped_state() {
    let mut store = GearStore::new();
    let id = store.add_gear(PLAYER, GUILD, GearSlot::Boots, 1, Some(1), "shop", None);
    store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Boots)
        .expect("equip");
    assert_eq!(store.tick_gear_durability_ids(&[id]), [id]);
    assert!(store.gear_by_id(id).expect("piece").equipped);
    assert!(store.tick_gear_durability_ids(&[id]).is_empty());
}

#[test]
fn test_broken_gear_repair_all_reactivates_broken_piece_in_place() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 1)
        .expect("buy")
        .gear_id
        .expect("id");
    service.equip_gear(PLAYER, GUILD, id);
    service.store.repair_gear(id, 1);
    assert_eq!(service.store.tick_gear_durability(PLAYER, GUILD), [id]);
    assert!(service.repair_all_gear(PLAYER, GUILD).success);
    let armor = service.store.equipped_gear(PLAYER, GUILD)[&GearSlot::Armor];
    assert_eq!(armor.id, id);
    assert_eq!(armor.durability, 20);
}

#[test]
fn test_broken_gear_effects_piece_contributes_no_combat_modifiers() {
    let loadout = GearLoadout {
        armor: Some(regular_piece(GearSlot::Armor, 3, 0)),
        ..GearLoadout::default()
    };
    assert_eq!(loadout.combat_modifiers(), CombatModifiers::default());
}

#[test]
fn test_broken_gear_effects_weapon_has_no_active_pickaxe_effects() {
    let mut service = GearService::fixture();
    let starter = service.store.equipped_gear(PLAYER, GUILD)[&GearSlot::Weapon].id;
    service.store.unequip_gear(starter);
    let id = service
        .store
        .add_gear(PLAYER, GUILD, GearSlot::Weapon, 7, Some(0), "shop", None);
    service
        .store
        .equip_gear(id, PLAYER, GUILD, GearSlot::Weapon)
        .expect("equip");
    assert_eq!(service.active_pickaxe_tier(PLAYER, GUILD), 0);
    assert_eq!(service.active_pickaxe_data(PLAYER, GUILD), None);
}

#[test]
fn test_prorated_repair_full_repair_value_is_ten_percent() {
    assert_close(GEAR_REPAIR_COST_PCT, 0.10);
}

#[test]
fn test_prorated_repair_single_repair_prorates_cost_by_missing_durability() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.repair_gear(id, 10);
    let preview = service.compute_repair_cost(GearSlot::Armor, 3, None, 10, Some(20));
    let before = service.store.balance(PLAYER, GUILD);
    let result = service.repair_gear(PLAYER, GUILD, id);
    assert_eq!(preview, 9);
    assert_eq!(result.cost, preview);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - preview);
}

#[test]
fn test_prorated_repair_all_preview_matches_prorated_debit() {
    let mut service = GearService::fixture();
    let armor = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    let boots = service
        .buy_gear(PLAYER, GUILD, "boots", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.store.repair_gear(armor, 10);
    service.store.repair_gear(boots, 15);
    let preview = service.compute_repair_all_cost(PLAYER, GUILD);
    let before = service.store.balance(PLAYER, GUILD);
    let result = service.repair_all_gear(PLAYER, GUILD);
    assert_eq!(preview, 14);
    assert_eq!(result.cost, preview);
    assert_eq!(service.store.balance(PLAYER, GUILD), before - preview);
}

#[test]
fn test_prorated_repair_broken_loadout_marks_effects_disabled() {
    let mut service = GearService::fixture();
    let id = service
        .buy_gear(PLAYER, GUILD, "armor", 3)
        .expect("buy")
        .gear_id
        .expect("id");
    service.equip_gear(PLAYER, GUILD, id);
    service.store.repair_gear(id, 0);
    let loadout = service.loadout(PLAYER, GUILD);
    let embed = build_gear_embed(&loadout, 2, service.store.gear(PLAYER, GUILD).len(), 1);
    let armor = embed_field(&embed, "Armor");
    assert!(armor.value.contains("BROKEN"));
    assert!(armor.value.contains("effects disabled"));
}
