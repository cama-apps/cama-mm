//! Existing-schema persistence for per-user ntfy push notification targets.
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

impl PushNotificationKind {
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Readycheck => "readycheck_enabled",
            Self::MatchStarted => "match_started_enabled",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushNotificationTarget {
    pub topic: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushNotificationConfig {
    pub target: PushNotificationTarget,
    pub readycheck_enabled: bool,
    pub match_started_enabled: bool,
}

impl PushNotificationConfig {
    #[must_use]
    pub const fn enabled(&self, kind: PushNotificationKind) -> bool {
        match kind {
            PushNotificationKind::Readycheck => self.readycheck_enabled,
            PushNotificationKind::MatchStarted => self.match_started_enabled,
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
                "SELECT ntfy_topic,readycheck_enabled,match_started_enabled
                   FROM push_notification_targets
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(PushNotificationConfig {
                        target: PushNotificationTarget { topic: row.get(0)? },
                        readycheck_enabled: row.get(1)?,
                        match_started_enabled: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert or replace the generated delivery topic. A new target enables
    /// both alert kinds; regenerating an existing topic preserves its toggles.
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

    /// Toggle one alert kind. Returns `false` when no target row exists yet.
    pub fn set_enabled(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        kind: PushNotificationKind,
        enabled: bool,
        updated_at: i64,
    ) -> Result<bool, PushNotificationRepositoryError> {
        // `kind` is closed and the only source for this interpolated identifier.
        let column = kind.column();
        let sql = format!(
            "UPDATE push_notification_targets SET {column}=?1,updated_at=?2
             WHERE discord_id=?3 AND guild_id=?4"
        );
        let changed = self.connection()?.execute(
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

    /// Resolve delivery targets for whichever of `discord_ids` have `kind`
    /// enabled. Order is unspecified; callers only need the filtered set.
    pub fn enabled_targets(
        &self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
        kind: PushNotificationKind,
    ) -> Result<Vec<(i64, PushNotificationTarget)>, PushNotificationRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let column = kind.column();
        let placeholders = discord_ids
            .iter()
            .enumerate()
            .map(|(index, _)| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id,ntfy_topic FROM push_notification_targets
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
        let rows = statement.query_map(params_from_iter(params), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                PushNotificationTarget { topic: row.get(1)? },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
#[path = "push_notifications/tests.rs"]
mod tests;
