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

const RETENTION_SECONDS: i64 = 15 * 60;

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
    pending_map: Option<PendingMap>,
    map_create_started_at: Option<i64>,
    map_generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingMap {
    frame: LiveMapFrame,
    created_at: i64,
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
    lease: tokio::sync::Mutex<()>,
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
            lease: tokio::sync::Mutex::new(()),
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
        let _lease = self.lease.lock().await;
        let path = self.path.clone();
        let guilds = self.guilds.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let pending = PendingMatchRepository::new(&path);
            let repository = DotaSpectatorRepository::new(&path);
            for guild in &guilds {
                for game in pending.pending_matches(*guild).map_err(|e| e.to_string())? {
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
            repository.list().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        for row in rows {
            if let Err(error) = self.reconcile(row, now).await {
                tracing::debug!(%error,"spectator delivery withheld; reconciliation will retry");
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
                state.map_message_id = None;
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
                    state.map_message_id = None;
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
                state.map_message_id = None;
                state.map_create_started_at = None;
                state.pending_map = None;
            }
            state.queued = None;
            state.previous = None;
            self.save(&mut record, &state, now).await?;
            return Ok(());
        }
        if record.channel_id.is_none() {
            if state.creating_until > now {
                return Ok(());
            }
            state.creating_until = now.saturating_add(120);
            self.save(&mut record, &state, now).await?;
        }
        if record.channel_id.is_none() || changed {
            let channel = match self
                .discord
                .ensure_spectator_channel(
                    guild,
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
                        state.map_message_id = None;
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
                        "📻 **Match #{} · Spectator lounge**\nGrab a seat. We’ll keep you posted on kills, gold swings, and objectives once betting closes and the live feed comes in.\n_Players can’t see this channel. It closes 15 minutes after the Cama match ends._",
                        record.pending_match_id
                    ),
                ));
                state.sequence += 1;
            }
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
            return Ok(());
        };
        let Some(mut frame) = snapshot.announcement_frame else {
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
        state.pending_map = snapshot
            .map_frame
            .filter(|map| map.match_id == frame.match_id && map.game_time == frame.game_time)
            .map(|frame| PendingMap {
                frame,
                created_at: now,
            });
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

    async fn try_deliver_map(&self, record: &mut DotaSpectatorRecord, state: &mut State, now: i64) {
        if let Err(error) = self.deliver_map(record, state, now).await {
            // A missing icon/render or Discord image failure must not suppress
            // the independent text commentary outbox.
            tracing::debug!(%error, "spectator map withheld; text delivery remains available");
        }
    }

    fn map_is_fresh(&self, record: &DotaSpectatorRecord, map: &PendingMap, now: i64) -> bool {
        now.saturating_sub(map.created_at) <= 90
            && self
                .live
                .snapshot(record.guild_id, record.pending_match_id)
                .filter(|snapshot| !snapshot.stale)
                .and_then(|snapshot| snapshot.map_frame)
                .is_some_and(|latest| {
                    latest.match_id == map.frame.match_id && latest.game_time == map.frame.game_time
                })
    }

    async fn deliver_map(
        &self,
        record: &mut DotaSpectatorRecord,
        state: &mut State,
        now: i64,
    ) -> Result<(), String> {
        let Some(map) = state.pending_map.clone() else {
            return Ok(());
        };
        if !self.map_is_fresh(record, &map, now) {
            state.pending_map = None;
            return self.save(record, state, now).await;
        }
        let channel = record.channel_id.ok_or("spectator channel unavailable")? as u64;
        let guild = u64::try_from(record.guild_id).map_err(|_| "invalid spectator guild")?;
        if state.map_create_started_at.is_none() && state.map_message_id.is_none() {
            state.map_create_started_at = Some(now);
            self.save(record, state, now).await?;
        }
        let frame = map.frame.clone();
        let bytes =
            tokio::task::spawn_blocking(move || crate::dota_spectator_map::render_map(&frame))
                .await
                .map_err(|error| error.to_string())??;
        // One stable create nonce per private channel survives crashes between
        // Discord accepting the image and our receipt being persisted.
        let digest = Sha256::digest(format!(
            "{}:map:{channel}:{}",
            record.marker, state.map_generation
        ));
        let key = format!(
            "m{}",
            digest[..12]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
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
        // Rendering and history lookup can take time. Audit after both, then
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
            state.map_message_id = None;
            state.map_create_started_at = None;
            state.pending_map = None;
            state.queued = None;
            state.previous = None;
            self.save(record, state, now).await?;
            return Ok(());
        }
        if !self.map_is_fresh(record, &map, now) {
            state.pending_map = None;
            return self.save(record, state, now).await;
        }
        let clock = map.frame.game_time;
        let filename = format!("map-{}-{clock}.png", map.frame.match_id);
        let embed = InteractionEmbed::titled(format!(
            "Last received map · {}:{:02}",
            clock.div_euclid(60),
            clock.rem_euclid(60),
        ))
        .image(format!("attachment://{filename}"));
        let message = DiscordMessage::silent(
            InteractionResponse::message("")
                .embed(embed)
                .attachment(InteractionAttachment::bytes(filename, bytes)),
        );
        let message_id = if let Some(id) = existing {
            if let Err(error) = self.discord.edit_message(channel, id, message).await {
                // Only a confirmed missing message permits a new create nonce;
                // transient lookup/edit failures retain the existing identity.
                if self.discord.fetch_message(channel, id).await?.is_none() {
                    state.map_message_id = None;
                    state.map_create_started_at = None;
                    state.map_generation = state.map_generation.saturating_add(1);
                    self.save(record, state, now).await?;
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
        state.map_message_id = Some(message_id);
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
            state.map_message_id = None;
            state.map_create_started_at = None;
            state.pending_map = None;
            state.queued = None;
            state.previous = None;
            self.save(record, state, now).await?;
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
                state.map_message_id = None;
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
                        channel,
                        &queued.key,
                        DiscordMessage::silent(InteractionResponse::message(&queued.content)),
                    )
                    .await?;
            }
            state.queued = None;
            self.save(record, state, now).await?;
        }
        Ok(true)
    }
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
            if !context.sleep(Duration::from_secs(15)).await {
                return Ok(());
            }
        }
    }
}
pub fn worker_spec(worker: SpectatorWorker) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new("dota_spectators", Arc::new(worker))
}

#[cfg(test)]
mod tests;
