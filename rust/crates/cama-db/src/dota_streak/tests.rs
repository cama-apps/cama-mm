use std::path::Path;
use std::sync::{Arc, Barrier};

use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42_424;
const OTHER_GUILD: i64 = 42_425;

struct Fixture {
    database: NamedTempFile,
    repository: DotaStreakRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary Dota streak database");
        Connection::open(database.path())
            .expect("open fixture")
            .execute_batch(FIXTURE_SCHEMA)
            .expect("create disposable schema");
        let repository = DotaStreakRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open fixture")
    }

    fn player(&self, discord_id: i64, guild_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (discord_id, guild_id, discord_username)
                 VALUES (?1, ?2, ?3)",
                params![discord_id, guild_id, format!("P{discord_id}")],
            )
            .expect("insert player");
    }

    fn set_streak(&self, discord_id: i64, guild_id: i64, days: i64, date: &str) {
        self.connection()
            .execute(
                "UPDATE players SET dota_streak_days = ?1, dota_last_played_date = ?2
                 WHERE discord_id = ?3 AND guild_id = ?4",
                params![days, date, discord_id, guild_id],
            )
            .expect("prime streak");
    }
}

fn repository(path: &Path) -> DotaStreakRepository {
    DotaStreakRepository::new(path)
}

#[test]
fn test_first_play_starts_at_one() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD);
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(&[100], Some(GUILD), "2026-05-07", "2026-05-06")
            .expect("advance"),
        vec![1]
    );
    assert_eq!(
        fixture
            .repository
            .get_streak(100, Some(GUILD))
            .expect("state"),
        DotaStreakState {
            days: 1,
            last_played_date: Some("2026-05-07".to_owned())
        }
    );
}

#[test]
fn test_consecutive_day_increments() {
    let fixture = Fixture::new();
    fixture.player(101, GUILD);
    fixture
        .repository
        .advance_bulk_atomic(&[101], Some(GUILD), "2026-05-06", "2026-05-05")
        .expect("day one");
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(&[101], Some(GUILD), "2026-05-07", "2026-05-06")
            .expect("day two"),
        vec![2]
    );
}

#[test]
fn test_same_day_replay_keeps_streak_same() {
    let fixture = Fixture::new();
    fixture.player(102, GUILD);
    let first = fixture
        .repository
        .advance_bulk_atomic(&[102], Some(GUILD), "2026-05-07", "2026-05-06")
        .expect("first match");
    let replay = fixture
        .repository
        .advance_bulk_atomic(&[102], Some(GUILD), "2026-05-07", "2026-05-06")
        .expect("same-day replay");
    assert_eq!((first, replay), (vec![1], vec![1]));
}

#[test]
fn test_skipped_day_resets_to_one() {
    let fixture = Fixture::new();
    fixture.player(103, GUILD);
    fixture
        .repository
        .advance_bulk_atomic(&[103], Some(GUILD), "2026-04-01", "2026-03-31")
        .expect("old match");
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(&[103], Some(GUILD), "2026-05-07", "2026-05-06")
            .expect("gap reset"),
        vec![1]
    );
}

#[test]
fn test_unknown_player_returns_zero() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(&[99_999], Some(GUILD), "2026-05-07", "2026-05-06")
            .expect("missing player"),
        vec![0]
    );
}

#[test]
fn test_streak_is_per_guild() {
    let fixture = Fixture::new();
    fixture.player(104, GUILD);
    fixture.player(104, OTHER_GUILD);
    fixture
        .repository
        .advance_bulk_atomic(&[104], Some(GUILD), "2026-05-07", "2026-05-06")
        .expect("primary guild");
    assert_eq!(
        fixture
            .repository
            .get_streak(104, Some(OTHER_GUILD))
            .expect("other guild"),
        DotaStreakState::default()
    );
}

#[test]
fn test_bulk_preserves_order_duplicates_and_missing_players() {
    let fixture = Fixture::new();
    fixture.player(105, GUILD);
    fixture.player(106, GUILD);
    fixture
        .repository
        .advance_bulk_atomic(&[105, 106], Some(GUILD), "2026-05-06", "2026-05-05")
        .expect("day one");
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(
                &[99_999, 106, 105, 106],
                Some(GUILD),
                "2026-05-07",
                "2026-05-06",
            )
            .expect("ordered bulk"),
        vec![0, 2, 2, 2]
    );
    assert_eq!(
        fixture
            .repository
            .get_streak(105, Some(GUILD))
            .expect("105")
            .days,
        2
    );
    assert_eq!(
        fixture
            .repository
            .get_streak(106, Some(GUILD))
            .expect("106")
            .days,
        2
    );
}

#[test]
fn test_bulk_streak_is_per_guild() {
    let fixture = Fixture::new();
    fixture.player(107, GUILD);
    fixture.player(107, OTHER_GUILD);
    assert_eq!(
        fixture
            .repository
            .advance_bulk_atomic(&[107], Some(GUILD), "2026-05-07", "2026-05-06")
            .expect("guild advance"),
        vec![1]
    );
    assert_eq!(
        fixture
            .repository
            .get_streak(107, Some(OTHER_GUILD))
            .expect("other guild"),
        DotaStreakState::default()
    );
}

#[test]
fn test_concurrent_bulk_advances_increment_once() {
    let fixture = Fixture::new();
    fixture.player(108, GUILD);
    fixture
        .repository
        .advance_bulk_atomic(&[108], Some(GUILD), "2026-05-06", "2026-05-05")
        .expect("day one");
    let barrier = Arc::new(Barrier::new(3));
    let first_repository = repository(fixture.database.path());
    let second_repository = repository(fixture.database.path());
    let (first, second) = std::thread::scope(|scope| {
        let first_barrier = Arc::clone(&barrier);
        let first = scope.spawn(move || {
            first_barrier.wait();
            first_repository
                .advance_bulk_atomic(&[108], Some(GUILD), "2026-05-07", "2026-05-06")
                .expect("first concurrent advance")
        });
        let second_barrier = Arc::clone(&barrier);
        let second = scope.spawn(move || {
            second_barrier.wait();
            second_repository
                .advance_bulk_atomic(&[108], Some(GUILD), "2026-05-07", "2026-05-06")
                .expect("second concurrent advance")
        });
        barrier.wait();
        (
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        )
    });
    assert_eq!((first, second), (vec![2], vec![2]));
    assert_eq!(
        fixture
            .repository
            .get_streak(108, Some(GUILD))
            .expect("state"),
        DotaStreakState {
            days: 2,
            last_played_date: Some("2026-05-07".to_owned())
        }
    );
}

#[test]
fn test_no_blood_pact_real_repository_uses_two_connections_and_exact_ledgers() {
    let fixture = Fixture::new();
    for (discord_id, streak) in [(301, 2), (302, 6), (303, 2)] {
        fixture.player(discord_id, GUILD);
        fixture.set_streak(discord_id, GUILD, streak, "2026-05-06");
    }
    let streaks = fixture
        .repository
        .advance_bulk_atomic(&[301, 302, 303], Some(GUILD), "2026-05-07", "2026-05-06")
        .expect("first connection advances streaks");
    assert_eq!(streaks, vec![3, 7, 3]);
    let credits = [
        DotaStreakCredit {
            discord_id: 301,
            streak_days: 3,
            bonus: 1,
        },
        DotaStreakCredit {
            discord_id: 302,
            streak_days: 7,
            bonus: 3,
        },
        DotaStreakCredit {
            discord_id: 303,
            streak_days: 3,
            bonus: 1,
        },
    ];
    let outcomes = fixture
        .repository
        .credit_awards_ordered_atomic(&credits, Some(GUILD), &ProfitDeductionPolicy::none())
        .expect("second connection credits ordered awards");
    assert_eq!(
        outcomes.iter().map(|row| row.bonus).collect::<Vec<_>>(),
        vec![1, 3, 1]
    );

    let ledger = {
        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT account_id, delta, source, related_type, reason, metadata
                 FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .expect("ledger query");
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .expect("ledger rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ledger")
    };
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger[0].0, 301);
    assert_eq!(ledger[1].0, 302);
    assert_eq!(ledger[2].0, 303);
    assert_eq!(
        ledger.iter().map(|row| row.1).collect::<Vec<_>>(),
        vec![1, 3, 1]
    );
    for row in &ledger {
        assert_eq!(row.2, "match_streak");
        assert_eq!(row.3, "dota_streak");
        assert_eq!(row.4, "Dota streak match bonus");
    }
    assert_eq!(ledger[0].5, "{\"streak_days\":3,\"bonus\":1}");
    assert_eq!(ledger[1].5, "{\"streak_days\":7,\"bonus\":3}");
    assert_eq!(ledger[2].5, "{\"streak_days\":3,\"bonus\":1}");
}

#[test]
fn match_streak_credit_recovery_is_exact_once_per_player_and_match() {
    let fixture = Fixture::new();
    fixture.player(401, GUILD);
    fixture.player(402, GUILD);
    let credits = [
        DotaStreakCredit {
            discord_id: 401,
            streak_days: 7,
            bonus: 3,
        },
        DotaStreakCredit {
            discord_id: 402,
            streak_days: 3,
            bonus: 1,
        },
    ];

    let first = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            8_001,
            &ProfitDeductionPolicy::none(),
        )
        .expect("credit match streaks");
    assert!(first.iter().all(|outcome| outcome.applied));
    assert_eq!(
        first
            .iter()
            .map(|outcome| outcome.balance_after)
            .collect::<Vec<_>>(),
        vec![6, 4]
    );

    let retry = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            8_001,
            &ProfitDeductionPolicy::none(),
        )
        .expect("recover match streaks");
    assert!(retry.iter().all(|outcome| !outcome.applied));
    assert_eq!(
        retry
            .iter()
            .map(|outcome| outcome.balance_after)
            .collect::<Vec<_>>(),
        vec![6, 4]
    );
    let connection = fixture.connection();
    let persisted: (i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT jopacoin_balance FROM players
                  WHERE guild_id=?1 AND discord_id=401),
                 (SELECT jopacoin_balance FROM players
                  WHERE guild_id=?1 AND discord_id=402),
                 (SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE guild_id=?1 AND source='match_streak'
                    AND related_type='match' AND related_id='8001')",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect exact match streak state");
    assert_eq!(persisted, (6, 4, 2));

    let next_match = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            8_002,
            &ProfitDeductionPolicy::none(),
        )
        .expect("credit a distinct match");
    assert!(next_match.iter().all(|outcome| outcome.applied));
    assert_eq!(
        next_match
            .iter()
            .map(|outcome| outcome.balance_after)
            .collect::<Vec<_>>(),
        vec![9, 5]
    );
}

fn deduction_policy<'a>(
    vanity: &'a BTreeSet<i64>,
    low_priority: &'a BTreeSet<i64>,
) -> ProfitDeductionPolicy<'a> {
    ProfitDeductionPolicy {
        bankruptcy: Some(BankruptcyPenaltyPolicy {
            rate_per_game: 0.05,
        }),
        vanity_tax_rate: 0.10,
        vanity_taxable_ids: vanity,
        low_priority_tax_rate: 0.25,
        low_priority_taxable_ids: low_priority,
    }
}

fn streak_ledger(connection: &Connection, discord_id: i64) -> Vec<(String, String, i64)> {
    let mut statement = connection
        .prepare(
            "SELECT source, reason, delta FROM economy_ledger_entries
             WHERE account_id = ?1 AND source != 'balance_update' ORDER BY ledger_id",
        )
        .expect("ledger query");
    statement
        .query_map([discord_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("ledger rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger")
}

#[test]
fn match_streak_bonus_withholds_scaled_bankruptcy_penalty_and_taxes() {
    let fixture = Fixture::new();
    for discord_id in [501, 502, 503, 504] {
        fixture.player(discord_id, GUILD);
        fixture
            .connection()
            .execute(
                "UPDATE players SET jopacoin_balance = 1000
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
            )
            .expect("fund player");
    }
    for (discord_id, remaining) in [(501, 1), (502, 6)] {
        fixture
            .connection()
            .execute(
                "INSERT INTO bankruptcy_state (
                     discord_id, guild_id, penalty_games_remaining, bankruptcy_count
                 ) VALUES (?1, ?2, ?3, 1)",
                params![discord_id, GUILD, remaining],
            )
            .expect("bankruptcy row");
    }
    let credits = [501, 502, 503, 504].map(|discord_id| DotaStreakCredit {
        discord_id,
        streak_days: 30,
        bonus: 120,
    });
    let vanity = BTreeSet::from([503]);
    let empty = BTreeSet::new();
    let outcomes = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            9_001,
            &deduction_policy(&vanity, &empty),
        )
        .expect("credit taxed streaks");
    // 120 JC bonus at 5% per outstanding game: one game withholds 6, six
    // games 36; the vanity share is a flat 10%.
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| (outcome.withheld, outcome.balance_after, outcome.applied))
            .collect::<Vec<_>>(),
        vec![
            (6, 1114, true),
            (36, 1084, true),
            (12, 1108, true),
            (0, 1120, true)
        ]
    );
    let connection = fixture.connection();
    for (discord_id, expected) in [(501, 1114), (502, 1084), (503, 1108), (504, 1120)] {
        let balance: i64 = connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("balance");
        assert_eq!(balance, expected, "player {discord_id}");
    }
    assert_eq!(
        streak_ledger(&connection, 502),
        vec![
            (
                "match_streak".to_owned(),
                "Dota streak match bonus".to_owned(),
                120
            ),
            (
                "match_streak".to_owned(),
                "Dota streak match bonus bankruptcy penalty".to_owned(),
                -36
            ),
        ]
    );
    assert_eq!(
        streak_ledger(&connection, 503),
        vec![
            (
                "match_streak".to_owned(),
                "Dota streak match bonus".to_owned(),
                120
            ),
            (
                "vanity_tax".to_owned(),
                "vanity tax on JC profit".to_owned(),
                -12
            ),
        ]
    );
    assert_eq!(streak_ledger(&connection, 504).len(), 1);
    drop(connection);

    // Recovery reports the same withholding without moving balances again.
    let retry = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            9_001,
            &deduction_policy(&vanity, &empty),
        )
        .expect("recover taxed streaks");
    assert_eq!(
        retry
            .iter()
            .map(|outcome| (outcome.withheld, outcome.balance_after, outcome.applied))
            .collect::<Vec<_>>(),
        vec![
            (6, 1114, false),
            (36, 1084, false),
            (12, 1108, false),
            (0, 1120, false)
        ]
    );
}

#[test]
fn ordered_streak_awards_withhold_the_low_priority_share() {
    let fixture = Fixture::new();
    fixture.player(601, GUILD);
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance = 1000
             WHERE discord_id = 601 AND guild_id = ?1",
            [GUILD],
        )
        .expect("fund player");
    let low_priority = BTreeSet::from([601]);
    let empty = BTreeSet::new();
    let outcomes = fixture
        .repository
        .credit_awards_ordered_atomic(
            &[DotaStreakCredit {
                discord_id: 601,
                streak_days: 30,
                bonus: 120,
            }],
            Some(GUILD),
            &deduction_policy(&empty, &low_priority),
        )
        .expect("credit low-priority streak");
    assert_eq!(outcomes[0].withheld, 30);
    assert_eq!(outcomes[0].balance_after, 1090);
    let connection = fixture.connection();
    assert_eq!(
        streak_ledger(&connection, 601),
        vec![
            (
                "match_streak".to_owned(),
                "Dota streak match bonus".to_owned(),
                120
            ),
            (
                "low_priority_tax".to_owned(),
                "low priority tax on JC profit".to_owned(),
                -30
            ),
        ]
    );
}

const FIXTURE_SCHEMA: &str = "
CREATE TABLE players (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    discord_username TEXT NOT NULL,
    jopacoin_balance INTEGER NOT NULL DEFAULT 3,
    dota_streak_days INTEGER NOT NULL DEFAULT 0,
    dota_last_played_date TEXT,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (discord_id, guild_id)
);
CREATE TABLE bankruptcy_state (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    last_bankruptcy_at INTEGER,
    penalty_games_remaining INTEGER DEFAULT 0,
    bankruptcy_count INTEGER DEFAULT 0,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (discord_id, guild_id)
);
CREATE TABLE economy_ledger_context (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    source TEXT,
    actor_id INTEGER,
    related_type TEXT,
    related_id TEXT,
    reason TEXT,
    metadata TEXT
);
CREATE TABLE economy_ledger_entries (
    ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL,
    account_type TEXT NOT NULL,
    account_id INTEGER,
    delta INTEGER NOT NULL,
    balance_before INTEGER NOT NULL,
    balance_after INTEGER NOT NULL,
    source TEXT NOT NULL,
    actor_id INTEGER,
    related_type TEXT,
    related_id TEXT,
    reason TEXT,
    metadata TEXT,
    created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
);
CREATE TRIGGER trg_economy_ledger_players_update
AFTER UPDATE OF jopacoin_balance ON players
WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
BEGIN
    INSERT INTO economy_ledger_entries (
        guild_id, account_type, account_id, delta, balance_before, balance_after,
        source, actor_id, related_type, related_id, reason, metadata
    ) VALUES (
        NEW.guild_id, 'player', NEW.discord_id,
        NEW.jopacoin_balance - OLD.jopacoin_balance,
        OLD.jopacoin_balance, NEW.jopacoin_balance,
        COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
        (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
        (SELECT related_type FROM economy_ledger_context WHERE id = 1),
        (SELECT related_id FROM economy_ledger_context WHERE id = 1),
        (SELECT reason FROM economy_ledger_context WHERE id = 1),
        (SELECT metadata FROM economy_ledger_context WHERE id = 1)
    );
END;
";

#[test]
fn match_streak_recovery_reports_the_originally_debited_withholding() {
    let fixture = Fixture::new();
    fixture.player(701, GUILD);
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance = 1000
             WHERE discord_id = 701 AND guild_id = ?1",
            [GUILD],
        )
        .expect("fund player");
    fixture
        .connection()
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, penalty_games_remaining, bankruptcy_count
             ) VALUES (701, ?1, 5, 1)",
            [GUILD],
        )
        .expect("bankruptcy row");
    let credits = [DotaStreakCredit {
        discord_id: 701,
        streak_days: 30,
        bonus: 120,
    }];
    let vanity = BTreeSet::from([701]);
    let empty = BTreeSet::new();
    let first = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            9_101,
            &deduction_policy(&vanity, &empty),
        )
        .expect("credit penalized streak");
    // Five outstanding games at 5% each withhold 25% (30) plus the 10%
    // vanity share (12).
    assert_eq!(
        first
            .iter()
            .map(|outcome| (outcome.withheld, outcome.balance_after, outcome.applied))
            .collect::<Vec<_>>(),
        vec![(42, 1078, true)]
    );

    // A later win in the same settlement decrements the counter before the
    // retry re-enters; the recovered receipt must still describe the debit
    // that actually happened.
    fixture
        .connection()
        .execute(
            "UPDATE bankruptcy_state SET penalty_games_remaining = 1
             WHERE discord_id = 701 AND guild_id = ?1",
            [GUILD],
        )
        .expect("decrement penalty");
    let retry = fixture
        .repository
        .credit_match_awards_ordered_atomic(
            &credits,
            Some(GUILD),
            9_101,
            &deduction_policy(&vanity, &empty),
        )
        .expect("recover penalized streak");
    assert_eq!(
        retry
            .iter()
            .map(|outcome| (outcome.withheld, outcome.balance_after, outcome.applied))
            .collect::<Vec<_>>(),
        vec![(42, 1078, false)]
    );
    assert_eq!(streak_ledger(&fixture.connection(), 701).len(), 3);
}
