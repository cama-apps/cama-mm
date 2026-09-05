use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 7_001;
const OTHER_GUILD: i64 = 7_002;
const NOW: i64 = 1_786_051_200;

struct Fixture {
    file: NamedTempFile,
    repository: ReferralRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary referral database");
        let connection = Connection::open(file.path()).expect("open referral fixture");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     wins INTEGER DEFAULT 0,
                     losses INTEGER DEFAULT 0,
                     jopacoin_balance INTEGER DEFAULT 3,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     team1_players TEXT NOT NULL,
                     team2_players TEXT NOT NULL,
                     winning_team INTEGER,
                     match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     lobby_type TEXT DEFAULT 'shuffle',
                     balancing_rating_system TEXT DEFAULT 'glicko',
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     pending_match_id INTEGER,
                     jc_changes TEXT,
                     lobby_kind TEXT
                 );
                 CREATE UNIQUE INDEX uq_matches_guild_pending_match
                     ON matches(guild_id, pending_match_id)
                     WHERE pending_match_id IS NOT NULL;
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     team_number INTEGER,
                     won INTEGER,
                     side TEXT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (match_id, discord_id)
                 );
                 CREATE TABLE referrals (
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     referred_id INTEGER NOT NULL,
                     referrer_id INTEGER NOT NULL,
                     created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     rewarded_match_id INTEGER,
                     reward_amount INTEGER,
                     rewarded_at TIMESTAMP,
                     PRIMARY KEY (guild_id, referred_id),
                     CHECK (referrer_id != referred_id)
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
                     created_at INTEGER NOT NULL DEFAULT 0
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
                 CREATE TRIGGER trg_referral_test_ledger
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         NEW.guild_id, 'player', NEW.discord_id,
                         COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                         COALESCE(OLD.jopacoin_balance, 0), COALESCE(NEW.jopacoin_balance, 0),
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                  'balance_update'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;",
            )
            .expect("create disposable referral schema");
        drop(connection);
        let repository = ReferralRepository::new(file.path());
        Self { file, repository }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn add_player(&self, discord_id: i64, guild_id: i64) {
        self.add_player_with_balance(discord_id, guild_id, 3);
    }

    fn add_player_with_balance(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, wins, losses, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, 0, 0, ?4)",
                params![discord_id, guild_id, format!("Player{discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn remove_player(&self, discord_id: i64, guild_id: i64) {
        self.connection()
            .execute(
                "DELETE FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
            )
            .expect("remove player");
    }

    fn context_count(&self) -> i64 {
        self.connection()
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get(0)
            })
            .expect("ledger context count")
    }

    fn set_penalty_games(&self, discord_id: i64, guild_id: i64, remaining: i64) {
        self.connection()
            .execute(
                "INSERT INTO bankruptcy_state (
                     discord_id, guild_id, penalty_games_remaining, bankruptcy_count
                 ) VALUES (?1, ?2, ?3, 1)",
                params![discord_id, guild_id, remaining],
            )
            .expect("seed bankruptcy state");
    }

    fn all_ledger_entries(&self) -> Vec<(i64, String, i64)> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT account_id, source, delta FROM economy_ledger_entries
                 ORDER BY ledger_id",
            )
            .expect("prepare ledger");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query ledger")
            .collect::<Result<_, _>>()
            .expect("ledger rows")
    }
}

fn request<'a>(
    radiant_ids: &'a [i64],
    dire_ids: &'a [i64],
    guild_id: i64,
    pending_match_id: i64,
) -> RecordReferralMatchRequest<'a> {
    RecordReferralMatchRequest {
        guild_id: Some(guild_id),
        radiant_ids,
        dire_ids,
        winning_side: MatchSide::Radiant,
        pending_match_id,
        recorded_at: NOW,
        deductions: ProfitDeductionPolicy::none(),
    }
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

fn enroll_then_register(fixture: &Fixture, referrer_id: i64, referred_id: i64, guild_id: i64) {
    fixture.add_player(referrer_id, guild_id);
    fixture
        .repository
        .create_referral(referrer_id, referred_id, Some(guild_id))
        .expect("enroll referred player");
    fixture.add_player(referred_id, guild_id);
}

#[test]
fn test_referrals_schema_is_guild_scoped() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .referral_schema_columns()
            .expect("referral columns"),
        [
            "guild_id",
            "referred_id",
            "referrer_id",
            "created_at",
            "rewarded_match_id",
            "reward_amount",
            "rewarded_at",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
    );
}

#[test]
fn test_create_referral_requires_registered_referrer_and_unregistered_target() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    fixture
        .repository
        .create_referral(10, 20, Some(GUILD))
        .expect("first enrollment");
    assert!(matches!(
        fixture.repository.create_referral(10, 20, Some(GUILD)),
        Err(ReferralRepositoryError::AlreadyReferred)
    ));
}

#[test]
fn test_create_referral_rejects_unregistered_referrer() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.repository.create_referral(10, 20, Some(GUILD)),
        Err(ReferralRepositoryError::ReferrerNotRegistered)
    ));
}

#[test]
fn test_create_referral_allows_registered_target_without_recorded_games() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    fixture.add_player(20, GUILD);
    fixture
        .repository
        .create_referral(10, 20, Some(GUILD))
        .expect("zero-game registered target is eligible");
}

#[test]
fn test_create_referral_rejects_target_with_one_win() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    fixture.add_player(20, GUILD);
    fixture
        .connection()
        .execute(
            "UPDATE players SET wins = 1 WHERE discord_id = ?1 AND guild_id = ?2",
            params![20, GUILD],
        )
        .expect("seed recorded game");
    assert!(matches!(
        fixture.repository.create_referral(10, 20, Some(GUILD)),
        Err(ReferralRepositoryError::ReferredAlreadyPlayed)
    ));
}

#[test]
fn test_create_referral_rejects_target_with_one_loss() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    fixture.add_player(20, GUILD);
    fixture
        .connection()
        .execute(
            "UPDATE players SET losses = 1 WHERE discord_id = ?1 AND guild_id = ?2",
            params![20, GUILD],
        )
        .expect("seed recorded game");
    assert!(matches!(
        fixture.repository.create_referral(10, 20, Some(GUILD)),
        Err(ReferralRepositoryError::ReferredAlreadyPlayed)
    ));
}

#[test]
fn test_create_referral_rejects_self_referral() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    assert!(matches!(
        fixture.repository.create_referral(10, 10, Some(GUILD)),
        Err(ReferralRepositoryError::SelfReferral)
    ));
}

#[test]
fn test_create_referral_isolated_by_guild() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    fixture.add_player(10, OTHER_GUILD);
    fixture
        .repository
        .create_referral(10, 20, Some(GUILD))
        .expect("first guild");
    fixture
        .repository
        .create_referral(10, 20, Some(OTHER_GUILD))
        .expect("second guild");
    assert_eq!(
        fixture
            .repository
            .referral(20, Some(GUILD))
            .expect("first referral")
            .expect("first row")
            .guild_id,
        GUILD
    );
    assert_eq!(
        fixture
            .repository
            .referral(20, Some(OTHER_GUILD))
            .expect("second referral")
            .expect("second row")
            .guild_id,
        OTHER_GUILD
    );
}

#[test]
fn test_create_referral_allows_exactly_one_concurrent_enrollment() {
    let fixture = Fixture::new();
    fixture.add_player(10, GUILD);
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = fixture.repository.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.create_referral(10, 20, Some(GUILD))
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("enrollment thread"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(ReferralRepositoryError::AlreadyReferred)))
            .count(),
        1
    );
}

#[test]
fn test_first_recorded_match_atomically_rewards_both_referral_accounts() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 10, 20, GUILD);
    fixture.add_player(30, GUILD);
    let outcome = fixture
        .repository
        .record_match_with_referrals(request(&[20], &[30], GUILD, 1_001))
        .expect("record first match");
    assert!(outcome.inserted);
    assert_eq!(
        outcome.rewards,
        [ReferralReward {
            referred_id: 20,
            referrer_id: 10,
            reward_amount: REFERRAL_REWARD,
            referred_net: REFERRAL_REWARD,
            referrer_net: REFERRAL_REWARD,
        }]
    );
    assert_eq!(
        fixture.repository.balance(10, Some(GUILD)).unwrap(),
        Some(103)
    );
    assert_eq!(
        fixture.repository.balance(20, Some(GUILD)).unwrap(),
        Some(103)
    );
    let ledger = fixture.repository.ledger_entries().expect("ledger entries");
    assert_eq!(
        ledger
            .iter()
            .map(|entry| (entry.account_id, entry.delta))
            .collect::<Vec<_>>(),
        [(20, 100), (10, 100)]
    );
    assert!(ledger.iter().all(|entry| {
        entry.source == "referral_reward"
            && entry.related_type.as_deref() == Some("referral")
            && entry.related_id.as_deref() == Some("20")
    }));
    assert!(
        ledger[0]
            .metadata
            .as_deref()
            .is_some_and(|metadata| metadata.contains("\"beneficiary_role\":\"referred\""))
    );
    assert!(
        ledger[1]
            .metadata
            .as_deref()
            .is_some_and(|metadata| metadata.contains("\"beneficiary_role\":\"referrer\""))
    );
    let referral = fixture
        .repository
        .referral(20, Some(GUILD))
        .unwrap()
        .expect("referral row");
    assert_eq!(
        (
            referral.rewarded_match_id,
            referral.reward_amount,
            referral.rewarded_at
        ),
        (Some(outcome.match_id), Some(100), Some(NOW))
    );
    assert_eq!(fixture.context_count(), 0);
}

#[test]
fn test_referral_rewards_withhold_scaled_bankruptcy_penalty_and_taxes() {
    let fixture = Fixture::new();
    // Referrer 10 has one penalty game left, referrer 11 six, referred 21 is
    // vanity taxable, and referred 20 is unlisted.
    enroll_then_register(&fixture, 10, 20, GUILD);
    enroll_then_register(&fixture, 11, 21, GUILD);
    fixture.add_player(30, GUILD);
    fixture.set_penalty_games(10, GUILD, 1);
    fixture.set_penalty_games(11, GUILD, 6);
    let vanity = BTreeSet::from([21]);
    let empty = BTreeSet::new();
    let mut taxed = request(&[20, 21], &[30], GUILD, 1_101);
    taxed.deductions = deduction_policy(&vanity, &empty);
    let outcome = fixture
        .repository
        .record_match_with_referrals(taxed)
        .expect("record taxed referral match");
    // 100 JC basis at 5% per outstanding game: one game withholds 5, six
    // games 30, and the 10% vanity share is 10.
    assert_eq!(
        outcome.rewards,
        [
            ReferralReward {
                referred_id: 20,
                referrer_id: 10,
                reward_amount: 100,
                referred_net: 100,
                referrer_net: 95,
            },
            ReferralReward {
                referred_id: 21,
                referrer_id: 11,
                reward_amount: 100,
                referred_net: 90,
                referrer_net: 70,
            },
        ]
    );
    assert_eq!(
        outcome.referral_jc_changes,
        BTreeMap::from([(10, 95), (11, 70), (20, 100), (21, 90)])
    );
    for (discord_id, expected) in [(10, 98), (11, 73), (20, 103), (21, 93)] {
        assert_eq!(
            fixture.repository.balance(discord_id, Some(GUILD)).unwrap(),
            Some(expected),
            "player {discord_id}"
        );
    }
    assert_eq!(
        fixture.all_ledger_entries(),
        vec![
            (20, "referral_reward".to_owned(), 100),
            (10, "referral_reward".to_owned(), 100),
            (10, "referral_reward".to_owned(), -5),
            (21, "referral_reward".to_owned(), 100),
            (21, "vanity_tax".to_owned(), -10),
            (11, "referral_reward".to_owned(), 100),
            (11, "referral_reward".to_owned(), -30),
        ]
    );
    assert_eq!(
        fixture
            .repository
            .match_jc_changes_json(outcome.match_id, Some(GUILD))
            .unwrap()
            .as_deref(),
        Some(
            "{\"10\":{\"referral\":95},\"11\":{\"referral\":70},\
             \"20\":{\"referral\":100},\"21\":{\"referral\":90}}"
        )
    );
    assert_eq!(fixture.context_count(), 0);

    // A retry reconstructs the same net projection from the ledger.
    let retry = fixture
        .repository
        .record_match_with_referrals(request(&[20, 21], &[30], GUILD, 1_101))
        .expect("retry taxed referral match");
    assert!(!retry.inserted);
    assert_eq!(retry.rewards, outcome.rewards);
    assert_eq!(retry.referral_jc_changes, outcome.referral_jc_changes);
    assert_eq!(fixture.all_ledger_entries().len(), 7);
}

#[test]
fn test_referral_reward_withholds_the_low_priority_share() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 12, 22, GUILD);
    fixture.add_player(31, GUILD);
    let low_priority = BTreeSet::from([12]);
    let empty = BTreeSet::new();
    let mut taxed = request(&[22], &[31], GUILD, 1_102);
    taxed.deductions = deduction_policy(&empty, &low_priority);
    let outcome = fixture
        .repository
        .record_match_with_referrals(taxed)
        .expect("record low-priority referral match");
    assert_eq!(outcome.rewards[0].referrer_net, 75);
    assert_eq!(outcome.rewards[0].referred_net, 100);
    assert_eq!(
        fixture.repository.balance(12, Some(GUILD)).unwrap(),
        Some(78)
    );
    assert_eq!(
        fixture.all_ledger_entries(),
        vec![
            (22, "referral_reward".to_owned(), 100),
            (12, "referral_reward".to_owned(), 100),
            (12, "low_priority_tax".to_owned(), -25),
        ]
    );
}

#[test]
fn test_later_match_does_not_pay_referral_again() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 40, 41, GUILD);
    fixture.add_player(42, GUILD);
    let first = fixture
        .repository
        .record_match_with_referrals(request(&[41], &[42], GUILD, 1_002))
        .expect("first match");
    let second = fixture
        .repository
        .record_match_with_referrals(request(&[41], &[42], GUILD, 1_003))
        .expect("second match");
    assert_eq!(first.rewards.len(), 1);
    assert!(second.rewards.is_empty());
    assert_eq!(fixture.repository.ledger_entries().unwrap().len(), 2);
    assert_eq!(
        fixture.repository.balance(40, Some(GUILD)).unwrap(),
        Some(103)
    );
    assert_eq!(
        fixture.repository.balance(41, Some(GUILD)).unwrap(),
        Some(103)
    );
}

#[test]
fn test_idempotent_match_retry_does_not_pay_referral_again() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 50, 51, GUILD);
    fixture.add_player(52, GUILD);
    let first = fixture
        .repository
        .record_match_with_referrals(request(&[51], &[52], GUILD, 1_004))
        .expect("first attempt");
    let retry = fixture
        .repository
        .record_match_with_referrals(request(&[51], &[52], GUILD, 1_004))
        .expect("retry");
    assert_eq!(retry.match_id, first.match_id);
    assert!(!retry.inserted);
    assert_eq!(retry.rewards, first.rewards);
    assert_eq!(retry.referral_jc_changes, first.referral_jc_changes);
    assert_eq!(fixture.repository.match_count().unwrap(), 1);
    assert_eq!(fixture.repository.ledger_entries().unwrap().len(), 2);
    assert_eq!(
        fixture.repository.balance(50, Some(GUILD)).unwrap(),
        Some(103)
    );
    assert_eq!(
        fixture.repository.balance(51, Some(GUILD)).unwrap(),
        Some(103)
    );
}

#[test]
fn test_result_correction_does_not_add_or_reverse_referral_rewards() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 60, 61, GUILD);
    fixture.add_player(62, GUILD);
    let recorded = fixture
        .repository
        .record_match_with_referrals(request(&[61], &[62], GUILD, 1_005))
        .expect("record match");
    let balances = (
        fixture.repository.balance(60, Some(GUILD)).unwrap(),
        fixture.repository.balance(61, Some(GUILD)).unwrap(),
    );
    let jc_changes = fixture
        .repository
        .match_jc_changes_json(recorded.match_id, Some(GUILD))
        .expect("stored JC changes");
    fixture
        .repository
        .correct_match_result(recorded.match_id, Some(GUILD), MatchSide::Dire)
        .expect("correct result");
    assert_eq!(
        (
            fixture.repository.balance(60, Some(GUILD)).unwrap(),
            fixture.repository.balance(61, Some(GUILD)).unwrap(),
        ),
        balances
    );
    assert_eq!(fixture.repository.ledger_entries().unwrap().len(), 2);
    assert_eq!(
        fixture
            .repository
            .referral_rewards_for_match(recorded.match_id, Some(GUILD))
            .unwrap(),
        recorded.rewards
    );
    assert_eq!(
        fixture
            .repository
            .match_jc_changes_json(recorded.match_id, Some(GUILD))
            .unwrap(),
        jc_changes
    );
}

#[test]
fn test_referral_recording_is_one_atomic_repository_transaction() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 100, 101, GUILD);
    fixture.add_player(102, GUILD);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER abort_referral_claim
             BEFORE UPDATE OF rewarded_match_id ON referrals
             WHEN NEW.referred_id = 101
             BEGIN SELECT RAISE(ABORT, 'fixture claim failure'); END;",
        )
        .expect("install failure trigger");
    assert!(matches!(
        fixture
            .repository
            .record_match_with_referrals(request(&[101], &[102], GUILD, 1_006)),
        Err(ReferralRepositoryError::Sqlite(_))
    ));
    assert_eq!(fixture.repository.match_count().unwrap(), 0);
    assert_eq!(
        fixture.repository.balance(100, Some(GUILD)).unwrap(),
        Some(3)
    );
    assert_eq!(
        fixture.repository.balance(101, Some(GUILD)).unwrap(),
        Some(3)
    );
    assert!(fixture.repository.ledger_entries().unwrap().is_empty());
    assert_eq!(fixture.context_count(), 0);
    let referral = fixture
        .repository
        .referral(101, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(referral.rewarded_match_id, None);
}

#[test]
fn test_missing_referrer_skips_partial_reward_but_commits_match() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 70, 71, GUILD);
    fixture.remove_player(70, GUILD);
    fixture.add_player(72, GUILD);
    let outcome = fixture
        .repository
        .record_match_with_referrals(request(&[71], &[72], GUILD, 1_007))
        .expect("record without referrer");
    assert!(outcome.inserted);
    assert!(outcome.rewards.is_empty());
    assert_eq!(fixture.repository.match_count().unwrap(), 1);
    assert_eq!(
        fixture.repository.balance(71, Some(GUILD)).unwrap(),
        Some(3)
    );
    assert!(fixture.repository.ledger_entries().unwrap().is_empty());
    assert_eq!(
        fixture
            .repository
            .referral(71, Some(GUILD))
            .unwrap()
            .unwrap()
            .rewarded_match_id,
        None
    );
}

#[test]
fn test_missing_referred_account_skips_referrer_reward_but_commits_match() {
    let fixture = Fixture::new();
    fixture.add_player(73, GUILD);
    fixture
        .repository
        .create_referral(73, 74, Some(GUILD))
        .expect("enroll missing account");
    fixture.add_player(75, GUILD);
    let outcome = fixture
        .repository
        .record_match_with_referrals(request(&[74], &[75], GUILD, 1_008))
        .expect("record without referred account");
    assert!(outcome.inserted);
    assert!(outcome.rewards.is_empty());
    assert_eq!(fixture.repository.match_count().unwrap(), 1);
    assert_eq!(
        fixture.repository.balance(73, Some(GUILD)).unwrap(),
        Some(3)
    );
    assert!(fixture.repository.ledger_entries().unwrap().is_empty());
    assert_eq!(
        fixture
            .repository
            .referral(74, Some(GUILD))
            .unwrap()
            .unwrap()
            .rewarded_match_id,
        None
    );
}

#[test]
fn test_same_match_settles_multiple_referrals_independently() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 80, 82, GUILD);
    enroll_then_register(&fixture, 81, 83, GUILD);
    fixture.add_player(84, GUILD);
    let outcome = fixture
        .repository
        .record_match_with_referrals(request(&[82, 83], &[84], GUILD, 1_009))
        .expect("record multi-referral match");
    assert_eq!(
        outcome.rewards,
        [
            ReferralReward {
                referred_id: 82,
                referrer_id: 80,
                reward_amount: 100,
                referred_net: 100,
                referrer_net: 100,
            },
            ReferralReward {
                referred_id: 83,
                referrer_id: 81,
                reward_amount: 100,
                referred_net: 100,
                referrer_net: 100,
            },
        ]
    );
    for discord_id in [80, 81, 82, 83] {
        assert_eq!(
            fixture.repository.balance(discord_id, Some(GUILD)).unwrap(),
            Some(103)
        );
    }
    assert_eq!(fixture.repository.ledger_entries().unwrap().len(), 4);
}

#[test]
fn test_referral_settlement_is_guild_scoped() {
    let fixture = Fixture::new();
    fixture.add_player(90, GUILD);
    fixture.add_player(90, OTHER_GUILD);
    fixture
        .repository
        .create_referral(90, 91, Some(GUILD))
        .expect("first-guild referral");
    fixture.add_player(91, OTHER_GUILD);
    fixture.add_player(92, OTHER_GUILD);
    let outcome = fixture
        .repository
        .record_match_with_referrals(request(&[91], &[92], OTHER_GUILD, 1_010))
        .expect("other-guild match");
    assert!(outcome.rewards.is_empty());
    assert_eq!(
        fixture.repository.balance(90, Some(GUILD)).unwrap(),
        Some(3)
    );
    assert_eq!(
        fixture.repository.balance(90, Some(OTHER_GUILD)).unwrap(),
        Some(3)
    );
    assert_eq!(
        fixture.repository.balance(91, Some(OTHER_GUILD)).unwrap(),
        Some(3)
    );
    assert!(fixture.repository.ledger_entries().unwrap().is_empty());
    assert!(
        fixture
            .repository
            .referral_rewards_for_match(outcome.match_id, Some(GUILD))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn signed_ids_and_none_guild_normalize_without_cross_guild_reads() {
    let fixture = Fixture::new();
    fixture.add_player(-10, 0);
    fixture.add_player(-10, OTHER_GUILD);
    fixture
        .repository
        .create_referral(-10, -20, None)
        .expect("signed global referral");
    assert!(fixture.repository.referral(-20, None).unwrap().is_some());
    assert!(
        fixture
            .repository
            .referral(-20, Some(OTHER_GUILD))
            .unwrap()
            .is_none()
    );
}

#[test]
fn concurrent_pending_match_retries_credit_each_beneficiary_once() {
    let fixture = Fixture::new();
    enroll_then_register(&fixture, 200, 201, GUILD);
    fixture.add_player(202, GUILD);
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = fixture.repository.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.record_match_with_referrals(request(&[201], &[202], GUILD, 2_001))
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("record thread")
                .expect("record outcome")
        })
        .collect::<Vec<_>>();
    assert_eq!(outcomes[0].match_id, outcomes[1].match_id);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.inserted).count(),
        1
    );
    assert_eq!(fixture.repository.match_count().unwrap(), 1);
    assert_eq!(fixture.repository.ledger_entries().unwrap().len(), 2);
    assert_eq!(
        fixture.repository.balance(200, Some(GUILD)).unwrap(),
        Some(103)
    );
    assert_eq!(
        fixture.repository.balance(201, Some(GUILD)).unwrap(),
        Some(103)
    );
}
