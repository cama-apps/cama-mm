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
use std::sync::{Arc, Mutex};
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
    InteractionActionRow, InteractionAttachment, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionMessageReceipt,
    InteractionModal, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionStringSelect, InteractionStringSelectOption,
    InteractionTextInput, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
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
            return Ok(());
        }
        if let Some(raw) = custom_id.strip_prefix(ROUTE_COMPONENT_PREFIX) {
            return self
                .handle_route_component(raw, user_id, guild_id, responder)
                .await;
        }
        let action = custom_id.strip_prefix(COMPONENT_PREFIX).unwrap_or_default();
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
        if let Some(route) = action.strip_prefix("route:") {
            return self
                .handle_route_component(route, user_id, guild_id, responder)
                .await;
        }
        if let Some(raw) = action.strip_prefix("boss:fight:") {
            let (owner_id, expected_guild) = parse_boss_owner(raw)?;
            if owner_id != user_id || expected_guild != guild_id {
                return respond(
                    &responder,
                    InteractionResponse::message("Only the tunnel owner can fight.").ephemeral(),
                )
                .await;
            }
            let info = self.boss_encounter(user_id, guild_id, unix_now()).await?;
            if let (Some(wager), Some(risk_tier)) = (info.carried_wager, info.carried_risk_tier) {
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
            let mut risk =
                InteractionTextInput::short("risk_tier", "Risk Tier (cautious / bold / reckless)");
            risk.placeholder = Some("bold".to_owned());
            risk.min_length = Some(1);
            risk.max_length = Some(10);
            let (custom_id, title, inputs) = if info.wager_allowed {
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
            return responder
                .show_modal(InteractionModal {
                    custom_id,
                    title: title.to_owned(),
                    inputs,
                })
                .await
                .map_err(|error| error.to_string());
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
        if let Some(gear_action) = action.strip_prefix("gear:") {
            return self
                .handle_gear_component(user_id, guild_id, gear_action, &values, responder)
                .await;
        }
        let _ = interaction_id;
        // Boss and gear views are process-local in Python.
        // A restart therefore yields an explicit, safe recovery response.
        respond(
            &responder,
            InteractionResponse::message(
                "This Dig interaction expired. Use `/dig go` to reopen it.",
            )
            .ephemeral(),
        )
        .await
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
            self.deliver_to_channel(&pending).await?;
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
                .map_err(|error| format!("Dig flavor receipt recovery failed: {error}"))?
                .ok_or_else(|| {
                    "Dig flavor phase remains pending; Discord delivery is blocked".to_owned()
                })
        })
        .await?;
        self.finalize_delivery_snapshot(DigRuntimeFinalizeDelivery {
            action_id,
            source_key: delivery.source_key.clone(),
            flavor: dig_runtime_flavor_snapshot(receipt),
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
    #[cfg(test)]
    fn reseed(&self, seed: u64) {
        *self.random.lock().expect("boss entropy lock") = fastrand::Rng::with_seed(seed);
    }
}

impl EntropyPort for RuntimeBossEntropy {
    fn next_unit(&mut self) -> f64 {
        self.random.lock().expect("boss entropy lock").f64()
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            0
        } else {
            self.random
                .lock()
                .expect("boss entropy lock")
                .usize(..upper_bound)
        }
    }

    fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32 {
        if minimum >= maximum {
            minimum
        } else {
            self.random
                .lock()
                .expect("boss entropy lock")
                .i32(minimum..=maximum)
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
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|error| format!("Dig SQLite task failed: {error}"))?
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
    let mut response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("dig:boss:fight:{owner_id}:{guild_id}"), "Fight")
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
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use cama_app::dig_media_runtime::DigMediaRuntime;
    use cama_app::dig_runtime::{DEFAULT_DIG_ASSET_ROOT, DigRuntimeService};
    use cama_app::dig_runtime::{DigRuntimeConfig, DigRuntimeOutcome};
    use cama_app::service_container::{ServiceContainer, ServiceContainerOptions, VanityMember};
    use cama_db::core_repositories::{NewPlayer, PlayerRepository};
    use cama_db::schema_manager::initialize_or_migrate;
    use rusqlite::{Connection, params};
    use tempfile::{NamedTempFile, tempdir};

    use super::dig_options;
    use super::{
        DigAbandonViewAdmission, DigBonusDispatchPort, DigBossNeonVictory, DigChannelSnapshot,
        DigDiscordPort, DigEventPendingDeliveryQuery, DigPrestigeViewAdmission, DigPublicHistory,
        DigPublicHistoryMessage, DigPublicSendFailure, DigRegistrationProvider,
        DigRuntimeBloodPactSnapshot, JOPACOIN_EMOTE,
    };
    use crate::application_config::ApplicationConfig;
    use crate::gateway_events::{GatewayMember, GuildMemberPageSource, ReadyRecoveryContext};
    use crate::registration::{
        CommandOptionChoice, CommandOptionKind, CommandOptionSpec, InteractionHandler,
        InteractionMessageReceipt, InteractionModal, InteractionOption, InteractionRequest,
        InteractionResponder, InteractionResponse, InteractionResponseError, InteractionValue,
        RegistrationProvider,
    };

    const USER: u64 = 77_001;
    const GUILD: u64 = 77_002;
    const CHANNEL: u64 = 77_003;

    #[test]
    fn prestige_picker_embeds_match_python_presentation() {
        use cama_app::dig_prestige_runtime::{
            DigPrestigePreview, PrestigeMutationChoice, PrestigeMutationPreview, PrestigePerkChoice,
        };

        let mutation_choice = PrestigeMutationChoice {
            id: "bright".to_owned(),
            name: "Bright Vein".to_owned(),
            description: "More treasure, more danger.".to_owned(),
            positive: true,
        };
        let preview = DigPrestigePreview {
            can_prestige: true,
            reason: None,
            current_level: 7,
            target_level: 8,
            run_score: 4_200,
            available_perks: vec!["steady_hands".to_owned()],
            offered_perks: vec![
                PrestigePerkChoice {
                    id: "steady_hands".to_owned(),
                    name: "Steady Hands".to_owned(),
                },
                PrestigePerkChoice {
                    id: "deep_pockets".to_owned(),
                    name: "Deep Pockets".to_owned(),
                },
            ],
            mutation: Some(PrestigeMutationPreview {
                forced: PrestigeMutationChoice {
                    id: "brittle".to_owned(),
                    name: "Brittle Stone".to_owned(),
                    description: "Cave-ins strike harder.".to_owned(),
                    positive: false,
                },
                choices: vec![mutation_choice],
            }),
            ascension_unlock: None,
        };

        let mutation = super::prestige_mutation_response(&preview, "token");
        let mutation_embed = &mutation.embeds[0];
        assert_eq!(mutation_embed.title.as_deref(), Some("Prestige to P8?"));
        assert!(mutation_embed.fields.iter().any(|field| {
            field.name == "Choose a Mutation"
                && field.value == "**Bright Vein** — More treasure, more danger."
        }));

        let initial_perks = super::prestige_perk_response(&preview, "token", None);
        let initial_embed = &initial_perks.embeds[0];
        assert_eq!(initial_embed.title.as_deref(), Some("Prestige to P8?"));
        assert!(initial_embed.fields.iter().any(|field| {
            field.name == "Choose a Perk" && field.value == "**Steady Hands**\n**Deep Pockets**"
        }));

        let post_mutation = super::prestige_perk_response(&preview, "token", Some("bright"));
        let post_mutation_embed = &post_mutation.embeds[0];
        assert_eq!(
            post_mutation_embed.title.as_deref(),
            Some("Choose a Prestige Perk")
        );
        assert_eq!(
            post_mutation_embed.description.as_deref(),
            Some("**Steady Hands**\n**Deep Pockets**")
        );
        assert!(post_mutation_embed.fields.is_empty());
    }

    fn persistent_vanity_tax(
        database_path: impl AsRef<std::path::Path>,
    ) -> Arc<cama_app::service_container::PersistentVanityTaxService> {
        persistent_vanity_tax_with_rate(database_path, 0.10)
    }

    fn persistent_vanity_tax_with_rate(
        database_path: impl AsRef<std::path::Path>,
        rate: f64,
    ) -> Arc<cama_app::service_container::PersistentVanityTaxService> {
        let options = ServiceContainerOptions {
            vanity_tax_rate: rate,
            ..ServiceContainerOptions::default()
        };
        let mut container = ServiceContainer::new(database_path, options);
        container.initialize();
        Arc::clone(
            &container
                .components()
                .expect("vanity-tax service components")
                .vanity_tax_service,
        )
    }

    #[test]
    fn production_constructor_owns_media_and_fails_closed_on_schema_admission() {
        let admitted = NamedTempFile::new().expect("admitted provider database");
        initialize_or_migrate(admitted.path()).expect("admit canonical schema");
        let vanity_tax = persistent_vanity_tax(admitted.path());
        let provider = DigRegistrationProvider::production(
            admitted.path(),
            &config(),
            Arc::clone(&vanity_tax),
            Arc::new(TestDiscord::default()),
            None,
            None,
            Arc::new(RecordingBonusDispatcher::default()),
        )
        .expect("production constructor admits canonical schema");
        let mut registry = crate::registration::RegistryBuilder::default();
        provider
            .register(&mut registry)
            .expect("production provider registers");

        let missing_directory = tempdir().expect("missing schema directory");
        let result = DigRegistrationProvider::production(
            missing_directory.path().join("not-created.db"),
            &config(),
            vanity_tax,
            Arc::new(TestDiscord::default()),
            None,
            None,
            Arc::new(RecordingBonusDispatcher::default()),
        );
        assert!(matches!(
            result,
            Err(super::DigProviderBuildError::Database(message))
                if message.contains("does not exist") || message.contains("schema")
        ));
    }

    #[test]
    fn boss_reminder_predicate_requires_a_committed_resolution() {
        let pending = cama_app::dig_boss_runtime::DigBossRuntimeResult {
            outcome: (),
            action_id: None,
            abandoned_action_id: None,
            abandoned_wager_forfeit: 0,
            abandoned_gear_wear: Default::default(),
            gear_drop: None,
            prestige_relic_drop: None,
            broken_gear: Vec::new(),
            warnings: Vec::new(),
            notices: Vec::new(),
        };
        assert!(!super::boss_resume_is_resolved(&pending));

        let committed = cama_app::dig_boss_runtime::DigBossRuntimeResult {
            action_id: Some(44),
            ..pending
        };
        assert!(super::boss_resume_is_resolved(&committed));
    }

    fn boss_call_result<T>(
        outcome: T,
        action_id: Option<i64>,
    ) -> cama_app::dig_boss_runtime::DigBossRuntimeResult<T> {
        cama_app::dig_boss_runtime::DigBossRuntimeResult {
            outcome,
            action_id,
            abandoned_action_id: None,
            abandoned_wager_forfeit: 0,
            abandoned_gear_wear: Default::default(),
            gear_drop: None,
            prestige_relic_drop: None,
            broken_gear: Vec::new(),
            warnings: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn pinnacle_boss_projection_fixture(
        won: bool,
        phase: u8,
        next_phase: u8,
    ) -> cama_app::dig_boss_runtime::DigPinnacleResolved {
        cama_app::dig_boss_runtime::DigPinnacleResolved {
            won,
            boss_id: "forgotten_king".to_owned(),
            boss_name: "The Crowned Hunger".to_owned(),
            phase,
            next_phase,
            risk_tier: cama_app::boss_duel::RiskTier::Bold,
            wager: 10,
            win_chance: 0.60,
            jc_delta: if won { 500 } else { -10 },
            payout: if won { 500 } else { 0 },
            wager_payout: if won { 500 } else { 0 },
            gross_jc: 500,
            scaled_base_jc: 500,
            reward_multiplier: 1.0,
            gross_payout: if won { 500 } else { 0 },
            bankruptcy_penalty: 0,
            vanity_tax: 0,
            new_depth: if won { 350 } else { 340 },
            boss_hp_remaining: if won { 0 } else { 50 },
            boss_hp_max: 100,
            knockback: if won { 0 } else { 10 },
            round_log: Vec::new(),
            gear_wear: Default::default(),
            phase_event_id: None,
            relic_drop: None,
            rescue_line_used: false,
            warding_salts_blocked: false,
        }
    }

    #[test]
    fn boss_neon_start_and_resume_emit_only_terminal_regular_wins() {
        let terminal = regular_boss_projection_fixture(true);
        let start = super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                terminal.clone(),
            )),
            Some(101),
        ))
        .expect("terminal start victory");
        assert_eq!(start.boss_name, "Grothak the Unbreakable");
        assert_eq!(start.boundary, 100);
        assert_eq!(start.jc_delta, terminal.jc_delta);

        let resume = super::boss_resume_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(terminal)),
            Some(102),
        ));
        assert!(
            resume.is_some(),
            "legacy and namespaced resume use the same gate"
        );

        let mut phase_only = regular_boss_projection_fixture(true);
        phase_only.phase_transition = Some(cama_app::dig_bosses::BossStatus::PhaseOneDefeated);
        assert!(
            super::boss_start_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                    phase_only.clone(),
                )),
                Some(103),
            ))
            .is_none()
        );
        assert!(
            super::boss_resume_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(phase_only)),
                Some(104),
            ))
            .is_none()
        );

        let loss = regular_boss_projection_fixture(false);
        assert!(
            super::boss_start_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(loss)),
                Some(105),
            ))
            .is_none()
        );
        assert!(
            super::boss_resume_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(
                    regular_boss_projection_fixture(false),
                )),
                Some(106),
            ))
            .is_none()
        );
        assert!(
            super::boss_start_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                    regular_boss_projection_fixture(true),
                )),
                None,
            ))
            .is_none(),
            "uncommitted outcomes cannot emit"
        );
    }

    #[test]
    fn boss_neon_prestige_relic_does_not_masquerade_as_trophy_boost() {
        let mut result = boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                regular_boss_projection_fixture(true),
            )),
            Some(107),
        );
        result.prestige_relic_drop = Some(cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
            database_id: 7,
            artifact_id: "pinnacle_relic".to_owned(),
            name: "Pinnacle Relic".to_owned(),
            rarity: "legendary".to_owned(),
        });
        let victory = super::boss_start_neon_victory(&result).expect("terminal victory");
        assert!(!victory.trophy_relic_drop);
    }

    #[test]
    fn boss_neon_pinnacle_gate_selects_terminal_mode_only() {
        let terminal = pinnacle_boss_projection_fixture(true, 3, 0);
        let start = super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::PinnacleResolved(Box::new(
                terminal.clone(),
            )),
            Some(201),
        ))
        .expect("terminal Pinnacle start victory");
        assert_eq!(
            start.boundary,
            i64::from(cama_app::dig_bosses::PINNACLE_DEPTH)
        );
        assert_eq!(start.boss_name, "The Crowned Hunger");

        let resume = super::boss_resume_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossResolvedOutcome::Pinnacle(Box::new(terminal)),
            Some(202),
        ));
        assert!(
            resume.is_some(),
            "terminal Pinnacle resume uses pinnacle(false)"
        );

        assert!(
            super::boss_start_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossStartOutcome::PinnacleResolved(Box::new(
                    pinnacle_boss_projection_fixture(true, 2, 3),
                )),
                Some(203),
            ))
            .is_none(),
            "phase-only Pinnacle progress must not emit"
        );
        assert!(
            super::boss_resume_neon_victory(&boss_call_result(
                cama_app::dig_boss_runtime::DigBossResolvedOutcome::Pinnacle(Box::new(
                    pinnacle_boss_projection_fixture(false, 3, 3),
                )),
                Some(204),
            ))
            .is_none(),
            "Pinnacle losses must not emit"
        );
    }

    // tests/test_dig_reminder_command.py::test_run_dig_reuses_registration_check_and_embedded_notice
    #[tokio::test]
    async fn run_dig_returns_the_registered_typed_execution_and_delivery_notice() {
        let (_database, provider, _discord) = fixture();
        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                super::unix_now(),
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0x1234,
                    CHANNEL as i64,
                    "Dig Test Miner",
                    None,
                ),
            )
            .await
            .expect("registered dig execution");
        assert!(execution.outcome.success);
        let delivery = execution.delivery.expect("immutable delivery notice");
        assert_eq!(delivery.context.channel_id, CHANNEL as i64);
        assert_eq!(delivery.context.display_name, "Dig Test Miner");
        assert!(!delivery.render.description.is_empty());
    }

    #[tokio::test]
    async fn production_runtime_injects_shared_vanity_tax_into_live_dig() {
        let (database, base, discord) = fixture();
        let now = 1_900_210_029;
        let connection = Connection::open(database.path()).expect("vanity provider database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,luminosity,
                  boss_progress)
                 VALUES (?1,?2,276,276,1,?3,100,?4)",
                params![
                    USER as i64,
                    GUILD as i64,
                    now - 7_200,
                    r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"},"100":{"status":"defeated"},"150":{"status":"defeated"},"200":{"status":"defeated"},"275":{"status":"defeated"}}"#,
                ],
            )
            .expect("normal Dig tunnel");
        let game_date = cama_domain::game_date::game_date_for_timestamp(now as f64)
            .expect("vanity-tax game date");
        connection
            .execute(
                "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES (?1,?2,'The Hollow','mineral_vein'),
                        (?1,?2,'Dirt','earthworm_migration')",
                params![GUILD as i64, game_date],
            )
            .expect("deterministic vanity-tax weather");
        drop(connection);
        let vanity_tax = persistent_vanity_tax_with_rate(database.path(), 1.0);
        vanity_tax
            .refresh_guild(
                GUILD as i64,
                &[VanityMember {
                    discord_id: USER as i64,
                    nickname: None,
                }],
            )
            .expect("refresh taxable Dig member");
        let provider = DigRegistrationProvider::with_media_ai_and_vanity(
            database.path(),
            &config(),
            discord,
            None,
            Arc::clone(&base.handler.state.media),
            None,
            Some(vanity_tax),
        )
        .expect("provider with shared vanity tax");
        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                now,
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0xdec0_de01,
                    CHANNEL as i64,
                    "Taxed Dig Miner",
                    None,
                ),
            )
            .await
            .expect("taxed normal Dig");
        assert!(execution.outcome.success);
        assert!(!execution.outcome.cave_in);
        assert!(
            execution.outcome.vanity_tax > 0,
            "production Dig outcome was not taxed: {:?}",
            execution.outcome
        );
        let connection = Connection::open(database.path()).expect("inspect vanity-tax ledger");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2 AND source='vanity_tax'",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("provider vanity-tax ledger count"),
            1,
        );
    }

    #[tokio::test]
    async fn delivery_settles_blood_pact_before_flavor_and_projects_net_copy() {
        let (database, provider, _discord) = fixture();
        let now = 1_900_210_029;
        let skimmer = 77_004_i64;
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                skimmer,
                "blood-pact-holder",
                Some(GUILD as i64),
            ))
            .expect("Blood Pact holder");
        let connection = Connection::open(database.path()).expect("Blood Pact provider database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,luminosity,
                  boss_progress)
                 VALUES (?1,?2,76,76,1,?3,100,?4)",
                params![
                    USER as i64,
                    GUILD as i64,
                    now - 7_200,
                    r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                ],
            )
            .expect("Blood Pact normal Dig tunnel");
        connection
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                     triggered,data
                 ) VALUES(?1,?2,'blood_pact',?3,?4,?5,0,?6)",
                params![
                    skimmer,
                    GUILD as i64,
                    USER as i64,
                    now - 1,
                    now + 86_400,
                    serde_json::json!({
                        "skimmed_total": 0,
                        "cap": 100,
                        "skim_rate": 1.0,
                    })
                    .to_string(),
                ],
            )
            .expect("active Blood Pact");
        drop(connection);

        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                now,
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0xdec0_de02,
                    CHANNEL as i64,
                    "Pacted Dig Miner",
                    None,
                ),
            )
            .await
            .expect("committed Blood Pact Dig");
        let gross = execution.outcome.jc_earned;
        assert!(gross > 0);
        let pending = execution.delivery.expect("pending Blood Pact delivery");
        assert_eq!(pending.blood_pact, DigRuntimeBloodPactSnapshot::Pending);

        let settled = provider
            .handler
            .prepare_delivery(&pending)
            .await
            .expect("settle Blood Pact before flavor");
        let skimmed = match settled.blood_pact {
            DigRuntimeBloodPactSnapshot::Applied { skimmed } => skimmed,
            ref other => panic!("expected applied Blood Pact, got {other:?}"),
        };
        assert!(skimmed > 0);
        assert!(settled.flavor.is_terminal());
        assert_eq!(
            settled.outcome.jc_earned, gross,
            "immutable outcome changed"
        );
        let projected = super::dig_blood_pact_projected_outcome(&settled);
        assert_eq!(projected.jc_earned, gross - skimmed);
        assert_eq!(
            projected.balance_after,
            settled.outcome.balance_after - skimmed
        );
        let (response, event) = super::dig_delivery_responses(
            &settled,
            &provider.handler.state.media,
            &provider.handler.state.view_nonce,
        );
        assert!(event.is_none());
        let progress = response.embeds[0]
            .fields
            .iter()
            .find(|field| field.name == "Progress")
            .map(|field| field.value.as_str())
            .expect("Blood Pact delivery progress");
        assert!(progress.contains(&format!("+{} {JOPACOIN_EMOTE}", gross - skimmed)));
        assert!(!progress.contains(&format!("+{gross} {JOPACOIN_EMOTE}")));
        let connection = Connection::open(database.path()).expect("inspect Blood Pact delivery");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM hostile_loss_events
                     WHERE guild_id=?1 AND victim_id=?2",
                    params![GUILD as i64, USER as i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("Blood Pact hostile-loss event count"),
            1,
        );
    }

    // tests/test_dig_reminder_command.py::test_schedule_dig_reminder_is_noop_without_service
    #[tokio::test]
    async fn schedule_dig_reminder_is_a_noop_without_a_reminder_service() {
        let (_database, provider, _discord) = fixture();
        assert!(provider.handler.state.reminder_hooks.is_none());
        provider
            .handler
            .reconcile_dig_reminder(USER as i64, GUILD as i64, super::unix_now())
            .await;
    }

    // tests/test_dig_reminder_command.py::test_reconciliation_failure_does_not_interrupt_dig_and_warns
    #[tokio::test]
    async fn failed_reminder_reconciliation_does_not_interrupt_a_committed_dig() {
        let (database, base, _discord) = fixture();
        let invalid_store = tempdir().expect("invalid reminder store directory");
        let reminder = crate::reminder_provider::tests::test_provider(
            invalid_store.path(),
            Arc::new(crate::reminder_provider::tests::MockDiscord::default()),
            Default::default(),
        );
        let provider = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            Some(reminder.hooks()),
            Arc::clone(&base.handler.state.media),
        );
        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                super::unix_now(),
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0x1235,
                    CHANNEL as i64,
                    "Dig Test Miner",
                    None,
                ),
            )
            .await
            .expect("dig remains successful when reminder storage fails");
        assert!(execution.outcome.success);
        provider
            .handler
            .reconcile_dig_reminder(USER as i64, GUILD as i64, super::unix_now())
            .await;
    }

    // tests/test_dig_reminder_command.py::test_boss_encounter_receives_reminder_callback
    #[tokio::test]
    async fn resolved_boss_reconciles_the_dig_reminder_callback() {
        let (database, base, _discord) = fixture();
        let now = super::unix_now();
        Connection::open(database.path())
            .expect("boss callback database")
            .execute(
                "INSERT INTO tunnels (discord_id,guild_id,last_dig_at)
                 VALUES (?1,?2,?3)",
                rusqlite::params![USER as i64, GUILD as i64, now],
            )
            .expect("seed boss callback tunnel");
        Connection::open(database.path())
            .expect("boss reminder preferences database")
            .execute(
                "INSERT INTO reminder_preferences
                    (discord_id,guild_id,dig_enabled,updated_at)
                 VALUES (?1,?2,1,?3)",
                rusqlite::params![USER as i64, GUILD as i64, now],
            )
            .expect("enable boss callback reminder");
        let reminder = crate::reminder_provider::tests::test_provider(
            database.path(),
            Arc::new(crate::reminder_provider::tests::MockDiscord::default()),
            Default::default(),
        );
        let provider = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            Some(reminder.hooks()),
            Arc::clone(&base.handler.state.media),
        );
        provider
            .handler
            .reconcile_resolved_boss(USER as i64, GUILD as i64, now)
            .await;
        let key = cama_app::reminders::ReminderTaskKey::new(
            cama_app::reminders::UserId::new(USER as i64),
            cama_app::reminders::GuildId::new(GUILD as i64),
            cama_app::reminders::ReminderKind::Dig,
        );
        let task = reminder
            .hooks()
            .test_task_snapshot(key)
            .expect("boss callback reminder snapshot")
            .expect("boss callback schedules reminder");
        assert_eq!(
            task.due_at,
            now + cama_app::dig_service::FREE_DIG_COOLDOWN_SECONDS
        );
    }

    // tests/test_dig_reminder_command.py::test_boss_encounter_embed_surfaces_carried_wager
    #[test]
    fn boss_encounter_embed_surfaces_the_carried_wager() {
        let (_database, provider, _discord) = fixture();
        let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
            boundary: 350,
            boss_id: "forgotten_king".to_owned(),
            boss_name: "The Crowned Hunger".to_owned(),
            dialogue: "The crown burns.".to_owned(),
            is_pinnacle: true,
            phase: 2,
            wager_allowed: true,
            carried_wager: Some(1_500),
            carried_risk_tier: None,
            has_scout_lantern: false,
            luminosity: 100,
            encounter_key: "carried-wager".to_owned(),
        };
        let response = super::boss_encounter_response(
            &info,
            USER as i64,
            GUILD as i64,
            &provider.handler.state.media,
        );
        let field = response.embeds[0]
            .fields
            .iter()
            .find(|field| field.name == "Carried Wager")
            .expect("carried wager field");
        assert_eq!(
            field.value,
            format!("**1,500** {JOPACOIN_EMOTE} is already riding on this phase.")
        );
        assert!(!field.value.contains("**1,500** JC "));
    }

    // tests/test_dig_reminder_command.py::test_resumed_pinnacle_encounter_uses_phase_presentation[2]
    #[test]
    fn resumed_pinnacle_phase_two_uses_the_normal_presentation() {
        let (_database, provider, _discord) = fixture();
        let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
            boundary: 350,
            boss_id: "forgotten_king".to_owned(),
            boss_name: "The Crowned Hunger".to_owned(),
            dialogue: "The crown burns.".to_owned(),
            is_pinnacle: true,
            phase: 2,
            wager_allowed: true,
            carried_wager: None,
            carried_risk_tier: None,
            has_scout_lantern: false,
            luminosity: 100,
            encounter_key: "phase-two".to_owned(),
        };
        let response = super::boss_encounter_response(
            &info,
            USER as i64,
            GUILD as i64,
            &provider.handler.state.media,
        );
        assert_eq!(
            response.embeds[0].title.as_deref(),
            Some("Boss Encountered: The Crowned Hunger!")
        );
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some("The crown burns.")
        );
    }

    // tests/test_dig_reminder_command.py::test_resumed_pinnacle_encounter_uses_phase_presentation[3]
    #[test]
    fn resumed_pinnacle_phase_three_uses_the_secret_presentation() {
        let (_database, provider, _discord) = fixture();
        let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
            boundary: 350,
            boss_id: "forgotten_king".to_owned(),
            boss_name: "The Last Breath of Kings".to_owned(),
            dialogue: "Last breath.".to_owned(),
            is_pinnacle: true,
            phase: 3,
            wager_allowed: true,
            carried_wager: None,
            carried_risk_tier: None,
            has_scout_lantern: false,
            luminosity: 100,
            encounter_key: "phase-three".to_owned(),
        };
        let response = super::boss_encounter_response_with_roll(
            &info,
            USER as i64,
            GUILD as i64,
            &provider.handler.state.media,
            Some(0.0),
        );
        assert_eq!(
            response.embeds[0].title.as_deref(),
            Some("Boss Encountered: The Crown Remembers!")
        );
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some("A king's last room opens behind the throne.")
        );
    }

    fn regular_boss_projection_fixture(won: bool) -> cama_app::boss_multi_tier::ResolvedFight {
        cama_app::boss_multi_tier::ResolvedFight {
            won,
            boss_id: "grothak".to_owned(),
            boundary: 100,
            wager: 10,
            win_chance: 0.60,
            jc_delta: if won { 500 } else { -10 },
            gross_payout: if won { 500 } else { 0 },
            bankruptcy_penalty: 0,
            vanity_tax: 0,
            new_depth: if won { 100 } else { 95 },
            boss_hp_remaining: if won { 0 } else { 50 },
            boss_hp_max: 100,
            extra_knockback: 0,
            extra_cooldown_seconds: 0,
            round_log: Vec::new(),
            gear_wear: cama_app::boss_multi_tier::GearWearResult::default(),
            phase_transition: None,
            boss_preparation: None,
            rescue_line_used: false,
            warding_salts_blocked: false,
        }
    }

    fn boss_projection_field<'a>(
        embed: &'a crate::registration::InteractionEmbed,
        name: &str,
    ) -> Option<&'a crate::registration::InteractionEmbedField> {
        embed.fields.iter().find(|field| field.name == name)
    }

    // tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_no_drop_omits_field
    #[test]
    fn regular_boss_victory_without_drop_omits_both_loud_drop_fields() {
        let resolved = regular_boss_projection_fixture(true);
        let embed = super::regular_boss_result_embed(&resolved, None, None, &[]);

        assert!(boss_projection_field(&embed, "Boss Drop").is_none());
        assert!(boss_projection_field(&embed, "Relic Found").is_none());
    }

    // tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_gear_drop_renders_field
    #[test]
    fn regular_boss_victory_projects_gear_drop_name_and_slot() {
        let resolved = regular_boss_projection_fixture(true);
        let drop = cama_app::dig_boss_runtime::DigBossGearDrop {
            gear_id: 1,
            slot: cama_domain::dig_gear::GearSlot::Weapon,
            tier: 6,
            name: "Void-Touched Pickaxe".to_owned(),
        };
        let embed = super::regular_boss_result_embed(&resolved, Some(&drop), None, &[]);
        let field = boss_projection_field(&embed, "Boss Drop").expect("boss drop field");

        assert!(field.value.contains("Void-Touched Pickaxe"));
        assert!(field.value.contains("weapon"));
    }

    // tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_prestige_relic_drop_renders_field
    #[test]
    fn regular_boss_victory_projects_prestige_relic_name() {
        let resolved = regular_boss_projection_fixture(true);
        let drop = cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
            database_id: 2,
            artifact_id: "echo_stone".to_owned(),
            name: "Echo Stone".to_owned(),
            rarity: "Rare".to_owned(),
        };
        let embed = super::regular_boss_result_embed(&resolved, None, Some(&drop), &[]);
        let field = boss_projection_field(&embed, "Relic Found").expect("relic field");

        assert!(field.value.contains("Echo Stone"));
    }

    // tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_loss_does_not_render_drops
    #[test]
    fn regular_boss_loss_suppresses_loud_drop_fields() {
        let resolved = regular_boss_projection_fixture(false);
        let gear = cama_app::dig_boss_runtime::DigBossGearDrop {
            gear_id: 1,
            slot: cama_domain::dig_gear::GearSlot::Weapon,
            tier: 1,
            name: "Should not appear".to_owned(),
        };
        let relic = cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
            database_id: 2,
            artifact_id: "also_hidden".to_owned(),
            name: "Also Hidden".to_owned(),
            rarity: "Rare".to_owned(),
        };
        let embed = super::regular_boss_result_embed(&resolved, Some(&gear), Some(&relic), &[]);

        assert!(boss_projection_field(&embed, "Boss Drop").is_none());
        assert!(boss_projection_field(&embed, "Relic Found").is_none());
    }

    // tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_broken_gear_renders_repair_notification
    #[test]
    fn regular_boss_broken_gear_projects_name_disabled_effects_and_repair_all() {
        let resolved = regular_boss_projection_fixture(true);
        let broken = [cama_app::dig_boss_runtime::DigBossBrokenGear {
            gear_id: 3,
            name: "Ironclad Armor".to_owned(),
        }];
        let embed = super::regular_boss_result_embed(&resolved, None, None, &broken);
        let field = boss_projection_field(&embed, "Gear Broken").expect("broken gear field");

        assert!(field.value.contains("Ironclad Armor"));
        assert!(field.value.contains("effects disabled"));
        assert!(field.value.contains("Repair All"));
    }

    #[test]
    fn boss_failure_uses_python_error_embed() {
        let response = super::boss_error_response("The guardian refuses the wager.");
        assert!(response.content.is_empty());
        let embed = &response.embeds[0];
        assert_eq!(embed.title.as_deref(), Some("Boss Fight Error"));
        assert_eq!(
            embed.description.as_deref(),
            Some("The guardian refuses the wager.")
        );
        assert_eq!(embed.color, Some(0xFF_A5_00));
    }

    // tests/test_dig_event_messaging.py::test_event_result_embed_surfaces_gear_drop_details
    #[test]
    fn failed_event_uses_python_error_embed() {
        let outcome = cama_app::dig_event_runtime::DigEventRuntimeOutcome {
            success: false,
            error: Some("The tunnel rejects that choice.".to_owned()),
            resolution: None,
            depth_before: 12,
            depth_after: 12,
            balance_after: 50,
            action_id: Some(17),
            reward_row_id: None,
            applied_now: false,
            splash: None,
            guild_modifier: None,
            chain_event: None,
            quest_finale: None,
        };

        let response = super::event_resolution_response(&outcome);
        assert!(response.ephemeral);
        assert!(response.content.is_empty());
        let embed = &response.embeds[0];
        assert_eq!(embed.title.as_deref(), Some("Event Failed"));
        assert_eq!(
            embed.description.as_deref(),
            Some("The tunnel rejects that choice.")
        );
        assert_eq!(embed.color, Some(0xFF_44_44));
    }

    // tests/test_dig_event_messaging.py::test_event_result_embed_surfaces_gear_drop_details
    #[test]
    fn event_result_embed_surfaces_exact_unique_gear_drop_details() {
        let outcome = cama_app::dig_event_runtime::DigEventRuntimeOutcome {
            success: true,
            error: None,
            resolution: Some(cama_app::dig_loot::CanonicalEventResolution {
                event_id: "collapsed_armory".to_owned(),
                event_name: "Collapsed Armory".to_owned(),
                choice: "risky".to_owned(),
                complexity: cama_app::dig_loot::EventComplexity::Choice,
                descriptions: Vec::new(),
                steps: Vec::new(),
                boon_options: Vec::new(),
                ascii_art: None,
                social: false,
                succeeded: true,
                message: "The armory coughs up one last bad idea.".to_owned(),
                advance: 0,
                jc: 4,
                cave_in: false,
                streak_loss: 0,
                streak_days_after: None,
                curse: None,
                persisted_curse: None,
                buff: None,
                black_wax_seal_spent: false,
                active_curse_remaining_after: None,
                curse_cleared: false,
                gear_reward_pool: vec!["glassbreaker_pick".to_owned()],
                consumable_reward_pool: Vec::new(),
                artifact_reward_pool: Vec::new(),
                splash: None,
                guild_modifier_on_success: None,
                quest_id: None,
                quest_step: None,
                next_event_id: None,
                chained_event_id: None,
                reward: Some(cama_app::dig_loot::CanonicalReward::Gear(
                    "glassbreaker_pick".to_owned(),
                )),
                duplicate_gear_reward: false,
                splash_payout_ratio: None,
                economy_gross_jc: 4,
                cruel_echoes: false,
                boss_encounter: false,
                random_plan: cama_app::dig_loot::CanonicalEventRandomPlan::default(),
            }),
            depth_before: 175,
            depth_after: 175,
            balance_after: 504,
            action_id: Some(17),
            reward_row_id: Some(23),
            applied_now: true,
            splash: None,
            guild_modifier: None,
            chain_event: None,
            quest_finale: None,
        };

        let response = super::event_resolution_response(&outcome);
        let field = response.embeds[0]
            .fields
            .iter()
            .find(|field| field.name == "Gear Drop")
            .expect("gear drop field");
        assert!(field.value.contains("Glassbreaker Pick"));
        assert!(field.value.contains("Weapon"));
        assert!(field.value.contains("Durability: 8"));
        assert!(
            field
                .value
                .contains("Diamond dig bonuses; +2 boss damage; -8% hit chance.")
        );
        assert!(field.value.contains("Stored in your gear inventory"));
        assert!(field.value.contains("`/dig gear`"));
    }

    #[test]
    fn rust_deployment_retains_authored_dig_assets_at_the_runtime_root() {
        const DOCKERFILE: &str = include_str!("../../../../Dockerfile.rust");
        const COPY_CONTRACT: &str = "COPY --chown=appuser:appuser assets/dig/ /app/assets/dig/";

        assert_eq!(DEFAULT_DIG_ASSET_ROOT, "/app/assets/dig");
        assert!(
            DOCKERFILE.lines().any(|line| line.trim() == COPY_CONTRACT),
            "Dockerfile.rust must retain the authored Dig tree at {DEFAULT_DIG_ASSET_ROOT}"
        );
    }

    struct TestDiscord {
        public: StdMutex<Vec<InteractionResponse>>,
        temporary: StdMutex<Vec<(i64, Duration, InteractionResponse)>>,
        reactions: StdMutex<Vec<(i64, u64, String)>>,
        public_history: StdMutex<Vec<DigPublicHistoryMessage>>,
        message_channels: StdMutex<BTreeMap<u64, i64>>,
        available_channels: StdMutex<BTreeSet<i64>>,
        accept_then_fail_nonce_send: StdMutex<bool>,
        reject_next_configured_nonce_send: StdMutex<bool>,
        fail_next_history: StdMutex<bool>,
        reject_un_nonnced_public_send: StdMutex<bool>,
        gamba: bool,
        avatar_url: Option<String>,
        lifecycle: Option<Arc<StdMutex<Vec<&'static str>>>>,
    }

    impl Default for TestDiscord {
        fn default() -> Self {
            Self {
                public: StdMutex::new(Vec::new()),
                temporary: StdMutex::new(Vec::new()),
                reactions: StdMutex::new(Vec::new()),
                public_history: StdMutex::new(Vec::new()),
                message_channels: StdMutex::new(BTreeMap::new()),
                available_channels: StdMutex::new(BTreeSet::new()),
                accept_then_fail_nonce_send: StdMutex::new(false),
                reject_next_configured_nonce_send: StdMutex::new(false),
                fail_next_history: StdMutex::new(false),
                reject_un_nonnced_public_send: StdMutex::new(false),
                gamba: true,
                avatar_url: None,
                lifecycle: None,
            }
        }
    }

    impl TestDiscord {
        fn with_channels(channels: impl IntoIterator<Item = i64>) -> Self {
            let discord = Self::default();
            discord
                .available_channels
                .lock()
                .expect("available channels")
                .extend(channels);
            discord
        }

        fn arm_accept_then_fail_nonce_send(&self) {
            *self
                .accept_then_fail_nonce_send
                .lock()
                .expect("nonce send fault") = true;
        }

        fn reject_next_configured_nonce_send(&self) {
            *self
                .reject_next_configured_nonce_send
                .lock()
                .expect("configured nonce send fault") = true;
        }

        fn reject_un_nonnced_public_send(&self) {
            *self
                .reject_un_nonnced_public_send
                .lock()
                .expect("un-nonced send fault") = true;
        }

        fn allow_un_nonnced_public_send(&self) {
            *self
                .reject_un_nonnced_public_send
                .lock()
                .expect("un-nonced send fault") = false;
        }

        fn with_lifecycle(mut self, lifecycle: Arc<StdMutex<Vec<&'static str>>>) -> Self {
            self.lifecycle = Some(lifecycle);
            self
        }
    }

    #[async_trait]
    impl DigDiscordPort for TestDiscord {
        async fn dig_channel(&self, channel_id: i64) -> Result<Option<DigChannelSnapshot>, String> {
            Ok(self
                .available_channels
                .lock()
                .expect("available channels")
                .contains(&channel_id)
                .then_some(DigChannelSnapshot {
                    id: channel_id,
                    guild_id: Some(GUILD as i64),
                    parent_id: (channel_id == CHANNEL as i64).then_some(CHANNEL as i64 + 1),
                    can_send: true,
                    is_text: true,
                }))
        }

        async fn dig_channel_is_gamba(
            &self,
            _guild_id: i64,
            _channel_id: i64,
        ) -> Result<bool, String> {
            Ok(self.gamba)
        }

        async fn dig_user_avatar_url(
            &self,
            _guild_id: i64,
            _user_id: i64,
        ) -> Result<Option<String>, String> {
            Ok(self.avatar_url.clone())
        }

        async fn dig_send_public(
            &self,
            _channel_id: i64,
            response: InteractionResponse,
        ) -> Result<(), String> {
            if *self
                .reject_un_nonnced_public_send
                .lock()
                .expect("un-nonced send fault")
            {
                return Err("test forbids un-nonced public sends".to_owned());
            }
            self.public.lock().expect("public responses").push(response);
            Ok(())
        }

        async fn dig_send_temporary(
            &self,
            channel_id: i64,
            response: InteractionResponse,
            delete_after: Duration,
        ) -> Result<(), String> {
            let result = self.dig_send_public(channel_id, response.clone()).await;
            if result.is_ok() {
                if let Some(lifecycle) = &self.lifecycle {
                    lifecycle.lock().expect("lifecycle log").push("temporary");
                }
                self.temporary.lock().expect("temporary responses").push((
                    channel_id,
                    delete_after,
                    response,
                ));
            }
            result
        }

        async fn dig_send_public_once(
            &self,
            channel_id: i64,
            response: InteractionResponse,
            nonce: &str,
        ) -> Result<InteractionMessageReceipt, DigPublicSendFailure> {
            const BOT_USER_ID: u64 = 8_008;
            let reject_configured = channel_id == CHANNEL as i64 + 1
                && std::mem::replace(
                    &mut *self
                        .reject_next_configured_nonce_send
                        .lock()
                        .expect("configured nonce send fault"),
                    false,
                );
            if reject_configured {
                return Err(DigPublicSendFailure::rejected(
                    "test configured channel rejected the send",
                ));
            }
            if let Some(existing) = self
                .public_history
                .lock()
                .expect("public history")
                .iter()
                .find(|message| {
                    message.nonce.as_deref() == Some(nonce)
                        && self
                            .message_channels
                            .lock()
                            .expect("message channels")
                            .get(&message.message_id)
                            .copied()
                            .is_none_or(|message_channel| message_channel == channel_id)
                })
                .cloned()
            {
                return Ok(InteractionMessageReceipt {
                    message_id: existing.message_id,
                    channel_id: u64::try_from(channel_id)
                        .map_err(|_| DigPublicSendFailure::rejected("negative test channel"))?,
                    delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
                });
            }
            let accept_then_fail = {
                let mut armed = self
                    .accept_then_fail_nonce_send
                    .lock()
                    .expect("nonce send fault");
                let armed_now = *armed;
                *armed = false;
                armed_now
            };
            self.public.lock().expect("public responses").push(response);
            let mut history = self.public_history.lock().expect("public history");
            let message_id = u64::try_from(history.len() + 1)
                .map_err(|_| DigPublicSendFailure::ambiguous("test history overflow"))?;
            history.push(DigPublicHistoryMessage {
                message_id,
                author_id: BOT_USER_ID,
                nonce: Some(nonce.to_owned()),
                interaction_id: None,
                content: String::new(),
                embed_titles: Vec::new(),
                embed_descriptions: Vec::new(),
            });
            self.message_channels
                .lock()
                .expect("message channels")
                .insert(message_id, channel_id);
            if accept_then_fail {
                *self.fail_next_history.lock().expect("history fault") = true;
                return Err(DigPublicSendFailure::ambiguous(
                    "test connection lost after Discord accepted the message",
                ));
            }
            Ok(InteractionMessageReceipt {
                message_id,
                channel_id: u64::try_from(channel_id)
                    .map_err(|_| DigPublicSendFailure::rejected("negative test channel"))?,
                delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
            })
        }

        async fn dig_add_reaction(
            &self,
            channel_id: i64,
            message_id: u64,
            emoji: &str,
        ) -> Result<(), String> {
            self.reactions.lock().expect("reactions").push((
                channel_id,
                message_id,
                emoji.to_owned(),
            ));
            Ok(())
        }

        async fn dig_public_history(
            &self,
            channel_id: i64,
            _after_unix_seconds: i64,
            limit: usize,
        ) -> Result<DigPublicHistory, String> {
            let fail = {
                let mut fail_next = self.fail_next_history.lock().expect("history fault");
                let fail = *fail_next;
                *fail_next = false;
                fail
            };
            if fail {
                return Err("test connection lost before receipt history".to_owned());
            }
            Ok(DigPublicHistory {
                bot_user_id: 8_008,
                messages: self
                    .public_history
                    .lock()
                    .expect("public history")
                    .iter()
                    .filter(|message| {
                        self.message_channels
                            .lock()
                            .expect("message channels")
                            .get(&message.message_id)
                            .copied()
                            .is_none_or(|message_channel| message_channel == channel_id)
                    })
                    .rev()
                    .take(limit)
                    .cloned()
                    .collect(),
            })
        }
    }

    #[derive(Default)]
    struct TestResponder {
        defers: StdMutex<Vec<bool>>,
        responses: StdMutex<Vec<InteractionResponse>>,
        followups: StdMutex<Vec<InteractionResponse>>,
        updates: StdMutex<Vec<InteractionResponse>>,
        original_edits: StdMutex<Vec<InteractionResponse>>,
        message_edits: StdMutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
        autocompletes: StdMutex<Vec<Vec<CommandOptionChoice>>>,
        modals: StdMutex<Vec<InteractionModal>>,
        lifecycle: Option<Arc<StdMutex<Vec<&'static str>>>>,
    }

    impl TestResponder {
        fn with_lifecycle(lifecycle: Arc<StdMutex<Vec<&'static str>>>) -> Self {
            Self {
                lifecycle: Some(lifecycle),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl InteractionResponder for TestResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.responses.lock().expect("responses").push(response);
            Ok(())
        }

        async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
            self.defers.lock().expect("defers").push(ephemeral);
            Ok(())
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.followups.lock().expect("followups").push(response);
            if let Some(lifecycle) = &self.lifecycle {
                lifecycle.lock().expect("lifecycle log").push("followup");
            }
            Ok(())
        }

        async fn followup_with_receipt(
            &self,
            response: InteractionResponse,
        ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
            self.followup(response).await?;
            Ok(Some(InteractionMessageReceipt {
                message_id: 99,
                channel_id: CHANNEL,
                delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
            }))
        }

        async fn update(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.updates.lock().expect("updates").push(response);
            Ok(())
        }

        async fn edit_original(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.original_edits
                .lock()
                .expect("original edits")
                .push(response);
            Ok(())
        }

        async fn edit_message(
            &self,
            receipt: InteractionMessageReceipt,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.message_edits
                .lock()
                .expect("message edits")
                .push((receipt, response));
            Ok(())
        }

        async fn autocomplete(
            &self,
            choices: Vec<CommandOptionChoice>,
        ) -> Result<(), InteractionResponseError> {
            self.autocompletes
                .lock()
                .expect("autocompletes")
                .push(choices);
            Ok(())
        }

        async fn show_modal(
            &self,
            modal: InteractionModal,
        ) -> Result<(), InteractionResponseError> {
            self.modals.lock().expect("modals").push(modal);
            Ok(())
        }
    }

    struct AcceptedThenLostEventResponder {
        inner: Arc<TestResponder>,
        discord: Arc<TestDiscord>,
        interaction_id: u64,
        channel_id: i64,
    }

    #[async_trait]
    impl InteractionResponder for AcceptedThenLostEventResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.inner.respond(response).await
        }

        async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
            self.inner.defer(ephemeral).await
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.inner.followup(response).await
        }

        async fn followup_with_receipt(
            &self,
            response: InteractionResponse,
        ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
            self.inner.followup(response.clone()).await?;
            self.discord
                .public
                .lock()
                .expect("accepted event public response")
                .push(response.clone());
            let mut history = self
                .discord
                .public_history
                .lock()
                .expect("accepted event history");
            let message_id = u64::try_from(history.len() + 1).expect("event history id");
            history.push(DigPublicHistoryMessage {
                message_id,
                author_id: 8_008,
                nonce: None,
                interaction_id: Some(self.interaction_id),
                content: response.content,
                embed_titles: response
                    .embeds
                    .iter()
                    .map(|embed| embed.title.clone())
                    .collect(),
                embed_descriptions: response
                    .embeds
                    .iter()
                    .map(|embed| embed.description.clone())
                    .collect(),
            });
            self.discord
                .message_channels
                .lock()
                .expect("accepted event channel")
                .insert(message_id, self.channel_id);
            Err(InteractionResponseError::new(
                "test connection lost after event follow-up acceptance",
            ))
        }

        async fn update(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.inner.update(response).await
        }
    }

    #[derive(Default)]
    struct RejectingPublicFollowupResponder {
        defers: StdMutex<Vec<bool>>,
        attempts: StdMutex<Vec<InteractionResponse>>,
    }

    #[async_trait]
    impl InteractionResponder for RejectingPublicFollowupResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.attempts.lock().expect("attempts").push(response);
            Ok(())
        }

        async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
            self.defers.lock().expect("defers").push(ephemeral);
            Ok(())
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            let public = !response.ephemeral;
            self.attempts.lock().expect("attempts").push(response);
            if public {
                Err(InteractionResponseError::new("public followup forbidden"))
            } else {
                Ok(())
            }
        }
    }

    fn config() -> ApplicationConfig {
        ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
            "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
            _ => None,
        })
        .expect("provider test config")
    }

    fn neon_config(chance: &str) -> ApplicationConfig {
        ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
            "NEON_DEGEN_ENABLED" => Some("true".to_owned()),
            "DIG_LLM_ENABLED" => Some("false".to_owned()),
            "NEON_DIG_CHANCE" => Some(chance.to_owned()),
            _ => None,
        })
        .expect("Neon provider test config")
    }

    fn fixture() -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
        fixture_with_discord(Arc::new(TestDiscord::default()))
    }

    fn fixture_with_discord(
        discord: Arc<TestDiscord>,
    ) -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
        fixture_with_discord_and_config(discord, config())
    }

    fn fixture_with_discord_and_config(
        discord: Arc<TestDiscord>,
        config: ApplicationConfig,
    ) -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                USER as i64,
                "dig-test-miner",
                Some(GUILD as i64),
            ))
            .expect("registered player");
        Connection::open(database.path())
            .expect("open test database")
            .execute(
                "UPDATE players SET jopacoin_balance=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![500_i64, USER as i64, GUILD as i64],
            )
            .expect("seed balance");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|ancestor| ancestor.join("assets/dig"))
            .find(|candidate| candidate.is_dir())
            .expect("repository Dig asset tree");
        let media = Arc::new(DigMediaRuntime::production(
            &DigRuntimeConfig::with_asset_root(root),
        ));
        let provider = DigRegistrationProvider::with_media(
            database.path(),
            &config,
            discord.clone(),
            None,
            media,
        );
        (database, provider, discord)
    }

    fn hook_outcome() -> DigRuntimeOutcome {
        DigRuntimeOutcome {
            success: true,
            error: None,
            depth_before: 100,
            depth_after: 90,
            advance: 1,
            jc_earned: 2,
            vanity_tax: 0,
            balance_after: 100,
            tunnel_name: "Test Tunnel".to_owned(),
            milestone_bonus: 0,
            streak_bonus: 0,
            bankruptcy_penalty: 0,
            luminosity_after: 100,
            luminosity_drained: 0,
            corruption_description: None,
            mutation_names: Vec::new(),
            tip: "Keep digging!".to_owned(),
            cave_in: false,
            cave_in_detail: None,
            event_id: None,
            artifact_id: None,
            boss_boundary: None,
            first_dig: false,
            paid_dig_cost: 0,
            cooldown_remaining: 0,
            paid_dig_available: false,
            items_used: Vec::new(),
            consumed_item_ids: Vec::new(),
            action_id: Some(11),
            route_choice_required: false,
            pickaxe_tier: 1,
            pet_dig_bonus: 0,
            pet_name: None,
            forced_event_consumed: false,
            relic_trim_notice: false,
            weather: None,
        }
    }

    #[derive(Default)]
    struct RecordingBonusDispatcher {
        bonuses: StdMutex<Vec<cama_app::dig_bonus_events::DigBonus>>,
    }

    #[async_trait]
    impl DigBonusDispatchPort for RecordingBonusDispatcher {
        async fn dispatch_bonus(
            &self,
            _action_id: i64,
            _user_id: i64,
            _guild_id: i64,
            _channel_id: i64,
            bonus: cama_app::dig_bonus_events::DigBonus,
            _responder: Arc<dyn InteractionResponder>,
        ) -> Result<(), String> {
            self.bonuses.lock().expect("bonus calls").push(bonus);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailingBonusDispatcher {
        attempts: StdMutex<usize>,
    }

    #[async_trait]
    impl DigBonusDispatchPort for FailingBonusDispatcher {
        async fn dispatch_bonus(
            &self,
            _action_id: i64,
            _user_id: i64,
            _guild_id: i64,
            _channel_id: i64,
            _bonus: cama_app::dig_bonus_events::DigBonus,
            _responder: Arc<dyn InteractionResponder>,
        ) -> Result<(), String> {
            *self.attempts.lock().expect("bonus attempts") += 1;
            Err("bonus adapter failed after dispatch began".to_owned())
        }
    }

    #[test]
    fn post_result_policy_reuses_authored_flame_copy_and_stable_bonus_roll() {
        assert_eq!(
            super::catastrophic_flame_line(11),
            super::catastrophic_flame_line(11)
        );
        assert!(super::CATASTROPHIC_LINES.contains(&super::catastrophic_flame_line(11)));
        let roll = super::deterministic_dig_bonus_roll(11);
        assert!((0.0..=1.0).contains(&roll));
        assert_eq!(
            cama_app::dig_bonus_events::pick_dig_bonus(roll),
            cama_app::dig_bonus_events::pick_dig_bonus(roll)
        );
    }

    #[test]
    fn pet_activity_source_key_is_action_scoped_and_retry_stable() {
        assert_eq!(
            super::dig_pet_activity_source_key(42),
            super::dig_pet_activity_source_key(42)
        );
        assert_ne!(
            super::dig_pet_activity_source_key(42),
            super::dig_pet_activity_source_key(43)
        );
    }

    #[test]
    fn post_result_policy_maps_only_rare_or_legendary_artifacts_to_neon() {
        assert_eq!(
            super::dig_artifact_neon_info("echo_stone"),
            Some(("Echo Stone".to_owned(), "rare".to_owned()))
        );
        assert_eq!(
            super::dig_artifact_neon_info("hollow_eye"),
            Some(("Hollow Eye".to_owned(), "legendary".to_owned()))
        );
        assert_eq!(super::dig_artifact_neon_info("crystal_compass"), None);
        assert_eq!(
            super::dig_artifact_neon_info("pinnacle:Cloak of the Necropolis:Long Silence"),
            Some(("Cloak of the Necropolis".to_owned(), "legendary".to_owned()))
        );
    }

    #[test]
    fn post_result_reactions_match_python_result_shapes() {
        let mut outcome = hook_outcome();
        outcome.cave_in = true;
        outcome.artifact_id = Some("echo_stone".to_owned());
        assert_eq!(
            super::dig_result_reactions(&outcome, None),
            vec!["⛏️", "💥", "💎"]
        );
        outcome.boss_boundary = Some(101);
        assert_eq!(super::dig_result_reactions(&outcome, None), vec!["💀"]);
        outcome.boss_boundary = None;
        outcome.first_dig = true;
        assert!(super::dig_result_reactions(&outcome, None).is_empty());
    }

    #[tokio::test]
    async fn catastrophic_flame_is_claimed_once_and_never_blocks_dig_delivery() {
        let (_database, provider, discord) = fixture();
        let mut outcome = hook_outcome();
        outcome.cave_in = true;
        outcome.cave_in_detail =
            Some(serde_json::json!({"type": "catastrophic", "block_loss": 10}).to_string());
        provider
            .handler
            .post_catastrophic_flame(11, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
            .await;
        provider
            .handler
            .post_catastrophic_flame(11, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
            .await;
        assert_eq!(discord.public.lock().expect("public responses").len(), 1);
    }

    #[tokio::test]
    async fn catastrophic_flame_transport_error_keeps_terminal_claim() {
        let (_database, provider, discord) = fixture();
        let mut outcome = hook_outcome();
        outcome.cave_in = true;
        outcome.cave_in_detail = Some(serde_json::json!({"type": "catastrophic"}).to_string());
        *discord
            .reject_un_nonnced_public_send
            .lock()
            .expect("send fault") = true;
        provider
            .handler
            .post_catastrophic_flame(12, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
            .await;
        *discord
            .reject_un_nonnced_public_send
            .lock()
            .expect("send fault") = false;
        provider
            .handler
            .post_catastrophic_flame(12, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
            .await;
        assert!(
            discord.public.lock().expect("public responses").is_empty(),
            "an ambiguous post-send error must not be retried into a duplicate"
        );
    }

    #[tokio::test]
    async fn dig_bonus_roll_and_dispatch_are_stable_across_provider_retry() {
        let (_database, provider, _discord) = fixture();
        let dispatcher = Arc::new(RecordingBonusDispatcher::default());
        provider.set_bonus_dispatcher(dispatcher.clone());
        let action_id = (1_i64..10_000)
            .find(|action_id| {
                cama_app::dig_bonus_events::pick_dig_bonus(super::deterministic_dig_bonus_roll(
                    *action_id,
                ))
                .is_some()
            })
            .expect("stable test action should select a bonus");
        let mut outcome = hook_outcome();
        outcome.action_id = Some(action_id);
        let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::default());
        provider
            .handler
            .maybe_send_dig_bonus(
                &outcome,
                USER as i64,
                GUILD as i64,
                CHANNEL as i64,
                Arc::clone(&responder),
            )
            .await;
        provider
            .handler
            .maybe_send_dig_bonus(
                &outcome,
                USER as i64,
                GUILD as i64,
                CHANNEL as i64,
                responder,
            )
            .await;
        assert_eq!(dispatcher.bonuses.lock().expect("bonus calls").len(), 1);
    }

    #[tokio::test]
    async fn ambiguous_bonus_dispatch_keeps_claim_terminal_across_retry() {
        let (_database, provider, _discord) = fixture();
        let dispatcher = Arc::new(FailingBonusDispatcher::default());
        provider.set_bonus_dispatcher(dispatcher.clone());
        let action_id = (1_i64..10_000)
            .find(|action_id| {
                cama_app::dig_bonus_events::pick_dig_bonus(super::deterministic_dig_bonus_roll(
                    *action_id,
                ))
                .is_some()
            })
            .expect("stable test action should select a bonus");
        let mut outcome = hook_outcome();
        outcome.action_id = Some(action_id);
        let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::default());
        provider
            .handler
            .maybe_send_dig_bonus(
                &outcome,
                USER as i64,
                GUILD as i64,
                CHANNEL as i64,
                Arc::clone(&responder),
            )
            .await;
        provider
            .handler
            .maybe_send_dig_bonus(
                &outcome,
                USER as i64,
                GUILD as i64,
                CHANNEL as i64,
                responder,
            )
            .await;
        assert_eq!(*dispatcher.attempts.lock().expect("bonus attempts"), 1);
    }

    fn go_request() -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 1,
            name: "dig".to_owned(),
            user_id: USER,
            user_display_name: "Dig Test Miner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: "go".to_owned(),
                value: InteractionValue::Subcommand(Vec::new()),
            }],
        }
    }

    fn component_request(custom_id: impl Into<String>, values: Vec<String>) -> InteractionRequest {
        component_request_as(USER, "Dig Test Miner", custom_id, values)
    }

    fn event_component_id(
        provider: &DigRegistrationProvider,
        action_id: i64,
        choice: &str,
    ) -> String {
        format!(
            "dig:event-action:{}:{action_id}:{choice}",
            provider.handler.state.view_nonce
        )
    }

    fn component_request_as(
        user_id: u64,
        display_name: &str,
        custom_id: impl Into<String>,
        values: Vec<String>,
    ) -> InteractionRequest {
        InteractionRequest::Component {
            interaction_id: 2,
            custom_id: custom_id.into(),
            user_id,
            user_display_name: display_name.to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            values,
        }
    }

    fn modal_request(
        custom_id: impl Into<String>,
        fields: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> InteractionRequest {
        InteractionRequest::Modal {
            interaction_id: 3,
            custom_id: custom_id.into(),
            user_id: USER,
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    fn command_request(
        interaction_id: u64,
        subcommand: &str,
        options: Vec<InteractionOption>,
    ) -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id,
            name: "dig".to_owned(),
            user_id: USER,
            user_display_name: "Dig Test Miner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: subcommand.to_owned(),
                value: InteractionValue::Subcommand(options),
            }],
        }
    }

    fn grouped_command_request(
        interaction_id: u64,
        group: &str,
        subcommand: &str,
        options: Vec<InteractionOption>,
    ) -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id,
            name: "dig".to_owned(),
            user_id: USER,
            user_display_name: "Dig Test Miner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: group.to_owned(),
                value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                    name: subcommand.to_owned(),
                    value: InteractionValue::Subcommand(options),
                }]),
            }],
        }
    }

    fn autocomplete_request(
        interaction_id: u64,
        subcommand: &str,
        focused_option: &str,
        focused_value: &str,
    ) -> InteractionRequest {
        InteractionRequest::Autocomplete {
            interaction_id,
            name: "dig".to_owned(),
            user_id: USER,
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            focused_option: focused_option.to_owned(),
            focused_value: focused_value.to_owned(),
            options: vec![InteractionOption {
                name: subcommand.to_owned(),
                value: InteractionValue::Subcommand(vec![InteractionOption {
                    name: focused_option.to_owned(),
                    value: InteractionValue::String(focused_value.to_owned()),
                }]),
            }],
        }
    }

    #[tokio::test]
    async fn accepted_public_delivery_is_reconciled_after_restart_without_duplicate_send() {
        let (database, provider, discord) = fixture();
        let now = super::unix_now();
        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                now,
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0xfedc_ba98_7654_3210,
                    CHANNEL as i64,
                    "Dig Test Miner",
                    None,
                ),
            )
            .await
            .expect("commit mechanics and immutable delivery");
        let delivery = execution.delivery.expect("committed delivery snapshot");
        let nonce = super::dig_delivery_nonce(
            &delivery,
            cama_app::dig_runtime::DigRuntimeDeliveryPart::Main,
        );
        assert_eq!(nonce, "cama-d:fedcba9876543210:m");
        assert!(nonce.len() <= 25);

        // Model a process dying immediately after Discord accepts the stable
        // nonce, before mark_delivery_part can commit its SQLite CAS.
        let (main, event) = super::dig_delivery_responses(
            &delivery,
            &provider.handler.state.media,
            &provider.handler.state.view_nonce,
        );
        assert!(event.is_none(), "first Dig has one delivery part");
        discord
            .dig_send_public_once(CHANNEL as i64, main, &nonce)
            .await
            .expect("Discord accepted the message before the crash");
        assert_eq!(discord.public.lock().expect("public responses").len(), 1);
        assert_eq!(
            provider
                .handler
                .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("delivery remains pending before restart")
                .len(),
            1
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        restarted
            .handler
            .deliver_to_channel(&delivery)
            .await
            .expect("restart reconciles the accepted nonce");
        assert_eq!(
            discord.public.lock().expect("single public response").len(),
            1,
            "history reconciliation must not issue a second Discord send"
        );
        assert!(
            restarted
                .handler
                .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("reload reconciled delivery")
                .is_empty(),
            "the recovered receipt completes the durable delivery CAS"
        );
    }

    #[tokio::test]
    async fn accepted_event_delivery_is_reconciled_after_restart_without_duplicate_send() {
        let (database, provider, discord) = fixture();
        let connection = Connection::open(database.path()).expect("open event outbox database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
                  prestige_perks,boss_progress)
                 VALUES (?1,?2,30,30,1,100,'[]','{}')",
                params![USER as i64, GUILD as i64],
            )
            .expect("seed event outbox tunnel");
        let prompt_action_id = {
            connection
                .execute(
                    "INSERT INTO dig_actions (
                         guild_id, actor_id, target_id, action_type, depth_before,
                         depth_after, jc_delta, detail, created_at
                     ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                    params![
                        GUILD as i64,
                        USER as i64,
                        serde_json::json!({"event":"underground_stream"}).to_string(),
                        super::unix_now() - 1,
                    ],
                )
                .expect("seed event outbox prompt");
            connection.last_insert_rowid()
        };
        let service = cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
            database.path(),
            provider.handler.state.dig_config.clone(),
        );
        let outcome = service
            .resolve_action_event_with_delivery(
                cama_app::dig_event_runtime::DigEventActionRequest {
                    discord_id: USER as i64,
                    guild_id: GUILD as i64,
                    dig_action_id: prompt_action_id,
                    choice: "safe",
                    now: super::unix_now(),
                },
                cama_app::dig_event_runtime::DigEventDeliveryContext::new(
                    USER as i64,
                    GUILD as i64,
                    0x1234_5678,
                    CHANNEL as i64,
                ),
            )
            .expect("settle event and attach outbox");
        let action_id = outcome.action_id.expect("resolved event action id");
        let delivery = provider
            .handler
            .event_delivery_for_action(action_id, USER as i64, GUILD as i64)
            .await
            .expect("query event Ready projection")
            .expect("event Ready projection");
        assert_eq!(delivery.context.channel_id, CHANNEL as i64);
        let response = super::event_resolution_response(&delivery.outcome);
        discord
            .dig_send_public_once(CHANNEL as i64, response, &delivery.nonce())
            .await
            .expect("Discord accepted event result before CAS");

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        restarted
            .handler
            .deliver_event_to_channel(&delivery)
            .await
            .expect("restart reconciles accepted event nonce");
        assert_eq!(
            discord.public.lock().expect("single event response").len(),
            1,
            "event history reconciliation must not issue a duplicate send"
        );
        assert!(
            restarted
                .handler
                .pending_event_deliveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query event outbox after restart")
                .is_empty(),
            "event delivery CAS completes after nonce reconciliation"
        );
    }

    struct EmptyGatewayMembers;

    #[async_trait]
    impl GuildMemberPageSource for EmptyGatewayMembers {
        async fn fetch_page(
            &self,
            _guild_id: u64,
            _after: Option<u64>,
            _limit: u64,
        ) -> Result<Vec<GatewayMember>, String> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn live_event_followup_acceptance_is_reconciled_by_ready_without_duplicate() {
        let (database, provider, discord) = fixture();
        let connection = Connection::open(database.path()).expect("open live event database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
                  prestige_perks,boss_progress)
                 VALUES (?1,?2,30,30,1,100,'[]','{}')",
                params![USER as i64, GUILD as i64],
            )
            .expect("seed live event tunnel");
        let prompt_action_id = {
            connection
                .execute(
                    "INSERT INTO dig_actions (
                         guild_id, actor_id, target_id, action_type, depth_before,
                         depth_after, jc_delta, detail, created_at
                     ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                    params![
                        GUILD as i64,
                        USER as i64,
                        serde_json::json!({"event":"underground_stream"}).to_string(),
                        super::unix_now() - 1,
                    ],
                )
                .expect("seed live event prompt");
            connection.last_insert_rowid()
        };
        // The follow-up is recorded with the component interaction metadata,
        // then the history read fails. This models a process dying after
        // Discord accepted the message and before the delivery CAS.
        *discord
            .fail_next_history
            .lock()
            .expect("event history fault") = true;
        let inner = Arc::new(TestResponder::default());
        let responder: Arc<dyn InteractionResponder> = Arc::new(AcceptedThenLostEventResponder {
            inner: Arc::clone(&inner),
            discord: Arc::clone(&discord),
            interaction_id: 2,
            channel_id: CHANNEL as i64,
        });
        let result = provider
            .handler
            .handle(
                component_request(
                    event_component_id(&provider, prompt_action_id, "safe"),
                    Vec::new(),
                ),
                responder,
            )
            .await;
        assert!(
            result.is_err(),
            "the lost history read leaves Ready for recovery"
        );
        assert_eq!(inner.updates.lock().expect("event source update").len(), 1);
        assert_eq!(
            discord.public.lock().expect("accepted event result").len(),
            1
        );
        assert_eq!(
            provider
                .handler
                .pending_event_deliveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query Ready event")
                .len(),
            1
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        let report = restarted
            .gateway_observer()
            .ready_recovery(ReadyRecoveryContext::new(
                vec![GUILD],
                Arc::new(EmptyGatewayMembers),
            ))
            .await;
        assert!(
            report.failures.is_empty(),
            "event READY recovery: {report:?}"
        );
        assert_eq!(report.members_refreshed, 1);
        assert_eq!(
            discord.public.lock().expect("single event result").len(),
            1,
            "interaction-history reconciliation must avoid a nonce repost"
        );
        assert!(
            restarted
                .handler
                .pending_event_deliveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query recovered event")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn ready_recovery_freezes_pending_event_before_sending_once() {
        let (database, provider, discord) = fixture();
        let connection = Connection::open(database.path()).expect("open pending event database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
                  prestige_perks,boss_progress)
                 VALUES (?1,?2,30,30,1,100,'[]','{}')",
                params![USER as i64, GUILD as i64],
            )
            .expect("seed pending event tunnel");
        let prompt_action_id = {
            connection
                .execute(
                    "INSERT INTO dig_actions (
                         guild_id, actor_id, target_id, action_type, depth_before,
                         depth_after, jc_delta, detail, created_at
                     ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                    params![
                        GUILD as i64,
                        USER as i64,
                        serde_json::json!({"event":"underground_stream"}).to_string(),
                        super::unix_now() - 1,
                    ],
                )
                .expect("seed pending event prompt");
            connection.last_insert_rowid()
        };
        let service = cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
            database.path(),
            provider.handler.state.dig_config.clone(),
        );
        let outcome = service
            .resolve_action_event_with_delivery(
                cama_app::dig_event_runtime::DigEventActionRequest {
                    discord_id: USER as i64,
                    guild_id: GUILD as i64,
                    dig_action_id: prompt_action_id,
                    choice: "safe",
                    now: super::unix_now(),
                },
                cama_app::dig_event_runtime::DigEventDeliveryContext::new(
                    USER as i64,
                    GUILD as i64,
                    0xfeed_beef,
                    CHANNEL as i64,
                ),
            )
            .expect("settle event outbox");
        let action_id = outcome.action_id.expect("event action id");
        // Simulate a process stopping after actor settlement but before the
        // application-owned quest/finale follow-up froze the projection.
        connection
            .execute(
                "UPDATE dig_actions
                    SET detail=json_set(detail, '$.event_delivery.state', 'pending')
                  WHERE id=?1 AND action_type='event'",
                params![action_id],
            )
            .expect("rewind event delivery to pending crash state");
        assert_eq!(
            provider
                .handler
                .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query pending event recovery")
                .len(),
            1
        );

        let report = provider
            .gateway_observer()
            .ready_recovery(ReadyRecoveryContext::new(
                vec![GUILD],
                Arc::new(EmptyGatewayMembers),
            ))
            .await;
        assert!(
            report.failures.is_empty(),
            "pending event recovery: {report:?}"
        );
        assert_eq!(report.members_refreshed, 1);
        assert_eq!(discord.public.lock().expect("event send").len(), 1);
        assert!(
            provider
                .handler
                .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query recovered pending event")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn configured_public_send_crash_reconciles_without_duplicate() {
        const CONFIGURED_CHANNEL: i64 = CHANNEL as i64 + 1;
        let discord = Arc::new(TestDiscord::with_channels([
            CHANNEL as i64,
            CONFIGURED_CHANNEL,
        ]));
        discord.arm_accept_then_fail_nonce_send();
        discord.reject_un_nonnced_public_send();
        let configured = ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
            "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
            "DIG_CHANNEL_ID" => Some(CONFIGURED_CHANNEL.to_string()),
            _ => None,
        })
        .expect("configured Dig provider test config");
        let (database, provider, discord) =
            fixture_with_discord_and_config(discord, configured.clone());
        let responder = Arc::new(TestResponder::default());

        // The configured-channel send accepts the nonce-addressed message,
        // then the process loses its connection before the durable CAS.  The
        // provider must not fall back to an un-nonced public send in this
        // ambiguous state.
        let initial = provider.handler.handle(go_request(), responder).await;
        assert!(initial.is_err());
        assert_eq!(
            discord.public.lock().expect("public responses").len(),
            1,
            "Discord accepted exactly one configured-channel message"
        );
        assert_eq!(
            discord.public_history.lock().expect("public history").len(),
            1
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &configured,
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        let report = restarted
            .gateway_observer()
            .ready_recovery(ReadyRecoveryContext::new(
                vec![GUILD],
                Arc::new(EmptyGatewayMembers),
            ))
            .await;
        assert!(report.failures.is_empty(), "recovery report: {report:?}");
        assert_eq!(report.members_refreshed, 1);
        assert_eq!(
            discord.public.lock().expect("single public response").len(),
            1,
            "READY reconciliation must not issue a duplicate configured send"
        );
        assert!(
            restarted
                .handler
                .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("pending recovery query")
                .is_empty(),
            "receipt recovery completes the durable delivery CAS"
        );
    }

    #[tokio::test]
    async fn configured_fallback_crash_reconciles_without_duplicate_after_rebind() {
        const CONFIGURED_CHANNEL: i64 = CHANNEL as i64 + 1;
        let discord = Arc::new(TestDiscord::with_channels([
            CHANNEL as i64,
            CONFIGURED_CHANNEL,
        ]));
        discord.reject_next_configured_nonce_send();
        discord.arm_accept_then_fail_nonce_send();
        discord.reject_un_nonnced_public_send();
        let configured = ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
            "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
            "DIG_CHANNEL_ID" => Some(CONFIGURED_CHANNEL.to_string()),
            _ => None,
        })
        .expect("configured Dig provider test config");
        let (database, provider, discord) =
            fixture_with_discord_and_config(discord, configured.clone());
        let responder = Arc::new(TestResponder::default());

        // Configured-channel rejection falls back to the interaction channel;
        // the fallback is accepted, then the process loses the CAS window.
        assert!(
            provider
                .handler
                .handle(go_request(), responder)
                .await
                .is_err()
        );
        assert_eq!(
            discord
                .public
                .lock()
                .expect("fallback public response")
                .len(),
            1
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &configured,
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        let report = restarted
            .gateway_observer()
            .ready_recovery(ReadyRecoveryContext::new(
                vec![GUILD],
                Arc::new(EmptyGatewayMembers),
            ))
            .await;
        assert!(report.failures.is_empty(), "recovery report: {report:?}");
        assert_eq!(
            discord.public.lock().expect("public responses").len(),
            1,
            "persisted fallback-channel rebind must prevent configured-channel repost"
        );
    }

    #[tokio::test]
    async fn accepted_interaction_followup_is_reconciled_by_interaction_and_immutable_body() {
        let (database, provider, discord) = fixture();
        let execution = provider
            .handler
            .run_dig(
                USER as i64,
                GUILD as i64,
                super::unix_now(),
                false,
                false,
                cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                    0x1234_5678,
                    CHANNEL as i64,
                    "Dig Test Miner",
                    None,
                ),
            )
            .await
            .expect("commit immutable followup delivery");
        let delivery = execution.delivery.expect("delivery snapshot");
        let (main, event) = super::dig_delivery_responses(
            &delivery,
            &provider.handler.state.media,
            &provider.handler.state.view_nonce,
        );
        assert!(event.is_none());

        // Interaction followups do not accept a Discord nonce. HTTP history
        // still exposes the originating interaction ID, so the immutable
        // title/description body supplies a stable, per-part discriminator.
        discord
            .public
            .lock()
            .expect("public responses")
            .push(main.clone());
        discord
            .public_history
            .lock()
            .expect("public history")
            .push(DigPublicHistoryMessage {
                message_id: 91,
                author_id: 8_008,
                nonce: None,
                interaction_id: Some(delivery.context.interaction_id),
                content: main.content.clone(),
                embed_titles: main
                    .embeds
                    .iter()
                    .map(|embed| embed.title.clone())
                    .collect(),
                embed_descriptions: main
                    .embeds
                    .iter()
                    .map(|embed| embed.description.clone())
                    .collect(),
            });

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        restarted
            .handler
            .deliver_to_channel(&delivery)
            .await
            .expect("restart reconciles accepted interaction followup");
        assert_eq!(discord.public.lock().expect("public responses").len(), 1);
        assert!(
            restarted
                .handler
                .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("pending queue")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn provider_go_route_event_and_info_use_typed_app_service() {
        let (database, provider, discord) = fixture();
        let go_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(go_request(), go_responder.clone())
            .await
            .expect("/dig go should be admitted");

        assert_eq!(
            go_responder.defers.lock().expect("defers").as_slice(),
            &[false]
        );
        {
            let followups = go_responder.followups.lock().expect("followups");
            assert_eq!(followups.len(), 1);
            assert_eq!(followups[0].embeds.len(), 1);
            assert_eq!(
                followups[0].embeds[0].title.as_deref(),
                Some("Welcome to the Mines!")
            );
            assert_eq!(
                followups[0].embeds[0].description.as_deref(),
                Some(
                    "You've started digging your very own tunnel!\n\nUse `/dig` to advance deeper, `/dig shop` to buy items, and `/dig guide` for a full tutorial.\n\nGood luck, miner! **DIG DUG!**"
                )
            );
            assert!(followups[0].embeds[0].fields.is_empty());
        }
        assert!(discord.public.lock().expect("public responses").is_empty());

        let service = DigRuntimeService::sqlite(database.path());
        let first = service
            .snapshot(USER as i64, GUILD as i64)
            .expect("typed snapshot after go");
        assert_eq!(
            first.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
            Some(1)
        );
        assert_eq!(
            first.gear.len(),
            1,
            "starter gear is committed with the first dig"
        );

        let trap_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(3, "trap", Vec::new()),
                trap_responder.clone(),
            )
            .await
            .expect("trap should use typed app mutation");
        assert!(
            trap_responder
                .followups
                .lock()
                .expect("trap followups")
                .last()
                .is_some_and(|response| response.content.contains("Trap set!"))
        );
        assert!(!trap_responder.followups.lock().unwrap()[0].ephemeral);
        assert_eq!(trap_responder.defers.lock().unwrap().as_slice(), &[false]);
        let insure_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(4, "insure", Vec::new()),
                insure_responder.clone(),
            )
            .await
            .expect("insurance should use typed app mutation");
        assert!(
            insure_responder
                .followups
                .lock()
                .expect("insurance followups")
                .last()
                .is_some_and(|response| response.content.contains("Insurance purchased"))
        );
        assert!(!insure_responder.followups.lock().unwrap()[0].ephemeral);
        assert_eq!(insure_responder.defers.lock().unwrap().as_slice(), &[false]);

        let weather_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(5, "weather", Vec::new()),
                weather_responder.clone(),
            )
            .await
            .expect("weather should use typed app read model");
        assert!(
            weather_responder
                .responses
                .lock()
                .expect("weather responses")
                .last()
                .is_some_and(|response| !response.embeds.is_empty())
        );

        let action_count: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .expect("dig audit count");
        assert_eq!(action_count, 1);

        // Seed one canonical pending route exactly as the migrated route
        // repository serializes it, then drive the real component adapter.
        let pending_route = r#"{"layer":"Stone","start_depth":25,"end_depth":50,"offered":["shored_passage","old_supports","fossil_seam"],"selected":null}"#;
        Connection::open(database.path())
            .expect("open route fixture")
            .execute(
                "UPDATE tunnels SET route_state=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![pending_route, USER as i64, GUILD as i64],
            )
            .expect("seed pending route");
        let route_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request("dig:route:old_supports", vec!["old_supports".to_owned()]),
                route_responder.clone(),
            )
            .await
            .expect("legacy route component should be rejected safely");
        assert!(
            route_responder
                .responses
                .lock()
                .expect("route responses")
                .last()
                .is_some_and(|response| response.content.contains("expired after a restart"))
        );
        let routed = service
            .snapshot(USER as i64, GUILD as i64)
            .expect("typed snapshot after route");
        assert!(
            routed
                .tunnel
                .as_ref()
                .and_then(|tunnel| tunnel.route_state.as_deref())
                .and_then(|state| cama_app::dig_routes::parse_route_state(Some(state)))
                .and_then(|state| state.selected)
                .is_none()
        );

        // Event choices are also settled by the app service; the provider
        // only validates ownership and presents the typed action result.
        let event_action_id = {
            let connection = Connection::open(database.path()).expect("open event fixture");
            connection
                .execute(
                    "INSERT INTO dig_actions (
                         guild_id, actor_id, target_id, action_type, depth_before,
                         depth_after, jc_delta, detail, created_at
                     ) VALUES (?1, ?2, NULL, 'dig', 5, 5, 0, ?3, ?4)",
                    params![
                        GUILD as i64,
                        USER as i64,
                        serde_json::json!({"event":"underground_stream"}).to_string(),
                        super::unix_now() - 1
                    ],
                )
                .expect("seed durable event action");
            connection.last_insert_rowid()
        };
        let event_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    event_component_id(&provider, event_action_id, "safe"),
                    Vec::new(),
                ),
                event_responder.clone(),
            )
            .await
            .expect("event component should be admitted");
        {
            let updates = event_responder.updates.lock().expect("event updates");
            assert_eq!(updates.len(), 1);
            assert!(
                updates[0].embeds.is_empty(),
                "component-only update preserves the authored source embed"
            );
            assert_eq!(updates[0].components.len(), 1);
            assert!(
                updates[0].components[0]
                    .buttons
                    .iter()
                    .all(|button| button.disabled),
                "the source event is locked before its result is published"
            );
        }
        {
            let followups = event_responder.followups.lock().expect("event followup");
            assert_eq!(followups.len(), 1);
            assert_eq!(followups[0].embeds.len(), 1);
            assert_eq!(
                followups[0].embeds[0].description.as_deref(),
                Some("You cross safely and find coins on the far bank.")
            );
        }
        assert!(
            provider
                .handler
                .pending_event_deliveries(DigEventPendingDeliveryQuery {
                    guild_id: Some(GUILD as i64),
                    discord_id: Some(USER as i64),
                    limit: 10,
                })
                .await
                .expect("query resolved event delivery")
                .is_empty(),
            "the resolved action's event outbox is marked, not the prompt action"
        );

        let info_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(4, "info", Vec::new()),
                info_responder.clone(),
            )
            .await
            .expect("info should read typed app projection");
        {
            let info = info_responder.responses.lock().expect("info responses");
            assert_eq!(info.len(), 1);
            assert_eq!(info[0].embeds.len(), 1);
            assert!(
                info[0].embeds[0]
                    .fields
                    .iter()
                    .any(|field| field.name == "Depth")
            );
        }

        let action_count: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .expect("action count after components");
        assert_eq!(
            action_count, 4,
            "go, terminal flavor receipt, durable event source, and event choice are audited"
        );
    }

    #[tokio::test]
    async fn event_components_lock_source_publish_once_and_reject_forged_choices() {
        const OTHER: u64 = 77_099;
        let (database, provider, discord) = fixture();
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                OTHER as i64,
                "event-copycat",
                Some(GUILD as i64),
            ))
            .expect("register copied-component actor");
        let connection = Connection::open(database.path()).expect("open event database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
                  prestige_perks,boss_progress)
                 VALUES (?1,?2,30,30,1,100,'[]','{}')",
                params![USER as i64, GUILD as i64],
            )
            .expect("seed event tunnel");
        let seed_action = |event_id: &str| {
            connection
                .execute(
                    "INSERT INTO dig_actions (
                         guild_id,actor_id,target_id,action_type,depth_before,
                         depth_after,jc_delta,detail,created_at
                     ) VALUES (?1,?2,NULL,'dig',30,30,0,?3,?4)",
                    params![
                        GUILD as i64,
                        USER as i64,
                        serde_json::json!({"event":event_id}).to_string(),
                        super::unix_now() - 1,
                    ],
                )
                .expect("seed event action");
            connection.last_insert_rowid()
        };
        let choice_action = seed_action("underground_stream");

        let copied = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request_as(
                    OTHER,
                    "Event Copycat",
                    event_component_id(&provider, choice_action, "safe"),
                    Vec::new(),
                ),
                copied.clone(),
            )
            .await
            .expect("copied event is rejected cleanly");
        assert!(copied.updates.lock().expect("copied updates").is_empty());
        assert_eq!(
            copied.responses.lock().expect("copied response")[0].content,
            "This isn't your event."
        );

        let forged = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    event_component_id(&provider, choice_action, "boon_99"),
                    Vec::new(),
                ),
                forged.clone(),
            )
            .await
            .expect("forged choice is rejected cleanly");
        assert!(forged.updates.lock().expect("forged updates").is_empty());
        assert_eq!(
            forged.responses.lock().expect("forged response")[0].content,
            "That event choice is no longer available."
        );

        let boon_action = seed_action("enchanting_table");
        drop(connection);
        let boon_custom_id = event_component_id(&provider, boon_action, "boon_1");
        let first = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(boon_custom_id.clone(), Vec::new()),
                first.clone(),
            )
            .await
            .expect("boon choice resolves");
        {
            let updates = first.updates.lock().expect("boon source update");
            assert_eq!(updates.len(), 1);
            assert!(updates[0].embeds.is_empty());
            assert_eq!(updates[0].components[0].buttons.len(), 3);
            assert!(
                updates[0].components[0]
                    .buttons
                    .iter()
                    .all(|button| button.disabled)
            );
        }
        {
            let followups = first.followups.lock().expect("boon followup result");
            assert_eq!(followups.len(), 1);
            assert_eq!(
                followups[0].embeds[0].title.as_deref(),
                Some("Enchanting Table")
            );
            assert!(
                followups[0].embeds[0]
                    .fields
                    .iter()
                    .any(|field| field.name.starts_with("Buff:"))
            );
        }

        let duplicate = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(boon_custom_id.clone(), Vec::new()),
                duplicate.clone(),
            )
            .await
            .expect("duplicate delivery replays receipt safely");
        assert_eq!(duplicate.updates.lock().expect("duplicate lock").len(), 1);
        {
            let followups = duplicate.followups.lock().expect("duplicate followup");
            assert_eq!(followups.len(), 1);
            assert!(followups[0].ephemeral);
            assert_eq!(followups[0].content, "You've already resolved this event.");
        }
        assert_eq!(
            discord.public.lock().expect("single public result").len(),
            0,
            "event results stay on the interaction follow-up path"
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            discord.clone(),
            None,
            provider.handler.state.media.clone(),
        );
        assert_ne!(
            restarted.handler.state.view_nonce,
            provider.handler.state.view_nonce
        );
        let stale = Arc::new(TestResponder::default());
        restarted
            .handler
            .handle(component_request(boon_custom_id, Vec::new()), stale.clone())
            .await
            .expect("pre-restart event control expires cleanly");
        assert!(stale.updates.lock().expect("stale updates").is_empty());
        let responses = stale.responses.lock().expect("stale response");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        assert_eq!(responses[0].content, "This Dig event expired.");
    }

    #[tokio::test]
    async fn boss_encounter_modal_and_durable_duel_execute_live_policy() {
        let (database, provider, _discord) = fixture();
        let fixture_connection = Connection::open(database.path()).expect("boss fixture DB");
        fixture_connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
                  boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
                 VALUES (?1,?2,24,24,0,?3,0,0,50,5,'Provider Boss')",
                params![
                    USER as i64,
                    GUILD as i64,
                    r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
                ],
            )
            .expect("boss tunnel");
        fixture_connection
            .execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'lantern',0,100)",
                params![USER as i64, GUILD as i64],
            )
            .expect("scout lantern");

        let encounter = provider
            .handler
            .render_boss_encounter(USER as i64, GUILD as i64, super::unix_now())
            .await
            .expect("encounter response");
        assert_eq!(
            encounter.embeds[0].title.as_deref(),
            Some("Boss Encountered: Grothak the Unbreakable!")
        );
        assert_eq!(encounter.components.len(), 1);
        assert_eq!(
            encounter.components[0]
                .buttons
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Fight", "Retreat", "Scout", "Cheer"]
        );
        assert!(
            encounter.components[0]
                .buttons
                .iter()
                .find(|button| button.label == "Scout")
                .is_some_and(|button| !button.disabled)
        );
        assert!(
            !encounter.attachments.is_empty(),
            "authored or native boss art"
        );

        let scout = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(format!("dig:boss:scout:{USER}:{GUILD}"), Vec::new()),
                scout.clone(),
            )
            .await
            .expect("scout executes live policy");
        {
            let scout_followups = scout.followups.lock().expect("scout followups");
            assert_eq!(
                scout_followups[0].embeds[0].title.as_deref(),
                Some("Boss Scouted")
            );
            assert!(
                scout_followups[0].embeds[0]
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("Cautious")
                        && description.contains("free")
                        && description.contains("payout"))
            );
        }
        assert_eq!(
            fixture_connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_inventory
                      WHERE discord_id=?1 AND guild_id=?2 AND item_type='lantern'",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("lantern count"),
            0
        );

        const CHEERER: u64 = USER + 1;
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                CHEERER as i64,
                "provider-cheerer",
                Some(GUILD as i64),
            ))
            .expect("cheerer player");
        let cheer = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request_as(
                    CHEERER,
                    "Cheer Miner",
                    format!("dig:boss:cheer:{USER}:{GUILD}"),
                    Vec::new(),
                ),
                cheer.clone(),
            )
            .await
            .expect("cheer executes live policy");
        assert!(
            cheer.followups.lock().expect("cheer followups")[0]
                .content
                .contains("+5% (1/3 cheers)")
        );

        let fight = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(format!("dig:boss:fight:{USER}:{GUILD}"), Vec::new()),
                fight.clone(),
            )
            .await
            .expect("fight opens modal");
        let modal = fight.modals.lock().expect("fight modals")[0].clone();
        assert_eq!(modal.title, "Boss Fight Wager");
        assert_eq!(
            modal
                .inputs
                .iter()
                .map(|input| (input.custom_id.as_str(), input.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("risk_tier", "Risk Tier (cautious / bold / reckless)"),
                ("wager", "Wager Amount (max 1,000 JC)"),
            ]
        );

        let submitted = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                modal_request(modal.custom_id, [("risk_tier", "bold"), ("wager", "10")]),
                submitted.clone(),
            )
            .await
            .expect("modal executes boss policy");
        let first = submitted.followups.lock().expect("boss followups")[0].clone();
        assert_ne!(
            first.content,
            format!(
                "Boss wager **10** {} received. Use `/dig go` to continue.",
                cama_domain::formatting::JOPACOIN_EMOTE
            ),
            "the retired echo-only modal path must never return"
        );

        let duel_button = first
            .components
            .iter()
            .flat_map(|row| &row.buttons)
            .find(|button| button.custom_id.starts_with("dig:boss:duel:"))
            .map(|button| button.custom_id.clone());
        if let Some(duel_button) = duel_button {
            // Recreate the provider to prove the component is backed by the
            // durable SQLite duel rather than process-local View state.
            let restarted = DigRegistrationProvider::with_media(
                database.path(),
                &config(),
                Arc::new(TestDiscord::default()),
                None,
                Arc::clone(&provider.handler.state.media),
            );
            let resumed = Arc::new(TestResponder::default());
            restarted
                .handler
                .handle(component_request(duel_button, Vec::new()), resumed.clone())
                .await
                .expect("restart resumes durable duel");
            assert_eq!(resumed.followups.lock().expect("resume followups").len(), 1);
        }
        let connection = Connection::open(database.path()).expect("verify boss DB");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_active_duels", [], |row| row
                    .get::<_, i64>(0),)
                .expect("active duel count"),
            0,
            "the live result either resolved directly or was resumed exactly once"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions WHERE action_type='boss_fight'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("boss audit count"),
            1
        );
    }

    #[tokio::test]
    async fn boss_retreat_component_uses_atomic_application_settlement() {
        let (database, provider, _discord) = fixture();
        Connection::open(database.path())
            .expect("retreat fixture DB")
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
                  boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
                 VALUES (?1,?2,24,24,0,?3,0,0,100,5,'Retreat Boss')",
                params![
                    USER as i64,
                    GUILD as i64,
                    r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
                ],
            )
            .expect("retreat tunnel");
        let response = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(format!("dig:boss:retreat:{USER}:{GUILD}"), Vec::new()),
                response.clone(),
            )
            .await
            .expect("retreat component");
        assert!(
            response.followups.lock().expect("retreat followups")[0]
                .content
                .starts_with("You retreated safely, losing")
        );
        let connection = Connection::open(database.path()).expect("verify retreat DB");
        let (depth, audits): (i64, i64) = connection
            .query_row(
                "SELECT t.depth,
                        (SELECT COUNT(*) FROM dig_actions a
                          WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                            AND a.action_type='boss_retreat')
                   FROM tunnels t WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("retreat state");
        assert!((21..=22).contains(&depth));
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn abandon_preview_owner_restart_cancel_and_atomic_settlement_are_exact() {
        let (database, provider, _discord) = fixture();
        let connection = Connection::open(database.path()).expect("abandon fixture DB");
        connection
            .execute(
                "INSERT INTO tunnels (
                    discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                    pickaxe_tier,prestige_level,boss_progress,streak_days,
                    pinnacle_boss_id,pinnacle_phase,route_state
                 ) VALUES (
                    ?1,?2,100,350,99,777,4,2,'{\"25\":\"defeated\"}',8,
                    'forgotten_king',2,'{\"selected\":\"old_supports\"}'
                 )",
                params![USER as i64, GUILD as i64],
            )
            .expect("seed abandon tunnel");

        let preview_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(58, "abandon", Vec::new()),
                preview_responder.clone(),
            )
            .await
            .expect("abandon preview");
        let preview = preview_responder.responses.lock().unwrap()[0].clone();
        assert!(!preview.ephemeral);
        assert_eq!(preview.embeds[0].title.as_deref(), Some("Abandon Tunnel?"));
        assert!(
            preview.embeds[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Refund: **7**"))
        );
        assert_eq!(preview.components[0].buttons.len(), 2);
        let confirm_id = preview.components[0].buttons[0].custom_id.clone();

        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                USER as i64 + 1,
                "abandon-copy",
                Some(GUILD as i64),
            ))
            .expect("register copied-view actor");
        let copied = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request_as(USER + 1, "Copy", &confirm_id, Vec::new()),
                copied.clone(),
            )
            .await
            .expect("copied abandon rejected");
        assert!(
            copied.responses.lock().unwrap()[0]
                .content
                .contains("isn't your tunnel")
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            None,
            Arc::clone(&provider.handler.state.media),
        );
        let expired = Arc::new(TestResponder::default());
        restarted
            .handler
            .handle(component_request(&confirm_id, Vec::new()), expired.clone())
            .await
            .expect("restart expires abandon view");
        assert!(
            expired.responses.lock().unwrap()[0]
                .content
                .contains("expired")
        );

        let legacy = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request("dig:abandon:confirm", Vec::new()),
                legacy.clone(),
            )
            .await
            .expect("unowned legacy abandon expires");
        assert!(
            legacy.responses.lock().unwrap()[0]
                .content
                .contains("expired")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            100
        );

        let committed = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(&confirm_id, Vec::new()),
                committed.clone(),
            )
            .await
            .expect("abandon commits");
        let update = committed.updates.lock().unwrap()[0].clone();
        assert_eq!(
            update.content,
            format!("Tunnel abandoned. You received **7** {JOPACOIN_EMOTE}.")
        );
        assert!(update.components.is_empty());
        let state: (
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<String>,
            Option<String>,
            i64,
            i64,
        ) = connection
            .query_row(
                "SELECT depth,max_depth,total_digs,total_jc_earned,prestige_level,
                            pinnacle_boss_id,route_state,
                            (SELECT jopacoin_balance FROM players
                              WHERE discord_id=t.discord_id AND guild_id=t.guild_id),
                            (SELECT COUNT(*) FROM dig_actions a
                              WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                                AND a.action_type='abandon')
                       FROM tunnels t WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            state,
            (0, 350, 99, 777, 2, None, None, 507, 1),
            "only Python's partial reset fields change"
        );

        let duplicate = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(&confirm_id, Vec::new()),
                duplicate.clone(),
            )
            .await
            .expect("duplicate abandon is harmless");
        assert!(
            duplicate.responses.lock().unwrap()[0]
                .content
                .contains("already answered")
        );

        connection
            .execute(
                "UPDATE tunnels SET depth=100 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
            )
            .unwrap();
        let cancel_preview = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(59, "abandon", Vec::new()),
                cancel_preview.clone(),
            )
            .await
            .unwrap();
        let cancel_id = cancel_preview.responses.lock().unwrap()[0].components[0].buttons[1]
            .custom_id
            .clone();
        let cancelled = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(component_request(&cancel_id, Vec::new()), cancelled.clone())
            .await
            .unwrap();
        assert_eq!(
            cancelled.updates.lock().unwrap()[0].content,
            "Abandon cancelled."
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            100
        );
    }

    #[test]
    fn abandon_view_owner_timeout_and_claim_boundary_are_exact() {
        let (_database, provider, _discord) = fixture();
        let token = provider
            .handler
            .create_abandon_view(USER as i64, GUILD as i64, 100)
            .unwrap();
        assert_eq!(
            provider
                .handler
                .claim_abandon_view(&token, USER as i64 + 1, GUILD as i64, 159)
                .unwrap(),
            DigAbandonViewAdmission::WrongOwner
        );
        assert_eq!(
            provider
                .handler
                .claim_abandon_view(&token, USER as i64, GUILD as i64, 159)
                .unwrap(),
            DigAbandonViewAdmission::Admitted
        );
        assert_eq!(
            provider
                .handler
                .claim_abandon_view(&token, USER as i64, GUILD as i64, 159)
                .unwrap(),
            DigAbandonViewAdmission::AlreadyResolved
        );
        let expired = provider
            .handler
            .create_abandon_view(USER as i64, GUILD as i64, 200)
            .unwrap();
        assert_eq!(
            provider
                .handler
                .claim_abandon_view(&expired, USER as i64, GUILD as i64, 260)
                .unwrap(),
            DigAbandonViewAdmission::Expired
        );
    }

    #[tokio::test]
    async fn guide_has_four_exact_owner_bound_pages_and_expires_at_180_seconds() {
        let (_database, provider, _discord) = fixture();
        let token = provider
            .handler
            .create_guide_view(USER as i64, GUILD as i64, 100)
            .expect("create guide view");
        assert_eq!(
            provider
                .handler
                .navigate_guide_view(
                    &token,
                    USER as i64 + 1,
                    GUILD as i64,
                    super::DigGuideDirection::Next,
                    279,
                )
                .expect("wrong owner admission"),
            super::DigGuideViewAdmission::WrongOwner
        );
        for expected_page in 1..super::DIG_GUIDE_PAGES.len() {
            assert_eq!(
                provider
                    .handler
                    .navigate_guide_view(
                        &token,
                        USER as i64,
                        GUILD as i64,
                        super::DigGuideDirection::Next,
                        279,
                    )
                    .expect("owner navigation"),
                super::DigGuideViewAdmission::Admitted(expected_page)
            );
        }
        assert_eq!(
            provider
                .handler
                .navigate_guide_view(
                    &token,
                    USER as i64,
                    GUILD as i64,
                    super::DigGuideDirection::Next,
                    279,
                )
                .expect("last-page clamp"),
            super::DigGuideViewAdmission::Admitted(3)
        );
        assert_eq!(
            provider
                .handler
                .navigate_guide_view(
                    &token,
                    USER as i64,
                    GUILD as i64,
                    super::DigGuideDirection::Previous,
                    280,
                )
                .expect("inclusive timeout"),
            super::DigGuideViewAdmission::Expired
        );

        let pages = (0..super::DIG_GUIDE_PAGES.len())
            .map(|page| super::guide_response(page, "owner-token"))
            .collect::<Vec<_>>();
        assert_eq!(
            pages
                .iter()
                .map(|response| response.embeds[0].title.as_deref().unwrap())
                .collect::<Vec<_>>(),
            [
                "Dig Guide — Basics",
                "Dig Guide — Items & Pickaxes",
                "Dig Guide — Bosses",
                "Dig Guide — Prestige",
            ]
        );
        assert_eq!(
            pages
                .iter()
                .map(|response| response.embeds[0].color)
                .collect::<Vec<_>>(),
            [
                Some(0x8B_45_13),
                Some(0x80_80_80),
                Some(0x00_CE_D1),
                Some(0xFF_45_00),
            ]
        );
        assert!(pages[0].components[0].buttons[0].disabled);
        assert!(pages[3].components[0].buttons[1].disabled);
        assert!(pages.iter().all(|response| {
            response.components[0]
                .buttons
                .iter()
                .all(|button| button.custom_id.contains("owner-token"))
        }));
        let expired = super::expired_guide_response();
        assert_eq!(expired.content, "*The moment passed.*");
        assert!(
            expired.components[0]
                .buttons
                .iter()
                .all(|button| button.disabled)
        );

        let (_restarted_database, restarted, _discord) = fixture();
        assert_eq!(
            restarted
                .handler
                .navigate_guide_view(
                    &token,
                    USER as i64,
                    GUILD as i64,
                    super::DigGuideDirection::Next,
                    101,
                )
                .expect("restart admission"),
            super::DigGuideViewAdmission::Expired
        );
    }

    #[test]
    fn flex_projection_renders_empty_roast_and_full_profile_exactly() {
        let empty = cama_app::dig_runtime::DigRuntimeFlexData {
            tunnel_name: "Unknown".to_owned(),
            depth: 0,
            total_digs: 1,
            total_jc_earned: 0,
            prestige_level: 0,
            prestige_emoji: String::new(),
            titles: Vec::new(),
            streak: 0,
            layer: "Dirt".to_owned(),
        };
        let response = super::flex_response(
            &empty,
            "Fresh Miner",
            Some("https://cdn.example/avatar.png"),
            7,
        );
        let embed = &response.embeds[0];
        assert_eq!(embed.title.as_deref(), Some("Fresh Miner's Mining Profile"));
        assert_eq!(embed.color, Some(0xFF_D7_00));
        assert_eq!(
            embed.description.as_deref(),
            Some("*Your pickaxe is still in the shrinkwrap.*")
        );
        assert!(embed.fields.is_empty());
        assert_eq!(
            embed.thumbnail_url.as_deref(),
            Some("https://cdn.example/avatar.png")
        );

        let veteran = cama_app::dig_runtime::DigRuntimeFlexData {
            tunnel_name: "Veteran Shaft".to_owned(),
            depth: 205,
            total_digs: 123,
            total_jc_earned: 456,
            prestige_level: 8,
            prestige_emoji: "⭐⭐⭐⭐⭐".to_owned(),
            titles: vec!["Boss Slayer".to_owned()],
            streak: 9,
            layer: "Frozen Core".to_owned(),
        };
        let response = super::flex_response(&veteran, "Veteran", None, 0);
        let embed = &response.embeds[0];
        assert_eq!(
            embed.description.as_deref(),
            Some("*\"Boss Slayer\"*  ⭐⭐⭐⭐⭐")
        );
        assert_eq!(embed.fields.len(), 1);
        assert_eq!(embed.fields[0].name, "Stats");
        assert_eq!(
            embed.fields[0].value,
            "Tunnel: **Veteran Shaft**\nDepth: **205** (Frozen Core)\nTotal digs: **123**\nTotal JC earned: **456**\nStreak: **9** days\nPrestige: **8**"
        );
    }

    #[test]
    fn weather_effect_copy_matches_every_python_effect_phrase() {
        let effects = cama_app::dig_runtime::DigWeatherEffects {
            cave_in_bonus: -0.1,
            cave_in_loss_bonus: 0,
            cave_in_loss_cap: None,
            advance_bonus: 1,
            event_chance_multiplier: 0.5,
            luminosity_drain_multiplier: 0.5,
            jc_multiplier: -0.25,
            jc_bonus: 2,
            artifact_multiplier: 3.0,
        };
        assert_eq!(
            super::weather_effect_copy(effects),
            "cave-in risk eases, ore veins are thin, seams glitter, ground is soft, the deep stirs, relics surface more often, darkness drains lanterns quickly"
        );
        assert_eq!(
            super::weather_effect_copy(cama_app::dig_runtime::DigWeatherEffects::default()),
            "no notable effect"
        );
    }

    #[test]
    fn paid_prompt_is_tokenized_owner_bound_one_shot_and_exactly_formatted() {
        let (_database, provider, _discord) = fixture();
        let token = provider
            .handler
            .create_paid_view(USER as i64, GUILD as i64, 100)
            .expect("create paid view");
        assert_eq!(
            provider
                .handler
                .claim_paid_view(&token, USER as i64 + 1, GUILD as i64, 159)
                .expect("wrong owner admission"),
            super::DigPaidViewAdmission::WrongOwner
        );
        assert_eq!(
            provider
                .handler
                .claim_paid_view(&token, USER as i64, GUILD as i64, 159)
                .expect("owner claim"),
            super::DigPaidViewAdmission::Admitted
        );
        assert_eq!(
            provider
                .handler
                .claim_paid_view(&token, USER as i64, GUILD as i64, 159)
                .expect("duplicate claim"),
            super::DigPaidViewAdmission::AlreadyClaimed
        );
        let expired = provider
            .handler
            .create_paid_view(USER as i64, GUILD as i64, 200)
            .expect("create expiring paid view");
        assert_eq!(
            provider
                .handler
                .claim_paid_view(&expired, USER as i64, GUILD as i64, 260)
                .expect("inclusive expiry"),
            super::DigPaidViewAdmission::Expired
        );
        let (_restarted_database, restarted, _discord) = fixture();
        assert_eq!(
            restarted
                .handler
                .claim_paid_view(&token, USER as i64, GUILD as i64, 101)
                .expect("restart invalidation"),
            super::DigPaidViewAdmission::Expired
        );

        let result = DigRuntimeOutcome {
            success: false,
            error: Some("cooldown".to_owned()),
            depth_before: 10,
            depth_after: 10,
            advance: 0,
            jc_earned: 0,
            vanity_tax: 0,
            balance_after: 100,
            tunnel_name: "Test Tunnel".to_owned(),
            milestone_bonus: 0,
            streak_bonus: 0,
            bankruptcy_penalty: 0,
            luminosity_after: 100,
            luminosity_drained: 0,
            corruption_description: None,
            mutation_names: Vec::new(),
            tip: String::new(),
            cave_in: false,
            cave_in_detail: None,
            event_id: None,
            artifact_id: None,
            boss_boundary: None,
            first_dig: false,
            paid_dig_cost: 25,
            cooldown_remaining: 3_599,
            paid_dig_available: true,
            items_used: Vec::new(),
            consumed_item_ids: Vec::new(),
            action_id: None,
            route_choice_required: false,
            pickaxe_tier: 0,
            pet_dig_bonus: 0,
            pet_name: None,
            forced_event_consumed: false,
            relic_trim_notice: false,
            weather: None,
        };
        let response = super::paid_dig_response(&result, "secret-token");
        assert_eq!(response.embeds[0].color, Some(0xFF_A5_00));
        let expected = format!(
            "Free dig on cooldown for **59m 59s**.\nContinuing costs **25** {JOPACOIN_EMOTE}. Proceed?"
        );
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            response.components[0].buttons[0].custom_id,
            "dig:paid:confirm:secret-token"
        );
        assert_eq!(
            response.components[0].buttons[1].custom_id,
            "dig:paid:cancel:secret-token"
        );
        assert!(
            !response.components[0].buttons[0]
                .custom_id
                .contains(&USER.to_string())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unresolved_paid_prompt_is_actively_cancelled_at_timeout() {
        let (_database, provider, _discord) = fixture();
        let responder = Arc::new(TestResponder::default());
        let token = provider
            .handler
            .create_paid_view(USER as i64, GUILD as i64, 100)
            .expect("create paid view");
        provider.handler.schedule_paid_view_timeout(
            token.clone(),
            responder.clone(),
            Some(InteractionMessageReceipt {
                message_id: 101,
                channel_id: CHANNEL,
                delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
            }),
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        let edits = responder.message_edits.lock().expect("timeout edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1.content, "Dig cancelled.");
        assert!(edits[0].1.components.is_empty());
        drop(edits);
        assert_eq!(
            provider
                .handler
                .claim_paid_view(&token, USER as i64, GUILD as i64, 160)
                .expect("retired timeout view"),
            super::DigPaidViewAdmission::Expired
        );
    }

    #[tokio::test]
    async fn admin_depth_and_cooldown_commands_preserve_max_and_report_missing_tunnel() {
        let (database, provider, _discord) = fixture();
        Connection::open(database.path())
            .expect("open migrated database")
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,last_dig_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    USER as i64,
                    GUILD as i64,
                    75_i64,
                    275_i64,
                    1_700_000_000_i64
                ],
            )
            .expect("seed admin target tunnel");
        let set_depth = Arc::new(TestResponder::default());
        provider
            .handler
            .command_admin(
                USER as i64,
                GUILD as i64,
                Some(1_u64 << 3),
                "setdepth",
                &[
                    InteractionOption {
                        name: "user".to_owned(),
                        value: InteractionValue::User {
                            id: USER,
                            display_name: Some("Dig Test Miner".to_owned()),
                            is_bot: Some(false),
                        },
                    },
                    InteractionOption {
                        name: "depth".to_owned(),
                        value: InteractionValue::Integer(12),
                    },
                ],
                set_depth.clone(),
            )
            .await
            .expect("set depth command");
        assert_eq!(set_depth.defers.lock().unwrap().as_slice(), &[true]);
        assert_eq!(
            set_depth.followups.lock().unwrap()[0].content,
            format!("Set <@{USER}> to depth **12** and reset cooldown.")
        );
        let stored = Connection::open(database.path())
            .expect("reopen migrated database")
            .query_row(
                "SELECT depth,max_depth,last_dig_at FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("read admin target");
        assert_eq!(stored, (12, 275, 0));

        let missing = Arc::new(TestResponder::default());
        provider
            .handler
            .command_admin(
                USER as i64,
                GUILD as i64,
                Some(1_u64 << 3),
                "resetcooldown",
                &[InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: USER + 1,
                        display_name: Some("Missing Miner".to_owned()),
                        is_bot: Some(false),
                    },
                }],
                missing.clone(),
            )
            .await
            .expect("missing cooldown target");
        assert_eq!(
            missing.followups.lock().unwrap()[0].content,
            "That player doesn't have a tunnel."
        );
    }

    #[tokio::test]
    async fn prestige_preview_selection_restart_and_forgery_use_atomic_app_service() {
        let (database, provider, discord) = fixture();
        let mut progress = serde_json::Map::new();
        for boundary in cama_app::dig_bosses::BOSS_BOUNDARIES {
            progress.insert(
                boundary.to_string(),
                serde_json::Value::String("defeated".to_owned()),
            );
        }
        progress.insert(
            cama_app::dig_bosses::PINNACLE_DEPTH.to_string(),
            serde_json::json!({"status": "defeated", "boss_id": "forgotten_king"}),
        );
        Connection::open(database.path())
            .expect("prestige fixture DB")
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,depth,max_depth,prestige_level,
                     prestige_perks,boss_progress,current_run_jc,current_run_events,
                     pickaxe_tier,tunnel_name
                 ) VALUES (?1,?2,350,350,0,'[]',?3,50,7,4,'Prestige Tunnel')",
                params![
                    USER as i64,
                    GUILD as i64,
                    serde_json::Value::Object(progress).to_string(),
                ],
            )
            .expect("seed prestige tunnel");

        let preview_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(50, "prestige", Vec::new()),
                preview_responder.clone(),
            )
            .await
            .expect("prestige preview");
        let preview = preview_responder
            .responses
            .lock()
            .expect("preview responses")[0]
            .clone();
        assert!(preview.ephemeral);
        assert!(
            preview.embeds[0]
                .title
                .as_deref()
                .is_some_and(|title| title.contains("Prestige to P1"))
        );
        assert_eq!(preview.components[0].buttons.len(), 4);
        let chosen = preview.components[0].buttons[0].custom_id.clone();

        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                USER as i64 + 1,
                "prestige-copy",
                Some(GUILD as i64),
            ))
            .expect("register copied-view actor");
        let copied = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request_as(USER + 1, "Copy", &chosen, Vec::new()),
                copied.clone(),
            )
            .await
            .expect("copied prestige rejected");
        assert!(
            copied.responses.lock().expect("copied responses")[0]
                .content
                .contains("isn't your prestige")
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            None,
            Arc::clone(&provider.handler.state.media),
        );
        let expired = Arc::new(TestResponder::default());
        restarted
            .handler
            .handle(component_request(&chosen, Vec::new()), expired.clone())
            .await
            .expect("old process nonce expires");
        assert!(
            expired.responses.lock().expect("expired responses")[0]
                .content
                .contains("expired")
        );

        let forged_perk = cama_app::dig_tunnels::PRESTIGE_PERKS
            .iter()
            .find(|perk| {
                !preview.components[0]
                    .buttons
                    .iter()
                    .any(|button| button.custom_id.contains(**perk))
            })
            .expect("an unoffered catalog perk");
        let mut forged_parts = chosen.split(':').map(str::to_owned).collect::<Vec<_>>();
        let perk_index = forged_parts.len() - 2;
        forged_parts[perk_index] = (*forged_perk).to_owned();
        let forged = forged_parts.join(":");
        let forged_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(forged, Vec::new()),
                forged_responder.clone(),
            )
            .await
            .expect("forged offered subset rejected");
        assert!(
            forged_responder.updates.lock().expect("forged updates")[0]
                .content
                .contains("Invalid perk")
        );

        let committed = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(component_request(chosen, Vec::new()), committed.clone())
            .await
            .expect("prestige selection commits");
        let result = committed.updates.lock().expect("prestige update")[0].clone();
        assert!(result.components.is_empty());
        assert_eq!(
            result.embeds[0].title.as_deref(),
            Some("Prestige 1 Complete!")
        );
        {
            let public = discord.public.lock().expect("public ascension responses");
            assert_eq!(public.len(), 1);
            assert_eq!(public[0].content, "*Dig Test Miner has ascended.*");
            assert!(
                public[0].attachments.is_empty(),
                "test config disables Neon"
            );
        }
        let connection = Connection::open(database.path()).expect("verify prestige DB");
        let (depth, level, pickaxe, balance, audits, relics): (i64, i64, i64, i64, i64, i64) =
            connection
                .query_row(
                    "SELECT CAST(t.depth AS INTEGER), CAST(t.prestige_level AS INTEGER),
                            CAST(t.pickaxe_tier AS INTEGER), p.jopacoin_balance,
                            (SELECT COUNT(*) FROM dig_actions a
                              WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                                AND a.action_type='prestige'),
                            (SELECT COUNT(*) FROM dig_artifacts r
                              WHERE r.discord_id=t.discord_id AND r.guild_id=t.guild_id)
                       FROM tunnels t JOIN players p
                         ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                      WHERE t.discord_id=?1 AND t.guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .expect("prestige state");
        assert_eq!((depth, level, pickaxe), (0, 1, 4));
        assert_eq!(balance, 1_150);
        assert_eq!(audits, 1);
        assert_eq!(relics, 1);

        let duplicate = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    preview.components[0].buttons[0].custom_id.clone(),
                    Vec::new(),
                ),
                duplicate.clone(),
            )
            .await
            .expect("duplicate delivery is harmless");
        assert!(
            duplicate.responses.lock().expect("duplicate response")[0]
                .content
                .contains("already made your selection")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1_150
        );
    }

    #[tokio::test]
    async fn live_prestige_provider_attaches_pinnacle_gif_through_temporary_send() {
        let config = ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("dig-provider-neon-test-token".to_owned()),
            "NEON_DEGEN_ENABLED" => Some("true".to_owned()),
            _ => None,
        })
        .expect("Neon-enabled provider test config");
        let (database, provider, discord) =
            fixture_with_discord_and_config(Arc::new(TestDiscord::default()), config);
        let mut progress = serde_json::Map::new();
        for boundary in cama_app::dig_bosses::BOSS_BOUNDARIES {
            progress.insert(
                boundary.to_string(),
                serde_json::Value::String("defeated".to_owned()),
            );
        }
        progress.insert(
            cama_app::dig_bosses::PINNACLE_DEPTH.to_string(),
            serde_json::json!({"status": "defeated", "boss_id": "forgotten_king"}),
        );
        Connection::open(database.path())
            .expect("live Neon fixture DB")
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,depth,max_depth,prestige_level,
                     prestige_perks,boss_progress,current_run_jc,current_run_events,
                     pickaxe_tier,tunnel_name
                 ) VALUES (?1,?2,350,350,0,'[]',?3,50,7,4,'Prestige Tunnel')",
                params![
                    USER as i64,
                    GUILD as i64,
                    serde_json::Value::Object(progress).to_string(),
                ],
            )
            .expect("seed live Neon prestige tunnel");

        // Make the service's production provider call deterministic while
        // retaining the real SeededDigNeonRandom and cooldown implementation.
        {
            let mut neon = provider
                .handler
                .state
                .neon
                .lock()
                .expect("Neon service lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }

        let preview_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(60, "prestige", Vec::new()),
                preview_responder.clone(),
            )
            .await
            .expect("live Neon prestige preview");
        let chosen = preview_responder
            .responses
            .lock()
            .expect("live Neon preview response")[0]
            .components[0]
            .buttons[0]
            .custom_id
            .clone();
        let committed = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(component_request(chosen, Vec::new()), committed.clone())
            .await
            .expect("live Neon prestige selection");

        // The component handler above is the real production path:
        // prestige_neon_response -> dig_neon_response ->
        // DigDiscordPort::dig_send_temporary. TestDiscord retains temporary
        // messages, allowing the attachment and lifecycle boundary to be
        // asserted without a Discord network dependency.
        let public = discord.public.lock().expect("live Neon public responses");
        let neon_response = public
            .iter()
            .find(|response| !response.attachments.is_empty())
            .expect("live prestige path sends a Neon attachment");
        assert_eq!(neon_response.attachments.len(), 1);
        let attachment = &neon_response.attachments[0];
        assert_eq!(attachment.filename, "jopat_terminal.gif");
        let media = cama_app::dig_assets::inspect_media(&attachment.bytes)
            .expect("live provider attachment is valid media");
        assert_eq!(media.format, cama_app::dig_assets::MediaFormat::Gif);
        assert_eq!((media.width, media.height), (320, 180));
        assert_eq!(media.frame_count, 30);
    }

    #[tokio::test]
    async fn boss_neon_live_transport_is_time_limited_and_idempotent() {
        let lifecycle = Arc::new(StdMutex::new(Vec::new()));
        let discord = Arc::new(TestDiscord::default().with_lifecycle(Arc::clone(&lifecycle)));
        let (database, provider, discord) =
            fixture_with_discord_and_config(discord, neon_config("0.30"));
        {
            let mut neon = provider
                .handler
                .state
                .neon
                .lock()
                .expect("Neon service lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }

        // The real provider transport path is used here; the primary result
        // follow-up is accepted before the best-effort temporary Neon hook.
        let responder = TestResponder::with_lifecycle(Arc::clone(&lifecycle));
        responder
            .followup(InteractionResponse::message("primary boss result"))
            .await
            .expect("primary follow-up");
        provider
            .handler
            .post_boss_neon(
                Some(8801),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(DigBossNeonVictory {
                    boss_name: "Grothak".to_owned(),
                    boundary: 100,
                    layer_name: "Stone".to_owned(),
                    jc_delta: 500,
                    gear_drop: false,
                    trophy_relic_drop: false,
                }),
            )
            .await;
        assert_eq!(
            *lifecycle.lock().expect("lifecycle log"),
            vec!["followup", "temporary"]
        );
        {
            let temporary = discord.temporary.lock().expect("temporary Neon sends");
            assert_eq!(temporary.len(), 1);
            assert_eq!(temporary[0].0, CHANNEL as i64);
            assert_eq!(temporary[0].1, Duration::from_secs(60));
            assert_eq!(temporary[0].2.attachments.len(), 1);
        }

        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                USER as i64 + 1,
                "dig-pinnacle-neon-miner",
                Some(GUILD as i64),
            ))
            .expect("register Pinnacle Neon miner");
        {
            let mut neon = provider
                .handler
                .state
                .neon
                .lock()
                .expect("Pinnacle Neon lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }
        provider
            .handler
            .post_boss_neon(
                Some(8804),
                USER as i64 + 1,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(DigBossNeonVictory {
                    boss_name: "The Crowned Hunger".to_owned(),
                    boundary: cama_app::dig_bosses::PINNACLE_DEPTH.into(),
                    layer_name: "The Pinnacle".to_owned(),
                    jc_delta: 500,
                    gear_drop: false,
                    trophy_relic_drop: false,
                }),
            )
            .await;
        {
            let temporary = discord.temporary.lock().expect("Pinnacle temporary send");
            assert_eq!(temporary.len(), 2);
            let pinnacle_media =
                cama_app::dig_assets::inspect_media(&temporary[1].2.attachments[0].bytes)
                    .expect("Pinnacle Neon GIF");
            assert_eq!((pinnacle_media.width, pinnacle_media.height), (320, 180));
        }

        // A retry of the same resolved action is suppressed by the durable
        // one-time claim, even though the provider process is still alive.
        provider
            .handler
            .post_boss_neon(
                Some(8801),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(DigBossNeonVictory {
                    boss_name: "Grothak".to_owned(),
                    boundary: 100,
                    layer_name: "Stone".to_owned(),
                    jc_delta: 500,
                    gear_drop: false,
                    trophy_relic_drop: false,
                }),
            )
            .await;
        assert_eq!(
            discord
                .temporary
                .lock()
                .expect("duplicate Neon sends")
                .len(),
            2
        );
        assert_eq!(
            Connection::open(database.path())
                .expect("Neon claim database")
                .query_row(
                    "SELECT COUNT(*) FROM neon_events
                     WHERE discord_id=?1 AND guild_id=?2 AND event_type=?3",
                    params![USER as i64, GUILD as i64, "dig:8801:boss_neon"],
                    |row| row.get::<_, i64>(0),
                )
                .expect("boss Neon claim"),
            1
        );
    }

    #[tokio::test]
    async fn boss_neon_miss_and_delivery_failure_remain_terminal() {
        let discord = Arc::new(TestDiscord::default());
        let (_database, provider, discord) =
            fixture_with_discord_and_config(discord, neon_config("0.0"));
        {
            let mut neon = provider
                .handler
                .state
                .neon
                .lock()
                .expect("Neon service lock");
            *neon.random_mut() =
                cama_app::dig_neon::SeededDigNeonRandom::new(0x1234_5678_9abc_def0);
        }
        let victory = DigBossNeonVictory {
            boss_name: "Grothak".to_owned(),
            boundary: 100,
            layer_name: "Stone".to_owned(),
            jc_delta: 500,
            gear_drop: false,
            trophy_relic_drop: false,
        };
        provider
            .handler
            .post_boss_neon(
                Some(8802),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(victory.clone()),
            )
            .await;
        assert!(discord.temporary.lock().expect("miss sends").is_empty());
        {
            let mut neon = provider
                .handler
                .state
                .neon
                .lock()
                .expect("Neon service lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }
        provider
            .handler
            .post_boss_neon(
                Some(8802),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(victory.clone()),
            )
            .await;
        assert!(
            discord
                .temporary
                .lock()
                .expect("miss retry sends")
                .is_empty(),
            "miss claim prevents a retry reroll"
        );

        let failed_discord = Arc::new(TestDiscord::default());
        failed_discord.reject_un_nonnced_public_send();
        let (failed_database, failed_provider, failed_discord) =
            fixture_with_discord_and_config(failed_discord, neon_config("0.30"));
        {
            let mut neon = failed_provider
                .handler
                .state
                .neon
                .lock()
                .expect("failed Neon service lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }
        failed_provider
            .handler
            .post_boss_neon(
                Some(8803),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(victory.clone()),
            )
            .await;
        failed_discord.allow_un_nonnced_public_send();
        failed_provider
            .handler
            .post_boss_neon(
                Some(8803),
                USER as i64,
                GUILD as i64,
                Some(CHANNEL as i64),
                Some(victory),
            )
            .await;
        assert!(
            failed_discord
                .temporary
                .lock()
                .expect("failed retry sends")
                .is_empty(),
            "delivery failure remains at-most-once"
        );
        assert_eq!(
            Connection::open(failed_database.path())
                .expect("failed Neon claim database")
                .query_row(
                    "SELECT COUNT(*) FROM neon_events
                     WHERE discord_id=?1 AND guild_id=?2 AND event_type=?3",
                    params![USER as i64, GUILD as i64, "dig:8803:boss_neon"],
                    |row| row.get::<_, i64>(0),
                )
                .expect("failed boss Neon claim"),
            1
        );
    }

    #[tokio::test]
    async fn boss_modal_and_resume_live_path_sends_neon_after_primary_result() {
        let lifecycle = Arc::new(StdMutex::new(Vec::new()));
        let discord = Arc::new(TestDiscord::default().with_lifecycle(Arc::clone(&lifecycle)));
        let (database, provider, discord) =
            fixture_with_discord_and_config(discord, neon_config("0.30"));
        Connection::open(database.path())
            .expect("live boss fixture DB")
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
                  boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
                 VALUES (?1,?2,24,24,0,?3,0,0,50,5,'Live Neon Boss')",
                params![
                    USER as i64,
                    GUILD as i64,
                    r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
                ],
            )
            .expect("live boss tunnel");
        provider.handler.state.boss_entropy.reseed(1);
        {
            let mut neon = provider.handler.state.neon.lock().expect("live Neon lock");
            *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
        }

        let mut response = Arc::new(TestResponder::with_lifecycle(Arc::clone(&lifecycle)));
        provider
            .handler
            .handle(
                modal_request(
                    format!("dig:boss:wager:{USER}:{GUILD}"),
                    [("risk_tier", "cautious"), ("wager", "0")],
                ),
                response.clone(),
            )
            .await
            .expect("live boss modal path");

        // Resolve any authored mechanic prompt through its actual namespaced
        // component route. A terminal result is the only result allowed to
        // reach the Neon side effect.
        for _ in 0..4 {
            let next = response
                .followups
                .lock()
                .expect("boss followups")
                .iter()
                .flat_map(|followup| &followup.components)
                .flat_map(|row| &row.buttons)
                .find(|button| button.custom_id.starts_with("dig:boss:duel:"))
                .map(|button| button.custom_id.clone());
            let Some(next) = next else {
                break;
            };
            response = Arc::new(TestResponder::with_lifecycle(Arc::clone(&lifecycle)));
            provider
                .handler
                .handle(component_request(next, Vec::new()), response.clone())
                .await
                .expect("live boss resume path");
        }

        let temporary = discord.temporary.lock().expect("live boss Neon sends");
        assert_eq!(temporary.len(), 1, "only terminal win emits Neon");
        assert_eq!(temporary[0].1, Duration::from_secs(60));
        assert_eq!(temporary[0].2.attachments.len(), 1);
        let log = lifecycle.lock().expect("live lifecycle ordering");
        let primary = log.iter().position(|event| *event == "followup");
        let neon = log.iter().position(|event| *event == "temporary");
        assert!(primary.is_some_and(|index| neon.is_some_and(|neon| index < neon)));
    }

    #[test]
    fn prestige_view_owner_timeout_transition_and_claim_are_exact() {
        let (_database, provider, _discord) = fixture();
        let token = provider
            .handler
            .create_prestige_view(USER as i64, GUILD as i64, false, 100)
            .expect("create perk-only view");
        assert_eq!(
            provider
                .handler
                .inspect_prestige_view(&token, USER as i64, GUILD as i64, 159)
                .unwrap(),
            DigPrestigeViewAdmission::Admitted
        );
        assert_eq!(
            provider
                .handler
                .inspect_prestige_view(&token, USER as i64 + 1, GUILD as i64, 159)
                .unwrap(),
            DigPrestigeViewAdmission::WrongOwner
        );
        assert_eq!(
            provider
                .handler
                .inspect_prestige_view(&token, USER as i64, GUILD as i64, 160)
                .unwrap(),
            DigPrestigeViewAdmission::Expired
        );

        let mutation_token = provider
            .handler
            .create_prestige_view(USER as i64, GUILD as i64, true, 200)
            .expect("create mutation view");
        assert_eq!(
            provider
                .handler
                .claim_prestige_view(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    Some("fractured_vein"),
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::InvalidTransition
        );
        assert_eq!(
            provider
                .handler
                .select_prestige_mutation(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    "fractured_vein",
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::Admitted
        );
        assert_eq!(
            provider
                .handler
                .select_prestige_mutation(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    "volatile_depths",
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::AlreadyClaimed
        );
        assert_eq!(
            provider
                .handler
                .claim_prestige_view(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    Some("volatile_depths"),
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::InvalidTransition
        );
        assert_eq!(
            provider
                .handler
                .claim_prestige_view(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    Some("fractured_vein"),
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::Admitted
        );
        assert_eq!(
            provider
                .handler
                .claim_prestige_view(
                    &mutation_token,
                    USER as i64,
                    GUILD as i64,
                    Some("fractured_vein"),
                    200,
                )
                .unwrap(),
            DigPrestigeViewAdmission::AlreadyClaimed
        );
    }

    #[test]
    fn rendered_media_keeps_stats_and_event_ui_separate_with_exact_attachments() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|ancestor| ancestor.join("assets/dig"))
            .find(|candidate| candidate.is_dir())
            .expect("repository Dig asset tree");
        let media = DigMediaRuntime::production(&DigRuntimeConfig::with_asset_root(root));
        let result = DigRuntimeOutcome {
            success: true,
            error: None,
            depth_before: 20,
            depth_after: 24,
            advance: 4,
            jc_earned: 6,
            vanity_tax: 0,
            balance_after: 106,
            tunnel_name: "The Media Mine".to_owned(),
            milestone_bonus: 0,
            streak_bonus: 3,
            bankruptcy_penalty: 0,
            luminosity_after: 90,
            luminosity_drained: 10,
            corruption_description: Some("-1 JC this dig".to_owned()),
            mutation_names: vec!["Dark Sight".to_owned()],
            tip: "Keep digging!".to_owned(),
            cave_in: false,
            cave_in_detail: None,
            event_id: Some("underground_stream".to_owned()),
            artifact_id: None,
            boss_boundary: None,
            first_dig: false,
            paid_dig_cost: 0,
            cooldown_remaining: 0,
            paid_dig_available: false,
            items_used: vec!["torch".to_owned(), "dynamite".to_owned()],
            consumed_item_ids: vec![1, 2],
            action_id: Some(91),
            route_choice_required: false,
            pickaxe_tier: 0,
            pet_dig_bonus: 0,
            pet_name: None,
            forced_event_consumed: false,
            relic_trim_notice: false,
            weather: Some(cama_app::dig_runtime::DigRuntimeWeatherInfo {
                name: "Earthworm Migration".to_owned(),
                description: "Worms churn the soil. Digging is easy, but they ate all the coins."
                    .to_owned(),
            }),
        };

        let (stats, event) =
            super::dig_responses(&result, "Media Miner", None, &media, "test-nonce", None);
        let event = event.expect("choice event UI");
        assert!(
            stats.components.is_empty(),
            "stats card has no event controls"
        );
        assert_eq!(stats.attachments.len(), 3);
        assert!(
            stats
                .attachments
                .iter()
                .any(|file| file.filename == "items_used.png")
        );
        assert!(
            stats.embeds[0]
                .thumbnail_url
                .as_deref()
                .is_some_and(|url| url.starts_with("attachment://layer_"))
        );
        assert_eq!(
            stats.embeds[0].title.as_deref(),
            Some("The Media Mine — Depth 24")
        );
        assert!(stats.embeds[0].fields.iter().any(|field| {
            field.name == "Progress" && field.value.starts_with("+4 blocks | +6")
        }));
        assert!(stats.embeds[0].fields.iter().any(|field| {
            field.name == "Luminosity" && field.value == "`[█████████░]` 90% — Bright (-10)"
        }));
        assert!(
            stats.embeds[0]
                .fields
                .iter()
                .any(|field| field.name == "Corruption" && field.value == "-1 JC this dig")
        );
        assert!(
            !stats.embeds[0]
                .fields
                .iter()
                .any(|field| field.name == "Weather"
                    || field.name == "Depth"
                    || field.name == "Layer")
        );
        assert_eq!(
            stats.embeds[0].footer.as_deref(),
            Some("Mutations: Dark Sight | Keep digging!")
        );
        assert_eq!(
            stats.embeds[0].footer_icon_url.as_deref(),
            Some("attachment://pickaxe_wooden.png")
        );
        assert_eq!(
            stats.embeds[0].image_url.as_deref(),
            Some("attachment://items_used.png")
        );
        assert_eq!(event.attachments.len(), 1);
        assert_eq!(event.components.len(), 1);
        assert_eq!(event.components[0].buttons.len(), 2);
        assert!(event.components[0].buttons.iter().all(|button| {
            button
                .custom_id
                .starts_with("dig:event-action:test-nonce:91:")
        }));

        let (retry_stats, retry_event) =
            super::dig_responses(&result, "Media Miner", None, &media, "test-nonce", None);
        assert_eq!(stats.attachments, retry_stats.attachments);
        assert_eq!(
            event.attachments,
            retry_event.expect("retry event").attachments
        );
    }

    #[test]
    fn event_prompt_applies_durable_darkness_and_reading_the_stone_policy() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|ancestor| ancestor.join("assets/dig"))
            .find(|candidate| candidate.is_dir())
            .expect("repository Dig asset tree");
        let media = DigMediaRuntime::production(&DigRuntimeConfig::with_asset_root(root));
        let result = DigRuntimeOutcome {
            success: true,
            error: None,
            depth_before: 20,
            depth_after: 24,
            advance: 4,
            jc_earned: 6,
            vanity_tax: 0,
            balance_after: 106,
            tunnel_name: "The Event Mine".to_owned(),
            milestone_bonus: 0,
            streak_bonus: 0,
            bankruptcy_penalty: 0,
            luminosity_after: 0,
            luminosity_drained: 10,
            corruption_description: None,
            mutation_names: Vec::new(),
            tip: "Keep digging!".to_owned(),
            cave_in: false,
            cave_in_detail: None,
            event_id: Some("underground_stream".to_owned()),
            artifact_id: None,
            boss_boundary: None,
            first_dig: false,
            paid_dig_cost: 0,
            cooldown_remaining: 0,
            paid_dig_available: false,
            items_used: Vec::new(),
            consumed_item_ids: Vec::new(),
            action_id: Some(92),
            route_choice_required: false,
            pickaxe_tier: 0,
            pet_dig_bonus: 0,
            pet_name: None,
            forced_event_consumed: false,
            relic_trim_notice: false,
            weather: None,
        };
        let prompt = cama_app::dig_event_runtime::DigEventActionPresentation {
            event: cama_app::dig_loot::canonical_event_presentation("underground_stream")
                .expect("canonical prompt"),
            luminosity: 0,
            safe_disabled: true,
            reading_the_stone_hint: Some(
                "The stones hum louder beside the bolder path.".to_owned(),
            ),
        };

        let (_, event) = super::dig_responses(
            &result,
            "Dark Miner",
            None,
            &media,
            "test-nonce",
            Some(&prompt),
        );
        let event = event.expect("event prompt");
        assert!(event.embeds[0].fields.iter().any(|field| {
            field
                .value
                .contains("The stones hum louder beside the bolder path.")
        }));
        let safe = event.components[0]
            .buttons
            .iter()
            .find(|button| button.custom_id.ends_with(":safe"))
            .expect("safe button");
        assert_eq!(safe.label, "Darkness consumes safety");
        assert!(safe.disabled);
    }

    #[tokio::test]
    async fn shop_buy_and_inventory_attach_the_canonical_media_contract() {
        let (_database, provider, _discord) = fixture();
        let go = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(go_request(), go)
            .await
            .expect("first dig creates a tunnel");

        let shop = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(command_request(20, "shop", Vec::new()), shop.clone())
            .await
            .expect("shop renders");
        {
            let shop_responses = shop.followups.lock().expect("shop responses");
            assert_eq!(shop_responses.len(), 1);
            assert_eq!(shop_responses[0].attachments.len(), 1);
            assert_eq!(shop_responses[0].attachments[0].filename, "shop_grid.png");
            assert_eq!(
                shop_responses[0].embeds[0].image_url.as_deref(),
                Some("attachment://shop_grid.png")
            );
        }

        let buy = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(
                    21,
                    "buy",
                    vec![InteractionOption {
                        name: "item".to_owned(),
                        value: InteractionValue::String("torch".to_owned()),
                    }],
                ),
                buy.clone(),
            )
            .await
            .expect("buy renders");
        {
            let buy_responses = buy.followups.lock().expect("buy responses");
            assert_eq!(buy_responses.len(), 1);
            assert!(buy_responses[0].ephemeral);
            assert_eq!(buy_responses[0].attachments.len(), 1);
            assert!(
                buy_responses[0].attachments[0]
                    .filename
                    .starts_with("item_torch.")
            );
            assert!(
                buy_responses[0].embeds[0]
                    .thumbnail_url
                    .as_deref()
                    .is_some_and(|url| url.starts_with("attachment://item_torch."))
            );
        }

        let inventory = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(22, "inventory", Vec::new()),
                inventory.clone(),
            )
            .await
            .expect("inventory renders");
        let inventory_responses = inventory.followups.lock().expect("inventory responses");
        assert_eq!(inventory_responses.len(), 1);
        assert_eq!(inventory_responses[0].attachments.len(), 1);
        assert_eq!(
            inventory_responses[0].attachments[0].filename,
            "pickaxe_wooden.png"
        );
        assert_eq!(
            inventory_responses[0].embeds[0].thumbnail_url.as_deref(),
            Some("attachment://pickaxe_wooden.png")
        );
        assert_eq!(inventory_responses[0].embeds[0].color, Some(0x8B_45_13));
        assert_eq!(inventory_responses[0].embeds[0].fields.len(), 1);
        assert_eq!(
            inventory_responses[0].embeds[0].fields[0].name,
            "Torch [QUEUED]"
        );
        assert_eq!(
            inventory_responses[0].embeds[0].fields[0].value,
            "+50 luminosity. Light the way."
        );
    }

    // tests/test_dig_shop.py::test_dig_shop_handler_falls_back_to_ephemeral_when_public_send_fails
    #[tokio::test]
    async fn shop_public_send_falls_back_to_an_ephemeral_response() {
        let (_database, provider, _discord) = fixture();
        let responder = Arc::new(RejectingPublicFollowupResponder::default());
        provider
            .handler
            .handle(command_request(23, "shop", Vec::new()), responder.clone())
            .await
            .expect("ephemeral fallback");
        assert_eq!(responder.defers.lock().unwrap().as_slice(), &[false]);
        let attempts = responder.attempts.lock().unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(!attempts[0].ephemeral);
        assert!(attempts[1].ephemeral);
        assert_eq!(attempts[0].embeds, attempts[1].embeds);
        assert_eq!(attempts[0].attachments, attempts[1].attachments);
    }

    // tests/test_dig_shop.py::test_shop_consumables_field_fits_discord_limit
    #[tokio::test]
    async fn shop_consumables_fields_stay_within_discord_limit_without_dropping_rows() {
        let (database, provider, _discord) = fixture();
        let responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(command_request(230, "shop", Vec::new()), responder.clone())
            .await
            .expect("shop field rendering");
        let attempts = responder.followups.lock().unwrap();
        assert_eq!(attempts.len(), 1);
        assert!(
            attempts[0].embeds[0]
                .fields
                .iter()
                .all(|field| field.value.len() <= 1_024)
        );

        let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
            .shop(USER as i64, GUILD as i64)
            .expect("shop")
            .expect("registered player");
        let rendered = attempts[0].embeds[0]
            .fields
            .iter()
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shop.consumables.iter().all(|item| {
                rendered.contains(&item.name) && rendered.contains(&item.description)
            })
        );
    }

    fn live_buy_autocomplete_choices(query: &str) -> Vec<CommandOptionChoice> {
        let (database, _provider, _discord) = fixture();
        let connection = Connection::open(database.path()).expect("shop DB");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,prestige_level,pickaxe_tier)
                 VALUES (?1,?2,0,275,5,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("shop tunnel");
        let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
            .shop(USER as i64, GUILD as i64)
            .expect("shop")
            .expect("registered player");
        super::dig_buy_choices(query, &shop)
    }

    fn live_buy_autocomplete_values(query: &str) -> BTreeSet<String> {
        live_buy_autocomplete_choices(query)
            .into_iter()
            .map(|choice| match choice {
                CommandOptionChoice::String { name, value } => {
                    assert!(!name.trim().is_empty());
                    value
                }
                CommandOptionChoice::Integer { .. } | CommandOptionChoice::Number { .. } => {
                    panic!("Dig buy choices must be strings")
                }
            })
            .collect()
    }

    // tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[whetstone-expected0]
    #[test]
    fn buy_autocomplete_exposes_tempered_whetstone_sink() {
        assert!(live_buy_autocomplete_values("whetstone").contains("tempered_whetstone"));
    }

    // tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[warding-expected1]
    #[test]
    fn buy_autocomplete_exposes_warding_salts_sink() {
        assert!(live_buy_autocomplete_values("warding").contains("warding_salts"));
    }

    // tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[rescue-expected2]
    #[test]
    fn buy_autocomplete_exposes_rescue_line_sink() {
        assert!(live_buy_autocomplete_values("rescue").contains("rescue_line"));
    }

    // tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[amulet-expected3]
    #[test]
    fn buy_autocomplete_exposes_every_high_tier_amulet_sink() {
        let values = live_buy_autocomplete_values("amulet");
        assert!((4..=7).all(|tier| values.contains(&format!("amulet:{tier}"))));
    }

    // tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[boots-expected4]
    #[test]
    fn buy_autocomplete_exposes_every_high_tier_boots_sink() {
        let values = live_buy_autocomplete_values("boots");
        assert!((4..=7).all(|tier| values.contains(&format!("boots:{tier}"))));
    }

    // tests/test_dig_buy_command.py::test_every_shop_row_is_reachable_through_filtered_autocomplete
    #[test]
    fn live_shop_projection_drives_every_buy_autocomplete_row_with_owned_values() {
        let (database, _provider, _discord) = fixture();
        let connection = Connection::open(database.path()).expect("shop DB");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,prestige_level,pickaxe_tier)
                 VALUES (?1,?2,0,275,5,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("shop tunnel");
        let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
            .shop(USER as i64, GUILD as i64)
            .expect("shop")
            .expect("registered player");
        for item in &shop.consumables {
            assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
                .iter()
                .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &item.id)));
        }
        for item in &shop.pickaxe_upgrades {
            let expected = format!("weapon:{}", item.tier);
            assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
                .iter()
                .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &expected)));
        }
        for item in &shop.gear_for_sale {
            let expected = format!("{}:{}", item.slot.as_str(), item.tier);
            assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
                .iter()
                .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &expected)));
        }
    }

    #[tokio::test]
    async fn provider_buy_weapon_uses_sequential_atomic_upgrade_and_equips_it() {
        let (database, provider, _discord) = fixture();
        provider
            .handler
            .handle(go_request(), Arc::new(TestResponder::default()))
            .await
            .expect("first dig");
        let connection = Connection::open(database.path()).expect("upgrade DB");
        connection
            .execute(
                "UPDATE tunnels SET depth=25,max_depth=25
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
            )
            .expect("upgrade depth");
        let balance_before: i64 = connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .expect("balance");
        drop(connection);

        let responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(
                    24,
                    "buy",
                    vec![InteractionOption {
                        name: "item".to_owned(),
                        value: InteractionValue::String("weapon:1".to_owned()),
                    }],
                ),
                responder.clone(),
            )
            .await
            .expect("weapon buy");
        assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
        let response = responder.followups.lock().unwrap()[0].clone();
        assert!(response.ephemeral);
        assert_eq!(
            response.content,
            format!(
                "Upgraded your pickaxe to **Stone** for **15** {JOPACOIN_EMOTE}. It is equipped."
            )
        );
        let connection = Connection::open(database.path()).expect("verify upgrade DB");
        let state = connection
            .query_row(
                "SELECT p.jopacoin_balance,t.pickaxe_tier,
                        (SELECT tier FROM dig_gear
                          WHERE discord_id=p.discord_id AND guild_id=p.guild_id
                            AND slot='weapon' AND equipped=1)
                   FROM players p JOIN tunnels t
                     ON t.discord_id=p.discord_id AND t.guild_id=p.guild_id
                  WHERE p.discord_id=?1 AND p.guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("upgrade state");
        assert_eq!(state, (balance_before - 15, 1, 1));
    }

    #[tokio::test]
    async fn use_and_gift_autocomplete_are_scoped_to_owned_rows() {
        let (database, provider, _discord) = fixture();
        provider
            .handler
            .handle(go_request(), Arc::new(TestResponder::default()))
            .await
            .expect("first dig");
        let connection = Connection::open(database.path()).expect("owned rows DB");
        connection
            .execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'dynamite',0,10), (?1,?2,'torch',0,11)",
                params![USER as i64, GUILD as i64],
            )
            .expect("owned items");
        connection
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,'mole_claws',10,1,0),
                        (?1,?2,'mole_claws',11,1,0),
                        (?1,?2,'ordinary_art',12,0,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("owned relics");
        drop(connection);

        let responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                autocomplete_request(25, "use", "item", ""),
                responder.clone(),
            )
            .await
            .expect("use autocomplete");
        provider
            .handler
            .handle(
                autocomplete_request(26, "gift", "artifact", "mole"),
                responder.clone(),
            )
            .await
            .expect("gift autocomplete");
        let choices = responder.autocompletes.lock().unwrap();
        let owned_items = choices[0]
            .iter()
            .filter_map(|choice| match choice {
                CommandOptionChoice::String { value, .. } => Some(value.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(owned_items, BTreeSet::from(["dynamite", "torch"]));
        assert_eq!(
            choices[1]
                .iter()
                .filter(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == "mole_claws"))
                .count(),
            2,
            "Python preserves one autocomplete choice per duplicate relic row"
        );
    }

    #[tokio::test]
    async fn command_use_returns_private_embed_with_canonical_item_art() {
        let (database, provider, _discord) = fixture();
        provider
            .handler
            .handle(go_request(), Arc::new(TestResponder::default()))
            .await
            .expect("first dig");
        Connection::open(database.path())
            .expect("use DB")
            .execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'dynamite',0,10)",
                params![USER as i64, GUILD as i64],
            )
            .expect("owned item");
        let responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(
                    27,
                    "use",
                    vec![InteractionOption {
                        name: "item".to_owned(),
                        value: InteractionValue::String("dynamite".to_owned()),
                    }],
                ),
                responder.clone(),
            )
            .await
            .expect("use item");
        assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
        let response = responder.followups.lock().unwrap()[0].clone();
        assert!(response.ephemeral);
        assert_eq!(response.embeds[0].title.as_deref(), Some("Dynamite Queued"));
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some("Ready for your next dig.")
        );
        assert!(
            response.attachments[0]
                .filename
                .starts_with("item_dynamite.")
        );
    }

    #[tokio::test]
    async fn gear_panel_components_use_typed_atomic_service_and_restart_nonce() {
        let (database, provider, _discord) = fixture();
        provider
            .handler
            .handle(go_request(), Arc::new(TestResponder::default()))
            .await
            .expect("first dig");
        let connection = Connection::open(database.path()).expect("gear fixture DB");
        connection
            .execute(
                "UPDATE tunnels SET depth=100,max_depth=100,prestige_level=1
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
            )
            .expect("gear gates");
        connection
            .execute(
                "INSERT INTO dig_gear
                 (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
                 VALUES (?1,?2,'armor',2,5,0,100,'fixture')",
                params![USER as i64, GUILD as i64],
            )
            .expect("armor");
        let armor_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,'mole_claws',100,1,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("relic");
        let relic_id = connection.last_insert_rowid();
        let mut recycle_ids = Vec::new();
        for artifact_id in ["crystal_compass", "obsidian_shield", "mycelium_link"] {
            connection
                .execute(
                    "INSERT INTO dig_artifacts
                     (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                     VALUES (?1,?2,?3,100,1,0)",
                    params![USER as i64, GUILD as i64, artifact_id],
                )
                .expect("recyclable relic");
            recycle_ids.push(connection.last_insert_rowid());
        }

        let panel_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(30, "gear", Vec::new()),
                panel_responder.clone(),
            )
            .await
            .expect("gear panel");
        {
            let responses = panel_responder.responses.lock().expect("panel responses");
            assert_eq!(responses.len(), 1);
            assert_eq!(
                responses[0].embeds[0].title.as_deref(),
                Some("Your Loadout")
            );
            assert_eq!(responses[0].components.len(), 1);
            assert_eq!(responses[0].components[0].buttons.len(), 5);
            assert!(responses[0].components[0].buttons.iter().all(|button| {
                button
                    .custom_id
                    .contains(&provider.handler.state.view_nonce)
            }));
        }

        let open = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    format!(
                        "dig:gear:{}:{}:{}:open:equip:0",
                        provider.handler.state.view_nonce, USER, GUILD
                    ),
                    Vec::new(),
                ),
                open.clone(),
            )
            .await
            .expect("open selector");
        {
            let updates = open.updates.lock().expect("selector update");
            assert_eq!(updates.len(), 1);
            let select = updates[0].components[0]
                .string_select
                .as_ref()
                .expect("gear select");
            assert!(
                select
                    .options
                    .iter()
                    .any(|option| option.value == format!("gear:{armor_id}"))
            );
            assert!(
                select
                    .options
                    .iter()
                    .any(|option| option.value == format!("relic:{relic_id}"))
            );
        }

        let equip = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    format!(
                        "dig:gear:{}:{}:{}:select:equip:0",
                        provider.handler.state.view_nonce, USER, GUILD
                    ),
                    vec![format!("gear:{armor_id}")],
                ),
                equip.clone(),
            )
            .await
            .expect("equip");
        assert_eq!(equip.updates.lock().expect("equip update").len(), 1);
        assert!(equip.followups.lock().expect("equip followup")[0].ephemeral);
        assert_eq!(
            Connection::open(database.path())
                .expect("verify DB")
                .query_row(
                    "SELECT equipped FROM dig_gear WHERE id=?1",
                    [armor_id],
                    |row| row.get::<_, i64>(0)
                )
                .expect("equipped"),
            1
        );

        let open_recycle = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    format!(
                        "dig:gear:{}:{}:{}:open:recycle:0",
                        provider.handler.state.view_nonce, USER, GUILD
                    ),
                    Vec::new(),
                ),
                open_recycle.clone(),
            )
            .await
            .expect("open recycle selector");
        {
            let updates = open_recycle.updates.lock().expect("recycle update");
            assert_eq!(updates.len(), 1);
            let select = updates[0].components[0]
                .string_select
                .as_ref()
                .expect("recycle select");
            assert_eq!(select.min_values, 3);
            assert_eq!(select.max_values, 3);
            assert_eq!(select.options.len(), 3);
            assert!(
                select
                    .options
                    .iter()
                    .all(|option| option.label.starts_with("[Common]"))
            );
        }

        let recycle = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    format!(
                        "dig:gear:{}:{}:{}:recycle",
                        provider.handler.state.view_nonce, USER, GUILD
                    ),
                    recycle_ids.iter().map(i64::to_string).collect(),
                ),
                recycle.clone(),
            )
            .await
            .expect("recycle relics");
        assert_eq!(recycle.updates.lock().expect("recycle refresh").len(), 1);
        assert!(
            recycle.followups.lock().expect("recycle followup")[0]
                .content
                .contains("Recycled **3 Common** relics")
        );
        let verification = Connection::open(database.path()).expect("recycle verification DB");
        for row_id in recycle_ids {
            assert_eq!(
                verification
                    .query_row(
                        "SELECT COUNT(*) FROM dig_artifacts WHERE id=?1",
                        [row_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("recycled source count"),
                0
            );
        }
        assert_eq!(
            verification
                .query_row(
                    "SELECT COUNT(*) FROM dig_artifacts
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("relic count after recycle"),
            2
        );

        let stale = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(
                    format!("dig:gear:old-process:{}:{}:open:equip:0", USER, GUILD),
                    Vec::new(),
                ),
                stale.clone(),
            )
            .await
            .expect("stale recovery");
        assert!(
            stale.responses.lock().expect("stale responses")[0]
                .content
                .contains("expired after a restart")
        );
    }

    #[tokio::test]
    async fn provider_help_and_gift_use_typed_social_service_and_python_delivery_contract() {
        const TARGET: u64 = 77_004;
        let (database, provider, _discord) = fixture();
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(
                TARGET as i64,
                "dig-social-target",
                Some(GUILD as i64),
            ))
            .expect("registered social target");
        let connection = Connection::open(database.path()).expect("social provider fixture DB");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python legacy foreign-key behavior");
        connection
            .execute(
                "INSERT INTO tunnels
                    (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,boss_progress)
                 VALUES (?1,?2,'Target Descent',10,10,0,'{}')",
                params![TARGET as i64, GUILD as i64],
            )
            .expect("target tunnel");

        let user_option = InteractionOption {
            name: "user".to_owned(),
            value: InteractionValue::User {
                id: TARGET,
                display_name: Some("Social Target".to_owned()),
                is_bot: Some(false),
            },
        };
        let help = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(20, "help", vec![user_option.clone()]),
                help.clone(),
            )
            .await
            .expect("typed help provider path");
        assert_eq!(
            help.defers.lock().expect("help defers").as_slice(),
            &[false]
        );
        {
            let help_followups = help.followups.lock().expect("help followups");
            assert_eq!(help_followups.len(), 1);
            assert!(!help_followups[0].ephemeral);
            assert_eq!(
                help_followups[0].embeds[0].title.as_deref(),
                Some("Tunnel Assistance")
            );
            let help_description = help_followups[0].embeds[0]
                .description
                .as_deref()
                .expect("help description");
            assert!(help_description.contains("You helped **Social Target**'s tunnel!"));
            assert!(help_description.contains("Blocks added: **"));
        }
        assert_eq!(
            Connection::open(database.path())
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions
                     WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3 AND action_type='help'",
                    params![GUILD as i64, USER as i64, TARGET as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        Connection::open(database.path())
            .unwrap()
            .execute(
                "INSERT INTO dig_artifacts
                    (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,'mole_claws',100,1,1)",
                params![USER as i64, GUILD as i64],
            )
            .expect("gift relic fixture");
        let gift_options = vec![
            user_option,
            InteractionOption {
                name: "artifact".to_owned(),
                value: InteractionValue::String("mole_claws".to_owned()),
            },
        ];
        let gift = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(21, "gift", gift_options.clone()),
                gift.clone(),
            )
            .await
            .expect("typed gift provider path");
        assert_eq!(
            gift.defers.lock().expect("gift defers").as_slice(),
            &[false]
        );
        {
            let gift_followups = gift.followups.lock().expect("gift followups");
            assert_eq!(gift_followups.len(), 1);
            assert!(!gift_followups[0].ephemeral);
            assert_eq!(
                gift_followups[0].content,
                "You gifted **Mole Claws** to **Social Target**!"
            );
        }
        assert_eq!(
            Connection::open(database.path())
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM dig_artifacts
                     WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='mole_claws'",
                    params![TARGET as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let failed = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(command_request(22, "gift", gift_options), failed.clone())
            .await
            .expect("failed gift is a typed user response");
        let failed_followups = failed.followups.lock().expect("failed gift followups");
        assert_eq!(failed_followups.len(), 1);
        assert!(failed_followups[0].ephemeral);
        assert_eq!(failed_followups[0].content, "You don't have that artifact.");
    }

    #[tokio::test]
    async fn provider_sabotage_view_is_owner_bound_restart_expiring_and_exactly_once() {
        const TARGET: u64 = 77_004;
        const COPIED_ACTOR: u64 = 77_005;
        let (database, provider, _discord) = fixture();
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(
                TARGET as i64,
                "dig-sabotage-target",
                Some(GUILD as i64),
            ))
            .expect("registered sabotage target");
        players
            .add(&NewPlayer::new(
                COPIED_ACTOR as i64,
                "dig-sabotage-copy",
                Some(GUILD as i64),
            ))
            .expect("registered copied-view actor");
        let connection = Connection::open(database.path()).expect("sabotage provider fixture DB");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET as i64, GUILD as i64],
            )
            .expect("zero target balance for conservation assertion");
        connection
            .execute(
                "INSERT INTO tunnels
                    (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,boss_progress)
                 VALUES (?1,?2,'Attacker Descent',20,20,0,'{}')",
                params![USER as i64, GUILD as i64],
            )
            .expect("attacker tunnel");
        connection
            .execute(
                "INSERT INTO tunnels
                    (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                     boss_progress,trap_active)
                 VALUES (?1,?2,'Target Descent',100,100,0,'{}',1)",
                params![TARGET as i64, GUILD as i64],
            )
            .expect("trapped target tunnel");

        let preview_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                command_request(
                    23,
                    "sabotage",
                    vec![InteractionOption {
                        name: "user".to_owned(),
                        value: InteractionValue::User {
                            id: TARGET,
                            display_name: Some("Sabotage Target".to_owned()),
                            is_bot: Some(false),
                        },
                    }],
                ),
                preview_responder.clone(),
            )
            .await
            .expect("typed sabotage preview");
        let preview = preview_responder.responses.lock().unwrap()[0].clone();
        assert!(!preview.ephemeral);
        assert_eq!(preview.embeds[0].title.as_deref(), Some("Confirm Sabotage"));
        assert_eq!(
            preview.embeds[0]
                .description
                .as_deref()
                .expect("sabotage preview description"),
            format!(
                "**Target:** Sabotage Target\n**Cost:** 20 {JOPACOIN_EMOTE}\n**Potential damage:** 3-8 blocks\n\nAre you sure? If they have a trap set, you could take damage instead."
            )
        );
        assert_eq!(preview.components[0].buttons.len(), 2);
        let confirm_id = preview.components[0].buttons[0].custom_id.clone();
        assert!(confirm_id.starts_with("dig:sabotage:confirm:"));

        let copied = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request_as(COPIED_ACTOR, "Copied Actor", &confirm_id, Vec::new()),
                copied.clone(),
            )
            .await
            .expect("copied sabotage view rejected");
        assert_eq!(
            copied.responses.lock().unwrap()[0].content,
            "This isn't your sabotage."
        );

        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            None,
            Arc::clone(&provider.handler.state.media),
        );
        let expired = Arc::new(TestResponder::default());
        restarted
            .handler
            .handle(component_request(&confirm_id, Vec::new()), expired.clone())
            .await
            .expect("restart expires process-local sabotage view");
        assert_eq!(
            expired.responses.lock().unwrap()[0].content,
            "This sabotage expired. Use `/dig sabotage` again."
        );

        let committed = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(&confirm_id, Vec::new()),
                committed.clone(),
            )
            .await
            .expect("trapped sabotage settlement");
        let update = committed.updates.lock().unwrap()[0].clone();
        assert_eq!(update.embeds[0].title.as_deref(), Some("Trap Triggered!"));
        assert!(
            update.embeds[0]
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.starts_with("Your sabotage attempt backfired!\nTrap triggered!")
                        && description.contains("You lost 40 JC")
                        && description.contains("blocks!")
                })
        );
        assert!(update.components.is_empty());

        let state = Connection::open(database.path()).expect("verify sabotage settlement");
        assert_eq!(
            state
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            460
        );
        let target_balance = state
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let sabotage_detail: String = state
            .query_row(
                "SELECT detail FROM dig_actions
                 WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
                   AND action_type='sabotage'",
                params![GUILD as i64, USER as i64, TARGET as i64],
                |row| row.get(0),
            )
            .unwrap();
        let victim_tip = serde_json::from_str::<serde_json::Value>(&sabotage_detail)
            .expect("typed sabotage audit detail")["victim_tip"]
            .as_i64()
            .expect("victim tip audit value");
        assert_eq!(target_balance, 20 + victim_tip);
        let (actor_depth, trap_active, action_count): (i64, i64, i64) = (
            state
                .query_row(
                    "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get(0),
                )
                .unwrap(),
            state
                .query_row(
                    "SELECT trap_active FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![TARGET as i64, GUILD as i64],
                    |row| row.get(0),
                )
                .unwrap(),
            state
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions
                     WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
                       AND action_type='sabotage'",
                    params![GUILD as i64, USER as i64, TARGET as i64],
                    |row| row.get(0),
                )
                .unwrap(),
        );
        assert!((15..=17).contains(&actor_depth));
        assert_eq!(trap_active, 0);
        assert_eq!(action_count, 1);

        let duplicate = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                component_request(&confirm_id, Vec::new()),
                duplicate.clone(),
            )
            .await
            .expect("duplicate sabotage component is admitted as a user error");
        assert_eq!(
            duplicate.responses.lock().unwrap()[0].content,
            "This sabotage was already resolved."
        );
        assert_eq!(
            state
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions
                     WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
                       AND action_type='sabotage'",
                    params![GUILD as i64, USER as i64, TARGET as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn sabotage_result_embed_keeps_prediction_contract_detail() {
        let response = super::sabotage_result_response(
            &cama_app::dig_social_runtime::DigSabotageResult {
                cost: 20,
                damage: 7,
                target_tunnel: "Target Descent".to_owned(),
                target_depth_after: 93,
                trap_triggered: false,
                trap_detail: None,
                sabotage_hit: true,
                clue: None,
                is_reveal: false,
                insurance_applied: false,
                damage_reduced: false,
                absorbed_by_aegis: false,
                protection_source: None,
                victim_tip: 0,
                mana_steal_jc: 0,
                attacker_block_reward: 5,
                vendetta_reflect: 0,
                vendetta_bonus: 0,
                prediction_contract_steal: Some(
                    cama_app::dig_social_runtime::DigPredictionContractSteal {
                        prediction_id: 42,
                        side: "yes",
                        contracts: 3,
                    },
                ),
                action_id: 9,
            },
            "Sabotage Target",
        );
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some(
                "You sabotaged **Sabotage Target**'s tunnel!\nDamage dealt: **7** blocks\nStole **3 YES** prediction contracts from market **#42**."
            )
        );
    }

    #[tokio::test]
    async fn provider_miner_group_uses_typed_profile_allocation_respec_and_autobuy_service() {
        let (database, provider, _discord) = fixture();

        let profile_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                grouped_command_request(24, "miner", "profile", Vec::new()),
                profile_responder.clone(),
            )
            .await
            .expect("typed miner profile");
        assert_eq!(profile_responder.defers.lock().unwrap().as_slice(), &[true]);
        let profile = profile_responder.followups.lock().unwrap()[0].clone();
        assert!(profile.ephemeral);
        assert_eq!(
            profile.embeds[0].title.as_deref(),
            Some("Dig Test Miner - Miner Profile")
        );
        assert_eq!(
            profile.embeds[0].description.as_deref(),
            Some("Backstory not set.")
        );
        assert_eq!(
            profile.embeds[0].fields[0].value,
            "Strength **0** | Smarts **0** | Stamina **0**\nPoints: **5** total, **5** unspent\nEffects: +0/+0 advance range, -0% cave-in, -0% cooldown/paid costs"
        );
        assert_eq!(
            profile.embeds[0].footer.as_deref(),
            Some("Backstory locks after you set it. Boss first clears grant one extra S point.")
        );

        let about_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                grouped_command_request(
                    25,
                    "miner",
                    "about",
                    vec![InteractionOption {
                        name: "backstory".to_owned(),
                        value: InteractionValue::String(
                            " Former  cartographer @everyone ".to_owned(),
                        ),
                    }],
                ),
                about_responder.clone(),
            )
            .await
            .expect("typed miner backstory");
        let about = about_responder.responses.lock().unwrap()[0].clone();
        assert!(about.ephemeral);
        assert_eq!(
            about.embeds[0].title.as_deref(),
            Some("Backstory Locked In")
        );
        assert_eq!(
            about.embeds[0].description.as_deref(),
            Some("Former cartographer (at)everyone")
        );
        assert_eq!(
            about.embeds[0].footer.as_deref(),
            Some("This cannot be changed later.")
        );

        let build_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                grouped_command_request(
                    26,
                    "miner",
                    "build",
                    vec![
                        InteractionOption {
                            name: "strength".to_owned(),
                            value: InteractionValue::Integer(2),
                        },
                        InteractionOption {
                            name: "smarts".to_owned(),
                            value: InteractionValue::Integer(2),
                        },
                        InteractionOption {
                            name: "stamina".to_owned(),
                            value: InteractionValue::Integer(1),
                        },
                    ],
                ),
                build_responder.clone(),
            )
            .await
            .expect("typed miner allocation");
        assert_eq!(build_responder.defers.lock().unwrap().as_slice(), &[true]);
        let build = build_responder.followups.lock().unwrap()[0].clone();
        assert_eq!(build.embeds[0].title.as_deref(), Some("S Points Spent"));
        assert_eq!(
            build.embeds[0].description.as_deref(),
            Some(
                "Strength **2** | Smarts **2** | Stamina **1**\nPoints: **5** total, **0** unspent\nEffects: +0/+1 advance range, -4% cave-in, -4% cooldown/paid costs"
            )
        );

        let respec_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                grouped_command_request(27, "miner", "respec", Vec::new()),
                respec_responder.clone(),
            )
            .await
            .expect("typed miner respec");
        let respec = respec_responder.followups.lock().unwrap()[0].clone();
        assert_eq!(respec.embeds[0].title.as_deref(), Some("S Points Reset"));
        assert_eq!(
            respec.embeds[0].description.as_deref(),
            Some("Returned **5** allocated S points. You now have **5** unspent S points.")
        );
        assert_eq!(
            respec.embeds[0].footer.as_deref(),
            Some("50 JC spent on the respec.")
        );
        assert_eq!(
            Connection::open(database.path())
                .unwrap()
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![USER as i64, GUILD as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            450
        );

        let autobuy_responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                grouped_command_request(
                    28,
                    "miner",
                    "autobuy",
                    vec![
                        InteractionOption {
                            name: "item".to_owned(),
                            value: InteractionValue::String("both".to_owned()),
                        },
                        InteractionOption {
                            name: "enabled".to_owned(),
                            value: InteractionValue::Boolean(true),
                        },
                    ],
                ),
                autobuy_responder.clone(),
            )
            .await
            .expect("typed miner auto-buy");
        let autobuy = autobuy_responder.followups.lock().unwrap()[0].clone();
        assert_eq!(
            autobuy.embeds[0].title.as_deref(),
            Some("Dig Auto-Buy Updated")
        );
        assert_eq!(
            autobuy.embeds[0].description.as_deref(),
            Some("Torch: **ON**\nHard Hat: **ON**")
        );
        assert_eq!(
            autobuy.embeds[0].footer.as_deref(),
            Some("Auto-buy spends JC only when an actual dig starts.")
        );
        let settings: (i64, i64) = Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT auto_buy_torch,auto_buy_hard_hat FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(settings, (1, 1));
    }

    #[tokio::test]
    async fn provider_registration_channel_gate_and_autocomplete_are_live() {
        let (database, provider, _discord) = fixture();
        let connection = Connection::open(database.path()).expect("autocomplete fixture DB");
        connection
            .execute(
                "INSERT INTO tunnels(discord_id,guild_id,depth,max_depth,pickaxe_tier)
                 VALUES (?1,?2,0,0,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("autocomplete tunnel");
        connection
            .execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'hard_hat',0,1)",
                params![USER as i64, GUILD as i64],
            )
            .expect("owned autocomplete item");
        connection
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,'mole_claws',1,1,0)",
                params![USER as i64, GUILD as i64],
            )
            .expect("owned autocomplete relic");
        let mut registry = crate::registration::RegistryBuilder::default();
        provider
            .register(&mut registry)
            .expect("Dig command and component route register");
        let registry = registry.build();
        assert_eq!(registry.commands().count(), 1);
        assert_eq!(
            registry
                .component_routes()
                .iter()
                .map(|route| route.custom_id_prefix.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["dig:", "dig_route_", "duel_opt_"])
        );

        let responder = Arc::new(TestResponder::default());
        provider
            .handler
            .handle(
                autocomplete_request(5, "use", "item", "hard"),
                responder.clone(),
            )
            .await
            .expect("item autocomplete should respond");
        provider
            .handler
            .handle(
                autocomplete_request(6, "buy", "item", "stone"),
                responder.clone(),
            )
            .await
            .expect("buy autocomplete should respond");
        provider
            .handler
            .handle(
                autocomplete_request(7, "gift", "artifact", "mole"),
                responder.clone(),
            )
            .await
            .expect("artifact autocomplete should respond");
        {
            let choices = responder
                .autocompletes
                .lock()
                .expect("autocomplete choices");
            assert_eq!(choices.len(), 3);
            assert!(choices[0].iter().any(|choice| matches!(
                choice,
                CommandOptionChoice::String { value, .. } if value == "hard_hat"
            )));
            assert!(choices[1].iter().any(|choice| matches!(
                choice,
                CommandOptionChoice::String { value, .. } if value == "weapon:1"
            )));
            assert!(choices[2].iter().any(|choice| matches!(
                choice,
                CommandOptionChoice::String { value, .. } if value == "mole_claws"
            )));
        }

        let balance_before: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .expect("balance before gate");
        // The transport adapter's false result must debit exactly one JC
        // through the app service before it rejects the command.
        let gated_discord = Arc::new(TestDiscord {
            gamba: false,
            avatar_url: None,
            ..TestDiscord::default()
        });
        let gated_provider =
            DigRegistrationProvider::new(database.path(), &config(), gated_discord, None);
        let gate_responder = Arc::new(TestResponder::default());
        gated_provider
            .handler
            .handle(
                command_request(6, "guide", Vec::new()),
                gate_responder.clone(),
            )
            .await
            .expect("wrong-channel admission should be handled");
        let gate_response = gate_responder.responses.lock().expect("gate response");
        assert_eq!(gate_response.len(), 1);
        assert!(gate_response[0].content.contains("not consecrated"));
        drop(gate_response);
        let balance_after: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .expect("balance after gate");
        assert_eq!(balance_after, balance_before - 1);
    }

    fn find<'a>(options: &'a [CommandOptionSpec], name: &str) -> &'a CommandOptionSpec {
        options
            .iter()
            .find(|option| option.name == name)
            .unwrap_or_else(|| panic!("missing Dig option {name}"))
    }

    #[test]
    fn command_tree_matches_python_surface() {
        let options = dig_options();
        assert_eq!(options.len(), 22);
        for (name, description) in [
            ("go", "Dig deeper into your tunnel"),
            ("help", "Help another player's tunnel"),
            ("sabotage", "Sabotage another player's tunnel"),
            ("info", "View tunnel information"),
            ("leaderboard", "View top tunnels"),
            (
                "halloffame",
                "View the hall of fame (best prestige run scores)",
            ),
            ("use", "Queue a consumable for your next dig"),
            ("gift", "Gift a relic to another player"),
            ("shop", "Browse the mining shop"),
            ("buy", "Buy an item from the mining shop"),
            ("flex", "Show off your mining stats"),
            (
                "prestige",
                "Prestige your tunnel (reset depth, gain a perk)",
            ),
            ("abandon", "Abandon your tunnel (partial refund)"),
            ("trap", "Set a trap in your tunnel"),
            ("insure", "Buy cave-in insurance"),
            ("inventory", "View your mining inventory"),
            ("artifacts", "View all artifacts and the ones you own"),
            ("gear", "Manage your boss-combat gear"),
            ("weather", "View today's layer weather conditions"),
            ("guide", "Learn how to dig"),
        ] {
            assert_eq!(find(&options, name).description, description);
        }

        let admin = find(&options, "admin");
        assert_eq!(admin.kind, CommandOptionKind::SubcommandGroup);
        for (name, description, user_description) in [
            (
                "resetcooldown",
                "Reset a player's free dig cooldown (Admin only)",
                "The player whose cooldown to reset",
            ),
            (
                "forceevent",
                "Force next dig to trigger an event (Admin only)",
                "The player whose next dig gets an event",
            ),
            (
                "setdepth",
                "Set a player's tunnel depth (Admin only)",
                "The player",
            ),
        ] {
            let command = find(&admin.options, name);
            assert_eq!(command.description, description);
            let user = find(&command.options, "user");
            assert_eq!(user.kind, CommandOptionKind::User);
            assert!(user.required);
            assert_eq!(user.description, user_description);
        }
        let setdepth = find(&admin.options, "setdepth");
        assert_eq!(
            find(&setdepth.options, "depth").kind,
            CommandOptionKind::Integer
        );

        let miner = find(&options, "miner");
        assert_eq!(miner.kind, CommandOptionKind::SubcommandGroup);
        for (name, description) in [
            ("profile", "View your miner profile and S stats"),
            ("about", "Set your miner backstory once"),
            (
                "build",
                "Spend unallocated points on Strength, Smarts, and Stamina",
            ),
            ("respec", "Reset your allocated S points for 50 JC"),
            ("autobuy", "Auto-buy Torch and/or Hard Hat for each dig"),
        ] {
            assert_eq!(find(&miner.options, name).description, description);
        }
        let about = find(&miner.options, "about");
        let backstory = find(&about.options, "backstory");
        assert_eq!(
            backstory.description,
            "Short backstory blurb for the AI Dungeon Master"
        );
        assert_eq!(backstory.max_length, Some(500));

        let build = find(&miner.options, "build");
        for (name, description) in [
            (
                "strength",
                "Points to add. Increases how far you dig each action.",
            ),
            (
                "smarts",
                "Points to add. Helps you read the stone and avoid collapses.",
            ),
            (
                "stamina",
                "Points to add. Keeps you digging longer between rests.",
            ),
        ] {
            let stat = find(&build.options, name);
            assert_eq!(stat.kind, CommandOptionKind::Integer);
            assert_eq!(stat.description, description);
        }
        let autobuy = find(&miner.options, "autobuy");
        let item = find(&autobuy.options, "item");
        assert_eq!(item.choices.len(), 3);
        assert_eq!(
            item.choices,
            vec![
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
            ]
        );
        assert_eq!(
            find(&autobuy.options, "enabled").description,
            "Whether to auto-buy this item on each real dig"
        );

        for (name, option_name) in [("help", "user"), ("sabotage", "user"), ("gift", "user")] {
            assert_eq!(
                find(&find(&options, name).options, option_name).kind,
                CommandOptionKind::User
            );
        }
        assert!(find(&find(&options, "use").options, "item").autocomplete);
        assert!(find(&find(&options, "gift").options, "artifact").autocomplete);
        assert!(find(&find(&options, "buy").options, "item").autocomplete);
    }
}
