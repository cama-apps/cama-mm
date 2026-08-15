use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;

struct Fixture {
    database: NamedTempFile,
    repository: GoldenWheelRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT,
                     wins INTEGER DEFAULT 0,
                     losses INTEGER DEFAULT 0,
                     glicko_rating REAL,
                     jopacoin_balance INTEGER DEFAULT 3,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE wheel_spins (
                     spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     result INTEGER NOT NULL,
                     spin_time INTEGER NOT NULL,
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     is_bankrupt INTEGER DEFAULT 0,
                     is_golden INTEGER DEFAULT 0,
                     outcome_code TEXT,
                     is_bonus INTEGER NOT NULL DEFAULT 0,
                     event_id TEXT,
                     outcome_metadata TEXT
                 );",
            )
            .expect("fixture schema");
        let repository = GoldenWheelRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("fixture connection")
    }

    fn add_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, wins, losses,
                     glicko_rating, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, 1, 1, 1500.0, ?4)",
                params![discord_id, guild_id, format!("Player{discord_id}"), balance],
            )
            .expect("insert player");
    }
}

#[test]
fn test_get_leaderboard_bottom_empty() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .get_leaderboard_bottom(Some(GUILD), 3, 1)
            .unwrap(),
        []
    );
}

#[test]
fn test_get_leaderboard_bottom_returns_ascending_order() {
    let fixture = Fixture::new();
    fixture.add_player(101, GUILD, 503);
    fixture.add_player(102, GUILD, 53);
    fixture.add_player(103, GUILD, 13);
    fixture.add_player(999, GUILD + 1, 1);

    let bottom = fixture
        .repository
        .get_leaderboard_bottom(Some(GUILD), 3, 1)
        .unwrap();
    assert_eq!(
        bottom
            .iter()
            .map(|player| player.balance)
            .collect::<Vec<_>>(),
        [13, 53, 503]
    );
    assert!(bottom.iter().all(|player| player.guild_id == GUILD));
}

#[test]
fn test_get_leaderboard_bottom_excludes_negative_balance() {
    let fixture = Fixture::new();
    fixture.add_player(201, GUILD, -997);
    fixture.add_player(202, GUILD, 13);

    let bottom = fixture
        .repository
        .get_leaderboard_bottom(Some(GUILD), 3, 1)
        .unwrap();
    assert_eq!(
        bottom
            .iter()
            .map(|player| player.discord_id)
            .collect::<Vec<_>>(),
        [202]
    );
}

#[test]
fn test_get_total_positive_balance_empty() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .get_total_positive_balance(Some(GUILD))
            .unwrap(),
        0
    );
}

#[test]
fn test_get_total_positive_balance_sums_only_positive() {
    let fixture = Fixture::new();
    fixture.add_player(301, GUILD, 103);
    fixture.add_player(302, GUILD, 203);
    fixture.add_player(303, GUILD, -997);
    fixture.add_player(399, GUILD + 1, 50_000);

    assert_eq!(
        fixture
            .repository
            .get_total_positive_balance(Some(GUILD))
            .unwrap(),
        306
    );
}

#[test]
fn test_get_total_positive_balance_excludes_negative() {
    let fixture = Fixture::new();
    fixture.add_player(401, GUILD, -997);
    assert_eq!(
        fixture
            .repository
            .get_total_positive_balance(Some(GUILD))
            .unwrap(),
        0
    );
}

#[test]
fn test_log_wheel_spin_is_golden_column() {
    let fixture = Fixture::new();
    fixture.add_player(501, GUILD, 103);
    let spin_id = fixture
        .repository
        .log_wheel_spin(GoldenSpinLog {
            discord_id: 501,
            guild_id: Some(GUILD),
            result: 100,
            spin_time: 1_700_000_000,
            is_golden: true,
        })
        .unwrap();
    let spin = fixture
        .repository
        .get_wheel_spin(spin_id)
        .unwrap()
        .expect("stored spin");
    assert!(spin_id > 0);
    assert!(spin.is_golden);
}

#[test]
fn test_log_wheel_spin_is_golden_false_by_default() {
    let fixture = Fixture::new();
    fixture.add_player(502, GUILD, 53);
    let spin_id = fixture
        .repository
        .log_wheel_spin(GoldenSpinLog::legacy(502, Some(GUILD), 50, 1_700_000_001))
        .unwrap();
    let spin = fixture
        .repository
        .get_wheel_spin(spin_id)
        .unwrap()
        .expect("stored spin");
    assert!(!spin.is_golden);
}

#[test]
fn test_is_golden_column_exists() {
    let fixture = Fixture::new();
    let column = fixture
        .repository
        .is_golden_column_info()
        .unwrap()
        .expect("is_golden column");
    assert_eq!(column.name, "is_golden");
}

#[test]
fn test_is_golden_column_defaults_to_zero() {
    let fixture = Fixture::new();
    fixture.add_player(9_001, GUILD, 3);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO wheel_spins (guild_id, discord_id, result, spin_time)
             VALUES (?1, ?2, ?3, ?4)",
            (GUILD, 9_001, 50, 1_700_000_002_i64),
        )
        .unwrap();
    let is_golden = connection
        .query_row(
            "SELECT is_golden FROM wheel_spins WHERE discord_id = ?1",
            [9_001],
            |row| row.get::<_, Option<i64>>(0),
        )
        .unwrap();
    assert!(is_golden.is_none_or(|value| value == 0));

    let info = fixture
        .repository
        .is_golden_column_info()
        .unwrap()
        .expect("is_golden column");
    assert_eq!(info.default_value.as_deref(), Some("0"));
}
