use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cama_db::readycheck_repository::ReadycheckRepository;
use cama_db::schema_manager::initialize_or_migrate;
use tempfile::NamedTempFile;
use tokio::sync::Notify;

use super::*;
use crate::embeds::LobbyPlayer;
use crate::lobby_manager::PersistentLobbyManager;
use crate::lobby_service::{JoinFailure, PendingMatchState};

const GUILD: GuildId = GuildId(123);

fn scope(kind: LobbyKind) -> LobbyScope {
    LobbyScope::new(GUILD, kind)
}

fn users(ids: impl IntoIterator<Item = i64>) -> BTreeSet<UserId> {
    ids.into_iter().map(UserId).collect()
}

#[derive(Clone, Debug)]
struct FakePlayers {
    ratings: Arc<Mutex<BTreeMap<UserId, f64>>>,
    default_rating: f64,
}

impl Default for FakePlayers {
    fn default() -> Self {
        Self {
            ratings: Arc::new(Mutex::new(BTreeMap::new())),
            default_rating: 1_200.0,
        }
    }
}

impl FakePlayers {
    fn with_default_rating(default_rating: f64) -> Self {
        Self {
            default_rating,
            ..Self::default()
        }
    }
}

impl LobbyPlayerPort for FakePlayers {
    fn glicko_rating(&self, player_id: UserId, _guild_id: GuildId) -> Option<f64> {
        Some(
            self.ratings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&player_id)
                .copied()
                .unwrap_or(self.default_rating),
        )
    }

    fn get_by_ids(&self, player_ids: &[UserId], _guild_id: GuildId) -> Vec<LobbyPlayer> {
        player_ids
            .iter()
            .map(|player_id| LobbyPlayer::new(player_id.0, format!("Player{}", player_id.0)))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NoJoinPending;

impl PendingMatchPort for NoJoinPending {
    fn pending_match_for_player(
        &self,
        _guild_id: GuildId,
        _player_id: UserId,
    ) -> Option<PendingMatchState> {
        None
    }
}

#[derive(Debug, Default)]
struct IncrementingClock(AtomicI64);

impl LobbyClock for IncrementingClock {
    fn now_ns(&self) -> i64 {
        self.0.fetch_add(1_000_000_000, Ordering::Relaxed) + 1_000_000_000
    }
}

type TestRuntime = LobbyCommandRuntime<FakePlayers, NoJoinPending, IncrementingClock>;

fn runtime_with_rating(rating: f64) -> Arc<TestRuntime> {
    let service = Arc::new(LobbyService::new(
        FakePlayers::with_default_rating(rating),
        Some(NoJoinPending),
        IncrementingClock::default(),
        10,
        12,
    ));
    Arc::new(LobbyCommandRuntime::new(service))
}

fn runtime() -> Arc<TestRuntime> {
    runtime_with_rating(1_200.0)
}

fn open_lobby(runtime: &TestRuntime, kind: LobbyKind, creator: i64) {
    runtime
        .service()
        .get_or_create_lobby(Some(UserId(creator)), scope(kind))
        .expect("create lobby");
}

fn seat(runtime: &TestRuntime, player_id: i64, kinds: &[LobbyKind]) {
    for kind in kinds {
        let outcome = runtime
            .service()
            .join_lobby(UserId(player_id), scope(*kind));
        assert!(outcome.success, "join {kind:?} rejected: {outcome:?}");
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ThreadMessage {
    thread_id: ChannelId,
    content: String,
    allowed_mentions: AllowedMentions,
}

#[derive(Debug, Default)]
struct TransportState {
    display_syncs: Vec<LobbyScope>,
    reactions: Vec<(LobbyScope, UserId)>,
    thread_messages: Vec<ThreadMessage>,
    closes: Vec<LobbyScope>,
    archives: Vec<(LobbyScope, ChannelId)>,
    unpins: Vec<(LobbyScope, Option<ChannelId>, Option<MessageId>, ChannelId)>,
    responses: Vec<(String, AllowedMentions)>,
    fail_display: bool,
    fail_thread: bool,
    fail_cleanup: bool,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    state: Mutex<TransportState>,
    close_started: Option<Arc<Notify>>,
    release_close: Option<Arc<Notify>>,
}

impl RecordingTransport {
    fn blocking_close(close_started: Arc<Notify>, release_close: Arc<Notify>) -> Self {
        Self {
            state: Mutex::new(TransportState::default()),
            close_started: Some(close_started),
            release_close: Some(release_close),
        }
    }

    fn state(&self) -> MutexGuard<'_, TransportState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl LobbyRuntimeTransport for RecordingTransport {
    async fn sync_lobby_display(&self, scope: LobbyScope) -> Result<(), String> {
        let mut state = self.state();
        state.display_syncs.push(scope);
        if state.fail_display {
            Err("display failed".to_owned())
        } else {
            Ok(())
        }
    }

    async fn remove_lobby_reaction(
        &self,
        scope: LobbyScope,
        player_id: UserId,
    ) -> Result<(), String> {
        self.state().reactions.push((scope, player_id));
        Ok(())
    }

    async fn send_thread_message(
        &self,
        thread_id: ChannelId,
        content: &str,
        allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        let mut state = self.state();
        state.thread_messages.push(ThreadMessage {
            thread_id,
            content: content.to_owned(),
            allowed_mentions,
        });
        if state.fail_thread {
            Err("discord is down".to_owned())
        } else {
            Ok(())
        }
    }

    async fn close_lobby_message(
        &self,
        scope: LobbyScope,
        _message_ids: &LobbyMessageIds,
        _reason: &str,
    ) -> Result<(), String> {
        self.state().closes.push(scope);
        if let Some(started) = &self.close_started {
            started.notify_one();
        }
        if let Some(release) = &self.release_close {
            release.notified().await;
        }
        if self.state().fail_cleanup {
            Err("close failed".to_owned())
        } else {
            Ok(())
        }
    }

    async fn archive_lobby_thread(
        &self,
        scope: LobbyScope,
        thread_id: ChannelId,
        _reason: &str,
    ) -> Result<(), String> {
        let mut state = self.state();
        state.archives.push((scope, thread_id));
        if state.fail_cleanup {
            Err("archive failed".to_owned())
        } else {
            Ok(())
        }
    }

    async fn unpin_lobby_message(
        &self,
        scope: LobbyScope,
        channel_id: Option<ChannelId>,
        message_id: Option<MessageId>,
        fallback_channel_id: ChannelId,
    ) -> Result<(), String> {
        let mut state = self.state();
        state
            .unpins
            .push((scope, channel_id, message_id, fallback_channel_id));
        if state.fail_cleanup {
            Err("unpin failed".to_owned())
        } else {
            Ok(())
        }
    }

    async fn respond_ephemerally(
        &self,
        content: &str,
        allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        self.state()
            .responses
            .push((content.to_owned(), allowed_mentions));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct PendingMatches(Mutex<Vec<ScopedPendingMatch>>);

impl PendingMatches {
    fn new(matches: Vec<ScopedPendingMatch>) -> Self {
        Self(Mutex::new(matches))
    }
}

impl ResetPendingMatchPort for PendingMatches {
    fn pending_matches(&self, _guild_id: GuildId) -> Result<Vec<ScopedPendingMatch>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
}

#[derive(Clone, Copy, Debug)]
struct BrokenPendingGuard;

impl ResetPendingMatchPort for BrokenPendingGuard {
    fn pending_matches(&self, _guild_id: GuildId) -> Result<Vec<ScopedPendingMatch>, String> {
        Err("database credentials must stay private".to_owned())
    }
}

fn pending(kind: Option<LobbyKind>, jump_url: Option<&str>) -> ScopedPendingMatch {
    ScopedPendingMatch {
        lobby_kind: kind,
        shuffle_message_jump_url: jump_url.map(str::to_owned),
    }
}

fn reset_request(
    user_id: i64,
    is_admin: bool,
    explicit_kind: Option<LobbyKind>,
) -> ResetLobbyRequest {
    ResetLobbyRequest {
        guild_id: GUILD,
        user_id: UserId(user_id),
        is_admin,
        explicit_kind,
        interaction_channel_id: ChannelId(456),
    }
}

async fn reset(
    runtime: &TestRuntime,
    request: ResetLobbyRequest,
    pending_matches: &dyn ResetPendingMatchPort,
    drafts: &dyn ActiveDraftPort,
    transport: &RecordingTransport,
) -> ResetLobbyOutcome {
    runtime
        .reset_lobby(&request, pending_matches, drafts, transport)
        .await
}

// tests/test_dual_lobby_queue.py (17 exact behavior nodes)

#[test]
fn test_player_holds_a_seat_in_both_lobbies() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);

    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        assert!(
            runtime
                .service()
                .get_lobby(scope(kind))
                .expect("open lobby")
                .players
                .contains(&UserId(1))
        );
    }
}

#[test]
fn test_lobby_kinds_for_player_lists_every_seat_in_declaration_order() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::LowSkill, LobbyKind::Open]);
    seat(&runtime, 2, &[LobbyKind::LowSkill]);

    assert_eq!(
        runtime
            .service()
            .get_lobby_kinds_for_player(UserId(1), GUILD),
        [LobbyKind::Open, LobbyKind::LowSkill]
    );
    assert_eq!(
        runtime
            .service()
            .get_lobby_kinds_for_player(UserId(2), GUILD),
        [LobbyKind::LowSkill]
    );
    assert!(
        runtime
            .service()
            .get_lobby_kinds_for_player(UserId(3), GUILD)
            .is_empty()
    );
}

#[test]
fn test_rating_gate_still_bars_the_second_queue() {
    let runtime = runtime_with_rating(1_600.0);
    open_lobby(&runtime, LobbyKind::Open, 99);
    // An eligible creator is not needed once the low-skill lobby exists.
    runtime
        .service()
        .get_or_create_lobby(None, scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    seat(&runtime, 1, &[LobbyKind::Open]);

    assert_eq!(
        runtime
            .service()
            .join_lobby(UserId(1), scope(LobbyKind::LowSkill))
            .failure,
        Some(JoinFailure::RatingTooHigh)
    );
}

#[tokio::test]
async fn test_pop_evicts_only_the_players_who_got_a_match() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    for player_id in [1, 2, 3] {
        seat(&runtime, player_id, &[LobbyKind::Open, LobbyKind::LowSkill]);
    }

    let report = runtime
        .evict_matched_players_from_other_lobbies(
            &users([1, 2]),
            scope(LobbyKind::Open),
            &RecordingTransport::default(),
        )
        .await;

    assert_eq!(
        report.evicted,
        BTreeMap::from([(LobbyKind::LowSkill, users([1, 2]))])
    );
    assert_eq!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::LowSkill))
            .expect("low-skill lobby")
            .players,
        users([3])
    );
    assert_eq!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .expect("popped lobby remains")
            .players,
        users([1, 2, 3])
    );
}

#[tokio::test]
async fn test_pop_announces_the_departure_in_the_drained_lobbys_thread() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    assert!(runtime.service().set_lobby_message_ids(
        scope(LobbyKind::LowSkill),
        LobbyMessageIds {
            thread_id: Some(ChannelId(999)),
            ..LobbyMessageIds::default()
        }
    ));
    let transport = RecordingTransport::default();

    runtime
        .evict_matched_players_from_other_lobbies(&users([1]), scope(LobbyKind::Open), &transport)
        .await;

    let state = transport.state();
    assert_eq!(state.thread_messages.len(), 1);
    assert!(state.thread_messages[0].content.contains("<@1>"));
    assert!(
        state.thread_messages[0]
            .content
            .contains(LobbyKind::Open.label())
    );
    assert_eq!(
        state.thread_messages[0].allowed_mentions,
        AllowedMentions::None
    );
}

#[tokio::test]
async fn test_pop_reopens_the_ready_check_notice_when_quorum_breaks() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    assert!(runtime.service().set_readycheck_state(
        scope(LobbyKind::LowSkill),
        MessageId(555),
        ChannelId(456),
        users([1]),
        BTreeMap::from([(UserId(1), "<@1>".to_owned())]),
    ));
    runtime.mark_ready_notification(scope(LobbyKind::LowSkill), MessageId(555));
    runtime.mark_ready_notification(scope(LobbyKind::Open), MessageId(777));

    runtime
        .evict_matched_players_from_other_lobbies(
            &users([1]),
            scope(LobbyKind::Open),
            &RecordingTransport::default(),
        )
        .await;

    assert_eq!(runtime.ready_notification(scope(LobbyKind::LowSkill)), None);
    assert_eq!(
        runtime.ready_notification(scope(LobbyKind::Open)),
        Some(MessageId(777))
    );
}

#[tokio::test]
async fn test_pop_keeps_the_memo_when_the_lobby_is_still_ready() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    for player_id in 1..=12 {
        seat(&runtime, player_id, &[LobbyKind::LowSkill]);
    }
    seat(&runtime, 1, &[LobbyKind::Open]);
    assert!(
        runtime.service().set_readycheck_state(
            scope(LobbyKind::LowSkill),
            MessageId(555),
            ChannelId(456),
            users(1..=12),
            (1..=11)
                .map(|id| (UserId(id), format!("<@{id}>")))
                .collect(),
        )
    );
    runtime.mark_ready_notification(scope(LobbyKind::LowSkill), MessageId(555));

    runtime
        .evict_matched_players_from_other_lobbies(
            &users([1]),
            scope(LobbyKind::Open),
            &RecordingTransport::default(),
        )
        .await;

    assert_eq!(
        runtime.ready_notification(scope(LobbyKind::LowSkill)),
        Some(MessageId(555))
    );
    assert_eq!(
        runtime
            .service()
            .readycheck_confirmed_count(scope(LobbyKind::LowSkill)),
        10
    );
}

#[tokio::test]
async fn test_pop_is_a_no_op_when_nobody_was_double_queued() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open]);
    let transport = RecordingTransport::default();

    let report = runtime
        .evict_matched_players_from_other_lobbies(&users([1]), scope(LobbyKind::Open), &transport)
        .await;

    assert!(report.evicted.is_empty());
    assert!(transport.state().thread_messages.is_empty());
}

#[tokio::test]
async fn test_pop_survives_a_broken_thread_post() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    assert!(runtime.service().set_lobby_message_ids(
        scope(LobbyKind::LowSkill),
        LobbyMessageIds {
            thread_id: Some(ChannelId(999)),
            ..LobbyMessageIds::default()
        }
    ));
    let transport = RecordingTransport::default();
    transport.state().fail_thread = true;

    let report = runtime
        .evict_matched_players_from_other_lobbies(&users([1]), scope(LobbyKind::Open), &transport)
        .await;

    assert_eq!(
        report.evicted,
        BTreeMap::from([(LobbyKind::LowSkill, users([1]))])
    );
    assert!(
        report
            .publication_failures
            .iter()
            .any(|failure| failure.stage == PublicationStage::Thread)
    );
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::LowSkill))
            .expect("lobby remains")
            .players
            .is_empty()
    );
}

#[tokio::test]
async fn display_failure_does_not_undo_committed_cross_lobby_eviction() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    let transport = RecordingTransport::default();
    transport.state().fail_display = true;

    let report = runtime
        .evict_matched_players_from_other_lobbies(&users([1]), scope(LobbyKind::Open), &transport)
        .await;

    assert_eq!(
        report.evicted,
        BTreeMap::from([(LobbyKind::LowSkill, users([1]))])
    );
    assert!(
        report
            .publication_failures
            .iter()
            .any(|failure| failure.stage == PublicationStage::Display)
    );
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::LowSkill))
            .expect("lobby remains")
            .players
            .is_empty()
    );
}

#[test]
fn test_a_reserved_player_blocks_the_other_lobbys_shuffle() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    for player_id in [1, 2] {
        seat(&runtime, player_id, &[LobbyKind::Open, LobbyKind::LowSkill]);
    }
    let selected = users([1, 2]);

    assert!(
        runtime
            .service()
            .reserve_lobby_players(&selected, scope(LobbyKind::Open))
    );
    assert!(
        !runtime
            .service()
            .reserve_lobby_players(&selected, scope(LobbyKind::LowSkill))
    );
}

#[tokio::test]
async fn test_eviction_keeps_blocking_the_other_shuffle_after_the_release() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    for player_id in [1, 2] {
        seat(&runtime, player_id, &[LobbyKind::Open, LobbyKind::LowSkill]);
    }
    let selected = users([1, 2]);
    assert!(
        runtime
            .service()
            .reserve_lobby_players(&selected, scope(LobbyKind::Open))
    );

    runtime
        .evict_matched_players_from_other_lobbies(
            &selected,
            scope(LobbyKind::Open),
            &RecordingTransport::default(),
        )
        .await;
    runtime
        .service()
        .release_lobby_players(&selected, scope(LobbyKind::Open));

    assert!(
        !runtime
            .service()
            .reserve_lobby_players(&selected, scope(LobbyKind::LowSkill))
    );
}

#[test]
fn test_the_other_lobbys_reservation_does_not_pin_this_seat() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    assert!(
        runtime
            .service()
            .reserve_lobby_players(&users([1]), scope(LobbyKind::Open))
    );

    assert!(
        runtime
            .service()
            .leave_lobby(UserId(1), scope(LobbyKind::LowSkill))
    );
    assert!(
        !runtime
            .service()
            .leave_lobby(UserId(1), scope(LobbyKind::Open))
    );
}

#[test]
fn test_leave_clears_every_lobby_at_once() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);

    let outcome = runtime.leave_every_lobby(UserId(1), GUILD);

    assert_eq!(outcome.left, [LobbyKind::Open, LobbyKind::LowSkill]);
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        assert!(
            !runtime
                .service()
                .get_lobby(scope(kind))
                .expect("lobby remains")
                .players
                .contains(&UserId(1))
        );
        assert!(outcome.content.contains(kind.label()));
    }
}

#[test]
fn test_leave_keeps_the_seat_its_own_shuffle_is_holding() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);
    assert!(
        runtime
            .service()
            .reserve_lobby_players(&users([1]), scope(LobbyKind::Open))
    );

    let outcome = runtime.leave_every_lobby(UserId(1), GUILD);

    assert_eq!(outcome.left, [LobbyKind::LowSkill]);
    assert_eq!(outcome.pinned, [LobbyKind::Open]);
    assert!(
        outcome
            .content
            .contains(&format!("Left {}", LobbyKind::LowSkill.label()))
    );
    assert!(
        outcome
            .content
            .contains(&format!("Still in {}", LobbyKind::Open.label()))
    );
}

#[test]
fn test_readycheck_asks_which_lobby_when_the_caller_is_in_both() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);

    assert_eq!(
        runtime.select_readycheck_lobby(UserId(1), GUILD, None),
        Err(LobbySelectionError::Ambiguous)
    );
    assert_eq!(
        match runtime.select_readycheck_lobby(UserId(1), GUILD, None) {
            Err(LobbySelectionError::Ambiguous) => AMBIGUOUS_LOBBY_MESSAGE,
            _ => "wrong selection outcome",
        },
        AMBIGUOUS_LOBBY_MESSAGE
    );
}

#[test]
fn test_readycheck_accepts_an_explicit_lobby_from_a_dual_queued_caller() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::Open, LobbyKind::LowSkill]);

    assert_eq!(
        runtime.select_readycheck_lobby(UserId(1), GUILD, Some(LobbyKind::LowSkill)),
        Ok(LobbyKind::LowSkill)
    );
}

#[test]
fn test_readycheck_still_infers_a_single_seat() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::LowSkill, 99);
    seat(&runtime, 1, &[LobbyKind::LowSkill]);

    assert_eq!(
        runtime.select_readycheck_lobby(UserId(1), GUILD, None),
        Ok(LobbyKind::LowSkill)
    );
}

// tests/test_resetlobby_command.py (16 exact behavior nodes)

#[tokio::test]
async fn test_resetlobby_allows_admin() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 42, &[LobbyKind::Open]);
    seat(&runtime, 1, &[LobbyKind::Open]);
    let transport = RecordingTransport::default();

    let outcome = reset(&runtime, reset_request(1, true, None), &(), &(), &transport).await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
    assert!(
        transport.state().responses[0]
            .0
            .contains("All You Can Feed reset")
    );
}

#[tokio::test]
async fn test_resetlobby_allows_creator() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 7, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
}

#[tokio::test]
async fn test_resetlobby_infers_creator_lowskill_membership() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    open_lobby(&runtime, LobbyKind::LowSkill, 7);
    seat(&runtime, 7, &[LobbyKind::LowSkill]);

    reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert_eq!(
        runtime.service().get_lobby(scope(LobbyKind::LowSkill)),
        None
    );
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_blocks_pending_match() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 1, &[LobbyKind::Open]);
    let pending_matches = PendingMatches::new(vec![pending(None, Some("http://example.com"))]);

    let outcome = reset(
        &runtime,
        reset_request(1, true, None),
        &pending_matches,
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::PendingMatch,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_blocks_when_multiple_matches_are_pending() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 1, &[LobbyKind::Open]);
    let pending_matches = PendingMatches::new(vec![
        pending(Some(LobbyKind::Open), Some("http://example.com/1")),
        pending(Some(LobbyKind::Open), Some("http://example.com/2")),
    ]);

    let outcome = reset(
        &runtime,
        reset_request(1, true, None),
        &pending_matches,
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::PendingMatch,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_ignores_pending_match_from_sibling_lobby() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::LowSkill, 7);
    seat(&runtime, 7, &[LobbyKind::LowSkill]);
    let pending_matches = PendingMatches::new(vec![pending(Some(LobbyKind::Open), None)]);

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &pending_matches,
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(
        runtime.service().get_lobby(scope(LobbyKind::LowSkill)),
        None
    );
}

#[tokio::test]
async fn test_resetlobby_ignores_active_draft_from_sibling_lobby() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::LowSkill, 7);
    seat(&runtime, 7, &[LobbyKind::LowSkill]);
    let drafts = DraftStateManager::default();
    drafts
        .create_draft(Some(GUILD.0), DraftLobbyKind::Open)
        .expect("open draft");

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &drafts,
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(
        runtime.service().get_lobby(scope(LobbyKind::LowSkill)),
        None
    );
}

#[tokio::test]
async fn test_resetlobby_blocks_active_draft_from_same_lobby() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::LowSkill, 7);
    seat(&runtime, 7, &[LobbyKind::LowSkill]);
    let drafts = DraftStateManager::default();
    drafts
        .create_draft(Some(GUILD.0), DraftLobbyKind::LowSkill)
        .expect("low-skill draft");

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &drafts,
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::ActiveDraft,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::LowSkill))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_blocks_in_flight_shuffle_from_same_lobby() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 7, &[LobbyKind::Open]);
    assert!(
        runtime
            .service()
            .reserve_lobby_players(&users([7]), scope(LobbyKind::Open))
    );

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::InFlight,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_serializes_discord_cleanup_with_match_start() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 7, &[LobbyKind::Open]);
    seat(&runtime, 8, &[LobbyKind::Open]);
    let close_started = Arc::new(Notify::new());
    let release_close = Arc::new(Notify::new());
    let transport = Arc::new(RecordingTransport::blocking_close(
        Arc::clone(&close_started),
        Arc::clone(&release_close),
    ));

    let reset_runtime = Arc::clone(&runtime);
    let reset_transport = Arc::clone(&transport);
    let reset_task = tokio::spawn(async move {
        reset_runtime
            .reset_lobby(
                &reset_request(7, false, None),
                &(),
                &(),
                reset_transport.as_ref(),
            )
            .await
    });
    close_started.notified().await;

    let reserve_runtime = Arc::clone(&runtime);
    let mut reserve_task = tokio::spawn(async move {
        reserve_runtime
            .reserve_for_match(&users([7, 8]), scope(LobbyKind::Open))
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut reserve_task)
            .await
            .is_err()
    );

    release_close.notify_one();
    let outcome = reset_task.await.expect("reset task");
    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert!(!reserve_task.await.expect("reserve task"));
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
}

#[tokio::test]
async fn test_resetlobby_denies_non_admin_non_creator() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 5);
    seat(&runtime, 5, &[LobbyKind::Open]);
    seat(&runtime, 6, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(6, false, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::PermissionDenied,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_new_lobby_is_empty() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    for player_id in [1, 2, 3] {
        seat(&runtime, player_id, &[LobbyKind::Open]);
    }
    assert_eq!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .expect("lobby")
            .players
            .len(),
        3
    );

    reset(
        &runtime,
        reset_request(1, true, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;
    let new_lobby = runtime
        .service()
        .get_or_create_lobby(Some(UserId(99)), scope(LobbyKind::Open))
        .expect("new lobby");

    assert!(new_lobby.players.is_empty());
}

#[tokio::test]
async fn test_abort_resets_lobby_message_id() {
    let runtime = runtime();
    let open = scope(LobbyKind::Open);
    open_lobby(&runtime, LobbyKind::Open, 99);
    for player_id in 1..=10 {
        seat(&runtime, player_id, &[LobbyKind::Open]);
    }
    assert!(runtime.service().set_lobby_message_ids(
        open,
        LobbyMessageIds {
            channel_id: Some(ChannelId(456)),
            message_id: Some(MessageId(12_345)),
            ..LobbyMessageIds::default()
        }
    ));
    assert_eq!(
        runtime.service().get_lobby_message_id(open),
        Some(MessageId(12_345))
    );

    let outcome = reset(
        &runtime,
        reset_request(1, true, Some(LobbyKind::Open)),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;
    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(runtime.service().get_lobby_message_id(open), None);
    assert!(runtime.service().get_lobby(open).is_none());

    let fresh = runtime
        .service()
        .get_or_create_lobby(Some(UserId(99)), open)
        .expect("create fresh lobby after abort/reset");
    assert!(fresh.players.is_empty());
    assert!(runtime.service().set_lobby_message_ids(
        open,
        LobbyMessageIds {
            channel_id: Some(ChannelId(456)),
            message_id: Some(MessageId(99_999)),
            ..LobbyMessageIds::default()
        }
    ));
    assert_eq!(
        runtime.service().get_lobby_message_id(open),
        Some(MessageId(99_999))
    );
}

#[tokio::test]
async fn test_resetlobby_admin_can_reset_lobby_without_membership() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 42, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(1, true, Some(LobbyKind::Open)),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
}

#[tokio::test]
async fn test_resetlobby_departed_creator_can_reset_with_choice() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 42, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(7, false, Some(LobbyKind::Open)),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
}

#[tokio::test]
async fn test_resetlobby_choice_still_enforces_permissions() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 42, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(6, false, Some(LobbyKind::Open)),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::PermissionDenied,
            ..
        }
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

#[tokio::test]
async fn test_resetlobby_non_member_without_choice_gets_hint() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 99);
    seat(&runtime, 42, &[LobbyKind::Open]);

    let outcome = reset(
        &runtime,
        reset_request(1, true, None),
        &(),
        &(),
        &RecordingTransport::default(),
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::NoLobbySelection,
            ref content,
            ..
        } if content.contains("`lobby` option")
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

// Stronger lift-and-shift bridge beyond the mocked canonical nodes.

#[test]
fn existing_schema_restart_preserves_dual_seats_and_kind_scoped_reset() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("initialize canonical schema");
    let repository = ReadycheckRepository::new(database.path());
    let mut first = PersistentLobbyManager::new(repository.clone()).expect("first startup");
    let open = scope(LobbyKind::Open);
    let low = scope(LobbyKind::LowSkill);
    first
        .get_or_create_lobby_at(open, Some(UserId(99)), "2026-08-12T00:00:00Z")
        .expect("open lobby");
    first
        .get_or_create_lobby_at(low, Some(UserId(99)), "2026-08-12T00:00:00Z")
        .expect("low lobby");
    first
        .join_lobby_at(UserId(1), open, 100.0)
        .expect("join open");
    first
        .join_lobby_at(UserId(1), low, 101.0)
        .expect("join low");
    drop(first);

    let mut restarted = PersistentLobbyManager::new(repository.clone()).expect("restart hydration");
    assert!(
        restarted
            .get_lobby(open)
            .expect("open after restart")
            .players
            .contains(&UserId(1))
    );
    assert!(
        restarted
            .get_lobby(low)
            .expect("low after restart")
            .players
            .contains(&UserId(1))
    );
    restarted.reset_lobby(low).expect("reset only low");
    drop(restarted);

    let final_restart = PersistentLobbyManager::new(repository).expect("final restart");
    assert!(final_restart.get_lobby(open).is_some());
    assert!(final_restart.get_lobby(low).is_none());
}

#[tokio::test]
async fn reset_cleanup_failures_are_reported_but_do_not_resurrect_the_lobby() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 7, &[LobbyKind::Open]);
    assert!(runtime.service().set_lobby_message_ids(
        scope(LobbyKind::Open),
        LobbyMessageIds {
            message_id: Some(MessageId(10)),
            channel_id: Some(ChannelId(20)),
            thread_id: Some(ChannelId(30)),
            ..LobbyMessageIds::default()
        }
    ));
    let transport = RecordingTransport::default();
    transport.state().fail_cleanup = true;

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &(),
        &(),
        &transport,
    )
    .await;

    let ResetLobbyOutcome::Reset {
        publication_failures,
        ..
    } = outcome
    else {
        panic!("cleanup failures must remain fail-soft");
    };
    assert_eq!(publication_failures.len(), 3);
    assert_eq!(runtime.service().get_lobby(scope(LobbyKind::Open)), None);
    assert_eq!(transport.state().responses[0].1, AllowedMentions::Default);
}

#[tokio::test]
async fn reset_fails_closed_when_the_pending_match_guard_errors() {
    let runtime = runtime();
    open_lobby(&runtime, LobbyKind::Open, 7);
    seat(&runtime, 7, &[LobbyKind::Open]);
    let transport = RecordingTransport::default();

    let outcome = reset(
        &runtime,
        reset_request(7, false, None),
        &BrokenPendingGuard,
        &(),
        &transport,
    )
    .await;

    assert!(matches!(
        outcome,
        ResetLobbyOutcome::Refused {
            reason: ResetRefusal::GuardFailure,
            ref content,
            ..
        } if content.contains("Could not verify pending matches")
            && !content.contains("credentials")
    ));
    assert!(
        runtime
            .service()
            .get_lobby(scope(LobbyKind::Open))
            .is_some()
    );
}

// Exact counterparts for the legacy asyncio shuffle-lock contract. Rust does
// not need Python's manually timestamped stale-lock table: a Tokio mutex guard
// is released when its task is cancelled or unwinds. These cases therefore
// assert the same caller-visible guarantees against the production lock map.

#[tokio::test]
async fn test_shuffle_lock_prevents_concurrent_shuffles() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(scope(LobbyKind::Open));
    let guard = lock.lock().await;

    assert!(lock.try_lock().is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), lock.lock())
            .await
            .is_err()
    );

    drop(guard);
    let acquired = tokio::time::timeout(std::time::Duration::from_millis(100), lock.lock())
        .await
        .expect("released shuffle lock must be acquirable");
    drop(acquired);
}

#[tokio::test]
async fn test_shuffle_locks_are_per_guild() {
    let runtime = runtime();
    let first = runtime.scope_operation_lock(LobbyScope::new(GuildId(11_111), LobbyKind::Open));
    let second = runtime.scope_operation_lock(LobbyScope::new(GuildId(22_222), LobbyKind::Open));
    assert!(!Arc::ptr_eq(&first, &second));

    let first_guard = first.lock().await;
    let second_guard = second
        .try_lock()
        .expect("another guild must remain independently acquirable");
    drop(second_guard);
    drop(first_guard);
}

#[tokio::test]
async fn test_lock_released_on_exception() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(scope(LobbyKind::Open));
    let result: Result<(), &'static str> = {
        let guard = lock.lock().await;
        drop(guard);
        Err("simulated shuffle error")
    };
    assert_eq!(result, Err("simulated shuffle error"));
    assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn test_stale_lock_auto_releases() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(scope(LobbyKind::Open));
    let acquired = Arc::new(Notify::new());
    let task_lock = Arc::clone(&lock);
    let task_acquired = Arc::clone(&acquired);
    let task = tokio::spawn(async move {
        let _guard = task_lock.lock().await;
        task_acquired.notify_one();
        std::future::pending::<()>().await;
    });
    acquired.notified().await;
    assert!(lock.try_lock().is_err());

    task.abort();
    assert!(
        task.await
            .expect_err("task must be cancelled")
            .is_cancelled()
    );
    let guard = tokio::time::timeout(std::time::Duration::from_millis(100), lock.lock())
        .await
        .expect("cancelled owner must release its guard");
    drop(guard);
}

#[tokio::test]
async fn test_non_stale_lock_not_released() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(scope(LobbyKind::Open));
    let guard = lock.lock().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), lock.lock())
            .await
            .is_err()
    );
    assert!(lock.try_lock().is_err());
    drop(guard);
}

#[tokio::test]
async fn test_null_guild_id_normalized_to_zero() {
    let runtime = runtime();
    let normalized_none =
        runtime.scope_operation_lock(LobbyScope::new(GuildId::normalize(None), LobbyKind::Open));
    let explicit_zero = runtime.scope_operation_lock(LobbyScope::new(
        GuildId::normalize(Some(GuildId(0))),
        LobbyKind::Open,
    ));
    assert!(Arc::ptr_eq(&normalized_none, &explicit_zero));
}

#[tokio::test]
async fn test_get_shuffle_lock_creates_new_lock() {
    let runtime = runtime();
    let requested_scope = LobbyScope::new(GuildId(99_999), LobbyKind::Open);
    let first = runtime.scope_operation_lock(requested_scope);
    let second = runtime.scope_operation_lock(requested_scope);
    assert!(Arc::ptr_eq(&first, &second));
    assert!(first.try_lock().is_ok());
}

#[tokio::test]
async fn test_clear_lock_time_removes_entry() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(LobbyScope::new(GuildId(66_666), LobbyKind::Open));
    let guard = lock.lock().await;
    assert!(lock.try_lock().is_err());
    drop(guard);
    assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn test_shuffle_locks_are_per_lobby_kind() {
    let runtime = runtime();
    let open = runtime.scope_operation_lock(scope(LobbyKind::Open));
    let low_skill = runtime.scope_operation_lock(scope(LobbyKind::LowSkill));
    assert!(!Arc::ptr_eq(&open, &low_skill));

    let open_guard = open.lock().await;
    assert!(low_skill.try_lock().is_ok());
    drop(open_guard);
}

#[tokio::test]
async fn test_clear_lock_time_handles_missing_entry() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(LobbyScope::new(GuildId(77_777), LobbyKind::Open));
    assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn test_concurrent_shuffle_mutual_exclusion() {
    let runtime = runtime();
    let lock = runtime.scope_operation_lock(LobbyScope::new(GuildId(88_888), LobbyKind::Open));
    let entered = Arc::new(AtomicUsize::new(0));
    let max_entered = Arc::new(AtomicUsize::new(0));
    let first_acquired = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());

    let first_lock = Arc::clone(&lock);
    let first_entered = Arc::clone(&entered);
    let first_max = Arc::clone(&max_entered);
    let acquired_signal = Arc::clone(&first_acquired);
    let release_signal = Arc::clone(&release_first);
    let first = tokio::spawn(async move {
        let _guard = first_lock.lock().await;
        let current = first_entered.fetch_add(1, Ordering::SeqCst) + 1;
        first_max.fetch_max(current, Ordering::SeqCst);
        acquired_signal.notify_one();
        release_signal.notified().await;
        first_entered.fetch_sub(1, Ordering::SeqCst);
        "shuffle_1_success"
    });

    first_acquired.notified().await;
    let second = tokio::time::timeout(std::time::Duration::from_millis(10), lock.lock()).await;
    assert!(
        second.is_err(),
        "second shuffle must be rejected while held"
    );
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    assert_eq!(max_entered.load(Ordering::SeqCst), 1);

    release_first.notify_one();
    assert_eq!(first.await.unwrap(), "shuffle_1_success");
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    assert!(lock.try_lock().is_ok());
}

#[tokio::test]
async fn test_creation_lock_prevents_race_condition() {
    let runtime = runtime();
    let target = LobbyScope::new(GuildId(55_001), LobbyKind::Open);
    let first_inside = Arc::new(Notify::new());
    let first_may_finish = Arc::new(Notify::new());
    let creation_count = Arc::new(AtomicUsize::new(0));

    let first_runtime = Arc::clone(&runtime);
    let first_inside_signal = Arc::clone(&first_inside);
    let first_release = Arc::clone(&first_may_finish);
    let first_count = Arc::clone(&creation_count);
    let first = tokio::spawn(async move {
        let lock = first_runtime.scope_operation_lock(target);
        let _guard = lock.lock().await;
        assert!(first_runtime.service().get_lobby(target).is_none());
        first_inside_signal.notify_one();
        first_release.notified().await;
        first_runtime
            .service()
            .get_or_create_lobby(Some(UserId(1)), target)
            .expect("first creator");
        first_count.fetch_add(1, Ordering::SeqCst);
        "created"
    });

    first_inside.notified().await;
    let second_runtime = Arc::clone(&runtime);
    let second_count = Arc::clone(&creation_count);
    let second = tokio::spawn(async move {
        let lock = second_runtime.scope_operation_lock(target);
        let _guard = lock.lock().await;
        if second_runtime.service().get_lobby(target).is_some() {
            return "existing";
        }
        second_runtime
            .service()
            .get_or_create_lobby(Some(UserId(2)), target)
            .expect("second creator");
        second_count.fetch_add(1, Ordering::SeqCst);
        "created"
    });
    tokio::task::yield_now().await;
    first_may_finish.notify_one();

    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(creation_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == "created")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == "existing")
            .count(),
        1
    );
}

#[tokio::test]
async fn test_lock_serializes_access() {
    let runtime = runtime();
    let target = LobbyScope::new(GuildId(55_002), LobbyKind::Open);
    let order = Arc::new(Mutex::new(Vec::new()));
    let first_inside = Arc::new(Notify::new());
    let first_may_finish = Arc::new(Notify::new());

    let first_runtime = Arc::clone(&runtime);
    let first_order = Arc::clone(&order);
    let first_signal = Arc::clone(&first_inside);
    let first_release = Arc::clone(&first_may_finish);
    let first = tokio::spawn(async move {
        let lock = first_runtime.scope_operation_lock(target);
        let _guard = lock.lock().await;
        first_order.lock().unwrap().push("task1_start");
        first_signal.notify_one();
        first_release.notified().await;
        first_order.lock().unwrap().push("task1_end");
    });

    first_inside.notified().await;
    let second_runtime = Arc::clone(&runtime);
    let second_order = Arc::clone(&order);
    let second = tokio::spawn(async move {
        let lock = second_runtime.scope_operation_lock(target);
        let _guard = lock.lock().await;
        let mut order = second_order.lock().unwrap();
        order.push("task2_start");
        order.push("task2_end");
    });
    tokio::task::yield_now().await;
    first_may_finish.notify_one();
    first.await.unwrap();
    second.await.unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        ["task1_start", "task1_end", "task2_start", "task2_end"]
    );
}

#[tokio::test]
async fn test_lock_property_returns_same_instance() {
    let runtime = runtime();
    let target = LobbyScope::new(GuildId(55_003), LobbyKind::Open);
    let first = runtime.scope_operation_lock(target);
    let second = runtime.scope_operation_lock(target);
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn test_creation_locks_are_per_guild() {
    let runtime = runtime();
    let first = runtime.scope_operation_lock(LobbyScope::new(GuildId(1), LobbyKind::Open));
    let first_again = runtime.scope_operation_lock(LobbyScope::new(GuildId(1), LobbyKind::Open));
    let second = runtime.scope_operation_lock(LobbyScope::new(GuildId(2), LobbyKind::Open));
    assert!(Arc::ptr_eq(&first, &first_again));
    assert!(!Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn test_multiple_concurrent_calls_only_one_creates() {
    let runtime = runtime();
    let target = LobbyScope::new(GuildId(55_004), LobbyKind::Open);
    let creation_count = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for user_id in 1..=5 {
        let runtime = Arc::clone(&runtime);
        let creation_count = Arc::clone(&creation_count);
        tasks.push(tokio::spawn(async move {
            let lock = runtime.scope_operation_lock(target);
            let _guard = lock.lock().await;
            if runtime.service().get_lobby(target).is_some() {
                return "existing";
            }
            tokio::task::yield_now().await;
            runtime
                .service()
                .get_or_create_lobby(Some(UserId(user_id)), target)
                .expect("create lobby");
            creation_count.fetch_add(1, Ordering::SeqCst);
            "created"
        }));
    }
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }

    assert_eq!(creation_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == "created")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == "existing")
            .count(),
        4
    );
}
