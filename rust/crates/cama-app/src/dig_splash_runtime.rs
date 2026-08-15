//! Typed application orchestration for standalone Dig splash effects.
//!
//! Victim selection is read through the migrated DB projection, while mode
//! parsing, canonical scaling, fail-soft behavior, and game-date derivation
//! stay here.  The DB repository owns the immediate transaction that settles
//! all selected victims and appends the durable audit/protection rows.

use std::path::{Path, PathBuf};

use cama_db::dig_splash_runtime::{
    ACTIVE_DIGGERS_LOOKBACK_SECONDS, DigSplashRuntimeRepository, DigSplashRuntimeRepositoryError,
    SplashCandidateQuery, SplashMode, SplashSettlementRequest, SplashStrategy,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
};
use cama_domain::game_date::game_date_for_timestamp;
use thiserror::Error;

/// Request for one standalone splash resolution.
#[derive(Clone, Copy, Debug)]
pub struct DigSplashRequest<'a> {
    /// Guild whose player pool is queried.
    pub guild_id: i64,
    /// Digger/caster, always excluded from the victim pool.
    pub digger_id: i64,
    /// Event name retained in audit metadata.
    pub event_name: &'a str,
    /// Python strategy spelling; unknown values fail closed.
    pub strategy: &'a str,
    /// Number of victims requested.
    pub victim_count: usize,
    /// Authored per-victim amount before canonical mode scaling.
    pub penalty_jc: i64,
    /// Python mode spelling; unknown values fail closed.
    pub mode: &'a str,
    /// Stable prefix for per-victim idempotency keys.
    pub event_key_prefix: &'a str,
    /// Unix timestamp used for lookback, audit, and White protection.
    pub now: i64,
    /// Structural minigame scale, normally the deployment default of `1.0`.
    pub minigame_jc_delta_scale: f64,
    /// Optional daily-policy multiplier for grant mode.  This models the
    /// Python reward-adjuster seam without allowing a closure into persistence.
    pub grant_reward_multiplier: f64,
}

/// Required fields for constructing a splash request.  Keeping these inputs
/// in a named DTO avoids a positional, easy-to-misorder constructor while
/// leaving deployment defaults on [`DigSplashRequest`].
#[derive(Clone, Copy, Debug)]
pub struct DigSplashRequestSpec<'a> {
    /// Guild whose player pool is queried.
    pub guild_id: i64,
    /// Digger/caster, always excluded from the victim pool.
    pub digger_id: i64,
    /// Event name retained in audit metadata.
    pub event_name: &'a str,
    /// Python strategy spelling; unknown values fail closed.
    pub strategy: &'a str,
    /// Number of victims requested.
    pub victim_count: usize,
    /// Authored per-victim amount before canonical mode scaling.
    pub penalty_jc: i64,
    /// Python mode spelling; unknown values fail closed.
    pub mode: &'a str,
    /// Stable prefix for per-victim idempotency keys.
    pub event_key_prefix: &'a str,
    /// Unix timestamp used for lookback, audit, and White protection.
    pub now: i64,
}

impl<'a> DigSplashRequest<'a> {
    /// Construct a request with the default structural and grant scales.
    #[must_use]
    pub const fn from_spec(spec: DigSplashRequestSpec<'a>) -> Self {
        Self {
            guild_id: spec.guild_id,
            digger_id: spec.digger_id,
            event_name: spec.event_name,
            strategy: spec.strategy,
            victim_count: spec.victim_count,
            penalty_jc: spec.penalty_jc,
            mode: spec.mode,
            event_key_prefix: spec.event_key_prefix,
            now: spec.now,
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            grant_reward_multiplier: 1.0,
        }
    }
}

/// Application-facing result matching Python's `SplashResult` shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSplashResult {
    /// Strategy spelling copied from the request.
    pub strategy: String,
    /// Event name copied from the request.
    pub event_name: String,
    /// `(victim_id, actual_amount)` rows in settlement order.
    pub victims: Vec<(i64, i64)>,
    /// Sum of the actual amounts moved.
    pub total_burned: i64,
    /// Mode spelling copied from the request.
    pub mode: String,
    /// Total absorbed by White/other protection pools.
    pub absorbed_total: i64,
    /// Count of partially or fully protected victims.
    pub shielded_count: usize,
}

impl DigSplashResult {
    fn empty(request: DigSplashRequest<'_>) -> Self {
        Self {
            strategy: request.strategy.to_owned(),
            event_name: request.event_name.to_owned(),
            victims: Vec::new(),
            total_burned: 0,
            mode: request.mode.to_owned(),
            absorbed_total: 0,
            shielded_count: 0,
        }
    }
}

/// Application-level splash errors.  Invalid strategy/mode remains a normal
/// empty result, matching Python's fail-soft resolver contract.
#[derive(Debug, Error)]
pub enum DigSplashRuntimeError {
    /// The structural scale must be finite and positive.
    #[error("splash minigame scale must be finite and positive: {0}")]
    InvalidScale(f64),
    /// The stable idempotency key is missing.
    #[error(transparent)]
    Repository(#[from] DigSplashRuntimeRepositoryError),
    /// Game-date conversion failed for White protection.
    #[error("splash game date failed: {0}")]
    GameDate(String),
}

/// Existing-schema Dig splash service.
#[derive(Clone, Debug)]
pub struct DigSplashRuntimeService {
    path: PathBuf,
    repository: DigSplashRuntimeRepository,
}

impl DigSplashRuntimeService {
    /// Open the service against an already migrated SQLite database.
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            repository: DigSplashRuntimeRepository::new(&path),
            path,
        }
    }

    /// Resolve selection and settle one splash batch.
    ///
    /// Unknown strategy/mode, empty pools, non-positive counts, and
    /// non-positive authored amounts intentionally return an empty result.
    pub fn resolve(
        &self,
        request: DigSplashRequest<'_>,
    ) -> Result<DigSplashResult, DigSplashRuntimeError> {
        let Some(strategy) = SplashStrategy::parse(request.strategy) else {
            return Ok(DigSplashResult::empty(request));
        };
        let Some(mode) = SplashMode::parse(request.mode) else {
            return Ok(DigSplashResult::empty(request));
        };
        if request.victim_count == 0 || request.penalty_jc <= 0 {
            return Ok(DigSplashResult::empty(request));
        }
        if !request.minigame_jc_delta_scale.is_finite() || request.minigame_jc_delta_scale <= 0.0 {
            return Err(DigSplashRuntimeError::InvalidScale(
                request.minigame_jc_delta_scale,
            ));
        }
        let amount = scaled_amount(request, mode);
        if amount <= 0 {
            return Ok(DigSplashResult::empty(request));
        }
        let active_since = match strategy {
            SplashStrategy::RandomActive => request.now.saturating_sub(14 * 86_400),
            SplashStrategy::ActiveDiggers => {
                request.now.saturating_sub(ACTIVE_DIGGERS_LOOKBACK_SECONDS)
            }
            SplashStrategy::RichestN | SplashStrategy::DeepestN => 0,
        };
        let candidates = self.repository.candidate_ids(SplashCandidateQuery {
            guild_id: request.guild_id,
            digger_id: request.digger_id,
            strategy,
            victim_count: request.victim_count,
            active_since,
        })?;
        if candidates.is_empty() {
            return Ok(DigSplashResult::empty(request));
        }
        let game_date = game_date_for_timestamp(request.now as f64)
            .map_err(|error| DigSplashRuntimeError::GameDate(error.to_string()))?;
        let settlement = self.repository.settle_batch(&SplashSettlementRequest {
            guild_id: request.guild_id,
            digger_id: request.digger_id,
            event_name: request.event_name,
            strategy,
            mode,
            amount,
            victim_ids: &candidates,
            event_key_prefix: request.event_key_prefix,
            now: request.now,
            mana_date: &game_date,
        })?;
        Ok(DigSplashResult {
            strategy: request.strategy.to_owned(),
            event_name: request.event_name.to_owned(),
            victims: settlement
                .victims
                .into_iter()
                .map(|victim| (victim.victim_id, victim.amount))
                .collect(),
            total_burned: settlement.total_moved,
            mode: request.mode.to_owned(),
            absorbed_total: settlement.absorbed_total,
            shielded_count: settlement.shielded_count,
        })
    }

    /// Return the path used by the service for diagnostics and adapters.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn scaled_amount(request: DigSplashRequest<'_>, mode: SplashMode) -> i64 {
    match mode {
        SplashMode::Burn => scale_deflationary_minigame_jc_delta(
            strengthen_dig_event_penalty(request.penalty_jc) as f64,
            request.minigame_jc_delta_scale,
        ),
        SplashMode::Steal => {
            scale_minigame_jc_delta(request.penalty_jc as f64, request.minigame_jc_delta_scale)
        }
        SplashMode::Grant => {
            let gross =
                scale_minigame_jc_delta(request.penalty_jc as f64, request.minigame_jc_delta_scale);
            if !request.grant_reward_multiplier.is_finite() || request.grant_reward_multiplier < 0.0
            {
                return 0;
            }
            let adjusted = (gross as f64 * request.grant_reward_multiplier).round();
            if !adjusted.is_finite() || adjusted > i64::MAX as f64 {
                0
            } else {
                scale_positive_dig_jc(adjusted as i64)
            }
        }
    }
}

#[cfg(test)]
#[path = "dig_splash_runtime/tests.rs"]
mod tests;
