use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicUsize, Ordering},
};

use super::*;
use cama_db::moderation::{SuspensionCompletion, SuspensionScope};

const GUILD: GuildId = GuildId(99);

type PlayerLoadCall = (Vec<UserId>, GuildId);

fn scope(kind: LobbyKind) -> LobbyScope {
    LobbyScope::new(GUILD, kind)
}

fn users(ids: &[i64]) -> BTreeSet<UserId> {
    ids.iter().copied().map(UserId).collect()
}

#[derive(Clone, Debug)]
struct FakePlayers {
    ratings: Arc<Mutex<BTreeMap<(GuildId, UserId), f64>>>,
    default_rating: Option<f64>,
    load_calls: Arc<Mutex<Vec<PlayerLoadCall>>>,
}

impl Default for FakePlayers {
    fn default() -> Self {
        Self {
            ratings: Arc::new(Mutex::new(BTreeMap::new())),
            default_rating: Some(1_200.0),
            load_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl FakePlayers {
    fn set_rating(&self, player_id: UserId, guild_id: GuildId, rating: f64) {
        self.ratings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert((guild_id, player_id), rating);
    }

    fn load_calls(&self) -> Vec<PlayerLoadCall> {
        self.load_calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl LobbyPlayerPort for FakePlayers {
    fn glicko_rating(&self, player_id: UserId, guild_id: GuildId) -> Option<f64> {
        self.ratings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(guild_id, player_id))
            .copied()
            .or(self.default_rating)
    }

    fn get_by_ids(&self, player_ids: &[UserId], guild_id: GuildId) -> Vec<LobbyPlayer> {
        self.load_calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((player_ids.to_vec(), guild_id));
        player_ids
            .iter()
            .map(|player_id| {
                let mut player = LobbyPlayer::new(player_id.0, format!("Player {}", player_id.0));
                player.glicko_rating = self.glicko_rating(*player_id, guild_id);
                player
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
struct FakePendingMatches {
    pending: Arc<Mutex<Option<PendingMatchState>>>,
    calls: Arc<Mutex<Vec<(GuildId, UserId)>>>,
}

impl FakePendingMatches {
    fn with_pending(pending: PendingMatchState) -> Self {
        Self {
            pending: Arc::new(Mutex::new(Some(pending))),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl PendingMatchPort for FakePendingMatches {
    fn pending_match_for_player(
        &self,
        guild_id: GuildId,
        player_id: UserId,
    ) -> Option<PendingMatchState> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((guild_id, player_id));
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Clone, Debug)]
struct FakeClock {
    values: Arc<Mutex<VecDeque<i64>>>,
    fallback: Arc<Mutex<i64>>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::from_values([1_000_000_000])
    }
}

impl FakeClock {
    fn from_values(values: impl IntoIterator<Item = i64>) -> Self {
        let values = values.into_iter().collect::<VecDeque<_>>();
        let fallback = values.back().copied().unwrap_or_default().saturating_add(1);
        Self {
            values: Arc::new(Mutex::new(values)),
            fallback: Arc::new(Mutex::new(fallback)),
        }
    }
}

impl LobbyClock for FakeClock {
    fn now_ns(&self) -> i64 {
        if let Some(value) = self
            .values
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
        {
            return value;
        }
        let mut fallback = self
            .fallback
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let value = *fallback;
        *fallback = fallback.saturating_add(1);
        value
    }
}

type TestService = LobbyService<FakePlayers, FakePendingMatches, FakeClock>;

fn service(players: FakePlayers, max_players: usize) -> TestService {
    LobbyService::new(
        players,
        Some(FakePendingMatches::default()),
        FakeClock::default(),
        10,
        max_players,
    )
}

fn assert_joined(outcome: &JoinOutcome) -> JoinCommit {
    assert!(outcome.success);
    assert_eq!(outcome.failure, None);
    outcome.join_commit().expect("successful join has a commit")
}

fn suspension(scope: SuspensionScope) -> LobbySuspension {
    LobbySuspension {
        discord_id: 10,
        guild_id: GUILD.0,
        scope,
        completion: SuspensionCompletion::Time,
        reason: "Take a matchmaking break".to_owned(),
        source: cama_db::moderation::ModerationSource::Admin,
        actor_id: 900,
        expires_at: Some(i64::MAX),
        matches_required: None,
        matches_remaining: None,
        pending_match_watermark: 0,
        revision: 1,
        active: true,
        created_at: 1,
        updated_at: 1,
    }
}

#[derive(Debug, Default)]
struct FakeModeration {
    state: Mutex<Option<LobbySuspension>>,
    scripted: Mutex<VecDeque<Result<Option<LobbySuspension>, String>>>,
    calls: Mutex<Vec<(UserId, LobbyScope)>>,
}

impl FakeModeration {
    fn fixed(state: LobbySuspension) -> Self {
        Self {
            state: Mutex::new(Some(state)),
            ..Self::default()
        }
    }

    fn scripted(
        responses: impl IntoIterator<Item = Result<Option<LobbySuspension>, String>>,
    ) -> Self {
        Self {
            scripted: Mutex::new(responses.into_iter().collect()),
            ..Self::default()
        }
    }

    fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl LobbyModerationPort for FakeModeration {
    fn active_suspension(
        &self,
        player_id: UserId,
        lobby_scope: LobbyScope,
        _now: i64,
    ) -> Result<Option<LobbySuspension>, String> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((player_id, lobby_scope));
        if let Some(response) = self
            .scripted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
        {
            return response;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Ok(state.filter(|state| {
            state.scope == SuspensionScope::All
                || state.scope
                    == match lobby_scope.kind {
                        LobbyKind::Open => SuspensionScope::Open,
                        LobbyKind::LowSkill => SuspensionScope::Lowskill,
                    }
        }))
    }
}

#[derive(Debug, Default)]
struct RecordingPersistence {
    rows: Mutex<BTreeMap<(i64, i64), PersistedLobbyState>>,
}

impl RecordingPersistence {
    fn rows(&self) -> Vec<PersistedLobbyState> {
        self.rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

impl LobbyPersistencePort for RecordingPersistence {
    fn load_all_lobbies(&self) -> Result<Vec<PersistedLobbyState>, String> {
        Ok(self.rows())
    }

    fn save_lobby(&self, lobby: &PersistedLobbyState) -> Result<(), String> {
        self.rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert((lobby.lobby_id, lobby.guild_id), lobby.clone());
        Ok(())
    }

    fn clear_lobby(&self, lobby_scope: LobbyScope) -> Result<bool, String> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(lobby_scope.lobby_id(), lobby_scope.guild_id.0))
            .is_some())
    }
}

type PersistHook = dyn Fn() + Send + Sync;

/// Persistence that runs a hook inside `save_lobby`/`clear_lobby`, standing in
/// for a slow SQLite commit.
#[derive(Default)]
struct HookPersistence {
    inner: RecordingPersistence,
    hook: Mutex<Option<Arc<PersistHook>>>,
}

impl HookPersistence {
    fn set_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.hook.lock().unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(hook));
    }

    fn run_hook(&self) {
        let hook = self
            .hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl LobbyPersistencePort for HookPersistence {
    fn load_all_lobbies(&self) -> Result<Vec<PersistedLobbyState>, String> {
        self.inner.load_all_lobbies()
    }

    fn save_lobby(&self, lobby: &PersistedLobbyState) -> Result<(), String> {
        self.run_hook();
        self.inner.save_lobby(lobby)
    }

    fn clear_lobby(&self, lobby_scope: LobbyScope) -> Result<bool, String> {
        self.run_hook();
        self.inner.clear_lobby(lobby_scope)
    }
}

type ModerationHook = dyn Fn(usize, UserId, LobbyScope) + Send + Sync;

#[derive(Default)]
struct HookModeration {
    state: Mutex<Option<LobbySuspension>>,
    calls: AtomicUsize,
    hook: Mutex<Option<Arc<ModerationHook>>>,
}

impl std::fmt::Debug for HookModeration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookModeration")
            .field("calls", &self.calls.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl HookModeration {
    fn set_hook(&self, hook: impl Fn(usize, UserId, LobbyScope) + Send + Sync + 'static) {
        *self.hook.lock().unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(hook));
    }

    fn suspend(&self, state: LobbySuspension) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = Some(state);
    }
}

impl LobbyModerationPort for HookModeration {
    fn active_suspension(
        &self,
        player_id: UserId,
        lobby_scope: LobbyScope,
        _now: i64,
    ) -> Result<Option<LobbySuspension>, String> {
        // Snapshot first. A hook that commits a suspension models it landing
        // immediately after this unlocked read returned.
        let result = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .filter(|state| {
                state.scope == SuspensionScope::All
                    || state.scope
                        == match lobby_scope.kind {
                            LobbyKind::Open => SuspensionScope::Open,
                            LobbyKind::LowSkill => SuspensionScope::Lowskill,
                        }
            });
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = self
            .hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(call, player_id, lobby_scope);
        }
        Ok(result)
    }
}

fn persisted_record(service: &TestService, lobby_scope: LobbyScope) -> PersistedLobbyState {
    service
        .lock_state()
        .lobbies
        .get(&lobby_scope)
        .expect("provisional lobby record")
        .to_persisted()
}

#[derive(Debug, Default)]
struct FakePreviews {
    calls: Mutex<Vec<(GuildId, i64)>>,
}

impl FirstGamePoolPreviewPort for FakePreviews {
    fn first_game_pool_previews(
        &self,
        guild_id: GuildId,
        seed_amount: i64,
    ) -> FirstGamePoolPreviews {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((guild_id, seed_amount));
        FirstGamePoolPreviews {
            open: 125,
            low_skill: 275,
        }
    }
}

fn assert_bonus_pool(kind: LobbyKind, expected_amount: i64) {
    let service = service(FakePlayers::default(), 12);
    let lobby = service
        .get_or_create_lobby(None, scope(kind))
        .expect("lobby created");
    let previews = FakePreviews::default();
    let embed = service.build_lobby_embed(&lobby, Some(&previews));
    let field = embed
        .fields
        .iter()
        .find(|field| field.name == "🎲 Bonus Pool")
        .expect("bonus pool field");
    assert_eq!(
        field.value,
        format!(
            "**{expected_amount} <:jopacoin:954159801049440297>** available if this lobby shuffles next."
        )
    );
    assert_eq!(
        *previews
            .calls
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec![(GUILD, DOTA_BET_SEED_AMOUNT)]
    );
}

#[test]
fn test_lobby_embed_uses_preview_for_its_lobby_kind_open_125() {
    assert_bonus_pool(LobbyKind::Open, 125);
}

#[test]
fn test_lobby_embed_uses_preview_for_its_lobby_kind_lowskill_275() {
    assert_bonus_pool(LobbyKind::LowSkill, 275);
}

#[test]
fn test_join_lobby_blocks_player_in_pending_match() {
    let pending = PendingMatchState {
        pending_match_id: 42,
        shuffle_message_jump_url: Some("https://discord.com/jump/123".to_owned()),
    };
    let pending_port = FakePendingMatches::with_pending(pending.clone());
    let service = LobbyService::new(
        FakePlayers::default(),
        Some(pending_port),
        FakeClock::default(),
        10,
        12,
    );

    assert_eq!(
        service.join_lobby(UserId(12_345), scope(LobbyKind::Open)),
        JoinOutcome {
            success: false,
            failure: Some(JoinFailure::InPendingMatch),
            context: Some(JoinContext::PendingMatch(pending)),
        }
    );
    assert_eq!(service.get_lobby(scope(LobbyKind::Open)), None);
}

#[test]
fn test_join_lobby_allows_player_not_in_pending_match() {
    let service = service(FakePlayers::default(), 12);
    let outcome = service.join_lobby(UserId(12_345), scope(LobbyKind::Open));
    assert_joined(&outcome);
    assert_eq!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .expect("created lobby")
            .players,
        users(&[12_345])
    );
}

#[test]
fn test_lowskill_join_uses_strict_1400_cutoff_1399_99_true() {
    let players = FakePlayers::default();
    players.set_rating(UserId(1), GUILD, 1_399.99);
    let service = service(players, 12);
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)));
}

#[test]
fn test_lowskill_join_uses_strict_1400_cutoff_1400_false() {
    let players = FakePlayers::default();
    players.set_rating(UserId(1), GUILD, 1_400.0);
    let service = service(players, 12);
    assert_eq!(
        service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)),
        JoinOutcome::failed(JoinFailure::RatingTooHigh)
    );
    assert_eq!(service.get_lobby(scope(LobbyKind::LowSkill)), None);
}

#[test]
fn test_player_can_join_both_lobby_kinds() {
    let service = service(FakePlayers::default(), 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    service
        .get_or_create_lobby(Some(UserId(1)), scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)));
    assert_eq!(
        service.get_lobby_kinds_for_player(UserId(1), GUILD),
        vec![LobbyKind::Open, LobbyKind::LowSkill]
    );
}

#[test]
fn test_lowskill_eligibility_still_gates_the_second_queue() {
    let players = FakePlayers::default();
    players.set_rating(UserId(1), GUILD, 1_400.0);
    let service = service(players, 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    service
        .get_or_create_lobby(Some(UserId(2)), scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_eq!(
        service
            .join_lobby(UserId(1), scope(LobbyKind::LowSkill))
            .failure,
        Some(JoinFailure::RatingTooHigh)
    );
    assert_eq!(
        service
            .get_lobby(scope(LobbyKind::LowSkill))
            .expect("lobby remains")
            .players,
        BTreeSet::new()
    );
    assert_eq!(
        service.get_lobby_kinds_for_player(UserId(1), GUILD),
        vec![LobbyKind::Open]
    );
}

#[test]
fn test_get_lobby_kinds_for_player_lists_every_seat_in_declaration_order() {
    let service = service(FakePlayers::default(), 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    service
        .get_or_create_lobby(Some(UserId(1)), scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    assert!(
        service
            .get_lobby_kinds_for_player(UserId(1), GUILD)
            .is_empty()
    );
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)));
    assert_joined(&service.join_lobby(UserId(2), scope(LobbyKind::LowSkill)));
    assert_joined(&service.join_lobby(UserId(2), scope(LobbyKind::Open)));
    assert_eq!(
        service.get_lobby_kinds_for_player(UserId(1), GUILD),
        vec![LobbyKind::Open, LobbyKind::LowSkill]
    );
    assert_eq!(
        service.get_lobby_kinds_for_player(UserId(2), GUILD),
        vec![LobbyKind::Open, LobbyKind::LowSkill]
    );
}

#[test]
fn test_reserved_player_cannot_leave_or_join_the_other_lobby_until_released() {
    let service = service(FakePlayers::default(), 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    service
        .get_or_create_lobby(Some(UserId(2)), scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert!(service.reserve_lobby_players(&users(&[1]), scope(LobbyKind::Open)));
    assert!(!service.leave_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_eq!(
        service
            .join_lobby(UserId(1), scope(LobbyKind::LowSkill))
            .failure,
        Some(JoinFailure::InFlight)
    );
    service.release_lobby_players(&users(&[1]), scope(LobbyKind::Open));
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)));
    assert!(service.leave_lobby(UserId(1), scope(LobbyKind::Open)));
}

#[test]
fn test_open_reservation_does_not_pin_the_lowskill_seat() {
    let service = service(FakePlayers::default(), 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    service
        .get_or_create_lobby(Some(UserId(1)), scope(LobbyKind::LowSkill))
        .expect("low-skill lobby");
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::LowSkill)));
    assert!(service.reserve_lobby_players(&users(&[1]), scope(LobbyKind::Open)));
    assert!(service.leave_lobby(UserId(1), scope(LobbyKind::LowSkill)));
    assert!(!service.leave_lobby(UserId(1), scope(LobbyKind::Open)));
}

#[test]
fn test_remove_players_from_lobby_drops_only_the_seated_ids_it_was_given() {
    let service = service(FakePlayers::default(), 12);
    for player_id in [1, 2, 3] {
        assert_joined(&service.join_lobby(UserId(player_id), scope(LobbyKind::Open)));
    }
    assert_eq!(
        service.remove_players_from_lobby(&users(&[1, 2, 4]), scope(LobbyKind::Open)),
        users(&[1, 2])
    );
    assert_eq!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .expect("lobby remains")
            .players,
        users(&[3])
    );
}

#[test]
fn test_remove_players_from_lobby_is_a_no_op_when_the_lobby_is_absent() {
    let service = service(FakePlayers::default(), 12);
    service
        .get_or_create_lobby(None, scope(LobbyKind::Open))
        .expect("open lobby");
    assert!(
        service
            .remove_players_from_lobby(&users(&[1, 2]), scope(LobbyKind::LowSkill))
            .is_empty()
    );
    assert_eq!(service.get_lobby(scope(LobbyKind::LowSkill)), None);
}

#[test]
fn test_remove_players_from_lobby_skips_players_its_own_match_reserved() {
    let service = service(FakePlayers::default(), 12);
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(2), scope(LobbyKind::Open)));
    assert!(service.reserve_lobby_players(&users(&[1]), scope(LobbyKind::Open)));
    assert_eq!(
        service.remove_players_from_lobby(&users(&[1, 2]), scope(LobbyKind::Open)),
        users(&[2])
    );
    assert_eq!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .expect("lobby remains")
            .players,
        users(&[1])
    );
}

#[test]
fn test_same_lobby_players_cannot_be_reserved_by_two_match_starts() {
    let service = service(FakePlayers::default(), 12);
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(2), scope(LobbyKind::Open)));
    let selected = users(&[1, 2]);
    assert!(service.reserve_lobby_players(&selected, scope(LobbyKind::Open)));
    assert!(!service.reserve_lobby_players(&selected, scope(LobbyKind::Open)));
    assert!(service.has_in_flight_lobby_players(scope(LobbyKind::Open)));
    service.release_lobby_players(&selected, scope(LobbyKind::Open));
    assert!(service.reserve_lobby_players(&selected, scope(LobbyKind::Open)));
}

#[test]
fn test_high_rated_player_can_view_but_not_create_lowskill_lobby() {
    let players = FakePlayers::default();
    players.set_rating(UserId(1), GUILD, 1_400.0);
    let service = service(players, 12);
    assert_eq!(
        service.get_or_create_lobby(Some(UserId(1)), scope(LobbyKind::LowSkill)),
        Err(LobbyCreationError::RatingTooHigh)
    );
    service
        .get_or_create_lobby(Some(UserId(2)), scope(LobbyKind::LowSkill))
        .expect("eligible creator");
    assert_eq!(
        service
            .get_or_create_lobby(Some(UserId(1)), scope(LobbyKind::LowSkill))
            .expect("existing lobbies are viewable")
            .created_by,
        Some(UserId(2))
    );
}

#[test]
fn test_lobby_metadata_and_readychecks_are_isolated_by_kind() {
    let service = service(FakePlayers::default(), 12);
    let open = scope(LobbyKind::Open);
    let low_skill = scope(LobbyKind::LowSkill);
    service.get_or_create_lobby(None, open).expect("open lobby");
    service
        .get_or_create_lobby(None, low_skill)
        .expect("low-skill lobby");
    assert!(service.set_lobby_message_ids(
        open,
        LobbyMessageIds {
            message_id: Some(MessageId(101)),
            channel_id: Some(ChannelId(201)),
            ..LobbyMessageIds::default()
        }
    ));
    assert!(service.set_lobby_message_ids(
        low_skill,
        LobbyMessageIds {
            message_id: Some(MessageId(102)),
            channel_id: Some(ChannelId(202)),
            ..LobbyMessageIds::default()
        }
    ));
    assert!(service.set_readycheck_state(
        open,
        MessageId(301),
        ChannelId(401),
        users(&[1]),
        BTreeMap::new(),
    ));
    assert!(service.set_readycheck_state(
        low_skill,
        MessageId(302),
        ChannelId(402),
        users(&[2]),
        BTreeMap::new(),
    ));
    assert_eq!(service.get_lobby_message_id(open), Some(MessageId(101)));
    assert_eq!(
        service.get_lobby_message_id(low_skill),
        Some(MessageId(102))
    );
    assert_eq!(
        service.get_readycheck_message_id(open),
        Some(MessageId(301))
    );
    assert_eq!(
        service.get_readycheck_message_id(low_skill),
        Some(MessageId(302))
    );
    assert_eq!(
        service.get_readycheck_channel_id(low_skill),
        Some(ChannelId(402))
    );
}

#[test]
fn test_resetting_one_lobby_kind_leaves_the_other_open() {
    let service = service(FakePlayers::default(), 12);
    let open = scope(LobbyKind::Open);
    let low_skill = scope(LobbyKind::LowSkill);
    service.get_or_create_lobby(None, open).expect("open lobby");
    service
        .get_or_create_lobby(None, low_skill)
        .expect("low-skill lobby");
    assert!(service.reset_lobby(low_skill));
    assert_eq!(service.get_lobby(low_skill), None);
    assert!(service.get_lobby(open).is_some());
}

#[test]
fn test_join_lobby_without_match_state_service_works() {
    let service: TestService =
        LobbyService::new(FakePlayers::default(), None, FakeClock::default(), 10, 12);
    assert_joined(&service.join_lobby(UserId(12_345), scope(LobbyKind::Open)));
}

#[test]
fn test_join_lobby_returns_lobby_full_reason() {
    let service = service(FakePlayers::default(), 1);
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_eq!(
        service.join_lobby(UserId(2), scope(LobbyKind::Open)),
        JoinOutcome::failed(JoinFailure::LobbyFull)
    );
}

#[test]
fn test_join_lobby_returns_already_joined_reason() {
    let service = service(FakePlayers::default(), 12);
    assert_joined(&service.join_lobby(UserId(1), scope(LobbyKind::Open)));
    assert_eq!(
        service.join_lobby(UserId(1), scope(LobbyKind::Open)),
        JoinOutcome::failed(JoinFailure::AlreadyJoined)
    );
}

#[test]
fn suspension_gate_is_kind_scoped_and_preserves_exact_context() {
    for (suspension_scope, blocked) in [
        (
            SuspensionScope::All,
            BTreeSet::from([LobbyKind::Open, LobbyKind::LowSkill]),
        ),
        (SuspensionScope::Open, BTreeSet::from([LobbyKind::Open])),
        (
            SuspensionScope::Lowskill,
            BTreeSet::from([LobbyKind::LowSkill]),
        ),
    ] {
        let moderation = Arc::new(FakeModeration::fixed(suspension(suspension_scope)));
        let service = service(FakePlayers::default(), 12).with_moderation(moderation);
        for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
            let outcome = service.join_lobby(UserId(10), scope(kind));
            if blocked.contains(&kind) {
                assert_eq!(outcome.failure, Some(JoinFailure::Suspended));
                assert_eq!(
                    outcome.context,
                    Some(JoinContext::Suspension(Box::new(suspension(
                        suspension_scope
                    ))))
                );
                assert!(
                    service
                        .get_lobby(scope(kind))
                        .is_none_or(|lobby| !lobby.players.contains(&UserId(10)))
                );
            } else {
                assert_joined(&outcome);
                assert!(service.leave_lobby(UserId(10), scope(kind)));
            }
        }
    }
}

#[test]
fn suspended_creator_can_view_existing_lobby_but_cannot_create_a_new_one() {
    let moderation = Arc::new(FakeModeration::fixed(suspension(SuspensionScope::All)));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation);
    let open = scope(LobbyKind::Open);
    assert_eq!(
        service.get_or_create_lobby(Some(UserId(10)), open),
        Err(LobbyCreationError::Suspended(Box::new(suspension(
            SuspensionScope::All
        ))))
    );
    assert!(service.get_lobby(open).is_none());

    service
        .get_or_create_lobby(None, open)
        .expect("another caller creates the lobby");
    assert!(
        service.get_or_create_lobby(Some(UserId(10)), open).is_ok(),
        "Python allows suspended users to view an already-open lobby"
    );
}

#[test]
fn reservation_fails_closed_when_any_player_is_suspended() {
    let moderation = Arc::new(FakeModeration::default());
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());
    let open = scope(LobbyKind::Open);
    assert_joined(&service.join_lobby(UserId(10), open));
    *moderation
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(suspension(SuspensionScope::Open));
    assert!(!service.reserve_lobby_players(&users(&[10]), open));
    assert_eq!(
        service.get_in_flight_lobby_kind_for_player(UserId(10), GUILD),
        None
    );
}

#[test]
fn racing_suspension_is_rechecked_after_provisional_add_and_rolls_back() {
    let expected = suspension(SuspensionScope::All);
    let moderation = Arc::new(FakeModeration::scripted([
        Ok(None),
        Ok(Some(expected.clone())),
    ]));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());
    let open = scope(LobbyKind::Open);

    let outcome = service.join_lobby(UserId(10), open);

    assert_eq!(outcome, JoinOutcome::suspended(expected));
    assert!(service.get_lobby(open).is_none());
    assert_eq!(moderation.call_count(), 2);
}

#[test]
fn two_post_add_moderation_errors_fail_open_without_stranding_the_join() {
    let moderation = Arc::new(FakeModeration::scripted([
        Ok(None),
        Err("database is locked".to_owned()),
        Err("database is locked".to_owned()),
    ]));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());
    let open = scope(LobbyKind::Open);

    assert_joined(&service.join_lobby(UserId(10), open));
    assert!(
        service
            .get_lobby(open)
            .is_some_and(|lobby| lobby.players.contains(&UserId(10)))
    );
    assert_eq!(moderation.call_count(), 3);
}

#[test]
fn test_join_lobby_conditional_returns_deprecated_reason() {
    let service = service(FakePlayers::default(), 12);
    assert_eq!(
        service.join_lobby_conditional(),
        JoinOutcome::failed(JoinFailure::ConditionalJoinDeprecated)
    );
}

#[test]
fn test_readycheck_confirmation_snapshot_is_available_through_lobby_service() {
    let service = service(FakePlayers::default(), 12);
    let open = scope(LobbyKind::Open);
    service.get_or_create_lobby(None, open).expect("open lobby");
    assert!(service.set_readycheck_state(
        open,
        MessageId(111),
        ChannelId(222),
        users(&[1]),
        BTreeMap::new(),
    ));
    assert!(service.add_readycheck_reaction(open, UserId(1), "<@1>"));
    assert_eq!(
        service.get_readycheck_confirmation_snapshot(open),
        Some(ReadycheckConfirmationSnapshot {
            message_id: MessageId(111),
            reacted: users(&[1]),
        })
    );
}

#[test]
fn test_lobby_readycheck_snapshot_loads_players_from_atomic_manager_snapshot() {
    let players = FakePlayers::default();
    let observed_players = players.clone();
    let service = LobbyService::new(
        players,
        Some(FakePendingMatches::default()),
        FakeClock::from_values([1_000_000_000, 100_500_000_000, 200_500_000_000]),
        10,
        12,
    );
    let open = scope(LobbyKind::Open);
    service
        .get_or_create_lobby(Some(UserId(1)), open)
        .expect("open lobby");
    assert_joined(&service.join_lobby(UserId(1), open));
    assert_joined(&service.join_lobby(UserId(2), open));
    assert!(service.set_readycheck_state(
        open,
        MessageId(111),
        ChannelId(222),
        users(&[1, 2]),
        BTreeMap::new(),
    ));
    assert!(service.add_readycheck_reaction(open, UserId(1), "<@1>"));

    let snapshot = service
        .get_lobby_players_and_readycheck_snapshot(open)
        .expect("atomic snapshot");
    assert_eq!(snapshot.player_ids, vec![UserId(1), UserId(2)]);
    assert_eq!(
        snapshot
            .players
            .iter()
            .map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        snapshot.player_join_times,
        BTreeMap::from([(UserId(1), 100.5), (UserId(2), 200.5)])
    );
    assert_eq!(
        snapshot.readycheck,
        Some(ReadycheckConfirmationSnapshot {
            message_id: MessageId(111),
            reacted: users(&[1]),
        })
    );
    assert_eq!(
        observed_players.load_calls(),
        vec![(vec![UserId(1), UserId(2)], GUILD)]
    );
}

#[test]
fn test_join_lobby_success_carries_commit_ordered_watermark() {
    let service: LobbyService<FakePlayers, FakePendingMatches, SystemLobbyClock> =
        LobbyService::new(FakePlayers::default(), None, SystemLobbyClock, 10, 12);
    let open = scope(LobbyKind::Open);
    service.get_or_create_lobby(None, open).expect("open lobby");
    let clock = SystemLobbyClock;
    let before_ns = clock.now_ns();
    let first = service.join_lobby(UserId(1), open);
    let after_ns = clock.now_ns();
    let commit = assert_joined(&first);
    assert!(before_ns <= commit.joined_at_ns && commit.joined_at_ns <= after_ns);

    let second = service.join_lobby(UserId(1), open);
    assert!(!second.success);
    assert_eq!(second.join_commit(), None);
}

fn assert_scoped_suspension(suspension_scope: SuspensionScope, blocked: &[LobbyKind]) {
    let moderation = Arc::new(FakeModeration::fixed(suspension(suspension_scope)));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation);
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        let outcome = service.join_lobby(UserId(10), scope(kind));
        if blocked.contains(&kind) {
            assert_eq!(
                outcome,
                JoinOutcome::suspended(suspension(suspension_scope))
            );
            assert!(
                service
                    .get_lobby(scope(kind))
                    .is_none_or(|lobby| !lobby.players.contains(&UserId(10)))
            );
        } else {
            assert_joined(&outcome);
            assert!(service.leave_lobby(UserId(10), scope(kind)));
        }
    }
}

#[test]
fn test_manager_join_enforces_all_and_kind_scoped_suspensions_all() {
    assert_scoped_suspension(
        SuspensionScope::All,
        &[LobbyKind::Open, LobbyKind::LowSkill],
    );
}

#[test]
fn test_manager_join_enforces_all_and_kind_scoped_suspensions_open() {
    assert_scoped_suspension(SuspensionScope::Open, &[LobbyKind::Open]);
}

#[test]
fn test_manager_join_enforces_all_and_kind_scoped_suspensions_lowskill() {
    assert_scoped_suspension(SuspensionScope::Lowskill, &[LobbyKind::LowSkill]);
}

#[test]
fn test_lobby_service_returns_exact_suspension_context_from_locked_join() {
    let expected = suspension(SuspensionScope::Open);
    let service = service(FakePlayers::default(), 12)
        .with_moderation(Arc::new(FakeModeration::fixed(expected.clone())));

    let outcome = service.join_lobby(UserId(10), scope(LobbyKind::Open));

    assert_eq!(outcome.failure, Some(JoinFailure::Suspended));
    assert_eq!(
        outcome.context,
        Some(JoinContext::Suspension(Box::new(expected)))
    );
    assert!(service.get_lobby(scope(LobbyKind::Open)).is_none());
}

#[test]
fn test_suspended_creator_cannot_create_a_lobby() {
    let expected = suspension(SuspensionScope::All);
    let service = service(FakePlayers::default(), 12)
        .with_moderation(Arc::new(FakeModeration::fixed(expected.clone())));

    assert_eq!(
        service.get_or_create_lobby(Some(UserId(10)), scope(LobbyKind::Open)),
        Err(LobbyCreationError::Suspended(Box::new(expected)))
    );
    assert!(service.get_lobby(scope(LobbyKind::Open)).is_none());
}

#[test]
fn test_reservation_rechecks_suspensions_before_starting_future_match() {
    let moderation = Arc::new(FakeModeration::default());
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());
    let open = scope(LobbyKind::Open);
    assert_joined(&service.join_lobby(UserId(10), open));
    *moderation
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(suspension(SuspensionScope::Open));

    assert!(!service.reserve_lobby_players(&users(&[10]), open));
    assert_eq!(
        service.get_in_flight_lobby_kind_for_player(UserId(10), GUILD),
        None
    );
}

#[test]
fn test_suspension_does_not_revoke_an_existing_reservation() {
    let moderation = Arc::new(FakeModeration::default());
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());
    let open = scope(LobbyKind::Open);
    assert_joined(&service.join_lobby(UserId(10), open));
    assert!(service.reserve_lobby_players(&users(&[10]), open));
    *moderation
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(suspension(SuspensionScope::All));

    assert!(!service.reserve_lobby_players(&users(&[10]), open));
    assert_eq!(
        service.get_in_flight_lobby_kind_for_player(UserId(10), GUILD),
        Some(LobbyKind::Open)
    );
    service.release_lobby_players(&users(&[10]), open);
    assert_eq!(
        service.get_in_flight_lobby_kind_for_player(UserId(10), GUILD),
        None
    );
}

#[test]
fn test_join_rolls_back_when_suspension_lands_during_the_join() {
    let expected = suspension(SuspensionScope::All);
    let moderation = Arc::new(FakeModeration::scripted([
        Ok(None),
        Ok(Some(expected.clone())),
    ]));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());

    assert_eq!(
        service.join_lobby(UserId(10), scope(LobbyKind::Open)),
        JoinOutcome::suspended(expected)
    );
    assert!(service.get_lobby(scope(LobbyKind::Open)).is_none());
    assert_eq!(moderation.call_count(), 2);
}

#[test]
fn test_join_rollback_deletes_a_row_a_racing_persist_already_wrote() {
    let persistence = Arc::new(RecordingPersistence::default());
    let moderation = Arc::new(HookModeration::default());
    let service = Arc::new(
        service(FakePlayers::default(), 12)
            .with_persistence(persistence.clone())
            .expect("attach persistence")
            .with_moderation(moderation.clone()),
    );
    let weak_service: Weak<TestService> = Arc::downgrade(&service);
    let observed_rows = Arc::new(Mutex::new(Vec::new()));
    let observed_rows_for_hook = observed_rows.clone();
    let persistence_for_hook = persistence.clone();
    let moderation_for_hook = moderation.clone();
    moderation.set_hook(move |call, _, lobby_scope| {
        if call == 1 {
            moderation_for_hook.suspend(suspension(SuspensionScope::All));
        } else if call == 2 {
            let service = weak_service.upgrade().expect("service remains alive");
            persistence_for_hook
                .save_lobby(&persisted_record(&service, lobby_scope))
                .expect("racing persist");
            *observed_rows_for_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = persistence_for_hook.rows();
        }
    });

    assert_eq!(
        service
            .join_lobby(UserId(10), scope(LobbyKind::Open))
            .failure,
        Some(JoinFailure::Suspended)
    );
    assert!(
        observed_rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|row| row.players.contains(&10))
    );
    assert!(persistence.rows().is_empty());
    assert!(service.get_lobby(scope(LobbyKind::Open)).is_none());
}

#[test]
fn test_join_survives_a_failing_post_add_suspension_recheck() {
    let moderation = Arc::new(FakeModeration::scripted([
        Ok(None),
        Err("database is locked".to_owned()),
        Err("database is locked".to_owned()),
    ]));
    let service = service(FakePlayers::default(), 12).with_moderation(moderation.clone());

    assert_joined(&service.join_lobby(UserId(10), scope(LobbyKind::Open)));
    assert!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .is_some_and(|lobby| lobby.players.contains(&UserId(10)))
    );
    assert_eq!(moderation.call_count(), 3);
}

#[test]
fn test_join_rollback_preserves_a_lobby_others_already_occupy() {
    let moderation = Arc::new(HookModeration::default());
    let persistence = Arc::new(RecordingPersistence::default());
    let service = service(FakePlayers::default(), 12)
        .with_persistence(persistence.clone())
        .expect("attach persistence")
        .with_moderation(moderation.clone());
    assert_joined(&service.join_lobby(UserId(11), scope(LobbyKind::Open)));
    let moderation_for_hook = moderation.clone();
    moderation.set_hook(move |call, player_id, _| {
        if call == 3 && player_id == UserId(10) {
            moderation_for_hook.suspend(suspension(SuspensionScope::All));
        }
    });

    assert_eq!(
        service
            .join_lobby(UserId(10), scope(LobbyKind::Open))
            .failure,
        Some(JoinFailure::Suspended)
    );
    assert_eq!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .expect("occupied lobby remains")
            .players,
        users(&[11])
    );
    assert!(
        persistence
            .rows()
            .iter()
            .all(|row| !row.players.contains(&10))
    );
}

#[test]
fn test_join_rollback_leaves_reserved_player_seated() {
    let moderation = Arc::new(HookModeration::default());
    let service = Arc::new(service(FakePlayers::default(), 12).with_moderation(moderation.clone()));
    let weak_service = Arc::downgrade(&service);
    let moderation_for_hook = moderation.clone();
    moderation.set_hook(move |call, player_id, lobby_scope| {
        if call == 1 {
            let service = weak_service.upgrade().expect("service remains alive");
            service
                .lock_state()
                .in_flight
                .insert((lobby_scope.guild_id, player_id), lobby_scope.kind);
            moderation_for_hook.suspend(suspension(SuspensionScope::All));
        }
    });

    assert_joined(&service.join_lobby(UserId(10), scope(LobbyKind::Open)));
    assert!(
        service
            .get_lobby(scope(LobbyKind::Open))
            .is_some_and(|lobby| lobby.players.contains(&UserId(10)))
    );
}

#[test]
fn test_join_rollback_ignores_the_other_lobbys_reservation() {
    let moderation = Arc::new(HookModeration::default());
    let service = Arc::new(service(FakePlayers::default(), 12).with_moderation(moderation.clone()));
    let weak_service = Arc::downgrade(&service);
    let moderation_for_hook = moderation.clone();
    moderation.set_hook(move |call, player_id, lobby_scope| {
        if call == 1 {
            moderation_for_hook.suspend(suspension(SuspensionScope::All));
        } else if call == 2 {
            weak_service
                .upgrade()
                .expect("service remains alive")
                .lock_state()
                .in_flight
                .insert((lobby_scope.guild_id, player_id), LobbyKind::Open);
        }
    });

    assert_eq!(
        service
            .join_lobby(UserId(10), scope(LobbyKind::LowSkill))
            .failure,
        Some(JoinFailure::Suspended)
    );
    assert!(
        service
            .get_lobby(scope(LobbyKind::LowSkill))
            .is_none_or(|lobby| !lobby.players.contains(&UserId(10)))
    );
}

#[test]
fn test_durable_writes_never_hold_the_reader_lock() {
    let persistence = Arc::new(HookPersistence::default());
    let service = Arc::new(
        service(FakePlayers::default(), 12)
            .with_persistence(persistence.clone())
            .expect("hydrate hook persistence"),
    );
    let weak_service = Arc::downgrade(&service);
    persistence.set_hook(move || {
        let service = weak_service.upgrade().expect("service remains alive");
        assert!(
            service.state.try_lock().is_ok(),
            "a durable write ran while the lobby-state lock was held: every \
             reader on an async worker would wait on SQLite"
        );
        assert!(
            service.writes.try_lock().is_err(),
            "a durable write must hold the writer lock so no update is lost"
        );
    });

    assert_joined(&service.join_lobby(UserId(10), scope(LobbyKind::Open)));
    assert_joined(&service.join_lobby(UserId(11), scope(LobbyKind::Open)));
    assert!(
        service
            .try_set_lobby_message_ids(scope(LobbyKind::Open), LobbyMessageIds::default())
            .expect("set message ids")
    );
    assert_eq!(
        service
            .try_set_player_join_times(scope(LobbyKind::Open), &BTreeMap::from([(UserId(10), 5.0)]))
            .expect("set join times"),
        1
    );
    assert_eq!(
        service
            .try_remove_players_from_lobby(&users(&[11]), scope(LobbyKind::Open))
            .expect("remove players"),
        users(&[11])
    );
    assert!(
        service
            .try_leave_lobby(UserId(10), scope(LobbyKind::Open))
            .expect("leave lobby")
    );
    assert!(
        service
            .try_reset_lobby(scope(LobbyKind::Open))
            .expect("reset lobby")
    );
}

#[test]
fn test_a_concurrent_readycheck_survives_a_durable_write() {
    let persistence = Arc::new(HookPersistence::default());
    let service = Arc::new(
        service(FakePlayers::default(), 12)
            .with_persistence(persistence.clone())
            .expect("hydrate hook persistence"),
    );
    assert_joined(&service.join_lobby(UserId(10), scope(LobbyKind::Open)));
    let weak_service = Arc::downgrade(&service);
    persistence.set_hook(move || {
        // Stand in for a ready-check mutation landing while the durable write
        // is in flight: it touches only in-memory state, so publishing the
        // persisted record must not roll it back.
        let service = weak_service.upgrade().expect("service remains alive");
        // try_lock, not lock: a durable write that still held the state lock
        // would deadlock here instead of failing the assertion.
        let mut state = service
            .state
            .try_lock()
            .expect("the state lock is free during a durable write");
        if let Some(lobby) = state.lobbies.get_mut(&scope(LobbyKind::Open)) {
            lobby
                .readycheck
                .reacted
                .insert(UserId(10), "yes".to_owned());
        }
    });

    assert!(
        service
            .try_set_lobby_message_ids(scope(LobbyKind::Open), LobbyMessageIds::default())
            .expect("set message ids")
    );

    assert_eq!(
        service
            .lock_state()
            .lobbies
            .get(&scope(LobbyKind::Open))
            .expect("lobby remains")
            .readycheck
            .reacted
            .len(),
        1,
        "the publish preserved the in-memory ready-check reaction"
    );
}

#[test]
fn test_moderation_queries_run_outside_manager_state_lock() {
    let moderation = Arc::new(HookModeration::default());
    let service = Arc::new(service(FakePlayers::default(), 12).with_moderation(moderation.clone()));
    let weak_service = Arc::downgrade(&service);
    moderation.set_hook(move |_, _, _| {
        let service = weak_service.upgrade().expect("service remains alive");
        assert!(
            service.state.try_lock().is_ok(),
            "moderation I/O ran while the lobby-state lock was held"
        );
        assert!(
            service.writes.try_lock().is_ok(),
            "moderation I/O ran while the durable-write lock was held"
        );
    });

    assert_joined(&service.join_lobby(UserId(10), scope(LobbyKind::Open)));
    assert!(service.reserve_lobby_players(&users(&[10]), scope(LobbyKind::Open)));
    service.release_lobby_players(&users(&[10]), scope(LobbyKind::Open));
    moderation.suspend(suspension(SuspensionScope::All));
    assert_eq!(
        service.get_or_create_lobby(Some(UserId(10)), scope(LobbyKind::LowSkill)),
        Err(LobbyCreationError::Suspended(Box::new(suspension(
            SuspensionScope::All
        ))))
    );
    assert!(moderation.calls.load(Ordering::SeqCst) >= 4);
}
