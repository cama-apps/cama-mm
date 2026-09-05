use std::collections::BTreeSet;
use std::path::Path;

use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use cama_domain::pet::{ADULT_AGE_SECONDS, Pet};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::NamedTempFile;

use crate::pet_brawl_repository::{PetBrawlRepository, PetBrawlRepositoryError};
use crate::pet_repository::PetRepository;

use super::*;

const GUILD_ID: i64 = 123_456;
const T0: i64 = 1_800_000_000;

struct Fixture {
    file: NamedTempFile,
    repository: PetEatingRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("create disposable SQLite fixture");
        let connection = Connection::open(file.path()).expect("open disposable SQLite fixture");
        // Fixture-only DDL mirrors the relevant Python-created schema. The
        // production adapter above never creates or migrates tables.
        connection
            .execute_batch(
                r#"
                PRAGMA journal_mode=WAL;
                CREATE TABLE players (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL DEFAULT 0,
                    discord_username TEXT NOT NULL,
                    jopacoin_balance INTEGER DEFAULT 0,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (discord_id, guild_id)
                );
                CREATE TABLE bankruptcy_state (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL DEFAULT 0,
                    last_bankruptcy_at INTEGER,
                    penalty_games_remaining INTEGER NOT NULL DEFAULT 0,
                    bankruptcy_count INTEGER NOT NULL DEFAULT 0,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (discord_id, guild_id)
                );
                CREATE TABLE economy_ledger_entries (
                    ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    account_type TEXT NOT NULL,
                    account_id INTEGER,
                    delta INTEGER NOT NULL,
                    balance_before INTEGER NOT NULL,
                    balance_after INTEGER NOT NULL,
                    source TEXT NOT NULL DEFAULT 'balance_update',
                    actor_id INTEGER,
                    related_type TEXT,
                    related_id TEXT,
                    reason TEXT,
                    metadata TEXT,
                    created_at INTEGER NOT NULL
                        DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                );
                CREATE TABLE economy_ledger_context (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    source TEXT,
                    actor_id INTEGER,
                    related_type TEXT,
                    related_id TEXT,
                    reason TEXT,
                    metadata TEXT
                );
                CREATE TRIGGER trg_economy_ledger_players_update
                AFTER UPDATE OF jopacoin_balance ON players
                WHEN COALESCE(OLD.jopacoin_balance, 0)
                     != COALESCE(NEW.jopacoin_balance, 0)
                BEGIN
                    INSERT INTO economy_ledger_entries (
                        guild_id, account_type, account_id, delta,
                        balance_before, balance_after, source, actor_id,
                        related_type, related_id, reason, metadata
                    ) VALUES (
                        COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                        COALESCE(NEW.jopacoin_balance, 0)
                            - COALESCE(OLD.jopacoin_balance, 0),
                        COALESCE(OLD.jopacoin_balance, 0),
                        COALESCE(NEW.jopacoin_balance, 0),
                        COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'balance_update'),
                        (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT reason FROM economy_ledger_context WHERE id = 1),
                        (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                    );
                END;
                CREATE TABLE pets (
                    pet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL,
                    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 32),
                    species TEXT NOT NULL,
                    egg_tier TEXT NOT NULL DEFAULT 'standard',
                    adopted_at INTEGER NOT NULL,
                    hatched_at INTEGER NOT NULL,
                    adopt_fee INTEGER NOT NULL,
                    last_fed_at INTEGER NOT NULL,
                    hunger_at_last_fed INTEGER NOT NULL,
                    times_fed INTEGER NOT NULL DEFAULT 0,
                    feeds_today INTEGER NOT NULL DEFAULT 0,
                    feed_date TEXT,
                    week_consumed_jc INTEGER NOT NULL DEFAULT 0,
                    week_key TEXT,
                    prev_week_consumed_jc INTEGER NOT NULL DEFAULT 0,
                    prev_week_key TEXT,
                    pampered_until INTEGER,
                    accessory TEXT,
                    dig_work_units INTEGER NOT NULL DEFAULT 0,
                    dig_work_at INTEGER NOT NULL DEFAULT 0,
                    aegis_used INTEGER NOT NULL DEFAULT 0,
                    hatch_announced_at INTEGER,
                    died_at INTEGER,
                    death_cause TEXT,
                    death_announced_at INTEGER,
                    training_xp INTEGER NOT NULL DEFAULT 0,
                    training_str INTEGER NOT NULL DEFAULT 0,
                    training_int INTEGER NOT NULL DEFAULT 0,
                    training_dex INTEGER NOT NULL DEFAULT 0,
                    solo_training_sessions INTEGER NOT NULL DEFAULT 3,
                    solo_training_recharged_at INTEGER,
                    evolution_started_at INTEGER,
                    evolution_due_at INTEGER,
                    evolved_at INTEGER,
                    evolution_calling TEXT,
                    evolution_primary TEXT,
                    evolution_secondary TEXT,
                    evolution_announced_at INTEGER,
                    is_active INTEGER NOT NULL DEFAULT 1,
                    stabled_at INTEGER
                );
                CREATE UNIQUE INDEX idx_pets_one_active
                    ON pets(discord_id, guild_id)
                    WHERE died_at IS NULL AND is_active=1;
                CREATE TABLE pet_stable_state (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL,
                    last_voluntary_switch_at INTEGER,
                    PRIMARY KEY(discord_id,guild_id)
                );
                CREATE TABLE pet_brawls (
                    brawl_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    channel_id INTEGER NOT NULL,
                    challenger_id INTEGER NOT NULL,
                    recipient_id INTEGER NOT NULL,
                    challenger_pet_id INTEGER NOT NULL,
                    recipient_pet_id INTEGER,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL,
                    resolved_at INTEGER,
                    winner_id INTEGER,
                    winner_pet_id INTEGER,
                    loser_pet_id INTEGER,
                    rounds INTEGER,
                    winner_hunger_delta INTEGER NOT NULL DEFAULT 0,
                    loser_hunger_delta INTEGER NOT NULL DEFAULT 0,
                    wager INTEGER NOT NULL DEFAULT 0,
                    fee INTEGER NOT NULL DEFAULT 0,
                    challenger_xp_delta INTEGER NOT NULL DEFAULT 0,
                    recipient_xp_delta INTEGER NOT NULL DEFAULT 0,
                    challenger_stat_gain TEXT,
                    recipient_stat_gain TEXT,
                    personality_event_key TEXT
                );
                "#,
            )
            .expect("create Python-compatible disposable schema");
        drop(connection);
        let repository = PetEatingRepository::new(file.path());
        Self { file, repository }
    }

    fn path(&self) -> &Path {
        self.file.path()
    }

    fn connection(&self) -> Connection {
        Connection::open(self.path()).expect("open fixture connection")
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, GUILD_ID, format!("Owner {discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn insert_pet(&self, owner_id: i64, name: &str, age: i64) -> Pet {
        let hatched_at = T0 - age;
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id, guild_id, name, species, adopted_at, hatched_at,
                     adopt_fee, last_fed_at, hunger_at_last_fed,
                     dig_work_units, dig_work_at
                 ) VALUES (?1, ?2, ?3, 'common_cama', ?4, ?5, 20, ?6, 100, 0, ?5)",
                params![
                    owner_id,
                    GUILD_ID,
                    name,
                    hatched_at - 86_400,
                    hatched_at,
                    T0
                ],
            )
            .expect("seed pet");
        let pet_id = connection.last_insert_rowid();
        drop(connection);
        self.repository
            .get_pet_by_id(pet_id, Some(GUILD_ID))
            .expect("load seeded pet")
            .expect("seeded pet exists")
    }

    fn adult_pet(&self, owner_id: i64, name: &str) -> Pet {
        self.insert_pet(owner_id, name, ADULT_AGE_SECONDS)
    }

    fn stabled_pet(&self, owner_id: i64, name: &str, stabled_at: i64) -> Pet {
        let hatched_at = stabled_at - ADULT_AGE_SECONDS;
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,dig_work_units,dig_work_at,
                     is_active,stabled_at
                 ) VALUES (?1,?2,?3,'common_cama',?4,?5,20,?6,100,0,?6,0,?6)",
                params![
                    owner_id,
                    GUILD_ID,
                    name,
                    hatched_at - 86_400,
                    hatched_at,
                    stabled_at,
                ],
            )
            .expect("seed stabled pet");
        let pet_id = connection.last_insert_rowid();
        drop(connection);
        self.repository
            .get_pet_by_id(pet_id, Some(GUILD_ID))
            .expect("load stabled pet")
            .expect("stabled pet exists")
    }

    fn request(&self, pet: &Pet) -> EatAdultPetRequest {
        EatAdultPetRequest {
            discord_id: pet.discord_id,
            guild_id: Some(GUILD_ID),
            pet_id: pet.pet_id,
            expected_last_fed_at: pet.last_fed_at,
            expected_hunger: pet.hunger_at_last_fed,
            reward: 777,
            penalty_games: 4,
            now: T0,
        }
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD_ID],
                |row| row.get(0),
            )
            .expect("player balance")
    }
}

#[test]
fn test_consumption_is_atomic_and_adds_existing_penalty() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture
        .connection()
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count
             ) VALUES (100, ?1, NULL, 2, 0)",
            [GUILD_ID],
        )
        .expect("seed existing penalty");

    let outcome = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("eat adult pet atomically");

    assert_eq!(outcome.pet.died_at, Some(T0));
    assert_eq!(outcome.pet.death_cause.as_deref(), Some("eaten"));
    assert_eq!(outcome.pet.death_announced_at, None);
    assert_eq!(outcome.new_balance, 1_777);
    assert_eq!(fixture.balance(100), 1_777);
    assert_eq!(outcome.penalty_games_remaining, 6);
    let state = fixture
        .connection()
        .query_row(
            "SELECT last_bankruptcy_at, penalty_games_remaining, bankruptcy_count
               FROM bankruptcy_state WHERE discord_id = 100 AND guild_id = ?1",
            [GUILD_ID],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("bankruptcy state");
    assert_eq!(state, (None, 6, 0));
    let ledger = fixture
        .connection()
        .query_row(
            "SELECT delta, related_type, related_id,
                    json_extract(metadata, '$.pet_id'),
                    json_extract(metadata, '$.species'),
                    json_extract(metadata, '$.reward'),
                    json_extract(metadata, '$.penalty_games'),
                    json_extract(metadata, '$.penalty_games_remaining')
               FROM economy_ledger_entries
              WHERE source = 'pet' AND related_type = 'pet_eating'",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .expect("pet eating ledger row");
    assert_eq!(
        ledger,
        (
            777,
            "pet_eating".to_owned(),
            pet.pet_id.to_string(),
            pet.pet_id,
            "common_cama".to_owned(),
            777,
            4,
            6,
        )
    );
    assert_eq!(
        fixture
            .repository
            .get_eating_outcome(pet.pet_id, Some(GUILD_ID))
            .expect("durable outcome"),
        Some(DurableEatingOutcome {
            reward: 777,
            penalty_games_added: 4,
            penalty_games_remaining: 6,
            new_balance: 1_777,
            deductions: ProfitDeductions {
                profit: 777,
                ..ProfitDeductions::default()
            },
        })
    );
}

#[test]
fn test_consumption_automatically_activates_the_oldest_stabled_pet() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let eaten = fixture.adult_pet(100, "Dinner");
    let stabled = fixture.stabled_pet(100, "Backup", T0 - 600);

    let outcome = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&eaten), &ProfitDeductionPolicy::none())
        .expect("eat active pet and promote its stable mate");

    let activated = outcome.activated_pet.expect("automatic activation outcome");
    assert_eq!(activated.pet_id, stabled.pet_id);
    assert!(activated.is_active);
    assert_eq!(activated.stabled_at, None);
    assert_eq!(activated.hatched_at, stabled.hatched_at + 600);
    assert_eq!(activated.last_fed_at, stabled.last_fed_at + 600);
}

#[test]
fn test_bankruptcy_write_failure_rolls_back_pet_and_reward() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_pet_eating_penalty
             BEFORE INSERT ON bankruptcy_state
             WHEN NEW.discord_id = 100
             BEGIN SELECT RAISE(ABORT, 'penalty write failed'); END;",
        )
        .expect("install disposable failure trigger");

    let error = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect_err("bankruptcy failure must abort consumption");

    assert!(error.to_string().contains("penalty write failed"));
    assert_eq!(
        fixture
            .repository
            .get_active_pet(100, Some(GUILD_ID))
            .expect("active pet after rollback")
            .map(|active| active.pet_id),
        Some(pet.pet_id)
    );
    assert_eq!(fixture.balance(100), 1_000);
    let ledger_count = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
              WHERE source = 'pet' AND related_type = 'pet_eating'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("ledger count");
    assert_eq!(ledger_count, 0);
}

#[test]
fn test_rejects_baby() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let baby = fixture.insert_pet(100, "Blep", ADULT_AGE_SECONDS - 1);

    let error = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&baby), &ProfitDeductionPolicy::none())
        .expect_err("baby must be rejected");

    assert_eq!(error.failure(), Some(PetEatingFailure::NotAdult));
}

#[test]
fn test_rejects_stale_hunger_anchor() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    let mut request = fixture.request(&pet);
    request.expected_hunger = 99;

    let error = fixture
        .repository
        .eat_adult_pet_atomic(request, &ProfitDeductionPolicy::none())
        .expect_err("stale hunger anchor must be rejected");

    assert_eq!(error.failure(), Some(PetEatingFailure::StalePet));
}

fn deduction_policy(vanity: &BTreeSet<i64>) -> ProfitDeductionPolicy<'_> {
    static EMPTY: BTreeSet<i64> = BTreeSet::new();
    ProfitDeductionPolicy {
        bankruptcy: Some(BankruptcyPenaltyPolicy {
            rate_per_game: 0.05,
        }),
        vanity_tax_rate: 0.10,
        vanity_taxable_ids: vanity,
        low_priority_tax_rate: 0.0,
        low_priority_taxable_ids: &EMPTY,
    }
}

fn seed_penalty(fixture: &Fixture, remaining: i64) {
    fixture
        .connection()
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count
             ) VALUES (100, ?1, NULL, ?2, 1)",
            params![GUILD_ID, remaining],
        )
        .expect("seed existing penalty");
}

/// Eat a 120 JC pet as player 100 (1,000 JC, 4 penalty games added).
fn eat_for_120(fixture: &Fixture, policy: &ProfitDeductionPolicy<'_>) -> EatAdultPetOutcome {
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    let mut request = fixture.request(&pet);
    request.reward = 120;
    fixture
        .repository
        .eat_adult_pet_atomic(request, policy)
        .expect("eat adult pet")
}

#[test]
fn test_bankruptcy_share_uses_the_sentence_before_this_meal() {
    let empty = BTreeSet::new();
    let fixture = Fixture::new();
    seed_penalty(&fixture, 1);
    let outcome = eat_for_120(&fixture, &deduction_policy(&empty));
    // 1 game outstanding before the meal withholds 120 * 0.05 = 6; the 4
    // games added by the meal (5 remaining afterwards) would have taken 30.
    assert_eq!(
        outcome.deductions,
        ProfitDeductions {
            profit: 120,
            bankruptcy_penalty: 6,
            vanity_tax: 0,
            low_priority_tax: 0,
        }
    );
    assert_eq!(outcome.penalty_games_remaining, 5);
    assert_eq!(outcome.new_balance, 1_114);
    assert_eq!(fixture.balance(100), 1_114);
    let remaining: i64 = fixture
        .connection()
        .query_row(
            "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = 100",
            [],
            |row| row.get(0),
        )
        .expect("penalty games after the meal");
    assert_eq!(remaining, 5);
    let ledger: Vec<(i64, String, String)> = fixture
        .connection()
        .prepare(
            "SELECT delta, source, reason FROM economy_ledger_entries
              WHERE account_id = 100 ORDER BY ledger_id",
        )
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(
        ledger,
        vec![
            (120, "pet".to_owned(), "ate pet Blep".to_owned()),
            (
                -6,
                "pet".to_owned(),
                "ate pet Blep bankruptcy penalty".to_owned()
            ),
        ]
    );
    assert_eq!(
        fixture
            .repository
            .get_eating_outcome(outcome.pet.pet_id, Some(GUILD_ID))
            .expect("durable outcome"),
        Some(DurableEatingOutcome {
            reward: 120,
            penalty_games_added: 4,
            penalty_games_remaining: 5,
            new_balance: 1_114,
            deductions: outcome.deductions,
        })
    );

    // 6 games outstanding withhold six 5% steps (36), not the 10 left after.
    let fixture = Fixture::new();
    seed_penalty(&fixture, 6);
    let outcome = eat_for_120(&fixture, &deduction_policy(&empty));
    assert_eq!(outcome.deductions.bankruptcy_penalty, 36);
    assert_eq!(outcome.penalty_games_remaining, 10);
    assert_eq!(fixture.balance(100), 1_084);
}

#[test]
fn test_vanity_tax_applies_only_to_listed_eaters() {
    let roster = BTreeSet::from([100]);
    let fixture = Fixture::new();
    let outcome = eat_for_120(&fixture, &deduction_policy(&roster));
    assert_eq!(outcome.deductions.vanity_tax, 12);
    assert_eq!(outcome.deductions.bankruptcy_penalty, 0);
    assert_eq!(outcome.new_balance, 1_108);
    assert_eq!(fixture.balance(100), 1_108);

    let unlisted = BTreeSet::from([200]);
    let fixture = Fixture::new();
    let outcome = eat_for_120(&fixture, &deduction_policy(&unlisted));
    assert_eq!(outcome.deductions.total(), 0);
    assert_eq!(fixture.balance(100), 1_120);
}

#[test]
fn test_rejects_active_brawl() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    fixture.seed_player(200, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture.adult_pet(200, "Spit");
    PetBrawlRepository::new(fixture.path())
        .create_brawl_atomic(Some(GUILD_ID), 5, 100, 200, pet.pet_id, T0, T0 + 180, 0, 0)
        .expect("create pending brawl");

    let error = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect_err("open brawl must block eating");

    assert_eq!(error.failure(), Some(PetEatingFailure::InBrawl));
}

#[test]
fn test_brawl_cannot_be_created_after_challenger_pet_is_eaten() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    fixture.seed_player(200, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture.adult_pet(200, "Spit");
    fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("eat challenger pet");

    let error = PetBrawlRepository::new(fixture.path())
        .create_brawl_atomic(Some(GUILD_ID), 5, 100, 200, pet.pet_id, T0, T0 + 180, 0, 0)
        .expect_err("eaten challenger pet must be unavailable");

    assert!(matches!(
        error,
        PetBrawlRepositoryError::ChallengerPetUnavailable
    ));
}

#[test]
fn test_brawl_cannot_target_a_recipient_after_their_pet_is_eaten() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    fixture.seed_player(200, 1_000);
    let recipient_pet = fixture.adult_pet(100, "Blep");
    let challenger_pet = fixture.adult_pet(200, "Spit");
    fixture
        .repository
        .eat_adult_pet_atomic(
            fixture.request(&recipient_pet),
            &ProfitDeductionPolicy::none(),
        )
        .expect("eat recipient pet");

    let error = PetBrawlRepository::new(fixture.path())
        .create_brawl_atomic(
            Some(GUILD_ID),
            5,
            200,
            100,
            challenger_pet.pet_id,
            T0,
            T0 + 180,
            0,
            0,
        )
        .expect_err("recipient without living pet must be unavailable");

    assert!(matches!(
        error,
        PetBrawlRepositoryError::RecipientPetUnavailable
    ));
}

#[test]
fn test_expired_pending_brawl_does_not_block_eating() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    fixture.seed_player(200, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture.adult_pet(200, "Spit");
    PetBrawlRepository::new(fixture.path())
        .create_brawl_atomic(
            Some(GUILD_ID),
            5,
            100,
            200,
            pet.pet_id,
            T0 - 181,
            T0 - 1,
            0,
            0,
        )
        .expect("create already-expired pending brawl");

    let outcome = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("expired pending brawl does not block");

    assert_eq!(outcome.pet.death_cause.as_deref(), Some("eaten"));
}

#[test]
fn test_eaten_pet_does_not_advance_bad_luck_pity() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("eat adult pet");

    assert!(
        fixture
            .repository
            .recent_dead_species(100, Some(GUILD_ID), 10)
            .expect("pity history")
            .is_empty()
    );
}

#[test]
fn test_eating_suppresses_a_queued_evolution_announcement() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture
        .connection()
        .execute(
            "UPDATE pets SET evolution_due_at = ?1, evolved_at = ?1,
                    evolution_calling = 'guardian'
              WHERE pet_id = ?2",
            params![T0 - 1, pet.pet_id],
        )
        .expect("queue evolution announcement");

    let outcome = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("eat evolved pet");

    assert_eq!(outcome.pet.evolution_announced_at, Some(T0));
    assert!(
        fixture
            .repository
            .find_unannounced_evolution_refs(100)
            .expect("unannounced evolution refs")
            .is_empty()
    );
}

#[test]
fn test_eating_at_evolution_due_does_not_create_a_posthumous_calling() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    fixture
        .connection()
        .execute(
            "UPDATE pets SET evolution_started_at = ?1, evolution_due_at = ?2
              WHERE pet_id = ?3",
            params![T0 - ADULT_AGE_SECONDS, T0, pet.pet_id],
        )
        .expect("make evolution due now");

    let outcome = fixture
        .repository
        .eat_adult_pet_atomic(fixture.request(&pet), &ProfitDeductionPolicy::none())
        .expect("eat exactly at evolution due time");

    assert_eq!(outcome.pet.died_at, Some(T0));
    assert_eq!(outcome.pet.evolved_at, None);
    assert_eq!(outcome.pet.evolution_calling, None);
    assert_eq!(outcome.pet.evolution_primary, None);
    assert_eq!(outcome.pet.evolution_secondary, None);
    assert!(
        fixture
            .repository
            .find_unannounced_evolution_refs(100)
            .expect("no posthumous evolution")
            .is_empty()
    );
}

#[test]
fn test_sweep_reconstructs_an_undelivered_exact_outcome() {
    let fixture = Fixture::new();
    fixture.seed_player(100, 1_000);
    let pet = fixture.adult_pet(100, "Blep");
    let mut request = fixture.request(&pet);
    request.reward = 888;
    fixture
        .repository
        .eat_adult_pet_atomic(request, &ProfitDeductionPolicy::none())
        .expect("eat adult pet");

    let deaths = PetRepository::new(fixture.path())
        .get_unannounced_deaths(100)
        .expect("unannounced deaths");
    assert_eq!(deaths.len(), 1);
    assert_eq!(deaths[0].pet_id, pet.pet_id);
    assert_eq!(
        fixture
            .repository
            .get_eating_outcome(pet.pet_id, Some(GUILD_ID))
            .expect("reconstruct exact durable result"),
        Some(DurableEatingOutcome {
            reward: 888,
            penalty_games_added: 4,
            penalty_games_remaining: 4,
            new_balance: 1_888,
            deductions: ProfitDeductions {
                profit: 888,
                ..ProfitDeductions::default()
            },
        })
    );
    let announced = fixture
        .connection()
        .query_row(
            "SELECT death_announced_at FROM pets WHERE pet_id = ?1",
            [pet.pet_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .expect("read announcement state")
        .flatten();
    assert_eq!(announced, None);
}
