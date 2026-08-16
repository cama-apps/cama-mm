//! Pure scaling policy for generated minigame Jopacoin deltas.

/// The default structural scale used when no deployment override is supplied.
pub const DEFAULT_MINIGAME_JC_DELTA_SCALE: f64 = 1.0;

/// Extra pressure applied to deflationary minigame burns and losses.
pub const DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER: f64 = 1.10;

/// A daily reward policy that can adjust newly generated positive rewards.
pub trait DailyRewardPolicy {
    /// Adjust a positive, already structurally scaled reward for a guild.
    fn adjust_reward(&mut self, guild_id: Option<i64>, amount: i64) -> i64;
}

/// Scale a generated minigame delta with half-up rounding and preserved sign.
///
/// Any non-zero input produces a magnitude of at least one, matching the
/// Python policy. Deployment configuration is passed explicitly so this
/// domain function remains pure.
#[must_use]
pub fn scale_minigame_jc_delta(amount: f64, minigame_jc_delta_scale: f64) -> i64 {
    if amount == 0.0 {
        return 0;
    }

    let magnitude = (amount.abs() * minigame_jc_delta_scale + 0.5)
        .floor()
        .max(1.0) as i64;
    if amount > 0.0 { magnitude } else { -magnitude }
}

/// Apply the daily event to an already-scaled positive reward.
///
/// Zero and negative deltas intentionally skip the generic daily policy.
#[must_use]
pub fn apply_daily_reward_event(
    amount: i64,
    guild_id: Option<i64>,
    reward_policy: Option<&mut dyn DailyRewardPolicy>,
) -> i64 {
    match reward_policy {
        Some(policy) if amount > 0 => policy.adjust_reward(guild_id, amount),
        _ => amount,
    }
}

/// Apply structural scaling and then today's positive-reward policy exactly once.
///
/// This is only for newly generated Jopacoin. Existing-liquidity transfers,
/// returned stakes, refunds, reserve disbursements, and loan principal must
/// keep their original amounts. Negative deltas retain structural scaling but
/// skip the generic daily reward policy.
#[must_use]
pub fn adjust_generated_jc_reward(
    amount: f64,
    minigame_jc_delta_scale: f64,
    guild_id: Option<i64>,
    reward_policy: Option<&mut dyn DailyRewardPolicy>,
) -> i64 {
    let adjusted = scale_minigame_jc_delta(amount, minigame_jc_delta_scale);
    apply_daily_reward_event(adjusted, guild_id, reward_policy)
}

/// Scale a minigame burn or loss with the global deflation-pressure bump.
#[must_use]
pub fn scale_deflationary_minigame_jc_delta(amount: f64, minigame_jc_delta_scale: f64) -> i64 {
    let base = scale_minigame_jc_delta(amount, minigame_jc_delta_scale);
    let stronger = scale_minigame_jc_delta(
        amount * DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER,
        minigame_jc_delta_scale,
    );

    if base.unsigned_abs() < 4 {
        return stronger;
    }
    if base > 0 && stronger <= base {
        return base + 1;
    }
    if base < 0 && stronger >= base {
        return base - 1;
    }
    stronger
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MINIGAME_JC_DELTA_SCALE, DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER,
        DailyRewardPolicy, adjust_generated_jc_reward, scale_deflationary_minigame_jc_delta,
        scale_minigame_jc_delta,
    };

    #[test]
    fn test_scale_minigame_jc_delta_uses_half_up_rounding_and_preserves_sign() {
        let scale = DEFAULT_MINIGAME_JC_DELTA_SCALE;

        assert_eq!(scale_minigame_jc_delta(0.0, scale), 0);
        assert_eq!(scale_minigame_jc_delta(1.0, scale), 1);
        assert_eq!(scale_minigame_jc_delta(2.0, scale), 2);
        assert_eq!(scale_minigame_jc_delta(3.0, scale), 3);
        assert_eq!(scale_minigame_jc_delta(5.0, scale), 5);
        assert_eq!(scale_minigame_jc_delta(100.0, scale), 100);
        assert_eq!(scale_minigame_jc_delta(-1.0, scale), -1);
        assert_eq!(scale_minigame_jc_delta(-3.0, scale), -3);
        assert_eq!(scale_minigame_jc_delta(-15.0, scale), -15);
    }

    #[test]
    fn test_scale_minigame_jc_delta_reads_configured_policy() {
        let scale = 0.5;

        assert_eq!(scale_minigame_jc_delta(100.0, scale), 50);
        assert_eq!(scale_minigame_jc_delta(-15.0, scale), -8);
    }

    #[test]
    fn test_generated_reward_applies_central_scale_before_daily_policy() {
        #[derive(Default)]
        struct EventService {
            received: Option<(Option<i64>, i64)>,
        }

        impl DailyRewardPolicy for EventService {
            fn adjust_reward(&mut self, guild_id: Option<i64>, amount: i64) -> i64 {
                self.received = Some((guild_id, amount));
                amount / 2
            }
        }

        let mut events = EventService::default();
        assert_eq!(
            adjust_generated_jc_reward(100.0, 1.0, Some(123), Some(&mut events)),
            50
        );
        assert_eq!(events.received, Some((Some(123), 100)));
    }

    #[test]
    fn test_generated_reward_skips_daily_reward_policy_for_losses() {
        struct EventService;

        impl DailyRewardPolicy for EventService {
            fn adjust_reward(&mut self, _guild_id: Option<i64>, _amount: i64) -> i64 {
                panic!("losses must use their surface-specific policy");
            }
        }

        let mut events = EventService;
        assert_eq!(
            adjust_generated_jc_reward(-100.0, 1.0, Some(123), Some(&mut events)),
            -100
        );
    }

    #[test]
    fn test_scale_deflationary_minigame_jc_delta_is_ten_percent_stronger() {
        let scale = DEFAULT_MINIGAME_JC_DELTA_SCALE;

        assert_eq!(DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER, 1.10);
        assert_eq!(scale_deflationary_minigame_jc_delta(5.0, scale), 6);
        assert_eq!(scale_deflationary_minigame_jc_delta(10.0, scale), 11);
        assert_eq!(scale_deflationary_minigame_jc_delta(20.0, scale), 22);
        assert_eq!(scale_deflationary_minigame_jc_delta(-5.0, scale), -6);
        assert_eq!(scale_deflationary_minigame_jc_delta(-20.0, scale), -22);
        assert!(
            scale_deflationary_minigame_jc_delta(20.0, scale)
                > scale_minigame_jc_delta(20.0, scale)
        );
    }
}
