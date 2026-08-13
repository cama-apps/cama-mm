//! Production-supervised Camagotchi lifecycle sweep.
//!
//! SQLite and pet-card rendering are synchronous by design. This adapter
//! keeps both on Tokio's blocking pool, returns fully owned notices/media, and
//! never holds either service across a Discord await.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::{
    AIService, Message, MessageRole, ToolChoice, ToolDefinition, ToolProperty, ToolPropertySchema,
    ToolRequest, Value as AiValue,
};
use cama_app::pet::{SeededPetRandom, SweepOutcome, SystemPetClock};
use cama_app::pet_assets::{
    EvolutionVisual, FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
};
use cama_app::pet_commands::{
    Embed, EmbedColor, PetCommandService, build_death_embed, build_eating_outcome_embed,
    build_evolution_embed, build_hatch_embed, build_refund_embed,
};
use cama_app::pet_flavor::{
    BUNDLE_TOOL_NAME, FlavorClock, FlavorDataPort, FlavorRng, GuildAiPort, LINE_TOOL_NAME,
    LedgerEntry, LlmPort, LlmRequest, PetFlavorEvent, PetFlavorService,
    ToolCallResult as PetFlavorToolCallResult, ToolValue, fallbacks,
};
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::core_repositories::PlayerRepository;
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::pet_brawl_repository::PetBrawlRepository;
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::pet::{
    DeathNotice, EvolutionNotice, HatchNotice, PET_BRAWL_ACTIVE_TTL_SECONDS, Pet, PetMood,
    PetStage, RefundNotice,
};
use cama_domain::pet_evolution::{PetCalling, PetInstinct};
use chrono::Utc;
use rusqlite::{Connection, params};
use tracing::warn;

use crate::SerenityDiscordTransport;
use crate::application_config::ApplicationConfig;
use crate::discord_transport::DiscordMessage;
use crate::pet_death_delivery::is_active as direct_death_delivery_active;
use crate::registration::{InteractionAttachment, InteractionEmbed, InteractionResponse};
use crate::reminder_provider::ReminderHooks;
use crate::worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const PET_SWEEP_WORKER_NAME: &str = "pet_sweep";
pub const PET_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(10 * 60);

type ProductionPetService = SqlitePetCommandService<SeededPetRandom, SystemPetClock>;
type ProductionPetAssets = PetAssetLoader<FilesystemPetAssets, HybridPetRenderer>;

#[derive(Clone)]
struct ProductionPetFlavorLlm(Arc<AIService>);

impl LlmPort for ProductionPetFlavorLlm {
    fn call_with_tools(&self, request: LlmRequest) -> Result<PetFlavorToolCallResult, String> {
        let definition = pet_flavor_tool(request.tool_name)?;
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    other => return Err(format!("unsupported pet flavor message role {other}")),
                };
                Ok(Message::new(role, message.content))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let max_tokens = u32::try_from(request.max_tokens)
            .map_err(|_| format!("invalid pet flavor max token count {}", request.max_tokens))?;
        let result = self.0.call_with_tools(ToolRequest {
            messages,
            tools: vec![definition],
            tool_choice: ToolChoice::Required(request.tool_name.to_owned()),
            max_tokens: Some(max_tokens),
            temperature: Some(request.temperature),
            feature: request.feature,
        });
        let tool_name = result
            .tool_name
            .ok_or_else(|| "pet flavor provider returned no tool call".to_owned())?;
        Ok(PetFlavorToolCallResult {
            tool_name,
            tool_args: ToolValue::Object(
                result
                    .tool_args
                    .into_iter()
                    .map(|(key, value)| (key, pet_tool_value(value)))
                    .collect(),
            ),
        })
    }
}

#[derive(Clone)]
struct ProductionPetGuildAi(GuildConfigRepository);

impl GuildAiPort for ProductionPetGuildAi {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        self.0
            .get_ai_enabled(guild_id)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct ProductionPetFlavorData {
    database_path: PathBuf,
    players: PlayerRepository,
}

impl ProductionPetFlavorData {
    fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            players: PlayerRepository::new(database_path),
        }
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection =
            Connection::open(&self.database_path).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }
}

impl FlavorDataPort for ProductionPetFlavorData {
    fn balance(&self, discord_id: i64, guild_id: i64) -> Result<i64, String> {
        self.players
            .get_by_id(discord_id, Some(guild_id))
            .map_err(|error| error.to_string())?
            .map(|player| player.jopacoin_balance)
            .ok_or_else(|| "pet flavor player was not found".to_owned())
    }

    fn recent_entries(
        &self,
        guild_id: i64,
        discord_id: i64,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, String> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT CAST(delta AS TEXT),source,COALESCE(reason,''),COALESCE(metadata,'')
                 FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                 ORDER BY created_at DESC,ledger_id DESC LIMIT ?3",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(params![guild_id, discord_id, limit], |row| {
                Ok(LedgerEntry {
                    delta: row.get(0)?,
                    source: row.get(1)?,
                    reason: row.get(2)?,
                    metadata: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }
}

struct SystemPetFlavorClock;

impl FlavorClock for SystemPetFlavorClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

struct ProductionPetFlavorRng;

impl FlavorRng for ProductionPetFlavorRng {
    fn choose_index(&mut self, len: usize) -> usize {
        if len == 0 { 0 } else { fastrand::usize(..len) }
    }
}

#[async_trait]
trait PetSweepFlavorPort: Send + Sync {
    async fn generate(&self, event: PetFlavorEvent, pet: Pet) -> String;
}

struct ProductionPetFlavorRuntime {
    service: Arc<PetFlavorService>,
}

impl ProductionPetFlavorRuntime {
    fn new(
        database_path: impl AsRef<Path>,
        ai_service: Option<Arc<AIService>>,
        ai_features_default: bool,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let ai =
            ai_service.map(|service| Arc::new(ProductionPetFlavorLlm(service)) as Arc<dyn LlmPort>);
        Self {
            service: Arc::new(PetFlavorService::new(
                ai,
                Some(Arc::new(ProductionPetGuildAi(GuildConfigRepository::new(
                    &database_path,
                    ai_features_default,
                )))),
                Some(Arc::new(ProductionPetFlavorData::new(&database_path))),
                Arc::new(SystemPetFlavorClock),
                Box::new(ProductionPetFlavorRng),
            )),
        }
    }
}

#[async_trait]
impl PetSweepFlavorPort for ProductionPetFlavorRuntime {
    async fn generate(&self, event: PetFlavorEvent, pet: Pet) -> String {
        let service = Arc::clone(&self.service);
        let fallback_pet = pet.clone();
        match tokio::task::spawn_blocking(move || service.generate(event, &pet, None)).await {
            Ok(line) => line,
            Err(error) => {
                warn!(%error, "pet flavor blocking task failed; using fallback");
                fallback_lifecycle_flavor(event, &fallback_pet)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetSweepDeliveryError {
    Forbidden,
    Transient(String),
}

/// Cache/HTTP boundary used by the worker. Channel discovery is deliberately
/// cache-only and guild-scoped, matching `bot.get_guild().get_channel()`.
#[async_trait]
pub trait PetSweepDiscordPort: Send + Sync {
    async fn cached_text_channel(
        &self,
        guild_id: i64,
        configured_channel_id: i64,
    ) -> Result<Option<i64>, String>;

    async fn send_channel(
        &self,
        channel_id: i64,
        message: DiscordMessage,
    ) -> Result<(), PetSweepDeliveryError>;

    async fn send_dm(&self, user_id: i64, message: DiscordMessage) -> Result<(), String>;
}

#[async_trait]
trait PetSweepReminderPort: Send + Sync {
    async fn pet_enabled(&self, user_id: i64, guild_id: i64) -> Result<bool, String>;
    async fn rearm_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String>;
    async fn cancel_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String>;
}

#[async_trait]
impl PetSweepReminderPort for ReminderHooks {
    async fn pet_enabled(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        ReminderHooks::pet_enabled(self, user_id, guild_id).await
    }

    async fn rearm_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        ReminderHooks::rearm_pet(self, user_id, guild_id).await
    }

    async fn cancel_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        self.cancel_pet_async(user_id, guild_id).await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedPetMedia {
    filename: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PetMediaKind {
    Hatch,
    Evolution,
    Death,
}

#[derive(Debug)]
enum NoticeFailure {
    Cancelled,
    Failed(String),
}

impl From<String> for NoticeFailure {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

/// Async runtime adapter for the synchronous application sweep policy.
pub struct PetSweepWorker {
    database_path: PathBuf,
    configured_channel_id: i64,
    decay_per_day: i64,
    discord: Arc<dyn PetSweepDiscordPort>,
    reminders: Arc<dyn PetSweepReminderPort>,
    flavor: Arc<dyn PetSweepFlavorPort>,
    media: Arc<Mutex<ProductionPetAssets>>,
    wake_interval: Duration,
}

impl PetSweepWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        configured_channel_id: i64,
        decay_per_day: i64,
        discord: Arc<SerenityDiscordTransport>,
        reminders: ReminderHooks,
    ) -> Self {
        Self::new_with_ai(
            database_path,
            configured_channel_id,
            decay_per_day,
            discord,
            reminders,
            None,
            false,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_ai(
        database_path: impl AsRef<Path>,
        configured_channel_id: i64,
        decay_per_day: i64,
        discord: Arc<SerenityDiscordTransport>,
        reminders: ReminderHooks,
        ai_service: Option<Arc<AIService>>,
        ai_features_default: bool,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let flavor = Arc::new(ProductionPetFlavorRuntime::new(
            &database_path,
            ai_service,
            ai_features_default,
        ));
        Self::with_ports_and_flavor(
            database_path,
            configured_channel_id,
            decay_per_day,
            discord,
            Arc::new(reminders),
            flavor,
            FilesystemPetAssets::production(),
            PET_SWEEP_WAKE_INTERVAL,
        )
    }

    #[cfg(test)]
    fn with_ports(
        database_path: impl AsRef<Path>,
        configured_channel_id: i64,
        decay_per_day: i64,
        discord: Arc<dyn PetSweepDiscordPort>,
        reminders: Arc<dyn PetSweepReminderPort>,
        assets: FilesystemPetAssets,
        wake_interval: Duration,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let flavor = Arc::new(ProductionPetFlavorRuntime::new(&database_path, None, false));
        Self::with_ports_and_flavor(
            database_path,
            configured_channel_id,
            decay_per_day,
            discord,
            reminders,
            flavor,
            assets,
            wake_interval,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_ports_and_flavor(
        database_path: impl AsRef<Path>,
        configured_channel_id: i64,
        decay_per_day: i64,
        discord: Arc<dyn PetSweepDiscordPort>,
        reminders: Arc<dyn PetSweepReminderPort>,
        flavor: Arc<dyn PetSweepFlavorPort>,
        assets: FilesystemPetAssets,
        wake_interval: Duration,
    ) -> Self {
        let renderer = HybridPetRenderer::new(assets.components_directory());
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            configured_channel_id,
            decay_per_day,
            discord,
            reminders,
            flavor,
            media: Arc::new(Mutex::new(PetAssetLoader::new(assets, renderer))),
            wake_interval,
        }
    }

    async fn sweep_brawls(&self) -> Result<(), String> {
        let database_path = self.database_path.clone();
        tokio::task::spawn_blocking(move || {
            PetBrawlRepository::new(database_path)
                .sweep_stale(Utc::now().timestamp(), PET_BRAWL_ACTIVE_TTL_SECONDS)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("pet brawl sweep task failed: {error}"))?
    }

    async fn sweep_pets(&self) -> Result<SweepOutcome, String> {
        self.run_service(PetCommandService::sweep).await
    }

    async fn run_service<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProductionPetService) -> Result<T, String> + Send + 'static,
    {
        let database_path = self.database_path.clone();
        let decay_per_day = self.decay_per_day;
        let seed = entropy_seed();
        tokio::task::spawn_blocking(move || {
            let mut service = SqlitePetCommandService::new(
                database_path,
                SeededPetRandom::new(seed),
                SystemPetClock,
                decay_per_day,
            );
            operation(&mut service)
        })
        .await
        .map_err(|error| format!("pet SQLite task failed: {error}"))?
    }

    async fn render_media(&self, pet: &Pet, kind: PetMediaKind) -> Result<OwnedPetMedia, String> {
        let pet = pet.clone();
        let media = Arc::clone(&self.media);
        let decay_per_day = self.decay_per_day;
        tokio::task::spawn_blocking(move || {
            let mut loader = media
                .lock()
                .map_err(|_| "pet media cache lock poisoned".to_owned())?;
            let file = match kind {
                PetMediaKind::Hatch => loader.get_pet_card(&PetRenderRequest {
                    species_id: &pet.species,
                    stage: PetStage::Baby,
                    mood: PetMood::Happy,
                    seed: pet.pet_id,
                    accessory: None,
                    evolution: None,
                }),
                PetMediaKind::Evolution => loader.get_pet_card(&PetRenderRequest {
                    species_id: &pet.species,
                    stage: PetStage::Adult,
                    mood: PetMood::Happy,
                    seed: pet.pet_id,
                    accessory: pet.accessory.as_deref(),
                    evolution: evolution_visual(&pet),
                }),
                PetMediaKind::Death => {
                    // Keep calculation on the blocking pool even though the
                    // tombstone renderer only needs the durable name/seed.
                    let _mood = pet.mood(
                        pet.died_at.unwrap_or_else(|| Utc::now().timestamp()),
                        decay_per_day,
                    );
                    loader.get_tombstone_card(&pet.name, pet.pet_id)
                }
            };
            let bytes = file.bytes().to_vec();
            Ok(OwnedPetMedia {
                filename: file.filename,
                bytes,
            })
        })
        .await
        .map_err(|error| format!("pet media task failed: {error}"))?
    }

    async fn cached_channel(&self, guild_id: i64) -> Result<Option<i64>, String> {
        self.discord
            .cached_text_channel(guild_id, self.configured_channel_id)
            .await
    }

    async fn generate_flavor(
        &self,
        context: &WorkerContext,
        event: PetFlavorEvent,
        pet: Pet,
    ) -> Result<String, NoticeFailure> {
        let mut cancellation = context.clone();
        tokio::select! {
            () = cancellation.cancelled() => Err(NoticeFailure::Cancelled),
            line = self.flavor.generate(event, pet) => Ok(line),
        }
    }

    async fn deliver_channel(
        &self,
        context: &WorkerContext,
        channel_id: i64,
        message: DiscordMessage,
    ) -> Result<(), NoticeFailure> {
        let mut cancellation = context.clone();
        tokio::select! {
            () = cancellation.cancelled() => Err(NoticeFailure::Cancelled),
            result = self.discord.send_channel(channel_id, message) => match result {
                Ok(()) | Err(PetSweepDeliveryError::Forbidden) => Ok(()),
                Err(PetSweepDeliveryError::Transient(error)) => Err(NoticeFailure::Failed(error)),
            },
        }
    }

    async fn deliver_hatch(
        &self,
        context: &WorkerContext,
        notice: HatchNotice,
    ) -> Result<(), NoticeFailure> {
        let pet = notice.pet;
        if let Some(channel_id) = self.cached_channel(pet.guild_id).await? {
            let media = self.render_media(&pet, PetMediaKind::Hatch).await?;
            let flavor = self
                .generate_flavor(context, PetFlavorEvent::Hatched, pet.clone())
                .await?;
            let embed = with_flavor(build_hatch_embed(&pet), Some(flavor));
            self.deliver_channel(context, channel_id, pet_message(embed, Some(media)))
                .await?;
        }
        let marked_pet = pet.clone();
        self.run_service(move |service| service.mark_hatch_announced(&marked_pet))
            .await?;
        self.reminders
            .rearm_pet(pet.discord_id, pet.guild_id)
            .await?;
        Ok(())
    }

    async fn deliver_evolution(
        &self,
        context: &WorkerContext,
        notice: EvolutionNotice,
    ) -> Result<(), NoticeFailure> {
        let pet = notice.pet;
        if let Some(channel_id) = self.cached_channel(pet.guild_id).await? {
            let media = self.render_media(&pet, PetMediaKind::Evolution).await?;
            let flavor = self
                .generate_flavor(context, PetFlavorEvent::Evolved, pet.clone())
                .await?;
            let embed = with_flavor(build_evolution_embed(&pet), Some(flavor));
            self.deliver_channel(context, channel_id, pet_message(embed, Some(media)))
                .await?;
        }
        self.run_service(move |service| service.mark_evolution_announced(&pet))
            .await?;
        Ok(())
    }

    async fn deliver_death(
        &self,
        context: &WorkerContext,
        notice: DeathNotice,
    ) -> Result<(), NoticeFailure> {
        let pet = notice.pet;
        let channel_id = self.cached_channel(pet.guild_id).await?;
        // Python treats preference/DM failures as best effort. A failed read
        // therefore must not keep a public announcement in the retry queue.
        let dm_enabled = self
            .reminders
            .pet_enabled(pet.discord_id, pet.guild_id)
            .await
            .unwrap_or(false);
        let eating = notice.eating_outcome.as_ref();
        let needs_payload = channel_id.is_some() || dm_enabled;
        let (embed, media) = if let Some(outcome) = eating {
            (build_eating_outcome_embed(&pet, outcome), None)
        } else {
            let flavor = if needs_payload {
                Some(
                    self.generate_flavor(context, PetFlavorEvent::Died, pet.clone())
                        .await?,
                )
            } else {
                None
            };
            let embed = with_flavor(build_death_embed(&pet), flavor);
            let media = if needs_payload {
                Some(self.render_media(&pet, PetMediaKind::Death).await?)
            } else {
                None
            };
            (embed, media)
        };
        if let Some(channel_id) = channel_id {
            self.deliver_channel(
                context,
                channel_id,
                pet_message(embed.clone(), media.clone()),
            )
            .await?;
        }
        self.reminders
            .cancel_pet(pet.discord_id, pet.guild_id)
            .await?;
        if dm_enabled {
            let mut cancellation = context.clone();
            let message = pet_message(embed, media);
            tokio::select! {
                () = cancellation.cancelled() => return Err(NoticeFailure::Cancelled),
                _ = self.discord.send_dm(pet.discord_id, message) => {}
            }
        }
        self.run_service(move |service| service.mark_death_announced(&pet))
            .await?;
        Ok(())
    }

    async fn deliver_refund(
        &self,
        context: &WorkerContext,
        notice: RefundNotice,
    ) -> Result<(), NoticeFailure> {
        if let Some(channel_id) = self.cached_channel(notice.guild_id).await? {
            self.deliver_channel(
                context,
                channel_id,
                pet_message(build_refund_embed(&notice), None),
            )
            .await?;
        }
        self.run_service(move |service| service.mark_refund_announced(&notice))
            .await?;
        Ok(())
    }

    async fn sweep_once(&self, context: &WorkerContext) {
        if let Err(error) = self.sweep_brawls().await {
            warn!(%error, "pet brawl stale sweep failed");
        }
        let outcome = match self.sweep_pets().await {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!(%error, "pet lifecycle sweep failed");
                return;
            }
        };
        for notice in outcome.hatches {
            if context.shutdown_requested() {
                return;
            }
            let pet_id = notice.pet.pet_id;
            if let Err(error) = self.deliver_hatch(context, notice).await
                && report_notice_failure("hatch", pet_id, error)
            {
                return;
            }
        }
        for notice in outcome.evolutions {
            if context.shutdown_requested() {
                return;
            }
            let pet_id = notice.pet.pet_id;
            if let Err(error) = self.deliver_evolution(context, notice).await
                && report_notice_failure("evolution", pet_id, error)
            {
                return;
            }
        }
        for notice in outcome.deaths {
            if context.shutdown_requested() {
                return;
            }
            let pet_id = notice.pet.pet_id;
            if direct_death_delivery_active(&self.database_path, pet_id) {
                continue;
            }
            if let Err(error) = self.deliver_death(context, notice).await
                && report_notice_failure("death", pet_id, error)
            {
                return;
            }
        }
        for notice in outcome.refunds {
            if context.shutdown_requested() {
                return;
            }
            let guild_id = notice.guild_id;
            if let Err(error) = self.deliver_refund(context, notice).await
                && report_notice_failure("refund", guild_id, error)
            {
                return;
            }
        }
    }
}

#[async_trait]
impl BackgroundWorker for PetSweepWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            self.sweep_once(&context).await;
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

/// Build the feature-gated worker consumed by production composition.
#[must_use]
pub fn pet_sweep_worker_spec(
    database_path: impl AsRef<Path>,
    config: &ApplicationConfig,
    discord: Arc<SerenityDiscordTransport>,
    reminders: ReminderHooks,
) -> Option<BackgroundWorkerSpec> {
    pet_sweep_worker_spec_with_ai(database_path, config, discord, reminders, None)
}

/// AI-aware production composition. The existing spec remains source
/// compatible and selects the exact static fallback path.
#[must_use]
pub fn pet_sweep_worker_spec_with_ai(
    database_path: impl AsRef<Path>,
    config: &ApplicationConfig,
    discord: Arc<SerenityDiscordTransport>,
    reminders: ReminderHooks,
    ai_service: Option<Arc<AIService>>,
) -> Option<BackgroundWorkerSpec> {
    let channel_id = config.channels.pet?;
    Some(BackgroundWorkerSpec::new(
        PET_SWEEP_WORKER_NAME,
        Arc::new(PetSweepWorker::new_with_ai(
            database_path,
            channel_id,
            config.values.pet_hunger_decay_per_day,
            discord,
            reminders,
            ai_service,
            config.values.ai_features_enabled,
        )),
    ))
}

fn pet_message(embed: Embed, media: Option<OwnedPetMedia>) -> DiscordMessage {
    let mut runtime_embed = runtime_embed(embed);
    let mut response = InteractionResponse::message("");
    if let Some(media) = media {
        runtime_embed.image_url = Some(format!("attachment://{}", media.filename));
        response = response.attachment(InteractionAttachment::bytes(media.filename, media.bytes));
    }
    DiscordMessage::default_mentions(response.embed(runtime_embed))
}

fn runtime_embed(embed: Embed) -> InteractionEmbed {
    InteractionEmbed {
        title: Some(embed.title),
        description: Some(embed.description),
        color: Some(match embed.color {
            EmbedColor::Blue => 0x34_98_DB,
            EmbedColor::Green => 0x57_F2_87,
            EmbedColor::Gold => 0xF1_C4_0F,
            EmbedColor::Orange => 0xF3_9C_12,
            EmbedColor::Red => 0xED_42_45,
            EmbedColor::Slate => 0x5D_6D_7E,
            EmbedColor::Custom(color) => color,
        }),
        image_url: embed.image,
        footer: embed.footer,
        fields: embed
            .fields
            .into_iter()
            .map(|field| crate::registration::InteractionEmbedField {
                name: field.name,
                value: field.value,
                inline: field.inline,
            })
            .collect(),
        ..InteractionEmbed::default()
    }
}

fn with_flavor(mut embed: Embed, flavor: Option<String>) -> Embed {
    if let Some(flavor) = flavor {
        embed.field("💬 Cama chatter", flavor, false);
    }
    embed
}

fn pet_flavor_tool(name: &str) -> Result<ToolDefinition, String> {
    let (description, properties) = match name {
        BUNDLE_TOOL_NAME => (
            "Generate a reusable bundle of cheerful Camagotchi quips.",
            [("status", 6), ("fed", 5), ("spat", 3)]
                .into_iter()
                .map(|(name, count)| ToolProperty {
                    name: name.to_owned(),
                    description: String::new(),
                    enum_values: Vec::new(),
                    schema: ToolPropertySchema::StringArray {
                        min_items: count,
                        max_items: count,
                        unique_items: true,
                    },
                })
                .collect(),
        ),
        LINE_TOOL_NAME => (
            "Generate one safe, lighthearted Camagotchi line.",
            vec![ToolProperty {
                name: "line".to_owned(),
                description: String::new(),
                enum_values: Vec::new(),
                schema: ToolPropertySchema::String,
            }],
        ),
        other => return Err(format!("unsupported pet flavor tool {other}")),
    };
    Ok(ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        required: properties
            .iter()
            .map(|property| property.name.clone())
            .collect(),
        properties,
        additional_properties: Some(false),
    })
}

fn pet_tool_value(value: AiValue) -> ToolValue {
    match value {
        AiValue::Null => ToolValue::Null,
        AiValue::Text(value) => ToolValue::Text(value),
        AiValue::List(values) => ToolValue::Array(values.into_iter().map(pet_tool_value).collect()),
        AiValue::Object(values) => ToolValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, pet_tool_value(value)))
                .collect(),
        ),
        AiValue::Bool(value) => ToolValue::Text(value.to_string()),
        AiValue::Integer(value) => ToolValue::Text(value.to_string()),
        AiValue::Real(value) => ToolValue::Text(value.to_string()),
    }
}

fn fallback_lifecycle_flavor(event: PetFlavorEvent, pet: &Pet) -> String {
    let options = fallbacks(event);
    let event_offset = match event {
        PetFlavorEvent::Hatched => 0,
        PetFlavorEvent::Evolved => 1,
        PetFlavorEvent::Died => 2,
        _ => 0,
    };
    let pet_id = usize::try_from(pet.pet_id.unsigned_abs()).unwrap_or(0);
    options[(pet_id + event_offset) % options.len()].to_owned()
}

fn evolution_visual(pet: &Pet) -> Option<EvolutionVisual> {
    let calling = parse_calling(pet.evolution_calling.as_deref()?)?;
    let primary = parse_instinct(pet.evolution_primary.as_deref()?)?;
    Some(EvolutionVisual {
        calling,
        primary,
        secondary: pet.evolution_secondary.as_deref().and_then(parse_instinct),
    })
}

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

fn parse_instinct(value: &str) -> Option<PetInstinct> {
    PetInstinct::ALL
        .into_iter()
        .find(|instinct| instinct.as_str() == value)
}

fn entropy_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_secs() ^ u64::from(duration.subsec_nanos())
        })
        ^ u64::from(std::process::id())
}

fn report_notice_failure(kind: &'static str, id: i64, failure: NoticeFailure) -> bool {
    match failure {
        NoticeFailure::Cancelled => true,
        NoticeFailure::Failed(error) => {
            warn!(%error, notice_kind = kind, notice_id = id, "pet notice delivery failed; will retry");
            false
        }
    }
}

#[cfg(test)]
mod tests;
