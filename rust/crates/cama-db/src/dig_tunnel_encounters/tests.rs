use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const DIGGER: i64 = 10_001;
const NOW: i64 = 2_000_000_000;
const RECENT_ISO: &str = "2033-05-18T03:33:20+00:00";

struct Fixture {
    _database: NamedTempFile,
    repository: DigTunnelEncounterRepository,
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
                     jopacoin_balance INTEGER NOT NULL DEFAULT 0,
                     last_match_date TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     depth INTEGER NOT NULL DEFAULT 0,
                     max_depth INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     actor_id INTEGER NOT NULL,
                     target_id INTEGER,
                     action_type TEXT NOT NULL,
                     depth_before INTEGER NOT NULL DEFAULT 0,
                     depth_after INTEGER NOT NULL DEFAULT 0,
                     jc_delta INTEGER NOT NULL DEFAULT 0,
                     detail TEXT,
                     created_at INTEGER NOT NULL
                 );",
            )
            .expect("fixture schema");
        drop(connection);
        Self {
            repository: DigTunnelEncounterRepository::new(database.path()),
            _database: database,
        }
    }

    fn connection(&self) -> crate::RuntimeConnection {
        self.repository.connection().expect("fixture connection")
    }

    fn add_player(
        &self,
        discord_id: i64,
        guild_id: i64,
        balance: i64,
        last_match_date: Option<&str>,
    ) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     jopacoin_balance, last_match_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    discord_id,
                    guild_id,
                    format!("User{discord_id}"),
                    balance,
                    last_match_date
                ],
            )
            .expect("insert player");
    }

    fn add_tunnel(&self, discord_id: i64, guild_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, max_depth)
                 VALUES (?1, ?2, ?3, ?3)",
                params![discord_id, guild_id, depth],
            )
            .expect("insert tunnel");
    }

    fn log_dig(&self, discord_id: i64, guild_id: i64, created_at: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, 'dig', 0, 1, 0, '{}', ?3)",
                params![guild_id, discord_id, created_at],
            )
            .expect("insert dig action");
    }
}

fn candidate_query(
    strategy: EncounterVictimStrategy,
    limit: Option<usize>,
) -> EncounterCandidateQuery {
    EncounterCandidateQuery {
        guild_id: Some(GUILD),
        digger_discord_id: DIGGER,
        strategy,
        active_since: NOW - 14 * 86_400,
        limit,
    }
}

#[test]
fn test_richest_and_deepest_candidates_rank_others_and_exclude_the_digger() {
    let fixture = Fixture::new();
    for (id, balance, depth) in [
        (DIGGER, 9_000, 500),
        (10_002, 100, 10),
        (10_003, 200, 200),
        (10_004, 300, 50),
        (10_005, 400, 0),
    ] {
        fixture.add_player(id, GUILD, balance, None);
        fixture.add_tunnel(id, GUILD, depth);
    }
    fixture.add_player(10_006, OTHER_GUILD, 50_000, None);
    fixture.add_tunnel(10_006, OTHER_GUILD, 900);

    let candidates = |strategy| {
        fixture
            .repository
            .candidate_ids(candidate_query(strategy, Some(2)))
            .unwrap()
    };
    assert_eq!(
        candidates(EncounterVictimStrategy::RichestN),
        vec![10_005, 10_004]
    );
    assert_eq!(
        candidates(EncounterVictimStrategy::DeepestN),
        vec![10_003, 10_004]
    );
}

#[test]
fn test_active_digger_candidates_are_recent_guild_diggers() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, None);
    fixture.log_dig(DIGGER, GUILD, NOW);
    for id in [10_002, 10_003, 10_004] {
        fixture.add_player(id, GUILD, 100, None);
        fixture.log_dig(id, GUILD, NOW);
    }
    fixture.add_player(10_005, GUILD, 100, None);
    fixture.log_dig(10_005, GUILD, NOW - 8 * 86_400);
    fixture.add_player(10_002, OTHER_GUILD, 500, None);
    fixture.log_dig(10_002, OTHER_GUILD, NOW);
    let mut query = candidate_query(EncounterVictimStrategy::ActiveDiggers, Some(20));
    query.active_since = NOW - 7 * 86_400;

    let candidates = fixture.repository.candidate_ids(query).unwrap();

    assert_eq!(candidates, vec![10_002, 10_003, 10_004]);
}

#[test]
fn test_random_active_candidates_are_recent_guild_players() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, Some(RECENT_ISO));
    for id in [10_002, 10_003, 10_004] {
        fixture.add_player(id, GUILD, 100, Some(RECENT_ISO));
    }
    fixture.add_player(10_005, GUILD, 100, Some("2020-01-01T00:00:00+00:00"));
    fixture.add_player(10_002, OTHER_GUILD, 900, Some(RECENT_ISO));

    let candidates = fixture
        .repository
        .candidate_ids(candidate_query(EncounterVictimStrategy::RandomActive, None))
        .unwrap();

    assert_eq!(candidates, vec![10_002, 10_003, 10_004]);
}
