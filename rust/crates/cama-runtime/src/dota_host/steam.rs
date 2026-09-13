//! Translation between the Steam transport and the host's policy types.

use super::*;
use cama_steam::{
    DotaSteamClient, LobbyConfig, LobbyOutcome, LobbyState, SteamAuth, SteamAuthConfig, Team,
};

pub(super) async fn connect(config: &DotaHostConfig) -> Result<Arc<dyn DotaHostPort>, String> {
    let mut auth = SteamAuthConfig::new(config.username.expose(), &config.session_path);
    if let Some(password) = &config.password {
        auth = auth.with_password(password.expose());
    }
    if let Some(code) = &config.guard_code {
        auth = auth.with_guard_code(code.expose());
    }
    let client = DotaSteamClient::connect(&auth)
        .await
        .map_err(|e| e.to_string())?;
    if client.own_account_id() != config.account_id {
        return Err(
            "Steam session belongs to a different account than DOTA_BOT_ACCOUNT_ID".to_owned(),
        );
    }
    tracing::info!(
        account_id = client.own_account_id(),
        "Dota host connected to Steam and Game Coordinator"
    );
    Ok(Arc::new(SteamHost(client)))
}

pub async fn bootstrap_steam_login() -> Result<String, String> {
    let username = std::env::var("DOTA_STEAM_USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or("DOTA_STEAM_USERNAME is required")?;
    let password = std::env::var("DOTA_STEAM_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or("DOTA_STEAM_PASSWORD is required for this bootstrap invocation")?;
    let path = std::env::var_os("DOTA_STEAM_SESSION_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| "data/steam/session.json".into());
    let mut config = SteamAuthConfig::new(username, path)
        .with_password(password)
        .with_confirmation(cama_steam::GuardConfirmation::Console);
    if let Ok(code) = std::env::var("DOTA_STEAM_GUARD_CODE") {
        config = config.with_guard_code(code);
    }
    let authenticated = SteamAuth::bootstrap_login(&config)
        .await
        .map_err(|e| e.to_string())?;
    let account = authenticated
        .session
        .steam_id
        .checked_sub(dota_lobby::STEAM_INDIVIDUAL_BASE)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or("authenticated Steam ID is not an individual account")?;
    Ok(format!(
        "Steam session saved. Set DOTA_BOT_ACCOUNT_ID={account}; the serve process will reuse this session on startup."
    ))
}

struct SteamHost(DotaSteamClient);

fn playing_side(team: Team, coach_team: Option<Team>) -> Option<Side> {
    if matches!(coach_team, Some(Team::Radiant | Team::Dire)) {
        return None;
    }
    match team {
        Team::Radiant => Some(Side::Radiant),
        Team::Dire => Some(Side::Dire),
        _ => None,
    }
}

pub(super) fn lobby_seat(member: cama_steam::LobbyMember) -> LobbySeat {
    LobbySeat {
        account_id: member.account_id,
        side: playing_side(member.team, member.coach_team),
        // GC lobby player slots are 1..=5; admission uses 0..5. Keep an
        // unset/invalid slot invalid instead of aliasing it to the first seat.
        slot: member.slot.checked_sub(1).unwrap_or(u32::MAX),
    }
}

fn lobby_config(settings: &LobbySettings) -> LobbyConfig {
    LobbyConfig {
        game_name: settings.name.clone(),
        league_id: settings.league_id,
        server_region: settings.server_region,
        game_mode: settings.game_mode,
        cm_pick: match settings.first_pick_radiant {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        // An explicit empty key also clears a password if these settings are
        // used by a future lobby reconfiguration operation.
        pass_key: Some(settings.password.clone()),
        allow_cheats: false,
        fill_with_bots: false,
        allow_spectating: true,
        dota_tv_delay: settings.tv_delay as i32,
        visibility: settings.visibility,
        ..Default::default()
    }
}

#[async_trait]
impl DotaHostPort for SteamHost {
    async fn betting_observation_fresh(&self) -> bool {
        self.0
            .lobby_observation_is_fresh(Duration::from_secs(45))
            .await
    }
    async fn snapshot(&self) -> Result<Option<HostLobby>, String> {
        self.0
            .snapshot()
            .await
            .map_err(|e| e.to_string())?
            .map(|s| {
                let stage = match s.state {
                    LobbyState::Ui | LobbyState::ReadyUp | LobbyState::NotReady => {
                        LobbyStage::Gathering
                    }
                    LobbyState::ServerSetup | LobbyState::ServerAssign => LobbyStage::Allocating,
                    LobbyState::Run => LobbyStage::Running,
                    LobbyState::PostGame => LobbyStage::Postgame,
                    LobbyState::Unknown(_) => {
                        return Err("GC reported an unsupported lobby state".to_owned());
                    }
                };
                let first_pick_radiant = match s.cm_pick {
                    0 => None,
                    1 => Some(true),
                    2 => Some(false),
                    _ => return Err("GC reported an unsupported pick priority".to_owned()),
                };
                Ok(HostLobby {
                    id: s.lobby_id,
                    name: s.game_name,
                    owner_account_id: s
                        .leader_steam_id
                        .checked_sub(dota_lobby::STEAM_INDIVIDUAL_BASE)
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or("invalid lobby leader Steam ID")?,
                    league_id: s.league_id.unwrap_or(0),
                    game_mode: s.game_mode,
                    server_region: s.server_region,
                    first_pick_radiant,
                    cheats: s.allow_cheats,
                    fill_bots: s.fill_with_bots,
                    spectating: s.allow_spectating,
                    tv_delay: u32::try_from(s.dota_tv_delay).map_err(|_| "invalid TV delay")?,
                    visibility: s.visibility,
                    stage,
                    game_state: s.game_state,
                    match_id: s.match_id,
                    server_id: s.server_steam_id,
                    winner: match s.outcome {
                        LobbyOutcome::RadiantVictory => Some("radiant".into()),
                        LobbyOutcome::DireVictory => Some("dire".into()),
                        _ => None,
                    },
                    members: s.members.into_iter().map(lobby_seat).collect(),
                })
            })
            .transpose()
    }
    async fn create(&self, settings: &LobbySettings) -> Result<(), String> {
        let config = lobby_config(settings);
        self.0
            .create_lobby(&config)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    async fn configure(&self, lobby: u64, settings: &LobbySettings) -> Result<(), String> {
        self.0
            .configure_lobby(lobby, &lobby_config(settings))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    async fn invite(&self, lobby: u64, account: u32) -> Result<(), String> {
        self.0
            .invite_player(
                lobby,
                dota_lobby::STEAM_INDIVIDUAL_BASE + u64::from(account),
            )
            .await
            .map_err(|e| e.to_string())
    }
    async fn move_host_to_pool(&self, lobby: u64) -> Result<(), String> {
        // The request uses slot 1; Dota reports slot 0 after entering the pool.
        self.0
            .set_team_slot(lobby, self.0.own_account_id(), Team::PlayerPool, 1)
            .await
            .map_err(|e| e.to_string())
    }
    async fn kick_from_team(&self, lobby: u64, account: u32) -> Result<(), String> {
        self.0
            .kick_from_team(lobby, account)
            .await
            .map_err(|e| e.to_string())
    }
    async fn kick(&self, lobby: u64, account: u32) -> Result<(), String> {
        self.0
            .kick_player(lobby, account)
            .await
            .map_err(|e| e.to_string())
    }
    async fn launch(&self, lobby: u64) -> Result<(), String> {
        self.0
            .launch_lobby(lobby, 0)
            .await
            .map_err(|e| e.to_string())
    }
    async fn destroy(&self, lobby: u64) -> Result<(), String> {
        self.0.destroy_lobby(lobby).await.map_err(|e| e.to_string())
    }
    async fn match_details(&self, id: u64) -> Result<HostMatchDetails, String> {
        let details = self.0.match_details(id).await.map_err(|e| e.to_string())?;
        let winner = match details.outcome {
            LobbyOutcome::RadiantVictory => Some("radiant".into()),
            LobbyOutcome::DireVictory => Some("dire".into()),
            _ => None,
        };
        let replay = match details.replay {
            Some(r) => match r.state {
                0 => match (details.cluster, r.salt) {
                    (Some(cluster), Some(salt)) if cluster > 0 && salt > 0 => {
                        ReplayMetadata::Available { cluster, salt }
                    }
                    _ => ReplayMetadata::Pending,
                },
                1 => ReplayMetadata::NotRecorded,
                2 => ReplayMetadata::Expired,
                _ => ReplayMetadata::Pending,
            },
            None => ReplayMetadata::Pending,
        };
        let finished = details.outcome != LobbyOutcome::Unknown || details.duration > 0;
        let players = details
            .players
            .into_iter()
            .map(|p| match p.team {
                Team::Radiant => Ok(HostedMatchPlayer {
                    account32: p.account_id,
                    radiant: true,
                }),
                Team::Dire => Ok(HostedMatchPlayer {
                    account32: p.account_id,
                    radiant: false,
                }),
                _ => Err("completed match has a nonstandard player side".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default();
        // Sparse GC responses may omit identity fields needed to attach stats
        // safely. The authoritative result can still settle; API enrichment
        // will recover statistics without guessing a player's seat.
        let mut postgame_statistics =
            statistics_have_roster(&details.postgame_statistics, &players)
                .then_some(details.postgame_statistics);
        if finished
            && winner.is_some()
            && let Some(statistics) = postgame_statistics.as_mut()
            && let Some(graph) =
                optional_win_probability(|| self.0.postgame_win_probability(id)).await
        {
            statistics["_cama_win_probability"] = graph;
        }
        Ok(HostMatchDetails {
            match_id: details.match_id,
            league_id: details.league_id.unwrap_or(0),
            winner,
            finished,
            players,
            replay,
            postgame_statistics,
        })
    }
}

// Keep optional metadata availability out of authoritative result settlement.
async fn optional_win_probability<F, Fut>(mut fetch: F) -> Option<serde_json::Value>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<
            Output = Result<
                cama_steam::metadata::WinProbabilityGraph,
                cama_steam::metadata::MetadataError,
            >,
        >,
{
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        for attempt in 0..2 {
            match fetch().await {
                Ok(graph) => return Some(graph.statistics_value()),
                Err(error) => {
                    tracing::debug!(%error, "optional postgame win probability unavailable")
                }
            }
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

fn statistics_have_roster(payload: &serde_json::Value, roster: &[HostedMatchPlayer]) -> bool {
    let Some(players) = payload.get("players").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let mut accounts = std::collections::BTreeSet::new();
    players.len() == 10
        && roster.len() == 10
        && players.iter().all(|player| {
            let Some(account) = player
                .get("account_id")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
            else {
                return false;
            };
            let Some(slot) = player
                .get("player_slot")
                .and_then(serde_json::Value::as_u64)
                .filter(|slot| *slot <= 4 || (128..=132).contains(slot))
            else {
                return false;
            };
            accounts.insert(account)
                && roster
                    .iter()
                    .any(|entry| entry.account32 == account && entry.radiant == (slot < 128))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn optional_graph_timeout_does_not_hold_result_settlement() {
        let start = tokio::time::Instant::now();
        let graph = optional_win_probability(std::future::pending).await;
        assert!(graph.is_none());
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(8));
    }

    #[tokio::test(start_paused = true)]
    async fn optional_graph_retries_once_then_returns_without_statistics() {
        let mut calls = 0;
        let graph = optional_win_probability(|| {
            calls += 1;
            std::future::ready(Err(cama_steam::metadata::MetadataError::Unavailable))
        })
        .await;
        assert!(graph.is_none());
        assert_eq!(calls, 2);
    }

    #[test]
    fn sparse_statistics_without_canonical_identity_fall_back_to_api() {
        let roster = (1..=10)
            .map(|account32| HostedMatchPlayer {
                account32,
                radiant: account32 <= 5,
            })
            .collect::<Vec<_>>();
        let mut payload = serde_json::json!({"players": roster.iter().enumerate().map(|(index, player)| serde_json::json!({
            "account_id": player.account32, "player_slot": if player.radiant {index % 5} else {128 + index % 5}
        })).collect::<Vec<_>>()});
        assert!(statistics_have_roster(&payload, &roster));
        payload["players"][0]
            .as_object_mut()
            .unwrap()
            .remove("player_slot");
        assert!(!statistics_have_roster(&payload, &roster));
    }

    #[test]
    fn explicit_no_coach_team_preserves_a_players_side() {
        assert_eq!(
            playing_side(Team::Radiant, Some(Team::NoTeam)),
            Some(Side::Radiant)
        );
        assert_eq!(playing_side(Team::Dire, None), Some(Side::Dire));
        assert_eq!(playing_side(Team::Radiant, Some(Team::Radiant)), None);
        assert_eq!(playing_side(Team::Spectator, Some(Team::Dire)), None);
    }

    #[test]
    fn lobby_slot_conversion_keeps_invalid_slots_and_coaches_out_of_playing_seats() {
        for slot in [0, 1, 5, 6, u32::MAX] {
            let member = cama_steam::LobbyMember {
                steam_id: dota_lobby::STEAM_INDIVIDUAL_BASE + 1,
                account_id: 1,
                hero_id: 0,
                team: Team::Radiant,
                name: String::new(),
                slot,
                party_id: None,
                coach_team: None,
            };
            let seat = lobby_seat(member.clone());
            assert_eq!(seat.side, Some(Side::Radiant));
            assert_eq!(seat.slot < 5, (1..=5).contains(&slot));
            let coach = lobby_seat(cama_steam::LobbyMember {
                coach_team: Some(Team::Radiant),
                ..member
            });
            assert_eq!(coach.side, None);
        }
    }

    #[test]
    fn public_lobby_configuration_has_no_password_and_allows_spectators() {
        let settings = LobbySettings {
            name: "Cama 1:2".into(),
            password: String::new(),
            visibility: 0,
            league_id: 123,
            server_region: 1,
            game_mode: 2,
            first_pick_radiant: Some(false),
            tv_delay: 2,
        };
        let config = lobby_config(&settings);
        assert_eq!(config.visibility, 0);
        assert_eq!(config.pass_key.as_deref(), Some(""));
        assert!(config.allow_spectating);
        assert!(!config.allow_cheats && !config.fill_with_bots);
        assert_eq!(config.cm_pick, 2);
    }

    #[test]
    fn legacy_session_settings_keep_their_original_visibility() {
        let settings: LobbySettings = serde_json::from_value(serde_json::json!({
            "name": "old session", "password": "old lobby key", "league_id": 123,
            "server_region": 1, "game_mode": 2, "first_pick_radiant": null, "tv_delay": 2
        }))
        .unwrap();
        assert_eq!(settings.visibility, 2);
        assert_eq!(
            lobby_config(&settings).pass_key.as_deref(),
            Some("old lobby key")
        );
    }
}
