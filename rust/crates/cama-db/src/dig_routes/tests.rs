use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const NOW: i64 = 2_000_000_000;
const PENDING: &str = r#"{"end_depth": 50, "layer": "Stone", "offered": ["shored_passage", "old_supports", "fossil_seam"], "selected": null, "start_depth": 25}"#;
const SELECTED: &str = r#"{"end_depth": 50, "layer": "Stone", "offered": ["shored_passage", "old_supports", "fossil_seam"], "selected": "old_supports", "start_depth": 25}"#;

struct Fixture {
    _database: NamedTempFile,
    repository: DigRouteRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     jopacoin_balance INTEGER DEFAULT 3,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     depth INTEGER DEFAULT 0,
                     boss_progress TEXT,
                     stinger_curse TEXT,
                     route_state TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     actor_id INTEGER,
                     target_id INTEGER,
                     action_type TEXT NOT NULL,
                     depth_before INTEGER DEFAULT 0,
                     depth_after INTEGER DEFAULT 0,
                     jc_delta INTEGER DEFAULT 0,
                     detail TEXT,
                     created_at INTEGER NOT NULL
                 );",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigRouteRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> crate::RuntimeConnection {
        self.repository.connection().expect("runtime connection")
    }

    fn add_actor(&self, discord_id: i64, guild_id: i64, balance: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, guild_id, format!("Player{discord_id}"), balance],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO tunnels
                     (discord_id, guild_id, depth, boss_progress)
                 VALUES (?1, ?2, 0, NULL)",
                params![discord_id, guild_id],
            )
            .expect("insert tunnel");
    }

    fn set_route_state(&self, discord_id: i64, guild_id: i64, route_state: &str) {
        self.connection()
            .execute(
                "UPDATE tunnels SET route_state = ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![route_state, discord_id, guild_id],
            )
            .expect("seed route state");
    }

    fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get(0),
            )
            .expect("balance")
    }

    fn action_count(&self, discord_id: i64, guild_id: i64, action_type: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE actor_id = ?1 AND guild_id = ?2 AND action_type = ?3",
                params![discord_id, guild_id, action_type],
                |row| row.get(0),
            )
            .expect("action count")
    }
}

#[test]
fn test_route_state_defaults_to_null() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(GUILD))
            .unwrap()
            .unwrap()
            .route_state,
        None
    );
}

#[test]
fn test_route_state_transition_is_guarded_and_audited_atomically() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.set_route_state(ACTOR, GUILD, PENDING);
    let request = RouteChoiceRequest {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
        expected_route_state: PENDING,
        selected_route_state: SELECTED,
        route_id: "old_supports",
        layer: "Stone",
        detail_json: r#"{"route_id": "old_supports", "layer": "Stone"}"#,
        now: NOW,
    };
    assert_eq!(
        fixture.repository.choose_route_atomic(request).unwrap(),
        RouteChoiceClaim::Claimed
    );
    assert!(matches!(
        fixture.repository.choose_route_atomic(request).unwrap(),
        RouteChoiceClaim::Conflict { .. }
    ));
    assert_eq!(fixture.action_count(ACTOR, GUILD, "route_choice"), 1);
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(GUILD))
            .unwrap()
            .unwrap()
            .route_state
            .as_deref(),
        Some(SELECTED)
    );
}

#[test]
fn unmapped_signed_ids_and_guild_isolation_are_preserved() {
    let fixture = Fixture::new();
    let signed_actor = -9_223_372_036_854_000_000_i64;
    let signed_guild = -8_000_000_000_000_000_000_i64;
    fixture.add_actor(signed_actor, signed_guild, 40);
    fixture.add_actor(signed_actor, OTHER_GUILD, 70);
    fixture.set_route_state(signed_actor, signed_guild, PENDING);
    assert_eq!(
        fixture
            .repository
            .tunnel(signed_actor, Some(signed_guild))
            .unwrap()
            .unwrap()
            .route_state
            .as_deref(),
        Some(PENDING)
    );
    assert_eq!(
        fixture
            .repository
            .tunnel(signed_actor, Some(OTHER_GUILD))
            .unwrap()
            .unwrap()
            .route_state,
        None
    );
    assert_eq!(fixture.balance(signed_actor, signed_guild), 40);
    assert_eq!(fixture.balance(signed_actor, OTHER_GUILD), 70);
}
