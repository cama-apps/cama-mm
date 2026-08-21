use super::*;
use cama_app::match_recording::GamesMilestone;
use cama_db::core_repositories::NewPlayer;
use cama_db::economy_event_repository::EconomyEventRepository;
use cama_db::match_runtime::{PendingMatchRepository, PendingMatchState};
use rusqlite::{Connection, params};
use std::io::Cursor;
use tempfile::NamedTempFile;

use crate::registration::InteractionAllowedMentions;
use crate::test_support::initialize_test_database as initialize_or_migrate;

#[derive(Default)]
struct CompositionResponder;

#[derive(Clone, Default)]
struct WagerRefreshProbe {
    calls: Arc<Mutex<Vec<(i64, i64)>>>,
}

#[test]
fn match_wager_refresh_port_is_exposed_for_composition() {
    let constructor: fn(MatchRegistrationProvider) -> Arc<dyn BettingWagerRefreshPort> =
        match_wager_refresh_port;
    let _ = constructor;
}

#[test]
fn match_post_match_debrief_port_is_exposed_for_composition() {
    let constructor: fn(BettingRegistrationProvider) -> Arc<dyn MatchPostMatchDebriefPort> =
        match_post_match_debrief_port;
    let _ = constructor;
}

#[test]
fn invalid_leverage_tier_is_rejected_before_repository_work() {
    assert!(is_valid_leverage_tier(1));
    assert!(is_valid_leverage_tier(10));
    assert!(!is_valid_leverage_tier(4));
    assert!(!is_valid_leverage_tier(0));
}

#[test]
fn bets_team_rows_split_at_discords_field_value_limit() {
    let lines = (1..=15)
        .map(|index| format!("Bettor #{index} • {}", "x".repeat(90)))
        .collect::<Vec<_>>();

    let fields = bounded_bet_team_fields("🔴 Dire", 15, &lines);

    assert!(fields.len() > 1);
    assert_eq!(fields[0].0, "🔴 Dire Bets (15)");
    assert_eq!(fields[1].0, "🔴 Dire Bets (cont.)");
    assert!(
        fields
            .iter()
            .all(|(_, value)| value.chars().count() <= FIELD_VALUE_LIMIT)
    );
    assert_eq!(
        fields
            .iter()
            .flat_map(|(_, value)| value.lines())
            .collect::<Vec<_>>(),
        lines.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn bets_pages_preserve_every_row_within_discord_embed_limits() {
    let lines = (1..=180)
        .map(|index| format!("<@{index}> • {index} {JOPACOIN_EMOTE} • {}", "x".repeat(45)))
        .collect::<Vec<_>>();
    let mut fields = vec![("Current Odds".to_owned(), "Radiant 2x | Dire 2x".to_owned())];
    fields.extend(bounded_bet_team_fields("🔴 Dire", lines.len(), &lines));
    fields.push((
        "Pool Summary".to_owned(),
        "**Total:** 180 effective".to_owned(),
    ));

    let embeds = paginated_bet_embeds("📊 Match #511 — Pool Bets (180 bets)", fields);

    assert!(embeds.len() > 1);
    assert_eq!(embeds[0].fields[0].name, "Current Odds");
    assert_eq!(
        embeds.last().unwrap().fields.last().unwrap().name,
        "Pool Summary"
    );
    assert!(embeds.iter().all(|embed| embed.fields.len() <= MAX_FIELDS));
    assert!(embeds.iter().all(|embed| {
        embed
            .title
            .as_deref()
            .map_or(0, |value| value.chars().count())
            + embed
                .fields
                .iter()
                .map(|field| field.name.chars().count() + field.value.chars().count())
                .sum::<usize>()
            <= TOTAL_LIMIT
    }));
    assert_eq!(
        embeds
            .iter()
            .flat_map(|embed| &embed.fields)
            .filter(|field| field.name.starts_with("🔴 Dire Bets"))
            .flat_map(|field| field.value.lines())
            .collect::<Vec<_>>(),
        lines.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(embeds.iter().all(|embed| {
        embed
            .fields
            .iter()
            .all(|field| !field.value.contains("more"))
    }));
}

#[test]
fn investment_target_validation_rejects_self_short_and_unregistered_targets() {
    assert_eq!(
        validate_investment_target(1, 1, "short", true, true),
        Err("You cannot short yourself.".to_owned())
    );
    assert_eq!(
        validate_investment_target(1, 999, "long", true, false),
        Err("<@999> is not registered.".to_owned())
    );
}

fn invest_subcommand(action: &str, children: Vec<InteractionOption>) -> Vec<InteractionOption> {
    vec![InteractionOption {
        name: "invest".to_owned(),
        value: InteractionValue::Subcommand(
            std::iter::once(InteractionOption {
                name: "action".to_owned(),
                value: InteractionValue::String(action.to_owned()),
            })
            .chain(children)
            .collect(),
        ),
    }]
}

#[tokio::test]
async fn economy_invest_command_sets_lists_and_removes_positions() {
    let database = NamedTempFile::new().expect("investment command database");
    initialize_or_migrate(database.path()).expect("investment command schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(1, "investor", Some(42)))
        .expect("investor");
    players
        .add(&NewPlayer::new(2, "target", Some(42)))
        .expect("target");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("investment command configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    provider
        .handler
        .invest(
            1,
            Some(42),
            &invest_subcommand(
                "set",
                vec![
                    InteractionOption {
                        name: "player".to_owned(),
                        value: InteractionValue::User {
                            id: 2,
                            display_name: Some("target".to_owned()),
                            is_bot: Some(false),
                        },
                    },
                    InteractionOption {
                        name: "direction".to_owned(),
                        value: InteractionValue::String("long".to_owned()),
                    },
                    InteractionOption {
                        name: "percentage".to_owned(),
                        value: InteractionValue::Integer(7),
                    },
                ],
            ),
            &responder,
        )
        .await
        .expect("set investment");
    assert!(
        recording.responses.lock().unwrap()[0]
            .content
            .contains("Configured total: **7% / 50%**")
    );
    recording.responses.lock().unwrap().clear();

    provider
        .handler
        .invest(
            1,
            Some(42),
            &invest_subcommand("list", Vec::new()),
            &responder,
        )
        .await
        .expect("list investment");
    assert!(
        recording.responses.lock().unwrap()[0]
            .content
            .contains("1. LONG <@2> — 7%")
    );
    recording.responses.lock().unwrap().clear();

    provider
        .handler
        .invest(
            1,
            Some(42),
            &invest_subcommand(
                "remove",
                vec![InteractionOption {
                    name: "player".to_owned(),
                    value: InteractionValue::User {
                        id: 2,
                        display_name: None,
                        is_bot: None,
                    },
                }],
            ),
            &responder,
        )
        .await
        .expect("remove investment");
    assert!(
        recording.responses.lock().unwrap()[0]
            .content
            .contains("Removed your investment in <@2>.")
    );
    assert!(
        AutobetInvestmentRepository::new(database.path())
            .list(Some(42), 1)
            .expect("investment list")
            .is_empty()
    );
}

#[tokio::test]
async fn economy_invest_position_removal_survives_departed_target() {
    let database = NamedTempFile::new().expect("departed investment database");
    initialize_or_migrate(database.path()).expect("departed investment schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(1, "investor", Some(42)))
        .expect("departed investor");
    players
        .add(&NewPlayer::new(2, "departed", Some(42)))
        .expect("departed target");
    AutobetInvestmentRepository::new(database.path())
        .set(Some(42), 1, 2, "long", 7)
        .expect("departed target position");
    // A departed Discord member remains a durable player row; only the
    // gateway member snapshot is absent. Removing the row would violate
    // legacy SQLite foreign-key shapes, and is not Python's behavior.
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("departed configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    provider
        .handler
        .invest(
            1,
            Some(42),
            &invest_subcommand(
                "remove",
                vec![InteractionOption {
                    name: "position".to_owned(),
                    value: InteractionValue::Integer(1),
                }],
            ),
            &responder,
        )
        .await
        .expect("remove departed position");
    assert!(
        recording.responses.lock().unwrap()[0]
            .content
            .contains("Removed your investment in <@2>.")
    );
    assert!(
        AutobetInvestmentRepository::new(database.path())
            .list(Some(42), 1)
            .expect("departed position list")
            .is_empty()
    );
}

#[test]
fn economy_invest_rate_limit_is_five_calls_per_ten_seconds() {
    let database = NamedTempFile::new().expect("investment rate database");
    initialize_or_migrate(database.path()).expect("investment rate schema");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("investment rate configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    for _ in 0..5 {
        assert!(
            provider
                .handler
                .take_rate("invest", 42, 1, 5, DEFAULT_RATE_WINDOW)
                .expect("rate check")
        );
    }
    assert!(
        !provider
            .handler
            .take_rate("invest", 42, 1, 5, DEFAULT_RATE_WINDOW)
            .expect("rate rejection")
    );
    assert!(
        provider
            .handler
            .take_rate("invest", 43, 1, 5, DEFAULT_RATE_WINDOW)
            .expect("guild-scoped rate check")
    );
}

#[tokio::test]
async fn economy_invest_rate_limit_returns_ephemeral_retry_copy() {
    let database = NamedTempFile::new().expect("investment retry database");
    initialize_or_migrate(database.path()).expect("investment retry schema");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("investment retry configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    for _ in 0..5 {
        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand("list", Vec::new()),
                &responder,
            )
            .await
            .expect("rate-limited invest response");
    }
    {
        let mut rates = provider.handler.rates.lock().expect("investment rate lock");
        rates.insert(
            ("invest".to_owned(), 42, 1),
            vec![Instant::now() - Duration::from_millis(5_800); 5],
        );
    }
    provider
        .handler
        .invest(
            1,
            Some(42),
            &invest_subcommand("list", Vec::new()),
            &responder,
        )
        .await
        .expect("rate-limited invest response");
    let responses = recording.responses.lock().unwrap();
    let last = responses.last().expect("retry response");
    assert!(last.ephemeral);
    assert_eq!(
        last.content,
        "Slow down! Try configuring investments again in 4s."
    );
}

#[test]
fn economy_invest_retry_after_truncates_four_point_two_to_four_seconds() {
    assert_eq!(
        retry_after_seconds(DEFAULT_RATE_WINDOW, Duration::from_millis(5_800)),
        4
    );
}

#[test]
fn golden_wheel_member_snapshot_removes_departed_wealth_before_rank_selection() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 10_000;
    let mut first = Player::new("first");
    first.discord_id = Some(1);
    first.jopacoin_balance = 900;
    let mut second = Player::new("second");
    second.discord_id = Some(2);
    second.jopacoin_balance = 800;
    let mut third = Player::new("third");
    third.discord_id = Some(3);
    third.jopacoin_balance = 700;
    let raw = vec![departed, first, second, third];
    let visible_ids = BTreeSet::from([1, 2, 3]);
    let visible = filter_visible_wheel_leaderboard(raw, Some(&visible_ids));
    assert_eq!(
        visible
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn synthetic_wheel_members_require_explicit_opt_in_and_negative_ids() {
    assert!(is_enabled_synthetic_wheel_member(-101, true));
    assert!(!is_enabled_synthetic_wheel_member(-101, false));
    assert!(!is_enabled_synthetic_wheel_member(101, true));
}

#[test]
fn golden_wheel_member_snapshot_preserves_raw_order_when_unavailable() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 10_000;
    let mut visible = Player::new("visible");
    visible.discord_id = Some(1);
    visible.jopacoin_balance = 900;
    let raw = vec![departed, visible];
    let result = filter_visible_wheel_leaderboard(raw, None);
    assert_eq!(result[0].discord_id, Some(99));
}

#[test]
fn golden_wheel_dividend_uses_visible_positive_wealth_snapshot() {
    let mut departed = Player::new("departed");
    departed.discord_id = Some(99);
    departed.jopacoin_balance = 100_000;
    let mut first = Player::new("first");
    first.discord_id = Some(1);
    first.jopacoin_balance = 2_500;
    let mut second = Player::new("second");
    second.discord_id = Some(2);
    second.jopacoin_balance = 1_500;
    let raw = vec![departed, first, second];
    let visible_ids = BTreeSet::from([1, 2]);
    assert_eq!(
        visible_wheel_positive_balance(&raw, Some(&visible_ids)),
        Some(4_000)
    );
}

#[test]
fn generic_neon_claim_is_restart_safe_and_releasable() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("migrate Neon database");
    let repository = NeonEventRepository::new(database.path());
    assert!(
        repository
            .claim_one_time_event(0, 42, "generic_neon:7", 2, 100)
            .expect("first claim")
    );
    assert!(
        !repository
            .claim_one_time_event(0, 42, "generic_neon:7", 2, 101)
            .expect("duplicate claim")
    );
    repository
        .release_one_time_event(0, 42, "generic_neon:7")
        .expect("release failed delivery");
    assert!(
        repository
            .claim_one_time_event(0, 42, "generic_neon:7", 2, 102)
            .expect("retry claim")
    );
}

#[test]
fn match_hook_decision_markers_are_terminal_after_decline_and_attempt() {
    let database = NamedTempFile::new().expect("terminal match Neon database");
    initialize_or_migrate(database.path()).expect("migrate terminal Neon database");
    let repository = NeonEventRepository::new(database.path());

    for event_type in [
        "match_settlement:declined",
        "match_easter_eggs:declined",
        "match_debrief:declined",
        "match_settlement:attempted",
        "match_easter_eggs:attempted",
        "match_debrief:attempted",
    ] {
        assert!(
            repository
                .claim_one_time_event(0, 42, event_type, 2, 100)
                .expect("terminal first decision claim")
        );
        assert!(
            !repository
                .claim_one_time_event(0, 42, event_type, 2, 101)
                .expect("terminal replay decision claim")
        );
    }
}

#[test]
fn match_big_win_selection_matches_python_stake_flavor_and_top_choice() {
    let participant = |discord_id, amount, payout, won, refunded| MatchBetSettlementParticipant {
        discord_id,
        amount,
        leverage: 1,
        balance_after: 0,
        payout,
        won,
        refunded,
    };
    let underdog = select_match_big_win(&[
        participant(1, 100, 300, true, false),
        participant(2, 100, 300, true, false),
        participant(3, 250, 0, false, false),
    ])
    .expect("underdog top winner");
    assert_eq!(underdog.discord_id, 1, "Python max keeps the first tie");
    assert_eq!(underdog.flavor, BigWinFlavor::Underdog);

    let top_dog = select_match_big_win(&[
        participant(4, 100, 500, true, false),
        participant(5, 50, 400, false, false),
    ])
    .expect("top-dog winner");
    assert_eq!(top_dog.flavor, BigWinFlavor::TopDog);

    assert!(
        select_match_big_win(&[
            participant(6, 100, 500, true, true),
            participant(7, 500, 400, true, false),
        ])
        .is_none(),
        "a refunded Python top winner blocks fallback"
    );
}

#[test]
fn settlement_degen_candidates_are_only_active_betting_losers() {
    let participants = vec![MatchBetSettlementParticipant {
        discord_id: 11,
        amount: 100,
        leverage: 1,
        balance_after: -1,
        payout: 0,
        won: false,
        refunded: false,
    }];
    let match_loser_ids = [11, 12];
    assert_eq!(active_settlement_loser_ids(&participants), vec![11]);
    assert!(!active_settlement_loser_ids(&participants).contains(&match_loser_ids[1]));
}

#[tokio::test]
async fn match_hook_replays_do_not_reroll_declined_or_failed_attempts() {
    let database = NamedTempFile::new().expect("terminal replay database");
    initialize_or_migrate(database.path()).expect("migrate terminal replay database");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("terminal replay configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let port = MatchBettingPostMatchDebriefPort {
        handler: Arc::clone(&provider.handler),
    };

    let declined_easter = MatchEasterEggRequest {
        guild_id: 42,
        match_id: 100,
        channel_id: Some(7),
        games_milestones: vec![GamesMilestone {
            discord_id: 1,
            games_played: 10,
        }],
        ..MatchEasterEggRequest::default()
    };
    provider
        .handler
        .neon
        .lock()
        .expect("declined Neon lock")
        .queue_rolls([1.0]);
    port.on_match_easter_eggs(declined_easter.clone())
        .await
        .expect("declined Easter-egg attempt");
    let declined_rolls = provider
        .handler
        .neon
        .lock()
        .expect("declined Neon replay lock")
        .rolls
        .observed_chances()
        .len();
    provider
        .handler
        .neon
        .lock()
        .expect("queued declined replay lock")
        .queue_rolls([0.0]);
    port.on_match_easter_eggs(declined_easter)
        .await
        .expect("declined Easter-egg replay");
    assert_eq!(
        provider
            .handler
            .neon
            .lock()
            .expect("declined Neon final lock")
            .rolls
            .observed_chances()
            .len(),
        declined_rolls,
        "a declined Easter-egg decision is terminal and must not reroll"
    );

    let failed_easter = MatchEasterEggRequest {
        guild_id: 42,
        match_id: 101,
        channel_id: Some(7),
        games_milestones: vec![GamesMilestone {
            discord_id: 2,
            games_played: 10,
        }],
        ..MatchEasterEggRequest::default()
    };
    provider
        .handler
        .neon
        .lock()
        .expect("failed Easter-egg Neon lock")
        .queue_rolls([0.0]);
    port.on_match_easter_eggs(failed_easter.clone())
        .await
        .expect("failed Easter-egg delivery attempt");
    let repository = NeonEventRepository::new(database.path());
    assert!(
        repository
            .check_one_time_event(0, 42, "match_easter_eggs:fired:101")
            .expect("failed Easter-egg fired marker")
    );
    let failed_easter_rolls = provider
        .handler
        .neon
        .lock()
        .expect("failed Easter-egg replay lock")
        .rolls
        .observed_chances()
        .len();
    provider
        .handler
        .neon
        .lock()
        .expect("queued failed Easter-egg replay lock")
        .queue_rolls([0.0]);
    port.on_match_easter_eggs(failed_easter)
        .await
        .expect("failed Easter-egg replay");
    assert_eq!(
        provider
            .handler
            .neon
            .lock()
            .expect("failed Easter-egg final lock")
            .rolls
            .observed_chances()
            .len(),
        failed_easter_rolls,
        "an attempted Easter-egg delivery must not duplicate after transport failure"
    );

    let declined_debrief = MatchPostMatchDebriefRequest {
        guild_id: 42,
        match_id: 102,
        channel_id: Some(7),
        winner_id: None,
        loser_id: None,
        payout: 0,
        loss: 0,
        leverage: 1,
        rating_change: None,
        expected_win_prob: None,
    };
    provider
        .handler
        .neon
        .lock()
        .expect("declined debrief Neon lock")
        .queue_rolls([1.0]);
    port.on_post_match_debrief(declined_debrief.clone())
        .await
        .expect("declined debrief attempt");
    let declined_debrief_rolls = provider
        .handler
        .neon
        .lock()
        .expect("declined debrief replay lock")
        .rolls
        .observed_chances()
        .len();
    provider
        .handler
        .neon
        .lock()
        .expect("queued declined debrief replay lock")
        .queue_rolls([0.0]);
    port.on_post_match_debrief(declined_debrief)
        .await
        .expect("declined debrief replay");
    assert_eq!(
        provider
            .handler
            .neon
            .lock()
            .expect("declined debrief final lock")
            .rolls
            .observed_chances()
            .len(),
        declined_debrief_rolls,
        "a declined debrief decision is terminal and must not reroll"
    );

    let failed_debrief = MatchPostMatchDebriefRequest {
        guild_id: 42,
        match_id: 103,
        channel_id: Some(7),
        winner_id: None,
        loser_id: None,
        payout: 800,
        loss: 0,
        leverage: 1,
        rating_change: None,
        expected_win_prob: None,
    };
    provider
        .handler
        .neon
        .lock()
        .expect("failed debrief Neon lock")
        .queue_rolls([0.0]);
    port.on_post_match_debrief(failed_debrief.clone())
        .await
        .expect("failed debrief delivery attempt");
    assert!(
        repository
            .check_one_time_event(0, 42, "match_debrief:103")
            .expect("failed debrief terminal marker")
    );
    let failed_debrief_rolls = provider
        .handler
        .neon
        .lock()
        .expect("failed debrief replay lock")
        .rolls
        .observed_chances()
        .len();
    provider
        .handler
        .neon
        .lock()
        .expect("queued failed debrief replay lock")
        .queue_rolls([0.0]);
    port.on_post_match_debrief(failed_debrief)
        .await
        .expect("failed debrief replay");
    assert_eq!(
        provider
            .handler
            .neon
            .lock()
            .expect("failed debrief final lock")
            .rolls
            .observed_chances()
            .len(),
        failed_debrief_rolls,
        "an attempted debrief delivery must not duplicate after transport failure"
    );
}

#[async_trait]
impl BettingWagerRefreshPort for WagerRefreshProbe {
    async fn refresh_wager_message(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<BettingWagerRefreshReport, String> {
        self.calls
            .lock()
            .expect("wager refresh calls")
            .push((guild_id, pending_match_id));
        Ok(BettingWagerRefreshReport {
            attempted: 3,
            refreshed: 3,
            failures: Vec::new(),
        })
    }
}

#[async_trait]
impl InteractionResponder for CompositionResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }

    async fn defer(
        &self,
        _ephemeral: bool,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }

    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RecordingResponder {
    responses: Arc<Mutex<Vec<InteractionResponse>>>,
    defers: Arc<Mutex<Vec<bool>>>,
    edits: Arc<Mutex<Vec<InteractionResponse>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(
        &self,
        response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events
            .lock()
            .expect("recording responder events lock")
            .push("respond");
        self.responses
            .lock()
            .expect("recording responder responses lock")
            .push(response);
        Ok(())
    }

    async fn defer(
        &self,
        ephemeral: bool,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events
            .lock()
            .expect("recording responder events lock")
            .push("defer");
        self.defers
            .lock()
            .expect("recording responder defers lock")
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events
            .lock()
            .expect("recording responder events lock")
            .push("followup");
        self.responses
            .lock()
            .expect("recording responder responses lock")
            .push(response);
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events
            .lock()
            .expect("recording responder events lock")
            .push("edit_original");
        self.edits
            .lock()
            .expect("recording responder edits lock")
            .push(response);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RejectingGambaMediaResponder {
    events: Arc<Mutex<Vec<&'static str>>>,
    edits: Arc<Mutex<Vec<InteractionResponse>>>,
}

#[async_trait]
impl InteractionResponder for RejectingGambaMediaResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events.lock().unwrap().push("respond");
        Ok(())
    }

    async fn defer(
        &self,
        _ephemeral: bool,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events.lock().unwrap().push("defer");
        Ok(())
    }

    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events.lock().unwrap().push("followup");
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), crate::registration::InteractionResponseError> {
        self.events.lock().unwrap().push("edit_original");
        let rejects_media = !response.attachments.is_empty();
        self.edits.lock().unwrap().push(response);
        if rejects_media {
            Err(crate::registration::InteractionResponseError::new(
                "simulated media rejection",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn gamba_preflight_keeps_registration_and_cooldown_errors_private() {
    let database = NamedTempFile::new().expect("gamba preflight database");
    initialize_or_migrate(database.path()).expect("gamba preflight schema");
    let now = 1_700_000_000;
    assert_eq!(
        gamba_interaction_preflight(database.path(), 42, 7, now, false)
            .expect("unregistered preflight")
            .as_deref(),
        Some("You need to /player register before you can spin the wheel.")
    );

    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "spinner", Some(42)))
        .expect("gamba preflight player");
    assert_eq!(
        gamba_interaction_preflight(database.path(), 42, 7, now, false).expect("ready preflight"),
        None
    );

    players
        .set_last_wheel_spin(7, Some(42), now - 60)
        .expect("gamba preflight cooldown");
    let cooldown = gamba_interaction_preflight(database.path(), 42, 7, now, false)
        .expect("cooldown preflight")
        .expect("cooldown copy");
    assert!(cooldown.contains("You already spun the wheel today!"));
    assert_eq!(
        gamba_interaction_preflight(database.path(), 42, 7, now, true).expect("admin preflight"),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn slow_gamba_work_is_deferred_before_editing_the_original_response() {
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    let route = begin_gamba_response(&responder, false)
        .await
        .expect("public gamba defer");
    assert_eq!(route, GambaResponseRoute::DeferredOriginal);
    assert_eq!(
        recording.events.lock().expect("gamba events").as_slice(),
        &["defer"]
    );

    // Model a render that runs beyond Discord's initial-response window.
    // The interaction is already acknowledged before this work begins.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        deliver_gamba_response(
            &responder,
            InteractionResponse::message("")
                .attachment(InteractionAttachment::bytes("wheel.gif", vec![1, 2, 3],)),
            None,
            route,
        )
        .await
        .expect("deferred wheel delivery")
    );
    assert_eq!(
        recording.events.lock().expect("gamba events").as_slice(),
        &["defer", "edit_original"]
    );
    assert!(recording.responses.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deferred_gamba_media_fallback_never_reuses_initial_response() {
    let recording = RejectingGambaMediaResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    let route = begin_gamba_response(&responder, false)
        .await
        .expect("public gamba defer");
    let delivered_attachment = deliver_gamba_response(
        &responder,
        InteractionResponse::message("")
            .attachment(InteractionAttachment::bytes("wheel.gif", vec![1, 2, 3])),
        Some(InteractionEmbed::titled("Wheel result")),
        route,
    )
    .await
    .expect("text-only gamba fallback");
    assert!(!delivered_attachment);
    assert_eq!(
        recording.events.lock().unwrap().as_slice(),
        &["defer", "edit_original", "edit_original"]
    );
    let edits = recording.edits.lock().unwrap();
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].attachments.len(), 1);
    assert!(edits[1].attachments.is_empty());
    assert_eq!(edits[1].embeds[0].title.as_deref(), Some("Wheel result"));
}

struct EmptyMemberSource;

#[async_trait]
impl crate::gateway_events::GuildMemberPageSource for EmptyMemberSource {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<crate::gateway_events::GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn provider_composes_live_surface_and_dispatches_a_command() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("betting composition configuration");
    let provider = BettingRegistrationProvider::new(
        "/tmp/cama-mm-betting-provider-test.sqlite",
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&provider)
        .expect("register betting provider");
    let registry = builder.build();

    let command_names = registry
        .commands()
        .map(|command| command.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        command_names,
        BTreeSet::from(["bet", "mybets", "bets", "balance", "gamba", "economy"])
    );
    assert_eq!(registry.component_routes().len(), 4);
    assert!(registry.command_handler("bet").is_some());
    assert!(registry.component_handler("betting:wheel:0").is_some());
    assert!(
        registry
            .component_handler("betting:balance:1:next")
            .is_some()
    );
    assert!(registry.component_handler("tt_0").is_some());
    assert!(registry.component_handler("disc_0").is_some());
    assert!(registry.component_handler("disburse:approve").is_some());

    provider
        .handler
        .handle(
            InteractionRequest::Command {
                interaction_id: 1,
                name: "bet".to_owned(),
                user_id: 7,
                user_display_name: "bettor".to_owned(),
                guild_id: None,
                channel_id: None,
                member_permissions: None,
                options: Vec::new(),
            },
            Arc::new(CompositionResponder),
        )
        .await
        .expect("dispatch betting command");
    assert_eq!(
        provider.expire_pending_views().await.expect("view sweep"),
        0
    );
}

#[tokio::test]
async fn balance_is_a_private_paginated_portfolio_with_tax_ledger_exposure() {
    let database = NamedTempFile::new().expect("balance portfolio database");
    initialize_or_migrate(database.path()).expect("balance portfolio schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "portfolio-player", Some(42)))
        .expect("portfolio player");
    for balance in 101..=114 {
        players
            .update_balance(7, Some(42), balance)
            .expect("portfolio ledger movement");
    }
    let connection = Connection::open(database.path()).expect("portfolio seed connection");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy mismatched prediction foreign key for fixture seeding");
    for prediction_id in 1..=5_i64 {
        connection
            .execute(
                "INSERT INTO predictions(
                     prediction_id,guild_id,creator_id,question,status,created_at,closes_at,
                     current_price,initial_fair
                 ) VALUES (?1,42,7,?2,'open',100,9999999999,60,50)",
                params![
                    prediction_id,
                    format!("Will market {prediction_id} resolve YES?")
                ],
            )
            .expect("portfolio prediction");
        connection
            .execute(
                "INSERT INTO prediction_positions(
                     prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                     no_contracts,no_cost_basis_total
                 ) VALUES (?1,7,2,9,1,3)",
                [prediction_id],
            )
            .expect("portfolio position");
    }
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("balance portfolio configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    provider
        .handler
        .handle(
            InteractionRequest::Command {
                interaction_id: 700,
                name: "balance".to_owned(),
                user_id: 7,
                user_display_name: "Portfolio Player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                options: Vec::new(),
            },
            Arc::new(recording.clone()),
        )
        .await
        .expect("portfolio command");

    assert_eq!(recording.defers.lock().unwrap().as_slice(), &[true]);
    {
        let responses = recording.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        let response = &responses[0];
        assert!(response.ephemeral);
        assert_eq!(response.allowed_mentions, InteractionAllowedMentions::None);
        assert_eq!(response.components.len(), 1);
        assert!(response.components[0].buttons[0].disabled);
        assert!(!response.components[0].buttons[2].disabled);
        let overview = &response.embeds[0];
        assert_eq!(
            overview.title.as_deref(),
            Some("💰 Jopacoin Portfolio — Portfolio Player")
        );
        assert!(overview.fields.iter().any(|field| {
            field.name == "Asset Breakdown"
                && field.value.contains("Wallet")
                && field.value.contains("Markets")
        }));
        assert!(overview.fields.iter().any(|field| {
            field.name == "Market Exposure"
                && field.value.contains("5 market(s)")
                && field.value.contains("15 contracts")
        }));
        assert!(overview.fields.iter().any(|field| {
            field.name == "Liabilities & Capital at Risk"
                && field.value.contains("Total tax-ledger exposure")
        }));
        assert!(overview.footer.as_deref().unwrap().contains("Page 1/6"));
    }

    let intruder = RecordingResponder::default();
    provider
        .handler
        .handle(
            balance_component_request("betting:balance:700:next", 8),
            Arc::new(intruder.clone()),
        )
        .await
        .expect("reject portfolio intruder");
    assert_eq!(
        intruder.responses.lock().unwrap()[0].content,
        "This private balance statement belongs to another player."
    );

    for _ in 0..5 {
        provider
            .handler
            .handle(
                balance_component_request("betting:balance:700:next", 7),
                Arc::new(recording.clone()),
            )
            .await
            .expect("portfolio next page");
    }
    let edits = recording.edits.lock().unwrap();
    assert_eq!(edits.len(), 5);
    assert_eq!(edits[0].embeds[0].fields.len(), 4);
    assert_eq!(edits[1].embeds[0].fields.len(), 1);
    assert!(
        edits[0].embeds[0]
            .title
            .as_deref()
            .unwrap()
            .contains("Open Markets")
    );
    assert!(
        edits[2].embeds[0]
            .title
            .as_deref()
            .unwrap()
            .contains("Recent Activity")
    );
    assert_eq!(edits[2].embeds[0].fields.len(), 6);
    assert_eq!(edits[3].embeds[0].fields.len(), 6);
    assert_eq!(edits[4].embeds[0].fields.len(), 3);
    assert!(
        edits[4].embeds[0]
            .footer
            .as_deref()
            .unwrap()
            .contains("Page 6/6")
    );
    assert!(edits[4].components[0].buttons[2].disabled);
}

fn balance_component_request(custom_id: &str, user_id: u64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 701,
        custom_id: custom_id.to_owned(),
        user_id,
        user_display_name: "portfolio player".to_owned(),
        guild_id: Some(42),
        channel_id: Some(99),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[tokio::test]
async fn gateway_ready_recovery_preserves_process_local_betting_views() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("betting composition configuration");
    let provider = BettingRegistrationProvider::new(
        "/tmp/cama-mm-betting-provider-recovery-test.sqlite",
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let observer = provider.gateway_observer();
    assert_eq!(observer.name(), "betting-view-recovery");
    let report = observer
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from(vec![42_u64]),
            Arc::new(EmptyMemberSource),
        ))
        .await;
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 0);
    assert_eq!(report.guilds_superseded, 0);
    assert!(report.failures.is_empty());
}

#[tokio::test]
async fn timeout_worker_is_supervised_and_stops_on_runtime_shutdown() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("betting composition configuration");
    let provider = BettingRegistrationProvider::new(
        "/tmp/cama-mm-betting-provider-timeout-worker.sqlite",
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let _spec = provider.timeout_worker();
    let (_sender, receiver) = tokio::sync::watch::channel(true);
    BettingViewTimeoutWorker {
        handler: Arc::clone(&provider.handler),
    }
    .run(WorkerContext::new(receiver))
    .await
    .expect("worker exits cleanly on shutdown");
    assert_eq!(BETTING_VIEW_TIMEOUT_WORKER_NAME, "betting-view-timeouts");
    assert_eq!(BETTING_VIEW_TIMEOUT_WAKE_INTERVAL, Duration::from_secs(1));
}

#[tokio::test]
async fn expired_wheel_sqlite_failure_keeps_claim_for_retry() {
    let database = NamedTempFile::new().expect("temporary retry database");
    initialize_or_migrate(database.path()).expect("retry schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "retry-player", Some(42)))
        .expect("retry player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("retry configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("retry interaction lock")
        .insert(
            "disc_retry".to_owned(),
            PendingWheelInteraction {
                kind: WheelInteractionKind::Discover,
                user_id: 7,
                guild_id: 42,
                created_at: Instant::now() - Duration::from_secs(120),
                options: vec![WheelOption {
                    label: "LOSE".to_owned(),
                    value: WheelValue::Numeric(0),
                    color: "#000000",
                }],
                wheel_wedges: Vec::new(),
                golden: false,
                display_name: None,
                initial_attachment: None,
                votes: BTreeMap::new(),
                spin_kind: WheelKind::Regular,
                balance_before: 0,
                effects: ManaEffects::default(),
                event_id: "retry-event".to_owned(),
                config: BettingRuntimeConfig::from_application_config(&config),
                event_win_multiplier: 1.0,
                event_loss_multiplier: 1.0,
                bonus_spin: false,
                last_regular_spin: None,
                original_responder: None,
            },
        );
    Connection::open(database.path())
        .expect("open retry database")
        .execute_batch(
            "CREATE TRIGGER fail_first_wheel_log
             BEFORE INSERT ON wheel_spins
             BEGIN SELECT RAISE(ABORT, 'transient wheel failure'); END;",
        )
        .expect("install transient wheel failure");

    let first = provider.expire_pending_views().await;
    assert!(first.is_err(), "first timeout attempt should fail");
    assert!(
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("retained interaction lock")
            .contains_key("disc_retry")
    );
    assert!(
        provider
            .handler
            .wheel_in_flight
            .lock()
            .expect("released claim lock")
            .is_empty()
    );

    Connection::open(database.path())
        .expect("reopen retry database")
        .execute("DROP TRIGGER fail_first_wheel_log", [])
        .expect("remove transient failure");
    assert_eq!(
        provider
            .expire_pending_views()
            .await
            .expect("retry timeout attempt"),
        1
    );
    let spin_count: i64 = Connection::open(database.path())
        .expect("open retry result")
        .query_row(
            "SELECT COUNT(*) FROM wheel_spins WHERE event_id='retry-event'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("retry spin count");
    assert_eq!(spin_count, 1);
}

#[tokio::test]
async fn wheel_click_and_timeout_race_settles_exactly_once() {
    let database = NamedTempFile::new().expect("temporary race database");
    initialize_or_migrate(database.path()).expect("race schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "race-player", Some(42)))
        .expect("race player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("race configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("race interaction lock")
        .insert(
            "disc_race".to_owned(),
            PendingWheelInteraction {
                kind: WheelInteractionKind::Discover,
                user_id: 7,
                guild_id: 42,
                created_at: Instant::now() - Duration::from_secs(120),
                options: vec![WheelOption {
                    label: "LOSE".to_owned(),
                    value: WheelValue::Numeric(0),
                    color: "#000000",
                }],
                wheel_wedges: Vec::new(),
                golden: false,
                display_name: None,
                initial_attachment: None,
                votes: BTreeMap::new(),
                spin_kind: WheelKind::Regular,
                balance_before: 0,
                effects: ManaEffects::default(),
                event_id: "race-event".to_owned(),
                config: BettingRuntimeConfig::from_application_config(&config),
                event_win_multiplier: 1.0,
                event_loss_multiplier: 1.0,
                bonus_spin: false,
                last_regular_spin: None,
                original_responder: None,
            },
        );
    let responder = Arc::new(RecordingResponder::default());
    let component = InteractionRequest::Component {
        interaction_id: 99,
        custom_id: "disc_race:0".to_owned(),
        user_id: 7,
        user_display_name: "race-player".to_owned(),
        guild_id: Some(42),
        channel_id: Some(99),
        member_permissions: None,
        values: Vec::new(),
    };
    let (timeout, click) = tokio::join!(
        provider.expire_pending_views(),
        provider.handler.handle(component, responder),
    );
    assert!(timeout.is_ok() || click.is_ok());
    assert!(
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("race interaction cleanup lock")
            .is_empty()
    );
    assert!(
        provider
            .handler
            .wheel_in_flight
            .lock()
            .expect("race claim cleanup lock")
            .is_empty()
    );
    let spin_count: i64 = Connection::open(database.path())
        .expect("open race result")
        .query_row(
            "SELECT COUNT(*) FROM wheel_spins WHERE event_id='race-event'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("race spin count");
    assert_eq!(spin_count, 1);
}

#[tokio::test]
async fn bet_command_uses_existing_schema_and_red_green_policy() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "bettor", Some(42)))
        .expect("player");
    players.update_balance(7, Some(42), 100).expect("balance");
    let now = unix_seconds().expect("timestamp");
    let pending = PendingMatchRepository::new(database.path())
        .create_pending_match(
            42,
            &PendingMatchState {
                shuffle_timestamp: Some(now.saturating_sub(10)),
                bet_lock_until: Some(now.saturating_add(600)),
                ..PendingMatchState::default()
            },
        )
        .expect("pending match");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("betting composition configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let wager_refresh = WagerRefreshProbe::default();
    provider.set_wager_refresh_port(Arc::new(wager_refresh.clone()));
    let responder = RecordingResponder::default();
    provider
        .handler
        .handle(
            InteractionRequest::Command {
                interaction_id: 2,
                name: "bet".to_owned(),
                user_id: 7,
                user_display_name: "bettor".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                options: vec![
                    InteractionOption {
                        name: "team".to_owned(),
                        value: InteractionValue::String("radiant".to_owned()),
                    },
                    InteractionOption {
                        name: "amount".to_owned(),
                        value: InteractionValue::Integer(10),
                    },
                    InteractionOption {
                        name: "match".to_owned(),
                        value: InteractionValue::Integer(pending.pending_match_id),
                    },
                ],
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("dispatch bet command");
    assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
    let responses = responder.responses.lock().unwrap();
    assert_eq!(responses.len(), 1);
    assert!(responses[0].ephemeral);
    assert!(responses[0].content.contains("Bet placed (Match #"));
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        90
    );
    assert_eq!(
        BettingServiceRepository::new(database.path())
            .get_pending_bets(Some(42), Some(7), 0, Some(pending.pending_match_id))
            .expect("bet lookup")
            .len(),
        1
    );
    assert_eq!(
        wager_refresh
            .calls
            .lock()
            .expect("wager refresh calls")
            .as_slice(),
        &[(42, pending.pending_match_id)]
    );
}

#[tokio::test]
async fn bets_command_returns_more_than_fifteen_bets_without_omission() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let now = unix_seconds().expect("timestamp");
    let pending = PendingMatchRepository::new(database.path())
        .create_pending_match(
            42,
            &PendingMatchState {
                shuffle_timestamp: Some(now.saturating_sub(10)),
                bet_lock_until: Some(now.saturating_add(600)),
                betting_mode: "pool".to_owned(),
                ..PendingMatchState::default()
            },
        )
        .expect("pending match");
    let players = PlayerRepository::new(database.path());
    let betting = BettingServiceRepository::new(database.path());
    for index in 1..=16 {
        let discord_id = 1_000 + index;
        players
            .add(&NewPlayer::new(
                discord_id,
                format!("bettor-{index}"),
                Some(42),
            ))
            .expect("player");
        players
            .update_balance(discord_id, Some(42), 100)
            .expect("balance");
        betting
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(42),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team: BettingTeam::Dire,
                amount: index,
                bet_time: now,
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place bet");
    }
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("betting composition configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let responder = RecordingResponder::default();

    provider
        .handler
        .handle(
            InteractionRequest::Command {
                interaction_id: 3,
                name: "bets".to_owned(),
                user_id: 999,
                user_display_name: "viewer".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "match".to_owned(),
                    value: InteractionValue::Integer(pending.pending_match_id),
                }],
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("dispatch bets command");

    assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
    let responses = responder.responses.lock().unwrap();
    let dire_lines = responses
        .iter()
        .flat_map(|response| &response.embeds)
        .flat_map(|embed| &embed.fields)
        .filter(|field| field.name.starts_with("🔴 Dire Bets"))
        .flat_map(|field| field.value.lines())
        .collect::<Vec<_>>();
    assert_eq!(dire_lines.len(), 16);
    assert!(dire_lines.iter().all(|line| !line.contains("more")));
    assert!(responses.iter().all(|response| response.ephemeral));
}

#[test]
fn command_tree_contains_betting_and_economy_surface() {
    assert_eq!(bet_options(1).len(), 4);
    assert_eq!(economy_options().len(), 7);
    assert_eq!(parse_team("radiant").unwrap(), BettingTeam::Radiant);
    assert_eq!(parse_team("dire").unwrap(), BettingTeam::Dire);
}

#[test]
fn golden_wheel_announcement_matches_public_python_copy_and_mentions_top_players() {
    let mut first = Player::new("first");
    first.discord_id = Some(7);
    first.jopacoin_balance = 900;
    let mut second = Player::new("second");
    second.discord_id = Some(8);
    second.jopacoin_balance = 700;
    let announcement = golden_wheel_announcement(7, &[first, second]);
    assert!(!announcement.ephemeral);
    assert_eq!(
        announcement.allowed_mentions,
        crate::registration::InteractionAllowedMentions::Users(vec![7, 8])
    );
    assert_eq!(
        announcement.embeds[0].title.as_deref(),
        Some("👑 GOLDEN WHEEL INCOMING 👑")
    );
    let description = announcement.embeds[0]
        .description
        .as_deref()
        .unwrap_or_default();
    assert!(description.contains("<@7> is spinning the GOLDEN WHEEL!"));
    assert!(description.contains("**#1** <@7> — 900"));
    assert!(description.contains("**#2** <@8> — 700"));
}

#[test]
fn wheel_animation_message_contract_matches_python_attachment_then_embed_edit() {
    let attachment = InteractionAttachment::bytes("wheel.gif", vec![1, 2, 3]);
    let initial = wheel_animation_result(Some(attachment.clone()));
    assert_eq!(initial.message, "");
    assert_eq!(initial.completion_message.as_deref(), Some(""));
    assert_eq!(initial.attachment, Some(attachment));

    let reveal = wheel_completion_response("", Some(InteractionEmbed::titled("🎉 Winner!")), None)
        .preserve_attachments();
    assert_eq!(reveal.content, "");
    assert_eq!(reveal.embeds.len(), 1);
    assert!(reveal.attachments.is_empty());
    assert_eq!(
        reveal.attachment_policy,
        crate::registration::InteractionAttachmentPolicy::Preserve
    );
}

#[test]
fn heist_result_embed_uses_python_outcome_copy_without_generic_duplication() {
    let note = format!(
        "**HEIST**\n\n💰 You robbed **3** players at the bottom of the ladder!\nTotal stolen: **42** {JOPACOIN_EMOTE}\n\n*Crime pays — when you're already on top.*"
    );
    let embed = wheel_result_embed(
        "HEIST",
        WheelValue::Mechanic(WheelMechanic::Heist),
        WheelKind::Golden,
        123,
        1_800_000_000,
        &note,
        false,
        None,
    );
    assert_eq!(embed.title.as_deref(), Some("🥇 HEIST! 🥇"));
    assert_eq!(embed.description.as_deref(), Some(note.as_str()));
    assert_eq!(embed.fields[0].name, "New Balance");
    assert_eq!(embed.fields[1].name, "Next Spin");
}

#[test]
fn restart_resilience_pending_match_is_restored_from_migrated_sqlite() {
    let database = NamedTempFile::new().expect("restart database");
    initialize_or_migrate(database.path()).expect("migrate restart database");
    let pending = PendingMatchRepository::new(database.path())
        .create_pending_match(
            42,
            &PendingMatchState {
                radiant_team_ids: vec![1, 2, 3, 4, 5],
                dire_team_ids: vec![6, 7, 8, 9, 10],
                shuffle_timestamp: Some(1_700_000_000),
                bet_lock_until: Some(1_700_000_600),
                ..PendingMatchState::default()
            },
        )
        .expect("persist pending match");
    let restarted = PendingMatchRepository::new(database.path())
        .pending_match(42, pending.pending_match_id)
        .expect("reload pending match")
        .expect("pending match survives restart");
    assert_eq!(restarted.state.radiant_team_ids, vec![1, 2, 3, 4, 5]);
    assert_eq!(restarted.state.dire_team_ids, vec![6, 7, 8, 9, 10]);
    assert_eq!(restarted.state.bet_lock_until, Some(1_700_000_600));
}

#[test]
fn restart_resilience_bets_survive_and_settle_after_repository_recreation() {
    let database = NamedTempFile::new().expect("restart betting database");
    initialize_or_migrate(database.path()).expect("migrate restart betting database");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(9_999, "spectator", Some(42)))
        .expect("insert spectator");
    PlayerRepository::new(database.path())
        .update_balance(9_999, Some(42), 50)
        .expect("fund spectator");
    let pending = PendingMatchRepository::new(database.path())
        .create_pending_match(
            42,
            &PendingMatchState {
                radiant_team_ids: vec![1, 2, 3, 4, 5],
                dire_team_ids: vec![6, 7, 8, 9, 10],
                shuffle_timestamp: Some(1_700_000_000),
                bet_lock_until: Some(1_700_000_600),
                betting_mode: "house".to_owned(),
                ..PendingMatchState::default()
            },
        )
        .expect("persist pending match");
    BettingServiceRepository::new(database.path())
        .place_bet_atomic(PlaceBetRequest {
            guild_id: Some(42),
            pending_match_id: pending.pending_match_id,
            discord_id: 9_999,
            team: BettingTeam::Radiant,
            amount: 20,
            bet_time: 1_700_000_010,
            leverage: 1,
            max_debt: 500,
            is_blind: false,
            odds_at_placement: None,
        })
        .expect("place pre-restart bet");
    let restarted = BettingServiceRepository::new(database.path());
    assert_eq!(
        restarted
            .get_pending_bets(
                Some(42),
                Some(9_999),
                1_700_000_000,
                Some(pending.pending_match_id)
            )
            .expect("reload pre-restart bet")
            .len(),
        1
    );
    restarted
        .settle_pending_bets_atomic(
            7_001,
            Some(42),
            1_700_000_000,
            Some(pending.pending_match_id),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle after restart");
    assert_eq!(
        PlayerRepository::new(database.path())
            .get_by_id(9_999, Some(42))
            .expect("reload spectator")
            .expect("spectator")
            .jopacoin_balance,
        70
    );
}

#[test]
fn restart_resilience_full_betting_workflow_settles_both_sides_after_recreation() {
    let database = NamedTempFile::new().expect("full restart database");
    initialize_or_migrate(database.path()).expect("migrate full restart database");
    let players = PlayerRepository::new(database.path());
    for (discord_id, balance) in [(8_001, 100), (8_002, 100)] {
        players
            .add(&NewPlayer::new(discord_id, "spectator", Some(42)))
            .expect("insert spectator");
        players
            .update_balance(discord_id, Some(42), balance)
            .expect("fund spectator");
    }
    let pending = PendingMatchRepository::new(database.path())
        .create_pending_match(
            42,
            &PendingMatchState {
                radiant_team_ids: vec![1, 2, 3, 4, 5],
                dire_team_ids: vec![6, 7, 8, 9, 10],
                shuffle_timestamp: Some(1_700_000_000),
                bet_lock_until: Some(1_700_000_600),
                betting_mode: "house".to_owned(),
                ..PendingMatchState::default()
            },
        )
        .expect("persist full workflow match");
    let place = |discord_id, team, amount| {
        BettingServiceRepository::new(database.path())
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(42),
                pending_match_id: pending.pending_match_id,
                discord_id,
                team,
                amount,
                bet_time: 1_700_000_010,
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place workflow bet");
    };
    place(8_001, BettingTeam::Radiant, 30);
    place(8_002, BettingTeam::Dire, 25);

    let restarted_pending = PendingMatchRepository::new(database.path())
        .pending_match(42, pending.pending_match_id)
        .expect("reload full workflow match")
        .expect("full workflow match survives restart");
    assert_eq!(restarted_pending.pending_match_id, pending.pending_match_id);
    let restarted = BettingServiceRepository::new(database.path());
    restarted
        .settle_pending_bets_atomic(
            7_002,
            Some(42),
            1_700_000_000,
            Some(restarted_pending.pending_match_id),
            BettingTeam::Dire,
            BettingMode::House,
        )
        .expect("settle full workflow after restart");
    assert_eq!(
        players
            .get_by_id(8_001, Some(42))
            .expect("radiant balance")
            .expect("radiant")
            .jopacoin_balance,
        70
    );
    assert_eq!(
        players
            .get_by_id(8_002, Some(42))
            .expect("dire balance")
            .expect("dire")
            .jopacoin_balance,
        125
    );
    assert!(
        restarted
            .get_pending_bets(
                Some(42),
                None,
                1_700_000_000,
                Some(pending.pending_match_id)
            )
            .expect("pending bets after full workflow")
            .is_empty()
    );
}

#[test]
fn provider_exposes_dig_bonus_wheel_seam() {
    let _ = BettingRegistrationProvider::trigger_bonus_spin;
    let _ = BettingRegistrationProvider::with_wager_refresh_port;
    let explosion = resolve_explosion(true, 0, 1.0, 1.0);
    assert_eq!(explosion.credited_reward, 44);
    assert!(explosion.is_bonus);
    let next = cooldown_after_resolution(1_000, true, Some(900), CooldownOutcome::Other);
    assert_eq!(next.next_spin_at, 87_300);
    assert!(!next.schedule_reminder);
}

#[test]
fn wheel_and_explosion_media_are_valid_multi_frame_gifs() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let wheel = render_wheel_attachment(&wedges, 3, false, None).expect("wheel gif");
    let explosion = render_explosion_attachment().expect("explosion gif");
    let expected_wheel_delays = (0..WHEEL_MEDIA_FRAME_COUNT)
        .map(wheel_frame_delay_ms)
        .collect::<Vec<_>>();
    let expected_explosion_delays = (0..EXPLOSION_MEDIA_FRAME_COUNT)
        .map(explosion_frame_delay_ms)
        .collect::<Vec<_>>();
    assert_eq!(wheel.filename, "wheel.gif");
    assert_eq!(explosion.filename, "explosion.gif");
    for (attachment, expected_frames, expected_delays) in [
        (
            wheel,
            WHEEL_MEDIA_FRAME_COUNT,
            expected_wheel_delays.as_slice(),
        ),
        (
            explosion,
            EXPLOSION_MEDIA_FRAME_COUNT,
            expected_explosion_delays.as_slice(),
        ),
    ] {
        assert!(attachment.bytes.starts_with(b"GIF89a"));
        assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
        let mut decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(&attachment.bytes))
            .expect("decode gif header");
        assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
        assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
        let mut frames = 0;
        let mut delays = Vec::with_capacity(expected_frames);
        while decoder
            .read_next_frame()
            .expect("decode gif frame")
            .is_some_and(|frame| {
                delays.push(u64::from(frame.delay) * 10);
                frames += 1;
                true
            })
        {}
        let encoded_delays = expected_delays
            .iter()
            .map(|milliseconds| milliseconds / 10 * 10)
            .collect::<Vec<_>>();
        assert_eq!(delays, encoded_delays);
        assert_eq!(delays.first().copied(), Some(expected_delays[0]));
        assert_eq!(delays.last().copied(), Some(60_000));
        assert_eq!(frames, expected_frames);
    }
    assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
    let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
    assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
}

#[test]
fn wheel_final_pointer_is_centered_on_the_settled_wedge() {
    for wedge_count in [2, 6, 12, 24, 32] {
        let slice = std::f64::consts::TAU / wedge_count as f64;
        for target_index in 0..wedge_count {
            let rotation =
                wheel_frame_rotation(WHEEL_MEDIA_FRAME_COUNT - 1, target_index, wedge_count);
            assert_eq!(
                wheel_index_at_pointer(rotation, wedge_count),
                Some(target_index),
                "pointer must resolve the same wedge as settlement"
            );
            let target_label_angle = (target_index as f64 * slice - rotation + slice / 2.0)
                .rem_euclid(std::f64::consts::TAU);
            assert!(
                target_label_angle.min(std::f64::consts::TAU - target_label_angle) < 1e-9,
                "settled wedge label must be centered under the pointer"
            );
        }
    }
}

#[test]
fn wheel_media_labels_cover_python_themes_and_fractional_phases() {
    let policies = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let cases = [
        (get_wheel_wedges(false, false, policies), false, 0.0_f64),
        (get_wheel_wedges(true, false, policies), false, 47.25_f64),
        (get_wheel_wedges(false, true, policies), true, 271.875_f64),
        (get_wheel_wedges(false, false, policies), false, 13.37_f64),
        (get_wheel_wedges(true, false, policies), false, 222.615_f64),
        (get_wheel_wedges(true, false, policies), false, 358.905_f64),
    ];
    for (wedges, golden, rotation) in cases {
        let colors = wedges
            .iter()
            .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
            .collect::<Vec<_>>();
        let mut palette = vec![
            [30, 30, 35],
            [255, 215, 0],
            [231, 76, 60],
            if golden { [58, 40, 0] } else { [44, 62, 80] },
            [0, 0, 0],
            [255, 255, 255],
            [55, 190, 55],
            [55, 90, 55],
            [255, 255, 0],
            [160, 160, 160],
            [220, 220, 220],
            [150, 127, 0],
            [30, 110, 30],
        ];
        let indices = colors
            .iter()
            .map(|color| {
                palette
                    .iter()
                    .position(|candidate| candidate == color)
                    .unwrap_or_else(|| {
                        palette.push(*color);
                        palette.len() - 1
                    }) as u8
            })
            .collect::<Vec<_>>();
        let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
        let mut sprite_cache = WheelLabelSpriteCache::default();
        let label_atlas = build_wheel_label_atlas(&wedges, &mut sprite_cache);
        draw_wheel_frame(
            &mut pixels,
            &indices,
            &indices,
            &label_atlas,
            0,
            ((rotation / 11.75).round() as usize).min(WHEEL_MEDIA_FRAME_COUNT - 1),
            golden,
            None,
        );
        assert!(pixels.contains(&4));
        assert!(pixels.contains(&5));
        assert!(
            pixels
                .iter()
                .all(|pixel| usize::from(*pixel) < palette.len())
        );
    }
}

fn assert_cached_label_visual_case(
    wedges: &[cama_app::wheel::WheelWedge],
    golden: bool,
    frame_index: usize,
) {
    let colors = wedges
        .iter()
        .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
        .collect::<Vec<_>>();
    let palette = [
        [30, 30, 35],
        [255, 215, 0],
        [231, 76, 60],
        if golden { [58, 40, 0] } else { [44, 62, 80] },
        [0, 0, 0],
        [255, 255, 255],
        [55, 190, 55],
        [55, 90, 55],
        [255, 255, 0],
        [160, 160, 160],
        [220, 220, 220],
        [150, 127, 0],
        [30, 110, 30],
    ];
    let indices = colors
        .iter()
        .map(|color| {
            palette
                .iter()
                .position(|candidate| candidate == color)
                .unwrap_or(0) as u8
        })
        .collect::<Vec<_>>();
    let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
    let mut sprite_cache = WheelLabelSpriteCache::default();
    let atlas = build_wheel_label_atlas(wedges, &mut sprite_cache);
    draw_wheel_frame(
        &mut pixels,
        &indices,
        &indices,
        &atlas,
        0,
        frame_index.min(WHEEL_MEDIA_FRAME_COUNT - 1),
        golden,
        None,
    );
    assert!(pixels.contains(&4));
    assert!(pixels.contains(&5));
    assert!(pixels.iter().all(|pixel| usize::from(*pixel) < 256));
}

#[test]
fn cached_wedge_label_sprites_match_normal_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 0);
}

#[test]
fn cached_wedge_label_sprites_match_bankrupt_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 4);
}

#[test]
fn cached_wedge_label_sprites_match_golden_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, true, policy), true, 23);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_a() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 1);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_b() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 19);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_c() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 30);
}

#[test]
fn wheel_label_layout_is_measured_once_and_reused_across_frames() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let mut cache = WheelLabelSpriteCache::default();
    let first = build_wheel_label_atlas(&wedges, &mut cache);
    let misses_after_first = cache.misses;
    let hits_after_first = cache.hits;
    let second = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(first.wedge_sprite_indices, second.wedge_sprite_indices);
    assert_eq!(cache.misses, misses_after_first);
    assert_eq!(cache.hits - hits_after_first, first.sprites.len());
    assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
}

#[test]
fn wheel_label_sprite_cache_is_custom_safe_and_bounded() {
    let wedges = (0..(MAX_WHEEL_LABEL_SPRITES + 24))
        .map(|index| cama_app::wheel::WheelWedge {
            label: format!("CUSTOM_{index}"),
            value: WheelValue::Numeric(0),
            color: "#123456",
        })
        .collect::<Vec<_>>();
    let mut cache = WheelLabelSpriteCache::default();
    let atlas = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(atlas.wedge_sprite_indices.len(), wedges.len());
    assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
    assert_eq!(cache.misses, wedges.len());
    assert!(atlas.sprites.len() > MAX_WHEEL_LABEL_SPRITES);
}

#[test]
fn completed_wheel_attachment_cache_is_lru_bounded() {
    let wedges = vec![cama_app::wheel::WheelWedge {
        label: "CACHE".to_owned(),
        value: WheelValue::Numeric(5),
        color: "#123456",
    }];
    let keys = (0..=MAX_CACHED_WHEEL_ATTACHMENTS)
        .map(|target_index| WheelAttachmentCacheKey::new(&wedges, target_index, false, None))
        .collect::<Vec<_>>();
    let mut cache = WheelAttachmentCache::default();
    for (index, key) in keys.iter().cloned().enumerate() {
        cache.insert(
            key,
            InteractionAttachment::bytes("wheel.gif", vec![index as u8]),
        );
    }
    assert_eq!(cache.entries.len(), MAX_CACHED_WHEEL_ATTACHMENTS);
    assert!(!cache.entries.contains_key(&keys[0]));
    assert_eq!(
        cache.get(keys.last().expect("latest cache key")),
        Some(InteractionAttachment::bytes(
            "wheel.gif",
            vec![MAX_CACHED_WHEEL_ATTACHMENTS as u8],
        ))
    );
}

#[test]
fn wheel_label_sprite_cache_distinguishes_subpixel_geometry_keys() {
    let wedges = vec![cama_app::wheel::WheelWedge {
        label: "PHASE".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#123456",
    }];
    let mut cache = WheelLabelSpriteCache::default();
    let _ = cache.get_or_insert("PHASE", 1);
    let _ = cache.get_or_insert("PHASE", 2);
    let _ = cache.get_or_insert("PHASE", 1);
    assert_eq!(cache.misses, 2);
    assert_eq!(cache.hits, 1);
    let atlas = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(atlas.sprites.len(), 1);
    let expected_scale = label_scale("PHASE", std::f64::consts::TAU, 220);
    let expected = cache
        .entries
        .get(&("PHASE".to_owned(), expected_scale))
        .expect("atlas geometry is cached under its measured scale");
    assert_eq!(
        atlas.sprites[0].width, expected.width,
        "atlas reuses the font-backed sprite for the measured geometry"
    );
}

#[test]
fn wheel_production_media_is_500px_and_under_discord_limit() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let attachment = render_wheel_attachment(&wedges, 7, false, None).expect("production wheel");
    assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
    let decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(attachment.bytes))
        .expect("wheel header");
    assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
    assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
}

#[test]
fn explosion_production_media_uses_shared_palette_budget_and_upload_bound() {
    let attachment = render_explosion_attachment().expect("production explosion");
    assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
    assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
    let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
    assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
}

#[test]
fn wheel_gif_reuses_palette_seed_without_changing_timing() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let attachment =
        render_wheel_attachment(&get_wheel_wedges(false, false, policy), 0, false, None)
            .expect("seeded wheel GIF");
    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(attachment.bytes))
        .expect("wheel GIF header");
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("wheel frame") {
        delays.push(u64::from(frame.delay) * 10);
    }
    assert_eq!(delays.len(), WHEEL_MEDIA_FRAME_COUNT);
    assert_eq!(delays[..14], [30_u64; 14]);
    assert_eq!(delays.last().copied(), Some(60_000));
}

#[test]
fn explosion_gif_builds_one_shared_palette_without_changing_timing() {
    let attachment = render_explosion_attachment().expect("shared-palette explosion GIF");
    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(attachment.bytes))
        .expect("explosion GIF header");
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("explosion frame") {
        assert!(frame.buffer.iter().all(|index| *index < 6));
        delays.push(u64::from(frame.delay) * 10);
    }
    assert_eq!(delays.len(), EXPLOSION_MEDIA_FRAME_COUNT);
    assert_eq!(delays[0..14], [50_u64; 14]);
    assert_eq!(delays.last().copied(), Some(60_000));
}

#[test]
fn media_delay_contract_matches_python_frame_tables() {
    assert_eq!(
        (0..WHEEL_MEDIA_FRAME_COUNT)
            .map(wheel_frame_delay_ms)
            .collect::<Vec<_>>(),
        [
            [30; 14].as_slice(),
            [45; 14].as_slice(),
            [70; 14].as_slice(),
            [110; 16].as_slice(),
            [180].as_slice(),
            &[195, 210, 225, 240, 255, 270, 285, 300, 315],
            &[60_000, 60_000],
        ]
        .concat()
    );
    assert_eq!(
        (0..EXPLOSION_MEDIA_FRAME_COUNT)
            .map(explosion_frame_delay_ms)
            .collect::<Vec<_>>(),
        [
            [50; 14].as_slice(),
            &[60, 70, 80, 90, 100, 110, 120, 130, 140, 150],
            [60; 4].as_slice(),
            [80; 14].as_slice(),
            [100; 13].as_slice(),
            &[60_000],
        ]
        .concat()
    );
    assert_eq!(gif_delay_centiseconds(60_000), 6_000);
}

#[test]
fn neon_one_time_events_survive_provider_reconstruction() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let port = BettingSqliteNeonEventPort {
        repository: NeonEventRepository::new(database.path()),
    };
    let key = EventKey::new(7, Some(42), "degen_90");
    assert!(
        !port
            .check_one_time_event(&key)
            .expect("initial Neon lookup")
    );
    port.persist_one_time_event(key.clone(), 3)
        .expect("persist Neon event");
    let rebuilt = BettingSqliteNeonEventPort {
        repository: NeonEventRepository::new(database.path()),
    };
    assert!(
        rebuilt
            .check_one_time_event(&key)
            .expect("rebuilt Neon lookup")
    );
    assert_eq!(
        rebuilt.load_one_time_events().expect("load Neon events"),
        vec![key]
    );
}

#[test]
fn nested_option_parser_reads_discord_subcommand_payload() {
    let options = vec![InteractionOption {
        name: "tip".to_owned(),
        value: InteractionValue::Subcommand(vec![InteractionOption {
            name: "amount".to_owned(),
            value: InteractionValue::Integer(9),
        }]),
    }];
    assert_eq!(
        parse_nested_string(&options, "amount"),
        Some("9".to_owned())
    );
}

#[test]
fn event_multiplier_changes_numeric_wedges_but_not_special_values() {
    let mut positive = cama_app::wheel::WheelWedge {
        label: "10".to_owned(),
        value: WheelValue::Numeric(10),
        color: "#fff",
    };
    apply_event_multiplier_to_wedge(&mut positive, 1.5, 0.5);
    assert_eq!(positive.value, WheelValue::Numeric(15));
    assert_eq!(positive.label, "15");

    let mut zero = cama_app::wheel::WheelWedge {
        label: "LOSE".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#000",
    };
    apply_event_multiplier_to_wedge(&mut zero, 10.0, 0.0);
    assert_eq!(zero.value, WheelValue::Numeric(0));
    assert_eq!(zero.label, "LOSE");

    let mut mechanic = cama_app::wheel::WheelWedge {
        label: "DISCOVER".to_owned(),
        value: WheelValue::Mechanic(WheelMechanic::Discover),
        color: "#000",
    };
    apply_event_multiplier_to_wedge(&mut mechanic, 2.0, 2.0);
    assert_eq!(
        mechanic.value,
        WheelValue::Mechanic(WheelMechanic::Discover)
    );
    assert_eq!(mechanic.label, "DISCOVER");
}

#[test]
fn gamba_special_minted_rewards_apply_event_after_central_scaling() {
    assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
    assert_eq!(scale_minted_gamba_reward(100, 0.5, 1.0, false), 50);
}

#[test]
fn gamba_chain_reaction_scales_only_the_newly_minted_copy() {
    assert_eq!(scale_minted_gamba_reward(30, 0.5, 1.0, false), 15);
}

#[test]
fn gamba_dynamic_minted_rewards_apply_event_and_report_adjusted_amount() {
    assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
    assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
}

#[test]
fn gamba_heist_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
}

#[test]
fn gamba_market_crash_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(25, 0.5, 1.0, false), 12);
}

#[test]
fn gamba_hostile_takeover_fallback_is_event_scaled() {
    assert_eq!(scale_minted_gamba_reward(40, 0.5, 1.0, false), 20);
}

#[test]
fn gamba_numeric_and_player_transfer_rewards_are_not_double_scaled() {
    assert_eq!(apply_gamba_event_multiplier(20, 1.0, 1.0), 20);
    assert_eq!(apply_gamba_event_multiplier(12, 1.0, 1.0), 12);
}

#[test]
fn extend_wedge_only_changes_an_existing_bankruptcy_penalty() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "player", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let config = BettingRuntimeConfig::from_application_config(&config);
    let wedge = cama_app::wheel::WheelWedge {
        label: "+1".to_owned(),
        value: WheelValue::Mechanic(WheelMechanic::ExtendOne),
        color: "#8b0000",
    };
    let effects = ManaEffects::default();
    let taxable = BTreeSet::new();
    resolve_wheel_value(
        database.path(),
        42,
        7,
        0,
        &wedge,
        WheelKind::Bankrupt,
        &effects,
        &config,
        "extend-no-penalty",
        &taxable,
        &BTreeSet::new(),
        1.0,
        1.0,
        false,
        None,
        None,
    )
    .expect("resolve no-op extension");
    assert_eq!(
        BankruptcyRepository::new(database.path())
            .get_penalty_games(7, Some(42))
            .expect("read absent penalty"),
        0
    );

    BankruptcyRepository::new(database.path())
        .adjust_penalty_games_atomic(7, Some(42), 2)
        .expect("seed active penalty");
    resolve_wheel_value(
        database.path(),
        42,
        7,
        0,
        &wedge,
        WheelKind::Bankrupt,
        &effects,
        &config,
        "extend-active-penalty",
        &taxable,
        &BTreeSet::new(),
        1.0,
        1.0,
        false,
        None,
        None,
    )
    .expect("resolve active extension");
    assert_eq!(
        BankruptcyRepository::new(database.path())
            .get_penalty_games(7, Some(42))
            .expect("read extended penalty"),
        3
    );
}

#[test]
fn bankrupt_wheel_losses_apply_self_protection_and_log_only_the_applied_debit() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    let wedge = cama_app::wheel::WheelWedge {
        label: "BANKRUPT".to_owned(),
        value: WheelValue::Numeric(-20),
        color: "#1a1a1a",
    };

    for (capacity, expected_applied) in [(5, 15), (20, 0), (0, 20)] {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "spinner", Some(42)))
            .expect("spinner");
        players.update_balance(7, Some(42), 100).expect("balance");
        Connection::open(database.path())
            .expect("open database")
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data
                 ) VALUES(?1,?2,'aegis',0,2000000000,0,?3)",
                params![
                    7,
                    42,
                    format!(
                        r#"{{"capacity":{capacity},"capacity_remaining":{capacity},"rate":1.0}}"#
                    ),
                ],
            )
            .expect("seed Aegis");

        let event_id = format!("shielded-bankrupt-{capacity}");
        let resolution = resolve_wheel_value(
            database.path(),
            42,
            7,
            100,
            &wedge,
            WheelKind::Bankrupt,
            &ManaEffects::default(),
            &runtime,
            &event_id,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1.0,
            1.0,
            false,
            None,
            None,
        )
        .expect("resolve bankrupt loss");

        assert_eq!(resolution.resolved, WheelValue::Numeric(-expected_applied));
        assert_eq!(resolution.logged, -expected_applied);
        assert_eq!(resolution.cooldown_outcome, CooldownOutcome::Other);
        assert_eq!(
            resolution.extra_note.contains("White Mana Shields"),
            capacity > 0
        );
        assert_eq!(
            players
                .get_by_id(7, Some(42))
                .expect("spinner lookup")
                .expect("spinner")
                .jopacoin_balance,
            100 - expected_applied
        );
        assert_eq!(
            LoanRepository::new(database.path())
                .get_nonprofit_fund(Some(42))
                .expect("reserve"),
            expected_applied
        );
        assert_eq!(
            EconomyEventRepository::new(database.path())
                .get_surface_daily_volumes(Some(42), 1, 1_700_000_000)
                .expect("surface volumes")
                .gamba_debits,
            expected_applied as f64
        );

        let retry = resolve_wheel_value(
            database.path(),
            42,
            7,
            100,
            &wedge,
            WheelKind::Bankrupt,
            &ManaEffects::default(),
            &runtime,
            &event_id,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1.0,
            1.0,
            false,
            None,
            None,
        )
        .expect("retry bankrupt loss");
        assert_eq!(retry.logged, -expected_applied);
        assert_eq!(
            players
                .get_by_id(7, Some(42))
                .expect("spinner lookup")
                .expect("spinner")
                .jopacoin_balance,
            100 - expected_applied
        );
        assert_eq!(
            LoanRepository::new(database.path())
                .get_nonprofit_fund(Some(42))
                .expect("reserve after retry"),
            expected_applied
        );
        assert_eq!(
            Connection::open(database.path())
                .expect("open database")
                .query_row(
                    "SELECT COUNT(*) FROM hostile_loss_events WHERE event_key=?1",
                    [&event_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("hostile event count"),
            1
        );
    }
}

#[test]
fn fully_shielded_numeric_wheel_losses_keep_their_landed_presentation_and_history_identity() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    for (kind, label, expected_title) in [
        (WheelKind::Bankrupt, "BANKRUPT", "BANKRUPT"),
        (WheelKind::Golden, "OVEREXTENDED", "OVEREXTENDED"),
    ] {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "spinner", Some(42)))
            .expect("spinner");
        players.update_balance(7, Some(42), 100).expect("balance");
        Connection::open(database.path())
            .expect("open database")
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data
                 ) VALUES(7,42,'aegis',0,2000000000,0,
                     '{\"capacity\":20,\"capacity_remaining\":20,\"rate\":1.0}')",
                [],
            )
            .expect("seed Aegis");
        let event_id = format!("shielded-{label}-completion");
        let settled = resolve_pending_wheel(
            database.path(),
            42,
            7,
            100,
            kind,
            &ManaEffects::default(),
            &runtime,
            &event_id,
            &WheelOption {
                label: label.to_owned(),
                value: WheelValue::Numeric(-20),
                color: "#1a1a1a",
            },
            1_700_000_000,
            false,
            false,
            None,
            1.0,
            1.0,
            &BTreeSet::new(),
            &BTreeSet::new(),
            None,
        )
        .expect("resolve pending numeric loss");
        assert!(
            settled
                .completion_embed
                .title
                .as_deref()
                .is_some_and(|title| title.contains(expected_title))
        );
        assert!(
            settled
                .completion_embed
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.contains("**0**") && !description.contains("LOSE A TURN")
                })
        );
        let connection = Connection::open(database.path()).expect("open database");
        let (logged, outcome_code): (i64, String) = connection
            .query_row(
                "SELECT result,outcome_code FROM wheel_spins WHERE event_id=?1",
                [&event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("wheel history");
        assert_eq!(logged, 0);
        assert_eq!(outcome_code, label);
    }
}

#[test]
fn wheel_interaction_timeout_policy_matches_python_views() {
    assert_eq!(
        wheel_interaction_ttl(WheelInteractionKind::TownTrial),
        Duration::from_secs(300)
    );
    assert_eq!(
        wheel_interaction_ttl(WheelInteractionKind::Discover),
        Duration::from_secs(60)
    );
    assert_eq!(
        wheel_interaction_ttl(WheelInteractionKind::Scrying),
        Duration::from_secs(30)
    );
    assert_eq!(
        wheel_interaction_ttl(WheelInteractionKind::Reroll),
        Duration::from_secs(30)
    );
    let options = vec![
        WheelOption {
            label: "GOOD".to_owned(),
            value: WheelValue::Numeric(100),
            color: "#fff000",
        },
        WheelOption {
            label: "BAD".to_owned(),
            value: WheelValue::Numeric(-100),
            color: "#000000",
        },
    ];
    let base = || PendingWheelInteraction {
        kind: WheelInteractionKind::Discover,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: options.clone(),
        wheel_wedges: Vec::new(),
        golden: false,
        display_name: None,
        initial_attachment: None,
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Bankrupt,
        balance_before: -100,
        effects: ManaEffects::default(),
        event_id: "timeout".to_owned(),
        config: BettingRuntimeConfig {
            min_bet: 1,
            max_debt: 500,
            bankruptcy_cooldown_seconds: 0,
            bankruptcy_fresh_start_balance: 0,
            bankruptcy_penalty_games: 0,
            bankruptcy_penalty_rate: 0.0,
            garnishment_rate: 0.0,
            vanity_tax_rate: 0.0,
            low_priority_tax_rate: 0.0,
            wheel_target_ev: 0.0,
            wheel_bankrupt_target_ev: 0.0,
            wheel_golden_target_ev: 0.0,
            loan_cooldown_seconds: 0,
            loan_fee_rate: 0.0,
            loan_max_amount: 0,
            tip_fee_rate: 0.0,
            minigame_jc_delta_scale: 1.0,
            prediction_contract_value: 10,
            prediction_initial_fair_default: 50,
            synthetic_members_enabled: false,
            disburse_min_fund: 0,
            disburse_quorum_percentage: 0.0,
            lottery_activity_days: 14,
            economy_events_enabled: false,
            economy_normal_annual_rate: 0.0,
            economy_inflation_ceiling: 0.0,
            economy_event_lookback_days: 0,
            economy_event_max_reserve_burn_pct: 0.0,
            economy_event_max_wallet_burn_pct: 0.0,
            economy_event_trigger_hour_local: 0,
            neon_degen_enabled: false,
            neon_cooldown_seconds: 0,
            neon_bigwin_floor: 0.05,
            neon_bigwin_full_payout: 5_000,
            neon_bigwin_min_payout: 500,
            admin_user_ids: BTreeSet::new(),
            tax_man_user_ids: BTreeSet::new(),
        },
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: None,
    };
    assert_eq!(timeout_wheel_option(&base()).label, "BAD");
    let mut trial = base();
    trial.kind = WheelInteractionKind::TownTrial;
    trial.votes.insert(1, 0);
    trial.votes.insert(2, 1);
    trial.votes.insert(3, 1);
    assert_eq!(timeout_wheel_option(&trial).label, "BAD");
    let mut reroll = base();
    reroll.kind = WheelInteractionKind::Reroll;
    assert_eq!(timeout_wheel_option(&reroll).label, "GOOD");
}

#[test]
fn wheel_choice_and_blue_preview_embeds_match_python_presentation() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let options = vec![
        WheelOption {
            label: "25".to_owned(),
            value: WheelValue::Numeric(25),
            color: "#ffffff",
        },
        WheelOption {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#000000",
        },
    ];
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Scrying,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options,
        wheel_wedges: Vec::new(),
        golden: false,
        display_name: None,
        initial_attachment: None,
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Regular,
        balance_before: 100,
        effects: ManaEffects::default(),
        event_id: "embed".to_owned(),
        config: BettingRuntimeConfig::from_application_config(&config),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: None,
    };

    let scrying = wheel_interaction_prompt_embed(&pending);
    assert_eq!(scrying.title.as_deref(), Some("🏝️ MANA SCRYING"));
    assert_eq!(scrying.color, Some(0x34_98_DB));
    assert_eq!(
        scrying.description.as_deref(),
        Some(
            "🔮 <@7>, the Island reveals two fates:\n\n**A:** +25 JC\n**B:** LOSE (0 JC)\n\nChoose wisely. *(Blue mana: winnings reduced by 25%)*"
        )
    );
    let rows = wheel_interaction_rows("betting:scry:embed", &pending);
    assert_eq!(rows[0].buttons[0].label, "A: +25 JC");
    assert_eq!(rows[0].buttons[1].label, "B: LOSE (0 JC)");

    let preview = wheel_bankrupt_preview_embed("Blue mana reveals: CHAIN, BANANA, +1");
    assert_eq!(preview.title.as_deref(), Some("Wheel preview"));
    assert_eq!(preview.color, Some(0x34_98_DB));
    assert_eq!(
        preview.description.as_deref(),
        Some("Blue mana reveals: CHAIN, BANANA, +1")
    );

    for (kind, title, color) in [
        (WheelInteractionKind::TownTrial, "⚖️ TOWN TRIAL", 0x2A_1A_1A),
        (WheelInteractionKind::Discover, "🃏 DISCOVER", 0x1A_2A_2A),
        (
            WheelInteractionKind::Reroll,
            "Re-roll available",
            0xED_42_45,
        ),
    ] {
        let mut candidate = pending.clone();
        candidate.kind = kind;
        let embed = wheel_interaction_prompt_embed(&candidate);
        assert_eq!(embed.title.as_deref(), Some(title));
        assert_eq!(embed.color, Some(color));
    }

    let resolution = PendingWheelResolution {
        message: "Result settled.".to_owned(),
        completion_embed: InteractionEmbed::titled("Wheel Result"),
        neon_wheel_result: None,
        neon_wheel_balance_before: None,
        neon_wheel_balance: None,
        neon_lightning: None,
    };
    let option = pending.options[0].clone();
    let mut trial = pending.clone();
    trial.kind = WheelInteractionKind::TownTrial;
    let trial_timeout = wheel_timeout_response(&trial, &option, &resolution, None);
    assert_eq!(
        trial_timeout.embeds[0].title.as_deref(),
        Some("⚖️ THE TOWN HAS SPOKEN")
    );
    assert_eq!(
        trial_timeout.embeds[0].description.as_deref(),
        Some("The town decided: **25** for <@7>!")
    );

    let mut discover = pending;
    discover.kind = WheelInteractionKind::Discover;
    let discover_timeout = wheel_timeout_response(&discover, &option, &resolution, None);
    assert_eq!(
        discover_timeout.embeds[0].title.as_deref(),
        Some("🃏 DISCOVER — TIMEOUT")
    );
    assert_eq!(
        discover_timeout.embeds[0].description.as_deref(),
        Some("<@7> didn't choose in time. The worst fate applies: **25**!")
    );
}

#[test]
fn disbursement_vote_audit_matches_python_pagination_contract() {
    let proposal = DisbursementProposal {
        guild_id: 42,
        proposal_id: 900,
        message_id: None,
        channel_id: None,
        fund_amount: 250,
        quorum_required: 20,
        status: "active".to_owned(),
        created_at: None,
        votes: {
            let mut votes = DISBURSE_METHODS
                .iter()
                .map(|method| ((*method).to_owned(), 0))
                .collect::<BTreeMap<_, _>>();
            votes.insert("even".to_owned(), 16);
            votes
        },
    };
    let votes = (0..16)
        .map(|index| DisbursementVote {
            discord_id: 1_000 + index,
            vote_method: "even".to_owned(),
            voted_at: 10_000 + index,
        })
        .collect::<Vec<_>>();
    let state = PendingDisburseVotes {
        guild_id: 42,
        requester_id: 7,
        proposal,
        votes,
        page: 0,
        created_at: Instant::now(),
    };
    let response = disbursement_votes_response(&state);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("🔍 Disbursement Vote Details (Tax Man)")
    );
    assert!(response.embeds[0].fields[2].name.contains("1-15 of 16"));
    assert_eq!(response.components.len(), 1);
    assert!(response.components[0].buttons[0].disabled);
    assert!(!response.components[0].buttons[1].disabled);
    assert_eq!(
        parse_disbursement_page_component("disburse:votes:42:7:900:next"),
        Some(("42:7:900".to_owned(), DisburseVotesPageAction::Next))
    );
}

#[test]
fn disbursement_petition_embed_explains_every_option_and_progress() {
    let mut votes = DISBURSE_METHODS
        .iter()
        .map(|method| ((*method).to_owned(), 0))
        .collect::<BTreeMap<_, _>>();
    votes.insert("even".to_owned(), 2);
    votes.insert("burn".to_owned(), 1);
    let proposal = DisbursementProposal {
        guild_id: 42,
        proposal_id: 900,
        message_id: None,
        channel_id: None,
        fund_amount: 250,
        quorum_required: 10,
        status: "active".to_owned(),
        created_at: None,
        votes,
    };

    let response = disbursement_proposal_response(&proposal);
    let embed = &response.embeds[0];
    assert_eq!(
        embed.title.as_deref(),
        Some("🏛️ Jopacoin Reserve Allocation Vote")
    );
    assert!(
        embed
            .description
            .as_deref()
            .is_some_and(|description| description.contains("**250**"))
    );
    let expected_options = [
        ("Even Split", "Repays every debtor evenly", "**2 votes**"),
        ("Proportional", "in proportion", "**0 votes**"),
        ("Neediest First", "deepest in debt", "**0 votes**"),
        ("Stimulus", "outside the three richest", "**0 votes**"),
        ("Lottery", "randomly selected", "**0 votes**"),
        ("Social Security", "games played", "**0 votes**"),
        ("Richest", "highest balance", "**0 votes**"),
        ("Burn", "from circulation", "**1 vote**"),
        ("Next Match Pot", "next match's betting pot", "**0 votes**"),
        ("Cancel", "returns all locked funds", "**0 votes**"),
    ];
    for (label, explanation, count) in expected_options {
        let field = embed
            .fields
            .iter()
            .find(|field| field.name.contains(label))
            .unwrap_or_else(|| panic!("missing petition field for {label}"));
        assert!(field.inline, "petition option {label} must stay compact");
        assert!(field.value.contains(explanation));
        assert!(field.value.contains(count));
    }
    let progress = embed
        .fields
        .iter()
        .find(|field| field.name == "🗳️ Petition Progress")
        .expect("petition progress field");
    assert!(progress.value.contains("**3/10**"));
    assert!(progress.value.contains("**30%**"));
    assert!(progress.value.contains("7 more votes needed"));
    assert_eq!(embed.fields.len(), 11);
    assert_eq!(response.components.len(), 2);
    assert!(response.components.iter().all(|row| row.buttons.len() == 5));
    assert!(
        embed
            .footer
            .as_deref()
            .is_some_and(|footer| footer.contains("latest ballot replaces"))
    );

    let closed = disbursement_proposal_response_with_disabled_buttons(&proposal);
    assert!(
        closed
            .components
            .iter()
            .flat_map(|row| &row.buttons)
            .all(|button| button.disabled)
    );
    assert_eq!(
        closed.embeds[0].footer.as_deref(),
        Some("Voting closed • Ties favor Even Split")
    );
}

#[tokio::test]
async fn wrong_channel_penalty_debits_one_and_credits_reserve() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "bettor", Some(42)))
        .expect("player");
    players.update_balance(7, Some(42), 2).expect("balance");
    charge_wrong_channel(database.path(), 7, 42)
        .await
        .expect("penance");
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        1
    );
    assert_eq!(
        LoanRepository::new(database.path())
            .get_nonprofit_fund(Some(42))
            .expect("reserve"),
        1
    );
}

#[test]
fn wheel_income_applies_vanity_tax_snapshot_and_stable_exact_once_key() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "taxable", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    let taxable_ids = BTreeSet::from([7_i64]);
    let first = credit_wheel_income(
        database.path(),
        42,
        7,
        0,
        100,
        &runtime,
        "123:7:abc",
        "test wheel reward",
        &taxable_ids,
        &BTreeSet::new(),
    )
    .expect("credit taxable wheel reward");
    assert_eq!(first.gross, 100);
    assert_eq!(first.vanity_tax, 10);
    assert_eq!(first.net, 90);
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        93
    );
    assert_eq!(event_related_id("123:7:abc"), event_related_id("123:7:abc"));
    assert_ne!(event_related_id("123:7:abc"), event_related_id("124:7:abc"));
}

#[test]
fn wheel_income_stacks_low_priority_tax_beside_the_vanity_tax() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "doubly-taxed", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    let taxable_ids = BTreeSet::from([7_i64]);
    let receipt = credit_wheel_income(
        database.path(),
        42,
        7,
        0,
        100,
        &runtime,
        "321:7:abc",
        "test wheel reward",
        &taxable_ids,
        &taxable_ids,
    )
    .expect("credit doubly taxed wheel reward");
    // Both rates read the 100 JC gross, never the post-vanity remainder.
    assert_eq!(receipt.vanity_tax, 10);
    assert_eq!(receipt.low_priority_tax, 10);
    assert_eq!(receipt.net, 80);
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        83
    );
    let (rows, delta, reason): (i64, i64, String) = Connection::open(database.path())
        .expect("open wheel ledger")
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(delta),0), COALESCE(MAX(reason),'')
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='low_priority_tax'",
            params![42, 7],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read low priority tax ledger row");
    assert_eq!(
        (rows, delta, reason.as_str()),
        (1, -10, "low priority tax on JC profit")
    );
}

#[test]
fn wheel_income_leaves_players_outside_the_low_priority_set_untaxed() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(8, "untaxed", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    let receipt = credit_wheel_income(
        database.path(),
        42,
        8,
        0,
        100,
        &runtime,
        "322:8:abc",
        "test wheel reward",
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .expect("credit untaxed wheel reward");
    assert_eq!(receipt.vanity_tax, 0);
    assert_eq!(receipt.low_priority_tax, 0);
    assert_eq!(receipt.net, 100);
}

#[test]
fn wheel_positive_delta_applies_blood_pact_once_through_protection_gateway() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "target", Some(42)))
        .expect("target");
    players
        .add(&NewPlayer::new(9, "skimmer", Some(42)))
        .expect("skimmer");
    let skimmer_balance_before = players
        .get_by_id(9, Some(42))
        .expect("skimmer lookup")
        .expect("skimmer")
        .jopacoin_balance;
    let blood_pact = BuffService::new(ManashopRepository::new(database.path()), 1_700_000_000);
    blood_pact
        .grant_blood_pact(9, 42, 7)
        .expect("grant Blood Pact");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let runtime = BettingRuntimeConfig::from_application_config(&config);
    let credit = credit_wheel_income(
        database.path(),
        42,
        7,
        0,
        100,
        &runtime,
        "wheel-blood-pact",
        "test wheel reward",
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .expect("credit wheel reward");

    let first = apply_wheel_blood_pact_skim(
        database.path(),
        42,
        7,
        0,
        "wheel-blood-pact",
        1_700_000_001,
        runtime.minigame_jc_delta_scale,
    )
    .expect("first Blood Pact skim");
    let expected_skim = cama_domain::economy_scaling::scale_minigame_jc_delta(
        25.0,
        runtime.minigame_jc_delta_scale,
    );
    assert_eq!(first, (expected_skim, credit.balance_after - expected_skim));
    assert_eq!(
        players
            .get_by_id(9, Some(42))
            .expect("skimmer lookup")
            .expect("skimmer")
            .jopacoin_balance,
        skimmer_balance_before + expected_skim
    );

    let retry = apply_wheel_blood_pact_skim(
        database.path(),
        42,
        7,
        0,
        "wheel-blood-pact",
        1_700_000_002,
        runtime.minigame_jc_delta_scale,
    )
    .expect("retry Blood Pact skim");
    assert_eq!(retry, first);
    assert_eq!(
        blood_pact
            .blood_pact_skimmer(7, 42)
            .expect("Blood Pact lookup")
            .expect("active Blood Pact")
            .data
            .skimmed_total,
        Some(expected_skim)
    );
}

#[tokio::test]
async fn wheel_component_after_restart_returns_expiry_copy() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let responder = RecordingResponder::default();
    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 44,
                custom_id: "disc_expired:0".to_owned(),
                user_id: 7,
                user_display_name: "player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                values: Vec::new(),
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("expired component response");
    let responses = responder.responses.lock().expect("responses lock");
    assert_eq!(responses.len(), 1);
    assert!(responses[0].ephemeral);
    assert!(responses[0].content.contains("interaction expired"));
    assert!(responses[0].content.contains("/gamba"));
}

#[tokio::test]
async fn wheel_choice_rejects_non_owner_but_town_vote_is_public() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Discover,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: vec![WheelOption {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#000000",
        }],
        wheel_wedges: Vec::new(),
        golden: false,
        display_name: None,
        initial_attachment: None,
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Bankrupt,
        balance_before: -10,
        effects: ManaEffects::default(),
        event_id: "test".to_owned(),
        config: BettingRuntimeConfig::from_application_config(&config),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: None,
    };
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("wheel state lock")
        .insert("disc_test".to_owned(), pending);
    let responder = RecordingResponder::default();
    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 45,
                custom_id: "disc_test:0".to_owned(),
                user_id: 8,
                user_display_name: "spectator".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                values: Vec::new(),
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("owner guard response");
    let responses = responder.responses.lock().expect("responses lock");
    assert_eq!(responses[0].content, "This choice isn't yours to make.");
}

#[tokio::test(start_paused = true)]
async fn wheel_component_edits_public_prompt_after_resolution() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "player", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let responder = RecordingResponder::default();
    let initial_attachment = InteractionAttachment::bytes("wheel.gif", vec![1, 2, 3, 4]);
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Discover,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: vec![WheelOption {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#000000",
        }],
        wheel_wedges: Vec::new(),
        golden: false,
        display_name: Some("player".to_owned()),
        initial_attachment: Some(initial_attachment.clone()),
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Regular,
        balance_before: 0,
        effects: ManaEffects::default(),
        event_id: "public-edit".to_owned(),
        config: BettingRuntimeConfig::from_application_config(&config),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: Some(Arc::new(responder.clone())),
    };
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("wheel state lock")
        .insert("disc_public-edit".to_owned(), pending);

    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 46,
                custom_id: "disc_public-edit:0".to_owned(),
                user_id: 7,
                user_display_name: "player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                values: Vec::new(),
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("resolve wheel component");

    let responses = responder.responses.lock().expect("responses lock");
    assert!(responses.is_empty());
    drop(responses);
    assert_eq!(*responder.defers.lock().expect("defers lock"), [false]);
    let edits = responder.edits.lock().expect("edits lock");
    assert_eq!(edits.len(), 2);
    assert!(!edits[0].ephemeral);
    assert!(edits[0].content.is_empty());
    assert!(edits[0].embeds.is_empty());
    assert_eq!(edits[0].attachments, vec![initial_attachment]);
    assert!(edits[1].content.is_empty());
    assert_eq!(
        edits[1].embeds[0].title.as_deref(),
        Some("🚫 LOSE A TURN 🚫")
    );
    assert_eq!(edits[1].embeds.len(), 1);
    assert!(edits[1].attachments.is_empty());
    assert_eq!(
        edits[1].attachment_policy,
        crate::registration::InteractionAttachmentPolicy::Preserve
    );
}

#[tokio::test(start_paused = true)]
async fn blue_mana_sixty_jc_choice_is_acknowledged_before_render_and_settles_at_75_percent() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "player", Some(42)))
        .expect("player");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let wedges = vec![
        cama_app::wheel::WheelWedge {
            label: "10".to_owned(),
            value: WheelValue::Numeric(10),
            color: "#ffffff",
        },
        cama_app::wheel::WheelWedge {
            label: "60".to_owned(),
            value: WheelValue::Numeric(60),
            color: "#000000",
        },
    ];
    let expected =
        render_wheel_attachment(&wedges, 1, false, Some("player")).expect("selected wheel GIF");
    let responder = RecordingResponder::default();
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Scrying,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: wedges.iter().map(wheel_option).collect(),
        wheel_wedges: wedges,
        golden: false,
        display_name: Some("player".to_owned()),
        // Production does not render a random wedge before the user picks.
        initial_attachment: None,
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Regular,
        balance_before: 0,
        effects: ManaEffects {
            color: Some("Blue".to_owned()),
            blue_gamba_reduction: 0.25,
            ..Default::default()
        },
        event_id: "blue-scry-media".to_owned(),
        config: BettingRuntimeConfig::from_application_config(&config),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: Some(Arc::new(responder.clone())),
    };
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("wheel state lock")
        .insert("betting:scry:blue-scry-media".to_owned(), pending);

    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 48,
                custom_id: "betting:scry:blue-scry-media:b".to_owned(),
                user_id: 7,
                user_display_name: "player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                values: Vec::new(),
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("resolve Blue mana choice");

    assert!(
        responder
            .responses
            .lock()
            .expect("responses lock")
            .is_empty()
    );
    assert_eq!(*responder.defers.lock().expect("defers lock"), [false]);
    assert_eq!(
        *responder.events.lock().expect("events lock"),
        ["defer", "edit_original", "edit_original"]
    );
    let edits = responder.edits.lock().expect("edits lock");
    assert_eq!(edits.len(), 2);
    assert!(edits[0].content.is_empty());
    assert!(edits[0].embeds.is_empty());
    assert_eq!(edits[0].attachments, vec![expected]);
    assert!(edits[1].content.is_empty());
    assert_eq!(edits[1].embeds.len(), 1);
    assert_eq!(edits[1].embeds[0].title.as_deref(), Some("🎉 Winner!"));
    assert!(
        edits[1].embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("**+45 JC**"))
    );
    assert!(edits[1].attachments.is_empty());
    assert_eq!(
        edits[1].attachment_policy,
        crate::registration::InteractionAttachmentPolicy::Preserve
    );
}

#[tokio::test(start_paused = true)]
async fn wheel_component_reroll_replaces_public_gif_attachment() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "player", Some(42)))
        .expect("player");
    ManaRepository::new(database.path())
        .set_mana(7, Some(42), "Mountain", "2025-01-01")
        .expect("red mana");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let effects = ManaEffects {
        color: Some("Red".to_owned()),
        ..Default::default()
    };
    let wedges = vec![
        cama_app::wheel::WheelWedge {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#000000",
        },
        cama_app::wheel::WheelWedge {
            label: "GOOD".to_owned(),
            value: WheelValue::Numeric(5),
            color: "#ffffff",
        },
    ];
    let initial_attachment =
        render_wheel_attachment(&wedges, 0, false, None).expect("initial wheel GIF");
    let responder = RecordingResponder::default();
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Reroll,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: wedges.iter().map(wheel_option).collect(),
        wheel_wedges: wedges.clone(),
        golden: false,
        display_name: Some("player".to_owned()),
        initial_attachment: Some(initial_attachment.clone()),
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Bankrupt,
        balance_before: 0,
        effects,
        event_id: "reroll-media".to_owned(),
        config: BettingRuntimeConfig::from_application_config(&config),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: Some(Arc::new(responder.clone())),
    };
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    provider
        .handler
        .wheel_interactions
        .lock()
        .expect("wheel state lock")
        .insert("betting:reroll:reroll-media".to_owned(), pending);

    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 47,
                custom_id: "betting:reroll:reroll-media:yes".to_owned(),
                user_id: 7,
                user_display_name: "player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(99),
                member_permissions: None,
                values: Vec::new(),
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("resolve reroll component");

    let responses = responder.responses.lock().expect("responses lock");
    assert!(responses.is_empty());
    drop(responses);
    assert_eq!(*responder.defers.lock().expect("defers lock"), [false]);
    let edits = responder.edits.lock().expect("edits lock");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].attachments.len(), 1);
    assert_eq!(edits[0].attachments[0].filename, "wheel.gif");
    assert_ne!(edits[0].attachments[0].bytes, initial_attachment.bytes);
    assert!(edits[1].content.is_empty());
    assert_eq!(edits[1].embeds.len(), 1);
    assert!(edits[1].attachments.is_empty());
    assert_eq!(
        edits[1].attachment_policy,
        crate::registration::InteractionAttachmentPolicy::Preserve
    );
    assert!(
        ManaRepository::new(database.path())
            .is_bankrupt_buff_used(7, Some(42), BankruptBuff::Reroll)
            .expect("reroll claim")
    );
}

#[test]
fn hostile_result_notes_preserve_python_detail_fields() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let config = BettingRuntimeConfig::from_application_config(&config);
    let lightning = HostileResolution {
        lightning_total: 27,
        lightning_count: 2,
        total: 27,
        count: 2,
        victims: vec![("alice".to_owned(), 19), ("bob".to_owned(), 8)],
        shield_absorbed_total: 3,
        shielded_count: 1,
        ..HostileResolution::default()
    };
    let note = hostile_result_note(WheelMechanic::LightningBolt, &lightning, &config);
    assert!(note.contains("**2** player(s)"));
    assert!(note.contains("27"));
    assert!(note.contains("alice"));
    assert!(note.contains("White Mana Shields"));

    let shell = HostileResolution {
        total: 12,
        victim_name: Some("rival".to_owned()),
        victim_new_balance: Some(88),
        ..HostileResolution::default()
    };
    let note = hostile_result_note(WheelMechanic::RedShell, &shell, &config);
    assert!(note.contains("rival"));
    assert!(note.contains("Victim's new balance: **88**"));
}

#[test]
fn hostile_mechanics_only_target_visible_guild_members() {
    // Python parity: every hostile victim pool reads the guild-member
    // visibility-filtered leaderboard, so a departed ex-member with the
    // largest balance can neither hold a rank nor be robbed.
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(1, "spinner", Some(42)))
        .expect("spinner");
    players
        .add(&NewPlayer::new(2, "member", Some(42)))
        .expect("member");
    players
        .add(&NewPlayer::new(99, "departed", Some(42)))
        .expect("departed");
    players
        .update_balance(1, Some(42), 100)
        .expect("spinner balance");
    players
        .update_balance(2, Some(42), 100)
        .expect("member balance");
    players
        .update_balance(99, Some(42), 1_000)
        .expect("departed balance");
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("configuration");
    let config = BettingRuntimeConfig::from_application_config(&config);
    let visible = BTreeSet::from([1_i64, 2]);

    let resolution = resolve_hostile_mechanic(
        database.path(),
        42,
        1,
        100,
        WheelMechanic::Heist,
        "heist-visibility",
        &config,
        &BTreeSet::new(),
        &BTreeSet::new(),
        1.0,
        Some(&visible),
    )
    .expect("resolve heist");

    assert!(resolution.total > 0, "the visible member must be robbed");
    let departed_balance = players
        .get_by_id(99, Some(42))
        .expect("read departed")
        .expect("departed row")
        .jopacoin_balance;
    assert_eq!(
        departed_balance, 1_000,
        "a departed ex-member must never be a hostile victim"
    );
    let member_balance = players
        .get_by_id(2, Some(42))
        .expect("read member")
        .expect("member row")
        .jopacoin_balance;
    assert!(
        member_balance < 100,
        "the visible member funds the heist (was {member_balance})"
    );
}

/// Builds a disbursement candidate; only the fields the methods read matter.
fn disburse_player(discord_id: i64, balance: i64, games: i64) -> Player {
    let mut player = Player::new(format!("player{discord_id}"));
    player.discord_id = Some(discord_id);
    player.jopacoin_balance = balance;
    player.wins = games;
    player.losses = 0;
    player
}

#[test]
fn richest_disbursement_pays_the_highest_balance() {
    let players = vec![
        disburse_player(1, 1000, 5),
        disburse_player(2, 100, 5),
        disburse_player(3, -100, 5),
    ];

    let distributions = calculate_distributions("richest", 500, &players, None);

    assert_eq!(distributions, vec![(1, 500)]);
}

#[test]
fn richest_disbursement_breaks_balance_ties_on_the_lowest_id() {
    let players = vec![disburse_player(7, 250, 1), disburse_player(3, 250, 1)];

    let distributions = calculate_distributions("richest", 90, &players, None);

    assert_eq!(distributions, vec![(3, 90)]);
}

#[test]
fn stimulus_disbursement_requires_games_played() {
    // Five non-debtors, but two never played: only three remain, which is not
    // enough to survive the top-three exclusion.
    let players = vec![
        disburse_player(1, 900, 1),
        disburse_player(2, 800, 1),
        disburse_player(3, 700, 1),
        disburse_player(4, 600, 0),
        disburse_player(5, 500, 0),
    ];

    assert!(calculate_distributions("stimulus", 300, &players, None).is_empty());
}

#[test]
fn stimulus_disbursement_skips_the_three_richest_players_who_played() {
    let players = vec![
        disburse_player(1, 900, 2),
        disburse_player(2, 800, 2),
        disburse_player(3, 700, 2),
        disburse_player(4, 600, 2),
        disburse_player(5, 500, 2),
        // Unplayed accounts never displace a real player from the top three.
        disburse_player(6, 5000, 0),
    ];

    let distributions = calculate_distributions("stimulus", 100, &players, None);

    assert_eq!(distributions, vec![(4, 50), (5, 50)]);
}

#[test]
fn social_security_excludes_the_three_richest_non_debtors() {
    let players = vec![
        disburse_player(1, 900, 10),
        disburse_player(2, 800, 10),
        disburse_player(3, 700, 10),
        disburse_player(4, 50, 6),
        disburse_player(5, -500, 4),
    ];

    let distributions = calculate_distributions("social_security", 100, &players, None);

    // Debtors are never part of the richest exclusion, so 4 and 5 split the
    // fund 6:4 by games played.
    assert_eq!(distributions, vec![(4, 60), (5, 40)]);
}

#[test]
fn lottery_disbursement_pays_the_drawn_winner_and_nothing_without_one() {
    let players = vec![disburse_player(1, 10, 1), disburse_player(2, 20, 1)];

    assert_eq!(
        calculate_distributions("lottery", 250, &players, Some(2)),
        vec![(2, 250)]
    );
    assert!(calculate_distributions("lottery", 250, &players, None).is_empty());
}

#[test]
fn lottery_draw_is_uniform_over_the_active_roster() {
    let candidates = [11_i64, 22, 33];
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..500 {
        let winner = draw_lottery_winner(&candidates).expect("non-empty roster draws a winner");
        assert!(candidates.contains(&winner));
        seen.insert(winner);
    }

    assert_eq!(seen.len(), candidates.len(), "every candidate can win");
    assert_eq!(draw_lottery_winner(&[]), None);
}
