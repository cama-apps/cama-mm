//! Durable prelaunch settings changes, confirmed against the owned GC lobby.

use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LobbyConfigurationRequest {
    settings: LobbySettings,
    start_mode: StartMode,
    actor_id: u64,
    requested_at: i64,
    first_sent_at: Option<i64>,
    last_sent_at: Option<i64>,
    applied_at: Option<i64>,
}

pub async fn guild_configure_command(
    path: PathBuf,
    guild: i64,
    pending: Option<i64>,
    actor_id: u64,
    patch: DotaHostingOptions,
) -> Result<String, String> {
    if guild <= 0 || actor_id == 0 || pending.is_some_and(|id| id <= 0) {
        return Err("A server, operator and positive pending match ID are required.".into());
    }
    patch.validate()?;
    if patch.hosting.is_some() || patch == DotaHostingOptions::default() {
        return Err(
            "Choose at least one lobby setting; use `/admin dota manual` to change hosting.".into(),
        );
    }
    blocking(move || {
        let sessions = DotaSessionRepository::new(&path);
        let id = match pending {
            Some(id) => id,
            None => {
                let rows = sessions.active_sessions().map_err(|e| e.to_string())?;
                let ids = rows.iter().filter(|s| s.guild_id == guild).map(|s| s.pending_match_id).collect::<Vec<_>>();
                match ids.as_slice() {
                    [id] => *id,
                    [] => return Err("No bot-hosted lobby in this server.".into()),
                    _ => return Err("Several matches are active; specify pending_match.".into()),
                }
            }
        };
        let mut record = sessions.session(guild, id).map_err(|e| e.to_string())?
            .ok_or("No bot-hosted lobby for that pending match in this server.")?;
        let mut state: SessionState = serde_json::from_value(record.payload.clone()).map_err(|_| "Invalid saved hosting state.")?;
        if record.phase != Phase::Gathering || record.lobby_id.is_none()
            || record.valve_match_id.is_some() || state.launch_requested_at.is_some()
            || state.server_id.is_some() || state.cancel_requested || state.resolution.is_some()
            || state.recorded_match_id.is_some() || state.manual_start_requested
            || state.test_mode != DotaHostTestMode::Off
        {
            return Err("Settings can change only while a production bot lobby is gathering, before start or cancellation is requested.".into());
        }
        if state.configuration.is_some() {
            return Err("A settings change is already pending; check `/admin dota status`.".into());
        }
        let pending = PendingMatchRepository::new(&path).pending_match(guild, id)
            .map_err(|e| e.to_string())?.ok_or("Pending match no longer exists.")?;
        if !pending_matches_roster(&pending, &state.roster)
            || pending.state.extra.get("draft_setup_complete") == Some(&false.into())
            || pending.state.extra.get("shuffle_setup_complete") == Some(&false.into())
            || DotaHostingOptions::from_extra(&pending.state.extra)?.hosting == Some(HostingMode::Manual)
        {
            return Err("This match has incomplete or changed setup, or uses manual hosting.".into());
        }
        let mut settings = state.settings.clone();
        if let Some(region) = patch.region { settings.server_region = region; }
        if let Some(mode) = patch.game_mode { settings.game_mode = mode; }
        if let Some(delay) = patch.tv_delay { settings.tv_delay = delay; }
        if let Some(league) = patch.league_id { settings.league_id = league; }
        if let Some(visibility) = patch.visibility { settings.visibility = visibility as i32; }
        if let Some(pick) = patch.first_pick {
            settings.first_pick_radiant = match pick {
                FirstPick::Radiant => Some(true), FirstPick::Dire => Some(false), FirstPick::Random => None,
            };
        }
        let now = chrono::Utc::now().timestamp();
        state.configuration = Some(LobbyConfigurationRequest {
            settings, start_mode: patch.start.unwrap_or(state.start_mode), actor_id,
            requested_at: now, first_sent_at: None, last_sent_at: None, applied_at: None,
        });
        record.payload = serde_json::to_value(state).map_err(|e| e.to_string())?;
        // A worker that has already read the old revision must fail its launch
        // intent write before it can send launch to Steam (and vice versa).
        sessions.update(&record, record.revision, now).map_err(|e| e.to_string())?;
        Ok(format!("Lobby settings change queued for pending #{id}."))
    }).await
}

impl DotaHostWorker {
    pub(super) async fn configure_session(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        lobby: Option<&HostLobby>,
        now: i64,
    ) -> Result<(), String> {
        let mut request = state
            .configuration
            .clone()
            .ok_or("Missing settings request")?;
        let Some(lobby) = lobby else {
            return self
                .review(
                    record,
                    state,
                    "lobby disappeared while applying settings; reconcile before resuming",
                    now,
                )
                .await;
        };
        if !owns_lobby(record, state, lobby, self.config.account_id) {
            return self
                .review(
                    record,
                    state,
                    "settings change held: lobby ownership changed",
                    now,
                )
                .await;
        }
        let pending = self.pending(record).await?;
        if pending
            .as_ref()
            .is_none_or(|p| !pending_matches_roster(p, &state.roster))
        {
            return self
                .review(
                    record,
                    state,
                    "roster changed while applying lobby settings",
                    now,
                )
                .await;
        }
        if !port.betting_observation_fresh().await {
            self.suspend_betting(record).await?;
            return Err("waiting for fresh GC observation before changing lobby settings".into());
        }
        if lobby.stage != LobbyStage::Gathering
            || lobby.match_id.is_some()
            || lobby.server_id.is_some()
            || record.valve_match_id.is_some()
            || state.launch_requested_at.is_some()
            || state.server_id.is_some()
        {
            // An external launch can race a queued request (including a lost
            // configure reply). Reconcile known settings without sending any
            // mutation, then let normal match tracking/recording continue.
            self.suspend_betting(record).await?;
            let applied = settings_match(&request.settings, lobby);
            if !applied && !settings_match(&state.settings, lobby) {
                return self.review(record, state, "launched lobby matches neither saved nor requested settings; inspect before resuming", now).await;
            }
            if applied {
                state.settings = request.settings.clone();
                state.start_mode = request.start_mode;
                request.applied_at = Some(now);
            }
            state.configuration = None;
            state.last_configuration = Some(request);
            state.manual_start_requested = false;
            record.last_error = None;
            self.save(record, state, now).await?;
            return self.announce(record, state, if applied {
                "Dota launched with the requested settings. Match tracking continues."
            } else {
                "Settings change skipped: Dota launched with the previous settings. Match tracking continues."
            }, now).await;
        }
        if settings_match(&request.settings, lobby) {
            state.settings = request.settings.clone();
            state.start_mode = request.start_mode;
            state.manual_start_requested = false;
            request.applied_at = Some(now);
            state.last_configuration = Some(request);
            state.configuration = None;
            record.phase = Phase::Gathering;
            record.last_error = None;
            self.save(record, state, now).await?;
            return self
                .announce(
                    record,
                    state,
                    &format!(
                        "Lobby settings applied: {}",
                        settings_summary(&state.settings, state.start_mode)
                    ),
                    now,
                )
                .await;
        }
        if request
            .first_sent_at
            .is_some_and(|sent| now.saturating_sub(sent) > 90)
        {
            return self.review(record, state, "Dota has not confirmed the requested lobby settings; inspect and use `/admin dota resume` to retry", now).await;
        }
        if request
            .last_sent_at
            .is_some_and(|sent| now.saturating_sub(sent) < 15)
        {
            return Ok(());
        }
        // Never overwrite unrelated changes to league/safety settings.
        if !settings_match(&state.settings, lobby) {
            return self
                .review(
                    record,
                    state,
                    "observed lobby settings match neither the saved nor requested settings",
                    now,
                )
                .await;
        }
        self.suspend_betting(record).await?;
        request.first_sent_at.get_or_insert(now);
        request.last_sent_at = Some(now);
        state.configuration = Some(request.clone());
        self.save(record, state, now).await?;
        port.configure(lobby.id, &request.settings).await?;
        // Even a successful call is confirmed on the next authoritative read.
        Ok(())
    }
}

fn settings_summary(settings: &LobbySettings, start: StartMode) -> String {
    format!(
        "region {} · mode {} · first pick {} · {:?} start · TV delay {} · league {} · {}",
        settings.server_region,
        settings.game_mode,
        match settings.first_pick_radiant {
            Some(true) => "Radiant",
            Some(false) => "Dire",
            None => "random",
        },
        start,
        settings.tv_delay,
        settings.league_id,
        if settings.visibility == 0 {
            "public"
        } else {
            "unlisted"
        }
    )
}

pub(super) fn status(state: &SessionState) -> String {
    let current = settings_summary(&state.settings, state.start_mode);
    if let Some(request) = &state.configuration {
        format!(
            "Current: {current}\nSettings change pending: {}",
            settings_summary(&request.settings, request.start_mode)
        )
    } else if let Some(request) = &state.last_configuration {
        if let Some(applied_at) = request.applied_at {
            format!(
                "Current: {current}\nLast settings change confirmed <t:{}:R>.",
                applied_at
            )
        } else {
            format!(
                "Current: {current}\nLast settings change skipped because Dota had already launched."
            )
        }
    } else {
        format!("Current: {current}")
    }
}

pub(super) fn reset_retry(state: &mut SessionState) {
    if let Some(request) = &mut state.configuration {
        request.first_sent_at = None;
        request.last_sent_at = None;
    }
}
