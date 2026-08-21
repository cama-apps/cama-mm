//! Independent match and economy application services compiled beside the other app slices.

#[path = "../../cama-app/src/betting_validation.rs"]
pub mod betting_validation;
#[path = "../../cama-app/src/disburse_actions.rs"]
pub mod disburse_actions;
#[path = "../../cama-app/src/disburse_service.rs"]
pub mod disburse_service;
#[path = "../../cama-app/src/dota_info.rs"]
pub mod dota_info;
#[path = "../../cama-app/src/draft.rs"]
pub mod draft;
#[path = "../../cama-app/src/economy_actions.rs"]
pub mod economy_actions;
#[path = "../../cama-app/src/loan.rs"]
pub mod loan;
#[path = "../../cama-app/src/match_recording.rs"]
pub mod match_recording;
#[path = "../../cama-app/src/match_voting_service.rs"]
pub mod match_voting_service;
#[path = "../../cama-app/src/prediction_resolution.rs"]
pub mod prediction_resolution;
#[path = "../../cama-app/src/predictions.rs"]
pub mod predictions;
#[path = "../../cama-app/src/rating_comparison_service.rs"]
pub mod rating_comparison_service;
#[path = "../../cama-app/src/tax_commands.rs"]
pub mod tax_commands;
#[path = "../../cama-app/src/vanity_tax_service.rs"]
pub mod vanity_tax_service;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-app/src/test_support.rs"]
mod test_support;
