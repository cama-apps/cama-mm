//! Compatibility facade for independently compiled database repository slices.

pub use cama_db_core::*;
#[cfg(feature = "dig")]
pub use cama_db_dig::*;
#[cfg(feature = "economy")]
pub use cama_db_economy::*;
#[cfg(feature = "gameplay")]
pub use cama_db_gameplay::*;
#[cfg(feature = "match")]
pub use cama_db_match::*;
#[cfg(feature = "platform")]
pub use cama_db_platform::*;
