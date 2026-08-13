//! Python-compatible persistence for Camagotchi pets.
//!
//! Python remains the schema and migration authority during the side-by-side
//! rewrite. This module opens only an existing SQLite file, uses the existing
//! Python tables and ledger triggers, and never creates or migrates production
//! data. Multi-row state changes use `BEGIN IMMEDIATE`, matching Python's
//! single-writer transaction boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::path::{Path, PathBuf};

use cama_domain::game_date::game_day_start_ts;
use cama_domain::pet::{
    ADULT_AGE_SECONDS, BRAWL_TRAINING_LEVEL_CAP, BRAWL_TRAINING_STAT_CAP, BRAWL_TRAINING_XP_CAP,
    BRAWL_XP_PER_LEVEL, Pet, PetRow, PetRowError, RefundNotice, RefundPayout,
    SOLO_TRAINING_SESSION_CAP, SOLO_TRAINING_XP_PER_SESSION, SqliteValue, UNHATCHED_SPECIES,
};
use rusqlite::types::{Type, Value, ValueRef};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Row, Transaction, TransactionBehavior, params,
    params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

const PET_COLUMNS: &str = "pet_id, discord_id, guild_id, name, species, adopted_at, hatched_at, \
     adopt_fee, last_fed_at, hunger_at_last_fed, times_fed, feeds_today, \
     feed_date, week_consumed_jc, week_key, prev_week_consumed_jc, \
     prev_week_key, pampered_until, accessory, dig_work_units, dig_work_at, \
     aegis_used, hatch_announced_at, died_at, death_cause, death_announced_at, \
     egg_tier, training_xp, training_str, training_int, training_dex, \
     solo_training_sessions, solo_training_recharged_at, evolution_started_at, \
     evolution_due_at, evolved_at, evolution_calling, evolution_primary, \
     evolution_secondary, evolution_announced_at";

const PET_COLUMN_NAMES: [&str; 40] = [
    "pet_id",
    "discord_id",
    "guild_id",
    "name",
    "species",
    "adopted_at",
    "hatched_at",
    "adopt_fee",
    "last_fed_at",
    "hunger_at_last_fed",
    "times_fed",
    "feeds_today",
    "feed_date",
    "week_consumed_jc",
    "week_key",
    "prev_week_consumed_jc",
    "prev_week_key",
    "pampered_until",
    "accessory",
    "dig_work_units",
    "dig_work_at",
    "aegis_used",
    "hatch_announced_at",
    "died_at",
    "death_cause",
    "death_announced_at",
    "egg_tier",
    "training_xp",
    "training_str",
    "training_int",
    "training_dex",
    "solo_training_sessions",
    "solo_training_recharged_at",
    "evolution_started_at",
    "evolution_due_at",
    "evolved_at",
    "evolution_calling",
    "evolution_primary",
    "evolution_secondary",
    "evolution_announced_at",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetRepositoryFailure {
    InsufficientFunds,
    AlreadyHasPet,
    NoPet,
    PetDead,
    PetHatched,
    AlreadyGilded,
    EggNotReady,
    StackCap,
    NoSupplies,
    FeedCap,
    StalePet,
    AlreadyPampered,
    NotOwned,
    FundShort,
    InBrawl,
}

#[derive(Debug, Error)]
pub enum PetTrainingError {
    #[error("no_pet")]
    NoPet,
    #[error("pet_dead")]
    PetDead,
    #[error("pet_egg")]
    PetEgg,
    #[error("training_complete")]
    TrainingComplete,
    #[error("no_training_sessions")]
    NoTrainingSessions,
    #[error("in_brawl")]
    InBrawl,
    #[error("invalid_game_day")]
    InvalidGameDay,
    #[error("pet row error: {0}")]
    PetRow(#[from] PetRowError),
    #[error("pet SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl fmt::Display for PetRepositoryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InsufficientFunds => "insufficient_funds",
            Self::AlreadyHasPet => "already_has_pet",
            Self::NoPet => "no_pet",
            Self::PetDead => "pet_dead",
            Self::PetHatched => "pet_hatched",
            Self::AlreadyGilded => "already_gilded",
            Self::EggNotReady => "egg_not_ready",
            Self::StackCap => "stack_cap",
            Self::NoSupplies => "no_supplies",
            Self::FeedCap => "feed_cap",
            Self::StalePet => "stale_pet",
            Self::AlreadyPampered => "already_pampered",
            Self::NotOwned => "not_owned",
            Self::FundShort => "fund_short",
            Self::InBrawl => "in_brawl",
        })
    }
}

#[derive(Debug, Error)]
pub enum PetRepositoryError {
    #[error("{0}")]
    Rejected(PetRepositoryFailure),
    #[error("pet timestamp or balance arithmetic exceeds SQLite INTEGER range")]
    ArithmeticOutOfRange,
    #[error("{0}")]
    PetRow(#[from] PetRowError),
    #[error("pet-repository SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl PetRepositoryError {
    #[must_use]
    pub const fn failure(&self) -> Option<PetRepositoryFailure> {
        match self {
            Self::Rejected(failure) => Some(*failure),
            Self::ArithmeticOutOfRange | Self::PetRow(_) | Self::Sqlite(_) => None,
        }
    }
}

impl From<PetRepositoryFailure> for PetRepositoryError {
    fn from(value: PetRepositoryFailure) -> Self {
        Self::Rejected(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptPetRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub name: &'a str,
    pub egg_tier: &'a str,
    pub fee: i64,
    pub now: i64,
    pub hatch_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeEggRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub name: &'a str,
    pub premium: i64,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SacrificePetRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub old_pet_id: i64,
    pub expected_last_fed_at: i64,
    pub expected_hunger: i64,
    pub name: &'a str,
    pub species: &'a str,
    pub fee: i64,
    pub now: i64,
    pub hatch_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SacrificePetOutcome {
    pub dead_pet: Pet,
    pub new_pet: Pet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuySuppliesRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub item_id: &'a str,
    pub qty: i64,
    pub total_cost: i64,
    pub now: i64,
    pub stack_cap: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorGuard {
    pub expected_last_fed_at: i64,
    pub expected_hunger: i64,
    pub expected_dig_work_units: Option<i64>,
    pub expected_dig_work_at: Option<i64>,
    pub new_dig_work_units: Option<i64>,
    pub new_dig_work_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HungerTopUpRequest {
    pub pet_id: i64,
    pub guild_id: Option<i64>,
    pub anchor: AnchorGuard,
    pub new_last_fed_at: i64,
    pub new_hunger: i64,
}

/// Result of Python's atomic solo-training write.  The runtime brawl adapter
/// consumes this small value without duplicating the repository transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoloTrainingOutcome {
    pub sessions_used: i64,
    pub sessions_available: i64,
    pub xp_delta: i64,
    pub stat_gain: Option<String>,
}

impl AnchorGuard {
    #[must_use]
    pub const fn new(expected_last_fed_at: i64, expected_hunger: i64) -> Self {
        Self {
            expected_last_fed_at,
            expected_hunger,
            expected_dig_work_units: None,
            expected_dig_work_at: None,
            new_dig_work_units: None,
            new_dig_work_at: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedPetRequest<'a> {
    pub pet_id: i64,
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub item_id: &'a str,
    pub anchor: AnchorGuard,
    pub new_last_fed_at: i64,
    pub new_hunger: i64,
    pub feed_date: &'a str,
    pub feed_cap: i64,
    pub week_key: &'a str,
    pub consumed_jc: i64,
    pub spat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaltLickRequest<'a> {
    pub pet_id: i64,
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub cost: i64,
    pub now: i64,
    pub pampered_until: i64,
    pub week_key: &'a str,
    pub anchor: Option<AnchorGuard>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrinketPurchaseRequest<'a> {
    pub pet_id: i64,
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub accessory_id: &'a str,
    pub cost: i64,
    pub refund: i64,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrinketPurchaseOutcome {
    pub duplicate: bool,
    pub net_cost: i64,
    pub owned_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeathClaimRequest<'a> {
    pub pet_id: i64,
    pub guild_id: Option<i64>,
    pub died_at: i64,
    pub cause: &'a str,
    pub anchor: AnchorGuard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AegisReviveRequest {
    pub pet_id: i64,
    pub guild_id: Option<i64>,
    pub revived_last_fed_at: i64,
    pub revive_hunger: i64,
    pub anchor: AnchorGuard,
}

#[derive(Clone, Debug)]
pub struct PetRepository {
    path: PathBuf,
}

impl PetRepository {
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
    ) -> Result<Option<Pet>, PetRepositoryError> {
        let connection = self.connection()?;
        optional_pet(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets \
                 WHERE discord_id = ?1 AND guild_id = ?2 AND died_at IS NULL"
            ),
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )
    }

    /// Return the currently banked solo-training sessions, recharging one per
    /// game day since the persisted recharge anchor and never exceeding the
    /// Python cap of three.
    pub fn available_solo_training_sessions(
        pet: &Pet,
        now: i64,
    ) -> Result<i64, crate::pet_repository::PetTrainingError> {
        if pet.solo_training_recharged_at.is_none() {
            return Ok(SOLO_TRAINING_SESSION_CAP.min(pet.solo_training_sessions));
        }
        let now_day = game_day_start_ts(now)
            .map_err(|_| crate::pet_repository::PetTrainingError::InvalidGameDay)?;
        let recharge_day = game_day_start_ts(pet.solo_training_recharged_at.unwrap_or(now))
            .map_err(|_| crate::pet_repository::PetTrainingError::InvalidGameDay)?;
        let elapsed_days = ((now_day - recharge_day) / 86_400).max(0);
        Ok(SOLO_TRAINING_SESSION_CAP.min(pet.solo_training_sessions.saturating_add(elapsed_days)))
    }

    /// Commit Python's solo-training mutation under one immediate SQLite
    /// transaction.  Error strings are intentionally stable machine codes so
    /// the application service can preserve its existing public mapping while
    /// this repository remains independent of Discord.
    pub fn train_solo_atomic(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        now: i64,
        proposed_stat: Option<&str>,
    ) -> Result<SoloTrainingOutcome, crate::pet_repository::PetTrainingError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self
            .connection()
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?;
        let pet_row = transaction
            .query_row(
                &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1 AND guild_id = ?2"),
                params![pet_id, guild_id],
                pet_row,
            )
            .optional()
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?
            .ok_or(crate::pet_repository::PetTrainingError::NoPet)?;
        let pet =
            Pet::from_row(&pet_row).map_err(crate::pet_repository::PetTrainingError::PetRow)?;
        if pet.died_at.is_some() {
            return Err(crate::pet_repository::PetTrainingError::PetDead);
        }
        if now < pet.hatched_at {
            return Err(crate::pet_repository::PetTrainingError::PetEgg);
        }
        if pet.training_xp >= BRAWL_TRAINING_XP_CAP {
            return Err(crate::pet_repository::PetTrainingError::TrainingComplete);
        }
        let in_brawl = transaction
            .query_row(
                "SELECT 1 FROM pet_brawls
                 WHERE guild_id = ?1 AND status = 'active'
                   AND (challenger_id = ?2 OR recipient_id = ?2) LIMIT 1",
                params![guild_id, pet.discord_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?
            .is_some();
        if in_brawl {
            return Err(crate::pet_repository::PetTrainingError::InBrawl);
        }
        let available = Self::available_solo_training_sessions(&pet, now)?;
        if available <= 0 {
            return Err(crate::pet_repository::PetTrainingError::NoTrainingSessions);
        }
        let sessions_used = available.min(
            (BRAWL_TRAINING_XP_CAP - pet.training_xp)
                .div_euclid(SOLO_TRAINING_XP_PER_SESSION.max(1)),
        );
        if sessions_used <= 0 {
            return Err(crate::pet_repository::PetTrainingError::TrainingComplete);
        }
        let xp_delta = sessions_used * SOLO_TRAINING_XP_PER_SESSION;
        let new_xp = (pet.training_xp + xp_delta).min(BRAWL_TRAINING_XP_CAP);
        let mut strength = pet.training_str;
        let mut intelligence = pet.training_int;
        let mut dexterity = pet.training_dex;
        let mut stat_gain = None;
        if new_xp / BRAWL_XP_PER_LEVEL > pet.training_xp / BRAWL_XP_PER_LEVEL
            && strength + intelligence + dexterity < BRAWL_TRAINING_LEVEL_CAP
        {
            let selected = proposed_stat
                .filter(|stat| match *stat {
                    "str" => strength < BRAWL_TRAINING_STAT_CAP,
                    "int" => intelligence < BRAWL_TRAINING_STAT_CAP,
                    "dex" => dexterity < BRAWL_TRAINING_STAT_CAP,
                    _ => false,
                })
                .or_else(|| {
                    [("str", strength), ("int", intelligence), ("dex", dexterity)]
                        .into_iter()
                        .find(|(_, value)| *value < BRAWL_TRAINING_STAT_CAP)
                        .map(|(stat, _)| stat)
                });
            if let Some(selected) = selected {
                match selected {
                    "str" => strength += 1,
                    "int" => intelligence += 1,
                    "dex" => dexterity += 1,
                    _ => {}
                }
                stat_gain = Some(selected.to_owned());
            }
        }
        let sessions_available = available - sessions_used;
        let changed = transaction
            .execute(
                "UPDATE pets SET training_xp = ?1, training_str = ?2,
                    training_int = ?3, training_dex = ?4,
                    solo_training_sessions = ?5, solo_training_recharged_at = ?6
                 WHERE pet_id = ?7 AND guild_id = ?8 AND died_at IS NULL",
                params![
                    new_xp,
                    strength,
                    intelligence,
                    dexterity,
                    sessions_available,
                    now,
                    pet_id,
                    guild_id
                ],
            )
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?;
        if changed != 1 {
            return Err(crate::pet_repository::PetTrainingError::PetDead);
        }
        transaction
            .commit()
            .map_err(crate::pet_repository::PetTrainingError::Sqlite)?;
        Ok(SoloTrainingOutcome {
            sessions_used,
            sessions_available,
            xp_delta,
            stat_gain,
        })
    }

    pub fn get_pet_by_id(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError> {
        let connection = self.connection()?;
        optional_pet(
            &connection,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1 AND guild_id = ?2"),
            params![pet_id, Self::normalize_guild_id(guild_id)],
        )
    }

    pub fn count_dead_pets(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, PetRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM pets
                 WHERE discord_id = ?1 AND guild_id = ?2 AND died_at IS NOT NULL",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn get_latest_dead_pet(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError> {
        let connection = self.connection()?;
        optional_pet(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE discord_id = ?1 AND guild_id = ?2 AND died_at IS NOT NULL
                 ORDER BY died_at DESC LIMIT 1"
            ),
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )
    }

    pub fn get_graveyard(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        query_pets(
            &self.connection()?,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE discord_id=?1 AND guild_id=?2 AND died_at IS NOT NULL
                 ORDER BY died_at DESC LIMIT ?3"
            ),
            params![discord_id, Self::normalize_guild_id(guild_id), limit],
        )
    }

    pub fn get_oldest_living(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        query_pets(
            &self.connection()?,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE guild_id=?1 AND died_at IS NULL
                 ORDER BY hatched_at ASC LIMIT ?2"
            ),
            params![Self::normalize_guild_id(guild_id), limit],
        )
    }

    pub fn get_active_pets_for(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        let unique = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=unique.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {PET_COLUMNS} FROM pets
             WHERE guild_id = ? AND died_at IS NULL
               AND discord_id IN ({placeholders})
             ORDER BY pet_id"
        );
        let mut bindings = vec![Value::Integer(Self::normalize_guild_id(guild_id))];
        bindings.extend(unique.into_iter().map(Value::Integer));
        query_pets(&self.connection()?, &sql, params_from_iter(bindings))
    }

    pub fn get_supplies(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<String, i64>, PetRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT item_id, qty FROM pet_supplies
             WHERE discord_id = ?1 AND guild_id = ?2 AND qty > 0",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }

    pub fn adopt_pet(&self, request: &AdoptPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let hatched_at = request
            .now
            .checked_add(request.hatch_seconds)
            .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
        let evolution_due_at = hatched_at
            .checked_add(ADULT_AGE_SECONDS)
            .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            request.fee,
            DebitContext {
                related_type: "pet_adopt",
                related_id: request.discord_id.to_string(),
                reason: format!("adopted pet {}", request.name),
                metadata: Some(format!(
                    "{{\"egg_tier\": {}, \"fee\": {}}}",
                    json_string(request.egg_tier),
                    request.fee
                )),
            },
        )?;
        let inserted = transaction.execute(
            "INSERT INTO pets (
                 discord_id, guild_id, name, species, egg_tier, adopted_at,
                 hatched_at, adopt_fee, last_fed_at, hunger_at_last_fed,
                 dig_work_units, dig_work_at, evolution_started_at,
                 evolution_due_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?7, 100, 0, ?7, ?7, ?9)",
            params![
                request.discord_id,
                guild_id,
                request.name,
                UNHATCHED_SPECIES,
                request.egg_tier,
                request.now,
                hatched_at,
                request.fee,
                evolution_due_at,
            ],
        );
        if let Err(error) = inserted {
            if is_constraint_violation(&error) {
                return Err(PetRepositoryFailure::AlreadyHasPet.into());
            }
            return Err(error.into());
        }
        let pet_id = transaction.last_insert_rowid();
        let pet = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1"),
            [pet_id],
        )?;
        transaction.commit()?;
        Ok(pet)
    }

    pub fn upgrade_egg(&self, request: &UpgradeEggRequest<'_>) -> Result<Pet, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pet = optional_pet(
            &transaction,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE discord_id = ?1 AND guild_id = ?2 AND died_at IS NULL"
            ),
            params![request.discord_id, guild_id],
        )?
        .ok_or(PetRepositoryFailure::NoPet)?;
        if request.now >= pet.hatched_at || pet.species != UNHATCHED_SPECIES {
            return Err(PetRepositoryFailure::PetHatched.into());
        }
        if pet.egg_tier == "gilded" {
            return Err(PetRepositoryFailure::AlreadyGilded.into());
        }
        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            request.premium,
            DebitContext {
                related_type: "pet_upgrade",
                related_id: request.discord_id.to_string(),
                reason: format!("upgraded egg {} to gilded", pet.name),
                metadata: None,
            },
        )?;
        transaction.execute(
            "UPDATE pets SET name = ?1, egg_tier = 'gilded', adopt_fee = adopt_fee + ?2
             WHERE pet_id = ?3 AND guild_id = ?4",
            params![request.name, request.premium, pet.pet_id, guild_id],
        )?;
        let upgraded = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1 AND guild_id = ?2"),
            params![pet.pet_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(upgraded)
    }

    /// Atomically give one living pet to the altar, debit the escalating fee,
    /// and create the concrete-species rebirth egg. The anchor guard makes a
    /// concurrent feed or settlement retryable without ever charging for a
    /// pet that was not actually sacrificed.
    pub fn sacrifice_and_adopt_atomic(
        &self,
        request: &SacrificePetRequest<'_>,
    ) -> Result<SacrificePetOutcome, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let hatched_at = request
            .now
            .checked_add(request.hatch_seconds)
            .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
        let evolution_due_at = hatched_at
            .checked_add(ADULT_AGE_SECONDS)
            .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let in_brawl = transaction
            .query_row(
                "SELECT 1 FROM pet_brawls
                  WHERE guild_id = ?1
                    AND status IN ('pending', 'active')
                    AND (challenger_id = ?2 OR recipient_id = ?2)
                  LIMIT 1",
                params![guild_id, request.discord_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if in_brawl {
            return Err(PetRepositoryFailure::InBrawl.into());
        }

        let changed = transaction.execute(
            "UPDATE pets
                SET died_at = ?1,
                    death_cause = 'sacrifice',
                    evolved_at = CASE WHEN evolution_due_at < ?1 THEN evolved_at ELSE NULL END,
                    evolution_calling = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_calling ELSE NULL END,
                    evolution_primary = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_primary ELSE NULL END,
                    evolution_secondary = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_secondary ELSE NULL END,
                    evolution_announced_at = CASE
                        WHEN evolution_due_at < ?1 THEN evolution_announced_at ELSE NULL END
              WHERE pet_id = ?2 AND guild_id = ?3 AND discord_id = ?4
                AND died_at IS NULL
                AND last_fed_at = ?5 AND hunger_at_last_fed = ?6",
            params![
                request.now,
                request.old_pet_id,
                guild_id,
                request.discord_id,
                request.expected_last_fed_at,
                request.expected_hunger,
            ],
        )?;
        if changed != 1 {
            let died_at = transaction
                .query_row(
                    "SELECT died_at FROM pets
                      WHERE pet_id = ?1 AND guild_id = ?2 AND discord_id = ?3",
                    params![request.old_pet_id, guild_id, request.discord_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()?;
            return Err(match died_at {
                None => PetRepositoryFailure::NoPet,
                Some(Some(_)) => PetRepositoryFailure::PetDead,
                Some(None) => PetRepositoryFailure::StalePet,
            }
            .into());
        }

        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            request.fee,
            DebitContext {
                related_type: "pet_sacrifice",
                related_id: request.discord_id.to_string(),
                reason: format!("altar sacrifice, adopted {}", request.name),
                metadata: Some(format!(
                    "{{\"old_pet_id\": {}, \"species\": {}, \"fee\": {}}}",
                    request.old_pet_id,
                    json_string(request.species),
                    request.fee
                )),
            },
        )?;
        let inserted = transaction.execute(
            "INSERT INTO pets (
                 discord_id, guild_id, name, species, adopted_at, hatched_at,
                 adopt_fee, last_fed_at, hunger_at_last_fed, dig_work_units,
                 dig_work_at, evolution_started_at, evolution_due_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, 100, 0, ?6, ?6, ?8)",
            params![
                request.discord_id,
                guild_id,
                request.name,
                request.species,
                request.now,
                hatched_at,
                request.fee,
                evolution_due_at,
            ],
        );
        if let Err(error) = inserted {
            if is_constraint_violation(&error) {
                return Err(PetRepositoryFailure::AlreadyHasPet.into());
            }
            return Err(error.into());
        }
        let new_pet_id = transaction.last_insert_rowid();
        let dead_pet = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1"),
            [request.old_pet_id],
        )?;
        let new_pet = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1"),
            [new_pet_id],
        )?;
        transaction.commit()?;
        Ok(SacrificePetOutcome { dead_pet, new_pet })
    }

    pub fn resolve_hatch_species(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        species: &str,
        now: i64,
    ) -> Result<Pet, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pet = optional_pet(
            &transaction,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE pet_id = ?1 AND guild_id = ?2 AND died_at IS NULL"
            ),
            params![pet_id, guild_id],
        )?
        .ok_or(PetRepositoryFailure::NoPet)?;
        if pet.species != UNHATCHED_SPECIES {
            transaction.commit()?;
            return Ok(pet);
        }
        if now < pet.hatched_at {
            return Err(PetRepositoryFailure::EggNotReady.into());
        }
        transaction.execute(
            "UPDATE pets SET species = ?1
             WHERE pet_id = ?2 AND guild_id = ?3 AND died_at IS NULL AND species = ?4",
            params![species, pet_id, guild_id, UNHATCHED_SPECIES],
        )?;
        let resolved = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1 AND guild_id = ?2"),
            params![pet_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(resolved)
    }

    pub fn buy_supplies(
        &self,
        request: &BuySuppliesRequest<'_>,
    ) -> Result<i64, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT qty FROM pet_supplies
                 WHERE discord_id = ?1 AND guild_id = ?2 AND item_id = ?3",
                params![request.discord_id, guild_id, request.item_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let new_quantity = current
            .checked_add(request.qty)
            .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
        if new_quantity > request.stack_cap {
            return Err(PetRepositoryFailure::StackCap.into());
        }
        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            request.total_cost,
            DebitContext {
                related_type: "pet_supplies",
                related_id: request.discord_id.to_string(),
                reason: format!("bought {}x {}", request.qty, request.item_id),
                metadata: None,
            },
        )?;
        transaction.execute(
            "INSERT INTO pet_supplies (discord_id, guild_id, item_id, qty, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(discord_id, guild_id, item_id) DO UPDATE SET
                 qty = qty + excluded.qty,
                 updated_at = excluded.updated_at",
            params![
                request.discord_id,
                guild_id,
                request.item_id,
                request.qty,
                request.now
            ],
        )?;
        transaction.commit()?;
        Ok(new_quantity)
    }

    pub fn feed_pet(&self, request: &FeedPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = transaction
            .query_row(
                "SELECT died_at, last_fed_at, hunger_at_last_fed, feeds_today,
                        feed_date, dig_work_units, dig_work_at
                 FROM pets WHERE pet_id = ?1 AND guild_id = ?2",
                params![request.pet_id, guild_id],
                |row| {
                    Ok(FeedState {
                        died_at: row.get(0)?,
                        last_fed_at: row.get(1)?,
                        hunger: row.get(2)?,
                        feeds_today: row.get(3)?,
                        feed_date: row.get(4)?,
                        dig_work_units: row.get(5)?,
                        dig_work_at: row.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or(PetRepositoryFailure::NoPet)?;
        if state.died_at.is_some() {
            return Err(PetRepositoryFailure::PetDead.into());
        }
        if !anchor_matches(
            state.last_fed_at,
            state.hunger,
            state.dig_work_units,
            state.dig_work_at,
            request.anchor,
        ) {
            return Err(PetRepositoryFailure::StalePet.into());
        }
        let feeds_today = if state.feed_date.as_deref() == Some(request.feed_date) {
            state.feeds_today
        } else {
            0
        };
        if feeds_today >= request.feed_cap {
            return Err(PetRepositoryFailure::FeedCap.into());
        }
        let removed = transaction.execute(
            "UPDATE pet_supplies SET qty = qty - 1, updated_at = ?1
             WHERE discord_id = ?2 AND guild_id = ?3 AND item_id = ?4 AND qty >= 1",
            params![
                request.new_last_fed_at,
                request.discord_id,
                guild_id,
                request.item_id
            ],
        )?;
        if removed == 0 {
            return Err(PetRepositoryFailure::NoSupplies.into());
        }
        transaction.execute(
            "UPDATE pets SET
                 last_fed_at = CASE WHEN ?1 THEN last_fed_at ELSE ?2 END,
                 hunger_at_last_fed = CASE WHEN ?1 THEN hunger_at_last_fed ELSE ?3 END,
                 times_fed = times_fed + (CASE WHEN ?1 THEN 0 ELSE 1 END),
                 feeds_today = ?4,
                 feed_date = ?5,
                 dig_work_units = COALESCE(?6, dig_work_units),
                 dig_work_at = COALESCE(?7, dig_work_at),
                 prev_week_key = CASE
                     WHEN week_key = ?8 OR week_key IS NULL THEN prev_week_key ELSE week_key END,
                 prev_week_consumed_jc = CASE
                     WHEN week_key = ?8 OR week_key IS NULL
                     THEN prev_week_consumed_jc ELSE week_consumed_jc END,
                 week_consumed_jc = CASE
                     WHEN week_key = ?8 THEN week_consumed_jc + ?9 ELSE ?9 END,
                 week_key = ?8
             WHERE pet_id = ?10 AND guild_id = ?11",
            params![
                request.spat,
                request.new_last_fed_at,
                request.new_hunger,
                feeds_today + 1,
                request.feed_date,
                request.anchor.new_dig_work_units,
                request.anchor.new_dig_work_at,
                request.week_key,
                request.consumed_jc,
                request.pet_id,
                guild_id,
            ],
        )?;
        let pet = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1"),
            [request.pet_id],
        )?;
        transaction.commit()?;
        Ok(pet)
    }

    pub fn rename_pet(
        &self,
        pet_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        new_name: &str,
        cost: i64,
    ) -> Result<(), PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_living_pet(&transaction, pet_id, guild_id)?;
        debit_player(
            &transaction,
            discord_id,
            guild_id,
            cost,
            DebitContext {
                related_type: "pet_rename",
                related_id: discord_id.to_string(),
                reason: format!("renamed pet to {new_name}"),
                metadata: None,
            },
        )?;
        transaction.execute(
            "UPDATE pets SET name = ?1 WHERE pet_id = ?2 AND guild_id = ?3",
            params![new_name, pet_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn use_salt_lick(&self, request: &SaltLickRequest<'_>) -> Result<Pet, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = transaction
            .query_row(
                "SELECT died_at, pampered_until FROM pets WHERE pet_id = ?1 AND guild_id = ?2",
                params![request.pet_id, guild_id],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .ok_or(PetRepositoryFailure::NoPet)?;
        if state.0.is_some() {
            return Err(PetRepositoryFailure::PetDead.into());
        }
        if state.1.is_some_and(|until| until > request.now) {
            return Err(PetRepositoryFailure::AlreadyPampered.into());
        }
        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            request.cost,
            DebitContext {
                related_type: "pet_salt_lick",
                related_id: request.discord_id.to_string(),
                reason: "salt lick".to_owned(),
                metadata: None,
            },
        )?;
        let anchor = request.anchor;
        let changed = transaction.execute(
            "UPDATE pets SET
                 pampered_until = ?1,
                 dig_work_units = COALESCE(?2, dig_work_units),
                 dig_work_at = COALESCE(?3, dig_work_at),
                 prev_week_key = CASE
                     WHEN week_key = ?4 OR week_key IS NULL THEN prev_week_key ELSE week_key END,
                 prev_week_consumed_jc = CASE
                     WHEN week_key = ?4 OR week_key IS NULL
                     THEN prev_week_consumed_jc ELSE week_consumed_jc END,
                 week_consumed_jc = CASE
                     WHEN week_key = ?4 THEN week_consumed_jc + ?5 ELSE ?5 END,
                 week_key = ?4
             WHERE pet_id = ?6 AND guild_id = ?7
               AND (?8 IS NULL OR last_fed_at = ?8)
               AND (?9 IS NULL OR hunger_at_last_fed = ?9)
               AND (?10 IS NULL OR dig_work_units = ?10)
               AND (?11 IS NULL OR dig_work_at = ?11)",
            params![
                request.pampered_until,
                anchor.and_then(|value| value.new_dig_work_units),
                anchor.and_then(|value| value.new_dig_work_at),
                request.week_key,
                request.cost,
                request.pet_id,
                guild_id,
                anchor.map(|value| value.expected_last_fed_at),
                anchor.map(|value| value.expected_hunger),
                anchor.and_then(|value| value.expected_dig_work_units),
                anchor.and_then(|value| value.expected_dig_work_at),
            ],
        )?;
        if changed == 0 {
            return Err(PetRepositoryFailure::StalePet.into());
        }
        let pet = required_pet(
            &transaction,
            &format!("SELECT {PET_COLUMNS} FROM pets WHERE pet_id = ?1"),
            [request.pet_id],
        )?;
        transaction.commit()?;
        Ok(pet)
    }

    pub fn buy_trinket_atomic(
        &self,
        request: &TrinketPurchaseRequest<'_>,
    ) -> Result<TrinketPurchaseOutcome, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_living_pet(&transaction, request.pet_id, guild_id)?;
        let duplicate = transaction
            .query_row(
                "SELECT 1 FROM pet_accessories
                 WHERE discord_id = ?1 AND guild_id = ?2 AND accessory_id = ?3",
                params![request.discord_id, guild_id, request.accessory_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let net_cost = if duplicate {
            request.cost.saturating_sub(request.refund).max(0)
        } else {
            request.cost
        };
        debit_player(
            &transaction,
            request.discord_id,
            guild_id,
            net_cost,
            DebitContext {
                related_type: "pet_trinket",
                related_id: request.discord_id.to_string(),
                reason: format!("mystery trinket roll: {}", request.accessory_id),
                metadata: None,
            },
        )?;
        if !duplicate {
            transaction.execute(
                "INSERT INTO pet_accessories
                     (discord_id, guild_id, accessory_id, obtained_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    request.discord_id,
                    guild_id,
                    request.accessory_id,
                    request.now
                ],
            )?;
            transaction.execute(
                "UPDATE pets SET accessory = ?1
                 WHERE pet_id = ?2 AND guild_id = ?3 AND died_at IS NULL",
                params![request.accessory_id, request.pet_id, guild_id],
            )?;
        }
        let owned_count = transaction.query_row(
            "SELECT COUNT(*) FROM pet_accessories WHERE discord_id = ?1 AND guild_id = ?2",
            params![request.discord_id, guild_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(TrinketPurchaseOutcome {
            duplicate,
            net_cost,
            owned_count,
        })
    }

    pub fn get_accessories(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<String>, PetRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT accessory_id FROM pet_accessories
             WHERE discord_id = ?1 AND guild_id = ?2 ORDER BY obtained_at",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn species_raised(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Vec<String>, PetRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT species FROM pets
             WHERE discord_id=?1 AND guild_id=?2 AND hatched_at<=?3
             ORDER BY species",
        )?;
        Ok(statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id), now],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?)
    }

    pub fn recent_dead_species(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<String>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT species FROM pets
             WHERE discord_id=?1 AND guild_id=?2 AND died_at IS NOT NULL
               AND COALESCE(death_cause,'') NOT IN ('sacrifice','eaten')
             ORDER BY adopted_at DESC LIMIT ?3",
        )?;
        Ok(statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id), limit],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?)
    }

    pub fn equip_accessory(
        &self,
        pet_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: Option<&str>,
    ) -> Result<(), PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_living_pet(&transaction, pet_id, guild_id)?;
        if let Some(accessory_id) = accessory_id {
            let owned = transaction
                .query_row(
                    "SELECT 1 FROM pet_accessories
                     WHERE discord_id = ?1 AND guild_id = ?2 AND accessory_id = ?3",
                    params![discord_id, guild_id, accessory_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !owned {
                return Err(PetRepositoryFailure::NotOwned.into());
            }
        }
        transaction.execute(
            "UPDATE pets SET accessory = ?1 WHERE pet_id = ?2 AND guild_id = ?3",
            params![accessory_id, pet_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_death(&self, request: &DeathClaimRequest<'_>) -> Result<bool, PetRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let changed = connection.execute(
            "UPDATE pets SET died_at = ?1, death_cause = ?2,
                 dig_work_units = COALESCE(?3, dig_work_units),
                 dig_work_at = COALESCE(?4, dig_work_at)
             WHERE pet_id = ?5 AND guild_id = ?6 AND died_at IS NULL
               AND last_fed_at = ?7 AND hunger_at_last_fed = ?8
               AND (?9 IS NULL OR dig_work_units = ?9)
               AND (?10 IS NULL OR dig_work_at = ?10)",
            params![
                request.died_at,
                request.cause,
                request.anchor.new_dig_work_units,
                request.anchor.new_dig_work_at,
                request.pet_id,
                guild_id,
                request.anchor.expected_last_fed_at,
                request.anchor.expected_hunger,
                request.anchor.expected_dig_work_units,
                request.anchor.expected_dig_work_at,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Move the hunger anchor after a match reward while rejecting a stale
    /// snapshot. A concurrent feed/evolution/death wins cleanly and returns
    /// `false`, matching Python's optimistic update contract.
    pub fn apply_hunger_top_up(
        &self,
        request: HungerTopUpRequest,
    ) -> Result<bool, PetRepositoryError> {
        let anchor = request.anchor;
        Ok(self.connection()?.execute(
            "UPDATE pets SET
                 last_fed_at=?1,
                 hunger_at_last_fed=?2,
                 dig_work_units=COALESCE(?3,dig_work_units),
                 dig_work_at=COALESCE(?4,dig_work_at)
             WHERE pet_id=?5 AND guild_id=?6 AND died_at IS NULL
               AND last_fed_at=?7 AND hunger_at_last_fed=?8
               AND (?9 IS NULL OR dig_work_units=?9)
               AND (?10 IS NULL OR dig_work_at=?10)",
            params![
                request.new_last_fed_at,
                request.new_hunger,
                anchor.new_dig_work_units,
                anchor.new_dig_work_at,
                request.pet_id,
                Self::normalize_guild_id(request.guild_id),
                anchor.expected_last_fed_at,
                anchor.expected_hunger,
                anchor.expected_dig_work_units,
                anchor.expected_dig_work_at,
            ],
        )? == 1)
    }

    pub fn revive_with_aegis(
        &self,
        request: AegisReviveRequest,
    ) -> Result<bool, PetRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let changed = connection.execute(
            "UPDATE pets SET
                 aegis_used = 1,
                 last_fed_at = ?1,
                 hunger_at_last_fed = ?2,
                 dig_work_units = COALESCE(?3, dig_work_units),
                 dig_work_at = COALESCE(?4, dig_work_at)
             WHERE pet_id = ?5 AND guild_id = ?6 AND died_at IS NULL AND aegis_used = 0
               AND last_fed_at = ?7 AND hunger_at_last_fed = ?8
               AND (?9 IS NULL OR dig_work_units = ?9)
               AND (?10 IS NULL OR dig_work_at = ?10)",
            params![
                request.revived_last_fed_at,
                request.revive_hunger,
                request.anchor.new_dig_work_units,
                request.anchor.new_dig_work_at,
                request.pet_id,
                guild_id,
                request.anchor.expected_last_fed_at,
                request.anchor.expected_hunger,
                request.anchor.expected_dig_work_units,
                request.anchor.expected_dig_work_at,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn find_starvation_candidates(
        &self,
        now: i64,
        decay_per_day: i64,
        max_decay_pct: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        let denominator = i128::from(decay_per_day) * i128::from(max_decay_pct);
        let denominator = denominator.max(1);
        let seconds = i128::from(86_400 * 100 * 100) / denominator;
        let seconds =
            i64::try_from(seconds).map_err(|_| PetRepositoryError::ArithmeticOutOfRange)?;
        let connection = self.connection()?;
        query_pets(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE died_at IS NULL
                   AND last_fed_at + (hunger_at_last_fed * ?1 / 100) <= ?2"
            ),
            params![seconds, now],
        )
    }

    pub fn find_unannounced_hatches(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        query_pets(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE died_at IS NULL AND hatch_announced_at IS NULL AND hatched_at <= ?1
                 ORDER BY hatched_at, pet_id LIMIT ?2"
            ),
            params![now, limit],
        )
    }

    pub fn mark_hatch_announced(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE pets SET hatch_announced_at = ?1 WHERE pet_id = ?2 AND guild_id = ?3",
            params![announced_at, pet_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    pub fn get_unannounced_deaths(&self, limit: i64) -> Result<Vec<Pet>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        query_pets(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE died_at IS NOT NULL AND death_announced_at IS NULL
                 ORDER BY died_at, pet_id LIMIT ?1"
            ),
            [limit],
        )
    }

    pub fn mark_death_announced(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE pets SET death_announced_at = ?1
             WHERE pet_id = ?2 AND guild_id = ?3 AND died_at IS NOT NULL",
            params![announced_at, pet_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(())
    }

    pub fn list_pet_guild_ids(&self) -> Result<Vec<i64>, PetRepositoryError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT DISTINCT guild_id FROM pets WHERE died_at IS NULL")?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_week_care(
        &self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        let connection = self.connection()?;
        query_pets(
            &connection,
            &format!(
                "SELECT {PET_COLUMNS} FROM pets
                 WHERE guild_id = ?1 AND died_at IS NULL AND (
                     (week_key = ?2 AND week_consumed_jc > 0) OR
                     (prev_week_key = ?2 AND prev_week_consumed_jc > 0)
                 )"
            ),
            params![Self::normalize_guild_id(guild_id), week_key],
        )
    }

    pub fn is_refund_window_paid(
        &self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<bool, PetRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM pet_refund_windows WHERE guild_id = ?1 AND week_key = ?2",
                params![Self::normalize_guild_id(guild_id), week_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn get_nonprofit_balance(&self, guild_id: Option<i64>) -> Result<i64, PetRepositoryError> {
        let connection = self.connection()?;
        let balance = connection
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or(0);
        Ok(balance)
    }

    pub fn pay_refunds_atomic(
        &self,
        guild_id: Option<i64>,
        week_key: &str,
        payouts: &[(i64, i64)],
        now: i64,
        announcement: Option<&RefundNotice>,
    ) -> Result<bool, PetRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let total = payouts.iter().try_fold(0_i64, |total, (_, amount)| {
            total
                .checked_add(*amount)
                .ok_or(PetRepositoryError::ArithmeticOutOfRange)
        })?;
        let announcement_payload = announcement.map(serialize_refund_announcement);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO pet_refund_windows
                 (guild_id, week_key, paid_at, total_paid, announcement_payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![guild_id, week_key, now, total, announcement_payload],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        if total <= 0 {
            transaction.commit()?;
            return Ok(true);
        }

        set_ledger_context(
            &transaction,
            "pet_refund",
            None,
            "pet_refund",
            week_key,
            &format!("weekly pet care refund {week_key}"),
            None,
        )?;
        let debited = transaction.execute(
            "UPDATE nonprofit_fund
             SET total_collected = total_collected - ?1, updated_at = CURRENT_TIMESTAMP
             WHERE guild_id = ?2 AND total_collected >= ?1",
            params![total, guild_id],
        )?;
        if debited == 0 {
            clear_ledger_context(&transaction)?;
            return Err(PetRepositoryFailure::FundShort.into());
        }
        let mut unpaid = 0_i64;
        for (discord_id, amount) in payouts.iter().copied() {
            if amount <= 0 {
                continue;
            }
            let credited = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![amount, discord_id, guild_id],
            )?;
            if credited == 0 {
                unpaid = unpaid
                    .checked_add(amount)
                    .ok_or(PetRepositoryError::ArithmeticOutOfRange)?;
            }
        }
        if unpaid != 0 {
            transaction.execute(
                "UPDATE nonprofit_fund
                 SET total_collected = total_collected + ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE guild_id = ?2",
                params![unpaid, guild_id],
            )?;
            transaction.execute(
                "UPDATE pet_refund_windows SET total_paid = total_paid - ?1
                 WHERE guild_id = ?2 AND week_key = ?3",
                params![unpaid, guild_id, week_key],
            )?;
        }
        clear_ledger_context(&transaction)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn get_unannounced_refunds(
        &self,
        limit: i64,
    ) -> Result<Vec<RefundNotice>, PetRepositoryError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let rows = {
            let mut statement = connection.prepare(
                "SELECT guild_id, week_key, total_paid, announcement_payload
                 FROM pet_refund_windows
                 WHERE announcement_payload IS NOT NULL AND announced_at IS NULL
                 ORDER BY paid_at, guild_id, week_key LIMIT ?1",
            )?;
            let mapped = statement.query_map([limit], |row| {
                Ok(RefundRow {
                    guild_id: row.get(0)?,
                    week_key: row.get(1)?,
                    total_paid: row.get(2)?,
                    payload: row.get(3)?,
                })
            })?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(|row| parse_refund_notice(&connection, row))
            .collect()
    }

    pub fn mark_refund_announced(
        &self,
        guild_id: Option<i64>,
        week_key: &str,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE pet_refund_windows SET announced_at = ?1
             WHERE guild_id = ?2 AND week_key = ?3 AND announced_at IS NULL",
            params![announced_at, Self::normalize_guild_id(guild_id), week_key],
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct FeedState {
    died_at: Option<i64>,
    last_fed_at: i64,
    hunger: i64,
    feeds_today: i64,
    feed_date: Option<String>,
    dig_work_units: i64,
    dig_work_at: i64,
}

#[derive(Debug)]
struct RefundRow {
    guild_id: i64,
    week_key: String,
    total_paid: i64,
    payload: String,
}

fn anchor_matches(
    last_fed_at: i64,
    hunger: i64,
    dig_work_units: i64,
    dig_work_at: i64,
    anchor: AnchorGuard,
) -> bool {
    last_fed_at == anchor.expected_last_fed_at
        && hunger == anchor.expected_hunger
        && anchor
            .expected_dig_work_units
            .is_none_or(|expected| dig_work_units == expected)
        && anchor
            .expected_dig_work_at
            .is_none_or(|expected| dig_work_at == expected)
}

fn require_living_pet(
    transaction: &Transaction<'_>,
    pet_id: i64,
    guild_id: i64,
) -> Result<(), PetRepositoryError> {
    let died_at = transaction
        .query_row(
            "SELECT died_at FROM pets WHERE pet_id = ?1 AND guild_id = ?2",
            params![pet_id, guild_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .ok_or(PetRepositoryFailure::NoPet)?;
    if died_at.is_some() {
        return Err(PetRepositoryFailure::PetDead.into());
    }
    Ok(())
}

struct DebitContext<'a> {
    related_type: &'a str,
    related_id: String,
    reason: String,
    metadata: Option<String>,
}

fn debit_player(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    amount: i64,
    context: DebitContext<'_>,
) -> Result<(), PetRepositoryError> {
    if amount <= 0 {
        return Ok(());
    }
    set_ledger_context(
        transaction,
        "pet",
        Some(discord_id),
        context.related_type,
        &context.related_id,
        &context.reason,
        context.metadata.as_deref(),
    )?;
    let result = transaction.execute(
        "UPDATE players
         SET jopacoin_balance = jopacoin_balance - ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3
           AND COALESCE(jopacoin_balance, 0) >= ?1",
        params![amount, discord_id, guild_id],
    );
    let cleared = clear_ledger_context(transaction);
    let changed = result?;
    cleared?;
    if changed == 0 {
        return Err(PetRepositoryFailure::InsufficientFunds.into());
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: Option<i64>,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: Option<&str>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![source, actor_id, related_type, related_id, reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn optional_pet<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Option<Pet>, PetRepositoryError>
where
    P: rusqlite::Params,
{
    connection
        .query_row(sql, parameters, pet_row)
        .optional()?
        .map(|row| Pet::from_row(&row).map_err(Into::into))
        .transpose()
}

fn required_pet<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Pet, PetRepositoryError>
where
    P: rusqlite::Params,
{
    let row = connection.query_row(sql, parameters, pet_row)?;
    Pet::from_row(&row).map_err(Into::into)
}

fn query_pets<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<Pet>, PetRepositoryError>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(parameters, pet_row)?;
    rows.map(|row| {
        let row = row?;
        Pet::from_row(&row).map_err(Into::into)
    })
    .collect()
}

fn pet_row(row: &Row<'_>) -> Result<PetRow, rusqlite::Error> {
    let mut result = PetRow::new();
    for (index, column) in PET_COLUMN_NAMES.into_iter().enumerate() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => SqliteValue::Null,
            ValueRef::Integer(value) => SqliteValue::Integer(value),
            ValueRef::Text(value) => SqliteValue::Text(
                std::str::from_utf8(value)
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            index,
                            Type::Text,
                            Box::new(error),
                        )
                    })?
                    .to_owned(),
            ),
            ValueRef::Real(_) => {
                return Err(unsupported_pet_value(index, Type::Real, column));
            }
            ValueRef::Blob(_) => {
                return Err(unsupported_pet_value(index, Type::Blob, column));
            }
        };
        result.insert(column.to_owned(), value);
    }
    Ok(result)
}

#[derive(Debug)]
struct UnsupportedPetValue(&'static str);

impl fmt::Display for UnsupportedPetValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported SQLite storage class for pet column {}",
            self.0
        )
    }
}

impl StdError for UnsupportedPetValue {}

fn unsupported_pet_value(index: usize, kind: Type, column: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, kind, Box::new(UnsupportedPetValue(column)))
}

fn serialize_refund_announcement(notice: &RefundNotice) -> String {
    let payouts = notice
        .payouts
        .iter()
        .map(|payout| {
            format!(
                "{{\"amount\": {}, \"consumed_jc\": {}, \"discord_id\": {}, \
                 \"multiplier_pct\": {}}}",
                payout.amount, payout.consumed_jc, payout.discord_id, payout.multiplier_pct
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"payouts\": [{payouts}], \"scaled_down\": {}}}",
        if notice.scaled_down { "true" } else { "false" }
    )
}

fn parse_refund_notice(
    connection: &Connection,
    row: RefundRow,
) -> Result<RefundNotice, PetRepositoryError> {
    let valid = connection.query_row("SELECT json_valid(?1)", [&row.payload], |row| {
        row.get::<_, i64>(0)
    })? != 0;
    if !valid {
        return Ok(RefundNotice {
            guild_id: row.guild_id,
            week_key: row.week_key,
            payouts: Vec::new(),
            total_paid: row.total_paid,
            scaled_down: false,
        });
    }
    let scaled_down = connection.query_row(
        "SELECT COALESCE(json_extract(?1, '$.scaled_down'), 0)",
        [&row.payload],
        |row| row.get::<_, i64>(0),
    )? != 0;
    let mut statement = connection.prepare(
        "SELECT
             CAST(json_extract(value, '$.discord_id') AS INTEGER),
             CAST(json_extract(value, '$.consumed_jc') AS INTEGER),
             CAST(json_extract(value, '$.multiplier_pct') AS INTEGER),
             CAST(json_extract(value, '$.amount') AS INTEGER)
         FROM json_each(?1, '$.payouts')",
    )?;
    let payouts = statement
        .query_map([&row.payload], |entry| {
            Ok(RefundPayout {
                discord_id: entry.get(0)?,
                consumed_jc: entry.get(1)?,
                multiplier_pct: entry.get(2)?,
                amount: entry.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RefundNotice {
        guild_id: row.guild_id,
        week_key: row.week_key,
        payouts,
        total_paid: row.total_paid,
        scaled_down,
    })
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _) if inner.code == ErrorCode::ConstraintViolation
    )
}

fn json_string(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character if character.is_control() => {
                use fmt::Write;
                let _ = write!(result, "\\u{:04x}", u32::from(character));
            }
            character => result.push(character),
        }
    }
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use cama_domain::pet::{Pet, RefundNotice, RefundPayout};
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::{
        AdoptPetRequest, AegisReviveRequest, AnchorGuard, BuySuppliesRequest, DeathClaimRequest,
        FeedPetRequest, HungerTopUpRequest, PetRepository, PetRepositoryError,
        PetRepositoryFailure, SacrificePetRequest, SaltLickRequest, TrinketPurchaseOutcome,
        TrinketPurchaseRequest, UpgradeEggRequest,
    };

    const NOW: i64 = 1_800_000_000;
    const HATCH: i64 = 86_400;
    const DAY: i64 = 86_400;
    const WEEK: &str = "2026-W30";
    const TODAY: &str = "2026-07-26";
    const TEST_GUILD_ID: i64 = 987_654_321;

    struct Fixture {
        file: NamedTempFile,
        repository: PetRepository,
    }

    #[derive(Clone, Copy)]
    struct AdoptOptions {
        discord_id: i64,
        guild_id: Option<i64>,
        name: &'static str,
        egg_tier: &'static str,
        fee: i64,
        now: i64,
        hatch_seconds: i64,
    }

    impl Default for AdoptOptions {
        fn default() -> Self {
            Self {
                discord_id: 100,
                guild_id: Some(TEST_GUILD_ID),
                name: "Blep",
                egg_tier: "standard",
                fee: 20,
                now: NOW,
                hatch_seconds: HATCH,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct FeedOptions {
        new_last_fed_at: i64,
        new_hunger: i64,
        feed_date: &'static str,
        feed_cap: i64,
        week_key: &'static str,
        consumed_jc: i64,
        spat: bool,
        expected_dig_work_units: Option<i64>,
        expected_dig_work_at: Option<i64>,
        new_dig_work_units: Option<i64>,
        new_dig_work_at: Option<i64>,
    }

    impl Default for FeedOptions {
        fn default() -> Self {
            Self {
                new_last_fed_at: NOW + HATCH + 100,
                new_hunger: 100,
                feed_date: TODAY,
                feed_cap: 4,
                week_key: WEEK,
                consumed_jc: 5,
                spat: false,
                expected_dig_work_units: None,
                expected_dig_work_at: None,
                new_dig_work_units: None,
                new_dig_work_at: None,
            }
        }
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("create temporary SQLite file");
            let connection = Connection::open(file.path()).expect("open temporary SQLite file");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id       INTEGER NOT NULL,
                         guild_id         INTEGER NOT NULL DEFAULT 0,
                         discord_username TEXT NOT NULL,
                         jopacoin_balance  INTEGER DEFAULT 3,
                         updated_at        TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE nonprofit_fund (
                         guild_id        INTEGER PRIMARY KEY DEFAULT 0,
                         total_collected INTEGER DEFAULT 0,
                         updated_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                     );
                     CREATE TABLE economy_ledger_entries (
                         ledger_id      INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id       INTEGER NOT NULL,
                         account_type   TEXT NOT NULL,
                         account_id     INTEGER,
                         delta          INTEGER NOT NULL,
                         balance_before INTEGER NOT NULL,
                         balance_after  INTEGER NOT NULL,
                         source         TEXT NOT NULL DEFAULT 'balance_update',
                         actor_id       INTEGER,
                         related_type   TEXT,
                         related_id     TEXT,
                         reason         TEXT,
                         metadata       TEXT,
                         created_at     INTEGER NOT NULL
                             DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                     );
                     CREATE TABLE economy_ledger_context (
                         id           INTEGER PRIMARY KEY CHECK(id = 1),
                         source       TEXT,
                         actor_id     INTEGER,
                         related_type TEXT,
                         related_id   TEXT,
                         reason       TEXT,
                         metadata     TEXT
                     );
                     CREATE TRIGGER trg_test_ledger_players_update
                     AFTER UPDATE OF jopacoin_balance ON players
                     WHEN COALESCE(OLD.jopacoin_balance, 0)
                          != COALESCE(NEW.jopacoin_balance, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             NEW.guild_id, 'player', NEW.discord_id,
                             COALESCE(NEW.jopacoin_balance, 0)
                                 - COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(NEW.jopacoin_balance, 0),
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                      'balance_update'),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_test_ledger_nonprofit_update
                     AFTER UPDATE OF total_collected ON nonprofit_fund
                     WHEN COALESCE(OLD.total_collected, 0)
                          != COALESCE(NEW.total_collected, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             NEW.guild_id, 'nonprofit', NEW.guild_id,
                             COALESCE(NEW.total_collected, 0)
                                 - COALESCE(OLD.total_collected, 0),
                             COALESCE(OLD.total_collected, 0),
                             COALESCE(NEW.total_collected, 0),
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                      'nonprofit_update'),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TABLE pets (
                         pet_id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id             INTEGER NOT NULL,
                         guild_id               INTEGER NOT NULL,
                         name                   TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 32),
                         species                TEXT NOT NULL,
                         egg_tier               TEXT NOT NULL DEFAULT 'standard'
                             CHECK(egg_tier IN ('standard', 'gilded')),
                         adopted_at             INTEGER NOT NULL,
                         hatched_at             INTEGER NOT NULL,
                         adopt_fee              INTEGER NOT NULL CHECK(adopt_fee >= 0),
                         last_fed_at            INTEGER NOT NULL,
                         hunger_at_last_fed     INTEGER NOT NULL
                             CHECK(hunger_at_last_fed BETWEEN 0 AND 100),
                         times_fed              INTEGER NOT NULL DEFAULT 0,
                         feeds_today            INTEGER NOT NULL DEFAULT 0,
                         feed_date              TEXT,
                         week_consumed_jc       INTEGER NOT NULL DEFAULT 0,
                         week_key               TEXT,
                         prev_week_consumed_jc  INTEGER NOT NULL DEFAULT 0,
                         prev_week_key          TEXT,
                         pampered_until         INTEGER,
                         accessory              TEXT,
                         dig_work_units         INTEGER NOT NULL DEFAULT 0,
                         dig_work_at            INTEGER NOT NULL DEFAULT 0,
                         aegis_used             INTEGER NOT NULL DEFAULT 0,
                         hatch_announced_at     INTEGER,
                         died_at                INTEGER,
                         death_cause            TEXT,
                         death_announced_at     INTEGER,
                         training_xp            INTEGER NOT NULL DEFAULT 0,
                         training_str           INTEGER NOT NULL DEFAULT 0,
                         training_int           INTEGER NOT NULL DEFAULT 0,
                         training_dex           INTEGER NOT NULL DEFAULT 0,
                         solo_training_sessions INTEGER NOT NULL DEFAULT 3,
                         solo_training_recharged_at INTEGER,
                         evolution_started_at   INTEGER,
                         evolution_due_at       INTEGER,
                         evolved_at             INTEGER,
                         evolution_calling      TEXT,
                         evolution_primary      TEXT,
                         evolution_secondary    TEXT,
                         evolution_announced_at INTEGER,
                         CHECK(hatched_at >= adopted_at),
                         CHECK(died_at IS NULL OR died_at >= adopted_at)
                     );
                     CREATE UNIQUE INDEX idx_pets_one_alive
                         ON pets(discord_id, guild_id) WHERE died_at IS NULL;
                     CREATE TABLE pet_supplies (
                         discord_id INTEGER NOT NULL,
                         guild_id   INTEGER NOT NULL,
                         item_id    TEXT NOT NULL,
                         qty        INTEGER NOT NULL DEFAULT 0 CHECK(qty >= 0),
                         updated_at INTEGER NOT NULL,
                         PRIMARY KEY(discord_id, guild_id, item_id)
                     );
                     CREATE TABLE pet_accessories (
                         discord_id  INTEGER NOT NULL,
                         guild_id    INTEGER NOT NULL,
                         accessory_id TEXT NOT NULL,
                         obtained_at INTEGER NOT NULL,
                         PRIMARY KEY(discord_id, guild_id, accessory_id)
                     );
                     CREATE TABLE pet_brawls (
                         brawl_id      INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id     INTEGER NOT NULL,
                         challenger_id INTEGER NOT NULL,
                         recipient_id INTEGER NOT NULL,
                         status       TEXT NOT NULL,
                         expires_at   INTEGER NOT NULL
                     );
                     CREATE TABLE pet_refund_windows (
                         guild_id             INTEGER NOT NULL,
                         week_key             TEXT NOT NULL,
                         paid_at              INTEGER NOT NULL,
                         total_paid           INTEGER NOT NULL CHECK(total_paid >= 0),
                         announcement_payload TEXT,
                         announced_at         INTEGER,
                         PRIMARY KEY(guild_id, week_key)
                     );",
                )
                .expect("create Python-compatible pet repository schema");
            drop(connection);
            let repository = PetRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture database")
        }

        fn seed_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players
                         (discord_id, guild_id, discord_username, jopacoin_balance)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![discord_id, guild_id, format!("Owner {discord_id}"), balance],
                )
                .expect("seed player");
        }

        fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| row.get(0),
                )
                .expect("read player balance")
        }

        fn set_balance(&self, discord_id: i64, guild_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "UPDATE players SET jopacoin_balance = ?1
                     WHERE discord_id = ?2 AND guild_id = ?3",
                    params![balance, discord_id, guild_id],
                )
                .expect("set balance");
        }

        fn ledger_count(&self, source: &str) -> i64 {
            self.connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries WHERE source = ?1",
                    [source],
                    |row| row.get(0),
                )
                .expect("count ledger rows")
        }

        fn seed_nonprofit(&self, guild_id: i64, amount: i64) {
            self.connection()
                .execute(
                    "INSERT INTO nonprofit_fund (guild_id, total_collected)
                     VALUES (?1, ?2)
                     ON CONFLICT(guild_id) DO UPDATE
                         SET total_collected = excluded.total_collected",
                    params![guild_id, amount],
                )
                .expect("seed nonprofit fund");
        }

        fn adopt_result(&self, options: AdoptOptions) -> Result<Pet, PetRepositoryError> {
            self.repository.adopt_pet(&AdoptPetRequest {
                discord_id: options.discord_id,
                guild_id: options.guild_id,
                name: options.name,
                egg_tier: options.egg_tier,
                fee: options.fee,
                now: options.now,
                hatch_seconds: options.hatch_seconds,
            })
        }

        fn adopt(&self, options: AdoptOptions) -> Pet {
            self.adopt_result(options).expect("adopt pet")
        }

        fn adopt_standard(&self) -> Pet {
            self.adopt(AdoptOptions::default())
        }

        fn hatch(&self, pet: &Pet, species: &str) -> Pet {
            self.repository
                .resolve_hatch_species(pet.pet_id, Some(pet.guild_id), species, pet.hatched_at)
                .expect("resolve hatch species")
        }

        fn stock(&self, discord_id: i64, guild_id: Option<i64>, quantity: i64) -> i64 {
            self.repository
                .buy_supplies(&BuySuppliesRequest {
                    discord_id,
                    guild_id,
                    item_id: "tango",
                    qty: quantity,
                    total_cost: quantity * 5,
                    now: NOW,
                    stack_cap: 99,
                })
                .expect("buy supplies")
        }

        fn feed_result(&self, pet: &Pet, options: FeedOptions) -> Result<Pet, PetRepositoryError> {
            self.repository.feed_pet(&FeedPetRequest {
                pet_id: pet.pet_id,
                discord_id: pet.discord_id,
                guild_id: Some(pet.guild_id),
                item_id: "tango",
                anchor: AnchorGuard {
                    expected_last_fed_at: pet.last_fed_at,
                    expected_hunger: pet.hunger_at_last_fed,
                    expected_dig_work_units: options.expected_dig_work_units,
                    expected_dig_work_at: options.expected_dig_work_at,
                    new_dig_work_units: options.new_dig_work_units,
                    new_dig_work_at: options.new_dig_work_at,
                },
                new_last_fed_at: options.new_last_fed_at,
                new_hunger: options.new_hunger,
                feed_date: options.feed_date,
                feed_cap: options.feed_cap,
                week_key: options.week_key,
                consumed_jc: options.consumed_jc,
                spat: options.spat,
            })
        }

        fn feed(&self, pet: &Pet, options: FeedOptions) -> Pet {
            self.feed_result(pet, options).expect("feed pet")
        }

        fn claim_death(&self, pet: &Pet, died_at: i64) -> bool {
            self.repository
                .claim_death(&DeathClaimRequest {
                    pet_id: pet.pet_id,
                    guild_id: Some(pet.guild_id),
                    died_at,
                    cause: "starvation",
                    anchor: AnchorGuard::new(pet.last_fed_at, pet.hunger_at_last_fed),
                })
                .expect("claim death")
        }
    }

    fn rich_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.seed_player(100, TEST_GUILD_ID, 1_000);
        fixture
    }

    fn assert_failure<T: fmt::Debug>(
        result: Result<T, PetRepositoryError>,
        expected: PetRepositoryFailure,
    ) {
        assert_eq!(
            result.expect_err("operation should fail").failure(),
            Some(expected)
        );
    }

    mod adopt {
        use super::*;

        #[test]
        fn test_adopt_debits_fee_and_creates_egg() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert_eq!(pet.hatched_at, NOW + HATCH);
            assert_eq!(pet.hunger_at_last_fed, 100);
            assert_eq!(pet.last_fed_at, pet.hatched_at);
            assert_eq!(pet.species, "unhatched");
            assert_eq!(pet.egg_tier, "standard");
            assert_eq!(pet.dig_work_units, 0);
            assert_eq!(pet.dig_work_at, pet.hatched_at);
            assert_eq!(pet.evolution_started_at, Some(pet.hatched_at));
            assert_eq!(pet.evolution_due_at, Some(pet.hatched_at + 7 * DAY));
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 980);
            assert_eq!(fixture.ledger_count("pet"), 1);
        }

        #[test]
        fn test_adopt_persists_gilded_roll_pool_without_selecting_species() {
            let fixture = rich_fixture();
            let pet = fixture.adopt(AdoptOptions {
                egg_tier: "gilded",
                fee: 250,
                ..AdoptOptions::default()
            });
            assert_eq!(pet.species, "unhatched");
            assert_eq!(pet.egg_tier, "gilded");
        }

        #[test]
        fn test_adopt_insufficient_funds_leaves_no_row_and_no_debit() {
            let fixture = Fixture::new();
            fixture.seed_player(101, TEST_GUILD_ID, 10);
            assert_failure(
                fixture.adopt_result(AdoptOptions {
                    discord_id: 101,
                    ..AdoptOptions::default()
                }),
                PetRepositoryFailure::InsufficientFunds,
            );
            assert!(
                fixture
                    .repository
                    .get_active_pet(101, Some(TEST_GUILD_ID))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fixture.balance(101, TEST_GUILD_ID), 10);
        }

        #[test]
        fn test_adopt_with_living_pet_rejected_and_not_charged() {
            let fixture = rich_fixture();
            fixture.adopt_standard();
            assert_failure(
                fixture.adopt_result(AdoptOptions::default()),
                PetRepositoryFailure::AlreadyHasPet,
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 980);
            assert_eq!(fixture.ledger_count("pet"), 1);
        }

        #[test]
        fn test_dead_pet_does_not_block_re_adopt() {
            let fixture = rich_fixture();
            let first = fixture.adopt_standard();
            assert!(fixture.claim_death(&first, NOW + 9 * DAY));
            let second = fixture.adopt(AdoptOptions {
                name: "Blep II",
                ..AdoptOptions::default()
            });
            assert_ne!(first.pet_id, second.pet_id);
            assert_eq!(
                fixture
                    .repository
                    .count_dead_pets(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                1
            );
        }

        #[test]
        fn test_guild_isolation_and_none_normalization() {
            let fixture = rich_fixture();
            fixture.seed_player(100, 0, 500);
            fixture.adopt_standard();
            let dm = fixture.adopt(AdoptOptions {
                guild_id: None,
                name: "DMcama",
                ..AdoptOptions::default()
            });
            assert_eq!(dm.guild_id, 0);
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, None)
                    .unwrap()
                    .unwrap()
                    .pet_id,
                dm.pet_id
            );
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, Some(0))
                    .unwrap()
                    .unwrap()
                    .pet_id,
                dm.pet_id
            );
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .unwrap()
                    .name,
                "Blep"
            );
        }
    }

    mod sacrifice {
        use super::*;

        fn request<'a>(pet: &Pet, name: &'a str) -> SacrificePetRequest<'a> {
            SacrificePetRequest {
                discord_id: pet.discord_id,
                guild_id: Some(pet.guild_id),
                old_pet_id: pet.pet_id,
                expected_last_fed_at: pet.last_fed_at,
                expected_hunger: pet.hunger_at_last_fed,
                name,
                species: "moon_cama",
                fee: 50,
                now: pet.hatched_at + 3_600,
                hatch_seconds: HATCH,
            }
        }

        #[test]
        fn test_happy_path_is_one_transaction() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");

            let outcome = fixture
                .repository
                .sacrifice_and_adopt_atomic(&request(&pet, "Rebirth"))
                .expect("sacrifice and rebirth");

            assert_eq!(outcome.dead_pet.died_at, Some(pet.hatched_at + 3_600));
            assert_eq!(outcome.dead_pet.death_cause.as_deref(), Some("sacrifice"));
            assert_eq!(outcome.dead_pet.death_announced_at, None);
            assert_eq!(outcome.new_pet.species, "moon_cama");
            assert_eq!(outcome.new_pet.name, "Rebirth");
            assert_eq!(outcome.new_pet.hatched_at, pet.hatched_at + 3_600 + HATCH);
            assert_eq!(outcome.new_pet.hunger_at_last_fed, 100);
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 930);
            let ledger = fixture
                .connection()
                .query_row(
                    "SELECT source,related_type,reason,metadata
                       FROM economy_ledger_entries ORDER BY ledger_id DESC LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .expect("sacrifice ledger");
            assert_eq!(ledger.0, "pet");
            assert_eq!(ledger.1, "pet_sacrifice");
            assert_eq!(ledger.2, "altar sacrifice, adopted Rebirth");
            assert!(ledger.3.contains("moon_cama"));
        }

        #[test]
        fn test_insufficient_funds_rolls_back_the_death() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            let mut request = request(&pet, "Rebirth");
            request.fee = 5_000;

            assert_failure(
                fixture.repository.sacrifice_and_adopt_atomic(&request),
                PetRepositoryFailure::InsufficientFunds,
            );
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, Some(TEST_GUILD_ID))
                    .expect("active pet")
                    .expect("living pet")
                    .pet_id,
                pet.pet_id
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 980);
        }

        #[test]
        fn test_anchor_race_raises_stale_pet() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            let mut request = request(&pet, "Rebirth");
            request.expected_hunger = 42;
            assert_failure(
                fixture.repository.sacrifice_and_adopt_atomic(&request),
                PetRepositoryFailure::StalePet,
            );
        }

        #[test]
        fn test_dead_and_missing_pets_rejected() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            assert!(fixture.claim_death(&pet, pet.hatched_at + 1));
            assert_failure(
                fixture
                    .repository
                    .sacrifice_and_adopt_atomic(&request(&pet, "Rebirth")),
                PetRepositoryFailure::PetDead,
            );
            let mut missing = request(&pet, "Rebirth");
            missing.old_pet_id = 999_999;
            assert_failure(
                fixture.repository.sacrifice_and_adopt_atomic(&missing),
                PetRepositoryFailure::NoPet,
            );
        }

        #[test]
        fn test_sacrifice_egg_cannot_be_gilded() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            let outcome = fixture
                .repository
                .sacrifice_and_adopt_atomic(&request(&pet, "Rebirth"))
                .expect("sacrifice and rebirth");
            assert_failure(
                fixture.repository.upgrade_egg(&UpgradeEggRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    name: "Shiny",
                    premium: 230,
                    now: outcome.new_pet.adopted_at,
                }),
                PetRepositoryFailure::PetHatched,
            );
        }

        #[test]
        fn test_blocked_while_in_brawl() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            fixture
                .connection()
                .execute(
                    "INSERT INTO pet_brawls
                         (guild_id,challenger_id,recipient_id,status,expires_at)
                     VALUES (?1,100,200,'pending',?2)",
                    params![TEST_GUILD_ID, pet.hatched_at + 100],
                )
                .expect("seed pending brawl");
            assert_failure(
                fixture
                    .repository
                    .sacrifice_and_adopt_atomic(&request(&pet, "Rebirth")),
                PetRepositoryFailure::InBrawl,
            );
        }
    }

    mod egg_upgrade_and_hatch {
        use super::*;

        #[test]
        fn test_upgrade_debits_premium_and_changes_only_roll_pool() {
            let fixture = rich_fixture();
            let egg = fixture.adopt_standard();
            let upgraded = fixture
                .repository
                .upgrade_egg(&UpgradeEggRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    name: "Goldie",
                    premium: 230,
                    now: NOW + 1,
                })
                .unwrap();
            assert_eq!(upgraded.pet_id, egg.pet_id);
            assert_eq!(upgraded.name, "Goldie");
            assert_eq!(upgraded.hatched_at, egg.hatched_at);
            assert_eq!(upgraded.species, "unhatched");
            assert_eq!(upgraded.egg_tier, "gilded");
            assert_eq!(upgraded.adopt_fee, 250);
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 750);
            assert_eq!(fixture.ledger_count("pet"), 2);
        }

        #[test]
        fn test_upgrade_failure_rolls_back_state_and_payment() {
            let fixture = rich_fixture();
            let egg = fixture.adopt_standard();
            fixture.set_balance(100, TEST_GUILD_ID, 100);
            assert_failure(
                fixture.repository.upgrade_egg(&UpgradeEggRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    name: "Too Expensive",
                    premium: 230,
                    now: NOW + 1,
                }),
                PetRepositoryFailure::InsufficientFunds,
            );
            let unchanged = fixture
                .repository
                .get_pet_by_id(egg.pet_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(unchanged.name, "Blep");
            assert_eq!(unchanged.egg_tier, "standard");
            assert_eq!(unchanged.adopt_fee, 20);
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 100);
        }

        #[test]
        fn test_upgrade_rejects_gilded_or_hatched_egg() {
            let fixture = rich_fixture();
            let gilded = fixture.adopt(AdoptOptions {
                egg_tier: "gilded",
                fee: 250,
                ..AdoptOptions::default()
            });
            assert_failure(
                fixture.repository.upgrade_egg(&UpgradeEggRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    name: "Again",
                    premium: 230,
                    now: NOW + 1,
                }),
                PetRepositoryFailure::AlreadyGilded,
            );
            assert!(fixture.claim_death(&gilded, NOW + 2));
            let standard = fixture.adopt(AdoptOptions {
                name: "Second",
                ..AdoptOptions::default()
            });
            assert_failure(
                fixture.repository.upgrade_egg(&UpgradeEggRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    name: "Too Late",
                    premium: 230,
                    now: standard.hatched_at,
                }),
                PetRepositoryFailure::PetHatched,
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 730);
        }

        #[test]
        fn test_hatch_resolution_is_ready_gated_and_write_once() {
            let fixture = rich_fixture();
            let egg = fixture.adopt_standard();
            assert_failure(
                fixture.repository.resolve_hatch_species(
                    egg.pet_id,
                    Some(TEST_GUILD_ID),
                    "rama",
                    egg.hatched_at - 1,
                ),
                PetRepositoryFailure::EggNotReady,
            );
            let resolved = fixture
                .repository
                .resolve_hatch_species(egg.pet_id, Some(TEST_GUILD_ID), "rama", egg.hatched_at)
                .unwrap();
            assert_eq!(resolved.species, "rama");
            let retried = fixture
                .repository
                .resolve_hatch_species(
                    egg.pet_id,
                    Some(TEST_GUILD_ID),
                    "common_cama",
                    egg.hatched_at + 1,
                )
                .unwrap();
            assert_eq!(retried.species, "rama");
        }
    }

    mod supplies {
        use super::*;

        #[test]
        fn test_buy_upserts_and_debits() {
            let fixture = rich_fixture();
            assert_eq!(fixture.stock(100, Some(TEST_GUILD_ID), 3), 3);
            assert_eq!(fixture.stock(100, Some(TEST_GUILD_ID), 2), 5);
            assert_eq!(
                fixture
                    .repository
                    .get_supplies(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                BTreeMap::from([("tango".to_owned(), 5)])
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 975);
        }

        #[test]
        fn test_buy_respects_stack_cap_and_rolls_back_debit() {
            let fixture = rich_fixture();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            assert_failure(
                fixture.repository.buy_supplies(&BuySuppliesRequest {
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    item_id: "tango",
                    qty: 95,
                    total_cost: 475,
                    now: NOW,
                    stack_cap: 99,
                }),
                PetRepositoryFailure::StackCap,
            );
            assert_eq!(
                fixture
                    .repository
                    .get_supplies(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                BTreeMap::from([("tango".to_owned(), 5)])
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 975);
        }

        #[test]
        fn test_buy_insufficient_funds() {
            let fixture = Fixture::new();
            fixture.seed_player(102, TEST_GUILD_ID, 3);
            assert_failure(
                fixture.repository.buy_supplies(&BuySuppliesRequest {
                    discord_id: 102,
                    guild_id: Some(TEST_GUILD_ID),
                    item_id: "tango",
                    qty: 1,
                    total_cost: 5,
                    now: NOW,
                    stack_cap: 99,
                }),
                PetRepositoryFailure::InsufficientFunds,
            );
            assert!(
                fixture
                    .repository
                    .get_supplies(102, Some(TEST_GUILD_ID))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    mod feed {
        use super::*;

        #[test]
        fn test_feed_decrements_stock_and_moves_anchor() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            let fed = fixture.feed(
                &pet,
                FeedOptions {
                    new_hunger: 90,
                    ..FeedOptions::default()
                },
            );
            assert_eq!(fed.hunger_at_last_fed, 90);
            assert_eq!(fed.last_fed_at, NOW + HATCH + 100);
            assert_eq!(fed.times_fed, 1);
            assert_eq!(fed.feeds_today, 1);
            assert_eq!(fed.feed_date.as_deref(), Some(TODAY));
            assert_eq!(fed.week_consumed_jc, 5);
            assert_eq!(fed.week_key.as_deref(), Some(WEEK));
            assert_eq!(
                fixture
                    .repository
                    .get_supplies(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                BTreeMap::from([("tango".to_owned(), 4)])
            );
        }

        #[test]
        fn test_feed_without_stock_changes_nothing() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert_failure(
                fixture.feed_result(&pet, FeedOptions::default()),
                PetRepositoryFailure::NoSupplies,
            );
            let fresh = fixture
                .repository
                .get_active_pet(100, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(fresh.last_fed_at, pet.last_fed_at);
            assert_eq!(fresh.times_fed, 0);
        }

        #[test]
        fn test_feed_dead_pet_restores_stock() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            assert!(fixture.claim_death(&pet, NOW + 9 * DAY));
            assert_failure(
                fixture.feed_result(&pet, FeedOptions::default()),
                PetRepositoryFailure::PetDead,
            );
            assert_eq!(
                fixture
                    .repository
                    .get_supplies(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                BTreeMap::from([("tango".to_owned(), 5)])
            );
        }

        #[test]
        fn test_feed_cap_enforced_and_resets_across_game_days() {
            let fixture = rich_fixture();
            let mut current = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 10);
            for index in 0..4 {
                current = fixture.feed(
                    &current,
                    FeedOptions {
                        new_last_fed_at: NOW + HATCH + 100 + index,
                        ..FeedOptions::default()
                    },
                );
            }
            assert_failure(
                fixture.feed_result(
                    &current,
                    FeedOptions {
                        new_last_fed_at: NOW + HATCH + 200,
                        ..FeedOptions::default()
                    },
                ),
                PetRepositoryFailure::FeedCap,
            );
            let after = fixture.feed(
                &current,
                FeedOptions {
                    feed_date: "2026-07-27",
                    new_last_fed_at: NOW + HATCH + 300,
                    ..FeedOptions::default()
                },
            );
            assert_eq!(after.feeds_today, 1);
            assert_eq!(after.feed_date.as_deref(), Some("2026-07-27"));
        }

        #[test]
        fn test_week_accumulator_rolls_over() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            let first = fixture.feed(&pet, FeedOptions::default());
            let second = fixture.feed(
                &first,
                FeedOptions {
                    new_last_fed_at: first.last_fed_at + 10,
                    week_key: "2026-W31",
                    ..FeedOptions::default()
                },
            );
            assert_eq!(first.week_consumed_jc, 5);
            assert_eq!(second.week_consumed_jc, 5);
            assert_eq!(second.week_key.as_deref(), Some("2026-W31"));
            assert_eq!(second.prev_week_key.as_deref(), Some(WEEK));
            assert_eq!(second.prev_week_consumed_jc, 5);
            assert_eq!(second.consumed_in_week(WEEK), 5);
            assert_eq!(
                fixture
                    .repository
                    .list_week_care(Some(TEST_GUILD_ID), WEEK)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [second.pet_id]
            );
        }

        #[test]
        fn test_stale_anchor_rejected() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            fixture.feed(&pet, FeedOptions::default());
            assert_failure(
                fixture.feed_result(&pet, FeedOptions::default()),
                PetRepositoryFailure::StalePet,
            );
        }

        #[test]
        fn test_same_second_pet_work_change_rejects_stale_feed() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            fixture
                .connection()
                .execute(
                    "UPDATE pets SET dig_work_units = ?1 WHERE pet_id = ?2",
                    params![DAY, pet.pet_id],
                )
                .unwrap();
            assert_failure(
                fixture.feed_result(
                    &pet,
                    FeedOptions {
                        expected_dig_work_units: Some(pet.dig_work_units),
                        expected_dig_work_at: Some(pet.dig_work_at),
                        new_dig_work_units: Some(0),
                        new_dig_work_at: Some(pet.dig_work_at),
                        ..FeedOptions::default()
                    },
                ),
                PetRepositoryFailure::StalePet,
            );
        }

        #[test]
        fn test_spat_feed_consumes_item_but_not_hunger() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            let spat = fixture.feed(
                &pet,
                FeedOptions {
                    spat: true,
                    ..FeedOptions::default()
                },
            );
            assert_eq!(spat.hunger_at_last_fed, pet.hunger_at_last_fed);
            assert_eq!(spat.last_fed_at, pet.last_fed_at);
            assert_eq!(spat.times_fed, 0);
            assert_eq!(spat.feeds_today, 1);
            assert_eq!(spat.week_consumed_jc, 5);
            assert_eq!(
                fixture
                    .repository
                    .get_supplies(100, Some(TEST_GUILD_ID))
                    .unwrap(),
                BTreeMap::from([("tango".to_owned(), 4)])
            );
        }
    }

    mod trinkets {
        use super::*;

        fn request<'a>(pet: &Pet, accessory_id: &'a str, now: i64) -> TrinketPurchaseRequest<'a> {
            TrinketPurchaseRequest {
                pet_id: pet.pet_id,
                discord_id: pet.discord_id,
                guild_id: Some(pet.guild_id),
                accessory_id,
                cost: 25,
                refund: 10,
                now,
            }
        }

        #[test]
        fn test_dead_pet_is_never_charged_for_a_trinket() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert!(fixture.claim_death(&pet, NOW + 9 * DAY));
            assert_failure(
                fixture
                    .repository
                    .buy_trinket_atomic(&request(&pet, "top_hat", NOW)),
                PetRepositoryFailure::PetDead,
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 980);
            assert!(
                fixture
                    .repository
                    .get_accessories(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .is_empty()
            );
        }

        #[test]
        fn test_trinket_roll_debits_equips_and_dupe_refunds() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert_eq!(
                fixture
                    .repository
                    .buy_trinket_atomic(&request(&pet, "top_hat", NOW))
                    .unwrap(),
                TrinketPurchaseOutcome {
                    duplicate: false,
                    net_cost: 25,
                    owned_count: 1,
                }
            );
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .unwrap()
                    .accessory
                    .as_deref(),
                Some("top_hat")
            );
            assert_eq!(
                fixture
                    .repository
                    .buy_trinket_atomic(&request(&pet, "top_hat", NOW + 1))
                    .unwrap(),
                TrinketPurchaseOutcome {
                    duplicate: true,
                    net_cost: 15,
                    owned_count: 1,
                }
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 940);
        }

        #[test]
        fn test_equip_raises_distinct_codes() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert_failure(
                fixture.repository.equip_accessory(
                    pet.pet_id,
                    100,
                    Some(TEST_GUILD_ID),
                    Some("red_bow"),
                ),
                PetRepositoryFailure::NotOwned,
            );
            fixture
                .repository
                .buy_trinket_atomic(&request(&pet, "red_bow", NOW))
                .unwrap();
            fixture
                .repository
                .equip_accessory(pet.pet_id, 100, Some(TEST_GUILD_ID), Some("red_bow"))
                .unwrap();
            assert!(fixture.claim_death(&pet, NOW + 9 * DAY));
            assert_failure(
                fixture.repository.equip_accessory(
                    pet.pet_id,
                    100,
                    Some(TEST_GUILD_ID),
                    Some("red_bow"),
                ),
                PetRepositoryFailure::PetDead,
            );
        }
    }

    mod death_and_revival {
        use super::*;

        #[test]
        fn test_claim_death_is_once_only() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert!(fixture.claim_death(&pet, NOW + 777));
            assert!(!fixture.claim_death(&pet, NOW + 999));
            let dead = fixture
                .repository
                .get_pet_by_id(pet.pet_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(dead.died_at, Some(NOW + 777));
            assert_eq!(dead.death_cause.as_deref(), Some("starvation"));
        }

        #[test]
        fn test_aegis_revive_fires_exactly_once() {
            let fixture = rich_fixture();
            let egg = fixture.adopt_standard();
            let pet = fixture.hatch(&egg, "aegis_cama");
            assert!(
                fixture
                    .repository
                    .revive_with_aegis(AegisReviveRequest {
                        pet_id: pet.pet_id,
                        guild_id: Some(TEST_GUILD_ID),
                        revived_last_fed_at: NOW + 6 * DAY,
                        revive_hunger: 10,
                        anchor: AnchorGuard::new(pet.last_fed_at, pet.hunger_at_last_fed),
                    })
                    .unwrap()
            );
            let revived = fixture
                .repository
                .get_pet_by_id(pet.pet_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(revived.aegis_used, 1);
            assert_eq!(revived.hunger_at_last_fed, 10);
            assert!(
                !fixture
                    .repository
                    .revive_with_aegis(AegisReviveRequest {
                        pet_id: pet.pet_id,
                        guild_id: Some(TEST_GUILD_ID),
                        revived_last_fed_at: NOW + 7 * DAY,
                        revive_hunger: 10,
                        anchor: AnchorGuard::new(revived.last_fed_at, revived.hunger_at_last_fed),
                    })
                    .unwrap()
            );
        }

        #[test]
        fn test_starvation_candidates_boundary() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            let death = pet.last_fed_at + 5 * DAY;
            assert!(
                fixture
                    .repository
                    .find_starvation_candidates(death - DAY, 20, 110)
                    .unwrap()
                    .iter()
                    .all(|candidate| candidate.pet_id != pet.pet_id)
            );
            assert!(
                fixture
                    .repository
                    .find_starvation_candidates(death, 20, 110)
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate.pet_id == pet.pet_id)
            );
        }

        #[test]
        fn test_stale_anchor_death_claim_loses_to_a_feed() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            fixture.feed(&pet, FeedOptions::default());
            assert!(
                !fixture
                    .repository
                    .claim_death(&DeathClaimRequest {
                        pet_id: pet.pet_id,
                        guild_id: Some(TEST_GUILD_ID),
                        died_at: NOW + 5 * DAY,
                        cause: "starvation",
                        anchor: AnchorGuard::new(pet.last_fed_at, pet.hunger_at_last_fed),
                    })
                    .unwrap()
            );
            assert!(
                fixture
                    .repository
                    .get_active_pet(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .is_some()
            );
        }

        #[test]
        fn test_stale_anchor_aegis_revive_loses_to_a_feed() {
            let fixture = rich_fixture();
            let egg = fixture.adopt_standard();
            let pet = fixture.hatch(&egg, "aegis_cama");
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            let fed = fixture.feed(&pet, FeedOptions::default());
            assert!(
                !fixture
                    .repository
                    .revive_with_aegis(AegisReviveRequest {
                        pet_id: pet.pet_id,
                        guild_id: Some(TEST_GUILD_ID),
                        revived_last_fed_at: NOW + 5 * DAY,
                        revive_hunger: 10,
                        anchor: AnchorGuard::new(pet.last_fed_at, pet.hunger_at_last_fed),
                    })
                    .unwrap()
            );
            let fresh = fixture
                .repository
                .get_pet_by_id(pet.pet_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(fresh.aegis_used, 0);
            assert_eq!(fresh.last_fed_at, fed.last_fed_at);
        }

        #[test]
        fn test_unannounced_death_flow() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert!(fixture.claim_death(&pet, NOW + 500));
            assert_eq!(
                fixture
                    .repository
                    .get_unannounced_deaths(100)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [pet.pet_id]
            );
            fixture
                .repository
                .mark_death_announced(pet.pet_id, Some(TEST_GUILD_ID), NOW + 600)
                .unwrap();
            assert!(
                fixture
                    .repository
                    .get_unannounced_deaths(100)
                    .unwrap()
                    .is_empty()
            );
        }

        #[test]
        fn test_unannounced_hatch_flow() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            assert!(
                fixture
                    .repository
                    .find_unannounced_hatches(NOW, 100)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                fixture
                    .repository
                    .find_unannounced_hatches(NOW + HATCH, 100)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [pet.pet_id]
            );
            fixture
                .repository
                .mark_hatch_announced(pet.pet_id, Some(TEST_GUILD_ID), NOW + HATCH + 1)
                .unwrap();
            assert!(
                fixture
                    .repository
                    .find_unannounced_hatches(NOW + HATCH, 100)
                    .unwrap()
                    .is_empty()
            );
        }

        #[test]
        fn test_unannounced_pet_backlogs_are_oldest_first_and_bounded() {
            let fixture = rich_fixture();
            fixture.seed_player(101, TEST_GUILD_ID, 100);
            fixture.seed_player(102, TEST_GUILD_ID, 100);
            let pets = [
                fixture.adopt(AdoptOptions {
                    discord_id: 100,
                    now: NOW + 20,
                    hatch_seconds: 1,
                    ..AdoptOptions::default()
                }),
                fixture.adopt(AdoptOptions {
                    discord_id: 101,
                    now: NOW,
                    hatch_seconds: 1,
                    ..AdoptOptions::default()
                }),
                fixture.adopt(AdoptOptions {
                    discord_id: 102,
                    now: NOW + 10,
                    hatch_seconds: 1,
                    ..AdoptOptions::default()
                }),
            ];
            assert_eq!(
                fixture
                    .repository
                    .find_unannounced_hatches(NOW + 30, 2)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [pets[1].pet_id, pets[2].pet_id]
            );
            for (pet, died_at) in pets.iter().zip([NOW + 60, NOW + 40, NOW + 50]) {
                assert!(fixture.claim_death(pet, died_at));
            }
            assert_eq!(
                fixture
                    .repository
                    .get_unannounced_deaths(2)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [pets[1].pet_id, pets[2].pet_id]
            );
        }
    }

    mod salt_lick_and_rename {
        use super::*;

        #[test]
        fn test_salt_lick_sets_pampered_and_counts_care() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            let fresh = fixture
                .repository
                .use_salt_lick(&SaltLickRequest {
                    pet_id: pet.pet_id,
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    cost: 8,
                    now: NOW + HATCH,
                    pampered_until: NOW + HATCH + 43_200,
                    week_key: WEEK,
                    anchor: None,
                })
                .unwrap();
            assert_eq!(fresh.pampered_until, Some(NOW + HATCH + 43_200));
            assert_eq!(fresh.week_consumed_jc, 8);
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 972);
            assert_failure(
                fixture.repository.use_salt_lick(&SaltLickRequest {
                    pet_id: pet.pet_id,
                    discord_id: 100,
                    guild_id: Some(TEST_GUILD_ID),
                    cost: 8,
                    now: NOW + HATCH + 100,
                    pampered_until: NOW + HATCH + 43_300,
                    week_key: WEEK,
                    anchor: None,
                }),
                PetRepositoryFailure::AlreadyPampered,
            );
        }

        #[test]
        fn test_rename_debits_and_updates() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture
                .repository
                .rename_pet(pet.pet_id, 100, Some(TEST_GUILD_ID), "Sir Blep", 10)
                .unwrap();
            assert_eq!(
                fixture
                    .repository
                    .get_active_pet(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .unwrap()
                    .name,
                "Sir Blep"
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 970);
        }
    }

    mod refunds {
        use super::*;

        fn notice() -> RefundNotice {
            RefundNotice {
                guild_id: TEST_GUILD_ID,
                week_key: WEEK.to_owned(),
                payouts: vec![RefundPayout {
                    discord_id: 100,
                    consumed_jc: 20,
                    multiplier_pct: 200,
                    amount: 40,
                }],
                total_paid: 40,
                scaled_down: false,
            }
        }

        #[test]
        fn test_refund_notice_persists_until_marked() {
            let fixture = rich_fixture();
            fixture.seed_nonprofit(TEST_GUILD_ID, 500);
            let notice = notice();
            assert!(
                fixture
                    .repository
                    .pay_refunds_atomic(Some(TEST_GUILD_ID), WEEK, &[(100, 40)], NOW, Some(&notice))
                    .unwrap()
            );
            assert_eq!(
                fixture.repository.get_unannounced_refunds(100).unwrap(),
                [notice]
            );
            fixture
                .repository
                .mark_refund_announced(Some(TEST_GUILD_ID), WEEK, NOW + 1)
                .unwrap();
            assert!(
                fixture
                    .repository
                    .get_unannounced_refunds(100)
                    .unwrap()
                    .is_empty()
            );
        }

        #[test]
        fn test_refund_announcement_backlog_is_oldest_first_and_bounded() {
            let fixture = Fixture::new();
            let payload = "{\"payouts\": [], \"scaled_down\": false}";
            let connection = fixture.connection();
            for (week, paid_at) in [
                ("2026-W32", NOW + 20),
                ("2026-W30", NOW),
                ("2026-W31", NOW + 10),
            ] {
                connection.execute("INSERT INTO pet_refund_windows (guild_id, week_key, paid_at, total_paid, announcement_payload) VALUES (?1, ?2, ?3, 0, ?4)", params![TEST_GUILD_ID, week, paid_at, payload]).unwrap();
            }
            assert_eq!(
                fixture
                    .repository
                    .get_unannounced_refunds(2)
                    .unwrap()
                    .into_iter()
                    .map(|notice| notice.week_key)
                    .collect::<Vec<_>>(),
                ["2026-W30", "2026-W31"]
            );
        }

        #[test]
        fn test_pay_refunds_claims_window_once() {
            let fixture = rich_fixture();
            fixture.seed_nonprofit(TEST_GUILD_ID, 500);
            assert!(
                fixture
                    .repository
                    .pay_refunds_atomic(Some(TEST_GUILD_ID), WEEK, &[(100, 40)], NOW, None)
                    .unwrap()
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 1_040);
            assert_eq!(
                fixture
                    .repository
                    .get_nonprofit_balance(Some(TEST_GUILD_ID))
                    .unwrap(),
                460
            );
            assert!(
                !fixture
                    .repository
                    .pay_refunds_atomic(Some(TEST_GUILD_ID), WEEK, &[(100, 40)], NOW, None)
                    .unwrap()
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 1_040);
            assert!(
                fixture
                    .repository
                    .is_refund_window_paid(Some(TEST_GUILD_ID), WEEK)
                    .unwrap()
            );
            assert_eq!(fixture.ledger_count("pet_refund"), 2);
        }

        #[test]
        fn test_fund_short_rolls_back_claim() {
            let fixture = rich_fixture();
            fixture.seed_nonprofit(TEST_GUILD_ID, 10);
            assert_failure(
                fixture.repository.pay_refunds_atomic(
                    Some(TEST_GUILD_ID),
                    WEEK,
                    &[(100, 40)],
                    NOW,
                    None,
                ),
                PetRepositoryFailure::FundShort,
            );
            assert!(
                !fixture
                    .repository
                    .is_refund_window_paid(Some(TEST_GUILD_ID), WEEK)
                    .unwrap()
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 1_000);
        }

        #[test]
        fn test_missing_player_share_returns_to_fund() {
            let fixture = rich_fixture();
            fixture.seed_nonprofit(TEST_GUILD_ID, 500);
            assert!(
                fixture
                    .repository
                    .pay_refunds_atomic(
                        Some(TEST_GUILD_ID),
                        WEEK,
                        &[(100, 40), (999_999, 60)],
                        NOW,
                        None
                    )
                    .unwrap()
            );
            assert_eq!(fixture.balance(100, TEST_GUILD_ID), 1_040);
            assert_eq!(
                fixture
                    .repository
                    .get_nonprofit_balance(Some(TEST_GUILD_ID))
                    .unwrap(),
                460
            );
        }

        #[test]
        fn test_refund_ledger_covers_fund_debit() {
            let fixture = rich_fixture();
            fixture.seed_nonprofit(TEST_GUILD_ID, 500);
            fixture
                .repository
                .pay_refunds_atomic(Some(TEST_GUILD_ID), WEEK, &[(100, 40)], NOW, None)
                .unwrap();
            assert!(fixture.ledger_count("pet_refund") > 0);
        }

        #[test]
        fn test_zero_total_claims_without_fund() {
            let fixture = rich_fixture();
            assert!(
                fixture
                    .repository
                    .pay_refunds_atomic(Some(TEST_GUILD_ID), WEEK, &[], NOW, None)
                    .unwrap()
            );
            assert!(
                fixture
                    .repository
                    .is_refund_window_paid(Some(TEST_GUILD_ID), WEEK)
                    .unwrap()
            );
        }

        #[test]
        fn test_week_care_listing() {
            let fixture = rich_fixture();
            let pet = fixture.adopt_standard();
            fixture.stock(100, Some(TEST_GUILD_ID), 5);
            fixture.feed(&pet, FeedOptions::default());
            assert_eq!(
                fixture
                    .repository
                    .list_week_care(Some(TEST_GUILD_ID), WEEK)
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [pet.pet_id]
            );
            assert!(
                fixture
                    .repository
                    .list_week_care(Some(TEST_GUILD_ID), "2026-W99")
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                fixture.repository.list_pet_guild_ids().unwrap(),
                [TEST_GUILD_ID]
            );
        }
    }

    mod production_adapter_reads {
        use super::*;

        #[test]
        fn latest_collection_and_bulk_reads_match_python_filters() {
            let fixture = rich_fixture();
            fixture.seed_player(200, TEST_GUILD_ID, 1_000);
            let first = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            assert!(fixture.claim_death(&first, NOW + 10));
            let second = fixture.adopt(AdoptOptions {
                now: NOW + 20,
                name: "Second",
                ..AdoptOptions::default()
            });
            let second = fixture.hatch(&second, "moon_cama");
            fixture
                .connection()
                .execute(
                    "UPDATE pets SET died_at=?1,death_cause='sacrifice'
                 WHERE pet_id=?2",
                    params![NOW + 30, second.pet_id],
                )
                .unwrap();
            let active_other = fixture.adopt(AdoptOptions {
                discord_id: 200,
                name: "Other",
                now: NOW + 40,
                ..AdoptOptions::default()
            });

            assert_eq!(
                fixture
                    .repository
                    .get_latest_dead_pet(100, Some(TEST_GUILD_ID))
                    .unwrap()
                    .unwrap()
                    .pet_id,
                second.pet_id
            );
            assert_eq!(
                fixture
                    .repository
                    .recent_dead_species(100, Some(TEST_GUILD_ID), 3)
                    .unwrap(),
                ["common_cama"]
            );
            assert_eq!(
                fixture
                    .repository
                    .species_raised(100, Some(TEST_GUILD_ID), second.hatched_at)
                    .unwrap(),
                ["common_cama", "moon_cama"]
            );
            assert_eq!(
                fixture
                    .repository
                    .get_active_pets_for(&[200, 999, 200], Some(TEST_GUILD_ID))
                    .unwrap()
                    .into_iter()
                    .map(|pet| pet.pet_id)
                    .collect::<Vec<_>>(),
                [active_other.pet_id]
            );
        }

        #[test]
        fn hunger_top_up_is_optimistic_and_moves_dig_snapshot_atomically() {
            let fixture = rich_fixture();
            let pet = fixture.hatch(&fixture.adopt_standard(), "common_cama");
            let request = HungerTopUpRequest {
                pet_id: pet.pet_id,
                guild_id: Some(TEST_GUILD_ID),
                anchor: AnchorGuard {
                    expected_last_fed_at: pet.last_fed_at,
                    expected_hunger: pet.hunger_at_last_fed,
                    expected_dig_work_units: Some(pet.dig_work_units),
                    expected_dig_work_at: Some(pet.dig_work_at),
                    new_dig_work_units: Some(12),
                    new_dig_work_at: Some(NOW + 9),
                },
                new_last_fed_at: NOW + 10,
                new_hunger: 90,
            };
            assert!(fixture.repository.apply_hunger_top_up(request).unwrap());
            assert!(!fixture.repository.apply_hunger_top_up(request).unwrap());
            let fresh = fixture
                .repository
                .get_pet_by_id(pet.pet_id, Some(TEST_GUILD_ID))
                .unwrap()
                .unwrap();
            assert_eq!(fresh.last_fed_at, NOW + 10);
            assert_eq!(fresh.hunger_at_last_fed, 90);
            assert_eq!(fresh.dig_work_units, 12);
            assert_eq!(fresh.dig_work_at, NOW + 9);
        }
    }

    #[test]
    fn stronger_concurrent_adoptions_charge_exactly_once() {
        let fixture = Arc::new(rich_fixture());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let fixture = Arc::clone(&fixture);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    fixture.adopt_result(AdoptOptions::default())
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("adoption thread joins"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.as_ref().err().and_then(PetRepositoryError::failure)
                        == Some(PetRepositoryFailure::AlreadyHasPet)
                })
                .count(),
            1
        );
        assert_eq!(fixture.balance(100, TEST_GUILD_ID), 980);
        assert_eq!(fixture.ledger_count("pet"), 1);
    }
}
