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

use cama_db::dig_inventory_repository::{
    BuyInsuranceOutcome, DigInventoryRepository, SetTrapOutcome,
};
use cama_db::dig_weather::{DigWeatherEntry, DigWeatherRepository};
use cama_db::pet_repository::PetRepository;
use cama_domain::dig_gear::{WEAPON_TIERS, unique_gear};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::pet::{
    DIG_WORK_CAP_BLOCKS, DIG_WORK_UNITS_PER_BLOCK, PetDigWork, PetDigWorkClaim,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::dig_loot::{
    CanonicalEventResolution, DigLootModifiers, DigLootService, InventoryItem, LootActionResult,
    LootEntropy, LootRepository, RepositoryError, SeededLootEntropy, TunnelLootState,
};
use crate::dig_relic_rework::{
    RelicEntropy, RelicSet, YieldContext, is_first_dig_of_day, post_pinnacle_decay_factor,
    relic_jc_yield_multiplier, storm_negates_hazard,
};
use crate::dig_routes::{
    RouteChoiceEvaluation, evaluate_route_choice, parse_route_state, route_by_id, route_effect,
    route_status,
};
use crate::dig_service::{
    DigOutcomeInput, MinerAllocation, TunnelState, apply_dig_outcome, apply_first_dig,
    cooldown_remaining, layer_at, paid_dig_cost,
};
use crate::dig_tunnels::ascension_effects;
use crate::economy_event_service::EconomyEventConfig;

/// Production image root used by the Rust deployment. The Docker image must
/// copy the authored `assets/dig` tree here; procedural rendering is only the
/// explicit fallback implemented by [`crate::dig_assets::DigAssetService`].
pub const DEFAULT_DIG_ASSET_ROOT: &str = "/app/assets/dig";

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
                     WHERE id=?1 AND discord_id=?2 AND guild_id=?3 AND queued=1",
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
        for item_id in &request.consumed_item_ids {
            transaction.execute(
                "DELETE FROM dig_inventory WHERE id=?1 AND discord_id=?2 AND guild_id=?3 AND queued=1",
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
            injured: tunnel.injury_state.is_some(),
            hard_hat_charges: tunnel.hard_hat_charges,
            reinforced_until: tunnel.reinforced_until,
            void_bait_digs: tunnel.void_bait_digs,
            sonar_skip_pending: tunnel.sonar_skip_pending,
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

fn gear_effects(gear: &[DigRuntimeGear], tunnel: &DigRuntimeTunnel) -> DigGearEffects {
    let Some(weapon) = gear
        .iter()
        .find(|piece| piece.equipped && piece.slot == "weapon" && piece.durability > 0)
    else {
        return WEAPON_TIERS
            .get(usize::try_from(tunnel.pickaxe_tier.max(0)).unwrap_or_default())
            .map_or_else(DigGearEffects::default, |definition| DigGearEffects {
                advance_bonus: i64::from(definition.advance_bonus),
                cave_in_reduction: definition.cave_in_reduction,
                loot_bonus: i64::from(definition.loot_bonus),
            });
    };
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

struct LootRelicEntropy<'a>(&'a mut SeededLootEntropy);

impl RelicEntropy for LootRelicEntropy<'_> {
    fn unit(&mut self) -> f64 {
        self.0.unit()
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
        let snapshot = self.store.snapshot(request.discord_id, request.guild_id)?;
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
        let first_dig = current
            .tunnel
            .as_ref()
            .is_none_or(|tunnel| tunnel.total_digs == 0);
        if current.tunnel.is_none() {
            current.tunnel = Some(DigRuntimeTunnel::new(
                request.discord_id,
                request.guild_id,
                now,
            ));
        }
        let today = game_date_for_timestamp(now as f64).unwrap_or_else(|_| "unknown".to_owned());
        if !first_dig {
            current.weather = self.store.ensure_weather(request.guild_id, &today, now)?;
        }
        let tunnel = current
            .tunnel
            .as_ref()
            .expect("Dig always has a staged tunnel");
        if route_status(tunnel.route_state.as_deref()).choice_required && !first_dig {
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
        let paid_count = if tunnel.paid_dig_date.as_deref() == Some(today.as_str()) {
            usize::try_from(tunnel.paid_digs_today.max(0)).unwrap_or(usize::MAX)
        } else {
            0
        };
        let paid_cost = paid_dig_cost(paid_count, tunnel.stat_stamina, 0.0);
        let (curse_fx, _curse_remaining) =
            active_curse_effects(tunnel.temp_curses.as_deref()).unwrap_or_default();
        let cooldown_seconds = (3_600.0 * (1.0 + curse_fx.cooldown_penalty.max(0.0))) as i64;
        let cooldown = cooldown_remaining(tunnel.last_dig_at, now, cooldown_seconds);
        if !first_dig && cooldown > 0 && !request.paid {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                format!("Dig on cooldown ({cooldown}s remaining)."),
                paid_cost,
                cooldown,
            ));
        }
        if !first_dig && request.paid && cooldown > 0 && current.balance < paid_cost {
            return Ok(DigRuntimeOutcome::blocked(
                &current,
                format!(
                    "Paid dig costs {paid_cost} JC but you only have {} JC.",
                    current.balance
                ),
                paid_cost,
                cooldown,
            ));
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
            let mut state = tunnel_state(&current, None);
            let mut first = apply_first_dig(&mut state, advance, jc_roll, jc_roll, now);
            let pet_dig_bonus = pet_work.as_ref().map_or(0, |work| work.available_blocks());
            if pet_dig_bonus > 0 {
                first.advance = first.advance.saturating_add(pet_dig_bonus);
                state.depth = first.advance;
                state.max_depth = first.advance;
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
                    "pet_dig_bonus": pet_dig_bonus,
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
                boss_boundary: None,
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
        let equipped_relics = current
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect::<Vec<_>>();
        let relics = RelicSet::new(equipped_relics);
        let gear_fx = gear_effects(&current.gear, tunnel);
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
            luminosity_drain_multiplier: (1.0
                + route_luminosity_delta
                + weather_fx.luminosity_drain_multiplier
                + ascension_number("luminosity_drain_multiplier"))
            .max(0.0),
            luminosity_drain_bonus: curse_fx.luminosity_drain,
            jc_multiplier: (1.0 + weather_fx.jc_multiplier + ascension_number("jc_multiplier"))
                .max(0.0),
            jc_bonus: weather_fx.jc_bonus + gear_fx.loot_bonus + curse_fx.jc_bonus,
            artifact_multiplier: (1.0
                + weather_fx.artifact_multiplier
                + route_number("artifact_multiplier")
                + ascension_number("artifact_multiplier"))
            .max(0.0),
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
        };
        let consumed_item_ids = current
            .inventory
            .iter()
            .filter(|item| item.queued)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let mut loot = DigLootService::new(
            DigRuntimeLootRepository::new(current.clone()),
            SeededLootEntropy::new(seed_for(request)),
        );
        let loot_outcome =
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
                    is_paid_dig: request.paid,
                    include_random: true,
                },
                &mut entropy,
            )
        };
        let post_pinnacle_multiplier = post_pinnacle_decay_factor(depth_before, &relics);
        let artifact_multiplier = loot_modifiers.artifact_multiplier * post_pinnacle_multiplier;
        let artifact_id = if loot.entropy_mut().unit() < 0.005 * artifact_multiplier {
            let owned = current
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<BTreeSet<_>>();
            let mut candidates = crate::dig_loot::artifact_catalog()
                .into_iter()
                .filter(|artifact| {
                    artifact.is_relic
                        && !artifact.trophy
                        && artifact.min_prestige <= tunnel.prestige_level
                        && !owned.contains(artifact.id)
                })
                .collect::<Vec<_>>();
            let layer_name = layer.name;
            candidates.sort_by_key(|artifact| (artifact.layer != layer_name, artifact.id));
            let weight_total = candidates
                .iter()
                .map(|artifact| artifact.rarity.weight())
                .sum::<u64>();
            if weight_total == 0 {
                None
            } else {
                let pick = (loot.entropy_mut().unit() * weight_total as f64) as u64;
                let mut cursor = 0_u64;
                let selected = candidates.into_iter().find(|artifact| {
                    cursor = cursor.saturating_add(artifact.rarity.weight());
                    pick < cursor
                });
                selected.and_then(|artifact| {
                    loot.add_artifact(request.discord_id, request.guild_id, artifact.id)
                        .ok()
                        .map(|_| artifact.id.to_owned())
                })
            }
        } else {
            None
        };
        let cave_loss = if loot_outcome.cave_in {
            let rolled = loot.entropy_mut().advance(1, 12);
            let loss = crate::dig_service::cave_in_loss(
                depth_before,
                rolled.saturating_add(loot_modifiers.cave_in_loss_bonus),
                tunnel.reinforced_until > now,
            );
            loot_modifiers
                .cave_in_loss_cap
                .map_or(loss, |cap| loss.min(cap))
        } else {
            0
        };
        let mut staged = loot.repository().snapshot().clone();
        let mut state = tunnel_state(&staged, request.paid.then_some(paid_cost));
        state.depth = depth_before;
        let outcome_input = DigOutcomeInput {
            advance: loot_outcome.advance,
            gross_jc,
            cave_in_loss: cave_loss,
            dynamite: loot_outcome.items_used.contains(&"dynamite"),
            depth_charge: loot_outcome.items_used.contains(&"depth_charge"),
            authored_event: request.forced_event,
            weather_yield_percent: (loot_modifiers.jc_multiplier
                * relic_yield_multiplier
                * post_pinnacle_multiplier
                * 100.0) as i64,
            overgrowth_bonus: loot_modifiers.jc_bonus,
            ..DigOutcomeInput::default()
        };
        let mut outcome = apply_dig_outcome(&mut state, outcome_input, now);
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
                let mut candidate_state = tunnel_state(&staged, request.paid.then_some(paid_cost));
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
        staged = apply_state(&staged, state, &today, request.paid, paid_cost);
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
        let event_id = loot_outcome.event.clone().or_else(|| {
            request.forced_event.then(|| {
                loot.roll_event(
                    depth_before,
                    staged
                        .tunnel
                        .as_ref()
                        .map_or(100, |tunnel| tunnel.luminosity),
                    tunnel.prestige_level,
                )
                .map(|event| event.id.to_owned())
            })?
        });
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
            "event": event_id.clone(),
            "artifact": artifact_id.clone(),
            "items_used": loot_outcome.items_used,
            "paid": request.paid,
            "pet_dig_bonus": pet_dig_bonus,
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
            jc_delta: outcome
                .jc_earned
                .saturating_sub(if request.paid { paid_cost } else { 0 }),
            balance_cost: if request.paid { paid_cost } else { 0 },
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
            cave_in_detail: outcome.cave_in.then(|| {
                format!(
                    "Lost {} blocks in the cave-in.",
                    depth_before - outcome.depth_after
                )
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
    use cama_db::dig_inventory_repository::{BuyInsuranceOutcome, SetTrapOutcome};
    use cama_db::schema_manager::initialize_or_migrate;
    use cama_domain::pet::DIG_WORK_UNITS_PER_BLOCK;
    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use crate::dig_loot::LootEntropy;

    use super::{
        DigAdminMutationOutcome, DigRuntimeCommit, DigRuntimeDeliveryContext,
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
}

#[cfg(test)]
#[path = "dig_pet_runtime_tests.rs"]
mod dig_pet_runtime_tests;
