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

use cama_db::dig_event_runtime::{
    DigEventActorKey, DigEventActorSnapshot, DigEventQuestSnapshot, DigEventRuntimeRepository,
};
use cama_db::dig_guild_modifiers::DigGuildModifierRepository;
use cama_db::dig_inventory_repository::{
    AutoBuyRequest, AutoBuySelection, BuyInsuranceOutcome, DigInventoryRepository, SetTrapOutcome,
};
use cama_db::dig_weather::{DigWeatherEntry, DigWeatherRepository};
use cama_db::mana_service_repository::ManaRepository;
use cama_db::manashop_rework_repository::ManashopRepository;
use cama_db::pet_repository::PetRepository;
use cama_domain::dig_cave_in::{
    CAVE_IN_BLOCK_LOSS_RANGES, CAVE_IN_CATASTROPHIC_GEAR_TICKS, CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_MILESTONE_STEP, CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE,
    CAVE_IN_INJURY_DIGS_BY_BAND, CAVE_IN_MEDICAL_BILL_RANGES, CAVE_IN_STUN_DIGS_BY_BAND,
    CaveInApplicability, CaveInRng, cave_in_band, pick_cave_in_consequence,
    roll_catastrophic_cave_in,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_gear::{AMULET_TIERS, ARMOR_TIERS, BOOTS_TIERS, WEAPON_TIERS, unique_gear};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::pet::{
    DIG_WORK_CAP_BLOCKS, DIG_WORK_UNITS_PER_BLOCK, PetDigWork, PetDigWorkClaim,
};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

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
    DigOutcomeInput, MinerAllocation, TunnelState, apply_boss_gate, apply_dig_outcome,
    apply_first_dig, cooldown_remaining, layer_at, paid_dig_cost,
};
use crate::dig_tunnels::{
    aggregate_prestige_perk_effects, ascension_effects, mutation_effects, mutations_from_json,
    roll_corruption,
};
use crate::economy_event_service::EconomyEventConfig;
use crate::economy_event_sqlite::SqliteEconomyEventService;

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
    pub economy_event: EconomyEventConfig,
    /// Pet dig work is settled lazily from the persisted hunger/work anchors.
    /// Keeping the decay policy on the runtime config makes the dig aggregate
    /// use the same value as the pet application service without a scheduler.
    pub pet_decay_per_day: i64,
}

impl Default for DigRuntimeConfig {
    fn default() -> Self {
        Self {
            asset_root: PathBuf::from(DEFAULT_DIG_ASSET_ROOT),
            require_authored_assets: false,
            minigame_jc_delta_scale: 1.0,
            economy_event: EconomyEventConfig::default(),
            pet_decay_per_day: cama_domain::pet::DEFAULT_HUNGER_DECAY_PER_DAY,
        }
    }
}

impl DigRuntimeConfig {
    #[must_use]
    pub fn production() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_asset_root(root: impl Into<PathBuf>) -> Self {
        Self {
            asset_root: root.into(),
            ..Self::default()
        }
    }

    #[must_use]
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
    pub fn authored_asset_root(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.asset_root.join(relative)
    }
}

/// A single migrated inventory row needed by the Dig application aggregate.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeInventoryItem {
    pub id: i64,
    pub item_type: String,
    pub queued: bool,
}

/// A single migrated artifact row needed by the Dig application aggregate.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeArtifact {
    pub id: i64,
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

/// A persisted gear row needed to apply pickaxe and combat modifiers before a
/// dig is settled. Keeping this in the application snapshot is important:
/// selecting/equipping gear in a component must not race a concurrent dig.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeGear {
    pub id: i64,
    pub slot: String,
    pub tier: i64,
    pub durability: i64,
    pub equipped: bool,
    pub acquired_at: i64,
    pub source: String,
    pub item_id: Option<String>,
}

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

/// The tunnel columns touched by a real Dig commit.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DigRuntimeTunnel {
    pub discord_id: i64,
    pub guild_id: i64,
    pub depth: i64,
    pub max_depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub last_dig_at: Option<i64>,
    pub luminosity: i64,
    pub tunnel_name: String,
    pub prestige_level: i64,
    pub prestige_perks: String,
    pub boss_progress: String,
    pub boss_attempts: String,
    pub route_state: Option<String>,
    pub injury_state: Option<String>,
    pub hard_hat_charges: i64,
    pub reinforced_until: i64,
    pub void_bait_digs: i64,
    pub sonar_skip_pending: bool,
    pub temp_buffs: Option<String>,
    pub temp_curses: Option<String>,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
    pub stat_points: i64,
    pub paid_digs_today: i64,
    pub paid_dig_date: Option<String>,
    pub pickaxe_tier: i64,
    pub current_run_jc: i64,
    pub current_run_artifacts: i64,
    pub current_run_events: i64,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub streak_days: i64,
    pub streak_last_date: Option<String>,
    pub auto_buy_torch: bool,
    pub auto_buy_hard_hat: bool,
    pub trap_active: bool,
    pub trap_free_today: bool,
    pub trap_date: Option<String>,
    pub insured_until: Option<i64>,
    pub revenge_target: Option<i64>,
    pub revenge_type: Option<String>,
    pub revenge_until: Option<i64>,
    pub cheer_data: Option<String>,
    pub grappling_hook_charges: i64,
    pub lantern_stub_date: Option<String>,
    pub thick_skin_date: Option<String>,
    pub mutations: Option<String>,
    pub miner_origin: String,
    pub miner_about: String,
    pub engine_mode: String,
    pub stat_boss_awards: String,
    pub stinger_curse: Option<String>,
    pub last_lum_update_at: Option<i64>,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: i64,
    pub pinnacle_hp_remaining: Option<i64>,
    pub pinnacle_last_engaged_at: Option<i64>,
    pub retreat_cooldown_until: Option<i64>,
    pub last_cheer_at: Option<i64>,
    pub cavein_free_streak: i64,
    pub relic_trim_notice: bool,
}

impl DigRuntimeTunnel {
    #[must_use]
    pub fn new(discord_id: i64, guild_id: i64, _now: i64) -> Self {
        Self {
            discord_id,
            guild_id,
            depth: 0,
            max_depth: 0,
            total_digs: 0,
            total_jc_earned: 0,
            last_dig_at: None,
            luminosity: 100,
            tunnel_name: format!("Miner {discord_id}"),
            prestige_level: 0,
            prestige_perks: "[]".to_owned(),
            boss_progress: "{}".to_owned(),
            boss_attempts: "{}".to_owned(),
            route_state: None,
            injury_state: None,
            hard_hat_charges: 0,
            reinforced_until: 0,
            void_bait_digs: 0,
            sonar_skip_pending: false,
            temp_buffs: None,
            temp_curses: None,
            stat_strength: 0,
            stat_smarts: 0,
            stat_stamina: 0,
            stat_points: 5,
            paid_digs_today: 0,
            paid_dig_date: None,
            pickaxe_tier: 0,
            current_run_jc: 0,
            current_run_artifacts: 0,
            current_run_events: 0,
            best_run_score: 0,
            total_prestige_score: 0,
            streak_days: 0,
            streak_last_date: None,
            auto_buy_torch: false,
            auto_buy_hard_hat: false,
            trap_active: false,
            trap_free_today: true,
            trap_date: None,
            insured_until: None,
            revenge_target: None,
            revenge_type: None,
            revenge_until: None,
            cheer_data: None,
            grappling_hook_charges: 0,
            lantern_stub_date: None,
            thick_skin_date: None,
            mutations: None,
            miner_origin: String::new(),
            miner_about: String::new(),
            engine_mode: "legacy".to_owned(),
            stat_boss_awards: "[]".to_owned(),
            stinger_curse: None,
            last_lum_update_at: None,
            pinnacle_boss_id: None,
            pinnacle_phase: 0,
            pinnacle_hp_remaining: None,
            pinnacle_last_engaged_at: None,
            retreat_cooldown_until: None,
            last_cheer_at: None,
            cavein_free_streak: 0,
            relic_trim_notice: false,
        }
    }

    #[must_use]
    pub fn stats(&self) -> MinerAllocation {
        MinerAllocation {
            strength: self.stat_strength.max(0),
            smarts: self.stat_smarts.max(0),
            stamina: self.stat_stamina.max(0),
            stat_points: self.stat_points.max(0),
        }
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
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
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

    /// Resolve the mana paid-cost modifier from the same request-local
    /// snapshot used by the live Dig.  Non-SQLite stores remain neutral.
    fn paid_dig_cost_modifier(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _now: i64,
    ) -> Result<f64, DigRuntimeStoreError> {
        Ok(1.0)
    }

    /// Resolve the active mana hazard adjustment once for the live Dig.
    /// Non-SQLite stores stay neutral until they explicitly own this input.
    fn cave_in_mana_hazard_modifier(
        &self,
        _discord_id: i64,
        _guild_id: i64,
        _now: i64,
    ) -> Result<f64, DigRuntimeStoreError> {
        Ok(0.0)
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

    fn finalize_delivery(
        &self,
        _request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        Err(DigRuntimeStoreError::StateConflict)
    }
}

/// Existing-schema SQLite adapter.  It never creates or migrates tables.
#[derive(Clone, Debug)]
pub struct SqliteDigRuntimeStore {
    path: PathBuf,
}

/// One actor-scoped tunnel/wallet mutation admitted by the runtime SQLite
/// adapter. Keeping the audit identity beside the state deltas prevents
/// positional call sites from swapping cost, reward, or timestamp fields.
#[derive(Clone, Copy, Debug)]
pub struct AtomicTunnelBalanceUpdate<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub balance_delta: i64,
    pub balance_cost: i64,
    pub depth_after: Option<i64>,
    pub detail: &'a str,
    pub action_type: &'a str,
    pub now: i64,
}

impl SqliteDigRuntimeStore {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        Ok(connection)
    }

    /// Apply one actor-scoped Dig balance/tunnel mutation through the
    /// existing SQLite adapter.  The ledger context is installed only around
    /// the wallet writes, so a following ordinary balance update cannot
    /// inherit an event's actor or relation.
    pub fn atomic_tunnel_balance_update(
        &self,
        request: AtomicTunnelBalanceUpdate<'_>,
    ) -> Result<i64, DigRuntimeStoreError> {
        let AtomicTunnelBalanceUpdate {
            discord_id,
            guild_id,
            balance_delta,
            balance_cost,
            depth_after,
            detail,
            action_type,
            now,
        } = request;
        let detail_value = serde_json::from_str::<Value>(detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("balance update detail"))?;
        if !detail_value.is_object() {
            return Err(DigRuntimeStoreError::InvalidJson("balance update detail"));
        }
        let cost = balance_cost.max(0);
        let event_id = detail_value
            .get("event_id")
            .or_else(|| detail_value.get("event"))
            .and_then(Value::as_str);
        let related_type = event_id.map_or("dig_action", |_| "event");
        let related_id = event_id.unwrap_or(action_type);
        let reason = if balance_delta - cost >= 0 {
            "dig event credit"
        } else {
            "dig event debit"
        };

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = balance else {
            return Err(DigRuntimeStoreError::MissingPlayer);
        };
        let depth_before = transaction
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(depth_before) = depth_before else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        if balance < cost {
            return Err(DigRuntimeStoreError::InsufficientFunds);
        }
        let balance_after = balance
            .checked_sub(cost)
            .and_then(|value| value.checked_add(balance_delta))
            .ok_or(DigRuntimeStoreError::Conflict)?;

        if cost != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                "event",
                related_id,
                "dig paid cost",
                detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance=?4",
                params![balance - cost, discord_id, guild_id, balance],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        if balance_delta != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                related_type,
                related_id,
                reason,
                detail,
            )?;
            let before = balance - cost;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance=?4",
                params![balance_after, discord_id, guild_id, before],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        let depth_after = depth_after.unwrap_or(depth_before);
        let changed = transaction.execute(
            "UPDATE tunnels SET depth=?1 WHERE discord_id=?2 AND guild_id=?3
              AND depth=?4",
            params![depth_after, discord_id, guild_id, depth_before],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::Conflict);
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",
            params![
                guild_id,
                discord_id,
                action_type,
                depth_before,
                depth_after,
                balance_after - balance,
                detail,
                now,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(action_id)
    }
}

fn set_runtime_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'dig',?1,?2,?3,?4,?5)",
        params![actor_id, related_type, related_id, reason, metadata],
    )?;
    Ok(())
}

fn clear_runtime_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn runtime_ledger_reason(action_type: &str, delta: i64) -> &'static str {
    match (action_type, delta.signum()) {
        ("dig" | "paid_dig", 1) => "dig credit",
        ("dig" | "paid_dig", -1) => "dig debit",
        (_, 1) => "dig action credit",
        (_, -1) => "dig action debit",
        _ => "dig balance change",
    }
}

const TUNNEL_SELECT: &str = "SELECT depth,max_depth,total_digs,total_jc_earned,last_dig_at,
        luminosity,COALESCE(tunnel_name,?3),prestige_level,
        COALESCE(prestige_perks,'[]'),COALESCE(boss_progress,'{}'),
        COALESCE(boss_attempts,'{}'),route_state,injury_state,
        hard_hat_charges,COALESCE(reinforced_until,0),void_bait_digs,
        sonar_skip_pending,temp_buffs,temp_curses,stat_strength,
        stat_smarts,stat_stamina,stat_points,paid_digs_today,
        paid_dig_date,pickaxe_tier,current_run_jc,current_run_artifacts,
        current_run_events,streak_days,auto_buy_torch,auto_buy_hard_hat,
        best_run_score,total_prestige_score,streak_last_date,
        trap_active,trap_free_today,trap_date,insured_until,
        revenge_target,revenge_type,revenge_until,cheer_data,
        grappling_hook_charges,lantern_stub_date,thick_skin_date,
        mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
        stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
        pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
        last_cheer_at,cavein_free_streak,relic_trim_notice
 FROM tunnels WHERE discord_id=?1 AND guild_id=?2";

fn load_tunnel_row(
    row: &rusqlite::Row<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<DigRuntimeTunnel, rusqlite::Error> {
    Ok(DigRuntimeTunnel {
        discord_id,
        guild_id,
        depth: row.get(0)?,
        max_depth: row.get(1)?,
        total_digs: row.get(2)?,
        total_jc_earned: row.get(3)?,
        last_dig_at: row.get(4)?,
        luminosity: row.get(5)?,
        tunnel_name: row.get(6)?,
        prestige_level: row.get(7)?,
        prestige_perks: row.get(8)?,
        boss_progress: row.get(9)?,
        boss_attempts: row.get(10)?,
        route_state: row.get(11)?,
        injury_state: row.get(12)?,
        hard_hat_charges: row.get(13)?,
        reinforced_until: row.get(14)?,
        void_bait_digs: row.get(15)?,
        sonar_skip_pending: row.get::<_, i64>(16)? != 0,
        temp_buffs: row.get(17)?,
        temp_curses: row.get(18)?,
        stat_strength: row.get(19)?,
        stat_smarts: row.get(20)?,
        stat_stamina: row.get(21)?,
        stat_points: row.get(22)?,
        paid_digs_today: row.get(23)?,
        paid_dig_date: row.get(24)?,
        pickaxe_tier: row.get(25)?,
        current_run_jc: row.get(26)?,
        current_run_artifacts: row.get(27)?,
        current_run_events: row.get(28)?,
        streak_days: row.get(29)?,
        auto_buy_torch: row.get::<_, i64>(30)? != 0,
        auto_buy_hard_hat: row.get::<_, i64>(31)? != 0,
        best_run_score: row.get(32)?,
        total_prestige_score: row.get(33)?,
        streak_last_date: row.get(34)?,
        trap_active: row.get::<_, i64>(35)? != 0,
        trap_free_today: row.get::<_, i64>(36)? != 0,
        trap_date: row.get(37)?,
        insured_until: row.get(38)?,
        revenge_target: row.get(39)?,
        revenge_type: row.get(40)?,
        revenge_until: row.get(41)?,
        cheer_data: row.get(42)?,
        grappling_hook_charges: row.get(43)?,
        lantern_stub_date: row.get(44)?,
        thick_skin_date: row.get(45)?,
        mutations: row.get(46)?,
        engine_mode: row.get(47)?,
        miner_origin: row.get(48)?,
        miner_about: row.get(49)?,
        stat_boss_awards: row.get(50)?,
        stinger_curse: row.get(51)?,
        last_lum_update_at: row.get(52)?,
        pinnacle_boss_id: row.get(53)?,
        pinnacle_phase: row.get(54)?,
        pinnacle_hp_remaining: row.get(55)?,
        pinnacle_last_engaged_at: row.get(56)?,
        retreat_cooldown_until: row.get(57)?,
        last_cheer_at: row.get(58)?,
        cavein_free_streak: row.get(59)?,
        relic_trim_notice: row.get::<_, i64>(60)? != 0,
    })
}

fn event_actor_snapshot(
    snapshot: &DigRuntimeSnapshot,
    depth: i64,
    luminosity: i64,
) -> DigEventActorSnapshot {
    let tunnel = snapshot.tunnel.as_ref().expect("event actor needs tunnel");
    DigEventActorSnapshot {
        key: DigEventActorKey {
            discord_id: tunnel.discord_id,
            guild_id: Some(tunnel.guild_id),
        },
        depth,
        luminosity,
        prestige_level: tunnel.prestige_level,
        prestige_perks_json: tunnel.prestige_perks.clone(),
        boss_progress_json: tunnel.boss_progress.clone(),
        streak_days: tunnel.streak_days,
        temp_buff_json: tunnel.temp_buffs.clone(),
        temp_curse_json: tunnel.temp_curses.clone(),
        balance: snapshot.balance,
        inventory_count: snapshot.inventory.len(),
        owned_gear: snapshot
            .gear
            .iter()
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        equipped_gear: snapshot
            .gear
            .iter()
            .filter(|piece| piece.equipped && piece.durability > 0)
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        owned_artifacts: snapshot
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
        equipped_relics: snapshot
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
    }
}

impl DigRuntimeStore for SqliteDigRuntimeStore {
    fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError> {
        let connection = self.connection()?;
        let player = connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = player else {
            return Ok(DigRuntimeSnapshot {
                registered: false,
                balance: 0,
                tunnel: None,
                inventory: Vec::new(),
                artifacts: Vec::new(),
                gear: Vec::new(),
                weather: Vec::new(),
            });
        };

        let tunnel = connection
            .query_row(
                TUNNEL_SELECT,
                params![discord_id, guild_id, format!("Miner {discord_id}")],
                |row| load_tunnel_row(row, discord_id, guild_id),
            )
            .optional()?;
        let inventory = load_inventory(&connection, discord_id, guild_id)?;
        let artifacts = load_artifacts(&connection, discord_id, guild_id)?;
        let gear = load_gear(&connection, discord_id, guild_id)?;
        Ok(DigRuntimeSnapshot {
            registered: true,
            balance,
            tunnel,
            inventory,
            artifacts,
            gear,
            weather: Vec::new(),
        })
    }

    fn event_actor_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigEventActorSnapshot>, DigRuntimeStoreError> {
        DigEventRuntimeRepository::new(&self.path)
            .actor_snapshot_for_event(DigEventActorKey {
                discord_id,
                guild_id: Some(guild_id),
            })
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn event_quest_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<cama_db::dig_event_runtime::DigEventQuestSnapshot, DigRuntimeStoreError> {
        DigEventRuntimeRepository::new(&self.path)
            .quest_snapshot(
                DigEventActorKey {
                    discord_id,
                    guild_id: Some(guild_id),
                },
                now,
            )
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))
    }

    fn helltide_tax(&self, guild_id: i64, now: i64) -> Result<i64, DigRuntimeStoreError> {
        let modifier = DigGuildModifierRepository::new(&self.path)
            .get_active(Some(guild_id), now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?
            .into_iter()
            .find(|modifier| modifier.modifier_id == "helltide_active");
        Ok(modifier
            .map(|modifier| {
                modifier
                    .payload
                    .get("tax_per_dig")
                    .and_then(Value::as_i64)
                    .unwrap_or(5)
                    .max(0)
            })
            .unwrap_or(0))
    }

    fn adjust_daily_reward(
        &self,
        guild_id: i64,
        amount: i64,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<(i64, f64), DigRuntimeStoreError> {
        if amount <= 0 {
            return Ok((amount, 1.0));
        }
        let service = SqliteEconomyEventService::new(&self.path, economy_config.clone());
        let adjusted = service
            .adjust_reward_at(guild_id, amount, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let multiplier = (adjusted as f64 / amount as f64).max(0.0);
        Ok((adjusted, multiplier))
    }

    fn daily_reward_multiplier(
        &self,
        guild_id: i64,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<f64, DigRuntimeStoreError> {
        let effects = SqliteEconomyEventService::new(&self.path, economy_config.clone())
            .effects_at(guild_id, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        Ok(effects.reward_multiplier.max(0.0))
    }

    fn paid_dig_cost_modifier(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<f64, DigRuntimeStoreError> {
        let today = game_date_for_timestamp(now as f64)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        // Python treats a failed mana lookup as one neutral request-local
        // snapshot; it must not turn an otherwise valid Dig into an error or
        // retry the repository later in the same action.
        let row = match ManaRepository::new(&self.path).get_mana(discord_id, Some(guild_id)) {
            Ok(row) => row,
            Err(_) => return Ok(0.0),
        };
        let modifier = row
            .filter(|row| row.assigned_date == today && !row.consumed_today)
            .map(|row| match row.current_land.as_str() {
                "Mountain" => -0.05,
                _ => 0.0,
            })
            .unwrap_or(0.0);
        Ok((1.0_f64 + modifier).max(0.0))
    }

    fn cave_in_mana_hazard_modifier(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<f64, DigRuntimeStoreError> {
        let today = game_date_for_timestamp(now as f64)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let row = ManaRepository::new(&self.path)
            .get_mana(discord_id, Some(guild_id))
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        Ok(row
            .filter(|row| row.assigned_date == today && !row.consumed_today)
            .map(|row| match row.current_land.as_str() {
                "Forest" | "Mountain" => {
                    if row.current_land == "Forest" {
                        -0.01
                    } else {
                        0.01
                    }
                }
                "Swamp" => 0.01,
                _ => 0.0,
            })
            .unwrap_or(0.0))
    }

    fn overgrowth_active(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<bool, DigRuntimeStoreError> {
        let active = ManashopRepository::new(&self.path)
            .active_for(discord_id, Some(guild_id), "overgrowth", now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        Ok(active
            .iter()
            .any(|buff| buff.data.charges_remaining.unwrap_or(0) > 0))
    }

    fn auto_buy_items(
        &self,
        request: AutoBuyRequest<'_>,
    ) -> Result<Vec<cama_db::dig_inventory_repository::AutoBuyItemOutcome>, DigRuntimeStoreError>
    {
        DigInventoryRepository::new(&self.path)
            .ensure_auto_buy_items_atomic(request)
            .map_err(|error| DigRuntimeStoreError::Inventory(error.to_string()))
    }

    fn claim_slow_drip(
        &self,
        snapshot: &DigRuntimeSnapshot,
        now: i64,
        economy_config: &EconomyEventConfig,
    ) -> Result<Option<DigSlowDripClaim>, DigRuntimeStoreError> {
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let equipped = snapshot.artifacts.iter().any(|artifact| {
            artifact.is_relic && artifact.equipped && artifact.artifact_id == "slow_drip"
        });
        if !equipped {
            return Ok(None);
        }
        let claim_date = game_date_for_timestamp(now as f64)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let state = ManashopRepository::new(&self.path)
            .slow_drip_today(tunnel.discord_id, Some(tunnel.guild_id), &claim_date)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let claimed_before = state.claimed_today.clamp(0, 100);
        if claimed_before >= 100 {
            ManashopRepository::new(&self.path)
                .stamp_slow_drip_seen(tunnel.discord_id, Some(tunnel.guild_id), &claim_date, now)
                .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
            return Ok(None);
        }
        let Some(anchor_before) = (state.last_claim_at > 0)
            .then_some(state.last_claim_at)
            .or(tunnel.last_dig_at.filter(|anchor| *anchor > 0))
        else {
            ManashopRepository::new(&self.path)
                .stamp_slow_drip_seen(tunnel.discord_id, Some(tunnel.guild_id), &claim_date, now)
                .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
            return Ok(None);
        };
        if anchor_before >= now {
            return Ok(None);
        }
        let elapsed_minutes = now
            .saturating_sub(anchor_before)
            .checked_div(60)
            .unwrap_or(0);
        let gross = settle_slow_drip_claim(elapsed_minutes, claimed_before, 1.0);
        if gross.gross_jc <= 0 {
            return Ok(None);
        }
        let economy_adjusted = SqliteEconomyEventService::new(&self.path, economy_config.clone())
            .adjust_reward_at(tunnel.guild_id, gross.gross_jc, now)
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let credit_jc = scale_positive_dig_jc(economy_adjusted);
        let claim = DigSlowDripClaim {
            claim_date,
            gross_jc: gross.gross_jc,
            credit_jc,
            claimed_before,
            claimed_after: claimed_before.saturating_add(gross.gross_jc),
            anchor_before,
            expected_last_claim_at: state.last_claim_at,
            claimed_at: now,
        };

        // Python's fail-soft boundary is intentional: consume the gross cap
        // first, then perform the independent wallet credit.  Use the
        // expected claim row as a CAS so two concurrent Digs cannot both
        // consume the same idle interval or cross the daily cap.
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE slow_drip_claims
                SET claimed_today=claimed_today+?1,last_claim_at=?2
              WHERE discord_id=?3 AND guild_id=?4 AND claim_date=?5
                AND claimed_today=?6 AND last_claim_at=?7",
            params![
                claim.gross_jc,
                claim.claimed_at,
                tunnel.discord_id,
                tunnel.guild_id,
                claim.claim_date,
                claim.claimed_before,
                claim.expected_last_claim_at,
            ],
        )?;
        if updated != 1 {
            let inserted = transaction.execute(
                "INSERT INTO slow_drip_claims
                     (discord_id,guild_id,claim_date,claimed_today,last_claim_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(discord_id,guild_id,claim_date) DO NOTHING",
                params![
                    tunnel.discord_id,
                    tunnel.guild_id,
                    claim.claim_date,
                    claim.gross_jc,
                    claim.claimed_at,
                ],
            )?;
            if inserted != 1 {
                // Another claimant won the row CAS.  This is a normal
                // duplicate click, not a Dig failure.
                return Ok(None);
            }
        }
        transaction.commit()?;
        if claim.credit_jc > 0 {
            let metadata = serde_json::json!({
                "claim_date": claim.claim_date,
                "gross_jc": claim.gross_jc,
                "credit_jc": claim.credit_jc,
                "claimed_before": claim.claimed_before,
                "claimed_after": claim.claimed_after,
                "anchor_before": claim.anchor_before,
                "claimed_at": claim.claimed_at,
            })
            .to_string();
            let credit_result = (|| -> Result<(), DigRuntimeStoreError> {
                let mut connection = self.connection()?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let balance = transaction
                    .query_row(
                        "SELECT COALESCE(jopacoin_balance,0) FROM players
                         WHERE discord_id=?1 AND guild_id=?2",
                        params![tunnel.discord_id, tunnel.guild_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                let Some(balance) = balance else {
                    return Err(DigRuntimeStoreError::MissingPlayer);
                };
                let balance_after = balance
                    .checked_add(claim.credit_jc)
                    .ok_or(DigRuntimeStoreError::Conflict)?;
                set_runtime_ledger_context(
                    &transaction,
                    tunnel.discord_id,
                    "slow_drip",
                    &claim.claim_date,
                    "slow drip credit",
                    &metadata,
                )?;
                let changed = transaction.execute(
                    "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                    params![balance_after, tunnel.discord_id, tunnel.guild_id, balance],
                )?;
                clear_runtime_ledger_context(&transaction)?;
                if changed != 1 {
                    return Err(DigRuntimeStoreError::Conflict);
                }
                transaction.commit()?;
                Ok(())
            })();
            if let Err(error) = credit_result {
                let _ = error;
                return Ok(Some(DigSlowDripClaim {
                    credit_jc: 0,
                    ..claim
                }));
            }
        }
        Ok(Some(claim))
    }

    fn canonical_event_id(
        &self,
        snapshot: &DigRuntimeSnapshot,
        now: i64,
        in_boss: bool,
        entropy_seed: u64,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        let Some(tunnel) = snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let actor = event_actor_snapshot(snapshot, tunnel.depth, tunnel.luminosity);
        let quest = DigEventRuntimeRepository::new(&self.path)
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: tunnel.discord_id,
                    guild_id: Some(tunnel.guild_id),
                },
                now,
            )
            .map_err(|error| DigRuntimeStoreError::Event(error.to_string()))?;
        let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite(&self.path);
        let mut entropy = SeededLootEntropy::new(entropy_seed);
        Ok(service
            .roll_event_for_snapshot(&actor, &quest, true, in_boss, &mut entropy)
            .map(|event| event.event_id))
    }

    fn canonical_event_id_for_snapshot(
        &self,
        request: DigRuntimeCanonicalEventRequest<'_>,
    ) -> Result<Option<String>, DigRuntimeStoreError> {
        let Some(_tunnel) = request.snapshot.tunnel.as_ref() else {
            return Ok(None);
        };
        let actor = event_actor_snapshot(request.snapshot, request.depth, request.luminosity);
        let mut entropy = crate::dig_loot::ScriptedLootEntropy::new(
            [f64::from_bits(request.selection_roll_bits)],
            [],
        );
        let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite(&self.path);
        Ok(service
            .roll_event_for_snapshot_with_modifiers(
                crate::dig_event_runtime::CanonicalEventRollInput {
                    snapshot: &actor,
                    quest_snapshot: request.quest,
                    include_quest_events: true,
                    in_boss: request.in_boss,
                    void_bait_active: request.void_bait_active,
                    rare_event_multiplier: request.rare_event_multiplier,
                    legendary_event_multiplier: request.legendary_event_multiplier,
                },
                &mut entropy,
            )
            .map(|event| event.event_id))
    }

    fn preview_pet_dig_work(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        decay_per_day: i64,
    ) -> Result<Option<PetDigWork>, DigRuntimeStoreError> {
        if decay_per_day <= 0 {
            return Ok(None);
        }
        let Some(pet) = PetRepository::new(&self.path)
            .get_active_pet(discord_id, Some(guild_id))
            .map_err(|error| DigRuntimeStoreError::Pet(error.to_string()))?
        else {
            return Ok(None);
        };
        let settled_at = if let Some(died_at) = pet.died_at {
            now.min(died_at)
        } else {
            pet.starvation_time(decay_per_day)
                .map_err(|error| DigRuntimeStoreError::Pet(error.to_string()))?
                .min(now)
        }
        .max(pet.dig_work_at);
        let accrued = pet
            .dig_work_units_between(pet.dig_work_at, settled_at, decay_per_day)
            .map_err(|error| DigRuntimeStoreError::Pet(error.to_string()))?;
        let cap = DIG_WORK_CAP_BLOCKS.saturating_mul(DIG_WORK_UNITS_PER_BLOCK);
        Ok(Some(PetDigWork {
            pet_id: pet.pet_id,
            pet_name: pet.name,
            expected_units: pet.dig_work_units,
            expected_at: pet.dig_work_at,
            accrued_units: pet.dig_work_units.saturating_add(accrued).min(cap),
            as_of: settled_at,
        }))
    }

    fn commit_with_delivery(
        &self,
        mut request: DigRuntimeCommit,
        draft: DigRuntimeDeliveryDraft,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        request.delivery_draft = Some(draft);
        self.commit(request)
    }

    fn commit(
        &self,
        request: DigRuntimeCommit,
    ) -> Result<DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let player_balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![
                    request
                        .next
                        .tunnel
                        .as_ref()
                        .map_or(0, |tunnel| tunnel.discord_id),
                    request
                        .next
                        .tunnel
                        .as_ref()
                        .map_or(0, |tunnel| tunnel.guild_id)
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(player_balance) = player_balance else {
            return Err(DigRuntimeStoreError::MissingPlayer);
        };
        if player_balance != request.expected.balance {
            return Err(DigRuntimeStoreError::Conflict);
        }
        let Some(next_tunnel) = request.next.tunnel.as_ref() else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        let discord_id = next_tunnel.discord_id;
        let guild_id = next_tunnel.guild_id;

        for item_id in &request.consumed_item_ids {
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM dig_inventory
                     WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
                    params![item_id, discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(DigRuntimeStoreError::MissingQueuedItem(*item_id));
            }
        }

        let existing_tunnel = transaction
            .query_row(
                TUNNEL_SELECT,
                params![discord_id, guild_id, format!("Miner {discord_id}")],
                |row| load_tunnel_row(row, discord_id, guild_id),
            )
            .optional()?;
        let existing = existing_tunnel
            .as_ref()
            .map(|tunnel| (tunnel.depth, tunnel.total_digs, tunnel.last_dig_at));
        let live_inventory = load_inventory_transaction(&transaction, discord_id, guild_id)?;
        let live_artifacts = load_artifacts_transaction(&transaction, discord_id, guild_id)?;
        let live_gear = load_gear_transaction(&transaction, discord_id, guild_id)?;
        if !version_matches(existing, request.expected)
            || existing_tunnel
                .as_ref()
                .is_some_and(|tunnel| fingerprint(tunnel) != request.expected.tunnel_fingerprint)
            || fingerprint(&live_inventory) != request.expected.inventory_fingerprint
            || fingerprint(&live_artifacts) != request.expected.artifact_fingerprint
            || fingerprint(&live_gear) != request.expected.gear_fingerprint
        {
            return Err(DigRuntimeStoreError::Conflict);
        }

        if let Some(claim) = request.pet_work_claim {
            let changed = transaction.execute(
                "UPDATE pets
                    SET dig_work_units=?1, dig_work_at=?2
                  WHERE pet_id=?3 AND discord_id=?4 AND guild_id=?5
                    AND died_at IS NULL
                    AND dig_work_units=?6 AND dig_work_at=?7",
                params![
                    claim.new_units,
                    claim.new_at,
                    claim.pet_id,
                    discord_id,
                    guild_id,
                    claim.expected_units,
                    claim.expected_at,
                ],
            )?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::PetWorkConflict);
            }
        }

        if existing_tunnel.is_none() {
            transaction.execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,total_jc_earned,
                  luminosity,prestige_level,prestige_perks,boss_progress,boss_attempts,
                  route_state,injury_state,hard_hat_charges,reinforced_until,void_bait_digs,
                  sonar_skip_pending,temp_buffs,temp_curses,stat_strength,stat_smarts,
                  stat_stamina,stat_points,paid_digs_today,paid_dig_date,pickaxe_tier,
                  current_run_jc,current_run_artifacts,current_run_events,streak_days,
                  auto_buy_torch,auto_buy_hard_hat,last_dig_at,best_run_score,
                  total_prestige_score,streak_last_date,trap_active,trap_free_today,
                  trap_date,insured_until,revenge_target,revenge_type,revenge_until,
                  cheer_data,grappling_hook_charges,lantern_stub_date,thick_skin_date,
                  mutations,engine_mode,miner_origin,miner_about,stat_boss_awards,
                  stinger_curse,last_lum_update_at,pinnacle_boss_id,pinnacle_phase,
                  pinnacle_hp_remaining,pinnacle_last_engaged_at,retreat_cooldown_until,
                  last_cheer_at,cavein_free_streak,relic_trim_notice)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                         ?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,
                         ?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50,
                         ?51,?52,?53,?54,?55,?56,?57,?58,?59,?60,?61,?62,?63)",
                params![
                    discord_id,
                    guild_id,
                    next_tunnel.tunnel_name,
                    next_tunnel.depth,
                    next_tunnel.max_depth,
                    next_tunnel.total_digs,
                    next_tunnel.total_jc_earned,
                    next_tunnel.luminosity,
                    next_tunnel.prestige_level,
                    next_tunnel.prestige_perks,
                    next_tunnel.boss_progress,
                    next_tunnel.boss_attempts,
                    next_tunnel.route_state,
                    next_tunnel.injury_state,
                    next_tunnel.hard_hat_charges,
                    next_tunnel.reinforced_until,
                    next_tunnel.void_bait_digs,
                    i64::from(next_tunnel.sonar_skip_pending),
                    next_tunnel.temp_buffs,
                    next_tunnel.temp_curses,
                    next_tunnel.stat_strength,
                    next_tunnel.stat_smarts,
                    next_tunnel.stat_stamina,
                    next_tunnel.stat_points,
                    next_tunnel.paid_digs_today,
                    next_tunnel.paid_dig_date,
                    next_tunnel.pickaxe_tier,
                    next_tunnel.current_run_jc,
                    next_tunnel.current_run_artifacts,
                    next_tunnel.current_run_events,
                    next_tunnel.streak_days,
                    i64::from(next_tunnel.auto_buy_torch),
                    i64::from(next_tunnel.auto_buy_hard_hat),
                    next_tunnel.last_dig_at,
                    next_tunnel.best_run_score,
                    next_tunnel.total_prestige_score,
                    next_tunnel.streak_last_date,
                    i64::from(next_tunnel.trap_active),
                    i64::from(next_tunnel.trap_free_today),
                    next_tunnel.trap_date,
                    next_tunnel.insured_until,
                    next_tunnel.revenge_target,
                    next_tunnel.revenge_type,
                    next_tunnel.revenge_until,
                    next_tunnel.cheer_data,
                    next_tunnel.grappling_hook_charges,
                    next_tunnel.lantern_stub_date,
                    next_tunnel.thick_skin_date,
                    next_tunnel.mutations,
                    next_tunnel.engine_mode,
                    next_tunnel.miner_origin,
                    next_tunnel.miner_about,
                    next_tunnel.stat_boss_awards,
                    next_tunnel.stinger_curse,
                    next_tunnel.last_lum_update_at,
                    next_tunnel.pinnacle_boss_id,
                    next_tunnel.pinnacle_phase,
                    next_tunnel.pinnacle_hp_remaining,
                    next_tunnel.pinnacle_last_engaged_at,
                    next_tunnel.retreat_cooldown_until,
                    next_tunnel.last_cheer_at,
                    next_tunnel.cavein_free_streak,
                    i64::from(next_tunnel.relic_trim_notice),
                ],
            )?;
            // Python's tunnel constructor creates and equips a starter weapon
            // in the same admission transaction.  Keeping that invariant here
            // matters because the first dig snapshots gear before applying
            // pickaxe modifiers; a tunnel without this row silently falls
            // back to a procedural tier and loses the migrated gear identity.
            transaction.execute(
                "INSERT INTO dig_gear(
                     discord_id, guild_id, slot, tier, durability,
                     equipped, acquired_at, source, item_id
                 )
                 SELECT ?1, ?2, 'weapon', ?3, 20, 1, ?4, 'starter', NULL
                 WHERE NOT EXISTS (
                     SELECT 1 FROM dig_gear
                     WHERE discord_id=?1 AND guild_id=?2 AND slot='weapon'
                 )",
                params![discord_id, guild_id, next_tunnel.pickaxe_tier, request.now,],
            )?;
        } else {
            let changed = transaction.execute(
                "UPDATE tunnels SET depth=?1,max_depth=?2,total_digs=?3,total_jc_earned=?4,
                    last_dig_at=?5,luminosity=?6,prestige_level=?7,prestige_perks=?8,
                    boss_progress=?9,boss_attempts=?10,route_state=?11,injury_state=?12,
                    hard_hat_charges=?13,reinforced_until=?14,void_bait_digs=?15,
                    sonar_skip_pending=?16,temp_buffs=?17,temp_curses=?18,stat_strength=?19,
                    stat_smarts=?20,stat_stamina=?21,stat_points=?22,paid_digs_today=?23,
                    paid_dig_date=?24,pickaxe_tier=?25,current_run_jc=?26,
                    current_run_artifacts=?27,current_run_events=?28,streak_days=?29,
                    auto_buy_torch=?30,auto_buy_hard_hat=?31,tunnel_name=?32,
                    best_run_score=?38,total_prestige_score=?39,streak_last_date=?40,
                    trap_active=?41,trap_free_today=?42,trap_date=?43,insured_until=?44,
                    revenge_target=?45,revenge_type=?46,revenge_until=?47,cheer_data=?48,
                    grappling_hook_charges=?49,lantern_stub_date=?50,thick_skin_date=?51,
                    mutations=?52,engine_mode=?53,miner_origin=?54,miner_about=?55,
                    stat_boss_awards=?56,stinger_curse=?57,last_lum_update_at=?58,
                    pinnacle_boss_id=?59,pinnacle_phase=?60,pinnacle_hp_remaining=?61,
                    pinnacle_last_engaged_at=?62,retreat_cooldown_until=?63,
                    last_cheer_at=?64,cavein_free_streak=?65,relic_trim_notice=?66
                 WHERE discord_id=?33 AND guild_id=?34
                   AND depth=?35 AND total_digs=?36 AND last_dig_at IS ?37",
                params![
                    next_tunnel.depth,
                    next_tunnel.max_depth,
                    next_tunnel.total_digs,
                    next_tunnel.total_jc_earned,
                    next_tunnel.last_dig_at,
                    next_tunnel.luminosity,
                    next_tunnel.prestige_level,
                    next_tunnel.prestige_perks,
                    next_tunnel.boss_progress,
                    next_tunnel.boss_attempts,
                    next_tunnel.route_state,
                    next_tunnel.injury_state,
                    next_tunnel.hard_hat_charges,
                    next_tunnel.reinforced_until,
                    next_tunnel.void_bait_digs,
                    i64::from(next_tunnel.sonar_skip_pending),
                    next_tunnel.temp_buffs,
                    next_tunnel.temp_curses,
                    next_tunnel.stat_strength,
                    next_tunnel.stat_smarts,
                    next_tunnel.stat_stamina,
                    next_tunnel.stat_points,
                    next_tunnel.paid_digs_today,
                    next_tunnel.paid_dig_date,
                    next_tunnel.pickaxe_tier,
                    next_tunnel.current_run_jc,
                    next_tunnel.current_run_artifacts,
                    next_tunnel.current_run_events,
                    next_tunnel.streak_days,
                    i64::from(next_tunnel.auto_buy_torch),
                    i64::from(next_tunnel.auto_buy_hard_hat),
                    next_tunnel.tunnel_name,
                    discord_id,
                    guild_id,
                    request.expected.depth,
                    request.expected.total_digs,
                    request.expected.last_dig_at,
                    next_tunnel.best_run_score,
                    next_tunnel.total_prestige_score,
                    next_tunnel.streak_last_date,
                    i64::from(next_tunnel.trap_active),
                    i64::from(next_tunnel.trap_free_today),
                    next_tunnel.trap_date,
                    next_tunnel.insured_until,
                    next_tunnel.revenge_target,
                    next_tunnel.revenge_type,
                    next_tunnel.revenge_until,
                    next_tunnel.cheer_data,
                    next_tunnel.grappling_hook_charges,
                    next_tunnel.lantern_stub_date,
                    next_tunnel.thick_skin_date,
                    next_tunnel.mutations,
                    next_tunnel.engine_mode,
                    next_tunnel.miner_origin,
                    next_tunnel.miner_about,
                    next_tunnel.stat_boss_awards,
                    next_tunnel.stinger_curse,
                    next_tunnel.last_lum_update_at,
                    next_tunnel.pinnacle_boss_id,
                    next_tunnel.pinnacle_phase,
                    next_tunnel.pinnacle_hp_remaining,
                    next_tunnel.pinnacle_last_engaged_at,
                    next_tunnel.retreat_cooldown_until,
                    next_tunnel.last_cheer_at,
                    next_tunnel.cavein_free_streak,
                    i64::from(next_tunnel.relic_trim_notice),
                ],
            )?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }

        let balance_cost = request.balance_cost.max(0);
        if request.expected.balance < balance_cost {
            return Err(DigRuntimeStoreError::InsufficientFunds);
        }
        let balance_after_cost = request
            .expected
            .balance
            .checked_sub(balance_cost)
            .ok_or(DigRuntimeStoreError::InsufficientFunds)?;
        if balance_cost > 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                &request.action_type,
                &request.action_type,
                "paid dig cost",
                &request.detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![
                    balance_after_cost,
                    discord_id,
                    guild_id,
                    request.expected.balance
                ],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        let reward_delta = request.next.balance.saturating_sub(balance_after_cost);
        if reward_delta != 0 {
            set_runtime_ledger_context(
                &transaction,
                discord_id,
                &request.action_type,
                &request.action_type,
                runtime_ledger_reason(&request.action_type, reward_delta),
                &request.detail,
            )?;
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
                params![
                    request.next.balance,
                    discord_id,
                    guild_id,
                    balance_after_cost
                ],
            )?;
            clear_runtime_ledger_context(&transaction)?;
            if changed != 1 {
                return Err(DigRuntimeStoreError::Conflict);
            }
        }
        sync_inventory(
            &transaction,
            &request.next.inventory,
            discord_id,
            guild_id,
            request.now,
        )?;
        sync_artifacts(
            &transaction,
            &request.next.artifacts,
            discord_id,
            guild_id,
            request.now,
        )?;
        sync_gear(
            &transaction,
            &request.next.gear,
            discord_id,
            guild_id,
            request.now,
        )?;
        for item_id in &request.consumed_item_ids {
            // A dig can consume a queued item or spill an ordinary satchel
            // item.  The snapshot/CAS check above proves ownership, so the
            // same id list safely handles both removal paths.
            transaction.execute(
                "DELETE FROM dig_inventory WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
                params![item_id, discord_id, guild_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO dig_actions
             (guild_id,actor_id,target_id,action_type,depth_before,depth_after,jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",
            params![
                guild_id,
                discord_id,
                request.action_type,
                request.depth_before,
                request.depth_after,
                request.jc_delta,
                request.detail,
                request.now,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        if let Some(mut draft) = request.delivery_draft {
            draft.outcome.action_id = Some(action_id);
            if let Some(delivery) = build_delivery_snapshot(
                &draft.outcome,
                draft.discord_id,
                draft.guild_id,
                draft.context,
                draft.committed_at,
            ) {
                let mut detail_value = serde_json::from_str::<Value>(&request.detail)
                    .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
                let object = detail_value
                    .as_object_mut()
                    .ok_or(DigRuntimeStoreError::InvalidJson("dig action detail"))?;
                object.insert(
                    "delivery".to_owned(),
                    serde_json::to_value(delivery)
                        .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?,
                );
                transaction.execute(
                    "UPDATE dig_actions SET detail=?1 WHERE id=?2 AND actor_id=?3 AND guild_id=?4",
                    params![detail_value.to_string(), action_id, discord_id, guild_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(DigRuntimeCommitReceipt {
            balance_after: request.next.balance,
            action_id,
            inserted_item_ids: Vec::new(),
            inserted_artifact_ids: Vec::new(),
            inserted_gear_ids: Vec::new(),
        })
    }

    fn ensure_weather(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
    ) -> Result<Vec<DigRuntimeWeather>, DigRuntimeStoreError> {
        DigWeatherRepository::new(&self.path)
            .ensure_for_day(guild_id, game_date, now)
            .map(|entries| entries.into_iter().map(Into::into).collect())
            .map_err(|error| DigRuntimeStoreError::Weather(error.to_string()))
    }

    fn attach_delivery(
        &self,
        delivery: &DigRuntimeDeliverySnapshot,
    ) -> Result<(), DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1 AND actor_id=?2 AND guild_id=?3",
                params![delivery.action_id, delivery.discord_id, delivery.guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = detail
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let object = value
            .as_object_mut()
            .ok_or(DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        object.insert(
            "delivery".to_owned(),
            serde_json::to_value(delivery)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?,
        );
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1
             WHERE id=?2 AND actor_id=?3 AND guild_id=?4",
            params![
                value.to_string(),
                delivery.action_id,
                delivery.discord_id,
                delivery.guild_id
            ],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        transaction.commit()?;
        Ok(())
    }

    fn pending_deliveries(
        &self,
        query: DigRuntimePendingDeliveryQuery,
    ) -> Result<Vec<DigRuntimeDeliverySnapshot>, DigRuntimeStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT detail FROM dig_actions
             WHERE action_type='dig'
               AND (?1 IS NULL OR guild_id=?1)
               AND (?2 IS NULL OR actor_id=?2)
             ORDER BY id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                query.guild_id,
                query.discord_id,
                i64::try_from(query.limit).unwrap_or(i64::MAX)
            ],
            |row| row.get::<_, Option<String>>(0),
        )?;
        let mut deliveries = Vec::new();
        for row in rows {
            let Some(detail) = row? else { continue };
            let Some(raw) = serde_json::from_str::<Value>(&detail)
                .ok()
                .and_then(|value| value.get("delivery").cloned())
            else {
                continue;
            };
            let delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
                .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
            if delivery.main_delivered_at.is_none()
                || (delivery.render.kind.requires_event_part()
                    && delivery.event_delivered_at.is_none())
            {
                deliveries.push(delivery);
            }
        }
        Ok(deliveries)
    }

    fn mark_delivery_delivered(
        &self,
        request: DigRuntimeMarkDelivered,
    ) -> Result<bool, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(detail) = detail.flatten() else {
            transaction.commit()?;
            return Ok(false);
        };
        let Some(mut delivery) = serde_json::from_str::<Value>(&detail)
            .ok()
            .and_then(|value| value.get("delivery").cloned())
            .and_then(|value| serde_json::from_value::<DigRuntimeDeliverySnapshot>(value).ok())
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if delivery.source_key != request.source_key {
            transaction.commit()?;
            return Ok(false);
        }
        match request.part {
            DigRuntimeDeliveryPart::Main => {
                if delivery.main_delivered_at.is_none() {
                    delivery.main_delivered_at = Some(request.delivered_at);
                }
            }
            DigRuntimeDeliveryPart::Event => {
                if delivery.event_delivered_at.is_none() {
                    delivery.event_delivered_at = Some(request.delivered_at);
                }
            }
        }
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        value["delivery"] = serde_json::to_value(&delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    fn finalize_delivery(
        &self,
        request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = transaction
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                params![request.action_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut value = serde_json::from_str::<Value>(&detail)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("dig action detail"))?;
        let raw = value
            .get("delivery")
            .cloned()
            .ok_or(DigRuntimeStoreError::StateConflict)?;
        let mut delivery = serde_json::from_value::<DigRuntimeDeliverySnapshot>(raw)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        if delivery.source_key != request.source_key {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        delivery.flavor = request.flavor;
        if let Some(boss) = request.boss {
            delivery.render.kind = DigRuntimeRenderKind::Boss;
            delivery.render.boss = Some(boss);
        }
        delivery.render.flavor_narrative = delivery.flavor.narrative().map(str::to_owned);
        value["delivery"] = serde_json::to_value(&delivery)
            .map_err(|_| DigRuntimeStoreError::InvalidJson("delivery"))?;
        let changed = transaction.execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), request.action_id],
        )?;
        if changed != 1 {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        transaction.commit()?;
        Ok(delivery)
    }
}

fn sync_inventory(
    transaction: &Transaction<'_>,
    inventory: &[DigRuntimeInventoryItem],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for item in inventory {
        let changed = transaction.execute(
            "UPDATE dig_inventory SET queued=?1
             WHERE id=?2 AND discord_id=?3 AND guild_id=?4",
            params![i64::from(item.queued), item.id, discord_id, guild_id],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_inventory
                 (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    guild_id,
                    item.item_type,
                    i64::from(item.queued),
                    now,
                ],
            )?;
        }
    }
    Ok(())
}

fn sync_artifacts(
    transaction: &Transaction<'_>,
    artifacts: &[DigRuntimeArtifact],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for artifact in artifacts {
        let changed = transaction.execute(
            "UPDATE dig_artifacts SET equipped=?1,artifact_id=?2,is_relic=?3
             WHERE id=?4 AND discord_id=?5 AND guild_id=?6",
            params![
                i64::from(artifact.equipped),
                artifact.artifact_id,
                i64::from(artifact.is_relic),
                artifact.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    discord_id,
                    guild_id,
                    artifact.artifact_id,
                    now,
                    i64::from(artifact.is_relic),
                    i64::from(artifact.equipped),
                ],
            )?;
        }
    }
    Ok(())
}

fn sync_gear(
    transaction: &Transaction<'_>,
    gear: &[DigRuntimeGear],
    discord_id: i64,
    guild_id: i64,
    now: i64,
) -> Result<(), DigRuntimeStoreError> {
    for piece in gear {
        let changed = transaction.execute(
            "UPDATE dig_gear SET slot=?1,tier=?2,durability=?3,equipped=?4,
                    acquired_at=?5,source=?6,item_id=?7
             WHERE id=?8 AND discord_id=?9 AND guild_id=?10",
            params![
                piece.slot,
                piece.tier,
                piece.durability.max(0),
                i64::from(piece.equipped),
                piece.acquired_at,
                piece.source,
                piece.item_id,
                piece.id,
                discord_id,
                guild_id,
            ],
        )?;
        if changed == 0 {
            transaction.execute(
                "INSERT INTO dig_gear(
                     discord_id,guild_id,slot,tier,durability,equipped,
                     acquired_at,source,item_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    discord_id,
                    guild_id,
                    piece.slot,
                    piece.tier,
                    piece.durability.max(0),
                    i64::from(piece.equipped),
                    if piece.acquired_at == 0 {
                        now
                    } else {
                        piece.acquired_at
                    },
                    piece.source,
                    piece.item_id,
                ],
            )?;
        }
    }
    Ok(())
}

fn version_matches(existing: Option<(i64, i64, Option<i64>)>, expected: DigRuntimeVersion) -> bool {
    match (existing, expected.depth, expected.total_digs) {
        (None, None, None) => true,
        (Some((depth, total_digs, last_dig_at)), Some(expected_depth), Some(expected_total)) => {
            depth == expected_depth
                && total_digs == expected_total
                && last_dig_at == expected.last_dig_at
        }
        _ => false,
    }
}

fn load_inventory(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeInventoryItem>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,item_type,queued FROM dig_inventory
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeInventoryItem {
                id: row.get(0)?,
                item_type: row.get(1)?,
                queued: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect()
}

fn load_inventory_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeInventoryItem>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,item_type,queued FROM dig_inventory
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeInventoryItem {
                id: row.get(0)?,
                item_type: row.get(1)?,
                queued: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect()
}

fn load_artifacts(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeArtifact>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeArtifact {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
}

fn load_artifacts_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeArtifact>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeArtifact {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
}

fn load_gear(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeGear>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
         FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeGear {
                id: row.get(0)?,
                slot: row.get(1)?,
                tier: row.get(2)?,
                durability: row.get(3)?,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect()
}

fn load_gear_transaction(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<DigRuntimeGear>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
         FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(DigRuntimeGear {
                id: row.get(0)?,
                slot: row.get(1)?,
                tier: row.get(2)?,
                durability: row.get(3)?,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect()
}

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
    pub balance_after: i64,
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
}

/// Discord context captured before the Dig transaction commits.  The
/// delivery outbox stores this immutable projection so a restart can recover
/// a public message without depending on a live interaction object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeDeliveryContext {
    pub interaction_id: u64,
    pub channel_id: i64,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// Immutable delivery inputs captured before the actor transaction. The
/// SQLite adapter persists the resulting projection beside the action row;
/// lightweight stores use the existing attach hook as a compatibility path.
#[derive(Clone, Debug)]
pub struct DigRuntimeDeliveryDraft {
    pub discord_id: i64,
    pub guild_id: i64,
    pub outcome: DigRuntimeOutcome,
    pub context: DigRuntimeDeliveryContext,
    pub committed_at: i64,
}

impl DigRuntimeDeliveryContext {
    #[must_use]
    pub fn new(
        interaction_id: u64,
        channel_id: i64,
        display_name: impl Into<String>,
        avatar_url: Option<String>,
    ) -> Self {
        Self {
            interaction_id,
            channel_id,
            display_name: display_name.into(),
            avatar_url,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeRenderKind {
    First,
    Normal,
    Boss,
    Event,
}

impl DigRuntimeRenderKind {
    #[must_use]
    pub const fn requires_event_part(self) -> bool {
        matches!(self, Self::Event)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeEventKind {
    Simple,
    Choice,
    Boon,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeEventRenderSnapshot {
    pub event_id: String,
    pub description: String,
    pub ascii_art: Option<String>,
    pub boon_names: Vec<String>,
    pub reading_the_stone_hint: Option<String>,
    pub safe_disabled: bool,
    pub safe_label: Option<String>,
    pub risky_label: Option<String>,
    pub desperate_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeBossRenderSnapshot {
    pub boundary: i64,
    pub boss_id: String,
    pub boss_name: String,
    pub dialogue: String,
    pub is_pinnacle: bool,
    pub phase: i64,
    pub wager_allowed: bool,
    pub carried_wager: i64,
    pub has_scout_lantern: bool,
    pub luminosity: i64,
    pub encounter_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeRenderSnapshot {
    pub kind: DigRuntimeRenderKind,
    pub title: String,
    pub description: String,
    pub layer_color: u32,
    pub depth_transition: String,
    pub layer_name: String,
    pub flavor_narrative: Option<String>,
    pub footer: Option<String>,
    pub boss_boundary_copy: Option<String>,
    pub consumed_copy: Option<String>,
    pub artifact_name: Option<String>,
    pub relic_trim_notice_copy: Option<String>,
    pub event_kind: Option<DigRuntimeEventKind>,
    pub event: Option<DigRuntimeEventRenderSnapshot>,
    pub layer_media_key: String,
    pub pickaxe_tier: i64,
    pub item_media_keys: Vec<String>,
    pub boss: Option<DigRuntimeBossRenderSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeFlavorSnapshot {
    Pending,
    Applied {
        narrative: Option<String>,
        tone: Option<String>,
        callback_reference: Option<String>,
        npc_id_or_name: Option<String>,
        npc_line: Option<String>,
        picked_event_id: Option<String>,
        bonus_delta: i64,
    },
    Skipped,
}

impl DigRuntimeFlavorSnapshot {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Applied { .. } | Self::Skipped)
    }

    fn narrative(&self) -> Option<&str> {
        match self {
            Self::Applied { narrative, .. } => narrative.as_deref(),
            Self::Pending | Self::Skipped => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigRuntimeDeliverySnapshot {
    pub action_id: i64,
    pub source_key: String,
    pub discord_id: i64,
    pub guild_id: i64,
    pub committed_at: i64,
    pub context: DigRuntimeDeliveryContext,
    pub outcome: DigRuntimeOutcome,
    pub render: DigRuntimeRenderSnapshot,
    pub flavor: DigRuntimeFlavorSnapshot,
    pub main_delivered_at: Option<i64>,
    pub event_delivered_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DigRuntimeDeliveryPart {
    Main,
    Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeExecution {
    pub outcome: DigRuntimeOutcome,
    pub delivery: Option<DigRuntimeDeliverySnapshot>,
}

impl Deref for DigRuntimeExecution {
    type Target = DigRuntimeOutcome;

    fn deref(&self) -> &Self::Target {
        &self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimePendingDeliveryQuery {
    pub guild_id: Option<i64>,
    pub discord_id: Option<i64>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeMarkDelivered {
    pub action_id: i64,
    pub source_key: String,
    pub delivered_at: i64,
    pub part: DigRuntimeDeliveryPart,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeFinalizeDelivery {
    pub action_id: i64,
    pub source_key: String,
    pub flavor: DigRuntimeFlavorSnapshot,
    pub boss: Option<DigRuntimeBossRenderSnapshot>,
}

/// Typed read models used by the Discord transport.  Keeping these beside
/// the aggregate prevents provider code from becoming a second SQL read
/// model implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeTunnelInfo {
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub last_dig_at: Option<i64>,
    pub pickaxe_tier: i64,
    pub prestige_level: i64,
    pub luminosity: i64,
    pub tunnel_name: String,
    pub route_state: Option<String>,
}

/// Canonical `/dig flex` projection. Discord copy and entropy stay in the
/// transport, while persisted boss/title normalization remains here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeFlexData {
    pub tunnel_name: String,
    pub depth: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub prestige_emoji: String,
    pub titles: Vec<String>,
    pub streak: i64,
    pub layer: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigRuntimeWeatherPresentation {
    pub layer: String,
    pub name: String,
    pub description: String,
    pub effects: DigWeatherEffects,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeLeaderboardRow {
    pub name: String,
    pub depth: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeHallOfFameRow {
    pub name: String,
    pub user_id: i64,
    pub score: i64,
    pub prestige: i64,
}

/// Outcome of an administrator mutation that requires an existing tunnel.
///
/// Keeping the missing-tunnel case distinct prevents Discord adapters from
/// reporting a successful maintenance action when SQLite updated no rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigAdminMutationOutcome {
    Applied,
    MissingTunnel,
}

/// Typed result for a staged inventory, event, or route action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigRuntimeActionResult {
    pub success: bool,
    pub error: Option<String>,
    pub item: Option<String>,
    pub item_id: Option<i64>,
    pub route_id: Option<String>,
    pub cost: i64,
    pub queued: bool,
    pub balance_after: i64,
    pub action_id: Option<i64>,
}

/// Durable identity and policy inputs for one event component interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRuntimeEventRequest<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_id: &'a str,
    pub choice: &'a str,
    pub event_key: &'a str,
    pub now: i64,
    pub chained: bool,
}

/// Full typed event result retained through Discord presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct DigRuntimeEventOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub resolution: Option<CanonicalEventResolution>,
    pub depth_before: i64,
    pub depth_after: i64,
    pub balance_after: i64,
    pub action_id: Option<i64>,
    pub reward_row_id: Option<i64>,
    pub applied_now: bool,
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DigWeatherEffects {
    pub cave_in_bonus: f64,
    pub cave_in_loss_bonus: i64,
    pub cave_in_loss_cap: Option<i64>,
    pub advance_bonus: i64,
    pub event_chance_multiplier: f64,
    pub luminosity_drain_multiplier: f64,
    pub jc_multiplier: f64,
    pub jc_bonus: i64,
    pub artifact_multiplier: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ActiveCurseEffects {
    advance_bonus: i64,
    cave_in_bonus: f64,
    cooldown_penalty: f64,
    jc_bonus: i64,
    luminosity_drain: i64,
}

fn active_curse_effects(raw: Option<&str>) -> Option<(ActiveCurseEffects, i64)> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    let remaining = value.get("digs_remaining")?.as_i64()?;
    if remaining <= 0 {
        return None;
    }
    let effect = value.get("effect").or_else(|| value.get("effects"));
    let number_i64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_i64)
    };
    let number_f64 = |key: &str| {
        effect
            .and_then(|effect| effect.get(key))
            .and_then(Value::as_f64)
    };
    Some((
        ActiveCurseEffects {
            advance_bonus: number_i64("advance_bonus").unwrap_or(0),
            cave_in_bonus: number_f64("cave_in_bonus").unwrap_or(0.0),
            cooldown_penalty: number_f64("cooldown_penalty").unwrap_or(0.0),
            jc_bonus: number_i64("jc_bonus").unwrap_or(0),
            luminosity_drain: number_i64("luminosity_drain").unwrap_or(0),
        },
        remaining,
    ))
}

fn active_buff_cave_in_reduction(raw: Option<&str>) -> Option<(f64, i64)> {
    let value = serde_json::from_str::<Value>(raw?).ok()?;
    let remaining = value.get("digs_remaining")?.as_i64()?;
    if remaining <= 0 {
        return None;
    }
    let reduction = value
        .get("effect")
        .or_else(|| value.get("effects"))
        .and_then(|effect| effect.get("cave_in_reduction"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Some((reduction, remaining))
}

impl DigWeatherEffects {
    const fn neutral() -> Self {
        Self {
            cave_in_bonus: 0.0,
            cave_in_loss_bonus: 0,
            cave_in_loss_cap: None,
            advance_bonus: 0,
            event_chance_multiplier: 0.0,
            luminosity_drain_multiplier: 0.0,
            jc_multiplier: 0.0,
            jc_bonus: 0,
            artifact_multiplier: 0.0,
        }
    }
}

fn weather_effects(weather_id: &str) -> DigWeatherEffects {
    // These are the authored Python weather modifiers. Keeping the mapping
    // keyed by the persisted id makes unknown/migrated rows fail closed to a
    // neutral effect while remaining visible in the outcome detail.
    match weather_id {
        "earthworm_migration" => DigWeatherEffects {
            advance_bonus: 1,
            jc_bonus: -1,
            ..DigWeatherEffects::neutral()
        },
        "mudslide_warning" => DigWeatherEffects {
            cave_in_bonus: 0.10,
            cave_in_loss_cap: Some(3),
            ..DigWeatherEffects::neutral()
        },
        "root_overgrowth" => DigWeatherEffects {
            advance_bonus: -1,
            artifact_multiplier: 2.0,
            ..DigWeatherEffects::neutral()
        },
        "fossil_rush" => DigWeatherEffects {
            artifact_multiplier: 2.0,
            ..DigWeatherEffects::neutral()
        },
        "seismic_tremors" => DigWeatherEffects {
            cave_in_bonus: 0.08,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "mineral_vein" => DigWeatherEffects {
            jc_bonus: 2,
            ..DigWeatherEffects::neutral()
        },
        "crystal_resonance" => DigWeatherEffects {
            jc_multiplier: -0.25,
            ..DigWeatherEffects::neutral()
        },
        "prismatic_surge" => DigWeatherEffects {
            event_chance_multiplier: 1.0,
            jc_bonus: 3,
            ..DigWeatherEffects::neutral()
        },
        "shatter_warning" => DigWeatherEffects {
            cave_in_bonus: 0.12,
            jc_bonus: 3,
            ..DigWeatherEffects::neutral()
        },
        "eruption" => DigWeatherEffects {
            cave_in_bonus: 0.12,
            jc_multiplier: 0.75,
            ..DigWeatherEffects::neutral()
        },
        "cooling_period" => DigWeatherEffects {
            cave_in_bonus: -0.10,
            jc_multiplier: -0.25,
            ..DigWeatherEffects::neutral()
        },
        "lava_bloom" => DigWeatherEffects {
            artifact_multiplier: 1.5,
            luminosity_drain_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "void_tide" => DigWeatherEffects {
            cave_in_loss_bonus: 2,
            ..DigWeatherEffects::neutral()
        },
        "whisper_storm" => DigWeatherEffects {
            event_chance_multiplier: 1.0,
            ..DigWeatherEffects::neutral()
        },
        "deep_calm" => DigWeatherEffects {
            cave_in_bonus: -0.12,
            event_chance_multiplier: -0.50,
            ..DigWeatherEffects::neutral()
        },
        "spore_bloom" => DigWeatherEffects {
            advance_bonus: 2,
            luminosity_drain_multiplier: 1.0,
            ..DigWeatherEffects::neutral()
        },
        "mycelium_pulse" => DigWeatherEffects {
            jc_multiplier: 0.50,
            event_chance_multiplier: 0.25,
            ..DigWeatherEffects::neutral()
        },
        "fungal_frenzy" => DigWeatherEffects {
            event_chance_multiplier: 2.0,
            cave_in_bonus: 0.08,
            ..DigWeatherEffects::neutral()
        },
        "time_dilation" => DigWeatherEffects {
            jc_multiplier: 1.0,
            advance_bonus: -1,
            ..DigWeatherEffects::neutral()
        },
        "frozen_stillness" => DigWeatherEffects {
            cave_in_bonus: -1.0,
            event_chance_multiplier: -1.0,
            ..DigWeatherEffects::neutral()
        },
        "temporal_storm" => DigWeatherEffects {
            cave_in_bonus: 0.15,
            jc_multiplier: 0.75,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "hollow_breathes" => DigWeatherEffects {
            jc_multiplier: 0.50,
            cave_in_bonus: 0.10,
            event_chance_multiplier: 0.50,
            ..DigWeatherEffects::neutral()
        },
        "void_harvest" => DigWeatherEffects {
            artifact_multiplier: 3.0,
            cave_in_bonus: 0.15,
            ..DigWeatherEffects::neutral()
        },
        "deep_silence" => DigWeatherEffects {
            cave_in_bonus: -0.15,
            jc_multiplier: -0.50,
            ..DigWeatherEffects::neutral()
        },
        _ => DigWeatherEffects::neutral(),
    }
}

fn weather_code(weather_id: Option<&str>) -> Option<&str> {
    let weather_id = weather_id?;
    if weather_id.contains("storm") {
        Some("storm")
    } else if weather_id.contains("sunny") {
        Some("sunny")
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DigGearEffects {
    advance_bonus: i64,
    cave_in_reduction: f64,
    loot_bonus: i64,
}

fn active_pickaxe_tier(gear: &[DigRuntimeGear], tunnel: &DigRuntimeTunnel) -> i64 {
    if let Some(weapon) = gear
        .iter()
        .find(|piece| piece.equipped && piece.slot == "weapon")
    {
        return if weapon.durability > 0 {
            weapon.tier.max(0)
        } else {
            0
        };
    }
    tunnel.pickaxe_tier.max(0)
}

fn gear_effects(gear: &[DigRuntimeGear], tunnel: &DigRuntimeTunnel) -> DigGearEffects {
    let Some(weapon) = gear
        .iter()
        .find(|piece| piece.equipped && piece.slot == "weapon")
    else {
        return WEAPON_TIERS
            .get(usize::try_from(tunnel.pickaxe_tier.max(0)).unwrap_or_default())
            .map_or_else(DigGearEffects::default, |definition| DigGearEffects {
                advance_bonus: i64::from(definition.advance_bonus),
                cave_in_reduction: definition.cave_in_reduction,
                loot_bonus: i64::from(definition.loot_bonus),
            });
    };
    if weapon.durability <= 0 {
        return DigGearEffects::default();
    }
    if let Some(unique) = weapon.item_id.as_deref().and_then(unique_gear) {
        return DigGearEffects {
            advance_bonus: i64::from(unique.advance_bonus),
            cave_in_reduction: unique.cave_in_reduction,
            loot_bonus: i64::from(unique.loot_bonus),
        };
    }
    WEAPON_TIERS
        .get(usize::try_from(weapon.tier.max(0)).unwrap_or_default())
        .map_or_else(DigGearEffects::default, |definition| DigGearEffects {
            advance_bonus: i64::from(definition.advance_bonus),
            cave_in_reduction: definition.cave_in_reduction,
            loot_bonus: i64::from(definition.loot_bonus),
        })
}

fn runtime_gear_name(piece: &DigRuntimeGear) -> String {
    if let Some(item_id) = piece.item_id.as_deref()
        && let Some(definition) = unique_gear(item_id)
    {
        return definition.name.to_owned();
    }
    let tier = usize::try_from(piece.tier.max(0)).unwrap_or_default();
    let name = match piece.slot.as_str() {
        "armor" => ARMOR_TIERS.get(tier).map(|definition| definition.name),
        "boots" => BOOTS_TIERS.get(tier).map(|definition| definition.name),
        "amulet" => AMULET_TIERS.get(tier).map(|definition| definition.name),
        "weapon" => WEAPON_TIERS.get(tier).map(|definition| definition.name),
        _ => None,
    };
    name.map_or_else(
        || format!("{} tier {}", piece.slot, piece.tier),
        str::to_owned,
    )
}

/// Tick equipped gear for a cave-in and report only newly broken pieces.
///
/// Broken pieces remain equipped, but are not applicable to a later
/// `gear_nick` consequence.  This mirrors the repository's durability
/// contract while keeping the Dig runtime's staged snapshot authoritative.
pub fn apply_cave_in_gear_ticks(gear: &mut [DigRuntimeGear], ticks: u32) -> Vec<String> {
    let mut broken = Vec::new();
    for _ in 0..ticks {
        for piece in gear.iter_mut().filter(|piece| piece.equipped) {
            if piece.durability == 1 {
                let name = runtime_gear_name(piece);
                if !broken.contains(&name) {
                    broken.push(name);
                }
            }
            piece.durability = piece.durability.saturating_sub(1).max(0);
        }
    }
    broken
}

/// Apply the catastrophic depth rule after the ordinary block-loss roll.
/// Insurance covers only the milestone rollback; the ordinary loss remains in
/// place, exactly as in the Python service.
#[must_use]
pub fn catastrophic_cave_in_depth(
    depth_before: i64,
    block_loss: i64,
    block_loss_cap: Option<i64>,
    insured: bool,
) -> (i64, bool, i64) {
    let ordinary_depth = (depth_before - block_loss.max(0)).max(0);
    if insured {
        return (ordinary_depth, true, block_loss.max(0));
    }
    let milestone = ((depth_before.saturating_sub(1).max(0))
        / i64::from(CAVE_IN_CATASTROPHIC_MILESTONE_STEP))
        * i64::from(CAVE_IN_CATASTROPHIC_MILESTONE_STEP);
    let capped_depth = block_loss_cap
        .map(|cap| (depth_before - cap.max(0)).max(0))
        .unwrap_or(milestone);
    let depth_after = milestone.max(capped_depth);
    (
        depth_after,
        false,
        (depth_before - depth_after).max(block_loss.max(0)),
    )
}

struct CaveInLootRng<'a>(&'a mut SeededLootEntropy);

impl CaveInRng for CaveInLootRng<'_> {
    fn random_unit(&mut self) -> f64 {
        self.0.unit()
    }

    fn random_inclusive(&mut self, lower: u32, upper: u32) -> u32 {
        let lower = i64::from(lower);
        let upper = i64::from(upper);
        u32::try_from(self.0.advance(lower, upper)).unwrap_or(lower as u32)
    }
}

struct LootRelicEntropy<'a>(&'a mut SeededLootEntropy);

impl RelicEntropy for LootRelicEntropy<'_> {
    fn unit(&mut self) -> f64 {
        self.0.unit()
    }
}

/// Adapt the live Dig entropy stream to the pure prestige-four artifact policy.
///
/// The adapter is intentionally local to the runtime transaction: corruption,
/// cave consequences, JC, artifact, and event selection all advance the same
/// request-local stream, while the pure policy remains persistence-free.
struct DigPrestige4Entropy<'a>(&'a mut SeededLootEntropy);

impl Prestige4Entropy for DigPrestige4Entropy<'_> {
    fn unit(&mut self) -> f64 {
        self.0.unit()
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        self.0.choose_index(upper_bound)
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
            balance_after: snapshot.balance,
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
        }
    }
}

/// Full application orchestration for a Dig request.
#[derive(Clone, Debug)]
pub struct DigRuntimeService<S = SqliteDigRuntimeStore> {
    store: S,
    config: DigRuntimeConfig,
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
        let mut statement = connection.prepare(
            "SELECT COALESCE(tunnel_name,'Unnamed Tunnel'), depth
             FROM tunnels WHERE guild_id=?1
             ORDER BY depth DESC, total_jc_earned DESC, discord_id ASC LIMIT 10",
        )?;
        Ok(statement
            .query_map([guild_id], |row| {
                Ok(DigRuntimeLeaderboardRow {
                    name: row.get(0)?,
                    depth: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn hall_of_fame(
        &self,
        guild_id: i64,
    ) -> Result<Vec<DigRuntimeHallOfFameRow>, DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        let mut statement = connection.prepare(
            "SELECT COALESCE(tunnel_name,'Unnamed Tunnel'), discord_id,
                    best_run_score, prestige_level
             FROM tunnels WHERE guild_id=?1 AND best_run_score > 0
             ORDER BY best_run_score DESC, prestige_level DESC, discord_id ASC LIMIT 10",
        )?;
        Ok(statement
            .query_map([guild_id], |row| {
                Ok(DigRuntimeHallOfFameRow {
                    name: row.get(0)?,
                    user_id: row.get(1)?,
                    score: row.get(2)?,
                    prestige: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
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
        transaction.execute(
            "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                    updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![amount, discord_id, guild_id],
        )?;
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
        let target = transaction
            .query_row(
                "SELECT depth, max_depth FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![target_id, guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((depth, max_depth)) = target else {
            transaction.commit()?;
            return Ok("That miner has not started a tunnel yet.".to_owned());
        };
        let depth_after = depth.saturating_add(1);
        transaction.execute(
            "UPDATE tunnels SET depth=?1, max_depth=?2
             WHERE discord_id=?3 AND guild_id=?4",
            params![depth_after, max_depth.max(depth_after), target_id, guild_id],
        )?;
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, ?3, 'help', ?4, ?5, 0, '{}', ?6)",
            params![guild_id, actor_id, target_id, depth, depth_after, now],
        )?;
        transaction.commit()?;
        Ok(format!(
            "You steadied <@{target_id}>'s tunnel and helped them reach depth **{depth_after}**."
        ))
    }

    /// Return the provider-facing sabotage preview from a read-only snapshot.
    pub fn sabotage_preview(
        &self,
        target_id: i64,
        guild_id: i64,
    ) -> Result<(i64, String), DigRuntimeStoreError> {
        let snapshot = self.store.snapshot(target_id, guild_id)?;
        let Some(tunnel) = snapshot.tunnel else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        Ok((5_i64.max(tunnel.depth / 5), "3–8 blocks".to_owned()))
    }

    /// Apply sabotage through one immediate transaction.  The player-facing
    /// provider only renders this result; it does not debit or mutate either
    /// tunnel itself.
    pub fn sabotage(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<String, DigRuntimeStoreError> {
        if actor_id == target_id {
            return Ok("You can't sabotage yourself.".to_owned());
        }
        let snapshot = self.store.snapshot(target_id, guild_id)?;
        let Some(tunnel) = snapshot.tunnel else {
            return Ok("That miner has not started a tunnel yet.".to_owned());
        };
        let cost = 5_i64.max(tunnel.depth / 5);
        let mut connection = self.store.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![actor_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = balance else {
            transaction.commit()?;
            return Ok("You must be registered first.".to_owned());
        };
        if balance < cost {
            transaction.commit()?;
            return Ok(format!(
                "Sabotage costs {cost} {JOPACOIN_EMOTE}; your balance is {balance}."
            ));
        }
        transaction.execute(
            "UPDATE players SET jopacoin_balance=jopacoin_balance-?1,
                    updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![cost, actor_id, guild_id],
        )?;
        let depth_after = tunnel.depth.saturating_sub(3).max(0);
        transaction.execute(
            "UPDATE tunnels SET depth=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![depth_after, target_id, guild_id],
        )?;
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, ?3, 'sabotage', ?4, ?5, ?6, '{}', ?7)",
            params![
                guild_id,
                actor_id,
                target_id,
                tunnel.depth,
                depth_after,
                -cost,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(format!(
            "Sabotage complete. The target lost **{}** blocks; you spent **{cost}** {JOPACOIN_EMOTE}.",
            tunnel.depth.saturating_sub(depth_after)
        ))
    }

    /// Transfer one owned relic to another registered miner atomically.
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
        let target_exists = transaction
            .query_row(
                "SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![target_id, guild_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !target_exists {
            transaction.commit()?;
            return Ok(false);
        }
        let changed = transaction.execute(
            "UPDATE dig_artifacts SET discord_id=?1, guild_id=?2, equipped=0, found_at=?3
             WHERE id = (
                 SELECT id FROM dig_artifacts
                 WHERE discord_id=?4 AND guild_id=?2 AND artifact_id=?5
                   AND is_relic=1
                 ORDER BY id LIMIT 1
             )",
            params![target_id, guild_id, now, owner_id, artifact_id],
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
        let changed = connection.execute(
            "UPDATE tunnels SET last_dig_at=0
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )?;
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
        let changed = connection.execute(
            "UPDATE tunnels SET depth=?1, last_dig_at=0
             WHERE discord_id=?2 AND guild_id=?3",
            params![depth.max(0), discord_id, guild_id],
        )?;
        Ok(if changed == 1 {
            DigAdminMutationOutcome::Applied
        } else {
            DigAdminMutationOutcome::MissingTunnel
        })
    }

    pub fn set_about(
        &self,
        discord_id: i64,
        guild_id: i64,
        about: &str,
    ) -> Result<(), DigRuntimeStoreError> {
        let connection = self.store.connection()?;
        let existing = connection
            .query_row(
                "SELECT COALESCE(miner_about,'') FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Err(DigRuntimeStoreError::MissingTunnel);
        };
        if !existing.is_empty() {
            return Err(DigRuntimeStoreError::StateConflict);
        }
        connection.execute(
            "UPDATE tunnels SET miner_about=?1
             WHERE discord_id=?2 AND guild_id=?3",
            params![about.trim(), discord_id, guild_id],
        )?;
        Ok(())
    }

    pub fn spend_stats(
        &self,
        discord_id: i64,
        guild_id: i64,
        strength: i64,
        smarts: i64,
        stamina: i64,
    ) -> Result<String, DigRuntimeStoreError> {
        let strength = strength.max(0);
        let smarts = smarts.max(0);
        let stamina = stamina.max(0);
        let requested = strength.saturating_add(smarts).saturating_add(stamina);
        if requested == 0 {
            return Ok("Choose at least one point to spend.".to_owned());
        }
        let connection = self.store.connection()?;
        let changed = connection.execute(
            "UPDATE tunnels SET stat_strength=stat_strength+?1,
                    stat_smarts=stat_smarts+?2, stat_stamina=stat_stamina+?3,
                    stat_points=stat_points-?4
             WHERE discord_id=?5 AND guild_id=?6 AND stat_points>=?4",
            params![strength, smarts, stamina, requested, discord_id, guild_id],
        )?;
        if changed == 0 {
            return Ok("Not enough unspent S points for that build.".to_owned());
        }
        Ok(format!(
            "Build updated: +{strength} Strength, +{smarts} Smarts, +{stamina} Stamina."
        ))
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
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(balance) = balance else {
            transaction.commit()?;
            return Ok("You must be registered first.".to_owned());
        };
        let tunnel_stats = transaction
            .query_row(
                "SELECT stat_strength, stat_smarts, stat_stamina, stat_points
                   FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
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
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        transaction.execute(
            "INSERT INTO economy_ledger_context (
                 id, source, actor_id, related_type, related_id, reason, metadata
             ) VALUES (1, 'dig', ?1, 'miner_respec', 's_points',
                       'dig miner respec debit', ?2)",
            params![discord_id, detail],
        )?;
        let debited = transaction.execute(
            "UPDATE players SET jopacoin_balance=jopacoin_balance-?1,
                    updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance>=?1",
            params![COST, discord_id, guild_id],
        )?;
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        if debited != 1 {
            transaction.rollback()?;
            return Ok(format!(
                "Respec costs {COST} {JOPACOIN_EMOTE}; your balance is {balance}."
            ));
        }
        transaction.execute(
            "UPDATE players SET lowest_balance_ever=jopacoin_balance
             WHERE discord_id=?1 AND guild_id=?2
               AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
            params![discord_id, guild_id],
        )?;
        let changed = transaction.execute(
            "UPDATE tunnels SET stat_points=stat_points+stat_strength+stat_smarts+stat_stamina,
                    stat_strength=0, stat_smarts=0, stat_stamina=0
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )?;
        if changed == 0 {
            transaction.rollback()?;
            return Ok("You don't have a tunnel yet. Use /dig go to start.".to_owned());
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, NULL, 'miner_respec', 0, 0, ?3, ?4, ?5)",
            params![guild_id, discord_id, -COST, detail, now],
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
            "torch" => connection.execute(
                "UPDATE tunnels SET auto_buy_torch=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![value, discord_id, guild_id],
            )?,
            "hard_hat" => connection.execute(
                "UPDATE tunnels SET auto_buy_hard_hat=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![value, discord_id, guild_id],
            )?,
            "both" => connection.execute(
                "UPDATE tunnels SET auto_buy_torch=?1, auto_buy_hard_hat=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![value, discord_id, guild_id],
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
        Self { store, config }
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

    pub fn finalize_delivery(
        &self,
        request: DigRuntimeFinalizeDelivery,
    ) -> Result<DigRuntimeDeliverySnapshot, DigRuntimeStoreError> {
        self.store.finalize_delivery(request)
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
                balance_after: current.balance,
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
                balance_after: current.balance,
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
        let mana_paid_multiplier =
            self.store
                .paid_dig_cost_modifier(request.discord_id, request.guild_id, now)?;
        let paid_cost_preview =
            ((relic_paid_cost as f64 * mana_paid_multiplier.max(0.0)) as i64).max(1);
        let (curse_fx, _curse_remaining) =
            active_curse_effects(tunnel.temp_curses.as_deref()).unwrap_or_default();
        let base_cooldown_seconds = if injury_slows_cooldown(tunnel.injury_state.as_deref()) {
            6 * 3_600
        } else {
            3_600
        };
        let cooldown_seconds = (base_cooldown_seconds as f64
            * (1.0 + curse_fx.cooldown_penalty.clamp(0.0, 0.25)))
            as i64;
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
            if tunnel.auto_buy_torch {
                selections.push(AutoBuySelection {
                    item_type: "torch",
                    price: 6,
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
            )?
        } else {
            None
        };
        let pet_name = pet_work.as_ref().map(|work| work.pet_name.clone());

        if first_dig {
            let mut entropy = SeededLootEntropy::new(seed_for(request));
            let advance = entropy.advance(3, 7);
            let jc_roll = entropy.advance(1, 5);
            let (daily_adjusted_jc, economy_multiplier) = self.store.adjust_daily_reward(
                request.guild_id,
                jc_roll,
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
            let next = apply_state(&current, state, &today, false, 0);
            let commit = DigRuntimeCommit {
                expected: DigRuntimeVersion::from(&snapshot),
                next,
                delivery_draft: None,
                consumed_item_ids: Vec::new(),
                pet_work_claim,
                depth_before: 0,
                depth_after: first.advance,
                jc_delta: first.jc_earned,
                balance_cost: 0,
                action_type: "dig".to_owned(),
                detail: serde_json::json!({
                    "first_dig": true,
                    "gross_jc": jc_roll,
                    "economy_adjusted_jc": daily_adjusted_jc,
                    "economy_reward_multiplier": economy_multiplier,
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
                balance_after,
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
        let mutation_fx = mutation_effects(&mutations_from_json(tunnel.mutations.as_deref()));
        // Corruption is the first request-local random policy in Python. It
        // must consume the same entropy stream as the subsequent cave roll,
        // rather than a second seed that would shift only some Dig paths.
        let mut entropy = SeededLootEntropy::new(seed_for(request));
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
        let mana_hazard_modifier =
            self.store
                .cave_in_mana_hazard_modifier(request.discord_id, request.guild_id, now)?;
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
            active_buff_reduction: active_buff_cave_in_reduction(tunnel.temp_buffs.as_deref())
                .map_or(0.0, |(reduction, _)| reduction),
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
                + gear_fx.advance_bonus,
            advance_min: None,
            advance_max: active_route
                .and_then(|route| route_effect(route, "advance_max_penalty"))
                .map(|penalty| (layer_at(depth_before).advance_range.1 - penalty as i64).max(1)),
            event_chance_multiplier: (1.0
                + route_event_delta
                + weather_fx.event_chance_multiplier
                + ascension_number("event_chance_multiplier"))
            .max(0.0),
            luminosity_drain_multiplier: (1.0 + route_luminosity_delta).max(0.0),
            luminosity_drain_bonus: deep_luminosity_drain_bonus(depth_before),
            luminosity_pickaxe_reduction: active_pickaxe_tier >= 6,
            injury_reduces_advance,
            jc_multiplier: (1.0 + weather_fx.jc_multiplier + ascension_number("jc_multiplier"))
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
        let gross_jc = loot
            .entropy_mut()
            .advance(layer.jc_range.0, layer.jc_range.1);
        let relic_yield_multiplier = {
            let mut entropy = LootRelicEntropy(loot.entropy_mut());
            relic_jc_yield_multiplier(
                &relics,
                YieldContext {
                    weather_code: weather_code(weather_id),
                    luminosity: Some(tunnel.luminosity),
                    is_first_dig_today: is_first_dig_of_day(tunnel.last_dig_at, now),
                    is_paid_dig: paid_charge_active,
                    include_random: true,
                },
                &mut entropy,
            )
        };
        let post_pinnacle_multiplier = post_pinnacle_decay_factor(depth_before, &relics);
        let helltide_tax = self.store.helltide_tax(request.guild_id, now)?.max(0);
        let economy_multiplier = self.store.daily_reward_multiplier(
            request.guild_id,
            now,
            &self.config.economy_event,
        )?;
        let economy_multiplier_basis_points = (economy_multiplier.max(0.0) * 10_000.0 + 0.5) as i64;
        let streak_days = next_daily_streak(&tunnel, &today);
        let streak_reward = crate::dig_service::streak_bonus(streak_days);
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
        let mut state = tunnel_state(&staged, paid_charge_active.then_some(paid_cost));
        state.depth = depth_before;
        let outcome_input = DigOutcomeInput {
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
            weather_yield_percent: if loot_outcome.cave_in {
                100
            } else {
                (loot_modifiers.jc_multiplier
                    * relic_yield_multiplier
                    * post_pinnacle_multiplier
                    * 100.0) as i64
            },
            flat_jc_bonus: if loot_outcome.cave_in {
                0
            } else {
                loot_modifiers.jc_bonus
            },
            economy_reward_multiplier_basis_points: economy_multiplier_basis_points,
            economy_before_positive_scale: loot_outcome.cave_in,
            streak_bonus: streak_reward,
            helltide_tax,
            ..DigOutcomeInput::default()
        };
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
        staged = apply_state(&staged, state, &today, paid_charge_active, paid_cost);
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
        if let Some((_, remaining)) = active_buff_cave_in_reduction(tunnel.temp_buffs.as_deref())
            && let Some(next_tunnel) = staged.tunnel.as_mut()
        {
            next_tunnel.temp_buffs = if remaining <= 1 {
                None
            } else {
                let mut buff =
                    serde_json::from_str::<Value>(tunnel.temp_buffs.as_deref().unwrap_or("{}"))
                        .unwrap_or_else(|_| serde_json::json!({}));
                if let Some(value) = buff.get_mut("digs_remaining") {
                    *value = Value::from(remaining - 1);
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
            "block_loss": outcome.cave_in.then_some(depth_before - outcome.depth_after),
            "cave_in_detail": cave_in_detail_value.clone(),
            "event": event_id.clone(),
            "artifact": artifact_id.clone(),
            "items_used": loot_outcome.items_used,
            "paid": paid_charge_active,
            "pet_dig_bonus": pet_dig_bonus,
            "helltide_tax": helltide_tax,
            "gross_jc": outcome.economy_gross_jc,
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
        let commit = DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next: staged,
            delivery_draft: None,
            consumed_item_ids: consumed_item_ids.clone(),
            pet_work_claim,
            depth_before,
            depth_after: outcome.depth_after,
            jc_delta: outcome.jc_earned.saturating_sub(if paid_charge_active {
                paid_cost
            } else {
                0
            }),
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
            jc_earned: outcome.jc_earned,
            balance_after,
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
            depth_before: tunnel.depth,
            depth_after: tunnel.depth,
            jc_delta: 0,
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
            SeededLootEntropy::new(seed_for(DigRuntimeRequest {
                discord_id,
                guild_id,
                now,
                paid: false,
                forced_event: false,
            })),
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
            depth_before: depth,
            depth_after: depth,
            jc_delta: -result.cost,
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

fn build_delivery_snapshot(
    outcome: &DigRuntimeOutcome,
    discord_id: i64,
    guild_id: i64,
    context: DigRuntimeDeliveryContext,
    committed_at: i64,
) -> Option<DigRuntimeDeliverySnapshot> {
    let action_id = outcome.action_id?;
    let layer = layer_at(outcome.depth_after);
    let (kind, event_kind, event) = if let Some(event_id) = outcome.event_id.as_deref() {
        let authored = crate::dig_loot::canonical_event(event_id)?;
        let description = authored
            .descriptions
            .get(
                usize::try_from(action_id).unwrap_or_default() % authored.descriptions.len().max(1),
            )
            .cloned()
            .unwrap_or_default();
        let event_kind = match authored.complexity {
            crate::dig_loot::EventComplexity::Simple => DigRuntimeEventKind::Simple,
            crate::dig_loot::EventComplexity::Choice
            | crate::dig_loot::EventComplexity::Complex => DigRuntimeEventKind::Choice,
            crate::dig_loot::EventComplexity::Boon => DigRuntimeEventKind::Boon,
        };
        let event = DigRuntimeEventRenderSnapshot {
            event_id: authored.id.clone(),
            description,
            ascii_art: authored.ascii_art.clone(),
            boon_names: authored
                .boon_options
                .iter()
                .map(|boon| boon.name.clone())
                .collect(),
            reading_the_stone_hint: None,
            safe_disabled: false,
            safe_label: authored
                .safe_option
                .as_ref()
                .map(|choice| choice.label.clone()),
            risky_label: authored
                .risky_option
                .as_ref()
                .map(|choice| choice.label.clone()),
            desperate_label: authored
                .desperate_option
                .as_ref()
                .map(|choice| choice.label.clone()),
        };
        (DigRuntimeRenderKind::Event, Some(event_kind), Some(event))
    } else if outcome.boss_boundary.is_some() {
        (DigRuntimeRenderKind::Boss, None, None)
    } else if outcome.first_dig {
        (DigRuntimeRenderKind::First, None, None)
    } else {
        (DigRuntimeRenderKind::Normal, None, None)
    };
    let description = if outcome.first_dig {
        "You've started digging your very own tunnel!\n\nUse `/dig` to advance deeper, `/dig shop` to buy items, and `/dig guide` for a full tutorial.\n\nGood luck, miner! **DIG DUG!**".to_owned()
    } else {
        format!(
            "**{}** reached **{}** blocks in **{}**.\nAdvanced **{}** blocks and earned **{}** JC. Balance: **{}** JC.",
            context.display_name,
            outcome.depth_after,
            layer.name,
            outcome.advance,
            outcome.jc_earned,
            outcome.balance_after,
        )
    };
    let artifact_name = outcome.artifact_id.as_deref().map(|artifact_id| {
        crate::dig_loot::artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .map_or_else(
                || artifact_id.replace('_', " "),
                |artifact| artifact.name.to_owned(),
            )
    });
    let render = DigRuntimeRenderSnapshot {
        kind,
        title: if outcome.first_dig {
            "Welcome to the Mines!".to_owned()
        } else if kind == DigRuntimeRenderKind::Boss {
            "Boss boundary reached".to_owned()
        } else {
            "Dig complete".to_owned()
        },
        description,
        layer_color: delivery_layer_color(layer.name),
        depth_transition: format!("{} → {}", outcome.depth_before, outcome.depth_after),
        layer_name: layer.name.to_owned(),
        flavor_narrative: None,
        footer: outcome.cave_in.then(|| {
            "The cave-in was contained; inspect your tunnel before the next dig.".to_owned()
        }),
        boss_boundary_copy: outcome
            .boss_boundary
            .map(|boundary| format!("A boss encounter begins at depth {boundary}.")),
        consumed_copy: (!outcome.items_used.is_empty()).then(|| outcome.items_used.join(", ")),
        artifact_name,
        relic_trim_notice_copy: outcome
            .relic_trim_notice
            .then(|| "Your relic loadout was trimmed to the active capacity.".to_owned()),
        event_kind,
        event,
        layer_media_key: layer.name.to_owned(),
        pickaxe_tier: outcome.pickaxe_tier,
        item_media_keys: outcome.items_used.clone(),
        boss: None,
    };
    Some(DigRuntimeDeliverySnapshot {
        action_id,
        source_key: format!("dig:{action_id}"),
        discord_id,
        guild_id,
        committed_at,
        context,
        outcome: outcome.clone(),
        render,
        // Even the first-dig copy runs through the flavor service. With AI
        // disabled that service records a durable terminal `Skipped` receipt;
        // omitting the pending phase would lose the audited flavor boundary.
        flavor: DigRuntimeFlavorSnapshot::Pending,
        main_delivered_at: None,
        event_delivered_at: None,
    })
}

fn delivery_layer_color(layer_name: &str) -> u32 {
    match layer_name {
        "Dirt" => 0x8E_6E_53,
        "Stone" => 0x95_A5_A6,
        "Crystal" => 0x9B_59_B6,
        "Magma" => 0xE7_4C_3C,
        "Abyss" => 0x34_49_5E,
        "Fungal Depths" => 0x2E_CC_71,
        "Frozen Core" => 0x74_B9_FF,
        _ => 0x58_65_F2,
    }
}

fn seed_for(request: DigRuntimeRequest) -> u64 {
    let mut value = request.discord_id as u64;
    value = value.rotate_left(17) ^ request.guild_id as u64;
    value = value.rotate_left(23) ^ request.now as u64;
    value ^ u64::from(request.paid) ^ (u64::from(request.forced_event) << 1)
}

fn parked_boss_boundary(tunnel: &DigRuntimeTunnel) -> Option<i64> {
    let Value::Object(progress) = serde_json::from_str::<Value>(&tunnel.boss_progress).ok()? else {
        return None;
    };
    progress
        .into_iter()
        .filter_map(|(raw_boundary, value)| {
            let boundary = raw_boundary.parse::<i64>().ok()?;
            let status = value.as_str().map(str::to_owned).or_else(|| {
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })?;
            (status != "defeated" && tunnel.depth >= boundary.saturating_sub(1)).then_some(boundary)
        })
        .min()
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
        for (boundary, status) in progress {
            if status
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "defeated")
                && let Ok(boundary) = boundary.parse::<i64>()
            {
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
        stats: tunnel.stats(),
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
    paid_cost: i64,
) -> DigRuntimeSnapshot {
    let mut next = snapshot.clone();
    next.balance = state.balance;
    if let Some(tunnel) = next.tunnel.as_mut() {
        tunnel.depth = state.depth;
        tunnel.max_depth = state.max_depth;
        tunnel.total_digs = state.total_digs;
        tunnel.last_dig_at = state.last_dig_at;
        tunnel.luminosity = state.luminosity;
        tunnel.total_jc_earned = tunnel.total_jc_earned.max(0).saturating_add(
            state
                .balance
                .saturating_sub(snapshot.balance)
                .saturating_add(if paid { paid_cost } else { 0 }),
        );
        if paid {
            tunnel.paid_digs_today = tunnel.paid_digs_today.saturating_add(1);
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
mod tests {
    use cama_db::core_repositories::{NewPlayer, PlayerRepository};
    use cama_db::dig_event_runtime::{
        DigEventActorKey, DigEventQuestMutation, DigEventRuntimeRepository,
    };
    use cama_db::dig_guild_modifiers::DigGuildModifierRepository;
    use cama_db::dig_inventory_repository::{BuyInsuranceOutcome, SetTrapOutcome};
    use cama_db::economy_event_repository::{
        EconomyEventRepository, EventDirection, EventDraft, EventEffects,
    };
    use cama_db::schema_manager::initialize_or_migrate;
    use cama_domain::pet::DIG_WORK_UNITS_PER_BLOCK;
    use rusqlite::{Connection, params};
    use serde_json::Value;
    use tempfile::NamedTempFile;

    use crate::dig_loot::{LootEntropy, SeededLootEntropy};

    use super::{
        DigAdminMutationOutcome, DigRuntimeCommit, DigRuntimeConfig, DigRuntimeDeliveryContext,
        DigRuntimeDeliveryDraft, DigRuntimeDeliveryPart, DigRuntimeMarkDelivered,
        DigRuntimePendingDeliveryQuery, DigRuntimeRequest, DigRuntimeService, DigRuntimeSnapshot,
        DigRuntimeStore, DigRuntimeStoreError, DigRuntimeVersion, InMemoryDigRuntimeStore,
        SqliteDigRuntimeStore,
    };

    const PET_DAY: i64 = DIG_WORK_UNITS_PER_BLOCK;

    fn seed_runtime_pet(connection: &Connection, now: i64, work_units: i64) -> i64 {
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',?1,?1,0,?1,100,?2,?1)",
                params![now - 2 * PET_DAY, work_units],
            )
            .expect("seed runtime pet");
        connection.last_insert_rowid()
    }

    fn live_event_fixture() -> (NamedTempFile, SqliteDigRuntimeStore, DigRuntimeSnapshot) {
        let database = NamedTempFile::new().expect("temporary event picker database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(8_701, "live-event-picker", Some(8_702)))
            .expect("seed event picker player");
        let connection = Connection::open(database.path()).expect("open event picker database");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     total_digs, last_dig_at, prestige_level,
                     prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, 25, 25, 100, 1, ?3, 0, '[]', '{}', 4)",
                params![8_701_i64, 8_702_i64, 1_699_996_400_i64],
            )
            .expect("seed event picker tunnel");
        drop(connection);
        let store = SqliteDigRuntimeStore::new(database.path());
        let snapshot = store.snapshot(8_701, 8_702).expect("event picker snapshot");
        (database, store, snapshot)
    }

    fn set_live_picker_quest(database: &NamedTempFile, step: i64) {
        DigEventRuntimeRepository::new(database.path())
            .apply_quest_mutation(
                DigEventActorKey {
                    discord_id: 8_701,
                    guild_id: Some(8_702),
                },
                DigEventQuestMutation::SetActive {
                    quest_id: "agh_lost_trial",
                    next_step: step,
                },
                1_700_000_000,
            )
            .expect("set live picker quest");
    }

    fn canonical_seed_for(
        snapshot: &DigRuntimeSnapshot,
        quest: &cama_db::dig_event_runtime::DigEventQuestSnapshot,
        in_boss: bool,
        target: &str,
    ) -> u64 {
        let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite("");
        let tunnel = snapshot.tunnel.as_ref().expect("live tunnel");
        let actor = cama_db::dig_event_runtime::DigEventActorSnapshot {
            key: DigEventActorKey {
                discord_id: tunnel.discord_id,
                guild_id: Some(tunnel.guild_id),
            },
            depth: tunnel.depth,
            luminosity: tunnel.luminosity,
            prestige_level: tunnel.prestige_level,
            prestige_perks_json: tunnel.prestige_perks.clone(),
            boss_progress_json: tunnel.boss_progress.clone(),
            streak_days: tunnel.streak_days,
            temp_buff_json: tunnel.temp_buffs.clone(),
            temp_curse_json: tunnel.temp_curses.clone(),
            balance: snapshot.balance,
            inventory_count: snapshot.inventory.len(),
            owned_gear: snapshot
                .gear
                .iter()
                .filter_map(|piece| piece.item_id.clone())
                .collect(),
            equipped_gear: snapshot
                .gear
                .iter()
                .filter(|piece| piece.equipped && piece.durability > 0)
                .filter_map(|piece| piece.item_id.clone())
                .collect(),
            owned_artifacts: snapshot
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone())
                .collect(),
            equipped_relics: snapshot
                .artifacts
                .iter()
                .filter(|artifact| artifact.is_relic && artifact.equipped)
                .map(|artifact| artifact.artifact_id.clone())
                .collect(),
        };
        for seed in 0..10_000_u64 {
            let mut entropy = SeededLootEntropy::new(seed);
            if service
                .roll_event_for_snapshot(&actor, quest, true, in_boss, &mut entropy)
                .is_some_and(|event| event.event_id == target)
            {
                return seed;
            }
        }
        panic!("no deterministic seed for canonical event {target}");
    }

    #[test]
    fn test_live_dig_roll_event_excludes_quest_events_without_player_context() {
        let (_database, store, mut snapshot) = live_event_fixture();
        snapshot.tunnel = None;
        assert_eq!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, 0)
                .expect("missing actor remains fail-soft"),
            None
        );
    }

    #[test]
    fn test_live_dig_roll_event_includes_quest_starter_when_eligible() {
        let (database, store, snapshot) = live_event_fixture();
        let quest = DigEventRuntimeRepository::new(database.path())
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: 8_701,
                    guild_id: Some(8_702),
                },
                1_700_000_000,
            )
            .expect("default quest snapshot");
        let seed = canonical_seed_for(&snapshot, &quest, false, "agh_s1");
        assert_eq!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
                .expect("live canonical picker"),
            Some("agh_s1".to_owned())
        );
    }

    #[test]
    fn test_live_dig_roll_event_excludes_quest_event_when_player_on_different_step() {
        let (database, store, snapshot) = live_event_fixture();
        set_live_picker_quest(&database, 3);
        let quest = DigEventRuntimeRepository::new(database.path())
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: 8_701,
                    guild_id: Some(8_702),
                },
                1_700_000_000,
            )
            .expect("active quest snapshot");
        let seed = canonical_seed_for(&snapshot, &quest, false, "agh_s3");
        assert_eq!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
                .expect("live canonical picker"),
            Some("agh_s3".to_owned())
        );
        for seed in 0..128_u64 {
            assert_ne!(
                store
                    .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
                    .expect("live canonical picker"),
                Some("agh_s1".to_owned())
            );
        }
    }

    #[test]
    fn test_live_dig_roll_event_excludes_quest_event_during_boss_combat() {
        let (database, store, snapshot) = live_event_fixture();
        let quest = DigEventRuntimeRepository::new(database.path())
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: 8_701,
                    guild_id: Some(8_702),
                },
                1_700_000_000,
            )
            .expect("default quest snapshot");
        let _ = quest;
        for seed in 0..128_u64 {
            let event_id = store
                .canonical_event_id(&snapshot, 1_700_000_000, true, seed)
                .expect("live canonical picker");
            if let Some(event_id) = event_id {
                assert!(!event_id.starts_with("agh_s"));
            }
        }
    }

    #[test]
    fn test_live_dig_roll_event_passes_tunnel_through_to_quest_filter() {
        let (database, store, mut snapshot) = live_event_fixture();
        let quest = DigEventRuntimeRepository::new(database.path())
            .quest_snapshot(
                DigEventActorKey {
                    discord_id: 8_701,
                    guild_id: Some(8_702),
                },
                1_700_000_000,
            )
            .expect("default quest snapshot");
        let at_gate_seed = canonical_seed_for(&snapshot, &quest, false, "agh_s1");
        snapshot.tunnel.as_mut().expect("live tunnel").depth = 24;
        assert_ne!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, at_gate_seed)
                .expect("live canonical picker"),
            Some("agh_s1".to_owned())
        );
        snapshot.tunnel.as_mut().expect("live tunnel").depth = 25;
        assert_eq!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, at_gate_seed)
                .expect("live canonical picker"),
            Some("agh_s1".to_owned())
        );
    }

    #[test]
    fn first_dig_is_atomic_and_deterministic() {
        assert_eq!(
            super::DigRuntimeConfig::default().asset_root,
            std::path::PathBuf::from(super::DEFAULT_DIG_ASSET_ROOT)
        );
        let snapshot = DigRuntimeSnapshot::fresh(7, 9, 100, 1_700_000_000);
        let store = InMemoryDigRuntimeStore::new(snapshot);
        let service = DigRuntimeService::new(store.clone());
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("first dig should commit");
        assert!(outcome.success);
        assert!(outcome.first_dig);
        assert!((3..=7).contains(&outcome.advance));
        let current = store.current().expect("snapshot remains available");
        assert_eq!(
            current.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
            Some(1)
        );
        assert_eq!(
            current.tunnel.as_ref().map(|tunnel| tunnel.last_dig_at),
            Some(Some(1_700_000_000))
        );
    }

    #[test]
    fn sqlite_delivery_outbox_round_trips_and_marks_main_part_once() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "delivery-test", Some(9)))
            .expect("seed player");
        let service = DigRuntimeService::sqlite(database.path());
        let execution = service
            .dig_with_delivery(
                DigRuntimeRequest {
                    discord_id: 7,
                    guild_id: 9,
                    now: 1_700_000_000,
                    paid: false,
                    forced_event: false,
                },
                DigRuntimeDeliveryContext::new(99, 11, "Delivery Miner", None),
            )
            .expect("dig and outbox attach");
        let delivery = execution.delivery.expect("delivery snapshot");
        assert_eq!(delivery.source_key, format!("dig:{}", delivery.action_id));
        assert_eq!(delivery.context.display_name, "Delivery Miner");
        assert_eq!(
            service
                .pending_deliveries(DigRuntimePendingDeliveryQuery {
                    guild_id: Some(9),
                    discord_id: Some(7),
                    limit: 10,
                })
                .expect("pending outbox")
                .len(),
            1
        );
        assert!(
            service
                .mark_delivery_delivered(DigRuntimeMarkDelivered {
                    action_id: delivery.action_id,
                    source_key: delivery.source_key.clone(),
                    delivered_at: 1_700_000_001,
                    part: DigRuntimeDeliveryPart::Main,
                })
                .expect("mark main")
        );
        assert!(
            service
                .pending_deliveries(DigRuntimePendingDeliveryQuery {
                    guild_id: Some(9),
                    discord_id: Some(7),
                    limit: 10,
                })
                .expect("pending after main")
                .is_empty()
        );
    }

    #[test]
    fn sqlite_delivery_outbox_invalid_detail_rolls_back_actor_and_action() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "delivery-rollback", Some(9)))
            .expect("seed player");
        let store = SqliteDigRuntimeStore::new(database.path());
        let service = DigRuntimeService::new(store.clone());
        let first = service
            .dig(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("seed first dig");
        let before = store.snapshot(7, 9).expect("snapshot before rollback");
        let mut next = before.clone();
        let tunnel = next.tunnel.as_mut().expect("seed tunnel");
        tunnel.depth = tunnel.depth.saturating_add(1);
        tunnel.total_digs = tunnel.total_digs.saturating_add(1);
        tunnel.last_dig_at = Some(1_700_000_001);
        let request = DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&before),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            depth_before: before.tunnel.as_ref().map_or(0, |tunnel| tunnel.depth),
            depth_after: before
                .tunnel
                .as_ref()
                .map_or(1, |tunnel| tunnel.depth.saturating_add(1)),
            jc_delta: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "[]".to_owned(),
            now: 1_700_000_001,
        };
        let error = store
            .commit_with_delivery(
                request,
                DigRuntimeDeliveryDraft {
                    discord_id: 7,
                    guild_id: 9,
                    outcome: first,
                    context: DigRuntimeDeliveryContext::new(99, 11, "Delivery Rollback", None),
                    committed_at: 1_700_000_001,
                },
            )
            .expect_err("invalid detail must abort the transaction");
        assert!(matches!(error, DigRuntimeStoreError::InvalidJson(_)));
        assert_eq!(
            store.snapshot(7, 9).expect("snapshot after rollback"),
            before
        );
        let action_count: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
                params![7_i64, 9_i64],
                |row| row.get(0),
            )
            .expect("action count");
        assert_eq!(action_count, 1);
    }

    #[test]
    fn stale_snapshot_is_rejected_by_cas() {
        let snapshot = DigRuntimeSnapshot::fresh(7, 9, 100, 1_700_000_000);
        let store = InMemoryDigRuntimeStore::new(snapshot.clone());
        let service = DigRuntimeService::new(store.clone());
        service
            .dig(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("first commit");
        let stale = super::DigRuntimeCommit {
            expected: super::DigRuntimeVersion::from(&snapshot),
            next: snapshot,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            depth_before: 0,
            depth_after: 1,
            jc_delta: 1,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now: 1_700_000_001,
        };
        assert!(matches!(
            store.commit(stale),
            Err(super::DigRuntimeStoreError::Conflict)
        ));
    }

    #[test]
    fn migrated_sqlite_store_commits_and_reloads_the_full_stage() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-9001_i64, 42_i64, "runtime-test", 100_i64],
            )
            .expect("insert player");
        drop(connection);

        let store = SqliteDigRuntimeStore::new(database.path());
        let service = DigRuntimeService::new(store.clone());
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: -9001,
                guild_id: 42,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("migrated SQLite first dig");
        assert!(outcome.success);
        let reloaded = store.snapshot(-9001, 42).expect("reload snapshot");
        assert_eq!(reloaded.balance, 100 + outcome.jc_earned);
        assert_eq!(
            reloaded.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
            Some(1)
        );
        assert_eq!(reloaded.gear.len(), 1);
        assert_eq!(reloaded.gear[0].slot, "weapon");
        assert_eq!(reloaded.gear[0].tier, 0);
        assert_eq!(reloaded.gear[0].durability, 20);
        assert!(reloaded.gear[0].equipped);
        assert_eq!(reloaded.gear[0].source, "starter");
        let action_count: i64 = Connection::open(database.path())
            .expect("reopen database")
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
                params![-9001_i64, 42_i64],
                |row| row.get(0),
            )
            .expect("action audit");
        assert_eq!(action_count, 1);
    }

    #[test]
    fn paid_dig_debits_exactly_once_and_records_a_distinct_cost_ledger_entry() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-9_006_i64, 42_i64, "paid-once", 100_i64],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at)
                 VALUES (?1,?2,10,10,1,?3)",
                params![-9_006_i64, 42_i64, 1_700_000_000_i64],
            )
            .expect("insert tunnel");
        connection
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear setup ledger");
        drop(connection);

        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id: -9_006,
                guild_id: 42,
                now: 1_700_000_001,
                paid: true,
                forced_event: false,
            })
            .expect("paid dig transaction");
        assert!(outcome.success);
        assert_eq!(outcome.paid_dig_cost, 3);

        let connection = Connection::open(database.path()).expect("reopen paid database");
        let balance = connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![-9_006_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("paid balance");
        assert_eq!(balance, 100 - outcome.paid_dig_cost + outcome.jc_earned);
        assert_eq!(balance, outcome.balance_after);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2 AND delta=?3
                       AND source='dig' AND reason='paid dig cost'",
                    params![-9_006_i64, 42_i64, -outcome.paid_dig_cost],
                    |row| row.get::<_, i64>(0),
                )
                .expect("paid cost ledger count"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2",
                    params![-9_006_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("paid ledger net"),
            balance - 100
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT paid_digs_today FROM tunnels
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![-9_006_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("paid counter"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("ledger context cleared"),
            0
        );
    }

    #[test]
    fn sqlite_commit_preserves_distinct_trailing_tunnel_columns() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-9002_i64, 42_i64, "trailing-columns", 100_i64],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,
                  pinnacle_last_engaged_at,retreat_cooldown_until,last_cheer_at,
                  cavein_free_streak,relic_trim_notice)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    -9002_i64, 42_i64, 10_i64, 20_i64, 30_i64, 40_i64, 51_i64, 62_i64, 73_i64,
                    84_i64, 1_i64,
                ],
            )
            .expect("insert tunnel sentinels");
        drop(connection);

        let store = SqliteDigRuntimeStore::new(database.path());
        let snapshot = store.snapshot(-9002, 42).expect("snapshot");
        let mut next = snapshot.clone();
        let tunnel = next.tunnel.as_mut().expect("tunnel");
        tunnel.depth = 11;
        tunnel.max_depth = 21;
        store
            .commit(super::DigRuntimeCommit {
                expected: super::DigRuntimeVersion::from(&snapshot),
                next,
                delivery_draft: None,
                consumed_item_ids: Vec::new(),
                pet_work_claim: None,
                depth_before: 10,
                depth_after: 11,
                jc_delta: 0,
                balance_cost: 0,
                action_type: "dig".to_owned(),
                detail: "{}".to_owned(),
                now: 1_700_000_001,
            })
            .expect("commit stage");

        let reloaded = store.snapshot(-9002, 42).expect("reload snapshot");
        let tunnel = reloaded.tunnel.expect("persisted tunnel");
        assert_eq!(tunnel.pinnacle_last_engaged_at, Some(51));
        assert_eq!(tunnel.retreat_cooldown_until, Some(62));
        assert_eq!(tunnel.last_cheer_at, Some(73));
        assert_eq!(tunnel.cavein_free_streak, 84);
        assert!(tunnel.relic_trim_notice);
    }

    #[test]
    fn admin_tunnel_mutations_report_missing_and_preserve_permanent_depth() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,last_dig_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![-9_003_i64, 42_i64, 75_i64, 275_i64, 1_700_000_000_i64],
            )
            .expect("insert tunnel sentinels");
        drop(connection);

        let service = DigRuntimeService::sqlite(database.path());
        assert_eq!(
            service
                .set_depth(-9_003, 42, 12)
                .expect("set current depth"),
            DigAdminMutationOutcome::Applied
        );
        let (depth, max_depth, last_dig_at) = Connection::open(database.path())
            .expect("reopen migrated database")
            .query_row(
                "SELECT depth,max_depth,last_dig_at FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![-9_003_i64, 42_i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("read admin mutation");
        assert_eq!((depth, max_depth, last_dig_at), (12, 275, 0));

        assert_eq!(
            service
                .reset_cooldown(-9_003, 42)
                .expect("reset existing cooldown"),
            DigAdminMutationOutcome::Applied
        );
        assert_eq!(
            service
                .set_depth(-9_999, 42, 12)
                .expect("missing set-depth target"),
            DigAdminMutationOutcome::MissingTunnel
        );
        assert_eq!(
            service
                .reset_cooldown(-9_999, 42)
                .expect("missing reset target"),
            DigAdminMutationOutcome::MissingTunnel
        );
    }

    #[test]
    fn flex_projection_normalizes_boss_shapes_titles_and_prestige_badge() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(-9_004, "flex-test", Some(42)))
            .expect("insert player through the canonical repository");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,tunnel_name,depth,total_digs,total_jc_earned,
                  prestige_level,streak_days,boss_progress)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    -9_004_i64,
                    42_i64,
                    "The Test Shaft",
                    205_i64,
                    123_i64,
                    456_i64,
                    8_i64,
                    9_i64,
                    r#"{"25":"defeated","50":{"status":"defeated"},"75":"defeated","100":{"status":"defeated"},"150":"defeated","200":"defeated","275":{"status":"defeated"}}"#,
                ],
            )
            .expect("insert flex tunnel");
        drop(connection);

        let flex = DigRuntimeService::sqlite(database.path())
            .flex_data(-9_004, 42)
            .expect("flex projection")
            .expect("existing tunnel");
        assert_eq!(flex.tunnel_name, "The Test Shaft");
        assert_eq!(flex.depth, 205);
        assert_eq!(flex.total_digs, 123);
        assert_eq!(flex.total_jc_earned, 456);
        assert_eq!(flex.prestige_level, 8);
        assert_eq!(flex.prestige_emoji, "⭐⭐⭐⭐⭐");
        assert_eq!(flex.titles, ["Boss Slayer"]);
        assert_eq!(flex.streak, 9);
        assert_eq!(flex.layer, "Frozen Core");

        Connection::open(database.path())
            .expect("reopen migrated database")
            .execute(
                "UPDATE tunnels SET boss_progress=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![r#"{"25":"defeated"}"#, -9_004_i64, 42_i64],
            )
            .expect("make boss progress partial");
        let partial = DigRuntimeService::sqlite(database.path())
            .flex_data(-9_004, 42)
            .expect("partial flex projection")
            .expect("existing tunnel");
        assert!(partial.titles.is_empty());
        assert_eq!(
            DigRuntimeService::sqlite(database.path())
                .flex_data(-9_999, 42)
                .expect("missing flex projection"),
            None
        );
    }

    #[test]
    fn test_dig_atomic_balance_update_records_dig_context() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-301_i64, 42_i64, "event-test", 10_i64],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels (discord_id,guild_id,depth)
                 VALUES (?1,?2,0)",
                params![-301_i64, 42_i64],
            )
            .expect("insert tunnel");
        connection
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear setup ledger");
        drop(connection);

        let detail = r#"{"event_id":"crystal_garden","choice":"safe"}"#;
        let action_id = SqliteDigRuntimeStore::new(database.path())
            .atomic_tunnel_balance_update(super::AtomicTunnelBalanceUpdate {
                discord_id: -301,
                guild_id: 42,
                balance_delta: 5,
                balance_cost: 0,
                depth_after: Some(3),
                detail,
                action_type: "event",
                now: 2_000_000_005,
            })
            .expect("dig balance update");
        let connection = Connection::open(database.path()).expect("reopen database");
        let ledger = connection
            .query_row(
                "SELECT delta,source,actor_id,related_type,related_id,reason,metadata
                   FROM economy_ledger_entries",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .expect("event ledger row");
        assert_eq!(ledger.0, 5);
        assert_eq!(ledger.1, "dig");
        assert_eq!(ledger.2, Some(-301));
        assert_eq!(ledger.3.as_deref(), Some("event"));
        assert_eq!(ledger.4.as_deref(), Some("crystal_garden"));
        assert_eq!(ledger.5.as_deref(), Some("dig event credit"));
        assert_eq!(ledger.6, detail);
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![-301_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("balance"),
            15
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![-301_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("depth"),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT action_type,jc_delta FROM dig_actions WHERE id=?1",
                    [action_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .expect("event action"),
            ("event".to_owned(), 5)
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("context cleared"),
            0
        );
    }

    #[test]
    fn test_dig_atomic_paid_cost_rejects_insufficient_funds() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-302_i64, 42_i64, "paid-event-test", 2_i64],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels (discord_id,guild_id,depth)
                 VALUES (?1,?2,0)",
                params![-302_i64, 42_i64],
            )
            .expect("insert tunnel");
        connection
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear setup ledger");
        drop(connection);

        let result = SqliteDigRuntimeStore::new(database.path()).atomic_tunnel_balance_update(
            super::AtomicTunnelBalanceUpdate {
                discord_id: -302,
                guild_id: 42,
                balance_delta: 0,
                balance_cost: 3,
                depth_after: Some(3),
                detail: r#"{"event_id":"crystal_garden","choice":"safe"}"#,
                action_type: "event",
                now: 2_000_000_006,
            },
        );
        assert!(matches!(
            result,
            Err(super::DigRuntimeStoreError::InsufficientFunds)
        ));
        let connection = Connection::open(database.path()).expect("reopen database");
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![-302_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("balance"),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![-302_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("depth"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("no failed settlement ledger row"),
            0
        );
    }

    #[test]
    fn test_miner_respec_records_sink_context_and_action() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let connection = Connection::open(database.path()).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![-401_i64, 42_i64, "respec-test", 100_i64],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,stat_strength,stat_smarts,stat_stamina,stat_points)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![-401_i64, 42_i64, 2_i64, 1_i64, 7_i64, 12_i64],
            )
            .expect("insert miner tunnel");
        connection
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear setup ledger");
        drop(connection);

        let service = DigRuntimeService::sqlite(database.path());
        let result = service.respec(-401, 42, 2_000_000_003).expect("respec");
        assert!(result.contains("Respec complete"));
        let repeated = service
            .respec(-401, 42, 2_000_000_004)
            .expect("repeat respec");
        assert!(repeated.contains("allocated S points"));

        let connection = Connection::open(database.path()).expect("reopen database");
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![-401_i64, 42_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("balance"),
            50
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT stat_strength,stat_smarts,stat_stamina,stat_points
                       FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![-401_i64, 42_i64],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .expect("miner stats"),
            (0, 0, 0, 22)
        );
        let ledger = connection
            .query_row(
                "SELECT delta,source,actor_id,related_type,related_id,reason,metadata
                   FROM economy_ledger_entries",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .expect("respec ledger row");
        assert_eq!(ledger.0, -50);
        assert_eq!(ledger.1, "dig");
        assert_eq!(ledger.2, Some(-401));
        assert_eq!(ledger.3.as_deref(), Some("miner_respec"));
        assert_eq!(ledger.4.as_deref(), Some("s_points"));
        assert_eq!(ledger.5.as_deref(), Some("dig miner respec debit"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&ledger.6).expect("ledger metadata"),
            serde_json::json!({
                "cost": 50,
                "returned_points": 10,
                "previous_stats": {"strength": 2, "smarts": 1, "stamina": 7},
            })
        );
        let action = connection
            .query_row(
                "SELECT action_type,jc_delta,detail FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 ORDER BY id",
                params![-401_i64, 42_i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("respec action");
        assert_eq!(action.0, "miner_respec");
        assert_eq!(action.1, -50);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&action.2).expect("action detail"),
            serde_json::json!({
                "cost": 50,
                "returned_points": 10,
                "previous_stats": {"strength": 2, "smarts": 1, "stamina": 7},
            })
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("only first respec was charged"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                    .get::<_, i64>(0))
                .expect("only first respec was audited"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("context cleared"),
            0
        );
    }

    #[test]
    fn migrated_sqlite_defense_and_weather_actions_use_app_boundary() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(9_001, "defense-test", Some(42)))
            .expect("insert player");
        let service = DigRuntimeService::sqlite(database.path());
        service
            .dig(DigRuntimeRequest {
                discord_id: 9_001,
                guild_id: 42,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("first dig");

        assert!(matches!(
            service.set_trap(9_001, 42, "2023-11-14"),
            Ok(SetTrapOutcome::Set {
                cost: 0,
                balance_after: _
            })
        ));
        assert!(matches!(
            service.buy_insurance(9_001, 42, 1_700_000_000),
            Ok(BuyInsuranceOutcome::Purchased {
                cost: 5,
                expires_at: 1_700_086_400,
                balance_after: _
            })
        ));
        let weather = service
            .weather(42, "2023-11-14", 1_700_000_000)
            .expect("weather through app service");
        assert_eq!(weather.len(), 2);
        assert!(weather.iter().all(|entry| entry.definition().is_some()));
    }

    #[test]
    fn first_dig_defers_weather_and_second_dig_returns_stable_typed_effects() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(9_005, "weather-test", Some(42)))
            .expect("insert player");
        let service = DigRuntimeService::sqlite(database.path());
        service
            .dig(DigRuntimeRequest {
                discord_id: 9_005,
                guild_id: 42,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("first dig");
        let connection = Connection::open(database.path()).expect("open weather database");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| row
                    .get::<_, i64>(0))
                .expect("weather row count"),
            0
        );
        connection
            .execute(
                "UPDATE tunnels SET last_dig_at=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![1_699_990_000_i64, 9_005_i64, 42_i64],
            )
            .expect("clear cooldown");
        drop(connection);

        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: 9_005,
                guild_id: 42,
                now: 1_700_003_601,
                paid: false,
                forced_event: false,
            })
            .expect("second dig");
        assert!(outcome.success);
        let first = service
            .weather_projection(42, "2023-11-14", 1_700_003_601)
            .expect("typed weather projection");
        let second = service
            .weather_projection(42, "2023-11-14", 1_700_003_999)
            .expect("stable weather projection");
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert!(first.iter().any(|weather| weather.layer == "Dirt"));
        assert!(first.iter().all(|weather| {
            !weather.name.is_empty()
                && !weather.description.is_empty()
                && weather.effects != super::DigWeatherEffects::neutral()
        }));
    }

    #[test]
    fn sqlite_pet_work_first_dig_is_settled_and_claimed_atomically() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "pet-dig", Some(9)))
            .expect("insert player");
        let connection = Connection::open(database.path()).expect("open database");
        let pet_id = seed_runtime_pet(&connection, 1_800_000_000, 0);
        drop(connection);

        let service = DigRuntimeService::with_config(
            SqliteDigRuntimeStore::new(database.path()),
            super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
        );
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_800_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("first pet-assisted dig");
        assert!(outcome.success);
        assert_eq!(outcome.pet_dig_bonus, 12);
        assert_eq!(outcome.pet_name.as_deref(), Some("Blep"));

        let connection = Connection::open(database.path()).expect("reopen database");
        assert_eq!(
            connection
                .query_row(
                    "SELECT dig_work_units,dig_work_at FROM pets WHERE pet_id=?1",
                    [pet_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .expect("settled pet work"),
            (10 * PET_DAY + PET_DAY / 5, 1_800_000_000)
        );
    }

    #[test]
    fn sqlite_pet_work_cave_in_does_not_consume_the_offer() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "pet-cave", Some(9)))
            .expect("insert player");
        let connection = Connection::open(database.path()).expect("open database");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,prestige_perks,boss_progress,boss_attempts
                 ) VALUES (7,9,'Pet Mine',10,10,1,0,'[]','{}','{}')",
                [],
            )
            .expect("seed tunnel");
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',?1,?1,0,?1,100,?2,?1)",
                params![1_800_000_000_i64, 12 * PET_DAY],
            )
            .expect("seed pet");
        drop(connection);

        let now = (1_800_000_000_i64..1_800_010_000)
            .find(|candidate| {
                super::SeededLootEntropy::new(super::seed_for(DigRuntimeRequest {
                    discord_id: 7,
                    guild_id: 9,
                    now: *candidate,
                    paid: false,
                    forced_event: false,
                }))
                .unit()
                    < 0.05
            })
            .expect("deterministic cave-in seed");
        let service = DigRuntimeService::with_config(
            SqliteDigRuntimeStore::new(database.path()),
            super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
        );
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("cave-in dig");
        assert!(outcome.cave_in);
        assert_eq!(outcome.pet_dig_bonus, 0);
        let connection = Connection::open(database.path()).expect("reopen database");
        assert_eq!(
            connection
                .query_row("SELECT dig_work_units FROM pets", [], |row| row
                    .get::<_, i64>(0))
                .expect("pet work"),
            12 * PET_DAY
        );
    }

    #[test]
    fn sqlite_pet_work_conflict_rolls_back_the_whole_commit() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "pet-conflict", Some(9)))
            .expect("insert player");
        let connection = Connection::open(database.path()).expect("open database");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,prestige_perks,boss_progress,boss_attempts
                 ) VALUES (7,9,'Pet Mine',10,10,1,0,'[]','{}','{}')",
                [],
            )
            .expect("seed tunnel");
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',1,1,0,1,100,?1,1)",
                [12 * PET_DAY],
            )
            .expect("seed pet");
        let pet_id = connection.last_insert_rowid();
        drop(connection);

        let store = SqliteDigRuntimeStore::new(database.path());
        let snapshot = store.snapshot(7, 9).expect("snapshot");
        let mut next = snapshot.clone();
        next.tunnel.as_mut().expect("tunnel").depth = 20;
        let error = store
            .commit(super::DigRuntimeCommit {
                expected: super::DigRuntimeVersion::from(&snapshot),
                next,
                delivery_draft: None,
                consumed_item_ids: Vec::new(),
                pet_work_claim: Some(cama_domain::pet::PetDigWorkClaim {
                    pet_id,
                    expected_units: 13 * PET_DAY,
                    expected_at: 1,
                    new_units: 0,
                    new_at: 1,
                }),
                depth_before: 10,
                depth_after: 20,
                jc_delta: 0,
                balance_cost: 0,
                action_type: "dig".to_owned(),
                detail: "{}".to_owned(),
                now: 1_800_000_000,
            })
            .expect_err("stale pet claim");
        assert!(matches!(
            error,
            super::DigRuntimeStoreError::PetWorkConflict
        ));
        let connection = Connection::open(database.path()).expect("reopen database");
        assert_eq!(
            connection
                .query_row("SELECT depth FROM tunnels", [], |row| row.get::<_, i64>(0))
                .expect("depth"),
            10
        );
    }

    #[test]
    fn test_drain_at_cap_is_double() {
        let depth = super::PRESTIGE_HARD_CAP - 1;
        let total =
            super::layer_at(depth).luminosity_drain + super::deep_luminosity_drain_bonus(depth);
        assert_eq!(super::deep_luminosity_drain_bonus(depth), 7);
        assert_eq!(total, 17);
    }

    #[test]
    fn test_drain_at_start_depth_matches_base() {
        let depth = super::LUMINOSITY_DEEP_DRAIN_START_DEPTH;
        assert_eq!(super::deep_luminosity_drain_bonus(depth), 0);
        assert_eq!(
            super::layer_at(depth).luminosity_drain,
            10,
            "The Hollow's authored base drain"
        );
    }

    #[test]
    fn test_drain_below_start_depth_unchanged() {
        let depth = super::LUMINOSITY_DEEP_DRAIN_START_DEPTH - 100;
        assert_eq!(super::deep_luminosity_drain_bonus(depth), 0);
        assert_eq!(super::layer_at(depth).luminosity_drain, 7);
    }

    #[test]
    fn test_drain_increases_monotonically() {
        let drains = [350_i64, 400, 450]
            .into_iter()
            .map(|depth| {
                super::layer_at(depth).luminosity_drain + super::deep_luminosity_drain_bonus(depth)
            })
            .collect::<Vec<_>>();
        assert!(
            drains.windows(2).all(|pair| pair[0] < pair[1]),
            "{drains:?}"
        );
    }

    fn seed_cap_tunnel(database: &NamedTempFile, depth: i64, luminosity: i64) {
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(50_001, "cap-test", Some(50_002)))
            .expect("seed player");
        let connection = Connection::open(database.path()).expect("cap database");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=777
                 WHERE discord_id=50001 AND guild_id=50002",
                [],
            )
            .expect("seed balance");
        connection
            .execute(
                "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(50001,50002,'Cap Tunnel',?1,?1,1,0,0,?2,1,'[]','{}','{}')",
                params![depth, luminosity],
            )
            .expect("seed tunnel");
    }

    #[test]
    fn test_dig_at_cap_is_rejected() {
        let database = NamedTempFile::new().expect("cap database");
        seed_cap_tunnel(&database, super::PRESTIGE_HARD_CAP, 77);
        let service = DigRuntimeService::sqlite(database.path());
        let before = Connection::open(database.path()).expect("open cap database");
        let before_balance = before
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=50001 AND guild_id=50002",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance");
        drop(before);

        let result = service
            .dig(DigRuntimeRequest {
                discord_id: 50_001,
                guild_id: 50_002,
                now: 1_900_000_000,
                paid: false,
                forced_event: true,
            })
            .expect("hard cap response");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|message| message.contains("prestige cap"))
        );
        assert_eq!(result.depth_before, super::PRESTIGE_HARD_CAP);
        assert_eq!(result.depth_after, super::PRESTIGE_HARD_CAP);
        assert_eq!(result.jc_earned, 0);

        let connection = Connection::open(database.path()).expect("reopen cap database");
        assert_eq!(
            connection
                .query_row("SELECT jopacoin_balance FROM players", [], |row| row
                    .get::<_, i64>(0))
                .expect("balance unchanged"),
            before_balance
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT depth,luminosity,last_dig_at FROM tunnels",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    }
                )
                .expect("tunnel unchanged"),
            (super::PRESTIGE_HARD_CAP, 77, 0)
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| row
                    .get::<_, i64>(0))
                .expect("weather unchanged"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                    .get::<_, i64>(0))
                .expect("no audit action"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pets", [], |row| row.get::<_, i64>(0))
                .expect("no pet mutation"),
            0
        );
    }

    #[test]
    fn test_dig_one_below_cap_allowed() {
        let database = NamedTempFile::new().expect("cap database");
        seed_cap_tunnel(&database, super::PRESTIGE_HARD_CAP - 1, 100);
        let service = DigRuntimeService::sqlite(database.path());
        let result = service
            .dig(DigRuntimeRequest {
                discord_id: 50_001,
                guild_id: 50_002,
                now: 1_900_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("cap-1 dig response");
        assert!(
            !result
                .error
                .as_deref()
                .is_some_and(|message| message.contains("prestige cap"))
        );
        assert_eq!(
            Connection::open(database.path())
                .expect("reopen cap-1 database")
                .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                    .get::<_, i64>(0))
                .expect("cap-1 action"),
            1
        );
    }

    #[test]
    fn test_helltide_modifier_taxes_dig_yield() {
        fn seed(database: &NamedTempFile) {
            initialize_or_migrate(database.path()).expect("canonical migration");
            PlayerRepository::new(database.path())
                .add(&NewPlayer::new(51_001, "helltide-test", Some(51_002)))
                .expect("seed player");
            let connection = Connection::open(database.path()).expect("helltide database");
            connection
                .execute(
                    "UPDATE players SET jopacoin_balance=777
                     WHERE discord_id=51001 AND guild_id=51002",
                    [],
                )
                .expect("seed balance");
            connection
                .execute(
                    "INSERT INTO tunnels(
                         discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                         total_jc_earned,last_dig_at,luminosity,prestige_level,
                         prestige_perks,boss_progress,boss_attempts
                     ) VALUES(51001,51002,'Helltide Tunnel',10,10,1,0,0,100,0,'[]','{}','{}')",
                    [],
                )
                .expect("seed tunnel");
        }

        let baseline_db = NamedTempFile::new().expect("baseline database");
        seed(&baseline_db);
        let baseline = DigRuntimeService::sqlite(baseline_db.path())
            .dig(DigRuntimeRequest {
                discord_id: 51_001,
                guild_id: 51_002,
                now: 1_910_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("baseline dig");

        let helltide_db = NamedTempFile::new().expect("helltide database");
        seed(&helltide_db);
        DigGuildModifierRepository::new(helltide_db.path())
            .set_modifier_at(
                Some(51_002),
                "helltide_active",
                600,
                &serde_json::json!({"tax_per_dig": 5}),
                1_910_000_000,
            )
            .expect("activate helltide");
        let taxed = DigRuntimeService::sqlite(helltide_db.path())
            .dig(DigRuntimeRequest {
                discord_id: 51_001,
                guild_id: 51_002,
                now: 1_910_000_000,
                paid: false,
                forced_event: false,
            })
            .expect("taxed dig");

        assert!(baseline.success && taxed.success);
        assert!(taxed.jc_earned <= baseline.jc_earned);
        assert!(taxed.balance_after >= 0);
        let baseline_detail = baseline
            .action_id
            .and_then(|action_id| {
                Connection::open(baseline_db.path())
                    .ok()?
                    .query_row(
                        "SELECT detail FROM dig_actions WHERE id=?1",
                        [action_id],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
            })
            .expect("inactive tax detail");
        assert!(baseline_detail.contains("\"helltide_tax\":0"));
        let detail = taxed
            .action_id
            .and_then(|action_id| {
                Connection::open(helltide_db.path())
                    .ok()?
                    .query_row(
                        "SELECT detail FROM dig_actions WHERE id=?1",
                        [action_id],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
            })
            .expect("tax detail");
        assert!(detail.contains("\"helltide_tax\":5"));
    }

    #[test]
    fn test_slow_drip_tracks_gross_cap_but_credits_scaled_reward() {
        const ACTOR: i64 = 52_001;
        const GUILD: i64 = 52_002;
        const NOW: i64 = 1_700_000_000;
        let database = NamedTempFile::new().expect("slow drip database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(ACTOR, "slow-drip-test", Some(GUILD)))
            .expect("seed player");
        let connection = Connection::open(database.path()).expect("slow drip connection");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=100
                 WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
            )
            .expect("seed balance");
        connection
            .execute(
                "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(?1,?2,'Slow Drip Tunnel',10,10,1,0,?3,100,0,'[]','{}','{}')",
                params![ACTOR, GUILD, NOW - 4_000],
            )
            .expect("seed tunnel");
        connection
            .execute(
                "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'slow_drip',?3,1,1)",
                params![ACTOR, GUILD, NOW - 5_000],
            )
            .expect("equip slow drip");
        let claim_date =
            cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("claim date");
        connection
            .execute(
                "INSERT INTO slow_drip_claims(
                     discord_id,guild_id,claim_date,claimed_today,last_claim_at
                 ) VALUES(?1,?2,?3,0,?4)",
                params![ACTOR, GUILD, claim_date, NOW - 1_200],
            )
            .expect("seed slow drip anchor");
        drop(connection);

        EconomyEventRepository::new(database.path())
            .activate_event_atomic(
                Some(GUILD),
                &EventDraft {
                    event_date: cama_domain::game_date::game_date_for_timestamp(NOW as f64)
                        .expect("event date"),
                    name: "Double slow drip".to_owned(),
                    hero: "Earthshaker".to_owned(),
                    direction: EventDirection::Neutral,
                    severity: 1,
                    target_effect_jc: 0,
                    forecast_flow_jc: 0,
                    expected_effect_jc: 0,
                    monetary_stock_before: 0,
                    effects: EventEffects {
                        reward_multiplier: 2.0,
                        ..EventEffects::default()
                    },
                    announcement: "Double slow drip".to_owned(),
                    starts_at: NOW - 60,
                    ends_at: NOW + 60,
                    created_at: NOW - 60,
                },
            )
            .expect("activate slow drip economy event");

        let mut config = DigRuntimeConfig::default();
        config.economy_event.enabled = true;
        let service = DigRuntimeService::sqlite_with_config(database.path(), config);
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                now: NOW,
                paid: false,
                forced_event: false,
            })
            .expect("slow drip Dig");
        assert!(outcome.success);
        let connection = Connection::open(database.path()).expect("reload slow drip");
        let (claimed_today, last_claim_at): (i64, i64) = connection
            .query_row(
                "SELECT claimed_today,last_claim_at FROM slow_drip_claims
                 WHERE discord_id=?1 AND guild_id=?2 AND claim_date=?3",
                params![ACTOR, GUILD, claim_date],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("persisted gross claim");
        assert_eq!((claimed_today, last_claim_at), (10, NOW));
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![ACTOR, GUILD],
                    |row| row.get::<_, i64>(0),
                )
                .expect("slow drip wallet"),
            outcome.balance_after
        );
        assert!(outcome.balance_after >= 113);
        let action_detail: String = connection
            .query_row(
                "SELECT detail FROM dig_actions WHERE id=?1",
                [outcome.action_id.expect("slow drip action")],
                |row| row.get(0),
            )
            .expect("slow drip action detail");
        let action_json: Value = serde_json::from_str(&action_detail).expect("action JSON");
        assert_eq!(action_json["slow_drip"]["gross_jc"], 10);
        assert_eq!(action_json["slow_drip"]["credit_jc"], 13);
        let metadata: String = connection
            .query_row(
                "SELECT metadata FROM economy_ledger_entries
                 WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                   AND related_type='slow_drip'
                 ORDER BY ledger_id DESC LIMIT 1",
                params![GUILD, ACTOR],
                |row| row.get(0),
            )
            .expect("slow drip ledger context");
        let metadata_json: Value = serde_json::from_str(&metadata).expect("ledger metadata JSON");
        assert_eq!(metadata_json["gross_jc"], 10);
        assert_eq!(metadata_json["credit_jc"], 13);

        // A second request cannot backfill or double-claim the same anchor;
        // the first dig's cooldown blocks it before any wallet/claim write.
        let before_retry = connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                   AND related_type='slow_drip'",
                params![GUILD, ACTOR],
                |row| row.get::<_, i64>(0),
            )
            .expect("ledger count before retry");
        drop(connection);
        let retry = service
            .dig(DigRuntimeRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                now: NOW,
                paid: false,
                forced_event: false,
            })
            .expect("slow drip retry");
        assert!(!retry.success);
        assert_eq!(
            Connection::open(database.path())
                .expect("reload retry")
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                       AND related_type='slow_drip'",
                    params![GUILD, ACTOR],
                    |row| row.get::<_, i64>(0),
                )
                .expect("ledger count after retry"),
            before_retry
        );
    }

    #[test]
    fn test_parked_boss_reopens_after_slow_drip_before_cooldown() {
        const ACTOR: i64 = 52_101;
        const GUILD: i64 = 52_102;
        const LAST_DIG: i64 = 1_700_000_000;
        const NOW: i64 = LAST_DIG + 10;
        let database = NamedTempFile::new().expect("parked boss database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(ACTOR, "parked-boss", Some(GUILD)))
            .expect("seed player");
        let connection = Connection::open(database.path()).expect("parked boss connection");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=100
                 WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
            )
            .expect("seed balance");
        connection
            .execute(
                "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(?1,?2,'Parked Boss',24,24,1,0,?3,100,0,'[]',?4,'{}')",
                params![ACTOR, GUILD, LAST_DIG, r#"{"25":"active"}"#],
            )
            .expect("seed parked tunnel");
        connection
            .execute(
                "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'slow_drip',?3,1,1)",
                params![ACTOR, GUILD, LAST_DIG - 100],
            )
            .expect("equip slow drip");
        let claim_date =
            cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("claim date");
        connection
            .execute(
                "INSERT INTO slow_drip_claims(
                     discord_id,guild_id,claim_date,claimed_today,last_claim_at
                 ) VALUES(?1,?2,?3,0,?4)",
                params![ACTOR, GUILD, claim_date, LAST_DIG - 600],
            )
            .expect("seed slow drip");
        drop(connection);

        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                now: NOW,
                paid: true,
                forced_event: false,
            })
            .expect("parked boss reopen");
        assert!(outcome.success);
        assert_eq!(outcome.boss_boundary, Some(25));
        assert_eq!(outcome.advance, 0);
        assert_eq!(outcome.jc_earned, 0);
        assert_eq!(outcome.balance_after, 103);
        let connection = Connection::open(database.path()).expect("reload parked boss");
        assert_eq!(
            connection
                .query_row("SELECT claimed_today FROM slow_drip_claims", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .expect("gross claim"),
            5
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                    .get::<_, i64>(0))
                .expect("no parked action"),
            0
        );
    }

    fn cave_gear(durability: i64) -> super::DigRuntimeGear {
        super::DigRuntimeGear {
            id: 1,
            slot: "armor".to_owned(),
            tier: 1,
            durability,
            equipped: true,
            acquired_at: 1,
            source: "test".to_owned(),
            item_id: None,
        }
    }

    #[test]
    fn test_broken_gear_is_not_an_applicable_gear_nick_target() {
        let mut gear = vec![cave_gear(0)];
        assert!(super::apply_cave_in_gear_ticks(&mut gear, 1).is_empty());
        assert_eq!(gear[0].durability, 0);
    }

    #[test]
    fn test_gear_nick_reports_newly_broken_piece() {
        let mut gear = vec![cave_gear(1)];
        let broken = super::apply_cave_in_gear_ticks(&mut gear, 1);
        assert_eq!(broken, vec![cama_domain::dig_gear::ARMOR_TIERS[1].name]);
        assert_eq!(gear[0].durability, 0);
    }

    #[test]
    fn test_catastrophic_cave_in_reports_newly_broken_piece() {
        let mut gear = vec![cave_gear(3)];
        let broken = super::apply_cave_in_gear_ticks(&mut gear, 3);
        assert_eq!(broken, vec![cama_domain::dig_gear::ARMOR_TIERS[1].name]);
        assert_eq!(gear[0].durability, 0);
    }

    #[test]
    fn test_force_cave_in_at_deep() {
        assert_eq!(
            cama_domain::dig_cave_in::cave_in_band(180),
            cama_domain::dig_cave_in::CaveInBand::Deep
        );
        let (minimum, maximum) = cama_domain::dig_cave_in::CAVE_IN_BLOCK_LOSS_RANGES
            [cama_domain::dig_cave_in::CaveInBand::Deep];
        assert_eq!((minimum, maximum), (12, 25));
    }

    #[test]
    fn test_catastrophic_overrides_to_milestone() {
        let (depth_after, insurance_saved, block_loss) =
            super::catastrophic_cave_in_depth(240, 12, None, false);
        assert_eq!(depth_after, 225);
        assert!(!insurance_saved);
        assert_eq!(block_loss, 15);
    }

    #[test]
    fn test_insurance_protects_catastrophic_depth() {
        let (depth_after, insurance_saved, block_loss) =
            super::catastrophic_cave_in_depth(240, 12, None, true);
        assert_eq!(depth_after, 228);
        assert!(insurance_saved);
        assert_eq!(block_loss, 12);
    }

    fn seed_live_runtime_tunnel(
        database: &NamedTempFile,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        depth: i64,
        total_digs: i64,
        last_dig_at: Option<i64>,
    ) {
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(discord_id, "batch-one", Some(guild_id)))
            .expect("seed player");
        let connection = Connection::open(database.path()).expect("open database");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,luminosity,last_lum_update_at,prestige_perks,
                     boss_progress,boss_attempts
                 ) VALUES (?1,?2,'Batch One',?3,?3,?4,?5,100,?6,'[]','{}','{}')",
                params![discord_id, guild_id, depth, total_digs, last_dig_at, now],
            )
            .expect("seed tunnel");
    }

    fn find_non_cave_dig_time(discord_id: i64, guild_id: i64, start: i64) -> i64 {
        (start..start.saturating_add(10_000))
            .find(|candidate| {
                super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                    discord_id,
                    guild_id,
                    now: *candidate,
                    paid: false,
                    forced_event: false,
                }))
                .unit()
                    > 0.30
            })
            .expect("deterministic non-cave seed")
    }

    fn find_cave_dig_time(discord_id: i64, guild_id: i64, start: i64) -> i64 {
        (start..start.saturating_add(10_000))
            .find(|candidate| {
                super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                    discord_id,
                    guild_id,
                    now: *candidate,
                    paid: false,
                    forced_event: false,
                }))
                .unit()
                    < 0.05
            })
            .expect("deterministic cave seed")
    }

    fn find_dig_time_with_unit_between(
        discord_id: i64,
        guild_id: i64,
        start: i64,
        minimum: f64,
        maximum: f64,
    ) -> i64 {
        (start..start.saturating_add(10_000))
            .find(|candidate| {
                let unit =
                    super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                        discord_id,
                        guild_id,
                        now: *candidate,
                        paid: false,
                        forced_event: false,
                    }))
                    .unit();
                unit >= minimum && unit < maximum
            })
            .expect("deterministic cave probability seed")
    }

    #[test]
    fn sqlite_cave_in_does_not_persist_an_artifact() {
        let database = NamedTempFile::new().expect("cave artifact database");
        let actor = 60_101;
        let guild = 60_102;
        let now = 1_900_100_000;
        seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
        let dig_now = find_cave_dig_time(actor, guild, now);
        let service = DigRuntimeService::sqlite(database.path());
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("cave-in Dig");
        assert!(outcome.success && outcome.cave_in);
        assert_eq!(outcome.artifact_id, None);
        let snapshot = SqliteDigRuntimeStore::new(database.path())
            .snapshot(actor, guild)
            .expect("reload cave artifact database");
        assert_eq!(snapshot.artifacts.len(), 0);
        assert_eq!(
            snapshot.tunnel.expect("cave tunnel").current_run_artifacts,
            0
        );
    }

    #[test]
    fn sqlite_successful_artifact_persists_and_increments_current_run_artifacts_once() {
        let database = NamedTempFile::new().expect("successful artifact database");
        let actor = 61_001;
        let guild = 61_002;
        let seed_now = 1_900_100_000;
        seed_live_runtime_tunnel(
            &database,
            actor,
            guild,
            seed_now,
            55,
            1,
            Some(seed_now - 7_200),
        );
        let today = cama_domain::game_date::game_date_for_timestamp(seed_now as f64)
            .expect("artifact game date");
        let connection = Connection::open(database.path()).expect("artifact connection");
        connection
            .execute(
                "UPDATE tunnels SET route_state=?1,mutations=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    r#"{"end_depth":75,"layer":"Crystal","offered":["glass_labyrinth","prismatic_fault","resonant_gallery"],"selected":"glass_labyrinth","start_depth":50}"#,
                    r#"[{"id":"treasure_sense"}]"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed artifact route and mutation");
        connection
            .execute(
                "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'echo_stone',?3,1,1)",
                params![actor, guild, seed_now - 100],
            )
            .expect("seed echo stone");
        connection
            .execute(
                "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES (?1,?2,'Crystal','fossil_rush'),(?1,?2,'Dirt','earthworm_migration')",
                params![guild, today],
            )
            .expect("seed artifact weather");
        drop(connection);

        // The fixed request seed has a non-cave roll and a find roll inside
        // the composed 0.5% * (route * weather * Echo Stone * Treasure Sense)
        // rate. The artifact policy then consumes rarity and candidate entropy.
        let dig_now = seed_now + 312;
        let service = DigRuntimeService::sqlite(database.path());
        let outcome = service
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("successful artifact Dig");
        let artifact_id = outcome.artifact_id.expect("artifact drop");
        let retry = service
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("artifact retry response");
        assert!(
            !retry.success,
            "cooldown must prevent a duplicate artifact roll"
        );
        let snapshot = SqliteDigRuntimeStore::new(database.path())
            .snapshot(actor, guild)
            .expect("reload successful artifact database");
        assert_eq!(
            snapshot
                .artifacts
                .iter()
                .filter(|artifact| artifact.artifact_id == artifact_id)
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .tunnel
                .expect("successful artifact tunnel")
                .current_run_artifacts,
            1
        );
    }

    #[test]
    fn sqlite_artifact_commit_conflict_persists_neither_artifact_nor_counter() {
        let database = NamedTempFile::new().expect("artifact rollback database");
        let actor = 61_101;
        let guild = 61_102;
        let now = 1_900_110_000;
        seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
        let store = SqliteDigRuntimeStore::new(database.path());
        let snapshot = store
            .snapshot(actor, guild)
            .expect("artifact rollback snapshot");
        let mut next = snapshot.clone();
        next.artifacts.push(super::DigRuntimeArtifact {
            id: 1,
            artifact_id: "mole_claws".to_owned(),
            is_relic: true,
            equipped: false,
        });
        next.tunnel
            .as_mut()
            .expect("artifact rollback tunnel")
            .current_run_artifacts = 1;
        let connection = Connection::open(database.path()).expect("artifact conflict connection");
        connection
            .execute(
                "UPDATE tunnels SET depth=depth+1 WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("force artifact conflict");
        drop(connection);
        assert!(matches!(
            store.commit(super::DigRuntimeCommit {
                expected: super::DigRuntimeVersion::from(&snapshot),
                next,
                delivery_draft: None,
                consumed_item_ids: Vec::new(),
                pet_work_claim: None,
                depth_before: 10,
                depth_after: 11,
                jc_delta: 0,
                balance_cost: 0,
                action_type: "dig".to_owned(),
                detail: "{}".to_owned(),
                now,
            }),
            Err(super::DigRuntimeStoreError::Conflict)
        ));
        let connection = Connection::open(database.path()).expect("reload artifact rollback");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("artifact count after rollback"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT current_run_artifacts FROM tunnels", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("artifact counter after rollback"),
            0
        );
    }

    #[test]
    fn sqlite_luminosity_refill_updates_timestamp_and_applies_torch_after_drain() {
        let database = NamedTempFile::new().expect("luminosity database");
        let discord_id = 60_001;
        let guild_id = 60_002;
        let now = 1_900_000_000;
        seed_live_runtime_tunnel(
            &database,
            discord_id,
            guild_id,
            now,
            10,
            1,
            Some(now - 7_200),
        );
        let connection = Connection::open(database.path()).expect("open luminosity database");
        connection
            .execute(
                "UPDATE tunnels SET luminosity=10,last_lum_update_at=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![now - 86_400, discord_id, guild_id],
            )
            .expect("seed stale luminosity anchor");
        connection
            .execute(
                "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'torch',1,?3)",
                params![discord_id, guild_id, now],
            )
            .expect("queue torch");
        drop(connection);

        let dig_now = find_non_cave_dig_time(discord_id, guild_id, now);
        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id,
                guild_id,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("live luminosity Dig");
        assert!(outcome.success);
        let connection = Connection::open(database.path()).expect("reload luminosity database");
        let (luminosity, last_update) = connection
            .query_row(
                "SELECT luminosity,last_lum_update_at FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("persisted luminosity");
        assert_eq!(last_update, dig_now);
        // 10 + one day's 20-point refill, less the Dirt drain, then +50 Torch.
        assert!(
            luminosity >= 60,
            "torch must be applied after the drain: {luminosity}"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type='torch'",
                    params![discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("torch consumption"),
            0
        );
    }

    #[test]
    fn sqlite_queued_charges_stack_and_boss_prep_items_are_not_consumed() {
        let database = NamedTempFile::new().expect("queued-item database");
        let discord_id = 60_003;
        let guild_id = 60_004;
        let now = 1_900_010_000;
        seed_live_runtime_tunnel(
            &database,
            discord_id,
            guild_id,
            now,
            10,
            1,
            Some(now - 7_200),
        );
        let connection = Connection::open(database.path()).expect("open queued-item database");
        connection
            .execute(
                "UPDATE tunnels SET hard_hat_charges=2,grappling_hook_charges=4,
                 void_bait_digs=2 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
            )
            .expect("seed existing charges");
        for item_type in [
            "hard_hat",
            "grappling_hook",
            "void_bait",
            "reinforcement",
            "tempered_whetstone",
        ] {
            connection
                .execute(
                    "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                     VALUES (?1,?2,?3,1,?4)",
                    params![discord_id, guild_id, item_type, now],
                )
                .expect("queue consumable");
        }
        drop(connection);

        let dig_now = find_non_cave_dig_time(discord_id, guild_id, now);
        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id,
                guild_id,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("live queued-item Dig");
        assert!(outcome.success);
        let connection = Connection::open(database.path()).expect("reload queued-item database");
        let charges = connection
            .query_row(
                "SELECT hard_hat_charges,grappling_hook_charges,void_bait_digs,
                        reinforced_until
                 FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .expect("persisted queued charges");
        // The queued Hard Hat grants three charges, then the imminent Dig
        // consumes one unconditionally while bypassing the cave RNG.
        assert_eq!(charges.0, 4);
        assert_eq!(charges.1, 9);
        assert!(charges.2 >= 4);
        assert!(charges.3 > dig_now);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type='tempered_whetstone'",
                    params![discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("boss-prep inventory"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type IN
                       ('hard_hat','grappling_hook','void_bait','reinforcement')",
                    params![discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("applied inventory consumed"),
            0
        );
    }

    #[test]
    fn sqlite_cave_probability_uses_luminosity_perk_buff_stats_and_mana_modifiers() {
        let control_db = NamedTempFile::new().expect("control cave database");
        let modified_db = NamedTempFile::new().expect("modified cave database");
        let actor = 60_021;
        let guild = 60_022;
        let now = 1_900_015_000;
        seed_live_runtime_tunnel(&control_db, actor, guild, now, 10, 1, Some(now - 7_200));
        seed_live_runtime_tunnel(&modified_db, actor, guild, now, 10, 1, Some(now - 7_200));
        let today = cama_domain::game_date::game_date_for_timestamp(now as f64)
            .expect("cave probability date");
        for database in [&control_db, &modified_db] {
            let connection = Connection::open(database.path()).expect("seed cave weather");
            for (layer, weather) in [("Dirt", "earthworm_migration"), ("Stone", "fossil_rush")] {
                connection
                    .execute(
                        "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                         VALUES (?1,?2,?3,?4)",
                        params![guild, today, layer, weather],
                    )
                    .expect("seed neutral cave weather");
            }
        }
        let connection = Connection::open(modified_db.path()).expect("modified cave connection");
        connection
            .execute(
                "UPDATE tunnels SET luminosity=25,prestige_perks=?1,stat_smarts=1,
                 temp_buffs=?2 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    r#"["dark_adaptation"]"#,
                    r#"{"digs_remaining":1,"effect":{"cave_in_reduction":0.01}}"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed cave modifiers");
        connection
            .execute(
                "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'crystal_compass',?3,1,1)",
                params![actor, guild, now - 100],
            )
            .expect("seed crystal compass");
        connection
            .execute(
                "INSERT INTO player_mana(
                     discord_id,guild_id,current_land,assigned_date,consumed_today
                 ) VALUES(?1,?2,'Swamp',?3,0)",
                params![actor, guild, today],
            )
            .expect("seed mana hazard");
        drop(connection);

        // The same request seed is safely outside the P0 control chance
        // (~4.5%) but inside the dark, buff/stat/mana-adjusted chance.
        let dig_now = find_dig_time_with_unit_between(actor, guild, now, 0.10, 0.18);
        let control = DigRuntimeService::sqlite(control_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("control cave probability dig");
        let modified = DigRuntimeService::sqlite(modified_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("modified cave probability dig");
        assert!(
            !control.cave_in,
            "control should remain below the cave roll"
        );
        assert!(
            modified.cave_in,
            "live policy modifiers should raise dark cave risk"
        );
        assert_eq!(modified.depth_before, 10);
        assert!(modified.depth_after < modified.depth_before);
        assert_eq!(
            Connection::open(modified_db.path())
                .expect("reload modified cave database")
                .query_row(
                    "SELECT temp_buffs FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                    params![actor, guild],
                    |row| row.get::<_, Option<String>>(0),
                )
                .expect("expired cave buff"),
            None,
            "the active hazard buff is consumed by the same committed Dig",
        );
    }

    #[test]
    fn sqlite_queued_grapple_and_hard_hat_are_visible_to_cave_consequences() {
        let hard_hat_db = NamedTempFile::new().expect("hard-hat cave database");
        let grapple_db = NamedTempFile::new().expect("grapple cave database");
        let hard_hat_actor = 60_023;
        let grapple_actor = 60_024;
        let guild = 60_025;
        let now = 1_900_016_000;
        seed_live_runtime_tunnel(
            &hard_hat_db,
            hard_hat_actor,
            guild,
            now,
            10,
            1,
            Some(now - 7_200),
        );
        seed_live_runtime_tunnel(
            &grapple_db,
            grapple_actor,
            guild,
            now,
            180,
            1,
            Some(now - 7_200),
        );
        let connection = Connection::open(hard_hat_db.path()).expect("hard-hat connection");
        connection
            .execute(
                "UPDATE tunnels SET luminosity=40 WHERE discord_id=?1 AND guild_id=?2",
                params![hard_hat_actor, guild],
            )
            .expect("seed hard-hat luminosity");
        for item_type in ["hard_hat", "torch"] {
            connection
                .execute(
                    "INSERT INTO dig_inventory(
                         discord_id,guild_id,item_type,queued,created_at
                     ) VALUES(?1,?2,?3,1,?4)",
                    params![hard_hat_actor, guild, item_type, now],
                )
                .expect("queue hard-hat item");
        }
        drop(connection);
        let connection = Connection::open(grapple_db.path()).expect("grapple connection");
        connection
            .execute(
                "INSERT INTO dig_inventory(
                     discord_id,guild_id,item_type,queued,created_at
                 ) VALUES(?1,?2,'grappling_hook',1,?3)",
                params![grapple_actor, guild, now],
            )
            .expect("queue grapple");
        drop(connection);

        let hard_hat_now = find_non_cave_dig_time(hard_hat_actor, guild, now);
        let hard_hat = DigRuntimeService::sqlite(hard_hat_db.path())
            .dig(DigRuntimeRequest {
                discord_id: hard_hat_actor,
                guild_id: guild,
                now: hard_hat_now,
                paid: false,
                forced_event: false,
            })
            .expect("hard-hat dig");
        assert!(hard_hat.success);
        assert!(!hard_hat.cave_in);
        let hard_hat_snapshot = SqliteDigRuntimeStore::new(hard_hat_db.path())
            .snapshot(hard_hat_actor, guild)
            .expect("hard-hat snapshot");
        let hard_hat_tunnel = hard_hat_snapshot.tunnel.expect("hard-hat tunnel");
        assert_eq!(hard_hat_tunnel.hard_hat_charges, 2);
        // Dirt has no ordinary luminosity drain: 40 + 50 Torch - 10 Hard Hat.
        // This catches applying the protection cost before the Torch hook.
        assert_eq!(hard_hat_tunnel.luminosity, 80);

        let grapple_now = find_cave_dig_time(grapple_actor, guild, now);
        let grapple = DigRuntimeService::sqlite(grapple_db.path())
            .dig(DigRuntimeRequest {
                discord_id: grapple_actor,
                guild_id: guild,
                now: grapple_now,
                paid: false,
                forced_event: false,
            })
            .expect("grapple dig");
        assert!(grapple.success && grapple.cave_in);
        assert_eq!(grapple.depth_after, grapple.depth_before);
        let detail =
            serde_json::from_str::<Value>(&grapple.cave_in_detail.expect("cushioned cave detail"))
                .expect("cushioned cave JSON");
        assert_eq!(detail["type"], "cushioned");
        assert_eq!(detail["block_loss"], 0);
        let grapple_tunnel = SqliteDigRuntimeStore::new(grapple_db.path())
            .snapshot(grapple_actor, guild)
            .expect("grapple snapshot")
            .tunnel
            .expect("grapple tunnel");
        assert_eq!(grapple_tunnel.grappling_hook_charges, 4);
    }

    #[test]
    fn sqlite_blocked_cooldown_does_not_initialize_weather() {
        let database = NamedTempFile::new().expect("cooldown database");
        let discord_id = 60_005;
        let guild_id = 60_006;
        let now = 1_900_020_000;
        seed_live_runtime_tunnel(&database, discord_id, guild_id, now, 10, 1, Some(now - 1));
        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id,
                guild_id,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("blocked cooldown response");
        assert!(!outcome.success);
        assert!(outcome.cooldown_remaining > 0);
        let connection = Connection::open(database.path()).expect("reload cooldown database");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("weather row count"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("action row count"),
            0
        );
    }

    #[test]
    fn sqlite_first_dig_requires_unstarted_tunnel_and_persists_streak() {
        let database = NamedTempFile::new().expect("first-dig database");
        let discord_id = 60_007;
        let guild_id = 60_008;
        let now = 1_900_030_000;
        seed_live_runtime_tunnel(&database, discord_id, guild_id, now, 0, 0, Some(now - 1));
        let service = DigRuntimeService::sqlite(database.path());
        let partial = service
            .dig(DigRuntimeRequest {
                discord_id,
                guild_id,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("partial tunnel admission");
        assert!(!partial.success);
        assert!(!partial.first_dig);
        let connection = Connection::open(database.path()).expect("reopen first-dig database");
        connection
            .execute(
                "UPDATE tunnels SET last_dig_at=NULL WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
            )
            .expect("restore unstarted timestamp");
        drop(connection);

        let first = service
            .dig(DigRuntimeRequest {
                discord_id,
                guild_id,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("unstarted first Dig");
        assert!(first.success);
        assert!(first.first_dig);
        let today = super::game_date_for_timestamp(now as f64).expect("game date");
        let connection = Connection::open(database.path()).expect("reload first-dig database");
        let streak = connection
            .query_row(
                "SELECT total_digs,streak_days,streak_last_date
                 FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("first-dig streak");
        assert_eq!(streak, (1, 1, today));
    }

    #[test]
    fn sqlite_reduced_injury_ticks_and_slower_cooldown_does_not_halve_advance() {
        let reduced_db = NamedTempFile::new().expect("reduced injury database");
        let normal_db = NamedTempFile::new().expect("normal injury database");
        let actor = 60_009;
        let guild = 60_010;
        let now = 1_900_040_000;
        seed_live_runtime_tunnel(&reduced_db, actor, guild, now, 10, 1, Some(now - 7_200));
        seed_live_runtime_tunnel(&normal_db, actor, guild, now, 10, 1, Some(now - 7_200));
        let reduced_connection = Connection::open(reduced_db.path()).expect("reduced connection");
        reduced_connection
            .execute(
                "UPDATE tunnels SET injury_state=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    r#"{"type":"reduced_advance","digs_remaining":1}"#,
                    actor,
                    guild
                ],
            )
            .expect("seed reduced injury");
        drop(reduced_connection);
        let dig_now = find_non_cave_dig_time(actor, guild, now);
        let reduced = DigRuntimeService::sqlite(reduced_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("reduced injury Dig");
        let normal = DigRuntimeService::sqlite(normal_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("normal Dig");
        assert!(reduced.success && normal.success);
        assert!(reduced.advance <= normal.advance);
        let reduced_connection = Connection::open(reduced_db.path()).expect("reload reduced");
        assert_eq!(
            reduced_connection
                .query_row("SELECT injury_state FROM tunnels", [], |row| {
                    row.get::<_, Option<String>>(0)
                })
                .expect("injury state"),
            None
        );
        drop(reduced_connection);

        let slower_db = NamedTempFile::new().expect("slower injury database");
        let slower_control_db = NamedTempFile::new().expect("slower injury control database");
        seed_live_runtime_tunnel(&slower_db, actor, guild, now, 10, 1, Some(now - 3_600));
        seed_live_runtime_tunnel(
            &slower_control_db,
            actor,
            guild,
            now,
            10,
            1,
            Some(now - 6 * 3_600),
        );
        let game_date = super::game_date_for_timestamp(now as f64).expect("injury game date");
        for database in [&slower_db, &slower_control_db] {
            let connection = Connection::open(database.path()).expect("weather connection");
            for (layer, weather) in [("Dirt", "earthworm_migration"), ("Stone", "fossil_rush")] {
                connection
                    .execute(
                        "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                         VALUES (?1,?2,?3,?4)",
                        params![guild, game_date, layer, weather],
                    )
                    .expect("seed deterministic weather");
            }
        }
        let slower_connection = Connection::open(slower_db.path()).expect("slower connection");
        slower_connection
            .execute(
                "UPDATE tunnels SET injury_state=?1,temp_curses=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    r#"{"type":"slower_cooldown","digs_remaining":1}"#,
                    r#"{"digs_remaining":1,"effect":{"cooldown_penalty":1.0}}"#,
                    actor,
                    guild
                ],
            )
            .expect("seed slower injury");
        drop(slower_connection);
        let blocked = DigRuntimeService::sqlite(slower_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("slower injury cooldown");
        assert!(!blocked.success);
        assert_eq!(blocked.cooldown_remaining, 13 * 30 * 60);
        let slower_connection = Connection::open(slower_db.path()).expect("reopen slower");
        slower_connection
            .execute(
                "UPDATE tunnels SET last_dig_at=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![now - (15 * 3_600 / 2), actor, guild],
            )
            .expect("clear slower cooldown");
        drop(slower_connection);
        let admitted = DigRuntimeService::sqlite(slower_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("admit slower injury");
        let control = DigRuntimeService::sqlite(slower_control_db.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now,
                paid: false,
                forced_event: false,
            })
            .expect("admit no-injury control");
        assert!(admitted.success);
        assert!(control.success);
        assert_eq!(admitted.advance, control.advance);
    }

    #[test]
    fn sqlite_first_dig_pet_work_respects_live_boss_boundary_cap() {
        let database = NamedTempFile::new().expect("first pet boundary database");
        let actor = 7;
        let guild = 9;
        let now = 1_900_050_000;
        seed_live_runtime_tunnel(&database, actor, guild, now, 0, 0, None);
        let connection = Connection::open(database.path()).expect("pet boundary connection");
        connection
            .execute(
                "UPDATE tunnels SET boss_progress=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![r#"{"25":"active"}"#, actor, guild],
            )
            .expect("seed first boss boundary");
        seed_runtime_pet(&connection, now, 36 * DIG_WORK_UNITS_PER_BLOCK);
        drop(connection);
        let outcome = DigRuntimeService::with_config(
            SqliteDigRuntimeStore::new(database.path()),
            super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
        )
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("first pet Dig");
        assert!(outcome.success && outcome.first_dig);
        assert!(outcome.depth_after <= 24);
        assert!(outcome.pet_dig_bonus <= 12);
    }

    #[test]
    fn sqlite_queued_reinforcement_does_not_cap_current_cave_loss() {
        let database = NamedTempFile::new().expect("reinforcement database");
        let actor = 60_013;
        let guild = 60_014;
        let now = 1_900_060_000;
        seed_live_runtime_tunnel(&database, actor, guild, now, 180, 1, Some(now - 7_200));
        let connection = Connection::open(database.path()).expect("reinforcement connection");
        connection
            .execute(
                "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'reinforcement',1,?3)",
                params![actor, guild, now],
            )
            .expect("queue reinforcement");
        drop(connection);
        let dig_now = find_cave_dig_time(actor, guild, now);
        let outcome = DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("reinforced cave Dig");
        assert!(outcome.success && outcome.cave_in);
        let detail = outcome.cave_in_detail.expect("cave detail");
        let detail = serde_json::from_str::<Value>(&detail).expect("cave JSON");
        assert!(
            detail
                .get("block_loss")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 8
        );
        let connection = Connection::open(database.path()).expect("reload reinforcement");
        assert!(
            connection
                .query_row("SELECT reinforced_until FROM tunnels", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("reinforcement stamp")
                > dig_now
        );
    }

    #[test]
    fn sqlite_broken_equipped_pickaxe_does_not_reduce_luminosity_drain() {
        let active_db = NamedTempFile::new().expect("active pickaxe database");
        let broken_db = NamedTempFile::new().expect("broken pickaxe database");
        let actor = 60_015;
        let guild = 60_016;
        let now = 1_900_070_000;
        seed_live_runtime_tunnel(&active_db, actor, guild, now, 300, 1, Some(now - 7_200));
        seed_live_runtime_tunnel(&broken_db, actor, guild, now, 300, 1, Some(now - 7_200));
        for (database, durability) in [(&active_db, 20_i64), (&broken_db, 0_i64)] {
            let connection = Connection::open(database.path()).expect("pickaxe connection");
            connection
                .execute(
                    "UPDATE tunnels SET pickaxe_tier=6,luminosity=100
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![actor, guild],
                )
                .expect("seed pickaxe fallback");
            connection
                .execute(
                    "INSERT INTO dig_gear(
                         discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source
                     ) VALUES (?1,?2,'weapon',6,?3,1,?4,'batch-one')",
                    params![actor, guild, durability, now],
                )
                .expect("seed equipped pickaxe");
        }
        let dig_now = find_non_cave_dig_time(actor, guild, now);
        for database in [&active_db, &broken_db] {
            DigRuntimeService::sqlite(database.path())
                .dig(DigRuntimeRequest {
                    discord_id: actor,
                    guild_id: guild,
                    now: dig_now,
                    paid: false,
                    forced_event: false,
                })
                .expect("pickaxe Dig");
        }
        let active_luminosity = DigRuntimeService::sqlite(active_db.path())
            .snapshot(actor, guild)
            .expect("active snapshot")
            .tunnel
            .expect("active tunnel")
            .luminosity;
        let broken_luminosity = DigRuntimeService::sqlite(broken_db.path())
            .snapshot(actor, guild)
            .expect("broken snapshot")
            .tunnel
            .expect("broken tunnel")
            .luminosity;
        assert!(
            active_luminosity > broken_luminosity,
            "broken equipped pickaxe must not receive tier-6 drain reduction"
        );
    }
}

#[cfg(test)]
#[path = "dig_pet_runtime_tests.rs"]
mod dig_pet_runtime_tests;
