//! Existing-schema candidate reads for Dig tunnel-encounter splashes.
//!
//! The Rust schema manager owns schema and migrations. This adapter opens only
//! an already-migrated database through [`crate::open_runtime_connection`] and
//! returns the deterministic victim pool for each splash strategy.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncounterVictimStrategy {
    RichestN,
    ActiveDiggers,
    RandomActive,
    DeepestN,
}

impl EncounterVictimStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RichestN => "richest_n",
            Self::ActiveDiggers => "active_diggers",
            Self::RandomActive => "random_active",
            Self::DeepestN => "deepest_n",
        }
    }

    #[must_use]
    pub const fn uses_random_sample(self) -> bool {
        matches!(self, Self::ActiveDiggers | Self::RandomActive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncounterCandidateQuery {
    pub guild_id: Option<i64>,
    pub digger_discord_id: i64,
    pub strategy: EncounterVictimStrategy,
    pub active_since: i64,
    pub limit: Option<usize>,
}

#[derive(Debug, Error)]
pub enum DigTunnelEncounterRepositoryError {
    #[error("encounter arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("Dig tunnel-encounter SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigTunnelEncounterRepository {
    path: PathBuf,
}

impl DigTunnelEncounterRepository {
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

    /// Read a deterministic candidate pool. Application entropy samples the
    /// two random strategies from it.
    pub fn candidate_ids(
        &self,
        query: EncounterCandidateQuery,
    ) -> Result<Vec<i64>, DigTunnelEncounterRepositoryError> {
        let connection = self.connection()?;
        candidate_ids_on(&connection, query)
    }
}

fn candidate_ids_on(
    connection: &Connection,
    query: EncounterCandidateQuery,
) -> Result<Vec<i64>, DigTunnelEncounterRepositoryError> {
    let guild_id = DigTunnelEncounterRepository::normalize_guild_id(query.guild_id);
    let limit = match query.limit {
        Some(limit) => i64::try_from(limit)
            .map_err(|_| DigTunnelEncounterRepositoryError::ArithmeticOverflow)?,
        None => -1,
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = match query.strategy {
        EncounterVictimStrategy::RichestN => {
            "SELECT discord_id
               FROM players
              WHERE guild_id = ?1 AND discord_id != ?2
                AND COALESCE(jopacoin_balance, 0) >= 1
              ORDER BY COALESCE(jopacoin_balance, 0) DESC, discord_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::ActiveDiggers => {
            "SELECT actions.actor_id
               FROM dig_actions AS actions
               JOIN players
                 ON players.guild_id = actions.guild_id
                AND players.discord_id = actions.actor_id
              WHERE actions.guild_id = ?1 AND actions.actor_id != ?2
                AND actions.action_type = 'dig' AND actions.created_at >= ?3
              GROUP BY actions.actor_id
              ORDER BY MAX(actions.created_at) DESC, actions.actor_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::RandomActive => {
            "SELECT discord_id
               FROM players
              WHERE guild_id = ?1 AND discord_id != ?2
                AND last_match_date IS NOT NULL
                AND CAST(strftime('%s', last_match_date) AS INTEGER) >= ?3
              ORDER BY discord_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::DeepestN => {
            "SELECT tunnels.discord_id
               FROM tunnels
               JOIN players
                 ON players.guild_id = tunnels.guild_id
                AND players.discord_id = tunnels.discord_id
              WHERE tunnels.guild_id = ?1 AND tunnels.discord_id != ?2
                AND COALESCE(tunnels.depth, 0) > 0
              ORDER BY COALESCE(tunnels.depth, 0) DESC, tunnels.discord_id
              LIMIT ?4"
        }
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![guild_id, query.digger_discord_id, query.active_since, limit],
        |row| row.get::<_, i64>(0),
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[cfg(test)]
#[path = "dig_tunnel_encounters/tests.rs"]
mod tests;
