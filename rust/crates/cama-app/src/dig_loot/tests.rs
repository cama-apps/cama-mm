use std::collections::{BTreeMap, BTreeSet};

use super::*;

const USER: i64 = 10_001;
const RECEIVER: i64 = 10_002;
const GUILD: i64 = 12_345;

type TestService = DigLootService<InMemoryLootRepository, ScriptedLootEntropy>;

fn service(balance: i64) -> TestService {
    let mut repository = InMemoryLootRepository::new();
    repository.register_player(USER, GUILD, balance);
    repository.create_tunnel(USER, GUILD);
    DigLootService::new(repository, ScriptedLootEntropy::default())
}

fn service_with_receiver(balance: i64) -> TestService {
    let mut result = service(balance);
    result
        .repository_mut()
        .register_player(RECEIVER, GUILD, 100);
    result.repository_mut().create_tunnel(RECEIVER, GUILD);
    result
}

fn set_tunnel(service: &mut TestService, mut update: impl FnMut(&mut TunnelLootState)) {
    let mut tunnel = service.repository().tunnel(USER, GUILD).expect("tunnel");
    update(&mut tunnel);
    service.repository_mut().set_tunnel(USER, GUILD, tunnel);
}

fn buy_and_queue(service: &mut TestService, item: &str) -> i64 {
    let bought = service.buy_item(USER, GUILD, item);
    assert!(bought.success, "{:?}", bought.error);
    let item_id = bought.item_id.expect("purchased inventory row");
    if !bought.queued {
        assert!(service.queue_item(USER, GUILD, item_id).success);
    }
    item_id
}

fn field<'a>(embed: &'a EmbedModel, name: &str) -> &'a str {
    &embed
        .fields
        .iter()
        .find(|field| field.name == name)
        .expect("expected embed field")
        .value
}

#[test]
fn test_buy_item_deducts_jc() {
    let mut service = service(100);
    let before = service.repository().balance(USER, GUILD);
    let result = service.buy_item(USER, GUILD, "dynamite");
    assert!(result.success);
    assert_eq!(before - service.repository().balance(USER, GUILD), 5);

    // The repository port makes debit + insert one commit. Exercise rollback
    // here as a stronger check without inflating the mapped test count.
    service
        .repository_mut()
        .fail_next_write(WriteFault::Purchase);
    let balance = service.repository().balance(USER, GUILD);
    let inventory = service.repository().inventory(USER, GUILD);
    assert!(!service.buy_item(USER, GUILD, "lantern").success);
    assert_eq!(service.repository().balance(USER, GUILD), balance);
    assert_eq!(service.repository().inventory(USER, GUILD), inventory);
}

#[test]
fn test_inventory_max() {
    let mut service = service(500);
    for _ in 0..MAX_INVENTORY_SLOTS {
        assert!(service.buy_item(USER, GUILD, "dynamite").success);
    }
    assert!(!service.buy_item(USER, GUILD, "dynamite").success);
    assert_eq!(
        service.repository().inventory(USER, GUILD).len(),
        MAX_INVENTORY_SLOTS
    );
}

#[test]
fn test_dynamite_adds_blocks() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 10);
    buy_and_queue(&mut service, "dynamite");
    service.entropy_mut().push_unit(0.99);
    service.entropy_mut().push_advance(1);
    service.entropy_mut().push_unit(0.99);
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert_eq!(result.dynamite_bonus, 5);
    assert_eq!(result.advance, 6);
}

#[test]
fn test_dynamite_bonus_is_flat_when_injured() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| {
        tunnel.depth = 10;
        tunnel.injured = true;
    });
    buy_and_queue(&mut service, "dynamite");
    service.entropy_mut().push_unit(0.99);
    service.entropy_mut().push_advance(4);
    service.entropy_mut().push_unit(0.99);
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert_eq!(result.dynamite_bonus, 5);
    assert!(result.advance >= 5, "flat consumable bonus was scaled");
}

#[test]
fn test_hard_hat_prevents_cave_in() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| {
        tunnel.depth = 20;
        tunnel.hard_hat_charges = HARD_HAT_USES;
    });
    service.entropy_mut().push_unit(0.001);
    service.entropy_mut().push_unit(0.99);
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert!(!result.cave_in);
    assert_eq!(
        service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .hard_hat_charges,
        HARD_HAT_USES - 1
    );
}

#[test]
fn test_hard_hat_queued_sets_charges() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 20);
    let bought = service.buy_item(USER, GUILD, "hard_hat");
    assert!(bought.success && bought.queued);
    service.entropy_mut().push_unit(0.001);
    service.entropy_mut().push_unit(0.99);
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert!(!result.cave_in);
    assert_eq!(
        service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .hard_hat_charges,
        HARD_HAT_USES - 1
    );
}

#[test]
fn test_lantern_reduces_cave_in() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 76);
    buy_and_queue(&mut service, "lantern");
    service.entropy_mut().push_unit(0.13);
    service.entropy_mut().push_unit(0.99);
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert!(!result.cave_in);
    assert!(result.has_lantern);
}

#[test]
fn test_reinforcement_sets_reinforced_until() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 20);
    buy_and_queue(&mut service, "reinforcement");
    let now = 1_003_601;
    let result = service.dig(USER, GUILD, now);
    assert!(result.success);
    assert_eq!(
        service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .reinforced_until,
        now + REINFORCEMENT_SECONDS
    );
}

#[test]
fn test_void_bait_doubles_event_chance() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 20);
    buy_and_queue(&mut service, "void_bait");
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert_eq!(
        service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .void_bait_digs,
        2
    );
}

#[test]
fn test_sonar_pulse_returns_event_preview() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 20);
    buy_and_queue(&mut service, "sonar_pulse");
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert!(result.event_preview_included);
    assert!(result.event_preview.is_some());
}

#[test]
fn test_sonar_pulse_auto_skips_next_event() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 20);
    buy_and_queue(&mut service, "sonar_pulse");
    assert!(service.dig(USER, GUILD, 1_003_601).success);
    assert!(
        service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .sonar_skip_pending
    );

    service.entropy_mut().push_unit(0.15);
    service.entropy_mut().push_unit(0.15);
    let result = service.dig(USER, GUILD, 1_007_202);
    assert!(result.success);
    assert!(!result.cave_in);
    assert!(result.event.is_none());
    assert!(result.sonar_skipped);
    assert!(
        !service
            .repository()
            .tunnel(USER, GUILD)
            .expect("tunnel")
            .sonar_skip_pending
    );
}

#[test]
fn test_queue_item_for_next_dig() {
    let mut service = service(200);
    let item_id = service
        .buy_item(USER, GUILD, "dynamite")
        .item_id
        .expect("item id");
    assert!(service.queue_item(USER, GUILD, item_id).success);
    assert_eq!(
        service
            .repository()
            .inventory(USER, GUILD)
            .iter()
            .filter(|item| item.queued)
            .count(),
        1
    );
    assert!(service.dig(USER, GUILD, 1_003_601).success);
    assert!(service.repository().inventory(USER, GUILD).is_empty());
}

#[test]
fn test_artifact_found_tracked() {
    let mut service = service(100);
    let id = service
        .add_artifact(USER, GUILD, "mole_claws")
        .expect("artifact insert");
    assert!(id > 0);
    let artifacts = service.repository().artifacts(USER, GUILD);
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].artifact_id, "mole_claws");
}

#[test]
fn test_gift_relic_transfers() {
    let mut service = service_with_receiver(100);
    let id = service
        .add_artifact(USER, GUILD, "mole_claws")
        .expect("artifact insert");
    let result = service.gift_relic(USER, RECEIVER, GUILD, ArtifactSelector::DatabaseId(id));
    assert!(result.success);
    assert!(service.repository().artifacts(USER, GUILD).is_empty());
    assert_eq!(
        service.repository().artifacts(RECEIVER, GUILD)[0].artifact_id,
        "mole_claws"
    );
}

#[test]
fn test_autocomplete_value_gifts_successfully() {
    let mut service = service_with_receiver(100);
    service
        .add_artifact(USER, GUILD, "mole_claws")
        .expect("artifact insert");
    let choices = service.relic_autocomplete(USER, GUILD);
    assert_eq!(choices[0].value, "mole_claws");
    let result = service.gift_relic(
        USER,
        RECEIVER,
        GUILD,
        ArtifactSelector::CatalogId(&choices[0].value),
    );
    let reply = gift_command_reply(&result, "Receiver");
    assert!(result.success);
    assert!(reply.content.contains("You gifted"));
    assert!(!reply.ephemeral);
}

#[test]
fn test_failed_gift_reports_error_not_success() {
    let mut service = service_with_receiver(100);
    service
        .add_artifact(USER, GUILD, "mole_claws")
        .expect("artifact insert");
    let result = service.gift_relic(
        USER,
        RECEIVER,
        GUILD,
        ArtifactSelector::CatalogId("Not An Owned Id"),
    );
    let reply = gift_command_reply(&result, "Receiver");
    assert!(!result.success);
    assert!(reply.ephemeral);
    assert!(!reply.content.contains("You gifted"));
    assert_eq!(service.repository().artifacts(USER, GUILD).len(), 1);
}

#[test]
fn test_failed_trap_reports_error() {
    let result = LootActionResult::error("No trap placed.");
    let reply = trap_command_reply(&result);
    assert!(reply.ephemeral);
    assert!(!reply.content.contains("Trap set"));
    assert!(reply.content.contains("No trap placed."));
}

#[test]
fn test_dig_result_includes_has_lantern() {
    let mut service = service(200);
    set_tunnel(&mut service, |tunnel| tunnel.depth = 23);
    buy_and_queue(&mut service, "lantern");
    let result = service.dig(USER, GUILD, 1_003_601);
    assert!(result.success);
    assert!(result.has_lantern);
}

#[test]
fn test_use_item_unknown_type_returns_error() {
    let mut service = service(100);
    let result = service.use_item(USER, GUILD, "Dynamite");
    assert!(!result.success);
    assert!(result.error.expect("error").contains("Unknown"));
}

#[test]
fn test_use_item_valid_type_succeeds() {
    let mut service = service(200);
    assert!(service.buy_item(USER, GUILD, "dynamite").success);
    assert!(service.use_item(USER, GUILD, "dynamite").success);
}

#[test]
fn test_event_pool_size_floor() {
    assert!(event_catalog().len() >= 116);
    assert_eq!(event_catalog().len(), 203);
}

#[test]
fn test_new_events_have_complexity_field() {
    let events = event_catalog();
    assert!(events.iter().all(|event| matches!(
        event.complexity,
        EventComplexity::Simple
            | EventComplexity::Choice
            | EventComplexity::Complex
            | EventComplexity::Boon
    )));
}

#[test]
fn test_darkness_events_exist() {
    assert!(
        event_catalog()
            .iter()
            .filter(|event| event.requires_dark)
            .count()
            >= 3
    );
}

#[test]
fn test_roll_event_filters_by_layer() {
    let repository = InMemoryLootRepository::new();
    let mut service = DigLootService::new(repository, SeededLootEntropy::new(42));
    for _ in 0..100 {
        let event = service
            .roll_event(5, 100, 0)
            .expect("eligible shallow event");
        assert!(event.min_depth <= 5);
        assert!(event.max_depth.is_none_or(|maximum| maximum >= 5));
        assert!(event.layer.is_none_or(|layer| layer == "Dirt"));
    }
}

#[test]
fn test_dota_hero_events_exist() {
    let expected = BTreeSet::from([
        "pudge_fishing",
        "tinker_workshop",
        "the_burrow",
        "arcanist_library",
        "the_dark_rift",
        "roshan_lair",
    ]);
    let ids = event_catalog()
        .iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert!(expected.is_subset(&ids));
}

#[test]
fn test_ten_consumables() {
    assert_eq!(CONSUMABLES.len(), 13);
    let ids = CONSUMABLES
        .iter()
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("torch"));
    assert!(ids.contains("void_bait"));
    assert!(ids.contains("streak_charm"));
}

#[test]
fn test_artifact_count() {
    assert_eq!(artifact_catalog().len(), 47);
}

#[test]
fn test_fungal_artifacts_exist() {
    assert!(
        artifact_catalog()
            .iter()
            .filter(|artifact| artifact.layer == "Fungal Depths")
            .count()
            >= 4
    );
}

#[test]
fn test_aegis_fragment_exists() {
    let artifact = artifact_catalog()
        .into_iter()
        .find(|artifact| artifact.id == "aegis_fragment")
        .expect("aegis fragment");
    assert!(artifact.is_relic);
}

#[test]
fn test_buy_item_torch() {
    let mut service = service(200);
    let result = service.buy_item(USER, GUILD, "torch");
    assert!(result.success);
    assert_eq!(result.cost, 6);
    assert!(result.queued);
    let queued = service
        .repository()
        .inventory(USER, GUILD)
        .into_iter()
        .filter(|item| item.queued)
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert_eq!(queued, vec![result.item_id.expect("item id")]);
}

#[test]
fn test_event_pool_has_desperate_options() {
    assert!(
        event_catalog()
            .iter()
            .any(|event| event.has_desperate_option)
    );
}

#[test]
fn test_event_pool_has_boon_options() {
    assert!(event_catalog().iter().any(|event| event.boon_count > 0));
}

#[test]
fn test_roll_event_filters_by_prestige() {
    let events = event_catalog();
    let gated_ids = events
        .iter()
        .filter(|event| event.min_prestige > 0)
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    let mut entropy = SeededLootEntropy::new(42);
    for _ in 0..500 {
        let event = roll_event_from_catalog(&events, 200, 100, 0, false, &mut entropy)
            .expect("eligible event");
        assert!(!gated_ids.contains(event.id));
    }
}

#[test]
fn test_all_new_events_have_valid_structure() {
    for event in event_catalog() {
        assert!(!event.id.is_empty());
        assert!(matches!(
            event.rarity,
            Rarity::Common | Rarity::Uncommon | Rarity::Rare | Rarity::Legendary
        ));
        assert!(event.has_safe_option || event.boon_count > 0);
    }
}

#[test]
fn test_resolve_event_desperate_choice() {
    let mut service = service(500);
    let event_id = service
        .events()
        .iter()
        .find(|event| event.has_desperate_option)
        .expect("desperate event")
        .id;
    let result = service.resolve_event(USER, GUILD, event_id, "desperate");
    assert!(result.success);
    assert_eq!(result.choice.as_deref(), Some("desperate"));
}

#[test]
fn test_resolve_event_boon_choice() {
    let mut service = service(500);
    let event_id = service
        .events()
        .iter()
        .find(|event| event.boon_count > 0)
        .expect("boon event")
        .id;
    let result = service.resolve_event(USER, GUILD, event_id, "boon_0");
    assert!(result.success);
    assert_eq!(result.choice.as_deref(), Some("boon_0"));
    assert!(result.buff_applied.is_some());
}

#[test]
fn test_resolve_event_boon_invalid_index() {
    let mut service = service(500);
    let event = *service
        .events()
        .iter()
        .find(|event| event.boon_count > 0)
        .expect("boon event");
    let result = service.resolve_event(
        USER,
        GUILD,
        event.id,
        &format!("boon_{}", event.boon_count + 10),
    );
    assert!(!result.success);
}

#[test]
fn test_streak_charm_save_is_visible() {
    let embed = build_dig_embed(&DigPresentation {
        depth: 10,
        advance: 1,
        streak_charm_used: true,
        ..DigPresentation::default()
    });
    assert!(field(&embed, "Streak Charm").contains("Saved your daily streak"));
}

#[test]
fn test_pet_excavation_bonus_is_visible_in_progress() {
    let embed = build_dig_embed(&DigPresentation {
        depth: 17,
        advance: 17,
        pet_dig_bonus: 12,
        pet_name: Some("Blep".to_owned()),
        ..DigPresentation::default()
    });
    assert!(field(&embed, "Progress").contains("Blep excavated +12 blocks"));
}

#[test]
fn test_auto_buy_outcomes_are_visible() {
    let embed = build_dig_embed(&DigPresentation {
        depth: 10,
        advance: 1,
        auto_purchases: vec![
            AutoPurchasePresentation {
                item: "Torch".to_owned(),
                status: AutoBuyStatus::Purchased,
                cost: 6,
            },
            AutoPurchasePresentation {
                item: "Hard Hat".to_owned(),
                status: AutoBuyStatus::SkippedInsufficientBalance,
                cost: 8,
            },
        ],
        ..DigPresentation::default()
    });
    let value = field(&embed, "Auto-Buy");
    assert!(value.contains("Torch: bought"));
    assert!(value.contains("Hard Hat: skipped (need 8 JC)"));
}

#[test]
fn test_cave_in_broken_gear_renders_repair_notification() {
    let embed = build_dig_embed(&DigPresentation {
        depth: 180,
        cave_in: true,
        broken_gear: vec!["Ironclad Armor".to_owned()],
        ..DigPresentation::default()
    });
    let value = field(&embed, "Gear Broken");
    assert!(value.contains("Ironclad Armor"));
    assert!(value.contains("effects disabled"));
    assert!(value.contains("Repair All"));
}

#[test]
fn test_every_non_boon_event_has_safe_option() {
    let offenders = event_catalog()
        .into_iter()
        .filter(|event| event.complexity != EventComplexity::Boon && !event.has_safe_option)
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert!(offenders.is_empty(), "{offenders:?}");
}

#[test]
fn test_rarity_weights_constant() {
    assert_eq!(
        BTreeMap::from([
            ("common", 70),
            ("uncommon", 20),
            ("rare", 15),
            ("legendary", 10),
        ]),
        RARITY_WEIGHTS
            .iter()
            .map(|(rarity, weight)| (rarity.as_str(), *weight))
            .collect()
    );
}

#[test]
fn test_prestige_gates_unlocked() {
    let unlocked = BTreeSet::from(["infernal_gate", "aghanim_trial", "neow_blessing"]);
    for event in event_catalog()
        .into_iter()
        .filter(|event| unlocked.contains(event.id))
    {
        assert_eq!(event.min_prestige, 0, "{}", event.id);
    }
}

#[test]
fn test_widened_depth_ranges() {
    let expected = BTreeMap::from([
        ("creeper_ambush", (0, 75)),
        ("abandoned_minecart", (0, 75)),
        ("villager_trade", (0, 80)),
        ("enderman_stare", (0, 80)),
        ("mob_spawner", (0, 75)),
        ("witch_cauldron", (0, 75)),
        ("azurite_deposit", (40, 120)),
        ("crawler_breakdown", (40, 120)),
        ("fossil_cache", (40, 120)),
        ("breach_encounter", (40, 170)),
        ("vaal_side_area", (40, 170)),
        ("syndicate_ambush", (40, 170)),
        ("delve_smuggler", (40, 170)),
        ("brann_bronzebeard", (130, 340)),
        ("earthen_cache", (130, 340)),
        ("campfire_rest", (130, 350)),
        ("zekvir_shadow", (130, 340)),
        ("dark_rider", (130, 340)),
        ("titan_relic", (130, 340)),
        ("candle_glow", (130, 340)),
    ]);
    let actual = event_catalog()
        .into_iter()
        .filter(|event| expected.contains_key(event.id))
        .map(|event| {
            (
                event.id,
                (event.min_depth, event.max_depth.expect("bounded")),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
}

#[test]
fn canonical_event_snapshot_is_complete_and_matches_route_projection() {
    let canonical = canonical_event_catalog();
    assert_eq!(canonical_event_snapshot_hash(), 0x62cf4f258ebca030);
    assert_eq!(canonical.len(), 203);
    assert_eq!(
        canonical.first().map(|event| event.id.as_str()),
        Some("underground_stream")
    );
    assert_eq!(
        canonical.last().map(|event| event.id.as_str()),
        Some("first_descent_cartography")
    );

    let routes = event_catalog();
    assert_eq!(routes.len(), canonical.len());
    for route in routes {
        let event = canonical_event(route.id).expect("route has canonical event");
        assert_eq!(
            event.min_depth.unwrap_or(0),
            route.min_depth,
            "{}",
            route.id
        );
        assert_eq!(event.max_depth, route.max_depth, "{}", route.id);
        assert_eq!(event.layer.as_deref(), route.layer, "{}", route.id);
        assert_eq!(event.rarity, route.rarity, "{}", route.id);
        assert_eq!(event.requires_dark, route.requires_dark, "{}", route.id);
        assert_eq!(event.min_prestige, route.min_prestige, "{}", route.id);
        assert_eq!(event.complexity, route.complexity, "{}", route.id);
        assert_eq!(
            event.safe_option.is_some(),
            route.has_safe_option,
            "{}",
            route.id
        );
        assert_eq!(
            event.desperate_option.is_some(),
            route.has_desperate_option,
            "{}",
            route.id
        );
        assert_eq!(event.boon_options.len(), route.boon_count, "{}", route.id);
        assert_eq!(event.chain_only, route.chain_only, "{}", route.id);
        assert_eq!(event.quest_id.is_some(), route.quest_event, "{}", route.id);
    }
}

#[test]
fn canonical_quest_snapshot_preserves_typed_finales() {
    assert_eq!(canonical_quest_snapshot_hash(), 0x6a5271037f7e49f3);
    let quests = canonical_quest_catalog();
    assert_eq!(quests.len(), 3);
    assert_eq!(quests[0].quest_id, "agh_lost_trial");
    assert_eq!(quests[0].step_event_ids.len(), 5);
    assert_eq!(quests[1].quest_id, "necropolis_below");
    assert_eq!(
        quests[2].starter_prereq.system_predicate.as_deref(),
        Some("bet_within_7d")
    );

    assert!(matches!(
        quests[0].finale,
        CanonicalQuestFinale::JcPlusGuildModifier {
            personal_jc: 75,
            ref modifier_id,
            duration_seconds: 1_800,
            ..
        } if modifier_id == "reagent_spill"
    ));
    assert!(matches!(
        quests[1].finale,
        CanonicalQuestFinale::RelicGrant {
            roll_count: 2,
            ref relic_base,
            ..
        } if relic_base == "Cloak of the Necropolis"
    ));
}

#[test]
fn canonical_quest_eligibility_matches_active_starter_and_predicate_rules() {
    let empty = BTreeSet::new();
    let starters = canonical_eligible_quest_event_ids(25, 2, None, None, &empty, &empty);
    assert_eq!(
        starters,
        BTreeSet::from(["agh_s1".to_owned(), "necro_s1".to_owned()])
    );

    let bet = BTreeSet::from(["bet_within_7d".to_owned()]);
    let all_starters = canonical_eligible_quest_event_ids(25, 2, None, None, &empty, &bet);
    assert_eq!(
        all_starters,
        BTreeSet::from([
            "agh_s1".to_owned(),
            "bolas_s1".to_owned(),
            "necro_s1".to_owned(),
        ])
    );

    let completed = BTreeSet::from(["agh_lost_trial".to_owned()]);
    let remaining = canonical_eligible_quest_event_ids(25, 2, None, None, &completed, &bet);
    assert!(!remaining.contains("agh_s1"));

    // Active quests expose only the current stage and do not reapply starter
    // depth/prestige/system gates.
    let active =
        canonical_eligible_quest_event_ids(0, 0, Some("agh_lost_trial"), Some(3), &empty, &empty);
    assert_eq!(active, BTreeSet::from(["agh_s3".to_owned()]));
    assert!(
        canonical_eligible_quest_event_ids(25, 2, Some("missing"), Some(1), &empty, &bet)
            .is_empty()
    );
}

#[test]
fn canonical_quest_progression_and_finale_resolution_are_typed() {
    let empty = BTreeSet::new();
    assert_eq!(
        advance_canonical_quest_on_desperate_success("agh_s1", None, None, &empty, 25, 0, &empty,),
        CanonicalQuestProgress::Activated {
            quest_id: "agh_lost_trial".to_owned(),
            next_step: 2,
        }
    );
    assert_eq!(
        advance_canonical_quest_on_desperate_success(
            "agh_s2",
            Some("agh_lost_trial"),
            Some(2),
            &empty,
            0,
            0,
            &empty,
        ),
        CanonicalQuestProgress::Advanced {
            quest_id: "agh_lost_trial".to_owned(),
            next_step: 3,
        }
    );
    assert_eq!(
        advance_canonical_quest_on_desperate_success(
            "agh_s5",
            Some("agh_lost_trial"),
            Some(5),
            &empty,
            0,
            0,
            &empty,
        ),
        CanonicalQuestProgress::Completed {
            quest_id: "agh_lost_trial".to_owned(),
            finale: canonical_quest_catalog()[0].finale.clone(),
        }
    );
    assert_eq!(
        advance_canonical_quest_on_desperate_success(
            "agh_s2",
            Some("agh_lost_trial"),
            Some(4),
            &empty,
            0,
            0,
            &empty,
        ),
        CanonicalQuestProgress::Noop
    );

    let agh = resolve_canonical_quest_finale(&canonical_quest_catalog()[0].finale, &[])
        .expect("Aghanim finale");
    assert_eq!(
        agh,
        CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
            personal_jc: 75,
            modifier_id: "reagent_spill".to_owned(),
            duration_seconds: 1_800,
            modifier_payload: serde_json::json!({"jc_event_bonus_pct": 25}),
        }
    );

    let necro = resolve_canonical_quest_finale(&canonical_quest_catalog()[1].finale, &[0.0, 0.99])
        .expect("Necropolis finale");
    assert_eq!(
        necro,
        CanonicalQuestFinaleOutcome::RelicGrant {
            relic_name: "Cloak of the Necropolis of Long Silence".to_owned(),
            artifact_id:
                "pinnacle:Cloak of the Necropolis:Long Silence:boss_hp_minus_10:streak_immunity"
                    .to_owned(),
            relic_stat_ids: vec!["boss_hp_minus_10".to_owned(), "streak_immunity".to_owned()],
        }
    );
}

#[test]
fn canonical_event_snapshot_preserves_all_authored_effects() {
    let event = canonical_event("spore_debt").expect("spore debt");
    let curse = event
        .risky_option
        .as_ref()
        .and_then(|choice| choice.failure.as_ref())
        .and_then(|outcome| outcome.curse.as_ref())
        .expect("spore debt curse");
    assert_eq!(curse.id, "spore_debt_curse");
    assert_eq!(curse.duration_digs, 3);
    assert_eq!(curse.effect["cave_in_bonus"], serde_json::json!(0.08));

    let boon = canonical_event("enchanting_table").expect("enchanting table");
    assert_eq!(boon.boon_options.len(), 3);
    assert_eq!(boon.boon_options[0].id, "efficiency");
    assert_eq!(
        boon.boon_options[1].effect["jc_bonus"],
        serde_json::json!(7)
    );

    let marquee = canonical_event("helltide_bell").expect("helltide bell");
    assert_eq!(
        marquee.guild_modifier_on_success,
        Some(serde_json::json!({
            "id": "helltide_active",
            "duration_seconds": 1800,
            "payload": {"tax_per_dig": 5}
        }))
    );

    let quest = canonical_event("agh_s1").expect("quest event");
    assert_eq!(quest.quest_id.as_deref(), Some("agh_lost_trial"));
    assert_eq!(quest.quest_step, Some(1));

    let rewarded = canonical_event("collapsed_armory_cache").expect("reward event");
    let outcome = &rewarded.risky_option.as_ref().expect("risky").success;
    assert_eq!(outcome.gear_reward_pool.len(), 8);
    assert_eq!(outcome.consumable_reward_pool, ["warding_salts"]);

    let mut outcomes = 0;
    let mut curses = 0;
    let mut streak_losses = 0;
    let mut gear_rewards = 0;
    let mut consumable_rewards = 0;
    let mut artifact_rewards = 0;
    for event in canonical_event_catalog() {
        for option in [
            event.safe_option.as_ref(),
            event.risky_option.as_ref(),
            event.desperate_option.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for outcome in [Some(&option.success), option.failure.as_ref()]
                .into_iter()
                .flatten()
            {
                outcomes += 1;
                curses += usize::from(outcome.curse.is_some());
                streak_losses += usize::from(outcome.streak_loss > 0);
                gear_rewards += usize::from(!outcome.gear_reward_pool.is_empty());
                consumable_rewards += usize::from(!outcome.consumable_reward_pool.is_empty());
                artifact_rewards += usize::from(!outcome.artifact_reward_pool.is_empty());
            }
        }
    }
    assert_eq!(outcomes, 691);
    assert_eq!(curses, 43);
    assert_eq!(streak_losses, 27);
    assert_eq!(gear_rewards, 10);
    assert_eq!(consumable_rewards, 16);
    assert_eq!(artifact_rewards, 4);
}

#[test]
fn canonical_event_resolution_returns_typed_success_failure_and_boon() {
    let success = resolve_canonical_event("gas_pocket", "risky", 0.0).expect("success");
    assert!(success.succeeded);
    assert_eq!(success.advance, 2);
    assert_eq!(success.jc, 10);
    assert_eq!(success.message, "The gas was harmless! And behind it—gems!");

    let failure = resolve_canonical_event("gas_pocket", "risky", 0.99).expect("failure");
    assert!(!failure.succeeded);
    assert!(failure.cave_in);
    assert_eq!(failure.message, "The gas ignites! BOOM!");

    let boon = resolve_canonical_event("enchanting_table", "boon_1", 0.0).expect("boon");
    assert_eq!(
        boon.buff.as_ref().map(|buff| buff.id.as_str()),
        Some("fortune")
    );
    assert_eq!(boon.message, "You chose Fortune!");
    assert!(resolve_canonical_event("gas_pocket", "boon_99", 0.0).is_err());
}

#[test]
fn canonical_policy_ports_darkness_cruel_echoes_scaling_and_curse_persistence() {
    let dark = resolve_canonical_event_with_policy(
        "gas_pocket",
        "safe",
        CanonicalEventRolls {
            success_roll: 0.99,
            ..CanonicalEventRolls::default()
        },
        CanonicalEventPolicy {
            luminosity: 0,
            ..CanonicalEventPolicy::default()
        },
    )
    .expect("pitch-black forced risky");
    assert_eq!(dark.choice, "risky");
    assert!(!dark.succeeded);
    assert!(dark.cave_in);

    let cruel = resolve_canonical_event_with_policy(
        "underground_stream",
        "safe",
        CanonicalEventRolls {
            // Python draws this before the ordinary safe success roll.
            success_roll: 0.99,
            cruel_echo_roll: Some(0.05),
            ..CanonicalEventRolls::default()
        },
        CanonicalEventPolicy {
            cruel_safe_failure: 0.10,
            ..CanonicalEventPolicy::default()
        },
    )
    .expect("cruel echoes");
    assert_eq!(cruel.advance, -1);
    assert_eq!(cruel.jc, -1);
    assert_eq!(
        cruel.message,
        "Cruel Echoes! Even safety betrays you. Lost 1 block and 1 JC."
    );

    let curse = resolve_canonical_event_with_policy(
        "spore_debt",
        "risky",
        CanonicalEventRolls {
            success_roll: 0.99,
            ..CanonicalEventRolls::default()
        },
        CanonicalEventPolicy::default(),
    )
    .expect("curse failure");
    let persisted = curse.persisted_curse.expect("persisted curse");
    assert_eq!(persisted.duration_digs, 5);
    assert_eq!(persisted.effect["cave_in_bonus"], serde_json::json!(0.12));

    let payout = resolve_canonical_event_with_policy(
        "gas_pocket",
        "risky",
        CanonicalEventRolls {
            success_roll: 0.0,
            ..CanonicalEventRolls::default()
        },
        CanonicalEventPolicy::default(),
    )
    .expect("positive economy scaling");
    assert_eq!(payout.jc, 7);
}

#[test]
fn canonical_policy_uses_python_rng_plan_and_bankers_rounding() {
    let policy = CanonicalEventPolicy {
        cruel_safe_failure: 0.10,
        ..CanonicalEventPolicy::default()
    };
    assert!(canonical_event_needs_cruel_echo_roll(
        "underground_stream",
        "safe",
        policy
    ));
    assert!(canonical_event_needs_cruel_echo_roll(
        "gas_pocket",
        "safe",
        policy
    ));
    assert!(!canonical_event_needs_cruel_echo_roll(
        "underground_stream",
        "risky",
        policy
    ));
    assert!(!canonical_event_needs_cruel_echo_roll(
        "underground_stream",
        "safe",
        CanonicalEventPolicy {
            luminosity: 0,
            ..policy
        }
    ));

    // Python's round ties to the nearest even integer for both signs.
    assert_eq!(round_i64(1.5), 2);
    assert_eq!(round_i64(2.5), 2);
    assert_eq!(round_i64(-1.5), -2);
    assert_eq!(round_i64(-2.5), -2);
    assert_eq!(round_i64(1.6), 2);
    assert_eq!(round_i64(-1.6), -2);
}

#[test]
fn event_settlement_commits_tunnel_and_jc_as_one_repository_write() {
    let mut committed_service = service(0);
    set_tunnel(&mut committed_service, |tunnel| tunnel.depth = 10);
    committed_service.entropy_mut().push_unit(0.0);
    let result = committed_service.resolve_event(USER, GUILD, "gas_pocket", "risky");
    assert!(result.success);
    assert_eq!(result.advance, 2);
    assert_eq!(result.jc_delta, 10);
    assert_eq!(committed_service.repository().balance(USER, GUILD), 10);
    assert_eq!(
        committed_service
            .repository()
            .tunnel(USER, GUILD)
            .unwrap()
            .depth,
        12
    );

    let mut failed_service = service(0);
    set_tunnel(&mut failed_service, |tunnel| tunnel.depth = 10);
    failed_service
        .repository_mut()
        .fail_next_write(WriteFault::Event);
    failed_service.entropy_mut().push_unit(0.0);
    let result = failed_service.resolve_event(USER, GUILD, "gas_pocket", "risky");
    assert!(!result.success);
    assert_eq!(failed_service.repository().balance(USER, GUILD), 0);
    assert_eq!(
        failed_service
            .repository()
            .tunnel(USER, GUILD)
            .unwrap()
            .depth,
        10
    );
}

#[test]
fn test_rare_share_in_expected_band() {
    let events = event_catalog();
    let eligible = eligible_events(&events, 30, 100, 0, false);
    let total_weight = eligible
        .iter()
        .map(|event| event.rarity.weight())
        .sum::<u64>();
    let rare_weight = eligible
        .iter()
        .filter(|event| event.rarity == Rarity::Rare)
        .map(|event| event.rarity.weight())
        .sum::<u64>();
    let expected = rare_weight as f64 / total_weight as f64;
    let mut entropy = SeededLootEntropy::new(20_260_412);
    let rare_hits = (0..5_000)
        .filter(|_| {
            roll_event_from_catalog(&events, 30, 100, 0, false, &mut entropy)
                .is_some_and(|event| event.rarity == Rarity::Rare)
        })
        .count();
    let share = rare_hits as f64 / 5_000.0;
    assert!((share - expected).abs() < 0.02, "{share} vs {expected}");
}

#[test]
fn test_three_p5_relics_defined() {
    let ids = artifact_catalog()
        .into_iter()
        .filter(|artifact| artifact.is_relic && artifact.min_prestige >= 5 && !artifact.trophy)
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from(["echo_lantern", "hollow_fang", "patient_stone"])
    );
}

#[test]
fn test_p5_relics_cover_each_axis() {
    let ids = artifact_catalog()
        .into_iter()
        .filter(|artifact| artifact.min_prestige >= 5)
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("hollow_fang"));
    assert!(ids.contains("echo_lantern"));
    assert!(ids.contains("patient_stone"));
}

#[test]
fn test_drop_returns_none_for_low_prestige() {
    let mut service = service(200);
    service.entropy_mut().push_unit(0.0);
    assert!(service.maybe_drop_prestige_relic(USER, GUILD, 1).is_none());
}

#[test]
fn test_drop_can_return_relic_at_p5() {
    let mut service = service(200);
    service.entropy_mut().push_unit(0.0);
    service.entropy_mut().push_unit(0.0);
    let result = service
        .maybe_drop_prestige_relic(USER, GUILD, 5)
        .expect("max-luck prestige drop");
    let eligible = artifact_catalog()
        .into_iter()
        .filter(|artifact| {
            artifact.is_relic
                && artifact.min_prestige > 0
                && artifact.min_prestige <= 5
                && !artifact.trophy
        })
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    assert!(eligible.contains(result.id));
    assert_eq!(service.repository().artifacts(USER, GUILD).len(), 1);
}

#[test]
fn test_drop_rate_gates_unlucky_roll() {
    let mut service = service(200);
    service.entropy_mut().push_unit(0.99);
    assert!(service.maybe_drop_prestige_relic(USER, GUILD, 10).is_none());
    assert!(service.repository().artifacts(USER, GUILD).is_empty());
}

fn non_jc_reward_outcome(
    gear: &[&str],
    consumables: &[&str],
    artifacts: &[&str],
) -> CanonicalEventOutcome {
    CanonicalEventOutcome {
        description: "Found".to_owned(),
        advance: 0,
        jc: 0,
        cave_in: false,
        streak_loss: 0,
        curse: None,
        gear_reward_pool: gear.iter().map(ToString::to_string).collect(),
        consumable_reward_pool: consumables.iter().map(ToString::to_string).collect(),
        artifact_reward_pool: artifacts.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn non_jc_new_encounter_catalog_has_nine_non_jc_events() {
    let ids = BTreeSet::from([
        "abandoned_forge",
        "salted_threshold",
        "frayed_lifeline",
        "quartermaster_s_niche",
        "collapsed_armory_cache",
        "relic_bearing_strata",
        "prospector_s_last_pack",
        "song_below_stone",
        "first_descent_cartography",
    ]);
    for event_id in ids {
        let event = canonical_event(event_id).expect("non-JC event");
        for outcome in [
            event.safe_option.as_ref(),
            event.risky_option.as_ref(),
            event.desperate_option.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|option| [Some(&option.success), option.failure.as_ref()])
        .flatten()
        {
            assert!(outcome.jc <= 0, "{event_id}");
        }
    }
}

#[test]
fn non_jc_reward_pool_validation_rejects_unknown_ids() {
    let mut event = canonical_event_catalog()[0].clone();
    event
        .safe_option
        .as_mut()
        .expect("safe option")
        .success
        .consumable_reward_pool = vec!["missing_supply".to_owned()];
    let error = validate_canonical_event_reward_pools(&[event]).expect_err("invalid pool");
    assert!(error.contains("missing_supply"));
}

#[test]
fn non_jc_lore_curios_are_unique_and_statless() {
    let catalog = artifact_catalog();
    for artifact_id in ["miner_s_lullaby", "map_of_the_first_descent"] {
        let artifact = catalog
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .expect("lore curio");
        assert_eq!(artifact.rarity, Rarity::Legendary);
        assert!(!artifact.is_relic);
        assert!(!artifact.trophy);
    }
}

#[test]
fn non_jc_owned_unique_reward_falls_back_to_unowned_artifact() {
    let outcome = non_jc_reward_outcome(&["glassbreaker_pick"], &[], &["miner_s_lullaby"]);
    let selection = select_canonical_reward(
        &outcome,
        &BTreeSet::from(["glassbreaker_pick".to_owned()]),
        &BTreeSet::new(),
        0,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("reward selection");
    assert_eq!(
        selection.reward,
        Some(CanonicalReward::Artifact("miner_s_lullaby".to_owned()))
    );
}

#[test]
fn non_jc_full_inventory_falls_back_from_consumable_to_artifact() {
    let outcome = non_jc_reward_outcome(&[], &["rescue_line"], &["miner_s_lullaby"]);
    let selection = select_canonical_reward(
        &outcome,
        &BTreeSet::new(),
        &BTreeSet::new(),
        MAX_INVENTORY_SLOTS,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("reward selection");
    assert!(selection.eligible_consumables.is_empty());
    assert_eq!(
        selection.reward,
        Some(CanonicalReward::Artifact("miner_s_lullaby".to_owned()))
    );
}
