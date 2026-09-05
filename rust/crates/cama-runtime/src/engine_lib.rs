//! Production runtime engine compiled independently from leaf commands.
//!
//! The public `cama-runtime` crate is a compatibility facade over this engine,
//! the runtime core, and independently compiled command providers.

// Test-only source inclusion lets the core CI shard exercise production target
// tests without compiling separate binary, integration, and example harnesses.
#[cfg(all(test, feature = "runtime-test-core"))]
extern crate self as cama_runtime;

#[allow(dead_code)]
#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "../tests/runtime_lifecycle.rs"]
mod runtime_lifecycle_tests;

#[cfg(all(test, feature = "runtime-test-match"))]
mod architecture_tests;

pub mod admin_match_correction;
pub mod admin_provider;
pub(crate) use cama_runtime_core::ids;
pub use cama_runtime_core::{
    application_config, config, discord_transport, embed_colors, gateway_events, global_hooks,
    option_ext, raw_reactions, registration, runtime_ports,
};
pub mod betting_provider;
pub mod command_tree_contract;
pub mod curfew_sweep_worker;
pub mod dig_bonus_runtime;
pub mod dig_provider;
pub mod dig_weather_worker;
pub mod draft_provider;
pub mod duel_challenges_worker;
pub mod duel_provider;
pub mod economy_events_worker;
pub mod enrichment_provider;
pub mod first_game_pool_worker;
pub mod gamba_guild_source;
pub mod gateway;
pub mod health;
pub mod inventory;
pub mod lobby_provider;
pub mod mafia_provider;
pub mod mana_auto_assign_worker;
pub mod mana_provider;
pub mod manashop_debt_worker;
pub mod match_provider;
pub(crate) mod pet_death_delivery;
pub(crate) mod pet_flavor_runtime;
pub mod pet_provider;
pub mod pet_sweep_worker;
pub mod pin_helpers;
pub mod player_trivia_provider;
pub mod prediction_provider;
pub mod prediction_workers;
pub mod process_lock;
pub(crate) mod profit_deductions;
pub mod push_notification_provider;
pub mod registration_provider;
pub mod reminder_provider;
#[doc(hidden)]
pub mod runtime_cli;
pub mod serenity_transport;
pub mod shop_provider;
pub mod survey_provider;
pub mod trivia_provider;
pub mod vanity_tax_observer;
pub mod worker;
pub mod wrapped_provider;

#[cfg(test)]
pub(crate) mod test_support;

pub use admin_match_correction::{AdminMatchCorrectionBuildError, AdminMatchCorrectionRuntime};
pub use admin_provider::{
    AdminCommandSyncResult, AdminCorrectMatchBetResult, AdminCorrectMatchError,
    AdminCorrectMatchRequest, AdminCorrectMatchResult, AdminCorrectMatchSide, AdminDiscordControl,
    AdminDiscordHealth, AdminExtendBettingRequest, AdminExtendBettingResult, AdminFakeLobbyRequest,
    AdminFakeLobbyResult, AdminLobbyControl, AdminLobbyEjectionRequest, AdminLobbyEjectionResult,
    AdminLobbyScope, AdminMatchControl, AdminMatchCorrectionControl, AdminRegistrationProvider,
    AdminRuntimePorts, AdminSeedHeroGridRequest, AdminSeedHeroGridResult,
    CorrectionWinRewardControl, CorrectionWinRewardRequest, CorrectionWinRewardResult,
};
pub use application_config::{
    ApplicationConfig, ChannelConfig, IdentityConfig, LlmConfig, Secret, Values, config_py_env_keys,
};
pub use betting_provider::{
    BettingRegistrationProvider, BettingRuntimeConfig, BettingWagerRefreshPort,
    BettingWagerRefreshReport, match_post_match_debrief_port, match_wager_refresh_port,
};
pub use command_tree_contract::{
    CommandTreeContractError, CommandTreeSnapshot, snapshot_registry, validate_production_registry,
};
pub use config::{ConfigError, DiscordToken, RuntimeConfig};
pub use curfew_sweep_worker::{
    CURFEW_SWEEP_WAKE_INTERVAL, CURFEW_SWEEP_WORKER_NAME, CurfewLobbyDisplayPort,
    CurfewSweepWorker, curfew_sweep_worker_spec,
};
pub use dig_bonus_runtime::{
    BettingDigBonusWheelPort, DigBonusDiscordPort, DigBonusRewardSource, DigBonusRuntime,
    DigBonusRuntimeConfig, DigBonusRuntimeError, DigBonusSendFailure, DigBonusWheelFailure,
    DigBonusWheelPort, ResponderDigBonusMessagePort, SqliteDigBonusRewardSource,
};
pub use dig_provider::{
    DigChannelSnapshot, DigDiscordPort, DigProviderBuildError, DigPublicHistory,
    DigPublicHistoryMessage, DigRegistrationProvider,
};
pub use dig_weather_worker::{
    DIG_WEATHER_WAKE_INTERVAL, DIG_WEATHER_WORKER_NAME, DigWeatherWorker, dig_weather_worker_spec,
};
pub use draft_provider::{
    DraftNeonObserver, DraftNeonResult, DraftProviderBuildError, DraftRegistrationProvider,
    DraftReminderScheduler,
};
pub use duel_challenges_worker::{
    DUEL_CHALLENGES_WAKE_INTERVAL, DUEL_CHALLENGES_WORKER_NAME, DuelChallengesWorker,
    duel_challenges_worker_spec,
};
pub use duel_provider::DuelRegistrationProvider;
pub use economy_events_worker::{
    ECONOMY_EVENTS_WORKER_NAME, EconomyEventAnnouncementPort, EconomyEventsWorker,
    EconomyEventsWorkerConfig, economy_events_worker_spec,
};
pub use enrichment_provider::{
    EnrichmentProviderBuildError, EnrichmentRegistrationProvider, RecordedMatchDiscovery,
    RecordedMatchDiscoveryOutcome,
};
pub use first_game_pool_worker::{
    FIRST_GAME_POOL_REFRESH_RETRY_INTERVAL, FIRST_GAME_POOL_WAKE_INTERVAL,
    FIRST_GAME_POOL_WORKER_NAME, FirstGamePoolDisplayPort, FirstGamePoolGuildSource,
    FirstGamePoolWorker, first_game_pool_worker_spec,
};
pub use gamba_guild_source::{GambaDestination, GambaGuildSource};
pub use gateway::{
    DatabaseInitializationReport, GatewaySessionEnd, GatewayTransport, LifecycleEvent,
    ReconnectPolicy, Runtime, RuntimeError, SqliteDatabaseInitializer,
};
pub use gateway_events::{
    GatewayEventObserver, GatewayEventObservers, GatewayMember, GatewayObserverFailure,
    GuildMemberPageSource, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
pub use global_hooks::{GlobalInteractionHooks, UsageMonitor, UsageSnapshot};
pub use health::{
    DEFAULT_MAX_HEARTBEAT_AGE, HealthCheckReport, HealthError, HealthReporter, HealthSnapshot,
    HealthStatus, check_health, health_path,
};
pub use lobby_provider::{
    ConfirmedLobbyJoin, LiveLobbyService, LobbyJoinObserver, LobbyProviderBuildError,
    LobbyRegistrationProvider, LobbyRuntimeConfig, MatchActiveDraft, MatchLobbyPort,
    MatchLobbySnapshot, SqliteLobbyPlayers, SqlitePendingMatches,
};
pub use mafia_provider::{MafiaDiscordPort, MafiaRegistrationProvider};
pub use mana_auto_assign_worker::{
    MANA_AUTO_ASSIGN_WAKE_INTERVAL, MANA_AUTO_ASSIGN_WORKER_NAME, ManaAutoAssignWorker,
    mana_auto_assign_worker_spec,
};
pub use mana_provider::{ManaDiscordPort, ManaGuildMember, ManaRegistrationProvider};
pub use manashop_debt_worker::{
    MANASHOP_DEBT_WAKE_INTERVAL, MANASHOP_DEBT_WORKER_NAME, ManashopDebtWorker,
    manashop_debt_worker_spec,
};
pub use match_provider::{
    MatchBetSettlementParticipant, MatchBetSettlementRequest, MatchPostMatchDebriefPort,
    MatchPostMatchDebriefRequest, MatchProviderBuildError, MatchRegistrationProvider,
    MatchWagerRefreshReport,
};
pub use pet_provider::PetRegistrationProvider;
pub use pet_sweep_worker::{
    PET_SWEEP_WAKE_INTERVAL, PET_SWEEP_WORKER_NAME, PetSweepDeliveryError, PetSweepDiscordPort,
    PetSweepWorker, pet_sweep_worker_spec, pet_sweep_worker_spec_with_ai,
};
pub use pin_helpers::{PinManagementPort, safe_unpin_all_bot_messages, safe_unpin_message};
pub use player_trivia_provider::{PlayerTriviaDiscordPort, PlayerTriviaRegistrationProvider};
pub use prediction_provider::{
    PredictionBigWin, PredictionCommandConfig, PredictionCommandDiscordPort,
    PredictionMarketSurface, PredictionNeonConfig, PredictionNeonPort,
    PredictionRegistrationProvider, PredictionRuntimePorts, ProductionPredictionNeonPort,
};
pub use prediction_workers::{
    PREDICTION_DIGEST_WAKE_INTERVAL, PREDICTION_DIGEST_WORKER_NAME, PREDICTION_REFRESH_WORKER_NAME,
    PredictionDigestWorker, PredictionDiscordPort, PredictionRefreshWorker, PredictionWorkerConfig,
    prediction_digest_worker_spec, prediction_refresh_worker_spec,
};
pub use push_notification_provider::{PushNotificationHooks, PushNotificationRegistrationProvider};
pub use raw_reactions::{
    RawReactionEmoji, RawReactionEvent, RawReactionKind, RawReactionObserver,
    RawReactionObserverFailure, RawReactionObservers,
};
pub use registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAllowedMentions, InteractionAttachment, InteractionButton,
    InteractionButtonStyle, InteractionEmbed, InteractionEmbedField, InteractionHandler,
    InteractionHandlerError, InteractionMessageDelivery, InteractionMessageReceipt,
    InteractionModal, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionStringSelect, InteractionStringSelectOption,
    InteractionTextInput, InteractionTextInputStyle, InteractionValue, RegistrationError,
    RegistrationProvider, Registry, RegistryBuilder,
};
pub use registration_provider::{PlayerRegistrationConfig, PlayerRegistrationProvider};
pub use reminder_provider::{
    ReminderDeliveryReport, ReminderHooks, ReminderRecoveryObserver, ReminderRegistrationProvider,
};
pub use serenity_transport::{SerenityDiscordTransport, SerenityGateway};
pub use shop_provider::{ShopDiscordPort, ShopProviderBuildError, ShopRegistrationProvider};
pub use survey_provider::{
    SURVEY_RECOVERY_WAKE_INTERVAL, SURVEY_RECOVERY_WORKER_NAME, SurveyDiscordPort, SurveyDmError,
    SurveyDmErrorKind, SurveyDmHistory, SurveyEditError, SurveyEditErrorKind,
    SurveyProviderBuildError, SurveyRegistrationProvider,
};
pub use trivia_provider::{TriviaDiscordPort, TriviaRegistrationProvider};
pub use vanity_tax_observer::VanityTaxGatewayObserver;
pub use worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};
pub use wrapped_provider::{
    WrappedDiscordPort, WrappedDiscordProfile, WrappedRegistrationProvider,
};
