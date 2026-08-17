//! Persistence for named per-player curfew windows.
//!
//! This adapter opens the existing SQLite database through
//! [`crate::open_runtime_connection`]; schema for `player_curfew_windows` is
//! reconciled from `rust/schema/canonical_schema.sql`. Ports
//! `repositories/curfew_repository.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_domain::curfew::CurfewWindow;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::open_runtime_connection;

#[derive(Clone, Debug)]
pub struct CurfewRepository {
    path: PathBuf,
}

impl CurfewRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Create a named window, or overwrite it if that name already exists for this player.
    pub fn add_or_replace(&self, window: &CurfewWindow) -> Result<(), rusqlite::Error> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO player_curfew_windows
                (discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(discord_id, guild_id, name) DO UPDATE SET
                start_hour = excluded.start_hour,
                start_minute = excluded.start_minute,
                end_hour = excluded.end_hour,
                end_minute = excluded.end_minute,
                timezone = excluded.timezone,
                days = excluded.days",
            params![
                window.discord_id,
                window.guild_id,
                window.name,
                window.start_hour,
                window.start_minute,
                window.end_hour,
                window.end_minute,
                window.timezone,
                window.days.map(i64::from),
            ],
        )?;
        Ok(())
    }

    /// Delete a named window. Returns `true` if a row was actually removed.
    pub fn remove(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
    ) -> Result<bool, rusqlite::Error> {
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM player_curfew_windows WHERE discord_id = ?1 AND guild_id = ?2 AND name = ?3",
            params![discord_id, guild_id, name],
        )?;
        Ok(removed > 0)
    }

    pub fn list_for_player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<CurfewWindow>, rusqlite::Error> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days
             FROM player_curfew_windows
             WHERE discord_id = ?1 AND guild_id = ?2
             ORDER BY name",
        )?;
        let rows = statement
            .query_map(params![discord_id, guild_id], row_to_window)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Bulk-fetch windows for a set of players (e.g. everyone in a lobby).
    pub fn list_for_players(
        &self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeMap<i64, Vec<CurfewWindow>>, rusqlite::Error> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.connection()?;
        let placeholders = discord_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days
             FROM player_curfew_windows
             WHERE guild_id = ? AND discord_id IN ({placeholders})"
        );
        let mut statement = connection.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = vec![guild_id.into()];
        params.extend(discord_ids.iter().map(|id| (*id).into()));
        let rows = statement
            .query_map(params_from_iter(params), row_to_window)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut result: BTreeMap<i64, Vec<CurfewWindow>> = BTreeMap::new();
        for window in rows {
            result.entry(window.discord_id).or_default().push(window);
        }
        Ok(result)
    }

    /// Read a player's general timezone preference from `players.timezone`.
    pub fn general_timezone(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, rusqlite::Error> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT timezone FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
    }

    /// Whether a player row exists (mirrors `PlayerRepository.get_by_id` presence check).
    pub fn player_exists(&self, discord_id: i64, guild_id: i64) -> Result<bool, rusqlite::Error> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
    }
}

fn row_to_window(row: &rusqlite::Row<'_>) -> Result<CurfewWindow, rusqlite::Error> {
    Ok(CurfewWindow {
        discord_id: row.get(0)?,
        guild_id: row.get(1)?,
        name: row.get(2)?,
        start_hour: row.get(3)?,
        start_minute: row.get(4)?,
        end_hour: row.get(5)?,
        end_minute: row.get(6)?,
        timezone: row.get(7)?,
        days: row.get::<_, Option<i64>>(8)?.and_then(valid_day_mask),
    })
}

/// Day masks are only ever written by `parse_weekdays`, which guarantees a
/// value in `1..=0b0111_1111`. Anything else in the column is corruption, so
/// fall back to the documented every-day default (`None`) rather than
/// truncating into a mask that silently matches no day at all — a window that
/// never fires again but still renders like an every-day one.
fn valid_day_mask(value: i64) -> Option<u8> {
    u8::try_from(value)
        .ok()
        .filter(|mask| *mask != 0 && *mask & !0b0111_1111 == 0)
}

#[cfg(test)]
#[path = "curfew/tests.rs"]
mod tests;
