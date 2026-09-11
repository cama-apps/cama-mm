//! Dota 2 Game Coordinator transport and typed lobby state.
//!
//! Dota lobby commands are deliberately treated as asynchronous commands.  The
//! GC does not acknowledge most of them with a useful response; the authoritative
//! result arrives through the SOCache.  The client therefore only reports a
//! lobby as ready after the initial SOCache subscription has been observed, and
//! exposes subsequent changes through both [`DotaSteamClient::snapshot`] and
//! [`DotaSteamClient::subscribe`].

use crate::{AuthenticatedSteam, SO_TYPE_LOBBY, SteamAuth, SteamAuthConfig};
use futures_util::StreamExt;
use std::fmt::{Debug, Formatter};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use steam_vent::message::{EncodableMessage, MalformedBody};
use steam_vent::{ConnectionTrait, GameCoordinator, NetMessage, NetMessageHeader, NetworkError};
use steam_vent_proto_common::protobuf::{Enum, EnumOrUnknown, Message, MessageField};
use steam_vent_proto_dota2::base_gcmessages::CMsgInviteToLobby;
use steam_vent_proto_dota2::dota_gcmessages_client::{
    CMsgDOTADestroyLobbyRequest, CMsgGCMatchDetailsRequest, CMsgGCMatchDetailsResponse,
};
use steam_vent_proto_dota2::dota_gcmessages_client_match_management::{
    CMsgPracticeLobbyCreate, CMsgPracticeLobbyKick, CMsgPracticeLobbyKickFromTeam,
    CMsgPracticeLobbyLaunch, CMsgPracticeLobbyLeave, CMsgPracticeLobbySetDetails,
    CMsgPracticeLobbySetTeamSlot,
};
use steam_vent_proto_dota2::dota_gcmessages_common::CMsgDOTAMatch;
use steam_vent_proto_dota2::dota_gcmessages_common_lobby::{CSODOTALobby, CSODOTALobbyMember};
use steam_vent_proto_dota2::dota_gcmessages_msgid::EDOTAGCMsg;
use steam_vent_proto_dota2::dota_gcmessages_server::{
    CMsgDOTALiveScoreboardUpdate, cmsg_dotalive_scoreboard_update,
};
use steam_vent_proto_dota2::dota_shared_enums::{DOTA_CM_PICK, DOTA_GC_TEAM, DOTALobbyVisibility};
use steam_vent_proto_dota2::gcsdk_gcmessages::{
    CMsgClientWelcome, CMsgSOCacheSubscribed, CMsgSOCacheSubscribedUpToDate,
    CMsgSOCacheUnsubscribed, CMsgSOSingleObject,
};
use steam_vent_proto_dota2::gcsystemmsgs::ESOMsg;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::warn;

const EVENT_BUFFER: usize = 128;
const INITIAL_CACHE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ERESULT_OK: u32 = 1;
const SO_OWNER_ACCOUNT: u32 = 1;
const SO_OWNER_LOBBY: u32 = 3;

// steam-vent-proto-dota2 0.5.2 predates Valve's addition of the 60-second
// delay and the 900-second wire value.  Keep these current wire values raw in
// EnumOrUnknown rather than asking that stale generated enum to interpret them.
const DOTA_TV_DELAY_10_SECONDS: i32 = 0;
const DOTA_TV_DELAY_60_SECONDS: i32 = 1;
const DOTA_TV_DELAY_120_SECONDS: i32 = 2;
const DOTA_TV_DELAY_300_SECONDS: i32 = 3;
const DOTA_TV_DELAY_900_SECONDS: i32 = 4;
const VALID_DOTA_TV_DELAY_VALUES: [i32; 5] = [
    DOTA_TV_DELAY_10_SECONDS,
    DOTA_TV_DELAY_60_SECONDS,
    DOTA_TV_DELAY_120_SECONDS,
    DOTA_TV_DELAY_300_SECONDS,
    DOTA_TV_DELAY_900_SECONDS,
];

/// Errors returned by the Steam/GC lobby transport.
#[derive(Debug, Error)]
pub enum DotaSteamError {
    #[error("Steam authentication failed: {0}")]
    Auth(#[from] crate::AuthError),
    #[error("Steam network error: {0}")]
    Network(#[source] NetworkError),
    #[error("Dota protobuf error: {0}")]
    Protobuf(#[source] steam_vent_proto_common::protobuf::Error),
    #[error("the Dota Game Coordinator cache is not hydrated or the connection is unavailable")]
    NotReady,
    #[error("the authenticated account is not the lobby host")]
    NotHost,
    #[error("there is no authoritative lobby in the SOCache")]
    NoLobby,
    #[error("expected lobby {expected}, found {found:?}")]
    WrongLobby { expected: u64, found: Option<u64> },
    #[error("a lobby already exists ({0})")]
    AlreadyInLobby(u64),
    #[error("timed out waiting for the Dota Game Coordinator")]
    Timeout,
    #[error(
        "timed out establishing a Dota Game Coordinator session (last status: {last_status:?})"
    )]
    HandshakeTimeout { last_status: Option<i32> },
    #[error("the Dota Game Coordinator suspended this session (raw time_end: {time_end:?})")]
    SessionSuspended { time_end: Option<u32> },
    #[error("invalid {field} value {value}")]
    InvalidConfig { field: &'static str, value: i32 },
    #[error("invalid team value {0}")]
    InvalidTeam(i32),
    #[error("match details request failed with GC result {0}")]
    MatchRequestFailed(u32),
    #[error("the Game Coordinator returned no details for match {0}")]
    MatchNotFound(u64),
    #[error("event subscription is closed")]
    EventSubscriptionClosed,
}

impl From<NetworkError> for DotaSteamError {
    fn from(error: NetworkError) -> Self {
        Self::Network(error)
    }
}

/// A Dota team as used by a practice lobby.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Team {
    Radiant,
    Dire,
    Broadcaster,
    Spectator,
    PlayerPool,
    NoTeam,
    Custom(u8),
    Unknown(i32),
}

impl Team {
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Radiant => DOTA_GC_TEAM::DOTA_GC_TEAM_GOOD_GUYS as i32,
            Self::Dire => DOTA_GC_TEAM::DOTA_GC_TEAM_BAD_GUYS as i32,
            Self::Broadcaster => DOTA_GC_TEAM::DOTA_GC_TEAM_BROADCASTER as i32,
            Self::Spectator => DOTA_GC_TEAM::DOTA_GC_TEAM_SPECTATOR as i32,
            Self::PlayerPool => DOTA_GC_TEAM::DOTA_GC_TEAM_PLAYER_POOL as i32,
            Self::NoTeam => DOTA_GC_TEAM::DOTA_GC_TEAM_NOTEAM as i32,
            Self::Custom(team) => 5 + team as i32,
            Self::Unknown(value) => value,
        }
    }

    fn to_proto(self) -> Result<DOTA_GC_TEAM, DotaSteamError> {
        DOTA_GC_TEAM::from_i32(self.as_raw()).ok_or(DotaSteamError::InvalidTeam(self.as_raw()))
    }

    fn from_raw(value: i32) -> Self {
        match value {
            0 => Self::Radiant,
            1 => Self::Dire,
            2 => Self::Broadcaster,
            3 => Self::Spectator,
            4 => Self::PlayerPool,
            5 => Self::NoTeam,
            6..=13 => Self::Custom((value - 5) as u8),
            value => Self::Unknown(value),
        }
    }
}

/// The state of a practice lobby as reported by `CSODOTALobby`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LobbyState {
    Ui,
    ReadyUp,
    ServerSetup,
    Run,
    PostGame,
    NotReady,
    ServerAssign,
    Unknown(i32),
}

impl LobbyState {
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Ui => 0,
            Self::ReadyUp => 4,
            Self::ServerSetup => 1,
            Self::Run => 2,
            Self::PostGame => 3,
            Self::NotReady => 5,
            Self::ServerAssign => 6,
            Self::Unknown(value) => value,
        }
    }

    fn from_raw(value: i32) -> Self {
        match value {
            0 => Self::Ui,
            4 => Self::ReadyUp,
            1 => Self::ServerSetup,
            2 => Self::Run,
            3 => Self::PostGame,
            5 => Self::NotReady,
            6 => Self::ServerAssign,
            value => Self::Unknown(value),
        }
    }
}

/// Match result reported by the lobby or the match details response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LobbyOutcome {
    Unknown,
    RadiantVictory,
    DireVictory,
    NeutralVictory,
    NoTeamWinner,
    Custom(u8),
    NotScored(i32),
    UnknownValue(i32),
}

impl LobbyOutcome {
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Unknown => 0,
            Self::RadiantVictory => 2,
            Self::DireVictory => 3,
            Self::NeutralVictory => 4,
            Self::NoTeamWinner => 5,
            Self::Custom(value) => 5 + value as i32,
            Self::NotScored(value) | Self::UnknownValue(value) => value,
        }
    }

    fn from_raw(value: i32) -> Self {
        match value {
            0 => Self::Unknown,
            2 => Self::RadiantVictory,
            3 => Self::DireVictory,
            4 => Self::NeutralVictory,
            5 => Self::NoTeamWinner,
            6..=13 => Self::Custom((value - 5) as u8),
            64..=69 => Self::NotScored(value),
            value => Self::UnknownValue(value),
        }
    }
}

/// Configuration sent in a lobby create or set-details message.
///
/// `cm_pick`, `dota_tv_delay`, and `visibility` use numeric wire values from
/// Valve's protobufs so the application does not have to depend on generated
/// proto enum names.  The current Dota TV delay mapping is validated before a
/// command is sent because the bundled generated enum is stale.
#[derive(Clone, Eq, PartialEq)]
pub struct LobbyConfig {
    pub game_name: String,
    pub league_id: u32,
    pub server_region: u32,
    pub game_mode: u32,
    pub cm_pick: i32,
    pub pass_key: Option<String>,
    pub allow_cheats: bool,
    pub fill_with_bots: bool,
    pub allow_spectating: bool,
    pub dota_tv_delay: i32,
    pub visibility: i32,
    pub client_version: u32,
    pub timeout: Duration,
}

impl Default for LobbyConfig {
    fn default() -> Self {
        Self {
            game_name: String::new(),
            league_id: 0,
            server_region: 0,
            game_mode: 1,
            cm_pick: DOTA_CM_PICK::DOTA_CM_RANDOM as i32,
            pass_key: None,
            allow_cheats: false,
            fill_with_bots: false,
            allow_spectating: true,
            dota_tv_delay: DOTA_TV_DELAY_10_SECONDS,
            visibility: DOTALobbyVisibility::DOTALobbyVisibility_Public as i32,
            client_version: 0,
            timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

impl Debug for LobbyConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LobbyConfig")
            .field("game_name", &self.game_name)
            .field("league_id", &self.league_id)
            .field("server_region", &self.server_region)
            .field("game_mode", &self.game_mode)
            .field("cm_pick", &self.cm_pick)
            .field("pass_key", &self.pass_key.as_ref().map(|_| "<redacted>"))
            .field("allow_cheats", &self.allow_cheats)
            .field("fill_with_bots", &self.fill_with_bots)
            .field("allow_spectating", &self.allow_spectating)
            .field("dota_tv_delay", &self.dota_tv_delay)
            .field("visibility", &self.visibility)
            .field("client_version", &self.client_version)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl LobbyConfig {
    fn validate(&self) -> Result<(), DotaSteamError> {
        if DOTA_CM_PICK::from_i32(self.cm_pick).is_none() {
            return Err(DotaSteamError::InvalidConfig {
                field: "cm_pick",
                value: self.cm_pick,
            });
        }
        if !VALID_DOTA_TV_DELAY_VALUES.contains(&self.dota_tv_delay) {
            return Err(DotaSteamError::InvalidConfig {
                field: "dota_tv_delay",
                value: self.dota_tv_delay,
            });
        }
        if DOTALobbyVisibility::from_i32(self.visibility).is_none() {
            return Err(DotaSteamError::InvalidConfig {
                field: "visibility",
                value: self.visibility,
            });
        }
        Ok(())
    }
}

/// One account in the authoritative lobby snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyMember {
    pub steam_id: u64,
    pub account_id: u32,
    pub hero_id: i32,
    pub team: Team,
    pub name: String,
    pub slot: u32,
    pub party_id: Option<u64>,
    pub coach_team: Option<Team>,
}

/// An authoritative practice lobby state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbySnapshot {
    pub lobby_id: u64,
    pub leader_steam_id: u64,
    pub server_steam_id: Option<u64>,
    pub match_id: Option<u64>,
    pub game_name: String,
    pub league_id: Option<u32>,
    pub server_region: u32,
    pub game_mode: u32,
    pub cm_pick: i32,
    pub allow_cheats: bool,
    pub fill_with_bots: bool,
    pub allow_spectating: bool,
    pub dota_tv_delay: i32,
    pub visibility: i32,
    pub state: LobbyState,
    /// Raw `DOTA_GameState` wire value, if the lobby supplied one.
    ///
    /// This deliberately remains optional and untyped: the generated getter
    /// substitutes `INIT` when the field is absent and turns future enum
    /// values into the same fallback, which would make a missing or newer
    /// game state indistinguishable from an explicit `INIT` value.
    pub game_state: Option<i32>,
    pub outcome: LobbyOutcome,
    pub game_start_time: Option<u32>,
    pub match_duration: Option<u32>,
    pub connect: Option<String>,
    pub pending_invites: Vec<u64>,
    pub members: Vec<LobbyMember>,
}

/// A player entry in the live server scoreboard.
#[derive(Clone, Debug, PartialEq)]
pub struct LivePlayer {
    pub player_slot: u32,
    pub player_name: String,
    pub hero_name: String,
    pub hero_id: i32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub last_hits: u32,
    pub denies: u32,
    pub gold: u32,
    pub level: u32,
    pub gold_per_min: f32,
    pub xp_per_min: f32,
    pub account_id: u32,
    pub net_worth: u32,
}

/// Team scores and players from a live scoreboard update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveTeam {
    pub score: u32,
    pub tower_state: u32,
    pub barracks_state: u32,
    pub hero_picks: Vec<i32>,
    pub hero_bans: Vec<i32>,
    pub players: Vec<LivePlayer>,
}

/// Live scoreboard data sent by a Dota game server through the GC.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveScoreboard {
    pub tournament_id: u32,
    pub tournament_game_id: u32,
    pub duration: f32,
    pub hltv_delay: i32,
    pub roshan_respawn_timer: u32,
    pub league_id: Option<u32>,
    pub match_id: Option<u64>,
    pub radiant: LiveTeam,
    pub dire: LiveTeam,
}

/// A match player returned by the GC match-details endpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchPlayer {
    pub account_id: u32,
    pub player_slot: u32,
    pub team: Team,
    pub team_slot: Option<u32>,
    pub hero_id: i32,
    pub player_name: String,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub gold: u32,
    pub last_hits: u32,
    pub denies: u32,
    pub gold_per_min: u32,
    pub xp_per_min: u32,
    pub hero_damage: u32,
    pub tower_damage: u32,
    pub hero_healing: u32,
    pub level: u32,
    pub net_worth: u32,
}

/// Replay and game-server metadata attached to a match-details response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchReplay {
    pub state: i32,
    pub salt: Option<u32>,
    pub server_ip: Option<u32>,
    pub server_port: Option<u32>,
}

/// Sensitive download coordinates for a match's metadata file.
///
/// Keep these coordinates out of public statistics and logs. This type does
/// not implement serialization; exporting credentials must be explicit.
#[derive(Clone)]
pub struct MatchMetadataLocation {
    pub match_id: u64,
    pub duration: Option<u32>,
    pub pre_game_duration: Option<u32>,
    pub start_time: Option<u32>,
    pub radiant_win: Option<bool>,
    pub cluster: Option<u32>,
    pub replay_salt: Option<u32>,
    private_metadata_key: Option<u32>,
}

impl MatchMetadataLocation {
    pub fn private_metadata_key(&self) -> Option<u32> {
        self.private_metadata_key
    }
}

impl Debug for MatchMetadataLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatchMetadataLocation")
            .field("match_id", &self.match_id)
            .field("cluster_present", &self.cluster.is_some())
            .field("replay_salt", &"[redacted]")
            .field("private_metadata_key", &"[redacted]")
            .finish()
    }
}

fn map_metadata_location(match_: &CMsgDOTAMatch) -> MatchMetadataLocation {
    MatchMetadataLocation {
        match_id: match_.match_id(),
        duration: match_.duration,
        pre_game_duration: match_.pre_game_duration,
        start_time: match_.starttime,
        radiant_win: match_
            .match_outcome
            .and_then(|outcome| match outcome.value() {
                2 => Some(true),
                3 => Some(false),
                _ => None,
            }),
        cluster: match_.cluster,
        replay_salt: match_.replay_salt,
        private_metadata_key: match_.private_metadata_key,
    }
}

/// A team summary in match details.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchTeam {
    pub team: Team,
    pub team_id: Option<u32>,
    pub name: String,
    pub score: Option<u32>,
}

/// Match metadata and final outcome returned by the Dota GC.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchDetails {
    pub match_id: u64,
    pub duration: u32,
    pub start_time: u32,
    pub first_blood_time: Option<u32>,
    pub cluster: Option<u32>,
    pub league_id: Option<u32>,
    pub game_mode: Option<i32>,
    pub outcome: LobbyOutcome,
    pub radiant_team_id: Option<u32>,
    pub dire_team_id: Option<u32>,
    pub radiant_team_name: String,
    pub dire_team_name: String,
    pub radiant_score: Option<u32>,
    pub dire_score: Option<u32>,
    pub replay: Option<MatchReplay>,
    pub players: Vec<MatchPlayer>,
    pub teams: Vec<MatchTeam>,
    pub gc_result: u32,
    /// Sparse public statistics using OpenDota field names; absent GC fields stay absent.
    pub postgame_statistics: serde_json::Value,
}

/// Events emitted after authoritative cache or live-scoreboard updates.
#[derive(Clone, Debug, PartialEq)]
pub enum DotaSteamEvent {
    CacheHydrated,
    LobbyUpdated(LobbySnapshot),
    LobbyCleared { lobby_id: Option<u64> },
    LiveScoreboard(LiveScoreboard),
    TransportDisconnected { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheStatus {
    Hydrating,
    Ready,
    Disconnected,
}

#[derive(Debug)]
struct ClientState {
    own_steam_id: u64,
    status: CacheStatus,
    lobby: Option<LobbySnapshot>,
    lobby_observed_at: Option<Instant>,
}

impl ClientState {
    fn lobby_observation_is_fresh(&self, max_age: Duration) -> bool {
        self.status == CacheStatus::Ready
            && self.lobby.is_some()
            && self
                .lobby_observed_at
                .is_some_and(|at| at.elapsed() <= max_age)
    }
}

/// Dota Steam/Game Coordinator client.
#[derive(Clone)]
pub struct DotaSteamClient {
    // GameCoordinator alone does not retain steam-vent's heartbeat drop guard.
    _connection: steam_vent::Connection,
    gc: Arc<Mutex<GameCoordinator>>,
    state: Arc<RwLock<ClientState>>,
    events: broadcast::Sender<DotaSteamEvent>,
    _watcher: Arc<WatcherTask>,
    own_steam_id: u64,
    own_account_id: u32,
    command_timeout: Duration,
}

struct WatcherTask(JoinHandle<()>);

impl Debug for WatcherTask {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherTask").finish_non_exhaustive()
    }
}

impl Drop for WatcherTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Debug for DotaSteamClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DotaSteamClient")
            .field("own_steam_id", &self.own_steam_id)
            .field("own_account_id", &self.own_account_id)
            .field("command_timeout", &self.command_timeout)
            .finish_non_exhaustive()
    }
}

impl DotaSteamClient {
    /// Connect using a persisted refresh token.  This path is strictly
    /// non-interactive; use [`Self::bootstrap`] for the first login.
    pub async fn connect(config: &SteamAuthConfig) -> Result<Self, DotaSteamError> {
        let auth = SteamAuth::connect(config).await?;
        Self::from_authenticated(auth).await
    }

    /// Bootstrap an account with a password/Steam Guard handler and persist
    /// the resulting session before starting the GC transport.
    pub async fn bootstrap(config: &SteamAuthConfig) -> Result<Self, DotaSteamError> {
        let auth = SteamAuth::bootstrap_login(config).await?;
        Self::from_authenticated(auth).await
    }

    /// Build a Dota client from an already-authenticated Steam connection.
    pub async fn from_authenticated(auth: AuthenticatedSteam) -> Result<Self, DotaSteamError> {
        let steam_id = auth.connection.steam_id();
        let own_steam_id = steam_id.steam64();
        let own_account_id = steam_id.account_id();
        let (gc, welcome) = crate::handshake::connect(&auth.connection).await?;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let state = Arc::new(RwLock::new(ClientState {
            own_steam_id,
            status: CacheStatus::Hydrating,
            lobby: None,
            lobby_observed_at: None,
        }));
        let gc = Arc::new(Mutex::new(gc));
        let (watcher_ready, watcher_start) = oneshot::channel();
        // Register all message streams before processing the handshake result.
        // GameCoordinator's filter broadcasts each message to subscribers and
        // drops messages that arrive before a kind subscription exists.
        let watcher = spawn_watchers(&gc, &state, &events, watcher_start).await;
        let client = Self {
            _connection: auth.connection,
            gc,
            state,
            events,
            _watcher: Arc::new(watcher),
            own_steam_id,
            own_account_id,
            command_timeout: INITIAL_CACHE_TIMEOUT,
        };

        // Dota commonly includes the initial SOCache snapshot in the welcome
        // envelope itself.  It is not replayed through GameCoordinator's
        // message streams, so hydrate it explicitly before waiting for ready.
        apply_welcome(welcome, &client.state, &client.events).await;
        // Do not let a post-welcome SOCache update race the welcome snapshot.
        // The streams were registered above, so messages remain eligible for
        // delivery while the initial envelope is applied; only their state
        // handlers are gated until that ordering point is complete.
        let _ = watcher_ready.send(());
        client.wait_for_initial_cache().await?;
        Ok(client)
    }

    /// Steam64 of the authenticated account.
    pub const fn own_steam_id(&self) -> u64 {
        self.own_steam_id
    }

    /// Steam account id of the authenticated account.
    pub const fn own_account_id(&self) -> u32 {
        self.own_account_id
    }

    /// Subscribe to lobby/cache/live scoreboard updates.
    pub fn subscribe(&self) -> broadcast::Receiver<DotaSteamEvent> {
        self.events.subscribe()
    }

    /// Read the current authoritative lobby.  `None` means the cache is ready
    /// and the account is not in a lobby; [`DotaSteamError::NotReady`] means
    /// that no authoritative answer is available yet.
    pub async fn snapshot(&self) -> Result<Option<LobbySnapshot>, DotaSteamError> {
        let state = self.state.read().await;
        match state.status {
            CacheStatus::Ready => Ok(state.lobby.clone()),
            CacheStatus::Hydrating | CacheStatus::Disconnected => Err(DotaSteamError::NotReady),
        }
    }

    /// Whether actual lobby data arrived recently from the coordinator.
    /// Reading the cache does not refresh this observation.
    pub async fn lobby_observation_is_fresh(&self, max_age: Duration) -> bool {
        self.state.read().await.lobby_observation_is_fresh(max_age)
    }

    /// Return the current game-server SteamID used by server-side realtime
    /// stats APIs, if the lobby has already been assigned a server.
    pub async fn server_steam_id(&self) -> Result<Option<u64>, DotaSteamError> {
        Ok(self
            .snapshot()
            .await?
            .and_then(|snapshot| snapshot.server_steam_id))
    }

    /// Create a practice lobby and wait for its matching SOCache object.
    pub async fn create_lobby(
        &self,
        config: &LobbyConfig,
    ) -> Result<LobbySnapshot, DotaSteamError> {
        config.validate()?;
        if let Some(snapshot) = self.snapshot().await? {
            return Err(DotaSteamError::AlreadyInLobby(snapshot.lobby_id));
        }

        let details = build_lobby_details(config, None)?;
        let mut request = CMsgPracticeLobbyCreate::new();
        request.set_client_version(config.client_version);
        request.set_search_key(config.game_name.clone());
        if let Some(pass_key) = config.pass_key.clone() {
            request.set_pass_key(pass_key);
        }
        request.lobby_details = MessageField::some(details);
        self.send(request).await?;

        self.wait_for_lobby(config.timeout, |snapshot| {
            snapshot.game_name == config.game_name
                && (config.league_id == 0 || snapshot.league_id == Some(config.league_id))
        })
        .await
    }

    /// Update lobby configuration and wait for the authoritative matching
    /// SOCache update.
    pub async fn configure_lobby(
        &self,
        lobby_id: u64,
        config: &LobbyConfig,
    ) -> Result<LobbySnapshot, DotaSteamError> {
        config.validate()?;
        self.require_host(lobby_id).await?;
        let request = build_lobby_details(config, Some(lobby_id))?;
        self.send(request).await?;
        self.wait_for_lobby(config.timeout, |snapshot| {
            snapshot.lobby_id == lobby_id && lobby_config_matches(config, snapshot)
        })
        .await
    }

    /// Invite a Steam64 account to the current lobby.
    pub async fn invite_player(&self, lobby_id: u64, steam_id: u64) -> Result<(), DotaSteamError> {
        self.require_host(lobby_id).await?;
        let mut request = CMsgInviteToLobby::new();
        request.set_steam_id(steam_id);
        self.send(request).await
    }

    /// Set the authenticated account's own team and slot.  The wire message
    /// intentionally has no target account field; accepting another account
    /// here would report success while moving the wrong player.
    pub async fn set_team_slot(
        &self,
        lobby_id: u64,
        account_id: u32,
        team: Team,
        slot: u32,
    ) -> Result<(), DotaSteamError> {
        let snapshot = self.require_host(lobby_id).await?;
        if account_id != self.own_account_id {
            return Err(DotaSteamError::InvalidConfig {
                field: "account_id (set_team_slot must be authenticated account)",
                value: account_id as i32,
            });
        }
        if !snapshot
            .members
            .iter()
            .any(|member| member.account_id == account_id)
        {
            return Err(DotaSteamError::InvalidConfig {
                field: "account_id (account is not in lobby)",
                value: account_id as i32,
            });
        }
        let mut request = CMsgPracticeLobbySetTeamSlot::new();
        request.set_team(team.to_proto()?);
        request.set_slot(slot);
        self.send(request).await
    }

    /// Kick an account from the lobby entirely.
    pub async fn kick_player(&self, lobby_id: u64, account_id: u32) -> Result<(), DotaSteamError> {
        self.require_host(lobby_id).await?;
        let mut request = CMsgPracticeLobbyKick::new();
        request.set_account_id(account_id);
        self.send(request).await
    }

    /// Move an account from its team to the player pool.
    pub async fn kick_from_team(
        &self,
        lobby_id: u64,
        account_id: u32,
    ) -> Result<(), DotaSteamError> {
        self.require_host(lobby_id).await?;
        let mut request = CMsgPracticeLobbyKickFromTeam::new();
        request.set_account_id(account_id);
        self.send(request).await
    }

    /// Ask the GC to start the match.  The state transition and assigned
    /// server are delivered through the SOCache subscription.
    pub async fn launch_lobby(
        &self,
        lobby_id: u64,
        client_version: u32,
    ) -> Result<(), DotaSteamError> {
        self.require_host(lobby_id).await?;
        let mut request = CMsgPracticeLobbyLaunch::new();
        request.set_client_version(client_version);
        self.send(request).await
    }

    /// Leave the current lobby as the authenticated account.
    pub async fn leave_lobby(&self, lobby_id: u64) -> Result<(), DotaSteamError> {
        self.require_lobby(lobby_id).await?;
        self.send(CMsgPracticeLobbyLeave::new()).await
    }

    /// Destroy the current lobby using the dedicated destroy-lobby message.
    pub async fn destroy_lobby(&self, lobby_id: u64) -> Result<(), DotaSteamError> {
        self.require_host(lobby_id).await?;
        self.send(DestroyLobbyWire::default()).await
    }

    /// Request authoritative match details, including roster, outcome and
    /// replay metadata.
    pub async fn match_details(&self, match_id: u64) -> Result<MatchDetails, DotaSteamError> {
        let response = self.match_details_response(match_id).await?;
        let details = match_response_details(&response, match_id)?;
        Ok(map_match_details(details, response.result()))
    }

    /// Request metadata download coordinates without fetching a replay or file.
    pub async fn match_metadata_location(
        &self,
        match_id: u64,
    ) -> Result<MatchMetadataLocation, DotaSteamError> {
        let response = self.match_details_response(match_id).await?;
        let details = match_response_details(&response, match_id)?;
        if details.match_id != Some(match_id) || match_id == 0 {
            return Err(DotaSteamError::MatchNotFound(match_id));
        }
        Ok(map_metadata_location(details))
    }

    async fn match_details_response(
        &self,
        match_id: u64,
    ) -> Result<CMsgGCMatchDetailsResponse, DotaSteamError> {
        let mut request = CMsgGCMatchDetailsRequest::new();
        request.set_match_id(match_id);
        // Match details responses carry the Steam job target id.  Holding the
        // GC mutex for this request/response pair both uses that correlation
        // and serializes callers, so an archive lookup cannot consume the
        // response intended for the currently hosted match.
        let response = timeout(self.command_timeout, async {
            let gc = self.gc.lock().await;
            gc.job::<CMsgGCMatchDetailsRequest, CMsgGCMatchDetailsResponse>(request)
                .await
                .map_err(DotaSteamError::from)
        })
        .await
        .map_err(|_| DotaSteamError::Timeout)??;

        match_response_details(&response, match_id)?;
        Ok(response)
    }

    async fn send<Msg>(&self, message: Msg) -> Result<(), DotaSteamError>
    where
        Msg: NetMessage,
    {
        self.gc.lock().await.send(message).await.map_err(Into::into)
    }

    async fn require_lobby(&self, lobby_id: u64) -> Result<LobbySnapshot, DotaSteamError> {
        let snapshot = self.snapshot().await?.ok_or(DotaSteamError::NoLobby)?;
        if snapshot.lobby_id != lobby_id {
            return Err(DotaSteamError::WrongLobby {
                expected: lobby_id,
                found: Some(snapshot.lobby_id),
            });
        }
        Ok(snapshot)
    }

    async fn require_host(&self, lobby_id: u64) -> Result<LobbySnapshot, DotaSteamError> {
        let snapshot = self.require_lobby(lobby_id).await?;
        if snapshot.leader_steam_id != self.own_steam_id {
            return Err(DotaSteamError::NotHost);
        }
        Ok(snapshot)
    }

    async fn wait_for_initial_cache(&self) -> Result<(), DotaSteamError> {
        if self.state.read().await.status == CacheStatus::Ready {
            return Ok(());
        }
        let mut events = self.subscribe();
        timeout(INITIAL_CACHE_TIMEOUT, async {
            loop {
                if self.state.read().await.status == CacheStatus::Ready {
                    return Ok(());
                }
                match events.recv().await {
                    Ok(DotaSteamEvent::CacheHydrated)
                    | Ok(DotaSteamEvent::LobbyUpdated(_))
                    | Ok(DotaSteamEvent::LobbyCleared { .. }) => {}
                    Ok(DotaSteamEvent::TransportDisconnected { .. }) => {
                        return Err(DotaSteamError::NotReady);
                    }
                    Ok(DotaSteamEvent::LiveScoreboard(_)) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(DotaSteamError::EventSubscriptionClosed);
                    }
                }
            }
        })
        .await
        .map_err(|_| DotaSteamError::Timeout)??;
        Ok(())
    }

    async fn wait_for_lobby<F>(
        &self,
        wait_timeout: Duration,
        predicate: F,
    ) -> Result<LobbySnapshot, DotaSteamError>
    where
        F: Fn(&LobbySnapshot) -> bool,
    {
        if let Some(snapshot) = self.snapshot().await?
            && predicate(&snapshot)
        {
            return Ok(snapshot);
        }
        let mut events = self.subscribe();
        timeout(wait_timeout, async {
            loop {
                match events.recv().await {
                    Ok(DotaSteamEvent::LobbyUpdated(snapshot)) if predicate(&snapshot) => {
                        return Ok(snapshot);
                    }
                    Ok(DotaSteamEvent::TransportDisconnected { .. }) => {
                        return Err(DotaSteamError::NotReady);
                    }
                    Ok(DotaSteamEvent::LobbyCleared { .. })
                    | Ok(DotaSteamEvent::CacheHydrated)
                    | Ok(DotaSteamEvent::LiveScoreboard(_)) => {}
                    Ok(DotaSteamEvent::LobbyUpdated(_)) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(DotaSteamError::EventSubscriptionClosed);
                    }
                }
            }
        })
        .await
        .map_err(|_| DotaSteamError::Timeout)?
    }
}

fn build_lobby_details(
    config: &LobbyConfig,
    lobby_id: Option<u64>,
) -> Result<CMsgPracticeLobbySetDetails, DotaSteamError> {
    config.validate()?;
    let cm_pick = DOTA_CM_PICK::from_i32(config.cm_pick).ok_or(DotaSteamError::InvalidConfig {
        field: "cm_pick",
        value: config.cm_pick,
    })?;
    let visibility =
        DOTALobbyVisibility::from_i32(config.visibility).ok_or(DotaSteamError::InvalidConfig {
            field: "visibility",
            value: config.visibility,
        })?;
    let mut details = CMsgPracticeLobbySetDetails::new();
    if let Some(lobby_id) = lobby_id {
        details.set_lobby_id(lobby_id);
    }
    details.set_game_name(config.game_name.clone());
    details.set_server_region(config.server_region);
    details.set_game_mode(config.game_mode);
    details.set_cm_pick(cm_pick);
    details.set_allow_cheats(config.allow_cheats);
    details.set_fill_with_bots(config.fill_with_bots);
    details.set_allow_spectating(config.allow_spectating);
    // The generated setter accepts only the old enum variants and would
    // reject current wire value 4.  Assign EnumOrUnknown directly so every
    // validated current value is encoded without remapping.
    details.dota_tv_delay = Some(EnumOrUnknown::from_i32(config.dota_tv_delay));
    details.set_visibility(visibility);
    if config.league_id != 0 {
        details.set_leagueid(config.league_id);
    }
    if let Some(pass_key) = config.pass_key.clone() {
        details.set_pass_key(pass_key);
    }
    Ok(details)
}

fn lobby_config_matches(config: &LobbyConfig, snapshot: &LobbySnapshot) -> bool {
    snapshot.game_name == config.game_name
        && snapshot.server_region == config.server_region
        && snapshot.game_mode == config.game_mode
        && snapshot.cm_pick == config.cm_pick
        && snapshot.allow_cheats == config.allow_cheats
        && snapshot.fill_with_bots == config.fill_with_bots
        && snapshot.allow_spectating == config.allow_spectating
        && snapshot.dota_tv_delay == config.dota_tv_delay
        && snapshot.visibility == config.visibility
        && (config.league_id == 0 || snapshot.league_id == Some(config.league_id))
}

async fn spawn_watchers(
    gc: &Arc<Mutex<GameCoordinator>>,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
    watcher_start: oneshot::Receiver<()>,
) -> WatcherTask {
    let gc = Arc::clone(gc);
    let state = Arc::clone(state);
    let events = events.clone();
    // Constructing these receivers is the synchronization point that
    // prevents a post-welcome SOCache update from being lost between the
    // handshake and watcher task startup.
    let (
        mut subscribed,
        mut up_to_date,
        mut multiple,
        mut single_create,
        mut single_update,
        mut single_destroy,
        mut unsubscribed,
        mut scoreboard,
    ) = {
        let gc = gc.lock().await;
        (
            gc.on::<CMsgSOCacheSubscribed>(),
            gc.on::<CMsgSOCacheSubscribedUpToDate>(),
            gc.on::<SOMultipleObjectsWire>(),
            gc.on::<SOSingleObjectCreateWire>(),
            gc.on::<SOSingleObjectUpdateWire>(),
            gc.on::<SOSingleObjectDestroyWire>(),
            gc.on::<CMsgSOCacheUnsubscribed>(),
            gc.on::<LiveScoreboardWire>(),
        )
    };
    let task = tokio::spawn(async move {
        // The initial CMsgClientWelcome snapshot is applied by the caller.
        // Keep all stream handlers paused until that snapshot is authoritative.
        if watcher_start.await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                message = subscribed.next() => {
                    match message {
                        Some(Ok(message)) => apply_subscribed(message, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = up_to_date.next() => {
                    match message {
                        Some(Ok(_message)) => apply_cache_up_to_date(&state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = multiple.next() => {
                    match message {
                        Some(Ok(message)) => apply_multiple(message, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = single_create.next() => {
                    match message {
                        Some(Ok(message)) => apply_single(message.0, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = single_update.next() => {
                    match message {
                        Some(Ok(message)) => apply_single(message.0, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = single_destroy.next() => {
                    match message {
                        Some(Ok(message)) => apply_single(message.0, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = unsubscribed.next() => {
                    match message {
                        Some(Ok(message)) => apply_unsubscribed(message, &state, &events).await,
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
                message = scoreboard.next() => {
                    match message {
                        Some(Ok(message)) => {
                            let scoreboard = map_live_scoreboard(&message.0);
                            let _ = events.send(DotaSteamEvent::LiveScoreboard(scoreboard));
                        }
                        Some(Err(error)) => emit_disconnect(&state, &events, error),
                        None => break,
                    }
                }
            }
        }
    });
    WatcherTask(task)
}

fn emit_disconnect(
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
    error: NetworkError,
) {
    let reason = format!("{error}");
    let state = Arc::clone(state);
    let events = events.clone();
    tokio::spawn(async move {
        state.write().await.status = CacheStatus::Disconnected;
        let _ = events.send(DotaSteamEvent::TransportDisconnected { reason });
    });
}

async fn apply_subscribed(
    message: CMsgSOCacheSubscribed,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    let mut lobby = None;
    let mut contains_lobby_type = false;
    for object_type in &message.objects {
        if object_type.type_id() != SO_TYPE_LOBBY {
            continue;
        }
        contains_lobby_type = true;
        for data in &object_type.object_data {
            match CSODOTALobby::parse_from_bytes(data) {
                Ok(value) => lobby = Some(map_lobby(&value)),
                Err(error) => warn!(error = %error, "could not decode initial Dota lobby object"),
            }
        }
    }

    let previous = {
        let mut state = state.write().await;
        let previous = state.lobby.as_ref().map(|lobby| lobby.lobby_id);
        state.status = CacheStatus::Ready;
        if contains_lobby_type {
            state.lobby = lobby.clone();
            state.lobby_observed_at = lobby.as_ref().map(|_| Instant::now());
        }
        previous
    };
    let _ = events.send(DotaSteamEvent::CacheHydrated);
    if contains_lobby_type {
        match lobby {
            Some(lobby) => {
                let _ = events.send(DotaSteamEvent::LobbyUpdated(lobby));
            }
            None if previous.is_some() => {
                let _ = events.send(DotaSteamEvent::LobbyCleared { lobby_id: previous });
            }
            None => {}
        }
    }
}

async fn apply_welcome(
    welcome: CMsgClientWelcome,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    for cache in welcome.outofdate_subscribed_caches {
        apply_subscribed(cache, state, events).await;
    }
    // A cache with a matching version is represented by a subscription check
    // in the welcome.  We do not keep a local cache between runs, so the
    // check still establishes that the authoritative empty snapshot is ready;
    // an out-of-date cache processed above takes precedence for lobby contents.
    if !welcome.uptodate_subscribed_caches.is_empty() {
        apply_cache_up_to_date(state, events).await;
    }
}

async fn apply_cache_up_to_date(
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    let mut state = state.write().await;
    if state.status == CacheStatus::Hydrating {
        state.status = CacheStatus::Ready;
        drop(state);
        let _ = events.send(DotaSteamEvent::CacheHydrated);
    }
}

async fn apply_multiple(
    message: SOMultipleObjectsWire,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    for object in message
        .0
        .objects_added
        .iter()
        .chain(message.0.objects_modified.iter())
    {
        if object.type_id() != SO_TYPE_LOBBY {
            continue;
        }
        match CSODOTALobby::parse_from_bytes(object.object_data()) {
            Ok(value) => apply_lobby(map_lobby(&value), state, events).await,
            Err(error) => warn!(error = %error, "could not decode Dota lobby update"),
        }
    }
    for object in &message.0.objects_removed {
        if object.type_id() == SO_TYPE_LOBBY {
            clear_lobby(state, events).await;
        }
    }
}

async fn apply_single(
    message: CMsgSOSingleObject,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    if message.type_id() != SO_TYPE_LOBBY {
        return;
    }
    if message.has_object_data() && !message.object_data().is_empty() {
        match CSODOTALobby::parse_from_bytes(message.object_data()) {
            Ok(value) => apply_lobby(map_lobby(&value), state, events).await,
            Err(error) => warn!(error = %error, "could not decode Dota single-object update"),
        }
    } else {
        clear_lobby(state, events).await;
    }
}

async fn apply_unsubscribed(
    message: CMsgSOCacheUnsubscribed,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    let Some(owner) = message.owner_soid.as_ref() else {
        return;
    };
    let (previous, account_unsubscribed) = {
        let mut state = state.write().await;
        let account_unsubscribed =
            owner.type_ == Some(SO_OWNER_ACCOUNT) && owner.id == Some(state.own_steam_id);
        let lobby_unsubscribed = owner.type_ == Some(SO_OWNER_LOBBY)
            && owner.id.is_some()
            && owner.id == state.lobby.as_ref().map(|lobby| lobby.lobby_id);
        if !account_unsubscribed && !lobby_unsubscribed {
            return;
        }
        // Leaving or destroying a lobby unsubscribes its separate cache;
        // the authenticated account cache and GC session remain available.
        if account_unsubscribed {
            state.status = CacheStatus::Disconnected;
        }
        state.lobby_observed_at = None;
        (
            state.lobby.take().map(|lobby| lobby.lobby_id),
            account_unsubscribed,
        )
    };
    if previous.is_some() {
        let _ = events.send(DotaSteamEvent::LobbyCleared { lobby_id: previous });
    }
    if account_unsubscribed {
        let _ = events.send(DotaSteamEvent::TransportDisconnected {
            reason: "Dota account SOCache unsubscribed".to_owned(),
        });
    }
}

async fn apply_lobby(
    lobby: LobbySnapshot,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    {
        let mut state = state.write().await;
        state.status = CacheStatus::Ready;
        state.lobby = Some(lobby.clone());
        state.lobby_observed_at = Some(Instant::now());
    }
    let _ = events.send(DotaSteamEvent::LobbyUpdated(lobby));
}

async fn clear_lobby(state: &Arc<RwLock<ClientState>>, events: &broadcast::Sender<DotaSteamEvent>) {
    let previous = {
        let mut state = state.write().await;
        state.status = CacheStatus::Ready;
        state.lobby_observed_at = None;
        state.lobby.take().map(|lobby| lobby.lobby_id)
    };
    if previous.is_some() {
        let _ = events.send(DotaSteamEvent::LobbyCleared { lobby_id: previous });
    }
}

fn map_lobby(lobby: &CSODOTALobby) -> LobbySnapshot {
    LobbySnapshot {
        lobby_id: lobby.lobby_id(),
        leader_steam_id: lobby.leader_id(),
        server_steam_id: nonzero(lobby.server_id()),
        match_id: nonzero(lobby.match_id()),
        game_name: lobby.game_name().to_owned(),
        league_id: nonzero(lobby.leagueid()),
        server_region: lobby.server_region(),
        game_mode: lobby.game_mode(),
        cm_pick: lobby.cm_pick().value(),
        allow_cheats: lobby.allow_cheats(),
        fill_with_bots: lobby.fill_with_bots(),
        allow_spectating: lobby.allow_spectating(),
        // Use the raw wrapper instead of the generated getter.  The latter
        // maps the current wire value 4 (900 seconds) to its stale default.
        dota_tv_delay: lobby
            .dota_tv_delay
            .as_ref()
            .map_or(DOTA_TV_DELAY_10_SECONDS, |value| value.value()),
        visibility: lobby.visibility().value(),
        state: LobbyState::from_raw(lobby.state().value()),
        game_state: lobby.game_state.as_ref().map(|value| value.value()),
        outcome: LobbyOutcome::from_raw(lobby.match_outcome().value()),
        game_start_time: nonzero(lobby.game_start_time()),
        match_duration: nonzero(lobby.match_duration()),
        connect: (!lobby.connect().is_empty()).then(|| lobby.connect().to_owned()),
        pending_invites: lobby.pending_invites.clone(),
        members: lobby.all_members.iter().map(map_lobby_member).collect(),
    }
}

fn map_lobby_member(member: &CSODOTALobbyMember) -> LobbyMember {
    LobbyMember {
        steam_id: member.id(),
        account_id: member.id() as u32,
        hero_id: member.hero_id(),
        team: member.team().value().into(),
        name: member.name().to_owned(),
        slot: member.slot(),
        party_id: nonzero(member.party_id()),
        coach_team: member
            .has_coach_team()
            .then(|| member.coach_team().value().into()),
    }
}

fn nonzero<T>(value: T) -> Option<T>
where
    T: Copy + Default + PartialEq,
{
    (value != T::default()).then_some(value)
}

impl From<i32> for Team {
    fn from(value: i32) -> Self {
        Team::from_raw(value)
    }
}

fn map_live_scoreboard(message: &CMsgDOTALiveScoreboardUpdate) -> LiveScoreboard {
    LiveScoreboard {
        tournament_id: message.tournament_id(),
        tournament_game_id: message.tournament_game_id(),
        duration: message.duration(),
        hltv_delay: message.hltv_delay(),
        roshan_respawn_timer: message.roshan_respawn_timer(),
        league_id: message.league_id,
        match_id: message.match_id,
        radiant: message
            .team_good
            .as_ref()
            .map(map_live_team)
            .unwrap_or_default(),
        dire: message
            .team_bad
            .as_ref()
            .map(map_live_team)
            .unwrap_or_default(),
    }
}

fn map_live_team(team: &cmsg_dotalive_scoreboard_update::Team) -> LiveTeam {
    LiveTeam {
        score: team.score(),
        tower_state: team.tower_state(),
        barracks_state: team.barracks_state(),
        hero_picks: team.hero_picks.clone(),
        hero_bans: team.hero_bans.clone(),
        players: team.players.iter().map(map_live_player).collect(),
    }
}

fn map_live_player(player: &cmsg_dotalive_scoreboard_update::team::Player) -> LivePlayer {
    LivePlayer {
        player_slot: player.player_slot(),
        player_name: player.player_name().to_owned(),
        hero_name: player.hero_name().to_owned(),
        hero_id: player.hero_id(),
        kills: player.kills(),
        deaths: player.deaths(),
        assists: player.assists(),
        last_hits: player.last_hits(),
        denies: player.denies(),
        gold: player.gold(),
        level: player.level(),
        gold_per_min: player.gold_per_min(),
        xp_per_min: player.xp_per_min(),
        account_id: player.account_id(),
        net_worth: player.net_worth(),
    }
}

fn map_match_details(match_: &CMsgDOTAMatch, gc_result: u32) -> MatchDetails {
    let players: Vec<MatchPlayer> = match_
        .players
        .iter()
        .map(|player| MatchPlayer {
            account_id: player.account_id(),
            player_slot: player.player_slot(),
            team: player
                .team_number
                .map(|team| Team::from_raw(team.value()))
                .unwrap_or(Team::Unknown(-1)),
            team_slot: player.team_slot,
            hero_id: player.hero_id(),
            player_name: player.player_name().to_owned(),
            kills: player.kills(),
            deaths: player.deaths(),
            assists: player.assists(),
            gold: player.gold(),
            last_hits: player.last_hits(),
            denies: player.denies(),
            gold_per_min: player.gold_per_min(),
            xp_per_min: player.xp_per_min(),
            hero_damage: player.hero_damage(),
            tower_damage: player.tower_damage(),
            hero_healing: player.hero_healing(),
            level: player.level(),
            net_worth: player.net_worth(),
        })
        .collect();

    let teams = vec![
        MatchTeam {
            team: Team::Radiant,
            team_id: nonzero(match_.radiant_team_id()),
            name: match_.radiant_team_name().to_owned(),
            score: match_.radiant_team_score,
        },
        MatchTeam {
            team: Team::Dire,
            team_id: nonzero(match_.dire_team_id()),
            name: match_.dire_team_name().to_owned(),
            score: match_.dire_team_score,
        },
    ];

    MatchDetails {
        match_id: match_.match_id(),
        duration: match_.duration(),
        start_time: match_.starttime(),
        first_blood_time: match_.first_blood_time,
        cluster: nonzero(match_.cluster()),
        league_id: nonzero(match_.leagueid()),
        game_mode: match_.game_mode.map(|mode| mode.value()),
        outcome: match_
            .match_outcome
            .map(|outcome| LobbyOutcome::from_raw(outcome.value()))
            .unwrap_or(LobbyOutcome::Unknown),
        radiant_team_id: nonzero(match_.radiant_team_id()),
        dire_team_id: nonzero(match_.dire_team_id()),
        radiant_team_name: match_.radiant_team_name().to_owned(),
        dire_team_name: match_.dire_team_name().to_owned(),
        radiant_score: match_.radiant_team_score,
        dire_score: match_.dire_team_score,
        replay: match_.replay_state.as_ref().map(|state| MatchReplay {
            state: state.value(),
            salt: nonzero(match_.replay_salt()),
            server_ip: nonzero(match_.server_ip()),
            server_port: nonzero(match_.server_port()),
        }),
        players,
        teams,
        gc_result,
        postgame_statistics: map_postgame_statistics(match_),
    }
}

fn insert_stat<T: Into<serde_json::Value>>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        object.insert(name.to_owned(), value.into());
    }
}

fn map_postgame_statistics(match_: &CMsgDOTAMatch) -> serde_json::Value {
    use serde_json::{Map, Value};

    let mut stats = Map::new();
    insert_stat(&mut stats, "match_id", match_.match_id);
    for (name, value) in [
        ("duration", match_.duration),
        ("start_time", match_.starttime),
        ("first_blood_time", match_.first_blood_time),
        ("radiant_score", match_.radiant_team_score),
        ("dire_score", match_.dire_team_score),
        ("leagueid", match_.leagueid),
    ] {
        insert_stat(&mut stats, name, value);
    }
    insert_stat(&mut stats, "game_mode", match_.game_mode.map(|v| v.value()));
    insert_stat(
        &mut stats,
        "radiant_win",
        match_.match_outcome.and_then(|v| match v.value() {
            2 => Some(true),
            3 => Some(false),
            _ => None,
        }),
    );
    if !match_.players.is_empty() {
        let players = match_
            .players
            .iter()
            .map(|player| {
                let mut stats = Map::new();
                // Modern GC responses identify the side independently of player_slot.
                let slot = player
                    .team_number
                    .zip(player.team_slot)
                    .and_then(|(team, slot)| match (team.value(), slot) {
                        (0, 0..=4) => Some(slot),
                        (1, 0..=4) => Some(128 + slot),
                        _ => None,
                    })
                    .or_else(|| {
                        player.player_slot.filter(|slot| {
                            matches!(slot, 0..=4 | 128..=132)
                                && match player.team_number {
                                    Some(team) => match team.value() {
                                        0 => *slot < 128,
                                        1 => *slot >= 128,
                                        _ => false,
                                    },
                                    None => true,
                                }
                        })
                    });
                for (name, value) in [
                    ("account_id", player.account_id),
                    ("player_slot", slot),
                    ("kills", player.kills),
                    ("deaths", player.deaths),
                    ("assists", player.assists),
                    ("gold", player.gold),
                    ("last_hits", player.last_hits),
                    ("denies", player.denies),
                    ("gold_per_min", player.gold_per_min),
                    ("xp_per_min", player.xp_per_min),
                    ("hero_damage", player.hero_damage),
                    ("tower_damage", player.tower_damage),
                    ("hero_healing", player.hero_healing),
                    ("level", player.level),
                    ("net_worth", player.net_worth),
                ] {
                    insert_stat(&mut stats, name, value);
                }
                insert_stat(&mut stats, "hero_id", player.hero_id);
                for (index, value) in [
                    player.item_0,
                    player.item_1,
                    player.item_2,
                    player.item_3,
                    player.item_4,
                    player.item_5,
                    player.item_6,
                    player.item_7,
                    player.item_8,
                    player.item_9,
                ]
                .into_iter()
                .enumerate()
                {
                    insert_stat(&mut stats, &format!("item_{index}"), value);
                }
                if !player.ability_upgrades.is_empty() {
                    let upgrades = player
                        .ability_upgrades
                        .iter()
                        .map(|upgrade| {
                            let mut entry = Map::new();
                            insert_stat(&mut entry, "ability", upgrade.ability);
                            insert_stat(&mut entry, "time", upgrade.time);
                            Value::Object(entry)
                        })
                        .collect();
                    stats.insert("ability_upgrades".to_owned(), Value::Array(upgrades));
                }
                Value::Object(stats)
            })
            .collect();
        stats.insert("players".to_owned(), Value::Array(players));
    }
    if !match_.picks_bans.is_empty() {
        let draft = match_
            .picks_bans
            .iter()
            .enumerate()
            .map(|(order, event)| {
                let mut entry = Map::new();
                insert_stat(&mut entry, "is_pick", event.is_pick);
                insert_stat(&mut entry, "hero_id", event.hero_id);
                // Only the standard two-side encoding is compatible with OpenDota.
                insert_stat(&mut entry, "team", event.team.filter(|team| *team <= 1));
                entry.insert("order".to_owned(), Value::from(order));
                Value::Object(entry)
            })
            .collect();
        stats.insert("picks_bans".to_owned(), Value::Array(draft));
    }
    Value::Object(stats)
}

fn match_response_details(
    response: &CMsgGCMatchDetailsResponse,
    match_id: u64,
) -> Result<&CMsgDOTAMatch, DotaSteamError> {
    if response.result() != ERESULT_OK {
        return Err(DotaSteamError::MatchRequestFailed(response.result()));
    }
    let details = response
        .match_
        .as_ref()
        .ok_or(DotaSteamError::MatchNotFound(match_id))?;
    if details.match_id() != 0 && details.match_id() != match_id {
        return Err(DotaSteamError::MatchNotFound(match_id));
    }
    Ok(details)
}

macro_rules! proto_wire {
    ($name:ident, $message:ty, $kind_type:ty, $kind:path) => {
        #[derive(Debug)]
        struct $name($message);

        impl EncodableMessage for $name {
            fn read_body(
                data: bytes::BytesMut,
                _header: &NetMessageHeader,
            ) -> Result<Self, MalformedBody> {
                <$message>::parse_from_bytes(&data)
                    .map(Self)
                    .map_err(|error| MalformedBody::new(Self::KIND, error))
            }

            fn write_body<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
                self.0
                    .write_to_writer(&mut writer)
                    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
            }

            fn encode_size(&self) -> usize {
                self.0.compute_size() as usize
            }
        }

        impl NetMessage for $name {
            type KindEnum = $kind_type;
            const KIND: Self::KindEnum = $kind;
            const IS_PROTOBUF: bool = true;
        }
    };
}

// The generated SOCache update objects are RpcMessage-only because the same
// protobuf shape is sent with three different ESOMsg kinds.  Keep one typed
// adapter for each kind so filters remain exact and lossless.
proto_wire!(
    SOMultipleObjectsWire,
    steam_vent_proto_dota2::gcsdk_gcmessages::CMsgSOMultipleObjects,
    ESOMsg,
    ESOMsg::k_ESOMsg_UpdateMultiple
);
proto_wire!(
    SOSingleObjectCreateWire,
    steam_vent_proto_dota2::gcsdk_gcmessages::CMsgSOSingleObject,
    ESOMsg,
    ESOMsg::k_ESOMsg_Create
);
proto_wire!(
    SOSingleObjectUpdateWire,
    steam_vent_proto_dota2::gcsdk_gcmessages::CMsgSOSingleObject,
    ESOMsg,
    ESOMsg::k_ESOMsg_Update
);
proto_wire!(
    SOSingleObjectDestroyWire,
    steam_vent_proto_dota2::gcsdk_gcmessages::CMsgSOSingleObject,
    ESOMsg,
    ESOMsg::k_ESOMsg_Destroy
);

proto_wire!(
    LiveScoreboardWire,
    CMsgDOTALiveScoreboardUpdate,
    EDOTAGCMsg,
    EDOTAGCMsg::k_EMsgGCLiveScoreboardUpdate
);

proto_wire!(
    DestroyLobbyWire,
    CMsgDOTADestroyLobbyRequest,
    EDOTAGCMsg,
    EDOTAGCMsg::k_EMsgDestroyLobbyRequest
);

impl Default for DestroyLobbyWire {
    fn default() -> Self {
        Self(CMsgDOTADestroyLobbyRequest::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_vent_proto_dota2::dota_gcmessages_common_lobby::csodotalobby;
    use steam_vent_proto_dota2::gcsdk_gcmessages::CMsgSOIDOwner;

    const TEST_STEAM_ID: u64 = 76561197960287930;

    fn ready_lobby_state() -> Arc<RwLock<ClientState>> {
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        Arc::new(RwLock::new(ClientState {
            own_steam_id: TEST_STEAM_ID,
            status: CacheStatus::Ready,
            lobby: Some(map_lobby(&lobby)),
            lobby_observed_at: Some(Instant::now()),
        }))
    }

    #[tokio::test]
    async fn cached_reads_and_unrelated_updates_do_not_renew_lobby_observation() {
        let state = ready_lobby_state();
        let max_age = Duration::from_secs(45);
        let old = Instant::now() - Duration::from_secs(60);
        state.write().await.lobby_observed_at = Some(old);
        assert!(!state.read().await.lobby_observation_is_fresh(max_age));
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        apply_subscribed(CMsgSOCacheSubscribed::new(), &state, &events).await;
        assert_eq!(state.read().await.lobby_observed_at, Some(old));
        assert!(!state.read().await.lobby_observation_is_fresh(max_age));
        let lobby = state.read().await.lobby.clone().unwrap();
        apply_lobby(lobby, &state, &events).await;
        assert!(state.read().await.lobby_observation_is_fresh(max_age));
        state.write().await.status = CacheStatus::Disconnected;
        assert!(!state.read().await.lobby_observation_is_fresh(max_age));
        clear_lobby(&state, &events).await;
        assert!(!state.read().await.lobby_observation_is_fresh(max_age));
    }

    fn unsubscribe(owner_type: u32, owner_id: u64) -> CMsgSOCacheUnsubscribed {
        let message = CMsgSOCacheUnsubscribed {
            owner_soid: MessageField::some(CMsgSOIDOwner {
                type_: Some(owner_type),
                id: Some(owner_id),
                ..Default::default()
            }),
            ..Default::default()
        };
        CMsgSOCacheUnsubscribed::parse_from_bytes(&message.write_to_bytes().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn lobby_unsubscribe_clears_lobby_without_disconnecting_account() {
        let state = ready_lobby_state();
        let (events, mut received) = broadcast::channel(EVENT_BUFFER);

        apply_unsubscribed(unsubscribe(3, 7001), &state, &events).await;

        assert_eq!(state.read().await.status, CacheStatus::Ready);
        assert!(state.read().await.lobby.is_none());
        assert!(matches!(
            received.try_recv(),
            Ok(DotaSteamEvent::LobbyCleared {
                lobby_id: Some(7001)
            })
        ));
        assert!(matches!(
            received.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        // A duplicate notification after cleanup must remain harmless.
        apply_unsubscribed(unsubscribe(3, 7001), &state, &events).await;
        assert_eq!(state.read().await.status, CacheStatus::Ready);
        assert!(matches!(
            received.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn account_unsubscribe_invalidates_ready_state_and_clears_lobby() {
        let state = ready_lobby_state();
        let (events, mut received) = broadcast::channel(EVENT_BUFFER);

        apply_unsubscribed(unsubscribe(1, TEST_STEAM_ID), &state, &events).await;

        assert_eq!(state.read().await.status, CacheStatus::Disconnected);
        assert!(state.read().await.lobby.is_none());
        assert!(matches!(
            received.try_recv(),
            Ok(DotaSteamEvent::LobbyCleared {
                lobby_id: Some(7001)
            })
        ));
        assert!(matches!(
            received.try_recv(),
            Ok(DotaSteamEvent::TransportDisconnected { .. })
        ));
    }

    #[tokio::test]
    async fn unrelated_or_missing_unsubscribe_owners_do_not_change_state() {
        let state = ready_lobby_state();
        let (events, mut received) = broadcast::channel(EVENT_BUFFER);
        for message in [
            unsubscribe(1, TEST_STEAM_ID + 1),
            unsubscribe(3, 7000), // Delayed notification for a previous lobby.
            unsubscribe(1, 7001), // Matching id with the wrong owner type.
            unsubscribe(3, TEST_STEAM_ID),
            unsubscribe(2, 7001),
            CMsgSOCacheUnsubscribed::new(),
            CMsgSOCacheUnsubscribed {
                owner_soid: MessageField::some(CMsgSOIDOwner::new()),
                ..Default::default()
            },
        ] {
            apply_unsubscribed(message, &state, &events).await;
            let state = state.read().await;
            assert_eq!(state.status, CacheStatus::Ready);
            assert_eq!(state.lobby.as_ref().unwrap().lobby_id, 7001);
        }
        assert!(matches!(
            received.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn lobby_unsubscribe_does_not_restore_a_disconnected_account() {
        let state = ready_lobby_state();
        state.write().await.status = CacheStatus::Disconnected;
        let (events, _) = broadcast::channel(EVENT_BUFFER);

        apply_unsubscribed(unsubscribe(3, 7001), &state, &events).await;

        assert_eq!(state.read().await.status, CacheStatus::Disconnected);
        assert!(state.read().await.lobby.is_none());
    }

    #[test]
    fn maps_lobby_members_and_configuration_without_losing_unknown_enums() {
        let member = CSODOTALobbyMember {
            id: Some(76561197960287930),
            hero_id: Some(18),
            team: Some(EnumOrUnknown::new(DOTA_GC_TEAM::DOTA_GC_TEAM_GOOD_GUYS)),
            name: Some("captain".to_owned()),
            slot: Some(2),
            party_id: Some(77),
            coach_team: Some(EnumOrUnknown::new(DOTA_GC_TEAM::DOTA_GC_TEAM_NOTEAM)),
            ..Default::default()
        };
        let lobby = CSODOTALobby {
            lobby_id: Some(55),
            leader_id: Some(76561197960287930),
            server_id: Some(99),
            game_mode: Some(2),
            state: Some(EnumOrUnknown::new(csodotalobby::State::RUN)),
            game_name: Some("league match".to_owned()),
            server_region: Some(3),
            cm_pick: Some(EnumOrUnknown::new(DOTA_CM_PICK::DOTA_CM_BAD_GUYS)),
            allow_cheats: Some(false),
            fill_with_bots: Some(false),
            allow_spectating: Some(true),
            dota_tv_delay: Some(EnumOrUnknown::from_i32(DOTA_TV_DELAY_120_SECONDS)),
            leagueid: Some(42),
            all_members: vec![member],
            ..Default::default()
        };
        let snapshot = map_lobby(&lobby);
        assert_eq!(snapshot.lobby_id, 55);
        assert_eq!(snapshot.league_id, Some(42));
        assert_eq!(snapshot.state, LobbyState::Run);
        assert_eq!(snapshot.game_state, None);
        assert_eq!(snapshot.cm_pick, 2);
        assert_eq!(snapshot.members[0].team, Team::Radiant);
        assert_eq!(snapshot.members[0].account_id, 22_202);
    }

    #[test]
    fn round_trips_optional_raw_dota_game_states_without_getter_defaults() {
        let cases = [
            (None, None),
            (Some(2), Some(2)),     // DOTA_GAMERULES_STATE_HERO_SELECTION
            (Some(3), Some(3)),     // DOTA_GAMERULES_STATE_STRATEGY_TIME
            (Some(4), Some(4)),     // DOTA_GAMERULES_STATE_PRE_GAME
            (Some(5), Some(5)),     // DOTA_GAMERULES_STATE_GAME_IN_PROGRESS
            (Some(6), Some(6)),     // DOTA_GAMERULES_STATE_POST_GAME
            (Some(127), Some(127)), // A future/unknown enum value
        ];

        for (wire_value, expected) in cases {
            let lobby = CSODOTALobby {
                game_state: wire_value.map(EnumOrUnknown::from_i32),
                ..Default::default()
            };
            let encoded = lobby.write_to_bytes().expect("lobby should encode");
            let decoded = CSODOTALobby::parse_from_bytes(&encoded).expect("lobby should decode");

            assert_eq!(
                decoded.game_state.as_ref().map(|value| value.value()),
                expected,
                "protobuf round trip must preserve game_state presence and raw value"
            );
            assert_eq!(
                map_lobby(&decoded).game_state,
                expected,
                "snapshot mapping must not apply the generated getter default"
            );
        }
    }

    #[test]
    fn rejects_invalid_wire_configuration_before_sending() {
        let config = LobbyConfig {
            cm_pick: 900,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(DotaSteamError::InvalidConfig {
                field: "cm_pick",
                ..
            })
        ));
    }

    #[test]
    fn round_trips_current_dota_tv_delay_wire_values_without_remapping() {
        for &wire_value in &VALID_DOTA_TV_DELAY_VALUES {
            let config = LobbyConfig {
                dota_tv_delay: wire_value,
                ..Default::default()
            };
            let details = build_lobby_details(&config, Some(1234))
                .expect("current Dota TV delay value should validate");
            assert_eq!(
                details.dota_tv_delay.as_ref().map(|value| value.value()),
                Some(wire_value)
            );

            let encoded_details = details
                .write_to_bytes()
                .expect("lobby details should encode");
            let decoded_details = CMsgPracticeLobbySetDetails::parse_from_bytes(&encoded_details)
                .expect("lobby details should decode");
            assert_eq!(
                decoded_details
                    .dota_tv_delay
                    .as_ref()
                    .map(|value| value.value()),
                Some(wire_value)
            );

            let lobby = CSODOTALobby {
                dota_tv_delay: decoded_details.dota_tv_delay,
                ..Default::default()
            };
            let encoded_lobby = lobby.write_to_bytes().expect("lobby should encode");
            let decoded_lobby =
                CSODOTALobby::parse_from_bytes(&encoded_lobby).expect("lobby should decode");
            assert_eq!(map_lobby(&decoded_lobby).dota_tv_delay, wire_value);
        }
    }

    #[test]
    fn rejects_invalid_dota_tv_delay_wire_values_before_sending() {
        for wire_value in [i32::MIN, -1, 5, i32::MAX] {
            let config = LobbyConfig {
                dota_tv_delay: wire_value,
                ..Default::default()
            };
            assert!(matches!(
                config.validate(),
                Err(DotaSteamError::InvalidConfig {
                    field: "dota_tv_delay",
                    value,
                }) if value == wire_value
            ));
        }
    }

    #[test]
    fn maps_live_scoreboard_fixture() {
        let mut player = cmsg_dotalive_scoreboard_update::team::Player::new();
        player.set_account_id(123);
        player.set_kills(4);
        player.set_player_name("carry".to_owned());
        let mut team = cmsg_dotalive_scoreboard_update::Team::new();
        team.set_score(18);
        team.players.push(player);
        let mut message = CMsgDOTALiveScoreboardUpdate::new();
        message.set_match_id(987);
        message.team_good = MessageField::some(team);
        let mapped = map_live_scoreboard(&message);
        assert_eq!(mapped.match_id, Some(987));
        assert_eq!(mapped.radiant.score, 18);
        assert_eq!(mapped.radiant.players[0].kills, 4);
    }

    #[tokio::test]
    async fn hydrates_lobby_from_welcome_socache_fixture() {
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        lobby.set_leader_id(76561197960287930);
        lobby.set_game_name("welcome lobby".to_owned());
        lobby.set_leagueid(42);

        let mut subscribed_type =
            steam_vent_proto_dota2::gcsdk_gcmessages::cmsg_socache_subscribed::SubscribedType::new(
            );
        subscribed_type.set_type_id(SO_TYPE_LOBBY);
        subscribed_type
            .object_data
            .push(lobby.write_to_bytes().expect("encode lobby fixture"));
        let mut cache = CMsgSOCacheSubscribed::new();
        cache.objects.push(subscribed_type);

        let mut welcome = CMsgClientWelcome::new();
        welcome.outofdate_subscribed_caches.push(cache);
        let state = Arc::new(RwLock::new(ClientState {
            own_steam_id: 76561197960287930,
            status: CacheStatus::Hydrating,
            lobby: None,
            lobby_observed_at: None,
        }));
        let (events, _events_rx) = broadcast::channel(EVENT_BUFFER);

        apply_welcome(welcome, &state, &events).await;

        let state = state.read().await;
        assert_eq!(state.status, CacheStatus::Ready);
        assert_eq!(state.lobby.as_ref().map(|lobby| lobby.lobby_id), Some(7001));
        assert_eq!(
            state.lobby.as_ref().and_then(|lobby| lobby.league_id),
            Some(42)
        );
    }

    #[test]
    fn accepts_steam_eresult_ok_for_match_details() {
        let mut match_ = CMsgDOTAMatch::new();
        match_.set_match_id(31415);
        let mut response = CMsgGCMatchDetailsResponse::new();
        response.set_result(ERESULT_OK);
        response.match_ = MessageField::some(match_);

        let details = match_response_details(&response, 31415).expect("successful response");
        assert_eq!(details.match_id(), 31415);
        assert_eq!(map_match_details(details, response.result()).gc_result, 1);
    }

    #[test]
    fn metadata_location_preserves_wire_presence_and_redacts_credentials() {
        let mut match_ = CMsgDOTAMatch::new();
        match_.set_match_id(31415);
        let missing = map_metadata_location(&match_);
        assert_eq!(missing.private_metadata_key(), None);
        assert_eq!(missing.replay_salt, None);
        match_.set_private_metadata_key(3141592653);
        match_.set_replay_salt(2718281828);
        match_.set_cluster(152);
        let decoded = CMsgDOTAMatch::parse_from_bytes(
            &match_.write_to_bytes().expect("encode metadata location"),
        )
        .expect("decode metadata location");
        let location = map_metadata_location(&decoded);
        assert_eq!(location.private_metadata_key(), Some(3141592653));
        assert_eq!(location.replay_salt, Some(2718281828));
        let debug = format!("{location:?}");
        assert!(!debug.contains("3141592653"));
        assert!(!debug.contains("2718281828"));
        assert!(debug.contains("redacted"));
        assert!(
            map_postgame_statistics(&decoded)
                .get("private_metadata_key")
                .is_none()
        );
    }

    fn wire_match_details(match_: &CMsgDOTAMatch) -> MatchDetails {
        let bytes = match_.write_to_bytes().expect("encode match details");
        let decoded = CMsgDOTAMatch::parse_from_bytes(&bytes).expect("decode match details");
        map_match_details(&decoded, ERESULT_OK)
    }

    #[test]
    fn postgame_wire_fields_distinguish_absent_from_zero() {
        use steam_vent_proto_dota2::dota_gcmessages_common::cmsg_dotamatch::Player;
        let mut match_ = CMsgDOTAMatch::new();
        assert_eq!(
            wire_match_details(&match_).postgame_statistics,
            serde_json::json!({})
        );
        match_.set_match_id(31415);
        match_.set_duration(0);
        match_.set_first_blood_time(0);
        match_.set_leagueid(0);
        match_.match_outcome = Some(EnumOrUnknown::from_i32(3));
        let mut player = Player::new();
        player.set_account_id(42);
        player.set_kills(0);
        player.set_item_0(0);
        match_.players.push(player);
        // Public statistics must never copy these values from the wire object.
        match_.set_private_metadata_key(1234);
        match_.set_replay_salt(5678);
        match_.set_server_ip(9012);
        let details = wire_match_details(&match_);
        assert_eq!(details.first_blood_time, Some(0));
        assert_eq!(
            details.postgame_statistics,
            serde_json::json!({
                "match_id": 31415, "duration": 0, "first_blood_time": 0,
                "leagueid": 0, "radiant_win": false,
                "players": [{"account_id": 42, "kills": 0, "item_0": 0}]
            })
        );
        match_.match_outcome = Some(EnumOrUnknown::from_i32(64));
        assert!(
            wire_match_details(&match_)
                .postgame_statistics
                .get("radiant_win")
                .is_none()
        );
    }

    #[test]
    fn postgame_wire_slots_normalize_dire_and_reject_contradictions() {
        use steam_vent_proto_dota2::dota_gcmessages_common::cmsg_dotamatch::Player;
        let cases = [
            (Some(1), Some(3), Some(3), Some(131)),
            (Some(0), Some(0), None, Some(0)),
            (Some(1), None, Some(129), Some(129)),
            (None, None, Some(132), Some(132)),
            (Some(1), None, Some(2), None),
            (Some(0), None, Some(129), None),
            (Some(4), None, Some(0), None),
            (None, None, Some(5), None),
            (None, None, None, None),
        ];
        for (team, slot, raw, expected) in cases {
            let mut match_ = CMsgDOTAMatch::new();
            let mut player = Player::new();
            player.team_number = team.map(EnumOrUnknown::from_i32);
            player.team_slot = slot;
            player.player_slot = raw;
            match_.players.push(player);
            let details = wire_match_details(&match_);
            assert_eq!(
                details.postgame_statistics["players"][0].get("player_slot"),
                expected.map(serde_json::Value::from).as_ref(),
                "{team:?}/{slot:?}/{raw:?}"
            );
        }
    }

    #[test]
    fn postgame_wire_draft_and_abilities_preserve_present_values() {
        use steam_vent_proto_dota2::dota_gcmessages_common::{
            CMatchHeroSelectEvent, CMatchPlayerAbilityUpgrade, cmsg_dotamatch::Player,
        };
        let mut match_ = CMsgDOTAMatch::new();
        let mut player = Player::new();
        let mut upgrade = CMatchPlayerAbilityUpgrade::new();
        upgrade.set_ability(5034);
        upgrade.set_time(0);
        player.ability_upgrades.push(upgrade);
        player.set_item_9(42);
        match_.players.push(player);
        let mut pick = CMatchHeroSelectEvent::new();
        pick.set_is_pick(false);
        pick.set_team(1);
        pick.set_hero_id(23);
        match_.picks_bans.push(pick);
        assert_eq!(
            wire_match_details(&match_).postgame_statistics,
            serde_json::json!({
                "players": [{"item_9": 42, "ability_upgrades": [{"ability":5034,"time":0}]}],
                "picks_bans": [{"is_pick":false,"team":1,"hero_id":23,"order":0}]
            })
        );
    }

    #[test]
    fn treats_match_details_default_result_as_failure() {
        assert!(matches!(
            match_response_details(&CMsgGCMatchDetailsResponse::new(), 31415),
            Err(DotaSteamError::MatchRequestFailed(0))
        ));
    }

    #[test]
    fn lobby_config_debug_redacts_password() {
        let config = LobbyConfig {
            pass_key: Some("secret-lobby-password".to_owned()),
            ..Default::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("secret-lobby-password"));
        assert!(rendered.contains("redacted"));
    }
}
