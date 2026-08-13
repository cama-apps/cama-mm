//! Live Discord registration for the complete `/pet` command group.
//!
//! The command layer in `cama-app` is deliberately Discord-free.  This module
//! is the narrow runtime composition boundary: it translates slash options
//! and component ids, acknowledges interactions with Python's visibility and
//! timeout rules, renders the existing pet media, and delegates every durable
//! mutation to the existing SQLite/application services.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::{
    AIService, Message, MessageRole, ToolChoice, ToolDefinition, ToolProperty, ToolPropertySchema,
    ToolRequest, Value as AiValue,
};
use cama_app::pet::{SeededPetRandom, SystemPetClock};
use cama_app::pet_assets::{
    EvolutionVisual, FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
};
use cama_app::pet_brawl::{
    Clock as BrawlClock, PetBrawlPort, PetBrawlService, PetEvolutionPort, PetPort, PortError,
    PortResult, ServiceRng, SoloTrainingPortOutcome,
};
use cama_app::pet_brawl_commands::{
    BattleHandle, BattleViewModel, ButtonStyle, ChallengeHandle, DiscordEvent, InMemoryDiscord,
    InteractionModel, PetBrawlCommands, TargetMember, ViewModel,
};
use cama_app::pet_commands::{
    Embed, EmbedColor, PetCommandService, PetFlavorEvent, StatusAction, StatusEmbedRequest,
    StatusView, build_adoption_embed, build_altar_cancel_embed, build_altar_preview_embed,
    build_altar_success_embed, build_eating_outcome_embed, build_graveyard_embed_with_dex,
    build_leaderboard_embed_with_records, build_shop_embed, build_status_embed_with_details,
};
use cama_app::pet_eating::{
    EatAdultPetCommit, EatAdultPetRequest as AppEatAdultPetRequest, PetEatingApplicationPort,
    PetEatingClock, PetEatingRandomPort, PetEatingRepositoryFailure, PetEatingRepositoryPort,
    PetEatingService,
};
use cama_app::pet_evolution_app::{PetEvolutionService, SystemEvolutionClock};
use cama_app::pet_flavor::{
    BUNDLE_TOOL_NAME, FlavorClock, FlavorDataPort, FlavorRng, GuildAiPort, LedgerEntry, LlmPort,
    LlmRequest, PetFlavorEvent as FlavorEvent, PetFlavorService,
    ToolCallResult as FlavorToolCallResult, ToolValue,
};
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::core_repositories::PlayerRepository;
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::pet_brawl_repository::{
    BrawlSettlement, BrawlSettlementResult, DrawSettlementResult, PetBrawlRepository, SweepResult,
};
use cama_db::pet_eating_repository::{
    EatAdultPetRequest as DbEatAdultPetRequest, PetEatingRepository, PetEatingRepositoryError,
};
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::pet_repository::{PetRepository, PetTrainingError};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::pet::{
    FOOD_ITEMS, GILDED_EGG_PREMIUM, MAX_BUY_QTY, Pet, PetMood, PetStage, SALT_LICK, TRINKET_COST,
    UNHATCHED_SPECIES,
};
use cama_domain::pet_brawl::{PetBrawl, PetBrawlMove};
use cama_domain::pet_evolution::{PetActivity, PetCalling, PetInstinct};
use cama_domain::service_result::ServiceResult;
use chrono::Utc;
use rusqlite::{Connection, params};
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::application_config::ApplicationConfig;
use crate::discord_transport::DiscordTransport;
use crate::pet_death_delivery::DirectDeathDeliveryGuard;
#[cfg(test)]
use crate::pet_death_delivery::is_active as shared_direct_death_delivery_active;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAttachment, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionMessageReceipt,
    InteractionOption, InteractionRequest, InteractionResponder, InteractionResponse,
    InteractionValue, RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::reminder_provider::ReminderHooks;

const COMPONENT_PREFIX: &str = "pet";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const RATE_LIMIT: usize = 6;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const STATUS_TIMEOUT: Duration = Duration::from_secs(180);
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);
const PET_FLAVOR_AI_DEFAULT: bool = false;

type ProductionPetService = SqlitePetCommandService<SeededPetRandom, SystemPetClock>;
type ProductionPetAssets = PetAssetLoader<FilesystemPetAssets, HybridPetRenderer>;
type ProductionBrawlService = PetBrawlService<
    RuntimeBrawlPetPort,
    RuntimeBrawlPort,
    RuntimeBrawlClock,
    RuntimeBrawlRng,
    RuntimeEvolutionPort,
>;
type ProductionBrawlCommands = PetBrawlCommands<ProductionBrawlService>;
type ProductionEvolutionService =
    PetEvolutionService<PetEvolutionRepository, PetRepository, SystemEvolutionClock>;
type ProductionEatingService =
    PetEatingService<RuntimeEatingRepository, RuntimeEatingClock, RuntimeEatingRng>;

/// Production `/pet` provider.  Durable pet/brawl/eating state is always
/// reloaded from SQLite; only Discord views and confirmations are process
/// local, matching discord.py's non-persistent `View` objects.
#[derive(Clone)]
pub struct PetRegistrationProvider {
    handler: Arc<PetInteractionHandler>,
}

impl PetRegistrationProvider {
    /// Compose the live command group and all of its component/autocomplete
    /// routes. `discord` is retained for media-independent channel fallback
    /// edits; interaction acknowledgement remains on the responder.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        reminders: ReminderHooks,
        ai_service: Option<Arc<AIService>>,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let flavor = Arc::new(ProductionPetFlavorRuntime::new(
            &database_path,
            ai_service,
            config.values.ai_features_enabled || PET_FLAVOR_AI_DEFAULT,
        ));
        let brawl = PetBrawlCommands::new(
            ProductionBrawlService::new(
                RuntimeBrawlPetPort::new(&database_path, config.values.pet_hunger_decay_per_day),
                RuntimeBrawlPort::new(&database_path),
                RuntimeBrawlClock,
                RuntimeBrawlRng::new(entropy_seed()),
                Some(RuntimeEvolutionPort::new(&database_path)),
                config.values.tip_fee_rate,
            ),
            config.channels.pet.unwrap_or_default(),
        );
        let state = Arc::new(PetRuntimeState {
            database_path,
            decay_per_day: config.values.pet_hunger_decay_per_day,
            pet_channel_id: config.channels.pet,
            discord,
            reminders,
            flavor,
            assets: Arc::new(Mutex::new(PetAssetLoader::new(
                FilesystemPetAssets::production(),
                HybridPetRenderer::production(),
            ))),
            rates: Mutex::new(BTreeMap::new()),
            next_token: AtomicU64::new(1),
            status_views: Mutex::new(BTreeMap::new()),
            confirmations: Mutex::new(BTreeMap::new()),
            brawl: Mutex::new(brawl),
            challenges: Mutex::new(BTreeMap::new()),
            battle_views: Mutex::new(BTreeMap::new()),
            battle_channels: Mutex::new(BTreeMap::new()),
            battle_guilds: Mutex::new(BTreeMap::new()),
            challenge_receipts: Mutex::new(BTreeMap::new()),
            battle_receipts: Mutex::new(BTreeMap::new()),
        });
        Self {
            handler: Arc::new(PetInteractionHandler { state }),
        }
    }

    /// Test/embedding convenience for callers without a configured AI
    /// provider. It still uses the live SQLite repositories and deterministic
    /// fallback flavor lines.
    #[must_use]
    pub fn new_without_ai(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        reminders: ReminderHooks,
    ) -> Self {
        Self::new(database_path, config, discord, reminders, None)
    }
}

impl RegistrationProvider for PetRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        if self.handler.state.pet_channel_id.is_none() {
            return Ok(());
        }
        registry.command(CommandSpec {
            name: "pet".to_owned(),
            description: "Adopt and care for your cama (camel-llama hybrid)".to_owned(),
            options: pet_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct PetRuntimeState {
    database_path: PathBuf,
    decay_per_day: i64,
    pet_channel_id: Option<i64>,
    discord: Arc<dyn DiscordTransport>,
    reminders: ReminderHooks,
    flavor: Arc<ProductionPetFlavorRuntime>,
    assets: Arc<Mutex<ProductionPetAssets>>,
    rates: Mutex<PetRateLimits>,
    next_token: AtomicU64,
    status_views: Mutex<BTreeMap<String, StatusViewState>>,
    confirmations: Mutex<BTreeMap<String, ConfirmationState>>,
    brawl: Mutex<ProductionBrawlCommands>,
    challenges: Mutex<BTreeMap<i64, ChallengeHandle>>,
    battle_views: Mutex<BTreeMap<i64, BattleHandle>>,
    battle_channels: Mutex<BTreeMap<i64, i64>>,
    battle_guilds: Mutex<BTreeMap<i64, i64>>,
    challenge_receipts: Mutex<PetInteractionReceipts>,
    battle_receipts: Mutex<PetInteractionReceipts>,
}

type PetRateLimits = BTreeMap<(i64, i64, &'static str), VecDeque<StdInstant>>;
type PetInteractionReceipts =
    BTreeMap<i64, (InteractionMessageReceipt, Arc<dyn InteractionResponder>)>;

struct PetInteractionHandler {
    state: Arc<PetRuntimeState>,
}

#[derive(Clone)]
struct StatusViewState {
    owner_id: i64,
    guild_id: i64,
    receipt: Option<InteractionMessageReceipt>,
    responder: Arc<dyn InteractionResponder>,
    response: InteractionResponse,
    public: bool,
    generation: u64,
    expires_at: Instant,
}

#[derive(Clone)]
enum ConfirmationKind {
    Altar {
        owner_id: i64,
        guild_id: i64,
        name: String,
        preview: cama_app::pet::SacrificePreview,
    },
    Eat {
        owner_id: i64,
        guild_id: i64,
        pet: Pet,
    },
}

#[derive(Clone)]
struct ConfirmationState {
    kind: ConfirmationKind,
    receipt: Option<InteractionMessageReceipt>,
    responder: Arc<dyn InteractionResponder>,
    expires_at: Instant,
}

#[async_trait]
impl InteractionHandler for PetInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { .. } => self
                .handle_command(request, responder)
                .await
                .map_err(Into::into),
            InteractionRequest::Autocomplete { .. } => self
                .handle_autocomplete(request, responder)
                .await
                .map_err(Into::into),
            InteractionRequest::Component { .. } => self
                .handle_component(request, responder)
                .await
                .map_err(Into::into),
            InteractionRequest::Modal { .. } => self
                .handle_modal(request, responder)
                .await
                .map_err(Into::into),
        }
    }
}

fn pet_options() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "adopt",
            "Adopt a mysterious cama egg",
            vec![
                string_option(
                    "name",
                    "Your pet's name (you're naming an egg, brave)",
                    true,
                ),
                choices_string_option(
                    "egg",
                    &format!("Gilded Egg: +{GILDED_EGG_PREMIUM} JC, no commons in the pool"),
                    false,
                    [
                        ("Standard Egg".to_owned(), "standard".to_owned()),
                        (
                            format!("Gilded Egg (+{GILDED_EGG_PREMIUM} JC, uncommon or better)"),
                            "gilded".to_owned(),
                        ),
                    ],
                ),
            ],
        ),
        subcommand(
            "status",
            "Check on your cama (art, fullness, mood)",
            vec![
                CommandOptionSpec::new(
                    "user",
                    "Peek at someone else's pet",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "public",
                    "Show to the whole channel",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand(
            "train",
            "Cash in up to three banked solo training sessions",
            Vec::new(),
        ),
        subcommand(
            "feed",
            "Feed your cama from your supplies",
            vec![choices_string_option(
                "item",
                "Which food to serve",
                true,
                FOOD_ITEMS.iter().map(|food| {
                    (
                        format!(
                            "{} ({} JC, +{} fullness)",
                            food.display_name, food.cost, food.restore
                        ),
                        food.item_id.to_owned(),
                    )
                }),
            )],
        ),
        subcommand("shop", "Browse cama food and treats", Vec::new()),
        subcommand(
            "buy",
            "Buy cama supplies",
            vec![
                choices_string_option(
                    "item",
                    "What to buy",
                    true,
                    FOOD_ITEMS
                        .iter()
                        .map(|food| {
                            (
                                format!(
                                    "{} ({} JC, +{} fullness)",
                                    food.display_name, food.cost, food.restore
                                ),
                                food.item_id.to_owned(),
                            )
                        })
                        .chain(std::iter::once((
                            format!(
                                "{} ({} JC, pampers instantly)",
                                SALT_LICK.display_name, SALT_LICK.cost
                            ),
                            SALT_LICK.item_id.to_owned(),
                        ))),
                ),
                CommandOptionSpec::new(
                    "qty",
                    "How many (salt lick: 1)",
                    CommandOptionKind::Integer,
                )
                .required(false)
                .with_integer_range(1, MAX_BUY_QTY),
            ],
        ),
        subcommand(
            "rename",
            "Rename your cama (10 JC)",
            vec![string_option("name", "The new name", true)],
        ),
        subcommand(
            "trinket",
            &format!("Roll a Mystery Trinket ({TRINKET_COST} JC) or wear one you own"),
            vec![
                CommandOptionSpec::new(
                    "wear",
                    "Wear an owned trinket instead of rolling",
                    CommandOptionKind::String,
                )
                .autocomplete(),
            ],
        ),
        subcommand(
            "brawl",
            "Challenge someone to a pet brawl, optionally for up to 100 JC",
            vec![
                CommandOptionSpec::new("user", "Whose cama to challenge", CommandOptionKind::User)
                    .required(true),
                CommandOptionSpec::new(
                    "wager",
                    "Optional matching wager (0 or omitted is free; max 100 JC)",
                    CommandOptionKind::Integer,
                )
                .with_integer_range(0, 100),
            ],
        ),
        subcommand(
            "altar",
            "Sacrifice your cama for a better egg (dark, effective)",
            vec![string_option("name", "The new egg's name", true)],
        ),
        subcommand(
            "eat",
            "Make an irreversible choice about your adult cama",
            Vec::new(),
        ),
        subcommand(
            "graveyard",
            "Visit the cama memorial garden",
            vec![CommandOptionSpec::new(
                "user",
                "Whose graveyard to visit",
                CommandOptionKind::User,
            )],
        ),
        subcommand("leaderboard", "The oldest living camas", Vec::new()),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn string_option(name: &str, description: &str, required: bool) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::String).required(required)
}

fn choices_string_option<I, S, V>(
    name: &str,
    description: &str,
    required: bool,
    choices: I,
) -> CommandOptionSpec
where
    I: IntoIterator<Item = (S, V)>,
    S: Into<String>,
    V: Into<String>,
{
    let mut option = string_option(name, description, required);
    option.choices = choices
        .into_iter()
        .map(|(name, value)| CommandOptionChoice::String {
            name: name.into(),
            value: value.into(),
        })
        .collect();
    option
}

trait IntegerRangeOption {
    fn with_integer_range(self, minimum: i64, maximum: i64) -> Self;
}

impl IntegerRangeOption for CommandOptionSpec {
    fn with_integer_range(mut self, minimum: i64, maximum: i64) -> Self {
        self.min_integer = Some(minimum);
        self.max_integer = Some(maximum);
        self
    }
}

impl PetInteractionHandler {
    async fn handle_command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            options,
            ..
        } = request
        else {
            return Err("invalid pet command payload".to_owned());
        };
        if name != "pet" {
            return Err(format!("pet handler received command {name:?}"));
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
        let channel_id = channel_id
            .map(|channel| signed_id(channel, "channel"))
            .transpose()?;
        let (subcommand, sub_options) = command_path(&options)?;

        let rate_scope = match subcommand {
            "status" => Some("pet_status"),
            "adopt" | "brawl" | "altar" | "eat" => Some("pet"),
            _ => None,
        };
        if let Some(rate_scope) = rate_scope
            && let Some(retry_after) = self.take_rate(user_id, guild_id, rate_scope)?
        {
            return respond(
                &responder,
                InteractionResponse::message(format!("⏳ Please wait {retry_after}s.")).ephemeral(),
            )
            .await;
        }

        match subcommand {
            "adopt" => {
                self.command_adopt(
                    user_id,
                    guild_id,
                    &user_display_name,
                    sub_options,
                    responder,
                )
                .await
            }
            "status" => {
                self.command_status(
                    user_id,
                    guild_id,
                    &user_display_name,
                    sub_options,
                    responder,
                )
                .await
            }
            "train" => self.command_train(user_id, guild_id, responder).await,
            "feed" => {
                self.command_feed(user_id, guild_id, sub_options, responder)
                    .await
            }
            "shop" => self.command_shop(user_id, guild_id, responder).await,
            "buy" => {
                self.command_buy(user_id, guild_id, sub_options, responder)
                    .await
            }
            "rename" => {
                self.command_rename(user_id, guild_id, sub_options, responder)
                    .await
            }
            "trinket" => {
                self.command_trinket(user_id, guild_id, sub_options, responder)
                    .await
            }
            "brawl" => {
                self.command_brawl(
                    user_id,
                    guild_id,
                    channel_id.unwrap_or_default(),
                    sub_options,
                    responder,
                )
                .await
            }
            "altar" => {
                self.command_altar(user_id, guild_id, sub_options, responder)
                    .await
            }
            "eat" => self.command_eat(user_id, guild_id, responder).await,
            "graveyard" => {
                self.command_graveyard(
                    user_id,
                    guild_id,
                    &user_display_name,
                    sub_options,
                    responder,
                )
                .await
            }
            "leaderboard" => self.command_leaderboard(guild_id, responder).await,
            other => Err(format!("unknown /pet subcommand {other:?}")),
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
            return Err("invalid pet autocomplete payload".to_owned());
        };
        if name != "pet" || focused_option != "wear" {
            return responder
                .autocomplete(Vec::new())
                .await
                .map_err(|error| error.to_string());
        }
        let user_id = signed_id(user_id, "user")?;
        let Some(guild_id) = guild_id else {
            return responder
                .autocomplete(Vec::new())
                .await
                .map_err(|error| error.to_string());
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let _ = options;
        let current = focused_value.to_ascii_lowercase();
        let choices = self
            .run_service(move |service| {
                Ok(service
                    .owned_trinkets(user_id, Some(guild_id))
                    .into_iter()
                    .filter_map(|accessory_id| {
                        let accessory = cama_domain::pet::get_accessory(&accessory_id);
                        accessory
                            .display_name
                            .to_ascii_lowercase()
                            .contains(&current)
                            .then(|| CommandOptionChoice::String {
                                name: format!(
                                    "{} ({})",
                                    accessory.display_name,
                                    accessory.tier.as_str()
                                ),
                                value: accessory_id,
                            })
                    })
                    .take(25)
                    .collect::<Vec<_>>())
            })
            .await?;
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
            custom_id,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            values,
            ..
        } = request
        else {
            return Err("invalid pet component payload".to_owned());
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
        let channel_id = channel_id
            .map(|channel| signed_id(channel, "channel"))
            .transpose()?
            .unwrap_or_default();
        let action = custom_id.strip_prefix("pet:").unwrap_or_default();
        if let Some(action) = action.strip_prefix("status:") {
            return self
                .component_status(action, user_id, guild_id, &user_display_name, responder)
                .await;
        }
        if let Some(action) = action.strip_prefix("altar:") {
            return self
                .component_altar(action, user_id, guild_id, responder)
                .await;
        }
        if let Some(action) = action.strip_prefix("eat:") {
            return self
                .component_eat(action, user_id, guild_id, responder)
                .await;
        }
        if let Some(action) = action.strip_prefix("brawl:") {
            return self
                .component_brawl(action, user_id, guild_id, channel_id, responder)
                .await;
        }
        // `PetBrawlCommands` uses the historical `pet_brawl:` prefix for
        // battle move buttons.  They are routed here as well.
        if let Some(action) = custom_id.strip_prefix("pet_brawl:") {
            return self
                .component_battle(action, user_id, guild_id, channel_id, responder)
                .await;
        }
        let _ = values;
        respond(
            &responder,
            InteractionResponse::message(
                "This pet interaction expired. Use `/pet status` to reopen it.",
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
        let InteractionRequest::Modal { custom_id, .. } = request else {
            return Err("invalid pet modal payload".to_owned());
        };
        respond(
            &responder,
            InteractionResponse::message(format!("Pet modal {custom_id:?} is no longer active."))
                .ephemeral(),
        )
        .await
    }

    fn take_rate(
        &self,
        user_id: i64,
        guild_id: i64,
        scope: &'static str,
    ) -> Result<Option<i64>, String> {
        let mut rates = self
            .state
            .rates
            .lock()
            .map_err(|_| "pet rate limiter lock poisoned".to_owned())?;
        let now = StdInstant::now();
        let hits = rates.entry((user_id, guild_id, scope)).or_default();
        while hits
            .front()
            .is_some_and(|started| now.duration_since(*started) >= RATE_WINDOW)
        {
            hits.pop_front();
        }
        if hits.len() < RATE_LIMIT {
            hits.push_back(now);
            return Ok(None);
        }
        let remaining =
            RATE_WINDOW.saturating_sub(now.duration_since(*hits.front().expect("rate hit exists")));
        Ok(Some(
            remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() != 0))
                .max(1) as i64,
        ))
    }

    fn spawn_status_timeout(&self, token: String, generation: u64) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(STATUS_TIMEOUT).await;
            let delivery = state.status_views.lock().ok().and_then(|mut views| {
                let current = views.get(&token)?;
                if !status_timeout_is_current(current, generation, Instant::now()) {
                    return None;
                }
                let current = views.remove(&token)?;
                Some((current.receipt, current.responder, current.response))
            });
            let Some((receipt, responder, response)) = delivery else {
                return;
            };
            let Some(receipt) = receipt else {
                return;
            };
            let _ = responder
                .edit_message(receipt, disabled_status_response(response))
                .await;
        });
    }

    async fn command_adopt(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let name = required_string(options, "name")?;
        let egg = optional_string(options, "egg").unwrap_or_else(|| "standard".to_owned());
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let display_name = display_name.to_owned();
        let flavor = Arc::clone(&self.state.flavor);
        let result = self
            .run_service(move |service| {
                let result = service.adopt(user_id, Some(guild_id), &name, &egg);
                match result {
                    ServiceResult::Success(outcome) => {
                        let mut embed = build_adoption_embed(&outcome, &display_name);
                        if let Some(line) =
                            flavor.generate(PetFlavorEvent::Adopted, &outcome.pet, None)
                        {
                            embed.field("💬 Cama chatter", line, false);
                        }
                        Ok((outcome, embed))
                    }
                    ServiceResult::Failure { error, .. } => Err(error),
                }
            })
            .await;
        let (outcome, embed) = match result {
            Ok(value) => value,
            Err(error) => return followup_error(&responder, error).await,
        };
        let attachment = self.render_egg(outcome.pet.pet_id).await?;
        responder
            .followup(response_embed(embed, Some(attachment), Vec::new(), false))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_status(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let target =
            optional_user(options, "user")?.unwrap_or((user_id, display_name.to_owned(), false));
        let public = optional_bool(options, "public").unwrap_or(false);
        responder
            .defer(!public)
            .await
            .map_err(|error| error.to_string())?;
        let target_name = target.1.clone();
        let built = self
            .build_status(target.0, guild_id, &target_name, target.0 == user_id)
            .await?;
        let token = if target.0 == user_id {
            Some(self.next_token("status"))
        } else {
            None
        };
        let response = status_response(built, token.as_deref(), public);
        let receipt = responder
            .followup_with_receipt(response.clone())
            .await
            .map_err(|error| error.to_string())?;
        if let Some(token) = token {
            let mut views = self
                .state
                .status_views
                .lock()
                .map_err(|_| "pet status view lock poisoned".to_owned())?;
            views.insert(
                token.clone(),
                StatusViewState {
                    owner_id: user_id,
                    guild_id,
                    receipt,
                    responder: Arc::clone(&responder),
                    response,
                    public,
                    generation: 0,
                    expires_at: Instant::now() + STATUS_TIMEOUT,
                },
            );
            self.spawn_status_timeout(token, 0);
        }
        Ok(())
    }

    async fn command_train(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let response = self
            .run_brawl(move |commands| {
                let interaction = brawl_interaction(user_id, guild_id, 0, true);
                let mut discord = InMemoryDiscord::default();
                commands.train(&interaction, &mut discord);
                let message = last_message(&discord)
                    .ok_or_else(|| "pet training produced no response".to_owned())?;
                Ok(outbound_response(message, true))
            })
            .await?;
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_feed(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let item = required_string(options, "item")?;
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let flavor = Arc::clone(&self.state.flavor);
        let result = self
            .run_service(move |service| {
                let result = service.feed(user_id, Some(guild_id), &item);
                match result {
                    ServiceResult::Success(outcome) => {
                        let status = service.status(user_id, Some(guild_id)).value().cloned();
                        let event = if outcome.spat {
                            PetFlavorEvent::Spat
                        } else {
                            PetFlavorEvent::Fed
                        };
                        let line = flavor.generate(event, &outcome.pet, status.as_ref());
                        Ok(feed_copy(&item, &outcome, line.as_deref()))
                    }
                    ServiceResult::Failure { error, .. } => Err(error),
                }
            })
            .await;
        let content = match result {
            Ok(content) => content,
            Err(error) => return followup_error(&responder, error).await,
        };
        let _ = self.state.reminders.rearm_pet(user_id, guild_id).await;
        responder
            .followup(InteractionResponse::message(content).ephemeral())
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
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .run_service(move |service| {
                let status = match service.status(user_id, Some(guild_id)) {
                    ServiceResult::Success(status) => status,
                    ServiceResult::Failure { .. } => empty_status(),
                };
                let species = status
                    .pet
                    .as_ref()
                    .filter(|_| status.stage != Some(PetStage::Egg))
                    .map_or("", |pet| pet.species.as_str());
                Ok(build_shop_embed(
                    status.supplies.as_ref(),
                    species,
                    service.balance(user_id, Some(guild_id)),
                ))
            })
            .await?;
        responder
            .followup(response_embed(result, None, Vec::new(), true))
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
        let item = required_string(options, "item")?;
        let qty = optional_integer(options, "qty").unwrap_or(1);
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .run_service(
                move |service| match service.buy(user_id, Some(guild_id), &item, qty) {
                    ServiceResult::Success(outcome) => Ok(buy_copy(&item, &outcome)),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
            )
            .await;
        let content = match result {
            Ok(content) => content,
            Err(error) => return followup_error(&responder, error).await,
        };
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_rename(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let name = required_string(options, "name")?;
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .run_service(
                move |service| match service.rename(user_id, Some(guild_id), &name) {
                    ServiceResult::Success(pet) => Ok(format!(
                        "✏️ Henceforth known as **{}** (-10 {JOPACOIN_EMOTE})",
                        pet.name
                    )),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
            )
            .await;
        let content = match result {
            Ok(content) => content,
            Err(error) => return followup_error(&responder, error).await,
        };
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_trinket(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let wear = optional_string(options, "wear");
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .run_service(move |service| {
                if let Some(accessory_id) = wear {
                    return match service.wear_trinket(user_id, Some(guild_id), &accessory_id) {
                        ServiceResult::Success(equipped) => {
                            let accessory = cama_domain::pet::get_accessory(&equipped);
                            Ok(format!(
                                "{} Now wearing the **{}**.",
                                accessory.emoji, accessory.display_name
                            ))
                        }
                        ServiceResult::Failure { error, .. } => Err(error),
                    };
                }
                match service.roll_trinket(user_id, Some(guild_id)) {
                    ServiceResult::Success(outcome) => {
                        let accessory = cama_domain::pet::get_accessory(&outcome.accessory_id);
                        if outcome.duplicate {
                            Ok(format!(
                                "{} A duplicate **{}** — it dissolves into a partial refund (net -{} {JOPACOIN_EMOTE}). Collection: {}/{}",
                                accessory.emoji,
                                accessory.display_name,
                                outcome.net_cost,
                                outcome.owned_count,
                                cama_domain::pet::ACCESSORIES.len()
                            ))
                        } else {
                            let tier_flair = match accessory.tier.as_str() {
                                "uncommon" => "🔹 Uncommon! ",
                                "rare" => "🔮 RARE! ",
                                "legendary" => "⚡ LEGENDARY!!! ",
                                _ => "",
                            };
                            Ok(format!(
                                "🎁 {}**{}** {} — _{}_ (-{} {JOPACOIN_EMOTE})\nEquipped! Collection: {}/{}",
                                tier_flair,
                                accessory.display_name,
                                accessory.emoji,
                                accessory.blurb,
                                outcome.net_cost,
                                outcome.owned_count,
                                cama_domain::pet::ACCESSORIES.len()
                            ))
                        }
                    }
                    ServiceResult::Failure { error, .. } => Err(error),
                }
            })
            .await;
        let content = match result {
            Ok(content) => content,
            Err(error) => return followup_error(&responder, error).await,
        };
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_graveyard(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let target =
            optional_user(options, "user")?.unwrap_or((user_id, display_name.to_owned(), false));
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let name = target.1;
        let database_path = self.state.database_path.clone();
        let now = Utc::now().timestamp();
        let (pets, camadex) = self
            .run_service(move |service| {
                let pets = service.graveyard(target.0, Some(guild_id));
                if let ServiceResult::Failure { error, .. } =
                    service.status(target.0, Some(guild_id))
                {
                    return Err(error);
                }
                let camadex = PetRepository::new(database_path)
                    .species_raised(target.0, Some(guild_id), now)
                    .map_err(|error| error.to_string())?;
                Ok((pets, camadex))
            })
            .await?;
        let callingdex = self
            .run_evolution(move |service| service.callingdex(target.0, Some(guild_id)))
            .await?;
        let embed = build_graveyard_embed_with_dex(
            &pets,
            &name,
            Some((&camadex, cama_domain::pet::SPECIES.len())),
            Some((&callingdex.0, callingdex.1)),
        );
        responder
            .followup(response_embed(embed, None, Vec::new(), false))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_leaderboard(
        &self,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let now = Utc::now().timestamp();
        let decay = self.state.decay_per_day;
        let pets = self
            .run_service(move |service| Ok(service.leaderboard(Some(guild_id))))
            .await?;
        let pet_ids = pets.iter().map(|pet| pet.pet_id).collect::<Vec<_>>();
        let records = if pet_ids.is_empty() {
            BTreeMap::new()
        } else {
            self.run_brawl(move |commands| {
                commands
                    .service_mut()
                    .records_for(&pet_ids, Some(guild_id))
                    .map_err(|error| error.code)
            })
            .await?
        };
        let embed = build_leaderboard_embed_with_records(&pets, decay, now, Some(&records));
        responder
            .followup(response_embed(embed, None, Vec::new(), false))
            .await
            .map_err(|error| error.to_string())
    }

    async fn command_brawl(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let target = required_user(options, "user")?;
        let wager = optional_integer(options, "wager").unwrap_or(0);
        if target.2 {
            return respond(
                &responder,
                InteractionResponse::message("Bots keep no pets. Yet.").ephemeral(),
            )
            .await;
        }
        if !self.brawl_channel_allowed(guild_id, channel_id).await {
            return respond(
                &responder,
                InteractionResponse::message(format!(
                    "🐫 Pet brawls happen in <#{}> — take it to the arena.",
                    self.state.pet_channel_id.unwrap_or_default()
                ))
                .ephemeral(),
            )
            .await;
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let mut interaction = brawl_interaction(user_id, guild_id, channel_id, true);
        if channel_id != self.state.pet_channel_id.unwrap_or(channel_id) {
            interaction.parent_channel_id = self.state.pet_channel_id;
        }
        let target_member = TargetMember {
            id: target.0,
            is_bot: target.2,
        };
        let (handle, recorder) = self
            .run_brawl(move |commands| {
                let mut recorder = InMemoryDiscord::default();
                let handle =
                    commands.brawl(&interaction, target_member, Some(wager), &mut recorder);
                Ok((handle, recorder))
            })
            .await?;
        let Some(handle) = handle else {
            if let Some(message) = last_message(&recorder) {
                return responder
                    .followup(outbound_response(message, true))
                    .await
                    .map_err(|error| error.to_string());
            }
            return Ok(());
        };
        let brawl_id = handle.brawl.brawl_id;
        let brawl_for_media = handle.brawl.clone();
        self.state
            .challenges
            .lock()
            .map_err(|_| "pet challenge lock poisoned".to_owned())?
            .insert(brawl_id, handle);
        let mut response = last_message(&recorder)
            .map(|message| outbound_response(message, false))
            .ok_or_else(|| "pet challenge produced no response".to_owned())?;
        if let Ok(attachment) = self.render_brawl_versus(&brawl_for_media).await {
            response = response.attachment(attachment);
        }
        response.components = challenge_components(brawl_id);
        response = response.with_user_mentions(vec![u64::try_from(target.0).unwrap_or_default()]);
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(receipt) => receipt,
            Err(error) => {
                let void_result = self
                    .run_brawl(move |commands| {
                        match commands.service_mut().void(brawl_id, Some(guild_id)) {
                            ServiceResult::Success(()) => Ok(()),
                            ServiceResult::Failure { error, .. } => Err(error),
                        }
                    })
                    .await;
                if let Err(void_error) = void_result {
                    self.state
                        .challenges
                        .lock()
                        .map_err(|lock_error| {
                            format!("{error}; challenge void failed: {void_error}; {lock_error}")
                        })?
                        .remove(&brawl_id);
                    return Err(format!(
                        "{error}; challenge void/refund failed: {void_error}"
                    ));
                }
                self.state
                    .challenges
                    .lock()
                    .map_err(|_| "pet challenge lock poisoned".to_owned())?
                    .remove(&brawl_id);
                return Err(error.to_string());
            }
        };
        if let Some(receipt) = receipt {
            self.state
                .challenge_receipts
                .lock()
                .map_err(|_| "pet challenge receipt lock poisoned".to_owned())?
                .insert(brawl_id, (receipt, Arc::clone(&responder)));
            if let Some(challenge) = self
                .state
                .challenges
                .lock()
                .map_err(|_| "pet challenge lock poisoned".to_owned())?
                .get_mut(&brawl_id)
            {
                challenge.message_id = Some(receipt.message_id);
            }
        }
        self.spawn_challenge_timeout(brawl_id);
        Ok(())
    }

    async fn command_altar(
        &self,
        user_id: i64,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let name = required_string(options, "name")?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let preview = self
            .run_service(
                move |service| match service.sacrifice_preview(user_id, Some(guild_id)) {
                    ServiceResult::Success(preview) => Ok(preview),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
            )
            .await;
        let preview = match preview {
            Ok(preview) => preview,
            Err(error) => return followup_error(&responder, error).await,
        };
        let token = self.next_token("altar");
        let embed = build_altar_preview_embed(&preview);
        let response = confirmation_response(
            embed,
            vec![
                InteractionButton::new(format!("pet:altar:{token}:confirm"), "Sacrifice")
                    .emoji("🔪")
                    .style(InteractionButtonStyle::Danger),
                InteractionButton::new(format!("pet:altar:{token}:cancel"), "Cancel")
                    .style(InteractionButtonStyle::Secondary),
            ],
            false,
        );
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        self.state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .insert(
                token.clone(),
                ConfirmationState {
                    kind: ConfirmationKind::Altar {
                        owner_id: user_id,
                        guild_id,
                        name,
                        preview,
                    },
                    receipt,
                    responder: Arc::clone(&responder),
                    expires_at: Instant::now() + CONFIRMATION_TIMEOUT,
                },
            );
        self.spawn_confirmation_timeout(token);
        Ok(())
    }

    async fn command_eat(
        &self,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        // Python performs this preview before deferring; failures are an
        // immediate ephemeral response and never leave a public placeholder.
        let preview = self
            .run_eating(
                move |service| match service.eat_preview(user_id, Some(guild_id)) {
                    ServiceResult::Success(pet) => Ok(pet),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
            )
            .await;
        let pet = match preview {
            Ok(pet) => pet,
            Err(error) => {
                return respond(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")).ephemeral(),
                )
                .await;
            }
        };
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let token = self.next_token("eat");
        let embed = eating_warning_embed(&pet);
        let response = confirmation_response(
            embed,
            vec![
                InteractionButton::new(format!("pet:eat:{token}:confirm"), "Eat them")
                    .emoji("🍽️")
                    .style(InteractionButtonStyle::Danger),
                InteractionButton::new(format!("pet:eat:{token}:cancel"), "Let them live")
                    .style(InteractionButtonStyle::Secondary),
            ],
            false,
        );
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        self.state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .insert(
                token.clone(),
                ConfirmationState {
                    kind: ConfirmationKind::Eat {
                        owner_id: user_id,
                        guild_id,
                        pet,
                    },
                    receipt,
                    responder: Arc::clone(&responder),
                    expires_at: Instant::now() + CONFIRMATION_TIMEOUT,
                },
            );
        self.spawn_confirmation_timeout(token);
        Ok(())
    }

    async fn build_status(
        &self,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        with_view: bool,
    ) -> Result<BuiltStatus, String> {
        let display_name = display_name.to_owned();
        let decay = self.state.decay_per_day;
        let database_path = self.state.database_path.clone();
        let flavor_runtime = Arc::clone(&self.state.flavor);
        let now = Utc::now().timestamp();
        let data = self
            .run_service(move |service| {
                let status = match service.status(user_id, Some(guild_id)) {
                    ServiceResult::Success(status) => status,
                    ServiceResult::Failure { error, .. } => return Err(error),
                };
                let next_fee = service.next_adoption_fee(user_id, Some(guild_id));
                let mut view = None;
                if with_view && let Some(pet) = status.pet.as_ref() {
                    let species = if status.stage == Some(PetStage::Egg) {
                        UNHATCHED_SPECIES
                    } else {
                        pet.species.as_str()
                    };
                    let game_date = cama_domain::game_date::game_date_for_timestamp(now as f64)
                        .unwrap_or_default();
                    let can_feed = status.stage != Some(PetStage::Egg)
                        && pet.feeds_used_on(&game_date) < cama_domain::pet::FEED_CAP_PER_DAY;
                    view = Some(StatusView::new(
                        user_id,
                        Some(guild_id),
                        status.supplies.as_ref(),
                        species,
                        can_feed,
                    ));
                }
                let (brawl_record, career, solo_training_sessions) = if let Some(pet) = status
                    .pet
                    .as_ref()
                    .filter(|_| status.stage != Some(PetStage::Egg))
                {
                    let (wins, losses) = PetBrawlRepository::new(&database_path)
                        .get_pet_record(pet.pet_id, Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    let career = cama_app::pet_brawl::PetBrawlService::<
                        RuntimeBrawlPetPort,
                        RuntimeBrawlPort,
                        RuntimeBrawlClock,
                        RuntimeBrawlRng,
                        RuntimeEvolutionPort,
                    >::career_summary(pet, wins, losses);
                    let available = PetRepository::available_solo_training_sessions(pet, now)
                        .unwrap_or_default();
                    (Some((wins, losses)), Some(career), Some(available))
                } else {
                    (None, None, None)
                };
                let mut embed = build_status_embed_with_details(StatusEmbedRequest {
                    status: &status,
                    decay_per_day: decay,
                    now,
                    owner_name: &display_name,
                    next_fee,
                    brawl_record,
                    career: career.as_ref(),
                    solo_training_sessions,
                });
                if let Some(pet) = status.pet.as_ref().or(status.last_dead.as_ref()) {
                    let event = if status.pet.is_some() {
                        PetFlavorEvent::Status
                    } else {
                        PetFlavorEvent::Died
                    };
                    if let Some(flavor) = flavor_runtime.generate(event, pet, Some(&status)) {
                        embed.field("💬 Cama chatter", flavor, false);
                    }
                }
                Ok((status, embed, view))
            })
            .await?;
        let attachment = if let Some(pet) = data.0.pet.as_ref() {
            if data.0.stage == Some(PetStage::Egg) {
                Some(self.render_egg(pet.pet_id).await?)
            } else {
                Some(self.render_pet(pet).await?)
            }
        } else {
            None
        };
        Ok(BuiltStatus {
            embed: data.1,
            attachment,
            view: if with_view { data.2 } else { None },
        })
    }

    async fn component_status(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        display_name: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut pieces = action.splitn(3, ':');
        let token = pieces.next().unwrap_or_default();
        let operation = pieces.next().unwrap_or_default().to_owned();
        let item = pieces.next().unwrap_or_default().to_owned();
        let state = self
            .state
            .status_views
            .lock()
            .map_err(|_| "pet status view lock poisoned".to_owned())?
            .get(token)
            .cloned();
        let Some(state) = state else {
            return respond(
                &responder,
                InteractionResponse::message("This pet status interaction expired.").ephemeral(),
            )
            .await;
        };
        if state.guild_id != guild_id || state.owner_id != user_id {
            return respond(
                &responder,
                InteractionResponse::message(
                    "That's not your cama. Adopt your own with `/pet adopt`!",
                )
                .ephemeral(),
            )
            .await;
        }
        if Instant::now() >= state.expires_at {
            self.state
                .status_views
                .lock()
                .map_err(|_| "pet status view lock poisoned".to_owned())?
                .remove(token);
            return responder
                .update(
                    InteractionResponse::message("This pet status interaction expired.")
                        .action_rows(Vec::new()),
                )
                .await
                .map_err(|error| error.to_string());
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let owner = state.owner_id;
        let result = self
            .run_service(move |service| match operation.as_str() {
                "feed" => match service.feed(owner, Some(guild_id), &item) {
                    ServiceResult::Success(_) => Ok(()),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
                "buy" => match service.buy(owner, Some(guild_id), &item, 1) {
                    ServiceResult::Success(_) => Ok(()),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
                "salt" => match service.buy(owner, Some(guild_id), SALT_LICK.item_id, 1) {
                    ServiceResult::Success(_) => Ok(()),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
                _ => Err("Unknown status action.".to_owned()),
            })
            .await;
        if let Err(error) = result {
            return followup_error(&responder, error).await;
        }
        let _ = self.state.reminders.rearm_pet(owner, guild_id).await;
        let built = self
            .build_status(owner, guild_id, display_name, true)
            .await?;
        let public = state.public;
        let response = status_response(built, Some(token), public);
        let delivered = if let Some(receipt) = state.receipt {
            state
                .responder
                .edit_message(receipt, response.clone())
                .await
        } else {
            responder.edit_original(response.clone()).await
        };
        delivered.map_err(|error| error.to_string())?;
        let generation = {
            let mut views = self
                .state
                .status_views
                .lock()
                .map_err(|_| "pet status view lock poisoned".to_owned())?;
            let Some(view) = views.get_mut(token) else {
                return Ok(());
            };
            view.response = response;
            view.public = public;
            view.generation = view.generation.wrapping_add(1);
            view.expires_at = Instant::now() + STATUS_TIMEOUT;
            view.generation
        };
        self.spawn_status_timeout(token.to_owned(), generation);
        Ok(())
    }

    async fn component_altar(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut parts = action.splitn(2, ':');
        let token = parts.next().unwrap_or_default().to_owned();
        let decision = parts.next().unwrap_or_default();
        let state = self
            .state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .get(&token)
            .cloned();
        let Some(state) = state else {
            return respond(
                &responder,
                InteractionResponse::message("This altar interaction expired.").ephemeral(),
            )
            .await;
        };
        let ConfirmationKind::Altar {
            owner_id,
            guild_id: expected_guild,
            name,
            preview,
        } = state.kind.clone()
        else {
            return respond(
                &responder,
                InteractionResponse::message("This altar interaction expired.").ephemeral(),
            )
            .await;
        };
        if owner_id != user_id || expected_guild != guild_id {
            return respond(
                &responder,
                InteractionResponse::message("That's not your altar ritual.").ephemeral(),
            )
            .await;
        }
        if decision == "cancel" {
            self.state
                .confirmations
                .lock()
                .map_err(|_| "pet confirmation lock poisoned".to_owned())?
                .remove(&token);
            return responder
                .update(response_embed(
                    build_altar_cancel_embed(&preview),
                    None,
                    Vec::new(),
                    false,
                ))
                .await
                .map_err(|error| error.to_string());
        }
        if decision != "confirm" {
            return respond(
                &responder,
                InteractionResponse::message("Unknown altar action.").ephemeral(),
            )
            .await;
        }
        if Instant::now() >= state.expires_at {
            self.state
                .confirmations
                .lock()
                .map_err(|_| "pet confirmation lock poisoned".to_owned())?
                .remove(&token);
            return responder
                .update(
                    InteractionResponse::message("The altar ritual timed out.")
                        .action_rows(Vec::new()),
                )
                .await
                .map_err(|error| error.to_string());
        }
        self.state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .remove(&token);
        let result = self
            .run_service(move |service| {
                match service.sacrifice(owner_id, Some(expected_guild), &name) {
                    ServiceResult::Success(outcome) => Ok(outcome),
                    ServiceResult::Failure { error, .. } => Err(error),
                }
            })
            .await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                // Python sends the failure privately and removes the
                // confirmation controls from the public preview.
                let _ = responder
                    .update(response_embed(
                        build_altar_preview_embed(&preview),
                        None,
                        Vec::new(),
                        false,
                    ))
                    .await;
                return followup_error(&responder, error).await;
            }
        };
        let attachment = self
            .render_altar(&outcome.dead_pet.name, outcome.dead_pet.pet_id)
            .await
            .ok();
        let response = response_embed(
            build_altar_success_embed(&outcome),
            attachment,
            Vec::new(),
            false,
        );
        let updated = responder.update(response.clone()).await;
        let posted = if updated.is_ok() {
            true
        } else {
            responder.followup(response).await.is_ok()
        };
        if posted {
            let dead = outcome.dead_pet.clone();
            self.run_service(move |service| service.mark_death_announced(&dead))
                .await?;
        }
        // The replacement egg is durable even when Discord temporarily
        // rejects the farewell edit. Keep its reminder lifecycle alive while
        // leaving the old death unannounced for the sweep retry.
        let _ = self
            .state
            .reminders
            .rearm_pet(owner_id, expected_guild)
            .await;
        if posted {
            Ok(())
        } else {
            Err("altar farewell could not be posted; sweep will retry".to_owned())
        }
    }

    async fn component_eat(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut parts = action.splitn(2, ':');
        let token = parts.next().unwrap_or_default().to_owned();
        let decision = parts.next().unwrap_or_default();
        let state = self
            .state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .get(&token)
            .cloned();
        let Some(state) = state else {
            return respond(
                &responder,
                InteractionResponse::message("This eating interaction expired.").ephemeral(),
            )
            .await;
        };
        let ConfirmationKind::Eat {
            owner_id,
            guild_id: expected_guild,
            pet,
        } = state.kind.clone()
        else {
            return respond(
                &responder,
                InteractionResponse::message("This eating interaction expired.").ephemeral(),
            )
            .await;
        };
        if owner_id != user_id || expected_guild != guild_id {
            return respond(
                &responder,
                InteractionResponse::message("That's not your cama to eat.").ephemeral(),
            )
            .await;
        }
        if decision == "cancel" {
            self.state
                .confirmations
                .lock()
                .map_err(|_| "pet confirmation lock poisoned".to_owned())?
                .remove(&token);
            return responder
                .update(response_embed(
                    Embed::new(
                        "The hunger passes",
                        format!("**{}** lives to graze another day.", pet.name),
                        EmbedColor::Blue,
                    ),
                    None,
                    Vec::new(),
                    false,
                ))
                .await
                .map_err(|error| error.to_string());
        }
        if decision != "confirm" {
            return respond(
                &responder,
                InteractionResponse::message("Unknown eating action.").ephemeral(),
            )
            .await;
        }
        if Instant::now() >= state.expires_at {
            self.state
                .confirmations
                .lock()
                .map_err(|_| "pet confirmation lock poisoned".to_owned())?
                .remove(&token);
            return responder
                .update(
                    InteractionResponse::message("The eating decision timed out.")
                        .action_rows(Vec::new()),
                )
                .await
                .map_err(|error| error.to_string());
        }
        self.state
            .confirmations
            .lock()
            .map_err(|_| "pet confirmation lock poisoned".to_owned())?
            .remove(&token);
        let _direct_delivery_guard =
            DirectDeathDeliveryGuard::new(&self.state.database_path, pet.pet_id);
        let result = self
            .run_eating(
                move |service| match service.eat(owner_id, Some(expected_guild)) {
                    ServiceResult::Success(outcome) => Ok(outcome),
                    ServiceResult::Failure { error, .. } => Err(error),
                },
            )
            .await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = responder
                    .update(response_embed(
                        eating_warning_embed(&pet),
                        None,
                        Vec::new(),
                        false,
                    ))
                    .await;
                return followup_error(&responder, error).await;
            }
        };
        // Python cancels the old reminder before trying to deliver the direct
        // outcome. A failed post remains unannounced and is retried by the
        // durable sweep, without a stale reminder racing that retry.
        let _ = self
            .state
            .reminders
            .cancel_pet_async(owner_id, expected_guild)
            .await;
        let map = BTreeMap::from([
            ("reward".to_owned(), outcome.reward),
            (
                "penalty_games_added".to_owned(),
                outcome.penalty_games_added,
            ),
            (
                "penalty_games_remaining".to_owned(),
                outcome.penalty_games_remaining,
            ),
            ("new_balance".to_owned(), outcome.new_balance),
        ]);
        let response = response_embed(
            build_eating_outcome_embed(&outcome.pet, &map),
            None,
            Vec::new(),
            false,
        );
        let updated = responder.update(response.clone()).await;
        let posted = if updated.is_ok() {
            true
        } else {
            responder.followup(response).await.is_ok()
        };
        if posted {
            let announced_pet = outcome.pet.clone();
            let marked = self
                .run_eating(move |service| {
                    service
                        .mark_death_announced(&announced_pet)
                        .map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = marked {
                return Err(format!(
                    "eating result posted but death announcement mark failed: {error}"
                ));
            }
        }
        if posted {
            Ok(())
        } else {
            Err("eating result could not be posted; sweep will retry".to_owned())
        }
    }

    async fn component_brawl(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut parts = action.splitn(2, ':');
        let brawl_id = parts
            .next()
            .ok_or_else(|| "invalid brawl component".to_owned())?
            .parse::<i64>()
            .map_err(|_| "invalid brawl id".to_owned())?;
        let verb = parts.next().unwrap_or_default().to_owned();
        let interaction = brawl_interaction(user_id, guild_id, channel_id, true);
        let mut challenge = self
            .state
            .challenges
            .lock()
            .map_err(|_| "pet challenge lock poisoned".to_owned())?
            .remove(&brawl_id);
        let Some(mut challenge) = challenge.take() else {
            return respond(
                &responder,
                InteractionResponse::message("This brawl challenge expired.").ephemeral(),
            )
            .await;
        };
        let challenge_message_id = challenge.message_id;
        let challenge_channel_id = challenge.brawl.channel_id;
        let challenge_delivery = self
            .state
            .challenge_receipts
            .lock()
            .map_err(|_| "pet challenge receipt lock poisoned".to_owned())?
            .remove(&brawl_id);
        if !matches!(verb.as_str(), "accept" | "decline" | "withdraw") {
            self.state
                .challenges
                .lock()
                .map_err(|_| "pet challenge lock poisoned".to_owned())?
                .insert(brawl_id, challenge);
            if let Some(delivery) = challenge_delivery.clone() {
                self.state
                    .challenge_receipts
                    .lock()
                    .map_err(|_| "pet challenge receipt lock poisoned".to_owned())?
                    .insert(brawl_id, delivery);
            }
            return respond(
                &responder,
                InteractionResponse::message("Unknown brawl action.").ephemeral(),
            )
            .await;
        }
        let operation_verb = verb.clone();
        let (outcome, recorder, challenge) = self
            .run_brawl(move |commands| {
                let mut recorder = InMemoryDiscord::default();
                let outcome = match operation_verb.as_str() {
                    "accept" => commands.accept(&mut challenge, &interaction, &mut recorder),
                    "decline" => {
                        commands.decline(&mut challenge, &interaction, &mut recorder);
                        None
                    }
                    "withdraw" => {
                        commands.withdraw(&mut challenge, &interaction, &mut recorder);
                        None
                    }
                    _ => None,
                };
                Ok((outcome, recorder, challenge))
            })
            .await?;
        if verb == "accept" && outcome.is_some() {
            let _ = self
                .state
                .battle_channels
                .lock()
                .map(|mut channels| channels.insert(brawl_id, challenge_channel_id));
            let _ = self
                .state
                .battle_guilds
                .lock()
                .map(|mut guilds| guilds.insert(brawl_id, guild_id));
            if let Some(handle) = outcome.clone() {
                self.state
                    .battle_views
                    .lock()
                    .map_err(|_| "pet battle view lock poisoned".to_owned())?
                    .insert(brawl_id, handle);
            }
            if let Some(delivery) = challenge_delivery.clone() {
                self.state
                    .battle_receipts
                    .lock()
                    .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
                    .insert(brawl_id, delivery);
            }
            self.spawn_battle_timeout(brawl_id);
        }
        // A failed accept/decline/withdraw leaves the Python View alive: the
        // command layer resets its responded flag. Keep both the challenge
        // handle and its receipt so a later click can retry it. A pet-dead
        // failure emits an Edited tombstone and intentionally stays removed.
        let edited = last_edited_message(&recorder).is_some();
        if outcome.is_none() && !edited {
            self.state
                .challenges
                .lock()
                .map_err(|_| "pet challenge lock poisoned".to_owned())?
                .insert(brawl_id, challenge);
            if let Some(delivery) = challenge_delivery.clone() {
                self.state
                    .challenge_receipts
                    .lock()
                    .map_err(|_| "pet challenge receipt lock poisoned".to_owned())?
                    .insert(brawl_id, delivery);
            }
        }
        if let Some(message) = last_edited_message(&recorder) {
            let response = outbound_response(message, false);
            return match responder.update(response.clone()).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    if let Some((receipt, retained)) = challenge_delivery {
                        retained
                            .edit_message(receipt, response)
                            .await
                            .map_err(|fallback| {
                                format!("{error}; brawl receipt delivery failed: {fallback}")
                            })
                    } else if let Some(message_id) = challenge_message_id {
                        self.state
                            .discord
                            .edit_message(
                                u64::try_from(challenge_channel_id).unwrap_or_default(),
                                message_id,
                                crate::discord_transport::DiscordMessage::default_mentions(
                                    response,
                                ),
                            )
                            .await
                            .map_err(|fallback| {
                                format!("{error}; brawl fallback delivery failed: {fallback}")
                            })
                    } else {
                        Err(error.to_string())
                    }
                }
            };
        }
        if let Some(message) = last_message(&recorder) {
            let response = outbound_response(message, message.ephemeral);
            let deferred = recorder
                .events
                .iter()
                .any(|event| matches!(event, DiscordEvent::Deferred { .. }));
            return if deferred {
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                responder
                    .respond(response)
                    .await
                    .map_err(|error| error.to_string())
            };
        }
        Ok(())
    }

    async fn component_battle(
        &self,
        action: &str,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut parts = action.split(':');
        let brawl_id = parts
            .next()
            .ok_or_else(|| "invalid battle component".to_owned())?
            .parse::<i64>()
            .map_err(|_| "invalid brawl id".to_owned())?;
        let _round = parts
            .next()
            .ok_or_else(|| "invalid battle round".to_owned())?;
        let move_name = parts.next().unwrap_or_default();
        let move_ = PetBrawlMove::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == move_name)
            .ok_or_else(|| "invalid brawl move".to_owned())?;
        let view = self
            .state
            .battle_views
            .lock()
            .map_err(|_| "pet battle view lock poisoned".to_owned())?
            .get(&brawl_id)
            .cloned()
            .ok_or_else(|| "This brawl interaction expired.".to_owned())?;
        let interaction = brawl_interaction(user_id, guild_id, channel_id, true);
        let (outcome, recorder, mut view) = self
            .run_brawl(move |commands| {
                let mut view = view;
                let mut recorder = InMemoryDiscord::default();
                let outcome = commands.pick(&mut view, &interaction, move_, &mut recorder);
                Ok((outcome, recorder, view))
            })
            .await?;
        let battle_message_id = view.message_id;
        let next_round = matches!(
            outcome,
            cama_app::pet_brawl_commands::PickOutcome::RoundResolved
        );
        if next_round {
            view = view.next_round_handle();
        }
        let battle_delivery = self
            .state
            .battle_receipts
            .lock()
            .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
            .get(&brawl_id)
            .cloned();
        if matches!(
            outcome,
            cama_app::pet_brawl_commands::PickOutcome::BattleFinished
        ) {
            self.state
                .battle_views
                .lock()
                .map_err(|_| "pet battle view lock poisoned".to_owned())?
                .remove(&brawl_id);
            self.state
                .battle_guilds
                .lock()
                .map_err(|_| "pet battle guild lock poisoned".to_owned())?
                .remove(&brawl_id);
            self.state
                .battle_channels
                .lock()
                .map_err(|_| "pet battle channel lock poisoned".to_owned())?
                .remove(&brawl_id);
        } else {
            self.state
                .battle_views
                .lock()
                .map_err(|_| "pet battle view lock poisoned".to_owned())?
                .insert(brawl_id, view.clone());
        }
        if let Some(message) = last_edited_message(&recorder) {
            let mut response = outbound_response(message, false);
            if matches!(outcome, cama_app::pet_brawl_commands::PickOutcome::LockedIn) {
                response = response.action_rows(battle_components(&view.view_model()));
            }
            if matches!(
                outcome,
                cama_app::pet_brawl_commands::PickOutcome::BattleFinished
            ) && let Ok(Some(attachment)) = self.render_brawl_winner(brawl_id, guild_id).await
            {
                response = response.attachment(attachment);
            }
            return match responder.update(response.clone()).await {
                Ok(()) => {
                    if matches!(
                        outcome,
                        cama_app::pet_brawl_commands::PickOutcome::BattleFinished
                    ) {
                        self.state
                            .battle_receipts
                            .lock()
                            .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
                            .remove(&brawl_id);
                    }
                    Ok(())
                }
                Err(error) => {
                    let delivered = if let Some((receipt, retained)) = battle_delivery {
                        retained
                            .edit_message(receipt, response)
                            .await
                            .map_err(|fallback| {
                                format!("{error}; brawl receipt delivery failed: {fallback}")
                            })
                    } else if let Some(message_id) = battle_message_id {
                        self.state
                            .discord
                            .edit_message(
                                u64::try_from(channel_id).unwrap_or_default(),
                                message_id,
                                crate::discord_transport::DiscordMessage::default_mentions(
                                    response,
                                ),
                            )
                            .await
                            .map_err(|fallback| {
                                format!("{error}; brawl fallback delivery failed: {fallback}")
                            })
                    } else {
                        Err(error.to_string())
                    };
                    if delivered.is_ok()
                        && matches!(
                            outcome,
                            cama_app::pet_brawl_commands::PickOutcome::BattleFinished
                        )
                    {
                        self.state
                            .battle_receipts
                            .lock()
                            .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
                            .remove(&brawl_id);
                    }
                    delivered
                }
            };
        }
        if let Some(message) = last_message(&recorder) {
            let response = outbound_response(message, message.ephemeral);
            let deferred = recorder
                .events
                .iter()
                .any(|event| matches!(event, DiscordEvent::Deferred { .. }));
            return if deferred {
                responder
                    .followup(response)
                    .await
                    .map_err(|error| error.to_string())
            } else {
                responder
                    .respond(response)
                    .await
                    .map_err(|error| error.to_string())
            };
        }
        Ok(())
    }

    fn next_token(&self, kind: &str) -> String {
        let sequence = self.state.next_token.fetch_add(1, Ordering::Relaxed);
        format!("{kind}-{}-{sequence:x}", entropy_seed())
    }

    async fn brawl_channel_allowed(&self, guild_id: i64, channel_id: i64) -> bool {
        let Some(pet_channel_id) = self.state.pet_channel_id else {
            return true;
        };
        if channel_id == pet_channel_id {
            return true;
        }
        let (Ok(guild_id), Ok(channel_id)) = (u64::try_from(guild_id), u64::try_from(channel_id))
        else {
            return false;
        };
        self.state
            .discord
            .channel_parent_id(guild_id, channel_id)
            .await
            .ok()
            .flatten()
            .and_then(|parent| i64::try_from(parent).ok())
            == Some(pet_channel_id)
    }

    async fn expire_challenge_once(
        state: Arc<PetRuntimeState>,
        brawl_id: i64,
    ) -> Result<bool, String> {
        let challenge = state
            .challenges
            .lock()
            .map_err(|_| "pet challenge lock poisoned".to_owned())?
            .remove(&brawl_id);
        let Some(mut challenge) = challenge else {
            return Ok(true);
        };
        let worker_state = Arc::clone(&state);
        let (challenge, recorder) = tokio::task::spawn_blocking(move || {
            let mut commands = worker_state
                .brawl
                .lock()
                .map_err(|_| "pet brawl service lock poisoned".to_owned())?;
            let mut recorder = InMemoryDiscord::default();
            commands.challenge_timeout(&mut challenge, &mut recorder);
            Ok::<_, String>((challenge, recorder))
        })
        .await
        .map_err(join_error)??;
        let delivery = state
            .challenge_receipts
            .lock()
            .map_err(|_| "pet challenge receipt lock poisoned".to_owned())?
            .remove(&brawl_id);
        if let Some(message) = last_message(&recorder) {
            let channel_id = u64::try_from(challenge.brawl.channel_id).unwrap_or_default();
            let response = outbound_response(message, false);
            if let Some((receipt, responder)) = delivery {
                // A follow-up receipt is sufficient; it may be the only
                // durable handle when a responder omits a raw message id.
                let _ = responder.edit_message(receipt, response).await;
            } else if let Some(message_id) = challenge.message_id {
                let _ = state
                    .discord
                    .edit_message(
                        channel_id,
                        message_id,
                        crate::discord_transport::DiscordMessage::default_mentions(response),
                    )
                    .await;
            }
        }
        Ok(false)
    }

    fn spawn_challenge_timeout(&self, brawl_id: i64) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = Self::expire_challenge_once(state, brawl_id).await;
        });
    }

    async fn advance_battle_timeout_once(
        state: Arc<PetRuntimeState>,
        brawl_id: i64,
    ) -> Result<bool, String> {
        let view = state
            .battle_views
            .lock()
            .map_err(|_| "pet battle view lock poisoned".to_owned())?
            .get(&brawl_id)
            .cloned();
        let Some(mut view) = view else {
            return Ok(true);
        };
        let worker_state = Arc::clone(&state);
        let (outcome, mut view, recorder) = tokio::task::spawn_blocking(move || {
            let mut commands = worker_state
                .brawl
                .lock()
                .map_err(|_| "pet brawl service lock poisoned".to_owned())?;
            let mut recorder = InMemoryDiscord::default();
            let outcome = commands.battle_timeout(&mut view, &mut recorder);
            Ok::<_, String>((outcome, view, recorder))
        })
        .await
        .map_err(join_error)??;
        let finished = matches!(
            outcome,
            cama_app::pet_brawl_commands::PickOutcome::BattleFinished
        );
        let guild_for_media = state
            .battle_guilds
            .lock()
            .map_err(|_| "pet battle guild lock poisoned".to_owned())?
            .get(&brawl_id)
            .copied()
            .unwrap_or_default();
        let battle_delivery = state
            .battle_receipts
            .lock()
            .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
            .get(&brawl_id)
            .cloned();
        let battle_channel_id = state
            .battle_channels
            .lock()
            .map_err(|_| "pet battle channel lock poisoned".to_owned())?
            .get(&brawl_id)
            .copied()
            .unwrap_or_default();
        if finished {
            state
                .battle_views
                .lock()
                .map_err(|_| "pet battle view lock poisoned".to_owned())?
                .remove(&brawl_id);
            state
                .battle_channels
                .lock()
                .map_err(|_| "pet battle channel lock poisoned".to_owned())?
                .remove(&brawl_id);
            state
                .battle_guilds
                .lock()
                .map_err(|_| "pet battle guild lock poisoned".to_owned())?
                .remove(&brawl_id);
        } else {
            if matches!(
                outcome,
                cama_app::pet_brawl_commands::PickOutcome::RoundResolved
            ) {
                view = view.next_round_handle();
            }
            state
                .battle_views
                .lock()
                .map_err(|_| "pet battle view lock poisoned".to_owned())?
                .insert(brawl_id, view.clone());
        }
        if let Some(message) = last_message(&recorder) {
            let mut response = outbound_response(message, false);
            if finished {
                let handler = PetInteractionHandler {
                    state: Arc::clone(&state),
                };
                if let Ok(Some(attachment)) =
                    handler.render_brawl_winner(brawl_id, guild_for_media).await
                {
                    response = response.attachment(attachment);
                }
            }
            if let Some((receipt, responder)) = battle_delivery {
                let _ = responder.edit_message(receipt, response).await;
            } else if let Some(message_id) = view.message_id {
                let _ = state
                    .discord
                    .edit_message(
                        u64::try_from(battle_channel_id).unwrap_or_default(),
                        message_id,
                        crate::discord_transport::DiscordMessage::default_mentions(response),
                    )
                    .await;
            }
        }
        if finished {
            state
                .battle_receipts
                .lock()
                .map_err(|_| "pet battle receipt lock poisoned".to_owned())?
                .remove(&brawl_id);
        }
        Ok(finished)
    }

    fn spawn_battle_timeout(&self, brawl_id: i64) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                match Self::advance_battle_timeout_once(Arc::clone(&state), brawl_id).await {
                    Ok(true) | Err(_) => return,
                    Ok(false) => {}
                }
            }
        });
    }

    fn spawn_confirmation_timeout(&self, token: String) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(CONFIRMATION_TIMEOUT).await;
            let removed = state
                .confirmations
                .lock()
                .ok()
                .and_then(|mut map| map.remove(&token));
            let Some(confirmation) = removed else { return };
            let Some(receipt) = confirmation.receipt else {
                return;
            };
            let response = match confirmation.kind {
                ConfirmationKind::Altar { preview, .. } => {
                    response_embed(build_altar_cancel_embed(&preview), None, Vec::new(), false)
                }
                ConfirmationKind::Eat { pet, .. } => response_embed(
                    Embed::new(
                        "The hunger passes",
                        format!("**{}** lives to graze another day.", pet.name),
                        EmbedColor::Blue,
                    ),
                    None,
                    Vec::new(),
                    false,
                ),
            };
            let _ = confirmation.responder.edit_message(receipt, response).await;
        });
    }

    async fn run_service<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProductionPetService) -> Result<T, String> + Send + 'static,
    {
        let database_path = self.state.database_path.clone();
        let decay_per_day = self.state.decay_per_day;
        tokio::task::spawn_blocking(move || {
            let mut service = SqlitePetCommandService::new(
                database_path,
                SeededPetRandom::new(entropy_seed()),
                SystemPetClock,
                decay_per_day,
            );
            operation(&mut service)
        })
        .await
        .map_err(join_error)
        .and_then(|result| result)
    }

    async fn run_brawl<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProductionBrawlCommands) -> Result<T, String> + Send + 'static,
    {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let mut commands = state
                .brawl
                .lock()
                .map_err(|_| "pet brawl service lock poisoned".to_owned())?;
            operation(&mut commands)
        })
        .await
        .map_err(join_error)
        .and_then(|result| result)
    }

    async fn run_evolution<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProductionEvolutionService) -> Result<T, String> + Send + 'static,
    {
        let database_path = self.state.database_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut service = PetEvolutionService::new(
                PetEvolutionRepository::new(&database_path),
                PetRepository::new(&database_path),
                SystemEvolutionClock,
            );
            operation(&mut service)
        })
        .await
        .map_err(join_error)
        .and_then(|result| result)
    }

    async fn run_eating<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProductionEatingService) -> Result<T, String> + Send + 'static,
    {
        let database_path = self.state.database_path.clone();
        tokio::task::spawn_blocking(move || {
            operation(&mut PetEatingService::new(
                RuntimeEatingRepository::new(database_path),
                RuntimeEatingClock,
                RuntimeEatingRng::new(entropy_seed()),
            ))
        })
        .await
        .map_err(join_error)
        .and_then(|result| result)
    }

    async fn render_pet(&self, pet: &Pet) -> Result<InteractionAttachment, String> {
        let pet = pet.clone();
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_pet_card(&PetRenderRequest {
                species_id: &pet.species,
                stage: pet.stage(Utc::now().timestamp()),
                mood: pet.mood(Utc::now().timestamp(), 1),
                seed: pet.pet_id,
                accessory: pet.accessory.as_deref(),
                evolution: evolution_visual(&pet),
            });
            let filename = file.filename.clone();
            let bytes = file.bytes().to_vec();
            Ok(InteractionAttachment::bytes(filename, bytes))
        })
        .await
        .map_err(join_error)?
    }

    async fn render_egg(&self, pet_id: i64) -> Result<InteractionAttachment, String> {
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_egg_card(pet_id);
            let filename = file.filename.clone();
            let bytes = file.bytes().to_vec();
            Ok(InteractionAttachment::bytes(filename, bytes))
        })
        .await
        .map_err(join_error)?
    }

    async fn render_altar(&self, name: &str, seed: i64) -> Result<InteractionAttachment, String> {
        let name = name.to_owned();
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_altar_card(&name, seed);
            let filename = file.filename.clone();
            let bytes = file.bytes().to_vec();
            Ok(InteractionAttachment::bytes(filename, bytes))
        })
        .await
        .map_err(join_error)?
    }

    async fn render_brawl_versus(&self, brawl: &PetBrawl) -> Result<InteractionAttachment, String> {
        let brawl = brawl.clone();
        let database_path = self.state.database_path.clone();
        let decay_per_day = self.state.decay_per_day;
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let repository = PetRepository::new(&database_path);
            let left = repository
                .get_pet_by_id(brawl.challenger_pet_id, Some(brawl.guild_id))
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "challenger pet disappeared before brawl art".to_owned())?;
            let right = match brawl.recipient_pet_id {
                Some(right_id) => repository.get_pet_by_id(right_id, Some(brawl.guild_id)),
                None => repository.get_active_pet(brawl.recipient_id, Some(brawl.guild_id)),
            }
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recipient pet disappeared before brawl art".to_owned())?;
            let now = Utc::now().timestamp();
            let left_request = PetRenderRequest {
                species_id: &left.species,
                stage: left.stage(now),
                mood: left.mood(now, decay_per_day),
                seed: left.pet_id,
                accessory: left.accessory.as_deref(),
                evolution: evolution_visual(&left),
            };
            let right_request = PetRenderRequest {
                species_id: &right.species,
                stage: right.stage(now),
                mood: right.mood(now, decay_per_day),
                seed: right.pet_id,
                accessory: right.accessory.as_deref(),
                evolution: evolution_visual(&right),
            };
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_versus_card(&left_request, &right_request);
            Ok(InteractionAttachment::bytes(
                "pet-brawl-versus.png",
                file.bytes().to_vec(),
            ))
        })
        .await
        .map_err(join_error)?
    }

    async fn render_brawl_winner(
        &self,
        brawl_id: i64,
        guild_id: i64,
    ) -> Result<Option<InteractionAttachment>, String> {
        let database_path = self.state.database_path.clone();
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let brawl = PetBrawlRepository::new(&database_path)
                .get_brawl(brawl_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let Some(winner_id) = brawl.and_then(|brawl| brawl.winner_pet_id) else {
                return Ok(None);
            };
            let pet = PetRepository::new(&database_path)
                .get_pet_by_id(winner_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "winner pet disappeared before brawl art".to_owned())?;
            let now = Utc::now().timestamp();
            let request = PetRenderRequest {
                species_id: &pet.species,
                stage: pet.stage(now),
                mood: PetMood::Happy,
                seed: pet.pet_id,
                accessory: pet.accessory.as_deref(),
                evolution: evolution_visual(&pet),
            };
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_pet_card(&request);
            Ok(Some(InteractionAttachment::bytes(
                "pet-brawl-winner.png",
                file.bytes().to_vec(),
            )))
        })
        .await
        .map_err(join_error)?
    }
}

struct BuiltStatus {
    embed: Embed,
    attachment: Option<InteractionAttachment>,
    view: Option<StatusView>,
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} id is outside SQLite's supported range"))
}

fn command_path(options: &[InteractionOption]) -> Result<(&str, &[InteractionOption]), String> {
    let Some(option) = options.first() else {
        return Err("/pet requires a subcommand".to_owned());
    };
    match &option.value {
        InteractionValue::Subcommand(children) => Ok((&option.name, children.as_slice())),
        InteractionValue::SubcommandGroup(children) => {
            let nested = children
                .first()
                .ok_or_else(|| "empty /pet subcommand".to_owned())?;
            match &nested.value {
                InteractionValue::Subcommand(values) => Ok((&nested.name, values.as_slice())),
                _ => Err("invalid /pet subcommand payload".to_owned()),
            }
        }
        _ => Err("invalid /pet subcommand payload".to_owned()),
    }
}

fn option<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a InteractionValue> {
    options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
}

fn required_string(options: &[InteractionOption], name: &str) -> Result<String, String> {
    match option(options, name) {
        Some(InteractionValue::String(value)) => Ok(value.clone()),
        _ => Err(format!("missing /pet option {name}")),
    }
}

fn optional_string(options: &[InteractionOption], name: &str) -> Option<String> {
    match option(options, name) {
        Some(InteractionValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn optional_integer(options: &[InteractionOption], name: &str) -> Option<i64> {
    match option(options, name) {
        Some(InteractionValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn optional_bool(options: &[InteractionOption], name: &str) -> Option<bool> {
    match option(options, name) {
        Some(InteractionValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn empty_status() -> cama_domain::pet::PetStatus {
    cama_domain::pet::PetStatus {
        pet: None,
        hunger: 0,
        stage: None,
        mood: None,
        age_seconds: 0,
        supplies: Some(BTreeMap::new()),
        last_dead: None,
        dig_work_units: 0,
        dig_work_rate: 0,
        evolution_hint: None,
    }
}

fn optional_user(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<(i64, String, bool)>, String> {
    match option(options, name) {
        None => Ok(None),
        Some(InteractionValue::User {
            id,
            display_name,
            is_bot,
        }) => Ok(Some((
            signed_id(*id, "target user")?,
            display_name.clone().unwrap_or_else(|| format!("User {id}")),
            is_bot.unwrap_or(false),
        ))),
        _ => Err(format!("invalid /pet user option {name}")),
    }
}

fn required_user(options: &[InteractionOption], name: &str) -> Result<(i64, String, bool), String> {
    optional_user(options, name)?.ok_or_else(|| format!("missing /pet option {name}"))
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

async fn followup_error(
    responder: &Arc<dyn InteractionResponder>,
    error: String,
) -> Result<(), String> {
    responder
        .followup(InteractionResponse::message(format!("❌ {error}")).ephemeral())
        .await
        .map_err(|response_error| response_error.to_string())
}

fn color(color: EmbedColor) -> u32 {
    match color {
        EmbedColor::Blue => 0x34_98_db,
        EmbedColor::Green => 0x57_f287,
        EmbedColor::Gold => 0xf1_c4_0f,
        EmbedColor::Orange => 0xf3_9c_12,
        EmbedColor::Red => 0xe7_4c_3c,
        EmbedColor::Slate => 0x5d_6d_7e,
        EmbedColor::Custom(value) => value,
    }
}

fn interaction_embed(
    mut embed: Embed,
    attachment: Option<&InteractionAttachment>,
) -> InteractionEmbed {
    if let Some(attachment) = attachment
        && embed
            .image
            .as_deref()
            .is_some_and(|image| image.starts_with("attachment://"))
    {
        embed.image = Some(format!("attachment://{}", attachment.filename));
    }
    let mut output = InteractionEmbed::titled(embed.title)
        .description(embed.description)
        .color(color(embed.color));
    if let Some(image) = embed.image {
        output = output.image(image);
    }
    if let Some(footer) = embed.footer {
        output = output.footer(footer);
    }
    for field in embed.fields {
        output = output.field(field.name, field.value, field.inline);
    }
    output
}

fn response_embed(
    embed: Embed,
    attachment: Option<InteractionAttachment>,
    components: Vec<InteractionActionRow>,
    ephemeral: bool,
) -> InteractionResponse {
    let mut response = InteractionResponse::message("")
        .embed(interaction_embed(embed, attachment.as_ref()))
        .action_rows(components);
    if let Some(attachment) = attachment {
        response = response.attachment(attachment);
    }
    if ephemeral {
        response = response.ephemeral();
    }
    response
}

fn confirmation_response(
    embed: Embed,
    buttons: Vec<InteractionButton>,
    ephemeral: bool,
) -> InteractionResponse {
    response_embed(
        embed,
        None,
        vec![InteractionActionRow::buttons(buttons)],
        ephemeral,
    )
}

fn status_response(built: BuiltStatus, token: Option<&str>, public: bool) -> InteractionResponse {
    let mut rows = Vec::new();
    if let (Some(token), Some(view)) = (token, built.view) {
        let mut buttons = Vec::new();
        for button in view.buttons {
            let (verb, value, emoji) = match button.action {
                StatusAction::Feed(value) => ("feed", value, None),
                StatusAction::Buy(value) => ("buy", value, None),
                StatusAction::SaltLick => ("salt", SALT_LICK.item_id.to_owned(), Some("🧂")),
            };
            let mut interaction_button =
                InteractionButton::new(format!("pet:status:{token}:{verb}:{value}"), button.label)
                    .disabled(button.disabled)
                    .style(InteractionButtonStyle::Secondary);
            if let Some(emoji) = emoji {
                interaction_button = interaction_button.emoji(emoji);
            }
            buttons.push(interaction_button);
        }
        for chunk in buttons.chunks(5) {
            rows.push(InteractionActionRow::buttons(chunk.to_vec()));
        }
    }
    response_embed(built.embed, built.attachment, rows, !public)
}

fn disabled_status_response(mut response: InteractionResponse) -> InteractionResponse {
    for row in &mut response.components {
        for button in &mut row.buttons {
            button.disabled = true;
        }
        if let Some(select) = &mut row.string_select {
            select.disabled = true;
        }
    }
    response
}

fn status_timeout_is_current(view: &StatusViewState, generation: u64, now: Instant) -> bool {
    view.generation == generation && now >= view.expires_at
}

fn challenge_components(brawl_id: i64) -> Vec<InteractionActionRow> {
    vec![InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("pet:brawl:{brawl_id}:accept"), "Accept")
            .emoji("⚔️")
            .style(InteractionButtonStyle::Success),
        InteractionButton::new(format!("pet:brawl:{brawl_id}:decline"), "Decline")
            .style(InteractionButtonStyle::Secondary),
        InteractionButton::new(format!("pet:brawl:{brawl_id}:withdraw"), "Withdraw")
            .style(InteractionButtonStyle::Secondary),
    ])]
}

fn battle_components(view: &BattleViewModel) -> Vec<InteractionActionRow> {
    let buttons = view
        .buttons
        .iter()
        .filter_map(|button| {
            let custom_id = button.custom_id.clone()?;
            let style = match button.style {
                ButtonStyle::Primary => InteractionButtonStyle::Primary,
                ButtonStyle::Secondary => InteractionButtonStyle::Secondary,
                ButtonStyle::Success => InteractionButtonStyle::Success,
            };
            let mut interaction_button =
                InteractionButton::new(custom_id, button.label.clone()).style(style);
            if let Some(emoji) = &button.emoji {
                interaction_button = interaction_button.emoji(emoji.clone());
            }
            Some(interaction_button)
        })
        .collect();
    vec![InteractionActionRow::buttons(buttons)]
}

fn view_components(view: &ViewModel) -> Vec<InteractionActionRow> {
    match view {
        ViewModel::Challenge(view) => challenge_components(view.brawl_id),
        ViewModel::Battle(view) => battle_components(view),
    }
}

fn brawl_interaction(
    user_id: i64,
    guild_id: i64,
    channel_id: i64,
    rate_allowed: bool,
) -> InteractionModel {
    InteractionModel {
        user_id,
        display_name: format!("User {user_id}"),
        is_bot: false,
        guild_id: Some(guild_id),
        channel_id,
        parent_channel_id: None,
        rate_allowed,
        retry_after_seconds: 0,
    }
}

fn last_message(
    discord: &InMemoryDiscord,
) -> Option<&cama_app::pet_brawl_commands::OutboundMessage> {
    discord.events.iter().rev().find_map(|event| match event {
        DiscordEvent::Initial(message)
        | DiscordEvent::FollowupAttempt(message)
        | DiscordEvent::Edited { message, .. } => Some(message),
        DiscordEvent::Deferred { .. } | DiscordEvent::WaitingForSessionLock { .. } => None,
    })
}

fn last_edited_message(
    discord: &InMemoryDiscord,
) -> Option<&cama_app::pet_brawl_commands::OutboundMessage> {
    discord.events.iter().rev().find_map(|event| match event {
        DiscordEvent::Edited { message, .. } => Some(message),
        DiscordEvent::Initial(_)
        | DiscordEvent::Deferred { .. }
        | DiscordEvent::FollowupAttempt(_)
        | DiscordEvent::WaitingForSessionLock { .. } => None,
    })
}

fn outbound_response(
    message: &cama_app::pet_brawl_commands::OutboundMessage,
    ephemeral: bool,
) -> InteractionResponse {
    let mut response = InteractionResponse::message(message.content.clone().unwrap_or_default());
    if ephemeral || message.ephemeral {
        response = response.ephemeral();
    }
    if let Some(embed) = &message.embed {
        response = response.embed(outbound_embed(embed));
    }
    if !message.allowed_user_mentions.is_empty() {
        response = response.with_user_mentions(
            message
                .allowed_user_mentions
                .iter()
                .filter_map(|id| u64::try_from(*id).ok())
                .collect(),
        );
    }
    if let Some(view) = &message.view {
        response = response.action_rows(view_components(view));
    }
    response
}

fn outbound_embed(embed: &cama_app::pet_brawl_commands::EmbedModel) -> InteractionEmbed {
    let mut output = InteractionEmbed::titled(&embed.title)
        .description(&embed.description)
        .color(embed.color.unwrap_or(0x5d_6d_7e));
    if let Some(footer) = &embed.footer {
        output = output.footer(footer);
    }
    if let Some(image) = &embed.image_attachment {
        output = output.image(format!("attachment://{image}"));
    }
    for field in &embed.fields {
        output = output.field(&field.name, &field.value, field.inline);
    }
    output
}

fn feed_copy(item: &str, outcome: &cama_domain::pet::FeedOutcome, flavor: Option<&str>) -> String {
    let food = FOOD_ITEMS.iter().find(|food| food.item_id == item);
    let flavor_line = flavor.map_or_else(String::new, |line| format!("\n💬 {line}"));
    if outcome.spat {
        return format!(
            "💢 **{}** spat the {} straight back at you. The temperament of legends. ({} left){}",
            outcome.pet.name,
            food.map_or(item, |food| food.display_name),
            outcome.remaining_qty,
            flavor_line
        );
    }
    format!(
        "{} **{}** munches the {}. Fullness {} → **{}** `{}` · {} left · {} feeds left today{}",
        food.map_or("🍽️", |food| food.emoji),
        outcome.pet.name,
        food.map_or(item, |food| food.display_name),
        outcome.old_hunger,
        outcome.new_hunger,
        cama_app::pet_commands::hunger_bar(outcome.new_hunger),
        outcome.remaining_qty,
        outcome.feeds_left_today,
        flavor_line
    )
}

fn buy_copy(item: &str, outcome: &cama_app::pet::BuyOutcome) -> String {
    if item == SALT_LICK.item_id {
        return format!(
            "🧂 **{}** is thoroughly pampered until <t:{}:t> (-{} {JOPACOIN_EMOTE})",
            outcome
                .pet
                .as_ref()
                .map_or("Your cama", |pet| pet.name.as_str()),
            outcome.pampered_until.unwrap_or_default(),
            outcome.total_cost
        );
    }
    let food = FOOD_ITEMS.iter().find(|food| food.item_id == item);
    format!(
        "{} Bought {}× {} for {} {JOPACOIN_EMOTE} — you now have ×{}.",
        food.map_or("🎒", |food| food.emoji),
        outcome.qty,
        food.map_or(item, |food| food.display_name),
        outcome.total_cost,
        outcome.new_qty.unwrap_or_default()
    )
}

fn eating_warning_embed(pet: &Pet) -> Embed {
    Embed::new(
        "🍽️ A terrible hunger stirs…",
        format!(
            "Eat **{}**?\n\nYou may be rewarded handsomely—but bad karma will tax your future earnings until you win your way free.\n\n**This cannot be undone. {} will be gone forever.**",
            pet.name, pet.name
        ),
        EmbedColor::Slate,
    )
}

fn evolution_visual(pet: &Pet) -> Option<EvolutionVisual> {
    let calling = pet.evolution_calling.as_deref().and_then(|value| {
        PetCalling::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    })?;
    let primary = pet.evolution_primary.as_deref().and_then(|value| {
        PetInstinct::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    })?;
    let secondary = pet.evolution_secondary.as_deref().and_then(|value| {
        PetInstinct::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    });
    Some(EvolutionVisual {
        calling,
        primary,
        secondary,
    })
}

fn join_error(error: JoinError) -> String {
    format!("pet blocking task failed: {error}")
}

fn entropy_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    nanos ^ (std::process::id() as u64).rotate_left(17)
}

#[derive(Clone)]
struct ProductionPetFlavorRuntime {
    service: Arc<PetFlavorService>,
}

impl ProductionPetFlavorRuntime {
    fn new(
        database_path: impl AsRef<Path>,
        ai_service: Option<Arc<AIService>>,
        ai_default: bool,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let ai =
            ai_service.map(|service| Arc::new(ProductionPetFlavorLlm(service)) as Arc<dyn LlmPort>);
        Self {
            service: Arc::new(PetFlavorService::new(
                ai,
                Some(Arc::new(ProductionPetGuildAi(GuildConfigRepository::new(
                    &database_path,
                    ai_default,
                )))),
                Some(Arc::new(ProductionPetFlavorData::new(&database_path))),
                Arc::new(SystemPetFlavorClock),
                Box::new(ProductionPetFlavorRng),
            )),
        }
    }

    fn generate(
        &self,
        event: PetFlavorEvent,
        pet: &Pet,
        status: Option<&cama_domain::pet::PetStatus>,
    ) -> Option<String> {
        let event = match event {
            PetFlavorEvent::Adopted => FlavorEvent::Adopted,
            PetFlavorEvent::Status => FlavorEvent::Status,
            PetFlavorEvent::Fed => FlavorEvent::Fed,
            PetFlavorEvent::Spat => FlavorEvent::Spat,
            PetFlavorEvent::Hatched => FlavorEvent::Hatched,
            PetFlavorEvent::Evolved => FlavorEvent::Evolved,
            PetFlavorEvent::Died => FlavorEvent::Died,
        };
        Some(self.service.generate(event, pet, status))
    }
}

#[derive(Clone)]
struct ProductionPetFlavorLlm(Arc<AIService>);

impl LlmPort for ProductionPetFlavorLlm {
    fn call_with_tools(&self, request: LlmRequest) -> Result<FlavorToolCallResult, String> {
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
        Ok(FlavorToolCallResult {
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
        let connection =
            Connection::open(&self.database_path).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
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
        cama_app::pet_flavor::LINE_TOOL_NAME => (
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

#[derive(Clone)]
struct RuntimeBrawlPetPort {
    database_path: PathBuf,
    decay_per_day: i64,
}

impl RuntimeBrawlPetPort {
    fn new(database_path: impl AsRef<Path>, decay_per_day: i64) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            decay_per_day,
        }
    }

    fn with_service<T>(&self, operation: impl FnOnce(&mut ProductionPetService) -> T) -> T {
        let mut service = SqlitePetCommandService::new(
            &self.database_path,
            SeededPetRandom::new(entropy_seed()),
            SystemPetClock,
            self.decay_per_day,
        );
        operation(&mut service)
    }
}

impl PetPort for RuntimeBrawlPetPort {
    fn decay_per_day(&self) -> i64 {
        self.decay_per_day
    }

    fn living_pet(&mut self, discord_id: i64, guild_id: Option<i64>, now: i64) -> Option<Pet> {
        self.with_service(|service| {
            service
                .service_mut()
                .living_pet(discord_id, guild_id, now)
                .ok()
                .flatten()
        })
    }

    fn living_pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>, now: i64) -> Option<Pet> {
        let pet = PetRepository::new(&self.database_path)
            .get_pet_by_id(pet_id, guild_id)
            .ok()
            .flatten()?;
        if pet.died_at.is_some() || now < pet.hatched_at {
            return None;
        }
        Some(pet)
    }

    fn get_pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Option<Pet> {
        PetRepository::new(&self.database_path)
            .get_pet_by_id(pet_id, guild_id)
            .ok()
            .flatten()
    }

    fn available_solo_training_sessions(&self, pet: &Pet, now: i64) -> i64 {
        PetRepository::available_solo_training_sessions(pet, now).unwrap_or_default()
    }

    fn train_solo_atomic(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        now: i64,
        proposed_stat: Option<&str>,
    ) -> PortResult<SoloTrainingPortOutcome> {
        PetRepository::new(&self.database_path)
            .train_solo_atomic(pet_id, guild_id, now, proposed_stat)
            .map(|outcome| SoloTrainingPortOutcome {
                sessions_used: outcome.sessions_used,
                sessions_available: outcome.sessions_available,
                xp_delta: outcome.xp_delta,
                stat_gain: outcome.stat_gain,
            })
            .map_err(|error| PortError::new(error_code(&error)))
    }
}

#[derive(Clone)]
struct RuntimeBrawlPort {
    repository: PetBrawlRepository,
}

impl RuntimeBrawlPort {
    fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            repository: PetBrawlRepository::new(database_path),
        }
    }
}

impl PetBrawlPort for RuntimeBrawlPort {
    fn get_brawl(&mut self, brawl_id: i64, guild_id: Option<i64>) -> PortResult<Option<PetBrawl>> {
        self.repository
            .get_brawl(brawl_id, guild_id)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn create_brawl_atomic(
        &mut self,
        guild_id: Option<i64>,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        challenger_pet_id: i64,
        now: i64,
        expires_at: i64,
        wager: i64,
        fee: i64,
    ) -> PortResult<PetBrawl> {
        self.repository
            .create_brawl_atomic(
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                challenger_pet_id,
                now,
                expires_at,
                wager,
                fee,
            )
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn accept_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        recipient_pet_id: i64,
        now: i64,
    ) -> PortResult<PetBrawl> {
        self.repository
            .accept_atomic(brawl_id, guild_id, recipient_id, recipient_pet_id, now)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn decline_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        now: i64,
    ) -> PortResult<()> {
        self.repository
            .decline_atomic(brawl_id, guild_id, recipient_id, now)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn withdraw_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
        now: i64,
    ) -> PortResult<()> {
        self.repository
            .withdraw_atomic(brawl_id, guild_id, challenger_id, now)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn void_atomic(&mut self, brawl_id: i64, guild_id: Option<i64>, now: i64) -> PortResult<()> {
        self.repository
            .void_atomic(brawl_id, guild_id, now)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn settle_draw_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        participant_pet_ids: (i64, i64),
        rounds: i64,
        now: i64,
    ) -> PortResult<DrawSettlementResult> {
        self.repository
            .settle_draw_atomic(brawl_id, guild_id, participant_pet_ids, rounds, now)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn settle_brawl_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        settlement: BrawlSettlement<'_>,
    ) -> PortResult<BrawlSettlementResult> {
        self.repository
            .settle_brawl_atomic(brawl_id, guild_id, settlement)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn get_pet_record(&mut self, pet_id: i64, guild_id: Option<i64>) -> PortResult<(i64, i64)> {
        self.repository
            .get_pet_record(pet_id, guild_id)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn get_records_for(
        &mut self,
        pet_ids: &[i64],
        guild_id: Option<i64>,
    ) -> PortResult<BTreeMap<i64, (i64, i64)>> {
        self.repository
            .get_records_for(pet_ids, guild_id)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn sweep_stale(&mut self, now: i64, active_ttl_seconds: i64) -> PortResult<SweepResult> {
        self.repository
            .sweep_stale(now, active_ttl_seconds)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

#[derive(Clone, Copy)]
struct RuntimeBrawlClock;

impl BrawlClock for RuntimeBrawlClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[derive(Clone)]
struct RuntimeBrawlRng {
    state: u64,
}

impl RuntimeBrawlRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }
}

impl ServiceRng for RuntimeBrawlRng {
    fn next_u64(&mut self) -> u64 {
        self.next()
    }

    fn choose_index(&mut self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            (self.next() as usize) % len
        }
    }

    fn random_unit(&mut self) -> f64 {
        self.next() as f64 / u64::MAX as f64
    }
}

#[derive(Clone)]
struct RuntimeEvolutionPort {
    repository: PetEvolutionRepository,
}

impl RuntimeEvolutionPort {
    fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            repository: PetEvolutionRepository::new(database_path),
        }
    }
}

impl PetEvolutionPort for RuntimeEvolutionPort {
    fn record_activity(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        activity: PetActivity,
        source_key: &str,
        occurred_at: i64,
    ) {
        let _ = self.repository.record_activity(
            discord_id,
            guild_id,
            activity,
            source_key,
            occurred_at,
        );
    }
}

fn error_code(error: &PetTrainingError) -> String {
    match error {
        PetTrainingError::NoPet => "no_pet",
        PetTrainingError::PetDead => "pet_dead",
        PetTrainingError::PetEgg => "pet_egg",
        PetTrainingError::TrainingComplete => "training_complete",
        PetTrainingError::NoTrainingSessions => "no_training_sessions",
        PetTrainingError::InBrawl => "in_brawl",
        PetTrainingError::InvalidGameDay => "invalid_game_day",
        PetTrainingError::PetRow(_) | PetTrainingError::Sqlite(_) => "validation_error",
    }
    .to_owned()
}

#[derive(Clone)]
struct RuntimeEatingRepository {
    repository: PetEatingRepository,
    database_path: PathBuf,
}

impl RuntimeEatingRepository {
    fn new(database_path: impl AsRef<Path>) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        Self {
            repository: PetEatingRepository::new(&database_path),
            database_path,
        }
    }
}

impl PetEatingRepositoryPort for RuntimeEatingRepository {
    type Error = PetEatingRepositoryError;

    fn living_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        _now: i64,
    ) -> Result<Option<Pet>, Self::Error> {
        self.repository.get_active_pet(discord_id, guild_id)
    }

    fn has_open_brawl(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<bool, Self::Error> {
        self.repository.has_open_brawl(discord_id, guild_id, now)
    }

    fn eat_adult_pet_atomic(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        request: &AppEatAdultPetRequest,
    ) -> Result<EatAdultPetCommit, Self::Error> {
        let outcome = self.repository.eat_adult_pet_atomic(DbEatAdultPetRequest {
            discord_id,
            guild_id,
            pet_id: request.pet_id,
            expected_last_fed_at: request.expected_last_fed_at,
            expected_hunger: request.expected_hunger,
            reward: request.reward,
            penalty_games: request.penalty_games,
            now: request.now,
        })?;
        Ok(EatAdultPetCommit {
            pet: outcome.pet,
            new_balance: outcome.new_balance,
            penalty_games_remaining: outcome.penalty_games_remaining,
        })
    }

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), Self::Error> {
        PetRepository::new(&self.database_path)
            .mark_death_announced(pet.pet_id, Some(pet.guild_id), Utc::now().timestamp())
            .map_err(PetEatingRepositoryError::PetRepository)
    }

    fn classify_error(error: &Self::Error) -> PetEatingRepositoryFailure {
        match error.failure() {
            Some(cama_db::pet_eating_repository::PetEatingFailure::InBrawl) => {
                PetEatingRepositoryFailure::InBrawl
            }
            Some(cama_db::pet_eating_repository::PetEatingFailure::NoPet)
            | Some(cama_db::pet_eating_repository::PetEatingFailure::PetDead) => {
                PetEatingRepositoryFailure::NoPet
            }
            Some(cama_db::pet_eating_repository::PetEatingFailure::NotAdult) => {
                PetEatingRepositoryFailure::PetNotAdult
            }
            Some(cama_db::pet_eating_repository::PetEatingFailure::StalePet) => {
                PetEatingRepositoryFailure::StalePet
            }
            Some(cama_db::pet_eating_repository::PetEatingFailure::PlayerNotFound) | None => {
                PetEatingRepositoryFailure::Unavailable
            }
        }
    }
}

#[derive(Clone, Copy)]
struct RuntimeEatingClock;

impl PetEatingClock for RuntimeEatingClock {
    fn now_seconds(&mut self) -> i64 {
        Utc::now().timestamp()
    }
}

#[derive(Clone)]
struct RuntimeEatingRng {
    state: u64,
}

impl RuntimeEatingRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }
}

impl PetEatingRandomPort for RuntimeEatingRng {
    fn inclusive_i64(&mut self, minimum: i64, maximum: i64) -> i64 {
        if maximum <= minimum {
            return minimum;
        }
        minimum + (self.next() % u64::try_from(maximum - minimum + 1).unwrap_or(1)) as i64
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use cama_app::pet::{Clock, SeededPetRandom, SystemPetClock};
    use cama_app::pet_sqlite::SqlitePetCommandService;
    use cama_db::core_repositories::{NewPlayer, PlayerRepository};
    use cama_db::pet_repository::PetRepository;
    use cama_db::schema_manager::initialize_or_migrate;
    use cama_domain::pet::{ADULT_AGE_SECONDS, EGG_HATCH_SECONDS, UNHATCHED_SPECIES};
    use tempfile::NamedTempFile;

    use super::*;
    use crate::discord_transport::{
        DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
        DiscordMessageSnapshot, DiscordTransport,
    };
    use crate::registration::{InteractionMessageDelivery, InteractionResponseError};
    use crate::reminder_provider::ReminderRegistrationProvider;
    use crate::serenity_transport::SerenityDiscordTransport;

    const OWNER: u64 = 77_001;
    const RECIPIENT: u64 = 77_004;
    const GUILD: u64 = 77_002;
    const CHANNEL: u64 = 77_003;

    struct TestResponder {
        fail_update: AtomicBool,
        fail_receipt: AtomicBool,
        defers: StdMutex<Vec<bool>>,
        responses: StdMutex<Vec<InteractionResponse>>,
        followups: StdMutex<Vec<InteractionResponse>>,
        updates: StdMutex<Vec<InteractionResponse>>,
        edits: StdMutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
        autocompletes: StdMutex<Vec<Vec<CommandOptionChoice>>>,
    }

    impl TestResponder {
        fn new(fail_update: bool) -> Self {
            Self {
                fail_update: AtomicBool::new(fail_update),
                fail_receipt: AtomicBool::new(false),
                defers: StdMutex::new(Vec::new()),
                responses: StdMutex::new(Vec::new()),
                followups: StdMutex::new(Vec::new()),
                updates: StdMutex::new(Vec::new()),
                edits: StdMutex::new(Vec::new()),
                autocompletes: StdMutex::new(Vec::new()),
            }
        }

        fn first_button_id(&self) -> String {
            self.followups
                .lock()
                .expect("followups")
                .first()
                .and_then(|response| response.components.first())
                .and_then(|row| row.buttons.first())
                .map(|button| button.custom_id.clone())
                .expect("confirmation button")
        }

        fn button_id_containing(&self, needle: &str) -> String {
            self.followups
                .lock()
                .expect("followups")
                .first()
                .and_then(|response| {
                    response
                        .components
                        .iter()
                        .flat_map(|row| row.buttons.iter())
                        .find(|button| button.custom_id.contains(needle))
                })
                .map(|button| button.custom_id.clone())
                .expect("matching button")
        }

        fn fail_receipt(&self) {
            self.fail_receipt.store(true, Ordering::Relaxed);
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

        async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
            self.defers.lock().expect("defers").push(_ephemeral);
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

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.followups.lock().expect("followups").push(response);
            Ok(())
        }

        async fn followup_with_receipt(
            &self,
            response: InteractionResponse,
        ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
            if self.fail_receipt.load(Ordering::Relaxed) {
                return Err(InteractionResponseError::new("simulated follow-up failure"));
            }
            self.followups.lock().expect("followups").push(response);
            Ok(Some(InteractionMessageReceipt {
                message_id: 1,
                channel_id: CHANNEL,
                delivery: InteractionMessageDelivery::InteractionFollowup,
            }))
        }

        async fn update(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            if self.fail_update.load(Ordering::Relaxed) {
                return Err(InteractionResponseError::new("simulated edit failure"));
            }
            self.updates.lock().expect("updates").push(response);
            Ok(())
        }

        async fn edit_message(
            &self,
            receipt: InteractionMessageReceipt,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.edits.lock().expect("edits").push((receipt, response));
            Ok(())
        }
    }

    struct ParentTransport {
        parent: StdMutex<Option<u64>>,
    }

    impl ParentTransport {
        fn new(parent: Option<u64>) -> Self {
            Self {
                parent: StdMutex::new(parent),
            }
        }
    }

    #[async_trait]
    impl DiscordTransport for ParentTransport {
        async fn fetch_message(
            &self,
            _channel_id: u64,
            _message_id: u64,
        ) -> Result<Option<DiscordMessageSnapshot>, String> {
            Ok(None)
        }

        async fn send_message(
            &self,
            channel_id: u64,
            _message: DiscordMessage,
        ) -> Result<DiscordMessageReceipt, String> {
            Ok(DiscordMessageReceipt {
                channel_id,
                message_id: 1,
                jump_url: String::new(),
            })
        }

        async fn edit_message(
            &self,
            _channel_id: u64,
            _message_id: u64,
            _message: DiscordMessage,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
            Ok(())
        }

        async fn create_public_thread(
            &self,
            _channel_id: u64,
            _message_id: u64,
            _name: &str,
        ) -> Result<u64, String> {
            Ok(1)
        }

        async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
            Ok(())
        }

        async fn archive_thread(
            &self,
            _thread_id: u64,
            _name: &str,
            _locked: bool,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn add_reaction(
            &self,
            _channel_id: u64,
            _message_id: u64,
            _emoji: &DiscordEmoji,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn remove_reaction(
            &self,
            _channel_id: u64,
            _message_id: u64,
            _emoji: &DiscordEmoji,
            _user_id: u64,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn clear_reaction(
            &self,
            _channel_id: u64,
            _message_id: u64,
            _emoji: &DiscordEmoji,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
            Ok(())
        }

        async fn send_direct_message(
            &self,
            _user_id: u64,
            _message: DiscordMessage,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn guild_member(
            &self,
            _guild_id: u64,
            _user_id: u64,
        ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
            Ok(None)
        }

        async fn channel_parent_id(
            &self,
            _guild_id: u64,
            _channel_id: u64,
        ) -> Result<Option<u64>, String> {
            Ok(*self.parent.lock().expect("parent"))
        }
    }

    #[derive(Clone, Copy)]
    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now(&self) -> i64 {
            self.0
        }
    }

    fn config() -> ApplicationConfig {
        ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("provider-test-token".to_owned()),
            "PET_CHANNEL_ID" => Some(CHANNEL.to_string()),
            _ => None,
        })
        .expect("provider test config")
    }

    #[tokio::test]
    async fn brawl_channel_gate_accepts_channel_and_parent_thread_only() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        let discord = Arc::new(ParentTransport::new(Some(CHANNEL)));
        let reminders =
            ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
        let provider = PetRegistrationProvider::new(
            database.path(),
            &config(),
            discord.clone(),
            reminders.hooks(),
            None,
        );
        assert!(
            provider
                .handler
                .brawl_channel_allowed(GUILD as i64, CHANNEL as i64)
                .await
        );
        assert!(
            provider
                .handler
                .brawl_channel_allowed(GUILD as i64, 77_004)
                .await
        );
        *discord.parent.lock().expect("parent") = Some(88_888);
        assert!(
            !provider
                .handler
                .brawl_channel_allowed(GUILD as i64, 77_004)
                .await
        );
    }

    #[tokio::test]
    async fn test_failed_challenge_delivery_voids_and_refunds() {
        let (database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let starting_balance = PlayerRepository::new(database.path())
            .get_by_id(OWNER as i64, Some(GUILD as i64))
            .expect("read starting challenger")
            .expect("starting challenger player")
            .jopacoin_balance;
        let responder = Arc::new(TestResponder::new(false));
        responder.fail_receipt();
        let result = provider
            .handler
            .handle(brawl_command(RECIPIENT, 20), responder)
            .await;
        assert!(
            result.is_err(),
            "the failed first delivery must be surfaced"
        );

        let connection = rusqlite::Connection::open(database.path()).expect("open brawl db");
        let (status, wager): (String, i64) = connection
            .query_row(
                "SELECT status,wager FROM pet_brawls ORDER BY brawl_id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("created challenge row");
        assert_eq!(status, "void");
        assert_eq!(wager, 20);
        let challenger = PlayerRepository::new(database.path())
            .get_by_id(OWNER as i64, Some(GUILD as i64))
            .expect("read challenger")
            .expect("challenger player");
        assert_eq!(challenger.jopacoin_balance, starting_balance);
    }

    #[tokio::test]
    async fn challenge_click_failure_uses_retained_interaction_followup_receipt() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(brawl_command(RECIPIENT, 0), initial.clone())
            .await
            .expect("challenge delivery");
        let decline = initial.first_button_id().replace(":accept", ":decline");
        let click = Arc::new(TestResponder::new(true));
        provider
            .handler
            .handle(component_as(decline, RECIPIENT), click.clone())
            .await
            .expect("retained challenge edit");
        assert!(click.updates.lock().expect("click updates").is_empty());
        let edits = initial.edits.lock().expect("initial edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].0.delivery,
            InteractionMessageDelivery::InteractionFollowup
        );
    }

    #[tokio::test(start_paused = true)]
    async fn challenge_timeout_without_raw_id_uses_retained_interaction_followup_receipt() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(brawl_command(RECIPIENT, 0), initial.clone())
            .await
            .expect("challenge delivery");
        let challenge_id = brawl_id_from_button(&initial.first_button_id());
        provider
            .handler
            .state
            .challenges
            .lock()
            .expect("challenges")
            .get_mut(&challenge_id)
            .expect("challenge state")
            .message_id = None;
        PetInteractionHandler::expire_challenge_once(
            Arc::clone(&provider.handler.state),
            challenge_id,
        )
        .await
        .expect("challenge timeout delivery");
        let edits = initial.edits.lock().expect("initial edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].0.delivery,
            InteractionMessageDelivery::InteractionFollowup
        );
        assert_eq!(
            edits[0].1.embeds[0].title.as_deref(),
            Some("🌾 Challenge expired")
        );
    }

    #[tokio::test]
    async fn battle_click_failure_uses_retained_interaction_followup_receipt() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(brawl_command(RECIPIENT, 0), initial.clone())
            .await
            .expect("challenge delivery");
        let accept = initial.first_button_id();
        provider
            .handler
            .handle(
                component_as(accept.clone(), RECIPIENT),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("battle start");
        let battle_id = initial
            .followups
            .lock()
            .expect("followups")
            .first()
            .map(|response| brawl_id_from_button(&response.components[0].buttons[0].custom_id))
            .expect("challenge id");
        let move_id = provider
            .handler
            .state
            .battle_views
            .lock()
            .expect("battle views")
            .get(&battle_id)
            .expect("battle state")
            .view_model()
            .buttons[0]
            .custom_id
            .clone()
            .expect("move id");
        let click = Arc::new(TestResponder::new(true));
        provider
            .handler
            .handle(component_as(move_id, OWNER), click.clone())
            .await
            .expect("retained battle edit");
        assert!(click.updates.lock().expect("click updates").is_empty());
        let edits = initial.edits.lock().expect("initial edits");
        assert!(edits.iter().any(|(receipt, _)| {
            receipt.delivery == InteractionMessageDelivery::InteractionFollowup
        }));
    }

    #[tokio::test]
    async fn battle_round_transition_accepts_next_round_clicks() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(brawl_command(RECIPIENT, 0), initial.clone())
            .await
            .expect("challenge delivery");
        let accept = initial.first_button_id();
        provider
            .handler
            .handle(
                component_as(accept.clone(), RECIPIENT),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("battle start");
        let battle_id = brawl_id_from_button(&accept);
        let first_move = provider
            .handler
            .state
            .battle_views
            .lock()
            .expect("battle views")
            .get(&battle_id)
            .expect("battle state")
            .view_model()
            .buttons[0]
            .custom_id
            .clone()
            .expect("first move");
        provider
            .handler
            .handle(
                component_as(first_move.clone(), OWNER),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("challenger first move");
        provider
            .handler
            .handle(
                component_as(first_move, RECIPIENT),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("recipient first move");
        let second_move = provider
            .handler
            .state
            .battle_views
            .lock()
            .expect("next-round battle views")
            .get(&battle_id)
            .expect("next-round battle state")
            .view_model()
            .buttons[0]
            .custom_id
            .clone()
            .expect("next-round move");
        assert!(second_move.contains(":1:"));
        provider
            .handler
            .handle(
                component_as(second_move, OWNER),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("challenger next-round move");
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_battle_timeout_uses_retained_interaction_followup_receipt() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(brawl_command(RECIPIENT, 0), initial.clone())
            .await
            .expect("challenge delivery");
        let accept = initial.first_button_id();
        provider
            .handler
            .handle(
                component_as(accept, RECIPIENT),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("battle start");
        let battle_id = brawl_id_from_button(&initial.first_button_id());
        provider
            .handler
            .state
            .battle_views
            .lock()
            .expect("battle views")
            .get_mut(&battle_id)
            .expect("battle state")
            .message_id = None;
        assert!(
            !PetInteractionHandler::advance_battle_timeout_once(
                Arc::clone(&provider.handler.state),
                battle_id,
            )
            .await
            .expect("first battle timeout delivery")
        );
        assert!(
            PetInteractionHandler::advance_battle_timeout_once(
                Arc::clone(&provider.handler.state),
                battle_id,
            )
            .await
            .expect("terminal battle timeout delivery")
        );
        let edits = initial.edits.lock().expect("initial edits");
        assert!(
            edits.len() >= 2,
            "first and terminal timeouts edit the receipt"
        );
        assert!(edits.iter().all(|(receipt, _)| {
            receipt.delivery == InteractionMessageDelivery::InteractionFollowup
        }));
    }

    fn fixture(adult: bool) -> (NamedTempFile, PetRegistrationProvider, i64) {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(
                i64::try_from(OWNER).expect("owner id"),
                "Provider Test Owner",
                Some(i64::try_from(GUILD).expect("guild id")),
            ))
            .expect("register owner");
        players
            .update_balance(
                i64::try_from(OWNER).expect("owner id"),
                Some(i64::try_from(GUILD).expect("guild id")),
                10_000,
            )
            .expect("fund owner");

        let now = SystemPetClock.now();
        let adopted_at = if adult {
            now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10
        } else {
            now - EGG_HATCH_SECONDS - 10
        };
        let mut service = SqlitePetCommandService::new(
            database.path(),
            SeededPetRandom::new(11),
            FixedClock(adopted_at),
            20,
        );
        let adopted = match service.adopt(
            i64::try_from(OWNER).expect("owner id"),
            Some(i64::try_from(GUILD).expect("guild id")),
            "Provider Test Pet",
            "standard",
        ) {
            ServiceResult::Success(outcome) => outcome.pet,
            ServiceResult::Failure { error, .. } => panic!("adopt fixture pet: {error}"),
        };
        drop(service);
        PetRepository::new(database.path())
            .resolve_hatch_species(
                adopted.pet_id,
                Some(i64::try_from(GUILD).expect("guild id")),
                "common_cama",
                now,
            )
            .expect("resolve fixture pet");
        rusqlite::Connection::open(database.path())
            .expect("open fixture")
            .execute(
                "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1 WHERE pet_id=?2",
                rusqlite::params![now, adopted.pet_id],
            )
            .expect("refresh fixture hunger");

        let discord = Arc::new(SerenityDiscordTransport::new());
        let reminder_provider =
            ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
        let provider = PetRegistrationProvider::new_without_ai(
            database.path(),
            &config(),
            discord,
            reminder_provider.hooks(),
        );
        (database, provider, adopted.pet_id)
    }

    fn empty_fixture() -> (NamedTempFile, PetRegistrationProvider) {
        let database = NamedTempFile::new().expect("temporary empty database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(
                i64::try_from(OWNER).expect("owner id"),
                "Provider Test Owner",
                Some(i64::try_from(GUILD).expect("guild id")),
            ))
            .expect("register empty fixture owner");
        players
            .update_balance(
                i64::try_from(OWNER).expect("owner id"),
                Some(i64::try_from(GUILD).expect("guild id")),
                10_000,
            )
            .expect("fund empty fixture owner");
        let discord = Arc::new(SerenityDiscordTransport::new());
        let reminder_provider =
            ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
        let provider = PetRegistrationProvider::new_without_ai(
            database.path(),
            &config(),
            discord,
            reminder_provider.hooks(),
        );
        (database, provider)
    }

    fn brawl_fixture() -> (NamedTempFile, PetRegistrationProvider, i64, i64) {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        let players = PlayerRepository::new(database.path());
        for (user_id, name) in [
            (OWNER, "Provider Test Challenger"),
            (RECIPIENT, "Provider Test Recipient"),
        ] {
            players
                .add(&NewPlayer::new(
                    i64::try_from(user_id).expect("user id"),
                    name,
                    Some(i64::try_from(GUILD).expect("guild id")),
                ))
                .expect("register brawl player");
            players
                .update_balance(
                    i64::try_from(user_id).expect("user id"),
                    Some(i64::try_from(GUILD).expect("guild id")),
                    10_000,
                )
                .expect("fund brawl player");
        }
        let now = SystemPetClock.now();
        let adopted_at = now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10;
        let mut pet_ids = Vec::new();
        for (user_id, name, seed) in [
            (OWNER, "Provider Test Challenger Pet", 11_u64),
            (RECIPIENT, "Provider Test Recipient Pet", 13_u64),
        ] {
            let mut service = SqlitePetCommandService::new(
                database.path(),
                SeededPetRandom::new(seed),
                FixedClock(adopted_at),
                20,
            );
            let adopted = match service.adopt(
                i64::try_from(user_id).expect("user id"),
                Some(i64::try_from(GUILD).expect("guild id")),
                name,
                "standard",
            ) {
                ServiceResult::Success(outcome) => outcome.pet,
                ServiceResult::Failure { error, .. } => panic!("adopt brawl fixture pet: {error}"),
            };
            drop(service);
            PetRepository::new(database.path())
                .resolve_hatch_species(
                    adopted.pet_id,
                    Some(i64::try_from(GUILD).expect("guild id")),
                    "common_cama",
                    now,
                )
                .expect("resolve brawl fixture pet");
            rusqlite::Connection::open(database.path())
                .expect("open brawl fixture")
                .execute(
                    "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1 WHERE pet_id=?2",
                    rusqlite::params![now, adopted.pet_id],
                )
                .expect("refresh brawl fixture hunger");
            pet_ids.push(adopted.pet_id);
        }
        let discord = Arc::new(SerenityDiscordTransport::new());
        let reminder_provider =
            ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
        let provider = PetRegistrationProvider::new_without_ai(
            database.path(),
            &config(),
            discord,
            reminder_provider.hooks(),
        );
        (database, provider, pet_ids[0], pet_ids[1])
    }

    fn altar_command() -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 1,
            name: "pet".to_owned(),
            user_id: OWNER,
            user_display_name: "Provider Test Owner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: "altar".to_owned(),
                value: InteractionValue::Subcommand(vec![InteractionOption {
                    name: "name".to_owned(),
                    value: InteractionValue::String("Rebirth".to_owned()),
                }]),
            }],
        }
    }

    fn eat_command() -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 1,
            name: "pet".to_owned(),
            user_id: OWNER,
            user_display_name: "Provider Test Owner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: "eat".to_owned(),
                value: InteractionValue::Subcommand(Vec::new()),
            }],
        }
    }

    fn status_command() -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 3,
            name: "pet".to_owned(),
            user_id: OWNER,
            user_display_name: "Provider Test Owner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: "status".to_owned(),
                value: InteractionValue::Subcommand(Vec::new()),
            }],
        }
    }

    fn brawl_command(target_id: u64, wager: i64) -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 4,
            name: "pet".to_owned(),
            user_id: OWNER,
            user_display_name: "Provider Test Challenger".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: "brawl".to_owned(),
                value: InteractionValue::Subcommand(vec![
                    InteractionOption {
                        name: "user".to_owned(),
                        value: InteractionValue::User {
                            id: target_id,
                            display_name: Some("Provider Test Recipient".to_owned()),
                            is_bot: Some(false),
                        },
                    },
                    InteractionOption {
                        name: "wager".to_owned(),
                        value: InteractionValue::Integer(wager),
                    },
                ]),
            }],
        }
    }

    fn autocomplete_request(current: &str) -> InteractionRequest {
        InteractionRequest::Autocomplete {
            interaction_id: 5,
            name: "pet".to_owned(),
            user_id: OWNER,
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            focused_option: "wear".to_owned(),
            focused_value: current.to_owned(),
            options: Vec::new(),
        }
    }

    fn leaf_request(subcommand: &str, options: Vec<InteractionOption>) -> InteractionRequest {
        InteractionRequest::Command {
            interaction_id: 6,
            name: "pet".to_owned(),
            user_id: OWNER,
            user_display_name: "Provider Test Challenger".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            options: vec![InteractionOption {
                name: subcommand.to_owned(),
                value: InteractionValue::Subcommand(options),
            }],
        }
    }

    fn component(custom_id: String) -> InteractionRequest {
        component_as(custom_id, OWNER)
    }

    fn component_as(custom_id: String, user_id: u64) -> InteractionRequest {
        InteractionRequest::Component {
            interaction_id: 2,
            custom_id,
            user_id,
            user_display_name: "Provider Test Owner".to_owned(),
            guild_id: Some(GUILD),
            channel_id: Some(CHANNEL),
            member_permissions: None,
            values: Vec::new(),
        }
    }

    fn brawl_id_from_button(custom_id: &str) -> i64 {
        custom_id
            .split(':')
            .nth(2)
            .expect("brawl id segment")
            .parse()
            .expect("numeric brawl id")
    }

    fn death_announcement(path: &Path, pet_id: i64) -> Option<i64> {
        PetRepository::new(path)
            .get_pet_by_id(pet_id, Some(i64::try_from(GUILD).expect("guild id")))
            .expect("read pet")
            .expect("pet row")
            .death_announced_at
    }

    #[test]
    fn command_schema_matches_python_pet_group() {
        let options = pet_options();
        assert_eq!(
            options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "adopt",
                "status",
                "train",
                "feed",
                "shop",
                "buy",
                "rename",
                "trinket",
                "brawl",
                "altar",
                "eat",
                "graveyard",
                "leaderboard",
            ]
        );
        let descriptions = options
            .iter()
            .map(|option| (option.name.clone(), option.description.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            descriptions,
            BTreeMap::from([
                ("adopt".to_owned(), "Adopt a mysterious cama egg".to_owned()),
                (
                    "status".to_owned(),
                    "Check on your cama (art, fullness, mood)".to_owned(),
                ),
                (
                    "train".to_owned(),
                    "Cash in up to three banked solo training sessions".to_owned(),
                ),
                (
                    "feed".to_owned(),
                    "Feed your cama from your supplies".to_owned()
                ),
                ("shop".to_owned(), "Browse cama food and treats".to_owned()),
                ("buy".to_owned(), "Buy cama supplies".to_owned()),
                ("rename".to_owned(), "Rename your cama (10 JC)".to_owned()),
                (
                    "trinket".to_owned(),
                    format!("Roll a Mystery Trinket ({TRINKET_COST} JC) or wear one you own"),
                ),
                (
                    "brawl".to_owned(),
                    "Challenge someone to a pet brawl, optionally for up to 100 JC".to_owned(),
                ),
                (
                    "altar".to_owned(),
                    "Sacrifice your cama for a better egg (dark, effective)".to_owned(),
                ),
                (
                    "eat".to_owned(),
                    "Make an irreversible choice about your adult cama".to_owned(),
                ),
                (
                    "graveyard".to_owned(),
                    "Visit the cama memorial garden".to_owned(),
                ),
                (
                    "leaderboard".to_owned(),
                    "The oldest living camas".to_owned()
                ),
            ])
        );

        let adopt = &options[0];
        assert_eq!(
            adopt
                .options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>(),
            vec!["name", "egg"]
        );
        assert!(adopt.options[0].required);
        assert!(!adopt.options[1].required);
        assert_eq!(
            adopt.options[1].description,
            format!("Gilded Egg: +{GILDED_EGG_PREMIUM} JC, no commons in the pool")
        );
        assert_eq!(
            adopt.options[1].choices,
            vec![
                CommandOptionChoice::String {
                    name: "Standard Egg".to_owned(),
                    value: "standard".to_owned(),
                },
                CommandOptionChoice::String {
                    name: format!("Gilded Egg (+{GILDED_EGG_PREMIUM} JC, uncommon or better)"),
                    value: "gilded".to_owned(),
                },
            ]
        );

        let status = &options[1];
        assert_eq!(
            status
                .options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "public"]
        );
        assert!(!status.options[0].required && !status.options[1].required);
        let feed = &options[3];
        assert!(feed.options[0].required);
        assert_eq!(
            feed.options[0].choices,
            FOOD_ITEMS
                .iter()
                .map(|food| CommandOptionChoice::String {
                    name: format!(
                        "{} ({} JC, +{} fullness)",
                        food.display_name, food.cost, food.restore
                    ),
                    value: food.item_id.to_owned(),
                })
                .collect::<Vec<_>>()
        );
        let buy = &options[5];
        assert_eq!(buy.options[1].min_integer, Some(1));
        assert_eq!(buy.options[1].max_integer, Some(MAX_BUY_QTY));
        assert_eq!(
            buy.options[0].choices.last(),
            Some(&CommandOptionChoice::String {
                name: format!(
                    "{} ({} JC, pampers instantly)",
                    SALT_LICK.display_name, SALT_LICK.cost
                ),
                value: SALT_LICK.item_id.to_owned(),
            })
        );
        assert!(options[6].options[0].required);
        assert!(options[7].options[0].autocomplete);
        assert!(options[8].options[0].required);
        assert_eq!(options[8].options[1].min_integer, Some(0));
        assert_eq!(options[8].options[1].max_integer, Some(100));
        assert!(options[9].options[0].required);
        assert!(!options[11].options[0].required);
    }

    #[test]
    fn status_timeout_disables_controls_and_ignores_stale_generation() {
        let response = InteractionResponse::message("status").action_rows(vec![
            InteractionActionRow::buttons(vec![InteractionButton::new(
                "pet:status:1:feed:grain",
                "Feed",
            )]),
        ]);
        let disabled = disabled_status_response(response.clone());
        assert_eq!(disabled.content, response.content);
        assert!(disabled.components[0].buttons[0].disabled);

        let now = Instant::now();
        let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::new(false));
        let mut view = StatusViewState {
            owner_id: 1,
            guild_id: 2,
            receipt: None,
            responder,
            response,
            public: false,
            generation: 0,
            expires_at: now + STATUS_TIMEOUT,
        };
        assert!(!status_timeout_is_current(&view, 0, now));
        view.generation = 1;
        view.expires_at = now;
        assert!(!status_timeout_is_current(&view, 0, now));
        assert!(status_timeout_is_current(&view, 1, now));
    }

    #[tokio::test(start_paused = true)]
    async fn status_timeout_edits_the_interaction_followup_receipt() {
        let (_database, provider, _pet_id) = fixture(false);
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), responder.clone())
            .await
            .expect("status response");
        assert_eq!(
            provider
                .handler
                .state
                .status_views
                .lock()
                .expect("status views")
                .len(),
            1
        );
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::time::advance(STATUS_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let edits = responder.edits.lock().expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].0.delivery,
            InteractionMessageDelivery::InteractionFollowup
        );
        assert!(
            edits[0]
                .1
                .components
                .iter()
                .flat_map(|row| row.buttons.iter())
                .all(|button| button.disabled)
        );
    }

    #[tokio::test]
    async fn status_refresh_edits_the_retained_interaction_followup() {
        let (_database, provider, _pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), initial.clone())
            .await
            .expect("status response");
        let click = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(component(initial.button_id_containing(":buy:")), click)
            .await
            .expect("status refresh");
        let edits = initial.edits.lock().expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].0.delivery,
            InteractionMessageDelivery::InteractionFollowup
        );
        assert!(!edits[0].1.components.is_empty());
    }

    #[tokio::test]
    async fn status_click_after_process_restart_is_expired_and_ephemeral() {
        let (_database, provider, _pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), initial.clone())
            .await
            .expect("status response");
        let button = initial.button_id_containing(":buy:");
        provider
            .handler
            .state
            .status_views
            .lock()
            .expect("status views")
            .clear();
        let restarted = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(component(button), restarted.clone())
            .await
            .expect("expired status click");
        let followups = restarted.followups.lock().expect("restart followups");
        assert_eq!(followups.len(), 0);
        assert!(
            restarted
                .updates
                .lock()
                .expect("restart updates")
                .is_empty()
        );
        let responses = restarted.responses.lock().expect("restart responses");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        assert!(responses[0].content.contains("expired"));
    }

    #[tokio::test(start_paused = true)]
    async fn altar_confirmation_timeout_uses_the_cold_altar_embed() {
        let (_database, provider, _pet_id) = fixture(false);
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), responder.clone())
            .await
            .expect("altar preview");
        tokio::task::yield_now().await;
        tokio::time::advance(CONFIRMATION_TIMEOUT).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let edits = responder.edits.lock().expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].1.embeds[0].title.as_deref(),
            Some("🕯️ The altar goes cold")
        );
        assert!(edits[0].1.components.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn eat_confirmation_timeout_uses_the_hunger_passes_embed() {
        let (_database, provider, _pet_id) = fixture(true);
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(eat_command(), responder.clone())
            .await
            .expect("eat preview");
        tokio::task::yield_now().await;
        tokio::time::advance(CONFIRMATION_TIMEOUT).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let edits = responder.edits.lock().expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].1.embeds[0].title.as_deref(),
            Some("The hunger passes")
        );
        assert!(edits[0].1.components.is_empty());
    }

    #[tokio::test]
    async fn test_failed_edit_falls_back_and_still_marks() {
        let (database, provider, pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), initial.clone())
            .await
            .expect("altar preview");
        let confirm_id = initial.first_button_id();
        let fallback = Arc::new(TestResponder::new(true));
        provider
            .handler
            .handle(component(confirm_id), fallback.clone())
            .await
            .expect("altar fallback delivery");
        assert!(fallback.updates.lock().expect("updates").is_empty());
        assert_eq!(fallback.followups.lock().expect("followups").len(), 1);
        assert!(death_announcement(database.path(), pet_id).is_some());
    }

    #[tokio::test]
    async fn test_confirm_performs_the_rite() {
        let (database, provider, pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), initial.clone())
            .await
            .expect("altar preview");
        let confirmation = initial.first_button_id();
        let success = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(component(confirmation), success)
            .await
            .expect("altar delivery");
        assert!(death_announcement(database.path(), pet_id).is_some());
        assert!(
            PetRepository::new(database.path())
                .get_unannounced_deaths(20)
                .expect("read unannounced deaths")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_cancel_keeps_the_pet() {
        let (database, provider, pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), initial.clone())
            .await
            .expect("altar preview");
        let cancel = initial.first_button_id().replace(":confirm", ":cancel");
        provider
            .handler
            .handle(component(cancel), Arc::new(TestResponder::new(false)))
            .await
            .expect("altar cancellation");
        let pet = PetRepository::new(database.path())
            .get_pet_by_id(pet_id, Some(i64::try_from(GUILD).expect("guild id")))
            .expect("read pet")
            .expect("pet row");
        assert!(pet.died_at.is_none());
        assert!(pet.death_announced_at.is_none());
    }

    #[tokio::test]
    async fn eat_failed_edit_falls_back_and_marks_death_for_restart_sweep() {
        let (database, provider, pet_id) = fixture(true);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(eat_command(), initial.clone())
            .await
            .expect("eat preview");
        let confirm = initial.first_button_id();
        let fallback = Arc::new(TestResponder::new(true));
        provider
            .handler
            .handle(component(confirm), fallback.clone())
            .await
            .expect("eat fallback delivery");
        assert!(fallback.updates.lock().expect("updates").is_empty());
        assert_eq!(fallback.followups.lock().expect("followups").len(), 1);
        assert!(death_announcement(database.path(), pet_id).is_some());
        assert!(!shared_direct_death_delivery_active(
            database.path(),
            pet_id
        ));
    }

    #[test]
    fn direct_death_delivery_suppression_is_nested_and_released() {
        let database = tempfile::NamedTempFile::new().expect("temporary guard path");
        let pet_id = 9_999_991;
        assert!(!shared_direct_death_delivery_active(
            database.path(),
            pet_id
        ));
        {
            let _guard = DirectDeathDeliveryGuard::new(database.path(), pet_id);
            assert!(shared_direct_death_delivery_active(database.path(), pet_id));
            {
                let _nested_guard = DirectDeathDeliveryGuard::new(database.path(), pet_id);
                assert!(shared_direct_death_delivery_active(database.path(), pet_id));
            }
            assert!(shared_direct_death_delivery_active(database.path(), pet_id));
        }
        assert!(!shared_direct_death_delivery_active(
            database.path(),
            pet_id
        ));
    }

    #[tokio::test]
    async fn test_sweep_tombstone_names_the_altar() {
        let (_database, provider, _pet_id) = fixture(false);
        let initial = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), initial.clone())
            .await
            .expect("altar preview");
        let success = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(component(initial.first_button_id()), success.clone())
            .await
            .expect("altar delivery");
        let update = success
            .updates
            .lock()
            .expect("updates")
            .first()
            .cloned()
            .expect("altar tombstone update");
        assert_eq!(
            update.embeds[0].title.as_deref(),
            Some("🩸 Provider Test Pet was given to the altar")
        );
    }

    #[tokio::test]
    async fn live_adopt_dispatch_creates_the_egg_and_media() {
        let (database, provider) = empty_fixture();
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(
                leaf_request(
                    "adopt",
                    vec![InteractionOption {
                        name: "name".to_owned(),
                        value: InteractionValue::String("Fresh Egg".to_owned()),
                    }],
                ),
                responder.clone(),
            )
            .await
            .expect("adopt dispatch");
        assert_eq!(
            responder.defers.lock().expect("adopt defers").as_slice(),
            &[false]
        );
        let followups = responder.followups.lock().expect("adopt followups");
        assert_eq!(followups.len(), 1);
        assert_eq!(followups[0].attachments.len(), 1);
        assert!(
            followups[0].embeds[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Fresh Egg"))
        );
        drop(followups);
        let connection = rusqlite::Connection::open(database.path()).expect("open adopt db");
        let (name, species): (String, String) = connection
            .query_row(
                "SELECT name,species FROM pets WHERE discord_id=?1 ORDER BY pet_id DESC LIMIT 1",
                [i64::try_from(OWNER).expect("owner id")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("adopted egg row");
        assert_eq!(name, "Fresh Egg");
        assert_eq!(species, UNHATCHED_SPECIES);
    }

    #[tokio::test]
    async fn live_trinket_roll_persists_equipment_and_autocomplete() {
        let (database, provider, pet_id) = fixture(true);
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(leaf_request("trinket", Vec::new()), responder.clone())
            .await
            .expect("trinket dispatch");
        assert_eq!(
            responder.defers.lock().expect("trinket defers").as_slice(),
            &[true]
        );
        let content = responder
            .followups
            .lock()
            .expect("trinket followups")
            .first()
            .map(|response| response.content.clone())
            .expect("trinket response");
        assert!(content.contains("Equipped!") || content.contains("duplicate"));
        let connection = rusqlite::Connection::open(database.path()).expect("open trinket db");
        let accessory: Option<String> = connection
            .query_row(
                "SELECT accessory FROM pets WHERE pet_id=?1",
                [pet_id],
                |row| row.get(0),
            )
            .expect("equipped accessory row");
        assert!(accessory.is_some());
        let autocomplete = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(autocomplete_request(""), autocomplete.clone())
            .await
            .expect("trinket autocomplete");
        assert_eq!(
            autocomplete
                .autocompletes
                .lock()
                .expect("trinket choices")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn live_status_renders_evolution_pampered_trinket_brawl_projection() {
        let (database, provider, pet_id, recipient_pet_id) = brawl_fixture();
        let now = SystemPetClock.now();
        let connection = rusqlite::Connection::open(database.path()).expect("open status db");
        connection
            .execute(
                "UPDATE pets
                 SET pampered_until=?1,
                     accessory='top_hat',
                     evolution_calling='oracle',
                     evolution_primary='wisdom',
                     evolution_secondary='fellowship',
                     evolved_at=?2
                 WHERE pet_id=?3",
                rusqlite::params![now + 3_600, now - 10, pet_id],
            )
            .expect("seed evolved status");
        connection
            .execute(
                "INSERT INTO pet_brawls (
                    guild_id,channel_id,challenger_id,recipient_id,
                    challenger_pet_id,recipient_pet_id,status,created_at,expires_at,
                    resolved_at,winner_id,winner_pet_id,loser_pet_id,rounds
                 ) VALUES (?1,?2,?3,?4,?5,?6,'done',?7,?7,?7,?3,?5,?6,1)",
                rusqlite::params![
                    i64::try_from(GUILD).expect("guild"),
                    i64::try_from(CHANNEL).expect("channel"),
                    i64::try_from(OWNER).expect("owner"),
                    i64::try_from(RECIPIENT).expect("recipient"),
                    pet_id,
                    recipient_pet_id,
                    now - 100,
                ],
            )
            .expect("seed brawl record");

        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), responder.clone())
            .await
            .expect("status dispatch");
        let followups = responder.followups.lock().expect("status followups");
        let embed = followups[0].embeds.first().expect("status embed");
        assert!(
            embed
                .title
                .as_deref()
                .is_some_and(|title| title.contains("Oracle"))
        );
        assert!(
            embed
                .description
                .as_deref()
                .is_some_and(|description| description.starts_with('_'))
        );
        let field = |name: &str| {
            embed
                .fields
                .iter()
                .find(|field| field.name == name)
                .map(|field| field.value.as_str())
        };
        assert!(field("Mood").is_some_and(|value| value.contains("pampered")));
        assert!(field("Calling").is_some_and(|value| value.contains("Paths: Wisdom + Fellowship")));
        assert_eq!(field("Trinket"), Some("🎩 Top Hat"));
        assert_eq!(field("⚔️ Brawls"), Some("1W · 0L"));
        assert!(field("🏋️ Training").is_some_and(|value| value.contains("Solo:")));
    }

    #[tokio::test]
    async fn live_graveyard_and_leaderboard_render_python_projection_fields() {
        let (database, provider, pet_id, recipient_pet_id) = brawl_fixture();
        let now = SystemPetClock.now();
        let connection = rusqlite::Connection::open(database.path()).expect("open presentation db");
        connection
            .execute(
                "UPDATE pets
                 SET evolution_calling='oracle', evolution_primary='wisdom',
                     evolution_secondary='fellowship', evolved_at=?1
                 WHERE pet_id=?2",
                rusqlite::params![now - 10, pet_id],
            )
            .expect("seed calling");
        connection
            .execute(
                "INSERT INTO pet_brawls (
                    guild_id,channel_id,challenger_id,recipient_id,
                    challenger_pet_id,recipient_pet_id,status,created_at,expires_at,
                    resolved_at,winner_id,winner_pet_id,loser_pet_id,rounds
                 ) VALUES (?1,?2,?3,?4,?5,?6,'done',?7,?7,?7,?4,?6,?5,1)",
                rusqlite::params![
                    i64::try_from(GUILD).expect("guild"),
                    i64::try_from(CHANNEL).expect("channel"),
                    i64::try_from(OWNER).expect("owner"),
                    i64::try_from(RECIPIENT).expect("recipient"),
                    pet_id,
                    recipient_pet_id,
                    now - 100,
                ],
            )
            .expect("seed leaderboard record");

        let altar = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(altar_command(), altar.clone())
            .await
            .expect("graveyard altar preview");
        provider
            .handler
            .handle(
                component(altar.first_button_id()),
                Arc::new(TestResponder::new(false)),
            )
            .await
            .expect("graveyard sacrifice");

        let graveyard = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(leaf_request("graveyard", Vec::new()), graveyard.clone())
            .await
            .expect("graveyard dispatch");
        let graveyard_embed =
            graveyard.followups.lock().expect("graveyard followup")[0].embeds[0].clone();
        assert!(
            graveyard_embed
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.contains("Oracle") && description.contains("<t:")
                })
        );
        assert!(
            graveyard_embed
                .fields
                .iter()
                .any(|field| field.name.starts_with("📖 Camadex"))
        );
        assert!(
            graveyard_embed
                .fields
                .iter()
                .any(|field| field.name.starts_with("Callings"))
        );

        let leaderboard = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(leaf_request("leaderboard", Vec::new()), leaderboard.clone())
            .await
            .expect("leaderboard dispatch");
        let leaderboard_embed =
            leaderboard.followups.lock().expect("leaderboard followup")[0].embeds[0].clone();
        assert!(
            leaderboard_embed
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.contains("🥇") && description.contains("😊")
                })
        );
        assert!(
            leaderboard_embed
                .description
                .as_deref()
                .is_some_and(|description| { description.contains("⚔️ 1W-0L") })
        );
    }

    #[tokio::test]
    async fn status_feed_button_and_footer_reset_on_game_day_rollover() {
        let (database, provider, pet_id) = fixture(true);
        let now = SystemPetClock.now();
        let today =
            cama_domain::game_date::game_date_for_timestamp(now as f64).expect("current game date");
        let connection = rusqlite::Connection::open(database.path()).expect("open rollover db");
        connection
            .execute(
                "UPDATE pets SET feeds_today=4,feed_date='2000-01-01' WHERE pet_id=?1",
                [pet_id],
            )
            .expect("seed stale feed counter");
        connection
            .execute(
                "INSERT INTO pet_supplies (discord_id,guild_id,item_id,qty,updated_at)
                 VALUES (?1,?2,'tango',1,?3)
                 ON CONFLICT(discord_id,guild_id,item_id)
                 DO UPDATE SET qty=excluded.qty,updated_at=excluded.updated_at",
                rusqlite::params![
                    i64::try_from(OWNER).expect("owner"),
                    i64::try_from(GUILD).expect("guild"),
                    now,
                ],
            )
            .expect("seed feed supply");
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), responder.clone())
            .await
            .expect("rollover status");
        let followup = responder.followups.lock().expect("rollover followup");
        let feed_button = followup[0]
            .components
            .iter()
            .flat_map(|row| row.buttons.iter())
            .find(|button| button.custom_id.ends_with(":feed:tango"))
            .expect("tango feed button");
        assert!(!feed_button.disabled, "stale prior-day count must reset");
        assert_eq!(
            followup[0].embeds[0].footer.as_deref(),
            Some("4/4 feeds left today")
        );
        assert!(!today.is_empty());
    }

    #[tokio::test]
    async fn live_migrated_sqlite_dispatches_all_pet_leaves_and_persistent_paths() {
        let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
        let leaves = vec![
            (
                "adopt",
                vec![InteractionOption {
                    name: "name".to_owned(),
                    value: InteractionValue::String("Second Egg".to_owned()),
                }],
                false,
            ),
            ("status", Vec::new(), true),
            ("train", Vec::new(), true),
            ("shop", Vec::new(), true),
            (
                "buy",
                vec![
                    InteractionOption {
                        name: "item".to_owned(),
                        value: InteractionValue::String("tango".to_owned()),
                    },
                    InteractionOption {
                        name: "qty".to_owned(),
                        value: InteractionValue::Integer(1),
                    },
                ],
                true,
            ),
            (
                "feed",
                vec![InteractionOption {
                    name: "item".to_owned(),
                    value: InteractionValue::String("tango".to_owned()),
                }],
                true,
            ),
            (
                "rename",
                vec![InteractionOption {
                    name: "name".to_owned(),
                    value: InteractionValue::String("Renamed Pet".to_owned()),
                }],
                true,
            ),
            ("trinket", Vec::new(), true),
            ("eat", Vec::new(), false),
            (
                "brawl",
                vec![
                    InteractionOption {
                        name: "user".to_owned(),
                        value: InteractionValue::User {
                            id: RECIPIENT,
                            display_name: Some("Provider Test Recipient".to_owned()),
                            is_bot: Some(false),
                        },
                    },
                    InteractionOption {
                        name: "wager".to_owned(),
                        value: InteractionValue::Integer(0),
                    },
                ],
                false,
            ),
            (
                "altar",
                vec![InteractionOption {
                    name: "name".to_owned(),
                    value: InteractionValue::String("Altar Egg".to_owned()),
                }],
                false,
            ),
            ("graveyard", Vec::new(), false),
            ("leaderboard", Vec::new(), false),
        ];
        for (name, options, expected_defer) in leaves {
            let responder = Arc::new(TestResponder::new(false));
            provider
                .handler
                .handle(leaf_request(name, options), responder.clone())
                .await
                .unwrap_or_else(|error| panic!("/pet {name} dispatch: {error}"));
            assert_eq!(
                responder.defers.lock().expect("defer records").as_slice(),
                &[expected_defer],
                "/pet {name} defer visibility"
            );
            assert!(
                !responder.followups.lock().expect("followups").is_empty(),
                "/pet {name} must deliver a response"
            );
            let followups = responder.followups.lock().expect("followups");
            if name == "status" {
                assert!(!followups[0].embeds.is_empty());
                assert!(!followups[0].attachments.is_empty(), "status media");
            }
            if name == "buy" {
                assert!(
                    followups
                        .iter()
                        .any(|response| { response.content.contains(JOPACOIN_EMOTE) })
                );
            }
        }

        let autocomplete = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(autocomplete_request(""), autocomplete.clone())
            .await
            .expect("trinket autocomplete");
        assert_eq!(
            autocomplete
                .autocompletes
                .lock()
                .expect("autocomplete records")
                .len(),
            1
        );

        let status = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(status_command(), status.clone())
            .await
            .expect("persistent status panel");
        let click = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(
                component(status.button_id_containing(":buy:")),
                click.clone(),
            )
            .await
            .expect("persistent status refresh");
        assert_eq!(status.edits.lock().expect("status edits").len(), 1);
        assert_eq!(
            click.defers.lock().expect("component defers").as_slice(),
            &[false]
        );
    }
}
