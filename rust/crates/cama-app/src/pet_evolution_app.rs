//! Application integration for Camagotchi upbringing and adult evolution.
//!
//! The pure scoring/calling rules live in `cama-domain`, while Python remains
//! the migration authority for the shared SQLite schema.  This module joins
//! those layers with fail-open gameplay activity adapters, lazy lifecycle
//! ordering, announcement/collection orchestration, and Discord-independent
//! presentation models.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::pet_repository::PetRepository;
use cama_domain::pet::{EvolutionNotice, Pet, PetMood, PetStage, PetStatus};
use cama_domain::pet_evolution::{
    InstinctScores, PetActivity, PetCalling, PetInstinct, calling_profile, hint_key_for_scores,
    hint_text, instinct_label, resolve_evolution,
};

use crate::pet::{PetCareEvent, PetEvolutionPort as PetApplicationEvolutionPort};

const CARD_WIDTH: usize = 512;
const CARD_HEIGHT: usize = 288;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEvent {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub activity: PetActivity,
    pub source_key: String,
    pub occurred_at: Option<i64>,
}

impl ActivityEvent {
    #[must_use]
    pub fn new(
        discord_id: i64,
        guild_id: Option<i64>,
        activity: PetActivity,
        source_key: impl Into<String>,
        occurred_at: Option<i64>,
    ) -> Self {
        Self {
            discord_id,
            guild_id,
            activity,
            source_key: source_key.into(),
            occurred_at,
        }
    }
}

pub trait ActivityRecorder {
    fn record(&mut self, event: ActivityEvent) -> i64;
}

/// The live adapter uses a blocking executor (`asyncio.to_thread` in Python).
/// Keeping this boundary explicit lets tests prove acknowledgement/event-loop
/// work is separated from synchronous SQLite activity capture.
pub trait BlockingBoundary {
    fn run<T>(&mut self, operation: impl FnOnce() -> T) -> T;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InlineBlockingBoundary;

impl BlockingBoundary for InlineBlockingBoundary {
    fn run<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        operation()
    }
}

/// Best-effort command adapter. Disabled pets are a quiet zero-point no-op.
pub fn record_pet_activity<R, B>(
    recorder: Option<&mut R>,
    boundary: &mut B,
    event: ActivityEvent,
) -> i64
where
    R: ActivityRecorder,
    B: BlockingBoundary,
{
    recorder.map_or(0, |recorder| boundary.run(|| recorder.record(event)))
}

pub fn record_successful_bet<R: ActivityRecorder>(
    recorder: &mut R,
    discord_id: i64,
    guild_id: Option<i64>,
    persisted_bet_id: i64,
) -> i64 {
    recorder.record(ActivityEvent::new(
        discord_id,
        guild_id,
        PetActivity::BetPlaced,
        format!("bet:{persisted_bet_id}"),
        None,
    ))
}

pub fn record_prediction_trade<R: ActivityRecorder>(
    recorder: &mut R,
    discord_id: i64,
    guild_id: Option<i64>,
    trade_id: i64,
    trade_time: i64,
) -> i64 {
    recorder.record(ActivityEvent::new(
        discord_id,
        guild_id,
        PetActivity::PredictionTraded,
        format!("prediction-trade:{trade_id}"),
        Some(trade_time),
    ))
}

pub fn record_resolved_duel<R: ActivityRecorder>(
    recorder: &mut R,
    guild_id: Option<i64>,
    challenge_id: i64,
    challenger_id: i64,
    recipient_id: i64,
    occurred_at: i64,
) -> i64 {
    [challenger_id, recipient_id]
        .into_iter()
        .map(|discord_id| {
            recorder.record(ActivityEvent::new(
                discord_id,
                guild_id,
                PetActivity::DuelCompleted,
                format!("duel:{challenge_id}"),
                Some(occurred_at),
            ))
        })
        .sum()
}

pub fn record_settled_pet_brawl<R: ActivityRecorder>(
    recorder: &mut R,
    guild_id: Option<i64>,
    brawl_id: i64,
    owner_ids: [i64; 2],
    occurred_at: i64,
) -> i64 {
    owner_ids
        .into_iter()
        .map(|discord_id| {
            recorder.record(ActivityEvent::new(
                discord_id,
                guild_id,
                PetActivity::PetBrawlCompleted,
                format!("pet-brawl:{brawl_id}"),
                Some(occurred_at),
            ))
        })
        .sum()
}

pub trait EvolutionClock {
    fn now(&mut self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEvolutionClock;

impl EvolutionClock for SystemEvolutionClock {
    fn now(&mut self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(i64::MAX)
    }
}

pub trait EvolutionStore {
    fn record_activity(&mut self, event: &ActivityEvent, occurred_at: i64) -> Result<i64, String>;
    fn scores(&mut self, pet_id: i64, guild_id: Option<i64>) -> Result<InstinctScores, String>;
    fn claim_evolution(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        expected_due_at: i64,
    ) -> Result<bool, String>;
    fn find_due_refs(&mut self, now: i64, limit: i64) -> Result<Vec<(i64, i64)>, String>;
    fn find_unannounced_refs(&mut self, limit: i64) -> Result<Vec<(i64, i64)>, String>;
    fn callings_raised(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<PetCalling>, String>;
    fn mark_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), String>;
}

impl EvolutionStore for PetEvolutionRepository {
    fn record_activity(&mut self, event: &ActivityEvent, occurred_at: i64) -> Result<i64, String> {
        PetEvolutionRepository::record_activity(
            self,
            event.discord_id,
            event.guild_id,
            event.activity,
            &event.source_key,
            occurred_at,
        )
        .map_err(|error| error.to_string())
    }

    fn scores(&mut self, pet_id: i64, guild_id: Option<i64>) -> Result<InstinctScores, String> {
        self.get_scores(pet_id, guild_id)
            .map_err(|error| error.to_string())
    }

    fn claim_evolution(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        expected_due_at: i64,
    ) -> Result<bool, String> {
        PetEvolutionRepository::claim_evolution(self, pet_id, guild_id, expected_due_at, |scores| {
            resolve_evolution(scores, pet_id)
        })
        .map_err(|error| error.to_string())
    }

    fn find_due_refs(&mut self, now: i64, limit: i64) -> Result<Vec<(i64, i64)>, String> {
        PetEvolutionRepository::find_due_refs(self, now, limit).map_err(|error| error.to_string())
    }

    fn find_unannounced_refs(&mut self, limit: i64) -> Result<Vec<(i64, i64)>, String> {
        PetEvolutionRepository::find_unannounced_refs(self, limit)
            .map_err(|error| error.to_string())
    }

    fn callings_raised(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<PetCalling>, String> {
        PetEvolutionRepository::callings_raised(self, discord_id, guild_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|value| parse_calling(&value).ok_or_else(|| format!("unknown calling: {value}")))
            .collect()
    }

    fn mark_announced(
        &mut self,
        pet_id: i64,
        guild_id: Option<i64>,
        announced_at: i64,
    ) -> Result<(), String> {
        PetEvolutionRepository::mark_announced(self, pet_id, guild_id, announced_at)
            .map_err(|error| error.to_string())
    }
}

pub trait EvolutionPetStore {
    fn pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Result<Option<Pet>, String>;
}

impl EvolutionPetStore for PetRepository {
    fn pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Result<Option<Pet>, String> {
        self.get_pet_by_id(pet_id, guild_id)
            .map_err(|error| error.to_string())
    }
}

pub struct PetEvolutionService<R, P, C> {
    evolution_store: R,
    pet_store: P,
    clock: C,
}

impl<R, P, C> PetEvolutionService<R, P, C>
where
    R: EvolutionStore,
    P: EvolutionPetStore,
    C: EvolutionClock,
{
    #[must_use]
    pub const fn new(evolution_store: R, pet_store: P, clock: C) -> Self {
        Self {
            evolution_store,
            pet_store,
            clock,
        }
    }

    #[must_use]
    pub const fn evolution_store(&self) -> &R {
        &self.evolution_store
    }

    pub const fn evolution_store_mut(&mut self) -> &mut R {
        &mut self.evolution_store
    }

    #[must_use]
    pub const fn pet_store(&self) -> &P {
        &self.pet_store
    }

    pub const fn pet_store_mut(&mut self) -> &mut P {
        &mut self.pet_store
    }

    #[must_use]
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    pub fn record_activity(&mut self, mut event: ActivityEvent) -> i64 {
        let occurred_at = event.occurred_at.unwrap_or_else(|| self.clock.now());
        event.occurred_at = Some(occurred_at);
        self.evolution_store
            .record_activity(&event, occurred_at)
            .unwrap_or_default()
    }

    pub fn hint_for(&mut self, pet: &Pet) -> Result<Option<String>, String> {
        if pet.evolved_at.is_some() || pet.evolution_due_at.is_none() {
            return Ok(None);
        }
        let scores = self
            .evolution_store
            .scores(pet.pet_id, Some(pet.guild_id))?;
        Ok(hint_key_for_scores(&scores))
    }

    pub fn resolve_if_due(&mut self, pet: Pet, alive_through: i64) -> Result<Pet, String> {
        let Some(due_at) = pet.evolution_due_at else {
            return Ok(pet);
        };
        if pet.evolved_at.is_some() || due_at > alive_through {
            return Ok(pet);
        }
        let _claimed =
            self.evolution_store
                .claim_evolution(pet.pet_id, Some(pet.guild_id), due_at)?;
        self.pet_store
            .pet_by_id(pet.pet_id, Some(pet.guild_id))
            .map(|fresh| fresh.unwrap_or(pet))
    }

    pub fn find_due(&mut self, now: i64, limit: i64) -> Result<Vec<Pet>, String> {
        let refs = self.evolution_store.find_due_refs(now, limit)?;
        let mut pets = Vec::with_capacity(refs.len());
        for (pet_id, guild_id) in refs {
            if let Some(pet) = self.pet_store.pet_by_id(pet_id, Some(guild_id))? {
                pets.push(pet);
            }
        }
        Ok(pets)
    }

    pub fn resolve_due_batch(&mut self, now: i64, limit: i64) -> Result<Vec<Pet>, String> {
        let due = self.find_due(now, limit)?;
        due.into_iter()
            .map(|pet| self.resolve_if_due(pet, now))
            .collect()
    }

    pub fn get_unannounced(&mut self, limit: i64) -> Result<Vec<EvolutionNotice>, String> {
        let refs = self.evolution_store.find_unannounced_refs(limit)?;
        let mut notices = Vec::with_capacity(refs.len());
        for (pet_id, guild_id) in refs {
            if let Some(pet) = self.pet_store.pet_by_id(pet_id, Some(guild_id))? {
                notices.push(EvolutionNotice { pet });
            }
        }
        Ok(notices)
    }

    pub fn callingdex(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<(Vec<PetCalling>, usize), String> {
        self.evolution_store
            .callings_raised(discord_id, guild_id)
            .map(|raised| (raised, PetCalling::ALL.len()))
    }

    pub fn mark_announced(&mut self, pet: &Pet) -> Result<(), String> {
        let now = self.clock.now();
        self.evolution_store
            .mark_announced(pet.pet_id, Some(pet.guild_id), now)
    }
}

impl<R, P, C> ActivityRecorder for PetEvolutionService<R, P, C>
where
    R: EvolutionStore,
    P: EvolutionPetStore,
    C: EvolutionClock,
{
    fn record(&mut self, event: ActivityEvent) -> i64 {
        self.record_activity(event)
    }
}

impl<R, P, C> PetApplicationEvolutionPort for PetEvolutionService<R, P, C>
where
    R: EvolutionStore,
    P: EvolutionPetStore,
    C: EvolutionClock,
{
    fn resolve_if_due(&mut self, pet: Pet, alive_through: i64) -> Pet {
        PetEvolutionService::resolve_if_due(self, pet.clone(), alive_through).unwrap_or(pet)
    }

    fn record_activity(&mut self, event: PetCareEvent) {
        let _points = PetEvolutionService::record_activity(
            self,
            ActivityEvent::new(
                event.discord_id,
                event.guild_id,
                event.activity,
                event.source_key,
                Some(event.occurred_at),
            ),
        );
    }

    fn find_due(&mut self, now: i64) -> Vec<Pet> {
        PetEvolutionService::find_due(self, now, 100).unwrap_or_default()
    }

    fn get_unannounced(&mut self, limit: i64) -> Vec<EvolutionNotice> {
        PetEvolutionService::get_unannounced(self, limit).unwrap_or_default()
    }
}

/// Resolve evolution only while the pet was alive strictly before its
/// starvation moment, then persist the in-memory death projection. The real
/// pet service performs the corresponding atomic death claim after this step.
pub fn resolve_before_starvation<R, P, C>(
    service: &mut PetEvolutionService<R, P, C>,
    pet: Pet,
    observed_at: i64,
    starvation_at: i64,
) -> Result<Pet, String>
where
    R: EvolutionStore,
    P: EvolutionPetStore,
    C: EvolutionClock,
{
    let alive_through = observed_at.min(starvation_at.saturating_sub(1));
    let mut resolved = service.resolve_if_due(pet, alive_through)?;
    if observed_at >= starvation_at {
        resolved.died_at = Some(starvation_at);
        resolved.death_cause = Some("starvation".to_owned());
    }
    Ok(resolved)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentationField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetArtKey {
    pub species: String,
    pub stage: PetStage,
    pub art_mood: String,
    pub pet_id: i64,
    pub accessory: Option<String>,
    pub calling: Option<PetCalling>,
    pub primary: Option<PetInstinct>,
    pub secondary: Option<PetInstinct>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvolutionStatusPresentation {
    pub title: String,
    pub upbringing: Option<PresentationField>,
    pub calling: Option<PresentationField>,
    pub art_key: PetArtKey,
}

#[must_use]
pub fn build_evolution_status_presentation(
    pet: &Pet,
    status: &PetStatus,
    now: i64,
    decay_per_day: i64,
) -> EvolutionStatusPresentation {
    let parsed_calling = pet.evolution_calling.as_deref().and_then(parse_calling);
    let primary = pet.evolution_primary.as_deref().and_then(parse_instinct);
    let secondary = pet.evolution_secondary.as_deref().and_then(parse_instinct);
    let profile = parsed_calling.map(calling_profile);
    let mut title = pet.name.clone();
    if let Some(profile) = profile {
        title.push_str(&format!(" · {} {}", profile.emoji, profile.label));
    }
    let upbringing = hint_text(status.evolution_hint.as_deref()).map(|text| PresentationField {
        name: "Upbringing".to_owned(),
        value: text.to_owned(),
    });
    let calling = profile.map(|profile| {
        let paths = [primary, secondary]
            .into_iter()
            .flatten()
            .map(instinct_label)
            .collect::<Vec<_>>();
        PresentationField {
            name: "Calling".to_owned(),
            value: format!(
                "_{}_\nPaths: {}",
                profile.blurb,
                if paths.is_empty() {
                    "A little of every path".to_owned()
                } else {
                    paths.join(" + ")
                }
            ),
        }
    });
    EvolutionStatusPresentation {
        title,
        upbringing,
        calling,
        art_key: PetArtKey {
            species: pet.species.clone(),
            stage: status.stage.unwrap_or(PetStage::Baby),
            art_mood: pet.art_mood(now, decay_per_day).to_owned(),
            pet_id: pet.pet_id,
            accessory: pet.accessory.clone(),
            calling: parsed_calling,
            primary,
            secondary,
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvolutionAnnouncement {
    pub title: String,
    pub description: String,
}

#[must_use]
pub fn build_evolution_announcement(pet: &Pet) -> EvolutionAnnouncement {
    let calling = pet
        .evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .unwrap_or(PetCalling::Wayfarer);
    let profile = calling_profile(calling);
    let paths = [
        pet.evolution_primary.as_deref().and_then(parse_instinct),
        pet.evolution_secondary.as_deref().and_then(parse_instinct),
    ]
    .into_iter()
    .flatten()
    .map(instinct_label)
    .collect::<Vec<_>>();
    EvolutionAnnouncement {
        title: format!(
            "{} {} evolved — {}!",
            profile.emoji, pet.name, profile.label
        ),
        description: format!(
            "<@{}>'s cama found a calling.\n\n**{}**\n_{}_",
            pet.discord_id,
            if paths.is_empty() {
                "A little of every path".to_owned()
            } else {
                paths.join(" + ")
            },
            profile.blurb
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvolutionGraveyardPresentation {
    pub description: String,
    pub callingdex: PresentationField,
}

#[must_use]
pub fn build_evolution_graveyard(
    pets: &[Pet],
    raised_callings: &[PetCalling],
) -> EvolutionGraveyardPresentation {
    let description = pets
        .iter()
        .map(|pet| {
            let calling = pet
                .evolution_calling
                .as_deref()
                .and_then(parse_calling)
                .map(calling_profile)
                .map_or("", |profile| profile.label);
            format!("**{}** · {calling}", pet.name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let entries = PetCalling::ALL
        .into_iter()
        .map(|calling| {
            if raised_callings.contains(&calling) {
                calling_profile(calling).label
            } else {
                "???"
            }
        })
        .collect::<Vec<_>>();
    EvolutionGraveyardPresentation {
        description,
        callingdex: PresentationField {
            name: format!(
                "Callings {}/{}",
                raised_callings.len(),
                PetCalling::ALL.len()
            ),
            value: entries.join(" · "),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterCard {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RasterCard {
    #[must_use]
    pub fn region_is_clear(&self, left: usize, top: usize, right: usize, bottom: usize) -> bool {
        (top..bottom).all(|y| {
            (left..right).all(|x| {
                let offset = (y * self.width + x) * 4;
                self.pixels[offset..offset + 4] == [0, 0, 0, 0]
            })
        })
    }
}

#[must_use]
pub fn render_evolution_motif(
    calling: PetCalling,
    primary: PetInstinct,
    secondary: Option<PetInstinct>,
    stage: PetStage,
    seed: i64,
) -> Option<RasterCard> {
    if stage != PetStage::Adult {
        return None;
    }
    let mut pixels = vec![0; CARD_WIDTH * CARD_HEIGHT * 4];
    let signature = stable_signature(&format!(
        "{}:{}:{}:{seed}",
        calling.as_str(),
        primary.as_str(),
        secondary.map_or("", PetInstinct::as_str)
    ));
    for step in 0..160_usize {
        let left_side = step % 2 == 0;
        let x = if left_side {
            24 + (signature as usize + step * 17) % 140
        } else {
            348 + (signature as usize + step * 19) % 140
        };
        let y = 18 + (signature.rotate_left((step % 31) as u32) as usize + step * 13) % 246;
        let offset = (y * CARD_WIDTH + x) * 4;
        pixels[offset..offset + 4].copy_from_slice(&[
            (signature & 0xff) as u8,
            ((signature >> 8) & 0xff) as u8,
            ((signature >> 16) & 0xff) as u8,
            180,
        ]);
    }
    Some(RasterCard {
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        pixels,
    })
}

#[must_use]
pub fn render_pet_card_model(
    species: &str,
    stage: PetStage,
    mood: PetMood,
    seed: i64,
    evolution: Option<(PetCalling, PetInstinct, Option<PetInstinct>)>,
) -> RasterCard {
    let signature = stable_signature(&format!(
        "{species}:{}:{}:{seed}",
        stage.as_str(),
        mood.as_str()
    ));
    let mut pixels = vec![0; CARD_WIDTH * CARD_HEIGHT * 4];
    for (index, chunk) in pixels.chunks_exact_mut(4).enumerate() {
        chunk.copy_from_slice(&[
            (signature & 0xff) as u8,
            ((signature >> 8) & 0xff) as u8,
            u8::try_from(index % 251).unwrap_or_default(),
            255,
        ]);
    }
    if let Some((calling, primary, secondary)) = evolution
        && let Some(motif) = render_evolution_motif(calling, primary, secondary, stage, seed)
    {
        for (target, overlay) in pixels.chunks_exact_mut(4).zip(motif.pixels.chunks_exact(4)) {
            if overlay[3] != 0 {
                target.copy_from_slice(overlay);
            }
        }
    }
    RasterCard {
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        pixels,
    }
}

fn stable_signature(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn parse_instinct(value: &str) -> Option<PetInstinct> {
    match value {
        "delving" => Some(PetInstinct::Delving),
        "competition" => Some(PetInstinct::Competition),
        "fortune" => Some(PetInstinct::Fortune),
        "fellowship" => Some(PetInstinct::Fellowship),
        "wisdom" => Some(PetInstinct::Wisdom),
        _ => None,
    }
}

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

#[must_use]
pub fn score_snapshot(entries: &[(PetInstinct, i64)]) -> InstinctScores {
    let mut scores = PetInstinct::ALL
        .into_iter()
        .map(|instinct| (instinct, 0))
        .collect::<BTreeMap<_, _>>();
    scores.extend(entries.iter().copied());
    scores
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use cama_domain::pet::{PetBase, PetStatus};

    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 987_654_321;

    fn make_pet() -> Pet {
        let mut pet = Pet::new(PetBase {
            pet_id: 7,
            discord_id: 100,
            guild_id: GUILD,
            name: "Blep".to_owned(),
            species: "common_cama".to_owned(),
            adopted_at: T0 - DAY,
            hatched_at: T0,
            adopt_fee: 20,
            last_fed_at: T0,
            hunger_at_last_fed: 100,
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
            hatch_announced_at: Some(T0),
            died_at: None,
            death_cause: None,
            death_announced_at: None,
        });
        pet.evolution_started_at = Some(T0);
        pet.evolution_due_at = Some(T0 + 7 * DAY);
        pet
    }

    fn make_status(pet: &Pet) -> PetStatus {
        PetStatus {
            pet: Some(pet.clone()),
            hunger: 80,
            stage: Some(PetStage::Baby),
            mood: Some(PetMood::Happy),
            age_seconds: 2 * DAY,
            supplies: Some(BTreeMap::new()),
            last_dead: None,
            dig_work_units: 0,
            dig_work_rate: 8,
            evolution_hint: None,
        }
    }

    #[derive(Default)]
    struct RecordingActivity {
        points: i64,
        events: Vec<ActivityEvent>,
    }

    impl ActivityRecorder for RecordingActivity {
        fn record(&mut self, event: ActivityEvent) -> i64 {
            self.events.push(event);
            self.points
        }
    }

    #[derive(Default)]
    struct RecordingBoundary {
        calls: usize,
    }

    impl BlockingBoundary for RecordingBoundary {
        fn run<T>(&mut self, operation: impl FnOnce() -> T) -> T {
            self.calls += 1;
            operation()
        }
    }

    #[derive(Clone, Debug)]
    struct FakeClock {
        now: i64,
        calls: usize,
    }

    impl EvolutionClock for FakeClock {
        fn now(&mut self) -> i64 {
            self.calls += 1;
            self.now
        }
    }

    type SharedPets = Rc<RefCell<BTreeMap<(i64, i64), Pet>>>;

    #[derive(Clone)]
    struct FakePetStore {
        pets: SharedPets,
        lookups: Rc<RefCell<Vec<(i64, i64)>>>,
    }

    impl EvolutionPetStore for FakePetStore {
        fn pet_by_id(&mut self, pet_id: i64, guild_id: Option<i64>) -> Result<Option<Pet>, String> {
            let guild_id = guild_id.unwrap_or_default();
            self.lookups.borrow_mut().push((pet_id, guild_id));
            Ok(self.pets.borrow().get(&(pet_id, guild_id)).cloned())
        }
    }

    struct FakeEvolutionStore {
        pets: SharedPets,
        events: Vec<(ActivityEvent, i64)>,
        fail_record: bool,
        scores: BTreeMap<i64, InstinctScores>,
        due_refs: Vec<(i64, i64)>,
        unannounced_refs: Vec<(i64, i64)>,
        raised: Vec<PetCalling>,
        marked: Vec<(i64, i64, i64)>,
        find_due_calls: Vec<(i64, i64)>,
    }

    impl FakeEvolutionStore {
        fn new(pets: SharedPets) -> Self {
            Self {
                pets,
                events: Vec::new(),
                fail_record: false,
                scores: BTreeMap::new(),
                due_refs: Vec::new(),
                unannounced_refs: Vec::new(),
                raised: Vec::new(),
                marked: Vec::new(),
                find_due_calls: Vec::new(),
            }
        }
    }

    impl EvolutionStore for FakeEvolutionStore {
        fn record_activity(
            &mut self,
            event: &ActivityEvent,
            occurred_at: i64,
        ) -> Result<i64, String> {
            if self.fail_record {
                return Err("locked".to_owned());
            }
            self.events.push((event.clone(), occurred_at));
            Ok(2)
        }

        fn scores(
            &mut self,
            pet_id: i64,
            _guild_id: Option<i64>,
        ) -> Result<InstinctScores, String> {
            Ok(self
                .scores
                .get(&pet_id)
                .cloned()
                .unwrap_or_else(|| score_snapshot(&[])))
        }

        fn claim_evolution(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            expected_due_at: i64,
        ) -> Result<bool, String> {
            let guild_id = guild_id.unwrap_or_default();
            let scores = self
                .scores
                .get(&pet_id)
                .cloned()
                .unwrap_or_else(|| score_snapshot(&[]));
            let mut pets = self.pets.borrow_mut();
            let Some(pet) = pets.get_mut(&(pet_id, guild_id)) else {
                return Ok(false);
            };
            if pet.evolved_at.is_some()
                || pet.evolution_due_at != Some(expected_due_at)
                || pet
                    .died_at
                    .is_some_and(|died_at| died_at <= expected_due_at)
            {
                return Ok(false);
            }
            let decision = resolve_evolution(&scores, pet_id);
            pet.evolved_at = Some(expected_due_at);
            pet.evolution_calling = Some(decision.calling.as_str().to_owned());
            pet.evolution_primary = decision.primary.map(|value| value.as_str().to_owned());
            pet.evolution_secondary = decision.secondary.map(|value| value.as_str().to_owned());
            if !self.unannounced_refs.contains(&(pet_id, guild_id)) {
                self.unannounced_refs.push((pet_id, guild_id));
            }
            Ok(true)
        }

        fn find_due_refs(&mut self, now: i64, limit: i64) -> Result<Vec<(i64, i64)>, String> {
            self.find_due_calls.push((now, limit));
            Ok(self
                .due_refs
                .iter()
                .take(usize::try_from(limit.max(0)).unwrap_or_default())
                .copied()
                .collect())
        }

        fn find_unannounced_refs(&mut self, limit: i64) -> Result<Vec<(i64, i64)>, String> {
            Ok(self
                .unannounced_refs
                .iter()
                .take(usize::try_from(limit.max(0)).unwrap_or_default())
                .copied()
                .collect())
        }

        fn callings_raised(
            &mut self,
            _discord_id: i64,
            _guild_id: Option<i64>,
        ) -> Result<Vec<PetCalling>, String> {
            Ok(self.raised.clone())
        }

        fn mark_announced(
            &mut self,
            pet_id: i64,
            guild_id: Option<i64>,
            announced_at: i64,
        ) -> Result<(), String> {
            let guild_id = guild_id.unwrap_or_default();
            self.marked.push((pet_id, guild_id, announced_at));
            self.unannounced_refs
                .retain(|candidate| *candidate != (pet_id, guild_id));
            if let Some(pet) = self.pets.borrow_mut().get_mut(&(pet_id, guild_id)) {
                pet.evolution_announced_at = Some(announced_at);
            }
            Ok(())
        }
    }

    type TestService = PetEvolutionService<FakeEvolutionStore, FakePetStore, FakeClock>;

    fn make_service(pet: Option<Pet>) -> TestService {
        let pets = Rc::new(RefCell::new(BTreeMap::new()));
        if let Some(pet) = pet {
            pets.borrow_mut().insert((pet.pet_id, pet.guild_id), pet);
        }
        PetEvolutionService::new(
            FakeEvolutionStore::new(Rc::clone(&pets)),
            FakePetStore {
                pets,
                lookups: Rc::new(RefCell::new(Vec::new())),
            },
            FakeClock { now: T0, calls: 0 },
        )
    }

    #[test]
    fn test_async_command_adapter_is_a_quiet_noop_when_pets_are_disabled() {
        let mut boundary = RecordingBoundary::default();

        let points = record_pet_activity::<RecordingActivity, _>(
            None,
            &mut boundary,
            ActivityEvent::new(100, Some(GUILD), PetActivity::DigCompleted, "dig:1", None),
        );

        assert_eq!(points, 0);
        assert_eq!(boundary.calls, 0);
    }

    #[test]
    fn test_async_command_adapter_runs_the_sync_boundary_off_loop() {
        let mut recorder = RecordingActivity {
            points: 2,
            events: Vec::new(),
        };
        let mut boundary = RecordingBoundary::default();

        let points = record_pet_activity(
            Some(&mut recorder),
            &mut boundary,
            ActivityEvent::new(
                100,
                Some(GUILD),
                PetActivity::DigCompleted,
                "dig:1",
                Some(T0),
            ),
        );

        assert_eq!(points, 2);
        assert_eq!(boundary.calls, 1);
        assert_eq!(
            recorder.events,
            vec![ActivityEvent::new(
                100,
                Some(GUILD),
                PetActivity::DigCompleted,
                "dig:1",
                Some(T0)
            )]
        );
    }

    #[test]
    fn test_successful_bet_records_fortune_with_persisted_bet_id() {
        let mut recorder = RecordingActivity::default();

        record_successful_bet(&mut recorder, 100, Some(GUILD), 77);

        assert_eq!(
            recorder.events,
            vec![ActivityEvent::new(
                100,
                Some(GUILD),
                PetActivity::BetPlaced,
                "bet:77",
                None
            )]
        );
    }

    #[test]
    fn test_prediction_buy_records_fortune_with_atomic_trade_identity() {
        let mut recorder = RecordingActivity::default();

        record_prediction_trade(&mut recorder, 100, Some(GUILD), 88, T0);

        assert_eq!(
            recorder.events,
            vec![ActivityEvent::new(
                100,
                Some(GUILD),
                PetActivity::PredictionTraded,
                "prediction-trade:88",
                Some(T0)
            )]
        );
    }

    #[test]
    fn test_resolved_duel_records_competition_for_both_participants() {
        let mut recorder = RecordingActivity::default();

        record_resolved_duel(&mut recorder, Some(GUILD), 42, 100, 200, T0);

        assert_eq!(
            recorder.events,
            vec![
                ActivityEvent::new(
                    100,
                    Some(GUILD),
                    PetActivity::DuelCompleted,
                    "duel:42",
                    Some(T0)
                ),
                ActivityEvent::new(
                    200,
                    Some(GUILD),
                    PetActivity::DuelCompleted,
                    "duel:42",
                    Some(T0)
                )
            ]
        );
    }

    #[test]
    fn test_settled_pet_brawl_records_competition_for_both_owners() {
        let mut recorder = RecordingActivity::default();

        record_settled_pet_brawl(&mut recorder, Some(GUILD), 55, [100, 200], T0);

        assert_eq!(
            recorder.events,
            vec![
                ActivityEvent::new(
                    100,
                    Some(GUILD),
                    PetActivity::PetBrawlCompleted,
                    "pet-brawl:55",
                    Some(T0)
                ),
                ActivityEvent::new(
                    200,
                    Some(GUILD),
                    PetActivity::PetBrawlCompleted,
                    "pet-brawl:55",
                    Some(T0)
                )
            ]
        );
    }

    #[test]
    fn test_record_activity_derives_clock_once() {
        let mut service = make_service(None);

        let points = service.record_activity(ActivityEvent::new(
            100,
            Some(GUILD),
            PetActivity::DigCompleted,
            "dig:123",
            None,
        ));

        assert_eq!(points, 2);
        assert_eq!(service.clock().calls, 1);
        assert_eq!(service.evolution_store().events.len(), 1);
        assert_eq!(service.evolution_store().events[0].1, T0);
        assert_eq!(service.evolution_store().events[0].0.source_key, "dig:123");
    }

    #[test]
    fn test_record_activity_is_fail_open_for_primary_gameplay() {
        let mut service = make_service(None);
        service.evolution_store_mut().fail_record = true;

        let points = service.record_activity(ActivityEvent::new(
            100,
            Some(GUILD),
            PetActivity::DigCompleted,
            "dig:123",
            Some(T0),
        ));

        assert_eq!(points, 0);
    }

    #[test]
    fn test_pet_evolves_at_due_time_before_a_later_starvation() {
        let due = T0 + 7 * DAY;
        let mut pet = make_pet();
        pet.evolution_due_at = Some(due);
        let mut service = make_service(Some(pet.clone()));
        service
            .evolution_store_mut()
            .scores
            .insert(pet.pet_id, score_snapshot(&[(PetInstinct::Delving, 4)]));
        service
            .evolution_store_mut()
            .raised
            .push(PetCalling::Delver);

        let resolved = resolve_before_starvation(&mut service, pet, due + 200, due + 100)
            .expect("resolve lifecycle");

        assert_eq!(resolved.evolved_at, Some(due));
        assert_eq!(resolved.evolution_calling.as_deref(), Some("delver"));
        assert_eq!(resolved.evolution_primary.as_deref(), Some("delving"));
        assert_eq!(resolved.evolution_secondary, None);
        assert_eq!(resolved.died_at, Some(due + 100));
        let notices = service.get_unannounced(100).expect("unannounced");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].pet.evolution_calling.as_deref(), Some("delver"));
        assert_eq!(
            service.callingdex(100, Some(GUILD)).expect("callingdex"),
            (vec![PetCalling::Delver], PetCalling::ALL.len())
        );
        service
            .mark_announced(&notices[0].pet)
            .expect("mark announced");
        assert!(
            service
                .get_unannounced(100)
                .expect("unannounced after mark")
                .is_empty()
        );
    }

    #[test]
    fn test_status_exposes_only_the_narrative_hint_key() {
        let pet = make_pet();
        let mut service = make_service(Some(pet.clone()));
        service
            .evolution_store_mut()
            .scores
            .insert(pet.pet_id, score_snapshot(&[(PetInstinct::Delving, 4)]));

        let hint = service.hint_for(&pet).expect("hint lookup");

        assert_eq!(hint.as_deref(), Some("delving"));
    }

    #[test]
    fn test_pet_that_starves_at_the_due_moment_does_not_evolve() {
        let due = T0 + 7 * DAY;
        let mut pet = make_pet();
        pet.evolution_due_at = Some(due);
        let mut service = make_service(Some(pet.clone()));

        let resolved =
            resolve_before_starvation(&mut service, pet, due, due).expect("resolve lifecycle");

        assert_eq!(resolved.died_at, Some(due));
        assert_eq!(resolved.evolved_at, None);
        assert_eq!(resolved.evolution_calling, None);
    }

    #[test]
    fn test_sweep_resolves_healthy_due_pets_without_scanning_all_pets() {
        let due = T0 + 7 * DAY;
        let mut pet = make_pet();
        pet.evolution_due_at = Some(due);
        let mut service = make_service(Some(pet.clone()));
        service
            .evolution_store_mut()
            .due_refs
            .push((pet.pet_id, pet.guild_id));

        let resolved = service
            .resolve_due_batch(due, 100)
            .expect("resolve due batch");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].evolved_at, Some(due));
        assert_eq!(resolved[0].evolution_calling.as_deref(), Some("wayfarer"));
        assert_eq!(service.evolution_store().find_due_calls, vec![(due, 100)]);
        assert_eq!(service.pet_store().lookups.borrow().len(), 2);
        let notices = service.get_unannounced(100).expect("unannounced");
        assert_eq!(
            notices,
            vec![EvolutionNotice {
                pet: resolved[0].clone()
            }]
        );
    }

    #[test]
    fn test_upbringing_status_shows_only_a_narrative_hint() {
        let pet = make_pet();
        let mut status = make_status(&pet);
        status.evolution_hint = Some("delving_strong".to_owned());

        let presentation = build_evolution_status_presentation(&pet, &status, T0 + 2 * DAY, 20);

        let field = presentation.upbringing.expect("upbringing field");
        assert!(
            field.value.to_lowercase().contains("deep")
                || field.value.to_lowercase().contains("stone")
        );
        assert!(
            !field
                .value
                .chars()
                .any(|character| character.is_ascii_digit())
        );
    }

    #[test]
    fn test_evolved_status_uses_calling_title_profile_and_cached_art_key() {
        let mut pet = make_pet();
        pet.evolved_at = Some(T0 + 7 * DAY);
        pet.evolution_calling = Some("prospector".to_owned());
        pet.evolution_primary = Some("delving".to_owned());
        pet.evolution_secondary = Some("fortune".to_owned());
        let mut status = make_status(&pet);
        status.stage = Some(PetStage::Adult);
        status.age_seconds = 8 * DAY;

        let presentation = build_evolution_status_presentation(&pet, &status, T0 + 8 * DAY, 20);

        assert!(presentation.title.contains("Prospector"));
        let calling = presentation.calling.expect("calling field");
        assert!(calling.value.to_lowercase().contains("fortune"));
        assert!(calling.value.to_lowercase().contains("delving"));
        assert_eq!(presentation.art_key.species, pet.species);
        assert_eq!(presentation.art_key.stage, PetStage::Adult);
        assert_eq!(
            presentation.art_key.art_mood,
            pet.art_mood(T0 + 8 * DAY, 20)
        );
        assert_eq!(presentation.art_key.pet_id, pet.pet_id);
        assert_eq!(presentation.art_key.calling, Some(PetCalling::Prospector));
        assert_eq!(presentation.art_key.primary, Some(PetInstinct::Delving));
        assert_eq!(presentation.art_key.secondary, Some(PetInstinct::Fortune));
    }

    #[test]
    fn test_evolution_announcement_and_graveyard_preserve_calling_identity() {
        let mut pet = make_pet();
        pet.evolved_at = Some(T0 + 7 * DAY);
        pet.evolution_calling = Some("oracle".to_owned());
        pet.evolution_primary = Some("fortune".to_owned());
        pet.evolution_secondary = Some("wisdom".to_owned());
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());

        let announcement = build_evolution_announcement(&pet);
        let graveyard = build_evolution_graveyard(&[pet], &[PetCalling::Oracle]);

        assert!(announcement.title.contains("Oracle"));
        assert!(announcement.description.contains("Fortune"));
        assert!(announcement.description.contains("Wisdom"));
        assert!(graveyard.description.contains("Oracle"));
        assert!(graveyard.callingdex.value.contains("Oracle"));
        assert!(graveyard.callingdex.value.contains("???"));
    }

    #[test]
    fn test_evolution_motifs_are_deterministic_and_change_the_adult_card() {
        let base = render_pet_card_model("common_cama", PetStage::Adult, PetMood::Happy, 7, None);
        let evolution = Some((
            PetCalling::Prospector,
            PetInstinct::Delving,
            Some(PetInstinct::Fortune),
        ));
        let evolved =
            render_pet_card_model("common_cama", PetStage::Adult, PetMood::Happy, 7, evolution);
        let repeated =
            render_pet_card_model("common_cama", PetStage::Adult, PetMood::Happy, 7, evolution);

        assert_ne!(evolved, base);
        assert_eq!(evolved, repeated);
        assert_eq!((evolved.width, evolved.height), (512, 288));
    }

    #[test]
    fn test_adult_evolution_motif_leaves_the_pet_body_region_clear() {
        let motif = render_evolution_motif(
            PetCalling::Prospector,
            PetInstinct::Delving,
            Some(PetInstinct::Fortune),
            PetStage::Adult,
            7,
        )
        .expect("adult evolution motif");

        assert!(motif.region_is_clear(190, 126, 323, 245));
    }
}
