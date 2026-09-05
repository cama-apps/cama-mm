//! Adult-pet eating policy and Discord-neutral command orchestration.
//!
//! Python remains the live SQLite, Discord, reminder, and randomness authority
//! during the side-by-side migration. This module keeps every one of those
//! boundaries behind a typed port while preserving the service validation,
//! inclusive rolls, confirmation copy, delivery, and death-announcement rules.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::profit_deductions::ProfitDeductions;
use cama_domain::pet::{
    PET_EAT_PENALTY_GAMES_MAX, PET_EAT_PENALTY_GAMES_MIN, PET_EAT_REWARD_MAX, PET_EAT_REWARD_MIN,
    Pet, PetStage, PetStatus,
};
use cama_domain::service_result::ServiceResult;

use crate::pet_commands::withholding_note;

pub const NO_PET: &str = "no_pet";
pub const PET_NOT_ADULT: &str = "pet_not_adult";
pub const BRAWL_BUSY: &str = "brawl_busy";
pub const VALIDATION_ERROR: &str = "validation_error";
pub const EAT_CONFIRM_LABEL: &str = "Eat them";
pub const EAT_CANCEL_LABEL: &str = "Let them live";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PetEatingRepositoryFailure {
    StalePet,
    InBrawl,
    PetNotAdult,
    NoPet,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EatAdultPetRequest {
    pub pet_id: i64,
    pub expected_last_fed_at: i64,
    pub expected_hunger: i64,
    pub reward: i64,
    pub penalty_games: i64,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EatAdultPetCommit {
    pub pet: Pet,
    pub activated_pet: Option<Pet>,
    pub new_balance: i64,
    pub penalty_games_remaining: i64,
    /// Shares withheld from the reward.
    pub deductions: ProfitDeductions,
}

/// Persistence boundary for the preview, atomic consume, and delivered-death
/// acknowledgement. A runtime adapter must keep the consume operation atomic.
pub trait PetEatingRepositoryPort {
    type Error: Display;

    fn living_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Option<Pet>, Self::Error>;

    fn has_open_brawl(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<bool, Self::Error>;

    fn eat_adult_pet_atomic(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        request: &EatAdultPetRequest,
    ) -> Result<EatAdultPetCommit, Self::Error>;

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), Self::Error>;

    fn classify_error(error: &Self::Error) -> PetEatingRepositoryFailure;
}

pub trait PetEatingClock {
    fn now_seconds(&mut self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPetEatingClock;

impl PetEatingClock for SystemPetEatingClock {
    fn now_seconds(&mut self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

/// Supplies an integer from the inclusive `minimum..=maximum` interval.
pub trait PetEatingRandomPort {
    fn inclusive_i64(&mut self, minimum: i64, maximum: i64) -> i64;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetEatingOutcome {
    pub pet: Pet,
    pub activated_pet: Option<Pet>,
    pub reward: i64,
    pub penalty_games_added: i64,
    pub penalty_games_remaining: i64,
    pub new_balance: i64,
    /// Shares withheld from `reward`.
    pub deductions: ProfitDeductions,
}

#[derive(Clone, Debug)]
pub struct PetEatingService<R, C, N> {
    repository: R,
    clock: C,
    random: N,
}

impl<R, C, N> PetEatingService<R, C, N>
where
    R: PetEatingRepositoryPort,
    C: PetEatingClock,
    N: PetEatingRandomPort,
{
    #[must_use]
    pub const fn new(repository: R, clock: C, random: N) -> Self {
        Self {
            repository,
            clock,
            random,
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    #[must_use]
    pub const fn random(&self) -> &N {
        &self.random
    }

    pub fn eat_preview(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Pet> {
        let now = self.clock.now_seconds();
        self.edible_pet(discord_id, guild_id, now)
    }

    pub fn eat(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<PetEatingOutcome> {
        for _ in 0..2 {
            let now = self.clock.now_seconds();
            let pet = match self.edible_pet(discord_id, guild_id, now) {
                ServiceResult::Success(pet) => pet,
                ServiceResult::Failure { error, error_code } => {
                    return ServiceResult::Failure { error, error_code };
                }
            };
            let reward = self
                .random
                .inclusive_i64(PET_EAT_REWARD_MIN, PET_EAT_REWARD_MAX);
            let penalty_games = self
                .random
                .inclusive_i64(PET_EAT_PENALTY_GAMES_MIN, PET_EAT_PENALTY_GAMES_MAX);
            let request = EatAdultPetRequest {
                pet_id: pet.pet_id,
                expected_last_fed_at: pet.last_fed_at,
                expected_hunger: pet.hunger_at_last_fed,
                reward,
                penalty_games,
                now,
            };
            match self
                .repository
                .eat_adult_pet_atomic(discord_id, guild_id, &request)
            {
                Ok(commit) => {
                    return ServiceResult::ok(PetEatingOutcome {
                        pet: commit.pet,
                        activated_pet: commit.activated_pet,
                        reward,
                        penalty_games_added: penalty_games,
                        penalty_games_remaining: commit.penalty_games_remaining,
                        new_balance: commit.new_balance,
                        deductions: commit.deductions,
                    });
                }
                Err(error) if R::classify_error(&error) == PetEatingRepositoryFailure::StalePet => {
                }
                Err(error) => return map_repository_error::<R, _>(&error),
            }
        }
        ServiceResult::fail_with_code(
            "Your pet's state changed before the deed — try again.",
            VALIDATION_ERROR,
        )
    }

    fn edible_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> ServiceResult<Pet> {
        let pet = match self.repository.living_pet(discord_id, guild_id, now) {
            Ok(Some(pet)) => pet,
            Ok(None) => {
                return ServiceResult::fail_with_code("You have no living pet to eat.", NO_PET);
            }
            Err(error) => return map_repository_error::<R, _>(&error),
        };
        if pet.stage(now) != PetStage::Adult {
            return ServiceResult::fail_with_code(
                "Only a fully grown adult cama can be eaten.",
                PET_NOT_ADULT,
            );
        }
        match self.repository.has_open_brawl(discord_id, guild_id, now) {
            Ok(true) => ServiceResult::fail_with_code(
                "Your cama has an open brawl—settle that first.",
                BRAWL_BUSY,
            ),
            Ok(false) => ServiceResult::ok(pet),
            Err(error) => map_repository_error::<R, _>(&error),
        }
    }
}

fn map_repository_error<R, T>(error: &R::Error) -> ServiceResult<T>
where
    R: PetEatingRepositoryPort,
{
    match R::classify_error(error) {
        PetEatingRepositoryFailure::NoPet => {
            ServiceResult::fail_with_code("You have no living pet to eat.", NO_PET)
        }
        PetEatingRepositoryFailure::PetNotAdult => ServiceResult::fail_with_code(
            "Only a fully grown adult cama can be eaten.",
            PET_NOT_ADULT,
        ),
        PetEatingRepositoryFailure::InBrawl => ServiceResult::fail_with_code(
            "Your cama has an open brawl—settle that first.",
            BRAWL_BUSY,
        ),
        PetEatingRepositoryFailure::StalePet | PetEatingRepositoryFailure::Unavailable => {
            ServiceResult::fail_with_code(format!("Pet error: {error}"), VALIDATION_ERROR)
        }
    }
}

/// Narrow service surface needed by the command adapter.
pub trait PetEatingApplicationPort {
    fn eat_preview(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Pet>;

    fn eat(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetEatingOutcome>;

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String>;
}

impl<R, C, N> PetEatingApplicationPort for PetEatingService<R, C, N>
where
    R: PetEatingRepositoryPort,
    C: PetEatingClock,
    N: PetEatingRandomPort,
{
    fn eat_preview(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Pet> {
        PetEatingService::eat_preview(self, discord_id, guild_id)
    }

    fn eat(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetEatingOutcome> {
        PetEatingService::eat(self, discord_id, guild_id)
    }

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String> {
        self.repository
            .mark_death_announced(pet)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EatingEmbedColor {
    Blue,
    Dead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EatingEmbed {
    pub title: String,
    pub description: String,
    pub color: EatingEmbedColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationChoice {
    Eat,
    Spare,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfirmationButton {
    pub label: &'static str,
    pub choice: ConfirmationChoice,
}

#[must_use]
pub const fn confirmation_buttons() -> [ConfirmationButton; 2] {
    [
        ConfirmationButton {
            label: EAT_CONFIRM_LABEL,
            choice: ConfirmationChoice::Eat,
        },
        ConfirmationButton {
            label: EAT_CANCEL_LABEL,
            choice: ConfirmationChoice::Spare,
        },
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationDecision {
    Confirmed,
    Declined,
    TimedOut,
}

pub trait PetEatingDiscordPort {
    type Error;
    type Message: Clone;

    fn send_initial_private(&mut self, content: &str) -> Result<(), Self::Error>;
    fn defer_public(&mut self) -> Result<(), Self::Error>;
    fn send_warning(
        &mut self,
        embed: &EatingEmbed,
        buttons: &[ConfirmationButton],
    ) -> Result<Option<Self::Message>, Self::Error>;
    fn await_confirmation(&mut self, owner_id: i64) -> ConfirmationDecision;
    fn edit_message(
        &mut self,
        message: &Self::Message,
        embed: &EatingEmbed,
        clear_view: bool,
    ) -> Result<(), Self::Error>;
    fn send_public(&mut self, embed: &EatingEmbed) -> Result<(), Self::Error>;
    fn send_private_followup(&mut self, content: &str) -> Result<(), Self::Error>;
}

pub trait PetEatingReminderPort {
    fn cancel_pet_reminder(&mut self, discord_id: i64, guild_id: i64);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetEatingCommandOutcome {
    ValidationRejected,
    DeferFailed,
    WarningFailed,
    Spared,
    EatingRejected,
    DeliveryFailed,
    Delivered(Box<PetEatingOutcome>),
}

pub struct PetEatingCommand<S, D, M> {
    service: S,
    discord: D,
    reminders: M,
    direct_death_deliveries: BTreeMap<i64, usize>,
}

impl<S, D, M> PetEatingCommand<S, D, M>
where
    S: PetEatingApplicationPort,
    D: PetEatingDiscordPort,
    M: PetEatingReminderPort,
{
    #[must_use]
    pub fn new(service: S, discord: D, reminders: M) -> Self {
        Self {
            service,
            discord,
            reminders,
            direct_death_deliveries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn service(&self) -> &S {
        &self.service
    }

    #[must_use]
    pub const fn discord(&self) -> &D {
        &self.discord
    }

    #[must_use]
    pub const fn reminders(&self) -> &M {
        &self.reminders
    }

    #[must_use]
    pub fn active_direct_deliveries(&self) -> &BTreeMap<i64, usize> {
        &self.direct_death_deliveries
    }

    pub fn run(&mut self, discord_id: i64, guild_id: i64) -> PetEatingCommandOutcome {
        let pet = match self.service.eat_preview(discord_id, Some(guild_id)) {
            ServiceResult::Success(pet) => pet,
            ServiceResult::Failure { error, .. } => {
                let _ = self.discord.send_initial_private(&format!("❌ {error}"));
                return PetEatingCommandOutcome::ValidationRejected;
            }
        };
        if self.discord.defer_public().is_err() {
            return PetEatingCommandOutcome::DeferFailed;
        }

        let warning = build_eating_warning_embed(&pet);
        let message = match self.discord.send_warning(&warning, &confirmation_buttons()) {
            Ok(message) => message,
            Err(_) => return PetEatingCommandOutcome::WarningFailed,
        };
        match self.discord.await_confirmation(discord_id) {
            ConfirmationDecision::Declined | ConfirmationDecision::TimedOut => {
                if let Some(message) = message.as_ref() {
                    let _ = self
                        .discord
                        .edit_message(message, &build_spared_embed(&pet), true);
                }
                return PetEatingCommandOutcome::Spared;
            }
            ConfirmationDecision::Confirmed => {}
        }

        self.begin_direct_delivery(pet.pet_id);
        let result = self.service.eat(discord_id, Some(guild_id));
        let outcome = match result {
            ServiceResult::Success(outcome) => outcome,
            ServiceResult::Failure { error, .. } => {
                let _ = self.discord.send_private_followup(&format!("❌ {error}"));
                if let Some(message) = message.as_ref() {
                    let _ = self.discord.edit_message(message, &warning, true);
                }
                self.end_direct_delivery(pet.pet_id);
                return PetEatingCommandOutcome::EatingRejected;
            }
        };

        self.reminders
            .cancel_pet_reminder(outcome.pet.discord_id, outcome.pet.guild_id);
        let posted = self.publish_outcome(message.as_ref(), &build_eating_outcome_embed(&outcome));
        if posted {
            let _ = self.service.mark_death_announced(&outcome.pet);
        }
        self.end_direct_delivery(pet.pet_id);
        if posted {
            PetEatingCommandOutcome::Delivered(Box::new(outcome))
        } else {
            PetEatingCommandOutcome::DeliveryFailed
        }
    }

    fn publish_outcome(&mut self, message: Option<&D::Message>, embed: &EatingEmbed) -> bool {
        if let Some(message) = message
            && self.discord.edit_message(message, embed, true).is_ok()
        {
            return true;
        }
        self.discord.send_public(embed).is_ok()
    }

    fn begin_direct_delivery(&mut self, pet_id: i64) {
        *self.direct_death_deliveries.entry(pet_id).or_default() += 1;
    }

    fn end_direct_delivery(&mut self, pet_id: i64) {
        match self.direct_death_deliveries.get(&pet_id).copied() {
            Some(count) if count > 1 => {
                self.direct_death_deliveries.insert(pet_id, count - 1);
            }
            Some(_) => {
                self.direct_death_deliveries.remove(&pet_id);
            }
            None => {}
        }
    }
}

#[must_use]
pub fn build_eating_warning_embed(pet: &Pet) -> EatingEmbed {
    EatingEmbed {
        title: "🍽️ A terrible hunger stirs…".to_owned(),
        description: format!(
            "Eat **{}**?\n\nYou may be rewarded handsomely—but bad karma will tax your future earnings until you win your way free.\n\n**This cannot be undone. {} will be gone forever.**",
            pet.name, pet.name
        ),
        color: EatingEmbedColor::Dead,
    }
}

#[must_use]
pub fn build_spared_embed(pet: &Pet) -> EatingEmbed {
    EatingEmbed {
        title: "The hunger passes".to_owned(),
        description: format!("**{}** lives to graze another day.", pet.name),
        color: EatingEmbedColor::Blue,
    }
}

#[must_use]
pub fn build_eating_outcome_embed(outcome: &PetEatingOutcome) -> EatingEmbed {
    let activation = outcome
        .activated_pet
        .as_ref()
        .map_or_else(String::new, |pet| {
            format!(
                "\n\n🏡 **{}** was automatically activated from your stable.",
                pet.name
            )
        });
    let note = withholding_note(
        outcome.deductions.bankruptcy_penalty,
        outcome.deductions.vanity_tax,
        outcome.deductions.low_priority_tax,
    );
    EatingEmbed {
        title: format!("🍖 {} was delicious", outcome.pet.name),
        description: format!(
            "You received **{}** JC{note}.\n\nBad karma adds **{}** game(s) to your sentence (**{} remaining**). Your future earnings are taxed until you win your way free.\n\nBalance: **{}** JC.{activation}",
            grouped(outcome.reward - outcome.deductions.total()),
            outcome.penalty_games_added,
            outcome.penalty_games_remaining,
            grouped(outcome.new_balance)
        ),
        color: EatingEmbedColor::Dead,
    }
}

fn grouped(amount: i64) -> String {
    let digits = amount.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if amount < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

#[must_use]
pub fn build_death_copy(pet: &Pet) -> String {
    let cause = match pet.death_cause.as_deref() {
        Some("sacrifice") => "was given to the altar",
        Some("eaten") => "was eaten",
        _ => "starved",
    };
    format!(
        "<@{}>'s {} {cause}. Time of death: <t:{}:f>. F.",
        pet.discord_id,
        pet.name,
        pet.died_at.unwrap_or_default()
    )
}

#[must_use]
pub fn build_petless_memorial_copy(status: &PetStatus, owner_name: &str, next_fee: i64) -> String {
    let Some(dead) = status.last_dead.as_ref() else {
        return format!(
            "{owner_name} has no cama. Adopt a mysterious egg with `/pet adopt` — {next_fee} JC"
        );
    };
    let death_phrase = if dead.death_cause.as_deref() == Some("eaten") {
        "was eaten".to_owned()
    } else {
        format!(
            "died of {}",
            dead.death_cause.as_deref().unwrap_or("starvation")
        )
    };
    format!(
        "In memoriam: {} — {death_phrase}. F. Adopt again with `/pet adopt` — {next_fee} JC",
        dead.name
    )
}

#[cfg(test)]
#[path = "pet_eating/tests.rs"]
mod tests;
