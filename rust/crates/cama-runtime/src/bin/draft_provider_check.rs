mod application_config {
    pub use cama_runtime::application_config::*;
}
mod discord_transport {
    pub use cama_runtime::discord_transport::*;
}
mod gateway_events {
    pub use cama_runtime::gateway_events::*;
}
mod lobby_provider {
    pub use cama_runtime::lobby_provider::*;
}
mod match_provider {
    pub use cama_runtime::match_provider::*;
}
mod registration {
    pub use cama_runtime::registration::*;
}

// Keep the source-inclusion checker as a second compile-closed target. Cargo
// does not run its inherited unit tests because they already run through the
// library target, but check/clippy still compile this independent inclusion.
#[allow(dead_code)]
#[path = "../draft_provider.rs"]
mod draft_provider_check_impl;

fn main() {
    let _ = std::any::type_name::<cama_runtime::DraftRegistrationProvider>();
}
