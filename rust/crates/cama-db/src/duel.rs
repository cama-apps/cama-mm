//! Python-compatible duel challenge persistence.
//!
//! Python remains the only production schema-migration authority during the
//! side-by-side rewrite. This module opens existing SQLite files without the
//! create flag and never creates or migrates caller data. Every state-changing
//! operation uses `BEGIN IMMEDIATE`, matching `BaseRepository.atomic_transaction`,
//! so escrow, settlement, and reminder claims serialize across processes.

use std::path::{Path, PathBuf};

use cama_domain::rating::cap_glicko_rd;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const DUEL_ISSUANCE_FEE: i64 = 50;
pub const DAY_SECONDS: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuelStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
    Resolved,
    Voided,
    DeliveryFailed,
}

impl DuelStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Resolved => "resolved",
            Self::Voided => "voided",
            Self::DeliveryFailed => "delivery_failed",
        }
    }

    fn from_database(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "declined" => Some(Self::Declined),
            "expired" => Some(Self::Expired),
            "resolved" => Some(Self::Resolved),
            "voided" => Some(Self::Voided),
            "delivery_failed" => Some(Self::DeliveryFailed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuelTrial {
    TrialByCombat,
    TrialOfFive,
}

impl DuelTrial {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrialByCombat => "trial_by_combat",
            Self::TrialOfFive => "trial_of_five",
        }
    }

    fn from_database(value: &str) -> Option<Self> {
        match value {
            "trial_by_combat" => Some(Self::TrialByCombat),
            "trial_of_five" => Some(Self::TrialOfFive),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuelDueKind {
    Reminder,
    Expired,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DuelChallenge {
    pub challenge_id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub message_id: Option<i64>,
    pub challenger_id: i64,
    pub recipient_id: i64,
    pub wager: i64,
    pub issuance_fee: i64,
    pub status: DuelStatus,
    pub trial_type: Option<DuelTrial>,
    pub challenger_glicko: f64,
    pub challenger_rd: f64,
    pub recipient_glicko: f64,
    pub recipient_rd: f64,
    pub created_at: i64,
    pub expires_at: i64,
    pub next_reminder_at: Option<i64>,
    pub responded_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub winner_id: Option<i64>,
    pub resolution_actor_id: Option<i64>,
}

impl DuelChallenge {
    #[must_use]
    pub const fn decline_penalty(&self) -> i64 {
        (self.wager + 1) / 2
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DuelDueResult {
    pub kind: DuelDueKind,
    pub challenge: DuelChallenge,
    pub remaining_seconds: i64,
    pub ping_recipient: bool,
    pub claimed_reminder_at: Option<i64>,
    pub is_redelivery: bool,
}

/// Preserve Python's distinction between an `int` wager and any float,
/// including an integral-looking float such as `500.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DuelWager {
    Integer(i64),
    Real(f64),
}

impl From<i64> for DuelWager {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for DuelWager {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<f64> for DuelWager {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

#[derive(Debug, Error)]
pub enum DuelRepositoryError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("The challenged player cannot cover the duel wager.")]
    RecipientFunding { recipient_id: i64, wager: i64 },
    #[error("duel SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DuelChallengeRepository {
    path: PathBuf,
}

impl DuelChallengeRepository {
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

    pub fn get_challenge(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<DuelChallenge>, DuelRepositoryError> {
        let connection = self.connection()?;
        find_challenge(
            &connection,
            challenge_id,
            Self::normalize_guild_id(guild_id),
        )
    }

    pub fn get_pending_for_recipient(
        &self,
        recipient_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<DuelChallenge>, DuelRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE guild_id = ?1 AND recipient_id = ?2 AND status = 'pending'
                   AND message_id IS NOT NULL
                 ORDER BY created_at DESC, challenge_id DESC
                 LIMIT 1",
                params![Self::normalize_guild_id(guild_id), recipient_id],
                row_to_challenge,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_outstanding(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<DuelChallenge>, DuelRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM duel_challenges
             WHERE guild_id = ?1
               AND (status = 'accepted' OR (status = 'pending' AND message_id IS NOT NULL))
             ORDER BY created_at DESC, challenge_id DESC",
        )?;
        Ok(statement
            .query_map(
                params![Self::normalize_guild_id(guild_id)],
                row_to_challenge,
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_pending_all(&self) -> Result<Vec<DuelChallenge>, DuelRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM duel_challenges
             WHERE status = 'pending'
             ORDER BY created_at DESC, challenge_id DESC",
        )?;
        Ok(statement
            .query_map([], row_to_challenge)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_challenge_atomic<W: Into<DuelWager>>(
        &self,
        guild_id: Option<i64>,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: W,
        now: i64,
        challenger_cooldown_seconds: i64,
        recipient_cooldown_seconds: i64,
        response_seconds: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        if challenger_id == recipient_id {
            return Err(DuelRepositoryError::Invalid(
                "You cannot challenge yourself to a duel.",
            ));
        }
        let wager = match wager.into() {
            DuelWager::Integer(wager) => wager,
            DuelWager::Real(_) => {
                return Err(DuelRepositoryError::Invalid(
                    "The wager must be a whole number of jopacoin.",
                ));
            }
        };
        if !(500..=1000).contains(&wager) {
            return Err(DuelRepositoryError::Invalid(
                "The wager must be between 500 and 1000 jopacoin.",
            ));
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let upfront_cost = wager + DUEL_ISSUANCE_FEE;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let challenger = find_player_snapshot(&transaction, challenger_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Both duelists must be registered in this server."),
        )?;
        let recipient = find_player_snapshot(&transaction, recipient_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Both duelists must be registered in this server."),
        )?;
        let (challenger_glicko, challenger_rd) =
            challenger
                .rated_values()
                .ok_or(DuelRepositoryError::Invalid(
                    "Both duelists must be Glicko rated.",
                ))?;
        let (recipient_glicko, recipient_rd) =
            recipient
                .rated_values()
                .ok_or(DuelRepositoryError::Invalid(
                    "Both duelists must be Glicko rated.",
                ))?;
        if recipient_glicko < challenger_glicko {
            return Err(DuelRepositoryError::Invalid(
                "Challenges of honor forbid punching down in Glicko rating.",
            ));
        }

        let unresolved = transaction
            .query_row(
                "SELECT 1 FROM duel_challenges
                 WHERE guild_id = ?1 AND status IN ('pending', 'accepted')
                   AND (challenger_id IN (?2, ?3) OR recipient_id IN (?2, ?3))
                 LIMIT 1",
                params![guild_id, challenger_id, recipient_id],
                |_| Ok(()),
            )
            .optional()?;
        if unresolved.is_some() {
            return Err(DuelRepositoryError::Invalid(
                "One of these players is already involved in an unresolved duel.",
            ));
        }

        let challenger_history = transaction
            .query_row(
                "SELECT 1 FROM duel_challenges
                 WHERE guild_id = ?1 AND challenger_id = ?2
                   AND status != 'delivery_failed' AND created_at > ?3
                 LIMIT 1",
                params![guild_id, challenger_id, now - challenger_cooldown_seconds],
                |_| Ok(()),
            )
            .optional()?;
        if challenger_history.is_some() {
            return Err(DuelRepositoryError::Invalid(
                "Your duel challenge cooldown has not elapsed.",
            ));
        }
        let recipient_history = transaction
            .query_row(
                "SELECT 1 FROM duel_challenges
                 WHERE guild_id = ?1 AND recipient_id = ?2
                   AND status != 'delivery_failed' AND created_at > ?3
                 LIMIT 1",
                params![guild_id, recipient_id, now - recipient_cooldown_seconds],
                |_| Ok(()),
            )
            .optional()?;
        if recipient_history.is_some() {
            return Err(DuelRepositoryError::Invalid(
                "That player was recently challenged and has weekly protection.",
            ));
        }
        if challenger.balance < upfront_cost {
            return Err(DuelRepositoryError::Invalid(
                "Your jopacoin balance cannot cover the wager and issuance fee.",
            ));
        }
        if recipient.balance < wager {
            return Err(DuelRepositoryError::RecipientFunding {
                recipient_id,
                wager,
            });
        }

        let expires_at = now + response_seconds;
        transaction.execute(
            "INSERT INTO duel_challenges (
                 guild_id, channel_id, challenger_id, recipient_id, wager,
                 issuance_fee, status, challenger_glicko, challenger_rd,
                 recipient_glicko, recipient_rd, created_at, expires_at,
                 next_reminder_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13)",
            params![
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                wager,
                DUEL_ISSUANCE_FEE,
                challenger_glicko,
                challenger_rd,
                recipient_glicko,
                recipient_rd,
                now,
                expires_at,
                (now + DAY_SECONDS).min(expires_at),
            ],
        )?;
        let challenge_id = transaction.last_insert_rowid();
        let metadata = format!(
            "{{\"wager\": {wager}, \"issuance_fee\": {DUEL_ISSUANCE_FEE}, \
             \"total_debit\": {upfront_cost}, \"recipient_id\": {recipient_id}}}"
        );
        set_ledger_context(
            &transaction,
            Some(actor_id),
            challenge_id,
            "challenger_escrow",
            &metadata,
        )?;
        let debit = transaction.execute(
            "UPDATE players SET jopacoin_balance = jopacoin_balance - ?1
             WHERE discord_id = ?2 AND guild_id = ?3 AND jopacoin_balance >= ?1",
            params![upfront_cost, challenger_id, guild_id],
        );
        let clear = clear_ledger_context(&transaction);
        let changed = match debit {
            Ok(changed) => {
                clear?;
                changed
            }
            Err(error) => {
                let _ = clear;
                return Err(error.into());
            }
        };
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Your jopacoin balance cannot cover the wager and issuance fee.",
            ));
        }

        let challenge = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Duel challenge insert did not persist."),
        )?;
        transaction.commit()?;
        Ok(challenge)
    }

    pub fn bind_message(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        message_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE duel_challenges SET message_id = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3
               AND status = 'pending' AND message_id IS NULL",
            params![message_id, challenge_id, guild_id],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "The duel message could not be bound.",
            ));
        }
        let challenge = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Bound duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(challenge)
    }

    pub fn mark_delivery_failed_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = transaction
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE challenge_id = ?1 AND guild_id = ?2 AND status = 'pending'
                   AND message_id IS NULL",
                params![challenge_id, guild_id],
                row_to_challenge,
            )
            .optional()?
            .ok_or(DuelRepositoryError::Invalid(
                "Only an unbound pending duel can fail initial delivery.",
            ))?;
        let total_refund = challenge.wager + challenge.issuance_fee;
        let metadata = format!(
            "{{\"wager\": {}, \"issuance_fee\": {}, \"total_refund\": {total_refund}}}",
            challenge.wager, challenge.issuance_fee
        );
        update_player_balance(
            &transaction,
            BalanceUpdate {
                player_id: challenge.challenger_id,
                guild_id,
                delta: total_refund,
                challenge: &challenge,
                actor_id: Some(actor_id),
                reason: "initial_delivery_refund",
                require_nonnegative: false,
                metadata: &metadata,
            },
        )?;
        let changed = transaction.execute(
            "UPDATE duel_challenges
             SET status = 'delivery_failed', next_reminder_at = NULL,
                 resolved_at = ?1, resolution_actor_id = ?2
             WHERE challenge_id = ?3 AND guild_id = ?4 AND status = 'pending'
               AND message_id IS NULL",
            params![now, actor_id, challenge_id, guild_id],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Duel delivery failure state was not recorded.",
            ));
        }
        let failed = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Failed duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(failed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn accept_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        trial: DuelTrial,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = find_challenge(&transaction, challenge_id, guild_id)?
            .filter(|challenge| challenge.status == DuelStatus::Pending)
            .ok_or(DuelRepositoryError::Invalid(
                "Only a pending duel can be accepted.",
            ))?;
        if recipient_id != challenge.recipient_id || actor_id != recipient_id {
            return Err(DuelRepositoryError::Invalid(
                "Only the challenged recipient can accept this duel.",
            ));
        }
        if now >= challenge.expires_at {
            return Err(DuelRepositoryError::Invalid(
                "This duel challenge has expired.",
            ));
        }

        let metadata = format!("{{\"wager\": {}}}", challenge.wager);
        let funded = update_player_balance(
            &transaction,
            BalanceUpdate {
                player_id: challenge.recipient_id,
                guild_id,
                delta: -challenge.wager,
                challenge: &challenge,
                actor_id: Some(actor_id),
                reason: "recipient escrow debit",
                require_nonnegative: true,
                metadata: &metadata,
            },
        );
        if let Err(error) = funded {
            if !matches!(error, DuelRepositoryError::Invalid(message) if message.contains("cannot cover"))
            {
                return Err(error);
            }
            update_player_balance(
                &transaction,
                BalanceUpdate {
                    player_id: challenge.challenger_id,
                    guild_id,
                    delta: challenge.wager,
                    challenge: &challenge,
                    actor_id: Some(actor_id),
                    reason: "unfunded acceptance challenger wager refund",
                    require_nonnegative: false,
                    metadata: &metadata,
                },
            )?;
            let changed = transaction.execute(
                "UPDATE duel_challenges
                 SET status = 'voided', responded_at = ?1, resolved_at = ?1,
                     resolution_actor_id = ?2, next_reminder_at = NULL
                 WHERE challenge_id = ?3 AND guild_id = ?4 AND status = 'pending'
                   AND recipient_id = ?5 AND expires_at > ?1",
                params![now, actor_id, challenge_id, guild_id, recipient_id],
            )?;
            if changed != 1 {
                return Err(DuelRepositoryError::Invalid(
                    "Unfunded duel void state was not recorded.",
                ));
            }
            let voided = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
                DuelRepositoryError::Invalid("Voided duel challenge could not be read."),
            )?;
            transaction.commit()?;
            return Ok(voided);
        }

        let changed = transaction.execute(
            "UPDATE duel_challenges
             SET status = 'accepted', trial_type = ?1, responded_at = ?2,
                 next_reminder_at = ?3
             WHERE challenge_id = ?4 AND guild_id = ?5 AND status = 'pending'
               AND recipient_id = ?6 AND expires_at > ?2",
            params![
                trial.as_str(),
                now,
                now + DAY_SECONDS,
                challenge_id,
                guild_id,
                recipient_id
            ],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Duel acceptance state was not recorded.",
            ));
        }
        let accepted = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Accepted duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(accepted)
    }

    pub fn decline_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = find_challenge(&transaction, challenge_id, guild_id)?
            .filter(|challenge| challenge.status == DuelStatus::Pending)
            .ok_or(DuelRepositoryError::Invalid(
                "Only a pending duel can be declined.",
            ))?;
        if recipient_id != challenge.recipient_id || actor_id != recipient_id {
            return Err(DuelRepositoryError::Invalid(
                "Only the challenged recipient can decline this duel.",
            ));
        }
        if now >= challenge.expires_at {
            return Err(DuelRepositoryError::Invalid(
                "This duel challenge has expired.",
            ));
        }

        let penalty = challenge.decline_penalty();
        let metadata = format!("{{\"wager\": {}}}", challenge.wager);
        for (player_id, delta, reason) in [
            (
                challenge.challenger_id,
                challenge.wager,
                "challenger escrow refund",
            ),
            (
                challenge.challenger_id,
                penalty,
                "decline penalty challenger credit",
            ),
            (
                challenge.recipient_id,
                -penalty,
                "decline penalty recipient debit",
            ),
        ] {
            update_player_balance(
                &transaction,
                BalanceUpdate {
                    player_id,
                    guild_id,
                    delta,
                    challenge: &challenge,
                    actor_id: Some(actor_id),
                    reason,
                    require_nonnegative: false,
                    metadata: &metadata,
                },
            )?;
        }
        let changed = transaction.execute(
            "UPDATE duel_challenges
             SET status = 'declined', next_reminder_at = NULL,
                 responded_at = ?1, resolved_at = ?1, resolution_actor_id = ?2
             WHERE challenge_id = ?3 AND guild_id = ?4 AND status = 'pending'
               AND recipient_id = ?5 AND expires_at > ?1",
            params![now, actor_id, challenge_id, guild_id, recipient_id],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Duel decline state was not recorded.",
            ));
        }
        let declined = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Declined duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(declined)
    }

    pub fn expire_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = transaction
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE challenge_id = ?1 AND guild_id = ?2
                   AND status = 'pending' AND message_id IS NOT NULL",
                params![challenge_id, guild_id],
                row_to_challenge,
            )
            .optional()?
            .ok_or(DuelRepositoryError::Invalid(
                "Only a pending duel can expire.",
            ))?;
        if now < challenge.expires_at {
            return Err(DuelRepositoryError::Invalid(
                "This duel challenge has not expired yet.",
            ));
        }

        let penalty = challenge.decline_penalty();
        let metadata = format!("{{\"wager\": {}}}", challenge.wager);
        for (player_id, delta, reason) in [
            (
                challenge.challenger_id,
                challenge.wager,
                "expired challenger escrow refund",
            ),
            (
                challenge.challenger_id,
                penalty,
                "expired penalty challenger credit",
            ),
            (
                challenge.recipient_id,
                -penalty,
                "expired penalty recipient debit",
            ),
        ] {
            update_player_balance(
                &transaction,
                BalanceUpdate {
                    player_id,
                    guild_id,
                    delta,
                    challenge: &challenge,
                    actor_id: None,
                    reason,
                    require_nonnegative: false,
                    metadata: &metadata,
                },
            )?;
        }
        let changed = transaction.execute(
            "UPDATE duel_challenges
             SET status = 'expired', next_reminder_at = ?1, resolved_at = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3 AND status = 'pending'
               AND message_id IS NOT NULL AND expires_at <= ?1",
            params![now, challenge_id, guild_id],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Duel expiry state was not recorded.",
            ));
        }
        let expired = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Expired duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(expired)
    }

    pub fn resolve_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        winner_id: Option<i64>,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = find_challenge(&transaction, challenge_id, guild_id)?
            .filter(|challenge| challenge.status == DuelStatus::Accepted)
            .ok_or(DuelRepositoryError::Invalid(
                "Only an accepted duel can be resolved.",
            ))?;
        if winner_id.is_some_and(|winner| {
            winner != challenge.challenger_id && winner != challenge.recipient_id
        }) {
            return Err(DuelRepositoryError::Invalid(
                "The duel winner must be one of its participants.",
            ));
        }

        let metadata = format!("{{\"wager\": {}}}", challenge.wager);
        let status = if let Some(winner_id) = winner_id {
            update_player_balance(
                &transaction,
                BalanceUpdate {
                    player_id: winner_id,
                    guild_id,
                    delta: 2 * challenge.wager,
                    challenge: &challenge,
                    actor_id: Some(actor_id),
                    reason: "winner double-pot credit",
                    require_nonnegative: false,
                    metadata: &metadata,
                },
            )?;
            DuelStatus::Resolved
        } else {
            for (player_id, reason) in [
                (challenge.challenger_id, "void challenger stake refund"),
                (challenge.recipient_id, "void recipient stake refund"),
            ] {
                update_player_balance(
                    &transaction,
                    BalanceUpdate {
                        player_id,
                        guild_id,
                        delta: challenge.wager,
                        challenge: &challenge,
                        actor_id: Some(actor_id),
                        reason,
                        require_nonnegative: false,
                        metadata: &metadata,
                    },
                )?;
            }
            DuelStatus::Voided
        };
        let changed = transaction.execute(
            "UPDATE duel_challenges
             SET status = ?1, winner_id = ?2, resolved_at = ?3,
                 resolution_actor_id = ?4, next_reminder_at = NULL
             WHERE challenge_id = ?5 AND guild_id = ?6 AND status = 'accepted'",
            params![
                status.as_str(),
                winner_id,
                now,
                actor_id,
                challenge_id,
                guild_id
            ],
        )?;
        if changed != 1 {
            return Err(DuelRepositoryError::Invalid(
                "Duel resolution state was not recorded.",
            ));
        }
        let resolved = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Resolved duel challenge could not be read."),
        )?;
        transaction.commit()?;
        Ok(resolved)
    }

    pub fn get_due_challenge_ids(&self, now: i64) -> Result<Vec<(i64, i64)>, DuelRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT challenge_id, guild_id
             FROM duel_challenges
             WHERE (
                   status = 'pending' AND message_id IS NOT NULL
                   AND (expires_at <= ?1 OR (next_reminder_at IS NOT NULL AND next_reminder_at <= ?1))
               )
               OR (status = 'accepted' AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?1)
               OR (status = 'expired' AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?1)
             ORDER BY CASE WHEN status = 'pending' AND expires_at <= ?1 THEN 0 ELSE 1 END,
                      CASE WHEN status = 'pending' AND expires_at <= ?1
                           THEN expires_at ELSE next_reminder_at END,
                      challenge_id",
        )?;
        Ok(statement
            .query_map(params![now], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn claim_reminder_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Option<DuelDueResult>, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = transaction
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE challenge_id = ?1 AND guild_id = ?2
                   AND status = 'pending' AND message_id IS NOT NULL",
                params![challenge_id, guild_id],
                row_to_challenge,
            )
            .optional()?;
        let Some(challenge) = challenge else {
            transaction.commit()?;
            return Ok(None);
        };
        let Some(claimed_reminder_at) = challenge.next_reminder_at else {
            transaction.commit()?;
            return Ok(None);
        };
        if challenge.expires_at <= now || claimed_reminder_at > now {
            transaction.commit()?;
            return Ok(None);
        }

        let mut daily_boundary =
            challenge.created_at + ((now - challenge.created_at) / DAY_SECONDS + 1) * DAY_SECONDS;
        if daily_boundary - now < 600 {
            daily_boundary += DAY_SECONDS;
        }
        let next_reminder_at = daily_boundary.min(challenge.expires_at);
        let changed = transaction.execute(
            "UPDATE duel_challenges SET next_reminder_at = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3 AND status = 'pending'
               AND message_id IS NOT NULL AND expires_at > ?4
               AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?4",
            params![next_reminder_at, challenge_id, guild_id, now],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let claimed = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Claimed duel reminder could not be read."),
        )?;
        let remaining_seconds = claimed.expires_at - now;
        let result = DuelDueResult {
            kind: DuelDueKind::Reminder,
            challenge: claimed,
            remaining_seconds,
            ping_recipient: remaining_seconds <= 48 * 3600,
            claimed_reminder_at: Some(claimed_reminder_at),
            is_redelivery: false,
        };
        transaction.commit()?;
        Ok(Some(result))
    }

    pub fn claim_unresolved_reminder_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Option<DuelDueResult>, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = transaction
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE challenge_id = ?1 AND guild_id = ?2 AND status = 'accepted'",
                params![challenge_id, guild_id],
                row_to_challenge,
            )
            .optional()?;
        let Some(challenge) = challenge else {
            transaction.commit()?;
            return Ok(None);
        };
        let Some(claimed_reminder_at) = challenge.next_reminder_at else {
            transaction.commit()?;
            return Ok(None);
        };
        if claimed_reminder_at > now {
            transaction.commit()?;
            return Ok(None);
        }
        let anchor = challenge.responded_at.unwrap_or(claimed_reminder_at);
        let mut next_reminder_at = anchor + ((now - anchor) / DAY_SECONDS + 1) * DAY_SECONDS;
        if next_reminder_at - now < 600 {
            next_reminder_at += DAY_SECONDS;
        }
        let changed = transaction.execute(
            "UPDATE duel_challenges SET next_reminder_at = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3 AND status = 'accepted'
               AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?4",
            params![next_reminder_at, challenge_id, guild_id, now],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let claimed = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Claimed unresolved duel reminder could not be read."),
        )?;
        let result = DuelDueResult {
            kind: DuelDueKind::Unresolved,
            challenge: claimed,
            remaining_seconds: 0,
            ping_recipient: false,
            claimed_reminder_at: Some(claimed_reminder_at),
            is_redelivery: false,
        };
        transaction.commit()?;
        Ok(Some(result))
    }

    pub fn claim_expired_announcement_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        now: i64,
        retry_at: i64,
    ) -> Result<Option<DuelDueResult>, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let challenge = transaction
            .query_row(
                "SELECT * FROM duel_challenges
                 WHERE challenge_id = ?1 AND guild_id = ?2 AND status = 'expired'",
                params![challenge_id, guild_id],
                row_to_challenge,
            )
            .optional()?;
        let Some(challenge) = challenge else {
            transaction.commit()?;
            return Ok(None);
        };
        let Some(claimed_reminder_at) = challenge.next_reminder_at else {
            transaction.commit()?;
            return Ok(None);
        };
        if claimed_reminder_at > now {
            transaction.commit()?;
            return Ok(None);
        }
        let changed = transaction.execute(
            "UPDATE duel_challenges SET next_reminder_at = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3 AND status = 'expired'
               AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?4",
            params![retry_at, challenge_id, guild_id, now],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let claimed = find_challenge(&transaction, challenge_id, guild_id)?.ok_or(
            DuelRepositoryError::Invalid("Claimed expired duel announcement could not be read."),
        )?;
        let result = DuelDueResult {
            kind: DuelDueKind::Expired,
            challenge: claimed,
            remaining_seconds: 0,
            ping_recipient: false,
            claimed_reminder_at: Some(claimed_reminder_at),
            is_redelivery: true,
        };
        transaction.commit()?;
        Ok(Some(result))
    }

    pub fn clear_expired_announcement_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        expected_next_reminder_at: i64,
    ) -> Result<bool, DuelRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE duel_challenges SET next_reminder_at = NULL
             WHERE challenge_id = ?1 AND guild_id = ?2 AND status = 'expired'
               AND next_reminder_at = ?3",
            params![challenge_id, guild_id, expected_next_reminder_at],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn rearm_reminder_atomic(
        &self,
        challenge_id: i64,
        guild_id: Option<i64>,
        expected_status: DuelStatus,
        expected_next_reminder_at: i64,
        retry_at: i64,
    ) -> Result<bool, DuelRepositoryError> {
        if !matches!(expected_status, DuelStatus::Pending | DuelStatus::Accepted) {
            return Ok(false);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE duel_challenges SET next_reminder_at = ?1
             WHERE challenge_id = ?2 AND guild_id = ?3 AND status = ?4
               AND next_reminder_at = ?5",
            params![
                retry_at,
                challenge_id,
                guild_id,
                expected_status.as_str(),
                expected_next_reminder_at
            ],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }
}

#[derive(Clone, Copy, Debug)]
struct PlayerSnapshot {
    rating: Option<f64>,
    rd: Option<f64>,
    balance: i64,
}

impl PlayerSnapshot {
    fn rated_values(self) -> Option<(f64, f64)> {
        Some((self.rating?, self.rd?))
    }
}

fn find_player_snapshot(
    connection: &Connection,
    player_id: i64,
    guild_id: i64,
) -> Result<Option<PlayerSnapshot>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT glicko_rating, glicko_rd, COALESCE(jopacoin_balance, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![player_id, guild_id],
            |row| {
                Ok(PlayerSnapshot {
                    rating: row.get(0)?,
                    rd: row.get::<_, Option<f64>>(1)?.map(cap_glicko_rd),
                    balance: python_int(row.get_ref(2)?, 2)?,
                })
            },
        )
        .optional()
}

/// Match Python's `int(sqlite_value)` validation before SQLite itself applies
/// its dynamic numeric coercion in the guarded balance update.
fn python_int(value: ValueRef<'_>, column: usize) -> Result<i64, rusqlite::Error> {
    let converted = match value {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value)
            if value.is_finite()
                && value.trunc() >= i64::MIN as f64
                && value.trunc() <= i64::MAX as f64 =>
        {
            Some(value.trunc() as i64)
        }
        ValueRef::Text(value) => std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.trim().parse::<i64>().ok()),
        ValueRef::Null | ValueRef::Real(_) | ValueRef::Blob(_) => None,
    };
    converted.ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            value.data_type(),
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SQLite value cannot be converted with Python int semantics",
            )),
        )
    })
}

fn find_challenge(
    connection: &Connection,
    challenge_id: i64,
    guild_id: i64,
) -> Result<Option<DuelChallenge>, DuelRepositoryError> {
    Ok(connection
        .query_row(
            "SELECT * FROM duel_challenges WHERE challenge_id = ?1 AND guild_id = ?2",
            params![challenge_id, guild_id],
            row_to_challenge,
        )
        .optional()?)
}

fn row_to_challenge(row: &rusqlite::Row<'_>) -> Result<DuelChallenge, rusqlite::Error> {
    let status_raw: String = row.get(8)?;
    let status = DuelStatus::from_database(&status_raw).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(8, "status".to_owned(), rusqlite::types::Type::Text)
    })?;
    let trial_raw: Option<String> = row.get(9)?;
    let trial_type = trial_raw
        .as_deref()
        .map(|value| {
            DuelTrial::from_database(value).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    9,
                    "trial_type".to_owned(),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()?;
    Ok(DuelChallenge {
        challenge_id: row.get(0)?,
        guild_id: row.get(1)?,
        channel_id: row.get(2)?,
        message_id: row.get(3)?,
        challenger_id: row.get(4)?,
        recipient_id: row.get(5)?,
        wager: row.get(6)?,
        issuance_fee: row.get(7)?,
        status,
        trial_type,
        challenger_glicko: row.get(10)?,
        challenger_rd: row.get(11)?,
        recipient_glicko: row.get(12)?,
        recipient_rd: row.get(13)?,
        created_at: row.get(14)?,
        expires_at: row.get(15)?,
        next_reminder_at: row.get(16)?,
        responded_at: row.get(17)?,
        resolved_at: row.get(18)?,
        winner_id: row.get(19)?,
        resolution_actor_id: row.get(20)?,
    })
}

struct BalanceUpdate<'a> {
    player_id: i64,
    guild_id: i64,
    delta: i64,
    challenge: &'a DuelChallenge,
    actor_id: Option<i64>,
    reason: &'a str,
    require_nonnegative: bool,
    metadata: &'a str,
}

fn update_player_balance(
    connection: &Connection,
    update: BalanceUpdate<'_>,
) -> Result<(), DuelRepositoryError> {
    set_ledger_context(
        connection,
        update.actor_id,
        update.challenge.challenge_id,
        update.reason,
        update.metadata,
    )?;
    let result = if update.require_nonnegative {
        connection.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) + ?1 >= 0",
            params![update.delta, update.player_id, update.guild_id],
        )
    } else {
        connection.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![update.delta, update.player_id, update.guild_id],
        )
    };
    let clear = clear_ledger_context(connection);
    let changed = match result {
        Ok(changed) => {
            clear?;
            changed
        }
        Err(error) => {
            let _ = clear;
            return Err(error.into());
        }
    };
    if changed != 1 {
        if update.require_nonnegative {
            return Err(DuelRepositoryError::Invalid(
                "Your jopacoin balance cannot cover the duel wager.",
            ));
        }
        return Err(DuelRepositoryError::Invalid(
            "A duel participant balance could not be updated.",
        ));
    }
    Ok(())
}

fn set_ledger_context(
    connection: &Connection,
    actor_id: Option<i64>,
    challenge_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'duel_challenge', ?1, 'duel_challenge', ?2, ?3, ?4)",
        params![actor_id, challenge_id.to_string(), reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;
    use tempfile::NamedTempFile;

    use crate::test_support::FastTestDatabase;

    const GUILD_ID: i64 = 9_001;
    const NOW: i64 = 1_000_000;
    const DAY: i64 = DAY_SECONDS;

    static FIXTURE_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();

    enum DuelTestDatabase {
        Fast(FastTestDatabase),
        Durable(NamedTempFile),
    }

    impl DuelTestDatabase {
        fn path(&self) -> &Path {
            match self {
                Self::Fast(database) => database.path(),
                Self::Durable(database) => database.path(),
            }
        }
    }

    const TEST_SCHEMA: &str = "
        PRAGMA journal_mode=WAL;
        CREATE TABLE players (
            discord_id INTEGER NOT NULL,
            guild_id INTEGER NOT NULL DEFAULT 0,
            discord_username TEXT NOT NULL,
            glicko_rating REAL,
            glicko_rd REAL,
            jopacoin_balance INTEGER DEFAULT 0,
            PRIMARY KEY (discord_id, guild_id)
        );
        CREATE TABLE duel_challenges (
            challenge_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL,
            channel_id INTEGER NOT NULL,
            message_id INTEGER,
            challenger_id INTEGER NOT NULL,
            recipient_id INTEGER NOT NULL,
            wager INTEGER NOT NULL CHECK (wager BETWEEN 500 AND 1000),
            issuance_fee INTEGER NOT NULL DEFAULT 50 CHECK (issuance_fee = 50),
            status TEXT NOT NULL CHECK (status IN (
                'pending', 'accepted', 'declined', 'expired',
                'resolved', 'voided', 'delivery_failed'
            )),
            trial_type TEXT CHECK (
                trial_type IS NULL OR trial_type IN ('trial_by_combat', 'trial_of_five')
            ),
            challenger_glicko REAL NOT NULL,
            challenger_rd REAL NOT NULL,
            recipient_glicko REAL NOT NULL,
            recipient_rd REAL NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            next_reminder_at INTEGER,
            responded_at INTEGER,
            resolved_at INTEGER,
            winner_id INTEGER,
            resolution_actor_id INTEGER,
            CHECK (challenger_id != recipient_id)
        );
        CREATE INDEX idx_duel_guild_status
            ON duel_challenges(guild_id, status, created_at DESC);
        CREATE INDEX idx_duel_challenger_history
            ON duel_challenges(guild_id, challenger_id, created_at DESC);
        CREATE INDEX idx_duel_recipient_history
            ON duel_challenges(guild_id, recipient_id, created_at DESC);
        CREATE INDEX idx_duel_due_expiry
            ON duel_challenges(status, expires_at);
        CREATE INDEX idx_duel_due_reminder
            ON duel_challenges(status, next_reminder_at);
        CREATE TABLE economy_ledger_entries (
            ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id INTEGER NOT NULL,
            account_type TEXT NOT NULL CHECK(account_type IN ('player', 'nonprofit')),
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
            id INTEGER PRIMARY KEY CHECK(id = 1),
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
                COALESCE(OLD.jopacoin_balance, 0), COALESCE(NEW.jopacoin_balance, 0),
                COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                (SELECT reason FROM economy_ledger_context WHERE id = 1),
                (SELECT metadata FROM economy_ledger_context WHERE id = 1)
            );
        END;
    ";

    fn database_template() -> &'static NamedTempFile {
        FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
            let file = NamedTempFile::new().expect("create duel test database template");
            let connection =
                Connection::open(file.path()).expect("open duel test database template");
            connection
                .execute_batch(TEST_SCHEMA)
                .expect("create Python-compatible duel test schema template");
            drop(connection);
            file
        })
    }

    fn database() -> DuelTestDatabase {
        DuelTestDatabase::Fast(FastTestDatabase::from_template(database_template().path()))
    }

    fn durable_database() -> DuelTestDatabase {
        let file = NamedTempFile::new().expect("create duel test database");
        std::fs::copy(database_template().path(), file.path())
            .expect("copy duel test database template");
        DuelTestDatabase::Durable(file)
    }

    fn seed_player(
        file: &DuelTestDatabase,
        discord_id: i64,
        rating: Option<f64>,
        balance: i64,
        guild_id: i64,
    ) {
        let connection = Connection::open(file.path()).expect("open duel test database");
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     glicko_rating, glicko_rd, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    discord_id,
                    guild_id,
                    format!("Player {discord_id}"),
                    rating,
                    rating.map(|_| 80.0),
                    balance
                ],
            )
            .expect("seed duel player");
    }

    fn set_balance(file: &DuelTestDatabase, discord_id: i64, guild_id: i64, balance: i64) {
        let connection = Connection::open(file.path()).expect("open duel test database");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance = ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![balance, discord_id, guild_id],
            )
            .expect("set player balance");
    }

    fn balance(file: &DuelTestDatabase, discord_id: i64, guild_id: i64) -> i64 {
        Connection::open(file.path())
            .expect("open duel test database")
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn create_challenge(
        repository: &DuelChallengeRepository,
        challenger_id: i64,
        recipient_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> DuelChallenge {
        repository
            .create_challenge_atomic(
                guild_id,
                77,
                challenger_id,
                recipient_id,
                500_i64,
                now,
                15 * DAY,
                7 * DAY,
                7 * DAY,
                challenger_id,
            )
            .expect("create duel challenge")
    }

    fn error_contains<T>(result: Result<T, DuelRepositoryError>, expected: &str) {
        let error = result.err().expect("operation must fail");
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }

    struct BoundFixture {
        file: DuelTestDatabase,
        repository: DuelChallengeRepository,
        challenge: DuelChallenge,
    }

    fn bound_fixture(wager: i64, recipient_balance: i64) -> BoundFixture {
        bound_fixture_with_database(database(), wager, recipient_balance)
    }

    fn durable_bound_fixture(wager: i64, recipient_balance: i64) -> BoundFixture {
        bound_fixture_with_database(durable_database(), wager, recipient_balance)
    }

    fn bound_fixture_with_database(
        file: DuelTestDatabase,
        wager: i64,
        recipient_balance: i64,
    ) -> BoundFixture {
        seed_player(&file, 1, Some(1400.0), wager + 50, GUILD_ID);
        seed_player(
            &file,
            2,
            Some(1500.0),
            wager.max(recipient_balance),
            GUILD_ID,
        );
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = repository
            .create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                2,
                wager,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            )
            .expect("create fixture challenge");
        let challenge = repository
            .bind_message(
                challenge.challenge_id,
                Some(GUILD_ID),
                10_000 + challenge.challenge_id,
            )
            .expect("bind fixture challenge");
        set_balance(&file, 1, GUILD_ID, 500 - wager);
        set_balance(&file, 2, GUILD_ID, recipient_balance);
        Connection::open(file.path())
            .expect("open duel test database")
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear setup ledger");
        BoundFixture {
            file,
            repository,
            challenge,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct LedgerRow {
        account_id: i64,
        delta: i64,
        source: String,
        actor_id: Option<i64>,
        related_type: Option<String>,
        related_id: Option<String>,
        reason: Option<String>,
        metadata: Option<String>,
    }

    fn ledger_rows(file: &DuelTestDatabase, challenge_id: i64) -> Vec<LedgerRow> {
        let connection = Connection::open(file.path()).expect("open duel test database");
        let mut statement = connection
            .prepare(
                "SELECT account_id, delta, source, actor_id, related_type,
                        related_id, reason, metadata
                 FROM economy_ledger_entries
                 WHERE related_type = 'duel_challenge' AND related_id = ?1
                 ORDER BY ledger_id",
            )
            .expect("prepare duel ledger query");
        statement
            .query_map(params![challenge_id.to_string()], |row| {
                Ok(LedgerRow {
                    account_id: row.get(0)?,
                    delta: row.get(1)?,
                    source: row.get(2)?,
                    actor_id: row.get(3)?,
                    related_type: row.get(4)?,
                    related_id: row.get(5)?,
                    reason: row.get(6)?,
                    metadata: row.get(7)?,
                })
            })
            .expect("query duel ledger")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect duel ledger")
    }

    #[test]
    fn test_duel_model_reads_sqlite_row() {
        let connection = Connection::open_in_memory().expect("open in-memory SQLite");
        let challenge = connection
            .query_row(
                "SELECT 7, 42, 9, NULL, 1, 2, 501, 50,
                        'pending', NULL, 1400.0, 80.0, 1500.0, 70.0,
                        100, 200, 120, NULL, NULL, NULL, NULL",
                [],
                row_to_challenge,
            )
            .expect("decode duel row");
        assert_eq!(challenge.status, DuelStatus::Pending);
        assert_eq!(challenge.trial_type, None);
        assert_eq!(challenge.issuance_fee, 50);
        assert_eq!(challenge.decline_penalty(), 251);
    }

    #[test]
    fn test_duel_schema_enforces_wager_and_status() {
        let file = database();
        let connection = Connection::open(file.path()).expect("open duel test database");
        let columns = connection
            .prepare("PRAGMA table_info(duel_challenges)")
            .expect("prepare table info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .collect::<Result<BTreeSet<_>, _>>()
            .expect("collect columns");
        for required in [
            "challenge_id",
            "guild_id",
            "message_id",
            "issuance_fee",
            "next_reminder_at",
        ] {
            assert!(columns.contains(required));
        }
        let fee = connection
            .query_row(
                "SELECT \"notnull\", dflt_value
                 FROM pragma_table_info('duel_challenges') WHERE name = 'issuance_fee'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("read fee column contract");
        assert_eq!(fee, (1, "50".to_owned()));
        let indexes = connection
            .prepare("PRAGMA index_list(duel_challenges)")
            .expect("prepare index list")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query index list")
            .collect::<Result<BTreeSet<_>, _>>()
            .expect("collect indexes");
        for required in [
            "idx_duel_guild_status",
            "idx_duel_challenger_history",
            "idx_duel_recipient_history",
            "idx_duel_due_expiry",
            "idx_duel_due_reminder",
        ] {
            assert!(indexes.contains(required));
        }

        let invalid_wager = connection.execute(
            "INSERT INTO duel_challenges (
                 guild_id, channel_id, challenger_id, recipient_id, wager, status,
                 challenger_glicko, challenger_rd, recipient_glicko, recipient_rd,
                 created_at, expires_at
             ) VALUES (42, 9, 1, 2, 499, 'pending', 1400, 80, 1500, 70, 100, 200)",
            [],
        );
        assert!(invalid_wager.is_err());
        let invalid_fee = connection.execute(
            "INSERT INTO duel_challenges (
                 guild_id, channel_id, challenger_id, recipient_id, wager,
                 issuance_fee, status, challenger_glicko, challenger_rd,
                 recipient_glicko, recipient_rd, created_at, expires_at
             ) VALUES (42, 9, 1, 2, 500, 49, 'pending', 1400, 80, 1500, 70, 100, 200)",
            [],
        );
        assert!(invalid_fee.is_err());
    }

    #[test]
    fn test_create_requires_wager_plus_issuance_fee() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 500, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                2,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            ),
            "balance",
        );
        assert_eq!(balance(&file, 1, GUILD_ID), 500);
    }

    #[test]
    fn test_create_challenge_escrows_fee_and_snapshots() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 700, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 501, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = repository
            .create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                2,
                501_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            )
            .expect("create challenge");
        assert_eq!(challenge.challenger_glicko, 1400.0);
        assert_eq!(challenge.challenger_rd, 80.0);
        assert_eq!(challenge.recipient_glicko, 1500.0);
        assert_eq!(challenge.recipient_rd, 80.0);
        assert_eq!(challenge.issuance_fee, 50);
        assert_eq!(challenge.expires_at, NOW + 7 * DAY);
        assert_eq!(challenge.next_reminder_at, Some(NOW + DAY));
        assert_eq!(balance(&file, 1, GUILD_ID), 149);
        assert_eq!(balance(&file, 2, GUILD_ID), 501);
        let rows = ledger_rows(&file, challenge.challenge_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                &rows[0].account_id,
                &rows[0].delta,
                rows[0].reason.as_deref()
            ),
            (&1, &-551, Some("challenger_escrow"))
        );
        assert_eq!(
            rows[0].metadata.as_deref(),
            Some(
                "{\"wager\": 501, \"issuance_fee\": 50, \"total_debit\": 551, \
                 \"recipient_id\": 2}"
            )
        );
    }

    #[test]
    fn test_create_rejects_unfunded_recipient_without_side_effects_or_cooldown() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 1000, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 499, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let error = repository
            .create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                2,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            )
            .expect_err("unfunded recipient must fail");
        assert!(matches!(
            error,
            DuelRepositoryError::RecipientFunding {
                recipient_id: 2,
                wager: 500
            }
        ));
        assert_eq!(balance(&file, 1, GUILD_ID), 1000);
        assert_eq!(balance(&file, 2, GUILD_ID), 499);
        assert!(
            repository
                .list_pending_all()
                .expect("list pending")
                .is_empty()
        );
        assert!(ledger_rows(&file, 1).is_empty());

        set_balance(&file, 2, GUILD_ID, 500);
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        assert_eq!(challenge.status, DuelStatus::Pending);
        assert_eq!(balance(&file, 1, GUILD_ID), 450);
        assert_eq!(balance(&file, 2, GUILD_ID), 500);
    }

    fn invalid_challenge<W: Into<DuelWager>>(
        recipient_rating: f64,
        wager: W,
        challenger_balance: i64,
    ) -> (DuelTestDatabase, Result<DuelChallenge, DuelRepositoryError>) {
        let file = database();
        seed_player(&file, 1, Some(1400.0), challenger_balance, GUILD_ID);
        seed_player(&file, 2, Some(recipient_rating), 0, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let result = repository.create_challenge_atomic(
            Some(GUILD_ID),
            77,
            1,
            2,
            wager,
            NOW,
            30 * DAY,
            7 * DAY,
            7 * DAY,
            1,
        );
        (file, result)
    }

    #[test]
    fn test_create_rejects_invalid_challenges_1300_0_500_500_punching_down() {
        let (file, result) = invalid_challenge(1300.0, 500_i64, 500);
        error_contains(result, "punching down");
        assert_eq!(balance(&file, 1, GUILD_ID), 500);
    }

    #[test]
    fn test_create_rejects_invalid_challenges_1500_0_499_500_500() {
        let (file, result) = invalid_challenge(1500.0, 499_i64, 500);
        error_contains(result, "500");
        assert_eq!(balance(&file, 1, GUILD_ID), 500);
    }

    #[test]
    fn test_create_rejects_invalid_challenges_1500_0_1001_1001_1000() {
        let (file, result) = invalid_challenge(1500.0, 1001_i64, 1001);
        error_contains(result, "1000");
        assert_eq!(balance(&file, 1, GUILD_ID), 1001);
    }

    #[test]
    fn test_create_rejects_invalid_challenges_1500_0_500_5_1000_whole() {
        let (file, result) = invalid_challenge(1500.0, 500.5_f64, 1000);
        error_contains(result, "whole");
        assert_eq!(balance(&file, 1, GUILD_ID), 1000);
    }

    #[test]
    fn test_create_rejects_invalid_challenges_1500_0_500_499_balance() {
        let (file, result) = invalid_challenge(1500.0, 500_i64, 499);
        error_contains(result, "balance");
        assert_eq!(balance(&file, 1, GUILD_ID), 499);
    }

    #[test]
    fn test_create_rejects_self_challenge() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                1,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            ),
            "yourself",
        );
    }

    fn unregistered_or_unrated(
        missing_id: Option<i64>,
        unrated_id: Option<i64>,
    ) -> Result<DuelChallenge, DuelRepositoryError> {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let connection = Connection::open(file.path()).expect("open duel test database");
        if let Some(missing_id) = missing_id {
            connection
                .execute(
                    "DELETE FROM players WHERE guild_id = ?1 AND discord_id = ?2",
                    params![GUILD_ID, missing_id],
                )
                .expect("delete player");
        } else if let Some(unrated_id) = unrated_id {
            connection
                .execute(
                    "UPDATE players SET glicko_rating = NULL, glicko_rd = NULL
                     WHERE guild_id = ?1 AND discord_id = ?2",
                    params![GUILD_ID, unrated_id],
                )
                .expect("clear rating");
        }
        drop(connection);
        DuelChallengeRepository::new(file.path()).create_challenge_atomic(
            Some(GUILD_ID),
            77,
            1,
            2,
            500_i64,
            NOW,
            30 * DAY,
            7 * DAY,
            7 * DAY,
            1,
        )
    }

    #[test]
    fn test_create_rejects_unregistered_or_unrated_players_1_none_registered() {
        error_contains(unregistered_or_unrated(Some(1), None), "registered");
    }

    #[test]
    fn test_create_rejects_unregistered_or_unrated_players_2_none_registered() {
        error_contains(unregistered_or_unrated(Some(2), None), "registered");
    }

    #[test]
    fn test_create_rejects_unregistered_or_unrated_players_none_1_rated() {
        error_contains(unregistered_or_unrated(None, Some(1)), "rated");
    }

    #[test]
    fn test_create_rejects_unregistered_or_unrated_players_none_2_rated() {
        error_contains(unregistered_or_unrated(None, Some(2)), "rated");
    }

    #[test]
    fn test_create_allows_equal_ratings() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1400.0), 500, GUILD_ID);
        let challenge = create_challenge(
            &DuelChallengeRepository::new(file.path()),
            1,
            2,
            Some(GUILD_ID),
            NOW,
        );
        assert_eq!(challenge.status, DuelStatus::Pending);
    }

    #[test]
    fn test_challenger_cooldown_allows_exact_boundary() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 1500, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let first = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        Connection::open(file.path())
            .expect("open duel test database")
            .execute(
                "UPDATE duel_challenges SET status = 'declined', resolved_at = ?1
                 WHERE challenge_id = ?2",
                params![NOW + 1, first.challenge_id],
            )
            .expect("resolve first challenge");
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                1,
                2,
                500_i64,
                NOW + 15 * DAY - 1,
                15 * DAY,
                7 * DAY,
                7 * DAY,
                1,
            ),
            "Your duel challenge cooldown has not elapsed.",
        );
        let second = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW + 15 * DAY);
        assert_eq!(second.status, DuelStatus::Pending);
    }

    #[test]
    fn test_recipient_cooldown_allows_exact_boundary() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1600.0), 500, GUILD_ID);
        seed_player(&file, 3, Some(1500.0), 1000, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let first = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        Connection::open(file.path())
            .expect("open duel test database")
            .execute(
                "UPDATE duel_challenges SET status = 'declined', resolved_at = ?1
                 WHERE challenge_id = ?2",
                params![NOW + 1, first.challenge_id],
            )
            .expect("resolve first challenge");
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                3,
                2,
                500_i64,
                NOW + 7 * DAY - 1,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                3,
            ),
            "recently challenged",
        );
        let second = create_challenge(&repository, 3, 2, Some(GUILD_ID), NOW + 7 * DAY);
        assert_eq!(second.status, DuelStatus::Pending);
    }

    #[test]
    fn test_unresolved_participation_blocks_either_role() {
        let file = database();
        for (id, rating, balance) in [
            (1, 1400.0, 550),
            (2, 1500.0, 500),
            (3, 1600.0, 0),
            (4, 1300.0, 500),
        ] {
            seed_player(&file, id, Some(rating), balance, GUILD_ID);
        }
        let repository = DuelChallengeRepository::new(file.path());
        create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                2,
                3,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                2,
            ),
            "already involved",
        );
        error_contains(
            repository.create_challenge_atomic(
                Some(GUILD_ID),
                77,
                4,
                1,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                7 * DAY,
                4,
            ),
            "already involved",
        );
    }

    #[test]
    fn test_duel_creation_and_reads_are_guild_isolated() {
        let file = database();
        let other_guild = GUILD_ID + 1;
        for guild_id in [GUILD_ID, other_guild] {
            seed_player(&file, 1, Some(1400.0), 550, guild_id);
            seed_player(&file, 2, Some(1500.0), 500, guild_id);
        }
        let repository = DuelChallengeRepository::new(file.path());
        let first = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let second = create_challenge(&repository, 1, 2, Some(other_guild), NOW);
        repository
            .bind_message(first.challenge_id, Some(GUILD_ID), 1001)
            .expect("bind first");
        repository
            .bind_message(second.challenge_id, Some(other_guild), 1002)
            .expect("bind second");
        assert!(
            repository
                .get_challenge(first.challenge_id, Some(other_guild))
                .expect("read cross guild")
                .is_none()
        );
        assert_eq!(
            repository
                .list_outstanding(Some(GUILD_ID))
                .expect("list guild")
                .iter()
                .map(|row| row.challenge_id)
                .collect::<Vec<_>>(),
            vec![first.challenge_id]
        );
        assert_eq!(
            repository
                .list_outstanding(Some(other_guild))
                .expect("list other guild")
                .iter()
                .map(|row| row.challenge_id)
                .collect::<Vec<_>>(),
            vec![second.challenge_id]
        );
    }

    #[test]
    fn test_message_binding_is_guarded() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let bound = repository
            .bind_message(challenge.challenge_id, Some(GUILD_ID), 1234)
            .expect("bind message");
        assert_eq!(bound.message_id, Some(1234));
        error_contains(
            repository.bind_message(challenge.challenge_id, Some(GUILD_ID), 5678),
            "message",
        );
        error_contains(
            repository.bind_message(challenge.challenge_id, Some(GUILD_ID + 1), 5678),
            "message",
        );
    }

    #[test]
    fn test_unbound_pending_is_inert_but_available_for_startup_reconciliation() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        assert!(
            repository
                .get_pending_for_recipient(2, Some(GUILD_ID))
                .expect("get pending")
                .is_none()
        );
        assert!(
            repository
                .list_outstanding(Some(GUILD_ID))
                .expect("list outstanding")
                .is_empty()
        );
        assert_eq!(
            repository
                .list_pending_all()
                .expect("list pending")
                .iter()
                .map(|row| row.challenge_id)
                .collect::<Vec<_>>(),
            vec![challenge.challenge_id]
        );
        assert!(
            repository
                .get_due_challenge_ids(challenge.expires_at)
                .expect("scan due")
                .is_empty()
        );
        assert!(
            repository
                .claim_reminder_atomic(challenge.challenge_id, Some(GUILD_ID), NOW + DAY)
                .expect("claim reminder")
                .is_none()
        );
        let bound = repository
            .bind_message(challenge.challenge_id, Some(GUILD_ID), 1234)
            .expect("bind message");
        assert_eq!(
            repository
                .get_pending_for_recipient(2, Some(GUILD_ID))
                .expect("get pending"),
            Some(bound.clone())
        );
        assert_eq!(
            repository
                .list_outstanding(Some(GUILD_ID))
                .expect("list outstanding"),
            vec![bound]
        );
        assert_eq!(
            repository
                .get_due_challenge_ids(challenge.expires_at)
                .expect("scan due"),
            vec![(challenge.challenge_id, GUILD_ID)]
        );
    }

    #[test]
    fn test_outstanding_reads_are_ordered_and_status_filtered() {
        let file = database();
        for (id, rating, balance) in [
            (1, 1400.0, 550),
            (2, 1500.0, 500),
            (3, 1450.0, 550),
            (4, 1550.0, 500),
        ] {
            seed_player(&file, id, Some(rating), balance, GUILD_ID);
        }
        let repository = DuelChallengeRepository::new(file.path());
        let older = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let newer = create_challenge(&repository, 3, 4, Some(GUILD_ID), NOW + 10);
        repository
            .bind_message(newer.challenge_id, Some(GUILD_ID), 1234)
            .expect("bind newer");
        Connection::open(file.path())
            .expect("open duel test database")
            .execute(
                "UPDATE duel_challenges SET status = 'accepted' WHERE challenge_id = ?1",
                params![older.challenge_id],
            )
            .expect("mark older accepted");
        assert_eq!(
            repository
                .list_outstanding(Some(GUILD_ID))
                .expect("list outstanding")
                .iter()
                .map(|row| row.challenge_id)
                .collect::<Vec<_>>(),
            vec![newer.challenge_id, older.challenge_id]
        );
        assert_eq!(
            repository
                .list_pending_all()
                .expect("list pending")
                .iter()
                .map(|row| row.challenge_id)
                .collect::<Vec<_>>(),
            vec![newer.challenge_id]
        );
        assert_eq!(
            repository
                .get_pending_for_recipient(4, Some(GUILD_ID))
                .expect("get newest pending")
                .expect("pending exists")
                .challenge_id,
            newer.challenge_id
        );
    }

    #[test]
    fn test_delivery_failure_refunds_and_does_not_block_retry() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let first = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let failed = repository
            .mark_delivery_failed_atomic(first.challenge_id, Some(GUILD_ID), NOW + 1, 1)
            .expect("mark initial delivery failed");
        assert_eq!(failed.status, DuelStatus::DeliveryFailed);
        assert_eq!(failed.next_reminder_at, None);
        assert_eq!(balance(&file, 1, GUILD_ID), 550);
        let rows = ledger_rows(&file, first.challenge_id);
        let refund = rows.last().expect("refund ledger row");
        assert_eq!(
            (refund.account_id, refund.delta, refund.reason.as_deref()),
            (1, 550, Some("initial_delivery_refund"))
        );
        assert_eq!(
            refund.metadata.as_deref(),
            Some("{\"wager\": 500, \"issuance_fee\": 50, \"total_refund\": 550}")
        );
        error_contains(
            repository.mark_delivery_failed_atomic(first.challenge_id, Some(GUILD_ID), NOW + 2, 1),
            "pending",
        );
        assert_eq!(balance(&file, 1, GUILD_ID), 550);
        assert_eq!(
            ledger_rows(&file, first.challenge_id)
                .iter()
                .filter(|row| row.reason.as_deref() == Some("initial_delivery_refund"))
                .count(),
            1
        );
        let retry = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW + 2);
        assert_eq!(retry.status, DuelStatus::Pending);
    }

    #[test]
    fn test_delivery_failure_cannot_refund_a_bound_challenge() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let bound = repository
            .bind_message(challenge.challenge_id, Some(GUILD_ID), 1234)
            .expect("bind message");
        error_contains(
            repository.mark_delivery_failed_atomic(bound.challenge_id, Some(GUILD_ID), NOW + 1, 1),
            "unbound pending",
        );
        let stored = repository
            .get_challenge(bound.challenge_id, Some(GUILD_ID))
            .expect("read challenge")
            .expect("challenge exists");
        assert_eq!(stored.status, DuelStatus::Pending);
        assert_eq!(stored.message_id, Some(1234));
        assert_eq!(balance(&file, 1, GUILD_ID), 0);
        assert!(
            ledger_rows(&file, bound.challenge_id)
                .iter()
                .all(|row| row.reason.as_deref() != Some("initial_delivery_refund"))
        );
    }

    #[test]
    fn test_decline_uses_ceiling_half_and_allows_debt() {
        let fixture = bound_fixture(501, 0);
        let declined = fixture
            .repository
            .decline_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                NOW + 100,
                2,
            )
            .expect("decline duel");
        assert_eq!(declined.status, DuelStatus::Declined);
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 751);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), -251);
        assert_eq!(declined.next_reminder_at, None);
        let rows = ledger_rows(&fixture.file, fixture.challenge.challenge_id);
        assert_eq!(
            rows.iter()
                .map(|row| (row.account_id, row.delta))
                .collect::<Vec<_>>(),
            vec![(1, 501), (1, 251), (2, -251)]
        );
        assert!(rows.iter().all(|row| row.source == "duel_challenge"));
        assert!(rows.iter().all(|row| row.actor_id == Some(2)));
        assert!(
            rows.iter()
                .all(|row| row.related_type.as_deref() == Some("duel_challenge"))
        );
        assert!(rows.iter().all(|row| {
            row.related_id.as_deref() == Some(&fixture.challenge.challenge_id.to_string())
        }));
        assert_eq!(
            rows.iter()
                .filter_map(|row| row.reason.as_deref())
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn test_expired_announcement_claim_and_clear_lifecycle() {
        let fixture = bound_fixture(501, 0);
        let now = fixture.challenge.expires_at;
        let expired = fixture
            .repository
            .expire_atomic(fixture.challenge.challenge_id, Some(GUILD_ID), now)
            .expect("expire duel");
        assert_eq!(expired.status, DuelStatus::Expired);
        let balances = (
            balance(&fixture.file, 1, GUILD_ID),
            balance(&fixture.file, 2, GUILD_ID),
        );
        assert_eq!(balances, (751, -251));
        assert_eq!(expired.next_reminder_at, Some(now));
        assert_eq!(
            fixture
                .repository
                .get_due_challenge_ids(now)
                .expect("scan due"),
            vec![(fixture.challenge.challenge_id, GUILD_ID)]
        );
        let claimed = fixture
            .repository
            .claim_expired_announcement_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                now,
                now + 600,
            )
            .expect("claim expiry announcement")
            .expect("announcement is claimable");
        assert_eq!(claimed.kind, DuelDueKind::Expired);
        assert_eq!(claimed.challenge.next_reminder_at, Some(now + 600));
        assert_eq!(claimed.claimed_reminder_at, expired.next_reminder_at);
        assert!(claimed.is_redelivery);
        assert_eq!(
            (
                balance(&fixture.file, 1, GUILD_ID),
                balance(&fixture.file, 2, GUILD_ID)
            ),
            balances
        );
        assert!(
            fixture
                .repository
                .get_due_challenge_ids(now + 599)
                .expect("scan due")
                .is_empty()
        );
        assert!(
            fixture
                .repository
                .claim_expired_announcement_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    now + 599,
                    now + 1200,
                )
                .expect("try early claim")
                .is_none()
        );
        assert_eq!(
            fixture
                .repository
                .get_due_challenge_ids(now + 600)
                .expect("scan due"),
            vec![(fixture.challenge.challenge_id, GUILD_ID)]
        );
        assert!(
            !fixture
                .repository
                .clear_expired_announcement_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    now,
                )
                .expect("stale clear")
        );
        assert!(
            fixture
                .repository
                .clear_expired_announcement_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    now + 600,
                )
                .expect("clear announcement")
        );
        assert!(
            fixture
                .repository
                .get_due_challenge_ids(now + 10_000)
                .expect("scan due")
                .is_empty()
        );
        assert!(
            fixture
                .repository
                .claim_expired_announcement_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    now + 10_000,
                    now + 10_600,
                )
                .expect("try cleared claim")
                .is_none()
        );
    }

    #[test]
    fn test_expiry_rejects_unbound_pending_without_moving_balances() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 550, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let before = (balance(&file, 1, GUILD_ID), balance(&file, 2, GUILD_ID));
        error_contains(
            repository.expire_atomic(challenge.challenge_id, Some(GUILD_ID), challenge.expires_at),
            "pending",
        );
        assert_eq!(
            (balance(&file, 1, GUILD_ID), balance(&file, 2, GUILD_ID)),
            before
        );
        let stored = repository
            .get_challenge(challenge.challenge_id, Some(GUILD_ID))
            .expect("read challenge")
            .expect("challenge exists");
        assert_eq!(stored.status, DuelStatus::Pending);
        assert_eq!(stored.message_id, None);
    }

    #[test]
    fn test_accept_then_winner_receives_double_pot() {
        let fixture = bound_fixture(500, 500);
        let accepted = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW + 100,
                2,
            )
            .expect("accept duel");
        assert_eq!(accepted.status, DuelStatus::Accepted);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 0);
        let resolved = fixture
            .repository
            .resolve_atomic(
                accepted.challenge_id,
                Some(GUILD_ID),
                Some(2),
                NOW + 200,
                99,
            )
            .expect("resolve duel");
        assert_eq!(resolved.status, DuelStatus::Resolved);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 1000);
    }

    #[test]
    fn test_void_refunds_both_stakes() {
        let fixture = bound_fixture(500, 500);
        let accepted = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialOfFive,
                NOW + 100,
                2,
            )
            .expect("accept duel");
        let voided = fixture
            .repository
            .resolve_atomic(accepted.challenge_id, Some(GUILD_ID), None, NOW + 200, 99)
            .expect("void duel");
        assert_eq!(voided.status, DuelStatus::Voided);
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 500);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 500);
        assert!(
            fixture
                .repository
                .get_due_challenge_ids(NOW + 30 * DAY)
                .expect("scan due")
                .is_empty()
        );
        assert!(
            fixture
                .repository
                .claim_unresolved_reminder_atomic(
                    accepted.challenge_id,
                    Some(GUILD_ID),
                    NOW + 30 * DAY,
                )
                .expect("claim unresolved")
                .is_none()
        );
    }

    #[test]
    fn test_unfunded_acceptance_voids_and_refunds_only_challenger_wager() {
        let fixture = bound_fixture(500, 500);
        set_balance(&fixture.file, 2, GUILD_ID, 499);
        let voided = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW + 100,
                2,
            )
            .expect("void unfunded acceptance");
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 500);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 499);
        assert_eq!(voided.status, DuelStatus::Voided);
        assert_eq!(voided.trial_type, None);
        assert_eq!(voided.responded_at, Some(NOW + 100));
        assert_eq!(voided.resolved_at, Some(NOW + 100));
        assert_eq!(voided.resolution_actor_id, Some(2));
        assert_eq!(voided.next_reminder_at, None);
        assert_eq!(
            fixture
                .repository
                .get_challenge(fixture.challenge.challenge_id, Some(GUILD_ID))
                .expect("read challenge"),
            Some(voided)
        );
        let rows = ledger_rows(&fixture.file, fixture.challenge.challenge_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].account_id, rows[0].delta, rows[0].reason.as_deref()),
            (1, 500, Some("unfunded acceptance challenger wager refund"))
        );
        assert_eq!(rows[0].metadata.as_deref(), Some("{\"wager\": 500}"));
        error_contains(
            fixture.repository.accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW + 101,
                2,
            ),
            "Only a pending duel can be accepted",
        );
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 500);
        assert_eq!(
            ledger_rows(&fixture.file, fixture.challenge.challenge_id).len(),
            1
        );
    }

    #[test]
    fn test_accept_rejects_wrong_recipient() {
        let fixture = bound_fixture(500, 0);
        error_contains(
            fixture.repository.accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                3,
                DuelTrial::TrialByCombat,
                NOW + 100,
                3,
            ),
            "recipient",
        );
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 0);
        assert_eq!(
            fixture
                .repository
                .get_challenge(fixture.challenge.challenge_id, Some(GUILD_ID))
                .expect("read challenge")
                .expect("challenge exists")
                .status,
            DuelStatus::Pending
        );
    }

    #[test]
    fn test_expiry_rejects_before_deadline() {
        let fixture = bound_fixture(500, 0);
        error_contains(
            fixture.repository.expire_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                fixture.challenge.expires_at - 1,
            ),
            "expired",
        );
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 0);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 0);
    }

    #[test]
    fn test_resolution_rejects_invalid_winner_and_cross_guild() {
        let fixture = bound_fixture(500, 500);
        let accepted = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW + 100,
                2,
            )
            .expect("accept duel");
        error_contains(
            fixture.repository.resolve_atomic(
                accepted.challenge_id,
                Some(GUILD_ID),
                Some(3),
                NOW + 200,
                99,
            ),
            "winner",
        );
        error_contains(
            fixture.repository.resolve_atomic(
                accepted.challenge_id,
                Some(GUILD_ID + 1),
                Some(1),
                NOW + 200,
                99,
            ),
            "accepted",
        );
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 0);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 0);
    }

    #[test]
    fn test_repeated_transitions_cannot_double_pay() {
        let fixture = bound_fixture(500, 0);
        let declined = fixture
            .repository
            .decline_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                NOW + 100,
                2,
            )
            .expect("decline duel");
        error_contains(
            fixture.repository.decline_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                NOW + 101,
                2,
            ),
            "pending",
        );
        error_contains(
            fixture.repository.accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialOfFive,
                NOW + 101,
                2,
            ),
            "pending",
        );
        assert_eq!(declined.status, DuelStatus::Declined);
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 750);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), -250);
    }

    #[test]
    fn test_accept_and_decline_race_has_exactly_one_success() {
        let fixture = durable_bound_fixture(500, 500);
        let path = fixture.file.path().to_path_buf();
        let challenge_id = fixture.challenge.challenge_id;
        let barrier = Arc::new(Barrier::new(2));
        let accept_barrier = Arc::clone(&barrier);
        let accept_path = path.clone();
        let accept = thread::spawn(move || {
            accept_barrier.wait();
            DuelChallengeRepository::new(accept_path).accept_atomic(
                challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW + 100,
                2,
            )
        });
        let decline = thread::spawn(move || {
            barrier.wait();
            DuelChallengeRepository::new(path).decline_atomic(
                challenge_id,
                Some(GUILD_ID),
                2,
                NOW + 100,
                2,
            )
        });
        let outcomes = [
            accept.join().expect("accept thread"),
            decline.join().expect("decline thread"),
        ];
        let successes = outcomes
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect::<Vec<_>>();
        assert_eq!(successes.len(), 1);
        assert!(matches!(
            successes[0].status,
            DuelStatus::Accepted | DuelStatus::Declined
        ));
        assert!(matches!(
            (
                balance(&fixture.file, 1, GUILD_ID),
                balance(&fixture.file, 2, GUILD_ID)
            ),
            (0, 0) | (750, 250)
        ));
    }

    #[test]
    fn test_due_expiry_takes_precedence_over_reminder() {
        let fixture = bound_fixture(500, 0);
        assert_eq!(
            fixture
                .repository
                .get_due_challenge_ids(fixture.challenge.expires_at)
                .expect("scan due"),
            vec![(fixture.challenge.challenge_id, GUILD_ID)]
        );
        assert!(
            fixture
                .repository
                .claim_reminder_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    fixture.challenge.expires_at,
                )
                .expect("claim reminder")
                .is_none()
        );
        assert_eq!(
            fixture
                .repository
                .expire_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    fixture.challenge.expires_at,
                )
                .expect("expire duel")
                .status,
            DuelStatus::Expired
        );
    }

    #[test]
    fn test_due_scan_returns_guild_pairs_with_expiry_first() {
        let file = database();
        let other_guild = GUILD_ID + 1;
        for guild_id in [GUILD_ID, other_guild] {
            seed_player(&file, 1, Some(1400.0), 550, guild_id);
            seed_player(&file, 2, Some(1500.0), 500, guild_id);
        }
        let repository = DuelChallengeRepository::new(file.path());
        let reminder_due = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        let expiry_due = repository
            .create_challenge_atomic(
                Some(other_guild),
                77,
                1,
                2,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                DAY,
                1,
            )
            .expect("create short expiry challenge");
        repository
            .bind_message(reminder_due.challenge_id, Some(GUILD_ID), 1001)
            .expect("bind reminder challenge");
        repository
            .bind_message(expiry_due.challenge_id, Some(other_guild), 1002)
            .expect("bind expiry challenge");
        assert_eq!(
            repository
                .get_due_challenge_ids(NOW + DAY)
                .expect("scan due"),
            vec![
                (expiry_due.challenge_id, other_guild),
                (reminder_due.challenge_id, GUILD_ID),
            ]
        );
    }

    #[test]
    fn test_reminder_claim_catches_up_to_next_daily_boundary() {
        let fixture = bound_fixture(500, 0);
        let now = fixture.challenge.created_at + 3 * DAY;
        let claimed = fixture
            .repository
            .claim_reminder_atomic(fixture.challenge.challenge_id, Some(GUILD_ID), now)
            .expect("claim reminder")
            .expect("reminder due");
        assert_eq!(claimed.kind, DuelDueKind::Reminder);
        assert_eq!(
            claimed.remaining_seconds,
            fixture.challenge.expires_at - now
        );
        assert_eq!(
            claimed.challenge.next_reminder_at,
            Some(fixture.challenge.created_at + 4 * DAY)
        );
        assert!(!claimed.ping_recipient);
        assert_eq!(
            claimed.claimed_reminder_at,
            fixture.challenge.next_reminder_at
        );
    }

    #[test]
    fn test_failed_delivery_rearms_claimed_reminder_atomically() {
        let fixture = bound_fixture(500, 0);
        let claimed = fixture
            .repository
            .claim_reminder_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                fixture
                    .challenge
                    .next_reminder_at
                    .expect("reminder scheduled"),
            )
            .expect("claim reminder")
            .expect("reminder due");
        assert!(
            fixture
                .repository
                .rearm_reminder_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    DuelStatus::Pending,
                    claimed
                        .challenge
                        .next_reminder_at
                        .expect("claimed schedule"),
                    claimed.claimed_reminder_at.expect("claimed slot"),
                )
                .expect("rearm reminder")
        );
        assert_eq!(
            fixture
                .repository
                .get_challenge(fixture.challenge.challenge_id, Some(GUILD_ID))
                .expect("read challenge")
                .expect("challenge exists")
                .next_reminder_at,
            fixture.challenge.next_reminder_at
        );
    }

    #[test]
    fn test_rearm_does_not_overwrite_a_changed_duel_status() {
        let fixture = bound_fixture(500, 500);
        let claimed = fixture
            .repository
            .claim_reminder_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                fixture
                    .challenge
                    .next_reminder_at
                    .expect("reminder scheduled"),
            )
            .expect("claim reminder")
            .expect("reminder due");
        let accepted = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                fixture.challenge.recipient_id,
                DuelTrial::TrialByCombat,
                fixture
                    .challenge
                    .next_reminder_at
                    .expect("reminder scheduled"),
                fixture.challenge.recipient_id,
            )
            .expect("accept duel");
        assert!(
            !fixture
                .repository
                .rearm_reminder_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    DuelStatus::Pending,
                    claimed
                        .challenge
                        .next_reminder_at
                        .expect("claimed schedule"),
                    claimed.claimed_reminder_at.expect("claimed slot"),
                )
                .expect("try stale rearm")
        );
        assert_eq!(
            fixture
                .repository
                .get_challenge(fixture.challenge.challenge_id, Some(GUILD_ID))
                .expect("read challenge")
                .expect("challenge exists")
                .next_reminder_at,
            accepted.next_reminder_at
        );
    }

    #[test]
    fn test_rearm_does_not_overwrite_a_newer_reminder_schedule() {
        let fixture = bound_fixture(500, 0);
        let claimed = fixture
            .repository
            .claim_reminder_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                fixture
                    .challenge
                    .next_reminder_at
                    .expect("reminder scheduled"),
            )
            .expect("claim reminder")
            .expect("reminder due");
        let newer_schedule = claimed
            .challenge
            .next_reminder_at
            .expect("claimed schedule")
            + 60;
        Connection::open(fixture.file.path())
            .expect("open duel test database")
            .execute(
                "UPDATE duel_challenges SET next_reminder_at = ?1 WHERE challenge_id = ?2",
                params![newer_schedule, fixture.challenge.challenge_id],
            )
            .expect("advance reminder schedule");
        assert!(
            !fixture
                .repository
                .rearm_reminder_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    DuelStatus::Pending,
                    claimed
                        .challenge
                        .next_reminder_at
                        .expect("claimed schedule"),
                    claimed.claimed_reminder_at.expect("claimed slot"),
                )
                .expect("try stale rearm")
        );
        assert_eq!(
            fixture
                .repository
                .get_challenge(fixture.challenge.challenge_id, Some(GUILD_ID))
                .expect("read challenge")
                .expect("challenge exists")
                .next_reminder_at,
            Some(newer_schedule)
        );
    }

    #[test]
    fn test_only_one_concurrent_reminder_claim_succeeds() {
        let fixture = durable_bound_fixture(500, 0);
        let path = fixture.file.path().to_path_buf();
        let challenge_id = fixture.challenge.challenge_id;
        let now = fixture.challenge.created_at + DAY;
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                DuelChallengeRepository::new(path).claim_reminder_atomic(
                    challenge_id,
                    Some(GUILD_ID),
                    now,
                )
            }));
        }
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread").expect("claim call"))
            .filter(Option::is_some)
            .count();
        assert_eq!(successes, 1);
    }

    #[test]
    fn test_reminder_pings_recipient_in_final_48_hours() {
        let fixture = bound_fixture(500, 0);
        let now = fixture.challenge.expires_at - 48 * 3600;
        let claimed = fixture
            .repository
            .claim_reminder_atomic(fixture.challenge.challenge_id, Some(GUILD_ID), now)
            .expect("claim reminder")
            .expect("reminder due");
        assert_eq!(claimed.remaining_seconds, 48 * 3600);
        assert!(claimed.ping_recipient);
    }

    fn accepted_fixture(accepted_at: i64) -> (BoundFixture, DuelChallenge) {
        accepted_fixture_with(bound_fixture(500, 500), accepted_at)
    }

    fn durable_accepted_fixture(accepted_at: i64) -> (BoundFixture, DuelChallenge) {
        accepted_fixture_with(durable_bound_fixture(500, 500), accepted_at)
    }

    fn accepted_fixture_with(
        fixture: BoundFixture,
        accepted_at: i64,
    ) -> (BoundFixture, DuelChallenge) {
        let accepted = fixture
            .repository
            .accept_atomic(
                fixture.challenge.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                accepted_at,
                2,
            )
            .expect("accept fixture challenge");
        (fixture, accepted)
    }

    #[test]
    fn test_accept_schedules_daily_unresolved_reminder() {
        let (_fixture, accepted) = accepted_fixture(NOW + 100);
        assert_eq!(accepted.status, DuelStatus::Accepted);
        assert_eq!(
            accepted.next_reminder_at,
            accepted.responded_at.map(|responded| responded + DAY)
        );
    }

    #[test]
    fn test_unresolved_claim_bumps_to_next_daily_boundary() {
        let (fixture, accepted) = accepted_fixture(NOW + 100);
        let now = accepted.responded_at.expect("responded timestamp") + 3 * DAY + 500;
        let claimed = fixture
            .repository
            .claim_unresolved_reminder_atomic(accepted.challenge_id, Some(GUILD_ID), now)
            .expect("claim unresolved reminder")
            .expect("unresolved reminder due");
        assert_eq!(claimed.kind, DuelDueKind::Unresolved);
        assert_eq!(
            claimed.challenge.next_reminder_at,
            accepted.responded_at.map(|responded| responded + 4 * DAY)
        );
        assert_eq!(claimed.claimed_reminder_at, accepted.next_reminder_at);
        assert!(
            fixture
                .repository
                .claim_unresolved_reminder_atomic(accepted.challenge_id, Some(GUILD_ID), now)
                .expect("second claim")
                .is_none()
        );
    }

    #[test]
    fn test_unresolved_claim_returns_off_grid_rearm_to_daily_grid() {
        let (fixture, accepted) = accepted_fixture(NOW + 100);
        let responded_at = accepted.responded_at.expect("responded timestamp");
        let off_grid = responded_at + DAY + 7 * 3600;
        assert!(
            fixture
                .repository
                .rearm_reminder_atomic(
                    accepted.challenge_id,
                    Some(GUILD_ID),
                    DuelStatus::Accepted,
                    accepted.next_reminder_at.expect("unresolved reminder"),
                    off_grid,
                )
                .expect("rearm unresolved reminder")
        );
        let claimed = fixture
            .repository
            .claim_unresolved_reminder_atomic(accepted.challenge_id, Some(GUILD_ID), off_grid + 100)
            .expect("claim unresolved reminder")
            .expect("unresolved reminder due");
        assert_eq!(
            claimed.challenge.next_reminder_at,
            Some(responded_at + 2 * DAY)
        );
    }

    #[test]
    fn test_due_scan_includes_accepted_unresolved_reminder() {
        let (fixture, accepted) = accepted_fixture(NOW + 100);
        let due_at = accepted.next_reminder_at.expect("unresolved reminder");
        assert!(
            fixture
                .repository
                .get_due_challenge_ids(due_at - 1)
                .expect("scan before due")
                .is_empty()
        );
        assert_eq!(
            fixture
                .repository
                .get_due_challenge_ids(due_at)
                .expect("scan due"),
            vec![(accepted.challenge_id, GUILD_ID)]
        );
    }

    #[test]
    fn test_resolution_stops_unresolved_reminders() {
        let (fixture, accepted) = accepted_fixture(NOW + 100);
        fixture
            .repository
            .resolve_atomic(
                accepted.challenge_id,
                Some(GUILD_ID),
                Some(2),
                NOW + 200,
                99,
            )
            .expect("resolve duel");
        assert!(
            fixture
                .repository
                .get_due_challenge_ids(NOW + 30 * DAY)
                .expect("scan due")
                .is_empty()
        );
        assert!(
            fixture
                .repository
                .claim_unresolved_reminder_atomic(
                    accepted.challenge_id,
                    Some(GUILD_ID),
                    NOW + 30 * DAY,
                )
                .expect("claim resolved reminder")
                .is_none()
        );
    }

    #[test]
    fn test_only_one_concurrent_unresolved_claim_succeeds() {
        let (fixture, accepted) = durable_accepted_fixture(NOW + 100);
        let path = fixture.file.path().to_path_buf();
        let challenge_id = accepted.challenge_id;
        let now = accepted.responded_at.expect("responded timestamp") + DAY;
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                DuelChallengeRepository::new(path).claim_unresolved_reminder_atomic(
                    challenge_id,
                    Some(GUILD_ID),
                    now,
                )
            }));
        }
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread").expect("claim call"))
            .filter(Option::is_some)
            .count();
        assert_eq!(successes, 1);
    }

    #[test]
    fn test_due_scan_orders_pending_expiry_before_unresolved_reminder() {
        let file = database();
        let other_guild = GUILD_ID + 1;
        for guild_id in [GUILD_ID, other_guild] {
            seed_player(&file, 1, Some(1400.0), 550, guild_id);
            seed_player(&file, 2, Some(1500.0), 500, guild_id);
        }
        let repository = DuelChallengeRepository::new(file.path());
        let unresolved_due = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        repository
            .bind_message(unresolved_due.challenge_id, Some(GUILD_ID), 1001)
            .expect("bind unresolved challenge");
        repository
            .accept_atomic(
                unresolved_due.challenge_id,
                Some(GUILD_ID),
                2,
                DuelTrial::TrialByCombat,
                NOW,
                2,
            )
            .expect("accept unresolved challenge");
        let expiry_due = repository
            .create_challenge_atomic(
                Some(other_guild),
                77,
                1,
                2,
                500_i64,
                NOW,
                30 * DAY,
                7 * DAY,
                DAY,
                1,
            )
            .expect("create expiry challenge");
        repository
            .bind_message(expiry_due.challenge_id, Some(other_guild), 1002)
            .expect("bind expiry challenge");
        assert_eq!(
            repository
                .get_due_challenge_ids(NOW + DAY)
                .expect("scan due"),
            vec![
                (expiry_due.challenge_id, other_guild),
                (unresolved_due.challenge_id, GUILD_ID),
            ]
        );
    }

    #[test]
    fn test_schedule_unresolved_duel_reminders_migration_backfills_only_accepted() {
        let file = database();
        let connection = Connection::open(file.path()).expect("open duel test database");
        let seed_row = |challenger_id: i64, status: &str, next_reminder_at: Option<i64>| -> i64 {
            connection
                .execute(
                    "INSERT INTO duel_challenges (
                         guild_id, channel_id, challenger_id, recipient_id, wager,
                         issuance_fee, status, challenger_glicko, challenger_rd,
                         recipient_glicko, recipient_rd, created_at, expires_at,
                         next_reminder_at
                     ) VALUES (?1, 77, ?2, 999, 500, 50, ?3, 1400, 80, 1500, 80,
                               ?4, ?5, ?6)",
                    params![
                        GUILD_ID,
                        challenger_id,
                        status,
                        NOW,
                        NOW + 7 * DAY,
                        next_reminder_at
                    ],
                )
                .expect("seed legacy duel row");
            connection.last_insert_rowid()
        };
        let legacy_accepted = seed_row(1, "accepted", None);
        let already_scheduled = seed_row(2, "accepted", Some(999));
        let pending = seed_row(3, "pending", None);
        let resolved = seed_row(4, "resolved", None);
        let migration_now = 2_000_000;
        let run_backfill = || {
            connection
                .execute(
                    "UPDATE duel_challenges SET next_reminder_at = ?1
                     WHERE status = 'accepted' AND next_reminder_at IS NULL",
                    params![migration_now + DAY],
                )
                .expect("backfill unresolved reminders")
        };
        assert_eq!(run_backfill(), 1);
        let reminder = |challenge_id: i64| {
            connection
                .query_row(
                    "SELECT next_reminder_at FROM duel_challenges WHERE challenge_id = ?1",
                    params![challenge_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .expect("read reminder")
        };
        assert_eq!(reminder(legacy_accepted), Some(migration_now + DAY));
        assert_eq!(reminder(already_scheduled), Some(999));
        assert_eq!(reminder(pending), None);
        assert_eq!(reminder(resolved), None);
        assert_eq!(run_backfill(), 0);
        assert_eq!(reminder(legacy_accepted), Some(migration_now + DAY));
    }

    #[test]
    fn guild_none_and_signed_64_bit_discord_ids_interoperate() {
        let file = database();
        let challenger_id = i64::MIN + 1;
        let recipient_id = i64::MAX - 1;
        seed_player(&file, challenger_id, Some(1400.0), 550, 0);
        seed_player(&file, recipient_id, Some(1500.0), 500, 0);
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, challenger_id, recipient_id, None, NOW);
        assert_eq!(challenge.guild_id, 0);
        assert_eq!(challenge.challenger_id, challenger_id);
        assert_eq!(challenge.recipient_id, recipient_id);
        assert_eq!(
            repository
                .get_challenge(challenge.challenge_id, Some(0))
                .expect("read guild-zero challenge"),
            Some(challenge)
        );
    }

    #[test]
    fn sqlite_real_balance_uses_python_int_precheck_and_sqlite_arithmetic() {
        let file = database();
        seed_player(&file, 1, Some(1400.0), 551, GUILD_ID);
        seed_player(&file, 2, Some(1500.0), 500, GUILD_ID);
        Connection::open(file.path())
            .expect("open duel test database")
            .execute(
                "UPDATE players SET jopacoin_balance = 550.75
                 WHERE discord_id = 1 AND guild_id = ?1",
                params![GUILD_ID],
            )
            .expect("store dynamically typed real balance");
        let repository = DuelChallengeRepository::new(file.path());
        let challenge = create_challenge(&repository, 1, 2, Some(GUILD_ID), NOW);
        assert_eq!(challenge.status, DuelStatus::Pending);
        let remaining = Connection::open(file.path())
            .expect("open duel test database")
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = 1 AND guild_id = ?1",
                params![GUILD_ID],
                |row| row.get::<_, f64>(0),
            )
            .expect("read dynamic balance");
        assert_eq!(remaining, 0.75);
    }

    #[test]
    fn failed_transition_rolls_back_balance_and_ledger_context() {
        let fixture = bound_fixture(500, 0);
        Connection::open(fixture.file.path())
            .expect("open duel test database")
            .execute_batch(
                "CREATE TRIGGER fail_recipient_duel_debit
                 BEFORE UPDATE OF jopacoin_balance ON players
                 WHEN OLD.discord_id = 2
                 BEGIN
                     SELECT RAISE(ABORT, 'forced recipient balance failure');
                 END;",
            )
            .expect("install forced recipient failure");
        assert!(
            fixture
                .repository
                .decline_atomic(
                    fixture.challenge.challenge_id,
                    Some(GUILD_ID),
                    2,
                    NOW + 100,
                    2,
                )
                .is_err()
        );
        assert_eq!(balance(&fixture.file, 1, GUILD_ID), 0);
        assert_eq!(balance(&fixture.file, 2, GUILD_ID), 0);
        assert!(ledger_rows(&fixture.file, fixture.challenge.challenge_id).is_empty());
        let context_count = Connection::open(fixture.file.path())
            .expect("open duel test database")
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count ledger context");
        assert_eq!(context_count, 0);
    }
}
