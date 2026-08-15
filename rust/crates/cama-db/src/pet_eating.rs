//! Existing-schema persistence for consuming an adult Camagotchi.
//!
//! Python remains the schema, migration, and live runtime authority during the
//! side-by-side migration. This adapter opens only an already-migrated SQLite
//! database. The pet CAS, evolution suppression, player reward, bankruptcy
//! penalty, brawl guard, and trigger-backed ledger context share one
//! `BEGIN IMMEDIATE` transaction.

use std::fmt;
use std::path::{Path, PathBuf};

use cama_domain::pet::{ADULT_AGE_SECONDS, Pet};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;
use crate::pet_repository::{PetRepository, PetRepositoryError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PetEatingFailure {
    InBrawl,
    NoPet,
    PetDead,
    NotAdult,
    StalePet,
    PlayerNotFound,
}

impl fmt::Display for PetEatingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InBrawl => "in_brawl",
            Self::NoPet => "no_pet",
            Self::PetDead => "pet_dead",
            Self::NotAdult => "not_adult",
            Self::StalePet => "stale_pet",
            Self::PlayerNotFound => "player_not_found",
        })
    }
}

#[derive(Debug, Error)]
pub enum PetEatingRepositoryError {
    #[error("{0}")]
    Rejected(PetEatingFailure),
    #[error("pet-eating balance or penalty arithmetic exceeds SQLite INTEGER range")]
    ArithmeticOutOfRange,
    #[error("committed eaten pet {0} could not be reloaded")]
    CommittedPetMissing(i64),
    #[error(transparent)]
    PetRepository(#[from] PetRepositoryError),
    #[error("pet-eating SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl PetEatingRepositoryError {
    #[must_use]
    pub const fn failure(&self) -> Option<PetEatingFailure> {
        match self {
            Self::Rejected(failure) => Some(*failure),
            Self::ArithmeticOutOfRange
            | Self::CommittedPetMissing(_)
            | Self::PetRepository(_)
            | Self::Sqlite(_) => None,
        }
    }
}

impl From<PetEatingFailure> for PetEatingRepositoryError {
    fn from(value: PetEatingFailure) -> Self {
        Self::Rejected(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EatAdultPetRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub pet_id: i64,
    pub expected_last_fed_at: i64,
    pub expected_hunger: i64,
    pub reward: i64,
    pub penalty_games: i64,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EatAdultPetOutcome {
    pub pet: Pet,
    pub new_balance: i64,
    pub penalty_games_remaining: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableEatingOutcome {
    pub reward: i64,
    pub penalty_games_added: i64,
    pub penalty_games_remaining: i64,
    pub new_balance: i64,
}

#[derive(Clone, Debug)]
pub struct PetEatingRepository {
    path: PathBuf,
}

impl PetEatingRepository {
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

    pub fn get_active_pet(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetEatingRepositoryError> {
        PetRepository::new(&self.path)
            .get_active_pet(discord_id, guild_id)
            .map_err(Into::into)
    }

    pub fn get_pet_by_id(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetEatingRepositoryError> {
        PetRepository::new(&self.path)
            .get_pet_by_id(pet_id, guild_id)
            .map_err(Into::into)
    }

    pub fn has_open_brawl(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<bool, PetEatingRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM pet_brawls
                  WHERE guild_id = ?1
                    AND (status = 'active' OR (status = 'pending' AND expires_at > ?2))
                    AND (challenger_id = ?3 OR recipient_id = ?3)
                  LIMIT 1",
                params![Self::normalize_guild_id(guild_id), now, discord_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn eat_adult_pet_atomic(
        &self,
        request: EatAdultPetRequest,
    ) -> Result<EatAdultPetOutcome, PetEatingRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let adult_cutoff = request
            .now
            .checked_sub(ADULT_AGE_SECONDS)
            .ok_or(PetEatingRepositoryError::ArithmeticOutOfRange)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let in_brawl = transaction
            .query_row(
                "SELECT 1 FROM pet_brawls
                  WHERE guild_id = ?1
                    AND (status = 'active' OR (status = 'pending' AND expires_at > ?2))
                    AND (challenger_id = ?3 OR recipient_id = ?3)
                  LIMIT 1",
                params![guild_id, request.now, request.discord_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if in_brawl {
            return Err(PetEatingFailure::InBrawl.into());
        }

        let changed = transaction.execute(
            "UPDATE pets
                SET died_at = ?1,
                    death_cause = 'eaten',
                    evolved_at = CASE WHEN evolution_due_at < ?1 THEN evolved_at ELSE NULL END,
                    evolution_calling = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_calling ELSE NULL END,
                    evolution_primary = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_primary ELSE NULL END,
                    evolution_secondary = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_secondary ELSE NULL END,
                    evolution_announced_at = CASE
                        WHEN evolution_due_at < ?1 AND evolved_at IS NOT NULL
                        THEN COALESCE(evolution_announced_at, ?1)
                        ELSE NULL
                    END
              WHERE pet_id = ?2 AND guild_id = ?3 AND discord_id = ?4
                AND died_at IS NULL
                AND hatched_at <= ?5
                AND last_fed_at = ?6 AND hunger_at_last_fed = ?7",
            params![
                request.now,
                request.pet_id,
                guild_id,
                request.discord_id,
                adult_cutoff,
                request.expected_last_fed_at,
                request.expected_hunger,
            ],
        )?;
        if changed != 1 {
            let state = transaction
                .query_row(
                    "SELECT died_at, hatched_at FROM pets
                      WHERE pet_id = ?1 AND guild_id = ?2 AND discord_id = ?3",
                    params![request.pet_id, guild_id, request.discord_id],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let failure = match state {
                None => PetEatingFailure::NoPet,
                Some((Some(_), _)) => PetEatingFailure::PetDead,
                Some((None, hatched_at)) if hatched_at > adult_cutoff => PetEatingFailure::NotAdult,
                Some((None, _)) => PetEatingFailure::StalePet,
            };
            return Err(failure.into());
        }

        let (pet_name, species) = transaction.query_row(
            "SELECT name, species FROM pets WHERE pet_id = ?1 AND guild_id = ?2",
            params![request.pet_id, guild_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(PetEatingFailure::PlayerNotFound)?;
        let existing_penalty = transaction
            .query_row(
                "SELECT COALESCE(penalty_games_remaining, 0) FROM bankruptcy_state
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let new_balance = balance
            .checked_add(request.reward)
            .ok_or(PetEatingRepositoryError::ArithmeticOutOfRange)?;
        let penalty_games_remaining = existing_penalty
            .checked_add(request.penalty_games)
            .ok_or(PetEatingRepositoryError::ArithmeticOutOfRange)?;
        let metadata = format!(
            "{{\"pet_id\": {}, \"species\": {}, \"reward\": {}, \
             \"penalty_games\": {}, \"penalty_games_remaining\": {}}}",
            request.pet_id,
            json_string(&species),
            request.reward,
            request.penalty_games,
            penalty_games_remaining,
        );
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        transaction.execute(
            "INSERT INTO economy_ledger_context (
                 id, source, actor_id, related_type, related_id, reason, metadata
             ) VALUES (1, 'pet', ?1, 'pet_eating', ?2, ?3, ?4)",
            params![
                request.discord_id,
                request.pet_id.to_string(),
                format!("ate pet {pet_name}"),
                metadata,
            ],
        )?;
        let credited = transaction.execute(
            "UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                    updated_at = CURRENT_TIMESTAMP
              WHERE discord_id = ?2 AND guild_id = ?3",
            params![request.reward, request.discord_id, guild_id],
        );
        let cleared = transaction.execute("DELETE FROM economy_ledger_context", []);
        let credited = credited?;
        cleared?;
        if credited != 1 {
            return Err(PetEatingFailure::PlayerNotFound.into());
        }

        transaction.execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count, updated_at
             ) VALUES (?1, ?2, NULL, ?3, 0, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 penalty_games_remaining =
                     COALESCE(bankruptcy_state.penalty_games_remaining, 0)
                     + excluded.penalty_games_remaining,
                 updated_at = CURRENT_TIMESTAMP",
            params![request.discord_id, guild_id, request.penalty_games],
        )?;
        transaction.commit()?;

        let pet = PetRepository::new(&self.path)
            .get_pet_by_id(request.pet_id, request.guild_id)?
            .ok_or(PetEatingRepositoryError::CommittedPetMissing(
                request.pet_id,
            ))?;
        Ok(EatAdultPetOutcome {
            pet,
            new_balance,
            penalty_games_remaining,
        })
    }

    pub fn get_eating_outcome(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<DurableEatingOutcome>, PetEatingRepositoryError> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT delta, balance_after,
                        CASE WHEN json_valid(metadata)
                             THEN CAST(json_extract(metadata, '$.penalty_games') AS INTEGER) END,
                        CASE WHEN json_valid(metadata)
                             THEN CAST(json_extract(
                                 metadata, '$.penalty_games_remaining') AS INTEGER) END
                   FROM economy_ledger_entries
                  WHERE guild_id = ?1 AND account_type = 'player'
                    AND source = 'pet' AND related_type = 'pet_eating'
                    AND related_id = ?2
                  ORDER BY ledger_id DESC LIMIT 1",
                params![Self::normalize_guild_id(guild_id), pet_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(
            |(reward, new_balance, penalty_games_added, penalty_games_remaining)| {
                Some(DurableEatingOutcome {
                    reward,
                    penalty_games_added: penalty_games_added?,
                    penalty_games_remaining: penalty_games_remaining?,
                    new_balance,
                })
            },
        ))
    }

    pub fn recent_dead_species(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<String>, PetEatingRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT species FROM pets
              WHERE discord_id = ?1 AND guild_id = ?2 AND died_at IS NOT NULL
                AND COALESCE(death_cause, '') NOT IN ('sacrifice', 'eaten')
              ORDER BY adopted_at DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id), limit],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn find_unannounced_evolution_refs(
        &self,
        limit: i64,
    ) -> Result<Vec<(i64, i64)>, PetEatingRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT pet_id, guild_id FROM pets
              WHERE evolved_at IS NOT NULL AND evolution_announced_at IS NULL
              ORDER BY evolved_at, pet_id LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character < ' ' => {
                escaped.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
#[path = "pet_eating/tests.rs"]
mod tests;
