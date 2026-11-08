//! Discord-neutral Dota betting-seed split and display policy.

use cama_db::dota_bet_seed_repository::{BettingMode, SeedSplit};
use cama_domain::formatting::{JOPACOIN_EMOTE, PoolOddsSeeds, calculate_pool_odds};

/// Only the accumulated daily lobby allocation is reserved for participants;
/// regular seeds and contributed pots retain their existing betting behavior.
#[must_use]
pub fn betting_seed_without_lobby_bonus(
    seed: SeedSplit,
    first_game_reserved: i64,
    mode: BettingMode,
) -> SeedSplit {
    if first_game_reserved <= 0 {
        return seed;
    }
    cama_db::dota_bet_seed_repository::split_seed(
        seed.total().saturating_sub(first_game_reserved).max(0),
        mode,
    )
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BetTotals {
    pub radiant: i64,
    pub dire: i64,
}

impl BetTotals {
    #[must_use]
    pub const fn total(self) -> i64 {
        self.radiant.saturating_add(self.dire)
    }
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

#[cfg(test)]
#[path = "dota_bet_seed/tests.rs"]
mod tests;
