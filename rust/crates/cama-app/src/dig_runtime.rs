//! Production application orchestration for the `/dig` action.
//!
//! The Discord provider must not reproduce Dig mechanics or issue a handful
//! of unrelated SQLite writes.  This module is the application boundary for
//! that workflow: it admits a player, applies the existing Dig policy graph,
//! stages loot through [`crate::dig_loot::DigLootService`], and hands one
//! compare-and-swap commit to a migrated-database store.
//!
//! The store is deliberately a small port.  The SQLite implementation below
//! is useful in production and in migrated-database tests, while the in-memory
//! implementation makes the orchestration deterministic without a Discord
//! client.  The production graph treats weather, routes, inventory, gear,
//! relics, events, threats, encounters, bosses, prestige, economy, pet work,
//! flavor, and media as required policy inputs to this same commit boundary;
//! there is no provider-side "attach later" path.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::dig_blood_pact::{DigBloodPactRepository, DigBloodPactSettlementRequest};
use cama_db::dig_event_runtime::{
    DigEventActorKey, DigEventActorSnapshot, DigEventQuestSnapshot, DigEventRuntimeRepository,
};
use cama_db::dig_guild_modifiers::DigGuildModifierRepository;
use cama_db::dig_inventory_repository::{
    AutoBuyRequest, AutoBuySelection, BuyInsuranceOutcome, DigInventoryRepository, SetTrapOutcome,
};
use cama_db::dig_runtime_store;
use cama_db::dig_weather::{DigWeatherEntry, DigWeatherRepository, weather_by_id};
use cama_db::loan_repository::{LedgerContext, LoanRepository};
use cama_db::mana_service_repository::ManaRepository;
use cama_db::manashop_rework_repository::ManashopRepository;
use cama_domain::dig_cave_in::{
    CAVE_IN_BLOCK_LOSS_RANGES, CAVE_IN_CATASTROPHIC_GEAR_TICKS, CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_MILESTONE_STEP, CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
    CAVE_IN_INJURY_DIGS_BY_BAND, CAVE_IN_MEDICAL_BILL_RANGES, CAVE_IN_STUN_DIGS_BY_BAND,
    CaveInApplicability, CaveInRng, cave_in_band, pick_cave_in_consequence,
    roll_catastrophic_cave_in,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_gear::{AMULET_TIERS, ARMOR_TIERS, BOOTS_TIERS, WEAPON_TIERS, unique_gear};
use cama_domain::dig_stats::{MinerStats, miner_stat_effects};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::mana::{ManaEffects, weather_combo_modifiers};
use cama_domain::pet::{PetDigWork, PetDigWorkClaim};
use chrono::NaiveDate;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::dig_carry_wager::current_boss_boundary_from_json;
use crate::dig_loot::{
    CanonicalEventResolution, CaveInChanceRequest, DigLootModifiers, DigLootService, InventoryItem,
    LootActionResult, LootEntropy, LootRepository, RepositoryError, SeededLootEntropy,
    TunnelLootState, consumable, is_boss_prep_item, is_dig_consumable,
};
use crate::dig_prestige4_content::{
    ArtifactRollPlan, Prestige4Entropy, artifact_rate_modifier, roll_artifact_stage,
};
use crate::dig_relic_rework::{
    LanternStubRestoreInput, RelicEntropy, RelicSet, YieldContext, apply_lantern_stub_restore,
    is_first_dig_of_day, post_pinnacle_decay_factor, relic_aware_paid_cost,
    relic_jc_yield_multiplier, settle_slow_drip_claim, storm_negates_hazard,
};
use crate::dig_routes::{
    RouteChoiceEvaluation, evaluate_route_choice, parse_route_state, route_artifact_multiplier,
    route_by_id, route_effect, route_status,
};
use crate::dig_service::{
    DIG_REWARD_BASIS_POINTS, DIG_YIELD_MULTIPLIER_SCALE, DigOutcomeInput, DigProfitPolicy,
    MinerAllocation, TunnelState, apply_boss_gate, apply_dig_outcome, apply_first_dig,
    cooldown_remaining, layer_at, paid_dig_cost, scale_dig_minigame_jc, scale_dig_yield_once,
};
use crate::dig_tunnels::{
    aggregate_prestige_perk_effects, ascension_effects, mutation_effects, mutations_from_json,
    roll_corruption, round_prestige_perk_bonus_half_up,
};
use crate::economy_event_service::EconomyEventConfig;
use crate::economy_event_sqlite::SqliteEconomyEventService;
use crate::mana_effects_service::color_for_land;
use crate::pet::{SeededPetRandom, SystemPetClock};
use crate::pet_sqlite::SqlitePetCommandService;
use crate::service_container::PersistentVanityTaxService;
use crate::vanity_tax_service::VanityTaxService;

/// Production image root used by the Rust deployment. The Docker image must
/// copy the authored `assets/dig` tree here; procedural rendering is only the
/// explicit fallback implemented by [`crate::dig_assets::DigAssetService`].
pub const DEFAULT_DIG_ASSET_ROOT: &str = "/app/assets/dig";

/// The hard wall after the pinnacle run.  Keep this application constant in
/// the same units as the persisted tunnel depth so the runtime can reject a
/// request before weather, pet work, or any other side effect is staged.
pub const PRESTIGE_HARD_CAP: i64 = cama_app_boss_hard_cap();

/// Depth at which the authored endgame luminosity ramp begins.
pub const LUMINOSITY_DEEP_DRAIN_START_DEPTH: i64 = 350;
/// Every this many endgame blocks adds one luminosity drain point per dig.
pub const LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP: i64 = 20;

const fn cama_app_boss_hard_cap() -> i64 {
    crate::dig_bosses::PRESTIGE_HARD_CAP as i64
}

/// True when the next canonical boss row is at most one block below the
/// tunnel head, or the tunnel has reached the pinnacle stretch.
///
/// A boss at boundary `b` is engaged from depth `b - 1` (see
/// [`crate::dig_bosses::at_boss_boundary`]), so depths `b - 2` and `b - 1`
/// are the digs that reach or fight that boss.  The pinnacle engages from
/// `PINNACLE_DEPTH - 1` and the endgame luminosity drain ramps up from
/// there, so every deeper depth stays inside the window.
fn near_boss_boundary(depth: i64) -> bool {
    if depth >= i64::from(crate::dig_bosses::PINNACLE_DEPTH) - 1 {
        return true;
    }
    crate::dig_bosses::BOSS_BOUNDARIES
        .into_iter()
        .any(|boundary| {
            let boundary = i64::from(boundary);
            depth < boundary && boundary - depth <= 2
        })
}

/// Torch auto-buy admission, matching the legacy trigger: low light anywhere
/// (self-limiting, since a torch restores 50 luminosity) or the boss window.
///
/// Unlike the old `depth >= b` check this does not fire on every dig between
/// boundaries, so an opted-in miner with healthy light is not charged 6 JC
/// per dig.
fn should_auto_buy_torch(depth: i64, luminosity: i64) -> bool {
    luminosity <= 50 || near_boss_boundary(depth)
}

/// Return the additional luminosity consumed by one dig in the deep ramp.
///
/// The calculation intentionally uses the depth before the dig and floors
/// partial steps, matching the Python prestige policy.  At or below the
/// start there is no bonus.
#[must_use]
pub const fn deep_luminosity_drain_bonus(depth: i64) -> i64 {
    if depth <= LUMINOSITY_DEEP_DRAIN_START_DEPTH {
        0
    } else {
        (depth - LUMINOSITY_DEEP_DRAIN_START_DEPTH) / LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP
    }
}

#[derive(Clone, Debug)]
pub struct DigRuntimeConfig {
    pub asset_root: PathBuf,
    pub require_authored_assets: bool,
    pub minigame_jc_delta_scale: f64,
    pub bankruptcy_penalty_keep_basis_points: i64,
    pub economy_event: EconomyEventConfig,
    /// Pet dig work is settled lazily from the persisted hunger/work anchors.
    /// Keeping the decay policy on the runtime config makes the dig aggregate
    /// use the same value as the pet application service without a scheduler.
    pub pet_decay_per_day: i64,
    /// Server-side secret mixed into dig and dig-event seeds.
    ///
    /// Without it a dig's randomness derives entirely from values the player
    /// knows or controls -- their ids, the click second, and the paid/forced
    /// flags -- so the loot pipeline can be simulated for each upcoming second
    /// and a click timed for no cave-in, maximum JC, or an artifact. Production
    /// draws an unpredictable value; the default stays zero so tests keep their
    /// deterministic seeds.
    pub entropy_secret: u64,
}

impl Default for DigRuntimeConfig {
    fn default() -> Self {
        Self {
            asset_root: PathBuf::from(DEFAULT_DIG_ASSET_ROOT),
            require_authored_assets: false,
            minigame_jc_delta_scale: 1.0,
            bankruptcy_penalty_keep_basis_points: DIG_REWARD_BASIS_POINTS,
            economy_event: EconomyEventConfig::default(),
            pet_decay_per_day: cama_domain::pet::DEFAULT_HUNGER_DECAY_PER_DAY,
            entropy_secret: 0,
        }
    }
}

impl DigRuntimeConfig {
    #[must_use]
    pub fn production() -> Self {
        Self {
            entropy_secret: process_dig_secret(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_asset_root(root: impl Into<PathBuf>) -> Self {
        Self {
            asset_root: root.into(),
            ..Self::default()
        }
    }

    /// Set the secret mixed into dig seeds; zero keeps the legacy seed.
    #[must_use]
    pub const fn with_entropy_secret(mut self, secret: u64) -> Self {
        self.entropy_secret = secret;
        self
    }

    pub fn with_runtime_policy(
        mut self,
        minigame_jc_delta_scale: f64,
        economy_event: EconomyEventConfig,
    ) -> Self {
        self.minigame_jc_delta_scale = minigame_jc_delta_scale.max(0.0);
        self.economy_event = economy_event.normalized();
        self
    }

    #[must_use]
    pub fn with_pet_decay_per_day(mut self, decay_per_day: i64) -> Self {
        self.pet_decay_per_day = decay_per_day.max(0);
        self
    }

    #[must_use]
    pub fn with_bankruptcy_penalty_rate(mut self, kept_rate: f64) -> Self {
        self.bankruptcy_penalty_keep_basis_points = if kept_rate.is_finite() {
            (kept_rate.clamp(0.0, 1.0) * DIG_REWARD_BASIS_POINTS as f64).round() as i64
        } else {
            DIG_REWARD_BASIS_POINTS
        };
        self
    }

    #[must_use]
    pub fn authored_asset_root(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.asset_root.join(relative)
    }
}

pub use cama_db::dig_runtime_store::{
    DigRuntimeArtifact, DigRuntimeGear, DigRuntimeInventoryItem, DigRuntimeTunnel,
};

/// The persisted forecast row used by the current game date.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeWeather {
    pub layer_name: String,
    pub weather_id: String,
}

impl From<DigWeatherEntry> for DigRuntimeWeather {
    fn from(entry: DigWeatherEntry) -> Self {
        Self {
            layer_name: entry.layer_name,
            weather_id: entry.weather_id,
        }
    }
}

/// Allocated miner stats for the Dig policy graph.
fn miner_allocation(tunnel: &DigRuntimeTunnel) -> MinerAllocation {
    MinerAllocation {
        strength: tunnel.stat_strength.max(0),
        smarts: tunnel.stat_smarts.max(0),
        stamina: tunnel.stat_stamina.max(0),
        stat_points: tunnel.stat_points.max(0),
    }
}

/// One consistent read of the player, tunnel, inventory, and artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeSnapshot {
    pub registered: bool,
    pub balance: i64,
    pub tunnel: Option<DigRuntimeTunnel>,
    pub inventory: Vec<DigRuntimeInventoryItem>,
    pub artifacts: Vec<DigRuntimeArtifact>,
    pub gear: Vec<DigRuntimeGear>,
    pub weather: Vec<DigRuntimeWeather>,
}

/// A Slow Drip claim prepared from the same actor snapshot as the Dig.
///
/// The gross amount is the daily-cap accounting unit.  `credit_jc` is the
/// amount that reaches the wallet after the persisted daily economy effect
/// and the central positive-Dig scale.  The expected fields are carried into
/// the commit detail so SQLite can CAS the claim row in the same transaction
/// as the tunnel, wallet, inventory, gear, and action audit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSlowDripClaim {
    pub claim_date: String,
    pub gross_jc: i64,
    pub credit_jc: i64,
    pub claimed_before: i64,
    pub claimed_after: i64,
    pub anchor_before: i64,
    pub expected_last_claim_at: i64,
    pub claimed_at: i64,
}

/// Request-local inputs for one canonical event selection after the Dig
/// advance/boss gate. Keeping these values together prevents the runtime
/// store seam from dropping post-gate depth, live Void Bait, or ascension
/// rarity modifiers.
#[derive(Clone, Copy, Debug)]
pub struct DigRuntimeCanonicalEventRequest<'a> {
    pub snapshot: &'a DigRuntimeSnapshot,
    pub quest: &'a DigEventQuestSnapshot,
    pub depth: i64,
    pub luminosity: i64,
    pub in_boss: bool,
    pub void_bait_active: bool,
    pub rare_event_multiplier: f64,
    pub legendary_event_multiplier: f64,
    pub selection_roll_bits: u64,
}

impl DigRuntimeSnapshot {
    #[must_use]
    pub fn fresh(discord_id: i64, guild_id: i64, balance: i64, now: i64) -> Self {
        Self {
            registered: true,
            balance,
            tunnel: Some(DigRuntimeTunnel::new(discord_id, guild_id, now)),
            inventory: Vec::new(),
            artifacts: Vec::new(),
            gear: Vec::new(),
            weather: Vec::new(),
        }
    }
}

/// Version fields used to reject duplicate Discord clicks and stale views.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimeVersion {
    pub balance: i64,
    pub depth: Option<i64>,
    pub total_digs: Option<i64>,
    pub last_dig_at: Option<i64>,
    pub inventory_fingerprint: u64,
    pub artifact_fingerprint: u64,
    pub gear_fingerprint: u64,
    pub tunnel_fingerprint: u64,
}

impl From<&DigRuntimeSnapshot> for DigRuntimeVersion {
    fn from(snapshot: &DigRuntimeSnapshot) -> Self {
        Self {
            balance: snapshot.balance,
            depth: snapshot.tunnel.as_ref().map(|tunnel| tunnel.depth),
            total_digs: snapshot.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
            last_dig_at: snapshot
                .tunnel
                .as_ref()
                .and_then(|tunnel| tunnel.last_dig_at),
            inventory_fingerprint: fingerprint(&snapshot.inventory),
            artifact_fingerprint: fingerprint(&snapshot.artifacts),
            gear_fingerprint: fingerprint(&snapshot.gear),
            tunnel_fingerprint: snapshot.tunnel.as_ref().map_or(0, fingerprint),
        }
    }
}

/// One transaction request emitted by [`DigRuntimeService`].
#[derive(Clone, Debug)]
pub struct DigRuntimeCommit {
    pub expected: DigRuntimeVersion,
    pub next: DigRuntimeSnapshot,
    pub delivery_draft: Option<DigRuntimeDeliveryDraft>,
    pub consumed_item_ids: Vec<i64>,
    /// Optimistic pet work settlement.  The claim is applied in the same
    /// SQLite transaction as the tunnel, wallet, inventory, and audit rows so
    /// a stale pet cannot consume a paid dig or advance the tunnel.
    pub pet_work_claim: Option<PetDigWorkClaim>,
    /// Spend the request-local Overgrowth charge in this same settlement.
    /// The live reward/cave policy is calculated from a preview, so commit
    /// must reject the stage if that charge disappeared before the CAS.
    pub consume_overgrowth: bool,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    /// Tax withheld from the gross JC reward. `jc_delta` remains the net
    /// audited reward so historical Dig reports retain their Python meaning.
    pub vanity_tax: i64,
    /// Low-priority tax withheld from the same gross JC reward, accounted and
    /// ledgered separately from the vanity tax.
    pub low_priority_tax: i64,
    /// Conditional wallet debit reserved before the staged Dig reward. This
    /// remains in the same transaction while producing its own ledger entry.
    pub balance_cost: i64,
    pub action_type: String,
    pub detail: String,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeCommitReceipt {
    pub balance_after: i64,
    pub action_id: i64,
    /// Maps staged local identifiers to SQLite identifiers. A stage must not
    /// guess global AUTOINCREMENT values (another miner may own the next id).
    pub inserted_item_ids: Vec<(i64, i64)>,
    pub inserted_artifact_ids: Vec<(i64, i64)>,
    pub inserted_gear_ids: Vec<(i64, i64)>,
}

/// Persistence errors are intentionally typed so provider code can distinguish
/// a stale interaction from a missing player or a storage failure.
#[derive(Debug, Error)]
pub enum DigRuntimeStoreError {
    #[error("player is not registered")]
    MissingPlayer,
    #[error("tunnel is missing")]
    MissingTunnel,
    #[error("Dig state changed before the transaction committed")]
    Conflict,
    #[error("queued consumable {0} disappeared before the transaction committed")]
    MissingQueuedItem(i64),
    #[error("persisted Dig state changed before the transaction committed")]
    StateConflict,
    #[error("invalid persisted Dig JSON in {0}")]
    InvalidJson(&'static str),
    #[error("SQLite Dig operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("in-memory Dig store lock is poisoned")]
    Poisoned,
    #[error("Dig weather setup failed: {0}")]
    Weather(String),
    #[error("Dig inventory operation failed: {0}")]
    Inventory(String),
    #[error("Dig event operation failed: {0}")]
    Event(String),
    #[error("insufficient_funds")]
    InsufficientFunds,
    #[error("pet dig work changed before the transaction committed")]
    PetWorkConflict,
    #[error("Dig pet operation failed: {0}")]
    Pet(String),
    #[error("the previewed Overgrowth charge changed before Dig settlement")]
    OvergrowthConflict,
    #[error("Dig Blood Pact settlement failed: {0}")]
    BloodPact(String),
}

/// The only persistence seam used by the Dig application workflow.
pub trait DigRuntimeStore: Send + Sync {
    fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError>;

    fn commit(
        &self,
        request: DigRuntimeCommit,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError>;

    /// Commit a Dig and its immutable Discord projection. Production SQLite
    /// overrides this to keep the action and outbox in one transaction;
    /// lightweight stores retain the older commit-then-attach compatibility
    /// path.
    fn commit_with_delivery(
        &self,
        request: DigRuntimeCommit,
        draft: DigRuntimeDeliveryDraft,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        let receipt = self.commit(request)?;
        let mut outcome = draft.outcome;
        outcome.action_id = Some(receipt.action_id);
        if let Some(delivery) = build_delivery_snapshot(
            &outcome,
            draft.discord_id,
            draft.guild_id,
            draft.context,
            draft.committed_at,
        ) {
            self.attach_delivery(&delivery)?;
        }
        Ok(receipt)
    }

    /// Return a settled, bounded pet-work offer for one dig.  Non-SQLite
    /// stores may omit pet integration by keeping the default `None`.
    fn preview_pet_dig_work(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _now: i64,
        _decay_per_day: i64,
        _entropy_secret: u64,
    ) -> Result<Option<PetDigWork>, DigRuntimeStoreError> {
        Ok(None)
    }

    /// Ensure and return the two persisted weather rows for a game date. The
    /// default keeps deterministic non-SQLite tests independent of weather;
    /// production SQLite adapters override it with the canonical repository.
    fn ensure_weather(
        &self,
        _guild_id: i64,
        _game_date: &str,
        _now: i64,
    ) -> Result<Vec<DigRuntimeWeather>, DigRuntimeStoreError> {
        Ok(Vec::new())
    }

    fn event_actor_snapshot(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Option<DigEventActorSnapshot>, DigRuntimeStoreError> {
        Ok(None)
    }

    fn event_quest_snapshot(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _now: i64,
    ) -> Result<cama_db::dig_event_runtime::DigEventQuestSnapshot, DigRuntimeStoreError> {
        Ok(cama_db::dig_event_runtime::DigEventQuestSnapshot::default())
    }

    fn helltide_tax(&self, _guild_id: i64, _now: i64) -> Result<i64, DigRuntimeStoreError> {
        Ok(0)
    }

    fn adjust_daily_reward(
        &self,
        _guild_id: i64,
        amount: i64,
        _now: i64,
        _economy_config: &EconomyEventConfig,
    ) -> Result<(i64, f64), DigRuntimeStoreError> {
        Ok((amount, 1.0))
    }

    /// Read the active daily-economy multiplier without applying it to a
    /// partial roll.  Normal Digs must query this only after their structural
    /// payout (milestones/streak/central scale) has been assembled.
    fn daily_reward_multiplier(
        &self,
        _guild_id: i64,
        _now: i64,
        _economy_config: &EconomyEventConfig,
    ) -> Result<f64, DigRuntimeStoreError> {
        Ok(1.0)
    }

    /// Resolve one immutable Mana snapshot for the whole admitted Dig.
    /// Every paid-cost, cooldown, hazard, yield, and tax policy reuses this
    /// value so a request cannot observe two different daily assignments.
    fn mana_effects(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _today: &str,
    ) -> Result<ManaEffects, DigRuntimeStoreError> {
        Ok(ManaEffects::default())
    }

    fn bankruptcy_penalty_games(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<i64, DigRuntimeStoreError> {
        Ok(0)
    }

    /// Credit a successfully computed Plains tithe before the actor's Dig
    /// commit. Python keeps this as a separate fail-soft reserve boundary and
    /// subtracts the tithe from payout only when this call succeeds.
    fn credit_plains_tithe(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _total_jc: i64,
        _tithe: i64,
        _event_key: &str,
    ) -> Result<Option<i64>, DigRuntimeStoreError> {
        Ok(None)
    }

    /// Whether the player has an unspent Overgrowth charge at this instant.
    fn overgrowth_active(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _now: i64,
    ) -> Result<bool, DigRuntimeStoreError> {
        Ok(false)
    }

    fn auto_buy_items(
        &self,
        _request: AutoBuyRequest<'_>,
    ) -> Result<Vec<cama_db::dig_inventory_repository::AutoBuyItemOutcome>, DigRuntimeStoreError>
    {
        Ok(Vec::new())
    }

    /// Claim a relic-backed Slow Drip payout at the command boundary. Python
    /// deliberately records the gross daily-cap claim before the separate
    /// wallet credit, so a credit failure cannot restore idle time.  The
    /// pending Dig itself may subsequently be rejected (cooldown, cap, or
    /// boss), but the already-claimed payout remains durable.
    fn claim_slow_drip(
        &self,
        _snapshot: &DigRuntimeSnapshot,
        _now: i64,
        _economy_config: &EconomyEventConfig,
    ) -> Result<Option<DigSlowDripClaim>, DigRuntimeStoreError> {
        Ok(None)
    }

    fn canonical_event_id(
        &self,
        _snapshot: &DigRuntimeSnapshot,
        _now: i64,
        _in_boss: bool,
        _entropy_seed: u64,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        Ok(None)
    }

    /// Pick one canonical event from the already-loaded Dig stage.  The
    /// caller supplies the post-boss-gate depth/luminosity and the one
    /// selection draw so the repository never re-reads the tunnel or owns a
    /// second RNG stream.
    fn canonical_event_id_for_snapshot(
        &self,
        _request: DigRuntimeCanonicalEventRequest<'_>,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        Ok(None)
    }

    /// Attach the immutable public-delivery projection to the committed
    /// action.  SQLite overrides this with a compare-by-action-id update;
    /// lightweight policy stores may leave the outbox inert.
    fn attach_delivery(
        &self,
        _delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<(), DigRuntimeStoreError> {
        Ok(())
    }

    fn pending_deliveries(
        &self,
        _query: DigRuntimePendingDeliveryQuery,
    ) -> Result<Vec<DigRuntimeDeliverySnapshot>, DigRuntimeStoreError> {
        Ok(Vec::new())
    }

    fn mark_delivery_delivered(
        &self,
        _request: DigRuntimeMarkDelivered,
    ) -> Result<bool, DigRuntimeStoreError> {
        Ok(false)
    }

    /// Atomically move one still-pending delivery part to a fallback channel.
    /// This must commit before Discord is called so READY recovery observes
    /// the same channel even if the process dies after Discord accepts the
    /// fallback send but before the delivered receipt CAS.
    fn rebind_pending_delivery_channel(
        &self,
        _request: DigRuntimeRebindDeliveryChannel,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        Err(DigRuntimeStoreError::StateConflict)
    }

    fn finalize_delivery(
        &self,
        _request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        Err(DigRuntimeStoreError::StateConflict)
    }

    /// Settle the durable post-Dig Blood Pact effect before the delivery
    /// caller finalizes flavor or renders the message.  SQLite owns the
    /// exact-once repository boundary; lightweight stores leave the snapshot
    /// unchanged because they have no durable hostile-loss ledger.
    fn settle_blood_pact_delivery(
        &self,
        _request: DigRuntimeSettleBloodPact,
        _minigame_jc_delta_scale: f64,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        Err(DigRuntimeStoreError::StateConflict)
    }
}

mod delivery;
mod effects;
mod store;

use delivery::build_delivery_snapshot;
pub use delivery::{
    DigAdminMutationOutcome, DigRuntimeActionResult, DigRuntimeBloodPactSnapshot,
    DigRuntimeBossRenderSnapshot, DigRuntimeDeliveryContext, DigRuntimeDeliveryDraft,
    DigRuntimeDeliveryPart, DigRuntimeDeliverySnapshot, DigRuntimeEventKind,
    DigRuntimeEventOutcome, DigRuntimeEventRenderSnapshot, DigRuntimeEventRequest,
    DigRuntimeExecution, DigRuntimeFinalizeDelivery, DigRuntimeFlavorSnapshot, DigRuntimeFlexData,
    DigRuntimeHallOfFameRow, DigRuntimeLeaderboardRow, DigRuntimeMarkDelivered,
    DigRuntimePendingDeliveryQuery, DigRuntimeRebindDeliveryChannel, DigRuntimeRenderKind,
    DigRuntimeRenderSnapshot, DigRuntimeSettleBloodPact, DigRuntimeTunnelInfo,
    DigRuntimeWeatherInfo, DigRuntimeWeatherPresentation,
};
use effects::{
    CaveInLootRng, DigPrestige4Entropy, LootRelicEntropy, active_buff_effects,
    active_curse_effects, active_curse_summary, active_pickaxe_tier, apply_mana_base_yield,
    bonus_basis_points, event_chance_factor, gear_effects, luminosity_jc_multiplier,
    multiplier_millionths, proportional_mana_yield_tax, weather_code, weather_effects,
};
pub use effects::{DigWeatherEffects, apply_cave_in_gear_ticks, catastrophic_cave_in_depth};
pub use store::{AtomicTunnelBalanceUpdate, SqliteDigRuntimeStore};

/// A deterministic stage implementing the existing loot repository contract.
/// It never writes SQLite; the outer application service owns the final CAS.
#[derive(Clone, Debug)]
pub struct DigRuntimeLootRepository {
    snapshot: DigRuntimeSnapshot,
}

impl DigRuntimeLootRepository {
    #[must_use]
    pub const fn new(snapshot: DigRuntimeSnapshot) -> Self {
        Self { snapshot }
    }

    #[must_use]
    pub const fn snapshot(&self) -> &DigRuntimeSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn into_snapshot(self) -> DigRuntimeSnapshot {
        self.snapshot
    }
}

fn static_item(item_type: &str) -> Option<&'static str> {
    crate::dig_loot::consumable(item_type).map(|definition| definition.id)
}

impl LootRepository for DigRuntimeLootRepository {
    fn has_tunnel(&self, _discord_id: i64, _guild_id: i64) -> bool {
        self.snapshot.tunnel.is_some()
    }

    fn tunnel(&self, _discord_id: i64, _guild_id: i64) -> Option<TunnelLootState> {
        self.snapshot.tunnel.as_ref().map(|tunnel| TunnelLootState {
            depth: tunnel.depth,
            luminosity: tunnel.luminosity,
            injured: injury_reduces_advance(tunnel.injury_state.as_deref()),
            hard_hat_charges: tunnel.hard_hat_charges,
            reinforced_until: tunnel.reinforced_until,
            void_bait_digs: tunnel.void_bait_digs,
            sonar_skip_pending: tunnel.sonar_skip_pending,
            grappling_hook_charges: tunnel.grappling_hook_charges,
            temp_buff: tunnel.temp_buffs.clone(),
        })
    }

    fn set_tunnel(&mut self, _discord_id: i64, _guild_id: i64, tunnel: TunnelLootState) {
        if let Some(current) = self.snapshot.tunnel.as_mut() {
            current.depth = tunnel.depth;
            current.luminosity = tunnel.luminosity;
            current.hard_hat_charges = tunnel.hard_hat_charges;
            current.reinforced_until = tunnel.reinforced_until;
            current.void_bait_digs = tunnel.void_bait_digs;
            current.sonar_skip_pending = tunnel.sonar_skip_pending;
            current.grappling_hook_charges = tunnel.grappling_hook_charges;
            current.temp_buffs = tunnel.temp_buff;
        }
    }

    fn balance(&self, _discord_id: i64, _guild_id: i64) -> i64 {
        self.snapshot.balance
    }

    fn inventory(&self, discord_id: i64, guild_id: i64) -> Vec<InventoryItem> {
        self.snapshot
            .inventory
            .iter()
            .filter_map(|item| {
                Some(InventoryItem {
                    id: item.id,
                    discord_id,
                    guild_id,
                    item_type: static_item(&item.item_type)?,
                    queued: item.queued,
                })
            })
            .collect()
    }

    fn atomic_buy_item(
        &mut self,
        discord_id: i64,
        _guild_id: i64,
        item_type: &'static str,
        cost: i64,
        queued: bool,
    ) -> Result<i64, RepositoryError> {
        if self.snapshot.balance < cost {
            return Err(RepositoryError::InsufficientFunds);
        }
        let id = self
            .snapshot
            .inventory
            .iter()
            .map(|item| item.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.snapshot.balance -= cost;
        self.snapshot.inventory.push(DigRuntimeInventoryItem {
            id,
            item_type: item_type.to_owned(),
            queued,
        });
        let _ = discord_id;
        Ok(id)
    }

    fn queue_item(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        item_id: i64,
    ) -> Result<(), RepositoryError> {
        let Some(item_type) = self
            .snapshot
            .inventory
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| item.item_type.clone())
        else {
            return Err(RepositoryError::MissingItem);
        };
        if self
            .snapshot
            .inventory
            .iter()
            .any(|item| item.queued && item.item_type == item_type)
        {
            return Err(RepositoryError::MissingItem);
        }
        if let Some(item) = self
            .snapshot
            .inventory
            .iter_mut()
            .find(|item| item.id == item_id)
        {
            item.queued = true;
            Ok(())
        } else {
            Err(RepositoryError::MissingItem)
        }
    }

    fn atomic_commit_dig(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        tunnel: TunnelLootState,
        consumed_item_ids: &[i64],
    ) -> Result<(), RepositoryError> {
        if consumed_item_ids.iter().any(|item_id| {
            !self
                .snapshot
                .inventory
                .iter()
                .any(|item| item.id == *item_id && item.queued)
        }) {
            return Err(RepositoryError::MissingItem);
        }
        self.snapshot
            .inventory
            .retain(|item| !consumed_item_ids.contains(&item.id));
        self.set_tunnel(0, 0, tunnel);
        Ok(())
    }

    fn add_artifact(
        &mut self,
        discord_id: i64,
        _guild_id: i64,
        artifact_id: &str,
        is_relic: bool,
    ) -> Result<i64, RepositoryError> {
        let id = self
            .snapshot
            .artifacts
            .iter()
            .map(|artifact| artifact.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.snapshot.artifacts.push(DigRuntimeArtifact {
            id,
            artifact_id: artifact_id.to_owned(),
            is_relic,
            equipped: false,
        });
        let _ = discord_id;
        Ok(id)
    }

    fn artifacts(&self, discord_id: i64, guild_id: i64) -> Vec<cama_domain::dig_gear::Artifact> {
        self.snapshot
            .artifacts
            .iter()
            .map(|artifact| cama_domain::dig_gear::Artifact {
                id: artifact.id,
                discord_id,
                guild_id,
                artifact_id: artifact.artifact_id.clone(),
                is_relic: artifact.is_relic,
                equipped: artifact.equipped,
            })
            .collect()
    }

    fn atomic_gift_relic(
        &mut self,
        _giver_id: i64,
        receiver_id: i64,
        _guild_id: i64,
        artifact_db_id: i64,
    ) -> Result<(), RepositoryError> {
        let Some(artifact) = self
            .snapshot
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.id == artifact_db_id && artifact.is_relic)
        else {
            return Err(RepositoryError::InvalidArtifact);
        };
        artifact.equipped = false;
        let _ = receiver_id;
        Ok(())
    }
}

/// Request to execute one real or paid Dig.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimeRequest {
    pub discord_id: i64,
    pub guild_id: i64,
    pub now: i64,
    pub paid: bool,
    pub forced_event: bool,
}

/// Typed result consumed by Discord rendering and component dispatch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub depth_before: i64,
    pub depth_after: i64,
    pub advance: i64,
    pub jc_earned: i64,
    /// Vanity tax withheld from the post-Mana/post-Helltide gross reward.
    pub vanity_tax: i64,
    /// Low-priority tax withheld from the same gross reward as the vanity
    /// tax. Deliveries persisted before this sink existed default to zero.
    #[serde(default)]
    pub low_priority_tax: i64,
    pub balance_after: i64,
    /// Python's result-card inputs, captured at settlement so reconnect
    /// delivery does not have to reconstruct presentation from newer state.
    #[serde(default)]
    pub tunnel_name: String,
    #[serde(default)]
    pub milestone_bonus: i64,
    #[serde(default)]
    pub streak_bonus: i64,
    #[serde(default)]
    pub bankruptcy_penalty: i64,
    #[serde(default = "default_luminosity")]
    pub luminosity_after: i64,
    #[serde(default)]
    pub luminosity_drained: i64,
    #[serde(default)]
    pub corruption_description: Option<String>,
    #[serde(default)]
    pub mutation_names: Vec<String>,
    #[serde(default)]
    pub tip: String,
    pub cave_in: bool,
    pub cave_in_detail: Option<String>,
    pub event_id: Option<String>,
    pub artifact_id: Option<String>,
    pub boss_boundary: Option<i64>,
    pub first_dig: bool,
    pub paid_dig_cost: i64,
    pub cooldown_remaining: i64,
    pub paid_dig_available: bool,
    pub items_used: Vec<String>,
    pub consumed_item_ids: Vec<i64>,
    pub action_id: Option<i64>,
    pub route_choice_required: bool,
    pub pickaxe_tier: i64,
    /// Number of whole pet-work blocks applied to this dig.  This is kept
    /// separate from `advance` so Discord can explain the assist without
    /// reconstructing the policy roll.
    pub pet_dig_bonus: i64,
    pub pet_name: Option<String>,
    pub forced_event_consumed: bool,
    pub relic_trim_notice: bool,
    /// The authored weather affecting the layer reached by this Dig.  The
    /// first Dig intentionally has no weather: Python lazily rolls the daily
    /// forecast only once the main Dig path is admitted.
    #[serde(default)]
    pub weather: Option<DigRuntimeWeatherInfo>,
    /// The hex still riding the miner as this Dig resolves, if any.
    ///
    /// A curse outlives the Dig that applied it, so presentation needs it on
    /// every result rather than only on the one that inflicted it. `None`
    /// means no curse is active once this Dig's decrement is accounted for.
    #[serde(default)]
    pub active_curse: Option<ActiveCurseSummary>,
}

/// A curse currently afflicting the miner, with what it costs per Dig.
///
/// The penalty fields mirror the persisted curse effects rather than being
/// re-derived, so displayed numbers cannot drift from the ones actually
/// applied to the roll.  Penalties keep their applied sign: `advance_bonus`
/// and `jc_bonus` are negative for a curse.  The two rate penalties are
/// carried as whole percents rather than the stored fractions, both because
/// that is how they are shown and so this stays comparable by equality
/// alongside the rest of the outcome.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActiveCurseSummary {
    pub name: String,
    pub digs_remaining: i64,
    pub advance_bonus: i64,
    pub jc_bonus: i64,
    pub luminosity_drain: i64,
    pub cave_in_percent: i64,
    pub cooldown_percent: i64,
}

const fn default_luminosity() -> i64 {
    LUMINOSITY_MAX
}

impl DigRuntimeActionResult {
    fn error(snapshot: &DigRuntimeSnapshot, message: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(message.into()),
            item: None,
            item_id: None,
            route_id: None,
            cost: 0,
            queued: false,
            balance_after: snapshot.balance,
            action_id: None,
        }
    }
}

impl DigRuntimeOutcome {
    fn blocked(
        snapshot: &DigRuntimeSnapshot,
        message: impl Into<String>,
        cost: i64,
        cooldown: i64,
    ) -> Self {
        let depth = snapshot.tunnel.as_ref().map_or(0, |tunnel| tunnel.depth);
        Self {
            success: false,
            error: Some(message.into()),
            depth_before: depth,
            depth_after: depth,
            advance: 0,
            jc_earned: 0,
            vanity_tax: 0,
            low_priority_tax: 0,
            balance_after: snapshot.balance,
            tunnel_name: snapshot.tunnel.as_ref().map_or_else(
                || "Unknown Tunnel".to_owned(),
                |tunnel| tunnel.tunnel_name.clone(),
            ),
            milestone_bonus: 0,
            streak_bonus: 0,
            bankruptcy_penalty: 0,
            luminosity_after: snapshot
                .tunnel
                .as_ref()
                .map_or(LUMINOSITY_MAX, |tunnel| tunnel.luminosity),
            luminosity_drained: 0,
            corruption_description: None,
            mutation_names: Vec::new(),
            tip: String::new(),
            cave_in: false,
            cave_in_detail: None,
            event_id: None,
            artifact_id: None,
            boss_boundary: None,
            first_dig: false,
            paid_dig_cost: cost,
            cooldown_remaining: cooldown,
            paid_dig_available: snapshot.balance >= cost,
            items_used: Vec::new(),
            consumed_item_ids: Vec::new(),
            action_id: None,
            route_choice_required: false,
            pickaxe_tier: snapshot
                .tunnel
                .as_ref()
                .map_or(0, |tunnel| tunnel.pickaxe_tier),
            pet_dig_bonus: 0,
            pet_name: None,
            forced_event_consumed: false,
            relic_trim_notice: false,
            weather: None,
            active_curse: None,
        }
    }
}

/// Application seam for the gateway-maintained vanity-tax policy.
///
/// The default runtime is deliberately a no-op so existing deployments and
/// non-Discord callers retain the current economy until a provider injects the
/// shared eligibility service with [`DigRuntimeService::with_vanity_tax`].
pub trait DigRuntimeVanityTaxPort: Send + Sync + std::fmt::Debug {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64;
}

impl DigRuntimeVanityTaxPort for VanityTaxService {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64 {
        self.calculate_tax(discord_id, Some(guild_id), gross_profit)
    }
}

impl DigRuntimeVanityTaxPort for PersistentVanityTaxService {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64 {
        self.calculate_tax(discord_id, guild_id, gross_profit)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDigRuntimeVanityTax;

impl DigRuntimeVanityTaxPort for NoDigRuntimeVanityTax {
    fn calculate_tax(&self, _discord_id: i64, _guild_id: i64, _gross_profit: i64) -> i64 {
        0
    }
}

/// Application seam for the persisted active-low-priority tax policy.
///
/// This mirrors [`DigRuntimeVanityTaxPort`] as a separately accounted sink:
/// the default runtime is a no-op so non-Discord callers retain the current
/// economy until a provider injects the guild's active low-priority roster
/// with [`DigRuntimeService::with_low_priority_tax`].
pub trait DigRuntimeLowPriorityTaxPort: Send + Sync + std::fmt::Debug {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDigRuntimeLowPriorityTax;

impl DigRuntimeLowPriorityTaxPort for NoDigRuntimeLowPriorityTax {
    fn calculate_tax(&self, _discord_id: i64, _guild_id: i64, _gross_profit: i64) -> i64 {
        0
    }
}

/// Full application orchestration for a Dig request.
#[derive(Clone, Debug)]
pub struct DigRuntimeService<S = SqliteDigRuntimeStore> {
    store: S,
    config: DigRuntimeConfig,
    vanity_tax: Arc<dyn DigRuntimeVanityTaxPort>,
    low_priority_tax: Arc<dyn DigRuntimeLowPriorityTaxPort>,
}

impl DigRuntimeService<SqliteDigRuntimeStore> {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        Self::new(SqliteDigRuntimeStore::new(path))
    }

    #[must_use]
    pub fn sqlite_with_config(path: impl AsRef<Path>, config: DigRuntimeConfig) -> Self {
        Self::with_config(SqliteDigRuntimeStore::new(path), config)
    }

    pub fn tunnel_info(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigRuntimeTunnelInfo>, DigRuntimeStoreError> {
        let snapshot = self.snapshot(discord_id, guild_id)?;
        Ok(snapshot.tunnel.map(|tunnel| DigRuntimeTunnelInfo {
            depth: tunnel.depth,
            total_digs: tunnel.total_digs,
            total_jc_earned: tunnel.total_jc_earned,
            last_dig_at: tunnel.last_dig_at,
            pickaxe_tier: tunnel.pickaxe_tier,
            prestige_level: tunnel.prestige_level,
            luminosity: tunnel.luminosity,
            hard_hat_charges: tunnel.hard_hat_charges,
            tunnel_name: tunnel.tunnel_name,
            route_state: tunnel.route_state,
        }))
    }

    pub fn flex_data(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigRuntimeFlexData>, DigRuntimeStoreError> {
        let snapshot = self.snapshot(discord_id, guild_id)?;
        Ok(snapshot.tunnel.map(|tunnel| {
            let mut progress = crate::dig_service::BOSS_BOUNDARIES
                .into_iter()
                .map(|boundary| (boundary.to_string(), "active".to_owned()))
                .collect::<BTreeMap<_, _>>();
            if let Ok(Value::Object(stored)) = serde_json::from_str::<Value>(&tunnel.boss_progress)
            {
                for (boundary, value) in stored {
                    let status = match value {
                        Value::String(status) => Some(status),
                        Value::Object(entry) => entry
                            .get("status")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        _ => None,
                    }
                    .unwrap_or_else(|| "active".to_owned());
                    progress.insert(boundary, status);
                }
            }
            let titles = progress
                .values()
                .all(|status| status == "defeated")
                .then(|| "Boss Slayer".to_owned())
                .into_iter()
                .collect();
            let prestige_level = tunnel.prestige_level.max(0);
            let stars = usize::try_from(prestige_level.min(5)).unwrap_or_default();
            DigRuntimeFlexData {
                tunnel_name: tunnel.tunnel_name,
                depth: tunnel.depth,
                total_digs: tunnel.total_digs,
                total_jc_earned: tunnel.total_jc_earned,
                prestige_level,
                prestige_emoji: "⭐".repeat(stars),
                titles,
                streak: tunnel.streak_days,
                layer: layer_at(tunnel.depth).name.to_owned(),
            }
        }))
    }

    pub fn leaderboard(
        &self,
        guild_id: i64,
    ) -> Result<Vec<DigRuntimeLeaderboardRow>, DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        Ok(
            dig_runtime_store::top_tunnel_depth_rows(&connection, guild_id)?
                .into_iter()
                .map(|(name, depth)| DigRuntimeLeaderboardRow { name, depth })
                .collect(),
        )
    }

    pub fn hall_of_fame(
        &self,
        guild_id: i64,
    ) -> Result<Vec<DigRuntimeHallOfFameRow>, DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        Ok(
            dig_runtime_store::hall_of_fame_depth_rows(&connection, guild_id)?
                .into_iter()
                .map(|(name, user_id, score, prestige)| DigRuntimeHallOfFameRow {
                    name,
                    user_id,
                    score,
                    prestige,
                })
                .collect(),
        )
    }

    /// Apply the channel-admission penalty through the application boundary.
    /// Discord transport decides *when* this is needed; SQLite settlement
    /// stays here so a missing player cannot accidentally create money.
    pub fn debit_channel_penalty(
        &self,
        discord_id: i64,
        guild_id: i64,
        amount: i64,
    ) -> Result<(), DigRuntimeStoreError> {
        let mut connection = self.store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        dig_runtime_store::debit_player_balance(&transaction, amount, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Set the tunnel trap through the application boundary.  The migrated
    /// repository owns the exact free-use/cost/CAS policy; Discord only
    /// renders the typed result.
    pub fn set_trap(
        &self,
        discord_id: i64,
        guild_id: i64,
        game_date: &str,
    ) -> Result<SetTrapOutcome, DigRuntimeStoreError> {
        DigInventoryRepository::new(&self.store.path)
            .set_trap_atomic(discord_id, Some(guild_id), game_date)
            .map_err(|error| DigRuntimeStoreError::Inventory(error.to_string()))
    }

    /// Purchase cave-in insurance through the same typed application seam as
    /// every other Dig money mutation.
    pub fn buy_insurance(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<BuyInsuranceOutcome, DigRuntimeStoreError> {
        DigInventoryRepository::new(&self.store.path)
            .buy_insurance_atomic(discord_id, Some(guild_id), now)
            .map_err(|error| DigRuntimeStoreError::Inventory(error.to_string()))
    }

    /// Ensure and return today's authored weather rows for presentation.  A
    /// separate read model keeps the provider independent of SQLite and
    /// preserves the canonical weather descriptions/IDs.
    pub fn weather(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
    ) -> Result<Vec<DigWeatherEntry>, DigRuntimeStoreError> {
        DigWeatherRepository::new(&self.store.path)
            .ensure_for_day(guild_id, game_date, now)
            .map_err(|error| DigRuntimeStoreError::Weather(error.to_string()))
    }

    pub fn weather_projection(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
    ) -> Result<Vec<DigRuntimeWeatherPresentation>, DigRuntimeStoreError> {
        Ok(self
            .weather(guild_id, game_date, now)?
            .into_iter()
            .filter_map(|entry| {
                let definition = entry.definition()?;
                Some(DigRuntimeWeatherPresentation {
                    layer: definition.layer.to_owned(),
                    name: definition.name.to_owned(),
                    description: definition.description.to_owned(),
                    effects: weather_effects(definition.id),
                })
            })
            .collect())
    }

    /// Help another tunnel and append the canonical audit row atomically.
    pub fn help(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<String, DigRuntimeStoreError> {
        let mut connection = self.store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = dig_runtime_store::tunnel_depth_and_max(&transaction, target_id, guild_id)?;
        let Some((depth, max_depth)) = target else {
            transaction.commit()?;
            return Ok("That miner has not started a tunnel yet.".to_owned());
        };
        let depth_after = depth.saturating_add(1);
        dig_runtime_store::set_tunnel_depth_and_max(
            &transaction,
            depth_after,
            max_depth.max(depth_after),
            target_id,
            guild_id,
        )?;
        dig_runtime_store::insert_dig_action(
            &transaction,
            guild_id,
            actor_id,
            Some(target_id),
            "help",
            depth,
            depth_after,
            0,
            "{}",
            now,
        )?;
        transaction.commit()?;
        Ok(format!(
            "You steadied <@{target_id}>'s tunnel and helped them reach depth **{depth_after}**."
        ))
    }

    pub fn gift_relic(
        &self,
        owner_id: i64,
        target_id: i64,
        guild_id: i64,
        artifact_id: &str,
        now: i64,
    ) -> Result<bool, DigRuntimeStoreError> {
        if owner_id == target_id {
            return Ok(false);
        }
        let mut connection = self.store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_exists = dig_runtime_store::player_exists(&transaction, target_id, guild_id)?;
        if !target_exists {
            transaction.commit()?;
            return Ok(false);
        }
        let changed = dig_runtime_store::transfer_relic(
            &transaction,
            target_id,
            guild_id,
            now,
            owner_id,
            artifact_id,
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn reset_cooldown(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigAdminMutationOutcome, DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        let changed = dig_runtime_store::reset_tunnel_cooldown(&connection, discord_id, guild_id)?;
        Ok(if changed == 1 {
            DigAdminMutationOutcome::Applied
        } else {
            DigAdminMutationOutcome::MissingTunnel
        })
    }

    pub fn set_depth(
        &self,
        discord_id: i64,
        guild_id: i64,
        depth: i64,
    ) -> Result<DigAdminMutationOutcome, DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        let changed = dig_runtime_store::set_tunnel_depth_reset_cooldown(
            &connection,
            depth.max(0),
            discord_id,
            guild_id,
        )?;
        Ok(if changed == 1 {
            DigAdminMutationOutcome::Applied
        } else {
            DigAdminMutationOutcome::MissingTunnel
        })
    }

    pub fn respec(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<String, DigRuntimeStoreError> {
        const COST: i64 = 50;
        let mut connection = self.store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = dig_runtime_store::player_balance(&transaction, discord_id, guild_id)?;
        let Some(balance) = balance else {
            transaction.commit()?;
            return Ok("You must be registered first.".to_owned());
        };
        let tunnel_stats =
            dig_runtime_store::tunnel_stat_allocation(&transaction, discord_id, guild_id)?;
        let Some((strength, smarts, stamina, _stat_points)) = tunnel_stats else {
            transaction.commit()?;
            return Ok("You don't have any allocated S points to reset.".to_owned());
        };
        let returned_points = strength.saturating_add(smarts).saturating_add(stamina);
        if returned_points <= 0 {
            transaction.commit()?;
            return Ok("You don't have any allocated S points to reset.".to_owned());
        }
        if balance < COST {
            transaction.commit()?;
            return Ok(format!(
                "Respec costs {COST} {JOPACOIN_EMOTE}; your balance is {balance}."
            ));
        }

        let detail = serde_json::json!({
            "cost": COST,
            "returned_points": returned_points,
            "previous_stats": {
                "strength": strength,
                "smarts": smarts,
                "stamina": stamina,
            },
        })
        .to_string();
        dig_runtime_store::set_ledger_context(
            &transaction,
            "dig",
            discord_id,
            "miner_respec",
            "s_points",
            "dig miner respec debit",
            &detail,
        )?;
        let debited = dig_runtime_store::debit_player_balance_if_sufficient(
            &transaction,
            COST,
            discord_id,
            guild_id,
        )?;
        dig_runtime_store::clear_ledger_context(&transaction)?;
        if debited != 1 {
            transaction.rollback()?;
            return Ok(format!(
                "Respec costs {COST} {JOPACOIN_EMOTE}; your balance is {balance}."
            ));
        }
        dig_runtime_store::refresh_player_lowest_balance(&transaction, discord_id, guild_id)?;
        let changed = dig_runtime_store::respec_tunnel_stats(&transaction, discord_id, guild_id)?;
        if changed == 0 {
            transaction.rollback()?;
            return Ok("You don't have a tunnel yet. Use /dig go to start.".to_owned());
        }
        dig_runtime_store::insert_dig_action(
            &transaction,
            guild_id,
            discord_id,
            None,
            "miner_respec",
            0,
            0,
            -COST,
            &detail,
            now,
        )?;
        transaction.commit()?;
        Ok(format!("Respec complete. Spent {COST} {JOPACOIN_EMOTE}."))
    }

    pub fn autobuy(
        &self,
        discord_id: i64,
        guild_id: i64,
        item: &str,
        enabled: bool,
    ) -> Result<(), DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        let value = i64::from(enabled);
        let changed = match item {
            "torch" => {
                dig_runtime_store::set_auto_buy_torch(&connection, value, discord_id, guild_id)?
            }
            "hard_hat" => {
                dig_runtime_store::set_auto_buy_hard_hat(&connection, value, discord_id, guild_id)?
            }
            "both" => dig_runtime_store::set_auto_buy_torch_and_hard_hat(
                &connection,
                value,
                discord_id,
                guild_id,
            )?,
            _ => return Err(DigRuntimeStoreError::StateConflict),
        };
        if changed == 0 {
            return Err(DigRuntimeStoreError::MissingTunnel);
        }
        Ok(())
    }
}

impl<S> DigRuntimeService<S>
where
    S: DigRuntimeStore,
{
    #[must_use]
    pub fn new(store: S) -> Self {
        Self::with_config(store, DigRuntimeConfig::default())
    }

    #[must_use]
    pub fn with_config(store: S, config: DigRuntimeConfig) -> Self {
        Self {
            store,
            config,
            vanity_tax: Arc::new(NoDigRuntimeVanityTax),
            low_priority_tax: Arc::new(NoDigRuntimeLowPriorityTax),
        }
    }

    #[must_use]
    pub fn with_vanity_tax(mut self, vanity_tax: Arc<dyn DigRuntimeVanityTaxPort>) -> Self {
        self.vanity_tax = vanity_tax;
        self
    }

    #[must_use]
    pub fn with_low_priority_tax(
        mut self,
        low_priority_tax: Arc<dyn DigRuntimeLowPriorityTaxPort>,
    ) -> Self {
        self.low_priority_tax = low_priority_tax;
        self
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub const fn config(&self) -> &DigRuntimeConfig {
        &self.config
    }

    /// Read one aggregate snapshot for transport projections and component
    /// recovery.  The provider receives typed state rather than issuing SQL.
    pub fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError> {
        self.store.snapshot(discord_id, guild_id)
    }

    /// Execute the mechanical Dig and persist the immutable Discord delivery
    /// projection against the resulting action.  The provider receives one
    /// typed execution object and never reconstructs a render from a newer
    /// tunnel snapshot after a process restart.
    pub fn dig_with_delivery(
        &self,
        request: DigRuntimeRequest,
        context: DigRuntimeDeliveryContext,
    ) -> Result<DigRuntimeExecution, DigRuntimeStoreError> {
        let outcome = self.dig_inner(request, Some(context.clone()))?;
        let delivery = outcome
            .success
            .then(|| {
                build_delivery_snapshot(
                    &outcome,
                    request.discord_id,
                    request.guild_id,
                    context,
                    request.now,
                )
            })
            .flatten();
        Ok(DigRuntimeExecution { outcome, delivery })
    }

    fn commit_dig(
        &self,
        request: DigRuntimeCommit,
        outcome: DigRuntimeOutcome,
        context: Option<&DigRuntimeDeliveryContext>,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        let Some(context) = context else {
            return self.store.commit(request);
        };
        let (discord_id, guild_id) = request
            .next
            .tunnel
            .as_ref()
            .map_or((0, 0), |tunnel| (tunnel.discord_id, tunnel.guild_id));
        let committed_at = request.now;
        self.store.commit_with_delivery(
            request,
            DigRuntimeDeliveryDraft {
                discord_id,
                guild_id,
                outcome,
                context: context.clone(),
                committed_at,
            },
        )
    }

    pub fn pending_deliveries(
        &self,
        query: DigRuntimePendingDeliveryQuery,
    ) -> Result<Vec<DigRuntimeDeliverySnapshot>, DigRuntimeStoreError> {
        self.store.pending_deliveries(query)
    }

    pub fn mark_delivery_delivered(
        &self,
        request: DigRuntimeMarkDelivered,
    ) -> Result<bool, DigRuntimeStoreError> {
        self.store.mark_delivery_delivered(request)
    }

    pub fn rebind_pending_delivery_channel(
        &self,
        request: DigRuntimeRebindDeliveryChannel,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        self.store.rebind_pending_delivery_channel(request)
    }

    pub fn finalize_delivery(
        &self,
        request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        self.store.finalize_delivery(request)
    }

    /// Settle the durable Blood Pact effect for a committed delivery.  Callers
    /// should invoke this immediately after loading a pending delivery and
    /// before asking the flavor service to finalize/render it.
    pub fn settle_blood_pact_delivery(
        &self,
        request: DigRuntimeSettleBloodPact,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        self.store
            .settle_blood_pact_delivery(request, self.config.minigame_jc_delta_scale)
    }

    pub fn dig(
        &self,
        request: DigRuntimeRequest,
    ) -> Result<DigRuntimeOutcome, DigRuntimeStoreError> {
        self.dig_inner(request, None)
    }

    fn dig_inner(
        &self,
        request: DigRuntimeRequest,
        delivery_context: Option<DigRuntimeDeliveryContext>,
    ) -> Result<DigRuntimeOutcome, DigRuntimeStoreError> {
        let mut snapshot = self.store.snapshot(request.discord_id, request.guild_id)?;
        if !snapshot.registered {
            return Ok(DigRuntimeOutcome::blocked(
                &snapshot,
                "You need to register first. Use /player register.",
                0,
                0,
            ));
        }
        let now = request.now;
        let mut current = snapshot.clone();
        // A tunnel can be restored from a partial migration with a zero
        // counter but an existing timestamp/depth.  Python only treats the
        // truly unstarted shape as the guaranteed-safe first Dig.
        let first_dig = current.tunnel.as_ref().is_none_or(|tunnel| {
            tunnel.total_digs == 0 && tunnel.last_dig_at.is_none() && tunnel.depth == 0
        });
        if current.tunnel.is_none() {
            current.tunnel = Some(DigRuntimeTunnel::new(
                request.discord_id,
                request.guild_id,
                now,
            ));
        }
        let tunnel = current
            .tunnel
            .as_ref()
            .expect("Dig always has a staged tunnel")
            .clone();
        let equipped_relics = current
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect::<Vec<_>>();
        let relics = RelicSet::new(equipped_relics);
        if route_status(tunnel.route_state.as_deref()).choice_required {
            return Ok(DigRuntimeOutcome {
                success: false,
                error: Some("Choose your route before digging again.".to_owned()),
                depth_before: tunnel.depth,
                depth_after: tunnel.depth,
                advance: 0,
                jc_earned: 0,
                vanity_tax: 0,
                low_priority_tax: 0,
                balance_after: current.balance,
                tunnel_name: tunnel.tunnel_name.clone(),
                milestone_bonus: 0,
                streak_bonus: 0,
                bankruptcy_penalty: 0,
                luminosity_after: tunnel.luminosity,
                luminosity_drained: 0,
                corruption_description: None,
                mutation_names: mutations_from_json(tunnel.mutations.as_deref())
                    .into_iter()
                    .map(|mutation| mutation.name.to_owned())
                    .collect(),
                tip: String::new(),
                cave_in: false,
                cave_in_detail: None,
                event_id: None,
                artifact_id: None,
                boss_boundary: None,
                first_dig: false,
                paid_dig_cost: 0,
                cooldown_remaining: 0,
                paid_dig_available: false,
                items_used: Vec::new(),
                consumed_item_ids: Vec::new(),
                action_id: None,
                route_choice_required: true,
                pickaxe_tier: tunnel.pickaxe_tier,
                pet_dig_bonus: 0,
                pet_name: None,
                forced_event_consumed: false,
                relic_trim_notice: false,
                weather: None,
                active_curse: None,
            });
        }

        // Python claims Slow Drip after the pending-route gate but before
        // prestige/boss/cooldown admission.  Its gross cap and wallet credit
        // are already durable by the time any of those later gates return.
        let slow_drip_claim =
            match self
                .store
                .claim_slow_drip(&current, now, &self.config.economy_event)
            {
                Ok(claim) => claim,
                Err(error) => {
                    let _ = error;
                    None
                }
            };
        if let Some(claim) = slow_drip_claim.as_ref() {
            current.balance = current.balance.saturating_add(claim.credit_jc);
            snapshot.balance = snapshot.balance.saturating_add(claim.credit_jc);
        }

        // Re-open a boss that was already reached by a previous Dig before
        // applying cap/cooldown/paid gates.  This is presentation-only: no
        // new Dig is consumed, and Slow Drip (above) remains the one intended
        // pre-boss side effect.
        if !first_dig && let Some(boundary) = parked_boss_boundary(&tunnel) {
            return Ok(DigRuntimeOutcome {
                success: true,
                error: None,
                depth_before: tunnel.depth,
                depth_after: tunnel.depth,
                advance: 0,
                jc_earned: 0,
                vanity_tax: 0,
                low_priority_tax: 0,
                balance_after: current.balance,
                tunnel_name: tunnel.tunnel_name.clone(),
                milestone_bonus: 0,
                streak_bonus: 0,
                bankruptcy_penalty: 0,
                luminosity_after: tunnel.luminosity,
                luminosity_drained: 0,
                corruption_description: None,
                mutation_names: mutations_from_json(tunnel.mutations.as_deref())
                    .into_iter()
                    .map(|mutation| mutation.name.to_owned())
                    .collect(),
                tip: String::new(),
                cave_in: false,
                cave_in_detail: None,
                event_id: None,
                artifact_id: None,
                boss_boundary: Some(boundary),
                first_dig: false,
                paid_dig_cost: 0,
                cooldown_remaining: 0,
                paid_dig_available: false,
                items_used: Vec::new(),
                consumed_item_ids: Vec::new(),
                action_id: None,
                route_choice_required: false,
                pickaxe_tier: tunnel.pickaxe_tier,
                pet_dig_bonus: 0,
                pet_name: None,
                forced_event_consumed: false,
                relic_trim_notice: false,
                weather: None,
                active_curse: None,
            });
        }

        // Reject the hard wall after Slow Drip has settled, but before daily
        // weather initialization or any other Dig-only side effect.
        if !first_dig
            && current
                .tunnel
                .as_ref()
                .is_some_and(|tunnel| tunnel.depth >= PRESTIGE_HARD_CAP)
        {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                "The tunnel has reached the prestige cap. Ascend to begin a new run.",
                0,
                0,
            ));
        }
        let today = game_date_for_timestamp(now as f64).unwrap_or_else(|_| "unknown".to_owned());
        let mut tunnel = current
            .tunnel
            .as_ref()
            .expect("Dig always has a staged tunnel")
            .clone();
        let mutations = mutations_from_json(tunnel.mutations.as_deref());
        let mutation_fx = mutation_effects(&mutations);
        let mana_effects = if first_dig {
            ManaEffects::default()
        } else {
            self.store
                .mana_effects(request.discord_id, request.guild_id, &today)?
        };
        let paid_count = if tunnel.paid_dig_date.as_deref() == Some(today.as_str()) {
            usize::try_from(tunnel.paid_digs_today.max(0)).unwrap_or(usize::MAX)
        } else {
            0
        };
        let ascension_markup = ascension_effects(tunnel.prestige_level as i32)
            .get("paid_dig_cost_multiplier")
            .and_then(|effect| effect.number())
            .unwrap_or(0.0);
        let marked_up_paid_cost = paid_dig_cost(paid_count, 0, ascension_markup);
        let relic_paid_cost =
            relic_aware_paid_cost(marked_up_paid_cost, tunnel.stat_stamina, &relics);
        let mana_paid_multiplier = (1.0 + mana_effects.dig_paid_cost_modifier_pct).max(0.0);
        let paid_cost_preview =
            ((relic_paid_cost as f64 * mana_paid_multiplier.max(0.0)) as i64).max(1);
        let (curse_fx, _curse_remaining) =
            active_curse_effects(tunnel.temp_curses.as_deref()).unwrap_or_default();
        let restless_bonus = mutation_fx
            .get("cooldown_bonus_seconds")
            .and_then(|effect| effect.number())
            .unwrap_or(0.0)
            .max(0.0) as i64;
        let before_stamina = if injury_slows_cooldown(tunnel.injury_state.as_deref()) {
            6 * 3_600
        } else {
            3_600_i64.saturating_add(restless_bonus)
        };
        let stat_fx = miner_stat_effects(
            MinerStats::new(0, 0, tunnel.stat_stamina.max(0))
                .expect("normalized persisted stamina is non-negative"),
        );
        let after_stamina = (before_stamina as f64 * stat_fx.cooldown_multiplier) as i64;
        let cooldown_seconds = (after_stamina.max(1) as f64
            * (1.0
                + curse_fx
                    .cooldown_penalty
                    .clamp(0.0, crate::dig_event_threats::COOLDOWN_CURSE_CAP)))
            as i64;
        let cooldown_seconds = cooldown_seconds
            .saturating_sub(mana_effects.dig_cooldown_reduction_seconds.max(0))
            .max(0);
        let cooldown = cooldown_remaining(tunnel.last_dig_at, now, cooldown_seconds);
        // A paid flag only charges while it is actually bypassing an active
        // cooldown.  Python treats a paid click on an already-ready Dig as a
        // free Dig; keep the preview cost for a blocked free click, but do not
        // feed it into any committed state or relic context.
        let paid_charge_active = !first_dig && request.paid && cooldown > 0;
        let paid_cost = if paid_charge_active {
            paid_cost_preview
        } else {
            0
        };
        if !first_dig && cooldown > 0 && !request.paid {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                format!("Dig on cooldown ({cooldown}s remaining)."),
                paid_cost_preview,
                cooldown,
            ));
        }
        if paid_charge_active && current.balance < paid_cost_preview {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                format!(
                    "Paid dig costs {paid_cost_preview} JC but you only have {} JC.",
                    current.balance
                ),
                paid_cost_preview,
                cooldown,
            ));
        }

        // Auto-buy is admitted only for an imminent real Dig, after the
        // paid-cost reserve is known.  The existing-schema repository makes
        // each requested consumable fail-soft (reserve first, then purchase
        // only what the live balance/inventory can support).
        let mut auto_purchases = Vec::new();
        if !first_dig {
            let mut selections = Vec::new();
            if tunnel.auto_buy_hard_hat {
                selections.push(AutoBuySelection {
                    item_type: "hard_hat",
                    price: 8,
                });
            }
            let should_buy_torch = current
                .tunnel
                .as_ref()
                .is_some_and(|t| should_auto_buy_torch(t.depth, t.luminosity));
            if tunnel.auto_buy_torch && should_buy_torch {
                selections.push(AutoBuySelection {
                    item_type: "torch",
                    price: 6,
                });
            }
            // A queued hook is consumed the same dig for 5 charges that only
            // drain on cave-ins, so admission gates on having no usable
            // charges left; otherwise an opted-in miner would pay for a new
            // hook on every single dig while charges stack unbounded.
            if tunnel.auto_buy_grappling_hook && tunnel.grappling_hook_charges == 0 {
                selections.push(AutoBuySelection {
                    item_type: "grappling_hook",
                    price: consumable("grappling_hook")
                        .expect("grappling_hook is a shop consumable")
                        .cost,
                });
            }
            if !selections.is_empty() {
                auto_purchases = self.store.auto_buy_items(AutoBuyRequest {
                    discord_id: request.discord_id,
                    guild_id: Some(request.guild_id),
                    selections: &selections,
                    reserved_balance: paid_cost,
                    inventory_limit: crate::dig_loot::MAX_INVENTORY_SLOTS,
                    created_at: now,
                    observed_balance: Some(current.balance),
                })?;
                let weather = current.weather.clone();
                current = self.store.snapshot(request.discord_id, request.guild_id)?;
                current.weather = weather;
                snapshot = current.clone();
            }
        }

        // Weather rows are a real Dig side effect.  Initialize them only after
        // every cooldown/paid admission check and fail-soft auto-buy refresh
        // has passed, so blocked clicks do not create a guild weather history
        // row and the live roll sees the refreshed aggregate snapshot.
        if !first_dig {
            current.weather = self.store.ensure_weather(request.guild_id, &today, now)?;
        }

        // Injury is consumed immediately before the mechanical roll.  The
        // reduced-advance injury halves that roll; a slower-cooldown injury
        // only changes admission and must never halve advancement.
        let mut injury_reduces_advance = false;
        if !first_dig && let Some(next_tunnel) = current.tunnel.as_mut() {
            injury_reduces_advance = tick_injury(next_tunnel);
        }

        // Settle the pet's lazy work anchor only after route/cooldown
        // admission. A blocked interaction must not reserve a work claim.
        let pet_work = if self.config.pet_decay_per_day > 0 {
            self.store.preview_pet_dig_work(
                request.discord_id,
                request.guild_id,
                now,
                self.config.pet_decay_per_day,
                self.config.entropy_secret,
            )?
        } else {
            None
        };
        let pet_name = pet_work.as_ref().map(|work| work.pet_name.clone());

        if first_dig {
            let mut entropy = SeededLootEntropy::new(seed_for(request, self.config.entropy_secret));
            let advance = entropy.advance(3, 7);
            let jc_roll = entropy.advance(1, 5);
            let scaled_jc = scale_dig_minigame_jc(
                jc_roll,
                multiplier_millionths(self.config.minigame_jc_delta_scale),
            );
            let (daily_adjusted_jc, economy_multiplier) = self.store.adjust_daily_reward(
                request.guild_id,
                scaled_jc,
                now,
                &self.config.economy_event,
            )?;
            let mut state = tunnel_state(&current, None);
            let mut first = apply_first_dig(&mut state, advance, jc_roll, daily_adjusted_jc, now);
            let requested_pet_blocks = pet_work.as_ref().map_or(0, |work| work.available_blocks());
            let gated_base = apply_boss_gate(0, first.advance, &state.defeated_bosses);
            let gated_total = apply_boss_gate(
                0,
                first.advance.saturating_add(requested_pet_blocks),
                &state.defeated_bosses,
            );
            let pet_dig_bonus = gated_total
                .advance
                .saturating_sub(gated_base.advance)
                .min(requested_pet_blocks);
            first.advance = gated_total.advance;
            state.depth = gated_total.depth_after;
            state.max_depth = state.max_depth.max(gated_total.depth_after);
            if let Some(next_tunnel) = current.tunnel.as_mut() {
                next_tunnel.streak_days = 1;
                next_tunnel.streak_last_date = Some(today.clone());
            }
            let pet_work_claim = pet_work
                .as_ref()
                .and_then(|work| work.claim(pet_dig_bonus).ok());
            let next = apply_state(&current, state, &today, false, first.jc_earned);
            let commit = DigRuntimeCommit {
                expected: DigRuntimeVersion::from(&snapshot),
                next,
                delivery_draft: None,
                consumed_item_ids: Vec::new(),
                pet_work_claim,
                consume_overgrowth: false,
                depth_before: 0,
                depth_after: first.advance,
                jc_delta: first.jc_earned,
                vanity_tax: 0,
                low_priority_tax: 0,
                balance_cost: 0,
                action_type: "dig".to_owned(),
                detail: serde_json::json!({
                    "first_dig": true,
                    "gross_jc": daily_adjusted_jc,
                    "minigame_scaled_jc": scaled_jc,
                    "economy_adjusted_jc": daily_adjusted_jc,
                    "economy_reward_multiplier": economy_multiplier,
                    "vanity_tax": 0,
                    "low_priority_tax": 0,
                    "pet_dig_bonus": pet_dig_bonus,
                    "boss_boundary": gated_total.boss_encounter,
                })
                .to_string(),
                now,
            };
            let balance_after = commit.next.balance;
            let mut first_outcome = DigRuntimeOutcome {
                success: true,
                error: None,
                depth_before: 0,
                depth_after: first.advance,
                advance: first.advance,
                jc_earned: first.jc_earned,
                vanity_tax: 0,
                low_priority_tax: 0,
                balance_after,
                tunnel_name: current.tunnel.as_ref().map_or_else(
                    || "Unknown Tunnel".to_owned(),
                    |tunnel| tunnel.tunnel_name.clone(),
                ),
                milestone_bonus: 0,
                streak_bonus: 0,
                bankruptcy_penalty: 0,
                luminosity_after: current
                    .tunnel
                    .as_ref()
                    .map_or(LUMINOSITY_MAX, |tunnel| tunnel.luminosity),
                luminosity_drained: 0,
                corruption_description: None,
                mutation_names: current
                    .tunnel
                    .as_ref()
                    .map(|tunnel| mutations_from_json(tunnel.mutations.as_deref()))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mutation| mutation.name.to_owned())
                    .collect(),
                tip: "Welcome to the mines! Use /dig again after the cooldown.".to_owned(),
                cave_in: false,
                cave_in_detail: None,
                event_id: None,
                artifact_id: None,
                boss_boundary: gated_total.boss_encounter,
                first_dig: true,
                paid_dig_cost: 0,
                cooldown_remaining: 0,
                paid_dig_available: false,
                items_used: Vec::new(),
                consumed_item_ids: Vec::new(),
                action_id: None,
                route_choice_required: false,
                pickaxe_tier: current
                    .tunnel
                    .as_ref()
                    .map_or(0, |tunnel| tunnel.pickaxe_tier),
                pet_dig_bonus,
                pet_name,
                forced_event_consumed: false,
                relic_trim_notice: false,
                weather: None,
                active_curse: None,
            };
            let receipt =
                self.commit_dig(commit, first_outcome.clone(), delivery_context.as_ref())?;
            first_outcome.balance_after = receipt.balance_after;
            first_outcome.action_id = Some(receipt.action_id);
            return Ok(first_outcome);
        }

        if let Some(next_tunnel) = current.tunnel.as_mut() {
            apply_luminosity_refill(next_tunnel, now);
        }
        tunnel = current
            .tunnel
            .as_ref()
            .expect("admitted Dig requires a tunnel")
            .clone();
        let depth_before = tunnel.depth;
        let active_route = parse_route_state(tunnel.route_state.as_deref())
            .and_then(|state| state.selected)
            .and_then(|route_id| route_by_id(&route_id));
        let weather = current
            .weather
            .iter()
            .find(|weather| weather.layer_name == layer_at(depth_before).name);
        let weather_fx = weather
            .map(|weather| weather_effects(&weather.weather_id))
            .unwrap_or_default();
        let weather_id = weather.map(|weather| weather.weather_id.as_str());
        let mana_weather_combo = weather_combo_modifiers(&mana_effects, weather_code(weather_id));
        let gear_fx = gear_effects(&current.gear, &tunnel);
        let ascension = ascension_effects(tunnel.prestige_level as i32);
        let ascension_number = |key: &str| {
            ascension
                .get(key)
                .and_then(|effect| effect.number())
                .unwrap_or(0.0)
        };
        let route_number = |key: &str| {
            active_route
                .and_then(|route| route_effect(route, key))
                .unwrap_or(0.0)
        };
        let route_luminosity_delta = route_number("luminosity_drain_multiplier")
            - route_number("luminosity_drain_reduction");
        let route_event_delta = route_number("event_chance_multiplier");
        let storm_hazard_negated = storm_negates_hazard(&relics, weather_code(weather_id));
        let cave_weather_bonus = if storm_hazard_negated {
            0.0
        } else {
            weather_fx.cave_in_bonus
        };
        let active_pickaxe_tier = active_pickaxe_tier(&current.gear, &tunnel);
        let prestige_perks =
            serde_json::from_str::<Vec<String>>(&tunnel.prestige_perks).unwrap_or_default();
        let perk_fx = aggregate_prestige_perk_effects(&prestige_perks);
        let (buff_fx, buff_remaining) =
            active_buff_effects(tunnel.temp_buffs.as_deref()).unwrap_or_default();
        // Corruption is the first request-local random policy in Python. It
        // must consume the same entropy stream as the subsequent cave roll,
        // rather than a second seed that would shift only some Dig paths.
        let mut entropy = SeededLootEntropy::new(seed_for(request, self.config.entropy_secret));
        let corruption = roll_corruption(tunnel.prestige_level as i32, &mut entropy);
        let corruption_bonus = corruption
            .as_ref()
            .and_then(|corruption| {
                corruption
                    .effects
                    .iter()
                    .find(|effect| effect.key == "cave_in_bonus")
                    .and_then(|effect| effect.value.number())
            })
            .unwrap_or(0.0);
        let mana_hazard_modifier = mana_effects.dig_hazard_modifier;
        let overgrowth_active =
            self.store
                .overgrowth_active(request.discord_id, request.guild_id, now)?;
        let thick_skin = mutation_fx
            .get("daily_cave_in_shield")
            .and_then(|effect| effect.boolean())
            .unwrap_or(false)
            && tunnel.thick_skin_date.as_deref() != Some(today.as_str());
        // The cave probability is evaluated after Python's complete
        // luminosity pipeline. Project that value before entering the loot
        // stage (which owns the first entropy draw) and apply the same value
        // again below when the staged tunnel is settled.
        let projected_luminosity = {
            let layer = layer_at(depth_before);
            let mut base_drain = layer.luminosity_drain;
            if active_pickaxe_tier >= 6 {
                base_drain = base_drain.saturating_sub(base_drain / 4);
            }
            base_drain = base_drain.saturating_add(deep_luminosity_drain_bonus(depth_before));
            let drain = (base_drain as f64 * (1.0 + route_luminosity_delta).max(0.0)) as i64;
            let mut luminosity = tunnel.luminosity.saturating_sub(drain).max(0);
            let mut drained = tunnel.luminosity.saturating_sub(luminosity).max(0);
            if current
                .inventory
                .iter()
                .any(|item| item.queued && item.item_type == "torch")
            {
                luminosity = (luminosity + 50).min(LUMINOSITY_MAX);
            }
            if relics.contains("spore_cloak") && drained > 0 {
                let restored = drained / 2;
                luminosity = (luminosity + restored).min(LUMINOSITY_MAX);
                drained = drained.saturating_sub(restored);
            }
            for multiplier in [
                ascension_number("luminosity_drain_multiplier"),
                weather_fx.luminosity_drain_multiplier,
            ] {
                if multiplier > 0.0 && drained > 0 {
                    let extra = (drained as f64 * multiplier) as i64;
                    luminosity = luminosity.saturating_sub(extra).max(0);
                    drained = drained.saturating_add(extra);
                }
            }
            if curse_fx.luminosity_drain > 0 {
                luminosity = luminosity.saturating_sub(curse_fx.luminosity_drain).max(0);
                drained = drained.saturating_add(curse_fx.luminosity_drain);
            }
            let lantern_stub = apply_lantern_stub_restore(
                &relics,
                LanternStubRestoreInput {
                    luminosity_after: luminosity,
                    last_dig_at: tunnel.last_dig_at,
                    lantern_stub_date: tunnel.lantern_stub_date.as_deref(),
                    today: &today,
                },
            );
            luminosity = lantern_stub.luminosity_after;
            if prestige_perk_contains(&tunnel.prestige_perks, "deep_sight") && drained > 0 {
                let restored = (drained / 4).max(1);
                luminosity = (luminosity + restored).min(LUMINOSITY_MAX);
            }
            luminosity
        };
        let cave_in_policy = CaveInChanceRequest {
            base_layer: layer_at(depth_before).cave_in_chance,
            route_bonus: route_number("cave_in_bonus"),
            ascension_bonus: ascension_number("cave_in_bonus"),
            curse_bonus: curse_fx.cave_in_bonus,
            weather_bonus: cave_weather_bonus,
            corruption_bonus,
            luminosity: projected_luminosity,
            dark_adaptation: prestige_perks.iter().any(|perk| perk == "dark_adaptation"),
            dark_sight: mutation_fx
                .get("ignore_luminosity_cave_in")
                .and_then(|effect| effect.boolean())
                .unwrap_or(false),
            perk_reduction: perk_fx.get("cave_in_reduction").copied().unwrap_or(0.0),
            active_pickaxe_reduction: gear_fx.cave_in_reduction,
            active_buff_reduction: buff_fx.cave_in_reduction,
            smarts: tunnel.stat_smarts,
            lantern: current
                .inventory
                .iter()
                .any(|item| item.queued && item.item_type == "lantern"),
            crystal_compass: relics.contains("crystal_compass"),
            prestige_multiplier: crate::dig_service::prestige_cave_in_multiplier(
                tunnel.prestige_level,
            ),
            overgrowth: overgrowth_active,
            mana_hazard_modifier,
            thick_skin,
        };
        let loot_modifiers = DigLootModifiers {
            cave_in_chance_bonus: route_number("cave_in_bonus")
                + cave_weather_bonus
                + ascension_number("cave_in_bonus")
                + curse_fx.cave_in_bonus
                - gear_fx.cave_in_reduction,
            cave_in_chance_multiplier: crate::dig_service::prestige_cave_in_multiplier(
                tunnel.prestige_level,
            ),
            advance_bonus: route_number("advance_bonus") as i64
                + weather_fx.advance_bonus
                + curse_fx.advance_bonus
                + gear_fx.advance_bonus
                + buff_fx.advance_bonus,
            advance_min: None,
            advance_max: active_route
                .and_then(|route| route_effect(route, "advance_max_penalty"))
                .map(|penalty| (layer_at(depth_before).advance_range.1 - penalty as i64).max(1)),
            event_chance_multiplier: event_chance_factor(
                ascension_number("event_chance_multiplier"),
                weather_fx.event_chance_multiplier,
                route_event_delta,
            )
            .max(0.0),
            luminosity_drain_multiplier: (1.0 + route_luminosity_delta).max(0.0),
            luminosity_drain_bonus: deep_luminosity_drain_bonus(depth_before),
            luminosity_pickaxe_reduction: active_pickaxe_tier >= 6,
            injury_reduces_advance,
            jc_multiplier: (1.0 + weather_fx.jc_multiplier + ascension_number("jc_multiplier")
                - ascension_number("jc_layer_penalty"))
            .max(0.0),
            jc_bonus: weather_fx.jc_bonus + gear_fx.loot_bonus + curse_fx.jc_bonus,
            // Ordinary artifacts are settled below, after the cave branch
            // has established the final depth. Keep the loot-stage carrier
            // neutral so it cannot consume a pre-cave artifact roll.
            artifact_multiplier: 1.0,
            cave_in_loss_bonus: if storm_hazard_negated {
                0
            } else {
                weather_fx.cave_in_loss_bonus + route_number("cave_in_loss_bonus") as i64
            },
            cave_in_loss_cap: weather_fx.cave_in_loss_cap.or_else(|| {
                active_route
                    .and_then(|route| route_effect(route, "cave_in_loss_cap"))
                    .map(|value| value as i64)
            }),
            cave_in_policy: Some(cave_in_policy),
            defer_event_selection: true,
        };
        let sonar_skip_active_this_dig = tunnel.sonar_skip_pending;
        let mut consumed_item_ids = current
            .inventory
            .iter()
            .filter(|item| {
                item.queued
                    && is_dig_consumable(&item.item_type)
                    && !is_boss_prep_item(&item.item_type)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let mut loot = DigLootService::new(DigRuntimeLootRepository::new(current.clone()), entropy);
        let mut loot_outcome =
            loot.dig_with_modifiers(request.discord_id, request.guild_id, now, loot_modifiers);
        if !loot_outcome.success {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                loot_outcome
                    .error
                    .unwrap_or_else(|| "Dig did not commit.".to_owned()),
                paid_cost,
                cooldown,
            ));
        }
        // The loot stage has now performed the ordinary refill-aware drain.
        // Apply the remaining Python hooks in their authored order against
        // the same staged tunnel: queued torch, Spore Cloak, ascension and
        // weather extra drain, curse drain, Lantern Stub, then Deep Sight.
        let mut staged = loot.repository().snapshot().clone();
        let luminosity_before_drain = tunnel.luminosity;
        let mut luminosity_drained = staged.tunnel.as_ref().map_or(0, |next_tunnel| {
            luminosity_before_drain
                .saturating_sub(next_tunnel.luminosity)
                .max(0)
        });
        let has_torch = loot_outcome.items_used.contains(&"torch");
        if let Some(next_tunnel) = staged.tunnel.as_mut() {
            if has_torch {
                next_tunnel.luminosity = (next_tunnel.luminosity + 50).min(LUMINOSITY_MAX);
            }
            if relics.contains("spore_cloak") && luminosity_drained > 0 {
                let restored = luminosity_drained / 2;
                next_tunnel.luminosity = (next_tunnel.luminosity + restored).min(LUMINOSITY_MAX);
                luminosity_drained = luminosity_drained.saturating_sub(restored);
            }
            let apply_extra_drain = |luminosity: &mut i64, drained: &mut i64, multiplier: f64| {
                if multiplier > 0.0 && *drained > 0 {
                    let extra = (*drained as f64 * multiplier) as i64;
                    *luminosity = luminosity.saturating_sub(extra).max(0);
                    *drained = drained.saturating_add(extra);
                }
            };
            apply_extra_drain(
                &mut next_tunnel.luminosity,
                &mut luminosity_drained,
                ascension_number("luminosity_drain_multiplier"),
            );
            apply_extra_drain(
                &mut next_tunnel.luminosity,
                &mut luminosity_drained,
                weather_fx.luminosity_drain_multiplier,
            );
            if curse_fx.luminosity_drain > 0 {
                next_tunnel.luminosity = next_tunnel
                    .luminosity
                    .saturating_sub(curse_fx.luminosity_drain)
                    .max(0);
                luminosity_drained = luminosity_drained.saturating_add(curse_fx.luminosity_drain);
            }
            let lantern_stub = apply_lantern_stub_restore(
                &relics,
                LanternStubRestoreInput {
                    luminosity_after: next_tunnel.luminosity,
                    last_dig_at: tunnel.last_dig_at,
                    lantern_stub_date: next_tunnel.lantern_stub_date.as_deref(),
                    today: &today,
                },
            );
            next_tunnel.luminosity = lantern_stub.luminosity_after;
            if lantern_stub.lantern_stub_date.is_some() {
                next_tunnel.lantern_stub_date = lantern_stub.lantern_stub_date;
            }
            if prestige_perk_contains(&tunnel.prestige_perks, "deep_sight")
                && luminosity_drained > 0
            {
                let restored = (luminosity_drained / 4).max(1);
                next_tunnel.luminosity = (next_tunnel.luminosity + restored).min(LUMINOSITY_MAX);
            }
            // Hard Hat is deliberately last in Python's luminosity pipeline:
            // the ten-point protection cost follows ordinary drain, Torch,
            // Spore Cloak, ascension/weather/curse drains, Lantern Stub, and
            // Deep Sight restoration.
            if loot_outcome.hard_hat_absorbed {
                next_tunnel.luminosity = next_tunnel.luminosity.saturating_sub(10).max(0);
            }
        }
        let layer = layer_at(depth_before);
        let helltide_tax = self.store.helltide_tax(request.guild_id, now)?.max(0);
        let economy_multiplier = self.store.daily_reward_multiplier(
            request.guild_id,
            now,
            &self.config.economy_event,
        )?;
        let economy_multiplier_basis_points = (economy_multiplier.max(0.0) * 10_000.0 + 0.5) as i64;
        let streak_days = next_daily_streak(&tunnel, &today);
        let streak_reward = crate::dig_service::streak_bonus(streak_days);
        let next_cavein_free_streak = tunnel.cavein_free_streak.max(0).saturating_add(1);
        let mut gross_jc = 0_i64;
        let mut yield_multiplier_millionths = DIG_YIELD_MULTIPLIER_SCALE;
        let mut pre_cap_jc = None;
        if !loot_outcome.cave_in {
            gross_jc = loot
                .entropy_mut()
                .advance(layer.jc_range.0, layer.jc_range.1);
            let luminosity_after = staged
                .tunnel
                .as_ref()
                .map_or(LUMINOSITY_MAX, |next_tunnel| next_tunnel.luminosity);
            let relic_yield_multiplier = {
                let mut entropy = LootRelicEntropy(loot.entropy_mut());
                relic_jc_yield_multiplier(
                    &relics,
                    YieldContext {
                        weather_code: weather_code(weather_id),
                        luminosity: Some(luminosity_after),
                        is_first_dig_today: is_first_dig_of_day(tunnel.last_dig_at, now),
                        is_paid_dig: paid_charge_active,
                        include_random: true,
                    },
                    &mut entropy,
                )
            };
            let preview_state = tunnel_state(&staged, paid_charge_active.then_some(paid_cost));
            let requested_pet_blocks = pet_work.as_ref().map_or(0, |work| work.available_blocks());
            let projected_depth = apply_boss_gate(
                depth_before,
                loot_outcome.advance.saturating_add(requested_pet_blocks),
                &preview_state.defeated_bosses,
            )
            .depth_after;
            let composed_multiplier = loot_modifiers.jc_multiplier
                * relic_yield_multiplier
                * mana_weather_combo.yield_mult
                * luminosity_jc_multiplier(luminosity_after)
                * post_pinnacle_decay_factor(projected_depth, &relics);
            yield_multiplier_millionths = multiplier_millionths(composed_multiplier);
            let buff_multiplier_millionths = multiplier_millionths(buff_fx.yield_multiplier);
            let perk_flat =
                round_prestige_perk_bonus_half_up(perk_fx.get("jc_bonus").copied().unwrap_or(0.0));
            let mut base_jc = scale_dig_yield_once(
                gross_jc,
                &[yield_multiplier_millionths, buff_multiplier_millionths],
            )
            .saturating_add(loot_modifiers.jc_bonus)
            .saturating_add(i64::from(relics.contains("magma_heart")))
            .saturating_add(perk_flat);
            let corruption_number = |key: &str| {
                corruption.as_ref().and_then(|corruption| {
                    corruption
                        .effects
                        .iter()
                        .find(|effect| effect.key == key)
                        .and_then(|effect| effect.value.number())
                })
            };
            let corruption_flag = |key: &str| {
                corruption.as_ref().is_some_and(|corruption| {
                    corruption
                        .effects
                        .iter()
                        .any(|effect| effect.key == key && effect.value.boolean().unwrap_or(false))
                })
            };
            if let Some(fixed) = corruption_number("fixed_jc") {
                base_jc = fixed as i64;
            } else if corruption_flag("double_half_jc") {
                base_jc = base_jc.saturating_sub(base_jc.rem_euclid(2));
            } else {
                base_jc = base_jc
                    .saturating_sub(corruption_number("jc_penalty").unwrap_or(0.0).max(0.0) as i64);
            }
            let zero_chance = mutation_fx
                .get("zero_jc_chance")
                .and_then(|effect| effect.number())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            if zero_chance > 0.0 && loot.entropy_mut().unit() < zero_chance {
                base_jc = 0;
            } else {
                base_jc = base_jc.max(0);
            }
            base_jc = apply_mana_base_yield(base_jc, &mana_effects, loot.entropy_mut());
            if relics.contains("prospectors_streak") {
                base_jc = base_jc.saturating_add(next_cavein_free_streak.min(20));
            }
            pre_cap_jc = Some(base_jc);
        }
        let mut cave_in_grappling_absorbed = false;
        let mut cave_reward_gross = 0_i64;
        let cave_loss = if loot_outcome.cave_in {
            let (minimum, maximum) = CAVE_IN_BLOCK_LOSS_RANGES[cave_in_band(depth_before)];
            let rolled = loot
                .entropy_mut()
                .advance(i64::from(minimum), i64::from(maximum));
            let mut loss = rolled
                .saturating_add(loot_modifiers.cave_in_loss_bonus)
                .saturating_add(
                    mutation_fx
                        .get("cave_in_loss_bonus")
                        .and_then(|effect| effect.number())
                        .unwrap_or(0.0) as i64,
                );
            // Weather/route caps apply before player reductions; this is
            // intentionally not folded into the domain's old reinforcement
            // helper, which applied the cap too early.
            if let Some(cap) = loot_modifiers.cave_in_loss_cap {
                loss = loss.min(cap.max(0));
            }
            if let Some(&reduction) = perk_fx.get("cave_in_loss_reduction")
                && reduction > 0.0
            {
                loss = (loss as f64 * (1.0_f64 - reduction)).max(0.0) as i64;
            }
            if relics.contains("patient_stone") {
                loss = (loss as f64 * 0.7).max(0.0) as i64;
            }
            if tunnel.reinforced_until > now {
                loss = loss.min(8);
            }
            let loot_chance = mutation_fx
                .get("cave_in_loot_chance")
                .and_then(|effect| effect.number())
                .unwrap_or(0.0);
            if loot_chance > 0.0 && loot.entropy_mut().unit() < loot_chance {
                let minimum = mutation_fx
                    .get("cave_in_loot_min")
                    .and_then(|effect| effect.number())
                    .unwrap_or(1.0) as i64;
                let maximum = mutation_fx
                    .get("cave_in_loot_max")
                    .and_then(|effect| effect.number())
                    .unwrap_or(3.0) as i64;
                cave_reward_gross = cave_reward_gross.saturating_add(
                    loot.entropy_mut()
                        .advance(minimum.max(0), maximum.max(minimum)),
                );
            }
            let loss_before_save = loss;
            if loss_before_save > 0 && relics.contains("gamblers_charm") {
                cave_reward_gross = cave_reward_gross.saturating_add((loss_before_save / 2).max(1));
            }
            if staged
                .tunnel
                .as_ref()
                .is_some_and(|next_tunnel| next_tunnel.grappling_hook_charges > 0)
            {
                cave_in_grappling_absorbed = true;
                loss = 0;
            } else if tunnel.pickaxe_tier >= 7 {
                loss = loss.saturating_sub(1).max(1);
            }
            loss
        } else {
            0
        };
        let mut cave_in_detail_value = None;
        let mut cave_in_medical_requested = 0_i64;
        let mut catastrophic_depth_after = None;
        let mut catastrophic_cave_in = false;
        if loot_outcome.cave_in {
            let band = cave_in_band(depth_before);
            let injury_bonus = mutation_fx
                .get("injury_duration_bonus")
                .and_then(|effect| effect.number())
                .unwrap_or(0.0) as i64;
            let applicability = CaveInApplicability::new(
                !staged.inventory.is_empty(),
                staged
                    .gear
                    .iter()
                    .any(|piece| piece.equipped && piece.durability > 0),
                staged
                    .tunnel
                    .as_ref()
                    .is_some_and(|next_tunnel| next_tunnel.luminosity > 0),
                staged
                    .tunnel
                    .as_ref()
                    .is_some_and(|next_tunnel| next_tunnel.hard_hat_charges > 0),
            );
            let mut cave_rng = CaveInLootRng(loot.entropy_mut());
            let catastrophic =
                !cave_in_grappling_absorbed && roll_catastrophic_cave_in(band, &mut cave_rng);
            if cave_in_grappling_absorbed {
                if let Some(next_tunnel) = staged.tunnel.as_mut() {
                    next_tunnel.grappling_hook_charges =
                        next_tunnel.grappling_hook_charges.saturating_sub(1).max(0);
                }
                cave_in_detail_value = Some(serde_json::json!({
                    "type": "cushioned",
                    "block_loss": 0,
                    "message": "Cave-in! Your grappling line snapped taut and absorbed the impact.",
                }));
            } else if catastrophic {
                catastrophic_cave_in = true;
                let insured = tunnel.insured_until.is_some_and(|expires| expires > now);
                let (depth_after, insurance_saved, total_loss) = catastrophic_cave_in_depth(
                    depth_before,
                    cave_loss,
                    loot_modifiers.cave_in_loss_cap,
                    insured,
                );
                catastrophic_depth_after = Some(depth_after);
                let gear_broken =
                    apply_cave_in_gear_ticks(&mut staged.gear, CAVE_IN_CATASTROPHIC_GEAR_TICKS);
                cave_in_medical_requested = i64::from(cave_rng.random_inclusive(
                    CAVE_IN_CATASTROPHIC_MEDICAL_BILL.0,
                    CAVE_IN_CATASTROPHIC_MEDICAL_BILL.1,
                ));
                let stun_digs = i64::from(cave_rng.random_inclusive(
                    CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE.0,
                    CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE.1,
                )) + injury_bonus;
                if let Some(next_tunnel) = staged.tunnel.as_mut() {
                    next_tunnel.temp_buffs = None;
                    next_tunnel.injury_state = Some(
                        serde_json::json!({
                            "type": "slower_cooldown",
                            "digs_remaining": stun_digs,
                        })
                        .to_string(),
                    );
                    next_tunnel.cavein_free_streak = 0;
                }
                cave_in_detail_value = Some(serde_json::json!({
                    "type": "catastrophic",
                    "block_loss": total_loss,
                    "stun_digs": stun_digs,
                    "depth_after": depth_after,
                    "insurance_saved": insurance_saved,
                    "gear_broken": gear_broken,
                    "message": format!(
                        "CATASTROPHIC CAVE-IN! Tunnel folds in on itself. Lost {} blocks, paid {{jc_lost}} JC, stunned for {} digs, gear shattered.{}",
                        total_loss,
                        stun_digs,
                        if insurance_saved { " Insurance held the depth." } else { "" },
                    ),
                }));
            } else {
                let consequence = pick_cave_in_consequence(band, applicability, &mut cave_rng);
                if let Some(next_tunnel) = staged.tunnel.as_mut() {
                    next_tunnel.cavein_free_streak = 0;
                }
                match consequence.as_str() {
                    "stun" => {
                        let stun_digs = i64::from(CAVE_IN_STUN_DIGS_BY_BAND[band]) + injury_bonus;
                        if let Some(next_tunnel) = staged.tunnel.as_mut() {
                            next_tunnel.injury_state = Some(
                                serde_json::json!({
                                    "type": "slower_cooldown",
                                    "digs_remaining": stun_digs,
                                })
                                .to_string(),
                            );
                        }
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "stun",
                            "block_loss": cave_loss,
                            "message": format!(
                                "Cave-in! Lost {} blocks and you're stunned.", cave_loss
                            ),
                        }));
                    }
                    "injury" => {
                        let injury_digs =
                            i64::from(CAVE_IN_INJURY_DIGS_BY_BAND[band]) + injury_bonus;
                        if let Some(next_tunnel) = staged.tunnel.as_mut() {
                            next_tunnel.injury_state = Some(
                                serde_json::json!({
                                    "type": "reduced_advance",
                                    "digs_remaining": injury_digs,
                                })
                                .to_string(),
                            );
                        }
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "injury",
                            "block_loss": cave_loss,
                            "message": format!(
                                "Cave-in! Lost {} blocks and you're injured (reduced digging for {} digs).",
                                cave_loss, injury_digs
                            ),
                        }));
                    }
                    "medical_bill" => {
                        let (minimum, maximum) = CAVE_IN_MEDICAL_BILL_RANGES[band];
                        cave_in_medical_requested =
                            i64::from(cave_rng.random_inclusive(minimum, maximum));
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "medical_bill",
                            "block_loss": cave_loss,
                            "jc_lost": "{jc_lost}",
                            "message": format!(
                                "Cave-in! Lost {} blocks and paid {{jc_lost}} JC in medical bills.",
                                cave_loss
                            ),
                        }));
                    }
                    "gear_nick" => {
                        let gear_broken = apply_cave_in_gear_ticks(&mut staged.gear, 1);
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "gear_nick",
                            "block_loss": cave_loss,
                            "gear_broken": gear_broken,
                            "message": format!(
                                "Cave-in! Lost {} blocks. Gear took a beating.", cave_loss
                            ),
                        }));
                    }
                    "spilled_satchel" if !staged.inventory.is_empty() => {
                        let index = usize::try_from(
                            cave_rng.random_inclusive(
                                0,
                                u32::try_from(staged.inventory.len().saturating_sub(1))
                                    .unwrap_or(u32::MAX),
                            ),
                        )
                        .unwrap_or_default()
                        .min(staged.inventory.len().saturating_sub(1));
                        let item = staged.inventory.remove(index);
                        let item_name = consumable(&item.item_type).map_or_else(
                            || item.item_type.clone(),
                            |definition| definition.name.to_owned(),
                        );
                        consumed_item_ids.push(item.id);
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "spilled_satchel",
                            "block_loss": cave_loss,
                            "item_lost": item_name,
                            "message": format!(
                                "Cave-in! Lost {} blocks. Your {} spills into the dark.",
                                cave_loss, item_name
                            ),
                        }));
                    }
                    "snuffed_light"
                        if staged
                            .tunnel
                            .as_ref()
                            .is_some_and(|next_tunnel| next_tunnel.luminosity > 0) =>
                    {
                        if let Some(next_tunnel) = staged.tunnel.as_mut() {
                            next_tunnel.luminosity = (next_tunnel.luminosity - 25).max(0);
                        }
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "snuffed_light",
                            "block_loss": cave_loss,
                            "message": format!(
                                "Cave-in! Lost {} blocks. The dark presses in.", cave_loss
                            ),
                        }));
                    }
                    "cracked_hat"
                        if staged
                            .tunnel
                            .as_ref()
                            .is_some_and(|next_tunnel| next_tunnel.hard_hat_charges > 0) =>
                    {
                        if let Some(next_tunnel) = staged.tunnel.as_mut() {
                            next_tunnel.hard_hat_charges =
                                (next_tunnel.hard_hat_charges - 1).max(0);
                        }
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "cracked_hat",
                            "block_loss": cave_loss,
                            "message": format!(
                                "Cave-in! Lost {} blocks. Your hard hat takes a chunk out of itself.",
                                cave_loss
                            ),
                        }));
                    }
                    _ => {
                        let (minimum, maximum) = CAVE_IN_MEDICAL_BILL_RANGES[band];
                        cave_in_medical_requested =
                            i64::from(cave_rng.random_inclusive(minimum, maximum));
                        cave_in_detail_value = Some(serde_json::json!({
                            "type": "medical_bill",
                            "block_loss": cave_loss,
                            "jc_lost": "{jc_lost}",
                            "message": format!(
                                "Cave-in! Lost {} blocks and paid {{jc_lost}} JC in medical bills.",
                                cave_loss
                            ),
                        }));
                    }
                }
            }
        }
        if loot_outcome.items_used.contains(&"reinforcement")
            && let Some(next_tunnel) = staged.tunnel.as_mut()
        {
            next_tunnel.reinforced_until = next_tunnel
                .reinforced_until
                .max(now.saturating_add(crate::dig_loot::REINFORCEMENT_SECONDS));
        }
        let balance_before_outcome = staged.balance;
        let mut state = tunnel_state(&staged, paid_charge_active.then_some(paid_cost));
        state.depth = depth_before;
        let bankruptcy_penalty_games = if loot_outcome.cave_in {
            0
        } else {
            self.store
                .bankruptcy_penalty_games(request.discord_id, request.guild_id)?
        };
        let mut outcome_input = DigOutcomeInput {
            advance: loot_outcome.advance,
            gross_jc: if loot_outcome.cave_in {
                cave_reward_gross
            } else {
                gross_jc
            },
            cave_in: loot_outcome.cave_in,
            cave_in_loss: cave_loss,
            dynamite: loot_outcome.items_used.contains(&"dynamite"),
            depth_charge: loot_outcome.items_used.contains(&"depth_charge"),
            authored_event: request.forced_event,
            yield_buff_multiplier_millionths: (!loot_outcome.cave_in)
                .then(|| multiplier_millionths(buff_fx.yield_multiplier)),
            yield_multiplier_millionths: (!loot_outcome.cave_in)
                .then_some(yield_multiplier_millionths),
            pre_cap_jc,
            economy_reward_multiplier_basis_points: economy_multiplier_basis_points,
            economy_before_positive_scale: loot_outcome.cave_in,
            minigame_jc_delta_scale_millionths: multiplier_millionths(
                self.config.minigame_jc_delta_scale,
            ),
            streak_bonus: streak_reward,
            streak_bonus_multiplier_basis_points: bonus_basis_points(
                perk_fx
                    .get("streak_bonus_multiplier")
                    .copied()
                    .unwrap_or(0.0),
            ),
            milestone_multiplier_basis_points: bonus_basis_points(ascension_number(
                "milestone_multiplier",
            )),
            overgrowth_bonus: i64::from(overgrowth_active).saturating_mul(10),
            helltide_tax,
            profit_policy: DigProfitPolicy {
                bankruptcy_keep_basis_points: if bankruptcy_penalty_games > 0 {
                    self.config.bankruptcy_penalty_keep_basis_points
                } else {
                    DIG_REWARD_BASIS_POINTS
                },
                vanity_tax_basis_points: 0,
                low_priority_tax_basis_points: 0,
            },
            ..DigOutcomeInput::default()
        };
        if !loot_outcome.cave_in
            && (mana_effects.plains_tithe_rate > 0.0 || mana_effects.blue_tax_rate > 0.0)
        {
            let mut preview_state = state.clone();
            let preview = apply_dig_outcome(&mut preview_state, outcome_input, now);
            let total_jc = preview.economy_adjusted_jc.max(0);
            let mut modified = total_jc;
            if mana_effects.plains_tithe_rate > 0.0 && modified > 0 {
                let tithe = proportional_mana_yield_tax(modified, mana_effects.plains_tithe_rate);
                let event_key = delivery_context.as_ref().map_or_else(
                    || {
                        format!(
                            "dig-request:{}:{}:{}:{}:{}",
                            request.guild_id,
                            request.discord_id,
                            request.now,
                            u8::from(request.paid),
                            u8::from(request.forced_event),
                        )
                    },
                    |context| format!("dig-interaction:{}", context.interaction_id),
                );
                let credited = self
                    .store
                    .credit_plains_tithe(
                        request.discord_id,
                        request.guild_id,
                        total_jc,
                        tithe,
                        &event_key,
                    )
                    .unwrap_or_default()
                    .unwrap_or_default()
                    .max(0)
                    .min(modified);
                modified = modified.saturating_sub(credited);
            }
            if mana_effects.blue_tax_rate > 0.0 && modified > 0 {
                let tax = proportional_mana_yield_tax(modified, mana_effects.blue_tax_rate);
                modified = modified.saturating_sub(tax);
            }
            outcome_input.mana_yield_tax = total_jc.saturating_sub(modified);
        }
        let mut outcome = apply_dig_outcome(&mut state, outcome_input, now);
        if let Some(depth_after) = catastrophic_depth_after {
            state.depth = depth_after;
            outcome.depth_after = depth_after;
            outcome.advance = 0;
        }
        let cave_in_medical_cost = cave_in_medical_requested.min(state.balance.max(0));
        if cave_in_medical_cost > 0 {
            state.balance -= cave_in_medical_cost;
            outcome.jc_earned = outcome.jc_earned.saturating_sub(cave_in_medical_cost);
            if let Some(detail) = cave_in_detail_value.as_mut()
                && let Some(object) = detail.as_object_mut()
            {
                object.insert("jc_lost".to_owned(), Value::from(cave_in_medical_cost));
                if let Some(message) = object
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    object.insert(
                        "message".to_owned(),
                        Value::String(
                            message.replace("{jc_lost}", &cave_in_medical_cost.to_string()),
                        ),
                    );
                }
            }
        }
        let mana_paid_cave_refund = if outcome.cave_in && paid_charge_active {
            proportional_mana_yield_tax(paid_cost, mana_effects.dig_paid_refund_on_caveins)
        } else {
            0
        };
        state.balance = state.balance.saturating_add(mana_paid_cave_refund);
        let base_advance = outcome.advance;
        let mut pet_dig_bonus = 0;
        // A cave-in is a loss-only branch in Python: the pet's stored work is
        // not consumed. For a successful roll, try each additional block in
        // order and retain the first candidate that reaches a boss gate. The
        // gate returns depth boundary-1 plus the boundary identity, exactly
        // preserving the pending encounter while retaining unspent work.
        if !outcome.cave_in
            && outcome.boss_encounter.is_none()
            && let Some(work) = pet_work.as_ref()
        {
            for blocks in 1..=work.available_blocks() {
                let mut candidate_state =
                    tunnel_state(&staged, paid_charge_active.then_some(paid_cost));
                candidate_state.depth = depth_before;
                let candidate = apply_dig_outcome(
                    &mut candidate_state,
                    DigOutcomeInput {
                        advance: loot_outcome.advance.saturating_add(blocks),
                        // The normal cap applies to the base roll before
                        // Python adds pet work.  Mark this candidate as an
                        // already-capped application so pet blocks are not
                        // incorrectly clipped at the main-dig ceiling.
                        authored_event: true,
                        ..outcome_input
                    },
                    now,
                );
                // Python claims only the pet blocks that survived the same
                // boss cap as the base advance.  The requested loop count is
                // not necessarily the applied count at the boundary.
                pet_dig_bonus = candidate.advance.saturating_sub(base_advance);
                state = candidate_state;
                outcome = candidate;
                if outcome.boss_encounter.is_some() {
                    break;
                }
            }
        }
        let pet_work_claim = pet_work
            .as_ref()
            .and_then(|work| work.claim(pet_dig_bonus).ok());
        // Vanity taxation is a single post-policy adjustment.  The basis is
        // the reward after Mana and Helltide withholding but before profit
        // policies, reconstructed as net + bankruptcy withholding; this is
        // deliberately independent of bankruptcy and is skipped for cave-ins.
        if !outcome.cave_in {
            let vanity_tax_basis = outcome
                .jc_earned
                .saturating_add(outcome.bankruptcy_penalty)
                .max(0);
            // Withholding comes out of the reward still in hand, which the
            // bankruptcy penalty has already reduced. Clamping to the basis
            // alone would debit the wallet whenever the configured keep rate
            // leaves less in hand than the tax.
            let vanity_tax = self
                .vanity_tax
                .calculate_tax(request.discord_id, request.guild_id, vanity_tax_basis)
                .max(0)
                .min(vanity_tax_basis)
                .min(outcome.jc_earned.max(0));
            // The low-priority sink reads the same pre-subtraction basis, so
            // the two taxes stack additively instead of compounding. What is
            // actually withheld, though, comes out of the reward still in
            // hand, which the bankruptcy penalty has already reduced: clamping
            // the pair to `vanity_tax_basis` would let a heavily penalized
            // digger be debited more than they earned. Trim the low-priority
            // share, so a taxed reward may fall to zero but never becomes a
            // wallet debit.
            let low_priority_tax = self
                .low_priority_tax
                .calculate_tax(request.discord_id, request.guild_id, vanity_tax_basis)
                .max(0)
                .min(outcome.jc_earned.max(0).saturating_sub(vanity_tax).max(0));
            let withheld = vanity_tax.saturating_add(low_priority_tax);
            if withheld > 0 {
                state.balance = state.balance.saturating_sub(withheld);
                outcome.jc_earned = outcome.jc_earned.saturating_sub(withheld);
                outcome.vanity_tax = vanity_tax;
                outcome.low_priority_tax = low_priority_tax;
            }
        }
        let wallet_reward_delta = state
            .balance
            .saturating_sub(balance_before_outcome)
            .saturating_add(if paid_charge_active { paid_cost } else { 0 });
        let total_jc_increment = if outcome.cave_in {
            0
        } else {
            outcome.jc_earned.max(0)
        };
        staged = apply_state(
            &staged,
            state,
            &today,
            paid_charge_active,
            total_jc_increment,
        );
        if let Some(next_tunnel) = staged.tunnel.as_mut() {
            if outcome.cave_in {
                next_tunnel.cavein_free_streak = 0;
            } else {
                next_tunnel.cavein_free_streak = next_cavein_free_streak;
                next_tunnel.current_run_jc = next_tunnel
                    .current_run_jc
                    .saturating_add(outcome.jc_earned.max(0));
            }
        }
        // Python rolls ordinary artifacts only after the cave branch has
        // settled the final post-boss depth. Keep this roll on the same
        // entropy stream as the cave/JC/event stages, but stage the new row on
        // the final snapshot so the outer CAS persists it atomically.
        let mut artifact_id = None;
        let skip_artifact = outcome.cave_in
            || corruption.as_ref().is_some_and(|corruption| {
                corruption.effects.iter().any(|effect| {
                    effect.key == "skip_artifact" && effect.value.boolean().unwrap_or(false)
                })
            });
        if !skip_artifact {
            let weather_factor = if weather_fx.artifact_multiplier > 0.0 {
                weather_fx.artifact_multiplier
            } else {
                1.0
            };
            let ascension_factor = {
                let factor = ascension_number("artifact_multiplier");
                if factor > 0.0 { factor } else { 1.0 }
            };
            let treasure_bonus = mutation_fx
                .get("artifact_chance_bonus")
                .and_then(|effect| effect.number())
                .unwrap_or(0.0);
            let find_modifier = artifact_rate_modifier(
                relics.contains("echo_stone"),
                weather_factor,
                route_artifact_multiplier(active_route),
                ascension_factor,
                treasure_bonus,
                post_pinnacle_decay_factor(outcome.depth_after, &relics),
            );
            let owned = staged
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone())
                .collect::<BTreeSet<_>>();
            let mut entropy = DigPrestige4Entropy(loot.entropy_mut());
            if let Some(stage) = roll_artifact_stage(
                ArtifactRollPlan {
                    depth: outcome.depth_after,
                    rate_modifier: find_modifier,
                    skip_artifact: false,
                },
                &owned,
                &mut entropy,
            ) {
                artifact_id = Some(stage.definition.id.to_owned());
                let local_id = staged
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.id)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                staged.artifacts.push(DigRuntimeArtifact {
                    id: local_id,
                    artifact_id: stage.definition.id.to_owned(),
                    is_relic: stage.definition.is_relic,
                    equipped: false,
                });
                if let Some(next_tunnel) = staged.tunnel.as_mut() {
                    next_tunnel.current_run_artifacts = next_tunnel
                        .current_run_artifacts
                        .saturating_add(stage.current_run_artifacts_delta);
                }
            }
        }
        if let Some(next_tunnel) = staged.tunnel.as_mut() {
            next_tunnel.streak_days = streak_days;
            next_tunnel.streak_last_date = Some(today.clone());
        }
        if let Some((_, remaining)) = active_curse_effects(tunnel.temp_curses.as_deref())
            && let Some(next_tunnel) = staged.tunnel.as_mut()
        {
            next_tunnel.temp_curses = if remaining <= 1 {
                None
            } else {
                let mut curse =
                    serde_json::from_str::<Value>(tunnel.temp_curses.as_deref().unwrap_or("{}"))
                        .unwrap_or_else(|_| serde_json::json!({}));
                if let Some(value) = curse.get_mut("digs_remaining") {
                    *value = Value::from(remaining - 1);
                }
                Some(curse.to_string())
            };
        }
        // Report the hex the miner carries *out* of this Dig, so the count
        // shown is what still lies ahead rather than what was just spent. A
        // curse expiring on this Dig correctly reports nothing.
        let active_curse = staged
            .tunnel
            .as_ref()
            .and_then(|next_tunnel| active_curse_summary(next_tunnel.temp_curses.as_deref()));
        if !catastrophic_cave_in
            && buff_remaining > 0
            && let Some(next_tunnel) = staged.tunnel.as_mut()
        {
            next_tunnel.temp_buffs = if buff_remaining <= 1 {
                None
            } else {
                let mut buff =
                    serde_json::from_str::<Value>(tunnel.temp_buffs.as_deref().unwrap_or("{}"))
                        .unwrap_or_else(|_| serde_json::json!({}));
                if let Some(value) = buff.get_mut("digs_remaining") {
                    *value = Value::from(buff_remaining - 1);
                }
                Some(buff.to_string())
            };
        }
        // Canonical event selection is deliberately late: Python rolls the
        // gate after the post-boss `new_depth` is known, and the selected
        // catalog event sees that same depth, luminosity, quest snapshot, and
        // boss flag.  The loot stage already consumed exactly one gate draw;
        // only a passing gate (or Sonar preview/forced selection) consumes a
        // catalog-selection draw from that same entropy stream.
        let void_bait_charge_used = !outcome.cave_in
            && (tunnel.void_bait_digs > 0 || loot_outcome.items_used.contains(&"void_bait"));
        let event_roll = loot_outcome.event_roll_bits.map(f64::from_bits);
        let event_luminosity = staged
            .tunnel
            .as_ref()
            .map_or(100, |next_tunnel| next_tunnel.luminosity);
        let luminosity_event_multiplier = if event_luminosity <= 0 {
            3.0
        } else if event_luminosity <= 25 {
            2.5
        } else if event_luminosity < 76 {
            1.5
        } else {
            1.0
        };
        let mut event_gate_chance = match layer.name {
            "Crystal" | "Magma" => 0.27,
            "Abyss" | "Frozen Core" => 0.31,
            "Fungal Depths" => 0.38,
            "The Hollow" => 0.45,
            _ => 0.22,
        } * luminosity_event_multiplier
            * loot_modifiers.event_chance_multiplier.max(0.0)
            * (1.0
                + mutation_fx
                    .get("event_chance_bonus")
                    .and_then(|effect| effect.number())
                    .unwrap_or(0.0));
        if void_bait_charge_used {
            event_gate_chance = (event_gate_chance * 2.0).min(0.75);
        } else {
            event_gate_chance = event_gate_chance.min(0.75);
        }
        if request.forced_event {
            event_gate_chance = 1.0;
        }
        let event_gate_passed = !outcome.cave_in
            && (request.forced_event || event_roll.is_some_and(|roll| roll < event_gate_chance));
        let needs_event_selection = event_gate_passed || loot_outcome.event_preview_included;
        let mut selected_event = None;
        let mut preview_event = None;
        if needs_event_selection && staged.tunnel.is_some() {
            let quest =
                self.store
                    .event_quest_snapshot(request.discord_id, request.guild_id, now)?;
            let in_boss = outcome.boss_encounter.is_some();
            let rare_multiplier = ascension_number("rare_event_multiplier");
            let legendary_multiplier = ascension_number("legendary_event_multiplier");
            if event_gate_passed {
                selected_event = self.store.canonical_event_id_for_snapshot(
                    DigRuntimeCanonicalEventRequest {
                        snapshot: &staged,
                        quest: &quest,
                        depth: outcome.depth_after,
                        luminosity: event_luminosity,
                        in_boss,
                        void_bait_active: void_bait_charge_used,
                        rare_event_multiplier: rare_multiplier,
                        legendary_event_multiplier: legendary_multiplier,
                        selection_roll_bits: loot.entropy_mut().unit().to_bits(),
                    },
                )?;
            }
            if loot_outcome.event_preview_included {
                preview_event = self.store.canonical_event_id_for_snapshot(
                    DigRuntimeCanonicalEventRequest {
                        snapshot: &staged,
                        quest: &quest,
                        depth: outcome.depth_after,
                        luminosity: event_luminosity,
                        in_boss,
                        void_bait_active: void_bait_charge_used,
                        rare_event_multiplier: rare_multiplier,
                        legendary_event_multiplier: legendary_multiplier,
                        selection_roll_bits: loot.entropy_mut().unit().to_bits(),
                    },
                )?;
            }
        }
        loot_outcome.event = selected_event;
        if sonar_skip_active_this_dig && loot_outcome.event.is_some() {
            loot_outcome.sonar_skipped = true;
            loot_outcome.event_preview = loot_outcome.event.clone().or(preview_event);
            loot_outcome.event = None;
            if !loot_outcome.event_preview_included
                && let Some(next_tunnel) = staged.tunnel.as_mut()
            {
                next_tunnel.sonar_skip_pending = false;
            }
        } else if loot_outcome.event_preview_included {
            loot_outcome.event_preview = preview_event;
        }
        let event_id = loot_outcome.event.clone();
        if let Some(tunnel) = staged.tunnel.as_mut()
            && event_id.is_some()
        {
            tunnel.current_run_events = tunnel.current_run_events.saturating_add(1);
        }
        let pickaxe_tier = staged
            .tunnel
            .as_ref()
            .map_or(0, |tunnel| tunnel.pickaxe_tier);
        let forced_event_consumed = request.forced_event && event_id.is_some();
        let detail = serde_json::json!({
            "cave_in": outcome.cave_in,
            // Keep the audit field aligned with Python: this is the ordinary
            // rolled loss, while catastrophic milestone loss belongs to the
            // nested cave-in detail. The final depth delta can be smaller
            // when the milestone rollback is less than the ordinary roll.
            "block_loss": outcome.cave_in.then_some(cave_loss),
            "cave_in_detail": cave_in_detail_value.clone(),
            "event": event_id.clone(),
            "artifact": artifact_id.clone(),
            "items_used": loot_outcome.items_used,
            "paid": paid_charge_active,
            "pet_dig_bonus": pet_dig_bonus,
            "mana_yield_tax": outcome.mana_yield_tax,
            "mana_paid_cave_refund": mana_paid_cave_refund,
            "helltide_tax": helltide_tax,
            "bankruptcy_penalty": outcome.bankruptcy_penalty,
            "vanity_tax": outcome.vanity_tax,
            "low_priority_tax": outcome.low_priority_tax,
            // Python's audit contract names the pre-positive-scale authored
            // payout as gross.  Keep the economy-adjusted/post-scale bucket
            // separate so live Dig can prove the sink ordering without
            // relabeling an already scaled value as gross.
            "gross_jc": outcome.gross_jc,
            "economy_adjusted_jc": outcome.economy_adjusted_jc,
            "economy_reward_multiplier": economy_multiplier,
            "auto_purchases": auto_purchases.iter().map(|purchase| serde_json::json!({
                "item": purchase.item_type,
                "status": purchase.status.as_str(),
                "cost": purchase.cost,
                "item_id": purchase.item_id,
            })).collect::<Vec<_>>(),
            "slow_drip": slow_drip_claim.as_ref().map(|claim| serde_json::json!({
                "claim_date": claim.claim_date,
                "gross_jc": claim.gross_jc,
                "credit_jc": claim.credit_jc,
                "claimed_before": claim.claimed_before,
                "claimed_after": claim.claimed_after,
                "anchor_before": claim.anchor_before,
                "claimed_at": claim.claimed_at,
            })),
        })
        .to_string();
        let items_used = loot_outcome
            .items_used
            .iter()
            .map(|item| (*item).to_owned())
            .collect::<Vec<_>>();
        let balance_after = staged.balance;
        let luminosity_after = staged
            .tunnel
            .as_ref()
            .map_or(LUMINOSITY_MAX, |next_tunnel| next_tunnel.luminosity);
        let commit = DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next: staged,
            delivery_draft: None,
            consumed_item_ids: consumed_item_ids.clone(),
            pet_work_claim,
            consume_overgrowth: overgrowth_active,
            depth_before,
            depth_after: outcome.depth_after,
            jc_delta: if outcome.cave_in {
                wallet_reward_delta
            } else {
                outcome
                    .jc_earned
                    .saturating_sub(if paid_charge_active { paid_cost } else { 0 })
            },
            vanity_tax: if outcome.cave_in {
                0
            } else {
                outcome.vanity_tax
            },
            low_priority_tax: if outcome.cave_in {
                0
            } else {
                outcome.low_priority_tax
            },
            balance_cost: if paid_charge_active { paid_cost } else { 0 },
            action_type: "dig".to_owned(),
            detail,
            now,
        };
        let mut runtime_outcome = DigRuntimeOutcome {
            success: true,
            error: None,
            depth_before,
            depth_after: outcome.depth_after,
            advance: outcome.advance,
            jc_earned: if outcome.cave_in {
                0
            } else {
                outcome.jc_earned
            },
            vanity_tax: if outcome.cave_in {
                0
            } else {
                outcome.vanity_tax
            },
            low_priority_tax: if outcome.cave_in {
                0
            } else {
                outcome.low_priority_tax
            },
            balance_after,
            tunnel_name: tunnel.tunnel_name.clone(),
            milestone_bonus: outcome.milestone_bonus,
            streak_bonus: outcome.streak_bonus,
            bankruptcy_penalty: outcome.bankruptcy_penalty,
            luminosity_after,
            luminosity_drained,
            corruption_description: corruption
                .as_ref()
                .map(|corruption| corruption.description.to_owned()),
            mutation_names: mutations
                .iter()
                .map(|mutation| mutation.name.to_owned())
                .collect(),
            tip: dig_progressive_tip(outcome.depth_after, request.now),
            cave_in: outcome.cave_in,
            cave_in_detail: cave_in_detail_value.map(|mut detail| {
                if let Some(object) = detail.as_object_mut() {
                    object.insert("depth_after".to_owned(), Value::from(outcome.depth_after));
                }
                detail.to_string()
            }),
            event_id,
            artifact_id,
            boss_boundary: outcome.boss_encounter,
            first_dig: false,
            paid_dig_cost: paid_cost,
            cooldown_remaining: 0,
            paid_dig_available: false,
            items_used,
            consumed_item_ids,
            action_id: None,
            route_choice_required: false,
            pickaxe_tier,
            pet_dig_bonus,
            pet_name,
            forced_event_consumed,
            relic_trim_notice: false,
            weather: weather.and_then(|weather| {
                weather_by_id(&weather.weather_id).map(|definition| DigRuntimeWeatherInfo {
                    name: definition.name.to_owned(),
                    description: definition.description.to_owned(),
                })
            }),
            active_curse,
        };
        let receipt =
            self.commit_dig(commit, runtime_outcome.clone(), delivery_context.as_ref())?;
        runtime_outcome.balance_after = receipt.balance_after;
        runtime_outcome.action_id = Some(receipt.action_id);
        Ok(runtime_outcome)
    }

    /// Atomically choose one of the persisted route offers.  This is the
    /// application counterpart to Python's persistent `RouteChoiceView`.
    pub fn choose_route(
        &self,
        discord_id: i64,
        guild_id: i64,
        route_id: &str,
        now: i64,
    ) -> Result<DigRuntimeActionResult, DigRuntimeStoreError> {
        let snapshot = self.store.snapshot(discord_id, guild_id)?;
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Ok(DigRuntimeActionResult::error(
                &snapshot,
                "You don't have a tunnel.",
            ));
        };
        let evaluation = evaluate_route_choice(tunnel.route_state.as_deref(), route_id);
        let (selected_state, already_selected) = match evaluation {
            RouteChoiceEvaluation::Select {
                route: _,
                selected_state,
            } => (Some(selected_state), false),
            RouteChoiceEvaluation::AlreadySelected { .. } => (None, true),
            RouteChoiceEvaluation::Rejected(message) => {
                return Ok(DigRuntimeActionResult::error(&snapshot, message));
            }
        };
        if already_selected {
            return Ok(DigRuntimeActionResult {
                success: true,
                error: None,
                item: None,
                item_id: None,
                route_id: Some(route_id.to_owned()),
                cost: 0,
                queued: false,
                balance_after: snapshot.balance,
                action_id: None,
            });
        }
        let mut next = snapshot.clone();
        next.tunnel
            .as_mut()
            .expect("route snapshot has tunnel")
            .route_state = selected_state.map(|state| state.to_python_json());
        let receipt = self.store.commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: tunnel.depth,
            depth_after: tunnel.depth,
            jc_delta: 0,
            vanity_tax: 0,
            low_priority_tax: 0,
            balance_cost: 0,
            action_type: "route_choice".to_owned(),
            detail: serde_json::json!({"route_id": route_id}).to_string(),
            now,
        })?;
        Ok(DigRuntimeActionResult {
            success: true,
            error: None,
            item: None,
            item_id: None,
            route_id: Some(route_id.to_owned()),
            cost: 0,
            queued: false,
            balance_after: receipt.balance_after,
            action_id: Some(receipt.action_id),
        })
    }

    /// Buy a consumable and commit its balance/inventory mutation atomically.
    pub fn buy_item(
        &self,
        discord_id: i64,
        guild_id: i64,
        item_type: &str,
        now: i64,
    ) -> Result<DigRuntimeActionResult, DigRuntimeStoreError> {
        self.stage_loot_action(discord_id, guild_id, now, "dig_buy", |loot| {
            loot.buy_item(discord_id, guild_id, item_type)
        })
    }

    /// Queue an owned consumable for the next real Dig atomically.
    pub fn queue_item(
        &self,
        discord_id: i64,
        guild_id: i64,
        item_id: i64,
        now: i64,
    ) -> Result<DigRuntimeActionResult, DigRuntimeStoreError> {
        self.stage_loot_action(discord_id, guild_id, now, "dig_queue_item", |loot| {
            loot.queue_item(discord_id, guild_id, item_id)
        })
    }

    /// Use one unqueued consumable through the same transaction boundary as
    /// `/dig use`; the action only reserves the item and the next real dig
    /// burns it together with the tunnel outcome.
    pub fn use_item(
        &self,
        discord_id: i64,
        guild_id: i64,
        item_type: &str,
        now: i64,
    ) -> Result<DigRuntimeActionResult, DigRuntimeStoreError> {
        self.stage_loot_action(discord_id, guild_id, now, "dig_use_item", |loot| {
            loot.use_item(discord_id, guild_id, item_type)
        })
    }

    pub fn relic_autocomplete(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<String>, DigRuntimeStoreError> {
        let snapshot = self.store.snapshot(discord_id, guild_id)?;
        if !snapshot.registered || snapshot.tunnel.is_none() {
            return Ok(Vec::new());
        }
        let loot = DigLootService::new(
            DigRuntimeLootRepository::new(snapshot),
            SeededLootEntropy::new(0),
        );
        Ok(loot
            .relic_autocomplete(discord_id, guild_id)
            .into_iter()
            .map(|choice| choice.value)
            .collect())
    }

    fn stage_loot_action(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        action_type: &str,
        action: impl FnOnce(
            &mut DigLootService<DigRuntimeLootRepository, SeededLootEntropy>,
        ) -> LootActionResult,
    ) -> Result<DigRuntimeActionResult, DigRuntimeStoreError> {
        let snapshot = self.store.snapshot(discord_id, guild_id)?;
        if !snapshot.registered || snapshot.tunnel.is_none() {
            return Ok(DigRuntimeActionResult::error(
                &snapshot,
                "You don't have a tunnel.",
            ));
        }
        let mut loot = DigLootService::new(
            DigRuntimeLootRepository::new(snapshot.clone()),
            SeededLootEntropy::new(seed_for(
                DigRuntimeRequest {
                    discord_id,
                    guild_id,
                    now,
                    paid: false,
                    forced_event: false,
                },
                self.config.entropy_secret,
            )),
        );
        let result = action(&mut loot);
        if !result.success {
            return Ok(DigRuntimeActionResult::from_loot(&snapshot, result));
        }
        let next = loot.repository().snapshot().clone();
        let depth = snapshot.tunnel.as_ref().map_or(0, |tunnel| tunnel.depth);
        let receipt = self.store.commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: depth,
            depth_after: depth,
            jc_delta: -result.cost,
            vanity_tax: 0,
            low_priority_tax: 0,
            balance_cost: 0,
            action_type: action_type.to_owned(),
            detail: serde_json::json!({"item": result.item, "item_id": result.item_id}).to_string(),
            now,
        })?;
        Ok(DigRuntimeActionResult {
            success: true,
            error: None,
            item: result.item.map(str::to_owned),
            item_id: result.item_id,
            route_id: None,
            cost: result.cost,
            queued: result.queued,
            balance_after: receipt.balance_after,
            action_id: Some(receipt.action_id),
        })
    }
}

impl DigRuntimeActionResult {
    fn from_loot(snapshot: &DigRuntimeSnapshot, result: LootActionResult) -> Self {
        Self {
            success: result.success,
            error: result.error,
            item: result.item.map(str::to_owned),
            item_id: result.item_id,
            route_id: None,
            cost: result.cost,
            queued: result.queued,
            balance_after: snapshot.balance,
            action_id: None,
        }
    }
}

fn dig_progressive_tip(depth: i64, seed: i64) -> String {
    let tips: &[&str] = if depth <= 10 {
        &[
            "Use /dig to advance your tunnel. Your first dig each day is free!",
            "Buy items from the shop with /dig shop. Dynamite blasts through rock fast.",
            "Each layer gets harder but more rewarding. Keep digging!",
        ]
    } else if depth <= 25 {
        &[
            "Ask a friend to /dig help you — it slows down decay too.",
            "Watch out for sabotage! Buy insurance to protect your tunnel.",
            "Set a trap to punish anyone who tries to sabotage you.",
        ]
    } else if depth <= 50 {
        &[
            "Bosses guard each layer boundary. Choose your strategy wisely.",
            "Prestige resets your depth but grants permanent bonuses.",
            "Upgrade your pickaxe for better digging performance.",
        ]
    } else {
        &[
            "Relics give permanent bonuses — equip them from your inventory.",
            "Deeper layers have rarer artifacts. Keep exploring!",
            "Stack sabotage defenses: insurance + reinforcement + relics.",
        ]
    };
    let index = usize::try_from(seed.rem_euclid(tips.len() as i64)).unwrap_or_default();
    tips[index].to_owned()
}

/// Secret drawn once per process and mixed into every dig seed.
///
/// Without it a dig's seed is derived entirely from values the player knows or
/// controls -- their ids, the click second, and the paid/forced flags -- so the
/// open-source loot pipeline can be simulated for each upcoming second and a
/// click timed for no cave-in, maximum JC, or an artifact. Keeping the secret
/// per process preserves same-request determinism for retries.
fn process_dig_secret() -> u64 {
    static SECRET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SECRET.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        // RandomState is seeded by the OS, which is what makes this
        // unpredictable rather than merely varying.
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(std::process::id().into());
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        hasher.finish()
    })
}

pub(crate) fn seed_for(request: DigRuntimeRequest, secret: u64) -> u64 {
    let mut value = request.discord_id as u64;
    value = value.rotate_left(17) ^ request.guild_id as u64;
    value = value.rotate_left(23) ^ request.now as u64;
    value ^= u64::from(request.paid) ^ (u64::from(request.forced_event) << 1);
    // A zero secret reproduces the legacy seed exactly, which is what keeps
    // the deterministic tests meaningful.
    value ^ secret
}

fn parked_boss_boundary(tunnel: &DigRuntimeTunnel) -> Option<i64> {
    current_boss_boundary_from_json(tunnel.depth, &tunnel.boss_progress)
}

fn injury_reduces_advance(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|kind| kind == "reduced_advance")
}

fn injury_slows_cooldown(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|kind| kind == "slower_cooldown")
}

/// Consume one admitted Dig from the persisted injury state.  This is staged
/// before the loot service rolls so the same transaction clears the injury
/// when its final charge is used.
fn tick_injury(tunnel: &mut DigRuntimeTunnel) -> bool {
    let Some(raw) = tunnel.injury_state.as_deref() else {
        return false;
    };
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let Some(remaining) = value.get("digs_remaining").and_then(Value::as_i64) else {
        return false;
    };
    let reduced = value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "reduced_advance")
        && remaining > 0;
    if remaining <= 1 {
        tunnel.injury_state = None;
        return reduced;
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("digs_remaining".to_owned(), Value::from(remaining - 1));
    }
    tunnel.injury_state = Some(value.to_string());
    reduced
}

fn prestige_perk_contains(raw: &str, perk: &str) -> bool {
    serde_json::from_str::<Vec<String>>(raw)
        .ok()
        .is_some_and(|perks| perks.iter().any(|candidate| candidate == perk))
}

const LUMINOSITY_MAX: i64 = 100;
const LUMINOSITY_REFILL_PER_DAY: i64 = 20;

/// Apply Python's continuous refill and move the refill anchor into the same
/// staged tunnel snapshot as the rest of the Dig.
fn apply_luminosity_refill(tunnel: &mut DigRuntimeTunnel, now: i64) {
    let last_update = tunnel.last_lum_update_at.unwrap_or(now);
    let elapsed = now.saturating_sub(last_update).max(0);
    let refill = elapsed.saturating_mul(LUMINOSITY_REFILL_PER_DAY) / (24 * 3_600);
    tunnel.luminosity = tunnel
        .luminosity
        .saturating_add(refill)
        .clamp(0, LUMINOSITY_MAX);
    tunnel.last_lum_update_at = Some(now);
}

fn next_daily_streak(tunnel: &DigRuntimeTunnel, today: &str) -> i64 {
    let existing = tunnel.streak_days.max(0);
    let Some(last) = tunnel.streak_last_date.as_deref() else {
        return 1;
    };
    if last == today {
        return existing.max(1);
    }
    let (Ok(today), Ok(last)) = (
        NaiveDate::parse_from_str(today, "%Y-%m-%d"),
        NaiveDate::parse_from_str(last, "%Y-%m-%d"),
    ) else {
        return 1;
    };
    if today.signed_duration_since(last).num_days() == 1 {
        existing.saturating_add(1).max(1)
    } else {
        1
    }
}

fn fingerprint<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn tunnel_state(snapshot: &DigRuntimeSnapshot, paid_cost: Option<i64>) -> TunnelState {
    let tunnel = snapshot.tunnel.as_ref().expect("staged tunnel exists");
    let mut defeated_bosses = BTreeSet::new();
    if let Ok(Value::Object(progress)) = serde_json::from_str::<Value>(&tunnel.boss_progress) {
        for (boundary, value) in progress {
            let is_defeated = match value {
                Value::Object(fields) => fields
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s == "defeated"),
                Value::String(status) => status == "defeated",
                _ => false,
            };
            if is_defeated && let Ok(boundary) = boundary.parse::<i64>() {
                defeated_bosses.insert(boundary);
            }
        }
    }
    let artifacts = snapshot
        .artifacts
        .iter()
        .filter(|artifact| artifact.is_relic)
        .filter_map(|artifact| {
            crate::dig_loot::artifact_catalog()
                .into_iter()
                .find(|definition| definition.id == artifact.artifact_id)
                .map(|definition| definition.id)
        })
        .collect();
    TunnelState {
        depth: tunnel.depth,
        max_depth: tunnel.max_depth,
        balance: snapshot.balance.saturating_sub(paid_cost.unwrap_or(0)),
        total_digs: tunnel.total_digs,
        last_dig_at: tunnel.last_dig_at,
        luminosity: tunnel.luminosity,
        stats: miner_allocation(tunnel),
        paid_digs_today: usize::try_from(tunnel.paid_digs_today.max(0)).unwrap_or_default(),
        paid_dig_day: None,
        queued_consumables: snapshot
            .inventory
            .iter()
            .filter(|item| item.queued)
            .filter_map(|item| static_item(&item.item_type))
            .collect(),
        boss_preparation: Vec::new(),
        artifacts,
        awarded_bosses: BTreeSet::new(),
        defeated_bosses,
        buff: None,
    }
}

fn apply_state(
    snapshot: &DigRuntimeSnapshot,
    state: TunnelState,
    today: &str,
    paid: bool,
    total_jc_increment: i64,
) -> DigRuntimeSnapshot {
    let mut next = snapshot.clone();
    next.balance = state.balance;
    if let Some(tunnel) = next.tunnel.as_mut() {
        tunnel.depth = state.depth;
        tunnel.max_depth = state.max_depth;
        tunnel.total_digs = state.total_digs;
        tunnel.last_dig_at = state.last_dig_at;
        tunnel.luminosity = state.luminosity;
        tunnel.total_jc_earned = tunnel
            .total_jc_earned
            .max(0)
            .saturating_add(total_jc_increment.max(0));
        if paid {
            tunnel.paid_digs_today = if tunnel.paid_dig_date.as_deref() == Some(today) {
                tunnel.paid_digs_today.saturating_add(1)
            } else {
                1
            };
            tunnel.paid_dig_date = Some(today.to_owned());
        }
    }
    next
}

/// A lock-backed store for deterministic application tests.
#[derive(Clone, Debug, Default)]
pub struct InMemoryDigRuntimeStore {
    snapshot: Arc<Mutex<Option<DigRuntimeSnapshot>>>,
    next_action_id: Arc<Mutex<i64>>,
}

impl InMemoryDigRuntimeStore {
    #[must_use]
    pub fn new(snapshot: DigRuntimeSnapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Some(snapshot))),
            next_action_id: Arc::new(Mutex::new(1)),
        }
    }

    #[must_use]
    pub fn current(&self) -> Option<DigRuntimeSnapshot> {
        self.snapshot
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone())
    }
}

impl DigRuntimeStore for InMemoryDigRuntimeStore {
    fn snapshot(
        &self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError> {
        self.snapshot
            .lock()
            .map_err(|_| DigRuntimeStoreError::Poisoned)?
            .clone()
            .ok_or(DigRuntimeStoreError::MissingPlayer)
    }

    fn commit(
        &self,
        request: DigRuntimeCommit,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        let mut snapshot = self
            .snapshot
            .lock()
            .map_err(|_| DigRuntimeStoreError::Poisoned)?;
        let current = snapshot
            .as_ref()
            .ok_or(DigRuntimeStoreError::MissingPlayer)?;
        if DigRuntimeVersion::from(current) != request.expected {
            return Err(DigRuntimeStoreError::Conflict);
        }
        *snapshot = Some(request.next.clone());
        let mut action_id = self
            .next_action_id
            .lock()
            .map_err(|_| DigRuntimeStoreError::Poisoned)?;
        let receipt = DigRuntimeCommitReceipt {
            balance_after: request.next.balance,
            action_id: *action_id,
            inserted_item_ids: Vec::new(),
            inserted_artifact_ids: Vec::new(),
            inserted_gear_ids: Vec::new(),
        };
        *action_id = action_id.saturating_add(1);
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "dig_pet_runtime_tests.rs"]
mod dig_pet_runtime_tests;
