//! Compatibility facade for the independently compiled runtime crates.
//!
//! Production composition continues to depend on `cama-runtime`; Cargo can
//! compile the engine and leaf command providers in parallel behind this API.

#[doc(hidden)]
pub use cama_runtime_commands::herogrid_provider;
pub use cama_runtime_commands::{
    advstats_provider, ask_provider, blame_luke_provider, dota_info_provider, info_provider,
    profile_provider, rating_analysis_provider, scout_provider, tax_provider,
};
pub use cama_runtime_engine::*;

pub use advstats_provider::AdvancedStatsRegistrationProvider;
pub use ask_provider::AskRegistrationProvider;
pub use blame_luke_provider::BlameLukeRegistrationProvider;
pub use dota_info_provider::DotaInfoRegistrationProvider;
pub use info_provider::{
    InfoAnalyticsSnapshot, InfoRegistrationProvider, read_info_analytics_snapshot,
};
pub use profile_provider::{
    ProfileBalanceHistorySnapshot, ProfileRegistrationProvider, read_balance_history_snapshot,
};
pub use rating_analysis_provider::{
    RatingAnalysisProviderBuildError, RatingAnalysisRegistrationProvider,
};
pub use scout_provider::{ScoutProviderBuildError, ScoutRegistrationProvider};
pub use tax_provider::TaxRegistrationProvider;
