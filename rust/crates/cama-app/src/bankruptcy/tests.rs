use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::*;

const GUILD: i64 = 12_345;

#[derive(Clone, Debug, Default)]
struct FakeData {
    balances: BTreeMap<(i64, i64), i64>,
    states: BTreeMap<(i64, i64), StoredBankruptcyState>,
}

#[derive(Clone, Debug, Default)]
struct FakePort {
    data: Rc<RefCell<FakeData>>,
}

impl FakePort {
    fn guild(guild_id: Option<i64>) -> i64 {
        guild_id.unwrap_or(0)
    }

    fn add_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.data
            .borrow_mut()
            .balances
            .insert((guild_id, discord_id), balance);
    }

    fn set_balance(&self, discord_id: i64, guild_id: i64, balance: i64) {
        let replaced = self
            .data
            .borrow_mut()
            .balances
            .insert((guild_id, discord_id), balance);
        assert!(replaced.is_some(), "fixture player must exist");
    }
}

impl BankruptcyPort for FakePort {
    fn get_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, BankruptcyPortError> {
        Ok(self
            .data
            .borrow()
            .balances
            .get(&(Self::guild(guild_id), discord_id))
            .copied())
    }

    fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<StoredBankruptcyState>, BankruptcyPortError> {
        Ok(self
            .data
            .borrow()
            .states
            .get(&(Self::guild(guild_id), discord_id))
            .copied())
    }

    fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, StoredBankruptcyState>, BankruptcyPortError> {
        let guild_id = Self::guild(guild_id);
        let data = self.data.borrow();
        Ok(discord_ids
            .iter()
            .filter_map(|discord_id| {
                data.states
                    .get(&(guild_id, *discord_id))
                    .copied()
                    .map(|state| (*discord_id, state))
            })
            .collect())
    }

    fn get_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError> {
        Ok(self
            .data
            .borrow()
            .states
            .get(&(Self::guild(guild_id), discord_id))
            .map_or(0, |state| state.penalty_games_remaining))
    }

    fn declare_atomic(
        &self,
        request: AtomicDeclarationRequest,
    ) -> Result<AtomicDeclarationOutcome, BankruptcyPortError> {
        let guild_id = Self::guild(request.guild_id);
        let key = (guild_id, request.discord_id);
        let mut data = self.data.borrow_mut();
        let Some(balance) = data.balances.get(&key).copied() else {
            return Ok(AtomicDeclarationOutcome::PlayerNotFound);
        };
        if balance >= 0 {
            return Ok(AtomicDeclarationOutcome::NotInDebt { balance });
        }
        if let Some(last) = data
            .states
            .get(&key)
            .and_then(|state| state.last_bankruptcy_at)
            .filter(|last| *last != 0)
        {
            let elapsed = request.now.saturating_sub(last);
            if elapsed < request.cooldown_seconds {
                return Ok(AtomicDeclarationOutcome::Cooldown {
                    remaining_seconds: request.cooldown_seconds.saturating_sub(elapsed),
                });
            }
        }
        let bankruptcy_count = data
            .states
            .get(&key)
            .map_or(1, |state| state.bankruptcy_count.saturating_add(1));
        data.balances.insert(key, request.fresh_start_balance);
        data.states.insert(
            key,
            StoredBankruptcyState {
                discord_id: request.discord_id,
                guild_id,
                last_bankruptcy_at: Some(request.now),
                penalty_games_remaining: request.penalty_games,
                bankruptcy_count,
            },
        );
        Ok(AtomicDeclarationOutcome::Applied {
            debt_cleared: balance.saturating_abs(),
            new_balance: request.fresh_start_balance,
            penalty_games: request.penalty_games,
            bankruptcy_count,
        })
    }

    fn decrement_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyPortError> {
        let key = (Self::guild(guild_id), discord_id);
        let mut data = self.data.borrow_mut();
        let Some(state) = data.states.get_mut(&key) else {
            return Ok(0);
        };
        state.penalty_games_remaining = state.penalty_games_remaining.saturating_sub(1).max(0);
        Ok(state.penalty_games_remaining)
    }

    fn decrement_penalty_games_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BankruptcyPortError> {
        let mut remaining = BTreeMap::new();
        for discord_id in discord_ids {
            remaining.insert(
                *discord_id,
                self.decrement_penalty_games(*discord_id, guild_id)?,
            );
        }
        Ok(remaining)
    }
}

#[derive(Clone, Debug)]
struct FixedClock(Rc<Cell<i64>>);

impl Clock for FixedClock {
    fn unix_timestamp(&self) -> Result<i64, String> {
        Ok(self.0.get())
    }
}

struct Harness {
    service: BankruptcyService<FakePort, FixedClock>,
    port: FakePort,
    now: Rc<Cell<i64>>,
}

impl Harness {
    fn new() -> Self {
        Self::with_policy(bankruptcy_policy())
    }

    fn with_policy(policy: BankruptcyPolicy) -> Self {
        let port = FakePort::default();
        let now = Rc::new(Cell::new(1_700_000_000));
        let service =
            BankruptcyService::with_clock(port.clone(), FixedClock(Rc::clone(&now)), policy)
                .unwrap();
        Self { service, port, now }
    }
}

fn bankruptcy_policy() -> BankruptcyPolicy {
    BankruptcyPolicy {
        fresh_start_balance: 3,
        cooldown_seconds: 604_800,
        penalty_games: 5,
        penalty_rate_per_game: 0.1,
    }
}

fn declare(harness: &Harness, discord_id: i64) -> BankruptcyDeclaration {
    match harness
        .service
        .execute_bankruptcy(discord_id, Some(GUILD))
        .unwrap()
    {
        BankruptcyExecution::Applied(declaration) => declaration,
        BankruptcyExecution::Rejected(rejection) => panic!("unexpected rejection: {rejection:?}"),
    }
}

#[test]
fn test_cannot_declare_bankruptcy_with_positive_balance() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, 100);
    let validation = harness
        .service
        .validate_bankruptcy(1_001, Some(GUILD))
        .unwrap();
    assert_eq!(
        validation,
        BankruptcyValidation::Rejected(BankruptcyRejection::NotInDebt { balance: 100 })
    );
    let BankruptcyValidation::Rejected(rejection) = validation else {
        panic!("expected rejection");
    };
    assert_eq!(rejection.error_code(), NOT_IN_DEBT);
}

#[test]
fn test_cannot_declare_bankruptcy_with_zero_balance() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, 0);
    assert_eq!(
        harness
            .service
            .validate_bankruptcy(1_001, Some(GUILD))
            .unwrap(),
        BankruptcyValidation::Rejected(BankruptcyRejection::NotInDebt { balance: 0 })
    );
}

#[test]
fn test_can_declare_bankruptcy_with_negative_balance() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -100);
    assert_eq!(
        harness
            .service
            .validate_bankruptcy(1_001, Some(GUILD))
            .unwrap(),
        BankruptcyValidation::Eligible { debt: 100 }
    );
}

#[test]
fn test_cannot_declare_bankruptcy_on_cooldown() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);
    harness.port.set_balance(1_001, GUILD, -100);

    let validation = harness
        .service
        .validate_bankruptcy(1_001, Some(GUILD))
        .unwrap();
    assert_eq!(
        validation,
        BankruptcyValidation::Rejected(BankruptcyRejection::Cooldown {
            remaining_seconds: 604_800,
        })
    );
}

#[test]
fn test_cooldown_expires_after_duration() {
    let mut policy = bankruptcy_policy();
    policy.cooldown_seconds = 1;
    let harness = Harness::with_policy(policy);
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);
    harness.now.set(harness.now.get() + 2);
    harness.port.set_balance(1_001, GUILD, -100);

    assert!(
        harness
            .service
            .validate_bankruptcy(1_001, Some(GUILD))
            .unwrap()
            .is_eligible()
    );
}

#[test]
fn test_penalty_reduces_winnings() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -300);
    declare(&harness, 1_001);

    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(1_001, 10, Some(GUILD))
            .unwrap(),
        PenaltyResult {
            original: 10,
            penalized: 5,
            penalty_applied: 5,
        }
    );
}

#[test]
fn test_no_penalty_without_bankruptcy() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, 100);
    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(1_001, 10, Some(GUILD))
            .unwrap(),
        PenaltyResult {
            original: 10,
            penalized: 10,
            penalty_applied: 0,
        }
    );
}

#[test]
fn test_no_state_for_new_player() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, 3);
    assert_eq!(
        harness.service.get_state(1_001, Some(GUILD)).unwrap(),
        BankruptcyState {
            discord_id: 1_001,
            last_bankruptcy_at: None,
            penalty_games_remaining: 0,
            is_on_cooldown: false,
            cooldown_ends_at: None,
            bankruptcy_count: 0,
        }
    );
}

#[test]
fn test_state_after_bankruptcy() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);

    let state = harness.service.get_state(1_001, Some(GUILD)).unwrap();
    assert_eq!(state.last_bankruptcy_at, Some(harness.now.get()));
    assert_eq!(state.penalty_games_remaining, 5);
    assert!(state.is_on_cooldown);
    assert_eq!(state.cooldown_ends_at, Some(harness.now.get() + 604_800));
    assert_eq!(state.bankruptcy_count, 1);
}

#[test]
fn test_get_bulk_states_empty_list() {
    let harness = Harness::new();
    assert_eq!(
        harness.service.get_bulk_states(&[], Some(GUILD)).unwrap(),
        BTreeMap::new()
    );
}

#[test]
fn test_get_bulk_states_single_user_no_bankruptcy() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, 100);
    let states = harness
        .service
        .get_bulk_states(&[1_001], Some(GUILD))
        .unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[&1_001].penalty_games_remaining, 0);
    assert!(!states[&1_001].is_on_cooldown);
    assert_eq!(states[&1_001].last_bankruptcy_at, None);
}

#[test]
fn test_get_bulk_states_single_user_with_bankruptcy() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);

    let states = harness
        .service
        .get_bulk_states(&[1_001], Some(GUILD))
        .unwrap();
    assert_eq!(states[&1_001].penalty_games_remaining, 5);
    assert!(states[&1_001].is_on_cooldown);
    assert_eq!(states[&1_001].last_bankruptcy_at, Some(harness.now.get()));
}

#[test]
fn test_get_bulk_states_multiple_users_mixed() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -200);
    harness.port.add_player(1_002, GUILD, 50);
    harness.port.add_player(1_003, GUILD, 10);
    declare(&harness, 1_001);

    let states = harness
        .service
        .get_bulk_states(&[1_001, 1_002, 1_003], Some(GUILD))
        .unwrap();
    assert_eq!(states.len(), 3);
    assert_eq!(states[&1_001].penalty_games_remaining, 5);
    assert!(states[&1_001].is_on_cooldown);
    assert_eq!(states[&1_002].penalty_games_remaining, 0);
    assert!(!states[&1_002].is_on_cooldown);
    assert_eq!(states[&1_003].penalty_games_remaining, 0);
}

#[test]
fn test_get_bulk_states_cooldown_calculation() {
    let mut policy = bankruptcy_policy();
    policy.cooldown_seconds = 2;
    let harness = Harness::with_policy(policy);
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);

    let current = harness
        .service
        .get_bulk_states(&[1_001], Some(GUILD))
        .unwrap();
    assert!(current[&1_001].is_on_cooldown);
    harness.now.set(harness.now.get() + 3);
    let expired = harness
        .service
        .get_bulk_states(&[1_001], Some(GUILD))
        .unwrap();
    assert!(!expired[&1_001].is_on_cooldown);
    assert_eq!(expired[&1_001].cooldown_ends_at, None);
}

#[test]
fn test_get_bulk_states_nonexistent_user() {
    let harness = Harness::new();
    let states = harness
        .service
        .get_bulk_states(&[999_999_999], None)
        .unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[&999_999_999].penalty_games_remaining, 0);
    assert!(!states[&999_999_999].is_on_cooldown);
}

#[test]
fn test_get_bulk_states_with_duplicates() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -200);
    declare(&harness, 1_001);

    let states = harness
        .service
        .get_bulk_states(&[1_001, 1_001, 1_001], Some(GUILD))
        .unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[&1_001].penalty_games_remaining, 5);
}

fn debuff_harness() -> Harness {
    Harness::with_policy(BankruptcyPolicy::default())
}

#[test]
fn test_bankruptcy_debuff_sets_three_penalty_games() {
    let harness = debuff_harness();
    harness.port.add_player(5_001, GUILD, -50);
    let declaration = declare(&harness, 5_001);
    assert_eq!(declaration.penalty_games, 3);
    assert_eq!(declaration.new_balance, 3);
    assert_eq!(
        harness
            .service
            .get_state(5_001, Some(GUILD))
            .unwrap()
            .penalty_games_remaining,
        3
    );
}

#[test]
fn test_bankruptcy_debuff_three_wins_clear_penalty() {
    let harness = debuff_harness();
    harness.port.add_player(5_002, GUILD, -50);
    declare(&harness, 5_002);
    assert_eq!(harness.service.on_game_won(5_002, Some(GUILD)).unwrap(), 2);
    assert_eq!(harness.service.on_game_won(5_002, Some(GUILD)).unwrap(), 1);
    assert_eq!(harness.service.on_game_won(5_002, Some(GUILD)).unwrap(), 0);
    assert_eq!(
        harness
            .service
            .get_state(5_002, Some(GUILD))
            .unwrap()
            .penalty_games_remaining,
        0
    );
}

#[test]
fn test_bankruptcy_debuff_default_rate_withholds_five_percent_per_game() {
    let harness = debuff_harness();
    harness.port.add_player(5_003, GUILD, -50);
    declare(&harness, 5_003);
    // Three outstanding games at the default 5% per game withhold 15%.
    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(5_003, 100, Some(GUILD))
            .unwrap(),
        PenaltyResult {
            original: 100,
            penalized: 85,
            penalty_applied: 15,
        }
    );
}

#[test]
fn test_bankruptcy_debuff_trivia_base_and_streak_schedule() {
    use crate::trivia_commands::{jc_for_streak, streak_bonus_for_streak};

    assert_eq!(streak_bonus_for_streak(3), 1);
    assert_eq!(streak_bonus_for_streak(6), 1);
    assert_eq!(streak_bonus_for_streak(10), 2);
    assert_eq!(streak_bonus_for_streak(14), 1);
    assert_eq!(streak_bonus_for_streak(7), 0);
    assert_eq!(jc_for_streak(7), 1);
    assert_eq!(jc_for_streak(10), 3);
}

#[test]
fn test_bankruptcy_debuff_tiny_trivia_bonus_is_not_zeroed() {
    let harness = debuff_harness();
    harness.port.add_player(8_001, GUILD, -50);
    declare(&harness, 8_001);
    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(8_001, 2, Some(GUILD))
            .unwrap()
            .penalized,
        2
    );
    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(8_001, 1, Some(GUILD))
            .unwrap()
            .penalized,
        1
    );
}

#[test]
fn test_bankruptcy_debuff_plain_trivia_keeps_full_bonus() {
    let harness = debuff_harness();
    harness.port.add_player(8_002, GUILD, 100);
    assert_eq!(
        harness
            .service
            .apply_penalty_to_winnings(8_002, 2, Some(GUILD))
            .unwrap(),
        PenaltyResult {
            original: 2,
            penalized: 2,
            penalty_applied: 0,
        }
    );
}

#[test]
fn test_bankruptcy_debuff_trivia_reads_without_clearing_penalty() {
    let harness = debuff_harness();
    harness.port.add_player(8_003, GUILD, -50);
    declare(&harness, 8_003);
    for _ in 0..3 {
        let result = harness
            .service
            .apply_penalty_to_winnings(8_003, 2, Some(GUILD))
            .unwrap();
        assert_eq!(result.penalized, 2);
    }
    assert_eq!(
        harness
            .service
            .get_state(8_003, Some(GUILD))
            .unwrap()
            .penalty_games_remaining,
        3
    );
}

#[test]
fn test_penalty_scales_with_games_remaining() {
    let harness = Harness::new();
    harness.port.add_player(1_001, GUILD, -300);
    // The harness policy withholds 10% per outstanding game, starting at five.
    declare(&harness, 1_001);
    let full = harness
        .service
        .apply_penalty_to_winnings(1_001, 12, Some(GUILD))
        .unwrap();
    assert_eq!(full.penalty_applied, 6);

    harness
        .port
        .decrement_penalty_games(1_001, Some(GUILD))
        .unwrap();
    let four_left = harness
        .service
        .apply_penalty_to_winnings(1_001, 12, Some(GUILD))
        .unwrap();
    assert_eq!(four_left.penalty_applied, 4);

    for _ in 0..3 {
        harness
            .port
            .decrement_penalty_games(1_001, Some(GUILD))
            .unwrap();
    }
    let one_left = harness
        .service
        .apply_penalty_to_winnings(1_001, 12, Some(GUILD))
        .unwrap();
    assert_eq!(
        one_left,
        PenaltyResult {
            original: 12,
            penalized: 11,
            penalty_applied: 1,
        }
    );
}

#[test]
fn test_calculate_penalty_scales_linearly_and_caps_at_full_withholding() {
    // 5% per outstanding game: 1 -> 5%, 3 -> 15%, 6 -> 30%, 20+ -> 100%.
    assert_eq!(calculate_penalty(120, 1, 0.05).penalty_applied, 6);
    assert_eq!(calculate_penalty(120, 2, 0.05).penalty_applied, 12);
    assert_eq!(calculate_penalty(120, 3, 0.05).penalty_applied, 18);
    assert_eq!(calculate_penalty(120, 6, 0.05).penalty_applied, 36);
    assert_eq!(calculate_penalty(120, 20, 0.05).penalty_applied, 120);
    assert_eq!(calculate_penalty(120, 40, 0.05).penalty_applied, 120);
    assert_eq!(calculate_penalty(120, 0, 0.05).penalty_applied, 0);
}
