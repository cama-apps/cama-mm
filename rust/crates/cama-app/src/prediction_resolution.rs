//! Application orchestration for prediction expiry and resolution.
//!
//! Persistence stays behind a typed port so the Discord scheduler can supply
//! one clock value and delegate directly to the repository's atomic bulk-lock
//! operation. Settlement and rollback are similarly one-call operations; the
//! app layer never decomposes their transaction boundary.

use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::prediction_resolution_repository::{
    PredictionResolutionError, PredictionResolutionRepository, Resolution, RollbackResult,
    SettlementAdjustments,
};
use cama_db::predictions_repository::{ContractSide, NewLevel};

pub trait PredictionResolutionPort {
    type Error;

    fn lock_expired_predictions(&self, guild_id: i64, now: i64) -> Result<Vec<i64>, Self::Error>;

    fn settle_orderbook(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
        adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, Self::Error>;

    fn rollback_orderbook(
        &self,
        prediction_id: i64,
        guild_id: i64,
        levels: &[NewLevel],
        rolled_back_by: Option<i64>,
    ) -> Result<RollbackResult, Self::Error>;
}

impl PredictionResolutionPort for PredictionResolutionRepository {
    type Error = PredictionResolutionError;

    fn lock_expired_predictions(&self, guild_id: i64, now: i64) -> Result<Vec<i64>, Self::Error> {
        PredictionResolutionRepository::lock_expired_predictions(self, guild_id, now)
    }

    fn settle_orderbook(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
        adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, Self::Error> {
        PredictionResolutionRepository::settle_orderbook(
            self,
            prediction_id,
            outcome,
            resolved_by,
            adjustments,
        )
    }

    fn rollback_orderbook(
        &self,
        prediction_id: i64,
        guild_id: i64,
        levels: &[NewLevel],
        rolled_back_by: Option<i64>,
    ) -> Result<RollbackResult, Self::Error> {
        PredictionResolutionRepository::rollback_orderbook(
            self,
            prediction_id,
            guild_id,
            levels,
            rolled_back_by,
        )
    }
}

pub trait PredictionResolutionClock {
    fn now_seconds(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPredictionResolutionClock;

impl PredictionResolutionClock for SystemPredictionResolutionClock {
    fn now_seconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct PredictionResolutionService<R, C> {
    repository: R,
    clock: C,
}

impl<R, C> PredictionResolutionService<R, C>
where
    R: PredictionResolutionPort,
    C: PredictionResolutionClock,
{
    #[must_use]
    pub const fn new(repository: R, clock: C) -> Self {
        Self { repository, clock }
    }

    /// One clock read and one repository call; there is no list-then-update
    /// legacy path that could race another scheduler wake.
    pub fn check_and_lock_expired(&self, guild_id: i64) -> Result<Vec<i64>, R::Error> {
        self.repository
            .lock_expired_predictions(guild_id, self.clock.now_seconds())
    }

    pub fn resolve_orderbook(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        resolved_by: Option<i64>,
        adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, R::Error> {
        self.repository
            .settle_orderbook(prediction_id, outcome, resolved_by, adjustments)
    }

    pub fn rollback_orderbook(
        &self,
        prediction_id: i64,
        guild_id: i64,
        levels: &[NewLevel],
        rolled_back_by: Option<i64>,
    ) -> Result<RollbackResult, R::Error> {
        self.repository
            .rollback_orderbook(prediction_id, guild_id, levels, rolled_back_by)
    }
}

#[cfg(test)]
#[path = "prediction_resolution/tests.rs"]
mod tests;
