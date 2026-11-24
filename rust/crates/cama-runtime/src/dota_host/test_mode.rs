//! Explicit hosting previews, isolated from production launch and settlement.

use super::*;

impl DotaHostWorker {
    pub(super) async fn tick_test_session(
        &self,
        port: &dyn DotaHostPort,
        record: &mut DotaSessionRecord,
        state: &mut SessionState,
        now: i64,
    ) -> Result<(), String> {
        let simulated = state.test_mode == DotaHostTestMode::Simulated;
        // A persisted mode owns its session even when deployment settings change.
        // Never route a simulated session through Steam or resume a preview as
        // a production game. A real preview can still be cancelled with mode off.
        if port.is_simulated() != simulated
            || (state.test_mode != self.config.test_mode && !state.cancel_requested)
        {
            return self.review(record, state, "the saved hosting test mode differs from the current worker; restore its mode or cancel using the matching transport", now).await;
        }
        // Retire persisted previews that used placeholder seats. The cancellation
        // path checks ownership and protects externally launched real lobbies.
        if dota_lobby::validate_roster(&state.expected(), self.config.account_id).is_err() {
            state.cancel_requested = true;
        }
        if record.phase == Phase::NeedsReview && !state.cancel_requested {
            if !state.resume_requested {
                return Ok(());
            }
            state.resume_requested = false;
            state.create_requested_at = None;
            record.last_error = None;
            record.phase = Phase::Creating;
        }
        let roster = state.roster.clone();
        if simulated && !state.cancel_requested {
            port.prepare_simulation(&roster).await?;
        }
        let lobby = port.snapshot().await?;
        if let Some(lobby) = &lobby
            && !owns_lobby(record, state, lobby, self.config.account_id)
        {
            return self
                .review(
                    record,
                    state,
                    "another lobby is active; the test left it untouched",
                    now,
                )
                .await;
        }
        if state.cancel_requested {
            if let Some(lobby) = lobby {
                if !simulated && lobby.stage != LobbyStage::Gathering {
                    return self.review(record, state, "this preview was launched outside Cama; no game was destroyed or recorded", now).await;
                }
                port.destroy(lobby.id).await?;
                return Ok(());
            }
            record.phase = Phase::Cancelled;
            record.last_error = None;
            self.save(record, state, now).await?;
            let result = if simulated {
                state
                    .simulated_winner
                    .as_deref()
                    .map(|winner| format!("SIMULATED result: {winner} wins. "))
                    .unwrap_or_else(|| "SIMULATED test cancelled. ".to_owned())
            } else {
                "Real lobby preview finished. ".to_owned()
            };
            return self.announce(record, state, &format!("{result}No match result, ratings, or bets were settled. Use `/record` with Abort to clear the Cama pending match if it remains."), now).await;
        }
        let Some(pending) = self.pending(record).await? else {
            state.cancel_requested = true;
            return self.save(record, state, now).await;
        };
        if !pending_matches_roster(&pending, &roster) {
            return self
                .review(
                    record,
                    state,
                    "the test roster changed after it was frozen",
                    now,
                )
                .await;
        }
        if now
            > record
                .created_at
                .saturating_add(self.config.lobby_timeout_seconds as i64)
        {
            state.cancel_requested = true;
            return self.save(record, state, now).await;
        }
        let Some(lobby) = lobby else {
            if !simulated && let Some(sent) = state.create_requested_at {
                if now.saturating_sub(sent) > 30 {
                    return self.review(record, state, "the test lobby creation was not confirmed; inspect Dota before resuming", now).await;
                }
                return Ok(());
            }
            port.prepare_lobby_creation().await?;
            record.phase = Phase::Creating;
            state.create_requested_at = Some(now);
            // The in-memory simulation can be reconstructed after a restart.
            if simulated {
                record.lobby_id = None;
                state.launch_requested_at = None;
            }
            self.save(record, state, now).await?;
            port.create(&state.settings).await?;
            return Ok(());
        };
        if record.lobby_id.is_none() {
            record.lobby_id = Some(lobby.id.to_string());
            self.save(record, state, now).await?;
        }
        if !settings_match(&state.settings, &lobby) {
            return self
                .review(
                    record,
                    state,
                    "the test lobby settings differ from the saved configuration",
                    now,
                )
                .await;
        }
        if lobby.stage != LobbyStage::Gathering {
            if !simulated {
                return self.review(record, state, "this preview was launched outside Cama; automatic result recording is disabled", now).await;
            }
            record.phase = Phase::Running;
            self.save(record, state, now).await?;
            if lobby.stage == LobbyStage::Postgame {
                let details = port
                    .match_details(lobby.match_id.ok_or("simulation omitted match ID")?)
                    .await?;
                if !details.finished || !result_matches_roster(&details.players, &roster) {
                    return self
                        .review(
                            record,
                            state,
                            "simulated result did not match the test roster",
                            now,
                        )
                        .await;
                }
                let winner = details
                    .winner
                    .as_deref()
                    .filter(|w| matches!(*w, "radiant" | "dire"))
                    .ok_or("simulation omitted winner")?;
                state.simulated_winner = Some(winner.to_owned());
                state.cancel_requested = true;
                self.save(record, state, now).await?;
                return self.announce(record, state, &format!("SIMULATED Dota result: {winner} wins. No real match, ratings, or bets were recorded; cleaning up the simulated lobby."), now).await;
            }
            let stage = match lobby.game_state {
                Some(2 | 3) => "hero draft",
                Some(4) => "pregame",
                Some(5) => "gameplay",
                _ => "server allocation",
            };
            return self
                .announce(
                    record,
                    state,
                    &format!(
                        "SIMULATED Dota hosting: {stage}. No Steam connection or real settlement."
                    ),
                    now,
                )
                .await;
        }
        if lobby
            .members
            .iter()
            .any(|p| p.account_id == self.config.account_id && p.side.is_some())
        {
            port.move_host_to_pool(lobby.id).await?;
        }
        let wrong_side: Vec<_> = roster
            .iter()
            .filter(|player| {
                let expected = if player.radiant {
                    Side::Radiant
                } else {
                    Side::Dire
                };
                lobby.members.iter().any(|member| {
                    member.account_id == player.steam_account_id
                        && member.side.is_some_and(|side| side != expected)
                })
            })
            .collect();
        if !simulated {
            for player in &wrong_side {
                port.kick_from_team(lobby.id, player.steam_account_id)
                    .await?;
            }
        }
        let missing: Vec<_> = roster
            .iter()
            .filter(|p| {
                !lobby
                    .members
                    .iter()
                    .any(|member| member.account_id == p.steam_account_id)
            })
            .collect();
        if now.saturating_sub(state.last_invite_at) >= 60 {
            state.last_invite_at = now;
            self.save(record, state, now).await?;
            for player in &missing {
                port.invite(lobby.id, player.steam_account_id).await?;
            }
        }
        record.phase = Phase::Gathering;
        self.save(record, state, now).await?;
        if simulated {
            let all_seated = roster.iter().all(|p| {
                lobby.members.iter().any(|member| {
                    member.account_id == p.steam_account_id
                        && member.side == Some(if p.radiant { Side::Radiant } else { Side::Dire })
                        && member.slot < 5
                })
            });
            if all_seated
                && (state.start_mode == StartMode::Automatic || state.manual_start_requested)
                && !lobby
                    .members
                    .iter()
                    .any(|p| p.account_id == self.config.account_id && p.side.is_some())
            {
                state.launch_requested_at = Some(now);
                record.phase = Phase::Launching;
                self.save(record, state, now).await?;
                port.launch(lobby.id).await?;
            }
            if all_seated && state.start_mode == StartMode::Manual && !state.manual_start_requested
            {
                return self.announce(record, state, "SIMULATED Dota lobby: all players seated. Waiting for `/admin dota start`.", now).await;
            }
            return self.announce(record, state, "SIMULATED Dota lobby: seating the frozen test roster and exercising simulated launch. No real match will be started.", now).await;
        }
        self.announce(record, state, &format!("TEST LOBBY: **{}**. {}/{} players are in the lobby. Choose your assigned shuffle side; {} wrong-side player(s) returned to the player pool. Automatic launch and result recording are disabled. Use `/record` with Abort to clear the pending match and remove this test lobby.", state.settings.name, roster.len() - missing.len(), roster.len(), wrong_side.len()), now).await
    }
}
