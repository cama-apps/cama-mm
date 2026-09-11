//! Durable orchestration of the dedicated Steam account and Cama matches.
//!
//! Persist intent before remote mutations. A lost response is reconciled from
//! the GC cache; it never authorizes a second lobby or an inferred match win.

mod steam;
#[cfg(test)]
mod tests;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use cama_db::{
    core_repositories::MatchRepository,
    dota_session_repository::{
        DotaSessionClaim, DotaSessionPhase as Phase, DotaSessionRecord, DotaSessionRepository,
    },
    guild_config_repository::GuildConfigRepository,
    match_runtime::{PendingMatchRecord, PendingMatchRepository, PendingMatchRepositoryError},
    opendota_player::OpenDotaPlayerRepository,
};
use cama_domain::{
    dota_lobby::{self, ExpectedPlayer, LobbySeat, Side},
    guild_config::GuildConfigStore,
};
use serde::{Deserialize, Serialize};

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::dota_host_config::DotaHostConfig;
use crate::dota_replay::{ArchivedReplay, ReplayArchive};
use crate::match_provider::{
    HostedMatchPlayer, HostedMatchResult, HostedMatchRosterEntry, MatchRegistrationProvider,
};
use crate::{BackgroundWorker, BackgroundWorkerSpec, InteractionResponse, WorkerContext};

pub use steam::bootstrap_steam_login;

#[derive(Clone, Serialize, Deserialize)]
pub struct LobbySettings {
    pub name: String,
    pub password: String,
    /// Persist the visibility so a restart preserves an existing session's
    /// settings. Older sessions were unlisted; newly created games are public.
    #[serde(default = "legacy_lobby_visibility")]
    pub visibility: i32,
    pub league_id: u32,
    pub game_mode: u32,
    pub server_region: u32,
    pub first_pick_radiant: Option<bool>,
    pub tv_delay: u32,
}

const PUBLIC_LOBBY_VISIBILITY: i32 = 0;

fn legacy_lobby_visibility() -> i32 {
    2
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LobbyStage {
    Gathering,
    Allocating,
    Running,
    Postgame,
}

#[derive(Clone, Debug)]
pub struct HostLobby {
    pub id: u64,
    pub name: String,
    pub owner_account_id: u32,
    pub league_id: u32,
    pub game_mode: u32,
    pub server_region: u32,
    pub first_pick_radiant: Option<bool>,
    pub cheats: bool,
    pub fill_bots: bool,
    pub spectating: bool,
    pub tv_delay: u32,
    pub visibility: i32,
    pub stage: LobbyStage,
    /// Game-rules phase; RUN alone also includes the hero draft.
    pub game_state: Option<i32>,
    pub members: Vec<LobbySeat>,
    pub match_id: Option<u64>,
    pub server_id: Option<u64>,
    pub winner: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReplayMetadata {
    Pending,
    Available { cluster: u32, salt: u32 },
    NotRecorded,
    Expired,
}

#[derive(Clone, Debug)]
pub struct HostMatchDetails {
    pub match_id: u64,
    pub league_id: u32,
    pub winner: Option<String>,
    pub finished: bool,
    pub players: Vec<HostedMatchPlayer>,
    pub replay: ReplayMetadata,
}

#[async_trait]
pub trait DotaHostPort: Send + Sync {
    /// None is authoritative only after initial GC cache hydration. Connection
    /// loss and stale/incomplete cache must return an error.
    async fn snapshot(&self) -> Result<Option<HostLobby>, String>;
    async fn create(&self, settings: &LobbySettings) -> Result<(), String>;
    async fn invite(&self, lobby_id: u64, account_id: u32) -> Result<(), String>;
    async fn move_host_to_pool(&self, lobby_id: u64) -> Result<(), String>;
    async fn kick_from_team(&self, lobby_id: u64, account_id: u32) -> Result<(), String>;
    async fn kick(&self, lobby_id: u64, account_id: u32) -> Result<(), String>;
    async fn launch(&self, lobby_id: u64) -> Result<(), String>;
    async fn destroy(&self, lobby_id: u64) -> Result<(), String>;
    async fn match_details(&self, match_id: u64) -> Result<HostMatchDetails, String>;
}

#[async_trait]
pub trait HostedMatchRecorder: Send + Sync {
    async fn record(&self, result: HostedMatchResult) -> Result<i64, String>;
    async fn betting_closed(&self, guild: i64, pending: i64) -> Result<(), String>;
    async fn betting_window_changed(&self, guild: i64, pending: i64) -> Result<(), String>;
    /// Type-erased RAII lease shared with manual recording/abort. Holding it
    /// across the final checks and launch keeps those operations exclusive.
    fn try_acquire_launch_guard(&self, guild: i64, pending: i64) -> Option<Box<dyn Send>>;
}

#[async_trait]
impl HostedMatchRecorder for MatchRegistrationProvider {
    async fn record(&self, result: HostedMatchResult) -> Result<i64, String> {
        self.record_hosted_match(result).await
    }
    async fn betting_closed(&self, guild: i64, pending: i64) -> Result<(), String> {
        self.hosted_betting_closed(guild, pending).await
    }
    async fn betting_window_changed(&self, guild: i64, pending: i64) -> Result<(), String> {
        self.hosted_betting_window_changed(guild, pending).await
    }
    fn try_acquire_launch_guard(&self, guild: i64, pending: i64) -> Option<Box<dyn Send>> {
        self.try_acquire_hosted_finalization_guard(guild, pending)
            .map(|guard| Box::new(guard) as Box<dyn Send>)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionState {
    settings: LobbySettings,
    roster: Vec<HostedMatchRosterEntry>,
    channel_id: Option<u64>,
    #[serde(default)]
    message_id: Option<u64>,
    #[serde(default)]
    last_message: String,
    #[serde(default)]
    last_message_at: i64,
    #[serde(default)]
    last_invite_at: i64,
    #[serde(default)]
    create_requested_at: Option<i64>,
    #[serde(default)]
    launch_requested_at: Option<i64>,
    #[serde(default)]
    betting_closed: bool,
    #[serde(default)]
    betting_window_announced: bool,
    #[serde(default)]
    betting_notification_sent: bool,
    #[serde(default)]
    last_betting_notification_at: i64,
    #[serde(default)]
    cancel_requested: bool,
    #[serde(default)]
    resume_requested: bool,
    #[serde(default)]
    recorded_match_id: Option<i64>,
    #[serde(default)]
    server_id: Option<u64>,
    #[serde(default)]
    replay: Option<ReplayMetadata>,
    #[serde(default)]
    archive: Option<ArchivedReplay>,
    #[serde(default)]
    replay_last_attempt: i64,
    #[serde(default)]
    replay_error: Option<String>,
    #[serde(default)]
    last_result_poll: i64,
    #[serde(default)]
    lobby_deadline: i64,
    #[serde(default)]
    recording_failures: u32,
}

impl SessionState {
    fn expected(&self) -> Vec<ExpectedPlayer> {
        self.roster
            .iter()
            .map(|p| ExpectedPlayer {
                discord_id: p.discord_id,
                account_id: p.steam_account_id,
                side: if p.radiant { Side::Radiant } else { Side::Dire },
            })
            .collect()
    }
}

pub struct DotaHostWorker {
    path: PathBuf,
    config: DotaHostConfig,
    recorder: Arc<dyn HostedMatchRecorder>,
    discord: Arc<dyn DiscordTransport>,
    live: Arc<crate::dota_live::DotaLiveFeed>,
}

impl DotaHostWorker {
    pub fn new(
        path: impl AsRef<Path>,
        config: DotaHostConfig,
        recorder: Arc<dyn HostedMatchRecorder>,
        discord: Arc<dyn DiscordTransport>,
        live: Arc<crate::dota_live::DotaLiveFeed>,
    ) -> Self {
        Self {
            path: path.as_ref().into(),
            config,
            recorder,
            discord,
            live,
        }
    }

    fn account_key(&self) -> String {
        self.config.account_id.to_string()
    }

    async fn tick(&self, port: &dyn DotaHostPort, now: i64) -> Result<(), String> {
        let Some(mut record) = self.discover(now).await? else {
            return Ok(());
        };
        let mut state: SessionState = serde_json::from_value(record.payload.clone())
            .map_err(|_| "invalid durable Dota session payload")?;
        if record.phase == Phase::NeedsReview {
            if state.resume_requested {
                state.resume_requested = false;
                state.cancel_requested = false;
                state.recording_failures = 0;
                record.last_error = None;
                record.phase = if record.valve_match_id.is_some() {
                    Phase::Running
                } else {
                    Phase::Creating
                };
                state.lobby_deadline = now.saturating_add(self.config.lobby_timeout_seconds as i64);
                if record.valve_match_id.is_none() {
                    state.create_requested_at = None;
                    state.launch_requested_at = None;
                }
                self.save(&mut record, &state, now).await?;
            } else if !state.cancel_requested {
                self.monitor_review_betting(port, &mut record, &mut state, now)
                    .await?;
                return Ok(());
            }
        }
        let lobby = match port.snapshot().await {
            Ok(lobby) => lobby,
            Err(error) => {
                // GC disconnection is not an authoritative empty lobby, but
                // an already registered spectator can still confirm start.
                if let Some(match_id) = record
                    .valve_match_id
                    .as_deref()
                    .and_then(|id| id.parse().ok())
                {
                    self.publish_live(&record, &state, match_id).await;
                    if self.live.gameplay_started(
                        record.guild_id,
                        record.pending_match_id,
                        match_id,
                    ) {
                        self.close_betting(&mut record, &mut state, now).await?;
                        self.publish_live(&record, &state, match_id).await;
                    }
                }
                return Err(error);
            }
        };
        let pending = self.pending(&record).await?;
        let server_launched = record.valve_match_id.is_some()
            || state.launch_requested_at.is_some()
            || matches!(record.phase, Phase::Running | Phase::Finishing)
            || lobby
                .as_ref()
                .is_some_and(|lobby| lobby.stage != LobbyStage::Gathering);
        let deadline = if state.lobby_deadline > 0 {
            state.lobby_deadline
        } else {
            record
                .created_at
                .saturating_add(self.config.lobby_timeout_seconds as i64)
        };
        if !server_launched && (pending.is_none() || now > deadline) {
            state.cancel_requested = true;
            self.save(&mut record, &state, now).await?;
        }
        if let Some(lobby) = &lobby {
            if !owns_lobby(&record, &state, lobby, self.config.account_id) {
                return self
                    .review(
                        &mut record,
                        &mut state,
                        "the account has a different lobby; no action was taken on it",
                        now,
                    )
                    .await;
            }
            if record.lobby_id.is_none() {
                record.lobby_id = Some(lobby.id.to_string());
                self.save(&mut record, &state, now).await?;
            }
            if let Some(match_id) = lobby.match_id {
                if record
                    .valve_match_id
                    .as_ref()
                    .is_some_and(|id| id != &match_id.to_string())
                {
                    return self
                        .review(
                            &mut record,
                            &mut state,
                            "the GC match ID changed unexpectedly",
                            now,
                        )
                        .await;
                }
                if record.valve_match_id.is_none() {
                    record.valve_match_id = Some(match_id.to_string());
                    self.save(&mut record, &state, now).await?;
                }
            }
            if let Some(server_id) = lobby.server_id
                && state.server_id != Some(server_id)
            {
                state.server_id = Some(server_id);
                self.save(&mut record, &state, now).await?;
            }
        }
        if state.recorded_match_id.is_some() {
            if let Some(lobby) = lobby {
                if lobby.stage != LobbyStage::Postgame {
                    // Match details may reach us before the lobby's final
                    // update. Never destroy a server still reported active.
                    return Ok(());
                }
                port.destroy(lobby.id).await?;
                return Ok(());
            }
            record.phase = Phase::Recorded;
            self.save(&mut record, &state, now).await?;
            self.live
                .finish_match(record.guild_id, record.pending_match_id)
                .await;
            let content = format!(
                "Dota match {} recorded automatically as Cama match {}. Replay archival runs separately.",
                record.valve_match_id.as_deref().unwrap_or("unknown"),
                state.recorded_match_id.unwrap_or_default()
            );
            return self.announce(&mut record, &mut state, &content, now).await;
        }
        if state.cancel_requested {
            if record.valve_match_id.is_some()
                || state.launch_requested_at.is_some()
                || matches!(record.phase, Phase::Running | Phase::Finishing)
                || lobby
                    .as_ref()
                    .is_some_and(|l| l.stage != LobbyStage::Gathering)
            {
                state.cancel_requested = false;
                return self.review(&mut record,&mut state,"cancellation cannot discard a launched or completed Dota match; inspect the result and resume reconciliation",now).await;
            }
            if let Some(lobby) = lobby {
                port.destroy(lobby.id).await?;
                return Ok(());
            }
            if record.lobby_id.is_none() && state.create_requested_at.is_some() {
                state.cancel_requested = false;
                return self.review(&mut record,&mut state,"lobby creation is still unconfirmed; reconcile the Steam account before releasing it",now).await;
            }
            self.release_betting_window(&record, now).await?;
            record.phase = Phase::Cancelled;
            self.save(&mut record, &state, now).await?;
            self.live
                .finish_match(record.guild_id, record.pending_match_id)
                .await;
            return self.announce(&mut record,&mut state,"Dota hosting stopped. The Cama match can still be handled with the existing record/abort commands.",now).await;
        }
        if server_launched && pending.is_none() && self.recorded_id(&record).await?.is_none() {
            return self.review(&mut record,&mut state,"the Cama pending match was removed without a recorded result after Dota launch; inspect the manual abort before proceeding",now).await;
        }
        if !state.betting_closed
            && let Some(pending) = &pending
            && lobby
                .as_ref()
                .is_none_or(|lobby| settings_match(&state.settings, lobby))
        {
            if pending.state.betting_closed() || self.recorded_id(&record).await?.is_some() {
                // Restore the observed start without undoing any explicit
                // admin extension stored alongside it.
                state.betting_closed = true;
                self.save(&mut record, &state, now).await?;
            } else {
                self.manage_betting_window(&mut record, &mut state, pending, now)
                    .await?;
            }
        }
        // The GC's lobby lifecycle can lag behind the game-rules state or
        // spectator feed. Observe known matches before allocation/launch
        // waiting paths can return early.
        if let Some(match_id) = record
            .valve_match_id
            .as_deref()
            .and_then(|id| id.parse().ok())
        {
            self.publish_live(&record, &state, match_id).await;
            if state.betting_closed
                || lobby.as_ref().is_some_and(|lobby| {
                    lobby.stage == LobbyStage::Postgame
                        || lobby
                            .game_state
                            .is_some_and(dota_lobby::gameplay_has_started)
                })
                || self
                    .live
                    .gameplay_started(record.guild_id, record.pending_match_id, match_id)
            {
                self.close_betting(&mut record, &mut state, now).await?;
                self.publish_live(&record, &state, match_id).await;
            }
        }
        let Some(lobby) = lobby else {
            if let Some(match_id) = record
                .valve_match_id
                .as_deref()
                .and_then(|id| id.parse().ok())
            {
                return self
                    .finish(port, &mut record, &mut state, match_id, now)
                    .await;
            }
            if record.lobby_id.is_some() || state.launch_requested_at.is_some() {
                return self
                    .review(
                        &mut record,
                        &mut state,
                        "the hosted lobby disappeared before its match ID was observed",
                        now,
                    )
                    .await;
            }
            if let Some(sent) = state.create_requested_at {
                if now.saturating_sub(sent) > 90 {
                    return self.review(&mut record,&mut state,"lobby creation was requested but has not been observed; inspect the Steam account before resuming",now).await;
                }
                return Ok(());
            }
            state.create_requested_at = Some(now);
            self.save(&mut record, &state, now).await?;
            port.create(&state.settings).await?;
            return self.announce(&mut record,&mut state,"Creating the league lobby. Open Dota and allow lobby invitations from non-friends; accept the invite and select your assigned side.",now).await;
        };
        if matches!(lobby.stage, LobbyStage::Running | LobbyStage::Postgame) {
            if !settings_match(&state.settings, &lobby) {
                return self
                    .review(
                        &mut record,
                        &mut state,
                        "the running Dota lobby settings differ from the saved league game",
                        now,
                    )
                    .await;
            }
            if state.betting_closed
                || lobby
                    .game_state
                    .is_some_and(dota_lobby::gameplay_has_started)
                || lobby.stage == LobbyStage::Postgame
            {
                self.close_betting(&mut record, &mut state, now).await?;
            }
            record.phase = if lobby.stage == LobbyStage::Postgame {
                Phase::Finishing
            } else {
                Phase::Running
            };
            self.save(&mut record, &state, now).await?;
            if let Some(match_id) = lobby.match_id {
                let summary = self.match_status_summary(&record, now).await?;
                self.announce(
                    &mut record,
                    &mut state,
                    &format!(
                        "Dota match {match_id} is {}. {summary}",
                        if lobby.stage == LobbyStage::Postgame {
                            "finishing"
                        } else {
                            "running"
                        }
                    ),
                    now,
                )
                .await?;
                return self
                    .finish(port, &mut record, &mut state, match_id, now)
                    .await;
            }
            return Ok(());
        }
        if lobby.stage == LobbyStage::Allocating {
            if state
                .launch_requested_at
                .is_some_and(|t| now.saturating_sub(t) > 300)
            {
                return self
                    .review(
                        &mut record,
                        &mut state,
                        "Dota server allocation has not completed after five minutes",
                        now,
                    )
                    .await;
            }
            return Ok(());
        }
        if !settings_match(&state.settings, &lobby) {
            return self.review(&mut record,&mut state,"league, game mode, pick priority, or safety settings differ from the saved lobby settings",now).await;
        }
        if let Some(sent) = state.launch_requested_at {
            if now.saturating_sub(sent) > 90 {
                return self
                    .review(
                        &mut record,
                        &mut state,
                        "launch was requested but Dota has not allocated a server",
                        now,
                    )
                    .await;
            }
            // Do not resend an ambiguous launch or change its roster while
            // waiting for the allocation update.
            return Ok(());
        }
        let Some(pending) = pending else {
            return Ok(());
        };
        if !pending_matches_roster(&pending, &state.roster) {
            return self
                .review(
                    &mut record,
                    &mut state,
                    "the Cama roster changed after the Steam account mapping was frozen",
                    now,
                )
                .await;
        }
        let admission =
            dota_lobby::assess_admission(&state.expected(), &lobby.members, self.config.account_id);
        if lobby
            .members
            .iter()
            .any(|m| m.account_id == self.config.account_id && m.side.is_some())
        {
            port.move_host_to_pool(lobby.id).await?;
        }
        for account in admission
            .unexpected_players
            .iter()
            .filter(|id| **id != self.config.account_id)
        {
            port.kick(lobby.id, *account).await?;
        }
        for account in &admission.wrong_side {
            if lobby
                .members
                .iter()
                .any(|m| m.account_id == *account && m.side.is_some())
            {
                port.kick_from_team(lobby.id, *account).await?;
            }
        }
        if now.saturating_sub(state.last_invite_at) >= 60 {
            state.last_invite_at = now;
            self.save(&mut record, &state, now).await?;
            for account in &admission.missing {
                port.invite(lobby.id, *account).await?;
            }
        }
        if !admission.ready {
            if record.phase != Phase::Launching {
                record.phase = Phase::Gathering;
            }
            self.save(&mut record, &state, now).await?;
            let content = format!(
                "Dota lobby ready: **{}**. Accept your invite and choose your assigned side. {}/10 correctly seated; {} missing, {} on the wrong side. The match starts automatically after all ten are seated.",
                state.settings.name,
                10_usize.saturating_sub(admission.missing.len() + admission.wrong_side.len()),
                admission.missing.len(),
                admission.wrong_side.len()
            );
            return self.announce(&mut record, &mut state, &content, now).await;
        }
        let Some(_launch_guard) = self
            .recorder
            .try_acquire_launch_guard(record.guild_id, record.pending_match_id)
        else {
            return Ok(());
        };
        record.phase = Phase::Launching;
        state.launch_requested_at = Some(now);
        self.save(&mut record, &state, now).await?;
        let latest = port
            .snapshot()
            .await?
            .ok_or("lobby disappeared during launch checks")?;
        if !owns_lobby(&record, &state, &latest, self.config.account_id)
            || !settings_match(&state.settings, &latest)
            || latest.stage != LobbyStage::Gathering
            || !dota_lobby::assess_admission(
                &state.expected(),
                &latest.members,
                self.config.account_id,
            )
            .ready
            || self
                .pending(&record)
                .await?
                .is_none_or(|p| !pending_matches_roster(&p, &state.roster))
            || self.recorded_id(&record).await?.is_some()
        {
            state.launch_requested_at = None;
            record.phase = Phase::Gathering;
            self.save(&mut record, &state, now).await?;
            return Ok(());
        }
        port.launch(lobby.id).await?;
        self.announce(&mut record,&mut state,"All ten players are on the correct sides. Dota server launch requested; betting remains open through the hero draft.",now).await
    }

    async fn manage_betting_window(
        &self,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        pending: &PendingMatchRecord,
        now: i64,
    ) -> Result<(), String> {
        if !pending.state.hosted_betting_managed() {
            let repo = PendingMatchRepository::new(&self.path);
            let (guild, pending_id) = (record.guild_id, record.pending_match_id);
            let adopted = blocking(move || {
                repo.begin_hosted_betting(guild, pending_id, now)
                    .map_err(|error| error.to_string())
            })
            .await?;
            if adopted.state.betting_closed() {
                state.betting_closed = true;
                self.save(record, state, now).await?;
            }
        }
        if !state.betting_window_announced {
            match self
                .recorder
                .betting_window_changed(record.guild_id, record.pending_match_id)
                .await
            {
                Ok(()) => {
                    state.betting_window_announced = true;
                    self.save(record, state, now).await?;
                }
                Err(error) => tracing::warn!(%error, "hosted betting window display will retry"),
            }
        }
        Ok(())
    }

    async fn monitor_review_betting(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<(), String> {
        let Some(match_id) = record
            .valve_match_id
            .as_deref()
            .and_then(|id| id.parse().ok())
        else {
            return Ok(());
        };
        if self.pending(record).await?.is_none() && self.recorded_id(record).await?.is_none() {
            return Ok(());
        }
        self.publish_live(record, state, match_id).await;
        // An orchestration problem must not leave betting open after known
        // gameplay starts. Continue observing the original match while all
        // lobby mutations and automatic result recording remain paused.
        let gc_started = port.snapshot().await.ok().flatten().is_some_and(|lobby| {
            owns_lobby(record, state, &lobby, self.config.account_id)
                && lobby.match_id == Some(match_id)
                && (lobby.stage == LobbyStage::Postgame
                    || lobby
                        .game_state
                        .is_some_and(dota_lobby::gameplay_has_started))
        });
        if state.betting_closed
            || gc_started
            || self
                .live
                .gameplay_started(record.guild_id, record.pending_match_id, match_id)
        {
            self.close_betting(record, state, now).await?;
            self.publish_live(record, state, match_id).await;
        }
        Ok(())
    }

    async fn release_betting_window(
        &self,
        record: &DotaSessionRecord,
        now: i64,
    ) -> Result<(), String> {
        let repo = PendingMatchRepository::new(&self.path);
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        blocking(
            move || match repo.release_hosted_betting(guild, pending, now) {
                Ok(_)
                | Err(
                    PendingMatchRepositoryError::PendingMatchNotFound(_)
                    | PendingMatchRepositoryError::MatchAlreadyRecorded(_)
                    | PendingMatchRepositoryError::BettingClosedByDotaSession(_),
                ) => Ok(()),
                Err(error) => Err(error.to_string()),
            },
        )
        .await?;
        if let Err(error) = self.recorder.betting_window_changed(guild, pending).await {
            tracing::warn!(%error, "released betting window display refresh failed");
        }
        Ok(())
    }

    async fn close_betting(
        &self,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<(), String> {
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        if !state.betting_closed {
            // A manual settlement already rejects further wagers. Keep
            // following its Valve game so final identity can be reconciled.
            if self.recorded_id(record).await?.is_none() {
                let repo = PendingMatchRepository::new(&self.path);
                blocking(move || {
                    repo.close_betting_now(guild, pending, now)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .await?;
            }
            state.betting_closed = true;
            self.save(record, state, now).await?;
        }
        if state.betting_notification_sent
            || now.saturating_sub(state.last_betting_notification_at) < 30
        {
            return Ok(());
        }
        state.last_betting_notification_at = now;
        self.save(record, state, now).await?;
        // Display/reminder delivery failure must not reopen a closed window.
        if self.recorder.betting_closed(guild, pending).await.is_err() {
            tracing::warn!(
                guild_id = guild,
                pending_match_id = pending,
                "Dota bets closed; wager display refresh will retry"
            );
        } else {
            state.betting_notification_sent = true;
            self.save(record, state, now).await?;
        }
        Ok(())
    }

    async fn match_status_summary(
        &self,
        record: &DotaSessionRecord,
        now: i64,
    ) -> Result<String, String> {
        if self.recorded_id(record).await?.is_some() {
            return Ok("Betting is closed.".to_owned());
        }
        // The session's close flag records confirmed gameplay, while an admin
        // can subsequently reopen a timed window in the pending-match row.
        if let Some(pending) = self.pending(record).await?
            && pending.state.betting_open(now)
        {
            if pending.state.hosted_betting_managed() {
                return Ok(match pending
                    .state
                    .betting_extension_until()
                    .filter(|deadline| *deadline > now)
                {
                    Some(deadline) => format!(
                        "Betting remains open through the draft and at least until <t:{deadline}:R> (admin extension)."
                    ),
                    None => "Betting remains open through the draft; waiting for confirmed gameplay start.".to_owned(),
                });
            }
            return Ok(format!(
                "Betting is open until <t:{}:R> (admin extension).",
                pending.state.bet_lock_until.unwrap_or_default()
            ));
        }
        Ok(self
            .live
            .summary(record.guild_id, record.pending_match_id)
            .await
            .unwrap_or_else(|| "Betting is closed.".to_owned()))
    }

    async fn publish_live(&self, record: &DotaSessionRecord, state: &SessionState, match_id: u64) {
        self.live
            .publish_match(
                record.guild_id,
                record.pending_match_id,
                match_id,
                state.server_id,
                state.settings.league_id,
                state.settings.tv_delay,
                state.betting_closed,
                state.roster.iter().map(|p| p.steam_account_id).collect(),
            )
            .await;
    }

    async fn finish(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        match_id: u64,
        now: i64,
    ) -> Result<(), String> {
        if now.saturating_sub(state.last_result_poll) < 30 {
            return Ok(());
        }
        state.last_result_poll = now;
        self.save(record, state, now).await?;
        let details = match port.match_details(match_id).await {
            Ok(details) => details,
            Err(reason) => {
                // The GC lobby can report a completed result before the match
                // details service publishes it. Use that independent Valve
                // evidence only while its exact ten account/side entries are
                // still present; never substitute a guessed winner or GSI.
                let fallback = port.snapshot().await?.filter(|lobby| {
                    lobby.stage == LobbyStage::Postgame
                        && lobby.match_id == Some(match_id)
                        && lobby.winner.is_some()
                        && owns_lobby(record, state, lobby, self.config.account_id)
                        && settings_match(&state.settings, lobby)
                });
                let Some(lobby) = fallback else {
                    return Err(reason);
                };
                let players = lobby
                    .members
                    .iter()
                    .filter_map(|m| {
                        m.side.map(|side| HostedMatchPlayer {
                            account32: m.account_id,
                            radiant: side == Side::Radiant,
                        })
                    })
                    .collect::<Vec<_>>();
                if !result_matches_roster(&players, &state.roster) {
                    return Err(reason);
                }
                HostMatchDetails {
                    match_id,
                    league_id: lobby.league_id,
                    winner: lobby.winner,
                    finished: true,
                    players,
                    replay: ReplayMetadata::Pending,
                }
            }
        };
        if !details.finished {
            return Ok(());
        }
        if details.match_id != match_id
            || details.league_id != state.settings.league_id
            || !result_matches_roster(&details.players, &state.roster)
        {
            return self.review(record,state,"the completed Valve match does not match the frozen league and ten-player roster",now).await;
        }
        // Final match details can recover a missed live state after reconnect.
        self.close_betting(record, state, now).await?;
        let Some(winner) = details
            .winner
            .filter(|w| matches!(w.as_str(), "radiant" | "dire"))
        else {
            return self.review(record,state,"Valve reported no normal Radiant/Dire winner; automatic settlement is withheld",now).await;
        };
        state.replay = Some(details.replay);
        record.phase = Phase::Finishing;
        self.save(record, state, now).await?;
        let request = HostedMatchResult {
            guild_id: record.guild_id,
            pending_match_id: record.pending_match_id,
            valve_match_id: match_id,
            winning_team: winner,
            expected_roster: state.roster.clone(),
            players: details.players,
        };
        match self.recorder.record(request).await {
            Ok(id) => {
                state.recorded_match_id = Some(id);
                state.recording_failures = 0;
                record.last_error = None;
                self.save(record, state, now).await
            }
            Err(reason) => {
                state.recording_failures = state.recording_failures.saturating_add(1);
                if state.recording_failures >= 5 {
                    return self
                        .review(
                            record,
                            state,
                            &format!("automatic recording needs attention: {reason}"),
                            now,
                        )
                        .await;
                }
                record.last_error = Some(reason);
                self.save(record, state, now).await?;
                self.announce(record,state,"The Dota result is verified. Cama settlement is delayed; the bot will retry automatically.",now).await
            }
        }
    }

    async fn save(
        &self,
        record: &mut DotaSessionRecord,
        state: &SessionState,
        now: i64,
    ) -> Result<(), String> {
        record.payload = serde_json::to_value(state).map_err(|e| e.to_string())?;
        let repo = DotaSessionRepository::new(&self.path);
        let update = record.clone();
        *record = blocking(move || {
            repo.update(&update, update.revision, now)
                .map_err(|e| e.to_string())
        })
        .await?;
        Ok(())
    }

    async fn discover(&self, now: i64) -> Result<Option<DotaSessionRecord>, String> {
        let path = self.path.clone();
        let config = self.config.clone();
        let key = self.account_key();
        let (active, changed_windows) = blocking(move || {
            let sessions = DotaSessionRepository::new(&path);
            let mut active = sessions.active_sessions().map_err(|e| e.to_string())?.into_iter().find(|s| s.account_key == key);
            let mut changed_windows = Vec::new();
            let pending_repo = PendingMatchRepository::new(&path);
            let links = OpenDotaPlayerRepository::new(&path);
            let guilds = GuildConfigRepository::new(&path, false);
            let mut pending = Vec::new();
            for guild in &config.guild_ids { pending.extend(pending_repo.pending_matches(*guild).map_err(|e| e.to_string())?); }
            pending.sort_by_key(|p| (p.state.shuffle_timestamp.unwrap_or(0), p.pending_match_id));
            for pending in pending {
                if pending.state.shuffle_timestamp.is_none_or(|t| t < config.start_after)
                    || sessions.session(pending.guild_id, pending.pending_match_id).map_err(|e| e.to_string())?.is_some() { continue; }
                let Some(league) = guilds.get_league_id(pending.guild_id).map_err(|e| e.to_string())?
                    .and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0) else {
                    tracing::warn!(guild_id=pending.guild_id,"Dota hosting is waiting for /enrich setleague");
                    continue;
                };
                let mut roster = Vec::new();
                for (radiant, players) in [(true,&pending.state.radiant_team_ids),(false,&pending.state.dire_team_ids)] {
                    for discord_id in players {
                        if let Some(account) = links.get_steam_id(*discord_id).map_err(|e| e.to_string())?.and_then(dota_lobby::account_id) {
                            roster.push(HostedMatchRosterEntry { discord_id:*discord_id, steam_account_id:account,radiant });
                        }
                    }
                }
                let state = SessionState {
                    settings: LobbySettings { name:format!("Cama {}:{}",pending.guild_id,pending.pending_match_id),
                        password:String::new(), visibility:PUBLIC_LOBBY_VISIBILITY, league_id:league,
                        game_mode:config.game_mode, server_region:config.server_region,
                        first_pick_radiant:pending.state.first_pick_team.as_deref().and_then(|s| match s.to_ascii_lowercase().as_str() { "radiant" => Some(true),"dire"=>Some(false),_=>None }),
                        tv_delay:config.tv_delay },
                    roster, channel_id:pending_channel(&pending),message_id:None,last_message:String::new(),last_message_at:0,
                    last_invite_at:0,create_requested_at:None,launch_requested_at:None,betting_closed:false,betting_window_announced:false,betting_notification_sent:false,last_betting_notification_at:0,cancel_requested:false,
                    resume_requested:false,recorded_match_id:None,server_id:None,replay:None,archive:None,replay_last_attempt:0,replay_error:None,last_result_poll:0,lobby_deadline:0,recording_failures:0,
                };
                if let Err(reason) = dota_lobby::validate_roster(&state.expected(),config.account_id) {
                    tracing::warn!(guild_id=pending.guild_id,pending_match_id=pending.pending_match_id,%reason,"Dota hosting waiting for valid primary account links");
                    continue;
                }
                // Eligible queued games also wait for gameplay rather than
                // expiring while this account is occupied by another match.
                if !pending.state.hosted_betting_managed() && !pending.state.betting_closed() {
                    match pending_repo.begin_hosted_betting(pending.guild_id, pending.pending_match_id, now) {
                        Ok(adopted) if adopted.state.hosted_betting_managed() => {
                            changed_windows.push((pending.guild_id, pending.pending_match_id));
                        }
                        Ok(_) | Err(PendingMatchRepositoryError::PendingMatchNotFound(_)
                            | PendingMatchRepositoryError::MatchAlreadyRecorded(_)) => continue,
                        Err(error) => return Err(error.to_string()),
                    }
                }
                if active.is_some() { continue; }
                let payload = serde_json::to_value(&state).map_err(|e| e.to_string())?;
                active = Some(match sessions.claim_session(pending.guild_id,pending.pending_match_id,&key,payload,now)
                    .map_err(|e| e.to_string())? {
                        DotaSessionClaim::Created(s)|DotaSessionClaim::Existing(s)|DotaSessionClaim::Busy(s) => s,
                    });
            }
            Ok((active, changed_windows))
        }).await?;
        for (guild, pending) in changed_windows {
            if let Err(error) = self.recorder.betting_window_changed(guild, pending).await {
                tracing::warn!(%error, guild, pending, "queued betting window display refresh failed");
            }
        }
        Ok(active)
    }

    async fn pending(
        &self,
        record: &DotaSessionRecord,
    ) -> Result<Option<PendingMatchRecord>, String> {
        let repo = PendingMatchRepository::new(&self.path);
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        blocking(move || {
            repo.pending_match(guild, pending)
                .map_err(|e| e.to_string())
        })
        .await
    }

    async fn recorded_id(&self, record: &DotaSessionRecord) -> Result<Option<i64>, String> {
        let repo = MatchRepository::new(&self.path);
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        blocking(move || {
            repo.match_id_for_pending_match(guild, pending)
                .map_err(|e| e.to_string())
        })
        .await
    }

    async fn review(
        &self,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        reason: &str,
        now: i64,
    ) -> Result<(), String> {
        record.phase = Phase::NeedsReview;
        record.last_error = Some(reason.to_owned());
        self.save(record, state, now).await?;
        self.announce(record,state,&format!("Hosting paused: {reason}. An operator can inspect `dota-host status` and resume or cancel this session."),now).await
    }

    async fn announce(
        &self,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        content: &str,
        now: i64,
    ) -> Result<(), String> {
        if state.last_message == content || now.saturating_sub(state.last_message_at) < 15 {
            return Ok(());
        }
        let Some(channel) = state.channel_id else {
            return Ok(());
        };
        let message = DiscordMessage::silent(InteractionResponse::message(content));
        let result = if let Some(id) = state.message_id {
            self.discord
                .edit_message(channel, id, message)
                .await
                .map(|()| id)
        } else {
            self.discord
                .send_message_with_delivery_key(
                    channel,
                    &format!("dota-host-{}-{}", record.guild_id, record.pending_match_id),
                    message,
                )
                .await
                .map(|r| r.message_id)
        };
        match result {
            Ok(id) => {
                state.message_id = Some(id);
                state.last_message = content.to_owned();
                state.last_message_at = now;
                self.save(record, state, now).await?;
            }
            Err(_) => tracing::warn!(
                guild_id = record.guild_id,
                pending_match_id = record.pending_match_id,
                "could not update Dota lobby status in Discord"
            ),
        }
        Ok(())
    }

    async fn archive_once(&self, port: &dyn DotaHostPort, now: i64) -> Result<(), String> {
        let repo = DotaSessionRepository::new(&self.path);
        let records =
            blocking(move || repo.recent_sessions(1000).map_err(|e| e.to_string())).await?;
        for mut record in records
            .into_iter()
            .filter(|s| s.phase == Phase::Recorded && s.account_key == self.account_key())
        {
            let mut state: SessionState = serde_json::from_value(record.payload.clone())
                .map_err(|_| "invalid replay session payload")?;
            if state.archive.is_some()
                || matches!(
                    state.replay,
                    Some(ReplayMetadata::Expired | ReplayMetadata::NotRecorded)
                )
                || now.saturating_sub(state.replay_last_attempt) < 120
                || now.saturating_sub(record.created_at) > 172800
            {
                continue;
            }
            let Some(match_id) = record
                .valve_match_id
                .as_deref()
                .and_then(|id| id.parse().ok())
            else {
                continue;
            };
            state.replay_last_attempt = now;
            self.save(&mut record, &state, now).await?;
            if !matches!(state.replay, Some(ReplayMetadata::Available { .. })) {
                match port.match_details(match_id).await {
                    Ok(details) if details.match_id == match_id => {
                        state.replay = Some(details.replay)
                    }
                    _ => {
                        state.replay_error =
                            Some("Replay metadata unavailable; retry scheduled".to_owned());
                        self.save(&mut record, &state, now).await?;
                        continue;
                    }
                }
            }
            if let Some(ReplayMetadata::Available { cluster, salt }) = state.replay {
                let archive = ReplayArchive {
                    directory: self.config.replay_directory.clone(),
                    maximum_bytes: self.config.replay_max_bytes,
                    retention_days: self.config.replay_retention_days,
                };
                match archive.download(match_id, cluster, salt).await {
                    Ok(saved) => {
                        state.archive = Some(saved);
                        state.replay_error = None;
                    }
                    Err(reason) => state.replay_error = Some(reason),
                }
            }
            self.save(&mut record, &state, chrono::Utc::now().timestamp())
                .await?;
            // Bound each pass; hosting and the live feed run independently.
            break;
        }
        Ok(())
    }
}

#[async_trait]
impl BackgroundWorker for DotaHostWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        let port = tokio::select! {
            _=context.cancelled()=>return Ok(()),
            result=steam::connect(&self.config)=>result?,
        };
        let mut archive_context = context.clone();
        let host = async {
            loop {
                tokio::select! {
                    _=context.cancelled()=>return Ok::<(),String>(()),
                    result=self.tick(port.as_ref(),chrono::Utc::now().timestamp())=>result?,
                }
                if !context.sleep(Duration::from_secs(5)).await {
                    return Ok(());
                }
            }
        };
        let archive = async {
            loop {
                tokio::select! {
                    _=archive_context.cancelled()=>return Ok::<(),String>(()),
                    result=self.archive_once(port.as_ref(),chrono::Utc::now().timestamp())=>{
                        if let Err(reason)=result { tracing::warn!(%reason,"Dota replay archive pass failed; hosting continues"); }
                    },
                }
                if !archive_context.sleep(Duration::from_secs(60)).await {
                    return Ok(());
                }
            }
        };
        tokio::select! { result=host=>result, result=archive=>result }
    }
}

pub fn worker_spec(worker: DotaHostWorker) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new("dota_host", Arc::new(worker))
}

fn owns_lobby(
    record: &DotaSessionRecord,
    state: &SessionState,
    lobby: &HostLobby,
    bot: u32,
) -> bool {
    lobby.owner_account_id == bot
        && lobby.name == state.settings.name
        && record
            .lobby_id
            .as_ref()
            .is_none_or(|id| id == &lobby.id.to_string())
}

fn settings_match(expected: &LobbySettings, actual: &HostLobby) -> bool {
    expected.league_id == actual.league_id
        && expected.game_mode == actual.game_mode
        && expected.server_region == actual.server_region
        && expected.first_pick_radiant == actual.first_pick_radiant
        && expected.tv_delay == actual.tv_delay
        && !actual.cheats
        && !actual.fill_bots
        && actual.spectating
        && actual.visibility == expected.visibility
}

fn pending_matches_roster(pending: &PendingMatchRecord, roster: &[HostedMatchRosterEntry]) -> bool {
    let set = |ids: Vec<i64>| ids.into_iter().collect::<std::collections::BTreeSet<_>>();
    pending.state.radiant_team_ids.len() == 5
        && pending.state.dire_team_ids.len() == 5
        && set(pending.state.radiant_team_ids.clone())
            == set(roster
                .iter()
                .filter(|p| p.radiant)
                .map(|p| p.discord_id)
                .collect())
        && set(pending.state.dire_team_ids.clone())
            == set(roster
                .iter()
                .filter(|p| !p.radiant)
                .map(|p| p.discord_id)
                .collect())
}

fn result_matches_roster(
    actual: &[HostedMatchPlayer],
    expected: &[HostedMatchRosterEntry],
) -> bool {
    actual.len() == 10
        && expected.len() == 10
        && actual
            .iter()
            .map(|p| (p.account32, p.radiant))
            .collect::<std::collections::BTreeSet<_>>()
            == expected
                .iter()
                .map(|p| (p.steam_account_id, p.radiant))
                .collect::<std::collections::BTreeSet<_>>()
        && actual
            .iter()
            .map(|p| p.account32)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == 10
}

fn pending_channel(pending: &PendingMatchRecord) -> Option<u64> {
    pending
        .state
        .thread_shuffle_thread_id
        .or(pending.state.shuffle_channel_id)
        .or(pending.state.origin_channel_id)
        .and_then(|id| u64::try_from(id).ok())
        .filter(|id| *id > 0)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| "Dota persistence task failed".to_owned())?
}

/// Local operator commands only enqueue intent. The supervised host verifies
/// the actual GC lobby before acting. Status intentionally omits credentials,
/// the lobby password, and the serialized player mapping.
pub async fn operator_command(
    path: PathBuf,
    action: &str,
    guild: Option<i64>,
    pending: Option<i64>,
) -> Result<String, String> {
    let action = action.to_owned();
    blocking(move || {
        let repo=DotaSessionRepository::new(path);
        if action=="status" {
            let sessions=repo.recent_sessions(100).map_err(|e|e.to_string())?;
            return serde_json::to_string_pretty(&sessions.into_iter().map(|s|serde_json::json!({
                "guild_id":s.guild_id,"pending_match_id":s.pending_match_id,"phase":s.phase.as_str(),
                "lobby_id":s.lobby_id,"valve_match_id":s.valve_match_id,"server_id":s.payload.get("server_id"),"updated_at":s.updated_at,"error":s.last_error,
                "archive":s.payload.get("archive"),"replay_error":s.payload.get("replay_error"),
            })).collect::<Vec<_>>()).map_err(|e|e.to_string());
        }
        let (guild,pending)=(guild.ok_or("guild ID required")?,pending.ok_or("pending match ID required")?);
        let mut session=repo.session(guild,pending).map_err(|e|e.to_string())?.ok_or("Dota session not found")?;
        if !session.phase.is_active() { return Err("session is already terminal".to_owned()); }
        if action=="resume" && session.phase!=Phase::NeedsReview { return Err("only a session requiring review can be resumed".to_owned()); }
        let key=match action.as_str() {"resume"=>"resume_requested","cancel"=>"cancel_requested",_=>return Err("invalid host operation".to_owned())};
        session.payload.as_object_mut().ok_or("invalid session payload")?.insert(key.into(),true.into());
        repo.update(&session,session.revision,chrono::Utc::now().timestamp()).map_err(|e|e.to_string())?;
        Ok(format!("{action} requested for {guild}/{pending}; the host will reconcile Dota before acting"))
    }).await
}
