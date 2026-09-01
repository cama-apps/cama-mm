use super::*;
use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::core_repositories::NewPlayer;
use cama_db::pet_eating_repository::{EatAdultPetRequest, PetEatingRepository};
use cama_domain::pet::{ADULT_AGE_SECONDS, EGG_HATCH_SECONDS, PetStage};
use rusqlite::params;
use tempfile::NamedTempFile;

use crate::pet::SeededPetRandom;

fn assert_production_ports<S: PetStore, P: PlayerPort>() {}

#[test]
fn sqlite_repositories_satisfy_the_complete_pet_application_ports() {
    assert_production_ports::<PetRepository, SqlitePetPlayerPort>();
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

#[test]
fn production_service_admits_migrated_sqlite_and_recovers_pet_after_restart() {
    const NOW: i64 = 1_800_000_000;
    const GUILD: i64 = 42;
    const OWNER: i64 = 7;

    let file = NamedTempFile::new().expect("temporary SQLite database");
    initialize_or_migrate(file.path()).expect("initialize canonical schema");
    let players = PlayerRepository::new(file.path());
    players
        .add(&NewPlayer::new(OWNER, "Pet Owner", Some(GUILD)))
        .expect("register pet owner");
    players
        .update_balance(OWNER, Some(GUILD), 1_000)
        .expect("fund pet owner");

    let mut first =
        SqlitePetCommandService::new(file.path(), SeededPetRandom::new(11), FixedClock(NOW), 20);
    let adopted = first.adopt(OWNER, Some(GUILD), "Blep", "standard").unwrap();
    assert_eq!(adopted.pet.name, "Blep");
    assert_eq!(
        first.status(OWNER, Some(GUILD)).unwrap().stage,
        Some(PetStage::Egg)
    );
    drop(first);

    let mut restarted = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(11),
        FixedClock(NOW + EGG_HATCH_SECONDS),
        20,
    );
    let status = restarted.status(OWNER, Some(GUILD)).unwrap();
    assert_ne!(status.stage, Some(PetStage::Egg));
    assert_ne!(status.pet.unwrap().species, "unhatched");
    assert_eq!(restarted.balance(OWNER, Some(GUILD)), Some(980));
}

#[test]
fn production_altar_is_atomic_and_recovers_the_rebirth_after_restart() {
    const NOW: i64 = 1_800_000_000;
    const GUILD: i64 = 42;
    const OWNER: i64 = 7;

    let file = NamedTempFile::new().expect("temporary SQLite database");
    initialize_or_migrate(file.path()).expect("initialize canonical schema");
    let players = PlayerRepository::new(file.path());
    players
        .add(&NewPlayer::new(OWNER, "Pet Owner", Some(GUILD)))
        .expect("register pet owner");
    players
        .update_balance(OWNER, Some(GUILD), 1_000)
        .expect("fund pet owner");

    let mut service =
        SqlitePetCommandService::new(file.path(), SeededPetRandom::new(17), FixedClock(NOW), 20);
    let adopted = service
        .adopt(OWNER, Some(GUILD), "Offering", "standard")
        .unwrap();
    let ritual_at = adopted.pet.hatched_at + 1;
    drop(service);
    PetRepository::new(file.path())
        .resolve_hatch_species(
            adopted.pet.pet_id,
            Some(GUILD),
            "invoker_cama",
            adopted.pet.hatched_at,
        )
        .expect("resolve offered pet species");

    let mut service = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(17),
        FixedClock(ritual_at),
        20,
    );
    let preview = service.sacrifice_preview(OWNER, Some(GUILD)).unwrap();
    assert_eq!(preview.fee, 50);
    let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();
    assert_eq!(outcome.dead_pet.death_cause.as_deref(), Some("sacrifice"));
    assert_ne!(outcome.new_pet.species, "unhatched");
    assert_eq!(outcome.new_pet.hatched_at, ritual_at + EGG_HATCH_SECONDS);
    drop(service);

    let mut restarted = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(99),
        FixedClock(ritual_at + 1),
        20,
    );
    let status = restarted.status(OWNER, Some(GUILD)).unwrap();
    assert_eq!(
        status.pet.as_ref().map(|pet| pet.name.as_str()),
        Some("Rebirth")
    );
    assert_eq!(status.stage, Some(PetStage::Egg));
    assert_eq!(restarted.balance(OWNER, Some(GUILD)), Some(930));
    assert!(
        PetRepository::new(file.path())
            .recent_dead_species(OWNER, Some(GUILD), 10)
            .expect("read pity history")
            .is_empty(),
        "altar deaths must not count toward pity"
    );
}

#[test]
fn production_sweep_rehydrates_durable_eating_outcome_after_restart() {
    const NOW: i64 = 1_800_000_000;
    const GUILD: i64 = 42;
    const OWNER: i64 = 7;

    let file = NamedTempFile::new().expect("temporary SQLite database");
    initialize_or_migrate(file.path()).expect("initialize canonical schema");
    let players = PlayerRepository::new(file.path());
    players
        .add(&NewPlayer::new(OWNER, "Pet Owner", Some(GUILD)))
        .expect("register pet owner");
    players
        .update_balance(OWNER, Some(GUILD), 1_000)
        .expect("fund pet owner");

    let adult_at = NOW + EGG_HATCH_SECONDS + ADULT_AGE_SECONDS;
    let mut service =
        SqlitePetCommandService::new(file.path(), SeededPetRandom::new(13), FixedClock(NOW), 20);
    let adopted = service
        .adopt(OWNER, Some(GUILD), "Dinner", "standard")
        .unwrap();
    drop(service);
    PetRepository::new(file.path())
        .resolve_hatch_species(
            adopted.pet.pet_id,
            Some(GUILD),
            "common_cama",
            NOW + EGG_HATCH_SECONDS,
        )
        .expect("resolve hatch before adulthood");
    rusqlite::Connection::open(file.path())
        .expect("open pet fixture")
        .execute(
            "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1
             WHERE pet_id=?2",
            params![adult_at, adopted.pet.pet_id],
        )
        .expect("refresh adult hunger anchor");
    let mut service = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(13),
        FixedClock(adult_at),
        20,
    );
    let pet = service
        .status(OWNER, Some(GUILD))
        .unwrap()
        .pet
        .expect("adult pet");
    assert_eq!(pet.stage(adult_at), PetStage::Adult);
    PetEatingRepository::new(file.path())
        .eat_adult_pet_atomic(EatAdultPetRequest {
            discord_id: OWNER,
            guild_id: Some(GUILD),
            pet_id: pet.pet_id,
            expected_last_fed_at: pet.last_fed_at,
            expected_hunger: pet.hunger_at_last_fed,
            reward: 123,
            penalty_games: 4,
            now: adult_at,
        })
        .expect("eat pet atomically");
    drop(service);

    let mut restarted = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(99),
        FixedClock(adult_at + 1),
        20,
    );
    let outcome = restarted.sweep().expect("restart sweep succeeds");
    assert_eq!(outcome.deaths.len(), 1);
    let durable = outcome.deaths[0]
        .eating_outcome
        .as_ref()
        .expect("durable eating outcome");
    assert_eq!(durable.get("reward"), Some(&123));
    assert_eq!(durable.get("penalty_games_added"), Some(&4));
    assert_eq!(durable.get("penalty_games_remaining"), Some(&4));
    assert_eq!(durable.get("new_balance"), Some(&1_103));
}

#[test]
fn production_sweep_names_the_claim_activated_pet_across_instances() {
    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 42;
    const OWNER: i64 = 7;

    let file = NamedTempFile::new().expect("temporary SQLite database");
    initialize_or_migrate(file.path()).expect("initialize canonical schema");
    let players = PlayerRepository::new(file.path());
    players
        .add(&NewPlayer::new(OWNER, "Pet Owner", Some(GUILD)))
        .expect("register pet owner");
    players
        .update_balance(OWNER, Some(GUILD), 1_000)
        .expect("fund pet owner");

    let mut service =
        SqlitePetCommandService::new(file.path(), SeededPetRandom::new(11), FixedClock(NOW), 20);
    let first = service
        .adopt(OWNER, Some(GUILD), "Blep", "standard")
        .unwrap()
        .pet;
    let second = service
        .adopt(OWNER, Some(GUILD), "Second", "standard")
        .unwrap()
        .pet;
    let third = service
        .adopt(OWNER, Some(GUILD), "Third", "standard")
        .unwrap()
        .pet;
    drop(service);
    let repository = PetRepository::new(file.path());
    // Only the active pet can hatch; "Second" and "Third" stay stabled eggs.
    repository
        .resolve_hatch_species(first.pet_id, Some(GUILD), "common_cama", first.hatched_at)
        .expect("resolve hatch species");

    // Instance 1 (a command path) lazily claims the starvation death, which
    // auto-activates the oldest stabled pet, "Second".
    let claim_at = first.hatched_at + 6 * DAY;
    let mut claimer = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(11),
        FixedClock(claim_at),
        20,
    );
    let _ = claimer.status(OWNER, Some(GUILD));
    drop(claimer);
    assert!(
        repository
            .get_pet_by_id(first.pet_id, Some(GUILD))
            .unwrap()
            .unwrap()
            .died_at
            .is_some(),
        "the command path must have claimed the starvation death"
    );

    // The owner voluntarily switches to "Third" before any announcement.
    repository
        .activate_pet_atomic(ActivatePetRequest {
            discord_id: OWNER,
            guild_id: Some(GUILD),
            pet_id: third.pet_id,
            cost: 25,
            cooldown_seconds: DAY,
            now: claim_at + 100,
        })
        .expect("switch to third");

    // Instance 2 announces the death: it must name the pet the claim
    // activated, not the pet that is active now.
    let mut sweeper = SqlitePetCommandService::new(
        file.path(),
        SeededPetRandom::new(99),
        FixedClock(claim_at + 200),
        20,
    );
    let outcome = sweeper.sweep().expect("announcing sweep succeeds");
    assert_eq!(outcome.deaths.len(), 1);
    let notice = &outcome.deaths[0];
    assert_eq!(notice.pet.pet_id, first.pet_id);
    assert_eq!(
        notice.activated_pet.as_ref().map(|pet| pet.pet_id),
        Some(second.pet_id)
    );
    assert_eq!(
        notice.activated_pet.as_ref().map(|pet| pet.name.as_str()),
        Some("Second")
    );
    assert_eq!(
        repository
            .get_active_pet(OWNER, Some(GUILD))
            .unwrap()
            .unwrap()
            .name,
        "Third"
    );
}
