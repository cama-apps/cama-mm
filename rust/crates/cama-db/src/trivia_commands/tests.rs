use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 9_001;
const OTHER_GUILD: i64 = 99_999;
const PLAYER: i64 = 100_001;
const NOW: i64 = 1_800_000_000;
const COOLDOWN: i64 = 21_600;

struct Fixture {
    file: NamedTempFile,
    repository: TriviaCommandsRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open fixture database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     last_trivia_session INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE trivia_sessions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     streak INTEGER NOT NULL DEFAULT 0,
                     jc_earned INTEGER NOT NULL DEFAULT 0,
                     played_at INTEGER NOT NULL
                 );
                 CREATE INDEX idx_trivia_sessions_guild_played
                 ON trivia_sessions(guild_id, played_at);
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TABLE trivia_reward_audit (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     delta INTEGER NOT NULL,
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TRIGGER players_trivia_reward_audit
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN NEW.jopacoin_balance != OLD.jopacoin_balance
                 BEGIN
                     INSERT INTO trivia_reward_audit (
                         discord_id, guild_id, delta, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         NEW.discord_id,
                         NEW.guild_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         (SELECT source FROM economy_ledger_context WHERE id = 1),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;",
            )
            .expect("create disposable trivia schema");
        drop(connection);
        let repository = TriviaCommandsRepository::new(file.path());
        Self { file, repository }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn seed(&self, discord_id: i64, guild_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     jopacoin_balance, lowest_balance_ever
                 ) VALUES (?1, ?2, ?3, 3, 3)",
                params![discord_id, guild_id, format!("Player{discord_id}")],
            )
            .expect("seed player");
    }

    fn seed_default(&self) {
        self.seed(PLAYER, GUILD);
    }

    fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.repository
            .get_balance(discord_id, Some(guild_id))
            .expect("read balance")
            .expect("seeded player")
    }

    fn record(&self, discord_id: i64, streak: i64, played_at: i64) {
        self.repository
            .record_trivia_session(discord_id, Some(GUILD), streak, streak, played_at)
            .expect("record trivia session");
    }
}

#[test]
fn test_first_session_succeeds() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
            .expect("claim cooldown")
    );
}

#[test]
fn test_second_session_blocked_by_cooldown() {
    let fixture = Fixture::new();
    fixture.seed_default();
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let repository = fixture.repository.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            repository
                .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
                .expect("concurrent cooldown claim")
        }));
    }
    barrier.wait();
    let claims = handles
        .into_iter()
        .map(|handle| handle.join().expect("claim thread"))
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().filter(|claimed| **claimed).count(), 1);
    assert!(
        !fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW + 100, COOLDOWN)
            .expect("blocked claim")
    );
}

#[test]
fn test_session_available_after_cooldown() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
            .expect("first claim")
    );
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW + COOLDOWN + 1, COOLDOWN)
            .expect("claim after cooldown")
    );
}

#[test]
fn test_get_last_trivia_session_none_initially() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert_eq!(
        fixture
            .repository
            .get_last_trivia_session(PLAYER, Some(GUILD))
            .expect("read cooldown"),
        None
    );
}

#[test]
fn test_get_last_trivia_session_after_claim() {
    let fixture = Fixture::new();
    fixture.seed_default();
    fixture
        .repository
        .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
        .expect("claim cooldown");
    assert_eq!(
        fixture
            .repository
            .get_last_trivia_session(PLAYER, Some(GUILD))
            .expect("read cooldown"),
        Some(NOW)
    );
}

#[test]
fn test_balance_increases_on_correct_answer() {
    let fixture = Fixture::new();
    fixture.seed_default();
    let initial = fixture.balance(PLAYER, GUILD);
    assert_eq!(
        fixture
            .repository
            .adjust_balance(PLAYER, Some(GUILD), 1)
            .expect("credit answer"),
        initial + 1
    );
    assert_eq!(fixture.balance(PLAYER, GUILD), initial + 1);
}

#[test]
fn test_multiple_correct_answers() {
    let fixture = Fixture::new();
    fixture.seed_default();
    let initial = fixture.balance(PLAYER, GUILD);
    for _ in 0..5 {
        fixture
            .repository
            .adjust_balance(PLAYER, Some(GUILD), 1)
            .expect("credit answer");
    }
    assert_eq!(fixture.balance(PLAYER, GUILD), initial + 5);
}

#[test]
fn test_credit_trivia_reward_preserves_trigger_context_and_lowest_balance() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert_eq!(
        fixture
            .repository
            .credit_trivia_reward(PLAYER, Some(GUILD), 2, 4)
            .expect("credit settled answer"),
        5
    );
    let connection = fixture.connection();
    let lowest: i64 = connection
        .query_row(
            "SELECT lowest_balance_ever FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("read lowest balance");
    assert_eq!(lowest, 3);
    let audit = connection
        .query_row(
            "SELECT delta, source, actor_id, related_type, related_id, reason, metadata
             FROM trivia_reward_audit",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .expect("read ledger-trigger audit");
    assert_eq!(
        audit,
        (
            2,
            "trivia".to_owned(),
            PLAYER,
            "trivia_answer".to_owned(),
            "4".to_owned(),
            "scaled trivia reward".to_owned(),
            r#"{"adjusted_reward": 2}"#.to_owned(),
        )
    );
    let contexts: i64 = connection
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .expect("count cleared contexts");
    assert_eq!(contexts, 0);
}

#[test]
fn test_cooldown_per_guild() {
    let fixture = Fixture::new();
    fixture.seed(PLAYER, GUILD);
    fixture.seed(PLAYER, OTHER_GUILD);
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
            .expect("guild A claim")
    );
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(OTHER_GUILD), NOW, COOLDOWN)
            .expect("guild B claim")
    );
    assert_eq!(
        fixture
            .repository
            .get_last_trivia_session(PLAYER, Some(GUILD))
            .expect("guild A cooldown"),
        Some(NOW)
    );
    assert_eq!(
        fixture
            .repository
            .get_last_trivia_session(PLAYER, Some(OTHER_GUILD))
            .expect("guild B cooldown"),
        Some(NOW)
    );
}

#[test]
fn test_reset_clears_cooldown() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
            .expect("claim")
    );
    assert!(
        !fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW + 100, COOLDOWN)
            .expect("blocked")
    );
    assert!(
        fixture
            .repository
            .reset_trivia_cooldown(PLAYER, Some(GUILD))
            .expect("reset")
    );
    assert!(
        fixture
            .repository
            .try_claim_trivia_session(PLAYER, Some(GUILD), NOW + 100, COOLDOWN)
            .expect("claim after reset")
    );
}

#[test]
fn test_reset_returns_false_for_missing_player() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .repository
            .reset_trivia_cooldown(999_999, Some(GUILD))
            .expect("missing reset")
    );
}

#[test]
fn test_reset_returns_false_when_no_cooldown_set() {
    let fixture = Fixture::new();
    fixture.seed_default();
    assert!(
        !fixture
            .repository
            .reset_trivia_cooldown(PLAYER, Some(GUILD))
            .expect("no-op reset")
    );
}

#[test]
fn test_get_last_trivia_session_none_after_reset() {
    let fixture = Fixture::new();
    fixture.seed_default();
    fixture
        .repository
        .try_claim_trivia_session(PLAYER, Some(GUILD), NOW, COOLDOWN)
        .expect("claim");
    fixture
        .repository
        .reset_trivia_cooldown(PLAYER, Some(GUILD))
        .expect("reset");
    assert_eq!(
        fixture
            .repository
            .get_last_trivia_session(PLAYER, Some(GUILD))
            .expect("read reset cooldown"),
        None
    );
}

#[test]
fn test_record_and_leaderboard() {
    let fixture = Fixture::new();
    fixture.seed(PLAYER, GUILD);
    fixture.seed(100_002, GUILD);
    fixture.record(PLAYER, 5, NOW);
    fixture.record(100_002, 10, NOW);
    assert_eq!(
        fixture
            .repository
            .get_trivia_leaderboard(Some(GUILD), NOW - 1, 3)
            .expect("leaderboard"),
        vec![
            TriviaLeaderboardEntry {
                discord_id: 100_002,
                best_streak: 10,
            },
            TriviaLeaderboardEntry {
                discord_id: PLAYER,
                best_streak: 5,
            },
        ]
    );
}

#[test]
fn test_leaderboard_uses_max_streak() {
    let fixture = Fixture::new();
    for streak in [3, 8, 2] {
        fixture.record(PLAYER, streak, NOW);
    }
    assert_eq!(
        fixture
            .repository
            .get_trivia_leaderboard(Some(GUILD), NOW - 1, 3)
            .expect("leaderboard"),
        vec![TriviaLeaderboardEntry {
            discord_id: PLAYER,
            best_streak: 8,
        }]
    );
}

#[test]
fn test_leaderboard_empty_when_no_sessions() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .get_trivia_leaderboard(Some(GUILD), NOW - 1, 3)
            .expect("empty leaderboard")
            .is_empty()
    );
}

#[test]
fn test_leaderboard_respects_time_window() {
    let fixture = Fixture::new();
    fixture.record(PLAYER, 20, NOW - 8 * 86_400);
    assert!(
        fixture
            .repository
            .get_trivia_leaderboard(Some(GUILD), NOW - 7 * 86_400, 3)
            .expect("windowed leaderboard")
            .is_empty()
    );
}

#[test]
fn test_leaderboard_limit() {
    let fixture = Fixture::new();
    for offset in 0..5 {
        fixture.record(200_000 + offset, offset + 1, NOW);
    }
    let leaderboard = fixture
        .repository
        .get_trivia_leaderboard(Some(GUILD), NOW - 1, 3)
        .expect("limited leaderboard");
    assert_eq!(leaderboard.len(), 3);
    assert_eq!(
        leaderboard
            .iter()
            .map(|entry| entry.best_streak)
            .collect::<Vec<_>>(),
        [5, 4, 3]
    );
}

#[test]
fn test_leaderboard_uses_discord_id_for_streak_ties() {
    let fixture = Fixture::new();
    for discord_id in [300_002, 300_001] {
        fixture.record(discord_id, 7, NOW);
    }
    let leaderboard = fixture
        .repository
        .get_trivia_leaderboard(Some(GUILD), NOW - 1, 3)
        .expect("tie leaderboard");
    assert_eq!(
        leaderboard
            .iter()
            .map(|entry| entry.discord_id)
            .collect::<Vec<_>>(),
        [300_001, 300_002]
    );
}
