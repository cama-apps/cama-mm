use super::*;
use crate::{
    discord_transport::{
        DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot,
    },
    dota_host_config::DotaHostConfig,
};
use cama_db::match_runtime::{
    DOTA_BETTING_CLOSED_MARKER, DOTA_BETTING_EXTENDED_UNTIL, PendingMatchState,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tempfile::NamedTempFile;

#[derive(Clone, Debug, PartialEq)]
struct ChannelPolicy {
    guild: u64,
    marker: String,
    participants: Vec<u64>,
    viewers: Vec<u64>,
    known: Option<u64>,
}
#[derive(Default)]
struct FakeDiscord {
    ensure_calls: Mutex<Vec<ChannelPolicy>>,
    channel: Mutex<Option<(u64, ChannelPolicy)>>,
    deleted: Mutex<Vec<(u64, u64, String)>>,
    attempts: Mutex<Vec<(u64, String, String)>>,
    delivered: Mutex<std::collections::BTreeMap<String, String>>,
    maps: Mutex<BTreeMap<String, DiscordMessage>>,
    map_edits: Mutex<Vec<DiscordMessage>>,
    lose_map_reply: AtomicBool,
    map_message_deleted: AtomicBool,
    render_names: Mutex<BTreeMap<(u64, u64), String>>,
    fail_name_cache: AtomicBool,
    member_http_calls: AtomicUsize,
    fail_ensure: AtomicBool,
    fail_audit: AtomicBool,
    lose_reply: AtomicBool,
    lose_create_reply: AtomicBool,
    public_calls: AtomicUsize,
    audit_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    audit_hook_after: AtomicUsize,
}
#[async_trait]
impl DiscordTransport for FakeDiscord {
    async fn ensure_spectator_channel(
        &self,
        guild: u64,
        marker: &str,
        _name: &str,
        participants: &[u64],
        viewers: &[u64],
        known: Option<u64>,
    ) -> Result<u64, String> {
        let policy = ChannelPolicy {
            guild,
            marker: marker.into(),
            participants: participants.to_vec(),
            viewers: viewers.to_vec(),
            known,
        };
        self.ensure_calls.lock().unwrap().push(policy.clone());
        if self.fail_ensure.load(Ordering::SeqCst) {
            return Err("participant is admin / permissions unavailable".into());
        }
        if let Some((id, old)) = &*self.channel.lock().unwrap()
            && (old.guild != guild
                || old.marker != marker
                || known.is_some_and(|known| known != *id))
        {
            return Err("foreign channel".into());
        }
        *self.channel.lock().unwrap() = Some((500, policy));
        if self.lose_create_reply.swap(false, Ordering::SeqCst) {
            return Err("create response lost after Discord accepted channel".into());
        }
        Ok(500)
    }
    async fn audit_spectator_channel(
        &self,
        guild: u64,
        marker: &str,
        participants: &[u64],
        viewers: &[u64],
        channel: u64,
    ) -> Result<(), String> {
        if self.fail_audit.load(Ordering::SeqCst) {
            return Err("permissions changed".into());
        }
        let saved = self.channel.lock().unwrap();
        let Some((id, policy)) = saved.as_ref() else {
            return Err("missing channel".into());
        };
        if *id != channel
            || policy.guild != guild
            || policy.marker != marker
            || policy.participants != participants
            || policy.viewers != viewers
        {
            return Err("unsafe channel".into());
        }
        let deferred = self
            .audit_hook_after
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if !deferred && let Some(hook) = self.audit_hook.lock().unwrap().take() {
            hook();
        }
        Ok(())
    }
    async fn delete_spectator_channels_by_marker(
        &self,
        guild: u64,
        marker: &str,
    ) -> Result<(), String> {
        let owned_id = self
            .channel
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|(id, policy)| {
                (policy.guild == guild && policy.marker == marker).then_some(*id)
            });
        if let Some(id) = owned_id {
            self.delete_spectator_channel(guild, id, marker).await?;
        }
        Ok(())
    }

    async fn delete_spectator_channel(
        &self,
        guild: u64,
        channel: u64,
        marker: &str,
    ) -> Result<(), String> {
        let mut current = self.channel.lock().unwrap();
        if let Some((id, policy)) = &*current
            && (*id != channel || policy.guild != guild || policy.marker != marker)
        {
            return Err("foreign channel".into());
        }
        self.deleted
            .lock()
            .unwrap()
            .push((guild, channel, marker.into()));
        *current = None;
        Ok(())
    }
    async fn send_message_with_delivery_key(
        &self,
        channel: u64,
        key: &str,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        assert_eq!(channel, 500, "live content escaped the private channel");
        assert!(key.len() <= 25);
        if !message.response.attachments.is_empty() {
            self.maps
                .lock()
                .unwrap()
                .entry(key.into())
                .or_insert(message);
            if self.lose_map_reply.swap(false, Ordering::SeqCst) {
                return Err("map response lost after Discord accepted nonce".into());
            }
            return Ok(DiscordMessageReceipt {
                channel_id: channel,
                message_id: 700,
                jump_url: String::new(),
            });
        }
        self.attempts
            .lock()
            .unwrap()
            .push((channel, key.into(), message.response.content.clone()));
        self.delivered
            .lock()
            .unwrap()
            .entry(key.into())
            .or_insert(message.response.content);
        if self.lose_reply.swap(false, Ordering::SeqCst) {
            return Err("response lost after Discord accepted nonce".into());
        }
        Ok(DiscordMessageReceipt {
            channel_id: channel,
            message_id: 600,
            jump_url: String::new(),
        })
    }
    async fn fetch_message(
        &self,
        _: u64,
        _: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }
    async fn send_message(
        &self,
        _: u64,
        _: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        self.public_calls.fetch_add(1, Ordering::SeqCst);
        Err("public send forbidden".into())
    }
    async fn edit_message(
        &self,
        channel: u64,
        id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        if channel == 500 && id == 700 && !message.response.attachments.is_empty() {
            if self.map_message_deleted.swap(false, Ordering::SeqCst) {
                self.maps.lock().unwrap().clear();
                return Err("unknown message".into());
            }
            self.map_edits.lock().unwrap().push(message);
            return Ok(());
        }
        self.public_calls.fetch_add(1, Ordering::SeqCst);
        Err("public edit forbidden".into())
    }
    async fn find_message_by_delivery_key(
        &self,
        channel: u64,
        key: &str,
        _: i64,
        _: usize,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        Ok(self
            .maps
            .lock()
            .unwrap()
            .contains_key(key)
            .then_some(DiscordMessageReceipt {
                channel_id: channel,
                message_id: 700,
                jump_url: String::new(),
            }))
    }
    async fn delete_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn create_public_thread(&self, _: u64, _: u64, _: &str) -> Result<u64, String> {
        self.public_calls.fetch_add(1, Ordering::SeqCst);
        Err("public thread forbidden".into())
    }
    async fn pin_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn archive_thread(&self, _: u64, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    async fn add_reaction(&self, _: u64, _: u64, _: &DiscordEmoji) -> Result<(), String> {
        Ok(())
    }
    async fn remove_reaction(
        &self,
        _: u64,
        _: u64,
        _: &DiscordEmoji,
        _: u64,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn clear_reaction(&self, _: u64, _: u64, _: &DiscordEmoji) -> Result<(), String> {
        Ok(())
    }
    async fn unpin_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn send_direct_message(&self, _: u64, _: DiscordMessage) -> Result<(), String> {
        self.public_calls.fetch_add(1, Ordering::SeqCst);
        Err("direct fallback forbidden".into())
    }
    fn cached_guild_member_render_names(
        &self,
        guild: u64,
        users: &[u64],
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberRenderNames>, String> {
        if self.fail_name_cache.load(Ordering::SeqCst) {
            return Err("cache unavailable".into());
        }
        let names = self.render_names.lock().unwrap();
        Ok(Some(
            users
                .iter()
                .filter_map(|user| names.get(&(guild, *user)).map(|name| (*user, name.clone())))
                .collect(),
        ))
    }
    async fn guild_member(
        &self,
        _: u64,
        _: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        self.member_http_calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

struct Fixture {
    db: NamedTempFile,
    pending: i64,
    discord: Arc<FakeDiscord>,
    live: Arc<DotaLiveFeed>,
    worker: SpectatorWorker,
}
impl Fixture {
    fn new() -> Self {
        Self::with_upstream(None)
    }
    fn with_upstream(upstream: Option<&str>) -> Self {
        let db = NamedTempFile::new().unwrap();
        crate::test_support::initialize_test_database(db.path()).unwrap();
        let state = PendingMatchState {
            radiant_team_ids: (1..=5).collect(),
            dire_team_ids: (6..=10).collect(),
            shuffle_timestamp: Some(100),
            bet_lock_until: Some(100),
            extra: std::collections::BTreeMap::from([
                ("spectator_lobby_message_id".into(), 55.into()),
                (DOTA_BETTING_CLOSED_MARKER.into(), true.into()),
            ]),
            ..Default::default()
        };
        let pending = PendingMatchRepository::new(db.path())
            .create_pending_match(1, &state)
            .unwrap()
            .pending_match_id;
        let config = DotaHostConfig::from_lookup(|key| {
            match key {
                "STEAM_API_KEY" if upstream.is_some() => Some("local-fixture-web-key"),
                "DOTA_HOST_ENABLED" => Some("true"),
                "DOTA_HOST_GUILD_IDS" => Some("1"),
                "DOTA_STEAM_USERNAME" => Some("test-bot"),
                "DOTA_BOT_ACCOUNT_ID" => Some("99"),
                "DOTA_GSI_TOKEN" => Some("fixture-gsi-token-at-least-32-characters"),
                _ => None,
            }
            .map(str::to_owned)
        })
        .unwrap()
        .unwrap();
        let live = match upstream {
            Some(base) => DotaLiveFeed::new_with_upstream_base(&config, base),
            None => DotaLiveFeed::new(&config),
        }
        .unwrap();
        live.publish_match_with_expected_accounts(
            1,
            pending,
            123,
            None,
            19144,
            (1..=10).collect(),
            true,
        );
        let discord = Arc::new(FakeDiscord::default());
        let worker = SpectatorWorker::new(db.path(), vec![1], discord.clone(), live.clone());
        Self {
            db,
            pending,
            discord,
            live,
            worker,
        }
    }
    fn subscribe(&self, id: i64, enabled: bool) {
        DotaSpectatorRepository::new(self.db.path())
            .update_subscription(1, 55, id, enabled, 100)
            .unwrap();
    }
    fn row(&self) -> DotaSpectatorRecord {
        DotaSpectatorRepository::new(self.db.path())
            .get(1, self.pending)
            .unwrap()
            .unwrap()
    }
    fn state(&self) -> State {
        serde_json::from_value(self.row().payload).unwrap()
    }
    fn fresh_feed(&self) {
        let players: serde_json::Map<String, serde_json::Value> = (1..=10)
            .map(|id| {
                (
                    id.to_string(),
                    serde_json::json!({"steamid":id.to_string()}),
                )
            })
            .collect();
        self.live.ingest_gsi(serde_json::json!({"auth":{"token":"fixture-gsi-token-at-least-32-characters"},"map":{"matchid":"123","clock_time":100,"game_state":"DOTA_GAMERULES_STATE_GAME_IN_PROGRESS"},"allplayers":players}),chrono::Utc::now().timestamp()).unwrap();
    }
    async fn queue_live(&self, now: i64) -> String {
        let mut row = self.row();
        let mut state = self.state();
        let mut queued = batch(&row, state.sequence, "private kill burst".into());
        queued.is_live = true;
        queued.created_at = now;
        let key = queued.key.clone();
        state.sequence += 1;
        state.queued = Some(queued);
        self.worker.save(&mut row, &state, now).await.unwrap();
        key
    }
    fn pending_edit(&self, edit: impl FnOnce(&mut PendingMatchState)) {
        let repository = PendingMatchRepository::new(self.db.path());
        let mut pending = repository.pending_match(1, self.pending).unwrap().unwrap();
        edit(&mut pending.state);
        repository
            .update_pending_match(1, self.pending, &pending.state)
            .unwrap();
    }
    fn end_match(&self) {
        PendingMatchRepository::new(self.db.path())
            .delete_pending_match(1, self.pending)
            .unwrap();
    }
}

#[test]
fn shuffled_participants_are_excluded_even_if_they_subscribed_first() {
    assert_eq!(eligible_viewers(&[1, 2, 3, 3, -1], &[1, 2]), vec![3]);
}

#[tokio::test]
async fn subscriptions_before_shuffle_are_filtered_against_final_teams_and_create_private_once() {
    let f = Fixture::new();
    f.subscribe(1, true);
    f.subscribe(20, true);
    f.subscribe(21, true);
    f.worker.tick(1000).await.unwrap();
    let calls = f.discord.ensure_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].participants, (1..=10).collect::<Vec<_>>());
    assert_eq!(calls[0].viewers, vec![20, 21]);
    assert_eq!(calls[0].known, None);
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
    f.worker.tick(1015).await.unwrap();
    assert_eq!(f.discord.ensure_calls.lock().unwrap().len(), 1);
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn admin_or_unverifiable_permissions_block_every_message_without_public_fallback() {
    for fail_at_audit in [false, true] {
        let f = Fixture::new();
        f.subscribe(20, true);
        f.discord
            .fail_ensure
            .store(!fail_at_audit, Ordering::SeqCst);
        f.discord.fail_audit.store(fail_at_audit, Ordering::SeqCst);
        f.worker.tick(1000).await.unwrap();
        assert!(f.discord.attempts.lock().unwrap().is_empty());
        assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
        if fail_at_audit {
            assert!(f.discord.channel.lock().unwrap().is_none());
            assert_eq!(f.row().channel_id, None);
        }
    }
}

#[tokio::test]
async fn intro_lost_reply_and_worker_restart_reuse_the_exact_delivery_nonce() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.discord.lose_reply.store(true, Ordering::SeqCst);
    f.worker.tick(1000).await.unwrap();
    let queued = f.state().queued.unwrap();
    assert!(!queued.is_live);
    let restarted = SpectatorWorker::new(f.db.path(), vec![1], f.discord.clone(), f.live.clone());
    restarted.tick(1015).await.unwrap();
    let attempts = f.discord.attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].1, queued.key);
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    assert!(f.state().queued.is_none());
}

#[tokio::test]
async fn permissions_are_checked_again_before_a_queued_live_batch() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.worker.tick(1000).await.unwrap();
    f.fresh_feed();
    f.queue_live(1010).await;
    f.discord.fail_audit.store(true, Ordering::SeqCst);
    f.worker.tick(1015).await.unwrap();
    assert_eq!(f.discord.attempts.lock().unwrap().len(), 1);
    assert!(f.discord.channel.lock().unwrap().is_none());
    assert!(f.state().queued.is_none());
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn subscriptions_only_from_players_do_not_create_a_channel() {
    let f = Fixture::new();
    f.subscribe(1, true);
    f.worker.tick(1000).await.unwrap();
    assert!(f.discord.ensure_calls.lock().unwrap().is_empty());
    assert!(f.discord.attempts.lock().unwrap().is_empty());
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unreact_updates_access_and_newly_shuffled_viewer_loses_access() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.subscribe(21, true);
    f.worker.tick(1000).await.unwrap();
    f.subscribe(20, false);
    f.worker.tick(1015).await.unwrap();
    assert_eq!(
        f.discord
            .channel
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .1
            .viewers,
        vec![21]
    );
    f.pending_edit(|state| state.radiant_team_ids[0] = 21);
    f.worker.tick(1030).await.unwrap();
    assert!(f.discord.channel.lock().unwrap().is_none());
    assert!(f.state().viewers.is_empty());
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn roster_change_with_failed_permission_update_deletes_owned_channel() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.subscribe(21, true);
    f.worker.tick(1000).await.unwrap();
    f.pending_edit(|state| state.radiant_team_ids[0] = 20);
    f.discord.fail_ensure.store(true, Ordering::SeqCst);
    f.worker.tick(1015).await.unwrap();
    assert!(f.discord.channel.lock().unwrap().is_none());
    assert_eq!(f.row().channel_id, None);
    assert!(f.state().queued.is_none());
}

#[tokio::test]
async fn queued_live_delivery_retries_same_nonce_but_stale_or_reopened_betting_never_sends() {
    for blocked in ["none", "reopened", "stale", "expired"] {
        let f = Fixture::new();
        f.subscribe(20, true);
        f.worker.tick(1000).await.unwrap();
        f.fresh_feed();
        let key = f.queue_live(1010).await;
        match blocked {
            "reopened" => f.pending_edit(|state| {
                state
                    .extra
                    .insert(DOTA_BETTING_EXTENDED_UNTIL.into(), 2000.into());
            }),
            "stale" => f.live.finish_match(1, f.pending).await,
            _ => {}
        }
        let now = if blocked == "expired" { 1110 } else { 1015 };
        if blocked == "none" {
            f.discord.lose_reply.store(true, Ordering::SeqCst);
        }
        f.worker.tick(now).await.unwrap();
        if blocked == "none" {
            assert!(f.state().queued.is_some());
            let restarted =
                SpectatorWorker::new(f.db.path(), vec![1], f.discord.clone(), f.live.clone());
            restarted.tick(now + 15).await.unwrap();
            let attempts = f.discord.attempts.lock().unwrap();
            let live: Vec<_> = attempts
                .iter()
                .filter(|(_, nonce, _)| nonce == &key)
                .collect();
            assert_eq!(live.len(), 2);
            assert_eq!(live[0], live[1]);
            assert_eq!(f.discord.delivered.lock().unwrap().len(), 2);
        } else {
            assert_eq!(
                f.discord.attempts.lock().unwrap().len(),
                1,
                "blocked={blocked}"
            );
            assert!(f.state().queued.is_none());
        }
    }
}

#[tokio::test]
async fn postgame_channel_is_deleted_after_fifteen_minutes_and_unreact_revokes_earlier() {
    for unsubscribe in [false, true] {
        let f = Fixture::new();
        f.subscribe(20, true);
        f.worker.tick(1000).await.unwrap();
        f.end_match();
        f.worker.tick(1100).await.unwrap();
        assert_eq!(f.row().expires_at, Some(2000));
        if unsubscribe {
            f.subscribe(20, false);
            f.worker.tick(1115).await.unwrap();
            assert!(f.discord.channel.lock().unwrap().is_none());
        } else {
            f.worker.tick(1999).await.unwrap();
            assert!(f.discord.channel.lock().unwrap().is_some());
        }
        f.worker.tick(2000).await.unwrap();
        assert!(f.discord.channel.lock().unwrap().is_none());
        assert!(
            DotaSpectatorRepository::new(f.db.path())
                .get(1, f.pending)
                .unwrap()
                .is_none()
        );
        assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn foreign_channel_ownership_is_never_deleted_or_used_for_delivery() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.worker.tick(1000).await.unwrap();
    f.discord.channel.lock().unwrap().as_mut().unwrap().1.marker = "foreign".into();
    f.worker.tick(1015).await.unwrap();
    assert!(f.discord.deleted.lock().unwrap().is_empty());
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_create_response_is_cleaned_by_marker_after_unreact_or_match_end() {
    for ended in [false, true] {
        let f = Fixture::new();
        f.subscribe(20, true);
        f.discord.lose_create_reply.store(true, Ordering::SeqCst);
        f.worker.tick(1000).await.unwrap();
        assert_eq!(f.row().channel_id, None);
        assert!(f.discord.channel.lock().unwrap().is_some());
        assert!(f.discord.attempts.lock().unwrap().is_empty());
        if ended {
            f.end_match();
        } else {
            f.subscribe(20, false);
        }
        let restarted =
            SpectatorWorker::new(f.db.path(), vec![1], f.discord.clone(), f.live.clone());
        restarted.tick(1121).await.unwrap();
        assert!(f.discord.channel.lock().unwrap().is_none(), "ended={ended}");
        assert_eq!(f.discord.deleted.lock().unwrap().len(), 1);
        assert_eq!(f.discord.ensure_calls.lock().unwrap().len(), 1);
        assert!(f.discord.attempts.lock().unwrap().is_empty());
        assert_eq!(f.state().creating_until, 0);
    }
}

#[tokio::test]
async fn betting_reopened_during_permission_audit_blocks_queued_delivery() {
    let f = Fixture::new();
    f.subscribe(20, true);
    f.worker.tick(1000).await.unwrap();
    f.fresh_feed();
    f.queue_live(1010).await;
    let path = f.db.path().to_path_buf();
    let pending_id = f.pending;
    *f.discord.audit_hook.lock().unwrap() = Some(Box::new(move || {
        let repository = PendingMatchRepository::new(path);
        let mut pending = repository.pending_match(1, pending_id).unwrap().unwrap();
        pending
            .state
            .extra
            .insert(DOTA_BETTING_EXTENDED_UNTIL.into(), 2000.into());
        repository
            .update_pending_match(1, pending_id, &pending.state)
            .unwrap();
    }));
    f.worker.tick(1015).await.unwrap();
    assert_eq!(f.discord.attempts.lock().unwrap().len(), 1);
    assert!(f.state().queued.is_none());
    assert!(f.discord.channel.lock().unwrap().is_none());
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

fn link_player(fixture: &Fixture, discord: i64, guild: i64, account: i64, alternate: bool) {
    let connection = cama_db::open_runtime_connection(fixture.db.path()).unwrap();
    connection.execute(
        "INSERT INTO players (discord_id,guild_id,discord_username,steam_id) VALUES (?1,?2,'outdated stored name',?3)",
        rusqlite::params![discord, guild, if alternate { None } else { Some(account) }],
    ).unwrap();
    if alternate {
        connection.execute(
            "INSERT INTO player_steam_ids (discord_id,steam_id,is_primary,added_at) VALUES (?1,?2,0,100)",
            rusqlite::params![discord, account],
        ).unwrap();
    }
}

#[tokio::test]
async fn spectator_actor_names_use_current_guild_cache_for_legacy_and_alternate_links() {
    let fixture = Fixture::new();
    link_player(&fixture, 1, 1, 1, false);
    link_player(&fixture, 6, 1, 6, true);
    fixture.discord.render_names.lock().unwrap().extend([
        ((1, 1), "jaso7".into()),
        ((1, 6), "pf".into()),
        ((2, 1), "wrong server alias".into()),
    ]);
    let mut current = frame(100, 1, 0);
    fixture.worker.enrich_names(&mut current, 1, &[1, 6]).await;
    assert_eq!(current.players[0].display_name.as_deref(), Some("jaso7"));
    assert_eq!(current.players[5].display_name.as_deref(), Some("pf"));
    fixture
        .discord
        .render_names
        .lock()
        .unwrap()
        .insert((1, 1), "new nickname".into());
    fixture.worker.enrich_names(&mut current, 1, &[1, 6]).await;
    assert_eq!(
        current.players[0].display_name.as_deref(),
        Some("new nickname")
    );
    assert_eq!(fixture.discord.member_http_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spectator_actor_names_never_borrow_other_guild_or_outside_roster_players() {
    let fixture = Fixture::new();
    // The first account is linked only in another server; the second belongs
    // to a linked server member who is not in the final match roster.
    link_player(&fixture, 1, 2, 1, false);
    link_player(&fixture, 99, 1, 2, false);
    fixture.discord.render_names.lock().unwrap().extend([
        ((1, 1), "same Discord user, wrong guild link".into()),
        ((1, 99), "not playing".into()),
    ]);
    let mut current = frame(100, 1, 0);
    current.players[0].display_name = Some("untrusted upstream label".into());
    fixture.worker.enrich_names(&mut current, 1, &[1, 2]).await;
    assert!(
        current
            .players
            .iter()
            .all(|hero| hero.display_name.is_none())
    );
    assert_eq!(fixture.discord.member_http_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spectator_actor_names_fall_back_when_cache_missing_blank_or_failed() {
    let fixture = Fixture::new();
    link_player(&fixture, 1, 1, 1, false);
    let mut current = frame(100, 1, 0);
    for cached in [None, Some(" "), Some("current alias")] {
        fixture.discord.render_names.lock().unwrap().clear();
        if let Some(name) = cached {
            fixture
                .discord
                .render_names
                .lock()
                .unwrap()
                .insert((1, 1), name.into());
        }
        fixture
            .discord
            .fail_name_cache
            .store(cached == Some("current alias"), Ordering::SeqCst);
        current.players[0].display_name = Some("stale previous alias".into());
        fixture.worker.enrich_names(&mut current, 1, &[1]).await;
        assert!(current.players[0].display_name.is_none());
    }
    assert_eq!(fixture.discord.member_http_calls.load(Ordering::SeqCst), 0);
}

fn frame(game_time: i64, radiant_score: i64, lead: i64) -> LiveAnnouncementFrame {
    LiveAnnouncementFrame {
        match_id: 123,
        game_time,
        radiant_score: Some(radiant_score),
        dire_score: Some(0),
        radiant_net_worth: Some(20_000 + lead),
        dire_net_worth: Some(20_000),
        players: (1..=10)
            .map(|hero_id| cama_domain::live_announcements::LiveHero {
                hero_id,
                account_id: Some(hero_id),
                display_name: None,
                name: cama_app::hero_lookup::hero_name(i64::from(hero_id)),
                radiant: hero_id <= 5,
                kills: Some(if hero_id == 1 { radiant_score } else { 0 }),
                deaths: Some(if hero_id == 6 { radiant_score } else { 0 }),
            })
            .collect(),
        ..Default::default()
    }
}

async fn poll_frame(
    fixture: &Fixture,
    listener: &tokio::net::TcpListener,
    frame: &LiveAnnouncementFrame,
) {
    poll_frame_with_map(fixture, listener, frame, false).await;
}

async fn poll_frame_with_map(
    fixture: &Fixture,
    listener: &tokio::net::TcpListener,
    frame: &LiveAnnouncementFrame,
    include_map: bool,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let players = |radiant| {
        frame
            .players
            .iter()
            .filter(|hero| hero.radiant == radiant)
            .map(|hero| {
                let mut value = serde_json::json!({"account_id":hero.hero_id,"hero_id":hero.hero_id,
            "kills":hero.kills,"deaths":hero.deaths});
                if include_map {
                    value["position_x"] = (i64::from(hero.hero_id) * 400 - 2000).into();
                    value["position_y"] = (frame.game_time * 2).into();
                }
                value
            })
            .collect::<Vec<_>>()
    };
    let body = serde_json::json!({"result":{"games":[{
        "match_id":frame.match_id,
        "scoreboard":{
            "game_time":frame.game_time,
            "radiant_score":frame.radiant_score,
            "dire_score":frame.dire_score,
            "teams":[
                {"team_number":2,"net_worth":frame.radiant_net_worth,"players":players(true)},
                {"team_number":3,"net_worth":frame.dire_net_worth,"players":players(false)}
            ]
        }
    }]}})
    .to_string();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let read = stream.read(&mut request).await.unwrap();
        assert!(String::from_utf8_lossy(&request[..read]).contains("GetLiveLeagueGames"));
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, fixture.live.poll_once());
    })
    .await
    .expect("local live-feed fixture completed");
    let snapshot = fixture.live.snapshot(1, fixture.pending).unwrap();
    assert!(!snapshot.stale);
    assert_eq!(
        snapshot.announcement_frame.as_ref().unwrap().game_time,
        frame.game_time
    );
}

#[tokio::test]
async fn advancing_event_is_delivered_at_fifteen_seconds_without_an_extra_outbox_tick() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame(&f, &listener, &frame(100, 0, 0)).await;
    f.worker.tick(1000).await.unwrap();
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    poll_frame(&f, &listener, &frame(115, 1, 0)).await;
    f.worker.tick(1014).await.unwrap();
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    f.worker.tick(1015).await.unwrap();
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 2);
    assert!(f.state().queued.is_none());
    assert_eq!(f.state().previous.unwrap().game_time, 115);
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn repeated_cached_game_clock_preserves_the_baseline_and_accumulated_kills() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame(&f, &listener, &frame(100, 0, 0)).await;
    f.worker.tick(1000).await.unwrap();
    poll_frame(&f, &listener, &frame(100, 2, 0)).await;
    f.worker.tick(1015).await.unwrap();
    assert_eq!(f.state().previous.unwrap().radiant_score, Some(0));
    assert_eq!(f.state().last_sample_at, 1000);
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 1);
    poll_frame(&f, &listener, &frame(115, 3, 0)).await;
    f.worker.tick(1030).await.unwrap();
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 2);
    assert_eq!(f.state().previous.unwrap().radiant_score, Some(3));
}

#[tokio::test]
async fn new_live_outbox_is_durable_before_send_and_restart_reuses_nonce_and_memory() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame(&f, &listener, &frame(100, 0, 0)).await;
    f.worker.tick(1000).await.unwrap();
    poll_frame(&f, &listener, &frame(115, 1, 6000)).await;
    f.discord.lose_reply.store(true, Ordering::SeqCst);
    f.worker.tick(1015).await.unwrap();
    let state = f.state();
    let queued = state.queued.unwrap();
    assert!(queued.is_live);
    assert!(state.announcement_memory.radiant_gold_milestone > 0);
    assert!(state.announcement_memory.first_blood_announced);
    let milestone = state.announcement_memory.radiant_gold_milestone;
    let restarted = SpectatorWorker::new(f.db.path(), vec![1], f.discord.clone(), f.live.clone());
    restarted.tick(1030).await.unwrap();
    assert!(f.state().queued.is_none());
    assert_eq!(
        f.state().announcement_memory.radiant_gold_milestone,
        milestone
    );
    {
        let attempts = f.discord.attempts.lock().unwrap();
        let retries: Vec<_> = attempts
            .iter()
            .filter(|(_, key, _)| key == &queued.key)
            .collect();
        assert_eq!(retries.len(), 2);
        assert_eq!(retries[0], retries[1]);
    }
    // Drop below and regain the same milestone after restart: no repeated
    // milestone announcement should be generated from the cached history.
    poll_frame(&f, &listener, &frame(130, 1, 5900)).await;
    restarted.tick(1045).await.unwrap();
    poll_frame(&f, &listener, &frame(145, 1, 6000)).await;
    restarted.tick(1060).await.unwrap();
    assert_eq!(
        f.state().announcement_memory.radiant_gold_milestone,
        milestone
    );
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 2);
}

#[test]
fn old_persisted_state_defaults_new_announcement_memory() {
    let old = serde_json::json!({
        "previous":{"match_id":123,"game_time":100,"radiant_score":0,"dire_score":0,
        "radiant_net_worth":20000,"dire_net_worth":20000,"buildings":[]},
        "last_sample_at":1000,"sequence":3
    });
    let state: State = serde_json::from_value(old).unwrap();
    assert!(!state.announcement_memory.first_blood_announced);
    assert_eq!(state.announcement_memory.radiant_gold_milestone, 0);
    assert!(state.announcement_memory.seen_events.is_empty());
    assert_eq!(state.sequence, 3);
    assert_eq!(state.previous.unwrap().game_time, 100);
}

#[tokio::test]
async fn same_tick_delivery_reaudits_after_persistence_and_blocks_reopened_betting() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame(&f, &listener, &frame(100, 0, 0)).await;
    f.worker.tick(1000).await.unwrap();
    poll_frame(&f, &listener, &frame(115, 1, 0)).await;
    let path = f.db.path().to_owned();
    let pending_id = f.pending;
    f.discord.audit_hook_after.store(1, Ordering::SeqCst);
    *f.discord.audit_hook.lock().unwrap() = Some(Box::new(move || {
        let row = DotaSpectatorRepository::new(&path)
            .get(1, pending_id)
            .unwrap()
            .unwrap();
        let state: State = serde_json::from_value(row.payload).unwrap();
        assert!(
            state.queued.unwrap().is_live,
            "outbox must exist before the send audit"
        );
        assert!(state.announcement_memory.first_blood_announced);
        let repository = PendingMatchRepository::new(path);
        let mut pending = repository.pending_match(1, pending_id).unwrap().unwrap();
        pending
            .state
            .extra
            .insert(DOTA_BETTING_EXTENDED_UNTIL.into(), 2000.into());
        repository
            .update_pending_match(1, pending_id, &pending.state)
            .unwrap();
    }));
    f.worker.tick(1015).await.unwrap();
    assert!(
        f.discord.audit_hook.lock().unwrap().is_none(),
        "second audit must run"
    );
    assert_eq!(f.discord.attempts.lock().unwrap().len(), 1);
    assert!(f.state().queued.is_none());
    assert!(f.discord.channel.lock().unwrap().is_none());
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spectator_map_reuses_one_message_on_quiet_fifteen_second_ticks() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    f.worker.tick(1000).await.unwrap();
    assert_eq!(f.state().map_message_id, Some(700));
    assert!(f.state().pending_map.is_none());
    assert_eq!(f.discord.maps.lock().unwrap().len(), 1);
    poll_frame_with_map(&f, &listener, &frame(115, 0, 0), true).await;
    f.worker.tick(1014).await.unwrap();
    assert!(f.discord.map_edits.lock().unwrap().is_empty());
    f.worker.tick(1015).await.unwrap();
    assert_eq!(
        f.discord.delivered.lock().unwrap().len(),
        1,
        "no extra text for a quiet tick"
    );
    assert_eq!(f.discord.maps.lock().unwrap().len(), 1);
    {
        let edits = f.discord.map_edits.lock().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].response.attachments[0].filename, "map-123-115.png");
        assert!(
            edits[0].response.attachments[0]
                .bytes
                .starts_with(b"\x89PNG")
        );
        assert_eq!(
            edits[0].response.embeds[0].title.as_deref(),
            Some("Last received map · 1:55")
        );
    }
    // A repeated clock and a later position-less sample cannot turn the old
    // image into a freshly timestamped update.
    poll_frame_with_map(&f, &listener, &frame(115, 0, 0), true).await;
    f.worker.tick(1030).await.unwrap();
    poll_frame_with_map(&f, &listener, &frame(130, 0, 0), false).await;
    f.worker.tick(1045).await.unwrap();
    assert_eq!(f.discord.map_edits.lock().unwrap().len(), 1);
    assert_eq!(f.discord.public_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spectator_map_recovers_lost_create_receipt_by_nonce_after_restart() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    f.discord.lose_map_reply.store(true, Ordering::SeqCst);
    f.worker.tick(1000).await.unwrap();
    assert!(f.state().map_message_id.is_none());
    assert!(f.state().pending_map.is_some());
    let restarted = SpectatorWorker::new(f.db.path(), vec![1], f.discord.clone(), f.live.clone());
    restarted.tick(1015).await.unwrap();
    assert_eq!(f.discord.maps.lock().unwrap().len(), 1);
    assert_eq!(f.discord.map_edits.lock().unwrap().len(), 1);
    assert_eq!(f.state().map_message_id, Some(700));
    assert!(f.state().pending_map.is_none());
}

#[tokio::test]
async fn spectator_map_reaudits_betting_after_render_and_withholds_private_image() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    // The greeting audit comes first; change policy during the map audit,
    // after rendering, to prove a fresh DB check blocks image publication.
    f.discord.audit_hook_after.store(1, Ordering::SeqCst);
    let path = f.db.path().to_owned();
    let id = f.pending;
    *f.discord.audit_hook.lock().unwrap() = Some(Box::new(move || {
        let repository = PendingMatchRepository::new(path);
        let mut pending = repository.pending_match(1, id).unwrap().unwrap();
        pending.state.extra.remove(DOTA_BETTING_CLOSED_MARKER);
        repository
            .update_pending_match(1, id, &pending.state)
            .unwrap();
    }));
    f.worker.tick(1000).await.unwrap();
    assert!(f.discord.maps.lock().unwrap().is_empty());
    assert!(f.row().channel_id.is_none());
    assert!(f.state().pending_map.is_none());
}

#[tokio::test]
async fn spectator_map_retry_drops_expired_frame_without_blocking_text() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    f.discord.lose_map_reply.store(true, Ordering::SeqCst);
    f.worker.tick(1000).await.unwrap();
    assert!(f.state().pending_map.is_some());
    f.worker.tick(1091).await.unwrap();
    assert!(f.state().pending_map.is_none());
    assert!(f.discord.map_edits.lock().unwrap().is_empty());
    poll_frame_with_map(&f, &listener, &frame(115, 1, 0), false).await;
    f.worker.tick(1106).await.unwrap();
    assert_eq!(f.discord.delivered.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn spectator_map_recreates_a_confirmed_deleted_message_with_a_new_nonce() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    f.worker.tick(1000).await.unwrap();
    let old_nonce = f
        .discord
        .maps
        .lock()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    f.discord.map_message_deleted.store(true, Ordering::SeqCst);
    poll_frame_with_map(&f, &listener, &frame(115, 0, 0), true).await;
    f.worker.tick(1015).await.unwrap();
    assert!(f.state().map_message_id.is_none());
    assert_eq!(f.state().map_generation, 1);
    assert!(f.state().pending_map.is_some());
    f.worker.tick(1030).await.unwrap();
    assert_eq!(f.state().map_message_id, Some(700));
    let maps = f.discord.maps.lock().unwrap();
    assert_eq!(maps.len(), 1);
    assert!(!maps.contains_key(&old_nonce));
}

#[tokio::test]
async fn spectator_map_does_not_publish_after_channel_permissions_fail() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture::with_upstream(Some(&format!("http://{}", listener.local_addr().unwrap())));
    f.subscribe(20, true);
    poll_frame_with_map(&f, &listener, &frame(100, 0, 0), true).await;
    let discord = f.discord.clone();
    *f.discord.audit_hook.lock().unwrap() = Some(Box::new(move || {
        // Greeting passed; the post-render map audit must fail.
        discord.fail_audit.store(true, Ordering::SeqCst);
    }));
    f.worker.tick(1000).await.unwrap();
    assert!(f.discord.maps.lock().unwrap().is_empty());
    assert!(f.row().channel_id.is_none());
    assert!(f.state().map_message_id.is_none());
    assert!(f.state().pending_map.is_none());
}
