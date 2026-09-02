//! Independent digging foundations compiled beside the other app slices.

#[path = "../../cama-app/src/dig_assets.rs"]
pub mod dig_assets;
#[path = "../../cama-app/src/dig_bosses.rs"]
pub mod dig_bosses;
#[path = "../../cama-app/src/dig_command_views.rs"]
pub mod dig_command_views;
#[path = "../../cama-app/src/dig_event_threats.rs"]
pub mod dig_event_threats;
#[path = "../../cama-app/src/dig_gear_runtime.rs"]
pub mod dig_gear_runtime;
#[path = "../../cama-app/src/dig_inventory.rs"]
pub mod dig_inventory;
#[path = "../../cama-app/src/dig_leaderboard_runtime.rs"]
pub mod dig_leaderboard_runtime;
#[path = "../../cama-app/src/dig_loot.rs"]
pub mod dig_loot;
#[path = "../../cama-app/src/dig_new_events.rs"]
pub mod dig_new_events;
#[path = "../../cama-app/src/dig_quest_policy.rs"]
pub mod dig_quest_policy;
#[path = "../../cama-app/src/dig_relic_rework.rs"]
pub mod dig_relic_rework;
#[path = "../../cama-app/src/dig_routes.rs"]
pub mod dig_routes;
#[path = "../../cama-app/src/dig_service.rs"]
pub mod dig_service;
#[path = "../../cama-app/src/dig_splash_runtime.rs"]
pub mod dig_splash_runtime;
#[path = "../../cama-app/src/dig_sweep_fixes.rs"]
pub mod dig_sweep_fixes;
#[path = "../../cama-app/src/dig_tunnel_encounters.rs"]
pub mod dig_tunnel_encounters;
#[path = "../../cama-app/src/dig_tunnel_naming.rs"]
pub mod dig_tunnel_naming;
#[path = "../../cama-app/src/dig_tunnels.rs"]
pub mod dig_tunnels;
#[path = "../../cama-app/src/dig_view_supplements.rs"]
pub mod dig_view_supplements;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-app/src/test_support.rs"]
mod test_support;
