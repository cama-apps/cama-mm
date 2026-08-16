//! Production worker that removes lobby members whose curfew window has
//! started, then DMs them. Runs every minute, matching the Python
//! `tasks.loop(minutes=1)` sweep it replaces.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_app::curfew_service::CurfewService;
use chrono::Utc;
use tracing::{debug, info};

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::lobby_provider::LiveLobbyService;
use crate::registration::InteractionResponse;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const CURFEW_SWEEP_WORKER_NAME: &str = "curfew_sweep";
pub const CURFEW_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(60);

pub struct CurfewSweepWorker {
    curfew: CurfewService,
    lobby: Arc<LiveLobbyService>,
    discord: Arc<dyn DiscordTransport>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    wake_interval: Duration,
}

impl CurfewSweepWorker {
    #[must_use]
    pub fn new(
        curfew: CurfewService,
        lobby: Arc<LiveLobbyService>,
        discord: Arc<dyn DiscordTransport>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
    ) -> Self {
        Self {
            curfew,
            lobby,
            discord,
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
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        CURFEW_SWEEP_WORKER_NAME,
        Arc::new(CurfewSweepWorker::new(curfew, lobby, discord, guild_source)),
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
