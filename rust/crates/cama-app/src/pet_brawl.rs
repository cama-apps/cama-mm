//! Application orchestration for pet brawls.
//!
//! Discord, SQLite, clocks, and entropy are explicit ports. The service owns
//! validation and call ordering while combat remains in `cama-domain` and
//! transactional persistence remains in `cama-db`.

use std::collections::BTreeMap;

use cama_db::pet_brawl_repository::{
    BrawlSettlement, BrawlSettlementResult, DrawSettlementResult, SweepResult,
};
use cama_db::profit_deductions::ProfitDeductions;
use cama_domain::pet::{
    BRAWL_INSURANCE_REDUCTION, BRAWL_LOSS_FLOOR, BRAWL_LOSS_HUNGER, BRAWL_LOSS_XP,
    BRAWL_TRAINING_LEVEL_CAP, BRAWL_TRAINING_STAT_CAP, BRAWL_TRAINING_XP_CAP, BRAWL_WIN_HUNGER,
    BRAWL_WIN_HUNGER_DAILY_CAP, BRAWL_WIN_XP, BRAWL_XP_PER_LEVEL, PET_BRAWL_ACCEPT_SECONDS,
    PET_BRAWL_ACTIVE_TTL_SECONDS, Pet, PetMood, SOLO_TRAINING_XP_PER_SESSION, get_species,
};
use cama_domain::pet_brawl::{
    BrawlWinner, Duelist, DuelistSnapshot, PetBrawl, PetBrawlState, brawl_traits, build_duelist,
    initial_state,
};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::service_result::ServiceResult;

pub const NOT_FOUND: &str = "not_found";
pub const VALIDATION_ERROR: &str = "validation_error";
pub const INSUFFICIENT_FUNDS: &str = "insufficient_funds";
pub const COOLDOWN_ACTIVE: &str = "cooldown_active";
pub const NO_PET: &str = "no_pet";
pub const PET_DEAD: &str = "pet_dead";
pub const PET_EGG: &str = "pet_egg";
pub const BRAWL_BUSY: &str = "brawl_busy";

const BUILD_TITLES: [(&str, &str); 3] = [
    ("str", "Bruiser"),
    ("int", "Tactician"),
    ("dex", "Trickster"),
];

const SOLO_TRAINING_COPY: [&str; 3] = [
    "🏃 **{pet}** tears through a dusty obstacle course.",
    "🎯 **{pet}** practices until the training dummy looks nervous.",
    "🧩 **{pet}** studies an increasingly complicated pile of snacks.",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    pub code: String,
}

impl PortError {
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

pub type PortResult<T> = Result<T, PortError>;

pub trait Clock {
    fn now(&self) -> i64;
}

/// Entropy operations consumed by application policy.
pub trait ServiceRng {
    fn next_u64(&mut self) -> u64;
    fn choose_index(&mut self, len: usize) -> usize;
    fn random_unit(&mut self) -> f64;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WagerInput {
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

impl From<i64> for WagerInput {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoloTrainingPortOutcome {
    pub sessions_used: i64,
    pub sessions_available: i64,
    pub xp_delta: i64,
    pub stat_gain: Option<String>,
}

pub trait PetPort {
    fn decay_per_day(&self) -> i64;
    fn living_pet(&mut self, discord_id: i64, guild_id: Option<i64>, now: i64) -> Option<Pet>;
    fn living_pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>, now: i64) -> Option<Pet>;
    fn get_pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Option<Pet>;
    fn available_solo_training_sessions(&self, pet: &Pet, now: i64) -> i64;
    fn train_solo_atomic(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        now: i64,
        proposed_stat: Option<&str>,
    ) -> PortResult<SoloTrainingPortOutcome>;
}

#[allow(clippy::too_many_arguments)]
pub trait PetBrawlPort {
    fn get_brawl(&mut self, brawl_id: i64, guild_id: Option<i64>) -> PortResult<Option<PetBrawl>>;
    fn create_brawl_atomic(
        &mut self,
        guild_id: Option<i64>,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        challenger_pet_id: i64,
        now: i64,
        expires_at: i64,
        wager: i64,
        fee: i64,
    ) -> PortResult<PetBrawl>;
    fn accept_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        recipient_pet_id: i64,
        now: i64,
    ) -> PortResult<PetBrawl>;
    fn decline_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
        now: i64,
    ) -> PortResult<()>;
    fn withdraw_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
        now: i64,
    ) -> PortResult<()>;
    fn void_atomic(&mut self, brawl_id: i64, guild_id: Option<i64>, now: i64) -> PortResult<()>;
    fn settle_draw_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        participant_pet_ids: (i64, i64),
        rounds: i64,
        now: i64,
    ) -> PortResult<DrawSettlementResult>;
    fn settle_brawl_atomic(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        settlement: BrawlSettlement<'_>,
    ) -> PortResult<BrawlSettlementResult>;
    fn get_pet_record(&mut self, pet_id: i64, guild_id: Option<i64>) -> PortResult<(i64, i64)>;
    fn get_records_for(
        &mut self,
        pet_ids: &[i64],
        guild_id: Option<i64>,
    ) -> PortResult<BTreeMap<i64, (i64, i64)>>;
    fn sweep_stale(&mut self, now: i64, active_ttl_seconds: i64) -> PortResult<SweepResult>;
}

pub trait PetEvolutionPort {
    fn record_activity(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        activity: PetActivity,
        source_key: &str,
        occurred_at: i64,
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoloTrainingOutcome {
    pub sessions_used: i64,
    pub sessions_available: i64,
    pub xp_delta: i64,
    pub stat_gain: Option<String>,
    pub pet: Pet,
    pub career: CareerSummary,
    pub flavor: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChallengeOutcome {
    pub brawl: PetBrawl,
    pub challenger_pet: Pet,
    pub recipient_pet: Pet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptOutcome {
    pub brawl: PetBrawl,
    pub state: PetBrawlState,
    /// Seed handed to the transport layer for a battle-local RNG.
    pub combat_seed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CareerSummary {
    pub xp: i64,
    pub strength: i64,
    pub intelligence: i64,
    pub dexterity: i64,
    pub rank: Option<String>,
    pub build: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DrawOutcome {
    pub duelists: (Duelist, Duelist),
    pub records: BTreeMap<i64, (i64, i64)>,
    pub wager: i64,
    pub fee: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisiveOutcome {
    pub winner: Duelist,
    pub loser: Duelist,
    pub winner_delta: i64,
    pub loser_delta: i64,
    pub winner_xp_delta: i64,
    pub loser_xp_delta: i64,
    pub winner_stat_gain: Option<String>,
    pub loser_stat_gain: Option<String>,
    pub payout: i64,
    pub wager: i64,
    pub fee: i64,
    /// Shares withheld from the winner's profit (the wager).
    pub deductions: ProfitDeductions,
    pub records: BTreeMap<i64, (i64, i64)>,
    pub career: BTreeMap<i64, CareerSummary>,
    pub personality_event_key: Option<String>,
    pub personality_event: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettleOutcome {
    Draw(DrawOutcome),
    Decisive(DecisiveOutcome),
}

pub struct PetBrawlService<P, B, C, R, E>
where
    P: PetPort,
    B: PetBrawlPort,
    C: Clock,
    R: ServiceRng,
    E: PetEvolutionPort,
{
    pet_port: P,
    brawl_port: B,
    clock: C,
    rng: R,
    evolution_port: Option<E>,
    tip_fee_rate: f64,
}

impl<P, B, C, R, E> PetBrawlService<P, B, C, R, E>
where
    P: PetPort,
    B: PetBrawlPort,
    C: Clock,
    R: ServiceRng,
    E: PetEvolutionPort,
{
    #[must_use]
    pub const fn new(
        pet_port: P,
        brawl_port: B,
        clock: C,
        rng: R,
        evolution_port: Option<E>,
        tip_fee_rate: f64,
    ) -> Self {
        Self {
            pet_port,
            brawl_port,
            clock,
            rng,
            evolution_port,
            tip_fee_rate,
        }
    }

    #[must_use]
    pub const fn pet_port(&self) -> &P {
        &self.pet_port
    }

    #[must_use]
    pub const fn brawl_port(&self) -> &B {
        &self.brawl_port
    }

    #[must_use]
    pub const fn evolution_port(&self) -> Option<&E> {
        self.evolution_port.as_ref()
    }

    pub fn train_solo(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SoloTrainingOutcome> {
        let now = self.clock.now();
        let Some(pet) = self.pet_port.living_pet(discord_id, guild_id, now) else {
            return failure("You have no living pet to train.", NO_PET);
        };
        if now < pet.hatched_at {
            return failure("Eggs need to hatch before they can train.", PET_EGG);
        }
        let available = self.pet_port.available_solo_training_sessions(&pet, now);
        let award =
            (available * SOLO_TRAINING_XP_PER_SESSION).min(BRAWL_TRAINING_XP_CAP - pet.training_xp);
        let proposed_stat = self.choose_stat_gain(&pet, award);
        let outcome = match self.pet_port.train_solo_atomic(
            pet.pet_id,
            guild_id,
            now,
            proposed_stat.as_deref(),
        ) {
            Ok(outcome) => outcome,
            Err(error) => return map_port_error(error),
        };
        let Some(updated) = self.pet_port.get_pet_by_id(pet.pet_id, guild_id) else {
            return failure("Something went wrong with that brawl.", VALIDATION_ERROR);
        };
        let (wins, losses) = match self.brawl_port.get_pet_record(pet.pet_id, guild_id) {
            Ok(record) => record,
            Err(error) => return map_port_error(error),
        };
        let flavor_template = SOLO_TRAINING_COPY[self.rng.choose_index(SOLO_TRAINING_COPY.len())];
        ServiceResult::ok(SoloTrainingOutcome {
            sessions_used: outcome.sessions_used,
            sessions_available: outcome.sessions_available,
            xp_delta: outcome.xp_delta,
            stat_gain: outcome.stat_gain,
            career: Self::career_summary(&updated, wins, losses),
            flavor: flavor_template.replace("{pet}", &updated.name),
            pet: updated,
        })
    }

    pub fn challenge(
        &mut self,
        challenger_id: i64,
        recipient_id: i64,
        guild_id: Option<i64>,
        channel_id: i64,
        wager: WagerInput,
    ) -> ServiceResult<ChallengeOutcome> {
        let wager = match wager {
            WagerInput::Integer(wager) if (0..=100).contains(&wager) => wager,
            WagerInput::Integer(_) | WagerInput::Float(_) | WagerInput::Boolean(_) => {
                return failure(
                    "Pet-brawl wagers must be whole numbers from 0 to 100 JC.",
                    VALIDATION_ERROR,
                );
            }
        };
        if challenger_id == recipient_id {
            return failure("Your cama refuses to fight itself.", VALIDATION_ERROR);
        }
        let now = self.clock.now();
        let challenger_pet =
            match self.battle_ready_pet(challenger_id, guild_id, now, PetOwner::Challenger) {
                ServiceResult::Success(pet) => pet,
                ServiceResult::Failure { error, error_code } => {
                    return ServiceResult::Failure { error, error_code };
                }
            };
        let recipient_pet =
            match self.battle_ready_pet(recipient_id, guild_id, now, PetOwner::Recipient) {
                ServiceResult::Success(pet) => pet,
                ServiceResult::Failure { error, error_code } => {
                    return ServiceResult::Failure { error, error_code };
                }
            };
        let fee = if wager == 0 {
            0
        } else {
            ((wager as f64 * self.tip_fee_rate).ceil() as i64).max(1)
        };
        match self.brawl_port.create_brawl_atomic(
            guild_id,
            channel_id,
            challenger_id,
            recipient_id,
            challenger_pet.pet_id,
            now,
            now + PET_BRAWL_ACCEPT_SECONDS,
            wager,
            fee,
        ) {
            Ok(brawl) => ServiceResult::ok(ChallengeOutcome {
                brawl,
                challenger_pet,
                recipient_pet,
            }),
            Err(error) => map_port_error(error),
        }
    }

    pub fn accept(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<AcceptOutcome> {
        let now = self.clock.now();
        let brawl = match self.brawl_port.get_brawl(brawl_id, guild_id) {
            Ok(Some(brawl)) => brawl,
            Ok(None) => return failure("That brawl no longer exists.", NOT_FOUND),
            Err(error) => return map_port_error(error),
        };
        if self
            .pet_port
            .living_pet_by_id(brawl.challenger_pet_id, guild_id, now)
            .is_none()
        {
            let _ignored = self.brawl_port.void_atomic(brawl_id, guild_id, now);
            return failure(
                "The challenger's pet is no longer with us. Challenge voided.",
                PET_DEAD,
            );
        }
        let recipient_pet =
            match self.battle_ready_pet(recipient_id, guild_id, now, PetOwner::Challenger) {
                ServiceResult::Success(pet) => pet,
                ServiceResult::Failure { error, error_code } => {
                    return ServiceResult::Failure { error, error_code };
                }
            };
        let brawl = match self.brawl_port.accept_atomic(
            brawl_id,
            guild_id,
            recipient_id,
            recipient_pet.pet_id,
            now,
        ) {
            Ok(brawl) => brawl,
            Err(error) => return map_port_error(error),
        };
        let challenger_pet = self
            .pet_port
            .living_pet_by_id(brawl.challenger_pet_id, guild_id, now);
        let recipient_pet = brawl
            .recipient_pet_id
            .and_then(|pet_id| self.pet_port.living_pet_by_id(pet_id, guild_id, now));
        let (Some(challenger_pet), Some(recipient_pet)) = (challenger_pet, recipient_pet) else {
            let _ignored = self.brawl_port.void_atomic(brawl_id, guild_id, now);
            return failure(
                "One of those pets is no longer ready. Brawl voided.",
                PET_DEAD,
            );
        };
        let state = initial_state(
            self.to_duelist(&challenger_pet, now),
            self.to_duelist(&recipient_pet, now),
        );
        ServiceResult::ok(AcceptOutcome {
            brawl,
            state,
            combat_seed: self.rng.next_u64(),
        })
    }

    pub fn decline(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<()> {
        let now = self.clock.now();
        match self
            .brawl_port
            .decline_atomic(brawl_id, guild_id, recipient_id, now)
        {
            Ok(()) => ServiceResult::ok(()),
            Err(error) => map_port_error(error),
        }
    }

    pub fn withdraw(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
    ) -> ServiceResult<()> {
        let now = self.clock.now();
        match self
            .brawl_port
            .withdraw_atomic(brawl_id, guild_id, challenger_id, now)
        {
            Ok(()) => ServiceResult::ok(()),
            Err(error) => map_port_error(error),
        }
    }

    pub fn void(&mut self, brawl_id: i64, guild_id: Option<i64>) -> ServiceResult<()> {
        let now = self.clock.now();
        match self.brawl_port.void_atomic(brawl_id, guild_id, now) {
            Ok(()) => ServiceResult::ok(()),
            Err(error) => map_port_error(error),
        }
    }

    pub fn settle(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        final_state: &PetBrawlState,
        rounds: i64,
    ) -> ServiceResult<SettleOutcome> {
        let Some(winner_marker) = final_state.winner else {
            return failure("That brawl has no winner yet.", VALIDATION_ERROR);
        };
        if winner_marker == BrawlWinner::Draw {
            let now = self.clock.now();
            let settlement = match self.brawl_port.settle_draw_atomic(
                brawl_id,
                guild_id,
                (final_state.a.pet_id, final_state.b.pet_id),
                rounds,
                now,
            ) {
                Ok(settlement) => settlement,
                Err(error) => return map_port_error(error),
            };
            let records = match self
                .brawl_port
                .get_records_for(&[final_state.a.pet_id, final_state.b.pet_id], guild_id)
            {
                Ok(records) => records,
                Err(error) => return map_port_error(error),
            };
            return ServiceResult::ok(SettleOutcome::Draw(DrawOutcome {
                duelists: (final_state.a.clone(), final_state.b.clone()),
                records,
                wager: settlement.wager,
                fee: settlement.fee,
            }));
        }
        let (winner, loser) = if winner_marker == BrawlWinner::A {
            (&final_state.a, &final_state.b)
        } else {
            (&final_state.b, &final_state.a)
        };
        let Some(winner_pet) = self.pet_port.get_pet_by_id(winner.pet_id, guild_id) else {
            return failure("One of those pets no longer exists.", NOT_FOUND);
        };
        let Some(loser_pet) = self.pet_port.get_pet_by_id(loser.pet_id, guild_id) else {
            return failure("One of those pets no longer exists.", NOT_FOUND);
        };
        let before_records = match self
            .brawl_port
            .get_records_for(&[winner.pet_id, loser.pet_id], guild_id)
        {
            Ok(records) => records,
            Err(error) => return map_port_error(error),
        };
        let winner_stat_gain = self.choose_stat_gain(&winner_pet, BRAWL_WIN_XP);
        let loser_stat_gain = self.choose_stat_gain(&loser_pet, BRAWL_LOSS_XP);
        let milestone = Self::record_milestone(
            before_records
                .get(&winner.pet_id)
                .copied()
                .unwrap_or_default(),
            before_records
                .get(&loser.pet_id)
                .copied()
                .unwrap_or_default(),
        );
        let event_key = self.personality_event_key(winner, loser, milestone.as_deref());
        let winner_gain = BRAWL_WIN_HUNGER + get_species(&winner.species_id).match_feed_bonus;
        let loser_loss = BRAWL_LOSS_HUNGER
            - if brawl_traits(&loser.species_id).loss_insurance {
                BRAWL_INSURANCE_REDUCTION
            } else {
                0
            };
        let now = self.clock.now();
        let settlement = match self.brawl_port.settle_brawl_atomic(
            brawl_id,
            guild_id,
            BrawlSettlement {
                winner_id: winner.owner_id,
                winner_pet_id: winner.pet_id,
                loser_pet_id: loser.pet_id,
                rounds,
                now,
                decay_per_day: self.pet_port.decay_per_day(),
                winner_gain,
                loser_loss,
                loss_floor: BRAWL_LOSS_FLOOR,
                daily_win_cap: BRAWL_WIN_HUNGER_DAILY_CAP,
                winner_stat_gain: winner_stat_gain.as_deref(),
                loser_stat_gain: loser_stat_gain.as_deref(),
                personality_event_key: event_key.as_deref(),
            },
        ) {
            Ok(settlement) => settlement,
            Err(error) => return map_port_error(error),
        };
        if let Some(evolution_port) = self.evolution_port.as_mut() {
            let source_key = format!("pet-brawl:{brawl_id}");
            for duelist in [winner, loser] {
                evolution_port.record_activity(
                    duelist.owner_id,
                    guild_id,
                    PetActivity::PetBrawlCompleted,
                    &source_key,
                    now,
                );
            }
        }
        let records = match self
            .brawl_port
            .get_records_for(&[winner.pet_id, loser.pet_id], guild_id)
        {
            Ok(records) => records,
            Err(error) => return map_port_error(error),
        };
        let Some(updated_winner) = self.pet_port.get_pet_by_id(winner.pet_id, guild_id) else {
            return failure("One of those pets no longer exists.", NOT_FOUND);
        };
        let Some(updated_loser) = self.pet_port.get_pet_by_id(loser.pet_id, guild_id) else {
            return failure("One of those pets no longer exists.", NOT_FOUND);
        };
        let mut career = BTreeMap::new();
        let winner_record = records.get(&winner.pet_id).copied().unwrap_or_default();
        let loser_record = records.get(&loser.pet_id).copied().unwrap_or_default();
        career.insert(
            winner.pet_id,
            Self::career_summary(&updated_winner, winner_record.0, winner_record.1),
        );
        career.insert(
            loser.pet_id,
            Self::career_summary(&updated_loser, loser_record.0, loser_record.1),
        );
        let personality_event_key = settlement.personality_event_key;
        ServiceResult::ok(SettleOutcome::Decisive(DecisiveOutcome {
            winner: winner.clone(),
            loser: loser.clone(),
            winner_delta: settlement.winner_delta,
            loser_delta: settlement.loser_delta,
            winner_xp_delta: settlement.winner_xp_delta,
            loser_xp_delta: settlement.loser_xp_delta,
            winner_stat_gain: settlement.winner_stat_gain,
            loser_stat_gain: settlement.loser_stat_gain,
            payout: settlement.payout,
            wager: settlement.wager,
            fee: settlement.fee,
            deductions: settlement.deductions,
            records,
            career,
            personality_event: Self::personality_event_text(
                personality_event_key.as_deref(),
                winner,
                loser,
            ),
            personality_event_key,
        }))
    }

    pub fn sweep_stale(&mut self) -> PortResult<SweepResult> {
        self.brawl_port
            .sweep_stale(self.clock.now(), PET_BRAWL_ACTIVE_TTL_SECONDS)
    }

    pub fn record(&mut self, pet_id: i64, guild_id: Option<i64>) -> PortResult<(i64, i64)> {
        self.brawl_port.get_pet_record(pet_id, guild_id)
    }

    pub fn records_for(
        &mut self,
        pet_ids: &[i64],
        guild_id: Option<i64>,
    ) -> PortResult<BTreeMap<i64, (i64, i64)>> {
        self.brawl_port.get_records_for(pet_ids, guild_id)
    }

    #[must_use]
    pub fn career_summary(pet: &Pet, wins: i64, losses: i64) -> CareerSummary {
        let total = wins + losses;
        let rank = if wins >= 10 {
            Some("Champion".to_owned())
        } else if total >= 10 {
            Some("Veteran".to_owned())
        } else if total >= 1 {
            Some("Rookie".to_owned())
        } else {
            None
        };
        let build = if pet.training_xp >= BRAWL_TRAINING_XP_CAP {
            let stats = [
                ("str", pet.training_str),
                ("int", pet.training_int),
                ("dex", pet.training_dex),
            ];
            let highest = stats.iter().map(|(_, value)| *value).max().unwrap_or(0);
            let leaders = stats
                .iter()
                .filter(|(_, value)| *value == highest)
                .map(|(stat, _)| *stat)
                .collect::<Vec<_>>();
            Some(if leaders.len() == 1 {
                BUILD_TITLES
                    .iter()
                    .find(|(stat, _)| *stat == leaders[0])
                    .map_or("All-Rounder", |(_, title)| *title)
                    .to_owned()
            } else {
                "All-Rounder".to_owned()
            })
        } else {
            None
        };
        CareerSummary {
            xp: pet.training_xp,
            strength: pet.training_str,
            intelligence: pet.training_int,
            dexterity: pet.training_dex,
            rank,
            build,
        }
    }

    #[must_use]
    pub fn personality_event_text(
        event_key: Option<&str>,
        winner: &Duelist,
        loser: &Duelist,
    ) -> Option<String> {
        let template = match event_key? {
            "milestone.rookie" => "🌟 A first real brawl leaves a new Rookie in the ring.",
            "milestone.veteran" => "🎖️ Another hard-fought bout earns a Veteran's composure.",
            "milestone.champion" => "🏆 Ten wins. The pet-brawl circuit has a new Champion.",
            "milestone.build" => "🌠 Fully trained—this cama's battle style has taken shape.",
            "str.victory" => {
                "💪 **{winner}** paws the ground, already asking for heavier opposition."
            }
            "int.victory" => "🧠 **{winner}** studies the replay like the result was calculated.",
            "dex.victory" => "💨 **{winner}** celebrates by dodging an imaginary rematch.",
            "balanced.victory" => {
                "✨ **{winner}** looks pleased with a thoroughly well-rounded showing."
            }
            "defiant.loss" => "🔥 **{loser}** refuses to call that a loss—only research.",
            _ => return None,
        };
        Some(
            template
                .replace("{winner}", &winner.name)
                .replace("{loser}", &loser.name),
        )
    }

    fn battle_ready_pet(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
        owner: PetOwner,
    ) -> ServiceResult<Pet> {
        let Some(pet) = self.pet_port.living_pet(discord_id, guild_id, now) else {
            return match owner {
                PetOwner::Challenger => failure(
                    "You have no pet to brawl with — adopt one with /pet adopt.",
                    NO_PET,
                ),
                PetOwner::Recipient => {
                    failure("Your opponent has no living pet to brawl with.", NO_PET)
                }
            };
        };
        if now < pet.hatched_at {
            return failure(
                format!("**{}** is still an egg. Eggs don't brawl.", pet.name),
                PET_EGG,
            );
        }
        ServiceResult::ok(pet)
    }

    fn to_duelist(&self, pet: &Pet, now: i64) -> Duelist {
        build_duelist(DuelistSnapshot {
            pet_id: pet.pet_id,
            owner_id: pet.discord_id,
            name: &pet.name,
            species_id: &pet.species,
            stage: pet.stage(now),
            hunger: i32::try_from(pet.current_hunger(now, self.pet_port.decay_per_day()))
                .expect("persisted pet hunger fits the combat domain"),
            happy: pet.mood(now, self.pet_port.decay_per_day()) == PetMood::Happy,
            aegis_used: pet.aegis_used,
            training_str: i32::try_from(pet.training_str)
                .expect("persisted training strength fits the combat domain"),
            training_int: i32::try_from(pet.training_int)
                .expect("persisted training intelligence fits the combat domain"),
            training_dex: i32::try_from(pet.training_dex)
                .expect("persisted training dexterity fits the combat domain"),
        })
    }

    fn choose_stat_gain(&mut self, pet: &Pet, award: i64) -> Option<String> {
        let old_level = pet.training_xp / BRAWL_XP_PER_LEVEL;
        let new_level = BRAWL_TRAINING_XP_CAP.min(pet.training_xp + award) / BRAWL_XP_PER_LEVEL;
        if pet.training_xp >= BRAWL_TRAINING_XP_CAP || old_level == new_level {
            return None;
        }
        let stats = [
            ("str", pet.training_str),
            ("int", pet.training_int),
            ("dex", pet.training_dex),
        ];
        if stats.iter().map(|(_, value)| *value).sum::<i64>() >= BRAWL_TRAINING_LEVEL_CAP {
            return None;
        }
        let eligible = stats
            .iter()
            .filter(|(_, value)| *value < BRAWL_TRAINING_STAT_CAP)
            .map(|(stat, _)| *stat)
            .collect::<Vec<_>>();
        (!eligible.is_empty()).then(|| eligible[self.rng.choose_index(eligible.len())].to_owned())
    }

    fn record_milestone(winner_record: (i64, i64), loser_record: (i64, i64)) -> Option<String> {
        if winner_record.0 == 9 {
            Some("milestone.champion".to_owned())
        } else if winner_record.0 + winner_record.1 == 9 || loser_record.0 + loser_record.1 == 9 {
            Some("milestone.veteran".to_owned())
        } else if winner_record.0 + winner_record.1 == 0 || loser_record.0 + loser_record.1 == 0 {
            Some("milestone.rookie".to_owned())
        } else {
            None
        }
    }

    fn personality_event_key(
        &mut self,
        winner: &Duelist,
        _loser: &Duelist,
        milestone_key: Option<&str>,
    ) -> Option<String> {
        if let Some(milestone_key) = milestone_key {
            return Some(milestone_key.to_owned());
        }
        if self.rng.random_unit() >= 0.20 {
            return None;
        }
        let stats = [
            ("str", winner.training_str),
            ("int", winner.training_int),
            ("dex", winner.training_dex),
        ];
        let highest = stats.iter().map(|(_, value)| *value).max().unwrap_or(0);
        let leaders = stats
            .iter()
            .filter(|(_, value)| *value == highest)
            .map(|(stat, _)| *stat)
            .collect::<Vec<_>>();
        let winner_key = if leaders.len() == 1 && highest > 0 {
            format!("{}.victory", leaders[0])
        } else {
            "balanced.victory".to_owned()
        };
        let choices = [winner_key, "defiant.loss".to_owned()];
        Some(choices[self.rng.choose_index(choices.len())].clone())
    }
}

#[derive(Clone, Copy)]
enum PetOwner {
    Challenger,
    Recipient,
}

fn failure<T>(message: impl Into<String>, code: &str) -> ServiceResult<T> {
    ServiceResult::fail_with_code(message, code)
}

fn map_port_error<T>(error: PortError) -> ServiceResult<T> {
    let (message, code) = match error.code.as_str() {
        "brawl_busy" => (
            "One of you is already in a pet brawl. Finish that first.",
            BRAWL_BUSY,
        ),
        "no_brawl" => ("That brawl no longer exists.", NOT_FOUND),
        "not_recipient" => ("This challenge isn't yours to answer.", VALIDATION_ERROR),
        "not_challenger" => ("Only the challenger can withdraw.", VALIDATION_ERROR),
        "expired" => ("That challenge has expired.", NOT_FOUND),
        "not_pending" => (
            "That challenge has already been answered.",
            VALIDATION_ERROR,
        ),
        "not_open" => ("That brawl is already over.", VALIDATION_ERROR),
        "not_active" => ("That brawl isn't underway.", VALIDATION_ERROR),
        "already_done" => ("That brawl was already settled.", VALIDATION_ERROR),
        "challenger_funds" => ("You cannot cover that wager and fee.", INSUFFICIENT_FUNDS),
        "recipient_funds" => ("Your opponent cannot cover that wager.", INSUFFICIENT_FUNDS),
        "invalid_wager" => (
            "Pet-brawl wagers must be whole numbers from 0 to 100 JC.",
            VALIDATION_ERROR,
        ),
        "invalid_fee" => ("That pet-brawl fee is invalid.", VALIDATION_ERROR),
        "participant_mismatch" => (
            "Those pets do not match the accepted brawl.",
            VALIDATION_ERROR,
        ),
        "challenger_pet_unavailable" => ("Your cama is no longer ready to brawl.", PET_DEAD),
        "recipient_pet_unavailable" => (
            "Your opponent's cama is no longer ready to brawl.",
            PET_DEAD,
        ),
        "no_pet" => ("You have no living pet to train.", NO_PET),
        "pet_dead" => ("Your pet has passed away.", PET_DEAD),
        "pet_egg" => ("Eggs need to hatch before they can train.", PET_EGG),
        "in_brawl" => ("Your cama has an open brawl—settle that first.", BRAWL_BUSY),
        "no_training_sessions" => (
            "No solo sessions are ready yet. Another arrives next game day.",
            COOLDOWN_ACTIVE,
        ),
        "training_complete" => (
            "Your cama has completed its permanent training.",
            VALIDATION_ERROR,
        ),
        _ => ("Something went wrong with that brawl.", VALIDATION_ERROR),
    };
    failure(message, code)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;

    use cama_domain::game_date::game_day_start_ts;
    use cama_domain::pet::{
        BRAWL_LOSS_XP, BRAWL_TRAINING_LEVEL_CAP, BRAWL_TRAINING_STAT_CAP, BRAWL_TRAINING_XP_CAP,
        BRAWL_WIN_XP, BRAWL_XP_PER_LEVEL, PetBase, SOLO_TRAINING_SESSION_CAP,
    };
    use cama_domain::pet_brawl::{BrawlWinner, PetBrawlSnapshot};

    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 7;
    const CHALLENGER: i64 = 100;
    const RECIPIENT: i64 = 200;

    #[derive(Default)]
    struct World {
        next_pet_id: i64,
        next_brawl_id: i64,
        pets: BTreeMap<i64, Pet>,
        brawls: BTreeMap<i64, PetBrawl>,
        records: BTreeMap<i64, (i64, i64)>,
        balances: BTreeMap<i64, i64>,
        refresh_training_on_accept: Option<(i64, i64, i64)>,
    }

    #[derive(Clone)]
    struct FakeClock {
        now: Rc<Cell<i64>>,
    }

    impl Clock for FakeClock {
        fn now(&self) -> i64 {
            self.now.get()
        }
    }

    struct ScriptedRng {
        seed: u64,
        choices: VecDeque<usize>,
        random_values: VecDeque<f64>,
    }

    impl Default for ScriptedRng {
        fn default() -> Self {
            Self {
                seed: 42,
                choices: VecDeque::new(),
                random_values: VecDeque::new(),
            }
        }
    }

    impl ServiceRng for ScriptedRng {
        fn next_u64(&mut self) -> u64 {
            self.seed
        }

        fn choose_index(&mut self, len: usize) -> usize {
            assert!(len > 0, "choice requires at least one value");
            self.choices.pop_front().unwrap_or(0) % len
        }

        fn random_unit(&mut self) -> f64 {
            self.random_values.pop_front().unwrap_or(0.0)
        }
    }

    #[derive(Clone)]
    struct FakePetPort {
        world: Rc<RefCell<World>>,
        decay_per_day: i64,
    }

    impl FakePetPort {
        fn available_sessions(pet: &Pet, now: i64) -> i64 {
            let Some(recharged_at) = pet.solo_training_recharged_at else {
                return SOLO_TRAINING_SESSION_CAP.min(pet.solo_training_sessions);
            };
            let today = game_day_start_ts(now).expect("test timestamp is valid");
            let previous =
                game_day_start_ts(recharged_at).expect("test recharge timestamp is valid");
            let elapsed_days = ((today - previous) / DAY).max(0);
            SOLO_TRAINING_SESSION_CAP.min(pet.solo_training_sessions + elapsed_days)
        }

        fn is_living(&self, pet: &Pet, now: i64) -> bool {
            pet.died_at.is_none() && pet.current_hunger(now, self.decay_per_day) > 0
        }
    }

    impl PetPort for FakePetPort {
        fn decay_per_day(&self) -> i64 {
            self.decay_per_day
        }

        fn living_pet(&mut self, discord_id: i64, guild_id: Option<i64>, now: i64) -> Option<Pet> {
            let guild_id = guild_id.unwrap_or(0);
            self.world
                .borrow()
                .pets
                .values()
                .find(|pet| {
                    pet.discord_id == discord_id
                        && pet.guild_id == guild_id
                        && self.is_living(pet, now)
                })
                .cloned()
        }

        fn living_pet_by_id(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            now: i64,
        ) -> Option<Pet> {
            let guild_id = guild_id.unwrap_or(0);
            self.world
                .borrow()
                .pets
                .get(&pet_id)
                .filter(|pet| pet.guild_id == guild_id && self.is_living(pet, now))
                .cloned()
        }

        fn get_pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Option<Pet> {
            let guild_id = guild_id.unwrap_or(0);
            self.world
                .borrow()
                .pets
                .get(&pet_id)
                .filter(|pet| pet.guild_id == guild_id)
                .cloned()
        }

        fn available_solo_training_sessions(&self, pet: &Pet, now: i64) -> i64 {
            Self::available_sessions(pet, now)
        }

        fn train_solo_atomic(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            now: i64,
            proposed_stat: Option<&str>,
        ) -> PortResult<SoloTrainingPortOutcome> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(snapshot) = world.pets.get(&pet_id).cloned() else {
                return Err(PortError::new("no_pet"));
            };
            if snapshot.guild_id != guild_id {
                return Err(PortError::new("no_pet"));
            }
            if snapshot.died_at.is_some() {
                return Err(PortError::new("pet_dead"));
            }
            if now < snapshot.hatched_at {
                return Err(PortError::new("pet_egg"));
            }
            if snapshot.training_xp >= BRAWL_TRAINING_XP_CAP {
                return Err(PortError::new("training_complete"));
            }
            let in_active_brawl = world.brawls.values().any(|brawl| {
                brawl.guild_id == guild_id
                    && brawl.status == "active"
                    && (brawl.challenger_id == snapshot.discord_id
                        || brawl.recipient_id == snapshot.discord_id)
            });
            if in_active_brawl {
                return Err(PortError::new("in_brawl"));
            }
            let available = Self::available_sessions(&snapshot, now);
            if available <= 0 {
                return Err(PortError::new("no_training_sessions"));
            }
            let sessions_used = available.min(BRAWL_TRAINING_XP_CAP - snapshot.training_xp);
            let xp_delta = sessions_used;
            let new_xp = snapshot.training_xp + xp_delta;
            let mut stats = [
                ("str", snapshot.training_str),
                ("int", snapshot.training_int),
                ("dex", snapshot.training_dex),
            ];
            let crossed_level =
                new_xp / BRAWL_XP_PER_LEVEL > snapshot.training_xp / BRAWL_XP_PER_LEVEL;
            let stat_gain = if crossed_level
                && stats.iter().map(|(_, value)| *value).sum::<i64>() < BRAWL_TRAINING_LEVEL_CAP
            {
                let eligible = stats
                    .iter()
                    .filter(|(_, value)| *value < BRAWL_TRAINING_STAT_CAP)
                    .map(|(stat, _)| *stat)
                    .collect::<Vec<_>>();
                eligible
                    .iter()
                    .find(|stat| Some(**stat) == proposed_stat)
                    .copied()
                    .or_else(|| eligible.first().copied())
            } else {
                None
            };
            if let Some(stat_gain) = stat_gain {
                stats
                    .iter_mut()
                    .find(|(stat, _)| *stat == stat_gain)
                    .expect("selected stat exists")
                    .1 += 1;
            }
            let sessions_available = available - sessions_used;
            let pet = world.pets.get_mut(&pet_id).expect("pet still exists");
            pet.training_xp = new_xp;
            pet.training_str = stats[0].1;
            pet.training_int = stats[1].1;
            pet.training_dex = stats[2].1;
            pet.solo_training_sessions = sessions_available;
            pet.solo_training_recharged_at = Some(now);
            Ok(SoloTrainingPortOutcome {
                sessions_used,
                sessions_available,
                xp_delta,
                stat_gain: stat_gain.map(str::to_owned),
            })
        }
    }

    #[derive(Clone)]
    struct FakeBrawlPort {
        world: Rc<RefCell<World>>,
    }

    #[allow(clippy::too_many_arguments)]
    impl PetBrawlPort for FakeBrawlPort {
        fn get_brawl(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
        ) -> PortResult<Option<PetBrawl>> {
            let guild_id = guild_id.unwrap_or(0);
            Ok(self
                .world
                .borrow()
                .brawls
                .get(&brawl_id)
                .filter(|brawl| brawl.guild_id == guild_id)
                .cloned())
        }

        fn create_brawl_atomic(
            &mut self,
            guild_id: Option<i64>,
            channel_id: i64,
            challenger_id: i64,
            recipient_id: i64,
            challenger_pet_id: i64,
            now: i64,
            expires_at: i64,
            wager: i64,
            fee: i64,
        ) -> PortResult<PetBrawl> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            if world.brawls.values().any(|brawl| {
                brawl.guild_id == guild_id
                    && (brawl.status == "active"
                        || (brawl.status == "pending" && brawl.expires_at > now))
                    && [challenger_id, recipient_id].iter().any(|player| {
                        *player == brawl.challenger_id || *player == brawl.recipient_id
                    })
            }) {
                return Err(PortError::new("brawl_busy"));
            }
            if wager > 0 {
                let balance = world.balances.entry(challenger_id).or_default();
                if *balance < wager + fee {
                    return Err(PortError::new("challenger_funds"));
                }
                *balance -= wager + fee;
            }
            world.next_brawl_id += 1;
            let mut brawl: PetBrawl = PetBrawlSnapshot {
                brawl_id: world.next_brawl_id,
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                challenger_pet_id,
                recipient_pet_id: None,
                status: "pending",
                created_at: now,
                expires_at,
                resolved_at: None,
                winner_id: None,
                winner_pet_id: None,
                loser_pet_id: None,
                rounds: None,
                winner_hunger_delta: 0,
                loser_hunger_delta: 0,
            }
            .into();
            brawl.wager = wager;
            brawl.fee = fee;
            world.brawls.insert(brawl.brawl_id, brawl.clone());
            Ok(brawl)
        }

        fn accept_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            recipient_id: i64,
            recipient_pet_id: i64,
            now: i64,
        ) -> PortResult<PetBrawl> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(snapshot) = world.brawls.get(&brawl_id).cloned() else {
                return Err(PortError::new("no_brawl"));
            };
            if snapshot.guild_id != guild_id {
                return Err(PortError::new("no_brawl"));
            }
            if snapshot.recipient_id != recipient_id {
                return Err(PortError::new("not_recipient"));
            }
            if snapshot.status == "pending" && snapshot.expires_at <= now {
                return Err(PortError::new("expired"));
            }
            if snapshot.status != "pending" {
                return Err(PortError::new("not_pending"));
            }
            if snapshot.wager > 0 {
                let balance = world.balances.entry(recipient_id).or_default();
                if *balance < snapshot.wager {
                    return Err(PortError::new("recipient_funds"));
                }
                *balance -= snapshot.wager;
            }
            if let Some((pet_id, training_xp, training_int)) =
                world.refresh_training_on_accept.take()
            {
                let pet = world.pets.get_mut(&pet_id).expect("refresh pet exists");
                pet.training_xp = training_xp;
                pet.training_int = training_int;
            }
            let brawl = world
                .brawls
                .get_mut(&brawl_id)
                .expect("validated brawl exists");
            brawl.status = "active".to_owned();
            brawl.recipient_pet_id = Some(recipient_pet_id);
            Ok(brawl.clone())
        }

        fn decline_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            recipient_id: i64,
            now: i64,
        ) -> PortResult<()> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(brawl) = world.brawls.get_mut(&brawl_id) else {
                return Err(PortError::new("no_brawl"));
            };
            if brawl.guild_id != guild_id {
                return Err(PortError::new("no_brawl"));
            }
            if brawl.recipient_id != recipient_id {
                return Err(PortError::new("not_recipient"));
            }
            if brawl.status == "pending" && brawl.expires_at <= now {
                return Err(PortError::new("expired"));
            }
            if brawl.status != "pending" {
                return Err(PortError::new("not_pending"));
            }
            brawl.status = "declined".to_owned();
            brawl.resolved_at = Some(now);
            Ok(())
        }

        fn withdraw_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            challenger_id: i64,
            now: i64,
        ) -> PortResult<()> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(brawl) = world.brawls.get_mut(&brawl_id) else {
                return Err(PortError::new("no_brawl"));
            };
            if brawl.guild_id != guild_id {
                return Err(PortError::new("no_brawl"));
            }
            if brawl.challenger_id != challenger_id {
                return Err(PortError::new("not_challenger"));
            }
            if brawl.status != "pending" {
                return Err(PortError::new("not_pending"));
            }
            brawl.status = "void".to_owned();
            brawl.resolved_at = Some(now);
            Ok(())
        }

        fn void_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            now: i64,
        ) -> PortResult<()> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(brawl) = world.brawls.get_mut(&brawl_id) else {
                return Err(PortError::new("no_brawl"));
            };
            if brawl.guild_id != guild_id {
                return Err(PortError::new("no_brawl"));
            }
            if brawl.status != "pending" && brawl.status != "active" {
                return Err(PortError::new("not_open"));
            }
            brawl.status = "void".to_owned();
            brawl.resolved_at = Some(now);
            Ok(())
        }

        fn settle_draw_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            participant_pet_ids: (i64, i64),
            rounds: i64,
            now: i64,
        ) -> PortResult<DrawSettlementResult> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(snapshot) = world.brawls.get(&brawl_id).cloned() else {
                return Err(PortError::new("no_brawl"));
            };
            if snapshot.guild_id != guild_id || snapshot.status != "active" {
                return Err(PortError::new("not_active"));
            }
            let persisted = (snapshot.challenger_pet_id, snapshot.recipient_pet_id);
            let matches = persisted.1.is_some_and(|recipient_pet_id| {
                [participant_pet_ids.0, participant_pet_ids.1] == [persisted.0, recipient_pet_id]
                    || [participant_pet_ids.1, participant_pet_ids.0]
                        == [persisted.0, recipient_pet_id]
            });
            if !matches {
                return Err(PortError::new("participant_mismatch"));
            }
            let brawl = world
                .brawls
                .get_mut(&brawl_id)
                .expect("validated brawl exists");
            brawl.status = "done".to_owned();
            brawl.resolved_at = Some(now);
            brawl.rounds = Some(rounds);
            brawl.winner_id = None;
            brawl.winner_pet_id = None;
            brawl.loser_pet_id = None;
            Ok(DrawSettlementResult {
                wager: snapshot.wager,
                fee: snapshot.fee,
            })
        }

        fn settle_brawl_atomic(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            settlement: BrawlSettlement<'_>,
        ) -> PortResult<BrawlSettlementResult> {
            let guild_id = guild_id.unwrap_or(0);
            let mut world = self.world.borrow_mut();
            let Some(snapshot) = world.brawls.get(&brawl_id).cloned() else {
                return Err(PortError::new("no_brawl"));
            };
            if snapshot.guild_id != guild_id || snapshot.status != "active" {
                return Err(PortError::new("not_active"));
            }
            let Some(recipient_pet_id) = snapshot.recipient_pet_id else {
                return Err(PortError::new("participant_mismatch"));
            };
            let winner_pair_valid = (settlement.winner_id == snapshot.challenger_id
                && settlement.winner_pet_id == snapshot.challenger_pet_id)
                || (settlement.winner_id == snapshot.recipient_id
                    && settlement.winner_pet_id == recipient_pet_id);
            let pet_pair_valid = [settlement.winner_pet_id, settlement.loser_pet_id]
                == [snapshot.challenger_pet_id, recipient_pet_id]
                || [settlement.loser_pet_id, settlement.winner_pet_id]
                    == [snapshot.challenger_pet_id, recipient_pet_id];
            if !winner_pair_valid || !pet_pair_valid {
                return Err(PortError::new("participant_mismatch"));
            }

            let winner_delta = apply_winner_hunger(
                &mut world,
                settlement.winner_pet_id,
                settlement.now,
                settlement.decay_per_day,
                settlement.winner_gain,
            );
            let loser_delta = apply_loser_hunger(
                &mut world,
                settlement.loser_pet_id,
                settlement.now,
                settlement.decay_per_day,
                settlement.loser_loss,
                settlement.loss_floor,
            );
            let winner_training = apply_training(
                &mut world,
                settlement.winner_pet_id,
                BRAWL_WIN_XP,
                settlement.winner_stat_gain,
            );
            let loser_training = apply_training(
                &mut world,
                settlement.loser_pet_id,
                BRAWL_LOSS_XP,
                settlement.loser_stat_gain,
            );
            let mut personality_event_key = settlement.personality_event_key.map(str::to_owned);
            if (winner_training.2 || loser_training.2)
                && personality_event_key.as_deref() != Some("milestone.champion")
            {
                personality_event_key = Some("milestone.build".to_owned());
            }
            world.records.entry(settlement.winner_pet_id).or_default().0 += 1;
            world.records.entry(settlement.loser_pet_id).or_default().1 += 1;
            let payout = snapshot.wager * 2;
            *world.balances.entry(settlement.winner_id).or_default() += payout;
            let (
                challenger_xp_delta,
                recipient_xp_delta,
                challenger_stat_gain,
                recipient_stat_gain,
            ) = if settlement.winner_pet_id == snapshot.challenger_pet_id {
                (
                    winner_training.0,
                    loser_training.0,
                    winner_training.1.clone(),
                    loser_training.1.clone(),
                )
            } else {
                (
                    loser_training.0,
                    winner_training.0,
                    loser_training.1.clone(),
                    winner_training.1.clone(),
                )
            };
            let brawl = world
                .brawls
                .get_mut(&brawl_id)
                .expect("validated brawl exists");
            brawl.status = "done".to_owned();
            brawl.resolved_at = Some(settlement.now);
            brawl.winner_id = Some(settlement.winner_id);
            brawl.winner_pet_id = Some(settlement.winner_pet_id);
            brawl.loser_pet_id = Some(settlement.loser_pet_id);
            brawl.rounds = Some(settlement.rounds);
            brawl.winner_hunger_delta = winner_delta;
            brawl.loser_hunger_delta = loser_delta;
            brawl.challenger_xp_delta = challenger_xp_delta;
            brawl.recipient_xp_delta = recipient_xp_delta;
            brawl.challenger_stat_gain = challenger_stat_gain;
            brawl.recipient_stat_gain = recipient_stat_gain;
            brawl.personality_event_key = personality_event_key.clone();
            Ok(BrawlSettlementResult {
                winner_delta,
                loser_delta,
                winner_xp_delta: winner_training.0,
                loser_xp_delta: loser_training.0,
                winner_stat_gain: winner_training.1,
                loser_stat_gain: loser_training.1,
                payout,
                wager: snapshot.wager,
                fee: snapshot.fee,
                deductions: ProfitDeductions {
                    profit: snapshot.wager,
                    ..ProfitDeductions::default()
                },
                personality_event_key,
            })
        }

        fn get_pet_record(
            &mut self,
            pet_id: i64,
            _guild_id: Option<i64>,
        ) -> PortResult<(i64, i64)> {
            Ok(self
                .world
                .borrow()
                .records
                .get(&pet_id)
                .copied()
                .unwrap_or_default())
        }

        fn get_records_for(
            &mut self,
            pet_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> PortResult<BTreeMap<i64, (i64, i64)>> {
            let world = self.world.borrow();
            Ok(pet_ids
                .iter()
                .copied()
                .map(|pet_id| {
                    (
                        pet_id,
                        world.records.get(&pet_id).copied().unwrap_or_default(),
                    )
                })
                .collect())
        }

        fn sweep_stale(&mut self, now: i64, active_ttl_seconds: i64) -> PortResult<SweepResult> {
            let mut result = SweepResult {
                expired: 0,
                voided: 0,
            };
            for brawl in self.world.borrow_mut().brawls.values_mut() {
                if brawl.status == "pending" && brawl.expires_at <= now {
                    brawl.status = "expired".to_owned();
                    brawl.resolved_at = Some(now);
                    result.expired += 1;
                } else if brawl.status == "active" && brawl.created_at + active_ttl_seconds <= now {
                    brawl.status = "void".to_owned();
                    brawl.resolved_at = Some(now);
                    result.voided += 1;
                }
            }
            Ok(result)
        }
    }

    fn apply_winner_hunger(
        world: &mut World,
        pet_id: i64,
        now: i64,
        decay_per_day: i64,
        gain: i64,
    ) -> i64 {
        let pet = world.pets.get_mut(&pet_id).expect("winner pet exists");
        let hunger = pet.current_hunger(now, decay_per_day);
        if pet.died_at.is_some() || hunger <= 0 || gain <= 0 {
            return 0;
        }
        let new_hunger = (hunger + gain).min(100);
        pet.last_fed_at = now;
        pet.hunger_at_last_fed = new_hunger;
        new_hunger - hunger
    }

    fn apply_loser_hunger(
        world: &mut World,
        pet_id: i64,
        now: i64,
        decay_per_day: i64,
        loss: i64,
        floor: i64,
    ) -> i64 {
        let pet = world.pets.get_mut(&pet_id).expect("loser pet exists");
        let hunger = pet.current_hunger(now, decay_per_day);
        if pet.died_at.is_some() || hunger <= 0 {
            return 0;
        }
        let applied = loss.min((hunger - floor).max(0));
        if applied > 0 {
            pet.last_fed_at = now;
            pet.hunger_at_last_fed = hunger - applied;
        }
        -applied
    }

    fn apply_training(
        world: &mut World,
        pet_id: i64,
        award: i64,
        proposed_stat: Option<&str>,
    ) -> (i64, Option<String>, bool) {
        let pet = world.pets.get_mut(&pet_id).expect("training pet exists");
        if award <= 0 || pet.training_xp >= BRAWL_TRAINING_XP_CAP {
            return (0, None, false);
        }
        let old_xp = pet.training_xp;
        let new_xp = (old_xp + award).min(BRAWL_TRAINING_XP_CAP);
        let mut stats = [
            ("str", pet.training_str),
            ("int", pet.training_int),
            ("dex", pet.training_dex),
        ];
        let stat_gain = if new_xp / BRAWL_XP_PER_LEVEL > old_xp / BRAWL_XP_PER_LEVEL
            && stats.iter().map(|(_, value)| *value).sum::<i64>() < BRAWL_TRAINING_LEVEL_CAP
        {
            let eligible = stats
                .iter()
                .filter(|(_, value)| *value < BRAWL_TRAINING_STAT_CAP)
                .map(|(stat, _)| *stat)
                .collect::<Vec<_>>();
            eligible
                .iter()
                .find(|stat| Some(**stat) == proposed_stat)
                .copied()
                .or_else(|| eligible.first().copied())
        } else {
            None
        };
        if let Some(stat_gain) = stat_gain {
            stats
                .iter_mut()
                .find(|(stat, _)| *stat == stat_gain)
                .expect("selected stat exists")
                .1 += 1;
        }
        pet.training_xp = new_xp;
        pet.training_str = stats[0].1;
        pet.training_int = stats[1].1;
        pet.training_dex = stats[2].1;
        (
            new_xp - old_xp,
            stat_gain.map(str::to_owned),
            new_xp == BRAWL_TRAINING_XP_CAP,
        )
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct EvolutionCall {
        discord_id: i64,
        guild_id: Option<i64>,
        activity: PetActivity,
        source_key: String,
        occurred_at: i64,
    }

    #[derive(Clone, Default)]
    struct EvolutionSpy {
        calls: Rc<RefCell<Vec<EvolutionCall>>>,
    }

    impl PetEvolutionPort for EvolutionSpy {
        fn record_activity(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            activity: PetActivity,
            source_key: &str,
            occurred_at: i64,
        ) {
            self.calls.borrow_mut().push(EvolutionCall {
                discord_id,
                guild_id,
                activity,
                source_key: source_key.to_owned(),
                occurred_at,
            });
        }
    }

    type TestService =
        PetBrawlService<FakePetPort, FakeBrawlPort, FakeClock, ScriptedRng, EvolutionSpy>;

    struct Fixture {
        service: TestService,
        world: Rc<RefCell<World>>,
        now: Rc<Cell<i64>>,
        evolution_calls: Rc<RefCell<Vec<EvolutionCall>>>,
    }

    impl Fixture {
        fn new() -> Self {
            let world = Rc::new(RefCell::new(World::default()));
            let now = Rc::new(Cell::new(T0));
            let evolution = EvolutionSpy::default();
            let evolution_calls = Rc::clone(&evolution.calls);
            let service = PetBrawlService::new(
                FakePetPort {
                    world: Rc::clone(&world),
                    decay_per_day: 20,
                },
                FakeBrawlPort {
                    world: Rc::clone(&world),
                },
                FakeClock {
                    now: Rc::clone(&now),
                },
                ScriptedRng::default(),
                Some(evolution),
                0.01,
            );
            Self {
                service,
                world,
                now,
                evolution_calls,
            }
        }

        fn brawl(&self, brawl_id: i64) -> PetBrawl {
            self.world
                .borrow()
                .brawls
                .get(&brawl_id)
                .expect("brawl exists")
                .clone()
        }

        fn pet(&self, pet_id: i64) -> Pet {
            self.world
                .borrow()
                .pets
                .get(&pet_id)
                .expect("pet exists")
                .clone()
        }
    }

    struct PetSpec {
        species: &'static str,
        hunger: i64,
        last_fed_at: i64,
        hatched_at: i64,
        died_at: Option<i64>,
        training_xp: i64,
        training_str: i64,
        training_int: i64,
        training_dex: i64,
        solo_training_sessions: i64,
        solo_training_recharged_at: Option<i64>,
    }

    impl Default for PetSpec {
        fn default() -> Self {
            Self {
                species: "common_cama",
                hunger: 100,
                last_fed_at: T0,
                hatched_at: T0 - 9 * DAY,
                died_at: None,
                training_xp: 0,
                training_str: 0,
                training_int: 0,
                training_dex: 0,
                solo_training_sessions: 3,
                solo_training_recharged_at: None,
            }
        }
    }

    fn insert_pet(fixture: &Fixture, discord_id: i64, spec: PetSpec) -> i64 {
        let mut world = fixture.world.borrow_mut();
        world.next_pet_id += 1;
        let pet_id = world.next_pet_id;
        let mut pet = Pet::new(PetBase {
            pet_id,
            discord_id,
            guild_id: GUILD,
            name: "Brawler".to_owned(),
            species: spec.species.to_owned(),
            adopted_at: spec.hatched_at - DAY,
            hatched_at: spec.hatched_at,
            adopt_fee: 20,
            last_fed_at: spec.last_fed_at,
            hunger_at_last_fed: spec.hunger,
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
            dig_work_at: T0,
            aegis_used: 0,
            hatch_announced_at: None,
            died_at: spec.died_at,
            death_cause: None,
            death_announced_at: None,
        });
        pet.training_xp = spec.training_xp;
        pet.training_str = spec.training_str;
        pet.training_int = spec.training_int;
        pet.training_dex = spec.training_dex;
        pet.solo_training_sessions = spec.solo_training_sessions;
        pet.solo_training_recharged_at = spec.solo_training_recharged_at;
        world.pets.insert(pet_id, pet);
        pet_id
    }

    fn challenge(fixture: &mut Fixture) -> ChallengeOutcome {
        fixture
            .service
            .challenge(
                CHALLENGER,
                RECIPIENT,
                Some(GUILD),
                555,
                WagerInput::Integer(0),
            )
            .unwrap()
    }

    fn start_battle(fixture: &mut Fixture, challenger_spec: PetSpec) -> (AcceptOutcome, i64, i64) {
        let pet_a = insert_pet(fixture, CHALLENGER, challenger_spec);
        let pet_b = insert_pet(fixture, RECIPIENT, PetSpec::default());
        let challenge = challenge(fixture);
        let accepted = fixture
            .service
            .accept(challenge.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        (accepted, pet_a, pet_b)
    }

    fn force_a_wins(state: &PetBrawlState) -> PetBrawlState {
        let mut forced = state.clone();
        forced.b.hp = 0;
        forced.round_no = 3;
        forced.winner = Some(BrawlWinner::A);
        forced
    }

    #[test]
    fn test_first_training_spends_three_banked_sessions() {
        let mut fixture = Fixture::new();
        let pet_id = insert_pet(&fixture, CHALLENGER, PetSpec::default());

        let result = fixture.service.train_solo(CHALLENGER, Some(GUILD));

        assert!(result.success(), "{:?}", result.error());
        let value = result.unwrap();
        assert_eq!(value.sessions_used, 3);
        assert_eq!(value.xp_delta, 3);
        assert_eq!(value.stat_gain, None);
        assert_eq!(value.sessions_available, 0);
        let pet = fixture.pet(pet_id);
        assert_eq!(pet.training_xp, 3);
        assert_eq!(pet.solo_training_sessions, 0);
        assert_eq!(pet.solo_training_recharged_at, Some(T0));
        assert_eq!(fixture.service.record(pet_id, Some(GUILD)), Ok((0, 0)));
    }

    #[test]
    fn test_training_recharges_one_session_per_game_day() {
        let mut fixture = Fixture::new();
        let pet_id = insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                solo_training_sessions: 0,
                solo_training_recharged_at: Some(T0),
                ..PetSpec::default()
            },
        );
        fixture.now.set(T0 + 2 * DAY);

        let value = fixture.service.train_solo(CHALLENGER, Some(GUILD)).unwrap();

        assert_eq!(value.sessions_used, 2);
        assert_eq!(value.xp_delta, 2);
        let pet = fixture.pet(pet_id);
        assert_eq!(pet.solo_training_sessions, 0);
        assert_eq!(pet.solo_training_recharged_at, Some(T0 + 2 * DAY));
    }

    #[test]
    fn test_training_threshold_awards_a_random_eligible_stat() {
        let mut fixture = Fixture::new();
        let pet_id = insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 4,
                ..PetSpec::default()
            },
        );

        let value = fixture.service.train_solo(CHALLENGER, Some(GUILD)).unwrap();

        assert_eq!(value.xp_delta, 3);
        assert_eq!(value.stat_gain.as_deref(), Some("str"));
        let pet = fixture.pet(pet_id);
        assert_eq!((pet.training_xp, pet.training_str), (7, 1));
    }

    #[test]
    fn test_training_near_cap_preserves_unused_sessions() {
        let mut fixture = Fixture::new();
        let pet_id = insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 19,
                training_str: 1,
                training_int: 1,
                training_dex: 1,
                ..PetSpec::default()
            },
        );

        let value = fixture.service.train_solo(CHALLENGER, Some(GUILD)).unwrap();

        assert_eq!(value.sessions_used, 1);
        assert_eq!(value.sessions_available, 2);
        assert_eq!(value.stat_gain.as_deref(), Some("str"));
        let pet = fixture.pet(pet_id);
        assert_eq!(pet.training_xp, 20);
        assert_eq!(pet.solo_training_sessions, 2);
    }

    #[test]
    fn test_training_without_a_session_is_a_cooldown() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                solo_training_sessions: 0,
                solo_training_recharged_at: Some(T0),
                ..PetSpec::default()
            },
        );

        let result = fixture.service.train_solo(CHALLENGER, Some(GUILD));

        assert_eq!(result.error_code(), Some(COOLDOWN_ACTIVE));
    }

    #[test]
    fn test_training_rejects_eggs() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                hatched_at: T0 + DAY,
                last_fed_at: T0 + DAY,
                ..PetSpec::default()
            },
        );

        let result = fixture.service.train_solo(CHALLENGER, Some(GUILD));

        assert_eq!(result.error_code(), Some(PET_EGG));
    }

    #[test]
    fn test_training_rejects_an_active_brawl() {
        let mut fixture = Fixture::new();
        let _started = start_battle(&mut fixture, PetSpec::default());

        let result = fixture.service.train_solo(CHALLENGER, Some(GUILD));

        assert_eq!(result.error_code(), Some(BRAWL_BUSY));
    }

    #[test]
    fn test_training_is_available_during_a_pending_challenge() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let pending = challenge(&mut fixture);
        assert_eq!(pending.brawl.status, "pending");

        let result = fixture.service.train_solo(RECIPIENT, Some(GUILD));

        assert_eq!(result.unwrap().xp_delta, 3);
    }

    #[test]
    fn test_rejects_self() {
        let mut fixture = Fixture::new();
        let result = fixture.service.challenge(
            CHALLENGER,
            CHALLENGER,
            Some(GUILD),
            555,
            WagerInput::Integer(0),
        );
        assert_eq!(result.error_code(), Some(VALIDATION_ERROR));
    }

    #[test]
    fn test_rejects_petless_challenger() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let result = fixture.service.challenge(
            CHALLENGER,
            RECIPIENT,
            Some(GUILD),
            555,
            WagerInput::Integer(0),
        );
        assert_eq!(result.error_code(), Some(NO_PET));
    }

    #[test]
    fn test_rejects_petless_recipient() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        let result = fixture.service.challenge(
            CHALLENGER,
            RECIPIENT,
            Some(GUILD),
            555,
            WagerInput::Integer(0),
        );
        assert_eq!(result.error_code(), Some(NO_PET));
        assert!(
            result
                .error()
                .is_some_and(|error| error.contains("opponent"))
        );
    }

    #[test]
    fn test_rejects_egg() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                hatched_at: T0 + DAY,
                last_fed_at: T0 + DAY,
                ..PetSpec::default()
            },
        );
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let result = fixture.service.challenge(
            CHALLENGER,
            RECIPIENT,
            Some(GUILD),
            555,
            WagerInput::Integer(0),
        );
        assert_eq!(result.error_code(), Some(PET_EGG));
    }

    #[test]
    fn test_rejects_busy_player() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        insert_pet(&fixture, 300, PetSpec::default());
        assert!(challenge(&mut fixture).brawl.status == "pending");
        let result =
            fixture
                .service
                .challenge(300, CHALLENGER, Some(GUILD), 555, WagerInput::Integer(0));
        assert_eq!(result.error_code(), Some(BRAWL_BUSY));
    }

    #[test]
    fn test_starved_challenger_pet_resolves_to_no_pet() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                last_fed_at: T0 - 30 * DAY,
                hatched_at: T0 - 40 * DAY,
                ..PetSpec::default()
            },
        );
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let result = fixture.service.challenge(
            CHALLENGER,
            RECIPIENT,
            Some(GUILD),
            555,
            WagerInput::Integer(0),
        );
        assert_eq!(result.error_code(), Some(NO_PET));
    }

    #[test]
    fn test_optional_wager_uses_tip_fee_and_escrows_challenger() {
        let mut fixture = Fixture::new();
        fixture.world.borrow_mut().balances.insert(CHALLENGER, 100);
        fixture.world.borrow_mut().balances.insert(RECIPIENT, 100);
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());

        let value = fixture
            .service
            .challenge(
                CHALLENGER,
                RECIPIENT,
                Some(GUILD),
                555,
                WagerInput::Integer(20),
            )
            .unwrap();

        assert_eq!((value.brawl.wager, value.brawl.fee), (20, 1));
        assert_eq!(fixture.world.borrow().balances[&CHALLENGER], 79);
    }

    #[test]
    fn test_invalid_wager_is_rejected_negative_1() {
        assert_invalid_wager(WagerInput::Integer(-1));
    }

    #[test]
    fn test_invalid_wager_is_rejected_101() {
        assert_invalid_wager(WagerInput::Integer(101));
    }

    #[test]
    fn test_invalid_wager_is_rejected_1_5() {
        assert_invalid_wager(WagerInput::Float(1.5));
    }

    #[test]
    fn test_invalid_wager_is_rejected_true() {
        assert_invalid_wager(WagerInput::Boolean(true));
    }

    fn assert_invalid_wager(wager: WagerInput) {
        let mut fixture = Fixture::new();
        let result = fixture
            .service
            .challenge(CHALLENGER, RECIPIENT, Some(GUILD), 555, wager);
        assert_eq!(result.error_code(), Some(VALIDATION_ERROR));
    }

    #[test]
    fn test_accept_builds_initial_state() {
        let mut fixture = Fixture::new();
        let (accepted, pet_a, pet_b) = start_battle(&mut fixture, PetSpec::default());
        assert_eq!(accepted.state.a.pet_id, pet_a);
        assert_eq!(accepted.state.b.pet_id, pet_b);
        assert_eq!(accepted.state.a.owner_id, CHALLENGER);
        assert_eq!(accepted.state.round_no, 0);
        assert_eq!(accepted.brawl.status, "active");
        assert_eq!(accepted.combat_seed, 42);
    }

    #[test]
    fn test_snapshot_reflects_hunger_and_stage() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                hunger: 60,
                hatched_at: T0 - 2 * DAY,
                ..PetSpec::default()
            },
        );
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let pending = challenge(&mut fixture);
        let accepted = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        assert!(!accepted.state.a.is_adult);
        assert_eq!(accepted.state.a.max_hp, 80);
        assert_eq!(accepted.state.a.hp, 80 - (100 - 60) / 5);
    }

    #[test]
    fn test_snapshot_includes_permanent_training_stats() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 15,
                training_str: 2,
                training_int: 1,
                ..PetSpec::default()
            },
        );
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let pending = challenge(&mut fixture);
        let accepted = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        assert_eq!(
            (
                accepted.state.a.training_str,
                accepted.state.a.training_int,
                accepted.state.a.training_dex,
            ),
            (2, 1, 0)
        );
    }

    #[test]
    fn test_accept_refreshes_training_changed_after_initial_validation() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        let recipient_pet_id = insert_pet(
            &fixture,
            RECIPIENT,
            PetSpec {
                training_xp: 4,
                ..PetSpec::default()
            },
        );
        let pending = challenge(&mut fixture);
        fixture.world.borrow_mut().refresh_training_on_accept = Some((recipient_pet_id, 5, 1));

        let accepted = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();

        assert_eq!(accepted.state.b.training_int, 1);
    }

    #[test]
    fn test_accept_voids_when_challenger_pet_starved() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        fixture.now.set(T0 + 6 * DAY);

        let result = fixture.service.accept(brawl_id, Some(GUILD), RECIPIENT);

        assert_eq!(result.error_code(), Some(PET_DEAD));
        assert_eq!(fixture.brawl(brawl_id).status, "void");
    }

    #[test]
    fn test_wrong_recipient_rejected() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        let petless = fixture.service.accept(brawl_id, Some(GUILD), 999);
        assert_eq!(petless.error_code(), Some(NO_PET));
        insert_pet(&fixture, 999, PetSpec::default());
        let wrong = fixture.service.accept(brawl_id, Some(GUILD), 999);
        assert_eq!(wrong.error_code(), Some(VALIDATION_ERROR));
    }

    #[test]
    fn test_decline() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        assert!(
            fixture
                .service
                .decline(brawl_id, Some(GUILD), RECIPIENT)
                .success()
        );
        assert_eq!(fixture.brawl(brawl_id).status, "declined");
    }

    #[test]
    fn test_withdraw_challenger_only() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        assert!(
            !fixture
                .service
                .withdraw(brawl_id, Some(GUILD), RECIPIENT)
                .success()
        );
        assert!(
            fixture
                .service
                .withdraw(brawl_id, Some(GUILD), CHALLENGER)
                .success()
        );
        assert_eq!(fixture.brawl(brawl_id).status, "void");
    }

    #[test]
    fn test_withdraw_racing_accept_cannot_void_active_brawl() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        assert!(
            fixture
                .service
                .accept(brawl_id, Some(GUILD), RECIPIENT)
                .success()
        );

        let stale_withdraw = fixture.service.withdraw(brawl_id, Some(GUILD), CHALLENGER);

        assert!(!stale_withdraw.success());
        assert_eq!(fixture.brawl(brawl_id).status, "active");
    }

    #[test]
    fn test_settle_applies_hunger_and_records() {
        let mut fixture = Fixture::new();
        let (battle, pet_a, pet_b) = start_battle(
            &mut fixture,
            PetSpec {
                hunger: 50,
                ..PetSpec::default()
            },
        );
        let forced = force_a_wins(&battle.state);

        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &forced, 3)
            .unwrap();

        let SettleOutcome::Decisive(value) = outcome else {
            panic!("forced win must be decisive");
        };
        assert_eq!((value.winner.pet_id, value.loser.pet_id), (pet_a, pet_b));
        assert_eq!(value.records[&pet_a], (1, 0));
        assert_eq!(value.records[&pet_b], (0, 1));
        assert_eq!(value.winner_delta, 10);
        assert_eq!(value.loser_delta, -15);
    }

    #[test]
    fn test_pack_cama_winner_gains_bonus() {
        let mut fixture = Fixture::new();
        let (battle, pet_a, _) = start_battle(
            &mut fixture,
            PetSpec {
                species: "courier_cama",
                hunger: 50,
                ..PetSpec::default()
            },
        );
        let forced = force_a_wins(&battle.state);
        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &forced, 3)
            .unwrap();
        let SettleOutcome::Decisive(value) = outcome else {
            panic!("forced win must be decisive");
        };
        assert_eq!(value.winner.pet_id, pet_a);
        assert_eq!(value.winner_delta, 15);
    }

    #[test]
    fn test_gilded_loser_insured() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(
            &fixture,
            RECIPIENT,
            PetSpec {
                species: "jopacama",
                ..PetSpec::default()
            },
        );
        let pending = challenge(&mut fixture);
        let battle = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        let forced = force_a_wins(&battle.state);
        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &forced, 3)
            .unwrap();
        let SettleOutcome::Decisive(value) = outcome else {
            panic!("forced win must be decisive");
        };
        assert_eq!(value.loser_delta, -10);
    }

    #[test]
    fn test_settle_unfinished_state_rejected() {
        let mut fixture = Fixture::new();
        let (battle, _, _) = start_battle(&mut fixture, PetSpec::default());
        let result = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &battle.state, 0);
        assert_eq!(result.error_code(), Some(VALIDATION_ERROR));
    }

    #[test]
    fn test_settle_draw_has_no_winner_or_progression() {
        let mut fixture = Fixture::new();
        let (battle, pet_a, pet_b) = start_battle(&mut fixture, PetSpec::default());
        let mut draw = battle.state.clone();
        draw.round_no = 8;
        draw.winner = Some(BrawlWinner::Draw);

        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &draw, 8)
            .unwrap();

        let SettleOutcome::Draw(value) = outcome else {
            panic!("draw marker must produce draw result");
        };
        assert_eq!(value.duelists, (battle.state.a, battle.state.b));
        assert_eq!(
            value.records,
            BTreeMap::from([(pet_a, (0, 0)), (pet_b, (0, 0))])
        );
        let done = fixture.brawl(battle.brawl.brawl_id);
        assert_eq!(done.status, "done");
        assert_eq!(done.winner_id, None);
        assert_eq!(done.challenger_xp_delta, 0);
        assert_eq!(done.recipient_xp_delta, 0);
        assert!(fixture.evolution_calls.borrow().is_empty());
    }

    #[test]
    fn test_settle_returns_training_titles_and_persisted_personality_event() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 4,
                ..PetSpec::default()
            },
        );
        insert_pet(
            &fixture,
            RECIPIENT,
            PetSpec {
                training_xp: 4,
                ..PetSpec::default()
            },
        );
        let pending = challenge(&mut fixture);
        let battle = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        let forced = force_a_wins(&battle.state);

        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &forced, 3)
            .unwrap();

        let SettleOutcome::Decisive(value) = outcome else {
            panic!("forced win must be decisive");
        };
        assert_eq!(value.winner_xp_delta, 2);
        assert_eq!(value.loser_xp_delta, 1);
        assert!(matches!(
            value.winner_stat_gain.as_deref(),
            Some("str" | "int" | "dex")
        ));
        assert!(matches!(
            value.loser_stat_gain.as_deref(),
            Some("str" | "int" | "dex")
        ));
        assert_eq!(
            value.career[&forced.a.pet_id].rank.as_deref(),
            Some("Rookie")
        );
        assert_eq!(
            value.career[&forced.b.pet_id].rank.as_deref(),
            Some("Rookie")
        );
        let done = fixture.brawl(battle.brawl.brawl_id);
        assert_eq!(done.challenger_stat_gain, value.winner_stat_gain);
        assert_eq!(done.personality_event_key, value.personality_event_key);
        assert!(value.personality_event.is_some());
        assert_eq!(fixture.evolution_calls.borrow().len(), 2);
    }

    #[test]
    fn test_reaching_max_training_uses_build_milestone_event() {
        let mut fixture = Fixture::new();
        insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 19,
                training_str: 1,
                training_int: 1,
                training_dex: 1,
                ..PetSpec::default()
            },
        );
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let pending = challenge(&mut fixture);
        let battle = fixture
            .service
            .accept(pending.brawl.brawl_id, Some(GUILD), RECIPIENT)
            .unwrap();
        let forced = force_a_wins(&battle.state);

        let outcome = fixture
            .service
            .settle(battle.brawl.brawl_id, Some(GUILD), &forced, 3)
            .unwrap();

        let SettleOutcome::Decisive(value) = outcome else {
            panic!("forced win must be decisive");
        };
        assert_eq!(
            value.personality_event_key.as_deref(),
            Some("milestone.build")
        );
        assert!(
            value
                .personality_event
                .as_deref()
                .is_some_and(|event| event.to_lowercase().contains("fully trained"))
        );
        assert_eq!(
            fixture
                .brawl(battle.brawl.brawl_id)
                .personality_event_key
                .as_deref(),
            Some("milestone.build")
        );
    }

    #[test]
    fn test_career_summary_separates_rank_from_max_training_build() {
        let fixture = Fixture::new();
        let pet_id = insert_pet(
            &fixture,
            CHALLENGER,
            PetSpec {
                training_xp: 20,
                training_str: 2,
                training_int: 1,
                training_dex: 1,
                ..PetSpec::default()
            },
        );
        let summary = TestService::career_summary(&fixture.pet(pet_id), 10, 2);
        assert_eq!(
            summary,
            CareerSummary {
                xp: 20,
                strength: 2,
                intelligence: 1,
                dexterity: 1,
                rank: Some("Champion".to_owned()),
                build: Some("Bruiser".to_owned()),
            }
        );
    }

    #[test]
    fn test_sweep_stale_expires_and_voids() {
        let mut fixture = Fixture::new();
        insert_pet(&fixture, CHALLENGER, PetSpec::default());
        insert_pet(&fixture, RECIPIENT, PetSpec::default());
        let brawl_id = challenge(&mut fixture).brawl.brawl_id;
        fixture.now.set(T0 + 4 * DAY);

        let result = fixture.service.sweep_stale().expect("sweep succeeds");

        assert_eq!(
            result,
            SweepResult {
                expired: 1,
                voided: 0
            }
        );
        assert_eq!(fixture.brawl(brawl_id).status, "expired");
    }
}
