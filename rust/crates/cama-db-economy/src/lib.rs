//! Match economy, draft, prediction, and settlement repositories.

pub(crate) use cama_db_core::open_runtime_connection;
#[cfg(test)]
pub(crate) use cama_db_core::schema_manager;
#[cfg(test)]
pub(crate) use cama_db_match::core_repositories;

#[path = "../../cama-db/src/autobet_investments.rs"]
pub mod autobet_investments;
#[path = "../../cama-db/src/betting_service.rs"]
pub mod betting_service_repository;
#[path = "../../cama-db/src/disbursement.rs"]
pub mod disbursement;
#[path = "../../cama-db/src/dota_bet_seed.rs"]
pub mod dota_bet_seed_repository;
#[path = "../../cama-db/src/dota_streak.rs"]
pub mod dota_streak_repository;
#[path = "../../cama-db/src/draft_finalization.rs"]
pub mod draft_finalization;
#[path = "../../cama-db/src/draft_financial_execution.rs"]
pub mod draft_financial_execution;
#[path = "../../cama-db/src/draft_financial_setup.rs"]
pub mod draft_financial_setup;
#[path = "../../cama-db/src/draft_state.rs"]
pub mod draft_state;
#[path = "../../cama-db/src/gambling_stats.rs"]
pub mod gambling_stats_repository;
#[path = "../../cama-db/src/loan.rs"]
pub mod loan_repository;
#[path = "../../cama-db/src/prediction_resolution.rs"]
pub mod prediction_resolution_repository;
#[path = "../../cama-db/src/prediction_workers.rs"]
pub mod prediction_worker_repository;
#[path = "../../cama-db/src/predictions.rs"]
pub mod predictions_repository;
#[path = "../../cama-db/src/tax.rs"]
pub mod tax_repository;
#[path = "../../cama-db/src/vanity_tax.rs"]
pub mod vanity_tax_repository;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-db/src/test_support.rs"]
mod test_support;
