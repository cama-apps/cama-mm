//! Production Discord provider for Python's complete `/shop` extension.
//!
//! All validation that must remain private happens before a public defer. Money
//! movement is delegated to existing-schema SQLite transactions, and durable
//! purchases use the interaction ID as their idempotency key. The only
//! process-local state is Python-equivalent rate limiting and Neon cooldowns.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::{
    AIService, FlavorEvent, FlavorTextService, GuildAiConfigPort, PlayerContext, PlayerContextPort,
    Value as AiValue,
};
use cama_app::dig_loot::artifact_catalog;
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::mana_service::Land;
use cama_app::manashop_rework::BuffService;
use cama_app::neon_degen::{NeonDegenService, NeonResult};
use cama_app::shop_commands::{
    ManaItemSpec, ManaTier, PairingRecord, calculate_soft_avoid_cost, mana_item,
    soft_avoid_choice_name,
};
use cama_db::gambling_stats_repository::{
    GamblingStatsPort, GamblingStatsRepository, GamblingStatsService,
};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::loan_repository::LoanRepository;
use cama_db::mana_service_repository::ManaRepository;
use cama_db::manashop_rework_repository::{
    ManashopRepository, PurchaseFailureReason as ManaPurchaseFailure, PurchaseRequest,
};
use cama_db::package_deal_repository::{
    PACKAGE_DEAL_CANCELLATION_INACTIVITY_DAYS, PackageDealCancellationFailureReason,
    PackageDealPurchaseFailureReason, PackageDealRepository,
};
use cama_db::pairings_repository::PairingsRepository;
use cama_db::rating_history_repository::{
    RatingHistoryRepository, RecalibrationEligibility, RecalibrationExecution,
    RecalibrationService, RecalibrationServiceConfig,
};
use cama_db::shop_runtime::{
    DoubleOrNothingRequest, HostileDestination, HostileLossRequest, PaidPingKind,
    PurchaseFailureReason, ShopPlayer, ShopRuntimeRepository,
};
use cama_db::soft_avoid_repository::{SoftAvoidPurchaseFailureReason, SoftAvoidRepository};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::guild_config::GuildConfigStore;
use chrono::{DateTime, Utc};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;
use tracing::{debug, error, warn};

use crate::ApplicationConfig;
use crate::discord_transport::{DiscordIdPlayerNameResolver, GuildPlayerNameResolver};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionAttachment,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

const SHOP_RATE_LIMIT: usize = 3;
const SHOP_RATE_PERIOD: Duration = Duration::from_secs(60);
const PACKAGE_DEAL_CANCELLATION_RATE_LIMIT: usize = 1;
const PACKAGE_DEAL_CANCELLATION_RATE_PERIOD: Duration = Duration::from_secs(10);
const MANA_RATE_LIMIT: usize = 1;
const MANA_RATE_PERIOD: Duration = Duration::from_secs(10);
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const RECALIBRATION_MIN_GAMES: i64 = 5;
const SOFT_AVOID_GAMES: i64 = 10;
const PACKAGE_DEAL_MAX_GAMES: i64 = 10;
const PACKAGE_DEAL_NO_ACTIVE_COST: i64 = 1;
const SOUL_HARVEST_DRAIN_PER_TARGET: i64 = 2;
const SOUL_HARVEST_BONUS_DRAIN_CHANCE: f64 = 0.20;
const BOUNTY_HUNTER_ANNOUNCE_COLOR: u32 = 0xDD_C9_3F;
const JOPA_COIN_COLOR: u32 = 0xD4_AF_37;
const BOUNTY_HUNTER_IMAGE_URL: &str =
    "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/bounty_hunter.png";
const PINGEDASH_TENOR_URL: &str = "https://tenor.com/view/hiash-gif-25282310";
const PINGEDKEVIN_TENOR_URL: &str =
    "https://tenor.com/view/in-trouble-mad-face-gif-10963449859613356314";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";

const ANNOUNCE_MESSAGES: &[&str] = &[
    "BEHOLD! A being of IMMENSE wealth walks among you!",
    "Everyone stop what you're doing. This is important.",
    "Let the record show that on this day, wealth was flexed.",
    "I didn't want to brag, but actually yes I did.",
    "This announcement brought to you by poor financial decisions.",
    "Witness me.",
    "They said money can't buy happiness. They lied.",
    "Is this obnoxious? Yes. Do I care? I paid for this.",
    "POV: You're about to feel poor.",
    "I could have saved this. But I'm built different.",
    "Track THIS, Bounty Hunter.",
    "The jingling of coins is my love language.",
];

const ANNOUNCE_TARGET_MESSAGES: &[&str] = &[
    "{user} paid {cost} {emote} specifically to flex on {target}. Worth it.",
    "Attention {target}: {user} has money and you need to know about it.",
    "{target}, you've been summoned to witness {user}'s financial superiority.",
    "{user} wanted {target} to see this. Petty? Absolutely. Expensive? Very.",
    "HEY {target}! {user} spent {cost} {emote} just to get your attention. Feel special.",
    "{user} could have bought {ratio} announcements. Instead, they bought one that bothers {target}.",
    "{target}: You're witnessing a {cost} {emote} flex from {user}. Congratulations?",
    "A moment of silence for {target}, who must now acknowledge {user}'s wealth.",
];

const DOUBLE_OR_NOTHING_WIN_MESSAGES: &[&str] = &[
    "The coin gods smile upon you today. Enjoy it while it lasts.",
    "DOUBLED! The house weeps. (Don't worry, they'll get it back.)",
    "Fortune favors the bold. And apparently, the financially reckless.",
    "You absolute madman. It actually worked.",
    "Against all odds... just kidding, it was literally 50/50.",
    "Lady Luck is on your side! Quick, go lose it on something else.",
    "The spirits of the coin have blessed you. They're known for taking it back later.",
    "Congratulations on your temporary victory over probability.",
];

const DOUBLE_OR_NOTHING_LOSE_MESSAGES: &[&str] = &[
    "And it's gone. All of it. Every last jopacoin.",
    "The coin has spoken. You have nothing. Absolutely nothing.",
    "Fortune is a fickle mistress, and she just took EVERYTHING.",
    "Poof. Should have walked away. But you're not that person, are you?",
    "Your wealth has been redistributed to the void. The void thanks you.",
    "Gone. Reduced to atoms. Your jopacoin are in a better place now.",
    "The house always... wait, you did this to yourself.",
    "From hero to zero in one coin flip. Speedrun any%.",
    "Remember when you had money? That was nice.",
];

#[async_trait]
pub trait ShopDiscordPort: Send + Sync {
    async fn shop_user_avatar_url(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, String>;

    /// Send a process-local Neon side message and delete it after `delete_after`.
    async fn shop_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        delete_after: Duration,
    ) -> Result<(), String>;
}

#[derive(Debug, Error)]
pub enum ShopProviderBuildError {
    #[error("shop provider database is not migrated: {0}")]
    Database(#[from] cama_db::shop_runtime::ShopRuntimeRepositoryError),
}

#[derive(Clone)]
pub struct ShopRegistrationProvider {
    handler: Arc<ShopInteractionHandler>,
    player_names: Arc<SharedGuildPlayerNameResolver>,
}

impl ShopRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn ShopDiscordPort>,
        ai_service: Option<Arc<AIService>>,
    ) -> Result<Self, ShopProviderBuildError> {
        let path = database_path.as_ref().to_path_buf();
        let repository = ShopRuntimeRepository::new(&path);
        repository.verify_schema()?;
        let economy_config = shop_economy_config(config);
        let runtime_config = ShopRuntimeConfig::from_application_config(config);
        let player_names = Arc::new(SharedGuildPlayerNameResolver::default());
        let player_name_resolver: Arc<dyn GuildPlayerNameResolver> = player_names.clone();
        let recalibration = RecalibrationService::new(
            RatingHistoryRepository::new(&path),
            RecalibrationServiceConfig {
                cooldown_seconds: runtime_config.recalibration_cooldown,
                initial_rd: runtime_config.recalibration_initial_rd,
                initial_volatility: runtime_config.recalibration_initial_volatility,
                min_games: RECALIBRATION_MIN_GAMES,
            },
        );
        let flavor = ai_service.map(|ai| {
            Arc::new(
                FlavorTextService::new(
                    ai,
                    Arc::new(ProductionShopPlayerContext::new(
                        &path,
                        Arc::clone(&player_name_resolver),
                    )),
                )
                .with_config(Arc::new(ProductionShopGuildAiConfig {
                    repository: GuildConfigRepository::new(
                        &path,
                        config.values.ai_features_enabled,
                    ),
                })),
            )
        });
        let mut neon = NeonDegenService::new(shop_seed());
        neon.set_enabled(config.values.neon_degen_enabled);
        neon.set_cooldown_seconds(
            u64::try_from(config.values.neon_cooldown_seconds.max(0)).unwrap_or(u64::MAX),
        );
        Ok(Self {
            handler: Arc::new(ShopInteractionHandler {
                repository,
                pairings: PairingsRepository::new(&path),
                soft_avoids: SoftAvoidRepository::new(&path),
                package_deals: PackageDealRepository::new(&path),
                mana: ManaRepository::new(&path),
                manashop: ManashopRepository::new(&path),
                recalibration,
                gambling: GamblingStatsRepository::new(&path),
                economy_events: Arc::new(SqliteEconomyEventService::new(&path, economy_config)),
                discord,
                player_names: player_name_resolver,
                flavor,
                config: runtime_config,
                clock: Arc::new(SystemShopClock),
                entropy: Mutex::new(FastrandShopEntropy::new(shop_seed())),
                buy_rates: Mutex::new(BTreeMap::new()),
                package_deal_cancellation_rates: Mutex::new(BTreeMap::new()),
                mana_rates: Mutex::new(BTreeMap::new()),
                neon: Mutex::new(neon),
            }),
            player_names,
        })
    }

    #[must_use]
    pub fn with_player_names(self, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        self.player_names.replace(player_names);
        self
    }
}

struct SharedGuildPlayerNameResolver {
    inner: RwLock<Arc<dyn GuildPlayerNameResolver>>,
}

impl Default for SharedGuildPlayerNameResolver {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Arc::new(DiscordIdPlayerNameResolver)),
        }
    }
}

impl SharedGuildPlayerNameResolver {
    fn replace(&self, player_names: Arc<dyn GuildPlayerNameResolver>) {
        *self.inner.write().expect("shop player-name resolver lock") = player_names;
    }
}

impl GuildPlayerNameResolver for SharedGuildPlayerNameResolver {
    fn names_for_guild(
        &self,
        guild_id: i64,
        player_ids: &[i64],
    ) -> Result<crate::discord_transport::GuildPlayerNameDirectory, String> {
        self.inner
            .read()
            .map_err(|_| "shop player-name resolver lock is poisoned".to_owned())?
            .names_for_guild(guild_id, player_ids)
    }
}

impl RegistrationProvider for ShopRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "shop".to_owned(),
            description: "Jopacoin and mana shop commands".to_owned(),
            options: shop_subcommands(&self.handler.config),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone, Debug)]
struct ShopRuntimeConfig {
    announce_cost: i64,
    announce_target_cost: i64,
    jopa_coin_cost: i64,
    mystery_gift_cost: i64,
    double_or_nothing_cost: i64,
    double_or_nothing_cooldown: i64,
    recalibrate_cost: i64,
    recalibration_cooldown: i64,
    recalibration_initial_rd: f64,
    recalibration_initial_volatility: f64,
    soft_avoid_cost: i64,
    package_deal_base_cost: i64,
    package_deal_rating_divisor: f64,
    package_deal_games: i64,
    pingedash_cost: i64,
    pingedash_cooldown: i64,
    pingedash_target: Option<i64>,
    pingedkevin_cost: i64,
    pingedkevin_cooldown: i64,
    pingedkevin_target: Option<i64>,
    hostile_min_balance: i64,
    minigame_scale: f64,
    admin_user_ids: BTreeSet<i64>,
}

impl ShopRuntimeConfig {
    fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            announce_cost: values.shop_announce_cost,
            announce_target_cost: values.shop_announce_target_cost,
            jopa_coin_cost: values.shop_jopa_coin_cost,
            mystery_gift_cost: values.shop_new_mystery_gift_cost,
            double_or_nothing_cost: values.shop_double_or_nothing_cost,
            double_or_nothing_cooldown: values.double_or_nothing_cooldown_seconds,
            recalibrate_cost: values.shop_recalibrate_cost,
            recalibration_cooldown: values.recalibration_cooldown_seconds,
            recalibration_initial_rd: values.recalibration_initial_rd,
            recalibration_initial_volatility: values.recalibration_initial_volatility,
            soft_avoid_cost: values.shop_soft_avoid_cost,
            package_deal_base_cost: values.shop_package_deal_base_cost,
            package_deal_rating_divisor: values.shop_package_deal_rating_divisor,
            package_deal_games: values.package_deal_games_duration,
            pingedash_cost: values.pingedash_cost,
            pingedash_cooldown: values.pingedash_cooldown_seconds,
            pingedash_target: values.pingedash_target_user_id,
            pingedkevin_cost: values.pingedkevin_cost,
            pingedkevin_cooldown: values.pingedkevin_cooldown_seconds,
            pingedkevin_target: values.pingedkevin_target_user_id,
            hostile_min_balance: values.hostile_loss_min_balance,
            minigame_scale: values.minigame_jc_delta_scale,
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
        }
    }
}

struct ShopInteractionHandler {
    repository: ShopRuntimeRepository,
    pairings: PairingsRepository,
    soft_avoids: SoftAvoidRepository,
    package_deals: PackageDealRepository,
    mana: ManaRepository,
    manashop: ManashopRepository,
    recalibration: RecalibrationService,
    gambling: GamblingStatsRepository,
    economy_events: Arc<SqliteEconomyEventService>,
    discord: Arc<dyn ShopDiscordPort>,
    player_names: Arc<dyn GuildPlayerNameResolver>,
    flavor: Option<Arc<FlavorTextService>>,
    config: ShopRuntimeConfig,
    clock: Arc<dyn ShopClock>,
    entropy: Mutex<FastrandShopEntropy>,
    buy_rates: Mutex<BTreeMap<(i64, i64), VecDeque<Instant>>>,
    package_deal_cancellation_rates: Mutex<BTreeMap<(i64, i64), VecDeque<Instant>>>,
    mana_rates: Mutex<BTreeMap<(i64, i64), VecDeque<Instant>>>,
    neon: Mutex<NeonDegenService>,
}

trait ShopClock: Send + Sync {
    fn now(&self) -> Result<i64, String>;
}

struct SystemShopClock;

impl ShopClock for SystemShopClock {
    fn now(&self) -> Result<i64, String> {
        unix_timestamp()
    }
}

struct FastrandShopEntropy {
    rng: fastrand::Rng,
}

impl FastrandShopEntropy {
    fn new(seed: u64) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
        }
    }

    fn chance(&mut self, chance: f64) -> bool {
        self.rng.f64() < chance.clamp(0.0, 1.0)
    }

    fn i64_inclusive(&mut self, low: i64, high: i64) -> i64 {
        self.rng.i64(low..=high)
    }

    fn index(&mut self, len: usize) -> usize {
        self.rng.usize(0..len)
    }

    fn sample_indices(&mut self, len: usize, count: usize) -> Vec<usize> {
        let mut indices = (0..len).collect::<Vec<_>>();
        self.rng.shuffle(&mut indices);
        indices.truncate(count.min(len));
        indices
    }
}

#[derive(Clone, Debug)]
struct CommandContext {
    interaction_id: u64,
    user_id: i64,
    user_display_name: String,
    guild_id: i64,
    channel_id: Option<i64>,
    member_permissions: Option<u64>,
    subcommand: String,
    options: Vec<InteractionOption>,
    rendered_user_names: BTreeMap<i64, String>,
}

#[derive(Clone, Debug)]
struct UserOption {
    id: i64,
    unsigned_id: u64,
    display_name: String,
    ai_display_name: String,
}

#[derive(Clone, Debug)]
struct FlexStats {
    balance: i64,
    wins: i64,
    losses: i64,
    win_rate: Option<f64>,
    rating: Option<f64>,
    total_bets: i64,
    net_pnl: i64,
    degen_score: Option<i64>,
    bankruptcies: i64,
}

impl FlexStats {
    fn ai_value(&self) -> AiValue {
        AiValue::Object(BTreeMap::from([
            ("balance".to_owned(), AiValue::Integer(self.balance)),
            ("wins".to_owned(), AiValue::Integer(self.wins)),
            ("losses".to_owned(), AiValue::Integer(self.losses)),
            (
                "win_rate".to_owned(),
                self.win_rate.map_or(AiValue::Null, AiValue::Real),
            ),
            (
                "rating".to_owned(),
                self.rating.map_or(AiValue::Null, AiValue::Real),
            ),
            ("total_bets".to_owned(), AiValue::Integer(self.total_bets)),
            ("net_pnl".to_owned(), AiValue::Integer(self.net_pnl)),
            (
                "degen_score".to_owned(),
                self.degen_score.map_or(AiValue::Null, AiValue::Integer),
            ),
            (
                "bankruptcies".to_owned(),
                AiValue::Integer(self.bankruptcies),
            ),
        ]))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FlexComparison {
    buyer_wins: Vec<String>,
    target_wins: Vec<String>,
}

impl FlexComparison {
    fn ai_value(&self) -> AiValue {
        AiValue::Object(BTreeMap::from([
            (
                "buyer_wins".to_owned(),
                AiValue::List(self.buyer_wins.iter().cloned().map(AiValue::Text).collect()),
            ),
            (
                "target_wins".to_owned(),
                AiValue::List(
                    self.target_wins
                        .iter()
                        .cloned()
                        .map(AiValue::Text)
                        .collect(),
                ),
            ),
        ]))
    }
}

#[async_trait]
impl InteractionHandler for ShopInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                interaction_id,
                name,
                user_id,
                user_display_name,
                guild_id,
                channel_id,
                member_permissions,
                options,
            } => {
                if name != "shop" {
                    return Err(format!("shop handler received command {name:?}").into());
                }
                let Some(guild_id) = guild_id else {
                    return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
                };
                let (subcommand, options) = subcommand(&options)?;
                self.command(
                    CommandContext {
                        interaction_id,
                        user_id: signed_id(user_id, "user")?,
                        user_display_name,
                        guild_id: signed_id(guild_id, "guild")?,
                        channel_id: channel_id.map(|id| signed_id(id, "channel")).transpose()?,
                        member_permissions,
                        subcommand,
                        options,
                        rendered_user_names: BTreeMap::new(),
                    },
                    responder,
                )
                .await
            }
            InteractionRequest::Autocomplete {
                name,
                guild_id,
                focused_option,
                focused_value,
                options,
                ..
            } => {
                if name != "shop" || focused_option != "item" {
                    return Err("shop autocomplete received an unsupported option".into());
                }
                if guild_id.is_none() {
                    return responder
                        .autocomplete(Vec::new())
                        .await
                        .map_err(|error| error.to_string().into());
                }
                let (subcommand, _) = subcommand(&options)?;
                if subcommand != "buy" {
                    return Err("shop item autocomplete is only valid for /shop buy".into());
                }
                self.autocomplete(&focused_value, responder).await
            }
            InteractionRequest::Component { .. } | InteractionRequest::Modal { .. } => {
                Err("shop extension received an unsupported interaction".into())
            }
        }
    }
}

impl ShopInteractionHandler {
    async fn command(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match context.subcommand.as_str() {
            "buy" => self.buy(context, responder).await,
            "pingedash" => self.paid_ping(context, responder, PaidPingKind::Dash).await,
            "pingedkevin" => {
                self.paid_ping(context, responder, PaidPingKind::Kevin)
                    .await
            }
            "avoids" => self.avoids(context, responder).await,
            "deals" => self.deals(context, responder).await,
            "cancel-deal" => {
                let target = user_option(&context, "target")?
                    .ok_or("missing required user option \"target\"")?;
                self.cancel_package_deal(context, responder, target).await
            }
            "mana" => self.mana_shop(context, responder).await,
            value => Err(format!("unsupported /shop subcommand {value:?}").into()),
        }
    }

    async fn with_rendered_player_names(
        &self,
        mut context: CommandContext,
    ) -> Result<CommandContext, InteractionHandlerError> {
        let targets = context
            .options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| match option.value {
                InteractionValue::User { id, .. } => Some((index, id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut stored_names = BTreeMap::new();
        // The invoking user's interaction payload name is the stored fallback
        // when the member cache misses.
        stored_names.insert(context.user_id, context.user_display_name.clone());
        for (index, id) in &targets {
            let target_id = signed_id(*id, "target user")?;
            let payload_name = match &context.options[*index].value {
                InteractionValue::User { display_name, .. } => display_name.clone(),
                _ => None,
            };
            let stored_name = self
                .load_player(target_id, context.guild_id)
                .await?
                .map(|player| player.name)
                .or(payload_name)
                .unwrap_or_default();
            stored_names.insert(target_id, stored_name);
        }
        let rendered_names = self.project_stored_player_names(context.guild_id, stored_names);
        context.user_display_name = rendered_names
            .get(&context.user_id)
            .cloned()
            .unwrap_or_else(|| context.user_id.to_string());
        for (_index, id) in targets {
            let target_id = signed_id(id, "target user")?;
            context.rendered_user_names.insert(
                target_id,
                rendered_names
                    .get(&target_id)
                    .cloned()
                    .unwrap_or_else(|| target_id.to_string()),
            );
        }
        Ok(context)
    }

    fn project_stored_player_names(
        &self,
        guild_id: i64,
        stored_names: BTreeMap<i64, String>,
    ) -> BTreeMap<i64, String> {
        let player_ids = stored_names.keys().copied().collect::<Vec<_>>();
        let names = self
            .player_names
            .names_for_guild(guild_id, &player_ids)
            .unwrap_or_default();
        stored_names
            .into_iter()
            .map(|(player_id, stored_name)| (player_id, names.resolve_or(player_id, &stored_name)))
            .collect()
    }

    async fn autocomplete(
        &self,
        current: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let current = current.to_ascii_lowercase();
        let choices = self
            // Autocomplete must answer inside Discord's short callback window.
            // Keep this path storage-free; the purchase handler performs the
            // authoritative recalibration cooldown check after selection.
            .item_choices(false)
            .into_iter()
            .filter(|choice| {
                current.is_empty() || choice.name.to_ascii_lowercase().contains(&current)
            })
            .take(25)
            .map(|choice| CommandOptionChoice::String {
                name: choice.name,
                value: choice.value,
            })
            .collect();
        responder
            .autocomplete(choices)
            .await
            .map_err(|error| error.to_string().into())
    }

    fn item_choices(&self, recalibration_on_cooldown: bool) -> Vec<ShopChoice> {
        let package_games = self
            .config
            .package_deal_games
            .clamp(1, PACKAGE_DEAL_MAX_GAMES);
        let mut choices = vec![
            ShopChoice::new(
                format!("Announce Balance ({} jopacoin)", self.config.announce_cost),
                "announce",
            ),
            ShopChoice::new(
                format!(
                    "Announce Balance + Tag User ({} jopacoin)",
                    self.config.announce_target_cost
                ),
                "announce_target",
            ),
            ShopChoice::new(
                format!("Jopa Coin(TM) ({} jopacoin)", self.config.jopa_coin_cost),
                "jopa_coin",
            ),
            ShopChoice::new(
                format!("Mystery Gift ({} jopacoin)", self.config.mystery_gift_cost),
                "mystery_gift",
            ),
            ShopChoice::new(
                format!(
                    "Double or Nothing ({} jopacoin)",
                    self.config.double_or_nothing_cost
                ),
                "double_or_nothing",
            ),
            ShopChoice::new(
                soft_avoid_choice_name(self.config.soft_avoid_cost),
                "soft_avoid",
            ),
            ShopChoice::new(
                format!(
                    "Package Deal ({PACKAGE_DEAL_NO_ACTIVE_COST} JC at 0 active, {}+ after for {package_games} games)",
                    self.config.package_deal_base_cost
                ),
                "package_deal",
            ),
        ];
        choices.push(if recalibration_on_cooldown {
            ShopChoice::new("Recalibrate (ON COOLDOWN)", "recalibrate_cooldown")
        } else {
            ShopChoice::new(
                format!("Recalibrate ({} jopacoin)", self.config.recalibrate_cost),
                "recalibrate",
            )
        });
        choices
    }

    async fn buy(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = check_rate_limit(
            &self.buy_rates,
            (context.guild_id, context.user_id),
            SHOP_RATE_LIMIT,
            SHOP_RATE_PERIOD,
        )? {
            return respond_ephemeral(
                &responder,
                format!("Slow down there, big spender. Wait {retry}s before your next purchase."),
            )
            .await;
        }
        let item = required_string(&context.options, "item")?;
        let context = self.with_rendered_player_names(context).await?;
        let target = user_option(&context, "target")?;
        match item.as_str() {
            "announce" => {
                self.simple_purchase(context, responder, None, SimpleProduct::Announce)
                    .await
            }
            "announce_target" => {
                let Some(target) = target else {
                    return respond_ephemeral(
                        &responder,
                        "You selected 'Announce + Tag User' but didn't specify a target. Please provide a user to tag!",
                    )
                    .await;
                };
                self.simple_purchase(
                    context,
                    responder,
                    Some(target),
                    SimpleProduct::AnnounceTarget,
                )
                .await
            }
            "jopa_coin" => {
                self.simple_purchase(context, responder, None, SimpleProduct::JopaCoin)
                    .await
            }
            "mystery_gift" => {
                self.simple_purchase(context, responder, None, SimpleProduct::MysteryGift)
                    .await
            }
            "double_or_nothing" => self.double_or_nothing(context, responder).await,
            "soft_avoid" => {
                let Some(target) = target else {
                    return respond_ephemeral(
                        &responder,
                        "You selected 'Soft Avoid' but didn't specify a target. Please provide a user to avoid being teamed with!",
                    )
                    .await;
                };
                self.soft_avoid(context, responder, target).await
            }
            "package_deal" => {
                let Some(target) = target else {
                    return respond_ephemeral(
                        &responder,
                        "You selected 'Package Deal' but didn't specify a target. Please provide a user you want to be teamed with!",
                    )
                    .await;
                };
                self.package_deal(context, responder, target).await
            }
            "recalibrate" => self.recalibrate(context, responder).await,
            "recalibrate_cooldown" => self.recalibration_cooldown(context, responder).await,
            _ => respond_ephemeral(&responder, "That shop item is no longer available.").await,
        }
    }

    async fn paid_ping(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
        kind: PaidPingKind,
    ) -> Result<(), InteractionHandlerError> {
        let (display_name, target, tenor_url, cost, cooldown) = match kind {
            PaidPingKind::Dash => (
                "Pingedash",
                self.config.pingedash_target,
                PINGEDASH_TENOR_URL,
                self.config.pingedash_cost,
                self.config.pingedash_cooldown,
            ),
            PaidPingKind::Kevin => (
                "PingedKevin",
                self.config.pingedkevin_target,
                PINGEDKEVIN_TENOR_URL,
                self.config.pingedkevin_cost,
                self.config.pingedkevin_cooldown,
            ),
        };
        let Some(target) = target.filter(|target| *target > 0) else {
            return respond_ephemeral(
                &responder,
                format!("{display_name} is unavailable because its target user is not configured."),
            )
            .await;
        };
        let repository = self.repository.clone();
        let now = self.clock.now()?;
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let result = run_blocking("paid ping", move || {
            repository.purchase_paid_ping(kind, user_id, guild_id, cost, now, cooldown)
        })
        .await?;
        if !result.success {
            let command_name = kind.source();
            let message = match result.reason {
                Some(PurchaseFailureReason::NotRegistered) => {
                    format!("You need to `/player register` before using `/shop {command_name}`.")
                }
                Some(PurchaseFailureReason::OnCooldown) => format!(
                    "{display_name} is on cooldown. You can use it again <t:{}:R>.",
                    result.cooldown_ends_at.unwrap_or_default()
                ),
                Some(PurchaseFailureReason::InsufficientBalance) => format!(
                    "You need {cost} {JOPACOIN_EMOTE} for {display_name}, but you only have {}.",
                    result.balance
                ),
                _ => format!("{display_name} is unavailable right now."),
            };
            if let Err(error) = respond_ephemeral(&responder, message).await {
                warn!(user_id, %display_name, %error, "paid ping failure notice could not be sent");
            }
            return Ok(());
        }
        let _ = responder.defer(false).await;
        let unsigned_target =
            u64::try_from(target).map_err(|_| "configured target id is invalid")?;
        responder
            .followup(
                InteractionResponse::message(format!("<@{target}>\n{tenor_url}"))
                    .with_user_mentions(vec![unsigned_target]),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn avoids(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.soft_avoids.clone();
        let guild_id = context.guild_id;
        let user_id = context.user_id;
        let avoids = run_blocking("soft avoid list", move || {
            repository.get_user_avoids(Some(guild_id), user_id)
        })
        .await?;
        if avoids.is_empty() {
            return respond_ephemeral(&responder, "You have no active soft avoids.").await;
        }
        let description = avoids
            .iter()
            .map(|avoid| {
                format!(
                    "<@{}> - **{}** games",
                    avoid.avoided_discord_id, avoid.games_remaining
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        responder
            .respond(
                InteractionResponse::message("")
                    .ephemeral()
                    .without_mentions()
                    .embed(
                        InteractionEmbed::titled("Your Active Soft Avoids")
                            .description(description)
                            .color(0x72_89_DA),
                    ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn deals(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.package_deals.clone();
        let guild_id = context.guild_id;
        let user_id = context.user_id;
        let deals = run_blocking("package deal list", move || {
            repository.get_user_deals(Some(guild_id), user_id)
        })
        .await?;
        if deals.is_empty() {
            return respond_ephemeral(&responder, "You have no active package deals.").await;
        }
        let description = deals
            .iter()
            .map(|deal| {
                format!(
                    "<@{}> - **{}** games",
                    deal.partner_discord_id, deal.games_remaining
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        responder
            .respond(
                InteractionResponse::message("")
                    .ephemeral()
                    .without_mentions()
                    .embed(
                        InteractionEmbed::titled("Your Active Package Deals")
                            .description(description)
                            .color(0x2E_CC_71),
                    ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn cancel_package_deal(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
        target: UserOption,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = check_rate_limit(
            &self.package_deal_cancellation_rates,
            (context.guild_id, context.user_id),
            PACKAGE_DEAL_CANCELLATION_RATE_LIMIT,
            PACKAGE_DEAL_CANCELLATION_RATE_PERIOD,
        )? {
            return respond_ephemeral(
                &responder,
                format!("This command is on cooldown. Try again in {retry}s."),
            )
            .await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.package_deals.clone();
        let guild_id = context.guild_id;
        let buyer_id = context.user_id;
        let partner_id = target.id;
        let now = self.clock.now()?;
        let result = match run_blocking("package deal cancellation", move || {
            repository.cancel_deal(Some(guild_id), buyer_id, partner_id, now)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                warn!(%error, "package deal cancellation failed");
                return responder
                    .followup(
                        InteractionResponse::message(
                            "Package Deal cancellation could not be completed. Please try again.",
                        )
                        .ephemeral()
                        .without_mentions(),
                    )
                    .await
                    .map_err(|error| error.to_string().into());
            }
        };
        let message = match result.reason {
            None if result.cancelled => format!(
                "Cancelled your Package Deal with <@{partner_id}>. No jopacoin were refunded."
            ),
            Some(PackageDealCancellationFailureReason::Ineligible) => format!(
                "That Package Deal can only be cancelled after the target has not played for {PACKAGE_DEAL_CANCELLATION_INACTIVITY_DAYS} days and the deal has not been updated for {PACKAGE_DEAL_CANCELLATION_INACTIVITY_DAYS} days."
            ),
            Some(PackageDealCancellationFailureReason::NotFound) | None => {
                "You do not have an active Package Deal with that player.".to_owned()
            }
        };
        responder
            .followup(
                InteractionResponse::message(message)
                    .ephemeral()
                    .without_mentions(),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn mana_shop(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        self.execute_mana_purchase(context, responder).await
    }

    async fn execute_mana_purchase(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = check_rate_limit(
            &self.mana_rates,
            (context.guild_id, context.user_id),
            MANA_RATE_LIMIT,
            MANA_RATE_PERIOD,
        )? {
            return respond_ephemeral(
                &responder,
                format!("This command is on cooldown. Try again in {retry}s."),
            )
            .await;
        }
        let Some(player) = self.load_player(context.user_id, context.guild_id).await? else {
            return respond_ephemeral(&responder, "You need to `/player register` first.").await;
        };
        let item_key = required_string(&context.options, "item")?;
        let Some(spec) = mana_item(&item_key) else {
            return respond_ephemeral(&responder, "Unknown item.").await;
        };
        let context = self.with_rendered_player_names(context).await?;
        let target = user_option(&context, "target")?;
        let now = self.clock.now()?;
        let today = pacific_mana_day(now)?;
        let mana = self.mana.clone();
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let current = run_blocking("manashop mana lookup", move || {
            mana.get_mana(user_id, Some(guild_id))
        })
        .await?;
        if current.as_ref().is_some_and(|mana| mana.consumed_today) {
            return respond_ephemeral(
                &responder,
                "Your mana is spent — today's ultimate consumed your color. Come back tomorrow.",
            )
            .await;
        }
        let active_land = current
            .as_ref()
            .filter(|mana| mana.assigned_date == today)
            .and_then(|mana| Land::from_name(&mana.current_land));
        let Some(land) = active_land else {
            return respond_ephemeral(
                &responder,
                "You have no active mana today. Use `/mana` first.",
            )
            .await;
        };
        if land.color() != spec.color {
            return respond_ephemeral(
                &responder,
                format!(
                    "**{}** requires **{}** mana ({}). Your current mana is **{}**.",
                    spec.name,
                    spec.color,
                    land_for_color(spec.color),
                    land.color()
                ),
            )
            .await;
        }
        if spec.target_required && target.is_none() {
            return respond_ephemeral(
                &responder,
                format!("**{}** requires a `target` player.", spec.name),
            )
            .await;
        }
        if !spec.self_target_allowed
            && target
                .as_ref()
                .is_some_and(|target| target.id == context.user_id)
        {
            return respond_ephemeral(
                &responder,
                format!("**{}** must target another player.", spec.name),
            )
            .await;
        }
        if player.balance < spec.cost {
            return respond_ephemeral(
                &responder,
                format!(
                    "You need {} {JOPACOIN_EMOTE} for {}. You have {}.",
                    spec.cost, spec.name, player.balance
                ),
            )
            .await;
        }

        let purchase_id = format!("shop-mana:{}:{}", context.guild_id, context.interaction_id);
        let repository = self.manashop.clone();
        let request_id = purchase_id.clone();
        let used_date = today.clone();
        let purchase = run_blocking("manashop purchase", move || {
            repository.try_purchase_item_atomic(PurchaseRequest {
                purchase_id: &request_id,
                discord_id: user_id,
                guild_id: Some(guild_id),
                item_id: spec.key,
                used_date: &used_date,
                cost: spec.cost,
                tap_mana: spec.tier == ManaTier::Ultimate,
                now,
            })
        })
        .await?;
        if !purchase.success {
            let message = match purchase.reason {
                Some(ManaPurchaseFailure::AlreadyUsed) => {
                    format!("**{}** is once-per-day. Already used today.", spec.name)
                }
                Some(ManaPurchaseFailure::PurchaseInProgress) => format!(
                    "Your previous **{}** purchase is still starting. Try again shortly.",
                    spec.name
                ),
                Some(ManaPurchaseFailure::ManaTapped) => {
                    "Your mana was already tapped this turn. Try again tomorrow.".to_owned()
                }
                Some(ManaPurchaseFailure::NotRegistered) => {
                    "You need to `/player register` first.".to_owned()
                }
                Some(ManaPurchaseFailure::InsufficientBalance) | None => format!(
                    "You need {} {JOPACOIN_EMOTE} for {}. You have {}.",
                    spec.cost, spec.name, purchase.balance
                ),
            };
            if let Err(error) = respond_ephemeral(&responder, message).await {
                warn!(user_id, %error, "manashop failure notice could not be sent");
            }
            return Ok(());
        }
        let repository = self.manashop.clone();
        let applying_id = purchase_id.clone();
        let applying = run_blocking("manashop applying claim", move || {
            repository.mark_item_purchase_applying_atomic(&applying_id, now)
        })
        .await?;
        if !applying {
            self.refund_mana_purchase(&purchase_id, now).await;
            return respond_ephemeral(
                &responder,
                format!(
                    "Could not start **{}** safely; purchase refunded.",
                    spec.name
                ),
            )
            .await;
        }
        let _ = responder.defer(false).await;

        let ult_refund = if spec.tier == ManaTier::Ultimate {
            let repository = self.repository.clone();
            let has_conduit = run_blocking("mana conduit lookup", move || {
                repository.has_equipped_relic(user_id, guild_id, "mana_conduit")
            })
            .await
            .unwrap_or(false);
            if has_conduit {
                let refund = ((spec.cost as f64) * 0.25).floor() as i64;
                let refund = refund.max(1);
                let repository = self.repository.clone();
                let credited = run_blocking("mana conduit refund", move || {
                    repository.adjust_balance(
                        user_id, guild_id, refund, None, None, None, None, None, None,
                    )
                })
                .await;
                // Only report a refund the player actually received: the
                // reported balance is derived from this figure.
                if let Err(error) = &credited {
                    warn!(%error, user_id, guild_id, "mana conduit refund was not credited");
                }
                if credited.is_ok() { refund } else { 0 }
            } else {
                0
            }
        } else {
            0
        };

        let effect = self
            .apply_mana_effect(
                &context,
                target.as_ref(),
                spec,
                &purchase_id,
                &today,
                now,
                purchase.balance,
                ult_refund,
            )
            .await;
        let content = match effect {
            Ok(content) => content,
            Err(reason) => {
                self.refund_mana_purchase(&purchase_id, now).await;
                let allowed = target
                    .as_ref()
                    .map(|target| vec![target.unsigned_id])
                    .unwrap_or_default();
                responder
                    .followup(InteractionResponse::message(reason).with_user_mentions(allowed))
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
        };
        let mut allowed = vec![u64::try_from(context.user_id).map_err(|_| "invalid user id")?];
        if let Some(target) = &target {
            allowed.push(target.unsigned_id);
        }
        responder
            .followup(InteractionResponse::message(content).with_user_mentions(allowed))
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.manashop.clone();
        let completed_id = purchase_id.clone();
        let completed = run_blocking("manashop purchase completion", move || {
            repository.complete_item_purchase_atomic(&completed_id, now)
        })
        .await?;
        if !completed {
            return Err(format!("Manashop purchase {purchase_id} lost its applying state").into());
        }
        Ok(())
    }

    async fn refund_mana_purchase(&self, purchase_id: &str, now: i64) {
        let repository = self.manashop.clone();
        let purchase_id = purchase_id.to_owned();
        if let Err(error) = run_blocking("manashop refund", move || {
            repository.refund_item_purchase_atomic(&purchase_id, now)
        })
        .await
        {
            error!(%error, "failed to refund manashop purchase");
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_mana_effect(
        &self,
        context: &CommandContext,
        target: Option<&UserOption>,
        spec: ManaItemSpec,
        purchase_id: &str,
        today: &str,
        now: i64,
        balance_after_purchase: i64,
        ult_refund: i64,
    ) -> Result<String, String> {
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let new_balance = balance_after_purchase.saturating_add(ult_refund);
        match spec.key {
            "pyroclasm" => {
                let eligible = self.eligible_hostile_players(user_id, guild_id).await?;
                if eligible.is_empty() {
                    return Err("No eligible targets to scorch; refunded.".to_owned());
                }
                let indices = self
                    .entropy
                    .lock()
                    .map_err(|_| "shop entropy lock is poisoned".to_owned())?
                    .sample_indices(eligible.len(), 3);
                let selected = indices
                    .into_iter()
                    .map(|index| eligible[index].clone())
                    .collect::<Vec<_>>();
                let rolls = {
                    let mut entropy = self
                        .entropy
                        .lock()
                        .map_err(|_| "shop entropy lock is poisoned".to_owned())?;
                    (0..selected.len())
                        .map(|_| entropy.i64_inclusive(12, 28))
                        .collect::<Vec<_>>()
                };
                let requests = selected
                    .iter()
                    .zip(rolls)
                    .filter_map(|(victim, roll)| {
                        let amount = scale_deflationary_minigame_jc_delta(
                            roll as f64,
                            self.config.minigame_scale,
                        )
                        .min(victim.balance)
                        .max(0);
                        (amount > 0).then(|| {
                            hostile_request(
                                victim.discord_id,
                                guild_id,
                                amount,
                                "pyroclasm",
                                user_id,
                                format!("pyroclasm:{purchase_id}:{}", victim.discord_id),
                                HostileDestination::Burn,
                                None,
                                self.config.hostile_min_balance,
                                now,
                                today,
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                let victim_ids = requests
                    .iter()
                    .map(|request| request.victim_id)
                    .collect::<Vec<_>>();
                let outcomes = self.apply_hostile_batch(requests).await?;
                let stored_names = selected
                    .iter()
                    .map(|player| (player.discord_id, player.name.clone()))
                    .collect::<BTreeMap<_, _>>();
                let names = self.project_stored_player_names(guild_id, stored_names);
                let mut total_destroyed = 0;
                let mut total_absorbed = 0;
                let mut lines = Vec::new();
                for (victim_id, outcome) in victim_ids.into_iter().zip(outcomes) {
                    let Ok(outcome) = outcome else {
                        continue;
                    };
                    total_destroyed += outcome.applied;
                    total_absorbed += outcome.absorbed;
                    let shield = if outcome.absorbed > 0 {
                        format!(" (shield absorbed {})", outcome.absorbed)
                    } else {
                        String::new()
                    };
                    lines.push(format!(
                        "  - {}: -{} {JOPACOIN_EMOTE}{shield}",
                        names
                            .get(&victim_id)
                            .map(String::as_str)
                            .unwrap_or("Unknown"),
                        outcome.applied
                    ));
                }
                let bounty = 35.min(total_destroyed / 2);
                if bounty > 0 {
                    self.credit_mana_reward(
                        user_id,
                        guild_id,
                        bounty,
                        "manashop",
                        "hostile_loss_reward",
                        format!("pyroclasm:{purchase_id}"),
                        "manashop pyroclasm bounty",
                        json!({
                            "kind": "pyroclasm",
                            "applied_loss": total_destroyed,
                            "absorbed_amount": total_absorbed,
                        }),
                    )
                    .await?;
                }
                let victims = if lines.is_empty() {
                    "  No eligible targets.".to_owned()
                } else {
                    lines.join("\n")
                };
                let shield = if total_absorbed > 0 {
                    format!(" Shields absorbed **{total_absorbed} {JOPACOIN_EMOTE}**.")
                } else {
                    String::new()
                };
                Ok(format!(
                    "⛰️🔥 **PYROCLASM** — <@{user_id}> unleashes chaos!\n{victims}\n**{total_destroyed} {JOPACOIN_EMOTE} burned**.{shield} You claim **{bounty} {JOPACOIN_EMOTE}** from the ash.\n(cost: {} {JOPACOIN_EMOTE}, balance: {})",
                    spec.cost,
                    balance_after_purchase + bounty
                ))
            }
            "insight" => {
                let target =
                    target.ok_or_else(|| "Insight requires a target; refunded.".to_owned())?;
                if self
                    .load_player(target.id, guild_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    return Err(format!("<@{}> is not registered.", target.id));
                }
                let repository = self.repository.clone();
                let target_id = target.id;
                let info = run_blocking("Insight tunnel lookup", move || {
                    repository.dig_tunnel_info(target_id, guild_id)
                })
                .await
                .map_err(|error| error.to_string())?;
                let Some(info) = info else {
                    return Ok(format!(
                        "🏝️🔍 **INSIGHT** — <@{}> hasn't started digging.\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                        target.id, spec.cost
                    ));
                };
                let catalog = artifact_catalog()
                    .into_iter()
                    .map(|artifact| (artifact.id, artifact.name))
                    .collect::<BTreeMap<_, _>>();
                let relics = if info.relic_ids.is_empty() {
                    "—".to_owned()
                } else {
                    info.relic_ids
                        .iter()
                        .map(|id| catalog.get(id.as_str()).map_or(id.as_str(), |name| *name))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                Ok(format!(
                    "🏝️🔍 **INSIGHT** — <@{user_id}> reads the threads of fate.\n**{}** depth `{}` luminosity `{}` prestige `{}`\nEquipped relics: {relics}\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                    target.display_name,
                    info.depth,
                    info.luminosity,
                    info.prestige_level,
                    spec.cost
                ))
            }
            "sapling" => {
                let repository = self.repository.clone();
                let shaved = run_blocking("Sapling cooldown shave", move || {
                    repository.shave_dig_cooldown(user_id, guild_id, 45 * 60)
                })
                .await
                .unwrap_or(0);
                Ok(format!(
                    "🌲🌱 **SAPLING** — <@{user_id}> accelerates growth.\n`/dig` cooldown reduced by {}m.\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                    shaved / 60,
                    spec.cost
                ))
            }
            "reprieve" => {
                let repository = self.manashop.clone();
                let buff_id = run_blocking("Reprieve grant", move || {
                    BuffService::new(repository, now).grant_reprieve(user_id, guild_id)
                })
                .await
                .map_err(|_| "Could not raise the reprieve; refunded.".to_owned())?;
                let repository = self.repository.clone();
                let recovered = run_blocking("Reprieve reconciliation", move || {
                    repository.reconcile_purchased_pool(
                        user_id,
                        guild_id,
                        buff_id,
                        now.saturating_sub(24 * 3_600),
                        now,
                    )
                })
                .await
                .unwrap_or(0);
                Ok(format!(
                    "🌾🕊️ **REPRIEVE** — <@{user_id}> raises a rolling ward.\nRecovered **{recovered} {JOPACOIN_EMOTE}** from eligible losses in the past 24h. Remaining capacity protects 50% of hostile losses for 24h, up to **25 {JOPACOIN_EMOTE}** total.\n(cost: {} {JOPACOIN_EMOTE}, balance: {})",
                    spec.cost,
                    new_balance + recovered
                ))
            }
            "soul_harvest" => {
                let eligible = self.eligible_hostile_players(user_id, guild_id).await?;
                if eligible.is_empty() {
                    return Err("No living souls to drain; refunded.".to_owned());
                }
                let base = scale_minigame_jc_delta(
                    SOUL_HARVEST_DRAIN_PER_TARGET as f64,
                    self.config.minigame_scale,
                );
                let bonuses = {
                    let mut entropy = self
                        .entropy
                        .lock()
                        .map_err(|_| "shop entropy lock is poisoned".to_owned())?;
                    (0..eligible.len())
                        .map(|_| i64::from(entropy.chance(SOUL_HARVEST_BONUS_DRAIN_CHANCE)))
                        .collect::<Vec<_>>()
                };
                let requests = eligible
                    .iter()
                    .zip(bonuses)
                    .map(|(victim, bonus)| {
                        hostile_request(
                            victim.discord_id,
                            guild_id,
                            (base + bonus).min(victim.balance),
                            "soul_harvest",
                            user_id,
                            format!("soul_harvest:{purchase_id}:{}", victim.discord_id),
                            HostileDestination::Player,
                            Some(user_id),
                            self.config.hostile_min_balance,
                            now,
                            today,
                        )
                    })
                    .collect();
                let outcomes = self.apply_hostile_batch(requests).await?;
                let (total, absorbed) = hostile_totals(&outcomes);
                let shield = if absorbed > 0 {
                    format!(" Shields absorbed **{absorbed} {JOPACOIN_EMOTE}**.")
                } else {
                    String::new()
                };
                Ok(format!(
                    "🌿💀 **SOUL HARVEST** — <@{user_id}> drains the living!\nDrained up to **3 {JOPACOIN_EMOTE}** from **{}** players. Gained **{total} {JOPACOIN_EMOTE}**.{shield}\n(cost: {} {JOPACOIN_EMOTE}, balance: {})",
                    eligible.len(),
                    spec.cost,
                    balance_after_purchase + total
                ))
            }
            "dynamite_cache" => {
                let repository = self.repository.clone();
                run_blocking("Dynamite Cache write", move || {
                    repository.set_dig_temp_buff(
                        user_id,
                        guild_id,
                        &json!({
                            "id": "dynamite_cache",
                            "name": "Dynamite Cache",
                            "digs_remaining": 3,
                            "effect": {"yield_multiplier": 1.75},
                        }),
                    )
                })
                .await
                .map_err(|_| "Could not pack the cache; refunded.".to_owned())?;
                Ok(format!(
                    "⛰️🧨 **DYNAMITE CACHE** — <@{user_id}> packs the next 3 digs hot.\nYield +75% on your next 3 swings.\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                    spec.cost
                ))
            }
            "mana_shield" | "regrowth" => {
                let repository = self.repository.clone();
                let losses = run_blocking("manashop recent-loss lookup", move || {
                    repository.recent_loss_aggregates(
                        user_id,
                        guild_id,
                        now.saturating_sub(24 * 3_600),
                    )
                })
                .await
                .map_err(|error| error.to_string())?;
                let (base, percent, title, emoji, relation, reason) = if spec.key == "mana_shield" {
                    (
                        ((losses.largest as f64 * 0.60) as i64).min(120),
                        "60% of your largest loss",
                        "MANA SHIELD",
                        "🏝️🛡️",
                        "manashop_mana_shield",
                        "scaled manashop Mana Shield recovery",
                    )
                } else {
                    (
                        ((losses.total as f64 * 0.35) as i64).min(120),
                        "35% of your last 24h losses",
                        "REGROWTH",
                        "🌲💚",
                        "manashop_regrowth",
                        "scaled manashop Regrowth recovery",
                    )
                };
                let reward = self.adjust_generated_mana_reward(guild_id, base, now).await;
                if reward > 0 {
                    self.credit_mana_reward(
                        user_id,
                        guild_id,
                        reward,
                        "mana_reward",
                        relation,
                        today.to_owned(),
                        reason,
                        json!({"base_reward": base, "adjusted_reward": reward}),
                    )
                    .await?;
                }
                Ok(format!(
                    "{emoji} **{title}** — <@{user_id}> reclaims {reward} {JOPACOIN_EMOTE}.\n({percent} in the past 24h, capped at 120. Cost: {} {JOPACOIN_EMOTE}, balance: {})",
                    spec.cost,
                    new_balance + reward
                ))
            }
            "aegis" => {
                let repository = self.manashop.clone();
                run_blocking("Aegis grant", move || {
                    BuffService::new(repository, now).grant_aegis(user_id, guild_id)
                })
                .await
                .map_err(|_| "Could not raise the ward; refunded.".to_owned())?;
                Ok(format!(
                    "🌾🛡️ **AEGIS** — <@{user_id}> raises a fortified ward.\nFor 24h, it fully absorbs up to **75 {JOPACOIN_EMOTE}** of hostile losses (or one non-JC sabotage).\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                    spec.cost
                ))
            }
            "blood_pact" => {
                let target =
                    target.ok_or_else(|| "Blood Pact requires a target; refunded.".to_owned())?;
                let repository = self.manashop.clone();
                let target_id = target.id;
                run_blocking("Blood Pact grant", move || {
                    BuffService::new(repository, now).grant_blood_pact(user_id, guild_id, target_id)
                })
                .await
                .map_err(|_| "Could not seal the pact; refunded.".to_owned())?;
                Ok(format!(
                    "🌿🩸 **BLOOD PACT** — <@{user_id}> marks <@{}> for skim.\nFor 24h, you receive 25% of {}'s match/wheel/dig earnings, with no cap.\n(cost: {} {JOPACOIN_EMOTE}, balance: {new_balance})",
                    target.id, target.display_name, spec.cost
                ))
            }
            "wildfire" => {
                let eligible = self.eligible_hostile_players(user_id, guild_id).await?;
                let rolls = {
                    let mut entropy = self
                        .entropy
                        .lock()
                        .map_err(|_| "shop entropy lock is poisoned".to_owned())?;
                    (0..eligible.len())
                        .map(|_| entropy.i64_inclusive(4, 14))
                        .collect::<Vec<_>>()
                };
                let requests = eligible
                    .iter()
                    .zip(rolls)
                    .filter_map(|(victim, roll)| {
                        let amount = scale_deflationary_minigame_jc_delta(
                            roll as f64,
                            self.config.minigame_scale,
                        )
                        .min(victim.balance)
                        .max(0);
                        (amount > 0).then(|| {
                            hostile_request(
                                victim.discord_id,
                                guild_id,
                                amount,
                                "wildfire",
                                user_id,
                                format!("wildfire:{purchase_id}:{}", victim.discord_id),
                                HostileDestination::Burn,
                                None,
                                self.config.hostile_min_balance,
                                now,
                                today,
                            )
                        })
                    })
                    .collect();
                let outcomes = self.apply_hostile_batch(requests).await?;
                let (total, absorbed) = hostile_totals(&outcomes);
                let gain = (total as f64 * 0.45) as i64;
                if gain > 0 {
                    self.credit_mana_reward(
                        user_id,
                        guild_id,
                        gain,
                        "manashop",
                        "hostile_loss_reward",
                        format!("wildfire:{purchase_id}"),
                        "manashop wildfire harvest",
                        json!({
                            "kind": "wildfire",
                            "applied_loss": total,
                            "absorbed_amount": absorbed,
                        }),
                    )
                    .await?;
                }
                let shield = if absorbed > 0 {
                    format!(" Shields absorbed **{absorbed} {JOPACOIN_EMOTE}**.")
                } else {
                    String::new()
                };
                Ok(format!(
                    "⛰️🔥🔥 **WILDFIRE** — <@{user_id}> consumes the day's color in flame.\nDrained **{total} {JOPACOIN_EMOTE}** from {} players. You claim **{gain} {JOPACOIN_EMOTE}** of the harvest.{shield}\n(cost: {} {JOPACOIN_EMOTE}, mana spent, balance: {})",
                    eligible.len(),
                    spec.cost,
                    new_balance + gain
                ))
            }
            "counterspell" => {
                let repository = self.manashop.clone();
                run_blocking("Counterspell grant", move || {
                    BuffService::new(repository, now).grant_counterspell(user_id, guild_id)
                })
                .await
                .map_err(|_| "Could not weave the ward; refunded.".to_owned())?;
                Ok(format!(
                    "🏝️🜨 **COUNTERSPELL** — <@{user_id}> weaves a 24h ward.\nAll Pyroclasm / Soul Harvest / Sabotage / Blood Pact / Wildfire targeting you is repelled for the next 24 hours.\n(cost: {} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})",
                    spec.cost
                ))
            }
            "overgrowth" => {
                let repository = self.manashop.clone();
                run_blocking("Overgrowth grant", move || {
                    BuffService::new(repository, now).grant_overgrowth(user_id, guild_id)
                })
                .await
                .map_err(|_| "Could not seed the overgrowth; refunded.".to_owned())?;
                Ok(format!(
                    "🌲🌳 **OVERGROWTH** — <@{user_id}> burns the day's mana into the soil.\nFor 12h: your next 10 digs get +10 JC and cave-in chance halved.\n(cost: {} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})",
                    spec.cost
                ))
            }
            "sanctuary" => {
                let target =
                    target.ok_or_else(|| "Sanctuary requires a target; refunded.".to_owned())?;
                if self
                    .load_player(target.id, guild_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    return Err(format!("<@{}> is not registered; refunded.", target.id));
                }
                let repository = self.manashop.clone();
                let ally_id = target.id;
                run_blocking("Sanctuary grant", move || {
                    BuffService::new(repository, now).grant_sanctuary(user_id, guild_id, ally_id)
                })
                .await
                .map_err(|_| "Could not bind the sanctuary; refunded.".to_owned())?;
                Ok(format!(
                    "🌾🕊️ **SANCTUARY** — <@{user_id}> shelters <@{}>.\nFor 24h, both of you share **150 {JOPACOIN_EMOTE}** of full hostile-loss protection and non-JC PvP immunity.\n(cost: {} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})",
                    target.id, spec.cost
                ))
            }
            "dark_bargain" => {
                // The debt and its principal commit together: a player must
                // never owe the bargain without having been paid it.
                let repository = self.manashop.clone();
                run_blocking("Dark Bargain", move || {
                    BuffService::new(repository, now)
                        .grant_dark_bargain(user_id, guild_id, 700, 800)
                })
                .await
                .map_err(|_| "Could not strike the bargain; refunded.".to_owned())?;
                Ok(format!(
                    "🌿💀 **DARK BARGAIN** — <@{user_id}> signs in red ink.\n+800 {JOPACOIN_EMOTE} now. 700 due in 7 days. Default: -1600 + bankruptcy +5 matches.\n(cost: {} {JOPACOIN_EMOTE}, mana spent, balance: {})",
                    spec.cost,
                    new_balance + 800
                ))
            }
            _ => Err("Unknown item.".to_owned()),
        }
    }

    async fn eligible_hostile_players(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Vec<ShopPlayer>, String> {
        let repository = self.repository.clone();
        let players = run_blocking("manashop leaderboard", move || repository.players(guild_id))
            .await
            .map_err(|error| error.to_string())?;
        Ok(players
            .into_iter()
            .filter(|player| {
                player.discord_id != user_id && player.balance >= self.config.hostile_min_balance
            })
            .collect())
    }

    async fn apply_hostile_batch(
        &self,
        requests: Vec<HostileLossRequest>,
    ) -> Result<Vec<Result<cama_db::shop_runtime::HostileLossSettlement, String>>, String> {
        let repository = self.repository.clone();
        run_blocking("manashop hostile-loss batch", move || {
            repository.apply_hostile_losses(&requests)
        })
        .await
        .map_err(|error| error.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn credit_mana_reward(
        &self,
        user_id: i64,
        guild_id: i64,
        amount: i64,
        source: &'static str,
        related_type: &'static str,
        related_id: String,
        reason: &'static str,
        metadata: JsonValue,
    ) -> Result<i64, String> {
        let repository = self.repository.clone();
        run_blocking("manashop reward credit", move || {
            repository.adjust_balance(
                user_id,
                guild_id,
                amount,
                Some(source),
                Some(user_id),
                Some(related_type),
                Some(&related_id),
                Some(reason),
                Some(&metadata),
            )
        })
        .await
        .map_err(|error| error.to_string())
    }

    async fn adjust_generated_mana_reward(&self, guild_id: i64, base: i64, now: i64) -> i64 {
        let scaled = scale_minigame_jc_delta(base as f64, self.config.minigame_scale);
        let service = self.economy_events.clone();
        run_blocking("daily economy-event reward adjustment", move || {
            service.adjust_reward_at(guild_id, scaled, now)
        })
        .await
        .unwrap_or(scaled)
    }

    async fn flex_stats(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<FlexStats, InteractionHandlerError> {
        let player = self.load_player(discord_id, guild_id).await?;
        let gambling = self.gambling.clone();
        let (stats, bankruptcies) = run_blocking("shop flex-stat lookup", move || {
            let service = GamblingStatsService::new(gambling.clone());
            let stats = service
                .get_player_stats(discord_id, Some(guild_id))
                .ok()
                .flatten();
            let bankruptcies = gambling
                .player_bankruptcy_count(discord_id, Some(guild_id))
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>((stats, bankruptcies))
        })
        .await?;
        let (balance, wins, losses, rating) = player.map_or((0, 0, 0, None), |player| {
            (
                player.balance,
                player.wins,
                player.losses,
                player.glicko_rating,
            )
        });
        let games = wins.saturating_add(losses);
        Ok(FlexStats {
            balance,
            wins,
            losses,
            win_rate: (games > 0).then_some(wins as f64 / games as f64 * 100.0),
            rating,
            total_bets: stats.as_ref().map_or(0, |stats| stats.total_bets),
            net_pnl: stats.as_ref().map_or(0, |stats| stats.net_pnl),
            degen_score: stats.as_ref().map(|stats| stats.degen_score.total),
            bankruptcies,
        })
    }

    fn flex_comparison(buyer: &FlexStats, target: &FlexStats) -> FlexComparison {
        let mut comparison = FlexComparison::default();
        if buyer.balance > target.balance {
            comparison
                .buyer_wins
                .push(format!("{} more jopacoin", buyer.balance - target.balance));
        } else if target.balance > buyer.balance {
            comparison
                .target_wins
                .push(format!("{} more jopacoin", target.balance - buyer.balance));
        }
        if buyer.win_rate.is_some_and(|rate| rate != 0.0)
            && target.win_rate.is_some_and(|rate| rate != 0.0)
        {
            let buyer_rate = buyer.win_rate.unwrap_or_default();
            let target_rate = target.win_rate.unwrap_or_default();
            if buyer_rate > target_rate {
                comparison
                    .buyer_wins
                    .push(format!("{buyer_rate:.0}% vs {target_rate:.0}% win rate"));
            } else if target_rate > buyer_rate {
                comparison.target_wins.push("better win rate".to_owned());
            }
        }
        if buyer.rating.is_some_and(|rating| rating != 0.0)
            && target.rating.is_some_and(|rating| rating != 0.0)
        {
            let buyer_rating = buyer.rating.unwrap_or_default();
            let target_rating = target.rating.unwrap_or_default();
            if buyer_rating > target_rating {
                comparison.buyer_wins.push(format!(
                    "{} higher rating",
                    (buyer_rating - target_rating) as i64
                ));
            } else if target_rating > buyer_rating {
                comparison.target_wins.push("higher rating".to_owned());
            }
        }
        if buyer.net_pnl > target.net_pnl {
            comparison
                .buyer_wins
                .push(format!("{} better P&L", buyer.net_pnl - target.net_pnl));
        }
        if buyer.bankruptcies < target.bankruptcies {
            comparison.buyer_wins.push(format!(
                "fewer bankruptcies ({} vs {})",
                buyer.bankruptcies, target.bankruptcies
            ));
        } else if target.bankruptcies < buyer.bankruptcies {
            comparison.target_wins.push("fewer bankruptcies".to_owned());
        }
        comparison
    }

    fn cherry_picked_flex_stats(
        buyer: &FlexStats,
        target: &FlexStats,
        target_name: &str,
        targeted_cost: i64,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        if buyer.balance > target.balance {
            lines.push(format!(
                "**+{}** more jopacoin than {target_name}",
                buyer.balance - target.balance
            ));
        }
        if let (Some(buyer_rating), Some(target_rating)) = (buyer.rating, target.rating)
            && buyer_rating != 0.0
            && target_rating != 0.0
            && buyer_rating > target_rating
        {
            lines.push(format!(
                "**+{}** higher rating",
                (buyer_rating - target_rating) as i64
            ));
        }
        if let (Some(buyer_rate), Some(target_rate)) = (buyer.win_rate, target.win_rate)
            && buyer_rate != 0.0
            && target_rate != 0.0
            && buyer_rate > target_rate
        {
            lines.push(format!(
                "**{buyer_rate:.0}%** win rate vs {target_rate:.0}%"
            ));
        }
        if buyer.net_pnl > target.net_pnl {
            lines.push(format!(
                "**+{}** better gambling P&L",
                buyer.net_pnl - target.net_pnl
            ));
        }
        if buyer.bankruptcies < target.bankruptcies {
            lines.push(format!(
                "Only **{}** bankruptcies vs {}",
                buyer.bankruptcies, target.bankruptcies
            ));
        }
        if buyer.total_bets > target.total_bets {
            lines.push(format!(
                "**{}** bets placed (more action)",
                buyer.total_bets
            ));
        }
        if lines.is_empty() {
            lines.push(
                "*(Stats were cherry-picked but... we couldn't find any advantages)*".to_owned(),
            );
            lines.push(format!(
                "*At least you spent {targeted_cost} to flex on them*"
            ));
        }
        lines
    }

    async fn simple_purchase(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
        target: Option<UserOption>,
        product: SimpleProduct,
    ) -> Result<(), InteractionHandlerError> {
        let cost = product.cost(&self.config);
        let Some(player) = self.load_player(context.user_id, context.guild_id).await? else {
            let copy = if matches!(
                product,
                SimpleProduct::Announce | SimpleProduct::AnnounceTarget
            ) {
                "You need to `/player register` before you can shop. Hard to flex wealth you don't have."
            } else {
                "You need to `/player register` before you can shop."
            };
            return respond_ephemeral(&responder, copy).await;
        };
        if player.balance < cost {
            let suffix = if matches!(
                product,
                SimpleProduct::Announce | SimpleProduct::AnnounceTarget
            ) {
                " Maybe try earning some money before flexing?"
            } else {
                ""
            };
            return respond_ephemeral(
                &responder,
                format!(
                    "You need {cost} {JOPACOIN_EMOTE} for this, but you only have {}.{suffix}",
                    player.balance
                ),
            )
            .await;
        }
        let repository = self.repository.clone();
        let source = product.source();
        let reason = product.reason();
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let spent = run_blocking("shop purchase", move || {
            repository.try_spend(user_id, guild_id, cost, source, reason)
        })
        .await?;
        if !spent {
            if let Err(error) = respond_ephemeral(
                &responder,
                format!("You no longer have {cost} {JOPACOIN_EMOTE} for this."),
            )
            .await
            {
                warn!(user_id, %error, "shop shortfall notice could not be sent");
            }
            return Ok(());
        }

        let _ = responder.defer(false).await;
        let (title, description, color, footer, thumbnail, field) = match product {
            SimpleProduct::JopaCoin => (
                "🪙 Jopa Coin(TM) Minted!",
                format!("<@{user_id}> has minted a **Jopa Coin(TM)**!"),
                JOPA_COIN_COLOR,
                format!("Cost: {cost} jopacoin"),
                None,
                None,
            ),
            SimpleProduct::MysteryGift => (
                "🎁 Mystery Gift Redeemed!",
                format!("<@{user_id}> has redeemed a **Mystery Gift**!"),
                0x9B_59_B6,
                format!("Cost: {cost} jopacoin"),
                None,
                None,
            ),
            SimpleProduct::Announce | SimpleProduct::AnnounceTarget => {
                let buyer_stats = self.flex_stats(user_id, guild_id).await?;
                let new_balance = buyer_stats.balance;
                let target_stats = if let Some(target) = target.as_ref() {
                    Some(self.flex_stats(target.id, guild_id).await?)
                } else {
                    None
                };
                let comparison = target_stats
                    .as_ref()
                    .map(|target_stats| Self::flex_comparison(&buyer_stats, target_stats));
                let mut details = BTreeMap::from([
                    (
                        "buyer_name".to_owned(),
                        AiValue::Text(context.user_display_name),
                    ),
                    ("buyer_balance".to_owned(), AiValue::Integer(new_balance)),
                    ("cost_paid".to_owned(), AiValue::Integer(cost)),
                    ("buyer_stats".to_owned(), buyer_stats.ai_value()),
                ]);
                if let (Some(target), Some(target_stats), Some(comparison)) =
                    (target.as_ref(), target_stats.as_ref(), comparison.as_ref())
                {
                    details.insert(
                        "target_name".to_owned(),
                        AiValue::Text(target.ai_display_name.clone()),
                    );
                    details.insert("target_stats".to_owned(), target_stats.ai_value());
                    details.insert("comparison".to_owned(), comparison.ai_value());
                }
                let flavor = self
                    .shop_flavor(
                        context.guild_id,
                        context.user_id,
                        if target.is_some() {
                            FlavorEvent::ShopAnnounceTarget
                        } else {
                            FlavorEvent::ShopAnnounce
                        },
                        details,
                    )
                    .await;
                let fallback = if let Some(target) = target.as_ref() {
                    let template = {
                        let index = self
                            .entropy
                            .lock()
                            .map_err(|_| "shop entropy lock is poisoned")?
                            .index(ANNOUNCE_TARGET_MESSAGES.len());
                        ANNOUNCE_TARGET_MESSAGES[index]
                    };
                    let ratio = if self.config.announce_cost > 0 {
                        self.config.announce_target_cost / self.config.announce_cost
                    } else {
                        10
                    };
                    template
                        .replace("{user}", &format!("<@{user_id}>"))
                        .replace("{target}", &format!("<@{}>", target.id))
                        .replace("{cost}", &cost.to_string())
                        .replace("{emote}", JOPACOIN_EMOTE)
                        .replace("{ratio}", &ratio.to_string())
                } else {
                    let fallback = {
                        let index = self
                            .entropy
                            .lock()
                            .map_err(|_| "shop entropy lock is poisoned")?
                            .index(ANNOUNCE_MESSAGES.len());
                        ANNOUNCE_MESSAGES[index]
                    };
                    format!("\"{fallback}\"")
                };
                let opening = flavor.unwrap_or(fallback);
                let field =
                    target
                        .as_ref()
                        .zip(target_stats.as_ref())
                        .map(|(target, target_stats)| {
                            (
                                "The Numbers Don't Lie".to_owned(),
                                Self::cherry_picked_flex_stats(
                                    &buyer_stats,
                                    target_stats,
                                    &target.display_name,
                                    self.config.announce_target_cost,
                                )
                                .join("\n"),
                            )
                        });
                (
                    "WEALTH ANNOUNCEMENT",
                    format!(
                        "*{opening}*\n\n━━━━━━━━━━━━━━━━━━━━━━━━━\n\n<@{user_id}> has\n\n**{new_balance}** {JOPACOIN_EMOTE}\n\n━━━━━━━━━━━━━━━━━━━━━━━━━"
                    ),
                    BOUNTY_HUNTER_ANNOUNCE_COLOR,
                    format!("This flex cost {cost} jopacoin"),
                    Some(BOUNTY_HUNTER_IMAGE_URL),
                    field,
                )
            }
        };
        let mut embed = InteractionEmbed::titled(title)
            .description(description)
            .color(color)
            .footer(footer);
        if let Some(thumbnail) = thumbnail {
            embed = embed.thumbnail(thumbnail);
        }
        if let Some((name, value)) = field {
            embed = embed.field(name, value, false);
        }
        let mut response = InteractionResponse::message("").embed(embed);
        if let Some(target) = target {
            response.content = format!("<@{}>", target.id);
            response = response.with_user_mentions(vec![target.unsigned_id]);
        } else {
            response = response.without_mentions();
        }
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn double_or_nothing(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let now = self.clock.now()?;
        let won = self
            .entropy
            .lock()
            .map_err(|_| "shop entropy lock is poisoned")?
            .chance(0.5);
        let request = DoubleOrNothingRequest {
            discord_id: context.user_id,
            guild_id: context.guild_id,
            cost: self.config.double_or_nothing_cost,
            won,
            now,
            cooldown_seconds: self.config.double_or_nothing_cooldown,
            bypass_cooldown: is_admin(&context, &self.config),
        };
        let repository = self.repository.clone();
        let settlement = run_blocking("double-or-nothing settlement", move || {
            repository.settle_double_or_nothing(request)
        })
        .await?;
        if !settlement.success {
            let balance = settlement.balance_before;
            let message = match settlement.reason {
                Some(PurchaseFailureReason::NotRegistered) => {
                    "You need to `/player register` before you can gamble. Can't double nothing if you have nothing.".to_owned()
                }
                Some(PurchaseFailureReason::OnCooldown) => {
                    "You already tempted fate recently. Wait for your Double or Nothing cooldown before trying again.".to_owned()
                }
                Some(PurchaseFailureReason::InDebt) => format!(
                    "You're in debt ({balance} {JOPACOIN_EMOTE}). You can't double debt. Pay it off first!"
                ),
                Some(PurchaseFailureReason::NothingToDouble) => format!(
                    "You only have {balance} {JOPACOIN_EMOTE} — exactly the ante cost. Nothing left to double! Earn more first."
                ),
                _ => format!(
                    "You need {} {JOPACOIN_EMOTE} for this, but you only have {balance}. Can't afford the ante.",
                    self.config.double_or_nothing_cost
                ),
            };
            respond_ephemeral(&responder, message).await?;
            if settlement.reason == Some(PurchaseFailureReason::OnCooldown) {
                self.emit_neon_cooldown(&context, now).await;
            }
            return Ok(());
        }
        let _ = responder.defer(false).await;
        let flavor_event = if settlement.won {
            FlavorEvent::DoubleOrNothingWin
        } else {
            FlavorEvent::DoubleOrNothingLose
        };
        let generated_flavor = self
            .shop_flavor(
                context.guild_id,
                context.user_id,
                flavor_event,
                BTreeMap::from([
                    (
                        "starting_balance".to_owned(),
                        AiValue::Integer(settlement.balance_before),
                    ),
                    (
                        "cost".to_owned(),
                        AiValue::Integer(self.config.double_or_nothing_cost),
                    ),
                    (
                        "balance_at_risk".to_owned(),
                        AiValue::Integer(settlement.balance_at_risk),
                    ),
                    (
                        "final_balance".to_owned(),
                        AiValue::Integer(settlement.balance_after),
                    ),
                    ("won".to_owned(), AiValue::Bool(settlement.won)),
                    (
                        "net_change".to_owned(),
                        AiValue::Integer(settlement.balance_after - settlement.balance_before),
                    ),
                ]),
            )
            .await;
        let flavor = generated_flavor.unwrap_or_else(|| {
            let examples = if settlement.won {
                DOUBLE_OR_NOTHING_WIN_MESSAGES
            } else {
                DOUBLE_OR_NOTHING_LOSE_MESSAGES
            };
            let index = self
                .entropy
                .lock()
                .map_or(0, |mut entropy| entropy.index(examples.len()));
            examples[index].to_owned()
        });
        let title = if settlement.won {
            "DOUBLE!"
        } else {
            "NOTHING!"
        };
        let color = if settlement.won {
            0x00_FF_00
        } else {
            0xFF_00_00
        };
        let net = settlement.balance_after - settlement.balance_before;
        let result_line = if settlement.won {
            format!(
                "**Result:** {} x 2 = **{}** {JOPACOIN_EMOTE}\n**Net Gain:** +{net} {JOPACOIN_EMOTE}",
                settlement.balance_at_risk, settlement.balance_after
            )
        } else {
            format!(
                "**Result:** **{}** {JOPACOIN_EMOTE}\n**Net Loss:** {net} {JOPACOIN_EMOTE}",
                settlement.balance_after
            )
        };
        let avatar = self
            .discord
            .shop_user_avatar_url(context.guild_id, context.user_id)
            .await
            .unwrap_or(None);
        let mut embed = InteractionEmbed::titled(format!("Double or Nothing: {title}"))
            .description(format!(
                "*{flavor}*\n\n━━━━━━━━━━━━━━━━━━━━━━━━━\n\n**Starting Balance:** {} {JOPACOIN_EMOTE}\n**Entry Cost:** -{} {JOPACOIN_EMOTE}\n**At Risk:** {} {JOPACOIN_EMOTE}\n\n{result_line}",
                settlement.balance_before,
                self.config.double_or_nothing_cost,
                settlement.balance_at_risk
            ))
            .color(color)
            .footer(format!(
                "Entry: {} JC | Cooldown: 30 days",
                self.config.double_or_nothing_cost
            ));
        if let Some(avatar) = avatar {
            embed = embed.thumbnail(avatar);
        }
        responder
            .followup(
                InteractionResponse::message(format!("<@{}>", context.user_id))
                    .with_user_mentions(vec![
                        u64::try_from(context.user_id).map_err(|_| "invalid user id")?,
                    ])
                    .embed(embed),
            )
            .await
            .map_err(|error| error.to_string())?;
        self.emit_neon_double(&context, now, &settlement).await;
        Ok(())
    }

    async fn soft_avoid(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
        target: UserOption,
    ) -> Result<(), InteractionHandlerError> {
        if target.id == context.user_id {
            return respond_ephemeral(&responder, "You can't soft avoid yourself.").await;
        }
        if self
            .load_player(context.user_id, context.guild_id)
            .await?
            .is_none()
        {
            return respond_ephemeral(
                &responder,
                "You need to `/player register` before you can shop.",
            )
            .await;
        }
        if self
            .load_player(target.id, context.guild_id)
            .await?
            .is_none()
        {
            return respond_ephemeral(&responder, "The target player is not registered.").await;
        }
        let soft_avoids = self.soft_avoids.clone();
        let user_id = context.user_id;
        let target_id = target.id;
        let guild_id = context.guild_id;
        let active = run_blocking("soft avoid preflight", move || {
            soft_avoids.get_user_avoids(Some(guild_id), user_id)
        })
        .await?;
        if let Some(existing) = active
            .iter()
            .find(|avoid| avoid.avoided_discord_id == target.id)
        {
            return respond_ephemeral(
                &responder,
                format!(
                    "Your soft avoid for **{}** is already active with {} games remaining.",
                    target.display_name, existing.games_remaining
                ),
            )
            .await;
        }
        let pairings = self.pairings.clone();
        let pairing = run_blocking("soft avoid pairing lookup", move || {
            pairings.get_pairings_for_players(&[user_id, target_id], Some(guild_id))
        })
        .await?;
        let pair_key = PairingsRepository::canonical_pair(context.user_id, target.id);
        let cost = calculate_soft_avoid_cost(
            pairing.get(&pair_key).map(|pairing| PairingRecord {
                games_together: pairing.games_together,
                wins_together: pairing.wins_together,
            }),
            self.config.soft_avoid_cost,
        );
        if responder.defer(true).await.is_err() {
            return Ok(());
        }
        let repository = self.soft_avoids.clone();
        let result = run_blocking("soft avoid purchase", move || {
            repository.purchase_avoid(Some(guild_id), user_id, target_id, cost, SOFT_AVOID_GAMES)
        })
        .await;
        let response = match result {
            Err(error) => {
                error!(%error, "failed to complete soft avoid purchase");
                InteractionResponse::message(
                    "Soft avoid purchase failed before it could be activated. You were not charged.",
                )
                .ephemeral()
                .without_mentions()
            }
            Ok(purchase) if !purchase.success => {
                let content = match purchase.reason {
                    Some(SoftAvoidPurchaseFailureReason::AlreadyActive) => format!(
                        "Your soft avoid for **{}** is already active with {} games remaining.",
                        target.display_name,
                        purchase
                            .avoid
                            .as_ref()
                            .map_or(0, |avoid| avoid.games_remaining)
                    ),
                    Some(SoftAvoidPurchaseFailureReason::InsufficientBalance) => format!(
                        "You need {cost} {JOPACOIN_EMOTE} for this, but you only have {}.",
                        purchase.balance
                    ),
                    None => "Soft avoid purchase could not be completed. You were not charged."
                        .to_owned(),
                };
                InteractionResponse::message(content)
                    .ephemeral()
                    .without_mentions()
            }
            Ok(purchase) => {
                let avoid = purchase
                    .avoid
                    .ok_or("successful soft avoid purchase returned no activation")?;
                InteractionResponse::message("")
                    .ephemeral()
                    .without_mentions()
                    .embed(
                        InteractionEmbed::titled("Soft Avoid Active")
                            .description(format!(
                                "You are now soft-avoiding **{}**.\n\n**Games remaining:** {}\n\nWhen shuffling, the system will try to place you on opposite teams. The avoid count decreases each game where you're both playing and successfully placed on opposite teams.",
                                target.display_name, avoid.games_remaining
                            ))
                            .color(0x72_89_DA)
                            .footer(format!("Cost: {cost} jopacoin")),
                    )
            }
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn package_deal(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
        target: UserOption,
    ) -> Result<(), InteractionHandlerError> {
        if target.id == context.user_id {
            return respond_ephemeral(&responder, "You can't package deal with yourself.").await;
        }
        let Some(player) = self.load_player(context.user_id, context.guild_id).await? else {
            return respond_ephemeral(
                &responder,
                "You need to `/player register` before you can shop.",
            )
            .await;
        };
        let Some(target_player) = self.load_player(target.id, context.guild_id).await? else {
            return respond_ephemeral(&responder, "The target player is not registered.").await;
        };
        let divisor = self.config.package_deal_rating_divisor;
        let rating =
            |rating: Option<f64>| rating.filter(|rating| *rating != 0.0).unwrap_or(1_500.0);
        let active_cost = self.config.package_deal_base_cost
            + ((rating(player.glicko_rating) + rating(target_player.glicko_rating)) / divisor)
                as i64;
        let games = self
            .config
            .package_deal_games
            .clamp(1, PACKAGE_DEAL_MAX_GAMES);
        if responder.defer(true).await.is_err() {
            return Ok(());
        }
        let repository = self.package_deals.clone();
        let guild_id = context.guild_id;
        let user_id = context.user_id;
        let target_id = target.id;
        let result = run_blocking("package deal purchase", move || {
            repository.purchase_deal(
                Some(guild_id),
                user_id,
                target_id,
                games,
                PACKAGE_DEAL_NO_ACTIVE_COST,
                active_cost,
            )
        })
        .await;
        let response = match result {
            Err(error) => {
                error!(%error, "failed to complete package deal purchase");
                InteractionResponse::message(
                    "Package deal purchase failed before it could be activated. You were not charged.",
                )
                .ephemeral()
                .without_mentions()
            }
            Ok(purchase) if !purchase.success => {
                let content = match purchase.reason {
                    Some(PackageDealPurchaseFailureReason::AlreadyFull) => format!(
                        "Your Package Deal with **{}** already has {} games remaining. You were not charged.",
                        target.display_name,
                        purchase
                            .deal
                            .as_ref()
                            .map_or(0, |deal| deal.games_remaining)
                    ),
                    Some(PackageDealPurchaseFailureReason::InsufficientBalance) | None => format!(
                        "You need {} {JOPACOIN_EMOTE} for this, but you only have {}.",
                        purchase.cost, purchase.balance
                    ),
                };
                InteractionResponse::message(content)
                    .ephemeral()
                    .without_mentions()
            }
            Ok(purchase) => {
                let deal = purchase
                    .deal
                    .ok_or("successful package deal purchase returned no activation")?;
                let footer = if purchase.active_deal_count == 0 {
                    "No active package deals: 1 jopacoin".to_owned()
                } else {
                    format!(
                        "Base cost: {} + Rating bonus: {}",
                        self.config.package_deal_base_cost,
                        purchase.cost - self.config.package_deal_base_cost
                    )
                };
                InteractionResponse::message("")
                    .ephemeral()
                    .without_mentions()
                    .embed(
                        InteractionEmbed::titled("Package Deal Active")
                            .description(format!(
                                "You have a Package Deal with **{}**.\n\n**Games added:** {}\n**Games remaining:** {}\n**Cost:** {} {JOPACOIN_EMOTE}\n\nWhen shuffling, the system will try to place you on the **same team**. The deal count decreases each game where you're both playing and successfully placed on the same team.",
                                target.display_name,
                                deal.games_added,
                                deal.games_remaining,
                                purchase.cost
                            ))
                            .color(0x2E_CC_71)
                            .footer(footer),
                    )
            }
        };
        responder
            .followup(response)
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn recalibration_cooldown(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let recalibration = self.recalibration.clone();
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let now = self.clock.now()?;
        let state = run_blocking("recalibration cooldown lookup", move || {
            recalibration.get_state_at(user_id, Some(guild_id), now)
        })
        .await?;
        let message = state.cooldown_ends_at.map_or_else(
            || "Recalibration is on cooldown.".to_owned(),
            |ends| format!("Recalibration is on cooldown. You can recalibrate again <t:{ends}:R>."),
        );
        respond_ephemeral(&responder, message).await
    }

    async fn recalibrate(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let now = self.clock.now()?;
        let Some(player) = self.load_player(context.user_id, context.guild_id).await? else {
            return respond_ephemeral(
                &responder,
                "You need to `/player register` before you can recalibrate.",
            )
            .await;
        };
        let user_id = context.user_id;
        let guild_id = context.guild_id;
        let recalibration = self.recalibration.clone();
        let eligibility = run_blocking("recalibration eligibility", move || {
            recalibration.can_recalibrate_at(user_id, Some(guild_id), now)
        })
        .await?;
        match eligibility {
            RecalibrationEligibility::NotRegistered => {
                return respond_ephemeral(
                    &responder,
                    "You need to `/player register` before you can recalibrate.",
                )
                .await;
            }
            RecalibrationEligibility::NoRating => {
                return respond_ephemeral(
                    &responder,
                    "You don't have a rating yet. Play some games first!",
                )
                .await;
            }
            RecalibrationEligibility::InsufficientGames {
                games_played,
                min_games,
            } => {
                return respond_ephemeral(
                    &responder,
                    format!(
                        "You need at least {min_games} games to recalibrate (you have {games_played})."
                    ),
                )
                .await;
            }
            RecalibrationEligibility::OnCooldown { cooldown_ends_at } => {
                return respond_ephemeral(
                    &responder,
                    format!(
                        "Recalibration is on cooldown. You can recalibrate again <t:{cooldown_ends_at}:R>."
                    ),
                )
                .await;
            }
            RecalibrationEligibility::Allowed { .. } => {}
        }
        let mana = self.mana.clone();
        let today = pacific_mana_day(now)?;
        let mana_row = run_blocking("recalibration mana lookup", move || {
            mana.get_mana(user_id, Some(guild_id))
        })
        .await
        .ok()
        .flatten();
        let discount = mana_row
            .as_ref()
            .filter(|mana| mana.assigned_date == today && !mana.consumed_today)
            .and_then(|mana| Land::from_name(&mana.current_land))
            .filter(|land| *land == Land::Island)
            .map_or(0.0, |_| 0.10);
        let cost = ((self.config.recalibrate_cost as f64) * (1.0 - discount)).floor() as i64;
        let cost = cost.max(1);
        if player.balance < cost {
            return respond_ephemeral(
                &responder,
                format!(
                    "You need **{cost}** {JOPACOIN_EMOTE} to recalibrate but only have **{}** {JOPACOIN_EMOTE}.",
                    player.balance
                ),
            )
            .await;
        }
        let repository = self.repository.clone();
        let spent = run_blocking("recalibration debit", move || {
            repository.try_spend(
                user_id,
                guild_id,
                cost,
                "shop_recalibrate",
                "shop recalibration purchase",
            )
        })
        .await?;
        if !spent {
            return respond_ephemeral(
                &responder,
                format!("You no longer have {cost} {JOPACOIN_EMOTE} for this."),
            )
            .await;
        }
        let _ = responder.defer(false).await;
        let recalibration = self.recalibration.clone();
        let result = run_blocking("rating recalibration", move || {
            recalibration.recalibrate_at(user_id, Some(guild_id), now)
        })
        .await;
        let success = match result {
            Ok(RecalibrationExecution::Success(success)) => Some(success),
            Ok(RecalibrationExecution::Rejected(reason)) => {
                warn!(?reason, user_id, "recalibration rejected after debit");
                None
            }
            Err(error) => {
                error!(%error, user_id, "recalibration failed after debit");
                None
            }
        };
        let Some(success) = success else {
            let repository = self.repository.clone();
            let _ = run_blocking("recalibration refund", move || {
                repository.adjust_balance(
                    user_id,
                    guild_id,
                    cost,
                    Some("shop_recalibrate"),
                    Some(user_id),
                    Some("command"),
                    Some("recalibrate"),
                    Some("recalibration failed; purchase refunded"),
                    None,
                )
            })
            .await;
            return responder
                .followup(
                    InteractionResponse::message(
                        "Recalibration failed unexpectedly. You have been refunded.",
                    )
                    .without_mentions(),
                )
                .await
                .map_err(|error| error.to_string().into());
        };
        responder
            .followup(
                InteractionResponse::message("")
                    .without_mentions()
                    .embed(
                        InteractionEmbed::titled("Rating Recalibration")
                            .description(format!(
                                "<@{user_id}> has recalibrated their rating!\n\nTheir rating deviation has been reset — expect bigger rating swings for the next ~20 games."
                            ))
                            .color(0xE7_4C_3C)
                            .thumbnail("https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/bounty_hunter.png")
                            .field("Rating", format!("{:.0} (unchanged)", success.old_rating), true)
                            .field("RD", format!("{:.0} → {:.0}", success.old_rd, success.new_rd), true)
                            .field(
                                "Next Recalibration",
                                format!("<t:{}:R>", success.cooldown_ends_at),
                                true,
                            )
                            .field(
                                "Cost",
                                format!("{} {JOPACOIN_EMOTE}", self.config.recalibrate_cost),
                                true,
                            ),
                    ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn load_player(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<ShopPlayer>, InteractionHandlerError> {
        let repository = self.repository.clone();
        run_blocking("shop player lookup", move || {
            repository.player(user_id, guild_id)
        })
        .await
    }

    async fn shop_flavor(
        &self,
        guild_id: i64,
        user_id: i64,
        event: FlavorEvent,
        details: BTreeMap<String, AiValue>,
    ) -> Option<String> {
        let flavor = self.flavor.clone()?;
        tokio::task::spawn_blocking(move || {
            flavor.generate_event_flavor(Some(guild_id), event, user_id, details)
        })
        .await
        .ok()
        .flatten()
    }

    async fn emit_neon_cooldown(&self, context: &CommandContext, now: i64) {
        let (Ok(user_id), Ok(guild_id), Some(channel_id)) = (
            u64::try_from(context.user_id),
            u64::try_from(context.guild_id),
            context.channel_id,
        ) else {
            return;
        };
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now).unwrap_or_default());
            neon.on_cooldown_hit(user_id, Some(guild_id), "double_or_nothing")
        });
        if let Some(result) = result {
            self.send_neon(channel_id, result).await;
        }
    }

    async fn emit_neon_double(
        &self,
        context: &CommandContext,
        now: i64,
        settlement: &cama_db::shop_runtime::DoubleOrNothingSettlement,
    ) {
        let (Ok(user_id), Ok(guild_id), Some(channel_id)) = (
            u64::try_from(context.user_id),
            u64::try_from(context.guild_id),
            context.channel_id,
        ) else {
            return;
        };
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now).unwrap_or_default());
            neon.on_double_or_nothing(
                user_id,
                Some(guild_id),
                settlement.won,
                settlement.balance_at_risk,
                settlement.balance_after,
            )
        });
        if let Some(result) = result {
            self.send_neon(channel_id, result).await;
        }
    }

    async fn send_neon(&self, channel_id: i64, result: NeonResult) {
        let mut response = InteractionResponse::message(
            result.text_block.or(result.footer_text).unwrap_or_default(),
        )
        .without_mentions();
        if let Some(gif) = result.gif_file {
            response = response.attachment(InteractionAttachment::bytes(
                "jopat_terminal.gif",
                gif.bytes,
            ));
        }
        if let Err(error) = self
            .discord
            .shop_send_temporary(channel_id, response, Duration::from_secs(60))
            .await
        {
            debug!(%error, "failed to deliver temporary Neon shop message");
        }
    }
}

#[derive(Clone, Copy)]
enum SimpleProduct {
    Announce,
    AnnounceTarget,
    JopaCoin,
    MysteryGift,
}

impl SimpleProduct {
    const fn cost(self, config: &ShopRuntimeConfig) -> i64 {
        match self {
            Self::Announce => config.announce_cost,
            Self::AnnounceTarget => config.announce_target_cost,
            Self::JopaCoin => config.jopa_coin_cost,
            Self::MysteryGift => config.mystery_gift_cost,
        }
    }

    const fn source(self) -> &'static str {
        match self {
            Self::Announce | Self::AnnounceTarget => "shop_announce",
            Self::JopaCoin => "shop_jopa_coin",
            Self::MysteryGift => "shop_mystery_gift",
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::Announce | Self::AnnounceTarget => "shop announcement purchase",
            Self::JopaCoin => "Jopa Coin(TM) purchase",
            Self::MysteryGift => "Mystery Gift purchase",
        }
    }
}

#[derive(Clone, Debug)]
struct ShopChoice {
    name: String,
    value: String,
}

impl ShopChoice {
    fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

struct ProductionShopGuildAiConfig {
    repository: GuildConfigRepository,
}

impl GuildAiConfigPort for ProductionShopGuildAiConfig {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        self.repository
            .get_ai_enabled(guild_id)
            .map_err(|error| error.to_string())
    }
}

struct ProductionShopPlayerContext {
    shop: ShopRuntimeRepository,
    gambling: GamblingStatsRepository,
    loans: LoanRepository,
    player_names: Arc<dyn GuildPlayerNameResolver>,
}

impl ProductionShopPlayerContext {
    fn new(path: impl AsRef<Path>, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        Self {
            shop: ShopRuntimeRepository::new(path.as_ref()),
            gambling: GamblingStatsRepository::new(path.as_ref()),
            loans: LoanRepository::new(path.as_ref()),
            player_names,
        }
    }
}

impl PlayerContextPort for ProductionShopPlayerContext {
    fn player_context(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PlayerContext>, String> {
        let guild_id = guild_id.unwrap_or_default();
        let Some(player) = self
            .shop
            .player(discord_id, guild_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let games = player.games_played();
        let stats = GamblingStatsService::new(self.gambling.clone())
            .get_player_stats(discord_id, Some(guild_id))
            .ok()
            .flatten();
        let bankruptcies = self
            .gambling
            .player_bankruptcy_count(discord_id, Some(guild_id))
            .unwrap_or_default();
        let loan = self
            .loans
            .get_state(discord_id, Some(guild_id))
            .ok()
            .flatten();
        let username = self
            .player_names
            .names_for_guild(guild_id, &[discord_id])
            .unwrap_or_default()
            .resolve_or(discord_id, &player.name);
        Ok(Some(PlayerContext {
            username,
            balance: player.balance,
            debt_amount: (player.balance < 0).then_some(player.balance.saturating_abs()),
            win_rate: (games > 0).then_some(player.wins as f64 / games as f64 * 100.0),
            degen_score: stats.as_ref().map(|stats| stats.degen_score.total),
            bankruptcy_count: bankruptcies,
            total_bets: stats.as_ref().map_or(0, |stats| stats.total_bets),
            total_loans: loan.map_or(0, |state| state.total_loans_taken),
            negative_loans: loan.map_or(0, |state| state.negative_loans_taken),
            total_fees_paid: loan.map_or(0, |state| state.total_fees_paid),
            biggest_win: stats.as_ref().map(|stats| stats.biggest_win),
            biggest_loss: stats
                .as_ref()
                .map(|stats| stats.biggest_loss.saturating_abs()),
            bet_win_rate: stats.as_ref().map(|stats| stats.win_rate * 100.0),
            times_in_debt: bankruptcies,
            lowest_balance: player.lowest_balance,
        }))
    }
}

fn shop_economy_config(config: &ApplicationConfig) -> EconomyEventConfig {
    let values = &config.values;
    EconomyEventConfig {
        enabled: values.economy_events_enabled,
        normal_annual_rate: values.economy_normal_annual_rate,
        inflation_ceiling: values.economy_inflation_ceiling,
        lookback_days: values.economy_event_lookback_days,
        max_reserve_burn_pct: values.economy_event_max_reserve_burn_pct,
        max_wallet_burn_pct: values.economy_event_max_wallet_burn_pct,
        trigger_hour_local: u8::try_from(values.economy_event_trigger_hour_local.clamp(0, 23))
            .unwrap_or(10),
    }
    .normalized()
}

fn shop_subcommands(config: &ShopRuntimeConfig) -> Vec<CommandOptionSpec> {
    vec![
        subcommand_spec(
            "buy",
            "Spend jopacoin in the shop",
            vec![
                CommandOptionSpec::new("item", "What to buy", CommandOptionKind::String)
                    .required(true)
                    .autocomplete(),
                CommandOptionSpec::new(
                    "target",
                    "User to interact with (required for 'Announce + Tag', 'Soft Avoid', and 'Package Deal' options)",
                    CommandOptionKind::User,
                ),
            ],
        ),
        subcommand_spec(
            "pingedash",
            &format!(
                "Spend {} jopacoin to send the Pingedash",
                config.pingedash_cost
            ),
            vec![],
        ),
        subcommand_spec(
            "pingedkevin",
            &format!(
                "Spend {} jopacoin to send the PingedKevin",
                config.pingedkevin_cost
            ),
            vec![],
        ),
        subcommand_spec("avoids", "View your active soft avoids", vec![]),
        subcommand_spec("deals", "View your active package deals", vec![]),
        subcommand_spec(
            "cancel-deal",
            "Cancel a package deal with an inactive player",
            vec![
                CommandOptionSpec::new(
                    "target",
                    "Inactive player whose package deal you want to cancel",
                    CommandOptionKind::User,
                )
                .required(true),
            ],
        ),
        subcommand_spec(
            "mana",
            "Spend mana on color-exclusive items",
            vec![
                CommandOptionSpec {
                    choices: mana_choices(),
                    ..CommandOptionSpec::new(
                        "item",
                        "The mana item to purchase",
                        CommandOptionKind::String,
                    )
                    .required(true)
                },
                CommandOptionSpec::new(
                    "target",
                    "Target player (Sanctuary, Blood Pact, Insight)",
                    CommandOptionKind::User,
                ),
            ],
        ),
    ]
}

fn mana_choices() -> Vec<CommandOptionChoice> {
    const NAMES: &[(&str, &str)] = &[
        (
            "pyroclasm",
            "Red • Cheap • Pyroclasm (25 JC) — burn JC from 3 players, claim a bounty",
        ),
        (
            "dynamite_cache",
            "Red • Mid • Dynamite Cache (35 JC) — next 3 digs +75% yield",
        ),
        (
            "wildfire",
            "Red • Ult • Wildfire (150 JC, taps mana) — drain JC from every positive player",
        ),
        (
            "insight",
            "Blue • Cheap • Insight (10 JC) — peek at any digger's stats",
        ),
        (
            "mana_shield",
            "Blue • Mid • Mana Shield (35 JC) — refund 60% of largest 24h loss",
        ),
        (
            "counterspell",
            "Blue • Ult • Counterspell (75 JC, taps mana) — 24h PvP immunity",
        ),
        (
            "sapling",
            "Green • Cheap • Sapling (10 JC) — shave 45min off /dig cooldown",
        ),
        (
            "regrowth",
            "Green • Mid • Regrowth (25 JC) — recover 35% of last 24h losses",
        ),
        (
            "overgrowth",
            "Green • Ult • Overgrowth (90 JC, taps mana) — 12h dig overdrive",
        ),
        (
            "reprieve",
            "White • Cheap • Reprieve (15 JC) — 50% shield, 25 JC pool, rolling 24h recovery",
        ),
        (
            "aegis",
            "White • Mid • Aegis (35 JC) — fully absorb 75 JC of hostile losses for 24h",
        ),
        (
            "sanctuary",
            "White • Ult • Sanctuary (90 JC, taps mana) — shared 150 JC shield for you + an ally",
        ),
        (
            "soul_harvest",
            "Black • Cheap • Soul Harvest (25 JC) — drain 2 JC (20% chance 3) from each 50+ JC player",
        ),
        (
            "blood_pact",
            "Black • Mid • Blood Pact (75 JC) — skim 25% of target's earnings 24h",
        ),
        (
            "dark_bargain",
            "Black • Ult • Dark Bargain (150 JC, taps mana) — 800 JC now, 700 due later",
        ),
    ];
    NAMES
        .iter()
        .map(|(value, name)| CommandOptionChoice::String {
            name: (*name).to_owned(),
            value: (*value).to_owned(),
        })
        .collect()
}

fn subcommand_spec(
    name: &str,
    description: &str,
    options: Vec<CommandOptionSpec>,
) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn subcommand(
    options: &[InteractionOption],
) -> Result<(String, Vec<InteractionOption>), InteractionHandlerError> {
    let Some(option) = options
        .iter()
        .find(|option| matches!(option.value, InteractionValue::Subcommand(_)))
    else {
        return Err("missing /shop subcommand".into());
    };
    let InteractionValue::Subcommand(children) = &option.value else {
        unreachable!();
    };
    Ok((option.name.clone(), children.clone()))
}

fn required_string(options: &[InteractionOption], name: &str) -> Result<String, String> {
    crate::option_ext::string_option(options, name)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing required string option {name:?}"))
}

/// Shop names prefer the freshly rendered guild name over the payload display
/// name, falling back to the numeric ID (intentional shop-specific behavior).
fn user_option(context: &CommandContext, name: &str) -> Result<Option<UserOption>, String> {
    let Some(user) = crate::option_ext::user_option(&context.options, name)? else {
        return Ok(None);
    };
    let display_name = context
        .rendered_user_names
        .get(&user.id)
        .cloned()
        .unwrap_or_else(|| user.display_name_or_id());
    Ok(Some(UserOption {
        id: user.id,
        unsigned_id: user.raw_id,
        ai_display_name: display_name.clone(),
        display_name,
    }))
}

fn is_admin(context: &CommandContext, config: &ShopRuntimeConfig) -> bool {
    config.admin_user_ids.contains(&context.user_id)
        || context.member_permissions.is_some_and(|permissions| {
            permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
        })
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} id exceeds SQLite range"))
}

fn unix_timestamp() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system clock exceeds SQLite range".to_owned())
}

fn shop_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0xCA5A_5A0F)
}

fn land_for_color(color: &str) -> &'static str {
    match color {
        "Red" => "Mountain",
        "Blue" => "Island",
        "Green" => "Forest",
        "White" => "Plains",
        "Black" => "Swamp",
        _ => "?",
    }
}

#[allow(clippy::too_many_arguments)]
fn hostile_request(
    victim_id: i64,
    guild_id: i64,
    requested: i64,
    kind: &str,
    actor_id: i64,
    event_key: String,
    destination: HostileDestination,
    recipient_id: Option<i64>,
    min_balance: i64,
    occurred_at: i64,
    mana_date: &str,
) -> HostileLossRequest {
    HostileLossRequest {
        victim_id,
        guild_id,
        requested,
        kind: kind.to_owned(),
        actor_id: Some(actor_id),
        event_key,
        destination,
        recipient_id,
        clamp_to_balance: true,
        min_balance: Some(min_balance),
        protect_self: false,
        metadata: json!({
            "kind": kind,
            "attempted_loss": requested,
            "destination": match destination {
                HostileDestination::Burn => "burn",
                HostileDestination::Player => "player",
                HostileDestination::Reserve => "reserve",
            },
            "recipient_id": recipient_id,
        }),
        occurred_at,
        mana_date: mana_date.to_owned(),
    }
}

fn hostile_totals(
    outcomes: &[Result<cama_db::shop_runtime::HostileLossSettlement, String>],
) -> (i64, i64) {
    outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .fold((0, 0), |(applied, absorbed), outcome| {
            (
                applied.saturating_add(outcome.applied),
                absorbed.saturating_add(outcome.absorbed),
            )
        })
}

fn pacific_mana_day(timestamp: i64) -> Result<String, String> {
    use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, Timelike, Weekday};

    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)
        .ok_or_else(|| format!("invalid Unix timestamp {timestamp}"))?;
    let year = utc.year();
    let nth_weekday = |month: u32, weekday: Weekday, occurrence: u32| -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid fixed month");
        let delta = (7 + i64::from(weekday.num_days_from_sunday())
            - i64::from(first.weekday().num_days_from_sunday()))
            % 7;
        first + ChronoDuration::days(delta + 7 * i64::from(occurrence.saturating_sub(1)))
    };
    let start = nth_weekday(3, Weekday::Sun, 2)
        .and_hms_opt(10, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let end = nth_weekday(11, Weekday::Sun, 1)
        .and_hms_opt(9, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let offset = if (start..end).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    };
    let local = DateTime::<Utc>::from_timestamp(timestamp.saturating_add(offset), 0)
        .ok_or_else(|| format!("invalid Pacific timestamp {timestamp}"))?
        .naive_utc();
    let effective = if local.hour() < 4 {
        local - ChronoDuration::days(1)
    } else {
        local
    };
    Ok(effective.format("%Y-%m-%d").to_string())
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), InteractionHandlerError> {
    responder
        .respond(
            InteractionResponse::message(content.into())
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string().into())
}

async fn run_blocking<T, E, F>(label: &'static str, work: F) -> Result<T, InteractionHandlerError>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    crate::ids::blocking(label, move || {
        work().map_err(|error| format!("{label} failed: {error}"))
    })
    .await
    .map_err(InteractionHandlerError::from)
}

fn check_rate_limit(
    rates: &Mutex<BTreeMap<(i64, i64), VecDeque<Instant>>>,
    key: (i64, i64),
    limit: usize,
    period: Duration,
) -> Result<Option<u64>, String> {
    let now = Instant::now();
    let mut rates = rates
        .lock()
        .map_err(|_| "shop rate limiter lock is poisoned".to_owned())?;
    let entries = rates.entry(key).or_default();
    while entries
        .front()
        .is_some_and(|timestamp| now.saturating_duration_since(*timestamp) >= period)
    {
        entries.pop_front();
    }
    if entries.len() >= limit {
        let elapsed =
            now.saturating_duration_since(*entries.front().expect("non-empty rate window"));
        let remaining = period.saturating_sub(elapsed);
        let retry_after = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() != 0))
            .max(1);
        return Ok(Some(retry_after));
    }
    entries.push_back(now);
    Ok(None)
}

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "shop_provider/tests.rs"]
mod tests;
