//! Production Discord provider for Python's complete `/dig` extension.
//!
//! The legacy bot keeps the Dig cog deliberately close to Discord.  The Rust
//! boundary keeps that surface typed: nested slash-command registration,
//! channel admission, autocomplete, and persistent component dispatch live in
//! this crate while the migrated Dig rules/repositories remain transport
//! independent.  SQLite work is always moved to Tokio's blocking pool and
//! all money-moving seams use a single transaction.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::AIService;
use cama_app::boss_duel::RiskTier;
use cama_app::boss_multi_tier::{EntropyPort, PausedBossDuel, ResolvedFight};
use cama_app::dig_abandon_runtime::DigAbandonRuntimeService;
use cama_app::dig_assets::{BossIdentity, BossScene};
use cama_app::dig_boss_runtime::{
    DigBossBrokenGear, DigBossEncounterInfo, DigBossGearDrop, DigBossPrestigeRelicDrop,
    DigBossResolvedOutcome, DigBossRetreatOutcome, DigBossRuntimeConfig, DigBossRuntimeRequest,
    DigBossRuntimeResult as DigBossCallResult, DigBossRuntimeService, DigBossScoutOutcome,
    DigBossStartOutcome, DigPinnacleResolved,
};
use cama_app::dig_bosses::{
    PINNACLE_DEPTH, PINNACLE_SECRET_PHASE_CHANCE, luminosity_combat_penalty, pinnacle_by_id,
};
use cama_app::dig_event_runtime::{
    DigEventDeliveryContext, DigEventDeliveryMarkRequest, DigEventDeliverySnapshot,
    DigEventPendingDeliveryQuery,
};
use cama_app::dig_flavor::{
    AIServiceDigFlavorAiPort, ArtifactFlavorInfo, BossFlavorInfo, CATASTROPHIC_LINES,
    DigFlavorAiPort, DigFlavorDataPort, DigFlavorOutcome, DigFlavorRequest, DigFlavorService,
    DigToolCallResult, EligibleEvent, FlavorDeliveryReceipt, FlavorDeliveryState,
    SeededDigFlavorRng, SqliteDigFlavorConfigAdapter, SqliteDigFlavorContextAdapter,
    SqliteDigFlavorDataAdapter,
};
use cama_app::dig_inventory::DigInventoryService;
use cama_app::dig_media_runtime::DigMediaRuntime;
use cama_app::dig_miner_runtime::{DigMinerProfile, DigMinerRuntimeService};
use cama_app::dig_neon::{
    BossVictory, DigNeonCooldownPort, DigNeonService, RelicFound, SeededDigNeonRandom,
};
use cama_app::dig_prestige_runtime::{
    DigPrestigePreview, DigPrestigeRequest, DigPrestigeResult, DigPrestigeRuntimeService,
};
use cama_app::dig_routes::{parse_route_state, route_by_id};
use cama_app::dig_runtime::{
    DigAdminMutationOutcome, DigRuntimeBloodPactSnapshot, DigRuntimeConfig,
    DigRuntimeDeliveryContext, DigRuntimeDeliveryPart, DigRuntimeDeliverySnapshot,
    DigRuntimeExecution, DigRuntimeFinalizeDelivery, DigRuntimeFlavorSnapshot, DigRuntimeFlexData,
    DigRuntimeMarkDelivered, DigRuntimePendingDeliveryQuery, DigRuntimeRebindDeliveryChannel,
    DigRuntimeRenderKind, DigRuntimeSettleBloodPact, DigWeatherEffects,
};
use cama_app::dig_service::{PICKAXE_TIERS, layer_at};
use cama_app::dig_social_runtime::DigSocialRuntimeService;
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::core_repositories::PlayerRepository;
use cama_db::dig_inventory_repository::DigInventoryRepository;
use cama_db::dig_miner_runtime::{DigMinerAllocation, DigMinerAutoBuyUpdate};
use cama_db::neon_events::NeonEventRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::pet_evolution::PetActivity;
use tracing::warn;

use crate::application_config::ApplicationConfig;
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionAcknowledgementPolicy, InteractionActionRow, InteractionAttachment,
    InteractionButton, InteractionButtonStyle, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionMessageReceipt, InteractionModal, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionStringSelect,
    InteractionStringSelectOption, InteractionTextInput, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};
use crate::reminder_provider::ReminderHooks;

const COMPONENT_PREFIX: &str = "dig:";
const ROUTE_COMPONENT_PREFIX: &str = "dig_route_";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const REGISTER_FIRST_MESSAGE: &str = "You must be registered first. Use `/player register`.";
const WRONG_CHANNEL_PENALTY: i64 = 1;
const COMMAND_RATE_LIMIT: usize = 2;
const COMMAND_RATE_WINDOW: Duration = Duration::from_secs(30);
const ARTIFACT_RATE_LIMIT: usize = 1;
const ARTIFACT_RATE_WINDOW: Duration = Duration::from_secs(10);
/// How long an idle rate-limit bucket survives before the map drops it.
/// Must exceed every rate window above so an active bucket is never pruned.
const RATE_LIMIT_BUCKET_RETENTION: Duration = Duration::from_secs(300);
const ABANDON_VIEW_TIMEOUT_SECONDS: i64 = 60;
const PRESTIGE_VIEW_TIMEOUT_SECONDS: i64 = 60;
const SABOTAGE_VIEW_TIMEOUT_SECONDS: i64 = 30;
const GUIDE_VIEW_TIMEOUT_SECONDS: i64 = 180;
const PAID_VIEW_TIMEOUT_SECONDS: i64 = 60;
const ROUTE_VIEW_TIMEOUT_SECONDS: i64 = 180;
const DELIVERY_RECEIPT_GRACE_SECONDS: i64 = 30;
const DELIVERY_RECEIPT_SCAN_LIMIT: usize = 500;
const FLEX_ROASTS: [&str; 8] = [
    "Dug once, found nothing but regret.",
    "The tunnel is so shallow a worm filed a noise complaint.",
    "Achievement unlocked: Owning a shovel.",
    "Your tunnel has more cobwebs than depth.",
    "Even the dirt feels sorry for you.",
    "Depth: yes. Impressive: no.",
    "The mine safety inspector gave you a participation trophy.",
    "Your pickaxe is still in the shrinkwrap.",
];
const INVENTORY_LIMIT: usize = cama_app::dig_loot::MAX_INVENTORY_SLOTS;
const MAX_AUTOCOMPLETE_CHOICES: usize = 25;
const PUBLIC_COLOR: u32 = 0x58_65_F2;
const GOLD_COLOR: u32 = 0xD4_AF_37;
const FLEX_COLOR: u32 = 0xFF_D7_00;
const ERROR_COLOR: u32 = 0xED_42_45;
type DigRateKey = (i64, i64, &'static str);
type DigRateHits = VecDeque<Instant>;

/// Failure classification for the configured-channel outbox.
///
/// A rejected send with a clean, empty history is safe to fall back to the
/// interaction response.  Any history/CAS failure after an attempted send is
/// ambiguous: Discord may already have accepted the message, so publishing a
/// second response would violate the durable per-part nonce contract.
#[derive(Debug)]
enum DigDeliveryFailure {
    SafeFallback {
        part: DigRuntimeDeliveryPart,
        error: String,
    },
    Ambiguous(String),
}

#[derive(Debug)]
enum DigEventDeliveryFailure {
    Rejected(String),
    Ambiguous(String),
}

#[derive(Clone, Debug)]
enum DigNeonPost {
    Cave { block_loss: i64 },
    Relic { name: String, rarity: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DigBossNeonVictory {
    boss_name: String,
    boundary: i64,
    layer_name: String,
    jc_delta: i64,
    gear_drop: bool,
    trophy_relic_drop: bool,
}

/// Whether a nonce-addressed Discord send was definitely rejected or may
/// have been accepted before the transport failed. Only a definite rejection
/// is safe to redirect to the interaction channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigPublicSendFailureKind {
    Rejected,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPublicSendFailure {
    pub kind: DigPublicSendFailureKind,
    pub message: String,
}

impl DigPublicSendFailure {
    #[must_use]
    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: DigPublicSendFailureKind::Rejected,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self {
            kind: DigPublicSendFailureKind::Ambiguous,
            message: message.into(),
        }
    }
}

/// A narrow Discord seam for `/dig`'s channel and media policy.
///
/// The provider intentionally does not retain Serenity objects.  The live
/// adapter resolves channels from the cache/HTTP boundary and tests can model
/// the Python `require_dig_channel` fallback without a Discord client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigChannelSnapshot {
    pub id: i64,
    pub guild_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub can_send: bool,
    pub is_text: bool,
}

/// One public message returned by Discord's bounded channel-history lookup.
/// Discord preserves a message nonce in the HTTP message model, so receipt
/// recovery does not need to leak an internal marker into user-visible copy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPublicHistoryMessage {
    pub message_id: u64,
    pub author_id: u64,
    pub nonce: Option<String>,
    /// Present for interaction responses/followups, whose webhook transport
    /// cannot set a Discord nonce. Together with the immutable response body
    /// this is the equivalent stable identity for the canonical fallback.
    pub interaction_id: Option<u64>,
    pub content: String,
    pub embed_titles: Vec<Option<String>>,
    pub embed_descriptions: Vec<Option<String>>,
}

/// Bounded history used to reconcile an accepted Discord send whose SQLite
/// delivery CAS was interrupted before it could be persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPublicHistory {
    pub bot_user_id: u64,
    pub messages: Vec<DigPublicHistoryMessage>,
}

#[async_trait]
pub trait DigDiscordPort: Send + Sync {
    async fn dig_channel(&self, channel_id: i64) -> Result<Option<DigChannelSnapshot>, String>;

    async fn dig_channel_is_gamba(&self, guild_id: i64, channel_id: i64) -> Result<bool, String>;

    async fn dig_user_avatar_url(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, String>;

    /// Send a public Dig result into the selected channel.  The interaction
    /// responder remains responsible for the initial acknowledgement and
    /// private replies.
    async fn dig_send_public(
        &self,
        channel_id: i64,
        response: InteractionResponse,
    ) -> Result<(), String>;

    /// Send with Discord's nonce de-duplication enabled and return the
    /// accepted message identity. `nonce` is stable for one committed Dig
    /// action part across retry and process restart.
    async fn dig_send_public_once(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        nonce: &str,
    ) -> Result<InteractionMessageReceipt, DigPublicSendFailure>;

    /// Add one of the authored result reactions to a committed Dig message.
    ///
    /// This is deliberately best-effort at the provider boundary: the Dig
    /// transaction and its public message are already durable by the time a
    /// reaction is attempted.  The default keeps isolated transports source
    /// compatible; the Serenity adapter delegates to the existing
    /// [`DiscordTransport`] reaction operation.
    async fn dig_add_reaction(
        &self,
        _channel_id: i64,
        _message_id: u64,
        _emoji: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Read at most `limit` recent messages at or after the supplied time.
    /// Implementations must retain the Discord nonce and author identity.
    async fn dig_public_history(
        &self,
        channel_id: i64,
        after_unix_seconds: i64,
        limit: usize,
    ) -> Result<DigPublicHistory, String>;

    /// Send a best-effort Neon side message and remove it after the canonical
    /// delay. Test transports may retain it by delegating to `dig_send_public`.
    async fn dig_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        _delete_after: Duration,
    ) -> Result<(), String> {
        self.dig_send_public(channel_id, response).await
    }
}

/// Runtime adapter for the three rare bonus surfaces owned by the migrated
/// application layer (wheel, package deal, and trivia). The provider owns the
/// post-UI trigger and durable action claim; the adapter owns the existing
/// interactive Discord/session implementations.
#[async_trait]
pub trait DigBonusDispatchPort: Send + Sync {
    async fn dispatch_bonus(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        bonus: cama_app::dig_bonus_events::DigBonus,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String>;

    async fn report_partial_failure(
        &self,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .followup(
                InteractionResponse::message(cama_app::dig_bonus_events::PARTIAL_BONUS_FAILURE)
                    .ephemeral(),
            )
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DigProviderBuildError {
    #[error("dig provider failed to inspect the admitted SQLite schema: {0}")]
    Database(String),
}

#[derive(Clone)]
pub struct DigRegistrationProvider {
    handler: Arc<DigInteractionHandler>,
}

impl DigRegistrationProvider {
    /// Compose the fail-closed production `/dig` provider.
    ///
    /// Production callers provide the shared AI service when available while
    /// this constructor owns the canonical media root. The explicit-media
    /// [`Self::with_media_and_ai`] seam remains available for tests and
    /// deployment-specific asset fixtures.
    #[must_use = "inspect the provider admission result"]
    pub fn production(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn DigDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
        ai_service: Option<Arc<AIService>>,
        bonus_dispatcher: Arc<dyn DigBonusDispatchPort>,
    ) -> Result<Self, DigProviderBuildError> {
        let media = Arc::new(DigMediaRuntime::production(&DigRuntimeConfig::production()));
        let provider = Self::with_media_ai_and_vanity(
            database_path,
            config,
            discord,
            reminder_hooks,
            media,
            ai_service,
            Some(vanity_tax),
        )?;
        provider.set_bonus_dispatcher(bonus_dispatcher);
        Ok(provider)
    }

    /// Compose the complete production `/dig` command/component surface.
    #[cfg(test)]
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DigDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
    ) -> Self {
        let media = Arc::new(DigMediaRuntime::production(&DigRuntimeConfig::production()));
        Self::with_media(database_path, config, discord, reminder_hooks, media)
    }

    /// Compose the provider with an explicit media facade. Production uses
    /// [`Self::new`]; this seam keeps deployment-root and cached-byte behavior
    /// independently testable without putting filesystem work on Tokio.
    #[cfg(test)]
    #[must_use]
    pub fn with_media(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DigDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
        media: Arc<DigMediaRuntime>,
    ) -> Self {
        Self::with_media_and_ai(database_path, config, discord, reminder_hooks, media, None)
            .expect("admitted Dig schema in provider test")
    }

    /// Compose a test provider with an explicit AI service and no gateway
    /// vanity-tax state. Production must use [`Self::production`].
    #[cfg(test)]
    #[must_use = "inspect the provider admission result"]
    pub fn with_media_and_ai(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DigDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
        media: Arc<DigMediaRuntime>,
        ai_service: Option<Arc<AIService>>,
    ) -> Result<Self, DigProviderBuildError> {
        Self::with_media_ai_and_vanity(
            database_path,
            config,
            discord,
            reminder_hooks,
            media,
            ai_service,
            None,
        )
    }

    fn with_media_ai_and_vanity(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DigDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
        media: Arc<DigMediaRuntime>,
        ai_service: Option<Arc<AIService>>,
        vanity_tax: Option<Arc<PersistentVanityTaxService>>,
    ) -> Result<Self, DigProviderBuildError> {
        let path = database_path.as_ref().to_path_buf();
        let audit = cama_db::schema_manager_contracts::audit_existing_schema(&path)
            .map_err(|error| DigProviderBuildError::Database(error.to_string()))?;
        if !audit.is_compatible() {
            return Err(DigProviderBuildError::Database(format!(
                "schema-manager contract is incompatible: {audit:?}"
            )));
        }
        let dig_config = DigRuntimeConfig::production()
            .with_runtime_policy(
                config.values.minigame_jc_delta_scale,
                EconomyEventConfig {
                    enabled: config.values.economy_events_enabled,
                    normal_annual_rate: config.values.economy_normal_annual_rate,
                    inflation_ceiling: config.values.economy_inflation_ceiling,
                    lookback_days: config.values.economy_event_lookback_days,
                    max_reserve_burn_pct: config.values.economy_event_max_reserve_burn_pct,
                    max_wallet_burn_pct: config.values.economy_event_max_wallet_burn_pct,
                    trigger_hour_local: u8::try_from(
                        config.values.economy_event_trigger_hour_local.clamp(0, 23),
                    )
                    .unwrap_or_default(),
                },
            )
            .with_bankruptcy_penalty_rate(config.values.bankruptcy_penalty_rate)
            .with_pet_decay_per_day(config.values.pet_hunger_decay_per_day);
        let mut neon = DigNeonService::new(
            SeededDigNeonRandom::new(fastrand::u64(..)),
            RuntimeDigNeonCooldown::new(config.values.neon_cooldown_seconds),
        );
        neon.set_enabled(config.values.neon_degen_enabled);
        neon.set_dig_llm_enabled(config.values.dig_llm_enabled);
        neon.set_dig_chance(config.values.neon_dig_chance);
        let flavor_data = Arc::new(SqliteDigFlavorDataAdapter::new(&path));
        let ai_available = ai_service.is_some();
        let flavor_ai: Arc<dyn DigFlavorAiPort> = ai_service.map_or_else(
            || Arc::new(UnavailableDigFlavorAi) as Arc<dyn DigFlavorAiPort>,
            |service| Arc::new(AIServiceDigFlavorAiPort::new(service)),
        );
        let flavor_context = Arc::new(SqliteDigFlavorContextAdapter::new(
            &path,
            cama_app::dig_flavor::roster_lines(),
        ));
        let flavor_config = Arc::new(SqliteDigFlavorConfigAdapter::new(
            &path,
            config.values.dig_llm_enabled && ai_available,
        ));
        let flavor = Arc::new(
            DigFlavorService::new(
                flavor_ai,
                flavor_data.clone(),
                flavor_context,
                Box::new(SeededDigFlavorRng::new(fastrand::u64(..))),
            )
            .with_config(flavor_config),
        );
        Ok(Self {
            handler: Arc::new(DigInteractionHandler {
                state: Arc::new(DigRuntimeState {
                    database_path: path,
                    configured_channel_id: config.channels.dig,
                    admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
                    pet_hunger_decay_per_day: config.values.pet_hunger_decay_per_day,
                    dig_config,
                    discord,
                    reminder_hooks,
                    media,
                    flavor,
                    flavor_data,
                    vanity_tax,
                    neon: Mutex::new(neon),
                    boss_entropy: RuntimeBossEntropy::default(),
                    view_nonce: format!("{:016x}", fastrand::u64(..)),
                    abandon_views: Mutex::new(BTreeMap::new()),
                    prestige_views: Mutex::new(BTreeMap::new()),
                    sabotage_views: Mutex::new(BTreeMap::new()),
                    guide_views: Mutex::new(BTreeMap::new()),
                    paid_views: Mutex::new(BTreeMap::new()),
                    route_views: Mutex::new(BTreeMap::new()),
                    rate_limits: Mutex::new(BTreeMap::new()),
                    force_events: Mutex::new(BTreeSet::new()),
                    bonus_dispatcher: Mutex::new(None),
                }),
            }),
        })
    }

    /// READY/resume hook for persisted Dig views.  Dig's long-lived state is
    /// authoritative in SQLite; transient Discord views are intentionally
    /// invalidated on restart, matching discord.py's non-persistent Views.
    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(DigGatewayObserver {
            state: Arc::clone(&self.handler.state),
        })
    }

    /// Attach the composition-layer dispatcher for wheel/package/trivia
    /// bonus sessions. Keeping this setter separate lets the provider be
    /// admitted before the gateway has assembled those shared adapters.
    pub fn set_bonus_dispatcher(&self, dispatcher: Arc<dyn DigBonusDispatchPort>) {
        if let Ok(mut slot) = self.handler.state.bonus_dispatcher.lock() {
            *slot = Some(dispatcher);
        }
    }
}

impl RegistrationProvider for DigRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "dig".to_owned(),
            description: "Tunnel digging minigame".to_owned(),
            options: dig_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: ROUTE_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: "duel_opt_".to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct DigGatewayObserver {
    state: Arc<DigRuntimeState>,
}

#[async_trait]
impl GatewayEventObserver for DigGatewayObserver {
    fn name(&self) -> &'static str {
        "dig-pending-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let handler = DigInteractionHandler {
            state: Arc::clone(&self.state),
        };
        let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
        for guild_id in context.guild_ids() {
            let Ok(guild_id_signed) = i64::try_from(*guild_id) else {
                report.failures.push(ReadyRecoveryFailure {
                    guild_id: *guild_id,
                    message: "Dig recovery guild id exceeds SQLite range".to_owned(),
                });
                continue;
            };
            let mut recovered = 0;
            let mut failed = None;

            // Event settlement has a two-phase application boundary. READY
            // first freezes any actor-committed Pending projection, then the
            // normal scan admits only Ready rows to the nonce/history sender.
            match handler
                .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(guild_id_signed),
                    discord_id: None,
                    limit: 100,
                })
                .await
            {
                Ok(recoveries) => {
                    for delivery in recoveries {
                        match handler.recover_event_delivery(delivery.action_id).await {
                            Ok(true) => {}
                            Ok(false) => {
                                failed = Some(format!(
                                    "Dig event recovery did not freeze action {}",
                                    delivery.action_id
                                ));
                                break;
                            }
                            Err(message) => {
                                failed = Some(message);
                                break;
                            }
                        }
                    }
                }
                Err(message) => failed = Some(message),
            }

            if failed.is_none() {
                match handler
                    .pending_event_deliveries(DigEventPendingDeliveryQuery {
                        guild_id: Some(guild_id_signed),
                        discord_id: None,
                        limit: 100,
                    })
                    .await
                {
                    Ok(deliveries) => {
                        for delivery in deliveries {
                            match handler.deliver_event_to_channel(&delivery).await {
                                Ok(()) => recovered += 1,
                                Err(message) => {
                                    failed = Some(message);
                                    break;
                                }
                            }
                        }
                    }
                    Err(message) => failed = Some(message),
                }
            }

            // The original Dig outbox remains independent from event rows;
            // process it in the same READY pass so an event failure cannot
            // prevent recovery of an already committed normal result.
            let pending = match handler
                .pending_deliveries(DigRuntimePendingDeliveryQuery {
                    guild_id: Some(guild_id_signed),
                    discord_id: None,
                    limit: 100,
                })
                .await
            {
                Ok(pending) => pending,
                Err(message) => {
                    if failed.is_none() {
                        failed = Some(message);
                    }
                    Vec::new()
                }
            };
            for delivery in pending {
                match handler.deliver_to_channel(&delivery).await {
                    Ok(()) => recovered += 1,
                    Err(message) => {
                        if failed.is_none() {
                            failed = Some(message);
                        }
                        break;
                    }
                }
            }
            report.members_refreshed += recovered;
            if let Some(message) = failed {
                report.failures.push(ReadyRecoveryFailure {
                    guild_id: *guild_id,
                    message,
                });
            } else {
                report.guilds_refreshed += 1;
            }
        }
        report
    }
}

struct DigRuntimeState {
    database_path: PathBuf,
    configured_channel_id: Option<i64>,
    admin_user_ids: BTreeSet<i64>,
    pet_hunger_decay_per_day: i64,
    dig_config: DigRuntimeConfig,
    discord: Arc<dyn DigDiscordPort>,
    reminder_hooks: Option<ReminderHooks>,
    media: Arc<DigMediaRuntime>,
    flavor: Arc<DigFlavorService>,
    flavor_data: Arc<SqliteDigFlavorDataAdapter>,
    /// Always present for the fail-closed production constructor. Test-only
    /// constructors keep an explicit no-tax path so policy fixtures remain
    /// independent from gateway membership refresh state.
    vanity_tax: Option<Arc<PersistentVanityTaxService>>,
    neon: Mutex<DigNeonService<SeededDigNeonRandom, RuntimeDigNeonCooldown>>,
    boss_entropy: RuntimeBossEntropy,
    bonus_dispatcher: Mutex<Option<Arc<dyn DigBonusDispatchPort>>>,
    view_nonce: String,
    abandon_views: Mutex<BTreeMap<String, DigAbandonViewState>>,
    prestige_views: Mutex<BTreeMap<String, DigPrestigeViewState>>,
    sabotage_views: Mutex<BTreeMap<String, DigSabotageViewState>>,
    guide_views: Mutex<BTreeMap<String, DigGuideViewState>>,
    paid_views: Mutex<BTreeMap<String, DigPaidViewState>>,
    route_views: Mutex<BTreeMap<String, DigRouteViewState>>,
    rate_limits: Mutex<BTreeMap<DigRateKey, DigRateHits>>,
    force_events: Mutex<BTreeSet<(i64, i64)>>,
}

fn configured_dig_runtime(
    path: PathBuf,
    config: DigRuntimeConfig,
    vanity_tax: Option<Arc<PersistentVanityTaxService>>,
) -> cama_app::dig_runtime::DigRuntimeService {
    let service = cama_app::dig_runtime::DigRuntimeService::sqlite_with_config(path, config);
    if let Some(vanity_tax) = vanity_tax {
        service.with_vanity_tax(vanity_tax)
    } else {
        service
    }
}

fn configured_boss_runtime(
    path: PathBuf,
    pet_hunger_decay_per_day: i64,
    vanity_tax: Option<Arc<PersistentVanityTaxService>>,
) -> DigBossRuntimeService {
    let service =
        DigBossRuntimeService::sqlite(DigBossRuntimeConfig::new(path, pet_hunger_decay_per_day));
    if let Some(vanity_tax) = vanity_tax {
        service.with_vanity_tax(vanity_tax)
    } else {
        service
    }
}

struct UnavailableDigFlavorAi;

impl DigFlavorAiPort for UnavailableDigFlavorAi {
    fn call_with_tools(&self, _request: DigFlavorRequest) -> Result<DigToolCallResult, String> {
        Err("Dig flavor AI is disabled or unavailable".to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DigAbandonViewState {
    owner_id: i64,
    guild_id: i64,
    created_at: i64,
    resolved: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigAbandonViewAdmission {
    Admitted,
    WrongOwner,
    Expired,
    AlreadyResolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DigSabotageViewState {
    owner_id: i64,
    guild_id: i64,
    target_id: i64,
    target_name: String,
    created_at: i64,
    resolved: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DigSabotageViewAdmission {
    Admitted(DigSabotageViewState),
    WrongOwner,
    Expired,
    AlreadyResolved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DigGuideViewState {
    owner_id: i64,
    guild_id: i64,
    created_at: i64,
    page: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigGuideViewAdmission {
    Admitted(usize),
    WrongOwner,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigGuideDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DigPaidViewState {
    owner_id: i64,
    guild_id: i64,
    created_at: i64,
    claimed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DigRouteOfferView {
    id: String,
    name: String,
    description: String,
    layer: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DigRouteChoiceView {
    layer: String,
    start_depth: i64,
    end_depth: i64,
    offered_routes: Vec<DigRouteOfferView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DigRouteViewState {
    owner_id: i64,
    guild_id: i64,
    created_at: i64,
    choice: DigRouteChoiceView,
    claimed: bool,
    resolved: bool,
    timed_out: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DigRouteViewAdmission {
    Admitted(DigRouteChoiceView),
    WrongOwner,
    Expired,
    AlreadyResolved,
    InvalidRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigPaidViewAdmission {
    Admitted,
    WrongOwner,
    Expired,
    AlreadyClaimed,
}

#[derive(Clone, Debug)]
struct RuntimeDigNeonCooldown {
    cooldown_seconds: i64,
    last_fired: BTreeMap<(u64, u64), i64>,
}

impl RuntimeDigNeonCooldown {
    fn new(cooldown_seconds: i64) -> Self {
        Self {
            cooldown_seconds: cooldown_seconds.max(0),
            last_fired: BTreeMap::new(),
        }
    }
}

impl DigNeonCooldownPort for RuntimeDigNeonCooldown {
    fn is_ready(&self, discord_id: u64, guild_id: Option<u64>) -> bool {
        let now = unix_now();
        self.last_fired
            .get(&(discord_id, guild_id.unwrap_or_default()))
            .is_none_or(|last| now.saturating_sub(*last) >= self.cooldown_seconds)
    }

    fn mark_fired(&mut self, discord_id: u64, guild_id: Option<u64>) {
        self.last_fired
            .insert((discord_id, guild_id.unwrap_or_default()), unix_now());
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DigPrestigeViewState {
    owner_id: i64,
    guild_id: i64,
    created_at: i64,
    requires_mutation: bool,
    selected_mutation: Option<String>,
    claimed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigPrestigeViewAdmission {
    Admitted,
    WrongOwner,
    Expired,
    AlreadyClaimed,
    InvalidTransition,
}

fn prestige_view_admission(
    views: &mut BTreeMap<String, DigPrestigeViewState>,
    token: &str,
    owner_id: i64,
    guild_id: i64,
    now: i64,
) -> DigPrestigeViewAdmission {
    let Some(view) = views.get(token).cloned() else {
        return DigPrestigeViewAdmission::Expired;
    };
    if view.owner_id != owner_id || view.guild_id != guild_id {
        return DigPrestigeViewAdmission::WrongOwner;
    }
    if now.saturating_sub(view.created_at) >= PRESTIGE_VIEW_TIMEOUT_SECONDS {
        views.remove(token);
        return DigPrestigeViewAdmission::Expired;
    }
    if view.claimed {
        return DigPrestigeViewAdmission::AlreadyClaimed;
    }
    DigPrestigeViewAdmission::Admitted
}

struct DigInteractionHandler {
    state: Arc<DigRuntimeState>,
}

#[async_trait]
impl InteractionHandler for DigInteractionHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        match request {
            InteractionRequest::Component { custom_id, .. }
                if custom_id
                    .strip_prefix("dig:boss:fight:")
                    .is_some_and(|action| !action.starts_with("carried:")) =>
            {
                InteractionAcknowledgementPolicy::Modal
            }
            _ => InteractionAcknowledgementPolicy::Automatic,
        }
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { .. } => self.handle_command(request, responder).await,
            InteractionRequest::Autocomplete { .. } => {
                self.handle_autocomplete(request, responder).await
            }
            InteractionRequest::Component { .. } => self.handle_component(request, responder).await,
            InteractionRequest::Modal { .. } => self.handle_modal(request, responder).await,
        }
        .map_err(Into::into)
    }
}

impl DigInteractionHandler {
    fn force_event_pending(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        Ok(self
            .state
            .force_events
            .lock()
            .map_err(|_| "Dig force-event lock poisoned")?
            .contains(&(user_id, guild_id)))
    }

    fn consume_force_event(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        self.state
            .force_events
            .lock()
            .map_err(|_| "Dig force-event lock poisoned")?
            .remove(&(user_id, guild_id));
        Ok(())
    }

    fn create_paid_view(&self, owner_id: i64, guild_id: i64, now: i64) -> Result<String, String> {
        let mut views = self
            .state
            .paid_views
            .lock()
            .map_err(|_| "Dig paid-view lock poisoned")?;
        views.retain(|_, view| now.saturating_sub(view.created_at) < PAID_VIEW_TIMEOUT_SECONDS);
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigPaidViewState {
                    owner_id,
                    guild_id,
                    created_at: now,
                    claimed: false,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig paid view".to_owned())
    }

    fn create_route_view(
        &self,
        owner_id: i64,
        guild_id: i64,
        choice: DigRouteChoiceView,
        now: i64,
    ) -> Result<String, String> {
        let mut views = self
            .state
            .route_views
            .lock()
            .map_err(|_| "Dig route-view lock poisoned")?;
        // A route view is process-local, so stale entries are safe to retire
        // eagerly when the owner opens a replacement junction.  Exactly 180s
        // is expired, matching discord.py's View timeout boundary.
        views.retain(|_, view| {
            now.saturating_sub(view.created_at) < ROUTE_VIEW_TIMEOUT_SECONDS && !view.resolved
        });
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigRouteViewState {
                    owner_id,
                    guild_id,
                    created_at: now,
                    choice,
                    claimed: false,
                    resolved: false,
                    timed_out: false,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig route view".to_owned())
    }

    fn retire_route_view(&self, token: &str) -> Result<(), String> {
        self.state
            .route_views
            .lock()
            .map_err(|_| "Dig route-view lock poisoned")?
            .remove(token);
        Ok(())
    }

    fn claim_route_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        route_id: &str,
        now: i64,
    ) -> Result<DigRouteViewAdmission, String> {
        let mut views = self
            .state
            .route_views
            .lock()
            .map_err(|_| "Dig route-view lock poisoned")?;
        let Some(view) = views.get_mut(token) else {
            return Ok(DigRouteViewAdmission::Expired);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(DigRouteViewAdmission::WrongOwner);
        }
        if view.timed_out || now.saturating_sub(view.created_at) >= ROUTE_VIEW_TIMEOUT_SECONDS {
            view.timed_out = true;
            view.claimed = false;
            return Ok(DigRouteViewAdmission::Expired);
        }
        if view.resolved || view.claimed {
            return Ok(DigRouteViewAdmission::AlreadyResolved);
        }
        if !view
            .choice
            .offered_routes
            .iter()
            .any(|route| route.id == route_id)
        {
            return Ok(DigRouteViewAdmission::InvalidRoute);
        }
        // The mutex is the one-shot fence.  Release it before touching
        // SQLite; a duplicate component can therefore never settle twice.
        view.claimed = true;
        Ok(DigRouteViewAdmission::Admitted(view.choice.clone()))
    }

    fn reset_route_view_claim(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
    ) -> Result<bool, String> {
        let mut views = self
            .state
            .route_views
            .lock()
            .map_err(|_| "Dig route-view lock poisoned")?;
        let Some(view) = views.get_mut(token) else {
            return Ok(false);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(false);
        }
        if view.timed_out
            || unix_now().saturating_sub(view.created_at) >= ROUTE_VIEW_TIMEOUT_SECONDS
        {
            view.timed_out = true;
            view.claimed = false;
            return Ok(false);
        }
        if !view.claimed || view.resolved {
            return Ok(false);
        }
        view.claimed = false;
        Ok(true)
    }

    fn resolve_route_view(&self, token: &str, owner_id: i64, guild_id: i64) -> Result<(), String> {
        let mut views = self
            .state
            .route_views
            .lock()
            .map_err(|_| "Dig route-view lock poisoned")?;
        if let Some(view) = views.get_mut(token)
            && view.owner_id == owner_id
            && view.guild_id == guild_id
        {
            view.claimed = false;
            view.resolved = true;
        }
        Ok(())
    }

    fn schedule_route_view_timeout(
        &self,
        token: String,
        responder: Arc<dyn InteractionResponder>,
        receipt: Option<InteractionMessageReceipt>,
    ) {
        let state = Arc::clone(&self.state);
        let view_nonce = self.state.view_nonce.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(
                u64::try_from(ROUTE_VIEW_TIMEOUT_SECONDS).unwrap_or_default(),
            ))
            .await;
            let timed_out = state
                .route_views
                .lock()
                .ok()
                .and_then(|mut views| {
                    let view = views.get_mut(&token)?;
                    if view.claimed || view.resolved || view.timed_out {
                        return Some(None);
                    }
                    view.timed_out = true;
                    Some(Some(view.clone()))
                })
                .flatten();
            if let (Some(view), Some(receipt)) = (timed_out, receipt) {
                let response = route_choice_response(
                    &view.choice,
                    &view_nonce,
                    view.owner_id,
                    view.guild_id,
                    &token,
                    true,
                );
                let _ = responder.edit_message(receipt, response).await;
            }
        });
    }

    fn claim_paid_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigPaidViewAdmission, String> {
        let mut views = self
            .state
            .paid_views
            .lock()
            .map_err(|_| "Dig paid-view lock poisoned")?;
        let Some(view) = views.get(token).copied() else {
            return Ok(DigPaidViewAdmission::Expired);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(DigPaidViewAdmission::WrongOwner);
        }
        if now.saturating_sub(view.created_at) >= PAID_VIEW_TIMEOUT_SECONDS {
            views.remove(token);
            return Ok(DigPaidViewAdmission::Expired);
        }
        if view.claimed {
            return Ok(DigPaidViewAdmission::AlreadyClaimed);
        }
        views
            .get_mut(token)
            .expect("paid view was read above")
            .claimed = true;
        Ok(DigPaidViewAdmission::Admitted)
    }

    fn schedule_paid_view_timeout(
        &self,
        token: String,
        responder: Arc<dyn InteractionResponder>,
        receipt: Option<InteractionMessageReceipt>,
    ) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(
                u64::try_from(PAID_VIEW_TIMEOUT_SECONDS).unwrap_or_default(),
            ))
            .await;
            let should_retire = state
                .paid_views
                .lock()
                .ok()
                .and_then(|mut views| views.remove(&token))
                .is_some_and(|view| !view.claimed);
            if should_retire && let Some(receipt) = receipt {
                let _ = responder
                    .edit_message(
                        receipt,
                        InteractionResponse::message("Dig cancelled.").action_rows(Vec::new()),
                    )
                    .await;
            }
        });
    }

    fn create_guide_view(&self, owner_id: i64, guild_id: i64, now: i64) -> Result<String, String> {
        let mut views = self
            .state
            .guide_views
            .lock()
            .map_err(|_| "Dig guide-view lock poisoned")?;
        views.retain(|_, view| now.saturating_sub(view.created_at) < GUIDE_VIEW_TIMEOUT_SECONDS);
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigGuideViewState {
                    owner_id,
                    guild_id,
                    created_at: now,
                    page: 0,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig guide view".to_owned())
    }

    fn navigate_guide_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        direction: DigGuideDirection,
        now: i64,
    ) -> Result<DigGuideViewAdmission, String> {
        let mut views = self
            .state
            .guide_views
            .lock()
            .map_err(|_| "Dig guide-view lock poisoned")?;
        let Some(view) = views.get(token).copied() else {
            return Ok(DigGuideViewAdmission::Expired);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(DigGuideViewAdmission::WrongOwner);
        }
        if now.saturating_sub(view.created_at) >= GUIDE_VIEW_TIMEOUT_SECONDS {
            views.remove(token);
            return Ok(DigGuideViewAdmission::Expired);
        }
        let view = views.get_mut(token).expect("guide view was read above");
        view.page = match direction {
            DigGuideDirection::Previous => view.page.saturating_sub(1),
            DigGuideDirection::Next => (view.page + 1).min(DIG_GUIDE_PAGES.len() - 1),
        };
        Ok(DigGuideViewAdmission::Admitted(view.page))
    }

    fn create_abandon_view(
        &self,
        owner_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<String, String> {
        let mut views = self
            .state
            .abandon_views
            .lock()
            .map_err(|_| "Dig abandon-view lock poisoned")?;
        views.retain(|_, view| now.saturating_sub(view.created_at) < ABANDON_VIEW_TIMEOUT_SECONDS);
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigAbandonViewState {
                    owner_id,
                    guild_id,
                    created_at: now,
                    resolved: false,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig abandon view".to_owned())
    }

    fn claim_abandon_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigAbandonViewAdmission, String> {
        let mut views = self
            .state
            .abandon_views
            .lock()
            .map_err(|_| "Dig abandon-view lock poisoned")?;
        let Some(view) = views.get(token).copied() else {
            return Ok(DigAbandonViewAdmission::Expired);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(DigAbandonViewAdmission::WrongOwner);
        }
        if now.saturating_sub(view.created_at) >= ABANDON_VIEW_TIMEOUT_SECONDS {
            views.remove(token);
            return Ok(DigAbandonViewAdmission::Expired);
        }
        if view.resolved {
            return Ok(DigAbandonViewAdmission::AlreadyResolved);
        }
        views.get_mut(token).expect("view was read above").resolved = true;
        Ok(DigAbandonViewAdmission::Admitted)
    }

    fn create_sabotage_view(
        &self,
        owner_id: i64,
        guild_id: i64,
        target_id: i64,
        target_name: String,
        now: i64,
    ) -> Result<String, String> {
        let mut views = self
            .state
            .sabotage_views
            .lock()
            .map_err(|_| "Dig sabotage-view lock poisoned")?;
        views.retain(|_, view| now.saturating_sub(view.created_at) < SABOTAGE_VIEW_TIMEOUT_SECONDS);
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigSabotageViewState {
                    owner_id,
                    guild_id,
                    target_id,
                    target_name: target_name.clone(),
                    created_at: now,
                    resolved: false,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig sabotage view".to_owned())
    }

    fn claim_sabotage_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigSabotageViewAdmission, String> {
        let mut views = self
            .state
            .sabotage_views
            .lock()
            .map_err(|_| "Dig sabotage-view lock poisoned")?;
        let Some(view) = views.get(token).cloned() else {
            return Ok(DigSabotageViewAdmission::Expired);
        };
        if view.owner_id != owner_id || view.guild_id != guild_id {
            return Ok(DigSabotageViewAdmission::WrongOwner);
        }
        if now.saturating_sub(view.created_at) >= SABOTAGE_VIEW_TIMEOUT_SECONDS {
            views.remove(token);
            return Ok(DigSabotageViewAdmission::Expired);
        }
        if view.resolved {
            return Ok(DigSabotageViewAdmission::AlreadyResolved);
        }
        views.get_mut(token).expect("view was read above").resolved = true;
        Ok(DigSabotageViewAdmission::Admitted(view))
    }

    fn create_prestige_view(
        &self,
        owner_id: i64,
        guild_id: i64,
        requires_mutation: bool,
        now: i64,
    ) -> Result<String, String> {
        let mut views = self
            .state
            .prestige_views
            .lock()
            .map_err(|_| "Dig prestige-view lock poisoned")?;
        views.retain(|_, view| now.saturating_sub(view.created_at) < PRESTIGE_VIEW_TIMEOUT_SECONDS);
        for _ in 0..8 {
            let token = format!("{:016x}", fastrand::u64(..));
            if views.contains_key(&token) {
                continue;
            }
            views.insert(
                token.clone(),
                DigPrestigeViewState {
                    owner_id,
                    guild_id,
                    created_at: now,
                    requires_mutation,
                    selected_mutation: None,
                    claimed: false,
                },
            );
            return Ok(token);
        }
        Err("Could not allocate a Dig prestige view".to_owned())
    }

    fn inspect_prestige_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigPrestigeViewAdmission, String> {
        let mut views = self
            .state
            .prestige_views
            .lock()
            .map_err(|_| "Dig prestige-view lock poisoned")?;
        Ok(prestige_view_admission(
            &mut views, token, owner_id, guild_id, now,
        ))
    }

    fn select_prestige_mutation(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        mutation: &str,
        now: i64,
    ) -> Result<DigPrestigeViewAdmission, String> {
        let mut views = self
            .state
            .prestige_views
            .lock()
            .map_err(|_| "Dig prestige-view lock poisoned")?;
        let admission = prestige_view_admission(&mut views, token, owner_id, guild_id, now);
        if admission != DigPrestigeViewAdmission::Admitted {
            return Ok(admission);
        }
        let view = views.get_mut(token).expect("admitted view remains present");
        if !view.requires_mutation {
            return Ok(DigPrestigeViewAdmission::InvalidTransition);
        }
        if view.selected_mutation.is_some() {
            return Ok(DigPrestigeViewAdmission::AlreadyClaimed);
        }
        view.selected_mutation = Some(mutation.to_owned());
        Ok(DigPrestigeViewAdmission::Admitted)
    }

    fn claim_prestige_view(
        &self,
        token: &str,
        owner_id: i64,
        guild_id: i64,
        mutation: Option<&str>,
        now: i64,
    ) -> Result<DigPrestigeViewAdmission, String> {
        let mut views = self
            .state
            .prestige_views
            .lock()
            .map_err(|_| "Dig prestige-view lock poisoned")?;
        let admission = prestige_view_admission(&mut views, token, owner_id, guild_id, now);
        if admission != DigPrestigeViewAdmission::Admitted {
            return Ok(admission);
        }
        let view = views.get_mut(token).expect("admitted view remains present");
        let valid_transition = if view.requires_mutation {
            view.selected_mutation.as_deref() == mutation && mutation.is_some()
        } else {
            mutation.is_none()
        };
        if !valid_transition {
            return Ok(DigPrestigeViewAdmission::InvalidTransition);
        }
        view.claimed = true;
        Ok(DigPrestigeViewAdmission::Admitted)
    }

    async fn handle_command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            interaction_id,
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            member_permissions,
            options,
        } = request
        else {
            return Err("invalid Dig command payload".to_owned());
        };
        if name != "dig" {
            return Err(format!("dig handler received command {name:?}"));
        }
        let user_id = signed_id(user_id, "user")?;
        let Some(guild_id) = guild_id else {
            return respond(
                &responder,
                InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral(),
            )
            .await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let channel_id = channel_id.map(|id| signed_id(id, "channel")).transpose()?;
        let subcommand = command_path(&options);
        let rate_scope = if subcommand.as_slice() == ["artifacts"] {
            "artifacts"
        } else if subcommand.as_slice() == ["go"] {
            "go"
        } else {
            "other"
        };
        let limit = if rate_scope == "artifacts" {
            (ARTIFACT_RATE_LIMIT, ARTIFACT_RATE_WINDOW)
        } else if rate_scope == "go" {
            (COMMAND_RATE_LIMIT, COMMAND_RATE_WINDOW)
        } else {
            (usize::MAX, Duration::ZERO)
        };
        let admin_command = subcommand.first().is_some_and(|part| part == "admin");
        if !admin_command
            && !self
                .require_dig_channel(user_id, guild_id, channel_id, &responder)
                .await?
        {
            return Ok(());
        }
        if limit.0 != usize::MAX
            && let Some(retry_after) =
                self.take_rate_limit(user_id, guild_id, rate_scope, limit.0, limit.1)?
        {
            return respond(
                &responder,
                InteractionResponse::message(format!("Slow down! Wait {retry_after}s."))
                    .ephemeral(),
            )
            .await;
        }
        if !admin_command && !self.registered(user_id, guild_id).await? {
            return respond(
                &responder,
                InteractionResponse::message(REGISTER_FIRST_MESSAGE).ephemeral(),
            )
            .await;
        }

        match subcommand.as_slice() {
            [sub] if sub == "go" => {
                self.command_go(
                    interaction_id,
                    user_id,
                    guild_id,
                    channel_id,
                    &user_display_name,
                    responder,
                )
                .await
            }
            [sub] if sub == "help" => {
                self.command_help(user_id, guild_id, &options, responder)
                    .await
            }
            [sub] if sub == "sabotage" => {
                self.command_sabotage(user_id, guild_id, &options, responder)
                    .await
            }
            [sub] if sub == "info" => {
                self.command_info(user_id, guild_id, &user_display_name, &options, responder)
                    .await
            }
            [sub] if sub == "leaderboard" => {
                self.command_leaderboard(user_id, guild_id, responder).await
            }
            [sub] if sub == "halloffame" => self.command_hall_of_fame(guild_id, responder).await,
            [sub] if sub == "use" => {
                self.command_use(user_id, guild_id, &options, responder)
                    .await
            }
            [sub] if sub == "gift" => {
                self.command_gift(user_id, guild_id, &options, responder)
                    .await
            }
            [sub] if sub == "shop" => self.command_shop(user_id, guild_id, responder).await,
            [sub] if sub == "buy" => {
                self.command_buy(user_id, guild_id, &options, responder)
                    .await
            }
            [sub] if sub == "flex" => {
                self.command_flex(user_id, guild_id, &user_display_name, responder)
                    .await
            }
            [sub] if sub == "prestige" => self.command_prestige(user_id, guild_id, responder).await,
            [sub] if sub == "abandon" => self.command_abandon(user_id, guild_id, responder).await,
            [sub] if sub == "trap" => self.command_trap(user_id, guild_id, responder).await,
            [sub] if sub == "insure" => self.command_insure(user_id, guild_id, responder).await,
            [sub] if sub == "inventory" => {
                self.command_inventory(user_id, guild_id, responder).await
            }
            [sub] if sub == "artifacts" => {
                self.command_artifacts(user_id, guild_id, responder).await
            }
            [sub] if sub == "gear" => self.command_gear(user_id, guild_id, responder).await,
            [sub] if sub == "weather" => self.command_weather(guild_id, responder).await,
            [sub] if sub == "guide" => self.command_guide(user_id, guild_id, responder).await,
            [group, sub] if group == "admin" => {
                self.command_admin(
                    user_id,
                    guild_id,
                    member_permissions,
                    sub,
                    &options,
                    responder,
                )
                .await
            }
            [group, sub] if group == "miner" => {
                self.command_miner(
                    user_id,
                    guild_id,
                    &user_display_name,
                    sub,
                    &options,
                    responder,
                )
                .await
            }
            _ => Err(format!("unknown /dig subcommand: {}", subcommand.join(" "))),
        }
    }

    async fn handle_autocomplete(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Autocomplete {
            name,
            user_id,
            guild_id,
            focused_option,
            focused_value,
            options,
            ..
        } = request
        else {
            return Err("invalid Dig autocomplete payload".to_owned());
        };
        if name != "dig" {
            return Err(format!("dig handler received autocomplete {name:?}"));
        }
        let query = focused_value.to_ascii_lowercase();
        let path = command_path(&options);
        let identity = guild_id.and_then(|guild_id| {
            Some((i64::try_from(user_id).ok()?, i64::try_from(guild_id).ok()?))
        });
        let choices = match (path.as_slice(), focused_option.as_str()) {
            ([subcommand], "item") if subcommand == "use" => {
                if let Some((user_id, guild_id)) = identity {
                    let database_path = self.state.database_path.clone();
                    blocking(move || {
                        DigInventoryService::new(DigInventoryRepository::new(database_path))
                            .get_inventory(user_id, Some(guild_id))
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .map_or_else(|_| Vec::new(), |items| dig_item_choices(&query, &items))
                } else {
                    Vec::new()
                }
            }
            ([subcommand], "item") if subcommand == "buy" => {
                if let Some((user_id, guild_id)) = identity {
                    let database_path = self.state.database_path.clone();
                    blocking(move || {
                        cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database_path)
                            .shop(user_id, guild_id)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .ok()
                    .flatten()
                    .map_or_else(Vec::new, |shop| dig_buy_choices(&query, &shop))
                } else {
                    Vec::new()
                }
            }
            ([subcommand], "artifact") if subcommand == "gift" => {
                if let Some((user_id, guild_id)) = identity {
                    let database_path = self.state.database_path.clone();
                    blocking(move || {
                        cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database_path)
                            .panel(user_id, guild_id)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .ok()
                    .flatten()
                    .map_or_else(Vec::new, |panel| dig_relic_choices(&query, &panel))
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        responder
            .autocomplete(choices)
            .await
            .map_err(|error| error.to_string())
    }

    async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            interaction_id,
            custom_id,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            values,
            ..
        } = request
        else {
            return Err("invalid Dig component payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        let Some(guild_id) = guild_id else {
            return respond(
                &responder,
                InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral(),
            )
            .await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let channel_id = channel_id.map(|id| signed_id(id, "channel")).transpose()?;
        if !self.registered(user_id, guild_id).await? {
            return respond(
                &responder,
                InteractionResponse::message(REGISTER_FIRST_MESSAGE).ephemeral(),
            )
            .await;
        }
        if let Some(option_index) = custom_id
            .strip_prefix("duel_opt_")
            .and_then(|value| value.parse::<usize>().ok())
        {
            return self
                .handle_duel_option_component(
                    option_index,
                    user_id,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await;
        }
        if let Some(raw) = custom_id.strip_prefix(ROUTE_COMPONENT_PREFIX) {
            return self
                .handle_route_component(raw, user_id, guild_id, responder)
                .await;
        }
        let action = custom_id.strip_prefix(COMPONENT_PREFIX).unwrap_or_default();
        if action.starts_with("guide:") {
            return self
                .handle_guide_component(action, user_id, guild_id, responder)
                .await;
        }
        if action.starts_with("prestige-") {
            return self
                .handle_prestige_component(
                    action,
                    user_id,
                    &user_display_name,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await;
        }
        if action.starts_with("paid:") {
            return self
                .handle_paid_component(
                    action,
                    interaction_id,
                    user_id,
                    user_display_name,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await;
        }
        if action.starts_with("event-action:") || action.starts_with("event:") {
            return self
                .handle_event_component(
                    action,
                    interaction_id,
                    user_id,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await;
        }
        if action.starts_with("sabotage:") {
            return self
                .handle_sabotage_component(action, user_id, guild_id, responder)
                .await;
        }
        if action.starts_with("abandon:") {
            return self
                .handle_abandon_component(action, user_id, guild_id, responder)
                .await;
        }
        if let Some(route) = action.strip_prefix("route:") {
            return self
                .handle_route_component(route, user_id, guild_id, responder)
                .await;
        }
        if action.starts_with("boss:") {
            return self
                .handle_boss_component(
                    action,
                    user_id,
                    &user_display_name,
                    guild_id,
                    channel_id,
                    responder,
                )
                .await;
        }
        if let Some(gear_action) = action.strip_prefix("gear:") {
            return self
                .handle_gear_component(user_id, guild_id, gear_action, &values, responder)
                .await;
        }
        expired_dig_component(&responder).await
    }

    async fn handle_duel_option_component(
        &self,
        option_index: usize,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let now = unix_now();
        let result = match self.resume_boss(user_id, guild_id, option_index, now).await {
            Ok(result) => result,
            Err(error) => {
                return responder
                    .followup(boss_error_response(error))
                    .await
                    .map_err(|error| error.to_string());
            }
        };
        if boss_resume_is_resolved(&result) {
            self.reconcile_resolved_boss(user_id, guild_id, now).await;
        }
        let action_id = result.action_id;
        let neon_victory = boss_resume_neon_victory(&result);
        let next_phase = boss_resume_has_next_phase(&result);
        let media = Arc::clone(&self.state.media);
        responder
            .followup(blocking(move || Ok(boss_resume_response(&result, &media))).await?)
            .await
            .map_err(|error| error.to_string())?;
        self.post_boss_neon(action_id, user_id, guild_id, channel_id, neon_victory)
            .await;
        if next_phase {
            responder
                .followup(
                    self.render_boss_encounter(user_id, guild_id, unix_now())
                        .await?,
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn handle_guide_component(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("guide:") {
            let mut parts = raw.split(':');
            let token = parts.next().unwrap_or_default();
            let direction = match parts.next() {
                Some("previous") => Some(DigGuideDirection::Previous),
                Some("next") => Some(DigGuideDirection::Next),
                _ => None,
            };
            if token.is_empty() || direction.is_none() || parts.next().is_some() {
                return responder
                    .update(expired_guide_response())
                    .await
                    .map_err(|error| error.to_string());
            }
            match self.navigate_guide_view(
                token,
                user_id,
                guild_id,
                direction.expect("direction checked above"),
                unix_now(),
            )? {
                DigGuideViewAdmission::Admitted(page) => {
                    return responder
                        .update(guide_response(page, token))
                        .await
                        .map_err(|error| error.to_string());
                }
                DigGuideViewAdmission::WrongOwner => {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "Only the person who opened this guide can page it. Run `/dig guide` for your own copy.",
                        )
                        .ephemeral(),
                    )
                    .await;
                }
                DigGuideViewAdmission::Expired => {
                    return responder
                        .update(expired_guide_response())
                        .await
                        .map_err(|error| error.to_string());
                }
            }
        }
        expired_dig_component(&responder).await
    }

    async fn handle_prestige_component(
        &self,
        action: &str,
        user_id: i64,
        user_display_name: &str,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("prestige-perk:") {
            let mut parts = raw.split(':');
            let token = parts.next().unwrap_or_default();
            let perk = parts.next().unwrap_or_default().to_owned();
            let mutation = parts
                .next()
                .filter(|value| *value != "_")
                .map(str::to_owned);
            if token.is_empty() || perk.is_empty() || parts.next().is_some() {
                return respond(
                    &responder,
                    InteractionResponse::message("This prestige selection is no longer available.")
                        .ephemeral(),
                )
                .await;
            }
            let now = unix_now();
            if let Some(response) = prestige_admission_response(
                self.inspect_prestige_view(token, user_id, guild_id, now)?,
            ) {
                return respond(&responder, response).await;
            }
            let path = self.state.database_path.clone();
            let preview = blocking(move || {
                DigPrestigeRuntimeService::sqlite(&path)
                    .preview(user_id, guild_id)
                    .map_err(|error| error.to_string())
            })
            .await?;
            let offered = preview
                .offered_perks
                .iter()
                .any(|offered| offered.id == perk);
            let valid_mutation = match &preview.mutation {
                Some(roll) => mutation.as_deref().is_some_and(|selected| {
                    roll.choices.iter().any(|choice| choice.id == selected)
                }),
                None => mutation.is_none(),
            };
            if !offered || !valid_mutation {
                return responder
                    .update(
                        InteractionResponse::message(if offered {
                            "That mutation is no longer available."
                        } else {
                            "Invalid perk choice."
                        })
                        .ephemeral()
                        .action_rows(Vec::new()),
                    )
                    .await
                    .map_err(|error| error.to_string());
            }
            if let Some(response) = prestige_admission_response(self.claim_prestige_view(
                token,
                user_id,
                guild_id,
                mutation.as_deref(),
                now,
            )?) {
                return respond(&responder, response).await;
            }
            let path = self.state.database_path.clone();
            let result = blocking(move || {
                DigPrestigeRuntimeService::sqlite(&path)
                    .prestige(DigPrestigeRequest {
                        discord_id: user_id,
                        guild_id,
                        perk_choice: &perk,
                        mutation_choice: mutation.as_deref(),
                        now,
                    })
                    .map_err(|error| error.to_string())
            })
            .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return responder
                        .update(
                            InteractionResponse::message(error)
                                .ephemeral()
                                .action_rows(Vec::new()),
                        )
                        .await
                        .map_err(|error| error.to_string());
                }
            };
            responder
                .update(prestige_result_response(&result))
                .await
                .map_err(|error| error.to_string())?;
            if let Some(target) = self
                .public_channel(guild_id, channel_id)
                .await
                .ok()
                .flatten()
            {
                let announcement =
                    InteractionResponse::message(format!("*{user_display_name} has ascended.*"))
                        .without_mentions();
                let _ = self
                    .state
                    .discord
                    .dig_send_public(target, announcement)
                    .await;
                if let Ok(Some(neon)) = self.prestige_neon_response(user_id, guild_id).await {
                    let _ = self
                        .state
                        .discord
                        .dig_send_temporary(target, neon, Duration::from_secs(60))
                        .await;
                }
            }
            return Ok(());
        }
        if let Some(raw) = action.strip_prefix("prestige-mutation:") {
            let mut parts = raw.split(':');
            let token = parts.next().unwrap_or_default();
            let mutation = parts.next().unwrap_or_default().to_owned();
            if token.is_empty() || mutation.is_empty() || parts.next().is_some() {
                return respond(
                    &responder,
                    InteractionResponse::message("This prestige selection is no longer available.")
                        .ephemeral(),
                )
                .await;
            }
            let now = unix_now();
            if let Some(response) = prestige_admission_response(
                self.inspect_prestige_view(token, user_id, guild_id, now)?,
            ) {
                return respond(&responder, response).await;
            }
            let path = self.state.database_path.clone();
            let preview = blocking(move || {
                DigPrestigeRuntimeService::sqlite(&path)
                    .preview(user_id, guild_id)
                    .map_err(|error| error.to_string())
            })
            .await?;
            if preview
                .mutation
                .as_ref()
                .is_none_or(|roll| !roll.choices.iter().any(|choice| choice.id == mutation))
            {
                return respond(
                    &responder,
                    InteractionResponse::message("That mutation is no longer available.")
                        .ephemeral(),
                )
                .await;
            }
            if let Some(response) = prestige_admission_response(
                self.select_prestige_mutation(token, user_id, guild_id, &mutation, now)?,
            ) {
                return respond(&responder, response).await;
            }
            return responder
                .update(prestige_perk_response(&preview, token, Some(&mutation)))
                .await
                .map_err(|error| error.to_string());
        }
        expired_dig_component(&responder).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_paid_component(
        &self,
        action: &str,
        interaction_id: u64,
        user_id: i64,
        user_display_name: String,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("paid:confirm:") {
            if raw.is_empty() || raw.contains(':') {
                return responder
                    .update(InteractionResponse::message("Dig cancelled.").action_rows(Vec::new()))
                    .await
                    .map_err(|error| error.to_string());
            }
            match self.claim_paid_view(raw, user_id, guild_id, unix_now())? {
                DigPaidViewAdmission::Admitted => {}
                DigPaidViewAdmission::WrongOwner => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This isn't your dig.").ephemeral(),
                    )
                    .await;
                }
                DigPaidViewAdmission::Expired => {
                    return responder
                        .update(
                            InteractionResponse::message("Dig cancelled.").action_rows(Vec::new()),
                        )
                        .await
                        .map_err(|error| error.to_string());
                }
                DigPaidViewAdmission::AlreadyClaimed => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This paid dig was already answered.")
                            .ephemeral(),
                    )
                    .await;
                }
            }
            responder
                .update(
                    InteractionResponse::message("").embed(
                        InteractionEmbed::titled("Digging...")
                            .description("Your pickaxe swings.")
                            .color(0xFF_A5_00),
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
            let pending = self
                .pending_deliveries(DigRuntimePendingDeliveryQuery {
                    guild_id: Some(guild_id),
                    discord_id: Some(user_id),
                    limit: 10,
                })
                .await?;
            if !pending.is_empty() {
                for delivery in pending {
                    if let Err(error) = self.deliver_to_channel(&delivery).await {
                        warn!(
                            action_id = delivery.action_id,
                            %error,
                            "pending Dig delivery blocked paid Dig"
                        );
                    }
                }
                return responder
                    .edit_original(InteractionResponse::message(
                        "Your previous dig was already committed. Its result is being recovered; you were not charged for another dig. Run `/dig go` again after it appears.",
                    ))
                    .await
                    .map_err(|error| error.to_string());
            }
            let now = unix_now();
            let forced_event = self.force_event_pending(user_id, guild_id)?;
            let delivery_channel = self
                .public_channel(guild_id, channel_id)
                .await?
                .or(channel_id)
                .ok_or_else(|| "Paid Dig interaction is missing its channel".to_owned())?;
            let avatar = self
                .state
                .discord
                .dig_user_avatar_url(guild_id, user_id)
                .await
                .ok()
                .flatten();
            let result = match self
                .run_dig(
                    user_id,
                    guild_id,
                    now,
                    forced_event,
                    true,
                    DigRuntimeDeliveryContext::new(
                        interaction_id,
                        delivery_channel,
                        user_display_name.clone(),
                        avatar.clone(),
                    ),
                )
                .await
            {
                Ok(result) => result,
                Err(_) => {
                    return responder
                        .edit_original(InteractionResponse::message("Paid dig failed."))
                        .await
                        .map_err(|error| error.to_string());
                }
            };
            if result.forced_event_consumed {
                self.consume_force_event(user_id, guild_id)?;
            }
            if !result.success {
                return responder
                    .edit_original(InteractionResponse::message(
                        result
                            .error
                            .clone()
                            .unwrap_or_else(|| "Paid dig failed.".to_owned()),
                    ))
                    .await
                    .map_err(|error| error.to_string());
            }
            self.reconcile_dig_reminder(user_id, guild_id, now).await;
            let delivery = match result.delivery.as_ref() {
                Some(delivery) => Some(self.prepare_delivery(delivery).await?),
                None => None,
            };
            let bonus_outcome = result.outcome.clone();
            let (stats, event) = if let Some(delivery) = delivery.as_ref() {
                dig_delivery_responses(delivery, &self.state.media, &self.state.view_nonce)
            } else if result.boss_boundary.is_some() {
                (
                    self.render_boss_encounter(user_id, guild_id, unix_now())
                        .await?,
                    None,
                )
            } else {
                self.render_dig_responses(
                    bonus_outcome.clone(),
                    user_id,
                    guild_id,
                    user_display_name,
                    avatar,
                )
                .await?
            };
            responder
                .edit_original(stats)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(delivery) = delivery.as_ref() {
                self.mark_delivery_part(delivery, DigRuntimeDeliveryPart::Main, unix_now())
                    .await?;
            }
            if let Some(event) = event {
                responder
                    .followup(event)
                    .await
                    .map_err(|error| error.to_string())?;
                if let Some(delivery) = delivery.as_ref() {
                    self.mark_delivery_part(delivery, DigRuntimeDeliveryPart::Event, unix_now())
                        .await?;
                }
            }
            self.maybe_send_dig_bonus(
                &bonus_outcome,
                user_id,
                guild_id,
                delivery_channel,
                Arc::clone(&responder),
            )
            .await;
            return Ok(());
        }
        if let Some(raw) = action.strip_prefix("paid:cancel:") {
            if raw.is_empty() || raw.contains(':') {
                return responder
                    .update(InteractionResponse::message("Dig cancelled.").action_rows(Vec::new()))
                    .await
                    .map_err(|error| error.to_string());
            }
            match self.claim_paid_view(raw, user_id, guild_id, unix_now())? {
                DigPaidViewAdmission::Admitted | DigPaidViewAdmission::Expired => {}
                DigPaidViewAdmission::WrongOwner => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This isn't your dig.").ephemeral(),
                    )
                    .await;
                }
                DigPaidViewAdmission::AlreadyClaimed => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This paid dig was already answered.")
                            .ephemeral(),
                    )
                    .await;
                }
            }
            return responder
                .update(InteractionResponse::message("Dig cancelled.").action_rows(Vec::new()))
                .await
                .map_err(|error| error.to_string());
        }
        expired_dig_component(&responder).await
    }

    async fn handle_event_component(
        &self,
        action: &str,
        interaction_id: u64,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("event-action:") {
            let mut parts = raw.split(':');
            let nonce = parts.next().unwrap_or_default();
            let action_id = parts.next().and_then(|value| value.parse::<i64>().ok());
            let choice = parts.next().unwrap_or_default().to_owned();
            if nonce != self.state.view_nonce
                || action_id.is_none()
                || choice.is_empty()
                || parts.next().is_some()
            {
                return respond(
                    &responder,
                    InteractionResponse::message("This Dig event expired.").ephemeral(),
                )
                .await;
            }
            let action_id = action_id.expect("validated action id");
            let now = unix_now();
            let path = self.state.database_path.clone();
            let config = self.state.dig_config.clone();
            let prompt = match blocking(move || {
                cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
                    &path, config,
                )
                .action_presentation(user_id, guild_id, action_id, now)
                .map_err(|error| error.to_string())
            })
            .await
            {
                Ok(Some(prompt)) => prompt,
                Ok(None) => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This isn't your event.").ephemeral(),
                    )
                    .await;
                }
                Err(_) => {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "This Dig event could not be loaded. Try again.",
                        )
                        .ephemeral(),
                    )
                    .await;
                }
            };
            if !event_choice_is_valid(&prompt, &choice) {
                return respond(
                    &responder,
                    InteractionResponse::message("That event choice is no longer available.")
                        .ephemeral(),
                )
                .await;
            }

            // Match Python's resolved-view boundary: acknowledge the click by
            // preserving the source embed/attachments and disabling every
            // control, then post the independently rendered outcome. Two
            // concurrent deliveries may both lock the view, but the durable
            // application receipt admits only one public result.
            responder
                .update(locked_event_controls(
                    &prompt,
                    &self.state.view_nonce,
                    action_id,
                ))
                .await
                .map_err(|error| error.to_string())?;

            let delivery_channel = channel_id
                .ok_or_else(|| "Dig event interaction is missing its channel".to_owned())?;
            let event_delivery_context =
                DigEventDeliveryContext::new(user_id, guild_id, interaction_id, delivery_channel);
            let path = self.state.database_path.clone();
            let config = self.state.dig_config.clone();
            let choice_for_db = choice.clone();
            let result = match blocking(move || {
                cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
                    &path, config,
                )
                .resolve_action_event_with_delivery(
                    cama_app::dig_event_runtime::DigEventActionRequest {
                        discord_id: user_id,
                        guild_id,
                        dig_action_id: action_id,
                        choice: &choice_for_db,
                        now,
                    },
                    event_delivery_context,
                )
                .map_err(|error| error.to_string())
            })
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    return responder
                        .followup(
                            InteractionResponse::message(
                                "Event choice could not be resolved. Try `/dig go` to continue.",
                            )
                            .ephemeral(),
                        )
                        .await
                        .map_err(|error| error.to_string());
                }
            };
            if !result.success {
                let response = event_resolution_response(&result);
                return responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string());
            }
            let resolved_action_id = result
                .action_id
                .ok_or_else(|| "Resolved Dig event has no durable action id".to_owned())?;
            let Some(delivery) = self
                .event_delivery_for_action(resolved_action_id, user_id, guild_id)
                .await?
            else {
                return responder
                    .followup(
                        InteractionResponse::message("You've already resolved this event.")
                            .ephemeral(),
                    )
                    .await
                    .map_err(|error| error.to_string());
            };
            let response = event_resolution_response(&delivery.outcome);
            if result.applied_now {
                return self
                    .deliver_event_from_interaction(&delivery, response, responder)
                    .await;
            }
            // A prior click may have settled the actor but lost the delivery
            // CAS. Retry the immutable Ready projection by nonce/history;
            // never bind this newer interaction to the old outbox context.
            return self.deliver_event_to_channel(&delivery).await;
        }
        // Messages emitted by the pre-cutover provider carried the authored
        // event id in the component instead of a durable Dig action id. They
        // cannot be admitted safely because a client can substitute another
        // event and there is no stable duplicate-delivery key.
        if action.starts_with("event:") {
            return respond(
                &responder,
                InteractionResponse::message(
                    "This Dig event predates durable recovery. Use `/dig go` to continue.",
                )
                .ephemeral(),
            )
            .await;
        }
        expired_dig_component(&responder).await
    }

    async fn handle_sabotage_component(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("sabotage:confirm:") {
            if raw.is_empty() || raw.contains(':') {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This sabotage expired. Use `/dig sabotage` again.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            let now = unix_now();
            let view = match self.claim_sabotage_view(raw, user_id, guild_id, now)? {
                DigSabotageViewAdmission::Admitted(view) => view,
                DigSabotageViewAdmission::WrongOwner => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This isn't your sabotage.").ephemeral(),
                    )
                    .await;
                }
                DigSabotageViewAdmission::Expired => {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "This sabotage expired. Use `/dig sabotage` again.",
                        )
                        .ephemeral(),
                    )
                    .await;
                }
                DigSabotageViewAdmission::AlreadyResolved => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This sabotage was already resolved.")
                            .ephemeral(),
                    )
                    .await;
                }
            };
            let path = self.state.database_path.clone();
            let target_id = view.target_id;
            let target_name = view.target_name;
            let result = blocking(move || {
                Ok(DigSocialRuntimeService::sqlite(&path)
                    .sabotage(user_id, target_id, guild_id, now))
            })
            .await?;
            return responder
                .update(
                    match result {
                        Ok(result) => sabotage_result_response(&result, &target_name),
                        Err(error) => InteractionResponse::message(error.to_string()),
                    }
                    .action_rows(Vec::new()),
                )
                .await
                .map_err(|error| error.to_string());
        }
        if let Some(raw) = action.strip_prefix("sabotage:cancel:") {
            if raw.is_empty() || raw.contains(':') {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This sabotage expired. Use `/dig sabotage` again.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            match self.claim_sabotage_view(raw, user_id, guild_id, unix_now())? {
                DigSabotageViewAdmission::Admitted(_) => {}
                DigSabotageViewAdmission::WrongOwner => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This isn't your sabotage.").ephemeral(),
                    )
                    .await;
                }
                DigSabotageViewAdmission::Expired => {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "This sabotage expired. Use `/dig sabotage` again.",
                        )
                        .ephemeral(),
                    )
                    .await;
                }
                DigSabotageViewAdmission::AlreadyResolved => {
                    return respond(
                        &responder,
                        InteractionResponse::message("This sabotage was already resolved.")
                            .ephemeral(),
                    )
                    .await;
                }
            }
            return responder
                .update(InteractionResponse::message("Sabotage cancelled.").action_rows(Vec::new()))
                .await
                .map_err(|error| error.to_string());
        }
        if action == "sabotage:cancel" {
            return respond(
                &responder,
                InteractionResponse::message("This sabotage expired. Use `/dig sabotage` again.")
                    .ephemeral(),
            )
            .await;
        }
        expired_dig_component(&responder).await
    }

    async fn handle_abandon_component(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if action == "abandon:confirm" || action == "abandon:cancel" {
            return respond(
                &responder,
                InteractionResponse::message("This abandonment expired. Use `/dig abandon` again.")
                    .ephemeral(),
            )
            .await;
        }
        if let Some(raw) = action.strip_prefix("abandon:confirm:") {
            let mut parts = raw.split(':');
            let token = parts.next().unwrap_or_default();
            if token.is_empty() || parts.next().is_some() {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This abandonment expired. Use `/dig abandon` again.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            let now = unix_now();
            if let Some(response) =
                abandon_admission_response(self.claim_abandon_view(token, user_id, guild_id, now)?)
            {
                return respond(&responder, response).await;
            }
            let path = self.state.database_path.clone();
            let result = blocking(move || {
                DigAbandonRuntimeService::sqlite(&path)
                    .abandon(user_id, guild_id, now)
                    .map_err(|error| error.to_string())
            })
            .await;
            return responder
                .update(
                    match result {
                        Ok(result) => InteractionResponse::message(format!(
                            "Tunnel abandoned. You received **{}** {JOPACOIN_EMOTE}.",
                            result.refund
                        )),
                        Err(_) => InteractionResponse::message("Abandon failed."),
                    }
                    .action_rows(Vec::new()),
                )
                .await
                .map_err(|error| error.to_string());
        }
        if let Some(raw) = action.strip_prefix("abandon:cancel:") {
            let mut parts = raw.split(':');
            let token = parts.next().unwrap_or_default();
            if token.is_empty() || parts.next().is_some() {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This abandonment expired. Use `/dig abandon` again.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            if let Some(response) = abandon_admission_response(self.claim_abandon_view(
                token,
                user_id,
                guild_id,
                unix_now(),
            )?) {
                return respond(&responder, response).await;
            }
            return responder
                .update(InteractionResponse::message("Abandon cancelled.").action_rows(Vec::new()))
                .await
                .map_err(|error| error.to_string());
        }
        expired_dig_component(&responder).await
    }

    async fn handle_boss_component(
        &self,
        action: &str,
        user_id: i64,
        user_display_name: &str,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if let Some(raw) = action.strip_prefix("boss:fight:") {
            let (mode, raw) = if let Some(raw) = raw.strip_prefix("carried:") {
                (Some("carried"), raw)
            } else if let Some(raw) = raw.strip_prefix("wager:") {
                (Some("wager"), raw)
            } else if let Some(raw) = raw.strip_prefix("risk:") {
                (Some("risk"), raw)
            } else {
                (None, raw)
            };
            let (owner_id, expected_guild) = parse_boss_owner(raw)?;
            if owner_id != user_id || expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can fight.").ephemeral(),
                )
                .await;
            }
            if matches!(mode, Some("wager" | "risk")) {
                return show_boss_modal(
                    responder.as_ref(),
                    owner_id,
                    guild_id,
                    mode == Some("wager"),
                )
                .await;
            }
            if mode == Some("carried") {
                responder
                    .defer(false)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let info = self.boss_encounter(user_id, guild_id, unix_now()).await?;
            if let (Some(wager), Some(risk_tier)) = (info.carried_wager, info.carried_risk_tier) {
                if mode != Some("carried") {
                    responder
                        .defer(false)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let now = unix_now();
                let result = match self
                    .start_boss(user_id, guild_id, risk_tier, wager, now)
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return responder
                            .followup(boss_error_response(error))
                            .await
                            .map_err(|error| error.to_string());
                    }
                };
                if boss_start_is_resolved(&result) {
                    self.reconcile_resolved_boss(user_id, guild_id, now).await;
                }
                let action_id = result.action_id;
                let neon_victory = boss_start_neon_victory(&result);
                let media = Arc::clone(&self.state.media);
                let response =
                    blocking(move || Ok(boss_start_response(&result, user_id, guild_id, &media)))
                        .await?;
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())?;
                self.post_boss_neon(action_id, user_id, guild_id, channel_id, neon_victory)
                    .await;
                return Ok(());
            }
            if mode == Some("carried") {
                return responder
                    .followup(
                        InteractionResponse::message(
                            "This carried boss wager is no longer active. Use `/dig go` to continue.",
                        )
                        .ephemeral(),
                    )
                    .await
                    .map_err(|error| error.to_string());
            }
            return show_boss_modal(responder.as_ref(), owner_id, guild_id, info.wager_allowed)
                .await;
        }
        if let Some(raw) = action.strip_prefix("boss:duel:") {
            let mut parts = raw.split(':');
            let owner_id = parts.next().and_then(|value| value.parse::<i64>().ok());
            let expected_guild = parts.next().and_then(|value| value.parse::<i64>().ok());
            let option_index = parts.next().and_then(|value| value.parse::<usize>().ok());
            if parts.next().is_some() || owner_id.is_none() || option_index.is_none() {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This boss choice expired. Use `/dig go` to continue.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            if owner_id != Some(user_id) || expected_guild != Some(guild_id) {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can choose.").ephemeral(),
                )
                .await;
            }
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            let now = unix_now();
            let result = self
                .resume_boss(
                    user_id,
                    guild_id,
                    option_index.expect("validated option index"),
                    now,
                )
                .await?;
            if boss_resume_is_resolved(&result) {
                self.reconcile_resolved_boss(user_id, guild_id, now).await;
            }
            let action_id = result.action_id;
            let neon_victory = boss_resume_neon_victory(&result);
            let next_phase = boss_resume_has_next_phase(&result);
            let media = Arc::clone(&self.state.media);
            let response = blocking(move || Ok(boss_resume_response(&result, &media))).await?;
            responder
                .followup(response)
                .await
                .map_err(|error| error.to_string())?;
            self.post_boss_neon(action_id, user_id, guild_id, channel_id, neon_victory)
                .await;
            if next_phase {
                responder
                    .followup(
                        self.render_boss_encounter(user_id, guild_id, unix_now())
                            .await?,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            return Ok(());
        }
        if let Some(raw) = action.strip_prefix("boss:scout:") {
            let (owner_id, expected_guild) = parse_boss_owner(raw)?;
            if owner_id != user_id || expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can scout.").ephemeral(),
                )
                .await;
            }
            responder
                .defer(true)
                .await
                .map_err(|error| error.to_string())?;
            let result = self.scout_boss(user_id, guild_id, unix_now()).await?;
            return responder
                .followup(boss_scout_response(&result).ephemeral())
                .await
                .map_err(|error| error.to_string());
        }
        if let Some(raw) = action.strip_prefix("boss:retreat:") {
            let (owner_id, expected_guild) = parse_boss_owner(raw)?;
            if owner_id != user_id || expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can retreat.").ephemeral(),
                )
                .await;
            }
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            let result = self.retreat_boss(user_id, guild_id, unix_now()).await?;
            let mut content = format!(
                "You retreated safely, losing {} blocks. Now at depth {}.",
                result.outcome.block_loss, result.outcome.new_depth
            );
            if result.outcome.carried_wager_forfeit > 0 {
                content.push_str(&format!(
                    " Half the carried wager was forfeited: **{}** {JOPACOIN_EMOTE}.",
                    result.outcome.carried_wager_forfeit
                ));
            }
            return responder
                .followup(InteractionResponse::message(content))
                .await
                .map_err(|error| error.to_string());
        }
        if let Some(raw) = action.strip_prefix("boss:cheer:") {
            let (target_id, expected_guild) = parse_boss_owner(raw)?;
            if expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("This boss fight is in another server.")
                        .ephemeral(),
                )
                .await;
            }
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            let result = self
                .cheer_boss(user_id, target_id, guild_id, unix_now())
                .await?;
            return responder
                .followup(InteractionResponse::message(format!(
                    "{user_display_name} cheers for the fighter! Boss odds boosted by +{}% ({}/3 cheers)",
                    (result.total_boost * 100.0) as i32,
                    result.cheer_count
                )))
                .await
                .map_err(|error| error.to_string());
        }
        expired_dig_component(&responder).await
    }

    async fn handle_modal(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Modal {
            custom_id,
            user_id,
            guild_id,
            channel_id,
            fields,
            ..
        } = request
        else {
            return Err("invalid Dig modal payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        let Some(guild_id) = guild_id else {
            return respond(
                &responder,
                InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral(),
            )
            .await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let channel_id = channel_id.map(|id| signed_id(id, "channel")).transpose()?;
        let (raw_owner, wager_allowed) =
            if let Some(raw) = custom_id.strip_prefix("dig:boss:wager:") {
                (Some(raw), true)
            } else if let Some(raw) = custom_id.strip_prefix("dig:boss:risk:") {
                (Some(raw), false)
            } else {
                (None, false)
            };
        if let Some(raw_owner) = raw_owner {
            let (owner_id, expected_guild) = parse_boss_owner(raw_owner)?;
            if owner_id != user_id || expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("This isn't your boss fight.").ephemeral(),
                )
                .await;
            }
            let Some(risk_tier) = fields
                .get("risk_tier")
                .and_then(|value| parse_risk_tier(value))
            else {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "Invalid risk tier. Choose: cautious, bold, or reckless.",
                    )
                    .ephemeral(),
                )
                .await;
            };
            let wager = if wager_allowed {
                let Some(raw) = fields.get("wager") else {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "Invalid wager amount. Please enter a number.",
                        )
                        .ephemeral(),
                    )
                    .await;
                };
                let Ok(wager) = raw.trim().parse::<i64>() else {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "Invalid wager amount. Please enter a number.",
                        )
                        .ephemeral(),
                    )
                    .await;
                };
                if wager < 0 {
                    return respond(
                        &responder,
                        InteractionResponse::message("Wager must be non-negative.").ephemeral(),
                    )
                    .await;
                }
                wager
            } else {
                0
            };
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            let now = unix_now();
            let result = match self
                .start_boss(user_id, guild_id, risk_tier, wager, now)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    return responder
                        .followup(boss_error_response(error))
                        .await
                        .map_err(|error| error.to_string());
                }
            };
            if boss_start_is_resolved(&result) {
                self.reconcile_resolved_boss(user_id, guild_id, now).await;
            }
            let action_id = result.action_id;
            let neon_victory = boss_start_neon_victory(&result);
            let next_phase = boss_start_has_next_phase(&result);
            let media = Arc::clone(&self.state.media);
            let response =
                blocking(move || Ok(boss_start_response(&result, user_id, guild_id, &media)))
                    .await?;
            responder
                .followup(response)
                .await
                .map_err(|error| error.to_string())?;
            self.post_boss_neon(action_id, user_id, guild_id, channel_id, neon_victory)
                .await;
            if next_phase {
                responder
                    .followup(
                        self.render_boss_encounter(user_id, guild_id, unix_now())
                            .await?,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            return Ok(());
        }
        respond(
            &responder,
            InteractionResponse::message("This Dig modal expired. Use `/dig go` to reopen it.")
                .ephemeral(),
        )
        .await
    }

    fn take_rate_limit(
        &self,
        user_id: i64,
        guild_id: i64,
        scope: &'static str,
        limit: usize,
        window: Duration,
    ) -> Result<Option<u64>, String> {
        let now = Instant::now();
        let mut all = self
            .state
            .rate_limits
            .lock()
            .map_err(|_| "Dig rate-limit lock poisoned")?;
        // Discard buckets whose newest hit fell out of its window so the map
        // stays bounded by currently-active users instead of growing for the
        // process lifetime. Windows differ per scope, so use each bucket's
        // own recency rather than this call's window.
        all.retain(|_, bucket| {
            bucket
                .back()
                .is_some_and(|latest| now.duration_since(*latest) <= RATE_LIMIT_BUCKET_RETENTION)
        });
        let hits = all.entry((user_id, guild_id, scope)).or_default();
        while hits
            .front()
            .is_some_and(|started| now.duration_since(*started) > window)
        {
            hits.pop_front();
        }
        if hits.len() < limit {
            hits.push_back(now);
            return Ok(None);
        }
        Ok(hits.front().map(|started| {
            let remaining = window.saturating_sub(now.duration_since(*started));
            remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() != 0))
        }))
    }

    async fn require_dig_channel(
        &self,
        user_id: i64,
        guild_id: i64,
        current_channel_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let Some(current_channel_id) = current_channel_id else {
            return respond(
                responder,
                InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral(),
            )
            .await
            .map(|()| false);
        };
        if let Some(expected) = self.state.configured_channel_id {
            let configured = self.state.discord.dig_channel(expected).await?;
            if configured.is_some_and(|channel| channel.guild_id == Some(guild_id)) {
                let current = self.state.discord.dig_channel(current_channel_id).await?;
                if current.as_ref().is_some_and(|channel| {
                    channel.id == expected || channel.parent_id == Some(expected)
                }) {
                    return Ok(true);
                }
                self.debit_channel_penalty(user_id, guild_id).await?;
                return respond(
                    responder,
                    InteractionResponse::message(format!(
                        "The earth here is silent. Your tools belong in <#{expected}> — a single jopacoin dissolves into the ether as penance."
                    ))
                    .ephemeral(),
                )
                .await
                .map(|()| false);
            }
        }
        if self
            .state
            .discord
            .dig_channel_is_gamba(guild_id, current_channel_id)
            .await?
        {
            return Ok(true);
        }
        self.debit_channel_penalty(user_id, guild_id).await?;
        respond(
            responder,
            InteractionResponse::message(
                "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.",
            )
            .ephemeral(),
        )
        .await
        .map(|()| false)
    }

    async fn registered(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        let players = PlayerRepository::new(&self.state.database_path);
        blocking(move || {
            players
                .exists(user_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn debit_channel_penalty(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        let path = self.state.database_path.clone();
        blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .debit_channel_penalty(user_id, guild_id, WRONG_CHANNEL_PENALTY)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn command_go(
        &self,
        interaction_id: u64,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        display_name: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let now = unix_now();
        let avatar = self
            .state
            .discord
            .dig_user_avatar_url(guild_id, user_id)
            .await
            .ok()
            .flatten();
        let delivery_channel = self
            .public_channel(guild_id, channel_id)
            .await?
            .or(channel_id)
            .ok_or_else(|| "Dig interaction is missing its channel".to_owned())?;
        for pending in self
            .pending_deliveries(DigRuntimePendingDeliveryQuery {
                guild_id: Some(guild_id),
                discord_id: Some(user_id),
                limit: 10,
            })
            .await?
        {
            match self.deliver_to_channel_with_failure(&pending).await {
                Ok(()) => {}
                Err(DigDeliveryFailure::SafeFallback { part, error }) => {
                    let Some(interaction_channel) = channel_id else {
                        return Err(error);
                    };
                    if pending.context.channel_id == interaction_channel {
                        return Err(error);
                    }
                    let fallback = self
                        .rebind_delivery_channel(
                            &pending,
                            part,
                            pending.context.channel_id,
                            interaction_channel,
                        )
                        .await
                        .map_err(|rebind_error| {
                            format!(
                                "{error}; fallback delivery channel could not be persisted: {rebind_error}"
                            )
                        })?;
                    self.deliver_to_channel_with_failure(&fallback)
                        .await
                        .map_err(|failure| match failure {
                            DigDeliveryFailure::SafeFallback { error, .. }
                            | DigDeliveryFailure::Ambiguous(error) => error,
                        })?;
                }
                Err(DigDeliveryFailure::Ambiguous(error)) => return Err(error),
            }
        }
        let forced_event = self.force_event_pending(user_id, guild_id)?;
        let result = self
            .run_dig(
                user_id,
                guild_id,
                now,
                forced_event,
                false,
                DigRuntimeDeliveryContext::new(
                    interaction_id,
                    delivery_channel,
                    display_name,
                    avatar.clone(),
                ),
            )
            .await?;
        if result.forced_event_consumed {
            self.consume_force_event(user_id, guild_id)?;
        }
        if result.route_choice_required {
            let Some(choice) = self.load_route_choice(user_id, guild_id).await? else {
                return responder
                    .followup(
                        InteractionResponse::message(
                            "The junction could not be read. Use `/dig go` to try again.",
                        )
                        .ephemeral(),
                    )
                    .await
                    .map_err(|error| error.to_string());
            };
            let token = self.create_route_view(user_id, guild_id, choice.clone(), now)?;
            if let Err(error) = self
                .send_route_choice_view(
                    token.clone(),
                    choice,
                    user_id,
                    guild_id,
                    channel_id,
                    &responder,
                )
                .await
            {
                let _ = self.retire_route_view(&token);
                return Err(error);
            }
            return Ok(());
        }
        if result.success {
            self.reconcile_dig_reminder(user_id, guild_id, now).await;
        }
        if result.paid_dig_available {
            let token = self.create_paid_view(user_id, guild_id, now)?;
            let receipt = responder
                .followup_with_receipt(paid_dig_response(&result, &token))
                .await
                .map_err(|error| error.to_string())?;
            self.schedule_paid_view_timeout(token, responder, receipt);
            return Ok(());
        }
        let delivery = match result.delivery.as_ref() {
            Some(delivery) => Some(self.prepare_delivery(delivery).await?),
            None => None,
        };
        if result.success && !result.first_dig && !result.paid_dig_available {
            self.post_result_hooks(
                &result.outcome,
                user_id,
                guild_id,
                channel_id.unwrap_or(delivery_channel),
            )
            .await;
        }
        let reaction_outcome = result.outcome.clone();
        let (response, event_response) = if let Some(delivery) = delivery.as_ref() {
            dig_delivery_responses(delivery, &self.state.media, &self.state.view_nonce)
        } else if result.boss_boundary.is_some() {
            (
                self.render_boss_encounter(user_id, guild_id, now).await?,
                None,
            )
        } else {
            self.render_dig_responses(
                reaction_outcome.clone(),
                user_id,
                guild_id,
                display_name.to_owned(),
                avatar,
            )
            .await?
        };
        let target = self.public_channel(guild_id, channel_id).await?;
        if target.is_some_and(|target| Some(target) != channel_id) {
            // Configured-channel delivery is durable and nonce-addressed just
            // like READY recovery.  The delivery context was created with
            // this target above, so its per-part nonce/history lookup is bound
            // to the configured channel rather than the interaction channel.
            if let Some(delivery) = delivery.as_ref() {
                match self.deliver_to_channel_with_failure(delivery).await {
                    Ok(()) => {
                        self.maybe_send_dig_bonus(
                            &reaction_outcome,
                            user_id,
                            guild_id,
                            delivery_channel,
                            Arc::clone(&responder),
                        )
                        .await;
                        return Ok(());
                    }
                    Err(DigDeliveryFailure::SafeFallback { part, error }) => {
                        // Preserve Python's channel fallback, but keep it in
                        // the same durable nonce/history path.  Rebind the
                        // immutable snapshot to the interaction channel so a
                        // crash after this fallback send cannot cause READY
                        // recovery to repost to the configured channel.
                        let Some(interaction_channel) = channel_id else {
                            return Err(error);
                        };
                        let fallback = self
                            .rebind_delivery_channel(
                                delivery,
                                part,
                                delivery.context.channel_id,
                                interaction_channel,
                            )
                            .await
                            .map_err(|rebind_error| {
                                format!(
                                    "{error}; fallback delivery channel could not be persisted: {rebind_error}"
                                )
                            })?;
                        self.deliver_to_channel_with_failure(&fallback)
                            .await
                            .map_err(|failure| match failure {
                                DigDeliveryFailure::SafeFallback { error, .. }
                                | DigDeliveryFailure::Ambiguous(error) => error,
                            })?;
                        self.maybe_send_dig_bonus(
                            &reaction_outcome,
                            user_id,
                            guild_id,
                            interaction_channel,
                            Arc::clone(&responder),
                        )
                        .await;
                        return Ok(());
                    }
                    Err(DigDeliveryFailure::Ambiguous(error)) => {
                        // Never publish an interaction duplicate when the
                        // configured send may already have been accepted.
                        return Err(error);
                    }
                }
            }
        }
        let main_receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(receipt) = main_receipt.as_ref() {
            self.add_result_reactions(receipt, delivery.as_ref(), &reaction_outcome)
                .await;
        }
        if let Some(delivery) = delivery.as_ref() {
            self.mark_delivery_part(delivery, DigRuntimeDeliveryPart::Main, unix_now())
                .await?;
        }
        if let Some(event_response) = event_response {
            responder
                .followup(event_response)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(delivery) = delivery.as_ref() {
                self.mark_delivery_part(delivery, DigRuntimeDeliveryPart::Event, unix_now())
                    .await?;
            }
        }
        self.maybe_send_dig_bonus(
            &reaction_outcome,
            user_id,
            guild_id,
            channel_id.unwrap_or(delivery_channel),
            Arc::clone(&responder),
        )
        .await;
        Ok(())
    }

    async fn load_route_choice(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigRouteChoiceView>, String> {
        let path = self.state.database_path.clone();
        let info = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .tunnel_info(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(info.and_then(|info| route_choice_from_state(info.route_state.as_deref())))
    }

    async fn send_route_choice_view(
        &self,
        token: String,
        choice: DigRouteChoiceView,
        owner_id: i64,
        guild_id: i64,
        current_channel_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let response = route_choice_response(
            &choice,
            &self.state.view_nonce,
            owner_id,
            guild_id,
            &token,
            false,
        );
        let target = self.public_channel(guild_id, current_channel_id).await?;
        if let Some(target) = target.filter(|target| Some(*target) != current_channel_id)
            && self
                .state
                .discord
                .dig_send_public(target, response.clone())
                .await
                .is_ok()
        {
            self.schedule_route_view_timeout(token, Arc::clone(responder), None);
            return Ok(());
        }
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        self.schedule_route_view_timeout(token, Arc::clone(responder), receipt);
        Ok(())
    }

    async fn render_dig_responses(
        &self,
        result: DigRuntimeResult,
        user_id: i64,
        guild_id: i64,
        display_name: String,
        avatar: Option<String>,
    ) -> Result<(InteractionResponse, Option<InteractionResponse>), String> {
        let event_prompt =
            if let Some(action_id) = result.action_id.filter(|_| result.event_id.is_some()) {
                let path = self.state.database_path.clone();
                let config = self.state.dig_config.clone();
                blocking(move || {
                    cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
                        &path, config,
                    )
                    .action_presentation(user_id, guild_id, action_id, unix_now())
                    .map_err(|error| error.to_string())
                })
                .await
                .ok()
                .flatten()
            } else {
                None
            };
        let media = self.state.media.clone();
        let view_nonce = self.state.view_nonce.clone();
        blocking(move || {
            Ok(dig_responses(
                &result,
                &display_name,
                avatar,
                &media,
                &view_nonce,
                event_prompt.as_ref(),
            ))
        })
        .await
    }

    async fn boss_encounter(
        &self,
        user_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigBossEncounterInfo, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        let entropy = self.state.boss_entropy.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .encounter(
                    DigBossRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                    },
                    entropy,
                )
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn render_boss_encounter(
        &self,
        user_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<InteractionResponse, String> {
        let info = self.boss_encounter(user_id, guild_id, now).await?;
        let media = Arc::clone(&self.state.media);
        blocking(move || Ok(boss_encounter_response(&info, user_id, guild_id, &media))).await
    }

    async fn start_boss(
        &self,
        user_id: i64,
        guild_id: i64,
        risk_tier: RiskTier,
        wager: i64,
        now: i64,
    ) -> Result<DigBossCallResult<DigBossStartOutcome>, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        let entropy = self.state.boss_entropy.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .start(
                    DigBossRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                    },
                    risk_tier,
                    wager,
                    entropy,
                )
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn reconcile_resolved_boss(&self, user_id: i64, guild_id: i64, now: i64) {
        self.reconcile_dig_reminder(user_id, guild_id, now).await;
    }

    async fn reconcile_dig_reminder(&self, user_id: i64, guild_id: i64, now: i64) {
        if let Some(hooks) = &self.state.reminder_hooks
            && let Err(error) = hooks.reconcile_dig(user_id, guild_id, Some(now)).await
        {
            warn!(
                %error,
                user_id,
                guild_id,
                "dig reminder scheduling failed"
            );
        }
    }

    async fn resume_boss(
        &self,
        user_id: i64,
        guild_id: i64,
        option_index: usize,
        now: i64,
    ) -> Result<DigBossCallResult<DigBossResolvedOutcome>, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        let entropy = self.state.boss_entropy.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .resume(
                    DigBossRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                    },
                    option_index,
                    entropy,
                )
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn scout_boss(
        &self,
        user_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigBossCallResult<DigBossScoutOutcome>, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        let entropy = self.state.boss_entropy.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .scout(
                    DigBossRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                    },
                    entropy,
                )
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn retreat_boss(
        &self,
        user_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigBossCallResult<DigBossRetreatOutcome>, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        let entropy = self.state.boss_entropy.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .retreat(
                    DigBossRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                    },
                    entropy,
                )
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn cheer_boss(
        &self,
        cheerer_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<cama_app::dig_boss_runtime::DigBossCheerOutcome, String> {
        let path = self.state.database_path.clone();
        let decay = self.state.pet_hunger_decay_per_day;
        let vanity_tax = self.state.vanity_tax.clone();
        blocking(move || {
            configured_boss_runtime(path, decay, vanity_tax)
                .cheer(cheerer_id, target_id, guild_id, now)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn load_gear_panel(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<cama_app::dig_gear_runtime::DigGearRuntimePanel>, String> {
        let path = self.state.database_path.clone();
        blocking(move || {
            cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(path)
                .panel(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn execute_gear_action(
        &self,
        user_id: i64,
        guild_id: i64,
        action: cama_app::dig_gear_runtime::DigGearRuntimeAction,
    ) -> Result<cama_app::dig_gear_runtime::DigGearRuntimeOutcome, String> {
        let path = self.state.database_path.clone();
        blocking(move || {
            cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(path)
                .execute(user_id, guild_id, action, unix_now())
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn execute_relic_recycle(
        &self,
        user_id: i64,
        guild_id: i64,
        selected_row_ids: Vec<i64>,
    ) -> Result<cama_app::dig_relic_recycling::RecycleRelicsOutcome, String> {
        let path = self.state.database_path.clone();
        blocking(move || {
            let service = cama_app::dig_relic_recycling::DigRelicRecyclingService::new(
                cama_db::dig_relic_recycling::DigRelicRecyclingRepository::new(path),
            );
            let mut entropy = RuntimeRelicEntropy;
            Ok(service.recycle_relics(
                user_id,
                guild_id,
                &selected_row_ids,
                unix_now(),
                &mut entropy,
            ))
        })
        .await
    }

    async fn handle_gear_component(
        &self,
        user_id: i64,
        guild_id: i64,
        raw_action: &str,
        values: &[String],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let parts = raw_action.split(':').collect::<Vec<_>>();
        let nonce = parts.first().copied().unwrap_or_default();
        let owner = parts.get(1).and_then(|value| value.parse::<i64>().ok());
        let expected_guild = parts.get(2).and_then(|value| value.parse::<i64>().ok());
        if nonce != self.state.view_nonce {
            return respond(
                &responder,
                InteractionResponse::message(
                    "This gear panel expired after a restart. Reopen `/dig gear`.",
                )
                .ephemeral(),
            )
            .await;
        }
        if owner != Some(user_id) || expected_guild != Some(guild_id) {
            return respond(
                &responder,
                InteractionResponse::message("This isn't your gear panel.").ephemeral(),
            )
            .await;
        }
        let verb = parts.get(3).copied().unwrap_or_default();
        if verb == "back" {
            let panel = self
                .load_gear_panel(user_id, guild_id)
                .await?
                .ok_or_else(|| "Dig tunnel disappeared".to_owned())?;
            return responder
                .update(gear_panel_response(
                    &panel,
                    user_id,
                    guild_id,
                    &self.state.view_nonce,
                ))
                .await
                .map_err(|error| error.to_string());
        }
        if verb == "open" || verb == "page" {
            let mode = parts.get(4).copied().unwrap_or_default();
            let page = parts
                .get(5)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            if mode == "recycle" {
                let panel = self
                    .load_gear_panel(user_id, guild_id)
                    .await?
                    .ok_or_else(|| "Dig tunnel disappeared".to_owned())?;
                let Some(response) =
                    relic_recycle_response(&panel, user_id, guild_id, &self.state.view_nonce)
                else {
                    return respond(
                        &responder,
                        InteractionResponse::message(
                            "You need three unequipped ordinary relics of the same rarity.",
                        )
                        .ephemeral(),
                    )
                    .await;
                };
                return responder
                    .update(response)
                    .await
                    .map_err(|error| error.to_string());
            }
            let panel = self
                .load_gear_panel(user_id, guild_id)
                .await?
                .ok_or_else(|| "Dig tunnel disappeared".to_owned())?;
            let Some(response) = gear_select_response(
                &panel,
                user_id,
                guild_id,
                &self.state.view_nonce,
                mode,
                page,
            ) else {
                let message = match mode {
                    "equip" => "Nothing to equip.",
                    "unequip" => "Nothing equipped to unequip.",
                    "repair" => "Nothing damaged to repair.",
                    _ => "Unknown gear action.",
                };
                return respond(
                    &responder,
                    InteractionResponse::message(message).ephemeral(),
                )
                .await;
            };
            return responder
                .update(response)
                .await
                .map_err(|error| error.to_string());
        }
        if verb == "recycle" {
            let selected_row_ids = values
                .iter()
                .map(|value| value.parse::<i64>())
                .collect::<Result<Vec<_>, _>>()
                .ok();
            let Some(selected_row_ids) = selected_row_ids.filter(|rows| rows.len() == 3) else {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "Select exactly three different relics to recycle.",
                    )
                    .ephemeral(),
                )
                .await;
            };
            let outcome = self
                .execute_relic_recycle(user_id, guild_id, selected_row_ids)
                .await?;
            let panel = self
                .load_gear_panel(user_id, guild_id)
                .await?
                .ok_or_else(|| "Dig tunnel disappeared".to_owned())?;
            responder
                .update(gear_panel_response(
                    &panel,
                    user_id,
                    guild_id,
                    &self.state.view_nonce,
                ))
                .await
                .map_err(|error| error.to_string())?;
            return responder
                .followup(relic_recycle_followup(&outcome))
                .await
                .map_err(|error| error.to_string());
        }
        let action = if verb == "repair_all" {
            cama_app::dig_gear_runtime::DigGearRuntimeAction::RepairAll
        } else if verb == "select" {
            let mode = parts.get(4).copied().unwrap_or_default();
            let Some(value) = values.first() else {
                return respond(
                    &responder,
                    InteractionResponse::message("Invalid gear selection.").ephemeral(),
                )
                .await;
            };
            let mut selected = value.split(':');
            let kind = selected.next().unwrap_or_default();
            let id = selected.next().and_then(|value| value.parse::<i64>().ok());
            if id.is_none() || selected.next().is_some() {
                return respond(
                    &responder,
                    InteractionResponse::message("Invalid gear selection.").ephemeral(),
                )
                .await;
            }
            let id = id.expect("validated gear selection id");
            match (mode, kind) {
                ("equip", "gear") => {
                    cama_app::dig_gear_runtime::DigGearRuntimeAction::EquipGear { gear_id: id }
                }
                ("unequip", "gear") => {
                    cama_app::dig_gear_runtime::DigGearRuntimeAction::UnequipGear { gear_id: id }
                }
                ("repair", "gear") => {
                    cama_app::dig_gear_runtime::DigGearRuntimeAction::RepairGear { gear_id: id }
                }
                ("equip", "relic") => {
                    cama_app::dig_gear_runtime::DigGearRuntimeAction::EquipRelic {
                        artifact_row_id: id,
                    }
                }
                ("unequip", "relic") => {
                    cama_app::dig_gear_runtime::DigGearRuntimeAction::UnequipRelic {
                        artifact_row_id: id,
                    }
                }
                ("repair", "relic") => {
                    return respond(
                        &responder,
                        InteractionResponse::message("Relics can't be repaired.").ephemeral(),
                    )
                    .await;
                }
                _ => {
                    return respond(
                        &responder,
                        InteractionResponse::message("Unknown gear selection.").ephemeral(),
                    )
                    .await;
                }
            }
        } else {
            return respond(
                &responder,
                InteractionResponse::message("This gear interaction expired.").ephemeral(),
            )
            .await;
        };
        let outcome = self.execute_gear_action(user_id, guild_id, action).await?;
        let panel = if let Some(panel) = outcome.panel.clone() {
            panel
        } else {
            self.load_gear_panel(user_id, guild_id)
                .await?
                .ok_or_else(|| "Dig tunnel disappeared".to_owned())?
        };
        responder
            .update(gear_panel_response(
                &panel,
                user_id,
                guild_id,
                &self.state.view_nonce,
            ))
            .await
            .map_err(|error| error.to_string())?;
        responder
            .followup(gear_action_followup(&outcome))
            .await
            .map_err(|error| error.to_string())
    }

    async fn public_channel(
        &self,
        guild_id: i64,
        current: Option<i64>,
    ) -> Result<Option<i64>, String> {
        if let Some(configured) = self.state.configured_channel_id
            && self
                .state
                .discord
                .dig_channel(configured)
                .await?
                .is_some_and(|channel| channel.guild_id == Some(guild_id) && channel.can_send)
        {
            return Ok(Some(configured));
        }
        Ok(current)
    }

    async fn prestige_neon_response(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<InteractionResponse>, String> {
        let user_id =
            u64::try_from(user_id).map_err(|_| "Dig prestige user id is negative".to_owned())?;
        let guild_id =
            u64::try_from(guild_id).map_err(|_| "Dig prestige guild id is negative".to_owned())?;
        let state = Arc::clone(&self.state);
        blocking(move || {
            let result = state
                .neon
                .lock()
                .map_err(|_| "Dig Neon lock poisoned".to_owned())?
                .on_dig_prestige(user_id, Some(guild_id));
            Ok(result.map(dig_neon_response))
        })
        .await
    }

    async fn run_dig(
        &self,
        user_id: i64,
        guild_id: i64,
        now: i64,
        forced_event: bool,
        paid: bool,
        delivery: DigRuntimeDeliveryContext,
    ) -> Result<DigRuntimeExecution, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        let vanity_tax = self.state.vanity_tax.clone();
        blocking(move || {
            let pet_path = path.clone();
            let service = configured_dig_runtime(path, config, vanity_tax);
            let outcome = service
                .dig_with_delivery(
                    cama_app::dig_runtime::DigRuntimeRequest {
                        discord_id: user_id,
                        guild_id,
                        now,
                        paid,
                        forced_event,
                    },
                    delivery,
                )
                .map_err(|error| error.to_string())?;
            if outcome.success
                && let Some(action_id) = outcome.action_id
            {
                let source_key = dig_pet_activity_source_key(action_id);
                // Python records DIG_COMPLETED after the committed Dig. The
                // repository owns eligibility, daily caps, and duplicate
                // source-key suppression, so a retry after a provider crash
                // cannot award the activity twice.
                let _ = PetEvolutionRepository::new(pet_path).record_activity(
                    user_id,
                    Some(guild_id),
                    PetActivity::DigCompleted,
                    &source_key,
                    now,
                );
            }
            Ok(outcome)
        })
        .await
    }

    async fn record_dig_pet_activity(
        &self,
        user_id: i64,
        guild_id: i64,
        action_id: i64,
        occurred_at: i64,
    ) {
        let path = self.state.database_path.clone();
        let source_key = dig_pet_activity_source_key(action_id);
        if let Err(error) = blocking(move || {
            PetEvolutionRepository::new(path)
                .record_activity(
                    user_id,
                    Some(guild_id),
                    PetActivity::DigCompleted,
                    &source_key,
                    occurred_at,
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
        {
            warn!(action_id, %error, "Dig pet activity recovery failed");
        }
    }

    /// Run the Python post-result side effects after the Dig transaction has
    /// committed. These effects are intentionally outside the canonical
    /// delivery result: a Discord or Neon failure must never turn a settled
    /// Dig into an interaction failure.
    async fn post_result_hooks(
        &self,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
    ) {
        if !outcome.success
            || outcome.first_dig
            || outcome.paid_dig_available
            || outcome.action_id.is_none()
        {
            return;
        }
        let action_id = outcome.action_id.expect("checked action id");

        if let Some(block_loss) = catastrophic_cave_in_block_loss(outcome) {
            self.post_catastrophic_flame(action_id, user_id, guild_id, outcome, channel_id)
                .await;
            self.post_dig_neon(
                action_id,
                user_id,
                guild_id,
                outcome,
                channel_id,
                DigNeonPost::Cave { block_loss },
            )
            .await;
        } else if let Some(artifact_id) = outcome.artifact_id.as_deref()
            && let Some((name, rarity)) = dig_artifact_neon_info(artifact_id)
        {
            self.post_dig_neon(
                action_id,
                user_id,
                guild_id,
                outcome,
                channel_id,
                DigNeonPost::Relic { name, rarity },
            )
            .await;
        }
    }

    async fn post_catastrophic_flame(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
        channel_id: i64,
    ) {
        let event_type = format!("dig:{action_id}:catastrophic_flame");
        let path = self.state.database_path.clone();
        let depth_after = outcome.depth_after;
        let claimed = blocking(move || {
            NeonEventRepository::new(path)
                .claim_one_time_event(user_id, guild_id, &event_type, depth_after, unix_now())
                .map_err(|error| error.to_string())
        })
        .await;
        if !matches!(claimed, Ok(true)) {
            if let Err(error) = claimed {
                warn!(action_id, %error, "Dig catastrophic flame claim failed");
            }
            return;
        }
        let response =
            InteractionResponse::message(format!("💥 *{}*", catastrophic_flame_line(action_id)))
                .without_mentions();
        if let Err(error) = self
            .state
            .discord
            .dig_send_public(channel_id, response)
            .await
        {
            warn!(action_id, %error, "Dig catastrophic flame delivery failed");
            // The transport API cannot distinguish a definite rejection from
            // an accepted message followed by a lost response. Keep the
            // action marker terminal rather than risking a duplicate flame
            // on READY/retry.
        }
    }

    async fn post_dig_neon(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
        channel_id: i64,
        post: DigNeonPost,
    ) {
        let Ok(discord_id) = u64::try_from(user_id) else {
            return;
        };
        let Ok(guild_id_u64) = u64::try_from(guild_id) else {
            return;
        };
        let event_type = format!("dig:{action_id}:neon");
        let path = self.state.database_path.clone();
        let depth_after = outcome.depth_after;
        let claimed = blocking(move || {
            NeonEventRepository::new(path)
                .claim_one_time_event(user_id, guild_id, &event_type, depth_after, unix_now())
                .map_err(|error| error.to_string())
        })
        .await;
        if !matches!(claimed, Ok(true)) {
            if let Err(error) = claimed {
                warn!(action_id, %error, "Dig Neon claim failed");
            }
            return;
        }

        let layer_name = layer_at(depth_after).name;
        let neon_result = {
            let state = Arc::clone(&self.state);
            blocking(move || {
                let mut neon = state
                    .neon
                    .lock()
                    .map_err(|_| "Dig Neon lock poisoned".to_owned())?;
                let result = match post {
                    DigNeonPost::Cave { block_loss } => neon.on_dig_cave_in(
                        discord_id,
                        Some(guild_id_u64),
                        depth_after.saturating_add(block_loss),
                        depth_after,
                        layer_name,
                    ),
                    DigNeonPost::Relic { name, rarity } => neon.on_dig_relic_found(
                        discord_id,
                        Some(guild_id_u64),
                        RelicFound {
                            relic_name: &name,
                            rarity: &rarity,
                            layer_name,
                        },
                    ),
                };
                Ok(result)
            })
            .await
        };
        let neon_result = match neon_result {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                warn!(action_id, %error, "Dig Neon hook failed");
                return;
            }
        };
        if let Err(error) = self
            .state
            .discord
            .dig_send_temporary(
                channel_id,
                dig_neon_response(neon_result),
                Duration::from_secs(60),
            )
            .await
        {
            warn!(action_id, %error, "Dig Neon delivery failed");
            // `String` errors may arrive after Discord accepted the message;
            // retain the claim to make retries at-most-once.
        }
    }

    async fn post_boss_neon(
        &self,
        action_id: Option<i64>,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        victory: Option<DigBossNeonVictory>,
    ) {
        let Some(action_id) = action_id else {
            return;
        };
        let Some(channel_id) = channel_id else {
            return;
        };
        let Some(victory) = victory else {
            return;
        };
        let Ok(discord_id) = u64::try_from(user_id) else {
            return;
        };
        let Ok(guild_id_u64) = u64::try_from(guild_id) else {
            return;
        };
        let event_type = format!("dig:{action_id}:boss_neon");
        let path = self.state.database_path.clone();
        let boundary = victory.boundary;
        let claimed = blocking(move || {
            NeonEventRepository::new(path)
                .claim_one_time_event(user_id, guild_id, &event_type, boundary, unix_now())
                .map_err(|error| error.to_string())
        })
        .await;
        if !matches!(claimed, Ok(true)) {
            if let Err(error) = claimed {
                warn!(action_id, %error, "Dig boss Neon claim failed");
            }
            return;
        }

        let state = Arc::clone(&self.state);
        let neon_result = blocking(move || {
            let mut neon = state
                .neon
                .lock()
                .map_err(|_| "Dig Neon lock poisoned".to_owned())?;
            let result = neon.on_dig_boss_victory(
                discord_id,
                Some(guild_id_u64),
                BossVictory {
                    boss_name: &victory.boss_name,
                    boundary: victory.boundary,
                    layer_name: &victory.layer_name,
                    jc_delta: victory.jc_delta,
                    gear_drop: victory.gear_drop,
                    trophy_relic_drop: victory.trophy_relic_drop,
                },
            );
            Ok(result)
        })
        .await;
        let neon_result = match neon_result {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => {
                warn!(action_id, %error, "Dig boss Neon hook failed");
                return;
            }
        };
        if let Err(error) = self
            .state
            .discord
            .dig_send_temporary(
                channel_id,
                dig_neon_response(neon_result),
                Duration::from_secs(60),
            )
            .await
        {
            warn!(action_id, %error, "Dig boss Neon delivery failed");
        }
    }

    async fn add_result_reactions(
        &self,
        receipt: &InteractionMessageReceipt,
        delivery: Option<&DigRuntimeDeliverySnapshot>,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
    ) {
        let Ok(channel_id) = i64::try_from(receipt.channel_id) else {
            warn!("Dig reaction channel id exceeds SQLite INTEGER");
            return;
        };
        self.add_result_reactions_to_message(channel_id, receipt.message_id, delivery, outcome)
            .await;
    }

    async fn add_result_reactions_to_message(
        &self,
        channel_id: i64,
        message_id: u64,
        delivery: Option<&DigRuntimeDeliverySnapshot>,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
    ) {
        for emoji in dig_result_reactions(outcome, delivery) {
            if let Err(error) = self
                .state
                .discord
                .dig_add_reaction(channel_id, message_id, emoji)
                .await
            {
                warn!(message_id, emoji, %error, "Dig result reaction failed");
            }
        }
    }

    async fn maybe_send_dig_bonus(
        &self,
        outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) {
        if !outcome.success || outcome.first_dig {
            return;
        }
        let Some(action_id) = outcome.action_id else {
            return;
        };
        let dispatcher = self
            .state
            .bonus_dispatcher
            .lock()
            .ok()
            .and_then(|dispatcher| dispatcher.clone());
        let Some(dispatcher) = dispatcher else {
            return;
        };
        let Some(bonus) =
            cama_app::dig_bonus_events::pick_dig_bonus(deterministic_dig_bonus_roll(action_id))
        else {
            return;
        };
        let event_type = format!("dig:{action_id}:bonus:{}", bonus.as_str());
        let path = self.state.database_path.clone();
        let depth_after = outcome.depth_after;
        let claimed = blocking(move || {
            NeonEventRepository::new(path)
                .claim_one_time_event(user_id, guild_id, &event_type, depth_after, unix_now())
                .map_err(|error| error.to_string())
        })
        .await;
        if !matches!(claimed, Ok(true)) {
            if let Err(error) = claimed {
                warn!(action_id, %error, "Dig bonus claim failed");
            }
            return;
        }

        if let Err(error) = dispatcher
            .dispatch_bonus(
                action_id,
                user_id,
                guild_id,
                channel_id,
                bonus,
                Arc::clone(&responder),
            )
            .await
        {
            warn!(action_id, bonus = bonus.as_str(), %error, "Dig bonus dispatch failed");
            // Claim before dispatch is terminal.  The adapter may have
            // settled a wheel/package/trivia session before its presentation
            // failed; releasing this marker would permit a retry to settle
            // the same bonus twice.  The partial-failure notice is the
            // recoverable user-facing path, while the durable claim keeps the
            // economy at-most-once across provider restarts.
            if let Err(report_error) = dispatcher.report_partial_failure(responder).await {
                warn!(action_id, %report_error, "Dig bonus failure report failed");
            }
        }
    }

    async fn event_delivery_for_action(
        &self,
        action_id: i64,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigEventDeliverySnapshot>, String> {
        Ok(self
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(guild_id),
                discord_id: Some(discord_id),
                limit: 100,
            })
            .await?
            .into_iter()
            .find(|delivery| delivery.action_id == action_id))
    }

    async fn pending_event_deliveries(
        &self,
        query: DigEventPendingDeliveryQuery,
    ) -> Result<Vec<DigEventDeliverySnapshot>, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        blocking(move || {
            cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(&path, config)
                .pending_event_deliveries(query)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn pending_event_delivery_recoveries(
        &self,
        query: DigEventPendingDeliveryQuery,
    ) -> Result<Vec<DigEventDeliverySnapshot>, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        blocking(move || {
            cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(&path, config)
                .pending_event_delivery_recoveries(query)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn recover_event_delivery(&self, action_id: i64) -> Result<bool, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        blocking(move || {
            cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(&path, config)
                .recover_event_delivery(action_id)
                .map(|outcome| outcome.is_some())
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn mark_event_delivery(
        &self,
        delivery: &DigEventDeliverySnapshot,
    ) -> Result<bool, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        let request = DigEventDeliveryMarkRequest {
            action_id: delivery.action_id,
            discord_id: delivery.discord_id,
            guild_id: delivery.guild_id,
            source_key: delivery.source_key.clone(),
            delivered_at: unix_now(),
        };
        blocking(move || {
            cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(&path, config)
                .mark_event_delivery_delivered(request)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn reconcile_event_delivery(
        &self,
        delivery: &DigEventDeliverySnapshot,
        expected: &InteractionResponse,
    ) -> Result<bool, String> {
        let nonce = delivery.nonce();
        let history = self
            .state
            .discord
            .dig_public_history(
                delivery.context.channel_id,
                delivery
                    .committed_at
                    .saturating_sub(DELIVERY_RECEIPT_GRACE_SECONDS),
                DELIVERY_RECEIPT_SCAN_LIMIT,
            )
            .await?;
        let found = history
            .messages
            .iter()
            .take(DELIVERY_RECEIPT_SCAN_LIMIT)
            .any(|message| {
                message.author_id == history.bot_user_id
                    && (message.nonce.as_deref() == Some(nonce.as_str())
                        || event_history_matches(message, delivery, expected))
            });
        if found {
            self.mark_event_delivery(delivery).await?;
        }
        Ok(found)
    }

    async fn deliver_event_from_interaction(
        &self,
        delivery: &DigEventDeliverySnapshot,
        response: InteractionResponse,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        match responder.followup_with_receipt(response.clone()).await {
            Ok(Some(receipt))
                if i64::try_from(receipt.channel_id).ok() == Some(delivery.context.channel_id) =>
            {
                self.mark_event_delivery(delivery).await.map(|_| ())
            }
            Ok(Some(_)) | Ok(None) => {
                if self.reconcile_event_delivery(delivery, &response).await? {
                    Ok(())
                } else {
                    Err("Dig event follow-up was accepted without a bound receipt".to_owned())
                }
            }
            Err(send_error) => match self.reconcile_event_delivery(delivery, &response).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(send_error.to_string()),
                Err(history_error) => Err(format!(
                    "{}; event delivery history reconciliation failed: {history_error}",
                    send_error
                )),
            },
        }
    }

    async fn deliver_event_to_channel_with_failure(
        &self,
        delivery: &DigEventDeliverySnapshot,
        response: InteractionResponse,
    ) -> Result<(), DigEventDeliveryFailure> {
        match self.reconcile_event_delivery(delivery, &response).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(DigEventDeliveryFailure::Ambiguous(error)),
        }
        match self
            .state
            .discord
            .dig_send_public_once(
                delivery.context.channel_id,
                response.clone(),
                &delivery.nonce(),
            )
            .await
        {
            Ok(receipt)
                if i64::try_from(receipt.channel_id).ok() == Some(delivery.context.channel_id) =>
            {
                self.mark_event_delivery(delivery)
                    .await
                    .map(|_| ())
                    .map_err(DigEventDeliveryFailure::Ambiguous)
            }
            Ok(_) => Err(DigEventDeliveryFailure::Ambiguous(
                "Dig event delivery receipt belongs to a different channel".to_owned(),
            )),
            Err(send_error) => match self.reconcile_event_delivery(delivery, &response).await {
                Ok(true) => Ok(()),
                Ok(false) if send_error.kind == DigPublicSendFailureKind::Rejected => {
                    Err(DigEventDeliveryFailure::Rejected(send_error.message))
                }
                Ok(false) => Err(DigEventDeliveryFailure::Ambiguous(send_error.message)),
                Err(history_error) => Err(DigEventDeliveryFailure::Ambiguous(format!(
                    "{}; event delivery history reconciliation failed: {history_error}",
                    send_error.message
                ))),
            },
        }
    }

    async fn deliver_event_to_channel(
        &self,
        delivery: &DigEventDeliverySnapshot,
    ) -> Result<(), String> {
        self.deliver_event_to_channel_with_failure(
            delivery,
            event_resolution_response(&delivery.outcome),
        )
        .await
        .map_err(|failure| match failure {
            DigEventDeliveryFailure::Rejected(error)
            | DigEventDeliveryFailure::Ambiguous(error) => error,
        })
    }

    async fn pending_deliveries(
        &self,
        query: DigRuntimePendingDeliveryQuery,
    ) -> Result<Vec<DigRuntimeDeliverySnapshot>, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite_with_config(&path, config)
                .pending_deliveries(query)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn mark_delivery_part(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
        part: DigRuntimeDeliveryPart,
        delivered_at: i64,
    ) -> Result<bool, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        let request = DigRuntimeMarkDelivered {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            delivered_at,
            part,
        };
        blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite_with_config(&path, config)
                .mark_delivery_delivered(request)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn rebind_delivery_channel(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
        part: DigRuntimeDeliveryPart,
        expected_channel_id: i64,
        fallback_channel_id: i64,
    ) -> Result<DigRuntimeDeliverySnapshot, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        let request = DigRuntimeRebindDeliveryChannel {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            part,
            expected_channel_id,
            fallback_channel_id,
        };
        blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite_with_config(&path, config)
                .rebind_pending_delivery_channel(request)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn finalize_delivery_snapshot(
        &self,
        request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite_with_config(&path, config)
                .finalize_delivery(request)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn settle_blood_pact_delivery(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<DigRuntimeDeliverySnapshot, String> {
        let path = self.state.database_path.clone();
        let config = self.state.dig_config.clone();
        let vanity_tax = self.state.vanity_tax.clone();
        let request = DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            occurred_at: delivery.committed_at,
        };
        blocking(move || {
            configured_dig_runtime(path, config, vanity_tax)
                .settle_blood_pact_delivery(request)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn prepare_delivery(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<DigRuntimeDeliverySnapshot, String> {
        // Blood Pact is a post-commit economy effect and must become terminal
        // before flavor freezes user-visible copy. Its stable repository key
        // makes this safe across a crash between the effect and this outbox
        // projection update.
        let delivery = self.settle_blood_pact_delivery(delivery).await?;
        if !delivery.blood_pact.is_terminal() {
            return Err("Dig Blood Pact phase remains pending; delivery is blocked".to_owned());
        }
        if delivery.flavor.is_terminal() {
            return Ok(delivery);
        }
        let boss_info = if delivery.render.kind == DigRuntimeRenderKind::Boss {
            Some(
                self.boss_encounter(delivery.discord_id, delivery.guild_id, unix_now())
                    .await?,
            )
        } else {
            None
        };
        let flavor = Arc::clone(&self.state.flavor);
        let flavor_data = Arc::clone(&self.state.flavor_data);
        let mut flavor_outcome = dig_delivery_flavor_outcome(&delivery, boss_info.as_ref());
        let discord_id = delivery.discord_id;
        let guild_id = delivery.guild_id;
        let action_id = delivery.action_id;
        let receipt = blocking(move || {
            flavor.flavor(&mut flavor_outcome, discord_id, guild_id, false);
            flavor_data
                .get_flavor_receipt(action_id, discord_id, guild_id)
                .map_err(|error| format!("Dig flavor receipt recovery failed: {error}"))
        })
        .await;
        let flavor = match receipt {
            Ok(Some(receipt)) => dig_runtime_flavor_snapshot(receipt),
            Ok(None) => {
                warn!(
                    action_id,
                    "Dig flavor produced no durable receipt; finalizing without flavor"
                );
                DigRuntimeFlavorSnapshot::Skipped
            }
            Err(error) => {
                warn!(
                    action_id,
                    %error,
                    "Dig flavor receipt recovery failed; finalizing without flavor"
                );
                DigRuntimeFlavorSnapshot::Skipped
            }
        };
        self.finalize_delivery_snapshot(DigRuntimeFinalizeDelivery {
            action_id,
            source_key: delivery.source_key.clone(),
            flavor,
            boss: boss_info.map(|boss| cama_app::dig_runtime::DigRuntimeBossRenderSnapshot {
                boundary: i64::from(boss.boundary),
                boss_id: boss.boss_id,
                boss_name: boss.boss_name,
                dialogue: boss.dialogue,
                is_pinnacle: boss.is_pinnacle,
                phase: i64::from(boss.phase),
                wager_allowed: boss.wager_allowed,
                carried_wager: boss.carried_wager.unwrap_or_default(),
                has_scout_lantern: boss.has_scout_lantern,
                luminosity: boss.luminosity,
                encounter_key: Some(boss.encounter_key),
            }),
        })
        .await
    }

    async fn reconcile_delivery_part(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
        part: DigRuntimeDeliveryPart,
        expected: &InteractionResponse,
    ) -> Result<bool, String> {
        let nonce = dig_delivery_nonce(delivery, part);
        let history = self
            .state
            .discord
            .dig_public_history(
                delivery.context.channel_id,
                delivery
                    .committed_at
                    .saturating_sub(DELIVERY_RECEIPT_GRACE_SECONDS),
                DELIVERY_RECEIPT_SCAN_LIMIT,
            )
            .await?;
        let found_message_id = history
            .messages
            .iter()
            .take(DELIVERY_RECEIPT_SCAN_LIMIT)
            .find(|message| {
                message.author_id == history.bot_user_id
                    && (message.nonce.as_deref() == Some(nonce.as_str())
                        || interaction_history_matches(message, delivery, expected))
            })
            .map(|message| message.message_id);
        if let Some(message_id) = found_message_id {
            if part == DigRuntimeDeliveryPart::Main {
                self.add_result_reactions_to_message(
                    delivery.context.channel_id,
                    message_id,
                    Some(delivery),
                    &delivery.outcome,
                )
                .await;
            }
            self.mark_delivery_part(delivery, part, unix_now()).await?;
        }
        Ok(found_message_id.is_some())
    }

    async fn deliver_part_once_with_failure(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
        part: DigRuntimeDeliveryPart,
        response: InteractionResponse,
    ) -> Result<(), DigDeliveryFailure> {
        // A process can die after Discord accepts the message but before the
        // SQLite CAS below. Always reconcile first; if history is unavailable,
        // fail closed rather than publish a possible duplicate.
        match self
            .reconcile_delivery_part(delivery, part, &response)
            .await
        {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(DigDeliveryFailure::Ambiguous(error)),
        }
        let nonce = dig_delivery_nonce(delivery, part);
        match self
            .state
            .discord
            .dig_send_public_once(delivery.context.channel_id, response.clone(), &nonce)
            .await
        {
            Ok(receipt) => {
                if part == DigRuntimeDeliveryPart::Main {
                    self.add_result_reactions(&receipt, Some(delivery), &delivery.outcome)
                        .await;
                }
                self.mark_delivery_part(delivery, part, unix_now())
                    .await
                    .map(|_| ())
                    .map_err(DigDeliveryFailure::Ambiguous)
            }
            Err(send_error) => {
                // HTTP cancellation and timeouts can be ambiguous. Re-read a
                // bounded history before deciding the send failed.
                match self
                    .reconcile_delivery_part(delivery, part, &response)
                    .await
                {
                    Ok(true) => Ok(()),
                    Ok(false) if send_error.kind == DigPublicSendFailureKind::Rejected => {
                        Err(DigDeliveryFailure::SafeFallback {
                            part,
                            error: send_error.message,
                        })
                    }
                    Ok(false) => Err(DigDeliveryFailure::Ambiguous(send_error.message)),
                    Err(history_error) => Err(DigDeliveryFailure::Ambiguous(format!(
                        "{}; delivery history reconciliation failed: {history_error}",
                        send_error.message
                    ))),
                }
            }
        }
    }

    async fn deliver_to_channel_with_failure(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<(), DigDeliveryFailure> {
        // READY recovery may be the first process to observe a committed
        // Dig if the original worker stopped after the aggregate commit. The
        // activity key and post-result claims are action-scoped, so replaying
        // them here repairs that crash window without duplicating rewards or
        // atmospheric messages.
        self.record_dig_pet_activity(
            delivery.discord_id,
            delivery.guild_id,
            delivery.action_id,
            delivery.committed_at,
        )
        .await;
        let delivery = self
            .prepare_delivery(delivery)
            .await
            .map_err(DigDeliveryFailure::Ambiguous)?;
        self.post_result_hooks(
            &delivery.outcome,
            delivery.discord_id,
            delivery.guild_id,
            delivery.context.channel_id,
        )
        .await;
        let (main, event) =
            dig_delivery_responses(&delivery, &self.state.media, &self.state.view_nonce);
        if delivery.main_delivered_at.is_none() {
            self.deliver_part_once_with_failure(&delivery, DigRuntimeDeliveryPart::Main, main)
                .await?;
        }
        if delivery.event_delivered_at.is_none()
            && let Some(event) = event
        {
            self.deliver_part_once_with_failure(&delivery, DigRuntimeDeliveryPart::Event, event)
                .await?;
        }
        Ok(())
    }

    async fn deliver_to_channel(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<(), String> {
        self.deliver_to_channel_with_failure(delivery)
            .await
            .map_err(|failure| match failure {
                DigDeliveryFailure::SafeFallback { error, .. }
                | DigDeliveryFailure::Ambiguous(error) => error,
            })
    }

    async fn command_help(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some((target_id, target_name)) = user_option(options, "user")? else {
            return Err("/dig help requires user".to_owned());
        };
        if target_id == user_id {
            const SELF_HELP_LINES: [&str; 6] = [
                "You tried to help yourself. The pickaxe is confused.",
                "That's not how teamwork works, chief.",
                "Mining solo is fine, but helping yourself is just sad.",
                "Your tunnel filed a restraining order against your own help.",
                "You can't pat your own back with a pickaxe. Well, you can, but you shouldn't.",
                "Self-help books are in aisle 3. This is a mine.",
            ];
            return responder
                .followup(
                    InteractionResponse::message(
                        SELF_HELP_LINES[fastrand::usize(..SELF_HELP_LINES.len())],
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        }
        let path = self.state.database_path.clone();
        let now = unix_now();
        let result = blocking(move || {
            Ok(DigSocialRuntimeService::sqlite(&path).help(user_id, target_id, guild_id, now))
        })
        .await?;
        let response = match result {
            Ok(result) => InteractionResponse::message("").embed(
                InteractionEmbed::titled("Tunnel Assistance")
                    .description(format!(
                        "You helped **{}**'s tunnel!\nBlocks added: **{}**",
                        target_name.unwrap_or_else(|| format!("User {target_id}")),
                        result.advance
                    ))
                    .color(0x2E_CC_71),
            ),
            Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_sabotage(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some((target_id, target_name)) = user_option(options, "user")? else {
            return Err("/dig sabotage requires user".to_owned());
        };
        if target_id == user_id {
            return respond(
                &responder,
                InteractionResponse::message("You can't sabotage yourself.").ephemeral(),
            )
            .await;
        }
        let path = self.state.database_path.clone();
        let preview = blocking(move || {
            Ok(DigSocialRuntimeService::sqlite(&path)
                .sabotage_preview(user_id, target_id, guild_id))
        })
        .await?;
        let preview = match preview {
            Ok(preview) => preview,
            Err(error) => {
                return respond(
                    &responder,
                    InteractionResponse::message(error.to_string()).ephemeral(),
                )
                .await;
            }
        };
        let target_name = target_name.unwrap_or_else(|| format!("User {target_id}"));
        let token = self.create_sabotage_view(
            user_id,
            guild_id,
            target_id,
            target_name.clone(),
            unix_now(),
        )?;
        let custom = format!("dig:sabotage:confirm:{token}");
        let embed = InteractionEmbed::titled("Confirm Sabotage")
            .description(format!(
                "**Target:** {target_name}\n**Cost:** {} {JOPACOIN_EMOTE}\n**Potential damage:** {} blocks\n\nAre you sure? If they have a trap set, you could take damage instead.",
                preview.cost,
                preview.damage_range
            ))
            .color(0x2C_2F_33);
        let row = InteractionActionRow::buttons(vec![
            InteractionButton::new(custom, "Sabotage").style(InteractionButtonStyle::Danger),
            InteractionButton::new(format!("dig:sabotage:cancel:{token}"), "Cancel")
                .style(InteractionButtonStyle::Secondary),
        ]);
        respond(
            &responder,
            InteractionResponse::message("")
                .embed(embed)
                .action_row(row),
        )
        .await
    }

    async fn command_info(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let target = user_option(options, "user")?;
        let (target_id, target_name) = target
            .map(|(id, name)| (id, name.unwrap_or_else(|| format!("User {id}"))))
            .unwrap_or((user_id, display_name.to_owned()));
        let path = self.state.database_path.clone();
        let info = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .tunnel_info(target_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let Some(info) = info else {
            return respond(
                &responder,
                InteractionResponse::message(format!("{target_name} hasn't started digging yet."))
                    .ephemeral(),
            )
            .await;
        };
        let layer = layer_at(info.depth);
        let mut embed = InteractionEmbed::titled(format!("{target_name}'s Tunnel"))
            .color(layer_color(layer))
            .field(
                "Depth",
                format!(
                    "**{}** blocks — {} (P{})",
                    info.depth, layer.name, info.prestige_level
                ),
                true,
            )
            .field("Pickaxe", pickaxe_name(info.pickaxe_tier), true)
            .field("Luminosity", format!("{}%", info.luminosity), true);
        if let Some(route) = info.route_state.as_deref() {
            embed = embed.field("Route", route, false);
        }
        if info
            .last_dig_at
            .is_some_and(|last| unix_now().saturating_sub(last) < 3_600)
        {
            embed = embed.field("Cooldown", "Free dig is resting.", true);
        }
        if info.hard_hat_charges > 0 {
            embed = embed.field(
                "Protection",
                format!("Hard Hat ({})", info.hard_hat_charges),
                true,
            );
        }
        if let Ok(Some(avatar)) = self
            .state
            .discord
            .dig_user_avatar_url(guild_id, target_id)
            .await
        {
            embed = embed.thumbnail(avatar);
        }
        respond(&responder, InteractionResponse::message("").embed(embed)).await
    }

    async fn command_leaderboard(
        &self,
        _user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.state.database_path.clone();
        let rows = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .leaderboard(guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if rows.is_empty() {
            return respond(
                &responder,
                InteractionResponse::message("No tunnels yet! Use `/dig go` to start.").ephemeral(),
            )
            .await;
        }
        let max_depth = rows.iter().map(|row| row.depth).max().unwrap_or(1).max(1);
        let description = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let bar =
                    "█".repeat((20_i64.saturating_mul(row.depth) / max_depth).max(1) as usize);
                format!(
                    "`{:>2}.` **{}** — Depth {}\n`{bar}`",
                    index + 1,
                    row.name,
                    row.depth
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        respond(
            &responder,
            InteractionResponse::message("").embed(
                InteractionEmbed::titled("Tunnel Leaderboard")
                    .description(description)
                    .color(GOLD_COLOR)
                    .footer("Community Mine"),
            ),
        )
        .await
    }

    async fn command_hall_of_fame(
        &self,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.state.database_path.clone();
        let rows = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .hall_of_fame(guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if rows.is_empty() {
            return respond(
                &responder,
                InteractionResponse::message("The hall of fame is empty. Prestige to earn a spot!")
                    .ephemeral(),
            )
            .await;
        }
        let description = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let medal = match index {
                    0 => "🥇".to_owned(),
                    1 => "🥈".to_owned(),
                    2 => "🥉".to_owned(),
                    _ => format!("`#{}`", index + 1),
                };
                format!(
                    "{medal} **{}** (<@{}>) — Score: {} (P{})",
                    row.name, row.user_id, row.score, row.prestige
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        respond(
            &responder,
            InteractionResponse::message("").embed(
                InteractionEmbed::titled("🏆 Hall of Fame")
                    .description(description)
                    .color(GOLD_COLOR)
                    .footer("Best prestige run scores"),
            ),
        )
        .await
    }

    async fn command_use(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let item = string_option(options, "item")?.unwrap_or_default();
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let item_for_db = item.clone();
        let result = blocking(move || {
            DigInventoryService::new(DigInventoryRepository::new(path))
                .use_item(user_id, Some(guild_id), &item_for_db)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !result.success {
            return responder
                .followup(
                    InteractionResponse::message(
                        result
                            .error
                            .unwrap_or_else(|| "Failed to queue item.".to_owned()),
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        }
        let item_name = result.item.as_deref().unwrap_or(&item);
        let mut embed = InteractionEmbed::titled(format!("{item_name} Queued"))
            .description("Ready for your next dig.")
            .color(GOLD_COLOR);
        let media = self.state.media.clone();
        let item_for_media = item.clone();
        let item_art = blocking(move || Ok(media.item_art(&item_for_media))).await?;
        let mut response = InteractionResponse::message("").ephemeral();
        if let Some(item_art) = item_art {
            embed = embed.thumbnail(format!("attachment://{}", item_art.filename));
            response = response.attachment(interaction_attachment(item_art));
        }
        responder
            .followup(response.embed(embed))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_gift(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some((target_id, target_name)) = user_option(options, "user")? else {
            return Err("/dig gift requires user".to_owned());
        };
        let artifact = string_option(options, "artifact")?.unwrap_or_default();
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let artifact_for_db = artifact.clone();
        let now = unix_now();
        let result = blocking(move || {
            Ok(DigSocialRuntimeService::sqlite(&path).gift_relic(
                user_id,
                target_id,
                guild_id,
                &artifact_for_db,
                now,
            ))
        })
        .await?;
        let response = match result {
            Ok(result) => InteractionResponse::message(format!(
                "You gifted **{}** to **{}**!",
                result.artifact_name,
                target_name.unwrap_or_else(|| format!("User {target_id}"))
            )),
            Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_shop(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let shop = blocking(move || {
            cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(path)
                .shop(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let Some(shop) = shop else {
            return responder
                .followup(InteractionResponse::message(REGISTER_FIRST_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string());
        };
        let mut embed = shop_embed(&shop);
        let media = self.state.media.clone();
        let shop_art = blocking(move || Ok(media.compose_shop_grid())).await?;
        let mut response = InteractionResponse::message("");
        if let Some(shop_art) = shop_art {
            embed = embed.image(format!("attachment://{}", shop_art.filename));
            response = response.attachment(interaction_attachment(shop_art));
        }
        let response = response.embed(embed);
        if responder.followup(response.clone()).await.is_ok() {
            return Ok(());
        }
        responder
            .followup(response.ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_buy(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let item = string_option(options, "item")?.unwrap_or_default();
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        if let Some((slot, tier)) = parse_gear_shop_choice(&item) {
            let action = if slot == "weapon" || slot == "pickaxe" {
                cama_app::dig_gear_runtime::DigGearRuntimeAction::BuyPickaxe { tier }
            } else {
                cama_app::dig_gear_runtime::DigGearRuntimeAction::BuyGear {
                    slot: slot.to_owned(),
                    tier,
                }
            };
            let result = self.execute_gear_action(user_id, guild_id, action).await?;
            if !result.success {
                return responder
                    .followup(
                        InteractionResponse::message(
                            result
                                .error
                                .unwrap_or_else(|| "Purchase failed.".to_owned()),
                        )
                        .ephemeral(),
                    )
                    .await
                    .map_err(|error| error.to_string());
            }
            let name = gear_purchase_name(slot, tier).unwrap_or_else(|| item.clone());
            let content = if slot == "weapon" || slot == "pickaxe" {
                format!(
                    "Upgraded your pickaxe to **{}** for **{}** {JOPACOIN_EMOTE}. It is equipped.",
                    name.strip_suffix(" Pickaxe").unwrap_or(&name),
                    result.cost,
                )
            } else {
                format!(
                    "Bought **{name}** for **{}** {JOPACOIN_EMOTE}.\nEquip it via `/dig gear`.",
                    result.cost,
                )
            };
            return responder
                .followup(InteractionResponse::message(content).ephemeral())
                .await
                .map_err(|error| error.to_string());
        }

        let path = self.state.database_path.clone();
        let item_for_db = item.clone();
        let result = blocking(move || {
            DigInventoryService::new(DigInventoryRepository::new(path))
                .buy_item_at(user_id, Some(guild_id), &item_for_db, unix_now())
                .map_err(|error| error.to_string())
        })
        .await?;
        if !result.success {
            return responder
                .followup(
                    InteractionResponse::message(
                        result
                            .error
                            .unwrap_or_else(|| "Purchase failed.".to_owned()),
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        }
        let hint = if item == "streak_charm" {
            "This charm is passive and triggers automatically.".to_owned()
        } else if result.queued {
            "Queued automatically for your next dig.".to_owned()
        } else {
            format!("Use `/dig use {item}` to queue it.")
        };
        let item_name = result.item.as_deref().unwrap_or(&item);
        let balance_after = result.balance_after.unwrap_or_default();
        let mut embed = InteractionEmbed::titled(format!("Purchased: {item_name}"))
            .description(format!(
                "Cost: **{}** {JOPACOIN_EMOTE}\nBalance: **{}** {JOPACOIN_EMOTE}\n\n{hint}",
                result.cost, balance_after
            ))
            .color(GOLD_COLOR);
        let media = self.state.media.clone();
        let item_for_media = item.clone();
        let item_art = blocking(move || Ok(media.item_art(&item_for_media))).await?;
        let mut response = InteractionResponse::message("").ephemeral();
        if let Some(item_art) = item_art {
            embed = embed.thumbnail(format!("attachment://{}", item_art.filename));
            response = response.attachment(interaction_attachment(item_art));
        }
        responder
            .followup(response.embed(embed))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_flex(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let info = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .flex_data(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await;
        let Some(info) = (match info {
            Ok(info) => info,
            Err(_) => {
                return responder
                    .followup(InteractionResponse::message("Flex unavailable.").ephemeral())
                    .await
                    .map_err(|error| error.to_string());
            }
        }) else {
            return responder
                .followup(
                    InteractionResponse::message(
                        "You don't have a tunnel yet. Use `/dig go` to start!",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        };
        let avatar = self
            .state
            .discord
            .dig_user_avatar_url(guild_id, user_id)
            .await
            .ok()
            .flatten();
        responder
            .followup(flex_response(
                &info,
                display_name,
                avatar.as_deref(),
                fastrand::usize(..FLEX_ROASTS.len()),
            ))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_prestige(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.state.database_path.clone();
        let preview = blocking(move || {
            DigPrestigeRuntimeService::sqlite(&path)
                .preview(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !preview.can_prestige {
            return respond(
                &responder,
                InteractionResponse::message(
                    preview
                        .reason
                        .unwrap_or_else(|| "Cannot prestige.".to_owned()),
                )
                .ephemeral(),
            )
            .await;
        }
        let view_token =
            self.create_prestige_view(user_id, guild_id, preview.mutation.is_some(), unix_now())?;
        let response = if preview.mutation.is_some() {
            prestige_mutation_response(&preview, &view_token)
        } else {
            prestige_perk_response(&preview, &view_token, None)
        };
        respond(&responder, response).await
    }

    async fn command_abandon(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let now = unix_now();
        let path = self.state.database_path.clone();
        let preview = blocking(move || {
            DigAbandonRuntimeService::sqlite(&path)
                .preview_at(user_id, guild_id, now)
                .map_err(|error| error.to_string())
        })
        .await;
        let preview = match preview {
            Ok(preview) => preview,
            Err(error) => {
                return respond(&responder, InteractionResponse::message(error).ephemeral()).await;
            }
        };
        let token = self.create_abandon_view(user_id, guild_id, now)?;
        let embed = InteractionEmbed::titled("Abandon Tunnel?")
            .description(format!(
                "This will **permanently destroy** your tunnel.\nRefund: **{}** {JOPACOIN_EMOTE}\n\nAre you sure?",
                preview.refund
            ))
            .color(ERROR_COLOR);
        let row = InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("dig:abandon:confirm:{token}"), "Abandon Tunnel")
                .style(InteractionButtonStyle::Danger),
            InteractionButton::new(format!("dig:abandon:cancel:{token}"), "Cancel")
                .style(InteractionButtonStyle::Secondary),
        ]);
        respond(
            &responder,
            InteractionResponse::message("")
                .embed(embed)
                .action_row(row),
        )
        .await
    }

    async fn command_trap(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let game_date = cama_domain::game_date::get_game_date();
        let path = self.state.database_path.clone();
        let result = blocking(move || {
            DigInventoryService::new(DigInventoryRepository::new(path))
                .set_trap(user_id, Some(guild_id), &game_date)
                .map_err(|error| error.to_string())
        })
        .await?;
        let response = if result.success {
            let mut content = "Trap set!".to_owned();
            if result.cost > 0 {
                content.push_str(&format!(" (Cost: {} {JOPACOIN_EMOTE})", result.cost));
            }
            InteractionResponse::message(content)
        } else {
            InteractionResponse::message(
                result
                    .error
                    .unwrap_or_else(|| "Failed to set trap.".to_owned()),
            )
            .ephemeral()
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_insure(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let result = blocking(move || {
            DigInventoryService::new(DigInventoryRepository::new(path))
                .buy_insurance_at(user_id, Some(guild_id), unix_now())
                .map_err(|error| error.to_string())
        })
        .await?;
        let response = if result.success {
            InteractionResponse::message(format!(
                "Insurance purchased for **{}** {JOPACOIN_EMOTE}! Duration: 24 hours.",
                result.cost,
            ))
        } else {
            InteractionResponse::message(
                result
                    .error
                    .unwrap_or_else(|| "Failed to buy insurance.".to_owned()),
            )
            .ephemeral()
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_inventory(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.state.database_path.clone();
        let (items, shop) = blocking(move || {
            let items = DigInventoryService::new(DigInventoryRepository::new(&path))
                .get_inventory(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(&path)
                .shop(user_id, guild_id)
                .map_err(|error| error.to_string())?;
            Ok((items, shop))
        })
        .await?;
        let Some(shop) = shop else {
            return responder
                .followup(InteractionResponse::message(REGISTER_FIRST_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string());
        };
        let mut embed = InteractionEmbed::titled("Mining Inventory").color(0x8B_45_13);
        if items.is_empty() {
            embed = embed.description("Your inventory is empty. Visit `/dig shop` to buy items.");
        } else {
            for item in items.iter().take(5) {
                let status = if item.queued { " [QUEUED]" } else { "" };
                embed = embed.field(
                    format!("{}{status}", item.name),
                    if item.description.is_empty() {
                        "No description"
                    } else {
                        &item.description
                    },
                    false,
                );
            }
        }
        embed = embed.footer(format!("{}/{INVENTORY_LIMIT} slots used", items.len()));
        let media = self.state.media.clone();
        let pickaxe_tier = i64::from(shop.owned_pickaxe_tier);
        let pickaxe_art = blocking(move || Ok(media.pickaxe_art(pickaxe_tier))).await?;
        let mut response = InteractionResponse::message("");
        if let Some(pickaxe_art) = pickaxe_art {
            embed = embed.thumbnail(format!("attachment://{}", pickaxe_art.filename));
            response = response.attachment(interaction_attachment(pickaxe_art));
        }
        let response = response.embed(embed);
        if responder.followup(response.clone()).await.is_ok() {
            return Ok(());
        }
        responder
            .followup(response.ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_artifacts(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.state.database_path.clone();
        let artifacts = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .snapshot(user_id, guild_id)
                .map(|snapshot| {
                    let mut grouped = BTreeMap::<String, bool>::new();
                    for artifact in snapshot.artifacts {
                        grouped
                            .entry(artifact.artifact_id)
                            .and_modify(|equipped| *equipped |= artifact.equipped)
                            .or_insert(artifact.equipped);
                    }
                    grouped.into_iter().collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        })
        .await?;
        let description = if artifacts.is_empty() {
            "You haven't found any artifacts yet.".to_owned()
        } else {
            artifacts
                .iter()
                .map(|(id, equipped)| {
                    format!("• `{id}`{}", if *equipped { " *(equipped)*" } else { "" })
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        respond(
            &responder,
            InteractionResponse::message("").embed(
                InteractionEmbed::titled("Artifact Catalog")
                    .description(description)
                    .color(GOLD_COLOR),
            ),
        )
        .await
    }

    async fn command_gear(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.state.database_path.clone();
        let panel = blocking(move || {
            cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(&path)
                .panel(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let Some(panel) = panel else {
            return respond(
                &responder,
                InteractionResponse::message(
                    "You don't have a tunnel yet. Use `/dig go` to start!",
                )
                .ephemeral(),
            )
            .await;
        };
        respond(
            &responder,
            gear_panel_response(&panel, user_id, guild_id, &self.state.view_nonce),
        )
        .await
    }

    async fn command_weather(
        &self,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let date = cama_domain::game_date::get_game_date();
        let path = self.state.database_path.clone();
        let weather = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .weather_projection(guild_id, &date, unix_now())
                .map_err(|error| error.to_string())
        })
        .await?;
        if weather.is_empty() {
            return respond(
                &responder,
                InteractionResponse::message("No weather today — skies are clear.").ephemeral(),
            )
            .await;
        }
        let mut embed = InteractionEmbed::titled("Today's Layer Weather")
            .description("Conditions shift daily.")
            .color(PUBLIC_COLOR);
        for entry in &weather {
            embed = embed.field(
                format!("{} — {}", entry.layer, entry.name),
                format!(
                    "*{}*\n*{}*",
                    entry.description,
                    weather_effect_copy(entry.effects)
                ),
                false,
            );
        }
        respond(
            &responder,
            InteractionResponse::message("")
                .embed(embed.footer("Weather affects all diggers in that layer today.")),
        )
        .await
    }

    async fn command_guide(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let token = self.create_guide_view(user_id, guild_id, unix_now())?;
        respond(&responder, guide_response(0, &token)).await
    }

    async fn command_admin(
        &self,
        user_id: i64,
        guild_id: i64,
        permissions: Option<u64>,
        subcommand: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !is_admin(user_id, permissions, &self.state.admin_user_ids) {
            return respond(
                &responder,
                InteractionResponse::message("Admin only.").ephemeral(),
            )
            .await;
        }
        match subcommand {
            "resetcooldown" => {
                let Some((target_id, _)) = user_option(options, "user")? else {
                    return Err("/dig admin resetcooldown requires user".to_owned());
                };
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let path = self.state.database_path.clone();
                let outcome = blocking(move || {
                    cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                        .reset_cooldown(target_id, guild_id)
                        .map_err(|error| error.to_string())
                })
                .await?;
                let content = match outcome {
                    DigAdminMutationOutcome::Applied => {
                        format!("Reset free dig cooldown for <@{target_id}>.")
                    }
                    DigAdminMutationOutcome::MissingTunnel => {
                        "That player doesn't have a tunnel.".to_owned()
                    }
                };
                responder
                    .followup(InteractionResponse::message(content).ephemeral())
                    .await
                    .map_err(|error| error.to_string())
            }
            "forceevent" => {
                let Some((target_id, _)) = user_option(options, "user")? else {
                    return Err("/dig admin forceevent requires user".to_owned());
                };
                self.state
                    .force_events
                    .lock()
                    .map_err(|_| "Dig force-event lock poisoned")?
                    .insert((target_id, guild_id));
                respond(
                    &responder,
                    InteractionResponse::message("Next dig will force an event.").ephemeral(),
                )
                .await
            }
            "setdepth" => {
                let Some((target_id, _)) = user_option(options, "user")? else {
                    return Err("/dig admin setdepth requires user".to_owned());
                };
                let depth = integer_option(options, "depth")?.unwrap_or_default().max(0);
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let path = self.state.database_path.clone();
                let outcome = blocking(move || {
                    cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                        .set_depth(target_id, guild_id, depth)
                        .map_err(|error| error.to_string())
                })
                .await?;
                let content = match outcome {
                    DigAdminMutationOutcome::Applied => {
                        format!("Set <@{target_id}> to depth **{depth}** and reset cooldown.")
                    }
                    DigAdminMutationOutcome::MissingTunnel => {
                        "That player doesn't have a tunnel.".to_owned()
                    }
                };
                responder
                    .followup(InteractionResponse::message(content).ephemeral())
                    .await
                    .map_err(|error| error.to_string())
            }
            _ => Err(format!("unknown /dig admin subcommand {subcommand:?}")),
        }
    }

    async fn command_miner(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        subcommand: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        match subcommand {
            "profile" => {
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let path = self.state.database_path.clone();
                let result =
                    blocking(move || {
                        Ok(DigMinerRuntimeService::sqlite(&path).profile(
                            user_id,
                            guild_id,
                            unix_now(),
                        ))
                    })
                    .await?;
                let response = match result {
                    Ok(profile) => InteractionResponse::message("")
                        .embed(miner_profile_embed(display_name, &profile))
                        .ephemeral(),
                    Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
                };
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            }
            "about" => {
                let backstory = string_option(options, "backstory")?.unwrap_or_default();
                let path = self.state.database_path.clone();
                let result = blocking(move || {
                    Ok(DigMinerRuntimeService::sqlite(&path).set_backstory(
                        user_id,
                        guild_id,
                        &backstory,
                        unix_now(),
                    ))
                })
                .await?;
                let response = match result {
                    Ok(profile) => InteractionResponse::message("")
                        .embed(
                            InteractionEmbed::titled("Backstory Locked In")
                                .description(miner_backstory(&profile))
                                .color(PUBLIC_COLOR)
                                .footer("This cannot be changed later."),
                        )
                        .ephemeral(),
                    Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
                };
                respond(&responder, response).await
            }
            "build" => {
                let strength = integer_option(options, "strength")?.unwrap_or_default();
                let smarts = integer_option(options, "smarts")?.unwrap_or_default();
                let stamina = integer_option(options, "stamina")?.unwrap_or_default();
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let path = self.state.database_path.clone();
                let result = blocking(move || {
                    Ok(DigMinerRuntimeService::sqlite(&path).allocate_stats(
                        user_id,
                        guild_id,
                        DigMinerAllocation {
                            strength,
                            smarts,
                            stamina,
                        },
                        unix_now(),
                    ))
                })
                .await?;
                let response = match result {
                    Ok(profile) => InteractionResponse::message("")
                        .embed(
                            InteractionEmbed::titled("S Points Spent")
                                .description(format_miner_stats(&profile))
                                .color(PUBLIC_COLOR),
                        )
                        .ephemeral(),
                    Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
                };
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            }
            "respec" => {
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let path = self.state.database_path.clone();
                let result = blocking(move || {
                    Ok(DigMinerRuntimeService::sqlite(&path).respec(user_id, guild_id, unix_now()))
                })
                .await?;
                let response = match result {
                    Ok(result) => {
                        InteractionResponse::message("")
                            .embed(
                                InteractionEmbed::titled("S Points Reset")
                                    .description(format!(
                                        "Returned **{}** allocated S points. You now have **{}** unspent S points.",
                                        result.returned_points, result.stats.unspent_points
                                    ))
                                    .color(PUBLIC_COLOR)
                                    .field(
                                        "Current Build",
                                        format_miner_stat_values(result.stats, result.effects),
                                        false,
                                    )
                                    .footer(format!("{} JC spent on the respec.", result.cost)),
                            )
                            .ephemeral()
                    }
                    Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
                };
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            }
            "autobuy" => {
                let item = string_option(options, "item")?.unwrap_or_default();
                let enabled = bool_option(options, "enabled")?.unwrap_or(false);
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let update = match item.as_str() {
                    "torch" => DigMinerAutoBuyUpdate {
                        torch: Some(enabled),
                        hard_hat: None,
                    },
                    "hard_hat" => DigMinerAutoBuyUpdate {
                        torch: None,
                        hard_hat: Some(enabled),
                    },
                    "both" => DigMinerAutoBuyUpdate {
                        torch: Some(enabled),
                        hard_hat: Some(enabled),
                    },
                    _ => {
                        return responder
                            .followup(
                                InteractionResponse::message(
                                    "Choose at least one auto-buy setting to update.",
                                )
                                .ephemeral(),
                            )
                            .await
                            .map_err(|error| error.to_string());
                    }
                };
                let path = self.state.database_path.clone();
                let result = blocking(move || {
                    Ok(DigMinerRuntimeService::sqlite(&path).set_auto_buy(
                        user_id,
                        guild_id,
                        update,
                        unix_now(),
                    ))
                })
                .await?;
                let response = match result {
                    Ok(profile) => InteractionResponse::message("")
                        .embed(
                            InteractionEmbed::titled("Dig Auto-Buy Updated")
                                .description(format_miner_auto_buy(&profile))
                                .color(PUBLIC_COLOR)
                                .footer("Auto-buy spends JC only when an actual dig starts."),
                        )
                        .ephemeral(),
                    Err(error) => InteractionResponse::message(error.to_string()).ephemeral(),
                };
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            }
            _ => Err(format!("unknown /dig miner subcommand {subcommand:?}")),
        }
    }

    async fn handle_route_component(
        &self,
        raw: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some((nonce, owner_id, expected_guild, token, route_id)) = parse_route_component(raw)
        else {
            return respond(
                &responder,
                InteractionResponse::message(
                    "This route view expired after a restart. Use `/dig go` to reopen it.",
                )
                .ephemeral(),
            )
            .await;
        };
        if nonce != self.state.view_nonce {
            return respond(
                &responder,
                InteractionResponse::message(
                    "This route view expired after a restart. Use `/dig go` to reopen it.",
                )
                .ephemeral(),
            )
            .await;
        }
        if owner_id != user_id || expected_guild != guild_id {
            return respond(
                &responder,
                InteractionResponse::message("Only the tunnel owner can choose this route.")
                    .ephemeral(),
            )
            .await;
        }

        let now = unix_now();
        let admission = self.claim_route_view(&token, user_id, guild_id, &route_id, now)?;
        let choice = match admission {
            DigRouteViewAdmission::Admitted(choice) => choice,
            DigRouteViewAdmission::WrongOwner => {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can choose this route.")
                        .ephemeral(),
                )
                .await;
            }
            DigRouteViewAdmission::Expired => {
                return respond(
                    &responder,
                    InteractionResponse::message(
                        "This route view expired. Use `/dig go` to reopen it.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            DigRouteViewAdmission::AlreadyResolved => {
                // discord.py's callback acknowledges duplicate clicks without
                // issuing another follow-up.  Keep that quiet one-shot
                // behavior while still acknowledging the component.
                responder
                    .defer(false)
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            DigRouteViewAdmission::InvalidRoute => {
                return respond(
                    &responder,
                    InteractionResponse::message("That route was not offered for this junction.")
                        .ephemeral(),
                )
                .await;
            }
        };

        if let Err(error) = responder.defer(false).await {
            let _ = self.reset_route_view_claim(&token, user_id, guild_id);
            return Err(error.to_string());
        }
        let path = self.state.database_path.clone();
        let route_id_for_db = route_id.clone();
        let result = blocking(move || {
            cama_app::dig_runtime::DigRuntimeService::sqlite(&path)
                .choose_route(user_id, guild_id, &route_id_for_db, now)
                .map_err(|error| error.to_string())
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let reopened = self.reset_route_view_claim(&token, user_id, guild_id)?;
                return responder
                    .followup(
                        InteractionResponse::message(if reopened {
                            "The passage shifted before it could be marked. Try again."
                        } else {
                            "This route view expired. Use `/dig go` to reopen it."
                        })
                        .ephemeral(),
                    )
                    .await
                    .map_err(|followup_error| {
                        format!("{error}; could not report route failure: {followup_error}")
                    });
            }
        };
        if !result.success {
            let reopened = self.reset_route_view_claim(&token, user_id, guild_id)?;
            return responder
                .followup(
                    InteractionResponse::message(if reopened {
                        result.error.unwrap_or_else(|| {
                            "The passage shifted before it could be marked. Try again.".to_owned()
                        })
                    } else {
                        "This route view expired. Use `/dig go` to reopen it.".to_owned()
                    })
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        }
        self.resolve_route_view(&token, user_id, guild_id)?;
        let Some(route) = choice
            .offered_routes
            .iter()
            .find(|route| route.id == route_id)
        else {
            return responder
                .followup(
                    InteractionResponse::message("That route was not offered for this junction.")
                        .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        };
        responder
            .edit_original(locked_route_response(&choice.layer, route))
            .await
            .map_err(|error| error.to_string())
    }
}

fn dig_options() -> Vec<CommandOptionSpec> {
    let mut options = vec![
        subcommand("go", "Dig deeper into your tunnel", Vec::new()),
        subcommand(
            "help",
            "Help another player's tunnel",
            vec![
                CommandOptionSpec::new("user", "The player to help", CommandOptionKind::User)
                    .required(true),
            ],
        ),
        subcommand(
            "sabotage",
            "Sabotage another player's tunnel",
            vec![
                CommandOptionSpec::new("user", "The player to sabotage", CommandOptionKind::User)
                    .required(true),
            ],
        ),
        subcommand(
            "info",
            "View tunnel information",
            vec![CommandOptionSpec::new(
                "user",
                "View another player's tunnel (optional)",
                CommandOptionKind::User,
            )],
        ),
        subcommand("leaderboard", "View top tunnels", Vec::new()),
        subcommand(
            "halloffame",
            "View the hall of fame (best prestige run scores)",
            Vec::new(),
        ),
        subcommand(
            "use",
            "Queue a consumable for your next dig",
            vec![
                CommandOptionSpec::new("item", "The item to use", CommandOptionKind::String)
                    .required(true)
                    .autocomplete(),
            ],
        ),
        subcommand(
            "gift",
            "Gift a relic to another player",
            vec![
                CommandOptionSpec::new("user", "The player to gift to", CommandOptionKind::User)
                    .required(true),
                CommandOptionSpec::new("artifact", "The relic to gift", CommandOptionKind::String)
                    .required(true)
                    .autocomplete(),
            ],
        ),
        subcommand("shop", "Browse the mining shop", Vec::new()),
        subcommand(
            "buy",
            "Buy an item from the mining shop",
            vec![
                CommandOptionSpec::new("item", "Item to buy", CommandOptionKind::String)
                    .required(true)
                    .autocomplete(),
            ],
        ),
        subcommand("flex", "Show off your mining stats", Vec::new()),
        subcommand(
            "prestige",
            "Prestige your tunnel (reset depth, gain a perk)",
            Vec::new(),
        ),
        subcommand(
            "abandon",
            "Abandon your tunnel (partial refund)",
            Vec::new(),
        ),
        subcommand("trap", "Set a trap in your tunnel", Vec::new()),
        subcommand("insure", "Buy cave-in insurance", Vec::new()),
        subcommand("inventory", "View your mining inventory", Vec::new()),
        subcommand(
            "artifacts",
            "View all artifacts and the ones you own",
            Vec::new(),
        ),
        subcommand("gear", "Manage your boss-combat gear", Vec::new()),
        subcommand(
            "weather",
            "View today's layer weather conditions",
            Vec::new(),
        ),
        subcommand("guide", "Learn how to dig", Vec::new()),
    ];
    options.push(
        CommandOptionSpec::new(
            "admin",
            "Dig maintenance commands",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "resetcooldown",
                "Reset a player's free dig cooldown (Admin only)",
                vec![
                    CommandOptionSpec::new(
                        "user",
                        "The player whose cooldown to reset",
                        CommandOptionKind::User,
                    )
                    .required(true),
                ],
            ),
            subcommand(
                "forceevent",
                "Force next dig to trigger an event (Admin only)",
                vec![
                    CommandOptionSpec::new(
                        "user",
                        "The player whose next dig gets an event",
                        CommandOptionKind::User,
                    )
                    .required(true),
                ],
            ),
            subcommand(
                "setdepth",
                "Set a player's tunnel depth (Admin only)",
                vec![
                    CommandOptionSpec::new("user", "The player", CommandOptionKind::User)
                        .required(true),
                    CommandOptionSpec::new("depth", "New depth value", CommandOptionKind::Integer)
                        .required(true),
                ],
            ),
        ]),
    );
    options.push(
        CommandOptionSpec::new(
            "miner",
            "Miner profile and S stats",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand("profile", "View your miner profile and S stats", Vec::new()),
            subcommand(
                "about",
                "Set your miner backstory once",
                vec![{
                    let mut backstory = CommandOptionSpec::new(
                        "backstory",
                        "Short backstory blurb for the AI Dungeon Master",
                        CommandOptionKind::String,
                    )
                    .required(true);
                    backstory.max_length = Some(500);
                    backstory
                }],
            ),
            subcommand(
                "build",
                "Spend unallocated points on Strength, Smarts, and Stamina",
                vec![
                    CommandOptionSpec::new(
                        "strength",
                        "Points to add. Increases how far you dig each action.",
                        CommandOptionKind::Integer,
                    ),
                    CommandOptionSpec::new(
                        "smarts",
                        "Points to add. Helps you read the stone and avoid collapses.",
                        CommandOptionKind::Integer,
                    ),
                    CommandOptionSpec::new(
                        "stamina",
                        "Points to add. Keeps you digging longer between rests.",
                        CommandOptionKind::Integer,
                    ),
                ],
            ),
            subcommand(
                "respec",
                "Reset your allocated S points for 50 JC",
                Vec::new(),
            ),
            subcommand(
                "autobuy",
                "Auto-buy Torch and/or Hard Hat for each dig",
                vec![
                    CommandOptionSpec {
                        name: "item".to_owned(),
                        description: "Which auto-buy setting to update".to_owned(),
                        kind: CommandOptionKind::String,
                        required: true,
                        options: Vec::new(),
                        choices: vec![
                            CommandOptionChoice::String {
                                name: "Torch".to_owned(),
                                value: "torch".to_owned(),
                            },
                            CommandOptionChoice::String {
                                name: "Hard Hat".to_owned(),
                                value: "hard_hat".to_owned(),
                            },
                            CommandOptionChoice::String {
                                name: "Both".to_owned(),
                                value: "both".to_owned(),
                            },
                        ],
                        min_integer: None,
                        max_integer: None,
                        min_number: None,
                        max_number: None,
                        min_length: None,
                        max_length: None,
                        autocomplete: false,
                    },
                    CommandOptionSpec::new(
                        "enabled",
                        "Whether to auto-buy this item on each real dig",
                        CommandOptionKind::Boolean,
                    )
                    .required(true),
                ],
            ),
        ]),
    );
    options
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

// -------------------------------------------------------------------------
// Interaction plumbing
// -------------------------------------------------------------------------

type DigRuntimeResult = cama_app::dig_runtime::DigRuntimeOutcome;

struct RuntimeRelicEntropy;

impl cama_app::dig_relic_recycling::RelicEntropy for RuntimeRelicEntropy {
    fn unit(&mut self) -> f64 {
        fastrand::f64()
    }

    fn choose_index(&mut self, length: usize) -> usize {
        if length == 0 {
            0
        } else {
            fastrand::usize(..length)
        }
    }
}

#[derive(Clone)]
struct RuntimeBossEntropy {
    random: Arc<Mutex<fastrand::Rng>>,
}

impl std::fmt::Debug for RuntimeBossEntropy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBossEntropy")
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeBossEntropy {
    fn default() -> Self {
        Self {
            random: Arc::new(Mutex::new(fastrand::Rng::new())),
        }
    }
}

impl RuntimeBossEntropy {
    /// The guarded value is a plain RNG, so a panic elsewhere while the lock
    /// was held cannot leave it in a broken state. Recovering from poison
    /// keeps boss encounters alive instead of panicking until restart.
    fn random(&self) -> std::sync::MutexGuard<'_, fastrand::Rng> {
        self.random.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn reseed(&self, seed: u64) {
        *self.random() = fastrand::Rng::with_seed(seed);
    }
}

impl EntropyPort for RuntimeBossEntropy {
    fn next_unit(&mut self) -> f64 {
        self.random().f64()
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            0
        } else {
            self.random().usize(..upper_bound)
        }
    }

    fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32 {
        if minimum >= maximum {
            minimum
        } else {
            self.random().i32(minimum..=maximum)
        }
    }
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} id is outside SQLite's signed range"))
}

fn parse_boss_owner(raw: &str) -> Result<(i64, i64), String> {
    let mut parts = raw.split(':');
    let owner_id = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| "This boss interaction expired.".to_owned())?;
    let guild_id = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| "This boss interaction expired.".to_owned())?;
    if parts.next().is_some() {
        return Err("This boss interaction expired.".to_owned());
    }
    Ok((owner_id, guild_id))
}

async fn show_boss_modal(
    responder: &dyn InteractionResponder,
    owner_id: i64,
    guild_id: i64,
    wager_allowed: bool,
) -> Result<(), String> {
    let mut risk =
        InteractionTextInput::short("risk_tier", "Risk Tier (cautious / bold / reckless)");
    risk.placeholder = Some("bold".to_owned());
    risk.min_length = Some(1);
    risk.max_length = Some(10);
    let (custom_id, title, inputs) = if wager_allowed {
        let mut wager = InteractionTextInput::short("wager", "Wager Amount (max 1,000 JC)");
        wager.placeholder = Some("0-1000".to_owned());
        wager.min_length = Some(1);
        wager.max_length = Some(10);
        (
            format!("dig:boss:wager:{owner_id}:{guild_id}"),
            "Boss Fight Wager",
            vec![risk, wager],
        )
    } else {
        (
            format!("dig:boss:risk:{owner_id}:{guild_id}"),
            "Boss Phase Risk",
            vec![risk],
        )
    };
    responder
        .show_modal(InteractionModal {
            custom_id,
            title: title.to_owned(),
            inputs,
        })
        .await
        .map_err(|error| error.to_string())
}

fn parse_risk_tier(value: &str) -> Option<RiskTier> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cautious" => Some(RiskTier::Cautious),
        "bold" => Some(RiskTier::Bold),
        "reckless" => Some(RiskTier::Reckless),
        _ => None,
    }
}

fn boss_start_has_next_phase(result: &DigBossCallResult<DigBossStartOutcome>) -> bool {
    match &result.outcome {
        DigBossStartOutcome::Paused(_) => false,
        DigBossStartOutcome::RegularResolved(resolved) => resolved.phase_transition.is_some(),
        DigBossStartOutcome::PinnacleResolved(resolved) => {
            resolved.won && resolved.next_phase > resolved.phase
        }
    }
}

fn boss_start_is_resolved(result: &DigBossCallResult<DigBossStartOutcome>) -> bool {
    !matches!(&result.outcome, DigBossStartOutcome::Paused(_))
}

fn boss_start_neon_victory(
    result: &DigBossCallResult<DigBossStartOutcome>,
) -> Option<DigBossNeonVictory> {
    result.action_id?;
    match &result.outcome {
        DigBossStartOutcome::Paused(_) => None,
        DigBossStartOutcome::RegularResolved(resolved)
            if resolved.won && resolved.phase_transition.is_none() =>
        {
            Some(DigBossNeonVictory {
                boss_name: cama_app::dig_bosses::boss_by_id(&resolved.boss_id)
                    .map_or_else(|| resolved.boss_id.clone(), |boss| boss.name.to_owned()),
                boundary: i64::from(resolved.boundary),
                layer_name: layer_at(i64::from(resolved.boundary)).name.to_owned(),
                // `jc_delta` is the committed net payout after economy,
                // bankruptcy, and vanity sinks—the Rust counterpart to
                // Python's resolved `payout` field.
                jc_delta: resolved.jc_delta,
                gear_drop: result.gear_drop.is_some(),
                // `prestige_relic_drop` is a separate Pinnacle/relic reward,
                // not the Python trophy-relic boost signal.
                trophy_relic_drop: false,
            })
        }
        DigBossStartOutcome::PinnacleResolved(resolved)
            if resolved.won && resolved.phase == 3 && resolved.next_phase == 0 =>
        {
            Some(DigBossNeonVictory {
                boss_name: resolved.boss_name.clone(),
                boundary: i64::from(PINNACLE_DEPTH),
                layer_name: layer_at(i64::from(PINNACLE_DEPTH)).name.to_owned(),
                jc_delta: resolved.jc_delta,
                gear_drop: result.gear_drop.is_some(),
                trophy_relic_drop: false,
            })
        }
        DigBossStartOutcome::RegularResolved(_) | DigBossStartOutcome::PinnacleResolved(_) => None,
    }
}

fn boss_resume_neon_victory(
    result: &DigBossCallResult<DigBossResolvedOutcome>,
) -> Option<DigBossNeonVictory> {
    result.action_id?;
    match &result.outcome {
        DigBossResolvedOutcome::Regular(resolved)
            if resolved.won && resolved.phase_transition.is_none() =>
        {
            Some(DigBossNeonVictory {
                boss_name: cama_app::dig_bosses::boss_by_id(&resolved.boss_id)
                    .map_or_else(|| resolved.boss_id.clone(), |boss| boss.name.to_owned()),
                boundary: i64::from(resolved.boundary),
                layer_name: layer_at(i64::from(resolved.boundary)).name.to_owned(),
                jc_delta: resolved.jc_delta,
                gear_drop: result.gear_drop.is_some(),
                trophy_relic_drop: false,
            })
        }
        DigBossResolvedOutcome::Pinnacle(resolved)
            if resolved.won && resolved.phase == 3 && resolved.next_phase == 0 =>
        {
            Some(DigBossNeonVictory {
                boss_name: resolved.boss_name.clone(),
                boundary: i64::from(PINNACLE_DEPTH),
                layer_name: layer_at(i64::from(PINNACLE_DEPTH)).name.to_owned(),
                jc_delta: resolved.jc_delta,
                gear_drop: result.gear_drop.is_some(),
                trophy_relic_drop: false,
            })
        }
        DigBossResolvedOutcome::Regular(_) | DigBossResolvedOutcome::Pinnacle(_) => None,
    }
}

fn boss_resume_has_next_phase(result: &DigBossCallResult<DigBossResolvedOutcome>) -> bool {
    match &result.outcome {
        DigBossResolvedOutcome::Regular(resolved) => resolved.phase_transition.is_some(),
        DigBossResolvedOutcome::Pinnacle(resolved) => {
            resolved.won && resolved.next_phase > resolved.phase
        }
    }
}

fn boss_resume_is_resolved<T>(result: &DigBossCallResult<T>) -> bool {
    result.action_id.is_some()
}

async fn blocking<T, F>(job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    crate::ids::blocking("Dig SQLite", job).await
}

async fn respond(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), String> {
    responder
        .respond(response)
        .await
        .map_err(|error| error.to_string())
}

// Boss and gear views are process-local in Python.
// A restart therefore yields an explicit, safe recovery response.
async fn expired_dig_component(responder: &Arc<dyn InteractionResponder>) -> Result<(), String> {
    respond(
        responder,
        InteractionResponse::message("This Dig interaction expired. Use `/dig go` to reopen it.")
            .ephemeral(),
    )
    .await
}

fn miner_profile_embed(display_name: &str, profile: &DigMinerProfile) -> InteractionEmbed {
    InteractionEmbed::titled(format!("{display_name} - Miner Profile"))
        .description(miner_backstory(profile))
        .color(PUBLIC_COLOR)
        .field("S Stats", format_miner_stats(profile), false)
        .field("Auto-Buy", format_miner_auto_buy(profile), false)
        .footer("Backstory locks after you set it. Boss first clears grant one extra S point.")
}

fn miner_backstory(profile: &DigMinerProfile) -> &str {
    if profile.backstory.is_empty() {
        "Backstory not set."
    } else {
        &profile.backstory
    }
}

fn format_miner_stats(profile: &DigMinerProfile) -> String {
    format_miner_stat_values(profile.stats, profile.effects)
}

fn format_miner_stat_values(
    stats: cama_app::dig_miner_runtime::DigMinerStats,
    effects: cama_app::dig_miner_runtime::DigMinerEffects,
) -> String {
    let cave_in_percent = (effects.cave_in_reduction.max(0.0) * 100.0).round();
    let cooldown_percent = ((1.0 - effects.cooldown_multiplier).max(0.0) * 100.0).round();
    format!(
        "Strength **{}** | Smarts **{}** | Stamina **{}**\nPoints: **{}** total, **{}** unspent\nEffects: +{}/+{} advance range, -{cave_in_percent:.0}% cave-in, -{cooldown_percent:.0}% cooldown/paid costs",
        stats.strength,
        stats.smarts,
        stats.stamina,
        stats.stat_points,
        stats.unspent_points,
        effects.advance_min_bonus,
        effects.advance_max_bonus,
    )
}

fn format_miner_auto_buy(profile: &DigMinerProfile) -> String {
    format!(
        "Torch: **{}**\nHard Hat: **{}**",
        if profile.auto_buy.torch { "ON" } else { "OFF" },
        if profile.auto_buy.hard_hat {
            "ON"
        } else {
            "OFF"
        }
    )
}

fn sabotage_result_response(
    result: &cama_app::dig_social_runtime::DigSabotageResult,
    target_name: &str,
) -> InteractionResponse {
    let (title, description, color) = if result.trap_triggered {
        let trap_message = result
            .trap_detail
            .as_ref()
            .map_or("", |detail| detail.message.as_str());
        (
            "Trap Triggered!",
            format!("Your sabotage attempt backfired!\n{trap_message}"),
            0xFF_00_00,
        )
    } else if !result.sabotage_hit {
        (
            "Sabotage Missed",
            format!(
                "You tried to sabotage **{target_name}**'s tunnel, but the strike missed.\nDamage dealt: **{}** blocks",
                result.damage
            ),
            0x2C_2F_33,
        )
    } else {
        let mut description = format!(
            "You sabotaged **{target_name}**'s tunnel!\nDamage dealt: **{}** blocks",
            result.damage
        );
        if let Some(steal) = result.prediction_contract_steal.as_ref() {
            description.push_str(&format!(
                "\nStole **{} {}** prediction contracts from market **#{}**.",
                steal.contracts,
                steal.side.to_ascii_uppercase(),
                steal.prediction_id
            ));
        }
        ("Sabotage Successful", description, 0x2C_2F_33)
    };
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(title)
            .description(description)
            .color(color),
    )
}

fn dig_neon_response(result: cama_app::neon_degen::NeonResult) -> InteractionResponse {
    let mut response =
        InteractionResponse::message(result.text_block.or(result.footer_text).unwrap_or_default())
            .without_mentions();
    if let Some(gif) = result.gif_file {
        response = response.attachment(InteractionAttachment::bytes(
            "jopat_terminal.gif",
            gif.bytes,
        ));
    }
    response
}

fn cave_in_detail(outcome: &cama_app::dig_runtime::DigRuntimeOutcome) -> Option<serde_json::Value> {
    outcome
        .cave_in_detail
        .as_deref()
        .and_then(|detail| serde_json::from_str(detail).ok())
}

fn catastrophic_cave_in_block_loss(
    outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
) -> Option<i64> {
    let detail = cave_in_detail(outcome)?;
    (detail.get("type").and_then(serde_json::Value::as_str) == Some("catastrophic")).then(|| {
        detail
            .get("block_loss")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_else(|| outcome.depth_before.saturating_sub(outcome.depth_after))
            .max(0)
    })
}

#[must_use]
fn catastrophic_flame_line(action_id: i64) -> &'static str {
    let index = action_id.unsigned_abs() as usize % CATASTROPHIC_LINES.len();
    CATASTROPHIC_LINES[index]
}

#[must_use]
fn dig_artifact_neon_info(artifact_id: &str) -> Option<(String, String)> {
    if let Some(pinnacle) = artifact_id.strip_prefix("pinnacle:") {
        let name = pinnacle.split(':').next().unwrap_or("a relic");
        return Some((name.to_owned(), "legendary".to_owned()));
    }
    cama_app::dig_loot::artifact_catalog()
        .into_iter()
        .find(|artifact| {
            artifact.id == artifact_id
                && matches!(
                    artifact.rarity,
                    cama_app::dig_loot::Rarity::Rare | cama_app::dig_loot::Rarity::Legendary
                )
        })
        .map(|artifact| {
            (
                artifact.name.to_owned(),
                artifact.rarity.as_str().to_owned(),
            )
        })
}

#[must_use]
fn dig_result_reactions(
    outcome: &cama_app::dig_runtime::DigRuntimeOutcome,
    delivery: Option<&DigRuntimeDeliverySnapshot>,
) -> Vec<&'static str> {
    let kind = delivery.map(|delivery| delivery.render.kind);
    if kind == Some(DigRuntimeRenderKind::First) || outcome.first_dig {
        return Vec::new();
    }
    if kind == Some(DigRuntimeRenderKind::Boss) || outcome.boss_boundary.is_some() {
        return vec!["💀"];
    }
    if kind == Some(DigRuntimeRenderKind::Event)
        && delivery.is_some_and(|delivery| {
            delivery.render.event_kind != Some(cama_app::dig_runtime::DigRuntimeEventKind::Simple)
        })
    {
        return Vec::new();
    }
    let mut reactions = vec!["⛏️"];
    if outcome.cave_in {
        reactions.push("💥");
    }
    if outcome.artifact_id.is_some() {
        reactions.push("💎");
    }
    reactions
}

#[must_use]
fn deterministic_dig_bonus_roll(action_id: i64) -> f64 {
    // The action id is already durable. Deriving the roll from it avoids a
    // second RNG draw when a delivery is retried after a process restart.
    let mut state = action_id.unsigned_abs().wrapping_add(0x9E37_79B9_7F4A_7C15);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    state ^= state >> 31;
    state as f64 / u64::MAX as f64
}

#[must_use]
fn dig_pet_activity_source_key(action_id: i64) -> String {
    format!("dig:{action_id}")
}

fn prestige_admission_response(admission: DigPrestigeViewAdmission) -> Option<InteractionResponse> {
    let content = match admission {
        DigPrestigeViewAdmission::Admitted => return None,
        DigPrestigeViewAdmission::WrongOwner => "This isn't your prestige.",
        DigPrestigeViewAdmission::Expired => "This prestige expired. Use `/dig prestige` again.",
        DigPrestigeViewAdmission::AlreadyClaimed => "You've already made your selection.",
        DigPrestigeViewAdmission::InvalidTransition => {
            "This prestige selection is no longer available."
        }
    };
    Some(InteractionResponse::message(content).ephemeral())
}

fn abandon_admission_response(admission: DigAbandonViewAdmission) -> Option<InteractionResponse> {
    let content = match admission {
        DigAbandonViewAdmission::Admitted => return None,
        DigAbandonViewAdmission::WrongOwner => "This isn't your tunnel.",
        DigAbandonViewAdmission::Expired => "This abandonment expired. Use `/dig abandon` again.",
        DigAbandonViewAdmission::AlreadyResolved => "You've already answered this abandonment.",
    };
    Some(InteractionResponse::message(content).ephemeral())
}

fn prestige_preview_embed(preview: &DigPrestigePreview) -> InteractionEmbed {
    let mut embed = InteractionEmbed::titled(format!(
        "Prestige to P{}?",
        preview.target_level
    ))
    .description(format!(
        "This will **reset your tunnel depth to 0** but grant a permanent perk.\n\n**Run Score:** {}\n",
        preview.run_score
    ))
    .color(0xFF_D7_00);
    if let Some(ascension) = &preview.ascension_unlock {
        embed = embed.field(
            format!("Ascension Unlock: {}", ascension.name),
            format!(
                "Penalty: {}\nReward: {}",
                ascension.penalty, ascension.reward
            ),
            false,
        );
    }
    embed
}

fn prestige_mutation_response(
    preview: &DigPrestigePreview,
    view_token: &str,
) -> InteractionResponse {
    let mut embed = prestige_preview_embed(preview);
    let Some(mutation) = &preview.mutation else {
        return InteractionResponse::message("").ephemeral().embed(embed);
    };
    embed = embed.field(
        format!("Forced Mutation: {}", mutation.forced.name),
        &mutation.forced.description,
        false,
    );
    embed = embed.field(
        "Choose a Mutation",
        mutation
            .choices
            .iter()
            .map(|choice| format!("**{}** — {}", choice.name, choice.description))
            .collect::<Vec<_>>()
            .join("\n"),
        false,
    );
    let buttons = mutation
        .choices
        .iter()
        .map(|choice| {
            InteractionButton::new(
                format!("dig:prestige-mutation:{view_token}:{}", choice.id),
                &choice.name,
            )
            .style(if choice.positive {
                InteractionButtonStyle::Success
            } else {
                InteractionButtonStyle::Danger
            })
        })
        .collect();
    InteractionResponse::message("")
        .ephemeral()
        .embed(embed)
        .action_row(InteractionActionRow::buttons(buttons))
}

fn prestige_perk_response(
    preview: &DigPrestigePreview,
    view_token: &str,
    mutation_choice: Option<&str>,
) -> InteractionResponse {
    let perk_lines = preview
        .offered_perks
        .iter()
        .map(|perk| format!("**{}**", perk.name))
        .collect::<Vec<_>>()
        .join("\n");
    let embed = if mutation_choice.is_some() {
        // Python sends a fresh, deliberately minimal perk picker after the
        // mutation click instead of carrying the mutation preview forward.
        InteractionEmbed::titled("Choose a Prestige Perk")
            .description(perk_lines)
            .color(0xFF_D7_00)
    } else {
        prestige_preview_embed(preview).field("Choose a Perk", perk_lines, false)
    };
    let mutation = mutation_choice.unwrap_or("_");
    let buttons = preview
        .offered_perks
        .iter()
        .map(|perk| {
            InteractionButton::new(
                format!("dig:prestige-perk:{view_token}:{}:{mutation}", perk.id),
                &perk.name,
            )
        })
        .collect();
    InteractionResponse::message("")
        .ephemeral()
        .embed(embed)
        .action_row(InteractionActionRow::buttons(buttons))
}

fn prestige_result_response(result: &DigPrestigeResult) -> InteractionResponse {
    let mut description = vec![
        format!("You selected **{}**.", result.perk_name),
        format!(
            "Run Score: **{}** (Best: {})",
            result.run_score, result.best_run_score
        ),
        "Your tunnel has been reset. Dig deeper!".to_owned(),
    ];
    if let Some(relic) = result.prestige_grant.relic {
        description.push(format!(
            "Prestige Grant: **+{}** {JOPACOIN_EMOTE} · **{}** ({:?})",
            result.prestige_grant.jc, relic.name, relic.rarity
        ));
    } else {
        description.push(format!(
            "Prestige Grant: **+{}** {JOPACOIN_EMOTE}",
            result.prestige_grant.jc
        ));
    }
    let mut embed =
        InteractionEmbed::titled(format!("Prestige {} Complete!", result.prestige_level))
            .description(description.join("\n"))
            .color(0xFF_D7_00);
    if let Some(ascension) = &result.ascension_unlocked {
        embed = embed.field(
            format!("Ascension Unlocked: {}", ascension.name),
            format!(
                "Penalty: {}\nReward: {}",
                ascension.penalty, ascension.reward
            ),
            false,
        );
    }
    if let Some(mutations) = &result.mutations {
        let mut lines = vec![format!(
            "Forced: **{}** — {}",
            mutations.forced.name, mutations.forced.description
        )];
        if let Some(chosen) = &mutations.chosen {
            lines.push(format!(
                "Chosen: **{}** — {}",
                chosen.name, chosen.description
            ));
        }
        embed = embed.field("Mutations", lines.join("\n"), false);
    }
    InteractionResponse::message("").ephemeral().embed(embed)
}

fn command_path(options: &[InteractionOption]) -> Vec<String> {
    let Some(option) = options.iter().find(|option| {
        matches!(
            option.value,
            InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
        )
    }) else {
        return Vec::new();
    };
    let mut path = vec![option.name.clone()];
    let children = match &option.value {
        InteractionValue::Subcommand(children) | InteractionValue::SubcommandGroup(children) => {
            children
        }
        _ => return path,
    };
    path.extend(command_path(children));
    path
}

fn option<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a InteractionValue> {
    options.iter().find_map(|candidate| {
        if candidate.name == name {
            Some(&candidate.value)
        } else {
            match &candidate.value {
                InteractionValue::Subcommand(children)
                | InteractionValue::SubcommandGroup(children) => option(children, name),
                _ => None,
            }
        }
    })
}

fn user_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<(i64, Option<String>)>, String> {
    let Some(value) = option(options, name) else {
        return Ok(None);
    };
    match value {
        InteractionValue::User {
            id, display_name, ..
        } => Ok(Some((signed_id(*id, name)?, display_name.clone()))),
        InteractionValue::Unknown => Ok(None),
        _ => Err(format!("{name} must be a Discord user")),
    }
}

fn string_option(options: &[InteractionOption], name: &str) -> Result<Option<String>, String> {
    match option(options, name) {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{name} must be text")),
    }
}

fn integer_option(options: &[InteractionOption], name: &str) -> Result<Option<i64>, String> {
    match option(options, name) {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::Integer(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{name} must be an integer")),
    }
}

fn bool_option(options: &[InteractionOption], name: &str) -> Result<Option<bool>, String> {
    match option(options, name) {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{name} must be true or false")),
    }
}

fn is_admin(user_id: i64, permissions: Option<u64>, admin_ids: &BTreeSet<i64>) -> bool {
    admin_ids.contains(&user_id)
        || permissions.is_some_and(|bits| bits & ((1_u64 << 3) | (1_u64 << 5)) != 0)
}

#[derive(Clone, Copy)]
struct DigGuidePage {
    title: &'static str,
    description: &'static str,
    color: u32,
}

const DIG_GUIDE_PAGES: [DigGuidePage; 4] = [
    DigGuidePage {
        title: "Dig Guide — Basics",
        description: "**How Digging Works**\nUse `/dig` to advance your tunnel deeper. Each dig action advances you a number of blocks based on your pickaxe tier, active items, and a bit of luck.\n\n**Layers**\nThe mine has eight layers: **Dirt**, **Stone**, **Crystal**, **Magma**, **Abyss**, **Fungal Depths**, **Frozen Core**, and **The Hollow**. Each layer is harder but more rewarding.\n\n**Cave-ins**\nRandom cave-ins can collapse part of your tunnel, costing you depth. Insurance and reinforcements reduce the damage.\n\n**Decay**\nInactive tunnels slowly decay over time. Keep digging to stay deep!",
        color: 0x8B_45_13,
    },
    DigGuidePage {
        title: "Dig Guide — Items & Pickaxes",
        description: "**Consumables**\nBuy consumables from `/dig shop` and queue them with `/dig use`. You can hold up to 8 items at a time. Queued items are used on your next dig.\n\n**Pickaxes**\nUpgrade your pickaxe from `/dig shop` using `/dig buy`. Higher tiers require depth milestones, JC, and prestige levels. Better pickaxes dig more blocks per action.\n\n**Relics**\nRare artifacts found while digging. Equip them for passive bonuses. Gift duplicates to friends with `/dig gift`.",
        color: 0x80_80_80,
    },
    DigGuidePage {
        title: "Dig Guide — Bosses",
        description: "**Boss Encounters**\nBosses guard layer transitions. When you encounter one, you can:\n- **Fight**: Wager JC and choose a risk tier (Cautious/Bold/Reckless)\n- **Retreat**: Back away safely, keeping your depth\n- **Scout**: Use a lantern to reveal boss stats first\n\n**Cheering**\nOther players can cheer for you during boss fights, boosting your success chance. Rally your friends!\n\n**Risk Tiers**\n- **Cautious**: Lower wager multiplier, higher success chance\n- **Bold**: Balanced risk and reward\n- **Reckless**: Huge payoff potential, but high failure risk",
        color: 0x00_CE_D1,
    },
    DigGuidePage {
        title: "Dig Guide — Prestige",
        description: "**Prestige System**\nOnce you reach a deep enough depth, you can prestige. This resets your tunnel depth to zero but grants:\n- A permanent prestige level\n- A choice of prestige perks\n- Access to higher pickaxe tiers\n- Bragging rights\n\n**Perks**\nEach prestige lets you choose one perk that persists across resets. Choose wisely — they shape your digging strategy.\n\n**Relics**\nSome relics are only available at higher prestige levels.",
        color: 0xFF_45_00,
    },
];

fn guide_response(page: usize, token: &str) -> InteractionResponse {
    let page = page.min(DIG_GUIDE_PAGES.len() - 1);
    let definition = DIG_GUIDE_PAGES[page];
    let row = InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("dig:guide:{token}:previous"), "Previous")
            .style(InteractionButtonStyle::Secondary)
            .disabled(page == 0),
        InteractionButton::new(format!("dig:guide:{token}:next"), "Next")
            .style(InteractionButtonStyle::Secondary)
            .disabled(page == DIG_GUIDE_PAGES.len() - 1),
    ]);
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled(definition.title)
                .description(definition.description)
                .color(definition.color),
        )
        .action_row(row)
}

fn expired_guide_response() -> InteractionResponse {
    InteractionResponse::message("*The moment passed.*").action_row(InteractionActionRow::buttons(
        vec![
            InteractionButton::new("dig:guide:expired:previous", "Previous")
                .style(InteractionButtonStyle::Secondary)
                .disabled(true),
            InteractionButton::new("dig:guide:expired:next", "Next")
                .style(InteractionButtonStyle::Secondary)
                .disabled(true),
        ],
    ))
}

fn flex_response(
    flex: &DigRuntimeFlexData,
    display_name: &str,
    avatar_url: Option<&str>,
    roast_index: usize,
) -> InteractionResponse {
    let mut embed =
        InteractionEmbed::titled(format!("{display_name}'s Mining Profile")).color(FLEX_COLOR);
    if flex.depth <= 0 && flex.total_digs <= 1 {
        embed = embed.description(format!(
            "*{}*",
            FLEX_ROASTS[roast_index % FLEX_ROASTS.len()]
        ));
    } else {
        if !flex.titles.is_empty() {
            embed = embed.description(format!("*\"{}\"*", flex.titles.join(" | ")));
        }
        if !flex.prestige_emoji.is_empty() {
            let description = format!(
                "{}  {}",
                embed.description.as_deref().unwrap_or_default(),
                flex.prestige_emoji
            );
            embed = embed.description(description);
        }
        let mut stats = format!(
            "Tunnel: **{}**\nDepth: **{}** ({})\nTotal digs: **{}**\nTotal JC earned: **{}**\nStreak: **{}** days",
            flex.tunnel_name,
            flex.depth,
            flex.layer,
            flex.total_digs,
            flex.total_jc_earned,
            flex.streak,
        );
        if flex.prestige_level > 0 {
            stats.push_str(&format!("\nPrestige: **{}**", flex.prestige_level));
        }
        embed = embed.field("Stats", stats, false);
    }
    if let Some(avatar_url) = avatar_url {
        embed = embed.thumbnail(avatar_url);
    }
    InteractionResponse::message("").embed(embed)
}

fn weather_effect_copy(effects: DigWeatherEffects) -> String {
    let mut lines = Vec::new();
    if effects.cave_in_bonus != 0.0 {
        lines.push(if effects.cave_in_bonus > 0.0 {
            "cave-in risk surges"
        } else {
            "cave-in risk eases"
        });
    }
    if effects.jc_multiplier != 0.0 {
        lines.push(if effects.jc_multiplier > 0.0 {
            "ore veins are rich"
        } else {
            "ore veins are thin"
        });
    }
    if effects.jc_bonus != 0 {
        lines.push(if effects.jc_bonus > 0 {
            "seams glitter"
        } else {
            "seams run dry"
        });
    }
    if effects.advance_bonus != 0 {
        lines.push(if effects.advance_bonus > 0 {
            "ground is soft"
        } else {
            "ground is dense"
        });
    }
    if effects.event_chance_multiplier != 0.0 {
        lines.push(if effects.event_chance_multiplier > 0.0 {
            "the deep stirs"
        } else {
            "the deep is quiet"
        });
    }
    if effects.artifact_multiplier != 0.0 && effects.artifact_multiplier != 1.0 {
        lines.push(if effects.artifact_multiplier > 1.0 {
            "relics surface more often"
        } else {
            "relics are scarce"
        });
    }
    if effects.luminosity_drain_multiplier != 0.0 {
        lines.push("darkness drains lanterns quickly");
    }
    if lines.is_empty() {
        "no notable effect".to_owned()
    } else {
        lines.join(", ")
    }
}

fn dig_item_choices(
    query: &str,
    items: &[cama_app::dig_inventory::InventoryViewItem],
) -> Vec<CommandOptionChoice> {
    items
        .iter()
        .filter(|item| item.name.to_ascii_lowercase().contains(query))
        .take(MAX_AUTOCOMPLETE_CHOICES)
        .map(|item| CommandOptionChoice::String {
            name: item.name.clone(),
            value: item.item_type.clone(),
        })
        .collect()
}

fn dig_relic_choices(
    query: &str,
    panel: &cama_app::dig_gear_runtime::DigGearRuntimePanel,
) -> Vec<CommandOptionChoice> {
    panel
        .relics
        .iter()
        .map(|relic| (artifact_name(&relic.artifact_id), relic.artifact_id.clone()))
        .filter(|(name, _)| name.to_ascii_lowercase().contains(query))
        .take(MAX_AUTOCOMPLETE_CHOICES)
        .map(|(name, value)| CommandOptionChoice::String {
            name: truncate_chars(&name, 100),
            value: truncate_chars(&value, 100),
        })
        .collect()
}

fn dig_buy_choices(
    query: &str,
    shop: &cama_app::dig_gear_runtime::DigGearShop,
) -> Vec<CommandOptionChoice> {
    let mut entries = shop
        .consumables
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                format!("{} ({} JC)", item.name, item.price),
            )
        })
        .chain(shop.pickaxe_upgrades.iter().map(|item| {
            (
                format!("weapon:{}", item.tier),
                format!("{} ({} JC)", item.name, item.price),
            )
        }))
        .chain(shop.gear_for_sale.iter().map(|item| {
            (
                format!("{}:{}", item.slot.as_str(), item.tier),
                format!("{} ({} JC)", item.name, item.price),
            )
        }))
        .filter(|(value, label)| {
            query.is_empty()
                || value.to_ascii_lowercase().contains(query)
                || label.to_ascii_lowercase().contains(query)
        })
        .take(MAX_AUTOCOMPLETE_CHOICES)
        .map(|(value, name)| CommandOptionChoice::String { name, value })
        .collect::<Vec<_>>();
    entries.shrink_to_fit();
    entries
}

fn parse_gear_shop_choice(item: &str) -> Option<(&str, i32)> {
    let (slot, tier) = item.split_once(':')?;
    Some((slot, tier.parse::<i32>().unwrap_or(-1)))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn gear_purchase_name(slot: &str, tier: i32) -> Option<String> {
    let slot = if slot == "pickaxe" { "weapon" } else { slot };
    let slot = cama_domain::dig_gear::GearSlot::parse(slot)?;
    let tier = usize::try_from(tier).ok()?;
    cama_domain::dig_gear::tier_table(slot)
        .and_then(|table| table.get(tier))
        .map(|definition| definition.name.to_owned())
}

fn shop_embed(shop: &cama_app::dig_gear_runtime::DigGearShop) -> InteractionEmbed {
    let mut embed = InteractionEmbed::titled("Mining Shop").color(GOLD_COLOR);
    embed = add_split_lines_field(
        embed,
        "Consumables",
        shop.consumables.iter().map(|item| {
            format!(
                "**{}** — {} {JOPACOIN_EMOTE}: {}",
                item.name, item.price, item.description
            )
        }),
    );
    embed = add_split_lines_field(
        embed,
        "Pickaxe Upgrades",
        shop.pickaxe_upgrades.iter().map(|item| {
            format!(
                "**{}** — {} {JOPACOIN_EMOTE} (Depth {}, Prestige {})",
                item.name, item.price, item.depth_required, item.prestige_required,
            )
        }),
    );
    embed = add_split_lines_field(
        embed,
        "Boss Gear",
        shop.gear_for_sale.iter().map(|item| {
            let prestige = if item.prestige_required > 0 {
                format!(", Prestige {}", item.prestige_required)
            } else {
                String::new()
            };
            format!(
                "**{}** — {} {JOPACOIN_EMOTE} (Depth {}{prestige})",
                item.name, item.price, item.depth_required,
            )
        }),
    );
    embed.footer(format!(
        "Your inventory: {}/{} items | Hard Hat/Torch auto-queue; use /dig use <item> for other active items",
        shop.inventory_count, INVENTORY_LIMIT,
    ))
}

fn add_split_lines_field(
    mut embed: InteractionEmbed,
    name: &str,
    lines: impl IntoIterator<Item = String>,
) -> InteractionEmbed {
    const LIMIT: usize = 1_024;
    let mut chunks = Vec::<String>::new();
    let mut current = String::new();
    for line in lines {
        let separator = usize::from(!current.is_empty());
        if !current.is_empty() && current.len() + separator + line.len() > LIMIT {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    for (index, chunk) in chunks.into_iter().enumerate() {
        let field_name = if index == 0 {
            name.to_owned()
        } else {
            format!("{name} (cont.)")
        };
        embed = embed.field(field_name, chunk, false);
    }
    embed
}

fn gear_panel_response(
    panel: &cama_app::dig_gear_runtime::DigGearRuntimePanel,
    owner_id: i64,
    guild_id: i64,
    nonce: &str,
) -> InteractionResponse {
    let repair_cost = panel
        .pieces
        .iter()
        .filter(|piece| piece.durability < piece.max_durability)
        .map(|piece| {
            cama_domain::dig_gear::repair_cost(
                piece.slot,
                piece.tier,
                piece.item_id.as_deref(),
                piece.durability,
                Some(piece.max_durability),
            )
        })
        .sum::<i64>();
    let damaged = panel
        .pieces
        .iter()
        .filter(|piece| piece.durability < piece.max_durability)
        .count();
    let repair_label = if damaged == 0 {
        "Repair All".to_owned()
    } else if repair_cost == 0 {
        "Repair All (Free)".to_owned()
    } else {
        format!("Repair All ({repair_cost} JC)")
    };
    let prefix = format!("dig:gear:{nonce}:{owner_id}:{guild_id}");
    let buttons = vec![
        InteractionButton::new(format!("{prefix}:open:equip:0"), "Equip")
            .style(InteractionButtonStyle::Primary),
        InteractionButton::new(format!("{prefix}:open:unequip:0"), "Unequip")
            .style(InteractionButtonStyle::Secondary),
        InteractionButton::new(format!("{prefix}:open:repair:0"), "Repair")
            .style(InteractionButtonStyle::Success),
        InteractionButton::new(format!("{prefix}:repair_all"), repair_label)
            .style(InteractionButtonStyle::Danger)
            .disabled(damaged == 0),
        InteractionButton::new(format!("{prefix}:open:recycle:0"), "Recycle Relics")
            .style(InteractionButtonStyle::Secondary),
    ];
    InteractionResponse::message("")
        .embed(gear_panel_embed(panel))
        .action_row(InteractionActionRow::buttons(buttons))
}

fn gear_panel_embed(panel: &cama_app::dig_gear_runtime::DigGearRuntimePanel) -> InteractionEmbed {
    let mut embed = InteractionEmbed::titled("Your Loadout").color(0x8B_45_13);
    for slot in [
        cama_domain::dig_gear::GearSlot::Weapon,
        cama_domain::dig_gear::GearSlot::Armor,
        cama_domain::dig_gear::GearSlot::Boots,
        cama_domain::dig_gear::GearSlot::Amulet,
        cama_domain::dig_gear::GearSlot::Ring,
    ] {
        let value = panel
            .pieces
            .iter()
            .find(|piece| piece.slot == slot && piece.equipped)
            .map_or_else(
                || "_— Empty —_".to_owned(),
                |piece| {
                    let mut lines = vec![format!(
                        "**{}** ({}/{})",
                        piece.name, piece.durability, piece.max_durability
                    )];
                    if piece.durability <= 0 {
                        lines.push("**BROKEN — effects disabled until repaired.**".to_owned());
                    }
                    if let Some(effect) = &piece.effect {
                        lines.push(effect.clone());
                    }
                    if piece.durability < piece.max_durability {
                        let cost = cama_domain::dig_gear::repair_cost(
                            piece.slot,
                            piece.tier,
                            piece.item_id.as_deref(),
                            piece.durability,
                            Some(piece.max_durability),
                        );
                        if cost > 0 {
                            lines.push(format!("Repair: {cost} {JOPACOIN_EMOTE}"));
                        }
                    }
                    lines.join("\n")
                },
            );
        embed = embed.field(gear_slot_label(slot), value, false);
    }
    let equipped_relics = panel
        .relics
        .iter()
        .filter(|relic| relic.equipped)
        .collect::<Vec<_>>();
    let relic_value = if equipped_relics.is_empty() {
        "_None equipped_".to_owned()
    } else {
        equipped_relics
            .iter()
            .map(|relic| format!("• {}", artifact_name(&relic.artifact_id)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    embed = embed.field(
        format!(
            "Relics ({}/{} equipped)",
            equipped_relics.len(),
            panel.relic_cap
        ),
        relic_value,
        false,
    );
    let damaged = panel
        .pieces
        .iter()
        .filter(|piece| piece.durability < piece.max_durability)
        .count();
    let mut footer = vec![format!("{} owned", panel.pieces.len())];
    if damaged > 0 {
        footer.push(format!("{damaged} damaged"));
    }
    footer.push("Buy gear via /dig shop".to_owned());
    embed.footer(footer.join(" • "))
}

fn gear_select_response(
    panel: &cama_app::dig_gear_runtime::DigGearRuntimePanel,
    owner_id: i64,
    guild_id: i64,
    nonce: &str,
    mode: &str,
    requested_page: usize,
) -> Option<InteractionResponse> {
    let mut options = Vec::new();
    for piece in &panel.pieces {
        let include = match mode {
            "equip" => !piece.equipped && piece.durability > 0,
            "unequip" => piece.equipped,
            "repair" => piece.durability < piece.max_durability,
            _ => return None,
        };
        if !include {
            continue;
        }
        let mut label = format!(
            "[{}] {} ({}/{})",
            gear_slot_label(piece.slot),
            piece.name,
            piece.durability,
            piece.max_durability
        );
        if mode == "repair" {
            let cost = cama_domain::dig_gear::repair_cost(
                piece.slot,
                piece.tier,
                piece.item_id.as_deref(),
                piece.durability,
                Some(piece.max_durability),
            );
            if cost > 0 {
                label.push_str(&format!(" — {cost} JC"));
            }
        }
        let mut option = InteractionStringSelectOption::new(
            label.chars().take(100).collect::<String>(),
            format!("gear:{}", piece.id),
        );
        if let Some(effect) = &piece.effect {
            option = option.description(effect.chars().take(100).collect::<String>());
        }
        options.push(option);
    }
    if mode != "repair" {
        for relic in &panel.relics {
            let include = if mode == "equip" {
                !relic.equipped
            } else {
                relic.equipped
            };
            if include {
                options.push(InteractionStringSelectOption::new(
                    format!("[Relic] {}", artifact_name(&relic.artifact_id))
                        .chars()
                        .take(100)
                        .collect::<String>(),
                    format!("relic:{}", relic.row_id),
                ));
            }
        }
    }
    if options.is_empty() {
        return None;
    }
    let page_count = options.len().div_ceil(25).max(1);
    let page = requested_page.min(page_count - 1);
    let page_options = options
        .into_iter()
        .skip(page * 25)
        .take(25)
        .collect::<Vec<_>>();
    let verb = match mode {
        "equip" => "Equip",
        "unequip" => "Unequip",
        "repair" => "Repair",
        _ => return None,
    };
    let page_label = if page_count > 1 {
        format!(" · page {}/{}", page + 1, page_count)
    } else {
        String::new()
    };
    let prefix = format!("dig:gear:{nonce}:{owner_id}:{guild_id}");
    let select = InteractionStringSelect::new(
        format!("{prefix}:select:{mode}:{page}"),
        format!("{verb} which piece?{page_label}"),
        page_options,
    );
    let pagination = vec![
        InteractionButton::new(
            format!("{prefix}:page:{mode}:{}", page.saturating_sub(1)),
            "Previous",
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(page == 0),
        InteractionButton::new(
            format!("{prefix}:page:{mode}:{}", (page + 1).min(page_count - 1)),
            "Next",
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(page + 1 >= page_count),
        InteractionButton::new(format!("{prefix}:back"), "Back")
            .style(InteractionButtonStyle::Secondary),
    ];
    Some(
        InteractionResponse::message("")
            .embed(gear_panel_embed(panel))
            .action_row(InteractionActionRow::string_select(select))
            .action_row(InteractionActionRow::buttons(pagination)),
    )
}

fn relic_recycle_response(
    panel: &cama_app::dig_gear_runtime::DigGearRuntimePanel,
    owner_id: i64,
    guild_id: i64,
    nonce: &str,
) -> Option<InteractionResponse> {
    let catalog = cama_app::dig_loot::artifact_catalog();
    let candidates = panel
        .relics
        .iter()
        .filter(|relic| !relic.equipped)
        .filter_map(|relic| {
            catalog
                .iter()
                .find(|artifact| artifact.id == relic.artifact_id)
                .copied()
                .filter(|artifact| {
                    cama_app::dig_relic_recycling::is_ordinary_relic(*artifact)
                        && artifact.rarity != cama_app::dig_loot::Rarity::Legendary
                })
                .map(|artifact| (relic, artifact))
        })
        .collect::<Vec<_>>();
    let rarity_counts = candidates.iter().fold(
        BTreeMap::<cama_app::dig_loot::Rarity, usize>::new(),
        |mut counts, (_, artifact)| {
            *counts.entry(artifact.rarity).or_default() += 1;
            counts
        },
    );
    let options = candidates
        .into_iter()
        .filter(|(_, artifact)| rarity_counts.get(&artifact.rarity).copied().unwrap_or(0) >= 3)
        .take(25)
        .map(|(relic, artifact)| {
            InteractionStringSelectOption::new(
                format!(
                    "[{}] {}",
                    relic_rarity_label(artifact.rarity),
                    artifact.name
                )
                .chars()
                .take(100)
                .collect::<String>(),
                relic.row_id.to_string(),
            )
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return None;
    }
    let prefix = format!("dig:gear:{nonce}:{owner_id}:{guild_id}");
    let mut select = InteractionStringSelect::new(
        format!("{prefix}:recycle"),
        "Choose 3 relics of the same rarity",
        options,
    );
    select.min_values = 3;
    select.max_values = 3;
    Some(
        InteractionResponse::message("")
            .embed(gear_panel_embed(panel))
            .action_row(InteractionActionRow::string_select(select))
            .action_row(InteractionActionRow::buttons(vec![
                InteractionButton::new(format!("{prefix}:back"), "Back")
                    .style(InteractionButtonStyle::Secondary),
            ])),
    )
}

fn relic_recycle_followup(
    outcome: &cama_app::dig_relic_recycling::RecycleRelicsOutcome,
) -> InteractionResponse {
    if !outcome.success {
        return InteractionResponse::message(
            outcome
                .error
                .clone()
                .unwrap_or_else(|| "Recycling failed.".to_owned()),
        )
        .ephemeral();
    }
    InteractionResponse::message(format!(
        "Recycled **3 {}** relics into **{}** ({}).",
        outcome.source_rarity.map_or("", relic_rarity_label),
        outcome.relic_name.unwrap_or("a relic"),
        outcome.output_rarity.map_or("", relic_rarity_label),
    ))
    .ephemeral()
}

const fn relic_rarity_label(rarity: cama_app::dig_loot::Rarity) -> &'static str {
    match rarity {
        cama_app::dig_loot::Rarity::Common => "Common",
        cama_app::dig_loot::Rarity::Uncommon => "Uncommon",
        cama_app::dig_loot::Rarity::Rare => "Rare",
        cama_app::dig_loot::Rarity::Legendary => "Legendary",
    }
}

fn gear_action_followup(
    outcome: &cama_app::dig_gear_runtime::DigGearRuntimeOutcome,
) -> InteractionResponse {
    if !outcome.success {
        return InteractionResponse::message(
            outcome
                .error
                .clone()
                .unwrap_or_else(|| "Gear action failed.".to_owned()),
        )
        .ephemeral();
    }
    let content = if outcome.repaired > 0 {
        format!(
            "Repaired **{}** piece(s) for **{}** {JOPACOIN_EMOTE}.",
            outcome.repaired, outcome.cost
        )
    } else if outcome.cost > 0 {
        format!("Done for **{}** {JOPACOIN_EMOTE}.", outcome.cost)
    } else {
        "Done.".to_owned()
    };
    InteractionResponse::message(content).ephemeral()
}

fn gear_slot_label(slot: cama_domain::dig_gear::GearSlot) -> &'static str {
    match slot {
        cama_domain::dig_gear::GearSlot::Weapon => "Weapon",
        cama_domain::dig_gear::GearSlot::Armor => "Armor",
        cama_domain::dig_gear::GearSlot::Boots => "Boots",
        cama_domain::dig_gear::GearSlot::Amulet => "Amulet",
        cama_domain::dig_gear::GearSlot::Ring => "Ring",
        cama_domain::dig_gear::GearSlot::Relic => "Relic",
    }
}

fn artifact_name(artifact_id: &str) -> String {
    cama_app::dig_loot::artifact_catalog()
        .into_iter()
        .find(|artifact| artifact.id == artifact_id)
        .map_or_else(
            || artifact_id.replace('_', " "),
            |artifact| artifact.name.to_owned(),
        )
}

fn layer_color(layer: cama_app::dig_service::Layer) -> u32 {
    match layer.name {
        "Dirt" => 0x8B_45_13,
        "Stone" => 0x80_80_80,
        "Crystal" => 0x00_CE_D1,
        "Magma" => 0xFF_45_00,
        "Abyss" => 0x2F_00_47,
        "Fungal Depths" => 0x7C_FC_00,
        "Frozen Core" => 0x87_CE_EB,
        "The Hollow" => 0x0D_0D_0D,
        _ => 0x8B_45_13,
    }
}

fn route_choice_from_state(raw: Option<&str>) -> Option<DigRouteChoiceView> {
    let state = parse_route_state(raw)?;
    let offered_routes = state
        .offered
        .iter()
        .filter_map(|route_id| route_by_id(route_id))
        .map(|route| DigRouteOfferView {
            id: route.id.to_owned(),
            name: route.name.to_owned(),
            description: route.description.to_owned(),
            layer: route.layer.map(str::to_owned),
        })
        .collect::<Vec<_>>();
    (offered_routes.len() == state.offered.len()).then_some(DigRouteChoiceView {
        layer: state.layer,
        start_depth: state.start_depth,
        end_depth: state.end_depth,
        offered_routes,
    })
}

fn route_component_id(
    view_nonce: &str,
    owner_id: i64,
    guild_id: i64,
    token: &str,
    route_id: &str,
) -> String {
    format!("{ROUTE_COMPONENT_PREFIX}{view_nonce}:{owner_id}:{guild_id}:{token}:{route_id}")
}

fn parse_route_component(raw: &str) -> Option<(&str, i64, i64, String, String)> {
    let mut parts = raw.split(':');
    let nonce = parts.next()?.trim();
    let owner_id = parts.next()?.parse().ok()?;
    let guild_id = parts.next()?.parse().ok()?;
    let token = parts.next()?.trim();
    let route_id = parts.next()?.trim();
    if nonce.is_empty() || token.is_empty() || route_id.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((
        nonce,
        owner_id,
        guild_id,
        token.to_owned(),
        route_id.to_owned(),
    ))
}

fn route_choice_response(
    choice: &DigRouteChoiceView,
    view_nonce: &str,
    owner_id: i64,
    guild_id: i64,
    token: &str,
    disabled: bool,
) -> InteractionResponse {
    let mut embed = InteractionEmbed::titled(format!("{} Junction", choice.layer))
        .description(format!(
            "The way forward splits after depth **{}**. Choose the passage that will shape your descent toward **{}**.\n\nThis choice remains active until another junction replaces it.",
            choice.start_depth, choice.end_depth
        ))
        .color(cama_app::dig_view_supplements::layer_color(&choice.layer));
    for route in &choice.offered_routes {
        embed = embed.field(&route.name, &route.description, false);
    }
    embed = embed.footer("No rerolls. If this view expires, use /dig go to reopen it.");
    let buttons = choice
        .offered_routes
        .iter()
        .map(|route| {
            InteractionButton::new(
                route_component_id(view_nonce, owner_id, guild_id, token, &route.id),
                &route.name,
            )
            .style(if route.layer.is_none() {
                InteractionButtonStyle::Secondary
            } else {
                InteractionButtonStyle::Primary
            })
            .disabled(disabled)
        })
        .collect();
    InteractionResponse::message("")
        .embed(embed)
        .action_row(InteractionActionRow::buttons(buttons))
}

fn locked_route_response(layer: &str, route: &DigRouteOfferView) -> InteractionResponse {
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(format!("Route Locked: {}", route.name))
            .description(format!(
                "**{}** will guide you through **{}**.\n\n{}",
                route.name, layer, route.description
            ))
            .color(cama_app::dig_view_supplements::layer_color(layer)),
    )
}

fn pickaxe_name(tier: i64) -> &'static str {
    usize::try_from(tier)
        .ok()
        .and_then(|index| PICKAXE_TIERS.get(index))
        .map_or("Unknown Pickaxe", |pickaxe| pickaxe.name)
}

fn python_dig_result_embed(
    result: &DigRuntimeResult,
    title: &str,
    display_name: &str,
    avatar: Option<String>,
    narrative: Option<&str>,
    callback_reference: Option<&str>,
) -> InteractionEmbed {
    let layer = layer_at(result.depth_after);
    let mut embed = InteractionEmbed::titled(title).color(layer_color(layer));
    if let Some(narrative) = narrative.filter(|narrative| !narrative.is_empty()) {
        embed = embed.field("\u{200b}", format!("*{narrative}*"), false);
    }
    if !result.cave_in || result.advance > 0 || result.jc_earned > 0 {
        let mut progress = format!(
            "+{} blocks | +{} {JOPACOIN_EMOTE}",
            result.advance, result.jc_earned
        );
        if result.pet_dig_bonus > 0
            && let Some(pet_name) = result.pet_name.as_deref()
        {
            progress.push_str(&format!(
                "\n🐾 {pet_name} excavated +{} blocks",
                result.pet_dig_bonus
            ));
        }
        if result.bankruptcy_penalty > 0 {
            progress.push_str(&format!(
                "\n−{} {JOPACOIN_EMOTE} withheld while bankrupt",
                result.bankruptcy_penalty
            ));
        }
        if result.vanity_tax > 0 {
            progress.push_str(&format!(
                "\n−{} {JOPACOIN_EMOTE} vanity tax",
                result.vanity_tax
            ));
        }
        embed = embed.field("Progress", progress, false);
    }
    if result.relic_trim_notice {
        embed = embed.field(
            "Relic slots capped",
            "Relics are now capped at **6**. Your extra relics were unequipped and are safe in your inventory — re-pick with `/dig gear`.",
            false,
        );
    }
    if result.cave_in {
        let detail = result
            .cave_in_detail
            .as_deref()
            .and_then(|detail| serde_json::from_str::<serde_json::Value>(detail).ok());
        let block_loss = detail
            .as_ref()
            .and_then(|detail| detail.get("block_loss"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_else(|| result.depth_before.saturating_sub(result.depth_after));
        let jc_lost = detail
            .as_ref()
            .and_then(|detail| detail.get("jc_lost"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default();
        let cave_type = detail
            .as_ref()
            .and_then(|detail| detail.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let message = detail
            .as_ref()
            .and_then(|detail| detail.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mut value = format!("Lost **{block_loss}** blocks");
        if jc_lost > 0 {
            value.push_str(&format!(" and **{jc_lost}** {JOPACOIN_EMOTE}"));
        }
        if !message.is_empty() {
            value.push_str(&format!(". {message}"));
        }
        embed = embed.field(
            if cave_type == "catastrophic" {
                "CATASTROPHIC CAVE-IN!"
            } else {
                "Cave-in!"
            },
            value,
            false,
        );
    }
    if result.milestone_bonus > 0 {
        embed = embed.field(
            "DIG DUG! Milestone!",
            format!("+{} {JOPACOIN_EMOTE}", result.milestone_bonus),
            false,
        );
    }
    if result.streak_bonus > 0 {
        embed = embed.field(
            "Streak Bonus",
            format!("+{} {JOPACOIN_EMOTE}", result.streak_bonus),
            true,
        );
    }
    if let Some(artifact_id) = result.artifact_id.as_deref() {
        let artifact_name = cama_app::dig_loot::artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .map_or_else(
                || artifact_id.replace('_', " "),
                |artifact| artifact.name.to_owned(),
            );
        embed = embed.field("Artifact Found!", format!("**{artifact_name}**"), false);
    }
    if !result.items_used.is_empty() {
        embed = embed.field("Items Used", result.items_used.join(", "), true);
    }
    if result.luminosity_drained > 0 || result.luminosity_after < 100 {
        let luminosity = result.luminosity_after.clamp(0, 100);
        let filled = usize::try_from(luminosity / 10).unwrap_or_default();
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled));
        let level = if luminosity >= 76 {
            "Bright"
        } else if luminosity >= 51 {
            "Dim"
        } else if luminosity >= 26 {
            "Dark"
        } else {
            "Pitch Black"
        };
        let mut value = format!("`[{bar}]` {luminosity}% — {level}");
        if result.luminosity_drained > 0 {
            value.push_str(&format!(" (-{})", result.luminosity_drained));
        }
        embed = embed.field("Luminosity", value, false);
    }
    if let Some(corruption) = result
        .corruption_description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        embed = embed.field("Corruption", corruption, false);
    }
    if let Some(boundary) = result.boss_boundary {
        embed = embed.field(
            "Boss boundary",
            format!("A boss encounter begins at depth {boundary}."),
            false,
        );
    }
    let mut footer = result.tip.clone();
    if !result.mutation_names.is_empty() {
        footer = format!(
            "Mutations: {}{}",
            result.mutation_names.join(", "),
            if footer.is_empty() {
                String::new()
            } else {
                format!(" | {footer}")
            }
        );
    }
    if let Some(callback) = callback_reference.filter(|callback| !callback.is_empty()) {
        footer = if footer.is_empty() {
            callback.to_owned()
        } else {
            format!("{footer} | {callback}")
        };
    }
    if !footer.is_empty() {
        embed = embed.footer(footer);
    }
    if let Some(avatar) = avatar {
        embed = embed.author(display_name, Some(avatar));
    }
    embed
}

fn dig_responses(
    result: &DigRuntimeResult,
    display_name: &str,
    avatar: Option<String>,
    media: &DigMediaRuntime,
    view_nonce: &str,
    event_prompt: Option<&cama_app::dig_event_runtime::DigEventActionPresentation>,
) -> (InteractionResponse, Option<InteractionResponse>) {
    if let Some(error) = &result.error {
        return (
            InteractionResponse::message(error.clone()).ephemeral(),
            None,
        );
    }
    let layer = layer_at(result.depth_after);
    let title = if result.first_dig {
        "Your first tunnel opens".to_owned()
    } else {
        format!("{} — Depth {}", result.tunnel_name, result.depth_after)
    };
    let mut embed = python_dig_result_embed(result, &title, display_name, avatar, None, None);
    let mut attachments = Vec::new();
    if let Some(layer_art) = media.layer_thumbnail(layer.name) {
        embed = embed.thumbnail(format!("attachment://{}", layer_art.filename));
        attachments.push(interaction_attachment(layer_art));
    }
    if let Some(pickaxe_art) = media.pickaxe_art(result.pickaxe_tier) {
        embed = embed.footer_icon(format!("attachment://{}", pickaxe_art.filename));
        attachments.push(interaction_attachment(pickaxe_art));
    }
    if let Some(items_art) = media.compose_items_used(&result.items_used) {
        embed = embed.image(format!("attachment://{}", items_art.filename));
        attachments.push(interaction_attachment(items_art));
    }
    let mut response = InteractionResponse::message("").embed(embed);
    for attachment in attachments {
        response = response.attachment(attachment);
    }
    let event = event_prompt_response(result, media, layer.name, view_nonce, event_prompt);
    (response, event)
}

fn dig_delivery_responses(
    delivery: &DigRuntimeDeliverySnapshot,
    media: &DigMediaRuntime,
    view_nonce: &str,
) -> (InteractionResponse, Option<InteractionResponse>) {
    let render = &delivery.render;
    if render.kind == DigRuntimeRenderKind::First {
        return (
            InteractionResponse::message("").embed(
                InteractionEmbed::titled(render.title.clone())
                    .description(render.description.clone())
                    .color(render.layer_color),
            ),
            None,
        );
    }
    if render.kind == DigRuntimeRenderKind::Boss
        && let Some(boss) = render.boss.as_ref()
    {
        let info = DigBossEncounterInfo {
            boundary: i32::try_from(boss.boundary).unwrap_or(i32::MAX),
            boss_id: boss.boss_id.clone(),
            boss_name: boss.boss_name.clone(),
            dialogue: boss.dialogue.clone(),
            is_pinnacle: boss.is_pinnacle,
            phase: u8::try_from(boss.phase).unwrap_or(1),
            wager_allowed: boss.wager_allowed,
            carried_wager: (boss.carried_wager > 0).then_some(boss.carried_wager),
            carried_risk_tier: None,
            has_scout_lantern: boss.has_scout_lantern,
            luminosity: boss.luminosity,
            encounter_key: boss.encounter_key.clone().unwrap_or_default(),
        };
        return (
            boss_encounter_response(&info, delivery.discord_id, delivery.guild_id, media),
            None,
        );
    }
    let projected = dig_blood_pact_projected_outcome(delivery);
    let callback_reference = match &delivery.flavor {
        DigRuntimeFlavorSnapshot::Applied {
            callback_reference, ..
        } => callback_reference.as_deref(),
        DigRuntimeFlavorSnapshot::Pending | DigRuntimeFlavorSnapshot::Skipped => None,
    };
    let mut embed = python_dig_result_embed(
        &projected,
        &render.title,
        &delivery.context.display_name,
        delivery.context.avatar_url.clone(),
        render.flavor_narrative.as_deref(),
        callback_reference,
    );
    if render.event_kind == Some(cama_app::dig_runtime::DigRuntimeEventKind::Simple)
        && let Some(event) = render.event.as_ref()
    {
        let value = event.ascii_art.as_deref().map_or_else(
            || event.description.clone(),
            |art| format!("```\n{art}\n```\n{}", event.description),
        );
        embed = embed.field("\u{200b}", value, false);
    }
    let mut attachments = Vec::new();
    if let Some(layer_art) = media.layer_thumbnail(&render.layer_media_key) {
        embed = embed.thumbnail(format!("attachment://{}", layer_art.filename));
        attachments.push(interaction_attachment(layer_art));
    }
    if let Some(pickaxe_art) = media.pickaxe_art(render.pickaxe_tier) {
        embed = embed.footer_icon(format!("attachment://{}", pickaxe_art.filename));
        attachments.push(interaction_attachment(pickaxe_art));
    }
    if let Some(items_art) = media.compose_items_used(&render.item_media_keys) {
        embed = embed.image(format!("attachment://{}", items_art.filename));
        attachments.push(interaction_attachment(items_art));
    }
    let mut response = InteractionResponse::message("").embed(embed);
    for attachment in attachments {
        response = response.attachment(attachment);
    }
    (
        response,
        render
            .kind
            .requires_event_part()
            .then(|| {
                render.event.as_ref().map(|event| {
                    dig_delivery_event_response(
                        event,
                        &render.layer_media_key,
                        delivery.action_id,
                        media,
                        view_nonce,
                    )
                })
            })
            .flatten(),
    )
}

fn dig_delivery_flavor_outcome(
    delivery: &DigRuntimeDeliverySnapshot,
    boss: Option<&DigBossEncounterInfo>,
) -> DigFlavorOutcome {
    let outcome = dig_blood_pact_projected_outcome(delivery);
    let event = outcome
        .event_id
        .as_deref()
        .and_then(cama_app::dig_loot::canonical_event)
        .map(|event| EligibleEvent {
            id: event.id.clone(),
            name: event.name.clone(),
            description: event.descriptions.first().cloned().unwrap_or_default(),
        });
    let artifact = outcome.artifact_id.as_deref().map(|artifact_id| {
        let name = cama_app::dig_loot::artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .map_or_else(
                || artifact_id.replace('_', " "),
                |artifact| artifact.name.to_owned(),
            );
        ArtifactFlavorInfo { name }
    });
    DigFlavorOutcome {
        dig_action_id: Some(delivery.action_id),
        success: outcome.success,
        dig_consumed: Some(outcome.success),
        boss_pending: boss.is_some(),
        advance: outcome.advance,
        jc_earned: outcome.jc_earned,
        depth_before: outcome.depth_before,
        depth_after: outcome.depth_after,
        cave_in: outcome.cave_in,
        event,
        boss_encounter: boss.is_some(),
        boss_info: boss.map(|boss| BossFlavorInfo {
            name: boss.boss_name.clone(),
        }),
        artifact,
        ..DigFlavorOutcome::default()
    }
}

fn dig_blood_pact_projected_outcome(
    delivery: &DigRuntimeDeliverySnapshot,
) -> cama_app::dig_runtime::DigRuntimeOutcome {
    let mut outcome = delivery.outcome.clone();
    if let DigRuntimeBloodPactSnapshot::Applied { skimmed } = delivery.blood_pact {
        let skimmed = skimmed.max(0).min(outcome.jc_earned.max(0));
        outcome.jc_earned = outcome.jc_earned.saturating_sub(skimmed);
        outcome.balance_after = outcome.balance_after.saturating_sub(skimmed);
    }
    outcome
}

fn dig_runtime_flavor_snapshot(receipt: FlavorDeliveryReceipt) -> DigRuntimeFlavorSnapshot {
    match receipt.state {
        FlavorDeliveryState::Applied => {
            let (npc_id_or_name, npc_line) = receipt
                .npc_appearance
                .map_or((None, None), |npc| (Some(npc.id_or_name), Some(npc.line)));
            DigRuntimeFlavorSnapshot::Applied {
                narrative: receipt.narrative,
                tone: receipt.tone,
                callback_reference: receipt.callback_reference,
                npc_id_or_name,
                npc_line,
                picked_event_id: receipt.picked_event_id,
                bonus_delta: receipt.bonus.delta,
            }
        }
        FlavorDeliveryState::Skipped => DigRuntimeFlavorSnapshot::Skipped,
    }
}

fn dig_delivery_event_response(
    event: &cama_app::dig_runtime::DigRuntimeEventRenderSnapshot,
    layer_name: &str,
    action_id: i64,
    media: &DigMediaRuntime,
    view_nonce: &str,
) -> InteractionResponse {
    let is_boon = !event.boon_names.is_empty();
    let description = if is_boon {
        format!(
            "{}\n\n{}",
            event.description,
            event
                .boon_names
                .iter()
                .map(|name| format!("**{name}** — "))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        event.description.clone()
    };
    let mut embed = InteractionEmbed::default()
        .description(description)
        .color(if is_boon { PUBLIC_COLOR } else { GOLD_COLOR });
    if let Some(ascii_art) = event.ascii_art.as_deref() {
        embed = embed.field("\u{200b}", format!("```\n{ascii_art}\n```"), false);
    }
    if let Some(hint) = event.reading_the_stone_hint.as_deref() {
        embed = embed.field("\u{200b}", format!("_{hint}_"), false);
    }
    let mut response = InteractionResponse::message("");
    if let Some(attachment) = media.event_art(&event.event_id, layer_name) {
        embed = embed.image(format!("attachment://{}", attachment.filename));
        response = response.attachment(interaction_attachment(attachment));
    }
    let buttons = dig_delivery_event_buttons(event, view_nonce, action_id);
    response = response.embed(embed);
    if buttons.is_empty() {
        response
    } else {
        response.action_row(InteractionActionRow::buttons(buttons))
    }
}

fn dig_delivery_event_buttons(
    event: &cama_app::dig_runtime::DigRuntimeEventRenderSnapshot,
    view_nonce: &str,
    action_id: i64,
) -> Vec<InteractionButton> {
    if !event.boon_names.is_empty() {
        return event
            .boon_names
            .iter()
            .take(5)
            .enumerate()
            .map(|(index, name)| {
                InteractionButton::new(
                    format!("dig:event-action:{view_nonce}:{action_id}:boon_{index}"),
                    name.chars().take(80).collect::<String>(),
                )
                .style(InteractionButtonStyle::Primary)
            })
            .collect();
    }
    let mut buttons = Vec::new();
    if let Some(label) = event.safe_label.as_deref() {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:safe"),
                label,
            )
            .style(InteractionButtonStyle::Success)
            .disabled(event.safe_disabled),
        );
    }
    if let Some(label) = event.risky_label.as_deref() {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:risky"),
                label,
            )
            .style(InteractionButtonStyle::Primary),
        );
    }
    if let Some(label) = event.desperate_label.as_deref() {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:desperate"),
                label,
            )
            .style(InteractionButtonStyle::Danger),
        );
    }
    buttons
}

fn event_prompt_response(
    result: &DigRuntimeResult,
    media: &DigMediaRuntime,
    layer_name: &str,
    view_nonce: &str,
    prompt: Option<&cama_app::dig_event_runtime::DigEventActionPresentation>,
) -> Option<InteractionResponse> {
    if let (Some(event_id), Some(action_id)) = (result.event_id.as_deref(), result.action_id)
        && let Some(event) = cama_app::dig_loot::canonical_event(event_id)
    {
        let description = event
            .descriptions
            .get(usize::try_from(action_id).unwrap_or_default() % event.descriptions.len().max(1));
        let flavor = description
            .cloned()
            .unwrap_or_else(|| "Something happens...".to_owned());
        let is_boon = !event.boon_options.is_empty();
        let mut embed = InteractionEmbed::default()
            .description(if is_boon {
                format!(
                    "{flavor}\n\n{}",
                    event
                        .boon_options
                        .iter()
                        .map(|boon| format!("**{}** — ", boon.name))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else {
                flavor
            })
            .color(if is_boon { PUBLIC_COLOR } else { GOLD_COLOR });
        if let Some(ascii_art) = event.ascii_art.as_deref() {
            embed = embed.field("\u{200b}", format!("```\n{ascii_art}\n```"), false);
        }
        if let Some(hint) = prompt
            .filter(|prompt| prompt.event.event_id == event_id)
            .and_then(|prompt| prompt.reading_the_stone_hint.as_deref())
        {
            embed = embed.field("\u{200b}", format!("_{hint}_"), false);
        }
        let mut event_art = None;
        if let Some(attachment) = media.event_art(event_id, layer_name) {
            embed = embed.image(format!("attachment://{}", attachment.filename));
            event_art = Some(interaction_attachment(attachment));
        }
        let safe_disabled = prompt
            .filter(|prompt| prompt.event.event_id == event_id)
            .is_some_and(|prompt| prompt.safe_disabled);
        let buttons = event_action_buttons(event, view_nonce, action_id, safe_disabled, false);
        let mut response = InteractionResponse::message("").embed(embed);
        if let Some(event_art) = event_art {
            response = response.attachment(event_art);
        }
        if !buttons.is_empty() {
            response = response.action_row(InteractionActionRow::buttons(buttons));
        }
        return Some(response);
    }
    None
}

fn event_choice_is_valid(
    prompt: &cama_app::dig_event_runtime::DigEventActionPresentation,
    choice: &str,
) -> bool {
    let Some(event) = cama_app::dig_loot::canonical_event(&prompt.event.event_id) else {
        return false;
    };
    match choice {
        "safe" => event.safe_option.is_some() && !prompt.safe_disabled,
        "risky" => event.risky_option.is_some(),
        "desperate" => event.desperate_option.is_some(),
        choice => choice
            .strip_prefix("boon_")
            .and_then(|index| index.parse::<usize>().ok())
            .is_some_and(|index| index < event.boon_options.len().min(5)),
    }
}

fn locked_event_controls(
    prompt: &cama_app::dig_event_runtime::DigEventActionPresentation,
    view_nonce: &str,
    action_id: i64,
) -> InteractionResponse {
    let buttons = cama_app::dig_loot::canonical_event(&prompt.event.event_id)
        .map(|event| event_action_buttons(event, view_nonce, action_id, prompt.safe_disabled, true))
        .unwrap_or_default();
    InteractionResponse::message("").action_row(InteractionActionRow::buttons(buttons))
}

fn event_action_buttons(
    event: &cama_app::dig_loot::CanonicalEventDef,
    view_nonce: &str,
    action_id: i64,
    safe_disabled: bool,
    lock_all: bool,
) -> Vec<InteractionButton> {
    if !event.boon_options.is_empty() {
        return event
            .boon_options
            .iter()
            .take(5)
            .enumerate()
            .map(|(index, boon)| {
                InteractionButton::new(
                    format!("dig:event-action:{view_nonce}:{action_id}:boon_{index}"),
                    boon.name.chars().take(80).collect::<String>(),
                )
                .style(InteractionButtonStyle::Primary)
                .disabled(lock_all)
            })
            .collect();
    }
    let mut buttons = Vec::new();
    if let Some(option) = &event.safe_option {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:safe"),
                if safe_disabled {
                    "Darkness consumes safety".to_owned()
                } else {
                    option.label.chars().take(80).collect::<String>()
                },
            )
            .style(InteractionButtonStyle::Secondary)
            .disabled(lock_all || safe_disabled),
        );
    }
    if let Some(option) = &event.risky_option {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:risky"),
                option.label.chars().take(80).collect::<String>(),
            )
            .style(InteractionButtonStyle::Danger)
            .disabled(lock_all),
        );
    }
    if let Some(option) = &event.desperate_option {
        buttons.push(
            InteractionButton::new(
                format!("dig:event-action:{view_nonce}:{action_id}:desperate"),
                option.label.chars().take(80).collect::<String>(),
            )
            .style(InteractionButtonStyle::Danger)
            .disabled(lock_all),
        );
    }
    buttons
}

fn interaction_attachment(attachment: cama_app::dig_assets::Attachment) -> InteractionAttachment {
    InteractionAttachment::bytes(attachment.filename, attachment.bytes)
}

fn boss_encounter_response(
    info: &DigBossEncounterInfo,
    owner_id: i64,
    guild_id: i64,
    media: &DigMediaRuntime,
) -> InteractionResponse {
    boss_encounter_response_with_roll(info, owner_id, guild_id, media, None)
}

fn boss_encounter_response_with_roll(
    info: &DigBossEncounterInfo,
    owner_id: i64,
    guild_id: i64,
    media: &DigMediaRuntime,
    forced_roll: Option<f64>,
) -> InteractionResponse {
    let normal = cama_app::boss_encounter_view_guard::BossInfo {
        boss_id: info.boss_id.clone(),
        name: info.boss_name.clone(),
        dialogue: info.dialogue.clone(),
        is_pinnacle: info.is_pinnacle,
        phase: info.phase,
        wager_allowed: info.wager_allowed,
        encounter_key: info.encounter_key.clone(),
        carried_wager: info.carried_wager,
    };
    let secret = pinnacle_by_id(&info.boss_id).and_then(|boss| {
        let phase = boss.phases.get(usize::from(info.phase.saturating_sub(1)))?;
        phase.secret_title.map(|title| {
            let dialogue = if info.boss_id == "forgotten_king" && info.phase == 3 {
                vec!["A king's last room opens behind the throne.".to_owned()]
            } else {
                phase
                    .secret_dialogue
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect()
            };
            cama_app::boss_encounter_view_guard::SecretPhaseDefinition {
                title: title.to_owned(),
                dialogue,
            }
        })
    });
    let seed = format!(
        "{owner_id}:{}:{}:{}",
        info.boss_id, info.phase, info.encounter_key
    );
    let presentation = cama_app::boss_encounter_view_guard::resolve_encounter_presentation(
        &normal,
        info.phase,
        &seed,
        PINNACLE_SECRET_PHASE_CHANCE,
        secret.as_ref(),
        forced_roll,
    );
    let layer_name = layer_at(i64::from(info.boundary)).name;
    let art = if info.is_pinnacle {
        media.pinnacle_phase_art(&info.boss_id, info.phase, layer_name, presentation.secret)
    } else {
        media.boss_art(
            BossIdentity::Id(&info.boss_id),
            BossScene::Encounter,
            layer_name,
        )
    };
    let mut embed = InteractionEmbed::titled(format!("Boss Encountered: {}!", presentation.name))
        .description(presentation.dialogue)
        .color(ERROR_COLOR);
    if let Some(wager) = info.carried_wager.filter(|wager| *wager > 0) {
        embed = embed.field(
            "Carried Wager",
            format!(
                "**{}** {JOPACOIN_EMOTE} is already riding on this phase.",
                format_jc_amount(wager)
            ),
            false,
        );
    }
    if let Some(display) = luminosity_combat_display(info.luminosity) {
        embed = embed.field("\u{200b}", display, false);
    }
    let fight_mode = if info.carried_wager.is_some() && info.carried_risk_tier.is_some() {
        "carried"
    } else if info.wager_allowed {
        "wager"
    } else {
        "risk"
    };
    let mut response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new(
                format!("dig:boss:fight:{fight_mode}:{owner_id}:{guild_id}"),
                "Fight",
            )
            .emoji("⚔️")
            .style(InteractionButtonStyle::Danger),
            InteractionButton::new(format!("dig:boss:retreat:{owner_id}:{guild_id}"), "Retreat")
                .emoji("🏃")
                .style(InteractionButtonStyle::Secondary),
            InteractionButton::new(format!("dig:boss:scout:{owner_id}:{guild_id}"), "Scout")
                .emoji("🔦")
                .style(InteractionButtonStyle::Primary)
                .disabled(!info.has_scout_lantern),
            InteractionButton::new(format!("dig:boss:cheer:{owner_id}:{guild_id}"), "Cheer")
                .emoji("📣")
                .style(InteractionButtonStyle::Success),
        ]));
    if let Some(art) = art {
        embed = embed.image(format!("attachment://{}", art.filename));
        response = response.attachment(interaction_attachment(art));
    }
    response.embed(embed)
}

fn luminosity_combat_display(luminosity: i64) -> Option<String> {
    let luminosity = luminosity.clamp(0, 100) as i32;
    let (hit_offset, boss_damage) = luminosity_combat_penalty(luminosity);
    if hit_offset == 0.0 && boss_damage == 0 {
        return None;
    }
    let level = match luminosity {
        76.. => "Bright",
        26..=75 => "Dim",
        1..=25 => "Dark",
        _ => "Pitch Black",
    };
    let mut details = vec![format!("{}% hit", (hit_offset * 100.0) as i32)];
    if boss_damage != 0 {
        details.push(format!("+{boss_damage} boss dmg"));
    }
    Some(format!(
        "Luminosity: **{level} ({luminosity})** — {}",
        details.join(", ")
    ))
}

fn paused_boss_response(
    paused: &PausedBossDuel,
    owner_id: i64,
    guild_id: i64,
) -> InteractionResponse {
    let buttons = paused
        .pending_prompt
        .options
        .iter()
        .map(|option| {
            InteractionButton::new(
                format!(
                    "dig:boss:duel:{owner_id}:{guild_id}:{}",
                    option.option_index
                ),
                option.label.chars().take(80).collect::<String>(),
            )
            .style(
                if option.option_index == paused.pending_prompt.safe_option_index {
                    InteractionButtonStyle::Primary
                } else {
                    InteractionButtonStyle::Secondary
                },
            )
        })
        .collect();
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled(&paused.pending_prompt.prompt_title)
                .description(&paused.pending_prompt.prompt_description)
                .color(GOLD_COLOR),
        )
        .action_row(InteractionActionRow::buttons(buttons))
}

fn regular_boss_result_response(
    result: &ResolvedFight,
    gear_drop: Option<&DigBossGearDrop>,
    prestige_relic_drop: Option<&DigBossPrestigeRelicDrop>,
    broken_gear: &[DigBossBrokenGear],
    media: &DigMediaRuntime,
) -> InteractionResponse {
    let mut embed = regular_boss_result_embed(result, gear_drop, prestige_relic_drop, broken_gear);
    let layer_name = layer_at(i64::from(result.boundary)).name;
    let art = media.boss_art(
        BossIdentity::Id(&result.boss_id),
        if result.won {
            BossScene::Victory
        } else {
            BossScene::Defeat
        },
        layer_name,
    );
    let mut response = InteractionResponse::message("");
    if let Some(art) = art {
        embed = embed.image(format!("attachment://{}", art.filename));
        response = response.attachment(interaction_attachment(art));
    }
    response.embed(embed)
}

fn regular_boss_result_embed(
    result: &ResolvedFight,
    gear_drop: Option<&DigBossGearDrop>,
    prestige_relic_drop: Option<&DigBossPrestigeRelicDrop>,
    broken_gear: &[DigBossBrokenGear],
) -> InteractionEmbed {
    let boss_name = cama_app::dig_bosses::boss_by_id(&result.boss_id)
        .map_or(result.boss_id.as_str(), |boss| boss.name);
    let phase_cleared = result.won && result.phase_transition.is_some();
    let description = if phase_cleared {
        format!("You broke **{boss_name}** — it staggers...")
    } else if result.won {
        format!(
            "Victory! You defeated **{boss_name}** and won **{:+}** {JOPACOIN_EMOTE} profit!",
            result.jc_delta
        )
    } else {
        format!(
            "Defeat! **{boss_name}** overpowered you. You lost **{}** {JOPACOIN_EMOTE} and were knocked back {} blocks.",
            result.jc_delta.unsigned_abs(),
            result.boundary.saturating_sub(result.new_depth)
        )
    };
    let mut embed = InteractionEmbed::titled(if phase_cleared {
        "Phase Cleared"
    } else {
        "Boss Fight Result"
    })
    .description(description)
    .color(if result.won { 0x00_FF_00 } else { ERROR_COLOR })
    .field(
        "Details",
        format!(
            "Pre-fight win chance: {}%",
            (result.win_chance * 100.0) as i32
        ),
        false,
    );
    if let Some(assist) = result
        .round_log
        .iter()
        .find_map(|round| round.pet_assist.as_ref())
    {
        let damage = result
            .round_log
            .iter()
            .map(|round| round.pet_assist_damage)
            .sum::<i32>();
        embed = embed.field(
            "Pet Assist",
            format!(
                "**{}** ({}) lent a **{}%** damage assist and contributed **{damage} bonus damage**.",
                assist.pet_name, assist.species_name, assist.bonus_percent
            ),
            false,
        );
    }
    if result.won {
        if let Some(drop) = gear_drop {
            embed = embed.field(
                "Boss Drop",
                format!("**{}** ({})", drop.name, drop.slot.as_str()),
                false,
            );
        }
        if let Some(drop) = prestige_relic_drop {
            embed = embed.field("Relic Found", format!("**{}**", drop.name), false);
        }
    }
    if !broken_gear.is_empty() {
        embed = embed.field(
            "Gear Broken",
            broken_gear
                .iter()
                .map(|gear| format!("• **{}**", gear.name))
                .collect::<Vec<_>>()
                .join("\n")
                + "\nThese items stay equipped with their effects disabled until repaired. Use **Repair All** in `/dig gear`.",
            false,
        );
    }
    embed
}

fn pinnacle_boss_result_response(
    result: &DigPinnacleResolved,
    media: &DigMediaRuntime,
) -> InteractionResponse {
    let phase_cleared = result.won && result.next_phase > result.phase;
    let description = if phase_cleared {
        format!("You broke **{}** — it staggers...", result.boss_name)
    } else if result.won {
        format!(
            "Victory! You defeated **{}** and won **{:+}** {JOPACOIN_EMOTE} profit!",
            result.boss_name, result.jc_delta
        )
    } else {
        format!(
            "Defeat! **{}** overpowered you. You lost **{}** {JOPACOIN_EMOTE} and were knocked back {} blocks.",
            result.boss_name,
            result.jc_delta.unsigned_abs(),
            result.knockback
        )
    };
    let mut embed = InteractionEmbed::titled(if phase_cleared {
        "Phase Cleared"
    } else {
        "Boss Fight Result"
    })
    .description(description)
    .color(if result.won { 0x00_FF_00 } else { ERROR_COLOR })
    .field(
        "Details",
        format!(
            "Risk: {:?} | Pre-fight win chance: {}%",
            result.risk_tier,
            (result.win_chance * 100.0) as i32
        ),
        false,
    );
    if let Some(relic) = &result.relic_drop {
        embed = embed.field(
            "Pinnacle Relic",
            format!("**{}** (`{}`)", relic.relic.name, relic.relic.artifact_id),
            false,
        );
    }
    let layer_name = layer_at(i64::from(PINNACLE_DEPTH)).name;
    let art = media.pinnacle_phase_art(&result.boss_id, result.phase, layer_name, false);
    let mut response = InteractionResponse::message("");
    if let Some(art) = art {
        embed = embed.image(format!("attachment://{}", art.filename));
        response = response.attachment(interaction_attachment(art));
    }
    response.embed(embed)
}

fn boss_start_response(
    result: &DigBossCallResult<DigBossStartOutcome>,
    owner_id: i64,
    guild_id: i64,
    media: &DigMediaRuntime,
) -> InteractionResponse {
    match &result.outcome {
        DigBossStartOutcome::Paused(paused) => paused_boss_response(paused, owner_id, guild_id),
        DigBossStartOutcome::RegularResolved(resolved) => regular_boss_result_response(
            resolved,
            result.gear_drop.as_ref(),
            result.prestige_relic_drop.as_ref(),
            &result.broken_gear,
            media,
        ),
        DigBossStartOutcome::PinnacleResolved(resolved) => {
            pinnacle_boss_result_response(resolved, media)
        }
    }
}

fn boss_error_response(error: impl Into<String>) -> InteractionResponse {
    InteractionResponse::message("").embed(
        InteractionEmbed::titled("Boss Fight Error")
            .description(error)
            .color(0xFF_A5_00),
    )
}

fn boss_resume_response(
    result: &DigBossCallResult<DigBossResolvedOutcome>,
    media: &DigMediaRuntime,
) -> InteractionResponse {
    match &result.outcome {
        DigBossResolvedOutcome::Regular(resolved) => regular_boss_result_response(
            resolved,
            result.gear_drop.as_ref(),
            result.prestige_relic_drop.as_ref(),
            &result.broken_gear,
            media,
        ),
        DigBossResolvedOutcome::Pinnacle(resolved) => {
            pinnacle_boss_result_response(resolved, media)
        }
    }
}

fn boss_scout_response(result: &DigBossCallResult<DigBossScoutOutcome>) -> InteractionResponse {
    let mut lines = Vec::new();
    let (boss_name, enhanced) = match &result.outcome {
        DigBossScoutOutcome::Regular(scout) => {
            let boss_name = cama_app::dig_bosses::boss_by_id(&scout.boss_id)
                .map_or(scout.boss_id.as_str(), |boss| boss.name)
                .to_owned();
            if let Some(assist) = &scout.pet_assist {
                lines.push(format!(
                    "**{}** ({}) lends a **{}%** damage assist.\n",
                    assist.pet_name, assist.species_name, assist.bonus_percent
                ));
            }
            if scout.echo_applied {
                let killer = scout
                    .echo_killer_id
                    .map_or_else(|| "a guildmate".to_owned(), |killer| format!("<@{killer}>"));
                lines.push(format!(
                    "*Weakened — {killer} killed this boss in the last 24h.*\n"
                ));
            }
            for (name, details) in [
                ("Cautious", &scout.odds.cautious),
                ("Bold", &scout.odds.bold),
                ("Reckless", &scout.odds.reckless),
            ] {
                lines.push(format!(
                    "**{name}** — {}% win ({}% free) | {:.2}x payout",
                    (details.win_chance * 100.0) as i32,
                    (details.free_fight_chance * 100.0) as i32,
                    details.multiplier
                ));
            }
            if scout.enhanced {
                lines.push("\n_Great Lantern reveal_".to_owned());
                if !scout.mechanic_pool.is_empty() {
                    lines
                        .push("**Possible mid-fight mechanics** (one rolls per fight):".to_owned());
                    lines.extend(
                        scout
                            .mechanic_pool
                            .iter()
                            .map(|mechanic| format!("  • _{}_​", mechanic.replace('_', " "))),
                    );
                }
                if let Some(stinger) = &scout.stinger {
                    lines.push(format!(
                        "**On-loss stinger:** `{}` (+{} knockback, +{}m cooldown{})",
                        stinger.id,
                        stinger.extra_knockback,
                        stinger.extended_cooldown_seconds / 60,
                        stinger
                            .cursed_status
                            .map_or_else(String::new, |curse| format!(", curse: `{curse:?}`"))
                    ));
                }
            }
            (boss_name, scout.enhanced)
        }
        DigBossScoutOutcome::Pinnacle(scout) => {
            if let Some(assist) = &scout.pet_assist {
                lines.push(format!(
                    "**{}** ({}) lends a **{}%** damage assist.\n",
                    assist.pet_name, assist.species_name, assist.bonus_percent
                ));
            }
            for (name, details) in [
                ("Cautious", &scout.cautious),
                ("Bold", &scout.bold),
                ("Reckless", &scout.reckless),
            ] {
                lines.push(format!(
                    "**{name}** — {}% win ({}% free) | {:.2}x payout",
                    (details.win_chance * 100.0) as i32,
                    (details.free_fight_chance * 100.0) as i32,
                    details.multiplier
                ));
            }
            let enhanced = !scout.enhanced_mechanic_ids.is_empty();
            if enhanced {
                lines.push("\n_Great Lantern reveal_".to_owned());
                lines.extend(
                    scout
                        .enhanced_mechanic_ids
                        .iter()
                        .map(|mechanic| format!("  • _{}_​", mechanic.replace('_', " "))),
                );
            }
            (scout.boss_name.clone(), enhanced)
        }
    };
    lines.insert(0, format!("**{boss_name}** — Intel Report\n"));
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(if enhanced {
            "Boss Scouted (Great Lantern)"
        } else {
            "Boss Scouted"
        })
        .description(lines.join("\n"))
        .color(GOLD_COLOR),
    )
}

fn event_resolution_response(
    outcome: &cama_app::dig_event_runtime::DigEventRuntimeOutcome,
) -> InteractionResponse {
    if !outcome.success {
        return InteractionResponse::message("").ephemeral().embed(
            InteractionEmbed::titled("Event Failed")
                .description(
                    outcome
                        .error
                        .clone()
                        .unwrap_or_else(|| "Something went wrong.".to_owned()),
                )
                .color(0xFF_44_44),
        );
    }
    let Some(resolution) = outcome.resolution.as_ref() else {
        return InteractionResponse::message("Nothing happened.").ephemeral();
    };
    let boon = resolution.choice.starts_with("boon_");
    let harmful = !resolution.succeeded || resolution.cruel_echoes || resolution.cave_in;
    let mut embed = if boon {
        InteractionEmbed::titled(resolution.event_name.clone())
            .description(resolution.message.clone())
            .color(PUBLIC_COLOR)
    } else {
        InteractionEmbed::default()
            .description(resolution.message.clone())
            .color(if harmful { 0xFF_44_44 } else { 0x00_FF_00 })
    };
    let mut changes = Vec::new();
    if resolution.advance != 0 {
        changes.push(format!(
            "{}{} blocks",
            if resolution.advance > 0 { "+" } else { "" },
            resolution.advance
        ));
    }
    if resolution.jc != 0 {
        changes.push(format!(
            "{}{} {JOPACOIN_EMOTE}",
            if resolution.jc > 0 { "+" } else { "" },
            resolution.jc
        ));
    }
    if resolution.cave_in {
        changes.push("Cave-in triggered!".to_owned());
    }
    if resolution.streak_loss > 0 {
        changes.push(format!(
            "-{} streak {}",
            resolution.streak_loss,
            if resolution.streak_loss == 1 {
                "day"
            } else {
                "days"
            }
        ));
    }
    if !changes.is_empty() {
        embed = embed.field("Outcome", changes.join(" | "), false);
    }
    if let Some(reward) = &resolution.reward {
        let (title, value) = match reward {
            cama_app::dig_loot::CanonicalReward::Gear(item_id) => cama_domain::dig_gear::unique_gear(item_id).map_or_else(
                || ("Gear Drop", format!("**{item_id}**\nStored in your gear inventory — equip it with `/dig gear`.")),
                |gear| {
                    let presentation = cama_app::dig_event_runtime::gear_drop_presentation(
                        gear.name,
                        gear.slot.as_str(),
                        i64::from(gear.max_durability),
                        gear.effect_summary,
                    );
                    ("Gear Drop", presentation.field_value)
                },
            ),
            cama_app::dig_loot::CanonicalReward::Consumable(item_id) => (
                "Supply Found",
                format!(
                    "**{}**",
                    cama_app::dig_loot::consumable(item_id).map_or(item_id.as_str(), |item| item.name)
                ),
            ),
            cama_app::dig_loot::CanonicalReward::Artifact(artifact_id) => {
                let artifact = cama_app::dig_loot::artifact_catalog()
                    .into_iter()
                    .find(|artifact| artifact.id == artifact_id);
                (
                    artifact.map_or("Artifact Found", |artifact| {
                        if artifact.is_relic { "Relic Found" } else { "Curio Found" }
                    }),
                    format!(
                        "**{}**",
                        artifact.map_or(artifact_id.as_str(), |artifact| artifact.name)
                    ),
                )
            }
        };
        embed = embed.field(title, value, false);
    }
    if let Some(curse) = &resolution.curse {
        embed = embed.field(
            format!("Curse: {}", curse.name),
            format!(
                "A hex clings to you for the next {} {}.",
                curse.duration_digs,
                if curse.duration_digs == 1 {
                    "dig"
                } else {
                    "digs"
                }
            ),
            false,
        );
    }
    if let Some(buff) = &resolution.buff {
        embed = embed.field(
            format!("Buff: {}", buff.name),
            format!("Active for {} digs.", buff.duration_digs),
            true,
        );
    }
    if outcome.balance_after < 0 {
        embed = embed.field(
            "In Debt",
            format!(
                "That cost dropped you to {} {JOPACOIN_EMOTE}. You're in the red.",
                outcome.balance_after
            ),
            false,
        );
    }
    if resolution.boss_encounter {
        embed = embed.field(
            "Boss Encountered",
            "A guardian blocks the path. Use `/dig go` to reopen the encounter.",
            false,
        );
    }
    if let Some(chain) = &outcome.chain_event {
        let description = chain
            .descriptions
            .first()
            .cloned()
            .unwrap_or_else(|| "Another event triggers!".to_owned());
        embed = embed.field("\u{200b}", description, false);
    }
    if let Some(splash) = &outcome.splash
        && !splash.victims.is_empty()
    {
        let victims = splash
            .victims
            .iter()
            .map(|(victim_id, amount)| format!("<@{victim_id}>: {amount} {JOPACOIN_EMOTE}"))
            .collect::<Vec<_>>()
            .join("\n");
        let shields = (splash.shielded_count > 0).then(|| {
            format!(
                "\n{} shielded; {} absorbed.",
                splash.shielded_count, splash.absorbed_total
            )
        });
        embed = embed.field(
            "Aftermath",
            format!("{victims}{}", shields.as_deref().unwrap_or_default()),
            false,
        );
    }
    if let Some(modifier) = &outcome.guild_modifier {
        embed = embed.field(
            "Guild Effect",
            format!(
                "`{}` is active for {} seconds.",
                modifier.modifier_id, modifier.duration_seconds
            ),
            false,
        );
    }
    if let Some(finale) = &outcome.quest_finale {
        let detail = match finale {
            cama_app::dig_event_runtime::DigEventQuestFinale::JcAndModifier {
                quest_id,
                net_jc,
                ..
            } => format!("Quest `{quest_id}` complete: +{net_jc} {JOPACOIN_EMOTE}."),
            cama_app::dig_event_runtime::DigEventQuestFinale::Relic {
                quest_id,
                relic_name,
                ..
            } => format!("Quest `{quest_id}` complete: **{relic_name}**."),
        };
        embed = embed.field("Quest Finale", detail, false);
    }
    InteractionResponse::message("")
        .embed(embed)
        .with_user_mentions(Vec::new())
}

fn paid_dig_response(result: &DigRuntimeResult, token: &str) -> InteractionResponse {
    let row = InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("dig:paid:confirm:{token}"), "Confirm")
            .style(InteractionButtonStyle::Success),
        InteractionButton::new(format!("dig:paid:cancel:{token}"), "Cancel")
            .style(InteractionButtonStyle::Secondary),
    ]);
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled("Paid Dig Required")
                .description(format!(
                    "Free dig on cooldown for **{}**.\nContinuing costs **{}** {JOPACOIN_EMOTE}. Proceed?",
                    format_dig_duration(result.cooldown_remaining), result.paid_dig_cost
                ))
                .color(0xFF_A5_00),
        )
        .action_row(row)
}

fn format_dig_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        let minutes = seconds / 60;
        let remainder = seconds % 60;
        return if remainder == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {remainder}s")
        };
    }
    if seconds < 86_400 {
        let hours = seconds / 3_600;
        let minutes = (seconds % 3_600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        };
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {hours}h")
    }
}

fn format_jc_amount(amount: i64) -> String {
    let sign = if amount < 0 { "-" } else { "" };
    let digits = amount.unsigned_abs().to_string();
    let first = digits.len() % 3;
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    if first != 0 {
        formatted.push_str(&digits[..first]);
    }
    for chunk in digits.as_bytes()[first..].chunks(3) {
        if !formatted.is_empty() {
            formatted.push(',');
        }
        formatted.push_str(std::str::from_utf8(chunk).expect("ASCII JC digits"));
    }
    format!("{sign}{formatted}")
}

// -------------------------------------------------------------------------
// SQLite command policy
// -------------------------------------------------------------------------

#[must_use]
fn dig_delivery_nonce(
    delivery: &DigRuntimeDeliverySnapshot,
    part: DigRuntimeDeliveryPart,
) -> String {
    let part = match part {
        DigRuntimeDeliveryPart::Main => 'm',
        DigRuntimeDeliveryPart::Event => 'e',
    };
    // Discord accepts nonce strings up to 25 characters. A u64 interaction
    // id needs at most 16 hexadecimal digits, keeping this stable identity at
    // 25 characters or fewer without hashing/collision risk.
    format!("cama-d:{:x}:{part}", delivery.context.interaction_id)
}

fn interaction_history_matches(
    message: &DigPublicHistoryMessage,
    delivery: &DigRuntimeDeliverySnapshot,
    expected: &InteractionResponse,
) -> bool {
    message.interaction_id == Some(delivery.context.interaction_id)
        && message.content == expected.content
        && message.embed_titles
            == expected
                .embeds
                .iter()
                .map(|embed| embed.title.clone())
                .collect::<Vec<_>>()
        && message.embed_descriptions
            == expected
                .embeds
                .iter()
                .map(|embed| embed.description.clone())
                .collect::<Vec<_>>()
}

fn event_history_matches(
    message: &DigPublicHistoryMessage,
    delivery: &DigEventDeliverySnapshot,
    expected: &InteractionResponse,
) -> bool {
    message.interaction_id == Some(delivery.context.interaction_id)
        && message.content == expected.content
        && message.embed_titles
            == expected
                .embeds
                .iter()
                .map(|embed| embed.title.clone())
                .collect::<Vec<_>>()
        && message.embed_descriptions
            == expected
                .embeds
                .iter()
                .map(|embed| embed.description.clone())
                .collect::<Vec<_>>()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
#[path = "dig_provider/tests.rs"]
mod tests;
