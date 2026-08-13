//! Bankruptcy-recovery policies shared by mana, trivia, and the wheel.
//!
//! Python remains the live command/runtime authority. The money boundary is
//! deliberately atomic: production composition should back it with the
//! existing loan/player transaction rather than reproducing the Python
//! debit-then-credit compensation seam.

use std::collections::{BTreeMap, BTreeSet};

pub const WHITE_BANKRUPT_STIPEND: i64 = 5;
pub const TRIVIA_BANKRUPT_MULTIPLIER: f64 = 2.0;

/// Persistence boundary for bankruptcy stipends.
pub trait BankruptStipendPort {
    type Error;

    fn player_balance(&self, discord_id: i64, guild_id: i64) -> Result<Option<i64>, Self::Error>;

    fn nonprofit_fund(&self, guild_id: i64) -> Result<i64, Self::Error>;

    /// Atomically debit the reserve and credit one existing player.
    fn transfer_stipend_atomic(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        amount: i64,
    ) -> Result<bool, Self::Error>;

    /// Atomically pay a unique ordered batch until the reserve is empty.
    fn distribute_stipends_atomic(
        &mut self,
        discord_ids: &[i64],
        guild_id: i64,
        maximum_each: i64,
    ) -> Result<Vec<(i64, i64)>, Self::Error>;
}

/// Apply the white-mana stipend with the live service's fail-soft semantics.
#[must_use]
pub fn apply_bankrupt_stipend<P: BankruptStipendPort>(
    port: &mut P,
    discord_id: i64,
    guild_id: i64,
    land: &str,
) -> i64 {
    if land != "Plains" || WHITE_BANKRUPT_STIPEND <= 0 {
        return 0;
    }
    let Ok(Some(balance)) = port.player_balance(discord_id, guild_id) else {
        return 0;
    };
    if balance > 0 {
        return 0;
    }
    let Ok(fund) = port.nonprofit_fund(guild_id) else {
        return 0;
    };
    let amount = WHITE_BANKRUPT_STIPEND.min(fund.max(0));
    if amount == 0 {
        return 0;
    }
    match port.transfer_stipend_atomic(discord_id, guild_id, amount) {
        Ok(true) => amount,
        Ok(false) | Err(_) => 0,
    }
}

/// Pay only players freshly assigned Plains by the current `/mana all` call.
/// The caller supplies fresh assignments, so already-assigned self or board
/// retries are represented by an empty input and cannot pay twice.
#[must_use]
pub fn apply_fresh_assignment_stipends<P: BankruptStipendPort>(
    port: &mut P,
    assignments: &[(i64, String)],
    guild_id: i64,
) -> BTreeMap<i64, i64> {
    let mut seen = BTreeSet::new();
    let eligible = assignments
        .iter()
        .filter(|(discord_id, land)| land == "Plains" && seen.insert(*discord_id))
        .map(|(discord_id, _)| *discord_id)
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return BTreeMap::new();
    }
    port.distribute_stipends_atomic(&eligible, guild_id, WHITE_BANKRUPT_STIPEND)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Apply Red mana's 1.5 multiplier first, then the bankruptcy multiplier,
/// truncating after each stage exactly like Python's sequential `int` calls.
#[must_use]
pub fn trivia_bankrupt_reward(base: i64, red_mana: bool) -> i64 {
    let after_mana = if red_mana {
        ((base as f64 * 1.5) as i64).max(1)
    } else {
        base.max(1)
    };
    ((after_mana as f64 * TRIVIA_BANKRUPT_MULTIPLIER) as i64).max(1)
}

#[cfg(test)]
#[path = "bankruptcy_buffs/tests.rs"]
mod tests;
