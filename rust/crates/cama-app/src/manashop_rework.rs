//! Typed manashop buff policy and hostile-loss orchestration.
//!
//! This module is transport-neutral. Durable state is delegated to the existing-schema
//! SQLite adapter, while protection and transfer effects are explicit ports so
//! failed delivery can release reserved Blood Pact capacity.

use std::collections::BTreeSet;

use cama_db::manashop_rework_repository::{
    BuffData, BuffRecord, DarkBargainSettlement, GrantBuffRequest, ManashopBuffGrants,
    ManashopRepository, ManashopRepositoryError,
};
use cama_domain::economy_scaling::DEFAULT_MINIGAME_JC_DELTA_SCALE;

pub const HOURS: i64 = 3_600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuffType {
    Counterspell,
    Reprieve,
    Aegis,
    FirstAegisToday,
    Overgrowth,
    Sanctuary,
    BloodPact,
    DarkBargain,
}

impl BuffType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counterspell => "counterspell",
            Self::Reprieve => "reprieve",
            Self::Aegis => "aegis",
            Self::FirstAegisToday => "first_aegis_today",
            Self::Overgrowth => "overgrowth",
            Self::Sanctuary => "sanctuary",
            Self::BloodPact => "blood_pact",
            Self::DarkBargain => "dark_bargain",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostileLossRequest {
    pub target_id: i64,
    pub guild_id: i64,
    pub attempted_loss: i64,
    pub kind: &'static str,
    pub actor_id: i64,
    pub event_key: String,
    pub destination: &'static str,
    pub recipient_id: i64,
    pub clamp_to_balance: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostileLossSettlement {
    pub attempted_loss: i64,
    pub absorbed_amount: i64,
    pub applied_loss: i64,
}

#[derive(Clone, Debug)]
pub struct BuffService<R = ManashopRepository> {
    repository: R,
    now: i64,
    minigame_scale: f64,
}

impl<R: ManashopBuffGrants> BuffService<R> {
    #[must_use]
    pub fn new(repository: R, now: i64) -> Self {
        Self {
            repository,
            now,
            minigame_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
        }
    }

    #[must_use]
    pub fn with_minigame_scale(mut self, scale: f64) -> Self {
        self.minigame_scale = scale;
        self
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub fn grant_counterspell(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        self.grant(discord_id, guild_id, BuffType::Counterspell, 24, None, None)
    }

    pub fn grant_reprieve(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        self.grant(
            discord_id,
            guild_id,
            BuffType::Reprieve,
            24,
            None,
            Some(&protection_pool(25, 0.5, false, true)),
        )
    }

    pub fn grant_aegis(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        self.grant(
            discord_id,
            guild_id,
            BuffType::Aegis,
            24,
            None,
            Some(&protection_pool(75, 1.0, false, false)),
        )
    }

    pub fn grant_sanctuary(
        &self,
        caster_id: i64,
        guild_id: i64,
        ally_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        let mut data = protection_pool(150, 1.0, true, false);
        data.caster_id = Some(caster_id);
        data.ally_id = Some(ally_id);
        data.protected_user_ids = vec![caster_id, ally_id];
        self.grant(
            caster_id,
            guild_id,
            BuffType::Sanctuary,
            24,
            Some(ally_id),
            Some(&data),
        )
    }

    pub fn grant_overgrowth(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        let data = BuffData {
            charges_remaining: Some(10),
            ..BuffData::default()
        };
        self.repository.refresh_buff_atomic(GrantBuffRequest {
            discord_id,
            guild_id: Some(guild_id),
            buff_type: BuffType::Overgrowth.as_str(),
            target_id: None,
            granted_at: self.now,
            expires_at: self.now + 12 * HOURS,
            data: Some(&data),
        })
    }

    pub fn grant_blood_pact(
        &self,
        caster_id: i64,
        guild_id: i64,
        target_id: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        let data = BuffData {
            skimmed_total: Some(0),
            cap: None,
            skim_rate: Some(0.25),
            ..BuffData::default()
        };
        self.grant(
            caster_id,
            guild_id,
            BuffType::BloodPact,
            24,
            Some(target_id),
            Some(&data),
        )
    }

    /// Book the debt and credit the principal in one transaction.
    ///
    /// A player must never end up owing the bargain without having been paid
    /// it: the default penalty (-1600 JC plus five bankruptcy games) is charged
    /// off the debt buff alone.
    pub fn grant_dark_bargain(
        &self,
        discord_id: i64,
        guild_id: i64,
        amount_due: i64,
        principal: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        let data = BuffData {
            amount_due: Some(amount_due),
            amount_paid: Some(0),
            default_penalty: Some(1_600),
            default_penalty_games: Some(5),
            ..BuffData::default()
        };
        self.repository.grant_dark_bargain_atomic(
            GrantBuffRequest {
                discord_id,
                guild_id: Some(guild_id),
                buff_type: BuffType::DarkBargain.as_str(),
                target_id: None,
                granted_at: self.now,
                expires_at: self.now + 24 * 7 * HOURS,
                data: Some(&data),
            },
            principal,
        )
    }

    fn grant(
        &self,
        discord_id: i64,
        guild_id: i64,
        buff_type: BuffType,
        hours: i64,
        target_id: Option<i64>,
        data: Option<&BuffData>,
    ) -> Result<i64, ManashopRepositoryError> {
        self.repository.grant_buff(GrantBuffRequest {
            discord_id,
            guild_id: Some(guild_id),
            buff_type: buff_type.as_str(),
            target_id,
            granted_at: self.now,
            expires_at: self.now + hours * HOURS,
            data,
        })
    }
}

impl BuffService<ManashopRepository> {
    pub fn active_for(
        &self,
        discord_id: i64,
        guild_id: i64,
        buff_type: BuffType,
    ) -> Result<Vec<BuffRecord>, ManashopRepositoryError> {
        self.repository
            .active_for(discord_id, Some(guild_id), buff_type.as_str(), self.now)
    }

    pub fn blood_pact_skimmer(
        &self,
        target_id: i64,
        guild_id: i64,
    ) -> Result<Option<BuffRecord>, ManashopRepositoryError> {
        Ok(self
            .repository
            .active_targeted_at(
                target_id,
                Some(guild_id),
                BuffType::BloodPact.as_str(),
                self.now,
            )?
            .into_iter()
            .next())
    }

    pub fn blood_pact_targets(
        &self,
        target_ids: &[i64],
        guild_id: i64,
    ) -> Result<BTreeSet<i64>, ManashopRepositoryError> {
        self.repository.active_target_ids(
            target_ids,
            Some(guild_id),
            BuffType::BloodPact.as_str(),
            self.now,
        )
    }

    pub fn settle_due_dark_bargains(
        &self,
    ) -> Result<Vec<DarkBargainSettlement>, ManashopRepositoryError> {
        self.repository.settle_due_dark_bargains(self.now)
    }
}

fn protection_pool(capacity: i64, rate: f64, shared: bool, rolling_retroactive: bool) -> BuffData {
    BuffData {
        capacity: Some(capacity),
        capacity_remaining: Some(capacity),
        rate: Some(rate),
        shared: Some(shared),
        rolling_retroactive: Some(rolling_retroactive),
        ..BuffData::default()
    }
}

#[cfg(test)]
#[path = "manashop_rework/tests.rs"]
mod tests;
