//! Read-only discovery of registered identities, including departed Discord accounts.
use cama_domain::dota_lobby::{STEAM_INDIVIDUAL_BASE, account_id};
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
        let discord_id = recovery_discord_id(query);
        let bare_digits = query.bytes().all(|b| b.is_ascii_digit());
        let steam_id = if bare_digits {
            discord_id.and_then(account_id)
        } else {
            profile_account(query)
        };
        let name = (!bare_digits && !query.starts_with("<@") && !query.contains("://"))
            .then(|| query.strip_prefix('@').unwrap_or(query))
            .filter(|name| !name.is_empty());
        let connection = open_runtime_connection(&self.path)?;
        let mut registrations = connection.prepare(
            "WITH profiles AS (
                 SELECT p.discord_id,p.discord_username,p.wins,p.losses,n.display_name,
                        CASE WHEN p.steam_id >= ?5 THEN p.steam_id-?5 ELSE p.steam_id END AS primary_account
                 FROM players p LEFT JOIN player_display_names n
                   ON n.discord_id=p.discord_id AND n.guild_id=p.guild_id
                 WHERE p.guild_id=?1
             ), ownership AS (
                 SELECT *,
                        CASE WHEN primary_account BETWEEN 1 AND 4294967294
                            AND NOT EXISTS(SELECT 1 FROM player_steam_ids s WHERE s.discord_id=profiles.discord_id)
                            AND NOT EXISTS(SELECT 1 FROM player_steam_ids s WHERE s.steam_id=primary_account)
                            AND NOT EXISTS(SELECT 1 FROM deadlock_players d WHERE d.discord_id<>profiles.discord_id AND d.steam_id=primary_account)
                            AND NOT EXISTS(SELECT 1 FROM players other WHERE other.discord_id<>profiles.discord_id
                                           AND other.steam_id IN (primary_account,primary_account+?5))
                        THEN primary_account END AS legacy_id
                 FROM profiles
             ), candidates AS (
                 SELECT *, CASE
                     WHEN discord_id=?3 OR legacy_id=?4 OR EXISTS(SELECT 1 FROM player_steam_ids s
                             WHERE s.discord_id=ownership.discord_id AND s.steam_id=?4) THEN 0
                     WHEN lower(discord_username)=lower(?2) OR lower(display_name)=lower(?2) THEN 1
                     WHEN instr(lower(discord_username),lower(?2))>0 OR instr(lower(display_name),lower(?2))>0 THEN 2
                 END AS rank FROM ownership WHERE rank IS NOT NULL ORDER BY rank,discord_id LIMIT 11
             )
             SELECT c.discord_id,c.display_name,coalesce(c.wins,0),coalesce(c.losses,0),coalesce(s.steam_id,c.legacy_id)
             FROM candidates c LEFT JOIN player_steam_ids s ON s.discord_id=c.discord_id
             ORDER BY c.rank,c.discord_id,s.is_primary DESC,s.steam_id",
        )?;
        let mut rows = registrations.query(params![
            guild_id,
            name,
            discord_id,
            steam_id,
            STEAM_INDIVIDUAL_BASE as i64
        ])?;
        let mut results: Vec<AdminAccountLookup> = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            if results.last().is_none_or(|player| player.discord_id != id) {
                results.push(AdminAccountLookup {
                    discord_id: id,
                    archived_display_name: row.get(1)?,
                    wins: row.get(2)?,
                    losses: row.get(3)?,
                    steam_ids: Vec::new(),
                });
            }
            if let Some(steam) = row.get::<_, Option<i64>>(4)? {
                results
                    .last_mut()
                    .expect("registration was just added or matched")
                    .steam_ids
                    .push(steam);
            }
        }
        Ok(results)
    }
}

/// Accept either Discord's Copy User ID value or a raw user mention.
pub fn recovery_discord_id(value: &str) -> Option<i64> {
    let value = value.trim();
    let digits = if let Some(mention) = value.strip_prefix("<@") {
        let id = mention.strip_suffix('>')?;
        id.strip_prefix('!').unwrap_or(id)
    } else {
        value
    };
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|id| *id > 0)
}

/// Numeric profile links can identify a registration even when the player no longer
/// appears in Discord. Vanity URLs need a network lookup and are not account IDs.
fn profile_account(url: &str) -> Option<u32> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let profile = if matches!(host.as_str(), "dotabuff.com" | "www.dotabuff.com") {
        path.strip_prefix("players/")?
    } else if matches!(
        host.as_str(),
        "steamcommunity.com" | "www.steamcommunity.com"
    ) {
        path.strip_prefix("profiles/")?
    } else {
        return None;
    };
    let id = profile.split(['/', '?', '#']).next()?;
    if !id.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    account_id(id.parse().ok()?)
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
    fn explicit_mentions_and_profile_links_search_their_own_id_namespace() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO players(discord_id,guild_id,discord_username,steam_id) VALUES
            (600,7,'Old Bismi',NULL), (200,7,'Other',600), (300,7,'Other 600',NULL);",
        )
        .unwrap();
        let repo = AdminRepository::new(db.path());
        for query in ["<@600>", " <@!600> ", "@Old Bismi"] {
            let found = repo.lookup_accounts(7, query).unwrap();
            assert_eq!(found.len(), 1, "{query}");
            assert_eq!(found[0].discord_id, 600);
        }
        for query in [
            "https://www.dotabuff.com/players/600/matches?period=month",
            "https://steamcommunity.com/profiles/76561197960266328/",
        ] {
            let found = repo.lookup_accounts(7, query).unwrap();
            assert_eq!(found.len(), 1, "{query}");
            assert_eq!(found[0].discord_id, 200);
            assert_eq!(found[0].steam_ids, [600]);
        }
        assert_eq!(repo.lookup_accounts(7, "600").unwrap().len(), 2);
        for query in [
            "<@!!600>",
            "<@&600>",
            "https://bad.test#www.dotabuff.com/players/600",
            "https://dotabuff.com/players/600oops",
            "https://steamcommunity.com/id/600",
        ] {
            assert!(
                repo.lookup_accounts(7, query).unwrap().is_empty(),
                "{query}"
            );
        }
    }

    #[test]
    fn legacy_primary_is_supported_but_stored_urls_do_not_establish_ownership() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        conn.execute_batch("INSERT INTO players(discord_id,guild_id,discord_username,steam_id,dotabuff_url) VALUES
            (101,7,'legacy',76561197960266328,NULL),
            (102,7,'url only',NULL,'https://www.dotabuff.com/players/76561197960266428'),
            (103,7,'stale',100,'https://www.dotabuff.com/players/120'),
            (104,8,'other guild',700,NULL);
            INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at) VALUES(103,200,1,1);").unwrap();
        let repo = AdminRepository::new(db.path());
        let steam = repo.lookup_accounts(7, "600").unwrap();
        assert_eq!(steam.len(), 1);
        assert_eq!(steam[0].steam_ids, [600]);
        assert!(
            repo.lookup_accounts(7, "url only").unwrap()[0]
                .steam_ids
                .is_empty()
        );
        for query in ["100", "120", "700"] {
            assert!(repo.lookup_accounts(7, query).unwrap().is_empty());
        }
        assert_eq!(repo.lookup_accounts(7, "200").unwrap()[0].discord_id, 103);
    }

    #[test]
    fn removed_and_reassigned_steam_ids_never_point_to_the_former_owner() {
        let db = fast_migrated_database();
        let conn = open_runtime_connection(db.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO players(discord_id,guild_id,discord_username,dotabuff_url) VALUES
            (101,7,'former','https://www.dotabuff.com/players/600'),(202,8,'owner',NULL);",
        )
        .unwrap();
        let links = crate::opendota_player::OpenDotaPlayerRepository::new(db.path());
        let repo = AdminRepository::new(db.path());
        links.add_steam_id(101, 600, true, 1).unwrap();
        assert_eq!(repo.lookup_accounts(7, "600").unwrap()[0].discord_id, 101);
        assert!(links.remove_steam_id(101, 600).unwrap());
        assert!(repo.lookup_accounts(7, "600").unwrap().is_empty());
        links.add_steam_id(202, 600, true, 2).unwrap();
        // Simulate a stale denormalized primary in an upgraded database, as well
        // as the Dotabuff URL that removesteamid always leaves behind.
        conn.execute(
            "UPDATE players SET steam_id=76561197960266328 WHERE discord_id=101",
            [],
        )
        .unwrap();
        assert!(repo.lookup_accounts(7, "600").unwrap().is_empty());
        assert!(
            repo.lookup_accounts(7, "former").unwrap()[0]
                .steam_ids
                .is_empty()
        );
        assert_eq!(
            repo.lookup_accounts(8, "https://www.dotabuff.com/players/600")
                .unwrap()[0]
                .discord_id,
            202
        );
        conn.execute(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(202,7,'owner')",
            [],
        )
        .unwrap();
        let found = repo.lookup_accounts(7, "600").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].discord_id, 202);
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
