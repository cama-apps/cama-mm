//! Existing-schema SQLite reads used by the Scout command.
//!
//! The Rust startup migration runner owns schema admission. This repository
//! opens the admitted database without SQLite's create flag and performs only
//! the aggregate reads used by the live Scout command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::params_from_iter;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroStat {
    pub hero_id: i64,
    pub games: i64,
    pub wins: i64,
    pub losses: i64,
    pub primary_role: i64,
}

#[derive(Debug, Error)]
pub enum ScoutRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Read port consumed by the adapter-neutral Scout application service.
pub trait ScoutDataPort {
    type Error;

    fn player_hero_stats(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<HeroStat>>, Self::Error>;

    fn opposing_bans(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, Self::Error>;

    fn match_count(&self, discord_ids: &[i64], guild_id: Option<i64>) -> Result<i64, Self::Error>;
}

pub trait SteamIdsPort {
    type Error;

    fn steam_ids_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<i64>>, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct ScoutRepository {
    path: PathBuf,
}

impl ScoutRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn get_player_hero_stats_for_scout(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<HeroStat>>, ScoutRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let guild_id = guild_id.unwrap_or(0);
        let placeholders = repeat_placeholders(discord_ids.len());
        let sql = format!(
            "SELECT mp.discord_id, mp.hero_id, mp.lane_role, mp.won
               FROM match_participants mp
               JOIN matches m ON mp.match_id = m.match_id
              WHERE mp.discord_id IN ({placeholders})
                AND m.guild_id = ?
                AND mp.guild_id = ?
                AND mp.hero_id IS NOT NULL
                AND mp.hero_id > 0"
        );
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(&sql)?;
        let parameters = discord_ids.iter().copied().chain([guild_id, guild_id]);
        let rows = statement.query_map(params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })?;

        #[derive(Default)]
        struct Aggregate {
            games: i64,
            wins: i64,
            roles: Vec<i64>,
        }

        let mut aggregate: BTreeMap<i64, BTreeMap<i64, Aggregate>> = BTreeMap::new();
        for row in rows {
            let (discord_id, hero_id, lane_role, won) = row?;
            let stat = aggregate
                .entry(discord_id)
                .or_default()
                .entry(hero_id)
                .or_default();
            stat.games += 1;
            stat.wins += i64::from(won);
            if let Some(role) = lane_role {
                stat.roles.push(role);
            }
        }

        Ok(aggregate
            .into_iter()
            .map(|(discord_id, heroes)| {
                let mut heroes = heroes
                    .into_iter()
                    .map(|(hero_id, stat)| HeroStat {
                        hero_id,
                        games: stat.games,
                        wins: stat.wins,
                        losses: stat.games - stat.wins,
                        primary_role: mode_or_default(&stat.roles, 1),
                    })
                    .collect::<Vec<_>>();
                heroes.sort_by(|left, right| {
                    right
                        .games
                        .cmp(&left.games)
                        .then_with(|| left.hero_id.cmp(&right.hero_id))
                });
                (discord_id, heroes)
            })
            .collect())
    }

    pub fn get_bans_for_players(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, ScoutRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let guild_id = guild_id.unwrap_or(0);
        let placeholders = repeat_placeholders(discord_ids.len());
        let sql = format!(
            "WITH scouted_match_teams AS (
                 SELECT DISTINCT match_id, team_number
                   FROM match_participants
                  WHERE guild_id = ?
                    AND discord_id IN ({placeholders})
                    AND team_number IN (1, 2)
             )
             SELECT mb.hero_id, COUNT(*) AS ban_count
               FROM scouted_match_teams smt
               JOIN match_bans mb
                 ON mb.match_id = smt.match_id
                AND mb.team = CASE smt.team_number WHEN 1 THEN 1 WHEN 2 THEN 0 END
              GROUP BY mb.hero_id"
        );
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(&sql)?;
        let parameters = std::iter::once(guild_id).chain(discord_ids.iter().copied());
        let rows = statement.query_map(params_from_iter(parameters), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }

    pub fn get_match_count_for_players(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<i64, ScoutRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(0);
        }

        let guild_id = guild_id.unwrap_or(0);
        let placeholders = repeat_placeholders(discord_ids.len());
        let sql = format!(
            "SELECT COUNT(DISTINCT mp.match_id)
               FROM match_participants mp
               JOIN matches m ON mp.match_id = m.match_id
              WHERE mp.discord_id IN ({placeholders})
                AND m.guild_id = ?
                AND mp.guild_id = ?"
        );
        let connection = open_runtime_connection(&self.path)?;
        let parameters = discord_ids.iter().copied().chain([guild_id, guild_id]);
        connection
            .query_row(&sql, params_from_iter(parameters), |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn get_steam_ids_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<i64>>, ScoutRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut result = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut junction_found = BTreeSet::new();
        let connection = open_runtime_connection(&self.path)?;
        let unique_ids = unique_ids.into_iter().collect::<Vec<_>>();

        for chunk in unique_ids.chunks(900) {
            let placeholders = repeat_placeholders(chunk.len());
            let sql = format!(
                "SELECT discord_id, steam_id
                   FROM player_steam_ids
                  WHERE discord_id IN ({placeholders})
                  ORDER BY discord_id, is_primary DESC, added_at ASC"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(chunk.iter().copied()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (discord_id, steam_id) = row?;
                result.entry(discord_id).or_default().push(steam_id);
                junction_found.insert(discord_id);
            }
        }

        let missing = unique_ids
            .into_iter()
            .filter(|discord_id| !junction_found.contains(discord_id))
            .collect::<Vec<_>>();
        for chunk in missing.chunks(900) {
            let placeholders = repeat_placeholders(chunk.len());
            let guild_clause = guild_id.map_or("", |_| " AND guild_id = ?");
            let sql = format!(
                "SELECT discord_id, steam_id
                   FROM players
                  WHERE discord_id IN ({placeholders})
                    AND steam_id IS NOT NULL{guild_clause}"
            );
            let mut parameters = chunk.to_vec();
            if let Some(guild_id) = guild_id {
                parameters.push(guild_id);
            }
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(parameters), |row| {
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
}

impl ScoutDataPort for ScoutRepository {
    type Error = ScoutRepositoryError;

    fn player_hero_stats(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<HeroStat>>, Self::Error> {
        self.get_player_hero_stats_for_scout(discord_ids, guild_id)
    }

    fn opposing_bans(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, Self::Error> {
        self.get_bans_for_players(discord_ids, guild_id)
    }

    fn match_count(&self, discord_ids: &[i64], guild_id: Option<i64>) -> Result<i64, Self::Error> {
        self.get_match_count_for_players(discord_ids, guild_id)
    }
}

impl SteamIdsPort for ScoutRepository {
    type Error = ScoutRepositoryError;

    fn steam_ids_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Vec<i64>>, Self::Error> {
        self.get_steam_ids_bulk(discord_ids, guild_id)
    }
}

#[derive(Clone, Debug)]
pub struct PlayerSteamService<P> {
    port: P,
}

impl<P> PlayerSteamService<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P: SteamIdsPort> PlayerSteamService<P> {
    pub fn get_steam_ids_bulk(
        &self,
        discord_ids: &[i64],
    ) -> Result<BTreeMap<i64, Vec<i64>>, P::Error> {
        self.port.steam_ids_bulk(discord_ids, None)
    }
}

fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn mode_or_default(values: &[i64], default: i64) -> i64 {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(*value).or_insert(0_usize) += 1;
    }
    counts
        .into_iter()
        .max_by(|(left_value, left_count), (right_value, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_value.cmp(left_value))
        })
        .map_or(default, |(value, _)| value)
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD: i64 = 42;

    struct Fixture {
        file: NamedTempFile,
        connection: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("temporary database");
            let connection = Connection::open(file.path()).expect("open fixture database");
            connection
                .execute_batch(
                    "CREATE TABLE players (
                         discord_id INTEGER PRIMARY KEY,
                         discord_username TEXT NOT NULL,
                         guild_id INTEGER NOT NULL,
                         steam_id INTEGER
                     );
                     CREATE TABLE player_steam_ids (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id INTEGER NOT NULL,
                         steam_id INTEGER NOT NULL UNIQUE,
                         is_primary INTEGER DEFAULT 0,
                         added_at INTEGER NOT NULL,
                         UNIQUE(discord_id, steam_id)
                     );
                     CREATE TABLE matches (
                         match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL
                     );
                     CREATE TABLE match_participants (
                         match_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         team_number INTEGER,
                         won INTEGER,
                         side TEXT,
                         hero_id INTEGER,
                         lane_role INTEGER,
                         PRIMARY KEY(match_id, discord_id)
                     );
                     CREATE TABLE match_bans (
                         match_id INTEGER NOT NULL,
                         ban_index INTEGER NOT NULL,
                         team INTEGER NOT NULL,
                         hero_id INTEGER NOT NULL,
                         PRIMARY KEY(match_id, ban_index)
                     );",
                )
                .expect("create disposable test schema");
            Self { file, connection }
        }

        fn repository(&self) -> ScoutRepository {
            ScoutRepository::new(self.file.path())
        }

        fn player(&self, discord_id: i64, name: &str) {
            self.connection
                .execute(
                    "INSERT INTO players (discord_id, discord_username, guild_id)
                     VALUES (?1, ?2, ?3)",
                    params![discord_id, name, GUILD],
                )
                .expect("insert player");
        }

        fn match_with_players(&self, radiant: &[i64], dire: &[i64], winner: i64) -> i64 {
            self.connection
                .execute("INSERT INTO matches (guild_id) VALUES (?1)", [GUILD])
                .expect("insert match");
            let match_id = self.connection.last_insert_rowid();
            for (team, players) in [(1, radiant), (2, dire)] {
                for discord_id in players {
                    self.connection
                        .execute(
                            "INSERT INTO match_participants (
                                 match_id, discord_id, guild_id, team_number, won, side
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                match_id,
                                discord_id,
                                GUILD,
                                team,
                                i64::from(team == winner),
                                if team == 1 { "radiant" } else { "dire" },
                            ],
                        )
                        .expect("insert participant");
                }
            }
            match_id
        }

        fn enrich_participant(&self, match_id: i64, discord_id: i64, hero: i64, role: i64) {
            self.connection
                .execute(
                    "UPDATE match_participants
                        SET hero_id = ?1, lane_role = ?2
                      WHERE match_id = ?3 AND discord_id = ?4",
                    params![hero, role, match_id, discord_id],
                )
                .expect("enrich participant");
        }

        fn ban(&self, match_id: i64, index: i64, team: i64, hero: i64) {
            self.connection
                .execute(
                    "INSERT INTO match_bans (match_id, ban_index, team, hero_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![match_id, index, team, hero],
                )
                .expect("insert ban");
        }
    }

    #[test]
    fn test_get_player_hero_stats_for_scout_empty_list() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository()
                .get_player_hero_stats_for_scout(&[], Some(GUILD))
                .expect("empty read"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_get_player_hero_stats_for_scout_no_data() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository()
                .get_player_hero_stats_for_scout(&[999, 998], Some(GUILD))
                .expect("missing players"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_get_player_hero_stats_for_scout_with_data() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        let match_id = fixture.match_with_players(&[100], &[200], 1);
        fixture.enrich_participant(match_id, 100, 1, 1);
        fixture.enrich_participant(match_id, 200, 2, 2);

        let result = fixture
            .repository()
            .get_player_hero_stats_for_scout(&[100, 200], Some(GUILD))
            .expect("scout stats");
        assert_eq!(
            result[&100],
            [HeroStat {
                hero_id: 1,
                games: 1,
                wins: 1,
                losses: 0,
                primary_role: 1,
            }]
        );
        assert_eq!(result[&200][0].hero_id, 2);
        assert_eq!(result[&200][0].wins, 0);
        assert_eq!(result[&200][0].losses, 1);
    }

    #[test]
    fn test_get_player_hero_stats_multiple_games() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        for winner in [1, 1, 2] {
            let match_id = fixture.match_with_players(&[100], &[200], winner);
            fixture.enrich_participant(match_id, 100, 1, 1);
            fixture.enrich_participant(match_id, 200, 2, 2);
        }

        let result = fixture
            .repository()
            .get_player_hero_stats_for_scout(&[100], Some(GUILD))
            .expect("aggregated stats");
        assert_eq!(result[&100][0].games, 3);
        assert_eq!(result[&100][0].wins, 2);
        assert_eq!(result[&100][0].losses, 1);
    }

    #[test]
    fn test_get_bans_for_players_empty_list() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository()
                .get_bans_for_players(&[], Some(GUILD))
                .expect("empty ban read"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_get_bans_for_players_no_enrichment_data() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        let match_id = fixture.match_with_players(&[100], &[200], 1);
        fixture.enrich_participant(match_id, 100, 1, 1);
        assert_eq!(
            fixture
                .repository()
                .get_bans_for_players(&[100], Some(GUILD))
                .expect("ban read"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_get_bans_for_players_with_enrichment_data() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        let match_id = fixture.match_with_players(&[100], &[200], 1);
        fixture.enrich_participant(match_id, 100, 1, 1);
        fixture.ban(match_id, 0, 0, 10);
        fixture.ban(match_id, 1, 1, 20);
        fixture.ban(match_id, 3, 1, 30);

        let result = fixture
            .repository()
            .get_bans_for_players(&[100], Some(GUILD))
            .expect("opposing bans");
        assert_eq!(result, BTreeMap::from([(20, 1), (30, 1)]));
    }

    #[test]
    fn test_get_bans_deduplication_across_players() {
        let fixture = Fixture::new();
        for (id, name) in [(100, "Player1"), (101, "Player1b"), (200, "Player2")] {
            fixture.player(id, name);
        }
        let match_id = fixture.match_with_players(&[100, 101], &[200], 1);
        fixture.enrich_participant(match_id, 100, 1, 1);
        fixture.enrich_participant(match_id, 101, 2, 1);
        fixture.ban(match_id, 0, 1, 10);

        let result = fixture
            .repository()
            .get_bans_for_players(&[100, 101], Some(GUILD))
            .expect("deduplicated bans");
        assert_eq!(result.get(&10), Some(&1));
    }

    #[test]
    fn test_get_bans_ignores_same_team_bans() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        let match_id = fixture.match_with_players(&[100], &[200], 1);
        fixture.enrich_participant(match_id, 100, 1, 1);
        fixture.ban(match_id, 0, 0, 10);
        assert_eq!(
            fixture
                .repository()
                .get_bans_for_players(&[100], Some(GUILD))
                .expect("same-team bans"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_scout_ban_query_never_reads_raw_enrichment() {
        let fixture = Fixture::new();
        fixture.player(100, "Player1");
        fixture.player(200, "Player2");
        let match_id = fixture.match_with_players(&[100], &[200], 1);
        fixture.ban(match_id, 0, 1, 10);

        // This disposable `matches` shape intentionally has no
        // `enrichment_data` column. A successful query therefore proves the
        // production Scout path is sourced exclusively from `match_bans`.
        assert_eq!(
            fixture
                .repository()
                .get_bans_for_players(&[100], Some(GUILD))
                .expect("normalized Scout ban read"),
            BTreeMap::from([(10, 1)])
        );
    }

    #[test]
    fn test_empty_list_returns_empty_dict() {
        let fixture = Fixture::new();
        let service = PlayerSteamService::new(fixture.repository());
        assert_eq!(
            service.get_steam_ids_bulk(&[]).expect("empty bulk read"),
            BTreeMap::new()
        );
    }

    #[test]
    fn test_returns_all_linked_accounts() {
        let fixture = Fixture::new();
        fixture.player(100, "Solo");
        fixture.player(200, "Smurf");
        for (discord_id, steam_id, primary, added_at) in [
            (100, 111_111, 1, 10),
            (200, 333_333, 0, 10),
            (200, 222_222, 1, 20),
        ] {
            fixture
                .connection
                .execute(
                    "INSERT INTO player_steam_ids
                         (discord_id, steam_id, is_primary, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![discord_id, steam_id, primary, added_at],
                )
                .expect("insert Steam account");
        }

        let result = PlayerSteamService::new(fixture.repository())
            .get_steam_ids_bulk(&[100, 200])
            .expect("bulk Steam read");
        assert_eq!(result[&100], [111_111]);
        assert_eq!(result[&200], [222_222, 333_333]);
    }

    #[test]
    fn test_player_without_steam_id_has_empty_list() {
        let fixture = Fixture::new();
        fixture.player(100, "NoSteam");
        assert_eq!(
            PlayerSteamService::new(fixture.repository())
                .get_steam_ids_bulk(&[100])
                .expect("bulk Steam read"),
            BTreeMap::from([(100, Vec::new())])
        );
    }
}
