use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use crate::embeds::{LobbyEmbedRequest, create_lobby_embed};

use super::*;

const GUILD: GuildId = GuildId(42);
const OTHER_GUILD: GuildId = GuildId(-42);
const MESSAGE: MessageId = MessageId(111);
const CHANNEL: ChannelId = ChannelId(222);

fn scope() -> LobbyScope {
    LobbyScope::new(GUILD, LobbyKind::Open)
}

fn player(value: i64) -> UserId {
    UserId(value)
}

fn lobby_with(players: impl IntoIterator<Item = i64>) -> ReadyLobby {
    let mut lobby = ReadyLobby::new(scope(), player(0), "2026-07-14T12:34:56");
    for (index, player_id) in players.into_iter().enumerate() {
        assert!(lobby.add_player_at(player(player_id), 100.0 + index as f64));
    }
    lobby
}

fn data(ids: impl IntoIterator<Item = i64>) -> BTreeMap<UserId, ReadycheckPlayerData> {
    ids.into_iter()
        .map(|id| {
            (
                player(id),
                ReadycheckPlayerData::named(format!("Player {id}")),
            )
        })
        .collect()
}

fn set_generation(
    service: &ReadycheckService,
    ids: impl IntoIterator<Item = i64>,
    created_at: f64,
) {
    let ids = ids.into_iter().map(player).collect::<BTreeSet<_>>();
    let player_data = ids
        .iter()
        .map(|id| (*id, ReadycheckPlayerData::named(format!("Player {}", id.0))))
        .collect();
    service.set_readycheck_state(
        scope(),
        ReadycheckStateInput {
            message_id: MESSAGE,
            channel_id: CHANNEL,
            lobby_ids: ids,
            player_data,
            created_at: Some(created_at),
            initial_reacted: BTreeMap::new(),
        },
    );
}

#[test]
fn test_add_player_sets_join_time() {
    let mut lobby = lobby_with([]);

    assert!(lobby.add_player_at(player(100), 1_768_307_696.123_456));

    assert_eq!(
        lobby.player_join_times.get(&player(100)),
        Some(&1_768_307_696.123_456)
    );
}

#[test]
fn test_regular_join_clears_legacy_conditional_state() {
    let mut lobby = lobby_with([]);
    lobby.legacy_conditional_players.insert(player(100));
    lobby.player_join_times.insert(player(100), 1_000_000.0);

    assert!(lobby.add_player_at(player(100), 2_000_000.0));

    assert_eq!(lobby.player_join_times[&player(100)], 1_000_000.0);
    assert!(lobby.players.contains(&player(100)));
    assert!(!lobby.legacy_conditional_players.contains(&player(100)));
}

#[test]
fn test_conditional_join_is_rejected_without_moving_regular_player() {
    let mut lobby = lobby_with([100]);
    lobby.player_join_times.insert(player(100), 1_000_000.0);

    assert!(!lobby.add_conditional_player(player(100)));

    assert_eq!(lobby.player_join_times[&player(100)], 1_000_000.0);
    assert!(!lobby.legacy_conditional_players.contains(&player(100)));
    assert!(lobby.players.contains(&player(100)));
}

#[test]
fn test_remove_player_clears_join_time() {
    let mut lobby = lobby_with([100]);
    assert!(lobby.player_join_times.contains_key(&player(100)));

    assert!(lobby.remove_player(player(100)));

    assert!(!lobby.player_join_times.contains_key(&player(100)));
}

#[test]
fn test_remove_legacy_conditional_player_clears_join_time() {
    let mut lobby = lobby_with([]);
    lobby.legacy_conditional_players.insert(player(200));
    lobby.player_join_times.insert(player(200), 1_000_000.0);

    assert!(lobby.remove_conditional_player(player(200)));

    assert!(!lobby.player_join_times.contains_key(&player(200)));
}

#[test]
fn test_remove_nonexistent_player_no_error() {
    let mut lobby = lobby_with([]);

    assert!(!lobby.remove_player(player(999)));
    assert!(!lobby.player_join_times.contains_key(&player(999)));
}

#[test]
fn test_to_dict_includes_join_times() {
    let lobby = lobby_with([100]);

    let document = lobby.to_document();

    assert!(document.player_join_times.contains_key("100"));
}

#[test]
fn test_roundtrip_preserves_join_times() {
    let lobby = lobby_with([100]);

    let restored = ReadyLobby::from_document(lobby.to_document());

    assert_eq!(
        restored.player_join_times[&player(100)],
        lobby.player_join_times[&player(100)]
    );
}

#[test]
fn test_from_dict_missing_join_times_defaults_empty() {
    let document = LobbyDocument {
        lobby_id: 1,
        guild_id: 0,
        kind: LobbyKind::Open,
        created_by: 0,
        created_at: "2026-07-14T12:34:56".to_owned(),
        players: vec![100, 200],
        conditional_players: vec![300],
        player_join_times: BTreeMap::new(),
        status: LobbyStatus::Open,
    };

    let lobby = ReadyLobby::from_document(document);

    assert!(lobby.player_join_times.is_empty());
}

#[test]
fn test_playing_dota_game() {
    assert!(is_playing_dota(&[MemberActivity::game("Dota 2")]));
}

#[test]
fn test_playing_dota_case_insensitive() {
    assert!(is_playing_dota(&[MemberActivity::game("dota 2")]));
}

#[test]
fn test_not_playing_dota() {
    assert!(!is_playing_dota(&[MemberActivity::game(
        "Counter-Strike 2"
    )]));
}

#[test]
fn test_no_activities() {
    assert!(!is_playing_dota(&[]));
}

#[test]
fn test_activity_with_dota_substring() {
    assert!(is_playing_dota(&[MemberActivity {
        kind: ActivityKind::Other,
        name: Some("Playing Dota 2 with friends".to_owned()),
    }]));
}

#[test]
fn test_lobby_not_ready_under_10() {
    let lobby = lobby_with(0..9);

    assert!(lobby.total_count() < FULL_LOBBY_SIZE);
    assert!(!lobby.is_ready(FULL_LOBBY_SIZE));
}

#[test]
fn test_lobby_ready_at_10() {
    let lobby = lobby_with(0..10);

    assert!(lobby.total_count() >= FULL_LOBBY_SIZE);
    assert!(lobby.is_ready(FULL_LOBBY_SIZE));
}

#[test]
fn test_lobby_ignores_legacy_conditional_players() {
    let mut lobby = lobby_with(0..7);
    lobby.legacy_conditional_players = (7..10).map(player).collect();

    assert_eq!(lobby.total_count(), 7);
    assert!(!lobby.is_ready(FULL_LOBBY_SIZE));
}

#[test]
fn test_ready_lobby_embed_recommends_readycheck_before_shuffle() {
    let embed = create_lobby_embed(
        LobbyEmbedRequest {
            regular_count: 10,
            ready_threshold: 10,
            guild_id: Some(42),
            ..LobbyEmbedRequest::default()
        },
        &[],
        &[],
        None,
    );

    let ready_field = embed
        .fields
        .iter()
        .find(|field| field.name == "✅ Ready!")
        .expect("ready field");
    assert_eq!(ready_field.value, READY_LOBBY_RECOMMENDATION);
}

#[test]
fn test_set_readycheck_state_records_created_at() {
    let service = ReadycheckService::new();

    set_generation(&service, [1, 2], 1_000.0);

    assert_eq!(service.readycheck_created_at(scope()), Some(1_000.0));
}

#[test]
fn test_set_readycheck_state_defaults_created_at_to_now() {
    let service = ReadycheckService::new();
    let before = unix_time_now();
    service.set_readycheck_state(
        scope(),
        ReadycheckStateInput {
            message_id: MESSAGE,
            channel_id: CHANNEL,
            lobby_ids: BTreeSet::from([player(1)]),
            player_data: data([1]),
            created_at: None,
            initial_reacted: BTreeMap::new(),
        },
    );
    let after = unix_time_now();

    let timestamp = service
        .readycheck_created_at(scope())
        .expect("created timestamp");
    assert!((before..=after).contains(&timestamp));
}

#[test]
fn test_get_readycheck_created_at_defaults_none() {
    assert_eq!(
        ReadycheckService::new().readycheck_created_at(scope()),
        None
    );
}

#[test]
fn test_reset_lobby_clears_created_at() {
    let service = ReadycheckService::new();
    service.put_lobby(lobby_with([1]));
    set_generation(&service, [1], 1_000.0);

    service.reset_lobby(scope());

    assert_eq!(service.readycheck_created_at(scope()), None);
}

#[test]
fn test_snapshot_identifies_current_generation_and_copies_confirmations() {
    let service = ReadycheckService::new();
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));

    let snapshot = service
        .confirmation_snapshot(scope())
        .expect("current readycheck");
    assert_eq!(snapshot.message_id, MESSAGE);
    assert_eq!(snapshot.confirmed_ids, BTreeSet::from([player(1)]));

    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    service.set_readycheck_state(
        scope(),
        ReadycheckStateInput {
            message_id: MessageId(333),
            channel_id: CHANNEL,
            lobby_ids: BTreeSet::from([player(1), player(2)]),
            player_data: data([1, 2]),
            created_at: Some(2_000.0),
            initial_reacted: BTreeMap::new(),
        },
    );

    assert_eq!(snapshot.confirmed_ids, BTreeSet::from([player(1)]));
    assert_eq!(
        service.confirmation_snapshot(scope()),
        Some(ConfirmationSnapshot {
            message_id: MessageId(333),
            confirmed_ids: BTreeSet::new(),
        })
    );
}

#[test]
fn test_snapshot_is_none_without_a_current_readycheck() {
    assert_eq!(
        ReadycheckService::new().confirmation_snapshot(scope()),
        None
    );
}

#[test]
fn test_player_who_joins_after_readycheck_can_confirm() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);

    assert!(service.join_lobby_at(scope(), player(2), 200.0));

    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", Some(MESSAGE),));
    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(2), "<@2>".to_owned())])
    );
}

#[test]
fn test_player_who_left_after_readycheck_cannot_confirm() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    assert!(service.join_lobby_at(scope(), player(2), 200.0));
    set_generation(&service, [1, 2], 1_000.0);

    assert!(service.leave_lobby(scope(), player(2)));

    assert!(!service.add_readycheck_reaction(scope(), player(2), "<@2>", Some(MESSAGE),));
    assert!(service.readycheck_reacted(scope()).is_empty());
}

#[test]
fn test_roster_update_returns_departed_confirmations() {
    let service = ReadycheckService::new();
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));

    let update = service
        .update_readycheck_data(scope(), BTreeSet::from([player(1)]), data([1]), MESSAGE)
        .expect("current generation update");

    assert_eq!(update.removed_confirmations, BTreeSet::from([player(2)]));
    assert_eq!(
        service.readycheck_lobby_ids(scope()),
        BTreeSet::from([player(1)])
    );
    assert!(service.readycheck_reacted(scope()).is_empty());
}

#[test]
fn test_replaced_readycheck_rejects_old_roster_update() {
    let service = ReadycheckService::new();
    service.set_readycheck_state(
        scope(),
        ReadycheckStateInput {
            message_id: MessageId(222),
            channel_id: ChannelId(333),
            lobby_ids: BTreeSet::from([player(7)]),
            player_data: data([7]),
            created_at: Some(1_000.0),
            initial_reacted: BTreeMap::new(),
        },
    );

    let update =
        service.update_readycheck_data(scope(), BTreeSet::from([player(1)]), data([1]), MESSAGE);

    assert_eq!(update, None);
    assert_eq!(
        service.readycheck_lobby_ids(scope()),
        BTreeSet::from([player(7)])
    );
    assert_eq!(service.readycheck_player_data(scope()), data([7]));
}

#[test]
fn test_snapshot_copies_current_lobby_and_confirmations_together() {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with([1, 2]);
    lobby.player_join_times = BTreeMap::from([(player(1), 100.5), (player(2), 200.5)]);
    service.put_lobby(lobby);
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));

    let snapshot = service
        .lobby_readycheck_snapshot(scope())
        .expect("open lobby snapshot");

    assert_eq!(
        snapshot.player_ids.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([player(1), player(2)])
    );
    assert_eq!(
        snapshot.player_join_times,
        BTreeMap::from([(player(1), 100.5), (player(2), 200.5)])
    );
    assert_eq!(
        snapshot.readycheck,
        Some(ConfirmationSnapshot {
            message_id: MESSAGE,
            confirmed_ids: BTreeSet::from([player(1)]),
        })
    );

    assert!(service.join_lobby_at(scope(), player(3), 300.5));
    let mut changed = service.lobby(scope()).expect("lobby");
    changed.player_join_times.insert(player(1), 999.5);
    service.put_lobby(changed);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));

    // All snapshot fields are owned copies, unaffected by later mutations.
    assert_eq!(
        snapshot.player_join_times,
        BTreeMap::from([(player(1), 100.5), (player(2), 200.5)])
    );
    assert_eq!(
        snapshot.readycheck.expect("readycheck").confirmed_ids,
        BTreeSet::from([player(1)])
    );
}

#[test]
fn test_lobby_mutation_cannot_interleave_with_snapshot() {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with(1..=10);
    lobby.player_join_times = (1..=10).map(|id| (player(id), id as f64)).collect();
    service.put_lobby(lobby);
    set_generation(&service, 1..=10, 1_000.0);
    for id in 1..=10 {
        assert!(service.add_readycheck_reaction(scope(), player(id), format!("<@{id}>"), None,));
    }

    let (iteration_started_tx, iteration_started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let snapshot_service = service.clone();
    let snapshot_thread = thread::spawn(move || {
        snapshot_service.lobby_readycheck_snapshot_with_test_hook(scope(), || {
            iteration_started_tx
                .send(())
                .expect("announce locked snapshot");
            release_rx.recv().expect("release locked snapshot");
        })
    });
    iteration_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("snapshot acquired service lock");

    let (leave_started_tx, leave_started_rx) = mpsc::channel();
    let (leave_done_tx, leave_done_rx) = mpsc::channel();
    let leave_service = service.clone();
    let leave_thread = thread::spawn(move || {
        leave_started_tx.send(()).expect("announce leave attempt");
        leave_done_tx
            .send(leave_service.leave_lobby(scope(), player(10)))
            .expect("return leave result");
    });
    leave_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("leave attempted");
    assert!(
        leave_done_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );

    release_tx.send(()).expect("release snapshot");
    let snapshot = snapshot_thread
        .join()
        .expect("snapshot thread")
        .expect("snapshot");
    assert!(
        leave_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leave completes after snapshot")
    );
    leave_thread.join().expect("leave thread");

    assert_eq!(
        snapshot.player_ids.into_iter().collect::<BTreeSet<_>>(),
        (1..=10).map(player).collect()
    );
    assert_eq!(
        snapshot.readycheck.expect("readycheck").confirmed_ids,
        (1..=10).map(player).collect()
    );
}

#[test]
fn test_remove_reaction_marks_player_declined() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));

    assert!(service.remove_readycheck_reaction(scope(), player(1), None));

    assert_eq!(
        service.readycheck_declined(scope()),
        BTreeSet::from([player(1)])
    );
}

#[test]
fn test_removing_absent_reaction_does_not_mark_declined() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);

    assert!(!service.remove_readycheck_reaction(scope(), player(1), None));
    assert!(service.readycheck_declined(scope()).is_empty());
}

#[test]
fn test_confirming_again_clears_declined_marker() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));
    assert!(service.remove_readycheck_reaction(scope(), player(1), None));

    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));

    assert!(service.readycheck_declined(scope()).is_empty());
}

#[test]
fn test_new_readycheck_generation_resets_declined() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));
    assert!(service.remove_readycheck_reaction(scope(), player(1), None));

    service.set_readycheck_state(
        scope(),
        ReadycheckStateInput {
            message_id: MessageId(333),
            channel_id: CHANNEL,
            lobby_ids: BTreeSet::from([player(1)]),
            player_data: data([1]),
            created_at: Some(2_000.0),
            initial_reacted: BTreeMap::new(),
        },
    );

    assert!(service.readycheck_declined(scope()).is_empty());
}

#[test]
fn test_departed_player_declined_marker_is_pruned() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    assert!(service.join_lobby_at(scope(), player(2), 200.0));
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    assert!(service.remove_readycheck_reaction(scope(), player(2), None));

    service
        .update_readycheck_data(scope(), BTreeSet::from([player(1)]), data([1]), MESSAGE)
        .expect("current generation");

    assert!(service.readycheck_declined(scope()).is_empty());
}

#[test]
fn test_reset_lobby_clears_declined() {
    let service = ReadycheckService::new();
    assert!(service.join_lobby_at(scope(), player(1), 100.0));
    set_generation(&service, [1], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));
    assert!(service.remove_readycheck_reaction(scope(), player(1), None));

    service.reset_lobby(scope());

    assert!(service.readycheck_declined(scope()).is_empty());
}

fn command_ready_service() -> ReadycheckService {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with([1, 2]);
    lobby.thread_id = Some(ChannelId(999));
    service.put_lobby(lobby);
    service
}

fn request(now: f64) -> ReadycheckCommandRequest {
    ReadycheckCommandRequest {
        guild_id: Some(GUILD),
        kind: LobbyKind::Open,
        invoker_id: player(1),
        invocation_channel_id: ChannelId(555),
        now,
        confirm_invoker: true,
        existing_message: ExistingMessageState::Unknown,
        reserved_players: BTreeSet::new(),
        minimum_players: 0,
    }
}

#[test]
fn command_below_minimum_player_count_is_rejected() {
    let service = command_ready_service();
    let mut too_small = request(10_000.0);
    too_small.minimum_players = MINIMUM_READYCHECK_PLAYERS;

    let failure = service
        .prepare_command(too_small, data([1, 2]))
        .expect_err("a 2-player lobby must not clear the minimum floor");

    assert_eq!(
        failure,
        ReadycheckCommandFailure::TooFewPlayers {
            minimum: MINIMUM_READYCHECK_PLAYERS,
            current: 2,
        }
    );
}

#[test]
fn command_at_minimum_player_count_is_allowed() {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with(1..=MINIMUM_READYCHECK_PLAYERS as i64);
    lobby.thread_id = Some(ChannelId(999));
    service.put_lobby(lobby);
    let mut at_minimum = request(10_000.0);
    at_minimum.minimum_players = MINIMUM_READYCHECK_PLAYERS;

    let plan = service
        .prepare_command(at_minimum, data(1..=MINIMUM_READYCHECK_PLAYERS as i64))
        .expect("a lobby at the minimum floor clears it");

    assert_eq!(plan.mode, PublicationMode::New);
}

#[test]
fn stronger_concurrent_publication_reservation_is_atomic() {
    let service = command_ready_service();
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let service = service.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            service.prepare_command(request(10_000.0), data([1, 2]))
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("command thread"))
        .collect::<Vec<_>>();

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                matches!(result, Err(ReadycheckCommandFailure::PublicationInFlight))
            })
            .count(),
        1
    );
}

#[test]
fn stronger_stale_recovery_prunes_only_safe_afk_players_and_orders_transport() {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with(1..=6);
    lobby.thread_id = Some(ChannelId(999));
    lobby.player_join_times = BTreeMap::from([
        (player(1), 0.0),
        (player(2), 0.0),
        (player(3), 9_500.0),
        (player(4), 0.0),
        (player(5), 0.0),
        (player(6), 0.0),
    ]);
    service.put_lobby(lobby);
    set_generation(&service, 1..=6, 0.0);
    assert!(service.add_readycheck_reaction(scope(), player(4), "<@4>", None));
    let mut classifications = data(1..=6);
    classifications.get_mut(&player(6)).expect("player 6").group = ReadinessGroup::PlayingDota;
    let mut stale_request = request(10_000.0);
    stale_request.existing_message = ExistingMessageState::Available;
    stale_request.reserved_players.insert(player(5));

    let plan = service
        .prepare_command(stale_request, classifications)
        .expect("stale replacement plan");

    assert_eq!(plan.mode, PublicationMode::ReplaceStale);
    assert_eq!(plan.pruned_players, BTreeSet::from([player(2)]));
    assert_eq!(
        plan.transport.operations,
        vec![
            ReadycheckTransportOperation::DeleteReadycheck {
                channel_id: CHANNEL,
                message_id: MESSAGE,
                requirement: DeliveryRequirement::BestEffort,
            },
            ReadycheckTransportOperation::SyncLobbyDisplays {
                scope: scope(),
                requirement: DeliveryRequirement::Required,
            },
            ReadycheckTransportOperation::PostReadycheck {
                thread_id: ChannelId(999),
                requirement: DeliveryRequirement::Required,
            },
            ReadycheckTransportOperation::ReactToPostedReadycheck {
                emoji: "✅",
                requirement: DeliveryRequirement::BestEffort,
            },
            ReadycheckTransportOperation::MirrorPing {
                channel_id: ChannelId(555),
                mentioned_users: BTreeSet::from([player(3), player(4), player(5), player(6),]),
                requirement: DeliveryRequirement::Required,
            },
        ]
    );
}

#[test]
fn stronger_generation_permit_is_single_use_and_rejects_stale_commit() {
    let service = command_ready_service();
    let plan = service
        .prepare_command(request(10_000.0), data([1, 2]))
        .expect("new publication plan");
    let permit = plan.permit.expect("publication permit");

    assert_eq!(
        service.commit_publication(
            permit.clone(),
            MessageId(777),
            ChannelId(999),
            data([1, 2]),
            10_001.0,
        ),
        CommitPublicationResult::Applied
    );
    assert_eq!(
        service.commit_publication(
            permit,
            MessageId(778),
            ChannelId(999),
            data([1, 2]),
            10_002.0,
        ),
        CommitPublicationResult::StalePermit
    );
    assert_eq!(
        service
            .confirmation_snapshot(scope())
            .expect("committed generation")
            .message_id,
        MessageId(777)
    );
}

#[test]
fn stronger_completion_notification_is_exactly_once_per_generation() {
    let service = ReadycheckService::new();
    service.put_lobby(lobby_with([1, 2]));
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));

    assert!(service.take_completion_notification(scope(), MESSAGE));
    assert!(!service.take_completion_notification(scope(), MESSAGE));
    assert!(!service.take_completion_notification(scope(), MessageId(999)));
}

#[test]
fn failed_completion_delivery_can_retry_only_the_current_generation() {
    let service = ReadycheckService::new();
    service.put_lobby(lobby_with([1, 2]));
    set_generation(&service, [1, 2], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(1), "<@1>", None));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));

    assert!(service.take_completion_notification(scope(), MESSAGE));
    assert!(!service.release_completion_notification(scope(), MessageId(999)));
    assert!(service.release_completion_notification(scope(), MESSAGE));
    assert!(service.take_completion_notification(scope(), MESSAGE));
}

#[test]
fn stronger_readycheck_state_is_isolated_by_signed_guild() {
    let service = ReadycheckService::new();
    let other_scope = LobbyScope::new(OTHER_GUILD, LobbyKind::Open);
    set_generation(&service, [-1], 1_000.0);
    service.set_readycheck_state(
        other_scope,
        ReadycheckStateInput {
            message_id: MessageId(-111),
            channel_id: ChannelId(-222),
            lobby_ids: BTreeSet::from([player(-1)]),
            player_data: data([-1]),
            created_at: Some(2_000.0),
            initial_reacted: BTreeMap::new(),
        },
    );

    assert!(service.add_readycheck_reaction(scope(), player(-1), "default", None));
    assert!(service.add_readycheck_reaction(
        other_scope,
        player(-1),
        "other",
        Some(MessageId(-111)),
    ));

    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(-1), "default".to_owned())])
    );
    assert_eq!(
        service.readycheck_reacted(other_scope),
        BTreeMap::from([(player(-1), "other".to_owned())])
    );
}

#[test]
fn stronger_persisted_conversion_filters_orphan_timestamps() {
    let mut persisted = PersistedLobbyState::open(1, Some(-42), -7, "created");
    persisted.players = vec![-7];
    persisted.player_join_times = BTreeMap::from([(-7, 10.5), (-8, 20.5)]);

    let lobby = ReadyLobby::from_persisted(persisted);

    assert_eq!(lobby.scope.guild_id, GuildId(-42));
    assert_eq!(
        lobby.player_join_times,
        BTreeMap::from([(UserId(-7), 10.5)])
    );
}

fn staleness_service(ids: impl IntoIterator<Item = i64>) -> ReadycheckService {
    let service = ReadycheckService::new();
    let mut lobby = lobby_with(ids);
    lobby.thread_id = Some(ChannelId(999));
    service.put_lobby(lobby);
    service
}

fn grouped_data(entries: &[(i64, ReadinessGroup)]) -> BTreeMap<UserId, ReadycheckPlayerData> {
    entries
        .iter()
        .map(|(id, group)| {
            (
                player(*id),
                ReadycheckPlayerData {
                    name: format!("Player {id}"),
                    group: *group,
                    signals: String::new(),
                    joined_at: None,
                },
            )
        })
        .collect()
}

fn publish_new(
    service: &ReadycheckService,
    now: f64,
    player_data: BTreeMap<UserId, ReadycheckPlayerData>,
) -> ReadycheckCommandPlan {
    let plan = service
        .prepare_command(request(now), player_data.clone())
        .expect("prepare new readycheck");
    assert_eq!(plan.mode, PublicationMode::New);
    assert_eq!(
        service.commit_publication(
            plan.permit.clone().expect("new publication permit"),
            MESSAGE,
            CHANNEL,
            player_data,
            now,
        ),
        CommitPublicationResult::Applied
    );
    plan
}

#[test]
fn test_readycheck_allowed_under_10_players() {
    let service = staleness_service(1..=4);
    let plan = publish_new(&service, 1_000.0, data(1..=4));

    assert_eq!(plan.mode, PublicationMode::New);
    assert!(readycheck_description(4, 10).contains("need 6 more for a full game"));
}

#[test]
fn test_trigger_er_auto_counted_ready() {
    let service = staleness_service([1, 2]);
    let plan = publish_new(&service, 1_000.0, data([1, 2]));

    assert_eq!(plan.mention_ids, BTreeSet::from([player(2)]));
    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(1), "<@1>".to_owned())])
    );
}

#[test]
fn test_new_readycheck_accepts_reaction_before_checkmark_and_ping() {
    let service = staleness_service([1, 2]);
    let plan = publish_new(&service, 1_000.0, data([1, 2]));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", Some(MESSAGE)));

    assert_eq!(
        service
            .readycheck_reacted(scope())
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([player(1), player(2)])
    );
    assert!(matches!(
        plan.transport.operations.last(),
        Some(ReadycheckTransportOperation::MirrorPing { .. })
    ));
}

#[test]
fn test_new_readycheck_keeps_auto_confirms_when_ping_delivery_fails() {
    let service = staleness_service([1, 2]);
    let plan = publish_new(&service, 1_000.0, data([1, 2]));
    let mirror_delivery_failed = plan
        .transport
        .operations
        .iter()
        .any(|operation| matches!(operation, ReadycheckTransportOperation::MirrorPing { .. }));

    assert!(mirror_delivery_failed);
    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(1), "<@1>".to_owned())])
    );
}

#[test]
fn test_players_joined_under_ten_minutes_are_auto_counted_ready() {
    let service = staleness_service([1, 2, 3]);
    let player_data = grouped_data(&[
        (1, ReadinessGroup::Afk),
        (2, ReadinessGroup::Recent),
        (3, ReadinessGroup::Afk),
    ]);
    let plan = publish_new(&service, 2_000_000.0, player_data);

    assert_eq!(
        service
            .readycheck_reacted(scope())
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([player(1), player(2)])
    );
    assert_eq!(plan.mention_ids, BTreeSet::from([player(3)]));
}

#[test]
fn test_fresh_refresh_edits_in_place_and_preserves_reacted() {
    let service = staleness_service(1..=10);
    publish_new(&service, 1_000.0, data(1..=10));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;

    let plan = service
        .prepare_command(refresh, data(1..=10))
        .expect("fresh refresh");

    assert_eq!(plan.mode, PublicationMode::Refresh);
    assert!(plan.pruned_players.is_empty());
    assert!(service.readycheck_reacted(scope()).contains_key(&player(2)));
    assert_eq!(
        plan.transport.operations,
        [ReadycheckTransportOperation::EditReadycheck {
            channel_id: CHANNEL,
            message_id: MESSAGE,
            requirement: DeliveryRequirement::Required,
        }]
    );
}

#[test]
fn test_fresh_refresh_clears_departed_players_physical_reactions() {
    let service = staleness_service([1, 2, 3]);
    publish_new(&service, 1_000.0, data([1, 2, 3]));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    assert!(service.leave_lobby(scope(), player(2)));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;

    let plan = service
        .prepare_command(refresh, data([1, 3]))
        .expect("refresh after departure");

    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(1), "<@1>".to_owned())])
    );
    assert!(readycheck_description(2, 10).starts_with("**2** players"));
    assert!(matches!(
        plan.transport.operations.first(),
        Some(ReadycheckTransportOperation::RemoveDepartedReaction {
            player_id: UserId(2),
            requirement: DeliveryRequirement::BestEffort,
            ..
        })
    ));
}

#[test]
fn test_refresh_invoker_reaching_ten_announces_shuffle() {
    let service = staleness_service(1..=10);
    publish_new(&service, 1_000.0, data(1..=10));
    for id in 2..10 {
        assert!(service.add_readycheck_reaction(scope(), player(id), format!("<@{id}>"), None));
    }
    let mut refresh = request(1_121.0);
    refresh.invoker_id = player(10);
    refresh.existing_message = ExistingMessageState::Available;
    service
        .prepare_command(refresh, data(1..=10))
        .expect("invoker completes threshold");

    assert_eq!(service.readycheck_reacted(scope()).len(), 10);
    assert!(service.take_completion_notification(scope(), MESSAGE));
    assert!(ready_announcement(LobbyKind::Open, 10).contains("10+ players are ready"));
}

#[test]
fn test_stale_repost_deletes_old_and_resets_confirmations() {
    let service = staleness_service(1..=10);
    set_generation(&service, 1..=10, 0.0);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    let mut stale = request(2_000.0);
    stale.existing_message = ExistingMessageState::Available;

    let plan = service
        .prepare_command(stale, data(1..=10))
        .expect("stale replacement");
    assert_eq!(plan.mode, PublicationMode::ReplaceStale);
    assert!(matches!(
        plan.transport.operations.first(),
        Some(ReadycheckTransportOperation::DeleteReadycheck { .. })
    ));
    assert_eq!(
        service.commit_publication(
            plan.permit.expect("replacement permit"),
            MessageId(222),
            CHANNEL,
            data(1..=10),
            2_000.0,
        ),
        CommitPublicationResult::Applied
    );
    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(1), "<@1>".to_owned())])
    );
}

#[test]
fn test_stale_repost_keeps_afk_player_who_confirmed_old_check() {
    let service = staleness_service([1, 2, 3, 4]);
    set_generation(&service, [1, 2, 3, 4], 0.0);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    let classifications = grouped_data(&[
        (1, ReadinessGroup::Ready),
        (2, ReadinessGroup::Afk),
        (3, ReadinessGroup::Afk),
        (4, ReadinessGroup::Ready),
    ]);

    let plan = service
        .prepare_command(request(2_000.0), classifications)
        .expect("stale sweep");

    assert_eq!(plan.pruned_players, BTreeSet::from([player(3)]));
    assert!(
        service
            .lobby(scope())
            .expect("lobby")
            .players
            .contains(&player(2))
    );
}

#[test]
fn test_stale_repost_never_prunes_recent_join_even_if_classified_afk() {
    let service = staleness_service([1, 2]);
    let mut lobby = service.lobby(scope()).expect("lobby");
    lobby.player_join_times.insert(player(1), 0.0);
    lobby.player_join_times.insert(player(2), 1_500.0);
    service.put_lobby(lobby);
    set_generation(&service, [1, 2], 0.0);

    let plan = service
        .prepare_command(
            request(2_000.0),
            grouped_data(&[(1, ReadinessGroup::Ready), (2, ReadinessGroup::Afk)]),
        )
        .expect("stale recent-join sweep");

    assert!(plan.pruned_players.is_empty());
    assert!(
        service
            .lobby(scope())
            .expect("lobby")
            .players
            .contains(&player(2))
    );
}

#[test]
fn test_stale_repost_prunes_afk_no_shows() {
    let service = staleness_service(1..=6);
    set_generation(&service, 1..=6, 0.0);
    assert!(service.add_readycheck_reaction(scope(), player(4), "<@4>", None));
    let classifications = grouped_data(&[
        (1, ReadinessGroup::Afk),
        (2, ReadinessGroup::Afk),
        (3, ReadinessGroup::Afk),
        (4, ReadinessGroup::Afk),
        (5, ReadinessGroup::Ready),
        (6, ReadinessGroup::Afk),
    ]);

    let plan = service
        .prepare_command(request(2_000.0), classifications)
        .expect("stale sweep");

    assert_eq!(
        plan.pruned_players,
        BTreeSet::from([player(2), player(3), player(6)])
    );
    assert!(
        service
            .lobby(scope())
            .expect("lobby")
            .players
            .is_superset(&BTreeSet::from([player(1), player(4), player(5)]))
    );
}

#[test]
fn test_stale_prune_below_10_shows_shortfall_note() {
    let service = staleness_service(1..=12);
    set_generation(&service, 1..=12, 0.0);
    let classifications = (1..=12)
        .map(|id| {
            let group = if id <= 8 {
                ReadinessGroup::Ready
            } else {
                ReadinessGroup::Afk
            };
            (id, group)
        })
        .collect::<Vec<_>>();
    let plan = service
        .prepare_command(request(2_000.0), grouped_data(&classifications))
        .expect("stale prune");

    assert_eq!(plan.pruned_players.len(), 4);
    assert_eq!(service.lobby(scope()).expect("lobby").total_count(), 8);
    assert!(readycheck_description(8, 10).contains("need 2 more for a full game"));
}

#[test]
fn test_explicit_unready_is_not_reconfirmed_by_refresh() {
    let service = staleness_service([1, 2, 3]);
    let player_data = grouped_data(&[
        (1, ReadinessGroup::Afk),
        (2, ReadinessGroup::Recent),
        (3, ReadinessGroup::Afk),
    ]);
    publish_new(&service, 1_000.0, player_data.clone());
    assert!(service.remove_readycheck_reaction(scope(), player(2), None));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;
    service
        .prepare_command(refresh, player_data)
        .expect("fresh refresh");

    assert!(!service.readycheck_reacted(scope()).contains_key(&player(2)));
    assert!(service.readycheck_reacted(scope()).contains_key(&player(1)));
}

#[test]
fn test_explicit_unready_survives_other_player_leaving() {
    let service = staleness_service([1, 2, 3]);
    set_generation(&service, [1, 2, 3], 1_000.0);
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    assert!(service.remove_readycheck_reaction(scope(), player(2), None));
    assert!(service.leave_lobby(scope(), player(3)));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;
    service
        .prepare_command(refresh, data([1, 2]))
        .expect("sync after departure");

    assert!(!service.readycheck_reacted(scope()).contains_key(&player(2)));
}

#[test]
fn test_refresh_reconfirms_invoker_who_previously_unreadied() {
    let service = staleness_service([1, 2]);
    publish_new(&service, 1_000.0, data([1, 2]));
    assert!(service.remove_readycheck_reaction(scope(), player(1), None));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;
    service
        .prepare_command(refresh, data([1, 2]))
        .expect("invoker refresh");

    assert!(service.readycheck_reacted(scope()).contains_key(&player(1)));
    assert!(service.readycheck_declined(scope()).is_empty());
}

#[test]
fn test_reclicking_checkmark_after_unready_confirms_again() {
    let service = staleness_service([1, 2]);
    publish_new(
        &service,
        1_000.0,
        grouped_data(&[(1, ReadinessGroup::Afk), (2, ReadinessGroup::Recent)]),
    );
    assert!(service.remove_readycheck_reaction(scope(), player(2), None));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));

    assert!(service.readycheck_reacted(scope()).contains_key(&player(2)));
    assert!(!service.readycheck_declined(scope()).contains(&player(2)));
}

#[test]
fn test_failed_reaction_removal_still_prunes_departed_confirmation() {
    let service = staleness_service([1, 2, 3]);
    publish_new(&service, 1_000.0, data([1, 2, 3]));
    assert!(service.add_readycheck_reaction(scope(), player(2), "<@2>", None));
    assert!(service.leave_lobby(scope(), player(2)));
    let mut refresh = request(1_121.0);
    refresh.existing_message = ExistingMessageState::Available;

    service
        .prepare_command(refresh, data([1, 3]))
        .expect("state sync succeeds independently of physical removal");

    assert_eq!(
        service.readycheck_reacted(scope()),
        BTreeMap::from([(player(1), "<@1>".to_owned())])
    );
}

#[test]
fn test_stale_prune_keeps_player_reserved_by_in_flight_match() {
    let service = staleness_service([1, 2, 3]);
    set_generation(&service, [1, 2, 3], 0.0);
    let mut stale = request(2_000.0);
    stale.reserved_players.insert(player(2));
    let classifications = grouped_data(&[
        (1, ReadinessGroup::Ready),
        (2, ReadinessGroup::Afk),
        (3, ReadinessGroup::Afk),
    ]);

    let plan = service
        .prepare_command(stale, classifications)
        .expect("reserved stale sweep");

    assert_eq!(plan.pruned_players, BTreeSet::from([player(3)]));
    assert!(
        service
            .lobby(scope())
            .expect("lobby")
            .players
            .contains(&player(2))
    );
}

#[test]
fn test_ready_announcement_uses_configured_threshold() {
    assert!(ready_announcement(LobbyKind::Open, 3).contains("3+ players are ready"));
    assert!(!ready_announcement(LobbyKind::Open, 3).contains("10+ players"));
}
