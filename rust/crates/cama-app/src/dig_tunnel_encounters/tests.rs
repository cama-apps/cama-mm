use std::cell::RefCell;
use std::convert::Infallible;

use cama_domain::economy_scaling::DEFAULT_MINIGAME_JC_DELTA_SCALE;

use crate::dig_loot::ScriptedLootEntropy;

use super::*;

#[derive(Debug)]
struct FakePersistence {
    candidates: Vec<i64>,
    candidate_query: RefCell<Option<EncounterCandidateQuery>>,
    settlement_request: RefCell<Option<EncounterSettlementRequest>>,
}

impl Default for FakePersistence {
    fn default() -> Self {
        Self {
            candidates: vec![2, 3, 4, 5],
            candidate_query: RefCell::new(None),
            settlement_request: RefCell::new(None),
        }
    }
}

impl TunnelEncounterPersistence for FakePersistence {
    type Error = Infallible;

    fn candidate_ids(&self, query: EncounterCandidateQuery) -> Result<Vec<i64>, Self::Error> {
        *self.candidate_query.borrow_mut() = Some(query);
        Ok(self.candidates.clone())
    }

    fn settle_encounter(
        &self,
        request: &EncounterSettlementRequest,
    ) -> Result<EncounterSettlement, Self::Error> {
        *self.settlement_request.borrow_mut() = Some(request.clone());
        Ok(EncounterSettlement {
            succeeded: matches!(request.outcome, EncounterOutcome::Success { .. }),
            actor_delta: 0,
            actor_balance: 50,
            splash: None,
            applied_now: true,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct ExpectedEncounter {
    id: &'static str,
    rarity: Rarity,
    min_depth: i64,
    strategy: EncounterVictimStrategy,
    victim_count: usize,
    penalty_jc: i64,
    payout: i64,
}

fn assert_present(expected: ExpectedEncounter) {
    let event = encounter(expected.id).expect("authored encounter is present");
    assert_eq!(event.id, expected.id);
    assert!(event.eligible_at_depth(expected.min_depth));
    assert!(!event.eligible_at_depth(expected.min_depth - 1));
    assert!(event.max_depth.is_none());
}

fn assert_success_burn(expected: ExpectedEncounter) {
    let event = encounter(expected.id).unwrap();
    assert!(event.social);
    assert_eq!(event.splash.trigger, SplashTrigger::Success);
    assert_eq!(event.splash.mode, SplashMode::Burn);
    assert!(matches!(
        event.splash.strategy,
        EncounterVictimStrategy::RichestN
            | EncounterVictimStrategy::ActiveDiggers
            | EncounterVictimStrategy::RandomActive
            | EncounterVictimStrategy::DeepestN
    ));
}

fn assert_burn_dominates(expected: ExpectedEncounter) {
    let event = encounter(expected.id).unwrap();
    assert!(event.burn_dominates_maximum_payout());
    assert!(
        event.splash.victim_count as f64 * event.splash.penalty_jc as f64
            >= event.risky_success_jc as f64 * 1.5
    );
}

fn assert_authored(expected: ExpectedEncounter) {
    let event = encounter(expected.id).unwrap();
    assert_eq!(event.rarity, expected.rarity);
    assert_eq!(event.min_depth, expected.min_depth);
    assert_eq!(event.splash.strategy, expected.strategy);
    assert_eq!(event.splash.victim_count, expected.victim_count);
    assert_eq!(event.splash.penalty_jc, expected.penalty_jc);
    assert_eq!(event.risky_success_jc, expected.payout);

    let persistence = FakePersistence::default();
    let mut entropy = ScriptedLootEntropy::new([0.01, 0.5, 0.0, 0.0, 0.0], []);
    let resolution = resolve_risky_tunnel_encounter(
        &persistence,
        ResolveTunnelEncounterRequest {
            digger_discord_id: 1,
            guild_id: Some(123),
            event_id: expected.id,
            event_key: "authored-catalog-test",
            depth: 120,
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            now: 2_000_000_000,
        },
        &mut entropy,
    )
    .unwrap();
    assert_eq!(resolution.event_id, expected.id);
    assert!(resolution.settlement.succeeded);

    let query = persistence
        .candidate_query
        .borrow()
        .expect("candidate query was issued");
    assert_eq!(query.guild_id, Some(123));
    assert_eq!(query.strategy, expected.strategy);
    let request = persistence.settlement_request.borrow();
    let request = request.as_ref().expect("settlement was issued");
    assert_eq!(request.event_id, expected.id);
    match &request.outcome {
        EncounterOutcome::Success {
            strategy,
            victim_count,
            authored_penalty_jc,
            jittered_authored_reward_jc,
            preferred_victim_ids,
        } => {
            assert_eq!(*strategy, expected.strategy);
            assert_eq!(*victim_count, expected.victim_count);
            assert_eq!(*authored_penalty_jc, expected.penalty_jc);
            assert_eq!(*jittered_authored_reward_jc, expected.payout);
            if strategy.uses_random_sample() {
                assert_eq!(preferred_victim_ids.len(), expected.victim_count);
            } else {
                assert!(preferred_victim_ids.is_empty());
            }
        }
        EncounterOutcome::Failure { .. } => panic!("scripted success resolved as failure"),
    }
}

macro_rules! encounter_case {
    (
        $present:ident,
        $splash:ident,
        $dominates:ident,
        $authored:ident,
        $expected:expr
    ) => {
        #[test]
        fn $present() {
            assert_present($expected);
        }

        #[test]
        fn $splash() {
            assert_success_burn($expected);
        }

        #[test]
        fn $dominates() {
            assert_burn_dominates($expected);
        }

        #[test]
        fn $authored() {
            assert_authored($expected);
        }
    };
}

encounter_case!(
    test_event_present_in_pool_hungering_dark,
    test_splash_is_a_success_triggered_burn_hungering_dark,
    test_burn_dominates_payout_after_jitter_hungering_dark,
    test_matches_authored_spec_hungering_dark,
    ExpectedEncounter {
        id: "hungering_dark",
        rarity: Rarity::Uncommon,
        min_depth: 10,
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty_jc: 5,
        payout: 5,
    }
);

encounter_case!(
    test_event_present_in_pool_deny_the_seam,
    test_splash_is_a_success_triggered_burn_deny_the_seam,
    test_burn_dominates_payout_after_jitter_deny_the_seam,
    test_matches_authored_spec_deny_the_seam,
    ExpectedEncounter {
        id: "deny_the_seam",
        rarity: Rarity::Uncommon,
        min_depth: 20,
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty_jc: 6,
        payout: 6,
    }
);

encounter_case!(
    test_event_present_in_pool_turf_war,
    test_splash_is_a_success_triggered_burn_turf_war,
    test_burn_dominates_payout_after_jitter_turf_war,
    test_matches_authored_spec_turf_war,
    ExpectedEncounter {
        id: "turf_war",
        rarity: Rarity::Uncommon,
        min_depth: 35,
        strategy: EncounterVictimStrategy::ActiveDiggers,
        victim_count: 3,
        penalty_jc: 6,
        payout: 8,
    }
);

encounter_case!(
    test_event_present_in_pool_smoke_ambush,
    test_splash_is_a_success_triggered_burn_smoke_ambush,
    test_burn_dominates_payout_after_jitter_smoke_ambush,
    test_matches_authored_spec_smoke_ambush,
    ExpectedEncounter {
        id: "smoke_ambush",
        rarity: Rarity::Rare,
        min_depth: 60,
        strategy: EncounterVictimStrategy::ActiveDiggers,
        victim_count: 3,
        penalty_jc: 7,
        payout: 10,
    }
);

encounter_case!(
    test_event_present_in_pool_the_tear,
    test_splash_is_a_success_triggered_burn_the_tear,
    test_burn_dominates_payout_after_jitter_the_tear,
    test_matches_authored_spec_the_tear,
    ExpectedEncounter {
        id: "the_tear",
        rarity: Rarity::Rare,
        min_depth: 90,
        strategy: EncounterVictimStrategy::RandomActive,
        victim_count: 3,
        penalty_jc: 8,
        payout: 10,
    }
);

encounter_case!(
    test_event_present_in_pool_the_deep_hunter,
    test_splash_is_a_success_triggered_burn_the_deep_hunter,
    test_burn_dominates_payout_after_jitter_the_deep_hunter,
    test_matches_authored_spec_the_deep_hunter,
    ExpectedEncounter {
        id: "the_deep_hunter",
        rarity: Rarity::Rare,
        min_depth: 110,
        strategy: EncounterVictimStrategy::DeepestN,
        victim_count: 2,
        penalty_jc: 12,
        payout: 12,
    }
);
