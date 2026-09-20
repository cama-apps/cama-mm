use super::*;

const MAX_REPLACEMENT_LOBBIES: usize = 3;
const MAX_DESTROY_ATTEMPTS: u8 = 3;
const CLEANUP_TIMEOUT_SECONDS: i64 = 90;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LobbyRecreation {
    pub lobby_id: u64,
    pub match_id: Option<u64>,
    requested_at: i64,
    destroy_attempts: u8,
    last_destroy_at: Option<i64>,
}

pub(super) fn returned_to_lobby(lobby: &HostLobby) -> bool {
    lobby.stage == LobbyStage::Gathering
        && lobby.server_id.is_none()
        && lobby.winner.is_none()
        && matches!(lobby.game_state, None | Some(0 | 1 | 10))
}

impl DotaHostWorker {
    pub(super) async fn begin_failed_launch_recovery(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        lobby: Option<&HostLobby>,
        now: i64,
    ) -> Result<bool, String> {
        let Some(lobby) = lobby else { return Ok(false) };
        // A cached prelaunch lobby after an ambiguous launch is not proof of
        // failure. First require durable evidence that allocation/loading ran.
        if state.cancel_requested
            || !returned_to_lobby(lobby)
            || !(record.phase == Phase::Running || state.allocation_observed_at.is_some())
        {
            return Ok(false);
        }
        self.suspend_betting(record).await?;
        if !port.betting_observation_fresh().await {
            return Ok(true);
        }
        let pending = self.pending(record).await?;
        let gameplay_started = state.betting_closed
            || pending.as_ref().is_some_and(|p| p.state.betting_closed())
            || record
                .valve_match_id
                .as_deref()
                .and_then(|id| id.parse().ok())
                .is_some_and(|id| {
                    self.live
                        .gameplay_started(record.guild_id, record.pending_match_id, id)
                });
        if gameplay_started || !settings_match(&state.settings, lobby) {
            self.review(record, state,
                "Dota returned to the lobby, but a safe pregame reset could not be confirmed; automatic recreation is paused", now).await?;
            return Ok(true);
        }
        if pending
            .as_ref()
            .is_none_or(|p| !pending_matches_roster(p, &state.roster))
            || self.recorded_id(record).await?.is_some()
        {
            self.review(
                record,
                state,
                "the pending match changed before failed-launch recovery",
                now,
            )
            .await?;
            return Ok(true);
        }
        if state.failed_launches.len() >= MAX_REPLACEMENT_LOBBIES {
            self.review(record, state,
                "Dota launch failed again after three replacement lobbies; automatic retries are exhausted", now).await?;
            return Ok(true);
        }
        let attempt = LobbyRecreation {
            lobby_id: lobby.id,
            match_id: record
                .valve_match_id
                .as_deref()
                .and_then(|id| id.parse().ok()),
            requested_at: now,
            destroy_attempts: 0,
            last_destroy_at: None,
        };
        // Consume the budget before any remote mutation, including across
        // crashes and lost destroy/create replies. Never erase old identities.
        state.failed_launches.push(attempt.clone());
        state.lobby_recreation = Some(attempt);
        record.phase = Phase::Creating;
        record.last_error = None;
        self.save(record, state, now).await?;
        self.live
            .finish_match(record.guild_id, record.pending_match_id)
            .await;
        tracing::warn!(guild_id = record.guild_id, pending_match_id = record.pending_match_id,
            lobby_id = lobby.id, match_id = ?record.valve_match_id,
            attempt = state.failed_launches.len(), "Dota returned to lobby after failed launch; recreating lobby");
        self.announce(record, state, &format!(
            "Dota returned to the lobby before gameplay. Creating a fresh lobby (retry {}/3); teams and wagers are preserved. Accept the new invitation when it arrives.",
            state.failed_launches.len()), now).await?;
        Ok(true)
    }

    pub(super) async fn recreate_failed_lobby(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<(), String> {
        let Some(_guard) = self
            .recorder
            .try_acquire_launch_guard(record.guild_id, record.pending_match_id)
        else {
            return Ok(());
        };
        self.suspend_betting(record).await?;
        let Some(pending) = self.pending(record).await? else {
            return self.review(record, state, "the pending match was finalized during lobby recovery; automatic recreation is paused", now).await;
        };
        if self.recorded_id(record).await?.is_some()
            || !pending_matches_roster(&pending, &state.roster)
            || state.betting_closed
            || pending.state.betting_closed()
        {
            return self
                .review(
                    record,
                    state,
                    "the match changed during lobby recovery; automatic recreation is paused",
                    now,
                )
                .await;
        }
        let attempt = state
            .lobby_recreation
            .as_ref()
            .ok_or("missing lobby recovery intent")?
            .clone();
        // Re-read under the recording/launch guard. An unavailable cache is
        // never evidence that cleanup succeeded or permission to create again.
        let lobby = port.snapshot().await?;
        if let Some(lobby) = lobby {
            if now.saturating_sub(attempt.requested_at) >= CLEANUP_TIMEOUT_SECONDS {
                return self
                    .review(
                        record,
                        state,
                        "failed-launch lobby cleanup timed out; automatic recreation is paused",
                        now,
                    )
                    .await;
            }
            if !owns_lobby(record, state, &lobby, self.config.account_id)
                || lobby.id != attempt.lobby_id
                || !settings_match(&state.settings, &lobby)
                || !returned_to_lobby(&lobby)
                || lobby
                    .match_id
                    .is_some_and(|id| Some(id) != attempt.match_id)
            {
                return self.review(record, state, "the lobby changed during failed-launch cleanup; no destructive action was taken", now).await;
            }
            if !port.betting_observation_fresh().await
                || attempt.destroy_attempts >= MAX_DESTROY_ATTEMPTS
                || attempt
                    .last_destroy_at
                    .is_some_and(|last| now.saturating_sub(last) < 10)
            {
                return Ok(());
            }
            let intent = state.lobby_recreation.as_mut().unwrap();
            intent.destroy_attempts += 1;
            intent.last_destroy_at = Some(now);
            self.save(record, state, now).await?;
            port.destroy(lobby.id).await?;
            return Ok(());
        }
        record.lobby_id = None;
        record.valve_match_id = None;
        record.phase = Phase::Creating;
        record.last_error = None;
        state.server_id = None;
        state.create_requested_at = None;
        state.launch_requested_at = None;
        state.allocation_observed_at = None;
        state.last_allocation_log_at = None;
        state.last_result_poll = 0;
        state.last_invite_at = 0;
        state.replay = None;
        state.postgame_statistics = None;
        state.manual_start_requested = false;
        state.lobby_deadline = now.saturating_add(self.config.lobby_timeout_seconds as i64);
        state.lobby_recreation = None;
        self.save(record, state, now).await?;
        // The next normal tick honors manual/cancel requests and the existing
        // persist-before-create rule. It invites the same frozen roster.
        Ok(())
    }

    pub(super) async fn defer_match_details(
        &self,
        record: &mut DotaSessionRecord,
        state: &SessionState,
        match_id: u64,
        reason: &str,
        now: i64,
    ) -> Result<(), String> {
        // GC result 2 while players load is not a broken Steam connection.
        // finish() already verified the cache is readable; real disconnects
        // still propagate from snapshot() and restart the transport.
        if record.last_error.as_deref() != Some(reason) {
            tracing::info!(
                guild_id = record.guild_id,
                pending_match_id = record.pending_match_id,
                match_id,
                error = reason,
                "Dota match details unavailable; retaining connection and retrying later"
            );
        }
        record.last_error = Some(reason.to_owned());
        self.save(record, state, now).await
    }
}
