//! Dedicated-account Deadlock Game Coordinator transport.
//!
//! This adapter never logs into Dota or infers a final winner from a lobby
//! disappearing. Cache type IDs are deployment inputs: Valve does not publish
//! them in the protobuf schema and they must be qualified against a live account.
use crate::{AuthError, AuthenticatedSteam, SteamAuth, SteamAuthConfig};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, sync::Arc, time::Duration};
use steam_vent::{ConnectionTrait, GameCoordinator, NetMessage, NetworkError};
use steam_vent_proto_common::{
    RpcMessage, RpcMessageWithKind,
    protobuf::{self, Message, MessageField},
};
use steam_vent_proto_deadlock::{
    citadel_gcmessages_client::*, citadel_gcmessages_common::*, gcsdk_gcmessages::*,
};
use steam_vent_proto_steam::{
    enums_clientserver::EMsg, steammessages_clientserver_2::CMsgGCClient,
};
use tokio::{
    sync::{Mutex, RwLock, broadcast},
    task::JoinHandle,
    time::timeout,
};

pub const DEADLOCK_APP_ID: u32 = 1_422_450;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CACHE_OBJECT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlockCacheTypes {
    pub party: i32,
    pub lobby: i32,
}
impl DeadlockCacheTypes {
    pub fn validate(self) -> Result<(), DeadlockSteamError> {
        if self.party <= 0 || self.lobby <= 0 || self.party == self.lobby {
            return Err(DeadlockSteamError::InvalidConfiguration(
                "distinct positive, live-qualified shared-object IDs are required",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeadlockGameMode {
    StreetBrawl,
    Standard,
}
impl DeadlockGameMode {
    pub const fn wire_value(self) -> i32 {
        match self {
            Self::StreetBrawl => 4,
            Self::Standard => 1,
        }
    }
    pub const fn player_count(self) -> usize {
        match self {
            Self::StreetBrawl => 8,
            Self::Standard => 12,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeadlockTeam {
    One,
    Two,
}
impl DeadlockTeam {
    pub const fn wire_value(self) -> u32 {
        match self {
            Self::One => 0,
            Self::Two => 1,
        }
    }
}
#[derive(Clone, Debug)]
pub struct DeadlockPartyConfig {
    pub mode: DeadlockGameMode,
    pub client_version: u32,
    pub server_region: u32,
    pub data_center_codes: Vec<u32>,
    pub ping_times: Vec<u32>,
}
impl DeadlockPartyConfig {
    fn validate(&self) -> Result<(), DeadlockSteamError> {
        if self.client_version == 0
            || self.data_center_codes.is_empty()
            || self.data_center_codes.len() != self.ping_times.len()
        {
            return Err(DeadlockSteamError::InvalidConfiguration(
                "current client version and matching datacenter/ping measurements are required",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeadlockPartyMember {
    pub account_id: u32,
    pub ready: bool,
    pub spectator: bool,
    pub team: Option<u32>,
    pub admin: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeadlockPartySnapshot {
    pub party_id: u64,
    pub join_code: Option<u64>,
    pub mode: i32,
    pub private: bool,
    pub members: Vec<DeadlockPartyMember>,
    pub matchmaking: bool,
}
impl DeadlockPartySnapshot {
    /// Exact playing roster and GC team mapping, with no unexpected players.
    pub fn roster_ready(&self, mode: DeadlockGameMode, expected: &[(u32, DeadlockTeam)]) -> bool {
        let unique: BTreeSet<_> = expected.iter().map(|(id, _)| *id).collect();
        self.private
            && self.mode == mode.wire_value()
            && expected.len() == mode.player_count()
            && unique.len() == expected.len()
            && !unique.contains(&0)
            && expected
                .iter()
                .filter(|(_, team)| *team == DeadlockTeam::One)
                .count()
                == mode.player_count() / 2
            && self.members.iter().filter(|m| !m.spectator).count() == expected.len()
            && expected.iter().all(|(id, team)| {
                self.members.iter().any(|m| {
                    m.account_id == *id
                        && !m.spectator
                        && m.ready
                        && m.team == Some(team.wire_value())
                })
            })
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeadlockLobbySnapshot {
    pub lobby_id: u64,
    pub match_id: Option<u64>,
    pub mode: i32,
    pub server_state: Option<i32>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeadlockSnapshot {
    pub party: Option<DeadlockPartySnapshot>,
    pub lobby: Option<DeadlockLobbySnapshot>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeadlockSteamEvent {
    Updated(DeadlockSnapshot),
    Disconnected,
}
/// A locator only. Metadata salts are not evidence of a winner or completed match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlockMetadataLocation {
    pub match_id: u64,
    pub metadata_salt: u32,
    pub replay_salt: Option<u32>,
    pub replay_group_id: Option<u32>,
}
#[derive(Debug, thiserror::Error)]
pub enum DeadlockSteamError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error(transparent)]
    Protobuf(#[from] protobuf::Error),
    #[error("Deadlock coordinator operation timed out; its remote effect may be unknown")]
    Timeout,
    #[error("Deadlock coordinator is disconnected or its cache is unavailable")]
    NotReady,
    #[error("invalid Deadlock configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("Deadlock {operation} rejected with coordinator code {code}")]
    Rejected { operation: &'static str, code: i32 },
    #[error("Deadlock account is already in a party or match")]
    Busy,
    #[error("Authenticated Steam account differs from the configured dedicated Deadlock account")]
    UnexpectedAccount,
    #[error("Deadlock party identity or administrator rights do not match")]
    WrongParty,
    #[error("Deadlock playing roster, teams, format or readiness differ from the frozen match")]
    RosterMismatch,
    #[error("Deadlock version lookup failed")]
    VersionLookup,
    #[error("Deadlock metadata response did not contain a metadata salt")]
    MissingMetadata,
}
struct ClientState {
    ready: bool,
    snapshot: DeadlockSnapshot,
}
struct Watcher(JoinHandle<()>);
impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Clone)]
pub struct DeadlockSteamClient {
    _connection: steam_vent::Connection,
    gc: Arc<Mutex<GameCoordinator>>,
    state: Arc<RwLock<ClientState>>,
    events: broadcast::Sender<DeadlockSteamEvent>,
    _watcher: Arc<Watcher>,
    own_account_id: u32,
}
impl fmt::Debug for DeadlockSteamClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeadlockSteamClient")
            .field("own_account_id", &self.own_account_id)
            .finish_non_exhaustive()
    }
}
impl DeadlockSteamClient {
    pub async fn connect(
        config: &SteamAuthConfig,
        cache_types: DeadlockCacheTypes,
    ) -> Result<Self, DeadlockSteamError> {
        cache_types.validate()?;
        Self::from_authenticated(SteamAuth::connect(config).await?, cache_types).await
    }
    /// Verify the dedicated account before announcing Deadlock game presence.
    /// A mistaken username/session must never displace a running Dota host.
    pub async fn connect_for_account(
        config: &SteamAuthConfig,
        cache_types: DeadlockCacheTypes,
        expected_account: u32,
    ) -> Result<Self, DeadlockSteamError> {
        cache_types.validate()?;
        let authenticated = SteamAuth::connect(config).await?;
        if authenticated.connection.steam_id().account_id() != expected_account {
            return Err(DeadlockSteamError::UnexpectedAccount);
        }
        Self::from_authenticated(authenticated, cache_types).await
    }
    pub async fn from_authenticated(
        auth: AuthenticatedSteam,
        cache_types: DeadlockCacheTypes,
    ) -> Result<Self, DeadlockSteamError> {
        cache_types.validate()?;
        // Subscribe before the handshake; retain all following cache mutations.
        let mut raw = auth.connection.on::<FromGc>();
        let (gc, welcome) = timeout(
            COMMAND_TIMEOUT,
            auth.connection
                .game_coordinator(&steam_vent_proto_deadlock::GCHandshake::default()),
        )
        .await
        .map_err(|_| DeadlockSteamError::Timeout)??;
        let mut initial = DeadlockSnapshot::default();
        apply_welcome(&mut initial, &welcome, cache_types)?;
        let state = Arc::new(RwLock::new(ClientState {
            ready: true,
            snapshot: initial,
        }));
        let (events, _) = broadcast::channel(128);
        let watcher_state = state.clone();
        let watcher_events = events.clone();
        let watcher = tokio::spawn(async move {
            while let Some(Ok(FromGc(message))) = raw.next().await {
                let mut state = watcher_state.write().await;
                match apply_envelope(&mut state.snapshot, &message, cache_types) {
                    Ok(changed) => {
                        if changed {
                            let _ = watcher_events
                                .send(DeadlockSteamEvent::Updated(state.snapshot.clone()));
                        }
                    }
                    Err(_) => break,
                }
            }
            watcher_state.write().await.ready = false;
            let _ = watcher_events.send(DeadlockSteamEvent::Disconnected);
        });
        Ok(Self {
            own_account_id: auth.connection.steam_id().account_id(),
            _connection: auth.connection,
            gc: Arc::new(Mutex::new(gc)),
            state,
            events,
            _watcher: Arc::new(Watcher(watcher)),
        })
    }
    pub const fn own_account_id(&self) -> u32 {
        self.own_account_id
    }
    pub fn subscribe(&self) -> broadcast::Receiver<DeadlockSteamEvent> {
        self.events.subscribe()
    }
    pub async fn snapshot(&self) -> Result<DeadlockSnapshot, DeadlockSteamError> {
        let state = self.state.read().await;
        if !state.ready {
            return Err(DeadlockSteamError::NotReady);
        }
        Ok(state.snapshot.clone())
    }
    async fn job<Q: NetMessage, R: NetMessage>(&self, request: Q) -> Result<R, DeadlockSteamError> {
        timeout(COMMAND_TIMEOUT, async {
            let gc = self.gc.lock().await;
            gc.job::<Q, R>(request).await
        })
        .await
        .map_err(|_| DeadlockSteamError::Timeout)?
        .map_err(Into::into)
    }
    /// Caller must persist create intent first. Never retry an uncertain create.
    pub async fn create_party(
        &self,
        config: &DeadlockPartyConfig,
    ) -> Result<u64, DeadlockSteamError> {
        config.validate()?;
        let snapshot = self.snapshot().await?;
        if snapshot.party.is_some() || snapshot.lobby.is_some() {
            return Err(DeadlockSteamError::Busy);
        }
        let response: CMsgClientToGCPartyCreateResponse = self.job(create_request(config)).await?;
        check_result("create", response.result.map(|v| v.value()))?;
        response
            .party_id
            .filter(|id| *id != 0)
            .ok_or(DeadlockSteamError::WrongParty)
    }
    async fn require_party(
        &self,
        party_id: u64,
    ) -> Result<DeadlockPartySnapshot, DeadlockSteamError> {
        self.snapshot()
            .await?
            .party
            .filter(|party| {
                party.party_id == party_id
                    && party
                        .members
                        .iter()
                        .any(|member| member.account_id == self.own_account_id && member.admin)
            })
            .ok_or(DeadlockSteamError::WrongParty)
    }
    pub async fn set_member_team(
        &self,
        party_id: u64,
        account_id: u32,
        team: DeadlockTeam,
    ) -> Result<(), DeadlockSteamError> {
        self.action(party_id, account_id, 8, u64::from(team.wire_value()))
            .await
    }
    pub async fn move_host_to_spectator(&self, party_id: u64) -> Result<(), DeadlockSteamError> {
        self.action(party_id, self.own_account_id, 10, 31).await
    }
    async fn action(
        &self,
        party_id: u64,
        account_id: u32,
        action: i32,
        value: u64,
    ) -> Result<(), DeadlockSteamError> {
        self.require_party(party_id).await?;
        let response: CMsgClientToGCPartyActionResponse = self
            .job(CMsgClientToGCPartyAction {
                party_id: Some(party_id),
                target_account_id: Some(account_id),
                action_id: Some(protobuf::EnumOrUnknown::from_i32(action)),
                uint_value: Some(value),
                ..Default::default()
            })
            .await?;
        check_result("party action", response.result.map(|v| v.value()))
    }
    pub async fn set_ready(&self, party_id: u64, ready: bool) -> Result<(), DeadlockSteamError> {
        self.require_party(party_id).await?;
        let response: CMsgClientToGCPartySetReadyStateResponse = self
            .job(CMsgClientToGCPartySetReadyState {
                party_id: Some(party_id),
                ready_state: Some(ready),
                ..Default::default()
            })
            .await?;
        check_result("ready", response.result.map(|v| v.value()))
    }
    /// Caller must durably close betting and persist launch intent before calling.
    pub async fn start_match(
        &self,
        party_id: u64,
        mode: DeadlockGameMode,
        expected: &[(u32, DeadlockTeam)],
    ) -> Result<(), DeadlockSteamError> {
        let party = self.require_party(party_id).await?;
        if !party.roster_ready(mode, expected) {
            return Err(DeadlockSteamError::RosterMismatch);
        }
        let response: CMsgClientToGCPartyStartMatchResponse = self
            .job(CMsgClientToGCPartyStartMatch {
                party_id: Some(party_id),
                ..Default::default()
            })
            .await?;
        check_result("start", response.result.map(|v| v.value()))
    }
    /// Leave only the exact, positively terminal game-server lobby. The empty
    /// GC response is not proof of removal; callers must wait for cache removal.
    pub async fn leave_finished_lobby(
        &self,
        lobby_id: u64,
        match_id: u64,
    ) -> Result<(), DeadlockSteamError> {
        let lobby = self
            .snapshot()
            .await?
            .lobby
            .ok_or(DeadlockSteamError::WrongParty)?;
        if lobby.lobby_id != lobby_id
            || lobby.match_id != Some(match_id)
            || !matches!(lobby.server_state, Some(2..=4))
        {
            return Err(DeadlockSteamError::WrongParty);
        }
        let _: CMsgClientToGCLeaveLobbyResponse = self
            .job(CMsgClientToGCLeaveLobby {
                lobby_id: Some(lobby_id),
                ..Default::default()
            })
            .await?;
        Ok(())
    }
    pub async fn leave_party(&self, party_id: u64) -> Result<(), DeadlockSteamError> {
        let party = self.snapshot().await?.party;
        if party.as_ref().is_some_and(|p| p.party_id != party_id) {
            return Err(DeadlockSteamError::WrongParty);
        }
        let response: CMsgClientToGCPartyLeaveResponse = self
            .job(CMsgClientToGCPartyLeave {
                party_id: Some(party_id),
                ..Default::default()
            })
            .await?;
        match response.result.map(|v| v.value()) {
            Some(1 | 2) => Ok(()),
            code => check_result("leave", code),
        }
    }
    pub async fn metadata_location(
        &self,
        match_id: u64,
    ) -> Result<DeadlockMetadataLocation, DeadlockSteamError> {
        let response: CMsgClientToGCGetMatchMetaDataResponse = self
            .job(CMsgClientToGCGetMatchMetaData {
                match_id: Some(match_id),
                ..Default::default()
            })
            .await?;
        check_result("metadata", response.result.map(|v| v.value()))?;
        Ok(DeadlockMetadataLocation {
            match_id,
            metadata_salt: response
                .metadata_salt
                .ok_or(DeadlockSteamError::MissingMetadata)?,
            replay_salt: response.replay_salt,
            replay_group_id: response.replay_group_id,
        })
    }
}
/// Current minimum supported version, fetched for each creation (no stale fallback).
pub async fn current_client_version(http: &reqwest::Client) -> Result<u32, DeadlockSteamError> {
    let result = http
        .get("https://api.steampowered.com/IGCVersion_1422450/GetClientVersion/v1/")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| DeadlockSteamError::VersionLookup)?
        .error_for_status()
        .map_err(|_| DeadlockSteamError::VersionLookup)?
        .json::<serde_json::Value>()
        .await
        .map_err(|_| DeadlockSteamError::VersionLookup)?;
    parse_client_version(&result)
}
fn parse_client_version(value: &serde_json::Value) -> Result<u32, DeadlockSteamError> {
    if value
        .pointer("/result/success")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(DeadlockSteamError::VersionLookup);
    }
    value
        .pointer("/result/min_allowed_version")
        .or_else(|| value.get("min_allowed_version"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or(DeadlockSteamError::VersionLookup)
}
fn check_result(operation: &'static str, code: Option<i32>) -> Result<(), DeadlockSteamError> {
    match code {
        Some(1) => Ok(()),
        code => Err(DeadlockSteamError::Rejected {
            operation,
            code: code.unwrap_or(0),
        }),
    }
}
fn create_request(config: &DeadlockPartyConfig) -> CMsgClientToGCPartyCreate {
    CMsgClientToGCPartyCreate {
        party_mm_info: MessageField::some(CMsgPartyMMInfo {
            platform: Some(protobuf::EnumOrUnknown::from_i32(1)),
            client_version: Some(config.client_version),
            pgi_verified: Some(true),
            ping_times: MessageField::some(CMsgRegionPingTimesClient {
                data_center_codes: config.data_center_codes.clone(),
                ping_times: config.ping_times.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        disable_party_code: Some(false),
        is_private_lobby: Some(true),
        game_mode: Some(protobuf::EnumOrUnknown::from_i32(config.mode.wire_value())),
        mm_preference: Some(protobuf::EnumOrUnknown::from_i32(1)),
        private_lobby_settings: MessageField::some(csocitadel_party::PrivateLobbySettings {
            min_roster_size: Some(config.mode.player_count() as u32),
            server_region: Some(config.server_region),
            is_publicly_visible: Some(false),
            randomize_lanes: Some(false),
            cheats_enabled: Some(false),
            duplicate_heroes_enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn apply_welcome(
    snapshot: &mut DeadlockSnapshot,
    welcome: &CMsgClientWelcome,
    types: DeadlockCacheTypes,
) -> Result<(), DeadlockSteamError> {
    *snapshot = DeadlockSnapshot::default();
    for cache in &welcome.outofdate_subscribed_caches {
        apply_subscribed(snapshot, cache, types)?;
    }
    Ok(())
}
fn apply_subscribed(
    snapshot: &mut DeadlockSnapshot,
    cache: &CMsgSOCacheSubscribed,
    types: DeadlockCacheTypes,
) -> Result<(), DeadlockSteamError> {
    for object_type in &cache.objects {
        if object_type.type_id() == types.party {
            snapshot.party = None;
        }
        if object_type.type_id() == types.lobby {
            snapshot.lobby = None;
        }
        for data in &object_type.object_data {
            apply_object(snapshot, object_type.type_id(), data, false, types)?;
        }
    }
    Ok(())
}
fn apply_object(
    snapshot: &mut DeadlockSnapshot,
    type_id: i32,
    data: &[u8],
    destroy: bool,
    types: DeadlockCacheTypes,
) -> Result<(), DeadlockSteamError> {
    if type_id != types.party && type_id != types.lobby {
        return Ok(());
    }
    if data.len() > MAX_CACHE_OBJECT {
        return Err(DeadlockSteamError::InvalidConfiguration(
            "oversized coordinator object",
        ));
    }
    if type_id == types.party {
        let party = CSOCitadelParty::parse_from_bytes(data)?;
        if party.party_id() == 0 {
            return Err(DeadlockSteamError::WrongParty);
        }
        if destroy {
            if snapshot
                .party
                .as_ref()
                .is_some_and(|p| p.party_id == party.party_id())
            {
                snapshot.party = None;
            }
        } else {
            snapshot.party = Some(DeadlockPartySnapshot {
                party_id: party.party_id(),
                join_code: party.join_code.filter(|c| *c != 0),
                mode: party.game_mode.map_or(0, |v| v.value()),
                private: party.is_private_lobby(),
                matchmaking: party.match_making_start_time() > 0,
                members: party
                    .members
                    .iter()
                    .map(|m| DeadlockPartyMember {
                        account_id: m.account_id(),
                        ready: m.is_ready(),
                        spectator: m.player_type.map(|v| v.value()) == Some(1),
                        team: m.team,
                        admin: m.rights_flags() & 3 != 0,
                    })
                    .collect(),
            });
        }
    } else {
        let lobby = CSOCitadelLobby::parse_from_bytes(data)?;
        if lobby.lobby_id() == 0 {
            return Err(DeadlockSteamError::WrongParty);
        }
        if destroy {
            if snapshot
                .lobby
                .as_ref()
                .is_some_and(|l| l.lobby_id == lobby.lobby_id())
            {
                snapshot.lobby = None;
            }
        } else {
            snapshot.lobby = Some(DeadlockLobbySnapshot {
                lobby_id: lobby.lobby_id(),
                match_id: lobby.match_id.filter(|id| *id > 0),
                mode: lobby.game_mode.map_or(0, |v| v.value()),
                server_state: lobby.server_state.map(|v| v.value()),
            });
        }
    }
    Ok(())
}
fn apply_envelope(
    snapshot: &mut DeadlockSnapshot,
    message: &CMsgGCClient,
    types: DeadlockCacheTypes,
) -> Result<bool, DeadlockSteamError> {
    if message.appid() != DEADLOCK_APP_ID {
        return Ok(false);
    }
    let kind = message.msgtype() & 0x7fff_ffff;
    if !matches!(kind, 21 | 22 | 23 | 24 | 25 | 26 | 4004 | 4009) {
        return Ok(false);
    }
    let payload = message.payload();
    if message.msgtype() & 0x8000_0000 == 0 || payload.len() < 8 {
        return Err(NetworkError::InvalidHeader.into());
    }
    let header_len = u32::from_le_bytes(
        payload[4..8]
            .try_into()
            .map_err(|_| NetworkError::InvalidHeader)?,
    ) as usize;
    let body = payload[8..]
        .get(header_len..)
        .ok_or(NetworkError::InvalidHeader)?;
    let previous = snapshot.clone();
    match kind {
        4004 => apply_welcome(snapshot, &CMsgClientWelcome::parse_from_bytes(body)?, types)?,
        4009 => {
            let status = CMsgConnectionStatus::parse_from_bytes(body)?;
            if status.status.map(|v| v.value()) != Some(0) {
                return Err(DeadlockSteamError::NotReady);
            }
        }
        24 => apply_subscribed(
            snapshot,
            &CMsgSOCacheSubscribed::parse_from_bytes(body)?,
            types,
        )?,
        21..=23 => {
            let object = CMsgSOSingleObject::parse_from_bytes(body)?;
            apply_object(
                snapshot,
                object.type_id(),
                object.object_data(),
                kind == 23,
                types,
            )?;
        }
        25 => {
            *snapshot = DeadlockSnapshot::default();
        }
        26 => {
            let update = CMsgSOMultipleObjects::parse_from_bytes(body)?;
            for object in update.objects_added.iter().chain(&update.objects_modified) {
                apply_object(
                    snapshot,
                    object.type_id(),
                    object.object_data(),
                    false,
                    types,
                )?;
            }
            for object in &update.objects_removed {
                apply_object(
                    snapshot,
                    object.type_id(),
                    object.object_data(),
                    true,
                    types,
                )?;
            }
        }
        _ => {}
    }
    Ok(*snapshot != previous)
}
#[derive(Debug)]
struct FromGc(CMsgGCClient);
impl RpcMessageWithKind for FromGc {
    type KindEnum = EMsg;
    const KIND: EMsg = EMsg::k_EMsgClientFromGC;
}
impl RpcMessage for FromGc {
    fn parse(reader: &mut dyn std::io::Read) -> protobuf::Result<Self> {
        CMsgGCClient::parse_from_reader(reader).map(Self)
    }
    fn write(&self, writer: &mut dyn std::io::Write) -> protobuf::Result<()> {
        self.0.write_to_writer(writer)
    }
    fn encode_size(&self) -> usize {
        self.0.compute_size() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TYPES: DeadlockCacheTypes = DeadlockCacheTypes {
        party: 3001,
        lobby: 3002,
    };
    fn party() -> DeadlockPartySnapshot {
        DeadlockPartySnapshot {
            party_id: 42,
            join_code: Some(123),
            mode: 4,
            private: true,
            matchmaking: false,
            members: (1..=8)
                .map(|id| DeadlockPartyMember {
                    account_id: id,
                    ready: true,
                    spectator: false,
                    team: Some(if id <= 4 { 0 } else { 1 }),
                    admin: false,
                })
                .collect(),
        }
    }
    fn roster() -> Vec<(u32, DeadlockTeam)> {
        (1..=8)
            .map(|id| {
                (
                    id,
                    if id <= 4 {
                        DeadlockTeam::One
                    } else {
                        DeadlockTeam::Two
                    },
                )
            })
            .collect()
    }
    fn envelope(kind: u32, body: impl Message) -> CMsgGCClient {
        let mut payload = (kind | 0x8000_0000).to_le_bytes().to_vec();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend(body.write_to_bytes().unwrap());
        CMsgGCClient {
            appid: Some(DEADLOCK_APP_ID),
            msgtype: Some(kind | 0x8000_0000),
            payload: Some(payload),
            ..Default::default()
        }
    }
    #[test]
    fn exact_roster_blocks_wrong_mode_side_duplicates_unready_and_extra_players() {
        let expected = roster();
        let valid = party();
        assert!(valid.roster_ready(DeadlockGameMode::StreetBrawl, &expected));
        assert!(!valid.roster_ready(DeadlockGameMode::Standard, &expected));
        let mut wrong = valid.clone();
        wrong.members[0].team = Some(1);
        assert!(!wrong.roster_ready(DeadlockGameMode::StreetBrawl, &expected));
        wrong = valid.clone();
        wrong.members[0].ready = false;
        assert!(!wrong.roster_ready(DeadlockGameMode::StreetBrawl, &expected));
        wrong = valid.clone();
        wrong.members.push(wrong.members[0].clone());
        assert!(!wrong.roster_ready(DeadlockGameMode::StreetBrawl, &expected));
        let mut repeated = expected.clone();
        repeated[0] = repeated[1];
        assert!(!valid.roster_ready(DeadlockGameMode::StreetBrawl, &repeated));
    }
    #[test]
    fn private_requests_disable_cheats_and_have_exact_capacity() {
        for mode in [DeadlockGameMode::StreetBrawl, DeadlockGameMode::Standard] {
            let config = DeadlockPartyConfig {
                mode,
                client_version: 123,
                server_region: 1,
                data_center_codes: vec![123],
                ping_times: vec![20],
            };
            let request = create_request(&config);
            assert!(request.is_private_lobby());
            assert_eq!(request.game_mode.unwrap().value(), mode.wire_value());
            let settings = request.private_lobby_settings.unwrap();
            assert_eq!(settings.min_roster_size(), mode.player_count() as u32);
            assert!(!settings.is_publicly_visible());
            assert!(!settings.cheats_enabled());
            assert!(!settings.duplicate_heroes_enabled());
        }
    }
    #[test]
    fn full_and_incremental_cache_updates_and_destroy_preserve_identity() {
        let mut snapshot = DeadlockSnapshot::default();
        let object = CSOCitadelParty {
            party_id: Some(42),
            join_code: Some(555),
            game_mode: Some(protobuf::EnumOrUnknown::from_i32(4)),
            is_private_lobby: Some(true),
            ..Default::default()
        };
        let single = CMsgSOSingleObject {
            type_id: Some(TYPES.party),
            object_data: Some(object.write_to_bytes().unwrap()),
            ..Default::default()
        };
        apply_envelope(&mut snapshot, &envelope(21, single.clone()), TYPES).unwrap();
        assert_eq!(snapshot.party.as_ref().unwrap().join_code, Some(555));
        let mut unrelated = single.clone();
        unrelated.object_data = Some(
            CSOCitadelParty {
                party_id: Some(99),
                ..Default::default()
            }
            .write_to_bytes()
            .unwrap(),
        );
        apply_envelope(&mut snapshot, &envelope(23, unrelated), TYPES).unwrap();
        assert!(snapshot.party.is_some());
        apply_envelope(&mut snapshot, &envelope(23, single), TYPES).unwrap();
        assert!(snapshot.party.is_none());
        let multiple=CMsgSOMultipleObjects {objects_added:vec![steam_vent_proto_deadlock::gcsdk_gcmessages::cmsg_somultiple_objects::SingleObject {type_id:Some(TYPES.party),object_data:Some(object.write_to_bytes().unwrap()),..Default::default()}],..Default::default()};
        apply_envelope(&mut snapshot, &envelope(26, multiple), TYPES).unwrap();
        assert_eq!(snapshot.party.unwrap().party_id, 42);
    }
    #[test]
    fn disconnect_and_bad_headers_fail_closed_and_other_games_are_ignored() {
        let mut snapshot = DeadlockSnapshot::default();
        let status = CMsgConnectionStatus {
            status: Some(protobuf::EnumOrUnknown::from_i32(2)),
            ..Default::default()
        };
        assert!(matches!(
            apply_envelope(&mut snapshot, &envelope(4009, status), TYPES),
            Err(DeadlockSteamError::NotReady)
        ));
        let mut message = envelope(21, CMsgSOSingleObject::default());
        message.payload.as_mut().unwrap()[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(apply_envelope(&mut snapshot, &message, TYPES).is_err());
        message.appid = Some(570);
        assert!(!apply_envelope(&mut snapshot, &message, TYPES).unwrap());
    }
    #[test]
    fn unknown_coordinator_results_never_imply_success_or_winner() {
        for code in [None, Some(0), Some(2), Some(99)] {
            assert!(check_result("create", code).is_err());
        }
        assert!(check_result("create", Some(1)).is_ok());
        assert!(
            parse_client_version(
                &serde_json::json!({"result":{"success":true,"min_allowed_version":123}})
            )
            .is_ok()
        );
        assert!(
            parse_client_version(&serde_json::json!({"result":{"min_allowed_version":0}})).is_err()
        );
    }
}

/// Safe cache diagnostics for account qualification. No object contents, player
/// names, join codes, Steam IDs, tokens or match identifiers are exposed.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeadlockCacheProbe {
    pub type_id: i32,
    pub object_count: usize,
    pub encoded_bytes: usize,
    pub private_party_shape_candidates: usize,
    pub private_lobby_shape_candidates: usize,
}
/// Inspect the welcome snapshot without creating, joining or starting a match.
/// Shape hints help operators investigate IDs; they are never trusted as IDs by
/// the production client and must be checked with controlled party/match changes.
pub async fn probe_cache(
    auth: AuthenticatedSteam,
) -> Result<Vec<DeadlockCacheProbe>, DeadlockSteamError> {
    let own_account = auth.connection.steam_id().account_id();
    let (_gc, welcome) = timeout(
        COMMAND_TIMEOUT,
        auth.connection
            .game_coordinator(&steam_vent_proto_deadlock::GCHandshake::default()),
    )
    .await
    .map_err(|_| DeadlockSteamError::Timeout)??;
    Ok(probe_welcome(&welcome, own_account))
}
fn probe_welcome(welcome: &CMsgClientWelcome, own_account: u32) -> Vec<DeadlockCacheProbe> {
    let mut summaries: std::collections::BTreeMap<i32, DeadlockCacheProbe> =
        std::collections::BTreeMap::new();
    for cache in &welcome.outofdate_subscribed_caches {
        for object in &cache.objects {
            let summary = summaries
                .entry(object.type_id())
                .or_insert(DeadlockCacheProbe {
                    type_id: object.type_id(),
                    object_count: 0,
                    encoded_bytes: 0,
                    private_party_shape_candidates: 0,
                    private_lobby_shape_candidates: 0,
                });
            for bytes in &object.object_data {
                summary.object_count += 1;
                summary.encoded_bytes = summary.encoded_bytes.saturating_add(bytes.len());
                if bytes.len() > MAX_CACHE_OBJECT {
                    continue;
                }
                if let Ok(party) = CSOCitadelParty::parse_from_bytes(bytes)
                    && party.party_id() > 0
                    && party.is_private_lobby()
                    && matches!(party.game_mode.map(|v| v.value()), Some(1 | 4))
                    && party
                        .members
                        .iter()
                        .any(|member| member.account_id == Some(own_account))
                {
                    summary.private_party_shape_candidates += 1;
                }
                if let Ok(lobby) = CSOCitadelLobby::parse_from_bytes(bytes)
                    && lobby.lobby_id() > 0
                    && lobby.match_id() > 0
                    && lobby.match_mode.map(|v| v.value()) == Some(2)
                    && matches!(lobby.game_mode.map(|v| v.value()), Some(1 | 4))
                    && lobby.server_steam_id.is_some()
                {
                    summary.private_lobby_shape_candidates += 1;
                }
            }
        }
    }
    summaries.into_values().collect()
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn probe_reports_type_shape_without_private_object_contents() {
        let party = CSOCitadelParty {
            party_id: Some(123456789),
            join_code: Some(987654321),
            is_private_lobby: Some(true),
            game_mode: Some(protobuf::EnumOrUnknown::from_i32(4)),
            members: vec![csocitadel_party::Member {
                account_id: Some(42),
                persona_name: Some("Private persona".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let welcome=CMsgClientWelcome{outofdate_subscribed_caches:vec![CMsgSOCacheSubscribed{objects:vec![steam_vent_proto_deadlock::gcsdk_gcmessages::cmsg_socache_subscribed::SubscribedType{type_id:Some(3001),object_data:vec![party.write_to_bytes().unwrap()],..Default::default()}],..Default::default()}],..Default::default()};
        let summaries = probe_welcome(&welcome, 42);
        assert_eq!(summaries[0].type_id, 3001);
        assert_eq!(summaries[0].private_party_shape_candidates, 1);
        assert_eq!(
            probe_welcome(&welcome, 99)[0].private_party_shape_candidates,
            0
        );
        let rendered = serde_json::to_string(&summaries).unwrap();
        for secret in ["Private persona", "123456789", "987654321"] {
            assert!(!rendered.contains(secret));
        }
    }
}
