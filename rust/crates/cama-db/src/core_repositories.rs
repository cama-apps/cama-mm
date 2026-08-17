//! Existing-schema SQLite parity for Python's core player and match repositories.
//!
//! Python remains the migration and schema authority.  Every entry point in
//! this module opens an existing file through [`crate::open_runtime_connection`];
//! production code never creates tables, alters schema, or creates a database.
//! Multi-statement writes acquire a `BEGIN IMMEDIATE` transaction so readers
//! never observe a half-written match, streak update, delete, or coin transfer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_domain::player::{OPENSKILL_DISPLAY_SCALE, Player};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;
use crate::referrals::{
    aggregate_referral_changes, encode_referral_jc_changes, settle_first_match_referrals,
};

const BULK_CHUNK_SIZE: usize = 900;
const NEW_PLAYER_EXCLUSION_BOOST: i64 = 5;
const OPENSKILL_ALGORITHM_VERSION: i64 = 5;
const OPENSKILL_ALGORITHM_FINGERPRINT: &str = "ffdaf6752ef51115";
const DEFAULT_STREAK_MULTIPLIER_PER_GAME: f64 = 0.30;
const DEFAULT_STREAK_THRESHOLD: i64 = 3;

#[derive(Debug, Error)]
pub enum CoreRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("player with Discord ID {discord_id} already exists in guild {guild_id}")]
    DuplicatePlayer { discord_id: i64, guild_id: i64 },
    #[error("{0}")]
    InvalidInput(String),
    #[error("invalid persisted JSON: {0}")]
    InvalidJson(String),
}

/// Context copied into Python's trigger-backed economy ledger for a wallet
/// mutation.  The context row is transaction-local: callers install it only
/// around the balance update and the repository clears it before commit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalanceLedgerContext {
    pub source: Option<String>,
    pub actor_id: Option<i64>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub reason: Option<String>,
    pub metadata: Option<String>,
}

/// A small insertion-ordered mapping matching Python `dict` iteration.
///
/// Core repository bulk methods de-duplicate at the first occurrence and must
/// preserve that order even for signed Discord IDs.  `BTreeMap` and `HashMap`
/// cannot express that contract directly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrderedMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K: PartialEq, V> OrderedMap<K, V> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some((_, current)) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| candidate == &key)
        {
            return Some(std::mem::replace(current, value));
        }
        self.entries.push((key, value));
        None
    }

    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }
}

impl<K, V> IntoIterator for OrderedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewPlayer {
    pub discord_id: i64,
    pub discord_username: String,
    pub guild_id: Option<i64>,
    pub dotabuff_url: Option<String>,
    pub steam_id: Option<i64>,
    pub initial_mmr: Option<i64>,
    pub preferred_roles: Option<Vec<String>>,
    pub main_role: Option<String>,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub glicko_volatility: Option<f64>,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
}

impl NewPlayer {
    #[must_use]
    pub fn new(
        discord_id: i64,
        discord_username: impl Into<String>,
        guild_id: Option<i64>,
    ) -> Self {
        Self {
            discord_id,
            discord_username: discord_username.into(),
            guild_id,
            dotabuff_url: None,
            steam_id: None,
            initial_mmr: None,
            preferred_roles: None,
            main_role: None,
            glicko_rating: None,
            glicko_rd: None,
            glicko_volatility: None,
            os_mu: None,
            os_sigma: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchRatingInput {
    pub current_mmr: Value,
    pub glicko_rating: Value,
    pub glicko_rd: Value,
    pub glicko_volatility: Value,
    pub os_mu: Value,
    pub os_sigma: Value,
    pub last_match_date: Value,
    pub created_at: Value,
    pub first_calibrated_at: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReminderTimestamps {
    pub last_wheel_spin: Value,
    pub last_trivia_session: Value,
}

/// Result of the atomic regular-wheel cooldown claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WheelSpinClaim {
    MissingPlayer,
    Claimed { previous_spin: Option<i64> },
    Cooldown { last_spin: i64 },
}

/// Python tuple order: `(rating, rating_deviation, volatility)`.
pub type GlickoRating = (f64, Option<f64>, Option<f64>);

impl Default for ReminderTimestamps {
    fn default() -> Self {
        Self {
            last_wheel_spin: Value::Null,
            last_trivia_session: Value::Null,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShuffleInputs {
    pub players: Vec<Player>,
    pub last_match_dates: OrderedMap<i64, Value>,
    pub exclusion_counts: OrderedMap<i64, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsernameMatch {
    pub discord_id: i64,
    pub discord_username: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StealResult {
    pub amount: i64,
    pub thief_new_balance: i64,
    pub victim_new_balance: i64,
}

#[derive(Clone, Debug)]
pub struct PlayerRepository {
    path: PathBuf,
}

impl PlayerRepository {
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

    fn connection(&self) -> Result<Connection, CoreRepositoryError> {
        Ok(open_runtime_connection(&self.path)?)
    }

    pub fn add(&self, player: &NewPlayer) -> Result<(), CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(player.guild_id);
        let connection = self.connection()?;
        if connection
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![player.discord_id, guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(CoreRepositoryError::DuplicatePlayer {
                discord_id: player.discord_id,
                guild_id,
            });
        }

        let roles = player
            .preferred_roles
            .as_deref()
            .filter(|roles| !roles.is_empty())
            .map(encode_string_array);
        match connection.execute(
            "INSERT INTO players (
                discord_id, guild_id, discord_username, dotabuff_url, steam_id,
                initial_mmr, current_mmr, preferred_roles, main_role,
                glicko_rating, glicko_rd, glicko_volatility, os_mu, os_sigma,
                os_rating_version, os_algorithm_fingerprint,
                exclusion_count, jopacoin_balance, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, 3, CURRENT_TIMESTAMP
             )",
            params![
                player.discord_id,
                guild_id,
                player.discord_username,
                player.dotabuff_url,
                player.steam_id,
                player.initial_mmr,
                roles,
                player.main_role,
                player.glicko_rating,
                player.glicko_rd,
                player.glicko_volatility,
                player.os_mu,
                player.os_sigma,
                OPENSKILL_ALGORITHM_VERSION,
                OPENSKILL_ALGORITHM_FINGERPRINT,
                NEW_PLAYER_EXCLUSION_BOOST,
            ],
        ) {
            Ok(_) => Ok(()),
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
            {
                Err(CoreRepositoryError::DuplicatePlayer {
                    discord_id: player.discord_id,
                    guild_id,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn get_by_id(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Player>, CoreRepositoryError> {
        let connection = self.connection()?;
        query_player_by_id(&connection, discord_id, Self::normalize_guild_id(guild_id))
    }

    pub fn get_by_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let unique_ids = unique_i64(discord_ids);
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut players_by_id = HashMap::with_capacity(unique_ids.len());
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "{} WHERE guild_id = ? AND discord_id IN ({})",
                PLAYER_SELECT,
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), player_from_row)?;
            for row in rows {
                let player = row?;
                if let Some(discord_id) = player.discord_id {
                    players_by_id.entry(discord_id).or_insert(player);
                }
            }
        }
        Ok(discord_ids
            .iter()
            .filter_map(|discord_id| players_by_id.get(discord_id).cloned())
            .collect())
    }

    /// Return every registered player in one guild using SQLite's natural
    /// row order, matching Python's unqualified `SELECT * FROM players`.
    ///
    /// Daily mana assignment consumes entropy in this order, so this is kept
    /// separate from the sorted leaderboard queries.
    pub fn get_all(&self, guild_id: Option<i64>) -> Result<Vec<Player>, CoreRepositoryError> {
        let connection = self.connection()?;
        let sql = format!("{PLAYER_SELECT} WHERE guild_id = ?1");
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map([Self::normalize_guild_id(guild_id)], player_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn exists(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn update_roles(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        roles: &[String],
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE players
             SET preferred_roles = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![
                encode_string_array(roles),
                discord_id,
                Self::normalize_guild_id(guild_id)
            ],
        )?;
        Ok(())
    }

    pub fn update_glicko_rating(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        rating: f64,
        rd: f64,
        volatility: f64,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE players
             SET glicko_rating = ?1, glicko_rd = ?2, glicko_volatility = ?3,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?4 AND guild_id = ?5",
            params![
                rating,
                rd,
                volatility,
                discord_id,
                Self::normalize_guild_id(guild_id)
            ],
        )?;
        Ok(())
    }

    pub fn get_glicko_rating(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<GlickoRating>, CoreRepositoryError> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT glicko_rating, glicko_rd, glicko_volatility
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(|(rating, rd, volatility)| rating.map(|rating| (rating, rd, volatility))))
    }

    pub fn get_match_rating_inputs(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, MatchRatingInput>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut found = HashMap::new();
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, current_mmr, glicko_rating, glicko_rd,
                        glicko_volatility, os_mu, os_sigma, last_match_date,
                        created_at, first_calibrated_at
                 FROM players
                 WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    MatchRatingInput {
                        current_mmr: row.get(1)?,
                        glicko_rating: row.get(2)?,
                        glicko_rd: row.get(3)?,
                        glicko_volatility: row.get(4)?,
                        os_mu: row.get(5)?,
                        os_sigma: row.get(6)?,
                        last_match_date: row.get(7)?,
                        created_at: row.get(8)?,
                        first_calibrated_at: row.get(9)?,
                    },
                ))
            })?;
            for row in rows {
                let (discord_id, input) = row?;
                found.insert(discord_id, input);
            }
        }
        for discord_id in unique_ids {
            if let Some(input) = found.remove(&discord_id) {
                result.insert(discord_id, input);
            }
        }
        Ok(result)
    }

    /// Snapshot every rating input and the guild OpenSkill revision from one
    /// read transaction so callers can reject stale pre-computed updates at
    /// commit time.
    pub fn get_match_rating_inputs_with_openskill_revision(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<(OrderedMap<i64, MatchRatingInput>, i64), CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut found = HashMap::new();
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, current_mmr, glicko_rating, glicko_rd,
                        glicko_volatility, os_mu, os_sigma, last_match_date,
                        created_at, first_calibrated_at
                 FROM players
                 WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = transaction.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            for row in statement.query_map(params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    MatchRatingInput {
                        current_mmr: row.get(1)?,
                        glicko_rating: row.get(2)?,
                        glicko_rd: row.get(3)?,
                        glicko_volatility: row.get(4)?,
                        os_mu: row.get(5)?,
                        os_sigma: row.get(6)?,
                        last_match_date: row.get(7)?,
                        created_at: row.get(8)?,
                        first_calibrated_at: row.get(9)?,
                    },
                ))
            })? {
                let (discord_id, input) = row?;
                found.insert(discord_id, input);
            }
        }
        let revision = transaction
            .query_row(
                "SELECT revision FROM openskill_rating_revisions WHERE guild_id = ?1",
                [guild_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        transaction.commit()?;
        let mut inputs = OrderedMap::new();
        for discord_id in unique_ids {
            if let Some(input) = found.remove(&discord_id) {
                inputs.insert(discord_id, input);
            }
        }
        Ok((inputs, revision))
    }

    pub fn get_balances_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, i64>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        for discord_id in &unique_ids {
            result.insert(*discord_id, 0);
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, COALESCE(jopacoin_balance, 0)
                 FROM players WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (discord_id, balance) = row?;
                result.insert(discord_id, balance);
            }
        }
        Ok(result)
    }

    pub fn get_reminder_timestamps_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, ReminderTimestamps>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        for discord_id in &unique_ids {
            result.insert(*discord_id, ReminderTimestamps::default());
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, last_wheel_spin, last_trivia_session
                 FROM players WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ReminderTimestamps {
                        last_wheel_spin: row.get(1)?,
                        last_trivia_session: row.get(2)?,
                    },
                ))
            })?;
            for row in rows {
                let (discord_id, timestamps) = row?;
                result.insert(discord_id, timestamps);
            }
        }
        Ok(result)
    }

    pub fn get_shuffle_inputs(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<ShuffleInputs, CoreRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(ShuffleInputs {
                players: Vec::new(),
                last_match_dates: OrderedMap::new(),
                exclusion_counts: OrderedMap::new(),
            });
        }
        let unique_ids = unique_i64(discord_ids);
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut rows_by_id = HashMap::with_capacity(unique_ids.len());
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT player.*, player.last_match_date, player.exclusion_count
                 FROM ({PLAYER_SELECT}) AS player
                 WHERE player.guild_id = ? AND player.discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                let player = player_from_row(row)?;
                Ok((
                    player.discord_id.unwrap_or_default(),
                    player,
                    row.get::<_, Value>(25)?,
                    row.get::<_, Option<i64>>(26)?.unwrap_or(0),
                ))
            })?;
            for row in rows {
                let (discord_id, player, last_match_date, exclusions) = row?;
                rows_by_id.insert(discord_id, (player, last_match_date, exclusions));
            }
        }

        let mut players = Vec::new();
        let mut last_match_dates = OrderedMap::new();
        let mut exclusion_counts = OrderedMap::new();
        for discord_id in discord_ids {
            if let Some((player, last_match_date, exclusions)) = rows_by_id.get(discord_id) {
                players.push(player.clone());
                last_match_dates.insert(*discord_id, last_match_date.clone());
                exclusion_counts.insert(*discord_id, *exclusions);
            }
        }
        Ok(ShuffleInputs {
            players,
            last_match_dates,
            exclusion_counts,
        })
    }

    pub fn get_by_username(
        &self,
        username: &str,
        guild_id: Option<i64>,
    ) -> Result<Vec<UsernameMatch>, CoreRepositoryError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let search = format!("%{}%", username.to_lowercase());
        let mut statement = connection.prepare(
            "SELECT discord_id, discord_username
             FROM players
             WHERE LOWER(discord_username) LIKE ?1 AND guild_id = ?2",
        )?;
        statement
            .query_map(params![search, Self::normalize_guild_id(guild_id)], |row| {
                Ok(UsernameMatch {
                    discord_id: row.get(0)?,
                    discord_username: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_exclusion_counts(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, i64>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, COALESCE(exclusion_count, 0)
                 FROM players WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            for row in statement.query_map(params_from_iter(parameters), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (discord_id, count) = row?;
                result.insert(discord_id, count);
            }
        }
        // Python's method follows SQLite row order, not input order.  Preserve
        // deterministic first-input order for the same logical mapping.
        let mut ordered = OrderedMap::new();
        for discord_id in unique_ids {
            if let Some(count) = result.get(&discord_id) {
                ordered.insert(discord_id, *count);
            }
        }
        Ok(ordered)
    }

    pub fn increment_exclusion_count(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), CoreRepositoryError> {
        self.update_counter(
            "exclusion_count = COALESCE(exclusion_count, 0) + 6",
            discord_id,
            guild_id,
        )
    }

    pub fn decay_exclusion_count(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), CoreRepositoryError> {
        self.update_counter(
            "exclusion_count = MAX(COALESCE(exclusion_count, 0) - 1, 0)",
            discord_id,
            guild_id,
        )
    }

    pub fn increment_wins(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), CoreRepositoryError> {
        self.update_counter("wins = wins + 1", discord_id, guild_id)
    }

    fn update_counter(
        &self,
        assignment: &str,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        let sql = format!(
            "UPDATE players SET {assignment}, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2"
        );
        connection.execute(
            &sql,
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    pub fn update_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    /// Add to a player's balance while preserving the optional trigger
    /// context used by the central economy ledger.  The historical
    /// [`Self::update_balance`] setter intentionally remains context-free;
    /// event/economy callers use this additive operation when they need an
    /// auditable source and relation.
    pub fn add_balance_with_context(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: Option<&BalanceLedgerContext>,
    ) -> Result<(), CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(context) = context {
            install_balance_ledger_context(&transaction, context, None)?;
        }
        let update = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 lowest_balance_ever = MIN(
                     COALESCE(lowest_balance_ever,
                              COALESCE(jopacoin_balance, 0) + ?1),
                     COALESCE(jopacoin_balance, 0) + ?1),
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, discord_id, guild_id],
        );
        if context.is_some() {
            clear_balance_ledger_context(&transaction)?;
        }
        update?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_personal_best_win_streak(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        streak: i64,
    ) -> Result<bool, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE players
             SET personal_best_win_streak = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND (personal_best_win_streak IS NULL OR personal_best_win_streak < ?1)",
            params![streak, discord_id, Self::normalize_guild_id(guild_id)],
        )? > 0)
    }

    pub fn update_personal_best_win_streaks(
        &self,
        streaks: &OrderedMap<i64, i64>,
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, i64>, CoreRepositoryError> {
        let mut improvements = OrderedMap::new();
        if streaks.is_empty() {
            return Ok(improvements);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let ids = streaks.keys().copied().collect::<Vec<_>>();
        let mut previous = HashMap::new();
        for chunk in ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT discord_id, COALESCE(personal_best_win_streak, 0)
                 FROM players WHERE guild_id = ? AND discord_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = transaction.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            for row in statement.query_map(params_from_iter(parameters), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (discord_id, best) = row?;
                previous.insert(discord_id, best);
            }
        }
        for (discord_id, streak) in streaks.iter() {
            if let Some(previous_best) = previous.get(discord_id)
                && streak > previous_best
            {
                transaction.execute(
                    "UPDATE players
                     SET personal_best_win_streak = ?1, updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3
                       AND COALESCE(personal_best_win_streak, 0) < ?1",
                    params![streak, discord_id, guild_id],
                )?;
                improvements.insert(*discord_id, *previous_best);
            }
        }
        transaction.commit()?;
        Ok(improvements)
    }

    pub fn get_personal_best_win_streak(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(personal_best_win_streak, 0)
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn delete(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.commit()?;
            return Ok(false);
        }
        for table in [
            "openskill_rating_events",
            "match_participants",
            "rating_history",
        ] {
            let sql = format!("DELETE FROM {table} WHERE discord_id = ?1 AND guild_id = ?2");
            transaction.execute(&sql, params![discord_id, guild_id])?;
        }
        transaction.execute(
            "DELETE FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
        increment_openskill_revision(&transaction, guild_id)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Delete every synthetic player (`discord_id < 0`) and its dependent
    /// records in one guild, matching the retained Python maintenance path.
    ///
    /// Foreign keys remain disabled in production, so the dependent deletes
    /// are explicit and share the same immediate transaction as the player
    /// rows and OpenSkill revision bump.
    pub fn delete_fake_users(&self, guild_id: Option<i64>) -> Result<usize, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM players WHERE discord_id < 0 AND guild_id = ?1",
            [guild_id],
            |row| row.get(0),
        )?;
        if count == 0 {
            transaction.commit()?;
            return Ok(0);
        }
        for table in [
            "match_participants",
            "rating_history",
            "bets",
            "openskill_rating_events",
        ] {
            let sql = format!("DELETE FROM {table} WHERE discord_id < 0 AND guild_id = ?1");
            transaction.execute(&sql, [guild_id])?;
        }
        transaction.execute(
            "DELETE FROM players WHERE discord_id < 0 AND guild_id = ?1",
            [guild_id],
        )?;
        increment_openskill_revision(&transaction, guild_id)?;
        transaction.commit()?;
        usize::try_from(count)
            .map_err(|_| CoreRepositoryError::InvalidInput("negative fake user count".to_owned()))
    }

    pub fn get_leaderboard(
        &self,
        guild_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        self.query_leaderboard(
            "COALESCE(jopacoin_balance, 0) DESC,
             COALESCE(wins, 0) DESC,
             COALESCE(glicko_rating, 0) DESC,
             discord_id ASC",
            guild_id,
            limit,
            offset,
            0,
        )
    }

    pub fn get_leaderboard_by_glicko(
        &self,
        guild_id: Option<i64>,
        limit: i64,
        offset: i64,
        min_games: i64,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        self.query_leaderboard(
            "CASE WHEN glicko_rating IS NULL THEN 1 ELSE 0 END,
             glicko_rating DESC, COALESCE(wins, 0) DESC, discord_id ASC",
            guild_id,
            limit,
            offset,
            min_games,
        )
    }

    pub fn get_leaderboard_by_openskill(
        &self,
        guild_id: Option<i64>,
        limit: i64,
        offset: i64,
        min_games: i64,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        self.query_leaderboard(
            "CASE WHEN os_mu IS NULL THEN 1 ELSE 0 END,
             os_mu DESC, COALESCE(wins, 0) DESC, discord_id ASC",
            guild_id,
            limit,
            offset,
            min_games,
        )
    }

    fn query_leaderboard(
        &self,
        ordering: &str,
        guild_id: Option<i64>,
        limit: i64,
        offset: i64,
        min_games: i64,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        let connection = self.connection()?;
        let games_clause = if min_games > 0 {
            " AND (COALESCE(wins, 0) + COALESCE(losses, 0)) >= ?4"
        } else {
            ""
        };
        let sql = format!(
            "{PLAYER_SELECT} WHERE guild_id = ?1{games_clause}
             ORDER BY {ordering} LIMIT ?2 OFFSET ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = if min_games > 0 {
            statement.query_map(
                params![Self::normalize_guild_id(guild_id), limit, offset, min_games],
                player_from_row,
            )?
        } else {
            statement.query_map(
                params![Self::normalize_guild_id(guild_id), limit, offset],
                player_from_row,
            )?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_player_above(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_balance: Option<i64>,
    ) -> Result<Option<Player>, CoreRepositoryError> {
        self.get_balance_neighbor(discord_id, guild_id, min_balance, true)
    }

    pub fn get_player_below(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Player>, CoreRepositoryError> {
        self.get_balance_neighbor(discord_id, guild_id, None, false)
    }

    fn get_balance_neighbor(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_balance: Option<i64>,
        above: bool,
    ) -> Result<Option<Player>, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let user = connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0), COALESCE(wins, 0),
                        COALESCE(glicko_rating, 0)
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((balance, wins, rating)) = user else {
            return Ok(None);
        };

        let (comparison, ordering) = if above {
            (
                "COALESCE(jopacoin_balance, 0) > ?3
                 OR (COALESCE(jopacoin_balance, 0) = ?3 AND (
                    COALESCE(wins, 0) > ?4
                    OR (COALESCE(wins, 0) = ?4 AND (
                       COALESCE(glicko_rating, 0) > ?5
                       OR (COALESCE(glicko_rating, 0) = ?5 AND discord_id < ?6)
                    ))
                 ))",
                "COALESCE(jopacoin_balance, 0) ASC, COALESCE(wins, 0) ASC,
                 COALESCE(glicko_rating, 0) ASC, discord_id DESC",
            )
        } else {
            (
                "COALESCE(jopacoin_balance, 0) < ?3
                 OR (COALESCE(jopacoin_balance, 0) = ?3 AND (
                    COALESCE(wins, 0) < ?4
                    OR (COALESCE(wins, 0) = ?4 AND (
                       COALESCE(glicko_rating, 0) < ?5
                       OR (COALESCE(glicko_rating, 0) = ?5 AND discord_id > ?6)
                    ))
                 ))",
                "COALESCE(jopacoin_balance, 0) DESC, COALESCE(wins, 0) DESC,
                 COALESCE(glicko_rating, 0) DESC, discord_id ASC",
            )
        };
        let eligibility = if above {
            " AND COALESCE(jopacoin_balance, 0) >= ?2"
        } else {
            ""
        };
        let sql = format!(
            "{PLAYER_SELECT} WHERE guild_id = ?1{eligibility} AND ({comparison})
             ORDER BY {ordering} LIMIT 1"
        );
        let minimum = min_balance.unwrap_or(balance);
        connection
            .query_row(
                &sql,
                params![guild_id, minimum, balance, wins, rating, discord_id],
                player_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn steal_atomic(
        &self,
        thief_discord_id: i64,
        victim_discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<StealResult, CoreRepositoryError> {
        self.steal_atomic_with_context(thief_discord_id, victim_discord_id, guild_id, amount, None)
    }

    /// Transfer JC atomically and annotate the victim debit and thief credit
    /// separately in the central economy ledger.
    pub fn steal_atomic_with_context(
        &self,
        thief_discord_id: i64,
        victim_discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        context: Option<&BalanceLedgerContext>,
    ) -> Result<StealResult, CoreRepositoryError> {
        if amount <= 0 {
            return Err(CoreRepositoryError::InvalidInput(
                "amount must be positive".to_owned(),
            ));
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let victim_balance = required_balance(&transaction, victim_discord_id, guild_id, "victim")?;
        let thief_balance = required_balance(&transaction, thief_discord_id, guild_id, "thief")?;
        let victim_new_balance = victim_balance.checked_sub(amount).ok_or_else(|| {
            CoreRepositoryError::InvalidInput("steal amount overflows victim balance".to_owned())
        })?;
        let thief_new_balance = thief_balance.checked_add(amount).ok_or_else(|| {
            CoreRepositoryError::InvalidInput("steal amount overflows thief balance".to_owned())
        })?;
        if let Some(context) = context {
            install_balance_ledger_context(
                &transaction,
                context,
                Some(&format_reason(context, "victim debit")),
            )?;
        }
        let victim_update = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![victim_new_balance, victim_discord_id, guild_id],
        );
        if context.is_some() {
            clear_balance_ledger_context(&transaction)?;
        }
        victim_update?;
        if let Some(context) = context {
            install_balance_ledger_context(
                &transaction,
                context,
                Some(&format_reason(context, "thief credit")),
            )?;
        }
        let thief_update = transaction.execute(
            "UPDATE players
             SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![thief_new_balance, thief_discord_id, guild_id],
        );
        if context.is_some() {
            clear_balance_ledger_context(&transaction)?;
        }
        thief_update?;
        transaction.execute(
            "UPDATE players SET lowest_balance_ever = ?1
             WHERE discord_id = ?2 AND guild_id = ?3
               AND (lowest_balance_ever IS NULL OR ?1 < lowest_balance_ever)",
            params![victim_new_balance, victim_discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(StealResult {
            amount,
            thief_new_balance,
            victim_new_balance,
        })
    }

    pub fn get_lowest_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT lowest_balance_ever FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Read the persisted regular-wheel cooldown anchor for `/mana`'s
    /// relative timestamp. A missing player and a never-spun player both
    /// intentionally produce `None`, matching Python's repository contract.
    pub fn get_last_wheel_spin(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT last_wheel_spin FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Atomically claim a regular wheel spin for one player.
    ///
    /// The check and timestamp write run under `BEGIN IMMEDIATE`, matching
    /// Python's `try_claim_wheel_spin` contract.  A concurrent request can
    /// therefore never observe the same cooldown window as available.
    pub fn try_claim_wheel_spin(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<WheelSpinClaim, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT last_wheel_spin FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        let Some(previous_spin) = previous else {
            transaction.commit()?;
            return Ok(WheelSpinClaim::MissingPlayer);
        };
        if let Some(last_spin) = previous_spin
            && now < last_spin.saturating_add(cooldown_seconds)
        {
            transaction.commit()?;
            return Ok(WheelSpinClaim::Cooldown { last_spin });
        }
        transaction.execute(
            "UPDATE players SET last_wheel_spin = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![now, discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(WheelSpinClaim::Claimed { previous_spin })
    }

    /// Move the durable cooldown anchor after a resolved Lose-Turn outcome.
    /// This is deliberately a narrow timestamp write; it does not re-admit a
    /// second spin or alter any other player state.
    pub fn set_last_wheel_spin(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        timestamp: i64,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE players SET last_wheel_spin = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![timestamp, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        if changed != 1 {
            return Err(CoreRepositoryError::InvalidInput(format!(
                "player {discord_id} was not found"
            )));
        }
        Ok(())
    }

    /// Read the one-shot Comeback pardon carried by the existing players row.
    pub fn get_wheel_pardon(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(has_wheel_pardon, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|value| value != 0))
    }

    /// Set or clear the one-shot Comeback pardon.
    pub fn set_wheel_pardon(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        active: bool,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE players SET has_wheel_pardon = ?1,
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![
                i64::from(active),
                discord_id,
                Self::normalize_guild_id(guild_id)
            ],
        )?;
        if changed != 1 {
            return Err(CoreRepositoryError::InvalidInput(format!(
                "player {discord_id} was not found"
            )));
        }
        Ok(())
    }

    /// Consume Comeback's pardon and report whether this call won the race.
    pub fn consume_wheel_pardon_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players SET has_wheel_pardon = 0,
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2
               AND COALESCE(has_wheel_pardon, 0) = 1",
            params![discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }
}

const PLAYER_SELECT: &str = "SELECT discord_username, current_mmr, initial_mmr, COALESCE(wins, 0),
            COALESCE(losses, 0), preferred_roles, main_role, glicko_rating,
            glicko_rd, glicko_volatility, os_mu, os_sigma, discord_id, guild_id,
            COALESCE(jopacoin_balance, 0), steam_id,
            COALESCE(personal_best_win_streak, 0), COALESCE(total_bets_placed, 0),
            COALESCE(first_leverage_used, 0), COALESCE(is_solo_grinder, 0),
            solo_grinder_checked_at, preferred_region, inferred_region,
            last_match_date, exclusion_count
     FROM players";

fn query_player_by_id(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<Player>, CoreRepositoryError> {
    let sql = format!("{PLAYER_SELECT} WHERE discord_id = ?1 AND guild_id = ?2");
    connection
        .query_row(&sql, params![discord_id, guild_id], player_from_row)
        .optional()
        .map_err(Into::into)
}

fn player_from_row(row: &rusqlite::Row<'_>) -> Result<Player, rusqlite::Error> {
    let roles: Option<String> = row.get(5)?;
    let mmr = python_optional_i64(row.get_ref(1)?);
    let initial_mmr = python_optional_i64(row.get_ref(2)?);
    Ok(Player {
        name: row.get(0)?,
        mmr,
        initial_mmr,
        wins: row.get(3)?,
        losses: row.get(4)?,
        preferred_roles: roles
            .as_deref()
            .and_then(|json| decode_string_array(json).ok()),
        main_role: row.get(6)?,
        glicko_rating: row.get(7)?,
        glicko_rd: row.get(8)?,
        glicko_volatility: row.get(9)?,
        os_mu: row.get(10)?,
        os_sigma: row.get(11)?,
        discord_id: row.get(12)?,
        guild_id: row.get(13)?,
        jopacoin_balance: row.get(14)?,
        steam_id: row.get(15)?,
        personal_best_win_streak: row.get(16)?,
        total_bets_placed: row.get(17)?,
        first_leverage_used: python_truthy(row.get_ref(18)?),
        is_solo_grinder: python_truthy(row.get_ref(19)?),
        solo_grinder_checked_at: row.get(20)?,
        preferred_region: row.get(21)?,
        inferred_region: row.get(22)?,
    })
}

fn format_reason(context: &BalanceLedgerContext, suffix: &str) -> String {
    context
        .reason
        .as_deref()
        .map_or_else(|| suffix.to_owned(), |reason| format!("{reason} {suffix}"))
}

fn install_balance_ledger_context(
    transaction: &Transaction<'_>,
    context: &BalanceLedgerContext,
    reason_override: Option<&str>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            context.source,
            context.actor_id,
            context.related_type,
            context.related_id,
            reason_override.or(context.reason.as_deref()),
            context.metadata,
        ],
    )?;
    Ok(())
}

fn clear_balance_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn required_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    role: &str,
) -> Result<i64, CoreRepositoryError> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| CoreRepositoryError::InvalidInput(format!("{role} not found")))
}

fn increment_openskill_revision(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO openskill_rating_revisions (guild_id, revision, updated_at)
         VALUES (?1, 1, CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             revision = revision + 1,
             updated_at = CURRENT_TIMESTAMP",
        [guild_id],
    )?;
    Ok(())
}

fn python_optional_i64(value: ValueRef<'_>) -> Option<i64> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(value) if value != 0 => Some(value),
        ValueRef::Real(value) if value != 0.0 => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value != 0.0)
            .map(|value| value as i64),
        _ => None,
    }
}

fn python_truthy(value: ValueRef<'_>) -> bool {
    match value {
        ValueRef::Null => false,
        ValueRef::Integer(value) => value != 0,
        ValueRef::Real(value) => value != 0.0,
        ValueRef::Text(value) | ValueRef::Blob(value) => !value.is_empty(),
    }
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn unique_i64(values: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(values.len());
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

fn encode_i64_array(values: &[i64]) -> String {
    let body = values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}

fn decode_i64_array(json: &str) -> Result<Vec<i64>, CoreRepositoryError> {
    let value = json.trim();
    let Some(body) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(CoreRepositoryError::InvalidJson(json.to_owned()));
    };
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    body.split(',')
        .map(|part| {
            part.trim()
                .parse::<i64>()
                .map_err(|_| CoreRepositoryError::InvalidJson(json.to_owned()))
        })
        .collect()
}

fn encode_string_array(values: &[String]) -> String {
    // Python writes this column with `json.dumps` and reads it with
    // `json.loads`, which accepts any standard JSON formatting; delegating
    // to serde_json keeps every escaping edge case (control characters,
    // unicode escapes, surrogate pairs) covered by one battle-tested
    // implementation instead of a hand-rolled escaper.
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_owned())
}

fn decode_string_array(json: &str) -> Result<Vec<String>, CoreRepositoryError> {
    serde_json::from_str::<Vec<String>>(json)
        .map_err(|_| CoreRepositoryError::InvalidJson(json.to_owned()))
}

fn parse_enrichment_json(raw: &str) -> Result<EnrichmentJsonValue, CoreRepositoryError> {
    let mut parser = EnrichmentJsonParser { raw, cursor: 0 };
    let value = parser
        .parse_value()
        .map_err(|()| CoreRepositoryError::InvalidJson(raw.to_owned()))?;
    parser.skip_whitespace();
    if parser.cursor != raw.len() {
        return Err(CoreRepositoryError::InvalidJson(raw.to_owned()));
    }
    Ok(value)
}

struct EnrichmentJsonParser<'a> {
    raw: &'a str,
    cursor: usize,
}

impl EnrichmentJsonParser<'_> {
    fn parse_value(&mut self) -> Result<EnrichmentJsonValue, ()> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => {
                self.consume_literal("null")?;
                Ok(EnrichmentJsonValue::Null)
            }
            Some(b't') => {
                self.consume_literal("true")?;
                Ok(EnrichmentJsonValue::Bool(true))
            }
            Some(b'f') => {
                self.consume_literal("false")?;
                Ok(EnrichmentJsonValue::Bool(false))
            }
            Some(b'N') => {
                self.consume_literal("NaN")?;
                Ok(EnrichmentJsonValue::Float(f64::NAN))
            }
            Some(b'I') => {
                self.consume_literal("Infinity")?;
                Ok(EnrichmentJsonValue::Float(f64::INFINITY))
            }
            Some(b'-') if self.remaining().starts_with("-Infinity") => {
                self.consume_literal("-Infinity")?;
                Ok(EnrichmentJsonValue::Float(f64::NEG_INFINITY))
            }
            Some(b'"') => self.parse_string().map(EnrichmentJsonValue::String),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(()),
        }
    }

    fn parse_array(&mut self) -> Result<EnrichmentJsonValue, ()> {
        self.consume_byte(b'[')?;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.cursor += 1;
            return Ok(EnrichmentJsonValue::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.cursor += 1,
                Some(b']') => {
                    self.cursor += 1;
                    break;
                }
                _ => return Err(()),
            }
        }
        Ok(EnrichmentJsonValue::Array(values))
    }

    fn parse_object(&mut self) -> Result<EnrichmentJsonValue, ()> {
        self.consume_byte(b'{')?;
        self.skip_whitespace();
        let mut values = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.cursor += 1;
            return Ok(EnrichmentJsonValue::Object(values));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.consume_byte(b':')?;
            values.insert(key, self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.cursor += 1,
                Some(b'}') => {
                    self.cursor += 1;
                    break;
                }
                _ => return Err(()),
            }
        }
        Ok(EnrichmentJsonValue::Object(values))
    }

    fn parse_number(&mut self) -> Result<EnrichmentJsonValue, ()> {
        let start = self.cursor;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E')
        ) {
            self.cursor += 1;
        }
        let number = &self.raw[start..self.cursor];
        if number.contains(['.', 'e', 'E']) {
            number
                .parse::<f64>()
                .map(EnrichmentJsonValue::Float)
                .map_err(|_| ())
        } else {
            number
                .parse::<i64>()
                .map(EnrichmentJsonValue::Integer)
                .map_err(|_| ())
        }
    }

    fn parse_string(&mut self) -> Result<String, ()> {
        self.consume_byte(b'"')?;
        let mut value = String::new();
        loop {
            let byte = self.peek().ok_or(())?;
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return Ok(value);
                }
                b'\\' => {
                    self.cursor += 1;
                    let escaped = self.peek().ok_or(())?;
                    self.cursor += 1;
                    match escaped {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'/' => value.push('/'),
                        b'b' => value.push('\u{08}'),
                        b'f' => value.push('\u{0c}'),
                        b'n' => value.push('\n'),
                        b'r' => value.push('\r'),
                        b't' => value.push('\t'),
                        b'u' => value.push(self.parse_unicode_escape()?),
                        _ => return Err(()),
                    }
                }
                0..=0x1f => return Err(()),
                0x20..=0x7f => {
                    value.push(char::from(byte));
                    self.cursor += 1;
                }
                _ => {
                    let character = self.remaining().chars().next().ok_or(())?;
                    value.push(character);
                    self.cursor += character.len_utf8();
                }
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ()> {
        let first = self.parse_hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            self.consume_byte(b'\\')?;
            self.consume_byte(b'u')?;
            let second = self.parse_hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(());
            }
            0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(());
        } else {
            first
        };
        char::from_u32(scalar).ok_or(())
    }

    fn parse_hex_quad(&mut self) -> Result<u32, ()> {
        let end = self.cursor.checked_add(4).ok_or(())?;
        let digits = self.raw.get(self.cursor..end).ok_or(())?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(());
        }
        self.cursor = end;
        u32::from_str_radix(digits, 16).map_err(|_| ())
    }

    fn consume_literal(&mut self, literal: &str) -> Result<(), ()> {
        if !self.remaining().starts_with(literal) {
            return Err(());
        }
        self.cursor += literal.len();
        Ok(())
    }

    fn consume_byte(&mut self, expected: u8) -> Result<(), ()> {
        if self.peek() != Some(expected) {
            return Err(());
        }
        self.cursor += 1;
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.raw.as_bytes().get(self.cursor).copied()
    }

    fn remaining(&self) -> &str {
        &self.raw[self.cursor..]
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchRecord {
    pub team1_ids: Vec<i64>,
    pub team2_ids: Vec<i64>,
    pub winning_team: i64,
    pub guild_id: Option<i64>,
    pub dotabuff_match_id: Option<String>,
    pub notes: Option<String>,
    pub lobby_type: String,
    pub lobby_kind: Option<String>,
    pub balancing_rating_system: String,
    pub betting_mode: String,
}

impl MatchRecord {
    #[must_use]
    pub fn new(
        team1_ids: Vec<i64>,
        team2_ids: Vec<i64>,
        winning_team: i64,
        guild_id: Option<i64>,
    ) -> Self {
        Self {
            team1_ids,
            team2_ids,
            winning_team,
            guild_id,
            dotabuff_match_id: None,
            notes: None,
            lobby_type: "shuffle".to_owned(),
            lobby_kind: None,
            balancing_rating_system: "glicko".to_owned(),
            betting_mode: "pool".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchSummary {
    pub match_id: i64,
    pub team1_players: Vec<i64>,
    pub team2_players: Vec<i64>,
    pub winning_team: Value,
    pub match_date: Value,
    pub dotabuff_match_id: Option<String>,
    pub notes: Option<String>,
    pub valve_match_id: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub radiant_score: Option<i64>,
    pub dire_score: Option<i64>,
    pub game_mode: Option<i64>,
    pub lobby_type: Option<String>,
    pub balancing_rating_system: Option<String>,
    pub win_reward_jc: Option<i64>,
    pub jc_changes: BTreeMap<i64, BTreeMap<String, i64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MostRecentMatch {
    pub match_id: i64,
    pub team1_players: Vec<i64>,
    pub team2_players: Vec<i64>,
    pub winning_team: Value,
    pub match_date: Value,
    pub dotabuff_match_id: Option<String>,
    pub valve_match_id: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnenrichedMatch {
    pub match_id: i64,
    pub team1_players: Vec<i64>,
    pub team2_players: Vec<i64>,
    pub winning_team: Value,
    pub match_date: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchWithoutFantasyData {
    pub match_id: i64,
    pub valve_match_id: i64,
    pub match_date: Value,
}

/// Dependency-free parsed representation of Python's persisted OpenDota JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum EnrichmentJsonValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchParticipant {
    pub match_id: i64,
    pub discord_id: i64,
    pub team_number: Value,
    pub won: Value,
    pub side: Value,
    pub hero_id: Value,
    pub kills: Value,
    pub deaths: Value,
    pub assists: Value,
    pub last_hits: Value,
    pub denies: Value,
    pub gpm: Value,
    pub xpm: Value,
    pub hero_damage: Value,
    pub tower_damage: Value,
    pub net_worth: Value,
    pub hero_healing: Value,
    pub lane_role: Value,
    pub lane_efficiency: Value,
    pub guild_id: i64,
    pub bonus_jc: Value,
    pub win_bonus_jc: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerMatchParticipant {
    pub discord_id: i64,
    pub side: Value,
    pub lane_role: Value,
    pub lane_efficiency: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerMatch {
    pub match_id: i64,
    pub team1_players: Vec<i64>,
    pub team2_players: Vec<i64>,
    pub winning_team: Value,
    pub match_date: Value,
    pub player_team: Value,
    pub player_won: bool,
    pub side: Value,
    pub valve_match_id: Value,
    pub lobby_type: Value,
    pub hero_id: Value,
    pub kills: Value,
    pub deaths: Value,
    pub assists: Value,
    pub gpm: Value,
    pub hero_damage: Value,
    pub net_worth: Value,
    pub tower_damage: Value,
    pub hero_healing: Value,
    pub lane_role: Value,
    pub lane_efficiency: Value,
    pub participants: Vec<PlayerMatchParticipant>,
}

/// Minimal per-hero aggregation consumed by the profile performance chart.
/// The Python query returns many additional averages for the text sections;
/// the chart itself only needs games and wins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroPerformanceStat {
    pub hero_id: i64,
    pub games: i64,
    pub wins: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticipantStatsUpdate {
    pub hero_id: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub gpm: i64,
    pub xpm: i64,
    pub hero_damage: i64,
    pub tower_damage: i64,
    pub last_hits: i64,
    pub denies: i64,
    pub net_worth: i64,
    pub hero_healing: i64,
    pub lane_role: Option<i64>,
    pub lane_efficiency: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewCoreRatingHistory {
    pub discord_id: i64,
    pub rating: Option<f64>,
    pub rating_before: Option<f64>,
    pub rd_before: Option<f64>,
    pub rd_after: Option<f64>,
    pub volatility_before: Option<f64>,
    pub volatility_after: Option<f64>,
    pub expected_team_win_prob: Option<f64>,
    pub team_number: Option<i64>,
    pub won: Option<bool>,
    pub match_id: Option<i64>,
    pub os_mu_before: Option<f64>,
    pub os_mu_after: Option<f64>,
    pub os_sigma_before: Option<f64>,
    pub os_sigma_after: Option<f64>,
    pub fantasy_weight: Option<f64>,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
    pub streak_multiplier_per_game: Option<f64>,
    pub streak_threshold: Option<i64>,
    pub base_rating_delta_multiplier: Option<f64>,
    pub low_priority_gain_multiplier: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchPredictionSnapshot {
    pub radiant_rating: Option<f64>,
    pub dire_rating: Option<f64>,
    pub radiant_rd: Option<f64>,
    pub dire_rd: Option<f64>,
    pub expected_radiant_win_prob: Option<f64>,
    pub openskill_radiant_win_prob: Option<f64>,
    pub openskill_raw_radiant_win_prob: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreakInsertDefaults {
    pub multiplier_per_game: f64,
    pub threshold: i64,
}

impl Default for StreakInsertDefaults {
    fn default() -> Self {
        Self {
            multiplier_per_game: DEFAULT_STREAK_MULTIPLIER_PER_GAME,
            threshold: DEFAULT_STREAK_THRESHOLD,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoreMatchRecord {
    pub match_record: MatchRecord,
    pub pending_match_id: Option<i64>,
    pub match_date: String,
    pub winning_ids: Vec<i64>,
    pub losing_ids: Vec<i64>,
    pub rating_history_rows: Vec<NewCoreRatingHistory>,
    pub match_prediction: MatchPredictionSnapshot,
    pub streak_defaults: StreakInsertDefaults,
    pub glicko_updates: Vec<CoreGlickoUpdate>,
    pub openskill_updates: Vec<CoreOpenSkillUpdate>,
    pub first_calibration_ids: Vec<i64>,
    pub first_calibration_unix: i64,
    pub exclusion_decay_ids: Vec<i64>,
    pub full_exclusion_increment_ids: Vec<i64>,
    pub half_exclusion_increment_ids: Vec<i64>,
    pub effective_avoid_ids: Vec<i64>,
    pub effective_deal_ids: Vec<i64>,
    pub expected_openskill_revision: Option<i64>,
    pub expected_low_priority_ids: Option<HashSet<i64>>,
    pub win_reward_jc: Option<i64>,
    /// When present, settle eligible first-match referrals in the match-core
    /// transaction at this Unix timestamp. `None` preserves raw/test seeding.
    pub settle_referrals_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreGlickoUpdate {
    pub discord_id: i64,
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreOpenSkillUpdate {
    pub discord_id: i64,
    pub mu: f64,
    pub sigma: f64,
}

impl CoreMatchRecord {
    #[must_use]
    pub fn new(match_record: MatchRecord, match_date: impl Into<String>) -> Self {
        Self {
            match_record,
            pending_match_id: None,
            match_date: match_date.into(),
            winning_ids: Vec::new(),
            losing_ids: Vec::new(),
            rating_history_rows: Vec::new(),
            match_prediction: MatchPredictionSnapshot::default(),
            streak_defaults: StreakInsertDefaults::default(),
            glicko_updates: Vec::new(),
            openskill_updates: Vec::new(),
            first_calibration_ids: Vec::new(),
            first_calibration_unix: 0,
            exclusion_decay_ids: Vec::new(),
            full_exclusion_increment_ids: Vec::new(),
            half_exclusion_increment_ids: Vec::new(),
            effective_avoid_ids: Vec::new(),
            effective_deal_ids: Vec::new(),
            expected_openskill_revision: None,
            expected_low_priority_ids: None,
            win_reward_jc: None,
            settle_referrals_at: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TeamOpenSkillRatings {
    pub team1: Vec<(f64, f64)>,
    pub team2: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LobbyTypeStats {
    pub lobby_type: String,
    pub avg_swing: Option<f64>,
    pub glicko_avg_swing: Option<f64>,
    pub openskill_avg_swing: Option<f64>,
    pub glicko_samples: i64,
    pub openskill_samples: i64,
    pub games: i64,
    pub actual_win_rate: Option<f64>,
    pub expected_win_rate: Option<f64>,
    pub openskill_expected_win_rate: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnemyHeroStat {
    pub enemy_hero: i64,
    pub games: i64,
    pub wins: i64,
    pub losses: i64,
    pub rate: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AllyHeroStat {
    pub ally_hero: i64,
    pub games: i64,
    pub wins: i64,
    pub win_rate: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroVersusHeroStat {
    pub my_hero: i64,
    pub opponent_hero: i64,
    pub games: i64,
    pub wins: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeroPairwiseStats {
    pub nemesis: Vec<EnemyHeroStat>,
    pub easy: Vec<EnemyHeroStat>,
    pub synergies: Vec<AllyHeroStat>,
    pub hero_vs_hero: Vec<HeroVersusHeroStat>,
}

#[derive(Clone, Debug)]
pub struct MatchRepository {
    path: PathBuf,
}

impl MatchRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        PlayerRepository::normalize_guild_id(guild_id)
    }

    fn connection(&self) -> Result<Connection, CoreRepositoryError> {
        Ok(open_runtime_connection(&self.path)?)
    }

    pub fn record_match(&self, record: &MatchRecord) -> Result<i64, CoreRepositoryError> {
        validate_winning_team(record.winning_team)?;
        let guild_id = Self::normalize_guild_id(record.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let match_id = insert_match(&transaction, record, None, None)?;
        insert_participants(&transaction, match_id, record, guild_id)?;
        transaction.commit()?;
        Ok(match_id)
    }

    /// Claim the post-core bonus phase exactly once for a committed match.
    pub fn claim_match_bonuses_paid(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE matches SET bonuses_paid = 1
             WHERE match_id = ?1 AND guild_id = ?2
               AND COALESCE(bonuses_paid, 0) = 0",
            params![match_id, Self::normalize_guild_id(guild_id)],
        )? == 1)
    }

    pub fn release_match_bonuses_claim(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE matches SET bonuses_paid = 0
             WHERE match_id = ?1 AND guild_id = ?2",
            params![match_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    /// Replace the durable per-player component breakdown after each
    /// idempotent post-core phase. Callers first load the existing map so
    /// referral components written by the core transaction are preserved.
    pub fn update_match_jc_changes(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        changes: &BTreeMap<i64, BTreeMap<String, i64>>,
    ) -> Result<(), CoreRepositoryError> {
        let encoded = serde_json::to_string(changes)
            .map_err(|error| CoreRepositoryError::InvalidJson(error.to_string()))?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE matches SET jc_changes=?1 WHERE match_id=?2 AND guild_id=?3",
            params![encoded, match_id, Self::normalize_guild_id(guild_id)],
        )?;
        if changed != 1 {
            return Err(CoreRepositoryError::InvalidInput(format!(
                "match {match_id} was not found in guild {}",
                Self::normalize_guild_id(guild_id)
            )));
        }
        Ok(())
    }

    pub fn get_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<MatchSummary>, CoreRepositoryError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT match_id, team1_players, team2_players, winning_team,
                        match_date, dotabuff_match_id, notes, valve_match_id,
                        duration_seconds, radiant_score, dire_score, game_mode,
                        lobby_type, balancing_rating_system, win_reward_jc,
                        jc_changes
                 FROM matches WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Value>(3)?,
                        row.get::<_, Value>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<i64>>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                },
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        Ok(Some(MatchSummary {
            match_id: raw.0,
            team1_players: decode_i64_array(&raw.1)?,
            team2_players: decode_i64_array(&raw.2)?,
            winning_team: raw.3,
            match_date: raw.4,
            dotabuff_match_id: raw.5,
            notes: raw.6,
            valve_match_id: raw.7,
            duration_seconds: raw.8,
            radiant_score: raw.9,
            dire_score: raw.10,
            game_mode: raw.11,
            lobby_type: raw.12,
            balancing_rating_system: raw.13,
            win_reward_jc: raw.14,
            jc_changes: decode_jc_changes(&connection, raw.15.as_deref())?,
        }))
    }

    /// Resolve the durable core row associated with one pending-match token.
    /// This is the restart-recovery discriminator between an unplayed shuffle
    /// (keep/remind) and a committed record whose post-core work must resume.
    pub fn match_id_for_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<i64>, CoreRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2",
                params![guild_id, pending_match_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_match_participants(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<MatchParticipant>, CoreRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "{PARTICIPANT_SELECT} WHERE match_id = ?1 AND guild_id = ?2"
        ))?;
        statement
            .query_map(
                params![match_id, Self::normalize_guild_id(guild_id)],
                participant_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_match_participants_bulk(
        &self,
        match_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, Vec<MatchParticipant>>, CoreRepositoryError> {
        let unique_ids = unique_i64(match_ids);
        let mut result = OrderedMap::new();
        for match_id in &unique_ids {
            result.insert(*match_id, Vec::new());
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "{PARTICIPANT_SELECT} WHERE guild_id = ? AND match_id IN ({})",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            for row in statement.query_map(params_from_iter(parameters), participant_from_row)? {
                let participant = row?;
                if let Some(rows) = result.get_mut(&participant.match_id) {
                    rows.push(participant);
                }
            }
        }
        Ok(result)
    }

    pub fn get_player_matches(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<PlayerMatch>, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "WITH recent AS (
                SELECT m.match_id, m.team1_players, m.team2_players,
                       m.winning_team, m.match_date, m.valve_match_id,
                       m.lobby_type, mp.team_number, mp.won, mp.side,
                       mp.hero_id, mp.kills, mp.deaths, mp.assists, mp.gpm,
                       mp.hero_damage, mp.net_worth, mp.tower_damage,
                       mp.hero_healing, mp.lane_role, mp.lane_efficiency
                FROM matches AS m
                JOIN match_participants AS mp ON m.match_id = mp.match_id
                WHERE mp.discord_id = ?1 AND mp.guild_id = ?2
                ORDER BY m.match_date DESC
                LIMIT ?3
             )
             SELECT recent.*, all_mp.discord_id, all_mp.side, all_mp.lane_role,
                    all_mp.lane_efficiency
             FROM recent
             LEFT JOIN match_participants AS all_mp
               ON all_mp.match_id = recent.match_id AND all_mp.guild_id = ?2
             ORDER BY recent.match_date DESC, all_mp.rowid",
        )?;
        let mut rows = statement.query(params![discord_id, guild_id, limit])?;
        let mut result: OrderedMap<i64, PlayerMatch> = OrderedMap::new();
        while let Some(row) = rows.next()? {
            let match_id = row.get::<_, i64>(0)?;
            if result.get(&match_id).is_none() {
                result.insert(
                    match_id,
                    PlayerMatch {
                        match_id,
                        team1_players: decode_i64_array(&row.get::<_, String>(1)?)?,
                        team2_players: decode_i64_array(&row.get::<_, String>(2)?)?,
                        winning_team: row.get(3)?,
                        match_date: row.get(4)?,
                        valve_match_id: row.get(5)?,
                        lobby_type: row.get(6)?,
                        player_team: row.get(7)?,
                        player_won: python_truthy(row.get_ref(8)?),
                        side: row.get(9)?,
                        hero_id: row.get(10)?,
                        kills: row.get(11)?,
                        deaths: row.get(12)?,
                        assists: row.get(13)?,
                        gpm: row.get(14)?,
                        hero_damage: row.get(15)?,
                        net_worth: row.get(16)?,
                        tower_damage: row.get(17)?,
                        hero_healing: row.get(18)?,
                        lane_role: row.get(19)?,
                        lane_efficiency: row.get(20)?,
                        participants: Vec::new(),
                    },
                );
            }
            if let Some(participant_id) = row.get::<_, Option<i64>>(21)?
                && let Some(player_match) = result.get_mut(&match_id)
            {
                player_match.participants.push(PlayerMatchParticipant {
                    discord_id: participant_id,
                    side: row.get(22)?,
                    lane_role: row.get(23)?,
                    lane_efficiency: row.get(24)?,
                });
            }
        }
        Ok(result.into_iter().map(|(_, value)| value).collect())
    }

    /// Return Python-compatible per-hero chart aggregates, ordered by games.
    pub fn get_player_hero_chart_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<HeroPerformanceStat>, CoreRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT hero_id,
                    COUNT(*) AS games,
                    SUM(CASE WHEN won THEN 1 ELSE 0 END) AS wins
             FROM match_participants
             WHERE discord_id = ?1
               AND guild_id = ?2
               AND hero_id IS NOT NULL
               AND hero_id > 0
             GROUP BY hero_id
             ORDER BY games DESC
             LIMIT ?3",
        )?;
        statement
            .query_map(params![discord_id, guild_id, limit], |row| {
                Ok(HeroPerformanceStat {
                    hero_id: row.get(0)?,
                    games: row.get(1)?,
                    wins: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn update_participant_stats(
        &self,
        match_id: i64,
        discord_id: i64,
        update: &ParticipantStatsUpdate,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE match_participants
             SET hero_id = ?1, kills = ?2, deaths = ?3, assists = ?4,
                 gpm = ?5, xpm = ?6, hero_damage = ?7, tower_damage = ?8,
                 last_hits = ?9, denies = ?10, net_worth = ?11,
                 hero_healing = ?12, lane_role = ?13, lane_efficiency = ?14
             WHERE match_id = ?15 AND discord_id = ?16",
            params![
                update.hero_id,
                update.kills,
                update.deaths,
                update.assists,
                update.gpm,
                update.xpm,
                update.hero_damage,
                update.tower_damage,
                update.last_hits,
                update.denies,
                update.net_worth,
                update.hero_healing,
                update.lane_role,
                update.lane_efficiency,
                match_id,
                discord_id,
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_match_enrichment(
        &self,
        match_id: i64,
        valve_match_id: i64,
        duration_seconds: i64,
        radiant_score: i64,
        dire_score: i64,
        game_mode: i64,
        enrichment_data: Option<&str>,
    ) -> Result<(), CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE matches SET valve_match_id = ?1, duration_seconds = ?2,
                 radiant_score = ?3, dire_score = ?4, game_mode = ?5,
                 enrichment_data = ?6 WHERE match_id = ?7",
            params![
                valve_match_id,
                duration_seconds,
                radiant_score,
                dire_score,
                game_mode,
                enrichment_data,
                match_id,
            ],
        )?;
        Ok(())
    }

    pub fn add_rating_history(
        &self,
        guild_id: Option<i64>,
        entry: &NewCoreRatingHistory,
    ) -> Result<i64, CoreRepositoryError> {
        let connection = self.connection()?;
        insert_sparse_rating_history(
            &connection,
            Self::normalize_guild_id(guild_id),
            entry,
            entry.match_id,
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn get_player_recent_outcomes(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<bool>, CoreRepositoryError> {
        let connection = self.connection()?;
        query_outcomes(
            &connection,
            "SELECT won FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
             ORDER BY match_id DESC, id DESC LIMIT ?3",
            params![discord_id, Self::normalize_guild_id(guild_id), limit],
        )
    }

    pub fn get_player_recent_outcomes_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<OrderedMap<i64, Vec<bool>>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        for discord_id in &unique_ids {
            result.insert(*discord_id, Vec::new());
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for discord_id in unique_ids {
            let outcomes = query_outcomes(
                &connection,
                "SELECT won FROM rating_history
                 WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
                 ORDER BY match_id DESC, id DESC LIMIT ?3",
                params![discord_id, guild_id, limit],
            )?;
            result.insert(discord_id, outcomes);
        }
        Ok(result)
    }

    pub fn get_player_outcomes_before_match_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        match_id: i64,
        limit: i64,
    ) -> Result<OrderedMap<i64, Vec<bool>>, CoreRepositoryError> {
        let unique_ids = unique_i64(discord_ids);
        let mut result = OrderedMap::new();
        for discord_id in &unique_ids {
            result.insert(*discord_id, Vec::new());
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let sql = format!(
            "WITH target_rows AS (
                SELECT discord_id, MIN(id) AS cutoff_id
                FROM rating_history
                WHERE match_id = ? AND guild_id = ?
                  AND discord_id IN ({})
                GROUP BY discord_id
             ), ranked_outcomes AS (
                SELECT history.discord_id, history.won,
                       ROW_NUMBER() OVER (
                           PARTITION BY history.discord_id
                           ORDER BY history.match_id DESC, history.id DESC
                       ) AS outcome_rank
                FROM rating_history AS history
                JOIN target_rows AS target
                  ON target.discord_id = history.discord_id
                WHERE history.guild_id = ? AND history.won IS NOT NULL
                  AND (history.match_id < ? OR (
                      history.match_id IS NULL AND history.id < target.cutoff_id
                  ))
             )
             SELECT discord_id, won FROM ranked_outcomes
             WHERE outcome_rank <= ? ORDER BY discord_id, outcome_rank",
            placeholders(unique_ids.len())
        );
        let mut parameters = Vec::with_capacity(unique_ids.len() + 5);
        parameters.push(Value::Integer(match_id));
        parameters.push(Value::Integer(guild_id));
        parameters.extend(unique_ids.iter().copied().map(Value::Integer));
        parameters.push(Value::Integer(guild_id));
        parameters.push(Value::Integer(match_id));
        parameters.push(Value::Integer(limit));
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(parameters.iter()))?;
        while let Some(row) = rows.next()? {
            let discord_id = row.get::<_, i64>(0)?;
            if let Some(outcomes) = result.get_mut(&discord_id) {
                outcomes.push(python_truthy(row.get_ref(1)?));
            }
        }
        Ok(result)
    }

    pub fn get_player_outcomes_before_match(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        match_id: i64,
        limit: i64,
    ) -> Result<Vec<bool>, CoreRepositoryError> {
        let connection = self.connection()?;
        query_outcomes(
            &connection,
            "SELECT won FROM rating_history
             WHERE discord_id = ?1 AND guild_id = ?2 AND won IS NOT NULL
               AND (
                 match_id < ?3
                 OR (match_id IS NULL AND id < (
                   SELECT MIN(id) FROM rating_history
                   WHERE match_id = ?3 AND discord_id = ?1 AND guild_id = ?2
                 ))
               )
               AND EXISTS (
                 SELECT 1 FROM rating_history
                 WHERE match_id = ?3 AND discord_id = ?1 AND guild_id = ?2
               )
             ORDER BY match_id DESC, id DESC LIMIT ?4",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                match_id,
                limit
            ],
        )
    }

    pub fn get_os_ratings_for_matches(
        &self,
        match_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<OrderedMap<i64, TeamOpenSkillRatings>, CoreRepositoryError> {
        let unique_ids = unique_i64(match_ids);
        let mut result = OrderedMap::new();
        for match_id in &unique_ids {
            result.insert(*match_id, TeamOpenSkillRatings::default());
        }
        if unique_ids.is_empty() {
            return Ok(result);
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        for chunk in unique_ids.chunks(BULK_CHUNK_SIZE) {
            let sql = format!(
                "SELECT match_id, team_number, os_mu_before, os_sigma_before
                 FROM rating_history
                 WHERE guild_id = ? AND match_id IN ({})
                   AND os_mu_before IS NOT NULL AND os_sigma_before IS NOT NULL",
                placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let mut rows = statement.query(params_from_iter(parameters))?;
            while let Some(row) = rows.next()? {
                let match_id = row.get::<_, i64>(0)?;
                if let Some(ratings) = result.get_mut(&match_id) {
                    let rating = (row.get::<_, f64>(2)?, row.get::<_, f64>(3)?);
                    match row.get::<_, i64>(1)? {
                        1 => ratings.team1.push(rating),
                        2 => ratings.team2.push(rating),
                        _ => {}
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn get_os_ratings_for_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<TeamOpenSkillRatings, CoreRepositoryError> {
        Ok(self
            .get_os_ratings_for_matches(&[match_id], guild_id)?
            .get(&match_id)
            .cloned()
            .unwrap_or_default())
    }

    pub fn get_most_recent_match(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<MostRecentMatch>, CoreRepositoryError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT match_id, team1_players, team2_players, winning_team,
                        match_date, dotabuff_match_id, valve_match_id, notes
                 FROM matches WHERE guild_id = ?1
                 ORDER BY match_date DESC, match_id DESC LIMIT 1",
                [Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Value>(3)?,
                        row.get::<_, Value>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|raw| {
            Ok(MostRecentMatch {
                match_id: raw.0,
                team1_players: decode_i64_array(&raw.1)?,
                team2_players: decode_i64_array(&raw.2)?,
                winning_team: raw.3,
                match_date: raw.4,
                dotabuff_match_id: raw.5,
                valve_match_id: raw.6,
                notes: raw.7,
            })
        })
        .transpose()
    }

    /// Return the participants from the newest match in one guild.
    ///
    /// This is the exact repository boundary used by the shuffle policy's
    /// recent-player penalty. An empty history is an empty set rather than an
    /// error, and older matches never leak into the result.
    pub fn get_last_match_participant_ids(
        &self,
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, CoreRepositoryError> {
        Ok(self
            .get_most_recent_match(guild_id)?
            .map(|recent| {
                recent
                    .team1_players
                    .into_iter()
                    .chain(recent.team2_players)
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn get_matches_without_enrichment(
        &self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<UnenrichedMatch>, CoreRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT match_id, team1_players, team2_players, winning_team, match_date
               FROM matches
              WHERE guild_id = ?1 AND valve_match_id IS NULL
              ORDER BY match_date DESC
              LIMIT ?2",
        )?;
        statement
            .query_map(
                params![Self::normalize_guild_id(guild_id), limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Value>(3)?,
                        row.get::<_, Value>(4)?,
                    ))
                },
            )?
            .map(|row| {
                let row = row?;
                Ok(UnenrichedMatch {
                    match_id: row.0,
                    team1_players: decode_i64_array(&row.1)?,
                    team2_players: decode_i64_array(&row.2)?,
                    winning_team: row.3,
                    match_date: row.4,
                })
            })
            .collect()
    }

    pub fn set_valve_match_id(
        &self,
        match_id: i64,
        valve_match_id: i64,
    ) -> Result<bool, CoreRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE matches SET valve_match_id = ?1 WHERE match_id = ?2",
            params![valve_match_id, match_id],
        )? != 0)
    }

    pub fn get_matches_without_fantasy_data(
        &self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MatchWithoutFantasyData>, CoreRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT m.match_id, m.valve_match_id, m.match_date
               FROM matches AS m
               JOIN match_participants AS mp ON m.match_id = mp.match_id
              WHERE m.guild_id = ?1
                AND m.valve_match_id IS NOT NULL
                AND mp.fantasy_points IS NULL
              ORDER BY m.match_date DESC
              LIMIT ?2",
        )?;
        statement
            .query_map(
                params![Self::normalize_guild_id(guild_id), limit as i64],
                |row| {
                    Ok(MatchWithoutFantasyData {
                        match_id: row.get(0)?,
                        valve_match_id: row.get(1)?,
                        match_date: row.get(2)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_enrichment_data(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<EnrichmentJsonValue>, CoreRepositoryError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT enrichment_data FROM matches
                  WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        raw.map(|raw| parse_enrichment_json(&raw)).transpose()
    }

    pub fn get_lobby_type_stats(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<LobbyTypeStats>, CoreRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut statement = connection.prepare(
            "WITH swing_stats AS (
                SELECT COALESCE(m.lobby_type, 'shuffle') AS lobby_type,
                       AVG(CASE WHEN rh.rating_before IS NOT NULL
                                 AND rh.rating IS NOT NULL
                           THEN ABS(rh.rating - rh.rating_before) END)
                           AS glicko_avg_swing,
                       SUM(CASE WHEN rh.rating_before IS NOT NULL
                                 AND rh.rating IS NOT NULL THEN 1 ELSE 0 END)
                           AS glicko_samples,
                       AVG(CASE WHEN rh.os_mu_before IS NOT NULL
                                 AND rh.os_mu_after IS NOT NULL
                           THEN ABS(rh.os_mu_after-rh.os_mu_before) * ?1 END)
                           AS openskill_avg_swing,
                       SUM(CASE WHEN rh.os_mu_before IS NOT NULL
                                 AND rh.os_mu_after IS NOT NULL THEN 1 ELSE 0 END)
                           AS openskill_samples
                FROM rating_history AS rh
                JOIN matches AS m
                  ON m.match_id = rh.match_id AND m.guild_id = rh.guild_id
                WHERE rh.guild_id = ?2
                GROUP BY COALESCE(m.lobby_type, 'shuffle')
             ), match_stats AS (
                SELECT COALESCE(m.lobby_type, 'shuffle') AS lobby_type,
                       COUNT(*) AS games,
                       AVG(CASE WHEN m.winning_team = 1 THEN 1.0 ELSE 0.0 END)
                           AS actual_win_rate,
                       AVG(mp.expected_radiant_win_prob) AS expected_win_rate,
                       AVG(mp.openskill_radiant_win_prob)
                           AS openskill_expected_win_rate
                FROM matches AS m
                LEFT JOIN match_predictions AS mp ON mp.match_id = m.match_id
                WHERE m.guild_id = ?2 AND m.winning_team IN (1, 2)
                GROUP BY COALESCE(m.lobby_type, 'shuffle')
             )
             SELECT ms.lobby_type, ss.glicko_avg_swing, ss.glicko_samples,
                    ss.openskill_avg_swing, ss.openskill_samples, ms.games,
                    ms.actual_win_rate, ms.expected_win_rate,
                    ms.openskill_expected_win_rate
             FROM match_stats AS ms
             LEFT JOIN swing_stats AS ss ON ss.lobby_type = ms.lobby_type
             ORDER BY ms.lobby_type",
        )?;
        statement
            .query_map(
                params![OPENSKILL_DISPLAY_SCALE, guild_id],
                lobby_stats_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_player_lobby_type_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<LobbyTypeStats>, CoreRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT COALESCE(m.lobby_type, 'shuffle'),
                    AVG(CASE WHEN rh.rating_before IS NOT NULL
                              AND rh.rating IS NOT NULL
                        THEN ABS(rh.rating-rh.rating_before) END),
                    SUM(CASE WHEN rh.rating_before IS NOT NULL
                              AND rh.rating IS NOT NULL THEN 1 ELSE 0 END),
                    AVG(CASE WHEN rh.os_mu_before IS NOT NULL
                              AND rh.os_mu_after IS NOT NULL
                        THEN ABS(rh.os_mu_after-rh.os_mu_before) * ?1 END),
                    SUM(CASE WHEN rh.os_mu_before IS NOT NULL
                              AND rh.os_mu_after IS NOT NULL THEN 1 ELSE 0 END),
                    COUNT(DISTINCT rh.match_id),
                    AVG(CASE WHEN rh.won = 1 THEN 1.0 ELSE 0.0 END),
                    AVG(rh.expected_team_win_prob),
                    AVG(CASE WHEN rh.team_number = 1
                               THEN mp.openskill_radiant_win_prob
                             WHEN rh.team_number = 2
                               THEN 1.0-mp.openskill_radiant_win_prob END)
             FROM rating_history AS rh
             JOIN matches AS m
               ON rh.match_id = m.match_id AND rh.guild_id = m.guild_id
             LEFT JOIN match_predictions AS mp ON mp.match_id = rh.match_id
             WHERE rh.discord_id = ?2 AND rh.guild_id = ?3
               AND ((rh.rating_before IS NOT NULL AND rh.rating IS NOT NULL)
                 OR (rh.os_mu_before IS NOT NULL AND rh.os_mu_after IS NOT NULL))
             GROUP BY m.lobby_type",
        )?;
        statement
            .query_map(
                params![
                    OPENSKILL_DISPLAY_SCALE,
                    discord_id,
                    Self::normalize_guild_id(guild_id)
                ],
                lobby_stats_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_player_hero_pairwise_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        min_games: i64,
    ) -> Result<HeroPairwiseStats, CoreRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT subject.team_number = other.team_number AS is_ally,
                    subject.hero_id, other.hero_id, COUNT(*),
                    SUM(CASE WHEN subject.won = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN subject.won = 0 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN subject.won THEN 1 ELSE 0 END)
             FROM match_participants AS subject
             JOIN match_participants AS other
               ON other.match_id = subject.match_id
              AND other.discord_id != subject.discord_id
              AND other.guild_id = subject.guild_id
             WHERE subject.discord_id = ?1 AND subject.guild_id = ?2
               AND subject.team_number IS NOT NULL AND other.team_number IS NOT NULL
               AND other.hero_id IS NOT NULL AND other.hero_id > 0
             GROUP BY is_ally, subject.hero_id, other.hero_id",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? != 0,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )?;
        let mut enemy_totals: BTreeMap<i64, (i64, i64, i64)> = BTreeMap::new();
        let mut ally_totals: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
        let mut hero_vs_hero = Vec::new();
        for row in rows {
            let (is_ally, my_hero, other_hero, games, wins, losses, truthy_wins) = row?;
            if is_ally {
                let totals = ally_totals.entry(other_hero).or_default();
                totals.0 += games;
                totals.1 += wins;
            } else {
                let totals = enemy_totals.entry(other_hero).or_default();
                totals.0 += games;
                totals.1 += wins;
                totals.2 += losses;
                if let Some(my_hero) = my_hero.filter(|hero| *hero > 0)
                    && games >= min_games
                {
                    hero_vs_hero.push(HeroVersusHeroStat {
                        my_hero,
                        opponent_hero: other_hero,
                        games,
                        wins: truthy_wins,
                    });
                }
            }
        }
        let enemies = enemy_totals
            .into_iter()
            .filter(|(_, (games, _, _))| *games >= min_games)
            .map(|(enemy_hero, (games, wins, losses))| EnemyHeroStat {
                enemy_hero,
                games,
                wins,
                losses,
                rate: 0.0,
            })
            .collect::<Vec<_>>();
        let mut nemesis = enemies.clone();
        for stat in &mut nemesis {
            stat.rate = stat.losses as f64 / stat.games as f64;
        }
        nemesis.sort_by(|left, right| {
            right
                .rate
                .total_cmp(&left.rate)
                .then_with(|| right.games.cmp(&left.games))
                .then_with(|| left.enemy_hero.cmp(&right.enemy_hero))
        });
        nemesis.truncate(10);
        let mut easy = enemies;
        for stat in &mut easy {
            stat.rate = stat.wins as f64 / stat.games as f64;
        }
        easy.sort_by(|left, right| {
            right
                .rate
                .total_cmp(&left.rate)
                .then_with(|| right.games.cmp(&left.games))
                .then_with(|| left.enemy_hero.cmp(&right.enemy_hero))
        });
        easy.truncate(10);
        let mut synergies = ally_totals
            .into_iter()
            .filter(|(_, (games, _))| *games >= min_games)
            .map(|(ally_hero, (games, wins))| AllyHeroStat {
                ally_hero,
                games,
                wins,
                win_rate: wins as f64 / games as f64,
            })
            .collect::<Vec<_>>();
        synergies.sort_by(|left, right| {
            right
                .win_rate
                .total_cmp(&left.win_rate)
                .then_with(|| right.games.cmp(&left.games))
                .then_with(|| left.ally_hero.cmp(&right.ally_hero))
        });
        synergies.truncate(10);
        hero_vs_hero.sort_by(|left, right| {
            right
                .games
                .cmp(&left.games)
                .then_with(|| left.my_hero.cmp(&right.my_hero))
                .then_with(|| left.opponent_hero.cmp(&right.opponent_hero))
        });
        hero_vs_hero.truncate(30);
        Ok(HeroPairwiseStats {
            nemesis,
            easy,
            synergies,
            hero_vs_hero,
        })
    }

    pub fn save_pending_match(
        &self,
        guild_id: Option<i64>,
        payload_json: &str,
    ) -> Result<i64, CoreRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO pending_matches (guild_id, payload, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)",
            params![Self::normalize_guild_id(guild_id), payload_json],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Atomically persist the core match slice exercised by repository parity.
    ///
    /// This includes durable match/participant rows, idempotency by pending
    /// match, match-count suspension progression, prediction data, and rating
    /// history with live streak defaults.  Wider service orchestration remains
    /// in Python during the additive phase.
    pub fn record_match_core_atomic(
        &self,
        core: &CoreMatchRecord,
    ) -> Result<i64, CoreRepositoryError> {
        validate_winning_team(core.match_record.winning_team)?;
        let guild_id = Self::normalize_guild_id(core.match_record.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(pending_match_id) = core.pending_match_id
            && let Some(existing) = transaction
                .query_row(
                    "SELECT match_id FROM matches
                     WHERE guild_id = ?1 AND pending_match_id = ?2",
                    params![guild_id, pending_match_id],
                    |row| row.get(0),
                )
                .optional()?
        {
            transaction.commit()?;
            return Ok(existing);
        }

        if let Some(expected_revision) = core.expected_openskill_revision {
            let current_revision = transaction
                .query_row(
                    "SELECT revision FROM openskill_rating_revisions WHERE guild_id = ?1",
                    [guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or_default();
            if current_revision != expected_revision {
                return Err(CoreRepositoryError::InvalidInput(
                    "OpenSkill ratings changed while this match was being calculated; retry recording the match"
                        .to_owned(),
                ));
            }
        }
        if let Some(expected_ids) = core.expected_low_priority_ids.as_ref() {
            let participant_ids = unique_i64(
                &core
                    .match_record
                    .team1_ids
                    .iter()
                    .chain(&core.match_record.team2_ids)
                    .copied()
                    .collect::<Vec<_>>(),
            );
            let mut current_ids = HashSet::new();
            for chunk in participant_ids.chunks(BULK_CHUNK_SIZE) {
                let sql = format!(
                    "SELECT discord_id FROM low_priority_state
                     WHERE guild_id = ? AND active = 1 AND wins_remaining > 0
                       AND discord_id IN ({})",
                    placeholders(chunk.len())
                );
                let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
                let mut statement = transaction.prepare(&sql)?;
                for row in statement.query_map(params_from_iter(parameters), |row| row.get(0))? {
                    current_ids.insert(row?);
                }
            }
            if &current_ids != expected_ids {
                return Err(CoreRepositoryError::InvalidInput(
                    "Low-priority state changed while this match was being calculated; retry recording the match"
                        .to_owned(),
                ));
            }
        }

        let match_id = insert_match(
            &transaction,
            &core.match_record,
            core.pending_match_id,
            Some(&core.match_date),
        )?;
        if let Some(win_reward_jc) = core.win_reward_jc {
            transaction.execute(
                "UPDATE matches SET win_reward_jc = ?1
                 WHERE match_id = ?2 AND guild_id = ?3",
                params![win_reward_jc, match_id, guild_id],
            )?;
        }
        insert_participants(&transaction, match_id, &core.match_record, guild_id)?;
        update_match_pairings(
            &transaction,
            guild_id,
            match_id,
            &core.match_record.team1_ids,
            &core.match_record.team2_ids,
            core.match_record.winning_team,
        )?;
        if let Some(rewarded_at) = core.settle_referrals_at {
            let rewards = settle_first_match_referrals(
                &transaction,
                match_id,
                guild_id,
                &core.match_record.team1_ids,
                &core.match_record.team2_ids,
                rewarded_at,
            )
            .map_err(|error| CoreRepositoryError::InvalidInput(error.to_string()))?;
            let referral_changes = aggregate_referral_changes(&rewards);
            if !referral_changes.is_empty() {
                transaction.execute(
                    "UPDATE matches SET jc_changes=?1
                     WHERE match_id=?2 AND guild_id=?3",
                    params![
                        encode_referral_jc_changes(&referral_changes),
                        match_id,
                        guild_id
                    ],
                )?;
            }
        }
        for discord_id in &core.winning_ids {
            transaction.execute(
                "UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
            )?;
        }
        for discord_id in &core.losing_ids {
            transaction.execute(
                "UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
            )?;
        }
        for update in &core.glicko_updates {
            transaction.execute(
                "UPDATE players SET glicko_rating = ?1, glicko_rd = ?2,
                        glicko_volatility = ?3, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?4 AND guild_id = ?5",
                params![
                    update.rating,
                    update.rd,
                    update.volatility,
                    update.discord_id,
                    guild_id
                ],
            )?;
        }
        for update in &core.openskill_updates {
            transaction.execute(
                "UPDATE players SET os_mu = ?1, os_sigma = ?2,
                        os_rating_version = ?3, os_algorithm_fingerprint = ?4,
                        updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?5 AND guild_id = ?6",
                params![
                    update.mu,
                    update.sigma,
                    OPENSKILL_ALGORITHM_VERSION,
                    OPENSKILL_ALGORITHM_FINGERPRINT,
                    update.discord_id,
                    guild_id
                ],
            )?;
        }
        for discord_id in core
            .match_record
            .team1_ids
            .iter()
            .chain(&core.match_record.team2_ids)
        {
            transaction.execute(
                "UPDATE players SET last_match_date = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![core.match_date, discord_id, guild_id],
            )?;
        }
        update_exclusion_counts(
            &transaction,
            guild_id,
            &core.exclusion_decay_ids,
            &core.full_exclusion_increment_ids,
            &core.half_exclusion_increment_ids,
        )?;
        for discord_id in &core.first_calibration_ids {
            transaction.execute(
                "UPDATE players SET first_calibrated_at = ?1,
                        updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3
                   AND first_calibrated_at IS NULL",
                params![core.first_calibration_unix, discord_id, guild_id],
            )?;
        }
        progress_suspensions(
            &transaction,
            guild_id,
            core.pending_match_id,
            core.match_record.lobby_kind.as_deref(),
            match_id,
        )?;
        progress_low_priority_wins(
            &transaction,
            guild_id,
            core.pending_match_id,
            match_id,
            &core.winning_ids,
        )?;
        upsert_match_prediction(&transaction, match_id, &core.match_prediction)?;
        for history in &core.rating_history_rows {
            insert_rating_history(
                &transaction,
                guild_id,
                history,
                Some(match_id),
                core.streak_defaults,
            )?;
        }
        decrement_match_consumables(
            &transaction,
            guild_id,
            &core.effective_avoid_ids,
            &core.effective_deal_ids,
        )?;
        if !core.openskill_updates.is_empty() {
            increment_openskill_revision(&transaction, guild_id)?;
        }
        transaction.commit()?;
        Ok(match_id)
    }
}

const PARTICIPANT_SELECT: &str =
    "SELECT match_id, discord_id, team_number, won, side, hero_id, kills,
            deaths, assists, last_hits, denies, gpm, xpm, hero_damage,
            tower_damage, net_worth, hero_healing, lane_role, lane_efficiency,
            guild_id, bonus_jc, win_bonus_jc
     FROM match_participants";

fn participant_from_row(row: &rusqlite::Row<'_>) -> Result<MatchParticipant, rusqlite::Error> {
    Ok(MatchParticipant {
        match_id: row.get(0)?,
        discord_id: row.get(1)?,
        team_number: row.get(2)?,
        won: row.get(3)?,
        side: row.get(4)?,
        hero_id: row.get(5)?,
        kills: row.get(6)?,
        deaths: row.get(7)?,
        assists: row.get(8)?,
        last_hits: row.get(9)?,
        denies: row.get(10)?,
        gpm: row.get(11)?,
        xpm: row.get(12)?,
        hero_damage: row.get(13)?,
        tower_damage: row.get(14)?,
        net_worth: row.get(15)?,
        hero_healing: row.get(16)?,
        lane_role: row.get(17)?,
        lane_efficiency: row.get(18)?,
        guild_id: row.get(19)?,
        bonus_jc: row.get(20)?,
        win_bonus_jc: row.get(21)?,
    })
}

fn validate_winning_team(winning_team: i64) -> Result<(), CoreRepositoryError> {
    if matches!(winning_team, 1 | 2) {
        Ok(())
    } else {
        Err(CoreRepositoryError::InvalidInput(
            "winning_team must be 1 (radiant) or 2 (dire)".to_owned(),
        ))
    }
}

fn insert_match(
    transaction: &Transaction<'_>,
    record: &MatchRecord,
    pending_match_id: Option<i64>,
    match_date: Option<&str>,
) -> Result<i64, rusqlite::Error> {
    transaction.execute(
        "INSERT INTO matches (
            guild_id, team1_players, team2_players, winning_team,
            dotabuff_match_id, notes, lobby_type, lobby_kind,
            balancing_rating_system, betting_mode, pending_match_id, match_date
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                   COALESCE(?12, CURRENT_TIMESTAMP))",
        params![
            MatchRepository::normalize_guild_id(record.guild_id),
            encode_i64_array(&record.team1_ids),
            encode_i64_array(&record.team2_ids),
            record.winning_team,
            record.dotabuff_match_id,
            record.notes,
            record.lobby_type,
            record.lobby_kind,
            record.balancing_rating_system,
            record.betting_mode,
            pending_match_id,
            match_date,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn insert_participants(
    transaction: &Transaction<'_>,
    match_id: i64,
    record: &MatchRecord,
    guild_id: i64,
) -> Result<(), rusqlite::Error> {
    let team1_won = record.winning_team == 1;
    for discord_id in &record.team1_ids {
        transaction.execute(
            "INSERT INTO match_participants
             (match_id, discord_id, guild_id, team_number, won, side)
             VALUES (?1, ?2, ?3, 1, ?4, 'radiant')",
            params![match_id, discord_id, guild_id, team1_won],
        )?;
    }
    for discord_id in &record.team2_ids {
        transaction.execute(
            "INSERT INTO match_participants
             (match_id, discord_id, guild_id, team_number, won, side)
             VALUES (?1, ?2, ?3, 2, ?4, 'dire')",
            params![match_id, discord_id, guild_id, !team1_won],
        )?;
    }
    Ok(())
}

fn update_match_pairings(
    transaction: &Transaction<'_>,
    guild_id: i64,
    match_id: i64,
    radiant_ids: &[i64],
    dire_ids: &[i64],
    winning_team: i64,
) -> Result<(), rusqlite::Error> {
    for (team_number, ids) in [(1_i64, radiant_ids), (2_i64, dire_ids)] {
        let won = i64::from(team_number == winning_team);
        for (index, &first) in ids.iter().enumerate() {
            for &second in &ids[index + 1..] {
                let (player1, player2) = if first < second {
                    (first, second)
                } else {
                    (second, first)
                };
                transaction.execute(
                    "INSERT INTO player_pairings (
                         guild_id,player1_id,player2_id,games_together,
                         wins_together,last_match_id
                     ) VALUES (?1,?2,?3,1,?4,?5)
                     ON CONFLICT(guild_id,player1_id,player2_id) DO UPDATE SET
                         games_together=games_together+1,
                         wins_together=wins_together+excluded.wins_together,
                         last_match_id=excluded.last_match_id,
                         updated_at=CURRENT_TIMESTAMP",
                    params![guild_id, player1, player2, won, match_id],
                )?;
            }
        }
    }
    for &radiant in radiant_ids {
        for &dire in dire_ids {
            let (player1, player2) = if radiant < dire {
                (radiant, dire)
            } else {
                (dire, radiant)
            };
            let player1_won = if player1 == radiant {
                winning_team == 1
            } else {
                winning_team == 2
            };
            transaction.execute(
                "INSERT INTO player_pairings (
                     guild_id,player1_id,player2_id,games_against,
                     player1_wins_against,last_match_id
                 ) VALUES (?1,?2,?3,1,?4,?5)
                 ON CONFLICT(guild_id,player1_id,player2_id) DO UPDATE SET
                     games_against=games_against+1,
                     player1_wins_against=player1_wins_against
                         + excluded.player1_wins_against,
                     last_match_id=excluded.last_match_id,
                     updated_at=CURRENT_TIMESTAMP",
                params![guild_id, player1, player2, i64::from(player1_won), match_id],
            )?;
        }
    }
    Ok(())
}

fn update_exclusion_counts(
    transaction: &Transaction<'_>,
    guild_id: i64,
    decay_ids: &[i64],
    full_increment_ids: &[i64],
    half_increment_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    for discord_id in decay_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = MAX(COALESCE(exclusion_count, 0) - 1, 0),
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    for discord_id in full_increment_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = COALESCE(exclusion_count, 0) + 6,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    for discord_id in half_increment_ids {
        transaction.execute(
            "UPDATE players
             SET exclusion_count = COALESCE(exclusion_count, 0) + 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
        )?;
    }
    Ok(())
}

fn decrement_match_consumables(
    transaction: &Transaction<'_>,
    guild_id: i64,
    avoid_ids: &[i64],
    deal_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    let now = unix_now();
    for avoid_id in avoid_ids {
        transaction.execute(
            "UPDATE soft_avoids
             SET games_remaining = games_remaining - 1, updated_at = ?1
             WHERE guild_id = ?2 AND id = ?3 AND games_remaining > 0",
            params![now, guild_id, avoid_id],
        )?;
    }
    for deal_id in deal_ids {
        transaction.execute(
            "UPDATE package_deals
             SET games_remaining = games_remaining - 1, updated_at = ?1
             WHERE guild_id = ?2 AND id = ?3 AND games_remaining > 0",
            params![now, guild_id, deal_id],
        )?;
    }
    Ok(())
}

fn insert_rating_history(
    connection: &Connection,
    guild_id: i64,
    entry: &NewCoreRatingHistory,
    match_id: Option<i64>,
    defaults: StreakInsertDefaults,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO rating_history (
            discord_id, guild_id, rating, rating_before, rd_before, rd_after,
            volatility_before, volatility_after, expected_team_win_prob,
            team_number, won, match_id, os_mu_before, os_mu_after,
            os_sigma_before, os_sigma_after, fantasy_weight,
            os_algorithm_version, os_algorithm_fingerprint, streak_length,
            streak_multiplier, streak_multiplier_per_game, streak_threshold,
            base_rating_delta_multiplier, low_priority_gain_multiplier
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
         )",
        params![
            entry.discord_id,
            guild_id,
            entry.rating,
            entry.rating_before,
            entry.rd_before,
            entry.rd_after,
            entry.volatility_before,
            entry.volatility_after,
            entry.expected_team_win_prob,
            entry.team_number,
            entry.won,
            match_id,
            entry.os_mu_before,
            entry.os_mu_after,
            entry.os_sigma_before,
            entry.os_sigma_after,
            entry.fantasy_weight,
            OPENSKILL_ALGORITHM_VERSION,
            OPENSKILL_ALGORITHM_FINGERPRINT,
            entry.streak_length,
            entry.streak_multiplier,
            entry
                .streak_multiplier_per_game
                .unwrap_or(defaults.multiplier_per_game),
            entry.streak_threshold.unwrap_or(defaults.threshold),
            entry.base_rating_delta_multiplier.unwrap_or(0.75),
            entry.low_priority_gain_multiplier.unwrap_or(1.0),
        ],
    )?;
    Ok(())
}

/// Python's standalone `add_rating_history` predates the replay-parameter
/// columns and intentionally lets their schema defaults apply.  Core match
/// recording uses `insert_rating_history` above because it must stamp the live
/// streak configuration instead.
fn insert_sparse_rating_history(
    connection: &Connection,
    guild_id: i64,
    entry: &NewCoreRatingHistory,
    match_id: Option<i64>,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO rating_history (
            discord_id, guild_id, rating, rating_before, rd_before, rd_after,
            volatility_before, volatility_after, expected_team_win_prob,
            team_number, won, match_id, os_mu_before, os_mu_after,
            os_sigma_before, os_sigma_after, fantasy_weight,
            os_algorithm_version, os_algorithm_fingerprint, streak_length,
            streak_multiplier
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
         )",
        params![
            entry.discord_id,
            guild_id,
            entry.rating,
            entry.rating_before,
            entry.rd_before,
            entry.rd_after,
            entry.volatility_before,
            entry.volatility_after,
            entry.expected_team_win_prob,
            entry.team_number,
            entry.won,
            match_id,
            entry.os_mu_before,
            entry.os_mu_after,
            entry.os_sigma_before,
            entry.os_sigma_after,
            entry.fantasy_weight,
            OPENSKILL_ALGORITHM_VERSION,
            OPENSKILL_ALGORITHM_FINGERPRINT,
            entry.streak_length,
            entry.streak_multiplier,
        ],
    )?;
    Ok(())
}

fn query_outcomes<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<bool>, CoreRepositoryError> {
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(parameters, |row| Ok(python_truthy(row.get_ref(0)?)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn decode_jc_changes(
    connection: &Connection,
    json: Option<&str>,
) -> Result<BTreeMap<i64, BTreeMap<String, i64>>, CoreRepositoryError> {
    let Some(json) = json.filter(|json| !json.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let mut statement = connection.prepare(
        "SELECT CAST(player.key AS INTEGER), component.key,
                CAST(component.value AS INTEGER)
         FROM json_each(?1) AS player, json_each(player.value) AS component",
    )?;
    let mut result: BTreeMap<i64, BTreeMap<String, i64>> = BTreeMap::new();
    for row in statement.query_map([json], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })? {
        let (discord_id, component, amount) = row?;
        result
            .entry(discord_id)
            .or_default()
            .insert(component, amount);
    }
    Ok(result)
}

fn lobby_stats_from_row(row: &rusqlite::Row<'_>) -> Result<LobbyTypeStats, rusqlite::Error> {
    let glicko_avg_swing = row.get(1)?;
    Ok(LobbyTypeStats {
        lobby_type: row
            .get::<_, Option<String>>(0)?
            .unwrap_or_else(|| "shuffle".to_owned()),
        avg_swing: glicko_avg_swing,
        glicko_avg_swing,
        glicko_samples: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
        openskill_avg_swing: row.get(3)?,
        openskill_samples: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        games: row.get(5)?,
        actual_win_rate: row.get(6)?,
        expected_win_rate: row.get(7)?,
        openskill_expected_win_rate: row.get(8)?,
    })
}

#[derive(Debug)]
struct ProgressingSuspension {
    discord_id: i64,
    scope: String,
    completion: String,
    expires_at: Option<i64>,
    matches_required: Option<i64>,
    matches_remaining: i64,
    pending_match_watermark: i64,
}

fn progress_suspensions(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    lobby_kind: Option<&str>,
    match_id: i64,
) -> Result<(), rusqlite::Error> {
    let (Some(pending_match_id), Some(lobby_kind)) = (pending_match_id, lobby_kind) else {
        return Ok(());
    };
    let recorded_at = unix_now();
    let mut statement = transaction.prepare(
        "SELECT discord_id, scope, completion, expires_at, matches_required,
                matches_remaining, pending_match_watermark
         FROM lobby_suspensions
         WHERE guild_id = ?1 AND active = 1 AND matches_remaining > 0
           AND ?2 > pending_match_watermark
           AND (scope = 'all' OR scope = ?3)
           AND (completion != 'either' OR expires_at IS NULL OR expires_at > ?4)",
    )?;
    let progressing = statement
        .query_map(
            params![guild_id, pending_match_id, lobby_kind, recorded_at],
            |row| {
                Ok(ProgressingSuspension {
                    discord_id: row.get(0)?,
                    scope: row.get(1)?,
                    completion: row.get(2)?,
                    expires_at: row.get(3)?,
                    matches_required: row.get(4)?,
                    matches_remaining: row.get(5)?,
                    pending_match_watermark: row.get(6)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    transaction.execute(
        "UPDATE lobby_suspensions
         SET matches_remaining = MAX(0, matches_remaining - 1),
             active = CASE
               WHEN matches_remaining <= 1 AND completion IN ('matches','either') THEN 0
               WHEN matches_remaining <= 1 AND completion = 'both'
                    AND expires_at <= ?1 THEN 0
               ELSE active END,
             revision = revision + 1, updated_at = ?1
         WHERE guild_id = ?2 AND active = 1 AND matches_remaining > 0
           AND ?3 > pending_match_watermark
           AND (scope = 'all' OR scope = ?4)
           AND (completion != 'either' OR expires_at IS NULL OR expires_at > ?1)",
        params![recorded_at, guild_id, pending_match_id, lobby_kind],
    )?;

    for state in progressing {
        let completes = state.matches_remaining <= 1
            && (matches!(state.completion.as_str(), "matches" | "either")
                || (state.completion == "both"
                    && state
                        .expires_at
                        .is_some_and(|expires| expires <= recorded_at)));
        if completes {
            transaction.execute(
                "INSERT INTO moderation_events (
                    guild_id, discord_id, event_type, source, actor_id, reason,
                    scope, completion, expires_at, matches_required,
                    matches_remaining, pending_match_watermark, wins_required,
                    wins_remaining, match_id, created_at
                 ) VALUES (
                    ?1, ?2, 'complete', 'system', NULL,
                    'Suspension requirements completed', ?3, ?4, ?5, ?6, 0,
                    ?7, NULL, NULL, ?8, ?9
                 )",
                params![
                    guild_id,
                    state.discord_id,
                    state.scope,
                    state.completion,
                    state.expires_at,
                    state.matches_required,
                    state.pending_match_watermark,
                    match_id,
                    recorded_at,
                ],
            )?;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CompletingLowPriorityState {
    discord_id: i64,
    wins_required: i64,
    start_pending_match_id: Option<i64>,
}

fn progress_low_priority_wins(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: Option<i64>,
    match_id: i64,
    winning_ids: &[i64],
) -> Result<(), rusqlite::Error> {
    let winning_ids = unique_i64(winning_ids);
    if winning_ids.is_empty() {
        return Ok(());
    }

    let recorded_at = unix_now();
    for chunk in winning_ids.chunks(BULK_CHUNK_SIZE) {
        let eligible = format!(
            "guild_id = ? AND active = 1 AND wins_remaining > 0
             AND (
                 start_pending_match_id IS NULL
                 OR (? IS NOT NULL AND ? > start_pending_match_id)
             )
             AND discord_id IN ({})",
            placeholders(chunk.len())
        );
        let mut parameters = Vec::with_capacity(chunk.len() + 3);
        parameters.push(Value::Integer(guild_id));
        parameters.push(pending_match_id.map_or(Value::Null, Value::Integer));
        parameters.push(pending_match_id.map_or(Value::Null, Value::Integer));
        parameters.extend(chunk.iter().copied().map(Value::Integer));

        let mut statement = transaction.prepare(&format!(
            "SELECT discord_id, wins_required, start_pending_match_id
             FROM low_priority_state
             WHERE wins_remaining = 1 AND {eligible}"
        ))?;
        let completing = statement
            .query_map(params_from_iter(parameters.iter()), |row| {
                Ok(CompletingLowPriorityState {
                    discord_id: row.get(0)?,
                    wins_required: row.get(1)?,
                    start_pending_match_id: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        for state in completing {
            transaction.execute(
                "INSERT INTO moderation_events (
                    guild_id, discord_id, event_type, source, actor_id, reason,
                    scope, completion, expires_at, matches_required,
                    matches_remaining, pending_match_watermark, wins_required,
                    wins_remaining, match_id, created_at
                 ) VALUES (
                    ?1, ?2, 'lowprio_complete', 'system', NULL,
                    'Required wins completed', NULL, NULL, NULL, NULL, NULL,
                    ?3, ?4, 0, ?5, ?6
                 )",
                params![
                    guild_id,
                    state.discord_id,
                    state.start_pending_match_id,
                    state.wins_required,
                    match_id,
                    recorded_at,
                ],
            )?;
        }

        transaction.execute(
            &format!(
                "UPDATE low_priority_state
                 SET wins_remaining = MAX(0, wins_remaining - 1),
                     active = CASE WHEN wins_remaining <= 1 THEN 0 ELSE 1 END,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE {eligible}"
            ),
            params_from_iter(parameters.iter()),
        )?;
    }
    Ok(())
}

fn upsert_match_prediction(
    transaction: &Transaction<'_>,
    match_id: i64,
    prediction: &MatchPredictionSnapshot,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO match_predictions (
            match_id, radiant_rating, dire_rating, radiant_rd, dire_rd,
            expected_radiant_win_prob, openskill_radiant_win_prob,
            openskill_raw_radiant_win_prob, openskill_algorithm_version,
            openskill_algorithm_fingerprint
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(match_id) DO UPDATE SET
            radiant_rating=excluded.radiant_rating,
            dire_rating=excluded.dire_rating,
            radiant_rd=excluded.radiant_rd,
            dire_rd=excluded.dire_rd,
            expected_radiant_win_prob=excluded.expected_radiant_win_prob,
            openskill_radiant_win_prob=excluded.openskill_radiant_win_prob,
            openskill_raw_radiant_win_prob=excluded.openskill_raw_radiant_win_prob,
            openskill_algorithm_version=excluded.openskill_algorithm_version,
            openskill_algorithm_fingerprint=excluded.openskill_algorithm_fingerprint,
            timestamp=CURRENT_TIMESTAMP",
        params![
            match_id,
            prediction.radiant_rating,
            prediction.dire_rating,
            prediction.radiant_rd,
            prediction.dire_rd,
            prediction.expected_radiant_win_prob,
            prediction.openskill_radiant_win_prob,
            prediction.openskill_raw_radiant_win_prob,
            OPENSKILL_ALGORITHM_VERSION,
            OPENSKILL_ALGORITHM_FINGERPRINT,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::sync::OnceLock;

    use cama_domain::rating::{CamaRatingSystem, TeamPlayer};
    use cama_domain::shuffler::{BalancedShuffler, PoolOptions};
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::*;
    use crate::test_support::FastTestDatabase;

    const TEST_GUILD_ID: i64 = 987_654_321;
    const TEST_GUILD_ID_SECONDARY: i64 = 987_654_322;

    static FIXTURE_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();

    enum FixtureDatabase {
        Fast(FastTestDatabase),
        Durable(NamedTempFile),
    }

    impl FixtureDatabase {
        fn path(&self) -> &Path {
            match self {
                Self::Fast(database) => database.path(),
                Self::Durable(database) => database.path(),
            }
        }
    }

    struct Fixture {
        file: FixtureDatabase,
        players: PlayerRepository,
        matches: MatchRepository,
    }

    impl Fixture {
        fn new() -> Self {
            Self::from_database(FixtureDatabase::Fast(FastTestDatabase::from_template(
                Self::template().path(),
            )))
        }

        fn new_durable() -> Self {
            let file = NamedTempFile::new().expect("create on-disk SQLite fixture");
            std::fs::copy(Self::template().path(), file.path())
                .expect("copy SQLite fixture template");
            Self::from_database(FixtureDatabase::Durable(file))
        }

        fn template() -> &'static NamedTempFile {
            FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
                let file = NamedTempFile::new().expect("create SQLite fixture template");
                let connection =
                    Connection::open(file.path()).expect("open fixture template for schema setup");
                connection
                    .execute_batch(TEST_SCHEMA)
                    .expect("install Python-compatible disposable schema template");
                drop(connection);
                file
            })
        }

        fn from_database(file: FixtureDatabase) -> Self {
            Self {
                players: PlayerRepository::new(file.path()),
                matches: MatchRepository::new(file.path()),
                file,
            }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture directly")
        }

        fn add_player(&self, discord_id: i64, guild_id: i64) {
            self.players
                .add(&NewPlayer::new(
                    discord_id,
                    format!("Player {discord_id}"),
                    Some(guild_id),
                ))
                .expect("add fixture player");
        }

        fn record(&self, team1: &[i64], team2: &[i64], winner: i64, guild_id: i64) -> i64 {
            self.matches
                .record_match(&MatchRecord::new(
                    team1.to_vec(),
                    team2.to_vec(),
                    winner,
                    Some(guild_id),
                ))
                .expect("record fixture match")
        }

        fn add_history(
            &self,
            discord_id: i64,
            guild_id: i64,
            match_id: Option<i64>,
            won: Option<bool>,
        ) -> i64 {
            self.matches
                .add_rating_history(
                    Some(guild_id),
                    &NewCoreRatingHistory {
                        discord_id,
                        rating: Some(1500.0),
                        match_id,
                        won,
                        ..NewCoreRatingHistory::default()
                    },
                )
                .expect("insert fixture rating history")
        }
    }

    fn configured_player(discord_id: i64, name: &str, guild_id: i64) -> NewPlayer {
        let mut player = NewPlayer::new(discord_id, name, Some(guild_id));
        player.initial_mmr = Some(3_000);
        player.preferred_roles = Some(vec!["1".to_owned(), "2".to_owned()]);
        player.glicko_rating = Some(1_500.0);
        player.glicko_rd = Some(350.0);
        player.glicko_volatility = Some(0.06);
        player
    }

    fn assert_approx(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, found {actual}"
        );
    }

    fn ids_for_team(team: &cama_domain::team::Team) -> Vec<i64> {
        team.players
            .iter()
            .map(|player| player.discord_id.expect("fixture players have Discord IDs"))
            .collect()
    }

    fn core_match(team1_ids: &[i64], team2_ids: &[i64], winning_team: i64) -> CoreMatchRecord {
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(
                team1_ids.to_vec(),
                team2_ids.to_vec(),
                winning_team,
                Some(TEST_GUILD_ID),
            ),
            "2026-08-12T00:00:00+00:00",
        );
        if winning_team == 1 {
            core.winning_ids = team1_ids.to_vec();
            core.losing_ids = team2_ids.to_vec();
        } else {
            core.winning_ids = team2_ids.to_vec();
            core.losing_ids = team1_ids.to_vec();
        }
        core
    }

    fn add_rated_player(
        fixture: &Fixture,
        discord_id: i64,
        name: impl Into<String>,
        rating: f64,
        roles: Option<Vec<String>>,
    ) {
        let mut player = NewPlayer::new(discord_id, name, Some(TEST_GUILD_ID));
        player.glicko_rating = Some(rating);
        player.glicko_rd = Some(350.0);
        player.glicko_volatility = Some(0.06);
        player.preferred_roles = roles;
        fixture
            .players
            .add(&player)
            .expect("insert rated fixture player");
    }

    fn install_ledger_context_schema(fixture: &Fixture) {
        fixture
            .connection()
            .execute_batch(
                "CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TABLE economy_ledger_entries (
                     ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     account_type TEXT NOT NULL DEFAULT 'player',
                     account_id INTEGER,
                     delta INTEGER NOT NULL,
                     balance_before INTEGER,
                     balance_after INTEGER,
                     source TEXT NOT NULL,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         NEW.guild_id, 'player', NEW.discord_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         OLD.jopacoin_balance, NEW.jopacoin_balance,
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                  'balance_update'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;",
            )
            .expect("install economy ledger fixture schema");
    }

    #[test]
    fn test_player_balance_context_is_recorded_and_does_not_leak() {
        let fixture = Fixture::new();
        install_ledger_context_schema(&fixture);
        fixture.add_player(111, TEST_GUILD_ID);
        fixture
            .connection()
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear seed ledger row");

        let context = BalanceLedgerContext {
            source: Some("gamba".to_owned()),
            actor_id: Some(111),
            related_type: Some("wheel_spin".to_owned()),
            related_id: Some("LIGHTNING_BOLT".to_owned()),
            reason: Some("gamba lightning bolt tax".to_owned()),
            metadata: Some(r#"{"tax_pct":0.02}"#.to_owned()),
        };
        fixture
            .players
            .add_balance_with_context(111, Some(TEST_GUILD_ID), 10, Some(&context))
            .expect("contextual balance mutation");
        fixture
            .players
            .add_balance_with_context(111, Some(TEST_GUILD_ID), 1, None)
            .expect("ordinary balance mutation");

        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT source, actor_id, related_type, related_id, reason, metadata
                   FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .expect("prepare ledger query");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .expect("query ledger rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ledger rows");
        assert_eq!(
            rows,
            vec![
                (
                    "gamba".to_owned(),
                    Some(111),
                    Some("wheel_spin".to_owned()),
                    Some("LIGHTNING_BOLT".to_owned()),
                    Some("gamba lightning bolt tax".to_owned()),
                    Some(r#"{"tax_pct":0.02}"#.to_owned()),
                ),
                ("balance_update".to_owned(), None, None, None, None, None,),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("context cleared"),
            0
        );
    }

    #[test]
    fn test_gamba_steal_context_records_victim_and_thief_rows() {
        let fixture = Fixture::new();
        install_ledger_context_schema(&fixture);
        fixture.add_player(201, TEST_GUILD_ID);
        fixture.add_player(202, TEST_GUILD_ID);
        fixture
            .players
            .update_balance(201, Some(TEST_GUILD_ID), 10)
            .expect("seed thief balance");
        fixture
            .players
            .update_balance(202, Some(TEST_GUILD_ID), 30)
            .expect("seed victim balance");
        fixture
            .connection()
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear seed ledger rows");

        let context = BalanceLedgerContext {
            source: Some("gamba".to_owned()),
            actor_id: Some(201),
            related_type: Some("wheel_spin".to_owned()),
            related_id: Some("GREEN_SHELL".to_owned()),
            reason: Some("gamba green shell steal".to_owned()),
            metadata: None,
        };
        fixture
            .players
            .steal_atomic_with_context(201, 202, Some(TEST_GUILD_ID), 7, Some(&context))
            .expect("contextual atomic steal");

        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT account_id, delta, source, reason
                   FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .expect("prepare steal ledger query");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .expect("query steal ledger rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect steal ledger rows");
        assert_eq!(
            rows,
            vec![
                (
                    202,
                    -7,
                    "gamba".to_owned(),
                    Some("gamba green shell steal victim debit".to_owned()),
                ),
                (
                    201,
                    7,
                    "gamba".to_owned(),
                    Some("gamba green shell steal thief credit".to_owned()),
                ),
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("context cleared"),
            0
        );
    }

    #[test]
    fn test_add_and_get_player() {
        let fixture = Fixture::new();
        fixture
            .players
            .add(&configured_player(12_345, "TestPlayer", TEST_GUILD_ID))
            .unwrap();
        let player = fixture
            .players
            .get_by_id(12_345, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(player.name, "TestPlayer");
        assert_eq!(player.mmr, Some(3_000));
        assert_eq!(
            player.preferred_roles,
            Some(vec!["1".to_owned(), "2".to_owned()])
        );
    }

    #[test]
    fn test_player_not_found() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .players
                .get_by_id(99_999, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_exists() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        assert!(fixture.players.exists(12_345, Some(TEST_GUILD_ID)).unwrap());
        assert!(!fixture.players.exists(99_999, Some(TEST_GUILD_ID)).unwrap());
    }

    #[test]
    fn wheel_cooldown_claim_is_atomic_and_reports_the_persisted_anchor() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        assert_eq!(
            fixture
                .players
                .try_claim_wheel_spin(12_345, Some(TEST_GUILD_ID), 1_000, 86_400)
                .unwrap(),
            WheelSpinClaim::Claimed {
                previous_spin: None
            }
        );
        assert_eq!(
            fixture
                .players
                .try_claim_wheel_spin(12_345, Some(TEST_GUILD_ID), 1_001, 86_400)
                .unwrap(),
            WheelSpinClaim::Cooldown { last_spin: 1_000 }
        );
        assert_eq!(
            fixture
                .players
                .try_claim_wheel_spin(12_345, Some(TEST_GUILD_ID), 87_400, 86_400)
                .unwrap(),
            WheelSpinClaim::Claimed {
                previous_spin: Some(1_000)
            }
        );
        assert_eq!(
            fixture
                .players
                .try_claim_wheel_spin(99_999, Some(TEST_GUILD_ID), 1_000, 86_400)
                .unwrap(),
            WheelSpinClaim::MissingPlayer
        );
    }

    #[test]
    fn test_update_roles() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        fixture
            .players
            .update_roles(
                12_345,
                Some(TEST_GUILD_ID),
                &["3".to_owned(), "4".to_owned(), "5".to_owned()],
            )
            .unwrap();
        assert_eq!(
            fixture
                .players
                .get_by_id(12_345, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .preferred_roles,
            Some(vec!["3".to_owned(), "4".to_owned(), "5".to_owned()])
        );
    }

    #[test]
    fn test_update_glicko_rating() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        fixture
            .players
            .update_glicko_rating(12_345, Some(TEST_GUILD_ID), 1_600.0, 300.0, 0.05)
            .unwrap();
        assert_eq!(
            fixture
                .players
                .get_glicko_rating(12_345, Some(TEST_GUILD_ID))
                .unwrap(),
            Some((1_600.0, Some(300.0), Some(0.05)))
        );
    }

    #[test]
    fn test_get_match_rating_inputs_bulk() {
        let fixture = Fixture::new();
        let mut player = configured_player(12_345, "MatchPlayer", TEST_GUILD_ID);
        player.initial_mmr = Some(3_200);
        player.glicko_rating = Some(1_550.0);
        player.glicko_rd = Some(120.0);
        player.glicko_volatility = Some(0.05);
        player.os_mu = Some(27.5);
        player.os_sigma = Some(7.25);
        fixture.players.add(&player).unwrap();
        fixture
            .connection()
            .execute(
                "UPDATE players SET last_match_date=?1, first_calibrated_at=?2
             WHERE discord_id=?3 AND guild_id=?4",
                params![
                    "2026-07-20T12:00:00+00:00",
                    1_750_000_000_i64,
                    12_345,
                    TEST_GUILD_ID
                ],
            )
            .unwrap();
        let inputs = fixture
            .players
            .get_match_rating_inputs(&[12_345, 99_999, 12_345], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(inputs.keys().copied().collect::<Vec<_>>(), [12_345]);
        let input = inputs.get(&12_345).unwrap();
        assert_eq!(input.current_mmr, Value::Real(3_200.0));
        assert_eq!(input.os_mu, Value::Real(27.5));
        assert_eq!(
            input.last_match_date,
            Value::Text("2026-07-20T12:00:00+00:00".to_owned())
        );
        assert!(
            fixture
                .players
                .get_match_rating_inputs(&[], None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_balances_bulk_is_single_query_and_guild_scoped() {
        let fixture = Fixture::new();
        for (discord_id, balance) in [(12_346, 75), (12_347, -25)] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), balance)
                .unwrap();
        }
        fixture.add_player(12_346, TEST_GUILD_ID_SECONDARY);
        fixture
            .players
            .update_balance(12_346, Some(TEST_GUILD_ID_SECONDARY), 999)
            .unwrap();
        let balances = fixture
            .players
            .get_balances_bulk(&[12_347, 12_346, 99_999, 12_347], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            balances.into_iter().collect::<Vec<_>>(),
            [(12_347, -25), (12_346, 75), (99_999, 0)]
        );
    }

    #[test]
    fn test_get_reminder_timestamps_bulk_is_single_read_and_guild_scoped() {
        let fixture = Fixture::new();
        for discord_id in [12_348, 12_349] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }
        fixture.add_player(12_348, TEST_GUILD_ID_SECONDARY);
        let connection = fixture.connection();
        for values in [
            (
                Value::Integer(100),
                Value::Integer(200),
                12_348,
                TEST_GUILD_ID,
            ),
            (
                Value::Text("three hundred".to_owned()),
                Value::Null,
                12_349,
                TEST_GUILD_ID,
            ),
            (
                Value::Integer(999),
                Value::Integer(999),
                12_348,
                TEST_GUILD_ID_SECONDARY,
            ),
        ] {
            connection
                .execute(
                    "UPDATE players SET last_wheel_spin=?1,last_trivia_session=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                    params![values.0, values.1, values.2, values.3],
                )
                .unwrap();
        }
        let timestamps = fixture
            .players
            .get_reminder_timestamps_bulk(&[12_349, 99_999, 12_348, 12_349], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            timestamps.keys().copied().collect::<Vec<_>>(),
            [12_349, 99_999, 12_348]
        );
        assert_eq!(
            timestamps.get(&12_349).unwrap().last_wheel_spin,
            Value::Text("three hundred".to_owned())
        );
        assert_eq!(
            timestamps.get(&99_999),
            Some(&ReminderTimestamps::default())
        );
        assert_eq!(
            timestamps.get(&12_348).unwrap().last_wheel_spin,
            Value::Integer(100)
        );
    }

    #[test]
    fn test_update_personal_best_win_streaks_is_atomic_and_guild_scoped() {
        let fixture = Fixture::new();
        for discord_id in [12_351, 12_352, 12_353] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }
        fixture.add_player(12_351, TEST_GUILD_ID_SECONDARY);
        fixture
            .players
            .update_personal_best_win_streak(12_351, Some(TEST_GUILD_ID), 4)
            .unwrap();
        fixture
            .players
            .update_personal_best_win_streak(12_352, Some(TEST_GUILD_ID), 8)
            .unwrap();
        fixture
            .players
            .update_personal_best_win_streak(12_351, Some(TEST_GUILD_ID_SECONDARY), 10)
            .unwrap();
        let mut updates = OrderedMap::new();
        for (discord_id, streak) in [(12_351, 6), (12_352, 7), (12_353, 5), (99_999, 12)] {
            updates.insert(discord_id, streak);
        }
        let previous = fixture
            .players
            .update_personal_best_win_streaks(&updates, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            previous.into_iter().collect::<Vec<_>>(),
            [(12_351, 4), (12_353, 0)]
        );
        assert_eq!(
            fixture
                .players
                .get_personal_best_win_streak(12_351, Some(TEST_GUILD_ID))
                .unwrap(),
            6
        );
        assert_eq!(
            fixture
                .players
                .get_personal_best_win_streak(12_352, Some(TEST_GUILD_ID))
                .unwrap(),
            8
        );
        assert_eq!(
            fixture
                .players
                .get_personal_best_win_streak(12_351, Some(TEST_GUILD_ID_SECONDARY))
                .unwrap(),
            10
        );
    }

    #[test]
    fn test_get_by_ids_preserves_order() {
        let fixture = Fixture::new();
        for index in 0..5 {
            fixture
                .players
                .add(&NewPlayer::new(
                    1_000 + index,
                    format!("Player{index}"),
                    Some(TEST_GUILD_ID),
                ))
                .unwrap();
        }
        let players = fixture
            .players
            .get_by_ids(&[1_003, 1_001, 1_004, 1_000, 1_002], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            players
                .into_iter()
                .map(|player| player.name)
                .collect::<Vec<_>>(),
            ["Player3", "Player1", "Player4", "Player0", "Player2"]
        );
    }

    #[test]
    fn test_get_shuffle_inputs_batches_ordered_players_and_metadata() {
        let fixture = Fixture::new();
        for discord_id in [10_101, 10_102] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }
        fixture.add_player(10_101, TEST_GUILD_ID_SECONDARY);
        let connection = fixture.connection();
        connection.execute("UPDATE players SET last_match_date=?1,exclusion_count=7 WHERE discord_id=10101 AND guild_id=?2", params!["2026-07-20T12:00:00+00:00", TEST_GUILD_ID]).unwrap();
        connection.execute("UPDATE players SET last_match_date=NULL,exclusion_count=11 WHERE discord_id=10102 AND guild_id=?1", [TEST_GUILD_ID]).unwrap();
        let inputs = fixture
            .players
            .get_shuffle_inputs(&[10_102, 99_999, 10_101, 10_102], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            inputs
                .players
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<Vec<_>>(),
            [10_102, 10_101, 10_102]
        );
        assert_eq!(
            inputs.last_match_dates.keys().copied().collect::<Vec<_>>(),
            [10_102, 10_101]
        );
        assert_eq!(inputs.exclusion_counts.get(&10_102), Some(&11));
        assert_eq!(inputs.exclusion_counts.get(&10_101), Some(&7));
    }

    #[test]
    fn test_get_by_username_partial_case_insensitive() {
        let fixture = Fixture::new();
        for (discord_id, name) in [
            (2_001, "AlphaUser"),
            (2_002, "betaUser"),
            (2_003, "GammaTester"),
        ] {
            fixture
                .players
                .add(&NewPlayer::new(discord_id, name, Some(TEST_GUILD_ID)))
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_by_username("alpha", Some(TEST_GUILD_ID))
                .unwrap()[0]
                .discord_id,
            2_001
        );
        let matches = fixture
            .players
            .get_by_username("USER", Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            matches
                .into_iter()
                .map(|row| row.discord_id)
                .collect::<HashSet<_>>(),
            HashSet::from([2_001, 2_002])
        );
    }

    #[test]
    fn test_exclusion_counts() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[12_345], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&12_345),
            Some(&NEW_PLAYER_EXCLUSION_BOOST)
        );
        fixture
            .players
            .increment_exclusion_count(12_345, Some(TEST_GUILD_ID))
            .unwrap();
        fixture
            .players
            .increment_exclusion_count(12_345, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[12_345], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&12_345),
            Some(&(NEW_PLAYER_EXCLUSION_BOOST + 12))
        );
        for _ in 0..20 {
            fixture
                .players
                .decay_exclusion_count(12_345, Some(TEST_GUILD_ID))
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[12_345], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&12_345),
            Some(&0)
        );
    }

    #[test]
    fn test_delete_player() {
        let fixture = Fixture::new();
        fixture.add_player(12_345, TEST_GUILD_ID);
        assert!(fixture.players.delete(12_345, Some(TEST_GUILD_ID)).unwrap());
        assert!(!fixture.players.delete(12_345, Some(TEST_GUILD_ID)).unwrap());
        assert!(
            fixture
                .players
                .get_by_id(12_345, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_player_does_not_touch_other_guild() {
        let fixture = Fixture::new();
        let discord_id = 555_000_000;
        fixture.add_player(discord_id, TEST_GUILD_ID);
        fixture.add_player(discord_id, TEST_GUILD_ID_SECONDARY);

        assert!(
            fixture
                .players
                .delete(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
        );
        assert!(
            fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID_SECONDARY))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_delete_player_returns_false_when_guild_mismatch() {
        let fixture = Fixture::new();
        let discord_id = 555_000_000;
        fixture.add_player(discord_id, TEST_GUILD_ID_SECONDARY);

        assert!(
            !fixture
                .players
                .delete(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
        );
        assert!(
            fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID_SECONDARY))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_get_exclusion_counts_scoped_to_guild() {
        let fixture = Fixture::new();
        let discord_id = 555_000_000;
        fixture.add_player(discord_id, TEST_GUILD_ID);
        fixture.add_player(discord_id, TEST_GUILD_ID_SECONDARY);
        let connection = fixture.connection();
        connection
            .execute(
                "UPDATE players SET exclusion_count=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![3, discord_id, TEST_GUILD_ID],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE players SET exclusion_count=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![99, discord_id, TEST_GUILD_ID_SECONDARY],
            )
            .unwrap();

        let counts_a = fixture
            .players
            .get_exclusion_counts(&[discord_id], Some(TEST_GUILD_ID))
            .unwrap();
        let counts_b = fixture
            .players
            .get_exclusion_counts(&[discord_id], Some(TEST_GUILD_ID_SECONDARY))
            .unwrap();
        assert_eq!(counts_a.get(&discord_id), Some(&3));
        assert_eq!(counts_b.get(&discord_id), Some(&99));
    }

    // Exact compatibility aliases for the retained facade-level database
    // contracts.  The repository tests above remain the canonical focused
    // cases; these keep the legacy API surface independently enumerable in the
    // cross-language parity ledger.
    #[test]
    fn test_add_player() {
        test_add_and_get_player();
    }

    #[test]
    fn test_get_player_not_found() {
        test_player_not_found();
    }

    #[test]
    fn test_update_player_glicko_rating() {
        test_update_glicko_rating();
    }

    #[test]
    fn test_get_players_by_ids() {
        test_get_by_ids_preserves_order();
    }

    #[test]
    fn test_record_match_creates_match() {
        test_record_match();
    }

    #[test]
    fn test_database_delete_player() {
        test_delete_player();
    }

    #[test]
    fn test_get_exclusion_counts() {
        test_exclusion_counts();
    }

    #[test]
    fn test_get_by_ids_skips_missing_players() {
        let fixture = Fixture::new();
        for (discord_id, name) in [(7_001, "Player1"), (7_002, "Player2")] {
            fixture
                .players
                .add(&NewPlayer::new(discord_id, name, Some(TEST_GUILD_ID)))
                .unwrap();
        }
        let players = fixture
            .players
            .get_by_ids(&[7_001, 7_002, 99_999], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(players.len(), 2);
        assert!(
            players
                .iter()
                .all(|player| matches!(player.name.as_str(), "Player1" | "Player2"))
        );
    }

    #[test]
    fn test_record_match_win_loss_is_scoped_to_guild() {
        let fixture = Fixture::new();
        let winners = [4_001, 4_002];
        let losers = [4_003, 4_004];
        for discord_id in winners.into_iter().chain(losers) {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture.add_player(discord_id, TEST_GUILD_ID_SECONDARY);
        }
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(winners.to_vec(), losers.to_vec(), 1, Some(TEST_GUILD_ID)),
            "2026-08-12T00:00:00+00:00",
        );
        core.winning_ids = winners.to_vec();
        core.losing_ids = losers.to_vec();
        fixture.matches.record_match_core_atomic(&core).unwrap();

        let connection = fixture.connection();
        for discord_id in winners {
            let primary: (i64, i64) = connection
                .query_row(
                    "SELECT wins,losses FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![discord_id, TEST_GUILD_ID],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(primary, (1, 0));
        }
        for discord_id in losers {
            let primary: (i64, i64) = connection
                .query_row(
                    "SELECT wins,losses FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![discord_id, TEST_GUILD_ID],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(primary, (0, 1));
        }
        for discord_id in winners.into_iter().chain(losers) {
            let secondary: (i64, i64) = connection
                .query_row(
                    "SELECT wins,losses FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![discord_id, TEST_GUILD_ID_SECONDARY],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(secondary, (0, 0));
        }
    }

    #[test]
    fn test_delete_fake_users_cascades_related_tables() {
        let fixture = Fixture::new();
        fixture.add_player(100, TEST_GUILD_ID);
        fixture.add_player(-1, TEST_GUILD_ID);
        fixture.add_player(-2, TEST_GUILD_ID);
        let match_id = fixture.record(&[-1, -2], &[100], 1, TEST_GUILD_ID);
        let connection = fixture.connection();
        connection
            .execute(
                "INSERT INTO rating_history(discord_id,rating,match_id,guild_id)
                 VALUES(-1,1200,?1,?2)",
                params![match_id, TEST_GUILD_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,bet_time)
                 VALUES(?1,NULL,-2,'radiant',10,0)",
                [TEST_GUILD_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO openskill_rating_events(
                    guild_id,discord_id,event_type,value,event_at,
                    os_algorithm_version,os_algorithm_fingerprint
                 ) VALUES(?1,-1,'test',1,0,5,'test')",
                [TEST_GUILD_ID],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            fixture
                .players
                .delete_fake_users(Some(TEST_GUILD_ID))
                .unwrap(),
            2
        );
        let connection = fixture.connection();
        for table in [
            "players",
            "match_participants",
            "rating_history",
            "bets",
            "openskill_rating_events",
        ] {
            let count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE discord_id < 0"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "fake rows remain in {table}");
        }
        assert!(
            fixture
                .players
                .get_by_id(100, Some(TEST_GUILD_ID))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_clear_all_players_compatibility_operation() {
        let fixture = Fixture::new();
        for discord_id in 10_001..10_006 {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }
        let connection = fixture.connection();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM players", [], |row| row.get(0))
            .unwrap();
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 DELETE FROM players;
                 DELETE FROM match_participants;
                 DELETE FROM rating_history;
                 DELETE FROM match_predictions;
                 DELETE FROM matches;
                 COMMIT;",
            )
            .unwrap();
        assert_eq!(count, 5);
        assert!(
            fixture
                .players
                .get_by_id(10_001, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_remove_fake_users_across_guilds() {
        let fixture = Fixture::new();
        fixture.add_player(-1, TEST_GUILD_ID);
        fixture.add_player(-2, TEST_GUILD_ID_SECONDARY);
        fixture.add_player(200, TEST_GUILD_ID);
        let total = [TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY]
            .into_iter()
            .map(|guild_id| fixture.players.delete_fake_users(Some(guild_id)).unwrap())
            .sum::<usize>();
        assert_eq!(total, 2);
        assert!(
            fixture
                .players
                .get_by_id(-1, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .players
                .get_by_id(-2, Some(TEST_GUILD_ID_SECONDARY))
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .players
                .get_by_id(200, Some(TEST_GUILD_ID))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_increment_exclusion_count() {
        let fixture = Fixture::new();
        fixture.add_player(11_101, TEST_GUILD_ID);
        fixture
            .players
            .increment_exclusion_count(11_101, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[11_101], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&11_101),
            Some(&(NEW_PLAYER_EXCLUSION_BOOST + 6))
        );
        fixture
            .players
            .increment_exclusion_count(11_101, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[11_101], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&11_101),
            Some(&(NEW_PLAYER_EXCLUSION_BOOST + 12))
        );
    }

    #[test]
    fn test_decay_exclusion_count() {
        let fixture = Fixture::new();
        fixture.add_player(11_201, TEST_GUILD_ID);
        for _ in 0..10 {
            fixture
                .players
                .increment_exclusion_count(11_201, Some(TEST_GUILD_ID))
                .unwrap();
        }
        let mut expected = NEW_PLAYER_EXCLUSION_BOOST + 60;
        for _ in 0..6 {
            fixture
                .players
                .decay_exclusion_count(11_201, Some(TEST_GUILD_ID))
                .unwrap();
            expected -= 1;
            assert_eq!(
                fixture
                    .players
                    .get_exclusion_counts(&[11_201], Some(TEST_GUILD_ID))
                    .unwrap()
                    .get(&11_201),
                Some(&expected)
            );
        }
    }

    #[test]
    fn test_exclusion_count_decay_stops_at_zero() {
        let fixture = Fixture::new();
        fixture.add_player(11_301, TEST_GUILD_ID);
        for _ in 0..=NEW_PLAYER_EXCLUSION_BOOST {
            fixture
                .players
                .decay_exclusion_count(11_301, Some(TEST_GUILD_ID))
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_exclusion_counts(&[11_301], Some(TEST_GUILD_ID))
                .unwrap()
                .get(&11_301),
            Some(&0)
        );
    }

    #[test]
    fn test_get_player_above_returns_higher_balance() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .players
                .get_player_above(99_999, Some(TEST_GUILD_ID), None)
                .unwrap()
                .is_none()
        );
        for (discord_id, balance) in [(1_001, 10), (1_002, 50), (1_003, 100)] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), balance)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_above(1_001, Some(TEST_GUILD_ID), None)
                .unwrap()
                .unwrap()
                .discord_id,
            Some(1_002)
        );
        assert_eq!(
            fixture
                .players
                .get_player_above(1_002, Some(TEST_GUILD_ID), None)
                .unwrap()
                .unwrap()
                .discord_id,
            Some(1_003)
        );
        assert!(
            fixture
                .players
                .get_player_above(1_003, Some(TEST_GUILD_ID), None)
                .unwrap()
                .is_none()
        );
        for discord_id in [2_001, 2_002] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), 500)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_above(2_002, Some(TEST_GUILD_ID), None)
                .unwrap()
                .unwrap()
                .discord_id,
            Some(2_001)
        );
    }

    #[test]
    fn test_get_player_above_skips_players_below_minimum_balance() {
        let fixture = Fixture::new();
        for (discord_id, balance) in [(1_101, 2), (1_102, 10), (1_103, 100)] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), balance)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_above(1_101, Some(TEST_GUILD_ID), Some(50))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(1_103)
        );
    }

    #[test]
    fn test_get_player_below_returns_lower_balance() {
        let fixture = Fixture::new();
        for (discord_id, balance) in [(3_001, 10), (3_002, 50), (3_003, 100)] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), balance)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_below(3_003, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(3_002)
        );
        assert_eq!(
            fixture
                .players
                .get_player_below(3_002, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(3_001)
        );
        assert!(
            fixture
                .players
                .get_player_below(3_001, Some(TEST_GUILD_ID))
                .unwrap()
                .is_none()
        );
        for discord_id in [4_001, 4_002] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), 500)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_below(4_001, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(4_002)
        );
        assert_eq!(
            fixture
                .players
                .get_player_below(4_002, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(3_003)
        );
    }

    #[test]
    fn test_balance_neighbors_match_wins_and_rating_tiebreakers() {
        let fixture = Fixture::new();
        for (discord_id, rating) in [(5_003, 1_600.0), (5_002, 1_400.0), (5_001, 1_400.0)] {
            let mut player = NewPlayer::new(
                discord_id,
                format!("Player {discord_id}"),
                Some(TEST_GUILD_ID),
            );
            player.glicko_rating = Some(rating);
            fixture.players.add(&player).unwrap();
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), 50)
                .unwrap();
        }
        fixture
            .players
            .increment_wins(5_001, Some(TEST_GUILD_ID))
            .unwrap();
        fixture
            .players
            .increment_wins(5_002, Some(TEST_GUILD_ID))
            .unwrap();
        let leaderboard = fixture
            .players
            .get_leaderboard(Some(TEST_GUILD_ID), 3, 0)
            .unwrap();
        assert_eq!(
            leaderboard
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<Vec<_>>(),
            [5_001, 5_002, 5_003]
        );
        assert_eq!(
            fixture
                .players
                .get_player_above(5_002, Some(TEST_GUILD_ID), None)
                .unwrap()
                .unwrap()
                .discord_id,
            Some(5_001)
        );
        assert_eq!(
            fixture
                .players
                .get_player_below(5_002, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(5_003)
        );
    }

    #[test]
    fn test_leaderboard_sorts_by_jopacoin_then_wins_then_rating() {
        let fixture = Fixture::new();
        for (discord_id, balance, wins, rating) in [
            (400_001, 100, 1, 2_000.0),
            (400_002, 100, 5, 1_500.0),
            (400_003, 50, 10, 2_000.0),
            (400_004, 100, 1, 1_800.0),
        ] {
            let mut player = NewPlayer::new(
                discord_id,
                format!("Player {discord_id}"),
                Some(TEST_GUILD_ID),
            );
            player.glicko_rating = Some(rating);
            fixture
                .players
                .add(&player)
                .expect("insert leaderboard player");
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), balance)
                .expect("set leaderboard balance");
            for _ in 0..wins {
                fixture
                    .players
                    .increment_wins(discord_id, Some(TEST_GUILD_ID))
                    .expect("increment leaderboard wins");
            }
        }

        let leaderboard = fixture
            .players
            .get_leaderboard(Some(TEST_GUILD_ID), 4, 0)
            .expect("load balance leaderboard");
        assert_eq!(
            leaderboard
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<Vec<_>>(),
            [400_002, 400_001, 400_004, 400_003]
        );
    }

    #[test]
    fn test_balance_neighbors_use_rating_before_discord_id() {
        let fixture = Fixture::new();
        for (discord_id, rating) in [(5_101, 1_400.0), (5_102, 1_600.0)] {
            let mut player = NewPlayer::new(discord_id, "Rated", Some(TEST_GUILD_ID));
            player.glicko_rating = Some(rating);
            fixture.players.add(&player).unwrap();
            fixture
                .players
                .update_balance(discord_id, Some(TEST_GUILD_ID), 50)
                .unwrap();
        }
        assert_eq!(
            fixture
                .players
                .get_player_above(5_101, Some(TEST_GUILD_ID), None)
                .unwrap()
                .unwrap()
                .discord_id,
            Some(5_102)
        );
        assert_eq!(
            fixture
                .players
                .get_player_below(5_102, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .discord_id,
            Some(5_101)
        );
    }

    #[test]
    fn test_player_leaderboards_use_id_for_exact_ties() {
        let fixture = Fixture::new();
        for discord_id in [5_202, 5_201] {
            let mut player = NewPlayer::new(discord_id, "Tied", Some(TEST_GUILD_ID));
            player.glicko_rating = Some(1_500.0);
            player.os_mu = Some(25.0);
            fixture.players.add(&player).unwrap();
        }
        for leaderboard in [
            fixture
                .players
                .get_leaderboard(Some(TEST_GUILD_ID), 2, 0)
                .unwrap(),
            fixture
                .players
                .get_leaderboard_by_glicko(Some(TEST_GUILD_ID), 2, 0, 0)
                .unwrap(),
            fixture
                .players
                .get_leaderboard_by_openskill(Some(TEST_GUILD_ID), 2, 0, 0)
                .unwrap(),
        ] {
            assert_eq!(
                leaderboard
                    .iter()
                    .filter_map(|player| player.discord_id)
                    .collect::<Vec<_>>(),
                [5_201, 5_202]
            );
        }
    }

    #[test]
    fn test_steal_atomic_transfers_tracks_lowest_and_can_go_negative() {
        let fixture = Fixture::new();
        fixture.add_player(5_001, TEST_GUILD_ID);
        fixture.add_player(5_002, TEST_GUILD_ID);
        fixture
            .players
            .update_balance(5_001, Some(TEST_GUILD_ID), 0)
            .unwrap();
        fixture
            .players
            .update_balance(5_002, Some(TEST_GUILD_ID), 100)
            .unwrap();
        assert_eq!(
            fixture
                .players
                .steal_atomic(5_001, 5_002, Some(TEST_GUILD_ID), 50)
                .unwrap(),
            StealResult {
                amount: 50,
                thief_new_balance: 50,
                victim_new_balance: 50
            }
        );
        assert_eq!(
            fixture
                .players
                .get_lowest_balance(5_002, Some(TEST_GUILD_ID))
                .unwrap(),
            Some(50)
        );
        fixture
            .players
            .update_balance(5_002, Some(TEST_GUILD_ID), 100)
            .unwrap();
        assert_eq!(
            fixture
                .players
                .steal_atomic(5_001, 5_002, Some(TEST_GUILD_ID), 120)
                .unwrap()
                .victim_new_balance,
            -20
        );
        assert_eq!(
            fixture
                .players
                .get_lowest_balance(5_002, Some(TEST_GUILD_ID))
                .unwrap(),
            Some(-20)
        );
    }

    #[test]
    fn test_record_match() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);
        let summary = fixture
            .matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(summary.team1_players, [1, 2, 3, 4, 5]);
        assert_eq!(summary.team2_players, [6, 7, 8, 9, 10]);
        assert_eq!(summary.winning_team, Value::Integer(1));
    }

    #[test]
    fn test_new_api_requires_both_team_lists() {
        // Rust has one typed API rather than Python's dynamic overload: both
        // team vectors are constructor arguments, so an omitted side is a
        // compile-time error. Exercise the complete shape through SQLite.
        let fixture = Fixture::new();
        let record = MatchRecord::new(vec![43_000, 43_001], vec![43_002], 1, Some(TEST_GUILD_ID));
        let match_id = fixture
            .matches
            .record_match(&record)
            .expect("record typed two-team request");
        let summary = fixture
            .matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .expect("load typed match")
            .expect("typed match exists");
        assert_eq!(summary.team1_players, record.team1_ids);
        assert_eq!(summary.team2_players, record.team2_ids);
    }

    #[test]
    fn test_new_api_rejects_unknown_winning_team() {
        let fixture = Fixture::new();
        let error = fixture
            .matches
            .record_match(&MatchRecord::new(
                vec![43_000],
                vec![43_001],
                3,
                Some(TEST_GUILD_ID),
            ))
            .expect_err("unknown winner must be rejected");
        assert_eq!(
            error.to_string(),
            "winning_team must be 1 (radiant) or 2 (dire)"
        );
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count invalid matches"),
            0
        );
    }

    #[test]
    fn test_old_api_requires_both_team_lists() {
        // The legacy integer-winner call is represented by CoreMatchRecord;
        // it likewise owns two mandatory vectors instead of accepting a
        // partially-shaped dynamic call.
        let fixture = Fixture::new();
        let core = CoreMatchRecord::new(
            MatchRecord::new(vec![43_010], vec![43_011], 2, Some(TEST_GUILD_ID)),
            "2026-08-12T00:00:00+00:00",
        );
        let match_id = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect("record typed legacy-equivalent request");
        assert_eq!(
            fixture
                .matches
                .get_match_participants(match_id, Some(TEST_GUILD_ID))
                .expect("load typed participants")
                .len(),
            2
        );
    }

    #[test]
    fn test_mismatched_team_sizes_currently_not_rejected() {
        let fixture = Fixture::new();
        let radiant = vec![43_000, 43_001, 43_002];
        let dire = (43_003..43_010).collect::<Vec<_>>();
        let match_id = fixture
            .matches
            .record_match(&MatchRecord::new(
                radiant.clone(),
                dire.clone(),
                1,
                Some(TEST_GUILD_ID),
            ))
            .expect("persist current lopsided repository behavior");
        let connection = fixture.connection();
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT
                     SUM(CASE WHEN side='radiant' THEN 1 ELSE 0 END),
                     SUM(CASE WHEN side='dire' THEN 1 ELSE 0 END)
                 FROM match_participants WHERE match_id=?1",
                [match_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("count lopsided participants");
        assert_eq!(counts, (3, 7));
    }

    #[test]
    fn test_empty_team_currently_not_rejected() {
        let fixture = Fixture::new();
        let dire = (43_000..43_005).collect::<Vec<_>>();
        let match_id = fixture
            .matches
            .record_match(&MatchRecord::new(
                Vec::new(),
                dire.clone(),
                2,
                Some(TEST_GUILD_ID),
            ))
            .expect("persist current empty-team repository behavior");
        let summary = fixture
            .matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .expect("load empty-team match")
            .expect("empty-team match exists");
        assert!(summary.team1_players.is_empty());
        assert_eq!(summary.team2_players, dire);
    }

    #[test]
    fn test_balanced_record_writes_side_and_won_flags() {
        let fixture = Fixture::new();
        let radiant = (44_000..44_005).collect::<Vec<_>>();
        let dire = (44_005..44_010).collect::<Vec<_>>();
        let match_id = fixture
            .matches
            .record_match(&MatchRecord::new(
                radiant.clone(),
                dire.clone(),
                1,
                Some(TEST_GUILD_ID),
            ))
            .expect("record balanced match");
        let participants = fixture
            .matches
            .get_match_participants(match_id, Some(TEST_GUILD_ID))
            .expect("load balanced participants");
        assert_eq!(participants.len(), 10);
        for participant in participants {
            if radiant.contains(&participant.discord_id) {
                assert_eq!(participant.team_number, Value::Integer(1));
                assert_eq!(participant.side, Value::Text("radiant".to_owned()));
                assert_eq!(participant.won, Value::Integer(1));
            } else {
                assert!(dire.contains(&participant.discord_id));
                assert_eq!(participant.team_number, Value::Integer(2));
                assert_eq!(participant.side, Value::Text("dire".to_owned()));
                assert_eq!(participant.won, Value::Integer(0));
            }
        }
    }

    fn assert_match_storage_invariants(winning_team: i64) {
        let fixture = Fixture::new();
        let radiant = (46_000..46_005).collect::<Vec<_>>();
        let dire = (46_005..46_010).collect::<Vec<_>>();
        for discord_id in radiant.iter().chain(&dire) {
            fixture.add_player(*discord_id, TEST_GUILD_ID);
        }
        let (winning_ids, losing_ids) = if winning_team == 1 {
            (radiant.clone(), dire.clone())
        } else {
            (dire.clone(), radiant.clone())
        };
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(
                radiant.clone(),
                dire.clone(),
                winning_team,
                Some(TEST_GUILD_ID),
            ),
            "2026-08-12T00:00:00+00:00",
        );
        core.winning_ids = winning_ids.clone();
        core.losing_ids = losing_ids.clone();
        let match_id = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect("record storage invariant fixture");
        let summary = fixture
            .matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .expect("read stored match")
            .expect("stored match exists");
        assert_eq!(summary.team1_players, radiant);
        assert_eq!(summary.team2_players, dire);
        assert_eq!(summary.winning_team, Value::Integer(winning_team));
        let participants = fixture
            .matches
            .get_match_participants(match_id, Some(TEST_GUILD_ID))
            .expect("read stored participants");
        assert_eq!(participants.len(), 10);
        for participant in participants {
            let radiant_side = radiant.contains(&participant.discord_id);
            assert_eq!(
                participant.side,
                Value::Text(if radiant_side { "radiant" } else { "dire" }.to_owned())
            );
            assert_eq!(
                participant.won,
                Value::Integer(i64::from(winning_ids.contains(&participant.discord_id)))
            );
        }
        for discord_id in winning_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .expect("read winner")
                .expect("winner exists");
            assert_eq!((player.wins, player.losses), (1, 0));
        }
        for discord_id in losing_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .expect("read loser")
                .expect("loser exists");
            assert_eq!((player.wins, player.losses), (0, 1));
        }
    }

    #[test]
    fn test_radiant_wins_stores_winning_team_1() {
        assert_match_storage_invariants(1);
    }

    #[test]
    fn test_dire_wins_stores_winning_team_2() {
        assert_match_storage_invariants(2);
    }

    #[test]
    fn test_old_api_team1_wins() {
        assert_match_storage_invariants(1);
    }

    #[test]
    fn test_old_api_team2_wins() {
        assert_match_storage_invariants(2);
    }

    #[test]
    fn test_duplicate_player_across_teams_violates_participant_pk() {
        let fixture = Fixture::new();
        let shared = 45_000;
        let record = MatchRecord::new(
            vec![shared, 45_001, 45_002, 45_003, 45_004],
            vec![shared, 45_005, 45_006, 45_007, 45_008],
            1,
            Some(TEST_GUILD_ID),
        );
        let error = fixture
            .matches
            .record_match(&record)
            .expect_err("participant primary key rejects cross-team duplicate");
        assert!(error.to_string().contains("UNIQUE constraint failed"));
        let connection = fixture.connection();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count rolled-back duplicate match"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM match_participants", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count rolled-back duplicate participants"),
            0
        );
    }

    #[test]
    fn test_recording_requires_atomic_rating_snapshot_capability() {
        let fixture = Fixture::new();
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(vec![42_100], vec![42_101], 1, Some(TEST_GUILD_ID)),
            "2026-08-12T00:00:00+00:00",
        );
        core.expected_openskill_revision = Some(0);
        fixture
            .connection()
            .execute(
                "INSERT INTO openskill_rating_revisions(guild_id,revision,updated_at)
                 VALUES (?1,1,CURRENT_TIMESTAMP)",
                [TEST_GUILD_ID],
            )
            .expect("advance revision after atomic snapshot");

        let error = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect_err("stale atomic rating snapshot must fail closed");
        assert!(error.to_string().contains("OpenSkill ratings changed"));
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count stale-snapshot matches"),
            0
        );
    }

    #[test]
    fn test_get_most_recent_match() {
        let fixture = Fixture::new();
        let _first = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);
        let second = fixture.record(
            &[11, 12, 13, 14, 15],
            &[16, 17, 18, 19, 20],
            2,
            TEST_GUILD_ID,
        );

        assert_eq!(
            fixture
                .matches
                .get_most_recent_match(Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .match_id,
            second
        );
    }

    #[test]
    fn test_empty_when_no_matches() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .matches
                .get_last_match_participant_ids(Some(TEST_GUILD_ID))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_returns_participants_from_last_match() {
        let fixture = Fixture::new();
        let team1 = [1_001, 1_002, 1_003, 1_004, 1_005];
        let team2 = [1_006, 1_007, 1_008, 1_009, 1_010];
        fixture.record(&team1, &team2, 1, TEST_GUILD_ID);

        assert_eq!(
            fixture
                .matches
                .get_last_match_participant_ids(Some(TEST_GUILD_ID))
                .unwrap(),
            team1.into_iter().chain(team2).collect()
        );
    }

    #[test]
    fn test_returns_only_most_recent_match() {
        let fixture = Fixture::new();
        let old_team1 = [101, 102, 103, 104, 105];
        let old_team2 = [106, 107, 108, 109, 110];
        fixture.record(&old_team1, &old_team2, 1, TEST_GUILD_ID);
        let new_team1 = [201, 202, 203, 204, 205];
        let new_team2 = [206, 207, 208, 209, 210];
        fixture.record(&new_team1, &new_team2, 2, TEST_GUILD_ID);

        let participants = fixture
            .matches
            .get_last_match_participant_ids(Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            participants,
            new_team1.into_iter().chain(new_team2).collect()
        );
        assert!(participants.is_disjoint(&old_team1.into_iter().chain(old_team2).collect()));
    }

    #[test]
    fn test_update_match_enrichment() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);

        fixture
            .matches
            .update_match_enrichment(
                match_id,
                8_181_518_332,
                2_400,
                35,
                22,
                2,
                Some(r#"{"test":"data"}"#),
            )
            .unwrap();

        assert_eq!(
            fixture
                .matches
                .get_most_recent_match(Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .valve_match_id,
            Some(8_181_518_332)
        );
    }

    #[test]
    fn test_update_participant_stats() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[100], &[200], 1, TEST_GUILD_ID);

        fixture
            .matches
            .update_participant_stats(
                match_id,
                100,
                &ParticipantStatsUpdate {
                    hero_id: 1,
                    kills: 10,
                    deaths: 2,
                    assists: 5,
                    gpm: 600,
                    xpm: 550,
                    hero_damage: 25_000,
                    tower_damage: 3_000,
                    last_hits: 200,
                    denies: 10,
                    net_worth: 20_000,
                    hero_healing: 0,
                    lane_role: None,
                    lane_efficiency: None,
                },
            )
            .unwrap();

        let participant = fixture
            .matches
            .get_match_participants(match_id, Some(TEST_GUILD_ID))
            .unwrap()
            .into_iter()
            .find(|participant| participant.discord_id == 100)
            .unwrap();
        assert_eq!(participant.hero_id, Value::Integer(1));
        assert_eq!(participant.kills, Value::Integer(10));
        assert_eq!(participant.deaths, Value::Integer(2));
        assert_eq!(participant.assists, Value::Integer(5));
        assert_eq!(participant.gpm, Value::Integer(600));
    }

    #[test]
    fn test_get_matches_without_enrichment() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);

        let unenriched = fixture
            .matches
            .get_matches_without_enrichment(Some(TEST_GUILD_ID), 10)
            .unwrap();
        assert_eq!(unenriched.len(), 1);
        assert_eq!(unenriched[0].match_id, match_id);

        assert!(
            fixture
                .matches
                .set_valve_match_id(match_id, 8_181_518_332)
                .unwrap()
        );
        assert!(
            fixture
                .matches
                .get_matches_without_enrichment(Some(TEST_GUILD_ID), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_matches_without_fantasy_data_scoped_to_guild() {
        let fixture = Fixture::new();
        let other_match_id = fixture.record(
            &[1, 2, 3, 4, 5],
            &[6, 7, 8, 9, 10],
            1,
            TEST_GUILD_ID_SECONDARY,
        );
        fixture
            .matches
            .set_valve_match_id(other_match_id, 8_181_518_332)
            .unwrap();

        assert!(
            fixture
                .matches
                .get_matches_without_fantasy_data(Some(TEST_GUILD_ID), 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .matches
                .get_matches_without_fantasy_data(Some(TEST_GUILD_ID_SECONDARY), 100)
                .unwrap()
                .iter()
                .map(|row| row.match_id)
                .collect::<Vec<_>>(),
            [other_match_id]
        );
    }

    #[test]
    fn test_get_enrichment_data_returns_parsed_json() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);
        fixture
            .matches
            .update_match_enrichment(
                match_id,
                8_181_518_332,
                2_400,
                35,
                22,
                2,
                Some(
                    r#"{"radiant_gold_adv":[0,500,1200,2000],"radiant_xp_adv":[0,200,800,1500],"duration":2400}"#,
                ),
            )
            .unwrap();

        let Some(EnrichmentJsonValue::Object(data)) = fixture
            .matches
            .get_enrichment_data(match_id, Some(TEST_GUILD_ID))
            .unwrap()
        else {
            panic!("expected parsed enrichment object");
        };
        assert_eq!(
            data.get("radiant_gold_adv"),
            Some(&EnrichmentJsonValue::Array(vec![
                EnrichmentJsonValue::Integer(0),
                EnrichmentJsonValue::Integer(500),
                EnrichmentJsonValue::Integer(1_200),
                EnrichmentJsonValue::Integer(2_000),
            ]))
        );
        assert_eq!(
            data.get("radiant_xp_adv"),
            Some(&EnrichmentJsonValue::Array(vec![
                EnrichmentJsonValue::Integer(0),
                EnrichmentJsonValue::Integer(200),
                EnrichmentJsonValue::Integer(800),
                EnrichmentJsonValue::Integer(1_500),
            ]))
        );
        assert_eq!(
            data.get("duration"),
            Some(&EnrichmentJsonValue::Integer(2_400))
        );
    }

    #[test]
    fn test_get_enrichment_data_returns_none_when_not_enriched() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);

        assert_eq!(
            fixture
                .matches
                .get_enrichment_data(match_id, Some(TEST_GUILD_ID))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_get_match_participants_bulk_matches_point_reads_and_input_order() {
        let fixture = Fixture::new();
        let first = fixture.record(&[1, 2], &[3, 4], 1, TEST_GUILD_ID);
        let second = fixture.record(&[5, 6], &[7, 8], 2, TEST_GUILD_ID);
        let other = fixture.record(&[9], &[10], 1, TEST_GUILD_ID_SECONDARY);
        let participants = fixture
            .matches
            .get_match_participants_bulk(
                &[second, 999_999, first, other, second],
                Some(TEST_GUILD_ID),
            )
            .unwrap();
        assert_eq!(
            participants.keys().copied().collect::<Vec<_>>(),
            [second, 999_999, first, other]
        );
        assert_eq!(
            participants.get(&second).unwrap(),
            &fixture
                .matches
                .get_match_participants(second, Some(TEST_GUILD_ID))
                .unwrap()
        );
        assert_eq!(
            participants.get(&first).unwrap(),
            &fixture
                .matches
                .get_match_participants(first, Some(TEST_GUILD_ID))
                .unwrap()
        );
        assert!(participants.get(&999_999).unwrap().is_empty());
        assert!(participants.get(&other).unwrap().is_empty());
    }

    #[test]
    fn test_get_match_participants_bulk_chunks_on_one_connection() {
        let fixture = Fixture::new();
        let match_ids = (1..1_002).collect::<Vec<_>>();
        let participants = fixture
            .matches
            .get_match_participants_bulk(&match_ids, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(participants.keys().copied().collect::<Vec<_>>(), match_ids);
        assert!(participants.values().all(Vec::is_empty));
        assert!(
            fixture
                .matches
                .get_match_participants_bulk(&[], None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_player_matches() {
        let fixture = Fixture::new();
        let first = fixture.record(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10], 1, TEST_GUILD_ID);
        let second = fixture.record(&[1, 6, 3, 8, 5], &[2, 7, 4, 9, 10], 2, TEST_GUILD_ID);
        fixture
            .matches
            .update_participant_stats(
                second,
                1,
                &ParticipantStatsUpdate {
                    hero_id: 42,
                    kills: 8,
                    deaths: 2,
                    assists: 11,
                    gpm: 612,
                    xpm: 700,
                    hero_damage: 25_000,
                    tower_damage: 3_000,
                    last_hits: 250,
                    denies: 12,
                    net_worth: 19_000,
                    hero_healing: 400,
                    lane_role: Some(2),
                    lane_efficiency: Some(61),
                },
            )
            .unwrap();
        let matches = fixture
            .matches
            .get_player_matches(1, Some(TEST_GUILD_ID), 10)
            .unwrap();
        assert_eq!(
            matches
                .iter()
                .map(|row| row.match_id)
                .collect::<HashSet<_>>(),
            HashSet::from([first, second])
        );
        let enriched = matches.iter().find(|row| row.match_id == second).unwrap();
        assert_eq!(enriched.hero_id, Value::Integer(42));
        assert_eq!(enriched.kills, Value::Integer(8));
        assert_eq!(enriched.hero_damage, Value::Integer(25_000));
        assert_eq!(enriched.participants.len(), 10);
    }

    #[test]
    fn test_get_player_recent_outcomes_bulk_matches_point_reads() {
        let fixture = Fixture::new();
        for (discord_id, outcomes) in [
            (101, vec![true, false, true, true]),
            (202, vec![false, false, true]),
        ] {
            for won in outcomes {
                fixture.add_history(discord_id, TEST_GUILD_ID, None, Some(won));
            }
        }
        fixture.add_history(101, TEST_GUILD_ID_SECONDARY, None, Some(false));
        let bulk = fixture
            .matches
            .get_player_recent_outcomes_bulk(&[202, 101, 303, 202], Some(TEST_GUILD_ID), 3)
            .unwrap();
        assert_eq!(bulk.keys().copied().collect::<Vec<_>>(), [202, 101, 303]);
        assert_eq!(
            bulk.get(&101).unwrap(),
            &fixture
                .matches
                .get_player_recent_outcomes(101, Some(TEST_GUILD_ID), 3)
                .unwrap()
        );
        assert_eq!(
            bulk.get(&202).unwrap(),
            &fixture
                .matches
                .get_player_recent_outcomes(202, Some(TEST_GUILD_ID), 3)
                .unwrap()
        );
        assert!(bulk.get(&303).unwrap().is_empty());
    }

    #[test]
    fn test_get_player_outcomes_before_match_bulk_is_capped_and_single_query() {
        let fixture = Fixture::new();
        for won in [true, false, true, true] {
            fixture.add_history(101, TEST_GUILD_ID, None, Some(won));
        }
        for won in [false, true] {
            fixture.add_history(202, TEST_GUILD_ID, None, Some(won));
        }
        fixture.add_history(101, TEST_GUILD_ID_SECONDARY, None, Some(false));
        let target = fixture.record(&[101], &[202], 1, TEST_GUILD_ID);
        fixture.add_history(101, TEST_GUILD_ID, Some(target), Some(true));
        fixture.add_history(202, TEST_GUILD_ID, Some(target), Some(false));
        fixture.add_history(101, TEST_GUILD_ID, None, Some(false));
        fixture.add_history(202, TEST_GUILD_ID, None, Some(true));
        let outcomes = fixture
            .matches
            .get_player_outcomes_before_match_bulk(
                &[202, 101, 303, 202],
                Some(TEST_GUILD_ID),
                target,
                3,
            )
            .unwrap();
        assert_eq!(
            outcomes.keys().copied().collect::<Vec<_>>(),
            [202, 101, 303]
        );
        assert_eq!(outcomes.get(&202).unwrap(), &[true, false]);
        assert_eq!(outcomes.get(&101).unwrap(), &[true, true, false]);
        assert!(outcomes.get(&303).unwrap().is_empty());
    }

    #[test]
    fn test_get_os_ratings_for_matches_bulk_and_guild_scoped() {
        let fixture = Fixture::new();
        let match_ids = [
            fixture.record(&[1, 2], &[6, 7], 1, TEST_GUILD_ID),
            fixture.record(&[1, 2], &[6, 7], 1, TEST_GUILD_ID),
        ];
        for match_id in match_ids {
            for (discord_id, team_number) in [(1, 1), (2, 1), (6, 2), (7, 2)] {
                fixture
                    .matches
                    .add_rating_history(
                        Some(TEST_GUILD_ID),
                        &NewCoreRatingHistory {
                            discord_id,
                            rating: Some(1_500.0),
                            match_id: Some(match_id),
                            team_number: Some(team_number),
                            os_mu_before: Some(discord_id as f64),
                            os_sigma_before: Some(5.0),
                            ..NewCoreRatingHistory::default()
                        },
                    )
                    .unwrap();
            }
        }
        fixture
            .matches
            .add_rating_history(
                Some(TEST_GUILD_ID_SECONDARY),
                &NewCoreRatingHistory {
                    discord_id: 99,
                    rating: Some(1_500.0),
                    match_id: Some(match_ids[0]),
                    team_number: Some(1),
                    os_mu_before: Some(99.0),
                    os_sigma_before: Some(5.0),
                    ..NewCoreRatingHistory::default()
                },
            )
            .unwrap();
        let ratings = fixture
            .matches
            .get_os_ratings_for_matches(
                &[match_ids[1], match_ids[0], 999_999, match_ids[1]],
                Some(TEST_GUILD_ID),
            )
            .unwrap();
        assert_eq!(
            ratings.keys().copied().collect::<Vec<_>>(),
            [match_ids[1], match_ids[0], 999_999]
        );
        for match_id in match_ids {
            let mut team1 = ratings.get(&match_id).unwrap().team1.clone();
            team1.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut team2 = ratings.get(&match_id).unwrap().team2.clone();
            team2.sort_by(|a, b| a.0.total_cmp(&b.0));
            assert_eq!(team1, [(1.0, 5.0), (2.0, 5.0)]);
            assert_eq!(team2, [(6.0, 5.0), (7.0, 5.0)]);
        }
        assert_eq!(
            ratings.get(&999_999),
            Some(&TeamOpenSkillRatings::default())
        );
    }

    #[test]
    fn test_get_os_ratings_for_matches_chunks_on_one_connection() {
        let fixture = Fixture::new();
        let match_ids = (1..1_002).collect::<Vec<_>>();
        let ratings = fixture
            .matches
            .get_os_ratings_for_matches(&match_ids, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(ratings.len(), match_ids.len());
        assert!(
            ratings
                .values()
                .all(|value| value == &TeamOpenSkillRatings::default())
        );
    }

    #[test]
    fn test_match_summary_reads_do_not_load_enrichment_data() {
        let fixture = Fixture::new();
        let mut record = MatchRecord::new(
            (1..=5).collect(),
            (6..=10).collect(),
            1,
            Some(TEST_GUILD_ID),
        );
        record.dotabuff_match_id = Some("dotabuff-123".to_owned());
        record.notes = Some("summary contract".to_owned());
        record.lobby_type = "draft".to_owned();
        record.balancing_rating_system = "openskill".to_owned();
        let match_id = fixture.matches.record_match(&record).unwrap();
        let enrichment = "x".repeat(1_000_000);
        fixture
            .matches
            .update_match_enrichment(match_id, 8_181_518_332, 2_400, 35, 22, 2, Some(&enrichment))
            .unwrap();
        let summary = fixture
            .matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(summary.dotabuff_match_id.as_deref(), Some("dotabuff-123"));
        assert_eq!(summary.valve_match_id, Some(8_181_518_332));
        assert_eq!(summary.lobby_type.as_deref(), Some("draft"));
        assert_eq!(summary.jc_changes, BTreeMap::new());
        let player_matches = fixture
            .matches
            .get_player_matches(1, Some(TEST_GUILD_ID), 10)
            .unwrap();
        assert_eq!(player_matches[0].participants.len(), 10);
        assert_eq!(player_matches[0].hero_id, Value::Null);
        assert_eq!(
            fixture
                .matches
                .get_most_recent_match(Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap()
                .match_id,
            match_id
        );
    }

    #[test]
    fn test_get_lobby_type_stats_shuffle_only() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .matches
                .get_lobby_type_stats(Some(TEST_GUILD_ID))
                .unwrap()
                .is_empty()
        );
        let match_id = fixture.record(&[1], &[6], 1, TEST_GUILD_ID);
        for entry in [
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_520.0),
                rating_before: Some(1_500.0),
                match_id: Some(match_id),
                expected_team_win_prob: Some(0.55),
                won: Some(true),
                os_mu_before: Some(40.0),
                os_mu_after: Some(40.4),
                ..NewCoreRatingHistory::default()
            },
            NewCoreRatingHistory {
                discord_id: 6,
                rating: Some(1_480.0),
                rating_before: Some(1_500.0),
                match_id: Some(match_id),
                expected_team_win_prob: Some(0.45),
                won: Some(false),
                os_mu_before: Some(40.0),
                os_mu_after: Some(39.6),
                ..NewCoreRatingHistory::default()
            },
        ] {
            fixture
                .matches
                .add_rating_history(Some(TEST_GUILD_ID), &entry)
                .unwrap();
        }
        let stats = fixture
            .matches
            .get_lobby_type_stats(Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].lobby_type, "shuffle");
        assert_eq!(stats[0].games, 1);
        assert_eq!(stats[0].avg_swing, Some(20.0));
        assert_approx(stats[0].openskill_avg_swing.unwrap(), 20.0);
        assert_eq!(stats[0].actual_win_rate, Some(1.0));
        assert_eq!(stats[0].expected_win_rate, None);
    }

    #[test]
    fn test_get_lobby_type_stats_both_types() {
        let fixture = Fixture::new();
        let shuffle = fixture.record(&[1], &[2], 1, TEST_GUILD_ID);
        let mut draft_record = MatchRecord::new(vec![1], vec![2], 2, Some(TEST_GUILD_ID));
        draft_record.lobby_type = "draft".to_owned();
        let draft = fixture.matches.record_match(&draft_record).unwrap();
        for entry in [
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_520.0),
                rating_before: Some(1_500.0),
                match_id: Some(shuffle),
                won: Some(true),
                os_mu_before: Some(40.0),
                os_mu_after: Some(40.2),
                ..NewCoreRatingHistory::default()
            },
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_470.0),
                rating_before: Some(1_520.0),
                match_id: Some(draft),
                won: Some(false),
                os_mu_before: Some(40.2),
                os_mu_after: Some(39.6),
                ..NewCoreRatingHistory::default()
            },
        ] {
            fixture
                .matches
                .add_rating_history(Some(TEST_GUILD_ID), &entry)
                .unwrap();
        }
        let stats = fixture
            .matches
            .get_lobby_type_stats(Some(TEST_GUILD_ID))
            .unwrap();
        let shuffle_stats = stats
            .iter()
            .find(|row| row.lobby_type == "shuffle")
            .unwrap();
        let draft_stats = stats.iter().find(|row| row.lobby_type == "draft").unwrap();
        assert_eq!(shuffle_stats.avg_swing, Some(20.0));
        assert_approx(shuffle_stats.openskill_avg_swing.unwrap(), 10.0);
        assert_eq!(draft_stats.avg_swing, Some(50.0));
        assert_approx(draft_stats.openskill_avg_swing.unwrap(), 30.0);
    }

    #[test]
    fn test_lobby_type_stats_report_predictions_from_the_correct_side() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1], &[6], 1, TEST_GUILD_ID);
        fixture
            .matches
            .add_rating_history(
                Some(TEST_GUILD_ID),
                &NewCoreRatingHistory {
                    discord_id: 6,
                    rating: Some(1_490.0),
                    rating_before: Some(1_500.0),
                    expected_team_win_prob: Some(0.30),
                    team_number: Some(2),
                    won: Some(false),
                    match_id: Some(match_id),
                    os_mu_before: Some(40.0),
                    os_mu_after: Some(39.8),
                    ..NewCoreRatingHistory::default()
                },
            )
            .unwrap();
        fixture.connection().execute("INSERT INTO match_predictions(match_id, expected_radiant_win_prob, openskill_radiant_win_prob) VALUES(?1,0.70,0.65)", [match_id]).unwrap();
        let server = &fixture
            .matches
            .get_lobby_type_stats(Some(TEST_GUILD_ID))
            .unwrap()[0];
        assert_eq!(server.actual_win_rate, Some(1.0));
        assert_eq!(server.expected_win_rate, Some(0.70));
        assert_eq!(server.openskill_expected_win_rate, Some(0.65));
        let player = &fixture
            .matches
            .get_player_lobby_type_stats(6, Some(TEST_GUILD_ID))
            .unwrap()[0];
        assert_eq!(player.actual_win_rate, Some(0.0));
        assert_eq!(player.expected_win_rate, Some(0.30));
        assert_approx(player.openskill_expected_win_rate.unwrap(), 0.35);
    }

    #[test]
    fn test_get_player_lobby_type_stats_filters_by_player() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .matches
                .get_player_lobby_type_stats(999, Some(TEST_GUILD_ID))
                .unwrap()
                .is_empty()
        );
        let match_id = fixture.record(&[1], &[6], 1, TEST_GUILD_ID);
        for entry in [
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_530.0),
                rating_before: Some(1_500.0),
                match_id: Some(match_id),
                won: Some(true),
                os_mu_before: Some(40.0),
                os_mu_after: Some(40.6),
                ..NewCoreRatingHistory::default()
            },
            NewCoreRatingHistory {
                discord_id: 6,
                rating: Some(1_490.0),
                rating_before: Some(1_500.0),
                match_id: Some(match_id),
                won: Some(false),
                ..NewCoreRatingHistory::default()
            },
        ] {
            fixture
                .matches
                .add_rating_history(Some(TEST_GUILD_ID), &entry)
                .unwrap();
        }
        let p1 = &fixture
            .matches
            .get_player_lobby_type_stats(1, Some(TEST_GUILD_ID))
            .unwrap()[0];
        let p6 = &fixture
            .matches
            .get_player_lobby_type_stats(6, Some(TEST_GUILD_ID))
            .unwrap()[0];
        assert_eq!(p1.avg_swing, Some(30.0));
        assert_eq!(p1.actual_win_rate, Some(1.0));
        assert_eq!(p6.avg_swing, Some(10.0));
        assert_eq!(p6.actual_win_rate, Some(0.0));
    }

    #[test]
    fn test_get_player_lobby_type_stats_includes_openskill_only_history() {
        let fixture = Fixture::new();
        let match_id = fixture.record(&[1], &[6], 1, TEST_GUILD_ID);
        fixture
            .matches
            .add_rating_history(
                Some(TEST_GUILD_ID),
                &NewCoreRatingHistory {
                    discord_id: 1,
                    rating: None,
                    match_id: Some(match_id),
                    won: Some(true),
                    team_number: Some(1),
                    os_mu_before: Some(40.0),
                    os_mu_after: Some(40.5),
                    ..NewCoreRatingHistory::default()
                },
            )
            .unwrap();
        let stats = &fixture
            .matches
            .get_player_lobby_type_stats(1, Some(TEST_GUILD_ID))
            .unwrap()[0];
        assert_eq!(stats.avg_swing, None);
        assert_eq!(stats.glicko_samples, 0);
        assert_approx(stats.openskill_avg_swing.unwrap(), 25.0);
        assert_eq!(stats.openskill_samples, 1);
    }

    #[test]
    fn test_get_player_lobby_type_stats_both_types() {
        let fixture = Fixture::new();
        let shuffle = fixture.record(&[1], &[6], 1, TEST_GUILD_ID);
        let mut draft_record = MatchRecord::new(vec![1], vec![6], 1, Some(TEST_GUILD_ID));
        draft_record.lobby_type = "draft".to_owned();
        let draft = fixture.matches.record_match(&draft_record).unwrap();
        for entry in [
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_520.0),
                rating_before: Some(1_500.0),
                match_id: Some(shuffle),
                won: Some(true),
                ..NewCoreRatingHistory::default()
            },
            NewCoreRatingHistory {
                discord_id: 1,
                rating: Some(1_560.0),
                rating_before: Some(1_520.0),
                match_id: Some(draft),
                won: Some(true),
                ..NewCoreRatingHistory::default()
            },
        ] {
            fixture
                .matches
                .add_rating_history(Some(TEST_GUILD_ID), &entry)
                .unwrap();
        }
        let stats = fixture
            .matches
            .get_player_lobby_type_stats(1, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            stats
                .iter()
                .find(|row| row.lobby_type == "shuffle")
                .unwrap()
                .avg_swing,
            Some(20.0)
        );
        assert_eq!(
            stats
                .iter()
                .find(|row| row.lobby_type == "draft")
                .unwrap()
                .avg_swing,
            Some(40.0)
        );
    }

    #[test]
    fn test_hero_pairwise_batch_matches_legacy_views() {
        let fixture = Fixture::new();
        let mut connection = fixture.connection();
        let transaction = connection
            .transaction()
            .expect("seed hero pairwise fixture");
        for index in 0..36_i64 {
            for repeat in 0..2_i64 {
                let match_id = 10_000 + index * 2 + repeat;
                transaction.execute("INSERT INTO matches(match_id,team1_players,team2_players,winning_team,guild_id) VALUES(?1,'[1, 2]','[3]',1,?2)", params![match_id, TEST_GUILD_ID]).unwrap();
                transaction.execute("INSERT INTO match_participants(match_id,discord_id,guild_id,team_number,won,hero_id) VALUES(?1,1,?2,1,?3,?4)", params![match_id, TEST_GUILD_ID, repeat, 10 + index / 12]).unwrap();
                transaction.execute("INSERT INTO match_participants(match_id,discord_id,guild_id,team_number,won,hero_id) VALUES(?1,2,?2,1,?3,?4)", params![match_id, TEST_GUILD_ID, repeat, 200 + index % 3]).unwrap();
                transaction.execute("INSERT INTO match_participants(match_id,discord_id,guild_id,team_number,won,hero_id) VALUES(?1,3,?2,2,?3,?4)", params![match_id, TEST_GUILD_ID, 1-repeat, 100 + index % 12]).unwrap();
            }
        }
        transaction.commit().expect("commit hero pairwise fixture");
        let stats = fixture
            .matches
            .get_player_hero_pairwise_stats(1, Some(TEST_GUILD_ID), 2)
            .unwrap();
        assert_eq!(stats.nemesis.len(), 10);
        assert_eq!(stats.easy.len(), 10);
        assert_eq!(stats.synergies.len(), 3);
        assert_eq!(stats.hero_vs_hero.len(), 30);
        assert_eq!(
            fixture
                .matches
                .get_player_hero_pairwise_stats(1, Some(999_999), 2)
                .unwrap(),
            HeroPairwiseStats::default()
        );
    }

    fn insert_either_suspension(fixture: &Fixture, discord_id: i64, expires_at: i64) {
        let now = unix_now();
        fixture.connection().execute(
            "INSERT INTO lobby_suspensions(guild_id,discord_id,scope,completion,expires_at,matches_required,matches_remaining,pending_match_watermark,reason,source,actor_id,active,created_at,updated_at)
             VALUES(?1,?2,'all','either',?3,1,1,0,'test','admin',900,1,?4,?4)",
            params![TEST_GUILD_ID, discord_id, expires_at, now],
        ).unwrap();
    }

    fn core_record(fixture: &Fixture, pending_match_id: i64) -> i64 {
        let mut record = MatchRecord::new(vec![], vec![], 1, Some(TEST_GUILD_ID));
        record.lobby_kind = Some("open".to_owned());
        let mut core = CoreMatchRecord::new(record, "2026-08-05T00:00:00+00:00");
        core.pending_match_id = Some(pending_match_id);
        fixture.matches.record_match_core_atomic(&core).unwrap()
    }

    fn seed_low_priority(
        fixture: &Fixture,
        discord_id: i64,
        guild_id: i64,
        wins_required: i64,
        start_pending_match_id: Option<i64>,
    ) {
        fixture
            .connection()
            .execute(
                "INSERT INTO low_priority_state (
                    discord_id,guild_id,wins_required,wins_remaining,
                    start_pending_match_id,active,set_by
                 ) VALUES (?1,?2,?3,?3,?4,1,901)",
                params![discord_id, guild_id, wins_required, start_pending_match_id],
            )
            .expect("seed low-priority state");
    }

    fn record_low_priority_win(
        fixture: &Fixture,
        winner_id: i64,
        loser_id: i64,
        guild_id: i64,
        pending_match_id: Option<i64>,
    ) -> i64 {
        let mut match_record = MatchRecord::new(vec![winner_id], vec![loser_id], 1, Some(guild_id));
        match_record.lobby_kind = Some("open".to_owned());
        let mut core = CoreMatchRecord::new(match_record, "2026-08-12T00:00:00+00:00");
        core.pending_match_id = pending_match_id;
        core.winning_ids = vec![winner_id];
        core.losing_ids = vec![loser_id];
        fixture
            .matches
            .record_match_core_atomic(&core)
            .expect("record low-priority win")
    }

    #[test]
    fn test_exactly_three_recorded_wins_release_only_the_winner() {
        let fixture = Fixture::new();
        let winner_id = 101;
        let loser_id = 102;
        for guild_id in [TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY] {
            fixture.add_player(winner_id, guild_id);
            fixture.add_player(loser_id, guild_id);
        }
        seed_low_priority(&fixture, winner_id, TEST_GUILD_ID, 3, None);
        seed_low_priority(&fixture, loser_id, TEST_GUILD_ID, 3, None);
        seed_low_priority(&fixture, winner_id, TEST_GUILD_ID_SECONDARY, 3, None);

        for expected_remaining in [2, 1] {
            record_low_priority_win(&fixture, winner_id, loser_id, TEST_GUILD_ID, None);
            let state: (i64, i64) = fixture
                .connection()
                .query_row(
                    "SELECT wins_remaining,active FROM low_priority_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![winner_id, TEST_GUILD_ID],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("read progressing low-priority state");
            assert_eq!(state, (expected_remaining, 1));
        }
        record_low_priority_win(&fixture, winner_id, loser_id, TEST_GUILD_ID, None);

        let connection = fixture.connection();
        let released: (i64, i64) = connection
            .query_row(
                "SELECT wins_remaining,active FROM low_priority_state
                 WHERE discord_id=?1 AND guild_id=?2",
                params![winner_id, TEST_GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read released state");
        assert_eq!(released, (0, 0));
        assert_eq!(
            connection
                .query_row(
                    "SELECT wins_remaining FROM low_priority_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![loser_id, TEST_GUILD_ID],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read losing state"),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT wins_remaining FROM low_priority_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![winner_id, TEST_GUILD_ID_SECONDARY],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read other-guild state"),
            3
        );
    }

    #[test]
    fn test_retrying_same_pending_match_does_not_double_count_win() {
        let fixture = Fixture::new();
        let winner_id = 201;
        let loser_id = 202;
        fixture.add_player(winner_id, TEST_GUILD_ID);
        fixture.add_player(loser_id, TEST_GUILD_ID);
        seed_low_priority(&fixture, winner_id, TEST_GUILD_ID, 3, None);
        let pending_match_id = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .expect("save pending match");

        let first = record_low_priority_win(
            &fixture,
            winner_id,
            loser_id,
            TEST_GUILD_ID,
            Some(pending_match_id),
        );
        let second = record_low_priority_win(
            &fixture,
            winner_id,
            loser_id,
            TEST_GUILD_ID,
            Some(pending_match_id),
        );
        assert_eq!(second, first);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT wins_remaining FROM low_priority_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![winner_id, TEST_GUILD_ID],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read idempotent state"),
            2
        );
    }

    #[test]
    fn test_assignment_ignores_a_match_that_was_already_pending() {
        let fixture = Fixture::new();
        let winner_id = 301;
        let loser_id = 302;
        fixture.add_player(winner_id, TEST_GUILD_ID);
        fixture.add_player(loser_id, TEST_GUILD_ID);
        let existing_pending = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .expect("save pre-assignment match");
        seed_low_priority(
            &fixture,
            winner_id,
            TEST_GUILD_ID,
            2,
            Some(existing_pending),
        );

        record_low_priority_win(
            &fixture,
            winner_id,
            loser_id,
            TEST_GUILD_ID,
            Some(existing_pending),
        );
        let remaining = || {
            fixture
                .connection()
                .query_row(
                    "SELECT wins_remaining FROM low_priority_state
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![winner_id, TEST_GUILD_ID],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read watermarked state")
        };
        assert_eq!(remaining(), 2);

        let future_pending = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .expect("save future match");
        record_low_priority_win(
            &fixture,
            winner_id,
            loser_id,
            TEST_GUILD_ID,
            Some(future_pending),
        );
        assert_eq!(remaining(), 1);
    }

    #[test]
    fn test_automatic_completion_is_audited_once_with_match_id() {
        let fixture = Fixture::new();
        let winner_id = 401;
        let loser_id = 402;
        fixture.add_player(winner_id, TEST_GUILD_ID);
        fixture.add_player(loser_id, TEST_GUILD_ID);
        let watermark = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .expect("save assignment watermark");
        seed_low_priority(&fixture, winner_id, TEST_GUILD_ID, 1, Some(watermark));
        let pending_match_id = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .expect("save completing match");

        let match_id = record_low_priority_win(
            &fixture,
            winner_id,
            loser_id,
            TEST_GUILD_ID,
            Some(pending_match_id),
        );
        assert_eq!(
            record_low_priority_win(
                &fixture,
                winner_id,
                loser_id,
                TEST_GUILD_ID,
                Some(pending_match_id),
            ),
            match_id
        );
        let connection = fixture.connection();
        let event: (i64, String, String, String, Option<i64>, i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),event_type,source,reason,pending_match_watermark,
                        wins_required,wins_remaining,match_id
                 FROM moderation_events
                 WHERE guild_id=?1 AND discord_id=?2",
                params![TEST_GUILD_ID, winner_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .expect("read completion audit");
        assert_eq!(
            event,
            (
                1,
                "lowprio_complete".to_owned(),
                "system".to_owned(),
                "Required wins completed".to_owned(),
                Some(watermark),
                1,
                0,
                match_id,
            )
        );
    }

    #[test]
    fn test_recording_rejects_low_priority_state_changed_after_rating_snapshot() {
        let fixture = Fixture::new();
        let winner_id = 601;
        let loser_id = 602;
        fixture.add_player(winner_id, TEST_GUILD_ID);
        fixture.add_player(loser_id, TEST_GUILD_ID);
        seed_low_priority(&fixture, winner_id, TEST_GUILD_ID, 1, None);

        let mut record = MatchRecord::new(vec![winner_id], vec![loser_id], 1, Some(TEST_GUILD_ID));
        record.lobby_kind = Some("open".to_owned());
        let mut core = CoreMatchRecord::new(record, "2026-08-12T00:00:00+00:00");
        core.winning_ids = vec![winner_id];
        core.losing_ids = vec![loser_id];
        core.expected_low_priority_ids = Some(HashSet::from([winner_id]));
        fixture
            .connection()
            .execute(
                "UPDATE low_priority_state SET active=0,wins_remaining=0
                 WHERE guild_id=?1 AND discord_id=?2",
                params![TEST_GUILD_ID, winner_id],
            )
            .expect("simulate concurrent admin clear");

        let error = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect_err("stale low-priority calculation must fail");
        assert_eq!(
            error.to_string(),
            "Low-priority state changed while this match was being calculated; retry recording the match"
        );
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count rolled-back matches"),
            0
        );
    }

    #[test]
    fn core_match_pairings_commit_atomically_and_retry_exactly_once() {
        let fixture = Fixture::new();
        let radiant = (94_000..94_005).collect::<Vec<_>>();
        let dire = (94_005..94_010).collect::<Vec<_>>();
        for discord_id in radiant.iter().chain(&dire) {
            fixture.add_player(*discord_id, TEST_GUILD_ID);
        }
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(radiant.clone(), dire.clone(), 1, Some(TEST_GUILD_ID)),
            "2026-08-05T00:00:00+00:00",
        );
        core.pending_match_id = Some(94_001);
        let match_id = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect("record pairing match");
        assert_eq!(
            fixture
                .matches
                .record_match_core_atomic(&core)
                .expect("retry pairing match"),
            match_id
        );
        let connection = fixture.connection();
        let totals: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),COALESCE(SUM(games_together),0),
                        COALESCE(SUM(games_against),0),
                        COALESCE(SUM(player1_wins_against),0)
                 FROM player_pairings WHERE guild_id=?1",
                [TEST_GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read pairings");
        assert_eq!(totals, (45, 20, 25, 25));
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM player_pairings
                     WHERE guild_id=?1 AND last_match_id=?2",
                    params![TEST_GUILD_ID, match_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read attributed pairings"),
            45
        );
    }

    #[test]
    fn test_time_lapsed_either_suspension_is_not_decremented_or_completed() {
        let fixture = Fixture::new();
        insert_either_suspension(&fixture, 424_242, unix_now() - 3_600);
        let pending = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .unwrap();
        let match_id = core_record(&fixture, pending);
        let connection = fixture.connection();
        let state: (i64, i64) = connection.query_row("SELECT matches_remaining,active FROM lobby_suspensions WHERE guild_id=?1 AND discord_id=424242", [TEST_GUILD_ID], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(state, (1, 1));
        let events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM moderation_events WHERE guild_id=?1 AND match_id=?2",
                params![TEST_GUILD_ID, match_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 0);
    }

    #[test]
    fn test_unexpired_either_suspension_still_progresses() {
        let fixture = Fixture::new();
        insert_either_suspension(&fixture, 424_243, unix_now() + 3_600);
        let pending = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .unwrap();
        let match_id = core_record(&fixture, pending);
        let connection = fixture.connection();
        let state: (i64, i64) = connection.query_row("SELECT matches_remaining,active FROM lobby_suspensions WHERE guild_id=?1 AND discord_id=424243", [TEST_GUILD_ID], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(state, (0, 0));
        let event: (String, i64) = connection.query_row("SELECT event_type,discord_id FROM moderation_events WHERE guild_id=?1 AND match_id=?2", params![TEST_GUILD_ID, match_id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(event, ("complete".to_owned(), 424_243));
    }

    fn record_with_streak_row(
        fixture: &Fixture,
        defaults: StreakInsertDefaults,
        supplied_multiplier: Option<f64>,
        supplied_threshold: Option<i64>,
    ) -> i64 {
        let pending = fixture
            .matches
            .save_pending_match(Some(TEST_GUILD_ID), "{}")
            .unwrap();
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(vec![111], vec![222], 1, Some(TEST_GUILD_ID)),
            "2026-08-05T00:00:00+00:00",
        );
        core.pending_match_id = Some(pending);
        core.streak_defaults = defaults;
        core.rating_history_rows.push(NewCoreRatingHistory {
            discord_id: 111,
            rating: Some(1_520.0),
            streak_multiplier_per_game: supplied_multiplier,
            streak_threshold: supplied_threshold,
            ..NewCoreRatingHistory::default()
        });
        fixture.matches.record_match_core_atomic(&core).unwrap()
    }

    #[test]
    fn test_omitted_streak_keys_default_to_live_config() {
        let fixture = Fixture::new();
        let match_id = record_with_streak_row(
            &fixture,
            StreakInsertDefaults {
                multiplier_per_game: 0.40,
                threshold: 5,
            },
            None,
            None,
        );
        let row: (f64, i64) = fixture.connection().query_row("SELECT streak_multiplier_per_game,streak_threshold FROM rating_history WHERE guild_id=?1 AND match_id=?2 AND discord_id=111", params![TEST_GUILD_ID, match_id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(row, (0.40, 5));
    }

    #[test]
    fn test_supplied_streak_keys_are_stored_verbatim() {
        let fixture = Fixture::new();
        let match_id = record_with_streak_row(
            &fixture,
            StreakInsertDefaults::default(),
            Some(0.10),
            Some(7),
        );
        let row: (f64, i64) = fixture.connection().query_row("SELECT streak_multiplier_per_game,streak_threshold FROM rating_history WHERE guild_id=?1 AND match_id=?2 AND discord_id=111", params![TEST_GUILD_ID, match_id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(row, (0.10, 7));
    }

    #[test]
    fn test_stale_match_calculation_is_rejected_without_partial_commit() {
        let fixture = Fixture::new();
        let team1 = (93_000..93_005).collect::<Vec<_>>();
        let team2 = (93_005..93_010).collect::<Vec<_>>();
        for &discord_id in team1.iter().chain(&team2) {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }

        // The command calculated its match against revision zero. A concurrent
        // rating mutation advances the durable revision before the write lock
        // is acquired, so the stale calculation must commit nothing at all.
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(team1.clone(), team2.clone(), 1, Some(TEST_GUILD_ID)),
            "2026-08-05T00:00:00+00:00",
        );
        core.expected_openskill_revision = Some(0);
        core.openskill_updates = team1
            .iter()
            .chain(&team2)
            .map(|&discord_id| CoreOpenSkillUpdate {
                discord_id,
                mu: 40.0,
                sigma: 7.0,
            })
            .collect();

        let target = team1[0];
        let connection = fixture.connection();
        connection
            .execute(
                "UPDATE players SET os_mu=60.0,os_sigma=5.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![target, TEST_GUILD_ID],
            )
            .expect("apply concurrent OpenSkill mutation");
        connection
            .execute(
                "INSERT INTO openskill_rating_revisions(guild_id,revision,updated_at)
                 VALUES (?1,1,CURRENT_TIMESTAMP)",
                [TEST_GUILD_ID],
            )
            .expect("advance concurrent OpenSkill revision");
        drop(connection);

        let error = fixture
            .matches
            .record_match_core_atomic(&core)
            .expect_err("stale match calculation must be rejected");
        assert_eq!(
            error.to_string(),
            "OpenSkill ratings changed while this match was being calculated; retry recording the match"
        );
        let connection = fixture.connection();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM match_participants", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let current: (f64, f64, i64) = connection
            .query_row(
                "SELECT p.os_mu,p.os_sigma,r.revision
                 FROM players p
                 JOIN openskill_rating_revisions r ON r.guild_id=p.guild_id
                 WHERE p.discord_id=?1 AND p.guild_id=?2",
                params![target, TEST_GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load concurrent state after stale rejection");
        assert_eq!(current, (60.0, 5.0, 1));
    }

    #[test]
    fn migrated_core_settles_first_match_referral_before_counters_exactly_once() {
        let database = NamedTempFile::new().expect("temporary migrated referral database");
        crate::schema_manager::initialize_or_migrate(database.path())
            .expect("migrate referral database");
        let connection = Connection::open(database.path()).expect("open referral database");
        connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .expect("disable unrelated legacy fixture foreign keys");
        for (discord_id, name) in [(81_001, "referred"), (81_002, "referrer")] {
            connection
                .execute(
                    "INSERT INTO players (
                         discord_id,guild_id,discord_username,jopacoin_balance,wins,losses
                     ) VALUES (?1,?2,?3,3,0,0)",
                    params![discord_id, TEST_GUILD_ID, name],
                )
                .expect("insert referral player");
        }
        connection
            .execute(
                "INSERT INTO referrals(guild_id,referred_id,referrer_id)
                 VALUES (?1,81001,81002)",
                [TEST_GUILD_ID],
            )
            .expect("insert referral");
        drop(connection);

        let matches = MatchRepository::new(database.path());
        let mut core = CoreMatchRecord::new(
            MatchRecord::new(vec![81_001], vec![81_002], 1, Some(TEST_GUILD_ID)),
            "2026-08-05T00:00:00+00:00",
        );
        core.pending_match_id = Some(818);
        core.winning_ids = vec![81_001];
        core.losing_ids = vec![81_002];
        core.settle_referrals_at = Some(1_754_352_000);
        let match_id = matches
            .record_match_core_atomic(&core)
            .expect("record core referral match");
        assert_eq!(
            matches
                .record_match_core_atomic(&core)
                .expect("retry core referral match"),
            match_id
        );

        let connection = Connection::open(database.path()).expect("inspect referral database");
        let referral: (i64, i64, i64) = connection
            .query_row(
                "SELECT rewarded_match_id,reward_amount,rewarded_at
                 FROM referrals WHERE guild_id=?1 AND referred_id=81001",
                [TEST_GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load referral claim");
        assert_eq!(referral, (match_id, 100, 1_754_352_000));
        for (discord_id, expected_wins, expected_losses) in [(81_001, 1, 0), (81_002, 0, 1)] {
            let player: (i64, i64, i64) = connection
                .query_row(
                    "SELECT jopacoin_balance,wins,losses FROM players
                     WHERE guild_id=?1 AND discord_id=?2",
                    params![TEST_GUILD_ID, discord_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("load referral beneficiary");
            assert_eq!(player, (103, expected_wins, expected_losses));
        }
        let summary = matches
            .get_match(match_id, Some(TEST_GUILD_ID))
            .expect("load referral match")
            .expect("referral match exists");
        assert_eq!(summary.jc_changes[&81_001]["referral"], 100);
        assert_eq!(summary.jc_changes[&81_002]["referral"], 100);
    }

    // Stronger repository invariants intentionally left unmapped from Python.

    #[test]
    fn existing_schema_open_never_creates_a_missing_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("must-not-be-created.db");
        let repository = PlayerRepository::new(&path);
        assert!(repository.exists(1, None).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn record_match_rolls_back_match_and_participants_on_duplicate_signed_id() {
        let fixture = Fixture::new();
        let record = MatchRecord::new(vec![-42], vec![-42], 1, Some(TEST_GUILD_ID));
        assert!(fixture.matches.record_match(&record).is_err());
        let connection = fixture.connection();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM matches", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM match_participants", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn concurrent_steals_are_serialized_without_lost_updates() {
        let fixture = Fixture::new_durable();
        for discord_id in [-1, -2, -3] {
            fixture.add_player(discord_id, TEST_GUILD_ID);
        }
        fixture
            .players
            .update_balance(-1, Some(TEST_GUILD_ID), 0)
            .unwrap();
        fixture
            .players
            .update_balance(-2, Some(TEST_GUILD_ID), 0)
            .unwrap();
        fixture
            .players
            .update_balance(-3, Some(TEST_GUILD_ID), 100)
            .unwrap();
        let left = fixture.players.clone();
        let right = fixture.players.clone();
        let first =
            std::thread::spawn(move || left.steal_atomic(-1, -3, Some(TEST_GUILD_ID), 30).unwrap());
        let second = std::thread::spawn(move || {
            right.steal_atomic(-2, -3, Some(TEST_GUILD_ID), 40).unwrap()
        });
        first.join().unwrap();
        second.join().unwrap();
        let balances = fixture
            .players
            .get_balances_bulk(&[-1, -2, -3], Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(balances.get(&-1), Some(&30));
        assert_eq!(balances.get(&-2), Some(&40));
        assert_eq!(balances.get(&-3), Some(&30));
    }

    #[test]
    fn python_migrated_database_interop_smoke() {
        let file = NamedTempFile::new().unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.ancestors().nth(3).unwrap();
        let output = crate::test_support::parity_python(root)
            .args(["-c", "import sys; from infrastructure.schema_manager import SchemaManager; SchemaManager(sys.argv[1]).initialize()", file.path().to_str().unwrap()])
            .output()
            .expect("run Python schema authority");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let players = PlayerRepository::new(file.path());
        players
            .add(&NewPlayer::new(-9_223_372_036_854_000_000, "Signed", None))
            .unwrap();
        assert_eq!(
            players
                .get_by_id(-9_223_372_036_854_000_000, None)
                .unwrap()
                .unwrap()
                .name,
            "Signed"
        );
    }

    #[test]
    fn test_full_match_workflow_10_players() {
        let fixture = Fixture::new();
        let player_ids = (20_001..20_011).collect::<Vec<_>>();
        for (index, discord_id) in player_ids.iter().copied().enumerate() {
            let role = ["1", "2", "3", "4", "5"][index % 5];
            let mut player = NewPlayer::new(
                discord_id,
                format!("E2E Player {discord_id}"),
                Some(TEST_GUILD_ID),
            );
            player.initial_mmr = Some(1_500 + i64::try_from(index).unwrap() * 10);
            player.glicko_rating = Some(1_500.0 + index as f64 * 5.0);
            player.glicko_rd = Some(350.0);
            player.glicko_volatility = Some(0.06);
            player.preferred_roles = Some(vec![role.to_owned()]);
            fixture.players.add(&player).unwrap();
        }

        let registered = fixture
            .players
            .get_by_ids(&player_ids, Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(registered.len(), 10);
        assert!(registered.iter().all(|player| {
            player
                .preferred_roles
                .as_ref()
                .is_some_and(|roles| !roles.is_empty())
        }));

        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.off_role_flat_penalty = 50.0;
        let (team1, team2) = shuffler.shuffle(&registered).unwrap();
        assert_eq!(team1.players.len(), 5);
        assert_eq!(team2.players.len(), 5);
        let team1_ids = ids_for_team(&team1);
        let team2_ids = ids_for_team(&team2);
        let team1_set = team1_ids.iter().copied().collect::<HashSet<_>>();
        let team2_set = team2_ids.iter().copied().collect::<HashSet<_>>();
        assert!(team1_set.is_disjoint(&team2_set));
        assert_eq!(team1_set.len() + team2_set.len(), player_ids.len());
        assert_eq!(team1_set.union(&team2_set).count(), 10);

        let first_match = fixture
            .matches
            .record_match_core_atomic(&core_match(&team1_ids, &team2_ids, 1))
            .unwrap();
        assert!(
            fixture
                .matches
                .get_match(first_match, Some(TEST_GUILD_ID))
                .unwrap()
                .is_some()
        );
        for discord_id in &team1_ids {
            let player = fixture
                .players
                .get_by_id(*discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (1, 0));
        }
        for discord_id in &team2_ids {
            let player = fixture
                .players
                .get_by_id(*discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (0, 1));
        }

        fixture
            .matches
            .record_match_core_atomic(&core_match(&team1_ids, &team2_ids, 2))
            .unwrap();
        for discord_id in player_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (1, 1));
        }
    }

    #[test]
    fn test_workflow_with_pool_selection() {
        let fixture = Fixture::new();
        let player_ids = (30_001..30_013).collect::<Vec<_>>();
        for (index, discord_id) in player_ids.iter().copied().enumerate() {
            add_rated_player(
                &fixture,
                discord_id,
                format!("Pool Player {discord_id}"),
                1_500.0 + index as f64,
                Some(vec![["1", "2", "3", "4", "5"][index % 5].to_owned()]),
            );
        }
        let players = fixture
            .players
            .get_by_ids(&player_ids, Some(TEST_GUILD_ID))
            .unwrap();
        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.off_role_flat_penalty = 50.0;
        let result = shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .unwrap();
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), 2);

        let selected = result
            .team1
            .players
            .iter()
            .chain(&result.team2.players)
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let excluded = result
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert_eq!(selected.len(), 10);
        assert_eq!(excluded.len(), 2);
        assert!(selected.is_disjoint(&excluded));
        assert_eq!(selected.union(&excluded).count(), 12);
        assert_eq!(
            selected.union(&excluded).copied().collect::<HashSet<_>>(),
            player_ids.into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn test_workflow_with_rating_updates() {
        let fixture = Fixture::new();
        let player_ids = (50_001..50_011).collect::<Vec<_>>();
        for discord_id in player_ids.iter().copied() {
            add_rated_player(
                &fixture,
                discord_id,
                format!("Rated Player {discord_id}"),
                1_500.0,
                None,
            );
        }
        let players = fixture
            .players
            .get_by_ids(&player_ids, Some(TEST_GUILD_ID))
            .unwrap();
        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.off_role_flat_penalty = 50.0;
        let (team1, team2) = shuffler.shuffle(&players).unwrap();
        let team1_ids = ids_for_team(&team1);
        let team2_ids = ids_for_team(&team2);
        let initial_ratings = players
            .iter()
            .map(|player| (player.discord_id.unwrap(), player.glicko_rating.unwrap()))
            .collect::<HashMap<_, _>>();
        let to_rating_player = |player: &cama_domain::player::Player| {
            TeamPlayer::new(
                cama_domain::rating::Player::new_fixed(
                    player.glicko_rating.unwrap(),
                    player.glicko_rd.unwrap(),
                    player.glicko_volatility.unwrap(),
                ),
                player.discord_id.unwrap(),
            )
        };
        let rating_team1 = team1
            .players
            .iter()
            .map(to_rating_player)
            .collect::<Vec<_>>();
        let rating_team2 = team2
            .players
            .iter()
            .map(to_rating_player)
            .collect::<Vec<_>>();
        let updates = CamaRatingSystem::default()
            .update_ratings_after_match(&rating_team1, &rating_team2, 1)
            .unwrap();

        let mut core = core_match(&team1_ids, &team2_ids, 1);
        for update in updates.team1.iter().chain(&updates.team2) {
            core.glicko_updates.push(CoreGlickoUpdate {
                discord_id: update.id,
                rating: update.rating,
                rd: update.rd,
                volatility: update.volatility,
            });
        }
        fixture.matches.record_match_core_atomic(&core).unwrap();

        for update in &updates.team1 {
            let player = fixture
                .players
                .get_by_id(update.id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert!(player.glicko_rating.unwrap() > initial_ratings[&update.id]);
            assert!(player.glicko_rd.unwrap() < 350.0);
            assert_eq!((player.wins, player.losses), (1, 0));
        }
        for update in &updates.team2 {
            let player = fixture
                .players
                .get_by_id(update.id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert!(player.glicko_rating.unwrap() < initial_ratings[&update.id]);
            assert!(player.glicko_rd.unwrap() < 350.0);
            assert_eq!((player.wins, player.losses), (0, 1));
        }
    }

    #[test]
    fn test_record_to_stats_and_leaderboard_flow() {
        let fixture = Fixture::new();
        let player_ids = (91_001..91_011).collect::<Vec<_>>();
        for discord_id in player_ids.iter().copied() {
            add_rated_player(
                &fixture,
                discord_id,
                format!("Stats Player {discord_id}"),
                1_500.0,
                None,
            );
        }
        let radiant = player_ids[..5].to_vec();
        let dire = player_ids[5..].to_vec();
        fixture
            .matches
            .record_match_core_atomic(&core_match(&radiant, &dire, 1))
            .unwrap();

        let winner = fixture
            .players
            .get_by_id(radiant[0], Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        let loser = fixture
            .players
            .get_by_id(dire[0], Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!((winner.wins, winner.losses), (1, 0));
        assert_eq!((loser.wins, loser.losses), (0, 1));

        let leaderboard = fixture
            .players
            .get_leaderboard(Some(TEST_GUILD_ID), 10, 0)
            .unwrap();
        assert_eq!(leaderboard.len(), 10);
        assert!(leaderboard[..5].iter().all(|player| player.wins == 1));
        assert!(leaderboard[5..].iter().all(|player| player.losses == 1));
        assert_eq!(
            leaderboard
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<HashSet<_>>(),
            player_ids.into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn test_multi_match_accumulation_and_nonparticipant() {
        let fixture = Fixture::new();
        let player_ids = (92_001..92_011).collect::<Vec<_>>();
        for discord_id in player_ids.iter().copied() {
            add_rated_player(
                &fixture,
                discord_id,
                format!("Accumulation Player {discord_id}"),
                1_500.0,
                None,
            );
        }
        let bench = 93_001;
        add_rated_player(&fixture, bench, "Bench Player", 1_500.0, None);
        let radiant = player_ids[..5].to_vec();
        let dire = player_ids[5..].to_vec();
        fixture
            .matches
            .record_match_core_atomic(&core_match(&radiant, &dire, 1))
            .unwrap();
        fixture
            .matches
            .record_match_core_atomic(&core_match(&radiant, &dire, 2))
            .unwrap();

        for discord_id in player_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (1, 1));
        }
        let bench_player = fixture
            .players
            .get_by_id(bench, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!((bench_player.wins, bench_player.losses), (0, 0));
    }

    #[test]
    fn test_multiple_matches_with_exclusions() {
        let fixture = Fixture::new();
        let player_ids = (100_001..100_013).collect::<Vec<_>>();
        for discord_id in player_ids.iter().copied() {
            add_rated_player(
                &fixture,
                discord_id,
                format!("Exclusion Player {discord_id}"),
                1_500.0,
                None,
            );
        }

        let match1_radiant = player_ids[..5].to_vec();
        let match1_dire = player_ids[5..10].to_vec();
        fixture
            .matches
            .record_match_core_atomic(&core_match(&match1_radiant, &match1_dire, 1))
            .unwrap();
        for discord_id in &player_ids[10..] {
            let player = fixture
                .players
                .get_by_id(*discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (0, 0));
        }

        let match2_radiant = player_ids[2..7].to_vec();
        let match2_dire = player_ids[7..12].to_vec();
        fixture
            .matches
            .record_match_core_atomic(&core_match(&match2_dire, &match2_radiant, 1))
            .unwrap();

        for (discord_id, expected) in [
            (100_001, (1, 0)),
            (100_002, (1, 0)),
            (100_003, (1, 1)),
            (100_004, (1, 1)),
            (100_005, (1, 1)),
            (100_006, (0, 2)),
            (100_007, (0, 2)),
            (100_008, (1, 1)),
            (100_009, (1, 1)),
            (100_010, (1, 1)),
            (100_011, (1, 0)),
            (100_012, (1, 0)),
        ] {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(
                (player.wins, player.losses),
                expected,
                "player {discord_id}"
            );
        }
    }

    #[test]
    fn test_balanced_shuffle_real_world_example() {
        let fixture = Fixture::new();
        let player_data = [
            ("FakeUser172699", 1_623.0, "1"),
            ("FakeUser169817", 1_018.0, "2"),
            ("FakeUser167858", 1_744.0, "3"),
            ("FakeUser175544", 1_822.0, "4"),
            ("FakeUser173967", 1_836.0, "5"),
            ("FakeUser170233", 1_590.0, "1"),
            ("FakeUser174788", 1_457.0, "2"),
            ("FakeUser171621", 1_882.0, "3"),
            ("FakeUser166664", 1_579.0, "4"),
            ("FakeUser168472", 1_537.0, "5"),
        ];
        let player_ids = (90_001..90_011).collect::<Vec<_>>();
        for (discord_id, (name, rating, role)) in player_ids.iter().copied().zip(player_data) {
            add_rated_player(
                &fixture,
                discord_id,
                name,
                rating,
                Some(vec![role.to_owned()]),
            );
        }
        let players = fixture
            .players
            .get_by_ids(&player_ids, Some(TEST_GUILD_ID))
            .unwrap();
        assert!(players.iter().all(|player| {
            player
                .preferred_roles
                .as_ref()
                .is_some_and(|roles| roles.len() == 1)
        }));

        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.off_role_flat_penalty = 50.0;
        let (team1, team2) = shuffler.shuffle(&players).unwrap();
        let team1_value = team1.get_team_value(true, 1.0, false, false).unwrap();
        let team2_value = team2.get_team_value(true, 1.0, false, false).unwrap();
        let value_diff = (team1_value - team2_value).abs();
        assert!((7_000.0..9_000.0).contains(&team1_value));
        assert!((7_000.0..9_000.0).contains(&team2_value));
        assert!(value_diff < 10.0, "teams differ by {value_diff}");
        assert_eq!(team1.get_off_role_count().unwrap(), 0);
        assert_eq!(team2.get_off_role_count().unwrap(), 0);

        let team1_ids = ids_for_team(&team1);
        let team2_ids = ids_for_team(&team2);
        fixture
            .matches
            .record_match_core_atomic(&core_match(&team1_ids, &team2_ids, 2))
            .unwrap();
        for discord_id in team2_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (1, 0));
        }
        for discord_id in team1_ids {
            let player = fixture
                .players
                .get_by_id(discord_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!((player.wins, player.losses), (0, 1));
        }
    }

    const TEST_SCHEMA: &str = r#"
        PRAGMA journal_mode=WAL;
        PRAGMA busy_timeout=5000;
        CREATE TABLE players (
            discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL DEFAULT 0,
            discord_username TEXT NOT NULL, dotabuff_url TEXT, initial_mmr INTEGER,
            current_mmr REAL, wins INTEGER DEFAULT 0, losses INTEGER DEFAULT 0,
            preferred_roles TEXT, main_role TEXT, glicko_rating REAL, glicko_rd REAL,
            glicko_volatility REAL, os_mu REAL, os_sigma REAL, steam_id INTEGER,
            jopacoin_balance INTEGER DEFAULT 3, exclusion_count INTEGER DEFAULT 0,
            lowest_balance_ever INTEGER, last_match_date TIMESTAMP,
            first_calibrated_at INTEGER, is_captain_eligible INTEGER DEFAULT 0,
            last_wheel_spin INTEGER, last_double_or_nothing INTEGER,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            personal_best_win_streak INTEGER DEFAULT 0, total_bets_placed INTEGER DEFAULT 0,
            first_leverage_used INTEGER DEFAULT 0, has_wheel_pardon INTEGER DEFAULT 0,
            last_trivia_session INTEGER, is_solo_grinder INTEGER DEFAULT 0,
            solo_grinder_checked_at TEXT, dota_streak_days INTEGER NOT NULL DEFAULT 0,
            dota_last_played_date TEXT, preferred_region TEXT, inferred_region TEXT,
            last_pingedash INTEGER, last_pingedkevin INTEGER,
            os_rating_version INTEGER NOT NULL DEFAULT 5, os_algorithm_fingerprint TEXT,
            PRIMARY KEY(discord_id,guild_id)
        );
        CREATE TABLE matches (
            match_id INTEGER PRIMARY KEY AUTOINCREMENT, team1_players TEXT NOT NULL,
            team2_players TEXT NOT NULL, winning_team INTEGER,
            match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP, dotabuff_match_id TEXT,
            notes TEXT, valve_match_id INTEGER, duration_seconds INTEGER,
            radiant_score INTEGER, dire_score INTEGER, game_mode INTEGER,
            enrichment_data TEXT, enrichment_source TEXT, enrichment_confidence REAL,
            lobby_type TEXT DEFAULT 'shuffle', balancing_rating_system TEXT DEFAULT 'glicko',
            guild_id INTEGER NOT NULL DEFAULT 0, betting_mode TEXT DEFAULT 'pool',
            pending_match_id INTEGER, bonuses_paid INTEGER NOT NULL DEFAULT 0,
            win_reward_jc INTEGER, jc_changes TEXT,
            lobby_kind TEXT CHECK(lobby_kind IS NULL OR lobby_kind IN ('open','lowskill'))
        );
        CREATE UNIQUE INDEX uq_matches_guild_pending_match ON matches(guild_id,pending_match_id) WHERE pending_match_id IS NOT NULL;
        CREATE TABLE match_participants (
            match_id INTEGER, discord_id INTEGER, team_number INTEGER, won BOOLEAN,
            side TEXT, hero_id INTEGER, kills INTEGER, deaths INTEGER, assists INTEGER,
            last_hits INTEGER, denies INTEGER, gpm INTEGER, xpm INTEGER,
            hero_damage INTEGER, tower_damage INTEGER, net_worth INTEGER,
            hero_healing INTEGER, lane_role INTEGER, lane_efficiency INTEGER,
            towers_killed INTEGER, roshans_killed INTEGER, teamfight_participation REAL,
            obs_placed INTEGER, sen_placed INTEGER, camps_stacked INTEGER,
            rune_pickups INTEGER, firstblood_claimed INTEGER, stuns REAL,
            fantasy_points REAL, guild_id INTEGER NOT NULL DEFAULT 0,
            bonus_jc INTEGER, win_bonus_jc INTEGER, PRIMARY KEY(match_id,discord_id)
        );
        CREATE TABLE rating_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT, discord_id INTEGER, rating REAL,
            rating_before REAL, rd_before REAL, rd_after REAL,
            volatility_before REAL, volatility_after REAL,
            expected_team_win_prob REAL, team_number INTEGER, won BOOLEAN,
            match_id INTEGER, timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            os_mu_before REAL, os_mu_after REAL, os_sigma_before REAL,
            os_sigma_after REAL, fantasy_weight REAL, streak_length INTEGER,
            streak_multiplier REAL, guild_id INTEGER NOT NULL DEFAULT 0,
            os_algorithm_version INTEGER, os_algorithm_fingerprint TEXT,
            streak_multiplier_per_game REAL NOT NULL DEFAULT 0.20,
            streak_threshold INTEGER NOT NULL DEFAULT 3,
            low_priority_gain_multiplier REAL NOT NULL DEFAULT 1.0,
            base_rating_delta_multiplier REAL NOT NULL DEFAULT 0.75
        );
        CREATE TABLE match_predictions (
            match_id INTEGER PRIMARY KEY, radiant_rating REAL, dire_rating REAL,
            radiant_rd REAL, dire_rd REAL, expected_radiant_win_prob REAL,
            timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            openskill_radiant_win_prob REAL, openskill_raw_radiant_win_prob REAL,
            openskill_algorithm_version INTEGER, openskill_algorithm_fingerprint TEXT
        );
        CREATE TABLE bets (
            bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL DEFAULT 0,
            match_id INTEGER,
            discord_id INTEGER NOT NULL,
            team_bet_on TEXT NOT NULL,
            amount INTEGER NOT NULL,
            bet_time INTEGER NOT NULL
        );
        CREATE TABLE player_pairings (
            id INTEGER PRIMARY KEY AUTOINCREMENT, guild_id INTEGER NOT NULL,
            player1_id INTEGER NOT NULL, player2_id INTEGER NOT NULL,
            games_together INTEGER NOT NULL DEFAULT 0,
            wins_together INTEGER NOT NULL DEFAULT 0,
            games_against INTEGER NOT NULL DEFAULT 0,
            player1_wins_against INTEGER NOT NULL DEFAULT 0,
            last_match_id INTEGER, updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(guild_id,player1_id,player2_id)
        );
        CREATE TABLE pending_matches (
            pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL, payload TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE openskill_rating_revisions (
            guild_id INTEGER PRIMARY KEY, revision INTEGER NOT NULL DEFAULT 0,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE openskill_rating_events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT, guild_id INTEGER NOT NULL DEFAULT 0,
            discord_id INTEGER NOT NULL, event_type TEXT NOT NULL, value REAL NOT NULL,
            event_at TIMESTAMP NOT NULL, reason TEXT, actor_id INTEGER,
            os_algorithm_version INTEGER NOT NULL, os_algorithm_fingerprint TEXT NOT NULL
        );
        CREATE TABLE lobby_suspensions (
            guild_id INTEGER NOT NULL DEFAULT 0, discord_id INTEGER NOT NULL,
            scope TEXT NOT NULL, completion TEXT NOT NULL, expires_at INTEGER,
            matches_required INTEGER, matches_remaining INTEGER,
            pending_match_watermark INTEGER NOT NULL DEFAULT 0, reason TEXT NOT NULL,
            source TEXT NOT NULL, actor_id INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1,
            active INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL, PRIMARY KEY(guild_id,discord_id)
        );
        CREATE TABLE low_priority_state (
            discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL DEFAULT 0,
            wins_required INTEGER NOT NULL DEFAULT 3,
            wins_remaining INTEGER NOT NULL DEFAULT 3,
            start_pending_match_id INTEGER, active INTEGER NOT NULL DEFAULT 1,
            reason TEXT, set_by INTEGER NOT NULL, removed_by INTEGER,
            removed_reason TEXT, created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(discord_id,guild_id)
        );
        CREATE TABLE moderation_events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT, guild_id INTEGER NOT NULL DEFAULT 0,
            discord_id INTEGER NOT NULL, event_type TEXT NOT NULL, source TEXT NOT NULL,
            actor_id INTEGER, reason TEXT, scope TEXT, completion TEXT, expires_at INTEGER,
            matches_required INTEGER, matches_remaining INTEGER,
            pending_match_watermark INTEGER, wins_required INTEGER, wins_remaining INTEGER,
            match_id INTEGER, created_at INTEGER NOT NULL
        );
    "#;

    #[test]
    fn string_array_codec_round_trips_python_json_bytes() {
        // Bytes exactly as CPython json.dumps writes them (incl. a control
        // character, named escapes, and a surrogate-pair \uXXXX emoji).
        let python_written =
            r#"["a\u0001b", "tab\tnewline\n", "quote\"back\\", "emoji\ud83d\ude00"]"#;
        let decoded = decode_string_array(python_written).expect("decode Python-written column");
        assert_eq!(
            decoded,
            [
                "a\u{1}b".to_owned(),
                "tab\tnewline\n".to_owned(),
                "quote\"back\\".to_owned(),
                "emoji\u{1F600}".to_owned(),
            ]
        );
        // Rust-encoded bytes must round-trip through our own decoder (and are
        // standard JSON, which Python's json.loads accepts).
        let encoded = encode_string_array(&decoded);
        assert_eq!(decode_string_array(&encoded).expect("round trip"), decoded);
    }
}
