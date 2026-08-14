//! Production runtime boundary for the Rust lift-and-shift.
//!
//! Domain/application code stays independent of the Discord SDK. This crate
//! owns process configuration, database admission, gateway supervision, and
//! conversion between Discord interactions and the typed registration API.

pub mod admin_match_correction;
pub mod admin_provider;
pub mod advstats_provider;
pub mod application_config;
pub mod ask_provider;
pub mod betting_provider;
pub mod blame_luke_provider;
pub mod config;
pub mod dig_bonus_runtime;
pub mod dig_provider;
pub mod dig_weather_worker;
pub mod discord_transport;
pub mod dota_info_provider;
pub mod draft_provider;
pub mod duel_challenges_worker;
pub mod duel_provider;
pub mod economy_events_worker;
pub mod enrichment_provider;
pub mod first_game_pool_worker;
pub mod gamba_guild_source;
pub mod gateway;
pub mod gateway_events;
pub mod global_hooks;
pub mod health;
pub mod info_provider;
pub mod inventory;
pub mod lobby_provider;
pub mod mafia_provider;
pub mod mana_provider;
pub mod manashop_debt_worker;
pub mod match_provider;
pub(crate) mod pet_death_delivery;
pub mod pet_provider;
pub mod pet_sweep_worker;
pub mod pin_helpers;
pub mod player_trivia_provider;
pub mod prediction_provider;
pub mod prediction_workers;
pub mod process_lock;
pub mod profile_provider;
pub mod rating_analysis_provider;
pub mod raw_reactions;
pub mod registration;
pub mod registration_provider;
pub mod reminder_provider;
pub mod scout_provider;
pub mod serenity_transport;
pub mod shop_provider;
pub mod survey_provider;
pub mod tax_provider;
pub mod trivia_provider;
pub mod vanity_tax_observer;
pub mod worker;
pub mod wrapped_provider;

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
pub use advstats_provider::AdvancedStatsRegistrationProvider;
pub use application_config::{
    ApplicationConfig, ChannelConfig, IdentityConfig, LlmConfig, Secret, Values, config_py_env_keys,
};
pub use ask_provider::AskRegistrationProvider;
pub use betting_provider::{
    BettingRegistrationProvider, BettingRuntimeConfig, BettingWagerRefreshPort,
    BettingWagerRefreshReport, match_post_match_debrief_port, match_wager_refresh_port,
};
pub use blame_luke_provider::BlameLukeRegistrationProvider;
pub use config::{ConfigError, DiscordToken, RuntimeConfig};
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
pub use dota_info_provider::DotaInfoRegistrationProvider;
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
    CompletedDatabaseAdmission, DatabaseAdmission, GatewaySessionEnd, GatewayTransport,
    LifecycleEvent, ReconnectPolicy, Runtime, RuntimeError, SqliteDatabaseAdmission,
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
pub use info_provider::InfoRegistrationProvider;
pub use lobby_provider::{
    ConfirmedLobbyJoin, LobbyJoinObserver, LobbyProviderBuildError, LobbyRegistrationProvider,
    LobbyRuntimeConfig, MatchActiveDraft, MatchLobbyPort, MatchLobbySnapshot,
};
pub use mafia_provider::{MafiaDiscordPort, MafiaRegistrationProvider};
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
pub use profile_provider::ProfileRegistrationProvider;
pub use rating_analysis_provider::{
    RatingAnalysisProviderBuildError, RatingAnalysisRegistrationProvider,
};
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
pub use scout_provider::{ScoutProviderBuildError, ScoutRegistrationProvider};
pub use serenity_transport::{SerenityDiscordTransport, SerenityGateway};
pub use shop_provider::{ShopDiscordPort, ShopProviderBuildError, ShopRegistrationProvider};
pub use survey_provider::{
    SURVEY_RECOVERY_WAKE_INTERVAL, SURVEY_RECOVERY_WORKER_NAME, SurveyDiscordPort, SurveyDmError,
    SurveyDmErrorKind, SurveyDmHistory, SurveyProviderBuildError, SurveyRegistrationProvider,
};
pub use tax_provider::TaxRegistrationProvider;
pub use trivia_provider::{TriviaDiscordPort, TriviaRegistrationProvider};
pub use vanity_tax_observer::VanityTaxGatewayObserver;
pub use worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};
pub use wrapped_provider::{
    WrappedDiscordPort, WrappedDiscordProfile, WrappedRegistrationProvider,
};
