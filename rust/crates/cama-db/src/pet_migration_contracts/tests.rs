use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

fn apply_python_final_shape(connection: &Connection) {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS pets (
                pet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 32),
                species TEXT NOT NULL,
                egg_tier TEXT NOT NULL DEFAULT 'standard'
                    CHECK (egg_tier IN ('standard', 'gilded')),
                adopted_at INTEGER NOT NULL,
                hatched_at INTEGER NOT NULL,
                adopt_fee INTEGER NOT NULL CHECK (adopt_fee >= 0),
                last_fed_at INTEGER NOT NULL,
                hunger_at_last_fed INTEGER NOT NULL
                    CHECK (hunger_at_last_fed BETWEEN 0 AND 100),
                dig_work_units INTEGER NOT NULL DEFAULT 0 CHECK (dig_work_units >= 0),
                dig_work_at INTEGER NOT NULL DEFAULT 0 CHECK (dig_work_at >= 0),
                hatch_announced_at INTEGER,
                died_at INTEGER,
                death_cause TEXT,
                death_announced_at INTEGER,
                training_xp INTEGER NOT NULL DEFAULT 0 CHECK (training_xp BETWEEN 0 AND 20),
                training_str INTEGER NOT NULL DEFAULT 0 CHECK (training_str BETWEEN 0 AND 2),
                training_int INTEGER NOT NULL DEFAULT 0 CHECK (training_int BETWEEN 0 AND 2),
                training_dex INTEGER NOT NULL DEFAULT 0 CHECK (training_dex BETWEEN 0 AND 2),
                solo_training_sessions INTEGER NOT NULL DEFAULT 3
                    CHECK (solo_training_sessions BETWEEN 0 AND 3),
                solo_training_recharged_at INTEGER,
                evolution_started_at INTEGER,
                evolution_due_at INTEGER,
                evolved_at INTEGER,
                evolution_calling TEXT,
                evolution_primary TEXT,
                evolution_secondary TEXT,
                evolution_announced_at INTEGER,
                CHECK (hatched_at >= adopted_at),
                CHECK (died_at IS NULL OR died_at >= adopted_at),
                CHECK (death_announced_at IS NULL OR died_at IS NOT NULL)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pets_one_alive
                ON pets(discord_id, guild_id) WHERE died_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_pets_unannounced_deaths
                ON pets(guild_id, died_at)
                WHERE died_at IS NOT NULL AND death_announced_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_pets_evolution_due
                ON pets(evolution_due_at, pet_id)
                WHERE died_at IS NULL AND evolved_at IS NULL
                AND evolution_due_at IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_pets_evolution_unannounced
                ON pets(evolved_at, pet_id)
                WHERE evolved_at IS NOT NULL AND evolution_announced_at IS NULL;

            CREATE TABLE IF NOT EXISTS pet_supplies (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                item_id TEXT NOT NULL,
                qty INTEGER NOT NULL DEFAULT 0 CHECK (qty >= 0),
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, item_id)
            );
            CREATE TABLE IF NOT EXISTS pet_refund_windows (
                guild_id INTEGER NOT NULL,
                week_key TEXT NOT NULL,
                paid_at INTEGER NOT NULL,
                total_paid INTEGER NOT NULL CHECK (total_paid >= 0),
                announcement_payload TEXT,
                announced_at INTEGER,
                PRIMARY KEY (guild_id, week_key)
            );
            CREATE TABLE IF NOT EXISTS pet_brawls (
                brawl_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                challenger_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                challenger_pet_id INTEGER NOT NULL,
                recipient_pet_id INTEGER,
                status TEXT NOT NULL DEFAULT 'pending' CHECK (
                    status IN ('pending', 'active', 'done', 'declined', 'expired', 'void')
                ),
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                resolved_at INTEGER,
                winner_id INTEGER,
                winner_pet_id INTEGER,
                loser_pet_id INTEGER,
                rounds INTEGER,
                winner_hunger_delta INTEGER NOT NULL DEFAULT 0,
                loser_hunger_delta INTEGER NOT NULL DEFAULT 0,
                wager INTEGER NOT NULL DEFAULT 0 CHECK (wager BETWEEN 0 AND 100),
                fee INTEGER NOT NULL DEFAULT 0 CHECK (fee >= 0),
                challenger_xp_delta INTEGER NOT NULL DEFAULT 0
                    CHECK (challenger_xp_delta BETWEEN 0 AND 2),
                recipient_xp_delta INTEGER NOT NULL DEFAULT 0
                    CHECK (recipient_xp_delta BETWEEN 0 AND 2),
                challenger_stat_gain TEXT CHECK (
                    challenger_stat_gain IS NULL OR challenger_stat_gain IN ('str','int','dex')
                ),
                recipient_stat_gain TEXT CHECK (
                    recipient_stat_gain IS NULL OR recipient_stat_gain IN ('str','int','dex')
                ),
                personality_event_key TEXT,
                CHECK (challenger_id != recipient_id),
                CHECK (winner_id IS NULL OR winner_id IN (challenger_id, recipient_id))
            );
            CREATE INDEX IF NOT EXISTS idx_pet_brawls_open
                ON pet_brawls(guild_id, status) WHERE status IN ('pending', 'active');
            CREATE INDEX IF NOT EXISTS idx_pet_brawls_record_win
                ON pet_brawls(winner_pet_id) WHERE status = 'done';
            CREATE INDEX IF NOT EXISTS idx_pet_brawls_record_loss
                ON pet_brawls(loser_pet_id) WHERE status = 'done';
            CREATE TABLE IF NOT EXISTS pet_evolution_daily (
                pet_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                upbringing_day INTEGER NOT NULL CHECK (upbringing_day BETWEEN 0 AND 6),
                instinct TEXT NOT NULL,
                score INTEGER NOT NULL CHECK (score BETWEEN 0 AND 3),
                first_activity TEXT NOT NULL,
                first_source_key TEXT NOT NULL,
                second_activity TEXT,
                second_source_key TEXT,
                PRIMARY KEY (pet_id, upbringing_day, instinct)
            );
            CREATE TABLE IF NOT EXISTS reminder_preferences (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                pet_enabled INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );
            CREATE INDEX IF NOT EXISTS idx_reminder_prefs_pet
                ON reminder_preferences(guild_id, pet_enabled);
            ",
        )
        .expect("apply Python final pet shape");
}

fn fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("pet migration fixture");
    let connection = Connection::open(file.path()).expect("open fixture");
    apply_python_final_shape(&connection);
    drop(connection);
    file
}

struct PetSeed<'a> {
    discord_id: i64,
    guild_id: i64,
    name: &'a str,
    hatched_at: i64,
    hunger: i64,
    died_at: Option<i64>,
    death_announced_at: Option<i64>,
}

fn pet_seed(discord_id: i64, guild_id: i64) -> PetSeed<'static> {
    PetSeed {
        discord_id,
        guild_id,
        name: "Testcama",
        hatched_at: 2_000,
        hunger: 100,
        died_at: None,
        death_announced_at: None,
    }
}

fn insert_pet(connection: &Connection, seed: PetSeed<'_>) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO pets (
            discord_id, guild_id, name, species, adopted_at, hatched_at,
            adopt_fee, last_fed_at, hunger_at_last_fed, died_at,
            death_cause, death_announced_at
         ) VALUES (?1, ?2, ?3, 'common_cama', 1000, ?4, 20, ?4, ?5, ?6,
                   CASE WHEN ?6 IS NULL THEN NULL ELSE 'starvation' END, ?7)",
        params![
            seed.discord_id,
            seed.guild_id,
            seed.name,
            seed.hatched_at,
            seed.hunger,
            seed.died_at,
            seed.death_announced_at
        ],
    )
}

#[test]
fn test_tables_and_indexes_exist() {
    let file = fixture();
    let audit = audit_existing_schema(file.path()).expect("audit final schema");
    assert!(audit.is_compatible(), "{audit:?}");
}

#[test]
fn test_migration_is_idempotent() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    apply_python_final_shape(&connection);
    apply_python_final_shape(&connection);
    drop(connection);
    assert!(
        audit_existing_schema(file.path())
            .expect("audit repeated shape")
            .is_compatible()
    );
}

#[test]
fn test_one_living_pet_per_player_per_guild() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    insert_pet(&connection, pet_seed(1, 100)).expect("first living pet");
    assert!(
        insert_pet(
            &connection,
            PetSeed {
                name: "Second",
                ..pet_seed(1, 100)
            }
        )
        .is_err()
    );
}

#[test]
fn test_dead_pet_does_not_block_re_adoption() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    insert_pet(
        &connection,
        PetSeed {
            name: "Old",
            died_at: Some(90_000),
            ..pet_seed(1, 100)
        },
    )
    .expect("dead pet");
    insert_pet(
        &connection,
        PetSeed {
            name: "New",
            ..pet_seed(1, 100)
        },
    )
    .expect("new living pet");
    insert_pet(
        &connection,
        PetSeed {
            name: "Other Guild",
            ..pet_seed(1, 200)
        },
    )
    .expect("other guild pet");
}

#[test]
fn test_check_constraints() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    assert!(
        insert_pet(
            &connection,
            PetSeed {
                name: "",
                ..pet_seed(2, 100)
            }
        )
        .is_err()
    );
    assert!(
        insert_pet(
            &connection,
            PetSeed {
                name: "Hungry",
                hunger: 101,
                ..pet_seed(3, 100)
            }
        )
        .is_err()
    );
    assert!(
        insert_pet(
            &connection,
            PetSeed {
                name: "Time",
                hatched_at: 999,
                ..pet_seed(4, 100)
            }
        )
        .is_err()
    );
    assert!(
        insert_pet(
            &connection,
            PetSeed {
                name: "Ghost",
                death_announced_at: Some(5_000),
                ..pet_seed(5, 100)
            }
        )
        .is_err()
    );
    assert!(
        connection
            .execute(
                "INSERT INTO pet_supplies
                 (discord_id,guild_id,item_id,qty,updated_at) VALUES (1,100,'tango',-1,0)",
                [],
            )
            .is_err()
    );
}

#[test]
fn test_refund_window_claim_is_unique() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO pet_refund_windows(guild_id,week_key,paid_at,total_paid)
             VALUES (100,'2026-W30',1,50)",
            [],
        )
        .expect("first claim");
    assert!(
        connection
            .execute(
                "INSERT INTO pet_refund_windows(guild_id,week_key,paid_at,total_paid)
                 VALUES (100,'2026-W30',2,60)",
                [],
            )
            .is_err()
    );
}

#[test]
fn test_refund_windows_track_pending_announcements() {
    let file = fixture();
    let columns = table_columns(
        &Connection::open(file.path()).expect("open fixture"),
        "pet_refund_windows",
    )
    .expect("columns");
    assert!(columns.contains_key("announcement_payload"));
    assert!(columns.contains_key("announced_at"));
}

#[test]
fn test_hatch_roll_state_rerolls_only_unhatched_eggs() {
    let mut rows = [
        HatchRollRow {
            species: "common_cama".to_owned(),
            egg_tier: String::new(),
            adopt_fee: 20,
            hatched_at: 4_000_000_000,
            died_at: None,
        },
        HatchRollRow {
            species: "rama".to_owned(),
            egg_tier: String::new(),
            adopt_fee: 250,
            hatched_at: 4_000_000_000,
            died_at: None,
        },
        HatchRollRow {
            species: "jopacama".to_owned(),
            egg_tier: String::new(),
            adopt_fee: 250,
            hatched_at: 2_000,
            died_at: None,
        },
    ];
    model_hatch_roll_backfill(&mut rows, 3_000);
    assert_eq!(
        (&rows[0].species, &rows[0].egg_tier),
        (&"unhatched".to_owned(), &"standard".to_owned())
    );
    assert_eq!(
        (&rows[1].species, &rows[1].egg_tier),
        (&"unhatched".to_owned(), &"gilded".to_owned())
    );
    assert_eq!(
        (&rows[2].species, &rows[2].egg_tier),
        (&"jopacama".to_owned(), &"gilded".to_owned())
    );
}

#[test]
fn test_pet_rows_persist_passive_dig_work() {
    let file = fixture();
    let columns = table_columns(
        &Connection::open(file.path()).expect("open fixture"),
        "pets",
    )
    .expect("columns");
    assert!(columns.contains_key("dig_work_units"));
    assert!(columns.contains_key("dig_work_at"));
}

#[test]
fn test_dig_work_migration_starts_existing_pets_at_migration_time() {
    assert_eq!(model_dig_work_backfill(0, 1_234_567), 1_234_567);
    assert_eq!(model_dig_work_backfill(111, 1_234_567), 111);
}

#[test]
fn test_pet_brawls_table_and_indexes() {
    let file = fixture();
    let audit = audit_existing_schema(file.path()).expect("audit brawl shape");
    assert!(
        audit
            .missing_columns
            .iter()
            .all(|column| !column.starts_with("pet_brawls."))
    );
    for index in [
        "idx_pet_brawls_open",
        "idx_pet_brawls_record_win",
        "idx_pet_brawls_record_loss",
    ] {
        assert!(!audit.missing_indexes.contains(&index.to_owned()));
    }
}

#[test]
fn test_pet_brawls_check_constraints() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    let insert = |challenger, recipient, status: &str, winner: Option<i64>| {
        connection.execute(
            "INSERT INTO pet_brawls (
                guild_id,channel_id,challenger_id,recipient_id,challenger_pet_id,
                status,created_at,expires_at,winner_id
             ) VALUES (100,5,?1,?2,10,?3,1000,1300,?4)",
            params![challenger, recipient, status, winner],
        )
    };
    insert(1, 2, "pending", None).expect("valid brawl");
    assert!(insert(3, 3, "pending", None).is_err());
    assert!(insert(4, 5, "brewing", None).is_err());
    assert!(insert(6, 7, "done", Some(8)).is_err());
}

#[test]
fn test_pet_brawls_migration_is_idempotent() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    apply_python_final_shape(&connection);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pet_brawls'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("table count"),
        1
    );
}

#[test]
fn test_training_and_wager_columns_default_for_legacy_rows() {
    let file = fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    insert_pet(
        &connection,
        PetSeed {
            name: "Train",
            ..pet_seed(30, 100)
        },
    )
    .expect("pet");
    connection
        .execute(
            "INSERT INTO pet_brawls (
                guild_id,channel_id,challenger_id,recipient_id,challenger_pet_id,
                status,created_at,expires_at
             ) VALUES (100,5,30,31,10,'pending',1000,1300)",
            [],
        )
        .expect("brawl");
    let pet = connection
        .query_row(
            "SELECT training_xp,training_str,training_int,training_dex,
                    solo_training_sessions,solo_training_recharged_at
             FROM pets WHERE discord_id=30 AND guild_id=100",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .expect("pet defaults");
    assert_eq!(pet, (0, 0, 0, 0, 3, None));
    let brawl = connection
        .query_row(
            "SELECT wager,fee,challenger_xp_delta,recipient_xp_delta,
                    challenger_stat_gain,recipient_stat_gain,personality_event_key
             FROM pet_brawls WHERE challenger_id=30",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .expect("brawl defaults");
    assert_eq!(brawl, (0, 0, 0, 0, None, None, None));
}

#[test]
fn test_pet_evolution_schema_is_bounded_and_due_lookup_is_indexed() {
    let file = fixture();
    let audit = audit_existing_schema(file.path()).expect("audit evolution shape");
    assert!(audit.is_compatible(), "{audit:?}");
    let connection = Connection::open(file.path()).expect("open fixture");
    assert!(
        connection
            .execute(
                "INSERT INTO pet_evolution_daily (
                    pet_id,guild_id,upbringing_day,instinct,score,
                    first_activity,first_source_key
                 ) VALUES (1,100,7,'social',1,'tip','one')",
                [],
            )
            .is_err()
    );
}

#[test]
fn test_pet_evolution_migration_grandfathers_only_living_pets() {
    let now = 10_000;
    let old_living = model_evolution_backfill(
        EvolutionBackfill {
            hatched_at: 2_000,
            died_at: None,
            evolution_started_at: None,
            evolution_due_at: None,
        },
        now,
    );
    assert_eq!(old_living.evolution_started_at, Some(now));
    assert_eq!(old_living.evolution_due_at, Some(now + ADULT_AGE_SECONDS));
    let future = model_evolution_backfill(
        EvolutionBackfill {
            hatched_at: 20_000,
            died_at: None,
            evolution_started_at: None,
            evolution_due_at: None,
        },
        now,
    );
    assert_eq!(future.evolution_started_at, Some(20_000));
    let dead = model_evolution_backfill(
        EvolutionBackfill {
            hatched_at: 2_000,
            died_at: Some(3_000),
            evolution_started_at: None,
            evolution_due_at: None,
        },
        now,
    );
    assert_eq!(dead.evolution_started_at, None);
    assert_eq!(dead.evolution_due_at, None);
}

#[test]
fn test_pet_evolution_migration_is_idempotent() {
    let once = model_evolution_backfill(
        EvolutionBackfill {
            hatched_at: 2_000,
            died_at: None,
            evolution_started_at: None,
            evolution_due_at: None,
        },
        10_000,
    );
    assert_eq!(model_evolution_backfill(once, 20_000), once);
}

#[test]
fn test_reminder_preferences_pet_column() {
    let file = fixture();
    let columns = table_columns(
        &Connection::open(file.path()).expect("open fixture"),
        "reminder_preferences",
    )
    .expect("columns");
    assert!(columns.contains_key("pet_enabled"));
}
