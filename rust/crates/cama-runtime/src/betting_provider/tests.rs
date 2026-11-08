use super::*;
use cama_app::economy_actions::apply_gamba_event_multiplier;
use cama_app::manashop_rework::BuffService;
use cama_app::match_recording::GamesMilestone;
use cama_app::wheel::{
    CooldownOutcome, WheelEconomyPolicy, cooldown_after_resolution, get_wheel_wedges,
    next_wheel_reset_at, resolve_explosion, wheel_window_start_at,
};
use cama_app::wheel_settlement::{
    apply_wheel_blood_pact_skim, credit_wheel_income, event_related_id, resolve_wheel_value,
};
use cama_db::core_repositories::NewPlayer;
use cama_db::economy_event_repository::EconomyEventRepository;
use cama_db::mana_service_repository::{BankruptBuff, ManaRepository};
use cama_db::manashop_rework_repository::ManashopRepository;
use cama_db::match_runtime::{PendingMatchRepository, PendingMatchState};
use cama_db::wheel_spin_repository::{NewWheelSpin, WheelSpinRepository};
use rusqlite::{Connection, params};
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
    // No live or last-seen name exists, so the list shows the readable
    // placeholder rather than a mention or the registration name.
    let listed = recording.responses.lock().unwrap()[0].content.clone();
    assert!(listed.contains("1. LONG Unknown player — 7%"), "{listed}");
    assert!(!listed.contains("<@"));
    assert!(!listed.contains("target"));
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

#[tokio::test]
async fn economy_loan_defers_before_the_registration_lookup() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("loan configuration");
    let options = vec![InteractionOption {
        name: "amount".to_owned(),
        value: InteractionValue::Integer(50),
    }];

    // An unmigrated database makes the registration lookup fail, so only an
    // acknowledgement sent before that SQLite work can appear here.
    let unmigrated = NamedTempFile::new().expect("unmigrated loan database");
    let provider = BettingRegistrationProvider::new(
        unmigrated.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    let result = provider
        .handler
        .loan(1, Some(42), None, &options, &responder)
        .await;
    assert!(result.is_err());
    assert_eq!(*recording.events.lock().unwrap(), ["defer"]);
    assert_eq!(*recording.defers.lock().unwrap(), [false]);

    // Unregistered players get the private rejection as a follow-up.
    let database = NamedTempFile::new().expect("loan database");
    initialize_or_migrate(database.path()).expect("loan schema");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    provider
        .handler
        .loan(1, Some(42), None, &options, &responder)
        .await
        .expect("unregistered loan rejection");
    assert_eq!(*recording.events.lock().unwrap(), ["defer", "followup"]);
    let responses = recording.responses.lock().unwrap();
    assert!(responses[0].ephemeral);
    assert_eq!(
        responses[0].content,
        "You need to /player register before taking loans."
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
fn synthetic_wheel_members_require_explicit_opt_in_and_negative_ids() {
    assert!(is_enabled_synthetic_wheel_member(-101, true));
    assert!(!is_enabled_synthetic_wheel_member(-101, false));
    assert!(!is_enabled_synthetic_wheel_member(101, true));
}

#[tokio::test]
async fn wheel_member_snapshot_complete_cache_skips_member_requests() {
    let visible = resolve_wheel_member_snapshot(
        vec![1, 2, -101],
        Some(BTreeSet::from([1, 99])),
        true,
        |_| async { panic!("complete cache must not request members") },
    )
    .await
    .expect("complete cached snapshot");
    assert_eq!(visible, BTreeSet::from([-101, 1]));

    let empty =
        resolve_wheel_member_snapshot(vec![1, -101], Some(BTreeSet::new()), false, |_| async {
            panic!("empty complete cache must not request members")
        })
        .await
        .expect("authoritative empty snapshot");
    assert!(empty.is_empty());
}

#[tokio::test(start_paused = true)]
async fn wheel_member_snapshot_fallback_overlaps_requests_with_a_bound() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let started = tokio::time::Instant::now();
    let visible = resolve_wheel_member_snapshot((1..=24).collect(), None, false, |user_id| {
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        async move {
            let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(concurrent, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(user_id % 2 == 0)
        }
    })
    .await
    .expect("fallback member snapshot");

    assert_eq!(visible, (1..=24).filter(|id| id % 2 == 0).collect());
    assert_eq!(peak.load(Ordering::SeqCst), 8);
    assert_eq!(started.elapsed(), Duration::from_secs(3));
}

#[tokio::test]
async fn wheel_member_snapshot_fallback_preserves_unknown_and_synthetic_members() {
    let error = resolve_wheel_member_snapshot(vec![1, 2], None, false, |user_id| async move {
        if user_id == 2 {
            Err("Discord unavailable".to_owned())
        } else {
            Ok(true)
        }
    })
    .await;
    assert_eq!(error, Err("Discord unavailable".to_owned()));

    for enabled in [false, true] {
        let visible =
            resolve_wheel_member_snapshot(vec![-101, 1, 2], None, enabled, |id| async move {
                assert!(id <= 2, "synthetic IDs must not reach Discord");
                Ok(id == 1)
            })
            .await
            .expect("synthetic member snapshot");
        assert_eq!(
            visible,
            if enabled {
                BTreeSet::from([-101, 1])
            } else {
                BTreeSet::from([1])
            }
        );
    }
}

#[tokio::test(start_paused = true)]
async fn wheel_member_snapshot_failure_cancels_pending_requests() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct PendingLookupGuard {
        dropped: Arc<AtomicBool>,
        cancelled: Arc<tokio::sync::Notify>,
    }

    impl Drop for PendingLookupGuard {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
            self.cancelled.notify_one();
        }
    }

    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let result = resolve_wheel_member_snapshot(vec![1, 2], None, false, |user_id| {
        let started = Arc::clone(&started);
        let cancelled = Arc::clone(&cancelled);
        let dropped = Arc::clone(&dropped);
        async move {
            if user_id == 1 {
                let _guard = PendingLookupGuard { dropped, cancelled };
                started.notify_one();
                std::future::pending::<()>().await;
                Ok(true)
            } else {
                started.notified().await;
                Err("Discord unavailable".to_owned())
            }
        }
    })
    .await;

    assert_eq!(result, Err("Discord unavailable".to_owned()));
    tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
        .await
        .expect("failed snapshot must cancel its pending lookup");
    assert!(dropped.load(Ordering::SeqCst));
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
    assert!(cooldown.contains("The wheel resets in"));

    // Crossing the fixed 4 AM UTC-8 boundary refreshes the wheel even when
    // the prior spin was only one second earlier.
    let current_window = wheel_window_start_at(now);
    players
        .set_last_wheel_spin(7, Some(42), current_window - 1)
        .expect("set pre-reset wheel spin");
    assert_eq!(
        gamba_interaction_preflight(database.path(), 42, 7, current_window, false)
            .expect("fixed reset preflight"),
        None
    );
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

#[test]
fn black_mana_caps_bankruptcy_penalty_games_before_declaration() {
    let black = ManaEffects::for_color(Some("Black"), Some("Swamp"));
    assert!(black.swamp_bankruptcy_games < 10);
    assert_eq!(
        bankruptcy_penalty_games(10, &black),
        black.swamp_bankruptcy_games
    );
    assert_eq!(bankruptcy_penalty_games(1, &black), 1);
    assert_eq!(bankruptcy_penalty_games(10, &ManaEffects::default()), 10);
}

#[tokio::test]
async fn bankruptcy_announcement_reports_the_fresh_start_balance() {
    let database = NamedTempFile::new().expect("bankruptcy database");
    initialize_or_migrate(database.path()).expect("bankruptcy schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "bankrupt-player", Some(42)))
        .expect("bankrupt player");
    players
        .update_balance(7, Some(42), -100)
        .expect("player in debt");
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BANKRUPTCY_FRESH_START_BALANCE" => Some("7".to_owned()),
        "BANKRUPTCY_PENALTY_GAMES" => Some("4".to_owned()),
        _ => None,
    })
    .expect("bankruptcy configuration");
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    provider
        .handler
        .bankruptcy(7, Some(42), None, &responder)
        .await
        .expect("bankruptcy filing");

    let responses = recording.responses.lock().unwrap();
    let content = &responses[0].content;
    assert!(content.contains("Debt cleared: 100"), "{content}");
    assert!(content.contains("New balance: 7 "), "{content}");
    assert!(content.contains("**WIN** 4 games"), "{content}");
    let (balance, games): (i64, i64) = Connection::open(database.path())
        .expect("bankruptcy readback")
        .query_row(
            "SELECT p.jopacoin_balance, b.penalty_games_remaining
             FROM players p JOIN bankruptcy_state b
               ON b.discord_id = p.discord_id AND b.guild_id = p.guild_id
             WHERE p.discord_id = 7 AND p.guild_id = 42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("bankruptcy state");
    assert_eq!((balance, games), (7, 4));
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
            Some("💰 Jopacoin Portfolio — Unknown player")
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
                attempt: Arc::default(),
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
async fn neon_hooks_run_on_the_blocking_pool() {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("Neon configuration");
    let provider = BettingRegistrationProvider::new(
        "/tmp/cama-mm-betting-neon-blocking.sqlite",
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let async_thread = std::thread::current().id();
    let hook_thread = provider
        .handler
        .run_neon(|_| std::thread::current().id())
        .await
        .expect("Neon hook");
    assert_ne!(hook_thread, async_thread);
}

/// Balances of the spinner (7), two other players (8, 9), and the reserve.
fn retry_economy_snapshot(path: &Path) -> [i64; 4] {
    let players = PlayerRepository::new(path);
    let balance = |discord_id| {
        players
            .get_by_id(discord_id, Some(42))
            .expect("retry player lookup")
            .expect("retry player")
            .jopacoin_balance
    };
    [
        balance(7),
        balance(8),
        balance(9),
        LoanRepository::new(path)
            .get_nonprofit_fund(Some(42))
            .expect("retry reserve"),
    ]
}

/// Settle one expired interactive spin whose first wheel-history write fails,
/// then retry it. Returns the database and the economy after each attempt.
async fn retry_wheel_after_failed_history_write(
    spin_kind: WheelKind,
    option: WheelOption,
    seed: impl FnOnce(&Path),
) -> (NamedTempFile, [i64; 4], [i64; 4]) {
    let database = NamedTempFile::new().expect("temporary retry database");
    initialize_or_migrate(database.path()).expect("retry schema");
    let players = PlayerRepository::new(database.path());
    for (discord_id, balance) in [(7, 100), (8, 1_000), (9, 500)] {
        players
            .add(&NewPlayer::new(discord_id, "retry-player", Some(42)))
            .expect("retry player");
        players
            .update_balance(discord_id, Some(42), balance)
            .expect("retry balance");
    }
    seed(database.path());
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
            "disc_effect_retry".to_owned(),
            PendingWheelInteraction {
                kind: WheelInteractionKind::Discover,
                user_id: 7,
                guild_id: 42,
                created_at: Instant::now() - Duration::from_secs(120),
                options: vec![option],
                wheel_wedges: Vec::new(),
                golden: false,
                display_name: None,
                initial_attachment: None,
                votes: BTreeMap::new(),
                spin_kind,
                balance_before: 100,
                effects: ManaEffects::default(),
                event_id: "effect-retry-event".to_owned(),
                config: BettingRuntimeConfig::from_application_config(&config),
                event_win_multiplier: 1.0,
                event_loss_multiplier: 1.0,
                bonus_spin: false,
                last_regular_spin: None,
                original_responder: None,
                attempt: Arc::default(),
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
    assert!(
        provider.expire_pending_views().await.is_err(),
        "the first settlement attempt fails at the history write"
    );
    let after_failure = retry_economy_snapshot(database.path());
    Connection::open(database.path())
        .expect("reopen retry database")
        .execute("DROP TRIGGER fail_first_wheel_log", [])
        .expect("remove transient failure");
    assert_eq!(
        provider
            .expire_pending_views()
            .await
            .expect("the retried settlement completes"),
        1
    );
    assert!(
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("settled interaction lock")
            .is_empty()
    );
    let spin_count: i64 = Connection::open(database.path())
        .expect("open retry result")
        .query_row(
            "SELECT COUNT(*) FROM wheel_spins WHERE event_id='effect-retry-event'",
            [],
            |row| row.get(0),
        )
        .expect("retry spin count");
    assert_eq!(spin_count, 1);
    let after_retry = retry_economy_snapshot(database.path());
    (database, after_failure, after_retry)
}

fn mechanic_option(mechanic: WheelMechanic) -> WheelOption {
    WheelOption {
        label: mechanic.code().to_owned(),
        value: WheelValue::Mechanic(mechanic),
        color: "#000000",
    }
}

#[tokio::test]
async fn retried_chain_reaction_copies_a_loss_exactly_once() {
    let (_database, after_failure, after_retry) = retry_wheel_after_failed_history_write(
        WheelKind::Bankrupt,
        mechanic_option(WheelMechanic::ChainReaction),
        |path| {
            WheelSpinRepository::new(path)
                .log_wheel_spin(&NewWheelSpin {
                    event_id: Some("copied-spin".to_owned()),
                    ..NewWheelSpin::legacy(8, Some(42), -30, 1_000)
                })
                .expect("seed copied losing spin");
        },
    )
    .await;
    assert_eq!(after_failure, [70, 1_000, 500, 30]);
    assert_eq!(after_retry, after_failure);
}

#[tokio::test]
async fn retried_recession_charges_each_victim_exactly_once() {
    let (database, after_failure, after_retry) = retry_wheel_after_failed_history_write(
        WheelKind::Regular,
        mechanic_option(WheelMechanic::Recession),
        |_| {},
    )
    .await;
    assert!(after_failure[1] < 1_000 && after_failure[2] < 500);
    assert_eq!(after_retry, after_failure);
    let events: i64 = Connection::open(database.path())
        .expect("open recession events")
        .query_row(
            "SELECT COUNT(*) FROM hostile_loss_events
             WHERE event_key GLOB 'effect-retry-event:*'",
            [],
            |row| row.get(0),
        )
        .expect("count recession events");
    assert_eq!(events, 3);
}

#[tokio::test]
async fn retried_market_crash_keeps_its_random_amounts() {
    let (_database, after_failure, after_retry) = retry_wheel_after_failed_history_write(
        WheelKind::Regular,
        mechanic_option(WheelMechanic::MarketCrash),
        |_| {},
    )
    .await;
    assert!(
        after_failure[0] > 100,
        "the spinner stole from the top players"
    );
    assert_eq!(after_retry, after_failure);
}

#[tokio::test]
async fn retried_bankrupt_pardon_forgives_the_loss_exactly_once() {
    let (database, after_failure, after_retry) = retry_wheel_after_failed_history_write(
        WheelKind::Bankrupt,
        WheelOption {
            label: "BANKRUPT".to_owned(),
            value: WheelValue::Numeric(-20),
            color: "#1a1a1a",
        },
        |path| {
            PlayerRepository::new(path)
                .set_wheel_pardon(7, Some(42), true)
                .expect("arm pardon");
        },
    )
    .await;
    assert_eq!(after_failure, [100, 1_000, 500, 0]);
    assert_eq!(after_retry, after_failure);
    let pardon_armed: i64 = Connection::open(database.path())
        .expect("open pardon state")
        .query_row(
            "SELECT COALESCE(has_wheel_pardon,0) FROM players WHERE discord_id=7 AND guild_id=42",
            [],
            |row| row.get(0),
        )
        .expect("read pardon");
    assert_eq!(pardon_armed, 0, "the pardon is consumed exactly once");
    let logged: i64 = Connection::open(database.path())
        .expect("open pardon history")
        .query_row(
            "SELECT result FROM wheel_spins WHERE event_id='effect-retry-event'",
            [],
            |row| row.get(0),
        )
        .expect("pardoned spin");
    assert_eq!(logged, 0);
}

#[tokio::test]
async fn retried_extend_adds_its_penalty_game_exactly_once() {
    let (database, _, _) = retry_wheel_after_failed_history_write(
        WheelKind::Bankrupt,
        mechanic_option(WheelMechanic::ExtendOne),
        |path| {
            BankruptcyRepository::new(path)
                .adjust_penalty_games_atomic(7, Some(42), 2)
                .expect("seed penalty");
        },
    )
    .await;
    assert_eq!(
        BankruptcyRepository::new(database.path())
            .get_penalty_games(7, Some(42))
            .expect("read penalty"),
        3
    );
}

#[test]
fn a_retried_selection_keeps_the_first_attempts_option_and_reroll() {
    let first = mechanic_option(WheelMechanic::Recession);
    let pending = PendingWheelInteraction {
        kind: WheelInteractionKind::Reroll,
        user_id: 7,
        guild_id: 42,
        created_at: Instant::now(),
        options: vec![first.clone(), mechanic_option(WheelMechanic::Jailbreak)],
        wheel_wedges: Vec::new(),
        golden: false,
        display_name: None,
        initial_attachment: None,
        votes: BTreeMap::new(),
        spin_kind: WheelKind::Bankrupt,
        balance_before: 0,
        effects: ManaEffects::default(),
        event_id: "pinned".to_owned(),
        config: BettingRuntimeConfig::from_application_config(
            &ApplicationConfig::from_lookup(|name| {
                (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
            })
            .expect("configuration"),
        ),
        event_win_multiplier: 1.0,
        event_loss_multiplier: 1.0,
        bonus_spin: false,
        last_regular_spin: None,
        original_responder: None,
        attempt: Arc::default(),
    };
    let (pinned, reroll) = pin_wheel_selection(&pending.attempt, first.clone(), true);
    assert_eq!((pinned.label.as_str(), reroll), ("RECESSION", true));
    // A later click on the other option, or the timeout default, cannot
    // change what the first attempt began settling.
    let clone = pending.clone();
    let (pinned, reroll) = pin_wheel_selection(
        &clone.attempt,
        mechanic_option(WheelMechanic::Jailbreak),
        false,
    );
    assert_eq!(
        (pinned.label.as_str(), reroll),
        (first.label.as_str(), true)
    );
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
                attempt: Arc::default(),
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
async fn lobby_bonus_does_not_inflate_bettor_previews() {
    for mode in ["pool", "house"] {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let now = unix_seconds().expect("timestamp");
        let pending = PendingMatchRepository::new(database.path())
            .create_pending_match(
                42,
                &PendingMatchState {
                    shuffle_timestamp: Some(now.saturating_sub(10)),
                    bet_lock_until: Some(now.saturating_add(600)),
                    betting_mode: mode.to_owned(),
                    bet_seed_radiant: if mode == "pool" { 500 } else { 0 },
                    bet_seed_dire: if mode == "pool" { 500 } else { 0 },
                    bet_seed_bonus: if mode == "house" { 1000 } else { 0 },
                    first_game_pool_reserved: 600,
                    ..PendingMatchState::default()
                },
            )
            .expect("pending match");
        let players = PlayerRepository::new(database.path());
        let betting = BettingServiceRepository::new(database.path());
        for (discord_id, team) in [(7, BettingTeam::Radiant), (8, BettingTeam::Dire)] {
            players
                .add(&NewPlayer::new(discord_id, "bettor", Some(42)))
                .expect("player");
            players
                .update_balance(discord_id, Some(42), 100)
                .expect("balance");
            betting
                .place_bet_atomic(PlaceBetRequest {
                    guild_id: Some(42),
                    pending_match_id: pending.pending_match_id,
                    discord_id,
                    team,
                    amount: 10,
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
        .expect("configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        for command in ["mybets", "bets"] {
            let responder = RecordingResponder::default();
            provider
                .handler
                .handle(
                    InteractionRequest::Command {
                        interaction_id: 3,
                        name: command.to_owned(),
                        user_id: 7,
                        user_display_name: "bettor".to_owned(),
                        guild_id: Some(42),
                        channel_id: Some(99),
                        member_permissions: None,
                        options: Vec::new(),
                    },
                    Arc::new(responder.clone()),
                )
                .await
                .expect("dispatch preview");
            let responses = responder.responses.lock().unwrap();
            if command == "mybets" {
                let expected = if mode == "pool" {
                    "wins: ~220 "
                } else {
                    "wins: 420 "
                };
                assert!(
                    responses
                        .iter()
                        .any(|response| response.content.contains(expected))
                );
            } else {
                let fields = responses
                    .iter()
                    .flat_map(|response| &response.embeds)
                    .flat_map(|embed| &embed.fields)
                    .collect::<Vec<_>>();
                assert!(fields.iter().any(|field| {
                    field.name == "Lobby Bonus"
                        && field.value.contains("600 ")
                        && field
                            .value
                            .contains("shared equally among match participants")
                }));
                let odds = &fields
                    .iter()
                    .find(|field| field.name == "Current Odds")
                    .expect("odds field")
                    .value;
                // The field ends with the wall-clock lock time as a Discord
                // timestamp, whose digits can themselves contain "600" or
                // "1000"; only the amounts above that line are under test.
                let (amounts, lock_line) = odds
                    .split_once("\nBetting closes <t:")
                    .expect("open betting shows its lock time");
                assert!(lock_line.ends_with(":R>"));
                assert!(!amounts.contains("1000"));
                assert!(!amounts.contains("600"));
                assert!(odds.contains(if mode == "pool" {
                    "Seed: Radiant 200 "
                } else {
                    "Winner bonus: 400 "
                }));
            }
        }
    }
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

#[tokio::test]
async fn admin_bets_list_names_bettors_without_mentions_or_registration_names() {
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
    for discord_id in [1_001, 1_002] {
        players
            .add(&NewPlayer::new(
                discord_id,
                format!("registered-{discord_id}"),
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
                amount: 5,
                bet_time: now,
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place bet");
    }
    let discord = Arc::new(PartialMemberCacheDiscord {
        cached: BTreeMap::from([(1_001, "Cached Bettor".to_owned())]),
        ..PartialMemberCacheDiscord::default()
    });
    let provider = partial_member_cache_provider(&database, discord);
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
                member_permissions: Some(ADMINISTRATOR_PERMISSION),
                options: vec![InteractionOption {
                    name: "match".to_owned(),
                    value: InteractionValue::Integer(pending.pending_match_id),
                }],
            },
            Arc::new(responder.clone()),
        )
        .await
        .expect("dispatch bets command");

    let responses = responder.responses.lock().unwrap();
    let dire = responses
        .iter()
        .flat_map(|response| &response.embeds)
        .flat_map(|embed| &embed.fields)
        .filter(|field| field.name.starts_with("🔴 Dire Bets"))
        .map(|field| field.value.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(dire.contains("Cached Bettor • 5"), "{dire}");
    assert!(dire.contains("Unknown player • 5"), "{dire}");
    assert!(!dire.contains("<@"));
    assert!(!dire.contains("registered-"));
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
    let names = BTreeMap::from([(7, "First Player".to_owned())]);
    let announcement = golden_wheel_announcement(7, &[(7, 900), (8, 700)], &names);
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
    assert!(description.contains("First Player is spinning the GOLDEN WHEEL!"));
    assert!(description.contains("**#1** First Player — 900"));
    assert!(description.contains("**#2** Unknown player — 700"));
    assert!(!description.contains("<@"));
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
    assert_eq!(next.next_spin_at, next_wheel_reset_at(900));
    assert!(!next.schedule_reminder);
}

#[test]
fn wheel_and_explosion_attachments_keep_python_filenames() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let wheel = render_wheel_attachment(&wedges, 3, false, None).expect("wheel gif");
    let explosion = render_explosion_attachment().expect("explosion gif");
    assert_eq!(wheel.filename, "wheel.gif");
    assert_eq!(explosion.filename, "explosion.gif");
    assert!(wheel.bytes.starts_with(b"GIF89a"));
    assert!(explosion.bytes.starts_with(b"GIF89a"));
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
        &config.wheel_settlement(),
        "extend-no-penalty",
        &taxable,
        &BTreeSet::new(),
        1.0,
        1.0,
        false,
        None,
        None,
        &BTreeMap::new(),
        0,
        1_700_000_000,
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
        &config.wheel_settlement(),
        "extend-active-penalty",
        &taxable,
        &BTreeSet::new(),
        1.0,
        1.0,
        false,
        None,
        None,
        &BTreeMap::new(),
        0,
        1_700_000_000,
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
            &runtime.wheel_settlement(),
            &event_id,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1.0,
            1.0,
            false,
            None,
            None,
            &BTreeMap::new(),
            0,
            1_700_000_000,
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
            &runtime.wheel_settlement(),
            &event_id,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1.0,
            1.0,
            false,
            None,
            None,
            &BTreeMap::new(),
            0,
            1_700_000_000,
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
            &BTreeMap::new(),
            &Mutex::default(),
            &NoWheelReminders,
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
            bankruptcy_penalty_rate_per_game: 0.0,
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
        attempt: Arc::default(),
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
        attempt: Arc::default(),
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
        response
            .components
            .iter()
            .flat_map(|row| &row.buttons)
            .all(|button| button.custom_id.starts_with("disburse:900:"))
    );
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
async fn disbursement_worker_settles_a_legacy_ballot_without_another_vote() {
    let database = NamedTempFile::new().unwrap();
    initialize_or_migrate(database.path()).unwrap();
    let db = Connection::open(database.path()).unwrap();
    db.execute_batch(
        "INSERT INTO disburse_proposals(guild_id,proposal_id,fund_amount,quorum_required,status)
         VALUES(42,900,50,20,'active'),(43,901,10,20,'active');
         INSERT INTO disburse_votes(guild_id,proposal_id,discord_id,vote_method,voted_at)
         VALUES(42,900,1,'burn',1);",
    )
    .unwrap();
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .unwrap();
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let repository = DisbursementRepository::new(database.path());
    // Starting the worker between daily checks must leave the existing ballot open.
    let worker = provider.disbursement_worker();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let (result, ()) = tokio::join!(
        biased;
        worker.worker.run(WorkerContext::new(receiver)),
        async { shutdown.send(true).unwrap() }
    );
    result.unwrap();
    assert!(repository.get_active_proposal(Some(42)).unwrap().is_some());
    for _ in 0..2 {
        provider.handler.reconcile_disbursements().await.unwrap();
        assert!(repository.get_active_proposal(Some(42)).unwrap().is_none());
        assert_eq!(
            repository
                .get_last_disbursement(Some(42))
                .unwrap()
                .unwrap()
                .total_amount,
            50
        );
        let empty = repository.get_active_proposal(Some(43)).unwrap().unwrap();
        assert_eq!(empty.quorum_required, 1);
        assert!(!empty.quorum_reached());
    }
}

#[tokio::test]
async fn disbursement_status_links_to_the_original_ballot() {
    let database = NamedTempFile::new().unwrap();
    initialize_or_migrate(database.path()).unwrap();
    let repository = DisbursementRepository::new(database.path());
    let proposal = repository.create_proposal_atomic(Some(42), 900, 0).unwrap();
    assert!(repository.set_proposal_message(&proposal, 55, 77).unwrap());
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .unwrap();
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    let recording = RecordingResponder::default();
    let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
    provider
        .handler
        .disburse(1, Some(42), 0, &[], &responder)
        .await
        .unwrap();
    let responses = recording.responses.lock().unwrap();
    assert_eq!(responses.len(), 1);
    assert!(responses[0].ephemeral);
    assert!(
        responses[0]
            .content
            .contains("https://discord.com/channels/42/77/55")
    );
    assert!(responses[0].components.is_empty());
    let stored = repository.get_active_proposal(Some(42)).unwrap().unwrap();
    assert_eq!((stored.channel_id, stored.message_id), (Some(77), Some(55)));
}

#[tokio::test]
async fn disbursement_old_buttons_cannot_vote_on_a_replacement() {
    let database = NamedTempFile::new().expect("reserve button database");
    initialize_or_migrate(database.path()).expect("schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(1, "voter", Some(42)))
        .unwrap();
    let repository = DisbursementRepository::new(database.path());
    repository.create_proposal_atomic(Some(42), 900, 0).unwrap();
    repository
        .reset_and_return_fund_atomic(Some(42), "reset")
        .unwrap();
    repository.create_proposal_atomic(Some(42), 901, 0).unwrap();
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .unwrap();
    let provider = BettingRegistrationProvider::new(
        database.path(),
        &config,
        Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
    );
    for custom_id in ["disburse:burn", "disburse:900:burn"] {
        let recording = RecordingResponder::default();
        provider
            .handler
            .component(
                InteractionRequest::Component {
                    interaction_id: 1,
                    custom_id: custom_id.to_owned(),
                    user_id: 1,
                    user_display_name: "voter".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(7),
                    member_permissions: None,
                    values: Vec::new(),
                },
                Arc::new(recording.clone()),
            )
            .await
            .unwrap();
        assert!(
            recording.responses.lock().unwrap()[0]
                .content
                .contains("no longer")
        );
        let proposal = repository.get_active_proposal(Some(42)).unwrap().unwrap();
        assert_eq!(proposal.proposal_id, 901);
        assert_eq!(proposal.total_votes(), 0);
    }
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
    assert_eq!(
        crate::test_support::penance_ledger::gamba_channel_penance_ledger(database.path(), 42),
        crate::test_support::penance_ledger::GAMBA_CHANNEL_PENANCE_LEDGER
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
        &runtime.wheel_settlement(),
        "123:7:abc",
        "test wheel reward",
        &taxable_ids,
        &BTreeSet::new(),
    )
    .expect("credit taxable wheel reward");
    assert_eq!(first.gross, 100);
    assert_eq!(first.vanity_tax, 20);
    assert_eq!(first.net, 80);
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        83
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
        &runtime.wheel_settlement(),
        "321:7:abc",
        "test wheel reward",
        &taxable_ids,
        &taxable_ids,
    )
    .expect("credit doubly taxed wheel reward");
    // Both rates read the 100 JC gross, never the post-vanity remainder.
    assert_eq!(receipt.vanity_tax, 20);
    assert_eq!(receipt.low_priority_tax, 10);
    assert_eq!(receipt.net, 70);
    assert_eq!(
        players
            .get_by_id(7, Some(42))
            .expect("player lookup")
            .expect("player")
            .jopacoin_balance,
        73
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
        &runtime.wheel_settlement(),
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
        &runtime.wheel_settlement(),
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
        credit.balance_delta,
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
        credit.balance_delta,
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

#[test]
fn wheel_blood_pact_skims_only_the_wheel_receipts_not_a_concurrent_credit() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(7, "target", Some(42)))
        .expect("target");
    players
        .add(&NewPlayer::new(9, "skimmer", Some(42)))
        .expect("skimmer");
    BuffService::new(ManashopRepository::new(database.path()), 1_700_000_000)
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
        &runtime.wheel_settlement(),
        "wheel-concurrent-credit",
        "test wheel reward",
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .expect("credit wheel reward");
    // A tip or bet payout lands between the wheel's resolution and its skim.
    let current = players
        .get_by_id(7, Some(42))
        .expect("target lookup")
        .expect("target")
        .jopacoin_balance;
    players
        .update_balance(7, Some(42), current + 1_000)
        .expect("concurrent credit");

    let (skimmed, _) = apply_wheel_blood_pact_skim(
        database.path(),
        42,
        7,
        credit.balance_delta,
        "wheel-concurrent-credit",
        1_700_000_001,
        runtime.minigame_jc_delta_scale,
    )
    .expect("Blood Pact skim");
    assert_eq!(
        skimmed,
        cama_domain::economy_scaling::scale_minigame_jc_delta(
            25.0,
            runtime.minigame_jc_delta_scale
        )
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
        attempt: Arc::default(),
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
        attempt: Arc::default(),
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
        options: wedges.iter().map(WheelOption::from_wedge).collect(),
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
        attempt: Arc::default(),
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
        options: wedges.iter().map(WheelOption::from_wedge).collect(),
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
        attempt: Arc::default(),
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

/// Discord stub with a partial member cache: cached members render from the
/// gateway cache, everyone else needs a member fetch.
#[derive(Default)]
struct PartialMemberCacheDiscord {
    cached: BTreeMap<u64, String>,
    fetchable: BTreeMap<u64, String>,
    last_seen: BTreeMap<u64, String>,
    member_lookups: Mutex<Vec<u64>>,
}

#[async_trait]
impl DiscordTransport for PartialMemberCacheDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        _channel_id: u64,
        _message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        Err("unused transport".to_owned())
    }

    async fn edit_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Err("unused transport".to_owned())
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        user_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberSnapshot>, String> {
        self.member_lookups.lock().unwrap().push(user_id);
        Ok(self.fetchable.get(&user_id).map(|display_name| {
            crate::discord_transport::DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.clone(),
                presence: crate::discord_transport::DiscordPresence::Unknown,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            }
        }))
    }

    fn cached_guild_member_render_names(
        &self,
        _guild_id: u64,
        user_ids: &[u64],
    ) -> Result<Option<BTreeMap<u64, String>>, String> {
        Ok(Some(
            user_ids
                .iter()
                .filter_map(|id| self.cached.get(id).map(|name| (*id, name.clone())))
                .collect(),
        ))
    }

    fn last_seen_player_names(&self, _guild_id: u64, user_ids: &[u64]) -> BTreeMap<u64, String> {
        user_ids
            .iter()
            .filter_map(|id| self.last_seen.get(id).map(|name| (*id, name.clone())))
            .collect()
    }
}

fn partial_member_cache_provider(
    database: &NamedTempFile,
    discord: Arc<PartialMemberCacheDiscord>,
) -> BettingRegistrationProvider {
    let config = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
    })
    .expect("partial member cache configuration");
    BettingRegistrationProvider::new(database.path(), &config, discord)
}

#[tokio::test]
async fn betting_names_fall_back_from_cache_to_fetch_to_last_seen_name() {
    let database = NamedTempFile::new().expect("player name database");
    initialize_or_migrate(database.path()).expect("player name schema");
    let discord = Arc::new(PartialMemberCacheDiscord {
        cached: BTreeMap::from([(7, "Cached Seven".to_owned())]),
        fetchable: BTreeMap::from([(697_288_295_876_526_160, "Fetched Player".to_owned())]),
        last_seen: BTreeMap::from([
            (9, "Departed Nine".to_owned()),
            (697_288_295_876_526_160, "Stale Name".to_owned()),
        ]),
        ..PartialMemberCacheDiscord::default()
    });
    let provider = partial_member_cache_provider(&database, Arc::clone(&discord));

    let names = provider
        .handler
        .render_player_names(&[7, 697_288_295_876_526_160, 9, 10], Some(42))
        .await;

    assert_eq!(
        names,
        BTreeMap::from([
            (7, "Cached Seven".to_owned()),
            (697_288_295_876_526_160, "Fetched Player".to_owned()),
            (9, "Departed Nine".to_owned()),
            (10, "Unknown player".to_owned()),
        ])
    );
    assert_eq!(
        *discord.member_lookups.lock().unwrap(),
        vec![9, 10, 697_288_295_876_526_160],
        "only cache misses reach Discord"
    );
}

#[tokio::test]
async fn wheel_victim_names_use_last_seen_names_beneath_the_cache() {
    let database = NamedTempFile::new().expect("wheel name database");
    initialize_or_migrate(database.path()).expect("wheel name schema");
    let discord = Arc::new(PartialMemberCacheDiscord {
        cached: BTreeMap::from([(7, "Cached Seven".to_owned())]),
        last_seen: BTreeMap::from([
            (7, "Old Seven".to_owned()),
            (8, "Departed Eight".to_owned()),
        ]),
        ..PartialMemberCacheDiscord::default()
    });

    let names = CachedDiscordWheelPlayerNames::new(discord, 42).names_for(&[7, 8, 9]);

    assert_eq!(
        names,
        BTreeMap::from([
            (7, "Cached Seven".to_owned()),
            (8, "Departed Eight".to_owned()),
            (9, "Unknown player".to_owned()),
        ])
    );
}
