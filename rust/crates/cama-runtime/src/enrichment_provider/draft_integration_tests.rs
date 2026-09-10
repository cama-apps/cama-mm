use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

fn seed_stored_draft(path: &Path) {
    seed_manual_match(path);
    complete_manual_roster(path);
    let players = (0..10)
        .map(|index| {
            serde_json::json!({
                "account_id": 12345 + index,
                "player_slot": if index < 5 { index } else { 128 + index - 5 },
                "hero_id": index + 1,
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "match_id": 9001,
        "duration": 2400,
        "radiant_win": true,
        "radiant_score": 35,
        "dire_score": 22,
        "game_mode": 2,
        "radiant_captain": 12345,
        "dire_captain": 12350,
        "players": players,
    });
    let connection = Connection::open(path).unwrap();
    connection.execute(
        "UPDATE matches SET valve_match_id=9001,enrichment_data=?1,game_mode=2,duration_seconds=2400 WHERE match_id=7",
        [payload.to_string()],
    ).unwrap();
    connection
        .execute(
            "UPDATE match_participants SET hero_id=discord_id-99 WHERE match_id=7",
            [],
        )
        .unwrap();
}

fn competitive_state(path: &Path) -> (String, i64, i64) {
    Connection::open(path).unwrap().query_row(
        "SELECT
            (SELECT json_group_array(json_array(discord_id,wins,losses,glicko_rating,os_mu,os_sigma,jopacoin_balance)) FROM players),
            (SELECT COUNT(*) FROM rating_history),
            (SELECT COUNT(*) FROM matches)",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap()
}

fn response_text(response: &InteractionResponse) -> String {
    let mut parts = vec![response.content.clone()];
    for embed in &response.embeds {
        parts.extend(embed.title.iter().cloned());
        parts.extend(embed.description.iter().cloned());
        parts.extend(
            embed
                .fields
                .iter()
                .map(|field| format!("{}\n{}", field.name, field.value)),
        );
    }
    parts.join("\n")
}

fn selected_drafter() -> InteractionOption {
    option(
        "user",
        InteractionValue::User {
            id: 100,
            display_name: Some("Current drafter name".to_owned()),
            is_bot: Some(false),
        },
    )
}

async fn wait_for_draft_backfill(provider: &EnrichmentRegistrationProvider) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while provider
            .handler
            .draft_backfill_running
            .load(Ordering::Acquire)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background draft backfill completed");
}

struct AcknowledgedPrediction {
    responder: Arc<CapturingResponder>,
    calls: AtomicUsize,
}

impl DraftPredictionPort for AcknowledgedPrediction {
    fn predict(&self, draft: &cama_app::draft_analysis_http::HeroDraft) -> Result<i64, String> {
        assert_eq!(self.responder.captured.lock().unwrap().deferred, [true]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        FixtureDraftPrediction.predict(draft)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_draft_backfill_is_informational_cached_and_visible_everywhere() {
    let (_directory, path) = migrated();
    seed_stored_draft(&path);
    let backfill = Arc::new(CapturingResponder::default());
    let prediction = Arc::new(AcknowledgedPrediction {
        responder: backfill.clone(),
        calls: AtomicUsize::new(0),
    });
    let provider = EnrichmentRegistrationProvider::compose(
        &path,
        &application_config(),
        offline_services(),
        PRODUCTION_DOTABASE_PATH,
        MATCH_VIEW_TIMEOUT,
        prediction.clone(),
    )
    .unwrap();
    let registry = registry(&provider);
    let before = competitive_state(&path);
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD), None),
            backfill.clone(),
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    {
        let captured = backfill.captured.lock().unwrap();
        assert_eq!(captured.deferred, [true]);
        let completion = response_text(captured.original_edits.last().unwrap());
        assert!(completion.contains("With estimates: 1"), "{completion}");
        assert!(
            completion.contains("Errors/partial failures: 0"),
            "{completion}"
        );
    }
    let repo = cama_db::match_draft::MatchDraftRepository::new(&path);
    let mut saved = repo.get(7, GUILD as i64).unwrap().unwrap();
    assert_eq!(saved.radiant_win_probability_bps, Some(6166));
    assert_eq!(saved.radiant_drafter_discord_id, Some(100));
    assert_eq!(saved.dire_drafter_discord_id, Some(105));
    assert_eq!(saved.draft_winner, Some(1));
    saved.prediction_recorded_at = Some(123);
    repo.save(&saved).unwrap();
    let repeated = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD), None),
            repeated,
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    assert_eq!(prediction.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        repo.get(7, GUILD as i64)
            .unwrap()
            .unwrap()
            .prediction_recorded_at,
        Some(123)
    );
    assert_eq!(competitive_state(&path), before);

    for (subcommand, options) in [
        ("drafts", vec![selected_drafter()]),
        ("history", vec![selected_drafter()]),
        (
            "view",
            vec![option("match_id", InteractionValue::Integer(7))],
        ),
    ] {
        let responder = Arc::new(CapturingResponder::default());
        registry
            .command_handler("matches")
            .unwrap()
            .handle(
                command("matches", subcommand, options, 100, Some(GUILD), None),
                responder.clone(),
            )
            .await
            .unwrap();
        let captured = responder.captured.lock().unwrap();
        assert_eq!(captured.deferred, [subcommand != "view"]);
        let text = response_text(captured.followups.last().unwrap());
        for expected in [
            "61.66%",
            "38.34%",
            "Radiant favored",
            "[Batru](https://batru.gg)",
            "<@100>",
            "<@105>",
        ] {
            assert!(
                text.contains(expected),
                "{subcommand} missing {expected}: {text}"
            );
        }
        if subcommand == "drafts" {
            assert!(text.contains("Win rate: **100.0%**"), "{text}");
            assert!(text.contains("Draft W/L/S: **1/0/0**"), "{text}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_backfill_enforces_admin_and_guild_and_stats_stay_in_guild() {
    let (_directory, path) = migrated();
    seed_stored_draft(&path);
    let provider =
        EnrichmentRegistrationProvider::test_new(&path, &application_config(), offline_services())
            .unwrap();
    let registry = registry(&provider);
    for (user, guild, expected) in [(99, Some(GUILD), ADMIN_ONLY), (ADMIN, None, GUILD_ONLY)] {
        let responder = Arc::new(CapturingResponder::default());
        registry
            .command_handler("enrich")
            .unwrap()
            .handle(
                command("enrich", "drafts", Vec::new(), user, guild, None),
                responder.clone(),
            )
            .await
            .unwrap();
        let captured = responder.captured.lock().unwrap();
        assert!(captured.deferred.is_empty());
        assert_eq!(
            captured.immediate,
            [InteractionResponse::message(expected).ephemeral()]
        );
    }
    let repo = cama_db::match_draft::MatchDraftRepository::new(&path);
    assert!(repo.get(7, GUILD as i64).unwrap().is_none());
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD + 1), None),
            Arc::new(CapturingResponder::default()),
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    assert!(repo.get(7, GUILD as i64).unwrap().is_none());
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD), None),
            Arc::new(CapturingResponder::default()),
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    assert!(repo.get(7, GUILD as i64).unwrap().is_some());
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("matches")
        .unwrap()
        .handle(
            command(
                "matches",
                "drafts",
                vec![selected_drafter()],
                100,
                Some(GUILD + 1),
                None,
            ),
            responder.clone(),
        )
        .await
        .unwrap();
    let captured = responder.captured.lock().unwrap();
    let text = response_text(captured.followups.last().unwrap());
    assert!(
        text.contains("No recorded draft estimates found."),
        "{text}"
    );
    assert!(!text.contains("61.66%"), "{text}");
    assert!(text.contains("Draft W/L/S: **0/0/0**"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_history_pages_include_unknown_matches_and_rank_drafters_separately() {
    let (_directory, path) = migrated();
    seed_stored_draft(&path);
    Connection::open(&path).unwrap().execute(
        "INSERT INTO matches(match_id,guild_id,team1_players,team2_players,winning_team,match_date)
         VALUES (8,?1,'[]','[]',2,'2024-01-16'),(9,?1,'[]','[]',1,'2024-01-17')",
        [GUILD as i64],
    ).unwrap();
    let provider =
        EnrichmentRegistrationProvider::test_new(&path, &application_config(), offline_services())
            .unwrap();
    let registry = registry(&provider);
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD), None),
            Arc::new(CapturingResponder::default()),
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("matches")
        .unwrap()
        .handle(
            command(
                "matches",
                "drafts",
                vec![option("limit", InteractionValue::Integer(1))],
                100,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .unwrap();
    let next = {
        let captured = responder.captured.lock().unwrap();
        let response = captured.followups.last().unwrap();
        let text = response_text(response);
        for expected in [
            "Match #9",
            "Page 1 of 3",
            "Game result: **Radiant won**",
            "Draft estimate unavailable",
            "/matches view match_id:9",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        response.components[0].buttons[1].custom_id.clone()
    };
    for (guild, valid) in [(GUILD, true), (GUILD + 1, false)] {
        let responder = Arc::new(CapturingResponder::default());
        registry
            .component_handler(&next)
            .unwrap()
            .handle(
                InteractionRequest::Component {
                    interaction_id: 10,
                    custom_id: next.clone(),
                    user_id: 100,
                    user_display_name: "Current player".to_owned(),
                    guild_id: Some(guild),
                    channel_id: Some(7),
                    values: Vec::new(),
                    member_permissions: None,
                },
                responder.clone(),
            )
            .await
            .unwrap();
        let captured = responder.captured.lock().unwrap();
        if valid {
            assert_eq!(captured.deferred, [true]);
            let text = response_text(captured.updates.last().unwrap());
            assert!(text.contains("Match #8"), "{text}");
            assert!(text.contains("Page 2 of 3"), "{text}");
            assert!(text.contains("Game result: **Dire won**"), "{text}");
        } else {
            assert!(captured.deferred.is_empty());
            assert!(response_text(&captured.immediate[0]).contains("another server"));
        }
    }
    for (view, drafter, rate) in [
        ("best_drafters", "<@100>", "100.0%"),
        ("worst_drafters", "<@105>", "0.0%"),
    ] {
        let responder = Arc::new(CapturingResponder::default());
        registry
            .command_handler("matches")
            .unwrap()
            .handle(
                command(
                    "matches",
                    "drafts",
                    vec![
                        option("view", InteractionValue::String(view.to_owned())),
                        option("limit", InteractionValue::Integer(1)),
                    ],
                    100,
                    Some(GUILD),
                    None,
                ),
                responder.clone(),
            )
            .await
            .unwrap();
        let captured = responder.captured.lock().unwrap();
        let response = captured.followups.last().unwrap();
        let text = response_text(response);
        assert!(text.contains(drafter), "{text}");
        assert!(text.contains(rate), "{text}");
        assert!(text.contains("Decisive drafts: **1**"), "{text}");
        assert!(
            response.components[0].buttons[1]
                .custom_id
                .ends_with(if view == "best_drafters" { ":1" } else { ":2" })
        );
    }
}

struct QuotaThenPrediction(AtomicUsize);

impl DraftPredictionPort for QuotaThenPrediction {
    fn predict(&self, _draft: &cama_app::draft_analysis_http::HeroDraft) -> Result<i64, String> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("quota".to_owned())
        } else {
            Ok(6166)
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        (self.0.load(Ordering::SeqCst) == 1).then_some(Duration::from_millis(1))
    }
}

#[derive(Default)]
struct ExpiringDraftResponder;

#[async_trait]
impl InteractionResponder for ExpiringDraftResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Ok(())
    }
    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        Ok(())
    }
    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Ok(())
    }
    async fn edit_original(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new("interaction token expired"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_background_retries_quota_and_survives_expired_tokens_and_unlinked_accounts() {
    let (_directory, path) = migrated();
    seed_stored_draft(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute("DELETE FROM player_steam_ids", [])
        .unwrap();
    connection
        .execute("UPDATE players SET steam_id=NULL", [])
        .unwrap();
    let predictor = Arc::new(QuotaThenPrediction(AtomicUsize::new(0)));
    let provider = EnrichmentRegistrationProvider::compose(
        &path,
        &application_config(),
        offline_services(),
        PRODUCTION_DOTABASE_PATH,
        MATCH_VIEW_TIMEOUT,
        predictor.clone(),
    )
    .unwrap();
    let registry = registry(&provider);
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command("enrich", "drafts", Vec::new(), ADMIN, Some(GUILD), None),
            Arc::new(ExpiringDraftResponder),
        )
        .await
        .unwrap();
    wait_for_draft_backfill(&provider).await;
    assert_eq!(predictor.0.load(Ordering::SeqCst), 2);
    let saved = cama_db::match_draft::MatchDraftRepository::new(&path)
        .get(7, GUILD as i64)
        .unwrap()
        .unwrap();
    assert_eq!(saved.radiant_win_probability_bps, Some(6166));
    assert_eq!(saved.radiant_drafter_steam_id, Some(12345));
    assert_eq!(saved.radiant_drafter_discord_id, None);
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("enrich")
        .unwrap()
        .handle(
            command(
                "enrich",
                "drafts",
                vec![option("status", InteractionValue::Boolean(true))],
                ADMIN,
                Some(GUILD),
                None,
            ),
            responder.clone(),
        )
        .await
        .unwrap();
    let captured = responder.captured.lock().unwrap();
    let text = response_text(captured.followups.last().unwrap());
    assert!(text.contains("Draft backfill complete"), "{text}");
    assert!(text.contains("Processed: 1 · With estimates: 1"), "{text}");
    assert!(text.contains("Errors/partial failures: 0"), "{text}");
}
