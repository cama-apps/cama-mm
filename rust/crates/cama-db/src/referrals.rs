//! Existing-schema persistence for guild-scoped referral enrollment and first-match rewards.
//!
//! Python remains the schema, migration, and live-runtime authority. This adapter opens only
//! an existing SQLite database through [`crate::open_runtime_connection`]. Enrollment and match
//! settlement use `BEGIN IMMEDIATE`; all DDL in this tranche lives in disposable test fixtures.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

pub const REFERRAL_REWARD: i64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchSide {
    Radiant,
    Dire,
}

impl MatchSide {
    #[must_use]
    pub const fn number(self) -> i64 {
        match self {
            Self::Radiant => 1,
            Self::Dire => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordReferralMatchRequest<'a> {
    pub guild_id: Option<i64>,
    pub radiant_ids: &'a [i64],
    pub dire_ids: &'a [i64],
    pub winning_side: MatchSide,
    pub pending_match_id: i64,
    pub recorded_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralRecord {
    pub guild_id: i64,
    pub referred_id: i64,
    pub referrer_id: i64,
    pub rewarded_match_id: Option<i64>,
    pub reward_amount: Option<i64>,
    pub rewarded_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferralReward {
    pub referred_id: i64,
    pub referrer_id: i64,
    pub reward_amount: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordReferralMatchOutcome {
    pub match_id: i64,
    pub inserted: bool,
    pub rewards: Vec<ReferralReward>,
    pub referral_jc_changes: BTreeMap<i64, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralLedgerEntry {
    pub account_id: i64,
    pub delta: i64,
    pub source: String,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Error)]
pub enum ReferralRepositoryError {
    #[error("referral SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Self-referrals are not allowed.")]
    SelfReferral,
    #[error("Referrer must be registered in this guild.")]
    ReferrerNotRegistered,
    #[error("That player has already played a game in this guild.")]
    ReferredAlreadyPlayed,
    #[error("That player already has a referral.")]
    AlreadyReferred,
    #[error("a referral match must have at least one participant and no duplicate players")]
    InvalidParticipants,
    #[error("match {0} does not exist in this guild")]
    MissingMatch(i64),
    #[error("referral persistence invariant failed: {0}")]
    Invariant(&'static str),
}

#[derive(Clone, Debug)]
pub struct ReferralRepository {
    path: PathBuf,
}

impl ReferralRepository {
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

    /// Enroll a player who has not yet played a guild game under a registered referrer.
    ///
    /// `BEGIN IMMEDIATE` serializes the existence checks and the unique insert, so two processes
    /// racing to enroll the same target produce exactly one success.
    pub fn create_referral(
        &self,
        referrer_id: i64,
        referred_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(), ReferralRepositoryError> {
        if referrer_id == referred_id {
            return Err(ReferralRepositoryError::SelfReferral);
        }

        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if !player_exists(&transaction, referrer_id, guild_id)? {
            return Err(ReferralRepositoryError::ReferrerNotRegistered);
        }
        if player_games_played(&transaction, referred_id, guild_id)?.is_some_and(|games| games > 0)
        {
            return Err(ReferralRepositoryError::ReferredAlreadyPlayed);
        }
        if referral_exists(&transaction, referred_id, guild_id)? {
            return Err(ReferralRepositoryError::AlreadyReferred);
        }

        match transaction.execute(
            "INSERT INTO referrals (guild_id, referred_id, referrer_id)
             VALUES (?1, ?2, ?3)",
            params![guild_id, referred_id, referrer_id],
        ) {
            Ok(1) => transaction.commit().map_err(Into::into),
            Ok(_) => Err(ReferralRepositoryError::Invariant(
                "enrollment insert affected an unexpected number of rows",
            )),
            Err(error) if is_constraint_violation(&error) => {
                Err(ReferralRepositoryError::AlreadyReferred)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Persist the bounded match core and settle all eligible first-match referrals together.
    ///
    /// The pending-match lookup is the retry guard. An existing row returns its persisted reward
    /// projection without updating counters or balances again. On a new row, match insertion,
    /// participants, paired balance credits, ledger context, referral claims, JC components, and
    /// win/loss counters share one `BEGIN IMMEDIATE` transaction.
    pub fn record_match_with_referrals(
        &self,
        request: RecordReferralMatchRequest<'_>,
    ) -> Result<RecordReferralMatchOutcome, ReferralRepositoryError> {
        validate_participants(request.radiant_ids, request.dire_ids)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(match_id) = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id = ?1 AND pending_match_id = ?2",
                params![guild_id, request.pending_match_id],
                |row| row.get(0),
            )
            .optional()?
        {
            let rewards = referral_rewards_on(&transaction, match_id, guild_id)?;
            let referral_jc_changes = aggregate_referral_changes(&rewards);
            transaction.commit()?;
            return Ok(RecordReferralMatchOutcome {
                match_id,
                inserted: false,
                rewards,
                referral_jc_changes,
            });
        }

        transaction.execute(
            "INSERT INTO matches (
                 guild_id, team1_players, team2_players, winning_team,
                 lobby_type, lobby_kind, balancing_rating_system,
                 pending_match_id, match_date
             ) VALUES (?1, ?2, ?3, ?4, 'shuffle', 'open', 'glicko', ?5, ?6)",
            params![
                guild_id,
                encode_ids(request.radiant_ids),
                encode_ids(request.dire_ids),
                request.winning_side.number(),
                request.pending_match_id,
                request.recorded_at,
            ],
        )?;
        let match_id = transaction.last_insert_rowid();

        insert_participants(&transaction, match_id, guild_id, request)?;
        let rewards = settle_first_match_referrals(
            &transaction,
            match_id,
            guild_id,
            request.radiant_ids,
            request.dire_ids,
            request.recorded_at,
        )?;
        let referral_jc_changes = aggregate_referral_changes(&rewards);
        if !referral_jc_changes.is_empty() {
            transaction.execute(
                "UPDATE matches SET jc_changes = ?1
                 WHERE match_id = ?2 AND guild_id = ?3",
                params![
                    encode_referral_jc_changes(&referral_jc_changes),
                    match_id,
                    guild_id
                ],
            )?;
        }

        update_match_counters(&transaction, guild_id, request)?;
        transaction.commit()?;
        Ok(RecordReferralMatchOutcome {
            match_id,
            inserted: true,
            rewards,
            referral_jc_changes,
        })
    }

    /// Correct the winner while deliberately leaving referral claims, balances, and JC metadata
    /// untouched. Referral rewards are a first-record event, not a reversible result payout.
    pub fn correct_match_result(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
        new_winning_side: MatchSide,
    ) -> Result<(), ReferralRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old_winning_team = transaction
            .query_row(
                "SELECT winning_team FROM matches
                 WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(ReferralRepositoryError::MissingMatch(match_id))?;
        let new_winning_team = new_winning_side.number();
        if old_winning_team == new_winning_team {
            transaction.commit()?;
            return Ok(());
        }

        let participants = {
            let mut statement = transaction.prepare(
                "SELECT discord_id, team_number FROM match_participants
                 WHERE match_id = ?1 AND guild_id = ?2 ORDER BY discord_id",
            )?;
            statement
                .query_map(params![match_id, guild_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };

        let changed = transaction.execute(
            "UPDATE matches SET winning_team = ?1
             WHERE match_id = ?2 AND guild_id = ?3 AND winning_team = ?4",
            params![new_winning_team, match_id, guild_id, old_winning_team],
        )?;
        if changed != 1 {
            return Err(ReferralRepositoryError::Invariant(
                "winner correction lost its guarded match row",
            ));
        }

        for (discord_id, team_number) in participants {
            let old_won = team_number == old_winning_team;
            let new_won = team_number == new_winning_team;
            transaction.execute(
                "UPDATE players SET
                     wins = COALESCE(wins, 0) + ?1,
                     losses = COALESCE(losses, 0) + ?2,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?3 AND guild_id = ?4",
                params![
                    i64::from(new_won) - i64::from(old_won),
                    i64::from(!new_won) - i64::from(!old_won),
                    discord_id,
                    guild_id
                ],
            )?;
            transaction.execute(
                "UPDATE match_participants SET won = ?1
                 WHERE match_id = ?2 AND discord_id = ?3 AND guild_id = ?4",
                params![new_won, match_id, discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn referral(
        &self,
        referred_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<ReferralRecord>, ReferralRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT guild_id, referred_id, referrer_id, rewarded_match_id,
                        reward_amount, rewarded_at
                 FROM referrals WHERE guild_id = ?1 AND referred_id = ?2",
                params![Self::normalize_guild_id(guild_id), referred_id],
                |row| {
                    Ok(ReferralRecord {
                        guild_id: row.get(0)?,
                        referred_id: row.get(1)?,
                        referrer_id: row.get(2)?,
                        rewarded_match_id: row.get(3)?,
                        reward_amount: row.get(4)?,
                        rewarded_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn referral_rewards_for_match(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<ReferralReward>, ReferralRepositoryError> {
        let connection = self.connection()?;
        referral_rewards_on(&connection, match_id, Self::normalize_guild_id(guild_id))
    }

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, ReferralRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn match_jc_changes_json(
        &self,
        match_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<String>, ReferralRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT jc_changes FROM matches WHERE match_id = ?1 AND guild_id = ?2",
                params![match_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn match_count(&self) -> Result<i64, ReferralRepositoryError> {
        self.connection()?
            .query_row("SELECT COUNT(*) FROM matches", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn ledger_entries(&self) -> Result<Vec<ReferralLedgerEntry>, ReferralRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT account_id, delta, source, related_type, related_id, metadata
             FROM economy_ledger_entries
             WHERE source = 'referral_reward' ORDER BY ledger_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(ReferralLedgerEntry {
                    account_id: row.get(0)?,
                    delta: row.get(1)?,
                    source: row.get(2)?,
                    related_type: row.get(3)?,
                    related_id: row.get(4)?,
                    metadata: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn referral_schema_columns(&self) -> Result<BTreeSet<String>, ReferralRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("PRAGMA table_info(referrals)")?;
        statement
            .query_map([], |row| row.get(1))?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(Into::into)
    }
}

fn player_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn player_games_played(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(wins, 0) + COALESCE(losses, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn referral_exists(
    connection: &Connection,
    referred_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM referrals WHERE referred_id = ?1 AND guild_id = ?2",
            params![referred_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(sqlite_error, _)
            if sqlite_error.code == ErrorCode::ConstraintViolation
    )
}

fn validate_participants(
    radiant_ids: &[i64],
    dire_ids: &[i64],
) -> Result<(), ReferralRepositoryError> {
    let all = radiant_ids
        .iter()
        .chain(dire_ids)
        .copied()
        .collect::<Vec<_>>();
    let unique = all.iter().copied().collect::<BTreeSet<_>>();
    if all.is_empty() || all.len() != unique.len() {
        return Err(ReferralRepositoryError::InvalidParticipants);
    }
    Ok(())
}

fn insert_participants(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
    request: RecordReferralMatchRequest<'_>,
) -> Result<(), rusqlite::Error> {
    for (team, ids) in [
        (MatchSide::Radiant, request.radiant_ids),
        (MatchSide::Dire, request.dire_ids),
    ] {
        let won = team == request.winning_side;
        for discord_id in ids {
            transaction.execute(
                "INSERT INTO match_participants (
                     match_id, discord_id, guild_id, team_number, won, side
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    match_id,
                    discord_id,
                    guild_id,
                    team.number(),
                    won,
                    match team {
                        MatchSide::Radiant => "radiant",
                        MatchSide::Dire => "dire",
                    }
                ],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn settle_first_match_referrals(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
    radiant_ids: &[i64],
    dire_ids: &[i64],
    rewarded_at: i64,
) -> Result<Vec<ReferralReward>, ReferralRepositoryError> {
    let participant_ids = radiant_ids
        .iter()
        .chain(dire_ids)
        .copied()
        .collect::<Vec<_>>();
    if participant_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = std::iter::repeat_n("?", participant_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT r.referred_id, r.referrer_id
         FROM referrals r
         JOIN players referred
           ON referred.guild_id = r.guild_id
          AND referred.discord_id = r.referred_id
         JOIN players referrer
           ON referrer.guild_id = r.guild_id
          AND referrer.discord_id = r.referrer_id
         WHERE r.guild_id = ?
           AND r.rewarded_match_id IS NULL
           AND r.referred_id IN ({placeholders})
           AND COALESCE(referred.wins, 0) + COALESCE(referred.losses, 0) = 0
         ORDER BY r.referred_id"
    );
    let mut values = Vec::with_capacity(participant_ids.len() + 1);
    values.push(Value::Integer(guild_id));
    values.extend(participant_ids.into_iter().map(Value::Integer));
    let eligible = {
        let mut statement = transaction.prepare(&sql)?;
        statement
            .query_map(params_from_iter(values), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut rewards = Vec::with_capacity(eligible.len());
    for (referred_id, referrer_id) in eligible {
        for (beneficiary_id, beneficiary_role) in
            [(referred_id, "referred"), (referrer_id, "referrer")]
        {
            set_ledger_context(
                transaction,
                match_id,
                referred_id,
                referrer_id,
                beneficiary_role,
            )?;
            let changed = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE guild_id = ?2 AND discord_id = ?3",
                params![REFERRAL_REWARD, guild_id, beneficiary_id],
            )?;
            if changed != 1 {
                return Err(ReferralRepositoryError::Invariant(
                    "eligible beneficiary disappeared during settlement",
                ));
            }
        }

        let claimed = transaction.execute(
            "UPDATE referrals
             SET rewarded_match_id = ?1, reward_amount = ?2, rewarded_at = ?3
             WHERE guild_id = ?4 AND referred_id = ?5 AND rewarded_match_id IS NULL",
            params![
                match_id,
                REFERRAL_REWARD,
                rewarded_at,
                guild_id,
                referred_id
            ],
        )?;
        if claimed != 1 {
            return Err(ReferralRepositoryError::Invariant(
                "eligible referral claim lost its compare-and-set",
            ));
        }
        clear_ledger_context(transaction)?;
        rewards.push(ReferralReward {
            referred_id,
            referrer_id,
            reward_amount: REFERRAL_REWARD,
        });
    }
    Ok(rewards)
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    match_id: i64,
    referred_id: i64,
    referrer_id: i64,
    beneficiary_role: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    let metadata = format!(
        "{{\"match_id\":{match_id},\"referrer_id\":{referrer_id},\
         \"referred_id\":{referred_id},\"beneficiary_role\":\"{beneficiary_role}\"}}"
    );
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'referral_reward', NULL, 'referral', ?1,
                   'First-match referral reward', ?2)",
        params![referred_id.to_string(), metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

pub(crate) fn aggregate_referral_changes(rewards: &[ReferralReward]) -> BTreeMap<i64, i64> {
    let mut changes = BTreeMap::new();
    for reward in rewards {
        for beneficiary_id in [reward.referred_id, reward.referrer_id] {
            *changes.entry(beneficiary_id).or_insert(0) += reward.reward_amount;
        }
    }
    changes
}

pub(crate) fn encode_referral_jc_changes(changes: &BTreeMap<i64, i64>) -> String {
    format!(
        "{{{}}}",
        changes
            .iter()
            .map(|(discord_id, amount)| { format!("\"{discord_id}\":{{\"referral\":{amount}}}") })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn update_match_counters(
    transaction: &Transaction<'_>,
    guild_id: i64,
    request: RecordReferralMatchRequest<'_>,
) -> Result<(), rusqlite::Error> {
    for (team, ids) in [
        (MatchSide::Radiant, request.radiant_ids),
        (MatchSide::Dire, request.dire_ids),
    ] {
        let won = team == request.winning_side;
        for discord_id in ids {
            transaction.execute(
                "UPDATE players SET
                     wins = COALESCE(wins, 0) + ?1,
                     losses = COALESCE(losses, 0) + ?2,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?3 AND guild_id = ?4",
                params![i64::from(won), i64::from(!won), discord_id, guild_id],
            )?;
        }
    }
    Ok(())
}

fn referral_rewards_on(
    connection: &Connection,
    match_id: i64,
    guild_id: i64,
) -> Result<Vec<ReferralReward>, ReferralRepositoryError> {
    let mut statement = connection.prepare(
        "SELECT referred_id, referrer_id, reward_amount
         FROM referrals
         WHERE guild_id = ?1 AND rewarded_match_id = ?2
         ORDER BY referred_id",
    )?;
    statement
        .query_map(params![guild_id, match_id], |row| {
            Ok(ReferralReward {
                referred_id: row.get(0)?,
                referrer_id: row.get(1)?,
                reward_amount: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn encode_ids(ids: &[i64]) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
#[path = "referrals/tests.rs"]
mod tests;
