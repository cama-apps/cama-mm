use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = -71_001;
const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;

struct Fixture {
    _file: NamedTempFile,
    repository: DigPrestige4Repository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("fixture connection");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL,
                     jopacoin_balance INTEGER NOT NULL, updated_at TEXT,
                     PRIMARY KEY(discord_id,guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL,
                     depth INTEGER, max_depth INTEGER, prestige_level INTEGER,
                     boss_progress TEXT,
                     PRIMARY KEY(discord_id,guild_id)
                 );
                 CREATE TABLE dig_artifacts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL,
                     artifact_id TEXT NOT NULL, found_at INTEGER NOT NULL,
                     is_relic INTEGER NOT NULL DEFAULT 0,
                     equipped INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE dig_active_duels (
                     discord_id INTEGER NOT NULL, guild_id INTEGER NOT NULL,
                     boss_id TEXT NOT NULL, tier INTEGER NOT NULL,
                     mechanic_id TEXT NOT NULL, risk_tier TEXT NOT NULL,
                     wager INTEGER NOT NULL, player_hp INTEGER NOT NULL,
                     boss_hp INTEGER NOT NULL, round_num INTEGER NOT NULL,
                     round_log TEXT NOT NULL, pending_prompt TEXT,
                     rng_state TEXT NOT NULL, status_effects TEXT NOT NULL,
                     echo_applied INTEGER NOT NULL, echo_killer_id INTEGER,
                     player_hit REAL NOT NULL, player_dmg INTEGER NOT NULL,
                     boss_hit REAL NOT NULL, boss_dmg INTEGER NOT NULL,
                     crit_chance REAL NOT NULL DEFAULT 0,
                     crit_bonus INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL, last_interaction_at INTEGER NOT NULL,
                     PRIMARY KEY(discord_id,guild_id)
                 );
                 CREATE TABLE dig_boss_echoes (
                     guild_id INTEGER NOT NULL, boss_id TEXT NOT NULL,
                     depth INTEGER NOT NULL, killer_discord_id INTEGER NOT NULL,
                     weakened_until INTEGER NOT NULL,
                     PRIMARY KEY(guild_id,boss_id)
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL, actor_id INTEGER NOT NULL,
                     target_id INTEGER, action_type TEXT NOT NULL,
                     depth_before INTEGER NOT NULL, depth_after INTEGER NOT NULL,
                     jc_delta INTEGER NOT NULL, detail TEXT, created_at INTEGER NOT NULL
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY, source TEXT, actor_id INTEGER,
                     related_type TEXT, related_id TEXT, reason TEXT, metadata TEXT
                 );
                 CREATE TABLE economy_ledger_entries (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, guild_id INTEGER,
                     account_id INTEGER, delta INTEGER, source TEXT
                 );
                 CREATE TRIGGER ledger_update AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance BEGIN
                     INSERT INTO economy_ledger_entries(guild_id,account_id,delta,source)
                     VALUES(NEW.guild_id,NEW.discord_id,
                            NEW.jopacoin_balance-OLD.jopacoin_balance,
                            COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'));
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        Self {
            repository: DigPrestige4Repository::new(file.path()),
            _file: file,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self._file.path()).expect("fixture connection")
    }

    fn seed(&self, guild_id: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players VALUES(?1,?2,5000,NULL)",
                params![ACTOR, guild_id],
            )
            .expect("player");
        connection
            .execute(
                "INSERT INTO tunnels VALUES(?1,?2,150,150,4,?3)",
                params![
                    ACTOR,
                    guild_id,
                    r#"{"150":{"boss_id":"blightcoil","status":"phase1_defeated"}}"#
                ],
            )
            .expect("tunnel");
    }

    fn seed_duel(&self, guild_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_active_duels
                 (discord_id,guild_id,boss_id,tier,mechanic_id,risk_tier,wager,
                  player_hp,boss_hp,round_num,round_log,pending_prompt,rng_state,
                  status_effects,echo_applied,echo_killer_id,player_hit,player_dmg,
                  boss_hit,boss_dmg,crit_chance,crit_bonus,created_at,last_interaction_at)
                 VALUES(?1,?2,'blightcoil',150,'blightcoil_wards','cautious',0,
                        5,1,3,'[]',NULL,'','{}',0,NULL,0.9,1,0.0,1,0,0,1,1)",
                params![ACTOR, guild_id],
            )
            .expect("duel");
    }
}

fn key(guild_id: Option<i64>) -> PrestigeContentKey {
    PrestigeContentKey {
        discord_id: ACTOR,
        guild_id,
    }
}

#[test]
fn unique_grant_is_idempotent_and_guild_scoped() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    fixture.seed(OTHER_GUILD);
    assert!(matches!(
        fixture
            .repository
            .grant_artifact_unique(key(Some(GUILD)), "weeping_fang", true, 1)
            .expect("grant"),
        ArtifactGrantOutcome::Granted { .. }
    ));
    assert_eq!(
        fixture
            .repository
            .grant_artifact_unique(key(Some(GUILD)), "weeping_fang", true, 2)
            .expect("dedup"),
        ArtifactGrantOutcome::AlreadyOwned
    );
    assert!(
        fixture
            .repository
            .artifacts(key(Some(OTHER_GUILD)))
            .expect("other guild")
            .is_empty()
    );
}

#[test]
fn equip_refuses_legacy_duplicate_of_equipped_relic() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    let first = fixture
        .repository
        .insert_artifact_unchecked(key(Some(GUILD)), "mole_claws", true, 1)
        .expect("first");
    let second = fixture
        .repository
        .insert_artifact_unchecked(key(Some(GUILD)), "mole_claws", true, 2)
        .expect("second");
    assert_eq!(
        fixture
            .repository
            .equip_relic_unique(key(Some(GUILD)), first, 1, 7)
            .expect("equip"),
        EquipRelicOutcome::Equipped
    );
    assert_eq!(
        fixture
            .repository
            .equip_relic_unique(key(Some(GUILD)), second, 1, 7)
            .expect("duplicate"),
        EquipRelicOutcome::DuplicateAlreadyEquipped
    );
}

#[test]
fn test_equip_relic_is_scoped_to_owner() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    let owner_key = key(Some(GUILD));
    let foreign_key = PrestigeContentKey {
        discord_id: ACTOR - 1,
        guild_id: Some(GUILD),
    };
    let relic_id = fixture
        .repository
        .insert_artifact_unchecked(owner_key, "mole_claws", true, 1)
        .expect("insert owner relic");

    assert_eq!(
        fixture
            .repository
            .equip_relic_unique(foreign_key, relic_id, 1, 7)
            .expect("foreign equip is safe"),
        EquipRelicOutcome::Missing
    );
    assert!(
        !fixture
            .repository
            .artifacts(owner_key)
            .expect("owner relics")[0]
            .equipped
    );

    assert_eq!(
        fixture
            .repository
            .equip_relic_unique(owner_key, relic_id, 1, 7)
            .expect("owner equips relic"),
        EquipRelicOutcome::Equipped
    );
    assert!(
        fixture
            .repository
            .artifacts(owner_key)
            .expect("owner relics")[0]
            .equipped
    );

    assert!(
        !fixture
            .repository
            .unequip_relic_owned(foreign_key, relic_id)
            .expect("foreign unequip is safe")
    );
    assert!(
        fixture
            .repository
            .artifacts(owner_key)
            .expect("owner relics")[0]
            .equipped
    );
    assert!(
        fixture
            .repository
            .unequip_relic_owned(owner_key, relic_id)
            .expect("owner unequips relic")
    );
    assert!(
        !fixture
            .repository
            .artifacts(owner_key)
            .expect("owner relics")[0]
            .equipped
    );
}

#[test]
fn active_duel_claim_is_exactly_once_under_concurrency() {
    let fixture = Arc::new(Fixture::new());
    fixture.seed(GUILD);
    fixture.seed_duel(GUILD);
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                fixture.repository.claim_active_duel(key(Some(GUILD)))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker").expect("claim"))
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
}

#[test]
fn test_claim_active_duel_consumes_row_exactly_once() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    fixture.seed_duel(GUILD);

    let first = fixture
        .repository
        .claim_active_duel(key(Some(GUILD)))
        .expect("first duel claim")
        .expect("paused duel exists");
    assert_eq!(first.wager, 0);
    assert_eq!(first.boss_id, "blightcoil");
    assert_eq!(
        fixture
            .repository
            .claim_active_duel(key(Some(GUILD)))
            .expect("second duel claim"),
        None
    );
}

#[test]
fn test_concurrent_claims_exactly_one_wins() {
    let fixture = Arc::new(Fixture::new());
    fixture.seed(GUILD);
    fixture.seed_duel(GUILD);
    let barrier = Arc::new(Barrier::new(6));
    let handles = (0..5)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                fixture.repository.claim_active_duel(key(Some(GUILD)))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker").expect("claim"))
        .collect::<Vec<_>>();
    let winners = claims.iter().flatten().collect::<Vec<_>>();
    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].boss_id, "blightcoil");
    assert_eq!(winners[0].wager, 0);
}

#[test]
fn victory_boundary_commits_before_separate_trophy_insert() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    let snapshot = fixture
        .repository
        .tunnel_snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    let result = fixture
        .repository
        .complete_boss_victory(BossVictoryRequest {
            expected: &snapshot,
            boss_id: "blightcoil",
            depth_after: 150,
            max_depth_after: 150,
            boss_progress_after: r#"{"150":{"boss_id":"blightcoil","status":"defeated"}}"#,
            balance_delta: 20,
            echo_window_seconds: 86_400,
            detail_json: r#"{"won":true}"#,
            now: 1_000,
        })
        .expect("victory");
    assert_eq!(
        result,
        BossVictoryOutcome::Applied {
            balance_after: 5_020
        }
    );
    assert!(
        fixture
            .repository
            .artifacts(key(Some(GUILD)))
            .expect("artifacts before drop")
            .is_empty()
    );
    fixture
        .repository
        .grant_artifact_unique(key(Some(GUILD)), "weeping_fang", true, 1_001)
        .expect("separate carve");
    assert_eq!(
        fixture
            .repository
            .artifacts(key(Some(GUILD)))
            .expect("artifacts")
            .len(),
        1
    );
}

#[test]
fn stale_victory_snapshot_rolls_back_wallet_echo_and_audit() {
    let fixture = Fixture::new();
    fixture.seed(GUILD);
    let snapshot = fixture
        .repository
        .tunnel_snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET depth=149 WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("stale mutation");
    let result = fixture
        .repository
        .complete_boss_victory(BossVictoryRequest {
            expected: &snapshot,
            boss_id: "blightcoil",
            depth_after: 150,
            max_depth_after: 150,
            boss_progress_after: "{}",
            balance_delta: 20,
            echo_window_seconds: 86_400,
            detail_json: "{}",
            now: 1_000,
        })
        .expect("conflict");
    assert_eq!(result, BossVictoryOutcome::Conflict);
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        5_000
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        0
    );
}

#[test]
fn signed_ids_and_none_guild_normalize_without_cross_guild_writes() {
    let fixture = Fixture::new();
    fixture.seed(0);
    fixture.seed(OTHER_GUILD);
    fixture
        .repository
        .grant_artifact_unique(key(None), "aching_spine", true, 1)
        .expect("global grant");
    assert_eq!(
        fixture
            .repository
            .artifacts(key(None))
            .expect("global artifacts")
            .len(),
        1
    );
    assert!(
        fixture
            .repository
            .artifacts(key(Some(OTHER_GUILD)))
            .expect("other artifacts")
            .is_empty()
    );
}
