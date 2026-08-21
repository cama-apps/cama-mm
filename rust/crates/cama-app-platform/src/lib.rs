//! Independent platform application services compiled beside the other app slices.

#[path = "../../cama-app/src/admin_low_priority.rs"]
pub mod admin_low_priority;
#[path = "../../cama-app/src/blame_luke_media.rs"]
pub mod blame_luke_media;
#[path = "../../cama-app/src/bugfix_regressions.rs"]
pub mod bugfix_regressions;
#[path = "../../cama-app/src/channel_checks.rs"]
pub mod channel_checks;
#[path = "../../cama-app/src/embeds.rs"]
pub mod embeds;
#[path = "../../cama-app/src/font_assets.rs"]
pub mod font_assets;
#[path = "../../cama-app/src/moderation.rs"]
pub mod moderation;
#[path = "../../cama-app/src/player_mmr_fallback.rs"]
pub mod player_mmr_fallback;
#[path = "../../cama-app/src/player_trivia_service.rs"]
pub mod player_trivia_service;
#[path = "../../cama-app/src/playtime_service.rs"]
pub mod playtime_service;
#[path = "../../cama-app/src/python_random.rs"]
pub mod python_random;
#[path = "../../cama-app/src/referrals.rs"]
pub mod referrals;
#[path = "../../cama-app/src/registration.rs"]
pub mod registration;
#[path = "../../cama-app/src/reminders.rs"]
pub mod reminders;
#[path = "../../cama-app/src/survey_commands.rs"]
pub mod survey_commands;
#[path = "../../cama-app/src/timezone_service.rs"]
pub mod timezone_service;
#[path = "../../cama-app/src/trivia_questions.rs"]
pub mod trivia_questions;
#[path = "../../cama-app/src/unified_leaderboard.rs"]
pub mod unified_leaderboard;
#[path = "../../cama-app/src/wrapped_story.rs"]
pub mod wrapped_story;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../cama-app/src/test_support.rs"]
mod test_support;
