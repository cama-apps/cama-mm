//! Independent gameplay application services compiled beside the other app slices.

#[path = "../../cama-app/src/duel.rs"]
pub mod duel;
#[path = "../../cama-app/src/mafia_service.rs"]
pub mod mafia_service;
#[path = "../../cama-app/src/mana_effects_service.rs"]
pub mod mana_effects_service;
#[path = "../../cama-app/src/mana_service.rs"]
pub mod mana_service;
#[path = "../../cama-app/src/manashop_rework.rs"]
pub mod manashop_rework;
#[path = "../../cama-app/src/pet.rs"]
pub mod pet;
#[path = "../../cama-app/src/pet_assets.rs"]
pub mod pet_assets;
#[path = "../../cama-app/src/pet_brawl.rs"]
pub mod pet_brawl;
#[path = "../../cama-app/src/pet_commands.rs"]
pub mod pet_commands;
#[path = "../../cama-app/src/pet_eating.rs"]
pub mod pet_eating;
#[path = "../../cama-app/src/pet_evolution_app.rs"]
pub mod pet_evolution_app;
#[path = "../../cama-app/src/pet_flavor.rs"]
pub mod pet_flavor;
#[path = "../../cama-app/src/pet_sqlite.rs"]
pub mod pet_sqlite;
#[path = "../../cama-app/src/shop_commands.rs"]
pub mod shop_commands;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-app/src/test_support.rs"]
mod test_support;
