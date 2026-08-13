//! Discord-neutral Dota betting-seed orchestration and payout policy.
//!
//! The module keeps reserve/claim persistence behind a narrow port and models
//! settlement, display, projections, and retired-command policy with typed
//! values. Python remains the live command/runtime authority during parity.

use std::collections::BTreeMap;

use cama_db::dota_bet_seed_repository::{
    BettingMode, BettingTeam, DotaBetSeedRepository, FirstGameLobby, FirstGamePoolBalances,
    FundingOutcome, SeedAbort, SeedReservation, SeedSettlement, SeedSplit,
};
use cama_domain::formatting::{
    BettingDisplayOptions, JOPACOIN_EMOTE, PoolOddsSeeds, calculate_pool_odds,
    format_betting_display,
};
use cama_domain::game_date::get_game_date;

use crate::dedicated_lobby_channel::{GuildId, UserId};
use crate::embeds::LobbyKind;

pub trait DotaBetSeedPort {
    type Error;

    fn first_game_pool_previews(
        &self,
        guild_id: GuildId,
        regular_seed_amount: i64,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FirstGamePoolBalances, Self::Error>;

    fn fund_first_game_pools(
        &self,
        guild_id: GuildId,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FundingOutcome, Self::Error>;

    fn claim_first_game_pool(
        &self,
        guild_id: GuildId,
        lobby: LobbyKind,
        game_date: &str,
        pending_match_id: i64,
    ) -> Result<i64, Self::Error>;

    fn reserve_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        max_seed_amount: i64,
        first_game_reserved: i64,
        mode: BettingMode,
    ) -> Result<SeedReservation, Self::Error>;

    fn settle_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        mode: BettingMode,
        winning_team: Option<BettingTeam>,
    ) -> Result<SeedSettlement, Self::Error>;

    fn abort_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
    ) -> Result<SeedAbort, Self::Error>;
}

impl DotaBetSeedPort for DotaBetSeedRepository {
    type Error = cama_db::dota_bet_seed_repository::DotaBetSeedRepositoryError;

    fn first_game_pool_previews(
        &self,
        guild_id: GuildId,
        regular_seed_amount: i64,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FirstGamePoolBalances, Self::Error> {
        DotaBetSeedRepository::first_game_pool_previews(
            self,
            Some(guild_id.0),
            regular_seed_amount,
            Some(game_date),
            daily_amount,
        )
    }

    fn fund_first_game_pools(
        &self,
        guild_id: GuildId,
        game_date: &str,
        daily_amount: i64,
    ) -> Result<FundingOutcome, Self::Error> {
        DotaBetSeedRepository::fund_first_game_pools(
            self,
            Some(guild_id.0),
            game_date,
            daily_amount,
        )
    }

    fn claim_first_game_pool(
        &self,
        guild_id: GuildId,
        lobby: LobbyKind,
        game_date: &str,
        pending_match_id: i64,
    ) -> Result<i64, Self::Error> {
        let lobby = match lobby {
            LobbyKind::Open => FirstGameLobby::Open,
            LobbyKind::LowSkill => FirstGameLobby::LowSkill,
        };
        DotaBetSeedRepository::claim_first_game_pool(
            self,
            Some(guild_id.0),
            lobby,
            game_date,
            pending_match_id,
        )
    }

    fn reserve_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        max_seed_amount: i64,
        first_game_reserved: i64,
        mode: BettingMode,
    ) -> Result<SeedReservation, Self::Error> {
        DotaBetSeedRepository::reserve_seed_atomic(
            self,
            Some(guild_id.0),
            pending_match_id,
            max_seed_amount,
            first_game_reserved,
            mode,
        )
    }

    fn settle_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        mode: BettingMode,
        winning_team: Option<BettingTeam>,
    ) -> Result<SeedSettlement, Self::Error> {
        DotaBetSeedRepository::settle_seed_atomic(
            self,
            Some(guild_id.0),
            pending_match_id,
            mode,
            winning_team,
        )
    }

    fn abort_seed_atomic(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
    ) -> Result<SeedAbort, Self::Error> {
        DotaBetSeedRepository::abort_seed_atomic(self, Some(guild_id.0), pending_match_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingSeedRequest<'a> {
    pub guild_id: GuildId,
    pub pending_match_id: i64,
    pub mode: BettingMode,
    pub lobby: LobbyKind,
    pub game_date: &'a str,
}

#[derive(Clone, Debug)]
pub struct DotaBetSeedService<P> {
    port: P,
    regular_seed_amount: i64,
    first_game_daily_amount: i64,
    game_date: fn() -> String,
}

impl<P> DotaBetSeedService<P>
where
    P: DotaBetSeedPort,
{
    #[must_use]
    pub const fn new(port: P, regular_seed_amount: i64, first_game_daily_amount: i64) -> Self {
        Self {
            port,
            regular_seed_amount,
            first_game_daily_amount,
            game_date: get_game_date,
        }
    }

    #[cfg(test)]
    fn with_game_date_provider(mut self, game_date: fn() -> String) -> Self {
        self.game_date = game_date;
        self
    }

    pub fn reserve_pending(
        &self,
        request: PendingSeedRequest<'_>,
    ) -> Result<SeedReservation, P::Error> {
        let first_game_reserved =
            if request.mode == BettingMode::Pool && self.first_game_daily_amount > 0 {
                self.port.fund_first_game_pools(
                    request.guild_id,
                    request.game_date,
                    self.first_game_daily_amount,
                )?;
                self.port.claim_first_game_pool(
                    request.guild_id,
                    request.lobby,
                    request.game_date,
                    request.pending_match_id,
                )?
            } else {
                0
            };
        self.port.reserve_seed_atomic(
            request.guild_id,
            request.pending_match_id,
            self.regular_seed_amount,
            first_game_reserved,
            request.mode,
        )
    }

    /// Return both lobby previews for the current fixed-PST game date.
    pub fn get_first_game_pool_previews(
        &self,
        guild_id: GuildId,
    ) -> Result<FirstGamePoolBalances, P::Error> {
        let game_date = (self.game_date)();
        self.get_first_game_pool_previews_for_date(guild_id, &game_date)
    }

    /// Deterministic form used by workers/tests after their clock has resolved
    /// the current fixed-PST game date.
    pub fn get_first_game_pool_previews_for_date(
        &self,
        guild_id: GuildId,
        game_date: &str,
    ) -> Result<FirstGamePoolBalances, P::Error> {
        self.port.first_game_pool_previews(
            guild_id,
            self.regular_seed_amount,
            game_date,
            self.first_game_daily_amount,
        )
    }

    pub fn settle_pending(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
        mode: BettingMode,
        winning_team: BettingTeam,
        bets: &[PendingBet],
    ) -> Result<SettlementExecution, P::Error> {
        let has_real_winner = bets
            .iter()
            .any(|bet| bet.team == winning_team && bet.effective_amount() > 0);
        let settled_seed = self.port.settle_seed_atomic(
            guild_id,
            pending_match_id,
            mode,
            has_real_winner.then_some(winning_team),
        )?;
        let distribution = calculate_settlement(mode, winning_team, bets, settled_seed.routed);
        Ok(SettlementExecution {
            settled_seed,
            distribution,
        })
    }

    pub fn abort_pending(
        &self,
        guild_id: GuildId,
        pending_match_id: i64,
    ) -> Result<SeedAbort, P::Error> {
        self.port.abort_seed_atomic(guild_id, pending_match_id)
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingBet {
    pub bet_id: i64,
    pub user_id: UserId,
    pub team: BettingTeam,
    pub amount: i64,
    pub leverage: i64,
}

impl PendingBet {
    #[must_use]
    pub fn effective_amount(self) -> i64 {
        self.amount
            .max(0)
            .saturating_mul(if self.leverage > 0 { self.leverage } else { 1 })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WinnerPayout {
    pub bet_id: i64,
    pub user_id: UserId,
    pub team: BettingTeam,
    pub amount: i64,
    pub effective_amount: i64,
    pub payout: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LosingBet {
    pub bet_id: i64,
    pub user_id: UserId,
    pub team: BettingTeam,
    pub amount: i64,
    pub effective_amount: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettlementDistribution {
    pub winners: Vec<WinnerPayout>,
    pub losers: Vec<LosingBet>,
    pub seed_returned: i64,
}

impl SettlementDistribution {
    #[must_use]
    pub fn total_payout(&self) -> i64 {
        self.winners
            .iter()
            .fold(0_i64, |total, winner| total.saturating_add(winner.payout))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementExecution {
    pub settled_seed: SeedSettlement,
    pub distribution: SettlementDistribution,
}

#[must_use]
pub fn normalize_seed_for_mode(mode: BettingMode, mut seed: SeedSplit) -> SeedSplit {
    if mode == BettingMode::House && (seed.radiant != 0 || seed.dire != 0) {
        seed.bonus = seed
            .bonus
            .saturating_add(seed.radiant)
            .saturating_add(seed.dire);
        seed.radiant = 0;
        seed.dire = 0;
    }
    seed
}

#[must_use]
pub fn calculate_settlement(
    mode: BettingMode,
    winning_team: BettingTeam,
    bets: &[PendingBet],
    seed: SeedSplit,
) -> SettlementDistribution {
    let seed = normalize_seed_for_mode(mode, seed);
    match mode {
        BettingMode::Pool => calculate_pool_settlement(winning_team, bets, seed),
        BettingMode::House => calculate_house_settlement(winning_team, bets, seed.bonus),
    }
}

fn calculate_pool_settlement(
    winning_team: BettingTeam,
    bets: &[PendingBet],
    seed: SeedSplit,
) -> SettlementDistribution {
    let mut distribution = SettlementDistribution::default();
    let total_pool = bets.iter().fold(0_i128, |total, bet| {
        total + i128::from(bet.effective_amount())
    });
    let winner_pool = bets
        .iter()
        .filter(|bet| bet.team == winning_team)
        .fold(0_i128, |total, bet| {
            total + i128::from(bet.effective_amount())
        });
    if winner_pool <= 0 {
        distribution.losers = bets.iter().copied().map(losing_bet).collect();
        distribution.seed_returned = seed.radiant.saturating_add(seed.dire);
        return distribution;
    }

    let (winning_seed, losing_seed) = match winning_team {
        BettingTeam::Radiant => (seed.radiant, seed.dire),
        BettingTeam::Dire => (seed.dire, seed.radiant),
    };
    distribution.seed_returned = winning_seed;
    let payout_pool = total_pool + i128::from(losing_seed.max(0));
    let mut bets_by_user = BTreeMap::<UserId, Vec<(usize, PendingBet)>>::new();
    for (index, bet) in bets.iter().copied().enumerate() {
        if bet.team == winning_team {
            bets_by_user
                .entry(bet.user_id)
                .or_default()
                .push((index, bet));
        } else {
            distribution.losers.push(losing_bet(bet));
        }
    }
    let mut winners_by_index = Vec::new();
    for (user_id, user_bets) in bets_by_user {
        let user_effective = user_bets.iter().fold(0_i128, |total, (_, bet)| {
            total + i128::from(bet.effective_amount())
        });
        let user_payout = div_ceil_nonnegative(user_effective * payout_pool, winner_pool);
        let mut allocated = 0_i64;
        for (position, (source_index, bet)) in user_bets.iter().copied().enumerate() {
            let payout = if position + 1 == user_bets.len() {
                user_payout.saturating_sub(allocated)
            } else if user_effective == 0 {
                0
            } else {
                let share = clamp_i128_to_i64(
                    i128::from(user_payout) * i128::from(bet.effective_amount()) / user_effective,
                );
                allocated = allocated.saturating_add(share);
                share
            };
            winners_by_index.push((
                source_index,
                WinnerPayout {
                    bet_id: bet.bet_id,
                    user_id,
                    team: bet.team,
                    amount: bet.amount,
                    effective_amount: bet.effective_amount(),
                    payout,
                },
            ));
        }
    }
    winners_by_index.sort_by_key(|(index, _)| *index);
    distribution.winners = winners_by_index
        .into_iter()
        .map(|(_, winner)| winner)
        .collect();
    distribution
}

fn calculate_house_settlement(
    winning_team: BettingTeam,
    bets: &[PendingBet],
    seed_bonus: i64,
) -> SettlementDistribution {
    let mut distribution = SettlementDistribution::default();
    let mut user_order = Vec::<UserId>::new();
    let mut winning_effective = BTreeMap::<UserId, i64>::new();
    for bet in bets.iter().copied() {
        if bet.team == winning_team && bet.effective_amount() > 0 {
            if !winning_effective.contains_key(&bet.user_id) {
                user_order.push(bet.user_id);
            }
            let total = winning_effective.entry(bet.user_id).or_default();
            *total = total.saturating_add(bet.effective_amount());
        } else if bet.team != winning_team {
            distribution.losers.push(losing_bet(bet));
        }
    }
    let total_winning = winning_effective
        .values()
        .fold(0_i64, |total, amount| total.saturating_add(*amount));
    if total_winning <= 0 {
        distribution.seed_returned = seed_bonus.max(0);
        return distribution;
    }

    let mut bonus_by_user = BTreeMap::<UserId, i64>::new();
    let mut allocated = 0_i64;
    for (index, user_id) in user_order.iter().copied().enumerate() {
        let bonus = if index + 1 == user_order.len() {
            seed_bonus.max(0).saturating_sub(allocated)
        } else {
            let effective = winning_effective.get(&user_id).copied().unwrap_or(0);
            let share = clamp_i128_to_i64(
                i128::from(seed_bonus.max(0)) * i128::from(effective) / i128::from(total_winning),
            );
            allocated = allocated.saturating_add(share);
            share
        };
        bonus_by_user.insert(user_id, bonus);
    }

    let mut positions_by_user = BTreeMap::<UserId, Vec<(usize, PendingBet)>>::new();
    for (index, bet) in bets.iter().copied().enumerate() {
        if bet.team == winning_team && bet.effective_amount() > 0 {
            positions_by_user
                .entry(bet.user_id)
                .or_default()
                .push((index, bet));
        }
    }
    let mut winners_by_index = Vec::new();
    for user_id in user_order {
        let user_bets = positions_by_user.remove(&user_id).unwrap_or_default();
        let user_effective = winning_effective.get(&user_id).copied().unwrap_or(0);
        let user_bonus = bonus_by_user.get(&user_id).copied().unwrap_or(0);
        let mut allocated_bonus = 0_i64;
        for (position, (source_index, bet)) in user_bets.iter().copied().enumerate() {
            let entry_bonus = if position + 1 == user_bets.len() {
                user_bonus.saturating_sub(allocated_bonus)
            } else if user_effective == 0 {
                0
            } else {
                let share = clamp_i128_to_i64(
                    i128::from(user_bonus) * i128::from(bet.effective_amount())
                        / i128::from(user_effective),
                );
                allocated_bonus = allocated_bonus.saturating_add(share);
                share
            };
            winners_by_index.push((
                source_index,
                WinnerPayout {
                    bet_id: bet.bet_id,
                    user_id,
                    team: bet.team,
                    amount: bet.amount,
                    effective_amount: bet.effective_amount(),
                    payout: bet
                        .effective_amount()
                        .saturating_mul(2)
                        .saturating_add(entry_bonus),
                },
            ));
        }
    }
    winners_by_index.sort_by_key(|(index, _)| *index);
    distribution.winners = winners_by_index
        .into_iter()
        .map(|(_, winner)| winner)
        .collect();
    distribution
}

fn losing_bet(bet: PendingBet) -> LosingBet {
    LosingBet {
        bet_id: bet.bet_id,
        user_id: bet.user_id,
        team: bet.team,
        amount: bet.amount,
        effective_amount: bet.effective_amount(),
    }
}

fn div_ceil_nonnegative(numerator: i128, denominator: i128) -> i64 {
    if numerator <= 0 || denominator <= 0 {
        0
    } else {
        clamp_i128_to_i64((numerator + denominator - 1) / denominator)
    }
}

fn clamp_i128_to_i64(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BetTotals {
    pub radiant: i64,
    pub dire: i64,
}

impl BetTotals {
    #[must_use]
    pub const fn for_team(self, team: BettingTeam) -> i64 {
        match team {
            BettingTeam::Radiant => self.radiant,
            BettingTeam::Dire => self.dire,
        }
    }

    #[must_use]
    pub const fn total(self) -> i64 {
        self.radiant.saturating_add(self.dire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BetProjection {
    pub payout: i64,
    pub odds_to_one: f64,
    pub bonus_share: i64,
}

impl BetProjection {
    #[must_use]
    pub fn render(self, mode: BettingMode, team: BettingTeam) -> String {
        let team = team_label(team);
        match mode {
            BettingMode::Pool => format!(
                "📊 If {team} wins: ~{} {JOPACOIN_EMOTE} ({:.2}:1)",
                self.payout, self.odds_to_one
            ),
            BettingMode::House if self.bonus_share > 0 => format!(
                "If {team} wins: {} {JOPACOIN_EMOTE} (1:1 + ~{} bonus)",
                self.payout, self.bonus_share
            ),
            BettingMode::House => {
                format!("If {team} wins: {} {JOPACOIN_EMOTE} (1:1)", self.payout)
            }
        }
    }
}

#[must_use]
pub fn project_my_bet(
    mode: BettingMode,
    team: BettingTeam,
    my_effective_amount: i64,
    totals: BetTotals,
    seed: SeedSplit,
) -> Option<BetProjection> {
    let my_effective_amount = my_effective_amount.max(0);
    let team_total = totals.for_team(team);
    if my_effective_amount == 0 || team_total <= 0 {
        return None;
    }
    match mode {
        BettingMode::Pool => {
            let against = match team {
                BettingTeam::Radiant => seed.dire,
                BettingTeam::Dire => seed.radiant,
            };
            let seeded_pool = totals.total().saturating_add(against.max(0));
            let payout = clamp_i128_to_i64(
                i128::from(seeded_pool) * i128::from(my_effective_amount) / i128::from(team_total),
            );
            Some(BetProjection {
                payout,
                odds_to_one: seeded_pool as f64 / team_total as f64 - 1.0,
                bonus_share: 0,
            })
        }
        BettingMode::House => {
            let bonus_share = clamp_i128_to_i64(
                i128::from(seed.bonus.max(0)) * i128::from(my_effective_amount)
                    / i128::from(team_total),
            );
            Some(BetProjection {
                payout: my_effective_amount
                    .saturating_mul(2)
                    .saturating_add(bonus_share),
                odds_to_one: 1.0,
                bonus_share,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BettingField {
    pub name: String,
    pub value: String,
}

#[must_use]
pub fn betting_field(mode: BettingMode, totals: BetTotals, seed: SeedSplit) -> BettingField {
    let (name, value) = format_betting_display(
        totals.radiant,
        totals.dire,
        mode.as_str(),
        BettingDisplayOptions {
            seed_radiant: seed.radiant,
            seed_dire: seed.dire,
            seed_bonus: seed.bonus,
            ..BettingDisplayOptions::default()
        },
    );
    BettingField { name, value }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetsOverview {
    pub title: String,
    pub current_odds: String,
}

#[must_use]
pub fn bets_overview(
    pending_match_id: i64,
    bet_count: usize,
    mode: BettingMode,
    totals: BetTotals,
    seed: SeedSplit,
) -> BetsOverview {
    let mode_label = match mode {
        BettingMode::Pool => "Pool",
        BettingMode::House => "House",
    };
    let current_odds = match mode {
        BettingMode::Pool => {
            let (radiant, dire) = calculate_pool_odds(
                totals.radiant,
                totals.dire,
                PoolOddsSeeds {
                    seed_radiant: seed.radiant,
                    seed_dire: seed.dire,
                },
            );
            let radiant = format_overview_odds(radiant);
            let dire = format_overview_odds(dire);
            let mut value = format!(
                "🟢 Radiant: {} {JOPACOIN_EMOTE} ({radiant}) | 🔴 Dire: {} {JOPACOIN_EMOTE} ({dire})",
                totals.radiant, totals.dire
            );
            if seed.radiant != 0 || seed.dire != 0 {
                value.push_str(&format!(
                    "\nSeed: Radiant {} {JOPACOIN_EMOTE} | Dire {} {JOPACOIN_EMOTE}",
                    seed.radiant, seed.dire
                ));
            }
            value
        }
        BettingMode::House => {
            let seed = normalize_seed_for_mode(mode, seed);
            let mut value = format!(
                "🟢 Radiant: {} {JOPACOIN_EMOTE} (1:1) | 🔴 Dire: {} {JOPACOIN_EMOTE} (1:1)",
                totals.radiant, totals.dire
            );
            if seed.bonus != 0 {
                value.push_str(&format!("\nWinner bonus: {} {JOPACOIN_EMOTE}", seed.bonus));
            }
            value
        }
    };
    BetsOverview {
        title: format!("📊 Match #{pending_match_id} — {mode_label} Bets ({bet_count} bets)"),
        current_odds,
    }
}

fn format_overview_odds(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.2}x"))
}

const fn team_label(team: BettingTeam) -> &'static str {
    match team {
        BettingTeam::Radiant => "Radiant",
        BettingTeam::Dire => "Dire",
    }
}

pub const RETIRED_BETTING_EXTENSIONS: [&str; 2] = ["commands.roll", "commands.russianroulette"];
pub const REQUIRED_SOCIAL_EXTENSION: &str = "commands.mafia";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtensionPolicyReport {
    pub retired_loaded: Vec<String>,
    pub required_missing: bool,
}

impl ExtensionPolicyReport {
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.retired_loaded.is_empty() && !self.required_missing
    }
}

#[must_use]
pub fn inspect_extension_manifest<'a>(
    extensions: impl IntoIterator<Item = &'a str>,
) -> ExtensionPolicyReport {
    let extensions = extensions.into_iter().collect::<Vec<_>>();
    ExtensionPolicyReport {
        retired_loaded: RETIRED_BETTING_EXTENSIONS
            .into_iter()
            .filter(|retired| extensions.contains(retired))
            .map(str::to_owned)
            .collect(),
        required_missing: !extensions.contains(&REQUIRED_SOCIAL_EXTENSION),
    }
}

#[cfg(test)]
#[path = "dota_bet_seed/tests.rs"]
mod tests;
