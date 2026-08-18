use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const NOW: i64 = 2_000_000_000;
const ACTIVE: &str = r#"{"25": {"boss_id": "grothak", "status": "active"}}"#;
const DEFEATED: &str = r#"{"25": {"boss_id": "grothak", "status": "defeated"}}"#;
const DAMAGED: &str =
    r#"{"25": {"boss_id": "grothak", "status": "active", "hp_remaining": 3, "hp_max": 5}}"#;
const PENDING: &str = r#"{"end_depth": 50, "layer": "Stone", "offered": ["shored_passage", "old_supports", "fossil_seam"], "selected": null, "start_depth": 25}"#;
const SELECTED: &str = r#"{"end_depth": 50, "layer": "Stone", "offered": ["shored_passage", "old_supports", "fossil_seam"], "selected": "old_supports", "start_depth": 25}"#;
const OTHER_OFFER: &str = r#"{"end_depth": 50, "layer": "Stone", "offered": ["lantern_road", "old_supports", "fossil_seam"], "selected": null, "start_depth": 25}"#;

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
                 );
                 CREATE TABLE dig_boss_echoes (
                     guild_id INTEGER NOT NULL,
                     boss_id TEXT NOT NULL,
                     depth INTEGER NOT NULL,
                     killer_discord_id INTEGER NOT NULL,
                     weakened_until INTEGER NOT NULL,
                     PRIMARY KEY (guild_id, boss_id)
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
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                         COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                         COALESCE(OLD.jopacoin_balance, 0), COALESCE(NEW.jopacoin_balance, 0),
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigRouteRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
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

    fn prepare_boss(&self, discord_id: i64, guild_id: i64) {
        self.connection()
            .execute(
                "UPDATE tunnels SET depth = 24, boss_progress = ?1,
                                    stinger_curse = NULL, route_state = NULL
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![ACTIVE, discord_id, guild_id],
            )
            .expect("prepare boss");
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

fn victory_request(route_state_after: &'static str) -> BossVictoryRequest<'static> {
    BossVictoryRequest {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
        expected_depth: 24,
        expected_boss_progress: ACTIVE,
        expected_stinger_curse: TextGuard::Null,
        depth_after: 25,
        boss_progress_after: DEFEATED,
        route_state_after,
        clear_stinger_curse: true,
        jc_delta: 11,
        vanity_tax: 0,
        boss_id: "grothak",
        boss_depth: 25,
        echo_window_seconds: 86_400,
        detail_json: r#"{"boundary": 25, "won": true, "jc_delta": 11}"#,
        now: NOW,
    }
}

fn loss_request() -> BossLossRequest<'static> {
    BossLossRequest {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
        expected_depth: 24,
        expected_boss_progress: ACTIVE,
        expected_stinger_curse: TextGuard::Null,
        depth_after: 20,
        boss_progress_after: DAMAGED,
        stinger_curse_after: None,
        detail_json: r#"{"boundary": 25, "won": false}"#,
        now: NOW,
    }
}

#[test]
fn test_route_state_migration_defaults_to_null_and_is_repository_updatable() {
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

    assert_eq!(
        fixture
            .repository
            .set_route_state(ACTOR, Some(GUILD), Some(PENDING))
            .unwrap(),
        RouteStateWriteOutcome::Updated
    );
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(GUILD))
            .unwrap()
            .unwrap()
            .route_state
            .as_deref(),
        Some(PENDING)
    );
}

#[test]
fn test_route_state_transition_is_guarded_and_audited_atomically() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture
        .repository
        .set_route_state(ACTOR, Some(GUILD), Some(PENDING))
        .unwrap();
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
fn test_full_boss_victory_claim_prevents_duplicate_payout_and_offer_overwrite() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    assert_eq!(
        fixture
            .repository
            .claim_boss_victory_atomic(victory_request(PENDING))
            .unwrap(),
        BossOutcomeClaim::Claimed
    );
    assert!(matches!(
        fixture
            .repository
            .claim_boss_victory_atomic(victory_request(OTHER_OFFER))
            .unwrap(),
        BossOutcomeClaim::Conflict { .. }
    ));
    assert_eq!(fixture.balance(ACTOR, GUILD), 111);
    assert_eq!(fixture.action_count(ACTOR, GUILD, "boss_fight"), 1);
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(GUILD))
            .unwrap()
            .unwrap()
            .route_state
            .as_deref(),
        Some(PENDING)
    );
}

#[test]
fn test_boss_outcome_claim_allows_only_the_first_win_or_loss_victory() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    assert_eq!(
        fixture
            .repository
            .claim_boss_victory_atomic(victory_request(PENDING))
            .unwrap(),
        BossOutcomeClaim::Claimed
    );
    assert!(matches!(
        fixture
            .repository
            .claim_boss_loss_atomic(loss_request())
            .unwrap(),
        BossOutcomeClaim::Conflict { .. }
    ));
    let persisted = fixture
        .repository
        .tunnel(ACTOR, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(persisted.depth, 25);
    assert_eq!(persisted.boss_progress.as_deref(), Some(DEFEATED));
    assert_eq!(persisted.route_state.as_deref(), Some(PENDING));
}

#[test]
fn test_boss_outcome_claim_allows_only_the_first_win_or_loss_loss() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    assert_eq!(
        fixture
            .repository
            .claim_boss_loss_atomic(loss_request())
            .unwrap(),
        BossOutcomeClaim::Claimed
    );
    assert!(matches!(
        fixture
            .repository
            .claim_boss_victory_atomic(victory_request(PENDING))
            .unwrap(),
        BossOutcomeClaim::Conflict { .. }
    ));
    let persisted = fixture
        .repository
        .tunnel(ACTOR, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(persisted.depth, 20);
    assert_eq!(persisted.boss_progress.as_deref(), Some(DAMAGED));
    assert_eq!(persisted.route_state, None);
    assert_eq!(fixture.balance(ACTOR, GUILD), 100);
}

#[test]
fn test_concurrent_first_encounters_keep_the_first_locked_boss() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET depth = 24, boss_progress = ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![r#"{"25":"active"}"#, ACTOR, GUILD],
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["grothak", "stone_wyrm"]
        .into_iter()
        .map(|boss_id| {
            let repository = fixture.repository.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let locked = format!(r#"{{"25":{{"boss_id":"{boss_id}","status":"active"}}}}"#);
                barrier.wait();
                repository
                    .lock_boss_atomic(BossLockRequest {
                        discord_id: ACTOR,
                        guild_id: Some(GUILD),
                        expected_boss_progress: r#"{"25":"active"}"#,
                        locked_boss_progress: &locked,
                    })
                    .unwrap()
            })
        })
        .collect();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("boss-lock thread"))
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BossLockClaim::Locked { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BossLockClaim::Existing { .. }))
            .count(),
        1
    );
    let persisted = fixture
        .repository
        .tunnel(ACTOR, Some(GUILD))
        .unwrap()
        .unwrap()
        .boss_progress
        .unwrap();
    assert!(outcomes.iter().all(|outcome| match outcome {
        BossLockClaim::Locked { boss_progress } => boss_progress == &persisted,
        BossLockClaim::Existing { boss_progress } => {
            boss_progress.as_deref() == Some(persisted.as_str())
        }
        BossLockClaim::MissingTunnel => false,
    }));
}

#[test]
fn test_full_regular_boss_victory_persists_and_returns_route_offer_atomically() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET stinger_curse = ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![r#"{"drain_next_reward":true}"#, ACTOR, GUILD],
        )
        .unwrap();
    let mut request = victory_request(PENDING);
    request.expected_stinger_curse = TextGuard::Value(r#"{"drain_next_reward":true}"#);
    assert_eq!(
        fixture
            .repository
            .claim_boss_victory_atomic(request)
            .unwrap(),
        BossOutcomeClaim::Claimed
    );
    let persisted = fixture
        .repository
        .tunnel(ACTOR, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(persisted.route_state.as_deref(), Some(PENDING));
    assert_eq!(persisted.stinger_curse, None);
    let echo: (i64, i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT depth, killer_discord_id, weakened_until
             FROM dig_boss_echoes WHERE guild_id = ?1 AND boss_id = 'grothak'",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(echo, (25, ACTOR, NOW + 86_400));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE account_id = ?1 AND source = 'dig'",
                [ACTOR],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn test_bankruptcy_debuff_dig_atomic_credit_audits_vanity_tax() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    let mut request = victory_request(PENDING);
    request.jc_delta = 248;
    request.vanity_tax = 2;
    assert_eq!(
        fixture
            .repository
            .claim_boss_victory_atomic(request)
            .expect("taxed atomic Dig credit"),
        BossOutcomeClaim::Claimed
    );
    assert_eq!(fixture.balance(ACTOR, GUILD), 348);
    let ledger = fixture
        .connection()
        .prepare(
            "SELECT source, delta FROM economy_ledger_entries
             WHERE guild_id = ?1 AND account_id = ?2
               AND source IN ('dig', 'vanity_tax')
             ORDER BY ledger_id",
        )
        .expect("prepare ledger")
        .query_map(params![GUILD, ACTOR], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("query ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger");
    assert_eq!(
        ledger,
        [("dig".to_owned(), 250), ("vanity_tax".to_owned(), -2)]
    );
}

#[test]
fn unmapped_audit_failure_rolls_back_route_state_and_reward() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    fixture
        .connection()
        .execute("DROP TABLE dig_actions", [])
        .unwrap();
    assert!(matches!(
        fixture
            .repository
            .claim_boss_victory_atomic(victory_request(PENDING)),
        Err(DigRouteRepositoryError::Sqlite(_))
    ));
    let persisted = fixture
        .repository
        .tunnel(ACTOR, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(persisted.depth, 24);
    assert_eq!(persisted.boss_progress.as_deref(), Some(ACTIVE));
    assert_eq!(persisted.route_state, None);
    assert_eq!(fixture.balance(ACTOR, GUILD), 100);
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM dig_boss_echoes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn unmapped_concurrent_victory_and_loss_commit_exactly_one_outcome() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.prepare_boss(ACTOR, GUILD);
    let barrier = Arc::new(Barrier::new(2));
    let victory_repository = fixture.repository.clone();
    let victory_barrier = Arc::clone(&barrier);
    let victory = thread::spawn(move || {
        victory_barrier.wait();
        victory_repository
            .claim_boss_victory_atomic(victory_request(PENDING))
            .unwrap()
    });
    let loss_repository = fixture.repository.clone();
    let loss_barrier = Arc::clone(&barrier);
    let loss = thread::spawn(move || {
        loss_barrier.wait();
        loss_repository
            .claim_boss_loss_atomic(loss_request())
            .unwrap()
    });
    let outcomes = [victory.join().unwrap(), loss.join().unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == BossOutcomeClaim::Claimed)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BossOutcomeClaim::Conflict { .. }))
            .count(),
        1
    );
    assert_eq!(fixture.action_count(ACTOR, GUILD, "boss_fight"), 1);
}

#[test]
fn unmapped_signed_ids_and_guild_isolation_are_preserved() {
    let fixture = Fixture::new();
    let signed_actor = -9_223_372_036_854_000_000_i64;
    let signed_guild = -8_000_000_000_000_000_000_i64;
    fixture.add_actor(signed_actor, signed_guild, 40);
    fixture.add_actor(signed_actor, OTHER_GUILD, 70);
    fixture
        .repository
        .set_route_state(signed_actor, Some(signed_guild), Some(PENDING))
        .unwrap();
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

#[test]
fn unmapped_lifecycle_route_clears_are_guarded_and_guild_scoped() {
    let fixture = Fixture::new();
    fixture.add_actor(ACTOR, GUILD, 100);
    fixture.add_actor(ACTOR, OTHER_GUILD, 100);
    fixture
        .repository
        .set_route_state(ACTOR, Some(GUILD), Some(SELECTED))
        .unwrap();
    fixture
        .repository
        .set_route_state(ACTOR, Some(OTHER_GUILD), Some(SELECTED))
        .unwrap();
    let request = LifecycleRouteClearRequest {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
        expected_route_state: SELECTED,
        detail_json: r#"{"reason":"abandon"}"#,
        now: NOW,
    };
    assert_eq!(
        fixture.repository.clear_route_on_abandon(request).unwrap(),
        RouteChoiceClaim::Claimed
    );
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(GUILD))
            .unwrap()
            .unwrap()
            .route_state,
        None
    );
    assert_eq!(
        fixture
            .repository
            .tunnel(ACTOR, Some(OTHER_GUILD))
            .unwrap()
            .unwrap()
            .route_state
            .as_deref(),
        Some(SELECTED)
    );
    assert!(matches!(
        fixture.repository.clear_route_on_prestige(request).unwrap(),
        RouteChoiceClaim::Conflict {
            persisted_route_state: None
        }
    ));
}
