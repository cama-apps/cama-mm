//! Existing-schema persistence for player registration and preferred roles.
//!
//! Python remains the schema, migration, and live runtime authority. This
//! adapter opens only an already-migrated database through
//! [`crate::open_runtime_connection`]. Player creation and its globally unique
//! primary Steam link share one `BEGIN IMMEDIATE` transaction, so any junction
//! failure rolls the player row back as well.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cama_domain::rating::cap_glicko_rd;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const STEAM_ACCOUNT_ID_UPPER_BOUND: i64 = 1_i64 << 32;

pub type PlayHoursWithTimezone = (Option<String>, Option<Vec<i64>>);

#[derive(Clone, Debug, PartialEq)]
pub struct RegisterPlayerRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub discord_username: &'a str,
    pub steam_id: i64,
    pub dotabuff_url: &'a str,
    pub initial_mmr: i64,
    pub glicko_rating: f64,
    pub glicko_rd: f64,
    pub glicko_volatility: f64,
    pub os_mu: f64,
    pub os_sigma: f64,
    pub os_rating_version: i64,
    pub os_algorithm_fingerprint: &'a str,
    pub exclusion_count: i64,
    pub added_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredPlayer {
    pub discord_id: i64,
    pub guild_id: i64,
    pub steam_id: i64,
    pub initial_mmr: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationPlayerPreferences {
    pub preferred_region: Option<String>,
    pub inferred_region: Option<String>,
    pub exclusion_count: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionBackfillCandidate {
    pub discord_id: i64,
    pub guild_id: i64,
    pub steam_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmrPromptState {
    pub nonce: u64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub steam_id: i64,
    pub attempts: i64,
    pub expires_at: i64,
    pub completed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetRolesOutcome {
    Updated(Vec<String>),
    MissingPlayer,
}

#[derive(Debug, Error)]
pub enum RegistrationRepositoryError {
    #[error("registration SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("player {discord_id} is already registered in guild {guild_id}")]
    DuplicatePlayer { discord_id: i64, guild_id: i64 },
    #[error("Steam ID {steam_id} is already linked to player {owner_id}")]
    SteamAlreadyOwned { steam_id: i64, owner_id: i64 },
    #[error("invalid Steam32 account ID {0}")]
    InvalidSteamId(i64),
}

#[derive(Clone, Debug)]
pub struct RegistrationRepository {
    path: PathBuf,
}

impl RegistrationRepository {
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

    pub fn set_roles_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        roles: &[String],
    ) -> Result<SetRolesOutcome, RegistrationRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let encoded = encode_roles(roles);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE players
             SET preferred_roles = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![encoded, discord_id, guild_id],
        )?;
        if updated == 0 {
            transaction.commit()?;
            return Ok(SetRolesOutcome::MissingPlayer);
        }
        transaction.commit()?;
        Ok(SetRolesOutcome::Updated(roles.to_vec()))
    }

    pub fn get_roles(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<String>>, RegistrationRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT preferred_roles FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|row| row.map(|raw| raw.map_or_else(Vec::new, |raw| decode_roles(&raw))))
            .map_err(Into::into)
    }

    /// Atomically create the guild player row and globally unique primary link.
    pub fn register_player_atomic(
        &self,
        request: &RegisterPlayerRequest<'_>,
    ) -> Result<RegisteredPlayer, RegistrationRepositoryError> {
        validate_steam_id(request.steam_id)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if transaction
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(RegistrationRepositoryError::DuplicatePlayer {
                discord_id: request.discord_id,
                guild_id,
            });
        }
        if let Some(owner_id) = steam_owner(&transaction, request.steam_id)?
            && owner_id != request.discord_id
        {
            return Err(RegistrationRepositoryError::SteamAlreadyOwned {
                steam_id: request.steam_id,
                owner_id,
            });
        }

        transaction.execute(
            "INSERT INTO players (
                 discord_id, guild_id, discord_username, dotabuff_url, steam_id,
                 initial_mmr, current_mmr, glicko_rating, glicko_rd,
                 glicko_volatility, os_mu, os_sigma, os_rating_version,
                 os_algorithm_fingerprint, exclusion_count, jopacoin_balance,
                 updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, NULL, ?5, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, 3, CURRENT_TIMESTAMP
             )",
            params![
                request.discord_id,
                guild_id,
                request.discord_username,
                request.dotabuff_url,
                request.initial_mmr,
                request.glicko_rating,
                cap_glicko_rd(request.glicko_rd),
                request.glicko_volatility,
                request.os_mu,
                request.os_sigma,
                request.os_rating_version,
                request.os_algorithm_fingerprint,
                request.exclusion_count,
            ],
        )?;
        transaction.execute(
            "UPDATE player_steam_ids SET is_primary = 0 WHERE discord_id = ?1",
            [request.discord_id],
        )?;
        transaction.execute(
            "INSERT INTO player_steam_ids
                 (discord_id, steam_id, is_primary, added_at)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(discord_id, steam_id) DO UPDATE SET is_primary = 1",
            params![request.discord_id, request.steam_id, request.added_at],
        )?;
        transaction.execute(
            "UPDATE players SET steam_id = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2",
            params![request.steam_id, request.discord_id],
        )?;
        transaction.commit()?;
        Ok(RegisteredPlayer {
            discord_id: request.discord_id,
            guild_id,
            steam_id: request.steam_id,
            initial_mmr: request.initial_mmr,
        })
    }

    pub fn steam_owner(&self, steam_id: i64) -> Result<Option<i64>, RegistrationRepositoryError> {
        let connection = self.connection()?;
        steam_owner(&connection, steam_id).map_err(Into::into)
    }

    pub fn primary_steam_id(
        &self,
        discord_id: i64,
    ) -> Result<Option<i64>, RegistrationRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT steam_id FROM player_steam_ids
                 WHERE discord_id = ?1 AND is_primary = 1
                 ORDER BY added_at, id LIMIT 1",
                [discord_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn player_exists(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, RegistrationRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Into::into)
    }

    pub fn player_preferences(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RegistrationPlayerPreferences>, RegistrationRepositoryError> {
        self.connection()?
            .query_row(
                "SELECT preferred_region,inferred_region,COALESCE(exclusion_count,0)
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(RegistrationPlayerPreferences {
                        preferred_region: row.get(0)?,
                        inferred_region: row.get(1)?,
                        exclusion_count: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_preferred_region(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        region: &str,
    ) -> Result<bool, RegistrationRepositoryError> {
        let updated = self.connection()?.execute(
            "UPDATE players SET preferred_region=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![region, discord_id, Self::normalize_guild_id(guild_id),],
        )?;
        Ok(updated == 1)
    }

    pub fn set_inferred_region(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        region: &str,
    ) -> Result<bool, RegistrationRepositoryError> {
        let updated = self.connection()?.execute(
            "UPDATE players SET inferred_region=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![region, discord_id, Self::normalize_guild_id(guild_id),],
        )?;
        Ok(updated == 1)
    }

    /// Return every positive-ID player whose inferred region has not yet been
    /// attempted, resolving the primary junction Steam ID before the legacy
    /// column. READY recovery deduplicates the resulting Steam IDs before
    /// calling OpenDota.
    pub fn region_backfill_candidates(
        &self,
    ) -> Result<Vec<RegionBackfillCandidate>, RegistrationRepositoryError> {
        self.region_backfill_candidates_scoped(None)
    }

    pub fn region_backfill_candidates_for_guilds(
        &self,
        guild_ids: &BTreeSet<i64>,
    ) -> Result<Vec<RegionBackfillCandidate>, RegistrationRepositoryError> {
        self.region_backfill_candidates_scoped(Some(guild_ids))
    }

    fn region_backfill_candidates_scoped(
        &self,
        guild_ids: Option<&BTreeSet<i64>>,
    ) -> Result<Vec<RegionBackfillCandidate>, RegistrationRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.discord_id, p.guild_id,
                    COALESCE(psi.steam_id, p.steam_id) AS steam_id
             FROM players p
             LEFT JOIN player_steam_ids psi
                    ON psi.discord_id = p.discord_id AND psi.is_primary = 1
             WHERE p.inferred_region IS NULL
               AND p.discord_id > 0
               AND COALESCE(psi.steam_id, p.steam_id) IS NOT NULL
             ORDER BY p.discord_id, p.guild_id",
        )?;
        let candidates = statement
            .query_map([], |row| {
                Ok(RegionBackfillCandidate {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    steam_id: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RegistrationRepositoryError::from)?;
        Ok(candidates
            .into_iter()
            .filter(|candidate| {
                guild_ids.is_none_or(|guild_ids| guild_ids.contains(&candidate.guild_id))
            })
            .collect())
    }

    /// Persist a completed startup inference in one existing-schema
    /// transaction. Rows that no longer need backfill are left unchanged.
    pub fn set_inferred_regions_bulk(
        &self,
        updates: &[(i64, i64, String)],
    ) -> Result<usize, RegistrationRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for (discord_id, guild_id, region) in updates {
            changed += transaction.execute(
                "UPDATE players
                 SET inferred_region=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND inferred_region IS NULL",
                params![region, discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    /// Save a restart-safe callback state in the existing generic key/value table.
    pub fn save_mmr_prompt(
        &self,
        state: &MmrPromptState,
    ) -> Result<(), RegistrationRepositoryError> {
        let key = mmr_prompt_key(state.nonce);
        let value = encode_mmr_prompt(state);
        self.connection()?.execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)
             ON CONFLICT(guild_id,key) DO UPDATE SET value=excluded.value",
            params![state.guild_id, key, value],
        )?;
        Ok(())
    }

    pub fn mmr_prompt(
        &self,
        guild_id: i64,
        nonce: u64,
    ) -> Result<Option<MmrPromptState>, RegistrationRepositoryError> {
        self.connection()?
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, mmr_prompt_key(nonce)],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| decode_mmr_prompt(nonce, guild_id, &value))
            .transpose()
    }

    /// Increment invalid attempts under one write lock and return the new state.
    pub fn record_mmr_prompt_failure(
        &self,
        guild_id: i64,
        nonce: u64,
    ) -> Result<Option<MmrPromptState>, RegistrationRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(value) = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, mmr_prompt_key(nonce)],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut state = decode_mmr_prompt(nonce, guild_id, &value)?;
        state.attempts = state.attempts.saturating_add(1);
        transaction.execute(
            "UPDATE app_kv SET value=?1 WHERE guild_id=?2 AND key=?3",
            params![encode_mmr_prompt(&state), guild_id, mmr_prompt_key(nonce)],
        )?;
        transaction.commit()?;
        Ok(Some(state))
    }

    pub fn complete_mmr_prompt(
        &self,
        guild_id: i64,
        nonce: u64,
    ) -> Result<bool, RegistrationRepositoryError> {
        let Some(mut state) = self.mmr_prompt(guild_id, nonce)? else {
            return Ok(false);
        };
        state.completed = true;
        self.save_mmr_prompt(&state)?;
        Ok(true)
    }

    /// Persist a player's general timezone preference. Returns `false` if the
    /// player isn't registered in this guild.
    pub fn set_timezone(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        timezone: &str,
    ) -> Result<bool, RegistrationRepositoryError> {
        let updated = self.connection()?.execute(
            "UPDATE players SET timezone=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![timezone, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(updated == 1)
    }

    /// A player's general timezone setting, or `None` if unset or unregistered.
    pub fn timezone_of(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, RegistrationRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT timezone FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Persist a player's informational (never-enforced) dota play-time
    /// hours. `None` clears them. Returns `false` if the player isn't
    /// registered in this guild.
    pub fn set_dota_play_hours(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        hours: Option<&[i64]>,
    ) -> Result<bool, RegistrationRepositoryError> {
        let serialized = hours.map(|hours| serde_json::to_string(hours).unwrap_or_default());
        let updated = self.connection()?.execute(
            "UPDATE players SET dota_play_hours=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![serialized, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(updated == 1)
    }

    /// A player's informational dota play-time hours, or `None` if unset or
    /// unregistered.
    pub fn dota_play_hours_of(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<i64>>, RegistrationRepositoryError> {
        let raw: Option<String> = self
            .connection()?
            .query_row(
                "SELECT dota_play_hours FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(raw.map(|text| serde_json::from_str::<Vec<i64>>(&text).unwrap_or_default()))
    }

    /// Every registered player's timezone and informational play-time hours
    /// in a guild, for `/player playtime popular` aggregation.
    pub fn play_hours_for_guild(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<PlayHoursWithTimezone>, RegistrationRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT timezone, dota_play_hours FROM players WHERE guild_id=?1")?;
        let rows = statement.query_map(params![Self::normalize_guild_id(guild_id)], |row| {
            let timezone: Option<String> = row.get(0)?;
            let hours_text: Option<String> = row.get(1)?;
            Ok((timezone, hours_text))
        })?;
        let mut results = Vec::new();
        for row in rows {
            let (timezone, hours_text) = row?;
            let hours =
                hours_text.map(|text| serde_json::from_str::<Vec<i64>>(&text).unwrap_or_default());
            results.push((timezone, hours));
        }
        Ok(results)
    }
}

fn mmr_prompt_key(nonce: u64) -> String {
    format!("registration:mmr:{nonce}")
}

fn encode_mmr_prompt(state: &MmrPromptState) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        state.discord_id,
        state.steam_id,
        state.attempts,
        state.expires_at,
        i64::from(state.completed)
    )
}

fn decode_mmr_prompt(
    nonce: u64,
    guild_id: i64,
    value: &str,
) -> Result<MmrPromptState, RegistrationRepositoryError> {
    let invalid = || {
        rusqlite::Error::InvalidParameterName(format!(
            "invalid persisted MMR prompt {nonce}: {value}"
        ))
    };
    let mut values = value.split(':');
    let discord_id = values
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let steam_id = values
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let attempts = values
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let expires_at = values
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let completed = match values.next() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err(invalid().into()),
    };
    if values.next().is_some() {
        return Err(invalid().into());
    }
    Ok(MmrPromptState {
        nonce,
        discord_id,
        guild_id,
        steam_id,
        attempts,
        expires_at,
        completed,
    })
}

fn validate_steam_id(steam_id: i64) -> Result<(), RegistrationRepositoryError> {
    if steam_id <= 0 || steam_id >= STEAM_ACCOUNT_ID_UPPER_BOUND {
        return Err(RegistrationRepositoryError::InvalidSteamId(steam_id));
    }
    Ok(())
}

fn steam_owner(connection: &Connection, steam_id: i64) -> Result<Option<i64>, rusqlite::Error> {
    let junction = connection
        .query_row(
            "SELECT discord_id FROM player_steam_ids WHERE steam_id = ?1",
            [steam_id],
            |row| row.get(0),
        )
        .optional()?;
    if junction.is_some() {
        return Ok(junction);
    }
    connection
        .query_row(
            "SELECT discord_id FROM players WHERE steam_id = ?1 ORDER BY guild_id LIMIT 1",
            [steam_id],
            |row| row.get(0),
        )
        .optional()
}

// Python writes and reads this column with `json`, so both directions
// delegate to serde_json rather than a hand-rolled escaper that would drift
// from `core_repositories`' codec for the same column.
fn encode_roles(roles: &[String]) -> String {
    serde_json::to_string(roles).unwrap_or_else(|_| "[]".to_owned())
}

fn decode_roles(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

#[cfg(test)]
#[path = "registration/tests.rs"]
mod tests;
