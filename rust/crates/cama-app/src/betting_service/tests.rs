use std::collections::{BTreeMap, BTreeSet};

use super::*;

#[test]
fn test_shuffle_result_vs_pending_state_keys() {
    let players = (1..=10).collect::<Vec<_>>();
    let (result, pending) = typed_shuffle(&players, 123_456);
    assert_eq!(result.radiant_team.player_ids, [1, 2, 3, 4, 5]);
    assert_eq!(result.dire_team.player_ids, [6, 7, 8, 9, 10]);
    assert_eq!(pending.radiant_team_ids, result.radiant_team.player_ids);
    assert_eq!(pending.dire_team_ids, result.dire_team.player_ids);
    assert_eq!(pending.shuffle_timestamp, 123_456);
}

#[test]
fn test_disabled_top_tier_uses_base_rate_for_all_slots() {
    let candidates = (14_900..14_910)
        .map(|discord_id| WealthSnapshot {
            discord_id,
            balance: 100,
        })
        .collect::<Vec<_>>();
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &BTreeSet::new(),
        AutomaticSpectatorConfig {
            top_basis_points: 0,
            ..AutomaticSpectatorConfig::default()
        },
    );
    assert_eq!(plan.bets.len(), 10);
    assert_eq!(plan.top_count, 0);
    assert!(plan.bets.iter().all(|bet| bet.basis_points == 100));
}

#[test]
fn test_spectator_basis_point_rounding_matches_python_ties_to_even() {
    let candidates = [
        WealthSnapshot {
            discord_id: 15_200,
            balance: 250,
        }, // 2.5 -> 2
        WealthSnapshot {
            discord_id: 15_201,
            balance: 150,
        }, // 1.5 -> 2
        WealthSnapshot {
            discord_id: 15_202,
            balance: 50,
        }, // .5 -> 0, skipped
    ];
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &BTreeSet::new(),
        AutomaticSpectatorConfig {
            enabled: true,
            total_count: 3,
            top_count: 0,
            base_basis_points: 100,
            top_basis_points: 0,
        },
    );
    assert_eq!(
        plan.bets
            .iter()
            .map(|bet| (bet.discord_id, bet.amount))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([(15_200, 2), (15_201, 2)])
    );
}

#[test]
fn test_create_auto_spectator_bets_uses_top_richest_spectators() {
    let participants = (15_000..15_010).collect::<BTreeSet<_>>();
    let mut candidates = participants
        .iter()
        .map(|discord_id| WealthSnapshot {
            discord_id: *discord_id,
            balance: 1_000,
        })
        .collect::<Vec<_>>();
    candidates.extend(
        [1_000, 900, 800, 700, 600, 500, 400, 300, 200, 100, 90, 80]
            .into_iter()
            .enumerate()
            .map(|(index, balance)| WealthSnapshot {
                discord_id: 15_100 + i64::try_from(index).expect("small index"),
                balance,
            }),
    );
    let plan = plan_automatic_spectator_bets(
        &candidates,
        &participants,
        AutomaticSpectatorConfig::default(),
    );
    assert_eq!(plan.bets.len(), 10);
    assert_eq!(plan.total_count, 10);
    assert_eq!(plan.top_count, 5);
    assert_eq!(plan.top_basis_points, 200);
    assert_eq!(plan.base_basis_points, 100);
    assert_eq!(
        plan.bets
            .iter()
            .map(|bet| (bet.discord_id, bet.amount))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            (15_100, 20),
            (15_101, 18),
            (15_102, 16),
            (15_103, 14),
            (15_104, 12),
            (15_105, 5),
            (15_106, 4),
            (15_107, 3),
            (15_108, 2),
            (15_109, 1),
        ])
    );
    assert!(plan.total_radiant.saturating_sub(plan.total_dire).abs() <= 2);
    assert!(
        plan.bets
            .iter()
            .all(|bet| !participants.contains(&bet.discord_id))
    );
}

fn pending_state() -> PendingMatchState {
    PendingMatchState {
        radiant_team_ids: vec![1, 2, 3, 4, 5],
        dire_team_ids: vec![6, 7, 8, 9, 10],
        shuffle_timestamp: 100,
        bet_lock_until: 1_000,
        is_draft: false,
        is_bomb_pot: false,
        is_openskill_shuffle: false,
        balancing_rating_system: RatingSystem::Glicko,
    }
}

#[test]
fn test_bomb_pot_flag_persisted_in_pending_match() {
    let mut state = pending_state();
    state.is_bomb_pot = true;
    assert!(state.to_payload_json().contains("\"is_bomb_pot\":true"));
    state.is_bomb_pot = false;
    assert!(state.to_payload_json().contains("\"is_bomb_pot\":false"));
}

#[test]
fn test_openskill_shuffle_flag_persisted_in_pending_match() {
    let mut state = pending_state();
    state.is_openskill_shuffle = true;
    assert!(
        state
            .to_payload_json()
            .contains("\"is_openskill_shuffle\":true")
    );
    state.is_openskill_shuffle = false;
    assert!(
        state
            .to_payload_json()
            .contains("\"is_openskill_shuffle\":false")
    );
}

#[test]
fn test_balancing_rating_system_persisted_in_pending_match() {
    let mut state = pending_state();
    for (rating_system, expected) in [
        (RatingSystem::OpenSkill, "openskill"),
        (RatingSystem::Glicko, "glicko"),
        (RatingSystem::Jopacoin, "jopacoin"),
    ] {
        state.balancing_rating_system = rating_system;
        assert!(
            state
                .to_payload_json()
                .contains(&format!("\"balancing_rating_system\":\"{expected}\""))
        );
    }
}
