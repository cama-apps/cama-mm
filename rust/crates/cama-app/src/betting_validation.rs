//! Betting command validation and post-placement orchestration.
//!
//! Existing-schema placement, balance/debt validation, and automatic blind
//! batches live in `cama_db::betting_service_repository`. This module keeps the
//! Discord-neutral command seam: one mana snapshot is reused for the 10x gate,
//! steady reward, and badge; minted rewards pass through both central and daily
//! controls; confirmation and wager refresh start concurrently; a display
//! failure cannot hide a committed bet.

use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::formatting::JOPACOIN_EMOTE;
use thiserror::Error;

pub type DiscordId = i64;
pub type GuildId = i64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Team {
    Radiant,
    Dire,
}

impl Team {
    const fn key(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    const fn display_name(self) -> &'static str {
        match self {
            Self::Radiant => "Radiant",
            Self::Dire => "Dire",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingMode {
    Pool,
    House,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBetState {
    pub pending_match_id: i64,
    pub betting_mode: BettingMode,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BetManaEffects {
    pub red_10x_leverage: bool,
    pub match_bet_steady_bonus: i64,
    pub badge: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BetCommandRequest<'a> {
    pub guild_id: Option<GuildId>,
    pub user_id: DiscordId,
    pub team: Team,
    pub amount: i64,
    pub leverage: i64,
    pub pending: &'a PendingBetState,
    pub minigame_scale_basis_points: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteadyBonusCredit {
    pub user_id: DiscordId,
    pub guild_id: Option<GuildId>,
    pub amount: i64,
    pub source: &'static str,
    pub actor_id: DiscordId,
    pub related_type: &'static str,
    pub related_id: i64,
    pub reason: &'static str,
    pub base_reward: i64,
    pub adjusted_reward: i64,
}

pub trait BetCommandPort: Sync {
    fn mana_effects(
        &self,
        user_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Result<Option<BetManaEffects>, String>;

    fn place_bet(&self, request: &BetCommandRequest<'_>) -> Result<(), String>;

    fn adjust_daily_reward(&self, guild_id: Option<GuildId>, amount: i64) -> Result<i64, String>;

    fn credit_steady_bonus(&self, credit: &SteadyBonusCredit) -> Result<(), String>;

    fn refresh_wagers(
        &self,
        guild_id: Option<GuildId>,
        pending: &PendingBetState,
    ) -> Result<(), String>;

    fn send_confirmation(&self, content: &str, ephemeral: bool) -> Result<(), String>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BetCommandError {
    #[error("10x leverage is exclusive to Red mana (Mountain) players")]
    RedManaRequired,
    #[error("bet placement failed: {0}")]
    Placement(String),
    #[error("confirmation delivery failed: {0}")]
    Confirmation(String),
    #[error("confirmation worker panicked")]
    ConfirmationWorkerPanic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetCommandOutcome {
    pub confirmation: String,
    pub refresh_failed: bool,
    pub mana_resolutions: usize,
    pub steady_bonus: Option<SteadyBonusCredit>,
}

/// Execute the post-defer command path. Flavor/Neon delivery remains a later
/// runtime adapter and is deliberately outside this bounded contract.
pub fn execute_bet_command(
    port: &dyn BetCommandPort,
    request: BetCommandRequest<'_>,
) -> Result<BetCommandOutcome, BetCommandError> {
    let needs_effects = request.leverage == 10;
    let effects = port
        .mana_effects(request.user_id, request.guild_id)
        .ok()
        .flatten();
    let mana_resolutions = 1;
    if needs_effects
        && !effects
            .as_ref()
            .is_some_and(|effects| effects.red_10x_leverage)
    {
        return Err(BetCommandError::RedManaRequired);
    }
    port.place_bet(&request)
        .map_err(BetCommandError::Placement)?;

    let steady_bonus = effects.as_ref().and_then(|effects| {
        (effects.match_bet_steady_bonus > 0).then(|| {
            let scale = request.minigame_scale_basis_points as f64 / 10_000.0;
            let central = scale_minigame_jc_delta(effects.match_bet_steady_bonus as f64, scale);
            let adjusted = port
                .adjust_daily_reward(request.guild_id, central)
                .unwrap_or(central);
            SteadyBonusCredit {
                user_id: request.user_id,
                guild_id: request.guild_id,
                amount: adjusted,
                source: "mana_reward",
                actor_id: request.user_id,
                related_type: "match_bet_steady_bonus",
                related_id: request.pending.pending_match_id,
                reason: "scaled Green mana bet-placement reward",
                base_reward: effects.match_bet_steady_bonus,
                adjusted_reward: adjusted,
            }
        })
    });
    if let Some(credit) = &steady_bonus
        && credit.amount > 0
    {
        let _ = port.credit_steady_bonus(credit);
    }

    let confirmation = confirmation(&request, effects.as_ref());
    let (refresh_failed, confirmation_result) = std::thread::scope(|scope| {
        let refresh = scope.spawn(|| {
            port.refresh_wagers(request.guild_id, request.pending)
                .is_err()
        });
        let send = scope.spawn(|| port.send_confirmation(&confirmation, true));
        (
            refresh.join().unwrap_or(true),
            send.join()
                .map_err(|_| BetCommandError::ConfirmationWorkerPanic),
        )
    });
    confirmation_result?.map_err(BetCommandError::Confirmation)?;

    Ok(BetCommandOutcome {
        confirmation,
        refresh_failed,
        mana_resolutions,
        steady_bonus,
    })
}

fn confirmation(request: &BetCommandRequest<'_>, effects: Option<&BetManaEffects>) -> String {
    let prefix = effects
        .and_then(|effects| effects.badge.as_deref())
        .map_or(String::new(), |badge| format!("{badge} "));
    let match_note = if request.pending.pending_match_id != 0 {
        format!(" (Match #{})", request.pending.pending_match_id)
    } else {
        String::new()
    };
    let pool_warning = if request.pending.betting_mode == BettingMode::Pool {
        "\n⚠️ Pool mode: odds may shift as more bets come in. Use `/mybets` to check current EV."
    } else {
        ""
    };
    if request.leverage > 1 {
        format!(
            "{prefix}Bet placed{match_note}: {} {JOPACOIN_EMOTE} on {} at {}x leverage (effective: {} {JOPACOIN_EMOTE}).{pool_warning}",
            request.amount,
            request.team.display_name(),
            request.leverage,
            request.amount.saturating_mul(request.leverage),
        )
    } else {
        format!(
            "{prefix}Bet placed{match_note}: {} {JOPACOIN_EMOTE} on {}.{pool_warning}",
            request.amount,
            request.team.display_name(),
        )
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("betting_mode must be 'house' or 'pool'")]
pub struct InvalidBettingMode;

pub fn validate_betting_mode(mode: &str) -> Result<BettingMode, InvalidBettingMode> {
    match mode {
        "pool" => Ok(BettingMode::Pool),
        "house" => Ok(BettingMode::House),
        _ => Err(InvalidBettingMode),
    }
}

#[must_use]
pub const fn team_key(team: Team) -> &'static str {
    team.key()
}

#[cfg(test)]
#[path = "betting_validation/tests.rs"]
mod tests;
