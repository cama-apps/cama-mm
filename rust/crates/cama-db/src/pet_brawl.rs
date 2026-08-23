//! Python-compatible pet-brawl lifecycle, escrow, and settlement persistence.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This module opens only an existing SQLite file. Every lifecycle
//! mutation uses `BEGIN IMMEDIATE`, matching Python's serialization boundary
//! for state guards, wallet escrow/refunds, hunger/training settlement, and
//! the final brawl row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_domain::game_date::game_day_start_ts;
use cama_domain::pet::{
    BRAWL_LOSS_XP, BRAWL_TRAINING_LEVEL_CAP, BRAWL_TRAINING_STAT_CAP, BRAWL_TRAINING_XP_CAP,
    BRAWL_WIN_XP, BRAWL_XP_DAILY_CAP, BRAWL_XP_PER_LEVEL, get_species,
};
use cama_domain::pet_brawl::PetBrawl;
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

const SECONDS_PER_DAY: i128 = 86_400;

const BRAWL_COLUMNS: &str = concat!(
    "brawl_id, guild_id, channel_id, challenger_id, recipient_id, ",
    "challenger_pet_id, recipient_pet_id, status, created_at, expires_at, ",
    "resolved_at, winner_id, winner_pet_id, loser_pet_id, rounds, ",
    "winner_hunger_delta, loser_hunger_delta, wager, fee, ",
    "challenger_xp_delta, recipient_xp_delta, challenger_stat_gain, ",
    "recipient_stat_gain, personality_event_key"
);

#[derive(Debug, Error)]
pub enum PetBrawlRepositoryError {
    #[error("invalid_wager")]
    InvalidWager,
    #[error("invalid_fee")]
    InvalidFee,
    #[error("brawl_busy")]
    BrawlBusy,
    #[error("challenger_pet_unavailable")]
    ChallengerPetUnavailable,
    #[error("recipient_pet_unavailable")]
    RecipientPetUnavailable,
    #[error("challenger_funds")]
    ChallengerFunds,
    #[error("recipient_funds")]
    RecipientFunds,
    #[error("no_brawl")]
    NoBrawl,
    #[error("not_recipient")]
    NotRecipient,
    #[error("not_challenger")]
    NotChallenger,
    #[error("expired")]
    Expired,
    #[error("not_pending")]
    NotPending,
    #[error("not_open")]
    NotOpen,
    #[error("already_done")]
    AlreadyDone,
    #[error("not_active")]
    NotActive,
    #[error("participant_mismatch")]
    ParticipantMismatch,
    #[error("A pet-brawl participant balance could not be updated.")]
    ParticipantBalanceUpdate,
    #[error("game-day timestamp is outside the supported range")]
    InvalidGameDay,
    #[error("pet-brawl SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SweepResult {
    pub expired: i64,
    pub voided: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrawSettlementResult {
    pub wager: i64,
    pub fee: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct BrawlSettlement<'a> {
    pub winner_id: i64,
    pub winner_pet_id: i64,
    pub loser_pet_id: i64,
    pub rounds: i64,
    pub now: i64,
    pub decay_per_day: i64,
    pub winner_gain: i64,
    pub loser_loss: i64,
    pub loss_floor: i64,
    pub daily_win_cap: i64,
    pub winner_stat_gain: Option<&'a str>,
    pub loser_stat_gain: Option<&'a str>,
    pub personality_event_key: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrawlSettlementResult {
    pub winner_delta: i64,
    pub loser_delta: i64,
    pub winner_xp_delta: i64,
    pub loser_xp_delta: i64,
    pub winner_stat_gain: Option<String>,
    pub loser_stat_gain: Option<String>,
    pub payout: i64,
    pub wager: i64,
    pub fee: i64,
    pub personality_event_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PetBrawlRepository {
    path: PathBuf,
}

impl PetBrawlRepository {
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

    pub fn get_brawl(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PetBrawl>, PetBrawlRepositoryError> {
        let connection = self.connection()?;
        let sql =
            format!("SELECT {BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?1 AND guild_id = ?2");
        Ok(connection
            .query_row(
                &sql,
                params![brawl_id, Self::normalize_guild_id(guild_id)],
                row_to_brawl,
            )
            .optional()?)
    }

    pub fn get_pet_record(
        &self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(i64, i64), PetBrawlRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let wins = connection.query_row(
            "SELECT COUNT(*) FROM pet_brawls
             WHERE winner_pet_id = ?1 AND guild_id = ?2 AND status = 'done'",
            params![pet_id, guild_id],
            |row| row.get(0),
        )?;
        let losses = connection.query_row(
            "SELECT COUNT(*) FROM pet_brawls
             WHERE loser_pet_id = ?1 AND guild_id = ?2 AND status = 'done'",
            params![pet_id, guild_id],
            |row| row.get(0),
        )?;
        Ok((wins, losses))
    }

    pub fn get_records_for(
        &self,
        pet_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, (i64, i64)>, PetBrawlRepositoryError> {
        if pet_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let placeholders = (1..=pet_ids.len())
            .map(|index| format!("?{}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let mut records = pet_ids
            .iter()
            .copied()
            .map(|pet_id| (pet_id, (0, 0)))
            .collect::<BTreeMap<_, _>>();
        let parameters = std::iter::once(guild_id).chain(pet_ids.iter().copied());
        let wins_sql = format!(
            "SELECT winner_pet_id, COUNT(*) FROM pet_brawls
             WHERE guild_id = ?1 AND status = 'done'
               AND winner_pet_id IN ({placeholders}) GROUP BY winner_pet_id"
        );
        {
            let mut statement = connection.prepare(&wins_sql)?;
            let rows = statement.query_map(params_from_iter(parameters.clone()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (pet_id, wins) = row?;
                if let Some(record) = records.get_mut(&pet_id) {
                    record.0 = wins;
                }
            }
        }
        let losses_sql = format!(
            "SELECT loser_pet_id, COUNT(*) FROM pet_brawls
             WHERE guild_id = ?1 AND status = 'done'
               AND loser_pet_id IN ({placeholders}) GROUP BY loser_pet_id"
        );
        let mut statement = connection.prepare(&losses_sql)?;
        let rows = statement.query_map(params_from_iter(parameters), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (pet_id, losses) = row?;
            if let Some(record) = records.get_mut(&pet_id) {
                record.1 = losses;
            }
        }
        Ok(records)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_brawl_atomic(
        &self,
        guild_id: Option<i64>,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        challenger_pet_id: i64,
        now: i64,
        expires_at: i64,
        wager: i64,
        fee: i64,
    ) -> Result<PetBrawl, PetBrawlRepositoryError> {
        if !(0..=100).contains(&wager) {
            return Err(PetBrawlRepositoryError::InvalidWager);
        }
        if fee < 0 || (wager == 0) != (fee == 0) {
            return Err(PetBrawlRepositoryError::InvalidFee);
        }
        let challenger_cost = wager
            .checked_add(fee)
            .ok_or(PetBrawlRepositoryError::ChallengerFunds)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stale = query_brawls(
            &transaction,
            &format!(
                "SELECT {BRAWL_COLUMNS} FROM pet_brawls
                 WHERE guild_id = ?1 AND status = 'pending' AND expires_at <= ?2
                   AND (challenger_id IN (?3, ?4) OR recipient_id IN (?3, ?4))"
            ),
            params![guild_id, now, challenger_id, recipient_id],
        )?;
        for stale_brawl in stale {
            let changed = transaction.execute(
                "UPDATE pet_brawls SET status = 'expired', resolved_at = ?1
                 WHERE brawl_id = ?2 AND guild_id = ?3 AND status = 'pending'
                   AND expires_at <= ?1",
                params![now, stale_brawl.brawl_id, guild_id],
            )?;
            if changed == 1 {
                refund_escrow(&transaction, &stale_brawl, "pending", None)?;
            }
        }
        let busy = transaction
            .query_row(
                "SELECT 1 FROM pet_brawls
                 WHERE guild_id = ?1 AND status IN ('pending', 'active')
                   AND (challenger_id IN (?2, ?3) OR recipient_id IN (?2, ?3))
                 LIMIT 1",
                params![guild_id, challenger_id, recipient_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if busy {
            return Err(PetBrawlRepositoryError::BrawlBusy);
        }
        let challenger_available = transaction
            .query_row(
                "SELECT 1 FROM pets
                 WHERE pet_id = ?1 AND discord_id = ?2 AND guild_id = ?3
                   AND died_at IS NULL AND is_active=1 AND hatched_at <= ?4",
                params![challenger_pet_id, challenger_id, guild_id, now],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !challenger_available {
            return Err(PetBrawlRepositoryError::ChallengerPetUnavailable);
        }
        let recipient_available = transaction
            .query_row(
                "SELECT 1 FROM pets
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND died_at IS NULL AND is_active=1 AND hatched_at <= ?3 LIMIT 1",
                params![recipient_id, guild_id, now],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !recipient_available {
            return Err(PetBrawlRepositoryError::RecipientPetUnavailable);
        }
        if wager != 0 {
            let challenger_balance = player_balance(&transaction, challenger_id, guild_id)?;
            if challenger_balance.is_none_or(|balance| balance < challenger_cost) {
                return Err(PetBrawlRepositoryError::ChallengerFunds);
            }
            let recipient_balance = player_balance(&transaction, recipient_id, guild_id)?;
            if recipient_balance.is_none_or(|balance| balance < wager) {
                return Err(PetBrawlRepositoryError::RecipientFunds);
            }
        }
        transaction.execute(
            "INSERT INTO pet_brawls (
                 guild_id, channel_id, challenger_id, recipient_id,
                 challenger_pet_id, status, created_at, expires_at, wager, fee
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?9)",
            params![
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                challenger_pet_id,
                now,
                expires_at,
                wager,
                fee
            ],
        )?;
        let brawl_id = transaction.last_insert_rowid();
        let brawl = require_brawl_by_id(&transaction, brawl_id)?;
        if wager != 0 {
            update_player_balance(
                &transaction,
                &brawl,
                challenger_id,
                -challenger_cost,
                Some(challenger_id),
                "challenger_escrow",
                Some(FundsSide::Challenger),
            )?;
        }
        transaction.commit()?;
        Ok(brawl)
    }

    pub fn accept_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        recipient_pet_id: i64,
        now: i64,
    ) -> Result<PetBrawl, PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let brawl = get_brawl_in_transaction(&transaction, brawl_id, guild_id)?
            .ok_or(PetBrawlRepositoryError::NoBrawl)?;
        validate_recipient_transition(&brawl, recipient_id, now)?;
        if brawl.wager != 0 {
            update_player_balance(
                &transaction,
                &brawl,
                recipient_id,
                -brawl.wager,
                Some(recipient_id),
                "recipient_escrow",
                Some(FundsSide::Recipient),
            )?;
        }
        let changed = transaction.execute(
            "UPDATE pet_brawls SET status = 'active', recipient_pet_id = ?1
             WHERE brawl_id = ?2 AND guild_id = ?3 AND recipient_id = ?4
               AND status = 'pending' AND expires_at > ?5",
            params![recipient_pet_id, brawl_id, guild_id, recipient_id, now],
        )?;
        if changed != 1 {
            raise_transition_error(&transaction, brawl_id, guild_id, recipient_id, now)?;
        }
        let accepted = require_brawl_by_id(&transaction, brawl_id)?;
        transaction.commit()?;
        Ok(accepted)
    }

    pub fn decline_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        now: i64,
    ) -> Result<(), PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE pet_brawls SET status = 'declined', resolved_at = ?1
             WHERE brawl_id = ?2 AND guild_id = ?3 AND recipient_id = ?4
               AND status = 'pending'",
            params![now, brawl_id, guild_id, recipient_id],
        )?;
        if changed != 1 {
            raise_transition_error(&transaction, brawl_id, guild_id, recipient_id, now)?;
        }
        let brawl = require_brawl_by_id(&transaction, brawl_id)?;
        refund_escrow(&transaction, &brawl, "pending", Some(recipient_id))?;
        transaction.commit()?;
        Ok(())
    }

    pub fn withdraw_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
        now: i64,
    ) -> Result<(), PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE pet_brawls SET status = 'void', resolved_at = ?1
             WHERE brawl_id = ?2 AND guild_id = ?3 AND challenger_id = ?4
               AND status = 'pending'",
            params![now, brawl_id, guild_id, challenger_id],
        )?;
        if changed != 1 {
            let challenger = transaction
                .query_row(
                    "SELECT challenger_id FROM pet_brawls WHERE brawl_id = ?1 AND guild_id = ?2",
                    params![brawl_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            match challenger {
                None => return Err(PetBrawlRepositoryError::NoBrawl),
                Some(actual) if actual != challenger_id => {
                    return Err(PetBrawlRepositoryError::NotChallenger);
                }
                Some(_) => return Err(PetBrawlRepositoryError::NotPending),
            }
        }
        let brawl = require_brawl_by_id(&transaction, brawl_id)?;
        refund_escrow(&transaction, &brawl, "pending", Some(challenger_id))?;
        transaction.commit()?;
        Ok(())
    }

    pub fn void_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<(), PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let brawl = get_brawl_in_transaction(&transaction, brawl_id, guild_id)?
            .ok_or(PetBrawlRepositoryError::NotOpen)?;
        if !matches!(brawl.status.as_str(), "pending" | "active") {
            return Err(PetBrawlRepositoryError::NotOpen);
        }
        let changed = transaction.execute(
            "UPDATE pet_brawls SET status = 'void', resolved_at = ?1
             WHERE brawl_id = ?2 AND guild_id = ?3 AND status IN ('pending', 'active')",
            params![now, brawl_id, guild_id],
        )?;
        if changed != 1 {
            return Err(PetBrawlRepositoryError::NotOpen);
        }
        refund_escrow(&transaction, &brawl, &brawl.status, None)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn sweep_stale(
        &self,
        now: i64,
        active_ttl_seconds: i64,
    ) -> Result<SweepResult, PetBrawlRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stale = query_brawls(
            &transaction,
            &format!(
                "SELECT {BRAWL_COLUMNS} FROM pet_brawls
                 WHERE (status = 'pending' AND expires_at <= ?1)
                    OR (status = 'active' AND created_at < ?2)"
            ),
            params![now, now - active_ttl_seconds],
        )?;
        let mut result = SweepResult {
            expired: 0,
            voided: 0,
        };
        for brawl in stale {
            let new_status = if brawl.status == "pending" {
                "expired"
            } else {
                "void"
            };
            let changed = transaction.execute(
                "UPDATE pet_brawls SET status = ?1, resolved_at = ?2
                 WHERE brawl_id = ?3 AND status = ?4",
                params![new_status, now, brawl.brawl_id, brawl.status],
            )?;
            if changed != 1 {
                continue;
            }
            refund_escrow(&transaction, &brawl, &brawl.status, None)?;
            if new_status == "expired" {
                result.expired += 1;
            } else {
                result.voided += 1;
            }
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn settle_draw_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        participant_pet_ids: (i64, i64),
        rounds: i64,
        now: i64,
    ) -> Result<DrawSettlementResult, PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let brawl = settlement_brawl(&transaction, brawl_id, guild_id)?;
        let recipient_pet_id = brawl
            .recipient_pet_id
            .ok_or(PetBrawlRepositoryError::ParticipantMismatch)?;
        if !unordered_pair_matches(
            participant_pet_ids,
            (brawl.challenger_pet_id, recipient_pet_id),
        ) {
            return Err(PetBrawlRepositoryError::ParticipantMismatch);
        }
        transaction.execute(
            "UPDATE pet_brawls SET status = 'done', resolved_at = ?1,
                 winner_id = NULL, winner_pet_id = NULL, loser_pet_id = NULL, rounds = ?2
             WHERE brawl_id = ?3 AND guild_id = ?4",
            params![now, rounds, brawl_id, guild_id],
        )?;
        refund_escrow(&transaction, &brawl, "active", None)?;
        let result = DrawSettlementResult {
            wager: brawl.wager,
            fee: brawl.fee,
        };
        transaction.commit()?;
        Ok(result)
    }

    pub fn settle_brawl_atomic(
        &self,
        brawl_id: i64,
        guild_id: Option<i64>,
        settlement: BrawlSettlement<'_>,
    ) -> Result<BrawlSettlementResult, PetBrawlRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let brawl = settlement_brawl(&transaction, brawl_id, guild_id)?;
        let recipient_pet_id = brawl
            .recipient_pet_id
            .ok_or(PetBrawlRepositoryError::ParticipantMismatch)?;
        let winner_pair_valid = (settlement.winner_id == brawl.challenger_id
            && settlement.winner_pet_id == brawl.challenger_pet_id)
            || (settlement.winner_id == brawl.recipient_id
                && settlement.winner_pet_id == recipient_pet_id);
        if !winner_pair_valid
            || !unordered_pair_matches(
                (settlement.winner_pet_id, settlement.loser_pet_id),
                (brawl.challenger_pet_id, recipient_pet_id),
            )
        {
            return Err(PetBrawlRepositoryError::ParticipantMismatch);
        }

        let winner_delta = apply_winner_gain(
            &transaction,
            settlement.winner_pet_id,
            guild_id,
            settlement.now,
            settlement.decay_per_day,
            settlement.winner_gain,
            settlement.daily_win_cap,
        )?;
        let loser_delta = apply_loser_loss(
            &transaction,
            settlement.loser_pet_id,
            guild_id,
            settlement.now,
            settlement.decay_per_day,
            settlement.loser_loss,
            settlement.loss_floor,
        )?;
        let winner_training = apply_training_xp(
            &transaction,
            settlement.winner_pet_id,
            guild_id,
            settlement.now,
            BRAWL_WIN_XP,
            settlement.winner_stat_gain,
        )?;
        let loser_training = apply_training_xp(
            &transaction,
            settlement.loser_pet_id,
            guild_id,
            settlement.now,
            BRAWL_LOSS_XP,
            settlement.loser_stat_gain,
        )?;
        let (challenger_xp_delta, recipient_xp_delta, challenger_stat_gain, recipient_stat_gain) =
            if settlement.winner_pet_id == brawl.challenger_pet_id {
                (
                    winner_training.xp_delta,
                    loser_training.xp_delta,
                    winner_training.stat_gain.clone(),
                    loser_training.stat_gain.clone(),
                )
            } else {
                (
                    loser_training.xp_delta,
                    winner_training.xp_delta,
                    loser_training.stat_gain.clone(),
                    winner_training.stat_gain.clone(),
                )
            };
        let mut personality_event_key = settlement.personality_event_key.map(str::to_owned);
        if (winner_training.reached_cap || loser_training.reached_cap)
            && personality_event_key.as_deref() != Some("milestone.champion")
        {
            personality_event_key = Some("milestone.build".to_owned());
        }
        let payout = 2 * brawl.wager;
        if payout != 0 {
            update_player_balance(
                &transaction,
                &brawl,
                settlement.winner_id,
                payout,
                Some(settlement.winner_id),
                "winner_payout",
                None,
            )?;
            set_ledger_context(
                &transaction,
                &brawl,
                Some(settlement.winner_id),
                "brawl_fee_reserve_credit",
            )?;
            forfeit_to_nonprofit(&transaction, guild_id, brawl.fee)?;
            clear_ledger_context(&transaction)?;
        }
        transaction.execute(
            "UPDATE pet_brawls SET status = 'done', resolved_at = ?1,
                 winner_id = ?2, winner_pet_id = ?3, loser_pet_id = ?4, rounds = ?5,
                 winner_hunger_delta = ?6, loser_hunger_delta = ?7,
                 challenger_xp_delta = ?8, recipient_xp_delta = ?9,
                 challenger_stat_gain = ?10, recipient_stat_gain = ?11,
                 personality_event_key = ?12 WHERE brawl_id = ?13",
            params![
                settlement.now,
                settlement.winner_id,
                settlement.winner_pet_id,
                settlement.loser_pet_id,
                settlement.rounds,
                winner_delta,
                loser_delta,
                challenger_xp_delta,
                recipient_xp_delta,
                challenger_stat_gain,
                recipient_stat_gain,
                personality_event_key,
                brawl_id
            ],
        )?;
        let result = BrawlSettlementResult {
            winner_delta,
            loser_delta,
            winner_xp_delta: winner_training.xp_delta,
            loser_xp_delta: loser_training.xp_delta,
            winner_stat_gain: winner_training.stat_gain,
            loser_stat_gain: loser_training.stat_gain,
            payout,
            wager: brawl.wager,
            fee: brawl.fee,
            personality_event_key,
        };
        transaction.commit()?;
        Ok(result)
    }
}

#[derive(Clone, Copy)]
enum FundsSide {
    Challenger,
    Recipient,
}

#[derive(Debug)]
struct TrainingResult {
    xp_delta: i64,
    stat_gain: Option<String>,
    reached_cap: bool,
}

fn row_to_brawl(row: &Row<'_>) -> rusqlite::Result<PetBrawl> {
    Ok(PetBrawl {
        brawl_id: row.get(0)?,
        guild_id: row.get(1)?,
        channel_id: row.get(2)?,
        challenger_id: row.get(3)?,
        recipient_id: row.get(4)?,
        challenger_pet_id: row.get(5)?,
        recipient_pet_id: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        expires_at: row.get(9)?,
        resolved_at: row.get(10)?,
        winner_id: row.get(11)?,
        winner_pet_id: row.get(12)?,
        loser_pet_id: row.get(13)?,
        rounds: row.get(14)?,
        winner_hunger_delta: row.get(15)?,
        loser_hunger_delta: row.get(16)?,
        wager: row.get(17)?,
        fee: row.get(18)?,
        challenger_xp_delta: row.get(19)?,
        recipient_xp_delta: row.get(20)?,
        challenger_stat_gain: row.get(21)?,
        recipient_stat_gain: row.get(22)?,
        personality_event_key: row.get(23)?,
    })
}

fn query_brawls<P: rusqlite::Params>(
    transaction: &Transaction<'_>,
    sql: &str,
    parameters: P,
) -> Result<Vec<PetBrawl>, rusqlite::Error> {
    let mut statement = transaction.prepare(sql)?;
    statement
        .query_map(parameters, row_to_brawl)?
        .collect::<Result<Vec<_>, _>>()
}

fn get_brawl_in_transaction(
    transaction: &Transaction<'_>,
    brawl_id: i64,
    guild_id: i64,
) -> Result<Option<PetBrawl>, rusqlite::Error> {
    let sql =
        format!("SELECT {BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?1 AND guild_id = ?2");
    transaction
        .query_row(&sql, params![brawl_id, guild_id], row_to_brawl)
        .optional()
}

fn require_brawl_by_id(
    transaction: &Transaction<'_>,
    brawl_id: i64,
) -> Result<PetBrawl, PetBrawlRepositoryError> {
    let sql = format!("SELECT {BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?1");
    Ok(transaction.query_row(&sql, params![brawl_id], row_to_brawl)?)
}

fn validate_recipient_transition(
    brawl: &PetBrawl,
    recipient_id: i64,
    now: i64,
) -> Result<(), PetBrawlRepositoryError> {
    if brawl.recipient_id != recipient_id {
        return Err(PetBrawlRepositoryError::NotRecipient);
    }
    if brawl.status == "pending" && brawl.expires_at <= now {
        return Err(PetBrawlRepositoryError::Expired);
    }
    if brawl.status != "pending" {
        return Err(PetBrawlRepositoryError::NotPending);
    }
    Ok(())
}

fn raise_transition_error(
    transaction: &Transaction<'_>,
    brawl_id: i64,
    guild_id: i64,
    recipient_id: i64,
    now: i64,
) -> Result<(), PetBrawlRepositoryError> {
    let row = transaction
        .query_row(
            "SELECT recipient_id, status, expires_at FROM pet_brawls
             WHERE brawl_id = ?1 AND guild_id = ?2",
            params![brawl_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((actual_recipient, status, expires_at)) = row else {
        return Err(PetBrawlRepositoryError::NoBrawl);
    };
    if actual_recipient != recipient_id {
        return Err(PetBrawlRepositoryError::NotRecipient);
    }
    if status == "pending" && expires_at <= now {
        return Err(PetBrawlRepositoryError::Expired);
    }
    Err(PetBrawlRepositoryError::NotPending)
}

fn settlement_brawl(
    transaction: &Transaction<'_>,
    brawl_id: i64,
    guild_id: i64,
) -> Result<PetBrawl, PetBrawlRepositoryError> {
    let brawl = get_brawl_in_transaction(transaction, brawl_id, guild_id)?
        .ok_or(PetBrawlRepositoryError::NoBrawl)?;
    if brawl.status == "done" {
        return Err(PetBrawlRepositoryError::AlreadyDone);
    }
    if brawl.status != "active" {
        return Err(PetBrawlRepositoryError::NotActive);
    }
    Ok(brawl)
}

fn unordered_pair_matches(left: (i64, i64), right: (i64, i64)) -> bool {
    (left.0 == right.0 && left.1 == right.1) || (left.0 == right.1 && left.1 == right.0)
}

fn player_balance(
    transaction: &Transaction<'_>,
    player_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![player_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn update_player_balance(
    transaction: &Transaction<'_>,
    brawl: &PetBrawl,
    player_id: i64,
    delta: i64,
    actor_id: Option<i64>,
    reason: &str,
    require_nonnegative: Option<FundsSide>,
) -> Result<(), PetBrawlRepositoryError> {
    if delta == 0 {
        return Ok(());
    }
    set_ledger_context(transaction, brawl, actor_id, reason)?;
    let changed = if require_nonnegative.is_some() {
        transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) + ?1 >= 0",
            params![delta, player_id, brawl.guild_id],
        )?
    } else {
        transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![delta, player_id, brawl.guild_id],
        )?
    };
    if changed != 1 {
        if let Some(side) = require_nonnegative {
            return Err(match side {
                FundsSide::Challenger => PetBrawlRepositoryError::ChallengerFunds,
                FundsSide::Recipient => PetBrawlRepositoryError::RecipientFunds,
            });
        }
        if delta > 0 && player_balance(transaction, player_id, brawl.guild_id)?.is_none() {
            forfeit_to_nonprofit(transaction, brawl.guild_id, delta)?;
            clear_ledger_context(transaction)?;
            return Ok(());
        }
        return Err(PetBrawlRepositoryError::ParticipantBalanceUpdate);
    }
    clear_ledger_context(transaction)?;
    Ok(())
}

fn refund_escrow(
    transaction: &Transaction<'_>,
    brawl: &PetBrawl,
    prior_status: &str,
    actor_id: Option<i64>,
) -> Result<(), PetBrawlRepositoryError> {
    if brawl.wager <= 0 {
        return Ok(());
    }
    update_player_balance(
        transaction,
        brawl,
        brawl.challenger_id,
        brawl.wager + brawl.fee,
        actor_id,
        "challenger_refund",
        None,
    )?;
    if prior_status == "active" {
        update_player_balance(
            transaction,
            brawl,
            brawl.recipient_id,
            brawl.wager,
            actor_id,
            "recipient_refund",
            None,
        )?;
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    brawl: &PetBrawl,
    actor_id: Option<i64>,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'pet_brawl', ?1, 'pet_brawl', ?2, ?3, ?4)",
        params![
            actor_id,
            brawl.brawl_id.to_string(),
            reason,
            format!("{{\"wager\": {}, \"fee\": {}}}", brawl.wager, brawl.fee)
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn forfeit_to_nonprofit(
    transaction: &Transaction<'_>,
    guild_id: i64,
    amount: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
         VALUES (?1, ?2, CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO UPDATE SET
             total_collected = total_collected + excluded.total_collected,
             updated_at = CURRENT_TIMESTAMP",
        params![guild_id, amount],
    )?;
    Ok(())
}

fn apply_training_xp(
    transaction: &Transaction<'_>,
    pet_id: i64,
    guild_id: i64,
    now: i64,
    award: i64,
    proposed_stat: Option<&str>,
) -> Result<TrainingResult, PetBrawlRepositoryError> {
    let row = transaction
        .query_row(
            "SELECT training_xp, training_str, training_int, training_dex
             FROM pets WHERE pet_id = ?1 AND guild_id = ?2",
            params![pet_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((old_xp, strength, intelligence, dexterity)) = row else {
        return Ok(TrainingResult::none());
    };
    if award <= 0 || old_xp >= BRAWL_TRAINING_XP_CAP {
        return Ok(TrainingResult::none());
    }
    let game_day_start =
        game_day_start_ts(now).map_err(|_| PetBrawlRepositoryError::InvalidGameDay)?;
    let rewarded_today: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM pet_brawls
         WHERE guild_id = ?1 AND status = 'done' AND resolved_at >= ?2
           AND ((challenger_pet_id = ?3 AND challenger_xp_delta > 0)
             OR (recipient_pet_id = ?3 AND recipient_xp_delta > 0))",
        params![guild_id, game_day_start, pet_id],
        |row| row.get(0),
    )?;
    if rewarded_today >= BRAWL_XP_DAILY_CAP {
        return Ok(TrainingResult::none());
    }
    let new_xp = BRAWL_TRAINING_XP_CAP.min(old_xp + award);
    let xp_delta = new_xp - old_xp;
    let old_level = old_xp / BRAWL_XP_PER_LEVEL;
    let new_level = new_xp / BRAWL_XP_PER_LEVEL;
    let mut stats = [("str", strength), ("int", intelligence), ("dex", dexterity)];
    let mut stat_gain = None;
    if new_level > old_level
        && stats.iter().map(|(_, value)| *value).sum::<i64>() < BRAWL_TRAINING_LEVEL_CAP
    {
        let selected = proposed_stat
            .filter(|proposed| {
                stats
                    .iter()
                    .any(|(name, value)| name == proposed && *value < BRAWL_TRAINING_STAT_CAP)
            })
            .or_else(|| {
                stats
                    .iter()
                    .find(|(_, value)| *value < BRAWL_TRAINING_STAT_CAP)
                    .map(|(name, _)| *name)
            });
        if let Some(selected) = selected {
            if let Some((_, value)) = stats.iter_mut().find(|(name, _)| *name == selected) {
                *value += 1;
            }
            stat_gain = Some(selected.to_owned());
        }
    }
    transaction.execute(
        "UPDATE pets SET training_xp = ?1, training_str = ?2,
             training_int = ?3, training_dex = ?4
         WHERE pet_id = ?5 AND guild_id = ?6",
        params![new_xp, stats[0].1, stats[1].1, stats[2].1, pet_id, guild_id],
    )?;
    Ok(TrainingResult {
        xp_delta,
        stat_gain,
        reached_cap: new_xp == BRAWL_TRAINING_XP_CAP,
    })
}

impl TrainingResult {
    const fn none() -> Self {
        Self {
            xp_delta: 0,
            stat_gain: None,
            reached_cap: false,
        }
    }
}

fn read_living_hunger(
    transaction: &Transaction<'_>,
    pet_id: i64,
    guild_id: i64,
    now: i64,
    decay_per_day: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    let pet = transaction
        .query_row(
            "SELECT species, last_fed_at, hunger_at_last_fed, died_at
             FROM pets WHERE pet_id = ?1 AND guild_id = ?2 AND is_active=1",
            params![pet_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((species_id, last_fed_at, anchor_hunger, died_at)) = pet else {
        return Ok(None);
    };
    if died_at.is_some() {
        return Ok(None);
    }
    let elapsed = (i128::from(now) - i128::from(last_fed_at)).max(0);
    let numerator =
        elapsed * i128::from(decay_per_day) * i128::from(get_species(&species_id).decay_pct);
    let decayed = numerator.div_euclid(SECONDS_PER_DAY * 100);
    let hunger = (i128::from(anchor_hunger) - decayed).clamp(0, i128::from(i64::MAX)) as i64;
    Ok((hunger > 0).then_some(hunger))
}

#[allow(clippy::too_many_arguments)]
fn apply_winner_gain(
    transaction: &Transaction<'_>,
    pet_id: i64,
    guild_id: i64,
    now: i64,
    decay_per_day: i64,
    gain: i64,
    daily_win_cap: i64,
) -> Result<i64, PetBrawlRepositoryError> {
    let Some(hunger) = read_living_hunger(transaction, pet_id, guild_id, now, decay_per_day)?
    else {
        return Ok(0);
    };
    if gain <= 0 {
        return Ok(0);
    }
    let game_day_start =
        game_day_start_ts(now).map_err(|_| PetBrawlRepositoryError::InvalidGameDay)?;
    let rewarded_today: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM pet_brawls
         WHERE winner_pet_id = ?1 AND guild_id = ?2 AND status = 'done'
           AND winner_hunger_delta > 0 AND resolved_at >= ?3",
        params![pet_id, guild_id, game_day_start],
        |row| row.get(0),
    )?;
    if rewarded_today >= daily_win_cap {
        return Ok(0);
    }
    let new_hunger = 100.min(hunger + gain);
    if new_hunger == hunger {
        return Ok(0);
    }
    write_hunger_anchor(transaction, pet_id, now, new_hunger)?;
    Ok(new_hunger - hunger)
}

#[allow(clippy::too_many_arguments)]
fn apply_loser_loss(
    transaction: &Transaction<'_>,
    pet_id: i64,
    guild_id: i64,
    now: i64,
    decay_per_day: i64,
    loss: i64,
    floor: i64,
) -> Result<i64, PetBrawlRepositoryError> {
    let Some(hunger) = read_living_hunger(transaction, pet_id, guild_id, now, decay_per_day)?
    else {
        return Ok(0);
    };
    let applied = loss.min((hunger - floor).max(0));
    if applied <= 0 {
        return Ok(0);
    }
    write_hunger_anchor(transaction, pet_id, now, hunger - applied)?;
    Ok(-applied)
}

fn write_hunger_anchor(
    transaction: &Transaction<'_>,
    pet_id: i64,
    now: i64,
    hunger: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "UPDATE pets SET last_fed_at = ?1, hunger_at_last_fed = ?2
         WHERE pet_id = ?3 AND died_at IS NULL AND is_active=1",
        params![now, hunger, pet_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::path::Path;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use rusqlite::{Connection, OptionalExtension, params};
    use tempfile::NamedTempFile;

    use super::{
        BrawlSettlement, PetBrawlRepository, PetBrawlRepositoryError, SweepResult,
        read_living_hunger,
    };
    use crate::test_support::FastTestDatabase;

    const GUILD_ID: i64 = 123_456;
    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const DECAY: i64 = 20;

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
        database: FixtureDatabase,
        repository: PetBrawlRepository,
    }

    #[derive(Clone, Copy)]
    struct PetSeed {
        hunger: i64,
        last_fed_at: i64,
        died_at: Option<i64>,
        training_xp: i64,
        training_str: i64,
        training_int: i64,
        training_dex: i64,
    }

    impl Default for PetSeed {
        fn default() -> Self {
            Self {
                hunger: 100,
                last_fed_at: NOW,
                died_at: None,
                training_xp: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
            }
        }
    }

    fn fixture_database(durable: bool) -> FixtureDatabase {
        static TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
        let template = TEMPLATE.get_or_init(|| {
            let file = NamedTempFile::new().expect("create temporary SQLite database");
            let connection = Connection::open(file.path()).expect("open fixture database");
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         discord_username TEXT NOT NULL,
                         jopacoin_balance INTEGER DEFAULT 0,
                         lowest_balance_ever INTEGER DEFAULT 0,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE pets (
                         pet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         name TEXT NOT NULL,
                         species TEXT NOT NULL,
                         adopted_at INTEGER NOT NULL,
                         hatched_at INTEGER NOT NULL,
                         adopt_fee INTEGER NOT NULL,
                         last_fed_at INTEGER NOT NULL,
                         hunger_at_last_fed INTEGER NOT NULL,
                         died_at INTEGER,
                         death_cause TEXT,
                         is_active INTEGER NOT NULL DEFAULT 1,
                         training_xp INTEGER NOT NULL DEFAULT 0,
                         training_str INTEGER NOT NULL DEFAULT 0,
                         training_int INTEGER NOT NULL DEFAULT 0,
                         training_dex INTEGER NOT NULL DEFAULT 0
                     );
                     CREATE TABLE pet_brawls (
                         brawl_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         channel_id INTEGER NOT NULL,
                         challenger_id INTEGER NOT NULL,
                         recipient_id INTEGER NOT NULL,
                         challenger_pet_id INTEGER NOT NULL,
                         recipient_pet_id INTEGER,
                         status TEXT NOT NULL DEFAULT 'pending',
                         created_at INTEGER NOT NULL,
                         expires_at INTEGER NOT NULL,
                         resolved_at INTEGER,
                         winner_id INTEGER,
                         winner_pet_id INTEGER,
                         loser_pet_id INTEGER,
                         rounds INTEGER,
                         winner_hunger_delta INTEGER NOT NULL DEFAULT 0,
                         loser_hunger_delta INTEGER NOT NULL DEFAULT 0,
                         wager INTEGER NOT NULL DEFAULT 0,
                         fee INTEGER NOT NULL DEFAULT 0,
                         challenger_xp_delta INTEGER NOT NULL DEFAULT 0,
                         recipient_xp_delta INTEGER NOT NULL DEFAULT 0,
                         challenger_stat_gain TEXT,
                         recipient_stat_gain TEXT,
                         personality_event_key TEXT
                     );
                     CREATE TABLE nonprofit_fund (
                         guild_id INTEGER PRIMARY KEY,
                         total_collected INTEGER NOT NULL DEFAULT 0,
                         updated_at TEXT
                     );
                     CREATE TABLE economy_ledger_entries (
                         ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         account_type TEXT NOT NULL,
                         account_id INTEGER,
                         delta INTEGER NOT NULL,
                         balance_before INTEGER NOT NULL,
                         balance_after INTEGER NOT NULL,
                         source TEXT NOT NULL DEFAULT 'balance_update',
                         actor_id INTEGER,
                         related_type TEXT,
                         related_id TEXT,
                         reason TEXT,
                         metadata TEXT,
                         created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                     );
                     CREATE TABLE economy_ledger_context (
                         id INTEGER PRIMARY KEY CHECK (id = 1),
                         source TEXT,
                         actor_id INTEGER,
                         related_type TEXT,
                         related_id TEXT,
                         reason TEXT,
                         metadata TEXT
                     );
                     CREATE TRIGGER trg_economy_ledger_players_update
                     AFTER UPDATE OF jopacoin_balance ON players
                     WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                             COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(NEW.jopacoin_balance, 0),
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_economy_ledger_nonprofit_insert
                     AFTER INSERT ON nonprofit_fund
                     WHEN COALESCE(NEW.total_collected, 0) != 0
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                             COALESCE(NEW.total_collected, 0), 0,
                             COALESCE(NEW.total_collected, 0),
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_insert'),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_economy_ledger_nonprofit_update
                     AFTER UPDATE OF total_collected ON nonprofit_fund
                     WHEN COALESCE(OLD.total_collected, 0) != COALESCE(NEW.total_collected, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                             COALESCE(NEW.total_collected, 0) - COALESCE(OLD.total_collected, 0),
                             COALESCE(OLD.total_collected, 0),
                             COALESCE(NEW.total_collected, 0),
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_update'),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;",
                )
                .expect("create Python-compatible pet-brawl schema");
            drop(connection);
            file
        });
        if durable {
            let file = NamedTempFile::new().expect("durable pet-brawl database");
            std::fs::copy(template.path(), file.path()).expect("copy pet-brawl fixture template");
            FixtureDatabase::Durable(file)
        } else {
            FixtureDatabase::Fast(FastTestDatabase::from_template(template.path()))
        }
    }

    impl Fixture {
        fn new() -> Self {
            Self::from_database(fixture_database(false))
        }

        fn durable() -> Self {
            Self::from_database(fixture_database(true))
        }

        fn from_database(database: FixtureDatabase) -> Self {
            let repository = PetBrawlRepository::new(database.path());
            Self {
                database,
                repository,
            }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.database.path()).expect("open fixture database")
        }

        fn seed_player(&self, discord_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username,
                         jopacoin_balance, lowest_balance_ever
                     ) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![
                        discord_id,
                        GUILD_ID,
                        format!("Player {discord_id}"),
                        balance
                    ],
                )
                .expect("seed player");
        }

        fn insert_pet(&self, discord_id: i64, seed: PetSeed) -> i64 {
            let connection = self.connection();
            connection
                .execute(
                    "INSERT INTO pets (
                         discord_id, guild_id, name, species, adopted_at, hatched_at,
                         adopt_fee, last_fed_at, hunger_at_last_fed, died_at,
                         training_xp, training_str, training_int, training_dex
                     ) VALUES (?1, ?2, 'Brawler', 'common_cama', ?3, ?4, 20,
                               ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        discord_id,
                        GUILD_ID,
                        NOW - 10 * DAY,
                        NOW - 9 * DAY,
                        seed.last_fed_at,
                        seed.hunger,
                        seed.died_at,
                        seed.training_xp,
                        seed.training_str,
                        seed.training_int,
                        seed.training_dex
                    ],
                )
                .expect("seed pet");
            connection.last_insert_rowid()
        }

        fn make_pending(
            &self,
            challenger: i64,
            recipient: i64,
            wager: i64,
            fee: i64,
        ) -> (cama_domain::pet_brawl::PetBrawl, i64, i64) {
            let pet_a = self.insert_pet(challenger, PetSeed::default());
            let pet_b = self.insert_pet(recipient, PetSeed::default());
            let brawl = self
                .repository
                .create_brawl_atomic(
                    Some(GUILD_ID),
                    555,
                    challenger,
                    recipient,
                    pet_a,
                    NOW,
                    NOW + 180,
                    wager,
                    fee,
                )
                .expect("create pending brawl");
            (brawl, pet_a, pet_b)
        }

        fn make_active(
            &self,
            challenger: i64,
            recipient: i64,
        ) -> (cama_domain::pet_brawl::PetBrawl, i64, i64) {
            let (brawl, pet_a, pet_b) = self.make_pending(challenger, recipient, 0, 0);
            let brawl = self
                .repository
                .accept_atomic(brawl.brawl_id, Some(GUILD_ID), recipient, pet_b, NOW + 5)
                .expect("accept brawl");
            (brawl, pet_a, pet_b)
        }

        fn settle(
            &self,
            brawl_id: i64,
            winner_pet_id: i64,
            loser_pet_id: i64,
            winner_id: i64,
            now: i64,
        ) -> super::BrawlSettlementResult {
            self.repository
                .settle_brawl_atomic(
                    brawl_id,
                    Some(GUILD_ID),
                    settlement(winner_id, winner_pet_id, loser_pet_id, now),
                )
                .expect("settle active brawl")
        }

        fn balance(&self, discord_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT COALESCE(jopacoin_balance, 0) FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, GUILD_ID],
                    |row| row.get(0),
                )
                .expect("read player balance")
        }

        fn nonprofit_balance(&self) -> i64 {
            self.connection()
                .query_row(
                    "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
                    params![GUILD_ID],
                    |row| row.get(0),
                )
                .optional()
                .expect("read nonprofit balance")
                .unwrap_or(0)
        }

        fn pet_hunger(&self, pet_id: i64, now: i64) -> i64 {
            let mut connection = self.connection();
            let transaction = connection.transaction().expect("start read transaction");
            read_living_hunger(&transaction, pet_id, GUILD_ID, now, DECAY)
                .expect("derive hunger")
                .unwrap_or(0)
        }

        fn training(&self, pet_id: i64) -> (i64, i64, i64, i64) {
            self.connection()
                .query_row(
                    "SELECT training_xp, training_str, training_int, training_dex
                     FROM pets WHERE pet_id = ?1",
                    params![pet_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .expect("read pet training")
        }
    }

    fn settlement(
        winner_id: i64,
        winner_pet_id: i64,
        loser_pet_id: i64,
        now: i64,
    ) -> BrawlSettlement<'static> {
        BrawlSettlement {
            winner_id,
            winner_pet_id,
            loser_pet_id,
            rounds: 4,
            now,
            decay_per_day: DECAY,
            winner_gain: 10,
            loser_loss: 15,
            loss_floor: 10,
            daily_win_cap: 3,
            winner_stat_gain: None,
            loser_stat_gain: None,
            personality_event_key: None,
        }
    }

    fn assert_code<T: Debug>(result: Result<T, PetBrawlRepositoryError>, expected_code: &str) {
        assert_eq!(
            result.expect_err("operation must be rejected").to_string(),
            expected_code
        );
    }

    #[test]
    fn test_create_and_read() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_pending(100, 200, 0, 0);
        assert_eq!(brawl.status, "pending");
        assert_eq!(brawl.challenger_pet_id, pet_a);
        assert_eq!(brawl.recipient_pet_id, None);
        assert_eq!(
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(GUILD_ID))
                .unwrap(),
            Some(brawl.clone())
        );
        let accepted = fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        assert_eq!(accepted.status, "active");
        assert_eq!(accepted.recipient_pet_id, Some(pet_b));
    }

    #[test]
    fn test_one_open_brawl_per_player_cross_role() {
        let fixture = Fixture::new();
        fixture.make_pending(100, 200, 0, 0);
        let pet_c = fixture.insert_pet(300, PetSeed::default());
        assert_code(
            fixture.repository.create_brawl_atomic(
                Some(GUILD_ID),
                555,
                300,
                100,
                pet_c,
                NOW,
                NOW + 180,
                0,
                0,
            ),
            "brawl_busy",
        );
        assert_code(
            fixture.repository.create_brawl_atomic(
                Some(GUILD_ID),
                555,
                200,
                300,
                pet_c,
                NOW,
                NOW + 180,
                0,
                0,
            ),
            "brawl_busy",
        );
        let pet_d = fixture.insert_pet(400, PetSeed::default());
        fixture
            .repository
            .create_brawl_atomic(Some(GUILD_ID), 555, 300, 400, pet_c, NOW, NOW + 180, 0, 0)
            .unwrap();
        assert!(pet_d > 0);
    }

    #[test]
    fn test_closed_brawls_do_not_block() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        let again = fixture
            .repository
            .create_brawl_atomic(
                Some(GUILD_ID),
                555,
                100,
                200,
                pet_a,
                NOW + 100,
                NOW + 280,
                0,
                0,
            )
            .unwrap();
        assert_eq!(again.status, "pending");
    }

    #[test]
    fn test_accept_guards() {
        let fixture = Fixture::new();
        let (brawl, _, pet_b) = fixture.make_pending(100, 200, 0, 0);
        assert_code(
            fixture
                .repository
                .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 999, pet_b, NOW),
            "not_recipient",
        );
        assert_code(
            fixture
                .repository
                .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 999),
            "expired",
        );
        assert_code(
            fixture
                .repository
                .accept_atomic(12_345, Some(GUILD_ID), 200, pet_b, NOW),
            "no_brawl",
        );
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        assert_code(
            fixture
                .repository
                .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 6),
            "not_pending",
        );
    }

    #[test]
    fn test_decline() {
        let fixture = Fixture::new();
        let (brawl, _, _) = fixture.make_pending(100, 200, 0, 0);
        fixture
            .repository
            .decline_atomic(brawl.brawl_id, Some(GUILD_ID), 200, NOW + 5)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "declined"
        );
        assert_code(
            fixture
                .repository
                .decline_atomic(brawl.brawl_id, Some(GUILD_ID), 200, NOW + 6),
            "not_pending",
        );
    }

    #[test]
    fn test_void_open_states_only() {
        let fixture = Fixture::new();
        let (brawl, _, _) = fixture.make_pending(100, 200, 0, 0);
        fixture
            .repository
            .void_atomic(brawl.brawl_id, Some(GUILD_ID), NOW + 5)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "void"
        );
        assert_code(
            fixture
                .repository
                .void_atomic(brawl.brawl_id, Some(GUILD_ID), NOW + 6),
            "not_open",
        );
    }

    #[test]
    fn test_withdraw_atomic_pending_only() {
        let fixture = Fixture::new();
        let (brawl, _, _) = fixture.make_pending(100, 200, 0, 0);
        assert_code(
            fixture
                .repository
                .withdraw_atomic(brawl.brawl_id, Some(GUILD_ID), 200, NOW + 4),
            "not_challenger",
        );
        assert_code(
            fixture
                .repository
                .withdraw_atomic(12_345, Some(GUILD_ID), 100, NOW + 4),
            "no_brawl",
        );
        fixture
            .repository
            .withdraw_atomic(brawl.brawl_id, Some(GUILD_ID), 100, NOW + 5)
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "void"
        );
        let (active, _, _) = fixture.make_active(300, 400);
        assert_code(
            fixture
                .repository
                .withdraw_atomic(active.brawl_id, Some(GUILD_ID), 300, NOW + 6),
            "not_pending",
        );
    }

    #[test]
    fn test_wager_escrows_both_sides_and_settlement_pays_pot_and_reserve() {
        let fixture = Fixture::new();
        fixture.seed_player(100, 100);
        fixture.seed_player(200, 100);
        let (brawl, pet_a, pet_b) = fixture.make_pending(100, 200, 20, 1);
        assert_eq!((brawl.wager, brawl.fee), (20, 1));
        assert_eq!((fixture.balance(100), fixture.balance(200)), (79, 100));
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        assert_eq!((fixture.balance(100), fixture.balance(200)), (79, 80));
        let mut input = settlement(100, pet_a, pet_b, NOW + 60);
        input.winner_stat_gain = Some("str");
        input.loser_stat_gain = Some("int");
        let result = fixture
            .repository
            .settle_brawl_atomic(brawl.brawl_id, Some(GUILD_ID), input)
            .unwrap();
        assert_eq!(result.payout, 40);
        assert_eq!((fixture.balance(100), fixture.balance(200)), (119, 80));
        assert_eq!(fixture.nonprofit_balance(), 1);
        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT delta, source, related_type, related_id, reason
                 FROM economy_ledger_entries WHERE related_type = 'pet_brawl'
                 ORDER BY ledger_id",
            )
            .unwrap();
        let ledger = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            ledger
                .iter()
                .map(|row| (row.0, row.1.as_str(), row.2.as_str(), row.4.as_str()))
                .collect::<Vec<_>>(),
            [
                (-21, "pet_brawl", "pet_brawl", "challenger_escrow"),
                (-20, "pet_brawl", "pet_brawl", "recipient_escrow"),
                (40, "pet_brawl", "pet_brawl", "winner_payout"),
                (1, "pet_brawl", "pet_brawl", "brawl_fee_reserve_credit"),
            ]
        );
        assert!(ledger.iter().all(|row| row.3 == brawl.brawl_id.to_string()));
    }

    #[test]
    fn test_pending_decline_refunds_challenger_wager_and_fee() {
        let fixture = Fixture::new();
        fixture.seed_player(100, 100);
        fixture.seed_player(200, 100);
        let (brawl, _, _) = fixture.make_pending(100, 200, 20, 1);
        fixture
            .repository
            .decline_atomic(brawl.brawl_id, Some(GUILD_ID), 200, NOW + 5)
            .unwrap();
        assert_eq!((fixture.balance(100), fixture.balance(200)), (100, 100));
        assert_eq!(fixture.nonprofit_balance(), 0);
    }

    #[test]
    fn test_active_void_refunds_both_stakes_and_fee() {
        let fixture = Fixture::new();
        fixture.seed_player(100, 100);
        fixture.seed_player(200, 100);
        let (brawl, _, pet_b) = fixture.make_pending(100, 200, 20, 1);
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        fixture
            .repository
            .void_atomic(brawl.brawl_id, Some(GUILD_ID), NOW + 6)
            .unwrap();
        assert_eq!((fixture.balance(100), fixture.balance(200)), (100, 100));
        assert_eq!(fixture.nonprofit_balance(), 0);
    }

    #[test]
    fn test_draw_finalizes_without_rewards_and_refunds_all_escrow() {
        let fixture = Fixture::new();
        fixture.seed_player(100, 100);
        fixture.seed_player(200, 100);
        let (brawl, pet_a, pet_b) = fixture.make_pending(100, 200, 20, 1);
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        let result = fixture
            .repository
            .settle_draw_atomic(brawl.brawl_id, Some(GUILD_ID), (pet_a, pet_b), 8, NOW + 60)
            .unwrap();
        assert_eq!((result.wager, result.fee), (20, 1));
        assert_eq!((fixture.balance(100), fixture.balance(200)), (100, 100));
        assert_eq!(fixture.nonprofit_balance(), 0);
        let done = fixture
            .repository
            .get_brawl(brawl.brawl_id, Some(GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.rounds, Some(8));
        assert_eq!(
            (done.winner_id, done.winner_pet_id, done.loser_pet_id),
            (None, None, None)
        );
        assert_eq!(
            fixture
                .repository
                .get_records_for(&[pet_a, pet_b], Some(GUILD_ID))
                .unwrap(),
            [(pet_a, (0, 0)), (pet_b, (0, 0))].into_iter().collect()
        );
        assert_eq!(fixture.training(pet_a).0, 0);
        assert_eq!(fixture.training(pet_b).0, 0);
    }

    #[test]
    fn test_accept_insufficient_funds_keeps_pending_escrow_refundable() {
        let fixture = Fixture::new();
        fixture.seed_player(100, 100);
        fixture.seed_player(200, 20);
        let (brawl, _, pet_b) = fixture.make_pending(100, 200, 20, 1);
        fixture
            .connection()
            .execute(
                "UPDATE players SET jopacoin_balance = 0 WHERE discord_id = 200 AND guild_id = ?1",
                params![GUILD_ID],
            )
            .unwrap();
        assert_code(
            fixture
                .repository
                .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5),
            "recipient_funds",
        );
        assert_eq!(
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
        fixture
            .repository
            .withdraw_atomic(brawl.brawl_id, Some(GUILD_ID), 100, NOW + 6)
            .unwrap();
        assert_eq!((fixture.balance(100), fixture.balance(200)), (100, 0));
    }

    #[test]
    fn test_stale_sweep_refunds_pending_and_active_wagers() {
        let fixture = Fixture::new();
        for player_id in [100, 200, 300, 400] {
            fixture.seed_player(player_id, 100);
        }
        let (pending, _, _) = fixture.make_pending(100, 200, 20, 1);
        let (active, _, active_pet) = fixture.make_pending(300, 400, 20, 1);
        fixture
            .repository
            .accept_atomic(active.brawl_id, Some(GUILD_ID), 400, active_pet, NOW + 1)
            .unwrap();
        assert_eq!(
            fixture.repository.sweep_stale(NOW + 20, 1_800).unwrap(),
            SweepResult {
                expired: 0,
                voided: 0
            }
        );
        assert_eq!(
            fixture
                .repository
                .get_brawl(pending.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
        assert_eq!(
            fixture.repository.sweep_stale(NOW + 10_000, 1_800).unwrap(),
            SweepResult {
                expired: 1,
                voided: 1
            }
        );
        assert_eq!(
            fixture
                .repository
                .get_brawl(pending.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "expired"
        );
        assert_eq!(
            fixture
                .repository
                .get_brawl(active.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "void"
        );
        assert_eq!(
            [100, 200, 300, 400].map(|player_id| fixture.balance(player_id)),
            [100, 100, 100, 100]
        );
    }

    #[test]
    fn test_new_challenge_cleans_up_expired_pending_brawl() {
        let fixture = Fixture::new();
        for player_id in [100, 200, 300] {
            fixture.seed_player(player_id, 100);
        }
        let (stale, challenger_pet, _) = fixture.make_pending(100, 200, 20, 1);
        fixture.insert_pet(300, PetSeed::default());
        let replacement = fixture
            .repository
            .create_brawl_atomic(
                Some(GUILD_ID),
                555,
                100,
                300,
                challenger_pet,
                NOW + 181,
                NOW + 361,
                0,
                0,
            )
            .unwrap();
        assert_eq!(
            fixture
                .repository
                .get_brawl(stale.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "expired"
        );
        assert_eq!(replacement.status, "pending");
        assert_eq!(fixture.balance(100), 100);
    }

    #[test]
    fn test_happy_path_applies_gain_and_loss() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture
            .connection()
            .execute(
                "UPDATE pets SET hunger_at_last_fed = 50, last_fed_at = ?1 WHERE pet_id = ?2",
                params![NOW + 60, pet_a],
            )
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        assert_eq!(result.winner_delta, 10);
        assert_eq!(result.loser_delta, -15);
        assert_eq!(fixture.pet_hunger(pet_a, NOW + 60), 60);
        assert_eq!(fixture.pet_hunger(pet_b, NOW + 60), 85);
        let connection = fixture.connection();
        let died_at: Option<i64> = connection
            .query_row(
                "SELECT died_at FROM pets WHERE pet_id = ?1",
                params![pet_b],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(died_at, None);
        let done = fixture
            .repository
            .get_brawl(brawl.brawl_id, Some(GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.winner_id, Some(100));
        assert_eq!(done.rounds, Some(4));
        assert_eq!(
            (done.winner_hunger_delta, done.loser_hunger_delta),
            (10, -15)
        );
    }

    #[test]
    fn test_settlement_awards_xp_and_persists_threshold_stat_gain() {
        let fixture = Fixture::new();
        let pet_a = fixture.insert_pet(
            100,
            PetSeed {
                training_xp: 4,
                ..PetSeed::default()
            },
        );
        let pet_b = fixture.insert_pet(
            200,
            PetSeed {
                training_xp: 4,
                ..PetSeed::default()
            },
        );
        let brawl = fixture
            .repository
            .create_brawl_atomic(Some(GUILD_ID), 555, 100, 200, pet_a, NOW, NOW + 180, 0, 0)
            .unwrap();
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 5)
            .unwrap();
        let mut input = settlement(100, pet_a, pet_b, NOW + 60);
        input.winner_stat_gain = Some("str");
        input.loser_stat_gain = Some("dex");
        input.personality_event_key = Some("str.victory");
        let result = fixture
            .repository
            .settle_brawl_atomic(brawl.brawl_id, Some(GUILD_ID), input)
            .unwrap();
        assert_eq!(fixture.training(pet_a), (6, 1, 0, 0));
        assert_eq!(fixture.training(pet_b), (5, 0, 0, 1));
        assert_eq!((result.winner_xp_delta, result.loser_xp_delta), (2, 1));
        assert_eq!(result.winner_stat_gain.as_deref(), Some("str"));
        assert_eq!(result.loser_stat_gain.as_deref(), Some("dex"));
        let done = fixture
            .repository
            .get_brawl(brawl.brawl_id, Some(GUILD_ID))
            .unwrap()
            .unwrap();
        assert_eq!((done.challenger_xp_delta, done.recipient_xp_delta), (2, 1));
        assert_eq!(done.challenger_stat_gain.as_deref(), Some("str"));
        assert_eq!(done.recipient_stat_gain.as_deref(), Some("dex"));
        assert_eq!(done.personality_event_key.as_deref(), Some("str.victory"));
    }

    #[test]
    fn test_loser_loss_floors_at_ten() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture
            .connection()
            .execute(
                "UPDATE pets SET hunger_at_last_fed = 12, last_fed_at = ?1 WHERE pet_id = ?2",
                params![NOW + 60, pet_b],
            )
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        assert_eq!(result.loser_delta, -2);
        assert_eq!(fixture.pet_hunger(pet_b, NOW + 60), 10);
        assert_eq!(result.winner_delta, 0);
        assert!(fixture.pet_hunger(pet_a, NOW + 60) <= 100);

        let (brawl, pet_c, pet_d) = fixture.make_active(300, 400);
        fixture
            .connection()
            .execute(
                "UPDATE pets SET hunger_at_last_fed = 8, last_fed_at = ?1 WHERE pet_id = ?2",
                params![NOW + 60, pet_d],
            )
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, pet_c, pet_d, 300, NOW + 60);
        assert_eq!(result.loser_delta, 0);
        assert_eq!(fixture.pet_hunger(pet_d, NOW + 60), 8);
    }

    #[test]
    fn test_skips_pet_that_died_mid_brawl() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture
            .connection()
            .execute(
                "UPDATE pets SET died_at = ?1, death_cause = 'starvation', is_active = 0
                 WHERE pet_id = ?2",
                params![NOW + 30, pet_b],
            )
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        assert_eq!(result.loser_delta, 0);
        let died_at = fixture
            .connection()
            .query_row(
                "SELECT died_at FROM pets WHERE pet_id = ?1",
                params![pet_b],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(died_at, NOW + 30);
    }

    #[test]
    fn test_skips_derived_starved_pet() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture
            .connection()
            .execute(
                "UPDATE pets SET hunger_at_last_fed = 100, last_fed_at = ?1 WHERE pet_id = ?2",
                params![NOW - 30 * DAY, pet_b],
            )
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        assert_eq!(result.loser_delta, 0);
        let (last_fed_at, died_at) = fixture
            .connection()
            .query_row(
                "SELECT last_fed_at, died_at FROM pets WHERE pet_id = ?1",
                params![pet_b],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .unwrap();
        assert_eq!(last_fed_at, NOW - 30 * DAY);
        assert_eq!(died_at, None);
    }

    #[test]
    fn test_double_settle_rejected() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        assert_code(
            fixture.repository.settle_brawl_atomic(
                brawl.brawl_id,
                Some(GUILD_ID),
                settlement(100, pet_a, pet_b, NOW + 60),
            ),
            "already_done",
        );
    }

    #[test]
    fn test_settle_requires_active() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_pending(100, 200, 0, 0);
        assert_code(
            fixture.repository.settle_brawl_atomic(
                brawl.brawl_id,
                Some(GUILD_ID),
                settlement(100, pet_a, pet_b, NOW + 60),
            ),
            "not_active",
        );
    }

    #[test]
    fn test_daily_win_cap_and_next_day_reset() {
        let fixture = Fixture::new();
        let winner_pet = fixture.insert_pet(
            100,
            PetSeed {
                hunger: 50,
                ..PetSeed::default()
            },
        );
        let mut deltas = Vec::new();
        let mut xp_deltas = Vec::new();
        for index in 0..4 {
            let opponent = 200 + index;
            let opponent_pet = fixture.insert_pet(opponent, PetSeed::default());
            let created_at = NOW + index * 10;
            let brawl = fixture
                .repository
                .create_brawl_atomic(
                    Some(GUILD_ID),
                    555,
                    100,
                    opponent,
                    winner_pet,
                    created_at,
                    created_at + 180,
                    0,
                    0,
                )
                .unwrap();
            fixture
                .repository
                .accept_atomic(
                    brawl.brawl_id,
                    Some(GUILD_ID),
                    opponent,
                    opponent_pet,
                    created_at + 1,
                )
                .unwrap();
            let mut input = settlement(100, winner_pet, opponent_pet, created_at + 2);
            input.winner_stat_gain = Some("dex");
            let result = fixture
                .repository
                .settle_brawl_atomic(brawl.brawl_id, Some(GUILD_ID), input)
                .unwrap();
            deltas.push(result.winner_delta);
            xp_deltas.push(result.winner_xp_delta);
        }
        assert_eq!(deltas, [10, 10, 10, 0]);
        assert_eq!(xp_deltas, [2, 2, 2, 0]);
        assert_eq!(fixture.training(winner_pet), (6, 0, 0, 1));
        assert_eq!(
            fixture
                .repository
                .get_pet_record(winner_pet, Some(GUILD_ID))
                .unwrap(),
            (4, 0)
        );
        let later = NOW + 2 * DAY;
        let opponent_pet = fixture.insert_pet(999, PetSeed::default());
        let brawl = fixture
            .repository
            .create_brawl_atomic(
                Some(GUILD_ID),
                555,
                100,
                999,
                winner_pet,
                later,
                later + 180,
                0,
                0,
            )
            .unwrap();
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 999, opponent_pet, later + 1)
            .unwrap();
        let result = fixture.settle(brawl.brawl_id, winner_pet, opponent_pet, 100, later + 2);
        assert!(result.winner_delta > 0);
    }

    #[test]
    fn test_get_pet_record_and_batch() {
        let fixture = Fixture::new();
        let (brawl, pet_a, pet_b) = fixture.make_active(100, 200);
        fixture.settle(brawl.brawl_id, pet_a, pet_b, 100, NOW + 60);
        let brawl = fixture
            .repository
            .create_brawl_atomic(
                Some(GUILD_ID),
                555,
                100,
                200,
                pet_a,
                NOW + 90,
                NOW + 270,
                0,
                0,
            )
            .unwrap();
        fixture
            .repository
            .accept_atomic(brawl.brawl_id, Some(GUILD_ID), 200, pet_b, NOW + 100)
            .unwrap();
        fixture.settle(brawl.brawl_id, pet_b, pet_a, 200, NOW + 200);
        assert_eq!(
            fixture
                .repository
                .get_pet_record(pet_a, Some(GUILD_ID))
                .unwrap(),
            (1, 1)
        );
        assert_eq!(
            fixture
                .repository
                .get_pet_record(pet_b, Some(GUILD_ID))
                .unwrap(),
            (1, 1)
        );
        let batch = fixture
            .repository
            .get_records_for(&[pet_a, pet_b, 777], Some(GUILD_ID))
            .unwrap();
        assert_eq!(batch[&pet_a], (1, 1));
        assert_eq!(batch[&pet_b], (1, 1));
        assert_eq!(batch[&777], (0, 0));
        assert!(
            fixture
                .repository
                .get_records_for(&[], Some(GUILD_ID))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_sweep_still_refunds_others_when_a_payee_row_is_gone() {
        let fixture = Fixture::new();
        for player_id in [100, 200, 300, 400] {
            fixture.seed_player(player_id, 500);
        }
        let (doomed, _, _) = fixture.make_pending(100, 200, 50, 5);
        let (healthy, _, _) = fixture.make_pending(300, 400, 40, 4);
        let balance_before = fixture.balance(300);
        fixture
            .connection()
            .execute(
                "DELETE FROM players WHERE discord_id = 100 AND guild_id = ?1",
                params![GUILD_ID],
            )
            .unwrap();
        let result = fixture.repository.sweep_stale(NOW + 10_000, 1_800).unwrap();
        assert_eq!(result.expired, 2);
        assert_eq!(fixture.balance(300), balance_before + 44);
        assert!(fixture.nonprofit_balance() >= 55);
        assert_eq!(
            fixture
                .repository
                .get_brawl(doomed.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "expired"
        );
        assert_eq!(
            fixture
                .repository
                .get_brawl(healthy.brawl_id, Some(GUILD_ID))
                .unwrap()
                .unwrap()
                .status,
            "expired"
        );
    }

    #[test]
    fn invariant_none_guild_is_the_zero_sentinel() {
        let fixture = Fixture::new();
        let connection = fixture.connection();
        for discord_id in [100, 200] {
            connection
                .execute(
                    "INSERT INTO pets (
                         discord_id, guild_id, name, species, adopted_at, hatched_at,
                         adopt_fee, last_fed_at, hunger_at_last_fed
                     ) VALUES (?1, 0, 'Brawler', 'common_cama', ?2, ?3, 20, ?4, 100)",
                    params![discord_id, NOW - 10 * DAY, NOW - 9 * DAY, NOW],
                )
                .unwrap();
        }
        let pet_id = connection
            .query_row(
                "SELECT pet_id FROM pets WHERE discord_id = 100 AND guild_id = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        let brawl = fixture
            .repository
            .create_brawl_atomic(None, 555, 100, 200, pet_id, NOW, NOW + 180, 0, 0)
            .unwrap();
        assert_eq!(brawl.guild_id, 0);
        assert_eq!(
            fixture.repository.get_brawl(brawl.brawl_id, None).unwrap(),
            fixture
                .repository
                .get_brawl(brawl.brawl_id, Some(0))
                .unwrap()
        );
    }

    #[test]
    fn invariant_concurrent_create_serializes_cross_role_guard() {
        let fixture = Fixture::durable();
        let pet_a = fixture.insert_pet(100, PetSeed::default());
        fixture.insert_pet(200, PetSeed::default());
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository.create_brawl_atomic(
                        Some(GUILD_ID),
                        555,
                        100,
                        200,
                        pet_a,
                        NOW,
                        NOW + 180,
                        0,
                        0,
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("create thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(PetBrawlRepositoryError::BrawlBusy)))
                .count(),
            1
        );
    }
}
