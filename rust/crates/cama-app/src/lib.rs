//! Application orchestration for the Rust lift-and-shift runtime.
//!
//! This layer coordinates pure domain policies and Python-compatible SQLite
//! repositories. Discord transport, process startup, and schema migration stay
//! outside the crate so behavior can be tested without a live gateway or a
//! second migration authority.

pub use cama_app_dig::{
    dig_bosses, dig_command_views, dig_event_threats, dig_service, dig_splash_runtime,
    dig_sweep_fixes, dig_tunnel_naming, dig_view_supplements,
};
pub use cama_app_gameplay::{
    duel, mafia_service, mana_effects_service, mana_service, manashop_rework, pet, pet_assets,
    pet_brawl, pet_commands, pet_eating, pet_evolution_app, pet_flavor, pet_sqlite, shop_commands,
};
pub use cama_app_match::{
    betting_validation, disburse_actions, disburse_service, dota_info, draft, economy_actions,
    loan, match_recording, match_voting_service, prediction_resolution, predictions,
    rating_comparison_service, tax_commands, vanity_tax_service,
};
pub use cama_app_platform::{
    admin_low_priority, blame_luke_media, bugfix_regressions, channel_checks, embeds, font_assets,
    moderation, player_mmr_fallback, player_trivia_service, playtime_service, python_random,
    referrals, registration, reminders, survey_commands, timezone_service, trivia_questions,
    unified_leaderboard, wrapped_story,
};

pub mod admin_moderation_commands;
pub mod ai_http;
pub mod ai_query_sqlite;
pub mod ai_services;
pub mod ask;
pub mod balance_history_service;
pub mod bankruptcy;
pub mod bankruptcy_buffs;
pub mod betting_reminder_messaging;
pub mod betting_service;
pub mod boss_duel;
pub mod boss_encounter_view_guard;
pub mod boss_multi_tier;
pub mod bot_tasks;
pub mod curfew_service;
pub mod dedicated_lobby_channel;
pub mod dig_abandon_runtime;
pub mod dig_artifact_catalog_runtime;
pub mod dig_assets;
pub mod dig_bonus_events;
pub mod dig_boss_runtime;
pub mod dig_carry_wager;
pub mod dig_event_runtime;
pub mod dig_flavor;
pub mod dig_gear_runtime;
pub mod dig_inventory;
pub mod dig_leaderboard_runtime;
pub mod dig_loot;
pub mod dig_media_runtime;
pub mod dig_miner_runtime;
pub mod dig_neon;
pub mod dig_new_events;
pub mod dig_prestige4_content;
pub mod dig_prestige_runtime;
#[cfg(test)]
mod dig_pure_policy_admission_tests;
pub mod dig_quest_policy;
pub mod dig_relic_recycling;
pub mod dig_relic_rework;
pub mod dig_routes;
pub mod dig_runtime;
pub mod dig_social_runtime;
pub mod dig_tunnel_encounters;
pub mod dig_tunnels;
pub mod dota_bet_seed;
pub mod dota_streak;
pub mod dotabase_sqlite;
pub mod drawing;
pub mod duel_flavor;
pub mod economy_event_service;
pub mod economy_event_sqlite;
pub mod golden_wheel;
pub mod hero_lookup;
pub mod herogrid;
pub mod interaction_safety;
pub mod jopat_match_routing;
pub mod jopat_post_match;
pub mod lobby_commands_guild_id;
pub mod lobby_manager;
pub mod lobby_runtime;
pub mod lobby_service;
pub mod mafia_flavor;
pub mod match_discovery;
pub mod neon_bigwin_media;
pub mod neon_degen;
pub mod opendota_http;
pub mod opendota_player_service;
pub mod pet_brawl_commands;
pub mod post_match_gif_media;
pub mod rating_analysis_command;
pub mod rating_analysis_media;
pub mod readycheck;
pub mod reminder_sqlite;
pub mod scout;
pub mod service_container;
#[cfg(test)]
mod test_support;
pub mod trivia_commands;
pub mod trivia_data;
pub mod trivia_image_cache;
pub mod wheel;
pub mod wrapped_media;
