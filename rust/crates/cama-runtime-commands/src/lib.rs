//! Independent leaf command providers compiled beside the remaining runtime.

#[cfg(all(test, feature = "runtime-test-core"))]
extern crate self as cama_runtime;

#[allow(dead_code)]
#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "../../cama-runtime/examples/profile_snapshot_smoke.rs"]
mod profile_snapshot_smoke_tests;

pub use cama_runtime_core::*;

#[path = "../../cama-runtime/src/advstats_provider.rs"]
pub mod advstats_provider;
#[path = "../../cama-runtime/src/ask_provider.rs"]
pub mod ask_provider;
#[path = "../../cama-runtime/src/blame_luke_provider.rs"]
pub mod blame_luke_provider;
#[path = "../../cama-runtime/src/dota_info_provider.rs"]
pub mod dota_info_provider;
#[path = "../../cama-runtime/src/herogrid_provider.rs"]
pub mod herogrid_provider;
#[path = "../../cama-runtime/src/info_provider.rs"]
pub mod info_provider;
#[path = "../../cama-runtime/src/profile_provider.rs"]
pub mod profile_provider;
#[path = "../../cama-runtime/src/rating_analysis_provider.rs"]
pub mod rating_analysis_provider;
#[path = "../../cama-runtime/src/scout_provider.rs"]
pub mod scout_provider;
#[path = "../../cama-runtime/src/tax_provider.rs"]
pub mod tax_provider;

pub use info_provider::read_info_analytics_snapshot;
pub use profile_provider::read_balance_history_snapshot;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-runtime/src/test_support.rs"]
mod test_support;
