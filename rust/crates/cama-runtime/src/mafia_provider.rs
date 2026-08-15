//! Private production Mafia runtime.
//!
//! The module is intentionally private until migrated SQLite and Discord
//! recovery gates are complete.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};
use async_trait::async_trait;
use cama_app::mafia_flavor::MafiaFlavorService;
use cama_app::mafia_service::{compute_payouts, role_counts};
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::core_repositories::PlayerRepository;
use cama_db::mafia_repository::{
    AddBountyResult, MafiaAction, MafiaActionType, MafiaCancelResult, MafiaGame, MafiaHistoryRow,
    MafiaPhase, MafiaPlayer, MafiaRepository, MafiaRepositoryError, MafiaRole, MafiaTwist,
    MafiaWinner, ThreadIds,
};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::game_date::{game_date_for_timestamp, get_game_date, weekday_of_game_date};
use serde::{Deserialize, Serialize};
use tracing::warn;

const COMPONENT_PREFIX: &str = "mafia:";
const NIGHT_SECONDS: i64 = 24 * 60 * 60;
const DAY_SECONDS: i64 = 24 * 60 * 60;
const REMINDER_SECONDS: i64 = 8 * 60 * 60;
const LOOP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MIN_ROSTER: usize = 5;
const MAX_ROSTER: usize = 15;
const MAX_CYCLES: i64 = 14;
const JESTER_PROBABILITY: f64 = 0.20;
const BOOKIE_PROBABILITY: f64 = 0.20;
const TWIST_PROBABILITY: f64 = 0.30;
const ENTRY_FEE: i64 = 8;
const MVP_BONUS: i64 = 20;
const MAX_PAYOUT: i64 = 50;
const ADMINISTRATOR: u64 = 1 << 3;
const MANAGE_GUILD: u64 = 1 << 5;
const REMINDER_COOLDOWN: Duration = Duration::from_secs(60);

/// Mafia-specific Discord operations kept outside the command policy.
///
/// Unsupported transports fail private-thread creation explicitly.  They must
/// never downgrade a Mafia conversation to a public thread: the roster and
/// role information are private game state.
#[async_trait]
pub trait MafiaDiscordPort: Send + Sync {
    async fn create_private_thread(
        &self,
        channel_id: u64,
        name: &str,
        member_ids: &[u64],
    ) -> Result<Option<u64>, String>;

    async fn create_public_thread(
        &self,
        channel_id: u64,
        parent_message_id: u64,
        name: &str,
    ) -> Result<u64, String>;

    async fn add_private_thread_members(
        &self,
        thread_id: u64,
        member_ids: &[u64],
    ) -> Result<(), String>;

    async fn send_thread_message(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String>;

    /// Send private-thread copy and return its durable Discord message ID
    /// when the transport can provide one. The default preserves older test
    /// adapters; production Serenity returns the actual receipt.
    async fn send_thread_message_with_receipt(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<Option<u64>, String> {
        self.send_thread_message(thread_id, message).await?;
        Ok(None)
    }

    async fn archive_thread(&self, thread_id: u64, name: &str) -> Result<(), String>;

    /// Resolve the configured public post channel, falling back to the first
    /// sendable `gamba` text channel when the dedicated channel is absent.
    async fn resolve_public_channel(
        &self,
        _guild_id: u64,
        configured_channel_id: u64,
    ) -> Result<Option<u64>, String> {
        Ok(Some(configured_channel_id).filter(|id| *id != 0))
    }

    /// Return a thread's parent channel, if it is a thread under a guild
    /// channel.  This is part of the command gate as well as post routing.
    async fn parent_channel_id(
        &self,
        _guild_id: u64,
        _channel_id: u64,
    ) -> Result<Option<u64>, String> {
        Ok(None)
    }
}

struct DefaultMafiaDiscordPort {
    transport: Arc<dyn DiscordTransport>,
}

#[async_trait]
impl MafiaDiscordPort for DefaultMafiaDiscordPort {
    async fn create_private_thread(
        &self,
        channel_id: u64,
        name: &str,
        member_ids: &[u64],
    ) -> Result<Option<u64>, String> {
        self.transport
            .create_private_thread(channel_id, name, member_ids)
            .await
            .map(Some)
    }

    async fn create_public_thread(
        &self,
        channel_id: u64,
        parent_message_id: u64,
        name: &str,
    ) -> Result<u64, String> {
        self.transport
            .create_public_thread(channel_id, parent_message_id, name)
            .await
    }

    async fn add_private_thread_members(
        &self,
        thread_id: u64,
        member_ids: &[u64],
    ) -> Result<(), String> {
        self.transport
            .add_private_thread_members(thread_id, member_ids)
            .await
    }

    async fn send_thread_message(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.transport
            .send_message(thread_id, message)
            .await
            .map(|_| ())
    }

    async fn send_thread_message_with_receipt(
        &self,
        thread_id: u64,
        message: DiscordMessage,
    ) -> Result<Option<u64>, String> {
        self.transport
            .send_message(thread_id, message)
            .await
            .map(|receipt| Some(receipt.message_id))
    }

    async fn archive_thread(&self, thread_id: u64, name: &str) -> Result<(), String> {
        self.transport.archive_thread(thread_id, name, true).await
    }

    async fn resolve_public_channel(
        &self,
        guild_id: u64,
        configured_channel_id: u64,
    ) -> Result<Option<u64>, String> {
        if configured_channel_id != 0
            && self
                .transport
                .mafia_channel_available(guild_id, configured_channel_id)
                .await?
        {
            return Ok(Some(configured_channel_id));
        }
        self.transport.mafia_gamba_channel_id(guild_id).await
    }

    async fn parent_channel_id(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<Option<u64>, String> {
        self.transport.channel_parent_id(guild_id, channel_id).await
    }
}

#[derive(Clone)]
pub struct MafiaRegistrationProvider {
    handler: Arc<MafiaInteractionHandler>,
}

impl MafiaRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<std::path::Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        let mafia_discord: Arc<dyn MafiaDiscordPort> = Arc::new(DefaultMafiaDiscordPort {
            transport: Arc::clone(&discord),
        });
        Self::new_with_mafia_port(database_path, config, vanity_tax, discord, mafia_discord)
    }

    #[must_use]
    pub fn new_with_mafia_port(
        database_path: impl AsRef<std::path::Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn DiscordTransport>,
        mafia_discord: Arc<dyn MafiaDiscordPort>,
    ) -> Self {
        let path = database_path.as_ref().to_path_buf();
        Self {
            handler: Arc::new(MafiaInteractionHandler {
                repository: MafiaRepository::new(&path),
                players: PlayerRepository::new(&path),
                flavor: MafiaFlavorService::default(),
                vanity_tax,
                bankruptcy_penalty_rate: config.values.bankruptcy_penalty_rate,
                vanity_tax_rate: config.values.vanity_tax_rate,
                discord,
                mafia_discord,
                channel_id: config.channels.mafia,
                admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
                max_debt: config.values.max_debt.max(0),
                reminders: Mutex::new(BTreeMap::new()),
                setup_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(MafiaGatewayObserver {
            handler: self.handler.clone(),
        })
    }

    #[must_use]
    pub fn worker_spec(
        &self,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
    ) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new(
            "mafia_phase_loop",
            Arc::new(MafiaPhaseWorker {
                handler: Arc::clone(&self.handler),
                guild_source,
                wake_interval: LOOP_INTERVAL,
            }),
        )
    }
}

impl RegistrationProvider for MafiaRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "mafia".to_owned(),
            description: "Mafia subgame".to_owned(),
            options: mafia_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct MafiaGatewayObserver {
    handler: Arc<MafiaInteractionHandler>,
}

#[async_trait]
impl GatewayEventObserver for MafiaGatewayObserver {
    fn name(&self) -> &'static str {
        "mafia-ready-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        self.handler.recover(context.guild_ids()).await
    }
}

struct MafiaPhaseWorker {
    handler: Arc<MafiaInteractionHandler>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    wake_interval: Duration,
}

#[async_trait]
impl BackgroundWorker for MafiaPhaseWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            for guild_id in self.guild_source.live_guild_ids()? {
                if context.shutdown_requested() {
                    return Ok(());
                }
                if let Err(error) = self.handler.tick_guild(guild_id).await {
                    warn!(guild_id, %error, "Mafia phase tick failed");
                }
            }
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

struct MafiaInteractionHandler {
    repository: MafiaRepository,
    players: PlayerRepository,
    flavor: MafiaFlavorService,
    vanity_tax: Arc<PersistentVanityTaxService>,
    bankruptcy_penalty_rate: f64,
    vanity_tax_rate: f64,
    discord: Arc<dyn DiscordTransport>,
    mafia_discord: Arc<dyn MafiaDiscordPort>,
    channel_id: i64,
    admin_user_ids: BTreeSet<i64>,
    max_debt: i64,
    reminders: Mutex<BTreeMap<i64, Instant>>,
    setup_lock: tokio::sync::Mutex<()>,
}

#[derive(Clone, Debug)]
struct CommandContext {
    actor_id: i64,
    guild_id: Option<i64>,
    channel_id: Option<i64>,
    permissions: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ActionReply {
    ok: bool,
    error: Option<String>,
    result: Option<String>,
    changed: bool,
    action_type: Option<MafiaActionType>,
}

#[derive(Clone, Debug, Default)]
struct NightSummary {
    resolved: bool,
    killed: Vec<(i64, &'static str)>,
    revived_id: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct DaySummary {
    resolved: bool,
    continued: bool,
    game_id: i64,
    day_number: i64,
    winner: Option<MafiaWinner>,
    lynched_id: Option<i64>,
    mvp_id: Option<i64>,
    mvp_bonus: i64,
    payout_per_winner: i64,
    pot_total: i64,
    winning_ids: Vec<i64>,
    bookie_id: Option<i64>,
    bookie_payout: i64,
    nonprofit_overflow: i64,
    bounty_reward: i64,
    bankruptcy_penalties: BTreeMap<i64, i64>,
    vanity_taxes: BTreeMap<i64, i64>,
    vote_breakdown: BTreeMap<i64, i64>,
    vote_detail: Vec<(i64, i64)>,
    twist: Option<MafiaTwist>,
}

/// Durable, JSON-compatible terminal summary stored before the public
/// resolution send. This is deliberately independent from Discord message
/// IDs so a restart can reproduce the exact settled copy and payout fields.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ResolutionReceipt {
    resolved: bool,
    game_id: i64,
    day_number: i64,
    winner: Option<String>,
    lynched_id: Option<i64>,
    mvp_id: Option<i64>,
    mvp_bonus: i64,
    payout_per_winner: i64,
    pot_total: i64,
    winning_ids: Vec<i64>,
    bookie_id: Option<i64>,
    bookie_payout: i64,
    nonprofit_overflow: i64,
    bounty_reward: i64,
    bankruptcy_penalties: BTreeMap<i64, i64>,
    vanity_taxes: BTreeMap<i64, i64>,
    vote_breakdown: BTreeMap<i64, i64>,
    vote_detail: Vec<(i64, i64)>,
    twist: Option<String>,
}

impl ResolutionReceipt {
    fn from_summary(summary: &DaySummary) -> Self {
        Self {
            resolved: summary.resolved,
            game_id: summary.game_id,
            day_number: summary.day_number,
            winner: summary.winner.map(|winner| winner.as_str().to_owned()),
            lynched_id: summary.lynched_id,
            mvp_id: summary.mvp_id,
            mvp_bonus: summary.mvp_bonus,
            payout_per_winner: summary.payout_per_winner,
            pot_total: summary.pot_total,
            winning_ids: summary.winning_ids.clone(),
            bookie_id: summary.bookie_id,
            bookie_payout: summary.bookie_payout,
            nonprofit_overflow: summary.nonprofit_overflow,
            bounty_reward: summary.bounty_reward,
            bankruptcy_penalties: summary.bankruptcy_penalties.clone(),
            vanity_taxes: summary.vanity_taxes.clone(),
            vote_breakdown: summary.vote_breakdown.clone(),
            vote_detail: summary.vote_detail.clone(),
            twist: summary.twist.map(|twist| twist.as_str().to_owned()),
        }
    }

    fn into_summary(self) -> Result<DaySummary, String> {
        let winner = self.winner.as_deref().map(parse_winner).transpose()?;
        let twist = self.twist.as_deref().map(parse_twist).transpose()?;
        Ok(DaySummary {
            resolved: self.resolved,
            game_id: self.game_id,
            day_number: self.day_number,
            winner,
            lynched_id: self.lynched_id,
            mvp_id: self.mvp_id,
            mvp_bonus: self.mvp_bonus,
            payout_per_winner: self.payout_per_winner,
            pot_total: self.pot_total,
            winning_ids: self.winning_ids,
            bookie_id: self.bookie_id,
            bookie_payout: self.bookie_payout,
            nonprofit_overflow: self.nonprofit_overflow,
            bounty_reward: self.bounty_reward,
            bankruptcy_penalties: self.bankruptcy_penalties,
            vanity_taxes: self.vanity_taxes,
            vote_breakdown: self.vote_breakdown,
            vote_detail: self.vote_detail,
            twist,
            ..DaySummary::default()
        })
    }
}

#[async_trait]
impl InteractionHandler for MafiaInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                guild_id,
                channel_id,
                member_permissions,
                options,
                ..
            } if name == "mafia" => {
                let context = CommandContext {
                    actor_id: signed_id(user_id, "user")?,
                    guild_id: guild_id.map(|id| signed_id(id, "guild")).transpose()?,
                    channel_id: channel_id.map(|id| signed_id(id, "channel")).transpose()?,
                    permissions: member_permissions,
                };
                self.handle_command(context, &options, responder).await
            }
            InteractionRequest::Component { custom_id, .. }
                if custom_id.starts_with(COMPONENT_PREFIX) =>
            {
                respond_ephemeral(&responder, "This Mafia control is no longer active.")
                    .await
                    .map_err(|error| error.to_string().into())
            }
            InteractionRequest::Component { .. } => {
                Err("mafia handler received an unknown component route".into())
            }
            InteractionRequest::Command { name, .. } => {
                Err(format!("mafia handler received command {:?}", name).into())
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("Mafia has no autocomplete or modal routes".into())
            }
        }
    }
}

impl MafiaInteractionHandler {
    async fn handle_command(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = context.guild_id else {
            return respond_ephemeral(&responder, "This command can only be used in a server.")
                .await
                .map_err(|error| error.to_string().into());
        };
        let (channel_allowed, routed_channel) = self
            .channel_gate(guild_id, context.channel_id)
            .await
            .map_err(InteractionHandlerError::from)?;
        if !channel_allowed {
            let repository = self.repository.clone();
            let actor = context.actor_id;
            let _ = blocking(move || {
                repository
                    .charge_channel_penalty(Some(guild_id), actor)
                    .map_err(to_string)
            })
            .await
            .map_err(InteractionHandlerError::from)?;
            let text = routed_channel
                .filter(|channel_id| {
                    self.channel_id != 0 && *channel_id == self.channel_id
                })
                .map_or_else(
                || {
                    "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.".to_owned()
                },
                |channel_id| format!(
                    "The case is being worked elsewhere — take it to <#{}>. A single jopacoin slips into an informant's pocket as penance.",
                    channel_id
                ),
            );
            return responder
                .respond(
                    InteractionResponse::message(text)
                        .ephemeral()
                        .without_mentions(),
                )
                .await
                .map_err(|error| error.to_string().into());
        }
        let (subcommand, leaf_options) = leaf_command(options)?;
        match subcommand.as_str() {
            "role" => self.role(&context, guild_id, responder).await,
            "act" => self.act(&context, guild_id, leaf_options, responder).await,
            "vote" => self.vote(&context, guild_id, leaf_options, responder).await,
            "bounty" => {
                self.bounty(&context, guild_id, leaf_options, responder)
                    .await
            }
            "join" => self.join(&context, guild_id, responder).await,
            "remind" => self.remind(&context, guild_id, responder).await,
            "status" => self.status(&context, guild_id, responder).await,
            "history" => self.history(&context, guild_id, responder).await,
            "leaderboard" => self.leaderboard(&context, guild_id, responder).await,
            "info" => self.info(responder).await,
            "optout" => self.optout(&context, guild_id, responder).await,
            "optin" => self.optin(&context, guild_id, responder).await,
            "admin" => {
                self.handle_admin(&context, guild_id, leaf_options, responder)
                    .await
            }
            _ => Err(format!("unknown Mafia subcommand {:?}", subcommand).into()),
        }
    }

    async fn channel_gate(
        &self,
        guild_id: i64,
        channel_id: Option<i64>,
    ) -> Result<(bool, Option<i64>), String> {
        let guild_id_u64 = u64::try_from(guild_id)
            .map_err(|_| "Mafia guild id is outside Discord's supported range")?;
        let configured_u64 = u64::try_from(self.channel_id).unwrap_or_default();
        let routed = self
            .mafia_discord
            .resolve_public_channel(guild_id_u64, configured_u64)
            .await?
            .and_then(|id| i64::try_from(id).ok());
        let Some(channel_id) = channel_id else {
            return Ok((false, routed));
        };
        let Some(routed_id) = routed else {
            return Ok((false, None));
        };
        if channel_id == routed_id {
            return Ok((true, Some(routed_id)));
        }
        let parent = self
            .mafia_discord
            .parent_channel_id(
                guild_id_u64,
                u64::try_from(channel_id)
                    .map_err(|_| "Mafia channel id is outside Discord's supported range")?,
            )
            .await?
            .and_then(|id| i64::try_from(id).ok());
        Ok((parent == Some(routed_id), Some(routed_id)))
    }

    async fn public_channel_id(&self, guild_id: i64) -> Result<u64, String> {
        let guild_id = u64::try_from(guild_id)
            .map_err(|_| "Mafia guild id is outside Discord's supported range")?;
        let configured = u64::try_from(self.channel_id).unwrap_or_default();
        self.mafia_discord
            .resolve_public_channel(guild_id, configured)
            .await?
            .ok_or_else(|| "no configured Mafia or fallback gamba channel is available".to_owned())
    }

    fn is_admin(&self, context: &CommandContext) -> bool {
        self.admin_user_ids.contains(&context.actor_id)
            || context
                .permissions
                .is_some_and(|bits| bits & (ADMINISTRATOR | MANAGE_GUILD) != 0)
    }

    fn allow_reminder(&self, actor_id: i64) -> bool {
        let now = Instant::now();
        let mut reminders = self
            .reminders
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if reminders
            .get(&actor_id)
            .is_some_and(|last| now.duration_since(*last) < REMINDER_COOLDOWN)
        {
            return false;
        }
        reminders.insert(actor_id, now);
        true
    }

    async fn role(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let value = blocking(move || {
            let Some(game) = repository
                .get_active_game(Some(guild_id))
                .map_err(to_string)?
            else {
                return Ok(None);
            };
            let players = repository.get_players(game.game_id).map_err(to_string)?;
            let investigations = if players
                .iter()
                .find(|player| player.discord_id == actor)
                .is_some_and(|player| player.role == MafiaRole::Detective)
            {
                repository
                    .get_actions(
                        game.game_id,
                        Some(MafiaActionType::Investigate),
                        Some(MafiaPhase::Night),
                    )
                    .map_err(to_string)?
                    .into_iter()
                    .filter(|action| action.actor_id == actor)
                    .collect()
            } else {
                Vec::new()
            };
            Ok(players
                .iter()
                .find(|player| player.discord_id == actor)
                .cloned()
                .map(|player| (game, player, players, investigations)))
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let Some((game, player, players, investigations)) = value else {
            return respond_ephemeral(
                &responder,
                "You're not in today's mafia game (or no game is active).",
            )
            .await
            .map_err(|error| error.to_string().into());
        };
        let titles = self.titles(guild_id, actor).await?;
        responder
            .respond(
                InteractionResponse::message("")
                    .ephemeral()
                    .embed(role_embed(
                        &game,
                        &player,
                        &players,
                        &investigations,
                        &titles,
                    )),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn act(
        &self,
        context: &CommandContext,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let target = user_option(options, "target").ok_or("/mafia act requires target")?;
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let reply = blocking(move || submit_night_action(&repository, guild_id, actor, target))
            .await
            .map_err(InteractionHandlerError::from)?;
        if !reply.ok {
            return respond_ephemeral(
                &responder,
                reply.error.as_deref().unwrap_or("Action rejected."),
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        let text = match (reply.action_type, reply.result) {
            (Some(MafiaActionType::Investigate), Some(result)) => {
                format!("🕵️ Investigation result for <@{}>: **{}**.", target, result)
            }
            (Some(MafiaActionType::Save), _) => {
                format!("⚕️ You will protect <@{}> tonight.", target)
            }
            (Some(MafiaActionType::VigilanteKill), _) => {
                format!("🔫 Your one shot is locked on <@{}>.", target)
            }
            (Some(MafiaActionType::Wager), _) => format!(
                "🎰 Ticket placed: you're calling <@{}> to swing from the rax today. Call it right and the house pays out.",
                target
            ),
            _ => format!("🔪 Kill vote registered for <@{}>.", target),
        };
        responder
            .respond(
                InteractionResponse::message(text)
                    .ephemeral()
                    .with_user_mentions(vec![unsigned_id(target, "target")?]),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn vote(
        &self,
        context: &CommandContext,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let target = user_option(options, "target").ok_or("/mafia vote requires target")?;
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let reply = blocking(move || submit_day_vote(&repository, guild_id, actor, target))
            .await
            .map_err(InteractionHandlerError::from)?;
        if !reply.ok {
            return respond_ephemeral(
                &responder,
                reply.error.as_deref().unwrap_or("Vote rejected."),
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        let verb = if reply.changed {
            "changed to"
        } else {
            "locked in for"
        };
        responder.respond(InteractionResponse::message(format!("🗳️ Vote {} <@{}>. You can change it until dusk; tallies stay hidden until then.", verb, target)).ephemeral().with_user_mentions(vec![unsigned_id(target, "target")?])).await.map_err(|error| error.to_string().into())
    }

    async fn bounty(
        &self,
        context: &CommandContext,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let target = user_option(options, "target").ok_or("/mafia bounty requires target")?;
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let max_debt = self.max_debt;
        let reply = blocking(move || submit_bounty(&repository, guild_id, actor, target, max_debt))
            .await
            .map_err(InteractionHandlerError::from)?;
        if !reply.ok {
            return respond_ephemeral(
                &responder,
                reply.error.as_deref().unwrap_or("Bounty rejected."),
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        responder.respond(InteractionResponse::message(format!("🎯 Bounty placed: 1 {JOPACOIN_EMOTE} on <@{target}>. If the town lynches them today and they bleed mafia red, the contributors split the bounty (up to the number still alive).")).ephemeral().with_user_mentions(vec![unsigned_id(target, "target")?])).await.map_err(|error| error.to_string().into())
    }

    async fn join(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let players = self.players.clone();
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let joined = blocking(move || {
            if players
                .get_by_id(actor, Some(guild_id))
                .map_err(to_string)?
                .is_none()
            {
                return Ok(false);
            }
            repository
                .add_signup(Some(guild_id), actor)
                .map_err(to_string)?;
            repository
                .set_optout(Some(guild_id), actor, false)
                .map_err(to_string)?;
            Ok(true)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let text = if joined {
            "✅ You're queued for the next available Mafia roster. Seats go to registered, entry-fee-eligible players in signup order."
        } else {
            "You need to be registered before joining Mafia."
        };
        respond_ephemeral(&responder, text)
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn remind(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.allow_reminder(context.actor_id) {
            return respond_ephemeral(
                &responder,
                "This reminder is cooling down. Try again in a minute.",
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        let repository = self.repository.clone();
        let snapshot = blocking(move || {
            let Some(game) = repository
                .get_active_game(Some(guild_id))
                .map_err(to_string)?
            else {
                return Ok(None);
            };
            let players = repository.get_players(game.game_id).map_err(to_string)?;
            let missing = missing_players(&repository, &game, &players).map_err(to_string)?;
            Ok(Some((game, missing)))
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let Some((game, missing)) = snapshot else {
            return respond_ephemeral(&responder, "No game is waiting on anyone right now.")
                .await
                .map_err(|error| error.to_string().into());
        };
        if missing.is_empty() {
            return respond_ephemeral(
                &responder,
                "Everyone's already acted — the phase will resolve shortly.",
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        let mentions = missing
            .iter()
            .take(25)
            .map(|id| format!("<@{}>", id))
            .collect::<Vec<_>>()
            .join(" ");
        let verb = if game.phase == MafiaPhase::Night {
            "submit your night action with `/mafia act`"
        } else {
            "cast your vote with `/mafia vote`"
        };
        responder
            .respond(
                InteractionResponse::message(format!(
                    "⏰ {} — the game is waiting on you. Please {}.",
                    mentions, verb
                ))
                .with_user_mentions(
                    missing
                        .iter()
                        .take(25)
                        .filter_map(|id| u64::try_from(*id).ok())
                        .collect(),
                ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn status(
        &self,
        _context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let status = blocking(move || public_status(&repository, guild_id))
            .await
            .map_err(InteractionHandlerError::from)?;
        let Some((game, alive, deaths, voted, voters)) = status else {
            return respond_ephemeral(&responder, "No Mafia game is active. Use `/mafia join` to queue for the next roster; it starts once at least 5 eligible players are queued.").await.map_err(|error| error.to_string().into());
        };
        let mut embed = InteractionEmbed::titled(format!(
            "Cama Mafia #{} — {}",
            game.game_id,
            game.phase.as_str()
        ))
        .color(0x5865F2)
        .field("Alive", format!("{} / {}", alive, game.roster_size), true);
        if let Some(twist) = game.twist_event {
            embed = embed.field("Twist", twist_label(twist), true);
        }
        if game.phase == MafiaPhase::Day {
            embed = embed.field("Voted", format!("{} / {}", voted, voters), true);
        }
        embed = embed.field("Phase ends", format!("<t:{}:R>", phase_end(&game)), false);
        if !deaths.is_empty() {
            embed = embed.field(
                "Deaths so far",
                deaths
                    .iter()
                    .map(|(id, role, phase)| {
                        format!("<@{}> — {} ({})", id, role_display(*role), phase.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }
        responder
            .respond(InteractionResponse::message("").ephemeral().embed(embed))
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn history(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let actor = context.actor_id;
        let rows = blocking(move || {
            repository
                .get_player_history(actor, Some(guild_id), 10)
                .map_err(to_string)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        if rows.is_empty() {
            return respond_ephemeral(&responder, "No mafia history yet.")
                .await
                .map_err(|error| error.to_string().into());
        }
        responder
            .respond(
                InteractionResponse::message("").ephemeral().embed(
                    InteractionEmbed::titled("Your Mafia History")
                        .description(rows.iter().map(history_line).collect::<Vec<_>>().join("\n"))
                        .color(0x5865F2),
                ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn leaderboard(
        &self,
        _context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let rows = blocking(move || {
            repository
                .get_leaderboard(Some(guild_id), 20)
                .map_err(to_string)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        if rows.is_empty() {
            return respond_ephemeral(&responder, "No resolved mafia games yet.")
                .await
                .map_err(|error| error.to_string().into());
        }
        let text = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let win_rate = if row.games_played > 0 {
                    (row.wins as f64 / row.games_played as f64) * 100.0
                } else {
                    0.0
                };
                leaderboard_line(index + 1, row, win_rate)
            })
            .collect::<Vec<_>>()
            .join("\n");
        responder
            .respond(
                InteractionResponse::message("").ephemeral().embed(
                    InteractionEmbed::titled("Mafia Leaderboard")
                        .description(text)
                        .color(0xF1C40F),
                ),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn info(
        &self,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let text = mafia_info_text();
        responder
            .respond(
                InteractionResponse::message("")
                    .ephemeral()
                    .embed(InteractionEmbed::titled("Cama Mafia — Rules").description(text)),
            )
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn optout(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let actor = context.actor_id;
        blocking(move || {
            repository
                .set_optout(Some(guild_id), actor, true)
                .map_err(to_string)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        respond_ephemeral(&responder, "You're opted out. Mafia won't wait for or remind you, and any next-game signup was cancelled. Use `/mafia optin` to resume.").await.map_err(|error| error.to_string().into())
    }

    async fn optin(
        &self,
        context: &CommandContext,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let actor = context.actor_id;
        blocking(move || {
            repository
                .set_optout(Some(guild_id), actor, false)
                .map_err(to_string)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        respond_ephemeral(
            &responder,
            "Welcome back. Use `/mafia join` if you want a seat in the next game.",
        )
        .await
        .map_err(|error| error.to_string().into())
    }

    async fn handle_admin(
        &self,
        context: &CommandContext,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.is_admin(context) {
            return respond_ephemeral(
                &responder,
                "❌ Admin only (Administrator or Manage Server).",
            )
            .await
            .map_err(|error| error.to_string().into());
        }
        let (command, _) = leaf_command(options)?;
        match command.as_str() {
            "start" => {
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let repository = self.repository.clone();
                let max_debt = self.max_debt;
                let game =
                    blocking(move || start_game(&repository, guild_id, max_debt, unix_now()))
                        .await
                        .map_err(InteractionHandlerError::from)?;
                if let Some(game) = game.as_ref() {
                    self.post_setup(game)
                        .await
                        .map_err(InteractionHandlerError::from)?;
                }
                let text = game.map_or_else(
                    || {
                        "Couldn't start — too few eligible players, or a game is already running."
                            .to_owned()
                    },
                    |game| {
                        format!(
                            "✅ Started Cama Mafia #{} with {} players.",
                            game.game_id, game.roster_size
                        )
                    },
                );
                responder
                    .followup(InteractionResponse::message(text).ephemeral())
                    .await
                    .map_err(|error| error.to_string().into())
            }
            "stop" => {
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let repository = self.repository.clone();
                let bankruptcy_penalty_rate = self.bankruptcy_penalty_rate;
                let vanity_tax_rate = self.vanity_tax_rate;
                let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
                let summary = blocking(move || {
                    force_finalize(
                        &repository,
                        guild_id,
                        Some(bankruptcy_penalty_rate),
                        vanity_tax_rate,
                        &vanity_taxable_ids,
                    )
                })
                .await
                .map_err(InteractionHandlerError::from)?;
                if summary.resolved {
                    let repository = self.repository.clone();
                    if let Some(game) = blocking(move || {
                        repository
                            .get_game_by_id(summary.game_id)
                            .map_err(to_string)
                    })
                    .await
                    .map_err(InteractionHandlerError::from)?
                        && let Err(error) = self.post_resolution(&game, &summary).await
                    {
                        warn!(guild_id, %error, "Mafia admin resolution announcement failed; READY will retry");
                    }
                }
                let text = summary.winner.map_or_else(
                    || "No active game to stop.".to_owned(),
                    |winner| format!("🛑 Stopped & resolved — **{}** wins.", winner.as_str()),
                );
                responder
                    .followup(InteractionResponse::message(text).ephemeral())
                    .await
                    .map_err(|error| error.to_string().into())
            }
            "abort" => {
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let repository = self.repository.clone();
                let current_game = blocking({
                    let repository = repository.clone();
                    move || {
                        repository
                            .get_active_game(Some(guild_id))
                            .map_err(to_string)
                    }
                })
                .await
                .map_err(InteractionHandlerError::from)?;
                let result = blocking(move || {
                    let Some(game) = repository
                        .get_active_game(Some(guild_id))
                        .map_err(to_string)?
                    else {
                        return Ok(MafiaCancelResult::default());
                    };
                    repository
                        .cancel_game(game.game_id, true, unix_now())
                        .map_err(to_string)
                })
                .await
                .map_err(InteractionHandlerError::from)?;
                if result.applied {
                    if let Some(game) = current_game.as_ref()
                        && let Some(standings_message_id) = game.standings_message_id
                        && let Ok(channel_id) = self.public_channel_id(guild_id).await
                    {
                        let _ = self
                            .discord
                            .unpin_message(
                                channel_id,
                                unsigned_id(standings_message_id, "Mafia standings message")?,
                            )
                            .await;
                    }
                    if let Ok(channel_id) = self.public_channel_id(guild_id).await {
                        let _ = self
                            .discord
                            .send_message(
                                channel_id,
                                DiscordMessage::default_mentions(InteractionResponse::message(
                                    format!(
                                        "🚫 The current Mafia game was aborted by an admin. Entry fees were refunded to {} players.",
                                        result.refunded.len()
                                    ),
                                )),
                            )
                            .await;
                    }
                }
                let text = if result.applied {
                    "🚫 Game aborted and fees refunded.".to_owned()
                } else {
                    "No active game to abort.".to_owned()
                };
                responder
                    .followup(InteractionResponse::message(text).ephemeral())
                    .await
                    .map_err(|error| error.to_string().into())
            }
            _ => Err(format!("unknown Mafia admin subcommand {:?}", command).into()),
        }
    }

    async fn titles(
        &self,
        guild_id: i64,
        actor: i64,
    ) -> Result<Vec<String>, InteractionHandlerError> {
        let repository = self.repository.clone();
        let stats = blocking(move || {
            repository
                .compute_player_stats(Some(guild_id), actor)
                .map_err(to_string)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let mut titles = Vec::new();
        if stats.mafia_wins >= 10 {
            titles.push("Don of Dire".to_owned());
        }
        if stats.correct_reads >= 5 {
            titles.push("Inquisitor".to_owned());
        }
        if stats.mafia_kills >= 5 {
            titles.push("Reaper".to_owned());
        }
        if stats.doctor_saves >= 5 {
            titles.push("Lifesaver".to_owned());
        }
        if stats.jester_wins >= 1 {
            titles.push("Wildcard".to_owned());
        }
        Ok(titles)
    }

    async fn recover(&self, guild_ids: &[u64]) -> ReadyRecoveryReport {
        let mut failures = Vec::new();
        let mut refreshed = 0;
        for guild_id in guild_ids.iter().filter_map(|id| i64::try_from(*id).ok()) {
            // Terminal resolution receipts are an independent outbox.  An
            // administrator may start a replacement game before READY gets
            // another chance to deliver the previous game's announcement,
            // so never make this scan conditional on there being no active
            // game.
            let repository = self.repository.clone();
            match blocking(move || {
                repository
                    .get_unannounced_resolved_games(Some(guild_id))
                    .map_err(to_string)
            })
            .await
            {
                Ok(games) => {
                    for game in games {
                        if let Err(error) = self.recover_resolution(&game).await {
                            failures.push(ReadyRecoveryFailure {
                                guild_id: u64::try_from(guild_id).unwrap_or_default(),
                                message: error,
                            });
                            break;
                        }
                        refreshed += 1;
                    }
                }
                Err(error) => failures.push(ReadyRecoveryFailure {
                    guild_id: u64::try_from(guild_id).unwrap_or_default(),
                    message: error,
                }),
            }
            let repository = self.repository.clone();
            let active = blocking(move || {
                repository
                    .get_active_game(Some(guild_id))
                    .map_err(to_string)
            });
            match active.await {
                Ok(Some(game)) => {
                    if let Err(error) = self.recover_message(&game).await {
                        failures.push(ReadyRecoveryFailure {
                            guild_id: u64::try_from(guild_id).unwrap_or_default(),
                            message: error,
                        });
                    } else {
                        refreshed += 1;
                    }
                }
                Ok(None) => {}
                Err(error) => failures.push(ReadyRecoveryFailure {
                    guild_id: u64::try_from(guild_id).unwrap_or_default(),
                    message: error,
                }),
            }
        }
        ReadyRecoveryReport {
            observer: "mafia-ready-recovery",
            guilds_attempted: guild_ids.len(),
            guilds_refreshed: refreshed,
            guilds_superseded: 0,
            members_refreshed: 0,
            failures,
        }
    }

    async fn recover_message(&self, game: &MafiaGame) -> Result<(), String> {
        // `post_setup` repairs the setup/private/standings receipts. A DAY
        // game also owns a Town Square and a persistent Graveyard; reconcile
        // those independently so a crash between any two receipts is safe to
        // recover on READY.
        self.post_setup(game).await?;
        let repository = self.repository.clone();
        let game_id = game.game_id;
        let current = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .and_then(|game| game.ok_or_else(|| "active Mafia game disappeared".to_owned()))
        })
        .await?;
        if current.phase == MafiaPhase::Day {
            self.reconcile_day_surfaces(&current).await?;
        }
        Ok(())
    }

    async fn recover_resolution(&self, game: &MafiaGame) -> Result<(), String> {
        let summary = self.resolution_summary(game).await?;
        self.post_resolution(game, &summary).await
    }

    async fn resolution_summary(&self, game: &MafiaGame) -> Result<DaySummary, String> {
        if let Some(payload) = game.resolution_summary.as_deref() {
            return serde_json::from_str::<ResolutionReceipt>(payload)
                .map_err(|error| format!("invalid Mafia resolution receipt: {error}"))?
                .into_summary();
        }
        let repository = self.repository.clone();
        let game = game.clone();
        blocking(move || reconstruct_resolution_summary(&repository, &game)).await
    }

    async fn reconcile_day_surfaces(&self, game: &MafiaGame) -> Result<(), String> {
        let game_id = game.game_id;
        let public_channel = self.public_channel_id(game.guild_id).await?;
        let parent_message_id = game
            .setup_message_id
            .or(game.standings_message_id)
            .ok_or_else(|| "Mafia DAY recovery has no public parent receipt".to_owned())?;
        if game.discussion_thread_id.is_none() {
            let thread_id = self
                .mafia_discord
                .create_public_thread(
                    public_channel,
                    unsigned_id(parent_message_id, "Mafia parent message")?,
                    &format!("Day {} — Town Square", game.day_number),
                )
                .await?;
            let thread_id = i64::try_from(thread_id)
                .map_err(|_| "Town Square thread id is outside SQLite range")?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_thread_ids(
                        game_id,
                        ThreadIds {
                            discussion_thread_id: Some(thread_id),
                            ..ThreadIds::default()
                        },
                    )
                    .map_err(to_string)
            })
            .await?;
        }
        let repository = self.repository.clone();
        let players = blocking(move || repository.get_players(game_id).map_err(to_string)).await?;
        self.sync_graveyard(
            game,
            public_channel,
            unsigned_id(parent_message_id, "Mafia parent message")?,
            &players,
        )
        .await?;
        self.post_standings(game).await
    }

    async fn tick_guild(&self, guild_id: i64) -> Result<(), String> {
        let repository = self.repository.clone();
        let game = blocking(move || {
            repository
                .get_active_game(Some(guild_id))
                .map_err(to_string)
        })
        .await?;
        let Some(game) = game else {
            let repository = self.repository.clone();
            let max_debt = self.max_debt;
            if let Some(game) =
                blocking(move || start_game(&repository, guild_id, max_debt, unix_now())).await?
                && let Err(error) = self.post_setup(&game).await
            {
                warn!(guild_id, %error, "Mafia setup announcement failed; READY will retry");
            }
            return Ok(());
        };
        let elapsed = unix_now().saturating_sub(game.phase_started_at.unwrap_or(game.started_at));
        if game.phase == MafiaPhase::Night {
            let repository = self.repository.clone();
            let phase_game = game.clone();
            let ready = blocking(move || phase_ready(&repository, &phase_game, true)).await?;
            if ready || elapsed >= NIGHT_SECONDS {
                let repository = self.repository.clone();
                let summary = blocking(move || resolve_night(&repository, guild_id)).await?;
                if summary.resolved {
                    self.post_dawn(&game, &summary).await?;
                }
            } else if elapsed >= REMINDER_SECONDS {
                self.post_reminder(&game, guild_id).await?;
            }
        } else if game.phase == MafiaPhase::Day {
            let repository = self.repository.clone();
            let phase_game = game.clone();
            let ready = blocking(move || phase_ready(&repository, &phase_game, false)).await?;
            if ready || elapsed >= DAY_SECONDS {
                let repository = self.repository.clone();
                let bankruptcy_penalty_rate = self.bankruptcy_penalty_rate;
                let vanity_tax_rate = self.vanity_tax_rate;
                let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
                let summary = blocking(move || {
                    resolve_day(
                        &repository,
                        guild_id,
                        Some(bankruptcy_penalty_rate),
                        vanity_tax_rate,
                        &vanity_taxable_ids,
                    )
                })
                .await?;
                if summary.resolved {
                    if summary.continued {
                        self.post_dusk(&game, &summary).await?;
                    } else {
                        if let Err(error) = self.post_resolution(&game, &summary).await {
                            warn!(guild_id, %error, "Mafia resolution announcement failed; READY will retry");
                        }
                        let repository = self.repository.clone();
                        let max_debt = self.max_debt;
                        if let Some(next_game) = blocking(move || {
                            start_game(&repository, guild_id, max_debt, unix_now())
                        })
                        .await?
                            && let Err(error) = self.post_setup(&next_game).await
                        {
                            warn!(guild_id, %error, "Mafia next-game setup announcement failed; READY will retry");
                        }
                    }
                }
            } else if elapsed >= REMINDER_SECONDS {
                self.post_reminder(&game, guild_id).await?;
            }
        }
        Ok(())
    }

    async fn post_setup(&self, game: &MafiaGame) -> Result<(), String> {
        let _setup_guard = self.setup_lock.lock().await;
        let repository = self.repository.clone();
        let game_id = game.game_id;
        let fallback_game = game.clone();
        let game = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .map(|stored| stored.unwrap_or(fallback_game))
        })
        .await?;
        let repository = self.repository.clone();
        let game_id = game.game_id;
        let players = blocking(move || repository.get_players(game_id).map_err(to_string)).await?;
        let mafia_players = players
            .iter()
            .filter(|player| player.role == MafiaRole::Mafia)
            .collect::<Vec<_>>();
        let mafia_ids = mafia_players
            .iter()
            .filter_map(|player| u64::try_from(player.discord_id).ok())
            .collect::<Vec<_>>();

        let public_channel = self.public_channel_id(game.guild_id).await?;
        let setup_missing = game.setup_message_id.is_none();
        let private_missing = game.mafia_thread_id.is_none() && !mafia_ids.is_empty();
        let mafia_intro_missing = game.mafia_intro_message_id.is_none() && !mafia_ids.is_empty();

        // Python runs these branches independently. In particular, a public
        // 5xx must not prevent the private role channel from being created,
        // and a private-thread permission failure must not repost the public
        // setup receipt on READY.
        let public_branch = async {
            if !setup_missing {
                return Ok(());
            }
            let roster = players
                .iter()
                .map(|player| format!("<@{}>", player.discord_id))
                .collect::<Vec<_>>()
                .join(" ");
            let twist_line = game
                .twist_event
                .map(|twist| format!("\n**Twist:** {}", twist_label(twist)))
                .unwrap_or_default();
            let body = format!(
                "{}\n\n**Roster ({}):** {}\n**Stakes:** −{} {JOPACOIN_EMOTE} each → pot **{}** {JOPACOIN_EMOTE} for the winning faction.\nUse `/mafia role` to learn yours.\nMafia/Doctor/Detective/Vigilante: `/mafia act target:@x`.\nNight ends <t:{}:R>.{}",
                self.flavor.setup_narration(&app_game(&game)),
                game.roster_size,
                roster,
                ENTRY_FEE,
                game.roster_size * ENTRY_FEE,
                phase_end(&game),
                twist_line,
            );
            let embed =
                InteractionEmbed::titled(format!("🌑 Cama Mafia #{} — Night begins", game.game_id))
                    .description(body)
                    .color(0x2C2F33);
            let ids = players
                .iter()
                .filter_map(|p| u64::try_from(p.discord_id).ok())
                .collect::<BTreeSet<_>>();
            let response = InteractionResponse::message("")
                .embed(embed)
                .with_user_mentions(ids.iter().copied().collect());
            let receipt = self
                .discord
                .send_message(public_channel, DiscordMessage::mentioning(response, ids))
                .await?;
            let setup_message_id =
                i64::try_from(receipt.message_id).map_err(|_| "message id overflow")?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_thread_ids(
                        game_id,
                        ThreadIds {
                            setup_message_id: Some(setup_message_id),
                            ..ThreadIds::default()
                        },
                    )
                    .map_err(to_string)
            })
            .await
        };
        let private_branch = async {
            let thread_id = if private_missing {
                let created = self
                    .mafia_discord
                    .create_private_thread(
                        public_channel,
                        &format!("Mafia #{} — Mafia", game.game_id),
                        &mafia_ids,
                    )
                    .await?
                    .ok_or_else(|| {
                        "Mafia private-thread adapter returned no thread ID".to_owned()
                    })?;
                let thread_id_i64 =
                    i64::try_from(created).map_err(|_| "Mafia thread id overflow")?;
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .set_thread_ids(
                            game_id,
                            ThreadIds {
                                mafia_thread_id: Some(thread_id_i64),
                                ..ThreadIds::default()
                            },
                        )
                        .map_err(to_string)
                })
                .await?;
                created
            } else {
                unsigned_id(
                    game.mafia_thread_id.ok_or("missing Mafia thread receipt")?,
                    "Mafia thread",
                )?
            };
            // A newly created thread receives its bounded membership in the
            // adapter's create call (the Serenity implementation performs
            // the four-at-a-time REST adds there). Existing threads need a
            // membership reconciliation on READY.
            if !private_missing {
                self.mafia_discord
                    .add_private_thread_members(thread_id, &mafia_ids)
                    .await?;
            }
            if !mafia_intro_missing {
                return Ok(());
            }
            let godfather = mafia_players
                .iter()
                .find(|player| player.is_godfather)
                .map(|player| format!("\nGodfather: <@{}> 👑", player.discord_id))
                .unwrap_or_default();
            let allies = mafia_ids
                .iter()
                .map(|id| format!("<@{}>", id))
                .collect::<Vec<_>>()
                .join(", ");
            let thread_response = InteractionResponse::message(format!(
                "🔪 The mafia: {}.{}\nCoordinate kills here. Submit your final pick via `/mafia act`.",
                allies, godfather
            ))
            .with_user_mentions(mafia_ids.clone());
            let receipt = self
                .mafia_discord
                .send_thread_message_with_receipt(
                    thread_id,
                    DiscordMessage::mentioning(
                        thread_response,
                        mafia_ids.iter().copied().collect(),
                    ),
                )
                .await?;
            if let Some(message_id) = receipt {
                let message_id =
                    i64::try_from(message_id).map_err(|_| "Mafia intro message id overflow")?;
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .set_thread_ids(
                            game_id,
                            ThreadIds {
                                mafia_intro_message_id: Some(message_id),
                                ..ThreadIds::default()
                            },
                        )
                        .map_err(to_string)
                })
                .await?;
            }
            Ok(())
        };
        let (public_result, private_result) = tokio::join!(public_branch, private_branch);
        let standings_result = self.post_standings(&game).await;
        public_result.and(private_result).and(standings_result)
    }

    async fn post_standings(&self, game: &MafiaGame) -> Result<(), String> {
        let repository = self.repository.clone();
        let fallback_game = game.clone();
        let game_id = game.game_id;
        let game = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .map(|stored| stored.unwrap_or(fallback_game))
        })
        .await?;
        let repository = self.repository.clone();
        let players =
            blocking(move || repository.get_players(game.game_id).map_err(to_string)).await?;
        let alive = players
            .iter()
            .filter(|player| player.is_alive)
            .collect::<Vec<_>>();
        let dead = players
            .iter()
            .filter(|player| !player.is_alive)
            .collect::<Vec<_>>();
        let phase_word = if game.phase == MafiaPhase::Night {
            "🌙 Night"
        } else {
            "☀️ Day"
        };
        let alive_text = if alive.is_empty() {
            "—".to_owned()
        } else {
            alive
                .iter()
                .map(|player| format!("<@{}>", player.discord_id))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut embed =
            InteractionEmbed::titled(format!("📊 Cama Mafia #{} — Standings", game.game_id))
                .color(0x5865F2)
                .field(
                    format!("{} {}", phase_word, game.day_number),
                    format!("Phase ends <t:{}:R>", phase_end(&game)),
                    false,
                )
                .field(format!("🫀 Alive ({})", alive.len()), alive_text, false);
        if !dead.is_empty() {
            embed = embed.field(
                format!("💀 Fallen ({})", dead.len()),
                dead.iter()
                    .map(|player| {
                        format!("<@{}> — {}", player.discord_id, role_display(player.role))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }
        let mention_ids = players
            .iter()
            .filter_map(|player| u64::try_from(player.discord_id).ok())
            .collect::<BTreeSet<_>>();
        let response = InteractionResponse::message("")
            .embed(embed)
            .with_user_mentions(mention_ids.iter().copied().collect());
        let channel_id = self.public_channel_id(game.guild_id).await?;
        if let Some(message_id) = game.standings_message_id
            && self
                .discord
                .edit_message(
                    channel_id,
                    unsigned_id(message_id, "Mafia standings message")?,
                    DiscordMessage::mentioning(response.clone(), mention_ids.clone())
                        .preserving_content(),
                )
                .await
                .is_ok()
        {
            return Ok(());
        }
        let receipt = self
            .discord
            .send_message(
                channel_id,
                DiscordMessage::mentioning(response, mention_ids),
            )
            .await?;
        let _ = self
            .discord
            .pin_message(channel_id, receipt.message_id)
            .await;
        let standings_message_id =
            i64::try_from(receipt.message_id).map_err(|_| "standings message id overflow")?;
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .set_thread_ids(
                    game.game_id,
                    ThreadIds {
                        standings_message_id: Some(standings_message_id),
                        ..ThreadIds::default()
                    },
                )
                .map_err(to_string)
        })
        .await
    }

    async fn post_dawn(&self, game: &MafiaGame, summary: &NightSummary) -> Result<(), String> {
        let repository = self.repository.clone();
        let fallback_game = game.clone();
        let game_id = game.game_id;
        let current_game = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .map(|stored| stored.unwrap_or(fallback_game))
        })
        .await?;
        let repository = self.repository.clone();
        let players = blocking(move || repository.get_players(game_id).map_err(to_string)).await?;
        let by_id = players
            .iter()
            .map(|p| (p.discord_id, p))
            .collect::<BTreeMap<_, _>>();
        let mut lines = Vec::new();
        for (id, source) in &summary.killed {
            if let Some(player) = by_id.get(id) {
                lines.push(
                    self.flavor
                        .death_narration(&app_player(player), *source == "plague"),
                );
            }
        }
        if lines.is_empty() {
            lines.push("The night was quiet. No one died.".to_owned());
        }
        if let Some(revived_id) = summary.revived_id {
            lines.push(format!(
                "✨ The earth stirs — <@{}> claws back from the grave!",
                revived_id
            ));
        }
        let living_mentions = players
            .iter()
            .filter(|player| player.is_alive)
            .filter_map(|player| u64::try_from(player.discord_id).ok())
            .collect::<BTreeSet<_>>();
        let description = lines.join("\n");
        let public_channel = self.public_channel_id(current_game.guild_id).await?;
        let response = InteractionResponse::message(description.clone())
            .with_user_mentions(living_mentions.iter().copied().collect());
        let response = response.embed(
            InteractionEmbed::titled(format!(
                "☀️ Day {} dawns — Cama Mafia #{}",
                current_game.day_number, current_game.game_id
            ))
            .description(description)
            .color(0xF1C40F)
            .field("Phase ends", format!("<t:{}:R>", phase_end(&current_game)), false)
            .footer(
                "Living players: debate, then `/mafia vote target:@x` (you can change it until dusk). `/mafia bounty target:@x` to stake a read.",
            ),
        );
        let receipt = self
            .discord
            .send_message(
                public_channel,
                DiscordMessage::mentioning(response, living_mentions),
            )
            .await?;

        // Each dawn gets a fresh Town Square. Archive the prior discussion
        // first so only one live public discussion remains discoverable.
        if let Some(previous_thread) = current_game.discussion_thread_id {
            let _ = self
                .mafia_discord
                .archive_thread(
                    unsigned_id(previous_thread, "Mafia discussion thread")?,
                    &format!("Day {} — Town Square (archived)", current_game.day_number),
                )
                .await;
        }
        if let Ok(thread_id) = self
            .mafia_discord
            .create_public_thread(
                public_channel,
                receipt.message_id,
                &format!("Day {} — Town Square", current_game.day_number),
            )
            .await
        {
            let thread_id = i64::try_from(thread_id).map_err(|_| "Town Square id overflow")?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_thread_ids(
                        game_id,
                        ThreadIds {
                            discussion_thread_id: Some(thread_id),
                            ..ThreadIds::default()
                        },
                    )
                    .map_err(to_string)
            })
            .await?;
        }
        self.sync_graveyard(&current_game, public_channel, receipt.message_id, &players)
            .await?;
        self.post_standings(&current_game).await
    }

    async fn sync_graveyard(
        &self,
        game: &MafiaGame,
        public_channel: u64,
        _parent_message_id: u64,
        players: &[MafiaPlayer],
    ) -> Result<(), String> {
        let game_id = game.game_id;
        let dead_ids = players
            .iter()
            .filter(|player| !player.is_alive)
            .filter_map(|player| u64::try_from(player.discord_id).ok())
            .collect::<Vec<_>>();
        if dead_ids.is_empty() {
            return Ok(());
        }
        let thread_id = if let Some(thread_id) = game.graveyard_thread_id {
            let thread_id = unsigned_id(thread_id, "Mafia graveyard thread")?;
            self.mafia_discord
                .add_private_thread_members(thread_id, &dead_ids)
                .await?;
            thread_id
        } else {
            let thread_id = self
                .mafia_discord
                .create_private_thread(
                    public_channel,
                    &format!("⚰️ Mafia #{} — Graveyard", game.game_id),
                    &dead_ids,
                )
                .await?
                .ok_or_else(|| "Mafia graveyard adapter returned no thread ID".to_owned())?;
            let thread_id_i64 =
                i64::try_from(thread_id).map_err(|_| "Mafia graveyard thread id overflow")?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_thread_ids(
                        game_id,
                        ThreadIds {
                            graveyard_thread_id: Some(thread_id_i64),
                            ..ThreadIds::default()
                        },
                    )
                    .map_err(to_string)
            })
            .await?;
            thread_id
        };
        if game.graveyard_intro_message_id.is_some() {
            return Ok(());
        }
        let receipt = self
            .mafia_discord
            .send_thread_message_with_receipt(
                thread_id,
                DiscordMessage::default_mentions(InteractionResponse::message(
                    "Welcome to the graveyard. The dead see all and tell nothing (to the living). Spectate, gossip, and heckle in peace. 🪦",
                )),
            )
            .await?;
        if let Some(message_id) = receipt {
            let message_id = i64::try_from(message_id)
                .map_err(|_| "Mafia graveyard intro message id overflow")?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_thread_ids(
                        game_id,
                        ThreadIds {
                            graveyard_intro_message_id: Some(message_id),
                            ..ThreadIds::default()
                        },
                    )
                    .map_err(to_string)
            })
            .await?;
        }
        Ok(())
    }

    async fn post_dusk(&self, game: &MafiaGame, summary: &DaySummary) -> Result<(), String> {
        let repository = self.repository.clone();
        let fallback_game = game.clone();
        let game_id = game.game_id;
        let current_game = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .map(|stored| stored.unwrap_or(fallback_game))
        })
        .await?;
        let repository = self.repository.clone();
        let players = blocking(move || repository.get_players(game_id).map_err(to_string)).await?;
        let mut lines = Vec::new();
        if let Some(lynched_id) = summary.lynched_id {
            if let Some(victim) = players
                .iter()
                .find(|player| player.discord_id == lynched_id)
            {
                lines.push(self.flavor.lynch_narration(&app_player(victim)));
            }
        } else {
            lines.push(self.flavor.no_lynch_narration());
        }
        if !summary.vote_detail.is_empty() {
            lines.push("**🗳️ The votes are revealed:**".to_owned());
            lines.extend(
                summary
                    .vote_detail
                    .iter()
                    .map(|(actor, target)| format!("• <@{}> → <@{}>", actor, target)),
            );
        } else {
            lines.push("_No votes were cast._".to_owned());
        }
        if summary.bounty_reward > 0 {
            lines.push(format!(
                "🎯 The town bounty paid out **{}** {JOPACOIN_EMOTE}.",
                summary.bounty_reward
            ));
        }
        let bankruptcy_total = summary.bankruptcy_penalties.values().sum::<i64>();
        if bankruptcy_total > 0 {
            lines.push(format!(
                "−{} {JOPACOIN_EMOTE} bankruptcy penalty.",
                bankruptcy_total
            ));
        }
        let vanity_total = summary.vanity_taxes.values().sum::<i64>();
        if vanity_total > 0 {
            lines.push(format!("−{} {JOPACOIN_EMOTE} vanity tax.", vanity_total));
        }
        let living_mentions = players
            .iter()
            .filter(|player| player.is_alive)
            .filter_map(|player| u64::try_from(player.discord_id).ok())
            .collect::<BTreeSet<_>>();
        let response = InteractionResponse::message(lines.join("\n"))
            .with_user_mentions(living_mentions.iter().copied().collect())
            .embed(
                InteractionEmbed::titled(format!(
                    "🌙 Night {} falls — Cama Mafia #{}",
                    current_game.day_number, current_game.game_id
                ))
                .description(lines.join("\n"))
                .color(0xF1C40F)
                .field(
                    "Phase ends",
                    format!("<t:{}:R>", phase_end(&current_game)),
                    false,
                )
                .footer("Roles with night actions: `/mafia act target:@x`."),
            );
        let public_channel = self.public_channel_id(current_game.guild_id).await?;
        self.discord
            .send_message(
                public_channel,
                DiscordMessage::mentioning(response, living_mentions),
            )
            .await
            .map(|_| ())?;
        self.post_standings(&current_game).await
    }

    async fn post_resolution(&self, game: &MafiaGame, summary: &DaySummary) -> Result<(), String> {
        let repository = self.repository.clone();
        let fallback_game = game.clone();
        let game_id = game.game_id;
        let current_game = blocking(move || {
            repository
                .get_game_by_id(game_id)
                .map_err(to_string)
                .map(|stored| stored.unwrap_or(fallback_game))
        })
        .await?;
        // A delivery receipt is the durable idempotency boundary.  READY and
        // a retry after a process crash may call this method again, but a
        // Discord message must never be posted twice for one resolved game.
        // Keep the two Python lifecycle cleanups best-effort on retries.
        if current_game.resolution_message_id.is_some() {
            let public_channel = self.public_channel_id(current_game.guild_id).await?;
            if let Some(thread_id) = current_game.discussion_thread_id {
                let _ = self
                    .mafia_discord
                    .archive_thread(
                        unsigned_id(thread_id, "Mafia discussion thread")?,
                        &format!("Day {} — Town Square (resolved)", current_game.day_number),
                    )
                    .await;
            }
            if let Some(standings_message_id) = current_game.standings_message_id {
                let _ = self
                    .discord
                    .unpin_message(
                        public_channel,
                        unsigned_id(standings_message_id, "Mafia standings message")?,
                    )
                    .await;
            }
            return Ok(());
        }
        let summary = if let Some(payload) = current_game.resolution_summary.as_deref() {
            serde_json::from_str::<ResolutionReceipt>(payload)
                .map_err(|error| format!("invalid Mafia resolution receipt: {error}"))?
                .into_summary()?
        } else {
            let payload = serde_json::to_string(&ResolutionReceipt::from_summary(summary))
                .map_err(|error| format!("serialize Mafia resolution receipt: {error}"))?;
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .set_resolution_summary(game_id, &payload)
                    .map_err(to_string)
            })
            .await?;
            summary.clone()
        };
        let winner = summary.winner.unwrap_or(MafiaWinner::None);
        let repository = self.repository.clone();
        let players = blocking(move || repository.get_players(game_id).map_err(to_string)).await?;
        let by_id = players
            .iter()
            .map(|player| (player.discord_id, player))
            .collect::<BTreeMap<_, _>>();
        let mut lines = vec![self.flavor.resolution_narration(app_winner(winner))];
        if summary.twist == Some(MafiaTwist::TownHall) {
            lines.push("Town Hall was in session — no lynch today.".to_owned());
        } else if let Some(lynched_id) = summary.lynched_id {
            if let Some(victim) = by_id.get(&lynched_id) {
                lines.push(self.flavor.lynch_narration(&app_player(victim)));
            }
        } else {
            lines.push(self.flavor.no_lynch_narration());
        }
        if !summary.winning_ids.is_empty() {
            let winners = summary
                .winning_ids
                .iter()
                .take(15)
                .map(|id| format!("<@{}>", id))
                .collect::<Vec<_>>()
                .join(", ");
            let extra = summary.winning_ids.len().saturating_sub(15);
            lines.push(format!(
                "**Pot:** {} {JOPACOIN_EMOTE} → {} each to {}{}",
                summary.pot_total,
                summary.payout_per_winner,
                winners,
                if extra == 0 {
                    String::new()
                } else {
                    format!(" (+{} more)", extra)
                }
            ));
        }
        if let Some(mvp_id) = summary.mvp_id {
            lines.push(if summary.mvp_bonus > 0 {
                format!(
                    "**MVP:** <@{}> (+{} {JOPACOIN_EMOTE})",
                    mvp_id, summary.mvp_bonus
                )
            } else {
                format!("**MVP:** <@{}>", mvp_id)
            });
        }
        if let Some(bookie_id) = summary.bookie_id {
            lines.push(format!(
                "🎰 **The Bookie** <@{}> called the lynch and cashed out (+{} {JOPACOIN_EMOTE}). The house wins.",
                bookie_id, summary.bookie_payout
            ));
        }
        if summary.nonprofit_overflow > 0 {
            lines.push(format!(
                "_{} {JOPACOIN_EMOTE} over the +{}/winner cap skimmed into the Jopacoin Reserve._",
                summary.nonprofit_overflow, MAX_PAYOUT
            ));
        }
        if summary.bounty_reward > 0 {
            lines.push(format!(
                "🎯 The town bounty paid out **{}** {JOPACOIN_EMOTE}.",
                summary.bounty_reward
            ));
        }
        if !summary.vote_breakdown.is_empty() {
            lines.push("**Vote breakdown:**".to_owned());
            lines.extend(
                summary
                    .vote_breakdown
                    .iter()
                    .map(|(target, count)| format!("• <@{}>: {}", target, count)),
            );
        }
        let description = lines.join("\n\n");
        let winning_mentions = summary
            .winning_ids
            .iter()
            .filter_map(|id| u64::try_from(*id).ok())
            .collect::<BTreeSet<_>>();
        let response = InteractionResponse::message(description.clone())
            .with_user_mentions(winning_mentions.iter().copied().collect())
            .embed(
                InteractionEmbed::titled(format!(
                    "🏁 Cama Mafia #{} — {} wins",
                    current_game.game_id,
                    winner.as_str()
                ))
                .description(description)
                .color(if winner == MafiaWinner::Town {
                    0x57F287
                } else {
                    0xED4245
                }),
            );
        let public_channel = self.public_channel_id(current_game.guild_id).await?;
        let receipt = self
            .discord
            .send_message(
                public_channel,
                DiscordMessage::mentioning(response, winning_mentions),
            )
            .await?;
        let resolution_message_id =
            i64::try_from(receipt.message_id).map_err(|_| "resolution message id overflow")?;
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .set_resolution_message_id(game_id, resolution_message_id)
                .map_err(to_string)
                .map(|_| ())
        })
        .await?;

        if let Some(thread_id) = current_game.discussion_thread_id {
            let _ = self
                .mafia_discord
                .archive_thread(
                    unsigned_id(thread_id, "Mafia discussion thread")?,
                    &format!("Day {} — Town Square (resolved)", current_game.day_number),
                )
                .await;
        }
        if let Some(standings_message_id) = current_game.standings_message_id {
            let _ = self
                .discord
                .unpin_message(
                    public_channel,
                    unsigned_id(standings_message_id, "Mafia standings message")?,
                )
                .await;
        }
        Ok(())
    }

    async fn post_reminder(&self, game: &MafiaGame, guild_id: i64) -> Result<(), String> {
        // Resolve the same public destination used by the Python cog before
        // claiming the durable reminder slot.  A missing configured/fallback
        // channel must not leave a claim that suppresses the next retry.
        let public_channel = match self.public_channel_id(game.guild_id).await {
            Ok(channel_id) => channel_id,
            Err(_) => return Ok(()),
        };
        let repository = self.repository.clone();
        let game_snapshot = game.clone();
        let missing = blocking(move || {
            let players = repository
                .get_players(game_snapshot.game_id)
                .map_err(to_string)?;
            missing_players(&repository, &game_snapshot, &players).map_err(to_string)
        })
        .await?;
        if missing.is_empty() {
            return Ok(());
        }
        let repository = self.repository.clone();
        let game_id = game.game_id;
        let day_number = game.day_number;
        let phase = game.phase;
        if !blocking(move || {
            repository
                .claim_phase_reminder(Some(guild_id), game_id, day_number, phase)
                .map_err(to_string)
        })
        .await?
        {
            return Ok(());
        }

        // The phase may have advanced (or the last player may have acted)
        // between the initial scan and the claim. Re-read both the game and
        // recipients after claiming, exactly as Python does, and release the
        // claim on every no-send path.
        let repository = self.repository.clone();
        let current = match blocking(move || {
            repository
                .get_active_game(Some(guild_id))
                .map_err(to_string)
        })
        .await
        {
            Ok(Some(current))
                if current.game_id == game_id
                    && current.day_number == day_number
                    && current.phase == phase =>
            {
                current
            }
            Ok(_) => {
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .release_phase_reminder(Some(guild_id), game_id, day_number, phase)
                        .map_err(to_string)
                })
                .await?;
                return Ok(());
            }
            Err(error) => {
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .release_phase_reminder(Some(guild_id), game_id, day_number, phase)
                        .map_err(to_string)
                })
                .await?;
                return Err(error);
            }
        };
        let repository = self.repository.clone();
        let current_for_missing = current.clone();
        let current_missing = blocking(move || {
            let players = repository
                .get_players(current_for_missing.game_id)
                .map_err(to_string)?;
            missing_players(&repository, &current_for_missing, &players).map_err(to_string)
        })
        .await;
        let missing = match current_missing {
            Ok(missing) if !missing.is_empty() => missing,
            Ok(_) => {
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .release_phase_reminder(Some(guild_id), game_id, day_number, phase)
                        .map_err(to_string)
                })
                .await?;
                return Ok(());
            }
            Err(error) => {
                let repository = self.repository.clone();
                blocking(move || {
                    repository
                        .release_phase_reminder(Some(guild_id), game_id, day_number, phase)
                        .map_err(to_string)
                })
                .await?;
                return Err(error);
            }
        };
        let mentions = missing
            .iter()
            .take(25)
            .map(|id| format!("<@{}>", id))
            .collect::<Vec<_>>()
            .join(" ");
        let label = if current.phase == MafiaPhase::Night {
            "Night"
        } else {
            "Day"
        };
        let response = InteractionResponse::message(format!(
            "⚠️ {}\n{} {} haven't acted yet — {} {} ends <t:{}:R>.",
            mentions,
            missing.len(),
            if missing.len() == 1 {
                "player"
            } else {
                "players"
            },
            label,
            current.day_number,
            phase_end(&current)
        ))
        .with_user_mentions(
            missing
                .iter()
                .take(25)
                .filter_map(|id| u64::try_from(*id).ok())
                .collect(),
        );
        if self
            .discord
            .send_message(public_channel, DiscordMessage::default_mentions(response))
            .await
            .is_err()
        {
            let repository = self.repository.clone();
            blocking(move || {
                repository
                    .release_phase_reminder(Some(guild_id), game_id, day_number, phase)
                    .map_err(to_string)
            })
            .await?;
        }
        Ok(())
    }
}

async fn blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("Mafia SQLite task failed: {}", error))?
}

fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn signed_id(id: u64, label: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(id).map_err(|_| format!("{} ID exceeds SQLite INTEGER", label).into())
}
fn unsigned_id(id: i64, label: &str) -> Result<u64, String> {
    u64::try_from(id).map_err(|_| format!("{} ID is negative", label))
}
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or_default()
}
fn reject(text: &str) -> ActionReply {
    ActionReply {
        error: Some(text.to_owned()),
        ..ActionReply::default()
    }
}
async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    text: &str,
) -> Result<(), crate::registration::InteractionResponseError> {
    responder
        .respond(
            InteractionResponse::message(text)
                .ephemeral()
                .without_mentions(),
        )
        .await
}

fn leaf_command(options: &[InteractionOption]) -> Result<(String, &[InteractionOption]), String> {
    let option = options
        .iter()
        .find(|option| {
            matches!(
                option.value,
                InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
            )
        })
        .ok_or_else(|| "Mafia interaction is missing a subcommand".to_owned())?;
    match &option.value {
        InteractionValue::Subcommand(children) => Ok((option.name.clone(), children.as_slice())),
        // Preserve the group name for the dispatcher. The group handler then
        // parses its child once more (e.g. `admin stop`) so admin leaves are
        // actually reachable through Discord's nested option payload.
        InteractionValue::SubcommandGroup(children) => {
            Ok((option.name.clone(), children.as_slice()))
        }
        _ => unreachable!(),
    }
}
fn user_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match option.value {
            InteractionValue::User { id, .. } => i64::try_from(id).ok(),
            _ => None,
        })
}
fn mafia_options() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "role",
            "Privately reveal your role in today's mafia game",
            vec![],
        ),
        subcommand(
            "act",
            "Submit your nightly mafia action",
            vec![required(
                "target",
                "Player to act on (Bookie: who you predict the town will lynch)",
                CommandOptionKind::User,
            )],
        ),
        subcommand(
            "vote",
            "Vote to lynch a player during the day phase",
            vec![required(
                "target",
                "Player to vote against",
                CommandOptionKind::User,
            )],
        ),
        subcommand(
            "bounty",
            "Stake 1 jopacoin on a suspect — pays out if they're lynched and were mafia",
            vec![required(
                "target",
                "The player you suspect is mafia",
                CommandOptionKind::User,
            )],
        ),
        subcommand("join", "Reserve a seat in the next mafia game", vec![]),
        subcommand(
            "remind",
            "Ping players who still need to act this phase",
            vec![],
        ),
        subcommand("status", "Show today's mafia game status", vec![]),
        subcommand("history", "Your mafia game history", vec![]),
        subcommand("leaderboard", "Mafia leaderboard for this guild", vec![]),
        subcommand("info", "How to play Daily Mafia", vec![]),
        subcommand(
            "optout",
            "Stop required actions and cancel your next-game signup",
            vec![],
        ),
        subcommand("optin", "Resume required actions in Mafia", vec![]),
        group(
            "admin",
            "Mafia admin controls",
            vec![
                subcommand("start", "Force-start a new mafia game", vec![]),
                subcommand(
                    "stop",
                    "End the running game now with a standing tally",
                    vec![],
                ),
                subcommand(
                    "abort",
                    "Cancel the running game and refund entry fees",
                    vec![],
                ),
            ],
        ),
    ]
}
fn option(name: &str, description: &str, kind: CommandOptionKind) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, kind)
}
fn required(name: &str, description: &str, kind: CommandOptionKind) -> CommandOptionSpec {
    option(name, description, kind).required(true)
}
fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    option(name, description, CommandOptionKind::Subcommand).options(options)
}
fn group(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    option(name, description, CommandOptionKind::SubcommandGroup).options(options)
}

fn phase_end(game: &MafiaGame) -> i64 {
    game.phase_started_at.unwrap_or(game.started_at)
        + if game.phase == MafiaPhase::Night {
            NIGHT_SECONDS
        } else {
            DAY_SECONDS
        }
}
fn role_label(role: MafiaRole) -> &'static str {
    match role {
        MafiaRole::Mafia => "Mafia",
        MafiaRole::Doctor => "Doctor",
        MafiaRole::Detective => "Detective",
        MafiaRole::Vigilante => "Vigilante",
        MafiaRole::Townie => "Townie",
        MafiaRole::Jester => "Jester",
        MafiaRole::Bookie => "Bookie",
    }
}
fn role_emoji(role: MafiaRole) -> &'static str {
    match role {
        MafiaRole::Mafia => "🔪",
        MafiaRole::Doctor => "⚕️",
        MafiaRole::Detective => "🕵️",
        MafiaRole::Vigilante => "🔫",
        MafiaRole::Townie => "👥",
        MafiaRole::Jester => "🃏",
        MafiaRole::Bookie => "🎰",
    }
}
fn role_display(role: MafiaRole) -> String {
    format!("{} {}", role_emoji(role), role_label(role))
}
fn twist_label(twist: MafiaTwist) -> &'static str {
    match twist {
        MafiaTwist::BloodMoon => "🌑 Blood Moon",
        MafiaTwist::TownHall => "🏛️ Town Hall",
        MafiaTwist::MemoryFog => "🌫️ Memory Fog",
        MafiaTwist::Plague => "☠️ Plague",
        MafiaTwist::Resurrection => "✨ Resurrection",
    }
}
fn is_winning_role(winner: Option<MafiaWinner>, role: MafiaRole) -> bool {
    match winner {
        Some(MafiaWinner::Town) => matches!(
            role,
            MafiaRole::Townie | MafiaRole::Doctor | MafiaRole::Detective | MafiaRole::Vigilante
        ),
        Some(MafiaWinner::Mafia) => role == MafiaRole::Mafia,
        Some(MafiaWinner::Jester) => role == MafiaRole::Jester,
        Some(MafiaWinner::None) | None => false,
    }
}
fn leaderboard_line(
    rank: usize,
    row: &cama_db::mafia_repository::MafiaLeaderboardRow,
    win_rate: f64,
) -> String {
    format!(
        "`{:>2}.` <@{}> — {}W / {}G ({:.0}%) • 🔪{} 👥{} 🃏{} ⭐{}",
        rank,
        row.discord_id,
        row.wins,
        row.games_played,
        win_rate,
        row.mafia_wins,
        row.town_wins,
        row.jester_wins,
        row.mvp_count,
    )
}
fn mafia_info_text() -> String {
    format!(
        "A new game starts after the current one finishes once at least {} registered, entry-fee-eligible players are queued with `/mafia join`.\n\n**Phases**\n• Night (24h): Mafia/Doctor/Detective/Vigilante submit `/mafia act`.\n• Day (24h): Living players vote with `/mafia vote`. Tallies hidden.\n• Resolution: Winners split the pot, MVP gets a bonus from it.\n\n**Roles**\n🔪 Mafia — kill at night.\n⚕️ Doctor — protect one player at night.\n🕵️ Detective — investigate **one** player per night.\n🔫 Vigilante — one-shot kill (10+ rosters).\n👥 Townie — vote during the day.\n🃏 Jester — wins solo if lynched (rare).\n🎰 Bookie — neutral; wager at night on the day's lynch and cash out if you call it (rare).\n👑 Godfather — a mafia who reads as Town to the detective.\n\n**Stakes**: every rostered player pays {} {} entry. The pot (`roster × {}`) is split among the winning faction, MVP gets +{} from the pot, and each winner is capped at +{} {} — anything over the cap funds the Jopacoin Reserve. Long-run EV is 0 — play to win.\n\nUse `/mafia optout` to stop required actions and cancel a pending signup.",
        MIN_ROSTER, ENTRY_FEE, JOPACOIN_EMOTE, ENTRY_FEE, MVP_BONUS, MAX_PAYOUT, JOPACOIN_EMOTE,
    )
}
fn history_line(row: &MafiaHistoryRow) -> String {
    let outcome = if is_winning_role(row.winner, row.role) {
        "🏆 Win"
    } else {
        "💀 Loss"
    };
    let godfather = if row.is_godfather { " 👑" } else { "" };
    format!(
        "`#{}` {} — {}{} ({}) → {} • {}",
        row.game_id,
        row.game_date,
        role_display(row.role),
        godfather,
        row.hero_name.as_deref().unwrap_or("Unknown"),
        row.winner.map_or("NONE", MafiaWinner::as_str),
        outcome,
    )
}
fn role_embed(
    game: &MafiaGame,
    player: &MafiaPlayer,
    players: &[MafiaPlayer],
    investigations: &[MafiaAction],
    titles: &[String],
) -> InteractionEmbed {
    let title = format!(
        "{} You are a {}{}",
        role_emoji(player.role),
        role_label(player.role),
        if player.is_godfather { " 👑" } else { "" }
    );
    let title_line = if titles.is_empty() {
        String::new()
    } else {
        format!(" — _{}_", titles.join(", "))
    };
    let mut embed = InteractionEmbed::titled(title)
        .description(format!(
            "Hero: **{}**{}\nPhase: {}\n{}",
            player.hero_name.as_deref().unwrap_or("Unknown"),
            title_line,
            game.phase.as_str(),
            if player.is_alive {
                "Alive"
            } else {
                "💀 Dead"
            }
        ))
        .color(0x5865F2);
    if player.role == MafiaRole::Mafia {
        let allies = players
            .iter()
            .filter(|ally| ally.role == MafiaRole::Mafia && ally.discord_id != player.discord_id)
            .map(|ally| {
                format!(
                    "<@{}> ({}){}",
                    ally.discord_id,
                    ally.hero_name.as_deref().unwrap_or("Unknown"),
                    if ally.is_godfather { " 👑" } else { "" },
                )
            })
            .collect::<Vec<_>>();
        if !allies.is_empty() {
            embed = embed.field("Mafia allies", allies.join("\n"), false);
        }
    }
    if player.role == MafiaRole::Detective {
        let value = if investigations.is_empty() {
            "No investigations yet.".to_owned()
        } else {
            investigations
                .iter()
                .filter_map(|action| {
                    Some(format!(
                        "<@{}>: **{}**",
                        action.target_id?,
                        action.result.as_deref().unwrap_or("?")
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        embed = embed.field("Investigations", value, false);
    }
    match player.role {
        MafiaRole::Vigilante => {
            embed = embed.field(
                "Ability",
                "One-shot kill. Use `/mafia act target:@x` once during any night.",
                false,
            );
        }
        MafiaRole::Detective => {
            embed = embed.field(
                "Ability",
                "Investigate **one** player per night with `/mafia act target:@x` — choose carefully, your read is locked once it's in.",
                false,
            );
        }
        MafiaRole::Jester => {
            embed = embed.field(
                "Win condition",
                "Get yourself lynched during the day phase.",
                false,
            );
        }
        MafiaRole::Bookie => {
            embed = embed.field(
                "Win condition",
                "🎰 Neutral. At night, wager on who the town will lynch with `/mafia act target:@x`. Survive the day and call it right and you cash the ticket — a payout off the top, no matter who wins.",
                false,
            );
        }
        MafiaRole::Mafia | MafiaRole::Doctor | MafiaRole::Townie => {}
    }
    embed
}

fn app_game(game: &MafiaGame) -> cama_app::mafia_service::Game {
    cama_app::mafia_service::Game {
        game_id: u64::try_from(game.game_id).unwrap_or_default(),
        guild_id: game.guild_id,
        game_date: game.game_date.clone(),
        phase: app_phase(game.phase),
        roster_size: usize::try_from(game.roster_size).unwrap_or_default(),
        twist: game.twist_event.map(app_twist),
        day_number: u8::try_from(game.day_number).unwrap_or_default(),
        winner: game.winner.map(app_winner),
        entry_fee: game.entry_fee,
        status: cama_app::mafia_service::GameStatus::Active,
    }
}
fn parse_winner(value: &str) -> Result<MafiaWinner, String> {
    match value {
        "TOWN" => Ok(MafiaWinner::Town),
        "MAFIA" => Ok(MafiaWinner::Mafia),
        "JESTER" => Ok(MafiaWinner::Jester),
        "NONE" => Ok(MafiaWinner::None),
        other => Err(format!("invalid Mafia winner receipt value {other:?}")),
    }
}
fn parse_twist(value: &str) -> Result<MafiaTwist, String> {
    match value {
        "BLOOD_MOON" => Ok(MafiaTwist::BloodMoon),
        "TOWN_HALL" => Ok(MafiaTwist::TownHall),
        "MEMORY_FOG" => Ok(MafiaTwist::MemoryFog),
        "PLAGUE" => Ok(MafiaTwist::Plague),
        "RESURRECTION" => Ok(MafiaTwist::Resurrection),
        other => Err(format!("invalid Mafia twist receipt value {other:?}")),
    }
}
fn app_player(player: &MafiaPlayer) -> cama_app::mafia_service::Player {
    cama_app::mafia_service::Player {
        discord_id: player.discord_id,
        guild_id: player.guild_id,
        role: app_role(player.role),
        is_godfather: player.is_godfather,
        hero_name: player
            .hero_name
            .clone()
            .unwrap_or_else(|| "Unknown".to_owned()),
        is_alive: player.is_alive,
        eliminated_phase: player.eliminated_phase.map(app_phase),
    }
}
const fn app_phase(phase: MafiaPhase) -> cama_app::mafia_service::Phase {
    match phase {
        MafiaPhase::Setup => cama_app::mafia_service::Phase::Setup,
        MafiaPhase::Night => cama_app::mafia_service::Phase::Night,
        MafiaPhase::Day => cama_app::mafia_service::Phase::Day,
        MafiaPhase::Resolved => cama_app::mafia_service::Phase::Resolved,
    }
}
const fn app_role(role: MafiaRole) -> cama_app::mafia_service::Role {
    match role {
        MafiaRole::Mafia => cama_app::mafia_service::Role::Mafia,
        MafiaRole::Doctor => cama_app::mafia_service::Role::Doctor,
        MafiaRole::Detective => cama_app::mafia_service::Role::Detective,
        MafiaRole::Vigilante => cama_app::mafia_service::Role::Vigilante,
        MafiaRole::Townie => cama_app::mafia_service::Role::Townie,
        MafiaRole::Jester => cama_app::mafia_service::Role::Jester,
        MafiaRole::Bookie => cama_app::mafia_service::Role::Bookie,
    }
}
const fn app_twist(twist: MafiaTwist) -> cama_app::mafia_service::Twist {
    match twist {
        MafiaTwist::BloodMoon => cama_app::mafia_service::Twist::BloodMoon,
        MafiaTwist::TownHall => cama_app::mafia_service::Twist::TownHall,
        MafiaTwist::MemoryFog => cama_app::mafia_service::Twist::MemoryFog,
        MafiaTwist::Plague => cama_app::mafia_service::Twist::Plague,
        MafiaTwist::Resurrection => cama_app::mafia_service::Twist::Resurrection,
    }
}
const fn app_winner(winner: MafiaWinner) -> cama_app::mafia_service::Winner {
    match winner {
        MafiaWinner::Town => cama_app::mafia_service::Winner::Town,
        MafiaWinner::Mafia => cama_app::mafia_service::Winner::Mafia,
        MafiaWinner::Jester => cama_app::mafia_service::Winner::Jester,
        MafiaWinner::None => cama_app::mafia_service::Winner::None,
    }
}

fn start_game(
    repository: &MafiaRepository,
    guild_id: i64,
    max_debt: i64,
    seed: i64,
) -> Result<Option<MafiaGame>, String> {
    if let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    {
        return Ok(Some(game));
    }
    let now = unix_now();
    let mut rng = fastrand::Rng::with_seed(seed.unsigned_abs() ^ guild_id.unsigned_abs());
    let game_date = get_game_date();
    // Eligibility can change between the roster read and the atomic debit
    // (another command may spend a player's balance or claim the signup).
    // Python retries that race once with a freshly collected roster.
    for attempt in 0..2 {
        let ids = repository
            .get_eligible_signups(Some(guild_id), ENTRY_FEE, Some(max_debt))
            .map_err(to_string)?
            .into_iter()
            .take(MAX_ROSTER)
            .collect::<Vec<_>>();
        if ids.len() < MIN_ROSTER {
            return Ok(None);
        }
        let twist_event = roll_twist(&mut rng);
        let players = assign_roles(guild_id, &ids, &mut rng);
        let game_id = repository
            .create_game_with_players_and_entry_fees(cama_db::mafia_repository::MafiaGameStart {
                guild_id: Some(guild_id),
                game_date: &game_date,
                phase: MafiaPhase::Night,
                started_at: now,
                roster_size: i64::try_from(players.len()).map_err(to_string)?,
                twist_event,
                entry_fee: ENTRY_FEE,
                players: &players,
                max_debt,
            })
            .map_err(|error| match error {
                cama_db::mafia_repository::MafiaRepositoryError::Invalid("game_already_active") => {
                    "game_already_active".to_owned()
                }
                other => other.to_string(),
            });
        match game_id {
            Ok(game_id) => return repository.get_game_by_id(game_id).map_err(to_string),
            Err(error) if error == "game_already_active" => {
                return repository
                    .get_active_game(Some(guild_id))
                    .map_err(to_string);
            }
            Err(error) if error == "entry_fee_debit_failed" || error == "signup_claim_failed" => {
                if attempt == 0 {
                    continue;
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn assign_roles(guild_id: i64, ids: &[i64], rng: &mut fastrand::Rng) -> Vec<MafiaPlayer> {
    let mut order = ids.to_vec();
    rng.shuffle(&mut order);
    let counts = role_counts(ids.len());
    let mut roles = BTreeMap::new();
    let mut index = 0;
    for _ in 0..counts.mafia {
        if let Some(id) = order.get(index) {
            roles.insert(*id, MafiaRole::Mafia);
        }
        index += 1;
    }
    for role in [MafiaRole::Doctor, MafiaRole::Detective] {
        if let Some(id) = order.get(index) {
            roles.insert(*id, role);
        }
        index += 1;
    }
    for _ in 0..counts.vigilante {
        if let Some(id) = order.get(index) {
            roles.insert(*id, MafiaRole::Vigilante);
        }
        index += 1;
    }
    for id in order.iter().skip(index) {
        roles.insert(*id, MafiaRole::Townie);
    }
    let available_townies = townie_ids(&roles);
    if available_townies.len() >= 2
        && rng.f64() < JESTER_PROBABILITY
        && let Some(id) = rng.choice(&available_townies).copied()
    {
        roles.insert(id, MafiaRole::Jester);
    }
    let available_townies = townie_ids(&roles);
    if available_townies.len() >= 2
        && rng.f64() < BOOKIE_PROBABILITY
        && let Some(id) = rng.choice(&available_townies).copied()
    {
        roles.insert(id, MafiaRole::Bookie);
    }
    let godfather = roles
        .iter()
        .filter_map(|(&id, role)| (*role == MafiaRole::Mafia).then_some(id))
        .collect::<Vec<_>>();
    let godfather = rng.choice(&godfather).copied();
    let heroes = hero_names(ids.len(), rng);
    ids.iter()
        .enumerate()
        .map(|(index, id)| MafiaPlayer {
            game_id: 0,
            discord_id: *id,
            guild_id,
            role: roles.get(id).copied().unwrap_or(MafiaRole::Townie),
            is_godfather: Some(*id) == godfather,
            hero_name: Some(
                heroes
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("Hero {}", index + 1)),
            ),
            is_alive: true,
            eliminated_phase: None,
            eliminated_at: None,
            acted: false,
        })
        .collect()
}

fn hero_names(count: usize, rng: &mut fastrand::Rng) -> Vec<String> {
    static HERO_CATALOG: OnceLock<Vec<String>> = OnceLock::new();
    let catalog = HERO_CATALOG.get_or_init(|| {
        serde_json::from_str::<BTreeMap<String, String>>(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../utils/heroes.json"
        )))
        .unwrap_or_default()
        .into_values()
        .collect()
    });
    if catalog.is_empty() {
        return (0..count)
            .map(|index| format!("Hero {}", index + 1))
            .collect();
    }
    let mut selected = catalog.clone();
    if count <= selected.len() {
        rng.shuffle(&mut selected);
        selected.truncate(count);
        selected
    } else {
        (0..count)
            .filter_map(|_| rng.choice(catalog).cloned())
            .collect()
    }
}

fn townie_ids(roles: &BTreeMap<i64, MafiaRole>) -> Vec<i64> {
    roles
        .iter()
        .filter_map(|(&id, role)| (*role == MafiaRole::Townie).then_some(id))
        .collect()
}

fn roll_twist(rng: &mut fastrand::Rng) -> Option<MafiaTwist> {
    (rng.f64() < TWIST_PROBABILITY)
        .then(|| {
            rng.choice(&[
                MafiaTwist::BloodMoon,
                MafiaTwist::TownHall,
                MafiaTwist::MemoryFog,
                MafiaTwist::Plague,
                MafiaTwist::Resurrection,
            ])
            .copied()
        })
        .flatten()
}

fn submit_night_action(
    repository: &MafiaRepository,
    guild_id: i64,
    actor_id: i64,
    target_id: i64,
) -> Result<ActionReply, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(reject("Night phase is not active."));
    };
    if game.phase != MafiaPhase::Night {
        return Ok(reject("Night phase is not active."));
    }
    let Some(actor) = repository
        .get_player(game.game_id, actor_id)
        .map_err(to_string)?
    else {
        return Ok(reject("You're not in today's game."));
    };
    if !actor.is_alive {
        return Ok(reject("You're dead. Rest in peace."));
    }
    let Some(target) = repository
        .get_player(game.game_id, target_id)
        .map_err(to_string)?
    else {
        return Ok(reject("Target is not a living player."));
    };
    if !target.is_alive {
        return Ok(reject("Target is not a living player."));
    }
    let action_type = match actor.role {
        MafiaRole::Mafia => MafiaActionType::Kill,
        MafiaRole::Doctor => MafiaActionType::Save,
        MafiaRole::Detective => MafiaActionType::Investigate,
        MafiaRole::Vigilante => MafiaActionType::VigilanteKill,
        MafiaRole::Bookie => MafiaActionType::Wager,
        MafiaRole::Townie | MafiaRole::Jester => {
            return Ok(reject("Your role can't perform that action."));
        }
    };
    if action_type == MafiaActionType::Kill && target.role == MafiaRole::Mafia {
        return Ok(reject("Mafia don't kill their own."));
    }
    if action_type == MafiaActionType::Investigate && actor_id == target_id {
        return Ok(reject("You can't investigate yourself."));
    }
    if action_type == MafiaActionType::Investigate
        && let Some(prior) = repository
            .get_action_for_actor(
                game.game_id,
                actor_id,
                MafiaActionType::Investigate,
                Some(game.day_number),
            )
            .map_err(to_string)?
    {
        if prior.target_id == Some(target_id) {
            return Ok(ActionReply {
                ok: true,
                result: prior.result,
                action_type: Some(MafiaActionType::Investigate),
                ..ActionReply::default()
            });
        }
        return Ok(reject(
            "You've already used your read tonight — it's locked on your first target.",
        ));
    }
    if action_type == MafiaActionType::VigilanteKill
        && repository
            .get_action_for_actor(game.game_id, actor_id, MafiaActionType::VigilanteKill, None)
            .map_err(to_string)?
            .is_some()
    {
        return Ok(reject("You've already used your one shot."));
    }
    let result = if action_type == MafiaActionType::Investigate
        && game.twist_event != Some(MafiaTwist::MemoryFog)
        && target.role == MafiaRole::Mafia
        && !target.is_godfather
    {
        Some("Mafia".to_owned())
    } else if action_type == MafiaActionType::Investigate {
        Some("Town".to_owned())
    } else {
        None
    };
    repository
        .record_action(&MafiaAction {
            game_id: game.game_id,
            guild_id,
            actor_id,
            target_id: Some(target_id),
            action_type,
            phase: MafiaPhase::Night,
            day_number: game.day_number,
            result: result.clone(),
        })
        .map_err(to_string)?;
    Ok(ActionReply {
        ok: true,
        result,
        action_type: Some(action_type),
        ..ActionReply::default()
    })
}

fn submit_day_vote(
    repository: &MafiaRepository,
    guild_id: i64,
    actor_id: i64,
    target_id: i64,
) -> Result<ActionReply, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(reject("Day phase is not active."));
    };
    if game.phase != MafiaPhase::Day {
        return Ok(reject("Day phase is not active."));
    }
    let Some(actor) = repository
        .get_player(game.game_id, actor_id)
        .map_err(to_string)?
    else {
        return Ok(reject("You're not in today's game."));
    };
    if !actor.is_alive {
        return Ok(reject("Dead players don't vote."));
    }
    let Some(target) = repository
        .get_player(game.game_id, target_id)
        .map_err(to_string)?
    else {
        return Ok(reject("Target is not a living player."));
    };
    if !target.is_alive {
        return Ok(reject("Target is not a living player."));
    }
    let prior = repository
        .get_action_for_actor(
            game.game_id,
            actor_id,
            MafiaActionType::Vote,
            Some(game.day_number),
        )
        .map_err(to_string)?;
    let changed = prior
        .as_ref()
        .is_some_and(|action| action.target_id != Some(target_id));
    repository
        .record_action(&MafiaAction {
            game_id: game.game_id,
            guild_id,
            actor_id,
            target_id: Some(target_id),
            action_type: MafiaActionType::Vote,
            phase: MafiaPhase::Day,
            day_number: game.day_number,
            result: None,
        })
        .map_err(to_string)?;
    Ok(ActionReply {
        ok: true,
        changed,
        ..ActionReply::default()
    })
}

fn submit_bounty(
    repository: &MafiaRepository,
    guild_id: i64,
    actor_id: i64,
    target_id: i64,
    max_debt: i64,
) -> Result<ActionReply, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(reject("Bounties can only be placed during the day."));
    };
    if game.phase != MafiaPhase::Day {
        return Ok(reject("Bounties can only be placed during the day."));
    }
    let Some(actor) = repository
        .get_player(game.game_id, actor_id)
        .map_err(to_string)?
    else {
        return Ok(reject("Only living players can place bounties."));
    };
    if !actor.is_alive {
        return Ok(reject("Only living players can place bounties."));
    }
    if actor_id == target_id {
        return Ok(reject("You can't put a bounty on yourself."));
    }
    let Some(target) = repository
        .get_player(game.game_id, target_id)
        .map_err(to_string)?
    else {
        return Ok(reject("Target is not a living player."));
    };
    if !target.is_alive {
        return Ok(reject("Target is not a living player."));
    }
    match repository
        .add_bounty(
            game.game_id,
            Some(guild_id),
            game.day_number,
            target_id,
            actor_id,
            max_debt,
        )
        .map_err(to_string)?
    {
        AddBountyResult::Added => Ok(ActionReply {
            ok: true,
            ..ActionReply::default()
        }),
        AddBountyResult::AlreadyStaked => {
            Ok(reject("You've already staked your jopacoin on them today."))
        }
        AddBountyResult::InsufficientFunds => Ok(reject("You can't cover the 1 jopacoin stake.")),
    }
}

fn phase_ready(
    repository: &MafiaRepository,
    game: &MafiaGame,
    night: bool,
) -> Result<bool, String> {
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let opted_out = repository
        .get_opted_out_player_ids(Some(game.guild_id))
        .map_err(to_string)?;
    let actions = repository
        .get_actions(
            game.game_id,
            None,
            Some(if night {
                MafiaPhase::Night
            } else {
                MafiaPhase::Day
            }),
        )
        .map_err(to_string)?;
    Ok(players
        .iter()
        .filter(|player| {
            player.is_alive
                && !opted_out.contains(&player.discord_id)
                && (!night || !matches!(player.role, MafiaRole::Townie | MafiaRole::Jester))
        })
        .all(|player| {
            actions.iter().any(|action| {
                action.actor_id == player.discord_id
                    && action.day_number == game.day_number
                    && if night {
                        action.action_type != MafiaActionType::Vote
                    } else {
                        action.action_type == MafiaActionType::Vote
                    }
            })
        }))
}

fn missing_players(
    repository: &MafiaRepository,
    game: &MafiaGame,
    players: &[MafiaPlayer],
) -> Result<Vec<i64>, MafiaRepositoryError> {
    let opted_out = repository.get_opted_out_player_ids(Some(game.guild_id))?;
    let actions = repository.get_actions(game.game_id, None, Some(game.phase))?;
    Ok(players
        .iter()
        .filter(|player| {
            player.is_alive
                && !opted_out.contains(&player.discord_id)
                && if game.phase == MafiaPhase::Night {
                    !matches!(player.role, MafiaRole::Townie | MafiaRole::Jester)
                        && !actions.iter().any(|action| {
                            action.actor_id == player.discord_id
                                && action.day_number == game.day_number
                                && action.action_type != MafiaActionType::Vote
                        })
                } else {
                    !actions.iter().any(|action| {
                        action.actor_id == player.discord_id
                            && action.day_number == game.day_number
                            && action.action_type == MafiaActionType::Vote
                    })
                }
        })
        .map(|player| player.discord_id)
        .collect())
}

type PublicStatusSnapshot = (
    MafiaGame,
    usize,
    Vec<(i64, MafiaRole, MafiaPhase)>,
    i64,
    i64,
);

fn public_status(
    repository: &MafiaRepository,
    guild_id: i64,
) -> Result<Option<PublicStatusSnapshot>, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(None);
    };
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let alive = players.iter().filter(|player| player.is_alive).count();
    let deaths = players
        .iter()
        .filter(|player| !player.is_alive)
        .filter_map(|player| {
            player
                .eliminated_phase
                .map(|phase| (player.discord_id, player.role, phase))
        })
        .collect::<Vec<_>>();
    let (voted, voters) = if game.phase == MafiaPhase::Day {
        let missing = missing_players(repository, &game, &players).map_err(to_string)?;
        let opted_out = repository
            .get_opted_out_player_ids(Some(guild_id))
            .map_err(to_string)?;
        let voters = players
            .iter()
            .filter(|player| player.is_alive && !opted_out.contains(&player.discord_id))
            .count();
        (
            i64::try_from(voters.saturating_sub(missing.len())).unwrap_or_default(),
            i64::try_from(voters).unwrap_or_default(),
        )
    } else {
        (0, 0)
    };
    Ok(Some((game, alive, deaths, voted, voters)))
}

/// Compatibility fallback for terminal games created before the durable JSON
/// receipt column existed. New settlements always use `ResolutionReceipt`; the
/// fallback still reconstructs all immutable game/action fields and marks the
/// few historical economy-only details unavailable rather than guessing from
/// current balances.
fn reconstruct_resolution_summary(
    repository: &MafiaRepository,
    game: &MafiaGame,
) -> Result<DaySummary, String> {
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let winner = game.winner.unwrap_or(MafiaWinner::None);
    let winning_ids = players
        .iter()
        .filter_map(|player| match winner {
            MafiaWinner::Town
                if matches!(
                    player.role,
                    MafiaRole::Townie
                        | MafiaRole::Doctor
                        | MafiaRole::Detective
                        | MafiaRole::Vigilante
                ) =>
            {
                Some(player.discord_id)
            }
            MafiaWinner::Mafia if player.role == MafiaRole::Mafia => Some(player.discord_id),
            MafiaWinner::Jester if player.role == MafiaRole::Jester => Some(player.discord_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let lynched_id = players
        .iter()
        .filter(|player| player.eliminated_phase == Some(MafiaPhase::Day))
        .max_by_key(|player| player.eliminated_at.unwrap_or_default())
        .map(|player| player.discord_id);
    let votes = repository
        .get_actions(
            game.game_id,
            Some(MafiaActionType::Vote),
            Some(MafiaPhase::Day),
        )
        .map_err(to_string)?
        .into_iter()
        .filter(|vote| vote.day_number == game.day_number)
        .filter_map(|vote| vote.target_id.map(|target| (vote.actor_id, target)))
        .collect::<Vec<_>>();
    let mut vote_breakdown = BTreeMap::new();
    for &(_, target) in &votes {
        *vote_breakdown.entry(target).or_insert(0) += 1;
    }
    let bookie_id = bookie_hit_over_game(repository, game, &players, lynched_id)?;
    Ok(DaySummary {
        resolved: true,
        game_id: game.game_id,
        day_number: game.day_number,
        winner: Some(winner),
        lynched_id,
        mvp_id: game.mvp_id,
        payout_per_winner: game.payout_per_winner,
        pot_total: game.roster_size * game.entry_fee.max(ENTRY_FEE),
        winning_ids,
        bookie_id,
        vote_breakdown,
        vote_detail: votes,
        twist: game.twist_event,
        ..DaySummary::default()
    })
}

fn resolve_night(repository: &MafiaRepository, guild_id: i64) -> Result<NightSummary, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(NightSummary::default());
    };
    if game.phase != MafiaPhase::Night {
        return Ok(NightSummary::default());
    }
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let alive = players
        .iter()
        .filter(|player| player.is_alive)
        .map(|player| player.discord_id)
        .collect::<BTreeSet<_>>();
    let mafia = players
        .iter()
        .filter(|player| player.is_alive && player.role == MafiaRole::Mafia)
        .map(|player| player.discord_id)
        .collect::<BTreeSet<_>>();
    let actions = repository
        .get_actions(game.game_id, None, Some(MafiaPhase::Night))
        .map_err(to_string)?
        .into_iter()
        .filter(|action| action.day_number == game.day_number)
        .collect::<Vec<_>>();
    let mut rng = fastrand::Rng::with_seed(seed_for(&game, 0x004e_4947_4854));
    let target_count = usize::from(game.twist_event == Some(MafiaTwist::BloodMoon)) + 1;
    let valid_kills = actions
        .iter()
        .filter(|action| action.action_type == MafiaActionType::Kill)
        .filter_map(|action| {
            action
                .target_id
                .filter(|id| alive.contains(id) && !mafia.contains(id))
        })
        .collect::<Vec<_>>();
    let targets = if valid_kills.is_empty() {
        let mut pool = alive
            .iter()
            .filter(|id| !mafia.contains(id))
            .copied()
            .collect::<Vec<_>>();
        rng.shuffle(&mut pool);
        pool.into_iter().take(target_count).collect::<Vec<_>>()
    } else {
        let mut counts = BTreeMap::<i64, usize>::new();
        for target in valid_kills {
            *counts.entry(target).or_default() += 1;
        }
        let maximum = counts.values().copied().max().unwrap_or_default();
        let mut leaders = counts
            .iter()
            .filter_map(|(&target, &count)| (count == maximum).then_some(target))
            .collect::<Vec<_>>();
        let first = rng.choice(&leaders).copied();
        let mut targets = first.into_iter().collect::<Vec<_>>();
        if let Some(first) = first {
            leaders.retain(|target| *target != first);
            if targets.len() < target_count {
                let next_maximum = leaders
                    .iter()
                    .filter_map(|target| counts.get(target).copied())
                    .max()
                    .unwrap_or_default();
                let runners = leaders
                    .iter()
                    .filter_map(|target| {
                        (counts.get(target).copied() == Some(next_maximum)).then_some(*target)
                    })
                    .collect::<Vec<_>>();
                if let Some(second) = rng.choice(&runners).copied() {
                    targets.push(second);
                } else {
                    let mut pool = alive
                        .iter()
                        .filter(|id| !mafia.contains(id) && **id != first)
                        .copied()
                        .collect::<Vec<_>>();
                    rng.shuffle(&mut pool);
                    if let Some(second) = pool.into_iter().next() {
                        targets.push(second);
                    }
                }
            }
        }
        targets
    };
    let save_target = actions
        .iter()
        .find(|action| action.action_type == MafiaActionType::Save)
        .and_then(|action| action.target_id);
    let mut killed_ids = targets
        .into_iter()
        .filter(|target| Some(*target) != save_target)
        .collect::<Vec<_>>();
    let vig = actions
        .iter()
        .find(|action| action.action_type == MafiaActionType::VigilanteKill)
        .and_then(|action| action.target_id);
    let vig = vig.filter(|target| alive.contains(target) && !killed_ids.contains(target));
    if let Some(target) = vig {
        killed_ids.push(target);
    }
    let plague_id = if game.twist_event == Some(MafiaTwist::Plague) {
        let mut candidates = alive
            .iter()
            .filter(|id| !killed_ids.contains(id))
            .copied()
            .collect::<Vec<_>>();
        rng.shuffle(&mut candidates);
        candidates.into_iter().next()
    } else {
        None
    };
    if let Some(target) = plague_id {
        killed_ids.push(target);
    }
    let ended = unix_now();
    if !repository
        .apply_night_resolution(game.game_id, &killed_ids, ended)
        .map_err(to_string)?
    {
        return Ok(NightSummary {
            ..NightSummary::default()
        });
    }
    if game.twist_event != Some(MafiaTwist::MemoryFog) {
        backfill_investigations(repository, &game, &players, &actions)?;
    }
    let revived_id = maybe_resurrect(repository, &game, &killed_ids, ended)?;
    let killed = killed_ids
        .into_iter()
        .map(|id| {
            (
                id,
                if Some(id) == vig {
                    "vigilante"
                } else if Some(id) == plague_id {
                    "plague"
                } else {
                    "mafia"
                },
            )
        })
        .collect();
    Ok(NightSummary {
        resolved: true,
        killed,
        revived_id,
    })
}

fn seed_for(game: &MafiaGame, salt: u64) -> u64 {
    game.game_id.unsigned_abs() ^ game.day_number.unsigned_abs() ^ salt
}

fn backfill_investigations(
    repository: &MafiaRepository,
    game: &MafiaGame,
    players: &[MafiaPlayer],
    actions: &[MafiaAction],
) -> Result<(), String> {
    for action in actions
        .iter()
        .filter(|action| action.action_type == MafiaActionType::Investigate)
        .filter(|action| action.result.is_none())
    {
        let result = action
            .target_id
            .and_then(|target_id| players.iter().find(|player| player.discord_id == target_id))
            .filter(|target| target.role == MafiaRole::Mafia && !target.is_godfather)
            .map_or_else(|| "Town".to_owned(), |_| "Mafia".to_owned());
        repository
            .record_action(&MafiaAction {
                game_id: game.game_id,
                guild_id: game.guild_id,
                actor_id: action.actor_id,
                target_id: action.target_id,
                action_type: action.action_type,
                phase: action.phase,
                day_number: action.day_number,
                result: Some(result),
            })
            .map_err(to_string)?;
    }
    Ok(())
}

fn maybe_resurrect(
    repository: &MafiaRepository,
    game: &MafiaGame,
    killed_ids: &[i64],
    now: i64,
) -> Result<Option<i64>, String> {
    if game.twist_event != Some(MafiaTwist::Resurrection) {
        return Ok(None);
    }
    let game_date = game_date_for_timestamp(now as f64).map_err(to_string)?;
    if weekday_of_game_date(&game_date).map_err(to_string)? < 5 {
        return Ok(None);
    }
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let mut candidates = players
        .iter()
        .filter(|player| {
            !player.is_alive
                && player.role != MafiaRole::Mafia
                && !killed_ids.contains(&player.discord_id)
        })
        .map(|player| player.discord_id)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut rng = fastrand::Rng::with_seed(seed_for(game, 0x5245_5355_5252));
    rng.shuffle(&mut candidates);
    let revived = candidates[0];
    repository
        .revive_player(game.game_id, revived)
        .map_err(to_string)?;
    repository
        .set_twist_event(game.game_id, None)
        .map_err(to_string)?;
    Ok(Some(revived))
}

fn resolve_day(
    repository: &MafiaRepository,
    guild_id: i64,
    bankruptcy_penalty_rate: Option<f64>,
    vanity_tax_rate: f64,
    vanity_taxable_ids: &BTreeSet<i64>,
) -> Result<DaySummary, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(DaySummary::default());
    };
    if game.phase != MafiaPhase::Day {
        return Ok(DaySummary::default());
    }
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let alive = players
        .iter()
        .filter(|player| player.is_alive)
        .map(|player| player.discord_id)
        .collect::<BTreeSet<_>>();
    let votes = repository
        .get_actions(
            game.game_id,
            Some(MafiaActionType::Vote),
            Some(MafiaPhase::Day),
        )
        .map_err(to_string)?
        .into_iter()
        .filter(|vote| {
            vote.day_number == game.day_number
                && alive.contains(&vote.actor_id)
                && vote.target_id.is_some_and(|id| alive.contains(&id))
        })
        .collect::<Vec<_>>();
    let mut breakdown = BTreeMap::new();
    for vote in &votes {
        if let Some(target) = vote.target_id {
            *breakdown.entry(target).or_insert(0) += 1;
        }
    }
    let max_votes = breakdown.values().copied().max().unwrap_or(0);
    let leaders = breakdown
        .iter()
        .filter_map(|(target, count)| (*count == max_votes && max_votes > 0).then_some(*target))
        .collect::<Vec<_>>();
    let lynched_id = if game.twist_event == Some(MafiaTwist::TownHall) && game.day_number == 1 {
        None
    } else if leaders.len() == 1 {
        Some(leaders[0])
    } else {
        None
    };
    let lynched_role = lynched_id.and_then(|id| {
        players
            .iter()
            .find(|player| player.discord_id == id)
            .map(|player| player.role)
    });
    let alive_mafia = players
        .iter()
        .filter(|player| {
            player.is_alive
                && player.role == MafiaRole::Mafia
                && Some(player.discord_id) != lynched_id
        })
        .count();
    let alive_non_mafia = players
        .iter()
        .filter(|player| {
            player.is_alive
                && player.role != MafiaRole::Mafia
                && Some(player.discord_id) != lynched_id
        })
        .count();
    let winner = if lynched_role == Some(MafiaRole::Jester) {
        Some(MafiaWinner::Jester)
    } else if alive_mafia == 0 {
        Some(MafiaWinner::Town)
    } else if alive_mafia >= alive_non_mafia {
        Some(MafiaWinner::Mafia)
    } else if game.day_number >= MAX_CYCLES {
        Some(if alive_non_mafia > alive_mafia {
            MafiaWinner::Town
        } else {
            MafiaWinner::Mafia
        })
    } else {
        None
    };
    if let Some(winner) = winner {
        let winning_ids = players
            .iter()
            .filter_map(|player| match winner {
                MafiaWinner::Town
                    if matches!(
                        player.role,
                        MafiaRole::Townie
                            | MafiaRole::Doctor
                            | MafiaRole::Detective
                            | MafiaRole::Vigilante
                    ) =>
                {
                    Some(player.discord_id)
                }
                MafiaWinner::Mafia if player.role == MafiaRole::Mafia => Some(player.discord_id),
                MafiaWinner::Jester if player.role == MafiaRole::Jester => Some(player.discord_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut rng = fastrand::Rng::with_seed(seed_for(&game, 0x004d_5650));
        let mvp_id = compute_mvp(MvpContext {
            repository,
            game: &game,
            players: &players,
            winner,
            lynched_id,
            votes: &votes,
            winning_ids: &winning_ids,
            rng: &mut rng,
        })
        .or_else(|| rng.choice(&winning_ids).copied());
        let bookie_id = bookie_hit_over_game(repository, &game, &players, lynched_id)?;
        let pot_total = game.roster_size * game.entry_fee.max(ENTRY_FEE);
        let (payouts, payout_per_winner, overflow) =
            payout_plan(pot_total, &winning_ids, mvp_id, bookie_id);
        let mvp_bonus = mvp_id
            .and_then(|id| payouts.get(&id).copied())
            .unwrap_or_default()
            - payout_per_winner;
        let bookie_payout = bookie_id
            .and_then(|id| payouts.get(&id).copied())
            .unwrap_or_default();
        let vote_detail = votes
            .iter()
            .filter_map(|vote| vote.target_id.map(|target| (vote.actor_id, target)))
            .collect::<Vec<_>>();
        let receipt_winning_ids = winning_ids.clone();
        let receipt_breakdown = breakdown.clone();
        let receipt_vote_detail = vote_detail.clone();
        let receipt_game_id = game.game_id;
        let receipt_day_number = game.day_number;
        let receipt_twist = game.twist_event;
        let receipt_factory = move |result: &cama_db::mafia_repository::MafiaFinalizeResult| {
            let summary = DaySummary {
                resolved: true,
                game_id: receipt_game_id,
                day_number: receipt_day_number,
                winner: Some(winner),
                lynched_id,
                mvp_id,
                mvp_bonus,
                payout_per_winner,
                pot_total,
                winning_ids: receipt_winning_ids,
                bookie_id,
                bookie_payout,
                nonprofit_overflow: overflow,
                bounty_reward: result.bounty.reward,
                bankruptcy_penalties: result.bankruptcy_penalties.clone(),
                vanity_taxes: result.vanity_taxes.clone(),
                vote_breakdown: receipt_breakdown,
                vote_detail: receipt_vote_detail,
                twist: receipt_twist,
                ..DaySummary::default()
            };
            serde_json::to_string(&ResolutionReceipt::from_summary(&summary))
                .map_err(|_| MafiaRepositoryError::Invalid("resolution_receipt_serialize"))
        };
        let result = repository
            .finalize_day_resolution_with_receipt(
                game.game_id,
                winner,
                payout_per_winner,
                mvp_id,
                lynched_id,
                &payouts,
                game.entry_fee.max(ENTRY_FEE),
                overflow,
                unix_now(),
                Some(game.day_number),
                lynched_role == Some(MafiaRole::Mafia),
                i64::try_from(
                    alive
                        .len()
                        .saturating_sub(usize::from(lynched_id.is_some())),
                )
                .unwrap_or_default(),
                bankruptcy_penalty_rate,
                vanity_tax_rate,
                vanity_taxable_ids,
                receipt_factory,
            )
            .map_err(to_string)?;
        if !result.applied {
            return Ok(DaySummary::default());
        }
        return Ok(DaySummary {
            resolved: true,
            game_id: game.game_id,
            day_number: game.day_number,
            winner: Some(winner),
            lynched_id,
            mvp_id,
            mvp_bonus,
            payout_per_winner,
            pot_total,
            winning_ids,
            bookie_id,
            bookie_payout,
            nonprofit_overflow: overflow,
            bounty_reward: result.bounty.reward,
            bankruptcy_penalties: result.bankruptcy_penalties,
            vanity_taxes: result.vanity_taxes,
            vote_breakdown: breakdown,
            vote_detail,
            twist: game.twist_event,
            ..DaySummary::default()
        });
    }
    let commit = repository
        .advance_to_next_cycle(cama_db::mafia_repository::MafiaCycleAdvance {
            game_id: game.game_id,
            ended_at: unix_now(),
            guild_id: Some(guild_id),
            day_number: game.day_number,
            lynched_id,
            lynched_was_mafia: lynched_role == Some(MafiaRole::Mafia),
            alive_count: i64::try_from(
                alive
                    .len()
                    .saturating_sub(usize::from(lynched_id.is_some())),
            )
            .unwrap_or_default(),
        })
        .map_err(to_string)?;
    Ok(DaySummary {
        resolved: commit.applied,
        continued: commit.applied,
        game_id: game.game_id,
        day_number: game.day_number,
        lynched_id,
        bounty_reward: commit.bounty.reward,
        vote_breakdown: breakdown,
        vote_detail: votes
            .iter()
            .filter_map(|vote| vote.target_id.map(|target| (vote.actor_id, target)))
            .collect(),
        twist: game.twist_event,
        ..DaySummary::default()
    })
}

fn force_finalize(
    repository: &MafiaRepository,
    guild_id: i64,
    bankruptcy_penalty_rate: Option<f64>,
    vanity_tax_rate: f64,
    vanity_taxable_ids: &BTreeSet<i64>,
) -> Result<DaySummary, String> {
    let Some(game) = repository
        .get_active_game(Some(guild_id))
        .map_err(to_string)?
    else {
        return Ok(DaySummary::default());
    };
    let players = repository.get_players(game.game_id).map_err(to_string)?;
    let alive = players
        .iter()
        .filter(|player| player.is_alive)
        .collect::<Vec<_>>();
    let alive_mafia = alive
        .iter()
        .filter(|player| player.role == MafiaRole::Mafia)
        .count();
    let alive_non_mafia = alive.len().saturating_sub(alive_mafia);
    let winner = if alive_mafia == 0 {
        MafiaWinner::Town
    } else if alive_mafia >= alive_non_mafia {
        MafiaWinner::Mafia
    } else {
        MafiaWinner::Town
    };
    if !repository
        .try_reopen_for_finalize(game.game_id)
        .map_err(to_string)?
    {
        return Ok(DaySummary::default());
    }
    let entry_fee = game.entry_fee.max(ENTRY_FEE);
    let pot_total = game.roster_size * entry_fee;
    let winning_ids = players
        .iter()
        .filter_map(|player| match winner {
            MafiaWinner::Town
                if matches!(
                    player.role,
                    MafiaRole::Townie
                        | MafiaRole::Doctor
                        | MafiaRole::Detective
                        | MafiaRole::Vigilante
                ) =>
            {
                Some(player.discord_id)
            }
            MafiaWinner::Mafia if player.role == MafiaRole::Mafia => Some(player.discord_id),
            MafiaWinner::Jester if player.role == MafiaRole::Jester => Some(player.discord_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut rng = fastrand::Rng::with_seed(seed_for(&game, 0x004d_5650));
    let mvp_id = compute_mvp(MvpContext {
        repository,
        game: &game,
        players: &players,
        winner,
        lynched_id: None,
        votes: &[],
        winning_ids: &winning_ids,
        rng: &mut rng,
    });
    let bookie_id = bookie_hit_over_game(repository, &game, &players, None)?;
    let (payouts, payout_per_winner, overflow) =
        payout_plan(pot_total, &winning_ids, mvp_id, bookie_id);
    let mvp_bonus = mvp_id
        .and_then(|id| payouts.get(&id).copied())
        .unwrap_or_default()
        - payout_per_winner;
    let bookie_payout = bookie_id
        .and_then(|id| payouts.get(&id).copied())
        .unwrap_or_default();
    let receipt_winning_ids = winning_ids.clone();
    let receipt_game_id = game.game_id;
    let receipt_day_number = game.day_number;
    let receipt_twist = game.twist_event;
    let receipt_factory = move |result: &cama_db::mafia_repository::MafiaFinalizeResult| {
        let summary = DaySummary {
            resolved: true,
            game_id: receipt_game_id,
            day_number: receipt_day_number,
            winner: Some(winner),
            mvp_id,
            mvp_bonus,
            payout_per_winner,
            pot_total,
            winning_ids: receipt_winning_ids,
            bookie_id,
            bookie_payout,
            nonprofit_overflow: overflow,
            bankruptcy_penalties: result.bankruptcy_penalties.clone(),
            vanity_taxes: result.vanity_taxes.clone(),
            twist: receipt_twist,
            ..DaySummary::default()
        };
        serde_json::to_string(&ResolutionReceipt::from_summary(&summary))
            .map_err(|_| MafiaRepositoryError::Invalid("resolution_receipt_serialize"))
    };
    let result = repository
        .finalize_day_resolution_with_receipt(
            game.game_id,
            winner,
            payout_per_winner,
            mvp_id,
            None,
            &payouts,
            entry_fee,
            overflow,
            unix_now(),
            None,
            false,
            i64::try_from(players.iter().filter(|player| player.is_alive).count())
                .unwrap_or_default(),
            bankruptcy_penalty_rate,
            vanity_tax_rate,
            vanity_taxable_ids,
            receipt_factory,
        )
        .map_err(to_string)?;
    if !result.applied {
        return Ok(DaySummary::default());
    }
    Ok(DaySummary {
        resolved: true,
        game_id: game.game_id,
        day_number: game.day_number,
        winner: Some(winner),
        mvp_id,
        mvp_bonus,
        payout_per_winner,
        pot_total,
        winning_ids,
        bookie_id,
        bookie_payout,
        nonprofit_overflow: overflow,
        bankruptcy_penalties: result.bankruptcy_penalties,
        vanity_taxes: result.vanity_taxes,
        twist: game.twist_event,
        ..DaySummary::default()
    })
}

fn bookie_hit_over_game(
    repository: &MafiaRepository,
    game: &MafiaGame,
    players: &[MafiaPlayer],
    lynched_id: Option<i64>,
) -> Result<Option<i64>, String> {
    let Some(bookie) = players
        .iter()
        .find(|player| player.role == MafiaRole::Bookie && player.is_alive)
    else {
        return Ok(None);
    };
    let wagers = repository
        .get_actions(
            game.game_id,
            Some(MafiaActionType::Wager),
            Some(MafiaPhase::Night),
        )
        .map_err(to_string)?;
    let current_hit = lynched_id.is_some_and(|target| {
        wagers.iter().any(|wager| {
            wager.actor_id == bookie.discord_id
                && wager.day_number == game.day_number
                && wager.target_id == Some(target)
        })
    });
    let prior_hit = wagers
        .iter()
        .any(|wager| wager.actor_id == bookie.discord_id && wager.result.as_deref() == Some("HIT"));
    Ok((current_hit || prior_hit).then_some(bookie.discord_id))
}

struct MvpContext<'a> {
    repository: &'a MafiaRepository,
    game: &'a MafiaGame,
    players: &'a [MafiaPlayer],
    winner: MafiaWinner,
    lynched_id: Option<i64>,
    votes: &'a [MafiaAction],
    winning_ids: &'a [i64],
    rng: &'a mut fastrand::Rng,
}

fn compute_mvp(context: MvpContext<'_>) -> Option<i64> {
    let MvpContext {
        repository,
        game,
        players,
        winner,
        lynched_id,
        votes,
        winning_ids,
        rng,
    } = context;
    if winner == MafiaWinner::Jester {
        return players
            .iter()
            .find(|player| player.role == MafiaRole::Jester)
            .map(|player| player.discord_id);
    }
    if winner == MafiaWinner::Town
        && let Some(lynched_id) = lynched_id
    {
        let candidates = votes
            .iter()
            .filter(|vote| vote.target_id == Some(lynched_id))
            .filter_map(|vote| {
                players
                    .iter()
                    .find(|player| player.discord_id == vote.actor_id)
                    .filter(|player| player.role != MafiaRole::Mafia)
                    .map(|player| player.discord_id)
            })
            .collect::<Vec<_>>();
        if let Some(candidate) = rng.choice(&candidates).copied() {
            return Some(candidate);
        }
    }
    if winner == MafiaWinner::Mafia {
        let actual_victims = players
            .iter()
            .filter(|player| !player.is_alive && player.eliminated_phase == Some(MafiaPhase::Night))
            .map(|player| player.discord_id)
            .collect::<BTreeSet<_>>();
        if let Ok(kills) = repository.get_actions(
            game.game_id,
            Some(MafiaActionType::Kill),
            Some(MafiaPhase::Night),
        ) {
            let candidates = kills
                .iter()
                .filter(|kill| {
                    players.iter().any(|player| {
                        player.discord_id == kill.actor_id && player.role == MafiaRole::Mafia
                    }) && kill
                        .target_id
                        .is_some_and(|target| actual_victims.contains(&target))
                })
                .map(|kill| kill.actor_id)
                .collect::<Vec<_>>();
            if let Some(candidate) = rng.choice(&candidates).copied() {
                return Some(candidate);
            }
        }
        let mafia = players
            .iter()
            .filter(|player| player.role == MafiaRole::Mafia)
            .map(|player| player.discord_id)
            .collect::<Vec<_>>();
        if let Some(candidate) = rng.choice(&mafia).copied() {
            return Some(candidate);
        }
    }
    rng.choice(winning_ids).copied()
}

fn payout_plan(
    pot: i64,
    winners: &[i64],
    mvp: Option<i64>,
    bookie: Option<i64>,
) -> (BTreeMap<i64, i64>, i64, i64) {
    let plan = compute_payouts(pot, winners, mvp, bookie);
    (plan.deltas, plan.payout_per_winner, plan.nonprofit_overflow)
}

#[cfg(test)]
#[path = "mafia_provider_tests.rs"]
mod tests;
