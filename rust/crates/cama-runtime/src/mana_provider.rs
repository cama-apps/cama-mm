//! Production Discord provider for Python's complete `/mana` extension.
//!
//! The fixed `mana:board:<owner>:<page>` component route dispatches pagination
//! while the process-local 300-second view lease remains live. Every component
//! rebuilds the board from the persisted assignment snapshot and current
//! guild member cache, while the invoker ID encoded in the custom ID enforces
//! Python's public-message paging policy. Like the Python view, a restart drops
//! those live leases and leaves the old buttons inert.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::mana_service::{
    AssignmentBoard, BankruptcyState, BetOutcome, BoardRow, DegenScore, GuardianProtection, Land,
    ManaDataSource, ManaDetails, ManaService, ManaServiceError, PlayerProfile, TipStats,
    XorShift64Entropy, mana_day_label, mana_day_start_timestamp,
};
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::PlayerRepository;
use cama_db::gambling_stats_repository::{
    BetHistoryEntry, GamblingOutcome, GamblingStatsPort, GamblingStatsRepository,
    GamblingStatsService,
};
use cama_db::loan_repository::LoanRepository;
use cama_db::mana_protection::ManaProtectionRepository;
use cama_db::mana_service_repository::ManaRepository;
use cama_db::predictions_repository::PredictionRepository;
use cama_db::tip_repository::TipRepository;
use chrono::{DateTime, FixedOffset, Utc};

use crate::ApplicationConfig;
use crate::discord_transport::{DiscordIdPlayerNameResolver, GuildPlayerNameResolver};
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionButton, InteractionButtonStyle, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, RegistrationError, RegistrationProvider, RegistryBuilder,
};

const COMPONENT_PREFIX: &str = "mana:board:";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const WRONG_CHANNEL_MESSAGE: &str = "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.";
const PAGE_SIZE: usize = 12;
const COMMAND_RATE: usize = 3;
const COMMAND_PERIOD: Duration = Duration::from_secs(10);
const VIEW_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaGuildMember {
    pub user_id: i64,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role_names: Vec<String>,
}

impl ManaGuildMember {
    #[must_use]
    pub fn is_ash_fan(&self) -> bool {
        self.role_names
            .iter()
            .any(|role| role.to_ascii_lowercase().contains("ash"))
    }
}

/// Live Discord cache/HTTP operations that are specific to `/mana`.
#[async_trait]
pub trait ManaDiscordPort: Send + Sync {
    /// True when the channel itself, or a thread's parent, contains `gamba`.
    async fn mana_channel_is_gamba(&self, guild_id: i64, channel_id: i64) -> Result<bool, String>;

    /// Return the guild member cache used by Python for Ash roles and names.
    fn mana_guild_members(&self, guild_id: i64) -> Result<Vec<ManaGuildMember>, String>;

    /// Resolve a selected target through the guild cache or Discord HTTP.
    async fn mana_guild_member(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<ManaGuildMember>, String>;
}

#[derive(Clone)]
pub struct ManaRegistrationProvider {
    handler: Arc<ManaInteractionHandler>,
}

impl ManaRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn ManaDiscordPort>,
    ) -> Self {
        Self::from_parts(
            database_path.as_ref().to_path_buf(),
            config.values.wheel_cooldown_seconds,
            config.values.white_bankrupt_stipend,
            discord,
            Arc::new(SystemManaClock),
        )
    }

    fn from_parts(
        database_path: PathBuf,
        wheel_cooldown_seconds: i64,
        _white_stipend: i64,
        discord: Arc<dyn ManaDiscordPort>,
        clock: Arc<dyn ManaClock>,
    ) -> Self {
        Self {
            handler: Arc::new(ManaInteractionHandler {
                database_path,
                wheel_cooldown_seconds,
                discord,
                player_names: Arc::new(DiscordIdPlayerNameResolver),
                clock,
                command_cooldowns: Mutex::new(BTreeMap::new()),
                board_expirations: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    #[must_use]
    pub fn with_player_names(mut self, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        Arc::get_mut(&mut self.handler)
            .expect("new mana provider owns its handler")
            .player_names = player_names;
        self
    }
}

impl RegistrationProvider for ManaRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "mana".to_owned(),
            description: "Check your daily mana land assignment".to_owned(),
            options: vec![
                CommandOptionSpec::new(
                    "user",
                    "View another player's mana (optional)",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "all",
                    "Show the full guild's current mana assignments",
                    CommandOptionKind::Boolean,
                ),
            ],
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ManaMoment {
    pub now: i64,
    pub la_offset_seconds: i32,
}

pub(crate) trait ManaClock: Send + Sync {
    fn moment(&self) -> Result<ManaMoment, String>;
}

#[derive(Debug)]
pub(crate) struct SystemManaClock;

impl ManaClock for SystemManaClock {
    fn moment(&self) -> Result<ManaMoment, String> {
        let now = unix_timestamp()?;
        Ok(ManaMoment {
            now,
            la_offset_seconds: pacific_offset_seconds(now)?,
        })
    }
}

impl ManaMoment {
    fn local(self) -> Result<DateTime<FixedOffset>, String> {
        let offset = FixedOffset::east_opt(self.la_offset_seconds)
            .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?;
        DateTime::<Utc>::from_timestamp(self.now, 0)
            .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())
            .map(|time| time.with_timezone(&offset))
    }

    pub(crate) fn today(self) -> Result<String, String> {
        self.local().map(mana_day_label)
    }

    pub(crate) fn day_start(self) -> Result<i64, String> {
        self.local().map(mana_day_start_timestamp)
    }
}

struct ManaInteractionHandler {
    database_path: PathBuf,
    wheel_cooldown_seconds: i64,
    discord: Arc<dyn ManaDiscordPort>,
    player_names: Arc<dyn GuildPlayerNameResolver>,
    clock: Arc<dyn ManaClock>,
    command_cooldowns: Mutex<BTreeMap<i64, Vec<Instant>>>,
    board_expirations: Mutex<BTreeMap<(i64, i64), Instant>>,
}

#[async_trait]
impl InteractionHandler for ManaInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = match request {
            InteractionRequest::Command { ref name, .. } if name == "mana" => {
                self.command(request, responder).await
            }
            InteractionRequest::Component { .. } => self.component(request, responder).await,
            InteractionRequest::Command { name, .. } => {
                Err(format!("mana handler received command {name:?}"))
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("mana extension received an unsupported interaction".to_owned())
            }
        };
        result.map_err(Into::into)
    }
}

impl ManaInteractionHandler {
    async fn command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            options,
            ..
        } = request
        else {
            return Err("invalid mana command payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        if let Some(retry_after) = self.take_command_rate_limit(user_id)? {
            // discord.py's global handler intentionally hides cooldown detail.
            return Err(format!("mana command is on cooldown for {retry_after:.3}s"));
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let channel_id = channel_id
            .map(|value| signed_id(value, "channel"))
            .transpose()?;

        if !self
            .require_gamba(user_id, guild_id, channel_id, &responder)
            .await?
        {
            return Ok(());
        }
        if responder.defer(false).await.is_err() {
            return Ok(());
        }

        if boolean_option(&options, "all").unwrap_or(false) {
            return self.board_command(user_id, guild_id, responder).await;
        }
        let target = user_option(&options, "user");
        let target_id = target.as_ref().map_or(user_id, |target| target.user_id);
        let target_name = target.as_ref().map_or_else(
            || user_display_name.clone(),
            |target| target.display_name.clone(),
        );
        let mut member = self
            .discord
            .mana_guild_member(guild_id, target_id)
            .await?
            .unwrap_or_else(|| ManaGuildMember {
                user_id: target_id,
                display_name: target_name,
                avatar_url: None,
                role_names: Vec::new(),
            });
        self.render_member_names(guild_id, std::slice::from_mut(&mut member))
            .await?;
        self.single_command(user_id, guild_id, member, responder)
            .await
    }

    async fn render_member_names(
        &self,
        guild_id: i64,
        members: &mut [ManaGuildMember],
    ) -> Result<(), String> {
        let player_ids = members
            .iter()
            .map(|member| member.user_id)
            .collect::<Vec<_>>();
        let names = self
            .player_names
            .names_for_guild(guild_id, &player_ids)
            .unwrap_or_default();
        for member in members {
            if names.contains(member.user_id) {
                member.display_name = names.resolve(member.user_id);
            }
        }
        Ok(())
    }

    async fn single_command(
        &self,
        _invoker_id: i64,
        guild_id: i64,
        member: ManaGuildMember,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let moment = self.clock.moment()?;
        let today = moment.today()?;
        let target_id = member.user_id;
        let path = self.database_path.clone();
        let data_path = path.clone();
        let today_for_read = today.clone();
        let details = spawn_sqlite("mana current read", move || {
            let repository = ManaRepository::new(path);
            let mut service = live_service(repository, &data_path, moment);
            service.get_current_mana(target_id, guild_id, &today_for_read)
        })
        .await?;

        let response = if let Some(details) = details {
            let next_spin = self.next_wheel_spin_at(target_id, guild_id).await?;
            InteractionResponse::message("").embed(single_embed(
                &member,
                &details,
                &today,
                Some(next_spin),
                0,
            ))
        } else {
            InteractionResponse::message("").embed(
                InteractionEmbed::titled(format!("🔮 Daily Mana — {}", member.display_name))
                    .description("This player hasn't been assigned any mana yet.")
                    .color(0x95_A5_A6),
            )
        };
        followup(&responder, response).await
    }

    async fn read_board_rows(
        &self,
        guild_id: i64,
        _moment: ManaMoment,
    ) -> Result<Vec<BoardRow>, String> {
        let path = self.database_path.clone();
        let rows = spawn_sqlite("mana board read", move || {
            ManaRepository::new(path)
                .get_all_mana(Some(guild_id))
                .map_err(|error| error.to_string())
                .and_then(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            Ok(BoardRow {
                                discord_id: row.discord_id,
                                land: Land::from_name(&row.mana.current_land).ok_or_else(|| {
                                    format!("unknown mana land {:?}", row.mana.current_land)
                                })?,
                                assigned_date: row.mana.assigned_date,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
        })
        .await?;
        Ok(rows)
    }

    async fn board_command(
        &self,
        owner_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let moment = self.clock.moment()?;
        let today = moment.today()?;
        let mut members = self.discord.mana_guild_members(guild_id)?;
        self.render_member_names(guild_id, &mut members).await?;
        let rows = self.read_board_rows(guild_id, moment).await?;
        let pages = board_pages(&rows, &members, &today);
        let mut response = InteractionResponse::message("").embed(pages[0].clone());
        if pages.len() > 1 {
            response = response.action_row(board_controls(owner_id, 0, pages.len()));
            let now = Instant::now();
            let mut expirations = self
                .board_expirations
                .lock()
                .map_err(|_| "mana board expiration lock was poisoned".to_owned())?;
            expirations.retain(|_, deadline| *deadline > now);
            expirations.insert((guild_id, owner_id), now + VIEW_TIMEOUT);
        }
        followup(&responder, response).await
    }

    async fn component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            user_id,
            guild_id,
            ..
        } = request
        else {
            return Err("invalid mana component payload".to_owned());
        };
        let (owner_id, requested_page) = parse_board_component(&custom_id)?;
        let user_id = signed_id(user_id, "user")?;
        if user_id != owner_id {
            return respond_ephemeral(
                &responder,
                "Only the person who ran this command can page it.",
            )
            .await;
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let expired = {
            let expirations = self
                .board_expirations
                .lock()
                .map_err(|_| "mana board expiration lock was poisoned".to_owned())?;
            expirations
                .get(&(guild_id, owner_id))
                .is_none_or(|deadline| Instant::now() >= *deadline)
        };
        if expired {
            return Ok(());
        }

        let moment = self.clock.moment()?;
        let today = moment.today()?;
        let mut members = self.discord.mana_guild_members(guild_id)?;
        self.render_member_names(guild_id, &mut members).await?;
        let rows = self.read_board_rows(guild_id, moment).await?;
        let pages = board_pages(&rows, &members, &today);
        let page = requested_page.min(pages.len().saturating_sub(1));
        responder
            .update(
                InteractionResponse::message("")
                    .embed(pages[page].clone())
                    .action_row(board_controls(owner_id, page, pages.len())),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn require_gamba(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let valid = match channel_id {
            Some(channel_id) => self
                .discord
                .mana_channel_is_gamba(guild_id, channel_id)
                .await
                .unwrap_or(false),
            None => false,
        };
        if valid {
            return Ok(true);
        }
        let path = self.database_path.clone();
        spawn_sqlite("mana gamba-channel penalty", move || {
            PredictionRepository::new(path)
                .debit_gamba_channel_penalty(user_id, Some(guild_id))
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await?;
        respond_ephemeral(responder, WRONG_CHANNEL_MESSAGE).await?;
        Ok(false)
    }

    fn take_command_rate_limit(&self, user_id: i64) -> Result<Option<f64>, String> {
        let now = Instant::now();
        let mut cooldowns = self
            .command_cooldowns
            .lock()
            .map_err(|_| "mana command cooldown lock was poisoned".to_owned())?;
        for claims in cooldowns.values_mut() {
            claims.retain(|claimed| now.saturating_duration_since(*claimed) < COMMAND_PERIOD);
        }
        cooldowns.retain(|_, claims| !claims.is_empty());
        let claims = cooldowns.entry(user_id).or_default();
        if claims.len() >= COMMAND_RATE {
            let retry = COMMAND_PERIOD - now.saturating_duration_since(claims[0]);
            return Ok(Some(retry.as_secs_f64()));
        }
        claims.push(now);
        Ok(None)
    }

    async fn next_wheel_spin_at(&self, user_id: i64, guild_id: i64) -> Result<i64, String> {
        let now = self.clock.moment()?.now;
        let cooldown = self.wheel_cooldown_seconds;
        let path = self.database_path.clone();
        let last = spawn_sqlite("mana wheel cooldown read", move || {
            PlayerRepository::new(path)
                .get_last_wheel_spin(user_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(last.map_or(now, |last| {
            now.max(last.saturating_add(cooldown).saturating_add(1))
        }))
    }
}

pub(crate) async fn pay_batch_stipends_sqlite(
    path: PathBuf,
    board: &AssignmentBoard,
    guild_id: i64,
    white_stipend: i64,
) {
    if white_stipend <= 0 {
        return;
    }
    let ids = board
        .assignments
        .iter()
        .filter(|assignment| assignment.details.land == Land::Plains)
        .map(|assignment| assignment.discord_id)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    let _ = spawn_sqlite("guild mana White stipends", move || {
        LoanRepository::new(path)
            .distribute_nonprofit_stipends_atomic(&ids, Some(guild_id), white_stipend)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await;
}

#[derive(Clone)]
pub(crate) struct SqliteManaData {
    players: PlayerRepository,
    gambling: GamblingStatsRepository,
    bankruptcy: BankruptcyRepository,
    tips: TipRepository,
    loaded_histories: BTreeMap<(i64, i64), Vec<BetHistoryEntry>>,
}

impl SqliteManaData {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            players: PlayerRepository::new(&path),
            gambling: GamblingStatsRepository::new(&path),
            bankruptcy: BankruptcyRepository::new(&path),
            tips: TipRepository::new(path),
            loaded_histories: BTreeMap::new(),
        }
    }
}

impl ManaDataSource for SqliteManaData {
    fn player(
        &mut self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<PlayerProfile>, ManaServiceError> {
        self.players
            .get_by_id(discord_id, Some(guild_id))
            .map(|player| player.map(player_profile))
            .map_err(data_error)
    }

    fn players(&mut self, guild_id: i64) -> Result<Vec<PlayerProfile>, ManaServiceError> {
        self.players
            .get_all(Some(guild_id))
            .map(|players| players.into_iter().map(player_profile).collect())
            .map_err(data_error)
    }

    fn lowest_balance(
        &mut self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<i64>, ManaServiceError> {
        self.players
            .get_lowest_balance(discord_id, Some(guild_id))
            .map_err(data_error)
    }

    fn bet_history(
        &mut self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<BetOutcome>, ManaServiceError> {
        let history = self
            .gambling
            .player_bet_history(discord_id, Some(guild_id))
            .map_err(data_error)?;
        let outcomes = history
            .iter()
            .map(|bet| match bet.outcome {
                GamblingOutcome::Won => BetOutcome::Won,
                GamblingOutcome::Lost | GamblingOutcome::Neutral => BetOutcome::Lost,
            })
            .collect();
        self.loaded_histories
            .insert((discord_id, guild_id), history);
        Ok(outcomes)
    }

    fn degen_score(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        history: &[BetOutcome],
    ) -> Result<DegenScore, ManaServiceError> {
        let _ = history;
        let loaded = self.loaded_histories.remove(&(discord_id, guild_id));
        GamblingStatsService::new(self.gambling.clone())
            .calculate_degen_score_with_data(discord_id, Some(guild_id), loaded, None)
            .map(degen_score)
            .map_err(data_error)
    }

    fn bankruptcy_state(
        &mut self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<BankruptcyState, ManaServiceError> {
        self.bankruptcy
            .get_state(discord_id, Some(guild_id))
            .map(|state| {
                state.map_or_else(BankruptcyState::default, |state| BankruptcyState {
                    penalty_games_remaining: state.penalty_games_remaining,
                    last_bankruptcy_at: state.last_bankruptcy_at,
                })
            })
            .map_err(data_error)
    }

    fn tip_stats(&mut self, discord_id: i64, guild_id: i64) -> Result<TipStats, ManaServiceError> {
        self.tips
            .get_user_tip_stats(discord_id, Some(guild_id))
            .map(|stats| TipStats {
                total_sent: stats.total_sent,
                tips_sent_count: stats.tips_sent_count,
            })
            .map_err(data_error)
    }

    fn lowest_balances_bulk(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, Option<i64>>, ManaServiceError> {
        self.gambling
            .lowest_balances_bulk(discord_ids, Some(guild_id))
            .map_err(data_error)
    }

    fn degen_scores_bulk(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, DegenScore>, ManaServiceError> {
        GamblingStatsService::new(self.gambling.clone())
            .calculate_degen_scores_bulk(discord_ids, Some(guild_id))
            .map(|scores| {
                scores
                    .into_iter()
                    .map(|(discord_id, score)| (discord_id, degen_score(score)))
                    .collect()
            })
            .map_err(data_error)
    }

    fn bankruptcy_states_bulk(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, BankruptcyState>, ManaServiceError> {
        self.bankruptcy
            .get_bulk_states(discord_ids, Some(guild_id))
            .map(|states| {
                states
                    .into_iter()
                    .map(|(discord_id, state)| {
                        (
                            discord_id,
                            BankruptcyState {
                                penalty_games_remaining: state.penalty_games_remaining,
                                last_bankruptcy_at: state.last_bankruptcy_at,
                            },
                        )
                    })
                    .collect()
            })
            .map_err(data_error)
    }

    fn tip_stats_bulk(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, TipStats>, ManaServiceError> {
        self.tips
            .get_user_tip_stats_bulk(discord_ids, Some(guild_id))
            .map(|stats| {
                stats
                    .into_iter()
                    .map(|(discord_id, stats)| {
                        (
                            discord_id,
                            TipStats {
                                total_sent: stats.total_sent,
                                tips_sent_count: stats.tips_sent_count,
                            },
                        )
                    })
                    .collect()
            })
            .map_err(data_error)
    }

    fn current_streaks_bulk(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, i64>, ManaServiceError> {
        self.gambling
            .current_bet_streaks_bulk(discord_ids, Some(guild_id))
            .map_err(data_error)
    }
}

#[derive(Clone)]
pub(crate) struct SqliteGuardianProtection {
    repository: ManaProtectionRepository,
    moment: ManaMoment,
}

impl GuardianProtection for SqliteGuardianProtection {
    fn reconcile_guardian(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        since: i64,
    ) -> Result<i64, ManaServiceError> {
        let today = self.moment.today().map_err(ManaServiceError::Data)?;
        self.repository
            .reconcile_guardian(discord_id, Some(guild_id), since, &today, self.moment.now)
            .map_err(data_error)
    }

    fn reconcile_guardians(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
        since: i64,
    ) -> Result<BTreeMap<i64, i64>, ManaServiceError> {
        let today = self.moment.today().map_err(ManaServiceError::Data)?;
        self.repository
            .reconcile_guardians(discord_ids, Some(guild_id), since, &today, self.moment.now)
            .map_err(data_error)
    }
}

pub(crate) type LiveManaService =
    ManaService<ManaRepository, SqliteManaData, XorShift64Entropy, SqliteGuardianProtection>;

pub(crate) fn live_service(
    repository: ManaRepository,
    path: &Path,
    moment: ManaMoment,
) -> LiveManaService {
    let seed = u64::from_ne_bytes(moment.now.to_ne_bytes()) ^ fastrand::u64(..);
    ManaService::new(
        repository,
        SqliteManaData::new(path),
        XorShift64Entropy::new(seed),
        SqliteGuardianProtection {
            repository: ManaProtectionRepository::new(path),
            moment,
        },
    )
}

fn player_profile(player: cama_domain::player::Player) -> PlayerProfile {
    PlayerProfile {
        discord_id: player.discord_id,
        balance: player.jopacoin_balance,
        wins: player.wins,
        losses: player.losses,
        glicko_rating: player.glicko_rating,
    }
}

fn degen_score(score: cama_db::gambling_stats_repository::DegenScoreBreakdown) -> DegenScore {
    DegenScore {
        total: score.total,
        max_leverage_score: score.max_leverage_score,
        loss_chase_score: score.loss_chase_score,
        bet_size_score: score.bet_size_score,
        negative_loan_bonus: score.negative_loan_bonus,
        bankruptcy_score: score.bankruptcy_score,
        debt_depth_score: score.debt_depth_score,
    }
}

fn single_embed(
    member: &ManaGuildMember,
    mana: &ManaDetails,
    today: &str,
    next_wheel_spin_at: Option<i64>,
    stipend_paid: i64,
) -> InteractionEmbed {
    let mut land_value = format!("{} **{}** · {} Mana", mana.emoji, mana.land, mana.color);
    if let Some(next) = next_wheel_spin_at {
        land_value.push_str(&format!("\nNext wheel spin <t:{next}:R>"));
    }
    let mut embed = InteractionEmbed::titled(format!("🔮 Daily Mana — {}", member.display_name))
        .color(mana.land.embed_color())
        .field("Land", land_value, false);
    if mana.land == Land::Plains && mana.assigned_date == today && !mana.consumed {
        let retro = if mana.retro_refund > 0 {
            format!(
                " Recovered **{} JC** from earlier hostile losses.",
                mana.retro_refund
            )
        } else {
            String::new()
        };
        embed = embed.field(
            "🌾 Guardian Aura",
            format!(
                "Absorbs 50% of hostile JC losses until the 4 AM reset (**{}/25 JC** capacity remaining).{}",
                mana.guardian_remaining, retro
            ),
            false,
        );
    }
    if stipend_paid > 0 {
        embed = embed.field(
            "Stipend",
            format!("+{stipend_paid} JC (Jopacoin Reserve)"),
            false,
        );
    }
    if let Some(avatar) = &member.avatar_url {
        embed = embed.thumbnail(avatar.clone());
    }
    embed
}

fn board_pages(
    rows: &[BoardRow],
    members: &[ManaGuildMember],
    today: &str,
) -> Vec<InteractionEmbed> {
    let names = members
        .iter()
        .map(|member| (member.user_id, member.display_name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut sorted = rows.to_vec();
    sorted.sort_by(|left, right| {
        let left_rank = Land::ORDERED
            .iter()
            .position(|land| *land == left.land)
            .unwrap_or(usize::MAX);
        let right_rank = Land::ORDERED
            .iter()
            .position(|land| *land == right.land)
            .unwrap_or(usize::MAX);
        let left_name = names
            .get(&left.discord_id)
            .copied()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let right_name = names
            .get(&right.discord_id)
            .copied()
            .unwrap_or_default()
            .to_ascii_lowercase();
        (left_rank, left_name).cmp(&(right_rank, right_name))
    });
    let lines = sorted
        .iter()
        .map(|row| {
            let name = names.get(&row.discord_id).map_or_else(
                || format!("<@{}>", row.discord_id),
                |name| (*name).to_owned(),
            );
            format!("{} **{}** · {name}", row.land.emoji(), row.land)
        })
        .collect::<Vec<_>>();
    let chunks = if lines.is_empty() {
        vec![Vec::new()]
    } else {
        lines.chunks(PAGE_SIZE).map(<[String]>::to_vec).collect()
    };
    let today_count = rows.iter().filter(|row| row.assigned_date == today).count();
    let page_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(page, chunk)| {
            InteractionEmbed::titled("🔮 Guild Mana Board")
                .description(if chunk.is_empty() {
                    "No mana assigned yet.".to_owned()
                } else {
                    chunk.join("\n")
                })
                .color(0x9B_59_B6)
                .footer(format!(
                    "Page {}/{page_count} · {today_count}/{} assigned today · Resets 4 AM PST",
                    page + 1,
                    rows.len()
                ))
        })
        .collect()
}

fn board_controls(owner_id: i64, page: usize, pages: usize) -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(
            format!("{COMPONENT_PREFIX}{owner_id}:{}", page.saturating_sub(1)),
            "◀ Prev",
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(page == 0),
        InteractionButton::new(
            format!("{COMPONENT_PREFIX}{owner_id}:{}", page + 1),
            "Next ▶",
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(page + 1 >= pages),
    ])
}

fn parse_board_component(custom_id: &str) -> Result<(i64, usize), String> {
    let suffix = custom_id
        .strip_prefix(COMPONENT_PREFIX)
        .ok_or_else(|| "invalid mana board component".to_owned())?;
    let mut parts = suffix.split(':');
    let owner = parts
        .next()
        .ok_or_else(|| "missing mana board owner".to_owned())?
        .parse::<i64>()
        .map_err(|_| "invalid mana board owner".to_owned())?;
    let page = parts
        .next()
        .ok_or_else(|| "missing mana board page".to_owned())?
        .parse::<usize>()
        .map_err(|_| "invalid mana board page".to_owned())?;
    if parts.next().is_some() {
        return Err("invalid mana board component".to_owned());
    }
    Ok((owner, page))
}

#[derive(Clone, Debug)]
struct UserOption {
    user_id: i64,
    display_name: String,
}

fn user_option(options: &[InteractionOption], name: &str) -> Option<UserOption> {
    options.iter().find_map(|option| {
        if option.name != name {
            return None;
        }
        let crate::registration::InteractionValue::User {
            id, display_name, ..
        } = &option.value
        else {
            return None;
        };
        Some(UserOption {
            user_id: i64::try_from(*id).ok()?,
            display_name: display_name.clone().unwrap_or_else(|| format!("<@{id}>")),
        })
    })
}

fn boolean_option(options: &[InteractionOption], name: &str) -> Option<bool> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                crate::registration::InteractionValue::Boolean(value) => Some(*value),
                _ => None,
            })
    })
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} id exceeds SQLite range"))
}

fn data_error(error: impl std::fmt::Display) -> ManaServiceError {
    ManaServiceError::Data(error.to_string())
}

fn unix_timestamp() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system clock exceeds SQLite range".to_owned())
}

/// Resolve America's DST rule without adding a time-zone database dependency.
/// Post-2007 US rules are valid for every supported production timestamp.
fn pacific_offset_seconds(timestamp: i64) -> Result<i32, String> {
    use chrono::{Datelike, NaiveDate, TimeZone, Weekday};

    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?;
    let year = utc.year();
    let march =
        NaiveDate::from_ymd_opt(year, 3, 1).ok_or_else(|| "invalid March date".to_owned())?;
    let november =
        NaiveDate::from_ymd_opt(year, 11, 1).ok_or_else(|| "invalid November date".to_owned())?;
    let to_sunday = |date: NaiveDate| -> i64 {
        match date.weekday() {
            Weekday::Sun => 0,
            Weekday::Mon => 6,
            Weekday::Tue => 5,
            Weekday::Wed => 4,
            Weekday::Thu => 3,
            Weekday::Fri => 2,
            Weekday::Sat => 1,
        }
    };
    let second_sunday_march = march + chrono::Duration::days(to_sunday(march) + 7);
    let first_sunday_november = november + chrono::Duration::days(to_sunday(november));
    let standard = FixedOffset::west_opt(8 * 3_600)
        .ok_or_else(|| "invalid Pacific standard offset".to_owned())?;
    let daylight = FixedOffset::west_opt(7 * 3_600)
        .ok_or_else(|| "invalid Pacific daylight offset".to_owned())?;
    let starts = standard
        .from_local_datetime(
            &second_sunday_march
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST start time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST start".to_owned())?
        .timestamp();
    let ends = daylight
        .from_local_datetime(
            &first_sunday_november
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST end time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST end".to_owned())?
        .timestamp();
    Ok(if (starts..ends).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    })
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string())
}

async fn followup(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), String> {
    responder
        .followup(response)
        .await
        .map_err(|error| error.to_string())
}

async fn spawn_sqlite<T, E, F>(label: &'static str, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    crate::ids::blocking(label, move || {
        operation().map_err(|error| format!("{label} failed: {error}"))
    })
    .await
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "mana_provider/tests.rs"]
mod tests;
