//! In-memory Dota transport for explicitly selected local simulation sessions.
//! No Steam connection, account lookup, observer, or replay request is made.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use tokio::{sync::Mutex, time::Instant};

pub(super) fn connect(config: &DotaHostConfig) -> Arc<dyn DotaHostPort> {
    Arc::new(SimulatedHost::new(config.account_id))
}

struct SimulatedHost {
    owner: u32,
    state: Mutex<Model>,
}

#[derive(Default)]
struct Model {
    roster: Vec<HostedMatchRosterEntry>,
    lobby: Option<HostLobby>,
    invitations: BTreeMap<u32, Instant>,
    launched_at: Option<Instant>,
    results: BTreeMap<u64, HostMatchDetails>,
}

impl SimulatedHost {
    fn new(owner: u32) -> Self {
        Self {
            owner,
            state: Mutex::new(Model::default()),
        }
    }
}

fn roster_signature(roster: &[HostedMatchRosterEntry]) -> BTreeSet<(i64, u32, bool)> {
    roster
        .iter()
        .map(|player| (player.discord_id, player.steam_account_id, player.radiant))
        .collect()
}

fn validate_simulated_roster(roster: &[HostedMatchRosterEntry], owner: u32) -> Result<(), String> {
    if roster.len() != 10
        || roster.iter().filter(|player| player.radiant).count() != 5
        || roster
            .iter()
            .map(|player| player.discord_id)
            .collect::<BTreeSet<_>>()
            .len()
            != 10
        || roster
            .iter()
            .map(|player| player.steam_account_id)
            .collect::<BTreeSet<_>>()
            .len()
            != 10
        || roster.iter().any(|player| {
            player.discord_id == 0
                || matches!(player.steam_account_id, 0 | u32::MAX)
                || player.steam_account_id == owner
        })
    {
        return Err(
            "SIMULATED: expected ten distinct players, five on each side, excluding the host"
                .into(),
        );
    }
    Ok(())
}

// Stable across process restarts. These deliberately high synthetic IDs stay
// inside explicitly marked simulated sessions; they are never sent to Steam.
fn synthetic_lobby_id(name: &str) -> u64 {
    let hash = name.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    8_000_000_000_000_000_000 + (hash % 1_000_000_000_000) * 2
}

impl Model {
    fn lobby_mut(&mut self, lobby_id: u64) -> Result<&mut HostLobby, String> {
        self.lobby
            .as_mut()
            .filter(|lobby| lobby.id == lobby_id)
            .ok_or_else(|| "SIMULATED: lobby does not exist or ID differs".into())
    }

    fn advance(&mut self, now: Instant) {
        let Some(lobby) = self.lobby.as_mut() else {
            return;
        };
        if lobby.stage == LobbyStage::Gathering {
            let mut slots = [0_u32; 2];
            for player in &self.roster {
                let side = usize::from(!player.radiant);
                if self
                    .invitations
                    .get(&player.steam_account_id)
                    .is_some_and(|sent| now.duration_since(*sent) >= Duration::from_secs(2))
                    && let Some(seat) = lobby
                        .members
                        .iter_mut()
                        .find(|seat| seat.account_id == player.steam_account_id)
                {
                    seat.side = Some(if player.radiant {
                        Side::Radiant
                    } else {
                        Side::Dire
                    });
                    seat.slot = slots[side];
                }
                slots[side] += 1;
            }
            return;
        }
        let Some(started) = self.launched_at else {
            return;
        };
        let elapsed = now.duration_since(started).as_secs();
        if elapsed < 5 {
            return;
        }
        let match_id = lobby.id + 1;
        lobby.match_id = Some(match_id);
        lobby.stage = LobbyStage::Running;
        lobby.game_state = Some(match elapsed {
            0..=24 => 2,  // Hero draft; the betting window should still be open.
            25..=44 => 4, // Playable pregame.
            45..=64 => 5,
            _ => 6,
        });
        if elapsed >= 65 {
            lobby.stage = LobbyStage::Postgame;
            lobby.winner = Some("radiant".into());
            self.results.insert(match_id, self.details(match_id, true));
        }
    }

    fn details(&self, match_id: u64, finished: bool) -> HostMatchDetails {
        HostMatchDetails {
            match_id,
            league_id: self.lobby.as_ref().map_or(0, |lobby| lobby.league_id),
            winner: finished.then(|| "radiant".into()),
            finished,
            players: self
                .roster
                .iter()
                .map(|player| HostedMatchPlayer {
                    account32: player.steam_account_id,
                    radiant: player.radiant,
                })
                .collect(),
            replay: ReplayMetadata::NotRecorded,
            postgame_statistics: None,
        }
    }
}

#[async_trait]
impl DotaHostPort for SimulatedHost {
    fn is_simulated(&self) -> bool {
        true
    }

    async fn prepare_simulation(&self, roster: &[HostedMatchRosterEntry]) -> Result<(), String> {
        validate_simulated_roster(roster, self.owner)?;
        let mut state = self.state.lock().await;
        if state.lobby.is_some() && roster_signature(&state.roster) != roster_signature(roster) {
            return Err("SIMULATED: frozen roster cannot change while its lobby exists".into());
        }
        if state.lobby.is_none() {
            state.roster = roster.to_vec();
        }
        Ok(())
    }

    async fn snapshot(&self) -> Result<Option<HostLobby>, String> {
        let mut state = self.state.lock().await;
        state.advance(Instant::now());
        Ok(state.lobby.clone())
    }

    async fn create(&self, settings: &LobbySettings) -> Result<(), String> {
        let mut state = self.state.lock().await;
        validate_simulated_roster(&state.roster, self.owner)?;
        if state.lobby.is_some() {
            return Err("SIMULATED: a lobby already exists".into());
        }
        state.invitations.clear();
        state.launched_at = None;
        let lobby_id = synthetic_lobby_id(&settings.name);
        state.results.remove(&(lobby_id + 1));
        state.lobby = Some(HostLobby {
            id: lobby_id,
            name: settings.name.clone(),
            owner_account_id: self.owner,
            league_id: settings.league_id,
            game_mode: settings.game_mode,
            server_region: settings.server_region,
            first_pick_radiant: settings.first_pick_radiant,
            cheats: false,
            fill_bots: false,
            spectating: true,
            tv_delay: settings.tv_delay,
            visibility: settings.visibility,
            stage: LobbyStage::Gathering,
            game_state: None,
            members: vec![LobbySeat {
                account_id: self.owner,
                side: Some(Side::Radiant),
                slot: 0,
            }],
            match_id: None,
            server_id: None,
            winner: None,
        });
        Ok(())
    }

    async fn invite(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if !state
            .roster
            .iter()
            .any(|player| player.steam_account_id == account_id)
        {
            return Err("SIMULATED: invite is outside the frozen roster".into());
        }
        let lobby = state.lobby_mut(lobby_id)?;
        if lobby.stage != LobbyStage::Gathering {
            return Err("SIMULATED: invitations are closed after launch".into());
        }
        if !lobby
            .members
            .iter()
            .any(|seat| seat.account_id == account_id)
        {
            lobby.members.push(LobbySeat {
                account_id,
                side: None,
                slot: 0,
            });
        }
        state
            .invitations
            .entry(account_id)
            .or_insert_with(Instant::now);
        Ok(())
    }

    async fn move_host_to_pool(&self, lobby_id: u64) -> Result<(), String> {
        self.kick_from_team(lobby_id, self.owner).await
    }

    async fn kick_from_team(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let lobby = state.lobby_mut(lobby_id)?;
        if lobby.stage != LobbyStage::Gathering {
            return Err("SIMULATED: teams are frozen after launch".into());
        }
        if let Some(seat) = lobby
            .members
            .iter_mut()
            .find(|seat| seat.account_id == account_id)
        {
            seat.side = None;
            seat.slot = 0;
        }
        if let Some(invitation) = state.invitations.get_mut(&account_id) {
            *invitation = Instant::now();
        }
        Ok(())
    }

    async fn kick(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        if account_id == self.owner {
            return Err("SIMULATED: cannot kick the host".into());
        }
        let mut state = self.state.lock().await;
        let lobby = state.lobby_mut(lobby_id)?;
        if lobby.stage != LobbyStage::Gathering {
            return Err("SIMULATED: roster is frozen after launch".into());
        }
        lobby.members.retain(|seat| seat.account_id != account_id);
        state.invitations.remove(&account_id);
        Ok(())
    }

    async fn launch(&self, lobby_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.advance(Instant::now());
        let expected = &state.roster;
        let lobby = state
            .lobby
            .as_ref()
            .filter(|lobby| lobby.id == lobby_id)
            .ok_or("SIMULATED: launch lobby does not exist")?;
        if lobby.stage != LobbyStage::Gathering {
            return Err("SIMULATED: launch was already requested".into());
        }
        // Negative /addfake Discord IDs are allowed here only. The production
        // admission helper intentionally rejects those, so validate simulator
        // seats directly against the already frozen ten-player roster.
        let seats = lobby
            .members
            .iter()
            .filter(|seat| seat.side.is_some())
            .collect::<Vec<_>>();
        if seats.len() != 10
            || expected.iter().any(|player| {
                seats
                    .iter()
                    .filter(|seat| {
                        seat.account_id == player.steam_account_id
                            && seat.side
                                == Some(if player.radiant {
                                    Side::Radiant
                                } else {
                                    Side::Dire
                                })
                            && seat.slot < 5
                    })
                    .count()
                    != 1
            })
            || seats
                .iter()
                .map(|seat| (seat.side == Some(Side::Radiant), seat.slot))
                .collect::<BTreeSet<_>>()
                .len()
                != 10
        {
            return Err(
                "SIMULATED: all ten players must be correctly seated and the host in the pool"
                    .into(),
            );
        }
        state.lobby_mut(lobby_id)?.stage = LobbyStage::Allocating;
        state.launched_at = Some(Instant::now());
        Ok(())
    }

    async fn destroy(&self, lobby_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.lobby_mut(lobby_id)?;
        // Cancelling a simulation only discards in-memory state, including
        // during draft/gameplay; it cannot interrupt an actual Dota server.
        state.lobby = None;
        state.invitations.clear();
        state.launched_at = None;
        Ok(())
    }

    async fn match_details(&self, match_id: u64) -> Result<HostMatchDetails, String> {
        let mut state = self.state.lock().await;
        state.advance(Instant::now());
        if let Some(result) = state.results.get(&match_id) {
            return Ok(result.clone());
        }
        if state
            .lobby
            .as_ref()
            .is_some_and(|lobby| lobby.match_id == Some(match_id))
        {
            return Ok(state.details(match_id, false));
        }
        Err("SIMULATED: match result is unavailable".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<HostedMatchRosterEntry> {
        (0..10)
            .map(|index| HostedMatchRosterEntry {
                discord_id: if index == 0 { 12345 } else { -i64::from(index) },
                steam_account_id: 1000 + index,
                radiant: index < 5,
            })
            .collect()
    }

    fn settings() -> LobbySettings {
        LobbySettings {
            name: "SIMULATED Cama 1:2".into(),
            password: String::new(),
            visibility: 0,
            league_id: 23456,
            game_mode: 2,
            server_region: 1,
            first_pick_radiant: Some(false),
            tv_delay: 120,
        }
    }

    async fn gather(host: &SimulatedHost) -> u64 {
        let players = roster();
        host.prepare_simulation(&players).await.unwrap();
        host.create(&settings()).await.unwrap();
        let lobby_id = host.snapshot().await.unwrap().unwrap().id;
        host.move_host_to_pool(lobby_id).await.unwrap();
        for player in players {
            host.invite(lobby_id, player.steam_account_id)
                .await
                .unwrap();
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        lobby_id
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_exercises_seating_draft_pregame_and_explicit_result() {
        let host = SimulatedHost::new(99);
        assert!(host.is_simulated());
        assert!(host.snapshot().await.unwrap().is_none());
        let lobby_id = gather(&host).await;
        let gathered = host.snapshot().await.unwrap().unwrap();
        assert_eq!(
            gathered
                .members
                .iter()
                .filter(|seat| seat.side.is_some())
                .count(),
            10
        );
        assert_eq!(gathered.first_pick_radiant, Some(false));
        assert!(!gathered.cheats);
        assert!(!gathered.fill_bots);
        assert!(gathered.server_id.is_none());
        host.launch(lobby_id).await.unwrap();
        assert_eq!(
            host.snapshot().await.unwrap().unwrap().stage,
            LobbyStage::Allocating
        );

        tokio::time::advance(Duration::from_secs(5)).await;
        let draft = host.snapshot().await.unwrap().unwrap();
        let match_id = draft.match_id.unwrap();
        assert_eq!(draft.stage, LobbyStage::Running);
        assert_eq!(draft.game_state, Some(2));
        assert!(!host.match_details(match_id).await.unwrap().finished);
        // Extra worker reconciliation snapshots do not advance the simulation.
        for _ in 0..20 {
            assert_eq!(host.snapshot().await.unwrap().unwrap().game_state, Some(2));
        }
        host.prepare_simulation(&roster()).await.unwrap();
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(host.snapshot().await.unwrap().unwrap().game_state, Some(4));
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(host.snapshot().await.unwrap().unwrap().game_state, Some(5));
        tokio::time::advance(Duration::from_secs(20)).await;
        let finished = host.snapshot().await.unwrap().unwrap();
        assert_eq!(finished.stage, LobbyStage::Postgame);
        assert_eq!(finished.winner.as_deref(), Some("radiant"));
        let result = host.match_details(match_id).await.unwrap();
        assert!(result.finished);
        assert_eq!(result.league_id, settings().league_id);
        assert_eq!(result.players.len(), 10);
        assert_eq!(
            result
                .players
                .iter()
                .filter(|player| player.radiant)
                .count(),
            5
        );
        assert!(matches!(result.replay, ReplayMetadata::NotRecorded));
        host.destroy(lobby_id).await.unwrap();
        assert!(host.snapshot().await.unwrap().is_none());
        assert!(host.match_details(match_id).await.unwrap().finished);
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_requires_invites_seating_and_host_pool_before_launch() {
        let host = SimulatedHost::new(99);
        host.prepare_simulation(&roster()).await.unwrap();
        host.create(&settings()).await.unwrap();
        let id = host.snapshot().await.unwrap().unwrap().id;
        assert!(host.launch(id).await.is_err());
        for player in roster() {
            host.invite(id, player.steam_account_id).await.unwrap();
        }
        assert!(host.launch(id).await.is_err());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(host.launch(id).await.is_err());
        host.move_host_to_pool(id).await.unwrap();
        host.launch(id).await.unwrap();
        assert!(host.launch(id).await.is_err());
        assert!(host.invite(id, 1000).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_refuses_foreign_lobbies_accounts_and_roster_changes() {
        let host = SimulatedHost::new(99);
        let id = gather(&host).await;
        assert!(host.invite(id, 9999).await.is_err());
        assert!(host.invite(id + 10, 1000).await.is_err());
        assert!(host.launch(id + 10).await.is_err());
        assert!(host.destroy(id + 10).await.is_err());
        let mut changed = roster();
        changed[0].steam_account_id = 2000;
        assert!(host.prepare_simulation(&changed).await.is_err());
        assert!(host.create(&settings()).await.is_err());
        host.destroy(id).await.unwrap();
        host.prepare_simulation(&changed).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_can_be_cancelled_during_gameplay_without_a_result() {
        let host = SimulatedHost::new(99);
        let id = gather(&host).await;
        host.launch(id).await.unwrap();
        tokio::time::advance(Duration::from_secs(45)).await;
        let match_id = host.snapshot().await.unwrap().unwrap().match_id.unwrap();
        host.destroy(id).await.unwrap();
        assert!(host.snapshot().await.unwrap().is_none());
        assert!(host.match_details(match_id).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_rejects_incomplete_or_ambiguous_rosters() {
        let host = SimulatedHost::new(99);
        assert!(host.create(&settings()).await.is_err());
        let mut players = roster();
        players.pop();
        assert!(host.prepare_simulation(&players).await.is_err());
        players = roster();
        players[1].steam_account_id = players[0].steam_account_id;
        assert!(host.prepare_simulation(&players).await.is_err());
        players = roster();
        players[1].discord_id = players[0].discord_id;
        assert!(host.prepare_simulation(&players).await.is_err());
        players = roster();
        players[1].steam_account_id = 99;
        assert!(host.prepare_simulation(&players).await.is_err());
        players = roster();
        players[0].radiant = false;
        assert!(host.prepare_simulation(&players).await.is_err());
    }
}
