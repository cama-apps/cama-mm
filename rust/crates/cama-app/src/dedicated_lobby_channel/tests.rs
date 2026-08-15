use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::*;

const GUILD: GuildId = GuildId(1);
const OTHER_GUILD: GuildId = GuildId(2);
const INTERACTION_CHANNEL: ChannelId = ChannelId(200);
const DEDICATED_CHANNEL: ChannelId = ChannelId(100);

#[derive(Clone, Debug, Default)]
struct SharedStore(Rc<RefCell<StoreData>>);

#[derive(Clone, Debug, Default)]
struct StoreData {
    rows: BTreeMap<LobbyScope, LobbyRecord>,
    fail_save: bool,
    fail_delete: bool,
}

impl LobbyStateStore for SharedStore {
    fn load_all(&self) -> Result<Vec<LobbyRecord>, LobbyStoreError> {
        Ok(self.0.borrow().rows.values().cloned().collect())
    }

    fn save(&mut self, record: &LobbyRecord) -> Result<(), LobbyStoreError> {
        if self.0.borrow().fail_save {
            return Err(LobbyStoreError::Backend("save failed".to_owned()));
        }
        self.0
            .borrow_mut()
            .rows
            .insert(record.scope, record.clone());
        Ok(())
    }

    fn delete(&mut self, scope: LobbyScope) -> Result<(), LobbyStoreError> {
        if self.0.borrow().fail_delete {
            return Err(LobbyStoreError::Backend("delete failed".to_owned()));
        }
        self.0.borrow_mut().rows.remove(&scope);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingPort {
    cached: BTreeMap<ChannelId, DiscordChannel>,
    fetched: BTreeMap<ChannelId, DiscordChannel>,
    fetch_fails: bool,
    fetch_calls: Vec<ChannelId>,
    next_message_id: i64,
    lobby_publications: Vec<(ChannelId, LobbyKind)>,
    reaction_result: Option<Result<Vec<Subscriber>, TransportError>>,
    persistent_result: Option<Result<Vec<Subscriber>, TransportError>>,
    rally_publications: Vec<(ChannelId, Option<ChannelId>, RallyPayload)>,
    target_send_fails: bool,
    thread_send_fails: bool,
    shuffle_publications: Vec<ChannelId>,
    partial_edits: Vec<(ChannelId, MessageId, ClosedLobbyEmbed, bool)>,
    archived_threads: Vec<(ChannelId, String)>,
    unpins: Vec<(ChannelId, Option<MessageId>)>,
    ephemeral_responses: Vec<String>,
}

impl ChannelDirectory for RecordingPort {
    fn cached_channel(&self, channel_id: ChannelId) -> Option<DiscordChannel> {
        self.cached.get(&channel_id).cloned()
    }

    fn fetch_channel(
        &mut self,
        channel_id: ChannelId,
    ) -> Result<Option<DiscordChannel>, TransportError> {
        self.fetch_calls.push(channel_id);
        if self.fetch_fails {
            return Err(TransportError::discord("fetch failed"));
        }
        Ok(self.fetched.get(&channel_id).cloned())
    }
}

impl LobbyPublicationPort for RecordingPort {
    fn publish_lobby_embed(
        &mut self,
        channel_id: ChannelId,
        kind: LobbyKind,
    ) -> Result<MessageId, TransportError> {
        self.lobby_publications.push((channel_id, kind));
        self.next_message_id += 1;
        Ok(MessageId(self.next_message_id))
    }
}

impl RallyPublicationPort for RecordingPort {
    fn reaction_subscribers(
        &mut self,
        _channel_id: ChannelId,
        _message_id: MessageId,
    ) -> Result<Vec<Subscriber>, TransportError> {
        self.reaction_result.clone().unwrap_or(Ok(Vec::new()))
    }

    fn persistent_subscribers(
        &mut self,
        _scope: LobbyScope,
    ) -> Result<Vec<Subscriber>, TransportError> {
        self.persistent_result.clone().unwrap_or(Ok(Vec::new()))
    }

    fn publish_concurrently(
        &mut self,
        target_channel_id: ChannelId,
        thread_channel_id: Option<ChannelId>,
        payload: &RallyPayload,
    ) -> ParallelSendResult {
        self.rally_publications
            .push((target_channel_id, thread_channel_id, payload.clone()));
        ParallelSendResult {
            target: if self.target_send_fails {
                Err(TransportError::discord("target failed"))
            } else {
                Ok(())
            },
            thread: thread_channel_id.map(|_| {
                if self.thread_send_fails {
                    Err(TransportError::discord("thread failed"))
                } else {
                    Ok(())
                }
            }),
        }
    }
}

impl ShufflePublicationPort for RecordingPort {
    fn publish_shuffle(&mut self, channel_id: ChannelId) -> Result<MessageId, TransportError> {
        self.shuffle_publications.push(channel_id);
        self.next_message_id += 1;
        Ok(MessageId(self.next_message_id))
    }
}

impl ResetLobbyPort for RecordingPort {
    fn edit_partial_message(
        &mut self,
        channel_id: ChannelId,
        message_id: MessageId,
        embed: ClosedLobbyEmbed,
        clear_view: bool,
    ) -> Result<(), TransportError> {
        self.partial_edits
            .push((channel_id, message_id, embed, clear_view));
        Ok(())
    }

    fn archive_thread(&mut self, thread_id: ChannelId, reason: &str) -> Result<(), TransportError> {
        self.archived_threads.push((thread_id, reason.to_owned()));
        Ok(())
    }

    fn unpin_message(
        &mut self,
        channel_id: ChannelId,
        message_id: Option<MessageId>,
    ) -> Result<(), TransportError> {
        self.unpins.push((channel_id, message_id));
        Ok(())
    }

    fn respond_ephemerally(&mut self, content: &str) -> Result<(), TransportError> {
        self.ephemeral_responses.push(content.to_owned());
        Ok(())
    }
}

fn scope() -> LobbyScope {
    LobbyScope::new(GUILD, LobbyKind::Open)
}

fn channel(id: ChannelId) -> DiscordChannel {
    DiscordChannel::text(id, GUILD)
}

fn interaction_context() -> InteractionChannelContext {
    InteractionChannelContext {
        channel: channel(INTERACTION_CHANNEL),
        guild_id: Some(GUILD),
    }
}

fn fresh_service() -> (LobbyService<SharedStore>, SharedStore) {
    let store = SharedStore::default();
    let manager = LobbyManager::new(store.clone()).unwrap();
    (LobbyService::new(manager), store)
}

fn create_lobby(service: &mut LobbyService<SharedStore>, selected_scope: LobbyScope) {
    service
        .get_or_create_lobby(selected_scope, UserId(12_345))
        .unwrap();
}

fn set_all_message_ids(service: &mut LobbyService<SharedStore>, selected_scope: LobbyScope) {
    service
        .set_lobby_message_id(
            selected_scope,
            LobbyMessageUpdate {
                message_id: Some(MessageId(111)),
                channel_id: Some(ChannelId(222)),
                thread_id: PersistedIdPatch::Set(ChannelId(333)),
                embed_message_id: PersistedIdPatch::Set(MessageId(444)),
                origin_channel_id: PersistedIdPatch::Set(ChannelId(555)),
            },
        )
        .unwrap();
}

fn subscriber(id: i64) -> Subscriber {
    Subscriber {
        user_id: UserId(id),
        mention: format!("<@{id}>"),
        is_bot: false,
        glicko_rating: None,
    }
}

fn rally_request() -> RallyRequest {
    RallyRequest {
        scope: scope(),
        reaction_channel: channel(ChannelId(111)),
        thread_channel_id: Some(ChannelId(333)),
        lobby_players: BTreeSet::new(),
        total_players: 8,
        ready_threshold: 10,
        message_ids: LobbyMessageIds::default(),
        bot_user_id: None,
        now: 1_000,
        cooldown_seconds: 120,
    }
}

fn reset_fixture() -> (
    LobbyService<SharedStore>,
    RecordingPort,
    RallyCooldowns,
    ResetLobbyRequest,
) {
    let (mut service, _) = fresh_service();
    create_lobby(&mut service, scope());
    service
        .set_lobby_message_id(
            scope(),
            LobbyMessageUpdate {
                message_id: Some(MessageId(321)),
                channel_id: Some(DEDICATED_CHANNEL),
                thread_id: PersistedIdPatch::Set(ChannelId(444)),
                ..LobbyMessageUpdate::default()
            },
        )
        .unwrap();
    let request = ResetLobbyRequest {
        guild_id: GUILD,
        memberships: vec![LobbyKind::Open],
        explicit_kind: None,
        interaction_channel: channel(INTERACTION_CHANNEL),
    };
    (
        service,
        RecordingPort::default(),
        RallyCooldowns::default(),
        request,
    )
}

mod test_origin_channel_id_storage {
    use super::*;

    #[test]
    fn test_set_origin_channel_id() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(111)),
                    channel_id: Some(ChannelId(222)),
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(333)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        assert_eq!(service.get_origin_channel_id(scope()), Some(ChannelId(333)));
    }

    #[test]
    fn test_origin_channel_id_defaults_to_none() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        assert_eq!(service.get_origin_channel_id(scope()), None);
    }

    #[test]
    fn test_origin_channel_id_not_overwritten_when_not_passed() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(111)),
                    channel_id: Some(ChannelId(222)),
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(333)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(444)),
                    channel_id: Some(ChannelId(555)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        assert_eq!(service.get_origin_channel_id(scope()), Some(ChannelId(333)));
    }

    #[test]
    fn test_reset_lobby_clears_origin_channel_id() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        set_all_message_ids(&mut service, scope());
        store.0.borrow_mut().fail_delete = true;
        assert!(service.reset_lobby(scope()).is_err());
        assert_eq!(
            service.get_origin_channel_id(scope()),
            Some(ChannelId(555)),
            "failed durable delete must not clear the live cache"
        );
        store.0.borrow_mut().fail_delete = false;
        service.reset_lobby(scope()).unwrap();
        assert_eq!(service.get_origin_channel_id(scope()), None);
    }
}

mod test_origin_channel_id_persistence {
    use super::*;

    #[test]
    fn test_origin_channel_id_persists_across_restart() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(111)),
                    channel_id: Some(ChannelId(222)),
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(333)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert_eq!(
            restarted.get_origin_channel_id(scope()),
            Some(ChannelId(333))
        );
    }

    #[test]
    fn test_origin_channel_id_persists_with_all_ids() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        set_all_message_ids(&mut service, scope());
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert_eq!(
            restarted.get_lobby_message_ids(scope()),
            Some(&LobbyMessageIds {
                message_id: Some(MessageId(111)),
                channel_id: Some(ChannelId(222)),
                thread_id: Some(ChannelId(333)),
                embed_message_id: Some(MessageId(444)),
                origin_channel_id: Some(ChannelId(555)),
            })
        );
    }

    #[test]
    fn test_origin_channel_id_cleared_after_reset_persists() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        set_all_message_ids(&mut service, scope());
        service.reset_lobby(scope()).unwrap();
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert!(restarted.get_lobby(scope()).is_none());
        assert_eq!(restarted.get_origin_channel_id(scope()), None);
    }
}

mod test_lobby_repository_origin_channel_id {
    use super::*;

    #[test]
    fn test_save_and_load_origin_channel_id() {
        let store = SharedStore::default();
        let mut repository = store.clone();
        let mut record = LobbyRecord::open(scope(), UserId(12_345));
        record.players.insert(UserId(1_002));
        record.message_ids.message_id = Some(MessageId(111));
        record.message_ids.channel_id = Some(ChannelId(222));
        record.message_ids.origin_channel_id = Some(ChannelId(555));
        repository.save(&record).unwrap();
        let loaded = store.load_all().unwrap().pop().unwrap();
        assert_eq!(loaded.message_ids.origin_channel_id, Some(ChannelId(555)));
        assert_eq!(loaded.message_ids.message_id, Some(MessageId(111)));
        assert_eq!(loaded.message_ids.channel_id, Some(ChannelId(222)));
    }

    #[test]
    fn test_origin_channel_id_defaults_to_none() {
        let mut store = SharedStore::default();
        store
            .save(&LobbyRecord::open(scope(), UserId(12_345)))
            .unwrap();
        assert_eq!(
            store.load_all().unwrap()[0].message_ids.origin_channel_id,
            None
        );
    }

    #[test]
    fn test_origin_channel_id_update() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(111)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        store.0.borrow_mut().fail_save = true;
        assert!(
            service
                .set_lobby_message_id(
                    scope(),
                    LobbyMessageUpdate {
                        origin_channel_id: PersistedIdPatch::Set(ChannelId(222)),
                        ..LobbyMessageUpdate::default()
                    },
                )
                .is_err()
        );
        assert_eq!(service.get_origin_channel_id(scope()), Some(ChannelId(111)));
        store.0.borrow_mut().fail_save = false;
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(222)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        let other_scope = LobbyScope::new(OTHER_GUILD, LobbyKind::Open);
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert_eq!(
            restarted.get_origin_channel_id(scope()),
            Some(ChannelId(222))
        );
        assert_eq!(restarted.get_origin_channel_id(other_scope), None);
    }
}

mod test_lobby_service_origin_channel_id {
    use super::*;

    #[test]
    fn test_get_origin_channel_id() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(111)),
                    channel_id: Some(ChannelId(222)),
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(333)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        assert_eq!(service.get_origin_channel_id(scope()), Some(ChannelId(333)));
    }

    #[test]
    fn test_get_origin_channel_id_returns_none_when_not_set() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        assert_eq!(service.get_origin_channel_id(scope()), None);
    }
}

mod test_get_lobby_target_channel_helper {
    use super::*;

    #[test]
    fn test_lowskill_lobby_uses_its_dedicated_channel() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(ChannelId(22_222), channel(ChannelId(22_222)));
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::LowSkill,
            LobbyChannelConfig {
                regular: Some(ChannelId(11_111)),
                low_skill: Some(ChannelId(22_222)),
            },
        );
        assert_eq!(resolved.channel.id, ChannelId(22_222));
    }

    #[test]
    fn test_lowskill_lobby_falls_back_to_regular_lobby_channel() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(ChannelId(11_111), channel(ChannelId(11_111)));
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::LowSkill,
            LobbyChannelConfig {
                regular: Some(ChannelId(11_111)),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, ChannelId(11_111));
        assert!(port.fetch_calls.is_empty());
    }

    #[test]
    fn test_returns_interaction_channel_when_no_config() {
        let mut port = RecordingPort::default();
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig::default(),
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::NotConfigured)
        );
    }

    #[test]
    fn test_returns_dedicated_channel_when_configured() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, DEDICATED_CHANNEL);
        assert_eq!(resolved.source, ChannelResolutionSource::DedicatedCache);
    }

    #[test]
    fn test_falls_back_on_permission_error() {
        let mut dedicated = channel(DEDICATED_CHANNEL);
        dedicated.permissions.send_messages = false;
        let mut port = RecordingPort::default();
        port.cached.insert(DEDICATED_CHANNEL, dedicated);
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::MissingSendPermission)
        );
    }

    #[test]
    fn test_falls_back_on_channel_not_found() {
        let mut port = RecordingPort::default();
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::NotFound)
        );
    }

    #[test]
    fn test_falls_back_on_different_guild() {
        let mut port = RecordingPort::default();
        port.cached.insert(
            DEDICATED_CHANNEL,
            DiscordChannel::text(DEDICATED_CHANNEL, OTHER_GUILD),
        );
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::DifferentGuild)
        );
    }
}

mod test_dedicated_lobby_channel_e2e {
    use super::*;

    #[test]
    fn test_full_flow_with_origin_channel() {
        let (mut service, store) = fresh_service();
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let published = publish_new_lobby(
            &mut service,
            &mut port,
            scope(),
            UserId(12_345),
            &interaction_context(),
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        )
        .unwrap();
        assert_eq!(published.lobby_channel_id, DEDICATED_CHANNEL);
        assert_eq!(published.origin_channel_id, INTERACTION_CHANNEL);
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert_eq!(
            restarted.get_lobby_message_ids(scope()).unwrap().channel_id,
            Some(DEDICATED_CHANNEL)
        );
        assert_eq!(
            restarted.get_origin_channel_id(scope()),
            Some(INTERACTION_CHANNEL)
        );
    }

    #[test]
    fn test_reset_clears_all_channel_ids() {
        let (mut service, _) = fresh_service();
        create_lobby(&mut service, scope());
        set_all_message_ids(&mut service, scope());
        service.reset_lobby(scope()).unwrap();
        assert!(service.get_lobby_message_ids(scope()).is_none());
    }

    #[test]
    fn test_origin_channel_same_as_lobby_channel_when_no_dedicated() {
        let (mut service, _) = fresh_service();
        let mut port = RecordingPort::default();
        let publication = publish_new_lobby(
            &mut service,
            &mut port,
            scope(),
            UserId(12_345),
            &interaction_context(),
            LobbyChannelConfig::default(),
        )
        .unwrap();
        assert_eq!(publication.lobby_channel_id, publication.origin_channel_id);
    }

    #[test]
    fn test_origin_channel_different_from_lobby_channel() {
        let (mut service, _) = fresh_service();
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let publication = publish_new_lobby(
            &mut service,
            &mut port,
            scope(),
            UserId(12_345),
            &interaction_context(),
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        )
        .unwrap();
        assert_ne!(publication.lobby_channel_id, publication.origin_channel_id);
        assert_eq!(publication.lobby_channel_id, DEDICATED_CHANNEL);
        assert_eq!(publication.origin_channel_id, INTERACTION_CHANNEL);
    }
}

mod test_config_env_var {
    use super::*;

    #[test]
    fn test_lobby_channel_id_parsing_valid_int() {
        assert_eq!(
            parse_configured_channel_id(Some("123456789")),
            Some(ChannelId(123_456_789))
        );
    }

    #[test]
    fn test_lobby_channel_id_parsing_none_when_empty() {
        assert_eq!(parse_configured_channel_id(Some("")), None);
    }

    #[test]
    fn test_lobby_channel_id_parsing_none_on_invalid() {
        assert_eq!(parse_configured_channel_id(Some("not-a-number")), None);
    }

    #[test]
    fn test_lobby_channel_id_parsing_strips_whitespace() {
        assert_eq!(
            parse_configured_channel_id(Some("  123456789  ")),
            Some(ChannelId(123_456_789))
        );
    }

    #[test]
    fn test_lowskill_lobby_channel_id_parsing_valid_int() {
        let config = LobbyChannelConfig::from_env_values(None, Some("  987654321  "));
        assert_eq!(
            config.configured_for(LobbyKind::LowSkill),
            Some(ChannelId(987_654_321))
        );
    }
}

mod test_get_lobby_target_channel_edge_cases {
    use super::*;

    #[test]
    fn test_fetch_channel_fallback_when_get_channel_returns_none() {
        let mut port = RecordingPort::default();
        port.fetched
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(port.fetch_calls, [DEDICATED_CHANNEL]);
        assert_eq!(resolved.source, ChannelResolutionSource::DedicatedFetch);
    }

    #[test]
    fn test_dm_context_guild_none() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let context = InteractionChannelContext {
            channel: DiscordChannel {
                guild_id: None,
                ..channel(INTERACTION_CHANNEL)
            },
            guild_id: None,
        };
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &context,
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, DEDICATED_CHANNEL);
    }

    #[test]
    fn test_falls_back_on_missing_thread_permission() {
        let mut dedicated = channel(DEDICATED_CHANNEL);
        dedicated.permissions.create_public_threads = false;
        let mut port = RecordingPort::default();
        port.cached.insert(DEDICATED_CHANNEL, dedicated);
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::MissingThreadPermission)
        );
    }

    #[test]
    fn test_falls_back_on_forbidden_exception() {
        let mut port = RecordingPort {
            fetch_fails: true,
            ..RecordingPort::default()
        };
        let resolved = resolve_lobby_target_channel(
            &mut port,
            &interaction_context(),
            LobbyKind::Open,
            LobbyChannelConfig {
                regular: Some(DEDICATED_CHANNEL),
                low_skill: None,
            },
        );
        assert_eq!(resolved.channel.id, INTERACTION_CHANNEL);
        assert_eq!(
            resolved.fallback_reason,
            Some(ChannelFallbackReason::TransportFailure)
        );
    }
}

mod test_notify_lobby_rally {
    use super::*;

    #[test]
    fn test_rally_uses_origin_channel_when_different() {
        let mut port = RecordingPort::default();
        port.cached.insert(ChannelId(222), channel(ChannelId(222)));
        let mut request = rally_request();
        request.message_ids.origin_channel_id = Some(ChannelId(222));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert!(report.sent);
        assert!(report.used_origin);
        assert_eq!(report.target_channel_id, ChannelId(222));
    }

    #[test]
    fn test_rally_falls_back_to_reaction_channel() {
        let mut port = RecordingPort::default();
        let request = rally_request();
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert!(report.sent);
        assert_eq!(report.target_channel_id, ChannelId(111));
        assert!(!report.used_origin);
    }

    #[test]
    fn test_rally_falls_back_when_origin_channel_fetch_fails() {
        let mut port = RecordingPort {
            fetch_fails: true,
            ..RecordingPort::default()
        };
        let mut request = rally_request();
        request.message_ids.origin_channel_id = Some(ChannelId(222));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert!(report.sent);
        assert_eq!(report.target_channel_id, ChannelId(111));
    }

    #[test]
    fn test_rally_same_channel_no_duplicate() {
        let mut port = RecordingPort::default();
        let mut request = rally_request();
        request.message_ids.origin_channel_id = Some(ChannelId(111));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert!(report.sent);
        assert_eq!(port.rally_publications.len(), 1);
        assert!(port.fetch_calls.is_empty());
    }

    #[test]
    fn test_rally_mentions_only_eligible_clipboard_subscribers() {
        let mut bot_subscriber = subscriber(13);
        bot_subscriber.is_bot = true;
        let mut port = RecordingPort {
            reaction_result: Some(Ok(vec![
                subscriber(10),
                subscriber(11),
                subscriber(12),
                bot_subscriber,
            ])),
            persistent_result: Some(Err(TransportError::local(
                "DatabaseError",
                "database unavailable",
            ))),
            ..RecordingPort::default()
        };
        let mut request = rally_request();
        request.lobby_players.insert(UserId(11));
        request.message_ids.message_id = Some(MessageId(999));
        request.message_ids.channel_id = Some(ChannelId(111));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert_eq!(report.allowed_mention_ids, [UserId(10), UserId(12)]);
        assert_eq!(
            port.rally_publications[0].2.content.as_deref(),
            Some("<@10> <@12>")
        );
    }

    #[test]
    fn test_rally_mentions_persistent_subscriber_without_lobby_message() {
        let mut port = RecordingPort {
            persistent_result: Some(Ok(vec![subscriber(10), subscriber(11), subscriber(999)])),
            ..RecordingPort::default()
        };
        let mut request = rally_request();
        request.lobby_players.insert(UserId(11));
        request.bot_user_id = Some(UserId(999));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert_eq!(report.allowed_mention_ids, [UserId(10)]);
        assert_eq!(
            port.rally_publications[0].2.content.as_deref(),
            Some("<@10>")
        );
    }

    #[test]
    fn test_lowskill_rally_excludes_auto_subscribers_at_or_above_1400() {
        let mut port = RecordingPort {
            persistent_result: Some(Ok(vec![
                Subscriber::persistent(UserId(10), Some(1_399)),
                Subscriber::persistent(UserId(11), Some(1_400)),
                Subscriber::persistent(UserId(13), Some(1_500)),
            ])),
            ..RecordingPort::default()
        };
        let mut request = rally_request();
        request.scope.kind = LobbyKind::LowSkill;
        request.lobby_players.insert(UserId(12));
        request.bot_user_id = Some(UserId(999));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert_eq!(report.allowed_mention_ids, [UserId(10)]);
    }

    #[test]
    fn test_rally_deduplicates_reaction_and_persistent_subscribers() {
        let mut port = RecordingPort {
            reaction_result: Some(Ok(vec![subscriber(10), subscriber(11)])),
            persistent_result: Some(Ok(vec![subscriber(10), subscriber(12), subscriber(13)])),
            ..RecordingPort::default()
        };
        let mut request = rally_request();
        request.lobby_players.insert(UserId(12));
        request.message_ids.message_id = Some(MessageId(222));
        request.message_ids.channel_id = Some(ChannelId(111));
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert_eq!(
            report.allowed_mention_ids,
            [UserId(10), UserId(11), UserId(13)]
        );
        assert_eq!(
            port.rally_publications[0].2.content.as_deref(),
            Some("<@10> <@11> <@13>")
        );
    }

    #[test]
    fn test_rally_cooldown_prevents_repeat_clipboard_ping() {
        let mut port = RecordingPort::default();
        let request = rally_request();
        let mut cooldowns = RallyCooldowns::default();
        let first = notify_lobby_rally(&mut port, &mut cooldowns, &request);
        let second = notify_lobby_rally(&mut port, &mut cooldowns, &request);
        assert!(first.sent);
        assert!(!second.sent);
        assert_eq!(port.rally_publications.len(), 1);
    }

    #[test]
    fn test_rally_target_and_thread_sends_overlap() {
        let mut port = RecordingPort::default();
        let request = rally_request();
        let report = notify_lobby_rally(&mut port, &mut RallyCooldowns::default(), &request);
        assert!(report.sent);
        assert!(report.thread_attempted);
        assert_eq!(
            port.rally_publications[0].1, request.thread_channel_id,
            "one publication boundary carries both sends for concurrent start"
        );
    }

    #[test]
    fn test_rally_target_failure_is_required_and_releases_cooldown() {
        let mut port = RecordingPort {
            target_send_fails: true,
            ..RecordingPort::default()
        };
        let request = rally_request();
        let mut cooldowns = RallyCooldowns::default();
        let report = notify_lobby_rally(&mut port, &mut cooldowns, &request);
        assert!(!report.sent);
        assert!(report.thread_attempted);
        assert!(!cooldowns.contains(scope(), 2));
    }

    #[test]
    fn test_rally_thread_failure_is_best_effort_and_keeps_cooldown() {
        let mut port = RecordingPort {
            thread_send_fails: true,
            ..RecordingPort::default()
        };
        let request = rally_request();
        let mut cooldowns = RallyCooldowns::default();
        let report = notify_lobby_rally(&mut port, &mut cooldowns, &request);
        assert!(report.sent);
        assert!(report.thread_failed);
        assert!(cooldowns.contains(scope(), 2));
    }
}

mod test_shuffle_dedicated_channel {
    use super::*;

    #[test]
    fn test_shuffle_posts_to_dedicated_channel() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let report = publish_shuffle_result(&mut port, scope(), Some(DEDICATED_CHANNEL), None);
        assert_eq!(report.attempted_channels, [DEDICATED_CHANNEL]);
        assert!(report.lobby_message_id.is_some());
    }

    #[test]
    fn test_shuffle_no_double_post_when_run_from_dedicated_channel() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let command = channel(DEDICATED_CHANNEL);
        let report =
            publish_shuffle_result(&mut port, scope(), Some(DEDICATED_CHANNEL), Some(&command));
        assert_eq!(report.attempted_channels, [DEDICATED_CHANNEL]);
        assert!(report.command_message_id.is_none());
    }

    #[test]
    fn test_shuffle_posts_to_both_channels_when_different() {
        let mut port = RecordingPort::default();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let command = channel(INTERACTION_CHANNEL);
        let report =
            publish_shuffle_result(&mut port, scope(), Some(DEDICATED_CHANNEL), Some(&command));
        assert_eq!(
            report.attempted_channels,
            [DEDICATED_CHANNEL, INTERACTION_CHANNEL]
        );
        assert!(report.lobby_message_id.is_some());
        assert!(report.command_message_id.is_some());
    }
}

mod test_reset_lobby_dedicated_channel {
    use super::*;

    #[test]
    fn test_resetlobby_unpins_from_dedicated_channel() {
        let (mut service, mut port, mut cooldowns, request) = reset_fixture();
        port.cached
            .insert(DEDICATED_CHANNEL, channel(DEDICATED_CHANNEL));
        let outcome = reset_lobby_command(&mut service, &mut port, &mut cooldowns, &request);
        assert_eq!(
            outcome,
            ResetLobbyOutcome::Reset {
                scope: scope(),
                unpin_channel_id: DEDICATED_CHANNEL,
            }
        );
        assert_eq!(port.unpins, [(DEDICATED_CHANNEL, Some(MessageId(321)))]);
        assert_eq!(port.partial_edits.len(), 1);
        assert!(port.partial_edits[0].3);
        assert!(service.get_lobby(scope()).is_none());
    }

    #[test]
    fn test_resetlobby_falls_back_to_interaction_channel() {
        let (mut service, mut port, mut cooldowns, request) = reset_fixture();
        port.fetch_fails = true;
        let outcome = reset_lobby_command(&mut service, &mut port, &mut cooldowns, &request);
        assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
        assert_eq!(port.fetch_calls, [DEDICATED_CHANNEL]);
        assert_eq!(port.unpins, [(INTERACTION_CHANNEL, Some(MessageId(321)))]);
    }

    #[test]
    fn test_resetlobby_uses_interaction_channel_when_no_dedicated() {
        let (mut service, mut port, mut cooldowns, request) = reset_fixture();
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    message_id: Some(MessageId(321)),
                    channel_id: None,
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        let outcome = reset_lobby_command(&mut service, &mut port, &mut cooldowns, &request);
        assert!(matches!(outcome, ResetLobbyOutcome::Reset { .. }));
        assert!(port.fetch_calls.is_empty());
        assert_eq!(port.unpins, [(INTERACTION_CHANNEL, Some(MessageId(321)))]);
    }

    #[test]
    fn test_resetlobby_without_option_refuses_when_queued_in_both() {
        let (mut service, mut port, mut cooldowns, mut request) = reset_fixture();
        request.memberships = vec![LobbyKind::Open, LobbyKind::LowSkill];
        let outcome = reset_lobby_command(&mut service, &mut port, &mut cooldowns, &request);
        assert_eq!(
            outcome,
            ResetLobbyOutcome::Refused(LobbySelectionError::Ambiguous {
                message: AMBIGUOUS_LOBBY_MESSAGE,
            })
        );
        assert_eq!(port.ephemeral_responses, [AMBIGUOUS_LOBBY_MESSAGE]);
        assert!(port.unpins.is_empty());
        assert!(service.get_lobby(scope()).is_some());
    }
}

#[derive(Default)]
struct RecordingSchema {
    columns: Vec<SchemaColumn>,
    additions: Vec<(String, String)>,
}

impl LobbySchemaPort for RecordingSchema {
    fn lobby_state_columns(&mut self) -> Result<Vec<SchemaColumn>, LobbyStoreError> {
        Ok(self.columns.clone())
    }

    fn add_nullable_integer_column(
        &mut self,
        table: &str,
        column: &str,
    ) -> Result<(), LobbyStoreError> {
        self.additions.push((table.to_owned(), column.to_owned()));
        self.columns.push(SchemaColumn {
            name: column.to_owned(),
            sql_type: "INTEGER".to_owned(),
            nullable: true,
        });
        Ok(())
    }
}

mod test_schema_migration {
    use super::*;

    #[test]
    fn test_migration_adds_origin_channel_id_column() {
        let mut schema = RecordingSchema::default();
        assert_eq!(
            migrate_origin_channel_id(&mut schema).unwrap(),
            OriginChannelMigration::Added
        );
        assert_eq!(
            schema.additions,
            [("lobby_state".to_owned(), "origin_channel_id".to_owned())]
        );
        assert_eq!(
            migrate_origin_channel_id(&mut schema).unwrap(),
            OriginChannelMigration::AlreadyPresent
        );
    }

    #[test]
    fn test_migration_allows_null_origin_channel_id() {
        let mut schema = RecordingSchema::default();
        migrate_origin_channel_id(&mut schema).unwrap();
        let origin = schema
            .columns
            .iter()
            .find(|column| column.name == "origin_channel_id")
            .unwrap();
        assert_eq!(origin.sql_type, "INTEGER");
        assert!(origin.nullable);
        assert_eq!(LobbyMessageIds::default().origin_channel_id, None);
    }

    #[test]
    fn test_migration_stores_origin_channel_id() {
        let (mut service, store) = fresh_service();
        create_lobby(&mut service, scope());
        service
            .set_lobby_message_id(
                scope(),
                LobbyMessageUpdate {
                    origin_channel_id: PersistedIdPatch::Set(ChannelId(999_888_777)),
                    ..LobbyMessageUpdate::default()
                },
            )
            .unwrap();
        let restarted = LobbyService::new(LobbyManager::new(store).unwrap());
        assert_eq!(
            restarted.get_origin_channel_id(scope()),
            Some(ChannelId(999_888_777))
        );
    }
}
