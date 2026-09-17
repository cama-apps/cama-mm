//! Production worker that removes lobby members whose curfew window has
//! started, DMs them, then refreshes the lobby's Discord embed so the
//! removal is actually visible (matching the Python original's DM +
//! `_sync_lobby_displays` pair in `commands/lobby.py::_deliver_curfew_kick`).
//! The same wake also commits any strict-mode edit or delete whose staged
//! effective time has arrived. The 60-second wake interval is arbitrary; it
//! could be tightened to 15 seconds or some other value later if there's a
//! reason to.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_app::curfew_service::{CurfewKick, CurfewService};
use cama_app::embeds::LobbyKind;
use chrono::Utc;
use tracing::{debug, info};

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::lobby_provider::LiveLobbyService;
use crate::registration::InteractionResponse;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const CURFEW_SWEEP_WORKER_NAME: &str = "curfew_sweep";
pub const CURFEW_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(60);

/// Re-render a lobby's live Discord embed after curfew removed members from
/// it, and make each removal look like an ordinary leave — the thread's
/// usual "left" line — without any public mention of curfew. Implemented by
/// [`crate::lobby_provider::LobbyRegistrationProvider`].
#[async_trait]
pub trait CurfewLobbyDisplayPort: Send + Sync {
    async fn refresh_curfew_lobby(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), String>;

    async fn publish_curfew_leave(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
        discord_id: i64,
    ) -> Result<(), String>;
}

pub struct CurfewSweepWorker {
    curfew: CurfewService,
    lobby: Arc<LiveLobbyService>,
    discord: Arc<dyn DiscordTransport>,
    display: Arc<dyn CurfewLobbyDisplayPort>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    wake_interval: Duration,
}

impl CurfewSweepWorker {
    #[must_use]
    pub fn new(
        curfew: CurfewService,
        lobby: Arc<LiveLobbyService>,
        discord: Arc<dyn DiscordTransport>,
        display: Arc<dyn CurfewLobbyDisplayPort>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
    ) -> Self {
        Self {
            curfew,
            lobby,
            discord,
            display,
            guild_source,
            wake_interval: CURFEW_SWEEP_WAKE_INTERVAL,
        }
    }

    async fn sweep_once(&self) -> Result<usize, String> {
        self.apply_due_pending_changes_once().await;

        let guild_ids = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if guild_ids.is_empty() {
            return Ok(0);
        }
        let curfew = self.curfew.clone();
        let lobby = Arc::clone(&self.lobby);
        let now = Utc::now();
        let kicks = tokio::task::spawn_blocking(move || curfew.sweep(&lobby, &guild_ids, now))
            .await
            .map_err(|error| format!("curfew sweep blocking task failed: {error}"))?;
        for (discord_id, content) in curfew_kick_notices(&kicks) {
            let Ok(user_id) = u64::try_from(discord_id) else {
                continue;
            };
            let message = DiscordMessage::silent(InteractionResponse::message(content));
            if let Err(error) = self.discord.send_direct_message(user_id, message).await {
                debug!(discord_id, %error, "failed to DM user about curfew kick");
            }
        }
        for kick in &kicks {
            if let Err(error) = self
                .display
                .publish_curfew_leave(kick.guild_id, kick.lobby_kind, kick.discord_id)
                .await
            {
                debug!(
                    discord_id = kick.discord_id,
                    guild_id = kick.guild_id,
                    lobby_kind = ?kick.lobby_kind,
                    %error,
                    "failed to publish leave after curfew kick"
                );
            }
        }
        let affected_lobbies: BTreeSet<(i64, LobbyKind)> = kicks
            .iter()
            .map(|kick| (kick.guild_id, kick.lobby_kind))
            .collect();
        for (guild_id, lobby_kind) in affected_lobbies {
            if let Err(error) = self
                .display
                .refresh_curfew_lobby(guild_id, lobby_kind)
                .await
            {
                debug!(
                    guild_id,
                    ?lobby_kind,
                    %error,
                    "failed to refresh lobby display after curfew sweep"
                );
            }
        }
        Ok(kicks.len())
    }

    /// Commit any staged strict-mode edit/delete whose effective time
    /// has arrived, and let the player know. Best-effort: a DM failure here
    /// doesn't block the change from having been committed.
    async fn apply_due_pending_changes_once(&self) {
        let curfew = self.curfew.clone();
        let now = Utc::now();
        let applied = match tokio::task::spawn_blocking(move || {
            curfew.apply_due_pending_changes(now)
        })
        .await
        {
            Ok(Ok(applied)) => applied,
            Ok(Err(error)) => {
                debug!(%error, "failed to apply due curfew pending changes");
                return;
            }
            Err(error) => {
                debug!(%error, "curfew pending-change blocking task failed");
                return;
            }
        };
        for change in applied {
            let Ok(user_id) = u64::try_from(change.discord_id) else {
                continue;
            };
            let content = match &change.window {
                Some(window) => format!(
                    "🔒 Your \"{}\" curfew window's scheduled change is now in effect.",
                    window.name
                ),
                None => format!(
                    "🔒 Your \"{}\" curfew window has now been removed, as scheduled.",
                    change.name
                ),
            };
            let message = DiscordMessage::silent(InteractionResponse::message(content));
            if let Err(error) = self.discord.send_direct_message(user_id, message).await {
                debug!(
                    discord_id = change.discord_id,
                    %error,
                    "failed to DM user about an applied curfew pending change"
                );
            }
        }
    }
}

/// One DM per player per sweep, with no "remove the window" hint: strict
/// windows can't be lifted the same night anyway. A player sitting in both lobbies when their
/// window starts is kicked from each, but two near-identical DMs read as a
/// bug, so the lobbies are listed together instead. Grouped by window name
/// as well as player so the (unusual) case of different windows firing in
/// different guilds still names the right one.
fn curfew_kick_notices(kicks: &[CurfewKick]) -> Vec<(i64, String)> {
    let mut grouped: BTreeMap<(i64, &str), Vec<&'static str>> = BTreeMap::new();
    for kick in kicks {
        let labels = grouped
            .entry((kick.discord_id, kick.window_name.as_str()))
            .or_default();
        let label = kick.lobby_kind.label();
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    grouped
        .into_iter()
        .map(|((discord_id, window_name), labels)| {
            let lobbies = match labels.as_slice() {
                [only] => (*only).to_owned(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                [] => unreachable!("a grouped kick names at least one lobby"),
            };
            (
                discord_id,
                format!(
                    "🔒 Your \"{window_name}\" curfew window started, so you've been removed from {lobbies}."
                ),
            )
        })
        .collect()
}

/// Build the production worker specification retained by [`crate::Runtime`].
#[must_use]
pub fn curfew_sweep_worker_spec(
    curfew: CurfewService,
    lobby: Arc<LiveLobbyService>,
    discord: Arc<dyn DiscordTransport>,
    display: Arc<dyn CurfewLobbyDisplayPort>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        CURFEW_SWEEP_WORKER_NAME,
        Arc::new(CurfewSweepWorker::new(
            curfew,
            lobby,
            discord,
            display,
            guild_source,
        )),
    )
}

#[async_trait]
impl BackgroundWorker for CurfewSweepWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }

            let kicked = match self.sweep_once().await {
                Ok(kicked) => kicked,
                Err(_) if context.shutdown_requested() => return Ok(()),
                Err(error) => return Err(error),
            };
            if kicked > 0 {
                info!(kicked, "removed lobby members whose curfew window started");
            }

            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
#[path = "curfew_sweep_worker/tests.rs"]
mod tests;
