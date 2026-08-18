//! Frozen financial intent for durable Immortal Draft finalization.
//!
//! The initial pending-link writer snapshots every policy and database input
//! needed by Python's automatic-match liquidity sequence.  A later setup
//! transaction consumes these exact intents; it never re-ranks players or
//! re-sizes a wager from mutable configuration.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Schema nested inside Draft finalization plan v2.
pub const DRAFT_FINANCIAL_SETUP_PLAN_VERSION: u64 = 1;

/// Frozen configuration supplied by the live process before the atomic link.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftFinancialSetupPolicy {
    pub schema_version: u64,
    pub game_date: String,
    pub betting_mode: String,
    pub seed_max_amount: i64,
    pub first_game_daily_amount: i64,
    pub auto_blind_enabled: bool,
    pub normal_blind_threshold: i64,
    /// Exact IEEE-754 bits for Python's multiplier, encoded as 16 lowercase hex digits.
    pub normal_blind_percentage_bits: String,
    pub bomb_blind_percentage_bits: String,
    pub bomb_ante: i64,
    pub max_debt: i64,
    pub spectator_enabled: bool,
    pub spectator_total_count: u64,
    pub spectator_top_count: u64,
    pub spectator_base_percentage_bits: String,
    pub spectator_top_percentage_bits: String,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl DraftFinancialSetupPolicy {
    pub fn validate(&self) -> Result<(), DraftFinancialSetupError> {
        if self.schema_version != DRAFT_FINANCIAL_SETUP_PLAN_VERSION {
            return Err(DraftFinancialSetupError::InvalidPolicy(format!(
                "unsupported financial policy version {}",
                self.schema_version
            )));
        }
        CivilDate::parse(&self.game_date)?;
        if !matches!(self.betting_mode.as_str(), "pool" | "house") {
            return Err(DraftFinancialSetupError::InvalidPolicy(
                "betting_mode must be pool or house".to_owned(),
            ));
        }
        if self.seed_max_amount < 0
            || self.first_game_daily_amount < 0
            || self.normal_blind_threshold < 0
            || self.bomb_ante < 0
            || self.max_debt < 0
        {
            return Err(DraftFinancialSetupError::InvalidPolicy(
                "financial amounts and thresholds must be nonnegative".to_owned(),
            ));
        }
        for (name, bits) in [
            ("normal blind", &self.normal_blind_percentage_bits),
            ("bomb blind", &self.bomb_blind_percentage_bits),
            ("spectator base", &self.spectator_base_percentage_bits),
            ("spectator top", &self.spectator_top_percentage_bits),
        ] {
            let value = percentage_from_bits(bits)?;
            if !value.is_finite() || value < 0.0 {
                return Err(DraftFinancialSetupError::InvalidPolicy(format!(
                    "{name} percentage must be finite and nonnegative"
                )));
            }
        }
        if self.spectator_top_count > self.spectator_total_count {
            return Err(DraftFinancialSetupError::InvalidPolicy(
                "spectator top count exceeds total count".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftFinancialEffectKind {
    Seed,
    Blind,
    Investment,
    Spectator,
}

impl DraftFinancialEffectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Blind => "blind",
            Self::Investment => "investment",
            Self::Spectator => "spectator",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftFinancialIntentStatus {
    Applied,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftFinancialEffectIntent {
    pub effect_key: String,
    pub effect_kind: DraftFinancialEffectKind,
    pub ordinal: u64,
    pub intended_status: DraftFinancialIntentStatus,
    pub intended: Value,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftInvestmentSnapshot {
    pub investor_id: i64,
    pub target_id: i64,
    pub direction: String,
    pub percentage: i64,
    pub starting_balance: i64,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftWealthSnapshot {
    pub source_ordinal: u64,
    pub discord_id: i64,
    pub balance: i64,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Complete immutable financial plan stored inside Draft finalization v2.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftFinancialSetupPlan {
    pub schema_version: u64,
    pub pending_match_id: i64,
    pub policy: DraftFinancialSetupPolicy,
    pub radiant_player_ids: Vec<i64>,
    pub dire_player_ids: Vec<i64>,
    pub investment_positions: Vec<DraftInvestmentSnapshot>,
    /// Positive-wallet rows from the initial writer snapshot, before the
    /// frozen blind and investment intents are simulated. Spectator intents
    /// carry the corresponding post-simulation balance and rank.
    pub wealth_snapshot: Vec<DraftWealthSnapshot>,
    pub effects: Vec<DraftFinancialEffectIntent>,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl DraftFinancialSetupPlan {
    pub fn validate(&self) -> Result<(), DraftFinancialSetupError> {
        if self.schema_version != DRAFT_FINANCIAL_SETUP_PLAN_VERSION {
            return Err(DraftFinancialSetupError::InvalidPlan(format!(
                "unsupported financial setup version {}",
                self.schema_version
            )));
        }
        if self.pending_match_id <= 0 {
            return Err(DraftFinancialSetupError::InvalidPlan(
                "pending_match_id must be positive".to_owned(),
            ));
        }
        self.policy.validate()?;
        let mut participant_ids = BTreeSet::new();
        for discord_id in self.radiant_player_ids.iter().chain(&self.dire_player_ids) {
            if *discord_id <= 0 || !participant_ids.insert(*discord_id) {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial setup participants must be positive and unique".to_owned(),
                ));
            }
        }
        for position in &self.investment_positions {
            if position.investor_id <= 0
                || position.target_id <= 0
                || !matches!(position.direction.as_str(), "long" | "short")
                || !(1..=10).contains(&position.percentage)
            {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial investment snapshot is invalid".to_owned(),
                ));
            }
        }
        let mut wealth_ids = BTreeSet::new();
        for (expected_ordinal, snapshot) in self.wealth_snapshot.iter().enumerate() {
            if snapshot.source_ordinal != expected_ordinal as u64
                || snapshot.discord_id <= 0
                || snapshot.balance < 1
                || !wealth_ids.insert(snapshot.discord_id)
            {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial wealth snapshot identity/order is invalid".to_owned(),
                ));
            }
        }
        let mut keys = BTreeSet::new();
        let mut identity = None;
        let mut previous_kind = 0_u8;
        for (expected_ordinal, effect) in self.effects.iter().enumerate() {
            if effect.ordinal != expected_ordinal as u64 {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial effect ordinals are not contiguous".to_owned(),
                ));
            }
            if !keys.insert(effect.effect_key.as_str()) {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial effect keys are not unique".to_owned(),
                ));
            }
            if !effect.intended.is_object() {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial intended payload must be an object".to_owned(),
                ));
            }
            let kind_order = match effect.effect_kind {
                DraftFinancialEffectKind::Seed => 0,
                DraftFinancialEffectKind::Blind => 1,
                DraftFinancialEffectKind::Investment => 2,
                DraftFinancialEffectKind::Spectator => 3,
            };
            if kind_order < previous_kind {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial effects are not in seed/blind/investment/spectator order".to_owned(),
                ));
            }
            previous_kind = kind_order;
            let effect_identity = effect_identity(effect)?;
            if effect_identity.2 != self.pending_match_id {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial effect pending-match identity does not match its plan".to_owned(),
                ));
            }
            if identity
                .replace(effect_identity)
                .is_some_and(|current| current != effect_identity)
            {
                return Err(DraftFinancialSetupError::InvalidPlan(
                    "financial effects do not share one Draft identity".to_owned(),
                ));
            }
            validate_effect_key(effect, effect_identity)?;
        }
        if self
            .effects
            .first()
            .is_none_or(|effect| effect.effect_kind != DraftFinancialEffectKind::Seed)
        {
            return Err(DraftFinancialSetupError::InvalidPlan(
                "financial setup must begin with its seed intent".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn validate_for_identity(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<(), DraftFinancialSetupError> {
        self.validate()?;
        if self.pending_match_id != pending_match_id {
            return Err(DraftFinancialSetupError::InvalidPlan(
                "financial setup pending_match_id does not match its finalization job".to_owned(),
            ));
        }
        let expected = (guild_id, session_id, pending_match_id);
        if self
            .effects
            .iter()
            .map(effect_identity)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .any(|identity| identity != expected)
        {
            return Err(DraftFinancialSetupError::InvalidPlan(
                "financial effect identity does not match its finalization job".to_owned(),
            ));
        }
        Ok(())
    }
}

fn effect_identity(
    effect: &DraftFinancialEffectIntent,
) -> Result<(i64, u64, i64), DraftFinancialSetupError> {
    let object = effect.intended.as_object().ok_or_else(|| {
        DraftFinancialSetupError::InvalidPlan(
            "financial intended payload must be an object".to_owned(),
        )
    })?;
    let guild_id = positive_i64(object, "guild_id")?;
    let session_id = object
        .get("session_id")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            DraftFinancialSetupError::InvalidPlan(
                "financial effect session_id must be positive".to_owned(),
            )
        })?;
    let pending_match_id = positive_i64(object, "pending_match_id")?;
    Ok((guild_id, session_id, pending_match_id))
}

fn validate_effect_key(
    effect: &DraftFinancialEffectIntent,
    (guild_id, session_id, pending_match_id): (i64, u64, i64),
) -> Result<(), DraftFinancialSetupError> {
    let object = effect
        .intended
        .as_object()
        .expect("effect identity requires object");
    let base = format!("draft:{guild_id}:{session_id}");
    let expected = match effect.effect_kind {
        DraftFinancialEffectKind::Seed => {
            format!("{base}:seed:{pending_match_id}")
        }
        DraftFinancialEffectKind::Blind => {
            let discord_id = positive_i64(object, "discord_id")?;
            format!("{base}:blind:{pending_match_id}:{discord_id}")
        }
        DraftFinancialEffectKind::Investment => {
            let investor_id = positive_i64(object, "discord_id")?;
            let target_id = positive_i64(object, "target_id")?;
            let direction = object
                .get("direction")
                .and_then(Value::as_str)
                .filter(|direction| matches!(*direction, "long" | "short"))
                .ok_or_else(|| {
                    DraftFinancialSetupError::InvalidPlan(
                        "financial investment effect direction is invalid".to_owned(),
                    )
                })?;
            format!("{base}:investment:{pending_match_id}:{investor_id}:{target_id}:{direction}")
        }
        DraftFinancialEffectKind::Spectator => {
            let discord_id = positive_i64(object, "discord_id")?;
            format!("{base}:spectator:{pending_match_id}:{discord_id}")
        }
    };
    if effect.effect_key != expected {
        return Err(DraftFinancialSetupError::InvalidPlan(format!(
            "financial effect key {} does not match {expected}",
            effect.effect_key
        )));
    }
    Ok(())
}

fn positive_i64(object: &Map<String, Value>, field: &str) -> Result<i64, DraftFinancialSetupError> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            DraftFinancialSetupError::InvalidPlan(format!(
                "financial effect {field} must be positive"
            ))
        })
}

#[derive(Debug, Error)]
pub enum DraftFinancialSetupError {
    #[error("Draft financial setup SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Draft financial policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("Draft financial plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("Draft pending payload is invalid: {0}")]
    InvalidPending(String),
    #[error("Draft financial arithmetic overflowed")]
    Overflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Team {
    Radiant,
    Dire,
}

impl Team {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::Radiant => Self::Dire,
            Self::Dire => Self::Radiant,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Totals {
    radiant: i64,
    dire: i64,
}

impl Totals {
    fn total(self) -> i64 {
        self.radiant.saturating_add(self.dire)
    }

    fn side(self, team: Team) -> i64 {
        match team {
            Team::Radiant => self.radiant,
            Team::Dire => self.dire,
        }
    }

    fn add(&mut self, team: Team, amount: i64) {
        match team {
            Team::Radiant => self.radiant = self.radiant.saturating_add(amount),
            Team::Dire => self.dire = self.dire.saturating_add(amount),
        }
    }
}

struct Planner<'transaction, 'connection> {
    transaction: &'transaction Transaction<'connection>,
    completion_key: &'transaction str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    shuffle_timestamp: i64,
    is_bomb_pot: bool,
    lobby_kind: String,
    policy: DraftFinancialSetupPolicy,
    radiant: Vec<i64>,
    dire: Vec<i64>,
    investments: Vec<DraftInvestmentSnapshot>,
    wealth: Vec<DraftWealthSnapshot>,
    balances: BTreeMap<i64, i64>,
    totals: Totals,
    effects: Vec<DraftFinancialEffectIntent>,
}

/// Freeze all Draft financial intents while the pending-link transaction owns
/// SQLite's writer lock.
#[allow(clippy::too_many_arguments)]
pub(crate) fn freeze_financial_setup_plan(
    transaction: &Transaction<'_>,
    completion_key: &str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    pending_payload_json: &str,
    policy: DraftFinancialSetupPolicy,
) -> Result<DraftFinancialSetupPlan, DraftFinancialSetupError> {
    policy.validate()?;
    let pending: Value = serde_json::from_str(pending_payload_json)
        .map_err(|error| DraftFinancialSetupError::InvalidPending(error.to_string()))?;
    let pending = pending.as_object().ok_or_else(|| {
        DraftFinancialSetupError::InvalidPending("root value must be an object".to_owned())
    })?;
    let radiant = id_array(pending, "radiant_team_ids")?;
    let dire = id_array(pending, "dire_team_ids")?;
    let shuffle_timestamp = pending
        .get("shuffle_timestamp")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            DraftFinancialSetupError::InvalidPending(
                "shuffle_timestamp must be an integer".to_owned(),
            )
        })?;
    let is_bomb_pot = pending
        .get("is_bomb_pot")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let betting_mode = pending
        .get("betting_mode")
        .and_then(Value::as_str)
        .unwrap_or("pool");
    if betting_mode != policy.betting_mode {
        return Err(DraftFinancialSetupError::InvalidPending(
            "betting_mode does not match the frozen policy".to_owned(),
        ));
    }
    let lobby_kind = pending
        .get("lobby_kind")
        .and_then(Value::as_str)
        .unwrap_or("open")
        .to_owned();
    if !matches!(lobby_kind.as_str(), "open" | "lowskill") {
        return Err(DraftFinancialSetupError::InvalidPending(
            "lobby_kind must be open or lowskill".to_owned(),
        ));
    }

    let participant_ids = radiant
        .iter()
        .chain(&dire)
        .copied()
        .collect::<BTreeSet<_>>();
    if participant_ids.len() != radiant.len().saturating_add(dire.len()) {
        return Err(DraftFinancialSetupError::InvalidPending(
            "Draft participants must be unique".to_owned(),
        ));
    }
    let investments = investment_snapshot(transaction, guild_id, &participant_ids)?;
    let wealth = wealth_snapshot(transaction, guild_id)?;
    let mut required_ids = participant_ids.clone();
    required_ids.extend(investments.iter().map(|position| position.investor_id));
    let balances = balances_for(transaction, guild_id, &required_ids)?;
    let totals = pending_totals(transaction, guild_id, pending_match_id, shuffle_timestamp)?;
    let mut planner = Planner {
        transaction,
        completion_key,
        guild_id,
        session_id,
        pending_match_id,
        shuffle_timestamp,
        is_bomb_pot,
        lobby_kind,
        policy: policy.clone(),
        radiant: radiant.clone(),
        dire: dire.clone(),
        investments: investments.clone(),
        wealth: wealth.clone(),
        balances,
        totals,
        effects: Vec::new(),
    };
    planner.plan_seed()?;
    planner.plan_blinds()?;
    planner.plan_investments()?;
    planner.plan_spectators()?;
    let plan = DraftFinancialSetupPlan {
        schema_version: DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
        pending_match_id,
        policy,
        radiant_player_ids: radiant,
        dire_player_ids: dire,
        investment_positions: investments,
        wealth_snapshot: wealth,
        effects: planner.effects,
        extensions: BTreeMap::new(),
    };
    plan.validate()?;
    Ok(plan)
}

/// Expand a schema-v2 top-level plan seed into its complete frozen plan while
/// preserving every unknown top-level field semantically.
#[allow(clippy::too_many_arguments)]
pub(crate) fn freeze_finalization_plan_json(
    transaction: &Transaction<'_>,
    completion_key: &str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    pending_payload_json: &str,
    plan_seed_json: &str,
) -> Result<String, DraftFinancialSetupError> {
    let mut seed: Value = serde_json::from_str(plan_seed_json)
        .map_err(|error| DraftFinancialSetupError::InvalidPlan(error.to_string()))?;
    let object = seed.as_object_mut().ok_or_else(|| {
        DraftFinancialSetupError::InvalidPlan("plan seed must be an object".to_owned())
    })?;
    let policy: DraftFinancialSetupPolicy =
        serde_json::from_value(object.get("financial_setup").cloned().ok_or_else(|| {
            DraftFinancialSetupError::InvalidPlan(
                "plan seed is missing financial_setup policy".to_owned(),
            )
        })?)
        .map_err(|error| DraftFinancialSetupError::InvalidPolicy(error.to_string()))?;
    let plan = freeze_financial_setup_plan(
        transaction,
        completion_key,
        guild_id,
        session_id,
        pending_match_id,
        pending_payload_json,
        policy,
    )?;
    object.insert(
        "financial_setup".to_owned(),
        serde_json::to_value(plan)
            .map_err(|error| DraftFinancialSetupError::InvalidPlan(error.to_string()))?,
    );
    serde_json::to_string(&seed)
        .map_err(|error| DraftFinancialSetupError::InvalidPlan(error.to_string()))
}

impl Planner<'_, '_> {
    fn push(
        &mut self,
        effect_key: String,
        effect_kind: DraftFinancialEffectKind,
        status: DraftFinancialIntentStatus,
        intended: Value,
    ) {
        self.effects.push(DraftFinancialEffectIntent {
            effect_key,
            effect_kind,
            ordinal: self.effects.len() as u64,
            intended_status: status,
            intended,
            extensions: BTreeMap::new(),
        });
    }

    fn plan_seed(&mut self) -> Result<(), DraftFinancialSetupError> {
        let fund = fund_snapshot(self.transaction, self.guild_id)?;
        let pool_mode = self.policy.betting_mode == "pool";
        let funding = if pool_mode {
            simulate_daily_funding(
                self.transaction,
                self.guild_id,
                &self.policy.game_date,
                self.policy.first_game_daily_amount,
                fund,
            )?
        } else {
            FundingSimulation {
                after: fund,
                steps: Vec::new(),
            }
        };
        let date_claimed = if pool_mode {
            self.transaction
                .query_row(
                    "SELECT 1 FROM first_game_pool_claims
                     WHERE guild_id=?1 AND lobby_kind=?2 AND game_date=?3",
                    params![self.guild_id, self.lobby_kind, self.policy.game_date],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
        } else {
            false
        };
        let first_game_reserved = if self.policy.betting_mode == "pool"
            && self.policy.first_game_daily_amount > 0
            && !date_claimed
        {
            match self.lobby_kind.as_str() {
                "open" => funding.after.open,
                "lowskill" => funding.after.low_skill,
                _ => 0,
            }
            .max(0)
        } else {
            0
        };
        let regular_reserved = if funding.after.queued > 0 {
            funding.after.queued
        } else {
            self.policy.seed_max_amount.min(funding.after.total).max(0)
        };
        let reserved = regular_reserved
            .checked_add(first_game_reserved)
            .ok_or(DraftFinancialSetupError::Overflow)?;
        let (radiant, dire, bonus) = if self.policy.betting_mode == "pool" {
            (reserved.saturating_add(1) / 2, reserved / 2, 0)
        } else {
            (0, 0, reserved)
        };
        self.push(
            format!("{}:seed:{}", self.completion_key, self.pending_match_id),
            DraftFinancialEffectKind::Seed,
            DraftFinancialIntentStatus::Applied,
            json!({
                "guild_id": self.guild_id,
                "session_id": self.session_id,
                "pending_match_id": self.pending_match_id,
                "game_date": self.policy.game_date,
                "lobby_kind": self.lobby_kind,
                "betting_mode": self.policy.betting_mode,
                "seed_max_amount": self.policy.seed_max_amount,
                "first_game_daily_amount": self.policy.first_game_daily_amount,
                "expected_fund_before": fund.to_json(),
                "funding_steps": funding.steps,
                "expected_date_claimed": date_claimed,
                "regular_reserved": regular_reserved,
                "first_game_reserved": first_game_reserved,
                "reserved": reserved,
                "radiant": radiant,
                "dire": dire,
                "bonus": bonus,
            }),
        );
        Ok(())
    }

    fn plan_blinds(&mut self) -> Result<(), DraftFinancialSetupError> {
        if !self.policy.auto_blind_enabled {
            return Ok(());
        }
        let candidates = self
            .radiant
            .iter()
            .copied()
            .map(|id| (id, Team::Radiant))
            .chain(self.dire.iter().copied().map(|id| (id, Team::Dire)))
            .collect::<Vec<_>>();
        for (discord_id, team) in candidates {
            let balance = self.balances.get(&discord_id).copied().unwrap_or_default();
            let mut status = DraftFinancialIntentStatus::Applied;
            let mut reason = None;
            let amount = if self.is_bomb_pot {
                let percentage = if balance > 0 {
                    python_round_product(balance, &self.policy.bomb_blind_percentage_bits)?
                } else {
                    0
                };
                percentage
                    .checked_add(self.policy.bomb_ante)
                    .ok_or(DraftFinancialSetupError::Overflow)?
                    .max(self.policy.bomb_ante)
            } else if balance < self.policy.normal_blind_threshold {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some(format!(
                    "balance {balance} < threshold {}",
                    self.policy.normal_blind_threshold
                ));
                0
            } else {
                let amount =
                    python_round_product(balance, &self.policy.normal_blind_percentage_bits)?;
                if amount < 1 {
                    status = DraftFinancialIntentStatus::Skipped;
                    reason = Some(format!("blind amount {amount} < 1"));
                }
                amount
            };
            let before = balance;
            let mut after = before;
            let mut planned_odds_bits = None;
            if status == DraftFinancialIntentStatus::Applied {
                let candidate_after = before
                    .checked_sub(amount)
                    .ok_or(DraftFinancialSetupError::Overflow)?;
                let valid = if self.is_bomb_pot {
                    candidate_after >= -self.policy.max_debt
                } else {
                    before >= 0 && candidate_after >= 0
                };
                if valid {
                    after = candidate_after;
                    planned_odds_bits = odds_bits(self.totals, team);
                    self.balances.insert(discord_id, after);
                    self.totals.add(team, amount);
                } else {
                    status = DraftFinancialIntentStatus::Skipped;
                    reason = Some(if self.is_bomb_pot {
                        format!(
                            "player {discord_id} would exceed the maximum debt {}",
                            self.policy.max_debt
                        )
                    } else if before < 0 {
                        format!("player {discord_id} cannot bet while in debt")
                    } else {
                        format!(
                            "player {discord_id} has insufficient balance {before} for stake {amount}"
                        )
                    });
                }
            }
            self.push(
                format!(
                    "{}:blind:{}:{}",
                    self.completion_key, self.pending_match_id, discord_id
                ),
                DraftFinancialEffectKind::Blind,
                status,
                wager_json(WagerJson {
                    guild_id: self.guild_id,
                    session_id: self.session_id,
                    pending_match_id: self.pending_match_id,
                    discord_id,
                    team,
                    amount,
                    bet_time: self.shuffle_timestamp,
                    odds_bits: planned_odds_bits,
                    expected_balance_before: before,
                    expected_balance_after: after,
                    reason,
                    extra: Map::new(),
                }),
            );
        }
        Ok(())
    }

    fn plan_investments(&mut self) -> Result<(), DraftFinancialSetupError> {
        let sizing_balances = self.balances.clone();
        let teams = self
            .radiant
            .iter()
            .map(|id| (*id, Team::Radiant))
            .chain(self.dire.iter().map(|id| (*id, Team::Dire)))
            .collect::<BTreeMap<_, _>>();
        for position in self.investments.clone() {
            let target_team = teams.get(&position.target_id).copied().ok_or_else(|| {
                DraftFinancialSetupError::InvalidPlan(
                    "investment target is not a Draft participant".to_owned(),
                )
            })?;
            let team = if position.direction == "long" {
                target_team
            } else {
                target_team.opposite()
            };
            let sizing_balance = sizing_balances
                .get(&position.investor_id)
                .copied()
                .unwrap_or_default();
            let before = self
                .balances
                .get(&position.investor_id)
                .copied()
                .unwrap_or_default();
            let mut status = DraftFinancialIntentStatus::Applied;
            let mut reason = None;
            let amount = sizing_balance
                .checked_mul(position.percentage)
                .ok_or(DraftFinancialSetupError::Overflow)?
                / 100;
            if position.starting_balance < 50 {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some(format!(
                    "shuffle-start balance {} < 50",
                    position.starting_balance
                ));
            } else if sizing_balance <= 0 {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some(format!("balance {sizing_balance} is not positive"));
            } else if position.direction == "short" && position.investor_id == position.target_id {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some("cannot short yourself".to_owned());
            } else if teams
                .get(&position.investor_id)
                .is_some_and(|investor_team| *investor_team != team)
            {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some("investment would bet against the investor's team".to_owned());
            } else if amount < 1 {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some(format!("investment amount {amount} < 1"));
            }
            let mut after = before;
            let mut odds = None;
            if status == DraftFinancialIntentStatus::Applied {
                let candidate_after = before
                    .checked_sub(amount)
                    .ok_or(DraftFinancialSetupError::Overflow)?;
                if before < 0 || candidate_after < 0 {
                    status = DraftFinancialIntentStatus::Skipped;
                    reason = Some(if before < 0 {
                        format!("player {} cannot bet while in debt", position.investor_id)
                    } else {
                        format!(
                            "player {} has insufficient balance {before} for stake {amount}",
                            position.investor_id
                        )
                    });
                } else {
                    after = candidate_after;
                    odds = odds_bits(self.totals, team);
                    self.balances.insert(position.investor_id, after);
                    self.totals.add(team, amount);
                }
            }
            let mut extra = Map::new();
            extra.insert("target_id".to_owned(), Value::from(position.target_id));
            extra.insert(
                "direction".to_owned(),
                Value::from(position.direction.clone()),
            );
            extra.insert("percentage".to_owned(), Value::from(position.percentage));
            extra.insert(
                "starting_balance".to_owned(),
                Value::from(position.starting_balance),
            );
            extra.insert("sizing_balance".to_owned(), Value::from(sizing_balance));
            self.push(
                format!(
                    "{}:investment:{}:{}:{}:{}",
                    self.completion_key,
                    self.pending_match_id,
                    position.investor_id,
                    position.target_id,
                    position.direction
                ),
                DraftFinancialEffectKind::Investment,
                status,
                wager_json(WagerJson {
                    guild_id: self.guild_id,
                    session_id: self.session_id,
                    pending_match_id: self.pending_match_id,
                    discord_id: position.investor_id,
                    team,
                    amount,
                    bet_time: self.shuffle_timestamp,
                    odds_bits: odds,
                    expected_balance_before: before,
                    expected_balance_after: after,
                    reason,
                    extra,
                }),
            );
        }
        Ok(())
    }

    fn plan_spectators(&mut self) -> Result<(), DraftFinancialSetupError> {
        let base_percentage = percentage_from_bits(&self.policy.spectator_base_percentage_bits)?;
        let top_percentage = percentage_from_bits(&self.policy.spectator_top_percentage_bits)?;
        if !self.policy.spectator_enabled
            || self.policy.spectator_total_count == 0
            || base_percentage.max(top_percentage) <= 0.0
        {
            return Ok(());
        }
        let participants = self
            .radiant
            .iter()
            .chain(&self.dire)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut candidates = self
            .wealth
            .iter()
            .cloned()
            .map(|mut snapshot| {
                snapshot.balance = self
                    .balances
                    .get(&snapshot.discord_id)
                    .copied()
                    .unwrap_or(snapshot.balance);
                snapshot
            })
            .filter(|snapshot| snapshot.balance > 0)
            .collect::<Vec<_>>();
        // Stable sort retains SQLite's frozen source order for equal balances.
        candidates.sort_by(|left, right| right.balance.cmp(&left.balance));
        let total_count = usize::try_from(self.policy.spectator_total_count)
            .map_err(|_| DraftFinancialSetupError::Overflow)?;
        let limit = total_count
            .checked_mul(2)
            .and_then(|value| value.checked_add(participants.len()))
            .ok_or(DraftFinancialSetupError::Overflow)?;
        candidates.truncate(limit);
        candidates.retain(|candidate| !participants.contains(&candidate.discord_id));
        let top_count = if top_percentage > 0.0 {
            usize::try_from(self.policy.spectator_top_count)
                .map_err(|_| DraftFinancialSetupError::Overflow)?
                .min(total_count)
        } else {
            0
        };
        let mut successful = 0_usize;
        let mut spectator_totals = Totals::default();
        for candidate in candidates {
            if successful >= total_count {
                break;
            }
            let percentage_bits = if successful < top_count {
                &self.policy.spectator_top_percentage_bits
            } else {
                &self.policy.spectator_base_percentage_bits
            };
            let amount = python_round_product(candidate.balance, percentage_bits)?;
            let team = spectator_team(
                spectator_totals,
                amount,
                self.guild_id,
                self.pending_match_id,
                candidate.discord_id,
                successful,
            );
            let before = self
                .balances
                .get(&candidate.discord_id)
                .copied()
                .unwrap_or(candidate.balance);
            let mut status = DraftFinancialIntentStatus::Applied;
            let mut reason = None;
            if amount < 1 {
                status = DraftFinancialIntentStatus::Skipped;
                reason = Some(format!("auto-wager amount {amount} < 1"));
            }
            let mut after = before;
            let mut odds = None;
            if status == DraftFinancialIntentStatus::Applied {
                let candidate_after = before
                    .checked_sub(amount)
                    .ok_or(DraftFinancialSetupError::Overflow)?;
                if before < 0 || candidate_after < 0 {
                    status = DraftFinancialIntentStatus::Skipped;
                    reason = Some(if before < 0 {
                        format!("player {} cannot bet while in debt", candidate.discord_id)
                    } else {
                        format!(
                            "player {} has insufficient balance {before} for stake {amount}",
                            candidate.discord_id
                        )
                    });
                } else {
                    after = candidate_after;
                    odds = odds_bits(self.totals, team);
                    self.balances.insert(candidate.discord_id, after);
                    self.totals.add(team, amount);
                    spectator_totals.add(team, amount);
                    successful = successful.saturating_add(1);
                }
            }
            let mut extra = Map::new();
            extra.insert("networth".to_owned(), Value::from(candidate.balance));
            extra.insert(
                "percentage_bits".to_owned(),
                Value::from(percentage_bits.clone()),
            );
            extra.insert(
                "successful_rank".to_owned(),
                Value::from(
                    successful
                        .saturating_sub(usize::from(status == DraftFinancialIntentStatus::Applied))
                        as u64,
                ),
            );
            extra.insert(
                "source_ordinal".to_owned(),
                Value::from(candidate.source_ordinal),
            );
            self.push(
                format!(
                    "{}:spectator:{}:{}",
                    self.completion_key, self.pending_match_id, candidate.discord_id
                ),
                DraftFinancialEffectKind::Spectator,
                status,
                wager_json(WagerJson {
                    guild_id: self.guild_id,
                    session_id: self.session_id,
                    pending_match_id: self.pending_match_id,
                    discord_id: candidate.discord_id,
                    team,
                    amount,
                    bet_time: self.shuffle_timestamp,
                    odds_bits: odds,
                    expected_balance_before: before,
                    expected_balance_after: after,
                    reason,
                    extra,
                }),
            );
        }
        Ok(())
    }
}

struct WagerJson {
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    discord_id: i64,
    team: Team,
    amount: i64,
    bet_time: i64,
    odds_bits: Option<String>,
    expected_balance_before: i64,
    expected_balance_after: i64,
    reason: Option<String>,
    extra: Map<String, Value>,
}

fn wager_json(wager: WagerJson) -> Value {
    let mut object = wager.extra;
    object.insert("guild_id".to_owned(), Value::from(wager.guild_id));
    object.insert("session_id".to_owned(), Value::from(wager.session_id));
    object.insert(
        "pending_match_id".to_owned(),
        Value::from(wager.pending_match_id),
    );
    object.insert("discord_id".to_owned(), Value::from(wager.discord_id));
    object.insert("team".to_owned(), Value::from(wager.team.as_str()));
    object.insert("amount".to_owned(), Value::from(wager.amount));
    object.insert("bet_time".to_owned(), Value::from(wager.bet_time));
    object.insert(
        "odds_bits".to_owned(),
        wager.odds_bits.map_or(Value::Null, Value::from),
    );
    object.insert(
        "expected_balance_before".to_owned(),
        Value::from(wager.expected_balance_before),
    );
    object.insert(
        "expected_balance_after".to_owned(),
        Value::from(wager.expected_balance_after),
    );
    object.insert(
        "reason".to_owned(),
        wager.reason.map_or(Value::Null, Value::from),
    );
    Value::Object(object)
}

fn id_array(object: &Map<String, Value>, name: &str) -> Result<Vec<i64>, DraftFinancialSetupError> {
    object
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DraftFinancialSetupError::InvalidPending(format!("{name} must be an array"))
        })?
        .iter()
        .map(|value| {
            value.as_i64().filter(|id| *id > 0).ok_or_else(|| {
                DraftFinancialSetupError::InvalidPending(format!(
                    "{name} contains an invalid player ID"
                ))
            })
        })
        .collect()
}

fn investment_snapshot(
    transaction: &Transaction<'_>,
    guild_id: i64,
    targets: &BTreeSet<i64>,
) -> Result<Vec<DraftInvestmentSnapshot>, DraftFinancialSetupError> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", targets.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT i.investor_id,i.target_id,i.direction,i.percentage,
                COALESCE(p.jopacoin_balance,0)
           FROM autobet_investments i
           LEFT JOIN players p
             ON p.guild_id=i.guild_id AND p.discord_id=i.investor_id
          WHERE i.guild_id=?1 AND i.target_id IN ({placeholders})
          ORDER BY i.investor_id,i.target_id"
    );
    let mut values = Vec::with_capacity(targets.len() + 1);
    values.push(rusqlite::types::Value::Integer(guild_id));
    values.extend(targets.iter().copied().map(rusqlite::types::Value::Integer));
    let mut statement = transaction.prepare(&sql)?;
    statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(DraftInvestmentSnapshot {
                investor_id: row.get(0)?,
                target_id: row.get(1)?,
                direction: row.get(2)?,
                percentage: row.get(3)?,
                starting_balance: row.get(4)?,
                extensions: BTreeMap::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn wealth_snapshot(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<Vec<DraftWealthSnapshot>, DraftFinancialSetupError> {
    // Python's post-effect richest query is served by
    // idx_players_leaderboard. For equal *post-effect* balances its SQLite
    // source order is the index suffix (wins, Glicko, then rowid), not the
    // players' pre-effect balance order. Freeze that source order without
    // inventing a Discord-ID tiebreak; the stable post-simulation sort below
    // then matches the live query when investments make balances converge.
    let mut statement = transaction.prepare(
        "SELECT discord_id,COALESCE(jopacoin_balance,0)
           FROM players
          WHERE guild_id=?1 AND COALESCE(jopacoin_balance,0)>=1
          ORDER BY wins DESC,glicko_rating DESC,rowid ASC",
    )?;
    let rows = statement
        .query_map([guild_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, (discord_id, balance))| DraftWealthSnapshot {
            source_ordinal: ordinal as u64,
            discord_id,
            balance,
            extensions: BTreeMap::new(),
        })
        .collect())
}

fn balances_for(
    transaction: &Transaction<'_>,
    guild_id: i64,
    ids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, i64>, DraftFinancialSetupError> {
    let mut balances = BTreeMap::new();
    for discord_id in ids {
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                  WHERE guild_id=?1 AND discord_id=?2",
                params![guild_id, discord_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or_default();
        balances.insert(*discord_id, balance);
    }
    Ok(balances)
}

fn pending_totals(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
    shuffle_timestamp: i64,
) -> Result<Totals, DraftFinancialSetupError> {
    let mut totals = Totals::default();
    let mut statement = transaction.prepare(
        "SELECT team_bet_on,COALESCE(SUM(amount*COALESCE(leverage,1)),0)
           FROM bets
          WHERE guild_id=?1 AND match_id IS NULL
            AND (
                pending_match_id=?2
                OR (pending_match_id IS NULL AND bet_time>=?3)
            )
          GROUP BY team_bet_on",
    )?;
    let rows = statement.query_map(
        params![guild_id, pending_match_id, shuffle_timestamp],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    for row in rows {
        let (team, amount) = row?;
        match team.as_str() {
            "radiant" => totals.radiant = amount,
            "dire" => totals.dire = amount,
            _ => {}
        }
    }
    Ok(totals)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FundSnapshot {
    exists: bool,
    total: i64,
    queued: i64,
    open: i64,
    low_skill: i64,
}

impl FundSnapshot {
    fn to_json(self) -> Value {
        json!({
            "exists": self.exists,
            "total": self.total,
            "queued": self.queued,
            "open": self.open,
            "low_skill": self.low_skill,
        })
    }
}

fn fund_snapshot(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<FundSnapshot, DraftFinancialSetupError> {
    Ok(transaction
        .query_row(
            "SELECT COALESCE(total_collected,0),COALESCE(next_match_pot,0),
                    COALESCE(first_game_open_pool,0),
                    COALESCE(first_game_lowskill_pool,0)
               FROM nonprofit_fund WHERE guild_id=?1",
            [guild_id],
            |row| {
                Ok(FundSnapshot {
                    exists: true,
                    total: row.get::<_, i64>(0)?.max(0),
                    queued: row.get::<_, i64>(1)?.max(0),
                    open: row.get::<_, i64>(2)?.max(0),
                    low_skill: row.get::<_, i64>(3)?.max(0),
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

struct FundingSimulation {
    after: FundSnapshot,
    steps: Vec<Value>,
}

fn simulate_daily_funding(
    transaction: &Transaction<'_>,
    guild_id: i64,
    game_date: &str,
    daily_amount: i64,
    mut fund: FundSnapshot,
) -> Result<FundingSimulation, DraftFinancialSetupError> {
    if daily_amount <= 0 {
        return Ok(FundingSimulation {
            after: fund,
            steps: Vec::new(),
        });
    }
    let pair_cost = daily_amount
        .checked_mul(2)
        .ok_or(DraftFinancialSetupError::Overflow)?;
    let target = CivilDate::parse(game_date)?;
    let last = transaction
        .query_row(
            "SELECT MAX(game_date) FROM first_game_pool_funding_days WHERE guild_id=?1",
            [guild_id],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|value| CivilDate::parse(&value))
        .transpose()?;
    let mut next = last.map_or(target, CivilDate::next);
    let mut steps = Vec::new();
    while next <= target {
        let funded = fund.total >= pair_cost;
        if funded {
            fund.total -= pair_cost;
            fund.open = fund
                .open
                .checked_add(daily_amount)
                .ok_or(DraftFinancialSetupError::Overflow)?;
            fund.low_skill = fund
                .low_skill
                .checked_add(daily_amount)
                .ok_or(DraftFinancialSetupError::Overflow)?;
        }
        steps.push(json!({
            "game_date": next.to_string(),
            "daily_amount": daily_amount,
            "funded": funded,
        }));
        next = next.next();
    }
    Ok(FundingSimulation { after: fund, steps })
}

fn odds_bits(totals: Totals, team: Team) -> Option<String> {
    let total = totals.total();
    let side = totals.side(team);
    (total > 0 && side > 0).then(|| format!("{:016x}", (total as f64 / side as f64).to_bits()))
}

fn percentage_from_bits(bits: &str) -> Result<f64, DraftFinancialSetupError> {
    if bits.len() != 16
        || !bits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DraftFinancialSetupError::InvalidPolicy(
            "percentage bits must be 16 lowercase hex digits".to_owned(),
        ));
    }
    let bits = u64::from_str_radix(bits, 16).map_err(|error| {
        DraftFinancialSetupError::InvalidPolicy(format!("invalid percentage bits: {error}"))
    })?;
    Ok(f64::from_bits(bits))
}

fn python_round_product(
    balance: i64,
    percentage_bits: &str,
) -> Result<i64, DraftFinancialSetupError> {
    let rounded = (balance as f64 * percentage_from_bits(percentage_bits)?).round_ties_even();
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
        return Err(DraftFinancialSetupError::Overflow);
    }
    Ok(rounded as i64)
}

fn spectator_team(
    totals: Totals,
    amount: i64,
    guild_id: i64,
    pending_match_id: i64,
    discord_id: i64,
    successful_rank: usize,
) -> Team {
    let radiant_difference = totals
        .radiant
        .saturating_add(amount)
        .saturating_sub(totals.dire)
        .abs();
    let dire_difference = totals
        .dire
        .saturating_add(amount)
        .saturating_sub(totals.radiant)
        .abs();
    match radiant_difference.cmp(&dire_difference) {
        Ordering::Less => Team::Radiant,
        Ordering::Greater => Team::Dire,
        Ordering::Equal => {
            let seed = format!(
                "{guild_id}:{pending_match_id}:{discord_id}:{successful_rank}:auto-spectator"
            );
            if Sha256::digest(seed.as_bytes())[0] % 2 == 0 {
                Team::Radiant
            } else {
                Team::Dire
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

impl CivilDate {
    fn parse(value: &str) -> Result<Self, DraftFinancialSetupError> {
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
        parsed.ok_or_else(|| {
            DraftFinancialSetupError::InvalidPolicy(format!("invalid ISO game date {value}"))
        })
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
mod tests {
    use rusqlite::{Connection, TransactionBehavior, params};
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    fn bits(value: f64) -> String {
        format!("{:016x}", value.to_bits())
    }

    fn policy() -> DraftFinancialSetupPolicy {
        DraftFinancialSetupPolicy {
            schema_version: DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date: "2026-08-14".to_owned(),
            betting_mode: "pool".to_owned(),
            seed_max_amount: 0,
            first_game_daily_amount: 0,
            auto_blind_enabled: true,
            normal_blind_threshold: 50,
            normal_blind_percentage_bits: bits(0.1),
            bomb_blind_percentage_bits: bits(0.5),
            bomb_ante: 10,
            max_debt: 20,
            spectator_enabled: true,
            spectator_total_count: 2,
            spectator_top_count: 2,
            spectator_base_percentage_bits: bits(0.5),
            spectator_top_percentage_bits: bits(0.5),
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn banker_rounding_and_spectator_ties_match_python() {
        let half = bits(0.5);
        assert_eq!(python_round_product(5, &half).expect("round 2.5"), 2);
        assert_eq!(python_round_product(7, &half).expect("round 3.5"), 4);
        assert_eq!(
            spectator_team(Totals::default(), 450, 42, 1, 11, 0),
            Team::Dire
        );
        assert_eq!(
            spectator_team(
                Totals {
                    radiant: 10,
                    dire: 0,
                },
                5,
                42,
                1,
                11,
                0,
            ),
            Team::Dire
        );
    }

    #[test]
    fn planner_freezes_bomb_debt_post_blind_investments_and_spectator_ranks() {
        let file = NamedTempFile::new().expect("create financial planner fixture");
        crate::test_support::copy_migrated_database(file.path())
            .expect("copy financial planner fixture");
        let mut connection = Connection::open(file.path()).expect("open financial planner fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture setup");
        for (discord_id, balance) in [(1, 100), (2, -5), (3, 100), (11, 1_000), (12, 1)] {
            connection
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance
                     ) VALUES (?1,42,?2,?3)",
                    params![discord_id, format!("player-{discord_id}"), balance],
                )
                .expect("insert planner player");
        }
        for (investor_id, target_id) in [(1, 2), (11, 1)] {
            connection
                .execute(
                    "INSERT INTO autobet_investments(
                         guild_id,investor_id,target_id,direction,percentage
                     ) VALUES (42,?1,?2,'long',10)",
                    params![investor_id, target_id],
                )
                .expect("insert planner investment");
        }
        let pending_payload = json!({
            "radiant_team_ids": [1,2],
            "dire_team_ids": [3],
            "shuffle_timestamp": 1_700_000_000_i64,
            "bet_lock_until": 1_700_000_600_i64,
            "betting_mode": "pool",
            "is_bomb_pot": true,
            "lobby_kind": "open"
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO pending_matches(guild_id,payload,completion_key)
                 VALUES (42,?1,'draft:42:7')",
                [&pending_payload],
            )
            .expect("insert planner pending match");
        let pending_match_id = connection.last_insert_rowid();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin financial planner transaction");
        let plan = freeze_financial_setup_plan(
            &transaction,
            "draft:42:7",
            42,
            7,
            pending_match_id,
            &pending_payload,
            policy(),
        )
        .expect("freeze financial planner intents");
        transaction.rollback().expect("close planner transaction");

        let effect = |kind: DraftFinancialEffectKind, discord_id: i64| {
            plan.effects
                .iter()
                .find(|effect| {
                    effect.effect_kind == kind
                        && effect.intended["discord_id"].as_i64() == Some(discord_id)
                })
                .expect("planned financial effect")
        };
        let negative_bomb = effect(DraftFinancialEffectKind::Blind, 2);
        assert_eq!(
            negative_bomb.intended_status,
            DraftFinancialIntentStatus::Applied
        );
        assert_eq!(negative_bomb.intended["amount"], json!(10));
        assert_eq!(negative_bomb.intended["expected_balance_before"], json!(-5));
        assert_eq!(negative_bomb.intended["expected_balance_after"], json!(-15));

        let participant_investment = effect(DraftFinancialEffectKind::Investment, 1);
        assert_eq!(
            participant_investment.intended["starting_balance"],
            json!(100)
        );
        assert_eq!(participant_investment.intended["sizing_balance"], json!(40));
        assert_eq!(participant_investment.intended["amount"], json!(4));
        assert_eq!(
            participant_investment.intended["expected_balance_after"],
            json!(36)
        );

        let spectator_investment = effect(DraftFinancialEffectKind::Investment, 11);
        assert_eq!(spectator_investment.intended["amount"], json!(100));
        assert_eq!(
            plan.wealth_snapshot
                .iter()
                .find(|snapshot| snapshot.discord_id == 11)
                .expect("pre-effect spectator wealth")
                .balance,
            1_000
        );
        let spectator = effect(DraftFinancialEffectKind::Spectator, 11);
        assert_eq!(
            spectator.intended_status,
            DraftFinancialIntentStatus::Applied
        );
        assert_eq!(spectator.intended["networth"], json!(900));
        assert_eq!(spectator.intended["amount"], json!(450));
        assert_eq!(spectator.intended["successful_rank"], json!(0));
        assert_eq!(spectator.intended["team"], json!("dire"));
        let skipped = effect(DraftFinancialEffectKind::Spectator, 12);
        assert_eq!(skipped.intended_status, DraftFinancialIntentStatus::Skipped);
        assert_eq!(skipped.intended["amount"], json!(0));
        assert_eq!(skipped.intended["successful_rank"], json!(1));

        let mut malformed = plan.clone();
        malformed.effects[1].effect_key = "draft:42:7:blind:999:1".to_owned();
        assert!(matches!(
            malformed.validate_for_identity(42, 7, pending_match_id),
            Err(DraftFinancialSetupError::InvalidPlan(_))
        ));
    }

    #[test]
    fn spectator_convergence_tie_uses_sqlite_post_effect_source_order() {
        let file = NamedTempFile::new().expect("create convergence planner fixture");
        crate::test_support::copy_migrated_database(file.path())
            .expect("copy convergence planner fixture");
        let mut connection =
            Connection::open(file.path()).expect("open convergence planner fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture setup");
        for (discord_id, balance, wins) in [(1, 0, 0), (2, 0, 0), (11, 1_000, 0), (12, 900, 10)] {
            connection
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance,wins
                     ) VALUES (?1,42,?2,?3,?4)",
                    params![discord_id, format!("player-{discord_id}"), balance, wins],
                )
                .expect("insert convergence player");
        }
        connection
            .execute(
                "INSERT INTO autobet_investments(
                     guild_id,investor_id,target_id,direction,percentage
                 ) VALUES (42,11,1,'long',10)",
                [],
            )
            .expect("insert convergence investment");
        let pending_payload = json!({
            "radiant_team_ids": [1],
            "dire_team_ids": [2],
            "shuffle_timestamp": 1_700_000_000_i64,
            "bet_lock_until": 1_700_000_600_i64,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string();
        connection
            .execute(
                "INSERT INTO pending_matches(guild_id,payload,completion_key)
                 VALUES (42,?1,'draft:42:8')",
                [&pending_payload],
            )
            .expect("insert convergence pending match");
        let pending_match_id = connection.last_insert_rowid();
        let mut convergence_policy = policy();
        convergence_policy.auto_blind_enabled = false;
        convergence_policy.spectator_top_count = 1;
        convergence_policy.spectator_top_percentage_bits = bits(0.1);
        convergence_policy.spectator_base_percentage_bits = bits(0.01);
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin convergence planner transaction");
        let plan = freeze_financial_setup_plan(
            &transaction,
            "draft:42:8",
            42,
            8,
            pending_match_id,
            &pending_payload,
            convergence_policy,
        )
        .expect("freeze convergence financial intents");

        transaction
            .execute(
                "UPDATE players SET jopacoin_balance=900
                  WHERE guild_id=42 AND discord_id=11",
                [],
            )
            .expect("materialize simulated investment balance");
        let sqlite_order = {
            let mut statement = transaction
                .prepare(
                    "SELECT discord_id FROM players
                      WHERE guild_id=42 AND jopacoin_balance>=1
                      ORDER BY jopacoin_balance DESC LIMIT 6",
                )
                .expect("prepare authoritative richest query");
            statement
                .query_map([], |row| row.get::<_, i64>(0))
                .expect("query authoritative richest order")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect authoritative richest order")
        };
        let planned_order = plan
            .effects
            .iter()
            .filter(|effect| effect.effect_kind == DraftFinancialEffectKind::Spectator)
            .map(|effect| {
                effect.intended["discord_id"]
                    .as_i64()
                    .expect("spectator identity")
            })
            .collect::<Vec<_>>();
        assert_eq!(sqlite_order, [12, 11]);
        assert_eq!(planned_order, sqlite_order);
        let planned_amounts = plan
            .effects
            .iter()
            .filter(|effect| effect.effect_kind == DraftFinancialEffectKind::Spectator)
            .map(|effect| {
                effect.intended["amount"]
                    .as_i64()
                    .expect("spectator amount")
            })
            .collect::<Vec<_>>();
        assert_eq!(planned_amounts, [90, 9]);
        transaction
            .rollback()
            .expect("close convergence transaction");
    }
}
