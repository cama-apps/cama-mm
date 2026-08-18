use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::draft::DraftStateManager;
use cama_app::embeds::LobbyKind;
use cama_app::herogrid::{
    HeroGridAttachment, HeroGridCommandError, HeroGridCommandOutcome, HeroGridCommandRequest,
    HeroGridCommandService, HeroGridSource, HeroGridSources, Teams,
};
use cama_db::herogrid_repository::{HeroGridRepository, PersistedHeroGridSources};
use cama_runtime::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionAttachment,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use tracing::{error, warn};

// `discord.Color.blue()` in discord.py is the named blue 0x3498DB.
const DISCORD_BLUE: u32 = 0x34_98_db;
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const FRIENDLY_RENDER_ERROR: &str =
    "❌ Couldn't generate the hero grid right now — try again in a moment.";

pub struct HeroGridRegistrationProvider {
    handler: Arc<HeroGridHandler>,
}

impl HeroGridRegistrationProvider {
    #[must_use]
    pub fn new(db_path: impl AsRef<Path>, draft_states: Arc<DraftStateManager>) -> Self {
        let repository = HeroGridRepository::new(db_path);
        Self {
            handler: Arc::new(HeroGridHandler {
                service: HeroGridCommandService::new(repository.clone()),
                repository,
                draft_states,
            }),
        }
    }
}

impl RegistrationProvider for HeroGridRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "herogrid".to_owned(),
            description: "Generate a player x hero grid showing hero pools and win rates"
                .to_owned(),
            options: vec![
                CommandOptionSpec {
                    choices: vec![
                        string_choice("Auto (lobby if available)", "auto"),
                        string_choice("Current Lobby", "lobby"),
                        string_choice("All Players", "all"),
                    ],
                    ..CommandOptionSpec::new(
                        "source",
                        "Player source: auto picks lobby if available, otherwise all players",
                        CommandOptionKind::String,
                    )
                },
                CommandOptionSpec::new(
                    "min_games",
                    "Minimum games on a hero for it to appear (default: 2)",
                    CommandOptionKind::Integer,
                ),
                CommandOptionSpec::new(
                    "limit",
                    "Maximum number of players to include",
                    CommandOptionKind::Integer,
                ),
                CommandOptionSpec {
                    choices: vec![
                        string_choice("🍽️ All You Can Feed", "open"),
                        string_choice("🧀 Whine & Cheese", "lowskill"),
                    ],
                    ..CommandOptionSpec::new(
                        "lobby",
                        "Lobby to read (defaults to the lobby you're in)",
                        CommandOptionKind::String,
                    )
                },
            ],
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

struct HeroGridHandler {
    service: HeroGridCommandService<HeroGridRepository>,
    repository: HeroGridRepository,
    draft_states: Arc<DraftStateManager>,
}

#[async_trait]
impl InteractionHandler for HeroGridHandler {
    async fn handle(
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
            return Err("Hero Grid handler received a component interaction".into());
        };
        if name != "herogrid" {
            return Err(format!("Hero Grid handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            return responder
                .respond(InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral())
                .await
                .map_err(|error| error.to_string().into());
        };
        let guild_id = i64::try_from(guild_id)
            .map_err(|_| "Discord guild id exceeds SQLite's signed integer range".to_owned())?;
        let user_id = i64::try_from(user_id)
            .map_err(|_| "Discord user id exceeds SQLite's signed integer range".to_owned())?;

        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer Hero Grid interaction");
            return Ok(());
        }

        let source = parse_source(string_option(&options, "source"))?;
        let explicit_lobby = parse_lobby(string_option(&options, "lobby"))?;
        let min_games = integer_option(&options, "min_games").unwrap_or(2);
        let limit = integer_option(&options, "limit");
        let repository = self.repository.clone();
        let service = self.service.clone();
        let draft_states = Arc::clone(&self.draft_states);
        let outcome = tokio::task::spawn_blocking(move || {
            // Python's explicit `all` source returns before consulting any
            // lobby, pending-match, draft, or recent-match state.
            let persisted = if source == HeroGridSource::All {
                PersistedHeroGridSources::default()
            } else {
                repository
                    .get_persisted_sources(Some(guild_id), Some(user_id))
                    .map_err(|error| HeroGridFailure::Data(error.to_string()))?
            };
            let sources = runtime_sources(persisted, &draft_states, guild_id);
            service
                .execute(&HeroGridCommandRequest {
                    source,
                    guild_id: Some(guild_id),
                    invoking_user_id: Some(user_id),
                    explicit_lobby,
                    min_games,
                    limit,
                    sources: &sources,
                })
                .map_err(|error| match error {
                    HeroGridCommandError::Data(error) => HeroGridFailure::Data(error.to_string()),
                    HeroGridCommandError::Render(error) => {
                        HeroGridFailure::Render(error.to_string())
                    }
                })
        })
        .await
        .map_err(|error| format!("Hero Grid blocking task failed: {error}"))?;

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(HeroGridFailure::Render(message)) => {
                error!(%message, "Hero Grid image generation failed");
                return responder
                    .followup(InteractionResponse::message(FRIENDLY_RENDER_ERROR))
                    .await
                    .map_err(|error| error.to_string().into());
            }
            Err(HeroGridFailure::Data(message)) => return Err(message.into()),
        };
        responder
            .followup(outcome_response(outcome))
            .await
            .map_err(|error| error.to_string().into())
    }
}

enum HeroGridFailure {
    Data(String),
    Render(String),
}

fn runtime_sources(
    persisted: PersistedHeroGridSources,
    draft_states: &DraftStateManager,
    guild_id: i64,
) -> HeroGridSources {
    let draft_pool_ids = draft_states.get_state(Some(guild_id)).map(|state| {
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .player_pool_ids
            .clone()
    });
    HeroGridSources {
        open_lobby: persisted.open_lobby,
        low_skill_lobby: persisted.low_skill_lobby,
        invoking_user_lobbies: persisted
            .invoking_user_lobby_ids
            .into_iter()
            .map(|lobby_id| {
                if lobby_id == 2 {
                    LobbyKind::LowSkill
                } else {
                    LobbyKind::Open
                }
            })
            .collect(),
        invoking_pending_match: persisted.invoking_pending_match.map(teams),
        guild_pending_match: persisted.guild_pending_match.map(teams),
        invoking_pending_lookup_available: true,
        draft_pool_ids,
        last_match_ids: persisted.last_match_ids,
        ..HeroGridSources::default()
    }
}

fn teams(value: cama_db::herogrid_repository::HeroGridTeams) -> Teams {
    Teams::new(&value.radiant, &value.dire)
}

fn parse_source(value: Option<&str>) -> Result<HeroGridSource, String> {
    match value.unwrap_or("auto") {
        "auto" => Ok(HeroGridSource::Auto),
        "lobby" => Ok(HeroGridSource::Lobby),
        "all" => Ok(HeroGridSource::All),
        value => Err(format!("invalid Hero Grid source {value:?}")),
    }
}

fn parse_lobby(value: Option<&str>) -> Result<Option<LobbyKind>, String> {
    match value {
        None => Ok(None),
        Some("open") => Ok(Some(LobbyKind::Open)),
        Some("lowskill") => Ok(Some(LobbyKind::LowSkill)),
        Some(value) => Err(format!("invalid Hero Grid lobby {value:?}")),
    }
}

fn string_option<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a str> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| {
                if let InteractionValue::String(value) = value {
                    Some(value.as_str())
                } else {
                    None
                }
            })
    })
}

fn integer_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| {
                if let InteractionValue::Integer(value) = value {
                    Some(*value)
                } else {
                    None
                }
            })
    })
}

fn outcome_response(outcome: HeroGridCommandOutcome) -> InteractionResponse {
    match outcome {
        HeroGridCommandOutcome::Message(message) => InteractionResponse::message(message),
        HeroGridCommandOutcome::Attachment(attachment) => attachment_response(attachment),
    }
}

fn attachment_response(attachment: HeroGridAttachment) -> InteractionResponse {
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled(attachment.title)
                .description(attachment.description)
                .color(DISCORD_BLUE)
                .image(format!("attachment://{}", attachment.filename))
                .footer(attachment.footer),
        )
        .attachment(InteractionAttachment::bytes(
            attachment.filename,
            attachment.png,
        ))
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use std::sync::Mutex;

    use cama_db::schema_manager::initialize_or_migrate;
    use cama_runtime::registration::InteractionResponseError;
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::*;

    #[derive(Default)]
    struct CapturedResponses {
        deferred: Vec<bool>,
        immediate: Vec<InteractionResponse>,
        followups: Vec<InteractionResponse>,
    }

    #[derive(Default)]
    struct CapturingResponder {
        captured: Mutex<CapturedResponses>,
    }

    #[async_trait]
    impl InteractionResponder for CapturingResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.captured
                .lock()
                .expect("capture mutex")
                .immediate
                .push(response);
            Ok(())
        }

        async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
            self.captured
                .lock()
                .expect("capture mutex")
                .deferred
                .push(ephemeral);
            Ok(())
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.captured
                .lock()
                .expect("capture mutex")
                .followups
                .push(response);
            Ok(())
        }
    }

    #[tokio::test]
    async fn real_database_renders_png_and_dispatches_discord_neutral_followup() {
        let database = NamedTempFile::new().expect("temporary DB");
        initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
        let connection = Connection::open(database.path()).expect("open fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite policy");
        connection
            .execute(
                "INSERT INTO players (discord_id,guild_id,discord_username) VALUES (?1,?2,?3)",
                params![100_i64, 42_i64, "Alice"],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO matches (match_id,guild_id,team1_players,team2_players,winning_team)
                 VALUES (1,42,'[100]','[200]',1)",
                [],
            )
            .expect("insert match");
        connection
            .execute(
                "INSERT INTO match_participants
                    (match_id,discord_id,guild_id,team_number,won,hero_id)
                 VALUES (1,100,42,1,1,7)",
                [],
            )
            .expect("insert participant");
        drop(connection);

        let provider = HeroGridRegistrationProvider::new(
            database.path(),
            Arc::new(DraftStateManager::default()),
        );
        let mut registry = RegistryBuilder::default();
        provider.register(&mut registry).expect("register provider");
        let registry = registry.build();
        let handler = registry
            .command_handler("herogrid")
            .expect("Hero Grid handler");
        let responder = Arc::new(CapturingResponder::default());
        handler
            .handle(
                InteractionRequest::Command {
                    interaction_id: 1,
                    name: "herogrid".to_owned(),
                    user_id: 100,
                    user_display_name: "test user".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(9),
                    member_permissions: None,
                    options: vec![InteractionOption {
                        name: "source".to_owned(),
                        value: InteractionValue::String("all".to_owned()),
                    }],
                },
                responder.clone(),
            )
            .await
            .expect("execute Hero Grid");

        let captured = responder.captured.lock().expect("capture mutex");
        assert_eq!(captured.deferred, [false]);
        assert!(captured.immediate.is_empty());
        assert_eq!(captured.followups.len(), 1);
        let response = &captured.followups[0];
        assert_eq!(response.embeds[0].title.as_deref(), Some("Hero Grid"));
        assert_eq!(
            response.embeds[0].description.as_deref(),
            Some("1 players | min 2 games per hero")
        );
        assert_eq!(
            response.embeds[0].image_url.as_deref(),
            Some("attachment://hero_grid.png")
        );
        assert_eq!(response.embeds[0].color, Some(DISCORD_BLUE));
        assert_eq!(
            response.embeds[0].footer.as_deref(),
            Some(
                "Circle size = games played | Color = win rate (green ≥60%, yellow ≥40%, red <40%)"
            )
        );
        assert_eq!(response.attachments[0].filename, "hero_grid.png");
        assert!(
            response.attachments[0]
                .bytes
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );
        assert!(!registry.global_command_sync_enabled());
    }

    #[test]
    fn registration_preserves_the_exact_python_option_contract() {
        let database = NamedTempFile::new().expect("temporary DB");
        let provider = HeroGridRegistrationProvider::new(
            database.path(),
            Arc::new(DraftStateManager::default()),
        );
        let mut registry = RegistryBuilder::default();
        provider.register(&mut registry).expect("register provider");
        let registry = registry.build();
        let command = registry.commands().next().expect("Hero Grid command");

        assert_eq!(command.name, "herogrid");
        assert_eq!(
            command.description,
            "Generate a player x hero grid showing hero pools and win rates"
        );
        assert_eq!(
            command
                .options
                .iter()
                .map(|option| (
                    option.name.as_str(),
                    option.description.as_str(),
                    option.kind,
                    option.required,
                    option.choices.clone(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "source",
                    "Player source: auto picks lobby if available, otherwise all players",
                    CommandOptionKind::String,
                    false,
                    vec![
                        string_choice("Auto (lobby if available)", "auto"),
                        string_choice("Current Lobby", "lobby"),
                        string_choice("All Players", "all"),
                    ],
                ),
                (
                    "min_games",
                    "Minimum games on a hero for it to appear (default: 2)",
                    CommandOptionKind::Integer,
                    false,
                    Vec::new(),
                ),
                (
                    "limit",
                    "Maximum number of players to include",
                    CommandOptionKind::Integer,
                    false,
                    Vec::new(),
                ),
                (
                    "lobby",
                    "Lobby to read (defaults to the lobby you're in)",
                    CommandOptionKind::String,
                    false,
                    vec![
                        string_choice("🍽️ All You Can Feed", "open"),
                        string_choice("🧀 Whine & Cheese", "lowskill"),
                    ],
                ),
            ]
        );
        assert!(!registry.global_command_sync_enabled());
    }

    #[test]
    fn persisted_and_live_sources_bridge_lobby_pending_draft_and_recent_state() {
        let database = NamedTempFile::new().expect("temporary DB");
        initialize_or_migrate(database.path()).expect("initialize schema");
        let connection = Connection::open(database.path()).expect("open fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite policy");
        connection
            .execute(
                "INSERT INTO lobby_state (lobby_id,guild_id,players,status)
                 VALUES (1,42,'[10,11]','open'), (2,42,'[10,12]','open')",
                [],
            )
            .expect("insert lobbies");
        connection
            .execute(
                "INSERT INTO pending_matches (guild_id,payload)
                 VALUES (42,?1), (42,?2)",
                params![
                    r#"{"radiant_team_ids":[10,20],"dire_team_ids":[30]}"#,
                    r#"{"radiant_team_ids":[40],"dire_team_ids":[50]}"#,
                ],
            )
            .expect("insert pending matches");
        connection
            .execute(
                "INSERT INTO matches (match_id,guild_id,team1_players,team2_players,winning_team)
                 VALUES (1,42,'[70,71]','[71,72]',1)",
                [],
            )
            .expect("insert recent match");
        drop(connection);

        let persisted = HeroGridRepository::new(database.path())
            .get_persisted_sources(Some(42), Some(10))
            .expect("read persisted sources");
        assert_eq!(persisted.open_lobby, [10, 11]);
        assert_eq!(persisted.low_skill_lobby, [10, 12]);
        assert_eq!(persisted.invoking_user_lobby_ids, [1, 2]);
        assert_eq!(
            persisted.invoking_pending_match,
            Some(cama_db::herogrid_repository::HeroGridTeams {
                radiant: vec![10, 20],
                dire: vec![30],
            })
        );
        assert_eq!(persisted.guild_pending_match, None);
        assert_eq!(persisted.last_match_ids, [70, 71, 72]);

        let drafts = DraftStateManager::default();
        let draft = drafts
            .create_draft(Some(42), cama_app::draft::LobbyKind::Open)
            .expect("create live draft");
        draft
            .lock()
            .expect("draft mutex")
            .player_pool_ids
            .clone_from(&vec![60, 61]);
        let sources = runtime_sources(persisted, &drafts, 42);
        assert_eq!(
            sources.invoking_user_lobbies,
            [LobbyKind::Open, LobbyKind::LowSkill]
        );
        assert_eq!(sources.draft_pool_ids, Some(vec![60, 61]));
        assert_eq!(sources.last_match_ids, [70, 71, 72]);
    }

    #[tokio::test]
    async fn direct_message_is_rejected_before_deferral() {
        let database = NamedTempFile::new().expect("temporary DB");
        let provider = HeroGridRegistrationProvider::new(
            database.path(),
            Arc::new(DraftStateManager::default()),
        );
        let mut registry = RegistryBuilder::default();
        provider.register(&mut registry).expect("register provider");
        let handler = registry
            .build()
            .command_handler("herogrid")
            .expect("Hero Grid handler");
        let responder = Arc::new(CapturingResponder::default());
        handler
            .handle(
                InteractionRequest::Command {
                    interaction_id: 1,
                    name: "herogrid".to_owned(),
                    user_id: 100,
                    user_display_name: "test user".to_owned(),
                    guild_id: None,
                    channel_id: Some(9),
                    member_permissions: None,
                    options: Vec::new(),
                },
                responder.clone(),
            )
            .await
            .expect("reject DM");
        let captured = responder.captured.lock().expect("capture mutex");
        assert!(captured.deferred.is_empty());
        assert_eq!(
            captured.immediate,
            [InteractionResponse::message(GUILD_ONLY_MESSAGE).ephemeral()]
        );
    }
}
