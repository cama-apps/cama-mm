//! Existing-schema SQLite reads for the Hero Grid command.
//!
//! The runtime schema manager owns startup migration. This adapter only opens
//! an initialized database through [`crate::open_runtime_connection`]; production
//! code in this module never creates a database, table, index, or migration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::params_from_iter;
use thiserror::Error;

use crate::open_runtime_connection;

/// Leave room for the guild bind while remaining compatible with conservative
/// SQLite variable limits.  Python normally sends one query; chunking is
/// observationally equivalent and lets the Rust adapter accept large rosters.
const ID_CHUNK_SIZE: usize = 900;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroGridStat {
    pub discord_id: i64,
    pub hero_id: i64,
    pub games: i64,
    pub wins: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnrichedPlayer {
    pub discord_id: i64,
    pub total_games: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroGridPlayer {
    pub discord_id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HeroGridTeams {
    pub radiant: Vec<i64>,
    pub dire: Vec<i64>,
}

impl HeroGridTeams {
    #[must_use]
    pub fn contains(&self, discord_id: i64) -> bool {
        self.radiant.contains(&discord_id) || self.dire.contains(&discord_id)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersistedHeroGridSources {
    pub open_lobby: Vec<i64>,
    pub low_skill_lobby: Vec<i64>,
    pub invoking_user_lobby_ids: Vec<i64>,
    pub invoking_pending_match: Option<HeroGridTeams>,
    pub guild_pending_match: Option<HeroGridTeams>,
    pub last_match_ids: Vec<i64>,
}

#[derive(Debug, Error)]
pub enum HeroGridRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Read port used by the transport-neutral application service.
pub trait HeroGridDataPort {
    type Error;

    fn multi_player_hero_stats(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridStat>, Self::Error>;

    fn players_with_enriched_data(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<EnrichedPlayer>, Self::Error>;

    fn player_names(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridPlayer>, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct HeroGridRepository {
    path: PathBuf,
}

impl HeroGridRepository {
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

    /// Read the persisted portions of Python's Hero Grid source-priority
    /// chain. Active draft state is intentionally supplied by the shared
    /// in-memory `DraftStateManager` in the runtime composition.
    pub fn get_persisted_sources(
        &self,
        guild_id: Option<i64>,
        invoking_user_id: Option<i64>,
    ) -> Result<PersistedHeroGridSources, HeroGridRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let mut result = PersistedHeroGridSources::default();

        let mut lobby_statement = connection.prepare(
            "SELECT lobby_id, players
               FROM lobby_state
              WHERE guild_id=?1 AND status='open' AND lobby_id IN (1,2)
              ORDER BY lobby_id",
        )?;
        let lobbies = lobby_statement.query_map([guild_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for lobby in lobbies {
            let (lobby_id, payload) = lobby?;
            let players = decode_id_list(&payload);
            if invoking_user_id.is_some_and(|user_id| players.contains(&user_id)) {
                result.invoking_user_lobby_ids.push(lobby_id);
            }
            if lobby_id == 2 {
                result.low_skill_lobby = players;
            } else {
                result.open_lobby = players;
            }
        }

        let mut pending_statement = connection.prepare(
            "SELECT payload
               FROM pending_matches
              WHERE guild_id=?1
              ORDER BY created_at ASC, pending_match_id ASC",
        )?;
        let pending_payloads = pending_statement
            .query_map([guild_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let pending_matches = pending_payloads
            .iter()
            .filter_map(|payload| decode_pending_teams(payload))
            .collect::<Vec<_>>();
        result.guild_pending_match = (pending_payloads.len() == 1)
            .then(|| pending_matches.first().cloned())
            .flatten();
        result.invoking_pending_match = invoking_user_id.and_then(|user_id| {
            pending_matches
                .iter()
                .find(|teams| teams.contains(user_id))
                .cloned()
        });

        let recent = connection.query_row(
            "SELECT team1_players, team2_players
               FROM matches
              WHERE guild_id=?1
              ORDER BY match_date DESC, match_id DESC
              LIMIT 1",
            [guild_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match recent {
            Ok((team1, team2)) => {
                result.last_match_ids = stable_unique(
                    &decode_id_list(&team1)
                        .into_iter()
                        .chain(decode_id_list(&team2))
                        .collect::<Vec<_>>(),
                );
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(error.into()),
        }

        Ok(result)
    }

    /// Aggregate enriched hero rows for several players.
    ///
    /// Rows are sorted by signed Discord id, then games descending, matching
    /// Python's query.  Hero id is a deterministic final tie-breaker.
    pub fn get_multi_player_hero_stats(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridStat>, HeroGridRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }

        let unique_ids = stable_unique(discord_ids);
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let mut result = Vec::new();

        for chunk in unique_ids.chunks(ID_CHUNK_SIZE) {
            let placeholders = repeat_placeholders(chunk.len());
            let sql = format!(
                "SELECT mp.discord_id, mp.hero_id,
                        COUNT(*) AS games,
                        SUM(CASE WHEN mp.won THEN 1 ELSE 0 END) AS wins
                   FROM match_participants mp
                   JOIN matches m ON mp.match_id = m.match_id
                  WHERE mp.discord_id IN ({placeholders})
                    AND m.guild_id = ?
                    AND mp.hero_id IS NOT NULL
                    AND mp.hero_id > 0
                  GROUP BY mp.discord_id, mp.hero_id"
            );
            let parameters = chunk.iter().copied().chain([guild_id]);
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                Ok(HeroGridStat {
                    discord_id: row.get(0)?,
                    hero_id: row.get(1)?,
                    games: row.get(2)?,
                    wins: row.get(3)?,
                })
            })?;
            result.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }

        result.sort_by(|left, right| {
            left.discord_id
                .cmp(&right.discord_id)
                .then_with(|| right.games.cmp(&left.games))
                .then_with(|| left.hero_id.cmp(&right.hero_id))
        });
        Ok(result)
    }

    /// Return players with at least one positive hero id, most games first.
    pub fn get_players_with_enriched_data(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<EnrichedPlayer>, HeroGridRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT mp.discord_id, COUNT(*) AS total_games
               FROM match_participants mp
               JOIN matches m ON mp.match_id = m.match_id
              WHERE m.guild_id = ?1
                AND mp.hero_id IS NOT NULL
                AND mp.hero_id > 0
              GROUP BY mp.discord_id
              ORDER BY total_games DESC, mp.discord_id ASC",
        )?;
        let rows = statement.query_map([guild_id], |row| {
            Ok(EnrichedPlayer {
                discord_id: row.get(0)?,
                total_games: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Resolve names in the caller's requested order.
    ///
    /// Missing ids are deliberately omitted so the application layer can use
    /// Python's `User {id}` fallback without coupling the repository to copy.
    pub fn get_player_names(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridPlayer>, HeroGridRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }

        let unique_ids = stable_unique(discord_ids);
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let mut by_id = BTreeMap::new();
        for chunk in unique_ids.chunks(ID_CHUNK_SIZE) {
            let placeholders = repeat_placeholders(chunk.len());
            let sql = format!(
                "SELECT discord_id, discord_username
                   FROM players
                  WHERE guild_id = ?
                    AND discord_id IN ({placeholders})"
            );
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (discord_id, name) = row?;
                by_id.entry(discord_id).or_insert(name);
            }
        }

        Ok(discord_ids
            .iter()
            .filter_map(|discord_id| {
                by_id.get(discord_id).map(|name| HeroGridPlayer {
                    discord_id: *discord_id,
                    name: name.clone(),
                })
            })
            .collect())
    }
}

impl HeroGridDataPort for HeroGridRepository {
    type Error = HeroGridRepositoryError;

    fn multi_player_hero_stats(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridStat>, Self::Error> {
        self.get_multi_player_hero_stats(discord_ids, guild_id)
    }

    fn players_with_enriched_data(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<EnrichedPlayer>, Self::Error> {
        self.get_players_with_enriched_data(guild_id)
    }

    fn player_names(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroGridPlayer>, Self::Error> {
        self.get_player_names(discord_ids, guild_id)
    }
}

fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn stable_unique(values: &[i64]) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

fn decode_id_list(payload: &str) -> Vec<i64> {
    serde_json::from_str::<Vec<i64>>(payload).unwrap_or_default()
}

fn decode_pending_teams(payload: &str) -> Option<HeroGridTeams> {
    let payload = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    Some(HeroGridTeams {
        radiant: payload
            .get("radiant_team_ids")
            .and_then(serde_json::Value::as_array)
            .map_or_else(Vec::new, |ids| {
                ids.iter().filter_map(serde_json::Value::as_i64).collect()
            }),
        dire: payload
            .get("dire_team_ids")
            .and_then(serde_json::Value::as_array)
            .map_or_else(Vec::new, |ids| {
                ids.iter().filter_map(serde_json::Value::as_i64).collect()
            }),
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD: i64 = 12_345;
    const OTHER_GUILD: i64 = 67_890;

    struct Fixture {
        file: NamedTempFile,
        connection: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("temporary database");
            let connection = Connection::open(file.path()).expect("open disposable database");
            connection
                .execute_batch(
                    "CREATE TABLE players (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         discord_username TEXT NOT NULL,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE matches (
                         match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL
                     );
                     CREATE TABLE match_participants (
                         match_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         team_number INTEGER NOT NULL,
                         won INTEGER,
                         hero_id INTEGER,
                         PRIMARY KEY (match_id, discord_id)
                     );",
                )
                .expect("create test-owned schema");
            Self { file, connection }
        }

        fn repository(&self) -> HeroGridRepository {
            HeroGridRepository::new(self.file.path())
        }

        fn player(&self, discord_id: i64, name: &str, guild_id: i64) {
            self.connection
                .execute(
                    "INSERT INTO players (discord_id, guild_id, discord_username)
                     VALUES (?1, ?2, ?3)",
                    params![discord_id, guild_id, name],
                )
                .expect("insert player");
        }

        fn record_match(&self, radiant: &[i64], dire: &[i64], winner: i64, guild: i64) -> i64 {
            self.connection
                .execute("INSERT INTO matches (guild_id) VALUES (?1)", [guild])
                .expect("insert match");
            let match_id = self.connection.last_insert_rowid();
            for (team, ids) in [(1_i64, radiant), (2, dire)] {
                for discord_id in ids {
                    self.connection
                        .execute(
                            "INSERT INTO match_participants (
                                 match_id, discord_id, guild_id, team_number, won
                             ) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![match_id, discord_id, guild, team, i64::from(team == winner)],
                        )
                        .expect("insert participant");
                }
            }
            match_id
        }

        fn enrich(&self, match_id: i64, discord_id: i64, hero_id: i64) {
            self.connection
                .execute(
                    "UPDATE match_participants
                        SET hero_id = ?1
                      WHERE match_id = ?2 AND discord_id = ?3",
                    params![hero_id, match_id, discord_id],
                )
                .expect("enrich participant");
        }
    }

    #[test]
    fn test_empty_ids_returns_empty() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository()
                .get_multi_player_hero_stats(&[], Some(GUILD))
                .expect("empty aggregate"),
            []
        );
    }

    #[test]
    fn test_no_enriched_data_returns_empty() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        fixture.record_match(&[100], &[200], 1, GUILD);
        assert_eq!(
            fixture
                .repository()
                .get_multi_player_hero_stats(&[100, 200], Some(GUILD))
                .expect("unenriched aggregate"),
            []
        );
    }

    #[test]
    fn test_single_player_with_data() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        let match_id = fixture.record_match(&[100], &[200], 1, GUILD);
        fixture.enrich(match_id, 100, 1);

        assert_eq!(
            fixture
                .repository()
                .get_multi_player_hero_stats(&[100], Some(GUILD))
                .expect("single aggregate"),
            [HeroGridStat {
                discord_id: 100,
                hero_id: 1,
                games: 1,
                wins: 1,
            }]
        );
    }

    #[test]
    fn test_multiple_players() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        let match_id = fixture.record_match(&[100], &[200], 1, GUILD);
        fixture.enrich(match_id, 100, 1);
        fixture.enrich(match_id, 200, 2);

        let result = fixture
            .repository()
            .get_multi_player_hero_stats(&[100, 200], Some(GUILD))
            .expect("multi-player aggregate");
        assert_eq!(result.len(), 2);
        assert_eq!(
            result
                .iter()
                .map(|row| row.discord_id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([100, 200])
        );
    }

    #[test]
    fn test_aggregates_across_matches() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        for (radiant, dire, winner) in [
            (&[100][..], &[200][..], 1),
            (&[200], &[100], 1),
            (&[100], &[200], 1),
        ] {
            let match_id = fixture.record_match(radiant, dire, winner, GUILD);
            fixture.enrich(match_id, 100, 1);
        }

        let result = fixture
            .repository()
            .get_multi_player_hero_stats(&[100], Some(GUILD))
            .expect("cross-match aggregate");
        assert_eq!(result[0].games, 3);
        assert_eq!(result[0].wins, 2);
    }

    #[test]
    fn test_empty_db() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository()
                .get_players_with_enriched_data(Some(GUILD))
                .expect("empty enriched list"),
            []
        );
    }

    #[test]
    fn test_returns_sorted_by_games() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        for _ in 0..2 {
            let match_id = fixture.record_match(&[100], &[200], 1, GUILD);
            fixture.enrich(match_id, 100, 1);
        }
        let match_id = fixture.record_match(&[200], &[100], 1, GUILD);
        fixture.enrich(match_id, 200, 2);

        assert_eq!(
            fixture
                .repository()
                .get_players_with_enriched_data(Some(GUILD))
                .expect("sorted enriched players"),
            [
                EnrichedPlayer {
                    discord_id: 100,
                    total_games: 2,
                },
                EnrichedPlayer {
                    discord_id: 200,
                    total_games: 1,
                },
            ]
        );
    }

    #[test]
    fn test_excludes_players_without_hero_data() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        fixture.record_match(&[100], &[200], 1, GUILD);
        assert_eq!(
            fixture
                .repository()
                .get_players_with_enriched_data(Some(GUILD))
                .expect("unenriched players"),
            []
        );
    }

    #[test]
    fn repository_reads_are_guild_scoped_even_for_the_same_signed_id() {
        let fixture = Fixture::new();
        fixture.player(-9, "Primary", GUILD);
        fixture.player(-9, "Other", OTHER_GUILD);
        fixture.player(2, "Opponent", GUILD);
        fixture.player(3, "Other opponent", OTHER_GUILD);
        let own_match = fixture.record_match(&[-9], &[2], 1, GUILD);
        fixture.enrich(own_match, -9, 1);
        let other_match = fixture.record_match(&[-9], &[3], 1, OTHER_GUILD);
        fixture.enrich(other_match, -9, 2);

        assert_eq!(
            fixture
                .repository()
                .get_multi_player_hero_stats(&[-9], Some(GUILD))
                .expect("guild-scoped stats"),
            [HeroGridStat {
                discord_id: -9,
                hero_id: 1,
                games: 1,
                wins: 1,
            }]
        );
        assert_eq!(
            fixture
                .repository()
                .get_player_names(&[-9], Some(GUILD))
                .expect("guild-scoped name"),
            [HeroGridPlayer {
                discord_id: -9,
                name: "Primary".to_owned(),
            }]
        );
    }

    #[test]
    fn signed_ids_and_requested_name_order_survive_bulk_reads() {
        let fixture = Fixture::new();
        let low = i64::MIN + 7;
        let high = i64::MAX - 7;
        fixture.player(low, "Low", GUILD);
        fixture.player(high, "High", GUILD);
        assert_eq!(
            fixture
                .repository()
                .get_player_names(&[high, low, high], Some(GUILD))
                .expect("ordered signed ids"),
            [
                HeroGridPlayer {
                    discord_id: high,
                    name: "High".to_owned(),
                },
                HeroGridPlayer {
                    discord_id: low,
                    name: "Low".to_owned(),
                },
                HeroGridPlayer {
                    discord_id: high,
                    name: "High".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn enrichment_filter_and_tie_order_are_deterministic() {
        let fixture = Fixture::new();
        fixture.player(100, "Alice", GUILD);
        fixture.player(200, "Bob", GUILD);
        for hero_id in [3, 0, -1, 2] {
            let match_id = fixture.record_match(&[100], &[200], 1, GUILD);
            fixture.enrich(match_id, 100, hero_id);
        }
        let result = fixture
            .repository()
            .get_multi_player_hero_stats(&[100], Some(GUILD))
            .expect("filtered aggregate");
        assert_eq!(
            result.iter().map(|row| row.hero_id).collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn missing_database_is_not_created() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("does-not-exist.db");
        let error = HeroGridRepository::new(&path)
            .get_players_with_enriched_data(Some(GUILD))
            .expect_err("missing databases must fail");
        assert!(matches!(error, HeroGridRepositoryError::Sqlite(_)));
        assert!(!path.exists());
    }
}
