use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::rc::Rc;

use super::*;

const CREATED_AT: &str = "2026-07-22T12:00:00+00:00";

#[derive(Clone, Debug, Default)]
struct FakeStore {
    state: Rc<RefCell<FakeStoreState>>,
}

#[derive(Debug, Default)]
struct FakeStoreState {
    rows: BTreeMap<(i64, i64), PersistedLobbyState>,
    load_all_calls: usize,
    point_load_calls: usize,
    save_calls: usize,
    clear_calls: Vec<(i64, i64)>,
}

#[derive(Clone, Debug, Default)]
struct FailingClearStore(FakeStore);

impl ReadycheckLobbyStore for FailingClearStore {
    type Error = &'static str;

    fn save_lobby_state(&self, state: &PersistedLobbyState) -> Result<(), Self::Error> {
        self.0
            .save_lobby_state(state)
            .map_err(|error| match error {})
    }

    fn load_lobby_state(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PersistedLobbyState>, Self::Error> {
        self.0
            .load_lobby_state(lobby_id, guild_id)
            .map_err(|error| match error {})
    }

    fn load_all_lobby_states(&self) -> Result<Vec<PersistedLobbyState>, Self::Error> {
        self.0
            .load_all_lobby_states()
            .map_err(|error| match error {})
    }

    fn clear_lobby_state(
        &self,
        _lobby_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<bool, Self::Error> {
        Err("database is locked")
    }
}

impl FakeStore {
    fn seed(&self, state: PersistedLobbyState) {
        self.state
            .borrow_mut()
            .rows
            .insert((state.lobby_id, state.guild_id), state);
    }

    fn row(&self, lobby_id: i64, guild_id: i64) -> Option<PersistedLobbyState> {
        self.state.borrow().rows.get(&(lobby_id, guild_id)).cloned()
    }
}

impl ReadycheckLobbyStore for FakeStore {
    type Error = Infallible;

    fn save_lobby_state(&self, state: &PersistedLobbyState) -> Result<(), Self::Error> {
        let mut stored = self.state.borrow_mut();
        stored.save_calls += 1;
        stored
            .rows
            .insert((state.lobby_id, state.guild_id), state.clone());
        Ok(())
    }

    fn load_lobby_state(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PersistedLobbyState>, Self::Error> {
        let mut stored = self.state.borrow_mut();
        stored.point_load_calls += 1;
        Ok(stored
            .rows
            .get(&(lobby_id, guild_id.unwrap_or_default()))
            .cloned())
    }

    fn load_all_lobby_states(&self) -> Result<Vec<PersistedLobbyState>, Self::Error> {
        let mut stored = self.state.borrow_mut();
        stored.load_all_calls += 1;
        Ok(stored.rows.values().cloned().collect())
    }

    fn clear_lobby_state(&self, lobby_id: i64, guild_id: Option<i64>) -> Result<bool, Self::Error> {
        let guild_id = guild_id.unwrap_or_default();
        let mut stored = self.state.borrow_mut();
        stored.clear_calls.push((lobby_id, guild_id));
        Ok(stored.rows.remove(&(lobby_id, guild_id)).is_some())
    }
}

fn scope(guild_id: i64) -> LobbyScope {
    LobbyScope::new(GuildId(guild_id), LobbyKind::Open)
}

fn persisted(guild_id: i64, players: &[i64]) -> PersistedLobbyState {
    let mut state = PersistedLobbyState::open(1, Some(guild_id), 1_001, CREATED_AT);
    state.players = players.to_vec();
    state.player_join_times = players
        .iter()
        .enumerate()
        .map(|(index, player_id)| (*player_id, 100.5 + index as f64 * 100.0))
        .collect();
    state
}

fn manager() -> PersistentLobbyManager<FakeStore> {
    PersistentLobbyManager::new(FakeStore::default()).expect("fake store is infallible")
}

#[test]
fn test_lobby_creation() {
    let lobby = LobbyModel::open(scope(0), UserId(12_345), CREATED_AT);

    assert_eq!(lobby.scope.lobby_id(), 1);
    assert_eq!(lobby.scope.guild_id, GuildId::DEFAULT);
    assert_eq!(lobby.created_by, UserId(12_345));
    assert_eq!(lobby.created_at, CREATED_AT);
    assert_eq!(lobby.status, LobbyStatus::Open);
    assert!(lobby.players.is_empty());
    assert_eq!(lobby.total_count(), 0);
    assert_eq!(lobby.message_ids, LobbyMessageIds::default());
}

#[test]
fn test_legacy_conditional_players_are_ignored() {
    let mut state = persisted(0, &(1..10).collect::<Vec<_>>());
    state.conditional_players = vec![99];
    state.player_join_times = BTreeMap::from([(1, 100.0), (99, 200.0)]);

    let lobby = LobbyModel::from_persisted(state);

    assert_eq!(lobby.players, (1..10).map(UserId).collect());
    assert_eq!(lobby.conditional_player_count(), 0);
    assert_eq!(lobby.total_count(), 9);
    assert_eq!(
        lobby.player_join_times,
        BTreeMap::from([(UserId(1), 100.0)])
    );
    assert!(lobby.to_persisted().conditional_players.is_empty());
}

#[test]
fn test_add_and_remove_player() {
    let mut lobby = LobbyModel::open(scope(0), UserId(12_345), CREATED_AT);

    assert!(!lobby.remove_player(UserId(1_001)));
    assert!(lobby.add_player(UserId(1_001), 50.25));
    assert!(lobby.players.contains(&UserId(1_001)));
    assert_eq!(lobby.player_count(), 1);
    assert!(!lobby.add_player(UserId(1_001), 99.0));
    assert_eq!(lobby.player_join_times[&UserId(1_001)], 50.25);
    assert!(lobby.remove_player(UserId(1_001)));
    assert!(!lobby.players.contains(&UserId(1_001)));
    assert!(!lobby.player_join_times.contains_key(&UserId(1_001)));
    assert_eq!(lobby.player_count(), 0);

    lobby.close();
    assert!(!lobby.add_player(UserId(1_002), 100.0));
    assert!(!lobby.players.contains(&UserId(1_002)));
}

#[test]
fn test_is_ready() {
    let mut lobby = LobbyModel::open(scope(0), UserId(12_345), CREATED_AT);
    for player_id in 1_000..1_010 {
        assert!(lobby.add_player(UserId(player_id), player_id as f64));
    }

    assert_eq!(lobby.player_count(), READY_PLAYER_COUNT);
    assert!(lobby.is_ready());
    assert!(!lobby.is_ready_with_minimum(12));
}

#[test]
fn test_can_create_teams() {
    let mut lobby = LobbyModel::open(scope(0), UserId(12_345), CREATED_AT);
    let role_cycle = [
        LobbyRole::Carry,
        LobbyRole::Mid,
        LobbyRole::Offlane,
        LobbyRole::SoftSupport,
        LobbyRole::HardSupport,
    ];
    let mut balanced_roles = BTreeMap::new();
    for (index, player_id) in (1_000..1_010).enumerate() {
        let player_id = UserId(player_id);
        assert!(lobby.add_player(player_id, index as f64));
        balanced_roles.insert(player_id, vec![role_cycle[index % role_cycle.len()]]);
    }
    assert!(lobby.can_create_teams(&balanced_roles));

    let all_carry_roles = lobby
        .players
        .iter()
        .map(|player_id| (*player_id, vec![LobbyRole::Carry]))
        .collect();
    assert!(!lobby.can_create_teams(&all_carry_roles));
}

#[test]
fn test_get_or_create_and_get_lobby_lifecycle() {
    let mut manager = manager();
    let target = scope(0);
    assert!(manager.get_lobby(target).is_none());

    let created = manager
        .get_or_create_lobby_at(target, Some(UserId(12_345)), CREATED_AT)
        .expect("fake store is infallible")
        .clone();
    assert_eq!(created.created_by, UserId(12_345));
    assert_eq!(manager.get_lobby(target), Some(&created));
    assert_eq!(manager.store().state.borrow().save_calls, 1);

    manager
        .get_or_create_lobby_at(target, Some(UserId(99)), "later")
        .expect("existing lobby requires no write");
    assert_eq!(manager.store().state.borrow().save_calls, 1);

    manager
        .lobbies
        .get_mut(&target)
        .expect("lobby exists")
        .close();
    assert!(manager.get_lobby(target).is_none());
}

#[test]
fn test_join_and_leave_lobby() {
    let mut manager = manager();
    let target = scope(0);
    assert!(
        !manager
            .leave_lobby(UserId(1_001), target)
            .expect("fake store is infallible")
    );

    assert_eq!(
        manager
            .join_lobby_at(UserId(1_001), target, 1.0)
            .expect("fake store is infallible"),
        LobbyJoinOutcome::Ok
    );
    assert!(
        manager
            .get_lobby(target)
            .expect("join creates a lobby")
            .players
            .contains(&UserId(1_001))
    );
    assert!(
        manager
            .leave_lobby(UserId(1_001), target)
            .expect("fake store is infallible")
    );
    assert!(
        !manager
            .leave_lobby(UserId(1_001), target)
            .expect("fake store is infallible")
    );

    for player_id in 2_000..2_012 {
        assert_eq!(
            manager
                .join_lobby_at(UserId(player_id), target, player_id as f64)
                .expect("fake store is infallible"),
            LobbyJoinOutcome::Ok
        );
    }
    let saves_before_full_join = manager.store().state.borrow().save_calls;
    assert_eq!(
        manager
            .join_lobby_at(UserId(9_999), target, 9_999.0)
            .expect("fake store is infallible"),
        LobbyJoinOutcome::Full
    );
    let lobby = manager.get_lobby(target).expect("lobby remains open");
    assert!(!lobby.players.contains(&UserId(9_999)));
    assert_eq!(lobby.player_count(), MAX_LOBBY_PLAYERS);
    assert_eq!(
        manager.store().state.borrow().save_calls,
        saves_before_full_join
    );
}

#[test]
fn test_reset_lobby() {
    let store = FakeStore::default();
    let mut manager = PersistentLobbyManager::new(store.clone()).expect("fake store is infallible");
    let target = scope(0);
    manager
        .get_or_create_lobby_at(target, None, CREATED_AT)
        .expect("fake store is infallible");
    manager
        .join_lobby_at(UserId(1_001), target, 1.0)
        .expect("fake store is infallible");
    manager
        .set_lobby_message(
            target,
            LobbyMessageIds {
                message_id: Some(MessageId(12_345)),
                channel_id: Some(ChannelId(67_890)),
                ..LobbyMessageIds::default()
            },
        )
        .expect("fake store is infallible");

    manager
        .reset_lobby(target)
        .expect("fake store is infallible");

    assert!(manager.get_lobby(target).is_none());
    assert!(manager.lobby_message_ids(target).is_none());
    assert!(store.row(1, 0).is_none());
    assert_eq!(store.state.borrow().clear_calls, vec![(1, 0)]);
}

#[test]
fn test_fake_load_all_returns_full_shallow_copies() {
    let store = FakeStore::default();
    let mut state = persisted(42, &[101, 102]);
    state.message_id = Some(2_001);
    state.channel_id = Some(2_002);
    state.thread_id = Some(2_003);
    state.embed_message_id = Some(2_004);
    state.origin_channel_id = Some(2_005);
    store
        .save_lobby_state(&state)
        .expect("fake store is infallible");

    let mut rows = store
        .load_all_lobby_states()
        .expect("fake store is infallible");
    assert_eq!(rows, vec![state.clone()]);
    rows[0].status = "closed".to_owned();
    rows[0].players.clear();

    assert_eq!(store.row(1, 42), Some(state));
}

#[test]
fn test_startup_bulk_rehydrates_all_guild_metadata_without_point_reads() {
    let store = FakeStore::default();
    let mut first = persisted(42, &[101, 102]);
    first.message_id = Some(2_001);
    first.channel_id = Some(2_002);
    first.thread_id = Some(2_003);
    first.embed_message_id = Some(2_004);
    first.origin_channel_id = Some(2_005);
    let mut second = persisted(84, &[201, 202, 203]);
    second.created_by = 3_001;
    second.created_at = "2026-07-22T13:00:00+00:00".to_owned();
    second.message_id = Some(4_001);
    second.channel_id = Some(4_002);
    second.thread_id = Some(4_003);
    second.embed_message_id = Some(4_004);
    second.origin_channel_id = Some(4_005);
    store.seed(first.clone());
    store.seed(second.clone());

    let manager = PersistentLobbyManager::new(store.clone()).expect("fake store is infallible");

    let counters = store.state.borrow();
    assert_eq!(counters.load_all_calls, 1);
    assert_eq!(counters.point_load_calls, 0);
    assert_eq!(counters.save_calls, 0);
    drop(counters);

    for expected in [first, second] {
        let target = scope(expected.guild_id);
        let lobby = manager
            .get_lobby(target)
            .expect("open bulk row is restored");
        assert_eq!(lobby.scope.lobby_id(), expected.lobby_id);
        assert_eq!(lobby.scope.guild_id, GuildId(expected.guild_id));
        assert_eq!(
            lobby.players,
            expected.players.iter().copied().map(UserId).collect()
        );
        assert_eq!(lobby.status, LobbyStatus::Open);
        assert_eq!(lobby.created_by, UserId(expected.created_by));
        assert_eq!(lobby.created_at, expected.created_at);
        assert_eq!(
            lobby.message_ids.message_id,
            expected.message_id.map(MessageId)
        );
        assert_eq!(
            lobby.message_ids.channel_id,
            expected.channel_id.map(ChannelId)
        );
        assert_eq!(
            lobby.message_ids.thread_id,
            expected.thread_id.map(ChannelId)
        );
        assert_eq!(
            lobby.message_ids.embed_message_id,
            expected.embed_message_id.map(MessageId)
        );
        assert_eq!(
            lobby.message_ids.origin_channel_id,
            expected.origin_channel_id.map(ChannelId)
        );
        assert_eq!(
            lobby.player_join_times,
            expected
                .player_join_times
                .iter()
                .map(|(player_id, joined_at)| (UserId(*player_id), *joined_at))
                .collect()
        );
    }
}

#[test]
fn test_leave_lobby_removes_player_from_in_memory_state() {
    let store = FakeStore::default();
    let mut manager = PersistentLobbyManager::new(store.clone()).expect("fake store is infallible");
    let target = scope(0);
    for player_id in [101, 102, 103, 104] {
        assert_eq!(
            manager
                .join_lobby_at(UserId(player_id), target, player_id as f64)
                .expect("fake store is infallible"),
            LobbyJoinOutcome::Ok
        );
    }
    assert_eq!(
        manager.get_lobby(target).expect("lobby exists").players,
        [101, 102, 103, 104].into_iter().map(UserId).collect()
    );

    for player_id in [102, 104] {
        assert!(
            manager
                .leave_lobby(UserId(player_id), target)
                .expect("fake store is infallible")
        );
    }

    assert_eq!(
        manager.get_lobby(target).expect("lobby remains").players,
        [101, 103].into_iter().map(UserId).collect()
    );
    assert_eq!(
        store.row(1, 0).expect("prune is persisted").players,
        vec![101, 103]
    );
}

#[test]
fn test_conditional_lobby_join_is_deprecated() {
    let mut manager = manager();
    let target = scope(0);
    manager
        .join_lobby_at(UserId(1), target, 1.0)
        .expect("fake store is infallible");
    let saves_before_conditional = manager.store().state.borrow().save_calls;

    assert_eq!(
        manager.join_lobby_conditional(UserId(2), target),
        ConditionalLobbyJoinOutcome::Deprecated
    );
    let lobby = manager.get_lobby(target).expect("regular lobby exists");
    assert_eq!(lobby.players, BTreeSet::from([UserId(1)]));
    assert_eq!(lobby.conditional_player_count(), 0);
    assert_eq!(
        manager.store().state.borrow().save_calls,
        saves_before_conditional
    );
}

#[test]
fn test_prune_does_not_affect_other_guild() {
    let store = FakeStore::default();
    let mut manager = PersistentLobbyManager::new(store.clone()).expect("fake store is infallible");
    let guild_a = scope(100);
    let guild_b = scope(200);
    for target in [guild_a, guild_b] {
        manager
            .join_lobby_at(UserId(10), target, 10.0)
            .expect("fake store is infallible");
    }

    assert!(
        manager
            .leave_lobby(UserId(10), guild_a)
            .expect("fake store is infallible")
    );

    assert!(
        !manager
            .get_lobby(guild_a)
            .expect("empty lobby remains open")
            .players
            .contains(&UserId(10))
    );
    assert!(
        manager
            .get_lobby(guild_b)
            .expect("other guild remains open")
            .players
            .contains(&UserId(10))
    );
    assert!(
        store
            .row(1, 100)
            .expect("guild A persisted")
            .players
            .is_empty()
    );
    assert_eq!(
        store.row(1, 200).expect("guild B persisted").players,
        vec![10]
    );
}

#[test]
fn test_lobby_manager_persists_and_recovers_state() {
    let store = FakeStore::default();
    let target = scope(0);
    let mut first = PersistentLobbyManager::new(store.clone()).expect("first manager construction");
    first
        .get_or_create_lobby_at(target, Some(UserId(42)), CREATED_AT)
        .expect("create lobby");
    for (player_id, joined_at) in [(111, 111.0), (222, 222.0)] {
        assert_eq!(
            first
                .join_lobby_at(UserId(player_id), target, joined_at)
                .expect("persist join"),
            LobbyJoinOutcome::Ok
        );
    }
    drop(first);

    let restarted = PersistentLobbyManager::new(store).expect("restart manager");
    let loaded = restarted.get_lobby(target).expect("restored lobby");
    assert_eq!(loaded.players, BTreeSet::from([UserId(111), UserId(222)]));
    assert_eq!(loaded.created_by, UserId(42));
}

#[test]
fn test_lobby_kinds_persist_independently_in_same_guild() {
    let store = FakeStore::default();
    let open = LobbyScope::new(GuildId(77), LobbyKind::Open);
    let low_skill = LobbyScope::new(GuildId(77), LobbyKind::LowSkill);
    let mut first = PersistentLobbyManager::new(store.clone()).expect("first manager construction");
    first
        .get_or_create_lobby_at(open, Some(UserId(10)), CREATED_AT)
        .expect("create open lobby");
    first
        .get_or_create_lobby_at(low_skill, Some(UserId(20)), CREATED_AT)
        .expect("create low-skill lobby");
    first
        .join_lobby_at(UserId(101), open, 101.0)
        .expect("join open lobby");
    first
        .join_lobby_at(UserId(202), low_skill, 202.0)
        .expect("join low-skill lobby");
    drop(first);

    let restarted = PersistentLobbyManager::new(store).expect("restart manager");
    let restored_open = restarted.get_lobby(open).expect("restored open lobby");
    let restored_low = restarted
        .get_lobby(low_skill)
        .expect("restored low-skill lobby");
    assert_eq!(restored_open.players, BTreeSet::from([UserId(101)]));
    assert_eq!(restored_low.players, BTreeSet::from([UserId(202)]));
    assert_ne!(
        restored_open.scope.lobby_id(),
        restored_low.scope.lobby_id()
    );
}

#[test]
fn test_lobby_service_join_persists_players() {
    let store = FakeStore::default();
    let target = scope(0);
    let mut first = PersistentLobbyManager::new(store.clone()).expect("first manager construction");
    first
        .get_or_create_lobby_at(target, Some(UserId(100)), CREATED_AT)
        .expect("create lobby");
    for (player_id, joined_at) in [(111, 111.0), (222, 222.0), (333, 333.0)] {
        assert_eq!(
            first
                .join_lobby_at(UserId(player_id), target, joined_at)
                .expect("persist join"),
            LobbyJoinOutcome::Ok
        );
    }
    drop(first);

    let mut second = PersistentLobbyManager::new(store.clone()).expect("first restart manager");
    assert_eq!(
        second.get_lobby(target).expect("restored lobby").players,
        BTreeSet::from([UserId(111), UserId(222), UserId(333)])
    );
    assert_eq!(
        second
            .join_lobby_at(UserId(444), target, 444.0)
            .expect("join after restart"),
        LobbyJoinOutcome::Ok
    );
    drop(second);

    let third = PersistentLobbyManager::new(store).expect("second restart manager");
    assert_eq!(
        third.get_lobby(target).expect("restored lobby").players,
        BTreeSet::from([UserId(111), UserId(222), UserId(333), UserId(444)])
    );
}

#[test]
fn test_lobby_service_leave_persists() {
    let store = FakeStore::default();
    let target = scope(0);
    let mut first = PersistentLobbyManager::new(store.clone()).expect("first manager construction");
    for (player_id, joined_at) in [(111, 111.0), (222, 222.0), (333, 333.0)] {
        first
            .join_lobby_at(UserId(player_id), target, joined_at)
            .expect("persist join");
    }
    assert!(
        first
            .leave_lobby(UserId(222), target)
            .expect("persist leave")
    );
    drop(first);

    let restarted = PersistentLobbyManager::new(store).expect("restart manager");
    assert_eq!(
        restarted.get_lobby(target).expect("restored lobby").players,
        BTreeSet::from([UserId(111), UserId(333)])
    );
}

#[test]
fn test_lobby_message_id_persists() {
    let store = FakeStore::default();
    let target = scope(0);
    let mut first = PersistentLobbyManager::new(store.clone()).expect("first manager construction");
    first
        .get_or_create_lobby_at(target, Some(UserId(100)), CREATED_AT)
        .expect("create lobby");
    first
        .set_lobby_message(
            target,
            LobbyMessageIds {
                message_id: Some(MessageId(12_345_678)),
                channel_id: Some(ChannelId(87_654_321)),
                ..LobbyMessageIds::default()
            },
        )
        .expect("persist message identity");
    drop(first);

    let restarted = PersistentLobbyManager::new(store).expect("restart manager");
    let ids = restarted
        .lobby_message_ids(target)
        .expect("restored message identity");
    assert_eq!(ids.message_id, Some(MessageId(12_345_678)));
    assert_eq!(ids.channel_id, Some(ChannelId(87_654_321)));
}

#[test]
fn test_lobby_persistence_runs_outside_state_lock() {
    // Unlike Python's manager, this write-through manager has no process-wide
    // state mutex at all. Candidate rows are persisted before they replace the
    // in-memory snapshot, so a storage callback cannot run under such a lock.
    let store = FakeStore::default();
    let target = LobbyScope::new(GuildId(7), LobbyKind::Open);
    let mut manager = PersistentLobbyManager::new(store.clone()).expect("manager construction");
    manager
        .get_or_create_lobby_at(target, Some(UserId(42)), CREATED_AT)
        .expect("create lobby");
    manager
        .join_lobby_at(UserId(111), target, 111.0)
        .expect("persist join");
    manager
        .set_lobby_message(
            target,
            LobbyMessageIds {
                message_id: Some(MessageId(1)),
                channel_id: Some(ChannelId(2)),
                ..LobbyMessageIds::default()
            },
        )
        .expect("persist message");
    assert!(
        manager
            .leave_lobby(UserId(111), target)
            .expect("persist leave")
    );
    manager.reset_lobby(target).expect("persist reset");

    assert!(manager.get_lobby(target).is_none());
    let counters = store.state.borrow();
    assert_eq!(counters.save_calls, 4);
    assert_eq!(counters.clear_calls, [(target.lobby_id(), 7)]);
}

#[test]
fn test_reset_lobby_failed_db_delete_keeps_in_memory_state() {
    let store = FailingClearStore::default();
    let target = LobbyScope::new(GuildId(7), LobbyKind::Open);
    let mut manager = PersistentLobbyManager::new(store.clone()).expect("manager construction");
    manager
        .get_or_create_lobby_at(target, Some(UserId(42)), CREATED_AT)
        .expect("create lobby");
    manager
        .join_lobby_at(UserId(111), target, 111.0)
        .expect("persist join");

    assert_eq!(manager.reset_lobby(target), Err("database is locked"));
    assert!(
        manager
            .get_lobby(target)
            .expect("in-memory lobby survives")
            .players
            .contains(&UserId(111))
    );
    assert!(
        store
            .0
            .row(target.lobby_id(), 7)
            .expect("persisted row survives")
            .players
            .contains(&111)
    );
}
