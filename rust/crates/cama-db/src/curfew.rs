//! Persistence for named per-player curfew windows.
//!
//! This adapter opens the existing SQLite database through
//! [`crate::open_runtime_connection`]; schema for `player_curfew_windows`
//! and `player_curfew_pending_changes` is reconciled from `rust/schema/canonical_schema.sql`. Ports
//! `repositories/curfew_repository.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_domain::curfew::{CurfewWindow, parse_mode};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};

use crate::open_runtime_connection;

#[derive(Clone, Debug)]
pub struct CurfewRepository {
    path: PathBuf,
}

/// A strict-mode window edit or delete staged to take effect at
/// [`Self::effective_at`] rather than immediately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCurfewChange {
    /// `Some` — the window will become this. `None` — the window will be
    /// deleted.
    pub window: Option<CurfewWindow>,
    pub effective_at: DateTime<Utc>,
}

/// A pending change that has just been committed by
/// [`CurfewRepository::apply_due_pending_changes`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedPendingCurfewChange {
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    /// `Some` — the window is now this. `None` — the window was deleted.
    pub window: Option<CurfewWindow>,
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
        upsert_window(&self.connection()?, window)
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

    /// Read a single named window, if it exists.
    pub fn get_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
    ) -> Result<Option<CurfewWindow>, rusqlite::Error> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days, mode
                 FROM player_curfew_windows WHERE discord_id = ?1 AND guild_id = ?2 AND name = ?3",
                params![discord_id, guild_id, name],
                row_to_window,
            )
            .optional()
    }

    pub fn list_for_player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<CurfewWindow>, rusqlite::Error> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days, mode
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
            "SELECT discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days, mode
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

    /// Stage a strict-mode edit: the window itself is left untouched (so it
    /// keeps enforcing under its current settings) and `window` takes over
    /// at `effective_at`.
    pub fn stage_pending_upsert(
        &self,
        window: &CurfewWindow,
        effective_at: DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO player_curfew_pending_changes
                (discord_id, guild_id, name, action, start_hour, start_minute, end_hour, end_minute, timezone, days, mode, effective_at)
             VALUES (?1, ?2, ?3, 'upsert', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(discord_id, guild_id, name) DO UPDATE SET
                action = 'upsert',
                start_hour = excluded.start_hour,
                start_minute = excluded.start_minute,
                end_hour = excluded.end_hour,
                end_minute = excluded.end_minute,
                timezone = excluded.timezone,
                days = excluded.days,
                mode = excluded.mode,
                effective_at = excluded.effective_at",
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
                window.mode.as_str(),
                effective_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Stage a strict-mode delete: the window stays live (and enforced)
    /// until `effective_at`, when it's actually removed.
    pub fn stage_pending_delete(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
        effective_at: DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO player_curfew_pending_changes (discord_id, guild_id, name, action, effective_at)
             VALUES (?1, ?2, ?3, 'delete', ?4)
             ON CONFLICT(discord_id, guild_id, name) DO UPDATE SET
                action = 'delete',
                start_hour = NULL, start_minute = NULL, end_hour = NULL, end_minute = NULL,
                timezone = NULL, days = NULL, mode = NULL,
                effective_at = excluded.effective_at",
            params![discord_id, guild_id, name, effective_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn pending_changes_for_player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<BTreeMap<String, PendingCurfewChange>, rusqlite::Error> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {PENDING_CHANGE_COLUMNS} FROM player_curfew_pending_changes
             WHERE discord_id = ?1 AND guild_id = ?2"
        ))?;
        let rows = statement
            .query_map(params![discord_id, guild_id], row_to_pending_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|row| (row.name, row.change)).collect())
    }

    /// Commit every pending change whose `effective_at` has arrived. Applies
    /// each staged upsert or delete to `player_curfew_windows` and clears
    /// its pending row, atomically.
    pub fn apply_due_pending_changes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppliedPendingCurfewChange>, rusqlite::Error> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let due = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {PENDING_CHANGE_COLUMNS} FROM player_curfew_pending_changes
                 WHERE effective_at <= ?1"
            ))?;
            statement
                .query_map(params![now.to_rfc3339()], row_to_pending_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut applied = Vec::with_capacity(due.len());
        for row in due {
            match &row.change.window {
                Some(window) => upsert_window(&transaction, window)?,
                None => {
                    transaction.execute(
                        "DELETE FROM player_curfew_windows WHERE discord_id=?1 AND guild_id=?2 AND name=?3",
                        params![row.discord_id, row.guild_id, &row.name],
                    )?;
                }
            }
            transaction.execute(
                "DELETE FROM player_curfew_pending_changes WHERE discord_id=?1 AND guild_id=?2 AND name=?3",
                params![row.discord_id, row.guild_id, &row.name],
            )?;
            applied.push(AppliedPendingCurfewChange {
                discord_id: row.discord_id,
                guild_id: row.guild_id,
                name: row.name,
                window: row.change.window,
            });
        }
        transaction.commit()?;
        Ok(applied)
    }
}

/// Insert `window`, or overwrite the row with the same player and name.
/// Shared by the direct write and by committing a staged edit, so both
/// land exactly the same columns.
fn upsert_window(connection: &Connection, window: &CurfewWindow) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO player_curfew_windows
            (discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone, days, mode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(discord_id, guild_id, name) DO UPDATE SET
            start_hour = excluded.start_hour,
            start_minute = excluded.start_minute,
            end_hour = excluded.end_hour,
            end_minute = excluded.end_minute,
            timezone = excluded.timezone,
            days = excluded.days,
            mode = excluded.mode",
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
            window.mode.as_str(),
        ],
    )?;
    Ok(())
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
        mode: row
            .get::<_, String>(9)
            .ok()
            .and_then(|value| parse_mode(&value).ok())
            .unwrap_or_default(),
    })
}

/// The columns [`row_to_pending_row`] reads, by name, from
/// `player_curfew_pending_changes`.
const PENDING_CHANGE_COLUMNS: &str = "discord_id, guild_id, name, action, start_hour, start_minute, \
     end_hour, end_minute, timezone, days, mode, effective_at";

/// One `player_curfew_pending_changes` row: the change plus the key that
/// says which window it belongs to.
struct PendingRow {
    discord_id: i64,
    guild_id: i64,
    name: String,
    change: PendingCurfewChange,
}

fn row_to_pending_row(row: &rusqlite::Row<'_>) -> Result<PendingRow, rusqlite::Error> {
    let discord_id: i64 = row.get("discord_id")?;
    let guild_id: i64 = row.get("guild_id")?;
    let name: String = row.get("name")?;
    let action: String = row.get("action")?;
    let effective_at: String = row.get("effective_at")?;
    let effective_at = DateTime::parse_from_rfc3339(&effective_at)
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let window = if action == "delete" {
        None
    } else {
        Some(CurfewWindow {
            discord_id,
            guild_id,
            name: name.clone(),
            start_hour: row.get::<_, Option<u32>>("start_hour")?.unwrap_or(0),
            start_minute: row.get::<_, Option<u32>>("start_minute")?.unwrap_or(0),
            end_hour: row.get::<_, Option<u32>>("end_hour")?.unwrap_or(0),
            end_minute: row.get::<_, Option<u32>>("end_minute")?.unwrap_or(0),
            timezone: row.get("timezone")?,
            days: row.get::<_, Option<i64>>("days")?.and_then(valid_day_mask),
            mode: row
                .get::<_, Option<String>>("mode")?
                .and_then(|value| parse_mode(&value).ok())
                .unwrap_or_default(),
        })
    };
    Ok(PendingRow {
        discord_id,
        guild_id,
        name,
        change: PendingCurfewChange {
            window,
            effective_at,
        },
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
