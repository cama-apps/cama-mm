//! Gameplay, pet, shop, and community persistence repositories.

pub(crate) use cama_db_core::open_runtime_connection;
#[cfg(test)]
pub(crate) use cama_db_core::schema_manager;
#[cfg(test)]
pub(crate) use cama_db_match::core_repositories;

#[path = "../../cama-db/src/bankruptcy.rs"]
pub mod bankruptcy_repository;
#[path = "../../cama-db/src/dig_blood_pact.rs"]
pub mod dig_blood_pact;
#[path = "../../cama-db/src/duel.rs"]
pub mod duel_repository;
#[path = "../../cama-db/src/economy_events.rs"]
pub mod economy_event_repository;
#[path = "../../cama-db/src/golden_wheel_repository.rs"]
pub mod golden_wheel_repository;
#[path = "../../cama-db/src/mafia_repository.rs"]
pub mod mafia_repository;
#[path = "../../cama-db/src/mana_protection.rs"]
pub mod mana_protection;
#[path = "../../cama-db/src/mana_service.rs"]
pub mod mana_service_repository;
#[path = "../../cama-db/src/manashop_rework.rs"]
pub mod manashop_rework_repository;
#[path = "../../cama-db/src/package_deal.rs"]
pub mod package_deal_repository;
#[path = "../../cama-db/src/pet_brawl.rs"]
pub mod pet_brawl_repository;
#[path = "../../cama-db/src/pet_eating.rs"]
pub mod pet_eating_repository;
#[path = "../../cama-db/src/pet_evolution.rs"]
pub mod pet_evolution_repository;
#[path = "../../cama-db/src/pet_migration_contracts.rs"]
pub mod pet_migration_contracts;
#[path = "../../cama-db/src/pet.rs"]
pub mod pet_repository;
#[path = "../../cama-db/src/shop_runtime.rs"]
pub mod shop_runtime;
#[path = "../../cama-db/src/tip.rs"]
pub mod tip_repository;
#[path = "../../cama-db/src/trivia_commands.rs"]
pub mod trivia_commands_repository;
#[path = "../../cama-db/src/wheel_spins.rs"]
pub mod wheel_spin_repository;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-db/src/test_support.rs"]
mod test_support;
