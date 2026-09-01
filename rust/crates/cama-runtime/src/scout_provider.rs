use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::dotabase_sqlite::{DotabaseSqliteSource, PRODUCTION_DOTABASE_PATH};
use cama_app::draft::DraftStateManager;
use cama_app::embeds::LobbyKind;
use cama_app::scout::media::{NativeScoutImageRenderer, portrait_png_is_supported};
use cama_app::scout::{
    ContextSources, DraftContext, LinksOutcome, LinksRequest, PlayerProfile, ReportOutcome,
    ReportRequest, ScoutData, ScoutImageRenderer, ScoutReportInput, ScoutService, TeamFilter,
    Teams, plan_links, plan_report,
};
use cama_app::trivia_data::{TriviaDataSource, hero_image_url};
use cama_app::trivia_image_cache::production_steam_image_cache_root;
use cama_db::core_repositories::PlayerRepository;
use cama_db::herogrid_repository::{HeroGridRepository, HeroGridTeams, PersistedHeroGridSources};
use cama_db::scout_repository::ScoutRepository;
use thiserror::Error;
use tracing::{error, warn};

use crate::discord_transport::{GuildPlayerNameDirectory, GuildPlayerNameResolver};
use crate::embed_colors::{DISCORD_BLUE, DISCORD_GOLD};
use crate::option_ext::string_option;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAttachment, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionMessageDelivery,
    InteractionMessageReceipt, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
};

const HEROES_PER_PAGE: usize = 10;
const REPORT_LIMIT: usize = 100;
const VIEW_TIMEOUT: Duration = Duration::from_secs(840);
const COMPONENT_PREFIX: &str = "scout:";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const RENDER_FAILURE: &str = "Failed to generate scout report. Please try again.";

#[derive(Debug, Error)]
pub enum ScoutProviderBuildError {
    #[error("could not load the bundled Dotabase hero catalog: {0}")]
    HeroCatalog(String),
}

pub struct ScoutRegistrationProvider {
    handler: Arc<ScoutHandler>,
}

impl ScoutRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        draft_states: Arc<DraftStateManager>,
        player_names: Arc<dyn GuildPlayerNameResolver>,
    ) -> Result<Self, ScoutProviderBuildError> {
        Self::with_assets(
            database_path,
            draft_states,
            player_names,
            PRODUCTION_DOTABASE_PATH,
            production_steam_image_cache_root().join("scout"),
            VIEW_TIMEOUT,
        )
    }

    fn with_assets(
        database_path: impl AsRef<Path>,
        draft_states: Arc<DraftStateManager>,
        player_names: Arc<dyn GuildPlayerNameResolver>,
        dotabase_path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        view_timeout: Duration,
    ) -> Result<Self, ScoutProviderBuildError> {
        let hero_urls = DotabaseSqliteSource::new(dotabase_path)
            .heroes()
            .map_err(|error| ScoutProviderBuildError::HeroCatalog(error.to_string()))?
            .into_iter()
            .filter_map(|hero| {
                hero.name
                    .as_deref()
                    .and_then(hero_image_url)
                    .map(|url| (hero.id, url))
            })
            .collect();
        let database_path = database_path.as_ref().to_path_buf();
        Ok(Self {
            handler: Arc::new(ScoutHandler {
                scout_repository: ScoutRepository::new(&database_path),
                context_repository: HeroGridRepository::new(&database_path),
                player_repository: PlayerRepository::new(&database_path),
                player_names,
                draft_states,
                hero_urls: Arc::new(hero_urls),
                portrait_cache_root: cache_path.as_ref().to_path_buf(),
                portrait_batch_lock: Arc::new(Mutex::new(())),
                views: Arc::new(Mutex::new(BTreeMap::new())),
                next_view_id: AtomicU64::new(1),
                view_timeout,
            }),
        })
    }
}

impl RegistrationProvider for ScoutRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        let team = || CommandOptionSpec {
            choices: vec![
                string_choice("Radiant", "radiant"),
                string_choice("Dire", "dire"),
            ],
            ..CommandOptionSpec::new(
                "team",
                "Team to use from the active match: radiant or dire",
                CommandOptionKind::String,
            )
        };
        let lobby = || CommandOptionSpec {
            choices: vec![
                string_choice("🍽️ All You Can Feed", "open"),
                string_choice("🧀 Whine & Cheese", "lowskill"),
            ],
            ..CommandOptionSpec::new(
                "lobby",
                "Lobby to use (defaults to the lobby you're in)",
                CommandOptionKind::String,
            )
        };
        let players = || {
            CommandOptionSpec::new(
                "players",
                "@mention players to scout (optional)",
                CommandOptionKind::String,
            )
        };
        registry.command(CommandSpec {
            name: "scout".to_owned(),
            description: "Player scouting tools".to_owned(),
            options: vec![
                CommandOptionSpec::new(
                    "report",
                    "Generate hero scouting report for players",
                    CommandOptionKind::Subcommand,
                )
                .options(vec![players(), team(), lobby()]),
                CommandOptionSpec::new(
                    "links",
                    "List Dotabuff profile links for players in the current game",
                    CommandOptionKind::Subcommand,
                )
                .options(vec![players(), team(), lobby()]),
            ],
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

fn string_choice(name: &str, value: &str) -> CommandOptionChoice {
    CommandOptionChoice::String {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

struct ScoutHandler {
    scout_repository: ScoutRepository,
    context_repository: HeroGridRepository,
    player_repository: PlayerRepository,
    player_names: Arc<dyn GuildPlayerNameResolver>,
    draft_states: Arc<DraftStateManager>,
    hero_urls: Arc<BTreeMap<i64, String>>,
    portrait_cache_root: std::path::PathBuf,
    portrait_batch_lock: Arc<Mutex<()>>,
    views: Arc<Mutex<BTreeMap<u64, Arc<tokio::sync::Mutex<ScoutViewState>>>>>,
    next_view_id: AtomicU64,
    view_timeout: Duration,
}

#[derive(Clone)]
struct ScoutViewState {
    owner_id: u64,
    page: usize,
    scout_data: ScoutData,
    player_names: Vec<String>,
    title: String,
    /// Bumped on every accepted page, so the timer armed for an earlier one
    /// finds a newer generation and expires nothing.
    expiry_generation: u64,
    receipt: Option<InteractionMessageReceipt>,
    /// The responder that created the followup. Deleting an interaction
    /// followup goes through the token that produced it, so a later
    /// component's responder cannot stand in for it.
    owner: Option<Arc<dyn InteractionResponder>>,
}

#[async_trait]
impl InteractionHandler for ScoutHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { .. } => self.handle_command(request, responder).await,
            InteractionRequest::Component { .. } => self.handle_component(request, responder).await,
            InteractionRequest::Modal { .. } => Err("Scout handler received a modal".into()),
            InteractionRequest::Autocomplete { .. } => {
                Err("Scout handler received autocomplete".into())
            }
        }
    }
}

impl ScoutHandler {
    async fn handle_command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            name,
            user_id,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("Scout command handler received a non-command".into());
        };
        if name != "scout" {
            return Err(format!("Scout handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            return responder
                .respond(InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string().into());
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let invoking_user_id = signed_id(user_id, "user")?;
        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer Scout interaction");
            return Ok(());
        }
        let (subcommand, options) = subcommand(&options)?;
        let team_filter = parse_team(string_option(options, "team"))?;
        let explicit_lobby = parse_lobby(string_option(options, "lobby"))?;
        let mentions = string_option(options, "players").map(str::to_owned);
        let sources = self
            .load_context(guild_id, invoking_user_id)
            .await
            .map_err(InteractionHandlerError::from)?;
        match subcommand {
            "report" => {
                self.handle_report(
                    user_id,
                    guild_id,
                    mentions,
                    team_filter,
                    explicit_lobby,
                    sources,
                    responder,
                )
                .await
            }
            "links" => {
                self.handle_links(
                    guild_id,
                    mentions,
                    team_filter,
                    explicit_lobby,
                    sources,
                    responder,
                )
                .await
            }
            other => Err(format!("unknown Scout subcommand {other:?}").into()),
        }
    }

    async fn load_context(
        &self,
        guild_id: i64,
        invoking_user_id: i64,
    ) -> Result<ContextSources, String> {
        let repository = self.context_repository.clone();
        let drafts = Arc::clone(&self.draft_states);
        tokio::task::spawn_blocking(move || {
            let persisted = repository
                .get_persisted_sources(Some(guild_id), Some(invoking_user_id))
                .map_err(|error| error.to_string())?;
            Ok(runtime_sources(persisted, &drafts, guild_id))
        })
        .await
        .map_err(|error| format!("Scout context task failed: {error}"))?
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_report(
        &self,
        owner_id: u64,
        guild_id: i64,
        mentions: Option<String>,
        team_filter: Option<TeamFilter>,
        explicit_lobby: Option<LobbyKind>,
        sources: ContextSources,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let initial_plan = plan_report(&ReportRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: mentions.as_deref(),
            team_filter,
            explicit_lobby,
            has_hero_data: true,
        });
        let plan = match initial_plan {
            ReportOutcome::Message(message) => {
                return responder
                    .followup(InteractionResponse::message(message))
                    .await
                    .map_err(|error| error.to_string().into());
            }
            ReportOutcome::Render(plan) => plan,
        };
        let ids = plan.player_ids.clone();
        let query_ids = ids.clone();
        let repository = self.scout_repository.clone();
        let player_repository = self.player_repository.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let scout_data = ScoutService::new(repository)
                .get_scout_data(&query_ids, Some(guild_id), REPORT_LIMIT)
                .map_err(|error| error.to_string())?;
            let players = player_repository
                .get_by_ids(&query_ids, Some(guild_id))
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((scout_data, players))
        })
        .await
        .map_err(|error| format!("Scout report task failed: {error}"))?
        .map_err(InteractionHandlerError::from)?;
        if loaded.0.heroes.is_empty() {
            let outcome = plan_report(&ReportRequest {
                sources: &sources,
                has_invoking_user: true,
                mentions: mentions.as_deref(),
                team_filter,
                explicit_lobby,
                has_hero_data: false,
            });
            let ReportOutcome::Message(message) = outcome else {
                return Err("Scout empty-data plan unexpectedly requested rendering".into());
            };
            return responder
                .followup(InteractionResponse::message(message))
                .await
                .map_err(|error| error.to_string().into());
        }
        let player_names = match self.player_names.names_for_guild(guild_id, &ids) {
            Ok(names) => names,
            Err(error) => {
                warn!(%error, guild_id, "Scout report player-name snapshot failed");
                GuildPlayerNameDirectory::default()
            }
        };
        let player_names = loaded
            .1
            .into_iter()
            .filter_map(|player| {
                let discord_id = player.discord_id?;
                Some(player_names.resolve(discord_id))
            })
            .collect();
        let view_id = self.next_view_id.fetch_add(1, Ordering::Relaxed);
        let state = ScoutViewState {
            owner_id,
            page: 0,
            scout_data: loaded.0,
            player_names,
            title: plan.title,
            expiry_generation: 0,
            receipt: None,
            owner: None,
        };
        let response = match self.render_page(view_id, state.clone()).await {
            Ok(response) => response,
            Err(message) => {
                error!(%message, "Scout image generation failed");
                return responder
                    .followup(InteractionResponse::message(RENDER_FAILURE))
                    .await
                    .map_err(|error| error.to_string().into());
            }
        };
        self.views
            .lock()
            .map_err(|_| "Scout view registry lock was poisoned")?
            .insert(view_id, Arc::new(tokio::sync::Mutex::new(state)));
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Ok(mut views) = self.views.lock() {
                    views.remove(&view_id);
                }
                return Err(error.to_string().into());
            }
        };
        let view = self
            .views
            .lock()
            .ok()
            .and_then(|views| views.get(&view_id).cloned());
        if let Some(view) = view {
            let mut state = view.lock().await;
            state.receipt = receipt;
            state.owner = Some(Arc::clone(&responder));
        }
        self.schedule_expiry(view_id, 0, responder);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_links(
        &self,
        guild_id: i64,
        mentions: Option<String>,
        team_filter: Option<TeamFilter>,
        explicit_lobby: Option<LobbyKind>,
        sources: ContextSources,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let ids = match link_player_ids(&sources, mentions.as_deref(), team_filter, explicit_lobby)
        {
            Ok(ids) => ids,
            Err(message) => {
                return responder
                    .followup(InteractionResponse::message(message))
                    .await
                    .map_err(|error| error.to_string().into());
            }
        };
        if ids.is_empty() {
            let outcome = plan_links(&LinksRequest {
                sources: &sources,
                has_invoking_user: true,
                mentions: mentions.as_deref(),
                team_filter,
                explicit_lobby,
                profiles: &[],
                steam_ids: &BTreeMap::new(),
            });
            let LinksOutcome::Message(message) = outcome else {
                return Err("Scout empty link plan unexpectedly built an embed".into());
            };
            return responder
                .followup(InteractionResponse::message(message))
                .await
                .map_err(|error| error.to_string().into());
        }
        let player_names = match self.player_names.names_for_guild(guild_id, &ids) {
            Ok(names) => names,
            Err(error) => {
                warn!(%error, guild_id, "Scout server nickname lookup failed");
                Default::default()
            }
        };
        let player_repository = self.player_repository.clone();
        let scout_repository = self.scout_repository.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let players = player_repository
                .get_by_ids(&ids, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let profiles = players
                .iter()
                .filter_map(|player| {
                    player.discord_id.map(|discord_id| PlayerProfile {
                        discord_id,
                        name: player.name.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let registered = profiles
                .iter()
                .map(|profile| profile.discord_id)
                .collect::<Vec<_>>();
            let steam_ids = scout_repository
                .get_steam_ids_bulk(&registered, Some(guild_id))
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((profiles, steam_ids))
        })
        .await
        .map_err(|error| format!("Scout links task failed: {error}"))?
        .map_err(InteractionHandlerError::from)?;
        let (mut profiles, steam_ids) = loaded;
        for profile in &mut profiles {
            profile.name = player_names.resolve(profile.discord_id);
        }
        match plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: mentions.as_deref(),
            team_filter,
            explicit_lobby,
            profiles: &profiles,
            steam_ids: &steam_ids,
        }) {
            LinksOutcome::Message(message) => responder
                .followup(InteractionResponse::message(message))
                .await
                .map_err(|error| error.to_string().into()),
            LinksOutcome::Embed(embed) => {
                let mut response_embed = InteractionEmbed::titled(embed.title).color(DISCORD_BLUE);
                for field in embed.fields {
                    response_embed = response_embed.field(field.name, field.value, false);
                }
                responder
                    .followup(InteractionResponse::message("").embed(response_embed))
                    .await
                    .map_err(|error| error.to_string().into())
            }
        }
    }

    async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Component {
            custom_id, user_id, ..
        } = request
        else {
            return Err("Scout component handler received a non-component".into());
        };
        let (view_id, direction) = parse_component_id(&custom_id)?;
        let view = self
            .views
            .lock()
            .map_err(|_| "Scout view registry lock was poisoned")?
            .get(&view_id)
            .cloned();
        let Some(view) = view else {
            return Ok(());
        };
        let snapshot = {
            let mut state = view.lock().await;
            if state.owner_id != user_id {
                return responder
                    .respond(
                        InteractionResponse::message(
                            "Only the person who ran this command can page it.",
                        )
                        .ephemeral(),
                    )
                    .await
                    .map_err(|error| error.to_string().into());
            }
            let pages = total_pages(state.scout_data.heroes.len());
            let next = match direction {
                PageDirection::Previous => state.page.saturating_sub(1),
                PageDirection::Next => state.page.saturating_add(1).min(pages - 1),
            };
            if next == state.page {
                return responder
                    .defer(false)
                    .await
                    .map_err(|error| error.to_string().into());
            }
            state.page = next;
            state.expiry_generation = state.expiry_generation.wrapping_add(1);
            // Restart the deletion timer from this interaction, before any
            // await, so a timer that would otherwise fire mid-handler does not
            // delete a report the owner is actively paging.
            if let Some(owner) = state.owner.clone() {
                self.schedule_expiry(view_id, state.expiry_generation, owner);
            }
            state.clone()
        };
        let response = self
            .render_page(view_id, snapshot)
            .await
            .map_err(|message| {
                error!(%message, view_id, "Scout page rendering failed");
                InteractionHandlerError::from(message)
            })?;
        responder
            .update(response)
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn render_page(
        &self,
        view_id: u64,
        state: ScoutViewState,
    ) -> Result<InteractionResponse, String> {
        let hero_urls = Arc::clone(&self.hero_urls);
        let portrait_cache_root = self.portrait_cache_root.clone();
        let portrait_batch_lock = Arc::clone(&self.portrait_batch_lock);
        tokio::task::spawn_blocking(move || {
            let start = state.page.saturating_mul(HEROES_PER_PAGE);
            let end = start
                .saturating_add(HEROES_PER_PAGE)
                .min(state.scout_data.heroes.len());
            let heroes = state.scout_data.heroes[start..end].to_vec();
            let _cache_guard = portrait_batch_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let portraits = load_portraits(&heroes, &hero_urls, &portrait_cache_root);
            let page_data = ScoutData {
                player_count: state.scout_data.player_count,
                total_matches: state.scout_data.total_matches,
                heroes,
            };
            let png = NativeScoutImageRenderer::new(portraits)
                .render(&ScoutReportInput {
                    scout_data: page_data,
                    player_names: state.player_names,
                    title: state.title.clone(),
                })
                .map_err(|error| error.to_string())?;
            let pages = total_pages(state.scout_data.heroes.len());
            let description = format!(
                "{} players | Heroes {}-{} of {}",
                state.scout_data.player_count,
                start + 1,
                end,
                state.scout_data.heroes.len()
            );
            let embed = InteractionEmbed::titled(state.title)
                .description(description)
                .color(DISCORD_GOLD)
                .image("attachment://scout_report.png")
                .footer(format!(
                    "Page {}/{} | Tot=W+L+B | CR=Tot/Games | WR=W/(W+L)",
                    state.page + 1,
                    pages
                ));
            Ok(InteractionResponse::message("")
                .embed(embed)
                .attachment(InteractionAttachment::bytes("scout_report.png", png))
                .action_row(page_buttons(view_id, state.page, pages)))
        })
        .await
        .map_err(|error| format!("Scout render task failed: {error}"))?
    }

    /// Delete the report `view_timeout` after the last interaction with it.
    ///
    /// A single timer armed at creation would delete a report the owner is
    /// still paging through; the legacy view measured its timeout from the last
    /// interaction, so each accepted page re-arms this.
    fn schedule_expiry(
        &self,
        view_id: u64,
        expiry_generation: u64,
        responder: Arc<dyn InteractionResponder>,
    ) {
        let views = Arc::clone(&self.views);
        let timeout = self.view_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let view = views
                .lock()
                .ok()
                .and_then(|views| views.get(&view_id).cloned());
            let Some(view) = view else {
                return;
            };
            let receipt = {
                let state = view.lock().await;
                if state.expiry_generation != expiry_generation {
                    return;
                }
                state.receipt
            };
            if let Ok(mut views) = views.lock()
                && views
                    .get(&view_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &view))
            {
                views.remove(&view_id);
            }
            // Delete through the channel rather than the interaction
            // followup: the timer restarts on every interaction, so it can
            // fire after the interaction token's fifteen-minute life, and a
            // token-scoped delete would 404 and leave the message behind.
            if let Some(receipt) = receipt
                && let Err(error) = responder
                    .delete_message(InteractionMessageReceipt {
                        delivery: InteractionMessageDelivery::ChannelFallback,
                        ..receipt
                    })
                    .await
            {
                warn!(%error, view_id, "failed to delete expired Scout message");
            }
        });
    }
}

fn load_portraits(
    heroes: &[cama_app::scout::ScoutHero],
    hero_urls: &BTreeMap<i64, String>,
    cache_root: &Path,
) -> BTreeMap<i64, Vec<u8>> {
    let mut seen = BTreeSet::new();
    let queue = heroes
        .iter()
        .filter(|hero| seen.insert(hero.hero_id))
        .filter_map(|hero| {
            hero_urls
                .get(&hero.hero_id)
                .cloned()
                .map(|url| (hero.hero_id, url))
        })
        .collect::<VecDeque<_>>();
    if queue.is_empty() {
        return BTreeMap::new();
    }
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return BTreeMap::new();
    };
    let worker_count = queue.len().min(4);
    let queue = Arc::new(Mutex::new(queue));
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            let client = client.clone();
            scope.spawn(move || {
                loop {
                    let next = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                    let Some((hero_id, url)) = next else {
                        break;
                    };
                    if let Some(bytes) = fetch_portrait(hero_id, &url, cache_root, &client) {
                        let _ = sender.send((hero_id, bytes));
                    }
                }
            });
        }
    });
    drop(sender);
    receiver.into_iter().collect()
}

fn fetch_portrait(
    hero_id: i64,
    url: &str,
    cache_root: &Path,
    client: &reqwest::blocking::Client,
) -> Option<Vec<u8>> {
    let path = cache_root.join("heroes").join(format!("{hero_id}.png"));
    if let Ok(bytes) = fs::read(&path) {
        if portrait_png_is_supported(&bytes) {
            return Some(bytes);
        }
        let _ = fs::remove_file(&path);
    }
    let bytes = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::bytes)
        .ok()?
        .to_vec();
    if !portrait_png_is_supported(&bytes) {
        return None;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, &bytes);
    Some(bytes)
}

fn runtime_sources(
    persisted: PersistedHeroGridSources,
    drafts: &DraftStateManager,
    guild_id: i64,
) -> ContextSources {
    let draft = drafts.get_state(Some(guild_id)).map(|draft| {
        let draft = draft
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        DraftContext {
            teams: Teams {
                radiant: draft.radiant_player_ids.clone(),
                dire: draft.dire_player_ids.clone(),
            },
            player_pool: draft.player_pool_ids.clone(),
            lobby_kind: match draft.lobby_kind {
                cama_app::draft::LobbyKind::Open => LobbyKind::Open,
                cama_app::draft::LobbyKind::LowSkill => LobbyKind::LowSkill,
            },
        }
    });
    ContextSources {
        invoking_pending_match: persisted.invoking_pending_match.map(teams),
        guild_pending_match: persisted.guild_pending_match.map(teams),
        draft,
        open_lobby: persisted.open_lobby,
        low_skill_lobby: persisted.low_skill_lobby,
        invoking_user_lobbies: persisted
            .invoking_user_lobby_ids
            .into_iter()
            .map(|id| {
                if id == 2 {
                    LobbyKind::LowSkill
                } else {
                    LobbyKind::Open
                }
            })
            .collect(),
    }
}

fn teams(value: HeroGridTeams) -> Teams {
    Teams {
        radiant: value.radiant,
        dire: value.dire,
    }
}

fn link_player_ids(
    sources: &ContextSources,
    mentions: Option<&str>,
    team_filter: Option<TeamFilter>,
    explicit_lobby: Option<LobbyKind>,
) -> Result<Vec<i64>, String> {
    let mentioned = mentions
        .map(cama_app::scout::parse_mentions)
        .unwrap_or_default();
    if !mentioned.is_empty() {
        return Ok(stable_unique(&mentioned));
    }
    let context = cama_app::scout::resolve_team_context(sources, true, explicit_lobby)
        .map_err(|_| cama_app::scout::AMBIGUOUS_LOBBY_MESSAGE.to_owned())?;
    if context.split {
        return Ok(match team_filter {
            Some(TeamFilter::Radiant) => context.radiant,
            Some(TeamFilter::Dire) => context.dire,
            None => stable_unique(
                &context
                    .radiant
                    .into_iter()
                    .chain(context.dire)
                    .collect::<Vec<_>>(),
            ),
        });
    }
    Ok(context.flat)
}

fn stable_unique(values: &[i64]) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

fn subcommand(options: &[InteractionOption]) -> Result<(&str, &[InteractionOption]), String> {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::Subcommand(children) => {
                Some((option.name.as_str(), children.as_slice()))
            }
            _ => None,
        })
        .ok_or_else(|| "Scout command is missing a subcommand".to_owned())
}

fn parse_team(value: Option<&str>) -> Result<Option<TeamFilter>, String> {
    match value {
        None => Ok(None),
        Some("radiant") => Ok(Some(TeamFilter::Radiant)),
        Some("dire") => Ok(Some(TeamFilter::Dire)),
        Some(value) => Err(format!("invalid Scout team {value:?}")),
    }
}

fn parse_lobby(value: Option<&str>) -> Result<Option<LobbyKind>, String> {
    match value {
        None => Ok(None),
        Some("open") => Ok(Some(LobbyKind::Open)),
        Some("lowskill") => Ok(Some(LobbyKind::LowSkill)),
        Some(value) => Err(format!("invalid Scout lobby {value:?}")),
    }
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("Discord {label} id exceeds SQLite's signed range"))
}

#[derive(Clone, Copy)]
enum PageDirection {
    Previous,
    Next,
}

fn parse_component_id(custom_id: &str) -> Result<(u64, PageDirection), String> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("scout") || parts.next() != Some("page") {
        return Err(format!("invalid Scout component id {custom_id:?}"));
    }
    let view_id = parts
        .next()
        .ok_or_else(|| format!("invalid Scout component id {custom_id:?}"))?
        .parse()
        .map_err(|_| format!("invalid Scout view id in {custom_id:?}"))?;
    let direction = match parts.next() {
        Some("prev") => PageDirection::Previous,
        Some("next") => PageDirection::Next,
        _ => return Err(format!("invalid Scout direction in {custom_id:?}")),
    };
    if parts.next().is_some() {
        return Err(format!("invalid Scout component id {custom_id:?}"));
    }
    Ok((view_id, direction))
}

fn total_pages(hero_count: usize) -> usize {
    hero_count.div_ceil(HEROES_PER_PAGE).max(1)
}

fn page_buttons(view_id: u64, page: usize, pages: usize) -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("scout:page:{view_id}:prev"), "◀ Prev")
            .style(InteractionButtonStyle::Secondary)
            .disabled(page == 0),
        InteractionButton::new(format!("scout:page:{view_id}:next"), "Next ▶")
            .style(InteractionButtonStyle::Secondary)
            .disabled(page + 1 >= pages),
    ])
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "scout_provider/tests.rs"]
mod tests;
