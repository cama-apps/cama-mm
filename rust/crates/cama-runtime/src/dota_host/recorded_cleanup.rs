use super::*;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct LobbyCleanup {
    refresh_attempts: u8,
    destroy_attempts: u8,
    first_destroy_at: Option<i64>,
    last_destroy_at: Option<i64>,
}

// Callers independently require a fresh, owned, matching GC snapshot.
// Historical launch/server IDs must not veto a confirmed return to idle.
pub(super) fn can_release_lobby(
    record: &DotaSessionRecord,
    state: &SessionState,
    lobby: &HostLobby,
) -> bool {
    if lobby.stage == LobbyStage::Postgame {
        return true;
    }
    if !failed_launch::returned_to_lobby(lobby) || !settings_match(&state.settings, lobby) {
        return false;
    }
    let allocated = record.valve_match_id.is_some()
        || state.allocation_observed_at.is_some()
        || state.server_id.is_some();
    let unlaunched = state.launch_requested_at.is_none()
        && !matches!(record.phase, Phase::Running | Phase::Finishing);
    allocated || unlaunched || state.lobby_cleanup.destroy_attempts > 0
}

impl DotaHostWorker {
    pub(super) async fn fresh_cleanup_observation(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<bool, String> {
        if port.betting_observation_fresh().await {
            return Ok(true);
        }
        if state.lobby_cleanup.refresh_attempts >= 3 {
            self.review(record, state,
                "Dota lobby cleanup paused after three GC refresh attempts; the host remains reserved until a fresh lobby observation is available", now).await?;
            return Ok(false);
        }
        state.lobby_cleanup.refresh_attempts += 1;
        let message = format!(
            "Dota lobby cleanup needs a fresh GC observation; reconnecting (attempt {}/3). The host is not yet released",
            state.lobby_cleanup.refresh_attempts
        );
        self.review(record, state, &message, now).await?;
        // Persist the budget before the supervisor reconnects. Cache reads
        // alone must never turn stale ownership evidence into fresh evidence.
        Err(
            "refreshing stale GC ownership before operator resolution or recorded lobby cleanup"
                .into(),
        )
    }

    pub(super) async fn destroy_recorded_lobby(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        lobby_id: u64,
        now: i64,
    ) -> Result<(), String> {
        if state.lobby_cleanup.destroy_attempts >= 3
            || state
                .lobby_cleanup
                .first_destroy_at
                .is_some_and(|first| now.saturating_sub(first) >= 90)
        {
            return self.review(record, state,
                "Cama result recorded; Dota lobby removal remains unconfirmed and automatic cleanup retries are exhausted. The host is still reserved", now).await;
        }
        if state
            .lobby_cleanup
            .last_destroy_at
            .is_some_and(|last| now.saturating_sub(last) < 10)
        {
            return Ok(());
        }
        state.lobby_cleanup.destroy_attempts += 1;
        state.lobby_cleanup.first_destroy_at.get_or_insert(now);
        state.lobby_cleanup.last_destroy_at = Some(now);
        record.phase = Phase::Finishing;
        record.last_error = Some(format!(
            "Cama result recorded; removing the old Dota lobby (attempt {}/3), awaiting confirmation before releasing the host",
            state.lobby_cleanup.destroy_attempts
        ));
        self.save(record, state, now).await?;
        port.destroy(lobby_id).await
    }
}
