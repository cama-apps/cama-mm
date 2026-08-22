//! Player, matchmaking, recording, and rating persistence repositories.

#[cfg(test)]
pub(crate) use cama_db_core::schema_manager;
pub(crate) use cama_db_core::{json_numeric, open_runtime_connection};

#[path = "../../cama-db/src/core_repositories.rs"]
pub mod core_repositories;
#[path = "../../cama-db/src/match_correction.rs"]
pub mod match_correction_repository;
#[path = "../../cama-db/src/match_discovery.rs"]
pub mod match_discovery_repository;
#[path = "../../cama-db/src/match_recording.rs"]
pub mod match_recording_repository;
#[path = "../../cama-db/src/match_runtime.rs"]
pub mod match_runtime;
#[path = "../../cama-db/src/match_voting.rs"]
pub mod match_voting;
#[path = "../../cama-db/src/pairings.rs"]
pub mod pairings_repository;
#[path = "../../cama-db/src/pending_lobby.rs"]
pub mod pending_lobby;
#[path = "../../cama-db/src/rating_analysis.rs"]
pub mod rating_analysis;
#[path = "../../cama-db/src/rating_history.rs"]
pub mod rating_history_repository;
#[path = "../../cama-db/src/readycheck.rs"]
pub mod readycheck_repository;
#[path = "../../cama-db/src/referrals.rs"]
pub mod referrals;
#[path = "../../cama-db/src/scout.rs"]
pub mod scout_repository;
#[path = "../../cama-db/src/soft_avoid.rs"]
pub mod soft_avoid_repository;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-db/src/test_support.rs"]
mod test_support;
