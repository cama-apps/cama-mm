//! Adapter-neutral orchestration for active mana effects.
//!
//! The Python process remains the production Discord and SQLite authority
//! during the side-by-side migration. This module ports the policy exercised
//! by `tests/test_mana_effects_service.py` behind explicit persistence,
//! entropy, and daily-economy ports. The included in-memory repository applies
//! siphons and tithes atomically, making the money transitions executable
//! without opening the live database.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::ops::Deref;

use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, DailyRewardPolicy, scale_minigame_jc_delta,
};
use cama_domain::mana::ManaEffects;

pub type DiscordId = i64;
pub type GuildId = i64;

/// Python's default hostile-loss eligibility floor.
pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;

/// An opaque, read-only view of a resolved effect value.
///
/// `Deref` deliberately has no `DerefMut` counterpart. Callers can inspect the
/// domain fields but cannot mutate the service snapshot in place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManaEffectsSnapshot(ManaEffects);

impl ManaEffectsSnapshot {
    #[must_use]
    pub fn new(effects: ManaEffects) -> Self {
        Self(effects)
    }

    #[must_use]
    pub fn into_inner(self) -> ManaEffects {
        self.0
    }
}

impl Deref for ManaEffectsSnapshot {
    type Target = ManaEffects;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One persisted mana assignment, including the consumed bit from the same
/// read. Reusing that bit prevents a second, inconsistent repository lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaRecord {
    pub discord_id: DiscordId,
    pub guild_id: GuildId,
    pub land: Option<String>,
    pub color: Option<String>,
    pub assigned_date: String,
    pub consumed: bool,
}

impl ManaRecord {
    #[must_use]
    pub fn new(
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        land: impl Into<String>,
        assigned_date: impl Into<String>,
    ) -> Self {
        let land = land.into();
        Self {
            discord_id,
            guild_id: normalize_guild_id(guild_id),
            color: color_for_land(&land).map(str::to_owned),
            land: Some(land),
            assigned_date: assigned_date.into(),
            consumed: false,
        }
    }
}

#[must_use]
pub const fn normalize_guild_id(guild_id: Option<GuildId>) -> GuildId {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

#[must_use]
pub const fn color_for_land(land: &str) -> Option<&'static str> {
    match land.as_bytes() {
        b"Mountain" => Some("Red"),
        b"Island" => Some("Blue"),
        b"Forest" => Some("Green"),
        b"Plains" => Some("White"),
        b"Swamp" => Some("Black"),
        _ => None,
    }
}

/// Resolve a record against the current 4-AM-Pacific mana-day label.
#[must_use]
pub fn effects_from_record(record: Option<&ManaRecord>, today: &str) -> ManaEffectsSnapshot {
    let Some(record) = record else {
        return ManaEffectsSnapshot::default();
    };
    if record.assigned_date != today || record.consumed {
        return ManaEffectsSnapshot::default();
    }

    let land = record.land.as_deref();
    let color = record
        .color
        .as_deref()
        .or_else(|| land.and_then(color_for_land));
    ManaEffectsSnapshot::new(ManaEffects::for_color(color, land))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryError {
    operation: &'static str,
    message: String,
}

impl RepositoryError {
    #[must_use]
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.message)
    }
}

impl Error for RepositoryError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTarget {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub guild_id: GuildId,
    pub discord_id: DiscordId,
    pub delta: i64,
    pub source: &'static str,
    pub related_type: &'static str,
    pub reason: &'static str,
    pub metadata: BTreeMap<&'static str, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiphonTransfer {
    pub guild_id: GuildId,
    pub thief_id: DiscordId,
    pub victim_id: DiscordId,
    pub amount: i64,
    pub event_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitheTransfer {
    pub guild_id: GuildId,
    pub discord_id: DiscordId,
    pub amount: i64,
    pub gain: i64,
}

/// Persistence boundary required by mana-effect orchestration.
pub trait ManaEffectsRepository {
    fn current_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<ManaRecord>, RepositoryError>;

    fn all_mana(&mut self, guild_id: GuildId) -> Result<Vec<ManaRecord>, RepositoryError>;

    fn eligible_target(
        &mut self,
        guild_id: GuildId,
        exclude_id: DiscordId,
        min_balance: i64,
        selection_ticket: u64,
    ) -> Result<Option<PlayerTarget>, RepositoryError>;

    fn siphon_atomic(&mut self, transfer: SiphonTransfer) -> Result<i64, RepositoryError>;

    fn add_balance(&mut self, entry: LedgerEntry) -> Result<(), RepositoryError>;

    fn tithe_atomic(&mut self, transfer: TitheTransfer) -> Result<(), RepositoryError>;
}

/// All random decisions required for one siphon attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SiphonRoll {
    pub selection_ticket: u64,
    pub amount: i64,
    pub anonymous: bool,
    pub event_nonce: u128,
}

impl SiphonRoll {
    #[must_use]
    pub const fn new(
        selection_ticket: u64,
        amount: i64,
        anonymous: bool,
        event_nonce: u128,
    ) -> Self {
        Self {
            selection_ticket,
            amount,
            anonymous,
            event_nonce,
        }
    }
}

pub trait SiphonEntropy {
    fn next_roll(&mut self) -> SiphonRoll;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiphonResult {
    pub victim_id: DiscordId,
    pub amount: i64,
    pub attempted_amount: i64,
    pub absorbed_amount: i64,
    pub anonymous: bool,
    pub event_key: String,
}

/// Deduplicated effects in first-requested order.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectsBatch(Vec<(DiscordId, ManaEffectsSnapshot)>);

impl EffectsBatch {
    #[must_use]
    pub fn ids(&self) -> Vec<DiscordId> {
        self.0.iter().map(|(discord_id, _)| *discord_id).collect()
    }

    #[must_use]
    pub fn get(&self, discord_id: DiscordId) -> Option<&ManaEffectsSnapshot> {
        self.0
            .iter()
            .find_map(|(candidate, effects)| (*candidate == discord_id).then_some(effects))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShopItemKind {
    Info,
    Consumable,
    Other,
}

/// Mana-effect application service with injected state and randomness.
pub struct ManaEffectsService<R> {
    repository: R,
    today: String,
    minigame_jc_delta_scale: f64,
    entropy: Box<dyn SiphonEntropy>,
    reward_policy: Option<Box<dyn DailyRewardPolicy>>,
}

impl<R: ManaEffectsRepository> ManaEffectsService<R> {
    #[must_use]
    pub fn new(
        repository: R,
        today: impl Into<String>,
        entropy: impl SiphonEntropy + 'static,
    ) -> Self {
        Self {
            repository,
            today: today.into(),
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            entropy: Box::new(entropy),
            reward_policy: None,
        }
    }

    pub fn set_reward_policy(&mut self, policy: impl DailyRewardPolicy + 'static) {
        self.reward_policy = Some(Box::new(policy));
    }

    pub fn set_minigame_jc_delta_scale(&mut self, scale: f64) {
        self.minigame_jc_delta_scale = scale;
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    #[must_use]
    pub fn into_repository(self) -> R {
        self.repository
    }

    pub fn get_effects(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Result<ManaEffectsSnapshot, RepositoryError> {
        let record = self
            .repository
            .current_mana(discord_id, normalize_guild_id(guild_id))?;
        Ok(effects_from_record(record.as_ref(), &self.today))
    }

    pub fn get_effects_bulk(
        &mut self,
        discord_ids: &[DiscordId],
        guild_id: Option<GuildId>,
    ) -> Result<EffectsBatch, RepositoryError> {
        let mut seen = BTreeSet::new();
        let mut resolved: Vec<_> = discord_ids
            .iter()
            .copied()
            .filter(|discord_id| seen.insert(*discord_id))
            .map(|discord_id| (discord_id, ManaEffectsSnapshot::default()))
            .collect();
        if resolved.is_empty() {
            return Ok(EffectsBatch(resolved));
        }

        let records = self.repository.all_mana(normalize_guild_id(guild_id))?;
        let requested: BTreeSet<_> = resolved.iter().map(|(id, _)| *id).collect();
        for record in records {
            if requested.contains(&record.discord_id)
                && let Some((_, slot)) = resolved
                    .iter_mut()
                    .find(|(discord_id, _)| *discord_id == record.discord_id)
            {
                *slot = effects_from_record(Some(&record), &self.today);
            }
        }
        Ok(EffectsBatch(resolved))
    }

    /// Execute an existing-liquidity transfer. Repository failures are
    /// intentionally swallowed to match Python's best-effort siphon contract.
    pub fn execute_siphon(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Option<SiphonResult> {
        let guild_id = normalize_guild_id(guild_id);
        let roll = self.entropy.next_roll();
        let victim = self
            .repository
            .eligible_target(
                guild_id,
                discord_id,
                HOSTILE_LOSS_MIN_BALANCE,
                roll.selection_ticket,
            )
            .ok()??;
        let generated =
            scale_minigame_jc_delta(roll.amount.clamp(1, 3) as f64, self.minigame_jc_delta_scale);
        let attempted_amount = generated.min(victim.balance);
        if attempted_amount <= 0 {
            return None;
        }
        let event_key = format!(
            "swamp_siphon:{:032x}:{}",
            roll.event_nonce, victim.discord_id
        );
        let applied = self
            .repository
            .siphon_atomic(SiphonTransfer {
                guild_id,
                thief_id: discord_id,
                victim_id: victim.discord_id,
                amount: attempted_amount,
                event_key: event_key.clone(),
            })
            .ok()?;

        Some(SiphonResult {
            victim_id: victim.discord_id,
            amount: applied,
            attempted_amount,
            absorbed_amount: attempted_amount.saturating_sub(applied),
            anonymous: roll.anonymous,
            event_key,
        })
    }

    pub fn apply_blue_tax(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        gain: i64,
    ) -> Result<i64, RepositoryError> {
        let effects = self.get_effects(discord_id, guild_id)?;
        if effects.blue_tax_rate <= 0.0 || gain <= 0 {
            return Ok(0);
        }
        let tax = ((gain as f64 * effects.blue_tax_rate) as i64).max(1);
        self.repository.add_balance(LedgerEntry {
            guild_id: normalize_guild_id(guild_id),
            discord_id,
            delta: -tax,
            source: "mana",
            related_type: "blue_tax",
            reason: "blue mana gain tax",
            metadata: BTreeMap::from([("gain", gain), ("tax", tax)]),
        })?;
        Ok(tax)
    }

    pub fn apply_blue_cashback(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        loss: i64,
    ) -> Result<i64, RepositoryError> {
        let effects = self.get_effects(discord_id, guild_id)?;
        if effects.blue_cashback_rate <= 0.0 || loss <= 0 {
            return Ok(0);
        }
        let base_cashback =
            ((loss.unsigned_abs() as f64 * effects.blue_cashback_rate) as i64).max(1);
        let structurally_scaled =
            scale_minigame_jc_delta(base_cashback as f64, self.minigame_jc_delta_scale);
        let cashback = self
            .reward_policy
            .as_mut()
            .map_or(structurally_scaled, |policy| {
                policy.adjust_reward(guild_id, structurally_scaled)
            });
        if cashback <= 0 {
            return Ok(0);
        }
        self.repository.add_balance(LedgerEntry {
            guild_id: normalize_guild_id(guild_id),
            discord_id,
            delta: cashback,
            source: "mana_reward",
            related_type: "blue_cashback",
            reason: "blue mana loss cashback",
            metadata: BTreeMap::from([
                ("loss", loss),
                ("base_cashback", base_cashback),
                ("adjusted_cashback", cashback),
            ]),
        })?;
        Ok(cashback)
    }

    #[must_use]
    pub fn apply_green_cap(effects: &ManaEffects, gain: i64) -> i64 {
        effects.green_gain_cap.map_or(gain, |cap| gain.min(cap))
    }

    pub fn apply_plains_tithe(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        gain: i64,
    ) -> Result<i64, RepositoryError> {
        let effects = self.get_effects(discord_id, guild_id)?;
        if effects.plains_tithe_rate <= 0.0 || gain <= 0 {
            return Ok(0);
        }
        let tithe = ((gain as f64 * effects.plains_tithe_rate) as i64).max(1);
        self.repository.tithe_atomic(TitheTransfer {
            guild_id: normalize_guild_id(guild_id),
            discord_id,
            amount: tithe,
            gain,
        })?;
        Ok(tithe)
    }

    pub fn apply_shop_discount(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        base_cost: i64,
        kind: ShopItemKind,
    ) -> Result<i64, RepositoryError> {
        if base_cost <= 0 {
            return Ok(base_cost);
        }
        let effects = self.get_effects(discord_id, guild_id)?;
        if effects.color.is_none() {
            return Ok(base_cost);
        }
        let rate = match kind {
            ShopItemKind::Info => effects.shop_info_discount_rate,
            ShopItemKind::Consumable => effects.shop_consumable_discount_rate,
            ShopItemKind::Other => 0.0,
        };
        if rate <= 0.0 {
            return Ok(base_cost);
        }
        Ok(((base_cost as f64 * (1.0 - rate)) as i64).max(1))
    }
}

/// Reference adapter used by parity tests and non-production simulations.
#[derive(Clone, Debug, Default)]
pub struct InMemoryManaEffectsRepository {
    players: BTreeMap<(GuildId, DiscordId), i64>,
    mana: BTreeMap<(GuildId, DiscordId), ManaRecord>,
    nonprofit_funds: BTreeMap<GuildId, i64>,
    ledger: Vec<LedgerEntry>,
    current_mana_reads: usize,
    all_mana_reads: usize,
}

impl InMemoryManaEffectsRepository {
    pub fn register_player(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        balance: i64,
    ) {
        self.players
            .insert((normalize_guild_id(guild_id), discord_id), balance);
    }

    pub fn assign_mana(&mut self, record: ManaRecord) {
        self.mana
            .insert((record.guild_id, record.discord_id), record);
    }

    #[must_use]
    pub fn balance(&self, discord_id: DiscordId, guild_id: Option<GuildId>) -> Option<i64> {
        self.players
            .get(&(normalize_guild_id(guild_id), discord_id))
            .copied()
    }

    pub fn set_nonprofit_fund(&mut self, guild_id: Option<GuildId>, amount: i64) {
        self.nonprofit_funds
            .insert(normalize_guild_id(guild_id), amount);
    }

    #[must_use]
    pub fn nonprofit_fund(&self, guild_id: Option<GuildId>) -> i64 {
        self.nonprofit_funds
            .get(&normalize_guild_id(guild_id))
            .copied()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn ledger(&self) -> &[LedgerEntry] {
        &self.ledger
    }

    #[must_use]
    pub const fn current_mana_reads(&self) -> usize {
        self.current_mana_reads
    }

    #[must_use]
    pub const fn all_mana_reads(&self) -> usize {
        self.all_mana_reads
    }
}

impl ManaEffectsRepository for InMemoryManaEffectsRepository {
    fn current_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<ManaRecord>, RepositoryError> {
        self.current_mana_reads += 1;
        Ok(self.mana.get(&(guild_id, discord_id)).cloned())
    }

    fn all_mana(&mut self, guild_id: GuildId) -> Result<Vec<ManaRecord>, RepositoryError> {
        self.all_mana_reads += 1;
        Ok(self
            .mana
            .values()
            .filter(|record| record.guild_id == guild_id)
            .cloned()
            .collect())
    }

    fn eligible_target(
        &mut self,
        guild_id: GuildId,
        exclude_id: DiscordId,
        min_balance: i64,
        selection_ticket: u64,
    ) -> Result<Option<PlayerTarget>, RepositoryError> {
        let candidates: Vec<_> = self
            .players
            .iter()
            .filter(|((candidate_guild, candidate_id), balance)| {
                *candidate_guild == guild_id
                    && *candidate_id != exclude_id
                    && **balance >= min_balance
            })
            .map(|((_, discord_id), balance)| PlayerTarget {
                discord_id: *discord_id,
                balance: *balance,
            })
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let index = selection_ticket as usize % candidates.len();
        Ok(candidates.get(index).cloned())
    }

    fn siphon_atomic(&mut self, transfer: SiphonTransfer) -> Result<i64, RepositoryError> {
        let victim_key = (transfer.guild_id, transfer.victim_id);
        let thief_key = (transfer.guild_id, transfer.thief_id);
        let Some(victim_balance) = self.players.get(&victim_key).copied() else {
            return Err(RepositoryError::new("siphon_atomic", "victim missing"));
        };
        let Some(thief_balance) = self.players.get(&thief_key).copied() else {
            return Err(RepositoryError::new("siphon_atomic", "thief missing"));
        };
        let applied = transfer.amount.max(0).min(victim_balance.max(0));
        self.players.insert(victim_key, victim_balance - applied);
        self.players.insert(thief_key, thief_balance + applied);
        self.ledger.extend([
            LedgerEntry {
                guild_id: transfer.guild_id,
                discord_id: transfer.victim_id,
                delta: -applied,
                source: "mana",
                related_type: "hostile_loss",
                reason: "swamp siphon",
                metadata: BTreeMap::from([("attempted_loss", transfer.amount)]),
            },
            LedgerEntry {
                guild_id: transfer.guild_id,
                discord_id: transfer.thief_id,
                delta: applied,
                source: "mana",
                related_type: "hostile_loss",
                reason: "swamp siphon",
                metadata: BTreeMap::from([("attempted_loss", transfer.amount)]),
            },
        ]);
        Ok(applied)
    }

    fn add_balance(&mut self, entry: LedgerEntry) -> Result<(), RepositoryError> {
        let key = (entry.guild_id, entry.discord_id);
        let Some(balance) = self.players.get_mut(&key) else {
            return Err(RepositoryError::new("add_balance", "player missing"));
        };
        *balance += entry.delta;
        self.ledger.push(entry);
        Ok(())
    }

    fn tithe_atomic(&mut self, transfer: TitheTransfer) -> Result<(), RepositoryError> {
        let key = (transfer.guild_id, transfer.discord_id);
        let Some(balance) = self.players.get(&key).copied() else {
            return Err(RepositoryError::new("tithe_atomic", "player missing"));
        };
        let fund = self
            .nonprofit_funds
            .get(&transfer.guild_id)
            .copied()
            .unwrap_or(0);
        self.players.insert(key, balance - transfer.amount);
        self.nonprofit_funds
            .insert(transfer.guild_id, fund + transfer.amount);
        self.ledger.push(LedgerEntry {
            guild_id: transfer.guild_id,
            discord_id: transfer.discord_id,
            delta: -transfer.amount,
            source: "mana",
            related_type: "plains_tithe",
            reason: "plains mana tithe",
            metadata: BTreeMap::from([("gain", transfer.gain), ("tithe", transfer.amount)]),
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "mana_effects_service/tests.rs"]
mod tests;
