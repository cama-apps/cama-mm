//! Private, opt-in spectator delivery. No live details go to the shared thread.
use crate::{
    BackgroundWorker, BackgroundWorkerSpec, InteractionAttachment, InteractionEmbed,
    InteractionResponse, WorkerContext,
    discord_transport::{DiscordMessage, DiscordTransport},
    dota_live::{DotaLiveFeed, LiveMapFrame},
};
use async_trait::async_trait;
use cama_db::{
    dota_spectator_repository::{DotaSpectatorRecord, DotaSpectatorRepository},
    match_runtime::{PendingMatchRecord, PendingMatchRepository},
    opendota_player::OpenDotaPlayerRepository,
};
use cama_domain::live_announcements::{
    ANNOUNCEMENT_INTERVAL_SECONDS, AnnouncementMemory, LiveAnnouncementFrame,
    announcement_message_with_memory,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

mod observation;

const RETENTION_SECONDS: i64 = 15 * 60;
const SUBSCRIPTION_RETRY_SECONDS: i64 = 120;
const MAP_UPDATE_INTERVAL: Duration = Duration::from_secs(5);
const SURFACE_REFRESH_INTERVAL_SECONDS: i64 = 15;
const RECAP_MAINTENANCE_INTERVAL_SECONDS: i64 = 15;

#[derive(Default)]
struct WorkerSchedule {
    last_recap_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    lobby_message_id: Option<i64>,
    participants: Vec<u64>,
    viewers: Vec<u64>,
    previous: Option<LiveAnnouncementFrame>,
    announcement_memory: AnnouncementMemory,
    last_sample_at: i64,
    sequence: u64,
    queued: Option<Batch>,
    creating_until: i64,
    map_message_id: Option<u64>,
    split_layout: bool,
    commentary_thread_id: Option<u64>,
    joined_viewers: Vec<u64>,
    subscription_batch: Option<SubscriptionBatch>,
    pending_map: Option<PendingMap>,
    last_delivered_map: Option<(u64, i64)>,
    map_create_started_at: Option<i64>,
    map_generation: u64,
    last_surface_refresh_at: Option<i64>,
    map_status: Option<MapFreshness>,
}
impl State {
    fn reset_surface(&mut self) {
        self.map_message_id = None;
        self.map_create_started_at = None;
        self.pending_map = None;
        self.last_surface_refresh_at = None;
        self.last_delivered_map = None;
        self.map_status = None;
        self.commentary_thread_id = None;
        self.joined_viewers.clear();
        self.subscription_batch = None;
        self.map_generation = self.map_generation.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MapFreshnessPhase {
    #[default]
    Waiting,
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct MapFreshness {
    phase: MapFreshnessPhase,
    match_id: Option<u64>,
    game_time: Option<i64>,
    source_fetched_at: Option<i64>,
    delay_seconds: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SubscriptionBatch {
    key: String,
    viewers: Vec<u64>,
    #[serde(default)]
    created_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingMap {
    frame: LiveMapFrame,
    created_at: i64,
    #[serde(default)]
    source_fetched_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Batch {
    key: String,
    content: String,
    #[serde(default)]
    is_live: bool,
    #[serde(default)]
    created_at: i64,
}

pub struct SpectatorWorker {
    path: PathBuf,
    guilds: Vec<i64>,
    discord: Arc<dyn DiscordTransport>,
    live: Arc<DotaLiveFeed>,
    lease: tokio::sync::Mutex<WorkerSchedule>,
    diagnostics: std::sync::Mutex<observation::Diagnostics>,
}
impl SpectatorWorker {
    pub fn new(
        path: impl AsRef<Path>,
        guilds: Vec<i64>,
        discord: Arc<dyn DiscordTransport>,
        live: Arc<DotaLiveFeed>,
    ) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            guilds,
            discord,
            live,
            lease: Default::default(),
            diagnostics: Default::default(),
        }
    }
    /// Resolve only this match's linked players, using current guild cache names.
    /// Name lookup is optional: cache/database failures leave the hero fallback.
    async fn enrich_names(
        &self,
        frame: &mut LiveAnnouncementFrame,
        guild_id: i64,
        participants: &[u64],
    ) {
        for hero in &mut frame.players {
            // An upstream payload or a previously enriched frame is not an
            // authoritative source of Discord display names.
            hero.display_name = None;
        }
        let Ok(guild) = u64::try_from(guild_id) else {
            return;
        };
        let accounts = frame
            .players
            .iter()
            .filter_map(|hero| hero.account_id.filter(|id| *id > 0))
            .collect::<BTreeSet<_>>();
        if accounts.is_empty() {
            return;
        }
        let participants = participants.iter().copied().collect::<BTreeSet<_>>();
        let repository = OpenDotaPlayerRepository::new(&self.path);
        let Ok(linked) = tokio::task::spawn_blocking(move || {
            accounts
                .into_iter()
                .filter_map(|account| {
                    let player = repository
                        .get_by_steam_id(i64::from(account), Some(guild_id))
                        .ok()??;
                    let discord = u64::try_from(player.discord_id).ok()?;
                    (discord > 0 && participants.contains(&discord)).then_some((account, discord))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .await
        else {
            return;
        };
        if linked.is_empty() {
            return;
        }
        let users = linked.values().copied().collect::<Vec<_>>();
        let Ok(Some(names)) = self.discord.cached_guild_member_render_names(guild, &users) else {
            return;
        };
        for hero in &mut frame.players {
            hero.display_name = hero
                .account_id
                .and_then(|account| linked.get(&account))
                .and_then(|discord| names.get(discord))
                .filter(|name| !name.trim().is_empty())
                .cloned();
        }
    }
    async fn save(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &State,
        now: i64,
    ) -> Result<(), String> {
        record.payload = serde_json::to_value(state).map_err(|e| e.to_string())?;
        let repository = DotaSpectatorRepository::new(&self.path);
        let candidate = record.clone();
        let saved = tokio::task::spawn_blocking(move || {
            repository.save(&candidate, candidate.revision, now)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if !saved {
            return Err("spectator state changed; retry reconciliation".into());
        }
        record.revision += 1;
        Ok(())
    }
    async fn tick(&self, now: i64) -> Result<(), String> {
        let mut schedule = self.lease.lock().await;
        let path = self.path.clone();
        let guilds = self.guilds.clone();
        let (games, rows) = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let mut games = Vec::new();
            let pending = PendingMatchRepository::new(&path);
            let repository = DotaSpectatorRepository::new(&path);
            for guild in &guilds {
                for game in pending.pending_matches(*guild).map_err(|e| e.to_string())? {
                    if !bot_hosted(&game) {
                        continue;
                    }
                    games.push((game.clone(), 0));
                    let Some(message) = lobby_message(&game) else {
                        continue;
                    };
                    let Ok(participants) = valid_roster(&game) else {
                        continue;
                    };
                    let viewers = eligible_viewers(
                        &repository
                            .subscribers(*guild, message)
                            .map_err(|e| e.to_string())?,
                        &participants,
                    );
                    games.last_mut().expect("current game").1 = viewers.len();
                    if viewers.is_empty() {
                        continue;
                    }
                    let marker = format!(
                        "cama-spectator:{guild}:{}:{:032x}",
                        game.pending_match_id,
                        fastrand::u128(..)
                    );
                    repository
                        .create_or_get(
                            *guild,
                            game.pending_match_id,
                            &marker,
                            serde_json::to_value(State {
                                participants,
                                lobby_message_id: Some(message),
                                ..State::default()
                            })
                            .map_err(|e| e.to_string())?,
                            now,
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok((games, repository.list().map_err(|e| e.to_string())?))
        })
        .await
        .map_err(|e| e.to_string())??;
        self.observe_games(&games, now).await;
        for row in rows {
            let identity = (row.guild_id, row.pending_match_id);
            if let Err(error) = self.reconcile(row, now).await {
                self.log_status(identity, "delivery", &format!("withheld: {error}"), now);
            }
        }
        // A faster live map must not multiply postgame scans, encodes or retries.
        // Keep the lease through maintenance so slow work never overlaps ticks.
        if schedule.last_recap_at.is_none_or(|last| {
            now < last || now.saturating_sub(last) >= RECAP_MAINTENANCE_INTERVAL_SECONDS
        }) {
            schedule.last_recap_at = Some(now);
            if let Err(error) = crate::dota_spectator_recap::tick(
                &self.path,
                &self.guilds,
                self.discord.as_ref(),
                now,
            )
            .await
            {
                tracing::warn!(%error, "optional map recap maintenance failed");
            }
        }
        Ok(())
    }
    async fn cleanup_unconfirmed_create(
        &self,
        record: &DotaSpectatorRecord,
        state: &mut State,
        guild: u64,
        now: i64,
    ) -> Result<bool, String> {
        if record.channel_id.is_none() && state.creating_until != 0 {
            // A concurrent create may still be in flight. Keep its durable
            // ownership marker until the lease expires and discovery succeeds.
            if now < state.creating_until {
                return Ok(false);
            }
            self.discord
                .delete_spectator_channels_by_marker(guild, &record.marker)
                .await?;
            state.creating_until = 0;
        }
        Ok(true)
    }

    async fn delivery_is_current(
        &self,
        record: &DotaSpectatorRecord,
        state: &State,
        now: i64,
        require_closed: bool,
    ) -> Result<bool, String> {
        let path = self.path.clone();
        let record = record.clone();
        let state = state.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, String> {
            let repository = DotaSpectatorRepository::new(&path);
            if repository
                .get(record.guild_id, record.pending_match_id)
                .map_err(|e| e.to_string())?
                .is_none_or(|current| current.revision != record.revision)
            {
                return Ok(false);
            }
            let Some(pending) = PendingMatchRepository::new(path)
                .pending_match(record.guild_id, record.pending_match_id)
                .map_err(|e| e.to_string())?
            else {
                return Ok(false);
            };
            if !bot_hosted(&pending) {
                return Ok(false);
            }
            let Ok(roster) = valid_roster(&pending) else {
                return Ok(false);
            };
            if roster.iter().any(|id| !state.participants.contains(id))
                || lobby_message(&pending) != state.lobby_message_id
                || (require_closed
                    && (!pending.state.betting_closed() || pending.state.betting_open(now)))
            {
                return Ok(false);
            }
            let Some(message) = state.lobby_message_id else {
                return Ok(false);
            };
            let subscriptions = repository
                .subscribers(record.guild_id, message)
                .map_err(|e| e.to_string())?;
            Ok(eligible_viewers(&subscriptions, &state.participants) == state.viewers)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    async fn reconcile(&self, mut record: DotaSpectatorRecord, now: i64) -> Result<(), String> {
        let mut state: State =
            serde_json::from_value(record.payload.clone()).map_err(|e| e.to_string())?;
        let saved_message = state.lobby_message_id;
        let path = self.path.clone();
        let guild = record.guild_id;
        let pending_id = record.pending_match_id;
        let (pending, subscriptions) = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let pending = PendingMatchRepository::new(&path)
                .pending_match(guild, pending_id)
                .map_err(|e| e.to_string())?;
            let subscriptions = match pending.as_ref().and_then(lobby_message).or(saved_message) {
                Some(message) => DotaSpectatorRepository::new(path)
                    .subscribers(guild, message)
                    .map_err(|e| e.to_string())?,
                None => Vec::new(),
            };
            Ok((pending, subscriptions))
        })
        .await
        .map_err(|e| e.to_string())??;
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        if pending.is_none() || !self.guilds.contains(&record.guild_id) {
            if !self
                .cleanup_unconfirmed_create(&record, &mut state, guild, now)
                .await?
            {
                return Ok(());
            }
            state.queued = None;
            state.pending_map = None;
            if record.expires_at.is_none() {
                record.expires_at = Some(now.saturating_add(RETENTION_SECONDS));
                self.save(&mut record, &state, now).await?;
            }
            // Keep the channel private during its short postgame retention.
            if let Some(channel) = record.channel_id
                && (eligible_viewers(&subscriptions, &state.participants) != state.viewers
                    || self
                        .discord
                        .audit_spectator_channel(
                            guild,
                            &record.marker,
                            &state.participants,
                            &state.viewers,
                            channel as u64,
                        )
                        .await
                        .is_err()
                    || record.expires_at.is_some_and(|at| now >= at))
            {
                self.discord
                    .delete_spectator_channel(guild, channel as u64, &record.marker)
                    .await?;
                record.channel_id = None;
                state.reset_surface();
                state.map_create_started_at = None;
                state.pending_map = None;
                self.save(&mut record, &state, now).await?;
            }
            if record.expires_at.is_some_and(|at| now >= at) {
                let repository = DotaSpectatorRepository::new(&self.path);
                tokio::task::spawn_blocking(move || {
                    repository.delete(record.guild_id, record.pending_match_id, record.revision)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        let pending = pending.expect("checked pending");
        if !bot_hosted(&pending) {
            if !self
                .cleanup_unconfirmed_create(&record, &mut state, guild, now)
                .await?
            {
                return Ok(());
            }
            self.remove_surface(&mut record, &mut state, now).await?;
            return Ok(());
        }
        if record.channel_id.is_some() && !state.split_layout {
            // One-time migration of the old mixed map/commentary room. Deleting
            // our owned ephemeral parent also removes its attached threads.
            self.remove_surface(&mut record, &mut state, now).await?;
        }
        state.split_layout = true;
        let roster = match valid_roster(&pending) {
            Ok(roster) => roster,
            Err(error) => {
                if !self
                    .cleanup_unconfirmed_create(&record, &mut state, guild, now)
                    .await?
                {
                    return Ok(());
                }
                if let Some(channel) = record.channel_id {
                    self.discord
                        .delete_spectator_channel(guild, channel as u64, &record.marker)
                        .await?;
                    record.channel_id = None;
                    state.reset_surface();
                    state.map_create_started_at = None;
                    state.pending_map = None;
                }
                state.queued = None;
                state.previous = None;
                self.save(&mut record, &state, now).await?;
                return Err(error);
            }
        };
        // Former players remain excluded if an operator changes the roster.
        let participants = state
            .participants
            .iter()
            .copied()
            .chain(roster.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let viewers = eligible_viewers(&subscriptions, &participants);
        let changed = participants != state.participants || viewers != state.viewers;
        state.participants = participants;
        state.viewers = viewers;
        state.joined_viewers.retain(|id| state.viewers.contains(id));
        if state
            .subscription_batch
            .as_ref()
            .is_some_and(|batch| batch.viewers.iter().any(|id| !state.viewers.contains(id)))
        {
            state.subscription_batch = None;
        }
        if state.viewers.is_empty() {
            if !self
                .cleanup_unconfirmed_create(&record, &mut state, guild, now)
                .await?
            {
                return Ok(());
            }
            if let Some(channel) = record.channel_id {
                self.discord
                    .delete_spectator_channel(guild, channel as u64, &record.marker)
                    .await?;
                record.channel_id = None;
                state.reset_surface();
                state.map_create_started_at = None;
                state.pending_map = None;
            }
            state.queued = None;
            state.previous = None;
            self.save(&mut record, &state, now).await?;
            return Ok(());
        }
        // Preserve the available map before slow Discord setup/audits let a
        // thinner or newer scoreboard replace the shared feed snapshot.
        self.queue_available_map(&mut record, &mut state, &pending, &roster, now)
            .await?;
        if record.channel_id.is_none() {
            if state.creating_until > now {
                self.log_status(
                    (record.guild_id, record.pending_match_id),
                    "delivery",
                    "waiting to retry spectator thread creation",
                    now,
                );
                return Ok(());
            }
            state.creating_until = now.saturating_add(120);
            self.save(&mut record, &state, now).await?;
        }
        if record.channel_id.is_none() || changed {
            self.log_status(
                (record.guild_id, record.pending_match_id),
                "delivery",
                "checking or creating spectator thread",
                now,
            );
            let channel = match self
                .discord
                .ensure_spectator_channel(
                    guild,
                    pending
                        .state
                        .shuffle_channel_id
                        .or(pending.state.origin_channel_id)
                        .or(pending.state.cmd_shuffle_channel_id)
                        .or(pending.state.thread_shuffle_thread_id)
                        .and_then(|id| u64::try_from(id).ok())
                        .ok_or("spectator match source channel unavailable")?,
                    &record.marker,
                    &format!("match-{}-spectators", record.pending_match_id),
                    &state.participants,
                    &state.viewers,
                    record.channel_id.map(|id| id as u64),
                )
                .await
            {
                Ok(channel) => channel,
                Err(error) => {
                    if let Some(channel) = record.channel_id {
                        self.discord
                            .delete_spectator_channel(guild, channel as u64, &record.marker)
                            .await?;
                        record.channel_id = None;
                        state.reset_surface();
                        state.map_create_started_at = None;
                        state.pending_map = None;
                    }
                    state.queued = None;
                    state.previous = None;
                    self.save(&mut record, &state, now).await?;
                    return Err(error);
                }
            };
            let created = record.channel_id.is_none();
            record.channel_id = Some(i64::try_from(channel).map_err(|_| "channel ID overflow")?);
            state.creating_until = 0;
            if created {
                state.queued = Some(batch(
                    &record,
                    state.sequence,
                    format!(
                        "📻 **Match #{} · Commentary**\nFollow the live map, kills, gold swings, and objectives once betting closes and the live feed arrives.\n_Only eligible spectators are invited. This room closes 15 minutes after the Cama match ends._",
                        record.pending_match_id
                    ),
                ));
                state.sequence += 1;
            }
            self.save(&mut record, &state, now).await?;
            tracing::info!(
                guild_id = record.guild_id,
                pending_match_id = record.pending_match_id,
                channel_id = channel,
                viewers = state.viewers.len(),
                created,
                "Dota spectator channel ready"
            );
        }
        // Both layouts require an audited private destination; never fall back
        // to the shared match lobby.
        let refresh_surface = changed
            || state.map_message_id.is_none()
            || state.commentary_thread_id.is_none()
            || state.last_surface_refresh_at.is_none_or(|last| {
                now < last || now.saturating_sub(last) >= SURFACE_REFRESH_INTERVAL_SECONDS
            });
        // Reuse established surfaces between the normal membership checks.
        // Each map delivery still performs its own fresh permission/policy audit.
        if refresh_surface {
            self.ensure_surface(&mut record, &mut state, now).await?;
            if let Err(error) = self
                .subscribe_thread_viewers(&mut record, &mut state, now)
                .await
            {
                if record.channel_id.is_none() {
                    return Err(error);
                }
                self.log_status(
                    (record.guild_id, record.pending_match_id),
                    "subscription",
                    &format!("retrying: {error}"),
                    now,
                );
            }
            state.last_surface_refresh_at = Some(now);
            self.save(&mut record, &state, now).await?;
        }
        if !self
            .deliver_queued(&mut record, &mut state, &pending, now)
            .await?
        {
            return Ok(());
        }
        if !pending.state.betting_closed()
            || pending.state.betting_open(now)
            || now.saturating_sub(state.last_sample_at) < ANNOUNCEMENT_INTERVAL_SECONDS
        {
            self.try_deliver_map(&mut record, &mut state, now).await;
            return Ok(());
        }
        let Some(snapshot) = self
            .live
            .snapshot(record.guild_id, record.pending_match_id)
            .filter(|s| !s.stale)
        else {
            self.try_deliver_map(&mut record, &mut state, now).await;
            return Ok(());
        };
        let Some(mut frame) = snapshot.announcement_frame else {
            self.try_deliver_map(&mut record, &mut state, now).await;
            return Ok(());
        };
        // Cached or regressed game clocks must not replace the last distinct
        // sample: doing so discards changes before the next advancing frame.
        if state.previous.as_ref().is_some_and(|previous| {
            previous.match_id == frame.match_id && frame.game_time <= previous.game_time
        }) {
            self.try_deliver_map(&mut record, &mut state, now).await;
            return Ok(());
        }
        self.enrich_names(&mut frame, record.guild_id, &roster)
            .await;
        let message = announcement_message_with_memory(
            state.previous.as_ref(),
            &frame,
            snapshot.delay_seconds,
            &mut state.announcement_memory,
        );
        state.previous = Some(frame);
        state.last_sample_at = now;
        if let Some(message) = message {
            state.queued = Some(batch(&record, state.sequence, message));
            state.sequence += 1;
            if let Some(queued) = &mut state.queued {
                queued.is_live = true;
                queued.created_at = now;
            }
        }
        // Persist text and announcement memory atomically before sending.
        // Re-audit permissions and current database policy even in this same
        // tick; failed delivery keeps the exact batch/nonce for the next tick.
        self.save(&mut record, &state, now).await?;
        if state.queued.is_some() {
            self.deliver_queued(&mut record, &mut state, &pending, now)
                .await?;
        }
        self.try_deliver_map(&mut record, &mut state, now).await;
        Ok(())
    }

    async fn remove_surface(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        if let Some(channel) = record.channel_id {
            let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
            self.discord
                .delete_spectator_channel(guild, channel as u64, &record.marker)
                .await?;
        }
        record.channel_id = None;
        state.reset_surface();
        state.queued = None;
        state.previous = None;
        self.save(record, state, now).await
    }

    async fn audit_thread(
        &self,
        record: &DotaSpectatorRecord,
        state: &State,
    ) -> Result<(), String> {
        self.discord
            .audit_spectator_thread(
                u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?,
                record
                    .channel_id
                    .ok_or("spectator map channel unavailable")? as u64,
                state.map_message_id.ok_or("spectator map unavailable")?,
                state
                    .commentary_thread_id
                    .ok_or("spectator commentary unavailable")?,
                &record.marker,
                &state.participants,
                &state.viewers,
            )
            .await
    }

    async fn ensure_surface(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        let channel = record
            .channel_id
            .ok_or("spectator map channel unavailable")? as u64;
        if state.map_message_id.is_none() {
            if state.map_create_started_at.is_none() {
                state.map_create_started_at = Some(now);
                self.save(record, state, now).await?;
            }
            let key = map_delivery_key(record, state, channel);
            let existing = self
                .discord
                .find_message_by_delivery_key(
                    channel,
                    &key,
                    state.map_create_started_at.unwrap_or(now).saturating_sub(1),
                    500,
                )
                .await?;
            if self
                .discord
                .audit_spectator_channel(
                    guild,
                    &record.marker,
                    &state.participants,
                    &state.viewers,
                    channel,
                )
                .await
                .is_err()
                || !self.delivery_is_current(record, state, now, false).await?
            {
                self.remove_surface(record, state, now).await?;
                return Err("spectator access changed before map creation".into());
            }
            let id = match existing {
                Some(receipt) => receipt.message_id,
                None => {
                    self.discord
                        .send_message_with_delivery_key(
                            channel,
                            &key,
                            DiscordMessage::silent(
                                InteractionResponse::message("")
                                    .embed(InteractionEmbed::titled("Latest map").description(
                                        "Waiting for the first live map observation.",
                                    )),
                            ),
                        )
                        .await?
                        .message_id
                }
            };
            state.map_message_id = Some(id);
            self.save(record, state, now).await?;
        }
        let map = state.map_message_id.ok_or("spectator map unavailable")?;
        let thread = match self
            .discord
            .ensure_spectator_thread(
                guild,
                channel,
                map,
                &record.marker,
                &format!("match-{}-commentary", record.pending_match_id),
                &state.participants,
                &state.viewers,
                state.commentary_thread_id,
            )
            .await
        {
            Ok(thread) => thread,
            Err(error) => {
                // Preserve identity on transient failures or lost create replies;
                // the next ensure recovers the thread from its starter ID. A
                // confirmed deleted starter requires a clean replacement pair.
                if error == crate::discord_transport::SPECTATOR_THREAD_DELETED
                    || self
                        .discord
                        .audit_spectator_channel(
                            guild,
                            &record.marker,
                            &state.participants,
                            &state.viewers,
                            channel,
                        )
                        .await
                        .is_err()
                    || self.discord.fetch_message(channel, map).await?.is_none()
                {
                    self.remove_surface(record, state, now).await?;
                }
                return Err(error);
            }
        };
        if thread != map && thread != channel {
            self.remove_surface(record, state, now).await?;
            return Err("spectator commentary is outside its audited room".into());
        }
        if !self.delivery_is_current(record, state, now, false).await? {
            self.remove_surface(record, state, now).await?;
            return Err("spectator access changed during thread setup".into());
        }
        if state.commentary_thread_id != Some(thread) {
            state.commentary_thread_id = Some(thread);
            state.joined_viewers.clear();
            state.subscription_batch = None;
            self.save(record, state, now).await?;
        }
        Ok(())
    }

    async fn subscribe_thread_viewers(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        let thread = state
            .commentary_thread_id
            .ok_or("spectator commentary unavailable")?;
        // A message receipt is not proof of a Discord thread membership. This
        // also repairs saved flags from older workers which assumed it was.
        let members = self.discord.spectator_thread_members(thread).await?;
        let joined = state
            .viewers
            .iter()
            .copied()
            .filter(|id| members.contains(id))
            .collect::<Vec<_>>();
        let membership_changed = joined != state.joined_viewers;
        state.joined_viewers = joined;
        let batch_finished = state
            .subscription_batch
            .as_ref()
            .is_some_and(|batch| batch.viewers.iter().all(|id| members.contains(id)));
        // Retry accepted mentions whose membership never appeared, using a new
        // nonce after a bounded delay. Reusing their message receipt forever
        // would strand spectators across every subsequent deployment.
        let batch_expired = state.subscription_batch.as_ref().is_some_and(|batch| {
            now.saturating_sub(batch.created_at) >= SUBSCRIPTION_RETRY_SECONDS
        });
        if batch_expired && !batch_finished {
            let pending = state.subscription_batch.as_ref().expect("expired batch");
            // An incomplete history lookup must not turn an ambiguous earlier
            // send into a new invitation. Preserve the old durable batch until
            // both membership and delivery history can be inspected.
            self.discord
                .find_message_by_delivery_key(
                    thread,
                    &pending.key,
                    pending.created_at.saturating_sub(1),
                    500,
                )
                .await?;
        }
        if batch_finished || batch_expired {
            state.subscription_batch = None;
        }
        if membership_changed || batch_finished || batch_expired {
            self.save(record, state, now).await?;
        }
        if state.subscription_batch.is_none() {
            let viewers = state
                .viewers
                .iter()
                .copied()
                .filter(|id| !state.joined_viewers.contains(id))
                .collect::<Vec<_>>();
            if viewers.is_empty() {
                self.log_status(
                    (record.guild_id, record.pending_match_id),
                    "subscription",
                    &format!(
                        "{} spectator memberships verified",
                        state.joined_viewers.len()
                    ),
                    now,
                );
                return Ok(());
            }
            let key = batch(record, state.sequence, String::new()).key;
            state.sequence += 1;
            state.subscription_batch = Some(SubscriptionBatch {
                key,
                viewers,
                created_at: now,
            });
            self.save(record, state, now).await?;
        }
        let batch = state
            .subscription_batch
            .as_ref()
            .ok_or("spectator join missing")?;
        // A durable join can outlive Discord's short nonce deduplication window.
        // Search owned history first; a bounded/incomplete search fails closed.
        let delivered = self
            .discord
            .find_message_by_delivery_key(
                thread,
                &batch.key,
                batch.created_at.saturating_sub(1),
                500,
            )
            .await?;
        if self.audit_thread(record, state).await.is_err()
            || !self.delivery_is_current(record, state, now, false).await?
        {
            self.remove_surface(record, state, now).await?;
            return Err("spectator subscriptions changed before thread join".into());
        }
        let batch = state
            .subscription_batch
            .as_ref()
            .ok_or("spectator join missing")?;
        let mentions = batch
            .viewers
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join(" ");
        if delivered.is_none() {
            self.discord
                .send_message_with_delivery_key(
                    state
                        .commentary_thread_id
                        .ok_or("spectator commentary unavailable")?,
                    &batch.key,
                    DiscordMessage::mentioning(
                        InteractionResponse::message(format!("📻 Subscribed: {mentions}")),
                        batch.viewers.iter().copied().collect(),
                    )
                    .suppressing_notifications(),
                )
                .await?;
        }
        let members = self.discord.spectator_thread_members(thread).await?;
        state.joined_viewers = state
            .viewers
            .iter()
            .copied()
            .filter(|id| members.contains(id))
            .collect();
        if batch.viewers.iter().all(|id| members.contains(id)) {
            state.subscription_batch = None;
        }
        self.save(record, state, now).await?;
        if state.subscription_batch.is_some() {
            return Err(
                "spectator invitation sent; Discord membership is not yet confirmed".into(),
            );
        }
        self.log_status(
            (record.guild_id, record.pending_match_id),
            "subscription",
            &format!(
                "{} spectator memberships verified",
                state.joined_viewers.len()
            ),
            now,
        );
        Ok(())
    }

    async fn queue_available_map(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        pending: &PendingMatchRecord,
        roster: &[u64],
        now: i64,
    ) -> Result<(), String> {
        if !pending.state.betting_closed() || pending.state.betting_open(now) {
            if state.pending_map.is_some() {
                state.pending_map = None;
                self.save(record, state, now).await?;
            }
            return Ok(());
        }
        let Some(snapshot) = self
            .live
            .snapshot(record.guild_id, record.pending_match_id)
            .filter(|snapshot| !snapshot.stale)
        else {
            return Ok(());
        };
        let Some(mut map) = snapshot
            .map_frame
            .filter(|map| map.match_id == snapshot.match_id)
        else {
            return Ok(());
        };
        if state
            .last_delivered_map
            .is_some_and(|(id, clock)| id == map.match_id && clock >= map.game_time)
            || state.pending_map.as_ref().is_some_and(|pending| {
                pending.frame.match_id == map.match_id && pending.frame.game_time >= map.game_time
            })
        {
            return Ok(());
        }
        if let Some(mut frame) = snapshot
            .announcement_frame
            .filter(|frame| frame.match_id == map.match_id && frame.game_time == map.game_time)
        {
            self.enrich_names(&mut frame, record.guild_id, roster).await;
            apply_map_display_names(&mut map, &frame);
        }
        let map = PendingMap {
            frame: map,
            created_at: now,
            source_fetched_at: Some(snapshot.fetched_at),
        };
        state.pending_map = Some(map);
        self.save(record, state, now).await
    }

    async fn try_deliver_map(&self, record: &mut DotaSpectatorRecord, state: &mut State, now: i64) {
        if let Err(error) = self.deliver_map(record, state, now).await {
            // A missing icon/render or Discord image failure must not suppress
            // the independent text commentary outbox.
            self.log_status(
                (record.guild_id, record.pending_match_id),
                "map",
                &format!("withheld: {error}"),
                now,
            );
        }
    }

    fn map_rejection_reason(
        &self,
        record: &DotaSpectatorRecord,
        map: &PendingMap,
        now: i64,
    ) -> Option<&'static str> {
        if now.saturating_sub(map.created_at) > 90
            || map
                .source_fetched_at
                .is_none_or(|at| chrono::Utc::now().timestamp().saturating_sub(at) > 90)
        {
            return Some("pending map expired or lacks a trusted capture time");
        }
        match self.live.snapshot(record.guild_id, record.pending_match_id) {
            Some(snapshot) if !snapshot.stale && snapshot.match_id == map.frame.match_id => None,
            _ => Some("pending map belongs to an unavailable, stale, or different live match"),
        }
    }

    fn current_map_status(&self, record: &DotaSpectatorRecord, state: &State) -> MapFreshness {
        let previous = state.map_status.as_ref();
        let image = state
            .last_delivered_map
            .or_else(|| previous.and_then(|status| Some((status.match_id?, status.game_time?))));
        let source_fetched_at = previous
            .and_then(|status| status.source_fetched_at)
            .filter(|timestamp| *timestamp > 0);
        let delay_seconds = previous.and_then(|status| status.delay_seconds);
        let Some(snapshot) = self.live.snapshot(record.guild_id, record.pending_match_id) else {
            return if let Some((match_id, game_time)) = image {
                MapFreshness {
                    phase: MapFreshnessPhase::Unavailable,
                    match_id: Some(match_id),
                    game_time: Some(game_time),
                    source_fetched_at,
                    delay_seconds,
                }
            } else {
                MapFreshness::default()
            };
        };
        let Some((match_id, game_time)) = image else {
            // A pending render or first surface creation will publish the
            // first image. Keep the placeholder honest until that succeeds.
            return MapFreshness::default();
        };
        let same_match = snapshot.match_id == match_id
            && snapshot
                .map_frame
                .as_ref()
                .is_some_and(|map| map.match_id == match_id);
        let same_frame = same_match
            && snapshot
                .map_frame
                .as_ref()
                .is_some_and(|map| map.game_time == game_time);
        // A new source sample must not make an older displayed image look
        // newly observed. Only take source metadata from a sample carrying
        // the displayed frame; otherwise keep the last published metadata.
        let source_fetched_at = if same_frame {
            source_fetched_at.or_else(|| (snapshot.fetched_at > 0).then_some(snapshot.fetched_at))
        } else {
            source_fetched_at
        };
        let delay_seconds = if same_frame {
            snapshot.delay_seconds.or(delay_seconds)
        } else {
            delay_seconds
        };
        let phase = if !same_match {
            MapFreshnessPhase::Unavailable
        } else if snapshot.stale {
            MapFreshnessPhase::Stale
        } else if snapshot.map_frame.is_some() {
            MapFreshnessPhase::Fresh
        } else {
            MapFreshnessPhase::Unavailable
        };
        MapFreshness {
            phase,
            match_id: Some(match_id),
            game_time: Some(game_time),
            source_fetched_at,
            delay_seconds,
        }
    }

    async fn refresh_map_status(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        let Some(map_message_id) = state.map_message_id else {
            return Ok(());
        };
        let candidate = self.current_map_status(record, state);
        if !map_status_changed(state.map_status.as_ref(), &candidate) {
            return Ok(());
        }
        // The initial placeholder already communicates this state. Persisting
        // it avoids treating every five-second tick as a first transition.
        if state.map_status.is_none() && candidate.phase == MapFreshnessPhase::Waiting {
            state.map_status = Some(candidate);
            return self.save(record, state, now).await;
        }
        let channel = record.channel_id.ok_or("spectator channel unavailable")? as u64;
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        let audit = self
            .discord
            .audit_spectator_channel(
                guild,
                &record.marker,
                &state.participants,
                &state.viewers,
                channel,
            )
            .await;
        if audit.is_err() || !self.delivery_is_current(record, state, now, true).await? {
            self.discord
                .delete_spectator_channel(guild, channel, &record.marker)
                .await?;
            record.channel_id = None;
            state.reset_surface();
            state.map_create_started_at = None;
            state.pending_map = None;
            state.queued = None;
            state.previous = None;
            self.save(record, state, now).await?;
            return Ok(());
        }
        // The audit itself can race a feed transition. Re-read status after
        // the policy gate so the edit never describes an older lifecycle.
        let candidate = self.current_map_status(record, state);
        if !map_status_changed(state.map_status.as_ref(), &candidate) {
            return Ok(());
        }
        let image = state.last_delivered_map;
        let message = DiscordMessage::silent(
            InteractionResponse::message("")
                .embed(map_embed(&candidate, image))
                .preserve_attachments(),
        );
        if let Err(error) = self
            .discord
            .edit_message(channel, map_message_id, message)
            .await
        {
            // A status-only edit preserves the upload. Only a confirmed
            // deletion is allowed to discard the persisted surface identity.
            if self
                .discord
                .fetch_message(channel, map_message_id)
                .await?
                .is_none()
            {
                self.remove_surface(record, state, now).await?;
            }
            return Err(error);
        }
        state.map_status = Some(candidate);
        self.log_status(
            (record.guild_id, record.pending_match_id),
            "map",
            match state.map_status.as_ref().map(|status| status.phase) {
                Some(MapFreshnessPhase::Stale) => "feed stale",
                Some(MapFreshnessPhase::Unavailable) => "feed unavailable",
                Some(MapFreshnessPhase::Fresh) => "feed resumed",
                _ => "waiting for feed",
            },
            now,
        );
        self.save(record, state, now).await
    }

    async fn deliver_map(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        let Some(map) = state.pending_map.clone() else {
            return self.refresh_map_status(record, state, now).await;
        };
        if let Some(reason) = self.map_rejection_reason(record, &map, now) {
            self.log_status(
                (record.guild_id, record.pending_match_id),
                "map",
                reason,
                now,
            );
            state.pending_map = None;
            self.save(record, state, now).await?;
            return self.refresh_map_status(record, state, now).await;
        }
        let channel = record.channel_id.ok_or("spectator channel unavailable")? as u64;
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        if state.map_create_started_at.is_none() && state.map_message_id.is_none() {
            state.map_create_started_at = Some(now);
            self.save(record, state, now).await?;
        }
        let frame = map.frame.clone();
        let rendered =
            tokio::task::spawn_blocking(move || crate::dota_spectator_map::render_map(&frame))
                .await
                .map_err(|error| error.to_string())??;
        // One stable create nonce per private channel survives crashes between
        // Discord accepting the image and our receipt being persisted.
        let key = map_delivery_key(record, state, channel);
        let existing = match state.map_message_id {
            Some(id) => Some(id),
            None => self
                .discord
                .find_message_by_delivery_key(
                    channel,
                    &key,
                    state.map_create_started_at.unwrap_or(now).saturating_sub(1),
                    500,
                )
                .await?
                .map(|receipt| receipt.message_id),
        };
        // Rendering can take time. Audit after it, then
        // re-read betting/roster/subscription policy immediately before publish.
        let audit = self
            .discord
            .audit_spectator_channel(
                guild,
                &record.marker,
                &state.participants,
                &state.viewers,
                channel,
            )
            .await;
        let policy_current =
            audit.is_ok() && self.delivery_is_current(record, state, now, true).await?;
        if !policy_current {
            self.discord
                .delete_spectator_channel(guild, channel, &record.marker)
                .await?;
            record.channel_id = None;
            state.reset_surface();
            state.map_create_started_at = None;
            state.pending_map = None;
            state.queued = None;
            state.previous = None;
            self.save(record, state, now).await?;
            return Ok(());
        }
        if let Some(reason) = self.map_rejection_reason(record, &map, now) {
            self.log_status(
                (record.guild_id, record.pending_match_id),
                "map",
                reason,
                now,
            );
            state.pending_map = None;
            self.save(record, state, now).await?;
            return self.refresh_map_status(record, state, now).await;
        }
        let clock = map.frame.game_time;
        let filename = format!("map-{}-{clock}.png", map.frame.match_id);
        let delay_seconds = self
            .live
            .snapshot(record.guild_id, record.pending_match_id)
            .and_then(|snapshot| snapshot.delay_seconds);
        let status = MapFreshness {
            phase: MapFreshnessPhase::Fresh,
            match_id: Some(map.frame.match_id),
            game_time: Some(clock),
            source_fetched_at: map.source_fetched_at,
            delay_seconds,
        };
        let embed = map_embed(&status, Some((map.frame.match_id, clock)));
        let message = DiscordMessage::silent(
            InteractionResponse::message("")
                .embed(embed)
                .attachment(InteractionAttachment::bytes(filename, rendered)),
        );
        let message_id = if let Some(id) = existing {
            if let Err(error) = self.discord.edit_message(channel, id, message).await {
                // Only a confirmed missing message permits a new create nonce;
                // transient lookup/edit failures retain the existing identity.
                if self.discord.fetch_message(channel, id).await?.is_none() {
                    self.remove_surface(record, state, now).await?;
                }
                return Err(error);
            }
            id
        } else {
            self.discord
                .send_message_with_delivery_key(channel, &key, message)
                .await?
                .message_id
        };
        self.log_status(
            (record.guild_id, record.pending_match_id),
            "map",
            "delivered",
            now,
        );
        state.map_message_id = Some(message_id);
        state.last_delivered_map = Some((map.frame.match_id, map.frame.game_time));
        state.map_status = Some(status);
        state.pending_map = None;
        self.save(record, state, now).await
    }

    /// Audit immediately before attempting an already-persisted outbox batch.
    /// Returning false means the channel was revoked and reconciliation must stop.
    async fn deliver_queued(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        pending: &PendingMatchRecord,
        now: i64,
    ) -> Result<bool, String> {
        if state.queued.is_none() {
            return Ok(true);
        }
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        let channel = record.channel_id.ok_or("spectator channel unavailable")? as u64;
        if let Err(error) = self
            .discord
            .audit_spectator_channel(
                guild,
                &record.marker,
                &state.participants,
                &state.viewers,
                channel,
            )
            .await
        {
            // No fallback destination: competitors can read the normal thread.
            self.discord
                .delete_spectator_channel(guild, channel, &record.marker)
                .await?;
            record.channel_id = None;
            state.reset_surface();
            state.map_create_started_at = None;
            state.pending_map = None;
            state.queued = None;
            state.previous = None;
            self.save(record, state, now).await?;
            return Err(error);
        }
        let thread = state
            .commentary_thread_id
            .ok_or("spectator commentary unavailable")?;
        if let Err(error) = self.audit_thread(record, state).await {
            self.remove_surface(record, state, now).await?;
            return Err(error);
        }
        if let Some(queued) = &state.queued {
            // Discord audits involve network awaits. Re-read database policy
            // afterwards so changes made during those reads block this send.
            if !self
                .delivery_is_current(record, state, now, queued.is_live)
                .await?
            {
                self.discord
                    .delete_spectator_channel(guild, channel, &record.marker)
                    .await?;
                record.channel_id = None;
                state.reset_surface();
                state.map_create_started_at = None;
                state.pending_map = None;
                state.queued = None;
                state.previous = None;
                self.save(record, state, now).await?;
                return Ok(false);
            }
            let publish = !queued.is_live
                || (pending.state.betting_closed()
                    && !pending.state.betting_open(now)
                    && now.saturating_sub(queued.created_at) <= 90
                    && self
                        .live
                        .snapshot(record.guild_id, record.pending_match_id)
                        .is_some_and(|s| !s.stale));
            if publish {
                self.discord
                    .send_message_with_delivery_key(
                        thread,
                        &queued.key,
                        DiscordMessage::silent(InteractionResponse::message(&queued.content)),
                    )
                    .await?;
            }
            if publish {
                self.log_status(
                    (record.guild_id, record.pending_match_id),
                    "commentary",
                    "delivered",
                    now,
                );
            }
            state.queued = None;
            self.save(record, state, now).await?;
        }
        Ok(true)
    }
}
fn bot_hosted(pending: &PendingMatchRecord) -> bool {
    use cama_domain::dota_hosting::{DotaHostingOptions, HostingMode};
    DotaHostingOptions::from_extra(&pending.state.extra).is_ok_and(|options| {
        options.hosting != Some(HostingMode::Manual)
            && pending
                .state
                .extra
                .get("dota_host_account_key")
                .and_then(serde_json::Value::as_str)
                .and_then(|key| key.parse::<u32>().ok())
                .is_some_and(|account| account > 0)
    })
}
fn map_delivery_key(record: &DotaSpectatorRecord, state: &State, channel: u64) -> String {
    let digest = Sha256::digest(format!(
        "{}:map:{channel}:{}",
        record.marker, state.map_generation
    ));
    format!(
        "m{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn map_image_filename(map: Option<(u64, i64)>) -> Option<String> {
    map.map(|(match_id, game_time)| format!("map-{match_id}-{game_time}.png"))
}

fn map_status_changed(current: Option<&MapFreshness>, next: &MapFreshness) -> bool {
    let Some(current) = current else {
        return true;
    };
    // The relative Discord timestamp is tied to the last image that was
    // actually published. Do not edit the message merely because an upstream
    // poll refreshed a source timestamp while the displayed frame stayed the
    // same.
    current.phase != next.phase
        || current.match_id != next.match_id
        || current.game_time != next.game_time
        || current.delay_seconds != next.delay_seconds
}

fn map_status_title(status: &MapFreshness) -> String {
    let clock = status.game_time.map(map_clock);
    match (status.phase, clock) {
        (MapFreshnessPhase::Fresh, Some(clock)) => format!("Latest map · {clock}"),
        (MapFreshnessPhase::Stale, Some(clock)) => format!("Latest map · {clock} · feed stale"),
        (MapFreshnessPhase::Unavailable, Some(clock)) => {
            format!("Latest map · {clock} · feed unavailable")
        }
        (MapFreshnessPhase::Unavailable, None) => "Live map · unavailable".into(),
        (MapFreshnessPhase::Fresh, None) => "Live map · updating".into(),
        (MapFreshnessPhase::Stale, None) => "Live map · feed stale".into(),
        (MapFreshnessPhase::Waiting, _) => "Live map · waiting".into(),
    }
}

fn map_clock(seconds: i64) -> String {
    let value = seconds.unsigned_abs();
    format!(
        "{}{}:{:02}",
        if seconds < 0 { "-" } else { "" },
        value / 60,
        value % 60
    )
}

fn map_observation_label(source_fetched_at: Option<i64>) -> String {
    source_fetched_at
        .filter(|timestamp| *timestamp > 0)
        .map(|timestamp| format!("<t:{timestamp}:R>"))
        .unwrap_or_else(|| "an unknown time".into())
}

fn map_delay_label(delay_seconds: Option<i64>) -> String {
    delay_seconds
        .filter(|delay| *delay >= 0)
        .map(|delay| format!("Delay {delay}s"))
        .unwrap_or_else(|| "Delay unavailable".into())
}

fn map_status_description(status: &MapFreshness) -> String {
    match status.phase {
        MapFreshnessPhase::Waiting => "Waiting for the first live map observation.".into(),
        MapFreshnessPhase::Fresh => format!(
            "Updated {} · {}",
            map_observation_label(status.source_fetched_at),
            map_delay_label(status.delay_seconds),
        ),
        MapFreshnessPhase::Stale => format!(
            "Feed paused · Last update {} · {}",
            map_observation_label(status.source_fetched_at),
            map_delay_label(status.delay_seconds),
        ),
        MapFreshnessPhase::Unavailable => {
            if status.match_id.is_some() && status.game_time.is_some() {
                format!(
                    "Feed unavailable · Last update {} · {}",
                    map_observation_label(status.source_fetched_at),
                    map_delay_label(status.delay_seconds),
                )
            } else {
                "Feed unavailable · Last update unavailable · Delay unavailable".into()
            }
        }
    }
}

fn map_embed(status: &MapFreshness, image: Option<(u64, i64)>) -> InteractionEmbed {
    let mut embed = InteractionEmbed::titled(map_status_title(status))
        .description(map_status_description(status));
    if let Some(filename) = map_image_filename(image) {
        embed = embed.image(format!("attachment://{filename}"));
    }
    embed
}

fn lobby_message(pending: &PendingMatchRecord) -> Option<i64> {
    pending
        .state
        .extra
        .get("spectator_lobby_message_id")
        .and_then(serde_json::Value::as_i64)
        .filter(|id| *id > 0)
}
fn valid_roster(pending: &PendingMatchRecord) -> Result<Vec<u64>, String> {
    let players = pending
        .state
        .participant_ids()
        .into_iter()
        .map(|id| u64::try_from(id).ok().filter(|id| *id > 0))
        .collect::<Option<Vec<_>>>()
        .filter(|ids| ids.len() == 10)
        .ok_or("spectator delivery requires ten real Discord participants")?;
    Ok(players)
}
fn eligible_viewers(subscribers: &[i64], participants: &[u64]) -> Vec<u64> {
    subscribers
        .iter()
        .filter_map(|id| u64::try_from(*id).ok())
        .filter(|id| *id > 0 && !participants.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(80)
        .collect()
}
fn batch(record: &DotaSpectatorRecord, sequence: u64, content: String) -> Batch {
    let digest = Sha256::digest(format!("{}:{sequence}", record.marker));
    let key = format!(
        "s{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    Batch {
        key,
        content,
        is_live: false,
        created_at: 0,
    }
}
#[async_trait]
impl BackgroundWorker for SpectatorWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            tokio::select! {_=context.cancelled()=>return Ok(()),result=self.tick(chrono::Utc::now().timestamp())=>result?,}
            if !context.sleep(MAP_UPDATE_INTERVAL).await {
                return Ok(());
            }
        }
    }
}
pub fn worker_spec(worker: SpectatorWorker) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new("dota_spectators", Arc::new(worker))
}

fn apply_map_display_names(map: &mut LiveMapFrame, frame: &LiveAnnouncementFrame) {
    for hero in &mut map.heroes {
        if let Some(name) = frame
            .players
            .iter()
            .find(|player| player.hero_id == hero.hero_id && player.radiant == hero.radiant)
            .and_then(|player| player.display_name.as_ref())
        {
            hero.player_name = Some(name.clone());
        }
    }
}

#[cfg(test)]
mod tests;
