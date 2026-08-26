use std::sync::Mutex;

use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use cama_domain::embed_safety::EmbedModel;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, RegistryBuilder};

const ADMIN: u64 = 42;
const GUILD: u64 = 4242;

#[derive(Default)]
struct FakeWorker {
    calls: Mutex<Vec<RatingAnalysisInput>>,
    result: Mutex<Option<Result<CommandPlan, String>>>,
}

impl FakeWorker {
    fn returning(plan: CommandPlan) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(Some(Ok(plan))),
        }
    }
}

impl RatingAnalysisWorkPort for FakeWorker {
    fn execute(&self, input: RatingAnalysisInput) -> Result<CommandPlan, String> {
        self.calls.lock().unwrap().push(input);
        self.result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(CommandPlan::default()))
    }
}

#[derive(Default)]
struct Responder {
    responses: Mutex<Vec<InteractionResponse>>,
    followups: Mutex<Vec<InteractionResponse>>,
    defers: Mutex<Vec<bool>>,
    fail_defer: bool,
}

#[async_trait]
impl InteractionResponder for Responder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().unwrap().push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().unwrap().push(ephemeral);
        if self.fail_defer {
            Err(InteractionResponseError::new("expired"))
        } else {
            Ok(())
        }
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().unwrap().push(response);
        Ok(())
    }
}

fn provider(worker: Arc<dyn RatingAnalysisWorkPort>) -> RatingAnalysisRegistrationProvider {
    RatingAnalysisRegistrationProvider::with_worker(BTreeSet::from([ADMIN as i64]), worker)
}

fn command(
    user_id: u64,
    guild_id: Option<u64>,
    action: &str,
    selected: Option<(u64, &str)>,
    permissions: Option<u64>,
) -> InteractionRequest {
    let mut options = vec![InteractionOption {
        name: "action".to_owned(),
        value: InteractionValue::String(action.to_owned()),
    }];
    if let Some((id, name)) = selected {
        options.push(InteractionOption {
            name: "user".to_owned(),
            value: InteractionValue::User {
                id,
                display_name: Some(name.to_owned()),
                is_bot: Some(false),
            },
        });
    }
    InteractionRequest::Command {
        interaction_id: 9,
        name: "ratinganalysis".to_owned(),
        user_id,
        user_display_name: if user_id == ADMIN { "Admin" } else { "Viewer" }.to_owned(),
        guild_id,
        channel_id: Some(7),
        member_permissions: permissions,
        options,
    }
}

fn registered_handler(
    provider: &RatingAnalysisRegistrationProvider,
) -> Arc<dyn InteractionHandler> {
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).unwrap();
    builder.build().command_handler("ratinganalysis").unwrap()
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some(ADMIN.to_string()),
        "NEW_PLAYER_MMR_DISCOUNT" => Some("1000".to_owned()),
        "OPENSKILL_CALIBRATION_SIGMA_THRESHOLD" => Some("2.5".to_owned()),
        "OPENSKILL_PERFORMANCE_STRENGTH" => Some("0.3".to_owned()),
        _ => None,
    })
    .unwrap()
}

fn migrated() -> (TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cama.sqlite3");
    initialize_or_migrate_with_settings(&path, &MigrationSettings::default()).unwrap();
    (directory, path)
}

fn connection(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
}

#[test]
fn slash_schema_is_the_exact_python_surface() {
    let provider = provider(Arc::new(FakeWorker::default()));
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).unwrap();
    let registry = builder.build();
    let command = registry.commands().next().unwrap();
    assert_eq!(command.name, "ratinganalysis");
    assert_eq!(
        command.description,
        "Analyze and compare rating systems (Admin only)"
    );
    assert_eq!(command.options.len(), 2);
    assert_eq!(command.options[0].name, "action");
    assert_eq!(command.options[0].description, "Action to perform");
    assert!(command.options[0].required);
    assert_eq!(command.options[0].choices.len(), 5);
    assert_eq!(
        command.options[0]
            .choices
            .iter()
            .map(|choice| match choice {
                CommandOptionChoice::String { name, value } => (name.as_str(), value.as_str()),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>(),
        vec![
            ("compare - Show Glicko vs OpenSkill comparison", "compare"),
            ("calibration - Show calibration curves", "calibration"),
            ("trend - Show accuracy over time", "trend"),
            ("backfill - Recalculate OpenSkill from history", "backfill"),
            ("player - Show player's OpenSkill rating details", "player"),
        ]
    );
    assert_eq!(command.options[1].kind, CommandOptionKind::User);
    assert_eq!(
        command.options[1].description,
        "Optional: Analyze specific player's rating data"
    );
    assert!(!command.options[1].required);
}

#[test]
fn constructor_rejects_an_invalid_injected_probability_temperature() {
    let mut config = config();
    config.migration.openskill.win_probability_temperature = 0.0;

    assert!(matches!(
        RatingAnalysisRegistrationProvider::new("unused.sqlite3", &config),
        Err(RatingAnalysisProviderBuildError::OpenSkill(
            OpenSkillError::InvalidProbabilityTemperature(value)
        )) if value == 0.0
    ));
}

#[tokio::test]
async fn guild_admin_and_unknown_gates_are_initial_ephemeral_responses() {
    let worker = Arc::new(FakeWorker::default());
    let handler = registered_handler(&provider(worker.clone()));

    for (request, expected) in [
        (command(ADMIN, None, "compare", None, None), GUILD_ONLY),
        (command(7, Some(GUILD), "compare", None, None), ADMIN_ONLY),
        (
            command(ADMIN, Some(GUILD), "bogus", None, None),
            "Unknown action: bogus",
        ),
    ] {
        let responder = Arc::new(Responder::default());
        handler.handle(request, responder.clone()).await.unwrap();
        let responses = responder.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].content, expected);
        assert!(responses[0].ephemeral);
        assert_eq!(
            responses[0].allowed_mentions,
            InteractionAllowedMentions::None
        );
        assert!(responder.defers.lock().unwrap().is_empty());
    }
    assert!(worker.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn manage_guild_permission_defers_privately_then_posts_public_result() {
    let mut embed = EmbedModel {
        title: Some("Rating System Comparison".to_owned()),
        ..EmbedModel::default()
    };
    embed.add_field("Summary", "OpenSkill", false);
    let plan = CommandPlan {
        deferred_ephemeral: Some(true),
        deliveries: vec![Delivery {
            route: DeliveryRoute::Followup,
            content: None,
            embed: Some(embed),
            attachment: Some(cama_app::rating_analysis_command::AttachmentPlan {
                filename: "comparison.png".to_owned(),
                bytes: b"png".to_vec(),
            }),
            ephemeral: None,
        }],
    };
    let worker = Arc::new(FakeWorker::returning(plan));
    let handler = registered_handler(&provider(worker.clone()));
    let responder = Arc::new(Responder::default());
    handler
        .handle(
            command(
                7,
                Some(GUILD),
                "compare",
                None,
                Some(MANAGE_GUILD_PERMISSION),
            ),
            responder.clone(),
        )
        .await
        .unwrap();
    assert_eq!(*responder.defers.lock().unwrap(), vec![true]);
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), 1);
    assert!(!followups[0].ephemeral);
    assert_eq!(followups[0].attachments[0].filename, "comparison.png");
    assert_eq!(
        followups[0].embeds[0].image_url.as_deref(),
        Some("attachment://comparison.png")
    );
    assert_eq!(followups[0].embeds[0].color, Some(BLUE));
}

#[tokio::test]
async fn defer_failure_matches_safe_defer_and_does_not_run_work() {
    let worker = Arc::new(FakeWorker::default());
    let handler = registered_handler(&provider(worker.clone()));
    let responder = Arc::new(Responder {
        fail_defer: true,
        ..Responder::default()
    });
    handler
        .handle(
            command(ADMIN, Some(GUILD), "compare", None, None),
            responder.clone(),
        )
        .await
        .unwrap();
    assert!(worker.calls.lock().unwrap().is_empty());
    assert!(responder.followups.lock().unwrap().is_empty());
}

#[tokio::test]
async fn player_worker_failure_matches_the_global_python_error_followup() {
    let worker = Arc::new(FakeWorker {
        calls: Mutex::new(Vec::new()),
        result: Mutex::new(Some(Err("database path is unavailable".to_owned()))),
    });
    let handler = registered_handler(&provider(worker));
    let responder = Arc::new(Responder::default());
    handler
        .handle(
            command(ADMIN, Some(GUILD), "player", None, None),
            responder.clone(),
        )
        .await
        .unwrap();

    assert_eq!(*responder.defers.lock().unwrap(), vec![true]);
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), 1);
    assert_eq!(
        followups[0].content,
        "❌ An error occurred while processing your command. Please try again."
    );
    assert!(followups[0].ephemeral);
    assert_eq!(
        followups[0].allowed_mentions,
        InteractionAllowedMentions::None
    );
}

#[tokio::test]
async fn migrated_sqlite_player_action_uses_configured_calibration_and_real_history() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    connection.execute(
        "INSERT INTO players(discord_id,guild_id,discord_username,glicko_rating,glicko_rd,os_mu,os_sigma)
         VALUES(9,?1,'Newbie',1400,200,30,3)",
        [GUILD as i64],
    ).unwrap();
    connection.execute(
        "INSERT INTO matches(match_id,guild_id,winning_team,match_date,team1_players,team2_players)
         VALUES(91,?1,1,'2026-01-01 12:00:00','[9]','[10]')",
        [GUILD as i64],
    ).unwrap();
    connection.execute(
        "INSERT INTO rating_history(discord_id,guild_id,match_id,os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,won,fantasy_weight,streak_length,streak_multiplier)
         VALUES(9,?1,91,29,30,3.2,3,1,1.125,4,1.2)",
        [GUILD as i64],
    ).unwrap();
    let provider = RatingAnalysisRegistrationProvider::new(&path, &config()).unwrap();
    let handler = registered_handler(&provider);
    let responder = Arc::new(Responder::default());
    handler
        .handle(
            command(ADMIN, Some(GUILD), "player", Some((9, "Newbie")), None),
            responder.clone(),
        )
        .await
        .unwrap();
    assert_eq!(*responder.defers.lock().unwrap(), vec![true]);
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), 1);
    assert!(!followups[0].ephemeral);
    let embed = &followups[0].embeds[0];
    assert_eq!(embed.title.as_deref(), Some("OpenSkill Rating: 9"));
    let fields = embed
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.value.as_str()))
        .collect::<HashMap<_, _>>();
    assert!(fields["Calibrated"].contains("2.5"));
    assert!(fields["Recent OpenSkill Changes"].contains("perf×1.125"));
    assert!(fields["Recent OpenSkill Changes"].contains("streak W4×1.20"));
}

#[tokio::test]
async fn migrated_sqlite_comparison_delivers_native_semantic_png() {
    let (_directory, path) = migrated();
    let mut connection = connection(&path);
    let transaction = connection.transaction().unwrap();
    for index in 0_i64..10 {
        transaction.execute(
            "INSERT INTO matches(match_id,guild_id,winning_team,match_date,team1_players,team2_players)
             VALUES(?1,?2,?3,?4,'[1]','[2]')",
            params![100 + index, GUILD as i64, if index % 2 == 0 { 1 } else { 2 }, format!("2026-01-{:02} 12:00:00", index + 1)],
        ).unwrap();
        transaction.execute(
            "INSERT INTO match_predictions(match_id,expected_radiant_win_prob,openskill_radiant_win_prob,openskill_raw_radiant_win_prob)
             VALUES(?1,?2,?3,?4)",
            params![100 + index, if index % 2 == 0 { 0.7 } else { 0.3 }, if index % 2 == 0 { 0.8 } else { 0.2 }, if index % 2 == 0 { 0.82 } else { 0.18 }],
        ).unwrap();
    }
    transaction.commit().unwrap();
    let provider = RatingAnalysisRegistrationProvider::new(&path, &config()).unwrap();
    let handler = registered_handler(&provider);
    let responder = Arc::new(Responder::default());
    handler
        .handle(
            command(ADMIN, Some(GUILD), "compare", None, None),
            responder.clone(),
        )
        .await
        .unwrap();
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), 1);
    assert_eq!(
        followups[0].embeds[0].title.as_deref(),
        Some("Rating System Comparison")
    );
    assert_eq!(followups[0].attachments[0].filename, "comparison.png");
    assert_eq!(
        &followups[0].attachments[0].bytes[..8],
        b"\x89PNG\r\n\x1a\n"
    );
    assert!(followups[0].attachments[0].bytes.len() > 1_000_000);
}

#[tokio::test]
async fn migrated_sqlite_backfill_sends_start_first_and_persists_configured_seed() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,initial_mmr)
         VALUES(1,?1,'Seeded',4000)",
            [GUILD as i64],
        )
        .unwrap();
    let provider = RatingAnalysisRegistrationProvider::new(&path, &config()).unwrap();
    let handler = registered_handler(&provider);
    let responder = Arc::new(Responder::default());
    handler
        .handle(
            command(ADMIN, Some(GUILD), "backfill", None, None),
            responder.clone(),
        )
        .await
        .unwrap();
    let followups = responder.followups.lock().unwrap();
    assert_eq!(followups.len(), 2);
    assert_eq!(followups[0].content, STARTING_BACKFILL);
    assert!(followups[0].ephemeral);
    assert_eq!(
        followups[1].embeds[0].title.as_deref(),
        Some("OpenSkill Backfill Complete")
    );
    assert_eq!(followups[1].embeds[0].color, Some(GREEN));
    let fields = &followups[1].embeds[0].fields;
    assert!(
        fields
            .iter()
            .any(|field| field.name == "Matches Processed" && field.value == "0/0")
    );
    assert!(
        fields
            .iter()
            .any(|field| field.name == "Players Updated" && field.value == "1")
    );
    drop(followups);
    let mu: f64 = connection
        .query_row(
            "SELECT os_mu FROM players WHERE discord_id=1 AND guild_id=?1",
            [GUILD as i64],
            |row| row.get(0),
        )
        .unwrap();
    let expected = CamaOpenSkillSystem::with_config(config().migration.openskill)
        .unwrap()
        .mmr_to_os_mu(3000);
    assert_eq!(mu, expected);
}

#[test]
fn production_backfill_without_reset_is_rejected_before_touching_sqlite() {
    let (_directory, path) = migrated();
    let mut port = ProductionMatchPort {
        repository: RatingAnalysisRepository::new(
            &path,
            CamaOpenSkillSystem::new(),
            cama_domain::rating::CamaRatingSystem::default(),
            500,
        ),
        history: Vec::new(),
    };
    let error = port
        .backfill_openskill_ratings(Some(GUILD as i64), false)
        .expect_err("replaying without reset must fail closed");
    assert_eq!(
        error,
        "reset_first=False is unsupported because it reapplies complete history on top of already-final ratings"
    );
    assert_eq!(
        connection(&path)
            .query_row("SELECT COUNT(*) FROM openskill_replay_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
