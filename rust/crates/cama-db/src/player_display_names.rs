//! Persistence for the last Discord display name seen for each player.
//!
//! Live names come from Discord; this table is the fallback when Discord
//! cannot provide one (a departed member, a failed lookup, or the moments
//! after startup before the member list arrives). Schema for
//! `player_display_names` is reconciled from `rust/schema/canonical_schema.sql`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior, params};

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerDisplayName {
    pub discord_id: i64,
    pub guild_id: i64,
    pub display_name: String,
}

#[derive(Clone, Debug)]
pub struct PlayerDisplayNameRepository {
    path: PathBuf,
}

impl PlayerDisplayNameRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Every stored name across all guilds. The table holds one row per
    /// registered player per guild, so callers load it whole at startup.
    pub fn load_all(&self) -> Result<Vec<PlayerDisplayName>, rusqlite::Error> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, guild_id, display_name FROM player_display_names
             ORDER BY guild_id, discord_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(PlayerDisplayName {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    display_name: row.get(2)?,
                })
            })?
            .collect()
    }

    /// Store names seen at `seen_at` (Unix milliseconds). A row already
    /// stamped later is kept, so writes that land out of order cannot
    /// replace a newer name with an older one.
    pub fn record(&self, names: &[PlayerDisplayName], seen_at: i64) -> Result<(), rusqlite::Error> {
        if names.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO player_display_names (discord_id, guild_id, display_name, seen_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (discord_id, guild_id) DO UPDATE SET
                     display_name = excluded.display_name,
                     seen_at = excluded.seen_at
                 WHERE excluded.seen_at >= player_display_names.seen_at",
            )?;
            for name in names {
                statement.execute(params![
                    name.discord_id,
                    name.guild_id,
                    name.display_name,
                    seen_at
                ])?;
            }
        }
        transaction.commit()
    }
}

#[cfg(test)]
#[path = "player_display_names/tests.rs"]
mod tests;
