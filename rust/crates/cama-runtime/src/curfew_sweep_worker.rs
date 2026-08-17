//! Production worker that removes lobby members whose curfew window has
//! started, DMs them, then refreshes the lobby's Discord embed so the
//! removal is actually visible (matching the Python original's DM +
//! `_sync_lobby_displays` pair in `commands/lobby.py::_deliver_curfew_kick`).
//! The Python original ran this sweep every minute (`tasks.loop(minutes=1)`);
//! the Rust port runs it every 15 seconds instead — a sweep is just an
//! in-memory lobby scan plus one indexed SQLite lookup per populated lobby,
//! cheap enough that there's no reason to make a kicked player wait up to a
//! minute to find out.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_app::curfew_service::CurfewService;
use cama_app::embeds::LobbyKind;
use chrono::Utc;
use tracing::{debug, info};

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::lobby_provider::LiveLobbyService;
use crate::registration::InteractionResponse;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const CURFEW_SWEEP_WORKER_NAME: &str = "curfew_sweep";
pub const CURFEW_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(15);

/// Re-render a lobby's live Discord embed after curfew removed members from
/// it, and strip a removed player's own sword reaction so the message's
/// reactions stop implying they're still queued. Implemented by
/// [`crate::lobby_provider::LobbyRegistrationProvider`].
#[async_trait]
pub trait CurfewLobbyDisplayPort: Send + Sync {
    async fn refresh_curfew_lobby(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), String>;

    async fn remove_curfew_lobby_reaction(
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
        for kick in &kicks {
            let Ok(user_id) = u64::try_from(kick.discord_id) else {
                continue;
            };
            let message = DiscordMessage::silent(InteractionResponse::message(format!(
                "🔒 You've been removed from {} — your \"{}\" curfew window started. Use `/player curfew remove` if you'd rather queue through it.",
                kick.lobby_kind.label(),
                kick.window_name,
            )));
            if let Err(error) = self.discord.send_direct_message(user_id, message).await {
                debug!(
                    discord_id = kick.discord_id,
                    %error,
                    "failed to DM user about curfew kick"
                );
            }
            if let Err(error) = self
                .display
                .remove_curfew_lobby_reaction(kick.guild_id, kick.lobby_kind, kick.discord_id)
                .await
            {
                debug!(
                    discord_id = kick.discord_id,
                    guild_id = kick.guild_id,
                    lobby_kind = ?kick.lobby_kind,
                    %error,
                    "failed to remove sword reaction after curfew kick"
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
