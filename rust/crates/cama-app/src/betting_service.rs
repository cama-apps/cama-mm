//! Discord-neutral betting orchestration and reward policy.
//!
//! SQLite placement/settlement lives in
//! `cama_db::betting_service_repository`; nonprofit seed accounting remains in
//! [`crate::dota_bet_seed`]. This module models shuffle/pending payloads,
//! deterministic automatic spectator selection, and the ordering of economy,
//! vanity-tax, Communion Blessing, and Blood Pact hooks. Python remains the
//! live command and Discord authority during parity.

use std::collections::{BTreeMap, BTreeSet};

use cama_db::dota_bet_seed_repository::BettingTeam;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const DEFAULT_WIN_REWARD: i64 = 10;
pub const DEFAULT_EXCLUSION_REWARD: i64 = 5;
pub const DEFAULT_PARTICIPATION_REWARD: i64 = 5;
pub const DEFAULT_VANITY_TAX_BASIS_POINTS: i64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Team {
    pub player_ids: Vec<DiscordId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleResult {
    pub radiant_team: Team,
    pub dire_team: Team,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatingSystem {
    OpenSkill,
    Glicko,
    Jopacoin,
}

impl RatingSystem {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenSkill => "openskill",
            Self::Glicko => "glicko",
            Self::Jopacoin => "jopacoin",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMatchState {
    pub radiant_team_ids: Vec<DiscordId>,
    pub dire_team_ids: Vec<DiscordId>,
    pub shuffle_timestamp: i64,
    pub bet_lock_until: i64,
    pub is_draft: bool,
    pub is_bomb_pot: bool,
    pub is_openskill_shuffle: bool,
    pub balancing_rating_system: RatingSystem,
}

impl PendingMatchState {
    #[must_use]
    pub fn to_payload_json(&self) -> String {
        format!(
            "{{\"radiant_team_ids\":{},\"dire_team_ids\":{},\"shuffle_timestamp\":{},\"bet_lock_until\":{},\"is_draft\":{},\"is_bomb_pot\":{},\"is_openskill_shuffle\":{},\"balancing_rating_system\":\"{}\"}}",
            json_i64_array(&self.radiant_team_ids),
            json_i64_array(&self.dire_team_ids),
            self.shuffle_timestamp,
            self.bet_lock_until,
            self.is_draft,
            self.is_bomb_pot,
            self.is_openskill_shuffle,
            self.balancing_rating_system.as_str(),
        )
    }
}

#[must_use]
pub fn typed_shuffle(
    players: &[DiscordId],
    shuffle_timestamp: i64,
) -> (ShuffleResult, PendingMatchState) {
    let middle = players.len() / 2;
    let radiant = players[..middle].to_vec();
    let dire = players[middle..].to_vec();
    (
        ShuffleResult {
            radiant_team: Team {
                player_ids: radiant.clone(),
            },
            dire_team: Team {
                player_ids: dire.clone(),
            },
        },
        PendingMatchState {
            radiant_team_ids: radiant,
            dire_team_ids: dire,
            shuffle_timestamp,
            bet_lock_until: shuffle_timestamp.saturating_add(900),
            is_draft: false,
            is_bomb_pot: false,
            is_openskill_shuffle: false,
            balancing_rating_system: RatingSystem::Glicko,
        },
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WealthSnapshot {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorConfig {
    pub enabled: bool,
    pub total_count: usize,
    pub top_count: usize,
    pub base_basis_points: i64,
    pub top_basis_points: i64,
}

impl Default for AutomaticSpectatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            total_count: 10,
            top_count: 5,
            base_basis_points: 100,
            top_basis_points: 200,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorBet {
    pub discord_id: DiscordId,
    pub team: BettingTeam,
    pub amount: i64,
    pub networth: i64,
    pub basis_points: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorPlan {
    pub bets: Vec<AutomaticSpectatorBet>,
    pub top_count: usize,
    pub total_count: usize,
    pub base_basis_points: i64,
    pub top_basis_points: i64,
    pub total_radiant: i64,
    pub total_dire: i64,
}

/// Select the richest nonparticipants and greedily balance their deterministic
/// automatic wagers. The output is an explicit plan for the DB batch port;
/// planning itself performs no persistence or RNG calls.
#[must_use]
pub fn plan_automatic_spectator_bets(
    candidates: &[WealthSnapshot],
    participant_ids: &BTreeSet<DiscordId>,
    config: AutomaticSpectatorConfig,
) -> AutomaticSpectatorPlan {
    let top_count = if config.top_basis_points > 0 {
        config.top_count.min(config.total_count)
    } else {
        0
    };
    let mut plan = AutomaticSpectatorPlan {
        bets: Vec::new(),
        top_count,
        total_count: config.total_count,
        base_basis_points: config.base_basis_points,
        top_basis_points: config.top_basis_points,
        total_radiant: 0,
        total_dire: 0,
    };
    if !config.enabled
        || config.total_count == 0
        || config.base_basis_points.max(config.top_basis_points) <= 0
    {
        return plan;
    }
    let mut spectators = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.balance > 0)
        .filter(|candidate| !participant_ids.contains(&candidate.discord_id))
        .collect::<Vec<_>>();
    spectators.sort_by(|left, right| {
        right
            .balance
            .cmp(&left.balance)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });

    for spectator in spectators {
        if plan.bets.len() >= config.total_count {
            break;
        }
        let basis_points = if plan.bets.len() < top_count {
            config.top_basis_points
        } else {
            config.base_basis_points
        };
        let amount = rounded_basis_points(spectator.balance, basis_points);
        if amount < 1 {
            continue;
        }
        let radiant_difference = plan
            .total_radiant
            .saturating_add(amount)
            .saturating_sub(plan.total_dire)
            .abs();
        let dire_difference = plan
            .total_dire
            .saturating_add(amount)
            .saturating_sub(plan.total_radiant)
            .abs();
        let team = if radiant_difference <= dire_difference {
            BettingTeam::Radiant
        } else {
            BettingTeam::Dire
        };
        match team {
            BettingTeam::Radiant => {
                plan.total_radiant = plan.total_radiant.saturating_add(amount);
            }
            BettingTeam::Dire => {
                plan.total_dire = plan.total_dire.saturating_add(amount);
            }
        }
        plan.bets.push(AutomaticSpectatorBet {
            discord_id: spectator.discord_id,
            team,
            amount,
            networth: spectator.balance,
            basis_points,
        });
    }
    plan
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AwardPolicy {
    /// Fraction the player keeps after bankruptcy, in basis points.
    pub bankruptcy_keep_basis_points: i64,
    /// Vanity tax on gross generated JC, in basis points.
    pub vanity_tax_basis_points: i64,
}

impl AwardPolicy {
    #[must_use]
    pub const fn ordinary() -> Self {
        Self {
            bankruptcy_keep_basis_points: 10_000,
            vanity_tax_basis_points: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AwardResult {
    pub gross: i64,
    pub garnished: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub manashop_bonus: i64,
    pub blood_pact_skimmed: i64,
    pub net: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreditRequest {
    pub discord_id: DiscordId,
    pub amount: i64,
    pub source: CreditSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreditSource {
    Win,
    Exclusion,
    Participation,
    Sanctuary,
}

pub trait BettingRewardPort {
    fn credit_many(&mut self, guild_id: GuildId, credits: &[CreditRequest]) -> Result<(), String>;

    fn credit_one(&mut self, guild_id: GuildId, credit: CreditRequest) -> Result<(), String>;

    fn communion_blessings(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
    ) -> Result<BTreeMap<DiscordId, i64>, String>;

    fn consume_blessing_and_credit(
        &mut self,
        guild_id: GuildId,
        discord_id: DiscordId,
        buff_id: i64,
        amount: i64,
    ) -> Result<bool, String>;

    fn sanctuary_bonus(
        &mut self,
        guild_id: GuildId,
        discord_id: DiscordId,
        base_reward: i64,
    ) -> Result<i64, String>;

    fn adjust_generated_reward(&mut self, guild_id: GuildId, amount: i64) -> Result<i64, String>;

    fn blood_pact_targets(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
    ) -> Result<BTreeSet<DiscordId>, String>;

    fn skim_blood_pact(
        &mut self,
        guild_id: GuildId,
        discord_id: DiscordId,
        earning: i64,
        match_id: Option<i64>,
    ) -> Result<i64, String>;
}

pub struct BettingRewardService<P> {
    port: P,
}

impl<P> BettingRewardService<P>
where
    P: BettingRewardPort,
{
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    pub fn award_exclusion_bonus(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
    ) -> Result<BTreeMap<DiscordId, AwardResult>, String> {
        self.award_simple(
            guild_id,
            player_ids,
            DEFAULT_EXCLUSION_REWARD,
            CreditSource::Exclusion,
        )
    }

    pub fn award_exclusion_bonus_half(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
    ) -> Result<BTreeMap<DiscordId, AwardResult>, String> {
        self.award_simple(guild_id, player_ids, 1, CreditSource::Exclusion)
    }

    fn award_simple(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
        reward: i64,
        source: CreditSource,
    ) -> Result<BTreeMap<DiscordId, AwardResult>, String> {
        if player_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let credits = player_ids
            .iter()
            .map(|discord_id| CreditRequest {
                discord_id: *discord_id,
                amount: reward,
                source,
            })
            .collect::<Vec<_>>();
        self.port.credit_many(guild_id, &credits)?;
        let mut results = player_ids
            .iter()
            .map(|discord_id| {
                (
                    *discord_id,
                    AwardResult {
                        gross: reward,
                        net: reward,
                        ..AwardResult::default()
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.apply_pact_skims(guild_id, &mut results, false, None)?;
        Ok(results)
    }

    pub fn award_participation(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
    ) -> Result<BTreeMap<DiscordId, AwardResult>, String> {
        if player_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let pact_targets = self
            .port
            .blood_pact_targets(guild_id, player_ids)
            .unwrap_or_default();
        let mut results = BTreeMap::new();
        if pact_targets.is_empty() {
            let credits = player_ids
                .iter()
                .map(|discord_id| CreditRequest {
                    discord_id: *discord_id,
                    amount: DEFAULT_PARTICIPATION_REWARD,
                    source: CreditSource::Participation,
                })
                .collect::<Vec<_>>();
            self.port.credit_many(guild_id, &credits)?;
            for discord_id in player_ids {
                results.insert(
                    *discord_id,
                    AwardResult {
                        gross: DEFAULT_PARTICIPATION_REWARD,
                        net: DEFAULT_PARTICIPATION_REWARD,
                        ..AwardResult::default()
                    },
                );
            }
            return Ok(results);
        }
        for discord_id in player_ids {
            self.port.credit_one(
                guild_id,
                CreditRequest {
                    discord_id: *discord_id,
                    amount: DEFAULT_PARTICIPATION_REWARD,
                    source: CreditSource::Participation,
                },
            )?;
            let mut result = AwardResult {
                gross: DEFAULT_PARTICIPATION_REWARD,
                net: DEFAULT_PARTICIPATION_REWARD,
                ..AwardResult::default()
            };
            if pact_targets.contains(discord_id) {
                let skimmed = self
                    .port
                    .skim_blood_pact(guild_id, *discord_id, DEFAULT_PARTICIPATION_REWARD, None)
                    .unwrap_or_default();
                result.blood_pact_skimmed = skimmed;
                result.net = result.net.saturating_sub(skimmed);
            }
            results.insert(*discord_id, result);
        }
        Ok(results)
    }

    pub fn award_win_bonus(
        &mut self,
        guild_id: GuildId,
        player_ids: &[DiscordId],
        base_reward: i64,
        policies: &BTreeMap<DiscordId, AwardPolicy>,
    ) -> Result<BTreeMap<DiscordId, AwardResult>, String> {
        if player_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let blessings = self
            .port
            .communion_blessings(guild_id, player_ids)
            .unwrap_or_default();
        let mut results = BTreeMap::new();
        for discord_id in player_ids {
            let policy = policies
                .get(discord_id)
                .copied()
                .unwrap_or_else(AwardPolicy::ordinary);
            let mut result = calculate_award(base_reward, policy);
            self.port.credit_one(
                guild_id,
                CreditRequest {
                    discord_id: *discord_id,
                    amount: result.net,
                    source: CreditSource::Win,
                },
            )?;
            if let Ok(sanctuary_bonus) =
                self.port
                    .sanctuary_bonus(guild_id, *discord_id, base_reward)
                && sanctuary_bonus > 0
            {
                self.port.credit_one(
                    guild_id,
                    CreditRequest {
                        discord_id: *discord_id,
                        amount: sanctuary_bonus,
                        source: CreditSource::Sanctuary,
                    },
                )?;
                result.manashop_bonus = result.manashop_bonus.saturating_add(sanctuary_bonus);
                result.net = result.net.saturating_add(sanctuary_bonus);
            }
            if let Some(buff_id) = blessings.get(discord_id) {
                let raw_bonus = (base_reward / 10).max(1);
                let blessing_bonus = self
                    .port
                    .adjust_generated_reward(guild_id, raw_bonus)
                    .unwrap_or(raw_bonus);
                if self
                    .port
                    .consume_blessing_and_credit(guild_id, *discord_id, *buff_id, blessing_bonus)
                    .unwrap_or(false)
                {
                    result.manashop_bonus = result.manashop_bonus.saturating_add(blessing_bonus);
                    result.net = result.net.saturating_add(blessing_bonus);
                }
            }
            results.insert(*discord_id, result);
        }
        self.apply_pact_skims(guild_id, &mut results, true, None)?;
        Ok(results)
    }

    pub fn apply_settlement_hooks(
        &mut self,
        guild_id: GuildId,
        match_id: i64,
        settlement: &mut SettlementDistribution,
        penalized_ids: &BTreeSet<DiscordId>,
        bankruptcy_keep_basis_points: i64,
    ) -> Result<BTreeMap<DiscordId, i64>, String> {
        let profits = settlement.net_match_profits();
        for (discord_id, profit) in &profits {
            if *profit <= 0 || !penalized_ids.contains(discord_id) {
                continue;
            }
            // Python floors the withheld share, not the kept share.  This is
            // observable for small profits: at a 75% keep rate a 5 JC profit
            // loses 1 JC and keeps 4, rather than keeping only 3.
            let withheld_basis_points =
                10_000_i64.saturating_sub(bankruptcy_keep_basis_points.clamp(0, 10_000));
            let penalty = rounded_down_basis_points(*profit, withheld_basis_points);
            if penalty > 0 {
                settlement.bankruptcy_penalties.insert(*discord_id, penalty);
            }
        }
        let player_ids = profits.keys().copied().collect::<Vec<_>>();
        let pact_targets = self
            .port
            .blood_pact_targets(guild_id, &player_ids)
            .unwrap_or_default();
        let mut skims = BTreeMap::new();
        for (discord_id, profit) in profits {
            let net_profit = profit
                .saturating_sub(
                    settlement
                        .bankruptcy_penalties
                        .get(&discord_id)
                        .copied()
                        .unwrap_or_default(),
                )
                .saturating_sub(
                    settlement
                        .vanity_taxes
                        .get(&discord_id)
                        .copied()
                        .unwrap_or_default(),
                );
            if net_profit <= 0 || !pact_targets.contains(&discord_id) {
                continue;
            }
            let skimmed = self
                .port
                .skim_blood_pact(guild_id, discord_id, net_profit, Some(match_id))
                .unwrap_or_default();
            if skimmed > 0 {
                skims.insert(discord_id, skimmed);
            }
        }
        Ok(skims)
    }

    fn apply_pact_skims(
        &mut self,
        guild_id: GuildId,
        results: &mut BTreeMap<DiscordId, AwardResult>,
        use_net: bool,
        match_id: Option<i64>,
    ) -> Result<(), String> {
        let player_ids = results.keys().copied().collect::<Vec<_>>();
        let targets = self
            .port
            .blood_pact_targets(guild_id, &player_ids)
            .unwrap_or_default();
        for (discord_id, result) in results {
            if !targets.contains(discord_id) {
                continue;
            }
            let earning = if use_net { result.net } else { result.gross };
            let skimmed = self
                .port
                .skim_blood_pact(guild_id, *discord_id, earning, match_id)
                .unwrap_or_default();
            result.blood_pact_skimmed = skimmed;
            result.net = result.net.saturating_sub(skimmed);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettlementBet {
    pub discord_id: DiscordId,
    pub effective_stake: i64,
    pub payout: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettlementDistribution {
    pub winners: Vec<SettlementBet>,
    pub losers: Vec<SettlementBet>,
    pub bankruptcy_penalties: BTreeMap<DiscordId, i64>,
    pub vanity_taxes: BTreeMap<DiscordId, i64>,
}

impl SettlementDistribution {
    #[must_use]
    pub fn net_match_profits(&self) -> BTreeMap<DiscordId, i64> {
        let mut profits = BTreeMap::new();
        for winner in &self.winners {
            profits
                .entry(winner.discord_id)
                .and_modify(|profit: &mut i64| {
                    *profit = profit
                        .saturating_add(winner.payout)
                        .saturating_sub(winner.effective_stake);
                })
                .or_insert_with(|| winner.payout.saturating_sub(winner.effective_stake));
        }
        for loser in &self.losers {
            if let Some(profit) = profits.get_mut(&loser.discord_id) {
                *profit = profit.saturating_sub(loser.effective_stake);
            }
        }
        profits
    }
}

#[must_use]
pub fn calculate_award(gross: i64, policy: AwardPolicy) -> AwardResult {
    let gross = gross.max(0);
    let keep_basis_points = policy.bankruptcy_keep_basis_points.clamp(0, 10_000);
    let bankruptcy_penalty =
        rounded_down_basis_points(gross, 10_000_i64.saturating_sub(keep_basis_points));
    let vanity_tax =
        rounded_down_basis_points(gross, policy.vanity_tax_basis_points.clamp(0, 5_000));
    AwardResult {
        gross,
        bankruptcy_penalty,
        vanity_tax,
        net: gross
            .saturating_sub(bankruptcy_penalty)
            .saturating_sub(vanity_tax),
        ..AwardResult::default()
    }
}

fn rounded_basis_points(value: i64, basis_points: i64) -> i64 {
    let numerator = value.saturating_mul(basis_points);
    let quotient = numerator / 10_000;
    let remainder = numerator % 10_000;
    let absolute_remainder = remainder.abs();
    let adjustment =
        if absolute_remainder > 5_000 || (absolute_remainder == 5_000 && quotient % 2 != 0) {
            numerator.signum()
        } else {
            0
        };
    quotient.saturating_add(adjustment)
}

fn rounded_down_basis_points(value: i64, basis_points: i64) -> i64 {
    value.saturating_mul(basis_points) / 10_000
}

fn json_i64_array(values: &[i64]) -> String {
    let body = values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

#[cfg(test)]
#[path = "betting_service/tests.rs"]
mod tests;
