//! Existing-schema persistence for reminder preferences and one-shot lobby watches.
//!
//! Repository construction never creates or migrates schema. The runtime's
//! database admission owns migration before this adapter is published.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NotificationKind {
    Wheel,
    Trivia,
    Betting,
    Dig,
    Lobby,
    Pet,
}

impl NotificationKind {
    pub const ALL: [Self; 6] = [
        Self::Wheel,
        Self::Trivia,
        Self::Betting,
        Self::Dig,
        Self::Lobby,
        Self::Pet,
    ];

    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Wheel => "wheel_enabled",
            Self::Trivia => "trivia_enabled",
            Self::Betting => "betting_enabled",
            Self::Dig => "dig_enabled",
            Self::Lobby => "lobby_enabled",
            Self::Pet => "pet_enabled",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NotificationPreferences {
    pub wheel_enabled: bool,
    pub trivia_enabled: bool,
    pub betting_enabled: bool,
    pub dig_enabled: bool,
    pub lobby_enabled: bool,
    pub pet_enabled: bool,
}

impl NotificationPreferences {
    #[must_use]
    pub const fn enabled(self, kind: NotificationKind) -> bool {
        match kind {
            NotificationKind::Wheel => self.wheel_enabled,
            NotificationKind::Trivia => self.trivia_enabled,
            NotificationKind::Betting => self.betting_enabled,
            NotificationKind::Dig => self.dig_enabled,
            NotificationKind::Lobby => self.lobby_enabled,
            NotificationKind::Pet => self.pet_enabled,
        }
    }
}

#[derive(Debug, Error)]
pub enum NotificationRepositoryError {
    #[error("notification SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct NotificationRepository {
    path: PathBuf,
}

impl NotificationRepository {
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

    pub fn get_preferences(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<NotificationPreferences, NotificationRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT wheel_enabled,trivia_enabled,betting_enabled,
                        dig_enabled,lobby_enabled,pet_enabled
                   FROM reminder_preferences
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(NotificationPreferences {
                        wheel_enabled: row.get(0)?,
                        trivia_enabled: row.get(1)?,
                        betting_enabled: row.get(2)?,
                        dig_enabled: row.get(3)?,
                        lobby_enabled: row.get(4)?,
                        pet_enabled: row.get(5)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Persist one preference with Python's guild-scoped upsert semantics.
    pub fn set_preference(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        kind: NotificationKind,
        enabled: bool,
        updated_at: i64,
    ) -> Result<(), NotificationRepositoryError> {
        // `kind` is closed and the only source for this interpolated identifier.
        let column = kind.column();
        let sql = format!(
            "INSERT INTO reminder_preferences (discord_id,guild_id,{column},updated_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                 {column}=excluded.{column},updated_at=excluded.updated_at"
        );
        self.connection()?.execute(
            &sql,
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                i64::from(enabled),
                updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn enabled_users(
        &self,
        guild_id: Option<i64>,
        kind: NotificationKind,
    ) -> Result<Vec<i64>, NotificationRepositoryError> {
        let column = kind.column();
        let sql = format!(
            "SELECT discord_id FROM reminder_preferences
             WHERE guild_id=?1 AND {column}=1 ORDER BY discord_id"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([Self::normalize_guild_id(guild_id)], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Resolve several subscriber lists from one guild-scoped preference
    /// snapshot. Unknown kinds cannot enter this API because `NotificationKind`
    /// is closed, and duplicate kinds are collapsed by the map.
    pub fn enabled_users_bulk(
        &self,
        guild_id: Option<i64>,
        kinds: &[NotificationKind],
    ) -> Result<BTreeMap<NotificationKind, Vec<i64>>, NotificationRepositoryError> {
        let mut subscribers: BTreeMap<_, Vec<_>> = kinds
            .iter()
            .copied()
            .map(|kind| (kind, Vec::new()))
            .collect();
        if subscribers.is_empty() {
            return Ok(subscribers);
        }

        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,wheel_enabled,trivia_enabled,betting_enabled,
                    dig_enabled,lobby_enabled,pet_enabled
               FROM reminder_preferences
              WHERE guild_id=?1
              ORDER BY discord_id",
        )?;
        let rows = statement.query_map([Self::normalize_guild_id(guild_id)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                NotificationPreferences {
                    wheel_enabled: row.get(1)?,
                    trivia_enabled: row.get(2)?,
                    betting_enabled: row.get(3)?,
                    dig_enabled: row.get(4)?,
                    lobby_enabled: row.get(5)?,
                    pet_enabled: row.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (discord_id, preferences) = row?;
            for (kind, user_ids) in &mut subscribers {
                if preferences.enabled(*kind) {
                    user_ids.push(discord_id);
                }
            }
        }
        Ok(subscribers)
    }

    /// Insert a one-shot watch after the caller has proved DM deliverability.
    pub fn add_lobby_target_subscription(
        &self,
        subscriber_id: i64,
        target_id: i64,
        guild_id: Option<i64>,
        created_at_ns: i64,
    ) -> Result<bool, NotificationRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO lobby_target_subscriptions
                 (guild_id,target_id,subscriber_id,created_at)
             VALUES (?1,?2,?3,?4)",
            params![
                Self::normalize_guild_id(guild_id),
                target_id,
                subscriber_id,
                created_at_ns,
            ],
        )?;
        transaction.commit()?;
        Ok(inserted == 1)
    }

    pub fn has_lobby_target_subscription(
        &self,
        subscriber_id: i64,
        target_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, NotificationRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT 1 FROM lobby_target_subscriptions
                 WHERE guild_id=?1 AND target_id=?2 AND subscriber_id=?3",
                params![Self::normalize_guild_id(guild_id), target_id, subscriber_id,],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Atomically claim every watch that existed when the confirmed join committed.
    pub fn claim_lobby_target_subscribers(
        &self,
        target_id: i64,
        guild_id: Option<i64>,
        created_at_cutoff_ns: i64,
    ) -> Result<Vec<i64>, NotificationRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subscribers = {
            let mut statement = transaction.prepare(
                "SELECT subscriber_id FROM lobby_target_subscriptions
                 WHERE guild_id=?1 AND target_id=?2 AND created_at<=?3
                 ORDER BY created_at,subscriber_id",
            )?;
            let rows = statement
                .query_map(params![guild_id, target_id, created_at_cutoff_ns], |row| {
                    row.get(0)
                })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if !subscribers.is_empty() {
            transaction.execute(
                "DELETE FROM lobby_target_subscriptions
                 WHERE guild_id=?1 AND target_id=?2 AND created_at<=?3",
                params![guild_id, target_id, created_at_cutoff_ns],
            )?;
        }
        transaction.commit()?;
        Ok(subscribers)
    }
}
