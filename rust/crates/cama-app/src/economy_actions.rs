//! Discord-independent orchestration for small economy command actions.
//!
//! Validation order and response visibility are application policy. Discord,
//! rate limiting, and persistence remain explicit ports so the Rust side can
//! prove those contracts without opening a gateway or a second database.

use cama_domain::economy_scaling::{DailyRewardPolicy, adjust_generated_jc_reward};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::mana::ManaEffects;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionMessage {
    pub content: String,
    pub visibility: Visibility,
}

pub trait InteractionPort {
    fn user_id(&self) -> i64;
    fn guild_id(&self) -> Option<i64>;
    fn send_initial(&mut self, message: ActionMessage);
    /// Returns false when the interaction can no longer be acknowledged.
    fn defer(&mut self, visibility: Visibility) -> bool;
    fn send_followup(&mut self, message: ActionMessage);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub retry_after_seconds: i64,
}

pub trait RateLimiterPort {
    fn check(
        &mut self,
        scope: &str,
        guild_id: i64,
        user_id: i64,
        limit: i64,
        per_seconds: i64,
    ) -> RateLimitDecision;
}

pub trait DebtPort {
    fn pay_debt_atomic(
        &mut self,
        from_discord_id: i64,
        to_discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<i64, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipTransfer {
    pub from_discord_id: i64,
    pub to_discord_id: i64,
    pub guild_id: Option<i64>,
    pub amount: i64,
    pub fee: i64,
    pub tithe: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceAdjustment {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub amount: i64,
    pub source: &'static str,
    pub actor_id: i64,
    pub related_type: &'static str,
    pub related_id: i64,
    pub reason: &'static str,
    pub base_reward: i64,
}

pub trait TipPlayerPort {
    fn player_exists(&mut self, discord_id: i64, guild_id: Option<i64>) -> bool;
    fn balance(&mut self, discord_id: i64, guild_id: Option<i64>) -> i64;
    fn tip_atomic(&mut self, transfer: TipTransfer) -> Result<(), String>;
    fn adjust_balance(&mut self, adjustment: BalanceAdjustment) -> Result<(), String>;
}

/// Coerce a daily-event multiplier to Python's operational range.
#[must_use]
pub fn bounded_economy_multiplier(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 10.0)
    } else {
        1.0
    }
}

/// Scale one numeric wheel wedge without turning it into the special zero
/// "lose a turn" wedge. Float-to-integer conversion truncates toward zero.
#[must_use]
pub fn apply_gamba_event_multiplier(value: i64, win_multiplier: f64, loss_multiplier: f64) -> i64 {
    if value == 0 {
        return 0;
    }
    let multiplier = if value > 0 {
        win_multiplier
    } else {
        loss_multiplier
    };
    let magnitude = ((value.unsigned_abs() as f64) * bounded_economy_multiplier(multiplier)) as u64;
    if value > 0 {
        magnitude.clamp(1, i64::MAX as u64) as i64
    } else {
        let magnitude = magnitude.clamp(1, 1_u64 << 63);
        if magnitude == 1_u64 << 63 {
            i64::MIN
        } else {
            -(magnitude as i64)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GambaEventEffects {
    pub win_multiplier: f64,
    pub loss_multiplier: f64,
}

pub trait GambaEventPort {
    fn effects(&mut self, guild_id: Option<i64>) -> Result<GambaEventEffects, String>;
}

/// Fetch daily gamba effects, falling back to neutral values on absence or
/// failure, then independently sanitize both values.
#[must_use]
pub fn get_gamba_event_multipliers(
    event_port: Option<&mut dyn GambaEventPort>,
    guild_id: Option<i64>,
) -> (f64, f64) {
    let effects = event_port
        .and_then(|port| port.effects(guild_id).ok())
        .unwrap_or(GambaEventEffects {
            win_multiplier: 1.0,
            loss_multiplier: 1.0,
        });
    (
        bounded_economy_multiplier(effects.win_multiplier),
        bounded_economy_multiplier(effects.loss_multiplier),
    )
}

fn initial(content: impl Into<String>, visibility: Visibility) -> ActionMessage {
    ActionMessage {
        content: content.into(),
        visibility,
    }
}

/// Validate and execute a debt payment in Python's exact observable order.
pub fn paydebt_action<I, R, D>(
    interaction: &mut I,
    target_id: i64,
    amount: i64,
    rate_limiter: &mut R,
    debt_port: &mut D,
) where
    I: InteractionPort,
    R: RateLimiterPort,
    D: DebtPort,
{
    let user_id = interaction.user_id();
    let guild_id = interaction.guild_id();
    let rate_limit = rate_limiter.check("paydebt", guild_id.unwrap_or(0), user_id, 5, 10);
    if !rate_limit.allowed {
        interaction.send_initial(initial(
            format!(
                "Please wait {}s before using `/economy paydebt` again.",
                rate_limit.retry_after_seconds
            ),
            Visibility::Ephemeral,
        ));
        return;
    }
    if amount <= 0 {
        interaction.send_initial(initial("Amount must be positive.", Visibility::Ephemeral));
        return;
    }
    if target_id == user_id {
        interaction.send_initial(initial(
            "You cannot pay your own debt.",
            Visibility::Ephemeral,
        ));
        return;
    }
    if !interaction.defer(Visibility::Public) {
        return;
    }

    let message = match debt_port.pay_debt_atomic(user_id, target_id, guild_id, amount) {
        Ok(amount_paid) => initial(
            format!(
                "<@{user_id}> paid {amount_paid} {JOPACOIN_EMOTE} toward <@{target_id}>'s debt!"
            ),
            Visibility::Public,
        ),
        Err(error) => initial(error, Visibility::Ephemeral),
    };
    interaction.send_followup(message);
}

/// Parameters whose values are injected from deployment configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TipPolicy {
    pub fee_rate: f64,
    pub minigame_jc_delta_scale: f64,
}

/// Execute the transfer and the generated Green-mana post-effect.
///
/// Existing-liquidity transfer and fee accounting commit before the bonus.
/// The bonus is generated currency, so it is structurally scaled and offered
/// to the daily event policy exactly once. A suppressed reward writes nothing.
#[allow(clippy::too_many_arguments)]
pub fn tip_action<I, R, P>(
    interaction: &mut I,
    recipient_id: i64,
    amount: i64,
    rate_limiter: &mut R,
    players: &mut P,
    effects: Option<&ManaEffects>,
    policy: TipPolicy,
    daily_reward_policy: Option<&mut dyn DailyRewardPolicy>,
) where
    I: InteractionPort,
    R: RateLimiterPort,
    P: TipPlayerPort,
{
    let sender_id = interaction.user_id();
    let guild_id = interaction.guild_id();
    let rate_limit = rate_limiter.check("tip", guild_id.unwrap_or(0), sender_id, 5, 10);
    if !rate_limit.allowed {
        interaction.send_initial(initial(
            format!(
                "Please wait {}s before using `/economy tip` again.",
                rate_limit.retry_after_seconds
            ),
            Visibility::Ephemeral,
        ));
        return;
    }
    if !interaction.defer(Visibility::Public) {
        return;
    }
    if amount <= 0 {
        interaction.send_followup(initial("Amount must be positive.", Visibility::Ephemeral));
        return;
    }
    if recipient_id == sender_id {
        interaction.send_followup(initial("You cannot tip yourself.", Visibility::Ephemeral));
        return;
    }
    if !players.player_exists(sender_id, guild_id) {
        interaction.send_followup(initial(
            "You need to `/player register` before you can tip.",
            Visibility::Ephemeral,
        ));
        return;
    }
    if !players.player_exists(recipient_id, guild_id) {
        interaction.send_followup(initial(
            format!("<@{recipient_id}> is not registered."),
            Visibility::Ephemeral,
        ));
        return;
    }

    let fee = if effects.is_some_and(|value| {
        value.color.as_deref() == Some("White") && value.plains_tip_fee_rate.is_some()
    }) {
        0
    } else {
        ((amount as f64 * policy.fee_rate).ceil() as i64).max(1)
    };
    let tithe = effects.map_or(0, |value| {
        if value.plains_tithe_rate > 0.0 {
            ((amount as f64 * value.plains_tithe_rate) as i64).max(1)
        } else {
            0
        }
    });
    let total_cost = amount + fee + tithe;
    if players.balance(sender_id, guild_id) < total_cost {
        interaction.send_followup(initial(
            format!("Insufficient balance. You need {total_cost} {JOPACOIN_EMOTE}."),
            Visibility::Ephemeral,
        ));
        return;
    }
    if let Err(error) = players.tip_atomic(TipTransfer {
        from_discord_id: sender_id,
        to_discord_id: recipient_id,
        guild_id,
        amount,
        fee,
        tithe,
    }) {
        interaction.send_followup(initial(error, Visibility::Ephemeral));
        return;
    }

    if let Some(base_reward) = effects
        .map(|value| value.green_steady_bonus)
        .filter(|v| *v > 0)
    {
        let reward = adjust_generated_jc_reward(
            base_reward as f64,
            policy.minigame_jc_delta_scale,
            guild_id,
            daily_reward_policy,
        );
        if reward > 0 {
            let _ = players.adjust_balance(BalanceAdjustment {
                discord_id: recipient_id,
                guild_id,
                amount: reward,
                source: "mana_reward",
                actor_id: sender_id,
                related_type: "tip_steady_bonus",
                related_id: sender_id,
                reason: "scaled Green mana tip reward",
                base_reward,
            });
        }
    }

    interaction.send_followup(initial(
        format!(
            "<@{sender_id}> tipped {amount} {JOPACOIN_EMOTE} to <@{recipient_id}>! ({fee} {JOPACOIN_EMOTE} fee to Jopacoin Reserve)"
        ),
        Visibility::Public,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeInteraction {
        user_id: i64,
        guild_id: Option<i64>,
        initial: Vec<ActionMessage>,
        followups: Vec<ActionMessage>,
        defer_calls: usize,
    }

    impl InteractionPort for FakeInteraction {
        fn user_id(&self) -> i64 {
            self.user_id
        }

        fn guild_id(&self) -> Option<i64> {
            self.guild_id
        }

        fn send_initial(&mut self, message: ActionMessage) {
            self.initial.push(message);
        }

        fn defer(&mut self, _visibility: Visibility) -> bool {
            self.defer_calls += 1;
            true
        }

        fn send_followup(&mut self, message: ActionMessage) {
            self.followups.push(message);
        }
    }

    struct AllowAll;

    impl RateLimiterPort for AllowAll {
        fn check(
            &mut self,
            _scope: &str,
            _guild_id: i64,
            _user_id: i64,
            _limit: i64,
            _per_seconds: i64,
        ) -> RateLimitDecision {
            RateLimitDecision {
                allowed: true,
                retry_after_seconds: 0,
            }
        }
    }

    #[derive(Default)]
    struct FakeDebt {
        calls: usize,
    }

    impl DebtPort for FakeDebt {
        fn pay_debt_atomic(
            &mut self,
            _from_discord_id: i64,
            _to_discord_id: i64,
            _guild_id: Option<i64>,
            _amount: i64,
        ) -> Result<i64, String> {
            self.calls += 1;
            Ok(0)
        }
    }

    fn assert_early_debt_rejection(target_id: i64, amount: i64, expected: &str) {
        let mut interaction = FakeInteraction {
            user_id: 1,
            guild_id: Some(123),
            ..FakeInteraction::default()
        };
        let mut limiter = AllowAll;
        let mut debt = FakeDebt::default();

        paydebt_action(&mut interaction, target_id, amount, &mut limiter, &mut debt);

        assert_eq!(
            interaction.initial,
            vec![ActionMessage {
                content: expected.to_owned(),
                visibility: Visibility::Ephemeral,
            }]
        );
        assert_eq!(interaction.defer_calls, 0);
        assert_eq!(debt.calls, 0);
    }

    #[test]
    fn test_paydebt_rejects_zero_amount() {
        assert_early_debt_rejection(2, 0, "Amount must be positive.");
    }

    #[test]
    fn test_paydebt_rejects_negative_amount() {
        assert_early_debt_rejection(2, -5, "Amount must be positive.");
    }

    #[test]
    fn test_paydebt_rejects_self_target() {
        assert_early_debt_rejection(1, 10, "You cannot pay your own debt.");
    }

    #[derive(Default)]
    struct FakePlayers {
        transfers: Vec<TipTransfer>,
        adjustments: Vec<BalanceAdjustment>,
    }

    impl TipPlayerPort for FakePlayers {
        fn player_exists(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> bool {
            true
        }

        fn balance(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> i64 {
            100
        }

        fn tip_atomic(&mut self, transfer: TipTransfer) -> Result<(), String> {
            self.transfers.push(transfer);
            Ok(())
        }

        fn adjust_balance(&mut self, adjustment: BalanceAdjustment) -> Result<(), String> {
            self.adjustments.push(adjustment);
            Ok(())
        }
    }

    #[derive(Default)]
    struct SuppressRewards {
        calls: Vec<(Option<i64>, i64)>,
    }

    impl DailyRewardPolicy for SuppressRewards {
        fn adjust_reward(&mut self, guild_id: Option<i64>, amount: i64) -> i64 {
            self.calls.push((guild_id, amount));
            0
        }
    }

    #[test]
    fn test_tip_green_mana_generated_bonus_uses_daily_reward_policy() {
        let mut interaction = FakeInteraction {
            user_id: 1,
            guild_id: Some(123),
            ..FakeInteraction::default()
        };
        let mut limiter = AllowAll;
        let mut players = FakePlayers::default();
        let mut daily_events = SuppressRewards::default();
        let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));

        tip_action(
            &mut interaction,
            2,
            10,
            &mut limiter,
            &mut players,
            Some(&effects),
            TipPolicy {
                fee_rate: 0.01,
                minigame_jc_delta_scale: 1.0,
            },
            Some(&mut daily_events),
        );

        assert_eq!(daily_events.calls, vec![(Some(123), 1)]);
        assert!(players.adjustments.is_empty());
        assert_eq!(players.transfers.len(), 1);
        assert_eq!(players.transfers[0].amount, 10);
    }

    struct FakeGambaEvents;

    impl GambaEventPort for FakeGambaEvents {
        fn effects(&mut self, guild_id: Option<i64>) -> Result<GambaEventEffects, String> {
            assert_eq!(guild_id, Some(12_345));
            Ok(GambaEventEffects {
                win_multiplier: 0.6,
                loss_multiplier: 1.5,
            })
        }
    }

    #[test]
    fn test_gamba_effects_fetch_and_scale_exact_display_amounts() {
        let mut events = FakeGambaEvents;
        let (win_multiplier, loss_multiplier) =
            get_gamba_event_multipliers(Some(&mut events), Some(12_345));

        assert_eq!(
            apply_gamba_event_multiplier(25, win_multiplier, loss_multiplier),
            15
        );
        assert_eq!(
            apply_gamba_event_multiplier(-20, win_multiplier, loss_multiplier),
            -30
        );
    }

    #[test]
    fn test_gamba_multiplier_bounds_and_preserves_numeric_wedge_semantics() {
        assert_eq!(bounded_economy_multiplier(f64::INFINITY), 1.0);
        assert_eq!(bounded_economy_multiplier(-3.0), 0.0);
        assert_eq!(bounded_economy_multiplier(100.0), 10.0);
        assert_eq!(apply_gamba_event_multiplier(1, 0.0, 1.0), 1);
        assert_eq!(apply_gamba_event_multiplier(-1, 1.0, 0.0), -1);
    }
}
