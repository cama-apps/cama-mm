//! Existing-schema persistence for daily mana assignment.
//!
//! Python remains the schema-migration and production runtime authority during
//! the side-by-side migration. This adapter only opens an already-migrated
//! database through [`crate::open_runtime_connection`] and mirrors Python's
//! `BEGIN IMMEDIATE` claim semantics.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use self::white_stipend::pay_white_stipend;
use crate::open_runtime_connection;

#[path = "white_stipend.rs"]
mod white_stipend;

pub const PLAINS_GUARDIAN_CAPACITY: i64 = 25;

#[derive(Debug, Error)]
pub enum ManaRepositoryError {
    #[error("mana SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("mana White stipend mutation changed no row for player {0}")]
    StipendGuard(i64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaRow {
    pub current_land: String,
    pub assigned_date: String,
    pub consumed_today: bool,
    pub white_shield_remaining: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildManaRow {
    pub discord_id: i64,
    pub mana: ManaRow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaClaim {
    pub discord_id: i64,
    pub current_land: String,
    pub assigned_date: String,
    pub white_shield_remaining: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BankruptBuff {
    Insurance,
    Reroll,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownBankruptBuff(String);

impl fmt::Display for UnknownBankruptBuff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown bankrupt buff {:?}", self.0)
    }
}

impl std::error::Error for UnknownBankruptBuff {}

impl TryFrom<&str> for BankruptBuff {
    type Error = UnknownBankruptBuff;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "insurance" => Ok(Self::Insurance),
            "reroll" => Ok(Self::Reroll),
            unknown => Err(UnknownBankruptBuff(unknown.to_owned())),
        }
    }
}

impl BankruptBuff {
    const fn column(self) -> &'static str {
        match self {
            Self::Insurance => "bankrupt_insurance_used",
            Self::Reroll => "bankrupt_reroll_used",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ManaRepository {
    path: PathBuf,
}

impl ManaRepository {
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

    pub fn get_mana(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<ManaRow>, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT current_land, assigned_date, consumed_today,
                        white_shield_remaining
                 FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok(ManaRow {
                        current_land: row.get(0)?,
                        assigned_date: row.get(1)?,
                        consumed_today: row.get(2)?,
                        white_shield_remaining: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_mana(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        land: &str,
        assigned_date: &str,
    ) -> Result<(), ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO player_mana (
                 discord_id, guild_id, current_land, assigned_date, updated_at
             ) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 current_land = excluded.current_land,
                 assigned_date = excluded.assigned_date,
                 updated_at = CURRENT_TIMESTAMP",
            params![discord_id, guild_id, land, assigned_date],
        )?;
        Ok(())
    }

    pub fn get_all_mana(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<GuildManaRow>, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, current_land, assigned_date, consumed_today,
                    white_shield_remaining
             FROM player_mana
             WHERE guild_id = ?1
             ORDER BY discord_id",
        )?;
        let rows = statement.query_map([guild_id], |row| {
            Ok(GuildManaRow {
                discord_id: row.get(0)?,
                mana: ManaRow {
                    current_land: row.get(1)?,
                    assigned_date: row.get(2)?,
                    consumed_today: row.get(3)?,
                    white_shield_remaining: row.get(4)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Atomically advance one player's assignment unless this mana day was
    /// already claimed. A new day resets all daily-use state.
    pub fn claim_mana_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        land: &str,
        assigned_date: &str,
    ) -> Result<bool, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_date = transaction
            .query_row(
                "SELECT assigned_date FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        if current_date.as_deref() == Some(assigned_date) {
            transaction.commit()?;
            return Ok(false);
        }

        upsert_claim(&transaction, discord_id, guild_id, land, assigned_date)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Claim one mana day for a batch on one connection and one immediate
    /// transaction. Duplicate player IDs are ignored after their first entry;
    /// returned rows preserve the winning input order.
    ///
    /// When `white_stipend` is positive, the Plains White stipend is paid to
    /// eligible bankrupt Plains claimants inside the same transaction, so a
    /// claim and its stipend commit or roll back together. `white_stipend <= 0`
    /// skips stipend payment entirely.
    pub fn claim_mana_batch_atomic(
        &self,
        assignments: &[(i64, String)],
        guild_id: Option<i64>,
        assigned_date: &str,
        white_stipend: i64,
    ) -> Result<Vec<ManaClaim>, ManaRepositoryError> {
        if assignments.is_empty() {
            return Ok(Vec::new());
        }

        let mut seen = BTreeSet::new();
        let unique = assignments
            .iter()
            .filter(|(discord_id, _)| seen.insert(*discord_id))
            .collect::<Vec<_>>();
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claimed = Vec::with_capacity(unique.len());

        for (discord_id, land) in unique {
            let current_date = transaction
                .query_row(
                    "SELECT assigned_date FROM player_mana
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            if current_date.as_deref() == Some(assigned_date) {
                continue;
            }

            upsert_claim(&transaction, *discord_id, guild_id, land, assigned_date)?;
            claimed.push(ManaClaim {
                discord_id: *discord_id,
                current_land: land.clone(),
                assigned_date: assigned_date.to_owned(),
                white_shield_remaining: guardian_capacity(land),
            });
        }

        let plains_ids = claimed
            .iter()
            .filter(|claim| claim.current_land == "Plains")
            .map(|claim| claim.discord_id)
            .collect::<Vec<_>>();
        pay_plains_stipends(&transaction, &plains_ids, guild_id, white_stipend)?;

        transaction.commit()?;
        Ok(claimed)
    }

    pub fn mark_mana_consumed_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE player_mana
             SET consumed_today = 1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2 AND consumed_today = 0",
            params![discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn claim_bankrupt_buff_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        buff: BankruptBuff,
    ) -> Result<bool, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let statement = format!(
            "UPDATE player_mana SET {} = 1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2 AND {} = 0",
            buff.column(),
            buff.column()
        );
        let changed = transaction.execute(&statement, params![discord_id, guild_id])?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn is_bankrupt_buff_used(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        buff: BankruptBuff,
    ) -> Result<bool, ManaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let statement = format!(
            "SELECT {} FROM player_mana WHERE discord_id = ?1 AND guild_id = ?2",
            buff.column()
        );
        connection
            .query_row(&statement, params![discord_id, guild_id], |row| row.get(0))
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
    }
}

/// Pay the White stipend to eligible bankrupt Plains claimants inside the
/// caller's claim transaction, mirroring
/// `LoanRepository::distribute_nonprofit_stipends_atomic`: only players with a
/// non-positive balance are paid, in input order, until the nonprofit reserve
/// is empty, with each mutation running through the shared
/// [`white_stipend::pay_white_stipend`] core.
fn pay_plains_stipends(
    transaction: &Transaction<'_>,
    plains_ids: &[i64],
    guild_id: i64,
    white_stipend: i64,
) -> Result<(), ManaRepositoryError> {
    if white_stipend <= 0 || plains_ids.is_empty() {
        return Ok(());
    }
    let mut remaining = transaction
        .query_row(
            "SELECT COALESCE(total_collected, 0)
             FROM nonprofit_fund WHERE guild_id = ?1",
            [guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        .max(0);

    for discord_id in plains_ids {
        if remaining == 0 {
            break;
        }
        let bankrupt = transaction
            .query_row(
                "SELECT 1 FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND COALESCE(jopacoin_balance, 0) <= 0",
                params![discord_id, guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !bankrupt {
            continue;
        }
        let amount = white_stipend.min(remaining);
        if pay_white_stipend(transaction, guild_id, *discord_id, amount)?.is_err() {
            return Err(ManaRepositoryError::StipendGuard(*discord_id));
        }
        remaining -= amount;
    }
    Ok(())
}

fn guardian_capacity(land: &str) -> i64 {
    if land == "Plains" {
        PLAINS_GUARDIAN_CAPACITY
    } else {
        0
    }
}

fn upsert_claim(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    land: &str,
    assigned_date: &str,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO player_mana (
             discord_id, guild_id, current_land, assigned_date,
             bankrupt_insurance_used, bankrupt_reroll_used, consumed_today,
             white_shield_remaining, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 0, 0, 0, ?5, CURRENT_TIMESTAMP)
         ON CONFLICT(discord_id, guild_id) DO UPDATE SET
             current_land = excluded.current_land,
             assigned_date = excluded.assigned_date,
             bankrupt_insurance_used = 0,
             bankrupt_reroll_used = 0,
             consumed_today = 0,
             white_shield_remaining = excluded.white_shield_remaining,
             updated_at = CURRENT_TIMESTAMP",
        params![
            discord_id,
            guild_id,
            land,
            assigned_date,
            guardian_capacity(land)
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "mana_service/tests.rs"]
mod tests;
