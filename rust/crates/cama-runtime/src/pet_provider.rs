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
use cama_app::ai_services::AIService;
use cama_app::pet::{SeededPetRandom, SystemPetClock};
use cama_app::pet_assets::{
    FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
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
use cama_app::pet_flavor::{PetFlavorEvent as FlavorEvent, PetFlavorService};
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::pet_brawl_repository::{
    BrawlSettlement, BrawlSettlementResult, DrawSettlementResult, PetBrawlRepository, SweepResult,
};
use cama_db::pet_eating_repository::{
    EatAdultPetRequest as DbEatAdultPetRequest, PetEatingRepository, PetEatingRepositoryError,
};
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::pet_repository::{PetRepository, PetTrainingError};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::pet::{
    FOOD_ITEMS, GILDED_EGG_PREMIUM, MAX_BUY_QTY, Pet, PetMood, PetStage, SALT_LICK, TRINKET_COST,
    UNHATCHED_SPECIES,
};
use cama_domain::pet_brawl::{PetBrawl, PetBrawlMove};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::service_result::ServiceResult;
use chrono::Utc;
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::application_config::ApplicationConfig;
use crate::discord_transport::DiscordTransport;
use crate::pet_death_delivery::DirectDeathDeliveryGuard;
#[cfg(all(test, feature = "runtime-test-dig"))]
use crate::pet_death_delivery::is_active as shared_direct_death_delivery_active;
pub use crate::pet_flavor_runtime::read_production_pet_flavor_context;
use crate::pet_flavor_runtime::{evolution_visual, production_pet_flavor_service};
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
        // The preview runs two SQLite reads on a connection with a five-second
        // busy timeout, which can outlast Discord's three-second window, so the
        // interaction is acknowledged first -- the same order command_altar
        // uses for its preview.
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
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
            Err(error) => return followup_error(&responder, error).await,
        };
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
        } else if let Some(dead) = data.0.last_dead.as_ref() {
            Some(self.render_tombstone(&dead.name, dead.pet_id).await?)
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

    /// Give every round its own full picking window.
    ///
    /// The embed promises `PET_BRAWL_TURN_SECONDS` per move and the legacy
    /// views built a fresh timer per round. A fixed cadence started at accept
    /// would instead force-pick the safe move a fraction of a second into any
    /// round that began late in the cycle, changing combat outcomes and who
    /// wins the wager.
    fn spawn_battle_timeout(&self, brawl_id: i64) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let turn = Duration::from_secs(
                u64::try_from(cama_domain::pet::PET_BRAWL_TURN_SECONDS).unwrap_or(30),
            );
            // Polling keeps the window anchored to when each round actually
            // started, which a single fixed-cadence sleep cannot observe.
            let tick = Duration::from_secs(1);
            let mut armed_for = Self::battle_round(&state, brawl_id);
            let mut round_started = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(tick).await;
                let current = Self::battle_round(&state, brawl_id);
                match battle_timeout_step(armed_for, current, round_started.elapsed(), turn) {
                    BattleTimeoutStep::Stop => return,
                    BattleTimeoutStep::Wait => continue,
                    BattleTimeoutStep::Rearm => {
                        // The players resolved this round themselves; the next
                        // one gets its own full window.
                        armed_for = current;
                        round_started = tokio::time::Instant::now();
                    }
                    BattleTimeoutStep::Fire => {
                        match Self::advance_battle_timeout_once(Arc::clone(&state), brawl_id).await
                        {
                            Ok(true) | Err(_) => return,
                            Ok(false) => {
                                armed_for = Self::battle_round(&state, brawl_id);
                                round_started = tokio::time::Instant::now();
                            }
                        }
                    }
                }
            }
        });
    }

    /// The round a live battle is currently on, or `None` once it is gone.
    fn battle_round(state: &Arc<PetRuntimeState>, brawl_id: i64) -> Option<u8> {
        state
            .battle_views
            .lock()
            .ok()?
            .get(&brawl_id)
            .map(|handle| handle.round_no)
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

    async fn render_tombstone(
        &self,
        name: &str,
        seed: i64,
    ) -> Result<InteractionAttachment, String> {
        let name = name.to_owned();
        let assets = Arc::clone(&self.state.assets);
        tokio::task::spawn_blocking(move || {
            let mut assets = assets
                .lock()
                .map_err(|_| "pet asset lock poisoned".to_owned())?;
            let file = assets.get_tombstone_card(&name, seed);
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
    // Pet challenge updates mirror discord.py's `Message.edit` calls: the
    // challenge's versus image remains attached unless a later result
    // explicitly supplies replacement art. The transport still defaults to
    // explicit clearing for every other response family.
    let mut response = InteractionResponse::message(message.content.clone().unwrap_or_default())
        .preserve_attachments();
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
        Self {
            service: production_pet_flavor_service(database_path, ai_service, ai_default),
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

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "pet_provider/tests.rs"]
mod tests;

/// What the battle timeout task should do on one tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BattleTimeoutStep {
    /// The battle is gone; the task is finished.
    Stop,
    /// A new round started; restart its window.
    Rearm,
    /// The current round still has time left.
    Wait,
    /// The current round's window has expired.
    Fire,
}

/// Decide one tick of the battle timeout.
///
/// The window belongs to a round, not to the battle: a round that starts late
/// still gets the full `turn` the embed promises, and a round is never expired
/// on the strength of time that elapsed during an earlier one.
const fn battle_timeout_step(
    armed_for: Option<u8>,
    current: Option<u8>,
    round_elapsed: Duration,
    turn: Duration,
) -> BattleTimeoutStep {
    if current.is_none() {
        return BattleTimeoutStep::Stop;
    }
    // Option<u8> has no const PartialEq, so compare the discriminants directly.
    match (armed_for, current) {
        (Some(armed), Some(now)) if armed != now => BattleTimeoutStep::Rearm,
        (None, Some(_)) => BattleTimeoutStep::Rearm,
        _ => {
            if round_elapsed.as_secs() < turn.as_secs() {
                BattleTimeoutStep::Wait
            } else {
                BattleTimeoutStep::Fire
            }
        }
    }
}
