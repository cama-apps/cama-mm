use super::*;
use crate::wheel::{
    WheelEconomyPolicy, WheelKind, WheelValue, wheel_catalog, wheel_expected_value,
};

const GUILD: i64 = 4_242;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PortError;

#[derive(Debug)]
struct FakePort {
    balances: BTreeMap<i64, i64>,
    fund: i64,
    transfers: usize,
    fail_read: bool,
    fail_transfer: bool,
}

impl FakePort {
    fn with_player(discord_id: i64, balance: i64, fund: i64) -> Self {
        Self {
            balances: BTreeMap::from([(discord_id, balance)]),
            fund,
            transfers: 0,
            fail_read: false,
            fail_transfer: false,
        }
    }
}

impl BankruptStipendPort for FakePort {
    type Error = PortError;

    fn player_balance(&self, discord_id: i64, _guild_id: i64) -> Result<Option<i64>, Self::Error> {
        if self.fail_read {
            return Err(PortError);
        }
        Ok(self.balances.get(&discord_id).copied())
    }

    fn nonprofit_fund(&self, _guild_id: i64) -> Result<i64, Self::Error> {
        if self.fail_read {
            return Err(PortError);
        }
        Ok(self.fund)
    }

    fn transfer_stipend_atomic(
        &mut self,
        discord_id: i64,
        _guild_id: i64,
        amount: i64,
    ) -> Result<bool, Self::Error> {
        if self.fail_transfer {
            return Err(PortError);
        }
        let Some(balance) = self.balances.get_mut(&discord_id) else {
            return Ok(false);
        };
        if amount <= 0 || self.fund < amount {
            return Ok(false);
        }
        self.fund -= amount;
        *balance += amount;
        self.transfers += 1;
        Ok(true)
    }

    fn distribute_stipends_atomic(
        &mut self,
        discord_ids: &[i64],
        _guild_id: i64,
        maximum_each: i64,
    ) -> Result<Vec<(i64, i64)>, Self::Error> {
        if self.fail_transfer {
            return Err(PortError);
        }
        let mut paid = Vec::new();
        for discord_id in discord_ids {
            let Some(balance) = self.balances.get_mut(discord_id) else {
                continue;
            };
            if *balance > 0 || self.fund <= 0 {
                continue;
            }
            let amount = maximum_each.min(self.fund);
            self.fund -= amount;
            *balance += amount;
            self.transfers += 1;
            paid.push((*discord_id, amount));
        }
        Ok(paid)
    }
}

#[test]
fn test_bankrupt_wheel_mean_equals_target() {
    let policy = WheelEconomyPolicy::default();
    let wedges = wheel_catalog(WheelKind::Bankrupt, policy);
    let mean = wheel_expected_value(WheelKind::Bankrupt, &wedges, policy);
    assert!((mean - policy.bankrupt_target_ev).abs() <= 1.0);
}

#[test]
fn test_bankrupt_wedge_value_clamped_negative() {
    let wedges = wheel_catalog(WheelKind::Bankrupt, WheelEconomyPolicy::default());
    let negative = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            WheelValue::Numeric(_) | WheelValue::Mechanic(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(negative.len(), 2);
    assert!(negative.into_iter().all(|value| value <= -1));
}

#[test]
fn test_normal_wheel_target_ev_unchanged() {
    let policy = WheelEconomyPolicy::default();
    let wedges = wheel_catalog(WheelKind::Regular, policy);
    let mean = wheel_expected_value(WheelKind::Regular, &wedges, policy);
    assert!((mean - policy.regular_target_ev).abs() <= 1.0);
}

#[test]
fn test_bankrupt_wheel_distinct_from_normal_wheel() {
    let policy = WheelEconomyPolicy::default();
    let bankrupt = wheel_catalog(WheelKind::Bankrupt, policy);
    let regular = wheel_catalog(WheelKind::Regular, policy);
    assert_ne!(bankrupt, regular);
    assert!(bankrupt.iter().any(|candidate| {
        regular
            .iter()
            .all(|regular| regular.value != candidate.value)
    }));
}

#[test]
fn test_non_plains_no_stipend() {
    let mut port = FakePort::with_player(70_001, -10, 100);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_001, GUILD, "Mountain"),
        0
    );
    assert_eq!(port.transfers, 0);
    assert_eq!(port.balances[&70_001], -10);
}

#[test]
fn test_positive_balance_no_stipend() {
    let mut port = FakePort::with_player(70_002, 50, 100);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_002, GUILD, "Plains"),
        0
    );
    assert_eq!(port.transfers, 0);
}

#[test]
fn test_zero_balance_pays() {
    let mut port = FakePort::with_player(70_003, 0, 100);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_003, GUILD, "Plains"),
        WHITE_BANKRUPT_STIPEND
    );
    assert_eq!(port.balances[&70_003], WHITE_BANKRUPT_STIPEND);
    assert_eq!(port.fund, 100 - WHITE_BANKRUPT_STIPEND);
}

#[test]
fn test_negative_balance_pays_full() {
    let mut port = FakePort::with_player(70_004, -25, 100);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_004, GUILD, "Plains"),
        WHITE_BANKRUPT_STIPEND
    );
    assert_eq!(port.balances[&70_004], -25 + WHITE_BANKRUPT_STIPEND);
}

#[test]
fn test_partial_pay_when_fund_low() {
    let mut port = FakePort::with_player(70_005, -10, 3);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_005, GUILD, "Plains"),
        3
    );
    assert_eq!(port.balances[&70_005], -7);
    assert_eq!(port.fund, 0);
}

#[test]
fn test_empty_fund_skips() {
    let mut port = FakePort::with_player(70_006, -10, 0);
    assert_eq!(
        apply_bankrupt_stipend(&mut port, 70_006, GUILD, "Plains"),
        0
    );
    assert_eq!(port.transfers, 0);
}

#[test]
fn test_multiplier_is_2x() {
    assert_eq!(TRIVIA_BANKRUPT_MULTIPLIER, 2.0);
    assert_eq!(trivia_bankrupt_reward(1, false), 2);
}

#[test]
fn test_stack_with_red_mana() {
    assert_eq!(trivia_bankrupt_reward(1, true), 2);
}

#[test]
fn test_stack_with_red_mana_higher_streak_bonus() {
    assert_eq!(trivia_bankrupt_reward(2, true), 6);
}

#[test]
fn test_mana_all_pays_stipend_once_across_both_paths() {
    let player_id = 81_001;
    let mut port = FakePort::with_player(player_id, -500, 1_000);

    let first =
        apply_fresh_assignment_stipends(&mut port, &[(player_id, "Plains".to_owned())], GUILD);
    let repeated_board = apply_fresh_assignment_stipends(&mut port, &[], GUILD);
    let later_self_claim = apply_fresh_assignment_stipends(&mut port, &[], GUILD);

    assert_eq!(first, BTreeMap::from([(player_id, WHITE_BANKRUPT_STIPEND)]));
    assert!(repeated_board.is_empty());
    assert!(later_self_claim.is_empty());
    assert_eq!(port.balances[&player_id], -500 + WHITE_BANKRUPT_STIPEND);
    assert_eq!(port.fund, 1_000 - WHITE_BANKRUPT_STIPEND);
    assert_eq!(port.transfers, 1);
}
