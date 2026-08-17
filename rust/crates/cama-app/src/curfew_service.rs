//! Curfew-window management and enforcement.
//!
//! A player can register any number of named windows (e.g. "work", "night
//! shift" — the name and purpose are entirely up to them). Each independently
//! blocks a fresh lobby join and gets swept out of any lobby they're already
//! queued in once the window starts. Ports `services/curfew_service.py`.

use std::collections::BTreeMap;

use cama_db::curfew::CurfewRepository;
use cama_domain::curfew::{CurfewWindow, find_active_window, is_valid_timezone};
use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::dedicated_lobby_channel::{GuildId, LobbyScope, UserId};
use crate::embeds::LobbyKind;
use crate::lobby_service::{LobbyClock, LobbyPlayerPort, LobbyService, PendingMatchPort};

pub const MAX_WINDOW_NAME_LENGTH: usize = 40;

#[derive(Debug, Error)]
pub enum CurfewServiceError {
    #[error("Player not registered.")]
    PlayerNotRegistered,
    #[error("Give this window a name.")]
    EmptyName,
    #[error("Name must be {MAX_WINDOW_NAME_LENGTH} characters or fewer.")]
    NameTooLong,
    #[error("Hour must be between 0 and 23.")]
    InvalidHour,
    #[error("Minute must be between 0 and 59.")]
    InvalidMinute,
    #[error("Start and end time can't be the same.")]
    EqualStartAndEnd,
    #[error("Unknown timezone '{0}'. Use an IANA name like 'America/New_York'.")]
    InvalidTimezone(String),
    #[error("{0}")]
    InvalidDays(String),
    #[error("curfew SQLite operation failed: {0}")]
    Sqlite(String),
}

impl From<rusqlite::Error> for CurfewServiceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurfewKick {
    pub discord_id: i64,
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub window_name: String,
}

#[derive(Clone)]
pub struct CurfewService {
    repository: CurfewRepository,
}

impl CurfewService {
    #[must_use]
    pub const fn new(repository: CurfewRepository) -> Self {
        Self { repository }
    }

    /// Create a window, or overwrite it if the player already has one by that name.
    #[allow(clippy::too_many_arguments)]
    pub fn add_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
        start_hour: u32,
        start_minute: u32,
        end_hour: u32,
        end_minute: u32,
        timezone: Option<&str>,
        days: Option<&str>,
    ) -> Result<CurfewWindow, CurfewServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(CurfewServiceError::PlayerNotRegistered);
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(CurfewServiceError::EmptyName);
        }
        if name.chars().count() > MAX_WINDOW_NAME_LENGTH {
            return Err(CurfewServiceError::NameTooLong);
        }
        for (hour, minute) in [(start_hour, start_minute), (end_hour, end_minute)] {
            if hour > 23 {
                return Err(CurfewServiceError::InvalidHour);
            }
            if minute > 59 {
                return Err(CurfewServiceError::InvalidMinute);
            }
        }
        if start_hour == end_hour && start_minute == end_minute {
            return Err(CurfewServiceError::EqualStartAndEnd);
        }
        if let Some(timezone) = timezone
            && !is_valid_timezone(timezone)
        {
            return Err(CurfewServiceError::InvalidTimezone(timezone.to_owned()));
        }
        let days = days
            .map(cama_domain::curfew::parse_weekdays)
            .transpose()
            .map_err(CurfewServiceError::InvalidDays)?;
        let window = CurfewWindow {
            discord_id,
            guild_id,
            name: name.to_owned(),
            start_hour,
            start_minute,
            end_hour,
            end_minute,
            timezone: timezone.map(str::to_owned),
            days,
        };
        self.repository.add_or_replace(&window)?;
        Ok(window)
    }

    /// Delete a named window. Returns `true` if it existed.
    pub fn remove_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
    ) -> Result<bool, CurfewServiceError> {
        Ok(self.repository.remove(discord_id, guild_id, name.trim())?)
    }

    pub fn list_windows(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<CurfewWindow>, CurfewServiceError> {
        Ok(self.repository.list_for_player(discord_id, guild_id)?)
    }

    /// The player's general `/player timezone` setting, for display purposes.
    pub fn general_timezone(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, CurfewServiceError> {
        Ok(self.repository.general_timezone(discord_id, guild_id)?)
    }

    /// Return the player's currently-active window, if any.
    pub fn active_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<CurfewWindow>, CurfewServiceError> {
        let windows = self.repository.list_for_player(discord_id, guild_id)?;
        if windows.is_empty() {
            return Ok(None);
        }
        let general_timezone = self.repository.general_timezone(discord_id, guild_id)?;
        Ok(find_active_window(&windows, general_timezone.as_deref(), now).cloned())
    }

    /// Remove any lobby member currently inside one of their windows.
    ///
    /// Returns the players actually removed, for the caller to notify.
    #[must_use]
    pub fn sweep<P, M, C>(
        &self,
        lobby: &LobbyService<P, M, C>,
        guild_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Vec<CurfewKick>
    where
        P: LobbyPlayerPort,
        M: PendingMatchPort,
        C: LobbyClock,
    {
        let mut kicks = Vec::new();
        for &guild_id in guild_ids {
            for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
                kicks.extend(self.sweep_lobby(lobby, guild_id, kind, now));
            }
        }
        kicks
    }

    fn sweep_lobby<P, M, C>(
        &self,
        lobby: &LobbyService<P, M, C>,
        guild_id: i64,
        kind: LobbyKind,
        now: DateTime<Utc>,
    ) -> Vec<CurfewKick>
    where
        P: LobbyPlayerPort,
        M: PendingMatchPort,
        C: LobbyClock,
    {
        let scope = LobbyScope::new(GuildId(guild_id), kind);
        let Some(snapshot) = lobby.get_lobby(scope) else {
            return Vec::new();
        };
        if snapshot.players.is_empty() {
            return Vec::new();
        }
        let discord_ids: Vec<i64> = snapshot.players.iter().map(|player| player.0).collect();
        let Ok(windows_by_player) = self.repository.list_for_players(&discord_ids, guild_id) else {
            return Vec::new();
        };
        if windows_by_player.is_empty() {
            return Vec::new();
        }

        let mut due: BTreeMap<i64, String> = BTreeMap::new();
        for (discord_id, windows) in &windows_by_player {
            let general_timezone = self
                .repository
                .general_timezone(*discord_id, guild_id)
                .unwrap_or(None);
            if let Some(window) = find_active_window(windows, general_timezone.as_deref(), now) {
                due.insert(*discord_id, window.name.clone());
            }
        }
        if due.is_empty() {
            return Vec::new();
        }

        let player_ids = due.keys().map(|discord_id| UserId(*discord_id)).collect();
        let removed = lobby.remove_players_from_lobby(&player_ids, scope);
        removed
            .into_iter()
            .filter_map(|user_id| {
                due.get(&user_id.0).map(|window_name| CurfewKick {
                    discord_id: user_id.0,
                    guild_id,
                    lobby_kind: kind,
                    window_name: window_name.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "curfew_service/tests.rs"]
mod tests;
