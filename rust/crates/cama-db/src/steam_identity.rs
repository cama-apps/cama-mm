//! Global Steam account-link lookups shared by repository slices.
//!
//! Steam identity is intentionally global: `player_steam_ids` memberships are
//! not guild-scoped. The legacy `players.steam_id` compatibility column is
//! consulted only for Discord identities without any junction row.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params_from_iter};

const CHUNK_SIZE: usize = 900;

/// Resolve the Discord identity that owns a Steam account.
///
/// The junction wins; otherwise the lowest Discord id holding the account in
/// the legacy Dota column or the Deadlock roster is returned.
pub fn steam_id_owner(
    connection: &Connection,
    steam_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
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
             UNION SELECT discord_id FROM deadlock_players WHERE steam_id=?1 LIMIT 1",
            [steam_id],
            |row| row.get(0),
        )
        .optional()
}

/// Resolve many identities with one connection and junction-first fallback.
///
/// Every input id is present in the result, with its linked accounts primary
/// first, then oldest first, ties broken by junction row id. `guild_id`
/// scopes only the legacy fallback; without one the lowest guild id's legacy
/// value is used so the pick is deterministic.
pub fn steam_ids_bulk(
    connection: &Connection,
    discord_ids: &[i64],
    guild_id: Option<i64>,
) -> Result<HashMap<i64, Vec<i64>>, rusqlite::Error> {
    let mut seen = HashSet::new();
    let unique_ids = discord_ids
        .iter()
        .copied()
        .filter(|discord_id| seen.insert(*discord_id))
        .collect::<Vec<_>>();
    let mut result = unique_ids
        .iter()
        .copied()
        .map(|discord_id| (discord_id, Vec::new()))
        .collect::<HashMap<_, _>>();
    let mut junction_found = HashSet::new();

    for chunk in unique_ids.chunks(CHUNK_SIZE) {
        let mut statement = connection.prepare(&format!(
            "SELECT discord_id,steam_id FROM player_steam_ids
             WHERE discord_id IN ({})
             ORDER BY discord_id,is_primary DESC,added_at ASC,id ASC",
            placeholders(chunk.len())
        ))?;
        let rows = statement.query_map(params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (discord_id, steam_id) = row?;
            result.entry(discord_id).or_default().push(steam_id);
            junction_found.insert(discord_id);
        }
    }

    let missing = unique_ids
        .iter()
        .copied()
        .filter(|discord_id| !junction_found.contains(discord_id))
        .collect::<Vec<_>>();
    for chunk in missing.chunks(CHUNK_SIZE) {
        let mut parameters = chunk.to_vec();
        let guild_predicate = if let Some(guild_id) = guild_id {
            parameters.push(guild_id);
            " AND guild_id=?"
        } else {
            ""
        };
        let mut statement = connection.prepare(&format!(
            "SELECT discord_id,steam_id FROM players
             WHERE discord_id IN ({}){guild_predicate}
               AND steam_id IS NOT NULL
             ORDER BY discord_id,guild_id",
            placeholders(chunk.len())
        ))?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (discord_id, steam_id) = row?;
            let steam_ids = result.entry(discord_id).or_default();
            if steam_ids.is_empty() {
                steam_ids.push(steam_id);
            }
        }
    }
    Ok(result)
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    fn database() -> (NamedTempFile, crate::RuntimeConnection) {
        let file = NamedTempFile::new().expect("temporary database");
        crate::schema_manager::initialize_or_migrate(file.path()).expect("migrate");
        let connection = crate::open_runtime_connection(file.path()).expect("connection");
        (file, connection)
    }

    #[test]
    fn junction_ties_break_on_row_id_and_legacy_fallback_uses_the_lowest_guild() {
        let (_file, connection) = database();
        connection
            .execute_batch(
                "INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at)
                 VALUES (1,30,0,100),(1,10,0,100),(1,20,1,200);
                 INSERT INTO players(discord_id,guild_id,discord_username,steam_id)
                 VALUES (2,9,'later-guild',92),(2,4,'earlier-guild',42),(3,5,'scoped',55);",
            )
            .unwrap();
        let unscoped = steam_ids_bulk(&connection, &[2, 1, 2, 7], None).unwrap();
        assert_eq!(
            unscoped[&1],
            vec![20, 30, 10],
            "primary, then oldest, then row id"
        );
        assert_eq!(
            unscoped[&2],
            vec![42],
            "lowest guild wins without a guild scope"
        );
        assert!(unscoped[&7].is_empty());
        assert_eq!(unscoped.len(), 3);
        let scoped = steam_ids_bulk(&connection, &[2, 3], Some(9)).unwrap();
        assert_eq!(scoped[&2], vec![92]);
        assert!(
            scoped[&3].is_empty(),
            "another guild's legacy value is not borrowed"
        );
    }

    #[test]
    fn owner_prefers_the_junction_then_the_lowest_legacy_identity() {
        let (_file, connection) = database();
        connection
            .execute_batch(
                "INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at)
                 VALUES (8,70,1,1);
                 INSERT INTO players(discord_id,guild_id,discord_username,steam_id)
                 VALUES (9,1,'legacy',70),(6,1,'legacy',71);
                 INSERT INTO deadlock_players(guild_id,discord_id,steam_id,created_at)
                 VALUES (1,5,71,0),(1,4,72,0);",
            )
            .unwrap();
        assert_eq!(steam_id_owner(&connection, 70).unwrap(), Some(8));
        assert_eq!(steam_id_owner(&connection, 71).unwrap(), Some(5));
        assert_eq!(steam_id_owner(&connection, 72).unwrap(), Some(4));
        assert_eq!(steam_id_owner(&connection, 73).unwrap(), None);
    }
}
