use std::collections::VecDeque;
use std::convert::Infallible;

use cama_domain::pet::{ADULT_AGE_SECONDS, PetBase};

use super::*;

const T0: i64 = 1_800_000_000;
const OWNER_ID: i64 = 100;
const GUILD_ID: i64 = 77;

fn make_pet(age: i64) -> Pet {
    let hatched_at = T0 - age;
    Pet::new(PetBase {
        pet_id: 41,
        discord_id: OWNER_ID,
        guild_id: GUILD_ID,
        name: "Blep".to_owned(),
        species: "common_cama".to_owned(),
        adopted_at: hatched_at - 86_400,
        hatched_at,
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
        dig_work_at: hatched_at,
        aegis_used: 0,
        hatch_announced_at: None,
        died_at: None,
        death_cause: None,
        death_announced_at: None,
    })
}

fn adult_pet() -> Pet {
    make_pet(ADULT_AGE_SECONDS)
}

fn eaten_pet() -> Pet {
    let mut pet = adult_pet();
    pet.died_at = Some(T0);
    pet.death_cause = Some("eaten".to_owned());
    pet
}

#[derive(Debug)]
struct FakeRepository {
    pet: Option<Pet>,
    open_brawl: bool,
    consume_calls: Vec<EatAdultPetRequest>,
    announcement_calls: Vec<i64>,
}

impl FakeRepository {
    fn with_pet(pet: Option<Pet>) -> Self {
        Self {
            pet,
            open_brawl: false,
            consume_calls: Vec::new(),
            announcement_calls: Vec::new(),
        }
    }
}

impl PetEatingRepositoryPort for FakeRepository {
    type Error = Infallible;

    fn living_pet(
        &mut self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _now: i64,
    ) -> Result<Option<Pet>, Self::Error> {
        Ok(self.pet.clone())
    }

    fn has_open_brawl(
        &mut self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _now: i64,
    ) -> Result<bool, Self::Error> {
        Ok(self.open_brawl)
    }

    fn eat_adult_pet_atomic(
        &mut self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        request: &EatAdultPetRequest,
    ) -> Result<EatAdultPetCommit, Self::Error> {
        self.consume_calls.push(request.clone());
        let mut pet = self
            .pet
            .take()
            .expect("an edible pet must exist before the atomic call");
        pet.died_at = Some(request.now);
        pet.death_cause = Some("eaten".to_owned());
        Ok(EatAdultPetCommit {
            pet,
            activated_pet: None,
            new_balance: 1_000 + request.reward,
            penalty_games_remaining: request.penalty_games,
            deductions: cama_db::profit_deductions::ProfitDeductions {
                profit: request.reward,
                ..Default::default()
            },
        })
    }

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), Self::Error> {
        self.announcement_calls.push(pet.pet_id);
        Ok(())
    }

    fn classify_error(error: &Self::Error) -> PetEatingRepositoryFailure {
        match *error {}
    }
}

#[derive(Clone, Copy, Debug)]
struct FixedClock(i64);

impl PetEatingClock for FixedClock {
    fn now_seconds(&mut self) -> i64 {
        self.0
    }
}

#[derive(Debug)]
struct ScriptedRandom {
    values: VecDeque<i64>,
    calls: Vec<(i64, i64)>,
}

impl ScriptedRandom {
    fn new(values: impl IntoIterator<Item = i64>) -> Self {
        Self {
            values: values.into_iter().collect(),
            calls: Vec::new(),
        }
    }
}

impl PetEatingRandomPort for ScriptedRandom {
    fn inclusive_i64(&mut self, minimum: i64, maximum: i64) -> i64 {
        self.calls.push((minimum, maximum));
        self.values
            .pop_front()
            .expect("the canonical test supplies every random result")
    }
}

fn service_with_pet(
    pet: Option<Pet>,
    values: impl IntoIterator<Item = i64>,
) -> PetEatingService<FakeRepository, FixedClock, ScriptedRandom> {
    PetEatingService::new(
        FakeRepository::with_pet(pet),
        FixedClock(T0),
        ScriptedRandom::new(values),
    )
}

#[test]
fn test_rejects_petless_owner() {
    let mut service = service_with_pet(None, []);

    let result = service.eat(OWNER_ID, Some(GUILD_ID));

    assert_eq!(result.error_code(), Some(NO_PET));
    assert!(service.random().calls.is_empty());
}

#[test]
fn test_rejects_baby() {
    let mut service = service_with_pet(Some(make_pet(ADULT_AGE_SECONDS - 1)), []);

    let result = service.eat(OWNER_ID, Some(GUILD_ID));

    assert_eq!(result.error_code(), Some(PET_NOT_ADULT));
    assert!(
        result
            .error()
            .is_some_and(|error| error.contains("fully grown"))
    );
    assert!(service.random().calls.is_empty());
}

#[test]
fn test_preview_rejects_an_open_brawl() {
    let mut repository = FakeRepository::with_pet(Some(adult_pet()));
    repository.open_brawl = true;
    let mut service = PetEatingService::new(repository, FixedClock(T0), ScriptedRandom::new([]));

    let result = service.eat_preview(OWNER_ID, Some(GUILD_ID));

    assert_eq!(result.error_code(), Some(BRAWL_BUSY));
    assert!(service.repository().consume_calls.is_empty());
}

#[test]
fn test_rolls_inclusive_reward_and_penalty_ranges() {
    let mut service = service_with_pet(Some(adult_pet()), [500, 3]);

    let result = service.eat(OWNER_ID, Some(GUILD_ID));

    let outcome = result.value().expect("adult consumption should succeed");
    assert_eq!(outcome.reward, 500);
    assert_eq!(outcome.penalty_games_added, 3);
    assert_eq!(outcome.penalty_games_remaining, 3);
    assert_eq!(outcome.new_balance, 1_500);
    assert_eq!(outcome.pet.death_cause.as_deref(), Some("eaten"));
    assert_eq!(
        service.random().calls,
        vec![
            (PET_EAT_REWARD_MIN, PET_EAT_REWARD_MAX),
            (PET_EAT_PENALTY_GAMES_MIN, PET_EAT_PENALTY_GAMES_MAX),
        ]
    );
}

#[derive(Debug)]
struct FakeApplicationService {
    preview: ServiceResult<Pet>,
    eating: ServiceResult<PetEatingOutcome>,
    preview_calls: usize,
    eat_calls: usize,
    announced_pet_ids: Vec<i64>,
}

impl PetEatingApplicationPort for FakeApplicationService {
    fn eat_preview(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> ServiceResult<Pet> {
        self.preview_calls += 1;
        self.preview.clone()
    }

    fn eat(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> ServiceResult<PetEatingOutcome> {
        self.eat_calls += 1;
        self.eating.clone()
    }

    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String> {
        self.announced_pet_ids.push(pet.pet_id);
        Ok(())
    }
}

#[derive(Debug)]
struct FakeDiscord {
    initial_private: Vec<String>,
    defer_calls: usize,
    defer_succeeds: bool,
    warnings: Vec<EatingEmbed>,
    warning_buttons: Vec<Vec<&'static str>>,
    warning_succeeds: bool,
    decision: ConfirmationDecision,
    confirmation_owner_ids: Vec<i64>,
    edits: Vec<(u64, EatingEmbed, bool)>,
    edit_succeeds: bool,
    public_posts: Vec<EatingEmbed>,
    public_succeeds: bool,
    private_followups: Vec<String>,
}

impl Default for FakeDiscord {
    fn default() -> Self {
        Self {
            initial_private: Vec::new(),
            defer_calls: 0,
            defer_succeeds: true,
            warnings: Vec::new(),
            warning_buttons: Vec::new(),
            warning_succeeds: true,
            decision: ConfirmationDecision::Confirmed,
            confirmation_owner_ids: Vec::new(),
            edits: Vec::new(),
            edit_succeeds: true,
            public_posts: Vec::new(),
            public_succeeds: true,
            private_followups: Vec::new(),
        }
    }
}

impl PetEatingDiscordPort for FakeDiscord {
    type Error = ();
    type Message = u64;

    fn send_initial_private(&mut self, content: &str) -> Result<(), Self::Error> {
        self.initial_private.push(content.to_owned());
        Ok(())
    }

    fn defer_public(&mut self) -> Result<(), Self::Error> {
        self.defer_calls += 1;
        self.defer_succeeds.then_some(()).ok_or(())
    }

    fn send_warning(
        &mut self,
        embed: &EatingEmbed,
        buttons: &[ConfirmationButton],
    ) -> Result<Option<Self::Message>, Self::Error> {
        self.warnings.push(embed.clone());
        self.warning_buttons
            .push(buttons.iter().map(|button| button.label).collect());
        self.warning_succeeds.then_some(Some(7)).ok_or(())
    }

    fn await_confirmation(&mut self, owner_id: i64) -> ConfirmationDecision {
        self.confirmation_owner_ids.push(owner_id);
        self.decision
    }

    fn edit_message(
        &mut self,
        message: &Self::Message,
        embed: &EatingEmbed,
        clear_view: bool,
    ) -> Result<(), Self::Error> {
        self.edits.push((*message, embed.clone(), clear_view));
        self.edit_succeeds.then_some(()).ok_or(())
    }

    fn send_public(&mut self, embed: &EatingEmbed) -> Result<(), Self::Error> {
        self.public_posts.push(embed.clone());
        self.public_succeeds.then_some(()).ok_or(())
    }

    fn send_private_followup(&mut self, content: &str) -> Result<(), Self::Error> {
        self.private_followups.push(content.to_owned());
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeReminders {
    cancellations: Vec<(i64, i64)>,
}

impl PetEatingReminderPort for FakeReminders {
    fn cancel_pet_reminder(&mut self, discord_id: i64, guild_id: i64) {
        self.cancellations.push((discord_id, guild_id));
    }
}

fn command_outcome(pet: Pet, reward: i64, penalty_games: i64) -> PetEatingOutcome {
    PetEatingOutcome {
        pet,
        activated_pet: None,
        reward,
        penalty_games_added: penalty_games,
        penalty_games_remaining: penalty_games,
        new_balance: 1_000 + reward,
        deductions: cama_db::profit_deductions::ProfitDeductions {
            profit: reward,
            ..Default::default()
        },
    }
}

fn fake_command(
    preview: ServiceResult<Pet>,
    eating: ServiceResult<PetEatingOutcome>,
    discord: FakeDiscord,
) -> PetEatingCommand<FakeApplicationService, FakeDiscord, FakeReminders> {
    PetEatingCommand::new(
        FakeApplicationService {
            preview,
            eating,
            preview_calls: 0,
            eat_calls: 0,
            announced_pet_ids: Vec::new(),
        },
        discord,
        FakeReminders::default(),
    )
}

#[test]
fn test_validation_error_stays_private_before_public_defer() {
    let mut command = fake_command(
        ServiceResult::fail_with_code("You have no living pet to eat.", NO_PET),
        ServiceResult::fail("must not eat"),
        FakeDiscord::default(),
    );

    let result = command.run(OWNER_ID, GUILD_ID);

    assert_eq!(result, PetEatingCommandOutcome::ValidationRejected);
    assert_eq!(command.discord().defer_calls, 0);
    assert_eq!(command.discord().initial_private.len(), 1);
    assert!(command.discord().public_posts.is_empty());
    assert!(command.discord().warnings.is_empty());
}

#[test]
fn test_warning_is_ominous_but_hides_both_ranges() {
    let pet = adult_pet();
    let discord = FakeDiscord {
        decision: ConfirmationDecision::Declined,
        ..FakeDiscord::default()
    };
    let mut command = fake_command(
        ServiceResult::ok(pet),
        ServiceResult::fail("declining must not eat"),
        discord,
    );

    let result = command.run(OWNER_ID, GUILD_ID);

    assert_eq!(result, PetEatingCommandOutcome::Spared);
    let warning = &command.discord().warnings[0];
    let copy = format!("{}\n{}", warning.title, warning.description);
    assert!(copy.to_lowercase().contains("terrible hunger"));
    assert!(copy.to_lowercase().contains("tax your future earnings"));
    assert!(copy.to_lowercase().contains("cannot be undone"));
    assert!(copy.to_lowercase().contains("gone forever"));
    assert!(!copy.contains("500"));
    assert!(!copy.contains("1,000"));
    assert!(!copy.contains("3-5"));
    assert!(!copy.contains("3–5"));
    assert_eq!(command.service().eat_calls, 0);
}

#[test]
fn test_confirmation_reveals_rolls() {
    let pet = adult_pet();
    let dead = eaten_pet();
    let mut command = fake_command(
        ServiceResult::ok(pet),
        ServiceResult::ok(command_outcome(dead, 888, 4)),
        FakeDiscord::default(),
    );

    let result = command.run(OWNER_ID, GUILD_ID);

    assert!(matches!(result, PetEatingCommandOutcome::Delivered(_)));
    let (_, outcome, clear_view) = command
        .discord()
        .edits
        .last()
        .expect("confirmation should replace the warning");
    let copy = format!("{}\n{}", outcome.title, outcome.description);
    assert!(copy.contains("Blep"));
    assert!(copy.contains("888"));
    assert!(copy.contains('4'));
    assert!(copy.to_lowercase().contains("taxed"));
    assert!(*clear_view);
}

#[test]
fn test_confirmation_cleans_up_death_delivery() {
    let pet = adult_pet();
    let dead = eaten_pet();
    let mut command = fake_command(
        ServiceResult::ok(pet.clone()),
        ServiceResult::ok(command_outcome(dead.clone(), 888, 4)),
        FakeDiscord::default(),
    );

    let delivered = command.run(OWNER_ID, GUILD_ID);

    assert!(matches!(delivered, PetEatingCommandOutcome::Delivered(_)));
    assert_eq!(
        command.reminders().cancellations,
        vec![(OWNER_ID, GUILD_ID)]
    );
    assert_eq!(command.service().announced_pet_ids, vec![dead.pet_id]);
    assert!(command.active_direct_deliveries().is_empty());

    let failed_discord = FakeDiscord {
        edit_succeeds: false,
        public_succeeds: false,
        ..FakeDiscord::default()
    };
    let mut failed = fake_command(
        ServiceResult::ok(pet),
        ServiceResult::ok(command_outcome(dead, 888, 4)),
        failed_discord,
    );

    let delivery_failed = failed.run(OWNER_ID, GUILD_ID);

    assert_eq!(delivery_failed, PetEatingCommandOutcome::DeliveryFailed);
    assert_eq!(failed.reminders().cancellations, vec![(OWNER_ID, GUILD_ID)]);
    assert!(failed.service().announced_pet_ids.is_empty());
    assert!(failed.active_direct_deliveries().is_empty());
}

#[test]
fn test_confirmation_buttons_match_the_choice() {
    let labels: Vec<_> = confirmation_buttons()
        .into_iter()
        .map(|button| button.label)
        .collect();

    assert_eq!(labels, vec!["Eat them", "Let them live"]);
}

#[test]
fn test_eaten_death_copy_is_not_a_starvation_message() {
    let copy = build_death_copy(&eaten_pet());

    assert!(copy.contains("was eaten"));
    assert!(!copy.contains("starved"));
}

#[test]
fn test_petless_memorial_uses_eaten_copy() {
    let status = PetStatus {
        pet: None,
        hunger: 0,
        stage: None,
        mood: None,
        age_seconds: 0,
        supplies: None,
        last_dead: Some(eaten_pet()),
        dig_work_units: 0,
        dig_work_rate: 0,
        evolution_hint: None,
    };

    let copy = build_petless_memorial_copy(&status, "Owner", 50);

    assert!(copy.contains("was eaten"));
    assert!(!copy.contains("died of eaten"));
}
