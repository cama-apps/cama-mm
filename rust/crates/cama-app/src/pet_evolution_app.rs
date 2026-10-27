//! Application integration for Camagotchi upbringing and adult evolution:
//! activity recording, lazy evolution resolution, and announcement/collection
//! orchestration over the pure rules in `cama-domain`.

use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::pet_repository::PetRepository;
use cama_domain::pet::{EvolutionNotice, Pet};
use cama_domain::pet_evolution::{InstinctScores, PetActivity, PetCalling, resolve_evolution};

use crate::pet::{PetCareEvent, PetEvolutionPort as PetApplicationEvolutionPort};

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

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use cama_domain::pet_evolution::PetInstinct;

    use super::*;

    const T0: i64 = 1_800_000_000;
    const GUILD: i64 = 987_654_321;

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

    type TestService = PetEvolutionService<FakeEvolutionStore, FakePetStore, FakeClock>;

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

    fn score_snapshot(entries: &[(PetInstinct, i64)]) -> InstinctScores {
        let mut scores = PetInstinct::ALL
            .into_iter()
            .map(|instinct| (instinct, 0))
            .collect::<BTreeMap<_, _>>();
        scores.extend(entries.iter().copied());
        scores
    }

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
}
