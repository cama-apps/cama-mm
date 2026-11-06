//! Read-only discovery of registered identities, including departed Discord accounts.
use cama_domain::dota_lobby::account_id;
use rusqlite::params;

use super::{AdminRepository, AdminRepositoryError, open_runtime_connection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminAccountLookup {
    pub discord_id: i64,
    pub archived_display_name: Option<String>,
    pub steam_ids: Vec<i64>,
    pub wins: i64,
    pub losses: i64,
}

impl AdminRepository {
    /// Return up to eleven registrations in this guild. The extra result lets
    /// callers show ten candidates and ask the admin to narrow their query.
    /// Names are literal substrings; numeric identities match exactly.
    pub fn lookup_accounts(
        &self,
        guild_id: i64,
        query: &str,
    ) -> Result<Vec<AdminAccountLookup>, AdminRepositoryError> {
        let query = query.trim();
        if guild_id <= 0 || query.is_empty() || query.chars().count() > 100 {
            return Ok(Vec::new());
        }
        let numeric = query.parse::<i64>().ok().filter(|id| *id > 0);
        let steam = numeric.and_then(account_id).map(i64::from);
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT p.discord_id,n.display_name,COALESCE(p.wins,0),COALESCE(p.losses,0)
             FROM players p LEFT JOIN player_display_names n
               ON n.discord_id=p.discord_id AND n.guild_id=p.guild_id
             WHERE p.guild_id=?1 AND (
                 p.discord_id=?3
                 OR EXISTS(SELECT 1 FROM player_steam_ids s WHERE s.discord_id=p.discord_id AND s.steam_id=?4)
                 OR instr(lower(p.discord_username),lower(?2))>0
                 OR instr(lower(n.display_name),lower(?2))>0)
             ORDER BY CASE
                 WHEN p.discord_id=?3 OR EXISTS(SELECT 1 FROM player_steam_ids s WHERE s.discord_id=p.discord_id AND s.steam_id=?4) THEN 0
                 WHEN lower(p.discord_username)=lower(?2) OR lower(n.display_name)=lower(?2) THEN 1
                 ELSE 2 END,p.discord_id
             LIMIT 11",
        )?;
        let mut results = statement
            .query_map(params![guild_id, query, numeric, steam], |row| {
                Ok(AdminAccountLookup {
                    discord_id: row.get(0)?,
                    archived_display_name: row.get(1)?,
                    steam_ids: Vec::new(),
                    wins: row.get(2)?,
                    losses: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut steam_ids = connection.prepare(
            "SELECT steam_id FROM player_steam_ids WHERE discord_id=?1 ORDER BY is_primary DESC,steam_id",
        )?;
        for result in &mut results {
            result.steam_ids = steam_ids
                .query_map([result.discord_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fast_migrated_database;

    #[test]
    fn finds_departed_registrations_by_stored_and_archived_names_only_in_this_guild() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO players(discord_id,guild_id,discord_username,wins,losses) VALUES
               (10,7,'Bismi',12,4),(20,8,'Bismi',3,2),(30,7,'old-name',0,0);
             INSERT INTO player_display_names VALUES
               (30,7,'Bismi New Nick',1),(40,7,'Bismi Unregistered',1),(10,8,'Other Guild Secret',1);",
        ).unwrap();
        let repo = AdminRepository::new(db.path());
        let found = repo.lookup_accounts(7, " bIsMi ").unwrap();
        assert_eq!(
            found.iter().map(|p| p.discord_id).collect::<Vec<_>>(),
            [10, 30]
        );
        assert_eq!((found[0].wins, found[0].losses), (12, 4));
        assert_eq!(found[0].archived_display_name, None);
        assert_eq!(
            found[1].archived_display_name.as_deref(),
            Some("Bismi New Nick")
        );
        assert!(
            repo.lookup_accounts(7, "Other Guild Secret")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn finds_exact_discord_and_alternate_steam32_or_steam64_ids_without_duplicates() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES (101,7,'Bismi'),(1010,7,'Else');
             INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at) VALUES
                (101,500,1,1),(101,600,0,2);",
        ).unwrap();
        let repo = AdminRepository::new(db.path());
        for query in ["101", "500", "600", "76561197960266328"] {
            let found = repo.lookup_accounts(7, query).unwrap();
            assert_eq!(found.len(), 1, "{query}");
            assert_eq!(found[0].discord_id, 101);
            assert_eq!(found[0].steam_ids, [500, 600]);
        }
        assert!(repo.lookup_accounts(8, "600").unwrap().is_empty());
        assert!(
            repo.lookup_accounts(7, "18446744073709551616")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn treats_wildcards_literally_and_limits_ambiguous_candidates_with_exact_names_first() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        for id in 1..=15 {
            conn.execute("INSERT INTO players(discord_id,guild_id,discord_username) VALUES(?1,7,'Bismi suffix')",[id]).unwrap();
        }
        conn.execute_batch("INSERT INTO players(discord_id,guild_id,discord_username) VALUES (99,7,'Bismi'),(100,7,'100%_literal');").unwrap();
        let repo = AdminRepository::new(db.path());
        let found = repo.lookup_accounts(7, "Bismi").unwrap();
        assert_eq!(found.len(), 11);
        assert_eq!(found[0].discord_id, 99);
        assert_eq!(found[1].discord_id, 1);
        for query in ["%", "_", "%_"] {
            let found = repo.lookup_accounts(7, query).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].discord_id, 100);
        }
        for query in [" ".to_owned(), "x".repeat(101)] {
            assert!(repo.lookup_accounts(7, &query).unwrap().is_empty());
        }
    }
}
