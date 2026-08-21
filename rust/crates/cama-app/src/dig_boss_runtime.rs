//! Cohesive application and SQLite adapters for live Dig boss encounters.
//!
//! The Discord provider owns no boss SQL. This module rehydrates the typed
//! regular-boss policy from the Python-compatible schema, preserves unknown
//! JSON fields used by carried wagers, preparations, and phase events, and
//! delegates guarded settlement, durable duel recovery, inventory claims, and
//! post-resolution gear wear to `cama-db`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::dig_boss_runtime::{
    DigBossCheerCommit, DigBossCheerCommitOutcome, DigBossGearSnapshot, DigBossPausedDuelRow,
    DigBossPreparationActivation, DigBossPreparationActivationOutcome, DigBossRegularGearGrant,
    DigBossRuntimeArtifactGrant, DigBossRuntimeAudit, DigBossRuntimeCommit,
    DigBossRuntimeCommitOutcome, DigBossRuntimeEcho, DigBossRuntimeKey, DigBossRuntimeRepository,
    DigBossRuntimeSnapshot, DigBossRuntimeState,
};
use cama_db::dig_carry_wager::{CarryTunnelKey, DigCarryWagerRepository};
use cama_db::dig_gear_runtime::DigGearRuntimeRepository;
use cama_db::dig_inventory_repository::DigInventoryRepository;
use cama_db::mana_service_repository::ManaRepository;
use cama_db::pet_repository::PetRepository;
use cama_domain::dig_economy::{
    DIG_POSITIVE_JC_MULTIPLIER, WagerMultiplier, WinChance, effective_wager_multiplier,
    scale_positive_dig_jc, settled_wager_multiplier,
};
use cama_domain::dig_gear::{
    CombatStats as GearCombatStats, GEAR_BOSS_DROP_RATE, GEAR_MAX_DURABILITY, GearLoadout,
    GearSlot as DomainGearSlot, apply_gear_to_combat, drop_tier_at_depth, tier_table, unique_gear,
};
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::mana::ManaEffects;
use cama_domain::pet::{PetMood as DomainPetMood, PetStage as DomainPetStage, get_species};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::bankruptcy::{BankruptcyPolicy, BankruptcyService};
use crate::boss_duel::{
    PET_TRAINING_CAP, PHASE_TRANSITION_EVENTS, PetAssist, PetMood, PetStage, RiskTier,
    transition_event,
};
use crate::boss_multi_tier::{
    BOSS_ROUND_CAP, BankruptcyAdjustment, BankruptcyPort, BossAuditRecord, BossCombatPreparation,
    BossCombatPreparationPort, BossCombatPreparationRequest, BossMultiTierService, BossNotice,
    BossOutcomeSinkPort, BossPreparationActivationPort, BossPreparationMutation,
    BossPreparationState, BossProgress, BossProgressEntry, BossProgressValue, BossRepositoryPort,
    BossServiceError, BossStatus, CombatEffects, CombatState, Combatant, CurseKind, EchoRecord,
    EconomyEventPort, EntropyPort, FixedClock, GearRepairPort, GearSnapshot, GearWearResult,
    GuildId, MechanicDefinition, MechanicOption as CoreMechanicOption, MechanicRoundRecord,
    MechanicStatusEffect, OutcomeRoll as CoreOutcomeRoll, PLAYER_HIT_CEILING, PLAYER_HIT_FLOOR,
    PausedBossDuel, PendingPrompt, PlayerKey, PromptOption, RepositoryCommit, RepositoryError,
    ResolvedFight, RoundEffectLog, RoundRecord, ScoutResult, StartBossDuelOutcome,
    StingerCurseState, WAGERED_PLAYER_HIT_FLOOR, apply_option_outcome, at_boss_boundary,
    mechanic_by_id, regular_boss_wager_allowed, run_combat_round,
};
use crate::dig_bosses::{
    BarkContext, CombatStats as BossCombatStats, DialogueSlots, DuelOddsInput, PINNACLE_DEPTH,
    PINNACLE_POOL_IDS, PinnacleRelic, boss_by_id, dialogue_slots,
    duel_win_probability_with_damage_bonus, luminosity_combat_penalty,
    mechanic as pinnacle_mechanic, pinnacle_by_id, regular_phase, render_boss_bark,
    roll_pinnacle_relic, scale_boss_stats_for_archetype,
};
use crate::dig_carry_wager::{
    MAX_NEW_BOSS_WAGER, active_pinnacle_carry, current_boss_boundary, pinnacle_wager_profit,
};
use crate::dig_service::DIG_STARTING_STAT_POINTS;
use crate::economy_event_service::EconomyEventConfig;
use crate::economy_event_sqlite::SqliteEconomyEventService;
use crate::service_container::PersistentVanityTaxService;
use crate::vanity_tax_service::VanityTaxService;

#[derive(Clone, Debug)]
struct LoadedBossSnapshot {
    snapshot: DigBossRuntimeSnapshot,
    lantern_ids: Vec<i64>,
    has_great_lantern: bool,
}

#[derive(Debug, Default)]
struct BossSnapshotCache {
    next_revision: u64,
    latest: BTreeMap<PlayerKey, (LoadedBossSnapshot, u64)>,
    revisions: BTreeMap<(PlayerKey, u64), LoadedBossSnapshot>,
}

impl BossSnapshotCache {
    fn remember(&mut self, key: PlayerKey, loaded: LoadedBossSnapshot) -> u64 {
        if let Some((prior, revision)) = self.latest.get(&key)
            && prior.snapshot == loaded.snapshot
            && prior.lantern_ids == loaded.lantern_ids
            && prior.has_great_lantern == loaded.has_great_lantern
        {
            return *revision;
        }
        self.next_revision = self.next_revision.saturating_add(1).max(1);
        let revision = self.next_revision;
        self.latest.insert(key, (loaded.clone(), revision));
        self.revisions.insert((key, revision), loaded);
        revision
    }

    fn expected(&self, key: PlayerKey, revision: u64) -> Option<&LoadedBossSnapshot> {
        self.revisions.get(&(key, revision))
    }

    fn invalidate(&mut self, key: PlayerKey) {
        self.latest.remove(&key);
    }
}

/// Existing-schema implementation of the regular-boss repository port.
///
/// A fresh value is created for each command/component execution. SQLite is
/// the durable authority; the cache only associates the pure policy's opaque
/// revision token with the exact snapshot used by the guarded commit.
#[derive(Debug)]
pub struct SqliteBossRepository {
    repository: DigBossRuntimeRepository,
    now: i64,
    cache: RefCell<BossSnapshotCache>,
    audits: Vec<BossAuditRecord>,
    last_error: RefCell<Option<String>>,
    last_action_id: Option<i64>,
}

impl SqliteBossRepository {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, now: i64) -> Self {
        Self {
            repository: DigBossRuntimeRepository::new(database_path),
            now,
            cache: RefCell::new(BossSnapshotCache::default()),
            audits: Vec::new(),
            last_error: RefCell::new(None),
            last_action_id: None,
        }
    }

    #[must_use]
    pub const fn last_action_id(&self) -> Option<i64> {
        self.last_action_id
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.borrow_mut().take()
    }

    fn fail(&self, error: impl ToString) {
        *self.last_error.borrow_mut() = Some(error.to_string());
    }

    fn load(&self, key: PlayerKey) -> Result<Option<LoadedBossSnapshot>, String> {
        let database_key = database_key(key);
        let Some(snapshot) = self
            .repository
            .snapshot(database_key)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let (lantern_ids, has_great_lantern) = self
            .repository
            .lantern_state(database_key)
            .map_err(|error| error.to_string())?;
        Ok(Some(LoadedBossSnapshot {
            snapshot,
            lantern_ids,
            has_great_lantern,
        }))
    }
}

impl BossRepositoryPort for SqliteBossRepository {
    fn load_tunnel(&self, key: PlayerKey) -> Option<crate::boss_multi_tier::TunnelState> {
        let loaded = match self.load(key) {
            Ok(Some(loaded)) => loaded,
            Ok(None) => return None,
            Err(error) => {
                self.fail(error);
                return None;
            }
        };
        let revision = self.cache.borrow_mut().remember(key, loaded.clone());
        match tunnel_from_snapshot(&loaded, revision) {
            Ok(tunnel) => Some(tunnel),
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }

    fn commit(
        &mut self,
        key: PlayerKey,
        expected_revision: u64,
        commit: RepositoryCommit,
    ) -> Result<(), RepositoryError> {
        let loaded = self
            .cache
            .borrow()
            .expected(key, expected_revision)
            .cloned()
            .ok_or(RepositoryError::Conflict)?;
        let mut proposed = state_from_tunnel(&loaded.snapshot, &commit.next_tunnel)
            .map_err(|error| self.repository_error(error))?;
        apply_preparation_mutation(&mut proposed, commit.preparation_mutation)
            .map_err(|error| self.repository_error(error))?;
        if commit.audit.is_some() {
            proposed.cheer_data_json = None;
        }
        if let Some(audit) = commit.audit.as_ref()
            && audit.won
            && status_for_boundary(&commit.next_tunnel.boss_progress, audit.boundary)
                == BossStatus::Defeated
            && status_for_boundary_raw(
                loaded.snapshot.boss_progress_json.as_deref(),
                audit.boundary,
            ) != BossStatus::Defeated
        {
            apply_first_clear_stat_award(&loaded.snapshot, &mut proposed, audit.boundary)
                .map_err(|error| self.repository_error(error))?;
        }
        let detail = commit
            .audit
            .as_ref()
            .map(|audit| boss_audit_detail(audit, &commit));
        let audit = commit
            .audit
            .as_ref()
            .zip(detail.as_deref())
            .map(|(_audit, detail_json)| DigBossRuntimeAudit {
                action_type: "boss_fight",
                target_id: None,
                detail_json,
                created_at: self.now,
            });
        let echo = commit.echo.as_ref().map(|echo| DigBossRuntimeEcho {
            boss_id: &echo.boss_id,
            depth: i64::from(echo.boundary),
            killer_id: echo.killer_id,
            weakened_until: echo.weakened_until,
        });
        let outcome = self
            .repository
            .commit(DigBossRuntimeCommit {
                expected: &loaded.snapshot,
                proposed: &proposed,
                audit,
                echo,
                artifact: None,
                vanity_tax: commit.vanity_tax,
                low_priority_tax: commit.low_priority_tax,
            })
            .map_err(|error| self.repository_error(error))?;
        match outcome {
            DigBossRuntimeCommitOutcome::Applied(receipt) => {
                self.last_action_id = receipt.action_id;
                if let Some(audit) = commit.audit {
                    self.audits.push(audit);
                }
                self.cache.borrow_mut().invalidate(key);
                if loaded.lantern_ids.len() > commit.next_tunnel.lanterns as usize
                    && !loaded.has_great_lantern
                {
                    let Some(row_id) = loaded.lantern_ids.first().copied() else {
                        return Err(RepositoryError::Conflict);
                    };
                    let consumed = self
                        .repository
                        .consume_lantern(database_key(key), row_id)
                        .map_err(|error| self.repository_error(error))?;
                    if !consumed {
                        return Err(RepositoryError::Conflict);
                    }
                }
                Ok(())
            }
            DigBossRuntimeCommitOutcome::Conflict => Err(RepositoryError::Conflict),
            DigBossRuntimeCommitOutcome::MissingPlayer
            | DigBossRuntimeCommitOutcome::MissingTunnel => Err(RepositoryError::MissingTunnel),
        }
    }

    fn save_active_duel(
        &mut self,
        key: PlayerKey,
        duel: PausedBossDuel,
    ) -> Result<(), RepositoryError> {
        let row =
            paused_duel_row(key, &duel, self.now).map_err(|error| self.repository_error(error))?;
        self.repository
            .save_active_duel(&row)
            .map_err(|error| self.repository_error(error))
    }

    fn claim_active_duel(
        &mut self,
        key: PlayerKey,
    ) -> Result<Option<PausedBossDuel>, RepositoryError> {
        self.repository
            .claim_active_duel(database_key(key))
            .map_err(|error| self.repository_error(error))?
            .map(|row| paused_duel_from_row(&row))
            .transpose()
            .map_err(|error| self.repository_error(error))
    }

    fn active_duel(&self, key: PlayerKey) -> Option<PausedBossDuel> {
        match self.repository.active_duel(database_key(key)) {
            Ok(Some(row)) => match paused_duel_from_row(&row) {
                Ok(duel) => Some(duel),
                Err(error) => {
                    self.fail(error);
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }

    fn active_echo(&self, guild_id: GuildId, boss_id: &str, now: i64) -> Option<EchoRecord> {
        match self.repository.active_echo(guild_id.0, boss_id, now) {
            Ok(Some((boundary, killer_id, weakened_until))) => Some(EchoRecord {
                boss_id: boss_id.to_owned(),
                boundary: i32::try_from(boundary).unwrap_or(i32::MAX),
                killer_id,
                weakened_until,
            }),
            Ok(None) => None,
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }

    fn audits(&self) -> &[BossAuditRecord] {
        &self.audits
    }
}

fn apply_preparation_mutation(
    proposed: &mut DigBossRuntimeState,
    mutation: BossPreparationMutation,
) -> Result<(), String> {
    let (boundary, clear) = match mutation {
        BossPreparationMutation::Preserve => return Ok(()),
        BossPreparationMutation::MarkUsed { boundary } => (boundary, false),
        BossPreparationMutation::Clear { boundary } => (boundary, true),
    };
    let Some(raw) = proposed.boss_progress_json.as_deref() else {
        return Ok(());
    };
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| format!("boss_progress JSON is invalid: {error}"))?;
    let boundary = boundary.to_string();
    let Some(entry) = value
        .as_object_mut()
        .and_then(|root| root.get_mut(&boundary))
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    if clear {
        entry.remove("active_prep");
    } else if let Some(preparation) = entry.get_mut("active_prep").and_then(Value::as_object_mut) {
        preparation.insert("used".to_owned(), Value::Bool(true));
    }
    proposed.boss_progress_json = Some(
        serde_json::to_string(&value)
            .map_err(|error| format!("boss_progress JSON cannot be encoded: {error}"))?,
    );
    Ok(())
}

impl SqliteBossRepository {
    fn repository_error(&self, error: impl ToString) -> RepositoryError {
        self.fail(error);
        RepositoryError::InjectedFailure
    }
}

/// Post-resolution gear port. Its transaction intentionally happens after the
/// actor settlement, matching Python's visible partial-failure boundary.
#[derive(Debug)]
pub struct SqliteBossGearRepair {
    repository: DigBossRuntimeRepository,
    last_error: RefCell<Option<String>>,
}

impl SqliteBossGearRepair {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            repository: DigBossRuntimeRepository::new(database_path),
            last_error: RefCell::new(None),
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.borrow_mut().take()
    }

    fn fail(&self, error: impl ToString) {
        *self.last_error.borrow_mut() = Some(error.to_string());
    }
}

impl GearRepairPort for SqliteBossGearRepair {
    fn snapshot(&self, key: PlayerKey) -> GearSnapshot {
        match self.repository.gear_snapshot(database_key(key)) {
            Ok(snapshot) => GearSnapshot {
                gear_ids: snapshot.gear_ids,
                armor_id: snapshot.armor_id,
            },
            Err(error) => {
                self.fail(error);
                GearSnapshot::default()
            }
        }
    }

    fn repair_bill(&self) -> i64 {
        8
    }

    fn tick_resolved_fight(
        &mut self,
        key: PlayerKey,
        snapshot: &GearSnapshot,
        won: bool,
    ) -> GearWearResult {
        let database_snapshot = DigBossGearSnapshot {
            gear_ids: snapshot.gear_ids.clone(),
            armor_id: snapshot.armor_id,
        };
        match self
            .repository
            .tick_gear_after_resolution(database_key(key), &database_snapshot, won)
        {
            Ok(receipt) => GearWearResult {
                broken_ids: receipt.broken_ids,
            },
            Err(error) => {
                self.fail(error);
                GearWearResult::default()
            }
        }
    }
}

const BOSS_PREPARATION_ITEM_TYPES: [&str; 3] =
    ["tempered_whetstone", "warding_salts", "rescue_line"];

/// Claims the first queued authored boss preparation into the boundary entry.
/// Selection mirrors Python's queued-item order; the DB adapter CASes the full
/// tunnel snapshot and consumes that exact inventory row with the JSON update.
#[derive(Debug)]
pub struct SqliteBossPreparationActivation {
    database_path: PathBuf,
    last_error: RefCell<Option<String>>,
}

impl SqliteBossPreparationActivation {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            last_error: RefCell::new(None),
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.borrow_mut().take()
    }

    fn fail(&self, error: impl ToString) -> RepositoryError {
        *self.last_error.borrow_mut() = Some(error.to_string());
        RepositoryError::InjectedFailure
    }
}

impl BossPreparationActivationPort for SqliteBossPreparationActivation {
    fn activate(&self, key: PlayerKey, boundary: i32) -> Result<(), RepositoryError> {
        let repository = DigBossRuntimeRepository::new(&self.database_path);
        let inventory = DigInventoryRepository::new(&self.database_path);
        loop {
            let snapshot = repository
                .snapshot(database_key(key))
                .map_err(|error| self.fail(error))?
                .ok_or(RepositoryError::MissingTunnel)?;
            let progress = snapshot
                .boss_progress_json
                .as_deref()
                .filter(|raw| !raw.trim().is_empty())
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            let boundary_key = boundary.to_string();
            if progress
                .get(&boundary_key)
                .and_then(Value::as_object)
                .and_then(|entry| entry.get("active_prep"))
                .and_then(Value::as_object)
                .and_then(|prep| prep.get("item_type"))
                .and_then(Value::as_str)
                .is_some_and(|item_type| BOSS_PREPARATION_ITEM_TYPES.contains(&item_type))
            {
                return Ok(());
            }
            let queued = inventory
                .queued_items(key.player_id, Some(key.guild_id.0))
                .map_err(|error| self.fail(error))?;
            let Some(selected) = queued
                .iter()
                .find(|item| BOSS_PREPARATION_ITEM_TYPES.contains(&item.item_type.as_str()))
            else {
                return Ok(());
            };
            let mut proposed = progress;
            let entry = proposed
                .entry(boundary_key)
                .or_insert_with(|| json!({"status":"active"}));
            if !entry.is_object() {
                let status = entry.as_str().unwrap_or("active").to_owned();
                *entry = json!({"status":status});
            }
            let entry = entry
                .as_object_mut()
                .expect("the legacy boundary was normalized to an object");
            entry.insert(
                "active_prep".to_owned(),
                json!({"item_type":selected.item_type,"used":false}),
            );
            let proposed = serde_json::to_string(&proposed).map_err(|error| self.fail(error))?;
            match repository
                .activate_preparation(DigBossPreparationActivation {
                    expected: &snapshot,
                    inventory_item_id: selected.id,
                    item_type: &selected.item_type,
                    proposed_boss_progress_json: &proposed,
                })
                .map_err(|error| self.fail(error))?
            {
                DigBossPreparationActivationOutcome::Applied => return Ok(()),
                DigBossPreparationActivationOutcome::Conflict
                | DigBossPreparationActivationOutcome::ItemUnavailable => continue,
                DigBossPreparationActivationOutcome::MissingPlayer
                | DigBossPreparationActivationOutcome::MissingTunnel => {
                    return Err(RepositoryError::MissingTunnel);
                }
            }
        }
    }

    fn take_error(&self) -> Option<String> {
        SqliteBossPreparationActivation::take_error(self)
    }
}

/// Loads all external combat modifiers once for one attempt. Read failures are
/// fail-soft like Python's mana/pet lookups, but remain observable on the typed
/// runtime result.
#[derive(Debug)]
pub struct SqliteBossCombatPreparation {
    database_path: PathBuf,
    pet_hunger_decay_per_day: i64,
    last_error: RefCell<Option<String>>,
}

impl SqliteBossCombatPreparation {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, pet_hunger_decay_per_day: i64) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            pet_hunger_decay_per_day,
            last_error: RefCell::new(None),
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.borrow_mut().take()
    }

    fn fail(&self, error: impl ToString) {
        *self.last_error.borrow_mut() = Some(error.to_string());
    }

    fn loadout(&self, key: PlayerKey) -> GearLoadout {
        let snapshot = match DigGearRuntimeRepository::new(&self.database_path)
            .snapshot(key.player_id, key.guild_id.0)
        {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return GearLoadout::default(),
            Err(error) => {
                self.fail(error);
                return GearLoadout::default();
            }
        };
        let mut loadout = GearLoadout::default();
        for piece in snapshot.gear.into_iter().filter(|piece| piece.equipped) {
            match piece.slot {
                DomainGearSlot::Weapon => loadout.weapon = Some(piece),
                DomainGearSlot::Armor => loadout.armor = Some(piece),
                DomainGearSlot::Boots => loadout.boots = Some(piece),
                DomainGearSlot::Amulet => loadout.amulet = Some(piece),
                DomainGearSlot::Ring => loadout.ring = Some(piece),
                DomainGearSlot::Relic => {}
            }
        }
        loadout.relics = snapshot
            .artifacts
            .into_iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id)
            .collect();
        loadout
    }

    fn mana_effects(&self, key: PlayerKey, now: i64) -> ManaEffects {
        let Ok(today) = game_date_for_timestamp(now as f64) else {
            return ManaEffects::default();
        };
        match ManaRepository::new(&self.database_path).get_mana(key.player_id, Some(key.guild_id.0))
        {
            Ok(Some(mana)) if mana.assigned_date == today && !mana.consumed_today => {
                mana_effects_for_land(&mana.current_land)
            }
            Ok(_) => ManaEffects::default(),
            Err(error) => {
                self.fail(error);
                ManaEffects::default()
            }
        }
    }

    fn pet_assist(&self, key: PlayerKey, now: i64) -> Option<PetAssist> {
        let pet = match PetRepository::new(&self.database_path)
            .get_active_pet(key.player_id, Some(key.guild_id.0))
        {
            Ok(pet) => pet?,
            Err(error) => {
                self.fail(error);
                return None;
            }
        };
        if pet.stage(now) != DomainPetStage::Adult || pet.training_xp < i64::from(PET_TRAINING_CAP)
        {
            return None;
        }
        let mood = match pet.mood(now, self.pet_hunger_decay_per_day.max(1)) {
            DomainPetMood::Happy => PetMood::Happy,
            DomainPetMood::Content => PetMood::Content,
            DomainPetMood::Hungry => PetMood::Hungry,
            DomainPetMood::Starving => PetMood::Starving,
        };
        let species = get_species(&pet.species);
        crate::boss_duel::pet_boss_assist(&crate::boss_duel::PetSnapshot {
            name: pet.name,
            species_id: pet.species,
            species_name: species.display_name.to_owned(),
            training_xp: i32::try_from(pet.training_xp).unwrap_or(i32::MAX),
            stage: PetStage::Adult,
            mood,
        })
    }

    fn snapshot(&self, key: PlayerKey) -> Option<DigBossRuntimeSnapshot> {
        match DigBossRuntimeRepository::new(&self.database_path).snapshot(database_key(key)) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }
}

impl BossCombatPreparationPort for SqliteBossCombatPreparation {
    fn prepare(
        &self,
        request: BossCombatPreparationRequest<'_>,
        base_combat: CombatState,
    ) -> BossCombatPreparation {
        let loadout = self.loadout(request.key);
        let mut gear = apply_gear_to_combat(
            GearCombatStats {
                player_hp: base_combat.player_hp,
                boss_hp: base_combat.boss_hp,
                player_hit: base_combat.player_hit,
                player_dmg: base_combat.player_damage,
                boss_hit: base_combat.boss_hit,
                boss_dmg: base_combat.boss_damage,
                crit_chance: base_combat.critical_chance,
                crit_bonus: base_combat.critical_bonus,
            },
            &loadout,
        );
        if request.boundary == PINNACLE_DEPTH {
            let mut depth_damage_stacks = 0_i32;
            for stat_id in loadout
                .relics
                .iter()
                .filter_map(|artifact_id| {
                    artifact_id
                        .strip_prefix("pinnacle:")
                        .map(|remainder| remainder.split(':').skip(2))
                })
                .flatten()
            {
                match stat_id {
                    "hp_plus_1" => gear.player_hp = gear.player_hp.saturating_add(1),
                    "hit_plus_002" => gear.player_hit += 0.02,
                    "boss_hit_minus" => gear.boss_hit = (gear.boss_hit - 0.02).max(0.05),
                    "dmg_plus_per_100" => {
                        depth_damage_stacks = depth_damage_stacks.saturating_add(1);
                    }
                    _ => {}
                }
            }
            gear.player_dmg = gear
                .player_dmg
                .saturating_add(depth_damage_stacks.saturating_mul(request.boundary / 100));
        }
        let mut prepared = BossCombatPreparation::neutral(CombatState {
            player_hp: gear.player_hp,
            boss_hp: gear.boss_hp,
            player_hit: gear.player_hit,
            player_damage: gear.player_dmg,
            boss_hit: gear.boss_hit,
            boss_damage: gear.boss_dmg,
            critical_chance: gear.crit_chance,
            critical_bonus: gear.crit_bonus,
            effects: base_combat.effects,
        });
        let snapshot = self.snapshot(request.key);
        let luminosity = snapshot
            .as_ref()
            .and_then(|snapshot| i32::try_from(snapshot.luminosity).ok())
            .unwrap_or(request.tunnel.luminosity);
        let (luminosity_hit, luminosity_damage) = luminosity_combat_penalty(luminosity);
        prepared.player_hit_offset += luminosity_hit;
        prepared.boss_damage_delta += luminosity_damage;
        let active_cheers = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cheer_data_json.as_deref())
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|cheer| {
                cheer
                    .get("expires_at")
                    .and_then(Value::as_i64)
                    .is_some_and(|expires_at| expires_at > request.now)
            })
            .count()
            .min(3);
        prepared.player_hit_offset += active_cheers as f64 * 0.05;

        let mana = self.mana_effects(request.key, request.now);
        prepared.boss_hp_multiplier = mana.boss_hp_mult;
        prepared.player_damage_multiplier = mana.boss_damage_mult;
        prepared.damage_variance_modifier = mana.boss_damage_variance_modifier;
        prepared.boss_no_crit_against = mana.boss_no_crit_against;
        if loadout.relics.iter().any(|relic| relic == "hollow_fang") {
            prepared.player_damage_multiplier *= 1.15;
        }
        prepared.shifting_idol = loadout.relics.iter().any(|relic| relic == "shifting_idol");
        let effects = &mut prepared.base_combat.effects;
        effects.trophy_venom_rounds = if loadout.relics.iter().any(|relic| relic == "weeping_fang")
        {
            4
        } else {
            0
        };
        effects.trophy_lifesteal = loadout
            .relics
            .iter()
            .any(|relic| relic == "runebitten_shard");
        effects.trophy_regrowth = loadout.relics.iter().any(|relic| relic == "aching_spine");
        effects.trophy_forewarned = loadout
            .relics
            .iter()
            .any(|relic| relic == "listening_shard");
        effects.trophy_laststand = loadout.relics.iter().any(|relic| relic == "hateborn_ember");
        effects.relic_berserk_rage = loadout
            .relics
            .iter()
            .any(|relic| relic == "berserkers_mark")
            .then_some(0);
        effects.relic_double_hit = loadout.relics.iter().any(|relic| relic == "gamblers_edge");
        effects.relic_deaths_door = loadout.relics.iter().any(|relic| relic == "deaths_door");
        effects.relic_paper_crane = loadout.relics.iter().any(|relic| relic == "paper_crane");
        effects.relic_bottled_quake = loadout.relics.iter().any(|relic| relic == "bottled_quake");
        for piece in [
            loadout.weapon.as_ref(),
            loadout.armor.as_ref(),
            loadout.boots.as_ref(),
            loadout.amulet.as_ref(),
            loadout.ring.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|piece| piece.durability > 0)
        {
            match piece
                .item_id
                .as_deref()
                .and_then(unique_gear)
                .and_then(|definition| definition.effect_id)
            {
                Some("reflect_first_hit") => effects.gear_reflect_first_hit = true,
                Some("block_first_status") => effects.gear_block_first_status = true,
                Some("springheel_counter") => effects.gear_springheel_counter = true,
                Some("block_first_skip") => effects.gear_block_first_skip = true,
                Some("heal_first_crit") => effects.gear_heal_first_crit = true,
                Some("reroll_first_miss") => effects.gear_reroll_first_miss = true,
                _ => {}
            }
        }
        prepared.pet_assist = self.pet_assist(request.key, request.now);
        let active_prep = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.boss_progress_json.as_deref())
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| value.get(request.boundary.to_string()).cloned())
            .and_then(|entry| entry.get("active_prep").cloned())
            .and_then(|prep| {
                let item_type = prep
                    .get("item_type")
                    .or_else(|| prep.get("item"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)?;
                Some(BossPreparationState {
                    item_type,
                    used: prep.get("used").and_then(Value::as_bool).unwrap_or(false),
                })
            });
        if active_prep
            .as_ref()
            .is_some_and(|preparation| preparation.item_type == "tempered_whetstone")
        {
            prepared.player_damage_after_phase_delta = 1;
        }
        prepared.boss_preparation = active_prep;
        prepared
    }

    fn take_error(&self) -> Option<String> {
        SqliteBossCombatPreparation::take_error(self)
    }
}

fn mana_effects_for_land(land: &str) -> ManaEffects {
    let color = match land {
        "Mountain" => Some("Red"),
        "Island" => Some("Blue"),
        "Forest" => Some("Green"),
        "Plains" => Some("White"),
        "Swamp" => Some("Black"),
        _ => None,
    };
    ManaEffects::for_color(color, color.map(|_| land))
}

fn database_key(key: PlayerKey) -> DigBossRuntimeKey {
    DigBossRuntimeKey {
        discord_id: key.player_id,
        guild_id: key.guild_id.0,
    }
}

fn tunnel_from_snapshot(
    loaded: &LoadedBossSnapshot,
    revision: u64,
) -> Result<crate::boss_multi_tier::TunnelState, String> {
    let snapshot = &loaded.snapshot;
    Ok(crate::boss_multi_tier::TunnelState {
        depth: narrow_i32(snapshot.depth, "depth")?,
        max_depth: narrow_i32(snapshot.max_depth, "max_depth")?,
        prestige_level: narrow_i32(snapshot.prestige_level, "prestige_level")?,
        balance: snapshot.balance,
        boss_progress: parse_boss_progress(snapshot.boss_progress_json.as_deref())?,
        stinger_curse: parse_stinger_curse(snapshot.stinger_curse_json.as_deref()),
        lanterns: u32::try_from(loaded.lantern_ids.len()).unwrap_or(u32::MAX),
        has_great_lantern: loaded.has_great_lantern,
        boss_attempts: narrow_i32(snapshot.boss_attempts, "boss_attempts")?,
        last_dig_at: snapshot.last_dig_at.unwrap_or_default(),
        luminosity: narrow_i32(snapshot.luminosity, "luminosity")?,
        revision,
    })
}

fn state_from_tunnel(
    snapshot: &DigBossRuntimeSnapshot,
    tunnel: &crate::boss_multi_tier::TunnelState,
) -> Result<DigBossRuntimeState, String> {
    let mut state = DigBossRuntimeState::from(snapshot);
    state.depth = i64::from(tunnel.depth);
    state.max_depth = i64::from(tunnel.max_depth);
    state.balance = tunnel.balance;
    state.boss_progress_json = Some(merge_boss_progress(
        snapshot.boss_progress_json.as_deref(),
        &tunnel.boss_progress,
    )?);
    state.stinger_curse_json = merge_stinger_curse(
        snapshot.stinger_curse_json.as_deref(),
        &tunnel.stinger_curse,
    );
    state.boss_attempts = i64::from(tunnel.boss_attempts);
    state.last_dig_at = Some(tunnel.last_dig_at);
    state.luminosity = i64::from(tunnel.luminosity);
    Ok(state)
}

fn narrow_i32(value: i64, field: &'static str) -> Result<i32, String> {
    i32::try_from(value).map_err(|_| format!("boss tunnel {field} is outside i32 range"))
}

fn parse_boss_progress(raw: Option<&str>) -> Result<BossProgress, String> {
    let Some(raw) = raw.filter(|raw| !raw.trim().is_empty()) else {
        return Ok(BossProgress::new());
    };
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| format!("boss_progress JSON is invalid: {error}"))?;
    let Some(object) = value.as_object() else {
        return Ok(BossProgress::new());
    };
    let mut progress = BossProgress::new();
    for (boundary, value) in object {
        let parsed = if let Some(status) = value.as_str() {
            BossProgressValue::Legacy(parse_status(status))
        } else if let Some(entry) = value.as_object() {
            BossProgressValue::Entry(BossProgressEntry {
                status: entry
                    .get("status")
                    .and_then(Value::as_str)
                    .map_or(BossStatus::Active, parse_status),
                boss_id: entry
                    .get("boss_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                hp_remaining: json_i32(entry.get("hp_remaining")),
                hp_max: json_i32(entry.get("hp_max")),
                last_engaged_at: entry.get("last_engaged_at").and_then(Value::as_i64),
                last_outcome: entry
                    .get("last_outcome")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                first_meet_seen: entry
                    .get("first_meet_seen")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                pending_phase_event_id: entry
                    .get("pending_phase_event_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        } else {
            BossProgressValue::Legacy(BossStatus::Active)
        };
        progress.insert(boundary.clone(), parsed);
    }
    Ok(progress)
}

fn merge_boss_progress(raw: Option<&str>, progress: &BossProgress) -> Result<String, String> {
    let mut root = raw
        .filter(|raw| !raw.trim().is_empty())
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    for (boundary, value) in progress {
        match value {
            BossProgressValue::Legacy(status) => {
                root.insert(
                    boundary.clone(),
                    Value::String(status_name(*status).to_owned()),
                );
            }
            BossProgressValue::Entry(entry) => {
                let mut object = root
                    .remove(boundary)
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                object.insert(
                    "status".to_owned(),
                    Value::String(status_name(entry.status).to_owned()),
                );
                set_optional_string(&mut object, "boss_id", entry.boss_id.as_deref());
                set_optional_i64(
                    &mut object,
                    "hp_remaining",
                    entry.hp_remaining.map(i64::from),
                );
                set_optional_i64(&mut object, "hp_max", entry.hp_max.map(i64::from));
                set_optional_i64(&mut object, "last_engaged_at", entry.last_engaged_at);
                set_optional_string(&mut object, "last_outcome", entry.last_outcome.as_deref());
                if entry.first_meet_seen {
                    object.insert("first_meet_seen".to_owned(), Value::Bool(true));
                } else {
                    object.remove("first_meet_seen");
                }
                set_optional_string(
                    &mut object,
                    "pending_phase_event_id",
                    entry.pending_phase_event_id.as_deref(),
                );
                root.insert(boundary.clone(), Value::Object(object));
            }
        }
    }
    serde_json::to_string(&root).map_err(|error| error.to_string())
}

fn parse_status(status: &str) -> BossStatus {
    match status {
        "phase1_defeated" => BossStatus::PhaseOneDefeated,
        "phase2_defeated" => BossStatus::PhaseTwoDefeated,
        "defeated" => BossStatus::Defeated,
        _ => BossStatus::Active,
    }
}

fn status_name(status: BossStatus) -> &'static str {
    match status {
        BossStatus::Active => "active",
        BossStatus::PhaseOneDefeated => "phase1_defeated",
        BossStatus::PhaseTwoDefeated => "phase2_defeated",
        BossStatus::Defeated => "defeated",
    }
}

fn status_for_boundary(progress: &BossProgress, boundary: i32) -> BossStatus {
    progress
        .get(&boundary.to_string())
        .map_or(BossStatus::Active, BossProgressValue::status)
}

fn status_for_boundary_raw(raw: Option<&str>, boundary: i32) -> BossStatus {
    parse_boss_progress(raw)
        .ok()
        .map(|progress| status_for_boundary(&progress, boundary))
        .unwrap_or(BossStatus::Active)
}

fn json_i32(value: Option<&Value>) -> Option<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn set_optional_string(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    } else {
        object.remove(key);
    }
}

fn set_optional_i64(object: &mut Map<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::from(value));
    } else {
        object.remove(key);
    }
}

fn parse_stinger_curse(raw: Option<&str>) -> StingerCurseState {
    let object = raw
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut state = StingerCurseState::default();
    for (key, kind) in [
        (
            "halve_next_wager",
            crate::boss_multi_tier::CurseKind::HalveNextWager,
        ),
        (
            "no_scout_next_dig",
            crate::boss_multi_tier::CurseKind::NoScoutNextDig,
        ),
        (
            "drain_next_reward",
            crate::boss_multi_tier::CurseKind::DrainNextReward,
        ),
    ] {
        if object.get(key).and_then(Value::as_bool).unwrap_or(false) {
            state.active.insert(kind);
        }
    }
    state.boss_id = object
        .get("_boss_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    state
}

fn merge_stinger_curse(raw: Option<&str>, state: &StingerCurseState) -> Option<String> {
    let mut object = raw
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    for (key, kind) in [
        (
            "halve_next_wager",
            crate::boss_multi_tier::CurseKind::HalveNextWager,
        ),
        (
            "no_scout_next_dig",
            crate::boss_multi_tier::CurseKind::NoScoutNextDig,
        ),
        (
            "drain_next_reward",
            crate::boss_multi_tier::CurseKind::DrainNextReward,
        ),
    ] {
        if state.active.contains(&kind) {
            object.insert(key.to_owned(), Value::Bool(true));
        } else {
            object.remove(key);
        }
    }
    set_optional_string(&mut object, "_boss_id", state.boss_id.as_deref());
    (!object.is_empty()).then(|| Value::Object(object).to_string())
}

fn apply_first_clear_stat_award(
    snapshot: &DigBossRuntimeSnapshot,
    proposed: &mut DigBossRuntimeState,
    boundary: i32,
) -> Result<(), String> {
    let mut awards = stat_boss_awards(snapshot)?;
    if !awards.insert(boundary) {
        return Ok(());
    }
    proposed.stat_points = snapshot
        .stat_points
        .max(DIG_STARTING_STAT_POINTS)
        .saturating_add(1);
    proposed.stat_boss_awards_json = Some(
        json!({
            "prestige_level": snapshot.prestige_level,
            "awards": awards,
        })
        .to_string(),
    );
    Ok(())
}

fn stat_boss_awards(snapshot: &DigBossRuntimeSnapshot) -> Result<BTreeSet<i32>, String> {
    let Some(raw) = snapshot
        .stat_boss_awards_json
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    else {
        return Ok(BTreeSet::new());
    };
    let decoded: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let values = if let Some(object) = decoded.as_object() {
        if object
            .get("prestige_level")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            != snapshot.prestige_level
        {
            return Ok(BTreeSet::new());
        }
        object.get("awards").and_then(Value::as_array)
    } else if snapshot.prestige_level == 0 {
        decoded.as_array()
    } else {
        None
    };
    Ok(values
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .filter_map(|value| i32::try_from(value).ok())
        .collect())
}

fn boss_audit_detail(audit: &BossAuditRecord, commit: &RepositoryCommit) -> String {
    json!({
        "boss_id": audit.boss_id,
        "boundary": audit.boundary,
        "won": audit.won,
        "jc_delta": audit.jc_delta,
        "depth_after": audit.depth_after,
        "vanity_tax": commit.vanity_tax,
        "low_priority_tax": commit.low_priority_tax,
    })
    .to_string()
}

fn paused_duel_row(
    key: PlayerKey,
    duel: &PausedBossDuel,
    now: i64,
) -> Result<DigBossPausedDuelRow, String> {
    let round_log_json = serde_json::to_string(
        &duel
            .round_log
            .iter()
            .map(round_record_json)
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let pending_prompt_json = Some(prompt_json(&duel.pending_prompt).to_string());
    let status_effects_json = json!({
        "skip_next_round_for": combatant_name(duel.combat.effects.skip_next_round_for),
        "burn_rounds_remaining": duel.combat.effects.burn_rounds_remaining,
        "bleed_rounds_remaining": duel.combat.effects.bleed_rounds_remaining,
        "silenced_next_round": duel.combat.effects.silenced_next_round,
        "frostbite_next_round": duel.combat.effects.frostbite_next_round,
        "boss_exposed_next_round": duel.combat.effects.boss_exposed_next_round,
        "pet_boss_assist": duel.combat.effects.pet_assist.as_ref().map(|assist| json!({
            "pet_name": assist.pet_name,
            "species_id": assist.species_id,
            "species_name": assist.species_name,
            "bonus_pct": assist.bonus_percent,
        })),
        "shifting_idol_bonus": duel.combat.effects.shifting_idol_bonus,
        "boss_prep": duel.combat.effects.boss_preparation.as_ref().map(|preparation| json!({
            "item_type": preparation.item_type,
            "used": preparation.used,
        })),
        "trophy_start_hp": duel.combat.effects.trophy_start_hp,
        "trophy_venom": duel.combat.effects.trophy_venom_rounds,
        "trophy_lifesteal": duel.combat.effects.trophy_lifesteal,
        "trophy_regrowth": duel.combat.effects.trophy_regrowth,
        "trophy_forewarned": duel.combat.effects.trophy_forewarned,
        "trophy_laststand": duel.combat.effects.trophy_laststand,
        "relic_berserk_rage": duel.combat.effects.relic_berserk_rage,
        "relic_double_hit": duel.combat.effects.relic_double_hit,
        "relic_deaths_door": duel.combat.effects.relic_deaths_door,
        "relic_paper_crane": duel.combat.effects.relic_paper_crane,
        "relic_bottled_quake": duel.combat.effects.relic_bottled_quake,
        "gear_reflect_first_hit": duel.combat.effects.gear_reflect_first_hit,
        "gear_block_first_status": duel.combat.effects.gear_block_first_status,
        "gear_springheel_counter": duel.combat.effects.gear_springheel_counter,
        "gear_block_first_skip": duel.combat.effects.gear_block_first_skip,
        "gear_heal_first_crit": duel.combat.effects.gear_heal_first_crit,
        "gear_reroll_first_miss": duel.combat.effects.gear_reroll_first_miss,
        "paper_crane_blocked": duel.combat.effects.paper_crane_blocked.map(status_effect_name),
        "nullweave_blocked": duel.combat.effects.nullweave_blocked.map(status_effect_name),
        "anchor_boots_blocked": duel.combat.effects.anchor_boots_blocked,
        "attempts_this_fight": duel.attempts_this_fight,
        "initial_win_chance": duel.initial_win_chance,
        "multiplier": duel.payout_multiplier,
        "player_hp_max": duel.player_hp_max,
        "boss_hp_max": duel.boss_hp_max,
        "starting_boss_hp": duel.starting_boss_hp,
        "gear_snapshot_ids": duel.gear_snapshot.gear_ids,
        "armor_snapshot_id": duel.gear_snapshot.armor_id,
        "forced_no_wager_phase": duel.forced_no_wager_phase,
    })
    .to_string();
    Ok(DigBossPausedDuelRow {
        key: database_key(key),
        boss_id: duel.boss_id.clone(),
        boundary: i64::from(duel.boundary),
        mechanic_id: duel.mechanic_id.clone(),
        risk_tier: risk_name(duel.risk_tier).to_owned(),
        wager: duel.wager,
        player_hp: i64::from(duel.combat.player_hp),
        boss_hp: i64::from(duel.combat.boss_hp),
        round_num: i64::from(duel.round_num),
        round_log_json,
        pending_prompt_json,
        rng_state: String::new(),
        status_effects_json,
        echo_applied: duel.echo_applied,
        echo_killer_id: duel.echo_killer_id,
        player_hit: duel.combat.player_hit,
        player_damage: i64::from(duel.combat.player_damage),
        boss_hit: duel.combat.boss_hit,
        boss_damage: i64::from(duel.combat.boss_damage),
        critical_chance: duel.combat.critical_chance,
        critical_bonus: i64::from(duel.combat.critical_bonus),
        created_at: now,
        last_interaction_at: now,
    })
}

fn paused_duel_from_row(row: &DigBossPausedDuelRow) -> Result<PausedBossDuel, String> {
    let mechanic = runtime_mechanic_by_id(&row.mechanic_id)
        .ok_or_else(|| format!("paused duel has unknown mechanic {}", row.mechanic_id))?;
    let status: Value =
        serde_json::from_str(&row.status_effects_json).unwrap_or_else(|_| json!({}));
    let status = status.as_object().cloned().unwrap_or_default();
    let effects = CombatEffects {
        skip_next_round_for: parse_combatant(status.get("skip_next_round_for")),
        burn_rounds_remaining: json_u8(status.get("burn_rounds_remaining")),
        bleed_rounds_remaining: json_u8(status.get("bleed_rounds_remaining")),
        silenced_next_round: json_bool(status.get("silenced_next_round")),
        frostbite_next_round: json_bool(status.get("frostbite_next_round")),
        boss_exposed_next_round: json_bool(status.get("boss_exposed_next_round")),
        pet_assist: parse_pet_assist(status.get("pet_boss_assist")),
        shifting_idol_bonus: status
            .get("shifting_idol_bonus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        boss_preparation: parse_boss_preparation(status.get("boss_prep")),
        trophy_start_hp: json_i32(status.get("trophy_start_hp")),
        trophy_venom_rounds: json_u8(status.get("trophy_venom")),
        trophy_lifesteal: json_bool(status.get("trophy_lifesteal")),
        trophy_regrowth: json_bool(status.get("trophy_regrowth")),
        trophy_forewarned: json_bool(status.get("trophy_forewarned")),
        trophy_laststand: json_bool(status.get("trophy_laststand")),
        relic_berserk_rage: status
            .get("relic_berserk_rage")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
        relic_double_hit: json_bool(status.get("relic_double_hit")),
        relic_deaths_door: json_bool(status.get("relic_deaths_door")),
        relic_paper_crane: json_bool(status.get("relic_paper_crane")),
        relic_bottled_quake: json_bool(status.get("relic_bottled_quake")),
        gear_reflect_first_hit: json_bool(status.get("gear_reflect_first_hit")),
        gear_block_first_status: json_bool(status.get("gear_block_first_status")),
        gear_springheel_counter: json_bool(status.get("gear_springheel_counter")),
        gear_block_first_skip: json_bool(status.get("gear_block_first_skip")),
        gear_heal_first_crit: json_bool(status.get("gear_heal_first_crit")),
        gear_reroll_first_miss: json_bool(status.get("gear_reroll_first_miss")),
        paper_crane_blocked: parse_status_effect(status.get("paper_crane_blocked")),
        nullweave_blocked: parse_status_effect(status.get("nullweave_blocked")),
        anchor_boots_blocked: json_bool(status.get("anchor_boots_blocked")),
    };
    let prompt = row
        .pending_prompt_json
        .as_deref()
        .and_then(parse_prompt)
        .unwrap_or_else(|| PendingPrompt::from(&mechanic));
    Ok(PausedBossDuel {
        boss_id: row.boss_id.clone(),
        boundary: narrow_i32(row.boundary, "duel boundary")?,
        mechanic_id: row.mechanic_id.clone(),
        risk_tier: parse_risk(&row.risk_tier)?,
        wager: row.wager,
        combat: CombatState {
            player_hp: narrow_i32(row.player_hp, "duel player_hp")?,
            boss_hp: narrow_i32(row.boss_hp, "duel boss_hp")?,
            player_hit: row.player_hit,
            player_damage: narrow_i32(row.player_damage, "duel player_damage")?,
            boss_hit: row.boss_hit,
            boss_damage: narrow_i32(row.boss_damage, "duel boss_damage")?,
            critical_chance: row.critical_chance,
            critical_bonus: narrow_i32(row.critical_bonus, "duel critical_bonus")?,
            effects,
        },
        round_num: u8::try_from(row.round_num)
            .map_err(|_| "paused duel round_num is outside u8 range".to_owned())?,
        round_log: parse_round_log(&row.round_log_json),
        pending_prompt: prompt,
        attempts_this_fight: json_i32(status.get("attempts_this_fight")).unwrap_or_default(),
        initial_win_chance: json_f64(status.get("initial_win_chance"), 0.5),
        payout_multiplier: json_f64(status.get("multiplier"), 1.0),
        player_hp_max: json_i32(status.get("player_hp_max"))
            .unwrap_or_else(|| narrow_i32(row.player_hp, "duel player_hp").unwrap_or(1))
            .max(1),
        boss_hp_max: json_i32(status.get("boss_hp_max"))
            .unwrap_or_else(|| narrow_i32(row.boss_hp, "duel boss_hp").unwrap_or(1))
            .max(1),
        starting_boss_hp: json_i32(status.get("starting_boss_hp"))
            .unwrap_or_else(|| narrow_i32(row.boss_hp, "duel boss_hp").unwrap_or(1)),
        gear_snapshot: GearSnapshot {
            gear_ids: status
                .get("gear_snapshot_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect(),
            armor_id: status.get("armor_snapshot_id").and_then(Value::as_i64),
        },
        forced_no_wager_phase: json_bool(status.get("forced_no_wager_phase")),
        echo_applied: row.echo_applied,
        echo_killer_id: row.echo_killer_id,
    })
}

fn runtime_mechanic_by_id(mechanic_id: &str) -> Option<MechanicDefinition> {
    mechanic_by_id(mechanic_id).or_else(|| {
        let mechanic = pinnacle_mechanic(mechanic_id)?;
        Some(MechanicDefinition {
            id: mechanic.mechanic_id.to_owned(),
            archetype: "pinnacle".to_owned(),
            trigger_round: mechanic.trigger_round,
            prompt_title: mechanic.mechanic_id.replace('_', " "),
            prompt_description: "The pinnacle shifts the shape of the fight.".to_owned(),
            options: mechanic
                .options
                .into_iter()
                .map(|option| CoreMechanicOption {
                    label: option.label.to_owned(),
                    flavor: option.flavor.to_owned(),
                    outcome_rolls: option
                        .outcome_rolls
                        .into_iter()
                        .map(|outcome| CoreOutcomeRoll {
                            probability: outcome.probability,
                            player_hp_delta: outcome.player_hp_delta,
                            boss_hp_delta: outcome.boss_hp_delta,
                            skip_next_round_for: outcome.skip_next_round_for.map(|combatant| {
                                match combatant {
                                    crate::dig_bosses::Combatant::Player => Combatant::Player,
                                    crate::dig_bosses::Combatant::Boss => Combatant::Boss,
                                }
                            }),
                            status_effect: outcome.status_effect.map(|effect| match effect {
                                crate::dig_bosses::StatusEffect::Bleed => {
                                    MechanicStatusEffect::Bleed
                                }
                                crate::dig_bosses::StatusEffect::Reveal => {
                                    MechanicStatusEffect::Reveal
                                }
                                crate::dig_bosses::StatusEffect::Silence => {
                                    MechanicStatusEffect::Silence
                                }
                            }),
                            narrative: option.flavor.to_owned(),
                        })
                        .collect(),
                })
                .collect(),
            safe_option_index: mechanic.safe_option_index,
        })
    })
}

fn round_record_json(record: &RoundRecord) -> Value {
    let mut value = json!({
        "round": record.round,
        "player_hp": record.player_hp,
        "boss_hp": record.boss_hp,
        "player_hit": record.player_hit,
        "boss_hit": record.boss_hit,
        "pet_assist": record.pet_assist.as_ref().map(|assist| json!({
            "pet_name": assist.pet_name,
            "species_id": assist.species_id,
            "species_name": assist.species_name,
            "bonus_pct": assist.bonus_percent,
        })),
        "pet_assist_damage": record.pet_assist_damage,
        "crit": record.effect_log.critical,
    });
    let object = value
        .as_object_mut()
        .expect("authored round record JSON is an object");
    for (key, enabled) in [
        ("venom", record.effect_log.venom),
        ("deaths_door", record.effect_log.deaths_door),
        ("bottled_quake", record.effect_log.bottled_quake),
        ("laststand", record.effect_log.laststand),
        ("double_hit", record.effect_log.double_hit),
        ("blood_locket_heal", record.effect_log.blood_locket_heal),
        ("lifesteal", record.effect_log.lifesteal),
        ("forewarned", record.effect_log.forewarned),
        ("briarplate_reflect", record.effect_log.briarplate_reflect),
        ("springheel_counter", record.effect_log.springheel_counter),
        ("red_thread_reroll", record.effect_log.red_thread_reroll),
        ("regrowth", record.effect_log.regrowth),
        ("skipped_player", record.effect_log.skipped_player),
        ("skipped_boss", record.effect_log.skipped_boss),
    ] {
        if enabled {
            object.insert(key.to_owned(), Value::Bool(true));
        }
    }
    if record.effect_log.berserk_bonus > 0 {
        object.insert(
            "berserk_bonus".to_owned(),
            Value::from(record.effect_log.berserk_bonus),
        );
    }
    if let Some(mechanic) = &record.mechanic {
        object.insert(
            "mechanic_id".to_owned(),
            Value::String(mechanic.mechanic_id.clone()),
        );
        object.insert("option_idx".to_owned(), Value::from(mechanic.option_index));
        object.insert(
            "option_label".to_owned(),
            Value::String(mechanic.option_label.clone()),
        );
        object.insert(
            "narrative".to_owned(),
            Value::String(mechanic.narrative.clone()),
        );
        if mechanic.warding_salts_blocked {
            object.insert("warding_salts_blocked".to_owned(), Value::Bool(true));
        }
    }
    value
}

fn parse_round_log(raw: &str) -> Vec<RoundRecord> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            Some(RoundRecord {
                round: object
                    .get("round")
                    .and_then(Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok())?,
                player_hp: json_i32(object.get("player_hp")).unwrap_or_default(),
                boss_hp: json_i32(object.get("boss_hp")).unwrap_or_default(),
                player_hit: json_bool(object.get("player_hit")),
                boss_hit: object.get("boss_hit").and_then(Value::as_bool),
                pet_assist: parse_pet_assist(object.get("pet_assist")),
                pet_assist_damage: json_i32(object.get("pet_assist_damage")).unwrap_or_default(),
                effect_log: RoundEffectLog {
                    critical: json_bool(object.get("crit")),
                    venom: json_bool(object.get("venom")),
                    deaths_door: json_bool(object.get("deaths_door")),
                    bottled_quake: json_bool(object.get("bottled_quake")),
                    berserk_bonus: json_u8(object.get("berserk_bonus")),
                    laststand: json_bool(object.get("laststand")),
                    double_hit: json_bool(object.get("double_hit")),
                    blood_locket_heal: json_bool(object.get("blood_locket_heal")),
                    lifesteal: json_bool(object.get("lifesteal")),
                    forewarned: json_bool(object.get("forewarned")),
                    briarplate_reflect: json_bool(object.get("briarplate_reflect")),
                    springheel_counter: json_bool(object.get("springheel_counter")),
                    red_thread_reroll: json_bool(object.get("red_thread_reroll")),
                    regrowth: json_bool(object.get("regrowth")),
                    skipped_player: json_bool(object.get("skipped_player")),
                    skipped_boss: json_bool(object.get("skipped_boss")),
                },
                mechanic: object
                    .get("mechanic_id")
                    .and_then(Value::as_str)
                    .map(|mechanic_id| MechanicRoundRecord {
                        mechanic_id: mechanic_id.to_owned(),
                        option_index: object
                            .get("option_idx")
                            .and_then(Value::as_u64)
                            .and_then(|value| usize::try_from(value).ok())
                            .unwrap_or_default(),
                        option_label: object
                            .get("option_label")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        narrative: object
                            .get("narrative")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        warding_salts_blocked: json_bool(object.get("warding_salts_blocked")),
                    }),
            })
        })
        .collect()
}

fn prompt_json(prompt: &PendingPrompt) -> Value {
    json!({
        "mechanic_id": prompt.mechanic_id,
        "title": prompt.prompt_title,
        "description": prompt.prompt_description,
        "options": prompt.options.iter().map(|option| json!({
            "option_index": option.option_index,
            "label": option.label,
        })).collect::<Vec<_>>(),
        "safe_option_idx": prompt.safe_option_index,
    })
}

fn parse_prompt(raw: &str) -> Option<PendingPrompt> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let options = object
        .get("options")?
        .as_array()?
        .iter()
        .enumerate()
        .filter_map(|(index, option)| {
            let option = option.as_object()?;
            Some(PromptOption {
                option_index: option
                    .get("option_index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(index),
                label: option.get("label")?.as_str()?.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    Some(PendingPrompt {
        mechanic_id: object.get("mechanic_id")?.as_str()?.to_owned(),
        prompt_title: object
            .get("title")
            .or_else(|| object.get("prompt_title"))?
            .as_str()?
            .to_owned(),
        prompt_description: object
            .get("description")
            .or_else(|| object.get("prompt_description"))?
            .as_str()?
            .to_owned(),
        options,
        safe_option_index: object
            .get("safe_option_idx")
            .or_else(|| object.get("safe_option_index"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_default(),
    })
}

fn combatant_name(combatant: Option<Combatant>) -> Option<&'static str> {
    match combatant {
        Some(Combatant::Player) => Some("player"),
        Some(Combatant::Boss) => Some("boss"),
        None => None,
    }
}

fn parse_combatant(value: Option<&Value>) -> Option<Combatant> {
    match value.and_then(Value::as_str) {
        Some("player") => Some(Combatant::Player),
        Some("boss") => Some(Combatant::Boss),
        _ => None,
    }
}

fn status_effect_name(effect: MechanicStatusEffect) -> &'static str {
    match effect {
        MechanicStatusEffect::Burn => "burn",
        MechanicStatusEffect::Silence => "silence",
        MechanicStatusEffect::Bleed => "bleed",
        MechanicStatusEffect::Frostbite => "frostbite",
        MechanicStatusEffect::Reveal => "reveal",
    }
}

fn parse_status_effect(value: Option<&Value>) -> Option<MechanicStatusEffect> {
    match value.and_then(Value::as_str) {
        Some("burn") => Some(MechanicStatusEffect::Burn),
        Some("silence") => Some(MechanicStatusEffect::Silence),
        Some("bleed") => Some(MechanicStatusEffect::Bleed),
        Some("frostbite") => Some(MechanicStatusEffect::Frostbite),
        Some("reveal") => Some(MechanicStatusEffect::Reveal),
        _ => None,
    }
}

fn parse_pet_assist(value: Option<&Value>) -> Option<crate::boss_duel::PetAssist> {
    let object = value?.as_object()?;
    Some(crate::boss_duel::PetAssist {
        pet_name: object.get("pet_name")?.as_str()?.to_owned(),
        species_id: object.get("species_id")?.as_str()?.to_owned(),
        species_name: object.get("species_name")?.as_str()?.to_owned(),
        bonus_percent: json_i32(
            object
                .get("bonus_pct")
                .or_else(|| object.get("bonus_percent")),
        )?,
    })
}

fn parse_boss_preparation(value: Option<&Value>) -> Option<BossPreparationState> {
    let object = value?.as_object()?;
    Some(BossPreparationState {
        item_type: object.get("item_type")?.as_str()?.to_owned(),
        used: object.get("used").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn clear_carried_wager_json(
    raw: Option<&str>,
    boundary: i32,
) -> Result<Option<String>, DigBossRuntimeError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
    let boundary = boundary.to_string();
    if let Some(entry) = value
        .as_object_mut()
        .and_then(|root| root.get_mut(&boundary))
        .and_then(Value::as_object_mut)
    {
        entry.remove("carried_wager");
        entry.remove("carried_risk_tier");
    }
    serde_json::to_string(&value)
        .map(Some)
        .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))
}

fn gear_snapshot_from_paused_row(row: &DigBossPausedDuelRow) -> DigBossGearSnapshot {
    let status: Value =
        serde_json::from_str(&row.status_effects_json).unwrap_or_else(|_| json!({}));
    let gear_ids = status
        .get("gear_snapshot_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect();
    let armor_id = status.get("armor_snapshot_id").and_then(Value::as_i64);
    DigBossGearSnapshot { gear_ids, armor_id }
}

fn risk_name(risk: RiskTier) -> &'static str {
    match risk {
        RiskTier::Cautious => "cautious",
        RiskTier::Bold => "bold",
        RiskTier::Reckless => "reckless",
    }
}

fn parse_risk(value: &str) -> Result<RiskTier, String> {
    match value {
        "cautious" => Ok(RiskTier::Cautious),
        "bold" => Ok(RiskTier::Bold),
        "reckless" => Ok(RiskTier::Reckless),
        _ => Err(format!("paused duel has invalid risk tier {value}")),
    }
}

fn json_u8(value: Option<&Value>) -> u8 {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or_default()
}

fn json_bool(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn json_f64(value: Option<&Value>, default: f64) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(default)
}

/// Runtime hook for the gateway-maintained vanity eligibility cache.
pub trait DigBossVanityTaxPort: Send + Sync {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64;
}

impl DigBossVanityTaxPort for VanityTaxService {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64 {
        self.calculate_tax(discord_id, Some(guild_id), gross_profit)
    }
}

impl DigBossVanityTaxPort for PersistentVanityTaxService {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64 {
        self.calculate_tax(discord_id, guild_id, gross_profit)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDigBossVanityTax;

impl DigBossVanityTaxPort for NoDigBossVanityTax {
    fn calculate_tax(&self, _discord_id: i64, _guild_id: i64, _gross_profit: i64) -> i64 {
        0
    }
}

/// Separately accounted sibling of [`DigBossVanityTaxPort`] for players in
/// active low priority. Both sinks read the same gross boss winnings.
pub trait DigBossLowPriorityTaxPort: Send + Sync {
    fn calculate_tax(&self, discord_id: i64, guild_id: i64, gross_profit: i64) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDigBossLowPriorityTax;

impl DigBossLowPriorityTaxPort for NoDigBossLowPriorityTax {
    fn calculate_tax(&self, _discord_id: i64, _guild_id: i64, _gross_profit: i64) -> i64 {
        0
    }
}

/// Amount the profit policy withheld for low priority on the settlement in
/// flight.
///
/// The multi-tier commit contract carries the vanity amount only, so the
/// profit policy and the repository adapter share this handle to keep the
/// gross credit and the separate low-priority debit in one transaction. A
/// victory settlement always taxes immediately before its commit, and the
/// commit consumes the value, so no settlement can inherit another's.
struct SqliteBossEconomyEvent {
    service: SqliteEconomyEventService,
    now: i64,
    warnings: Vec<String>,
}

impl SqliteBossEconomyEvent {
    fn new(path: &Path, config: EconomyEventConfig, now: i64) -> Self {
        Self {
            service: SqliteEconomyEventService::new(path, config),
            now,
            warnings: Vec::new(),
        }
    }
}

impl EconomyEventPort for SqliteBossEconomyEvent {
    fn adjust_reward(&mut self, guild_id: GuildId, amount: i64) -> i64 {
        match self.service.adjust_reward_at(guild_id.0, amount, self.now) {
            Ok(adjusted) => adjusted,
            Err(error) => {
                self.warnings.push(error.to_string());
                amount
            }
        }
    }
}

struct SqliteBossProfitPolicy {
    bankruptcy: BankruptcyService<BankruptcyRepository>,
    vanity_tax: Arc<dyn DigBossVanityTaxPort>,
    low_priority_tax: Arc<dyn DigBossLowPriorityTaxPort>,
    warnings: Vec<String>,
}

impl std::fmt::Debug for SqliteBossProfitPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteBossProfitPolicy")
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl SqliteBossProfitPolicy {
    fn new(
        path: &Path,
        vanity_tax: Arc<dyn DigBossVanityTaxPort>,
        low_priority_tax: Arc<dyn DigBossLowPriorityTaxPort>,
    ) -> Self {
        Self {
            bankruptcy: BankruptcyService::new(
                BankruptcyRepository::new(path),
                BankruptcyPolicy::default(),
            )
            .expect("the default bankruptcy policy is valid"),
            vanity_tax,
            low_priority_tax,
            warnings: Vec::new(),
        }
    }
}

impl BankruptcyPort for SqliteBossProfitPolicy {
    fn apply_penalty(&mut self, key: PlayerKey, positive_winnings: i64) -> BankruptcyAdjustment {
        let penalty = match self.bankruptcy.apply_penalty_to_winnings(
            key.player_id,
            positive_winnings,
            Some(key.guild_id.0),
        ) {
            Ok(penalty) => penalty,
            Err(error) => {
                self.warnings.push(error.to_string());
                crate::bankruptcy::PenaltyResult {
                    original: positive_winnings,
                    penalized: positive_winnings,
                    penalty_applied: 0,
                }
            }
        };
        let vanity_tax = self
            .vanity_tax
            .calculate_tax(key.player_id, key.guild_id.0, positive_winnings)
            .clamp(0, penalty.penalized.max(0));
        // The low-priority sink reads the same gross winnings, so the two
        // taxes stack additively rather than compounding. Clamping to what the
        // bankruptcy penalty and the vanity tax left keeps the combined
        // withholding at or below the profit basis.
        let low_priority_tax = self
            .low_priority_tax
            .calculate_tax(key.player_id, key.guild_id.0, positive_winnings)
            .clamp(0, penalty.penalized.max(0).saturating_sub(vanity_tax));
        BankruptcyAdjustment {
            penalized: penalty
                .penalized
                .saturating_sub(vanity_tax)
                .saturating_sub(low_priority_tax),
            penalty_applied: penalty.penalty_applied,
            vanity_tax,
            low_priority_tax,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct DigBossOutcomeCollector {
    notices: Vec<BossNotice>,
}

impl BossOutcomeSinkPort for DigBossOutcomeCollector {
    fn emit(&mut self, notice: BossNotice) {
        self.notices.push(notice);
    }
}

type ComposedBossService<E> = BossMultiTierService<
    SqliteBossRepository,
    E,
    FixedClock,
    SqliteBossEconomyEvent,
    SqliteBossProfitPolicy,
    SqliteBossGearRepair,
    DigBossOutcomeCollector,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigBossRuntimeRequest {
    pub discord_id: i64,
    pub guild_id: i64,
    pub now: i64,
}

impl DigBossRuntimeRequest {
    #[must_use]
    pub const fn player_key(self) -> PlayerKey {
        PlayerKey::new(self.discord_id, Some(self.guild_id))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigBossRuntimeResult<T> {
    pub outcome: T,
    pub action_id: Option<i64>,
    pub abandoned_action_id: Option<i64>,
    pub abandoned_wager_forfeit: i64,
    pub abandoned_gear_wear: GearWearResult,
    pub gear_drop: Option<DigBossGearDrop>,
    pub prestige_relic_drop: Option<DigBossPrestigeRelicDrop>,
    pub broken_gear: Vec<DigBossBrokenGear>,
    /// Fail-soft post-policy integrations (daily event, bankruptcy lookup, or
    /// post-resolution gear wear) are explicit instead of silently erased.
    pub warnings: Vec<String>,
    pub notices: Vec<BossNotice>,
}

impl<T> DigBossRuntimeResult<T> {
    fn map_outcome<U>(self, map: impl FnOnce(T) -> U) -> DigBossRuntimeResult<U> {
        DigBossRuntimeResult {
            outcome: map(self.outcome),
            action_id: self.action_id,
            abandoned_action_id: self.abandoned_action_id,
            abandoned_wager_forfeit: self.abandoned_wager_forfeit,
            abandoned_gear_wear: self.abandoned_gear_wear,
            gear_drop: self.gear_drop,
            prestige_relic_drop: self.prestige_relic_drop,
            broken_gear: self.broken_gear,
            warnings: self.warnings,
            notices: self.notices,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossGearDrop {
    pub gear_id: i64,
    pub slot: DomainGearSlot,
    pub tier: u8,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossPrestigeRelicDrop {
    pub database_id: i64,
    pub artifact_id: String,
    pub name: String,
    pub rarity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossBrokenGear {
    pub gear_id: i64,
    pub name: String,
}

/// Durable read model for the Discord encounter view. Building this value
/// also performs Python's idempotent boss lock and first-meet transition, so
/// the transport never reads or mutates `boss_progress` itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigBossEncounterInfo {
    pub boundary: i32,
    pub boss_id: String,
    pub boss_name: String,
    pub dialogue: String,
    pub is_pinnacle: bool,
    pub phase: u8,
    pub wager_allowed: bool,
    pub carried_wager: Option<i64>,
    pub carried_risk_tier: Option<RiskTier>,
    pub has_scout_lantern: bool,
    pub luminosity: i64,
    pub encounter_key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DigPinnacleStartOutcome {
    Paused(Box<PausedBossDuel>),
    Resolved(Box<DigPinnacleResolved>),
}

/// Unified Discord-facing start result. The provider should not rediscover
/// whether a persisted boundary is regular or pinnacle.
#[derive(Clone, Debug, PartialEq)]
pub enum DigBossStartOutcome {
    Paused(Box<PausedBossDuel>),
    RegularResolved(Box<ResolvedFight>),
    PinnacleResolved(Box<DigPinnacleResolved>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DigBossResolvedOutcome {
    Regular(Box<ResolvedFight>),
    Pinnacle(Box<DigPinnacleResolved>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DigBossScoutOutcome {
    Regular(Box<ScoutResult>),
    Pinnacle(Box<DigPinnacleScoutResult>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigBossRetreatOutcome {
    pub boundary: i32,
    pub block_loss: i32,
    pub new_depth: i32,
    pub carried_wager_forfeit: i64,
    pub actual_balance_delta: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigBossCheerOutcome {
    pub cheer_count: usize,
    pub total_boost: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigPinnacleRelicDrop {
    pub relic: PinnacleRelic,
    pub database_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigPinnacleResolved {
    pub won: bool,
    pub boss_id: String,
    pub boss_name: String,
    pub phase: u8,
    pub next_phase: u8,
    pub risk_tier: RiskTier,
    pub wager: i64,
    pub win_chance: f64,
    pub jc_delta: i64,
    /// Net positive payout after bankruptcy and vanity sinks. This mirrors
    /// Python's result["payout"] while jc_delta remains the durable balance
    /// delta used by the settlement receipt.
    pub payout: i64,
    /// Wager profit after the daily economy event, before bankruptcy/vanity
    /// sinks. Python exposes this separately from the base reward fields.
    pub wager_payout: i64,
    /// Daily-economy-adjusted authored pinnacle base reward, before the
    /// positive Dig reward multiplier is applied.
    pub gross_jc: i64,
    /// Positive Dig scaling applied to gross_jc, before wager profit and
    /// post-reward sinks are combined.
    pub scaled_base_jc: i64,
    /// Positive Dig reward multiplier used for scaled_base_jc.
    pub reward_multiplier: f64,
    pub gross_payout: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
    pub new_depth: i32,
    pub boss_hp_remaining: i32,
    pub boss_hp_max: i32,
    pub starting_boss_hp: i32,
    pub knockback: i32,
    pub round_log: Vec<RoundRecord>,
    pub gear_wear: GearWearResult,
    pub phase_event_id: Option<String>,
    pub relic_drop: Option<DigPinnacleRelicDrop>,
    pub rescue_line_used: bool,
    pub warding_salts_blocked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigPinnacleScoutRisk {
    pub win_chance: f64,
    pub free_fight_chance: f64,
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub boss_hit: f64,
    pub multiplier: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigPinnacleScoutResult {
    pub boss_id: String,
    pub boss_name: String,
    pub phase: u8,
    pub cautious: DigPinnacleScoutRisk,
    pub bold: DigPinnacleScoutRisk,
    pub reckless: DigPinnacleScoutRisk,
    pub pet_assist: Option<PetAssist>,
    pub enhanced_mechanic_ids: Vec<String>,
}

#[derive(Clone, Debug)]
struct PreparedPinnacleAttempt {
    boss_id: String,
    phase: u8,
    mechanic: MechanicDefinition,
    combat: CombatState,
    player_hp_max: i32,
    boss_hp_max: i32,
    starting_boss_hp: i32,
    attempts: i32,
    risk_tier: RiskTier,
    wager: i64,
    win_chance: f64,
    gear_snapshot: GearSnapshot,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct PinnacleSettlementInput {
    boss_id: String,
    phase: u8,
    risk_tier: RiskTier,
    wager: i64,
    won: bool,
    ending_boss_hp: i32,
    boss_hp_max: i32,
    starting_boss_hp: i32,
    win_chance: f64,
    attempts: i32,
    round_log: Vec<RoundRecord>,
    gear_wear: GearWearResult,
    boss_preparation: Option<BossPreparationState>,
    warding_salts_blocked: bool,
}

const PINNACLE_BASE_JC_REWARD: i64 = 500;
const PINNACLE_JC_PER_PRESTIGE: i64 = 100;
const PINNACLE_TIER_HIT_PENALTY: f64 = 0.06;
const FREE_FIGHT_ACCURACY_MULTIPLIER: f64 = 0.70;

#[derive(Clone, Debug)]
struct ClaimedAbandonedDuel {
    row: DigBossPausedDuelRow,
    action_id: Option<i64>,
    wager_forfeit: i64,
}

#[derive(Debug, Error)]
pub enum DigBossRuntimeError {
    #[error(transparent)]
    Policy(#[from] BossServiceError),
    #[error("Boss wagers cannot exceed {maximum} JC (received {requested}).")]
    WagerTooLarge { requested: i64, maximum: i64 },
    #[error("the active pinnacle state is invalid: {0}")]
    InvalidPinnacle(String),
    #[error("You can't cheer for yourself.")]
    SelfCheer,
    #[error("That player doesn't have a tunnel.")]
    CheerTargetMissing,
    #[error("That player is not at a boss boundary.")]
    CheerTargetNotAtBoss,
    #[error("Cheer cooldown ({remaining_seconds}s remaining).")]
    CheerCooldown { remaining_seconds: i64 },
    #[error("Boss already at full cheer boost (3/3).")]
    CheerFull,
    #[error("Dig boss infrastructure failed: {0}")]
    Infrastructure(String),
}

/// Configuration retained by the final runtime constructor. Keeping the
/// database path explicit makes the application service restart-safe and
/// prevents command adapters from opening their own SQLite connections.
#[derive(Clone, Debug)]
pub struct DigBossRuntimeConfig {
    pub database_path: PathBuf,
    pub pet_hunger_decay_per_day: i64,
    pub economy_event: EconomyEventConfig,
}

impl DigBossRuntimeConfig {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, pet_hunger_decay_per_day: i64) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            pet_hunger_decay_per_day,
            economy_event: EconomyEventConfig::default(),
        }
    }
}

/// Production regular-boss application boundary. Entropy stays injected per
/// call so tests can replay Python fixtures while the Serenity adapter can use
/// OS-backed randomness without sharing mutable RNG state across blocking jobs.
#[derive(Clone)]
pub struct DigBossRuntimeService {
    config: DigBossRuntimeConfig,
    vanity_tax: Arc<dyn DigBossVanityTaxPort>,
    low_priority_tax: Arc<dyn DigBossLowPriorityTaxPort>,
}

impl std::fmt::Debug for DigBossRuntimeService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DigBossRuntimeService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

struct BorrowedBossEntropy<'a, E>(&'a mut E);

impl<E: EntropyPort> EntropyPort for BorrowedBossEntropy<'_, E> {
    fn next_unit(&mut self) -> f64 {
        self.0.next_unit()
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        self.0.choose_index(upper_bound)
    }

    fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32 {
        self.0.inclusive_i32(minimum, maximum)
    }
}

impl DigBossRuntimeService {
    #[must_use]
    pub fn sqlite(config: DigBossRuntimeConfig) -> Self {
        Self {
            config,
            vanity_tax: Arc::new(NoDigBossVanityTax),
            low_priority_tax: Arc::new(NoDigBossLowPriorityTax),
        }
    }

    #[must_use]
    pub fn with_vanity_tax(mut self, vanity_tax: Arc<dyn DigBossVanityTaxPort>) -> Self {
        self.vanity_tax = vanity_tax;
        self
    }

    #[must_use]
    pub fn with_low_priority_tax(
        mut self,
        low_priority_tax: Arc<dyn DigBossLowPriorityTaxPort>,
    ) -> Self {
        self.low_priority_tax = low_priority_tax;
        self
    }

    #[must_use]
    pub const fn config(&self) -> &DigBossRuntimeConfig {
        &self.config
    }

    /// Lock and describe the boss currently blocking the tunnel. The returned
    /// view model is restart-stable: boss identity, carried stake, phase and
    /// first-meet state all come from the guarded SQLite aggregate.
    pub fn encounter<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        mut entropy: E,
    ) -> Result<DigBossEncounterInfo, DigBossRuntimeError> {
        let carry_repository = DigCarryWagerRepository::new(&self.config.database_path);
        let carry_key = CarryTunnelKey {
            discord_id: request.discord_id,
            guild_id: Some(request.guild_id),
        };
        let initial_carry = carry_repository
            .snapshot(carry_key)
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let boundary =
            current_boss_boundary(&initial_carry).ok_or(BossServiceError::NotAtBossBoundary)?;
        let boundary_i32 =
            narrow_i32(boundary, "boss boundary").map_err(DigBossRuntimeError::Infrastructure)?;

        let (boss_id, boss_name, is_pinnacle) = if boundary_i32 == PINNACLE_DEPTH {
            let boss_id = self.ensure_pinnacle_locked(request, &mut entropy)?;
            let boss = pinnacle_by_id(&boss_id)
                .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(boss_id.clone()))?;
            (boss_id, boss.name.to_owned(), true)
        } else {
            let mut service = self.compose(request.now, BorrowedBossEntropy(&mut entropy));
            let boss = *service.ensure_boss_locked(request.player_key(), boundary_i32)?;
            (boss.boss_id.to_owned(), boss.name.to_owned(), false)
        };

        let snapshot = DigBossRuntimeRepository::new(&self.config.database_path)
            .snapshot(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let progress = parse_boss_progress(snapshot.boss_progress_json.as_deref())
            .map_err(DigBossRuntimeError::Infrastructure)?;
        let progress_entry = progress
            .get(&boundary.to_string())
            .and_then(|value| match value {
                BossProgressValue::Entry(entry) => Some(entry),
                BossProgressValue::Legacy(_) => None,
            });
        let phase = if is_pinnacle {
            u8::try_from(snapshot.pinnacle_phase.clamp(1, 3))
                .map_err(|_| DigBossRuntimeError::InvalidPinnacle("invalid phase".to_owned()))?
        } else {
            match progress
                .get(&boundary.to_string())
                .map(BossProgressValue::status)
                .unwrap_or(BossStatus::Active)
            {
                BossStatus::PhaseOneDefeated => 2,
                BossStatus::PhaseTwoDefeated => 3,
                BossStatus::Active | BossStatus::Defeated => 1,
            }
        };
        let display_name = if is_pinnacle {
            let pinnacle = pinnacle_by_id(&boss_id)
                .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(boss_id.clone()))?;
            pinnacle.phases[usize::from(phase - 1)].title.to_owned()
        } else if let Some(definition) = regular_phase(&boss_id, phase) {
            definition.title.to_owned()
        } else {
            boss_name
        };
        let dialogue = encounter_dialogue(
            &boss_id,
            phase,
            is_pinnacle,
            progress_entry,
            &snapshot,
            &mut entropy,
        );
        self.mark_encounter_seen(request, boundary_i32, &boss_id)?;

        let refreshed_carry = carry_repository
            .snapshot(carry_key)
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let carried = active_pinnacle_carry(&refreshed_carry, boundary);
        let (lantern_ids, has_great_lantern) =
            DigBossRuntimeRepository::new(&self.config.database_path)
                .lantern_state(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
        let encounter_key = boss_progress_object(snapshot.boss_progress_json.as_deref())
            .get(&if is_pinnacle {
                pinnacle_phase_key(phase)
            } else {
                boundary.to_string()
            })
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("last_engaged_at"))
            .and_then(Value::as_i64)
            .map_or_else(String::new, |value| value.to_string());
        Ok(DigBossEncounterInfo {
            boundary: boundary_i32,
            boss_id,
            boss_name: display_name,
            dialogue,
            is_pinnacle,
            phase,
            wager_allowed: is_pinnacle
                || regular_boss_wager_allowed(
                    &progress,
                    boundary_i32,
                    narrow_i32(snapshot.prestige_level, "prestige level")
                        .map_err(DigBossRuntimeError::Infrastructure)?,
                ),
            carried_wager: carried.map(|value| value.wager),
            carried_risk_tier: carried.map(|value| value.risk_tier),
            has_scout_lantern: has_great_lantern || !lantern_ids.is_empty(),
            luminosity: snapshot.luminosity.clamp(0, 100),
            encounter_key,
        })
    }

    /// Start the current encounter through the correct regular/pinnacle
    /// policy graph. This is the only start seam needed by Discord modals and
    /// carried-wager buttons.
    pub fn start<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        risk_tier: RiskTier,
        wager: i64,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<DigBossStartOutcome>, DigBossRuntimeError> {
        if self.current_boundary(request)? == PINNACLE_DEPTH {
            self.start_pinnacle(request, risk_tier, wager, entropy)
                .map(|result| {
                    result.map_outcome(|outcome| match outcome {
                        DigPinnacleStartOutcome::Paused(paused) => {
                            DigBossStartOutcome::Paused(paused)
                        }
                        DigPinnacleStartOutcome::Resolved(resolved) => {
                            DigBossStartOutcome::PinnacleResolved(resolved)
                        }
                    })
                })
        } else {
            self.start_regular(request, risk_tier, wager, entropy)
                .map(|result| {
                    result.map_outcome(|outcome| match outcome {
                        StartBossDuelOutcome::Paused(paused) => DigBossStartOutcome::Paused(paused),
                        StartBossDuelOutcome::Resolved(resolved) => {
                            DigBossStartOutcome::RegularResolved(Box::new(resolved))
                        }
                    })
                })
        }
    }

    /// Resume the exact durable duel without the transport guessing its
    /// implementation. The row is inspected before either claim path runs.
    pub fn resume<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        option_index: usize,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<DigBossResolvedOutcome>, DigBossRuntimeError> {
        let active = DigBossRuntimeRepository::new(&self.config.database_path)
            .active_duel(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::NoActiveDuel)?;
        if active.boundary == i64::from(PINNACLE_DEPTH) {
            self.resume_pinnacle(request, option_index, entropy)
                .map(|result| {
                    result
                        .map_outcome(|outcome| DigBossResolvedOutcome::Pinnacle(Box::new(outcome)))
                })
        } else {
            self.resume_regular(request, option_index, entropy)
                .map(|result| {
                    result.map_outcome(|outcome| DigBossResolvedOutcome::Regular(Box::new(outcome)))
                })
        }
    }

    pub fn scout<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<DigBossScoutOutcome>, DigBossRuntimeError> {
        if self.current_boundary(request)? == PINNACLE_DEPTH {
            let (_, enhanced) = DigBossRuntimeRepository::new(&self.config.database_path)
                .lantern_state(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
            self.scout_pinnacle(request, enhanced, entropy)
                .map(|result| {
                    result.map_outcome(|outcome| DigBossScoutOutcome::Pinnacle(Box::new(outcome)))
                })
        } else {
            self.scout_regular(request, entropy).map(|result| {
                result.map_outcome(|outcome| DigBossScoutOutcome::Regular(Box::new(outcome)))
            })
        }
    }

    /// Atomically retreat from either a regular or pinnacle encounter. Depth,
    /// carry removal, clamped half-stake forfeit and audit share the actor CAS,
    /// matching Python's double-click/failure boundary.
    pub fn retreat<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        mut entropy: E,
    ) -> Result<DigBossRuntimeResult<DigBossRetreatOutcome>, DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        loop {
            let expected = repository
                .snapshot(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            let carry_snapshot = DigCarryWagerRepository::new(&self.config.database_path)
                .snapshot(CarryTunnelKey {
                    discord_id: request.discord_id,
                    guild_id: Some(request.guild_id),
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            let boundary = current_boss_boundary(&carry_snapshot)
                .ok_or(BossServiceError::NotAtBossBoundary)?;
            let block_loss = entropy.inclusive_i32(2, 3).clamp(2, 3);
            let carried_wager_forfeit =
                active_pinnacle_carry(&carry_snapshot, boundary).map_or(0, |carry| carry.wager / 2);
            let actual_forfeit = carried_wager_forfeit.min(expected.balance.max(0));
            let mut proposed = DigBossRuntimeState::from(&expected);
            proposed.depth = proposed.depth.saturating_sub(i64::from(block_loss)).max(0);
            proposed.balance = proposed.balance.saturating_sub(actual_forfeit);
            let mut progress = boss_progress_object(proposed.boss_progress_json.as_deref());
            let entry = progress
                .entry(boundary.to_string())
                .or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({"status":entry.as_str().unwrap_or("active")});
            }
            let entry = entry.as_object_mut().ok_or_else(|| {
                DigBossRuntimeError::Infrastructure("boss retreat entry is invalid".to_owned())
            })?;
            entry.insert(
                "last_outcome".to_owned(),
                Value::String("retreat".to_owned()),
            );
            entry.insert("first_meet_seen".to_owned(), Value::Bool(true));
            entry.remove("carried_wager");
            entry.remove("carried_risk_tier");
            proposed.boss_progress_json = Some(Value::Object(progress).to_string());
            let detail = json!({
                "boundary":boundary,
                "loss":block_loss,
                "carried_wager_forfeit":actual_forfeit,
            })
            .to_string();
            match repository
                .commit(DigBossRuntimeCommit {
                    expected: &expected,
                    proposed: &proposed,
                    audit: Some(DigBossRuntimeAudit {
                        action_type: "boss_retreat",
                        target_id: None,
                        detail_json: &detail,
                        created_at: request.now,
                    }),
                    echo: None,
                    artifact: None,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossRuntimeCommitOutcome::Applied(receipt) => {
                    return Ok(DigBossRuntimeResult {
                        outcome: DigBossRetreatOutcome {
                            boundary: narrow_i32(boundary, "boss boundary")
                                .map_err(DigBossRuntimeError::Infrastructure)?,
                            block_loss,
                            new_depth: narrow_i32(proposed.depth, "retreat depth")
                                .map_err(DigBossRuntimeError::Infrastructure)?,
                            carried_wager_forfeit: actual_forfeit,
                            actual_balance_delta: -actual_forfeit,
                        },
                        action_id: receipt.action_id,
                        abandoned_action_id: None,
                        abandoned_wager_forfeit: 0,
                        abandoned_gear_wear: GearWearResult::default(),
                        gear_drop: None,
                        prestige_relic_drop: None,
                        broken_gear: Vec::new(),
                        warnings: Vec::new(),
                        notices: Vec::new(),
                    });
                }
                DigBossRuntimeCommitOutcome::Conflict => continue,
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    pub fn cheer(
        &self,
        cheerer_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigBossCheerOutcome, DigBossRuntimeError> {
        const CHEER_COOLDOWN_SECONDS: i64 = 45;
        const CHEER_DURATION_SECONDS: i64 = 3_600;
        const MAX_CHEERS: usize = 3;
        if cheerer_id == target_id {
            return Err(DigBossRuntimeError::SelfCheer);
        }
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        loop {
            let snapshot = repository
                .cheer_snapshot(cheerer_id, target_id, guild_id)
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(DigBossRuntimeError::CheerTargetMissing)?;
            let target_carry = DigCarryWagerRepository::new(&self.config.database_path)
                .snapshot(CarryTunnelKey {
                    discord_id: target_id,
                    guild_id: Some(guild_id),
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(DigBossRuntimeError::CheerTargetMissing)?;
            if current_boss_boundary(&target_carry).is_none() {
                return Err(DigBossRuntimeError::CheerTargetNotAtBoss);
            }
            let remaining = snapshot
                .cheerer_last_cheer_at
                .map_or(0, |last| CHEER_COOLDOWN_SECONDS - now.saturating_sub(last));
            if remaining > 0 {
                return Err(DigBossRuntimeError::CheerCooldown {
                    remaining_seconds: remaining,
                });
            }
            let mut cheers = parse_active_cheers(snapshot.target_cheer_data_json.as_deref(), now);
            if cheers.len() >= MAX_CHEERS {
                return Err(DigBossRuntimeError::CheerFull);
            }
            cheers.push(json!({
                "cheerer_id":cheerer_id,
                "expires_at":now.saturating_add(CHEER_DURATION_SECONDS),
            }));
            let encoded = Value::Array(cheers).to_string();
            match repository
                .commit_cheer(DigBossCheerCommit {
                    expected: &snapshot,
                    proposed_target_cheer_data_json: &encoded,
                    cheerer_last_cheer_at: now,
                    create_cheerer_tunnel_name: &format!("Miner {cheerer_id}"),
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossCheerCommitOutcome::Applied => {
                    let cheer_count = parse_active_cheers(Some(&encoded), now).len();
                    return Ok(DigBossCheerOutcome {
                        cheer_count,
                        total_boost: (cheer_count as f64 * 0.05).min(0.15),
                    });
                }
                DigBossCheerCommitOutcome::Conflict => continue,
                DigBossCheerCommitOutcome::MissingCheerer => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
                DigBossCheerCommitOutcome::MissingTarget => {
                    return Err(DigBossRuntimeError::CheerTargetMissing);
                }
            }
        }
    }

    pub fn start_regular<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        risk: RiskTier,
        wager: i64,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<StartBossDuelOutcome>, DigBossRuntimeError> {
        let boundary = self.regular_boundary(request)?;
        let abandoned = self.claim_abandoned_duel(request, boundary)?;
        let wager = self.validate_and_resolve_start_wager(request, boundary, wager)?;
        self.activate_start_preparation(request, boundary)?;
        let abandoned_gear_wear = self.settle_abandoned_gear(request, abandoned.as_ref())?;
        let mut service = self.compose(request.now, entropy);
        let outcome = service.start_boss_duel(request.player_key(), risk, wager);
        let mut result = finish_composed(&mut service, outcome)?;
        if let StartBossDuelOutcome::Resolved(resolved) = &result.outcome {
            self.enrich_regular_resolution(
                request,
                result.action_id.is_some(),
                resolved,
                &mut result.gear_drop,
                &mut result.prestige_relic_drop,
                &mut result.broken_gear,
                &mut result.warnings,
                &mut service.entropy,
            );
        }
        if let Some(abandoned) = abandoned {
            result.abandoned_action_id = abandoned.action_id;
            result.abandoned_wager_forfeit = abandoned.wager_forfeit;
            result.abandoned_gear_wear = abandoned_gear_wear;
        }
        Ok(result)
    }

    fn enrich_regular_runtime_result<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        result: &mut DigBossRuntimeResult<ResolvedFight>,
        entropy: &mut E,
    ) {
        let committed = result.action_id.is_some();
        self.enrich_regular_resolution(
            request,
            committed,
            &result.outcome,
            &mut result.gear_drop,
            &mut result.prestige_relic_drop,
            &mut result.broken_gear,
            &mut result.warnings,
            entropy,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the post-settlement outputs remain separate typed provider fields"
    )]
    fn enrich_regular_resolution<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        committed: bool,
        resolved: &ResolvedFight,
        gear_drop: &mut Option<DigBossGearDrop>,
        prestige_relic_drop: &mut Option<DigBossPrestigeRelicDrop>,
        broken_gear: &mut Vec<DigBossBrokenGear>,
        warnings: &mut Vec<String>,
        entropy: &mut E,
    ) {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let key = database_key(request.player_key());
        match repository.gear_presentation_rows(key, &resolved.gear_wear.broken_ids) {
            Ok(rows) => {
                broken_gear.extend(rows.into_iter().map(|row| DigBossBrokenGear {
                    gear_id: row.id,
                    name: gear_presentation_name(
                        row.id,
                        &row.slot,
                        row.tier,
                        row.item_id.as_deref(),
                    ),
                }));
            }
            Err(error) => warnings.push(format!("boss broken-gear projection failed: {error}")),
        }
        if !committed || !resolved.won || resolved.phase_transition.is_some() {
            return;
        }

        if let Some(tier) = drop_tier_at_depth(i64::from(resolved.boundary))
            && entropy.next_unit() < GEAR_BOSS_DROP_RATE
        {
            const DROP_SLOTS: [DomainGearSlot; 4] = [
                DomainGearSlot::Weapon,
                DomainGearSlot::Armor,
                DomainGearSlot::Boots,
                DomainGearSlot::Amulet,
            ];
            let slot = DROP_SLOTS[entropy.choose_index(DROP_SLOTS.len()) % DROP_SLOTS.len()];
            if let Some(definition) =
                tier_table(slot).and_then(|table| table.get(usize::from(tier)))
            {
                match repository.grant_regular_gear_drop(DigBossRegularGearGrant {
                    key,
                    slot: slot.as_str(),
                    tier: i64::from(tier),
                    durability: i64::from(GEAR_MAX_DURABILITY),
                    acquired_at: request.now,
                }) {
                    Ok(gear_id) => {
                        *gear_drop = Some(DigBossGearDrop {
                            gear_id,
                            slot,
                            tier,
                            name: definition.name.to_owned(),
                        });
                    }
                    Err(error) => warnings.push(format!("boss gear drop failed: {error}")),
                }
            }
        }

        let snapshot = match repository.snapshot(key) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                warnings.push("boss prestige-relic drop lost its tunnel snapshot".to_owned());
                return;
            }
            Err(error) => {
                warnings.push(format!("boss prestige-relic snapshot failed: {error}"));
                return;
            }
        };
        let owned = match repository.owned_artifact_ids(key) {
            Ok(owned) => owned,
            Err(error) => {
                warnings.push(format!("boss prestige-relic ownership failed: {error}"));
                return;
            }
        };
        let eligible = crate::dig_loot::artifact_catalog()
            .into_iter()
            .filter(|artifact| {
                artifact.is_relic
                    && !artifact.trophy
                    && artifact.min_prestige > 0
                    && artifact.min_prestige <= snapshot.prestige_level
                    && !owned.contains(artifact.id)
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() || entropy.next_unit() >= 0.10 {
            return;
        }
        let selected = eligible[entropy.choose_index(eligible.len()) % eligible.len()];
        match repository.grant_regular_relic_drop_if_unowned(key, selected.id, request.now) {
            Ok(Some(database_id)) => {
                *prestige_relic_drop = Some(DigBossPrestigeRelicDrop {
                    database_id,
                    artifact_id: selected.id.to_owned(),
                    name: selected.name.to_owned(),
                    rarity: selected.rarity.as_str().to_owned(),
                });
            }
            Ok(None) => {}
            Err(error) => warnings.push(format!("boss prestige-relic drop failed: {error}")),
        }
    }

    pub fn resume_regular<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        option_index: usize,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<ResolvedFight>, DigBossRuntimeError> {
        let mut service = self.compose(request.now, entropy);
        let outcome = service.resume_boss_duel(request.player_key(), option_index);
        let mut result = finish_composed(&mut service, outcome)?;
        self.enrich_regular_runtime_result(request, &mut result, &mut service.entropy);
        Ok(result)
    }

    pub fn fight_regular<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        risk: RiskTier,
        wager: i64,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<ResolvedFight>, DigBossRuntimeError> {
        let mut service = self.compose(request.now, entropy);
        let outcome = service.fight_boss(request.player_key(), risk, wager);
        let mut result = finish_composed(&mut service, outcome)?;
        self.enrich_regular_runtime_result(request, &mut result, &mut service.entropy);
        Ok(result)
    }

    pub fn scout_regular<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<ScoutResult>, DigBossRuntimeError> {
        let mut service = self.compose(request.now, entropy);
        let outcome = service.scout_boss(request.player_key());
        finish_composed(&mut service, outcome)
    }

    pub fn retreat_regular<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        entropy: E,
    ) -> Result<DigBossRuntimeResult<i32>, DigBossRuntimeError> {
        let mut service = self.compose(request.now, entropy);
        let outcome = service.retreat_boss(request.player_key());
        finish_composed(&mut service, outcome)
    }

    pub fn start_pinnacle<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        risk_tier: RiskTier,
        wager: i64,
        mut entropy: E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleStartOutcome>, DigBossRuntimeError> {
        self.pinnacle_snapshot(request)?;
        let abandoned = self.claim_abandoned_duel(request, PINNACLE_DEPTH)?;
        let (risk_tier, wager) =
            self.validate_and_resolve_pinnacle_wager(request, risk_tier, wager)?;
        self.activate_start_preparation(request, PINNACLE_DEPTH)?;
        let abandoned_gear_wear = self.settle_abandoned_gear(request, abandoned.as_ref())?;
        let attempt = self.prepare_pinnacle_attempt(request, risk_tier, wager, &mut entropy)?;
        let mut result = self.run_pinnacle_attempt(request, attempt, &mut entropy)?;
        if let Some(abandoned) = abandoned {
            result.abandoned_action_id = abandoned.action_id;
            result.abandoned_wager_forfeit = abandoned.wager_forfeit;
            result.abandoned_gear_wear = abandoned_gear_wear;
        }
        Ok(result)
    }

    pub fn resume_pinnacle<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        requested_option_index: usize,
        mut entropy: E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleResolved>, DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let row = repository
            .claim_active_duel(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::NoActiveDuel)?;
        if row.boundary != i64::from(PINNACLE_DEPTH) || pinnacle_by_id(&row.boss_id).is_none() {
            return Err(DigBossRuntimeError::InvalidPinnacle(format!(
                "paused duel {} is not a pinnacle encounter",
                row.boss_id
            )));
        }
        let mut paused =
            paused_duel_from_row(&row).map_err(DigBossRuntimeError::InvalidPinnacle)?;
        let mechanic = runtime_mechanic_by_id(&paused.mechanic_id)
            .ok_or_else(|| BossServiceError::UnknownMechanic(paused.mechanic_id.clone()))?;
        let option_index = if requested_option_index < mechanic.options.len() {
            requested_option_index
        } else {
            mechanic.safe_option_index
        };
        let warding_salts_blocked =
            paused
                .combat
                .effects
                .boss_preparation
                .as_mut()
                .is_some_and(|preparation| {
                    if preparation.item_type == "warding_salts" && !preparation.used {
                        preparation.used = true;
                        true
                    } else {
                        false
                    }
                });
        let narrative = if warding_salts_blocked {
            self.mark_pinnacle_preparation_used(request)?;
            "The warding salts flare and swallow the boss mechanic.".to_owned()
        } else {
            let outcome = apply_option_outcome(
                &mechanic.options[option_index],
                paused.combat.player_hp,
                paused.combat.boss_hp,
                &paused.combat.effects,
                &mut entropy,
            );
            paused.combat.player_hp = outcome.player_hp;
            paused.combat.boss_hp = outcome.boss_hp;
            paused.combat.effects = outcome.effects;
            outcome.narrative
        };
        paused.round_log.push(RoundRecord {
            round: paused.round_num,
            player_hp: paused.combat.player_hp.max(0),
            boss_hp: paused.combat.boss_hp.max(0),
            player_hit: false,
            boss_hit: None,
            pet_assist: paused.combat.effects.pet_assist.clone(),
            pet_assist_damage: 0,
            effect_log: RoundEffectLog::default(),
            mechanic: Some(MechanicRoundRecord {
                mechanic_id: paused.mechanic_id.clone(),
                option_index,
                option_label: mechanic.options[option_index].label.clone(),
                narrative,
                warding_salts_blocked,
            }),
        });
        let mut won = (paused.combat.boss_hp <= 0).then_some(true);
        if won.is_none() && paused.combat.player_hp <= 0 {
            won = Some(false);
        }
        if won.is_none() {
            for round in paused.round_num.saturating_add(1)..=BOSS_ROUND_CAP {
                let (record, terminal) = run_combat_round(round, &mut paused.combat, &mut entropy);
                paused.round_log.push(record);
                if terminal.is_some() {
                    won = terminal;
                    break;
                }
            }
        }
        let won = won.unwrap_or(false);
        let phase = self.claimed_pinnacle_phase(request)?;
        let gear_wear = self.tick_pinnacle_gear(
            request,
            &paused.gear_snapshot,
            won,
            paused.combat.effects.boss_preparation.as_ref(),
        )?;
        self.settle_pinnacle(
            request,
            PinnacleSettlementInput {
                boss_id: paused.boss_id,
                phase,
                risk_tier: paused.risk_tier,
                wager: paused.wager,
                won,
                ending_boss_hp: paused.combat.boss_hp,
                boss_hp_max: paused.boss_hp_max,
                starting_boss_hp: paused.starting_boss_hp,
                win_chance: paused.initial_win_chance,
                attempts: paused.attempts_this_fight,
                round_log: paused.round_log,
                gear_wear,
                boss_preparation: paused.combat.effects.boss_preparation,
                warding_salts_blocked,
            },
            &mut entropy,
        )
    }

    pub fn scout_pinnacle<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        enhanced: bool,
        mut entropy: E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleScoutResult>, DigBossRuntimeError> {
        self.scout_pinnacle_with_entropy(request, enhanced, &mut entropy)
    }

    fn pinnacle_snapshot(
        &self,
        request: DigBossRuntimeRequest,
    ) -> Result<DigBossRuntimeSnapshot, DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let snapshot = repository
            .snapshot(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let carry = DigCarryWagerRepository::new(&self.config.database_path)
            .snapshot(CarryTunnelKey {
                discord_id: request.discord_id,
                guild_id: Some(request.guild_id),
            })
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        if current_boss_boundary(&carry) != Some(i64::from(PINNACLE_DEPTH)) {
            return Err(BossServiceError::NotAtBossBoundary.into());
        }
        Ok(snapshot)
    }

    /// The pinnacle phase of an already-claimed duel, read without the
    /// at-the-boundary gate.
    ///
    /// Once `claim_active_duel` has deleted the paused row, the duel is the
    /// only record of the wagered stake: failing here would drop the fight
    /// without settling it, letting a player who retreated mid-mechanic void a
    /// losing wager entirely. The claimed row is the authority, so a player who
    /// has since left the boundary still settles.
    fn claimed_pinnacle_phase(
        &self,
        request: DigBossRuntimeRequest,
    ) -> Result<u8, DigBossRuntimeError> {
        let phase = DigBossRuntimeRepository::new(&self.config.database_path)
            .snapshot(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .map_or(1, |snapshot| snapshot.pinnacle_phase);
        Self::clamped_pinnacle_phase(phase)
    }

    fn clamped_pinnacle_phase(phase: i64) -> Result<u8, DigBossRuntimeError> {
        u8::try_from(phase.clamp(1, 3))
            .map_err(|_| DigBossRuntimeError::InvalidPinnacle("phase is outside u8".to_owned()))
    }

    fn validate_and_resolve_pinnacle_wager(
        &self,
        request: DigBossRuntimeRequest,
        requested_risk: RiskTier,
        requested_wager: i64,
    ) -> Result<(RiskTier, i64), DigBossRuntimeError> {
        let key = CarryTunnelKey {
            discord_id: request.discord_id,
            guild_id: Some(request.guild_id),
        };
        loop {
            let carry_repository = DigCarryWagerRepository::new(&self.config.database_path);
            let snapshot = carry_repository
                .snapshot(key)
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            if current_boss_boundary(&snapshot) != Some(i64::from(PINNACLE_DEPTH)) {
                return Err(BossServiceError::NotAtBossBoundary.into());
            }
            if let Some(carried) = active_pinnacle_carry(&snapshot, i64::from(PINNACLE_DEPTH)) {
                return Ok((carried.risk_tier, carried.wager));
            }
            if requested_wager < 0 {
                return Err(BossServiceError::NegativeWager.into());
            }
            if requested_wager > MAX_NEW_BOSS_WAGER {
                return Err(DigBossRuntimeError::WagerTooLarge {
                    requested: requested_wager,
                    maximum: MAX_NEW_BOSS_WAGER,
                });
            }
            let resolved = self.resolve_new_pinnacle_wager(request, requested_wager)?;
            let has_stale_markers = snapshot
                .entry(i64::from(PINNACLE_DEPTH))
                .is_some_and(cama_db::dig_carry_wager::CarryProgressEntry::has_either_marker);
            if !has_stale_markers {
                return Ok((requested_risk, resolved));
            }
            match carry_repository
                .clear_carry_markers(&snapshot, i64::from(PINNACLE_DEPTH))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                cama_db::dig_carry_wager::ClearCarryOutcome::Cleared { .. } => {
                    return Ok((requested_risk, resolved));
                }
                cama_db::dig_carry_wager::ClearCarryOutcome::Conflict => continue,
                cama_db::dig_carry_wager::ClearCarryOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    fn resolve_new_pinnacle_wager(
        &self,
        request: DigBossRuntimeRequest,
        wager: i64,
    ) -> Result<i64, DigBossRuntimeError> {
        if wager == 0 {
            return Ok(0);
        }
        loop {
            let mut repository = SqliteBossRepository::new(&self.config.database_path, request.now);
            let tunnel = repository
                .load_tunnel(request.player_key())
                .ok_or(BossServiceError::MissingTunnel)?;
            let cursed = tunnel.stinger_curse.contains(CurseKind::HalveNextWager);
            let effective = if cursed { wager / 2 + wager % 2 } else { wager };
            if tunnel.balance < effective {
                return Err(BossServiceError::InsufficientBalance {
                    wager: effective,
                    balance: tunnel.balance,
                }
                .into());
            }
            if !cursed {
                return Ok(effective);
            }
            let mut next = tunnel.clone();
            next.stinger_curse.consume(CurseKind::HalveNextWager);
            match repository.commit(
                request.player_key(),
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => return Ok(effective),
                Err(RepositoryError::Conflict) => continue,
                Err(error) => {
                    return Err(DigBossRuntimeError::Infrastructure(
                        repository.take_error().unwrap_or_else(|| error.to_string()),
                    ));
                }
            }
        }
    }

    fn ensure_pinnacle_locked<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        entropy: &mut E,
    ) -> Result<String, DigBossRuntimeError> {
        let selected = PINNACLE_POOL_IDS[entropy.choose_index(PINNACLE_POOL_IDS.len())];
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        loop {
            let snapshot = self.pinnacle_snapshot(request)?;
            if let Some(existing) = snapshot
                .pinnacle_boss_id
                .as_deref()
                .filter(|boss_id| pinnacle_by_id(boss_id).is_some())
            {
                return Ok(existing.to_owned());
            }
            let mut proposed = DigBossRuntimeState::from(&snapshot);
            proposed.pinnacle_boss_id = Some(selected.to_owned());
            proposed.pinnacle_phase = 1;
            match repository
                .commit(DigBossRuntimeCommit {
                    expected: &snapshot,
                    proposed: &proposed,
                    audit: None,
                    echo: None,
                    artifact: None,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossRuntimeCommitOutcome::Applied(_) => return Ok(selected.to_owned()),
                DigBossRuntimeCommitOutcome::Conflict => continue,
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    fn mark_pinnacle_preparation_used(
        &self,
        request: DigBossRuntimeRequest,
    ) -> Result<(), DigBossRuntimeError> {
        loop {
            let mut repository = SqliteBossRepository::new(&self.config.database_path, request.now);
            let tunnel = repository
                .load_tunnel(request.player_key())
                .ok_or(BossServiceError::MissingTunnel)?;
            match repository.commit(
                request.player_key(),
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: tunnel.clone(),
                    preparation_mutation: BossPreparationMutation::MarkUsed {
                        boundary: PINNACLE_DEPTH,
                    },
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => return Ok(()),
                Err(RepositoryError::Conflict) => continue,
                Err(error) => {
                    return Err(DigBossRuntimeError::Infrastructure(
                        repository.take_error().unwrap_or_else(|| error.to_string()),
                    ));
                }
            }
        }
    }

    fn prepare_pinnacle_attempt<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        risk_tier: RiskTier,
        wager: i64,
        entropy: &mut E,
    ) -> Result<PreparedPinnacleAttempt, DigBossRuntimeError> {
        let boss_id = self.ensure_pinnacle_locked(request, entropy)?;
        let pinnacle = pinnacle_by_id(&boss_id)
            .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(boss_id.clone()))?;
        let snapshot = self.pinnacle_snapshot(request)?;
        let phase = u8::try_from(snapshot.pinnacle_phase.clamp(1, 3))
            .map_err(|_| DigBossRuntimeError::InvalidPinnacle("invalid phase".to_owned()))?;
        let phase_definition = pinnacle.phases[usize::from(phase - 1)];
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let (lantern_ids, has_great_lantern) = repository
            .lantern_state(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
        let loaded = LoadedBossSnapshot {
            snapshot: snapshot.clone(),
            lantern_ids,
            has_great_lantern,
        };
        let tunnel =
            tunnel_from_snapshot(&loaded, 0).map_err(DigBossRuntimeError::Infrastructure)?;
        let placeholder = boss_by_id("grothak")
            .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle("missing base boss".to_owned()))?;
        let preparation_adapter = SqliteBossCombatPreparation::new(
            &self.config.database_path,
            self.config.pet_hunger_decay_per_day,
        );
        let preparation = preparation_adapter.prepare(
            BossCombatPreparationRequest {
                key: request.player_key(),
                tunnel: &tunnel,
                boss: placeholder,
                boundary: PINNACLE_DEPTH,
                risk_tier,
                wager,
                echo_applied: false,
                now: request.now,
            },
            pinnacle_base_combat(risk_tier),
        );
        let mut combat = preparation.base_combat;
        let scaled = scale_boss_stats_for_archetype(
            BossCombatStats {
                player_hp: combat.player_hp,
                boss_hp: combat.boss_hp,
                player_hit: combat.player_hit,
                player_damage: combat.player_damage,
                boss_hit: combat.boss_hit,
                boss_damage: combat.boss_damage,
            },
            phase_definition.archetype,
            PINNACLE_DEPTH,
            narrow_i32(snapshot.prestige_level, "prestige level")
                .map_err(DigBossRuntimeError::Infrastructure)?,
            false,
        );
        let mut progress = boss_progress_object(snapshot.boss_progress_json.as_deref());
        let boundary_key = PINNACLE_DEPTH.to_string();
        let pending_event_id = progress
            .get(&boundary_key)
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("pending_phase_event_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let phase_event = pending_event_id.as_deref().and_then(transition_event);
        let fresh_boss_hp = ((f64::from(
            scaled
                .boss_hp
                .saturating_add(phase_event.map_or(0, |event| event.boss_hp_delta))
                .max(1),
        ) * preparation.boss_hp_multiplier) as i32)
            .max(1);
        let phase_key = pinnacle_phase_key(phase);
        let (boss_hp, boss_hp_max) =
            resolve_pinnacle_hp(&progress, &phase_key, fresh_boss_hp, request.now);
        let phase_penalty = match phase {
            2 => 0.10,
            3 => 0.15,
            _ => 0.0,
        };
        let prestige_penalty = pinnacle_prestige_hit_penalty(
            narrow_i32(snapshot.prestige_level, "prestige level")
                .map_err(DigBossRuntimeError::Infrastructure)?,
        );
        let mut player_hit = combat.player_hit + preparation.player_hit_offset
            - PINNACLE_TIER_HIT_PENALTY
            - prestige_penalty
            - phase_penalty
            + phase_event.map_or(0.0, |event| event.player_hit_offset);
        if wager == 0 {
            player_hit *= FREE_FIGHT_ACCURACY_MULTIPLIER;
        }
        player_hit = player_hit.clamp(
            if wager > 0 {
                WAGERED_PLAYER_HIT_FLOOR
            } else {
                PLAYER_HIT_FLOOR
            },
            PLAYER_HIT_CEILING,
        );
        let luminosity_damage = if preparation.boss_no_crit_against {
            0
        } else {
            preparation.boss_damage_delta
        };
        let mut boss_damage = scaled
            .boss_damage
            .saturating_add(luminosity_damage)
            .saturating_add(phase_event.map_or(0, |event| event.boss_damage_delta))
            .max(1);
        let mut player_damage = ((f64::from(combat.player_damage)
            * preparation.player_damage_multiplier) as i32)
            .max(1)
            .saturating_add(phase_event.map_or(0, |event| event.player_damage_delta))
            .max(0);
        if preparation.damage_variance_modifier != 0.0 && entropy.next_unit() < 0.5 {
            let multiplier = 1.0 + preparation.damage_variance_modifier;
            player_damage = ((f64::from(player_damage) * multiplier) as i32).max(1);
            boss_damage = ((f64::from(boss_damage) * multiplier) as i32).max(1);
        }
        player_damage = player_damage.saturating_add(preparation.player_damage_after_phase_delta);
        let mut player_hp = combat
            .player_hp
            .saturating_add(phase_event.map_or(0, |event| event.player_hp_delta))
            .max(1);
        let shifting_idol = crate::boss_multi_tier::apply_shifting_idol_attempt_stats(
            preparation.shifting_idol,
            player_hp,
            player_hit,
            combat.critical_chance,
            entropy,
        );
        player_hp = i32::try_from(shifting_idol.player_hp).unwrap_or(i32::MAX);
        player_hit = shifting_idol.player_hit;
        combat.critical_chance = shifting_idol.crit_chance;
        combat.effects.shifting_idol_bonus =
            shifting_idol.bonus.map(|bonus| bonus.as_str().to_owned());
        combat.player_hp = player_hp;
        combat.boss_hp = boss_hp;
        combat.player_hit = player_hit;
        combat.player_damage = player_damage;
        combat.boss_hit = (scaled.boss_hit
            + phase_event.map_or(0.0, |event| event.boss_hit_offset))
        .clamp(0.05, 0.95);
        combat.boss_damage = boss_damage;
        combat.effects.pet_assist = preparation.pet_assist;
        combat.effects.boss_preparation = preparation.boss_preparation;
        if has_attempt_effect(&combat.effects) {
            combat.effects.trophy_start_hp = Some(player_hp);
        }
        let consumed_phase_event = phase_event.is_some();
        if consumed_phase_event {
            if let Some(entry) = progress
                .get_mut(&boundary_key)
                .and_then(Value::as_object_mut)
            {
                entry.remove("pending_phase_event_id");
            }
            let encoded = Value::Object(progress.clone()).to_string();
            let mut proposed = DigBossRuntimeState::from(&snapshot);
            proposed.boss_progress_json = Some(encoded);
            match repository
                .commit(DigBossRuntimeCommit {
                    expected: &snapshot,
                    proposed: &proposed,
                    audit: None,
                    echo: None,
                    artifact: None,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossRuntimeCommitOutcome::Applied(_) => {}
                DigBossRuntimeCommitOutcome::Conflict => {
                    return Err(DigBossRuntimeError::InvalidPinnacle(
                        "phase event changed during attempt preparation".to_owned(),
                    ));
                }
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
        let mechanic_id = phase_definition.mechanic_pool
            [entropy.choose_index(phase_definition.mechanic_pool.len())];
        let mechanic = runtime_mechanic_by_id(mechanic_id)
            .ok_or_else(|| BossServiceError::UnknownMechanic(mechanic_id.to_owned()))?;
        let win_chance = pinnacle_duel_win_probability(
            DuelOddsInput {
                player_hp: combat.player_hp,
                boss_hp: combat.boss_hp,
                player_hit: combat.player_hit,
                player_damage: combat.player_damage,
                boss_hit: combat.boss_hit,
                boss_damage: combat.boss_damage,
                critical_chance: combat.critical_chance,
                critical_bonus: combat.critical_bonus,
            },
            combat
                .effects
                .pet_assist
                .as_ref()
                .map_or(0, |assist| assist.bonus_percent),
        );
        let gear = repository
            .gear_snapshot(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
        let mut warnings = Vec::new();
        if let Some(error) = preparation_adapter.take_error() {
            warnings.push(error);
        }
        Ok(PreparedPinnacleAttempt {
            boss_id,
            phase,
            mechanic,
            player_hp_max: combat.player_hp.max(1),
            starting_boss_hp: combat.boss_hp,
            combat,
            boss_hp_max,
            attempts: narrow_i32(
                self.pinnacle_snapshot(request)?
                    .boss_attempts
                    .saturating_add(1),
                "boss attempts",
            )
            .map_err(DigBossRuntimeError::Infrastructure)?,
            risk_tier,
            wager,
            win_chance,
            gear_snapshot: GearSnapshot {
                gear_ids: gear.gear_ids,
                armor_id: gear.armor_id,
            },
            warnings,
        })
    }

    fn run_pinnacle_attempt<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        mut attempt: PreparedPinnacleAttempt,
        entropy: &mut E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleStartOutcome>, DigBossRuntimeError> {
        let mut round_log = Vec::new();
        let mut won = None;
        for round in 1..=BOSS_ROUND_CAP {
            if round == attempt.mechanic.trigger_round
                && attempt.combat.player_hp > 0
                && attempt.combat.boss_hp > 0
            {
                let paused = PausedBossDuel {
                    boss_id: attempt.boss_id.clone(),
                    boundary: PINNACLE_DEPTH,
                    mechanic_id: attempt.mechanic.id.clone(),
                    risk_tier: attempt.risk_tier,
                    wager: attempt.wager,
                    combat: attempt.combat,
                    round_num: round,
                    round_log,
                    pending_prompt: PendingPrompt::from(&attempt.mechanic),
                    attempts_this_fight: attempt.attempts,
                    initial_win_chance: attempt.win_chance,
                    payout_multiplier: pinnacle_total_multiplier(
                        attempt.risk_tier,
                        attempt.win_chance,
                    )?,
                    player_hp_max: attempt.player_hp_max,
                    boss_hp_max: attempt.boss_hp_max,
                    starting_boss_hp: attempt.starting_boss_hp,
                    gear_snapshot: attempt.gear_snapshot,
                    forced_no_wager_phase: false,
                    echo_applied: false,
                    echo_killer_id: None,
                };
                let row = paused_duel_row(request.player_key(), &paused, request.now)
                    .map_err(DigBossRuntimeError::Infrastructure)?;
                DigBossRuntimeRepository::new(&self.config.database_path)
                    .save_active_duel(&row)
                    .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
                return Ok(DigBossRuntimeResult {
                    outcome: DigPinnacleStartOutcome::Paused(Box::new(paused.clone())),
                    action_id: None,
                    abandoned_action_id: None,
                    abandoned_wager_forfeit: 0,
                    abandoned_gear_wear: GearWearResult::default(),
                    gear_drop: None,
                    prestige_relic_drop: None,
                    broken_gear: Vec::new(),
                    warnings: attempt.warnings,
                    notices: vec![BossNotice::PromptReady {
                        key: request.player_key(),
                        boss_id: attempt.boss_id,
                        prompt: paused.pending_prompt,
                    }],
                });
            }
            let (record, terminal) = run_combat_round(round, &mut attempt.combat, entropy);
            round_log.push(record);
            if terminal.is_some() {
                won = terminal;
                break;
            }
        }
        let won = won.unwrap_or(false);
        let gear_wear = self.tick_pinnacle_gear(
            request,
            &attempt.gear_snapshot,
            won,
            attempt.combat.effects.boss_preparation.as_ref(),
        )?;
        let settled = self.settle_pinnacle(
            request,
            PinnacleSettlementInput {
                boss_id: attempt.boss_id,
                phase: attempt.phase,
                risk_tier: attempt.risk_tier,
                wager: attempt.wager,
                won,
                ending_boss_hp: attempt.combat.boss_hp,
                boss_hp_max: attempt.boss_hp_max,
                starting_boss_hp: attempt.starting_boss_hp,
                win_chance: attempt.win_chance,
                attempts: attempt.attempts,
                round_log,
                gear_wear,
                boss_preparation: attempt.combat.effects.boss_preparation,
                warding_salts_blocked: false,
            },
            entropy,
        )?;
        Ok(DigBossRuntimeResult {
            outcome: DigPinnacleStartOutcome::Resolved(Box::new(settled.outcome)),
            action_id: settled.action_id,
            abandoned_action_id: None,
            abandoned_wager_forfeit: 0,
            abandoned_gear_wear: GearWearResult::default(),
            gear_drop: None,
            prestige_relic_drop: None,
            broken_gear: Vec::new(),
            warnings: attempt
                .warnings
                .into_iter()
                .chain(settled.warnings)
                .collect(),
            notices: settled.notices,
        })
    }

    fn tick_pinnacle_gear(
        &self,
        request: DigBossRuntimeRequest,
        snapshot: &GearSnapshot,
        won: bool,
        preparation: Option<&BossPreparationState>,
    ) -> Result<GearWearResult, DigBossRuntimeError> {
        let rescue_line_used =
            !won && preparation.is_some_and(|preparation| preparation.item_type == "rescue_line");
        DigBossRuntimeRepository::new(&self.config.database_path)
            .tick_gear_after_resolution(
                database_key(request.player_key()),
                &DigBossGearSnapshot {
                    gear_ids: snapshot.gear_ids.clone(),
                    armor_id: snapshot.armor_id,
                },
                won || rescue_line_used,
            )
            .map(|receipt| GearWearResult {
                broken_ids: receipt.broken_ids,
            })
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))
    }

    fn settle_pinnacle<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        input: PinnacleSettlementInput,
        entropy: &mut E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleResolved>, DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let mut profit_policy = SqliteBossProfitPolicy::new(
            &self.config.database_path,
            Arc::clone(&self.vanity_tax),
            Arc::clone(&self.low_priority_tax),
        );
        let mut economy_event = SqliteBossEconomyEvent::new(
            &self.config.database_path,
            self.config.economy_event.clone(),
            request.now,
        );
        loop {
            let expected = repository
                .snapshot(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            if expected.pinnacle_phase.clamp(1, 3) != i64::from(input.phase)
                || expected.pinnacle_boss_id.as_deref() != Some(input.boss_id.as_str())
            {
                return Err(DigBossRuntimeError::InvalidPinnacle(
                    "pinnacle phase or boss changed before settlement".to_owned(),
                ));
            }
            let pinnacle = pinnacle_by_id(&input.boss_id)
                .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(input.boss_id.clone()))?;
            let mut progress = boss_progress_object(expected.boss_progress_json.as_deref());
            let phase_key = pinnacle_phase_key(input.phase);
            let boundary_key = PINNACLE_DEPTH.to_string();
            let mut proposed = DigBossRuntimeState::from(&expected);
            let mut phase_event_id = None;
            let mut relic_drop = None;
            let mut gross_payout = 0;
            let mut gross_jc = 0;
            let mut scaled_base_jc = 0;
            let mut wager_payout = 0;
            let mut payout = 0;
            let mut bankruptcy_penalty = 0;
            let mut vanity_tax = 0;
            let mut low_priority_tax = 0;
            let mut jc_delta = 0;
            let mut knockback = 0;
            let mut next_phase = input.phase;
            let rescue_line_used = !input.won
                && input
                    .boss_preparation
                    .as_ref()
                    .is_some_and(|preparation| preparation.item_type == "rescue_line");
            if input.won && input.phase < 3 {
                next_phase = input.phase + 1;
                let phase_event =
                    PHASE_TRANSITION_EVENTS[entropy.choose_index(PHASE_TRANSITION_EVENTS.len())];
                phase_event_id = Some(phase_event.id.to_owned());
                progress.remove(&phase_key);
                let entry = progress
                    .entry(boundary_key.clone())
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or_else(|| {
                        DigBossRuntimeError::InvalidPinnacle(
                            "pinnacle progress entry is not an object".to_owned(),
                        )
                    })?;
                entry.insert(
                    "status".to_owned(),
                    Value::String(
                        if input.phase == 1 {
                            "phase1_defeated"
                        } else {
                            "phase2_defeated"
                        }
                        .to_owned(),
                    ),
                );
                entry.insert("boss_id".to_owned(), Value::String(input.boss_id.clone()));
                entry.insert(
                    "last_outcome".to_owned(),
                    Value::String("defeated".to_owned()),
                );
                entry.insert("first_meet_seen".to_owned(), Value::Bool(true));
                entry.insert(
                    "pending_phase_event_id".to_owned(),
                    Value::String(phase_event.id.to_owned()),
                );
                if input.wager > 0 {
                    entry.insert("carried_wager".to_owned(), Value::from(input.wager));
                    entry.insert(
                        "carried_risk_tier".to_owned(),
                        Value::String(risk_name(input.risk_tier).to_owned()),
                    );
                } else {
                    entry.remove("carried_wager");
                    entry.remove("carried_risk_tier");
                }
                proposed.pinnacle_phase = i64::from(next_phase);
                proposed.luminosity =
                    (proposed.luminosity + i64::from(phase_event.luminosity_delta)).clamp(0, 100);
                proposed.last_lum_update_at = Some(request.now);
                proposed.boss_attempts = i64::from(input.attempts.max(0));
                proposed.last_dig_at = Some(request.now);
            } else if input.won {
                progress.remove(&phase_key);
                progress.insert(
                    boundary_key,
                    json!({
                        "status":"defeated",
                        "last_outcome":if input.win_chance < 0.6 { "close_win" } else { "defeated" },
                        "first_meet_seen":true,
                        "boss_id":input.boss_id,
                        "hp_remaining":0,
                        "hp_max":input.boss_hp_max.max(1),
                        "last_engaged_at":request.now,
                    }),
                );
                let base_reward = PINNACLE_BASE_JC_REWARD.saturating_add(
                    PINNACLE_JC_PER_PRESTIGE.saturating_mul(expected.prestige_level.max(0)),
                );
                let gross_base =
                    economy_event.adjust_reward(GuildId(request.guild_id), base_reward);
                let scaled_base = scale_positive_dig_jc(gross_base);
                let wager_profit =
                    pinnacle_wager_profit(input.wager, input.risk_tier, input.win_chance)
                        .map_err(|error| DigBossRuntimeError::InvalidPinnacle(error.to_string()))?;
                let event_wager =
                    economy_event.adjust_reward(GuildId(request.guild_id), wager_profit);
                gross_payout = gross_base.saturating_add(event_wager);
                gross_jc = gross_base;
                scaled_base_jc = scaled_base;
                wager_payout = event_wager;
                let adjustment = profit_policy.apply_penalty(
                    request.player_key(),
                    scaled_base.saturating_add(event_wager),
                );
                bankruptcy_penalty = adjustment.penalty_applied;
                vanity_tax = adjustment.vanity_tax;
                low_priority_tax = adjustment.low_priority_tax;
                jc_delta = adjustment.penalized;
                payout = jc_delta;
                proposed.balance = expected.balance.saturating_add(jc_delta);
                proposed.depth = i64::from(PINNACLE_DEPTH);
                proposed.max_depth = proposed.max_depth.max(i64::from(PINNACLE_DEPTH));
                proposed.pinnacle_phase = 0;
                proposed.pinnacle_hp_remaining = None;
                proposed.pinnacle_last_engaged_at = None;
                proposed.boss_attempts = 0;
                proposed.cheer_data_json = None;
                proposed.last_dig_at = Some(request.now);
                let relic = roll_pinnacle_relic(
                    narrow_i32(expected.prestige_level, "prestige level")
                        .map_err(DigBossRuntimeError::Infrastructure)?,
                    &input.boss_id,
                    entropy.choose_index(crate::dig_bosses::PINNACLE_RELIC_STATS.len()),
                    entropy.choose_index(crate::dig_bosses::PINNACLE_RELIC_STATS.len()),
                )
                .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(input.boss_id.clone()))?;
                relic_drop = Some(relic);
                next_phase = 0;
            } else {
                knockback = entropy.inclusive_i32(8, 16).clamp(8, 16);
                if rescue_line_used {
                    knockback = (knockback + 1) / 2;
                }
                proposed.depth = proposed.depth.saturating_sub(i64::from(knockback)).max(0);
                let actual_debit = input.wager.max(0).min(expected.balance.max(0));
                proposed.balance = expected.balance.saturating_sub(actual_debit);
                jc_delta = -actual_debit;
                proposed.boss_attempts = i64::from(input.attempts.max(0));
                proposed.cheer_data_json = None;
                proposed.last_dig_at = Some(request.now);
                progress.insert(
                    phase_key,
                    json!({
                        "status":"active",
                        "boss_id":input.boss_id,
                        "hp_remaining":input.ending_boss_hp.max(0),
                        "hp_max":input.boss_hp_max.max(1),
                        "last_engaged_at":request.now,
                        "last_outcome":"loss",
                        "first_meet_seen":true,
                    }),
                );
                let entry = progress
                    .entry(boundary_key)
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or_else(|| {
                        DigBossRuntimeError::InvalidPinnacle(
                            "pinnacle progress entry is not an object".to_owned(),
                        )
                    })?;
                entry.insert(
                    "status".to_owned(),
                    Value::String(
                        match input.phase {
                            2 => "phase1_defeated",
                            3 => "phase2_defeated",
                            _ => "active",
                        }
                        .to_owned(),
                    ),
                );
                entry.insert("boss_id".to_owned(), Value::String(input.boss_id.clone()));
                entry.insert("last_outcome".to_owned(), Value::String("loss".to_owned()));
                entry.insert("first_meet_seen".to_owned(), Value::Bool(true));
                entry.remove("carried_wager");
                entry.remove("carried_risk_tier");
                entry.remove("active_prep");
            }
            proposed.boss_progress_json = Some(Value::Object(progress).to_string());
            let detail = json!({
                "pinnacle_id":input.boss_id,
                "phase":input.phase,
                "won":input.won,
                "rounds":input.round_log.iter().map(round_record_json).collect::<Vec<_>>(),
                "boss_hp_remaining":input.ending_boss_hp.max(0),
                "jc_delta":jc_delta,
                "gross_jc":gross_jc,
                "gross_payout":gross_payout,
                "scaled_base_jc":scaled_base_jc,
                "wager_payout":wager_payout,
                "reward_multiplier":DIG_POSITIVE_JC_MULTIPLIER,
                "bankruptcy_penalty":bankruptcy_penalty,
                "vanity_tax":vanity_tax,
                "low_priority_tax":low_priority_tax,
                "starting_boss_hp":input.starting_boss_hp,
            })
            .to_string();
            let audit = DigBossRuntimeAudit {
                action_type: "pinnacle_fight",
                target_id: None,
                detail_json: &detail,
                created_at: request.now,
            };
            let artifact = relic_drop
                .as_ref()
                .map(|relic| DigBossRuntimeArtifactGrant {
                    artifact_id: &relic.artifact_id,
                    found_at: request.now,
                    is_relic: true,
                });
            let commit = repository
                .commit(DigBossRuntimeCommit {
                    expected: &expected,
                    proposed: &proposed,
                    audit: Some(audit),
                    echo: None,
                    artifact,
                    vanity_tax,
                    low_priority_tax,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
            match commit {
                DigBossRuntimeCommitOutcome::Applied(receipt) => {
                    let relic_drop = relic_drop.map(|relic| DigPinnacleRelicDrop {
                        relic,
                        database_id: receipt.artifact_database_id.unwrap_or_default(),
                    });
                    let resolved = DigPinnacleResolved {
                        won: input.won,
                        boss_id: input.boss_id.clone(),
                        boss_name: if input.won && input.phase == 3 {
                            pinnacle.name.to_owned()
                        } else {
                            pinnacle.phases[usize::from(input.phase - 1)]
                                .title
                                .to_owned()
                        },
                        phase: input.phase,
                        next_phase,
                        risk_tier: input.risk_tier,
                        wager: input.wager,
                        win_chance: input.win_chance,
                        jc_delta,
                        payout,
                        wager_payout,
                        gross_jc,
                        scaled_base_jc,
                        reward_multiplier: if input.won && input.phase == 3 {
                            DIG_POSITIVE_JC_MULTIPLIER
                        } else {
                            0.0
                        },
                        gross_payout,
                        bankruptcy_penalty,
                        vanity_tax,
                        low_priority_tax,
                        new_depth: narrow_i32(proposed.depth, "depth")
                            .map_err(DigBossRuntimeError::Infrastructure)?,
                        boss_hp_remaining: if input.won {
                            0
                        } else {
                            input.ending_boss_hp.max(0)
                        },
                        boss_hp_max: input.boss_hp_max,
                        starting_boss_hp: input.starting_boss_hp,
                        knockback,
                        round_log: input.round_log.clone(),
                        gear_wear: input.gear_wear.clone(),
                        phase_event_id,
                        relic_drop,
                        rescue_line_used,
                        warding_salts_blocked: input.warding_salts_blocked,
                    };
                    let mut warnings = economy_event.warnings;
                    warnings.append(&mut profit_policy.warnings);
                    return Ok(DigBossRuntimeResult {
                        outcome: resolved,
                        action_id: receipt.action_id,
                        abandoned_action_id: None,
                        abandoned_wager_forfeit: 0,
                        abandoned_gear_wear: GearWearResult::default(),
                        gear_drop: None,
                        prestige_relic_drop: None,
                        broken_gear: Vec::new(),
                        warnings,
                        notices: vec![BossNotice::Resolved {
                            key: request.player_key(),
                            boss_id: input.boss_id,
                            won: input.won,
                            jc_delta,
                        }],
                    });
                }
                DigBossRuntimeCommitOutcome::Conflict => continue,
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    fn scout_pinnacle_with_entropy<E: EntropyPort>(
        &self,
        request: DigBossRuntimeRequest,
        enhanced: bool,
        entropy: &mut E,
    ) -> Result<DigBossRuntimeResult<DigPinnacleScoutResult>, DigBossRuntimeError> {
        let boss_id = self.ensure_pinnacle_locked(request, entropy)?;
        let pinnacle = pinnacle_by_id(&boss_id)
            .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle(boss_id.clone()))?;
        let snapshot = self.pinnacle_snapshot(request)?;
        let phase = u8::try_from(snapshot.pinnacle_phase.clamp(1, 3))
            .map_err(|_| DigBossRuntimeError::InvalidPinnacle("invalid phase".to_owned()))?;
        let phase_definition = pinnacle.phases[usize::from(phase - 1)];
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let (lantern_ids, has_great_lantern) = repository
            .lantern_state(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
        if !has_great_lantern && lantern_ids.is_empty() {
            return Err(BossServiceError::LanternRequired.into());
        }
        let loaded = LoadedBossSnapshot {
            snapshot: snapshot.clone(),
            lantern_ids: lantern_ids.clone(),
            has_great_lantern,
        };
        let tunnel =
            tunnel_from_snapshot(&loaded, 0).map_err(DigBossRuntimeError::Infrastructure)?;
        let placeholder = boss_by_id("grothak")
            .ok_or_else(|| DigBossRuntimeError::InvalidPinnacle("missing base boss".to_owned()))?;
        let preparation_adapter = SqliteBossCombatPreparation::new(
            &self.config.database_path,
            self.config.pet_hunger_decay_per_day,
        );
        let progress = boss_progress_object(snapshot.boss_progress_json.as_deref());
        let pending_event = progress
            .get(&PINNACLE_DEPTH.to_string())
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("pending_phase_event_id"))
            .and_then(Value::as_str)
            .and_then(transition_event);
        let make_risk = |risk_tier: RiskTier,
                         preparation: BossCombatPreparation|
         -> Result<DigPinnacleScoutRisk, DigBossRuntimeError> {
            let mut combat = preparation.base_combat;
            let scaled = scale_boss_stats_for_archetype(
                BossCombatStats {
                    player_hp: combat.player_hp,
                    boss_hp: combat.boss_hp,
                    player_hit: combat.player_hit,
                    player_damage: combat.player_damage,
                    boss_hit: combat.boss_hit,
                    boss_damage: combat.boss_damage,
                },
                phase_definition.archetype,
                PINNACLE_DEPTH,
                narrow_i32(snapshot.prestige_level, "prestige level")
                    .map_err(DigBossRuntimeError::Infrastructure)?,
                false,
            );
            let fresh_hp = ((f64::from(
                scaled
                    .boss_hp
                    .saturating_add(pending_event.map_or(0, |event| event.boss_hp_delta))
                    .max(1),
            ) * preparation.boss_hp_multiplier) as i32)
                .max(1);
            let (boss_hp, _) =
                resolve_pinnacle_hp(&progress, &pinnacle_phase_key(phase), fresh_hp, request.now);
            let phase_penalty = match phase {
                2 => 0.10,
                3 => 0.15,
                _ => 0.0,
            };
            let raw_hit = combat.player_hit + preparation.player_hit_offset
                - PINNACLE_TIER_HIT_PENALTY
                - pinnacle_prestige_hit_penalty(
                    narrow_i32(snapshot.prestige_level, "prestige level")
                        .map_err(DigBossRuntimeError::Infrastructure)?,
                )
                - phase_penalty
                + pending_event.map_or(0.0, |event| event.player_hit_offset);
            let paid_hit = raw_hit.clamp(WAGERED_PLAYER_HIT_FLOOR, PLAYER_HIT_CEILING);
            let free_hit = (raw_hit * FREE_FIGHT_ACCURACY_MULTIPLIER)
                .clamp(PLAYER_HIT_FLOOR, PLAYER_HIT_CEILING);
            let boss_hit = (scaled.boss_hit
                + pending_event.map_or(0.0, |event| event.boss_hit_offset))
            .clamp(0.05, 0.95);
            let boss_damage = scaled
                .boss_damage
                .saturating_add(if preparation.boss_no_crit_against {
                    0
                } else {
                    preparation.boss_damage_delta
                })
                .saturating_add(pending_event.map_or(0, |event| event.boss_damage_delta))
                .max(1);
            let player_hp = combat
                .player_hp
                .saturating_add(pending_event.map_or(0, |event| event.player_hp_delta))
                .max(1);
            let player_damage = ((f64::from(combat.player_damage)
                * preparation.player_damage_multiplier) as i32)
                .max(1)
                .saturating_add(pending_event.map_or(0, |event| event.player_damage_delta))
                .max(0)
                .saturating_add(preparation.player_damage_after_phase_delta);
            combat.player_hp = player_hp;
            let pet_bonus = preparation
                .pet_assist
                .as_ref()
                .map_or(0, |assist| assist.bonus_percent);
            let paid = pinnacle_duel_win_probability(
                DuelOddsInput {
                    player_hp,
                    boss_hp,
                    player_hit: paid_hit,
                    player_damage,
                    boss_hit,
                    boss_damage,
                    critical_chance: combat.critical_chance,
                    critical_bonus: combat.critical_bonus,
                },
                pet_bonus,
            );
            let free = pinnacle_duel_win_probability(
                DuelOddsInput {
                    player_hit: free_hit,
                    ..DuelOddsInput {
                        player_hp,
                        boss_hp,
                        player_hit: paid_hit,
                        player_damage,
                        boss_hit,
                        boss_damage,
                        critical_chance: combat.critical_chance,
                        critical_bonus: combat.critical_bonus,
                    }
                },
                pet_bonus,
            );
            Ok(DigPinnacleScoutRisk {
                win_chance: paid,
                free_fight_chance: free,
                player_hp,
                boss_hp,
                player_hit: paid_hit,
                boss_hit,
                multiplier: pinnacle_total_multiplier(risk_tier, paid)?,
            })
        };
        let preparation = |risk_tier| {
            preparation_adapter.prepare(
                BossCombatPreparationRequest {
                    key: request.player_key(),
                    tunnel: &tunnel,
                    boss: placeholder,
                    boundary: PINNACLE_DEPTH,
                    risk_tier,
                    wager: 1,
                    echo_applied: false,
                    now: request.now,
                },
                pinnacle_base_combat(risk_tier),
            )
        };
        let cautious = make_risk(RiskTier::Cautious, preparation(RiskTier::Cautious))?;
        let bold = make_risk(RiskTier::Bold, preparation(RiskTier::Bold))?;
        let reckless = make_risk(RiskTier::Reckless, preparation(RiskTier::Reckless))?;
        if !has_great_lantern {
            let row_id = lantern_ids[0];
            if !repository
                .consume_lantern(database_key(request.player_key()), row_id)
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                return Err(DigBossRuntimeError::InvalidPinnacle(
                    "Lantern changed during pinnacle scout".to_owned(),
                ));
            }
        }
        let mut warnings = Vec::new();
        if let Some(error) = preparation_adapter.take_error() {
            warnings.push(error);
        }
        Ok(DigBossRuntimeResult {
            outcome: DigPinnacleScoutResult {
                boss_id,
                boss_name: phase_definition.title.to_owned(),
                phase,
                pet_assist: preparation(RiskTier::Cautious).pet_assist,
                enhanced_mechanic_ids: if enhanced {
                    phase_definition
                        .mechanic_pool
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect()
                } else {
                    Vec::new()
                },
                cautious,
                bold,
                reckless,
            },
            action_id: None,
            abandoned_action_id: None,
            abandoned_wager_forfeit: 0,
            abandoned_gear_wear: GearWearResult::default(),
            gear_drop: None,
            prestige_relic_drop: None,
            broken_gear: Vec::new(),
            warnings,
            notices: Vec::new(),
        })
    }

    fn regular_boundary(&self, request: DigBossRuntimeRequest) -> Result<i32, DigBossRuntimeError> {
        let snapshot = DigBossRuntimeRepository::new(&self.config.database_path)
            .snapshot(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let progress = parse_boss_progress(snapshot.boss_progress_json.as_deref())
            .map_err(DigBossRuntimeError::Infrastructure)?;
        at_boss_boundary(
            narrow_i32(snapshot.depth, "tunnel depth")
                .map_err(DigBossRuntimeError::Infrastructure)?,
            &progress,
        )
        .ok_or_else(|| BossServiceError::NotAtBossBoundary.into())
    }

    fn current_boundary(&self, request: DigBossRuntimeRequest) -> Result<i32, DigBossRuntimeError> {
        let snapshot = DigCarryWagerRepository::new(&self.config.database_path)
            .snapshot(CarryTunnelKey {
                discord_id: request.discord_id,
                guild_id: Some(request.guild_id),
            })
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            .ok_or(BossServiceError::MissingTunnel)?;
        let boundary =
            current_boss_boundary(&snapshot).ok_or(BossServiceError::NotAtBossBoundary)?;
        narrow_i32(boundary, "boss boundary").map_err(DigBossRuntimeError::Infrastructure)
    }

    fn claim_abandoned_duel(
        &self,
        request: DigBossRuntimeRequest,
        boundary: i32,
    ) -> Result<Option<ClaimedAbandonedDuel>, DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let Some(row) = repository
            .claim_active_duel(database_key(request.player_key()))
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
        else {
            return Ok(None);
        };
        let requested_forfeit = row.wager.max(0);
        if requested_forfeit == 0 {
            return Ok(Some(ClaimedAbandonedDuel {
                row,
                action_id: None,
                wager_forfeit: 0,
            }));
        }
        loop {
            let expected = repository
                .snapshot(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            let wager_forfeit = requested_forfeit.min(expected.balance.max(0));
            let mut proposed = DigBossRuntimeState::from(&expected);
            proposed.balance = proposed.balance.saturating_sub(wager_forfeit);
            proposed.boss_progress_json =
                clear_carried_wager_json(proposed.boss_progress_json.as_deref(), boundary)?;
            let detail = json!({
                "boundary":boundary,
                "abandoned_duel":true,
                "wager_forfeit":wager_forfeit,
                "boss_id":row.boss_id,
            })
            .to_string();
            match repository
                .commit(DigBossRuntimeCommit {
                    expected: &expected,
                    proposed: &proposed,
                    audit: Some(DigBossRuntimeAudit {
                        action_type: "boss_duel_abandoned",
                        target_id: None,
                        detail_json: &detail,
                        created_at: request.now,
                    }),
                    echo: None,
                    artifact: None,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossRuntimeCommitOutcome::Applied(receipt) => {
                    return Ok(Some(ClaimedAbandonedDuel {
                        row,
                        action_id: receipt.action_id,
                        wager_forfeit,
                    }));
                }
                DigBossRuntimeCommitOutcome::Conflict => continue,
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    fn validate_and_resolve_start_wager(
        &self,
        request: DigBossRuntimeRequest,
        boundary: i32,
        wager: i64,
    ) -> Result<i64, DigBossRuntimeError> {
        if wager < 0 {
            return Err(BossServiceError::NegativeWager.into());
        }
        loop {
            let mut repository = SqliteBossRepository::new(&self.config.database_path, request.now);
            let tunnel = repository
                .load_tunnel(request.player_key())
                .ok_or(BossServiceError::MissingTunnel)?;
            if wager > 0
                && !regular_boss_wager_allowed(
                    &tunnel.boss_progress,
                    boundary,
                    tunnel.prestige_level,
                )
            {
                return Err(BossServiceError::WagerNotAllowed.into());
            }
            let cursed = wager > 0 && tunnel.stinger_curse.contains(CurseKind::HalveNextWager);
            let effective = if cursed { wager / 2 + wager % 2 } else { wager };
            if tunnel.balance < effective {
                return Err(BossServiceError::InsufficientBalance {
                    wager: effective,
                    balance: tunnel.balance,
                }
                .into());
            }
            if !cursed {
                return Ok(effective);
            }
            let mut next = tunnel.clone();
            next.stinger_curse.consume(CurseKind::HalveNextWager);
            match repository.commit(
                request.player_key(),
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => return Ok(effective),
                Err(RepositoryError::Conflict) => continue,
                Err(error) => {
                    let detail = repository.take_error().unwrap_or_else(|| error.to_string());
                    return Err(DigBossRuntimeError::Infrastructure(detail));
                }
            }
        }
    }

    fn activate_start_preparation(
        &self,
        request: DigBossRuntimeRequest,
        boundary: i32,
    ) -> Result<(), DigBossRuntimeError> {
        let activation = SqliteBossPreparationActivation::new(&self.config.database_path);
        activation
            .activate(request.player_key(), boundary)
            .map_err(|error| {
                DigBossRuntimeError::Infrastructure(
                    activation.take_error().unwrap_or_else(|| error.to_string()),
                )
            })
    }

    fn settle_abandoned_gear(
        &self,
        request: DigBossRuntimeRequest,
        abandoned: Option<&ClaimedAbandonedDuel>,
    ) -> Result<GearWearResult, DigBossRuntimeError> {
        let Some(abandoned) = abandoned else {
            return Ok(GearWearResult::default());
        };
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        let mut snapshot = gear_snapshot_from_paused_row(&abandoned.row);
        if snapshot.gear_ids.is_empty() {
            snapshot = repository
                .gear_snapshot(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?;
        }
        repository
            .tick_gear_after_resolution(database_key(request.player_key()), &snapshot, true)
            .map(|receipt| GearWearResult {
                broken_ids: receipt.broken_ids,
            })
            .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))
    }

    fn mark_encounter_seen(
        &self,
        request: DigBossRuntimeRequest,
        boundary: i32,
        boss_id: &str,
    ) -> Result<(), DigBossRuntimeError> {
        let repository = DigBossRuntimeRepository::new(&self.config.database_path);
        loop {
            let expected = repository
                .snapshot(database_key(request.player_key()))
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
                .ok_or(BossServiceError::MissingTunnel)?;
            let mut root = boss_progress_object(expected.boss_progress_json.as_deref());
            let key = boundary.to_string();
            let prior = root.remove(&key);
            let mut entry = prior
                .as_ref()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if prior.as_ref().and_then(Value::as_str).is_some() {
                entry.insert(
                    "status".to_owned(),
                    Value::String(
                        prior
                            .as_ref()
                            .and_then(Value::as_str)
                            .unwrap_or("active")
                            .to_owned(),
                    ),
                );
            }
            if entry
                .get("first_meet_seen")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && entry.get("boss_id").and_then(Value::as_str) == Some(boss_id)
            {
                return Ok(());
            }
            entry.insert("first_meet_seen".to_owned(), Value::Bool(true));
            entry
                .entry("status".to_owned())
                .or_insert_with(|| Value::String("active".to_owned()));
            entry.insert("boss_id".to_owned(), Value::String(boss_id.to_owned()));
            root.insert(key, Value::Object(entry));
            let mut proposed = DigBossRuntimeState::from(&expected);
            proposed.boss_progress_json = Some(Value::Object(root).to_string());
            match repository
                .commit(DigBossRuntimeCommit {
                    expected: &expected,
                    proposed: &proposed,
                    audit: None,
                    echo: None,
                    artifact: None,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                })
                .map_err(|error| DigBossRuntimeError::Infrastructure(error.to_string()))?
            {
                DigBossRuntimeCommitOutcome::Applied(_) => return Ok(()),
                DigBossRuntimeCommitOutcome::Conflict => continue,
                DigBossRuntimeCommitOutcome::MissingPlayer
                | DigBossRuntimeCommitOutcome::MissingTunnel => {
                    return Err(BossServiceError::MissingTunnel.into());
                }
            }
        }
    }

    fn compose<E: EntropyPort>(&self, now: i64, entropy: E) -> ComposedBossService<E> {
        BossMultiTierService::new(
            SqliteBossRepository::new(&self.config.database_path, now),
            entropy,
            FixedClock { unix_seconds: now },
            SqliteBossEconomyEvent::new(
                &self.config.database_path,
                self.config.economy_event.clone(),
                now,
            ),
            SqliteBossProfitPolicy::new(
                &self.config.database_path,
                Arc::clone(&self.vanity_tax),
                Arc::clone(&self.low_priority_tax),
            ),
            SqliteBossGearRepair::new(&self.config.database_path),
            DigBossOutcomeCollector::default(),
        )
        .with_preparation_activation(SqliteBossPreparationActivation::new(
            &self.config.database_path,
        ))
        .with_combat_preparation(SqliteBossCombatPreparation::new(
            &self.config.database_path,
            self.config.pet_hunger_decay_per_day,
        ))
    }
}

fn encounter_dialogue<E: EntropyPort>(
    boss_id: &str,
    phase: u8,
    is_pinnacle: bool,
    progress_entry: Option<&BossProgressEntry>,
    snapshot: &DigBossRuntimeSnapshot,
    entropy: &mut E,
) -> String {
    let choose = |lines: &[&str], entropy: &mut E| {
        if lines.is_empty() {
            "A fearsome guardian blocks your path!".to_owned()
        } else {
            lines[entropy.choose_index(lines.len()).min(lines.len() - 1)].to_owned()
        }
    };
    let template = if is_pinnacle {
        let slots = dialogue_slots(boss_id).unwrap_or(DialogueSlots {
            first_meet: &["A fearsome guardian blocks your path!"],
            after_defeat: &["The boss remembers your last clash."],
            after_retreat: &["The climb back did not end the fight."],
            after_close_win: &["The margin was narrow. The path remains."],
            after_scout: &["You measured the danger. It measured you."],
        });
        let lines = match progress_entry {
            Some(entry) if entry.first_meet_seen => match entry.last_outcome.as_deref() {
                Some("defeated") => slots.after_defeat,
                Some("retreat") => slots.after_retreat,
                Some("close_win") => slots.after_close_win,
                Some("scout") => slots.after_scout,
                _ => slots.first_meet,
            },
            _ => slots.first_meet,
        };
        choose(lines, entropy)
    } else if phase > 1 {
        regular_phase(boss_id, phase).map_or_else(
            || "The chamber changes shape.".to_owned(),
            |definition| choose(&definition.dialogue, entropy),
        )
    } else {
        let slots = dialogue_slots(boss_id).unwrap_or(DialogueSlots {
            first_meet: &["A fearsome guardian blocks your path!"],
            after_defeat: &["The boss remembers your last clash."],
            after_retreat: &["The climb back did not end the fight."],
            after_close_win: &["The margin was narrow. The path remains."],
            after_scout: &["You measured the danger. It measured you."],
        });
        let lines = match progress_entry {
            Some(entry) if entry.first_meet_seen => match entry.last_outcome.as_deref() {
                Some("defeated") => slots.after_defeat,
                Some("retreat") => slots.after_retreat,
                Some("close_win") => slots.after_close_win,
                Some("scout") => slots.after_scout,
                _ => slots.first_meet,
            },
            _ => slots.first_meet,
        };
        choose(lines, entropy)
    };
    render_boss_bark(
        &template,
        BarkContext {
            streak_days: 1,
            depth: narrow_i32(snapshot.depth, "depth").unwrap_or_default(),
            prestige_level: narrow_i32(snapshot.prestige_level, "prestige level")
                .unwrap_or_default(),
            killed_boss_name: None,
        },
    )
}

fn parse_active_cheers(raw: Option<&str>, now: i64) -> Vec<Value> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|cheer| {
            cheer
                .get("expires_at")
                .and_then(Value::as_i64)
                .is_some_and(|expires_at| expires_at > now)
        })
        .collect()
}

fn pinnacle_base_combat(risk_tier: RiskTier) -> CombatState {
    let (player_hp, boss_hp, player_hit, player_damage, boss_hit, critical_chance) = match risk_tier
    {
        RiskTier::Cautious => (5, 4, 0.60, 1, 0.30, 0.0),
        RiskTier::Bold => (3, 5, 0.42, 2, 0.45, 0.15),
        RiskTier::Reckless => (2, 6, 0.18, 3, 0.60, 0.30),
    };
    CombatState {
        player_hp,
        boss_hp,
        player_hit,
        player_damage,
        boss_hit,
        boss_damage: 1,
        critical_chance,
        critical_bonus: i32::from(risk_tier != RiskTier::Cautious),
        effects: CombatEffects::default(),
    }
}

fn boss_progress_object(raw: Option<&str>) -> Map<String, Value> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn pinnacle_phase_key(phase: u8) -> String {
    format!("{PINNACLE_DEPTH}:{phase}")
}

fn resolve_pinnacle_hp(
    progress: &Map<String, Value>,
    phase_key: &str,
    fresh_hp: i32,
    now: i64,
) -> (i32, i32) {
    let Some(entry) = progress.get(phase_key).and_then(Value::as_object) else {
        return (fresh_hp.max(1), fresh_hp.max(1));
    };
    let Some(remaining) = json_i32(entry.get("hp_remaining")) else {
        return (fresh_hp.max(1), fresh_hp.max(1));
    };
    let maximum = json_i32(entry.get("hp_max")).unwrap_or(fresh_hp).max(1);
    let elapsed_days = entry
        .get("last_engaged_at")
        .and_then(Value::as_i64)
        .map_or(0, |engaged| now.saturating_sub(engaged).max(0) / 86_400);
    let regenerated = remaining.saturating_add(i32::try_from(elapsed_days).unwrap_or(i32::MAX));
    (regenerated.clamp(1, maximum), maximum)
}

const fn pinnacle_prestige_hit_penalty(prestige_level: i32) -> f64 {
    match prestige_level {
        i32::MIN..=0 => 0.0,
        1 => 0.03,
        2 => 0.05,
        3 => 0.08,
        4 => 0.11,
        5 => 0.14,
        6 => 0.165,
        _ => 0.19,
    }
}

fn has_attempt_effect(effects: &CombatEffects) -> bool {
    effects.trophy_venom_rounds > 0
        || effects.trophy_lifesteal
        || effects.trophy_regrowth
        || effects.trophy_forewarned
        || effects.trophy_laststand
        || effects.relic_berserk_rage.is_some()
        || effects.relic_double_hit
        || effects.relic_deaths_door
        || effects.relic_paper_crane
        || effects.relic_bottled_quake
        || effects.gear_reflect_first_hit
        || effects.gear_block_first_status
        || effects.gear_springheel_counter
        || effects.gear_block_first_skip
        || effects.gear_heal_first_crit
        || effects.gear_reroll_first_miss
}

fn pinnacle_total_multiplier(
    risk_tier: RiskTier,
    win_chance: f64,
) -> Result<f64, DigBossRuntimeError> {
    let authored = match risk_tier {
        RiskTier::Cautious => 2.5,
        RiskTier::Bold => 3.8,
        RiskTier::Reckless => 7.5,
    };
    let base = WagerMultiplier::new(authored)
        .map_err(|error| DigBossRuntimeError::InvalidPinnacle(error.to_string()))?;
    let chance = WinChance::new(win_chance)
        .map_err(|error| DigBossRuntimeError::InvalidPinnacle(error.to_string()))?;
    Ok(settled_wager_multiplier(effective_wager_multiplier(base, chance), None).value())
}

fn pinnacle_duel_win_probability(input: DuelOddsInput, pet_bonus_percent: i32) -> f64 {
    duel_win_probability_with_damage_bonus(input, pet_bonus_percent)
}

fn gear_presentation_name(gear_id: i64, slot: &str, tier: i64, item_id: Option<&str>) -> String {
    if let Some(definition) = item_id.and_then(unique_gear) {
        return definition.name.to_owned();
    }
    let Some(slot) = DomainGearSlot::parse(slot) else {
        return format!("Gear #{gear_id}");
    };
    let Ok(tier) = u8::try_from(tier) else {
        return format!("Gear #{gear_id}");
    };
    tier_table(slot)
        .and_then(|table| table.get(usize::from(tier)))
        .map_or_else(
            || format!("Gear #{gear_id}"),
            |definition| definition.name.to_owned(),
        )
}

fn finish_composed<E: EntropyPort, T>(
    service: &mut ComposedBossService<E>,
    outcome: Result<T, BossServiceError>,
) -> Result<DigBossRuntimeResult<T>, DigBossRuntimeError> {
    if let Some(error) = service.repository.take_error() {
        return Err(DigBossRuntimeError::Infrastructure(error));
    }
    if let Some(error) = service.take_preparation_activation_error() {
        return Err(DigBossRuntimeError::Infrastructure(error));
    }
    let outcome = outcome?;
    let combat_preparation_error = service.take_combat_preparation_error();
    let mut warnings = std::mem::take(&mut service.economy_event.warnings);
    warnings.extend(std::mem::take(&mut service.bankruptcy.warnings));
    if let Some(error) = service.gear_repair.take_error() {
        warnings.push(error);
    }
    if let Some(error) = combat_preparation_error {
        warnings.push(error);
    }
    Ok(DigBossRuntimeResult {
        outcome,
        action_id: service.repository.last_action_id(),
        abandoned_action_id: None,
        abandoned_wager_forfeit: 0,
        abandoned_gear_wear: GearWearResult::default(),
        gear_drop: None,
        prestige_relic_drop: None,
        broken_gear: Vec::new(),
        warnings,
        notices: std::mem::take(&mut service.outcome_sink.notices),
    })
}

#[cfg(test)]
#[path = "dig_boss_runtime/tests.rs"]
mod tests;
