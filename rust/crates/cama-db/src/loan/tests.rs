use std::sync::{Arc, Barrier};
use std::thread;
use std::{path::Path, process::Command};

use rusqlite::{Connection, OptionalExtension, params};
use tempfile::NamedTempFile;

use super::*;

const TEST_GUILD_ID: i64 = 12_345;
const OTHER_GUILD_ID: i64 = 12_346;
const NOW: i64 = 1_700_000_000;

struct Fixture {
    _database: NamedTempFile,
    repository: LoanRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite file");
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
                 CREATE TABLE loan_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     last_loan_at INTEGER,
                     total_loans_taken INTEGER DEFAULT 0,
                     total_fees_paid INTEGER DEFAULT 0,
                     negative_loans_taken INTEGER DEFAULT 0,
                     outstanding_principal INTEGER DEFAULT 0,
                     outstanding_fee INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE nonprofit_fund (
                     guild_id INTEGER PRIMARY KEY DEFAULT 0,
                     total_collected INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE disburse_proposals (
                     guild_id INTEGER PRIMARY KEY,
                     proposal_id INTEGER NOT NULL,
                     fund_amount INTEGER NOT NULL,
                     quorum_required INTEGER NOT NULL,
                     status TEXT DEFAULT 'active',
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE disburse_votes (
                     guild_id INTEGER NOT NULL,
                     proposal_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     vote_method TEXT NOT NULL,
                     voted_at INTEGER NOT NULL,
                     PRIMARY KEY (guild_id, proposal_id, discord_id)
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
                 END;
                 CREATE TRIGGER ledger_nonprofit_insert
                 AFTER INSERT ON nonprofit_fund
                 WHEN COALESCE(NEW.total_collected, 0) != 0
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                         COALESCE(NEW.total_collected, 0), 0, COALESCE(NEW.total_collected, 0),
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_insert'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;
                 CREATE TRIGGER ledger_nonprofit_update
                 AFTER UPDATE OF total_collected ON nonprofit_fund
                 WHEN COALESCE(OLD.total_collected, 0) != COALESCE(NEW.total_collected, 0)
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                         COALESCE(NEW.total_collected, 0) - COALESCE(OLD.total_collected, 0),
                         COALESCE(OLD.total_collected, 0), COALESCE(NEW.total_collected, 0),
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_update'),
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
        let repository = LoanRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        self.repository.connection().expect("runtime connection")
    }

    fn add_player(&self, discord_id: i64, balance: i64) {
        self.add_player_in_guild(discord_id, TEST_GUILD_ID, balance);
    }

    fn add_player_in_guild(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, guild_id, format!("Player{discord_id}"), balance],
            )
            .expect("insert player");
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.balance_in_guild(discord_id, TEST_GUILD_ID)
    }

    fn balance_in_guild(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get(0),
            )
            .expect("player balance")
    }

    fn set_balance(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "UPDATE players SET jopacoin_balance = ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![balance, discord_id, TEST_GUILD_ID],
            )
            .expect("update balance");
    }

    fn fund(&self) -> i64 {
        self.repository
            .get_nonprofit_fund(Some(TEST_GUILD_ID))
            .expect("nonprofit total")
    }

    fn ledger_count(&self) -> i64 {
        self.connection()
            .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                row.get(0)
            })
            .expect("ledger count")
    }

    fn context_count(&self) -> i64 {
        self.connection()
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get(0)
            })
            .expect("context count")
    }
}

fn issue(
    fixture: &Fixture,
    discord_id: i64,
    amount: i64,
) -> Result<LoanExecution, LoanRepositoryError> {
    fixture.repository.execute_loan_atomic_at(
        discord_id,
        Some(TEST_GUILD_ID),
        amount,
        amount / 5,
        259_200,
        100,
        NOW,
    )
}

#[test]
fn test_cooldown_checked_after_repayment() {
    let fixture = Fixture::new();
    fixture.add_player(2_001, 200);
    issue(&fixture, 2_001, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(2_001, Some(TEST_GUILD_ID))
        .unwrap();
    assert!(matches!(
        fixture.repository.execute_loan_atomic_at(
            2_001,
            Some(TEST_GUILD_ID),
            50,
            10,
            259_200,
            100,
            NOW + 1
        ),
        Err(LoanRepositoryError::CooldownActive { .. })
    ));
}

#[test]
fn test_cooldown_expires() {
    let fixture = Fixture::new();
    fixture.add_player(2_002, 200);
    fixture
        .repository
        .execute_loan_atomic_at(2_002, Some(TEST_GUILD_ID), 20, 4, 1, 100, NOW)
        .unwrap();
    fixture
        .repository
        .execute_repayment_atomic(2_002, Some(TEST_GUILD_ID))
        .unwrap();
    assert!(
        fixture
            .repository
            .execute_loan_atomic_at(2_002, Some(TEST_GUILD_ID), 20, 4, 1, 100, NOW + 2)
            .is_ok()
    );
}

#[test]
fn test_take_loan_credits_full_amount() {
    let fixture = Fixture::new();
    fixture.add_player(3_001, 50);
    let result = issue(&fixture, 3_001, 100).unwrap();
    assert_eq!(
        (result.amount, result.fee, result.total_owed),
        (100, 20, 120)
    );
    assert_eq!(fixture.balance(3_001), 150);
}

#[test]
fn test_take_loan_tracks_outstanding() {
    let fixture = Fixture::new();
    fixture.add_player(3_002, 50);
    issue(&fixture, 3_002, 100).unwrap();
    let state = fixture
        .repository
        .get_state(3_002, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!(
        (state.outstanding_principal, state.outstanding_fee),
        (100, 20)
    );
    assert_eq!(state.outstanding_total(), Some(120));
    assert!(state.has_outstanding_loan());
}

#[test]
fn test_take_loan_updates_state() {
    let fixture = Fixture::new();
    fixture.add_player(3_003, 100);
    let result = issue(&fixture, 3_003, 50).unwrap();
    assert_eq!(result.total_loans_taken, 1);
    let state = fixture
        .repository
        .get_state(3_003, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!(state.total_loans_taken, 1);
    assert_eq!(state.total_fees_paid, 0);
    assert_eq!(state.last_loan_at, Some(NOW));
}

#[test]
fn test_repay_loan_deducts_full_amount() {
    let fixture = Fixture::new();
    fixture.add_player(4_001, 50);
    issue(&fixture, 4_001, 100).unwrap();
    let result = fixture
        .repository
        .execute_repayment_atomic(4_001, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(
        (result.principal, result.fee, result.total_owed),
        (100, 20, 120)
    );
    assert_eq!((result.balance_before, result.new_balance), (150, 30));
    assert_eq!(fixture.balance(4_001), 30);
}

#[test]
fn test_repay_loan_clears_outstanding() {
    let fixture = Fixture::new();
    fixture.add_player(4_002, 200);
    issue(&fixture, 4_002, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(4_002, Some(TEST_GUILD_ID))
        .unwrap();
    let state = fixture
        .repository
        .get_state(4_002, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!((state.outstanding_principal, state.outstanding_fee), (0, 0));
    assert!(!state.has_outstanding_loan());
}

#[test]
fn test_repay_loan_adds_fee_to_nonprofit() {
    let fixture = Fixture::new();
    fixture.add_player(4_003, 200);
    assert_eq!(fixture.fund(), 0);
    issue(&fixture, 4_003, 100).unwrap();
    assert_eq!(fixture.fund(), 0);
    fixture
        .repository
        .execute_repayment_atomic(4_003, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(fixture.fund(), 20);
}

#[test]
fn test_repay_loan_updates_fees_paid() {
    let fixture = Fixture::new();
    fixture.add_player(4_004, 200);
    issue(&fixture, 4_004, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(4_004, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .get_state(4_004, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap()
            .total_fees_paid,
        10
    );
}

#[test]
fn test_repay_loan_can_push_into_debt() {
    let fixture = Fixture::new();
    fixture.add_player(4_005, 0);
    issue(&fixture, 4_005, 100).unwrap();
    fixture.set_balance(4_005, 0);
    let result = fixture
        .repository
        .execute_repayment_atomic(4_005, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(result.new_balance, -120);
    assert_eq!(fixture.balance(4_005), -120);
}

#[test]
fn test_repay_no_outstanding_loan() {
    let fixture = Fixture::new();
    fixture.add_player(4_006, 100);
    assert!(matches!(
        fixture
            .repository
            .execute_repayment_atomic(4_006, Some(TEST_GUILD_ID)),
        Err(LoanRepositoryError::NoOutstandingLoan)
    ));
}

#[test]
fn test_full_repayment_flow() {
    let fixture = Fixture::new();
    fixture.add_player(14_001, 0);
    fixture
        .repository
        .execute_loan_atomic_at(14_001, Some(TEST_GUILD_ID), 50, 10, 60, 1_000, NOW)
        .expect("issue canonical loan");
    let fund_before = fixture.fund();

    let result = fixture
        .repository
        .execute_repayment_atomic(14_001, Some(TEST_GUILD_ID))
        .expect("repay canonical loan");

    assert_eq!(
        (
            result.principal,
            result.fee,
            result.total_owed,
            result.balance_before,
            result.new_balance,
        ),
        (50, 10, 60, 50, -10)
    );
    assert_eq!(fixture.balance(14_001), -10);
    let state = fixture
        .repository
        .get_state(14_001, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!((state.outstanding_principal, state.outstanding_fee), (0, 0));
    assert_eq!(fixture.fund() - fund_before, 10);
}

#[test]
fn test_raises_when_no_outstanding_loan() {
    let fixture = Fixture::new();
    fixture.add_player(14_002, 50);
    let error = fixture
        .repository
        .execute_repayment_atomic(14_002, Some(TEST_GUILD_ID))
        .expect_err("missing outstanding loan must fail");
    assert!(matches!(error, LoanRepositoryError::NoOutstandingLoan));
    assert_eq!(fixture.balance(14_002), 50);
}

#[test]
fn pay_debt_transfers_between_players_atomically() {
    let fixture = Fixture::new();
    fixture.add_player(6_001, 50);
    fixture.add_player(6_002, -30);
    let payment = fixture
        .repository
        .pay_debt_atomic(6_001, 6_002, Some(TEST_GUILD_ID), 20)
        .expect("debt payment");
    assert_eq!(
        payment,
        DebtPayment {
            amount_paid: 20,
            from_new_balance: 30,
            to_new_balance: -10,
        }
    );
    assert_eq!(fixture.balance(6_001), 30);
    assert_eq!(fixture.balance(6_002), -10);
}

#[test]
fn pay_debt_caps_payment_at_recipient_debt() {
    let fixture = Fixture::new();
    fixture.add_player(6_003, 100);
    fixture.add_player(6_004, -10);
    let payment = fixture
        .repository
        .pay_debt_atomic(6_003, 6_004, Some(TEST_GUILD_ID), 50)
        .expect("capped debt payment");
    assert_eq!(payment.amount_paid, 10);
    assert_eq!(payment.from_new_balance, 90);
    assert_eq!(payment.to_new_balance, 0);
}

#[test]
fn pay_debt_rejects_recipient_without_debt() {
    let fixture = Fixture::new();
    fixture.add_player(6_005, 100);
    fixture.add_player(6_006, 50);
    assert!(matches!(
        fixture
            .repository
            .pay_debt_atomic(6_005, 6_006, Some(TEST_GUILD_ID), 10),
        Err(LoanRepositoryError::RecipientHasNoDebt)
    ));
    assert_eq!(fixture.balance(6_005), 100);
    assert_eq!(fixture.balance(6_006), 50);
}

#[test]
fn pay_debt_rejects_insufficient_sender_balance() {
    let fixture = Fixture::new();
    fixture.add_player(6_007, 5);
    fixture.add_player(6_008, -100);
    assert!(matches!(
        fixture
            .repository
            .pay_debt_atomic(6_007, 6_008, Some(TEST_GUILD_ID), 10),
        Err(LoanRepositoryError::InsufficientDebtPaymentBalance(5))
    ));
    assert_eq!(fixture.balance(6_007), 5);
    assert_eq!(fixture.balance(6_008), -100);
}

#[test]
fn test_reset_returns_reserve_and_no_ops_on_retry() {
    let fixture = Fixture::new();
    fixture
        .repository
        .add_to_nonprofit_fund(Some(TEST_GUILD_ID), 500, None)
        .expect("seed reserve");
    fixture
        .repository
        .deduct_from_nonprofit_fund(Some(TEST_GUILD_ID), 200, None)
        .expect("reserve proposal funds");
    fixture
        .connection()
        .execute(
            "INSERT INTO disburse_proposals (
                 guild_id, proposal_id, fund_amount, quorum_required, status
             ) VALUES (?1, 1, 200, 3, 'active')",
            [TEST_GUILD_ID],
        )
        .expect("seed active proposal");

    assert!(
        fixture
            .repository
            .reset_and_return_fund_atomic(Some(TEST_GUILD_ID), 200)
            .expect("first reset")
    );
    assert_eq!(fixture.fund(), 500);
    let status: String = fixture
        .connection()
        .query_row(
            "SELECT status FROM disburse_proposals WHERE proposal_id = 1",
            [],
            |row| row.get(0),
        )
        .expect("proposal status");
    assert_eq!(status, "reset");
    assert!(
        !fixture
            .repository
            .reset_and_return_fund_atomic(Some(TEST_GUILD_ID), 200)
            .expect("idempotent retry")
    );
    assert_eq!(fixture.fund(), 500);
}

#[test]
fn test_get_state_no_loans() {
    let fixture = Fixture::new();
    fixture.add_player(6_001, 3);
    assert_eq!(
        fixture
            .repository
            .get_state(6_001, Some(TEST_GUILD_ID))
            .unwrap(),
        None
    );
}

#[test]
fn test_get_state_with_outstanding_loan() {
    let fixture = Fixture::new();
    fixture.add_player(6_002, 100);
    issue(&fixture, 6_002, 50).unwrap();
    let state = fixture
        .repository
        .get_state(6_002, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!(state.total_loans_taken, 1);
    assert_eq!(
        (state.outstanding_principal, state.outstanding_fee),
        (50, 10)
    );
    assert_eq!(state.total_fees_paid, 0);
}

#[test]
fn test_get_state_after_repayment() {
    let fixture = Fixture::new();
    fixture.add_player(6_003, 200);
    issue(&fixture, 6_003, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(6_003, Some(TEST_GUILD_ID))
        .unwrap();
    let state = fixture
        .repository
        .get_state(6_003, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!((state.total_loans_taken, state.total_fees_paid), (1, 10));
    assert_eq!((state.outstanding_principal, state.outstanding_fee), (0, 0));
}

#[test]
fn test_loan_while_negative_flagged() {
    let fixture = Fixture::new();
    fixture.add_player(7_001, -100);
    assert!(issue(&fixture, 7_001, 50).unwrap().was_negative_loan);
}

#[test]
fn test_loan_while_positive_not_flagged() {
    let fixture = Fixture::new();
    fixture.add_player(7_002, 100);
    assert!(!issue(&fixture, 7_002, 50).unwrap().was_negative_loan);
}

#[test]
fn test_negative_loans_counted() {
    let fixture = Fixture::new();
    fixture.add_player(7_003, -50);
    fixture
        .repository
        .execute_loan_atomic_at(7_003, Some(TEST_GUILD_ID), 20, 4, 0, 100, NOW)
        .unwrap();
    fixture
        .repository
        .execute_repayment_atomic(7_003, Some(TEST_GUILD_ID))
        .unwrap();
    fixture.set_balance(7_003, -50);
    fixture
        .repository
        .execute_loan_atomic_at(7_003, Some(TEST_GUILD_ID), 20, 4, 0, 100, NOW)
        .unwrap();
    let state = fixture
        .repository
        .get_state(7_003, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!(
        (state.negative_loans_taken, state.total_loans_taken),
        (2, 2)
    );
}

#[test]
fn test_full_loan_cycle() {
    let fixture = Fixture::new();
    fixture.add_player(8_001, 0);
    issue(&fixture, 8_001, 100).unwrap();
    fixture.set_balance(8_001, 150);
    let repayment = fixture
        .repository
        .execute_repayment_atomic(8_001, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(repayment.total_owed, 120);
    assert_eq!(fixture.balance(8_001), 30);
    assert_eq!(fixture.fund(), 20);
}

#[test]
fn test_loan_then_lose_everything() {
    let fixture = Fixture::new();
    fixture.add_player(8_002, 0);
    issue(&fixture, 8_002, 100).unwrap();
    fixture.set_balance(8_002, 0);
    fixture
        .repository
        .execute_repayment_atomic(8_002, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(fixture.balance(8_002), -120);
}

#[test]
fn test_successful_loan() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    let result = issue(&fixture, 12_345, 50).unwrap();
    assert_eq!(
        (result.amount, result.fee, result.new_balance),
        (50, 10, 60)
    );
    assert!(!result.was_negative_loan);
}

#[test]
fn test_loan_updates_balance() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    issue(&fixture, 12_345, 50).unwrap();
    assert_eq!(fixture.balance(12_345), 60);
}

#[test]
fn test_loan_creates_outstanding() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    issue(&fixture, 12_345, 50).unwrap();
    let state = fixture
        .repository
        .get_state(12_345, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert!(state.has_outstanding_loan());
    assert_eq!(
        (state.outstanding_principal, state.outstanding_fee),
        (50, 10)
    );
}

#[test]
fn test_negative_loan_tracked() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, -5);
    let result = issue(&fixture, 12_345, 50).unwrap();
    assert!(result.was_negative_loan);
    assert_eq!(
        fixture
            .repository
            .get_state(12_345, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap()
            .negative_loans_taken,
        1
    );
}

#[test]
fn test_successful_repayment() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    issue(&fixture, 12_345, 50).unwrap();
    let result = fixture
        .repository
        .execute_repayment_atomic(12_345, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(
        (result.principal, result.fee, result.total_owed),
        (50, 10, 60)
    );
}

#[test]
fn test_repayment_clears_outstanding() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    issue(&fixture, 12_345, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(12_345, Some(TEST_GUILD_ID))
        .unwrap();
    assert!(
        !fixture
            .repository
            .get_state(12_345, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap()
            .has_outstanding_loan()
    );
}

#[test]
fn test_repayment_adds_to_nonprofit() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    issue(&fixture, 12_345, 50).unwrap();
    let result = fixture
        .repository
        .execute_repayment_atomic(12_345, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(result.nonprofit_total, 10);
    assert_eq!(fixture.fund(), 10);
}

#[test]
fn test_no_outstanding_loan_fails() {
    let fixture = Fixture::new();
    fixture.add_player(12_345, 10);
    assert!(matches!(
        fixture
            .repository
            .execute_repayment_atomic(12_345, Some(TEST_GUILD_ID)),
        Err(LoanRepositoryError::NoOutstandingLoan)
    ));
}

#[test]
fn test_reset_preserves_outstanding_loan_state() {
    let fixture = Fixture::new();
    fixture.add_player(88_001, 0);
    issue(&fixture, 88_001, 100).unwrap();
    fixture
        .repository
        .reset_cooldown(88_001, Some(TEST_GUILD_ID))
        .unwrap();
    let state = fixture
        .repository
        .get_state(88_001, Some(TEST_GUILD_ID))
        .unwrap()
        .unwrap();
    assert_eq!(state.last_loan_at, Some(0));
    assert_eq!(
        (state.outstanding_principal, state.outstanding_fee),
        (100, 20)
    );
    assert_eq!(state.total_loans_taken, 1);
    assert!(matches!(
        issue(&fixture, 88_001, 50),
        Err(LoanRepositoryError::OutstandingLoan { .. })
    ));
}

#[test]
fn test_reset_without_state_row_is_noop() {
    let fixture = Fixture::new();
    fixture
        .repository
        .reset_cooldown(88_002, Some(TEST_GUILD_ID))
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .get_state(88_002, Some(TEST_GUILD_ID))
            .unwrap(),
        None
    );
}

fn transfer_context() -> LedgerContext {
    LedgerContext {
        source: Some("test".to_owned()),
        reason: Some("tithe".to_owned()),
        ..LedgerContext::default()
    }
}

#[test]
fn nonprofit_credit_once_recovers_the_first_receipt_without_minting_again() {
    let fixture = Fixture::new();
    let context = LedgerContext {
        source: Some("dig".to_owned()),
        actor_id: Some(88_000),
        related_type: Some("plains_tithe".to_owned()),
        related_id: Some("dig-interaction:123".to_owned()),
        reason: Some("Dig Plains tithe".to_owned()),
        metadata: None,
    };
    assert_eq!(
        fixture
            .repository
            .add_to_nonprofit_fund_once(Some(TEST_GUILD_ID), 7, &context)
            .expect("first idempotent credit"),
        NonprofitCreditReceipt {
            amount: 7,
            total: 7,
            applied: true,
        }
    );
    assert_eq!(
        fixture
            .repository
            .add_to_nonprofit_fund_once(Some(TEST_GUILD_ID), 11, &context)
            .expect("recover first idempotent credit"),
        NonprofitCreditReceipt {
            amount: 7,
            total: 7,
            applied: false,
        }
    );
    assert_eq!(fixture.fund(), 7);
    assert_eq!(fixture.ledger_count(), 1);
}

#[test]
fn test_missing_player_raises_and_mints_nothing() {
    let fixture = Fixture::new();
    fixture
        .repository
        .add_to_nonprofit_fund(Some(TEST_GUILD_ID), 40, None)
        .unwrap();
    assert!(matches!(
        fixture.repository.transfer_balance_to_nonprofit_atomic(
            999_999_999,
            Some(TEST_GUILD_ID),
            20,
            &transfer_context()
        ),
        Err(LoanRepositoryError::PlayerNotFound)
    ));
    assert_eq!(fixture.fund(), 40);
}

#[test]
fn test_transfer_moves_exact_amount() {
    let fixture = Fixture::new();
    fixture.add_player(88_003, 50);
    let total = fixture
        .repository
        .transfer_balance_to_nonprofit_atomic(88_003, Some(TEST_GUILD_ID), 20, &transfer_context())
        .unwrap();
    assert_eq!(total, 20);
    assert_eq!(fixture.balance(88_003), 30);
    assert_eq!(fixture.fund(), 20);
}

#[test]
fn test_pays_eligible_players_once_in_input_order() {
    let fixture = Fixture::new();
    fixture.add_player(88_101, 0);
    fixture.add_player(88_102, -10);
    fixture.add_player(88_103, 1);
    fixture
        .repository
        .add_to_nonprofit_fund(Some(TEST_GUILD_ID), 7, None)
        .unwrap();
    let paid = fixture
        .repository
        .distribute_nonprofit_stipends_atomic(
            &[88_101, 88_102, 88_101, 88_103],
            Some(TEST_GUILD_ID),
            5,
        )
        .unwrap();
    assert_eq!(
        paid,
        vec![
            StipendPayment {
                discord_id: 88_101,
                amount: 5
            },
            StipendPayment {
                discord_id: 88_102,
                amount: 2
            },
            StipendPayment {
                discord_id: 88_103,
                amount: 0
            }
        ]
    );
    assert_eq!(
        (
            fixture.balance(88_101),
            fixture.balance(88_102),
            fixture.balance(88_103)
        ),
        (5, -8, 1)
    );
    assert_eq!(fixture.fund(), 0);
}

#[test]
fn test_empty_fund_skips_every_recipient() {
    let fixture = Fixture::new();
    fixture.add_player(88_104, 0);
    fixture.add_player(88_105, -10);
    let paid = fixture
        .repository
        .distribute_nonprofit_stipends_atomic(&[88_104, 88_105], Some(TEST_GUILD_ID), 5)
        .unwrap();
    assert_eq!(
        paid.iter().map(|entry| entry.amount).collect::<Vec<_>>(),
        [0, 0]
    );
    assert_eq!((fixture.balance(88_104), fixture.balance(88_105)), (0, -10));
}

#[test]
fn test_outstanding_borrower_ids_are_bulk_filtered_and_guild_scoped() {
    let fixture = Fixture::new();
    fixture
        .repository
        .upsert_state(
            8_101,
            Some(TEST_GUILD_ID),
            LoanStatePatch {
                outstanding_principal: Some(50),
                outstanding_fee: Some(10),
                ..LoanStatePatch::default()
            },
        )
        .unwrap();
    fixture
        .repository
        .upsert_state(
            8_102,
            Some(TEST_GUILD_ID),
            LoanStatePatch {
                outstanding_principal: Some(0),
                outstanding_fee: Some(10),
                ..LoanStatePatch::default()
            },
        )
        .unwrap();
    fixture
        .repository
        .upsert_state(
            8_103,
            Some(OTHER_GUILD_ID),
            LoanStatePatch {
                outstanding_principal: Some(75),
                outstanding_fee: Some(15),
                ..LoanStatePatch::default()
            },
        )
        .unwrap();
    let requested = [8_101, 8_102, 8_103, 8_999, 8_101];
    assert_eq!(
        fixture
            .repository
            .get_outstanding_borrower_ids(&requested, Some(TEST_GUILD_ID))
            .unwrap(),
        HashSet::from([8_101])
    );
    assert_eq!(
        fixture
            .repository
            .get_outstanding_borrower_ids(&requested, Some(OTHER_GUILD_ID))
            .unwrap(),
        HashSet::from([8_103])
    );
}

#[test]
fn test_outstanding_borrower_ids_empty_input_skips_connection() {
    let missing = std::env::temp_dir().join("cama-loan-missing-db.sqlite");
    let repository = LoanRepository::new(missing);
    assert_eq!(
        repository
            .get_outstanding_borrower_ids(&[], Some(TEST_GUILD_ID))
            .unwrap(),
        HashSet::new()
    );
}

// Stronger, intentionally unmapped invariants follow.

#[test]
fn loan_failure_after_wallet_credit_rolls_back_state_balance_ledger_and_context() {
    let fixture = Fixture::new();
    fixture.add_player(90_001, 10);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER force_loan_state_failure
             BEFORE INSERT ON loan_state
             BEGIN
                 SELECT RAISE(ABORT, 'forced loan-state failure');
             END;",
        )
        .unwrap();
    let error = issue(&fixture, 90_001, 50).unwrap_err();
    assert!(error.to_string().contains("forced loan-state failure"));
    assert_eq!(fixture.balance(90_001), 10);
    assert_eq!(fixture.ledger_count(), 0);
    assert_eq!(fixture.context_count(), 0);
}

#[test]
fn repayment_failure_after_debits_rolls_back_every_money_surface() {
    let fixture = Fixture::new();
    fixture.add_player(90_002, 100);
    issue(&fixture, 90_002, 50).unwrap();
    let ledger_before = fixture.ledger_count();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER force_repayment_state_failure
             BEFORE UPDATE OF outstanding_principal ON loan_state
             BEGIN
                 SELECT RAISE(ABORT, 'forced repayment-state failure');
             END;",
        )
        .unwrap();
    let error = fixture
        .repository
        .execute_repayment_atomic(90_002, Some(TEST_GUILD_ID))
        .unwrap_err();
    assert!(error.to_string().contains("forced repayment-state failure"));
    assert_eq!(fixture.balance(90_002), 150);
    assert_eq!(fixture.fund(), 0);
    assert_eq!(fixture.ledger_count(), ledger_before);
    assert_eq!(fixture.context_count(), 0);
    assert_eq!(
        fixture
            .repository
            .get_state(90_002, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap()
            .outstanding_principal,
        50
    );
}

#[test]
fn concurrent_loan_requests_serialize_to_one_outstanding_loan() {
    let fixture = Fixture::new();
    fixture.add_player(90_003, 10);
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.execute_loan_atomic_at(90_003, Some(TEST_GUILD_ID), 50, 10, 0, 100, NOW)
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("loan thread"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(LoanRepositoryError::OutstandingLoan { .. })))
            .count(),
        1
    );
    assert_eq!(fixture.balance(90_003), 60);
    assert_eq!(
        fixture
            .repository
            .get_state(90_003, Some(TEST_GUILD_ID))
            .unwrap()
            .unwrap()
            .total_loans_taken,
        1
    );
}

#[test]
fn signed_ids_and_none_guild_round_trip_without_unsigned_coercion() {
    let fixture = Fixture::new();
    fixture.add_player_in_guild(-9_004, 0, -1);
    let result = fixture
        .repository
        .execute_loan_atomic_at(-9_004, None, 1, 0, 0, 100, NOW)
        .unwrap();
    assert!(result.was_negative_loan);
    let state = fixture.repository.get_state(-9_004, None).unwrap().unwrap();
    assert_eq!((state.discord_id, state.guild_id), (-9_004, 0));
    assert_eq!(fixture.balance_in_guild(-9_004, 0), 0);
}

#[test]
fn loan_and_repayment_emit_contextual_ledger_rows_and_leave_no_context() {
    let fixture = Fixture::new();
    fixture.add_player(90_005, 10);
    issue(&fixture, 90_005, 50).unwrap();
    fixture
        .repository
        .execute_repayment_atomic(90_005, Some(TEST_GUILD_ID))
        .unwrap();
    let connection = fixture.connection();
    let sources = connection
        .prepare("SELECT source FROM economy_ledger_entries ORDER BY ledger_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(sources, ["loan", "loan_repayment", "loan_repayment"]);
    assert_eq!(fixture.context_count(), 0);
}

#[test]
fn stipend_failure_rolls_back_prior_recipients_reserve_and_ledger() {
    let fixture = Fixture::new();
    fixture.add_player(90_006, 0);
    fixture.add_player(90_007, 0);
    fixture
        .repository
        .add_to_nonprofit_fund(Some(TEST_GUILD_ID), 10, None)
        .unwrap();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_second_stipend
             BEFORE UPDATE OF jopacoin_balance ON players
             WHEN OLD.discord_id = 90007
             BEGIN
                 SELECT RAISE(ABORT, 'forced second stipend failure');
             END;",
        )
        .unwrap();
    let ledger_before = fixture.ledger_count();
    let error = fixture
        .repository
        .distribute_nonprofit_stipends_atomic(&[90_006, 90_007], Some(TEST_GUILD_ID), 5)
        .unwrap_err();
    assert!(error.to_string().contains("forced second stipend failure"));
    assert_eq!((fixture.balance(90_006), fixture.balance(90_007)), (0, 0));
    assert_eq!(fixture.fund(), 10);
    assert_eq!(fixture.ledger_count(), ledger_before);
    assert_eq!(fixture.context_count(), 0);
}

#[test]
fn fee_only_state_is_not_bulk_outstanding() {
    let fixture = Fixture::new();
    fixture
        .repository
        .upsert_state(
            -91_001,
            None,
            LoanStatePatch {
                outstanding_principal: Some(0),
                outstanding_fee: Some(99),
                ..LoanStatePatch::default()
            },
        )
        .unwrap();
    assert!(
        fixture
            .repository
            .get_outstanding_borrower_ids(&[-91_001], None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn absent_nonprofit_row_reads_as_zero() {
    let fixture = Fixture::new();
    let raw = fixture
        .connection()
        .query_row(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
            [OTHER_GUILD_ID],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .unwrap();
    assert_eq!(raw, None);
    assert_eq!(
        fixture
            .repository
            .get_nonprofit_fund(Some(OTHER_GUILD_ID))
            .unwrap(),
        0
    );
}

/// Manual side-by-side smoke: Python owns schema creation, Rust only opens the
/// resulting existing file, and Python then observes Rust's committed cycle.
#[test]
#[ignore = "cross-language smoke; run explicitly when uv/Python are available"]
fn python_migrated_database_supports_rust_loan_cycle() {
    let database = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(&repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nc.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)\",(-70001,0,'interop',10))\nc.commit()\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python schema authority");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let repository = LoanRepository::new(database.path());
    repository
        .execute_loan_atomic_at(-70_001, None, 50, 10, 0, 100, NOW)
        .unwrap();
    repository.execute_repayment_atomic(-70_001, None).unwrap();

    let verify = Command::new(python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70001 AND guild_id=0').fetchone()[0] == 0\nassert c.execute('SELECT total_collected FROM nonprofit_fund WHERE guild_id=0').fetchone()[0] == 10\nassert c.execute('SELECT outstanding_principal,outstanding_fee,total_fees_paid FROM loan_state WHERE discord_id=-70001 AND guild_id=0').fetchone() == (0,0,10)\nassert [r[0] for r in c.execute(\"SELECT source FROM economy_ledger_entries WHERE source IN ('loan','loan_repayment') ORDER BY ledger_id\")] == ['loan','loan_repayment','loan_repayment']\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python post-Rust verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
