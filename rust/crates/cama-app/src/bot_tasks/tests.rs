use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use chrono::{Duration, TimeZone, Utc};

use super::*;

#[derive(Default)]
struct FakePoolPort {
    funding: VecDeque<Result<PoolFunding, PoolTaskError>>,
    refreshes: VecDeque<Result<bool, PoolTaskError>>,
    funding_calls: Vec<(i64, String, i64)>,
    refresh_calls: Vec<(i64, LobbyKind)>,
}

impl FakePoolPort {
    fn funded() -> Self {
        Self {
            funding: VecDeque::from([Ok(PoolFunding {
                funded_dates: vec!["2026-08-05".to_owned()],
            })]),
            ..Self::default()
        }
    }
}

impl FirstGamePoolPort for FakePoolPort {
    fn fund_first_game_pools(
        &mut self,
        guild_id: i64,
        game_date: &str,
        amount_per_lobby: i64,
    ) -> Result<PoolFunding, PoolTaskError> {
        self.funding_calls
            .push((guild_id, game_date.to_owned(), amount_per_lobby));
        self.funding.pop_front().unwrap_or_else(|| {
            Ok(PoolFunding {
                funded_dates: vec![game_date.to_owned()],
            })
        })
    }

    fn refresh_lobby_generation(
        &mut self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<bool, PoolTaskError> {
        self.refresh_calls.push((guild_id, lobby_kind));
        self.refreshes.pop_front().unwrap_or(Ok(true))
    }
}

#[test]
fn test_first_game_pool_loop_funds_every_guild_for_current_game_date() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort::default();
    let report = run_first_game_pool_wake(&mut state, &[41, 42], "2026-08-05", &mut port);

    assert_eq!(
        port.funding_calls,
        [
            (41, "2026-08-05".to_owned(), 100),
            (42, "2026-08-05".to_owned(), 100),
        ]
    );
    assert_eq!(report.sleep_seconds, 900);
}

#[test]
fn test_first_game_pool_loop_skips_guilds_already_processed_for_game_date() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort {
        funding: VecDeque::from([Ok(PoolFunding::default()), Ok(PoolFunding::default())]),
        ..FakePoolPort::default()
    };
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    assert_eq!(port.funding_calls.len(), 1);
    assert_eq!(state.processed_date(41), Some("2026-08-05"));
}

#[test]
fn test_first_game_pool_loop_funds_again_when_game_date_advances() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort::default();
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    run_first_game_pool_wake(&mut state, &[41], "2026-08-06", &mut port);
    assert_eq!(
        port.funding_calls,
        [
            (41, "2026-08-05".to_owned(), 100),
            (41, "2026-08-06".to_owned(), 100),
        ]
    );
}

#[test]
fn test_first_game_pool_loop_retries_failed_guild_on_next_wake() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort {
        funding: VecDeque::from([
            Err(PoolTaskError("db locked".to_owned())),
            Ok(PoolFunding::default()),
        ]),
        ..FakePoolPort::default()
    };
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    assert_eq!(state.processed_date(41), None);
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    assert_eq!(port.funding_calls.len(), 2);
    assert_eq!(state.processed_date(41), Some("2026-08-05"));
}

#[test]
fn test_first_game_pool_loop_refreshes_open_lobby_messages_after_funding() {
    let snapshots = [
        LobbyRefreshSnapshot {
            guild_id: 41,
            lobby_kind: LobbyKind::Open,
            is_open: true,
            message_id: Some(101),
            channel_id: Some(201),
            channel_cached: true,
        },
        LobbyRefreshSnapshot {
            guild_id: 41,
            lobby_kind: LobbyKind::LowSkill,
            is_open: true,
            message_id: Some(102),
            channel_id: Some(202),
            channel_cached: false,
        },
    ];
    assert_eq!(
        snapshots
            .iter()
            .filter_map(lobby_refresh_action)
            .collect::<Vec<_>>(),
        [
            LobbyRefreshAction {
                guild_id: 41,
                lobby_kind: LobbyKind::Open,
                expected_message_id: 101,
                channel_id: 201,
                channel_resolution: ChannelResolution::Cache,
            },
            LobbyRefreshAction {
                guild_id: 41,
                lobby_kind: LobbyKind::LowSkill,
                expected_message_id: 102,
                channel_id: 202,
                channel_resolution: ChannelResolution::Fetch,
            },
        ]
    );
}

#[test]
fn test_first_game_pool_loop_does_not_refresh_after_idempotent_funding() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort {
        funding: VecDeque::from([Ok(PoolFunding::default())]),
        ..FakePoolPort::default()
    };
    run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    assert!(port.refresh_calls.is_empty());
}

#[test]
fn test_first_game_pool_refresh_retries_failed_current_generation() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort::funded();
    port.refreshes = VecDeque::from([Ok(false), Ok(true), Ok(true)]);
    let report = run_first_game_pool_wake(&mut state, &[41], "2026-08-05", &mut port);
    assert_eq!(
        port.refresh_calls,
        [
            (41, LobbyKind::Open),
            (41, LobbyKind::Open),
            (41, LobbyKind::LowSkill),
        ]
    );
    assert_eq!(report.retry_delays, [1]);
    assert!(report.failed_refreshes.is_empty());
}

#[test]
fn test_first_game_pool_loop_isolates_lobby_refresh_failures_across_guilds() {
    let mut state = FirstGamePoolState::default();
    let mut port = FakePoolPort {
        funding: VecDeque::from([
            Ok(PoolFunding {
                funded_dates: vec!["2026-08-05".to_owned()],
            }),
            Ok(PoolFunding {
                funded_dates: vec!["2026-08-05".to_owned()],
            }),
        ]),
        refreshes: VecDeque::from([
            Err(PoolTaskError("missing lobby".to_owned())),
            Err(PoolTaskError("missing lobby".to_owned())),
            Ok(true),
            Ok(true),
            Ok(true),
        ]),
        ..FakePoolPort::default()
    };
    let report = run_first_game_pool_wake(&mut state, &[41, 42], "2026-08-05", &mut port);
    assert_eq!(report.failed_refreshes, [(41, LobbyKind::Open)]);
    assert!(port.refresh_calls.contains(&(42, LobbyKind::Open)));
    assert!(port.refresh_calls.contains(&(42, LobbyKind::LowSkill)));
}

#[test]
fn test_supervised_loop_restarts_after_crash() {
    let report = supervise([
        BodyExit::Crash("boom".to_owned()),
        BodyExit::Clean,
        BodyExit::Crash("must not run".to_owned()),
    ])
    .unwrap();
    assert_eq!(report.calls, 2);
    assert_eq!(report.crash_messages, ["boom"]);
    assert_eq!(report.backoff_seconds, [5]);
}

#[test]
fn test_supervised_loop_propagates_cancellation() {
    assert_eq!(supervise([BodyExit::Cancelled]), Err(SupervisionCancelled));
}

#[test]
fn test_supervised_loop_backoff_caps_at_300s() {
    let mut exits = (0..8)
        .map(|_| BodyExit::Crash("boom".to_owned()))
        .collect::<Vec<_>>();
    exits.push(BodyExit::Clean);
    assert_eq!(
        supervise(exits).unwrap().backoff_seconds,
        [5, 10, 20, 40, 80, 160, 300, 300]
    );
}

#[test]
fn test_log_task_exit_logs_unexpected_exception() {
    let log = unexpected_task_exit_log("test", &TaskExit::Failed("oops".to_owned())).unwrap();
    assert!(log.contains("test exited unexpectedly"));
    assert!(log.contains("oops"));
}

#[test]
fn test_log_task_exit_silent_on_cancel() {
    assert_eq!(unexpected_task_exit_log("test", &TaskExit::Cancelled), None);
}

#[derive(Default)]
struct FakeDuelPort {
    outcomes: VecDeque<Result<(), String>>,
}

impl DuelDeliveryPort for FakeDuelPort {
    fn process_due_challenge(
        &mut self,
        _challenge_id: i64,
        _guild_id: i64,
        _now: i64,
    ) -> Result<(), String> {
        self.outcomes.pop_front().unwrap_or(Ok(()))
    }
}

#[test]
fn test_duel_loop_catches_up_immediately_and_isolates_delivery_failures() {
    let mut port = FakeDuelPort {
        outcomes: VecDeque::from([Err("boom".to_owned()), Ok(())]),
    };
    let report = deliver_due_challenges(&[(7, 42), (8, 42)], 1_700_000_000, &mut port);
    assert_eq!(
        report.attempts,
        [(7, 42, 1_700_000_000), (8, 42, 1_700_000_000)]
    );
    assert_eq!(report.failures, [(7, 42)]);
    assert_eq!(report.sleep_seconds, 60);
}

#[test]
fn test_on_ready_retains_supervised_duel_and_first_game_pool_workers() {
    let mut coordinator = StartupCoordinator::default();
    coordinator.workers.ensure_running([
        "prediction_refresh",
        "prediction_digest",
        "manashop_debt",
        "economy_events",
    ]);
    let first = coordinator.on_ready(["duel_challenges", "first_game_pool"]);
    let second = coordinator.on_ready(["duel_challenges", "first_game_pool"]);
    assert_eq!(
        first,
        [
            WorkerStart {
                name: "duel_challenges".to_owned(),
                supervised: true,
                observe_exit: true,
            },
            WorkerStart {
                name: "first_game_pool".to_owned(),
                supervised: true,
                observe_exit: true,
            },
        ]
    );
    assert!(second.is_empty());
    assert!(coordinator.workers.is_running("duel_challenges"));
    assert!(coordinator.workers.is_running("first_game_pool"));
    assert_eq!(coordinator.lobby_reconciliation_runs, 2);
}

#[test]
fn test_refresh_vanity_tax_memberships_uses_current_guild_members() {
    let snapshots = vec![
        GuildMembersSnapshot {
            guild_id: 42,
            member_ids: vec![7],
            generation: 1,
        },
        GuildMembersSnapshot {
            guild_id: 84,
            member_ids: vec![8],
            generation: 5,
        },
    ];
    assert_eq!(vanity_refresh_plan(snapshots.clone()).snapshots, snapshots);
}

#[test]
fn test_refresh_vanity_tax_memberships_runs_off_the_event_loop() {
    let plan = vanity_refresh_plan(vec![GuildMembersSnapshot {
        guild_id: 42,
        member_ids: vec![7],
        generation: 3,
    }]);
    assert!(plan.run_off_event_loop);
    assert_eq!(plan.snapshots[0].generation, 3);
}

#[test]
fn test_member_events_keep_vanity_tax_membership_current() {
    let mut journal = VanityMembershipJournal::default();
    journal.joined(42, 7, None);
    journal.updated(42, 7, Some("Real Name"));
    journal.removed(42, 7);
    assert_eq!(
        journal.actions,
        [
            VanityMemberAction::Update {
                guild_id: 42,
                member_id: 7,
                nickname: None,
            },
            VanityMemberAction::Update {
                guild_id: 42,
                member_id: 7,
                nickname: Some("Real Name".to_owned()),
            },
            VanityMemberAction::Remove {
                guild_id: 42,
                member_id: 7,
            },
        ]
    );
}

#[test]
fn test_reconcile_persisted_lobby_message_removes_legacy_frogling_reaction() {
    let report = reconcile_lobby_message(&[ReconcileAttempt {
        edit_succeeded: true,
        legacy_reaction_present: true,
        reaction_clear_succeeded: true,
    }]);
    assert!(report.complete);
    assert_eq!(report.edit_attempts, 1);
    assert_eq!(report.reaction_clear_attempts, 1);
    assert_eq!(LEGACY_FROGLING_EMOJI_ID, 1_463_270_458_848_842_003);
}

#[test]
fn test_update_lobby_message_reports_failed_edit() {
    let publisher = SerializedLobbyPublisher::default();
    let updated = publisher.publish(
        LobbyMessageKey {
            guild_id: 42,
            lobby_kind: LobbyKind::Open,
        },
        || false,
    );
    assert!(!updated);
}

#[test]
fn test_update_lobby_message_generation_check() {
    assert!(!lobby_generation_is_current(Some(101), Some(202)));
    assert!(lobby_generation_is_current(Some(101), Some(101)));
    assert!(lobby_generation_is_current(None, Some(202)));
}

#[test]
fn test_update_lobby_message_serializes_same_lobby_refreshes() {
    let publisher = Arc::new(SerializedLobbyPublisher::default());
    let edits = Arc::new(Mutex::new(Vec::new()));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let key = LobbyMessageKey {
        guild_id: 42,
        lobby_kind: LobbyKind::Open,
    };

    let first_publisher = Arc::clone(&publisher);
    let first_edits = Arc::clone(&edits);
    let first = thread::spawn(move || {
        first_publisher.publish(key, || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            first_edits.lock().unwrap().push("old");
            true
        })
    });
    started_rx.recv().unwrap();

    let (second_started_tx, second_started_rx) = mpsc::channel();
    let second_publisher = Arc::clone(&publisher);
    let second_edits = Arc::clone(&edits);
    let second = thread::spawn(move || {
        second_publisher.publish(key, || {
            second_started_tx.send(()).unwrap();
            second_edits.lock().unwrap().push("new");
            true
        })
    });
    assert!(second_started_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    assert!(first.join().unwrap());
    second_started_rx.recv().unwrap();
    assert!(second.join().unwrap());
    assert_eq!(*edits.lock().unwrap(), ["old", "new"]);
}

#[test]
fn test_reconcile_persisted_lobby_message_retries_failed_edit() {
    let report = reconcile_lobby_message(&[
        ReconcileAttempt {
            edit_succeeded: false,
            legacy_reaction_present: false,
            reaction_clear_succeeded: true,
        },
        ReconcileAttempt {
            edit_succeeded: true,
            legacy_reaction_present: false,
            reaction_clear_succeeded: true,
        },
    ]);
    assert_eq!(report.edit_attempts, 2);
    assert_eq!(report.retry_delays, [1]);
    assert!(report.complete);
}

#[test]
fn test_reconcile_persisted_lobby_message_retries_failed_reaction_clear() {
    let report = reconcile_lobby_message(&[
        ReconcileAttempt {
            edit_succeeded: true,
            legacy_reaction_present: true,
            reaction_clear_succeeded: false,
        },
        ReconcileAttempt {
            edit_succeeded: true,
            legacy_reaction_present: true,
            reaction_clear_succeeded: true,
        },
    ]);
    assert_eq!(report.reaction_clear_attempts, 2);
    assert_eq!(report.retry_delays, [1]);
    assert!(report.complete);
}

fn reaction_input(kind: ReactionKind) -> ReactionInput {
    ReactionInput {
        kind,
        message_matches_current_lobby: true,
        lobby_open: true,
        already_in_lobby: false,
        lobby_kind: LobbyKind::Open,
        join_result: JoinResult::Joined,
        post_join_lobby_exists: true,
        lobby_ready: false,
        has_thread: false,
    }
}

#[test]
fn test_retired_frogling_reaction_is_removed_from_active_lobby_message() {
    let plan = route_reaction_add(&reaction_input(ReactionKind::LegacyFrogling));
    assert!(plan.use_partial_message);
    assert!(plan.remove_reaction);
    assert!(!plan.join_lobby);
    assert!(!plan.use_full_message_fetch);
}

#[test]
fn test_sword_reaction_uses_gateway_member_and_partial_message() {
    let mut input = reaction_input(ReactionKind::Sword);
    input.lobby_kind = LobbyKind::LowSkill;
    let plan = route_reaction_add(&input);
    assert_eq!(
        resolve_raw_user_source(true, true, true),
        RawUserSource::PayloadMember
    );
    assert!(plan.use_partial_message);
    assert!(plan.join_lobby);
    assert!(plan.update_lobby_message);
    assert!(plan.sync_readycheck);
    assert!(plan.notify_rally);
    assert!(plan.retain_target_notification);
    assert!(!plan.use_full_message_fetch);
}

#[test]
fn test_sword_join_retains_target_notification_when_lobby_refetch_disappears() {
    let mut input = reaction_input(ReactionKind::Sword);
    input.post_join_lobby_exists = false;
    let plan = route_reaction_add(&input);
    assert!(plan.retain_target_notification);
    assert!(!plan.update_lobby_message);

    let mut tasks = BackgroundTaskRegistry::default();
    tasks.retain(1);
    assert!(tasks.contains(1));
    tasks.finish(1);
    assert!(!tasks.contains(1));
}

#[test]
fn test_sword_reaction_explains_in_flight_lobby_switch() {
    let mut input = reaction_input(ReactionKind::Sword);
    input.join_result = JoinResult::Rejected(JoinRejection::InFlight);
    let plan = route_reaction_add(&input);
    assert!(plan.remove_reaction);
    assert!(
        plan.explanation
            .unwrap()
            .contains("can't switch lobbies while your current shuffle or draft is in progress")
    );
    assert!(!plan.retain_target_notification);
}

#[test]
fn test_jopacoin_reaction_uses_gateway_member_without_message_get() {
    let mut input = reaction_input(ReactionKind::Jopacoin);
    input.has_thread = true;
    let plan = route_reaction_add(&input);
    assert_eq!(
        resolve_raw_user_source(true, false, false),
        RawUserSource::PayloadMember
    );
    assert!(plan.send_gamba_thread_message);
    assert!(!plan.use_partial_message);
    assert!(!plan.use_full_message_fetch);
}

#[test]
fn test_lobby_reaction_wrong_message_returns_before_discord_lookups() {
    let mut input = reaction_input(ReactionKind::Sword);
    input.message_matches_current_lobby = false;
    let plan = route_reaction_add(&input);
    assert!(plan.returned_before_discord_lookups);
    assert!(!plan.join_lobby);
    assert!(!plan.use_partial_message);
}

#[test]
fn test_raw_reaction_user_checks_caches_before_rest() {
    assert_eq!(
        resolve_raw_user_source(false, true, true),
        RawUserSource::GuildMemberCache
    );
    assert_eq!(
        resolve_raw_user_source(false, false, true),
        RawUserSource::UserCache
    );
    assert_eq!(
        resolve_raw_user_source(false, false, false),
        RawUserSource::Rest
    );
}

#[test]
fn test_sword_reaction_remove_uses_partial_message_without_get() {
    let plan = route_sword_reaction_remove(true, true, true);
    assert!(plan.use_partial_message);
    assert!(!plan.use_full_message_fetch);
    assert!(plan.leave_lobby);
    assert!(plan.update_lobby_message);
    assert!(plan.sync_readycheck);
}

fn player(id: i64, recently_joined: bool) -> LiveReadycheckPlayer {
    LiveReadycheckPlayer {
        id,
        recently_joined,
    }
}

#[test]
fn test_readycheck_sync_tracks_live_lobby_and_clears_departed_reaction() {
    let mut state = ReadycheckState::new(500, 200, &[1, 2]);
    state.apply_reaction(500, 2, true);
    let report = sync_readycheck(
        &mut state,
        500,
        500,
        &[player(1, false), player(3, false)],
        10,
        true,
        &BTreeSet::new(),
    );
    assert!(report.state_updated);
    assert!(report.display_updated);
    assert_eq!(state.lobby_ids, BTreeSet::from([1, 3]));
    assert!(state.reacted.is_empty());
    assert_eq!(report.physical_reactions_to_clear, BTreeSet::from([2, 3]));
}

fn ten_player_readycheck() -> (ReadycheckState, Vec<LiveReadycheckPlayer>) {
    let mut state = ReadycheckState::new(500, 200, &(1..=9).collect::<Vec<_>>());
    for id in 1..=9 {
        state.apply_reaction(500, id, true);
    }
    let mut players = (1..=9).map(|id| player(id, false)).collect::<Vec<_>>();
    players.push(player(10, true));
    (state, players)
}

#[test]
fn test_recent_tenth_signup_completes_active_readycheck() {
    let (mut state, players) = ten_player_readycheck();
    let report = sync_readycheck(&mut state, 500, 500, &players, 10, true, &BTreeSet::new());
    assert!(report.display_updated);
    assert!(report.completion_reached);
    assert_eq!(state.lobby_ids, (1..=10).collect());
    assert_eq!(state.reacted.len(), 10);
    assert!(report.physical_reactions_to_clear.is_empty());
}

#[test]
fn test_recent_tenth_signup_notifies_when_readycheck_edit_fails() {
    let (mut state, players) = ten_player_readycheck();
    let report = sync_readycheck(&mut state, 500, 500, &players, 10, false, &BTreeSet::new());
    assert!(report.state_updated);
    assert!(!report.display_updated);
    assert!(report.completion_reached);
    assert_eq!(state.reacted.len(), 10);
}

#[test]
fn test_readycheck_sync_aborts_when_generation_is_replaced() {
    let mut state = ReadycheckState::new(500, 200, &[1]);
    let before = state.clone();
    let report = sync_readycheck(
        &mut state,
        500,
        600,
        &[player(1, false)],
        10,
        true,
        &BTreeSet::new(),
    );
    assert_eq!(report, ReadycheckSyncReport::default());
    assert_eq!(state, before);
}

#[test]
fn test_readycheck_sync_tolerates_departed_reaction_cleanup_failure() {
    let mut state = ReadycheckState::new(500, 200, &[1, 2]);
    state.apply_reaction(500, 2, true);
    let report = sync_readycheck(
        &mut state,
        500,
        500,
        &[player(1, false)],
        10,
        true,
        &BTreeSet::from([2]),
    );
    assert!(report.state_updated);
    assert!(report.display_updated);
    assert!(state.reacted.is_empty());
    assert_eq!(report.cleanup_failures_ignored, BTreeSet::from([2]));
}

#[test]
fn test_readycheck_sync_contains_message_edit_failure() {
    let mut state = ReadycheckState::new(500, 200, &[1]);
    let report = sync_readycheck(
        &mut state,
        500,
        500,
        &[player(1, false)],
        10,
        false,
        &BTreeSet::new(),
    );
    assert!(report.state_updated);
    assert!(!report.display_updated);
}

fn shortcut_result(status_ok: bool) -> ShortcutResult {
    ShortcutResult {
        status_ok,
        mention_ids: vec![2, 3],
        jump_url: "https://discord.com/channels/42/400/500".to_owned(),
        readycheck_channel_id: 400,
        readycheck_message_id: 500,
    }
}

#[test]
fn test_failed_readycheck_shortcut_removes_reaction_without_message_fetch() {
    let plan = readycheck_shortcut_plan(&shortcut_result(false), LobbyKind::Open, Some(300), 500);
    assert!(plan.use_partial_message_to_remove_failed_reaction);
    assert!(!plan.advertise_origin);
}

#[test]
fn test_successful_readycheck_shortcut_advertises_in_origin_channel() {
    let plan = readycheck_shortcut_plan(&shortcut_result(true), LobbyKind::Open, Some(300), 500);
    assert!(plan.advertise_origin);
    assert_eq!(
        plan.content.as_deref(),
        Some(
            "⚔️ **🍽️ All You Can Feed ready check!** <@2> <@3>\n\
             [React in the lobby thread](https://discord.com/channels/42/400/500)"
        )
    );
    assert_eq!(plan.allowed_mention_ids, [2, 3]);
}

#[test]
fn test_readycheck_shortcut_suppresses_invalid_origin_advertisement_generation_changes_during_resolution()
 {
    let plan = readycheck_shortcut_plan(&shortcut_result(true), LobbyKind::Open, Some(300), 501);
    assert!(!plan.advertise_origin);
    assert!(plan.content.is_none());
}

#[test]
fn test_readycheck_shortcut_suppresses_invalid_origin_advertisement_origin_is_readycheck_channel() {
    let plan = readycheck_shortcut_plan(&shortcut_result(true), LobbyKind::Open, Some(400), 500);
    assert!(!plan.advertise_origin);
    assert!(plan.content.is_none());
}

fn ready_snapshot(count: usize, message_id: i64) -> LobbyReadySnapshot {
    LobbyReadySnapshot {
        open: true,
        player_count: count,
        message_id: Some(message_id),
    }
}

#[test]
fn test_lobby_ready_notification_recommends_readycheck_before_shuffle() {
    let gate = LobbyReadyGate::default();
    assert!(gate.try_claim(
        42,
        LobbyKind::Open,
        10,
        ready_snapshot(10, 123),
        ready_snapshot(10, 123),
    ));
    assert_eq!(
        lobby_ready_next_step(),
        "Run `/readycheck` before `/shuffle` to confirm everyone is ready."
    );
}

#[test]
fn test_lobby_ready_notification_skips_lobbies_above_threshold() {
    let gate = LobbyReadyGate::default();
    assert!(!gate.try_claim(
        42,
        LobbyKind::Open,
        10,
        ready_snapshot(11, 123),
        ready_snapshot(11, 123),
    ));
}

#[test]
fn test_lobby_ready_notification_skips_replaced_lobby_generation() {
    let gate = LobbyReadyGate::default();
    assert!(!gate.try_claim(
        42,
        LobbyKind::Open,
        10,
        ready_snapshot(10, 123),
        ready_snapshot(10, 999),
    ));
}

#[test]
fn test_lobby_ready_notification_skips_if_lobby_grows_during_delivery() {
    let gate = LobbyReadyGate::default();
    assert!(!gate.try_claim(
        42,
        LobbyKind::Open,
        10,
        ready_snapshot(10, 123),
        ready_snapshot(11, 123),
    ));
}

#[test]
fn test_concurrent_lobby_ready_notifications_send_once() {
    let gate = LobbyReadyGate::default();
    let initial = ready_snapshot(10, 123);
    assert!(gate.try_claim(42, LobbyKind::Open, 10, initial, initial));
    assert!(!gate.try_claim(42, LobbyKind::Open, 10, initial, initial));
}

fn channels(origin: bool, fallback: bool) -> CompletionChannels {
    CompletionChannels {
        origin_lookup_succeeded: true,
        origin_available: origin,
        fallback_available: fallback,
    }
}

fn completion_attempt(
    reacted_count_before_resolution: Option<usize>,
    reacted_count_before_send: Option<usize>,
    channels: CompletionChannels,
    send_succeeds: bool,
) -> CompletionAttempt {
    CompletionAttempt {
        guild_id: 42,
        lobby_kind: LobbyKind::Open,
        readycheck_message_id: 500,
        reacted_count_before_resolution,
        reacted_count_before_send,
        ready_threshold: 10,
        channels,
        send_succeeds,
    }
}

#[test]
fn test_tenth_readycheck_confirmation_recommends_shuffle_once_in_origin_channel() {
    let mut registry = ReadycheckCompletionRegistry::default();
    assert_eq!(
        registry.deliver(completion_attempt(
            Some(9),
            Some(9),
            channels(true, true),
            true,
        )),
        None
    );
    assert_eq!(
        registry.deliver(completion_attempt(
            Some(10),
            Some(10),
            channels(true, true),
            true,
        )),
        Some(CompletionTarget::Origin)
    );
    assert_eq!(
        registry.deliver(completion_attempt(
            Some(10),
            Some(10),
            channels(true, true),
            true,
        )),
        None
    );
    assert_eq!(READYCHECK_COMPLETE_MESSAGE.matches("/shuffle").count(), 1);
}

#[test]
fn test_readycheck_completion_falls_back_when_origin_channel_fetch_fails() {
    let mut registry = ReadycheckCompletionRegistry::default();
    assert_eq!(
        registry.deliver(completion_attempt(None, None, channels(false, true), true)),
        Some(CompletionTarget::Fallback)
    );
}

#[test]
fn test_readycheck_completion_falls_back_when_origin_lookup_fails() {
    let mut registry = ReadycheckCompletionRegistry::default();
    let unavailable_origin = CompletionChannels {
        origin_lookup_succeeded: false,
        origin_available: false,
        fallback_available: true,
    };
    assert_eq!(
        registry.deliver(completion_attempt(None, None, unavailable_origin, true)),
        Some(CompletionTarget::Fallback)
    );
}

#[test]
fn test_readycheck_completion_uses_origin_when_source_channel_fetch_fails() {
    let mut registry = ReadycheckCompletionRegistry::default();
    assert_eq!(
        registry.deliver(completion_attempt(
            Some(10),
            Some(10),
            channels(true, false),
            true,
        )),
        Some(CompletionTarget::Origin)
    );
}

#[test]
fn test_replaced_readycheck_is_revalidated_before_completion_send() {
    let mut registry = ReadycheckCompletionRegistry::default();
    assert_eq!(
        registry.deliver(completion_attempt(
            Some(10),
            None,
            channels(true, true),
            true,
        )),
        None
    );
}

#[test]
fn test_reaction_from_replaced_readycheck_does_not_confirm_new_check() {
    let mut state = ReadycheckState::new(600, 200, &[9]);
    assert!(!state.apply_reaction(500, 9, true));
    assert!(state.reacted.is_empty());
}

#[test]
fn test_removal_from_replaced_readycheck_does_not_unconfirm_new_check() {
    let mut state = ReadycheckState::new(600, 200, &[9]);
    state.apply_reaction(600, 9, true);
    assert!(!state.apply_reaction(500, 9, false));
    assert_eq!(state.reacted, BTreeMap::from([(9, "<@9>".to_owned())]));
}

#[test]
fn test_concurrent_readycheck_completion_retries_after_failed_delivery() {
    let mut registry = ReadycheckCompletionRegistry::default();
    let first = registry.deliver(completion_attempt(None, None, channels(false, true), false));
    let second = registry.deliver(completion_attempt(None, None, channels(false, true), true));
    assert_eq!((first, second), (None, Some(CompletionTarget::Fallback)));
}

#[test]
fn test_economy_event_loop_does_not_pause_disbursement_voting() {
    let action = economy_wake_action(None, false);
    assert!(!action.alter_disbursement_ballot);
    assert_eq!(economy_wake_delay(3_600, 900.0), 900.0);
}

#[test]
fn test_economy_event_loop_preserves_ballot_for_severe_deflationary_edict() {
    let mut ballot = DisbursementBallot {
        proposal_id: 1,
        reserve_jc: 300,
        votes: BTreeMap::new(),
    };
    assert!(ballot.add_vote("even", false));
    ballot.apply_severe_deflationary_restriction();
    assert!(!ballot.add_vote("stimulus", true));
    assert_eq!(ballot.proposal_id, 1);
    assert_eq!(ballot.reserve_jc, 0);
    assert_eq!(ballot.votes.get("even"), Some(&1));
    assert!(ballot.add_vote("stimulus", false));
    assert_eq!(ballot.total_votes(), 2);
}

#[test]
fn test_economy_event_loop_wakes_at_interval_or_trigger_whichever_is_first_3600_75_75() {
    assert_eq!(economy_wake_delay(3_600, 75.0), 75.0);
}

#[test]
fn test_economy_event_loop_wakes_at_interval_or_trigger_whichever_is_first_60_3600_60() {
    assert_eq!(economy_wake_delay(60, 3_600.0), 60.0);
}

#[test]
fn test_economy_event_loop_posts_and_stamps_second_daily_announcement() {
    let action = economy_wake_action(Some(AnnouncementSlot::Reminder), true);
    assert!(action.announce);
    assert!(action.stamp_reminder);
    assert!(!action.stamp_initial);
}

fn ravage_event() -> EconomyEvent {
    EconomyEvent {
        name: Some("Ravage".to_owned()),
        severity: 3,
        direction: "deflationary".to_owned(),
        announcement: Some("A tidal shock tears through the Jopacoin economy.".to_owned()),
        ends_at: Some(1_752_943_600),
        effects: EconomyEventEffects {
            reward_multiplier: 0.76,
            reserve_burn_jc: 300,
            ..EconomyEventEffects::default()
        },
    }
}

#[test]
fn test_economy_event_announcement_uses_shared_public_embed() {
    let embed =
        build_public_economy_event_embed(&ravage_event(), Some("https://cdn.example/ravage.png"));
    assert_eq!(embed.title, "🌑 Ravage — Level III");
    assert!(embed.fields[0].1.contains("24% lower"));
    assert!(embed.fields[1].1.contains("300 JC"));
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some("https://cdn.example/ravage.png")
    );
    assert_eq!(
        embed.footer,
        "The treasury watches. Bands follow the rolling 7-day change around a 2% annual target."
    );
}

#[test]
fn test_economy_event_announcement_logs_when_prediction_cog_missing() {
    let plan = economy_announcement_plan(42, &ravage_event(), false, Ok(None));
    assert!(!plan.sent);
    assert!(
        plan.warning
            .unwrap()
            .contains("PredictionCommands cog not loaded")
    );
}

#[test]
fn test_economy_event_announcement_survives_icon_lookup_failure() {
    let plan = economy_announcement_plan(
        42,
        &ravage_event(),
        true,
        Err("dotabase unavailable".to_owned()),
    );
    assert!(plan.sent);
    assert!(plan.embed.unwrap().thumbnail_url.is_none());
    assert!(
        plan.warning
            .unwrap()
            .contains("economy event icon lookup failed for event=Ravage guild=42")
    );
}

#[test]
fn test_reminder_recovery_is_single_flight_and_runs_after_success() {
    let mut recovery = ReminderRecovery::default();
    let first = recovery.start(&[11, 22]);
    let overlapping = recovery.start(&[99]);
    assert!(first.newly_started);
    assert!(!overlapping.newly_started);
    assert_eq!(overlapping.token, first.token);
    assert_eq!(overlapping.guild_ids, [11, 22]);
    recovery.finish(first.token, None);
    let second = recovery.start(&[44]);
    assert!(second.newly_started);
    assert_ne!(second.token, first.token);
    assert_eq!(second.guild_ids, [44]);
}

#[test]
fn test_reminder_recovery_failure_is_logged_and_retryable() {
    let mut recovery = ReminderRecovery::default();
    let failed = recovery.start(&[7]);
    recovery.finish(failed.token, Some("reminder unavailable"));
    assert!(recovery.current_token().is_none());
    assert!(recovery.warnings[0].contains("Reminder recovery sweep failed"));
    let retried = recovery.start(&[7]);
    assert_ne!(retried.token, failed.token);
}

#[test]
fn test_stale_recovery_callbacks_do_not_clear_newer_handles() {
    let mut recovery = ReminderRecovery::default();
    let old = recovery.start(&[7]);
    recovery.finish(old.token, None);
    let newer = recovery.start(&[8]);
    recovery.finish(old.token, None);
    assert_eq!(recovery.current_token(), Some(newer.token));
}

fn utc(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 22, hour, minute, 0).unwrap()
}

#[test]
fn test_next_digest_run_schedules_strictly_future_anchor() {
    assert_eq!(next_digest_run(utc(6, 0), &[0, 12]), Some(utc(12, 0)));
    assert_eq!(
        next_digest_run(utc(13, 0), &[0, 12]),
        Some(utc(0, 0) + Duration::days(1))
    );
    let next = next_digest_run(utc(0, 1), &[0, 12]).unwrap();
    assert_eq!(next, utc(12, 0));
    assert_eq!(next - utc(0, 0), Duration::hours(12));
    assert_eq!(
        next_digest_run(utc(12, 0), &[0, 12]),
        Some(utc(0, 0) + Duration::days(1))
    );
}

#[test]
fn test_post_digest_charts_biggest_mover_and_attaches_file() {
    let markets = [
        DigestMarket {
            prediction_id: 1,
            current_price: 52,
            previous_price: 50,
            recent_volume: 9,
        },
        DigestMarket {
            prediction_id: 2,
            current_price: 30,
            previous_price: 38,
            recent_volume: 1,
        },
    ];
    let plan = build_digest_plan(&markets, Some("predict_2.png"));
    assert_eq!(plan.field_order, [1, 2]);
    assert_eq!(plan.chart_market_id, Some(2));
    assert_eq!(plan.attachment_filename.as_deref(), Some("predict_2.png"));
    assert_eq!(
        plan.image_url.as_deref(),
        Some("attachment://predict_2.png")
    );
    assert!(plan.description.unwrap().contains("Biggest mover:** #2 ↓8"));
}

#[test]
fn test_post_digest_no_chart_when_nothing_moved() {
    let markets = [
        DigestMarket {
            prediction_id: 1,
            current_price: 50,
            previous_price: 50,
            recent_volume: 1,
        },
        DigestMarket {
            prediction_id: 2,
            current_price: 17,
            previous_price: 17,
            recent_volume: 1,
        },
    ];
    let plan = build_digest_plan(&markets, Some("predict.png"));
    assert_eq!(plan.chart_market_id, None);
    assert_eq!(plan.attachment_filename, None);
    assert_eq!(plan.image_url, None);
}

fn refresh_summary() -> RefreshSummary {
    RefreshSummary {
        skipped: false,
        old_price: 50,
        new_price: 55,
        trade_count: 2,
        total_volume: 7,
        yes_volume: 4,
        no_volume: 3,
    }
}

#[test]
fn test_process_one_refresh_revives_thread_for_daily_summary() {
    let actions = process_refresh_plan(&refresh_summary(), Some(999), Some("open"));
    assert_eq!(actions[0], RefreshAction::RefreshMarketEmbed);
    assert_eq!(
        actions[1],
        RefreshAction::UnarchiveThread {
            auto_archive_minutes: 10_080,
        }
    );
    let RefreshAction::SendDailySummary(message) = &actions[2] else {
        panic!("summary must follow unarchive");
    };
    assert!(message.contains("Daily refresh"));
}

#[test]
fn test_process_one_refresh_skips_summary_when_market_no_longer_open() {
    assert_eq!(
        process_refresh_plan(&refresh_summary(), Some(999), Some("resolved")),
        [RefreshAction::RefreshMarketEmbed]
    );
}
