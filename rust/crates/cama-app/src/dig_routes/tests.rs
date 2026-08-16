use std::cell::Cell;
use std::collections::BTreeSet;

use cama_db::dig_routes_repository::RouteTunnelRecord;

use super::*;
use crate::dig_tunnels::TunnelKey;

#[derive(Clone, Debug)]
struct SequenceEntropy {
    values: Vec<usize>,
    cursor: usize,
}

impl SequenceEntropy {
    fn new(values: impl Into<Vec<usize>>) -> Self {
        Self {
            values: values.into(),
            cursor: 0,
        }
    }
}

impl RouteEntropy for SequenceEntropy {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        assert!(upper_exclusive > 0);
        let value = self.values[self.cursor % self.values.len()];
        self.cursor += 1;
        value % upper_exclusive
    }
}

#[derive(Clone, Debug)]
struct LcgEntropy(u64);

impl RouteEntropy for LcgEntropy {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 as usize) % upper_exclusive
    }
}

fn pending_state() -> RouteState {
    RouteState {
        layer: "Stone".to_owned(),
        start_depth: 25,
        end_depth: 50,
        offered: [
            "shored_passage".to_owned(),
            "old_supports".to_owned(),
            "fossil_seam".to_owned(),
        ],
        selected: None,
    }
}

fn assert_offer_shape(layer: &str) {
    let mut entropy = SequenceEntropy::new(vec![1, 2, 0, 1, 0]);
    let offered = generate_route_offer(layer, None, &mut entropy).unwrap();
    let universal_ids: BTreeSet<_> = UNIVERSAL_ROUTES.iter().map(|route| route.id).collect();
    let themed_ids: BTreeSet<_> = LAYER_ROUTE_POOLS
        .iter()
        .find(|pool| pool.layer == layer)
        .unwrap()
        .routes
        .iter()
        .map(|route| route.id)
        .collect();
    assert_eq!(offered.into_iter().collect::<BTreeSet<_>>().len(), 3);
    assert_eq!(
        offered
            .iter()
            .filter(|route_id| universal_ids.contains(*route_id))
            .count(),
        1
    );
    assert_eq!(
        offered
            .iter()
            .filter(|route_id| themed_ids.contains(*route_id))
            .count(),
        2
    );
}

fn layer(name: &str) -> Layer {
    LAYERS
        .iter()
        .find(|layer| layer.name == name)
        .copied()
        .unwrap()
}

fn precondition_input(
    layer_name: &str,
    luminosity: i64,
    event_chance: f64,
) -> RoutePreconditionInput {
    RoutePreconditionInput {
        layer: layer(layer_name),
        luminosity,
        prestige_cave_in_multiplier: 0.9,
        base_event_chance: event_chance,
        thick_skin_saved: false,
        social_cave_in_bonus: 0.0,
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1.0e-12,
        "{actual} != {expected}"
    );
}

#[test]
fn test_route_catalog_has_four_universal_and_three_per_post_dirt_layer() {
    assert_eq!(UNIVERSAL_ROUTES.len(), 4);
    assert_eq!(LAYER_ROUTE_POOLS.len(), 7);
    assert!(LAYER_ROUTE_POOLS.iter().all(|pool| pool.routes.len() == 3));
    assert_eq!(route_catalog().len(), 25);
    assert_eq!(
        LAYER_ROUTE_POOLS
            .iter()
            .map(|pool| pool.layer)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "Stone",
            "Crystal",
            "Magma",
            "Abyss",
            "Fungal Depths",
            "Frozen Core",
            "The Hollow",
        ])
    );
}

#[test]
fn test_route_catalog_ids_are_unique_and_never_modify_jc_directly() {
    let routes = route_catalog();
    assert_eq!(
        routes
            .iter()
            .map(|route| route.id)
            .collect::<BTreeSet<_>>()
            .len(),
        routes.len()
    );
    assert!(
        routes
            .iter()
            .all(|route| route_by_id(route.id) == Some(*route))
    );
    assert!(routes.iter().all(|route| {
        route
            .effects
            .iter()
            .all(|effect| !effect.key.contains("jc"))
    }));
    assert!(routes.iter().all(|route| {
        route
            .effects
            .iter()
            .all(|effect| ROUTE_EFFECT_KEYS.contains(&effect.key))
    }));
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_stone() {
    assert_offer_shape("Stone");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_crystal() {
    assert_offer_shape("Crystal");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_magma() {
    assert_offer_shape("Magma");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_abyss() {
    assert_offer_shape("Abyss");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_fungal_depths() {
    assert_offer_shape("Fungal Depths");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_frozen_core() {
    assert_offer_shape("Frozen Core");
}

#[test]
fn test_route_offer_contains_one_universal_and_two_routes_for_layer_the_hollow() {
    assert_offer_shape("The Hollow");
}

#[test]
fn test_route_offer_excludes_previous_universal_route_when_possible() {
    for seed in 0..30 {
        let mut entropy = LcgEntropy(seed);
        let offered =
            generate_route_offer("Stone", Some(UNIVERSAL_ROUTES[0].id), &mut entropy).unwrap();
        assert!(!offered.contains(&UNIVERSAL_ROUTES[0].id));
    }
}

#[test]
fn test_route_offer_rejects_layer_without_a_route_pool() {
    let mut entropy = LcgEntropy(1);
    let error = generate_route_offer("Dirt", None, &mut entropy).unwrap_err();
    assert!(error.to_string().contains("No route pool for layer"));
}

#[test]
fn test_service_excludes_previous_offered_universal_when_themed_route_was_selected() {
    let mut previous = pending_state();
    previous.selected = Some("old_supports".to_owned());
    let mut entropy = LcgEntropy(42);
    let state = build_route_offer_state(Some(&previous.to_python_json()), 50, &mut entropy)
        .unwrap()
        .unwrap();
    assert_eq!(state.layer, "Crystal");
    assert_eq!(state.start_depth, 50);
    assert_eq!(state.end_depth, 75);
    assert_eq!(state.selected, None);
    assert!(!state.offered.iter().any(|route| route == "shored_passage"));
    assert_eq!(
        state
            .offered
            .iter()
            .filter_map(|route_id| route_by_id(route_id))
            .filter(|route| route.layer.is_none())
            .count(),
        1
    );
}

#[test]
fn test_service_does_not_build_routes_for_non_boss_or_pinnacle() {
    let mut entropy = LcgEntropy(1);
    assert_eq!(
        build_route_offer_state(None, 24, &mut entropy).unwrap(),
        None
    );
    assert_eq!(
        build_route_offer_state(None, PINNACLE_DEPTH, &mut entropy).unwrap(),
        None
    );
}

#[test]
fn test_service_selects_only_offered_route_once_and_returns_active_effects() {
    let pending_json = pending_state().to_python_json();
    assert!(matches!(
        evaluate_route_choice(Some(&pending_json), "lava_tube"),
        RouteChoiceEvaluation::Rejected(message) if message.contains("not offered")
    ));
    let RouteChoiceEvaluation::Select {
        route,
        selected_state,
    } = evaluate_route_choice(Some(&pending_json), "old_supports")
    else {
        panic!("offered route should be selectable");
    };
    assert_eq!(route.id, "old_supports");
    assert_eq!(route_effect(route, "cave_in_loss_cap"), Some(6.0));
    let selected_json = selected_state.to_python_json();
    assert!(matches!(
        evaluate_route_choice(Some(&selected_json), "old_supports"),
        RouteChoiceEvaluation::AlreadySelected { route } if route.id == "old_supports"
    ));
    assert!(matches!(
        evaluate_route_choice(Some(&selected_json), "fossil_seam"),
        RouteChoiceEvaluation::Rejected(message) if message.contains("already locked")
    ));
}

#[test]
fn test_route_choice_status_distinguishes_pending_and_active() {
    let pending = pending_state();
    let status = route_status(Some(&pending.to_python_json()));
    assert!(status.choice_required);
    assert_eq!(
        status
            .offered_routes
            .iter()
            .map(|route| route.id)
            .collect::<Vec<_>>(),
        ["shored_passage", "old_supports", "fossil_seam"]
    );
    assert_eq!(status.active_route, None);

    let mut active = pending;
    active.selected = Some("fossil_seam".to_owned());
    let status = route_status(Some(&active.to_python_json()));
    assert!(!status.choice_required);
    assert_eq!(
        status.active_route.map(|route| route.id),
        Some("fossil_seam")
    );
}

#[test]
fn test_pending_route_blocks_dig_before_cost_or_consumable_use_dig() {
    let pending_json = pending_state().to_python_json();
    let side_effects = Cell::new(0);
    let result = live_dig_after_route_gate(Some(&pending_json), || {
        side_effects.set(side_effects.get() + 1);
    });
    assert!(result.unwrap_err().route_choice_required);
    assert_eq!(side_effects.get(), 0);
}

#[test]
fn test_pending_route_blocks_dig_before_cost_or_consumable_use_compute_preconditions() {
    let pending_json = pending_state().to_python_json();
    let side_effects = Cell::new(0);
    let result = deterministic_preconditions_after_route_gate(Some(&pending_json), || {
        side_effects.set(side_effects.get() + 1);
    });
    assert!(result.unwrap_err().route_choice_required);
    assert_eq!(side_effects.get(), 0);
}

#[test]
fn test_abandon_clears_route_state() {
    let mut tunnel = Tunnel::new(TunnelKey::new(10_001, 12_345), "Route Test", 100);
    tunnel.route_state = Some(pending_state().to_python_json());
    clear_route_for_abandon(&mut tunnel);
    assert_eq!(tunnel.route_state, None);
}

#[test]
fn test_prestige_clears_route_state() {
    let mut tunnel = Tunnel::new(TunnelKey::new(10_001, 12_345), "Route Test", 100);
    tunnel.route_state = Some(pending_state().to_python_json());
    clear_route_for_prestige(&mut tunnel);
    assert_eq!(tunnel.route_state, None);
}

#[test]
fn test_route_loss_modifiers_add_before_applying_the_route_cap() {
    assert_eq!(apply_cave_in_loss_effects(8, 3, Some(7), &[]), 7);
    assert_eq!(apply_cave_in_loss_effects(8, 2, None, &[]), 10);
}

#[test]
fn test_route_loss_bonus_does_not_exceed_weather_cap_live() {
    let route = route_by_id("fractured_shortcut");
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 20,
            rolled_loss: 10,
            weather_loss_cap: Some(3),
            steady_hands_reduction: 0.0,
            reinforcement_active: false,
            catastrophic: false,
        },
        route,
    );
    assert_eq!(result.block_loss, 3);
}

#[test]
fn test_route_loss_bonus_does_not_exceed_weather_cap_deterministic() {
    let route = route_by_id("fractured_shortcut");
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 20,
            rolled_loss: 10,
            weather_loss_cap: Some(3),
            steady_hands_reduction: 0.0,
            reinforcement_active: false,
            catastrophic: false,
        },
        route,
    );
    assert_eq!(result.depth_after, 17);
}

#[test]
fn test_route_loss_bonus_does_not_exceed_weather_cap_applied() {
    assert_eq!(
        applied_outcome_route_cave_in_loss(10, route_by_id("fractured_shortcut"), Some(3), false,),
        3
    );
}

#[test]
fn test_route_loss_keeps_reinforcement_after_steady_hands_live() {
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 40,
            rolled_loss: 14,
            weather_loss_cap: None,
            steady_hands_reduction: 0.25,
            reinforcement_active: true,
            catastrophic: false,
        },
        route_by_id("fractured_shortcut"),
    );
    assert_eq!(result.block_loss, 8);
}

#[test]
fn test_route_loss_keeps_reinforcement_after_steady_hands_deterministic() {
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 40,
            rolled_loss: 14,
            weather_loss_cap: None,
            steady_hands_reduction: 0.25,
            reinforcement_active: true,
            catastrophic: false,
        },
        route_by_id("fractured_shortcut"),
    );
    assert_eq!(result.depth_after, 32);
}

#[test]
fn test_catastrophic_cave_in_respects_route_loss_cap_live() {
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 148,
            rolled_loss: 14,
            weather_loss_cap: None,
            steady_hands_reduction: 0.0,
            reinforcement_active: false,
            catastrophic: true,
        },
        route_by_id("old_supports"),
    );
    assert!(result.catastrophic);
    assert_eq!(result.block_loss, 6);
    assert_eq!(result.depth_after, 142);
}

#[test]
fn test_catastrophic_cave_in_respects_route_loss_cap_deterministic() {
    let result = resolve_route_cave_in(
        RouteCaveInInput {
            depth_before: 148,
            rolled_loss: 14,
            weather_loss_cap: None,
            steady_hands_reduction: 0.0,
            reinforcement_active: false,
            catastrophic: true,
        },
        route_by_id("old_supports"),
    );
    assert_eq!(
        result,
        RouteCaveInOutcome {
            block_loss: 6,
            depth_after: 142,
            catastrophic: true,
        }
    );
}

#[test]
fn test_preconditions_include_active_route_modifiers_in_both_dig_engines_old_supports_stone_30_expected0()
 {
    let input = precondition_input("Stone", 100, 0.22);
    let route = route_by_id("old_supports");
    let live = live_route_preconditions(input, route);
    let deterministic = deterministic_route_preconditions(input, route);
    assert_eq!(live, deterministic);
    assert_close(live.cave_in_chance, 0.045);
    assert_eq!((live.advance_min, live.advance_max), (1, 2));
    assert_eq!(live.luminosity, 100);
    assert_close(live.event_chance, 0.22);
    assert_close(live.artifact_multiplier, 1.0);
}

#[test]
fn test_preconditions_include_active_route_modifiers_in_both_dig_engines_sporelit_garden_fungal_depths_160_expected1()
 {
    let input = precondition_input("Fungal Depths", 100, 0.38);
    let route = route_by_id("sporelit_garden");
    let live = live_route_preconditions(input, route);
    let deterministic = deterministic_route_preconditions(input, route);
    assert_eq!(live, deterministic);
    assert_close(live.cave_in_chance, 0.405);
    assert_eq!((live.advance_min, live.advance_max), (1, 2));
    assert_eq!(live.luminosity, 99);
    assert_close(live.event_chance, 0.285);
    assert_close(live.artifact_multiplier, 1.25);
}

#[test]
fn test_boss_victory_conflict_returns_the_persisted_route() {
    let persisted = RouteTunnelRecord {
        discord_id: 10_001,
        guild_id: 12_345,
        depth: 25,
        boss_progress: Some("defeated".to_owned()),
        stinger_curse: Some(r#"{"drain_next_reward":true}"#.to_owned()),
        route_state: Some(pending_state().to_python_json()),
    };
    let claim = BossOutcomeClaim::Conflict { persisted };
    let side_effects = Cell::new(0);
    let conflict = after_successful_boss_claim(&claim, || side_effects.set(1)).unwrap_err();
    assert!(conflict.route_choice_required);
    assert_eq!(
        conflict
            .route_choice
            .unwrap()
            .offered_routes
            .iter()
            .map(|route| route.id)
            .collect::<Vec<_>>(),
        ["shored_passage", "old_supports", "fossil_seam"]
    );
    assert_eq!(side_effects.get(), 0);
}

#[test]
fn test_boss_loss_conflict_cannot_overwrite_victory_or_damage_gear() {
    let persisted = RouteTunnelRecord {
        discord_id: 10_001,
        guild_id: 12_345,
        depth: 25,
        boss_progress: Some("defeated".to_owned()),
        stinger_curse: None,
        route_state: Some(pending_state().to_python_json()),
    };
    let claim = BossOutcomeClaim::Conflict { persisted };
    let durability = Cell::new(100);
    let conflict = after_successful_boss_claim(&claim, || durability.set(90)).unwrap_err();
    assert!(conflict.route_choice_required);
    assert_eq!(durability.get(), 100);
}

#[test]
fn test_boss_conflict_copy_mentions_route_only_when_one_is_pending() {
    let without_route = RouteTunnelRecord {
        discord_id: 10_001,
        guild_id: 12_345,
        depth: 20,
        boss_progress: Some("active".to_owned()),
        stinger_curse: None,
        route_state: None,
    };
    let result = boss_resolution_conflict_result(Some(&without_route));
    assert!(!result.error.to_ascii_lowercase().contains("saved route"));

    let with_route = RouteTunnelRecord {
        route_state: Some(pending_state().to_python_json()),
        ..without_route
    };
    let result = boss_resolution_conflict_result(Some(&with_route));
    assert!(result.error.to_ascii_lowercase().contains("saved route"));
}

#[test]
fn test_thick_skin_still_forces_zero_cave_in_chance_with_risky_route() {
    let mut input = precondition_input("The Hollow", 100, 0.45);
    input.thick_skin_saved = true;
    input.social_cave_in_bonus = 0.09;
    let result = apply_route_preconditions(input, route_by_id("maws_shortcut"));
    assert!(result.thick_skin_saved);
    assert_eq!(result.cave_in_chance, 0.0);
}

#[test]
fn test_live_and_deterministic_resolvers_apply_route_artifact_multiplier_live() {
    let multiplier = route_artifact_multiplier(route_by_id("fossil_seam"));
    assert_close(multiplier, 1.75);
}

#[test]
fn test_live_and_deterministic_resolvers_apply_route_artifact_multiplier_deterministic() {
    let route = route_by_id("fossil_seam");
    let multiplier =
        deterministic_route_preconditions(precondition_input("Stone", 100, 0.22), route)
            .artifact_multiplier;
    assert_close(multiplier, 1.75);
}

#[test]
fn unmapped_route_state_parser_rejects_unknown_and_unoffered_selection() {
    let unknown = r#"{"end_depth":50,"layer":"Stone","offered":["unknown","old_supports","fossil_seam"],"selected":null,"start_depth":25}"#;
    assert_eq!(parse_route_state(Some(unknown)), None);
    let unoffered = r#"{"end_depth":50,"layer":"Stone","offered":["shored_passage","old_supports","fossil_seam"],"selected":"lava_tube","start_depth":25}"#;
    assert_eq!(parse_route_state(Some(unoffered)), None);
}
