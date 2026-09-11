//! Durable orchestration of the dedicated Steam account and Cama matches.
//!
//! Persist intent before remote mutations. A lost response is reconciled from
//! the GC cache; it never authorizes a second lobby or an inferred match win.

mod simulated;
mod steam;
mod test_mode;
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
    dota_hosting::{DotaHostingOptions, FirstPick, HostingMode, StartMode},
    dota_lobby::{self, ExpectedPlayer, LobbySeat, Side},
    guild_config::GuildConfigStore,
};
use serde::{Deserialize, Serialize};

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::dota_host_config::{DotaHostConfig, DotaHostTestMode};
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

// Preserve existing session payloads without retaining replay download or
// retention behavior. New sessions only record metadata returned with results.
#[derive(Clone, Serialize, Deserialize)]
struct ArchivedReplay {
    match_id: String,
    filename: String,
    bytes: u64,
    sha256: String,
    archived_at: i64,
}

#[derive(Clone, Debug)]
pub struct HostMatchDetails {
    pub match_id: u64,
    pub league_id: u32,
    pub winner: Option<String>,
    pub finished: bool,
    pub players: Vec<HostedMatchPlayer>,
    pub replay: ReplayMetadata,
    pub postgame_statistics: Option<serde_json::Value>,
}

#[async_trait]
pub trait DotaHostPort: Send + Sync {
    fn is_simulated(&self) -> bool {
        false
    }
    async fn prepare_simulation(&self, _roster: &[HostedMatchRosterEntry]) -> Result<(), String> {
        Err("this transport does not support simulated players".to_owned())
    }
    /// None is authoritative only after initial GC cache hydration. Connection
    /// loss and stale/incomplete cache must return an error.
    async fn snapshot(&self) -> Result<Option<HostLobby>, String>;
    /// True only when the source has recent GC observation, not just a readable cache.
    async fn betting_observation_fresh(&self) -> bool {
        true
    }
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
struct OperatorResolution {
    outcome: String,
    actor_id: u64,
    reason: String,
    expected_valve_match_id: Option<String>,
    requested_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionState {
    #[serde(default)]
    test_mode: DotaHostTestMode,
    /// Negative Discord IDs created by /admin addfake, with their frozen side.
    /// These are placeholders only and never receive Steam invitations.
    #[serde(default)]
    fake_roster: Vec<(i64, bool)>,
    #[serde(default)]
    simulated_winner: Option<String>,
    #[serde(default)]
    start_mode: StartMode,
    #[serde(default)]
    manual_start_requested: bool,
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
    pending_status: Option<String>,
    #[serde(default)]
    resolution: Option<OperatorResolution>,
    #[serde(default)]
    resolution_history: Vec<OperatorResolution>,
    #[serde(default)]
    betting_control_audit: Vec<serde_json::Value>,
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
    postgame_statistics: Option<serde_json::Value>,
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
        self.retry_terminal_status(now).await?;
        let Some(mut record) = self.discover(now).await? else {
            return Ok(());
        };
        let mut state: SessionState = serde_json::from_value(record.payload.clone())
            .map_err(|_| "invalid durable Dota session payload")?;
        if let Some(content) = state.pending_status.clone() {
            self.announce(&mut record, &mut state, &content, now)
                .await?;
        }
        if self.config.test_mode == DotaHostTestMode::Off
            && state.test_mode != DotaHostTestMode::Off
        {
            return self.review(&mut record, &mut state, "a historical test session is quarantined; production hosting cannot operate it", now).await;
        }
        if state.resolution.is_some() {
            return self
                .resolve_session(port, &mut record, &mut state, now)
                .await;
        }
        if !self.config.guild_ids.contains(&record.guild_id)
            && !state.cancel_requested
            && record.valve_match_id.is_none()
            && state.launch_requested_at.is_none()
            && !matches!(record.phase, Phase::Running | Phase::Finishing)
        {
            return self.review(&mut record, &mut state,
                "this server was removed from the hosting allowlist; prelaunch hosting is paused", now).await;
        }
        // Re-read manual overrides before any external action. A queued match
        // can be opted out while discovery is claiming its session.
        if let Some(pending) = self.pending(&record).await?
            && DotaHostingOptions::from_extra(&pending.state.extra)?.hosting
                == Some(HostingMode::Manual)
        {
            state.cancel_requested = true;
            state.resume_requested = false;
        }
        if state.test_mode != DotaHostTestMode::Off {
            return self
                .tick_test_session(port, &mut record, &mut state, now)
                .await;
        }
        if self.config.test_mode != DotaHostTestMode::Off || !state.fake_roster.is_empty() {
            return self
                .review(
                    &mut record,
                    &mut state,
                    "this production session cannot run in a hosting test mode",
                    now,
                )
                .await;
        }
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
                self.suspend_betting(&record).await?;
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
        if state.recorded_match_id.is_none()
            && pending.is_none()
            && let Some(id) = self.recorded_id(&record).await?
        {
            state.recorded_match_id = Some(id);
            self.save(&mut record, &state, now).await?;
        }
        if state.recorded_match_id.is_some() {
            if let Some(lobby) = lobby {
                if lobby.stage != LobbyStage::Postgame {
                    // Match details may reach us before the lobby's final
                    // update. Never destroy a server still reported active.
                    return Ok(());
                }
                if !port.betting_observation_fresh().await {
                    return Err("refreshing stale GC ownership before lobby cleanup".into());
                }
                port.destroy(lobby.id).await?;
                return Ok(());
            }
            record.phase = Phase::Recorded;
            state.pending_status = Some(format!(
                "Dota match {} recorded automatically as Cama match {}.",
                record.valve_match_id.as_deref().unwrap_or("unknown"),
                state.recorded_match_id.unwrap_or_default()
            ));
            self.save(&mut record, &state, now).await?;
            self.live
                .finish_match(record.guild_id, record.pending_match_id)
                .await;
            let content = format!(
                "Dota match {} recorded automatically as Cama match {}.",
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
                if !port.betting_observation_fresh().await {
                    return Err("refreshing stale GC ownership before lobby cleanup".into());
                }
                port.destroy(lobby.id).await?;
                return Ok(());
            }
            if record.lobby_id.is_none() && state.create_requested_at.is_some() {
                state.cancel_requested = false;
                return self.review(&mut record,&mut state,"lobby creation is still unconfirmed; reconcile the Steam account before releasing it",now).await;
            }
            self.release_betting_window(&record, now).await?;
            record.phase = Phase::Cancelled;
            state.pending_status = Some("Dota hosting stopped. The Cama match can still be handled with the existing record/abort commands.".to_owned());
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
                .is_some_and(|lobby| settings_match(&state.settings, lobby))
        {
            if pending.state.betting_closed() || self.recorded_id(&record).await?.is_some() {
                // Restore the observed start without undoing any explicit
                // admin extension stored alongside it.
                state.betting_closed = true;
                self.save(&mut record, &state, now).await?;
            } else {
                self.manage_betting_window(&mut record, &mut state, pending, now)
                    .await?;
                // An owned, matching GC snapshot is the only source of lease renewal.
                // Discovery and connection failures never imply observation.
                let gameplay_observed = lobby.as_ref().is_some_and(|lobby| {
                    lobby.stage == LobbyStage::Postgame
                        || lobby
                            .game_state
                            .is_some_and(dota_lobby::gameplay_has_started)
                        || lobby.match_id.is_some_and(|id| {
                            self.live
                                .gameplay_started(record.guild_id, record.pending_match_id, id)
                        })
                });
                if !gameplay_observed && port.betting_observation_fresh().await {
                    self.observe_betting(&record, now).await?;
                } else {
                    self.suspend_betting(&record).await?;
                }
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
                "Dota lobby ready: **{}**. Accept your invite and choose your assigned side. {}/10 correctly seated; {} missing, {} on the wrong side. {}",
                state.settings.name,
                10_usize.saturating_sub(admission.missing.len() + admission.wrong_side.len()),
                admission.missing.len(),
                admission.wrong_side.len(),
                if state.start_mode == StartMode::Manual {
                    "An admin must use `/admin dota start` once all ten are seated."
                } else {
                    "The match starts automatically after all ten are seated."
                }
            );
            return self.announce(&mut record, &mut state, &content, now).await;
        }
        if state.start_mode == StartMode::Manual && !state.manual_start_requested {
            record.phase = Phase::Gathering;
            self.save(&mut record, &state, now).await?;
            return self
                .announce(
                    &mut record,
                    &mut state,
                    "All ten players are on the correct sides. Waiting for `/admin dota start`.",
                    now,
                )
                .await;
        }
        let Some(_launch_guard) = self
            .recorder
            .try_acquire_launch_guard(record.guild_id, record.pending_match_id)
        else {
            return Ok(());
        };
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
            || self.pending(&record).await?.is_none_or(|p| {
                !pending_matches_roster(&p, &state.roster)
                    || p.state.extra.get("shuffle_setup_complete")
                        == Some(&serde_json::Value::Bool(false))
                    || p.state.extra.get("draft_setup_complete")
                        == Some(&serde_json::Value::Bool(false))
                    || DotaHostingOptions::from_extra(&p.state.extra)
                        .map_or(true, |options| options.hosting == Some(HostingMode::Manual))
            })
            || self.recorded_id(&record).await?.is_some()
        {
            state.launch_requested_at = None;
            record.phase = Phase::Gathering;
            self.save(&mut record, &state, now).await?;
            return Ok(());
        }
        // All fallible reads precede durable intent. Once intent exists, only
        // the external launch remains ambiguous after a crash or lost reply.
        record.phase = Phase::Launching;
        state.launch_requested_at = Some(now);
        self.save(&mut record, &state, now).await?;
        port.launch(lobby.id).await?;
        self.announce(&mut record,&mut state,"All ten players are on the correct sides. Dota server launch requested; betting remains open through the hero draft.",now).await
    }

    async fn suspend_betting(&self, record: &DotaSessionRecord) -> Result<(), String> {
        let repo = PendingMatchRepository::new(&self.path);
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        blocking(move || {
            repo.suspend_hosted_betting(guild, pending)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }

    async fn observe_betting(&self, record: &DotaSessionRecord, now: i64) -> Result<(), String> {
        let repo = PendingMatchRepository::new(&self.path);
        let (guild, pending) = (record.guild_id, record.pending_match_id);
        blocking(move || {
            repo.observe_hosted_betting(guild, pending, now)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
    }

    async fn retry_terminal_status(&self, now: i64) -> Result<(), String> {
        let repo = DotaSessionRepository::new(&self.path);
        let key = self.account_key();
        let records = blocking(move || {
            repo.terminal_status_pending(&key)
                .map_err(|e| e.to_string())
        })
        .await?;
        for mut record in records {
            let mut state: SessionState =
                serde_json::from_value(record.payload.clone()).map_err(|e| e.to_string())?;
            if state.channel_id.is_some()
                && let Some(content) = state.pending_status.clone()
            {
                self.announce(&mut record, &mut state, &content, now)
                    .await?;
            }
        }
        Ok(())
    }

    async fn resolve_session(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<(), String> {
        let resolution = state
            .resolution
            .clone()
            .ok_or("Missing operator resolution")?;
        if resolution.expected_valve_match_id != record.valve_match_id {
            return self
                .review(
                    record,
                    state,
                    "the Dota identity changed after the resolution request; no action taken",
                    now,
                )
                .await;
        }
        let lobby = port.snapshot().await?;
        if lobby.is_some() && !port.betting_observation_fresh().await {
            self.review(record, state, "resolution needs a fresh GC lobby observation; reconnecting before any destructive action", now).await?;
            // A new coordinator welcome provides fresh ownership evidence.
            // This refresh is only for an explicit destructive resolution;
            // ordinary quiet-lobby betting suspension never reconnects.
            return Err("refreshing stale GC ownership before operator resolution".into());
        }
        if let Some(lobby) = &lobby {
            if !owns_lobby(record, state, lobby, self.config.account_id)
                || lobby
                    .match_id
                    .is_some_and(|id| Some(id.to_string()) != record.valve_match_id)
            {
                return self
                    .review(
                        record,
                        state,
                        "resolution is blocked by a different lobby or Dota match; no action taken",
                        now,
                    )
                    .await;
            }
            let never_launched = record.valve_match_id.is_none()
                && state.launch_requested_at.is_none()
                && !matches!(record.phase, Phase::Running | Phase::Finishing);
            if lobby.stage != LobbyStage::Postgame
                && !(lobby.stage == LobbyStage::Gathering && never_launched)
            {
                // Explicit recovery may bypass stale lifecycle cache only with
                // independent, complete Valve evidence for this exact game.
                let finished = if let Some(id) = record
                    .valve_match_id
                    .as_deref()
                    .and_then(|id| id.parse().ok())
                {
                    port.match_details(id).await.is_ok_and(|details| {
                        details.finished
                            && details.match_id == id
                            && details.league_id == state.settings.league_id
                            && result_matches_roster(&details.players, &state.roster)
                    })
                } else {
                    false
                };
                if !finished {
                    return self.review(record, state, "resolution is waiting for Dota to finish; an active server will not be destroyed", now).await;
                }
            }
        }
        let recorded = self.recorded_id(record).await?;
        let pending = self.pending(record).await?;
        match resolution.outcome.as_str() {
            "recorded" => {
                let Some(id) = recorded else {
                    return self
                        .review(
                            record,
                            state,
                            "record the verified winner with /record before resolving as recorded",
                            now,
                        )
                        .await;
                };
                if pending.is_some() {
                    return self.review(record, state, "recording settlement is incomplete; retry /record with the committed winner first", now).await;
                }
                let repo = MatchRepository::new(&self.path);
                let guild = record.guild_id;
                let committed =
                    blocking(move || repo.get_match(id, Some(guild)).map_err(|e| e.to_string()))
                        .await?
                        .ok_or("Committed Cama result disappeared")?;
                let radiant = state
                    .roster
                    .iter()
                    .filter(|p| p.radiant)
                    .map(|p| p.discord_id)
                    .collect::<std::collections::BTreeSet<_>>();
                let dire = state
                    .roster
                    .iter()
                    .filter(|p| !p.radiant)
                    .map(|p| p.discord_id)
                    .collect::<std::collections::BTreeSet<_>>();
                if committed
                    .valve_match_id
                    .is_some_and(|id| Some(id.to_string()) != record.valve_match_id)
                    || committed
                        .team1_players
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>()
                        != radiant
                    || committed
                        .team2_players
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>()
                        != dire
                {
                    return self.review(record, state, "the committed Cama result has a different Dota identity or roster; resolve that conflict first", now).await;
                }
                state.recorded_match_id = Some(id);
            }
            "void" => {
                if recorded.is_some() {
                    return self.review(record, state, "a committed result cannot be voided by hosting recovery; use the match correction workflow", now).await;
                }
                let path = self.path.clone();
                let (guild, pending_id) = (record.guild_id, record.pending_match_id);
                let participants = state
                    .roster
                    .iter()
                    .map(|p| p.discord_id)
                    .collect::<Vec<_>>();
                blocking(move || {
                    cama_db::betting_service_repository::BettingServiceRepository::new(path)
                        .abort_pending_match_atomic(Some(guild), pending_id, &participants)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .await?;
            }
            _ => return Err("Invalid durable resolution outcome".into()),
        }
        if let Some(lobby) = lobby {
            // Confirm removal in the next snapshot before releasing the account.
            port.destroy(lobby.id).await?;
            return Ok(());
        }
        record.phase = if resolution.outcome == "recorded" {
            Phase::Recorded
        } else {
            Phase::Cancelled
        };
        record.last_error = None;
        state.cancel_requested = false;
        state.resume_requested = false;
        state.pending_status = Some(format!(
            "Hosting resolved as {} for pending #{} (Dota {}). Operator <@{}>: {}",
            resolution.outcome,
            record.pending_match_id,
            record.valve_match_id.as_deref().unwrap_or("unassigned"),
            resolution.actor_id,
            resolution.reason
        ));
        self.save(record, state, now).await?;
        self.live
            .finish_match(record.guild_id, record.pending_match_id)
            .await;
        let content = state
            .pending_status
            .clone()
            .expect("saved resolution status");
        self.announce(record, state, &content, now).await
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
        let snapshot = port.snapshot().await?;
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
        let gc_started = snapshot.is_some_and(|lobby| {
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
        // The shared match thread includes competitors. Live scoreboard and
        // economy details belong exclusively in the audited spectator channel.
        Ok("Betting is closed.".to_owned())
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
                    postgame_statistics: None,
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
        if let Some(statistics) = details.postgame_statistics {
            state.postgame_statistics = Some(statistics);
        }
        record.phase = Phase::Finishing;
        self.save(record, state, now).await?;
        let request = HostedMatchResult {
            guild_id: record.guild_id,
            pending_match_id: record.pending_match_id,
            valve_match_id: match_id,
            winning_team: winner,
            expected_roster: state.roster.clone(),
            players: details.players,
            postgame_statistics: state.postgame_statistics.clone(),
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
        let active = blocking(move || {
            let sessions = DotaSessionRepository::new(&path);
            let mut active = sessions.active_sessions().map_err(|e| e.to_string())?.into_iter().find(|s| s.account_key == key);
            if active.is_some() { return Ok(active); }
            let pending_repo = PendingMatchRepository::new(&path);
            let links = OpenDotaPlayerRepository::new(&path);
            let guilds = GuildConfigRepository::new(&path, false);
            let mut pending = Vec::new();
            for guild in &config.guild_ids { pending.extend(pending_repo.pending_matches(*guild).map_err(|e| e.to_string())?); }
            pending.sort_by_key(|p| (p.state.shuffle_timestamp.unwrap_or(0), p.pending_match_id));
            for pending in pending {
                if pending.state.extra.get("shuffle_setup_complete") == Some(&serde_json::Value::Bool(false))
                    || pending.state.extra.get("draft_setup_complete") == Some(&serde_json::Value::Bool(false))
                    || pending.state.extra.get("dota_host_account_key").and_then(serde_json::Value::as_str) != Some(key.as_str())
                    || sessions.session(pending.guild_id, pending.pending_match_id).map_err(|e| e.to_string())?.is_some() { continue; }
                let options = if pending.state.extra.contains_key("dota_hosting") {
                    DotaHostingOptions::from_extra(&pending.state.extra)?
                } else {
                    guilds.dota_hosting_options(pending.guild_id).map_err(|e| e.to_string())?
                };
                options.validate()?;
                if options.hosting == Some(HostingMode::Manual) { continue; }
                let Some(league) = options.league_id.or(guilds.get_league_id(pending.guild_id).map_err(|e| e.to_string())?
                    .and_then(|id| u32::try_from(id).ok()).filter(|id| *id > 0)) else {
                    tracing::warn!(guild_id=pending.guild_id,"Dota hosting is waiting for /enrich setleague");
                    continue;
                };
                let mut roster = Vec::new();
                let mut fake_roster = Vec::new();
                for (radiant, players) in [(true,&pending.state.radiant_team_ids),(false,&pending.state.dire_team_ids)] {
                    for discord_id in players {
                        if *discord_id < 0 && config.test_mode != DotaHostTestMode::Off {
                            if cama_db::core_repositories::PlayerRepository::new(&path)
                                .get_by_id(*discord_id, Some(pending.guild_id)).map_err(|e|e.to_string())?.is_some() {
                                fake_roster.push((*discord_id, radiant));
                            }
                            continue;
                        }
                        if let Some(account) = links.get_steam_id(*discord_id).map_err(|e| e.to_string())?.and_then(dota_lobby::account_id) {
                            roster.push(HostedMatchRosterEntry { discord_id:*discord_id, steam_account_id:account,radiant });
                        }
                    }
                }
                let state = SessionState {
                    test_mode: config.test_mode, fake_roster, simulated_winner: None,
                    start_mode: options.start.unwrap_or_default(), manual_start_requested: false,
                    settings: LobbySettings { name:if config.test_mode == DotaHostTestMode::Off {format!("Cama {}:{}",pending.guild_id,pending.pending_match_id)} else {format!("Cama TEST {}:{}",pending.guild_id,pending.pending_match_id)},
                        password:String::new(), visibility:if config.test_mode == DotaHostTestMode::Off {options.visibility.map(|value| value as i32).unwrap_or(PUBLIC_LOBBY_VISIBILITY)} else {2}, league_id:league,
                        game_mode:options.game_mode.unwrap_or(config.game_mode), server_region:options.region.unwrap_or(config.server_region),
                        first_pick_radiant:match options.first_pick { Some(FirstPick::Radiant) => Some(true), Some(FirstPick::Dire) => Some(false), _ => pending.state.first_pick_team.as_deref().and_then(|s| match s.to_ascii_lowercase().as_str() { "radiant" => Some(true),"dire"=>Some(false),_=>None }) },
                        tv_delay:options.tv_delay.unwrap_or(config.tv_delay) },
                    roster, channel_id:pending_channel(&pending),message_id:None,last_message:String::new(),last_message_at:0,pending_status:None,resolution:None,resolution_history:Vec::new(),betting_control_audit:Vec::new(),
                    last_invite_at:0,create_requested_at:None,launch_requested_at:None,betting_closed:false,betting_window_announced:false,betting_notification_sent:false,last_betting_notification_at:0,cancel_requested:false,
                    resume_requested:false,recorded_match_id:None,server_id:None,replay:None,postgame_statistics:None,archive:None,replay_last_attempt:0,replay_error:None,last_result_poll:0,lobby_deadline:0,recording_failures:0,
                };
                let valid_roster = if config.test_mode == DotaHostTestMode::Off {
                    dota_lobby::validate_roster(&state.expected(),config.account_id)
                } else {
                    test_mode::validate_test_roster(&state, config.account_id)
                        .and_then(|()| if test_mode::pending_matches_test_roster(&pending, &state) { Ok(()) } else { Err("test hosting requires links for every real player and registered fake placeholders") })
                };
                if let Err(reason) = valid_roster {
                    tracing::warn!(guild_id=pending.guild_id,pending_match_id=pending.pending_match_id,%reason,"Dota hosting waiting for valid primary account links");
                    continue;
                }
                if active.is_some() { break; }
                let payload = serde_json::to_value(&state).map_err(|e| e.to_string())?;
                active = Some(match sessions.claim_reserved_session(pending.guild_id,pending.pending_match_id,&key,payload,now) {
                        Err(cama_db::dota_session_repository::DotaSessionRepositoryError::ReservationUnavailable {..}) => continue,
                        Err(error) => return Err(error.to_string()),
                        Ok(claim) => match claim {
                        DotaSessionClaim::Created(s)|DotaSessionClaim::Existing(s)|DotaSessionClaim::Busy(s) => s,
                    }});
            }
            Ok(active)
        }).await?;
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
        self.suspend_betting(record).await?;
        record.phase = Phase::NeedsReview;
        record.last_error = Some(reason.to_owned());
        self.save(record, state, now).await?;
        self.announce(record,state,&format!("Hosting paused: {reason}. Use `/admin dota status` to inspect the saved identities and `/admin dota resolve` for terminal recovery."),now).await
    }

    async fn announce(
        &self,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        content: &str,
        now: i64,
    ) -> Result<(), String> {
        if state.last_message == content {
            if state.pending_status.take().is_some() {
                self.save(record, state, now).await?;
            }
            return Ok(());
        }
        if state.pending_status.as_deref() != Some(content) {
            state.pending_status = Some(content.to_owned());
            self.save(record, state, now).await?;
        }
        if record.phase.is_active()
            && record.phase != Phase::NeedsReview
            && now.saturating_sub(state.last_message_at) < 15
        {
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
                    &status_delivery_key(record.guild_id, record.pending_match_id),
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
                state.pending_status = None;
                self.save(record, state, now).await?;
            }
            Err(_) => {
                // Recreate an externally deleted status message only after a
                // successful read proves absence. Permission/network errors
                // remain durable retries against the same destination.
                if let Some(id) = state.message_id
                    && matches!(self.discord.fetch_message(channel, id).await, Ok(None))
                {
                    state.message_id = None;
                    self.save(record, state, now).await?;
                }
                tracing::warn!(
                    guild_id = record.guild_id,
                    pending_match_id = record.pending_match_id,
                    "could not update Dota lobby status in Discord"
                );
            }
        }
        Ok(())
    }

    async fn run_connected(
        &self,
        port: &dyn DotaHostPort,
        mut context: WorkerContext,
    ) -> Result<(), String> {
        loop {
            tokio::select! {
                _ = context.cancelled() => return Ok(()),
                result = self.tick(port, chrono::Utc::now().timestamp()) => result?,
            }
            if !context.sleep(Duration::from_secs(5)).await {
                return Ok(());
            }
        }
    }
}

#[async_trait]
impl BackgroundWorker for DotaHostWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        tracing::info!(test_mode = ?self.config.test_mode, guild_ids = ?self.config.guild_ids, "starting Dota hosting worker");
        let port = if self.config.test_mode == DotaHostTestMode::Simulated {
            simulated::connect(&self.config)
        } else {
            tokio::select! {
                _=context.cancelled()=>return Ok(()),
                result=steam::connect(&self.config)=>result?,
            }
        };
        self.run_connected(port.as_ref(), context).await
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

fn status_delivery_key(guild: i64, pending: i64) -> String {
    use sha2::{Digest, Sha256};
    // Discord caps string nonces at 25 characters. Preserve guild/session
    // identity and retry stability without exposing an overlong raw key.
    let digest = Sha256::digest(format!("dota-host-{guild}-{pending}"));
    let mut key = String::from("h");
    for byte in &digest[..12] {
        use std::fmt::Write;
        write!(key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    key
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
                "test_mode":s.payload.get("test_mode"), "start_mode":s.payload.get("start_mode"),
                "archive":s.payload.get("archive"),"replay_error":s.payload.get("replay_error"),
            })).collect::<Vec<_>>()).map_err(|e|e.to_string());
        }
        let (guild,pending)=(guild.ok_or("guild ID required")?,pending.ok_or("pending match ID required")?);
        let mut session=repo.session(guild,pending).map_err(|e|e.to_string())?.ok_or("Dota session not found")?;
        if !session.phase.is_active() { return Err("session is already terminal".to_owned()); }
        if action=="resume" && session.phase!=Phase::NeedsReview { return Err("only a session requiring review can be resumed".to_owned()); }
        let key=match action.as_str() {"resume"=>"resume_requested","cancel"=>"cancel_requested",_=>return Err("invalid host operation".to_owned())};
        let payload = session.payload.as_object_mut().ok_or("invalid session payload")?;
        payload.insert(key.into(),true.into());
        payload.insert(if action == "cancel" {"resume_requested"} else {"cancel_requested"}.into(), false.into());
        if action == "cancel" { payload.insert("manual_start_requested".into(), false.into()); }
        repo.update(&session,session.revision,chrono::Utc::now().timestamp()).map_err(|e|e.to_string())?;
        Ok(format!("{action} requested for {guild}/{pending}; the host will reconcile Dota before acting"))
    }).await
}

/// Guild-scoped Discord controls. These persist intent; only the supervised
/// worker can touch Steam, after checking ownership and the frozen roster.
pub async fn guild_operator_command(
    path: PathBuf,
    action: &str,
    guild: i64,
    pending: Option<i64>,
) -> Result<String, String> {
    if guild <= 0 || pending.is_some_and(|id| id <= 0) {
        return Err("A server and positive match ID are required.".into());
    }
    let action = action.to_owned();
    blocking(move || {
        let sessions = DotaSessionRepository::new(&path);
        if action == "status" {
            let rows = sessions.recent_sessions_for_guild(guild, 100).map_err(|e| e.to_string())?;
            let active_ids = rows.iter().filter(|s|s.phase.is_active()).map(|s|s.pending_match_id).collect::<std::collections::BTreeSet<_>>();
            let mut lines: Vec<_> = rows.into_iter()
                .filter(|s| s.guild_id == guild && pending.is_none_or(|id| s.pending_match_id == id))
                .take(3)
                .map(|s| {
                    let state: SessionState = serde_json::from_value(s.payload).map_err(|_| "Invalid saved hosting state.")?;
                    let pending = PendingMatchRepository::new(&path).pending_match(guild, s.pending_match_id).map_err(|e|e.to_string())?;
                    let betting = if pending.as_ref().is_some_and(|p| p.state.extra.get("dota_betting_suspended") == Some(&serde_json::Value::Bool(true))) { "suspended by operator" }
                        else if pending.as_ref().is_some_and(|p| p.state.betting_open(chrono::Utc::now().timestamp())) { "open" }
                        else { "closed" };
                    Ok(format!("Pending #{}: {} · {:?} · region {} · mode {} · {:?} start\nLobby {} · Dota {} · Cama {} · updated <t:{}:R> · betting {}\n{}", s.pending_match_id, s.phase.as_str(), state.test_mode, state.settings.server_region, state.settings.game_mode, state.start_mode,
                        s.lobby_id.as_deref().unwrap_or("unassigned"), s.valve_match_id.as_deref().unwrap_or("unassigned"), state.recorded_match_id.map_or("unrecorded".into(), |id| id.to_string()), s.updated_at,
                        betting,
                        s.last_error.map_or_else(|| "No saved error.".to_owned(), |e| format!("Attention: {}", e.chars().take(300).collect::<String>()))))
                }).collect::<Result<_, String>>()?;
            for p in PendingMatchRepository::new(&path).pending_matches(guild).map_err(|e|e.to_string())?.into_iter()
                .filter(|p|pending.is_none_or(|id|id==p.pending_match_id) && !active_ids.contains(&p.pending_match_id)).take(5) {
                let mode = DotaHostingOptions::from_extra(&p.state.extra)?;
                if mode.hosting == Some(HostingMode::Manual) || !p.state.extra.contains_key("dota_host_account_key") {
                    let reason = p.state.extra.get("dota_hosting_fallback_reason").and_then(serde_json::Value::as_str).unwrap_or("manual");
                    lines.push(format!("Pending #{}: manual hosting ({reason}); timed betting {}, record with /record.",p.pending_match_id,
                        if p.state.betting_open(chrono::Utc::now().timestamp()) {"open"} else {"closed"}));
                }
            }
            return Ok(if lines.is_empty() {"No Dota hosting sessions in this server.".into()} else {lines.join("\n").chars().take(1950).collect()});
        }
        let pending = match pending {
            Some(id) => id,
            None => {
                let ids: Vec<_> = if action == "manual" {
                    PendingMatchRepository::new(&path).pending_matches(guild).map_err(|e| e.to_string())?.into_iter().map(|p| p.pending_match_id).collect()
                } else {
                    sessions.active_sessions().map_err(|e| e.to_string())?.into_iter().filter(|s| s.guild_id == guild).map(|s| s.pending_match_id).collect()
                };
                match ids.as_slice() {
                    [id] => *id,
                    [] => return Err("No eligible match in this server.".into()),
                    _ => return Err("Several matches are active; specify pending_match.".into()),
                }
            }
        };
        if action == "manual" {
            PendingMatchRepository::new(&path).request_manual_dota_hosting(guild, pending).map_err(|e| e.to_string())?;
            return Ok(format!("Match #{pending} switched to manual hosting. Any owned pregame bot lobby will be cleaned up first. Create the Dota lobby yourself and use `/record` afterward."));
        }
        let mut session = sessions.session(guild, pending).map_err(|e| e.to_string())?.ok_or("No hosted session for that match in this server.")?;
        if !session.phase.is_active() { return Err("That hosting session has already finished.".into()); }
        let mut state: SessionState = serde_json::from_value(session.payload.clone()).map_err(|_| "Invalid saved hosting state.")?;
        match action.as_str() {
            "start" => {
                if state.test_mode == DotaHostTestMode::RealLobby { return Err("Real-lobby preview never launches. Use simulated mode for launch tests.".into()); }
                if session.phase != Phase::Gathering || state.cancel_requested || state.launch_requested_at.is_some() || session.valve_match_id.is_some() { return Err("Start is available only while an uncancelled lobby is gathering players.".into()); }
                state.manual_start_requested = true;
            }
            "cancel" => {
                state.cancel_requested = true;
                state.resume_requested = false;
                state.manual_start_requested = false;
            }
            "resume" => {
                if session.phase != Phase::NeedsReview { return Err("Only a session needing review can be resumed.".into()); }
                state.resume_requested = true;
                state.cancel_requested = false;
                if let Some(withdrawn) = state.resolution.take() {
                    state.resolution_history.push(withdrawn);
                }
            }
            _ => return Err("Unknown Dota hosting action.".into()),
        }
        session.payload = serde_json::to_value(state).map_err(|e| e.to_string())?;
        sessions.update(&session, session.revision, chrono::Utc::now().timestamp()).map_err(|e| e.to_string())?;
        Ok(format!("{action} requested for match #{pending}. The host will check lobby ownership, settings, and player readiness before acting."))
    }).await
}

/// Persist an audited, identity-bound resolution. The worker independently
/// verifies the external lobby and economic outcome before releasing its lease.
pub async fn guild_resolution_command(
    path: PathBuf,
    guild: i64,
    pending: i64,
    actor_id: u64,
    outcome: String,
    expected_valve_match_id: u64,
    reason: String,
) -> Result<String, String> {
    if guild <= 0
        || pending <= 0
        || actor_id == 0
        || !matches!(outcome.as_str(), "recorded" | "void")
    {
        return Err("Valid server, pending match, operator and resolution are required.".into());
    }
    let reason = reason.trim().to_owned();
    if reason.is_empty() || reason.chars().count() > 300 {
        return Err("Give an audit reason between 1 and 300 characters.".into());
    }
    blocking(move || {
        let repo = DotaSessionRepository::new(path);
        let mut session = repo.session(guild, pending).map_err(|e| e.to_string())?
            .ok_or("No hosting session for that pending match in this server.")?;
        if !session.phase.is_active() { return Err("That session is already resolved.".into()); }
        let expected = (expected_valve_match_id != 0).then(|| expected_valve_match_id.to_string());
        if expected != session.valve_match_id { return Err("Dota match ID does not match the saved session. Inspect /admin dota status first.".into()); }
        let mut state: SessionState = serde_json::from_value(session.payload.clone()).map_err(|e| e.to_string())?;
        if let Some(existing) = &state.resolution {
            if existing.outcome == outcome && existing.expected_valve_match_id == expected {
                return Ok("That resolution is already queued; inspect /admin dota status for any remaining blocker.".into());
            }
            return Err("A different resolution is already queued; resume the session to withdraw that request before replacing it.".into());
        }
        state.resolution = Some(OperatorResolution { outcome: outcome.clone(), actor_id, reason,
            expected_valve_match_id: expected, requested_at: chrono::Utc::now().timestamp() });
        state.manual_start_requested = false;
        session.payload = serde_json::to_value(state).map_err(|e| e.to_string())?;
        repo.update(&session, session.revision, chrono::Utc::now().timestamp()).map_err(|e| e.to_string())?;
        Ok(format!("Audited {outcome} resolution queued for pending #{pending}. The worker will verify Dota has ended, reconcile funds, and confirm the owned lobby is gone before releasing the account."))
    }).await
}
