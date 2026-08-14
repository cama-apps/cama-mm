use super::*;
use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use cama_app::trivia_questions::{AbilityData, HeroData, ItemData};
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use crate::registration::{InteractionResponseError, Registry};

const FLOW_GUILD: i64 = 91_001;
const FLOW_CHANNEL: i64 = 91_002;
const FLOW_USER: i64 = 91_003;

struct MigratedFixture {
    _directory: TempDir,
    path: std::path::PathBuf,
}

impl MigratedFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("migrate temporary database");
        let connection = Connection::open(&path).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     jopacoin_balance, lowest_balance_ever
                 ) VALUES (?1, ?2, 'trivia-flow', 3, 3)",
                params![FLOW_USER, FLOW_GUILD],
            )
            .expect("seed registered player");
        let now = unix_timestamp().expect("fixture timestamp");
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id, guild_id, name, species, adopted_at, hatched_at,
                     adopt_fee, last_fed_at, hunger_at_last_fed,
                     evolution_started_at, evolution_due_at
                 ) VALUES (?1, ?2, 'Quizcamel', 'camel', ?3, ?3, 0, ?3, 100, ?3, ?4)",
                params![FLOW_USER, FLOW_GUILD, now - 60, now + 604_800],
            )
            .expect("seed upbringing pet");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
    }

    fn balance(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![FLOW_USER, FLOW_GUILD],
                |row| row.get(0),
            )
            .expect("read trivia player balance")
    }

    fn vanity_tax(&self) -> Arc<PersistentVanityTaxService> {
        let mut container = ServiceContainer::new(&self.path, ServiceContainerOptions::default());
        container.initialize();
        Arc::clone(
            &container
                .components()
                .expect("resolve service container")
                .vanity_tax_service,
        )
    }
}

#[derive(Default)]
struct FlowDiscord {
    edits: Mutex<Vec<(u64, u64, InteractionResponse)>>,
    deletes: Mutex<Vec<(u64, u64)>>,
}

#[async_trait]
impl TriviaDiscordPort for FlowDiscord {
    async fn trivia_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String> {
        Ok(guild_id == FLOW_GUILD && channel_id == FLOW_CHANNEL)
    }

    async fn trivia_user_avatar_url(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, String> {
        assert_eq!(guild_id, FLOW_GUILD);
        Ok((user_id == FLOW_USER).then(|| "https://cdn.invalid/avatar.png".to_owned()))
    }

    async fn edit_trivia_message(
        &self,
        channel_id: u64,
        message_id: u64,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.edits
            .lock()
            .expect("discord edits")
            .push((channel_id, message_id, response));
        Ok(())
    }

    async fn delete_trivia_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.deletes
            .lock()
            .expect("discord deletes")
            .push((channel_id, message_id));
        Ok(())
    }
}

#[derive(Default)]
struct FlowResponses {
    immediate: Mutex<Vec<InteractionResponse>>,
    deferred: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    updates: Mutex<Vec<InteractionResponse>>,
    receipt: Option<InteractionMessageReceipt>,
    update_status_code: Option<u16>,
}

impl FlowResponses {
    fn with_receipt(message_id: u64) -> Self {
        Self {
            receipt: Some(InteractionMessageReceipt {
                message_id,
                channel_id: FLOW_CHANNEL as u64,
                delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
            }),
            ..Self::default()
        }
    }

    fn with_receipt_and_update_status(message_id: u64, status_code: u16) -> Self {
        Self {
            update_status_code: Some(status_code),
            ..Self::with_receipt(message_id)
        }
    }
}

#[async_trait]
impl InteractionResponder for FlowResponses {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.immediate
            .lock()
            .expect("immediate responses")
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.deferred
            .lock()
            .expect("deferred responses")
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups
            .lock()
            .expect("follow-up responses")
            .push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(self.receipt)
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.updates
            .lock()
            .expect("updated responses")
            .push(response);
        self.update_status_code.map_or(Ok(()), |status_code| {
            Err(InteractionResponseError::with_status_code(
                "simulated Discord edit response",
                Some(status_code),
            ))
        })
    }
}

struct UnusedMemberSource;

#[async_trait]
impl crate::gateway_events::GuildMemberPageSource for UnusedMemberSource {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<crate::gateway_events::GatewayMember>, String> {
        Ok(Vec::new())
    }
}

fn sample_question() -> TriviaQuestion {
    TriviaQuestion {
        text: "Who is this hero?".to_owned(),
        options: vec![
            "Anti-Mage".to_owned(),
            "Axe".to_owned(),
            "Bane".to_owned(),
            "Chen".to_owned(),
        ],
        correct_index: 0,
        difficulty: cama_app::trivia_questions::Difficulty::Easy,
        image_url: Some("https://cdn.invalid/antimage.png".to_owned()),
        category: "hero_image",
        explanation: Some("He hates magic.".to_owned()),
    }
}

fn flow_config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|key| match key {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ECONOMY_EVENTS_ENABLED" => Some("false".to_owned()),
        "TRIVIA_ANSWER_TIMEOUT_SECONDS" => Some("300".to_owned()),
        _ => None,
    })
    .expect("trivia flow configuration")
}

fn flow_catalog(cache_root: &Path) -> TriviaCatalog {
    let heroes = (0..8)
        .map(|index| HeroData {
            id: index + 1,
            localized_name: format!("Hero {index}"),
            primary_attribute: Some(
                ["strength", "agility", "intelligence", "universal"]
                    [usize::try_from(index).expect("small hero index") % 4]
                    .to_owned(),
            ),
            is_melee: index < 4,
            image_url: Some(format!(
                "https://cdn.invalid/apps/dota_react/heroes/hero{index}.png"
            )),
            ..HeroData::default()
        })
        .collect::<Vec<_>>();
    let abilities = (0..4)
        .map(|index| AbilityData {
            localized_name: format!("Spell {index}"),
            hero_id: Some(index + 1),
            hero_name: Some(format!("Hero {index}")),
            damage_type: Some(
                ["magical", "physical", "pure", "magical"][index as usize].to_owned(),
            ),
            damage: Some("100".to_owned()),
            icon_url: Some(format!(
                "https://cdn.invalid/apps/dota_react/abilities/spell{index}.png"
            )),
            ..AbilityData::default()
        })
        .collect::<Vec<_>>();
    let mut items = (0..4)
        .map(|index| ItemData {
            localized_name: format!("Shop Item {index}"),
            cost: Some(100 + index * 100),
            icon_url: Some(format!(
                "https://cdn.invalid/apps/dota_react/items/shop{index}.png"
            )),
            ..ItemData::default()
        })
        .collect::<Vec<_>>();
    items.extend((0..4).map(|index| ItemData {
        localized_name: format!("Neutral Item {index}"),
        neutral_tier: Some(index + 1),
        icon_url: Some(format!(
            "https://cdn.invalid/apps/dota_react/items/neutral{index}.png"
        )),
        ..ItemData::default()
    }));
    for relative in heroes
        .iter()
        .filter_map(|hero| hero.image_url.as_deref())
        .chain(
            abilities
                .iter()
                .filter_map(|ability| ability.icon_url.as_deref()),
        )
        .chain(items.iter().filter_map(|item| item.icon_url.as_deref()))
        .filter_map(|url| url.split_once("dota_react/").map(|(_, path)| path))
    {
        let path = cache_root.join(relative);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache path");
        std::fs::write(path, [0x89, b'P', b'N', b'G']).expect("seed cached image");
    }
    TriviaCatalog {
        heroes,
        abilities,
        items,
        voicelines: Vec::new(),
    }
}

fn flow_registry(provider: &TriviaRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register trivia provider");
    builder.build()
}

fn trivia_command() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "trivia".to_owned(),
        user_id: FLOW_USER as u64,
        user_display_name: "Trivia Player".to_owned(),
        guild_id: Some(FLOW_GUILD as u64),
        channel_id: Some(FLOW_CHANNEL as u64),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn trivia_component(interaction_id: u64, question: i64, choice: usize) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id,
        custom_id: format!("trivia_{FLOW_USER}_{question}_{choice}"),
        user_id: FLOW_USER as u64,
        user_display_name: "Trivia Player".to_owned(),
        guild_id: Some(FLOW_GUILD as u64),
        channel_id: Some(FLOW_CHANNEL as u64),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[test]
fn component_ids_preserve_python_shape() {
    assert!(matches!(
        parse_component_id("trivia_42_7_3"),
        Ok(ParsedComponentId {
            user_id: 42,
            question_number: 7,
            choice: 3,
        })
    ));
    assert!(parse_component_id("trivia_42_0_0").is_err());
    assert!(parse_component_id("trivia_42_7_4").is_err());
}

#[test]
fn payout_breakpoints_match_python() {
    assert_eq!(streak_bonus_for_streak(1), 0);
    assert_eq!(streak_bonus_for_streak(3), 1);
    assert_eq!(streak_bonus_for_streak(6), 1);
    assert_eq!(streak_bonus_for_streak(10), 2);
    assert_eq!(streak_bonus_for_streak(14), 1);
}

#[test]
fn pacific_mana_day_rolls_at_four_even_across_dst() {
    let before = chrono::DateTime::parse_from_rfc3339("2026-07-18T10:59:00Z")
        .expect("valid timestamp")
        .timestamp();
    let after = chrono::DateTime::parse_from_rfc3339("2026-07-18T11:00:00Z")
        .expect("valid timestamp")
        .timestamp();
    assert_eq!(pacific_mana_day(before).as_deref(), Ok("2026-07-17"));
    assert_eq!(pacific_mana_day(after).as_deref(), Ok("2026-07-18"));
}

#[test]
fn question_response_preserves_embed_media_and_two_by_two_component_contract() {
    let prepared = PreparedRuntimeQuestion {
        question: sample_question(),
        image: Some(CachedTriviaImage {
            filename: "antimage.png".to_owned(),
            bytes: vec![1, 2, 3],
        }),
    };
    let response = question_response(
        &prepared,
        7,
        6,
        8,
        "Magina",
        Some("https://cdn.invalid/avatar.png"),
        Duration::from_secs(15),
        42,
    );
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "antimage.png");
    assert_eq!(response.components.len(), 2);
    assert_eq!(response.components[0].buttons.len(), 2);
    assert_eq!(response.components[1].buttons.len(), 2);
    assert_eq!(response.components[0].buttons[0].custom_id, "trivia_42_7_0");
    assert_eq!(response.components[1].buttons[1].custom_id, "trivia_42_7_3");
    let embed = &response.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Dota 2 Trivia — Question 7"));
    assert_eq!(embed.author_name.as_deref(), Some("Magina"));
    assert_eq!(
        embed.author_icon_url.as_deref(),
        Some("https://cdn.invalid/avatar.png")
    );
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some("attachment://antimage.png")
    );
    assert_eq!(
        embed.footer.as_deref(),
        Some("Streak: 6 | Difficulty: Easy | JC earned: 8 | 15s to answer")
    );
}

#[test]
fn terminal_responses_clear_components_and_attachments() {
    let snapshot = SessionSnapshot {
        user_id: 42,
        guild_id: 9,
        user_display_name: "Magina".to_owned(),
        user_avatar_url: None,
        streak: 5,
        total_jc: 6,
        recent_categories: vec!["hero_image"],
        current: LiveQuestion {
            question: sample_question(),
            number: 6,
            message: None,
        },
        previous_message: None,
    };
    for response in [
        correct_response(&snapshot, 1),
        game_over_response(&snapshot, false),
        game_over_response(&snapshot, true),
    ] {
        assert!(response.components.is_empty());
        assert!(response.attachments.is_empty());
    }
    let wrong = game_over_response(&snapshot, false);
    assert_eq!(
        wrong.embeds[0].description.as_deref(),
        Some("The correct answer was: **A.** Anti-Mage\n\n*He hates magic.*")
    );
}

#[tokio::test]
async fn ready_observer_retains_one_background_warm_and_returns_immediately() {
    let cache_dir = tempfile::tempdir().expect("cache directory");
    let observer = TriviaImageCacheWarmObserver {
        catalog: Arc::new(TriviaCatalog::default()),
        image_cache: Arc::new(TriviaImageCache::new(cache_dir.path()).expect("cache")),
        task: Mutex::new(None),
    };
    let context = ReadyRecoveryContext::new(vec![9, 10], Arc::new(UnusedMemberSource));
    let report = observer.ready_recovery(context.clone()).await;
    assert_eq!(report.observer, CACHE_OBSERVER_NAME);
    assert_eq!(report.guilds_attempted, 2);
    assert!(report.failures.is_empty());
    assert!(observer.task.lock().expect("task lock").is_some());

    let reconnect = observer.ready_recovery(context).await;
    assert_eq!(reconnect.observer, CACHE_OBSERVER_NAME);
    assert_eq!(reconnect.guilds_attempted, 2);
}

#[tokio::test]
async fn migrated_provider_runs_command_correct_answer_and_game_over_with_sqlite_effects() {
    let fixture = MigratedFixture::new();
    let cache = tempfile::tempdir().expect("trivia cache directory");
    let discord = Arc::new(FlowDiscord::default());
    let provider = TriviaRegistrationProvider::from_catalog(
        &fixture.path,
        flow_catalog(cache.path()),
        TriviaImageCache::new(cache.path()).expect("image cache"),
        &flow_config(),
        fixture.vanity_tax(),
        discord,
        None,
    )
    .expect("construct production trivia provider");
    let registry = flow_registry(&provider);
    assert_eq!(
        registry
            .commands()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["trivia", "trivia-reset-cooldown"]
    );
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "trivia_");

    let command_responses = Arc::new(FlowResponses::with_receipt(500));
    registry
        .command_handler("trivia")
        .expect("trivia command handler")
        .handle(trivia_command(), command_responses.clone())
        .await
        .expect("start trivia command");
    assert_eq!(
        *command_responses.deferred.lock().expect("command deferral"),
        [false]
    );
    {
        let command_followups = command_responses
            .followups
            .lock()
            .expect("command follow-ups");
        assert_eq!(command_followups.len(), 1);
        assert_eq!(command_followups[0].components.len(), 2);
        assert_eq!(command_followups[0].attachments.len(), 1);
    }

    let first_correct = provider
        .handler
        .state
        .sessions_lock()
        .expect("first session")
        .get(&(FLOW_USER, FLOW_GUILD))
        .expect("active first question")
        .current
        .question
        .correct_index;
    // Python treats a deleted current message as settled and continues the
    // session. Exercise the production handler's status-aware 404 branch.
    let correct_responses = Arc::new(FlowResponses::with_receipt_and_update_status(501, 404));
    registry
        .component_handler(&format!("trivia_{FLOW_USER}_1_{first_correct}"))
        .expect("persistent trivia component handler")
        .handle(
            trivia_component(2, 1, first_correct),
            correct_responses.clone(),
        )
        .await
        .expect("settle correct answer");
    assert_eq!(fixture.balance(), 4);
    {
        let correct_updates = correct_responses.updates.lock().expect("correct updates");
        assert_eq!(correct_updates.len(), 1);
        assert_eq!(
            correct_updates[0].embeds[0].title.as_deref(),
            Some("Question 1 — Correct! +1 <:jopacoin:954159801049440297>")
        );
        assert!(correct_updates[0].components.is_empty());
        assert!(correct_updates[0].attachments.is_empty());
    }
    assert_eq!(
        correct_responses
            .followups
            .lock()
            .expect("next question")
            .len(),
        1
    );

    let second_correct = provider
        .handler
        .state
        .sessions_lock()
        .expect("second session")
        .get(&(FLOW_USER, FLOW_GUILD))
        .expect("active second question")
        .current
        .question
        .correct_index;
    let wrong_choice = (second_correct + 1) % 4;
    let wrong_responses = Arc::new(FlowResponses::default());
    registry
        .component_handler(&format!("trivia_{FLOW_USER}_2_{wrong_choice}"))
        .expect("persistent trivia component handler")
        .handle(
            trivia_component(3, 2, wrong_choice),
            wrong_responses.clone(),
        )
        .await
        .expect("settle wrong answer");
    let wrong_updates = wrong_responses.updates.lock().expect("wrong update");
    assert_eq!(wrong_updates.len(), 1);
    assert_eq!(
        wrong_updates[0].embeds[0].title.as_deref(),
        Some("Question 2 — Wrong!")
    );
    assert!(wrong_updates[0].components.is_empty());
    drop(wrong_updates);
    assert!(
        !provider
            .handler
            .state
            .session_is_active(FLOW_USER, FLOW_GUILD)
            .expect("session state")
    );

    let connection = fixture.connection();
    let cooldown: Option<i64> = connection
        .query_row(
            "SELECT last_trivia_session FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("read claimed cooldown");
    assert!(cooldown.is_some());
    let session: (i64, i64) = connection
        .query_row(
            "SELECT streak, jc_earned FROM trivia_sessions
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read completed trivia session");
    assert_eq!(session, (1, 1));
    let ledger: (i64, String, i64, String, String) = connection
        .query_row(
            "SELECT delta, source, actor_id, related_type, related_id
             FROM economy_ledger_entries
             WHERE guild_id = ?1 AND account_id = ?2 AND source = 'trivia'",
            params![FLOW_GUILD, FLOW_USER],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("read trivia reward ledger entry");
    assert_eq!(
        ledger,
        (
            1,
            "trivia".to_owned(),
            FLOW_USER,
            "trivia_answer".to_owned(),
            "1".to_owned(),
        )
    );
    let contexts: i64 = connection
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .expect("read cleared ledger context");
    assert_eq!(contexts, 0);
    let pet_activity: (i64, String, Option<String>) = connection
        .query_row(
            "SELECT score, first_source_key, second_source_key
             FROM pet_evolution_daily
             WHERE guild_id = ?1 AND instinct = 'wisdom'",
            params![FLOW_GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read trivia pet activity");
    assert_eq!(
        pet_activity,
        (
            3,
            "trivia-answer:2".to_owned(),
            Some("trivia-answer:3".to_owned()),
        )
    );
}

#[tokio::test]
async fn classic_trivia_restart_drops_ephemeral_session_but_preserves_economic_state() {
    let fixture = MigratedFixture::new();
    let cache = tempfile::tempdir().expect("trivia cache directory");
    let catalog = flow_catalog(cache.path());
    let vanity_tax = fixture.vanity_tax();

    // Python-equivalent restart contract: the active classic Trivia question,
    // rendered view, streak, and timeout are process-local. SQLite remains
    // authoritative for cooldown claims, settled answer balance/ledger writes,
    // and completed leaderboard sessions.
    let first_correct = {
        let provider = TriviaRegistrationProvider::from_catalog(
            &fixture.path,
            catalog.clone(),
            TriviaImageCache::new(cache.path()).expect("image cache"),
            &flow_config(),
            Arc::clone(&vanity_tax),
            Arc::new(FlowDiscord::default()),
            None,
        )
        .expect("construct pre-restart trivia provider");
        let registry = flow_registry(&provider);
        let command_responses = Arc::new(FlowResponses::with_receipt(700));
        registry
            .command_handler("trivia")
            .expect("trivia command handler")
            .handle(trivia_command(), command_responses)
            .await
            .expect("start pre-restart trivia command");

        let first_correct = provider
            .handler
            .state
            .sessions_lock()
            .expect("pre-restart session")
            .get(&(FLOW_USER, FLOW_GUILD))
            .expect("active pre-restart question")
            .current
            .question
            .correct_index;
        let answer_responses = Arc::new(FlowResponses::with_receipt(701));
        registry
            .component_handler(&format!("trivia_{FLOW_USER}_1_{first_correct}"))
            .expect("pre-restart trivia component handler")
            .handle(trivia_component(702, 1, first_correct), answer_responses)
            .await
            .expect("settle pre-restart correct answer");
        assert_eq!(fixture.balance(), 4);
        assert!(
            provider
                .handler
                .state
                .session_is_active(FLOW_USER, FLOW_GUILD)
                .expect("pre-restart session state")
        );
        first_correct
    };

    let before_restart = fixture.connection();
    let cooldown: Option<i64> = before_restart
        .query_row(
            "SELECT last_trivia_session FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("read pre-restart cooldown");
    assert!(cooldown.is_some(), "the start claim must survive restart");
    let ledger_count: i64 = before_restart
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
             WHERE guild_id = ?1 AND account_id = ?2 AND source = 'trivia'",
            params![FLOW_GUILD, FLOW_USER],
            |row| row.get(0),
        )
        .expect("count pre-restart trivia ledger entries");
    assert_eq!(ledger_count, 1);
    let unfinished_sessions: i64 = before_restart
        .query_row(
            "SELECT COUNT(*) FROM trivia_sessions
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("count unfinished trivia leaderboard rows");
    assert_eq!(unfinished_sessions, 0);

    let provider = TriviaRegistrationProvider::from_catalog(
        &fixture.path,
        catalog,
        TriviaImageCache::new(cache.path()).expect("restarted image cache"),
        &flow_config(),
        vanity_tax,
        Arc::new(FlowDiscord::default()),
        None,
    )
    .expect("construct restarted trivia provider");
    assert!(
        !provider
            .handler
            .state
            .session_is_active(FLOW_USER, FLOW_GUILD)
            .expect("restarted session state")
    );

    let registry = flow_registry(&provider);
    let stale_responses = Arc::new(FlowResponses::default());
    registry
        .component_handler(&format!("trivia_{FLOW_USER}_1_{first_correct}"))
        .expect("restarted trivia component handler")
        .handle(
            trivia_component(703, 1, first_correct),
            stale_responses.clone(),
        )
        .await
        .expect("reject stale pre-restart component");
    let immediate = stale_responses.immediate.lock().expect("stale response");
    assert_eq!(immediate.len(), 1);
    assert_eq!(
        immediate[0].content,
        "This trivia question is no longer active."
    );
    assert!(immediate[0].ephemeral);
    drop(immediate);

    let after_restart = fixture.connection();
    let balance: i64 = after_restart
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("read post-restart balance");
    assert_eq!(balance, 4, "settled answer reward must survive restart");
    let remaining_ledger_count: i64 = after_restart
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
             WHERE guild_id = ?1 AND account_id = ?2 AND source = 'trivia'",
            params![FLOW_GUILD, FLOW_USER],
            |row| row.get(0),
        )
        .expect("count post-restart trivia ledger entries");
    assert_eq!(remaining_ledger_count, ledger_count);
    let remaining_cooldown: Option<i64> = after_restart
        .query_row(
            "SELECT last_trivia_session FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("read post-restart cooldown");
    assert_eq!(remaining_cooldown, cooldown);
    let leaderboard_rows: i64 = after_restart
        .query_row(
            "SELECT COUNT(*) FROM trivia_sessions
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![FLOW_USER, FLOW_GUILD],
            |row| row.get(0),
        )
        .expect("count post-restart leaderboard rows");
    assert_eq!(leaderboard_rows, 0);
}

#[test]
#[ignore = "requires the pinned Dotabase package asset"]
fn pinned_catalog_constructs_the_full_production_provider() {
    let pinned = std::env::var("CAMA_DOTABASE_TEST_PATH")
        .expect("set CAMA_DOTABASE_TEST_PATH to pinned dotabase.db");
    let fixture = MigratedFixture::new();
    let cache = tempfile::tempdir().expect("trivia cache directory");
    let provider = TriviaRegistrationProvider::from_paths(
        &fixture.path,
        pinned,
        cache.path(),
        &flow_config(),
        fixture.vanity_tax(),
        Arc::new(FlowDiscord::default()),
        None,
    )
    .expect("construct provider from pinned catalog");
    let registry = flow_registry(&provider);
    assert!(registry.command_handler("trivia").is_some());
    assert!(registry.component_handler("trivia_1_1_0").is_some());
}
