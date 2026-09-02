//! Dig persistence repositories compiled independently from other database slices.

pub(crate) use cama_db_core::open_runtime_connection;
#[cfg(test)]
pub(crate) use cama_db_core::{expected_migrations, schema_manager};
#[cfg(test)]
pub(crate) use cama_db_match::core_repositories;

#[path = "../../cama-db/src/dig_abandon_runtime.rs"]
pub mod dig_abandon_runtime;
#[path = "../../cama-db/src/dig_action_history.rs"]
pub mod dig_action_history;
#[path = "../../cama-db/src/dig_bonus_events.rs"]
pub mod dig_bonus_events_repository;
#[path = "../../cama-db/src/dig_boss_runtime.rs"]
pub mod dig_boss_runtime;
#[path = "../../cama-db/src/dig_carry_wager.rs"]
pub mod dig_carry_wager;
#[path = "../../cama-db/src/dig_event_runtime.rs"]
pub mod dig_event_runtime;
#[path = "../../cama-db/src/dig_event_threats.rs"]
pub mod dig_event_threats;
#[path = "../../cama-db/src/dig_flavor_repository.rs"]
pub mod dig_flavor_repository;
#[path = "../../cama-db/src/dig_gear_runtime.rs"]
pub mod dig_gear_runtime;
#[path = "../../cama-db/src/dig_guild_modifiers.rs"]
pub mod dig_guild_modifiers;
#[path = "../../cama-db/src/dig_inventory.rs"]
pub mod dig_inventory_repository;
#[path = "../../cama-db/src/dig_migration_contracts.rs"]
pub mod dig_migration_contracts;
#[path = "../../cama-db/src/dig_miner_runtime.rs"]
pub mod dig_miner_runtime;
#[path = "../../cama-db/src/dig_new_events_repository.rs"]
pub mod dig_new_events_repository;
#[path = "../../cama-db/src/dig_prestige4_content.rs"]
pub mod dig_prestige4_content;
#[path = "../../cama-db/src/dig_prestige_runtime.rs"]
pub mod dig_prestige_runtime;
#[path = "../../cama-db/src/dig_quest_repository.rs"]
pub mod dig_quest_repository;
#[path = "../../cama-db/src/dig_relic_recycling.rs"]
pub mod dig_relic_recycling;
#[path = "../../cama-db/src/dig_relic_rework.rs"]
pub mod dig_relic_rework;
#[path = "../../cama-db/src/dig_routes.rs"]
pub mod dig_routes_repository;
#[path = "../../cama-db/src/dig_runtime_store.rs"]
pub mod dig_runtime_store;
#[path = "../../cama-db/src/dig_social_runtime.rs"]
pub mod dig_social_runtime;
#[path = "../../cama-db/src/dig_splash_runtime.rs"]
pub mod dig_splash_runtime;
#[path = "../../cama-db/src/dig_sweep_fixes.rs"]
pub mod dig_sweep_fixes_repository;
#[path = "../../cama-db/src/dig_tunnel_encounters.rs"]
pub mod dig_tunnel_encounters_repository;
#[path = "../../cama-db/src/dig_weather.rs"]
pub mod dig_weather;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-db/src/test_support.rs"]
mod test_support;
