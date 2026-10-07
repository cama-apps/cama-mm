//! Guild-scoped Deadlock markets backed by the existing JC wallets.
//!
//! Every mutation is serialized with the Dota economy through BEGIN IMMEDIATE.
//! Deadlock IDs never enter Dota match, bet, seed, or tax tables. Setup, wallet
//! movements, and their receipts commit together; retries cannot charge twice.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::betting_service_repository::{PendingBetRecord, pool_payouts, rounded_percentage};
use crate::dota_bet_seed_repository::BettingTeam;
use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum DeadlockBettingError {
    #[error("Deadlock betting storage: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Deadlock betting snapshot: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
type Result<T> = std::result::Result<T, DeadlockBettingError>;
fn invalid(message: impl Into<String>) -> DeadlockBettingError {
    DeadlockBettingError::Invalid(message.into())
}

/// Economic terms are immutable once a market opens. Tax eligibility and rates
/// are frozen here; each bettor's bankruptcy basis is frozen at their first bet.
/// Dota match-count obligations are deliberately not advanced by Deadlock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeadlockMarketTerms {
    pub betting_window_seconds: i64,
    pub min_bet: i64,
    pub max_debt: i64,
    pub seed: i64,
    pub blind_threshold: i64,
    pub blind_percentage: i64,
    pub payout_multiplier: f64,
    pub minigame_jc_delta_scale: f64,
    pub bankruptcy_rate_per_game: f64,
    pub vanity_tax_rate: f64,
    pub vanity_taxable_ids: BTreeSet<i64>,
    pub low_priority_tax_rate: f64,
    pub low_priority_taxable_ids: BTreeSet<i64>,
}
impl Default for DeadlockMarketTerms {
    fn default() -> Self {
        Self {
            betting_window_seconds: 180,
            min_bet: 1,
            max_debt: 500,
            seed: 0,
            blind_threshold: 50,
            blind_percentage: 10,
            payout_multiplier: 1.0,
            minigame_jc_delta_scale: 1.0,
            bankruptcy_rate_per_game: 0.05,
            vanity_tax_rate: 0.0,
            vanity_taxable_ids: BTreeSet::new(),
            low_priority_tax_rate: 0.0,
            low_priority_taxable_ids: BTreeSet::new(),
        }
    }
}
impl DeadlockMarketTerms {
    fn validate(&self) -> Result<()> {
        if !(1..=86400).contains(&self.betting_window_seconds)
            || self.min_bet <= 0
            || self.max_debt < 0
            || self.seed < 0
            || self.blind_threshold < 0
            || !(0..=100).contains(&self.blind_percentage)
            || !self.payout_multiplier.is_finite()
            || !self.minigame_jc_delta_scale.is_finite()
            || self.minigame_jc_delta_scale <= 0.0
            || !(0.0..=10.0).contains(&self.payout_multiplier)
            || ![
                self.bankruptcy_rate_per_game,
                self.vanity_tax_rate,
                self.low_priority_tax_rate,
            ]
            .iter()
            .all(|rate| rate.is_finite() && (0.0..=1.0).contains(rate))
        {
            return Err(invalid("Invalid Deadlock market terms"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadlockMarket {
    pub market_id: String,
    pub guild_id: i64,
    pub match_id: i64,
    pub format: String,
    pub status: String,
    pub deadline: i64,
    pub seed: i64,
    pub team1: Vec<i64>,
    pub team2: Vec<i64>,
    pub pool1: i64,
    pub pool2: i64,
    pub terms: DeadlockMarketTerms,
}
#[derive(Clone, Debug)]
pub struct DeadlockBetRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub discord_id: i64,
    pub side: i64,
    pub amount: i64,
    pub leverage: i64,
    /// Resolve the existing active mana effects in the runtime, as for Dota.
    pub allow_10x: bool,
    /// Existing Green mana placement reward after scaling/economy adjustment.
    /// Persisted atomically with the wager; automatic setup always passes zero.
    pub green_mana_bonus: i64,
    /// Stable interaction/event identity, never a per-attempt random value.
    pub request_key: String,
    pub now: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlockWager {
    pub wager_id: i64,
    pub market_id: String,
    pub discord_id: i64,
    pub side: i64,
    pub amount: i64,
    pub leverage: i64,
    pub effective_stake: i64,
    pub kind: String,
    pub created_at: i64,
    pub payout: Option<i64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlockSettlement {
    pub market_id: String,
    pub gross_payouts: BTreeMap<i64, i64>,
    pub deductions: BTreeMap<i64, i64>,
    pub refunds: BTreeMap<i64, i64>,
}
pub type DeadlockEconomyRecovery = Vec<(i64, i64, std::result::Result<DeadlockSettlement, String>)>;
/// Durable outbox input for the existing protection-aware Blood Pact engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeadlockBloodPactJob {
    pub market_id: String,
    pub guild_id: i64,
    pub discord_id: i64,
    pub earning: i64,
    pub event_key: String,
    pub occurred_at: i64,
    pub minigame_jc_delta_scale: f64,
}
#[derive(Clone, Debug)]
pub struct DeadlockBettingRepository {
    path: PathBuf,
}
#[derive(Deserialize)]
struct Player {
    discord_id: i64,
}
#[derive(Deserialize)]
struct Teams {
    team1: Vec<Player>,
    team2: Vec<Player>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UserPolicy {
    bankruptcy_rate: f64,
    vanity_rate: f64,
    low_priority_rate: f64,
}

impl DeadlockBettingRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
    fn connection(&self) -> Result<Connection> {
        Ok(open_runtime_connection(&self.path)?)
    }

    pub fn market(&self, guild_id: i64, match_id: i64) -> Result<Option<DeadlockMarket>> {
        market(&self.connection()?, guild_id, match_id)
    }
    pub fn wagers(&self, guild_id: i64, match_id: i64) -> Result<Vec<DeadlockWager>> {
        let connection = self.connection()?;
        let market = market(&connection, guild_id, match_id)?
            .ok_or_else(|| invalid("Deadlock market not found"))?;
        wagers(&connection, guild_id, &market.market_id)
    }
    pub fn user_wagers(
        &self,
        guild_id: i64,
        discord_id: i64,
        active_only: bool,
    ) -> Result<Vec<DeadlockWager>> {
        let connection = self.connection()?;
        let mut statement=connection.prepare(&format!("SELECT {WAGER_COLUMNS} FROM deadlock_wagers WHERE guild_id=?1 AND discord_id=?2 AND (?3=0 OR market_id IN(SELECT market_id FROM deadlock_betting_markets WHERE guild_id=?1 AND status IN('open','closed'))) ORDER BY created_at DESC,wager_id DESC"))?;
        Ok(statement
            .query_map(params![guild_id, discord_id, active_only], wager_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn seed_balance(&self, guild_id: i64) -> Result<i64> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT balance FROM deadlock_betting_funds WHERE guild_id=?1",
                [guild_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
    /// Setup is one transaction: funding, participant blinds, opted-in
    /// investments, then opted-in spectator liquidity. No public bet can see
    /// or alter the market until every setup operation commits.
    pub fn open_market(
        &self,
        guild_id: i64,
        match_id: i64,
        terms: &DeadlockMarketTerms,
        now: i64,
    ) -> Result<DeadlockMarket> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = market(&tx, guild_id, match_id)? {
            return Ok(existing);
        }
        let (format,status,roster,frozen_terms):(String,String,String,String)=tx.query_row(
            "SELECT format,status,roster_json,economy_terms_json FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
            params![guild_id,match_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .optional()?.ok_or_else(||invalid("Deadlock match not found"))?;
        let frozen = if frozen_terms == "{}" {
            terms.clone()
        } else {
            serde_json::from_str(&frozen_terms)?
        };
        let terms = &frozen;
        terms.validate()?;
        if status != "economic_setup" {
            return Err(invalid("Match is not awaiting economic setup"));
        }
        let teams: Teams = serde_json::from_str(&roster)?;
        let required = match format.as_str() {
            "street_brawl" => 4,
            "standard" => 6,
            _ => return Err(invalid("Invalid Deadlock format")),
        };
        let unique = teams
            .team1
            .iter()
            .chain(&teams.team2)
            .map(|p| p.discord_id)
            .collect::<BTreeSet<_>>();
        if teams.team1.len() != required
            || teams.team2.len() != required
            || unique.len() != required * 2
            || unique.iter().any(|id| *id <= 0)
        {
            return Err(invalid("Invalid frozen Deadlock roster"));
        }
        let market_id = format!("deadlock:{match_id}");
        let deadline = now
            .checked_add(terms.betting_window_seconds)
            .ok_or_else(|| invalid("Deadline overflow"))?;
        let fund: i64 = tx
            .query_row(
                "SELECT balance FROM deadlock_betting_funds WHERE guild_id=?1",
                [guild_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if fund < terms.seed {
            return Err(invalid(
                "Deadlock seed fund is insufficient; fund it or configure a zero seed",
            ));
        }
        tx.execute("INSERT INTO deadlock_betting_markets(market_id,guild_id,match_id,format,roster_json,terms_json,deadline,status,seed,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'open',?8,?9)",
            params![market_id,guild_id,match_id,format,roster,serde_json::to_string(terms)?,deadline,terms.seed,now])?;
        if terms.seed > 0 {
            tx.execute(
                "UPDATE deadlock_betting_funds SET balance=balance-?1 WHERE guild_id=?2",
                params![terms.seed, guild_id],
            )?;
            receipt(
                &tx,
                &market_id,
                guild_id,
                None,
                "seed_reserved",
                terms.seed,
                "{}",
                now,
            )?;
        }
        let mut current = market(&tx, guild_id, match_id)?
            .ok_or_else(|| invalid("Missing newly created market"))?;
        // Snapshot eligibility before blinds, but investments size from balances
        // after blinds. Missing participant wallets fail the entire setup.
        let mut balances = BTreeMap::new();
        for id in &unique {
            balances.insert(*id, balance(&tx, guild_id, *id)?);
        }
        let investments = read_investments(&tx, guild_id)?;
        let liquidity = read_liquidity(&tx, guild_id)?;
        for id in investments
            .iter()
            .map(|i| i.0)
            .chain(liquidity.iter().map(|i| i.0))
        {
            if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(id) {
                entry.insert(balance(&tx, guild_id, id)?);
            }
        }
        for (side, players) in [(1, &current.team1), (2, &current.team2)] {
            for id in players {
                let available = balances[id];
                if available >= terms.blind_threshold && terms.blind_percentage > 0 {
                    let amount = rounded_percentage(available, terms.blind_percentage)
                        .map_err(|error| invalid(error.to_string()))?;
                    if amount > 0 {
                        place(
                            &tx,
                            &current,
                            &DeadlockBetRequest {
                                guild_id,
                                match_id,
                                discord_id: *id,
                                side,
                                amount,
                                leverage: 1,
                                allow_10x: false,
                                green_mana_bonus: 0,
                                request_key: format!("{market_id}:blind:{id}"),
                                now,
                            },
                            "blind",
                            true,
                        )?;
                    }
                }
            }
        }
        let mut investment_bases = BTreeMap::new();
        for (id, _, _, _) in &investments {
            investment_bases.insert(*id, balance(&tx, guild_id, *id)?.max(0));
        }
        for (investor, target, direction, pct) in investments {
            if balances[&investor] < 0 {
                continue;
            }
            let target_side = participant_side(&current, target);
            let Some(target_side) = target_side else {
                continue;
            };
            let side = if direction == "long" {
                target_side
            } else {
                3 - target_side
            };
            if participant_side(&current, investor).is_some_and(|own| own != side) {
                continue;
            }
            let amount = percentage(investment_bases[&investor], pct)?;
            if amount <= 0 {
                continue;
            }
            place(
                &tx,
                &current,
                &DeadlockBetRequest {
                    guild_id,
                    match_id,
                    discord_id: investor,
                    side,
                    amount,
                    leverage: 1,
                    allow_10x: false,
                    green_mana_bonus: 0,
                    request_key: format!("{market_id}:investment:{investor}:{target}"),
                    now,
                },
                "investment",
                true,
            )?;
        }
        for (id, pct) in liquidity {
            if unique.contains(&id) || balances[&id] < 0 {
                continue;
            }
            current =
                market(&tx, guild_id, match_id)?.ok_or_else(|| invalid("Market disappeared"))?;
            let side = if current.pool1 <= current.pool2 { 1 } else { 2 };
            let amount = percentage(balance(&tx, guild_id, id)?.max(0), pct)?;
            if amount <= 0 {
                continue;
            }
            place(
                &tx,
                &current,
                &DeadlockBetRequest {
                    guild_id,
                    match_id,
                    discord_id: id,
                    side,
                    amount,
                    leverage: 1,
                    allow_10x: false,
                    green_mana_bonus: 0,
                    request_key: format!("{market_id}:liquidity:{id}"),
                    now,
                },
                "liquidity",
                true,
            )?;
        }
        tx.execute("UPDATE deadlock_matches SET status='gathering',updated_at=?1 WHERE guild_id=?2 AND match_id=?3 AND status='economic_setup'",params![now,guild_id,match_id])?;
        receipt(&tx, &market_id, guild_id, None, "setup", 0, "{}", now)?;
        let result =
            market(&tx, guild_id, match_id)?.ok_or_else(|| invalid("Market disappeared"))?;
        tx.commit()?;
        Ok(result)
    }
    pub fn place_bet(&self, request: &DeadlockBetRequest) -> Result<DeadlockWager> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = market(&transaction, request.guild_id, request.match_id)?
            .ok_or_else(|| invalid("Deadlock market not found"))?;
        let result = place(&transaction, &current, request, "manual", false)?;
        transaction.commit()?;
        Ok(result)
    }
    /// This commit must precede any external launch request. Closure is sticky
    /// across reconnects and ambiguous Steam responses.
    pub fn close_market(&self, guild_id: i64, match_id: i64, now: i64) -> Result<DeadlockMarket> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            market(&tx, guild_id, match_id)?.ok_or_else(|| invalid("Deadlock market not found"))?;
        let state: String = tx.query_row(
            "SELECT status FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
            params![guild_id, match_id],
            |row| row.get(0),
        )?;
        if !matches!(state.as_str(), "gathering" | "running") {
            return Err(invalid(
                "Only an active Deadlock match can close betting for launch",
            ));
        }
        if !matches!(current.status.as_str(), "open" | "closed") {
            return Err(invalid("Deadlock market is already resolved"));
        }
        tx.execute("UPDATE deadlock_betting_markets SET status='closed',closed_at=COALESCE(closed_at,?1) WHERE guild_id=?2 AND match_id=?3",params![now,guild_id,match_id])?;
        let result =
            market(&tx, guild_id, match_id)?.ok_or_else(|| invalid("Market disappeared"))?;
        tx.commit()?;
        Ok(result)
    }
    pub fn settle_recorded(
        &self,
        guild_id: i64,
        match_id: i64,
        now: i64,
    ) -> Result<DeadlockSettlement> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            market(&tx, guild_id, match_id)?.ok_or_else(|| invalid("Deadlock market not found"))?;
        let (status, winner, recorded_at): (String, Option<i64>, i64) = tx.query_row(
            "SELECT status,winner,updated_at FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
            params![guild_id, match_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if !matches!(status.as_str(), "recorded" | "settled") {
            return Err(invalid(
                "Deadlock result must be recorded before settlement",
            ));
        }
        let winner = winner
            .filter(|side| matches!(side, 1 | 2))
            .ok_or_else(|| invalid("Recorded result has no valid winner"))?;
        if let Some(result) = settlement_receipt(&tx, &current.market_id, "settlement")? {
            return Ok(result);
        }
        if matches!(current.status.as_str(), "refunded" | "settled") {
            return Err(invalid(
                "Market already resolved without a matching receipt",
            ));
        }
        let bets = wagers(&tx, guild_id, &current.market_id)?;
        let total = bets
            .iter()
            .try_fold(0_i64, |sum, bet| sum.checked_add(bet.effective_stake))
            .ok_or_else(|| invalid("Pool overflow"))?;
        total
            .checked_add(current.seed)
            .ok_or_else(|| invalid("Seeded pool overflow"))?;
        let legacy = bets
            .iter()
            .map(|bet| PendingBetRecord {
                bet_id: bet.wager_id,
                guild_id,
                discord_id: bet.discord_id,
                team: team(bet.side),
                amount: bet.amount,
                bet_time: bet.created_at,
                leverage: bet.leverage,
                is_blind: bet.kind == "blind",
                odds_at_placement: None,
                pending_match_id: None,
                investment_target_id: None,
                investment_direction: None,
            })
            .collect::<Vec<_>>();
        let payouts = pool_payouts(
            &legacy,
            team(winner),
            current.seed,
            current.terms.payout_multiplier,
        );
        let mut result = DeadlockSettlement {
            market_id: current.market_id.clone(),
            ..Default::default()
        };
        let mut stakes = BTreeMap::<i64, i64>::new();
        for bet in &bets {
            let payout = payouts.get(&bet.wager_id).copied().unwrap_or(0);
            tx.execute(
                "UPDATE deadlock_wagers SET payout=?1 WHERE wager_id=?2 AND guild_id=?3",
                params![payout, bet.wager_id, guild_id],
            )?;
            checked_accumulate(&mut stakes, bet.discord_id, bet.effective_stake)?;
            checked_accumulate(&mut result.gross_payouts, bet.discord_id, payout)?;
        }
        for (id, gross) in &result.gross_payouts {
            let profit = gross.saturating_sub(stakes[id]).max(0);
            let policy_json: String = tx.query_row(
                "SELECT detail_json FROM deadlock_economic_receipts WHERE receipt_id=?1",
                [format!("{}:policy:{id}", current.market_id)],
                |r| r.get(0),
            )?;
            let policy: UserPolicy = serde_json::from_str(&policy_json)?;
            let bankruptcy = (profit as f64 * policy.bankruptcy_rate) as i64;
            let vanity = ((profit as f64 * policy.vanity_rate) as i64).min(profit - bankruptcy);
            let low_priority = ((profit as f64 * policy.low_priority_rate) as i64)
                .min(profit - bankruptcy - vanity);
            if *gross > 0 {
                wallet_delta(
                    &tx,
                    &current.market_id,
                    guild_id,
                    *id,
                    *gross,
                    "deadlock_payout",
                    now,
                )?;
            }
            for (kind, amount) in [
                ("bankruptcy", bankruptcy),
                ("vanity_tax", vanity),
                ("low_priority_tax", low_priority),
            ] {
                if amount > 0 {
                    wallet_delta(&tx, &current.market_id, guild_id, *id, -amount, kind, now)?;
                    receipt(
                        &tx,
                        &current.market_id,
                        guild_id,
                        Some(*id),
                        kind,
                        amount,
                        "{}",
                        now,
                    )?;
                }
            }
            result
                .deductions
                .insert(*id, bankruptcy + vanity + low_priority);
            let earning = profit - bankruptcy - vanity - low_priority;
            if earning > 0 {
                let job = DeadlockBloodPactJob {
                    market_id: current.market_id.clone(),
                    guild_id,
                    discord_id: *id,
                    earning,
                    event_key: format!("deadlock-bet-blood-pact:{match_id}:{id}"),
                    occurred_at: recorded_at,
                    minigame_jc_delta_scale: current.terms.minigame_jc_delta_scale,
                };
                receipt(
                    &tx,
                    &current.market_id,
                    guild_id,
                    Some(*id),
                    "blood_pact_pending",
                    earning,
                    &serde_json::to_string(&job)?,
                    now,
                )?;
            }
        }
        // Dota's zero-winning-stake rule pays no bettors. Return unused seed
        // to its funding source; unclaimed player stakes remain a sink, just
        // like Dota. Taxes likewise remain sinks, never a new seed subsidy.
        if payouts.is_empty() {
            fund_credit(&tx, guild_id, current.seed)?;
            receipt(
                &tx,
                &current.market_id,
                guild_id,
                None,
                "unclaimed_pool",
                total,
                "{}",
                now,
            )?;
        }
        receipt(
            &tx,
            &current.market_id,
            guild_id,
            None,
            "settlement",
            0,
            &serde_json::to_string(&result)?,
            now,
        )?;
        tx.execute("UPDATE deadlock_betting_markets SET status='settled',closed_at=COALESCE(closed_at,?1),resolved_at=?1 WHERE guild_id=?2 AND match_id=?3",params![now,guild_id,match_id])?;
        tx.execute("UPDATE deadlock_matches SET status='settled',updated_at=?1 WHERE guild_id=?2 AND match_id=?3",params![now,guild_id,match_id])?;
        tx.commit()?;
        Ok(result)
    }
    /// Core abort writes the audited reason and releases participant claims.
    /// This operation refunds only an already-aborted match, allowing restart
    /// recovery without races with result recording.
    pub fn refund_aborted(
        &self,
        guild_id: i64,
        match_id: i64,
        now: i64,
    ) -> Result<DeadlockSettlement> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = tx.query_row(
            "SELECT status FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
            params![guild_id, match_id],
            |r| r.get(0),
        )?;
        if status != "aborted" {
            return Err(invalid("Only an aborted match can be refunded"));
        }
        let Some(current) = market(&tx, guild_id, match_id)? else {
            return Ok(DeadlockSettlement {
                market_id: format!("deadlock:{match_id}"),
                ..Default::default()
            });
        };
        if let Some(result) = settlement_receipt(&tx, &current.market_id, "refund")? {
            return Ok(result);
        }
        if !matches!(current.status.as_str(), "open" | "closed") {
            return Err(invalid("Market cannot be refunded"));
        }
        let mut result = DeadlockSettlement {
            market_id: current.market_id.clone(),
            ..Default::default()
        };
        for bet in wagers(&tx, guild_id, &current.market_id)? {
            checked_accumulate(&mut result.refunds, bet.discord_id, bet.effective_stake)?;
        }
        for (id, amount) in &result.refunds {
            wallet_delta(
                &tx,
                &current.market_id,
                guild_id,
                *id,
                *amount,
                "deadlock_refund",
                now,
            )?;
        }
        fund_credit(&tx, guild_id, current.seed)?;
        receipt(
            &tx,
            &current.market_id,
            guild_id,
            None,
            "refund",
            0,
            &serde_json::to_string(&result)?,
            now,
        )?;
        tx.execute("UPDATE deadlock_betting_markets SET status='refunded',closed_at=COALESCE(closed_at,?1),resolved_at=?1 WHERE guild_id=?2 AND match_id=?3",params![now,guild_id,match_id])?;
        tx.commit()?;
        Ok(result)
    }
    /// No caller-supplied winner is accepted by recovery; the durable result
    /// table is the sole source of truth. One failed market leaves others usable.
    pub fn recover(&self, now: i64) -> Result<DeadlockEconomyRecovery> {
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT m.guild_id,m.match_id,m.status FROM deadlock_matches m LEFT JOIN deadlock_betting_markets b ON b.guild_id=m.guild_id AND b.match_id=m.match_id WHERE (m.status IN ('recorded','aborted') AND b.status IN ('open','closed')) OR (m.status='economic_setup' AND m.economy_terms_json<>'{}' AND b.market_id IS NULL) ORDER BY m.match_id")?;
        let pending = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        Ok(pending
            .into_iter()
            .map(|(guild, id, status)| {
                let result = if status == "recorded" {
                    self.settle_recorded(guild, id, now)
                } else if status == "aborted" {
                    self.refund_aborted(guild, id, now)
                } else {
                    self.open_market(guild, id, &DeadlockMarketTerms::default(), now)
                        .map(|market| DeadlockSettlement {
                            market_id: market.market_id,
                            ..Default::default()
                        })
                };
                (guild, id, result.map_err(|error| error.to_string()))
            })
            .collect())
    }
    pub fn pending_blood_pacts(&self) -> Result<Vec<DeadlockBloodPactJob>> {
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT r.detail_json FROM deadlock_economic_receipts r WHERE r.kind='blood_pact_pending' AND NOT EXISTS(SELECT 1 FROM deadlock_economic_receipts done WHERE done.market_id=r.market_id AND done.guild_id=r.guild_id AND done.discord_id=r.discord_id AND done.kind='blood_pact_complete') ORDER BY r.created_at,r.receipt_id")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|json| serde_json::from_str(&json).map_err(Into::into))
            .collect()
    }
    /// Acknowledge only after the atomic protected transfer has returned a
    /// durable receipt (including a confirmed no-op). Retry is harmless.
    pub fn complete_blood_pact(&self, job: &DeadlockBloodPactJob, applied: i64) -> Result<()> {
        if applied < 0 {
            return Err(invalid("Invalid Blood Pact transfer"));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: String = tx.query_row(
            "SELECT detail_json FROM deadlock_economic_receipts WHERE receipt_id=?1",
            [format!(
                "{}:blood_pact_pending:{}",
                job.market_id, job.discord_id
            )],
            |r| r.get(0),
        )?;
        if serde_json::from_str::<DeadlockBloodPactJob>(&stored)? != *job {
            return Err(invalid("Blood Pact job payload mismatch"));
        }
        let key = format!("{}:blood_pact_complete:{}", job.market_id, job.discord_id);
        if let Some(prior) = tx
            .query_row(
                "SELECT amount FROM deadlock_economic_receipts WHERE receipt_id=?1",
                [key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            if prior != applied {
                return Err(invalid("Blood Pact completion conflicts"));
            }
            return Ok(());
        }
        receipt(
            &tx,
            &job.market_id,
            job.guild_id,
            Some(job.discord_id),
            "blood_pact_complete",
            applied,
            "{}",
            job.occurred_at,
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Explicit Deadlock opt-in; legacy Dota investment rows are untouched.
    /// Both games spend the same serialized wallet and cannot overdraw it.
    pub fn set_investment(
        &self,
        guild_id: i64,
        investor: i64,
        target: i64,
        direction: &str,
        percentage: i64,
    ) -> Result<()> {
        if !matches!(direction, "long" | "short")
            || !(0..=10).contains(&percentage)
            || investor <= 0
            || target <= 0
        {
            return Err(invalid("Investments require long/short and 0–10 percent"));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        balance(&tx, guild_id, investor)?;
        let sum:i64=tx.query_row("SELECT (SELECT COALESCE(SUM(percentage),0) FROM deadlock_betting_investments WHERE guild_id=?1 AND investor_id=?2 AND target_id<>?3) + (SELECT COALESCE(SUM(percentage),0) FROM autobet_investments WHERE guild_id=?1 AND investor_id=?2)",params![guild_id,investor,target],|r|r.get(0))?;
        if sum + percentage > 50 {
            return Err(invalid(
                "Investments across Dota and Deadlock cannot exceed 50 percent in total",
            ));
        }
        if percentage == 0 {
            tx.execute("DELETE FROM deadlock_betting_investments WHERE guild_id=?1 AND investor_id=?2 AND target_id=?3",params![guild_id,investor,target])?;
        } else {
            tx.execute("INSERT INTO deadlock_betting_investments(guild_id,investor_id,target_id,direction,percentage) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(guild_id,investor_id,target_id) DO UPDATE SET direction=excluded.direction,percentage=excluded.percentage",params![guild_id,investor,target,direction,percentage])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn set_liquidity(&self, guild_id: i64, discord_id: i64, percentage: i64) -> Result<()> {
        if !(0..=50).contains(&percentage) {
            return Err(invalid("Liquidity requires 0–50 percent"));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        balance(&tx, guild_id, discord_id)?;
        if percentage == 0 {
            tx.execute(
                "DELETE FROM deadlock_betting_liquidity WHERE guild_id=?1 AND discord_id=?2",
                params![guild_id, discord_id],
            )?;
        } else {
            tx.execute("INSERT INTO deadlock_betting_liquidity(guild_id,discord_id,percentage) VALUES(?1,?2,?3) ON CONFLICT(guild_id,discord_id) DO UPDATE SET percentage=excluded.percentage",params![guild_id,discord_id,percentage])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Transfer an explicit contribution from a wallet into the Deadlock seed
    /// earmark. This cannot consume Dota treasury allocations or mint currency.
    pub fn contribute_seed(
        &self,
        guild_id: i64,
        discord_id: i64,
        amount: i64,
        request_key: &str,
        now: i64,
    ) -> Result<()> {
        if amount <= 0 || request_key.is_empty() {
            return Err(invalid(
                "A positive contribution and stable request identity are required",
            ));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = format!("deadlock:fund:{guild_id}:{request_key}");
        if let Some((id, prior)) = tx
            .query_row(
                "SELECT discord_id,amount FROM deadlock_economic_receipts WHERE receipt_id=?1",
                [&key],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
        {
            if id != discord_id || prior != amount {
                return Err(invalid("Contribution request identity conflicts"));
            }
            return Ok(());
        }
        if balance(&tx, guild_id, discord_id)? < amount {
            return Err(invalid("Insufficient wallet balance for seed contribution"));
        }
        wallet_delta(
            &tx,
            &key,
            guild_id,
            discord_id,
            -amount,
            "deadlock_seed_contribution",
            now,
        )?;
        fund_credit(&tx, guild_id, amount)?;
        tx.execute("INSERT INTO deadlock_economic_receipts(receipt_id,market_id,guild_id,discord_id,kind,amount,created_at) VALUES(?1,?1,?2,?3,'seed_contribution',?4,?5)",params![key,guild_id,discord_id,amount,now])?;
        tx.commit()?;
        Ok(())
    }
}

fn team(side: i64) -> BettingTeam {
    if side == 1 {
        BettingTeam::Radiant
    } else {
        BettingTeam::Dire
    }
}
fn checked_accumulate(values: &mut BTreeMap<i64, i64>, id: i64, amount: i64) -> Result<()> {
    let value = values.entry(id).or_default();
    *value = value
        .checked_add(amount)
        .ok_or_else(|| invalid("Economic arithmetic overflow"))?;
    Ok(())
}
fn percentage(balance: i64, pct: i64) -> Result<i64> {
    balance
        .checked_mul(pct)
        .map(|n| n / 100)
        .ok_or_else(|| invalid("Percentage overflow"))
}
fn participant_side(market: &DeadlockMarket, id: i64) -> Option<i64> {
    if market.team1.contains(&id) {
        Some(1)
    } else if market.team2.contains(&id) {
        Some(2)
    } else {
        None
    }
}
fn balance(connection: &Connection, guild: i64, id: i64) -> Result<i64> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players WHERE guild_id=?1 AND discord_id=?2",
            params![guild, id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid("Player has no wallet in this guild"))
}
fn market(connection: &Connection, guild: i64, id: i64) -> Result<Option<DeadlockMarket>> {
    let row=connection.query_row("SELECT market_id,format,roster_json,terms_json,deadline,status,seed FROM deadlock_betting_markets WHERE guild_id=?1 AND match_id=?2",params![guild,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?))).optional()?;
    let Some((market_id, format, roster_json, terms_json, deadline, status, seed)) = row else {
        return Ok(None);
    };
    let teams: Teams = serde_json::from_str(&roster_json)?;
    let (pool1,pool2)=connection.query_row("SELECT COALESCE(SUM(CASE WHEN side=1 THEN effective_stake ELSE 0 END),0),COALESCE(SUM(CASE WHEN side=2 THEN effective_stake ELSE 0 END),0) FROM deadlock_wagers WHERE guild_id=?1 AND market_id=?2",params![guild,market_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok(Some(DeadlockMarket {
        market_id,
        guild_id: guild,
        match_id: id,
        format,
        status,
        deadline,
        seed,
        team1: teams.team1.iter().map(|p| p.discord_id).collect(),
        team2: teams.team2.iter().map(|p| p.discord_id).collect(),
        pool1,
        pool2,
        terms: serde_json::from_str(&terms_json)?,
    }))
}
fn wager_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeadlockWager> {
    Ok(DeadlockWager {
        wager_id: row.get(0)?,
        market_id: row.get(1)?,
        discord_id: row.get(2)?,
        side: row.get(3)?,
        amount: row.get(4)?,
        leverage: row.get(5)?,
        effective_stake: row.get(6)?,
        kind: row.get(7)?,
        created_at: row.get(8)?,
        payout: row.get(9)?,
    })
}
const WAGER_COLUMNS: &str =
    "wager_id,market_id,discord_id,side,amount,leverage,effective_stake,kind,created_at,payout";
fn wagers(connection: &Connection, guild: i64, id: &str) -> Result<Vec<DeadlockWager>> {
    let mut statement=connection.prepare(&format!("SELECT {WAGER_COLUMNS} FROM deadlock_wagers WHERE guild_id=?1 AND market_id=?2 ORDER BY created_at,wager_id"))?;
    Ok(statement
        .query_map(params![guild, id], wager_row)?
        .collect::<std::result::Result<_, _>>()?)
}
fn place(
    tx: &Transaction<'_>,
    market: &DeadlockMarket,
    request: &DeadlockBetRequest,
    kind: &str,
    automatic: bool,
) -> Result<DeadlockWager> {
    if request.guild_id != market.guild_id
        || request.match_id != market.match_id
        || request.discord_id <= 0
        || request.request_key.is_empty()
        || request.request_key.len() > 200
    {
        return Err(invalid("Invalid market or request identity"));
    }
    if let Some(existing) = tx
        .query_row(
            &format!(
                "SELECT {WAGER_COLUMNS} FROM deadlock_wagers WHERE guild_id=?1 AND request_key=?2"
            ),
            params![request.guild_id, request.request_key],
            wager_row,
        )
        .optional()?
    {
        if existing.market_id != market.market_id
            || existing.discord_id != request.discord_id
            || existing.side != request.side
            || existing.amount != request.amount
            || existing.leverage != request.leverage
            || existing.kind != kind
        {
            return Err(invalid(
                "Wager request identity conflicts with its original payload",
            ));
        }
        return Ok(existing);
    }
    if !matches!(request.side, 1 | 2)
        || request.green_mana_bonus < 0
        || !matches!(request.leverage, 1 | 2 | 3 | 5 | 10)
        || request.amount <= 0
        || (!automatic && request.amount < market.terms.min_bet)
    {
        return Err(invalid("Invalid amount, team, or leverage"));
    }
    if request.leverage == 10 && !request.allow_10x {
        return Err(invalid("10x leverage requires active Red mana"));
    }
    let state: String = tx.query_row(
        "SELECT status FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
        params![market.guild_id, market.match_id],
        |r| r.get(0),
    )?;
    let allowed = state == "gathering" || (automatic && state == "economic_setup");
    if market.status != "open" || request.now >= market.deadline || !allowed {
        return Err(invalid("Deadlock betting is closed"));
    }
    if participant_side(market, request.discord_id).is_some_and(|side| side != request.side) {
        return Err(invalid("Participants may bet only on their own team"));
    }
    let stake = request
        .amount
        .checked_mul(request.leverage)
        .ok_or_else(|| invalid("Stake overflow"))?;
    let available = balance(tx, request.guild_id, request.discord_id)?;
    let next = available
        .checked_sub(stake)
        .ok_or_else(|| invalid("Wallet overflow"))?;
    if available < 0 {
        return Err(invalid("Players in debt cannot place bets"));
    }
    if request.leverage == 1 && next < 0 {
        return Err(invalid("Insufficient wallet balance"));
    }
    if request.leverage > 1 && next < -market.terms.max_debt {
        return Err(invalid("Wager exceeds the maximum debt"));
    }
    // Freeze per-bettor penalty basis before the first debit. No future Dota
    // result or changed configuration can alter a retried settlement.
    let policy_key = format!("{}:policy:{}", market.market_id, request.discord_id);
    let exists = tx
        .query_row(
            "SELECT 1 FROM deadlock_economic_receipts WHERE receipt_id=?1",
            [&policy_key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        let games = cama_db_core::profit_deductions::penalty_games_remaining(
            tx,
            request.guild_id,
            request.discord_id,
        )?;
        let policy = UserPolicy {
            bankruptcy_rate: cama_domain::bankruptcy::withheld_rate(
                market.terms.bankruptcy_rate_per_game,
                games,
            ),
            vanity_rate: if market
                .terms
                .vanity_taxable_ids
                .contains(&request.discord_id)
            {
                market.terms.vanity_tax_rate
            } else {
                0.0
            },
            low_priority_rate: if market
                .terms
                .low_priority_taxable_ids
                .contains(&request.discord_id)
            {
                market.terms.low_priority_tax_rate
            } else {
                0.0
            },
        };
        tx.execute("INSERT INTO deadlock_economic_receipts(receipt_id,market_id,guild_id,discord_id,kind,amount,detail_json,created_at) VALUES(?1,?2,?3,?4,'policy',0,?5,?6)",params![policy_key,market.market_id,request.guild_id,request.discord_id,serde_json::to_string(&policy)?,request.now])?;
    }
    wallet_delta(
        tx,
        &market.market_id,
        request.guild_id,
        request.discord_id,
        -stake,
        "deadlock_bet",
        request.now,
    )?;
    tx.execute("INSERT INTO deadlock_wagers(market_id,guild_id,discord_id,side,amount,leverage,effective_stake,request_key,kind,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![market.market_id,request.guild_id,request.discord_id,request.side,request.amount,request.leverage,stake,request.request_key,kind,request.now])?;
    let wager_id = tx.last_insert_rowid();
    if !automatic {
        tx.execute("UPDATE players SET total_bets_placed=COALESCE(total_bets_placed,0)+1 WHERE guild_id=?1 AND discord_id=?2",params![request.guild_id,request.discord_id])?;
        if request.green_mana_bonus > 0 {
            wallet_delta(
                tx,
                &market.market_id,
                request.guild_id,
                request.discord_id,
                request.green_mana_bonus,
                "mana_reward",
                request.now,
            )?;
            receipt(
                tx,
                &market.market_id,
                request.guild_id,
                Some(request.discord_id),
                &format!("mana_bonus:{wager_id}"),
                request.green_mana_bonus,
                "{}",
                request.now,
            )?;
        }
    }
    Ok(DeadlockWager {
        wager_id,
        market_id: market.market_id.clone(),
        discord_id: request.discord_id,
        side: request.side,
        amount: request.amount,
        leverage: request.leverage,
        effective_stake: stake,
        kind: kind.to_owned(),
        created_at: request.now,
        payout: None,
    })
}
fn wallet_delta(
    tx: &Transaction<'_>,
    market: &str,
    guild: i64,
    id: i64,
    delta: i64,
    source: &str,
    _now: i64,
) -> Result<()> {
    let next = balance(tx, guild, id)?
        .checked_add(delta)
        .ok_or_else(|| invalid("Wallet overflow"))?;
    tx.execute("INSERT OR REPLACE INTO economy_ledger_context(id,source,related_type,related_id,reason) VALUES(1,?1,'deadlock_market',?2,?1)",params![source,market])?;
    tx.execute("UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP WHERE guild_id=?2 AND discord_id=?3",params![next,guild,id])?;
    tx.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
    Ok(())
}
fn fund_credit(tx: &Transaction<'_>, guild: i64, amount: i64) -> Result<()> {
    if amount <= 0 {
        return Ok(());
    }
    let current: i64 = tx
        .query_row(
            "SELECT balance FROM deadlock_betting_funds WHERE guild_id=?1",
            [guild],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let next = current
        .checked_add(amount)
        .ok_or_else(|| invalid("Seed fund overflow"))?;
    tx.execute("INSERT INTO deadlock_betting_funds(guild_id,balance) VALUES(?1,?2) ON CONFLICT(guild_id) DO UPDATE SET balance=excluded.balance",params![guild,next])?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn receipt(
    tx: &Transaction<'_>,
    market: &str,
    guild: i64,
    id: Option<i64>,
    kind: &str,
    amount: i64,
    detail: &str,
    now: i64,
) -> Result<()> {
    let key = id.map_or_else(
        || format!("{market}:{kind}"),
        |id| format!("{market}:{kind}:{id}"),
    );
    tx.execute("INSERT INTO deadlock_economic_receipts(receipt_id,market_id,guild_id,discord_id,kind,amount,detail_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![key,market,guild,id,kind,amount,detail,now])?;
    Ok(())
}
fn settlement_receipt(
    connection: &Connection,
    market: &str,
    kind: &str,
) -> Result<Option<DeadlockSettlement>> {
    connection
        .query_row(
            "SELECT detail_json FROM deadlock_economic_receipts WHERE receipt_id=?1",
            [format!("{market}:{kind}")],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}
fn read_investments(connection: &Connection, guild: i64) -> Result<Vec<(i64, i64, String, i64)>> {
    let mut statement=connection.prepare("SELECT investor_id,target_id,direction,percentage FROM deadlock_betting_investments WHERE guild_id=?1 ORDER BY investor_id,target_id")?;
    Ok(statement
        .query_map([guild], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<std::result::Result<_, _>>()?)
}
fn read_liquidity(connection: &Connection, guild: i64) -> Result<Vec<(i64, i64)>> {
    let mut statement=connection.prepare("SELECT discord_id,percentage FROM deadlock_betting_liquidity WHERE guild_id=?1 ORDER BY discord_id")?;
    Ok(statement
        .query_map([guild], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FastTestDatabase, fast_migrated_database};

    fn fixture() -> (FastTestDatabase, DeadlockBettingRepository) {
        let db = fast_migrated_database();
        let repo = DeadlockBettingRepository::new(db.path());
        let c = repo.connection().unwrap();
        for guild in [1, 2] {
            for id in 1..=12 {
                c.execute("INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(?1,?2,'Player',100)",params![id,guild]).unwrap();
            }
        }
        let team1 = (1..=4)
            .map(|id| serde_json::json!({"discord_id":id}))
            .collect::<Vec<_>>();
        let team2 = (5..=8)
            .map(|id| serde_json::json!({"discord_id":id}))
            .collect::<Vec<_>>();
        c.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at) VALUES(1,1,'street_brawl','economic_setup',?1,1,0,0)",[serde_json::json!({"team1":team1,"team2":team2}).to_string()]).unwrap();
        (db, repo)
    }
    fn terms() -> DeadlockMarketTerms {
        DeadlockMarketTerms {
            blind_percentage: 0,
            ..Default::default()
        }
    }
    fn bet(id: i64, side: i64, amount: i64, key: &str) -> DeadlockBetRequest {
        DeadlockBetRequest {
            guild_id: 1,
            match_id: 1,
            discord_id: id,
            side,
            amount,
            leverage: 1,
            allow_10x: false,
            green_mana_bonus: 0,
            request_key: key.into(),
            now: 10,
        }
    }
    fn record(repo: &DeadlockBettingRepository, winner: i64) {
        repo.connection()
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET status='recorded',winner=?1 WHERE match_id=1",
                [winner],
            )
            .unwrap();
    }
    #[test]
    fn setup_is_atomic_idempotent_and_never_touches_dota_tables() {
        let (_db, repo) = fixture();
        let c = repo.connection().unwrap();
        let market = repo
            .open_market(1, 1, &DeadlockMarketTerms::default(), 0)
            .unwrap();
        assert_eq!((market.pool1, market.pool2), (40, 40));
        assert_eq!(repo.open_market(1, 1, &terms(), 30).unwrap().deadline, 180);
        assert_eq!(balance(&c, 1, 1).unwrap(), 90);
        assert_eq!(balance(&c, 2, 1).unwrap(), 100);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM bets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM deadlock_wagers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            8
        );
        assert_eq!(c.query_row("SELECT COUNT(*) FROM economy_ledger_entries WHERE related_type='deadlock_market' AND related_id='deadlock:1'",[],|r|r.get::<_,i64>(0)).unwrap(),8);
    }
    #[test]
    fn unfunded_setup_fails_without_partial_debits_or_open_market() {
        let (_db, repo) = fixture();
        let configured = DeadlockMarketTerms {
            seed: 50,
            ..Default::default()
        };
        assert!(repo.open_market(1, 1, &configured, 0).is_err());
        assert!(repo.market(1, 1).unwrap().is_none());
        assert_eq!(balance(&repo.connection().unwrap(), 1, 1).unwrap(), 100);
        repo.contribute_seed(1, 9, 50, "contribution", 0).unwrap();
        repo.contribute_seed(1, 9, 50, "contribution", 0).unwrap();
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 50);
        assert!(repo.contribute_seed(1, 9, 51, "contribution", 0).is_err());
        assert_eq!(repo.open_market(1, 1, &configured, 0).unwrap().seed, 50);
    }
    #[test]
    fn wager_identity_guild_roster_and_close_are_enforced() {
        let (_db, repo) = fixture();
        repo.open_market(1, 1, &terms(), 0).unwrap();
        assert!(repo.place_bet(&bet(1, 2, 10, "wrong-side")).is_err());
        let first = repo.place_bet(&bet(1, 1, 10, "one")).unwrap();
        assert_eq!(repo.place_bet(&bet(1, 1, 10, "one")).unwrap(), first);
        assert!(repo.place_bet(&bet(1, 1, 11, "one")).is_err());
        let mut foreign = bet(1, 1, 10, "foreign");
        foreign.guild_id = 2;
        assert!(repo.place_bet(&foreign).is_err());
        repo.close_market(1, 1, 11).unwrap();
        assert!(repo.place_bet(&bet(9, 1, 10, "late")).is_err());
        assert_eq!(repo.place_bet(&bet(1, 1, 10, "one")).unwrap(), first);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 1).unwrap(), 90);
    }
    #[test]
    fn aborted_match_cannot_close_betting_for_a_stale_launch_request() {
        let (_db, repo) = fixture();
        repo.open_market(1, 1, &terms(), 0).unwrap();
        repo.connection()
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET status='aborted' WHERE match_id=1",
                [],
            )
            .unwrap();
        assert!(repo.close_market(1, 1, 20).is_err());
        assert!(repo.place_bet(&bet(9, 1, 10, "after-abort")).is_err());
        repo.refund_aborted(1, 1, 30).unwrap();
    }
    #[test]
    fn green_mana_credit_and_manual_bet_counter_replay_exactly_once() {
        let (_db, repo) = fixture();
        repo.open_market(1, 1, &terms(), 0).unwrap();
        let mut request = bet(9, 1, 10, "green");
        request.green_mana_bonus = 2;
        repo.place_bet(&request).unwrap();
        repo.place_bet(&request).unwrap();
        let connection = repo.connection().unwrap();
        assert_eq!(balance(&connection, 1, 9).unwrap(), 92);
        assert_eq!(
            connection
                .query_row(
                    "SELECT total_bets_placed FROM players WHERE guild_id=1 AND discord_id=9",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM economy_ledger_entries WHERE related_id='deadlock:1' AND source='mana_reward'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
    #[test]
    fn leverage_obeys_debt_and_mana_policy_and_refunds_full_stake_once() {
        let (_db, repo) = fixture();
        repo.open_market(1, 1, &terms(), 0).unwrap();
        let mut leveraged = bet(9, 1, 100, "leveraged");
        leveraged.leverage = 10;
        assert!(repo.place_bet(&leveraged).is_err());
        leveraged.allow_10x = true;
        assert!(repo.place_bet(&leveraged).is_err());
        leveraged.amount = 50;
        assert_eq!(repo.place_bet(&leveraged).unwrap().effective_stake, 500);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), -400);
        assert!(repo.place_bet(&bet(9, 1, 1, "in-debt")).is_err());
        assert!(repo.refund_aborted(1, 1, 30).is_err());
        repo.connection()
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET status='aborted' WHERE match_id=1",
                [],
            )
            .unwrap();
        let refund = repo.refund_aborted(1, 1, 30).unwrap();
        assert_eq!(refund.refunds.get(&9), Some(&500));
        assert_eq!(repo.refund_aborted(1, 1, 31).unwrap(), refund);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 100);
        assert!(repo.settle_recorded(1, 1, 32).is_err());
    }
    #[test]
    fn recovery_settles_once_with_shared_pool_rounding_and_frozen_deductions() {
        let (_db, repo) = fixture();
        let configured = DeadlockMarketTerms {
            vanity_tax_rate: 0.2,
            vanity_taxable_ids: BTreeSet::from([9]),
            ..terms()
        };
        repo.open_market(1, 1, &configured, 0).unwrap();
        repo.place_bet(&bet(9, 1, 10, "win1")).unwrap();
        repo.place_bet(&bet(9, 1, 10, "win2")).unwrap();
        repo.place_bet(&bet(10, 2, 80, "lose")).unwrap();
        assert!(repo.settle_recorded(1, 1, 20).is_err());
        record(&repo, 1);
        let recovered = repo.recover(30).unwrap();
        let result = recovered[0].2.as_ref().unwrap();
        assert_eq!(result.gross_payouts.get(&9), Some(&100));
        assert_eq!(result.deductions.get(&9), Some(&16));
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 164);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 10).unwrap(), 20);
        assert_eq!(&repo.settle_recorded(1, 1, 40).unwrap(), result);
        assert!(repo.recover(50).unwrap().is_empty());
        assert!(repo.refund_aborted(1, 1, 50).is_err());
    }
    #[test]
    fn auto_preferences_are_opt_in_investments_follow_blinds_and_short_own_team_is_skipped() {
        let (_db, repo) = fixture();
        repo.set_investment(1, 9, 1, "long", 10).unwrap();
        repo.set_investment(1, 1, 1, "short", 10).unwrap();
        repo.set_investment(1, 1, 2, "long", 10).unwrap();
        repo.set_liquidity(1, 10, 10).unwrap();
        repo.open_market(1, 1, &DeadlockMarketTerms::default(), 0)
            .unwrap();
        let c = repo.connection().unwrap();
        assert_eq!(balance(&c, 1, 1).unwrap(), 81);
        assert_eq!(balance(&c, 1, 9).unwrap(), 90);
        assert_eq!(balance(&c, 1, 10).unwrap(), 90);
        let bets = repo.wagers(1, 1).unwrap();
        assert!(
            bets.iter()
                .any(|b| b.discord_id == 1 && b.kind == "investment" && b.amount == 9)
        );
        assert!(
            bets.iter()
                .any(|b| b.discord_id == 10 && b.kind == "liquidity" && b.side == 2)
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM autobet_investments", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn investment_cap_is_shared_and_wallet_changes_are_visible_to_both_games() {
        let (_db, repo) = fixture();
        let dota = crate::autobet_investments::AutobetInvestmentRepository::new(&repo.path);
        for target in 1..=4 {
            dota.set(Some(1), 9, target, "long", 10).unwrap();
        }
        repo.set_investment(1, 9, 5, "long", 10).unwrap();
        assert!(repo.set_investment(1, 9, 6, "long", 1).is_err());
        assert!(dota.set(Some(1), 9, 6, "long", 1).is_err());
        repo.set_investment(1, 9, 5, "long", 0).unwrap();
        assert_eq!(dota.set(Some(1), 9, 6, "long", 10).unwrap(), 50);
        repo.open_market(1, 1, &terms(), 0).unwrap();
        repo.place_bet(&bet(9, 1, 80, "deadlock-stake")).unwrap();
        // A Dota wallet mutation observes the same balance; a second Deadlock
        // stake cannot spend the original pre-Dota amount.
        repo.connection().unwrap().execute("UPDATE players SET jopacoin_balance=jopacoin_balance-15 WHERE guild_id=1 AND discord_id=9",[]).unwrap();
        assert!(repo.place_bet(&bet(9, 1, 6, "overspend")).is_err());
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 5);
    }
    #[test]
    fn no_winning_stakes_preserves_existing_sink_policy() {
        let (_db, repo) = fixture();
        repo.open_market(1, 1, &terms(), 0).unwrap();
        repo.place_bet(&bet(9, 2, 25, "lose")).unwrap();
        record(&repo, 1);
        repo.settle_recorded(1, 1, 30).unwrap();
        assert_eq!(repo.seed_balance(1).unwrap(), 0);
        assert_eq!(repo.connection().unwrap().query_row("SELECT amount FROM deadlock_economic_receipts WHERE receipt_id='deadlock:1:unclaimed_pool'",[],|r|r.get::<_,i64>(0)).unwrap(),25);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 75);
    }
    #[test]
    fn duplicate_concurrent_wagers_and_settlements_cannot_double_spend_or_pay() {
        let (_db, source) = fixture();
        source.open_market(1, 1, &terms(), 0).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        source
            .connection()
            .unwrap()
            .backup(rusqlite::MAIN_DB, file.path(), None)
            .unwrap();
        let repo = DeadlockBettingRepository::new(file.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let left = repo.clone();
            let right = repo.clone();
            let b = barrier.clone();
            let one = scope.spawn(move || {
                b.wait();
                left.place_bet(&bet(9, 1, 25, "same-interaction")).unwrap()
            });
            let two = scope.spawn(move || {
                barrier.wait();
                right.place_bet(&bet(9, 1, 25, "same-interaction")).unwrap()
            });
            assert_eq!(one.join().unwrap(), two.join().unwrap());
        });
        repo.place_bet(&bet(10, 2, 25, "loser")).unwrap();
        record(&repo, 1);
        std::thread::scope(|scope| {
            let left = repo.clone();
            let right = repo.clone();
            let one = scope.spawn(move || left.settle_recorded(1, 1, 30).unwrap());
            let two = scope.spawn(move || right.settle_recorded(1, 1, 30).unwrap());
            assert_eq!(one.join().unwrap(), two.join().unwrap());
        });
        assert_eq!(balance(&repo.connection().unwrap(), 1, 9).unwrap(), 125);
        assert_eq!(balance(&repo.connection().unwrap(), 1, 10).unwrap(), 75);
        assert_eq!(repo.wagers(1, 1).unwrap().len(), 2);
    }
    #[test]
    fn recovery_honors_shuffle_terms_and_blood_pact_jobs_have_durable_acknowledgements() {
        let (_db, repo) = fixture();
        let frozen = DeadlockMarketTerms {
            betting_window_seconds: 90,
            blind_percentage: 0,
            ..Default::default()
        };
        repo.connection()
            .unwrap()
            .execute(
                "UPDATE deadlock_matches SET economy_terms_json=?1",
                [serde_json::to_string(&frozen).unwrap()],
            )
            .unwrap();
        assert_eq!(repo.recover(10).unwrap().len(), 1);
        assert_eq!(repo.market(1, 1).unwrap().unwrap().deadline, 100);
        repo.place_bet(&bet(9, 1, 25, "winner")).unwrap();
        repo.place_bet(&bet(10, 2, 25, "loser")).unwrap();
        record(&repo, 1);
        repo.settle_recorded(1, 1, 30).unwrap();
        let jobs = repo.pending_blood_pacts().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].earning, 25);
        assert_eq!(jobs[0].event_key, "deadlock-bet-blood-pact:1:9");
        repo.complete_blood_pact(&jobs[0], 5).unwrap();
        repo.complete_blood_pact(&jobs[0], 5).unwrap();
        assert!(repo.complete_blood_pact(&jobs[0], 6).is_err());
        assert!(repo.pending_blood_pacts().unwrap().is_empty());
    }
    #[test]
    fn failed_late_setup_rolls_back_seed_blinds_and_investments() {
        let (_db, repo) = fixture();
        repo.set_investment(1, 9, 1, "long", 10).unwrap();
        repo.connection()
            .unwrap()
            .execute("DELETE FROM players WHERE guild_id=1 AND discord_id=9", [])
            .unwrap();
        assert!(
            repo.open_market(1, 1, &DeadlockMarketTerms::default(), 0)
                .is_err()
        );
        assert!(repo.market(1, 1).unwrap().is_none());
        assert_eq!(balance(&repo.connection().unwrap(), 1, 1).unwrap(), 100);
        assert!(repo.wagers(1, 1).is_err());
    }
}
