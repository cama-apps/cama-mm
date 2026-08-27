//! Administration, identity, configuration, and utility repositories.

#[cfg(test)]
pub(crate) use cama_db_core::schema_manager;
pub(crate) use cama_db_core::{json_numeric, open_runtime_connection};

#[path = "../../cama-db/src/admin.rs"]
pub mod admin;
#[path = "../../cama-db/src/blame_luke.rs"]
pub mod blame_luke;
#[path = "../../cama-db/src/curfew.rs"]
pub mod curfew;
#[path = "../../cama-db/src/guild_config.rs"]
pub mod guild_config_repository;
#[path = "../../cama-db/src/herogrid.rs"]
pub mod herogrid_repository;
#[path = "../../cama-db/src/llm_request.rs"]
pub mod llm_request;
#[path = "../../cama-db/src/low_priority.rs"]
pub mod low_priority_repository;
#[path = "../../cama-db/src/moderation.rs"]
pub mod moderation;
#[path = "../../cama-db/src/neon_events.rs"]
pub mod neon_events;
#[path = "../../cama-db/src/notifications.rs"]
pub mod notifications;
#[path = "../../cama-db/src/opendota_player.rs"]
pub mod opendota_player;
#[path = "../../cama-db/src/player_trivia.rs"]
pub mod player_trivia;
#[path = "../../cama-db/src/push_notifications.rs"]
pub mod push_notifications;
#[path = "../../cama-db/src/registration.rs"]
pub mod registration_repository;
#[path = "../../cama-db/src/reminders.rs"]
pub mod reminder_repository;
#[path = "../../cama-db/src/survey.rs"]
pub mod survey;
#[path = "../../cama-db/src/wrapped_live.rs"]
pub mod wrapped_live;
#[path = "../../cama-db/src/wrapped.rs"]
pub mod wrapped_repository;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-db/src/test_support.rs"]
mod test_support;
