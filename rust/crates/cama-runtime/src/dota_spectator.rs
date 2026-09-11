//! Private, opt-in spectator delivery. No live details go to the shared thread.
use crate::{
    BackgroundWorker, BackgroundWorkerSpec, InteractionResponse, WorkerContext,
    discord_transport::{DiscordMessage, DiscordTransport},
    dota_live::DotaLiveFeed,
};
use async_trait::async_trait;
use cama_db::{
    dota_spectator_repository::{DotaSpectatorRecord, DotaSpectatorRepository},
    match_runtime::{PendingMatchRecord, PendingMatchRepository},
};
use cama_domain::live_announcements::{LiveAnnouncementFrame, announcement_message};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const RETENTION_SECONDS: i64 = 15 * 60;
const ANNOUNCEMENT_INTERVAL: i64 = 45;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    lobby_message_id: Option<i64>,
    participants: Vec<u64>,
    viewers: Vec<u64>,
    previous: Option<LiveAnnouncementFrame>,
    last_sample_at: i64,
    sequence: u64,
    queued: Option<Batch>,
    creating_until: i64,
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
            .chain(roster)
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
                        "📻 **Match #{} · Spectator lounge**\nKills, net-worth swings, and objectives as the game unfolds. Updates begin once betting closes and a live feed is available.\n_Players cannot access this channel. It closes 15 minutes after the Cama match ends._",
                        record.pending_match_id
                    ),
                ));
                state.sequence += 1;
            }
            self.save(&mut record, &state, now).await?;
        }
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
            state.queued = None;
            state.previous = None;
            self.save(&mut record, &state, now).await?;
            return Err(error);
        }
        if let Some(queued) = &state.queued {
            // Discord audits involve network awaits. Re-read database policy
            // afterwards so changes made during those reads block this send.
            if !self
                .delivery_is_current(&record, &state, now, queued.is_live)
                .await?
            {
                self.discord
                    .delete_spectator_channel(guild, channel, &record.marker)
                    .await?;
                record.channel_id = None;
                state.queued = None;
                state.previous = None;
                self.save(&mut record, &state, now).await?;
                return Ok(());
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
            self.save(&mut record, &state, now).await?;
        }
        if !pending.state.betting_closed()
            || pending.state.betting_open(now)
            || now.saturating_sub(state.last_sample_at) < ANNOUNCEMENT_INTERVAL
        {
            return Ok(());
        }
        let Some(snapshot) = self
            .live
            .snapshot(record.guild_id, record.pending_match_id)
            .filter(|s| !s.stale)
        else {
            return Ok(());
        };
        let Some(frame) = snapshot.announcement_frame else {
            return Ok(());
        };
        let message = announcement_message(state.previous.as_ref(), &frame, snapshot.delay_seconds);
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
        // Persist the exact outbox text before delivery; next tick re-audits
        // permissions and resumes the same deterministic Discord nonce.
        self.save(&mut record, &state, now).await
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
