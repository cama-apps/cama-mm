//! Application orchestration for Camagotchi pets.
//!
//! This layer owns time-sensitive workflows, lazy starvation, gacha policy,
//! care events, match hooks, and refund sweeps. Persistence, randomness, time,
//! and evolution are explicit ports so the behavior remains testable without
//! Discord, a network connection, or a second schema authority.

use std::collections::BTreeMap;

use cama_db::pet_repository::{
    AdoptPetRequest, AegisReviveRequest, AnchorGuard, BuySuppliesRequest, DeathClaimRequest,
    FeedPetRequest, HungerTopUpRequest, PetRepositoryError, PetRepositoryFailure,
    SacrificePetOutcome as RepositorySacrificeOutcome, SacrificePetRequest, SaltLickRequest,
    TrinketPurchaseOutcome, TrinketPurchaseRequest, UpgradeEggRequest,
};
use cama_domain::game_date::{GameDateError, game_date_for_timestamp};
use cama_domain::pet::{
    ACCESSORIES, AEGIS_REVIVE_HUNGER, DIG_WORK_CAP_BLOCKS, DIG_WORK_RATE_CONTENT,
    DIG_WORK_RATE_HAPPY, DIG_WORK_RATE_HUNGRY, DIG_WORK_UNITS_PER_BLOCK, DeathNotice,
    EGG_HATCH_SECONDS, EvolutionNotice, FEED_CAP_PER_DAY, FOOD_ITEMS, FeedOutcome,
    GILDED_EGG_PREMIUM, GILDED_TIER_WEIGHTS, HatchNotice, MATCH_WIN_HUNGER, MAX_BUY_QTY,
    MAX_HUNGER, PET_NAME_MAX_LEN, PITY_THRESHOLD, PITY_TIER_WEIGHTS, Pet, PetDigWork, PetError,
    PetMood, PetStage, PetStatus, REFUND_MULT_MAX, REFUND_MULT_MIN, RefundNotice, RefundPayout,
    SALT_LICK, SALT_LICK_DURATION_SECONDS, SPECIES, SUPPLY_STACK_CAP, SpeciesTier, TRINKET_COST,
    TRINKET_DUPE_REFUND, TierWeights, TrinketOutcome, UNHATCHED_SPECIES, WARNING_HUNGER,
    adoption_fee, food_cost, get_species, sacrifice_tier_weights, species_ids_by_tier,
};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::service_result::ServiceResult;
use chrono::{Datelike, NaiveDate};
use thiserror::Error;

pub const VALIDATION_ERROR: &str = "validation_error";
pub const PLAYER_NOT_FOUND: &str = "player_not_found";
pub const INSUFFICIENT_FUNDS: &str = "insufficient_funds";
pub const ALREADY_HAS_PET: &str = "already_has_pet";
pub const NO_PET: &str = "no_pet";
pub const PET_DEAD: &str = "pet_dead";
pub const PET_EGG: &str = "pet_egg";
pub const PET_HATCHED: &str = "pet_hatched";
pub const ALREADY_GILDED: &str = "already_gilded";
pub const PET_FULL: &str = "pet_full";
pub const NO_SUPPLIES: &str = "no_supplies";
pub const FEED_CAP: &str = "feed_cap";
pub const ALREADY_PAMPERED: &str = "already_pampered";
pub const STACK_CAP: &str = "stack_cap";
pub const BRAWL_BUSY: &str = "brawl_busy";

const FORBIDDEN_NAME_FRAGMENTS: [&str; 4] = ["@", "http://", "https://", "discord.gg"];
const DAY_SECONDS: i64 = 86_400;

#[derive(Debug, Error)]
pub enum PetApplicationError {
    #[error(transparent)]
    Repository(#[from] PetRepositoryError),
    #[error(transparent)]
    Domain(#[from] PetError),
    #[error(transparent)]
    GameDate(#[from] GameDateError),
    #[error("pet is still an egg")]
    PetEgg,
    #[error("timestamp is outside chrono's supported date range")]
    TimestampOutOfRange,
}

pub trait Clock {
    fn now(&self) -> i64;
}

/// Production wall clock for the live pet command and sweep adapters.
///
/// Keeping this implementation in the application crate lets the runtime
/// compose the same [`PetService`] used by deterministic tests without
/// duplicating time policy in the Discord layer.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPetClock;

impl Clock for SystemPetClock {
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

/// Economy identity gate. Balance-changing pet writes remain on [`PetStore`]
/// because Python commits the pet row, player balance, and ledger entry in one
/// SQLite transaction; splitting those writes across ports would lose parity.
pub trait PlayerPort {
    fn exists(&mut self, discord_id: i64, guild_id: i64) -> bool;
}

/// Entropy/flavor selection port for species, accessories, quirks, and refund
/// copy inputs. Rendering and Discord delivery intentionally stay outside the
/// application service.
pub trait PetRandom {
    /// Choose one id from explicit integer weights.
    fn weighted_id(&mut self, candidates: &[(&str, i64)]) -> String;

    /// Perform Python's two-stage tier then uniform-species roll.
    fn tiered_species(&mut self, weights: TierWeights) -> String;

    /// Inclusive integer sampling.
    fn integer(&mut self, minimum: i64, maximum: i64) -> i64;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetCareEvent {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub activity: PetActivity,
    pub source_key: String,
    pub occurred_at: i64,
}

/// Event/evolution collaborator. Activity recording is fire-and-forget just as
/// in Python; due/unannounced work remains bounded by the sweep caller.
pub trait PetEvolutionPort {
    fn resolve_if_due(&mut self, pet: Pet, _alive_through: i64) -> Pet {
        pet
    }

    fn record_activity(&mut self, _event: PetCareEvent) {}

    fn find_due(&mut self, _now: i64) -> Vec<Pet> {
        Vec::new()
    }

    fn get_unannounced(&mut self, _limit: i64) -> Vec<EvolutionNotice> {
        Vec::new()
    }
}

pub trait PetStore {
    fn get_active_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError>;

    fn get_pet_by_id(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError>;

    fn get_latest_dead_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError>;

    fn get_supplies(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<String, i64>, PetRepositoryError>;

    fn count_dead_pets(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, PetRepositoryError>;

    fn adopt_pet(&mut self, request: &AdoptPetRequest<'_>) -> Result<Pet, PetRepositoryError>;

    fn upgrade_egg(&mut self, request: &UpgradeEggRequest<'_>) -> Result<Pet, PetRepositoryError>;

    fn sacrifice_and_adopt_atomic(
        &mut self,
        request: &SacrificePetRequest<'_>,
    ) -> Result<RepositorySacrificeOutcome, PetRepositoryError>;

    fn resolve_hatch_species(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        species: &str,
        now: i64,
    ) -> Result<Pet, PetRepositoryError>;

    fn recent_dead_species(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<String>, PetRepositoryError>;

    fn buy_supplies(&mut self, request: &BuySuppliesRequest<'_>)
    -> Result<i64, PetRepositoryError>;

    fn feed_pet(&mut self, request: &FeedPetRequest<'_>) -> Result<Pet, PetRepositoryError>;

    fn use_salt_lick(&mut self, request: &SaltLickRequest<'_>) -> Result<Pet, PetRepositoryError>;

    fn buy_trinket_atomic(
        &mut self,
        request: &TrinketPurchaseRequest<'_>,
    ) -> Result<TrinketPurchaseOutcome, PetRepositoryError>;

    fn equip_accessory(
        &mut self,
        pet_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: Option<&str>,
    ) -> Result<(), PetRepositoryError>;

    fn get_accessories(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<String>, PetRepositoryError>;

    fn species_raised(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Vec<String>, PetRepositoryError>;

    fn get_active_pets_for(
        &mut self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<Pet>, PetRepositoryError>;

    fn apply_hunger_top_up(
        &mut self,
        request: HungerTopUpRequest,
    ) -> Result<bool, PetRepositoryError>;

    fn claim_death(&mut self, request: &DeathClaimRequest<'_>) -> Result<bool, PetRepositoryError>;

    fn revive_with_aegis(
        &mut self,
        request: AegisReviveRequest,
    ) -> Result<bool, PetRepositoryError>;

    fn find_starvation_candidates(
        &mut self,
        now: i64,
        decay_per_day: i64,
        max_decay_pct: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError>;

    fn get_unannounced_deaths(&mut self, limit: i64) -> Result<Vec<Pet>, PetRepositoryError>;

    fn find_unannounced_hatches(
        &mut self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError>;

    fn mark_death_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError>;

    fn mark_hatch_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError>;

    fn list_pet_guild_ids(&mut self) -> Result<Vec<i64>, PetRepositoryError>;

    fn list_week_care(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<Vec<Pet>, PetRepositoryError>;

    fn is_refund_window_paid(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<bool, PetRepositoryError>;

    fn get_nonprofit_balance(&mut self, guild_id: Option<i64>) -> Result<i64, PetRepositoryError>;

    fn pay_refunds_atomic(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
        payouts: &[(i64, i64)],
        now: i64,
        announcement: Option<&RefundNotice>,
    ) -> Result<bool, PetRepositoryError>;

    fn get_unannounced_refunds(
        &mut self,
        limit: i64,
    ) -> Result<Vec<RefundNotice>, PetRepositoryError>;

    fn mark_refund_announced(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptOutcome {
    pub pet: Pet,
    pub egg_tier: String,
    pub pity_active: bool,
    pub upgraded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuyOutcome {
    pub item_id: String,
    pub qty: i64,
    pub total_cost: i64,
    pub new_qty: Option<i64>,
    pub pampered_until: Option<i64>,
    pub pet: Option<Pet>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SacrificePreview {
    pub pet: Pet,
    pub tier: SpeciesTier,
    pub was_adult: bool,
    pub fee: i64,
    pub weights: TierWeights,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SacrificeOutcome {
    pub dead_pet: Pet,
    pub new_pet: Pet,
    pub fee: i64,
    pub tier: SpeciesTier,
    pub was_adult: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    pub deaths: Vec<DeathNotice>,
    pub hatches: Vec<HatchNotice>,
    pub evolutions: Vec<EvolutionNotice>,
    pub refunds: Vec<RefundNotice>,
}

pub struct PetService<S, P, R, C, E> {
    store: S,
    players: P,
    random: R,
    clock: C,
    evolution: E,
    decay_per_day: i64,
}

impl<S, P, R, C, E> PetService<S, P, R, C, E>
where
    S: PetStore,
    P: PlayerPort,
    R: PetRandom,
    C: Clock,
    E: PetEvolutionPort,
{
    #[must_use]
    pub const fn new(
        store: S,
        players: P,
        random: R,
        clock: C,
        evolution: E,
        decay_per_day: i64,
    ) -> Self {
        Self {
            store,
            players,
            random,
            clock,
            evolution,
            decay_per_day,
        }
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    pub const fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    #[must_use]
    pub const fn random(&self) -> &R {
        &self.random
    }

    pub const fn random_mut(&mut self) -> &mut R {
        &mut self.random
    }

    #[must_use]
    pub const fn evolution(&self) -> &E {
        &self.evolution
    }

    pub const fn evolution_mut(&mut self) -> &mut E {
        &mut self.evolution
    }

    #[must_use]
    pub const fn decay_per_day(&self) -> i64 {
        self.decay_per_day
    }

    #[must_use]
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    fn dig_work_settlement(&self, pet: &Pet, now: i64) -> Result<(i64, i64), PetError> {
        let settled_at = if let Some(died_at) = pet.died_at {
            now.min(died_at)
        } else {
            now.min(pet.starvation_time(self.decay_per_day)?)
        }
        .max(pet.dig_work_at);
        let accrued =
            pet.dig_work_units_between(pet.dig_work_at, settled_at, self.decay_per_day)?;
        let cap = DIG_WORK_CAP_BLOCKS * DIG_WORK_UNITS_PER_BLOCK;
        Ok(((pet.dig_work_units + accrued).min(cap), settled_at))
    }

    fn dig_work_rate(&self, pet: &Pet, now: i64) -> Result<i64, PetError> {
        if now < pet.hatched_at || pet.is_starved(now, self.decay_per_day)? {
            return Ok(0);
        }
        Ok(match pet.mood(now, self.decay_per_day) {
            PetMood::Happy => DIG_WORK_RATE_HAPPY,
            PetMood::Content => DIG_WORK_RATE_CONTENT,
            PetMood::Hungry => DIG_WORK_RATE_HUNGRY,
            PetMood::Starving => 0,
        })
    }

    fn resolve_hatch(&mut self, pet: Pet, now: i64) -> Result<Pet, PetApplicationError> {
        if pet.species != UNHATCHED_SPECIES || now < pet.hatched_at {
            return Ok(pet);
        }
        let species = if pet.egg_tier == "gilded" {
            self.random.tiered_species(GILDED_TIER_WEIGHTS)
        } else if self.pity_active(pet.discord_id, Some(pet.guild_id))? {
            self.random.tiered_species(PITY_TIER_WEIGHTS)
        } else {
            let choices = SPECIES
                .iter()
                .map(|species| (species.species_id, species.weight))
                .collect::<Vec<_>>();
            self.random.weighted_id(&choices)
        };
        self.store
            .resolve_hatch_species(pet.pet_id, Some(pet.guild_id), &species, now)
            .map_err(Into::into)
    }

    pub fn resolve_starvation(
        &mut self,
        pet: Pet,
        now: i64,
    ) -> Result<Option<Pet>, PetApplicationError> {
        let mut current = Some(self.resolve_hatch(pet, now)?);
        for _ in 0..4 {
            let Some(mut pet) = current else {
                return Ok(None);
            };
            if pet.died_at.is_some() {
                return Ok(None);
            }
            let starvation_at = pet.starvation_time(self.decay_per_day)?;
            pet = self
                .evolution
                .resolve_if_due(pet, now.min(starvation_at.saturating_sub(1)));
            if pet.died_at.is_some() {
                return Ok(None);
            }
            if !pet.is_starved(now, self.decay_per_day)? {
                return Ok(Some(pet));
            }
            let (work_units, work_at) = self.dig_work_settlement(&pet, starvation_at)?;
            let anchor = AnchorGuard {
                expected_last_fed_at: pet.last_fed_at,
                expected_hunger: pet.hunger_at_last_fed,
                expected_dig_work_units: Some(pet.dig_work_units),
                expected_dig_work_at: Some(pet.dig_work_at),
                new_dig_work_units: Some(work_units),
                new_dig_work_at: Some(work_at),
            };
            if get_species(&pet.species).has_aegis && pet.aegis_used == 0 {
                let _revived = self.store.revive_with_aegis(AegisReviveRequest {
                    pet_id: pet.pet_id,
                    guild_id: Some(pet.guild_id),
                    revived_last_fed_at: starvation_at,
                    revive_hunger: AEGIS_REVIVE_HUNGER,
                    anchor,
                })?;
                current = self.store.get_pet_by_id(pet.pet_id, Some(pet.guild_id))?;
                continue;
            }
            if self.store.claim_death(&DeathClaimRequest {
                pet_id: pet.pet_id,
                guild_id: Some(pet.guild_id),
                died_at: starvation_at,
                cause: "starvation",
                anchor,
            })? {
                return Ok(None);
            }
            current = self.store.get_pet_by_id(pet.pet_id, Some(pet.guild_id))?;
        }
        Ok(None)
    }

    pub fn living_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Option<Pet>, PetApplicationError> {
        let Some(pet) = self.store.get_active_pet(discord_id, guild_id)? else {
            return Ok(None);
        };
        self.resolve_starvation(pet, now)
    }

    /// Return the passive work offer for one admitted Dig.
    ///
    /// Lifecycle resolution intentionally happens before the offer is built:
    /// Python's `PetService.preview_dig_work` goes through `living_pet`, so a
    /// ready egg hatches and a stale starving pet is revived or claimed dead
    /// before Dig can inspect its work.  Healthy passive work remains a
    /// preview-only calculation; the Dig commit owns the subsequent CAS
    /// claim that advances `dig_work_units` and `dig_work_at`.
    pub fn preview_dig_work(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Option<PetDigWork>, PetApplicationError> {
        let Some(pet) = self.living_pet(discord_id, guild_id, now)? else {
            return Ok(None);
        };
        if now < pet.hatched_at {
            return Ok(None);
        }
        let (accrued_units, as_of) = self.dig_work_settlement(&pet, now)?;
        Ok(Some(PetDigWork {
            pet_id: pet.pet_id,
            pet_name: pet.name,
            expected_units: pet.dig_work_units,
            expected_at: pet.dig_work_at,
            accrued_units,
            as_of,
        }))
    }

    pub fn get_status(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<PetStatus> {
        match self.get_status_inner(discord_id, guild_id) {
            Ok(status) => ServiceResult::ok(status),
            Err(error) => internal_failure(error),
        }
    }

    fn get_status_inner(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<PetStatus, PetApplicationError> {
        let now = self.clock.now();
        let pet = self.living_pet(discord_id, guild_id, now)?;
        let supplies = self.store.get_supplies(discord_id, guild_id)?;
        let Some(pet) = pet else {
            return Ok(PetStatus {
                pet: None,
                hunger: 0,
                stage: None,
                mood: None,
                age_seconds: 0,
                supplies: Some(supplies),
                last_dead: self.store.get_latest_dead_pet(discord_id, guild_id)?,
                dig_work_units: 0,
                dig_work_rate: 0,
                evolution_hint: None,
            });
        };
        let (work_units, _) = self.dig_work_settlement(&pet, now)?;
        Ok(PetStatus {
            hunger: pet.current_hunger(now, self.decay_per_day),
            stage: Some(pet.stage(now)),
            mood: Some(pet.mood(now, self.decay_per_day)),
            age_seconds: pet.age_seconds(now),
            supplies: Some(supplies),
            last_dead: None,
            dig_work_units: work_units,
            dig_work_rate: self.dig_work_rate(&pet, now)?,
            evolution_hint: None,
            pet: Some(pet),
        })
    }

    pub fn get_warning_crossing(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<(i64, String)>, PetApplicationError> {
        let now = self.clock.now();
        let Some(pet) = self.living_pet(discord_id, guild_id, now)? else {
            return Ok(None);
        };
        if now < pet.hatched_at {
            return Ok(None);
        }
        Ok(Some((
            pet.hunger_crossing_time(WARNING_HUNGER, self.decay_per_day)?,
            pet.name,
        )))
    }

    pub fn adopt(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        egg_tier: &str,
    ) -> ServiceResult<AdoptOutcome> {
        if let Some(error) = validate_name(name) {
            return error;
        }
        if !matches!(egg_tier, "standard" | "gilded") {
            return ServiceResult::fail_with_code("Unknown egg tier.", VALIDATION_ERROR);
        }
        let normalized_guild = guild_id.unwrap_or(0);
        if !self.players.exists(discord_id, normalized_guild) {
            return ServiceResult::fail_with_code(
                "Register with the league before adopting a pet.",
                PLAYER_NOT_FOUND,
            );
        }
        match self.adopt_inner(discord_id, guild_id, name.trim(), egg_tier) {
            Ok(outcome) => ServiceResult::ok(outcome),
            Err(PetApplicationError::Repository(error)) => map_repo_error(error, None),
            Err(error) => internal_failure(error),
        }
    }

    fn adopt_inner(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        egg_tier: &str,
    ) -> Result<AdoptOutcome, PetApplicationError> {
        let now = self.clock.now();
        if let Some(existing) = self.living_pet(discord_id, guild_id, now)? {
            if egg_tier == "gilded" && now < existing.hatched_at && existing.egg_tier == "standard"
            {
                let pet = self.store.upgrade_egg(&UpgradeEggRequest {
                    discord_id,
                    guild_id,
                    name,
                    premium: GILDED_EGG_PREMIUM,
                    now,
                })?;
                return Ok(AdoptOutcome {
                    pet,
                    egg_tier: "gilded".to_owned(),
                    pity_active: false,
                    upgraded: true,
                });
            }
            return Err(PetApplicationError::Repository(
                PetRepositoryError::Rejected(PetRepositoryFailure::AlreadyHasPet),
            ));
        }
        let prior_deaths = self.store.count_dead_pets(discord_id, guild_id)?;
        let mut fee = adoption_fee(prior_deaths);
        let pity_active = egg_tier == "standard" && self.pity_active(discord_id, guild_id)?;
        if egg_tier == "gilded" {
            fee += GILDED_EGG_PREMIUM;
        }
        let pet = self.store.adopt_pet(&AdoptPetRequest {
            discord_id,
            guild_id,
            name,
            egg_tier,
            fee,
            now,
            hatch_seconds: EGG_HATCH_SECONDS,
        })?;
        Ok(AdoptOutcome {
            pet,
            egg_tier: egg_tier.to_owned(),
            pity_active,
            upgraded: false,
        })
    }

    pub fn pity_active(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, PetApplicationError> {
        let recent = self
            .store
            .recent_dead_species(discord_id, guild_id, PITY_THRESHOLD)?;
        Ok(
            i64::try_from(recent.len()).unwrap_or(i64::MAX) >= PITY_THRESHOLD
                && recent
                    .iter()
                    .all(|species| get_species(species).tier == SpeciesTier::Common),
        )
    }

    /// Return the exact altar offer before Discord asks for confirmation.
    pub fn sacrifice_preview(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SacrificePreview> {
        match self.sacrifice_preview_inner(discord_id, guild_id) {
            Ok(preview) => ServiceResult::ok(preview),
            Err(PetApplicationError::Repository(PetRepositoryError::Rejected(
                PetRepositoryFailure::NoPet,
            ))) => ServiceResult::fail_with_code("You have no living pet to sacrifice.", NO_PET),
            Err(PetApplicationError::Repository(PetRepositoryError::Rejected(
                PetRepositoryFailure::EggNotReady,
            ))) => ServiceResult::fail_with_code(
                "You can't sacrifice a mystery. Let it hatch first.",
                PET_EGG,
            ),
            Err(error) => internal_failure(error),
        }
    }

    fn sacrifice_preview_inner(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<SacrificePreview, PetApplicationError> {
        let now = self.clock.now();
        let Some(pet) = self.living_pet(discord_id, guild_id, now)? else {
            return Err(PetRepositoryError::Rejected(PetRepositoryFailure::NoPet).into());
        };
        if now < pet.hatched_at {
            return Err(PetApplicationError::Repository(
                PetRepositoryError::Rejected(PetRepositoryFailure::EggNotReady),
            ));
        }
        let tier = get_species(&pet.species).tier;
        let was_adult = pet.stage(now) == PetStage::Adult;
        let fee = adoption_fee(
            self.store
                .count_dead_pets(discord_id, guild_id)?
                .saturating_add(1),
        );
        Ok(SacrificePreview {
            pet,
            tier,
            was_adult,
            fee,
            weights: sacrifice_tier_weights(tier, was_adult),
        })
    }

    /// Give the current cama to the altar and create its rebirth egg. One
    /// retry mirrors the feed path when a concurrent anchor update wins.
    pub fn sacrifice(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
    ) -> ServiceResult<SacrificeOutcome> {
        if let Some(error) = validate_name(name) {
            return error;
        }
        let name = name.trim();
        for _ in 0..2 {
            let preview = match self.sacrifice_preview_inner(discord_id, guild_id) {
                Ok(preview) => preview,
                Err(PetApplicationError::Repository(PetRepositoryError::Rejected(
                    PetRepositoryFailure::NoPet,
                ))) => {
                    return ServiceResult::fail_with_code(
                        "You have no living pet to sacrifice.",
                        NO_PET,
                    );
                }
                Err(PetApplicationError::Repository(PetRepositoryError::Rejected(
                    PetRepositoryFailure::EggNotReady,
                ))) => {
                    return ServiceResult::fail_with_code(
                        "You can't sacrifice a mystery. Let it hatch first.",
                        PET_EGG,
                    );
                }
                Err(error) => return internal_failure(error),
            };
            let now = self.clock.now();
            let species = self.random.tiered_species(preview.weights);
            match self.store.sacrifice_and_adopt_atomic(&SacrificePetRequest {
                discord_id,
                guild_id,
                old_pet_id: preview.pet.pet_id,
                expected_last_fed_at: preview.pet.last_fed_at,
                expected_hunger: preview.pet.hunger_at_last_fed,
                name,
                species: &species,
                fee: preview.fee,
                now,
                hatch_seconds: EGG_HATCH_SECONDS,
            }) {
                Ok(outcome) => {
                    return ServiceResult::ok(SacrificeOutcome {
                        dead_pet: outcome.dead_pet,
                        new_pet: outcome.new_pet,
                        fee: preview.fee,
                        tier: preview.tier,
                        was_adult: preview.was_adult,
                    });
                }
                Err(error) if error.failure() == Some(PetRepositoryFailure::StalePet) => {}
                Err(error) => return map_repo_error(error, Some(preview.fee)),
            }
        }
        ServiceResult::fail_with_code(
            "Your pet's state changed mid-ritual — try again.",
            VALIDATION_ERROR,
        )
    }

    pub fn buy(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        quantity: i64,
    ) -> ServiceResult<BuyOutcome> {
        if item_id == SALT_LICK.item_id {
            return self.buy_salt_lick(discord_id, guild_id, quantity);
        }
        let Some(item) = FOOD_ITEMS.iter().find(|item| item.item_id == item_id) else {
            return ServiceResult::fail_with_code("Unknown item.", VALIDATION_ERROR);
        };
        if !(1..=MAX_BUY_QTY).contains(&quantity) {
            return ServiceResult::fail_with_code(
                format!("Quantity must be between 1 and {MAX_BUY_QTY}."),
                VALIDATION_ERROR,
            );
        }
        let now = self.clock.now();
        let pet = match self.living_pet(discord_id, guild_id, now) {
            Ok(pet) => pet,
            Err(error) => return internal_failure(error),
        };
        let species_id = pet
            .as_ref()
            .filter(|pet| now >= pet.hatched_at)
            .map_or("", |pet| pet.species.as_str());
        let unit_cost = match food_cost(species_id, item.item_id) {
            Ok(cost) => cost,
            Err(error) => return internal_failure(error.into()),
        };
        let Some(total_cost) = unit_cost.checked_mul(quantity) else {
            return ServiceResult::fail_with_code("Pet shop total is too large.", VALIDATION_ERROR);
        };
        match self.store.buy_supplies(&BuySuppliesRequest {
            discord_id,
            guild_id,
            item_id,
            qty: quantity,
            total_cost,
            now,
            stack_cap: SUPPLY_STACK_CAP,
        }) {
            Ok(new_quantity) => ServiceResult::ok(BuyOutcome {
                item_id: item_id.to_owned(),
                qty: quantity,
                total_cost,
                new_qty: Some(new_quantity),
                pampered_until: None,
                pet: None,
            }),
            Err(error) => map_repo_error(error, Some(total_cost)),
        }
    }

    fn buy_salt_lick(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        quantity: i64,
    ) -> ServiceResult<BuyOutcome> {
        if quantity != 1 {
            return ServiceResult::fail_with_code(
                "Salt licks are applied immediately — buy one at a time.",
                VALIDATION_ERROR,
            );
        }
        match self.buy_salt_lick_inner(discord_id, guild_id) {
            Ok(outcome) => ServiceResult::ok(outcome),
            Err(PetApplicationError::PetEgg) => ServiceResult::fail_with_code(
                "It's still an egg. Eggs are beyond flattery.",
                PET_EGG,
            ),
            Err(PetApplicationError::Repository(error)) => map_repo_error(error, None),
            Err(error) => internal_failure(error),
        }
    }

    fn buy_salt_lick_inner(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BuyOutcome, PetApplicationError> {
        let now = self.clock.now();
        let Some(pet) = self.living_pet(discord_id, guild_id, now)? else {
            return Err(PetApplicationError::Repository(
                PetRepositoryError::Rejected(PetRepositoryFailure::NoPet),
            ));
        };
        if now < pet.hatched_at {
            return Err(PetApplicationError::PetEgg);
        }
        let species = get_species(&pet.species);
        let duration = SALT_LICK_DURATION_SECONDS * species.salt_lick_pct / 100;
        let cost = food_cost(&pet.species, SALT_LICK.item_id)?;
        let (work_units, work_at) = self.dig_work_settlement(&pet, now)?;
        let fresh = self.store.use_salt_lick(&SaltLickRequest {
            pet_id: pet.pet_id,
            discord_id,
            guild_id: Some(pet.guild_id),
            cost,
            now,
            pampered_until: now + duration,
            week_key: &week_key_for_timestamp(now)?,
            anchor: Some(AnchorGuard {
                expected_last_fed_at: pet.last_fed_at,
                expected_hunger: pet.hunger_at_last_fed,
                expected_dig_work_units: Some(pet.dig_work_units),
                expected_dig_work_at: Some(pet.dig_work_at),
                new_dig_work_units: Some(work_units),
                new_dig_work_at: Some(work_at),
            }),
        })?;
        self.evolution.record_activity(PetCareEvent {
            discord_id,
            guild_id,
            activity: PetActivity::PetCared,
            source_key: format!("pet-care:{}:salt:{now}", fresh.pet_id),
            occurred_at: now,
        });
        Ok(BuyOutcome {
            item_id: SALT_LICK.item_id.to_owned(),
            qty: 1,
            total_cost: cost,
            new_qty: None,
            pampered_until: fresh.pampered_until,
            pet: Some(fresh),
        })
    }

    pub fn feed(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
    ) -> ServiceResult<FeedOutcome> {
        let Some(item) = FOOD_ITEMS.iter().find(|item| item.item_id == item_id) else {
            return ServiceResult::fail_with_code("Unknown food.", VALIDATION_ERROR);
        };
        let mut stale = false;
        for _ in 0..2 {
            let now = self.clock.now();
            let pet = match self.living_pet(discord_id, guild_id, now) {
                Ok(Some(pet)) => pet,
                Ok(None) => {
                    return ServiceResult::fail_with_code("You have no living pet.", NO_PET);
                }
                Err(error) => return internal_failure(error),
            };
            if now < pet.hatched_at {
                return ServiceResult::fail_with_code(
                    "It's still an egg. It absorbs patience, not tangos.",
                    PET_EGG,
                );
            }
            let current_hunger = pet.current_hunger(now, self.decay_per_day);
            if current_hunger >= MAX_HUNGER {
                return ServiceResult::fail_with_code(
                    format!("{} is completely full.", pet.name),
                    PET_FULL,
                );
            }
            let species = get_species(&pet.species);
            let spat = species.spit_one_in > 0 && self.random.integer(1, species.spit_one_in) == 1;
            let new_hunger = if spat {
                pet.hunger_at_last_fed
            } else {
                pet.restored_hunger(now, self.decay_per_day, item.restore)
            };
            let (work_units, work_at) = match self.dig_work_settlement(&pet, now) {
                Ok(settlement) => settlement,
                Err(error) => return internal_failure(error.into()),
            };
            let feed_date = match game_date_for_timestamp(now as f64) {
                Ok(date) => date,
                Err(error) => return internal_failure(error.into()),
            };
            let week_key = match week_key_for_timestamp(now) {
                Ok(key) => key,
                Err(error) => return internal_failure(error),
            };
            let consumed_jc = match food_cost(&pet.species, item_id) {
                Ok(cost) => cost,
                Err(error) => return internal_failure(error.into()),
            };
            let request = FeedPetRequest {
                pet_id: pet.pet_id,
                discord_id,
                guild_id: Some(pet.guild_id),
                item_id,
                anchor: AnchorGuard {
                    expected_last_fed_at: pet.last_fed_at,
                    expected_hunger: pet.hunger_at_last_fed,
                    expected_dig_work_units: Some(pet.dig_work_units),
                    expected_dig_work_at: Some(pet.dig_work_at),
                    new_dig_work_units: Some(work_units),
                    new_dig_work_at: Some(work_at),
                },
                new_last_fed_at: now,
                new_hunger,
                feed_date: &feed_date,
                feed_cap: FEED_CAP_PER_DAY,
                week_key: &week_key,
                consumed_jc,
                spat,
            };
            let fresh = match self.store.feed_pet(&request) {
                Ok(pet) => pet,
                Err(error) if error.failure() == Some(PetRepositoryFailure::StalePet) => {
                    stale = true;
                    continue;
                }
                Err(error) => return map_repo_error(error, None),
            };
            let supplies = match self.store.get_supplies(discord_id, guild_id) {
                Ok(supplies) => supplies,
                Err(error) => return map_repo_error(error, None),
            };
            self.evolution.record_activity(PetCareEvent {
                discord_id,
                guild_id,
                activity: PetActivity::PetCared,
                source_key: format!(
                    "pet-care:{}:{}:{}",
                    fresh.pet_id, fresh.last_fed_at, fresh.feeds_today
                ),
                occurred_at: now,
            });
            return ServiceResult::ok(FeedOutcome {
                new_hunger: fresh.current_hunger(now, self.decay_per_day),
                remaining_qty: supplies.get(item_id).copied().unwrap_or(0),
                feeds_left_today: (FEED_CAP_PER_DAY - fresh.feeds_today).max(0),
                pet: fresh,
                item_id: item_id.to_owned(),
                old_hunger: current_hunger,
                spat,
            });
        }
        if stale {
            ServiceResult::fail_with_code("Your pet was busy — try again.", VALIDATION_ERROR)
        } else {
            ServiceResult::fail_with_code("Unable to feed pet.", VALIDATION_ERROR)
        }
    }

    pub fn roll_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<TrinketOutcome> {
        let now = self.clock.now();
        let pet = match self.living_pet(discord_id, guild_id, now) {
            Ok(Some(pet)) => pet,
            Ok(None) => {
                return ServiceResult::fail_with_code(
                    "Trinkets are gifts — you need a living pet first.",
                    NO_PET,
                );
            }
            Err(error) => return internal_failure(error),
        };
        if now < pet.hatched_at {
            return ServiceResult::fail_with_code(
                "It's still an egg. Eggs have no fashion sense.",
                PET_EGG,
            );
        }
        let candidates = ACCESSORIES
            .iter()
            .map(|accessory| (accessory.accessory_id, accessory.weight))
            .collect::<Vec<_>>();
        let accessory_id = self.random.weighted_id(&candidates);
        match self.store.buy_trinket_atomic(&TrinketPurchaseRequest {
            pet_id: pet.pet_id,
            discord_id,
            guild_id,
            accessory_id: &accessory_id,
            cost: TRINKET_COST,
            refund: TRINKET_DUPE_REFUND,
            now,
        }) {
            Ok(outcome) => ServiceResult::ok(TrinketOutcome {
                accessory_id,
                duplicate: outcome.duplicate,
                net_cost: outcome.net_cost,
                owned_count: outcome.owned_count,
            }),
            Err(error) => map_repo_error(error, Some(TRINKET_COST)),
        }
    }

    pub fn wear_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: &str,
    ) -> ServiceResult<String> {
        if !ACCESSORIES
            .iter()
            .any(|accessory| accessory.accessory_id == accessory_id)
        {
            return ServiceResult::fail_with_code("Unknown trinket.", VALIDATION_ERROR);
        }
        let now = self.clock.now();
        let pet = match self.living_pet(discord_id, guild_id, now) {
            Ok(Some(pet)) => pet,
            Ok(None) => {
                return ServiceResult::fail_with_code("You have no living pet.", NO_PET);
            }
            Err(error) => return internal_failure(error),
        };
        match self
            .store
            .equip_accessory(pet.pet_id, discord_id, guild_id, Some(accessory_id))
        {
            Ok(()) => ServiceResult::ok(accessory_id.to_owned()),
            Err(error) => map_repo_error(error, None),
        }
    }

    pub fn camadex(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(Vec<String>, usize), PetApplicationError> {
        let now = self.clock.now();
        let _living = self.living_pet(discord_id, guild_id, now)?;
        Ok((
            self.store.species_raised(discord_id, guild_id, now)?,
            SPECIES.len(),
        ))
    }

    pub fn on_match_win(&mut self, winner_ids: &[i64], guild_id: Option<i64>) -> Vec<(Pet, i64)> {
        let Ok(pets) = self.store.get_active_pets_for(winner_ids, guild_id) else {
            return Vec::new();
        };
        let now = self.clock.now();
        let mut results = Vec::new();
        for pet in pets {
            let Ok(Some(living)) = self.resolve_starvation(pet, now) else {
                continue;
            };
            if now < living.hatched_at {
                continue;
            }
            let amount = MATCH_WIN_HUNGER + get_species(&living.species).match_feed_bonus;
            let current = living.current_hunger(now, self.decay_per_day);
            let new_hunger = (current + amount).min(MAX_HUNGER);
            let applied_amount = new_hunger - current;
            let Ok((work_units, work_at)) = self.dig_work_settlement(&living, now) else {
                continue;
            };
            let request = HungerTopUpRequest {
                pet_id: living.pet_id,
                guild_id: Some(living.guild_id),
                anchor: AnchorGuard {
                    expected_last_fed_at: living.last_fed_at,
                    expected_hunger: living.hunger_at_last_fed,
                    expected_dig_work_units: Some(living.dig_work_units),
                    expected_dig_work_at: Some(living.dig_work_at),
                    new_dig_work_units: Some(work_units),
                    new_dig_work_at: Some(work_at),
                },
                new_last_fed_at: now,
                new_hunger,
            };
            if self.store.apply_hunger_top_up(request).unwrap_or(false) {
                let mut fresh = living;
                fresh.last_fed_at = now;
                fresh.hunger_at_last_fed = new_hunger;
                fresh.dig_work_units = work_units;
                fresh.dig_work_at = work_at;
                results.push((fresh, applied_amount));
            }
        }
        results
    }

    pub fn sweep(&mut self, announcement_limit: i64) -> Result<SweepOutcome, PetApplicationError> {
        let now = self.clock.now();
        for pet in self.evolution.find_due(now) {
            let _resolved = self.resolve_starvation(pet, now)?;
        }
        let max_decay_pct = SPECIES
            .iter()
            .map(|species| species.decay_pct)
            .max()
            .unwrap_or(100);
        for pet in self
            .store
            .find_starvation_candidates(now, self.decay_per_day, max_decay_pct)?
        {
            let _resolved = self.resolve_starvation(pet, now)?;
        }
        let deaths = self
            .store
            .get_unannounced_deaths(announcement_limit)?
            .into_iter()
            .map(|pet| DeathNotice {
                pet,
                eating_outcome: None,
            })
            .collect();
        let mut hatches = Vec::new();
        for pet in self
            .store
            .find_unannounced_hatches(now, announcement_limit)?
        {
            hatches.push(HatchNotice {
                pet: self.resolve_hatch(pet, now)?,
            });
        }
        let evolutions = self.evolution.get_unannounced(announcement_limit);
        let refunds = self.sweep_refunds(now, announcement_limit)?;
        Ok(SweepOutcome {
            deaths,
            hatches,
            evolutions,
            refunds,
        })
    }

    pub fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), PetRepositoryError> {
        self.store
            .mark_death_announced(pet.pet_id, Some(pet.guild_id), self.clock.now())
    }

    pub fn mark_hatch_announced(&mut self, pet: &Pet) -> Result<(), PetRepositoryError> {
        self.store
            .mark_hatch_announced(pet.pet_id, Some(pet.guild_id), self.clock.now())
    }

    pub fn mark_refund_announced(
        &mut self,
        notice: &RefundNotice,
    ) -> Result<(), PetRepositoryError> {
        self.store
            .mark_refund_announced(Some(notice.guild_id), &notice.week_key, self.clock.now())
    }

    fn sweep_refunds(
        &mut self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<RefundNotice>, PetApplicationError> {
        let week_keys = [
            week_key_for_timestamp(now - 14 * DAY_SECONDS)?,
            week_key_for_timestamp(now - 7 * DAY_SECONDS)?,
        ];
        for guild_id in self.store.list_pet_guild_ids()? {
            for week_key in &week_keys {
                // One bad/short guild window cannot drop the global sweep.
                let _ignored = self.pay_guild_refunds(guild_id, week_key, now);
            }
        }
        self.store
            .get_unannounced_refunds(limit)
            .map_err(Into::into)
    }

    fn pay_guild_refunds(
        &mut self,
        guild_id: i64,
        week_key: &str,
        now: i64,
    ) -> Result<Option<RefundNotice>, PetApplicationError> {
        if self.store.is_refund_window_paid(Some(guild_id), week_key)? {
            return Ok(None);
        }
        let care = self.store.list_week_care(Some(guild_id), week_key)?;
        let mut payouts = Vec::new();
        for pet in care {
            let consumed = pet.consumed_in_week(week_key);
            if consumed <= 0 {
                continue;
            }
            let multiplier = self.random.integer(REFUND_MULT_MIN, REFUND_MULT_MAX)
                + get_species(&pet.species).refund_bonus_pp;
            payouts.push(RefundPayout {
                discord_id: pet.discord_id,
                consumed_jc: consumed,
                multiplier_pct: multiplier,
                amount: consumed * multiplier / 100,
            });
        }
        let total = payouts.iter().map(|payout| payout.amount).sum::<i64>();
        let fund = self.store.get_nonprofit_balance(Some(guild_id))?;
        let scaled_down = total > fund;
        if scaled_down && total > 0 {
            for payout in &mut payouts {
                payout.amount = payout.amount * fund / total;
            }
            payouts.retain(|payout| payout.amount > 0);
            if payouts.is_empty() {
                return Ok(None);
            }
        }
        let notice = RefundNotice {
            guild_id,
            week_key: week_key.to_owned(),
            total_paid: payouts.iter().map(|payout| payout.amount).sum(),
            payouts,
            scaled_down,
        };
        let transfer = notice
            .payouts
            .iter()
            .map(|payout| (payout.discord_id, payout.amount))
            .collect::<Vec<_>>();
        let paid = self.store.pay_refunds_atomic(
            Some(guild_id),
            week_key,
            &transfer,
            now,
            (!notice.payouts.is_empty()).then_some(&notice),
        )?;
        if paid && !notice.payouts.is_empty() {
            Ok(Some(notice))
        } else {
            Ok(None)
        }
    }
}

pub fn week_key_for_timestamp(timestamp: i64) -> Result<String, PetApplicationError> {
    let game_date = game_date_for_timestamp(timestamp as f64)?;
    let date = NaiveDate::parse_from_str(&game_date, "%Y-%m-%d")
        .map_err(|_| PetApplicationError::TimestampOutOfRange)?;
    let week = date.iso_week();
    Ok(format!("{}-W{:02}", week.year(), week.week()))
}

pub(crate) fn validate_name<T>(name: &str) -> Option<ServiceResult<T>> {
    let name = name.trim();
    let length = name.chars().count();
    if !(1..=usize::try_from(PET_NAME_MAX_LEN).unwrap_or(32)).contains(&length) {
        return Some(ServiceResult::fail_with_code(
            format!("Name must be 1-{PET_NAME_MAX_LEN} characters."),
            VALIDATION_ERROR,
        ));
    }
    if name.chars().any(char::is_control) {
        return Some(ServiceResult::fail_with_code(
            "Name contains unprintable characters.",
            VALIDATION_ERROR,
        ));
    }
    let lowered = name.to_lowercase();
    if FORBIDDEN_NAME_FRAGMENTS
        .iter()
        .any(|fragment| lowered.contains(fragment))
    {
        return Some(ServiceResult::fail_with_code(
            "No mentions or links in pet names.",
            VALIDATION_ERROR,
        ));
    }
    None
}

pub(crate) fn map_repo_error<T>(error: PetRepositoryError, fee: Option<i64>) -> ServiceResult<T> {
    let Some(failure) = error.failure() else {
        return ServiceResult::fail_with_code(format!("Pet error: {error}"), VALIDATION_ERROR);
    };
    let (message, code) = match failure {
        PetRepositoryFailure::InsufficientFunds => (
            fee.map_or_else(
                || "Not enough jopacoin.".to_owned(),
                |fee| format!("Not enough jopacoin (need {fee})."),
            ),
            INSUFFICIENT_FUNDS,
        ),
        PetRepositoryFailure::AlreadyHasPet => (
            "You already have a pet. One cama per household.".to_owned(),
            ALREADY_HAS_PET,
        ),
        PetRepositoryFailure::NoPet => ("You have no living pet.".to_owned(), NO_PET),
        PetRepositoryFailure::PetDead => ("Your pet has passed away.".to_owned(), PET_DEAD),
        PetRepositoryFailure::PetHatched => (
            "Only an unhatched egg can be upgraded.".to_owned(),
            PET_HATCHED,
        ),
        PetRepositoryFailure::AlreadyGilded => {
            ("Your egg is already gilded.".to_owned(), ALREADY_GILDED)
        }
        PetRepositoryFailure::NoSupplies => (
            "You're out of that food. Visit /pet shop.".to_owned(),
            NO_SUPPLIES,
        ),
        PetRepositoryFailure::FeedCap => (
            "Your pet has eaten enough today (4 feeds per day).".to_owned(),
            FEED_CAP,
        ),
        PetRepositoryFailure::AlreadyPampered => (
            "Your pet is already thoroughly pampered.".to_owned(),
            ALREADY_PAMPERED,
        ),
        PetRepositoryFailure::StackCap => {
            ("You can't hoard more of that item.".to_owned(), STACK_CAP)
        }
        PetRepositoryFailure::NotOwned => (
            "You don't own that trinket (roll one with `/pet trinket`).".to_owned(),
            VALIDATION_ERROR,
        ),
        PetRepositoryFailure::InBrawl => (
            "Your cama has an open brawl—settle that first.".to_owned(),
            BRAWL_BUSY,
        ),
        PetRepositoryFailure::StalePet
        | PetRepositoryFailure::EggNotReady
        | PetRepositoryFailure::FundShort => (format!("Pet error: {failure}"), VALIDATION_ERROR),
    };
    ServiceResult::fail_with_code(message, code)
}

fn internal_failure<T>(error: PetApplicationError) -> ServiceResult<T> {
    match error {
        PetApplicationError::Repository(error) => map_repo_error(error, None),
        error => ServiceResult::fail_with_code(format!("Pet error: {error}"), VALIDATION_ERROR),
    }
}

/// A tiny deterministic adapter useful to runtimes that provide their own
/// entropy seed. Production wiring may replace this through [`PetRandom`].
#[derive(Clone, Debug)]
pub struct SeededPetRandom {
    state: u64,
}

impl SeededPetRandom {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }
}

impl PetRandom for SeededPetRandom {
    fn weighted_id(&mut self, candidates: &[(&str, i64)]) -> String {
        let total = candidates
            .iter()
            .map(|(_, weight)| (*weight).max(0) as u64)
            .sum::<u64>();
        if total == 0 {
            return candidates
                .first()
                .map_or_else(String::new, |(id, _)| (*id).to_owned());
        }
        let mut roll = self.next() % total;
        for (id, weight) in candidates {
            let weight = (*weight).max(0) as u64;
            if roll < weight {
                return (*id).to_owned();
            }
            roll -= weight;
        }
        candidates
            .last()
            .map_or_else(String::new, |(id, _)| (*id).to_owned())
    }

    fn tiered_species(&mut self, weights: TierWeights) -> String {
        let tiers = [
            ("common", weights.common),
            ("uncommon", weights.uncommon),
            ("rare", weights.rare),
            ("legendary", weights.legendary),
        ];
        let tier = match self.weighted_id(&tiers).as_str() {
            "uncommon" => SpeciesTier::Uncommon,
            "rare" => SpeciesTier::Rare,
            "legendary" => SpeciesTier::Legendary,
            _ => SpeciesTier::Common,
        };
        let species = species_ids_by_tier(tier);
        let index = if species.is_empty() {
            0
        } else {
            (self.next() as usize) % species.len()
        };
        species
            .get(index)
            .copied()
            .unwrap_or(UNHATCHED_SPECIES)
            .to_owned()
    }

    fn integer(&mut self, minimum: i64, maximum: i64) -> i64 {
        if maximum <= minimum {
            return minimum;
        }
        let width = u64::try_from(maximum - minimum + 1).unwrap_or(u64::MAX);
        minimum + i64::try_from(self.next() % width).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::rc::Rc;

    use cama_domain::pet::{PetBase, PetStage};

    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 12_345;
    const OWNER: i64 = 100;

    fn rejected(failure: PetRepositoryFailure) -> PetRepositoryError {
        PetRepositoryError::Rejected(failure)
    }

    fn normalized_guild(guild_id: Option<i64>) -> i64 {
        guild_id.unwrap_or(0)
    }

    fn anchor_matches(pet: &Pet, anchor: AnchorGuard) -> bool {
        pet.last_fed_at == anchor.expected_last_fed_at
            && pet.hunger_at_last_fed == anchor.expected_hunger
            && anchor
                .expected_dig_work_units
                .is_none_or(|expected| pet.dig_work_units == expected)
            && anchor
                .expected_dig_work_at
                .is_none_or(|expected| pet.dig_work_at == expected)
    }

    fn apply_work_anchor(pet: &mut Pet, anchor: AnchorGuard) {
        if let Some(units) = anchor.new_dig_work_units {
            pet.dig_work_units = units;
        }
        if let Some(at) = anchor.new_dig_work_at {
            pet.dig_work_at = at;
        }
    }

    #[derive(Clone)]
    struct FakeClock(Rc<Cell<i64>>);

    impl FakeClock {
        fn new(now: i64) -> Self {
            Self(Rc::new(Cell::new(now)))
        }

        fn set(&self, now: i64) {
            self.0.set(now);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> i64 {
            self.0.get()
        }
    }

    #[derive(Default)]
    struct FakePlayers {
        players: BTreeSet<(i64, i64)>,
    }

    impl PlayerPort for FakePlayers {
        fn exists(&mut self, discord_id: i64, guild_id: i64) -> bool {
            self.players.contains(&(discord_id, guild_id))
        }
    }

    #[derive(Default)]
    struct ScriptedRandom {
        weighted: VecDeque<String>,
        tiered: VecDeque<String>,
        integers: VecDeque<i64>,
        last_weighted: Vec<(String, i64)>,
        tier_weights: Vec<TierWeights>,
    }

    impl ScriptedRandom {
        fn push_weighted(&mut self, value: &str) {
            self.weighted.push_back(value.to_owned());
        }

        fn push_tiered(&mut self, value: &str) {
            self.tiered.push_back(value.to_owned());
        }

        fn push_integer(&mut self, value: i64) {
            self.integers.push_back(value);
        }
    }

    impl PetRandom for ScriptedRandom {
        fn weighted_id(&mut self, candidates: &[(&str, i64)]) -> String {
            self.last_weighted = candidates
                .iter()
                .map(|(id, weight)| ((*id).to_owned(), *weight))
                .collect();
            self.weighted.pop_front().unwrap_or_else(|| {
                candidates
                    .iter()
                    .find(|(_, weight)| *weight > 0)
                    .map_or_else(String::new, |(id, _)| (*id).to_owned())
            })
        }

        fn tiered_species(&mut self, weights: TierWeights) -> String {
            self.tier_weights.push(weights);
            self.tiered.pop_front().unwrap_or_else(|| {
                let tier = if weights.uncommon > 0 {
                    SpeciesTier::Uncommon
                } else if weights.rare > 0 {
                    SpeciesTier::Rare
                } else if weights.legendary > 0 {
                    SpeciesTier::Legendary
                } else {
                    SpeciesTier::Common
                };
                species_ids_by_tier(tier)
                    .first()
                    .copied()
                    .unwrap_or(UNHATCHED_SPECIES)
                    .to_owned()
            })
        }

        fn integer(&mut self, minimum: i64, maximum: i64) -> i64 {
            self.integers
                .pop_front()
                .unwrap_or(minimum)
                .clamp(minimum, maximum)
        }
    }

    #[derive(Default)]
    struct RecordingEvolution {
        events: Vec<PetCareEvent>,
        due: Vec<Pet>,
        unannounced: Vec<EvolutionNotice>,
    }

    impl PetEvolutionPort for RecordingEvolution {
        fn record_activity(&mut self, event: PetCareEvent) {
            self.events.push(event);
        }

        fn find_due(&mut self, _now: i64) -> Vec<Pet> {
            self.due.clone()
        }

        fn get_unannounced(&mut self, limit: i64) -> Vec<EvolutionNotice> {
            self.unannounced
                .iter()
                .take(usize::try_from(limit.max(0)).unwrap_or(usize::MAX))
                .cloned()
                .collect()
        }
    }

    #[derive(Default)]
    struct FakeStore {
        pets: Vec<Pet>,
        next_pet_id: i64,
        balances: BTreeMap<(i64, i64), i64>,
        supplies: BTreeMap<(i64, i64, String), i64>,
        accessories: BTreeMap<(i64, i64), Vec<String>>,
        nonprofit_fund: BTreeMap<i64, i64>,
        paid_windows: BTreeSet<(i64, String)>,
        refund_announcements: Vec<(RefundNotice, Option<i64>, i64)>,
        fail_active_lookup: bool,
        in_brawl: bool,
        stale_feed_attempts: i64,
    }

    impl FakeStore {
        fn pet_index(&self, pet_id: i64, guild_id: Option<i64>) -> Option<usize> {
            let guild_id = normalized_guild(guild_id);
            self.pets
                .iter()
                .position(|pet| pet.pet_id == pet_id && pet.guild_id == guild_id)
        }

        fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
            self.balances
                .get(&(discord_id, guild_id))
                .copied()
                .unwrap_or(0)
        }

        fn set_balance(&mut self, discord_id: i64, guild_id: i64, balance: i64) {
            self.balances.insert((discord_id, guild_id), balance);
        }

        fn charge(
            &mut self,
            discord_id: i64,
            guild_id: i64,
            amount: i64,
        ) -> Result<(), PetRepositoryError> {
            let balance = self.balance(discord_id, guild_id);
            if balance < amount {
                return Err(rejected(PetRepositoryFailure::InsufficientFunds));
            }
            self.set_balance(discord_id, guild_id, balance - amount);
            Ok(())
        }

        fn insert_pet(&mut self, mut pet: Pet) -> Pet {
            if pet.pet_id == 0 {
                self.next_pet_id += 1;
                pet.pet_id = self.next_pet_id;
            } else {
                self.next_pet_id = self.next_pet_id.max(pet.pet_id);
            }
            self.pets.push(pet.clone());
            pet
        }

        fn active(&self, discord_id: i64, guild_id: Option<i64>) -> Option<&Pet> {
            let guild_id = normalized_guild(guild_id);
            self.pets
                .iter()
                .filter(|pet| {
                    pet.discord_id == discord_id
                        && pet.guild_id == guild_id
                        && pet.died_at.is_none()
                })
                .max_by_key(|pet| pet.pet_id)
        }
    }

    impl PetStore for FakeStore {
        fn get_active_pet(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> Result<Option<Pet>, PetRepositoryError> {
            Ok(self.active(discord_id, guild_id).cloned())
        }

        fn get_pet_by_id(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
        ) -> Result<Option<Pet>, PetRepositoryError> {
            Ok(self
                .pet_index(pet_id, guild_id)
                .map(|index| self.pets[index].clone()))
        }

        fn get_latest_dead_pet(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> Result<Option<Pet>, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            Ok(self
                .pets
                .iter()
                .filter(|pet| {
                    pet.discord_id == discord_id
                        && pet.guild_id == guild_id
                        && pet.died_at.is_some()
                })
                .max_by_key(|pet| (pet.died_at, pet.pet_id))
                .cloned())
        }

        fn get_supplies(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> Result<BTreeMap<String, i64>, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            Ok(self
                .supplies
                .iter()
                .filter(|((owner, guild, _), _)| *owner == discord_id && *guild == guild_id)
                .map(|((_, _, item), qty)| (item.clone(), *qty))
                .collect())
        }

        fn count_dead_pets(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> Result<i64, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            Ok(i64::try_from(
                self.pets
                    .iter()
                    .filter(|pet| {
                        pet.discord_id == discord_id
                            && pet.guild_id == guild_id
                            && pet.died_at.is_some()
                    })
                    .count(),
            )
            .unwrap_or(i64::MAX))
        }

        fn adopt_pet(&mut self, request: &AdoptPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
            let guild_id = normalized_guild(request.guild_id);
            if self.active(request.discord_id, request.guild_id).is_some() {
                return Err(rejected(PetRepositoryFailure::AlreadyHasPet));
            }
            self.charge(request.discord_id, guild_id, request.fee)?;
            let mut pet = Pet::new(PetBase {
                pet_id: 0,
                discord_id: request.discord_id,
                guild_id,
                name: request.name.to_owned(),
                species: UNHATCHED_SPECIES.to_owned(),
                adopted_at: request.now,
                hatched_at: request.now + request.hatch_seconds,
                adopt_fee: request.fee,
                last_fed_at: request.now + request.hatch_seconds,
                hunger_at_last_fed: MAX_HUNGER,
                times_fed: 0,
                feeds_today: 0,
                feed_date: None,
                week_consumed_jc: 0,
                week_key: None,
                prev_week_consumed_jc: 0,
                prev_week_key: None,
                pampered_until: None,
                accessory: None,
                dig_work_units: 0,
                dig_work_at: request.now + request.hatch_seconds,
                aegis_used: 0,
                hatch_announced_at: None,
                died_at: None,
                death_cause: None,
                death_announced_at: None,
            });
            pet.egg_tier = request.egg_tier.to_owned();
            Ok(self.insert_pet(pet))
        }

        fn upgrade_egg(
            &mut self,
            request: &UpgradeEggRequest<'_>,
        ) -> Result<Pet, PetRepositoryError> {
            let guild_id = normalized_guild(request.guild_id);
            let Some(index) = self.pets.iter().position(|pet| {
                pet.discord_id == request.discord_id
                    && pet.guild_id == guild_id
                    && pet.died_at.is_none()
            }) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            let pet = &self.pets[index];
            if request.now >= pet.hatched_at || pet.species != UNHATCHED_SPECIES {
                return Err(rejected(PetRepositoryFailure::PetHatched));
            }
            if pet.egg_tier == "gilded" {
                return Err(rejected(PetRepositoryFailure::AlreadyGilded));
            }
            self.charge(request.discord_id, guild_id, request.premium)?;
            let pet = &mut self.pets[index];
            pet.name = request.name.to_owned();
            pet.egg_tier = "gilded".to_owned();
            pet.adopt_fee += request.premium;
            Ok(pet.clone())
        }

        fn sacrifice_and_adopt_atomic(
            &mut self,
            request: &SacrificePetRequest<'_>,
        ) -> Result<RepositorySacrificeOutcome, PetRepositoryError> {
            if self.in_brawl {
                return Err(rejected(PetRepositoryFailure::InBrawl));
            }
            let guild_id = normalized_guild(request.guild_id);
            let Some(index) = self.pets.iter().position(|pet| {
                pet.pet_id == request.old_pet_id
                    && pet.discord_id == request.discord_id
                    && pet.guild_id == guild_id
            }) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            let pet = &self.pets[index];
            if pet.died_at.is_some() {
                return Err(rejected(PetRepositoryFailure::PetDead));
            }
            if pet.last_fed_at != request.expected_last_fed_at
                || pet.hunger_at_last_fed != request.expected_hunger
            {
                return Err(rejected(PetRepositoryFailure::StalePet));
            }
            self.charge(request.discord_id, guild_id, request.fee)?;
            let dead_pet = {
                let pet = &mut self.pets[index];
                pet.died_at = Some(request.now);
                pet.death_cause = Some("sacrifice".to_owned());
                if pet
                    .evolution_due_at
                    .is_none_or(|due_at| due_at >= request.now)
                {
                    pet.evolved_at = None;
                    pet.evolution_calling = None;
                    pet.evolution_primary = None;
                    pet.evolution_secondary = None;
                    pet.evolution_announced_at = None;
                }
                pet.clone()
            };
            let hatched_at = request.now + request.hatch_seconds;
            let mut new_pet = Pet::new(PetBase {
                pet_id: 0,
                discord_id: request.discord_id,
                guild_id,
                name: request.name.to_owned(),
                species: request.species.to_owned(),
                adopted_at: request.now,
                hatched_at,
                adopt_fee: request.fee,
                last_fed_at: hatched_at,
                hunger_at_last_fed: MAX_HUNGER,
                times_fed: 0,
                feeds_today: 0,
                feed_date: None,
                week_consumed_jc: 0,
                week_key: None,
                prev_week_consumed_jc: 0,
                prev_week_key: None,
                pampered_until: None,
                accessory: None,
                dig_work_units: 0,
                dig_work_at: hatched_at,
                aegis_used: 0,
                hatch_announced_at: None,
                died_at: None,
                death_cause: None,
                death_announced_at: None,
            });
            new_pet.evolution_started_at = Some(hatched_at);
            new_pet.evolution_due_at = Some(hatched_at + cama_domain::pet::ADULT_AGE_SECONDS);
            Ok(RepositorySacrificeOutcome {
                dead_pet,
                new_pet: self.insert_pet(new_pet),
            })
        }

        fn resolve_hatch_species(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            species: &str,
            now: i64,
        ) -> Result<Pet, PetRepositoryError> {
            let Some(index) = self.pet_index(pet_id, guild_id) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            let pet = &mut self.pets[index];
            if pet.died_at.is_some() {
                return Err(rejected(PetRepositoryFailure::PetDead));
            }
            if pet.species != UNHATCHED_SPECIES {
                return Ok(pet.clone());
            }
            if now < pet.hatched_at {
                return Err(rejected(PetRepositoryFailure::EggNotReady));
            }
            pet.species = species.to_owned();
            Ok(pet.clone())
        }

        fn recent_dead_species(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            limit: i64,
        ) -> Result<Vec<String>, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            let mut pets = self
                .pets
                .iter()
                .filter(|pet| {
                    pet.discord_id == discord_id
                        && pet.guild_id == guild_id
                        && pet.died_at.is_some()
                        && !matches!(pet.death_cause.as_deref(), Some("sacrifice" | "eaten"))
                })
                .collect::<Vec<_>>();
            pets.sort_by_key(|pet| std::cmp::Reverse((pet.adopted_at, pet.pet_id)));
            Ok(pets
                .into_iter()
                .take(usize::try_from(limit.max(0)).unwrap_or(usize::MAX))
                .map(|pet| pet.species.clone())
                .collect())
        }

        fn buy_supplies(
            &mut self,
            request: &BuySuppliesRequest<'_>,
        ) -> Result<i64, PetRepositoryError> {
            let guild_id = normalized_guild(request.guild_id);
            let key = (request.discord_id, guild_id, request.item_id.to_owned());
            let current = self.supplies.get(&key).copied().unwrap_or(0);
            if current + request.qty > request.stack_cap {
                return Err(rejected(PetRepositoryFailure::StackCap));
            }
            self.charge(request.discord_id, guild_id, request.total_cost)?;
            let new_qty = current + request.qty;
            self.supplies.insert(key, new_qty);
            Ok(new_qty)
        }

        fn feed_pet(&mut self, request: &FeedPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
            if self.stale_feed_attempts > 0 {
                self.stale_feed_attempts -= 1;
                return Err(rejected(PetRepositoryFailure::StalePet));
            }
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            let pet = &self.pets[index];
            if pet.died_at.is_some() {
                return Err(rejected(PetRepositoryFailure::PetDead));
            }
            if !anchor_matches(pet, request.anchor) {
                return Err(rejected(PetRepositoryFailure::StalePet));
            }
            let feeds_today = if pet.feed_date.as_deref() == Some(request.feed_date) {
                pet.feeds_today
            } else {
                0
            };
            if feeds_today >= request.feed_cap {
                return Err(rejected(PetRepositoryFailure::FeedCap));
            }
            let supply_key = (
                request.discord_id,
                normalized_guild(request.guild_id),
                request.item_id.to_owned(),
            );
            let available = self.supplies.get(&supply_key).copied().unwrap_or(0);
            if available < 1 {
                return Err(rejected(PetRepositoryFailure::NoSupplies));
            }
            self.supplies.insert(supply_key, available - 1);
            let pet = &mut self.pets[index];
            if !request.spat {
                pet.last_fed_at = request.new_last_fed_at;
                pet.hunger_at_last_fed = request.new_hunger;
                pet.times_fed += 1;
            }
            pet.feeds_today = feeds_today + 1;
            pet.feed_date = Some(request.feed_date.to_owned());
            apply_work_anchor(pet, request.anchor);
            if pet.week_key.as_deref() == Some(request.week_key) {
                pet.week_consumed_jc += request.consumed_jc;
            } else {
                pet.prev_week_key = pet.week_key.take();
                pet.prev_week_consumed_jc = pet.week_consumed_jc;
                pet.week_key = Some(request.week_key.to_owned());
                pet.week_consumed_jc = request.consumed_jc;
            }
            Ok(pet.clone())
        }

        fn use_salt_lick(
            &mut self,
            request: &SaltLickRequest<'_>,
        ) -> Result<Pet, PetRepositoryError> {
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            let pet = &self.pets[index];
            if pet.died_at.is_some() {
                return Err(rejected(PetRepositoryFailure::PetDead));
            }
            if pet.pampered_until.is_some_and(|until| until > request.now) {
                return Err(rejected(PetRepositoryFailure::AlreadyPampered));
            }
            if request
                .anchor
                .is_some_and(|anchor| !anchor_matches(pet, anchor))
            {
                return Err(rejected(PetRepositoryFailure::StalePet));
            }
            self.charge(
                request.discord_id,
                normalized_guild(request.guild_id),
                request.cost,
            )?;
            let pet = &mut self.pets[index];
            pet.pampered_until = Some(request.pampered_until);
            if let Some(anchor) = request.anchor {
                apply_work_anchor(pet, anchor);
            }
            if pet.week_key.as_deref() == Some(request.week_key) {
                pet.week_consumed_jc += request.cost;
            } else {
                pet.prev_week_key = pet.week_key.take();
                pet.prev_week_consumed_jc = pet.week_consumed_jc;
                pet.week_key = Some(request.week_key.to_owned());
                pet.week_consumed_jc = request.cost;
            }
            Ok(pet.clone())
        }

        fn buy_trinket_atomic(
            &mut self,
            request: &TrinketPurchaseRequest<'_>,
        ) -> Result<TrinketPurchaseOutcome, PetRepositoryError> {
            let guild_id = normalized_guild(request.guild_id);
            let key = (request.discord_id, guild_id);
            let duplicate = self
                .accessories
                .get(&key)
                .is_some_and(|owned| owned.iter().any(|item| item == request.accessory_id));
            let net_cost = request.cost - if duplicate { request.refund } else { 0 };
            self.charge(request.discord_id, guild_id, net_cost)?;
            let owned = self.accessories.entry(key).or_default();
            if !duplicate {
                owned.push(request.accessory_id.to_owned());
            }
            let owned_count = i64::try_from(owned.len()).unwrap_or(i64::MAX);
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            self.pets[index].accessory = Some(request.accessory_id.to_owned());
            Ok(TrinketPurchaseOutcome {
                duplicate,
                net_cost,
                owned_count,
            })
        }

        fn equip_accessory(
            &mut self,
            pet_id: i64,
            discord_id: i64,
            guild_id: Option<i64>,
            accessory_id: Option<&str>,
        ) -> Result<(), PetRepositoryError> {
            let normalized_guild = normalized_guild(guild_id);
            if let Some(accessory_id) = accessory_id
                && !self
                    .accessories
                    .get(&(discord_id, normalized_guild))
                    .is_some_and(|owned| owned.iter().any(|item| item == accessory_id))
            {
                return Err(rejected(PetRepositoryFailure::NotOwned));
            }
            let Some(index) = self.pet_index(pet_id, guild_id) else {
                return Err(rejected(PetRepositoryFailure::NoPet));
            };
            self.pets[index].accessory = accessory_id.map(str::to_owned);
            Ok(())
        }

        fn get_accessories(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> Result<Vec<String>, PetRepositoryError> {
            Ok(self
                .accessories
                .get(&(discord_id, normalized_guild(guild_id)))
                .cloned()
                .unwrap_or_default())
        }

        fn species_raised(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            now: i64,
        ) -> Result<Vec<String>, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            let mut species = self
                .pets
                .iter()
                .filter(|pet| {
                    pet.discord_id == discord_id
                        && pet.guild_id == guild_id
                        && pet.hatched_at <= now
                        && pet.species != UNHATCHED_SPECIES
                })
                .map(|pet| pet.species.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            species.sort_by_key(|id| {
                SPECIES
                    .iter()
                    .position(|species| species.species_id == id)
                    .unwrap_or(usize::MAX)
            });
            Ok(species)
        }

        fn get_active_pets_for(
            &mut self,
            discord_ids: &[i64],
            guild_id: Option<i64>,
        ) -> Result<Vec<Pet>, PetRepositoryError> {
            if self.fail_active_lookup {
                return Err(PetRepositoryError::ArithmeticOutOfRange);
            }
            let guild_id = normalized_guild(guild_id);
            Ok(self
                .pets
                .iter()
                .filter(|pet| {
                    discord_ids.contains(&pet.discord_id)
                        && pet.guild_id == guild_id
                        && pet.died_at.is_none()
                })
                .cloned()
                .collect())
        }

        fn apply_hunger_top_up(
            &mut self,
            request: HungerTopUpRequest,
        ) -> Result<bool, PetRepositoryError> {
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Ok(false);
            };
            let pet = &mut self.pets[index];
            if pet.died_at.is_some() || !anchor_matches(pet, request.anchor) {
                return Ok(false);
            }
            pet.last_fed_at = request.new_last_fed_at;
            pet.hunger_at_last_fed = request.new_hunger;
            apply_work_anchor(pet, request.anchor);
            Ok(true)
        }

        fn claim_death(
            &mut self,
            request: &DeathClaimRequest<'_>,
        ) -> Result<bool, PetRepositoryError> {
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Ok(false);
            };
            let pet = &mut self.pets[index];
            if pet.died_at.is_some() || !anchor_matches(pet, request.anchor) {
                return Ok(false);
            }
            pet.died_at = Some(request.died_at);
            pet.death_cause = Some(request.cause.to_owned());
            apply_work_anchor(pet, request.anchor);
            Ok(true)
        }

        fn revive_with_aegis(
            &mut self,
            request: AegisReviveRequest,
        ) -> Result<bool, PetRepositoryError> {
            let Some(index) = self.pet_index(request.pet_id, request.guild_id) else {
                return Ok(false);
            };
            let pet = &mut self.pets[index];
            if pet.died_at.is_some() || pet.aegis_used != 0 || !anchor_matches(pet, request.anchor)
            {
                return Ok(false);
            }
            pet.last_fed_at = request.revived_last_fed_at;
            pet.hunger_at_last_fed = request.revive_hunger;
            pet.aegis_used = 1;
            apply_work_anchor(pet, request.anchor);
            Ok(true)
        }

        fn find_starvation_candidates(
            &mut self,
            now: i64,
            decay_per_day: i64,
            _max_decay_pct: i64,
        ) -> Result<Vec<Pet>, PetRepositoryError> {
            Ok(self
                .pets
                .iter()
                .filter(|pet| {
                    pet.died_at.is_none()
                        && pet.hatched_at <= now
                        && pet.is_starved(now, decay_per_day).unwrap_or(false)
                })
                .cloned()
                .collect())
        }

        fn get_unannounced_deaths(&mut self, limit: i64) -> Result<Vec<Pet>, PetRepositoryError> {
            let mut pets = self
                .pets
                .iter()
                .filter(|pet| pet.died_at.is_some() && pet.death_announced_at.is_none())
                .cloned()
                .collect::<Vec<_>>();
            pets.sort_by_key(|pet| (pet.died_at, pet.pet_id));
            pets.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
            Ok(pets)
        }

        fn find_unannounced_hatches(
            &mut self,
            now: i64,
            limit: i64,
        ) -> Result<Vec<Pet>, PetRepositoryError> {
            let mut pets = self
                .pets
                .iter()
                .filter(|pet| {
                    pet.died_at.is_none()
                        && pet.hatched_at <= now
                        && pet.hatch_announced_at.is_none()
                })
                .cloned()
                .collect::<Vec<_>>();
            pets.sort_by_key(|pet| (pet.hatched_at, pet.pet_id));
            pets.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
            Ok(pets)
        }

        fn mark_death_announced(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            announced_at: i64,
        ) -> Result<(), PetRepositoryError> {
            if let Some(index) = self.pet_index(pet_id, guild_id) {
                self.pets[index].death_announced_at = Some(announced_at);
            }
            Ok(())
        }

        fn mark_hatch_announced(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            announced_at: i64,
        ) -> Result<(), PetRepositoryError> {
            if let Some(index) = self.pet_index(pet_id, guild_id) {
                self.pets[index].hatch_announced_at = Some(announced_at);
            }
            Ok(())
        }

        fn list_pet_guild_ids(&mut self) -> Result<Vec<i64>, PetRepositoryError> {
            Ok(self
                .pets
                .iter()
                .filter(|pet| pet.died_at.is_none())
                .map(|pet| pet.guild_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect())
        }

        fn list_week_care(
            &mut self,
            guild_id: Option<i64>,
            week_key: &str,
        ) -> Result<Vec<Pet>, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            Ok(self
                .pets
                .iter()
                .filter(|pet| {
                    pet.guild_id == guild_id
                        && pet.died_at.is_none()
                        && pet.consumed_in_week(week_key) > 0
                })
                .cloned()
                .collect())
        }

        fn is_refund_window_paid(
            &mut self,
            guild_id: Option<i64>,
            week_key: &str,
        ) -> Result<bool, PetRepositoryError> {
            Ok(self
                .paid_windows
                .contains(&(normalized_guild(guild_id), week_key.to_owned())))
        }

        fn get_nonprofit_balance(
            &mut self,
            guild_id: Option<i64>,
        ) -> Result<i64, PetRepositoryError> {
            Ok(self
                .nonprofit_fund
                .get(&normalized_guild(guild_id))
                .copied()
                .unwrap_or(0))
        }

        fn pay_refunds_atomic(
            &mut self,
            guild_id: Option<i64>,
            week_key: &str,
            payouts: &[(i64, i64)],
            now: i64,
            announcement: Option<&RefundNotice>,
        ) -> Result<bool, PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            let key = (guild_id, week_key.to_owned());
            if self.paid_windows.contains(&key) {
                return Ok(false);
            }
            let total = payouts.iter().map(|(_, amount)| *amount).sum::<i64>();
            let fund = self.nonprofit_fund.get(&guild_id).copied().unwrap_or(0);
            if total > fund {
                return Err(rejected(PetRepositoryFailure::FundShort));
            }
            for (discord_id, amount) in payouts {
                let balance = self.balance(*discord_id, guild_id);
                self.set_balance(*discord_id, guild_id, balance + amount);
            }
            self.nonprofit_fund.insert(guild_id, fund - total);
            self.paid_windows.insert(key);
            if let Some(notice) = announcement {
                self.refund_announcements.push((notice.clone(), None, now));
            }
            Ok(true)
        }

        fn get_unannounced_refunds(
            &mut self,
            limit: i64,
        ) -> Result<Vec<RefundNotice>, PetRepositoryError> {
            let mut notices = self
                .refund_announcements
                .iter()
                .filter(|(_, announced_at, _)| announced_at.is_none())
                .collect::<Vec<_>>();
            notices.sort_by_key(|(_, _, paid_at)| *paid_at);
            Ok(notices
                .into_iter()
                .take(usize::try_from(limit.max(0)).unwrap_or(usize::MAX))
                .map(|(notice, _, _)| notice.clone())
                .collect())
        }

        fn mark_refund_announced(
            &mut self,
            guild_id: Option<i64>,
            week_key: &str,
            announced_at: i64,
        ) -> Result<(), PetRepositoryError> {
            let guild_id = normalized_guild(guild_id);
            for (notice, marked_at, _) in &mut self.refund_announcements {
                if notice.guild_id == guild_id && notice.week_key == week_key {
                    *marked_at = Some(announced_at);
                }
            }
            Ok(())
        }
    }

    type TestService =
        PetService<FakeStore, FakePlayers, ScriptedRandom, FakeClock, RecordingEvolution>;

    fn fixture() -> (TestService, FakeClock) {
        let mut store = FakeStore::default();
        store.set_balance(OWNER, GUILD, 1_000);
        let mut players = FakePlayers::default();
        players.players.insert((OWNER, GUILD));
        let clock = FakeClock::new(T0);
        (
            PetService::new(
                store,
                players,
                ScriptedRandom::default(),
                clock.clone(),
                RecordingEvolution::default(),
                20,
            ),
            clock,
        )
    }

    fn adopt_species(service: &mut TestService, name: &str, species: &str, egg_tier: &str) -> Pet {
        let egg = service
            .adopt(OWNER, Some(GUILD), name, egg_tier)
            .unwrap()
            .pet;
        service
            .store_mut()
            .resolve_hatch_species(egg.pet_id, Some(GUILD), species, egg.hatched_at)
            .unwrap()
    }

    fn adopt_common(service: &mut TestService, name: &str) -> Pet {
        adopt_species(service, name, "common_cama", "standard")
    }

    fn stored(service: &mut TestService, pet_id: i64) -> Pet {
        service
            .store_mut()
            .get_pet_by_id(pet_id, Some(GUILD))
            .unwrap()
            .unwrap()
    }

    fn error_code<T>(result: &ServiceResult<T>) -> Option<&str> {
        result.error_code()
    }

    fn previous_week_key(now: i64) -> String {
        week_key_for_timestamp(now - 7 * DAY).unwrap()
    }

    fn keep_alive_until_previous_week(
        service: &mut TestService,
        clock: &FakeClock,
        target_week: &str,
        item_id: &str,
    ) {
        for _ in 0..8 {
            if previous_week_key(clock.now()) == target_week {
                return;
            }
            clock.set(clock.now() + DAY);
            assert!(service.feed(OWNER, Some(GUILD), item_id).success());
        }
        panic!("target care week did not become the previous game week");
    }

    mod adopt {
        use super::*;

        #[test]
        fn adopt_creates_egg_and_charges_first_fee() {
            let (mut service, _clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");

            assert_eq!(pet.adopt_fee, 20);
            assert_eq!(pet.hatched_at, T0 + EGG_HATCH_SECONDS);
            let status = service.get_status(OWNER, Some(GUILD)).unwrap();
            assert_eq!(status.stage, Some(PetStage::Egg));
            assert_eq!(status.hunger, 100);
            assert_eq!(service.store().balance(OWNER, GUILD), 980);
        }

        #[test]
        fn adopt_over_a_starved_pet_claims_death_and_escalates_fee() {
            let (mut service, clock) = fixture();
            let first = adopt_common(&mut service, "Blep");
            clock.set(first.hatched_at + 9 * DAY);

            let second = adopt_common(&mut service, "Blep II");

            assert_eq!(second.adopt_fee, 50);
            let dead = stored(&mut service, first.pet_id);
            assert_eq!(dead.died_at, Some(first.hatched_at + 5 * DAY));
            assert_eq!(dead.death_cause.as_deref(), Some("starvation"));
        }

        #[test]
        fn species_roll_respects_tier_weights() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Weighted", "standard")
                .unwrap()
                .pet;
            assert_eq!(egg.species, UNHATCHED_SPECIES);
            assert_eq!(egg.egg_tier, "standard");
            clock.set(egg.hatched_at);
            service.random_mut().push_weighted("rama");

            let status = service.get_status(OWNER, Some(GUILD)).unwrap();

            assert_eq!(status.pet.unwrap().species, "rama");
            let weights = &service.random().last_weighted;
            let by_id = weights.iter().cloned().collect::<BTreeMap<_, _>>();
            assert_eq!(by_id["common_cama"], 1_550);
            assert_eq!(by_id["embergear_cama"], 1_500);
            assert_eq!(by_id["riverglow_cama"], 750);
            assert_eq!(by_id["prismwool_cama"], 225);
            assert_eq!(by_id["rama"], 10);
            assert_eq!(by_id["moondrift_cama"], 20);
            assert_eq!(by_id["sunspun_cama"], 20);
            assert_eq!(
                weights.iter().map(|(_, weight)| weight).sum::<i64>(),
                10_000
            );
        }

        #[test]
        fn unregistered_player_rejected() {
            let (mut service, _clock) = fixture();
            let result = service.adopt(999, Some(GUILD), "Ghost", "standard");
            assert_eq!(error_code(&result), Some(PLAYER_NOT_FOUND));
            assert!(service.store().pets.is_empty());
        }

        #[test]
        fn name_validation() {
            let (mut service, _clock) = fixture();
            for name in [
                "",
                "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "hi @everyone",
                "https://spam.example",
            ] {
                assert_eq!(
                    error_code(&service.adopt(OWNER, Some(GUILD), name, "standard")),
                    Some(VALIDATION_ERROR)
                );
            }
            assert!(service.store().pets.is_empty());
            assert_eq!(service.store().balance(OWNER, GUILD), 1_000);
        }
    }

    mod sacrifice {
        use super::*;

        #[test]
        fn test_rejects_petless_and_egg() {
            let (mut service, _clock) = fixture();

            let petless = service.sacrifice(OWNER, Some(GUILD), "Rebirth");
            assert_eq!(error_code(&petless), Some(NO_PET));
            assert_eq!(
                petless.error(),
                Some("You have no living pet to sacrifice.")
            );

            let egg = service
                .adopt(OWNER, Some(GUILD), "Eggy", "standard")
                .unwrap()
                .pet;
            let result = service.sacrifice(OWNER, Some(GUILD), "Rebirth");

            assert_eq!(error_code(&result), Some(PET_EGG));
            assert_eq!(
                result.error(),
                Some("You can't sacrifice a mystery. Let it hatch first.")
            );
            assert_eq!(stored(&mut service, egg.pet_id).died_at, None);
        }

        #[test]
        fn test_rejects_bad_name() {
            let (mut service, _clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");

            let result = service.sacrifice(OWNER, Some(GUILD), "join discord.gg/scam");

            assert_eq!(error_code(&result), Some(VALIDATION_ERROR));
            assert_eq!(stored(&mut service, pet.pet_id).died_at, None);
        }

        #[test]
        fn test_fee_counts_the_sacrificed_pet() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 1);

            let preview = service.sacrifice_preview(OWNER, Some(GUILD)).unwrap();
            assert_eq!(preview.fee, cama_domain::pet::ADOPTION_FEES[1]);
            service.random_mut().push_tiered("rama");

            let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();

            assert_eq!(outcome.fee, cama_domain::pet::ADOPTION_FEES[1]);
            assert_eq!(outcome.dead_pet.death_cause.as_deref(), Some("sacrifice"));
            assert_eq!(outcome.new_pet.species, "rama");
            assert_eq!(outcome.new_pet.egg_tier, "standard");
            assert_eq!(service.store().balance(OWNER, GUILD), 930);
        }

        #[test]
        fn test_sacrifice_at_evolution_due_does_not_evolve() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            let due_at = pet.hatched_at + cama_domain::pet::ADULT_AGE_SECONDS;
            let stored = service
                .store_mut()
                .pets
                .iter_mut()
                .find(|candidate| candidate.pet_id == pet.pet_id)
                .unwrap();
            stored.evolution_started_at = Some(pet.hatched_at);
            stored.evolution_due_at = Some(due_at);
            stored.evolved_at = Some(due_at);
            stored.evolution_calling = Some("guardian".to_owned());
            stored.evolution_primary = Some("steadfast".to_owned());
            stored.evolution_secondary = Some("hungry".to_owned());
            stored.evolution_announced_at = Some(due_at);
            stored.last_fed_at = due_at;
            stored.hunger_at_last_fed = MAX_HUNGER;
            clock.set(due_at);
            service.random_mut().push_tiered("rama");

            let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();

            assert_eq!(outcome.dead_pet.died_at, Some(due_at));
            assert_eq!(outcome.dead_pet.evolved_at, None);
            assert_eq!(outcome.dead_pet.evolution_calling, None);
            assert!(service.evolution().unannounced.is_empty());
        }

        #[test]
        fn test_weights_follow_tier_and_stage() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Blep", "invoker_cama", "standard");
            clock.set(pet.hatched_at + 3_600);

            let baby = service.sacrifice_preview(OWNER, Some(GUILD)).unwrap();
            assert_eq!(baby.tier, SpeciesTier::Rare);
            assert!(!baby.was_adult);
            assert_eq!(
                baby.weights,
                sacrifice_tier_weights(SpeciesTier::Rare, false)
            );

            let adult_at = pet.hatched_at + cama_domain::pet::ADULT_AGE_SECONDS;
            let stored = service
                .store_mut()
                .pets
                .iter_mut()
                .find(|candidate| candidate.pet_id == pet.pet_id)
                .unwrap();
            stored.last_fed_at = adult_at;
            stored.hunger_at_last_fed = MAX_HUNGER;
            clock.set(adult_at);

            let adult = service.sacrifice_preview(OWNER, Some(GUILD)).unwrap();
            let expected = sacrifice_tier_weights(SpeciesTier::Rare, true);
            assert!(adult.was_adult);
            assert_eq!(adult.weights, expected);
            service.random_mut().push_tiered("rama");

            let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();
            assert_eq!(outcome.new_pet.species, "rama");
            assert_eq!(service.random().tier_weights.last(), Some(&expected));
        }

        #[test]
        fn test_sacrifice_excluded_from_pity() {
            let (mut service, clock) = fixture();
            for index in 0..3 {
                let pet = adopt_common(&mut service, &format!("Common {index}"));
                clock.set(pet.hatched_at + 9 * DAY);
                assert!(
                    service
                        .living_pet(OWNER, Some(GUILD), clock.now())
                        .unwrap()
                        .is_none()
                );
            }
            assert!(service.pity_active(OWNER, Some(GUILD)).unwrap());

            let egg = service
                .adopt(OWNER, Some(GUILD), "Altar Fodder", "standard")
                .unwrap()
                .pet;
            clock.set(egg.hatched_at + 1);
            service.random_mut().push_tiered("common_cama");
            service.random_mut().push_tiered("banana_ears");

            let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();

            assert_eq!(outcome.new_pet.species, "banana_ears");
            assert_eq!(
                service
                    .store_mut()
                    .recent_dead_species(OWNER, Some(GUILD), 10)
                    .unwrap()
                    .len(),
                3
            );
            assert!(service.pity_active(OWNER, Some(GUILD)).unwrap());
            assert_eq!(
                service
                    .store_mut()
                    .count_dead_pets(OWNER, Some(GUILD))
                    .unwrap(),
                4
            );
        }

        #[test]
        fn test_blocked_while_in_brawl() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 1);
            service.store_mut().in_brawl = true;
            service.random_mut().push_tiered("rama");

            let result = service.sacrifice(OWNER, Some(GUILD), "Rebirth");

            assert_eq!(error_code(&result), Some(BRAWL_BUSY));
            assert_eq!(stored(&mut service, pet.pet_id).died_at, None);
            assert_eq!(service.store().balance(OWNER, GUILD), 980);
        }
    }

    mod egg_hatch_and_upgrade {
        use super::*;

        #[test]
        fn second_gilded_adoption_upgrades_and_renames_existing_egg() {
            let (mut service, _clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Blep", "standard")
                .unwrap()
                .pet;

            let result = service
                .adopt(OWNER, Some(GUILD), "Goldie", "gilded")
                .unwrap();

            assert!(result.upgraded);
            let upgraded = result.pet;
            assert_eq!(upgraded.pet_id, egg.pet_id);
            assert_eq!(upgraded.name, "Goldie");
            assert_eq!(upgraded.hatched_at, egg.hatched_at);
            assert_eq!(upgraded.species, UNHATCHED_SPECIES);
            assert_eq!(upgraded.egg_tier, "gilded");
            assert_eq!(upgraded.adopt_fee, 250);
            assert_eq!(service.store().balance(OWNER, GUILD), 750);
            assert_eq!(service.store().pets.len(), 1);
        }

        #[test]
        fn second_gilded_adoption_rejects_repeat_hatched_and_unaffordable_requests() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Blep", "standard")
                .unwrap()
                .pet;
            assert!(
                service
                    .adopt(OWNER, Some(GUILD), "Goldie", "gilded")
                    .success()
            );
            assert_eq!(
                error_code(&service.adopt(OWNER, Some(GUILD), "Again", "gilded")),
                Some(ALREADY_HAS_PET)
            );

            clock.set(egg.hatched_at);
            service.random_mut().push_tiered("rama");
            assert_eq!(
                error_code(&service.adopt(OWNER, Some(GUILD), "Too Late", "gilded")),
                Some(ALREADY_HAS_PET)
            );
            let current = stored(&mut service, egg.pet_id);
            let claimed = service
                .store_mut()
                .claim_death(&DeathClaimRequest {
                    pet_id: egg.pet_id,
                    guild_id: Some(GUILD),
                    died_at: clock.now(),
                    cause: "test",
                    anchor: AnchorGuard::new(current.last_fed_at, current.hunger_at_last_fed),
                })
                .unwrap();
            assert!(claimed);
            clock.set(clock.now() + 1);
            service.store_mut().set_balance(OWNER, GUILD, 100);
            assert!(
                service
                    .adopt(OWNER, Some(GUILD), "Second", "standard")
                    .success()
            );

            let failed = service.adopt(OWNER, Some(GUILD), "Pricier", "gilded");

            assert_eq!(error_code(&failed), Some(INSUFFICIENT_FUNDS));
            let unchanged = service
                .store_mut()
                .get_active_pet(OWNER, Some(GUILD))
                .unwrap()
                .unwrap();
            assert_eq!(unchanged.egg_tier, "standard");
            assert_eq!(unchanged.name, "Second");
        }

        #[test]
        fn second_standard_adoption_remains_rejected() {
            let (mut service, _clock) = fixture();
            assert!(
                service
                    .adopt(OWNER, Some(GUILD), "Blep", "standard")
                    .success()
            );
            let result = service.adopt(OWNER, Some(GUILD), "Replacement", "standard");
            assert_eq!(error_code(&result), Some(ALREADY_HAS_PET));
            assert_eq!(service.store().pets.len(), 1);
        }

        #[test]
        fn hatch_resolution_is_stable_across_sweep_retries() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Stable", "standard")
                .unwrap()
                .pet;
            clock.set(egg.hatched_at);
            service.random_mut().push_weighted("rama");
            let first = service.sweep(100).unwrap().hatches[0].pet.clone();
            service.random_mut().push_weighted("common_cama");

            let retried = service.sweep(100).unwrap().hatches[0].pet.clone();

            assert_eq!(first.species, "rama");
            assert_eq!(retried.species, "rama");
            assert_eq!(service.random().weighted.len(), 1);
        }

        #[test]
        fn warning_crossing_is_deferred_until_species_resolves() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Later", "standard")
                .unwrap()
                .pet;
            assert_eq!(
                service.get_warning_crossing(OWNER, Some(GUILD)).unwrap(),
                None
            );
            clock.set(egg.hatched_at);
            service.random_mut().push_weighted("dromedary_cross");

            let crossing = service.get_warning_crossing(OWNER, Some(GUILD)).unwrap();

            assert_eq!(
                crossing,
                Some((
                    egg.hatched_at + 70 * DAY * 100 / (20 * 90),
                    "Later".to_owned()
                ))
            );
        }
    }

    mod feeding {
        use super::*;

        #[test]
        fn successful_feed_records_one_care_activity() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 1).success());
            clock.set(pet.hatched_at + DAY);

            assert!(service.feed(OWNER, Some(GUILD), "tango").success());

            assert_eq!(
                service.evolution().events,
                vec![PetCareEvent {
                    discord_id: OWNER,
                    guild_id: Some(GUILD),
                    activity: PetActivity::PetCared,
                    source_key: format!("pet-care:{}:{}:1", pet.pet_id, clock.now()),
                    occurred_at: clock.now(),
                }]
            );
        }

        #[test]
        fn feed_flow() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 5).success());
            clock.set(pet.hatched_at + 2 * DAY);

            let outcome = service.feed(OWNER, Some(GUILD), "tango").unwrap();

            assert_eq!(outcome.old_hunger, 60);
            assert_eq!(outcome.new_hunger, 85);
            assert_eq!(outcome.remaining_qty, 4);
            assert_eq!(outcome.feeds_left_today, 3);
            assert_eq!(outcome.pet.times_fed, 1);
        }

        #[test]
        fn feed_egg_rejected() {
            let (mut service, _clock) = fixture();
            let _pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 1).success());

            let result = service.feed(OWNER, Some(GUILD), "tango");

            assert_eq!(error_code(&result), Some(PET_EGG));
            assert_eq!(service.store().supplies.values().sum::<i64>(), 1);
        }

        #[test]
        fn feed_full_pet_rejected() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 1).success());
            clock.set(pet.hatched_at);

            let result = service.feed(OWNER, Some(GUILD), "tango");

            assert_eq!(error_code(&result), Some(PET_FULL));
            assert_eq!(service.store().supplies.values().sum::<i64>(), 1);
        }

        #[test]
        fn feed_without_supplies() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + DAY);

            let result = service.feed(OWNER, Some(GUILD), "tango");

            assert_eq!(error_code(&result), Some(NO_SUPPLIES));
            assert_eq!(stored(&mut service, pet.pet_id).times_fed, 0);
        }

        #[test]
        fn starved_pet_feeds_as_dead() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 2).success());
            clock.set(pet.hatched_at + 6 * DAY);

            let result = service.feed(OWNER, Some(GUILD), "cheese");

            assert_eq!(error_code(&result), Some(NO_PET));
            let dead = stored(&mut service, pet.pet_id);
            assert_eq!(dead.died_at, Some(pet.hatched_at + 5 * DAY));
            assert_eq!(service.store().supplies.values().sum::<i64>(), 2);
        }

        #[test]
        fn rama_spits_at_seeded_roll() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Rama", "rama", "standard");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 2).success());
            clock.set(pet.hatched_at + 2 * DAY);
            service.random_mut().push_integer(1);

            let spat = service.feed(OWNER, Some(GUILD), "tango").unwrap();

            assert!(spat.spat);
            assert_eq!(spat.new_hunger, spat.old_hunger);
            assert_eq!(spat.remaining_qty, 1);
            assert_eq!(spat.pet.times_fed, 0);
            service.random_mut().push_integer(2);
            let eaten = service.feed(OWNER, Some(GUILD), "tango").unwrap();
            assert!(!eaten.spat);
            assert!(eaten.new_hunger > eaten.old_hunger);
            assert_eq!(eaten.remaining_qty, 0);
        }
    }

    mod passive_dig_work {
        use super::*;

        #[test]
        fn status_previews_historical_work_without_persisting_it() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 2 * DAY);

            let status = service.get_status(OWNER, Some(GUILD)).unwrap();
            let persisted = stored(&mut service, pet.pet_id);

            assert_eq!(status.dig_work_units, 22 * DAY + DAY / 5);
            assert_eq!(status.dig_work_rate, 8);
            assert_eq!(persisted.dig_work_units, 0);
            assert_eq!(persisted.dig_work_at, pet.hatched_at);
        }

        #[test]
        fn feed_settles_old_moods_before_moving_the_hunger_anchor() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 1).success());
            clock.set(pet.hatched_at + 2 * DAY);

            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());

            let settled = stored(&mut service, pet.pet_id);
            assert_eq!(settled.dig_work_units, 22 * DAY + DAY / 5);
            assert_eq!(settled.dig_work_at, clock.now());
            clock.set(clock.now() + DAY);
            let status = service.get_status(OWNER, Some(GUILD)).unwrap();
            assert_eq!(status.dig_work_units, 34 * DAY + DAY / 5);
        }

        #[test]
        fn pamper_settles_before_the_happy_override_starts() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 2 * DAY);

            assert!(service.buy(OWNER, Some(GUILD), "salt_lick", 1).success());

            clock.set(clock.now() + DAY / 2);
            let status = service.get_status(OWNER, Some(GUILD)).unwrap();
            assert_eq!(status.dig_work_units, 28 * DAY + DAY / 5);
        }

        #[test]
        fn match_top_up_settles_before_moving_the_hunger_anchor() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 2 * DAY);

            let fed = service.on_match_win(&[OWNER], Some(GUILD));

            assert_eq!(fed.len(), 1);
            let settled = stored(&mut service, pet.pet_id);
            assert_eq!(settled.dig_work_units, 22 * DAY + DAY / 5);
            assert_eq!(settled.dig_work_at, clock.now());
        }

        #[test]
        fn starvation_settles_only_the_productive_lifetime() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 9 * DAY);

            assert!(
                service
                    .get_status(OWNER, Some(GUILD))
                    .unwrap()
                    .pet
                    .is_none()
            );

            let dead = stored(&mut service, pet.pet_id);
            assert_eq!(dead.dig_work_units, 34 * DAY + 7 * DAY / 20);
            assert_eq!(dead.dig_work_at, pet.hatched_at + 5 * DAY);
        }

        #[test]
        fn dig_work_preview_resolves_ready_egg_before_offering_work() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Blep", "standard")
                .unwrap()
                .pet;
            clock.set(egg.hatched_at);

            let offer = service
                .preview_dig_work(OWNER, Some(GUILD), clock.now())
                .unwrap()
                .expect("ready egg should offer passive work");
            let hatched = stored(&mut service, egg.pet_id);

            assert_ne!(hatched.species, UNHATCHED_SPECIES);
            assert_eq!(offer.pet_id, egg.pet_id);
            assert_eq!(offer.expected_units, hatched.dig_work_units);
            assert_eq!(offer.expected_at, hatched.dig_work_at);
        }

        #[test]
        fn dig_work_preview_rejects_pre_hatch_egg_without_resolving_species() {
            let (mut service, clock) = fixture();
            let egg = service
                .adopt(OWNER, Some(GUILD), "Blep", "standard")
                .unwrap()
                .pet;
            let now = egg.hatched_at - 1;
            clock.set(now);

            assert!(
                service
                    .preview_dig_work(OWNER, Some(GUILD), now)
                    .unwrap()
                    .is_none()
            );
            let still_egg = stored(&mut service, egg.pet_id);
            assert_eq!(still_egg.species, UNHATCHED_SPECIES);
        }

        #[test]
        fn dig_work_preview_claims_historical_starvation_before_rejecting_work() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            let starvation_at = pet.starvation_time(service.decay_per_day()).unwrap();
            clock.set(starvation_at + DAY);

            assert!(
                service
                    .preview_dig_work(OWNER, Some(GUILD), clock.now())
                    .unwrap()
                    .is_none()
            );
            let dead = stored(&mut service, pet.pet_id);
            assert_eq!(dead.died_at, Some(starvation_at));
            assert_eq!(dead.death_cause.as_deref(), Some("starvation"));
            assert_eq!(dead.dig_work_units, 34 * DAY + 7 * DAY / 20);
            assert_eq!(dead.dig_work_at, starvation_at);
        }

        #[test]
        fn aegis_revive_preserves_work_earned_before_starvation() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Shelly", "aegis_cama", "standard");
            let starved_at = pet.starvation_time(service.decay_per_day()).unwrap();
            clock.set(starved_at + 60);

            let status = service.get_status(OWNER, Some(GUILD)).unwrap();

            let revived = status.pet.unwrap();
            assert_eq!(revived.dig_work_units, 34 * DAY + 7 * DAY / 20);
            assert_eq!(revived.dig_work_at, starved_at);
            assert_eq!(revived.aegis_used, 1);
        }

        #[test]
        fn dig_work_preview_revives_aegis_before_offering_work() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Shelly", "aegis_cama", "standard");
            let starvation_at = pet.starvation_time(service.decay_per_day()).unwrap();
            clock.set(starvation_at + 3_600);

            let offer = service
                .preview_dig_work(OWNER, Some(GUILD), clock.now())
                .unwrap()
                .expect("Aegis should revive the pet before Dig work is offered");
            let revived = stored(&mut service, pet.pet_id);

            assert_eq!(revived.aegis_used, 1);
            assert_eq!(revived.last_fed_at, starvation_at);
            assert_eq!(revived.hunger_at_last_fed, AEGIS_REVIVE_HUNGER);
            assert_eq!(revived.dig_work_units, 34 * DAY + 7 * DAY / 20);
            assert_eq!(revived.dig_work_at, starvation_at);
            assert_eq!(offer.expected_units, revived.dig_work_units);
            assert_eq!(offer.expected_at, revived.dig_work_at);
        }

        #[test]
        fn dig_work_preview_claims_second_starvation_after_aegis_is_spent() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Shelly", "aegis_cama", "standard");
            let first_starvation = pet.starvation_time(service.decay_per_day()).unwrap();
            clock.set(first_starvation + 3_600);

            assert!(
                service
                    .preview_dig_work(OWNER, Some(GUILD), clock.now())
                    .unwrap()
                    .is_some()
            );
            let revived = stored(&mut service, pet.pet_id);
            assert_eq!(revived.aegis_used, 1);

            let second_starvation = first_starvation + DAY / 2;
            clock.set(second_starvation + 3_600);
            assert!(
                service
                    .preview_dig_work(OWNER, Some(GUILD), clock.now())
                    .unwrap()
                    .is_none()
            );
            let dead = stored(&mut service, pet.pet_id);
            assert_eq!(dead.aegis_used, 1);
            assert_eq!(dead.died_at, Some(second_starvation));
            assert_eq!(dead.death_cause.as_deref(), Some("starvation"));
            assert_eq!(dead.dig_work_at, second_starvation);
        }
    }

    mod quirks {
        use super::*;

        #[test]
        fn aegis_revive_fires_once_then_death() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Shelly", "aegis_cama", "standard");
            let starve_1 = pet.hatched_at + 5 * DAY;
            let starve_2 = starve_1 + DAY / 2;
            clock.set(starve_1 + 3_600);

            let revived = service.get_status(OWNER, Some(GUILD)).unwrap();

            assert_eq!(revived.pet.as_ref().map(|pet| pet.aegis_used), Some(1));
            clock.set(starve_2 + 3_600);
            let dead = service.get_status(OWNER, Some(GUILD)).unwrap();
            assert!(dead.pet.is_none());
            assert_eq!(dead.last_dead.unwrap().died_at, Some(starve_2));
        }

        #[test]
        fn frostwool_discount_applies_to_buy() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Frosty", "crystal_cama", "standard");
            clock.set(pet.hatched_at);

            let result = service.buy(OWNER, Some(GUILD), "cheese", 2).unwrap();

            assert_eq!(result.total_cost, 54);
            assert_eq!(result.new_qty, Some(2));
        }

        #[test]
        fn unhatched_frostwool_uses_species_neutral_prices() {
            let (mut service, clock) = fixture();
            let pet = service
                .adopt(OWNER, Some(GUILD), "Secret", "standard")
                .unwrap()
                .pet;
            assert!(clock.now() < pet.hatched_at);
            assert_eq!(pet.species, UNHATCHED_SPECIES);

            let result = service.buy(OWNER, Some(GUILD), "cheese", 2).unwrap();

            assert_eq!(result.total_cost, 60);
        }

        #[test]
        fn invoker_salt_lick_lasts_longer() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Orbs", "invoker_cama", "standard");
            clock.set(pet.hatched_at);

            let result = service.buy(OWNER, Some(GUILD), "salt_lick", 1).unwrap();

            assert_eq!(result.pampered_until, Some(clock.now() + 18 * 3_600));
            assert_eq!(service.evolution().events.len(), 1);
        }

        #[test]
        fn gilded_refund_bonus() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Goldie", "jopacama", "standard");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 20).success());
            clock.set(pet.hatched_at + 2 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            service.store_mut().nonprofit_fund.insert(GUILD, 500);
            let care_week = week_key_for_timestamp(clock.now()).unwrap();
            keep_alive_until_previous_week(&mut service, &clock, &care_week, "cheese");
            service.random_mut().push_integer(100);

            let notices = service.sweep(100).unwrap().refunds;

            let payout = notices
                .iter()
                .find(|notice| notice.week_key == care_week)
                .unwrap()
                .payouts[0];
            assert_eq!(payout.multiplier_pct, 110);
            assert_eq!(payout.amount, payout.consumed_jc * 110 / 100);
        }
    }

    mod gacha {
        use super::*;

        #[test]
        fn gilded_egg_charges_premium_and_skips_commons() {
            let (mut service, clock) = fixture();
            let outcome = service
                .adopt(OWNER, Some(GUILD), "Fancy", "gilded")
                .unwrap();
            let pet = outcome.pet;
            assert_eq!(outcome.egg_tier, "gilded");
            assert_eq!(pet.adopt_fee, 20 + 230);
            assert_eq!(pet.species, UNHATCHED_SPECIES);
            clock.set(pet.hatched_at);
            service.random_mut().push_tiered("rama");

            let hatched = service.get_status(OWNER, Some(GUILD)).unwrap().pet.unwrap();

            assert_ne!(get_species(&hatched.species).tier, SpeciesTier::Common);
            assert_eq!(service.store().balance(OWNER, GUILD), 750);
        }

        #[test]
        fn gilded_odds_over_many_rolls_never_common() {
            let (mut service, _clock) = fixture();
            let tiers = (0..200)
                .map(|_| {
                    let species = service.random_mut().tiered_species(GILDED_TIER_WEIGHTS);
                    get_species(&species).tier
                })
                .collect::<BTreeSet<_>>();

            assert!(!tiers.contains(&SpeciesTier::Common));
            assert!(tiers.contains(&SpeciesTier::Uncommon));
            assert!(
                service
                    .random()
                    .tier_weights
                    .iter()
                    .all(|weights| weights.common == 0)
            );
        }

        #[test]
        fn pity_activates_after_three_common_deaths() {
            let (mut service, clock) = fixture();
            service.store_mut().set_balance(OWNER, GUILD, 5_000);
            for index in 0..3 {
                let pet = adopt_common(&mut service, &format!("Common {index}"));
                clock.set(pet.hatched_at + 9 * DAY);
                assert!(
                    service
                        .living_pet(OWNER, Some(GUILD), clock.now())
                        .unwrap()
                        .is_none()
                );
            }
            assert!(service.pity_active(OWNER, Some(GUILD)).unwrap());

            let outcome = service
                .adopt(OWNER, Some(GUILD), "Pity Roll", "standard")
                .unwrap();

            assert!(outcome.pity_active);
            clock.set(outcome.pet.hatched_at);
            service.random_mut().push_tiered("jopacama");
            let pet = service.get_status(OWNER, Some(GUILD)).unwrap().pet.unwrap();
            assert_ne!(get_species(&pet.species).tier, SpeciesTier::Common);
            assert_eq!(
                service.random().tier_weights.last(),
                Some(&PITY_TIER_WEIGHTS)
            );
        }

        #[test]
        fn pity_broken_by_a_non_common() {
            let (mut service, clock) = fixture();
            service.store_mut().set_balance(OWNER, GUILD, 5_000);
            for (name, species) in [
                ("C1", "common_cama"),
                ("Fancy", "jopacama"),
                ("C2", "common_cama"),
            ] {
                let pet = adopt_species(&mut service, name, species, "standard");
                clock.set(pet.hatched_at + 9 * DAY);
                assert!(
                    service
                        .living_pet(OWNER, Some(GUILD), clock.now())
                        .unwrap()
                        .is_none()
                );
            }
            assert!(!service.pity_active(OWNER, Some(GUILD)).unwrap());
        }

        #[test]
        fn trinket_roll_equips_and_dupes_refund() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + DAY);
            service.random_mut().push_weighted("top_hat");

            let first = service.roll_trinket(OWNER, Some(GUILD)).unwrap();

            assert!(!first.duplicate);
            assert_eq!(first.net_cost, 25);
            assert_eq!(first.owned_count, 1);
            assert_eq!(
                stored(&mut service, pet.pet_id).accessory.as_deref(),
                Some("top_hat")
            );
            let after_first = service.store().balance(OWNER, GUILD);
            service.random_mut().push_weighted("top_hat");
            let duplicate = service.roll_trinket(OWNER, Some(GUILD)).unwrap();
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.net_cost, 15);
            assert_eq!(duplicate.owned_count, 1);
            assert_eq!(service.store().balance(OWNER, GUILD), after_first - 15);
        }

        #[test]
        fn trinket_requires_hatched_living_pet() {
            let (mut service, _clock) = fixture();
            assert_eq!(
                error_code(&service.roll_trinket(OWNER, Some(GUILD))),
                Some(NO_PET)
            );
            let _pet = adopt_common(&mut service, "Blep");
            assert_eq!(
                error_code(&service.roll_trinket(OWNER, Some(GUILD))),
                Some(PET_EGG)
            );
        }

        #[test]
        fn wear_trinket_owned_only() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + DAY);
            assert_eq!(
                error_code(&service.wear_trinket(OWNER, Some(GUILD), "red_bow")),
                Some(VALIDATION_ERROR)
            );
            service.random_mut().push_weighted("red_bow");
            assert!(service.roll_trinket(OWNER, Some(GUILD)).success());
            service.random_mut().push_weighted("top_hat");
            assert!(service.roll_trinket(OWNER, Some(GUILD)).success());

            let result = service.wear_trinket(OWNER, Some(GUILD), "red_bow");

            assert!(result.success());
            assert_eq!(
                stored(&mut service, pet.pet_id).accessory.as_deref(),
                Some("red_bow")
            );
        }

        #[test]
        fn camadex_counts_hatched_species_only() {
            let (mut service, clock) = fixture();
            assert_eq!(service.camadex(OWNER, Some(GUILD)).unwrap(), (vec![], 15));
            let pet = adopt_common(&mut service, "Blep");
            assert_eq!(
                service.camadex(OWNER, Some(GUILD)).unwrap().0,
                Vec::<String>::new()
            );
            clock.set(pet.hatched_at + DAY);

            let (raised, total) = service.camadex(OWNER, Some(GUILD)).unwrap();

            assert_eq!(raised, vec!["common_cama"]);
            assert_eq!(total, 15);
        }
    }

    mod match_hook {
        use super::*;

        #[test]
        fn win_feeds_living_pets_and_skips_eggs() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 2 * DAY);

            let fed = service.on_match_win(&[OWNER, 555], Some(GUILD));

            assert_eq!(fed.len(), 1);
            assert_eq!(fed[0].0.pet_id, pet.pet_id);
            assert_eq!(fed[0].1, 10);
            assert_eq!(fed[0].0.last_fed_at, clock.now());
            assert_eq!(fed[0].0.hunger_at_last_fed, 70);
            assert_eq!(service.get_status(OWNER, Some(GUILD)).unwrap().hunger, 70);
        }

        #[test]
        fn win_reports_only_fullness_actually_applied() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + DAY / 4);

            let fed = service.on_match_win(&[OWNER], Some(GUILD));

            assert_eq!(fed[0].1, 5);
            assert_eq!(service.get_status(OWNER, Some(GUILD)).unwrap().hunger, 100);
        }

        #[test]
        fn pack_cama_gets_bonus() {
            let (mut service, clock) = fixture();
            let pet = adopt_species(&mut service, "Donkey", "courier_cama", "standard");
            clock.set(pet.hatched_at + 2 * DAY);

            let fed = service.on_match_win(&[OWNER], Some(GUILD));

            assert_eq!(fed[0].1, 15);
            assert_eq!(fed[0].0.hunger_at_last_fed, 75);
        }

        #[test]
        fn hook_never_raises() {
            let (mut service, _clock) = fixture();
            service.store_mut().fail_active_lookup = true;
            assert!(service.on_match_win(&[OWNER], Some(GUILD)).is_empty());
        }
    }

    mod sweep {
        use super::*;

        #[test]
        fn sweep_hatches_once() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 60);

            let result = service.sweep(100).unwrap();

            assert_eq!(
                result
                    .hatches
                    .iter()
                    .map(|notice| notice.pet.pet_id)
                    .collect::<Vec<_>>(),
                vec![pet.pet_id]
            );
            service
                .mark_hatch_announced(&result.hatches[0].pet)
                .unwrap();
            assert!(service.sweep(100).unwrap().hatches.is_empty());
        }

        #[test]
        fn sweep_claims_death_at_starvation_moment() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            clock.set(pet.hatched_at + 9 * DAY);

            let result = service.sweep(100).unwrap();

            assert_eq!(result.deaths.len(), 1);
            assert_eq!(result.deaths[0].pet.died_at, Some(pet.hatched_at + 5 * DAY));
            assert_eq!(service.sweep(100).unwrap().deaths.len(), 1);
            service.mark_death_announced(&result.deaths[0].pet).unwrap();
            assert!(service.sweep(100).unwrap().deaths.is_empty());
        }

        #[test]
        fn refund_requires_alive_at_payout() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "tango", 4).success());
            clock.set(pet.hatched_at + DAY);
            assert!(service.feed(OWNER, Some(GUILD), "tango").success());
            service.store_mut().nonprofit_fund.insert(GUILD, 500);
            let balance_before = service.store().balance(OWNER, GUILD);
            clock.set(pet.hatched_at + 12 * DAY);

            let result = service.sweep(100).unwrap();

            assert!(result.refunds.is_empty());
            assert_eq!(service.store().balance(OWNER, GUILD), balance_before);
            assert!(stored(&mut service, pet.pet_id).died_at.is_some());
        }

        #[test]
        fn refund_pays_and_claims_window_once() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 3).success());
            clock.set(pet.hatched_at + 2 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            service.store_mut().nonprofit_fund.insert(GUILD, 500);
            let care_week = week_key_for_timestamp(clock.now()).unwrap();
            keep_alive_until_previous_week(&mut service, &clock, &care_week, "cheese");
            let balance_before = service.store().balance(OWNER, GUILD);
            service.random_mut().push_integer(200);

            let result = service.sweep(100).unwrap();

            assert_eq!(result.refunds.len(), 1);
            let notice = result.refunds[0].clone();
            assert_eq!(notice.week_key, care_week);
            assert_eq!(notice.total_paid, 60);
            assert!(!notice.scaled_down);
            assert_eq!(service.store().balance(OWNER, GUILD), balance_before + 60);
            service.random_mut().push_integer(200);
            assert_eq!(service.sweep(100).unwrap().refunds, vec![notice.clone()]);
            assert_eq!(service.store().balance(OWNER, GUILD), balance_before + 60);
            service.mark_refund_announced(&notice).unwrap();
            assert!(service.sweep(100).unwrap().refunds.is_empty());
        }

        #[test]
        fn stale_snapshot_never_kills_a_fed_pet() {
            let (mut service, clock) = fixture();
            let stale = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 2).success());
            clock.set(stale.hatched_at + 4 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            clock.set(stale.hatched_at + 6 * DAY);

            let resolved = service
                .resolve_starvation(stale.clone(), clock.now())
                .unwrap();

            assert!(resolved.as_ref().is_some_and(|pet| pet.died_at.is_none()));
            assert!(stored(&mut service, stale.pet_id).died_at.is_none());
        }

        #[test]
        fn empty_fund_leaves_week_unclaimed_until_topped_up() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 20).success());
            clock.set(pet.hatched_at + 2 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            let care_week = week_key_for_timestamp(clock.now()).unwrap();
            keep_alive_until_previous_week(&mut service, &clock, &care_week, "cheese");
            service.store_mut().nonprofit_fund.insert(GUILD, 0);
            service.random_mut().push_integer(200);

            assert!(service.sweep(100).unwrap().refunds.is_empty());
            assert!(
                !service
                    .store_mut()
                    .is_refund_window_paid(Some(GUILD), &care_week)
                    .unwrap()
            );
            service.store_mut().nonprofit_fund.insert(GUILD, 500);
            let _keep_alive_attempt = service.feed(OWNER, Some(GUILD), "cheese");
            service.random_mut().push_integer(200);
            let notices = service.sweep(100).unwrap().refunds;
            assert_eq!(notices.len(), 1);
            assert_eq!(notices[0].total_paid, 60);
        }

        #[test]
        fn outage_spanning_week_still_pays_both_weeks_via_lookback() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 25).success());
            clock.set(pet.hatched_at + 2 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            let week_w = week_key_for_timestamp(clock.now()).unwrap();
            service.store_mut().nonprofit_fund.insert(GUILD, 500);
            while week_key_for_timestamp(clock.now()).unwrap() == week_w {
                clock.set(clock.now() + DAY);
                assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            }
            let week_w1 = week_key_for_timestamp(clock.now()).unwrap();
            while week_key_for_timestamp(clock.now() + DAY).unwrap() == week_w1 {
                clock.set(clock.now() + DAY);
                assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            }
            clock.set(clock.now() + DAY);
            service.random_mut().push_integer(100);
            service.random_mut().push_integer(100);

            let notices = service.sweep(100).unwrap().refunds;

            let paid_weeks = notices
                .iter()
                .map(|notice| notice.week_key.clone())
                .collect::<BTreeSet<_>>();
            assert!(paid_weeks.contains(&week_w));
            assert!(paid_weeks.contains(&week_w1));
            assert_eq!(paid_weeks.len(), 2);
        }

        #[test]
        fn refund_scales_down_to_fund() {
            let (mut service, clock) = fixture();
            let pet = adopt_common(&mut service, "Blep");
            assert!(service.buy(OWNER, Some(GUILD), "cheese", 3).success());
            clock.set(pet.hatched_at + 2 * DAY);
            assert!(service.feed(OWNER, Some(GUILD), "cheese").success());
            service.store_mut().nonprofit_fund.insert(GUILD, 5);
            let care_week = week_key_for_timestamp(clock.now()).unwrap();
            keep_alive_until_previous_week(&mut service, &clock, &care_week, "cheese");
            service.random_mut().push_integer(200);

            let result = service.sweep(100).unwrap();

            let notice = result
                .refunds
                .iter()
                .find(|notice| notice.week_key == care_week)
                .unwrap();
            assert!(notice.scaled_down);
            assert_eq!(notice.total_paid, 5);
            assert_eq!(
                service
                    .store_mut()
                    .get_nonprofit_balance(Some(GUILD))
                    .unwrap(),
                0
            );
        }
    }
}
