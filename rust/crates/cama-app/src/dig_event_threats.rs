//! Discord-neutral policy for `/dig` event threats.
//!
//! Synthetic event definitions and injected rolls keep this module independent
//! of Python's live catalog and RNG. Python remains authoritative for command
//! registration and the full dig pipeline; Rust mirrors the threat arithmetic,
//! curse lifecycle, atomic persistence boundary, and result presentation.

use cama_db::dig_event_threats::{
    AtomicThreatSettlement, DigEventThreatRepository, DigEventThreatRepositoryError,
    ReplaceCurseOutcome, ThreatCurse, ThreatCurseEffects, ThreatCurseMutation,
    ThreatSettlementOutcome, ThreatSnapshot, ThreatTunnelKey,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
};
use cama_domain::embed_safety::EmbedModel;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursedDigInput {
    pub base_advance: i64,
    pub base_jc: i64,
    /// Luminosity after the ordinary layer/weather drain.
    pub clean_luminosity_after: i64,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursedDigApplied {
    pub advance: i64,
    pub jc_earned: i64,
    pub luminosity_after: i64,
    pub curse_remaining: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CursedDigOutcome {
    Applied(CursedDigApplied),
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

    /// Apply one active curse exactly once, decrementing/clearing it in the
    /// same guarded transaction as the resulting dig state and wallet credit.
    pub fn apply_cursed_dig(
        &self,
        key: ThreatTunnelKey,
        input: CursedDigInput,
    ) -> Result<CursedDigOutcome, P::Error> {
        let Some(snapshot) = self.port.snapshot(key)? else {
            return Ok(CursedDigOutcome::MissingTunnel);
        };
        let active = snapshot.temp_curse.active();
        let effects = active.map_or_else(ThreatCurseEffects::default, |curse| curse.effects);
        let applied = apply_curse_to_dig(
            input.base_advance,
            input.base_jc,
            input.clean_luminosity_after,
            effects,
        );
        let curse_after = active.and_then(|curse| {
            let mut next = curse.clone();
            next.digs_remaining -= 1;
            (next.digs_remaining > 0).then_some(next)
        });
        let detail = format!(
            "{{\"curse_id\":{},\"advance\":{},\"jc\":{}}}",
            active.map_or_else(
                || "null".to_owned(),
                |curse| format!("\"{}\"", escape_json(&curse.id)),
            ),
            applied.advance,
            applied.jc_earned,
        );
        let settlement = self.port.settle_atomic(AtomicThreatSettlement {
            expected: &snapshot,
            depth_after: snapshot.depth.saturating_add(applied.advance),
            streak_days_after: snapshot.streak_days,
            luminosity_after: applied.luminosity_after,
            curse_mutation: curse_after
                .as_ref()
                .map_or(ThreatCurseMutation::Clear, ThreatCurseMutation::Replace),
            balance_delta: applied.jc_earned,
            action_type: "dig",
            related_id: "temp_curse",
            reason: "dig reward after temporary curse",
            detail_json: &detail,
            created_at: input.now,
        })?;
        Ok(match settlement {
            ThreatSettlementOutcome::Applied { .. } => {
                CursedDigOutcome::Applied(CursedDigApplied {
                    advance: applied.advance,
                    jc_earned: applied.jc_earned,
                    luminosity_after: applied.luminosity_after,
                    curse_remaining: curse_after.map(|curse| curse.digs_remaining),
                })
            }
            ThreatSettlementOutcome::Conflict => CursedDigOutcome::Conflict,
            ThreatSettlementOutcome::MissingTunnel => CursedDigOutcome::MissingTunnel,
            ThreatSettlementOutcome::MissingPlayer => CursedDigOutcome::MissingPlayer,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedCurseDig {
    pub advance: i64,
    pub jc_earned: i64,
    pub luminosity_after: i64,
}

#[must_use]
pub fn apply_curse_to_dig(
    base_advance: i64,
    base_jc: i64,
    clean_luminosity_after: i64,
    effects: ThreatCurseEffects,
) -> AppliedCurseDig {
    AppliedCurseDig {
        advance: base_advance
            .saturating_add(effects.advance_bonus.unwrap_or(0))
            .max(1),
        jc_earned: base_jc.saturating_add(effects.jc_bonus.unwrap_or(0)).max(0),
        luminosity_after: clean_luminosity_after
            .saturating_sub(effects.luminosity_drain.unwrap_or(0).max(0))
            .max(0),
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
pub fn capped_curse_effect(effects: ThreatCurseEffects, effect: FractionalCurseEffect) -> f64 {
    let (value, cap) = match effect {
        FractionalCurseEffect::CaveIn => (effects.cave_in_bonus, CAVE_IN_CURSE_CAP),
        FractionalCurseEffect::Cooldown => (effects.cooldown_penalty, COOLDOWN_CURSE_CAP),
    };
    value
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, cap)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FractionalCurseEffect {
    CaveIn,
    Cooldown,
}

/// Stamina runs before the curse; mana reduction runs after it, matching the
/// canonical Python ordering.
#[must_use]
pub fn cursed_cooldown(
    after_stamina_seconds: i64,
    effects: ThreatCurseEffects,
    mana_reduction_seconds: i64,
) -> i64 {
    let penalty = capped_curse_effect(effects, FractionalCurseEffect::Cooldown);
    ((after_stamina_seconds.max(1) as f64 * (1.0 + penalty)) as i64)
        .saturating_sub(mana_reduction_seconds.max(0))
        .max(1)
}

#[must_use]
pub fn cursed_cave_in_chance(base_chance: f64, effects: ThreatCurseEffects) -> f64 {
    base_chance + capped_curse_effect(effects, FractionalCurseEffect::CaveIn)
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

#[must_use]
pub fn build_threat_result_embed(resolution: &ThreatResolution) -> EmbedModel {
    let mut embed = EmbedModel {
        description: Some(resolution.message.clone()),
        ..EmbedModel::default()
    };
    let mut outcome = Vec::new();
    if resolution.depth_delta != 0 {
        outcome.push(format!(
            "{}{value} blocks",
            if resolution.depth_delta > 0 { "+" } else { "" },
            value = resolution.depth_delta,
        ));
    }
    if resolution.jc_delta != 0 {
        outcome.push(format!(
            "{}{value} JC",
            if resolution.jc_delta > 0 { "+" } else { "" },
            value = resolution.jc_delta,
        ));
    }
    if resolution.streak_loss > 0 {
        let noun = if resolution.streak_loss == 1 {
            "day"
        } else {
            "days"
        };
        outcome.push(format!("-{} streak {noun}", resolution.streak_loss));
    }
    if !outcome.is_empty() {
        embed.add_field("Outcome", outcome.join(" | "), false);
    }
    if let Some(curse) = resolution.curse_applied.as_ref() {
        let noun = if curse.duration_digs == 1 {
            "dig"
        } else {
            "digs"
        };
        embed.add_field(
            format!("Curse: {}", curse.name),
            format!(
                "A hex clings to you for the next {} {noun}.",
                curse.duration_digs
            ),
            false,
        );
    }
    if let Some(balance) = resolution.balance_after.filter(|balance| *balance < 0) {
        embed.add_field(
            "In Debt",
            format!("That cost dropped you to {balance} JC. You're in the red."),
            false,
        );
    }
    embed
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

#[must_use]
pub fn default_economy_scale() -> f64 {
    DEFAULT_MINIGAME_JC_DELTA_SCALE
}

#[cfg(test)]
#[path = "dig_event_threats/tests.rs"]
mod tests;
