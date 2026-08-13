use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::convert::Infallible;

use super::*;

const GUILD: i64 = 12_345;

fn player(discord_id: i64, balance: i64) -> Player {
    Player {
        discord_id: Some(discord_id),
        guild_id: Some(GUILD),
        jopacoin_balance: balance,
        ..Player::new(format!("Player {discord_id}"))
    }
}

fn static_golden() -> Vec<WheelWedge> {
    golden_wheel_wedges(WheelEconomyPolicy::default())
}

fn representative_economy<'a>(
    other_top_balances: &'a [i64],
    bottom_player_balances: &'a [i64],
) -> LiveGoldenEconomy<'a> {
    LiveGoldenEconomy {
        spinner_balance: 2_500,
        other_top_balances,
        rank_next_balance: Some(1_200),
        total_positive_balance: 25_000,
        bottom_player_balances,
    }
}

#[test]
fn test_golden_wheel_has_24_wedges() {
    assert_eq!(static_golden().len(), 24);
}

#[test]
fn test_golden_wheel_all_overextended_negative() {
    let overextended = static_golden()
        .into_iter()
        .filter(|wedge| wedge.color == "#4a3000")
        .collect::<Vec<_>>();
    assert_eq!(overextended.len(), 2);
    assert!(
        overextended
            .iter()
            .all(|wedge| { matches!(wedge.value, WheelValue::Numeric(value) if value < 0) })
    );
}

#[test]
fn test_golden_wheel_contains_crown_jewel() {
    let wedges = static_golden();
    let crown = wedges
        .iter()
        .filter(|wedge| wedge.color == "#fffacd")
        .collect::<Vec<_>>();
    assert_eq!(crown.len(), 1);
    assert_eq!(crown[0].label, "250");
    assert_eq!(crown[0].value, WheelValue::Numeric(250));
}

#[test]
fn test_golden_wheel_contains_new_mechanics() {
    let values = static_golden()
        .into_iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<HashSet<_>>();
    for mechanic in [
        WheelMechanic::Heist,
        WheelMechanic::MarketCrash,
        WheelMechanic::CompoundInterest,
        WheelMechanic::TrickleDown,
        WheelMechanic::Dividend,
        WheelMechanic::HostileTakeover,
        WheelMechanic::Recession,
    ] {
        assert!(values.contains(&mechanic), "missing {mechanic:?}");
    }
}

#[test]
fn test_golden_wheel_contains_shell_mechanics() {
    let values = static_golden()
        .into_iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<HashSet<_>>();
    assert!(values.contains(&WheelMechanic::RedShell));
    assert!(values.contains(&WheelMechanic::BlueShell));
}

#[test]
fn test_golden_wheel_ev_approximately_correct() {
    let other_top = [2_000, 1_800];
    let bottom = [
        4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17, 18, 19, 20, 21, 22, 12, 10, 8, 7, 6, 5, 4, 4,
        5, 6, 7, 8,
    ];
    let policy = WheelEconomyPolicy::default();
    let economy = representative_economy(&other_top, &bottom);
    let wedges = compute_live_golden_wedges(economy, policy);
    let expected_value = live_golden_expected_value(&wedges, economy, policy);
    let target = scale_minigame_jc_delta(policy.golden_target_ev, policy.minigame_scale) as f64;
    assert!((expected_value - target).abs() < 1.0);
    assert!(expected_value < 0.0);
}

#[test]
fn test_live_golden_wheel_overextended_label_shows_value() {
    let other_top = [2_000, 1_800];
    let bottom = [10; 30];
    let wedges = compute_live_golden_wedges(
        representative_economy(&other_top, &bottom),
        WheelEconomyPolicy::default(),
    );
    let overextended = wedges
        .into_iter()
        .filter(|wedge| wedge.color == "#4a3000")
        .collect::<Vec<_>>();
    assert_eq!(overextended.len(), 2);
    for wedge in overextended {
        let WheelValue::Numeric(value) = wedge.value else {
            panic!("OVEREXTENDED must be numeric");
        };
        assert!(value < 0);
        assert_ne!(wedge.label, "OVEREXTENDED");
        assert_eq!(wedge.label, value.to_string());
    }
}

#[test]
fn test_live_golden_wheel_in_rainbow_order() {
    let other_top = [2_000, 1_800];
    let bottom = [10; 30];
    let live = compute_live_golden_wedges(
        representative_economy(&other_top, &bottom),
        WheelEconomyPolicy::default(),
    );
    assert_eq!(
        live.iter().map(|wedge| wedge.color).collect::<Vec<_>>(),
        static_golden()
            .iter()
            .map(|wedge| wedge.color)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_get_wheel_wedges_returns_golden_when_flag_set() {
    let policy = WheelEconomyPolicy::default();
    assert_eq!(wheel_wedges_for_flags(false, true, policy), static_golden());
    assert_eq!(
        wheel_wedges_for_flags(true, false, policy),
        wheel_catalog(WheelKind::Bankrupt, policy)
    );
    assert_eq!(
        wheel_wedges_for_flags(false, false, policy),
        wheel_catalog(WheelKind::Regular, policy)
    );
}

#[test]
fn test_bankrupt_takes_priority_over_golden() {
    let policy = WheelEconomyPolicy::default();
    assert_eq!(
        wheel_wedges_for_flags(true, false, policy),
        wheel_catalog(WheelKind::Bankrupt, policy)
    );
    assert_eq!(
        select_live_wheel_kind(1, -10, false, true, &[player(1, -10)]),
        WheelKind::Bankrupt
    );
}

#[test]
fn test_heist_appears_twice() {
    assert_eq!(
        static_golden()
            .iter()
            .filter(|wedge| wedge.value == WheelValue::Mechanic(WheelMechanic::Heist))
            .count(),
        2
    );
}

#[derive(Clone, Debug, PartialEq)]
enum ServiceCall {
    Bottom {
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    },
    Total {
        guild_id: Option<i64>,
    },
    Log(GoldenSpinRequest),
}

#[derive(Debug)]
struct FakeGoldenRepository {
    bottom: Vec<Player>,
    total: i64,
    spin_id: i64,
    calls: RefCell<Vec<ServiceCall>>,
}

impl Default for FakeGoldenRepository {
    fn default() -> Self {
        Self {
            bottom: Vec::new(),
            total: 0,
            spin_id: 17,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl GoldenWheelRepositoryPort for FakeGoldenRepository {
    type Error = Infallible;

    fn get_leaderboard_bottom(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    ) -> Result<Vec<Player>, Self::Error> {
        self.calls.borrow_mut().push(ServiceCall::Bottom {
            guild_id,
            limit,
            min_balance,
        });
        Ok(self.bottom.clone())
    }

    fn get_total_positive_balance(&self, guild_id: Option<i64>) -> Result<i64, Self::Error> {
        self.calls
            .borrow_mut()
            .push(ServiceCall::Total { guild_id });
        Ok(self.total)
    }

    fn log_wheel_spin(&self, spin: GoldenSpinRequest) -> Result<i64, Self::Error> {
        self.calls.borrow_mut().push(ServiceCall::Log(spin));
        Ok(self.spin_id)
    }
}

#[test]
fn test_get_leaderboard_bottom_passthrough() {
    let repository = FakeGoldenRepository {
        bottom: vec![player(3, 10), player(2, 50)],
        ..FakeGoldenRepository::default()
    };
    let service = GoldenWheelService::new(repository);
    let result = service.get_leaderboard_bottom(Some(GUILD), 3, 1).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(
        *service.repository().calls.borrow(),
        [ServiceCall::Bottom {
            guild_id: Some(GUILD),
            limit: 3,
            min_balance: 1
        }]
    );
}

#[test]
fn test_get_total_positive_balance_passthrough() {
    let repository = FakeGoldenRepository {
        total: 306,
        ..FakeGoldenRepository::default()
    };
    let service = GoldenWheelService::new(repository);
    assert_eq!(
        service.get_total_positive_balance(Some(GUILD)).unwrap(),
        306
    );
    assert_eq!(
        *service.repository().calls.borrow(),
        [ServiceCall::Total {
            guild_id: Some(GUILD)
        }]
    );
}

#[test]
fn test_log_wheel_spin_with_is_golden() {
    let service = GoldenWheelService::new(FakeGoldenRepository::default());
    let request = GoldenSpinRequest {
        discord_id: 601,
        guild_id: Some(GUILD),
        result: 100,
        spin_time: 1_700_000_000,
        is_golden: true,
    };
    assert_eq!(service.log_wheel_spin(request.clone()).unwrap(), 17);
    assert_eq!(
        *service.repository().calls.borrow(),
        [ServiceCall::Log(request)]
    );
}

#[test]
fn test_top_1_is_eligible() {
    let leaderboard = [
        player(1, 1_000),
        player(2, 500),
        player(3, 10),
        player(4, 1),
    ];
    assert!(is_golden_eligible(1, &leaderboard, 3));
    assert_eq!(
        select_live_wheel_kind(1, 1_000, false, false, &leaderboard),
        WheelKind::Golden
    );
}

#[test]
fn test_top_3_is_eligible() {
    let leaderboard = [
        player(1, 1_000),
        player(2, 500),
        player(3, 100),
        player(4, 10),
    ];
    assert!(is_golden_eligible(3, &leaderboard, 3));
}

#[test]
fn test_rank_4_is_not_eligible() {
    let leaderboard = [
        player(1, 1_000),
        player(2, 500),
        player(3, 100),
        player(4, 10),
    ];
    assert!(!is_golden_eligible(4, &leaderboard, 3));
    assert_eq!(
        select_live_wheel_kind(4, 10, false, false, &leaderboard),
        WheelKind::Regular
    );
}

#[test]
fn test_empty_leaderboard_not_eligible() {
    assert!(!is_golden_eligible(1, &[], 3));
}

#[test]
fn test_bankrupt_wheel_priority() {
    assert_eq!(
        select_live_wheel_kind(1, -1, false, true, &[player(1, 1_000)]),
        WheelKind::Bankrupt
    );
}

#[test]
fn test_departed_players_do_not_displace_visible_top_three() {
    let raw = [
        player(99, 10_000),
        player(1, 900),
        player(2, 800),
        player(3, 700),
    ];
    let members = BTreeSet::from([1, 2, 3]);
    let visible = filter_visible_leaderboard(&raw, Some(GUILD), Some(&members), 3);
    assert_eq!(
        visible
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[derive(Debug)]
struct VisibleRepository {
    players: Vec<Player>,
    calls: Vec<(Option<i64>, usize)>,
}

impl VisibleBalanceLeaderboardPort for VisibleRepository {
    type Error = Infallible;

    fn balance_leaderboard(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error> {
        self.calls.push((guild_id, limit));
        Ok(self.players.iter().take(limit).cloned().collect())
    }
}

#[test]
fn test_golden_lookup_fetches_past_raw_top_three_before_filtering() {
    let mut repository = VisibleRepository {
        players: vec![
            player(99, 10_000),
            player(98, 9_000),
            player(1, 900),
            player(2, 800),
            player(3, 700),
        ],
        calls: Vec::new(),
    };
    let members = BTreeSet::from([1, 2, 3]);
    let visible =
        load_visible_balance_leaderboard(&mut repository, Some(GUILD), Some(&members), 3).unwrap();
    assert_eq!(
        visible
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        repository.calls,
        [(Some(GUILD), ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT)]
    );
}

#[test]
fn test_dividend_uses_visible_guild_wealth() {
    let raw = [
        player(99, 100_000),
        player(1, 2_500),
        player(2, 1_500),
        Player {
            guild_id: Some(GUILD + 1),
            ..player(3, 50_000)
        },
    ];
    let members = BTreeSet::from([1, 2, 3]);
    let resolution = resolve_visible_dividend(&raw, Some(GUILD), Some(&members), 1.0);
    assert_eq!(
        resolution,
        DividendResolution {
            total_positive_balance: 4_000,
            reward: 20
        }
    );
}
