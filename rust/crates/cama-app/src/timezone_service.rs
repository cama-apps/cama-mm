//! General per-player timezone preference: validation and persistence.
//!
//! Ports the timezone-related methods of `services/player_service.py`.

use cama_db::registration_repository::{RegistrationRepository, RegistrationRepositoryError};
use cama_domain::timezone::is_valid_timezone;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TimezoneServiceError {
    #[error("Player not registered.")]
    PlayerNotRegistered,
    #[error("Unknown timezone '{0}'. Use an IANA name like 'America/New_York'.")]
    InvalidTimezone(String),
    #[error("timezone SQLite operation failed: {0}")]
    Sqlite(String),
}

impl From<RegistrationRepositoryError> for TimezoneServiceError {
    fn from(error: RegistrationRepositoryError) -> Self {
        Self::Sqlite(error.to_string())
    }
}

#[derive(Clone)]
pub struct TimezoneService {
    repository: RegistrationRepository,
}

impl TimezoneService {
    #[must_use]
    pub const fn new(repository: RegistrationRepository) -> Self {
        Self { repository }
    }

    /// Persist a player's general timezone (used by curfew windows and other
    /// time-based features).
    pub fn set_timezone(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        timezone: &str,
    ) -> Result<(), TimezoneServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(TimezoneServiceError::PlayerNotRegistered);
        }
        if !is_valid_timezone(timezone) {
            return Err(TimezoneServiceError::InvalidTimezone(timezone.to_owned()));
        }
        self.repository
            .set_timezone(discord_id, guild_id, timezone)?;
        Ok(())
    }

    /// The player's general timezone setting for display, or `None` if unset.
    pub fn timezone_info(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, TimezoneServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(TimezoneServiceError::PlayerNotRegistered);
        }
        Ok(self.repository.timezone_of(discord_id, guild_id)?)
    }
}

#[cfg(test)]
#[path = "timezone_service/tests.rs"]
mod tests;
