//! Production-supervised Camagotchi lifecycle sweep.
//!
//! SQLite and pet-card rendering are synchronous by design. This adapter
//! keeps both on Tokio's blocking pool, returns fully owned notices/media, and
//! never holds either service across a Discord await.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::AIService;
use cama_app::pet::{SeededPetRandom, SweepOutcome, SystemPetClock};
use cama_app::pet_assets::{
    FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
};
use cama_app::pet_commands::{
    Embed, EmbedColor, PetCommandService, build_death_embed, build_eating_outcome_embed,
    build_evolution_embed, build_hatch_embed, build_refund_embed,
};
use cama_app::pet_flavor::{PetFlavorEvent, PetFlavorService, fallbacks};
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::pet_brawl_repository::PetBrawlRepository;
use cama_db::pet_repository::PetRepository;
use cama_domain::pet::{
    DeathNotice, EvolutionNotice, HatchNotice, PET_BRAWL_ACTIVE_TTL_SECONDS, Pet, PetMood,
    PetStage, RefundNotice,
};
use chrono::Utc;
use tracing::{debug, warn};

use crate::SerenityDiscordTransport;
use crate::application_config::ApplicationConfig;
use crate::discord_transport::DiscordMessage;
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::pet_death_delivery::is_active as direct_death_delivery_active;
pub use crate::pet_flavor_runtime::read_production_pet_flavor_context;
use crate::pet_flavor_runtime::{evolution_visual, production_pet_flavor_service};
use crate::registration::{InteractionAttachment, InteractionEmbed, InteractionResponse};
use crate::reminder_provider::ReminderHooks;
use crate::worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const PET_SWEEP_WORKER_NAME: &str = "pet_sweep";
pub const PET_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(10 * 60);

type ProductionPetService = SqlitePetCommandService<SeededPetRandom, SystemPetClock>;
type ProductionPetAssets = PetAssetLoader<FilesystemPetAssets, HybridPetRenderer>;

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
        Self {
            service: production_pet_flavor_service(database_path, ai_service, ai_features_default),
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
pub trait PetSweepDiscordPort: FirstGamePoolGuildSource + Send + Sync {
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

    #[cfg(all(test, feature = "runtime-test-dig"))]
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

    async fn worker_state_guild_ids(&self) -> Result<Vec<i64>, String> {
        let database_path = self.database_path.clone();
        tokio::task::spawn_blocking(move || {
            PetRepository::new(database_path)
                .list_worker_guild_ids()
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("pet guild-inventory task failed: {error}"))?
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
        // The hatch is already durable and announced, so this is best effort:
        // failing here logs "will retry" but nothing ever retries it, and the
        // announcement would be re-sent instead.
        if let Err(error) = self.reminders.rearm_pet(pet.discord_id, pet.guild_id).await {
            warn!(
                pet_id = pet.pet_id,
                ?error,
                "pet hatch reminder rearm failed"
            );
        }
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
        // The public death message has already been delivered, so a reminder
        // failure must not fail the notice: the sweep would re-announce the
        // same death on every pass because the pet is never marked announced.
        if let Err(error) = self
            .reminders
            .cancel_pet(pet.discord_id, pet.guild_id)
            .await
        {
            warn!(
                pet_id = pet.pet_id,
                ?error,
                "pet death reminder cancellation failed"
            );
        }
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
        let live_guilds = match self.discord.live_guild_ids() {
            Ok(guilds) => guilds.into_iter().collect::<BTreeSet<_>>(),
            Err(error) => {
                warn!(%error, "pet live-guild inventory failed; skipping sweep");
                return;
            }
        };
        let persisted_guilds = match self.worker_state_guild_ids().await {
            Ok(guilds) => guilds,
            Err(error) => {
                warn!(%error, "pet persisted-guild inventory failed; skipping sweep");
                return;
            }
        };
        let foreign_guilds = persisted_guilds
            .into_iter()
            .filter(|guild_id| !live_guilds.contains(guild_id))
            .collect::<Vec<_>>();
        if !foreign_guilds.is_empty() {
            debug!(
                ?foreign_guilds,
                "pet sweep skipped because the database contains non-live guild state"
            );
            return;
        }
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

#[cfg(all(test, feature = "runtime-test-dig"))]
mod tests;
