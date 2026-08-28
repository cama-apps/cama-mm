//! Existing-schema persistence for per-user push notification targets and
//! preferences (ntfy.sh and Discord direct message).
//!
//! Repository construction never creates or migrates schema. The runtime's
//! database initialization owns migration before this adapter is published.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PushNotificationKind {
    Readycheck,
    MatchStarted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PushNotificationChannel {
    Ntfy,
    DirectMessage,
}

impl PushNotificationKind {
    #[must_use]
    pub const fn column(self, channel: PushNotificationChannel) -> &'static str {
        match (self, channel) {
            (Self::Readycheck, PushNotificationChannel::Ntfy) => "readycheck_enabled",
            (Self::MatchStarted, PushNotificationChannel::Ntfy) => "match_started_enabled",
            (Self::Readycheck, PushNotificationChannel::DirectMessage) => "dm_readycheck_enabled",
            (Self::MatchStarted, PushNotificationChannel::DirectMessage) => {
                "dm_match_started_enabled"
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushNotificationTarget {
    pub topic: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushNotificationConfig {
    /// Absent when the player has never generated an ntfy topic — direct
    /// message delivery does not require one.
    pub target: Option<PushNotificationTarget>,
    pub readycheck_enabled: bool,
    pub match_started_enabled: bool,
    pub dm_readycheck_enabled: bool,
    pub dm_match_started_enabled: bool,
}

impl PushNotificationConfig {
    #[must_use]
    pub const fn enabled(
        &self,
        kind: PushNotificationKind,
        channel: PushNotificationChannel,
    ) -> bool {
        match (kind, channel) {
            (PushNotificationKind::Readycheck, PushNotificationChannel::Ntfy) => {
                self.readycheck_enabled
            }
            (PushNotificationKind::MatchStarted, PushNotificationChannel::Ntfy) => {
                self.match_started_enabled
            }
            (PushNotificationKind::Readycheck, PushNotificationChannel::DirectMessage) => {
                self.dm_readycheck_enabled
            }
            (PushNotificationKind::MatchStarted, PushNotificationChannel::DirectMessage) => {
                self.dm_match_started_enabled
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum PushNotificationRepositoryError {
    #[error("push notification SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct PushNotificationRepository {
    path: PathBuf,
}

impl PushNotificationRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn get_config(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PushNotificationConfig>, PushNotificationRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT ntfy_topic,readycheck_enabled,match_started_enabled,
                        dm_readycheck_enabled,dm_match_started_enabled
                   FROM push_notification_targets
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(PushNotificationConfig {
                        target: row
                            .get::<_, Option<String>>(0)?
                            .map(|topic| PushNotificationTarget { topic }),
                        readycheck_enabled: row.get(1)?,
                        match_started_enabled: row.get(2)?,
                        dm_readycheck_enabled: row.get(3)?,
                        dm_match_started_enabled: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert or replace the generated delivery topic. A new target enables
    /// both ntfy alert kinds; regenerating an existing topic preserves every
    /// toggle, including direct-message preferences.
    pub fn set_target(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        topic: &str,
        updated_at: i64,
    ) -> Result<(), PushNotificationRepositoryError> {
        self.connection()?.execute(
            "INSERT INTO push_notification_targets
                 (discord_id,guild_id,ntfy_topic,readycheck_enabled,match_started_enabled,updated_at)
             VALUES (?1,?2,?3,1,1,?4)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                 ntfy_topic=excluded.ntfy_topic,
                 updated_at=excluded.updated_at",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                topic,
                updated_at,
            ],
        )?;
        Ok(())
    }

    /// Toggle one alert kind on one delivery channel. An ntfy toggle requires
    /// an existing target row (there is nothing to delay-deliver to without a
    /// topic) and returns `false` when none exists. A direct-message toggle
    /// needs no prior setup and creates the preference row on first use.
    pub fn set_enabled(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        kind: PushNotificationKind,
        channel: PushNotificationChannel,
        enabled: bool,
        updated_at: i64,
    ) -> Result<bool, PushNotificationRepositoryError> {
        // `kind`/`channel` are closed and the only source for this
        // interpolated identifier.
        let column = kind.column(channel);
        let connection = self.connection()?;
        match channel {
            PushNotificationChannel::Ntfy => {
                let sql = format!(
                    "UPDATE push_notification_targets SET {column}=?1,updated_at=?2
                     WHERE discord_id=?3 AND guild_id=?4"
                );
                let changed = connection.execute(
                    &sql,
                    params![
                        i64::from(enabled),
                        updated_at,
                        discord_id,
                        Self::normalize_guild_id(guild_id),
                    ],
                )?;
                Ok(changed > 0)
            }
            PushNotificationChannel::DirectMessage => {
                let sql = format!(
                    "INSERT INTO push_notification_targets (discord_id,guild_id,{column},updated_at)
                     VALUES (?1,?2,?3,?4)
                     ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                         {column}=excluded.{column},
                         updated_at=excluded.updated_at"
                );
                connection.execute(
                    &sql,
                    params![
                        discord_id,
                        Self::normalize_guild_id(guild_id),
                        i64::from(enabled),
                        updated_at,
                    ],
                )?;
                Ok(true)
            }
        }
    }

    pub fn delete_target(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, PushNotificationRepositoryError> {
        let changed = self.connection()?.execute(
            "DELETE FROM push_notification_targets WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(changed > 0)
    }

    /// Resolve ntfy delivery targets for whichever of `discord_ids` have
    /// `kind` enabled over ntfy and actually hold a topic. Order is
    /// unspecified; callers only need the filtered set.
    pub fn enabled_ntfy_targets(
        &self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
        kind: PushNotificationKind,
    ) -> Result<Vec<(i64, PushNotificationTarget)>, PushNotificationRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let column = kind.column(PushNotificationChannel::Ntfy);
        let placeholders = discord_ids
            .iter()
            .enumerate()
            .map(|(index, _)| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id,ntfy_topic FROM push_notification_targets
             WHERE guild_id=?1 AND ntfy_topic IS NOT NULL AND {column}=1
               AND discord_id IN ({placeholders})"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::ToSql>> = std::iter::once(Box::new(
            Self::normalize_guild_id(guild_id),
        )
            as Box<dyn rusqlite::ToSql>)
        .chain(
            discord_ids
                .iter()
                .copied()
                .map(|id| Box::new(id) as Box<dyn rusqlite::ToSql>),
        )
        .collect();
        let rows = statement.query_map(params_from_iter(params), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                PushNotificationTarget { topic: row.get(1)? },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Resolve which of `discord_ids` have `kind` enabled for direct-message
    /// delivery. No topic is required for this channel.
    pub fn enabled_dm_ids(
        &self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
        kind: PushNotificationKind,
    ) -> Result<Vec<i64>, PushNotificationRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let column = kind.column(PushNotificationChannel::DirectMessage);
        let placeholders = discord_ids
            .iter()
            .enumerate()
            .map(|(index, _)| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id FROM push_notification_targets
             WHERE guild_id=?1 AND {column}=1 AND discord_id IN ({placeholders})"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::ToSql>> = std::iter::once(Box::new(
            Self::normalize_guild_id(guild_id),
        )
            as Box<dyn rusqlite::ToSql>)
        .chain(
            discord_ids
                .iter()
                .copied()
                .map(|id| Box::new(id) as Box<dyn rusqlite::ToSql>),
        )
        .collect();
        let rows = statement.query_map(params_from_iter(params), |row| row.get::<_, i64>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
#[path = "push_notifications/tests.rs"]
mod tests;
