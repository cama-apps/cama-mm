use std::collections::{BTreeMap, BTreeSet};

use super::*;

fn by_id(event_id: &str) -> EventDefinition {
    event_by_id(event_id).unwrap_or_else(|| panic!("missing event {event_id}"))
}

struct AuthoredExpectation<'a> {
    rarity: Rarity,
    min_depth: Option<i64>,
    max_depth: Option<i64>,
    layer: Option<&'a str>,
    safe_jc: i64,
    risky_chance: f64,
    success_jc: i64,
    failure_advance: i64,
    failure_jc: i64,
}

fn assert_authored(event_id: &str, expected: AuthoredExpectation<'_>) {
    let event = by_id(event_id);
    assert_eq!(event.rarity, expected.rarity);
    assert_eq!(event.min_depth, expected.min_depth);
    assert_eq!(event.max_depth, expected.max_depth);
    assert_eq!(event.layer, expected.layer);
    assert_eq!(event.safe_option.success.jc, expected.safe_jc);
    assert!(event.safe_option.failure.is_none());
    assert_eq!(event.safe_option.success_chance, 1.0);
    assert_eq!(event.risky_option.success_chance, expected.risky_chance);
    assert_eq!(event.risky_option.success.jc, expected.success_jc);
    let failure = event.risky_option.failure.unwrap();
    assert_eq!(failure.advance, expected.failure_advance);
    assert_eq!(failure.jc, expected.failure_jc);
}

#[derive(Clone, Debug)]
struct SequenceEntropy {
    values: Vec<f64>,
    cursor: usize,
}

impl SequenceEntropy {
    fn new(values: impl Into<Vec<f64>>) -> Self {
        Self {
            values: values.into(),
            cursor: 0,
        }
    }
}

impl EventEntropy for SequenceEntropy {
    fn unit_interval(&mut self) -> f64 {
        let value = self.values[self.cursor % self.values.len()];
        self.cursor += 1;
        value
    }
}

#[derive(Clone, Copy, Debug)]
struct LcgEntropy(u64);

impl EventEntropy for LcgEntropy {
    fn unit_interval(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / ((1_u64 << 53) as f64)
    }
}

#[derive(Debug)]
struct IndexEntropy {
    selected: usize,
    requested: Vec<usize>,
}

impl IndexEntropy {
    fn selecting(selected: usize) -> Self {
        Self {
            selected,
            requested: Vec::new(),
        }
    }
}

impl SplashEntropy for IndexEntropy {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.requested.push(upper_exclusive);
        self.selected % upper_exclusive
    }
}

#[test]
fn dig_pacing_v2_pick_splash_target_wealthiest() {
    let candidates = [
        SplashCandidate {
            discord_id: 1,
            balance: 100,
        },
        SplashCandidate {
            discord_id: 2,
            balance: 5_000,
        },
        SplashCandidate {
            discord_id: 3,
            balance: 200,
        },
    ];
    assert_eq!(select_wealthiest_target(&candidates, None), Some(2));
}

#[test]
fn dig_pacing_v2_pick_splash_target_excludes_caster() {
    let candidates = [
        SplashCandidate {
            discord_id: 1,
            balance: 5_000,
        },
        SplashCandidate {
            discord_id: 2,
            balance: 100,
        },
    ];
    assert_eq!(select_wealthiest_target(&candidates, Some(1)), Some(2));
}

#[test]
fn dig_pacing_v2_pick_splash_target_random_recent_weights_by_balance() {
    let candidates = [
        SplashCandidate {
            discord_id: 1,
            balance: 10_000,
        },
        SplashCandidate {
            discord_id: 2,
            balance: 100,
        },
        SplashCandidate {
            discord_id: 3,
            balance: 100,
        },
    ];
    let mut entropy = IndexEntropy::selecting(0);
    assert_eq!(
        select_random_recent_weighted(&candidates, None, &mut entropy),
        Some(1)
    );
    // ``random.choices`` receives [10000, 100, 100] in Python; the
    // equivalent cumulative domain has this exact total in Rust.
    assert_eq!(entropy.requested, vec![10_200]);
}

#[test]
fn dig_pacing_v2_select_deepest_n_orders_by_depth() {
    let candidates = [
        SplashDepthCandidate {
            discord_id: 1,
            depth: 10,
        },
        SplashDepthCandidate {
            discord_id: 2,
            depth: 200,
        },
        SplashDepthCandidate {
            discord_id: 3,
            depth: 50,
        },
    ];
    assert_eq!(select_deepest_n(&candidates, 999, 2), vec![2, 3]);
}

#[test]
fn test_all_new_events_in_random_events() {
    let ids = event_catalog()
        .into_iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert!(NEW_EVENT_IDS.iter().all(|event_id| ids.contains(event_id)));
}

#[test]
fn test_all_new_events_in_event_pool() {
    let ids = serialized_event_pool()
        .into_iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert!(NEW_EVENT_IDS.iter().all(|event_id| ids.contains(event_id)));
}

#[test]
fn test_all_new_events_have_multiple_descriptions() {
    assert!(
        NEW_EVENT_IDS
            .iter()
            .all(|event_id| by_id(event_id).descriptions.len() >= 2)
    );
}

#[test]
fn test_all_six_events_are_registered_once_and_serialized() {
    let catalog = event_catalog();
    let serialized = serialized_event_pool();
    for event_id in ROGUELIKE_EVENT_IDS {
        assert_eq!(
            catalog.iter().filter(|event| event.id == event_id).count(),
            1
        );
        assert_eq!(
            serialized
                .iter()
                .filter(|event| event.id == event_id)
                .count(),
            1
        );
    }
}

#[test]
fn test_authored_catalog_values_collapsed_armory() {
    assert_authored(
        "collapsed_armory",
        AuthoredExpectation {
            rarity: Rarity::Uncommon,
            min_depth: Some(76),
            max_depth: Some(150),
            layer: None,
            safe_jc: -6,
            risky_chance: 0.35,
            success_jc: 4,
            failure_advance: 0,
            failure_jc: -18,
        },
    );
}

#[test]
fn test_authored_catalog_values_dead_prospectors_pack() {
    assert_authored(
        "dead_prospectors_pack",
        AuthoredExpectation {
            rarity: Rarity::Uncommon,
            min_depth: Some(76),
            max_depth: Some(200),
            layer: None,
            safe_jc: -5,
            risky_chance: 0.40,
            success_jc: 3,
            failure_advance: -2,
            failure_jc: -16,
        },
    );
}

#[test]
fn test_authored_catalog_values_spore_debt() {
    assert_authored(
        "spore_debt",
        AuthoredExpectation {
            rarity: Rarity::Uncommon,
            min_depth: None,
            max_depth: None,
            layer: Some("Fungal Depths"),
            safe_jc: -4,
            risky_chance: 0.55,
            success_jc: 8,
            failure_advance: 0,
            failure_jc: -12,
        },
    );
}

#[test]
fn test_authored_catalog_values_clockwork_toll() {
    assert_authored(
        "clockwork_toll",
        AuthoredExpectation {
            rarity: Rarity::Rare,
            min_depth: None,
            max_depth: None,
            layer: Some("Frozen Core"),
            safe_jc: -5,
            risky_chance: 0.50,
            success_jc: 6,
            failure_advance: 0,
            failure_jc: -18,
        },
    );
}

#[test]
fn test_authored_catalog_values_collectors_roots() {
    assert_authored(
        "collectors_roots",
        AuthoredExpectation {
            rarity: Rarity::Uncommon,
            min_depth: Some(51),
            max_depth: None,
            layer: None,
            safe_jc: -4,
            risky_chance: 0.60,
            success_jc: 6,
            failure_advance: 0,
            failure_jc: -12,
        },
    );
}

#[test]
fn test_authored_catalog_values_hungry_beacon() {
    assert_authored(
        "hungry_beacon",
        AuthoredExpectation {
            rarity: Rarity::Rare,
            min_depth: Some(101),
            max_depth: None,
            layer: None,
            safe_jc: -6,
            risky_chance: 0.50,
            success_jc: 10,
            failure_advance: 0,
            failure_jc: -20,
        },
    );
}

#[test]
fn test_gear_reward_pools_are_slot_specific() {
    assert_eq!(
        by_id("collapsed_armory")
            .risky_option
            .success
            .gear_reward_pool,
        ["glassbreaker_pick", "needle_pick"]
    );
    assert_eq!(
        by_id("dead_prospectors_pack")
            .risky_option
            .success
            .gear_reward_pool,
        ["briarplate", "nullweave_mantle"]
    );
    assert_eq!(
        by_id("spore_debt").risky_option.success.gear_reward_pool,
        ["springheel_boots", "anchor_boots"]
    );
    assert_eq!(
        by_id("clockwork_toll")
            .risky_option
            .success
            .gear_reward_pool,
        ["loaded_die", "blood_locket"]
    );
}

#[test]
fn test_fractional_curses_keep_authored_values() {
    let spore = by_id("spore_debt")
        .risky_option
        .failure
        .unwrap()
        .curse
        .unwrap();
    let clock = by_id("clockwork_toll")
        .risky_option
        .failure
        .unwrap()
        .curse
        .unwrap();
    assert_eq!(spore.duration_digs, 3);
    assert_eq!(spore.effect, CurseEffect::CaveInBonus(0.08));
    assert_eq!(clock.duration_digs, 3);
    assert_eq!(clock.effect, CurseEffect::CooldownPenalty(0.20));
}

#[test]
fn test_success_burn_splashes_target_active_diggers_collectors_roots_2_8() {
    let event = by_id("collectors_roots");
    assert!(event.social);
    assert_eq!(
        event.splash,
        Some(SplashConfig {
            strategy: SplashStrategy::ActiveDiggers,
            victim_count: 2,
            penalty_jc: 8,
            trigger: SplashTrigger::Success,
            mode: SplashMode::Burn,
        })
    );
}

#[test]
fn test_success_burn_splashes_target_active_diggers_hungry_beacon_3_6() {
    let event = by_id("hungry_beacon");
    assert!(event.social);
    assert_eq!(
        event.splash,
        Some(SplashConfig {
            strategy: SplashStrategy::ActiveDiggers,
            victim_count: 3,
            penalty_jc: 6,
            trigger: SplashTrigger::Success,
            mode: SplashMode::Burn,
        })
    );
}

#[test]
fn test_mode_round_trips_through_splash_to_dict() {
    for mode in [SplashMode::Burn, SplashMode::Grant, SplashMode::Steal] {
        let result = SplashResult {
            strategy: SplashStrategy::RandomActive,
            event_name: "Mode Test".to_owned(),
            victims: vec![(99_001, 10)],
            total_burned: 10,
            mode,
        };
        assert_eq!(splash_to_payload(&result).mode, mode.as_str());
    }
}

#[test]
fn test_event_rates_bumped() {
    assert_eq!(event_rate("Dirt"), 0.22);
    assert_eq!(event_rate("The Hollow"), 0.45);
    assert_eq!(
        EVENT_RATES
            .iter()
            .filter(|(layer, _)| *layer == "Dirt")
            .count(),
        1
    );
    assert_eq!(
        EVENT_RATES
            .iter()
            .filter(|(layer, _)| *layer == "The Hollow")
            .count(),
        1
    );
}

#[test]
fn test_all_in_random_events() {
    let ids = event_catalog()
        .into_iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert!(RUMOR_PASS_IDS.iter().all(|event_id| ids.contains(event_id)));
}

#[test]
fn test_all_in_event_pool() {
    let ids = serialized_event_pool()
        .into_iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert!(RUMOR_PASS_IDS.iter().all(|event_id| ids.contains(event_id)));
}

#[test]
fn test_all_have_two_descriptions() {
    assert!(
        RUMOR_PASS_IDS
            .iter()
            .all(|event_id| by_id(event_id).descriptions.len() >= 2)
    );
}

#[test]
fn test_cross_player_events_have_splash() {
    for event_id in [
        "wisps_tether",
        "tunnel_echoes",
        "rivals_cache",
        "forsaken_pact",
        "mapworks_drift",
        "the_eye_opens",
    ] {
        let event = by_id(event_id);
        assert!(event.splash.is_some(), "{event_id}");
        assert!(event.social, "{event_id}");
    }
}

#[test]
fn test_solo_events_have_no_splash() {
    for event_id in ["stalled_caravan", "volatile_affix"] {
        assert!(by_id(event_id).splash.is_none(), "{event_id}");
    }
}

#[test]
fn test_rarity_distribution() {
    let mut counts = BTreeMap::new();
    for event_id in RUMOR_PASS_IDS {
        *counts.entry(by_id(event_id).rarity).or_insert(0) += 1;
    }
    assert_eq!(
        counts,
        BTreeMap::from([
            (Rarity::Common, 3),
            (Rarity::Uncommon, 3),
            (Rarity::Rare, 1),
            (Rarity::Legendary, 1),
        ])
    );
}

#[test]
fn test_eye_opens_has_desperate_option_and_dark_gate() {
    let event = by_id("the_eye_opens");
    assert!(event.desperate_option.is_some());
    assert!(event.requires_dark);
    assert!(event.buff_on_success.is_some());
}

#[test]
fn test_volatile_affix_grants_buff() {
    assert_eq!(
        by_id("volatile_affix").buff_on_success.map(|buff| buff.id),
        Some("affix_hum")
    );
}

#[test]
fn test_splash_modes_only_use_existing() {
    for event_id in RUMOR_PASS_IDS {
        let Some(splash) = by_id(event_id).splash else {
            continue;
        };
        assert!(matches!(
            splash.mode,
            SplashMode::Burn | SplashMode::Grant | SplashMode::Steal
        ));
        assert!(matches!(
            splash.strategy,
            SplashStrategy::RandomActive | SplashStrategy::RichestN | SplashStrategy::ActiveDiggers
        ));
    }
}

#[test]
fn test_chain_events_registered() {
    let serialized = serialized_event_pool();
    for event_id in HOLLOW_CHAIN_IDS {
        assert!(event_by_id(event_id).is_some());
        assert!(serialized.iter().any(|event| event.id == event_id));
    }
}

#[test]
fn test_chain_events_gated_to_p6() {
    assert!(
        HOLLOW_CHAIN_IDS
            .iter()
            .all(|event_id| by_id(event_id).min_prestige == 6)
    );
}

#[test]
fn test_chain_links_are_deterministic() {
    assert_eq!(
        by_id("hollow_court_overture").next_event_id,
        Some("hollow_court_audience")
    );
    assert_eq!(
        by_id("hollow_court_audience").next_event_id,
        Some("hollow_court_recess")
    );
    assert_eq!(by_id("hollow_court_recess").next_event_id, None);
}

#[test]
fn test_below_gate_player_does_not_chain_into_step2() {
    let result = chain_event(
        250,
        5,
        Rarity::Legendary,
        Some("hollow_court_overture"),
        &mut SequenceEntropy::new(vec![0.99]),
    );
    assert!(result.is_none());
}

#[test]
fn test_at_gate_player_chains_into_step2() {
    let result = chain_event(
        250,
        6,
        Rarity::Legendary,
        Some("hollow_court_overture"),
        &mut SequenceEntropy::new(vec![0.99]),
    )
    .unwrap();
    assert_eq!(result.event.id, "hollow_court_audience");
    assert!(result.chained);
}

#[test]
fn test_at_gate_player_chains_into_step3() {
    let result = chain_event(
        250,
        6,
        Rarity::Legendary,
        Some("hollow_court_audience"),
        &mut SequenceEntropy::new(vec![0.99]),
    )
    .unwrap();
    assert_eq!(result.event.id, "hollow_court_recess");
}

#[test]
fn test_chain_terminates_at_step3() {
    let result = chain_event(
        250,
        6,
        Rarity::Legendary,
        Some("hollow_court_recess"),
        &mut SequenceEntropy::new(vec![0.99]),
    );
    assert!(result.is_none());
}

#[test]
fn test_audience_and_recess_excluded_from_random_pool() {
    let eligible = eligible_random_events(EventContext {
        depth: 250,
        luminosity: 100,
        prestige_level: 6,
    });
    assert!(
        eligible
            .iter()
            .all(|event| !["hollow_court_audience", "hollow_court_recess"].contains(&event.id))
    );
    let mut entropy = LcgEntropy(0x5EED);
    for _ in 0..200 {
        let event = roll_event(
            EventContext {
                depth: 250,
                luminosity: 100,
                prestige_level: 6,
            },
            &mut entropy,
        )
        .unwrap();
        assert!(!["hollow_court_audience", "hollow_court_recess",].contains(&event.event.id));
    }
}
