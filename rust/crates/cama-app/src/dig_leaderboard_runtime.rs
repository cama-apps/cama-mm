//! Dig read models for leaderboard and collection parity.
//!
//! The adapter reads the migrated schema only and has no write path. Keeping
//! this projection separate from command rendering makes the Python service
//! contract directly testable and reusable by the production Dig provider.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::dig_loot::{Rarity, artifact_catalog};
use rusqlite::{Connection, params};
use thiserror::Error;

/// Errors returned by the read-only Dig leaderboard adapter.
#[derive(Debug, Error)]
pub enum DigLeaderboardRuntimeError {
    /// The migrated SQLite read failed.
    #[error("Dig leaderboard SQLite read failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// One tunnel row exposed by the Python leaderboard service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigLeaderboardTunnel {
    pub discord_id: i64,
    pub guild_id: i64,
    pub tunnel_name: Option<String>,
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub best_run_score: i64,
}

/// Exact shaping returned by `DigLeaderboardService.get_leaderboard`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigLeaderboardResult {
    pub tunnels: Vec<DigLeaderboardTunnel>,
    pub ascii_art: String,
}

/// One hall-of-fame entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHallOfFameEntry {
    pub discord_id: i64,
    pub tunnel_name: Option<String>,
    pub prestige_level: i64,
    pub best_run_score: i64,
}

/// Exact shaping returned by `DigLeaderboardService.get_hall_of_fame`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHallOfFameResult {
    pub success: bool,
    pub error: Option<String>,
    pub entries: Vec<DigHallOfFameEntry>,
}

/// One equipped relic row carried into `/dig info` rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigEquippedRelic {
    pub id: i64,
    pub artifact_id: String,
}

/// One persisted artifact with the catalog rarity resolved for collection UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigCollectionArtifact {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub artifact_id: String,
    pub found_at: i64,
    pub is_relic: bool,
    pub equipped: bool,
    pub rarity: String,
}

/// Exact shaping returned by `DigLeaderboardService.get_collection`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigCollectionResult {
    pub artifacts: BTreeMap<String, Vec<DigCollectionArtifact>>,
    pub total: usize,
}

/// A guild's most-active/deepest tunnel projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGuildLeader {
    pub discord_id: i64,
    pub name: Option<String>,
    pub total_digs: i64,
    pub depth: i64,
}

/// Exact shaping returned by `DigLeaderboardService.get_guild_stats`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGuildStatsResult {
    pub success: bool,
    pub error: Option<String>,
    pub total_digs: i64,
    pub total_depth: i64,
    pub total_jc_earned: i64,
    pub most_active: Option<DigGuildLeader>,
    pub deepest: Option<DigGuildLeader>,
    pub tunnel_count: usize,
}

/// Read-only migrated-schema adapter for the Dig leaderboard service.
#[derive(Clone, Debug)]
pub struct DigLeaderboardRuntime {
    path: PathBuf,
}

impl DigLeaderboardRuntime {
    /// Bind the read model to an existing migrated SQLite database.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        Ok(connection)
    }

    /// Return the top ten tunnels and the 40-block ASCII chart.
    pub fn leaderboard(
        &self,
        guild_id: i64,
    ) -> Result<DigLeaderboardResult, DigLeaderboardRuntimeError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,guild_id,tunnel_name,depth,total_digs,total_jc_earned,
                    prestige_level,best_run_score
             FROM tunnels
             WHERE guild_id=?1
             ORDER BY prestige_level DESC, depth DESC, discord_id ASC
             LIMIT 10",
        )?;
        let tunnels = statement
            .query_map(params![guild_id], |row| {
                Ok(DigLeaderboardTunnel {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    tunnel_name: row.get(2)?,
                    depth: row.get(3)?,
                    total_digs: row.get(4)?,
                    total_jc_earned: row.get(5)?,
                    prestige_level: row.get(6)?,
                    best_run_score: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let max_depth = tunnels
            .iter()
            .map(|tunnel| tunnel.depth)
            .max()
            .unwrap_or(1)
            .max(1);
        let ascii_art = tunnels
            .iter()
            .enumerate()
            .map(|(index, tunnel)| {
                let bar_len = (40_i64
                    .saturating_mul(tunnel.depth)
                    .checked_div(max_depth)
                    .unwrap_or(0))
                .max(1);
                let name = tunnel.tunnel_name.as_deref().unwrap_or("???");
                let name = name.chars().take(15).collect::<String>();
                format!(
                    "{:>2}. {:<15} {} {}m",
                    index + 1,
                    name,
                    "█".repeat(usize::try_from(bar_len).unwrap_or(1)),
                    tunnel.depth
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(DigLeaderboardResult { tunnels, ascii_art })
    }

    /// Return positive best-run scores in descending score order.
    pub fn hall_of_fame(
        &self,
        guild_id: i64,
    ) -> Result<DigHallOfFameResult, DigLeaderboardRuntimeError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,tunnel_name,prestige_level,best_run_score
             FROM tunnels
             WHERE guild_id=?1 AND best_run_score > 0
             ORDER BY best_run_score DESC
             LIMIT 10",
        )?;
        let entries = statement
            .query_map(params![guild_id], |row| {
                Ok(DigHallOfFameEntry {
                    discord_id: row.get(0)?,
                    tunnel_name: row.get(1)?,
                    prestige_level: row.get(2)?,
                    best_run_score: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DigHallOfFameResult {
            success: true,
            error: None,
            entries,
        })
    }

    /// Return the one-based position from the exact top-tunnel ordering.
    pub fn player_rank(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<usize>, DigLeaderboardRuntimeError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id FROM tunnels WHERE guild_id=?1
             ORDER BY prestige_level DESC, depth DESC, discord_id ASC",
        )?;
        Ok(statement
            .query_map(params![guild_id], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .position(|candidate| candidate == discord_id)
            .map(|index| index + 1))
    }

    /// Return equipped relic rows with the persisted artifact identifier that
    /// the info renderer needs for authored labels.
    pub fn equipped_relics(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<DigEquippedRelic>, DigLeaderboardRuntimeError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,artifact_id FROM dig_artifacts
             WHERE discord_id=?1 AND guild_id=?2 AND is_relic=1 AND equipped=1
             ORDER BY id",
        )?;
        Ok(statement
            .query_map(params![discord_id, guild_id], |row| {
                Ok(DigEquippedRelic {
                    id: row.get(0)?,
                    artifact_id: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Group one player's known artifacts by the authored catalog rarity.
    pub fn collection(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigCollectionResult, DigLeaderboardRuntimeError> {
        let rarity_by_id = artifact_catalog()
            .into_iter()
            .map(|artifact| (artifact.id, rarity_title(artifact.rarity)))
            .collect::<BTreeMap<_, _>>();
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,discord_id,guild_id,artifact_id,found_at,is_relic,equipped
             FROM dig_artifacts
             WHERE discord_id=?1 AND guild_id=?2
             ORDER BY id",
        )?;
        let mut artifacts = BTreeMap::<String, Vec<DigCollectionArtifact>>::new();
        for row in statement.query_map(params![discord_id, guild_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)? != 0,
                row.get::<_, i64>(6)? != 0,
            ))
        })? {
            let (id, owner, owner_guild, artifact_id, found_at, is_relic, equipped) = row?;
            let Some(rarity) = rarity_by_id.get(artifact_id.as_str()) else {
                continue;
            };
            artifacts
                .entry((*rarity).to_owned())
                .or_default()
                .push(DigCollectionArtifact {
                    id,
                    discord_id: owner,
                    guild_id: owner_guild,
                    artifact_id,
                    found_at,
                    is_relic,
                    equipped,
                    rarity: (*rarity).to_owned(),
                });
        }
        let total = artifacts.values().map(Vec::len).sum();
        Ok(DigCollectionResult { artifacts, total })
    }

    /// Aggregate tunnel totals and choose the first maximum, like Python's
    /// `max` over the repository's stable row order.
    pub fn guild_stats(
        &self,
        guild_id: i64,
    ) -> Result<DigGuildStatsResult, DigLeaderboardRuntimeError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,tunnel_name,depth,total_digs,total_jc_earned
             FROM tunnels WHERE guild_id=?1",
        )?;
        let tunnels = statement
            .query_map(params![guild_id], |row| {
                Ok((
                    DigGuildLeader {
                        discord_id: row.get(0)?,
                        name: row.get(1)?,
                        depth: row.get(2)?,
                        total_digs: row.get(3)?,
                    },
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if tunnels.is_empty() {
            return Ok(DigGuildStatsResult {
                success: true,
                error: None,
                total_digs: 0,
                total_depth: 0,
                total_jc_earned: 0,
                most_active: None,
                deepest: None,
                tunnel_count: 0,
            });
        }

        // The query above intentionally keeps the row order emitted by
        // SQLite.  `max` in Python retains the first row on ties, so `max_by`
        // is not used here.
        let mut total_digs = 0_i64;
        let mut total_depth = 0_i64;
        let mut total_jc_earned = 0_i64;
        let mut most_active = tunnels[0].0.clone();
        let mut deepest = tunnels[0].0.clone();
        for (tunnel, jc) in &tunnels {
            total_digs = total_digs.saturating_add(tunnel.total_digs);
            total_depth = total_depth.saturating_add(tunnel.depth);
            total_jc_earned = total_jc_earned.saturating_add(*jc);
            if tunnel.total_digs > most_active.total_digs {
                most_active = tunnel.clone();
            }
            if tunnel.depth > deepest.depth {
                deepest = tunnel.clone();
            }
        }
        Ok(DigGuildStatsResult {
            success: true,
            error: None,
            total_digs,
            total_depth,
            total_jc_earned,
            most_active: Some(most_active),
            deepest: Some(deepest),
            tunnel_count: tunnels.len(),
        })
    }
}

fn rarity_title(rarity: Rarity) -> &'static str {
    match rarity {
        Rarity::Common => "Common",
        Rarity::Uncommon => "Uncommon",
        Rarity::Rare => "Rare",
        Rarity::Legendary => "Legendary",
    }
}
