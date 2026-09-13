//! Guild-scoped Dota settings and durable operator requests.

use cama_db::guild_config_repository::GuildConfigRepository;
use cama_domain::dota_hosting::{DotaHostingOptions, FirstPick, HostingMode, StartMode};

use super::*;

impl AdminHandler {
    pub(super) async fn dota_command(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        if context.guild_id <= 0 {
            return respond_ephemeral(&responder, GUILD_ONLY).await;
        }
        let action = context.path.get(1).map_or("", String::as_str);
        let patch = if matches!(action, "settings" | "configure") {
            let parsed = if action == "configure" {
                parse_active_settings(&context.options)
            } else {
                parse_settings(&context.options)
            };
            match parsed {
                Ok(value) => Some(value),
                Err(error) => return respond_ephemeral(&responder, error).await,
            }
        } else {
            None
        };
        let pending = match context
            .options
            .iter()
            .find(|option| option.name == "pending_match")
        {
            None => None,
            Some(InteractionOption {
                value: InteractionValue::Integer(value),
                ..
            }) if *value > 0 => Some(*value),
            Some(_) => {
                return respond_ephemeral(
                    &responder,
                    "Pending match ID must be a positive integer.",
                )
                .await;
            }
        };
        // Never persist a request whose interaction could not be acknowledged.
        defer(&responder, true).await?;
        let path = self.monitoring.database_path.clone();
        let guild_id = context.guild_id;
        let result = match action {
            "settings" => {
                let patch = patch.expect("settings parsed above");
                let updating = !context.options.is_empty();
                tokio::task::spawn_blocking(move || {
                    let repository = GuildConfigRepository::new(path, false);
                    let settings = if updating {
                        repository.update_dota_hosting_options(guild_id, &patch)
                    } else {
                        repository.dota_hosting_options(guild_id)
                    }
                    .map_err(|error| error.to_string())?;
                    Ok(format_settings(&settings, updating))
                })
                .await
                .map_err(|error| format!("Dota settings task failed: {error}"))?
            }
            "configure" => crate::dota_host::guild_configure_command(
                path, guild_id, pending, context.actor_id, patch.expect("configure parsed above"),
            ).await.map(|message| format!(
                "{message}\nChanges are queued, not yet confirmed applied. Use `/admin dota status` to check. Future-shuffle defaults are unchanged."
            )),
            "reset" => tokio::task::spawn_blocking(move || {
                GuildConfigRepository::new(path, false)
                    .reset_dota_hosting_options(guild_id)
                    .map_err(|error| error.to_string())?;
                Ok("Dota settings reset to deployment defaults for future shuffles. Existing matches keep their saved settings.".to_owned())
            })
            .await
            .map_err(|error| format!("Dota settings task failed: {error}"))?,
            "betting" => {
                let action = required_string(&context.options, "action")?;
                let reason = required_string(&context.options, "reason")?;
                let actor = i64::try_from(context.actor_id).map_err(|_| "Operator ID is out of range")?;
                if !matches!(action.as_str(), "suspend" | "resume") {
                    return respond_ephemeral(&responder, "Betting action must be suspend or resume.").await;
                }
                tokio::task::spawn_blocking(move || {
                    let repo = cama_db::match_runtime::PendingMatchRepository::new(path);
                    let pending = if let Some(id) = pending { id } else {
                        let matches = repo.pending_matches(guild_id).map_err(|e| e.to_string())?;
                        match matches.as_slice() {
                            [pending] => pending.pending_match_id,
                            [] => return Err("No pending match in this server.".to_owned()),
                            _ => return Err("Several matches are pending; specify pending_match.".to_owned()),
                        }
                    };
                    let suspended = action == "suspend";
                    if !repo.set_betting_suspended_audited(guild_id, pending, suspended, actor, &reason,
                        chrono::Utc::now().timestamp()).map_err(|e| e.to_string())? {
                        return Err("Pending match was not found in this server.".to_owned());
                    }
                    Ok(if suspended {format!("Betting suspended for pending #{pending}, including admin extensions. Hosting continues. The operator and reason were saved.")}
                        else {format!("Betting suspension removed for pending #{pending}. Existing deadlines, gameplay closure, and observation requirements still apply. The operator and reason were saved.")})
                }).await.map_err(|e| format!("Betting control task failed: {e}"))?
            }
            "resolve" => {
                let pending = pending.ok_or("pending_match is required")?;
                let outcome = required_string(&context.options, "outcome")?;
                let valve_id = required_integer(&context.options, "dota_match")?;
                let reason = required_string(&context.options, "reason")?;
                let valve_id = u64::try_from(valve_id).map_err(|_| "Dota match ID must be nonnegative")?;
                crate::dota_host::guild_resolution_command(path, guild_id, pending, context.actor_id,
                    outcome.to_owned(), valve_id, reason.to_owned()).await
            }
            _ => crate::dota_host::guild_operator_command(path, action, guild_id, pending).await,
        };
        respond_ephemeral(
            &responder,
            result.unwrap_or_else(|error| format!("Dota hosting: {error}")),
        )
        .await
    }
}

fn parse_active_settings(options: &[InteractionOption]) -> Result<DotaHostingOptions, String> {
    if options.iter().any(|option| option.name == "hosting") {
        return Err("Use `/admin dota manual` to hand hosting to a human.".to_owned());
    }
    let settings: Vec<_> = options
        .iter()
        .filter(|option| option.name != "pending_match")
        .cloned()
        .collect();
    if settings.is_empty() {
        return Err("Choose at least one setting to change in the active lobby.".to_owned());
    }
    parse_settings(&settings)
}

fn parse_settings(options: &[InteractionOption]) -> Result<DotaHostingOptions, String> {
    let mut settings = DotaHostingOptions::default();
    for option in options {
        match (option.name.as_str(), &option.value) {
            ("hosting", InteractionValue::String(value)) => {
                settings.hosting = Some(match value.as_str() {
                    "bot" => HostingMode::Bot,
                    "manual" => HostingMode::Manual,
                    _ => return Err("Hosting must be bot or manual.".to_owned()),
                });
            }
            ("first_pick", InteractionValue::String(value)) => {
                settings.first_pick = Some(match value.as_str() {
                    "radiant" => FirstPick::Radiant,
                    "dire" => FirstPick::Dire,
                    "random" => FirstPick::Random,
                    _ => return Err("First pick must be radiant, dire, or random.".to_owned()),
                });
            }
            ("start", InteractionValue::String(value)) => {
                settings.start = Some(match value.as_str() {
                    "automatic" => StartMode::Automatic,
                    "manual" => StartMode::Manual,
                    _ => return Err("Start must be automatic or manual.".to_owned()),
                });
            }
            (name, InteractionValue::Integer(value)) => {
                let value = u32::try_from(*value)
                    .map_err(|_| format!("{name} must be a nonnegative 32-bit integer."))?;
                match name {
                    "server_region" => settings.region = Some(value),
                    "game_mode" => settings.game_mode = Some(value),
                    "tv_delay" => settings.tv_delay = Some(value),
                    "league_id" => settings.league_id = Some(value),
                    "visibility" => settings.visibility = Some(value),
                    _ => return Err(format!("Unknown Dota setting: {name}.")),
                }
            }
            _ => return Err(format!("Invalid value for Dota setting {}.", option.name)),
        }
    }
    settings.validate()?;
    Ok(settings)
}

fn format_settings(settings: &DotaHostingOptions, updated: bool) -> String {
    let numeric = |value: Option<u32>| {
        value.map_or("deployment default".to_owned(), |value| value.to_string())
    };
    let hosting = match settings.hosting {
        Some(HostingMode::Bot) => "bot",
        Some(HostingMode::Manual) => "manual (human creates the Dota lobby)",
        None => "deployment default",
    };
    let first_pick = match settings.first_pick {
        Some(FirstPick::Radiant) => "Radiant",
        Some(FirstPick::Dire) => "Dire",
        Some(FirstPick::Random) => "random",
        None => "deployment default",
    };
    let start = match settings.start {
        Some(StartMode::Automatic) => "automatic",
        Some(StartMode::Manual) => "manual (/admin dota start once everyone is ready)",
        None => "deployment default",
    };
    format!(
        "{}\nHosting: {hosting}\nServer region: {}\nGame mode: {}\nTV delay: {}\nLeague: {}\nFirst pick: {first_pick}\nStart: {start}\nVisibility: {}\nApplies to future shuffles. Existing matches keep their saved settings. Use `/admin dota reset` to clear overrides.",
        if updated {
            "Dota guild settings saved."
        } else {
            "Dota guild settings"
        },
        numeric(settings.region),
        numeric(settings.game_mode),
        numeric(settings.tv_delay),
        numeric(settings.league_id),
        numeric(settings.visibility),
    )
}

fn bounded_integer(name: &str, description: &str, min: i64, max: i64) -> CommandOptionSpec {
    let mut result = option(name, description, CommandOptionKind::Integer);
    result.min_integer = Some(min);
    result.max_integer = Some(max);
    result
}

fn integer_choices(name: &str, description: &str, values: &[(&str, i64)]) -> CommandOptionSpec {
    let mut result = option(name, description, CommandOptionKind::Integer);
    result.choices = values
        .iter()
        .map(|(name, value)| CommandOptionChoice::Integer {
            name: (*name).to_owned(),
            value: *value,
        })
        .collect();
    result
}

pub(super) fn options() -> Vec<CommandOptionSpec> {
    let mut result = vec![
        subcommand(
            "settings",
            "Show or update defaults for future shuffles in this server",
            vec![
                choices(
                    option(
                        "hosting",
                        "Who creates the Dota lobby",
                        CommandOptionKind::String,
                    ),
                    &[("Bot", "bot"), ("Manual (human host)", "manual")],
                ),
                bounded_integer("server_region", "Dota server region ID (1-100)", 1, 100),
                integer_choices(
                    "game_mode",
                    "Dota game mode",
                    &[
                        ("All Pick", 1),
                        ("Captains Mode", 2),
                        ("Random Draft", 3),
                        ("Single Draft", 4),
                        ("All Random", 5),
                        ("Least Played", 12),
                        ("Captains Draft", 16),
                        ("Ability Draft", 18),
                        ("All Random Deathmatch", 20),
                        ("Ranked All Pick", 22),
                        ("Turbo", 23),
                    ],
                ),
                choices(
                    option(
                        "first_pick",
                        "Team with first pick in the Dota draft",
                        CommandOptionKind::String,
                    ),
                    &[
                        ("Radiant", "radiant"),
                        ("Dire", "dire"),
                        ("Random", "random"),
                    ],
                ),
                choices(
                    option(
                        "start",
                        "Start automatically when ready or wait for an admin",
                        CommandOptionKind::String,
                    ),
                    &[
                        ("Automatic", "automatic"),
                        ("Manual (/admin dota start)", "manual"),
                    ],
                ),
                bounded_integer(
                    "tv_delay",
                    "Dota TV delay enum: 0, 1 (1m), 2 (2m), 3 (5m), 4 (15m)",
                    0,
                    4,
                ),
                bounded_integer("league_id", "Dota league ID", 1, i64::from(u32::MAX)),
                integer_choices(
                    "visibility",
                    "Lobby visibility (joining still requires its password)",
                    &[("Public", 0), ("Unlisted", 2)],
                ),
            ],
        ),
        subcommand(
            "reset",
            "Reset future-shuffle Dota settings to deployment defaults",
            vec![],
        ),
    ];
    let mut active_settings: Vec<_> = result[0]
        .options
        .iter()
        .filter(|option| option.name != "hosting")
        .cloned()
        .collect();
    active_settings.push(bounded_integer(
        "pending_match",
        "Pending ID; omit when this server has exactly one active bot lobby",
        1,
        9_007_199_254_740_991,
    ));
    result.push(subcommand(
        "configure",
        "Queue settings for an existing prelaunch bot lobby; future defaults unchanged",
        active_settings,
    ));
    result.push(subcommand(
        "betting",
        "Suspend or resume wager admission with an audit reason",
        vec![
            choices(
                required(
                    "action",
                    "Suspend all wagers or remove the suspension",
                    CommandOptionKind::String,
                ),
                &[("Suspend", "suspend"), ("Resume", "resume")],
            ),
            required(
                "reason",
                "Audit reason (1-300 characters)",
                CommandOptionKind::String,
            ),
            bounded_integer(
                "pending_match",
                "Pending ID; omit if exactly one match is pending",
                1,
                9_007_199_254_740_991,
            ),
        ],
    ));
    result.push(subcommand(
        "resolve",
        "Resolve a finished hosted match and safely release the bot account",
        vec![
            required(
                "pending_match",
                "Pending match ID shown by status",
                CommandOptionKind::Integer,
            ),
            choices(
                required(
                    "outcome",
                    "Recorded result or void with refunds",
                    CommandOptionKind::String,
                ),
                &[
                    ("Recorded and settled", "recorded"),
                    ("Void and refund", "void"),
                ],
            ),
            required(
                "dota_match",
                "Exact Dota match ID from status; 0 only if unassigned",
                CommandOptionKind::Integer,
            ),
            required(
                "reason",
                "Audit reason (1-300 characters)",
                CommandOptionKind::String,
            ),
        ],
    ));
    for (name, description) in [
        ("status", "Show this server's active Dota hosting session"),
        (
            "start",
            "Request launch when all required players are correctly seated",
        ),
        ("cancel", "Stop hosting and clean up a pregame Dota lobby"),
        ("resume", "Resume a safely recoverable held hosting session"),
        ("manual", "Hand this pending match to a human Dota host"),
    ] {
        result.push(subcommand(
            name,
            description,
            vec![bounded_integer(
                "pending_match",
                "Pending match ID; omit when this server has exactly one active match",
                1,
                9_007_199_254_740_991,
            )],
        ));
    }
    result
}
