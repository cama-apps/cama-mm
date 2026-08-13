use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::balance_history_service::BalanceHistoryService;
use cama_app::dedicated_lobby_channel::UserId;
use cama_app::drawing::{
    GambaInfo, GambaPoint, GambaStats, HeroPerformanceEntry, RatingHistoryEntry,
    draw_balance_chart, draw_gamba_chart, draw_hero_performance_chart, draw_lane_distribution,
    draw_rating_history_chart, draw_role_graph,
};
use cama_app::match_discovery::SteamId;
use cama_app::opendota_http::{OpenDotaHttpClient, OpenDotaRuntimeServices};
use cama_app::opendota_player_service::{
    OpenDotaPlayerService, RecordedHeroCatalog, SystemOpenDotaPlayerClock,
};
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::{MatchRepository, PlayerRepository};
use cama_db::gambling_stats_repository::{
    AutoBetGroupStats, AutoBetPerformance, GamblingOutcome, GamblingSource,
    GamblingStatsRepository, GamblingStatsService, PlayerAutoBetStats,
};
use cama_db::loan_repository::LoanRepository;
use cama_db::opendota_player::OpenDotaPlayerRepository;
use cama_db::pairings_repository::{PairingsRepository, PairingsService};
use cama_db::predictions_repository::{CONTRACT_VALUE, PredictionRepository};
use cama_db::rating_history_repository::RatingHistoryRepository;
use cama_db::tip_repository::TipRepository;
use cama_domain::formatting::{JOPACOIN_EMOTE, TOMBSTONE_EMOJI};
use rusqlite::types::Value;
use tracing::warn;

#[path = "profile_balance_history.rs"]
mod profile_balance_history;

use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionAttachment, InteractionButton, InteractionButtonStyle, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionMessageReceipt, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

const DISCORD_BLUE: u32 = 0x34_98_db;
const DISCORD_GREEN: u32 = 0x2e_cc_71;
const DISCORD_ORANGE: u32 = 0xe6_7e_22;
const DISCORD_RED: u32 = 0xe7_4c_3c;
const DISCORD_PURPLE: u32 = 0x9b_59_b6;
const PROFILE_VIEW_TIMEOUT: Duration = Duration::from_secs(840);
const PROFILE_CLICK_WINDOW: Duration = Duration::from_secs(2);
const PROFILE_COMMAND_WINDOW: Duration = Duration::from_secs(30);
const PROFILE_COMMAND_LIMIT: usize = 5;
const PROFILE_COMPONENT_PREFIX: &str = "profile:";

type LiveDotaProfileService = OpenDotaPlayerService<
    OpenDotaPlayerRepository,
    OpenDotaHttpClient,
    SystemOpenDotaPlayerClock,
    RecordedHeroCatalog,
>;
type ProfileChartJob<'a> = Box<dyn FnOnce() -> Vec<u8> + Send + 'a>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum ProfileTab {
    Overview,
    Rating,
    Economy,
    Gambling,
    Predictions,
    Dota,
    Teammates,
    Heroes,
}

#[derive(Clone)]
pub struct ProfileRegistrationProvider {
    handler: Arc<ProfileHandler>,
}

impl ProfileRegistrationProvider {
    /// Compose the live command from the existing SQLite adapters and the
    /// shared production OpenDota transport retained by `ServiceContainer`.
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, opendota: Arc<OpenDotaRuntimeServices>) -> Self {
        Self::with_timeout(database_path, opendota, PROFILE_VIEW_TIMEOUT)
    }

    fn with_timeout(
        database_path: impl AsRef<Path>,
        opendota: Arc<OpenDotaRuntimeServices>,
        view_timeout: Duration,
    ) -> Self {
        Self {
            handler: Arc::new(ProfileHandler {
                sources: ProfileDataSources::new(database_path, opendota),
                command_rate_limit: Mutex::new(CommandRateLimit::default()),
                views: Arc::new(Mutex::new(BTreeMap::new())),
                next_view_id: AtomicU64::new(1),
                view_timeout,
            }),
        }
    }
}

impl RegistrationProvider for ProfileRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "profile".to_owned(),
            description: "View unified player profile with tabbed stats".to_owned(),
            options: vec![CommandOptionSpec::new(
                "user",
                "Player to view profile for (defaults to yourself)",
                CommandOptionKind::User,
            )],
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: PROFILE_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct ProfileHandler {
    sources: ProfileDataSources,
    command_rate_limit: Mutex<CommandRateLimit>,
    views: Arc<Mutex<BTreeMap<u64, Arc<tokio::sync::Mutex<ProfileViewState>>>>>,
    next_view_id: AtomicU64,
    view_timeout: Duration,
}

#[derive(Debug)]
struct ProfileViewState {
    target_id: i64,
    guild_id: Option<i64>,
    target_name: String,
    current_tab: ProfileTab,
    last_click: BTreeMap<u64, Instant>,
    cache: BTreeMap<ProfileTab, ProfilePage>,
}

#[derive(Debug, Default)]
struct CommandRateLimit {
    hits: BTreeMap<(u64, u64), VecDeque<Instant>>,
}

impl CommandRateLimit {
    fn retry_after_seconds(&mut self, guild_id: Option<u64>, user_id: u64) -> Option<u64> {
        let now = Instant::now();
        let key = (guild_id.unwrap_or_default(), user_id);
        let hits = self.hits.entry(key).or_default();
        while hits
            .front()
            .is_some_and(|started| now.duration_since(*started) >= PROFILE_COMMAND_WINDOW)
        {
            hits.pop_front();
        }
        if hits.len() < PROFILE_COMMAND_LIMIT {
            hits.push_back(now);
            return None;
        }
        hits.front().map(|started| {
            let remaining = PROFILE_COMMAND_WINDOW.saturating_sub(now.duration_since(*started));
            remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() != 0))
                .max(1)
        })
    }
}

#[async_trait]
impl InteractionHandler for ProfileHandler {
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
            InteractionRequest::Component { .. } => self
                .handle_component(request, responder)
                .await
                .map_err(Into::into),
            InteractionRequest::Modal { .. } => {
                Err("profile handler received a modal interaction".into())
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("profile handler received an autocomplete interaction".into())
            }
        }
    }
}

impl ProfileHandler {
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
            options,
            ..
        } = request
        else {
            return Err("profile command handler received a component".to_owned());
        };
        if name != "profile" {
            return Err(format!("profile handler received command {name:?}"));
        }
        let retry_after = self
            .command_rate_limit
            .lock()
            .map_err(|_| "profile rate-limit lock was poisoned".to_owned())?
            .retry_after_seconds(guild_id, user_id);
        if let Some(retry_after) = retry_after {
            return responder
                .respond(
                    InteractionResponse::message(format!(
                        "⏳ Please wait {retry_after}s before using `/profile` again."
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        }
        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer profile interaction");
            return Ok(());
        }

        let (target_id, requested_name) = user_option(&options).map_or_else(
            || (user_id, Some(user_display_name)),
            |(id, display_name)| (id, display_name),
        );
        let target_id = signed_discord_id(target_id, "target user")?;
        let guild_id = signed_optional_discord_id(guild_id, "guild")?;
        let sources = self.sources.clone();
        let registered_name =
            tokio::task::spawn_blocking(move || sources.registered_name(target_id, guild_id))
                .await
                .map_err(|error| format!("profile player lookup task failed: {error}"))??;
        let Some(database_name) = registered_name else {
            let target_name = requested_name.unwrap_or_else(|| format!("User {target_id}"));
            return responder
                .followup(InteractionResponse::message(format!(
                    "❌ {target_name} is not registered. Use `/player register` to get started."
                )))
                .await
                .map_err(|error| error.to_string());
        };
        let target_name = requested_name.unwrap_or(database_name);
        let sources = self.sources.clone();
        let build_name = target_name.clone();
        let page = tokio::task::spawn_blocking(move || {
            sources.build_page(ProfileTab::Overview, target_id, guild_id, &build_name)
        })
        .await
        .map_err(|error| format!("profile overview task failed: {error}"))??;

        let view_id = self.next_view_id.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(tokio::sync::Mutex::new(ProfileViewState {
            target_id,
            guild_id,
            target_name,
            current_tab: ProfileTab::Overview,
            last_click: BTreeMap::new(),
            cache: BTreeMap::new(),
        }));
        self.views
            .lock()
            .map_err(|_| "profile view registry lock was poisoned".to_owned())?
            .insert(view_id, state);
        let response = profile_response(page, view_id, ProfileTab::Overview);
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        self.schedule_expiry(view_id, receipt, responder);
        Ok(())
    }

    async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id, user_id, ..
        } = request
        else {
            return Err("profile component handler received a command".to_owned());
        };
        let (view_id, tab) = parse_component_id(&custom_id)?;
        let state = self
            .views
            .lock()
            .map_err(|_| "profile view registry lock was poisoned".to_owned())?
            .get(&view_id)
            .cloned();
        let Some(state) = state else {
            return responder
                .respond(
                    InteractionResponse::message(
                        "This profile view has expired. Run `/profile` again.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
        };

        let mut state = state.lock().await;
        let now = Instant::now();
        if state
            .last_click
            .get(&user_id)
            .is_some_and(|last| now.duration_since(*last) < PROFILE_CLICK_WINDOW)
        {
            return responder
                .defer(false)
                .await
                .map_err(|error| error.to_string());
        }
        state.last_click.insert(user_id, now);
        let cached = state.cache.get(&tab).cloned();
        let deferred = tab.expensive() && cached.is_none();
        if deferred && let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer profile component interaction");
            return Ok(());
        }
        let page = if let Some(cached) = cached {
            cached
        } else {
            let sources = self.sources.clone();
            let target_id = state.target_id;
            let guild_id = state.guild_id;
            let target_name = state.target_name.clone();
            let page = tokio::task::spawn_blocking(move || {
                sources.build_page(tab, target_id, guild_id, &target_name)
            })
            .await
            .map_err(|error| format!("profile {tab:?} task failed: {error}"))??;
            if tab.expensive() {
                state.cache.insert(tab, page.clone());
            }
            page
        };
        state.current_tab = tab;
        drop(state);

        let response = profile_response(page, view_id, tab);
        if deferred {
            responder.edit_original(response).await
        } else {
            responder.update(response).await
        }
        .map_err(|error| error.to_string())
    }

    fn schedule_expiry(
        &self,
        view_id: u64,
        receipt: Option<InteractionMessageReceipt>,
        responder: Arc<dyn InteractionResponder>,
    ) {
        let views = Arc::clone(&self.views);
        let timeout = self.view_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if let Ok(mut views) = views.lock() {
                views.remove(&view_id);
            }
            if let Some(receipt) = receipt
                && let Err(error) = responder.delete_message(receipt).await
            {
                warn!(%error, view_id, "failed to delete expired profile message");
            }
        });
    }
}

fn user_option(options: &[InteractionOption]) -> Option<(u64, Option<String>)> {
    options.iter().find_map(|option| {
        if option.name != "user" {
            return None;
        }
        match &option.value {
            InteractionValue::User {
                id, display_name, ..
            } => Some((*id, display_name.clone())),
            _ => None,
        }
    })
}

fn signed_discord_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("Discord {label} id exceeds SQLite's signed range"))
}

fn signed_optional_discord_id(value: Option<u64>, label: &str) -> Result<Option<i64>, String> {
    value
        .map(|value| signed_discord_id(value, label))
        .transpose()
}

fn parse_component_id(custom_id: &str) -> Result<(u64, ProfileTab), String> {
    let mut parts = custom_id.split(':');
    let Some("profile") = parts.next() else {
        return Err(format!("invalid profile component id {custom_id:?}"));
    };
    let view_id = parts
        .next()
        .ok_or_else(|| format!("invalid profile component id {custom_id:?}"))?
        .parse()
        .map_err(|_| format!("invalid profile view id in {custom_id:?}"))?;
    let tab = parts
        .next()
        .and_then(ProfileTab::parse)
        .ok_or_else(|| format!("invalid profile tab in {custom_id:?}"))?;
    if parts.next().is_some() {
        return Err(format!("invalid profile component id {custom_id:?}"));
    }
    Ok((view_id, tab))
}

fn profile_response(page: ProfilePage, view_id: u64, active: ProfileTab) -> InteractionResponse {
    let mut embed = InteractionEmbed::titled(page.title).color(page.color);
    if let Some(description) = page.description {
        embed = embed.description(description);
    }
    for field in page.fields {
        embed = embed.field(field.name, field.value, field.inline);
    }
    if let Some(footer) = page.footer {
        embed = embed.footer(footer);
    }
    if let Some(image_url) = page.image_url {
        embed = embed.image(image_url);
    }
    let mut response = InteractionResponse::message("")
        .embed(embed)
        .action_rows(profile_action_rows(view_id, active));
    for attachment in page.attachments {
        response = response.attachment(attachment);
    }
    response
}

fn profile_action_rows(view_id: u64, active: ProfileTab) -> Vec<InteractionActionRow> {
    [0_u8, 1]
        .into_iter()
        .map(|row| {
            InteractionActionRow::buttons(
                ProfileTab::ALL
                    .into_iter()
                    .filter(|tab| tab.row() == row)
                    .map(|tab| {
                        InteractionButton::new(
                            format!("profile:{view_id}:{}", tab.key()),
                            tab.label(),
                        )
                        .style(if tab == active {
                            InteractionButtonStyle::Primary
                        } else {
                            InteractionButtonStyle::Secondary
                        })
                    })
                    .collect(),
            )
        })
        .collect()
}

impl ProfileTab {
    const ALL: [Self; 8] = [
        Self::Overview,
        Self::Rating,
        Self::Economy,
        Self::Gambling,
        Self::Predictions,
        Self::Dota,
        Self::Teammates,
        Self::Heroes,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Rating => "rating",
            Self::Economy => "economy",
            Self::Gambling => "gambling",
            Self::Predictions => "predictions",
            Self::Dota => "dota",
            Self::Teammates => "teammates",
            Self::Heroes => "heroes",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Rating => "Rating",
            Self::Economy => "Economy",
            Self::Gambling => "Gambling",
            Self::Predictions => "Predictions",
            Self::Dota => "Dota",
            Self::Teammates => "Teammates",
            Self::Heroes => "Heroes",
        }
    }

    const fn row(self) -> u8 {
        match self {
            Self::Overview | Self::Rating | Self::Economy | Self::Gambling | Self::Predictions => 0,
            Self::Dota | Self::Teammates | Self::Heroes => 1,
        }
    }

    const fn expensive(self) -> bool {
        matches!(
            self,
            Self::Dota
                | Self::Gambling
                | Self::Heroes
                | Self::Economy
                | Self::Rating
                | Self::Teammates
        )
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.key() == value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProfileField {
    name: String,
    value: String,
    inline: bool,
}

impl ProfileField {
    fn new(name: impl Into<String>, value: impl Into<String>, inline: bool) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            inline,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProfilePage {
    title: String,
    description: Option<String>,
    color: u32,
    fields: Vec<ProfileField>,
    footer: Option<String>,
    image_url: Option<String>,
    attachments: Vec<InteractionAttachment>,
}

impl ProfilePage {
    fn new(title: impl Into<String>, color: u32) -> Self {
        Self {
            title: title.into(),
            description: None,
            color,
            fields: Vec::new(),
            footer: None,
            image_url: None,
            attachments: Vec::new(),
        }
    }

    fn description(mut self, value: impl Into<String>) -> Self {
        self.description = Some(value.into());
        self
    }

    fn field(mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) -> Self {
        self.fields.push(ProfileField::new(name, value, inline));
        self
    }

    fn footer(mut self, value: impl Into<String>) -> Self {
        self.footer = Some(value.into());
        self
    }

    fn image_attachment(mut self, filename: impl Into<String>, bytes: Vec<u8>) -> Self {
        let filename = filename.into();
        self.image_url = Some(format!("attachment://{filename}"));
        self = self.attachment(filename, bytes);
        self
    }

    fn attachment(mut self, filename: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.attachments
            .push(InteractionAttachment::bytes(filename, bytes));
        self
    }
}

#[derive(Clone)]
struct ProfileDataSources {
    player: PlayerRepository,
    steam: OpenDotaPlayerRepository,
    matches: MatchRepository,
    bankruptcy: BankruptcyRepository,
    loans: LoanRepository,
    gambling: GamblingStatsService<GamblingStatsRepository>,
    predictions: PredictionRepository,
    pairings: PairingsService<PairingsRepository>,
    rating_history: RatingHistoryRepository,
    balance_history: BalanceHistoryService<profile_balance_history::SqliteBalanceHistoryPort>,
    tips: TipRepository,
    dota: Arc<LiveDotaProfileService>,
    opendota: Arc<OpenDotaRuntimeServices>,
}

impl ProfileDataSources {
    fn new(database_path: impl AsRef<Path>, opendota: Arc<OpenDotaRuntimeServices>) -> Self {
        let database_path = database_path.as_ref();
        Self {
            player: PlayerRepository::new(database_path),
            steam: OpenDotaPlayerRepository::new(database_path),
            matches: MatchRepository::new(database_path),
            bankruptcy: BankruptcyRepository::new(database_path),
            loans: LoanRepository::new(database_path),
            gambling: GamblingStatsService::new(GamblingStatsRepository::new(database_path)),
            predictions: PredictionRepository::new(database_path),
            pairings: PairingsService::new(PairingsRepository::new(database_path)),
            rating_history: RatingHistoryRepository::new(database_path),
            balance_history: BalanceHistoryService::new(
                profile_balance_history::SqliteBalanceHistoryPort::new(database_path),
            ),
            tips: TipRepository::new(database_path),
            dota: Arc::new(opendota.profile_player_service(database_path)),
            opendota,
        }
    }

    fn registered_name(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, String> {
        self.player
            .get_by_id(discord_id, guild_id)
            .map(|player| player.map(|player| player.name))
            .map_err(|error| error.to_string())
    }

    fn build_page(
        &self,
        tab: ProfileTab,
        target_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        match tab {
            ProfileTab::Overview => self.overview(target_id, guild_id, target_name),
            ProfileTab::Rating => self.rating(target_id, guild_id, target_name),
            ProfileTab::Economy => self.economy(target_id, guild_id, target_name),
            ProfileTab::Gambling => self.gambling(target_id, guild_id, target_name),
            ProfileTab::Predictions => self.predictions(target_id, guild_id, target_name),
            ProfileTab::Dota => self.dota(target_id, guild_id, target_name),
            ProfileTab::Teammates => self.teammates(target_id, guild_id, target_name),
            ProfileTab::Heroes => self.heroes(target_id, guild_id, target_name),
        }
    }

    fn player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<cama_domain::player::Player, String> {
        self.player
            .get_by_id(discord_id, guild_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("player {discord_id} is not registered"))
    }

    fn overview(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        let player = self.player(discord_id, guild_id)?;
        let total_games = player.wins.saturating_add(player.losses);
        let record = if total_games == 0 {
            "No games".to_owned()
        } else {
            format!(
                "{}W-{}L ({:.0}%)",
                player.wins,
                player.losses,
                player.wins as f64 / total_games as f64 * 100.0
            )
        };
        let glicko = player.glicko_rating.map_or_else(
            || "**Glicko:** Not set".to_owned(),
            |rating| {
                let certainty = player
                    .glicko_rd
                    .map_or(0.0, |rd| (100.0 - rd / 350.0 * 100.0).clamp(0.0, 100.0));
                format!("**Glicko:** {rating:.0} ({certainty:.0}% certain)")
            },
        );
        let openskill = player.os_mu.map_or_else(
            || "**OpenSkill:** Not set".to_owned(),
            |mu| {
                let display = ((mu - 25.0) * 50.0).max(0.0).round();
                let certainty = player.os_sigma.map_or(0.0, |sigma| {
                    (100.0 - sigma / 25.0 * 3.0 * 100.0).clamp(0.0, 100.0)
                });
                format!("**OpenSkill:** {display:.0} ({certainty:.0}% certain)")
            },
        );
        let roles = player.preferred_roles.as_ref().map_or_else(
            || "Not set".to_owned(),
            |roles| {
                roles
                    .iter()
                    .map(|role| role_name(role))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
        let region = player
            .preferred_region
            .as_deref()
            .or(player.inferred_region.as_deref())
            .map_or_else(|| "Not set".to_owned(), region_name);
        let mut page = ProfilePage::new(format!("Profile: {target_name}"), DISCORD_BLUE)
            .field("Rating", format!("{glicko}\n{openskill}"), true)
            .field("Record", record, true)
            .field(
                "Balance",
                format!("{} Jopacoin", player.jopacoin_balance),
                true,
            )
            .field("Roles", roles, true)
            .field("Server", region, true);
        if let Some(main_role) = &player.main_role {
            page = page.field("Main", role_name(main_role), true);
        }
        if let Ok(Some(state)) = self.bankruptcy.get_state(discord_id, guild_id)
            && state.penalty_games_remaining > 0
        {
            page = page.field(
                "🪦 Bankruptcy Penalty",
                format!("{} game(s) remaining", state.penalty_games_remaining),
                false,
            );
        }
        Ok(page)
    }

    fn rating(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        let player = self.player(discord_id, guild_id)?;
        let glicko = player.glicko_rating.map_or_else(
            || "Not set".to_owned(),
            |rating| {
                format!(
                    "**Rating:** {rating:.0}\n**RD:** {}\n**Volatility:** {}",
                    optional_decimal(player.glicko_rd),
                    optional_decimal(player.glicko_volatility)
                )
            },
        );
        let openskill = player.os_mu.map_or_else(
            || "Not set".to_owned(),
            |mu| {
                format!(
                    "**Display:** {:.0}\n**μ:** {mu:.2}\n**σ:** {}",
                    ((mu - 25.0) * 50.0).max(0.0),
                    optional_decimal(player.os_sigma)
                )
            },
        );
        let history = self
            .rating_history
            .get_rating_chart_history(discord_id, guild_id, 999)
            .map_err(|error| error.to_string())?;
        let chart = (history.len() >= 2).then(|| {
            let history = history
                .into_iter()
                .map(|entry| RatingHistoryEntry {
                    rating: entry.rating,
                    openskill_mu: entry.openskill_mu,
                    won: entry.won,
                })
                .collect::<Vec<_>>();
            draw_rating_history_chart(target_name, &history).into_inner()
        });
        let mut page = ProfilePage::new(format!("Profile: {target_name} > Rating"), DISCORD_BLUE)
            .field("Glicko", glicko, true)
            .field("OpenSkill", openskill, true)
            .field(
                "Matchmaking MMR",
                player
                    .mmr
                    .map_or_else(|| "Not set".to_owned(), |mmr| mmr.to_string()),
                true,
            );
        if let Some(chart) = chart {
            page = page.image_attachment("rating_chart.png", chart);
        }
        Ok(page)
    }

    fn economy(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        let Some(player) = self
            .player
            .get_by_id(discord_id, guild_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(ProfilePage::new("Not Registered", DISCORD_RED)
                .description(format!("{target_name} is not registered.")));
        };
        let loan = self
            .loans
            .get_state(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let bankruptcy = self
            .bankruptcy
            .get_state(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let loan_text = loan.map_or_else(
            || "No loan history".to_owned(),
            |state| {
                let mut lines = vec![
                    format!("**Loans Taken:** {}", state.total_loans_taken),
                    format!("**Fees Paid:** {} {JOPACOIN_EMOTE}", state.total_fees_paid),
                ];
                if state.negative_loans_taken > 0 {
                    lines.push(format!(
                        "🔥 **Borrowed While Broke:** {}x",
                        state.negative_loans_taken
                    ));
                }
                if state.has_outstanding_loan() {
                    lines.push("\n⚠️ **Outstanding Loan:**".to_owned());
                    lines.push(format!(
                        "  Principal: {} {JOPACOIN_EMOTE}",
                        state.outstanding_principal
                    ));
                    lines.push(format!("  Fee: {} {JOPACOIN_EMOTE}", state.outstanding_fee));
                    if let Some(total) = state.outstanding_total() {
                        lines.push(format!("  **Total Owed:** {total} {JOPACOIN_EMOTE}"));
                    }
                    lines.push("  *(Repaid on next match)*".to_owned());
                } else {
                    lines.push("\n✅ **Loan Available**".to_owned());
                }
                lines.join("\n")
            },
        );
        let bankruptcy_text = bankruptcy.map_or_else(
            || "**Declarations:** 0".to_owned(),
            |state| {
                format!(
                    "**Declarations:** {}\n**Penalty games:** {}",
                    state.bankruptcy_count, state.penalty_games_remaining
                )
            },
        );
        let lowest_balance = self
            .player
            .get_lowest_balance(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let tips = self
            .tips
            .get_user_tip_stats(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let history = self
            .balance_history
            .get_balance_event_series(discord_id, MatchRepository::normalize_guild_id(guild_id));
        let (balance_emoji, color) = match player.jopacoin_balance.cmp(&0) {
            std::cmp::Ordering::Greater => ("💰", DISCORD_GREEN),
            std::cmp::Ordering::Less => ("⚠️", DISCORD_RED),
            std::cmp::Ordering::Equal => ("📭", DISCORD_ORANGE),
        };
        let mut page = ProfilePage::new(format!("Profile: {target_name} > Economy"), color)
            .field(
                format!("{balance_emoji} Balance"),
                format!("**{}** {JOPACOIN_EMOTE}", player.jopacoin_balance),
                false,
            )
            .field("🏦 Loans", loan_text, true)
            .field(
                format!("{TOMBSTONE_EMOJI} Bankruptcy"),
                bankruptcy_text,
                true,
            );
        if let Some(lowest_balance) = lowest_balance.filter(|balance| *balance < 0) {
            page = page.field(
                "📉 Lowest Balance",
                format!("**{lowest_balance}** {JOPACOIN_EMOTE}"),
                true,
            );
        }
        if tips.tips_sent_count > 0 || tips.tips_received_count > 0 {
            let mut tip_lines = Vec::new();
            if tips.tips_sent_count > 0 {
                tip_lines.push(format!(
                    "**Sent:** {} {JOPACOIN_EMOTE} ({} tips)",
                    tips.total_sent, tips.tips_sent_count
                ));
            }
            if tips.tips_received_count > 0 {
                tip_lines.push(format!(
                    "**Received:** {} {JOPACOIN_EMOTE} ({} tips)",
                    tips.total_received, tips.tips_received_count
                ));
            }
            if tips.fees_paid > 0 {
                tip_lines.push(format!(
                    "**Fees Paid:** {} {JOPACOIN_EMOTE}",
                    tips.fees_paid
                ));
            }
            page = page.field("💝 Tipping", tip_lines.join("\n"), true);
        }
        if history.series.len() >= 2 {
            let chart_points = history.chart_points();
            let chart = draw_balance_chart(target_name, &chart_points, &history.chart_totals())
                .into_inner();
            page = page
                .field(
                    "📊 Balance History",
                    format_balance_history_breakdown(&history.chart_totals()),
                    false,
                )
                .image_attachment("balance_history.png", chart);
        }
        Ok(page.footer("Tip: Use /balance for a quick check, /economy loan to borrow"))
    }

    fn gambling(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        let player = self.player(discord_id, guild_id)?;
        let Some(stats) = self
            .gambling
            .get_player_stats(discord_id, guild_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(ProfilePage::new(
                format!("Profile: {target_name} > Gambling"),
                DISCORD_BLUE,
            )
            .description(
                "No betting history yet.\n\nPlay matches and use `/bet` to get started!",
            ));
        };
        let chart = match self.gambling.cumulative_pnl_series(discord_id, guild_id) {
            Ok(series) if series.len() >= 2 => {
                let series = series
                    .into_iter()
                    .map(|point| {
                        let event = point.event;
                        GambaPoint {
                            event_number: i32::try_from(point.event_number).unwrap_or(i32::MAX),
                            cumulative: point.cumulative_pnl,
                            info: GambaInfo {
                                source: match event.source {
                                    GamblingSource::Bet => "bet",
                                    GamblingSource::Wheel => "wheel",
                                    GamblingSource::DoubleOrNothing => "double_or_nothing",
                                }
                                .to_owned(),
                                outcome: Some(
                                    match event.outcome {
                                        GamblingOutcome::Won => "won",
                                        GamblingOutcome::Lost => "lost",
                                        GamblingOutcome::Neutral => "neutral",
                                    }
                                    .to_owned(),
                                ),
                                leverage: event.leverage,
                                profit: event.profit,
                            },
                        }
                    })
                    .collect::<Vec<_>>();
                Some(
                    draw_gamba_chart(
                        target_name,
                        i32::try_from(stats.degen_score.total).unwrap_or(i32::MAX),
                        stats.degen_score.title,
                        &series,
                        GambaStats {
                            total_bets: usize::try_from(stats.total_bets).unwrap_or(usize::MAX),
                            win_rate: stats.win_rate,
                            net_pnl: stats.net_pnl,
                            roi: stats.roi,
                        },
                    )
                    .into_inner(),
                )
            }
            Ok(_) => None,
            Err(error) => {
                warn!(%error, discord_id, ?guild_id, "unable to render profile gambling chart");
                None
            }
        };
        let pnl = if stats.net_pnl >= 0 {
            format!("+{}", stats.net_pnl)
        } else {
            stats.net_pnl.to_string()
        };
        let streak = if stats.current_streak >= 0 {
            format!("W{}", stats.current_streak)
        } else {
            format!("L{}", stats.current_streak.unsigned_abs())
        };
        let streak_emoji = if stats.current_streak > 0 {
            "🔥"
        } else if stats.current_streak < 0 {
            "💀"
        } else {
            "➖"
        };
        let leverage = [1_i64, 2, 3, 5]
            .into_iter()
            .filter_map(|level| {
                let count = *stats.leverage_distribution.get(&level).unwrap_or(&0);
                (count > 0).then(|| {
                    format!(
                        "{level}×({:.0}%)",
                        count as f64 / stats.total_bets as f64 * 100.0
                    )
                })
            })
            .collect::<Vec<_>>();
        let roi = if stats.roi >= 0.0 {
            format!("+{:.1}%", stats.roi * 100.0)
        } else {
            format!("{:.1}%", stats.roi * 100.0)
        };
        let flavor_text = if stats.degen_score.flavor_texts.is_empty() {
            stats.degen_score.tagline.to_owned()
        } else {
            stats.degen_score.flavor_texts.join(" • ")
        };
        let mut page = ProfilePage::new(
            format!("Profile: {target_name} > Gambling"),
            if stats.net_pnl >= 0 {
                DISCORD_GREEN
            } else {
                DISCORD_RED
            },
        )
        .description(format!(
            "**Degen Score: {}** {} {}\n*{}*",
            stats.degen_score.total,
            stats.degen_score.emoji,
            stats.degen_score.title,
            flavor_text
        ))
        .field(
            "Performance",
            format!(
                "**Balance:** {} {JOPACOIN_EMOTE}\n**Net P&L:** {pnl} {JOPACOIN_EMOTE}\n**ROI:** {roi}\n**Record:** {}W-{}L ({:.0}%)",
                player.jopacoin_balance,
                stats.wins,
                stats.losses,
                stats.win_rate * 100.0
            ),
            true,
        )
        .field(
            "Volume",
            format!(
                "**Total Bets:** {}\n**Wagered:** {} {JOPACOIN_EMOTE}\n**Avg Bet:** {:.1} {JOPACOIN_EMOTE}",
                stats.total_bets, stats.total_wagered, stats.avg_bet_size
            ),
            true,
        )
        .field(
            "Risk Profile",
            format!(
                "**Leverage:** {}\n**Streak:** {streak_emoji} {streak}\n**Best/Worst:** W{} / L{}",
                if leverage.is_empty() {
                    "None".to_owned()
                } else {
                    leverage.join(" ")
                },
                stats.best_streak,
                stats.worst_streak.unsigned_abs()
            ),
            true,
        );
        page = page.field(
            "Extremes",
            format!(
                "**Peak:** {} {JOPACOIN_EMOTE}\n**Trough:** {} {JOPACOIN_EMOTE}\n**Best Win:** {} {JOPACOIN_EMOTE}\n**Worst Loss:** {} {JOPACOIN_EMOTE}",
                if stats.peak_pnl > 0 {
                    format!("+{}", stats.peak_pnl)
                } else {
                    stats.peak_pnl.to_string()
                },
                stats.trough_pnl,
                if stats.biggest_win > 0 {
                    format!("+{}", stats.biggest_win)
                } else {
                    "None".to_owned()
                },
                if stats.biggest_loss < 0 {
                    stats.biggest_loss.to_string()
                } else {
                    "None".to_owned()
                }
            ),
            true,
        );
        if stats.matches_played > 0 {
            let paper_rate = stats.paper_hands_count as f64 / stats.matches_played as f64 * 100.0;
            let paper_emoji = if paper_rate >= 30.0 {
                "📄"
            } else if paper_rate >= 10.0 {
                "🤔"
            } else {
                "💎"
            };
            page = page.field(
                format!("{paper_emoji} Paper Hands"),
                format!(
                    "**Played:** {} matches\n**No Self-Bet:** {} ({paper_rate:.0}%)",
                    stats.matches_played, stats.paper_hands_count
                ),
                true,
            );
        }
        let degen = &stats.degen_score;
        let mut breakdown = Vec::new();
        if degen.max_leverage_score > 0 {
            breakdown.push(format!("5x Addiction: {}/25", degen.max_leverage_score));
        }
        if degen.bet_size_score > 0 {
            breakdown.push(format!("Bet Size: {}/25", degen.bet_size_score));
        }
        if degen.debt_depth_score > 0 {
            breakdown.push(format!("Debt Depth: {}/20", degen.debt_depth_score));
        }
        if degen.bankruptcy_score > 0 {
            breakdown.push(format!("Bankruptcies: {}/15", degen.bankruptcy_score));
        }
        if degen.frequency_score > 0 {
            breakdown.push(format!("Frequency: {}/10", degen.frequency_score));
        }
        if degen.loss_chase_score > 0 {
            breakdown.push(format!("Loss Chasing: {}/5", degen.loss_chase_score));
        }
        if degen.negative_loan_bonus > 0 {
            breakdown.push(format!("🔥 Negative Loans: +{}", degen.negative_loan_bonus));
        }
        if !breakdown.is_empty() {
            page = page.field("Degen Breakdown", breakdown.join("\n"), true);
        }
        if let Some(chart) = chart {
            page = page.image_attachment("gamba_chart.png", chart);
        }
        append_auto_bet_field(&mut page, &stats.auto_bet_performance);
        Ok(page)
    }

    fn predictions(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        self.player(discord_id, guild_id)?;
        let stats = self
            .predictions
            .user_orderbook_stats(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let positions = self
            .predictions
            .get_user_open_position_summaries(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        if positions.is_empty() && stats.resolved_markets == 0 {
            return Ok(ProfilePage::new(
                format!("Profile: {target_name} > Predictions"),
                DISCORD_BLUE,
            )
            .description(
                "No prediction market activity yet.\n\nUse `/predict list` to find active markets!",
            ));
        }
        let mut position_lines = Vec::new();
        for marked in positions.iter().take(5) {
            let position = marked.marked.position;
            let question = truncate_question(&marked.question, 40);
            if position.yes_contracts != 0 {
                let average = position.yes_cost_basis_total as f64 * 100.0
                    / (position.yes_contracts * CONTRACT_VALUE) as f64;
                position_lines.push(format!(
                    "**#{}** {question}\n   YES {} @ {average:.0}%",
                    position.prediction_id, position.yes_contracts
                ));
            }
            if position.no_contracts != 0 {
                let average = position.no_cost_basis_total as f64 * 100.0
                    / (position.no_contracts * CONTRACT_VALUE) as f64;
                position_lines.push(format!(
                    "**#{}** {question}\n   NO {} @ {average:.0}%",
                    position.prediction_id, position.no_contracts
                ));
            }
        }
        if positions.len() > 5 {
            position_lines.push(format!("*+{} more*", positions.len() - 5));
        }
        let realized = if stats.realized_pnl >= 0 {
            format!("+{}", stats.realized_pnl)
        } else {
            stats.realized_pnl.to_string()
        };
        let mut page = ProfilePage::new(
            format!("Profile: {target_name} > Predictions"),
            if stats.realized_pnl >= 0 {
                DISCORD_GREEN
            } else {
                DISCORD_RED
            },
        )
        .field(
            "Performance",
            format!(
                "**Realized P&L:** {realized} Jopacoin\n**Record:** {}W-{}L ({} resolved)",
                stats.wins, stats.losses, stats.resolved_markets
            ),
            false,
        )
        .footer("Tip: Use /predict mine for live position values");
        if positions.is_empty() {
            page = page.field("Open Positions", "No open positions", false);
        } else {
            page = page.field(
                format!("Open Positions ({})", positions.len()),
                position_lines.join("\n"),
                false,
            );
        }
        Ok(page)
    }

    fn dota(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        self.player(discord_id, guild_id)?;
        let Some(steam_id) = self
            .steam
            .get_steam_id(discord_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(ProfilePage::new(
                format!("Profile: {target_name} > Dota Stats"),
                DISCORD_ORANGE,
            )
            .description(
                "No Steam account linked.\n\nUse `/player link` to link your Steam ID, or `/player register` if you're new.",
            ));
        };
        let stats = self
            .dota
            .get_dota_tab_stats(UserId(discord_id), 50, Some(SteamId(steam_id)))
            .map_err(|error| error.to_string())?;
        let mut page =
            ProfilePage::new(format!("Profile: {target_name} > Dota Stats"), DISCORD_BLUE)
                .footer("Data from OpenDota | Based on last 50 matches");
        let full = stats.full_stats.as_ref();
        let lanes = full
            .map(|stats| ordered_positive_lanes(&stats.lane_distribution))
            .unwrap_or_default();
        let role_title = format!("Roles: {target_name}");
        let role_job: Option<ProfileChartJob<'_>> = stats.role_distribution.as_ref().map(|roles| {
            Box::new(|| draw_role_graph(roles, &role_title).into_inner()) as ProfileChartJob<'_>
        });
        let lane_job: Option<ProfileChartJob<'_>> = (!lanes.is_empty()).then(|| {
            Box::new(|| draw_lane_distribution(&lanes).into_inner()) as ProfileChartJob<'_>
        });
        let (role_chart, lane_chart) = render_independent_dota_charts(role_job, lane_job);

        if let (Some(roles), Some(role_chart)) = (&stats.role_distribution, role_chart) {
            page = page.field("Role Distribution", distribution_lines(roles), true);
            page = page.image_attachment("role_graph.png", role_chart);
        }
        let Some(full) = full else {
            return Ok(page.description("Could not fetch OpenDota stats. Try again later."));
        };
        if let Some(lane_chart) = lane_chart {
            page = page.field(
                "Lane Distribution",
                format!(
                    "Based on {} parsed matches\n*(see lane_graph.png below)*",
                    full.lane_parsed_count,
                ),
                true,
            );
            // Python only sets the embed image for the role graph. The lane
            // chart remains an additional attachment when role metadata is
            // unavailable, preserving that established Discord contract.
            page = page.attachment("lane_graph.png", lane_chart);
        }
        page = page
            .field(
                "OpenDota Win Rate",
                format!("{:.1}% (last 50)", full.total_win_rate),
                true,
            )
            .field(
                "Avg KDA",
                format!(
                    "{:.1} / {:.1} / {:.1}",
                    full.averages.kills, full.averages.deaths, full.averages.assists
                ),
                true,
            );
        if !full.top_heroes.is_empty() {
            page = page.field(
                "Top Heroes",
                full.top_heroes
                    .iter()
                    .take(5)
                    .map(|hero| {
                        format!(
                            "**{}** - {}g ({:.0}%)",
                            hero.hero_name, hero.games, hero.win_rate
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }
        Ok(page)
    }

    fn teammates(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        match self.player.get_by_id(discord_id, guild_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(ProfilePage::new("Not Registered", DISCORD_RED)
                    .description(format!("{target_name} is not registered.")));
            }
            Err(_) => {
                return Ok(
                    ProfilePage::new("Error", DISCORD_RED).description("Pairings data unavailable")
                );
            }
        }
        let Ok(summary) = self
            .pairings
            .get_player_pairing_summary(discord_id, guild_id, 3, 5)
        else {
            return Ok(
                ProfilePage::new("Error", DISCORD_RED).description("Pairings data unavailable")
            );
        };
        let teammate_record_lines = |rows: &[cama_db::pairings_repository::TeammatePairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> - {:.0}% ({}W/{}L)",
                            row.teammate_id,
                            row.win_rate * 100.0,
                            row.wins_together,
                            row.games_together - row.wins_together
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let opponent_record_lines = |rows: &[cama_db::pairings_repository::OpponentPairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> - {:.0}% ({}W/{}L)",
                            row.opponent_id,
                            row.win_rate * 100.0,
                            row.wins_against,
                            row.games_against - row.wins_against
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let teammate_games_lines = |rows: &[cama_db::pairings_repository::TeammatePairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> - {}g ({:.0}%)",
                            row.teammate_id,
                            row.games_together,
                            row.win_rate * 100.0
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let opponent_games_lines = |rows: &[cama_db::pairings_repository::OpponentPairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> - {}g ({:.0}%)",
                            row.opponent_id,
                            row.games_against,
                            row.win_rate * 100.0
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let even_teammate_lines = |rows: &[cama_db::pairings_repository::TeammatePairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> ({}W/{}L)",
                            row.teammate_id,
                            row.wins_together,
                            row.games_together - row.wins_together
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let even_opponent_lines = |rows: &[cama_db::pairings_repository::OpponentPairing]| {
            if rows.is_empty() {
                "No data yet".to_owned()
            } else {
                rows.iter()
                    .map(|row| {
                        format!(
                            "<@{}> ({}W/{}L)",
                            row.opponent_id,
                            row.wins_against,
                            row.games_against - row.wins_against
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let footer = if summary.counts.unique_teammates > 0 || summary.counts.unique_opponents > 0 {
            format!(
                "Min 3 games | {} teammates, {} opponents tracked",
                summary.counts.unique_teammates, summary.counts.unique_opponents
            )
        } else {
            "Min 3 games".to_owned()
        };
        Ok(ProfilePage::new(
            format!("Profile: {target_name} > Teammates"),
            DISCORD_PURPLE,
        )
        .field(
            "🏆 Best Teammates",
            teammate_record_lines(&summary.best_teammates),
            true,
        )
        .field(
            "💀 Worst Teammates",
            teammate_record_lines(&summary.worst_teammates),
            true,
        )
        .field("\u{200b}", "\u{200b}", true)
        .field(
            "😈 Dominates",
            opponent_record_lines(&summary.best_matchups),
            true,
        )
        .field(
            "😰 Struggles Against",
            opponent_record_lines(&summary.worst_matchups),
            true,
        )
        .field("\u{200b}", "\u{200b}", true)
        .field(
            "👥 Most Played With",
            teammate_games_lines(&summary.most_played_with),
            true,
        )
        .field(
            "⚔️ Most Played Against",
            opponent_games_lines(&summary.most_played_against),
            true,
        )
        .field("\u{200b}", "\u{200b}", true)
        .field(
            "⚖️ Even Teammates",
            even_teammate_lines(&summary.even_teammates),
            true,
        )
        .field(
            "⚖️ Even Opponents",
            even_opponent_lines(&summary.even_opponents),
            true,
        )
        .field("\u{200b}", "\u{200b}", true)
        .footer(footer))
    }

    fn heroes(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        target_name: &str,
    ) -> Result<ProfilePage, String> {
        self.player(discord_id, guild_id)?;
        let matches = self
            .matches
            .get_player_matches(discord_id, guild_id, 100)
            .map_err(|error| error.to_string())?;
        let chart_stats = self
            .matches
            .get_player_hero_chart_stats(discord_id, guild_id, 20)
            .map_err(|error| error.to_string())?;
        let mut heroes = BTreeMap::<i64, HeroAggregate>::new();
        for player_match in &matches {
            let Some(hero_id) = persisted_i64(&player_match.hero_id).filter(|hero_id| *hero_id > 0)
            else {
                continue;
            };
            heroes.entry(hero_id).or_default().record(player_match);
        }
        if heroes.is_empty() {
            return Ok(ProfilePage::new(
                format!("Profile: {target_name} > Heroes"),
                DISCORD_ORANGE,
            )
            .description(
                "No enriched match data available.\n\nHero stats require matches to be enriched with Dota 2 data.\nAsk an admin to run `/enrich match` or `/enrich discover` to add match data.",
            ));
        }
        let mut heroes = heroes.into_iter().collect::<Vec<_>>();
        heroes.sort_by(|left, right| {
            right
                .1
                .games
                .cmp(&left.1.games)
                .then_with(|| left.0.cmp(&right.0))
        });
        let total_games = heroes.iter().map(|(_, stats)| stats.games).sum::<i64>();
        let total_kills = heroes.iter().map(|(_, stats)| stats.kills).sum::<i64>();
        let total_deaths = heroes.iter().map(|(_, stats)| stats.deaths).sum::<i64>();
        let total_assists = heroes.iter().map(|(_, stats)| stats.assists).sum::<i64>();
        let total_gpm = heroes.iter().map(|(_, stats)| stats.gpm).sum::<i64>();
        let top = heroes
            .iter()
            .take(5)
            .map(|(hero_id, stats)| {
                let name = self.opendota.hero_name(*hero_id).unwrap_or("Unknown");
                format!(
                    "**{name}** - {}g ({:.0}%) | {:.1}/{:.1}/{:.1} | {:.0}g",
                    stats.games,
                    stats.wins as f64 / stats.games as f64 * 100.0,
                    stats.kills as f64 / stats.games as f64,
                    stats.deaths as f64 / stats.games as f64,
                    stats.assists as f64 / stats.games as f64,
                    stats.gpm as f64 / stats.games as f64
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut page = ProfilePage::new(format!("Profile: {target_name} > Heroes"), DISCORD_BLUE)
            .field(
                "Overview",
                format!(
                    "**Enriched Games:** {total_games}\n**Avg KDA:** {:.1}/{:.1}/{:.1}\n**Avg GPM:** {:.0}",
                    total_kills as f64 / total_games as f64,
                    total_deaths as f64 / total_games as f64,
                    total_assists as f64 / total_games as f64,
                    total_gpm as f64 / total_games as f64
                ),
                true,
            )
            .field("Top Heroes", top, false)
            .footer(format!(
                "Based on {total_games} enriched matches | Use /enrich match to add more"
            ));
        let average_heroes = heroes
            .iter()
            .filter(|(_, stats)| stats.games >= 2 && stats.wins * 2 == stats.games)
            .take(5)
            .map(|(hero_id, stats)| {
                let name = cama_app::hero_lookup::hero_name(*hero_id);
                format!(
                    "**{name}** - {}g ({}-{}) | {:.1}/{:.1}/{:.1} | {:.0}g",
                    stats.games,
                    stats.wins,
                    stats.wins,
                    stats.kills as f64 / stats.games as f64,
                    stats.deaths as f64 / stats.games as f64,
                    stats.assists as f64 / stats.games as f64,
                    stats.gpm as f64 / stats.games as f64
                )
            })
            .collect::<Vec<_>>();
        if !average_heroes.is_empty() {
            page = page.field("😐 Average Heroes", average_heroes.join("\n"), false);
        }
        if !chart_stats.is_empty() {
            let entries = chart_stats
                .into_iter()
                .map(|stat| HeroPerformanceEntry {
                    // Python's renderer resolves the static hero catalog, so
                    // chart labels stay stable even when the HTTP service is
                    // configured with a partial/custom name map.
                    hero_name: cama_app::hero_lookup::hero_name(stat.hero_id),
                    games: stat.games,
                    wins: stat.wins,
                })
                .collect::<Vec<_>>();
            page = page.image_attachment(
                "hero_chart.png",
                draw_hero_performance_chart(&entries, target_name).into_inner(),
            );
        }
        Ok(page)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HeroAggregate {
    games: i64,
    wins: i64,
    kills: i64,
    deaths: i64,
    assists: i64,
    gpm: i64,
}

impl HeroAggregate {
    fn record(&mut self, player_match: &cama_db::core_repositories::PlayerMatch) {
        self.games += 1;
        self.wins += i64::from(player_match.player_won);
        self.kills += persisted_i64(&player_match.kills).unwrap_or_default();
        self.deaths += persisted_i64(&player_match.deaths).unwrap_or_default();
        self.assists += persisted_i64(&player_match.assists).unwrap_or_default();
        self.gpm += persisted_i64(&player_match.gpm).unwrap_or_default();
    }
}

fn format_balance_history_breakdown(totals: &BTreeMap<String, i64>) -> String {
    if totals.is_empty() {
        return "_No net activity_".to_owned();
    }
    let mut ordered = totals.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .1
            .unsigned_abs()
            .cmp(&left.1.unsigned_abs())
            .then_with(|| balance_source_rank(left.0).cmp(&balance_source_rank(right.0)))
            .then_with(|| left.0.cmp(right.0))
    });
    ordered
        .into_iter()
        .map(|(source, total)| format!("**{}:** {total:+} Jopacoin", balance_source_label(source)))
        .collect::<Vec<_>>()
        .join("\n")
}

const PROFILE_EMBED_TOTAL_LIMIT: usize = 6_000;
const PROFILE_FIELD_VALUE_LIMIT: usize = 1_024;

fn grouped_number(value: i64) -> String {
    let raw = value.to_string();
    let (sign, digits) = raw
        .strip_prefix('-')
        .map_or(("", raw.as_str()), |digits| ("-", digits));
    let first_len = digits.len() % 3;
    let mut grouped = String::new();
    if first_len > 0 {
        grouped.push_str(&digits[..first_len]);
    }
    for chunk in digits.as_bytes()[first_len..].chunks(3) {
        if !grouped.is_empty() {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    format!("{sign}{grouped}")
}

fn grouped_signed(value: i64) -> String {
    if value >= 0 {
        format!("+{}", grouped_number(value))
    } else {
        grouped_number(value)
    }
}

fn auto_bet_group_line(label: &str, group: &AutoBetGroupStats) -> String {
    let losses = group.bet_count.saturating_sub(group.wins);
    let bet_word = if group.bet_count == 1 { "bet" } else { "bets" };
    format!(
        "**{label}:** {} JC · {} {} · {}W–{}L · {} wagered",
        grouped_signed(group.net_pnl),
        grouped_number(i64::try_from(group.bet_count).unwrap_or(i64::MAX)),
        bet_word,
        grouped_number(i64::try_from(group.wins).unwrap_or(i64::MAX)),
        grouped_number(i64::try_from(losses).unwrap_or(i64::MAX)),
        grouped_number(group.total_wagered)
    )
}

fn auto_bet_direction_label(directions: &[String]) -> (&'static str, String) {
    let normalized = directions
        .iter()
        .map(|direction| direction.to_ascii_lowercase())
        .filter(|direction| matches!(direction.as_str(), "long" | "short"))
        .fold(Vec::<String>::new(), |mut seen, direction| {
            if !seen.contains(&direction) {
                seen.push(direction);
            }
            seen
        });
    match normalized.as_slice() {
        [direction] if direction == "long" => ("↗", "LONG".to_owned()),
        [direction] if direction == "short" => ("↘", "SHORT".to_owned()),
        [] => ("•", "AUTO".to_owned()),
        directions => (
            "↕",
            directions
                .iter()
                .map(|direction| direction.to_ascii_uppercase())
                .collect::<Vec<_>>()
                .join("/"),
        ),
    }
}

fn auto_bet_target_line(target: &PlayerAutoBetStats) -> String {
    let (icon, directions) = auto_bet_direction_label(&target.directions);
    let losses = target.bet_count.saturating_sub(target.wins);
    format!(
        "{icon} <@{}> {directions} · {} JC · {}W–{}L · {} wagered",
        target.target_id,
        grouped_signed(target.net_pnl),
        grouped_number(i64::try_from(target.wins).unwrap_or(i64::MAX)),
        grouped_number(i64::try_from(losses).unwrap_or(i64::MAX)),
        grouped_number(target.total_wagered)
    )
}

fn truncate_profile_text(value: &str, budget: usize) -> String {
    value.chars().take(budget).collect()
}

fn truncate_question(question: &str, limit: usize) -> String {
    if question.chars().count() <= limit {
        question.to_owned()
    } else {
        format!("{}...", question.chars().take(limit).collect::<String>())
    }
}

fn profile_text_len(page: &ProfilePage) -> usize {
    page.title.chars().count()
        + page
            .description
            .as_deref()
            .map_or(0, |value| value.chars().count())
        + page
            .footer
            .as_deref()
            .map_or(0, |value| value.chars().count())
        + page
            .fields
            .iter()
            .map(|field| field.name.chars().count() + field.value.chars().count())
            .sum::<usize>()
}

fn append_auto_bet_field(page: &mut ProfilePage, performance: &AutoBetPerformance) {
    if performance.total.bet_count == 0
        && performance.generic.bet_count == 0
        && performance.arbitrage.bet_count == 0
        && performance
            .targets
            .iter()
            .all(|target| target.bet_count == 0)
    {
        return;
    }

    let name = "Auto-Bet P&L";
    let remaining_total =
        PROFILE_EMBED_TOTAL_LIMIT.saturating_sub(profile_text_len(page) + name.chars().count());
    let budget = PROFILE_FIELD_VALUE_LIMIT.min(remaining_total);
    if budget < 4 {
        return;
    }

    let summary_lines = [
        auto_bet_group_line("Auto total", &performance.total),
        auto_bet_group_line("Generic auto", &performance.generic),
        auto_bet_group_line("Arbitrage hedges", &performance.arbitrage),
    ];
    let target_lines = performance
        .targets
        .iter()
        .map(auto_bet_target_line)
        .collect::<Vec<_>>();
    let heading = "**By player:**";
    let compose = |count: usize| {
        let mut lines = summary_lines.to_vec();
        if !target_lines.is_empty() {
            lines.push(heading.to_owned());
            lines.extend(target_lines.iter().take(count).cloned());
            let omitted = target_lines.len().saturating_sub(count);
            if omitted > 0 {
                let noun = if omitted == 1 { "target" } else { "targets" };
                lines.push(format!(
                    "*…{} more {noun} omitted*",
                    grouped_number(i64::try_from(omitted).unwrap_or(i64::MAX))
                ));
            }
        }
        lines.join("\n")
    };
    let mut value = compose(0);
    if value.chars().count() > budget {
        let suffix = if target_lines.is_empty() {
            String::new()
        } else {
            format!(
                "\n{heading}\n*…{} more {} omitted*",
                grouped_number(i64::try_from(target_lines.len()).unwrap_or(i64::MAX)),
                if target_lines.len() == 1 {
                    "target"
                } else {
                    "targets"
                }
            )
        };
        let prefix_budget = budget.saturating_sub(suffix.chars().count());
        value = if prefix_budget == 0 {
            truncate_profile_text(suffix.trim_start(), budget)
        } else {
            format!(
                "{}{suffix}",
                truncate_profile_text(&summary_lines.join("\n"), prefix_budget)
            )
        };
    } else {
        for count in 1..=target_lines.len() {
            let candidate = compose(count);
            if candidate.chars().count() > budget {
                break;
            }
            value = candidate;
        }
    }
    if !value.is_empty() {
        page.fields.push(ProfileField::new(name, value, false));
    }
}

fn balance_source_label(source: &str) -> &str {
    match source {
        "bets" => "Bets",
        "predictions" => "Predictions",
        "wheel" => "Wheel",
        "double_or_nothing" => "DoN",
        "tips" => "Tips",
        "disburse" => "Disburse",
        "bonus" => "Bonuses",
        "dig" => "Dig",
        _ => source,
    }
}

fn balance_source_rank(source: &str) -> usize {
    match source {
        "bets" => 0,
        "predictions" => 1,
        "wheel" => 2,
        "double_or_nothing" => 3,
        "tips" => 4,
        "disburse" => 5,
        "bonus" => 6,
        "dig" => 7,
        _ => usize::MAX,
    }
}

fn persisted_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        Value::Real(value) if value.is_finite() => Some(*value as i64),
        Value::Text(value) => value.parse::<f64>().ok().map(|value| value as i64),
        _ => None,
    }
}

fn optional_decimal(value: Option<f64>) -> String {
    value.map_or_else(|| "Not set".to_owned(), |value| format!("{value:.2}"))
}

fn role_name(role: &str) -> String {
    match role {
        "1" => "Carry".to_owned(),
        "2" => "Mid".to_owned(),
        "3" => "Offlane".to_owned(),
        "4" => "Soft Support".to_owned(),
        "5" => "Hard Support".to_owned(),
        value => value.to_owned(),
    }
}

fn region_name(region: &str) -> String {
    match region {
        "USW" => "US West".to_owned(),
        "USE" => "US East".to_owned(),
        "EUW" => "Europe West".to_owned(),
        "EUE" => "Europe East".to_owned(),
        "SEA" => "Southeast Asia".to_owned(),
        value => value.to_owned(),
    }
}

fn distribution_lines(distribution: &BTreeMap<String, f64>) -> String {
    let mut rows = distribution.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| right.1.total_cmp(left.1).then_with(|| left.0.cmp(right.0)));
    let mut result = String::new();
    for (index, (name, percentage)) in rows.into_iter().enumerate() {
        if index != 0 {
            result.push('\n');
        }
        let _ = write!(result, "**{name}:** {percentage:.0}%");
    }
    result
}

fn render_independent_dota_charts(
    role_job: Option<ProfileChartJob<'_>>,
    lane_job: Option<ProfileChartJob<'_>>,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    std::thread::scope(|scope| {
        let role_handle = role_job.map(|job| scope.spawn(job));
        let lane_handle = lane_job.map(|job| scope.spawn(job));
        let join = |handle: std::thread::ScopedJoinHandle<'_, Vec<u8>>| match handle.join() {
            Ok(bytes) => bytes,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        (role_handle.map(join), lane_handle.map(join))
    })
}

fn ordered_positive_lanes(distribution: &BTreeMap<String, f64>) -> Vec<(String, f64)> {
    ["Roaming", "Safe Lane", "Mid", "Off Lane", "Jungle"]
        .into_iter()
        .filter_map(|lane| {
            let value = distribution
                .get(lane)
                .copied()
                .filter(|value| *value > 0.0)?;
            Some((lane.to_owned(), value))
        })
        .collect()
}

#[cfg(test)]
mod tests;
