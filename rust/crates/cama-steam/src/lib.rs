//! Steam/Game Coordinator transport for Dota 2.
//!
//! The crate intentionally stops at the Steam/GC boundary.  It owns the
//! authenticated connection, the Dota GC handshake, the shared-object cache,
//! and the typed lobby/match operations.  Application code can consume
//! [`DotaSteamClient::subscribe`] and decide how to persist, announce, or
//! expose the resulting snapshots.

#![forbid(unsafe_code)]

mod auth;
mod handshake;
mod lobby;
pub mod metadata;

pub use auth::{
    AuthError, AuthenticatedSteam, GuardConfirmation, SteamAuth, SteamAuthConfig, SteamSession,
};
pub use lobby::{
    DotaSteamClient, DotaSteamError, DotaSteamEvent, LivePlayer, LiveScoreboard, LiveTeam,
    LobbyConfig, LobbyMember, LobbyOutcome, LobbySnapshot, LobbyState, MatchDetails,
    MatchMetadataLocation, MatchPlayer, MatchReplay, MatchTeam, Team,
};

/// Dota 2's Steam app ID.
pub const DOTA_APP_ID: u32 = 570;

/// Shared-object type id for [`steam_vent_proto_dota2::dota_gcmessages_common_lobby::CSODOTALobby`].
pub const SO_TYPE_LOBBY: i32 = 2004;

/// Shared-object type id for a Dota lobby invite.
pub const SO_TYPE_LOBBY_INVITE: i32 = 2011;
