//! Deadlock queue, shuffle and shared-wallet betting Discord adapter.
//! SQLite work is always offloaded after acknowledgement. No drafting routes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::deadlock::{DeadlockRepository, DeadlockSeed};
use cama_db::deadlock_betting::{
    DeadlockBetRequest, DeadlockBettingRepository, DeadlockMarket, DeadlockMarketTerms,
};
use cama_db::deadlock_host::DeadlockHostRepository;
use cama_db::low_priority_repository::LowPriorityRepository;
use cama_domain::deadlock::DeadlockFormat;
use cama_domain::rate_limiter::RateLimiter;

use crate::application_config::ApplicationConfig;
use crate::deadlock_config::DeadlockConfig;
use crate::deadlock_ratings::{DeadlockRatingClient, ImportedRatings, steam_account_id};
use crate::discord_transport::{DiscordMessage, DiscordTransport, resolve_guild_player_names};
use crate::option_ext::{integer_option, string_option, user_option};
use crate::registration::*;
use crate::worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

#[cfg(test)]
#[path = "deadlock_provider_tests.rs"]
mod tests;

pub struct DeadlockCommandRouter {
    dota: Arc<dyn InteractionHandler>,
    deadlock: Arc<dyn InteractionHandler>,
    selector: &'static str,
}
impl DeadlockCommandRouter {
    pub fn new(
        dota: Arc<dyn InteractionHandler>,
        deadlock: Arc<dyn InteractionHandler>,
        selector: &'static str,
    ) -> Self {
        Self {
            dota,
            deadlock,
            selector,
        }
    }
}
#[async_trait]
impl InteractionHandler for DeadlockCommandRouter {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let options = match &request {
            InteractionRequest::Command { options, .. }
            | InteractionRequest::Autocomplete { options, .. } => options,
            _ => return Err("Invalid game command route".into()),
        };
        if string_option(options, self.selector) == Some("deadlock") {
            return self.deadlock.handle(request, responder).await;
        }
        if options.iter().any(|o| o.name == "format")
            || string_option(options, "team").is_some_and(|s| s == "team1" || s == "team2")
        {
            return responder.respond(InteractionResponse::message("Select Deadlock for these options: `/shuffle lobby:deadlock` or `/bet game:deadlock`.").ephemeral()).await.map_err(|e|e.to_string().into());
        }
        self.dota.handle(request, responder).await
    }
}

pub fn format_option() -> CommandOptionSpec {
    choices(
        "format",
        "Deadlock format (default Street Brawl 4v4)",
        false,
        &[
            ("Street Brawl 4v4", "street_brawl"),
            ("Standard 6v6", "standard"),
        ],
    )
}
fn choices(
    name: &str,
    description: &str,
    required: bool,
    values: &[(&str, &str)],
) -> CommandOptionSpec {
    let mut option =
        CommandOptionSpec::new(name, description, CommandOptionKind::String).required(required);
    option.choices = values
        .iter()
        .map(|(name, value)| CommandOptionChoice::String {
            name: (*name).into(),
            value: (*value).into(),
        })
        .collect();
    option
}
fn number(name: &str, description: &str, required: bool, min: i64, max: i64) -> CommandOptionSpec {
    let mut option =
        CommandOptionSpec::new(name, description, CommandOptionKind::Integer).required(required);
    option.min_integer = Some(min);
    // Discord integer options are limited to JavaScript's exactly representable range.
    option.max_integer = Some(max.min(9_007_199_254_740_991));
    option
}
fn match_option(required: bool) -> CommandOptionSpec {
    number("match", "Deadlock match number", required, 1, i64::MAX)
}
fn sub(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}
fn commands() -> Vec<CommandOptionSpec> {
    vec![
        sub(
            "register",
            "Link a Steam account and import initial Deadlock ratings",
            vec![
                CommandOptionSpec::new(
                    "steam",
                    "Numeric SteamID64 or account ID",
                    CommandOptionKind::String,
                )
                .required(true),
            ],
        ),
        sub("join", "Join the Deadlock queue", vec![]),
        sub("leave", "Leave the gathering queue", vec![]),
        sub(
            "ready",
            "Confirm availability for the next ten minutes",
            vec![format_option()],
        ),
        sub(
            "lobby",
            "Show the queue and joining controls",
            vec![format_option()],
        ),
        sub(
            "shuffle",
            "Balance ready players; defaults to Street Brawl",
            vec![format_option()],
        ),
        sub(
            "match",
            "Show a Deadlock match, market and host status",
            vec![match_option(false)],
        ),
        sub(
            "bet",
            "Bet JC on a Deadlock match",
            vec![
                match_option(true),
                choices(
                    "team",
                    "Team shown on the match",
                    true,
                    &[("Team 1", "team1"), ("Team 2", "team2")],
                ),
                number("amount", "JC stake before leverage", true, 1, i64::MAX),
                number("leverage", "1, 2, 3, 5, or mana-gated 10", false, 1, 10),
            ],
        ),
        sub(
            "close",
            "Close betting before starting a manually hosted game",
            vec![match_option(true)],
        ),
        sub(
            "record",
            "Record the whole-match winner and settle bets (organizer/admin)",
            vec![
                match_option(true),
                choices(
                    "winner",
                    "Whole-match winner, not a Brawl round",
                    true,
                    &[("Team 1", "team1"), ("Team 2", "team2")],
                ),
                number(
                    "external_match",
                    "Optional verified in-game match ID",
                    false,
                    1,
                    i64::MAX,
                ),
            ],
        ),
        sub(
            "abort",
            "Cancel an unresolved match and refund its wagers (organizer/admin)",
            vec![
                match_option(true),
                CommandOptionSpec::new(
                    "reason",
                    "Reason for cancellation",
                    CommandOptionKind::String,
                )
                .required(true),
            ],
        ),
        sub(
            "invest",
            "Opt into Deadlock automatic bets on a player",
            vec![
                CommandOptionSpec::new("player", "Player to follow", CommandOptionKind::User)
                    .required(true),
                number(
                    "percentage",
                    "0 removes; 1–10 percent of available balance",
                    true,
                    0,
                    10,
                ),
                choices(
                    "direction",
                    "Long bets with them; short against them",
                    false,
                    &[("Long", "long"), ("Short", "short")],
                ),
            ],
        ),
        sub(
            "liquidity",
            "Opt into Deadlock spectator liquidity",
            vec![number(
                "percentage",
                "0 disables; percent of available balance",
                true,
                0,
                10,
            )],
        ),
        sub(
            "rating",
            "Show your independent Deadlock mode ratings",
            vec![],
        ),
        sub("history", "Show recent Deadlock matches", vec![]),
        sub(
            "fund",
            "Contribute your JC to future Deadlock betting seeds",
            vec![number(
                "amount",
                "JC to transfer to the Deadlock seed fund",
                true,
                1,
                i64::MAX,
            )],
        ),
        sub(
            "bets",
            "Show wagers on a Deadlock match",
            vec![match_option(false)],
        ),
        sub("mybets", "Show your active Deadlock wagers", vec![]),
    ]
}

#[derive(Clone)]
pub struct DeadlockRegistrationProvider {
    handler: Arc<DeadlockHandler>,
}
impl DeadlockRegistrationProvider {
    pub fn new(
        path: impl AsRef<Path>,
        config: DeadlockConfig,
        application: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        vanity: Arc<PersistentVanityTaxService>,
        host_account: Option<u32>,
    ) -> Result<Self, String> {
        let ratings = DeadlockRatingClient::new(config.api_key.clone())?;
        let terms = DeadlockMarketTerms {
            betting_window_seconds: config.betting_window_seconds,
            min_bet: application.values.jopacoin_min_bet,
            max_debt: application.values.max_debt,
            seed: config.seed_amount,
            blind_threshold: application.values.auto_blind_threshold,
            blind_percentage: if application.values.auto_blind_enabled {
                (application.values.auto_blind_percentage * 100.0).round() as i64
            } else {
                0
            },
            bankruptcy_rate_per_game: application.values.bankruptcy_penalty_rate_per_game,
            vanity_tax_rate: application.values.vanity_tax_rate,
            low_priority_tax_rate: application.values.low_priority_profit_tax_rate,
            minigame_jc_delta_scale: application.values.minigame_jc_delta_scale,
            ..DeadlockMarketTerms::default()
        };
        Ok(Self {
            handler: Arc::new(DeadlockHandler {
                path: path.as_ref().to_owned(),
                config,
                ratings,
                discord,
                vanity,
                terms,
                host_account,
                rate_limiter: Mutex::new(RateLimiter::new()),
                rate_clock: Instant::now(),
                admin_ids: application.identities.admin_user_ids.clone(),
                economy: Arc::new(SqliteEconomyEventService::new(
                    path.as_ref(),
                    crate::match_provider::match_economy_config(application),
                )),
            }),
        })
    }
    pub fn handler(&self) -> Arc<dyn InteractionHandler> {
        self.handler.clone()
    }
    pub fn recovery_worker(&self) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new(
            "deadlock-settlement",
            Arc::new(DeadlockRecovery {
                handler: self.handler.clone(),
            }),
        )
    }
}
impl RegistrationProvider for DeadlockRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "deadlock".into(),
            description: "Deadlock queues, balanced teams and JC betting".into(),
            options: commands(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: "deadlock:".into(),
            handler: self.handler.clone(),
        })
    }
}

struct DeadlockHandler {
    path: PathBuf,
    config: DeadlockConfig,
    ratings: DeadlockRatingClient,
    discord: Arc<dyn DiscordTransport>,
    admin_ids: Vec<i64>,
    terms: DeadlockMarketTerms,
    vanity: Arc<PersistentVanityTaxService>,
    economy: Arc<SqliteEconomyEventService>,
    host_account: Option<u32>,
    rate_limiter: Mutex<RateLimiter>,
    rate_clock: Instant,
}
struct Context {
    guild: i64,
    user: i64,
    channel: Option<u64>,
    display_name: String,
    permissions: Option<u64>,
    interaction: u64,
    action: String,
    options: Vec<InteractionOption>,
}
fn parse_format(options: &[InteractionOption]) -> Result<DeadlockFormat, String> {
    match string_option(options, "format") {
        None | Some("street_brawl") => Ok(DeadlockFormat::StreetBrawl),
        Some("standard") => Ok(DeadlockFormat::Standard),
        _ => Err("Unknown Deadlock format".into()),
    }
}
fn side(value: Option<&str>) -> Result<i64, String> {
    match value {
        Some("team1") => Ok(1),
        Some("team2") => Ok(2),
        _ => Err("Choose Deadlock Team 1 or Team 2.".into()),
    }
}
fn positive_id(value: u64) -> Result<i64, String> {
    i64::try_from(value)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| "Invalid Discord ID".to_owned())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|e| format!("Deadlock task failed: {e}"))?
}
fn text_option(name: &str, value: &str) -> InteractionOption {
    InteractionOption {
        name: name.into(),
        value: InteractionValue::String(value.into()),
    }
}
fn int_option(name: &str, value: i64) -> InteractionOption {
    InteractionOption {
        name: name.into(),
        value: InteractionValue::Integer(value),
    }
}

#[async_trait]
impl InteractionHandler for DeadlockHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        if matches!(request,InteractionRequest::Component{custom_id,..} if custom_id.starts_with("deadlock:bet:"))
        {
            InteractionAcknowledgementPolicy::Modal
        } else {
            InteractionAcknowledgementPolicy::Automatic
        }
    }
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let InteractionRequest::Autocomplete {
            guild_id, options, ..
        } = &request
        {
            let Some(guild) = guild_id
                .and_then(|id| positive_id(id).ok())
                .filter(|id| self.config.allows(*id))
            else {
                return responder
                    .autocomplete(vec![])
                    .await
                    .map_err(|e| e.to_string().into());
            };
            let repo = DeadlockRepository::new(&self.path);
            let matches = blocking(move || repo.active_matches(guild).map_err(|e| e.to_string()))
                .await
                .map_err(InteractionHandlerError::from)?;
            let _ = options;
            return responder
                .autocomplete(
                    matches
                        .into_iter()
                        .take(25)
                        .map(|m| CommandOptionChoice::Integer {
                            name: format!("Deadlock #{} · {}", m.match_id, m.format.label()),
                            value: m.match_id,
                        })
                        .collect(),
                )
                .await
                .map_err(|e| e.to_string().into());
        }
        if let InteractionRequest::Component { custom_id, .. } = &request
            && let Some(parts) = custom_id.strip_prefix("deadlock:bet:")
        {
            let (id, side) = parts.split_once(':').ok_or("Invalid betting button")?;
            if id.parse::<i64>().ok().is_none_or(|id| id <= 0) || !matches!(side, "1" | "2") {
                return Err("Invalid betting button".into());
            }
            return responder
                .show_modal(InteractionModal {
                    custom_id: format!("deadlock:wager:{id}:{side}"),
                    title: format!("Deadlock #{id} · Team {side}"),
                    inputs: vec![
                        InteractionTextInput::short("amount", "JC amount"),
                        InteractionTextInput::short("leverage", "Leverage: 1, 2, 3, 5, or 10"),
                    ],
                })
                .await
                .map_err(|e| e.to_string().into());
        }
        let context = match parse_context(request) {
            Ok(context) => context,
            Err(error) => {
                return responder
                    .respond(
                        InteractionResponse::message(error)
                            .ephemeral()
                            .without_mentions(),
                    )
                    .await
                    .map_err(|e| e.to_string().into());
            }
        };
        if !self.config.allows(context.guild) {
            return responder
                .respond(
                    InteractionResponse::message("Deadlock is not enabled in this server.")
                        .ephemeral(),
                )
                .await
                .map_err(|e| e.to_string().into());
        }
        responder.defer(false).await.map_err(|e| e.to_string())?;
        let decision = self
            .rate_limiter
            .lock()
            .map_err(|_| "Deadlock rate limiter unavailable")?
            .check_at(
                self.rate_clock.elapsed().as_secs_f64(),
                &context.action,
                context.guild,
                context.user,
                if context.action == "register" { 5 } else { 20 },
                60,
            );
        if !decision.allowed {
            return responder
                .followup(
                    InteractionResponse::message(format!(
                        "Please wait {} seconds before trying again.",
                        decision.retry_after_seconds
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(|e| e.to_string().into());
        }
        let dedicated = self.config.channels[&context.guild];
        let in_channel = match context.channel {
            Some(channel) if channel == dedicated => true,
            Some(channel) => {
                self.discord
                    .channel_parent_id(context.guild as u64, channel)
                    .await
                    .ok()
                    .flatten()
                    == Some(dedicated)
            }
            None => false,
        };
        if !in_channel {
            return responder
                .followup(
                    InteractionResponse::message(format!(
                        "Use #deadlock-mm for Deadlock matchmaking: <#{dedicated}>."
                    ))
                    .ephemeral()
                    .without_mentions(),
                )
                .await
                .map_err(|e| e.to_string().into());
        }
        match self.execute(&context).await {
            Ok((response, publication)) => {
                if let Some(match_id) = publication {
                    match self.publish_match(context.guild, match_id).await {
                        Ok(thread) => responder
                            .followup(
                                InteractionResponse::message(format!(
                                    "Deadlock #{match_id}: <#{thread}>"
                                ))
                                .without_mentions(),
                            )
                            .await
                            .map_err(|e| e.to_string())?,
                        Err(error) => {
                            tracing::warn!(%error,match_id,"Deadlock publication pending recovery");
                            responder.followup(InteractionResponse::message(format!("Deadlock #{match_id} is saved. Posting its match thread needs recovery; use `/deadlock match match:{match_id}` to inspect it.")).without_mentions()).await.map_err(|e|e.to_string())?;
                        }
                    }
                } else {
                    responder
                        .followup(response.without_mentions())
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            Err(error) => responder
                .followup(
                    InteractionResponse::message(error)
                        .ephemeral()
                        .without_mentions(),
                )
                .await
                .map_err(|e| e.to_string().into()),
        }
    }
}

fn parse_context(request: InteractionRequest) -> Result<Context, String> {
    let (guild, user, channel, display_name, permissions, interaction, action, options) =
        match request {
            InteractionRequest::Command {
                guild_id,
                user_id,
                channel_id,
                user_display_name,
                member_permissions,
                interaction_id,
                name,
                mut options,
                ..
            } => {
                let action = if name == "deadlock" {
                    if options.len() != 1 {
                        return Err("Choose a Deadlock action".into());
                    }
                    let option = options.remove(0);
                    let InteractionValue::Subcommand(nested) = option.value else {
                        return Err("Choose a Deadlock action".into());
                    };
                    options = nested;
                    option.name
                } else {
                    name
                };
                (
                    guild_id,
                    user_id,
                    channel_id,
                    user_display_name,
                    member_permissions,
                    interaction_id,
                    action,
                    options,
                )
            }
            InteractionRequest::Component {
                guild_id,
                user_id,
                channel_id,
                user_display_name,
                member_permissions,
                interaction_id,
                custom_id,
                ..
            } => {
                let parts: Vec<_> = custom_id.split(':').collect();
                let mut options = vec![];
                let action = match parts.as_slice() {
                    ["deadlock", action] if matches!(*action, "join" | "leave" | "lobby") => {
                        *action
                    }
                    ["deadlock", "ready", format] => {
                        options.push(text_option("format", format));
                        "ready"
                    }
                    ["deadlock", "match", id] => {
                        options.push(int_option(
                            "match",
                            id.parse().map_err(|_| "Invalid match button")?,
                        ));
                        "match"
                    }
                    _ => return Err("This Deadlock control is invalid".into()),
                }
                .to_owned();
                (
                    guild_id,
                    user_id,
                    channel_id,
                    user_display_name,
                    member_permissions,
                    interaction_id,
                    action,
                    options,
                )
            }
            InteractionRequest::Modal {
                guild_id,
                user_id,
                channel_id,
                member_permissions,
                interaction_id,
                custom_id,
                fields,
                ..
            } => {
                let parts: Vec<_> = custom_id.split(':').collect();
                let ["deadlock", "wager", id, team] = parts.as_slice() else {
                    return Err("Invalid bet form".into());
                };
                let amount = fields
                    .get("amount")
                    .and_then(|s| s.trim().parse().ok())
                    .ok_or("Enter a positive integer stake")?;
                let leverage = fields
                    .get("leverage")
                    .and_then(|s| s.trim().parse().ok())
                    .ok_or("Enter leverage 1, 2, 3, 5, or 10")?;
                let options = vec![
                    int_option("match", id.parse().map_err(|_| "Invalid market")?),
                    int_option("amount", amount),
                    int_option("leverage", leverage),
                    text_option("team", &format!("team{team}")),
                ];
                (
                    guild_id,
                    user_id,
                    channel_id,
                    "Unknown player".into(),
                    member_permissions,
                    interaction_id,
                    "bet".into(),
                    options,
                )
            }
            _ => return Err("Unsupported Deadlock action".into()),
        };
    Ok(Context {
        guild: positive_id(guild.ok_or("Deadlock requires a server")?)?,
        user: positive_id(user)?,
        channel,
        display_name,
        permissions,
        interaction,
        action,
        options,
    })
}

impl DeadlockHandler {
    fn is_admin(&self, c: &Context) -> bool {
        self.admin_ids.contains(&c.user)
            || c.permissions
                .is_some_and(|p| p & ((1 << 3) | (1 << 5)) != 0)
    }
    async fn execute(&self, c: &Context) -> Result<(InteractionResponse, Option<i64>), String> {
        let repo = DeadlockRepository::new(&self.path);
        let bets = DeadlockBettingRepository::new(&self.path);
        let guild = c.guild;
        let user = c.user;
        let at = now();
        let format = parse_format(&c.options)?;
        let message = match c.action.as_str() {
            "register" => {
                let account = steam_account_id(
                    string_option(&c.options, "steam").ok_or("Enter your Steam ID")?,
                )?;
                let lookup = repo.clone();
                let reply = if let Some(existing) =
                    blocking(move || lookup.enrolled(guild, user).map_err(|e| e.to_string()))
                        .await?
                {
                    if existing.steam_id != i64::from(account) {
                        return Err("Your Deadlock account is already linked; ask an admin to review an account change.".into());
                    }
                    let lookup = repo.clone();
                    let ratings =
                        blocking(move || lookup.ratings(guild, user).map_err(|e| e.to_string()))
                            .await?;
                    if ratings.iter().any(|rating| {
                        rating.games == 0
                            && rating.revision == 0
                            && rating.seed_source == "provisional-v1"
                    }) {
                        let imported = self.ratings.fetch(account, at).await;
                        let source = rating_source_label(&imported.standard.source);
                        let reason = registration_import_reason(&imported);
                        let seeds = rating_seeds(imported, at);
                        let updated = blocking(move || {
                            repo.refresh_unplayed_provisional(
                                guild,
                                user,
                                i64::from(account),
                                &seeds,
                                at,
                            )
                            .map_err(|e| e.to_string())
                        })
                        .await?;
                        if updated > 0 {
                            let refreshed = DeadlockRepository::new(&self.path);
                            let ratings = blocking(move || {
                                refreshed.ratings(guild, user).map_err(|e| e.to_string())
                            })
                            .await?;
                            format!(
                                "Registration refreshed. {}\nInitial rating: {source}.{reason}\nUnplayed provisional ratings were updated. Join with `/deadlock join`.",
                                local_rating_summary(&ratings)
                            )
                        } else {
                            format!(
                                "You are already registered. {}\nYour local ratings were preserved.{reason}",
                                local_rating_summary(&ratings)
                            )
                        }
                    } else {
                        format!(
                            "You are already registered. {}\nYour local ratings were preserved.",
                            local_rating_summary(&ratings)
                        )
                    }
                } else {
                    let imported = self.ratings.fetch(account, at).await;
                    let summary = registration_import_summary(&imported);
                    let seeds = rating_seeds(imported, at);
                    let name = c.display_name.clone();
                    blocking(move || {
                        repo.enroll(guild, user, i64::from(account), &name, &seeds, at)
                            .map_err(|e| e.to_string())
                    })
                    .await?;
                    format!("Registered for Deadlock. {summary}\nJoin with `/deadlock join`.")
                };
                format!(
                    "{reply}\n[Statlocker profile]({})",
                    statlocker_profile_url(account)
                )
            }
            "join" => {
                blocking(move || repo.queue_join(guild, user, at).map_err(|e| e.to_string()))
                    .await?;
                return Ok((self.queue_response(guild, format).await?, None));
            }
            "leave" => {
                blocking(move || repo.queue_leave(guild, user).map_err(|e| e.to_string())).await?;
                return Ok((self.queue_response(guild, format).await?, None));
            }
            "ready" => {
                blocking(move || {
                    repo.ready(guild, user, format, at)
                        .map_err(|e| e.to_string())
                })
                .await?;
                return Ok((self.queue_response(guild, format).await?, None));
            }
            "lobby" => return Ok((self.queue_response(guild, format).await?, None)),
            "shuffle" => {
                if c.options.iter().any(|o| {
                    matches!(
                        o.name.as_str(),
                        "mode"
                            | "rating_system"
                            | "game_mode"
                            | "first_pick"
                            | "tv_delay"
                            | "league_id"
                            | "visibility"
                            | "server_region"
                            | "start"
                            | "hosting"
                    )
                }) {
                    return Err("Dota shuffle settings do not apply to Deadlock. Choose only `lobby:deadlock` and optionally `format:standard`.".into());
                }
                let replay = repo.clone();
                let key = format!("discord:{}", c.interaction);
                let lookup_key = key.clone();
                if let Some(existing) = blocking(move || {
                    replay
                        .match_by_request(guild, &lookup_key)
                        .map_err(|e| e.to_string())
                })
                .await?
                {
                    let host_account = self.host_account;
                    let saved = existing.economy_terms_json;
                    let existing = blocking(move || {
                        repo.shuffle_with_request(
                            guild,
                            format,
                            user,
                            at,
                            &key,
                            &saved,
                            host_account,
                        )
                        .map_err(|e| e.to_string())
                    })
                    .await?;
                    self.recover_setup(guild, existing.match_id, at).await?;
                    return Ok((
                        self.match_response(guild, existing.match_id).await?,
                        Some(existing.match_id),
                    ));
                }
                let members = repo.clone();
                let is_admin = self.is_admin(c);
                if !is_admin
                    && !blocking(move || {
                        members
                            .queue(guild, format)
                            .map(|q| q.iter().any(|p| p.player.discord_id == user))
                            .map_err(|e| e.to_string())
                    })
                    .await?
                {
                    return Err("Join the Deadlock queue before shuffling.".into());
                }
                let mut terms = self.terms.clone();
                let vanity = self.vanity.clone();
                let economy = self.economy.clone();
                let path = self.path.clone();
                terms = blocking(move || {
                    terms.vanity_taxable_ids = vanity.taxable_ids(guild);
                    terms.low_priority_taxable_ids = LowPriorityRepository::new(path)
                        .active_taxable_ids(Some(guild))
                        .map_err(|e| e.to_string())?;
                    terms.payout_multiplier = economy
                        .effects_at(guild, at)
                        .map_err(|e| e.to_string())?
                        .bet_payout_multiplier;
                    Ok(terms)
                })
                .await?;
                let terms_json = serde_json::to_string(&terms).map_err(|e| e.to_string())?;
                let key = format!("discord:{}", c.interaction);
                let host_account = self.host_account;
                let created = blocking(move || {
                    repo.shuffle_with_request(
                        guild,
                        format,
                        user,
                        at,
                        &key,
                        &terms_json,
                        host_account,
                    )
                    .map_err(|e| e.to_string())
                })
                .await?;
                let id = created.match_id;
                let setup = blocking(move || {
                    bets.open_market(guild, id, &terms, at)
                        .map_err(|e| e.to_string())
                })
                .await;
                if let Err(error) = setup {
                    return Err(format!(
                        "Deadlock #{id} was reserved but its betting setup needs recovery: {error}. Use `/deadlock match match:{id}` to retry or `/deadlock abort`. No second shuffle is needed."
                    ));
                }
                return Ok((self.match_response(guild, id).await?, Some(id)));
            }
            "match" => {
                let id = self.select_match(guild, &c.options).await?;
                self.recover_setup(guild, id, at).await?;
                return Ok((self.match_response(guild, id).await?, None));
            }
            "bet" => {
                let id = self.select_match(guild, &c.options).await?;
                let selected = side(string_option(&c.options, "team"))?;
                let amount =
                    integer_option(&c.options, "amount").ok_or("Enter a positive stake")?;
                let leverage = integer_option(&c.options, "leverage").unwrap_or(1);
                let path = self.path.clone();
                let economy = self.economy.clone();
                let scale = self.terms.minigame_jc_delta_scale;
                let (allow_10x, green_mana_bonus) = blocking(move || {
                    let effects =
                        crate::betting_provider::active_mana_effects(&path, user, Some(guild), at)?;
                    let base = cama_domain::economy_scaling::scale_minigame_jc_delta(
                        effects.match_bet_steady_bonus as f64,
                        scale,
                    );
                    let bonus = if base > 0 {
                        economy
                            .adjust_reward_at(guild, base, at)
                            .map_err(|e| e.to_string())?
                    } else {
                        0
                    };
                    Ok((effects.red_10x_leverage, bonus.max(0)))
                })
                .await?;
                let request = DeadlockBetRequest {
                    guild_id: guild,
                    match_id: id,
                    discord_id: user,
                    side: selected,
                    amount,
                    leverage,
                    allow_10x,
                    green_mana_bonus,
                    request_key: format!("discord:{}", c.interaction),
                    now: at,
                };
                let wager =
                    blocking(move || bets.place_bet(&request).map_err(|e| e.to_string())).await?;
                format!(
                    "Deadlock #{id}: bet {} JC on Team {} at {}× ({} JC debited).",
                    wager.amount, wager.side, wager.leverage, wager.effective_stake
                )
            }
            "close" | "record" | "abort" => {
                let id = integer_option(&c.options, "match")
                    .filter(|id| *id > 0)
                    .ok_or("Choose a Deadlock match")?;
                // Organizer identity is checked from the immutable creation record by the repository.
                let permissions = repo.clone();
                let admin = self.is_admin(c);
                if !admin
                    && !blocking(move || {
                        permissions
                            .is_organizer(guild, id, user)
                            .map_err(|e| e.to_string())
                    })
                    .await?
                {
                    return Err("Only this match's organizer or an admin can resolve it.".into());
                }
                match c.action.as_str() {
                    "close" => {
                        blocking(move || {
                            bets.close_market(guild, id, at).map_err(|e| e.to_string())
                        })
                        .await?;
                        format!(
                            "Betting on Deadlock #{id} is closed. Verify both teams and the selected mode before starting."
                        )
                    }
                    "record" => {
                        let winner = side(string_option(&c.options, "winner"))?;
                        let external = integer_option(&c.options, "external_match");
                        blocking(move || {
                            repo.record_result(guild, id, winner, user, external, at)
                                .map_err(|e| e.to_string())?;
                            bets.settle_recorded(guild, id, at)
                                .map_err(|e| e.to_string())
                        })
                        .await?;
                        return Ok((self.match_response(guild, id).await?, Some(id)));
                    }
                    _ => {
                        let reason = string_option(&c.options, "reason")
                            .ok_or("An abort reason is required")?
                            .to_owned();
                        blocking(move || {
                            repo.abort(guild, id, user, &reason, at)
                                .map_err(|e| e.to_string())?;
                            bets.refund_aborted(guild, id, at)
                                .map_err(|e| e.to_string())
                        })
                        .await?;
                        return Ok((self.match_response(guild, id).await?, Some(id)));
                    }
                }
            }
            "invest" => {
                let target = user_option(&c.options, "player")?
                    .ok_or("Choose a player")?
                    .id;
                let percent =
                    integer_option(&c.options, "percentage").ok_or("Choose a percentage")?;
                let direction = string_option(&c.options, "direction")
                    .unwrap_or("long")
                    .to_owned();
                blocking(move || {
                    bets.set_investment(guild, user, target, &direction, percent)
                        .map_err(|e| e.to_string())
                })
                .await?;
                "Deadlock investment preference saved. Existing Dota preferences are unchanged."
                    .to_owned()
            }
            "liquidity" => {
                let percent =
                    integer_option(&c.options, "percentage").ok_or("Choose a percentage")?;
                blocking(move || {
                    bets.set_liquidity(guild, user, percent)
                        .map_err(|e| e.to_string())
                })
                .await?;
                "Deadlock spectator liquidity preference saved.".to_owned()
            }
            "rating" => {
                let ratings =
                    blocking(move || repo.ratings(guild, user).map_err(|e| e.to_string())).await?;
                if ratings.is_empty() {
                    "Register with `/deadlock register` first.".into()
                } else {
                    ratings
                        .iter()
                        .map(|r| {
                            format!(
                                "**{}:** {:.1} ± {:.1} · {} games\nInitial source: {}",
                                r.format.label(),
                                r.mu,
                                r.sigma,
                                r.games,
                                rating_source_label(&r.seed_source)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "history" => {
                let matches =
                    blocking(move || repo.recent_matches(guild, 10).map_err(|e| e.to_string()))
                        .await?;
                if matches.is_empty() {
                    "No Deadlock matches yet.".into()
                } else {
                    matches
                        .iter()
                        .map(|m| {
                            format!(
                                "#{} · {} · {}{}",
                                m.match_id,
                                m.format.label(),
                                m.status,
                                m.winner
                                    .map_or_else(String::new, |s| format!(" · Team {s} won"))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "fund" => {
                let amount =
                    integer_option(&c.options, "amount").ok_or("Enter a positive contribution")?;
                let key = format!("discord:{}", c.interaction);
                blocking(move || {
                    bets.contribute_seed(guild, user, amount, &key, at)
                        .map_err(|e| e.to_string())
                })
                .await?;
                format!("Transferred {amount} JC from your wallet to the Deadlock seed fund.")
            }
            "mybets" => {
                let wagers = blocking(move || {
                    bets.user_wagers(guild, user, true)
                        .map_err(|e| e.to_string())
                })
                .await?;
                let rows = wagers
                    .iter()
                    .take(20)
                    .map(|w| {
                        format!(
                            "{} · Team {} · {} JC at {}×",
                            w.market_id, w.side, w.effective_stake, w.leverage
                        )
                    })
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    "You have no active Deadlock wagers.".into()
                } else {
                    rows.join("\n")
                }
            }
            "bets" => {
                let id = self.select_match(guild, &c.options).await?;
                let wagers =
                    blocking(move || bets.wagers(guild, id).map_err(|e| e.to_string())).await?;
                let ids = wagers.iter().map(|w| w.discord_id).collect::<Vec<_>>();
                let names =
                    resolve_guild_player_names(self.discord.as_ref(), Some(guild as u64), &ids)
                        .await;
                let mut rows = wagers
                    .iter()
                    .take(20)
                    .map(|w| {
                        format!(
                            "{} · Team {} · {} JC ({}×){}",
                            names.resolve(w.discord_id),
                            w.side,
                            w.effective_stake,
                            w.leverage,
                            w.payout
                                .map_or_else(String::new, |p| format!(" · paid {p}"))
                        )
                    })
                    .collect::<Vec<_>>();
                if wagers.len() > 20 {
                    rows.push(format!("…{} more wagers", wagers.len() - 20));
                }
                format!(
                    "**Deadlock #{id} wagers**\n{}",
                    if rows.is_empty() {
                        "No wagers.".into()
                    } else {
                        rows.join("\n")
                    }
                )
            }
            _ => return Err("Unknown Deadlock command".into()),
        };
        Ok((InteractionResponse::message(message), None))
    }
    async fn select_match(&self, guild: i64, options: &[InteractionOption]) -> Result<i64, String> {
        if let Some(id) = integer_option(options, "match").filter(|id| *id > 0) {
            return Ok(id);
        }
        let repo = DeadlockRepository::new(&self.path);
        let matches =
            blocking(move || repo.active_matches(guild).map_err(|e| e.to_string())).await?;
        if matches.len() != 1 {
            return Err("Specify a Deadlock match number; there must be exactly one active match to auto-select.".into());
        }
        Ok(matches[0].match_id)
    }
    async fn recover_setup(&self, guild: i64, id: i64, at: i64) -> Result<(), String> {
        let repo = DeadlockRepository::new(&self.path);
        let bets = DeadlockBettingRepository::new(&self.path);
        blocking(move || {
            let game = repo
                .match_by_id(guild, id)
                .map_err(|e| e.to_string())?
                .ok_or("Deadlock match not found")?;
            if game.status == "economic_setup" {
                let terms = serde_json::from_str(&game.economy_terms_json)
                    .map_err(|e| format!("Invalid saved betting policy: {e}"))?;
                bets.open_market(guild, id, &terms, at)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .await
    }
    async fn queue_response(
        &self,
        guild: i64,
        format: DeadlockFormat,
    ) -> Result<InteractionResponse, String> {
        let repo = DeadlockRepository::new(&self.path);
        let queue = blocking(move || repo.queue(guild, format).map_err(|e| e.to_string())).await?;
        let ids: Vec<_> = queue.iter().map(|p| p.player.discord_id).collect();
        let names =
            resolve_guild_player_names(self.discord.as_ref(), u64::try_from(guild).ok(), &ids)
                .await;
        let at = now();
        let ready = queue
            .iter()
            .filter(|p| p.ready_format == Some(format) && p.ready_until.is_some_and(|t| t > at))
            .count();
        let needed = format.player_count().saturating_sub(ready);
        let status = if queue.is_empty() {
            "The lobby is open. Join below to start gathering players.".to_owned()
        } else if needed == 0 {
            "**Ready to shuffle.** Create balanced teams with the command below.".to_owned()
        } else {
            format!(
                "Waiting for **{needed} more ready player{}**. Join and mark yourself ready below.",
                if needed == 1 { "" } else { "s" }
            )
        };
        let mut embed = InteractionEmbed::titled(format!("Deadlock · {}", format.label()))
            .color(0xc69c6d)
            .description(format!(
                "**{ready}/{} ready** · **{} queued**\n\n{status}",
                format.player_count(),
                queue.len()
            ))
            .footer("Local ratings in brackets · Ready expires after ten minutes.");
        if queue.is_empty() {
            embed = embed.field("Players", "No players yet. Use **Join** below.", false);
        } else {
            let mut rows = String::new();
            let mut continuation = false;
            for p in queue.iter().take(20) {
                let ready = p.ready_format == Some(format) && p.ready_until.is_some_and(|t| t > at);
                let name = names.resolve(p.player.discord_id);
                let name_label = statlocker_player_link(&name, p.player.steam_id);
                let rating = format!("{:.1}", p.player.mu);
                let rating = if rating.len() > 16 {
                    format!("{:.2e}", p.player.mu)
                } else {
                    rating
                };
                let row = format!(
                    "{} {} [{}] · {}",
                    if ready { "✓" } else { "○" },
                    name_label,
                    rating,
                    if ready { "Ready" } else { "Waiting" }
                );
                if !rows.is_empty()
                    && rows.encode_utf16().count() + row.encode_utf16().count() + 2 > 1024
                {
                    embed = embed.field(
                        if continuation {
                            "Players (continued)"
                        } else {
                            "Players"
                        },
                        std::mem::take(&mut rows),
                        false,
                    );
                    continuation = true;
                }
                if !rows.is_empty() {
                    rows.push_str("\n\n");
                }
                rows.push_str(&row);
            }
            embed = embed.field(
                if continuation {
                    "Players (continued)"
                } else {
                    "Players"
                },
                rows,
                false,
            );
            if queue.len() > 20 {
                embed = embed.field(
                    "Also queued",
                    format!("{} more players are waiting.", queue.len() - 20),
                    false,
                );
            }
        }
        let commands = match format {
            DeadlockFormat::StreetBrawl => {
                "**Street Brawl:** `/shuffle lobby:deadlock`\n\n**Standard 6v6:** ready with **Ready 6v6**, then `/shuffle lobby:deadlock format:standard`"
            }
            DeadlockFormat::Standard => {
                "**Standard:** `/shuffle lobby:deadlock format:standard`\n\n**Street Brawl 4v4:** ready with **Ready 4v4**, then `/shuffle lobby:deadlock`"
            }
        };
        embed = embed.field("Shuffle", commands, false);
        let mut response = InteractionResponse::message("")
            .embed(embed)
            .without_mentions();
        response.components.push(InteractionActionRow::buttons(vec![
            InteractionButton::new("deadlock:join", "Join"),
            InteractionButton::new("deadlock:leave", "Leave")
                .style(InteractionButtonStyle::Secondary),
            InteractionButton::new("deadlock:ready:street_brawl", "Ready 4v4"),
            InteractionButton::new("deadlock:ready:standard", "Ready 6v6"),
            InteractionButton::new("deadlock:lobby", "Refresh")
                .style(InteractionButtonStyle::Secondary),
        ]));
        Ok(response)
    }
    async fn match_response(&self, guild: i64, id: i64) -> Result<InteractionResponse, String> {
        let repo = DeadlockRepository::new(&self.path);
        let bets = DeadlockBettingRepository::new(&self.path);
        let hosts = DeadlockHostRepository::new(&self.path);
        let (game, market, host) = blocking(move || {
            Ok((
                repo.match_by_id(guild, id)
                    .map_err(|e| e.to_string())?
                    .ok_or("Deadlock match not found")?,
                bets.market(guild, id).map_err(|e| e.to_string())?,
                hosts.for_match(guild, id).map_err(|e| e.to_string())?,
            ))
        })
        .await?;
        let ids: Vec<_> = game
            .team1
            .iter()
            .chain(&game.team2)
            .map(|p| p.discord_id)
            .collect();
        let names =
            resolve_guild_player_names(self.discord.as_ref(), u64::try_from(guild).ok(), &ids)
                .await;
        let render = |team: &[cama_domain::deadlock::DeadlockPlayer]| {
            team.iter()
                .map(|p| statlocker_player_link(&names.resolve(p.discord_id), p.steam_id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut response=InteractionResponse::message(format!("**Deadlock #{id} · {} · {}**\n**Team 1:** {}\n**Team 2:** {}\n{}\n{}",game.format.label(),game.status,render(&game.team1),render(&game.team2),market_summary(market.as_ref(),now()),game.winner.map_or_else(||format!("Organizer: close betting with `/deadlock close match:{id}` before manual launch. Record the whole-match winner with `/deadlock record`."),|winner|format!("Whole-match winner: Team {winner}."))));
        if let Some(host) = host {
            response.content.push_str(&format!(
                "\nHost: {}{}{}",
                host.phase,
                host.join_code
                    .map_or_else(String::new, |code| format!(" · Join code: **{code}**")),
                host.error.map_or_else(String::new, |_| {
                    " · Organizer review needed; use manual recovery.".into()
                })
            ));
        } else {
            response.content.push_str("\nHosting: manual lobby.");
        }
        let open = market
            .as_ref()
            .is_some_and(|m| m.status == "open" && m.deadline > now());
        response.components.push(InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("deadlock:bet:{id}:1"), "Bet Team 1").disabled(!open),
            InteractionButton::new(format!("deadlock:bet:{id}:2"), "Bet Team 2").disabled(!open),
            InteractionButton::new(format!("deadlock:match:{id}"), "Refresh")
                .style(InteractionButtonStyle::Secondary),
        ]));
        Ok(response)
    }

    /// The channel starter and attached thread have stable identities. Sending
    /// with a delivery key and reconciling the thread parent handles lost replies.
    async fn publish_match(&self, guild: i64, id: i64) -> Result<u64, String> {
        let channel = *self
            .config
            .channels
            .get(&guild)
            .ok_or("Missing #deadlock-mm channel")?;
        let repo = DeadlockRepository::new(&self.path);
        let game = blocking(move || {
            repo.match_by_id(guild, id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Deadlock match not found".into())
        })
        .await?;
        let response = self.match_response(guild, id).await?;
        let starter = if let Some(message) = game.publication_message_id {
            let saved_channel = game
                .publication_channel_id
                .ok_or("Incomplete match publication receipt")?;
            if saved_channel != channel as i64 {
                return Err("The configured channel changed; preserve the original match channel until it finishes".into());
            }
            self.discord
                .edit_message(
                    channel,
                    message as u64,
                    DiscordMessage::silent(response.clone()),
                )
                .await?;
            message as u64
        } else {
            let key = format!("deadlock:{guild}:{id}:starter");
            let receipt = match self
                .discord
                .find_message_by_delivery_key(channel, &key, game.created_at, 100)
                .await?
            {
                Some(receipt) => receipt,
                None => {
                    self.discord
                        .send_message_with_delivery_key(
                            channel,
                            &key,
                            DiscordMessage::silent(response.clone()),
                        )
                        .await?
                }
            };
            let repo = DeadlockRepository::new(&self.path);
            let message = positive_id(receipt.message_id)?;
            blocking(move || {
                repo.set_publication(guild, id, channel as i64, message)
                    .map_err(|e| e.to_string())
            })
            .await?;
            receipt.message_id
        };
        let thread = if let Some(thread) = game.publication_thread_id {
            thread as u64
        } else {
            match self
                .discord
                .create_public_thread(
                    channel,
                    starter,
                    &format!("Deadlock #{id} · {}", game.format.label()),
                )
                .await
            {
                Ok(thread) => thread,
                Err(error) => {
                    // Attached public thread IDs equal their starter message IDs.
                    if self
                        .discord
                        .channel_parent_id(guild as u64, starter)
                        .await?
                        == Some(channel)
                    {
                        starter
                    } else {
                        return Err(error);
                    }
                }
            }
        };
        if let Some(message) = game.thread_message_id {
            self.discord
                .edit_message(thread, message as u64, DiscordMessage::silent(response))
                .await?;
        } else {
            let key = format!("deadlock:{guild}:{id}:thread");
            let receipt = match self
                .discord
                .find_message_by_delivery_key(thread, &key, game.created_at, 100)
                .await?
            {
                Some(receipt) => receipt,
                None => {
                    self.discord
                        .send_message_with_delivery_key(
                            thread,
                            &key,
                            DiscordMessage::silent(response),
                        )
                        .await?
                }
            };
            let repo = DeadlockRepository::new(&self.path);
            let thread_id = positive_id(thread)?;
            let message = positive_id(receipt.message_id)?;
            blocking(move || {
                repo.set_thread(guild, id, thread_id, message)
                    .map_err(|e| e.to_string())
            })
            .await?;
        }
        // Silent mentions subscribe participants without Discord's noisy member-add event.
        let key = format!("deadlock:{guild}:{id}:participants");
        if self
            .discord
            .find_message_by_delivery_key(thread, &key, game.created_at, 100)
            .await?
            .is_none()
        {
            let mentions = game
                .team1
                .iter()
                .chain(&game.team2)
                .map(|p| format!("<@{}>", p.discord_id))
                .collect::<Vec<_>>()
                .join(" ");
            self.discord
                .send_message_with_delivery_key(
                    thread,
                    &key,
                    DiscordMessage::silent(InteractionResponse::message(format!(
                        "Match participants: {mentions}"
                    ))),
                )
                .await?;
        }
        Ok(thread)
    }
}

fn market_summary(market: Option<&DeadlockMarket>, at: i64) -> String {
    match market {
        None => "Betting setup pending.".into(),
        Some(m) => format!(
            "JC pool: Team 1 {} · Team 2 {} · seed {}\nBetting: {} · deadline <t:{}:R>",
            m.pool1,
            m.pool2,
            m.seed,
            if m.status == "open" && m.deadline <= at {
                "closed"
            } else {
                &m.status
            },
            m.deadline
        ),
    }
}
fn rating_seeds(imported: ImportedRatings, _at: i64) -> Vec<DeadlockSeed> {
    [
        (DeadlockFormat::Standard, imported.standard),
        (DeadlockFormat::StreetBrawl, imported.brawl),
    ]
    .into_iter()
    .map(|(format, r)| DeadlockSeed {
        format,
        mu: r.mu,
        sigma: r.sigma,
        source: r.source,
        source_value: r.raw_value,
        provenance: Some(r.provenance.to_string()),
        source_at: r
            .provenance
            .get("source_time")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|time| time.timestamp()),
    })
    .collect()
}

fn rating_source_label(source: &str) -> &'static str {
    if source.starts_with("statlocker") {
        "Statlocker (provisional)"
    } else if source.starts_with("valve") {
        "Valve ranked badge (provisional)"
    } else if source.starts_with("standard-weak") {
        "Standard skill estimate (provisional)"
    } else {
        "Neutral provisional rating"
    }
}

fn registration_import_reason(imported: &ImportedRatings) -> String {
    if imported.standard.source == "provisional-v1" {
        "\nNo external rating was available; starting with the default rating.".to_owned()
    } else {
        String::new()
    }
}

fn statlocker_profile_url(account: u32) -> String {
    format!("https://statlocker.gg/profile/{account}")
}

fn statlocker_player_link(name: &str, account: i64) -> String {
    // Bound and escape the display label, preserving readable names in Discord links.
    let mut label: String = name.chars().take(32).collect();
    if name.chars().count() > 32 {
        label.push('…');
    }
    let mut escaped = String::new();
    for ch in label.chars() {
        if matches!(ch, '\\' | '[' | ']' | '*' | '_' | '`' | '~' | '|') {
            escaped.push('\\');
        }
        if !ch.is_control() {
            escaped.push(ch);
        }
    }
    match u32::try_from(account).ok().filter(|id| *id > 0) {
        Some(account) => format!("[{escaped}]({})", statlocker_profile_url(account)),
        None => escaped,
    }
}

fn registration_import_summary(imported: &ImportedRatings) -> String {
    if imported.standard.source == "provisional-v1" {
        return format!(
            "Standard **{:.1}** · Brawl **{:.1}**.{}",
            imported.standard.mu,
            imported.brawl.mu,
            registration_import_reason(imported)
        );
    }
    let source = rating_source_label(&imported.standard.source);
    format!(
        "Standard **{:.1}** · Brawl **{:.1}**.\nInitial rating: {source}.{}",
        imported.standard.mu,
        imported.brawl.mu,
        registration_import_reason(imported)
    )
}

fn local_rating_summary(ratings: &[cama_db::deadlock::DeadlockRating]) -> String {
    [DeadlockFormat::Standard, DeadlockFormat::StreetBrawl]
        .into_iter()
        .filter_map(|format| {
            ratings
                .iter()
                .find(|rating| rating.format == format)
                .map(|rating| {
                    format!(
                        "{} **{:.1}**",
                        if format == DeadlockFormat::Standard {
                            "Standard"
                        } else {
                            "Brawl"
                        },
                        rating.mu
                    )
                })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

struct DeadlockRecovery {
    handler: Arc<DeadlockHandler>,
}
#[async_trait]
impl BackgroundWorker for DeadlockRecovery {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        let mut published = BTreeMap::<(i64, i64), String>::new();
        while !context.shutdown_requested() {
            let mut retained = BTreeSet::new();
            let bets = DeadlockBettingRepository::new(&self.handler.path);
            let results = blocking(move || bets.recover(now()).map_err(|e| e.to_string())).await?;
            for (guild, game, result) in results {
                if let Err(error) = result {
                    tracing::warn!(guild,game,%error,"Deadlock settlement recovery pending");
                }
            }
            let path = self.handler.path.clone();
            let blood =
                blocking(move || crate::deadlock_economy_hooks::recover_blood_pacts(&path)).await?;
            for (key, result) in blood {
                if let Err(error) = result {
                    tracing::warn!(%key,%error,"Deadlock profit deduction recovery pending");
                }
            }
            for &guild in &self.handler.config.guild_ids {
                let repo = DeadlockRepository::new(&self.handler.path);
                let games = blocking(move || {
                    repo.publication_candidates(guild)
                        .map_err(|e| e.to_string())
                })
                .await?;
                for game in games {
                    let id = game.match_id;
                    retained.insert((guild, id));
                    let rendered = match self.handler.match_response(guild, id).await {
                        Ok(response) => response,
                        Err(error) => {
                            tracing::warn!(guild,id,%error,"Deadlock match rendering will retry");
                            continue;
                        }
                    };
                    // Stable text has no ticking countdown, so unchanged messages need no edit.
                    if published.get(&(guild, id)) == Some(&rendered.content) {
                        continue;
                    }
                    match self.handler.publish_match(guild, id).await {
                        Ok(_) => {
                            published.insert((guild, id), rendered.content);
                        }
                        Err(error) => {
                            tracing::warn!(guild,id,%error,"Deadlock match publication will retry")
                        }
                    }
                }
            }
            published.retain(|key, _| retained.contains(key));
            if !context.sleep(Duration::from_secs(15)).await {
                break;
            }
        }
        Ok(())
    }
}
