use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const GUILD: i64 = 4242;
const NOW: i64 = 10_000;

static FIXTURE_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

fn fixture() -> NamedTempFile {
    let template = FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
        let file = NamedTempFile::new().expect("temporary betting database template");
        let connection = Connection::open(file.path()).expect("open fixture template");
        connection
            .execute_batch(
                "CREATE TABLE players (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 jopacoin_balance INTEGER DEFAULT 3,
                 updated_at TEXT,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE pending_matches (
                 pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL,
                 payload TEXT NOT NULL,
                 created_at TEXT,
                 updated_at TEXT
             );
             CREATE TABLE bets (
                 bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 match_id INTEGER,
                 discord_id INTEGER NOT NULL,
                 team_bet_on TEXT NOT NULL,
                 amount INTEGER NOT NULL,
                 bet_time INTEGER NOT NULL,
                 created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                 leverage INTEGER DEFAULT 1,
                 payout INTEGER,
                 is_blind INTEGER DEFAULT 0,
                 odds_at_placement REAL,
                 pending_match_id INTEGER,
                 investment_target_id INTEGER,
                 investment_direction TEXT
             );
             CREATE TABLE matches (
                 match_id INTEGER PRIMARY KEY,
                 winning_team INTEGER NOT NULL
             );
             CREATE TABLE manashop_buffs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 buff_type TEXT NOT NULL,
                 target_id INTEGER,
                 granted_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 triggered INTEGER NOT NULL DEFAULT 0,
                 data TEXT
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
             CREATE TABLE autobet_investments (
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 investor_id INTEGER NOT NULL,
                 target_id INTEGER NOT NULL,
                 direction TEXT NOT NULL,
                 percentage INTEGER NOT NULL,
                 created_at TEXT,
                 updated_at TEXT,
                 PRIMARY KEY (guild_id, investor_id, target_id)
             );",
            )
            .expect("create disposable schema template");
        drop(connection);
        std::fs::read(file.path()).expect("read betting database template")
    });
    let mut file = NamedTempFile::new().expect("temporary betting database");
    file.as_file_mut()
        .write_all(template)
        .expect("copy betting database template");
    file
}

fn repo(path: &Path) -> BettingServiceRepository {
    BettingServiceRepository::new(path)
}

fn add_player(path: &Path, discord_id: i64, balance: i64) {
    add_player_in_guild(path, discord_id, GUILD, balance);
}

fn add_player_in_guild(path: &Path, discord_id: i64, guild_id: i64, balance: i64) {
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO players (discord_id, guild_id, jopacoin_balance)
             VALUES (?1, ?2, ?3)",
            params![discord_id, guild_id, balance],
        )
        .expect("insert player");
}

fn add_counter_columns(path: &Path) {
    Connection::open(path)
        .expect("open fixture")
        .execute_batch(
            "ALTER TABLE players ADD COLUMN total_bets_placed INTEGER DEFAULT 0;
             ALTER TABLE players ADD COLUMN first_leverage_used INTEGER DEFAULT 0;",
        )
        .expect("add durable betting counters");
}

fn add_pending(path: &Path, pending_match_id: i64, is_draft: bool) {
    let payload = format!(
        "{{\"radiant_team_ids\":[1,2,3,4,5],\"dire_team_ids\":[6,7,8,9,10],\"bet_lock_until\":{},\"is_draft\":{is_draft}}}",
        NOW + 1_000
    );
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (?1, ?2, ?3)",
            params![pending_match_id, GUILD, payload],
        )
        .expect("insert pending match");
}

fn request(
    pending_match_id: i64,
    discord_id: i64,
    team: BettingTeam,
    amount: i64,
) -> PlaceBetRequest {
    PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id,
        discord_id,
        team,
        amount,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    }
}

fn balance(path: &Path, discord_id: i64) -> i64 {
    Connection::open(path)
        .expect("open fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, GUILD],
            |row| row.get(0),
        )
        .expect("player balance")
}

fn add_pending_payload(path: &Path, pending_match_id: i64, payload: &str) {
    Connection::open(path)
        .expect("open fixture")
        .execute(
            "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (?1, ?2, ?3)",
            params![pending_match_id, GUILD, payload],
        )
        .expect("insert custom pending match");
}

fn migrated_concurrent_fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary migrated concurrent database");
    initialize_or_migrate(file.path()).expect("migrate concurrent database");
    let connection = Connection::open(file.path()).expect("open migrated concurrent database");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("disable unrelated legacy fixture foreign keys");
    for pending_match_id in [11_i64, 12_i64] {
        let payload = format!(
            "{{\"radiant_team_ids\":[1,2,3,4,5],\"dire_team_ids\":[6,7,8,9,10],\"bet_lock_until\":{}}}",
            NOW + 1_000
        );
        connection
            .execute(
                "INSERT INTO pending_matches(pending_match_id,guild_id,payload)
                 VALUES (?1,?2,?3)",
                params![pending_match_id, GUILD, payload],
            )
            .expect("insert migrated pending match");
    }
    connection
        .execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected)
             VALUES (?1,0)",
            [GUILD],
        )
        .expect("insert migrated nonprofit fund");
    for discord_id in [3_001_i64, 3_002, 3_101, 3_201, 3_202, 3_301, 3_302] {
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![
                    discord_id,
                    GUILD,
                    format!("concurrent-{discord_id}"),
                    200_i64
                ],
            )
            .expect("insert migrated concurrent bettor");
    }
    connection
        .execute(
            "INSERT INTO matches(match_id,guild_id,team1_players,team2_players,winning_team)
             VALUES (?1,?2,'[]','[]',1)",
            params![5_041_i64, GUILD],
        )
        .expect("insert migrated settlement match");
    drop(connection);
    file
}

fn migrated_request(
    pending_match_id: i64,
    discord_id: i64,
    team: BettingTeam,
    amount: i64,
) -> PlaceBetRequest {
    PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id,
        discord_id,
        team,
        amount,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    }
}

#[test]
fn test_betting_totals_only_include_pending_bets() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 100, 20);
    add_player(file.path(), 101, 20);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 100, BettingTeam::Radiant, 3))
        .expect("radiant bet");
    repository
        .place_bet_atomic(request(1, 101, BettingTeam::Dire, 2))
        .expect("dire bet");
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(1))
            .expect("pending totals"),
        PotTotals {
            radiant: 3,
            dire: 2
        }
    );

    let first = repository
        .settle_pending_bets_atomic(
            900,
            Some(GUILD),
            NOW,
            Some(1),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settlement");
    assert_eq!(first.winners.len(), 1);
    assert_eq!(first.losers.len(), 1);
    assert_eq!(first.losers[0].balance_after, 18);
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(1))
            .expect("settled totals"),
        PotTotals::default()
    );
    assert_eq!(
        repository
            .settle_pending_bets_atomic(
                900,
                Some(GUILD),
                NOW,
                Some(1),
                BettingTeam::Radiant,
                BettingMode::House,
            )
            .expect("idempotent retry"),
        SettlementResult::default()
    );
}

#[test]
fn test_bet_settlement_is_guild_scoped_for_same_discord_id() {
    let file = fixture();
    let guild_one = 1_001;
    let guild_two = 2_002;
    let player_id = 9_001;
    let spectator_id = 9_002;
    add_player_in_guild(file.path(), player_id, guild_one, 200);
    add_player_in_guild(file.path(), player_id, guild_two, 200);
    add_player_in_guild(file.path(), spectator_id, guild_one, 200);
    Connection::open(file.path())
        .expect("open guild-isolation fixture")
        .execute(
            "INSERT INTO pending_matches (pending_match_id, guild_id, payload)
             VALUES (?1, ?2, ?3)",
            params![
                1,
                guild_one,
                format!(
                    "{{\"radiant_team_ids\":[{player_id}],\"dire_team_ids\":[9003],\"bet_lock_until\":{},\"is_draft\":false}}",
                    NOW + 1_000
                )
            ],
        )
        .expect("insert guild-one pending match");
    let repository = BettingServiceRepository::new(file.path());
    repository
        .place_bet_atomic(PlaceBetRequest {
            guild_id: Some(guild_one),
            pending_match_id: 1,
            discord_id: spectator_id,
            team: BettingTeam::Radiant,
            amount: 20,
            bet_time: NOW,
            leverage: 1,
            max_debt: 500,
            is_blind: false,
            odds_at_placement: None,
        })
        .expect("place guild-one wager");
    repository
        .settle_pending_bets_atomic(
            999,
            Some(guild_one),
            NOW,
            Some(1),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle guild-one wager");
    let guild_two_balance = Connection::open(file.path())
        .expect("open guild-isolation result")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![player_id, guild_two],
            |row| row.get::<_, i64>(0),
        )
        .expect("read guild-two balance");
    assert_eq!(guild_two_balance, 200);
    assert_eq!(balance_for_guild(file.path(), spectator_id, guild_one), 220);
}

fn balance_for_guild(path: &Path, discord_id: i64, guild_id: i64) -> i64 {
    Connection::open(path)
        .expect("open balance fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .expect("player balance")
}

#[test]
fn successful_bet_counter_is_durable_and_guild_scoped() {
    let file = fixture();
    add_counter_columns(file.path());
    add_player(file.path(), 4_201, 125);
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO players
                 (discord_id, guild_id, jopacoin_balance, total_bets_placed)
             VALUES (?1, ?2, ?3, ?4)",
            params![4_201, GUILD + 1, 900, 99],
        )
        .expect("insert same user in sibling guild");
    let repository = repo(file.path());
    let first = repository
        .record_successful_bet_counter(4_201, Some(GUILD), 5)
        .expect("record first wager");
    assert_eq!(
        first,
        BetCounterReceipt {
            total_bets_placed: 1,
            balance_after: 125,
            first_leverage_candidate: true,
        }
    );
    let sibling = repository
        .record_successful_bet_counter(4_201, Some(GUILD + 1), 1)
        .expect("record sibling guild wager");
    assert_eq!(sibling.total_bets_placed, 100);
    assert!(!sibling.first_leverage_candidate);
    assert!(
        repository
            .mark_first_leverage_used(4_201, Some(GUILD))
            .expect("claim first leverage marker")
    );
    assert!(
        !repository
            .mark_first_leverage_used(4_201, Some(GUILD))
            .expect("repeat first leverage marker")
    );
    let after_marker = repository
        .record_successful_bet_counter(4_201, Some(GUILD), 2)
        .expect("record second wager");
    assert_eq!(after_marker.total_bets_placed, 2);
    assert!(!after_marker.first_leverage_candidate);
}

#[test]
fn test_stale_pending_bets_do_not_show_or_block_new_match() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_pending(file.path(), 2, false);
    add_player(file.path(), 110, 50);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 110, BettingTeam::Radiant, 5))
        .expect("old bet");
    assert_eq!(
        repository
            .refund_pending_bets_atomic(Some(GUILD), NOW, Some(1))
            .expect("refund old match"),
        1
    );
    assert_eq!(balance(file.path(), 110), 50);
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW + 1, Some(2))
            .expect("new totals"),
        PotTotals::default()
    );
    let mut new_request = request(2, 110, BettingTeam::Dire, 4);
    new_request.bet_time = NOW + 1;
    repository
        .place_bet_atomic(new_request)
        .expect("new match bet");
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW + 1, Some(2))
            .expect("new totals"),
        PotTotals {
            radiant: 0,
            dire: 4
        }
    );
}

#[test]
fn test_get_pot_odds_isolated_between_matches() {
    let file = fixture();
    add_pending(file.path(), 11, false);
    add_pending(file.path(), 12, false);
    add_player(file.path(), 3_001, 200);
    add_player(file.path(), 3_002, 200);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(11, 3_001, BettingTeam::Radiant, 50))
        .expect("match one bet");
    repository
        .place_bet_atomic(request(12, 3_002, BettingTeam::Dire, 30))
        .expect("match two bet");

    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(11))
            .expect("match one totals"),
        PotTotals {
            radiant: 50,
            dire: 0,
        }
    );
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(12))
            .expect("match two totals"),
        PotTotals {
            radiant: 0,
            dire: 30,
        }
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(11))
            .expect("match one bets")
            .iter()
            .map(|bet| bet.discord_id)
            .collect::<Vec<_>>(),
        vec![3_001]
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(12))
            .expect("match two bets")
            .iter()
            .map(|bet| bet.discord_id)
            .collect::<Vec<_>>(),
        vec![3_002]
    );
}

#[test]
fn test_spectator_can_bet_on_multiple_matches() {
    let file = fixture();
    add_pending(file.path(), 21, false);
    add_pending(file.path(), 22, false);
    add_player(file.path(), 3_101, 200);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(21, 3_101, BettingTeam::Radiant, 50))
        .expect("first spectator bet");
    repository
        .place_bet_atomic(request(22, 3_101, BettingTeam::Dire, 50))
        .expect("second spectator bet");

    let first = repository
        .get_pending_bets(Some(GUILD), Some(3_101), NOW, Some(21))
        .expect("first spectator bet lookup");
    let second = repository
        .get_pending_bets(Some(GUILD), Some(3_101), NOW, Some(22))
        .expect("second spectator bet lookup");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].team, BettingTeam::Radiant);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].team, BettingTeam::Dire);
    assert_eq!(balance(file.path(), 3_101), 100);
}

#[test]
fn test_refund_bets_only_affects_aborted_match() {
    let file = fixture();
    add_pending(file.path(), 31, false);
    add_pending(file.path(), 32, false);
    add_player(file.path(), 3_201, 100);
    add_player(file.path(), 3_202, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(31, 3_201, BettingTeam::Radiant, 50))
        .expect("aborted match bet");
    repository
        .place_bet_atomic(request(32, 3_202, BettingTeam::Dire, 30))
        .expect("surviving match bet");
    assert_eq!(balance(file.path(), 3_201), 50);
    assert_eq!(balance(file.path(), 3_202), 70);

    assert_eq!(
        repository
            .refund_pending_bets_atomic(Some(GUILD), NOW, Some(31))
            .expect("refund aborted match"),
        1
    );
    assert_eq!(balance(file.path(), 3_201), 100);
    assert_eq!(balance(file.path(), 3_202), 70);
    assert!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(31))
            .expect("aborted bets")
            .is_empty()
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(32))
            .expect("surviving bets")
            .len(),
        1
    );
}

#[test]
fn test_record_preserves_sibling_bet() {
    let file = fixture();
    add_pending(file.path(), 41, false);
    add_pending(file.path(), 42, false);
    add_player(file.path(), 3_301, 100);
    add_player(file.path(), 3_302, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(41, 3_301, BettingTeam::Radiant, 50))
        .expect("recorded match bet");
    repository
        .place_bet_atomic(request(42, 3_302, BettingTeam::Dire, 30))
        .expect("sibling match bet");
    let settled = repository
        .settle_pending_bets_atomic(
            5_041,
            Some(GUILD),
            NOW,
            Some(41),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("record and settle first match");
    assert_eq!(settled.winners.len(), 1);
    assert!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(41))
            .expect("recorded match pending bets")
            .is_empty()
    );
    let sibling = repository
        .get_pending_bets(Some(GUILD), None, NOW, Some(42))
        .expect("sibling pending bets");
    assert_eq!(sibling.len(), 1);
    assert_eq!(sibling[0].discord_id, 3_302);
    assert_eq!(sibling[0].pending_match_id, Some(42));
}

#[test]
fn test_concurrent_match_pot_odds_isolated_on_migrated_sqlite() {
    let file = migrated_concurrent_fixture();
    let repository = repo(file.path());
    repository
        .place_bet_atomic(migrated_request(11, 3_001, BettingTeam::Radiant, 50))
        .expect("migrated match one bet");
    repository
        .place_bet_atomic(migrated_request(12, 3_002, BettingTeam::Dire, 30))
        .expect("migrated match two bet");

    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(11))
            .expect("migrated match one totals"),
        PotTotals {
            radiant: 50,
            dire: 0,
        }
    );
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(12))
            .expect("migrated match two totals"),
        PotTotals {
            radiant: 0,
            dire: 30,
        }
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(11))
            .expect("migrated match one bets")
            .iter()
            .map(|bet| bet.discord_id)
            .collect::<Vec<_>>(),
        vec![3_001]
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(12))
            .expect("migrated match two bets")
            .iter()
            .map(|bet| bet.discord_id)
            .collect::<Vec<_>>(),
        vec![3_002]
    );
}

#[test]
fn test_concurrent_match_spectator_can_bet_twice_on_migrated_sqlite() {
    let file = migrated_concurrent_fixture();
    let repository = repo(file.path());
    repository
        .place_bet_atomic(migrated_request(11, 3_101, BettingTeam::Radiant, 50))
        .expect("migrated first spectator bet");
    repository
        .place_bet_atomic(migrated_request(12, 3_101, BettingTeam::Dire, 50))
        .expect("migrated second spectator bet");

    let first = repository
        .get_pending_bets(Some(GUILD), Some(3_101), NOW, Some(11))
        .expect("migrated first spectator lookup");
    let second = repository
        .get_pending_bets(Some(GUILD), Some(3_101), NOW, Some(12))
        .expect("migrated second spectator lookup");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].team, BettingTeam::Radiant);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].team, BettingTeam::Dire);
    assert_eq!(balance(file.path(), 3_101), 100);
}

#[test]
fn test_concurrent_match_record_settlement_preserves_sibling_on_migrated_sqlite() {
    let file = migrated_concurrent_fixture();
    let repository = repo(file.path());
    repository
        .place_bet_atomic(migrated_request(11, 3_301, BettingTeam::Radiant, 50))
        .expect("migrated recorded-match bet");
    repository
        .place_bet_atomic(migrated_request(12, 3_302, BettingTeam::Dire, 30))
        .expect("migrated sibling-match bet");

    let settled = repository
        .settle_pending_bets_atomic(
            5_041,
            Some(GUILD),
            NOW,
            Some(11),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle migrated first match");
    assert_eq!(settled.winners.len(), 1);
    assert!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(11))
            .expect("settled match bets")
            .is_empty()
    );
    let sibling = repository
        .get_pending_bets(Some(GUILD), None, NOW, Some(12))
        .expect("migrated sibling bets");
    assert_eq!(sibling.len(), 1);
    assert_eq!(sibling[0].discord_id, 3_302);
    assert_eq!(sibling[0].pending_match_id, Some(12));
}

#[test]
fn test_settled_match_leverage_recovery_preserves_per_bet_tier() {
    let file = migrated_concurrent_fixture();
    let repository = repo(file.path());
    let mut request = migrated_request(11, 3_301, BettingTeam::Radiant, 50);
    request.leverage = 5;
    repository
        .place_bet_atomic(request)
        .expect("place leveraged migrated bet");

    let settled = repository
        .settle_pending_bets_atomic(
            5_041,
            Some(GUILD),
            NOW,
            Some(11),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle leveraged migrated bet");
    let bet_id = settled.winners[0].bet_id;
    let leverages = repository
        .get_match_bet_leverages(5_041, Some(GUILD))
        .expect("recover settled leverage");
    assert_eq!(leverages.get(&bet_id), Some(&5));
}

#[test]
fn test_concurrent_match_refund_only_aborted_match_on_migrated_sqlite() {
    let file = migrated_concurrent_fixture();
    let repository = repo(file.path());
    repository
        .place_bet_atomic(migrated_request(11, 3_201, BettingTeam::Radiant, 50))
        .expect("migrated aborted-match bet");
    repository
        .place_bet_atomic(migrated_request(12, 3_202, BettingTeam::Dire, 30))
        .expect("migrated surviving-match bet");
    assert_eq!(balance(file.path(), 3_201), 150);
    assert_eq!(balance(file.path(), 3_202), 170);

    assert_eq!(
        repository
            .refund_pending_bets_atomic(Some(GUILD), NOW, Some(11))
            .expect("refund migrated aborted match"),
        1
    );
    assert_eq!(balance(file.path(), 3_201), 200);
    assert_eq!(balance(file.path(), 3_202), 170);
    assert!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(11))
            .expect("aborted migrated bets")
            .is_empty()
    );
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), None, NOW, Some(12))
            .expect("surviving migrated bets")
            .len(),
        1
    );
}

#[test]
fn test_can_place_multiple_bets_same_team() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 120, 103);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 120, BettingTeam::Radiant, 10))
        .expect("first bet");
    repository
        .place_bet_atomic(request(1, 120, BettingTeam::Radiant, 15))
        .expect("second bet");
    let mut leveraged = request(1, 120, BettingTeam::Radiant, 5);
    leveraged.leverage = 2;
    repository
        .place_bet_atomic(leveraged)
        .expect("leveraged bet");
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(120), NOW, Some(1))
        .expect("pending bets");
    assert_eq!(bets.len(), 3);
    assert_eq!(
        bets.iter().map(|bet| bet.amount).collect::<Vec<_>>(),
        [10, 15, 5]
    );
    assert_eq!(bets[2].leverage, 2);
    assert_eq!(balance(file.path(), 120), 68);
}

#[test]
fn test_get_pending_bets_returns_empty_when_none() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 130, 100);
    assert!(
        repo(file.path())
            .get_pending_bets(Some(GUILD), Some(130), NOW, Some(1))
            .expect("empty pending bets")
            .is_empty()
    );
}

#[test]
fn test_multiple_bets_with_different_leverage() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 140, 503);
    let repository = repo(file.path());
    for leverage in [1, 2, 3, 5] {
        let mut bet = request(1, 140, BettingTeam::Radiant, 10);
        bet.leverage = leverage;
        repository.place_bet_atomic(bet).expect("leveraged bet");
    }
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(140), NOW, Some(1))
        .expect("pending bets");
    assert_eq!(
        bets.iter().map(|bet| bet.leverage).collect::<Vec<_>>(),
        [1, 2, 3, 5]
    );
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(1))
            .expect("totals")
            .radiant,
        110
    );
    assert_eq!(balance(file.path(), 140), 393);
}

#[test]
fn test_create_auto_blind_bets_basic() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for discord_id in 1..=10 {
        add_player(file.path(), discord_id, 100);
    }
    let requests = (1..=10)
        .map(|discord_id| AutomaticBetRequest {
            discord_id,
            team: if discord_id <= 5 {
                BettingTeam::Radiant
            } else {
                BettingTeam::Dire
            },
            percentage: 10,
        })
        .collect::<Vec<_>>();
    let repository = repo(file.path());
    let outcome = repository
        .place_automatic_bets_atomic(Some(GUILD), Some(1), NOW, &requests)
        .expect("automatic bets");
    assert_eq!(outcome.created.len(), 10);
    assert!(outcome.skipped.is_empty());
    assert!(outcome.created.iter().all(|bet| bet.amount == 10));
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(1))
            .expect("totals"),
        PotTotals {
            radiant: 50,
            dire: 50
        }
    );
}

#[test]
fn test_exact_basis_point_bets_round_like_python_and_persist_amounts() {
    let file = fixture();
    add_pending(file.path(), 81, true);
    add_player(file.path(), 20_001, 250); // 2.5 -> Python round => 2
    add_player(file.path(), 20_002, 150); // 1.5 -> Python round => 2
    add_player(file.path(), 20_003, 99); // .99 -> 1
    add_player(file.path(), 20_004, 50); // .5 -> 0, skipped
    let requests = [
        AutomaticBasisPointBetRequest {
            discord_id: 20_001,
            team: BettingTeam::Radiant,
            basis_points: 100,
        },
        AutomaticBasisPointBetRequest {
            discord_id: 20_002,
            team: BettingTeam::Dire,
            basis_points: 100,
        },
        AutomaticBasisPointBetRequest {
            discord_id: 20_003,
            team: BettingTeam::Radiant,
            basis_points: 100,
        },
        AutomaticBasisPointBetRequest {
            discord_id: 20_004,
            team: BettingTeam::Dire,
            basis_points: 100,
        },
    ];
    let outcome = repo(file.path())
        .place_automatic_basis_point_bets_atomic(Some(GUILD), Some(81), NOW, &requests, 500)
        .expect("exact spectator wagers");
    assert_eq!(
        outcome
            .created
            .iter()
            .map(|bet| bet.amount)
            .collect::<Vec<_>>(),
        [2, 2, 1]
    );
    assert_eq!(outcome.created[0].odds_at_placement, None);
    assert_eq!(outcome.created[1].odds_at_placement, None);
    assert_eq!(outcome.created[2].odds_at_placement, Some(2.0));
    assert_eq!(outcome.skipped, vec![20_004]);
    assert_eq!(balance(file.path(), 20_001), 248);
    assert_eq!(balance(file.path(), 20_002), 148);
    assert_eq!(balance(file.path(), 20_003), 98);
    assert_eq!(balance(file.path(), 20_004), 50);
    let persisted = Connection::open(file.path())
        .expect("open exact wager fixture")
        .query_row(
            "SELECT GROUP_CONCAT(amount, ',') FROM bets
             WHERE guild_id = ?1 AND pending_match_id = ?2 ORDER BY bet_id",
            params![GUILD, 81],
            |row| row.get::<_, String>(0),
        )
        .expect("persisted exact wager amounts");
    assert_eq!(persisted, "2,2,1");
}

#[test]
fn test_exact_amount_bets_are_idempotent_and_reject_debt() {
    let file = fixture();
    add_pending(file.path(), 82, true);
    add_player(file.path(), 20_101, 7);
    add_player(file.path(), 20_102, 1);
    add_player(file.path(), 20_103, -1);
    let request = AutomaticAmountBetRequest {
        discord_id: 20_101,
        team: BettingTeam::Radiant,
        amount: 4,
    };
    let repository = repo(file.path());
    let first = repository
        .place_automatic_amount_bets_atomic(Some(GUILD), Some(82), NOW, &[request], 500)
        .expect("first exact amount wager");
    assert_eq!(first.created.len(), 1);
    assert_eq!(first.created[0].amount, 4);
    let retry = repository
        .place_automatic_amount_bets_atomic(Some(GUILD), Some(82), NOW, &[request], 500)
        .expect("idempotent exact amount retry");
    assert!(retry.created.is_empty());
    assert_eq!(retry.skipped, vec![20_101]);
    assert_eq!(balance(file.path(), 20_101), 3);
    let debt_request = AutomaticAmountBetRequest {
        discord_id: 20_102,
        team: BettingTeam::Dire,
        amount: 2,
    };
    let debt = repository
        .place_automatic_amount_bets_atomic(Some(GUILD), Some(82), NOW, &[debt_request], 500)
        .expect("debt candidate is isolated");
    assert!(debt.created.is_empty());
    assert_eq!(debt.skipped, vec![20_102]);
    assert_eq!(balance(file.path(), 20_102), 1);
    let already_in_debt = AutomaticAmountBetRequest {
        discord_id: 20_103,
        team: BettingTeam::Radiant,
        amount: 1,
    };
    let debt = repository
        .place_automatic_amount_bets_atomic(Some(GUILD), Some(82), NOW, &[already_in_debt], 500)
        .expect("already-debt candidate is isolated");
    assert!(debt.created.is_empty());
    assert_eq!(debt.skipped, vec![20_103]);
    assert_eq!(balance(file.path(), 20_103), -1);
}

#[test]
fn test_configured_investments_use_snapshot_team_policy_and_attribution() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for discord_id in 1..=10 {
        add_player(file.path(), discord_id, 100);
    }
    add_player(file.path(), 11, 100);
    add_player(file.path(), 12, 100);
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO autobet_investments
             (guild_id, investor_id, target_id, direction, percentage)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![GUILD, 11, 1, "long", 10],
        )
        .expect("long investment");
    connection
        .execute(
            "INSERT INTO autobet_investments
             (guild_id, investor_id, target_id, direction, percentage)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![GUILD, 12, 1, "short", 10],
        )
        .expect("short investment");
    let outcome = repo(file.path())
        .create_configured_investment_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &[1, 2, 3, 4, 5],
            &[6, 7, 8, 9, 10],
            500,
        )
        .expect("configured investments");
    assert_eq!(outcome.created.len(), 2);
    assert!(outcome.skipped.is_empty());
    assert_eq!(balance(file.path(), 11), 90);
    assert_eq!(balance(file.path(), 12), 90);
    let rows = connection
        .prepare(
            "SELECT discord_id, team_bet_on, investment_target_id, investment_direction
             FROM bets ORDER BY discord_id",
        )
        .expect("prepare investment rows")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .expect("query investment rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect investment rows");
    assert_eq!(
        rows,
        vec![
            (11, "radiant".to_owned(), 1, "long".to_owned()),
            (12, "dire".to_owned(), 1, "short".to_owned()),
        ]
    );
}

#[test]
fn configured_investment_shuffle_hook_uses_typed_request() {
    let file = fixture();
    add_pending(file.path(), 2, false);
    add_player(file.path(), 22_001, 100);
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "INSERT INTO autobet_investments
             (guild_id, investor_id, target_id, direction, percentage)
             VALUES (?1, ?2, ?3, 'long', 10)",
            params![GUILD, 22_001, 1],
        )
        .expect("configured position");
    let outcome = repo(file.path())
        .create_configured_investment_bets_for_shuffle(ConfiguredInvestmentRequest {
            guild_id: Some(GUILD),
            pending_match_id: Some(2),
            bet_time: NOW,
            radiant_ids: vec![1, 2, 3, 4, 5],
            dire_ids: vec![6, 7, 8, 9, 10],
            max_debt: 500,
            starting_balances: BTreeMap::new(),
        })
        .expect("typed configured investment hook");
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.created[0].discord_id, 22_001);
    assert_eq!(balance(file.path(), 22_001), 90);
}

#[test]
fn test_configured_investment_skips_participant_opposite_team() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for discord_id in 1..=10 {
        add_player(file.path(), discord_id, 100);
    }
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO autobet_investments
             (guild_id, investor_id, target_id, direction, percentage)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![GUILD, 1, 6, "long", 10],
        )
        .expect("opposite participant investment");
    let outcome = repo(file.path())
        .create_configured_investment_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &[1, 2, 3, 4, 5],
            &[6, 7, 8, 9, 10],
            500,
        )
        .expect("configured investment skip");
    assert!(outcome.created.is_empty());
    assert_eq!(outcome.skipped, vec![1]);
    assert_eq!(balance(file.path(), 1), 100);
}

#[test]
fn generic_automatic_bet_defaults_to_null_investment_attribution() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 12_500, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 1,
        discord_id: 12_500,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: true,
        odds_at_placement: None,
    };
    repo(file.path())
        .place_bet_atomic(request)
        .expect("generic automatic bet");
    let row = Connection::open(file.path())
        .expect("open generic automatic fixture")
        .query_row(
            "SELECT is_blind, investment_target_id, investment_direction
             FROM bets WHERE discord_id=?1",
            [12_500],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .expect("generic attribution row");
    assert_eq!(row, (1, None, None));
}

#[test]
fn inconsistent_investment_attribution_is_rejected_before_debit() {
    let file = fixture();
    add_pending(file.path(), 2, false);
    add_player(file.path(), 12_501, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 2,
        discord_id: 12_501,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    };
    assert!(matches!(
        repo(file.path()).place_investment_bet_atomic(request, 12_502, "sideways"),
        Err(BettingServiceRepositoryError::InvalidWager)
    ));
    assert_eq!(balance(file.path(), 12_501), 100);
    assert_eq!(
        Connection::open(file.path())
            .expect("open rejected attribution fixture")
            .query_row("SELECT COUNT(*) FROM bets", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn attribution_direction_requires_a_target_before_debit() {
    let file = fixture();
    add_pending(file.path(), 6, false);
    add_player(file.path(), 12_506, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 6,
        discord_id: 12_506,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: true,
        odds_at_placement: None,
    };
    assert!(matches!(
        repo(file.path()).place_bet_atomic_with_attribution(
            request,
            Some(BetAttribution {
                investment_target_id: None,
                investment_direction: Some("long".to_owned()),
            }),
        ),
        Err(BettingServiceRepositoryError::InvalidWager)
    ));
    assert_eq!(balance(file.path(), 12_506), 100);
}

#[test]
fn attributed_wager_requires_a_blind_request_before_debit() {
    let file = fixture();
    add_pending(file.path(), 7, false);
    add_player(file.path(), 12_507, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 7,
        discord_id: 12_507,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: false,
        odds_at_placement: None,
    };
    assert!(matches!(
        repo(file.path()).place_bet_atomic_with_attribution(
            request,
            Some(BetAttribution {
                investment_target_id: Some(202),
                investment_direction: Some("long".to_owned()),
            }),
        ),
        Err(BettingServiceRepositoryError::InvalidWager)
    ));
    assert_eq!(balance(file.path(), 12_507), 100);
}

#[test]
fn attributed_wager_requires_a_direction_before_debit() {
    let file = fixture();
    add_pending(file.path(), 8, false);
    add_player(file.path(), 12_508, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 8,
        discord_id: 12_508,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: true,
        odds_at_placement: None,
    };
    assert!(matches!(
        repo(file.path()).place_bet_atomic_with_attribution(
            request,
            Some(BetAttribution {
                investment_target_id: Some(202),
                investment_direction: None,
            }),
        ),
        Err(BettingServiceRepositoryError::InvalidWager)
    ));
    assert_eq!(balance(file.path(), 12_508), 100);
}

#[test]
fn attributed_wager_rejects_non_long_short_direction_before_debit() {
    let file = fixture();
    add_pending(file.path(), 9, false);
    add_player(file.path(), 12_509, 100);
    let request = PlaceBetRequest {
        guild_id: Some(GUILD),
        pending_match_id: 9,
        discord_id: 12_509,
        team: BettingTeam::Radiant,
        amount: 10,
        bet_time: NOW,
        leverage: 1,
        max_debt: 500,
        is_blind: true,
        odds_at_placement: None,
    };
    assert!(matches!(
        repo(file.path()).place_bet_atomic_with_attribution(
            request,
            Some(BetAttribution {
                investment_target_id: Some(202),
                investment_direction: Some("sideways".to_owned()),
            }),
        ),
        Err(BettingServiceRepositoryError::InvalidWager)
    ));
    assert_eq!(balance(file.path(), 12_509), 100);
}

#[test]
fn configured_investments_resolve_long_short_without_opposing_participants() {
    let file = fixture();
    add_pending(file.path(), 4, false);
    for discord_id in [1, 2, 3, 4, 101, 102, 103, 104] {
        add_player(file.path(), discord_id, 100);
    }
    let connection = Connection::open(file.path()).expect("open investment direction fixture");
    let positions = [
        (1, 1, "long"),
        (2, 3, "short"),
        (101, 1, "long"),
        (102, 1, "short"),
        (3, 1, "long"),
        (4, 3, "short"),
    ];
    for (investor, target, direction) in positions {
        connection
            .execute(
                "INSERT INTO autobet_investments
                 (guild_id, investor_id, target_id, direction, percentage)
                 VALUES (?1, ?2, ?3, ?4, 10)",
                params![GUILD, investor, target, direction],
            )
            .expect("investment position");
    }
    let outcome = repo(file.path())
        .create_configured_investment_bets_atomic(Some(GUILD), Some(4), NOW, &[1, 2], &[3, 4], 500)
        .expect("resolve configured investments");
    let created = outcome
        .created
        .iter()
        .map(|bet| (bet.discord_id, bet.team, bet.amount))
        .collect::<Vec<_>>();
    assert_eq!(
        created,
        vec![
            (1, BettingTeam::Radiant, 10),
            (2, BettingTeam::Radiant, 10),
            (101, BettingTeam::Radiant, 10),
            (102, BettingTeam::Dire, 10),
        ]
    );
    assert_eq!(outcome.skipped, vec![3, 4]);
}

#[test]
fn configured_investments_require_shuffle_start_balance_and_floor_live_balance() {
    let file = fixture();
    add_pending_payload(
        file.path(),
        5,
        &format!(
            "{{\"radiant_team_ids\":[1,2],\"dire_team_ids\":[3],\"bet_lock_until\":{}}}",
            NOW + 1_000
        ),
    );
    for discord_id in [1, 2, 3] {
        add_player(file.path(), discord_id, 100);
    }
    let connection = Connection::open(file.path()).expect("open investment threshold fixture");
    for (investor, target, direction, percentage) in
        [(1, 1, "long", 10), (2, 1, "long", 10), (3, 1, "short", 7)]
    {
        connection
            .execute(
                "INSERT INTO autobet_investments
                 (guild_id, investor_id, target_id, direction, percentage)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![GUILD, investor, target, direction, percentage],
            )
            .expect("threshold investment position");
    }
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=45 WHERE guild_id=?1 AND discord_id=1",
            [GUILD],
        )
        .expect("participant blind debit");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=44 WHERE guild_id=?1 AND discord_id=2",
            [GUILD],
        )
        .expect("second participant blind debit");
    let outcome = repo(file.path())
        .create_configured_investment_bets_for_shuffle(ConfiguredInvestmentRequest {
            guild_id: Some(GUILD),
            pending_match_id: Some(5),
            bet_time: NOW,
            radiant_ids: vec![1, 2],
            dire_ids: Vec::new(),
            max_debt: 500,
            starting_balances: BTreeMap::from([(1, 50), (2, 49), (3, 100)]),
        })
        .expect("threshold configured investments");
    assert_eq!(
        outcome
            .created
            .iter()
            .map(|bet| (bet.discord_id, bet.amount, bet.team))
            .collect::<Vec<_>>(),
        vec![(1, 4, BettingTeam::Radiant), (3, 7, BettingTeam::Dire)]
    );
    assert!(outcome.skipped.contains(&2));
}

#[test]
fn test_create_auto_blind_bets_batches_balance_snapshot() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 150, 100);
    let requests = [
        AutomaticBetRequest {
            discord_id: 150,
            team: BettingTeam::Radiant,
            percentage: 10,
        },
        AutomaticBetRequest {
            discord_id: 150,
            team: BettingTeam::Radiant,
            percentage: 10,
        },
    ];
    let outcome = repo(file.path())
        .place_automatic_bets_atomic(Some(GUILD), Some(1), NOW, &requests)
        .expect("snapshot batch");
    assert_eq!(outcome.balance_snapshot, BTreeMap::from([(150, 100)]));
    assert_eq!(
        outcome
            .created
            .iter()
            .map(|bet| bet.amount)
            .collect::<Vec<_>>(),
        [10, 10]
    );
    assert_eq!(balance(file.path(), 150), 80);
}

#[test]
fn test_blind_bet_is_blind_flag() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 1, 100);
    let repository = repo(file.path());
    repository
        .place_automatic_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &[AutomaticBetRequest {
                discord_id: 1,
                team: BettingTeam::Radiant,
                percentage: 10,
            }],
        )
        .expect("blind bet");
    repository
        .place_bet_atomic(request(1, 1, BettingTeam::Radiant, 10))
        .expect("manual bet");
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(1), NOW, Some(1))
        .expect("pending bets");
    assert_eq!(bets.len(), 2);
    assert!(bets[0].is_blind);
    assert!(!bets[1].is_blind);
}

#[test]
fn test_get_all_pending_bets() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for discord_id in 1..=10 {
        add_player(file.path(), discord_id, 100);
    }
    add_player(file.path(), 160, 100);
    let requests = (1..=10)
        .map(|discord_id| AutomaticBetRequest {
            discord_id,
            team: if discord_id <= 5 {
                BettingTeam::Radiant
            } else {
                BettingTeam::Dire
            },
            percentage: 10,
        })
        .collect::<Vec<_>>();
    let repository = repo(file.path());
    repository
        .place_automatic_bets_atomic(Some(GUILD), Some(1), NOW, &requests)
        .expect("blind bets");
    repository
        .place_bet_atomic(request(1, 160, BettingTeam::Radiant, 20))
        .expect("manual spectator bet");
    let all = repository
        .get_pending_bets(Some(GUILD), None, NOW, Some(1))
        .expect("all pending");
    assert_eq!(all.len(), 11);
    assert_eq!(all.iter().filter(|bet| bet.is_blind).count(), 10);
    assert_eq!(all.iter().filter(|bet| !bet.is_blind).count(), 1);
}

#[test]
fn test_blind_bets_integration_like_shuffle_command() {
    let file = fixture();
    add_pending(file.path(), 7, false);
    for discord_id in 1..=10 {
        add_player(file.path(), discord_id, 100);
    }
    let pending_team_ids = (1..=10).collect::<Vec<_>>();
    let requests = pending_team_ids
        .iter()
        .map(|discord_id| AutomaticBetRequest {
            discord_id: *discord_id,
            team: if *discord_id <= 5 {
                BettingTeam::Radiant
            } else {
                BettingTeam::Dire
            },
            percentage: 10,
        })
        .collect::<Vec<_>>();
    let repository = repo(file.path());
    let created = repository
        .place_automatic_bets_atomic(Some(GUILD), Some(7), NOW, &requests)
        .expect("shuffle command automatic bets");
    assert_eq!(created.created.len(), 10);
    assert_eq!(
        repository
            .get_pending_totals(Some(GUILD), NOW, Some(7))
            .expect("pot totals"),
        PotTotals {
            radiant: 50,
            dire: 50
        }
    );
}

#[test]
fn test_auto_spectator_can_manual_bet_opposite_team_on_draft() {
    let file = fixture();
    add_pending(file.path(), 1, true);
    add_player(file.path(), 170, 5_000);
    let repository = repo(file.path());
    repository
        .place_automatic_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &[AutomaticBetRequest {
                discord_id: 170,
                team: BettingTeam::Radiant,
                percentage: 2,
            }],
        )
        .expect("auto spectator bet");
    repository
        .place_bet_atomic(request(1, 170, BettingTeam::Dire, 10))
        .expect("opposite manual bet");
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(170), NOW, Some(1))
        .expect("spectator bets");
    assert_eq!(
        bets.iter().map(|bet| bet.team).collect::<Vec<_>>(),
        [BettingTeam::Radiant, BettingTeam::Dire]
    );
}

#[test]
fn test_consume_and_credit_atomic_pays_once() {
    let file = fixture();
    add_player(file.path(), 180, 0);
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO manashop_buffs
                 (discord_id, guild_id, buff_type, granted_at, expires_at)
             VALUES (?1, ?2, 'communion_blessing', ?3, ?4)",
            params![180, GUILD, NOW, NOW + 3_600],
        )
        .expect("grant blessing");
    let buff_id = connection.last_insert_rowid();
    drop(connection);
    let repository = repo(file.path());
    assert!(
        repository
            .consume_and_credit_atomic(buff_id, 180, Some(GUILD), 25)
            .expect("first claim")
    );
    assert!(
        !repository
            .consume_and_credit_atomic(buff_id, 180, Some(GUILD), 25)
            .expect("second claim")
    );
    assert_eq!(balance(file.path(), 180), 25);
}

#[test]
fn test_bet_history_distinguishes_zero_payout_from_null() {
    let file = fixture();
    add_player(file.path(), 190, 0);
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute_batch("INSERT INTO matches (match_id, winning_team) VALUES (901, 1), (902, 1);")
        .expect("insert matches");
    connection
        .execute(
            "INSERT INTO bets
                 (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, payout)
             VALUES (?1, 901, 190, 'radiant', 10, 1, 1, 0),
                    (?1, 902, 190, 'radiant', 10, 2, 1, NULL)",
            [GUILD],
        )
        .expect("insert settled bets");
    let history = repo(file.path())
        .get_player_bet_history(190, Some(GUILD))
        .expect("bet history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].payout, Some(0));
    assert_eq!(history[0].profit, -10);
    assert_eq!(history[1].payout, None);
    assert_eq!(history[1].profit, 10);
}

/// Python owns migrations and creates the fixture, Rust performs the guarded
/// betting transactions, then Python verifies balances, status, and ledger
/// attribution. This is deliberately outside the 30-node parity mapping.
#[test]
#[ignore = "cross-language smoke invokes the repository Python environment"]
fn unmapped_python_migrated_database_betting_service_interop_smoke() {
    let file = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import json,sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\npayload={'radiant_team_ids':[1,2,3,4,5],'dire_team_ids':[6,7,8,9,10],'shuffle_timestamp':1700000000,'bet_lock_until':2000000000,'is_draft':False}\nc.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)\",(-70001,0,'bet-interop',100))\nc.execute(\"INSERT INTO pending_matches (pending_match_id,guild_id,payload) VALUES (?,?,?)\",(900,0,json.dumps(payload)))\nc.execute(\"INSERT INTO matches (match_id,team1_players,team2_players,winning_team) VALUES (?,?,?,?)\",(991,'[]','[]',1))\nc.execute(\"INSERT INTO manashop_buffs (discord_id,guild_id,buff_type,granted_at,expires_at) VALUES (?,?,?,?,?)\",(-70001,0,'communion_blessing',1700000000,2000000000))\nc.commit(); c.close()",
            file.path().to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("initialize Python-migrated database");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let repository = BettingServiceRepository::new(file.path());
    let placed = repository
        .place_bet_atomic(PlaceBetRequest {
            guild_id: None,
            pending_match_id: 900,
            discord_id: -70_001,
            team: BettingTeam::Radiant,
            amount: 20,
            bet_time: 1_700_000_000,
            leverage: 1,
            max_debt: 500,
            is_blind: false,
            odds_at_placement: None,
        })
        .expect("place through Python schema");
    let settlement = repository
        .settle_pending_bets_atomic(
            991,
            None,
            1_700_000_000,
            Some(900),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("settle through Python schema");
    assert_eq!(settlement.winners.len(), 1);
    assert_eq!(settlement.winners[0].payout, 40);
    assert!(
        repository
            .settle_pending_bets_atomic(
                991,
                None,
                1_700_000_000,
                Some(900),
                BettingTeam::Radiant,
                BettingMode::House,
            )
            .expect("idempotent interop retry")
            .winners
            .is_empty()
    );
    assert!(
        repository
            .consume_and_credit_atomic(1, -70_001, None, 5)
            .expect("consume blessing through Python schema")
    );

    let verify = Command::new(python)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nb=c.execute(\"SELECT jopacoin_balance FROM players WHERE discord_id=-70001 AND guild_id=0\").fetchone()[0]\nbet=c.execute(\"SELECT match_id,payout FROM bets WHERE bet_id=?\",(int(sys.argv[2]),)).fetchone()\ntriggered=c.execute(\"SELECT triggered FROM manashop_buffs WHERE id=1\").fetchone()[0]\nsources={r[0] for r in c.execute(\"SELECT source FROM economy_ledger_entries WHERE account_id=-70001\")}\nassert b == 125\nassert bet == (991,40)\nassert triggered == 1\nassert {'bet','bet_settlement','manashop_buff'} <= sources\nc.close()",
            file.path().to_str().expect("UTF-8 path"),
            &placed.bet_id.to_string(),
        ])
        .output()
        .expect("verify Rust writes from Python");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

fn migrated_seed_fixture(match_id: i64, pending_match_id: i64, mode: BettingMode) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary migrated seed database");
    initialize_or_migrate(file.path()).expect("migrate seed database");
    let connection = Connection::open(file.path()).expect("open migrated seed database");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("disable unrelated legacy fixture foreign keys");
    let (radiant, dire, bonus) = match mode {
        BettingMode::Pool => (25, 25, 0),
        BettingMode::House => (0, 0, 50),
    };
    let payload = format!(
        "{{\"radiant_team_ids\":[],\"dire_team_ids\":[],\"bet_lock_until\":{},\"bet_seed_reserved\":50,\"bet_seed_radiant\":{radiant},\"bet_seed_dire\":{dire},\"bet_seed_bonus\":{bonus},\"first_game_pool_reserved\":0}}",
        NOW + 1_000
    );
    connection
        .execute(
            "INSERT INTO pending_matches(pending_match_id,guild_id,payload)
             VALUES (?1,?2,?3)",
            params![pending_match_id, GUILD, payload],
        )
        .expect("insert seeded pending match");
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,team1_players,team2_players,winning_team
             ) VALUES (?1,?2,'[]','[]',1)",
            params![match_id, GUILD],
        )
        .expect("insert settlement match");
    connection
        .execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected)
             VALUES (?1,50)",
            [GUILD],
        )
        .expect("insert post-reservation fund");
    for discord_id in [91_001, 91_002, 91_003] {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance
                 ) VALUES (?1,?2,?3,100)",
                params![discord_id, GUILD, format!("seed-{discord_id}")],
            )
            .expect("insert seed bettor");
    }
    drop(connection);
    file
}

fn seed_adjustments() -> BetSettlementAdjustments {
    BetSettlementAdjustments {
        consume_seed: true,
        ..BetSettlementAdjustments::default()
    }
}

#[test]
fn migrated_pool_seed_multiplier_penalty_and_tax_share_one_settlement() {
    let match_id = 9_101;
    let pending_match_id = 9_001;
    let file = migrated_seed_fixture(match_id, pending_match_id, BettingMode::Pool);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(pending_match_id, 91_001, BettingTeam::Radiant, 20))
        .expect("place winning pool bet");
    repository
        .place_bet_atomic(request(pending_match_id, 91_002, BettingTeam::Dire, 30))
        .expect("place losing pool bet");
    Connection::open(file.path())
        .expect("open bankruptcy state")
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (91001,?1,0,2,1)",
            [GUILD],
        )
        .expect("insert penalized bettor");
    let settlement = repository
        .settle_pending_bets_with_adjustments_atomic(
            match_id,
            Some(GUILD),
            NOW,
            Some(pending_match_id),
            BettingTeam::Radiant,
            BettingMode::Pool,
            &BetSettlementAdjustments {
                bankruptcy_keep_basis_points: Some(7_500),
                vanity_tax_basis_points: 1_000,
                vanity_taxable_ids: BTreeSet::from([91_001]),
                payout_multiplier: 2.0,
                consume_seed: true,
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("settle seeded multiplied pool");
    assert_eq!(settlement.seed.winner_seed, 25);
    assert_eq!(settlement.seed.returned_to_fund, 25);
    assert_eq!(settlement.winners[0].payout, 150);
    assert_eq!(settlement.bankruptcy_penalties[&91_001], 32);
    assert_eq!(settlement.vanity_taxes[&91_001], 13);
    assert_eq!(settlement.winners[0].balance_after, 185);
    Connection::open(file.path())
        .expect("open sibling snapshot")
        .execute(
            "UPDATE matches SET jc_changes=?1 WHERE match_id=?2 AND guild_id=?3",
            params![r#"{"91099":{"referral":2}}"#, match_id, GUILD],
        )
        .expect("seed sibling snapshot component");
    repository
        .persist_settlement_bet_jc_changes(match_id, Some(GUILD), &settlement)
        .expect("persist base bet snapshot");
    let base_snapshot: String = Connection::open(file.path())
        .expect("open base snapshot")
        .query_row(
            "SELECT jc_changes FROM matches WHERE match_id=?1 AND guild_id=?2",
            params![match_id, GUILD],
            |row| row.get(0),
        )
        .expect("load base bet snapshot");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&base_snapshot)
            .expect("decode base bet snapshot")["91001"]["bet"],
        85
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&base_snapshot).expect("decode sibling snapshot")
            ["91099"]["referral"],
        2
    );
    Connection::open(file.path())
        .expect("open recovery match")
        .execute(
            "UPDATE matches SET pending_match_id=?1 WHERE match_id=?2",
            params![pending_match_id, match_id],
        )
        .expect("tag recovery match with pending id");
    let recovered = repository
        .recover_settlement_for_match(match_id, Some(GUILD))
        .expect("recover committed settlement");
    assert_eq!(recovered.winners, settlement.winners);
    assert_eq!(recovered.losers, settlement.losers);
    assert_eq!(
        recovered.bankruptcy_penalties,
        settlement.bankruptcy_penalties
    );
    assert_eq!(recovered.vanity_taxes, settlement.vanity_taxes);
    Connection::open(file.path())
        .expect("open Blood Pact ledger")
        .execute(
            "INSERT INTO economy_ledger_entries(
                 guild_id,account_type,account_id,delta,balance_before,
                 balance_after,source,related_type,related_id,reason
             ) VALUES (?1,'player',?2,-7,185,178,
                       'dota_bet_blood_pact','match',?3,'bet Blood Pact')",
            params![GUILD, 91_001, match_id.to_string()],
        )
        .expect("persist Blood Pact skim");
    assert_eq!(
        repository
            .match_blood_pact_skims(match_id, Some(GUILD))
            .expect("recover Blood Pact skim"),
        BTreeMap::from([(91_001, 7)])
    );
    Connection::open(file.path())
        .expect("open protected Blood Pact ledger")
        .execute(
            "INSERT INTO hostile_loss_events(
                 guild_id,victim_id,actor_id,event_key,kind,destination,
                 recipient_id,requested,attempted,absorbed,applied,
                 victim_balance_before,victim_balance_after,
                 destination_balance_before,destination_balance_after,
                 shieldable,retro_covered,protection_details,metadata,
                 occurred_at,created_at
             ) VALUES (?1,?2,?3,?4,'blood_pact','player',?3,
                       3,3,0,3,178,175,0,3,1,0,'[]','{}',?5,?5)",
            params![
                GUILD,
                91_001,
                91_003,
                format!("dota-bet-blood-pact:{match_id}:protected"),
                NOW,
            ],
        )
        .expect("persist protected Blood Pact skim");
    assert_eq!(
        repository
            .match_blood_pact_skims(match_id, Some(GUILD))
            .expect("recover protected Blood Pact skim"),
        BTreeMap::from([(91_001, 10)])
    );
    let connection = Connection::open(file.path()).expect("inspect pool settlement");
    let state: (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                 (SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1),
                 (SELECT payout FROM bets WHERE discord_id=91001 AND match_id=?2),
                 CAST(json_extract(payload,'$.bet_seed_reserved') AS INTEGER),
                 CAST(json_extract(payload,'$.bet_seed_radiant') AS INTEGER),
                 CAST(json_extract(payload,'$.bet_seed_dire') AS INTEGER)
             FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?3",
            params![GUILD, match_id, pending_match_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("load pool seed state");
    assert_eq!(state, (75, 150, 0, 0, 0));
    drop(connection);
    assert_eq!(
        repository
            .settle_pending_bets_with_adjustments_atomic(
                match_id,
                Some(GUILD),
                NOW,
                Some(pending_match_id),
                BettingTeam::Radiant,
                BettingMode::Pool,
                &seed_adjustments(),
            )
            .expect("retry seeded pool"),
        SettlementResult::default()
    );
}

#[test]
fn migrated_house_seed_bonus_is_scaled_with_gross_payout() {
    let match_id = 9_102;
    let pending_match_id = 9_002;
    let file = migrated_seed_fixture(match_id, pending_match_id, BettingMode::House);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(pending_match_id, 91_001, BettingTeam::Radiant, 20))
        .expect("place house winner");
    repository
        .place_bet_atomic(request(pending_match_id, 91_002, BettingTeam::Dire, 30))
        .expect("place house loser");
    let settlement = repository
        .settle_pending_bets_with_adjustments_atomic(
            match_id,
            Some(GUILD),
            NOW,
            Some(pending_match_id),
            BettingTeam::Radiant,
            BettingMode::House,
            &BetSettlementAdjustments {
                house_payout_multiplier: 1.0,
                payout_multiplier: 0.5,
                consume_seed: true,
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("settle seeded house");
    assert_eq!(settlement.seed.winner_seed, 50);
    assert_eq!(settlement.seed.returned_to_fund, 0);
    assert_eq!(settlement.winners[0].payout, 45);
    assert_eq!(settlement.winners[0].balance_after, 125);
    assert_eq!(balance(file.path(), 91_002), 70);
    let fund: i64 = Connection::open(file.path())
        .expect("inspect house fund")
        .query_row(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("load house fund");
    assert_eq!(fund, 50);
}

#[test]
fn migrated_pool_without_real_winner_returns_every_reserved_seed() {
    let match_id = 9_103;
    let pending_match_id = 9_003;
    let file = migrated_seed_fixture(match_id, pending_match_id, BettingMode::Pool);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(pending_match_id, 91_002, BettingTeam::Dire, 30))
        .expect("place only losing bet");
    let settlement = repository
        .settle_pending_bets_with_adjustments_atomic(
            match_id,
            Some(GUILD),
            NOW,
            Some(pending_match_id),
            BettingTeam::Radiant,
            BettingMode::Pool,
            &seed_adjustments(),
        )
        .expect("settle no-winner pool");
    assert!(settlement.winners.is_empty());
    assert_eq!(settlement.losers.len(), 1);
    assert_eq!(settlement.seed.returned_to_fund, 50);
    let fund: i64 = Connection::open(file.path())
        .expect("inspect returned pool seed")
        .query_row(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("load returned pool fund");
    assert_eq!(fund, 100);
}

#[test]
fn test_place_bet_requires_pending_state() {
    let file = fixture();
    add_player(file.path(), 1001, 20);
    let error = repo(file.path())
        .place_bet_atomic(request(1, 1001, BettingTeam::Radiant, 5))
        .expect_err("missing pending match must reject");
    assert!(matches!(
        error,
        BettingServiceRepositoryError::MissingPendingMatch {
            pending_match_id: 1,
            ..
        }
    ));
}

#[test]
fn test_bet_lock_enforced() {
    let file = fixture();
    add_pending_payload(
        file.path(),
        1,
        &format!(
            "{{\"radiant_team_ids\":[1,2,3,4,5],\"dire_team_ids\":[6,7,8,9,10],\"bet_lock_until\":{NOW}}}"
        ),
    );
    add_player(file.path(), 1001, 10);
    assert!(matches!(
        repo(file.path())
            .place_bet_atomic(request(1, 1001, BettingTeam::Radiant, 5))
            .expect_err("closed betting must reject"),
        BettingServiceRepositoryError::BettingClosed
    ));
}

#[test]
fn test_pending_bet_enforces_participant_team_and_lock() {
    let file = fixture();
    add_pending_payload(
        file.path(),
        2,
        &format!(
            "{{\"radiant_team_ids\":[1],\"dire_team_ids\":[2],\"bet_lock_until\":{}}}",
            NOW + 100
        ),
    );
    add_player(file.path(), 1, 20);
    let repository = repo(file.path());
    assert!(matches!(
        repository
            .place_bet_atomic(request(2, 1, BettingTeam::Dire, 5))
            .expect_err("participant must stay on their team"),
        BettingServiceRepositoryError::ParticipantTeamOnly { .. }
    ));
    add_pending_payload(
        file.path(),
        3,
        &format!(
            "{{\"radiant_team_ids\":[1],\"dire_team_ids\":[2],\"bet_lock_until\":{}}}",
            NOW
        ),
    );
    assert!(matches!(
        repository
            .place_bet_atomic(request(3, 1, BettingTeam::Radiant, 5))
            .expect_err("closed pending wager"),
        BettingServiceRepositoryError::BettingClosed
    ));
}

#[test]
fn test_participant_can_only_bet_on_own_team() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 1, 20);
    add_player(file.path(), 2_000, 20);
    let repository = repo(file.path());
    assert!(matches!(
        repository
            .place_bet_atomic(request(1, 1, BettingTeam::Dire, 5))
            .expect_err("participant cannot oppose own team"),
        BettingServiceRepositoryError::ParticipantTeamOnly { .. }
    ));
    repository
        .place_bet_atomic(request(1, 2_000, BettingTeam::Dire, 5))
        .expect("spectator first team");
    repository
        .place_bet_atomic(request(1, 2_000, BettingTeam::Dire, 3))
        .expect("spectator same team");
    repository
        .place_bet_atomic(request(1, 2_000, BettingTeam::Radiant, 5))
        .expect("shuffle spectator opposite team");
}

#[test]
fn test_shuffle_spectator_can_bet_both_teams() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 10_300, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 10_300, BettingTeam::Radiant, 10))
        .expect("radiant spectator bet");
    repository
        .place_bet_atomic(request(1, 10_300, BettingTeam::Dire, 10))
        .expect("dire spectator bet");
    let teams = repository
        .get_pending_bets(Some(GUILD), Some(10_300), NOW, Some(1))
        .expect("spectator bets")
        .into_iter()
        .map(|bet| bet.team)
        .collect::<Vec<_>>();
    assert_eq!(teams.len(), 2);
    assert!(teams.contains(&BettingTeam::Radiant));
    assert!(teams.contains(&BettingTeam::Dire));
}

#[test]
fn test_multiple_bets_balance_enforced_each_bet() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 11_500, 10);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 11_500, BettingTeam::Radiant, 5))
        .expect("first bet");
    assert_eq!(balance(file.path(), 11_500), 5);
    repository
        .place_bet_atomic(request(1, 11_500, BettingTeam::Radiant, 3))
        .expect("second bet");
    assert_eq!(balance(file.path(), 11_500), 2);
    assert!(matches!(
        repository
            .place_bet_atomic(request(1, 11_500, BettingTeam::Radiant, 5))
            .expect_err("each bet rechecks live balance"),
        BettingServiceRepositoryError::InsufficientBalance { balance: 2, .. }
    ));
}

#[test]
fn test_draft_participant_can_only_bet_on_own_team() {
    let file = fixture();
    add_pending(file.path(), 1, true);
    add_player(file.path(), 1, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 1, BettingTeam::Radiant, 10))
        .expect("first own-team bet");
    repository
        .place_bet_atomic(request(1, 1, BettingTeam::Radiant, 15))
        .expect("second own-team bet");
    let mut leveraged = request(1, 1, BettingTeam::Radiant, 5);
    leveraged.leverage = 2;
    repository
        .place_bet_atomic(leveraged)
        .expect("leveraged own-team bet");
    assert_eq!(
        repository
            .get_pending_bets(Some(GUILD), Some(1), NOW, Some(1))
            .expect("participant bets")
            .len(),
        3
    );
    assert!(matches!(
        repository
            .place_bet_atomic(request(1, 1, BettingTeam::Dire, 5))
            .expect_err("draft participant cannot oppose own team"),
        BettingServiceRepositoryError::ParticipantTeamOnly { .. }
    ));
}

#[test]
fn test_leverage_respects_max_debt() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 11_800, 100);
    let repository = repo(file.path());
    let mut excessive = request(1, 11_800, BettingTeam::Radiant, 150);
    excessive.leverage = 5;
    assert!(matches!(
        repository
            .place_bet_atomic(excessive)
            .expect_err("excessive leverage must reject"),
        BettingServiceRepositoryError::DebtLimit { .. }
    ));
    let mut allowed = request(1, 11_800, BettingTeam::Radiant, 100);
    allowed.leverage = 5;
    repository
        .place_bet_atomic(allowed)
        .expect("within max debt");
    assert_eq!(balance(file.path(), 11_800), -400);
    let mut retry = request(1, 11_800, BettingTeam::Radiant, 10);
    retry.leverage = 2;
    assert!(matches!(
        repository
            .place_bet_atomic(retry)
            .expect_err("debt blocks another wager"),
        BettingServiceRepositoryError::PlayerInDebt(11_800)
    ));
}

#[test]
fn test_in_debt_user_cannot_place_any_bet() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 12_000, 50);
    let repository = repo(file.path());
    let mut initial = request(1, 12_000, BettingTeam::Radiant, 100);
    initial.leverage = 5;
    repository
        .place_bet_atomic(initial)
        .expect("initial leveraged bet");
    assert_eq!(balance(file.path(), 12_000), -450);
    assert!(matches!(
        repository
            .place_bet_atomic(request(1, 12_000, BettingTeam::Radiant, 1))
            .expect_err("debt blocks 1x bet"),
        BettingServiceRepositoryError::PlayerInDebt(12_000)
    ));
    let mut leveraged = request(1, 12_000, BettingTeam::Radiant, 10);
    leveraged.leverage = 5;
    assert!(matches!(
        repository
            .place_bet_atomic(leveraged)
            .expect_err("debt blocks leveraged bet"),
        BettingServiceRepositoryError::PlayerInDebt(12_000)
    ));
}

#[test]
fn leveraged_loss_pushes_balance_into_debt() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    add_player(file.path(), 12_100, 20);
    let repository = repo(file.path());
    let mut bet = request(1, 12_100, BettingTeam::Radiant, 10);
    bet.leverage = 5;
    repository.place_bet_atomic(bet).expect("leveraged wager");
    assert_eq!(balance(file.path(), 12_100), -30);
    let result = repository
        .settle_pending_bets_atomic(
            12_101,
            Some(GUILD),
            NOW,
            Some(1),
            BettingTeam::Dire,
            BettingMode::House,
        )
        .expect("settle losing leveraged wager");
    assert_eq!(result.losers[0].effective_amount, 50);
    assert_eq!(balance(file.path(), 12_100), -30);
}

#[test]
fn leveraged_bet_deducts_effective_amount() {
    let file = fixture();
    add_pending(file.path(), 12_090, false);
    add_player(file.path(), 12_091, 100);
    let repository = repo(file.path());
    let mut bet = request(12_090, 12_091, BettingTeam::Radiant, 10);
    bet.leverage = 3;
    repository.place_bet_atomic(bet).expect("leveraged wager");
    assert_eq!(balance(file.path(), 12_091), 70);
    let pending = repository
        .get_pending_bets(Some(GUILD), Some(12_091), NOW, Some(12_090))
        .expect("pending leveraged wager");
    assert_eq!(pending[0].effective_amount(), 30);
}

#[test]
fn leveraged_bet_rejects_wager_above_maximum_debt() {
    let file = fixture();
    add_pending(file.path(), 12_092, false);
    add_player(file.path(), 12_093, 0);
    let repository = repo(file.path());
    let mut bet = request(12_092, 12_093, BettingTeam::Radiant, 200);
    bet.leverage = 5;
    assert!(matches!(
        repository.place_bet_atomic(bet),
        Err(BettingServiceRepositoryError::DebtLimit { .. })
    ));
    assert_eq!(balance(file.path(), 12_093), 0);
}

#[test]
fn leveraged_bet_lifecycle_blocks_new_wagers_until_debt_is_cleared() {
    let file = fixture();
    add_pending(file.path(), 2, false);
    add_pending(file.path(), 3, false);
    add_player(file.path(), 12_110, 50);
    let repository = repo(file.path());
    let mut initial = request(2, 12_110, BettingTeam::Radiant, 20);
    initial.leverage = 5;
    repository.place_bet_atomic(initial).expect("initial wager");
    assert_eq!(balance(file.path(), 12_110), -50);
    assert!(matches!(
        repository
            .place_bet_atomic(request(3, 12_110, BettingTeam::Radiant, 1))
            .expect_err("debt blocks follow-up wager"),
        BettingServiceRepositoryError::PlayerInDebt(12_110)
    ));
    Connection::open(file.path())
        .expect("open debt lifecycle fixture")
        .execute(
            "UPDATE players SET jopacoin_balance=10
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, 12_110],
        )
        .expect("restore positive balance");
    repository
        .place_bet_atomic(request(3, 12_110, BettingTeam::Radiant, 1))
        .expect("wager after debt is cleared");
    assert_eq!(balance(file.path(), 12_110), 9);
}

#[test]
fn pool_leverage_pays_from_effective_stake() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        12_120,
        12_121,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (12_121, BettingTeam::Radiant, 10, 3),
            (12_122, BettingTeam::Dire, 10, 1),
        ],
    );
    assert_eq!(result.winners[0].effective_amount, 30);
    assert_eq!(result.winners[0].payout, 40);
    assert_eq!(balance(file.path(), 12_121), 1_010);
}

#[test]
fn pool_leverage_splits_multiple_winners_by_effective_stake() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        12_130,
        12_131,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (12_131, BettingTeam::Radiant, 10, 1),
            (12_132, BettingTeam::Radiant, 10, 2),
            (12_133, BettingTeam::Dire, 30, 1),
        ],
    );
    let payouts = result
        .winners
        .iter()
        .map(|winner| (winner.discord_id, winner.payout))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(payouts, BTreeMap::from([(12_131, 20), (12_132, 40)]));
}

fn assert_spectator_dual_team(is_draft: Option<bool>) {
    let file = fixture();
    let draft_fragment = is_draft.map_or(String::new(), |is_draft| {
        format!(",\"is_draft\":{is_draft}")
    });
    add_pending_payload(
        file.path(),
        1,
        &format!(
            "{{\"radiant_team_ids\":[1,2,3,4,5],\"dire_team_ids\":[6,7,8,9,10],\"bet_lock_until\":{}{} }}",
            NOW + 1_000,
            draft_fragment
        ),
    );
    add_player(file.path(), 12_600, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(1, 12_600, BettingTeam::Dire, 10))
        .expect("first spectator bet");
    repository
        .place_bet_atomic(request(1, 12_600, BettingTeam::Radiant, 5))
        .expect("opposite spectator bet");
    let teams = repository
        .get_pending_bets(Some(GUILD), Some(12_600), NOW, Some(1))
        .expect("dual-team bets")
        .into_iter()
        .map(|bet| bet.team)
        .collect::<Vec<_>>();
    assert_eq!(teams.len(), 2);
    assert!(teams.contains(&BettingTeam::Radiant));
    assert!(teams.contains(&BettingTeam::Dire));
}

#[test]
fn test_shuffle_spectator_can_add_opposite_team_after_first_bet() {
    assert_spectator_dual_team(Some(false));
}

#[test]
fn test_draft_spectator_can_bet_both_teams() {
    assert_spectator_dual_team(Some(true));
}

#[test]
fn test_legacy_pending_payload_allows_spectator_dual_team() {
    assert_spectator_dual_team(None);
}

#[test]
fn integration_repo_betting_flow_settles_pending_wager() {
    let file = fixture();
    add_pending(file.path(), 50_001, false);
    add_player(file.path(), 50_101, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(50_001, 50_101, BettingTeam::Radiant, 10))
        .expect("integration wager");
    let settlement = repository
        .settle_pending_bets_atomic(
            50_101,
            Some(GUILD),
            NOW,
            Some(50_001),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .expect("integration settlement");
    assert_eq!(settlement.winners.len(), 1);
    assert_eq!(settlement.winners[0].payout, 20);
    assert_eq!(balance(file.path(), 50_101), 110);
}

#[test]
fn integration_repo_manual_same_team_wagers_accumulate() {
    let file = fixture();
    add_pending(file.path(), 50_002, false);
    add_player(file.path(), 50_102, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(50_002, 50_102, BettingTeam::Radiant, 10))
        .expect("first same-team wager");
    repository
        .place_bet_atomic(request(50_002, 50_102, BettingTeam::Radiant, 5))
        .expect("second same-team wager");
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(50_102), NOW, Some(50_002))
        .expect("same-team wagers");
    assert_eq!(bets.len(), 2);
    assert_eq!(balance(file.path(), 50_102), 85);
}

#[test]
fn integration_repo_spectator_can_bet_both_teams_on_shuffle() {
    let file = fixture();
    add_pending_payload(
        file.path(),
        50_003,
        &format!(
            "{{\"radiant_team_ids\":[1],\"dire_team_ids\":[2],\"bet_lock_until\":{}}}",
            NOW + 100
        ),
    );
    add_player(file.path(), 50_103, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(50_003, 50_103, BettingTeam::Radiant, 5))
        .expect("spectator Radiant wager");
    repository
        .place_bet_atomic(request(50_003, 50_103, BettingTeam::Dire, 5))
        .expect("spectator Dire wager");
    let bets = repository
        .get_pending_bets(Some(GUILD), Some(50_103), NOW, Some(50_003))
        .expect("spectator both-team wagers");
    assert_eq!(
        bets.iter().map(|bet| bet.team).collect::<Vec<_>>(),
        vec![BettingTeam::Radiant, BettingTeam::Dire]
    );
    assert_eq!(balance(file.path(), 50_103), 90);
}

#[test]
fn integration_repo_participant_team_and_lock_rejections_are_atomic() {
    let file = fixture();
    add_pending_payload(
        file.path(),
        50_004,
        &format!(
            "{{\"radiant_team_ids\":[1],\"dire_team_ids\":[2],\"bet_lock_until\":{}}}",
            NOW + 100
        ),
    );
    add_player(file.path(), 1, 100);
    let repository = repo(file.path());
    assert!(matches!(
        repository
            .place_bet_atomic(request(50_004, 1, BettingTeam::Dire, 5))
            .expect_err("participant team restriction"),
        BettingServiceRepositoryError::ParticipantTeamOnly { .. }
    ));
    add_pending_payload(
        file.path(),
        50_005,
        &format!(
            "{{\"radiant_team_ids\":[1],\"dire_team_ids\":[2],\"bet_lock_until\":{}}}",
            NOW
        ),
    );
    assert!(matches!(
        repository
            .place_bet_atomic(request(50_005, 1, BettingTeam::Radiant, 5))
            .expect_err("bet lock restriction"),
        BettingServiceRepositoryError::BettingClosed
    ));
    assert_eq!(balance(file.path(), 1), 100);
}

#[test]
fn batch_repo_uses_one_locked_snapshot_for_repeated_candidates() {
    let file = fixture();
    add_pending(file.path(), 50_006, false);
    add_player(file.path(), 50_106, 100);
    let outcome = repo(file.path())
        .place_automatic_bets_atomic(
            Some(GUILD),
            Some(50_006),
            NOW,
            &[
                AutomaticBetRequest {
                    discord_id: 50_106,
                    team: BettingTeam::Radiant,
                    percentage: 10,
                },
                AutomaticBetRequest {
                    discord_id: 50_106,
                    team: BettingTeam::Radiant,
                    percentage: 10,
                },
            ],
        )
        .expect("snapshot batch");
    assert_eq!(outcome.balance_snapshot[&50_106], 100);
    assert_eq!(
        outcome
            .created
            .iter()
            .map(|bet| bet.amount)
            .collect::<Vec<_>>(),
        vec![10, 10]
    );
    assert_eq!(balance(file.path(), 50_106), 80);
}

#[test]
fn batch_repo_isolates_debt_candidate_and_keeps_odds_sequential() {
    let file = fixture();
    add_pending(file.path(), 50_007, false);
    add_player(file.path(), 50_107, 100);
    add_player(file.path(), 50_108, -1);
    let outcome = repo(file.path())
        .place_automatic_amount_bets_atomic(
            Some(GUILD),
            Some(50_007),
            NOW,
            &[
                AutomaticAmountBetRequest {
                    discord_id: 50_107,
                    team: BettingTeam::Radiant,
                    amount: 10,
                },
                AutomaticAmountBetRequest {
                    discord_id: 50_108,
                    team: BettingTeam::Dire,
                    amount: 10,
                },
            ],
            500,
        )
        .expect("isolated debt candidate");
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.skipped, vec![50_108]);
    assert_eq!(outcome.created[0].odds_at_placement, None);
}

#[test]
fn batch_repo_exact_basis_points_round_and_keep_failed_candidate_out_of_odds() {
    let file = fixture();
    add_pending(file.path(), 50_008, true);
    for (id, balance) in [(50_109, 250), (50_110, 150), (50_111, 50)] {
        add_player(file.path(), id, balance);
    }
    let outcome = repo(file.path())
        .place_automatic_basis_point_bets_atomic(
            Some(GUILD),
            Some(50_008),
            NOW,
            &[
                AutomaticBasisPointBetRequest {
                    discord_id: 50_109,
                    team: BettingTeam::Radiant,
                    basis_points: 100,
                },
                AutomaticBasisPointBetRequest {
                    discord_id: 50_110,
                    team: BettingTeam::Radiant,
                    basis_points: 100,
                },
                AutomaticBasisPointBetRequest {
                    discord_id: 50_111,
                    team: BettingTeam::Dire,
                    basis_points: 100,
                },
            ],
            500,
        )
        .expect("basis point batch");
    assert_eq!(
        outcome
            .created
            .iter()
            .map(|bet| bet.amount)
            .collect::<Vec<_>>(),
        vec![2, 2]
    );
    assert_eq!(outcome.skipped, vec![50_111]);
    assert_eq!(outcome.created[1].odds_at_placement, Some(1.0));
}

fn blind_candidates(ids: impl IntoIterator<Item = i64>) -> Vec<BlindBetCandidate> {
    ids.into_iter()
        .enumerate()
        .map(|(index, discord_id)| BlindBetCandidate {
            discord_id,
            team: if index < 5 {
                BettingTeam::Radiant
            } else {
                BettingTeam::Dire
            },
            ante_override: None,
        })
        .collect()
}

#[test]
fn test_create_auto_blind_bets_threshold() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for (index, discord_id) in (12_400..12_410).enumerate() {
        add_player(
            file.path(),
            discord_id,
            if index % 2 == 0 { 100 } else { 30 },
        );
    }
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &blind_candidates(12_400..12_410),
            BlindBetPolicy::default(),
        )
        .expect("threshold batch");
    assert_eq!(outcome.created.len(), 5);
    assert_eq!(outcome.skipped.len(), 5);
    assert!(
        outcome
            .skipped
            .iter()
            .all(|skip| skip.reason.contains("threshold"))
    );
}

#[test]
fn test_create_auto_blind_bets_in_debt() {
    let file = fixture();
    add_pending(file.path(), 1, false);
    for (index, discord_id) in (12_600..12_610).enumerate() {
        add_player(file.path(), discord_id, if index < 5 { 100 } else { -100 });
    }
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(1),
            NOW,
            &blind_candidates(12_600..12_610),
            BlindBetPolicy::default(),
        )
        .expect("debt-skipping batch");
    assert_eq!(outcome.created.len(), 5);
    assert_eq!(outcome.skipped.len(), 5);
}

#[test]
fn race_fix5_auto_blind_rejects_debt_candidate_after_balance_drain() {
    let file = fixture();
    add_pending(file.path(), 50_009, false);
    add_player(file.path(), 50_117, -1);
    let outcome = repo(file.path())
        .place_automatic_bets_atomic(
            Some(GUILD),
            Some(50_009),
            NOW,
            &[AutomaticBetRequest {
                discord_id: 50_117,
                team: BettingTeam::Radiant,
                percentage: 10,
            }],
        )
        .expect("debt candidate race policy");
    assert!(outcome.created.is_empty());
    assert_eq!(outcome.skipped, vec![50_117]);
    assert_eq!(balance(file.path(), 50_117), -1);
}

#[test]
fn race_fix5_auto_blind_uses_atomic_balance_check() {
    let file = fixture();
    add_pending(file.path(), 50_010, false);
    add_player(file.path(), 50_118, 5);
    let outcome = repo(file.path())
        .place_automatic_amount_bets_atomic(
            Some(GUILD),
            Some(50_010),
            NOW,
            &[AutomaticAmountBetRequest {
                discord_id: 50_118,
                team: BettingTeam::Radiant,
                amount: 10,
            }],
            500,
        )
        .expect("atomic balance candidate");
    assert!(outcome.created.is_empty());
    assert_eq!(outcome.skipped, vec![50_118]);
    assert_eq!(balance(file.path(), 50_118), 5);
    assert_eq!(
        repo(file.path())
            .get_pending_bets(Some(GUILD), Some(50_118), NOW, Some(50_010))
            .unwrap(),
        Vec::<PendingBetRecord>::new()
    );
}

#[test]
fn test_bomb_pot_mandatory_ante_no_threshold() {
    let file = fixture();
    let ids = 2_001..=2_010;
    for discord_id in ids.clone() {
        add_player(file.path(), discord_id, 20);
    }
    let candidates = blind_candidates(ids);
    let repository = repo(file.path());
    let normal = repository
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            None,
            NOW,
            &candidates,
            BlindBetPolicy::default(),
        )
        .expect("normal batch");
    assert!(normal.created.is_empty());
    assert_eq!(normal.skipped.len(), 10);
    let bomb = repository
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            None,
            NOW + 1,
            &candidates,
            BlindBetPolicy {
                is_bomb_pot: true,
                ..BlindBetPolicy::default()
            },
        )
        .expect("bomb-pot batch");
    assert_eq!(bomb.created.len(), 10);
    assert!(bomb.skipped.is_empty());
    assert_eq!(bomb.total_radiant + bomb.total_dire, 130);
}

#[test]
fn test_bomb_pot_zero_balance_still_antes() {
    let file = fixture();
    add_player(file.path(), 6_001, 0);
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            None,
            NOW,
            &[BlindBetCandidate {
                discord_id: 6_001,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy {
                is_bomb_pot: true,
                ..BlindBetPolicy::default()
            },
        )
        .expect("zero-balance ante");
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.total_radiant, 10);
    assert_eq!(balance(file.path(), 6_001), -10);
}

#[test]
fn test_bomb_pot_player_already_in_debt_can_ante() {
    let file = fixture();
    add_player(file.path(), 7_001, -100);
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            None,
            NOW,
            &[BlindBetCandidate {
                discord_id: 7_001,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy {
                is_bomb_pot: true,
                ..BlindBetPolicy::default()
            },
        )
        .expect("debt ante");
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.total_radiant, 10);
    assert_eq!(balance(file.path(), 7_001), -110);
}

fn settle_test_bets(
    file: &NamedTempFile,
    pending_match_id: i64,
    match_id: i64,
    mode: BettingMode,
    winning_team: BettingTeam,
    bets: &[(i64, BettingTeam, i64, i64)],
) -> SettlementResult {
    add_pending(file.path(), pending_match_id, false);
    let bettors = bets
        .iter()
        .map(|(discord_id, _, _, _)| *discord_id)
        .collect::<BTreeSet<_>>();
    for discord_id in bettors {
        add_player(file.path(), discord_id, 1_000);
    }
    let repository = repo(file.path());
    for (discord_id, team, amount, leverage) in bets {
        let mut request = request(pending_match_id, *discord_id, *team, *amount);
        request.leverage = *leverage;
        repository
            .place_bet_atomic(request)
            .expect("place settlement test bet");
    }
    repository
        .settle_pending_bets_atomic(
            match_id,
            Some(GUILD),
            NOW,
            Some(pending_match_id),
            winning_team,
            mode,
        )
        .expect("settle settlement test bets")
}

#[test]
fn pool_settlement_pays_proportionally_to_effective_winner_stake() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_001,
        30_101,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_201, BettingTeam::Radiant, 10, 1),
            (30_202, BettingTeam::Dire, 20, 1),
        ],
    );
    assert_eq!(result.winners[0].payout, 30);
    assert_eq!(result.winners[0].effective_amount, 10);
    assert_eq!(balance(file.path(), 30_201), 1_020);
    assert_eq!(balance(file.path(), 30_202), 980);
}

#[test]
fn pool_settlement_splits_multiple_winners_by_effective_stake() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_002,
        30_102,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_211, BettingTeam::Radiant, 10, 1),
            (30_212, BettingTeam::Radiant, 20, 1),
            (30_213, BettingTeam::Dire, 30, 1),
        ],
    );
    let payouts = result
        .winners
        .iter()
        .map(|winner| (winner.discord_id, winner.payout))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(payouts, BTreeMap::from([(30_211, 20), (30_212, 40)]));
    assert_eq!(balance(file.path(), 30_213), 970);
}

#[test]
fn pool_settlement_uses_integer_payouts_without_fractional_coins() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_003,
        30_103,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_221, BettingTeam::Radiant, 10, 1),
            (30_222, BettingTeam::Dire, 11, 1),
        ],
    );
    assert_eq!(result.winners[0].payout, 21);
    assert!(result.winners.iter().all(|winner| winner.payout >= 0));
}

#[test]
fn pool_settlement_allocates_split_bets_without_rounding_exploit() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_004,
        30_104,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_231, BettingTeam::Radiant, 4, 1),
            (30_231, BettingTeam::Radiant, 6, 1),
            (30_232, BettingTeam::Dire, 11, 1),
        ],
    );
    assert_eq!(result.winners.len(), 2);
    assert_eq!(
        result
            .winners
            .iter()
            .map(|winner| winner.payout)
            .sum::<i64>(),
        21
    );
    assert_eq!(
        result
            .winners
            .iter()
            .map(|winner| winner.effective_amount)
            .sum::<i64>(),
        10
    );
}

#[test]
fn house_settlement_handles_multiple_bets_for_one_bettor() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_005,
        30_105,
        BettingMode::House,
        BettingTeam::Radiant,
        &[
            (30_241, BettingTeam::Radiant, 10, 1),
            (30_241, BettingTeam::Radiant, 20, 1),
            (30_242, BettingTeam::Dire, 30, 1),
        ],
    );
    assert_eq!(
        result
            .winners
            .iter()
            .map(|winner| winner.payout)
            .sum::<i64>(),
        60
    );
    assert_eq!(balance(file.path(), 30_241), 1_030);
}

#[test]
fn house_settlement_pays_default_gross_payout() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_023,
        30_123,
        BettingMode::House,
        BettingTeam::Radiant,
        &[
            (30_245, BettingTeam::Radiant, 10, 1),
            (30_246, BettingTeam::Dire, 10, 1),
        ],
    );
    assert_eq!(result.winners[0].payout, 20);
    assert_eq!(balance(file.path(), 30_245), 1_010);
}

#[test]
fn pool_settlement_handles_multiple_bets_for_one_bettor() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_006,
        30_106,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_251, BettingTeam::Radiant, 10, 1),
            (30_251, BettingTeam::Radiant, 20, 1),
            (30_252, BettingTeam::Dire, 30, 1),
        ],
    );
    assert_eq!(
        result
            .winners
            .iter()
            .map(|winner| winner.payout)
            .sum::<i64>(),
        60
    );
    assert_eq!(balance(file.path(), 30_251), 1_030);
}

#[test]
fn multiple_pending_bets_refund_together_and_restore_wallets() {
    let file = fixture();
    add_pending(file.path(), 30_007, false);
    add_player(file.path(), 30_261, 100);
    add_player(file.path(), 30_262, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_007, 30_261, BettingTeam::Radiant, 20))
        .expect("place blind refund bet");
    let mut manual = request(30_007, 30_262, BettingTeam::Dire, 15);
    manual.is_blind = false;
    repository
        .place_bet_atomic(manual)
        .expect("place manual refund bet");
    assert_eq!(
        repository
            .refund_pending_bets_atomic(Some(GUILD), NOW, Some(30_007))
            .unwrap(),
        2
    );
    assert_eq!(balance(file.path(), 30_261), 100);
    assert_eq!(balance(file.path(), 30_262), 100);
}

#[test]
fn blind_bet_rounding_uses_python_ties_to_even_amounts() {
    let file = fixture();
    add_pending(file.path(), 30_008, false);
    add_player(file.path(), 30_271, 55);
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(30_008),
            NOW,
            &[BlindBetCandidate {
                discord_id: 30_271,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy::default(),
        )
        .unwrap();
    assert_eq!(outcome.created[0].amount, 6);
    assert_eq!(balance(file.path(), 30_271), 49);
}

#[test]
fn blind_bet_settlement_credits_the_stored_blind_row() {
    let file = fixture();
    add_pending(file.path(), 30_009, false);
    add_player(file.path(), 30_281, 100);
    let repository = repo(file.path());
    let outcome = repository
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(30_009),
            NOW,
            &[BlindBetCandidate {
                discord_id: 30_281,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy::default(),
        )
        .unwrap();
    assert!(outcome.created[0].is_blind);
    let settled = repository
        .settle_pending_bets_atomic(
            30_109,
            Some(GUILD),
            NOW,
            Some(30_009),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    assert_eq!(settled.winners[0].payout, 20);
    assert_eq!(balance(file.path(), 30_281), 110);
}

#[test]
fn mixed_blind_and_manual_bets_are_refunded_on_abort() {
    let file = fixture();
    add_pending(file.path(), 30_010, false);
    add_player(file.path(), 30_291, 100);
    add_player(file.path(), 30_292, 100);
    let repository = repo(file.path());
    repository
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(30_010),
            NOW,
            &[BlindBetCandidate {
                discord_id: 30_291,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy::default(),
        )
        .unwrap();
    repository
        .place_bet_atomic(request(30_010, 30_292, BettingTeam::Dire, 12))
        .unwrap();
    assert_eq!(
        repository
            .refund_pending_bets_atomic(Some(GUILD), NOW, Some(30_010))
            .unwrap(),
        2
    );
    assert_eq!(balance(file.path(), 30_291), 100);
    assert_eq!(balance(file.path(), 30_292), 100);
}

#[test]
fn bomb_pot_blinds_use_higher_percentage_and_mandatory_ante() {
    let file = fixture();
    add_pending(file.path(), 30_011, false);
    add_player(file.path(), 30_301, 100);
    let outcome = repo(file.path())
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(30_011),
            NOW,
            &[BlindBetCandidate {
                discord_id: 30_301,
                team: BettingTeam::Radiant,
                ante_override: None,
            }],
            BlindBetPolicy {
                is_bomb_pot: true,
                ..BlindBetPolicy::default()
            },
        )
        .unwrap();
    assert_eq!(outcome.created[0].amount, 25);
    assert_eq!(balance(file.path(), 30_301), 75);
}

#[test]
fn bomb_pot_losing_blind_is_tagged_without_a_payout() {
    let file = fixture();
    add_pending(file.path(), 30_012, false);
    add_player(file.path(), 30_311, 100);
    let repository = repo(file.path());
    let outcome = repository
        .create_auto_blind_bets_atomic(
            Some(GUILD),
            Some(30_012),
            NOW,
            &[BlindBetCandidate {
                discord_id: 30_311,
                team: BettingTeam::Dire,
                ante_override: None,
            }],
            BlindBetPolicy {
                is_bomb_pot: true,
                ..BlindBetPolicy::default()
            },
        )
        .unwrap();
    let result = repository
        .settle_pending_bets_atomic(
            30_112,
            Some(GUILD),
            NOW,
            Some(30_012),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    assert!(result.winners.is_empty());
    assert_eq!(result.losers[0].effective_amount, outcome.created[0].amount);
    let match_id: Option<i64> = Connection::open(file.path())
        .unwrap()
        .query_row(
            "SELECT match_id FROM bets WHERE bet_id=?1",
            [outcome.created[0].bet_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(match_id, Some(30_112));
}

#[test]
fn house_settlement_double_retry_is_idempotent() {
    let file = fixture();
    let first = settle_test_bets(
        &file,
        30_013,
        30_113,
        BettingMode::House,
        BettingTeam::Radiant,
        &[
            (30_321, BettingTeam::Radiant, 10, 1),
            (30_322, BettingTeam::Dire, 10, 1),
        ],
    );
    let retry = repo(file.path())
        .settle_pending_bets_atomic(
            30_113,
            Some(GUILD),
            NOW,
            Some(30_013),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    assert_eq!(first.winners[0].payout, 20);
    assert_eq!(retry, SettlementResult::default());
    assert_eq!(balance(file.path(), 30_321), 1_010);
}

#[test]
fn pool_settlement_double_retry_is_idempotent() {
    let file = fixture();
    let first = settle_test_bets(
        &file,
        30_014,
        30_114,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_331, BettingTeam::Radiant, 10, 1),
            (30_332, BettingTeam::Dire, 20, 1),
        ],
    );
    let retry = repo(file.path())
        .settle_pending_bets_atomic(
            30_114,
            Some(GUILD),
            NOW,
            Some(30_014),
            BettingTeam::Radiant,
            BettingMode::Pool,
        )
        .unwrap();
    assert_eq!(first.winners[0].payout, 30);
    assert_eq!(retry, SettlementResult::default());
    assert_eq!(balance(file.path(), 30_331), 1_020);
}

#[test]
fn settlement_after_refund_pays_nobody() {
    let file = fixture();
    add_pending(file.path(), 30_015, false);
    add_player(file.path(), 30_341, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_015, 30_341, BettingTeam::Radiant, 10))
        .unwrap();
    repository
        .refund_pending_bets_atomic(Some(GUILD), NOW, Some(30_015))
        .unwrap();
    let result = repository
        .settle_pending_bets_atomic(
            30_115,
            Some(GUILD),
            NOW,
            Some(30_015),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    assert_eq!(result, SettlementResult::default());
    assert_eq!(balance(file.path(), 30_341), 100);
}

#[test]
fn settlement_with_no_pending_state_is_a_noop() {
    let file = fixture();
    let result = repo(file.path())
        .settle_pending_bets_atomic(
            30_116,
            Some(GUILD),
            NOW,
            Some(30_016),
            BettingTeam::Dire,
            BettingMode::Pool,
        )
        .unwrap();
    assert_eq!(result, SettlementResult::default());
}

#[test]
fn pool_payout_never_exceeds_pool_by_more_than_integer_rounding() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_017,
        30_117,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_351, BettingTeam::Radiant, 7, 1),
            (30_352, BettingTeam::Radiant, 5, 1),
            (30_353, BettingTeam::Dire, 9, 1),
        ],
    );
    let payouts = result
        .winners
        .iter()
        .map(|winner| winner.payout)
        .sum::<i64>();
    // Each winner is rounded independently like Python's integer ceiling;
    // the aggregate can therefore exceed the pool by at most one coin per
    // split winner, but never by an unbounded amount.
    assert!(payouts <= 22);
    assert!(payouts >= 21);
}

#[test]
fn pool_winner_balances_reflect_exact_distribution() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_018,
        30_118,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_361, BettingTeam::Radiant, 7, 1),
            (30_362, BettingTeam::Radiant, 5, 1),
            (30_363, BettingTeam::Dire, 9, 1),
        ],
    );
    for winner in result.winners {
        assert_eq!(
            balance(file.path(), winner.discord_id),
            1_000 - winner.effective_amount + winner.payout
        );
    }
}

#[test]
fn house_profit_uses_configured_multiplier() {
    let file = fixture();
    add_pending(file.path(), 30_019, false);
    add_player(file.path(), 30_371, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_019, 30_371, BettingTeam::Radiant, 10))
        .unwrap();
    let result = repository
        .settle_pending_bets_with_adjustments_atomic(
            30_119,
            Some(GUILD),
            NOW,
            Some(30_019),
            BettingTeam::Radiant,
            BettingMode::House,
            &BetSettlementAdjustments {
                house_payout_multiplier: 1.5,
                ..BetSettlementAdjustments::default()
            },
        )
        .unwrap();
    assert_eq!(result.winners[0].payout, 25);
}

#[test]
fn house_settlement_event_multiplier_scales_winning_gross_payout() {
    let file = fixture();
    add_pending(file.path(), 30_021, false);
    add_player(file.path(), 30_391, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_021, 30_391, BettingTeam::Radiant, 20))
        .expect("event house bet");
    let result = repository
        .settle_pending_bets_with_adjustments_atomic(
            30_121,
            Some(GUILD),
            NOW,
            Some(30_021),
            BettingTeam::Radiant,
            BettingMode::House,
            &BetSettlementAdjustments {
                payout_multiplier: 0.5,
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("event house settlement");
    assert_eq!(result.winners[0].payout, 20);
    assert_eq!(balance(file.path(), 30_391), 100);
}

#[test]
fn pool_settlement_event_multiplier_scales_and_persists_winning_payout() {
    let file = fixture();
    add_pending(file.path(), 30_022, false);
    add_player(file.path(), 30_392, 100);
    add_player(file.path(), 30_393, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_022, 30_392, BettingTeam::Radiant, 30))
        .expect("event pool winner");
    repository
        .place_bet_atomic(request(30_022, 30_393, BettingTeam::Dire, 20))
        .expect("event pool loser");
    let result = repository
        .settle_pending_bets_with_adjustments_atomic(
            30_122,
            Some(GUILD),
            NOW,
            Some(30_022),
            BettingTeam::Radiant,
            BettingMode::Pool,
            &BetSettlementAdjustments {
                payout_multiplier: 0.5,
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("event pool settlement");
    assert_eq!(result.winners[0].payout, 25);
    assert_eq!(balance(file.path(), 30_392), 95);
    let stored: i64 = Connection::open(file.path())
        .expect("open event pool fixture")
        .query_row(
            "SELECT payout FROM bets WHERE discord_id=?1 AND match_id=?2",
            params![30_392, 30_122],
            |row| row.get(0),
        )
        .expect("stored event payout");
    assert_eq!(stored, 25);
}

#[test]
fn invalid_event_multiplier_falls_back_to_default_gross_payout() {
    let file = fixture();
    add_pending(file.path(), 30_023, false);
    add_player(file.path(), 30_394, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_023, 30_394, BettingTeam::Radiant, 10))
        .expect("invalid multiplier bet");
    let result = repository
        .settle_pending_bets_with_adjustments_atomic(
            30_123,
            Some(GUILD),
            NOW,
            Some(30_023),
            BettingTeam::Radiant,
            BettingMode::House,
            &BetSettlementAdjustments {
                payout_multiplier: f64::NAN,
                ..BetSettlementAdjustments::default()
            },
        )
        .expect("invalid multiplier settlement");
    assert_eq!(result.winners[0].payout, 20);
}

#[test]
fn house_leverage_payout_scales_with_effective_stake() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_020,
        30_120,
        BettingMode::House,
        BettingTeam::Radiant,
        &[
            (30_381, BettingTeam::Radiant, 10, 3),
            (30_382, BettingTeam::Dire, 30, 1),
        ],
    );
    assert_eq!(result.winners[0].effective_amount, 30);
    assert_eq!(result.winners[0].payout, 60);
}

#[test]
fn leveraged_win_payout_reports_effective_tier_and_balance() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_024,
        30_124,
        BettingMode::House,
        BettingTeam::Radiant,
        &[(30_384, BettingTeam::Radiant, 10, 3)],
    );
    assert_eq!(result.winners[0].effective_amount, 30);
    assert_eq!(result.winners[0].payout, 60);
    assert_eq!(balance(file.path(), 30_384), 1_030);
}

#[test]
fn pool_without_a_winning_side_burns_effective_losing_stakes() {
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_021,
        30_121,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[(30_391, BettingTeam::Dire, 10, 2)],
    );
    assert!(result.winners.is_empty());
    assert_eq!(result.losers[0].effective_amount, 20);
    assert_eq!(balance(file.path(), 30_391), 980);
}

#[test]
fn settled_legacy_bet_rows_are_tagged_once_without_double_pay() {
    let file = fixture();
    add_pending(file.path(), 30_022, false);
    add_player(file.path(), 30_401, 100);
    let repository = repo(file.path());
    repository
        .place_bet_atomic(request(30_022, 30_401, BettingTeam::Radiant, 10))
        .unwrap();
    let first = repository
        .settle_pending_bets_atomic(
            30_122,
            Some(GUILD),
            NOW,
            Some(30_022),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    let tagged: i64 = Connection::open(file.path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bets WHERE match_id=30_122",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let second = repository
        .settle_pending_bets_atomic(
            30_122,
            Some(GUILD),
            NOW,
            Some(30_022),
            BettingTeam::Radiant,
            BettingMode::House,
        )
        .unwrap();
    assert_eq!(first.winners.len(), 1);
    assert_eq!(tagged, 1);
    assert!(second.winners.is_empty());
    assert_eq!(balance(file.path(), 30_401), 110);
}

#[test]
fn pool_settlement_reproduces_python_float_ceiling_ulp() {
    // Python parity pin (bet_repository.py::_calculate_pool_payouts): the
    // raw payout is an IEEE double, and `(90 / 676) * 676` lands 1 ULP above
    // 90.0, so `math.ceil` credits 91 on a pure stake refund. CPython vector:
    //   {stake 90 -> 91, stake 586 -> 586}
    // An exact integer ceiling (the pre-parity Rust behavior) pays 90 here.
    let file = fixture();
    let result = settle_test_bets(
        &file,
        30_020,
        30_120,
        BettingMode::Pool,
        BettingTeam::Radiant,
        &[
            (30_371, BettingTeam::Radiant, 90, 1),
            (30_372, BettingTeam::Radiant, 586, 1),
        ],
    );
    let mut payouts = result
        .winners
        .iter()
        .map(|winner| (winner.effective_amount, winner.payout))
        .collect::<Vec<_>>();
    payouts.sort_unstable();
    assert_eq!(payouts, [(90, 91), (586, 586)]);
}
