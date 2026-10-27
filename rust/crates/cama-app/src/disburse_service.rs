//! Jopacoin Reserve governance policy: distribution methods, recipient
//! selection and settlement math.
//!
//! The SQLite proposal/vote state machine lives in `cama_db::disbursement`;
//! this module owns the storage-independent rules it and the runtime share.

use std::collections::BTreeSet;
use std::fmt;

pub type DiscordId = i64;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DistributionMethod {
    Even,
    Proportional,
    Neediest,
    Stimulus,
    Lottery,
    SocialSecurity,
    Richest,
    Burn,
    NextMatchPot,
    Cancel,
}

impl DistributionMethod {
    pub const ALL: [Self; 10] = [
        Self::Even,
        Self::Proportional,
        Self::Neediest,
        Self::Stimulus,
        Self::Lottery,
        Self::SocialSecurity,
        Self::Richest,
        Self::Burn,
        Self::NextMatchPot,
        Self::Cancel,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Even => "even",
            Self::Proportional => "proportional",
            Self::Neediest => "neediest",
            Self::Stimulus => "stimulus",
            Self::Lottery => "lottery",
            Self::SocialSecurity => "social_security",
            Self::Richest => "richest",
            Self::Burn => "burn",
            Self::NextMatchPot => "next_match_pot",
            Self::Cancel => "cancel",
        }
    }

    /// Parses a stored method code; the inverse of [`Self::as_str`].
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_str() == code)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Even => "Even Split",
            Self::Proportional => "Proportional",
            Self::Neediest => "Neediest First",
            Self::Stimulus => "Stimulus",
            Self::Lottery => "Lottery",
            Self::SocialSecurity => "Social Security",
            Self::Richest => "Richest",
            Self::Burn => "Burn",
            Self::NextMatchPot => "Next Match Pot",
            Self::Cancel => "Cancel",
        }
    }
}

impl fmt::Display for DistributionMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Distribution {
    pub discord_id: DiscordId,
    pub amount: i64,
}

impl Distribution {
    #[must_use]
    pub const fn new(discord_id: DiscordId, amount: i64) -> Self {
        Self { discord_id, amount }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerSnapshot {
    pub discord_id: DiscordId,
    pub username: String,
    pub balance: i64,
    pub wins: u32,
    pub losses: u32,
    pub last_match_at: Option<i64>,
}

impl PlayerSnapshot {
    #[must_use]
    pub const fn games_played(&self) -> u32 {
        self.wins.saturating_add(self.losses)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Debtor {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StimulusCandidate {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamesCandidate {
    pub discord_id: DiscordId,
    pub games_played: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RichestCandidate {
    pub discord_id: DiscordId,
    pub balance: i64,
}

/// Debtors eligible for `even`, `proportional`, and `neediest`, ordered
/// most-indebted first with a stable discord-id tiebreak.
#[must_use]
pub fn select_debtors(players: &[PlayerSnapshot]) -> Vec<Debtor> {
    let mut debtors: Vec<_> = players
        .iter()
        .filter(|player| player.balance < 0)
        .map(|player| Debtor {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .collect();
    debtors.sort_by_key(|debtor| (debtor.balance, debtor.discord_id));
    debtors
}

/// Non-debtors who have actually played, richest first with a stable
/// discord-id tiebreak, with the top three balances excluded.
#[must_use]
pub fn select_stimulus_recipients(players: &[PlayerSnapshot]) -> Vec<StimulusCandidate> {
    let mut eligible: Vec<_> = players
        .iter()
        .filter(|player| player.balance >= 0 && player.games_played() > 0)
        .map(|player| StimulusCandidate {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .collect();
    eligible.sort_by(|left, right| {
        right
            .balance
            .cmp(&left.balance)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    eligible.into_iter().skip(3).collect()
}

fn top_three_non_debtors(players: &[PlayerSnapshot]) -> BTreeSet<DiscordId> {
    let mut non_debtors: Vec<_> = players
        .iter()
        .filter(|player| player.balance >= 0)
        .map(|player| (player.discord_id, player.balance))
        .collect();
    non_debtors.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    non_debtors
        .into_iter()
        .take(3)
        .map(|(discord_id, _)| discord_id)
        .collect()
}

/// Players with games played, most games first with a stable discord-id
/// tiebreak, excluding the three richest non-debtors.
#[must_use]
pub fn select_social_security_recipients(players: &[PlayerSnapshot]) -> Vec<GamesCandidate> {
    let excluded = top_three_non_debtors(players);
    let mut recipients: Vec<_> = players
        .iter()
        .filter(|player| player.games_played() > 0 && !excluded.contains(&player.discord_id))
        .map(|player| GamesCandidate {
            discord_id: player.discord_id,
            games_played: player.games_played(),
        })
        .collect();
    recipients.sort_by(|left, right| {
        right
            .games_played
            .cmp(&left.games_played)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    recipients
}

/// The single richest player (debtors included), lowest discord id on ties.
#[must_use]
pub fn select_richest_candidate(players: &[PlayerSnapshot]) -> Option<RichestCandidate> {
    players
        .iter()
        .map(|player| RichestCandidate {
            discord_id: player.discord_id,
            balance: player.balance,
        })
        .max_by(|left, right| {
            left.balance
                .cmp(&right.balance)
                .then_with(|| right.discord_id.cmp(&left.discord_id))
        })
}

/// Round-robin even split over `debtors` in slice order, capping each player
/// at their outstanding debt.
///
/// When the fund does not divide evenly, remainder coins go to the earliest
/// entries, so with the [`select_debtors`] ordering every caller uses
/// (balance ascending, then discord id ascending) the most indebted players
/// are topped up first. This deterministic remainder allocation intentionally
/// supersedes the legacy runtime behavior, which paid remainders in
/// unspecified SQLite roster order.
#[must_use]
pub fn calculate_even_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 || debtors.is_empty() {
        return Vec::new();
    }
    let mut received = vec![0_i64; debtors.len()];
    let debts: Vec<_> = debtors
        .iter()
        .map(|debtor| debtor.balance.saturating_abs())
        .collect();
    let mut remaining = fund;
    let mut unfilled: Vec<usize> = (0..debtors.len()).collect();

    while remaining > 0 && !unfilled.is_empty() {
        let divisor = i64::try_from(unfilled.len()).unwrap_or(i64::MAX);
        let per_player = (remaining / divisor).max(1);
        let mut distributed = 0_i64;
        let mut still_unfilled = Vec::new();
        for &index in &unfilled {
            let need = debts[index].saturating_sub(received[index]);
            if need <= 0 {
                continue;
            }
            let available_this_round = remaining.saturating_sub(distributed);
            let give = per_player.min(need).min(available_this_round);
            if give > 0 {
                received[index] = received[index].saturating_add(give);
                distributed = distributed.saturating_add(give);
                if received[index] < debts[index] {
                    still_unfilled.push(index);
                }
            }
        }
        if distributed == 0 {
            break;
        }
        remaining -= distributed;
        unfilled = still_unfilled;
    }

    debtors
        .iter()
        .zip(received)
        .filter(|(_, amount)| *amount > 0)
        .map(|(debtor, amount)| Distribution::new(debtor.discord_id, amount))
        .collect()
}

#[must_use]
pub fn calculate_proportional_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 || debtors.is_empty() {
        return Vec::new();
    }
    let total_debt: i128 = debtors
        .iter()
        .map(|debtor| i128::from(debtor.balance.saturating_abs()))
        .sum();
    if total_debt == 0 {
        return calculate_even_distribution(fund, debtors);
    }
    let mut sorted = debtors.to_vec();
    sorted.sort_by_key(|debtor| (debtor.balance, debtor.discord_id));
    let mut remaining = fund;
    let last = sorted.len().saturating_sub(1);
    let mut distributions = Vec::new();
    for (index, debtor) in sorted.into_iter().enumerate() {
        let debt = debtor.balance.saturating_abs();
        let amount = if index == last {
            remaining.min(debt)
        } else {
            let numerator = i128::from(debt) * i128::from(fund);
            let share = i64::try_from(numerator / total_debt).unwrap_or(i64::MAX);
            share.min(debt).min(remaining)
        };
        if amount > 0 {
            distributions.push(Distribution::new(debtor.discord_id, amount));
            remaining -= amount;
        }
    }
    distributions
}

#[must_use]
pub fn calculate_neediest_distribution(fund: i64, debtors: &[Debtor]) -> Vec<Distribution> {
    if fund <= 0 {
        return Vec::new();
    }
    debtors
        .iter()
        .min_by_key(|debtor| (debtor.balance, debtor.discord_id))
        .map(|debtor| {
            vec![Distribution::new(
                debtor.discord_id,
                fund.min(debtor.balance.saturating_abs()),
            )]
        })
        .unwrap_or_default()
}

#[must_use]
pub fn calculate_stimulus_distribution(
    fund: i64,
    eligible: &[StimulusCandidate],
) -> Vec<Distribution> {
    if fund <= 0 || eligible.is_empty() {
        return Vec::new();
    }
    let count = i64::try_from(eligible.len()).unwrap_or(i64::MAX);
    let per_player = fund / count;
    let remainder = usize::try_from(fund % count).unwrap_or(0);
    eligible
        .iter()
        .enumerate()
        .map(|(index, player)| {
            let amount = per_player + i64::from(index < remainder);
            Distribution::new(player.discord_id, amount)
        })
        .filter(|distribution| distribution.amount > 0)
        .collect()
}

#[must_use]
pub fn calculate_lottery_distribution(
    fund: i64,
    players: &[DiscordId],
    random_draw: u64,
) -> Vec<Distribution> {
    if fund <= 0 || players.is_empty() {
        return Vec::new();
    }
    let count = u64::try_from(players.len()).unwrap_or(u64::MAX);
    let winner_index = usize::try_from(random_draw % count).unwrap_or(0);
    vec![Distribution::new(players[winner_index], fund)]
}

#[must_use]
pub fn calculate_social_security_distribution(
    fund: i64,
    players: &[GamesCandidate],
) -> Vec<Distribution> {
    if fund <= 0 || players.is_empty() {
        return Vec::new();
    }
    let total_games: u64 = players
        .iter()
        .map(|player| u64::from(player.games_played))
        .sum();
    if total_games == 0 {
        return Vec::new();
    }
    let mut sorted = players.to_vec();
    sorted.sort_by(|left, right| {
        right
            .games_played
            .cmp(&left.games_played)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    let last = sorted.len().saturating_sub(1);
    let mut remaining = fund;
    let mut distributions = Vec::new();
    for (index, player) in sorted.into_iter().enumerate() {
        let amount = if index == last {
            remaining
        } else {
            let numerator = i128::from(player.games_played) * i128::from(fund);
            i64::try_from(numerator / i128::from(total_games)).unwrap_or(i64::MAX)
        };
        if amount > 0 {
            distributions.push(Distribution::new(player.discord_id, amount));
            remaining -= amount;
        }
    }
    distributions
}

#[must_use]
pub fn calculate_richest_distribution(
    fund: i64,
    richest: Option<RichestCandidate>,
) -> Vec<Distribution> {
    if fund <= 0 {
        return Vec::new();
    }
    richest
        .map(|player| vec![Distribution::new(player.discord_id, fund)])
        .unwrap_or_default()
}

/// Recipient selection plus settlement math for one distribution method over
/// a guild roster snapshot; the single owner of this policy for both the
/// production runtime adapter.
///
/// `lottery_winner` is drawn by the caller from its recently-active roster so
/// the calculation stays deterministic; `None` disburses nothing. `Burn`,
/// `NextMatchPot`, and `Cancel` never pay players and always return no
/// distributions.
#[must_use]
pub fn calculate_method_distributions(
    method: DistributionMethod,
    fund: i64,
    players: &[PlayerSnapshot],
    lottery_winner: Option<DiscordId>,
) -> Vec<Distribution> {
    match method {
        DistributionMethod::Even => calculate_even_distribution(fund, &select_debtors(players)),
        DistributionMethod::Proportional => {
            calculate_proportional_distribution(fund, &select_debtors(players))
        }
        DistributionMethod::Neediest => {
            calculate_neediest_distribution(fund, &select_debtors(players))
        }
        DistributionMethod::Stimulus => {
            calculate_stimulus_distribution(fund, &select_stimulus_recipients(players))
        }
        DistributionMethod::Lottery => lottery_winner
            .map(|winner| calculate_lottery_distribution(fund, &[winner], 0))
            .unwrap_or_default(),
        DistributionMethod::SocialSecurity => calculate_social_security_distribution(
            fund,
            &select_social_security_recipients(players),
        ),
        DistributionMethod::Richest => {
            calculate_richest_distribution(fund, select_richest_candidate(players))
        }
        DistributionMethod::Burn
        | DistributionMethod::NextMatchPot
        | DistributionMethod::Cancel => Vec::new(),
    }
}

#[cfg(test)]
#[path = "disburse_service/tests.rs"]
mod tests;
