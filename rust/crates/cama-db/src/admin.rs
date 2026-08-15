//! Existing-schema persistence used by the live `/admin` extension.
//!
//! The public repositories already own most individual admin operations. This
//! module contains only the cross-column/cross-table writes that must remain a
//! single SQLite transaction: dual-rating overrides, uncertainty bumps,
//! attributed balance adjustments, durable pending-match betting extension,
//! and recalibration cooldown reset.

use std::path::{Path, PathBuf};

use cama_domain::openskill::CamaOpenSkillSystem;
use cama_domain::player::{OPENSKILL_DISPLAY_SCALE, OPENSKILL_MIN_MU};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

const OPENSKILL_ALGORITHM_VERSION: i64 = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct AdminPlayerSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub username: String,
    pub wins: i64,
    pub losses: i64,
    pub balance: i64,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub glicko_volatility: Option<f64>,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
}

impl AdminPlayerSnapshot {
    #[must_use]
    pub fn games_played(&self) -> i64 {
        self.wins.saturating_add(self.losses)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UncertaintyBumpSummary {
    pub count: usize,
    pub average_rd_before: f64,
    pub average_rd_after: f64,
    pub openskill_count: usize,
    pub average_sigma_before: Option<f64>,
    pub average_sigma_after: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SetBothDisplayRatingsRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub display_rating: f64,
    pub rd: f64,
    pub volatility: f64,
    pub reason: &'a str,
    pub actor_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BettingExtension {
    pub pending_match_id: i64,
    pub old_lock_until: i64,
    pub new_lock_until: i64,
    pub lobby_kind: Option<String>,
    pub jump_url: Option<String>,
}

#[derive(Debug, Error)]
pub enum AdminRepositoryError {
    #[error("admin SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("player not found")]
    PlayerNotFound,
    #[error("pending match {0} was not found")]
    PendingMatchNotFound(i64),
    #[error("pending match {0} contains malformed JSON: {1}")]
    MalformedPendingMatch(i64, String),
    #[error("pending match {0} has no betting window")]
    MissingBettingWindow(i64),
    #[error("integer arithmetic overflow")]
    ArithmeticOverflow,
}

#[derive(Clone, Debug)]
pub struct AdminRepository {
    path: PathBuf,
}

impl AdminRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<AdminPlayerSnapshot>, AdminRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT discord_id,guild_id,discord_username,
                        COALESCE(wins,0),COALESCE(losses,0),
                        COALESCE(jopacoin_balance,0),
                        glicko_rating,glicko_rd,glicko_volatility,os_mu,os_sigma
                   FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| {
                    Ok(AdminPlayerSnapshot {
                        discord_id: row.get(0)?,
                        guild_id: row.get(1)?,
                        username: row.get(2)?,
                        wins: row.get(3)?,
                        losses: row.get(4)?,
                        balance: row.get(5)?,
                        glicko_rating: row.get(6)?,
                        glicko_rd: row.get(7)?,
                        glicko_volatility: row.get(8)?,
                        os_mu: row.get(9)?,
                        os_sigma: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Atomically update both visible rating systems and append the replayable
    /// OpenSkill override event used by Python.
    pub fn set_both_display_ratings(
        &self,
        request: SetBothDisplayRatingsRequest<'_>,
    ) -> Result<(), AdminRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let os_mu = OPENSKILL_MIN_MU + request.display_rating.max(0.0) / OPENSKILL_DISPLAY_SCALE;
        let fingerprint = CamaOpenSkillSystem::default().algorithm_fingerprint();
        let changed = transaction.execute(
            "UPDATE players
                SET glicko_rating=?1,glicko_rd=?2,glicko_volatility=?3,
                    os_mu=?4,os_sigma=COALESCE(os_sigma,?5),
                    os_rating_version=?6,os_algorithm_fingerprint=?7,
                    updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?8 AND guild_id=?9",
            params![
                request.display_rating,
                request.rd,
                request.volatility,
                os_mu,
                CamaOpenSkillSystem::DEFAULT_SIGMA,
                OPENSKILL_ALGORITHM_VERSION,
                fingerprint,
                request.discord_id,
                request.guild_id,
            ],
        )?;
        if changed != 1 {
            return Err(AdminRepositoryError::PlayerNotFound);
        }
        transaction.execute(
            "INSERT INTO openskill_rating_events (
                 guild_id,discord_id,event_type,value,event_at,reason,actor_id,
                 os_algorithm_version,os_algorithm_fingerprint
             ) VALUES (?1,?2,'set_mu',?3,?4,?5,?6,?7,?8)",
            params![
                request.guild_id,
                request.discord_id,
                os_mu,
                Utc::now().to_rfc3339(),
                request.reason,
                request.actor_id,
                OPENSKILL_ALGORITHM_VERSION,
                fingerprint,
            ],
        )?;
        increment_openskill_revision(&transaction, request.guild_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_glicko_uncertainty(
        &self,
        discord_id: i64,
        guild_id: i64,
        rating: f64,
        rd: f64,
        volatility: f64,
    ) -> Result<(), AdminRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let changed = connection.execute(
            "UPDATE players
                SET glicko_rating=?1,glicko_rd=?2,glicko_volatility=?3,
                    updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?4 AND guild_id=?5",
            params![rating, rd, volatility, discord_id, guild_id],
        )?;
        if changed != 1 {
            return Err(AdminRepositoryError::PlayerNotFound);
        }
        Ok(())
    }

    /// Increase Glicko RD and OpenSkill sigma together and append one replay
    /// event per OpenSkill player in the same writer transaction.
    pub fn bump_uncertainties(
        &self,
        guild_id: i64,
        rd_amount: f64,
        actor_id: i64,
    ) -> Result<Option<UncertaintyBumpSummary>, AdminRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT discord_id,glicko_rd,os_sigma
               FROM players
              WHERE guild_id=?1 AND glicko_rating IS NOT NULL
              ORDER BY discord_id",
        )?;
        let rows = statement
            .query_map([guild_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if rows.is_empty() {
            transaction.commit()?;
            return Ok(None);
        }
        let sigma_amount = rd_amount / OPENSKILL_DISPLAY_SCALE;
        let fingerprint = CamaOpenSkillSystem::default().algorithm_fingerprint();
        let event_at = Utc::now().to_rfc3339();
        let mut rd_before = 0.0;
        let mut rd_after = 0.0;
        let mut sigma_before = 0.0;
        let mut sigma_after = 0.0;
        let mut openskill_count = 0_usize;
        for (discord_id, old_rd, old_sigma) in &rows {
            let new_rd = (old_rd + rd_amount).min(350.0);
            rd_before += old_rd;
            rd_after += new_rd;
            transaction.execute(
                "UPDATE players SET glicko_rd=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3",
                params![new_rd, discord_id, guild_id],
            )?;
            let Some(old_sigma) = old_sigma else {
                continue;
            };
            let new_sigma = (old_sigma + sigma_amount).min(CamaOpenSkillSystem::DEFAULT_SIGMA);
            sigma_before += old_sigma;
            sigma_after += new_sigma;
            openskill_count += 1;
            transaction.execute(
                "UPDATE players
                    SET os_sigma=?1,os_rating_version=?2,
                        os_algorithm_fingerprint=?3,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?4 AND guild_id=?5",
                params![
                    new_sigma,
                    OPENSKILL_ALGORITHM_VERSION,
                    fingerprint,
                    discord_id,
                    guild_id,
                ],
            )?;
            transaction.execute(
                "INSERT INTO openskill_rating_events (
                     guild_id,discord_id,event_type,value,event_at,reason,actor_id,
                     os_algorithm_version,os_algorithm_fingerprint
                 ) VALUES (?1,?2,'add_sigma',?3,?4,
                           'major_patch_uncertainty_bump',?5,?6,?7)",
                params![
                    guild_id,
                    discord_id,
                    sigma_amount,
                    event_at,
                    actor_id,
                    OPENSKILL_ALGORITHM_VERSION,
                    fingerprint,
                ],
            )?;
        }
        if openskill_count > 0 {
            increment_openskill_revision(&transaction, guild_id)?;
        }
        transaction.commit()?;
        let count = rows.len();
        Ok(Some(UncertaintyBumpSummary {
            count,
            average_rd_before: rd_before / count as f64,
            average_rd_after: rd_after / count as f64,
            openskill_count,
            average_sigma_before: (openskill_count > 0)
                .then_some(sigma_before / openskill_count as f64),
            average_sigma_after: (openskill_count > 0)
                .then_some(sigma_after / openskill_count as f64),
        }))
    }

    /// Apply the exact attributed `admin_givecoin` balance movement expected
    /// by the economy-ledger trigger, returning pre/post balances atomically.
    pub fn adjust_player_balance(
        &self,
        discord_id: i64,
        guild_id: i64,
        amount: i64,
        actor_id: i64,
    ) -> Result<(i64, i64), AdminRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old_balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(AdminRepositoryError::PlayerNotFound)?;
        let new_balance = old_balance
            .checked_add(amount)
            .ok_or(AdminRepositoryError::ArithmeticOverflow)?;
        transaction.execute(
            "INSERT OR REPLACE INTO economy_ledger_context (
                 id,source,actor_id,related_type,related_id,reason,metadata
             ) VALUES (1,'admin_givecoin',?1,'admin_adjustment',?2,
                       'admin balance adjustment',?3)",
            params![
                actor_id,
                discord_id.to_string(),
                format!("{{\"amount\":{amount}}}"),
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?2 AND guild_id=?3",
            params![new_balance, discord_id, guild_id],
        )?;
        transaction.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
        if changed != 1 {
            return Err(AdminRepositoryError::PlayerNotFound);
        }
        transaction.commit()?;
        Ok((old_balance, new_balance))
    }

    pub fn reset_recalibration_cooldown(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, AdminRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let changed = connection.execute(
            "UPDATE recalibration_state
                SET last_recalibration_at=0,updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?1 AND guild_id=?2
                AND last_recalibration_at IS NOT NULL",
            params![discord_id, guild_id],
        )?;
        Ok(changed == 1)
    }

    /// Extend the selected pending match from `max(current_lock, now)` and
    /// persist the complete JSON document under one immediate writer lock.
    pub fn extend_betting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        minutes: i64,
        now: i64,
    ) -> Result<BettingExtension, AdminRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT payload FROM pending_matches
                  WHERE guild_id=?1 AND pending_match_id=?2",
                params![guild_id, pending_match_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(AdminRepositoryError::PendingMatchNotFound(pending_match_id))?;
        let mut payload = serde_json::from_str::<Value>(&raw).map_err(|error| {
            AdminRepositoryError::MalformedPendingMatch(pending_match_id, error.to_string())
        })?;
        let object = payload.as_object_mut().ok_or_else(|| {
            AdminRepositoryError::MalformedPendingMatch(
                pending_match_id,
                "root value is not an object".to_owned(),
            )
        })?;
        let old_lock_until = object
            .get("bet_lock_until")
            .and_then(Value::as_i64)
            .filter(|value| *value != 0)
            .ok_or(AdminRepositoryError::MissingBettingWindow(pending_match_id))?;
        let seconds = minutes
            .checked_mul(60)
            .ok_or(AdminRepositoryError::ArithmeticOverflow)?;
        let new_lock_until = old_lock_until
            .max(now)
            .checked_add(seconds)
            .ok_or(AdminRepositoryError::ArithmeticOverflow)?;
        object.insert("bet_lock_until".to_owned(), Value::from(new_lock_until));
        let lobby_kind = object
            .get("lobby_kind")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let jump_url = object
            .get("shuffle_message_jump_url")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let encoded = serde_json::to_string(&payload).map_err(|error| {
            AdminRepositoryError::MalformedPendingMatch(pending_match_id, error.to_string())
        })?;
        let changed = transaction.execute(
            "UPDATE pending_matches SET payload=?1
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(AdminRepositoryError::PendingMatchNotFound(pending_match_id));
        }
        transaction.commit()?;
        Ok(BettingExtension {
            pending_match_id,
            old_lock_until,
            new_lock_until,
            lobby_kind,
            jump_url,
        })
    }
}

fn increment_openskill_revision(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO openskill_rating_revisions (guild_id,revision,updated_at)
         VALUES (?1,1,CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             revision=openskill_rating_revisions.revision+1,
             updated_at=CURRENT_TIMESTAMP",
        [guild_id],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "admin/tests.rs"]
mod tests;
