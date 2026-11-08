use std::collections::BTreeSet;

use super::*;

/// Every route `route_by_id` can resolve, in catalog order.
fn all_routes() -> Vec<&'static DigRoute> {
    UNIVERSAL_ROUTES
        .iter()
        .chain(LAYER_ROUTE_POOLS.iter().flat_map(|pool| pool.routes.iter()))
        .collect()
}

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
    assert_eq!(all_routes().len(), 25);
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
    let routes = all_routes();
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
        build_route_offer_state(None, i64::from(PINNACLE_DEPTH), &mut entropy).unwrap(),
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
fn test_live_and_deterministic_resolvers_apply_route_artifact_multiplier_live() {
    let multiplier = route_artifact_multiplier(route_by_id("fossil_seam"));
    assert_close(multiplier, 1.75);
}

#[test]
fn unmapped_route_state_parser_rejects_unknown_and_unoffered_selection() {
    let unknown = r#"{"end_depth":50,"layer":"Stone","offered":["unknown","old_supports","fossil_seam"],"selected":null,"start_depth":25}"#;
    assert_eq!(parse_route_state(Some(unknown)), None);
    let unoffered = r#"{"end_depth":50,"layer":"Stone","offered":["shored_passage","old_supports","fossil_seam"],"selected":"lava_tube","start_depth":25}"#;
    assert_eq!(parse_route_state(Some(unoffered)), None);
}
