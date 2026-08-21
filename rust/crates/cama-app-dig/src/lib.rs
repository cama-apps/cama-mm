//! Independent digging foundations compiled beside the other app slices.

#[path = "../../cama-app/src/dig_bosses.rs"]
pub mod dig_bosses;
#[path = "../../cama-app/src/dig_command_views.rs"]
pub mod dig_command_views;
#[path = "../../cama-app/src/dig_event_threats.rs"]
pub mod dig_event_threats;
#[path = "../../cama-app/src/dig_service.rs"]
pub mod dig_service;
#[path = "../../cama-app/src/dig_splash_runtime.rs"]
pub mod dig_splash_runtime;
#[path = "../../cama-app/src/dig_sweep_fixes.rs"]
pub mod dig_sweep_fixes;
#[path = "../../cama-app/src/dig_tunnel_naming.rs"]
pub mod dig_tunnel_naming;
#[path = "../../cama-app/src/dig_view_supplements.rs"]
pub mod dig_view_supplements;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-app/src/test_support.rs"]
mod test_support;
