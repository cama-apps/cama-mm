//! Python-compatible pairings repository and command service.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This module opens only an existing SQLite database, never creates
//! or migrates caller data, and stores every pair in signed-64-bit canonical
//! order (`player1_id < player2_id`). Match updates and rebuilds use one
//! deferred transaction, matching Python's connection context manager.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};

#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

use crate::open_runtime_connection;

/// The persisted aggregate for one canonical pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pairing {
    pub player1_id: i64,
    pub player2_id: i64,
    pub games_together: i64,
    pub wins_together: i64,
    pub games_against: i64,
    pub player1_wins_against: i64,
    pub last_match_id: Option<i64>,
}

impl Pairing {
    /// Opposing-team wins from the perspective of the queried player.
    #[must_use]
    pub fn queried_player_wins_against(&self, queried_player_id: i64) -> i64 {
        if queried_player_id == self.player1_id {
            self.player1_wins_against
        } else {
            self.games_against - self.player1_wins_against
        }
    }
}

/// A teammate projection returned by the ranking queries.
#[derive(Clone, Debug, PartialEq)]
pub struct TeammatePairing {
    pub teammate_id: i64,
    pub games_together: i64,
    pub wins_together: i64,
    pub win_rate: f64,
}

/// An opponent projection returned by the ranking queries.
#[derive(Clone, Debug, PartialEq)]
pub struct OpponentPairing {
    pub opponent_id: i64,
    pub games_against: i64,
    pub wins_against: i64,
    pub win_rate: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PairingCounts {
    pub unique_teammates: i64,
    pub unique_opponents: i64,
}

/// Every profile category produced by Python's `PairingsService`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerPairingSummary {
    pub best_teammates: Vec<TeammatePairing>,
    pub worst_teammates: Vec<TeammatePairing>,
    pub best_matchups: Vec<OpponentPairing>,
    pub worst_matchups: Vec<OpponentPairing>,
    pub most_played_with: Vec<TeammatePairing>,
    pub most_played_against: Vec<OpponentPairing>,
    pub even_teammates: Vec<TeammatePairing>,
    pub even_opponents: Vec<OpponentPairing>,
    pub counts: PairingCounts,
}

/// Repository for Python's existing `player_pairings` and match-history tables.
#[derive(Clone, Debug)]
pub struct PairingsRepository {
    path: PathBuf,
    #[cfg(test)]
    connection_count: Arc<AtomicUsize>,
}

impl PairingsRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            #[cfg(test)]
            connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Convert Python's nullable guild convention to its persisted sentinel.
    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    #[must_use]
    pub const fn canonical_pair(id1: i64, id2: i64) -> (i64, i64) {
        if id1 < id2 { (id1, id2) } else { (id2, id1) }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        #[cfg(test)]
        self.connection_count.fetch_add(1, AtomicOrdering::Relaxed);
        open_runtime_connection(&self.path)
    }

    #[cfg(test)]
    fn observed_connection_count(&self) -> usize {
        self.connection_count.load(AtomicOrdering::Relaxed)
    }

    /// Aggregate all teammate and opponent relationships from one match.
    pub fn update_pairings_for_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        team1_ids: &[i64],
        team2_ids: &[i64],
        winning_team: i64,
    ) -> Result<(), rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        update_match_pairings(
            &transaction,
            match_id,
            guild_id,
            team1_ids,
            team2_ids,
            winning_team,
        )?;
        transaction.commit()
    }

    /// Return all rows involving one player in the requested guild.
    pub fn get_pairings_for_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<Pairing>, rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT player1_id, player2_id, games_together, wins_together,
                    games_against, player1_wins_against, last_match_id
             FROM player_pairings
             WHERE guild_id = ?1 AND (player1_id = ?2 OR player2_id = ?2)",
        )?;
        statement
            .query_map(params![guild_id, discord_id], row_to_pairing)?
            .collect()
    }

    /// Fetch all pairings wholly contained in a group with one SQL query.
    pub fn get_pairings_for_players(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<(i64, i64), Pairing>, rusqlite::Error> {
        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        if unique_ids.len() < 2 {
            return Ok(BTreeMap::new());
        }

        let placeholders = std::iter::repeat_n("?", unique_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT player1_id, player2_id, games_together, wins_together,
                    games_against, player1_wins_against, last_match_id
             FROM player_pairings
             WHERE guild_id = ?
               AND player1_id IN ({placeholders})
               AND player2_id IN ({placeholders})"
        );
        let guild_id = Self::normalize_guild_id(guild_id);
        let parameters = std::iter::once(guild_id)
            .chain(unique_ids.iter().copied())
            .chain(unique_ids.iter().copied());
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let pairings = statement
            .query_map(params_from_iter(parameters), row_to_pairing)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(pairings
            .into_iter()
            .map(|pairing| ((pairing.player1_id, pairing.player2_id), pairing))
            .collect())
    }

    pub fn get_best_teammates(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<TeammatePairing>, rusqlite::Error> {
        self.query_teammates(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(wins_together AS REAL) / games_together > 0.5
             ORDER BY win_rate DESC, games_together DESC",
        )
    }

    pub fn get_worst_teammates(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<TeammatePairing>, rusqlite::Error> {
        self.query_teammates(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(wins_together AS REAL) / games_together < 0.5
             ORDER BY win_rate ASC, games_together DESC",
        )
    }

    pub fn get_most_played_with(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<TeammatePairing>, rusqlite::Error> {
        self.query_teammates(
            discord_id,
            guild_id,
            min_games,
            limit,
            "ORDER BY games_together DESC, win_rate DESC",
        )
    }

    pub fn get_evenly_matched_teammates(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<TeammatePairing>, rusqlite::Error> {
        self.query_teammates(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(wins_together AS REAL) / games_together = 0.5
             ORDER BY games_together DESC",
        )
    }

    fn query_teammates(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
        suffix: &str,
    ) -> Result<Vec<TeammatePairing>, rusqlite::Error> {
        let sql = format!(
            "SELECT CASE WHEN player1_id = ?1 THEN player2_id ELSE player1_id END,
                    games_together, wins_together,
                    CAST(wins_together AS REAL) / games_together AS win_rate
             FROM player_pairings
             WHERE guild_id = ?2 AND (player1_id = ?1 OR player2_id = ?1)
               AND games_together >= ?3
             {suffix}
             LIMIT ?4"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(
                params![
                    discord_id,
                    Self::normalize_guild_id(guild_id),
                    min_games,
                    limit
                ],
                |row| {
                    Ok(TeammatePairing {
                        teammate_id: row.get(0)?,
                        games_together: row.get(1)?,
                        wins_together: row.get(2)?,
                        win_rate: row.get(3)?,
                    })
                },
            )?
            .collect()
    }

    pub fn get_best_matchups(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<OpponentPairing>, rusqlite::Error> {
        self.query_opponents(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(
                 CASE WHEN player1_id = ?1
                      THEN player1_wins_against
                      ELSE games_against - player1_wins_against END AS REAL
             ) / games_against > 0.5
             ORDER BY win_rate DESC, games_against DESC",
        )
    }

    pub fn get_worst_matchups(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<OpponentPairing>, rusqlite::Error> {
        self.query_opponents(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(
                 CASE WHEN player1_id = ?1
                      THEN player1_wins_against
                      ELSE games_against - player1_wins_against END AS REAL
             ) / games_against < 0.5
             ORDER BY win_rate ASC, games_against DESC",
        )
    }

    pub fn get_most_played_against(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<OpponentPairing>, rusqlite::Error> {
        self.query_opponents(
            discord_id,
            guild_id,
            min_games,
            limit,
            "ORDER BY games_against DESC, win_rate DESC",
        )
    }

    pub fn get_evenly_matched_opponents(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
    ) -> Result<Vec<OpponentPairing>, rusqlite::Error> {
        self.query_opponents(
            discord_id,
            guild_id,
            min_games,
            limit,
            "AND CAST(
                 CASE WHEN player1_id = ?1
                      THEN player1_wins_against
                      ELSE games_against - player1_wins_against END AS REAL
             ) / games_against = 0.5
             ORDER BY games_against DESC",
        )
    }

    fn query_opponents(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: i64,
        suffix: &str,
    ) -> Result<Vec<OpponentPairing>, rusqlite::Error> {
        let sql = format!(
            "SELECT CASE WHEN player1_id = ?1 THEN player2_id ELSE player1_id END,
                    games_against,
                    CASE WHEN player1_id = ?1
                         THEN player1_wins_against
                         ELSE games_against - player1_wins_against END,
                    CAST(
                        CASE WHEN player1_id = ?1
                             THEN player1_wins_against
                             ELSE games_against - player1_wins_against END AS REAL
                    ) / games_against AS win_rate
             FROM player_pairings
             WHERE guild_id = ?2 AND (player1_id = ?1 OR player2_id = ?1)
               AND games_against >= ?3
             {suffix}
             LIMIT ?4"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(
                params![
                    discord_id,
                    Self::normalize_guild_id(guild_id),
                    min_games,
                    limit
                ],
                |row| {
                    Ok(OpponentPairing {
                        opponent_id: row.get(0)?,
                        games_against: row.get(1)?,
                        wins_against: row.get(2)?,
                        win_rate: row.get(3)?,
                    })
                },
            )?
            .collect()
    }

    pub fn get_pairing_counts(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
    ) -> Result<PairingCounts, rusqlite::Error> {
        let connection = self.connection()?;
        connection.query_row(
            "SELECT COUNT(CASE WHEN games_together >= ?1 THEN 1 END),
                    COUNT(CASE WHEN games_against >= ?1 THEN 1 END)
             FROM player_pairings
             WHERE guild_id = ?2 AND (player1_id = ?3 OR player2_id = ?3)",
            params![min_games, Self::normalize_guild_id(guild_id), discord_id],
            |row| {
                Ok(PairingCounts {
                    unique_teammates: row.get(0)?,
                    unique_opponents: row.get(1)?,
                })
            },
        )
    }

    pub fn get_head_to_head(
        &self,
        player1_id: i64,
        player2_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pairing>, rusqlite::Error> {
        let (player1_id, player2_id) = Self::canonical_pair(player1_id, player2_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT player1_id, player2_id, games_together, wins_together,
                        games_against, player1_wins_against, last_match_id
                 FROM player_pairings
                 WHERE guild_id = ?1 AND player1_id = ?2 AND player2_id = ?3",
                params![Self::normalize_guild_id(guild_id), player1_id, player2_id],
                row_to_pairing,
            )
            .optional()
    }

    /// Rebuild all aggregates from completed matches in one guild.
    pub fn rebuild_all_pairings(&self, guild_id: Option<i64>) -> Result<usize, rusqlite::Error> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        transaction.execute(
            "DELETE FROM player_pairings WHERE guild_id = ?1",
            params![guild_id],
        )?;

        let rows = {
            let mut statement = transaction.prepare(
                "SELECT m.match_id, m.winning_team, mp.discord_id, mp.team_number
                 FROM matches AS m
                 JOIN match_participants AS mp ON m.match_id = mp.match_id
                 WHERE m.guild_id = ?1 AND m.winning_team IS NOT NULL
                 ORDER BY m.match_id",
            )?;
            statement
                .query_map(params![guild_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<Vec<(i64, i64, i64, i64)>, _>>()?
        };

        let mut matches = BTreeMap::<i64, (i64, Vec<i64>, Vec<i64>)>::new();
        for (match_id, winning_team, discord_id, team_number) in rows {
            let entry = matches
                .entry(match_id)
                .or_insert_with(|| (winning_team, Vec::new(), Vec::new()));
            if team_number == 1 {
                entry.1.push(discord_id);
            } else {
                entry.2.push(discord_id);
            }
        }

        for (match_id, (winning_team, team1_ids, team2_ids)) in matches {
            update_match_pairings(
                &transaction,
                match_id,
                guild_id,
                &team1_ids,
                &team2_ids,
                winning_team,
            )?;
        }

        let count = transaction.query_row(
            "SELECT COUNT(*) FROM player_pairings WHERE guild_id = ?1",
            params![guild_id],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.commit()?;
        usize::try_from(count).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })
    }
}

fn row_to_pairing(row: &rusqlite::Row<'_>) -> Result<Pairing, rusqlite::Error> {
    Ok(Pairing {
        player1_id: row.get(0)?,
        player2_id: row.get(1)?,
        games_together: row.get(2)?,
        wins_together: row.get(3)?,
        games_against: row.get(4)?,
        player1_wins_against: row.get(5)?,
        last_match_id: row.get(6)?,
    })
}

fn update_match_pairings(
    connection: &Connection,
    match_id: i64,
    guild_id: i64,
    team1_ids: &[i64],
    team2_ids: &[i64],
    winning_team: i64,
) -> Result<(), rusqlite::Error> {
    let team1_won = winning_team == 1;
    for (index, player1_id) in team1_ids.iter().enumerate() {
        for player2_id in &team1_ids[index + 1..] {
            update_together(
                connection,
                guild_id,
                *player1_id,
                *player2_id,
                team1_won,
                match_id,
            )?;
        }
    }

    let team2_won = winning_team == 2;
    for (index, player1_id) in team2_ids.iter().enumerate() {
        for player2_id in &team2_ids[index + 1..] {
            update_together(
                connection,
                guild_id,
                *player1_id,
                *player2_id,
                team2_won,
                match_id,
            )?;
        }
    }

    for player1_id in team1_ids {
        for player2_id in team2_ids {
            update_against(
                connection,
                guild_id,
                *player1_id,
                *player2_id,
                team1_won,
                match_id,
            )?;
        }
    }
    Ok(())
}

fn update_together(
    connection: &Connection,
    guild_id: i64,
    id1: i64,
    id2: i64,
    won: bool,
    match_id: i64,
) -> Result<(), rusqlite::Error> {
    let (player1_id, player2_id) = PairingsRepository::canonical_pair(id1, id2);
    connection.execute(
        "INSERT INTO player_pairings (
             guild_id, player1_id, player2_id,
             games_together, wins_together, last_match_id
         ) VALUES (?1, ?2, ?3, 1, ?4, ?5)
         ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
             games_together = games_together + 1,
             wins_together = wins_together + ?4,
             last_match_id = ?5,
             updated_at = CURRENT_TIMESTAMP",
        params![guild_id, player1_id, player2_id, i64::from(won), match_id],
    )?;
    Ok(())
}

fn update_against(
    connection: &Connection,
    guild_id: i64,
    id1: i64,
    id2: i64,
    id1_won: bool,
    match_id: i64,
) -> Result<(), rusqlite::Error> {
    let (player1_id, player2_id) = PairingsRepository::canonical_pair(id1, id2);
    let player1_won = if id1 == player1_id { id1_won } else { !id1_won };
    connection.execute(
        "INSERT INTO player_pairings (
             guild_id, player1_id, player2_id,
             games_against, player1_wins_against, last_match_id
         ) VALUES (?1, ?2, ?3, 1, ?4, ?5)
         ON CONFLICT(guild_id, player1_id, player2_id) DO UPDATE SET
             games_against = games_against + 1,
             player1_wins_against = player1_wins_against + ?4,
             last_match_id = ?5,
             updated_at = CURRENT_TIMESTAMP",
        params![
            guild_id,
            player1_id,
            player2_id,
            i64::from(player1_won),
            match_id
        ],
    )?;
    Ok(())
}

/// Storage boundary used by the command-oriented service.
pub trait PairingsStore {
    fn get_pairings_for_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<Pairing>, rusqlite::Error>;

    fn get_head_to_head(
        &self,
        player1_id: i64,
        player2_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pairing>, rusqlite::Error>;

    fn rebuild_all_pairings(&self, guild_id: Option<i64>) -> Result<usize, rusqlite::Error>;
}

impl PairingsStore for PairingsRepository {
    fn get_pairings_for_player(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<Pairing>, rusqlite::Error> {
        PairingsRepository::get_pairings_for_player(self, discord_id, guild_id)
    }

    fn get_head_to_head(
        &self,
        player1_id: i64,
        player2_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pairing>, rusqlite::Error> {
        PairingsRepository::get_head_to_head(self, player1_id, player2_id, guild_id)
    }

    fn rebuild_all_pairings(&self, guild_id: Option<i64>) -> Result<usize, rusqlite::Error> {
        PairingsRepository::rebuild_all_pairings(self, guild_id)
    }
}

/// Thin command service mirroring `services/pairings_service.py`.
#[derive(Clone, Debug)]
pub struct PairingsService<S> {
    store: S,
}

impl<S: PairingsStore> PairingsService<S> {
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get_head_to_head(
        &self,
        player1_id: i64,
        player2_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pairing>, rusqlite::Error> {
        self.store.get_head_to_head(
            player1_id,
            player2_id,
            Some(PairingsRepository::normalize_guild_id(guild_id)),
        )
    }

    pub fn rebuild_all_pairings(&self, guild_id: Option<i64>) -> Result<usize, rusqlite::Error> {
        self.store
            .rebuild_all_pairings(Some(PairingsRepository::normalize_guild_id(guild_id)))
    }

    /// Build all profile categories from one repository read.
    pub fn get_player_pairing_summary(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
        limit: usize,
    ) -> Result<PlayerPairingSummary, rusqlite::Error> {
        let pairings = self.store.get_pairings_for_player(
            discord_id,
            Some(PairingsRepository::normalize_guild_id(guild_id)),
        )?;
        Ok(build_summary(discord_id, pairings, min_games, limit))
    }
}

fn build_summary(
    discord_id: i64,
    pairings: Vec<Pairing>,
    min_games: i64,
    limit: usize,
) -> PlayerPairingSummary {
    let mut teammates = Vec::new();
    let mut opponents = Vec::new();
    for pairing in pairings {
        let other_id = if pairing.player1_id == discord_id {
            pairing.player2_id
        } else {
            pairing.player1_id
        };
        if pairing.games_together >= min_games {
            teammates.push(TeammatePairing {
                teammate_id: other_id,
                games_together: pairing.games_together,
                wins_together: pairing.wins_together,
                win_rate: pairing.wins_together as f64 / pairing.games_together as f64,
            });
        }
        if pairing.games_against >= min_games {
            let wins_against = pairing.queried_player_wins_against(discord_id);
            opponents.push(OpponentPairing {
                opponent_id: other_id,
                games_against: pairing.games_against,
                wins_against,
                win_rate: wins_against as f64 / pairing.games_against as f64,
            });
        }
    }

    let mut best_teammates = teammates
        .iter()
        .filter(|row| row.win_rate > 0.5)
        .cloned()
        .collect::<Vec<_>>();
    best_teammates.sort_by(teammate_rate_desc);
    best_teammates.truncate(limit);

    let mut worst_teammates = teammates
        .iter()
        .filter(|row| row.win_rate < 0.5)
        .cloned()
        .collect::<Vec<_>>();
    worst_teammates.sort_by(teammate_rate_asc);
    worst_teammates.truncate(limit);

    let mut best_matchups = opponents
        .iter()
        .filter(|row| row.win_rate > 0.5)
        .cloned()
        .collect::<Vec<_>>();
    best_matchups.sort_by(opponent_rate_desc);
    best_matchups.truncate(limit);

    let mut worst_matchups = opponents
        .iter()
        .filter(|row| row.win_rate < 0.5)
        .cloned()
        .collect::<Vec<_>>();
    worst_matchups.sort_by(opponent_rate_asc);
    worst_matchups.truncate(limit);

    let mut most_played_with = teammates.clone();
    most_played_with.sort_by(|left, right| {
        right
            .games_together
            .cmp(&left.games_together)
            .then_with(|| right.win_rate.total_cmp(&left.win_rate))
            .then_with(|| left.teammate_id.cmp(&right.teammate_id))
    });
    most_played_with.truncate(limit);

    let mut most_played_against = opponents.clone();
    most_played_against.sort_by(|left, right| {
        right
            .games_against
            .cmp(&left.games_against)
            .then_with(|| right.win_rate.total_cmp(&left.win_rate))
            .then_with(|| left.opponent_id.cmp(&right.opponent_id))
    });
    most_played_against.truncate(limit);

    let mut even_teammates = teammates
        .iter()
        .filter(|row| row.wins_together * 2 == row.games_together)
        .cloned()
        .collect::<Vec<_>>();
    even_teammates.sort_by(|left, right| {
        right
            .games_together
            .cmp(&left.games_together)
            .then_with(|| left.teammate_id.cmp(&right.teammate_id))
    });
    even_teammates.truncate(limit);

    let mut even_opponents = opponents
        .iter()
        .filter(|row| row.wins_against * 2 == row.games_against)
        .cloned()
        .collect::<Vec<_>>();
    even_opponents.sort_by(|left, right| {
        right
            .games_against
            .cmp(&left.games_against)
            .then_with(|| left.opponent_id.cmp(&right.opponent_id))
    });
    even_opponents.truncate(limit);

    PlayerPairingSummary {
        best_teammates,
        worst_teammates,
        best_matchups,
        worst_matchups,
        most_played_with,
        most_played_against,
        even_teammates,
        even_opponents,
        counts: PairingCounts {
            unique_teammates: i64::try_from(teammates.len()).unwrap_or(i64::MAX),
            unique_opponents: i64::try_from(opponents.len()).unwrap_or(i64::MAX),
        },
    }
}

fn teammate_rate_desc(left: &TeammatePairing, right: &TeammatePairing) -> Ordering {
    right
        .win_rate
        .total_cmp(&left.win_rate)
        .then_with(|| right.games_together.cmp(&left.games_together))
        .then_with(|| left.teammate_id.cmp(&right.teammate_id))
}

fn teammate_rate_asc(left: &TeammatePairing, right: &TeammatePairing) -> Ordering {
    left.win_rate
        .total_cmp(&right.win_rate)
        .then_with(|| right.games_together.cmp(&left.games_together))
        .then_with(|| left.teammate_id.cmp(&right.teammate_id))
}

fn opponent_rate_desc(left: &OpponentPairing, right: &OpponentPairing) -> Ordering {
    right
        .win_rate
        .total_cmp(&left.win_rate)
        .then_with(|| right.games_against.cmp(&left.games_against))
        .then_with(|| left.opponent_id.cmp(&right.opponent_id))
}

fn opponent_rate_asc(left: &OpponentPairing, right: &OpponentPairing) -> Ordering {
    left.win_rate
        .total_cmp(&right.win_rate)
        .then_with(|| right.games_against.cmp(&left.games_against))
        .then_with(|| left.opponent_id.cmp(&right.opponent_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    const GUILD_ID: i64 = 987_654_321_012_345_678;
    const SECONDARY_GUILD_ID: i64 = 876_543_210_123_456_789;
    const TEAM1: [i64; 5] = [1, 2, 3, 4, 5];
    const TEAM2: [i64; 5] = [6, 7, 8, 9, 10];

    fn database() -> NamedTempFile {
        let file = NamedTempFile::new().expect("create pairings test database");
        let connection = Connection::open(file.path()).expect("open pairings test database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     winning_team INTEGER,
                     guild_id INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     team_number INTEGER NOT NULL,
                     PRIMARY KEY (match_id, discord_id)
                 );
                 CREATE TABLE player_pairings (
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     player1_id INTEGER NOT NULL,
                     player2_id INTEGER NOT NULL,
                     games_together INTEGER DEFAULT 0,
                     wins_together INTEGER DEFAULT 0,
                     games_against INTEGER DEFAULT 0,
                     player1_wins_against INTEGER DEFAULT 0,
                     last_match_id INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (guild_id, player1_id, player2_id),
                     CHECK (player1_id < player2_id)
                 );",
            )
            .expect("create Python-compatible pairings schema");
        file
    }

    fn update(
        repository: &PairingsRepository,
        match_id: i64,
        guild_id: Option<i64>,
        team1: &[i64],
        team2: &[i64],
        winning_team: i64,
    ) {
        repository
            .update_pairings_for_match(match_id, guild_id, team1, team2, winning_team)
            .expect("update pairings for match");
    }

    fn head(
        repository: &PairingsRepository,
        player1_id: i64,
        player2_id: i64,
        guild_id: Option<i64>,
    ) -> Pairing {
        repository
            .get_head_to_head(player1_id, player2_id, guild_id)
            .expect("query head-to-head")
            .expect("head-to-head row exists")
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_pairing(
        file: &NamedTempFile,
        guild_id: i64,
        player1_id: i64,
        player2_id: i64,
        games_together: i64,
        wins_together: i64,
        games_against: i64,
        player1_wins_against: i64,
    ) {
        let connection = Connection::open(file.path()).expect("open pairings test database");
        connection
            .execute(
                "INSERT INTO player_pairings (
                     guild_id, player1_id, player2_id,
                     games_together, wins_together,
                     games_against, player1_wins_against
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    guild_id,
                    player1_id,
                    player2_id,
                    games_together,
                    wins_together,
                    games_against,
                    player1_wins_against
                ],
            )
            .expect("insert pairing aggregate");
    }

    fn record_match(
        file: &NamedTempFile,
        match_id: i64,
        guild_id: i64,
        team1: &[i64],
        team2: &[i64],
        winning_team: i64,
    ) {
        let mut connection = Connection::open(file.path()).expect("open pairings test database");
        let transaction = connection.transaction().expect("begin match insert");
        transaction
            .execute(
                "INSERT INTO matches (match_id, winning_team, guild_id) VALUES (?1, ?2, ?3)",
                params![match_id, winning_team, guild_id],
            )
            .expect("insert match");
        for (team_number, players) in [(1_i64, team1), (2_i64, team2)] {
            for discord_id in players {
                transaction
                    .execute(
                        "INSERT INTO match_participants (match_id, discord_id, team_number)
                         VALUES (?1, ?2, ?3)",
                        params![match_id, discord_id, team_number],
                    )
                    .expect("insert match participant");
            }
        }
        transaction.commit().expect("commit match insert");
    }

    #[test]
    fn test_update_pairings_for_match_teammates() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, Some(GUILD_ID), &TEAM1, &TEAM2, 1);

        let winners = head(&repository, 1, 2, Some(GUILD_ID));
        assert_eq!(winners.games_together, 1);
        assert_eq!(winners.wins_together, 1);

        let losers = head(&repository, 6, 7, Some(GUILD_ID));
        assert_eq!(losers.games_together, 1);
        assert_eq!(losers.wins_together, 0);
    }

    #[test]
    fn test_update_pairings_for_match_opponents() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, Some(GUILD_ID), &TEAM1, &TEAM2, 1);

        let from_winner = head(&repository, 1, 6, Some(GUILD_ID));
        assert_eq!(from_winner.games_against, 1);
        assert_eq!(from_winner.queried_player_wins_against(1), 1);

        let from_loser = head(&repository, 6, 1, Some(GUILD_ID));
        assert_eq!(from_loser.queried_player_wins_against(6), 0);
    }

    #[test]
    fn test_multiple_matches_accumulate() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, Some(GUILD_ID), &TEAM1, &TEAM2, 1);
        update(&repository, 2, Some(GUILD_ID), &TEAM1, &TEAM2, 2);
        update(&repository, 3, Some(GUILD_ID), &TEAM1, &TEAM2, 1);

        let teammates = head(&repository, 1, 2, Some(GUILD_ID));
        assert_eq!((teammates.games_together, teammates.wins_together), (3, 2));
        assert_eq!(teammates.last_match_id, Some(3));

        let opponents = head(&repository, 1, 6, Some(GUILD_ID));
        assert_eq!(
            (opponents.games_against, opponents.player1_wins_against),
            (3, 2)
        );
    }

    #[test]
    fn test_min_games_filter() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 100, Some(GUILD_ID), &TEAM1, &TEAM2, 1);
        update(&repository, 101, Some(GUILD_ID), &TEAM1, &TEAM2, 1);

        assert!(
            repository
                .get_best_teammates(1, Some(GUILD_ID), 3, 5)
                .expect("query best teammates")
                .is_empty()
        );
        assert!(
            !repository
                .get_best_teammates(1, Some(GUILD_ID), 2, 5)
                .expect("query best teammates")
                .is_empty()
        );
    }

    #[test]
    fn test_canonical_ordering() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(
            &repository,
            1,
            Some(GUILD_ID),
            &[200, 100, 3, 4, 5],
            &TEAM2,
            1,
        );

        let forward = head(&repository, 100, 200, Some(GUILD_ID));
        assert_eq!((forward.player1_id, forward.player2_id), (100, 200));
        let reverse = head(&repository, 200, 100, Some(GUILD_ID));
        assert_eq!((reverse.player1_id, reverse.player2_id), (100, 200));
    }

    #[test]
    fn test_service_rebuild_all_pairings_threads_guild_id() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        for index in 0..3 {
            record_match(
                &file,
                index + 1,
                GUILD_ID,
                &TEAM1,
                &TEAM2,
                if index % 2 == 0 { 1 } else { 2 },
            );
        }

        let service = PairingsService::new(repository.clone());
        assert_eq!(
            service
                .rebuild_all_pairings(Some(GUILD_ID))
                .expect("rebuild guild pairings"),
            45
        );
        assert_eq!(head(&repository, 1, 2, Some(GUILD_ID)).games_together, 3);
        assert_eq!(
            service
                .rebuild_all_pairings(Some(SECONDARY_GUILD_ID))
                .expect("rebuild empty guild"),
            0
        );
        assert_eq!(
            head(&repository, 1, 2, Some(GUILD_ID)).games_together,
            3,
            "rebuilding another guild must not clear the requested guild"
        );
    }

    #[test]
    fn test_get_pairings_for_player() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, Some(GUILD_ID), &TEAM1, &TEAM2, 1);

        assert_eq!(
            repository
                .get_pairings_for_player(1, Some(GUILD_ID))
                .expect("get player pairings")
                .len(),
            9
        );
    }

    #[test]
    fn test_get_pairings_for_players_batches_group() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, Some(GUILD_ID), &TEAM1, &TEAM2, 1);
        let before = repository.observed_connection_count();

        let pairings = repository
            .get_pairings_for_players(&[6, 2, 1, 6], Some(GUILD_ID))
            .expect("get batched pairings");
        assert_eq!(
            pairings.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([(1, 2), (1, 6), (2, 6)])
        );
        assert_eq!(pairings[&(1, 2)].games_together, 1);
        assert_eq!(pairings[&(1, 6)].games_against, 1);
        assert_eq!(repository.observed_connection_count(), before + 1);

        assert!(
            repository
                .get_pairings_for_players(&[1], Some(GUILD_ID))
                .expect("short-circuit one-player batch")
                .is_empty()
        );
        assert_eq!(repository.observed_connection_count(), before + 1);
    }

    #[test]
    fn test_best_worst_teammates_no_overlap() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        for (teammate_id, wins) in [(2, 4), (3, 3), (4, 2), (5, 1), (6, 0)] {
            insert_pairing(&file, GUILD_ID, 1, teammate_id, 4, wins, 0, 0);
        }

        let best = repository
            .get_best_teammates(1, Some(GUILD_ID), 4, 10)
            .expect("get best teammates");
        let worst = repository
            .get_worst_teammates(1, Some(GUILD_ID), 4, 10)
            .expect("get worst teammates");
        let best_ids = best
            .iter()
            .map(|row| row.teammate_id)
            .collect::<BTreeSet<_>>();
        let worst_ids = worst
            .iter()
            .map(|row| row.teammate_id)
            .collect::<BTreeSet<_>>();

        assert!(best_ids.is_disjoint(&worst_ids));
        assert_eq!((best[0].teammate_id, best[0].win_rate), (2, 1.0));
        assert_eq!((worst[0].teammate_id, worst[0].win_rate), (6, 0.0));
        assert!(best_ids.contains(&2));
        assert!(!worst_ids.contains(&2));
        assert!(!best_ids.contains(&4));
        assert!(!worst_ids.contains(&4));
        assert!(worst_ids.contains(&6));
        assert!(!best_ids.contains(&6));
    }

    #[test]
    fn test_best_worst_matchups_no_overlap() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        for (opponent_id, wins) in [(10, 4), (11, 2), (12, 0)] {
            insert_pairing(&file, GUILD_ID, 1, opponent_id, 0, 0, 4, wins);
        }

        let best = repository
            .get_best_matchups(1, Some(GUILD_ID), 4, 10)
            .expect("get best matchups");
        let worst = repository
            .get_worst_matchups(1, Some(GUILD_ID), 4, 10)
            .expect("get worst matchups");
        let best_ids = best
            .iter()
            .map(|row| row.opponent_id)
            .collect::<BTreeSet<_>>();
        let worst_ids = worst
            .iter()
            .map(|row| row.opponent_id)
            .collect::<BTreeSet<_>>();

        assert!(best_ids.is_disjoint(&worst_ids));
        assert_eq!(best[0].win_rate, 1.0);
        assert_eq!(worst[0].win_rate, 0.0);
        assert!(best_ids.contains(&10));
        assert!(!worst_ids.contains(&10));
        assert!(!best_ids.contains(&11));
        assert!(!worst_ids.contains(&11));
        assert!(worst_ids.contains(&12));
        assert!(!best_ids.contains(&12));
    }

    #[test]
    fn test_win_rate_boundary_filtering() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        insert_pairing(&file, GUILD_ID, 1, 2, 4, 2, 0, 0);

        let pairing = head(&repository, 1, 2, Some(GUILD_ID));
        assert_eq!((pairing.games_together, pairing.wins_together), (4, 2));
        assert_eq!(
            pairing.wins_together as f64 / pairing.games_together as f64,
            0.5
        );
        assert!(
            repository
                .get_best_teammates(1, Some(GUILD_ID), 4, 10)
                .expect("get best teammates")
                .is_empty()
        );
        assert!(
            repository
                .get_worst_teammates(1, Some(GUILD_ID), 4, 10)
                .expect("get worst teammates")
                .is_empty()
        );
    }

    #[test]
    fn test_get_most_played_with() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        insert_pairing(&file, GUILD_ID, 1, 2, 6, 6, 0, 0);
        insert_pairing(&file, GUILD_ID, 1, 3, 3, 3, 0, 0);

        let most_played = repository
            .get_most_played_with(1, Some(GUILD_ID), 3, 5)
            .expect("get most-played teammates");
        assert!(most_played.len() >= 2);
        assert_eq!(
            (most_played[0].teammate_id, most_played[0].games_together),
            (2, 6)
        );
    }

    #[test]
    fn test_get_most_played_against() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        insert_pairing(&file, GUILD_ID, 1, 10, 0, 0, 5, 5);
        insert_pairing(&file, GUILD_ID, 1, 15, 0, 0, 3, 3);

        let most_played = repository
            .get_most_played_against(1, Some(GUILD_ID), 3, 5)
            .expect("get most-played opponents");
        assert!(most_played.len() >= 2);
        assert_eq!(
            (most_played[0].opponent_id, most_played[0].games_against),
            (10, 5)
        );
    }

    #[test]
    fn test_get_evenly_matched_teammates_and_opponents() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        insert_pairing(&file, GUILD_ID, 1, 2, 4, 2, 0, 0);
        insert_pairing(&file, GUILD_ID, 1, 6, 0, 0, 4, 2);

        let teammates = repository
            .get_evenly_matched_teammates(1, Some(GUILD_ID), 4, 5)
            .expect("get even teammates");
        assert!(teammates.iter().any(|row| row.teammate_id == 2));
        let opponents = repository
            .get_evenly_matched_opponents(1, Some(GUILD_ID), 4, 5)
            .expect("get even opponents");
        assert!(opponents.iter().any(|row| row.opponent_id == 6));
    }

    #[test]
    fn test_get_pairing_counts() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        for teammate_id in 2..=5 {
            insert_pairing(&file, GUILD_ID, 1, teammate_id, 1, 1, 0, 0);
        }
        for opponent_id in 6..=10 {
            insert_pairing(&file, GUILD_ID, 1, opponent_id, 0, 0, 1, 1);
        }

        assert_eq!(
            repository
                .get_pairing_counts(1, Some(GUILD_ID), 1)
                .expect("get pairing counts"),
            PairingCounts {
                unique_teammates: 4,
                unique_opponents: 5,
            }
        );
    }

    #[test]
    fn test_guild_id_none_normalized_to_zero() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        update(&repository, 1, None, &TEAM1, &TEAM2, 1);

        let via_none = repository
            .get_head_to_head(1, 2, None)
            .expect("query none guild");
        let via_zero = repository
            .get_head_to_head(1, 2, Some(0))
            .expect("query guild zero");
        assert!(via_none.is_some());
        assert_eq!(via_zero, via_none);
    }

    #[test]
    fn signed_64_bit_ids_round_trip_and_reverse_perspective() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        let low = i64::MIN + 1;
        let high = i64::MAX - 1;
        update(&repository, i64::MAX, Some(GUILD_ID), &[high], &[low], 1);

        let pairing = head(&repository, high, low, Some(GUILD_ID));
        assert_eq!((pairing.player1_id, pairing.player2_id), (low, high));
        assert_eq!(pairing.last_match_id, Some(i64::MAX));
        assert_eq!(pairing.queried_player_wins_against(high), 1);
        assert_eq!(pairing.queried_player_wins_against(low), 0);
    }

    #[test]
    fn service_summary_is_stable_and_uses_one_batched_read() {
        let file = database();
        let repository = PairingsRepository::new(file.path());
        insert_pairing(&file, GUILD_ID, 1, 3, 4, 3, 4, 3);
        insert_pairing(&file, GUILD_ID, 1, 2, 4, 3, 4, 3);
        insert_pairing(&file, GUILD_ID, 1, 5, 4, 2, 4, 2);
        insert_pairing(&file, GUILD_ID, 1, 4, 4, 2, 4, 2);
        let before = repository.observed_connection_count();

        let summary = PairingsService::new(repository.clone())
            .get_player_pairing_summary(1, Some(GUILD_ID), 4, 10)
            .expect("build service summary");
        assert_eq!(repository.observed_connection_count(), before + 1);
        assert_eq!(
            summary
                .best_teammates
                .iter()
                .map(|row| row.teammate_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            summary
                .even_teammates
                .iter()
                .map(|row| row.teammate_id)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(summary.counts.unique_teammates, 4);
        assert!(summary.best_teammates.iter().all(|row| row.win_rate > 0.5));
        assert!(summary.worst_teammates.iter().all(|row| row.win_rate < 0.5));
    }
}
