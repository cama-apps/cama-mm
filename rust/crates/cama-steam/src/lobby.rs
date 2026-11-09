//! Dota 2 Game Coordinator transport and typed lobby state.
//!
//! Dota lobby commands are deliberately treated as asynchronous commands.  The
//! GC does not acknowledge most of them with a useful response; the authoritative
//! result arrives through the SOCache.  The client therefore only reports a
//! lobby as ready after the initial SOCache subscription has been observed, and
//! exposes subsequent changes through both [`DotaSteamClient::snapshot`] and
//! [`DotaSteamClient::subscribe`].

use crate::{AuthenticatedSteam, SO_TYPE_LOBBY, SteamAuth, SteamAuthConfig};
use futures_util::{Stream, StreamExt};
use std::fmt::{Debug, Formatter};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use steam_vent::message::{EncodableMessage, MalformedBody};
use steam_vent::{
    ConnectionTrait, GameCoordinator, NetMessage, NetMessageHeader, NetworkError, RawNetMessage,
};
use steam_vent_proto_common::protobuf::{Enum, EnumOrUnknown, Message, MessageField};
use steam_vent_proto_dota2::base_gcmessages::CMsgInviteToLobby;
use steam_vent_proto_dota2::dota_gcmessages_client::{
    CMsgClientSuspended, CMsgDOTADestroyLobbyRequest, CMsgGCMatchDetailsRequest,
    CMsgGCMatchDetailsResponse,
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
    CMsgClientHello, CMsgClientWelcome, CMsgConnectionStatus, CMsgSOCacheSubscribed,
    CMsgSOCacheSubscribedUpToDate, CMsgSOCacheSubscriptionRefresh, CMsgSOCacheUnsubscribed,
    CMsgSOIDOwner, CMsgSOSingleObject, GCConnectionStatus,
};
use steam_vent_proto_dota2::gcsystemmsgs::{EGCBaseClientMsg, ESOMsg};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::warn;

const EVENT_BUFFER: usize = 128;
const INITIAL_CACHE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const CREATION_SESSION_MAX_AGE: Duration = Duration::from_secs(5);
/// A quiet lobby receives no SOCache pushes, so ask for a resend well before
/// callers' freshness windows lapse.
const LOBBY_REFRESH_AFTER: Duration = Duration::from_secs(15);
/// Minimum spacing between resend requests while the GC has not answered.
const LOBBY_REFRESH_RETRY: Duration = Duration::from_secs(5);
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
    /// Monotonic evidence retained across updates for this lobby identity.
    pub launch_history: LobbyLaunchHistory,
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

/// Launch milestones that must survive a brief allocation/loading/return cycle
/// between application polls. Never use this as the current lobby state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LobbyLaunchHistory {
    pub allocation_observed: bool,
    pub postgame_observed: bool,
    pub game_states: std::collections::BTreeSet<i32>,
    pub match_id: Option<u64>,
    pub conflicting_match_ids: bool,
}

impl LobbySnapshot {
    fn retain_launch_history(&mut self, previous: Option<&(u64, LobbyLaunchHistory)>) {
        self.launch_history = previous
            .filter(|(id, _)| *id == self.lobby_id)
            .map(|(_, history)| history.clone())
            .unwrap_or_default();
        let history = &mut self.launch_history;
        history.allocation_observed |= matches!(
            self.state,
            LobbyState::ServerSetup | LobbyState::ServerAssign | LobbyState::Run
        );
        history.postgame_observed |= self.state == LobbyState::PostGame
            || matches!(
                self.outcome,
                LobbyOutcome::RadiantVictory | LobbyOutcome::DireVictory
            );
        if let Some(game_state) = self.game_state {
            history.game_states.insert(game_state);
        }
        if let Some(match_id) = self.match_id {
            history.conflicting_match_ids |= history.match_id.is_some_and(|old| old != match_id);
            history.match_id.get_or_insert(match_id);
        }
    }
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
    // Keep milestones through an unsubscribe/resubscribe of the same lobby.
    // A different lobby ID replaces this, bounding retained history to one lobby.
    lobby_launch_history: Option<(u64, LobbyLaunchHistory)>,
    cache_versions: std::collections::BTreeMap<(u32, u64), u64>,
    lobby_observed_at: Option<Instant>,
    lobby_refresh_requested_at: Option<Instant>,
    welcome_generation: u64,
    welcome_observed_at: Option<tokio::time::Instant>,
    // Set when GC messages were dropped; the next hydrating Welcome is the
    // authoritative resync, including lobby absence.
    resync_pending: bool,
}

impl ClientState {
    fn accept_lobby_version(
        &mut self,
        owner: Option<&CMsgSOIDOwner>,
        lobby_id: Option<u64>,
        version: Option<u64>,
    ) -> bool {
        let Some(version) = version else { return true };
        let key = owner
            .and_then(|owner| owner.type_.zip(owner.id))
            .or_else(|| lobby_id.map(|id| (SO_OWNER_LOBBY, id)));
        let Some(key) = key else { return true };
        if self
            .cache_versions
            .get(&key)
            .is_some_and(|old| *old > version)
        {
            return false;
        }
        self.cache_versions.insert(key, version);
        true
    }

    fn observe_lobby(&mut self, lobby: &mut LobbySnapshot) {
        self.cache_versions
            .retain(|(kind, id), _| *kind != SO_OWNER_LOBBY || *id == lobby.lobby_id);
        lobby.retain_launch_history(self.lobby_launch_history.as_ref());
        log_lobby_transition(self.lobby.as_ref(), lobby);
        self.lobby_launch_history = Some((lobby.lobby_id, lobby.launch_history.clone()));
    }

    /// Cache traffic proves a hydrating session ready. A disconnected session
    /// stays disconnected: its loss was already announced, and a lobby update
    /// arriving afterwards must not make the client usable again.
    fn mark_ready(&mut self) {
        if self.status == CacheStatus::Hydrating {
            self.status = CacheStatus::Ready;
        }
    }

    /// The lobby whose cache should be resent, when the current observation is
    /// ageing. A disconnected or empty cache has nothing trustworthy to renew.
    fn lobby_refresh_target(&self) -> Option<u64> {
        if self.status != CacheStatus::Ready
            || self
                .lobby_observed_at
                .is_some_and(|at| at.elapsed() < LOBBY_REFRESH_AFTER)
            || self
                .lobby_refresh_requested_at
                .is_some_and(|at| at.elapsed() < LOBBY_REFRESH_RETRY)
        {
            return None;
        }
        self.lobby.as_ref().map(|lobby| lobby.lobby_id)
    }

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
    _pump: Arc<WatcherTask>,
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
        // One envelope subscription preserves ordering across GC message kinds.
        // Register before admission so updates adjacent to Welcome are retained,
        // and drain it immediately: steam-vent buffers only 16 messages per
        // kind and reports overflow as EOF, so an unread subscription across
        // the handshake would otherwise lose the Welcome or initial caches.
        let (messages, pump) =
            crate::gc_stream::buffer_gc_messages(auth.connection.on::<crate::handshake::FromGc>());
        let (gc, _welcome) = crate::handshake::connect(&auth.connection).await?;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let state = Arc::new(RwLock::new(ClientState {
            own_steam_id,
            status: CacheStatus::Hydrating,
            lobby: None,
            lobby_launch_history: None,
            cache_versions: Default::default(),
            welcome_generation: 0,
            welcome_observed_at: None,
            lobby_observed_at: None,
            lobby_refresh_requested_at: None,
            resync_pending: false,
        }));
        let gc = Arc::new(Mutex::new(gc));
        let resync_gc = Arc::downgrade(&gc);
        let watcher = spawn_watchers(messages, &state, &events, move || {
            // Hello makes the GC answer with a Welcome carrying every cache.
            // Send off the watcher so a held GC mutex cannot stall updates.
            let Some(gc) = resync_gc.upgrade() else {
                return;
            };
            tokio::spawn(async move {
                if let Err(error) = gc.lock().await.send(CMsgClientHello::new()).await {
                    warn!(%error, "Dota coordinator resync request failed");
                }
            });
        });
        let client = Self {
            _connection: auth.connection,
            gc,
            state,
            events,
            _watcher: Arc::new(watcher),
            _pump: Arc::new(WatcherTask(pump)),
            own_steam_id,
            own_account_id,
            command_timeout: INITIAL_CACHE_TIMEOUT,
        };

        // The same ordered stream applies Welcome and every subsequent update.
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

    /// Like [`Self::lobby_observation_is_fresh`], but also asks the GC to
    /// resend an ageing lobby cache. The GC pushes lobby objects only on
    /// change, so an idle lobby would otherwise look stale. The request is not
    /// awaited: callers act on the observation they already hold, and the
    /// resent lobby object renews it well before `max_age` lapses. An ignored
    /// or failed request leaves the observation to expire normally.
    pub async fn refresh_lobby_observation(&self, max_age: Duration) -> bool {
        let lobby_id = {
            let mut state = self.state.write().await;
            let Some(lobby_id) = state.lobby_refresh_target() else {
                return state.lobby_observation_is_fresh(max_age);
            };
            state.lobby_refresh_requested_at = Some(Instant::now());
            lobby_id
        };
        let request = CMsgSOCacheSubscriptionRefresh {
            owner_soid: MessageField::some(CMsgSOIDOwner {
                type_: Some(SO_OWNER_LOBBY),
                id: Some(lobby_id),
                ..Default::default()
            }),
            ..Default::default()
        };
        if let Err(error) = self.send(request).await {
            warn!(%error, lobby_id, "Dota lobby cache refresh failed");
        }
        self.lobby_observation_is_fresh(max_age).await
    }

    /// Return the current game-server SteamID used by server-side realtime
    /// stats APIs, if the lobby has already been assigned a server.
    pub async fn server_steam_id(&self) -> Result<Option<u64>, DotaSteamError> {
        Ok(self
            .snapshot()
            .await?
            .and_then(|snapshot| snapshot.server_steam_id))
    }

    /// Revalidate the GC session before persisting an intent to create a lobby.
    /// This sends only ClientHello: failure cannot have created a lobby.
    pub async fn prepare_lobby_creation(&self) -> Result<(), DotaSteamError> {
        let events = self.subscribe();
        let generation = {
            let state = self.state.read().await;
            // A fresh admission already proves readiness. This also lets a
            // restarted worker progress if an idle GC ignores repeated Hello.
            if creation_session_is_fresh(&state)? {
                return Ok(());
            }
            state.welcome_generation
        };
        timeout(self.command_timeout, async {
            self.send(CMsgClientHello::new()).await?;
            wait_for_creation_preflight(&self.state, events, generation).await
        })
        .await
        .map_err(|_| DotaSteamError::Timeout)?
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

        let request = build_create_request(config)?;
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

    /// Send a launch only while the exact authorized observation remains fresh.
    /// Hold the cache read lock through enqueueing so an update cannot replace
    /// the checked state between validation and the command send.
    pub async fn launch_lobby_checked(
        &self,
        lobby_id: u64,
        client_version: u32,
        max_age: Duration,
        predicate: impl FnOnce(&LobbySnapshot) -> bool + Send,
    ) -> Result<bool, DotaSteamError> {
        let gc = self.gc.lock().await;
        let state = self.state.read().await;
        if !authorized_lobby_action(&state, self.own_steam_id, lobby_id, max_age, predicate) {
            return Ok(false);
        }
        let mut request = CMsgPracticeLobbyLaunch::new();
        request.set_client_version(client_version);
        gc.send(request).await.map_err(DotaSteamError::from)?;
        drop(state);
        Ok(true)
    }

    /// Destroy only the fresh owned snapshot authorized by the caller.
    pub async fn destroy_lobby_checked(
        &self,
        lobby_id: u64,
        max_age: Duration,
        predicate: impl FnOnce(&LobbySnapshot) -> bool + Send,
    ) -> Result<bool, DotaSteamError> {
        let gc = self.gc.lock().await;
        let state = self.state.read().await;
        if !authorized_lobby_action(&state, self.own_steam_id, lobby_id, max_age, predicate) {
            return Ok(false);
        }
        gc.send(DestroyLobbyWire::default())
            .await
            .map_err(DotaSteamError::from)?;
        drop(state);
        Ok(true)
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
        // Register before checking readiness, including an already-disconnected
        // state whose notification may have preceded this wait.
        let events = self.subscribe();
        timeout(
            INITIAL_CACHE_TIMEOUT,
            wait_for_initial_cache_state(&self.state, events),
        )
        .await
        .map_err(|_| DotaSteamError::Timeout)?
    }

    async fn wait_for_lobby<F>(
        &self,
        wait_timeout: Duration,
        predicate: F,
    ) -> Result<LobbySnapshot, DotaSteamError>
    where
        F: Fn(&LobbySnapshot) -> bool,
    {
        // Subscribe before reading state so an update cannot fall between the
        // snapshot and registration. Re-read after every wake, including lag.
        let events = self.subscribe();
        timeout(
            wait_timeout,
            wait_for_state(&self.state, events, |state| {
                Ok(state
                    .lobby
                    .as_ref()
                    .filter(|lobby| predicate(lobby))
                    .cloned())
            }),
        )
        .await
        .map_err(|_| DotaSteamError::Timeout)?
    }
}

fn authorized_lobby_action(
    state: &ClientState,
    own_steam_id: u64,
    lobby_id: u64,
    max_age: Duration,
    predicate: impl FnOnce(&LobbySnapshot) -> bool,
) -> bool {
    state.lobby_observation_is_fresh(max_age)
        && state.lobby.as_ref().is_some_and(|lobby| {
            lobby.lobby_id == lobby_id && lobby.leader_steam_id == own_steam_id && predicate(lobby)
        })
}

fn creation_session_is_fresh(state: &ClientState) -> Result<bool, DotaSteamError> {
    if state.status != CacheStatus::Ready {
        return Err(DotaSteamError::NotReady);
    }
    if let Some(lobby) = &state.lobby {
        return Err(DotaSteamError::AlreadyInLobby(lobby.lobby_id));
    }
    Ok(state
        .welcome_observed_at
        .is_some_and(|at| at.elapsed() <= CREATION_SESSION_MAX_AGE))
}

async fn wait_for_initial_cache_state(
    state: &Arc<RwLock<ClientState>>,
    mut events: broadcast::Receiver<DotaSteamEvent>,
) -> Result<(), DotaSteamError> {
    loop {
        match state.read().await.status {
            CacheStatus::Ready => return Ok(()),
            CacheStatus::Disconnected => return Err(DotaSteamError::NotReady),
            CacheStatus::Hydrating => {}
        }
        match events.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => {
                return Err(DotaSteamError::EventSubscriptionClosed);
            }
        }
    }
}

async fn wait_for_creation_preflight(
    state: &Arc<RwLock<ClientState>>,
    events: broadcast::Receiver<DotaSteamEvent>,
    generation: u64,
) -> Result<(), DotaSteamError> {
    wait_for_state(state, events, |state| {
        if state.welcome_generation == generation {
            return Ok(None);
        }
        if let Some(lobby) = &state.lobby {
            return Err(DotaSteamError::AlreadyInLobby(lobby.lobby_id));
        }
        Ok(Some(()))
    })
    .await
}

async fn wait_for_state<T>(
    state: &Arc<RwLock<ClientState>>,
    mut events: broadcast::Receiver<DotaSteamEvent>,
    predicate: impl Fn(&ClientState) -> Result<Option<T>, DotaSteamError>,
) -> Result<T, DotaSteamError> {
    loop {
        {
            let state = state.read().await;
            if state.status != CacheStatus::Ready {
                return Err(DotaSteamError::NotReady);
            }
            if let Some(result) = predicate(&state)? {
                return Ok(result);
            }
        }
        match events.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => {
                return Err(DotaSteamError::EventSubscriptionClosed);
            }
        }
    }
}

fn build_create_request(config: &LobbyConfig) -> Result<CMsgPracticeLobbyCreate, DotaSteamError> {
    let details = build_lobby_details(config, None)?;
    let mut request = CMsgPracticeLobbyCreate::new();
    request.set_client_version(config.client_version);
    // Leave search_key unset. It is a server-search restriction, not the lobby
    // display name; using our name here leaves launch stuck in SERVERSETUP.
    // The visible name belongs exclusively in lobby_details.game_name.
    if let Some(pass_key) = config.pass_key.clone() {
        request.set_pass_key(pass_key);
    }
    request.lobby_details = MessageField::some(details);
    Ok(request)
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

type GcMessage = Result<crate::handshake::FromGc, NetworkError>;

fn spawn_watchers(
    messages: impl Stream<Item = GcMessage> + Send + 'static,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
    request_resync: impl Fn() + Send + 'static,
) -> WatcherTask {
    let state = Arc::clone(state);
    let events = events.clone();
    let task = tokio::spawn(async move {
        futures_util::pin_mut!(messages);
        let reason = loop {
            let message = match messages.next().await {
                Some(Ok(message)) => message.0,
                // steam-vent maps a lagged broadcast to EOF; a closed one ends
                // the stream. Lost messages make the cache unverified, not the
                // session dead: resync instead of reconnecting mid-lobby.
                Some(Err(NetworkError::EOF)) => {
                    begin_resync(&state).await;
                    request_resync();
                    continue;
                }
                Some(Err(error)) => break error.to_string(),
                None => break "Dota coordinator message stream closed".to_owned(),
            };
            if message.appid() != crate::DOTA_APP_ID {
                continue;
            }
            let expected_kind = message.msgtype() & 0x7fff_ffff;
            let expected_protobuf = message.msgtype() & 0x8000_0000 != 0;
            let raw = match RawNetMessage::read(message.payload().to_vec()) {
                Ok(raw)
                    if raw.kind.0 as u32 == expected_kind
                        && raw.is_protobuf == expected_protobuf =>
                {
                    raw
                }
                Ok(_) => break NetworkError::InvalidHeader.to_string(),
                Err(error) => break error.to_string(),
            };
            if let Err(reason) = apply_gc_message(raw, &state, &events).await {
                break reason;
            }
        };
        emit_disconnect(&state, &events, reason).await;
    });
    WatcherTask(task)
}

async fn apply_gc_message(
    raw: RawNetMessage,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) -> Result<(), String> {
    macro_rules! decode {
        ($ty:ty) => {
            raw.into_message::<$ty>()
                .map_err(|error| error.to_string())?
        };
    }
    let kind = raw.kind;
    let valid = if kind == CMsgClientWelcome::KIND {
        apply_welcome(decode!(CMsgClientWelcome), state, events).await
    } else if kind == ConnectionStatusWire::KIND {
        if let Some(reason) = lost_session_reason(&decode!(ConnectionStatusWire).0) {
            let state = state.read().await;
            // The ordered subscription starts before admission. Retry/queue
            // statuses preceding the first Welcome are historical handshake
            // traffic, whereas any loss after hydration invalidates the cache.
            if state.status != CacheStatus::Hydrating || state.welcome_generation != 0 {
                return Err(reason);
            }
        }
        true
    } else if kind == CMsgClientSuspended::KIND {
        return Err(DotaSteamError::SessionSuspended {
            time_end: decode!(CMsgClientSuspended).time_end,
        }
        .to_string());
    } else if kind == CMsgSOCacheSubscribed::KIND {
        apply_subscribed(decode!(CMsgSOCacheSubscribed), state, events).await
    } else if kind == CMsgSOCacheSubscribedUpToDate::KIND {
        let _ = decode!(CMsgSOCacheSubscribedUpToDate);
        apply_cache_up_to_date(state, events).await;
        true
    } else if kind == SOMultipleObjectsWire::KIND {
        apply_multiple(decode!(SOMultipleObjectsWire), state, events).await
    } else if kind == SOSingleObjectCreateWire::KIND {
        apply_single(decode!(SOSingleObjectCreateWire).0, state, events).await
    } else if kind == SOSingleObjectUpdateWire::KIND {
        apply_single(decode!(SOSingleObjectUpdateWire).0, state, events).await
    } else if kind == SOSingleObjectDestroyWire::KIND {
        apply_single_destroy(decode!(SOSingleObjectDestroyWire).0, state, events).await
    } else if kind == CMsgSOCacheUnsubscribed::KIND {
        apply_unsubscribed(decode!(CMsgSOCacheUnsubscribed), state, events).await;
        true
    } else if kind == LiveScoreboardWire::KIND {
        let scoreboard = map_live_scoreboard(&decode!(LiveScoreboardWire).0);
        let _ = events.send(DotaSteamEvent::LiveScoreboard(scoreboard));
        true
    } else {
        true // Match-detail job replies are consumed by GameCoordinator.
    };
    valid
        .then_some(())
        .ok_or_else(|| "invalid Dota lobby cache payload".to_owned())
}

fn lost_session_reason(message: &CMsgConnectionStatus) -> Option<String> {
    let status = message.status.map(|status| status.value());
    (status != Some(GCConnectionStatus::GCConnectionStatus_HAVE_SESSION as i32))
        .then(|| format!("Dota coordinator session unavailable (status: {status:?})"))
}

async fn begin_resync(state: &Arc<RwLock<ClientState>>) {
    let mut state = state.write().await;
    // Keep the last snapshot for identity, but no action may rely on it until
    // the GC resends caches: a lost update may have changed or removed it.
    state.lobby_observed_at = None;
    state.lobby_refresh_requested_at = None;
    state.resync_pending = true;
    warn!("Dota coordinator messages were dropped; resynchronizing caches");
}

async fn emit_disconnect(
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
    reason: String,
) {
    // Stream EOF is a disconnect too. Invalidate before notifying waiters;
    // leaving a Ready cache behind can authorize commands on a dead socket.
    state.write().await.status = CacheStatus::Disconnected;
    warn!(%reason, "Dota coordinator connection invalidated");
    let _ = events.send(DotaSteamEvent::TransportDisconnected { reason });
}

async fn apply_subscribed(
    message: CMsgSOCacheSubscribed,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) -> bool {
    let mut lobby = None;
    let mut contains_lobby_type = false;
    for object_type in &message.objects {
        if object_type.type_id() != SO_TYPE_LOBBY {
            continue;
        }
        contains_lobby_type = true;
        for data in &object_type.object_data {
            match CSODOTALobby::parse_from_bytes(data) {
                Ok(value) if value.lobby_id() != 0 => lobby = Some(map_lobby(&value)),
                _ => {
                    invalidate_lobby_cache(state, events).await;
                    return false;
                }
            }
        }
    }

    let previous = {
        let mut state = state.write().await;
        let previous = state.lobby.as_ref().map(|lobby| lobby.lobby_id);
        if contains_lobby_type
            && !state.accept_lobby_version(
                message.owner_soid.as_ref(),
                lobby.as_ref().map(|lobby| lobby.lobby_id).or(previous),
                message.version,
            )
        {
            return true;
        }
        state.mark_ready();
        if contains_lobby_type {
            if let Some(lobby) = &mut lobby {
                state.observe_lobby(lobby);
            }
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
    true
}

async fn invalidate_lobby_cache(
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    // Retain the last known identity for diagnostics, but never expose a
    // decode failure as authoritative absence or a usable stale snapshot.
    emit_disconnect(state, events, "invalid Dota lobby cache payload".to_owned()).await;
}

async fn apply_welcome(
    welcome: CMsgClientWelcome,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) -> bool {
    let hydrated = !welcome.outofdate_subscribed_caches.is_empty()
        || !welcome.uptodate_subscribed_caches.is_empty();
    let lobby_cache_present = welcome.outofdate_subscribed_caches.iter().any(|cache| {
        cache.owner_soid.type_ == Some(SO_OWNER_LOBBY)
            || cache
                .objects
                .iter()
                .any(|objects| objects.type_id() == SO_TYPE_LOBBY)
    }) || welcome
        .uptodate_subscribed_caches
        .iter()
        .any(|cache| cache.owner_soid.type_ == Some(SO_OWNER_LOBBY));
    for cache in welcome.outofdate_subscribed_caches {
        if !apply_subscribed(cache, state, events).await {
            return false;
        }
    }
    // A cache with a matching version is represented by a subscription check
    // in the welcome.  We do not keep a local cache between runs, so the
    // check still establishes that the authoritative empty snapshot is ready;
    // an out-of-date cache processed above takes precedence for lobby contents.
    if !welcome.uptodate_subscribed_caches.is_empty() {
        apply_cache_up_to_date(state, events).await;
    }
    if hydrated {
        let mut state = state.write().await;
        state.welcome_generation = state.welcome_generation.wrapping_add(1);
        state.welcome_observed_at = Some(tokio::time::Instant::now());
        // A resync Welcome without a lobby cache is authoritative absence: the
        // removal was among the dropped messages.
        let cleared = if std::mem::take(&mut state.resync_pending) && !lobby_cache_present {
            state.lobby_observed_at = None;
            state.lobby.take().map(|lobby| lobby.lobby_id)
        } else {
            None
        };
        drop(state);
        if cleared.is_some() {
            let _ = events.send(DotaSteamEvent::LobbyCleared { lobby_id: cleared });
        }
        let _ = events.send(DotaSteamEvent::CacheHydrated);
    }
    true
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
) -> bool {
    let mut updates = Vec::new();
    let mut removals = Vec::new();
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
            Ok(value) if value.lobby_id() != 0 => updates.push(map_lobby(&value)),
            _ => {
                invalidate_lobby_cache(state, events).await;
                return false;
            }
        }
    }
    for object in &message.0.objects_removed {
        if object.type_id() != SO_TYPE_LOBBY {
            continue;
        }
        match CSODOTALobby::parse_from_bytes(object.object_data()) {
            Ok(value) if value.lobby_id() != 0 => removals.push(value.lobby_id()),
            _ => {
                invalidate_lobby_cache(state, events).await;
                return false;
            }
        }
    }
    let lobby_id = updates
        .first()
        .map(|lobby| lobby.lobby_id)
        .or_else(|| removals.first().copied());
    if lobby_id.is_some()
        && !state.write().await.accept_lobby_version(
            message.0.owner_soid.as_ref(),
            lobby_id,
            message.0.version,
        )
    {
        return true;
    }
    for lobby in updates {
        apply_lobby(lobby, state, events).await;
    }
    for lobby_id in removals {
        clear_matching_lobby(lobby_id, state, events).await;
    }
    true
}

async fn apply_single(
    message: CMsgSOSingleObject,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) -> bool {
    if message.type_id() != SO_TYPE_LOBBY {
        return true;
    }
    let lobby = match CSODOTALobby::parse_from_bytes(message.object_data()) {
        Ok(value) if value.lobby_id() != 0 => map_lobby(&value),
        _ => {
            invalidate_lobby_cache(state, events).await;
            return false;
        }
    };
    if state.write().await.accept_lobby_version(
        message.owner_soid.as_ref(),
        Some(lobby.lobby_id),
        message.version,
    ) {
        apply_lobby(lobby, state, events).await;
    }
    true
}

async fn apply_single_destroy(
    message: CMsgSOSingleObject,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) -> bool {
    if message.type_id() != SO_TYPE_LOBBY {
        return true;
    }
    let lobby_id = match CSODOTALobby::parse_from_bytes(message.object_data()) {
        Ok(value) if value.lobby_id() != 0 => value.lobby_id(),
        _ => {
            invalidate_lobby_cache(state, events).await;
            return false;
        }
    };
    if state.write().await.accept_lobby_version(
        message.owner_soid.as_ref(),
        Some(lobby_id),
        message.version,
    ) {
        clear_matching_lobby(lobby_id, state, events).await;
    }
    true
}

async fn clear_matching_lobby(
    lobby_id: u64,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    let mut state = state.write().await;
    if state
        .lobby
        .as_ref()
        .is_none_or(|lobby| lobby.lobby_id != lobby_id)
    {
        return;
    }
    state.mark_ready();
    state.lobby_observed_at = None;
    state.lobby = None;
    drop(state);
    let _ = events.send(DotaSteamEvent::LobbyCleared {
        lobby_id: Some(lobby_id),
    });
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
    mut lobby: LobbySnapshot,
    state: &Arc<RwLock<ClientState>>,
    events: &broadcast::Sender<DotaSteamEvent>,
) {
    {
        let mut state = state.write().await;
        state.mark_ready();
        state.observe_lobby(&mut lobby);
        state.lobby = Some(lobby.clone());
        state.lobby_observed_at = Some(Instant::now());
    }
    let _ = events.send(DotaSteamEvent::LobbyUpdated(lobby));
}

#[cfg(test)]
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

fn log_lobby_transition(previous: Option<&LobbySnapshot>, lobby: &LobbySnapshot) {
    if previous.is_none_or(|old| {
        old.lobby_id != lobby.lobby_id
            || old.state != lobby.state
            || old.game_state != lobby.game_state
            || old.match_id != lobby.match_id
            || old.server_steam_id != lobby.server_steam_id
            || old.outcome != lobby.outcome
    }) {
        tracing::info!(lobby_id = lobby.lobby_id, state = ?lobby.state,
            previous_state = ?previous.map(|old| old.state), game_state = ?lobby.game_state,
            match_id = ?lobby.match_id, server_id = ?lobby.server_steam_id,
            outcome = ?lobby.outcome, "Dota GC lobby transition observed");
    }
}

fn map_lobby(lobby: &CSODOTALobby) -> LobbySnapshot {
    LobbySnapshot {
        launch_history: LobbyLaunchHistory::default(),
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
    ConnectionStatusWire,
    CMsgConnectionStatus,
    EGCBaseClientMsg,
    EGCBaseClientMsg::k_EMsgGCClientConnectionStatus
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
            lobby_launch_history: None,
            cache_versions: Default::default(),
            welcome_generation: 0,
            welcome_observed_at: None,
            lobby_observed_at: Some(Instant::now()),
            lobby_refresh_requested_at: None,
            resync_pending: false,
        }))
    }

    fn lobby_subscription(lobby: &CSODOTALobby, version: u64) -> CMsgSOCacheSubscribed {
        let mut object =
            steam_vent_proto_dota2::gcsdk_gcmessages::cmsg_socache_subscribed::SubscribedType::new(
            );
        object.set_type_id(SO_TYPE_LOBBY);
        object.object_data.push(lobby.write_to_bytes().unwrap());
        let mut message = CMsgSOCacheSubscribed::new();
        message.set_version(version);
        message.objects.push(object);
        message
    }

    fn lobby_update(lobby: &CSODOTALobby, version: u64) -> CMsgSOSingleObject {
        let mut message = CMsgSOSingleObject::new();
        message.set_type_id(SO_TYPE_LOBBY);
        message.set_version(version);
        message.set_object_data(lobby.write_to_bytes().unwrap());
        message
    }

    fn gc_envelope<Msg: NetMessage>(message: Msg) -> crate::handshake::FromGc {
        assert!(Msg::IS_PROTOBUF);
        let kind: steam_vent_proto_common::MsgKind = Msg::KIND.into();
        let mut payload = (kind.0 as u32 | 0x8000_0000).to_le_bytes().to_vec();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        message.write_body(&mut payload).unwrap();
        crate::handshake::FromGc(
            steam_vent_proto_steam::steammessages_clientserver_2::CMsgGCClient {
                appid: Some(crate::DOTA_APP_ID),
                msgtype: Some(kind.0 as u32 | if Msg::IS_PROTOBUF { 0x8000_0000 } else { 0 }),
                payload: Some(payload),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn ordered_gc_stream_preserves_fast_allocation_return_across_message_kinds() {
        let state = ready_lobby_state();
        state.write().await.status = CacheStatus::Hydrating;
        let mut no_session = CMsgConnectionStatus::new();
        no_session.set_status(GCConnectionStatus::GCConnectionStatus_NO_SESSION);
        let admission_retry = gc_envelope(ConnectionStatusWire(no_session.clone()));
        let disconnected = gc_envelope(ConnectionStatusWire(no_session));
        let (events, mut receiver) = broadcast::channel(EVENT_BUFFER);
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        lobby.set_state(csodotalobby::State::UI);
        let mut welcome = CMsgClientWelcome::new();
        welcome
            .outofdate_subscribed_caches
            .push(lobby_subscription(&lobby, 1));
        let initial = gc_envelope(welcome);
        lobby.set_state(csodotalobby::State::RUN);
        lobby.set_match_id(888);
        lobby.set_server_id(999);
        let running = gc_envelope(SOSingleObjectUpdateWire(lobby_update(&lobby, 2)));
        lobby.set_state(csodotalobby::State::UI);
        lobby.clear_server_id();
        let returned = gc_envelope(lobby_subscription(&lobby, 3));
        let _watcher = spawn_watchers(
            futures_util::stream::iter([
                Ok(admission_retry),
                Ok(initial),
                Ok(running),
                Ok(returned),
                Ok(disconnected),
            ]),
            &state,
            &events,
            || {},
        );
        let (stages, reason) = timeout(Duration::from_secs(1), async {
            let mut stages = Vec::new();
            loop {
                match receiver.recv().await.unwrap() {
                    DotaSteamEvent::LobbyUpdated(lobby) => stages.push(lobby.state),
                    DotaSteamEvent::TransportDisconnected { reason } => break (stages, reason),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            stages,
            vec![LobbyState::Ui, LobbyState::Run, LobbyState::Ui]
        );
        assert!(reason.contains("session unavailable"), "{reason}");
        let state = state.read().await;
        assert_eq!(state.status, CacheStatus::Disconnected);
        let current = state.lobby.as_ref().unwrap();
        assert_eq!(current.state, LobbyState::Ui);
        assert!(current.launch_history.allocation_observed);
        assert_eq!(current.launch_history.match_id, Some(888));
    }

    use steam_vent_proto_steam::steammessages_clientserver_2::CMsgGCClient;

    /// Mirror steam-vent's per-kind subscription: a 16-slot broadcast whose
    /// lag is reported as EOF and whose closure ends the stream.
    fn steam_vent_subscription(
        receiver: broadcast::Receiver<CMsgGCClient>,
    ) -> impl Stream<Item = GcMessage> + Send + 'static {
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            match receiver.recv().await {
                Ok(message) => Some((Ok(crate::handshake::FromGc(message)), receiver)),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Some((Err(NetworkError::EOF), receiver))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            }
        })
    }

    async fn burst_through(
        stream: impl Stream<Item = GcMessage> + Send + 'static,
        sender: broadcast::Sender<CMsgGCClient>,
    ) -> Vec<GcMessage> {
        // Like a 30s handshake: GC traffic arrives while nobody reads the stream.
        for _ in 0..100 {
            sender
                .send(gc_envelope(CMsgSOCacheSubscribedUpToDate::new()).0)
                .unwrap();
            tokio::task::yield_now().await;
        }
        drop(sender);
        stream.collect().await
    }

    #[tokio::test]
    async fn unread_subscription_across_handshake_burst_keeps_every_message() {
        let (sender, receiver) = broadcast::channel(16);
        let unbuffered = burst_through(steam_vent_subscription(receiver), sender).await;
        assert!(
            unbuffered
                .iter()
                .any(|item| matches!(item, Err(NetworkError::EOF))),
            "fixture must reproduce steam-vent's 16-message overflow"
        );

        let (sender, receiver) = broadcast::channel(16);
        let (buffered, pump) =
            crate::gc_stream::buffer_gc_messages(steam_vent_subscription(receiver));
        let _pump = WatcherTask(pump);
        let buffered = burst_through(buffered, sender).await;
        assert_eq!(buffered.len(), 100);
        assert!(buffered.iter().all(Result::is_ok));
    }

    #[tokio::test]
    async fn dropped_gc_messages_resync_instead_of_disconnecting() {
        let state = ready_lobby_state();
        let (events, mut receiver) = broadcast::channel(EVENT_BUFFER);
        let resyncs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&resyncs);
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        lobby.set_state(csodotalobby::State::RUN);
        let mut welcome = CMsgClientWelcome::new();
        welcome
            .outofdate_subscribed_caches
            .push(lobby_subscription(&lobby, 5));
        let (input, messages) = tokio::sync::mpsc::unbounded_channel();
        let mut messages = messages;
        let _watcher = spawn_watchers(
            futures_util::stream::poll_fn(move |cx| messages.poll_recv(cx)),
            &state,
            &events,
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );

        input.send(Err(NetworkError::EOF)).unwrap();
        timeout(Duration::from_secs(1), async {
            while resyncs.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        {
            let state = state.read().await;
            assert_eq!(state.status, CacheStatus::Ready);
            assert_eq!(state.lobby.as_ref().map(|l| l.lobby_id), Some(7001));
            // Nothing may act on the pre-loss observation until the GC resends.
            assert!(!state.lobby_observation_is_fresh(Duration::from_secs(60)));
        }

        input.send(Ok(gc_envelope(welcome))).unwrap();
        timeout(Duration::from_secs(1), async {
            loop {
                if let DotaSteamEvent::LobbyUpdated(lobby) = receiver.recv().await.unwrap() {
                    break lobby;
                }
            }
        })
        .await
        .unwrap();
        let state = state.read().await;
        assert_eq!(state.status, CacheStatus::Ready);
        assert_eq!(state.lobby.as_ref().unwrap().state, LobbyState::Run);
        assert!(state.lobby_observation_is_fresh(Duration::from_secs(60)));
        assert!(!state.resync_pending);
    }

    #[tokio::test]
    async fn resync_welcome_without_lobby_cache_clears_a_lobby_removed_while_lagged() {
        let state = ready_lobby_state();
        let (events, _receiver) = broadcast::channel(EVENT_BUFFER);
        let mut account = CMsgSOCacheSubscribed::new();
        account.owner_soid = MessageField::some(CMsgSOIDOwner {
            type_: Some(SO_OWNER_ACCOUNT),
            id: Some(TEST_STEAM_ID),
            ..Default::default()
        });
        let mut welcome = CMsgClientWelcome::new();
        welcome.outofdate_subscribed_caches.push(account);

        // An ordinary Welcome never infers absence from a missing lobby cache.
        assert!(apply_welcome(welcome.clone(), &state, &events).await);
        assert!(state.read().await.lobby.is_some());

        begin_resync(&state).await;
        assert!(apply_welcome(welcome, &state, &events).await);
        let state = state.read().await;
        assert_eq!(state.status, CacheStatus::Ready);
        assert!(state.lobby.is_none());
        assert!(!state.resync_pending);
    }

    #[tokio::test]
    async fn older_subscription_and_destroy_cannot_replace_a_newer_running_observation() {
        let state = ready_lobby_state();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        lobby.set_state(csodotalobby::State::UI);
        let old = lobby_subscription(&lobby, 1);
        let old_destroy = lobby_update(&lobby, 1);
        lobby.set_state(csodotalobby::State::RUN);
        lobby.set_match_id(888);
        lobby.set_server_id(999);
        assert!(apply_single(lobby_update(&lobby, 2), &state, &events).await);
        let observed_at = state.read().await.lobby_observed_at;
        assert!(apply_subscribed(old, &state, &events).await);
        assert!(apply_single_destroy(old_destroy, &state, &events).await);
        let state = state.read().await;
        assert_eq!(state.lobby.as_ref().unwrap().state, LobbyState::Run);
        assert_eq!(state.lobby_observed_at, observed_at);
    }

    #[tokio::test]
    async fn single_destroy_dispatch_removes_only_its_matching_lobby() {
        for id in [7001, 7002] {
            let state = ready_lobby_state();
            let (events, _) = broadcast::channel(EVENT_BUFFER);
            let mut key = CSODOTALobby::new();
            key.set_lobby_id(id);
            let raw = RawNetMessage::from_message(
                NetMessageHeader::default(),
                SOSingleObjectDestroyWire(lobby_update(&key, 1)),
            )
            .unwrap();
            apply_gc_message(raw, &state, &events).await.unwrap();
            assert_eq!(
                state
                    .read()
                    .await
                    .lobby
                    .as_ref()
                    .map(|lobby| lobby.lobby_id),
                if id == 7001 { None } else { Some(7001) }
            );
        }
    }

    #[tokio::test]
    async fn malformed_subscription_is_unavailable_instead_of_authoritative_absence() {
        for payload in [vec![0xff], Vec::new()] {
            let state = ready_lobby_state();
            let (events, mut receiver) = broadcast::channel(EVENT_BUFFER);
            let mut lobby = CSODOTALobby::new();
            lobby.set_lobby_id(7001);
            let mut cache = lobby_subscription(&lobby, 1);
            cache.objects[0].object_data = vec![payload];
            // A later unrelated cache in this Welcome must not re-authorize it.
            let mut welcome = CMsgClientWelcome::new();
            welcome.outofdate_subscribed_caches = vec![cache, CMsgSOCacheSubscribed::new()];
            assert!(!apply_welcome(welcome, &state, &events).await);
            assert_eq!(state.read().await.status, CacheStatus::Disconnected);
            assert_eq!(state.read().await.lobby.as_ref().unwrap().lobby_id, 7001);
            assert!(matches!(
                receiver.recv().await.unwrap(),
                DotaSteamEvent::TransportDisconnected { .. }
            ));
            assert!(receiver.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn checked_actions_reject_changed_stale_foreign_and_unavailable_observations() {
        let state = ready_lobby_state();
        let mut state = state.write().await;
        state.lobby.as_mut().unwrap().leader_steam_id = TEST_STEAM_ID;
        let expected = state.lobby.clone().unwrap();
        let authorized = |state: &ClientState| {
            authorized_lobby_action(
                state,
                TEST_STEAM_ID,
                7001,
                Duration::from_secs(45),
                |current| current == &expected,
            )
        };
        assert!(authorized(&state));
        state.lobby.as_mut().unwrap().state = LobbyState::Run;
        assert!(!authorized(&state));
        state.lobby = Some(expected.clone());
        state.lobby_observed_at = Some(Instant::now() - Duration::from_secs(46));
        assert!(!authorized(&state));
        state.lobby_observed_at = Some(Instant::now());
        state.lobby.as_mut().unwrap().leader_steam_id = TEST_STEAM_ID + 1;
        assert!(!authorized(&state));
        state.lobby = Some(expected.clone());
        state.status = CacheStatus::Disconnected;
        assert!(!authorized(&state));
    }

    #[tokio::test]
    async fn launch_history_survives_fast_return_and_cache_resubscription() {
        let state = ready_lobby_state();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let mut wire = CSODOTALobby::new();
        wire.set_lobby_id(7001);
        wire.set_state(csodotalobby::State::SERVERSETUP);
        let mut update = CMsgSOSingleObject::new();
        update.set_type_id(SO_TYPE_LOBBY);
        update.set_object_data(wire.write_to_bytes().unwrap());
        apply_single(update, &state, &events).await;
        wire.set_state(csodotalobby::State::RUN);
        wire.set_match_id(888);
        wire.set_server_id(999);
        let mut update = CMsgSOSingleObject::new();
        update.set_type_id(SO_TYPE_LOBBY);
        update.set_object_data(wire.write_to_bytes().unwrap());
        apply_single(update, &state, &events).await;

        // A full UI snapshot can replace both updates before the worker polls.
        wire.set_state(csodotalobby::State::UI);
        wire.clear_match_id();
        wire.clear_server_id();
        let mut object =
            steam_vent_proto_dota2::gcsdk_gcmessages::cmsg_socache_subscribed::SubscribedType::new(
            );
        object.set_type_id(SO_TYPE_LOBBY);
        object.object_data.push(wire.write_to_bytes().unwrap());
        let mut subscribed = CMsgSOCacheSubscribed::new();
        subscribed.objects.push(object);
        apply_subscribed(subscribed, &state, &events).await;
        let current = state.read().await.lobby.clone().unwrap();
        assert_eq!(current.state, LobbyState::Ui);
        assert_eq!(current.match_id, None);
        assert_eq!(current.server_steam_id, None);
        assert!(current.launch_history.allocation_observed);
        assert_eq!(current.launch_history.match_id, Some(888));
        assert!(!current.launch_history.conflicting_match_ids);

        clear_lobby(&state, &events).await;
        apply_lobby(map_lobby(&wire), &state, &events).await;
        assert!(
            state
                .read()
                .await
                .lobby
                .as_ref()
                .unwrap()
                .launch_history
                .allocation_observed
        );
        assert_eq!(
            state
                .read()
                .await
                .lobby
                .as_ref()
                .unwrap()
                .launch_history
                .match_id,
            Some(888)
        );

        wire.set_lobby_id(7002);
        apply_lobby(map_lobby(&wire), &state, &events).await;
        assert_eq!(
            state.read().await.lobby.as_ref().unwrap().launch_history,
            LobbyLaunchHistory::default()
        );
    }

    #[tokio::test]
    async fn launch_history_retains_gameplay_and_conflicting_match_ids() {
        let state = ready_lobby_state();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let mut current = state.read().await.lobby.clone().unwrap();
        current.state = LobbyState::Run;
        current.game_state = Some(5);
        current.match_id = Some(888);
        apply_lobby(current.clone(), &state, &events).await;
        current.state = LobbyState::Ui;
        current.game_state = Some(0);
        current.match_id = Some(889);
        apply_lobby(current, &state, &events).await;
        let history = state
            .read()
            .await
            .lobby
            .as_ref()
            .unwrap()
            .launch_history
            .clone();
        assert!(history.game_states.contains(&5));
        assert!(history.game_states.contains(&0));
        assert!(history.conflicting_match_ids);
        assert_eq!(history.match_id, Some(888));
    }

    #[tokio::test]
    async fn initial_cache_wait_rejects_disconnect_that_precedes_subscription() {
        let state = ready_lobby_state();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        emit_disconnect(&state, &events, "connection lost".into()).await;
        assert!(matches!(
            wait_for_initial_cache_state(&state, events.subscribe()).await,
            Err(DotaSteamError::NotReady)
        ));
    }

    #[tokio::test]
    async fn subscribed_wait_reads_an_update_delivered_before_waiting() {
        let state = ready_lobby_state();
        let (events, receiver) = broadcast::channel(EVENT_BUFFER);
        // The authoritative update lands after subscription but before the
        // first read, exactly the window that previously lost create replies.
        let mut lobby = state.read().await.lobby.clone().unwrap();
        lobby.game_name = "new lobby".into();
        apply_lobby(lobby, &state, &events).await;
        let result = wait_for_state(&state, receiver, |state| {
            Ok(state
                .lobby
                .as_ref()
                .filter(|lobby| lobby.game_name == "new lobby")
                .cloned())
        })
        .await
        .unwrap();
        assert_eq!(result.game_name, "new lobby");
    }

    #[tokio::test]
    async fn lagged_wait_recovers_the_authoritative_lobby() {
        let state = ready_lobby_state();
        let (events, receiver) = broadcast::channel(1);
        let waiter = wait_for_state(&state, receiver, |state| {
            Ok(state
                .lobby
                .as_ref()
                .filter(|lobby| lobby.game_name == "new lobby")
                .cloned())
        });
        let update = async {
            tokio::task::yield_now().await;
            let mut lobby = state.read().await.lobby.clone().unwrap();
            lobby.game_name = "new lobby".into();
            apply_lobby(lobby, &state, &events).await;
            for _ in 0..3 {
                let _ = events.send(DotaSteamEvent::CacheHydrated);
            }
        };
        let (result, ()) = tokio::join!(waiter, update);
        assert_eq!(result.unwrap().game_name, "new lobby");
    }

    fn fresh_welcome() -> CMsgClientWelcome {
        CMsgClientWelcome {
            outofdate_subscribed_caches: vec![CMsgSOCacheSubscribed::new()],
            ..Default::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn recent_admission_can_prepare_creation_but_idle_cache_cannot() {
        let state = ready_lobby_state();
        state.write().await.lobby = None;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        assert!(!creation_session_is_fresh(&*state.read().await).unwrap());
        apply_welcome(fresh_welcome(), &state, &events).await;
        assert!(creation_session_is_fresh(&*state.read().await).unwrap());
        tokio::time::advance(CREATION_SESSION_MAX_AGE + Duration::from_secs(1)).await;
        // Incidental SOCache traffic must not renew session admission freshness.
        apply_subscribed(CMsgSOCacheSubscribed::new(), &state, &events).await;
        assert!(!creation_session_is_fresh(&*state.read().await).unwrap());
    }

    #[tokio::test]
    async fn fresh_admission_never_bypasses_existing_lobby_or_disconnect() {
        let state = ready_lobby_state();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        apply_welcome(fresh_welcome(), &state, &events).await;
        assert!(matches!(
            creation_session_is_fresh(&*state.read().await),
            Err(DotaSteamError::AlreadyInLobby(7001))
        ));
        state.write().await.lobby = None;
        emit_disconnect(&state, &events, "connection lost".into()).await;
        assert!(matches!(
            creation_session_is_fresh(&*state.read().await),
            Err(DotaSteamError::NotReady)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn creation_preflight_rejects_stale_ready_cache_and_incidental_updates() {
        let state = ready_lobby_state();
        state.write().await.lobby = None;
        let (events, receiver) = broadcast::channel(EVENT_BUFFER);
        apply_subscribed(CMsgSOCacheSubscribed::new(), &state, &events).await;
        let result = timeout(
            Duration::from_secs(1),
            wait_for_creation_preflight(&state, receiver, 0),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_welcome_does_not_prove_creation_readiness() {
        let state = ready_lobby_state();
        state.write().await.lobby = None;
        let (events, receiver) = broadcast::channel(EVENT_BUFFER);
        apply_welcome(CMsgClientWelcome::new(), &state, &events).await;
        assert_eq!(state.read().await.welcome_generation, 0);
        assert!(
            timeout(
                Duration::from_secs(1),
                wait_for_creation_preflight(&state, receiver, 0),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn pending_lobby_wait_observes_disconnect_instead_of_timing_out() {
        let state = ready_lobby_state();
        state.write().await.lobby = None;
        let (events, receiver) = broadcast::channel(EVENT_BUFFER);
        let waiter = wait_for_state(&state, receiver, |state| Ok(state.lobby.clone()));
        let disconnect = async {
            tokio::task::yield_now().await;
            emit_disconnect(&state, &events, "connection lost".into()).await;
        };
        let (result, ()) = tokio::join!(waiter, disconnect);
        assert!(matches!(result, Err(DotaSteamError::NotReady)));
    }

    #[tokio::test]
    async fn creation_preflight_requires_fresh_welcome_and_no_existing_lobby() {
        for existing_lobby in [false, true] {
            let state = ready_lobby_state();
            if !existing_lobby {
                state.write().await.lobby = None;
            }
            let (events, receiver) = broadcast::channel(EVENT_BUFFER);
            apply_welcome(fresh_welcome(), &state, &events).await;
            let result = wait_for_creation_preflight(&state, receiver, 0).await;
            if existing_lobby {
                assert!(matches!(result, Err(DotaSteamError::AlreadyInLobby(7001))));
            } else {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn creation_preflight_fails_on_disconnect_before_a_welcome() {
        let state = ready_lobby_state();
        state.write().await.lobby = None;
        let (events, receiver) = broadcast::channel(EVENT_BUFFER);
        emit_disconnect(&state, &events, "connection lost".into()).await;
        assert!(matches!(
            wait_for_creation_preflight(&state, receiver, 0).await,
            Err(DotaSteamError::NotReady)
        ));
    }

    #[test]
    fn losing_gc_session_invalidates_readiness_even_when_steam_stays_connected() {
        let healthy = CMsgConnectionStatus {
            status: Some(GCConnectionStatus::GCConnectionStatus_HAVE_SESSION.into()),
            ..Default::default()
        };
        assert!(lost_session_reason(&healthy).is_none());
        for status in [
            None,
            Some(EnumOrUnknown::from_i32(1)),
            Some(EnumOrUnknown::from_i32(999)),
        ] {
            assert!(
                lost_session_reason(&CMsgConnectionStatus {
                    status,
                    ..Default::default()
                })
                .is_some()
            );
        }
    }

    #[tokio::test]
    async fn watcher_exit_invalidates_cached_lobby_before_notifying_waiters() {
        let state = ready_lobby_state();
        let (events, mut receiver) = broadcast::channel(EVENT_BUFFER);
        emit_disconnect(&state, &events, "stream closed".into()).await;

        assert_eq!(state.read().await.status, CacheStatus::Disconnected);
        assert!(state.read().await.lobby.is_some());
        assert!(
            !state
                .read()
                .await
                .lobby_observation_is_fresh(Duration::from_secs(45))
        );
        assert!(matches!(
            receiver.try_recv().unwrap(),
            DotaSteamEvent::TransportDisconnected { reason } if reason == "stream closed"
        ));
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

    #[tokio::test]
    async fn quiet_lobby_refresh_resend_renews_observation() {
        let state = ready_lobby_state();
        let max_age = Duration::from_secs(45);
        // A recently observed lobby needs no resend request.
        assert_eq!(state.read().await.lobby_refresh_target(), None);

        // An idle lobby receives no pushes; its ageing observation is refreshed.
        let old = Instant::now() - Duration::from_secs(60);
        state.write().await.lobby_observed_at = Some(old);
        assert_eq!(state.read().await.lobby_refresh_target(), Some(7001));
        assert!(!state.read().await.lobby_observation_is_fresh(max_age));
        // An unanswered request is not repeated on every freshness check.
        state.write().await.lobby_refresh_requested_at = Some(Instant::now());
        assert_eq!(state.read().await.lobby_refresh_target(), None);
        state.write().await.lobby_refresh_requested_at = Some(old);
        assert_eq!(state.read().await.lobby_refresh_target(), Some(7001));

        // The GC answers a lobby-owner refresh with a full cache resend.
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        let mut subscribed_type =
            steam_vent_proto_dota2::gcsdk_gcmessages::cmsg_socache_subscribed::SubscribedType::new(
            );
        subscribed_type.set_type_id(SO_TYPE_LOBBY);
        subscribed_type
            .object_data
            .push(lobby.write_to_bytes().expect("encode lobby fixture"));
        let mut cache = CMsgSOCacheSubscribed::new();
        cache.objects.push(subscribed_type);
        cache.owner_soid = MessageField::some(CMsgSOIDOwner {
            type_: Some(SO_OWNER_LOBBY),
            id: Some(7001),
            ..Default::default()
        });
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        apply_subscribed(cache, &state, &events).await;
        assert!(state.read().await.lobby_observation_is_fresh(max_age));
        assert_eq!(state.read().await.lobby_refresh_target(), None);

        // Nothing trustworthy to renew without a live cache and lobby.
        state.write().await.lobby_observed_at = Some(old);
        state.write().await.status = CacheStatus::Disconnected;
        assert_eq!(state.read().await.lobby_refresh_target(), None);
        state.write().await.status = CacheStatus::Ready;
        state.write().await.lobby = None;
        assert_eq!(state.read().await.lobby_refresh_target(), None);
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

    #[tokio::test]
    async fn lobby_traffic_after_account_unsubscribe_cannot_restore_readiness() {
        let mut lobby = CSODOTALobby::new();
        lobby.set_lobby_id(7001);
        lobby.set_state(csodotalobby::State::UI);
        type Apply = for<'a> fn(
            &'a CSODOTALobby,
            &'a Arc<RwLock<ClientState>>,
            &'a broadcast::Sender<DotaSteamEvent>,
        ) -> futures_util::future::BoxFuture<'a, bool>;
        let paths: [(&str, Apply); 3] = [
            ("subscription", |lobby, state, events| {
                Box::pin(apply_subscribed(
                    lobby_subscription(lobby, 9),
                    state,
                    events,
                ))
            }),
            ("single update", |lobby, state, events| {
                Box::pin(apply_single(lobby_update(lobby, 9), state, events))
            }),
            ("destroy", |lobby, state, events| {
                Box::pin(apply_single_destroy(lobby_update(lobby, 9), state, events))
            }),
        ];
        for (name, apply) in paths {
            let state = ready_lobby_state();
            let (events, _) = broadcast::channel(EVENT_BUFFER);
            apply_unsubscribed(
                unsubscribe(SO_OWNER_ACCOUNT, TEST_STEAM_ID),
                &state,
                &events,
            )
            .await;
            assert_eq!(state.read().await.status, CacheStatus::Disconnected);
            // Keep a lobby present so a destroy still has something to match.
            state.write().await.lobby = Some(map_lobby(&lobby));

            assert!(apply(&lobby, &state, &events).await, "{name}");

            assert_eq!(
                state.read().await.status,
                CacheStatus::Disconnected,
                "{name} restored a session without its account cache"
            );
        }
    }

    #[test]
    fn lobby_name_is_not_a_server_search_key() {
        let config = LobbyConfig {
            game_name: "Cama pending 607".into(),
            pass_key: Some("private-lobby-password".into()),
            server_region: 27,
            ..LobbyConfig::default()
        };
        let request = CMsgPracticeLobbyCreate::parse_from_bytes(
            &build_create_request(&config)
                .unwrap()
                .write_to_bytes()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(request.search_key, None);
        assert_eq!(request.lobby_details.game_name(), "Cama pending 607");
        assert_eq!(request.lobby_details.server_region(), 27);
        assert_eq!(request.pass_key(), "private-lobby-password");
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
            lobby_launch_history: None,
            cache_versions: Default::default(),
            welcome_generation: 0,
            welcome_observed_at: None,
            lobby_observed_at: None,
            lobby_refresh_requested_at: None,
            resync_pending: false,
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
