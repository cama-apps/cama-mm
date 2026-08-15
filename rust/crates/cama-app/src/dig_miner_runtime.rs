//! Cohesive application service for `/dig miner`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cama_db::dig_miner_runtime::{
    DigMinerAllocation, DigMinerAutoBuyUpdate, DigMinerMutationStatus, DigMinerRuntimeRepository,
    DigMinerRuntimeRepositoryError, DigMinerSnapshot, DigMinerTunnelSnapshot,
};
use cama_domain::dig_stats::{MinerStats, miner_stat_effects};
use serde_json::Value;
use thiserror::Error;

use crate::dig_service::DIG_STARTING_STAT_POINTS;
use crate::dig_social_runtime::FastrandDigSocialEntropy;
use crate::dig_tunnel_naming::{DigTunnelNamingService, TunnelNameEntropy};

pub const MINER_BACKSTORY_MAX_LENGTH: usize = 500;
pub use crate::dig_service::MINER_RESPEC_COST;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigMinerStats {
    pub strength: i64,
    pub smarts: i64,
    pub stamina: i64,
    pub stat_points: i64,
    pub spent_points: i64,
    pub unspent_points: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigMinerEffects {
    pub advance_min_bonus: u64,
    pub advance_max_bonus: u64,
    pub cave_in_reduction: f64,
    pub cooldown_multiplier: f64,
    pub paid_cost_multiplier: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigMinerAutoBuy {
    pub torch: bool,
    pub hard_hat: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigMinerProfile {
    pub tunnel_name: String,
    pub backstory: String,
    pub stats: DigMinerStats,
    pub effects: DigMinerEffects,
    pub awarded_bosses: Vec<i64>,
    pub auto_buy: DigMinerAutoBuy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigMinerRespecResult {
    pub cost: i64,
    pub returned_points: i64,
    pub stats: DigMinerStats,
    pub effects: DigMinerEffects,
    pub action_id: i64,
}

#[derive(Debug, Error)]
pub enum DigMinerRuntimeError {
    #[error("You need to register first. Use /player register.")]
    MissingPlayer,
    #[error("Your miner backstory is already set and cannot be changed.")]
    BackstoryLocked,
    #[error("Provide a backstory to lock in.")]
    MissingBackstory,
    #[error("S stats cannot be negative.")]
    NegativeStats,
    #[error("Spend at least one point.")]
    EmptyAllocation,
    #[error("That spends {requested} points, but you only have {available} unspent.")]
    InsufficientPoints { requested: i64, available: i64 },
    #[error("Choose at least one auto-buy setting to update.")]
    MissingAutoBuySetting,
    #[error("You don't have any allocated S points to reset.")]
    NoAllocatedPoints,
    #[error("You need 50 JC to respec your S points.")]
    InsufficientRespecBalance,
    #[error("That miner profile changed while this action was processing. Try again.")]
    Conflict,
    #[error(transparent)]
    Repository(#[from] DigMinerRuntimeRepositoryError),
    #[error("invalid Dig miner state: {0}")]
    InvalidState(String),
}

#[derive(Clone, Debug)]
pub struct DigMinerRuntimeService {
    path: PathBuf,
    repository: DigMinerRuntimeRepository,
}

impl DigMinerRuntimeService {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            repository: DigMinerRuntimeRepository::new(&path),
            path,
        }
    }

    pub fn profile(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.profile_with_entropy(discord_id, guild_id, now, &mut entropy)
    }

    pub fn profile_with_entropy(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        entropy: &mut impl TunnelNameEntropy,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let snapshot = self.ensure_profile(discord_id, guild_id, now, entropy)?;
        profile_from_snapshot(&snapshot)
    }

    pub fn set_backstory(
        &self,
        discord_id: i64,
        guild_id: i64,
        backstory: &str,
        now: i64,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.set_backstory_with_entropy(discord_id, guild_id, backstory, now, &mut entropy)
    }

    pub fn set_backstory_with_entropy(
        &self,
        discord_id: i64,
        guild_id: i64,
        backstory: &str,
        now: i64,
        entropy: &mut impl TunnelNameEntropy,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let snapshot = self.ensure_profile(discord_id, guild_id, now, entropy)?;
        let tunnel = snapshot.tunnel.as_ref().ok_or_else(|| {
            DigMinerRuntimeError::InvalidState("missing ensured tunnel".to_owned())
        })?;
        if !tunnel.miner_about.trim().is_empty() {
            return Err(DigMinerRuntimeError::BackstoryLocked);
        }
        let sanitized =
            crate::dig_service::sanitize_backstory(backstory, MINER_BACKSTORY_MAX_LENGTH);
        if sanitized.is_empty() {
            return Err(DigMinerRuntimeError::MissingBackstory);
        }
        let outcome = self.repository.lock_backstory(
            discord_id,
            guild_id,
            &tunnel.miner_about,
            &sanitized,
        )?;
        match outcome.status {
            DigMinerMutationStatus::Applied => profile_from_snapshot(&outcome.snapshot),
            DigMinerMutationStatus::Locked => Err(DigMinerRuntimeError::BackstoryLocked),
            DigMinerMutationStatus::Conflict => Err(DigMinerRuntimeError::Conflict),
            DigMinerMutationStatus::MissingPlayer => Err(DigMinerRuntimeError::MissingPlayer),
            status => Err(unexpected_status("backstory", status)),
        }
    }

    pub fn allocate_stats(
        &self,
        discord_id: i64,
        guild_id: i64,
        allocation: DigMinerAllocation,
        now: i64,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.allocate_stats_with_entropy(discord_id, guild_id, allocation, now, &mut entropy)
    }

    pub fn allocate_stats_with_entropy(
        &self,
        discord_id: i64,
        guild_id: i64,
        allocation: DigMinerAllocation,
        now: i64,
        entropy: &mut impl TunnelNameEntropy,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let snapshot = self.ensure_profile(discord_id, guild_id, now, entropy)?;
        if allocation.strength < 0 || allocation.smarts < 0 || allocation.stamina < 0 {
            return Err(DigMinerRuntimeError::NegativeStats);
        }
        let requested = allocation.total();
        if requested == 0 {
            return Err(DigMinerRuntimeError::EmptyAllocation);
        }
        let tunnel = snapshot.tunnel.as_ref().ok_or_else(|| {
            DigMinerRuntimeError::InvalidState("missing ensured tunnel".to_owned())
        })?;
        let available = stats_from_tunnel(tunnel).unspent_points;
        if requested > available {
            return Err(DigMinerRuntimeError::InsufficientPoints {
                requested,
                available,
            });
        }
        let outcome = self
            .repository
            .allocate_stats(discord_id, guild_id, tunnel, allocation)?;
        match outcome.status {
            DigMinerMutationStatus::Applied => profile_from_snapshot(&outcome.snapshot),
            DigMinerMutationStatus::InsufficientPoints => {
                let available = outcome
                    .snapshot
                    .tunnel
                    .as_ref()
                    .map_or(0, |tunnel| stats_from_tunnel(tunnel).unspent_points);
                Err(DigMinerRuntimeError::InsufficientPoints {
                    requested,
                    available,
                })
            }
            DigMinerMutationStatus::Conflict => Err(DigMinerRuntimeError::Conflict),
            status => Err(unexpected_status("stat allocation", status)),
        }
    }

    pub fn set_auto_buy(
        &self,
        discord_id: i64,
        guild_id: i64,
        update: DigMinerAutoBuyUpdate,
        now: i64,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.set_auto_buy_with_entropy(discord_id, guild_id, update, now, &mut entropy)
    }

    pub fn set_auto_buy_with_entropy(
        &self,
        discord_id: i64,
        guild_id: i64,
        update: DigMinerAutoBuyUpdate,
        now: i64,
        entropy: &mut impl TunnelNameEntropy,
    ) -> Result<DigMinerProfile, DigMinerRuntimeError> {
        if update.torch.is_none() && update.hard_hat.is_none() {
            return Err(DigMinerRuntimeError::MissingAutoBuySetting);
        }
        self.ensure_profile(discord_id, guild_id, now, entropy)?;
        let outcome = self.repository.set_auto_buy(discord_id, guild_id, update)?;
        match outcome.status {
            DigMinerMutationStatus::Applied => profile_from_snapshot(&outcome.snapshot),
            DigMinerMutationStatus::Conflict => Err(DigMinerRuntimeError::Conflict),
            status => Err(unexpected_status("auto-buy", status)),
        }
    }

    pub fn respec(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigMinerRespecResult, DigMinerRuntimeError> {
        let snapshot = self.repository.snapshot(discord_id, guild_id)?;
        if !snapshot.player_exists {
            return Err(DigMinerRuntimeError::MissingPlayer);
        }
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Err(DigMinerRuntimeError::NoAllocatedPoints);
        };
        if stats_from_tunnel(tunnel).spent_points <= 0 {
            return Err(DigMinerRuntimeError::NoAllocatedPoints);
        }
        let outcome = self
            .repository
            .respec(discord_id, guild_id, MINER_RESPEC_COST, now)?;
        match outcome.status {
            DigMinerMutationStatus::Applied => {
                let profile = profile_from_snapshot(&outcome.snapshot)?;
                Ok(DigMinerRespecResult {
                    cost: MINER_RESPEC_COST,
                    returned_points: outcome.returned_points,
                    stats: profile.stats,
                    effects: profile.effects,
                    action_id: outcome.action_id.ok_or_else(|| {
                        DigMinerRuntimeError::InvalidState("missing respec action id".to_owned())
                    })?,
                })
            }
            DigMinerMutationStatus::NoAllocatedPoints => {
                Err(DigMinerRuntimeError::NoAllocatedPoints)
            }
            DigMinerMutationStatus::InsufficientBalance => {
                Err(DigMinerRuntimeError::InsufficientRespecBalance)
            }
            DigMinerMutationStatus::Conflict => Err(DigMinerRuntimeError::Conflict),
            status => Err(unexpected_status("respec", status)),
        }
    }

    fn ensure_profile(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        entropy: &mut impl TunnelNameEntropy,
    ) -> Result<DigMinerSnapshot, DigMinerRuntimeError> {
        let snapshot = self.repository.snapshot(discord_id, guild_id)?;
        if !snapshot.player_exists {
            return Err(DigMinerRuntimeError::MissingPlayer);
        }
        if snapshot.tunnel.is_some() {
            return Ok(snapshot);
        }
        let name = DigTunnelNamingService::new(entropy).generate_tunnel_name();
        let outcome = self
            .repository
            .ensure_tunnel(discord_id, guild_id, &name, now)?;
        match outcome.status {
            DigMinerMutationStatus::Applied => Ok(outcome.snapshot),
            DigMinerMutationStatus::MissingPlayer => Err(DigMinerRuntimeError::MissingPlayer),
            status => Err(unexpected_status("profile creation", status)),
        }
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.path
    }
}

fn profile_from_snapshot(
    snapshot: &DigMinerSnapshot,
) -> Result<DigMinerProfile, DigMinerRuntimeError> {
    let tunnel = snapshot
        .tunnel
        .as_ref()
        .ok_or_else(|| DigMinerRuntimeError::InvalidState("missing profile tunnel".to_owned()))?;
    let stats = stats_from_tunnel(tunnel);
    Ok(DigMinerProfile {
        tunnel_name: tunnel.tunnel_name.clone(),
        backstory: tunnel.miner_about.clone(),
        stats,
        effects: effects_from_stats(stats),
        awarded_bosses: stat_boss_awards(tunnel),
        auto_buy: DigMinerAutoBuy {
            torch: tunnel.auto_buy_torch,
            hard_hat: tunnel.auto_buy_hard_hat,
        },
    })
}

fn stats_from_tunnel(tunnel: &DigMinerTunnelSnapshot) -> DigMinerStats {
    let strength = tunnel.stat_strength.max(0);
    let smarts = tunnel.stat_smarts.max(0);
    let stamina = tunnel.stat_stamina.max(0);
    let stat_points = tunnel.stat_points.max(DIG_STARTING_STAT_POINTS);
    let spent_points = strength.saturating_add(smarts).saturating_add(stamina);
    DigMinerStats {
        strength,
        smarts,
        stamina,
        stat_points,
        spent_points,
        unspent_points: stat_points.saturating_sub(spent_points).max(0),
    }
}

fn effects_from_stats(stats: DigMinerStats) -> DigMinerEffects {
    let effects = miner_stat_effects(
        MinerStats::new(stats.strength, stats.smarts, stats.stamina)
            .expect("normalized miner stats are non-negative"),
    );
    DigMinerEffects {
        advance_min_bonus: effects.advance_min_bonus,
        advance_max_bonus: effects.advance_max_bonus,
        cave_in_reduction: effects.cave_in_reduction,
        cooldown_multiplier: effects.cooldown_multiplier,
        paid_cost_multiplier: effects.paid_cost_multiplier,
    }
}

fn stat_boss_awards(tunnel: &DigMinerTunnelSnapshot) -> Vec<i64> {
    let Some(raw) = tunnel
        .stat_boss_awards_json
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    else {
        return Vec::new();
    };
    let Ok(decoded) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let values = if let Some(object) = decoded.as_object() {
        if object
            .get("prestige_level")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            != tunnel.prestige_level
        {
            return Vec::new();
        }
        object.get("awards").and_then(Value::as_array)
    } else if tunnel.prestige_level == 0 {
        decoded.as_array()
    } else {
        None
    };
    values
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn unexpected_status(context: &str, status: DigMinerMutationStatus) -> DigMinerRuntimeError {
    DigMinerRuntimeError::InvalidState(format!("unexpected {context} status {status:?}"))
}

#[cfg(test)]
#[path = "dig_miner_runtime/tests.rs"]
mod tests;
