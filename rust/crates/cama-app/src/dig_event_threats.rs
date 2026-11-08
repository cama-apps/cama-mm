//! Discord-neutral policy for `/dig` event threats.
//!
//! Synthetic event definitions and injected rolls keep this module independent
//! of the runtime catalog and RNG. It owns the threat arithmetic, curse
//! lifecycle, atomic persistence boundary, and result presentation.

use cama_db::dig_event_threats::{
    AtomicThreatSettlement, DigEventThreatRepository, DigEventThreatRepositoryError,
    ReplaceCurseOutcome, ThreatCurse, ThreatCurseEffects, ThreatCurseMutation,
    ThreatSettlementOutcome, ThreatSnapshot, ThreatTunnelKey,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use thiserror::Error;

pub const CURSE_STRENGTH_MULTIPLIER: f64 = 1.5;
pub const CURSE_DURATION_BONUS_DIGS: i64 = 2;
pub const NEGATIVE_EVENT_JC_MULTIPLIER: f64 = 1.3;
pub const CAVE_IN_CURSE_CAP: f64 = 0.10;
pub const COOLDOWN_CURSE_CAP: f64 = 0.25;

#[derive(Clone, Debug, PartialEq)]
pub struct TempCurseDefinition {
    pub id: String,
    pub name: String,
    pub duration_digs: i64,
    pub effects: ThreatCurseEffects,
}

impl TempCurseDefinition {
    #[must_use]
    pub fn persisted_unscaled(&self) -> ThreatCurse {
        ThreatCurse {
            id: self.id.clone(),
            name: self.name.clone(),
            digs_remaining: self.duration_digs,
            effects: self.effects,
        }
    }

    #[must_use]
    pub fn persisted_after_event(&self) -> ThreatCurse {
        ThreatCurse {
            id: self.id.clone(),
            name: self.name.clone(),
            digs_remaining: self.duration_digs.saturating_add(CURSE_DURATION_BONUS_DIGS),
            effects: scale_curse_effects(self.effects, CURSE_STRENGTH_MULTIPLIER),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreatEventOutcome {
    pub description: String,
    pub advance: i64,
    pub jc: i64,
    pub cave_in: bool,
    pub streak_loss: i64,
    pub curse: Option<TempCurseDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreatEventOption {
    pub success_chance: f64,
    pub success: ThreatEventOutcome,
    pub failure: Option<ThreatEventOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventThreatChoice {
    Safe,
    Risky,
    Desperate,
}

impl EventThreatChoice {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Risky => "risky",
            Self::Desperate => "desperate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreatRolls {
    pub success_roll: f64,
    pub jc_jitter_multiplier: f64,
    pub advance_jitter: i64,
}

impl Default for ThreatRolls {
    fn default() -> Self {
        Self {
            success_roll: 0.0,
            jc_jitter_multiplier: 1.0,
            advance_jitter: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolveThreatRequest<'a> {
    pub key: ThreatTunnelKey,
    pub event_id: &'a str,
    pub choice: EventThreatChoice,
    pub option: &'a ThreatEventOption,
    pub rolls: ThreatRolls,
    pub minigame_jc_delta_scale: f64,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreatResolution {
    pub choice: EventThreatChoice,
    pub succeeded: bool,
    pub message: String,
    pub depth_delta: i64,
    pub jc_delta: i64,
    pub streak_loss: i64,
    /// The authored definition is surfaced by Python's embed (so a two-dig
    /// curse says two digs even though the persisted curse is strengthened).
    pub curse_applied: Option<TempCurseDefinition>,
    pub balance_after: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResolveThreatOutcome {
    Applied(Box<ThreatResolution>),
    Conflict,
    MissingTunnel,
    MissingPlayer,
}

pub trait DigEventThreatPort {
    type Error;

    fn snapshot(&self, key: ThreatTunnelKey) -> Result<Option<ThreatSnapshot>, Self::Error>;

    fn replace_temp_curse(
        &self,
        key: ThreatTunnelKey,
        curse: Option<&ThreatCurse>,
    ) -> Result<ReplaceCurseOutcome, Self::Error>;

    fn settle_atomic(
        &self,
        request: AtomicThreatSettlement<'_>,
    ) -> Result<ThreatSettlementOutcome, Self::Error>;
}

impl DigEventThreatPort for DigEventThreatRepository {
    type Error = DigEventThreatRepositoryError;

    fn snapshot(&self, key: ThreatTunnelKey) -> Result<Option<ThreatSnapshot>, Self::Error> {
        DigEventThreatRepository::snapshot(self, key)
    }

    fn replace_temp_curse(
        &self,
        key: ThreatTunnelKey,
        curse: Option<&ThreatCurse>,
    ) -> Result<ReplaceCurseOutcome, Self::Error> {
        DigEventThreatRepository::replace_temp_curse(self, key, curse)
    }

    fn settle_atomic(
        &self,
        request: AtomicThreatSettlement<'_>,
    ) -> Result<ThreatSettlementOutcome, Self::Error> {
        DigEventThreatRepository::settle_atomic(self, request)
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ThreatPolicyError {
    #[error("event success chance must be finite and in [0, 1]")]
    InvalidSuccessChance,
    #[error("event rolls and economy scale must be finite")]
    InvalidRoll,
    #[error("streak loss cannot be negative")]
    NegativeStreakLoss,
}

#[derive(Debug)]
pub enum DigEventThreatServiceError<E> {
    Port(E),
    Policy(ThreatPolicyError),
}

#[derive(Clone, Debug)]
pub struct DigEventThreatService<P> {
    port: P,
}

impl<P> DigEventThreatService<P>
where
    P: DigEventThreatPort,
{
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    /// Helper boundary: replace an active curse without strengthening it.
    pub fn set_temp_curse(
        &self,
        key: ThreatTunnelKey,
        curse: &TempCurseDefinition,
    ) -> Result<ReplaceCurseOutcome, P::Error> {
        self.port
            .replace_temp_curse(key, Some(&curse.persisted_unscaled()))
    }

    pub fn resolve_event(
        &self,
        request: ResolveThreatRequest<'_>,
    ) -> Result<ResolveThreatOutcome, DigEventThreatServiceError<P::Error>> {
        validate_request(&request).map_err(DigEventThreatServiceError::Policy)?;
        let snapshot = self
            .port
            .snapshot(request.key)
            .map_err(DigEventThreatServiceError::Port)?;
        let Some(snapshot) = snapshot else {
            return Ok(ResolveThreatOutcome::MissingTunnel);
        };

        let succeeded = request.rolls.success_roll < request.option.success_chance;
        let outcome = if succeeded {
            &request.option.success
        } else {
            request
                .option
                .failure
                .as_ref()
                .unwrap_or(&request.option.success)
        };
        let depth_delta = jitter_advance(outcome.advance, request.rolls.advance_jitter);
        let depth_after = snapshot.depth.saturating_add(depth_delta).max(0);
        let streak_loss = streak_setback(snapshot.streak_days, outcome.streak_loss);
        let streak_days_after = snapshot.streak_days.saturating_sub(streak_loss).max(0);
        let jc_delta = event_jc_delta(
            outcome.jc,
            request.rolls.jc_jitter_multiplier,
            request.minigame_jc_delta_scale,
        );
        let persisted_curse = outcome
            .curse
            .as_ref()
            .map(TempCurseDefinition::persisted_after_event);
        let detail = format!(
            "{{\"event_id\":\"{}\",\"choice\":\"{}\",\"succeeded\":{},\"streak_days_lost\":{},\"curse\":{}}}",
            escape_json(request.event_id),
            request.choice.as_str(),
            succeeded,
            streak_loss,
            outcome.curse.as_ref().map_or_else(
                || "null".to_owned(),
                |curse| format!("\"{}\"", escape_json(&curse.name)),
            )
        );
        let settlement = self
            .port
            .settle_atomic(AtomicThreatSettlement {
                expected: &snapshot,
                depth_after,
                streak_days_after,
                luminosity_after: snapshot.luminosity,
                curse_mutation: persisted_curse
                    .as_ref()
                    .map_or(ThreatCurseMutation::Preserve, ThreatCurseMutation::Replace),
                balance_delta: jc_delta,
                action_type: "event",
                related_id: request.event_id,
                reason: if jc_delta < 0 {
                    "dig event loss"
                } else {
                    "dig event reward"
                },
                detail_json: &detail,
                created_at: request.now,
            })
            .map_err(DigEventThreatServiceError::Port)?;
        Ok(match settlement {
            ThreatSettlementOutcome::Applied { balance_after, .. } => {
                ResolveThreatOutcome::Applied(Box::new(ThreatResolution {
                    choice: request.choice,
                    succeeded,
                    message: outcome.description.clone(),
                    depth_delta,
                    jc_delta,
                    streak_loss,
                    curse_applied: outcome.curse.clone(),
                    balance_after: (jc_delta < 0).then_some(balance_after),
                }))
            }
            ThreatSettlementOutcome::Conflict => ResolveThreatOutcome::Conflict,
            ThreatSettlementOutcome::MissingTunnel => ResolveThreatOutcome::MissingTunnel,
            ThreatSettlementOutcome::MissingPlayer => ResolveThreatOutcome::MissingPlayer,
        })
    }
}

#[must_use]
pub fn scale_curse_effects(effects: ThreatCurseEffects, multiplier: f64) -> ThreatCurseEffects {
    ThreatCurseEffects {
        advance_bonus: effects
            .advance_bonus
            .map(|value| round_half_even(value as f64 * multiplier)),
        cave_in_bonus: effects.cave_in_bonus.map(|value| value * multiplier),
        cooldown_penalty: effects.cooldown_penalty.map(|value| value * multiplier),
        jc_bonus: effects
            .jc_bonus
            .map(|value| round_half_even(value as f64 * multiplier)),
        luminosity_drain: effects
            .luminosity_drain
            .map(|value| round_half_even(value as f64 * multiplier)),
    }
}

#[must_use]
pub fn streak_setback(current_streak: i64, authored_loss: i64) -> i64 {
    let current = current_streak.max(0);
    let setback = authored_loss.max(0).saturating_add(current / 20);
    current.min(setback)
}

#[must_use]
pub fn event_jc_delta(authored: i64, jitter_multiplier: f64, scale: f64) -> i64 {
    if authored == 0 {
        return 0;
    }
    let tuned = if authored < 0 {
        round_half_even(authored as f64 * NEGATIVE_EVENT_JC_MULTIPLIER)
    } else {
        authored
    };
    let jittered = round_half_even(tuned as f64 * jitter_multiplier);
    let structurally_scaled = if jittered < 0 {
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(jittered) as f64, scale)
    } else {
        scale_minigame_jc_delta(jittered as f64, scale)
    };
    scale_positive_dig_jc(structurally_scaled)
}

fn validate_request(request: &ResolveThreatRequest<'_>) -> Result<(), ThreatPolicyError> {
    if !request.option.success_chance.is_finite()
        || !(0.0..=1.0).contains(&request.option.success_chance)
    {
        return Err(ThreatPolicyError::InvalidSuccessChance);
    }
    if !request.rolls.success_roll.is_finite()
        || !request.rolls.jc_jitter_multiplier.is_finite()
        || !request.minigame_jc_delta_scale.is_finite()
        || request.minigame_jc_delta_scale < 0.0
    {
        return Err(ThreatPolicyError::InvalidRoll);
    }
    if request.option.success.streak_loss < 0
        || request
            .option
            .failure
            .as_ref()
            .is_some_and(|outcome| outcome.streak_loss < 0)
    {
        return Err(ThreatPolicyError::NegativeStreakLoss);
    }
    Ok(())
}

fn jitter_advance(authored: i64, jitter: i64) -> i64 {
    match authored.cmp(&0) {
        std::cmp::Ordering::Greater => authored.saturating_add(jitter).max(1),
        std::cmp::Ordering::Less => authored.saturating_add(jitter).min(-1),
        std::cmp::Ordering::Equal => 0,
    }
}

/// Python's `round` is ties-to-even; curse scaling and authored negative event
/// tuning rely on it (not Rust's ties-away-from-zero `f64::round`).
fn round_half_even(value: f64) -> i64 {
    let lower = value.floor();
    let fraction = value - lower;
    if (fraction - 0.5).abs() > f64::EPSILON * value.abs().max(1.0) {
        return value.round() as i64;
    }
    let lower_i64 = lower as i64;
    if lower_i64 % 2 == 0 {
        lower_i64
    } else {
        lower_i64.saturating_add(1)
    }
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
#[path = "dig_event_threats/tests.rs"]
mod tests;
