//! Runtime-ready vanity-tax eligibility, persistence, and announcement policy.
//!
//! Discord member snapshots are refreshed off the gateway event loop while
//! member updates continue to arrive on it.  The generation and mutation
//! journal below preserve those newer events when an older snapshot finishes,
//! and a separate I/O mutex serializes repository refreshes with manual admin
//! writes without holding the cache mutex across SQLite operations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use cama_db::vanity_tax_repository::VanityTaxRepository;
use thiserror::Error;

pub const DEFAULT_VANITY_TAX_BASIS_POINTS: i64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanityMember {
    pub discord_id: i64,
    /// A global display name deliberately does not participate in this rule.
    pub nickname: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VanityEligibility {
    ManualTaxation,
    Taxable,
    NicknameExemption,
    Unknown,
}

impl VanityEligibility {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManualTaxation => "manual_taxation",
            Self::Taxable => "taxable",
            Self::NicknameExemption => "nickname_exemption",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum VanityTaxServiceError {
    #[error("vanity-tax persistence failed: {0}")]
    Persistence(String),
}

pub trait VanityTaxPersistence: Send + Sync {
    fn get_enforcements(&self, guild_id: i64) -> Result<BTreeSet<i64>, VanityTaxServiceError>;

    fn set_enforcement(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxServiceError>;
}

impl VanityTaxPersistence for VanityTaxRepository {
    fn get_enforcements(&self, guild_id: i64) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        self.get_vanity_tax_enforcements(Some(guild_id))
            .map_err(|error| VanityTaxServiceError::Persistence(error.to_string()))
    }

    fn set_enforcement(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        self.set_vanity_tax_enforcement(Some(guild_id), discord_id, enforced, actor_id)
            .map_err(|error| VanityTaxServiceError::Persistence(error.to_string()))
    }
}

#[derive(Clone, Debug, Default)]
struct VanityCache {
    known: BTreeMap<i64, BTreeSet<i64>>,
    nickname_taxable: BTreeMap<i64, BTreeSet<i64>>,
    manual_taxable: BTreeMap<i64, BTreeSet<i64>>,
    mutated_members: BTreeMap<i64, BTreeSet<i64>>,
    refresh_generation: BTreeMap<i64, u64>,
}

pub struct VanityTaxService {
    repository: Option<Arc<dyn VanityTaxPersistence>>,
    rate: f64,
    cache: Mutex<VanityCache>,
    io_lock: Mutex<()>,
}

impl fmt::Debug for VanityTaxService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VanityTaxService")
            .field("has_repository", &self.repository.is_some())
            .field("rate", &self.rate)
            .finish_non_exhaustive()
    }
}

impl Default for VanityTaxService {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl VanityTaxService {
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(None, DEFAULT_VANITY_TAX_BASIS_POINTS)
    }

    #[must_use]
    pub fn persistent(repository: Arc<dyn VanityTaxPersistence>) -> Self {
        Self::new(Some(repository), DEFAULT_VANITY_TAX_BASIS_POINTS)
    }

    /// Configure the exact Python floating-point vanity-tax rate.
    #[must_use]
    pub fn with_rate_fraction(
        repository: Option<Arc<dyn VanityTaxPersistence>>,
        rate: f64,
    ) -> Self {
        Self::new_fraction(repository, rate)
    }

    fn new(repository: Option<Arc<dyn VanityTaxPersistence>>, rate_basis_points: i64) -> Self {
        Self::new_fraction(repository, rate_basis_points as f64 / 10_000.0)
    }

    fn new_fraction(repository: Option<Arc<dyn VanityTaxPersistence>>, rate: f64) -> Self {
        Self {
            repository,
            rate: if rate.is_nan() {
                0.5
            } else {
                rate.clamp(0.0, 0.5)
            },
            cache: Mutex::new(VanityCache::default()),
            io_lock: Mutex::new(()),
        }
    }

    /// Begin a Discord member snapshot and return its monotonic generation.
    /// Events recorded after this call are replayed over the snapshot when it
    /// is eventually stored.
    #[must_use]
    pub fn begin_refresh(&self, guild_id: i64) -> u64 {
        let mut cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        cache.mutated_members.remove(&guild_id);
        let generation = cache
            .refresh_generation
            .get(&guild_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        cache.refresh_generation.insert(guild_id, generation);
        generation
    }

    pub fn refresh_guild(
        &self,
        guild_id: i64,
        members: &[VanityMember],
        generation: Option<u64>,
    ) -> Result<bool, VanityTaxServiceError> {
        let mut known = members
            .iter()
            .map(|member| member.discord_id)
            .collect::<BTreeSet<_>>();
        let mut nickname_taxable = members
            .iter()
            .filter(|member| member.nickname.is_none())
            .map(|member| member.discord_id)
            .collect::<BTreeSet<_>>();

        if let Some(repository) = &self.repository {
            let _io = self.io_lock.lock().expect("vanity-tax I/O mutex poisoned");
            let manual = repository.get_enforcements(guild_id)?;
            Ok(self.store_refresh(
                guild_id,
                &mut known,
                &mut nickname_taxable,
                manual,
                generation,
            ))
        } else {
            let manual = self
                .cache
                .lock()
                .expect("vanity-tax cache mutex poisoned")
                .manual_taxable
                .get(&guild_id)
                .cloned()
                .unwrap_or_default();
            Ok(self.store_refresh(
                guild_id,
                &mut known,
                &mut nickname_taxable,
                manual,
                generation,
            ))
        }
    }

    fn store_refresh(
        &self,
        guild_id: i64,
        known: &mut BTreeSet<i64>,
        nickname_taxable: &mut BTreeSet<i64>,
        manual_taxable: BTreeSet<i64>,
        generation: Option<u64>,
    ) -> bool {
        let mut cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        if generation.is_some_and(|candidate| {
            cache.refresh_generation.get(&guild_id).copied() != Some(candidate)
        }) {
            return false;
        }

        let mutated = cache.mutated_members.remove(&guild_id).unwrap_or_default();
        if !mutated.is_empty() {
            let live_known = cache.known.get(&guild_id).cloned().unwrap_or_default();
            let live_taxable = cache
                .nickname_taxable
                .get(&guild_id)
                .cloned()
                .unwrap_or_default();
            for discord_id in mutated {
                if live_known.contains(&discord_id) {
                    known.insert(discord_id);
                    if live_taxable.contains(&discord_id) {
                        nickname_taxable.insert(discord_id);
                    } else {
                        nickname_taxable.remove(&discord_id);
                    }
                } else {
                    known.remove(&discord_id);
                    nickname_taxable.remove(&discord_id);
                }
            }
        }
        cache.known.insert(guild_id, known.clone());
        cache
            .nickname_taxable
            .insert(guild_id, nickname_taxable.clone());
        cache.manual_taxable.insert(guild_id, manual_taxable);
        true
    }

    pub fn update_member(&self, guild_id: i64, discord_id: i64, nickname: Option<&str>) {
        let mut cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        cache.known.entry(guild_id).or_default().insert(discord_id);
        let taxable = cache.nickname_taxable.entry(guild_id).or_default();
        if nickname.is_none() {
            taxable.insert(discord_id);
        } else {
            taxable.remove(&discord_id);
        }
        cache
            .mutated_members
            .entry(guild_id)
            .or_default()
            .insert(discord_id);
    }

    pub fn remove_member(&self, guild_id: i64, discord_id: i64) {
        let mut cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        cache.known.entry(guild_id).or_default().remove(&discord_id);
        cache
            .nickname_taxable
            .entry(guild_id)
            .or_default()
            .remove(&discord_id);
        cache
            .mutated_members
            .entry(guild_id)
            .or_default()
            .insert(discord_id);
    }

    pub fn set_manual_taxation(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
    ) -> Result<(), VanityTaxServiceError> {
        let manual = if let Some(repository) = &self.repository {
            let _io = self.io_lock.lock().expect("vanity-tax I/O mutex poisoned");
            repository.set_enforcement(guild_id, discord_id, enforced, actor_id)?
        } else {
            let mut cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
            let manual = cache.manual_taxable.entry(guild_id).or_default();
            if enforced {
                manual.insert(discord_id);
            } else {
                manual.remove(&discord_id);
            }
            return Ok(());
        };
        self.cache
            .lock()
            .expect("vanity-tax cache mutex poisoned")
            .manual_taxable
            .insert(guild_id, manual);
        Ok(())
    }

    #[must_use]
    pub fn eligibility_status(&self, guild_id: Option<i64>, discord_id: i64) -> VanityEligibility {
        let Some(guild_id) = guild_id else {
            return VanityEligibility::Unknown;
        };
        let cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        if cache
            .manual_taxable
            .get(&guild_id)
            .is_some_and(|members| members.contains(&discord_id))
        {
            return VanityEligibility::ManualTaxation;
        }
        if !cache
            .known
            .get(&guild_id)
            .is_some_and(|members| members.contains(&discord_id))
        {
            return VanityEligibility::Unknown;
        }
        if cache
            .nickname_taxable
            .get(&guild_id)
            .is_some_and(|members| members.contains(&discord_id))
        {
            VanityEligibility::Taxable
        } else {
            VanityEligibility::NicknameExemption
        }
    }

    #[must_use]
    pub fn is_manually_taxed(&self, guild_id: Option<i64>, discord_id: i64) -> bool {
        self.eligibility_status(guild_id, discord_id) == VanityEligibility::ManualTaxation
    }

    #[must_use]
    pub fn taxable_ids(&self, guild_id: Option<i64>) -> BTreeSet<i64> {
        let Some(guild_id) = guild_id else {
            return BTreeSet::new();
        };
        let cache = self.cache.lock().expect("vanity-tax cache mutex poisoned");
        cache
            .nickname_taxable
            .get(&guild_id)
            .into_iter()
            .chain(cache.manual_taxable.get(&guild_id))
            .flatten()
            .copied()
            .collect()
    }

    #[must_use]
    pub fn calculate_tax(&self, discord_id: i64, guild_id: Option<i64>, profit: i64) -> i64 {
        if profit <= 0 || !self.taxable_ids(guild_id).contains(&discord_id) {
            return 0;
        }
        (profit as f64 * self.rate) as i64
    }
}

#[cfg(test)]
#[path = "vanity_tax_service/tests.rs"]
mod tests;
