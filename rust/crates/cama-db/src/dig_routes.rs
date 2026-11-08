//! Existing-schema persistence for persistent Dig routes.
//!
//! The Rust schema manager owns schema and migrations. This repository only
//! opens an already-migrated SQLite database
//! through [`crate::open_runtime_connection`]. Route choices and their audit
//! rows use `BEGIN IMMEDIATE` so stale Discord views cannot overwrite the first
//! committed result.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

/// Route-facing tunnel columns read from Python's `tunnels` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTunnelRecord {
    pub discord_id: i64,
    pub guild_id: i64,
    pub depth: i64,
    pub boss_progress: Option<String>,
    pub stinger_curse: Option<String>,
    pub route_state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteChoiceClaim {
    Claimed,
    Conflict {
        persisted_route_state: Option<String>,
    },
    MissingTunnel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteChoiceRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_route_state: &'a str,
    pub selected_route_state: &'a str,
    pub route_id: &'a str,
    pub layer: &'a str,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Debug, Error)]
pub enum DigRouteRepositoryError {
    #[error("dig route SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Python-compatible adapter over route-related tunnel state.
#[derive(Clone, Debug)]
pub struct DigRouteRepository {
    path: PathBuf,
}

impl DigRouteRepository {
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

    fn connection(&self) -> Result<crate::RuntimeConnection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RouteTunnelRecord>, DigRouteRepositoryError> {
        let connection = self.connection()?;
        query_tunnel(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    /// Claim a pending offer and write its audit row in one transaction.
    pub fn choose_route_atomic(
        &self,
        request: RouteChoiceRequest<'_>,
    ) -> Result<RouteChoiceClaim, DigRouteRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels SET route_state = ?1
             WHERE discord_id = ?2 AND guild_id = ?3 AND route_state = ?4",
            params![
                request.selected_route_state,
                request.discord_id,
                guild_id,
                request.expected_route_state
            ],
        )?;
        if changed == 0 {
            let persisted = query_route_state(&transaction, request.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(match persisted {
                Some(persisted_route_state) => RouteChoiceClaim::Conflict {
                    persisted_route_state,
                },
                None => RouteChoiceClaim::MissingTunnel,
            });
        }

        insert_action(
            &transaction,
            request.discord_id,
            guild_id,
            "route_choice",
            0,
            request.detail_json,
            request.now,
        )?;
        transaction.commit()?;
        Ok(RouteChoiceClaim::Claimed)
    }
}

fn query_tunnel(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<RouteTunnelRecord>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT discord_id, guild_id, depth, boss_progress,
                    stinger_curse, route_state
             FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| {
                Ok(RouteTunnelRecord {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    depth: row.get(2)?,
                    boss_progress: row.get(3)?,
                    stinger_curse: row.get(4)?,
                    route_state: row.get(5)?,
                })
            },
        )
        .optional()
}

fn query_route_state(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<Option<String>>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT route_state FROM tunnels
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn insert_action(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    action_type: &str,
    jc_delta: i64,
    detail_json: &str,
    now: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions
             (guild_id, actor_id, target_id, action_type, depth_before,
              depth_after, jc_delta, detail, created_at)
         VALUES (?1, ?2, NULL, ?3, 0, 0, ?4, ?5, ?6)",
        params![
            guild_id,
            discord_id,
            action_type,
            jc_delta,
            detail_json,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_routes/tests.rs"]
mod tests;
