//! Personal dota play-time preference: purely informational, never enforced.
//!
//! Ports the play-time-related methods of `services/player_service.py`.

use cama_db::registration_repository::{RegistrationRepository, RegistrationRepositoryError};
use cama_domain::playtime::{PlayHoursPlayer, aggregate_popular_hours};
use cama_domain::timezone::DEFAULT_TIMEZONE;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlaytimeServiceError {
    #[error("Player not registered.")]
    PlayerNotRegistered,
    #[error("Give at least one hour.")]
    EmptyHours,
    #[error("Hours must be between 0 and 23.")]
    InvalidHour,
    #[error("playtime SQLite operation failed: {0}")]
    Sqlite(String),
}

impl From<RegistrationRepositoryError> for PlaytimeServiceError {
    fn from(error: RegistrationRepositoryError) -> Self {
        Self::Sqlite(error.to_string())
    }
}

#[derive(Clone)]
pub struct PlaytimeService {
    repository: RegistrationRepository,
}

impl PlaytimeService {
    #[must_use]
    pub const fn new(repository: RegistrationRepository) -> Self {
        Self { repository }
    }

    /// Persist a player's informational dota play-time hours.
    ///
    /// Purely informational — unlike curfew windows, this never blocks or
    /// removes anyone from a lobby. It only feeds `popular_play_hours`.
    pub fn set_play_hours(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        hours: &[i64],
    ) -> Result<(), PlaytimeServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(PlaytimeServiceError::PlayerNotRegistered);
        }
        if hours.is_empty() {
            return Err(PlaytimeServiceError::EmptyHours);
        }
        if hours.iter().any(|&hour| !(0..=23).contains(&hour)) {
            return Err(PlaytimeServiceError::InvalidHour);
        }
        let mut sorted: Vec<i64> = hours.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        self.repository
            .set_dota_play_hours(discord_id, guild_id, Some(&sorted))?;
        Ok(())
    }

    /// Clear a player's informational dota play-time hours.
    pub fn clear_play_hours(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), PlaytimeServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(PlaytimeServiceError::PlayerNotRegistered);
        }
        self.repository
            .set_dota_play_hours(discord_id, guild_id, None)?;
        Ok(())
    }

    /// The player's informational play-time hours for display.
    pub fn play_hours_info(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<i64>>, PlaytimeServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(PlaytimeServiceError::PlayerNotRegistered);
        }
        Ok(self.repository.dota_play_hours_of(discord_id, guild_id)?)
    }

    /// Aggregate every registered player's play hours into a 24-length
    /// histogram, index i = count of players who want to play at hour i
    /// (0-23) in `reporting_timezone` (default America/New_York).
    pub fn popular_play_hours(
        &self,
        guild_id: Option<i64>,
        reporting_timezone: Option<&str>,
    ) -> Result<[u32; 24], PlaytimeServiceError> {
        let rows = self.repository.play_hours_for_guild(guild_id)?;
        let players: Vec<PlayHoursPlayer> = rows
            .into_iter()
            .map(|(timezone, hours)| PlayHoursPlayer {
                dota_play_hours: hours.map(|hours| {
                    hours
                        .into_iter()
                        .filter_map(|hour| u32::try_from(hour).ok())
                        .collect()
                }),
                timezone,
            })
            .collect();
        Ok(aggregate_popular_hours(
            &players,
            reporting_timezone.unwrap_or(DEFAULT_TIMEZONE),
            None,
        ))
    }
}

#[cfg(test)]
#[path = "playtime_service/tests.rs"]
mod tests;
