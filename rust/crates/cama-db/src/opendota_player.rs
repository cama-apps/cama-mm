//! Existing-schema Steam-account persistence for OpenDota player lookups.
//!
//! The Rust runtime consumes the initialized canonical schema. Production
//! methods in this module only open an already-migrated database through
//! [`crate::open_runtime_connection`]. Every multi-statement write uses
//! `BEGIN IMMEDIATE`; DDL exists solely in the child test fixture.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{
    json_numeric::{coerce_integral_i64, number_f64},
    open_runtime_connection,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamLinkedPlayer {
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub steam_id: Option<i64>,
    pub dotabuff_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamIdBackfillCandidate {
    pub discord_id: i64,
    pub guild_id: i64,
    pub dotabuff_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BulkSteamIdLinkResult {
    pub success: bool,
    pub is_primary: bool,
    pub error: Option<String>,
}

#[derive(Debug, Error)]
pub enum OpenDotaPlayerRepositoryError {
    #[error("player {discord_id} does not exist")]
    MissingPlayer { discord_id: i64 },
    #[error("Steam ID {steam_id} is already linked to another player ({owner_id})")]
    SteamIdAlreadyLinked { steam_id: i64, owner_id: i64 },
    #[error("OpenDota player SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct OpenDotaPlayerRepository {
    path: PathBuf,
}

impl OpenDotaPlayerRepository {
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

    /// Return the global primary account used by the Python player service.
    ///
    /// The migrated junction table is authoritative. `players.steam_id` is a
    /// backward-compatible fallback for rows predating that table.
    pub fn get_steam_id(
        &self,
        discord_id: i64,
    ) -> Result<Option<i64>, OpenDotaPlayerRepositoryError> {
        let connection = self.connection()?;
        let primary = connection
            .query_row(
                "SELECT steam_id FROM player_steam_ids
                 WHERE discord_id=?1 AND is_primary=1
                 ORDER BY added_at,id LIMIT 1",
                params![discord_id],
                |row| row.get(0),
            )
            .optional()?;
        if primary.is_some() {
            return Ok(primary);
        }
        connection
            .query_row(
                "SELECT steam_id FROM players
                 WHERE discord_id=?1 AND steam_id IS NOT NULL
                 ORDER BY guild_id LIMIT 1",
                params![discord_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Return every globally linked account for a Discord identity, primary first.
    ///
    /// Any junction row makes the migrated table authoritative. The legacy
    /// column is consulted only for an identity with no junction memberships.
    pub fn get_steam_ids(
        &self,
        discord_id: i64,
    ) -> Result<Vec<i64>, OpenDotaPlayerRepositoryError> {
        let connection = self.connection()?;
        get_steam_ids(&connection, discord_id).map_err(Into::into)
    }

    /// Resolve many identities with one connection and junction-first fallback.
    ///
    /// Duplicate inputs are collapsed in first-seen order. `guild_id` scopes
    /// only the legacy fallback because junction memberships are global.
    pub fn get_steam_ids_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashMap<i64, Vec<i64>>, OpenDotaPlayerRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let unique_ids = deduplicate(discord_ids);
        let mut result = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, Vec::new()))
            .collect::<HashMap<_, _>>();
        let mut junction_found = HashSet::new();
        let connection = self.connection()?;

        for chunk in unique_ids.chunks(900) {
            let placeholders = repeat_placeholders(chunk.len());
            let mut statement = connection.prepare(&format!(
                "SELECT discord_id,steam_id FROM player_steam_ids
                 WHERE discord_id IN ({placeholders})
                 ORDER BY discord_id,is_primary DESC,added_at ASC,id ASC"
            ))?;
            let rows = statement.query_map(params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (discord_id, steam_id) = row?;
                result
                    .get_mut(&discord_id)
                    .expect("queried Discord ID was initialized")
                    .push(steam_id);
                junction_found.insert(discord_id);
            }
        }

        let missing = unique_ids
            .iter()
            .copied()
            .filter(|discord_id| !junction_found.contains(discord_id))
            .collect::<Vec<_>>();
        for chunk in missing.chunks(900) {
            let placeholders = repeat_placeholders(chunk.len());
            let mut parameters = chunk.to_vec();
            let guild_predicate = if let Some(guild_id) = guild_id {
                parameters.push(Self::normalize_guild_id(Some(guild_id)));
                " AND guild_id=?"
            } else {
                ""
            };
            let mut statement = connection.prepare(&format!(
                "SELECT discord_id,steam_id FROM players
                 WHERE discord_id IN ({placeholders}){guild_predicate}
                   AND steam_id IS NOT NULL
                 ORDER BY discord_id,guild_id"
            ))?;
            let rows = statement.query_map(params_from_iter(parameters.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (discord_id, steam_id) = row?;
                let steam_ids = result
                    .get_mut(&discord_id)
                    .expect("queried Discord ID was initialized");
                if steam_ids.is_empty() {
                    steam_ids.push(steam_id);
                }
            }
        }
        Ok(result)
    }

    /// Atomically update the compatibility column and global primary junction.
    ///
    /// This intentionally mirrors Python's cross-guild primary-account model:
    /// all rows for one Discord identity receive the same legacy value. Guild
    /// isolation is applied when resolving a Steam account back to a player.
    pub fn set_steam_id(
        &self,
        discord_id: i64,
        steam_id: i64,
        added_at: i64,
    ) -> Result<(), OpenDotaPlayerRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let membership_added = !has_exact_junction_membership(&transaction, discord_id, steam_id)?;
        let players = transaction.execute(
            "UPDATE players SET steam_id=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2",
            params![steam_id, discord_id],
        )?;
        if players == 0 {
            transaction.rollback()?;
            return Err(OpenDotaPlayerRepositoryError::MissingPlayer { discord_id });
        }
        transaction.execute(
            "UPDATE player_steam_ids SET is_primary=0 WHERE discord_id=?1",
            params![discord_id],
        )?;
        transaction.execute(
            "INSERT INTO player_steam_ids (discord_id,steam_id,is_primary,added_at)
             VALUES (?1,?2,1,?3)
             ON CONFLICT(discord_id,steam_id) DO UPDATE SET is_primary=1",
            params![discord_id, steam_id, added_at],
        )?;
        if membership_added {
            refresh_wrapped_enrichment_facts(&transaction, &[discord_id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Add or update one global account membership atomically.
    pub fn add_steam_id(
        &self,
        discord_id: i64,
        steam_id: i64,
        is_primary: bool,
        added_at: i64,
    ) -> Result<(), OpenDotaPlayerRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        reject_other_owner(&transaction, discord_id, steam_id)?;
        let membership_added = !has_exact_junction_membership(&transaction, discord_id, steam_id)?;

        if is_primary {
            transaction.execute(
                "UPDATE player_steam_ids SET is_primary=0 WHERE discord_id=?1",
                [discord_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO player_steam_ids (discord_id,steam_id,is_primary,added_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(discord_id,steam_id)
             DO UPDATE SET is_primary=excluded.is_primary",
            params![discord_id, steam_id, i64::from(is_primary), added_at],
        )?;
        if is_primary {
            transaction.execute(
                "UPDATE players SET steam_id=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2",
                params![steam_id, discord_id],
            )?;
        }
        if membership_added {
            refresh_wrapped_enrichment_facts(&transaction, &[discord_id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Link ordered candidates with one connection and one immediate transaction.
    ///
    /// Ownership conflicts produce aligned failures without preventing later
    /// candidates from succeeding. The first successful account for an
    /// identity with no effective account becomes primary.
    pub fn add_steam_ids_bulk(
        &self,
        steam_ids: &[(i64, i64)],
        added_at: i64,
    ) -> Result<Vec<BulkSteamIdLinkResult>, OpenDotaPlayerRepositoryError> {
        if steam_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(steam_ids.len());
        let mut affected_discord_ids = BTreeSet::new();
        for &(discord_id, steam_id) in steam_ids {
            if let Some(owner_id) = other_owner(&transaction, discord_id, steam_id)? {
                results.push(BulkSteamIdLinkResult {
                    success: false,
                    is_primary: false,
                    error: Some(format!(
                        "Steam ID {steam_id} is already linked to another player"
                    )),
                });
                debug_assert_ne!(owner_id, discord_id);
                continue;
            }

            let membership_added =
                !has_exact_junction_membership(&transaction, discord_id, steam_id)?;
            let is_primary = !has_effective_steam_id(&transaction, discord_id)?;
            if is_primary {
                transaction.execute(
                    "UPDATE player_steam_ids SET is_primary=0 WHERE discord_id=?1",
                    [discord_id],
                )?;
            }
            transaction.execute(
                "INSERT INTO player_steam_ids (discord_id,steam_id,is_primary,added_at)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(discord_id,steam_id)
                 DO UPDATE SET is_primary=excluded.is_primary",
                params![discord_id, steam_id, i64::from(is_primary), added_at],
            )?;
            if is_primary {
                transaction.execute(
                    "UPDATE players SET steam_id=?1,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2",
                    params![steam_id, discord_id],
                )?;
            }
            if membership_added {
                affected_discord_ids.insert(discord_id);
            }
            results.push(BulkSteamIdLinkResult {
                success: true,
                is_primary,
                error: None,
            });
        }
        let affected_discord_ids = affected_discord_ids.into_iter().collect::<Vec<_>>();
        refresh_wrapped_enrichment_facts(&transaction, &affected_discord_ids)?;
        transaction.commit()?;
        Ok(results)
    }

    /// Remove a membership and promote the oldest remaining account atomically.
    pub fn remove_steam_id(
        &self,
        discord_id: i64,
        steam_id: i64,
    ) -> Result<bool, OpenDotaPlayerRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let was_primary = transaction
            .query_row(
                "SELECT is_primary FROM player_steam_ids
                 WHERE discord_id=?1 AND steam_id=?2",
                params![discord_id, steam_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        let removed = transaction.execute(
            "DELETE FROM player_steam_ids WHERE discord_id=?1 AND steam_id=?2",
            params![discord_id, steam_id],
        )?;

        if removed == 0 {
            let legacy_removed = transaction.execute(
                "UPDATE players SET steam_id=NULL,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?1 AND steam_id=?2",
                params![discord_id, steam_id],
            )?;
            if legacy_removed > 0 {
                refresh_wrapped_enrichment_facts(&transaction, &[discord_id])?;
            }
            transaction.commit()?;
            return Ok(legacy_removed > 0);
        }

        if was_primary == Some(true) {
            let next = transaction
                .query_row(
                    "SELECT steam_id FROM player_steam_ids
                     WHERE discord_id=?1 ORDER BY added_at ASC,id ASC LIMIT 1",
                    [discord_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(next) = next {
                transaction.execute(
                    "UPDATE player_steam_ids SET is_primary=1
                     WHERE discord_id=?1 AND steam_id=?2",
                    params![discord_id, next],
                )?;
                transaction.execute(
                    "UPDATE players SET steam_id=?1,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2",
                    params![next, discord_id],
                )?;
            } else {
                transaction.execute(
                    "UPDATE players SET steam_id=NULL,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?1",
                    [discord_id],
                )?;
            }
        } else if !has_junction_membership(&transaction, discord_id)? {
            transaction.execute(
                "UPDATE players SET steam_id=NULL,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?1 AND steam_id=?2",
                params![discord_id, steam_id],
            )?;
        }
        refresh_wrapped_enrichment_facts(&transaction, &[discord_id])?;
        transaction.commit()?;
        Ok(true)
    }

    /// Promote an existing membership and update every legacy guild row.
    pub fn set_primary_steam_id(
        &self,
        discord_id: i64,
        steam_id: i64,
    ) -> Result<bool, OpenDotaPlayerRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let linked = transaction
            .query_row(
                "SELECT 1 FROM player_steam_ids
                 WHERE discord_id=?1 AND steam_id=?2",
                params![discord_id, steam_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !linked {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE player_steam_ids SET is_primary=0 WHERE discord_id=?1",
            [discord_id],
        )?;
        transaction.execute(
            "UPDATE player_steam_ids SET is_primary=1
             WHERE discord_id=?1 AND steam_id=?2",
            params![discord_id, steam_id],
        )?;
        transaction.execute(
            "UPDATE players SET steam_id=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2",
            params![steam_id, discord_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Return the global owner from the junction table, then legacy fallback.
    pub fn get_steam_id_owner(
        &self,
        steam_id: i64,
    ) -> Result<Option<i64>, OpenDotaPlayerRepositoryError> {
        let connection = self.connection()?;
        steam_id_owner(&connection, steam_id).map_err(Into::into)
    }

    /// Resolve only migrated junction membership in one normalized guild.
    pub fn get_player_by_any_steam_id(
        &self,
        steam_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<SteamLinkedPlayer>, OpenDotaPlayerRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT p.discord_id,p.guild_id,p.discord_username,
                        p.steam_id,p.dotabuff_url
                 FROM players p
                 JOIN player_steam_ids psi ON p.discord_id=psi.discord_id
                 WHERE psi.steam_id=?1 AND p.guild_id=?2
                 ORDER BY p.discord_id LIMIT 1",
                params![steam_id, Self::normalize_guild_id(guild_id)],
                player_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Resolve any linked account to the corresponding player in one guild.
    pub fn get_by_steam_id(
        &self,
        steam_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<SteamLinkedPlayer>, OpenDotaPlayerRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let linked = self.get_player_by_any_steam_id(steam_id, Some(guild_id))?;
        if linked.is_some() {
            return Ok(linked);
        }
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT discord_id,guild_id,discord_username,steam_id,dotabuff_url
                 FROM players WHERE steam_id=?1 AND guild_id=?2
                 ORDER BY discord_id LIMIT 1",
                params![steam_id, guild_id],
                player_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Return legacy backfill candidates exactly as selected by Python.
    pub fn get_all_with_dotabuff_no_steam_id(
        &self,
    ) -> Result<Vec<SteamIdBackfillCandidate>, OpenDotaPlayerRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,guild_id,dotabuff_url
             FROM players
             WHERE dotabuff_url IS NOT NULL AND dotabuff_url!=''
               AND steam_id IS NULL
             ORDER BY guild_id,discord_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(SteamIdBackfillCandidate {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    dotabuff_url: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[derive(Debug)]
struct AffectedWrappedMatch {
    enrichment_data: String,
    participants: BTreeMap<i64, Option<i64>>,
}

/// Rebuild only the compact historical facts whose Steam membership changed.
///
/// The caller owns the surrounding immediate transaction, so deleting stale
/// attribution, changing memberships, parsing shared matches, and inserting
/// replacement facts either commit together or roll back together. Junction
/// rows are global and authoritative; a guild-scoped legacy ID is used only
/// when an identity has no junction membership.
fn refresh_wrapped_enrichment_facts(
    transaction: &rusqlite::Transaction<'_>,
    discord_ids: &[i64],
) -> Result<usize, rusqlite::Error> {
    let discord_ids = deduplicate(discord_ids);
    if discord_ids.is_empty() {
        return Ok(0);
    }

    let mut steam_ids_by_discord = discord_ids
        .iter()
        .copied()
        .map(|discord_id| (discord_id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut affected_matches = BTreeMap::<(i64, i64), AffectedWrappedMatch>::new();

    for chunk in discord_ids.chunks(900) {
        let placeholders = repeat_placeholders(chunk.len());
        transaction.execute(
            &format!(
                "DELETE FROM wrapped_enrichment_facts
                  WHERE discord_id IN ({placeholders})"
            ),
            params_from_iter(chunk.iter()),
        )?;

        let mut statement = transaction.prepare(&format!(
            "SELECT discord_id,steam_id FROM player_steam_ids
              WHERE discord_id IN ({placeholders})"
        ))?;
        let memberships = statement.query_map(params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for membership in memberships {
            let (discord_id, steam_id) = membership?;
            steam_ids_by_discord
                .get_mut(&discord_id)
                .expect("affected Discord ID was initialized")
                .insert(steam_id);
        }
        drop(statement);

        let mut statement = transaction.prepare(&format!(
            "SELECT m.guild_id,m.match_id,m.enrichment_data,
                    mp.discord_id,p.steam_id
               FROM match_participants AS mp
               JOIN matches AS m
                 ON m.match_id=mp.match_id AND m.guild_id=mp.guild_id
               LEFT JOIN players AS p
                 ON p.discord_id=mp.discord_id AND p.guild_id=mp.guild_id
              WHERE mp.discord_id IN ({placeholders})
                AND m.enrichment_data IS NOT NULL
              ORDER BY m.match_id,mp.discord_id"
        ))?;
        let rows = statement.query_map(params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        for row in rows {
            let (guild_id, match_id, enrichment_data, discord_id, legacy_steam_id) = row?;
            affected_matches
                .entry((guild_id, match_id))
                .or_insert_with(|| AffectedWrappedMatch {
                    enrichment_data,
                    participants: BTreeMap::new(),
                })
                .participants
                .insert(discord_id, legacy_steam_id);
        }
    }

    let mut parsed_matches = 0;
    for ((guild_id, match_id), affected) in affected_matches {
        parsed_matches += 1;
        let Ok(JsonValue::Object(match_data)) =
            serde_json::from_str::<JsonValue>(&affected.enrichment_data)
        else {
            continue;
        };
        let players = match_data
            .get("players")
            .and_then(JsonValue::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        for (discord_id, legacy_steam_id) in affected.participants {
            let linked = steam_ids_by_discord
                .get(&discord_id)
                .expect("affected Discord ID was initialized");
            let player = players.iter().find(|candidate| {
                candidate
                    .as_object()
                    .and_then(|fields| fields.get("account_id"))
                    .and_then(coerce_integral_i64)
                    .is_some_and(|account_id| {
                        linked.contains(&account_id)
                            || (linked.is_empty() && legacy_steam_id == Some(account_id))
                    })
            });
            let player = player.and_then(JsonValue::as_object);
            let rapier_count = player
                .and_then(|fields| fields.get("purchase_log"))
                .and_then(JsonValue::as_array)
                .map(|purchases| {
                    purchases
                        .iter()
                        .filter(|purchase| {
                            purchase
                                .as_object()
                                .and_then(|fields| fields.get("key"))
                                .and_then(JsonValue::as_str)
                                == Some("rapier")
                        })
                        .count()
                })
                .and_then(|count| i64::try_from(count).ok())
                .unwrap_or_default();

            transaction.execute(
                "INSERT INTO wrapped_enrichment_facts (
                     guild_id,match_id,discord_id,actions_per_min,
                     courier_kills,pings,rapier_count,lane_role,comeback,throw
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    guild_id,
                    match_id,
                    discord_id,
                    player
                        .and_then(|fields| fields.get("actions_per_min"))
                        .and_then(JsonValue::as_i64),
                    player
                        .and_then(|fields| fields.get("courier_kills"))
                        .and_then(JsonValue::as_i64),
                    player
                        .and_then(|fields| fields.get("pings"))
                        .and_then(JsonValue::as_i64),
                    rapier_count,
                    player
                        .and_then(|fields| fields.get("lane_role"))
                        .and_then(JsonValue::as_i64),
                    number_f64(match_data.get("comeback")),
                    number_f64(match_data.get("throw")),
                ],
            )?;
        }
    }
    Ok(parsed_matches)
}

fn get_steam_ids(connection: &Connection, discord_id: i64) -> Result<Vec<i64>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT steam_id FROM player_steam_ids
         WHERE discord_id=?1 ORDER BY is_primary DESC,added_at ASC,id ASC",
    )?;
    let junction = statement
        .query_map([discord_id], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if !junction.is_empty() {
        return Ok(junction);
    }
    connection
        .query_row(
            "SELECT steam_id FROM players
             WHERE discord_id=?1 AND steam_id IS NOT NULL
             ORDER BY guild_id LIMIT 1",
            [discord_id],
            |row| row.get(0),
        )
        .optional()
        .map(|steam_id| steam_id.into_iter().collect())
}

fn steam_id_owner(connection: &Connection, steam_id: i64) -> Result<Option<i64>, rusqlite::Error> {
    let junction = connection
        .query_row(
            "SELECT discord_id FROM player_steam_ids WHERE steam_id=?1",
            [steam_id],
            |row| row.get(0),
        )
        .optional()?;
    if junction.is_some() {
        return Ok(junction);
    }
    connection
        .query_row(
            "SELECT discord_id FROM players WHERE steam_id=?1
             ORDER BY guild_id,discord_id LIMIT 1",
            [steam_id],
            |row| row.get(0),
        )
        .optional()
}

fn other_owner(
    connection: &Connection,
    discord_id: i64,
    steam_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    let junction_owner = connection
        .query_row(
            "SELECT discord_id FROM player_steam_ids WHERE steam_id=?1",
            [steam_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if junction_owner.is_some_and(|owner| owner != discord_id) {
        return Ok(junction_owner);
    }
    connection
        .query_row(
            "SELECT discord_id FROM players
             WHERE steam_id=?1 AND discord_id!=?2
             ORDER BY guild_id,discord_id LIMIT 1",
            params![steam_id, discord_id],
            |row| row.get(0),
        )
        .optional()
}

fn reject_other_owner(
    connection: &Connection,
    discord_id: i64,
    steam_id: i64,
) -> Result<(), OpenDotaPlayerRepositoryError> {
    if let Some(owner_id) = other_owner(connection, discord_id, steam_id)? {
        return Err(OpenDotaPlayerRepositoryError::SteamIdAlreadyLinked { steam_id, owner_id });
    }
    Ok(())
}

fn has_junction_membership(
    connection: &Connection,
    discord_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM player_steam_ids WHERE discord_id=?1 LIMIT 1",
            [discord_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn has_exact_junction_membership(
    connection: &Connection,
    discord_id: i64,
    steam_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM player_steam_ids
             WHERE discord_id=?1 AND steam_id=?2",
            params![discord_id, steam_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn has_effective_steam_id(
    connection: &Connection,
    discord_id: i64,
) -> Result<bool, rusqlite::Error> {
    if has_junction_membership(connection, discord_id)? {
        return Ok(true);
    }
    connection
        .query_row(
            "SELECT 1 FROM players
             WHERE discord_id=?1 AND steam_id IS NOT NULL LIMIT 1",
            [discord_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn deduplicate(values: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn player_from_row(row: &rusqlite::Row<'_>) -> Result<SteamLinkedPlayer, rusqlite::Error> {
    Ok(SteamLinkedPlayer {
        discord_id: row.get(0)?,
        guild_id: row.get(1)?,
        name: row.get(2)?,
        steam_id: row.get(3)?,
        dotabuff_url: row.get(4)?,
    })
}

#[cfg(test)]
mod tests;
