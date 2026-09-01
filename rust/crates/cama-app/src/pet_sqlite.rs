//! Concrete existing-schema SQLite ports for [`crate::pet::PetService`].
//!
//! The repository methods retain Python's atomic transaction boundaries; this
//! adapter is intentionally mechanical and contains no duplicate pet policy.
//! Runtime callers invoke its synchronous methods from Tokio's blocking pool.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cama_db::core_repositories::PlayerRepository;
use cama_db::pet_eating_repository::PetEatingRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::pet_repository::{
    ActivatePetOutcome, ActivatePetRequest, AdoptPetRequest, AegisReviveRequest,
    BuySuppliesRequest, DeathClaimOutcome, DeathClaimRequest, FeedPetRequest, HungerTopUpRequest,
    PetRepository, PetRepositoryError, SacrificePetOutcome, SacrificePetRequest, SaltLickRequest,
    TrinketPurchaseOutcome, TrinketPurchaseRequest, UpgradeEggRequest,
};
use cama_domain::pet::{Pet, PetStatus, RENAME_COST, RefundNotice, adoption_fee};
use cama_domain::service_result::ServiceResult;

use crate::pet::{
    AdoptOutcome, BuyOutcome, Clock, NO_PET, PetRandom, PetService, PetStore, PlayerPort,
    SacrificeOutcome as ApplicationSacrificeOutcome, SacrificePreview, SweepOutcome,
    map_repo_error, validate_name,
};
use crate::pet_commands::PetCommandService;
use crate::pet_evolution_app::{PetEvolutionService, SystemEvolutionClock};

impl PetStore for PetRepository {
    fn get_active_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError> {
        PetRepository::get_active_pet(self, discord_id, guild_id)
    }

    fn get_pet_by_id(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError> {
        PetRepository::get_pet_by_id(self, pet_id, guild_id)
    }

    fn get_latest_dead_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Pet>, PetRepositoryError> {
        PetRepository::get_latest_dead_pet(self, discord_id, guild_id)
    }

    fn get_supplies(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<String, i64>, PetRepositoryError> {
        PetRepository::get_supplies(self, discord_id, guild_id)
    }

    fn count_dead_pets(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, PetRepositoryError> {
        PetRepository::count_dead_pets(self, discord_id, guild_id)
    }

    fn adopt_pet(&mut self, request: &AdoptPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
        PetRepository::adopt_pet(self, request)
    }

    fn upgrade_egg(&mut self, request: &UpgradeEggRequest<'_>) -> Result<Pet, PetRepositoryError> {
        PetRepository::upgrade_egg(self, request)
    }

    fn list_living_pets(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::list_living_pets(self, discord_id, guild_id)
    }

    fn activate_pet_atomic(
        &mut self,
        request: ActivatePetRequest,
    ) -> Result<ActivatePetOutcome, PetRepositoryError> {
        PetRepository::activate_pet_atomic(self, request)
    }

    fn sacrifice_and_adopt_atomic(
        &mut self,
        request: &SacrificePetRequest<'_>,
    ) -> Result<SacrificePetOutcome, PetRepositoryError> {
        PetRepository::sacrifice_and_adopt_atomic(self, request)
    }

    fn resolve_hatch_species(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        species: &str,
        now: i64,
    ) -> Result<Pet, PetRepositoryError> {
        PetRepository::resolve_hatch_species(self, pet_id, guild_id, species, now)
    }

    fn recent_dead_species(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<String>, PetRepositoryError> {
        PetRepository::recent_dead_species(self, discord_id, guild_id, limit)
    }

    fn buy_supplies(
        &mut self,
        request: &BuySuppliesRequest<'_>,
    ) -> Result<i64, PetRepositoryError> {
        PetRepository::buy_supplies(self, request)
    }

    fn feed_pet(&mut self, request: &FeedPetRequest<'_>) -> Result<Pet, PetRepositoryError> {
        PetRepository::feed_pet(self, request)
    }

    fn use_salt_lick(&mut self, request: &SaltLickRequest<'_>) -> Result<Pet, PetRepositoryError> {
        PetRepository::use_salt_lick(self, request)
    }

    fn buy_trinket_atomic(
        &mut self,
        request: &TrinketPurchaseRequest<'_>,
    ) -> Result<TrinketPurchaseOutcome, PetRepositoryError> {
        PetRepository::buy_trinket_atomic(self, request)
    }

    fn equip_accessory(
        &mut self,
        pet_id: i64,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: Option<&str>,
    ) -> Result<(), PetRepositoryError> {
        PetRepository::equip_accessory(self, pet_id, discord_id, guild_id, accessory_id)
    }

    fn get_accessories(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<String>, PetRepositoryError> {
        PetRepository::get_accessories(self, discord_id, guild_id)
    }

    fn species_raised(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Vec<String>, PetRepositoryError> {
        PetRepository::species_raised(self, discord_id, guild_id, now)
    }

    fn get_active_pets_for(
        &mut self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::get_active_pets_for(self, discord_ids, guild_id)
    }

    fn apply_hunger_top_up(
        &mut self,
        request: HungerTopUpRequest,
    ) -> Result<bool, PetRepositoryError> {
        PetRepository::apply_hunger_top_up(self, request)
    }

    fn claim_death(
        &mut self,
        request: &DeathClaimRequest<'_>,
    ) -> Result<Option<DeathClaimOutcome>, PetRepositoryError> {
        PetRepository::claim_death(self, request)
    }

    fn get_death_activation(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<DeathClaimOutcome>, PetRepositoryError> {
        PetRepository::get_death_activation(self, pet_id, guild_id)
    }

    fn revive_with_aegis(
        &mut self,
        request: AegisReviveRequest,
    ) -> Result<bool, PetRepositoryError> {
        PetRepository::revive_with_aegis(self, request)
    }

    fn find_starvation_candidates(
        &mut self,
        now: i64,
        decay_per_day: i64,
        max_decay_pct: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::find_starvation_candidates(self, now, decay_per_day, max_decay_pct)
    }

    fn get_unannounced_deaths(&mut self, limit: i64) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::get_unannounced_deaths(self, limit)
    }

    fn find_unannounced_hatches(
        &mut self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::find_unannounced_hatches(self, now, limit)
    }

    fn mark_death_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        PetRepository::mark_death_announced(self, pet_id, guild_id, announced_at)
    }

    fn mark_hatch_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        PetRepository::mark_hatch_announced(self, pet_id, guild_id, announced_at)
    }

    fn list_pet_guild_ids(&mut self) -> Result<Vec<i64>, PetRepositoryError> {
        PetRepository::list_pet_guild_ids(self)
    }

    fn list_week_care(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<Vec<Pet>, PetRepositoryError> {
        PetRepository::list_week_care(self, guild_id, week_key)
    }

    fn is_refund_window_paid(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
    ) -> Result<bool, PetRepositoryError> {
        PetRepository::is_refund_window_paid(self, guild_id, week_key)
    }

    fn get_nonprofit_balance(&mut self, guild_id: Option<i64>) -> Result<i64, PetRepositoryError> {
        PetRepository::get_nonprofit_balance(self, guild_id)
    }

    fn pay_refunds_atomic(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
        payouts: &[(i64, i64)],
        now: i64,
        announcement: Option<&RefundNotice>,
    ) -> Result<bool, PetRepositoryError> {
        PetRepository::pay_refunds_atomic(self, guild_id, week_key, payouts, now, announcement)
    }

    fn get_unannounced_refunds(
        &mut self,
        limit: i64,
    ) -> Result<Vec<RefundNotice>, PetRepositoryError> {
        PetRepository::get_unannounced_refunds(self, limit)
    }

    fn mark_refund_announced(
        &mut self,
        guild_id: Option<i64>,
        week_key: &str,
        announced_at: i64,
    ) -> Result<(), PetRepositoryError> {
        PetRepository::mark_refund_announced(self, guild_id, week_key, announced_at)
    }
}

#[derive(Clone, Debug)]
pub struct SqlitePetPlayerPort {
    repository: PlayerRepository,
}

impl SqlitePetPlayerPort {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            repository: PlayerRepository::new(database_path),
        }
    }
}

impl PlayerPort for SqlitePetPlayerPort {
    fn exists(&mut self, discord_id: i64, guild_id: i64) -> bool {
        self.repository
            .exists(discord_id, Some(guild_id))
            .unwrap_or(false)
    }
}

type SqlitePetEvolution =
    PetEvolutionService<PetEvolutionRepository, PetRepository, SystemEvolutionClock>;

/// Fully composed application service used by the live command provider and
/// supervised sweep. It owns only cloneable path-backed repositories; every
/// call still opens the shared database through Cama's runtime SQLite policy.
pub struct SqlitePetCommandService<R, C> {
    service: PetService<PetRepository, SqlitePetPlayerPort, R, C, SqlitePetEvolution>,
    database_path: PathBuf,
}

impl<R, C> SqlitePetCommandService<R, C>
where
    R: PetRandom,
    C: Clock,
{
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>, random: R, clock: C, decay_per_day: i64) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        let evolution = PetEvolutionService::new(
            PetEvolutionRepository::new(&database_path),
            PetRepository::new(&database_path),
            SystemEvolutionClock,
        );
        Self {
            service: PetService::new(
                PetRepository::new(&database_path),
                SqlitePetPlayerPort::new(&database_path),
                random,
                clock,
                evolution,
                decay_per_day,
            ),
            database_path,
        }
    }

    #[must_use]
    pub const fn service(
        &self,
    ) -> &PetService<PetRepository, SqlitePetPlayerPort, R, C, SqlitePetEvolution> {
        &self.service
    }

    /// Mutable access for application adapters that compose the same
    /// starvation/hatch policy with another pet command surface (brawls and
    /// reminders).  The accessor does not expose a second persistence path;
    /// callers still use this service's existing-schema repositories.
    pub fn service_mut(
        &mut self,
    ) -> &mut PetService<PetRepository, SqlitePetPlayerPort, R, C, SqlitePetEvolution> {
        &mut self.service
    }
}

impl<R, C> PetCommandService for SqlitePetCommandService<R, C>
where
    R: PetRandom,
    C: Clock,
{
    fn decay_per_day(&self) -> i64 {
        self.service.decay_per_day()
    }

    fn sweep(&mut self) -> Result<SweepOutcome, String> {
        let mut outcome = self.service.sweep(100).map_err(|error| error.to_string())?;
        let eating = PetEatingRepository::new(&self.database_path);
        for notice in &mut outcome.deaths {
            if notice.pet.death_cause.as_deref() != Some("eaten") {
                continue;
            }
            notice.eating_outcome = eating
                .get_eating_outcome(notice.pet.pet_id, Some(notice.pet.guild_id))
                .map_err(|error| error.to_string())?
                .map(|durable| {
                    BTreeMap::from([
                        ("reward".to_owned(), durable.reward),
                        (
                            "penalty_games_added".to_owned(),
                            durable.penalty_games_added,
                        ),
                        (
                            "penalty_games_remaining".to_owned(),
                            durable.penalty_games_remaining,
                        ),
                        ("new_balance".to_owned(), durable.new_balance),
                    ])
                });
        }
        Ok(outcome)
    }

    fn mark_hatch_announced(&mut self, pet: &Pet) -> Result<(), String> {
        self.service
            .mark_hatch_announced(pet)
            .map_err(|error| error.to_string())
    }

    fn mark_evolution_announced(&mut self, pet: &Pet) -> Result<(), String> {
        PetEvolutionRepository::new(&self.database_path)
            .mark_announced(pet.pet_id, Some(pet.guild_id), self.service.now())
            .map_err(|error| error.to_string())
    }

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String> {
        self.service
            .mark_death_announced(pet)
            .map_err(|error| error.to_string())
    }

    fn mark_refund_announced(&mut self, notice: &RefundNotice) -> Result<(), String> {
        self.service
            .mark_refund_announced(notice)
            .map_err(|error| error.to_string())
    }

    fn adopt(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        egg_tier: &str,
    ) -> ServiceResult<AdoptOutcome> {
        self.service.adopt(discord_id, guild_id, name, egg_tier)
    }

    fn stable(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Vec<Pet>> {
        self.service.stable(discord_id, guild_id)
    }

    fn activate(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<ActivatePetOutcome> {
        self.service.activate(discord_id, guild_id, pet_id)
    }

    fn upgrade(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<Pet> {
        self.service.upgrade(discord_id, guild_id, pet_id)
    }

    fn sacrifice_preview(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SacrificePreview> {
        self.service.sacrifice_preview(discord_id, guild_id)
    }

    fn sacrifice(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
    ) -> ServiceResult<ApplicationSacrificeOutcome> {
        self.service.sacrifice(discord_id, guild_id, name)
    }

    fn status(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetStatus> {
        self.service.get_status(discord_id, guild_id)
    }

    fn next_adoption_fee(&mut self, discord_id: i64, guild_id: Option<i64>) -> i64 {
        self.service
            .store_mut()
            .count_dead_pets(discord_id, guild_id)
            .map(adoption_fee)
            .unwrap_or_else(|_| adoption_fee(0))
    }

    fn balance(&mut self, discord_id: i64, guild_id: Option<i64>) -> Option<i64> {
        PlayerRepository::new(&self.database_path)
            .get_by_id(discord_id, guild_id)
            .ok()
            .flatten()
            .map(|player| player.jopacoin_balance)
    }

    fn buy(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        quantity: i64,
    ) -> ServiceResult<BuyOutcome> {
        self.service.buy(discord_id, guild_id, item_id, quantity)
    }

    fn feed(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
    ) -> ServiceResult<cama_domain::pet::FeedOutcome> {
        self.service.feed(discord_id, guild_id, item_id)
    }

    fn roll_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<cama_domain::pet::TrinketOutcome> {
        self.service.roll_trinket(discord_id, guild_id)
    }

    fn wear_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: &str,
    ) -> ServiceResult<String> {
        self.service
            .wear_trinket(discord_id, guild_id, accessory_id)
    }

    fn owned_trinkets(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<String> {
        self.service
            .store_mut()
            .get_accessories(discord_id, guild_id)
            .unwrap_or_default()
    }

    fn rename(&mut self, discord_id: i64, guild_id: Option<i64>, name: &str) -> ServiceResult<Pet> {
        if let Some(error) = validate_name(name) {
            return error;
        }
        let trimmed = name.trim();
        let Some(pet) = self
            .service
            .living_pet(discord_id, guild_id, self.service.now())
            .unwrap_or_default()
        else {
            return ServiceResult::fail_with_code("You have no living pet.", NO_PET);
        };
        if let Err(error) = self.service.store_mut().rename_pet(
            pet.pet_id,
            discord_id,
            guild_id,
            trimmed,
            RENAME_COST,
        ) {
            return map_repo_error(error, Some(RENAME_COST));
        }
        match self.service.store_mut().get_pet_by_id(pet.pet_id, guild_id) {
            Ok(Some(pet)) => ServiceResult::ok(pet),
            Ok(None) => ServiceResult::fail_with_code("You have no living pet.", NO_PET),
            Err(error) => map_repo_error(error, None),
        }
    }

    fn graveyard(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<Pet> {
        PetRepository::new(&self.database_path)
            .get_graveyard(discord_id, guild_id, 10)
            .unwrap_or_default()
    }

    fn leaderboard(&mut self, guild_id: Option<i64>) -> Vec<Pet> {
        PetRepository::new(&self.database_path)
            .get_oldest_living(guild_id, 10)
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "pet_sqlite/tests.rs"]
mod tests;
