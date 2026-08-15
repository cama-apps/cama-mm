//! Existing-schema persistence for reserve-backed Dota betting seeds.
//!
//! Python remains the schema and migration authority. Production code in this
//! module only opens an already-migrated database through
//! [`crate::open_runtime_connection`]. DDL is confined to disposable tests.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingMode {
    Pool,
    House,
}

impl BettingMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::House => "house",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingTeam {
    Radiant,
    Dire,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirstGameLobby {
    Open,
    LowSkill,
}

impl FirstGameLobby {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::LowSkill => "lowskill",
        }
    }

    const fn pool_column(self) -> &'static str {
        match self {
            Self::Open => "first_game_open_pool",
            Self::LowSkill => "first_game_lowskill_pool",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeedSplit {
    pub radiant: i64,
    pub dire: i64,
    pub bonus: i64,
}

impl SeedSplit {
    #[must_use]
    pub const fn total(self) -> i64 {
        self.radiant
            .saturating_add(self.dire)
            .saturating_add(self.bonus)
    }
}

#[must_use]
pub const fn split_seed(amount: i64, mode: BettingMode) -> SeedSplit {
    let amount = if amount > 0 { amount } else { 0 };
    match mode {
        BettingMode::Pool => SeedSplit {
            radiant: amount.saturating_add(1) / 2,
            dire: amount / 2,
            bonus: 0,
        },
        BettingMode::House => SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: amount,
        },
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeedReservation {
    pub reserved: i64,
    pub split: SeedSplit,
    pub first_game_reserved: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FirstGamePoolBalances {
    pub open: i64,
    pub low_skill: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FundingOutcome {
    pub processed_dates: Vec<String>,
    pub funded_dates: Vec<String>,
    pub skipped_dates: Vec<String>,
    pub balances: FirstGamePoolBalances,
}

/// Durable lobby-display work emitted in the same transaction as daily pool
/// funding. `generation` is compared on acknowledgement so an older Discord
/// edit can never clear a newer game-date refresh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstGamePoolRefresh {
    pub guild_id: i64,
    pub generation: String,
}

const FIRST_GAME_POOL_REFRESH_KEY: &str = "first_game_pool_display_refresh";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstGameClaim {
    pub guild_id: i64,
    pub lobby: FirstGameLobby,
    pub game_date: String,
    pub pending_match_id: i64,
    pub amount: i64,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeedSettlement {
    /// Seed shape after first-game recovery and legacy-house normalization.
    pub routed: SeedSplit,
    /// Seed made available to winning players.
    pub winner_seed: i64,
    /// Seed atomically credited back to the nonprofit reserve.
    pub returned_to_fund: i64,
    /// Unsettled first-game claim made non-refundable by this call.
    pub first_game_settled: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeedAbort {
    pub consumed: SeedReservation,
    pub first_game_restored: i64,
    pub returned_to_fund: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SeedStock {
    pub available: i64,
    pub queued: i64,
    pub first_game_pools: i64,
    pub active_first_game_claims: i64,
}

impl SeedStock {
    #[must_use]
    pub const fn monetary_stock(self) -> i64 {
        self.available
            .saturating_add(self.queued)
            .saturating_add(self.first_game_pools)
            .saturating_add(self.active_first_game_claims)
    }
}

#[derive(Debug, Error)]
pub enum DotaBetSeedRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("pending match {pending_match_id} does not exist in guild {guild_id}")]
    MissingPendingMatch {
        guild_id: i64,
        pending_match_id: i64,
    },
    #[error("invalid ISO game date: {0}")]
    InvalidGameDate(String),
    #[error("daily first-game amount must be positive")]
    InvalidDailyAmount,
    #[error("daily first-game amount is too large")]
    DailyAmountOverflow,
}

#[derive(Clone, Debug)]
pub struct DotaBetSeedRepository {
    path: PathBuf,
}

impl DotaBetSeedRepository {
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

    /// Process every unhandled game date through `game_date`, funding both
    /// lobby pools or neither on each date. A failed-funds date is still
    /// recorded, matching Python's catch-up/idempotency contract.
    pub fn fund_first_game_pools(
        &self,
        guild_id: Option<i64>,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FundingOutcome, DotaBetSeedRepositoryError> {
        self.fund_first_game_pools_inner(guild_id, game_date, daily_amount, false)
    }

    /// Fund daily pools and atomically enqueue a durable lobby-display
    /// refresh whenever at least one date received liquidity.
    pub fn fund_first_game_pools_and_queue_refresh(
        &self,
        guild_id: Option<i64>,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FundingOutcome, DotaBetSeedRepositoryError> {
        self.fund_first_game_pools_inner(guild_id, game_date, daily_amount, true)
    }

    fn fund_first_game_pools_inner(
        &self,
        guild_id: Option<i64>,
        game_date: &str,
        daily_amount: i64,
        queue_refresh: bool,
    ) -> Result<FundingOutcome, DotaBetSeedRepositoryError> {
        if daily_amount <= 0 {
            return Err(DotaBetSeedRepositoryError::InvalidDailyAmount);
        }
        let pair_cost = daily_amount
            .checked_mul(2)
            .ok_or(DotaBetSeedRepositoryError::DailyAmountOverflow)?;
        let target = CivilDate::parse(game_date)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_fund_row(&transaction, guild_id)?;

        let last_processed = transaction
            .query_row(
                "SELECT MAX(game_date) FROM first_game_pool_funding_days
                  WHERE guild_id = ?1",
                [guild_id],
                |row| row.get::<_, Option<String>>(0),
            )?
            .map(|date| CivilDate::parse(&date))
            .transpose()?;
        let mut next = match last_processed {
            Some(last) => last.next(),
            None => target,
        };
        let mut outcome = FundingOutcome::default();
        while next <= target {
            let date_text = next.to_string();
            let funded = transaction.execute(
                "UPDATE nonprofit_fund
                    SET total_collected = total_collected - ?1,
                        first_game_open_pool = first_game_open_pool + ?2,
                        first_game_lowskill_pool = first_game_lowskill_pool + ?2,
                        updated_at = CURRENT_TIMESTAMP
                  WHERE guild_id = ?3 AND total_collected >= ?1",
                params![pair_cost, daily_amount, guild_id],
            )? == 1;
            transaction.execute(
                "INSERT INTO first_game_pool_funding_days
                     (guild_id, game_date, daily_amount, funded)
                 VALUES (?1, ?2, ?3, ?4)",
                params![guild_id, date_text, daily_amount, i64::from(funded)],
            )?;
            outcome.processed_dates.push(date_text.clone());
            if funded {
                outcome.funded_dates.push(date_text);
            } else {
                outcome.skipped_dates.push(date_text);
            }
            next = next.next();
        }
        if queue_refresh && !outcome.funded_dates.is_empty() {
            transaction.execute(
                "INSERT INTO app_kv (guild_id, key, value)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(guild_id, key) DO UPDATE SET value = excluded.value",
                params![guild_id, FIRST_GAME_POOL_REFRESH_KEY, game_date],
            )?;
        }
        outcome.balances = pool_balances_in_txn(&transaction, guild_id)?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// List durable Discord refreshes in deterministic guild order.
    pub fn pending_first_game_pool_refreshes(
        &self,
    ) -> Result<Vec<FirstGamePoolRefresh>, DotaBetSeedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id, value FROM app_kv
              WHERE key = ?1 ORDER BY guild_id",
        )?;
        let rows = statement.query_map([FIRST_GAME_POOL_REFRESH_KEY], |row| {
            Ok(FirstGamePoolRefresh {
                guild_id: row.get(0)?,
                generation: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Acknowledge exactly the generation that was displayed. Returns false
    /// when newer funding replaced the outbox value while Discord I/O was in
    /// flight, leaving that newer refresh pending for the next drain.
    pub fn acknowledge_first_game_pool_refresh(
        &self,
        refresh: &FirstGamePoolRefresh,
    ) -> Result<bool, DotaBetSeedRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "DELETE FROM app_kv
              WHERE guild_id = ?1 AND key = ?2 AND value = ?3",
            params![
                refresh.guild_id,
                FIRST_GAME_POOL_REFRESH_KEY,
                refresh.generation
            ],
        )? == 1)
    }

    /// Claim a lobby's entire stacked first-game pool once for a game date.
    /// Idempotency is keyed by both the daily uniqueness constraint and the
    /// pending match, including retries after the date rolls over.
    pub fn claim_first_game_pool(
        &self,
        guild_id: Option<i64>,
        lobby: FirstGameLobby,
        game_date: &str,
        pending_match_id: i64,
    ) -> Result<i64, DotaBetSeedRepositoryError> {
        CivilDate::parse(game_date)?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let same_match = transaction
            .query_row(
                "SELECT amount, settled FROM first_game_pool_claims
                  WHERE guild_id = ?1 AND pending_match_id = ?2",
                params![guild_id, pending_match_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        if let Some((amount, settled)) = same_match {
            transaction.commit()?;
            return Ok(if settled { 0 } else { amount.max(0) });
        }
        let date_claimed = transaction
            .query_row(
                "SELECT 1 FROM first_game_pool_claims
                  WHERE guild_id = ?1 AND lobby_kind = ?2 AND game_date = ?3",
                params![guild_id, lobby.as_str(), game_date],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if date_claimed {
            transaction.commit()?;
            return Ok(0);
        }

        ensure_pending_match(&transaction, guild_id, pending_match_id)?;
        ensure_fund_row(&transaction, guild_id)?;
        let select_pool = format!(
            "SELECT COALESCE({}, 0) FROM nonprofit_fund WHERE guild_id = ?1",
            lobby.pool_column()
        );
        let amount = transaction
            .query_row(&select_pool, [guild_id], |row| row.get::<_, i64>(0))?
            .max(0);
        if amount > 0 {
            let clear_pool = format!(
                "UPDATE nonprofit_fund SET {} = 0, updated_at = CURRENT_TIMESTAMP
                  WHERE guild_id = ?1",
                lobby.pool_column()
            );
            transaction.execute(&clear_pool, [guild_id])?;
        }
        transaction.execute(
            "INSERT INTO first_game_pool_claims
                 (guild_id, lobby_kind, game_date, pending_match_id, amount)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                guild_id,
                lobby.as_str(),
                game_date,
                pending_match_id,
                amount
            ],
        )?;
        transaction.commit()?;
        Ok(amount)
    }

    /// Atomically reserve a queued pot (preferred) or up to `max_seed_amount`
    /// from the nonprofit reserve and persist the seed fields in the Python
    /// pending-match JSON. Repeating the call for an already-seeded match is a
    /// read-only idempotent success.
    pub fn reserve_seed_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: i64,
        max_seed_amount: i64,
        first_game_reserved: i64,
        mode: BettingMode,
    ) -> Result<SeedReservation, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = seed_state_in_txn(&transaction, guild_id, pending_match_id)?;
        if existing.reserved > 0 {
            transaction.commit()?;
            return Ok(existing);
        }
        ensure_fund_row(&transaction, guild_id)?;

        let queued = transaction
            .query_row(
                "SELECT COALESCE(next_match_pot, 0) FROM nonprofit_fund
                  WHERE guild_id = ?1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )?
            .max(0);
        let regular_reserved = if queued > 0 {
            transaction.execute(
                "UPDATE nonprofit_fund
                    SET next_match_pot = 0, updated_at = CURRENT_TIMESTAMP
                  WHERE guild_id = ?1",
                [guild_id],
            )?;
            queued
        } else {
            let available = transaction
                .query_row(
                    "SELECT COALESCE(total_collected, 0) FROM nonprofit_fund
                      WHERE guild_id = ?1",
                    [guild_id],
                    |row| row.get::<_, i64>(0),
                )?
                .max(0);
            let deduction = max_seed_amount.max(0).min(available);
            if deduction > 0 {
                let changed = transaction.execute(
                    "UPDATE nonprofit_fund
                        SET total_collected = total_collected - ?1,
                            updated_at = CURRENT_TIMESTAMP
                      WHERE guild_id = ?2 AND total_collected >= ?1",
                    params![deduction, guild_id],
                )?;
                if changed != 1 {
                    return Err(rusqlite::Error::ExecuteReturnedResults.into());
                }
            }
            deduction
        };
        let first_game_reserved = first_game_reserved.max(0);
        let reserved = regular_reserved.saturating_add(first_game_reserved);
        if reserved <= 0 {
            transaction.commit()?;
            return Ok(SeedReservation::default());
        }
        let reservation = SeedReservation {
            reserved,
            split: split_seed(reserved, mode),
            first_game_reserved,
        };
        write_seed_state(&transaction, guild_id, pending_match_id, reservation)?;
        transaction.commit()?;
        Ok(reservation)
    }

    pub fn seed_state(
        &self,
        guild_id: Option<i64>,
        pending_match_id: i64,
    ) -> Result<SeedReservation, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        seed_state_on_connection(&connection, guild_id, pending_match_id)
    }

    /// Consume and zero persisted seed fields, settle an active first-game
    /// claim, normalize legacy house routing, and return the retained portion
    /// to the reserve in one immediate transaction. `winning_team == None`
    /// means there are no real winning bets.
    pub fn settle_seed_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: i64,
        mode: BettingMode,
        winning_team: Option<BettingTeam>,
    ) -> Result<SeedSettlement, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settlement = settle_seed_in_transaction(
            &transaction,
            guild_id,
            pending_match_id,
            mode,
            winning_team,
        )?;
        transaction.commit()?;
        Ok(settlement)
    }

    /// Atomically zero the pending payload, restore any unsettled first-game
    /// claim to its lobby pool, and return only the regular seed portion to the
    /// nonprofit reserve. Repeating an abort is a no-op.
    pub fn abort_seed_atomic(
        &self,
        guild_id: Option<i64>,
        pending_match_id: i64,
    ) -> Result<SeedAbort, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let consumed = consume_seed_state(&transaction, guild_id, pending_match_id)?;
        let first_game_restored =
            release_first_game_claim(&transaction, guild_id, pending_match_id)?;
        let returned_to_fund = consumed.reserved.saturating_sub(first_game_restored).max(0);
        credit_fund(&transaction, guild_id, returned_to_fund)?;
        transaction.commit()?;
        Ok(SeedAbort {
            consumed,
            first_game_restored,
            returned_to_fund,
        })
    }

    pub fn nonprofit_balance(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection
            .query_row(
                "SELECT COALESCE(total_collected, 0) FROM nonprofit_fund
                  WHERE guild_id = ?1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn pool_balances(
        &self,
        guild_id: Option<i64>,
    ) -> Result<FirstGamePoolBalances, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        pool_balances_on_connection(&connection, guild_id)
    }

    /// Return the reserve-backed amount each lobby could seed if it shuffled
    /// immediately, without changing any persisted state.
    ///
    /// When a current game date and positive daily amount are supplied, the
    /// preview simulates the same oldest-first, both-pools-or-neither catch-up
    /// performed by [`Self::fund_first_game_pools`]. A queued next-match pot
    /// takes precedence over the regular capped seed exactly as it does during
    /// reservation.
    pub fn first_game_pool_previews(
        &self,
        guild_id: Option<i64>,
        regular_seed_amount: i64,
        game_date: Option<&str>,
        daily_amount: i64,
    ) -> Result<FirstGamePoolBalances, DotaBetSeedRepositoryError> {
        let target = if daily_amount > 0 {
            game_date.map(CivilDate::parse).transpose()?
        } else {
            None
        };
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        let row = connection
            .query_row(
                "SELECT COALESCE(fund.total_collected, 0),
                        COALESCE(fund.next_match_pot, 0),
                        COALESCE(fund.first_game_open_pool, 0),
                        COALESCE(fund.first_game_lowskill_pool, 0),
                        (SELECT MAX(days.game_date)
                           FROM first_game_pool_funding_days AS days
                          WHERE days.guild_id = fund.guild_id)
                   FROM nonprofit_fund AS fund
                  WHERE fund.guild_id = ?1",
                [guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((available, queued, open, low_skill, last_processed)) = row else {
            return Ok(FirstGamePoolBalances::default());
        };

        let mut available = available.max(0);
        let mut previews = FirstGamePoolBalances {
            open: open.max(0),
            low_skill: low_skill.max(0),
        };
        if let (Some(target), true) = (target, daily_amount > 0) {
            let last_processed = last_processed
                .as_deref()
                .map(CivilDate::parse)
                .transpose()?;
            let pair_cost = daily_amount
                .checked_mul(2)
                .ok_or(DotaBetSeedRepositoryError::DailyAmountOverflow)?;
            let mut next = last_processed.map_or(target, CivilDate::next);
            while next <= target {
                if available >= pair_cost {
                    available -= pair_cost;
                    previews.open = previews.open.saturating_add(daily_amount);
                    previews.low_skill = previews.low_skill.saturating_add(daily_amount);
                }
                next = next.next();
            }
        }

        let regular_seed = if queued > 0 {
            queued
        } else {
            available.min(regular_seed_amount.max(0))
        };
        previews.open = previews.open.saturating_add(regular_seed);
        previews.low_skill = previews.low_skill.saturating_add(regular_seed);
        Ok(previews)
    }

    pub fn first_game_claim(
        &self,
        guild_id: Option<i64>,
        pending_match_id: i64,
    ) -> Result<Option<FirstGameClaim>, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT lobby_kind, game_date, amount, settled
                   FROM first_game_pool_claims
                  WHERE guild_id = ?1 AND pending_match_id = ?2",
                params![guild_id, pending_match_id],
                |row| {
                    let lobby = match row.get::<_, String>(0)?.as_str() {
                        "open" => FirstGameLobby::Open,
                        "lowskill" => FirstGameLobby::LowSkill,
                        other => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                0,
                                other.to_owned(),
                                rusqlite::types::Type::Text,
                            ));
                        }
                    };
                    Ok(FirstGameClaim {
                        guild_id,
                        lobby,
                        game_date: row.get(1)?,
                        pending_match_id,
                        amount: row.get(2)?,
                        settled: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Narrow monetary-stock view used to prove that a claimed first-game pool
    /// remains reserve stock while its pending match exists.
    pub fn seed_stock(
        &self,
        guild_id: Option<i64>,
    ) -> Result<SeedStock, DotaBetSeedRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "WITH reserve AS (
                     SELECT COALESCE(total_collected, 0) AS available,
                            COALESCE(next_match_pot, 0) AS queued,
                            COALESCE(first_game_open_pool, 0)
                              + COALESCE(first_game_lowskill_pool, 0)
                              AS first_game_pools
                       FROM nonprofit_fund WHERE guild_id = ?1
                 ), claims AS (
                     SELECT COALESCE(SUM(c.amount), 0) AS active
                       FROM first_game_pool_claims c
                       JOIN pending_matches p
                         ON p.guild_id = c.guild_id
                        AND p.pending_match_id = c.pending_match_id
                      WHERE c.guild_id = ?1 AND c.settled = 0
                 )
                 SELECT COALESCE((SELECT available FROM reserve), 0),
                        COALESCE((SELECT queued FROM reserve), 0),
                        COALESCE((SELECT first_game_pools FROM reserve), 0),
                        COALESCE((SELECT active FROM claims), 0)",
                [guild_id],
                |row| {
                    Ok(SeedStock {
                        available: row.get(0)?,
                        queued: row.get(1)?,
                        first_game_pools: row.get(2)?,
                        active_first_game_claims: row.get(3)?,
                    })
                },
            )
            .map_err(Into::into)
    }
}

/// Consume and route a pending seed inside a caller-owned match-settlement
/// transaction. This is the single crash-safe path used when real bets must
/// receive the losing-side pool seed or the house bonus before bets are
/// tagged as settled.
pub(crate) fn settle_seed_in_transaction(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
    mode: BettingMode,
    winning_team: Option<BettingTeam>,
) -> Result<SeedSettlement, DotaBetSeedRepositoryError> {
    let consumed = consume_seed_state(transaction, guild_id, pending_match_id)?;
    let first_game_settled = settle_first_game_claim(transaction, guild_id, pending_match_id)?;
    let missing_first_game = first_game_settled
        .saturating_sub(consumed.first_game_reserved)
        .max(0);
    let recovered = split_seed(missing_first_game, mode);
    let mut routed = SeedSplit {
        radiant: consumed.split.radiant.saturating_add(recovered.radiant),
        dire: consumed.split.dire.saturating_add(recovered.dire),
        bonus: consumed.split.bonus.saturating_add(recovered.bonus),
    };
    if mode == BettingMode::House && (routed.radiant != 0 || routed.dire != 0) {
        routed.bonus = routed
            .bonus
            .saturating_add(routed.radiant)
            .saturating_add(routed.dire);
        routed.radiant = 0;
        routed.dire = 0;
    }

    let returned_to_fund = match (mode, winning_team) {
        (_, None) => routed.total(),
        (BettingMode::Pool, Some(BettingTeam::Radiant)) => routed.radiant,
        (BettingMode::Pool, Some(BettingTeam::Dire)) => routed.dire,
        (BettingMode::House, Some(_)) => 0,
    };
    let winner_seed = routed.total().saturating_sub(returned_to_fund);
    credit_fund(transaction, guild_id, returned_to_fund)?;
    Ok(SeedSettlement {
        routed,
        winner_seed,
        returned_to_fund,
        first_game_settled,
    })
}

fn ensure_fund_row(transaction: &Transaction<'_>, guild_id: i64) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO nonprofit_fund
             (guild_id, total_collected, next_match_pot,
              first_game_open_pool, first_game_lowskill_pool, updated_at)
         VALUES (?1, 0, 0, 0, 0, CURRENT_TIMESTAMP)
         ON CONFLICT(guild_id) DO NOTHING",
        [guild_id],
    )?;
    Ok(())
}

fn ensure_pending_match(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<(), DotaBetSeedRepositoryError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM pending_matches
              WHERE guild_id = ?1 AND pending_match_id = ?2",
            params![guild_id, pending_match_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(DotaBetSeedRepositoryError::MissingPendingMatch {
            guild_id,
            pending_match_id,
        })
    }
}

fn seed_state_on_connection(
    connection: &rusqlite::Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<SeedReservation, DotaBetSeedRepositoryError> {
    connection
        .query_row(
            seed_state_sql(),
            params![guild_id, pending_match_id],
            seed_state_from_row,
        )
        .optional()?
        .ok_or(DotaBetSeedRepositoryError::MissingPendingMatch {
            guild_id,
            pending_match_id,
        })
}

fn seed_state_in_txn(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<SeedReservation, DotaBetSeedRepositoryError> {
    transaction
        .query_row(
            seed_state_sql(),
            params![guild_id, pending_match_id],
            seed_state_from_row,
        )
        .optional()?
        .ok_or(DotaBetSeedRepositoryError::MissingPendingMatch {
            guild_id,
            pending_match_id,
        })
}

fn seed_state_sql() -> &'static str {
    "SELECT
         CASE WHEN json_valid(payload)
              THEN CAST(COALESCE(json_extract(payload, '$.bet_seed_reserved'), 0) AS INTEGER)
              ELSE 0 END,
         CASE WHEN json_valid(payload)
              THEN CAST(COALESCE(json_extract(payload, '$.bet_seed_radiant'), 0) AS INTEGER)
              ELSE 0 END,
         CASE WHEN json_valid(payload)
              THEN CAST(COALESCE(json_extract(payload, '$.bet_seed_dire'), 0) AS INTEGER)
              ELSE 0 END,
         CASE WHEN json_valid(payload)
              THEN CAST(COALESCE(json_extract(payload, '$.bet_seed_bonus'), 0) AS INTEGER)
              ELSE 0 END,
         CASE WHEN json_valid(payload)
              THEN CAST(COALESCE(json_extract(payload, '$.first_game_pool_reserved'), 0) AS INTEGER)
              ELSE 0 END
       FROM pending_matches
      WHERE guild_id = ?1 AND pending_match_id = ?2"
}

fn seed_state_from_row(row: &rusqlite::Row<'_>) -> Result<SeedReservation, rusqlite::Error> {
    Ok(SeedReservation {
        reserved: row.get::<_, i64>(0)?.max(0),
        split: SeedSplit {
            radiant: row.get::<_, i64>(1)?.max(0),
            dire: row.get::<_, i64>(2)?.max(0),
            bonus: row.get::<_, i64>(3)?.max(0),
        },
        first_game_reserved: row.get::<_, i64>(4)?.max(0),
    })
}

fn write_seed_state(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
    seed: SeedReservation,
) -> Result<(), DotaBetSeedRepositoryError> {
    let changed = transaction.execute(
        "UPDATE pending_matches
            SET payload = json_set(
                    CASE WHEN json_valid(payload) THEN payload ELSE '{}' END,
                    '$.bet_seed_reserved', ?1,
                    '$.bet_seed_radiant', ?2,
                    '$.bet_seed_dire', ?3,
                    '$.bet_seed_bonus', ?4,
                    '$.first_game_pool_reserved', ?5
                ),
                updated_at = CURRENT_TIMESTAMP
          WHERE guild_id = ?6 AND pending_match_id = ?7",
        params![
            seed.reserved,
            seed.split.radiant,
            seed.split.dire,
            seed.split.bonus,
            seed.first_game_reserved,
            guild_id,
            pending_match_id
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(DotaBetSeedRepositoryError::MissingPendingMatch {
            guild_id,
            pending_match_id,
        })
    }
}

fn consume_seed_state(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<SeedReservation, DotaBetSeedRepositoryError> {
    let seed = seed_state_in_txn(transaction, guild_id, pending_match_id)?;
    if seed != SeedReservation::default() {
        write_seed_state(
            transaction,
            guild_id,
            pending_match_id,
            SeedReservation::default(),
        )?;
    }
    Ok(seed)
}

fn settle_first_game_claim(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<i64, rusqlite::Error> {
    let amount = transaction
        .query_row(
            "SELECT amount FROM first_game_pool_claims
              WHERE guild_id = ?1 AND pending_match_id = ?2 AND settled = 0",
            params![guild_id, pending_match_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        .max(0);
    if amount > 0 {
        transaction.execute(
            "UPDATE first_game_pool_claims SET settled = 1
              WHERE guild_id = ?1 AND pending_match_id = ?2 AND settled = 0",
            params![guild_id, pending_match_id],
        )?;
    }
    Ok(amount)
}

fn release_first_game_claim(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<i64, rusqlite::Error> {
    let claim = transaction
        .query_row(
            "SELECT lobby_kind, amount FROM first_game_pool_claims
              WHERE guild_id = ?1 AND pending_match_id = ?2 AND settled = 0",
            params![guild_id, pending_match_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((lobby_kind, amount)) = claim else {
        return Ok(0);
    };
    let amount = amount.max(0);
    let pool_column = match lobby_kind.as_str() {
        "open" => FirstGameLobby::Open.pool_column(),
        "lowskill" => FirstGameLobby::LowSkill.pool_column(),
        _ => return Ok(0),
    };
    if amount > 0 {
        let restore = format!(
            "UPDATE nonprofit_fund
                SET {pool_column} = {pool_column} + ?1,
                    updated_at = CURRENT_TIMESTAMP
              WHERE guild_id = ?2"
        );
        transaction.execute(&restore, params![amount, guild_id])?;
    }
    transaction.execute(
        "DELETE FROM first_game_pool_claims
          WHERE guild_id = ?1 AND pending_match_id = ?2",
        params![guild_id, pending_match_id],
    )?;
    Ok(amount)
}

fn credit_fund(
    transaction: &Transaction<'_>,
    guild_id: i64,
    amount: i64,
) -> Result<(), rusqlite::Error> {
    if amount <= 0 {
        return Ok(());
    }
    ensure_fund_row(transaction, guild_id)?;
    transaction.execute(
        "UPDATE nonprofit_fund
            SET total_collected = total_collected + ?1,
                updated_at = CURRENT_TIMESTAMP
          WHERE guild_id = ?2",
        params![amount, guild_id],
    )?;
    Ok(())
}

fn pool_balances_on_connection(
    connection: &rusqlite::Connection,
    guild_id: i64,
) -> Result<FirstGamePoolBalances, DotaBetSeedRepositoryError> {
    Ok(connection
        .query_row(
            "SELECT COALESCE(first_game_open_pool, 0),
                    COALESCE(first_game_lowskill_pool, 0)
               FROM nonprofit_fund WHERE guild_id = ?1",
            [guild_id],
            |row| {
                Ok(FirstGamePoolBalances {
                    open: row.get(0)?,
                    low_skill: row.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn pool_balances_in_txn(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<FirstGamePoolBalances, rusqlite::Error> {
    transaction.query_row(
        "SELECT COALESCE(first_game_open_pool, 0),
                COALESCE(first_game_lowskill_pool, 0)
           FROM nonprofit_fund WHERE guild_id = ?1",
        [guild_id],
        |row| {
            Ok(FirstGamePoolBalances {
                open: row.get(0)?,
                low_skill: row.get(1)?,
            })
        },
    )
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

impl CivilDate {
    fn parse(value: &str) -> Result<Self, DotaBetSeedRepositoryError> {
        let mut parts = value.split('-');
        let parsed = (|| {
            let year = parts.next()?.parse::<i32>().ok()?;
            let month = parts.next()?.parse::<u32>().ok()?;
            let day = parts.next()?.parse::<u32>().ok()?;
            if parts.next().is_some() || year < 1 || !(1..=12).contains(&month) {
                return None;
            }
            let date = Self { year, month, day };
            (day >= 1 && day <= date.days_in_month()).then_some(date)
        })();
        parsed.ok_or_else(|| DotaBetSeedRepositoryError::InvalidGameDate(value.to_owned()))
    }

    const fn days_in_month(self) -> u32 {
        match self.month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap_year(self.year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    const fn next(self) -> Self {
        if self.day < self.days_in_month() {
            Self {
                day: self.day + 1,
                ..self
            }
        } else if self.month < 12 {
            Self {
                year: self.year,
                month: self.month + 1,
                day: 1,
            }
        } else {
            Self {
                year: self.year + 1,
                month: 1,
                day: 1,
            }
        }
    }
}

impl std::fmt::Display for CivilDate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[cfg(test)]
#[path = "dota_bet_seed/tests.rs"]
mod tests;
