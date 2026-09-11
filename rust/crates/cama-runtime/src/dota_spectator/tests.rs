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
    fail_ensure: AtomicBool,
    fail_audit: AtomicBool,
    lose_reply: AtomicBool,
    lose_create_reply: AtomicBool,
    public_calls: AtomicUsize,
    audit_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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
        if let Some(hook) = self.audit_hook.lock().unwrap().take() {
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
    async fn edit_message(&self, _: u64, _: u64, _: DiscordMessage) -> Result<(), String> {
        self.public_calls.fetch_add(1, Ordering::SeqCst);
        Err("public edit forbidden".into())
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
    async fn guild_member(
        &self,
        _: u64,
        _: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
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
        let live = DotaLiveFeed::new(&config).unwrap();
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
