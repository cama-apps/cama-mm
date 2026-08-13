use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};

use super::*;

const TEST_GUILD_ID: i64 = 12_345;
const NOW: i64 = 1_700_000_000;

#[derive(Clone, Copy, Debug)]
struct FixedClock(i64);

impl Clock for FixedClock {
    fn unix_timestamp(&self) -> Result<i64, String> {
        Ok(self.0)
    }
}

#[derive(Debug)]
struct FakeLoanPort {
    states: RefCell<HashMap<(i64, i64), StoredLoanState>>,
    balances: RefCell<HashMap<(i64, i64), i64>>,
    outstanding: RefCell<HashSet<i64>>,
    bulk_calls: RefCell<Vec<(Vec<i64>, Option<i64>)>>,
    repayment_calls: RefCell<Vec<(i64, Option<i64>)>>,
    repayments: RefCell<VecDeque<Result<RepaymentResult, LoanPortError>>>,
}

impl Default for FakeLoanPort {
    fn default() -> Self {
        Self {
            states: RefCell::new(HashMap::new()),
            balances: RefCell::new(HashMap::new()),
            outstanding: RefCell::new(HashSet::new()),
            bulk_calls: RefCell::new(Vec::new()),
            repayment_calls: RefCell::new(Vec::new()),
            repayments: RefCell::new(VecDeque::new()),
        }
    }
}

impl FakeLoanPort {
    fn guild_key(discord_id: i64, guild_id: Option<i64>) -> (i64, i64) {
        (discord_id, guild_id.unwrap_or(0))
    }

    fn set_balance(&self, discord_id: i64, guild_id: Option<i64>, balance: i64) {
        self.balances
            .borrow_mut()
            .insert(Self::guild_key(discord_id, guild_id), balance);
    }

    fn set_state(&self, state: StoredLoanState, guild_id: Option<i64>) {
        self.states
            .borrow_mut()
            .insert(Self::guild_key(state.discord_id, guild_id), state);
    }
}

impl LoanPort for FakeLoanPort {
    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredLoanState>, LoanPortError> {
        Ok(self
            .states
            .borrow()
            .get(&Self::guild_key(discord_id, guild_id))
            .copied())
    }

    fn get_balance(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, LoanPortError> {
        Ok(self
            .balances
            .borrow()
            .get(&Self::guild_key(discord_id, guild_id))
            .copied()
            .unwrap_or(0))
    }

    fn execute_loan_atomic(&self, request: AtomicLoanRequest) -> Result<LoanResult, LoanPortError> {
        if request.amount <= 0 {
            return Err(LoanPortError::InvalidLoanAmount);
        }
        if request.amount > request.max_amount {
            return Err(LoanPortError::LoanAmountExceeded {
                max_amount: request.max_amount,
            });
        }
        let balance = self.get_balance(request.discord_id, request.guild_id)?;
        let was_negative_loan = balance < 0;
        Ok(LoanResult {
            amount: request.amount,
            fee: request.fee,
            total_owed: request.amount + request.fee,
            new_balance: balance + request.amount,
            total_loans_taken: 1,
            was_negative_loan,
        })
    }

    fn execute_repayment_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<RepaymentResult, LoanPortError> {
        self.repayment_calls
            .borrow_mut()
            .push((discord_id, guild_id));
        self.repayments
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(LoanPortError::NoOutstandingLoan))
    }

    fn get_outstanding_borrower_ids(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashSet<i64>, LoanPortError> {
        self.bulk_calls
            .borrow_mut()
            .push((discord_ids.to_vec(), guild_id));
        Ok(self.outstanding.borrow().clone())
    }

    fn get_nonprofit_fund(&self, _guild_id: Option<i64>) -> Result<i64, LoanPortError> {
        Ok(0)
    }

    fn distribute_nonprofit_stipends_atomic(
        &self,
        discord_ids: &[i64],
        _guild_id: Option<i64>,
        _max_stipend: i64,
    ) -> Result<Vec<StipendPayment>, LoanPortError> {
        Ok(discord_ids
            .iter()
            .copied()
            .map(|discord_id| StipendPayment {
                discord_id,
                amount: 0,
            })
            .collect())
    }

    fn transfer_balance_to_nonprofit_atomic(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _amount: i64,
        _context: &TransferContext,
    ) -> Result<i64, LoanPortError> {
        Ok(0)
    }

    fn reset_cooldown(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<(), LoanPortError> {
        Ok(())
    }
}

fn policy() -> LoanPolicy {
    LoanPolicy {
        cooldown_seconds: 259_200,
        max_amount: 100,
        fee_rate: 0.20,
        max_debt: 500,
    }
}

fn service(port: FakeLoanPort) -> LoanService<FakeLoanPort, FixedClock> {
    LoanService::with_clock(port, FixedClock(NOW), policy())
}

fn registered_service() -> LoanService<FakeLoanPort, FixedClock> {
    let port = FakeLoanPort::default();
    port.set_balance(12_345, Some(TEST_GUILD_ID), 10);
    service(port)
}

#[test]
fn test_can_take_loan_with_positive_balance() {
    let port = FakeLoanPort::default();
    port.set_balance(1_001, Some(TEST_GUILD_ID), 50);
    let approval = service(port)
        .validate_loan(1_001, 50, Some(TEST_GUILD_ID), None, None)
        .unwrap();
    assert_eq!(
        approval,
        LoanApproval {
            amount: 50,
            fee: 10,
            total_owed: 60,
            new_balance: 100,
        }
    );
}

#[test]
fn test_cannot_take_loan_exceeding_max() {
    let port = FakeLoanPort::default();
    port.set_balance(1_002, Some(TEST_GUILD_ID), 100);
    let error = service(port)
        .validate_loan(1_002, 150, Some(TEST_GUILD_ID), None, None)
        .unwrap_err();
    assert_eq!(error.code, LOAN_AMOUNT_EXCEEDED);
}

#[test]
fn test_cannot_take_loan_with_outstanding_loan() {
    let port = FakeLoanPort::default();
    port.set_balance(1_003, Some(TEST_GUILD_ID), 100);
    port.set_state(
        StoredLoanState {
            discord_id: 1_003,
            outstanding_principal: 50,
            outstanding_fee: 10,
            ..StoredLoanState::default()
        },
        Some(TEST_GUILD_ID),
    );
    let error = service(port)
        .validate_loan(1_003, 50, Some(TEST_GUILD_ID), None, None)
        .unwrap_err();
    assert_eq!(error.code, LOAN_ALREADY_EXISTS);
}

#[test]
fn test_cannot_take_invalid_amount() {
    let port = FakeLoanPort::default();
    port.set_balance(1_004, Some(TEST_GUILD_ID), 50);
    let service = service(port);
    for amount in [0, -10] {
        assert_eq!(
            service
                .validate_loan(1_004, amount, Some(TEST_GUILD_ID), None, None)
                .unwrap_err()
                .code,
            VALIDATION_ERROR
        );
    }
}

#[test]
fn test_valid_loan_returns_approval() {
    let approval = registered_service()
        .validate_loan(12_345, 50, Some(TEST_GUILD_ID), None, None)
        .unwrap();
    assert_eq!(
        approval,
        LoanApproval {
            amount: 50,
            fee: 10,
            total_owed: 60,
            new_balance: 60,
        }
    );
}

#[test]
fn test_outstanding_loan_fails() {
    let service = registered_service();
    service.port().set_state(
        StoredLoanState {
            discord_id: 12_345,
            total_loans_taken: 1,
            outstanding_principal: 50,
            outstanding_fee: 10,
            ..StoredLoanState::default()
        },
        None,
    );
    let error = service
        .validate_loan(12_345, 25, None, None, None)
        .unwrap_err();
    assert_eq!(error.code, LOAN_ALREADY_EXISTS);
    assert!(error.message.to_lowercase().contains("outstanding loan"));
}

#[test]
fn test_cooldown_fails() {
    let service = registered_service();
    service.port().set_state(
        StoredLoanState {
            discord_id: 12_345,
            last_loan_at: Some(NOW - 60),
            total_loans_taken: 1,
            ..StoredLoanState::default()
        },
        None,
    );
    let error = service
        .validate_loan(12_345, 25, None, None, None)
        .unwrap_err();
    assert_eq!(error.code, COOLDOWN_ACTIVE);
    assert!(error.message.to_lowercase().contains("cooldown"));
}

#[test]
fn test_invalid_amount_fails() {
    let error = registered_service()
        .validate_loan(12_345, 0, Some(TEST_GUILD_ID), None, None)
        .unwrap_err();
    assert_eq!(error.code, VALIDATION_ERROR);
}

#[test]
fn test_exceeds_max_fails() {
    let error = registered_service()
        .validate_loan(12_345, 200, Some(TEST_GUILD_ID), None, None)
        .unwrap_err();
    assert_eq!(error.code, LOAN_AMOUNT_EXCEEDED);
}

#[test]
fn test_boolean_context() {
    let result = registered_service().validate_loan(12_345, 50, None, None, None);
    assert!(result.is_ok());
}

#[test]
fn test_unwrap_on_success() {
    let approval = registered_service()
        .validate_loan(12_345, 50, None, None, None)
        .unwrap();
    assert_eq!(approval.amount, 50);
}

#[test]
fn test_unwrap_or_on_failure() {
    let approval = registered_service()
        .validate_loan(12_345, -10, None, None, None)
        .ok();
    assert_eq!(approval, None);
}

#[test]
fn test_loan_service_delegates_bulk_borrower_lookup() {
    let port = FakeLoanPort::default();
    port.outstanding.borrow_mut().insert(2);
    let service = service(port);
    assert_eq!(
        service
            .get_outstanding_borrower_ids(&[1, 2, 2], Some(77))
            .unwrap(),
        HashSet::from([2])
    );
    assert_eq!(
        service.port().bulk_calls.borrow().as_slice(),
        &[(vec![1, 2, 2], Some(77))]
    );
}

fn repayment(
    principal: i64,
    fee: i64,
    balance_before: i64,
    new_balance: i64,
    nonprofit_total: i64,
) -> RepaymentResult {
    RepaymentResult {
        principal,
        fee,
        total_repaid: principal + fee,
        balance_before,
        new_balance,
        nonprofit_total,
    }
}

#[test]
fn test_match_repayment_bulk_checks_once_and_attempts_each_borrower_once() {
    let port = FakeLoanPort::default();
    port.outstanding.borrow_mut().extend([2, 4, 5]);
    port.repayments.borrow_mut().extend([
        Err(LoanPortError::Backend("Loan was already repaid".to_owned())),
        Ok(repayment(40, 8, 100, 52, 8)),
        Ok(repayment(25, 5, 50, 20, 13)),
    ]);
    let service = service(port);
    let participant_ids = [1, 2, 2, 3, 4, 5, 99];
    let repayments = service
        .repay_outstanding_match_loans(&participant_ids, Some(77))
        .unwrap();
    assert_eq!(
        service.port().bulk_calls.borrow().as_slice(),
        &[(participant_ids.to_vec(), Some(77))]
    );
    assert_eq!(
        service.port().repayment_calls.borrow().as_slice(),
        &[(2, Some(77)), (4, Some(77)), (5, Some(77))]
    );
    assert_eq!(
        repayments
            .iter()
            .map(|repayment| repayment.player_id)
            .collect::<Vec<_>>(),
        [4, 5]
    );
    assert_eq!(
        repayments
            .iter()
            .map(|repayment| repayment.total_repaid)
            .collect::<Vec<_>>(),
        [48, 30]
    );
}

// Stronger, intentionally unmapped service invariants follow.

#[test]
fn zero_timestamp_is_not_a_cooldown_and_future_timestamp_is() {
    let reset = derive_state(
        -1,
        Some(StoredLoanState {
            discord_id: -1,
            last_loan_at: Some(0),
            ..StoredLoanState::default()
        }),
        NOW,
        100,
    );
    assert!(!reset.is_on_cooldown);
    assert_eq!(reset.cooldown_ends_at, None);

    let future = derive_state(
        -1,
        Some(StoredLoanState {
            discord_id: -1,
            last_loan_at: Some(NOW + 10),
            ..StoredLoanState::default()
        }),
        NOW,
        100,
    );
    assert!(future.is_on_cooldown);
    assert_eq!(future.cooldown_ends_at, Some(NOW + 110));
}

#[test]
fn overrides_are_per_call_and_do_not_mutate_service_policy() {
    let service = registered_service();
    let adjusted = service
        .validate_loan(12_345, 150, Some(TEST_GUILD_ID), Some(0.10), Some(200))
        .unwrap();
    assert_eq!((adjusted.fee, adjusted.total_owed), (15, 165));
    assert_eq!(
        service
            .validate_loan(12_345, 150, Some(TEST_GUILD_ID), None, None)
            .unwrap_err()
            .code,
        LOAN_AMOUNT_EXCEEDED
    );
}

#[test]
fn repository_error_variants_map_without_message_parsing() {
    let cases = [
        (
            LoanPortError::OutstandingLoan {
                principal: 1,
                fee: 1,
                total_owed: 2,
            },
            LOAN_ALREADY_EXISTS,
        ),
        (
            LoanPortError::CooldownActive {
                hours: 1,
                minutes: 2,
            },
            COOLDOWN_ACTIVE,
        ),
        (LoanPortError::InvalidLoanAmount, VALIDATION_ERROR),
        (
            LoanPortError::LoanAmountExceeded { max_amount: 100 },
            LOAN_AMOUNT_EXCEEDED,
        ),
        (LoanPortError::PlayerNotFound, PLAYER_NOT_FOUND),
        (LoanPortError::NoOutstandingLoan, NO_OUTSTANDING_LOAN),
        (LoanPortError::Backend("db".to_owned()), STORAGE_ERROR),
    ];
    for (error, expected) in cases {
        assert_eq!(map_port_error(error).code, expected);
    }
}

#[test]
fn signed_participant_ids_preserve_input_order_in_match_hook() {
    let port = FakeLoanPort::default();
    port.outstanding.borrow_mut().extend([-5, -2]);
    port.repayments
        .borrow_mut()
        .extend([Ok(repayment(1, 0, 0, -1, 0)), Ok(repayment(1, 0, 0, -1, 0))]);
    let service = service(port);
    let repayments = service
        .repay_outstanding_match_loans(&[-5, -2, -5], None)
        .unwrap();
    assert_eq!(
        repayments
            .iter()
            .map(|repayment| repayment.player_id)
            .collect::<Vec<_>>(),
        [-5, -2]
    );
}
