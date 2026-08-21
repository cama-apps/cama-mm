//! Typed manashop buff policy and hostile-loss orchestration.
//!
//! This module is transport-neutral: Python remains the live Discord command
//! and migration authority. Durable state is delegated to the existing-schema
//! SQLite adapter, while protection and transfer effects are explicit ports so
//! failed delivery can release reserved Blood Pact capacity.

use std::collections::BTreeSet;

use cama_db::manashop_rework_repository::{
    BloodPactClaim, BuffData, BuffRecord, DarkBargainSettlement, GrantBuffRequest,
    ManashopRepository, ManashopRepositoryError,
};
use cama_domain::economy_scaling::{DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_minigame_jc_delta};

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

pub trait ProtectionGateway {
    type Error;

    fn apply_hostile_loss(
        &mut self,
        request: HostileLossRequest,
    ) -> Result<HostileLossSettlement, Self::Error>;
}

pub trait BalanceTransferPort {
    type Error;

    fn transfer(
        &mut self,
        from_id: i64,
        to_id: i64,
        guild_id: i64,
        amount: i64,
    ) -> Result<(), Self::Error>;
}

impl BalanceTransferPort for ManashopRepository {
    type Error = ManashopRepositoryError;

    fn transfer(
        &mut self,
        from_id: i64,
        to_id: i64,
        guild_id: i64,
        amount: i64,
    ) -> Result<(), Self::Error> {
        self.transfer_balance_atomic(from_id, to_id, Some(guild_id), amount)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiphonOutcome {
    pub target_id: i64,
    pub amount: i64,
    pub attempted_amount: i64,
    pub absorbed_amount: i64,
    pub event_key: String,
}

/// Route a Swamp siphon through the shared hostile-loss gateway.
pub fn execute_swamp_siphon<G: ProtectionGateway>(
    actor_id: i64,
    guild_id: i64,
    target_id: i64,
    target_balance: i64,
    rolled_amount: i64,
    event_nonce: u128,
    gateway: &mut G,
) -> Result<SiphonOutcome, G::Error> {
    let generated = scale_minigame_jc_delta(
        rolled_amount.clamp(1, 3) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    );
    let attempted = generated.min(target_balance.max(0));
    let event_key = format!("swamp_siphon:{event_nonce:032x}:{target_id}");
    let settlement = gateway.apply_hostile_loss(HostileLossRequest {
        target_id,
        guild_id,
        attempted_loss: attempted,
        kind: "swamp_siphon",
        actor_id,
        event_key: event_key.clone(),
        destination: "player",
        recipient_id: actor_id,
        clamp_to_balance: true,
    })?;
    Ok(SiphonOutcome {
        target_id,
        amount: settlement.applied_loss,
        attempted_amount: attempted,
        absorbed_amount: settlement.absorbed_amount,
        event_key,
    })
}

#[derive(Clone, Debug)]
pub struct BuffService {
    repository: ManashopRepository,
    now: i64,
    minigame_scale: f64,
}

impl BuffService {
    #[must_use]
    pub fn new(repository: ManashopRepository, now: i64) -> Self {
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
    pub const fn repository(&self) -> &ManashopRepository {
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
            cap: Some(150),
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

    pub fn active_for(
        &self,
        discord_id: i64,
        guild_id: i64,
        buff_type: BuffType,
    ) -> Result<Vec<BuffRecord>, ManashopRepositoryError> {
        self.repository
            .active_for(discord_id, Some(guild_id), buff_type.as_str(), self.now)
    }

    pub fn has_pvp_immunity(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        if !self
            .active_for(discord_id, guild_id, BuffType::Counterspell)?
            .is_empty()
            || !self
                .active_for(discord_id, guild_id, BuffType::Sanctuary)?
                .is_empty()
        {
            return Ok(true);
        }
        Ok(!self
            .repository
            .active_targeted_at(
                discord_id,
                Some(guild_id),
                BuffType::Sanctuary.as_str(),
                self.now,
            )?
            .is_empty())
    }

    pub fn has_sanctuary_match_bonus(&self, _discord_id: i64, _guild_id: i64) -> bool {
        false
    }

    pub fn consume_aegis_charge(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        for kind in [BuffType::Aegis, BuffType::FirstAegisToday] {
            for buff in self.active_for(discord_id, guild_id, kind)? {
                if self.repository.consume_buff_atomic(buff.id)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn has_overgrowth(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        Ok(!self
            .active_for(discord_id, guild_id, BuffType::Overgrowth)?
            .is_empty())
    }

    pub fn consume_overgrowth_charge(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        self.repository.consume_data_charge_atomic(
            discord_id,
            Some(guild_id),
            BuffType::Overgrowth.as_str(),
            self.now,
        )
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

    pub fn record_blood_pact_skim(
        &self,
        buff_id: i64,
        current: &BuffData,
        new_total: i64,
    ) -> Result<(), ManashopRepositoryError> {
        let mut data = current.clone();
        data.skimmed_total = Some(new_total);
        self.repository.update_buff_data(buff_id, &data)
    }

    pub fn claim_blood_pact_skim(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
    ) -> Result<Option<BloodPactClaim>, ManashopRepositoryError> {
        self.repository
            .claim_blood_pact_skim_atomic(target_id, Some(guild_id), earning, self.now)
    }

    pub fn apply_blood_pact_with_transfer<T: BalanceTransferPort>(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
        transfer: &mut T,
    ) -> Result<i64, ManashopRepositoryError> {
        let Some((claim, amount)) = self.reserve_scaled_skim(target_id, guild_id, earning)? else {
            return Ok(0);
        };
        if transfer
            .transfer(target_id, claim.skimmer_id, guild_id, amount)
            .is_err()
        {
            self.repository
                .revert_blood_pact_skim(claim.buff_id, amount)?;
            return Ok(0);
        }
        Ok(amount)
    }

    pub fn apply_blood_pact_with_protection<G: ProtectionGateway>(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
        event_nonce: u128,
        gateway: &mut G,
    ) -> Result<i64, ManashopRepositoryError> {
        self.apply_blood_pact_with_protection_key(
            target_id,
            guild_id,
            earning,
            |buff_id| format!("blood-pact:{buff_id}:{event_nonce:032x}"),
            gateway,
        )
    }

    /// Apply a Blood Pact skim through the shared protection gateway with a
    /// caller-owned idempotency key.
    ///
    /// Most callers can use [`Self::apply_blood_pact_with_protection`], which
    /// derives a stable key from an event nonce.  Durable event producers such
    /// as the wheel and Dota settlement already have a domain-specific key;
    /// accepting it here keeps the claim/transfer/revert policy in one place
    /// without forcing those producers to duplicate the reservation logic.
    pub fn apply_blood_pact_with_protection_event_key<G: ProtectionGateway>(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
        event_key: String,
        gateway: &mut G,
    ) -> Result<i64, ManashopRepositoryError> {
        self.apply_blood_pact_with_protection_key(
            target_id,
            guild_id,
            earning,
            move |_| event_key,
            gateway,
        )
    }

    fn apply_blood_pact_with_protection_key<G, F>(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
        event_key: F,
        gateway: &mut G,
    ) -> Result<i64, ManashopRepositoryError>
    where
        G: ProtectionGateway,
        F: FnOnce(i64) -> String,
    {
        let Some((claim, amount)) = self.reserve_scaled_skim(target_id, guild_id, earning)? else {
            return Ok(0);
        };
        let request = HostileLossRequest {
            target_id,
            guild_id,
            attempted_loss: amount,
            kind: "blood_pact",
            actor_id: claim.skimmer_id,
            event_key: event_key(claim.buff_id),
            destination: "player",
            recipient_id: claim.skimmer_id,
            clamp_to_balance: false,
        };
        let Ok(settlement) = gateway.apply_hostile_loss(request) else {
            self.repository
                .revert_blood_pact_skim(claim.buff_id, amount)?;
            return Ok(0);
        };
        if settlement.applied_loss < amount {
            self.repository
                .revert_blood_pact_skim(claim.buff_id, amount - settlement.applied_loss)?;
        }
        Ok(settlement.applied_loss)
    }

    fn reserve_scaled_skim(
        &self,
        target_id: i64,
        guild_id: i64,
        earning: i64,
    ) -> Result<Option<(BloodPactClaim, i64)>, ManashopRepositoryError> {
        if earning <= 0 {
            return Ok(None);
        }
        let Some(claim) = self.claim_blood_pact_skim(target_id, guild_id, earning)? else {
            return Ok(None);
        };
        let mut amount = scale_minigame_jc_delta(claim.amount as f64, self.minigame_scale);
        if amount > claim.amount {
            let extra = self
                .repository
                .claim_blood_pact_extra_atomic(claim.buff_id, amount - claim.amount)?;
            amount = claim.amount + extra;
        }
        Ok(Some((claim, amount)))
    }

    pub fn dark_bargain_debt(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<BuffRecord>, ManashopRepositoryError> {
        Ok(self
            .active_for(discord_id, guild_id, BuffType::DarkBargain)?
            .into_iter()
            .next())
    }

    pub fn settle_due_dark_bargains(
        &self,
    ) -> Result<Vec<DarkBargainSettlement>, ManashopRepositoryError> {
        self.repository.settle_due_dark_bargains(self.now)
    }

    pub fn cleanup_expired(&self) -> Result<usize, ManashopRepositoryError> {
        self.repository.cleanup_expired_buffs(self.now)
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
