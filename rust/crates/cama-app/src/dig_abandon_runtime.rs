//! Canonical application orchestration for `/dig abandon`.
//!
//! Preview and settlement share the daily economy-event policy and positive
//! Dig reward scaling. The SQLite repository owns the guarded 24-hour check,
//! partial reset, wallet credit, ledger context, and audit transaction.

use std::path::{Path, PathBuf};

use cama_db::dig_abandon_runtime::{
    AtomicDigAbandonSettlement, DigAbandonKey, DigAbandonRuntimeRepository,
    DigAbandonRuntimeRepositoryError, DigAbandonSettlementOutcome, DigAbandonSnapshot,
};
use cama_domain::dig_economy::{DIG_POSITIVE_JC_MULTIPLIER, scale_positive_dig_jc};
use serde_json::Value;
use thiserror::Error;

use crate::dig_bosses::BOSS_BOUNDARIES;
use crate::dig_runtime::DigRuntimeConfig;
use crate::economy_event_sqlite::{SqliteEconomyEventError, SqliteEconomyEventService};

pub const ABANDON_MIN_DEPTH: i64 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigAbandonPreview {
    pub refund: i64,
    pub gross_refund: i64,
    pub current_depth: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigAbandonResult {
    pub depth_lost: i64,
    pub refund: i64,
    pub balance_after: i64,
    pub prestige_level: i64,
    pub audit_action_id: i64,
}

#[derive(Debug, Error)]
pub enum DigAbandonRuntimeError {
    #[error("You don't have a tunnel.")]
    MissingTunnel,
    #[error("The registered player row is missing.")]
    MissingPlayer,
    #[error("Tunnel must be at least 10 blocks deep to abandon.")]
    TooShallow,
    #[error("You can only abandon once every 24 hours.")]
    Cooldown,
    #[error("Your tunnel changed before abandonment could settle. Try again.")]
    Conflict,
    #[error("Stored abandon state is invalid: {0}")]
    InvalidState(String),
    #[error(transparent)]
    Repository(#[from] DigAbandonRuntimeRepositoryError),
    #[error(transparent)]
    Economy(#[from] SqliteEconomyEventError),
}

#[derive(Clone, Debug)]
pub struct DigAbandonRuntimeService {
    path: PathBuf,
    repository: DigAbandonRuntimeRepository,
    config: DigRuntimeConfig,
}

impl DigAbandonRuntimeService {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        Self::sqlite_with_config(path, DigRuntimeConfig::default())
    }

    #[must_use]
    pub fn sqlite_with_config(path: impl AsRef<Path>, config: DigRuntimeConfig) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            repository: DigAbandonRuntimeRepository::new(&path),
            path,
            config,
        }
    }

    pub fn preview_at(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigAbandonPreview, DigAbandonRuntimeError> {
        let snapshot = self.snapshot(discord_id, guild_id)?;
        preview_from_snapshot(&self.path, &self.config, snapshot, now)
    }

    pub fn abandon(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigAbandonResult, DigAbandonRuntimeError> {
        let snapshot = self.snapshot(discord_id, guild_id)?;
        let preview = preview_from_snapshot(&self.path, &self.config, snapshot, now)?;
        let boss_progress_json = active_boss_progress_json()?;
        let detail_json = serde_json::json!({
            "depth": preview.current_depth,
            "refund": preview.refund,
            "gross_jc": preview.gross_refund,
            "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
        })
        .to_string();
        let outcome = self.repository.atomic_settle(AtomicDigAbandonSettlement {
            expected: snapshot,
            refund: preview.refund,
            boss_progress_json: &boss_progress_json,
            detail_json: &detail_json,
            created_at: now,
        })?;
        match outcome {
            DigAbandonSettlementOutcome::Applied(receipt) => Ok(DigAbandonResult {
                depth_lost: receipt.depth_lost,
                refund: preview.refund,
                balance_after: receipt.balance_after,
                prestige_level: receipt.prestige_level,
                audit_action_id: receipt.audit_action_id,
            }),
            DigAbandonSettlementOutcome::Conflict => Err(DigAbandonRuntimeError::Conflict),
            DigAbandonSettlementOutcome::Cooldown => Err(DigAbandonRuntimeError::Cooldown),
            DigAbandonSettlementOutcome::TooShallow => Err(DigAbandonRuntimeError::TooShallow),
            DigAbandonSettlementOutcome::MissingTunnel => {
                Err(DigAbandonRuntimeError::MissingTunnel)
            }
            DigAbandonSettlementOutcome::MissingPlayer => {
                Err(DigAbandonRuntimeError::MissingPlayer)
            }
        }
    }

    fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigAbandonSnapshot, DigAbandonRuntimeError> {
        self.repository
            .snapshot(DigAbandonKey {
                discord_id,
                guild_id,
            })?
            .ok_or(DigAbandonRuntimeError::MissingTunnel)
    }
}

fn preview_from_snapshot(
    path: &Path,
    config: &DigRuntimeConfig,
    snapshot: DigAbandonSnapshot,
    now: i64,
) -> Result<DigAbandonPreview, DigAbandonRuntimeError> {
    if snapshot.depth < ABANDON_MIN_DEPTH {
        return Err(DigAbandonRuntimeError::TooShallow);
    }
    let gross_refund = snapshot.depth / 10;
    let economy = SqliteEconomyEventService::new(path, config.economy_event.clone());
    let event_refund = economy.adjust_reward_at(snapshot.key.guild_id, gross_refund, now)?;
    Ok(DigAbandonPreview {
        refund: scale_positive_dig_jc(event_refund),
        gross_refund,
        current_depth: snapshot.depth,
    })
}

fn active_boss_progress_json() -> Result<String, DigAbandonRuntimeError> {
    let progress = BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| (boundary.to_string(), Value::String("active".to_owned())))
        .collect::<serde_json::Map<_, _>>();
    serde_json::to_string(&progress)
        .map_err(|error| DigAbandonRuntimeError::InvalidState(error.to_string()))
}

#[cfg(test)]
#[path = "dig_abandon_runtime/tests.rs"]
mod tests;
