//! Pure hostile Dig splash selection and burn policy.

use crate::economy_scaling::scale_deflationary_minigame_jc_delta;

pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;
pub const DIG_EVENT_DEFLATION_MULTIPLIER: f64 = 1.375;

#[must_use]
pub fn strengthen_dig_event_penalty(amount: i64) -> i64 {
    if amount == 0 {
        return 0;
    }
    let magnitude = ((amount.unsigned_abs() as f64) * DIG_EVENT_DEFLATION_MULTIPLIER).ceil() as i64;
    if amount > 0 { magnitude } else { -magnitude }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnSplashResult {
    pub victims: Vec<(i64, i64)>,
    pub total_burned: i64,
}

pub trait BurnSplashPort {
    fn registered_players(&mut self, guild_id: i64) -> Vec<i64>;
    fn balance(&mut self, discord_id: i64, guild_id: i64) -> Result<i64, String>;
    fn burn(&mut self, discord_id: i64, guild_id: i64, amount: i64);
    fn log_victim(&mut self, discord_id: i64, guild_id: i64, amount: i64);
}

/// Resolve the deterministic portion of a `random_active` burn after the
/// caller has supplied its request-local player ordering. The live entropy
/// adapter may shuffle this pool first; threshold, scaling, clamping, writes,
/// and audit behavior remain fixed here.
pub fn resolve_random_active_burn<P: BurnSplashPort>(
    port: &mut P,
    guild_id: i64,
    digger_id: i64,
    victim_count: usize,
    authored_penalty: i64,
    minigame_jc_delta_scale: f64,
) -> BurnSplashResult {
    if victim_count == 0 || authored_penalty <= 0 {
        return BurnSplashResult {
            victims: Vec::new(),
            total_burned: 0,
        };
    }
    let penalty = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(authored_penalty) as f64,
        minigame_jc_delta_scale,
    );
    let candidates = port
        .registered_players(guild_id)
        .into_iter()
        .filter(|discord_id| *discord_id != digger_id)
        .take(victim_count)
        .collect::<Vec<_>>();
    let mut victims = Vec::new();
    for discord_id in candidates {
        let Ok(balance) = port.balance(discord_id, guild_id) else {
            continue;
        };
        if balance < HOSTILE_LOSS_MIN_BALANCE {
            continue;
        }
        let actual = penalty.min(balance);
        if actual <= 0 {
            continue;
        }
        port.burn(discord_id, guild_id, actual);
        port.log_victim(discord_id, guild_id, actual);
        victims.push((discord_id, actual));
    }
    BurnSplashResult {
        total_burned: victims.iter().map(|(_, amount)| amount).sum(),
        victims,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct FakePort {
        balances: BTreeMap<i64, i64>,
        deltas: Vec<(i64, i64)>,
        actions: Vec<(i64, i64)>,
    }

    impl BurnSplashPort for FakePort {
        fn registered_players(&mut self, guild_id: i64) -> Vec<i64> {
            assert_eq!(guild_id, 123);
            vec![2, 3]
        }

        fn balance(&mut self, discord_id: i64, guild_id: i64) -> Result<i64, String> {
            assert_eq!(guild_id, 123);
            Ok(self.balances[&discord_id])
        }

        fn burn(&mut self, discord_id: i64, guild_id: i64, amount: i64) {
            assert_eq!(guild_id, 123);
            *self.balances.get_mut(&discord_id).unwrap() -= amount;
            self.deltas.push((discord_id, -amount));
        }

        fn log_victim(&mut self, discord_id: i64, guild_id: i64, amount: i64) {
            assert_eq!(guild_id, 123);
            self.actions.push((discord_id, -amount));
        }
    }

    #[test]
    fn test_dig_splash_burn_skips_players_below_auto_blind_threshold() {
        let mut port = FakePort {
            balances: BTreeMap::from([(2, 49), (3, 50)]),
            deltas: Vec::new(),
            actions: Vec::new(),
        };

        let result = resolve_random_active_burn(&mut port, 123, 1, 2, 10, 1.0);
        let expected =
            scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(10) as f64, 1.0);

        assert_eq!(result.victims, vec![(3, expected)]);
        assert_eq!(port.balances, BTreeMap::from([(2, 49), (3, 50 - expected)]));
        assert_eq!(port.deltas, vec![(3, -expected)]);
        assert_eq!(port.actions, vec![(3, -expected)]);
    }
}
