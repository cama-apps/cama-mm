use std::collections::BTreeSet;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::predictions_repository::{Book, Position, PredictionRepository, Trade};
use crate::test_support::FastTestDatabase;

const GUILD: i64 = 7_777;
const OTHER_GUILD: i64 = 8_888;
const ADMIN: i64 = 999;
const NOW: i64 = 2_000_000_000;

fn fixture_template() -> &'static NamedTempFile {
    static TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let file = NamedTempFile::new().expect("prediction resolution template");
        Connection::open(file.path())
            .expect("open fixture template")
            .execute_batch(FIXTURE_SCHEMA)
            .expect("create migrated-schema fixture");
        file
    })
}

struct Fixture {
    file: FastTestDatabase,
    repository: PredictionResolutionRepository,
    orderbook: PredictionRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = FastTestDatabase::from_template(fixture_template().path());
        Self {
            repository: PredictionResolutionRepository::new(file.path()),
            orderbook: PredictionRepository::new(file.path()),
            file,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("fixture connection")
    }

    fn player(&self, discord_id: i64, balance: i64) {
        self.player_in_guild(discord_id, GUILD, balance);
    }

    fn player_in_guild(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![discord_id, guild_id, format!("user{discord_id}"), balance],
            )
            .expect("insert player");
    }

    fn delete_player(&self, discord_id: i64) {
        self.connection()
            .execute(
                "DELETE FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
            )
            .expect("delete player");
    }

    fn set_balance(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "UPDATE players SET jopacoin_balance=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![balance, discord_id, GUILD],
            )
            .expect("set balance");
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("balance")
    }

    fn set_bankruptcy_penalty(&self, discord_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO bankruptcy_state(discord_id,guild_id,penalty_games_remaining)
                 VALUES (?1,?2,3)",
                params![discord_id, GUILD],
            )
            .expect("penalty state");
    }

    fn market(&self, question: &str) -> i64 {
        self.orderbook
            .create_orderbook_prediction(Some(GUILD), ADMIN, question, 50, None, &initial_levels())
            .expect("create orderbook market")
    }

    fn bare_market(&self, guild_id: i64, status: &str, created_at: i64, closes_at: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO predictions
                     (guild_id,creator_id,question,status,created_at,closes_at,
                      current_price,initial_fair,last_refresh_at,lp_pnl)
                 VALUES (?1,1,'expiry?',?2,?3,?4,50,50,?3,0)",
                params![guild_id, status, created_at, closes_at],
            )
            .expect("insert bare market");
        connection.last_insert_rowid()
    }

    fn buy(&self, prediction_id: i64, discord_id: i64, side: ContractSide, amount: i64) {
        self.orderbook
            .buy_contracts_atomic(prediction_id, discord_id, side, amount)
            .expect("buy contracts");
    }

    fn settle(
        &self,
        prediction_id: i64,
        outcome: ContractSide,
        adjustments: &SettlementAdjustments,
    ) -> Result<Resolution, PredictionResolutionError> {
        self.repository
            .settle_orderbook_at(prediction_id, outcome, Some(ADMIN), adjustments, NOW)
    }

    fn rollback(&self, prediction_id: i64) -> Result<RollbackResult, PredictionResolutionError> {
        self.repository.rollback_orderbook_at(
            prediction_id,
            GUILD,
            &initial_levels(),
            Some(ADMIN),
            NOW + 1,
        )
    }

    fn status(&self, prediction_id: i64) -> String {
        self.connection()
            .query_row(
                "SELECT status FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| row.get(0),
            )
            .expect("status")
    }

    fn lp_pnl(&self, prediction_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT COALESCE(lp_pnl,0) FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| row.get(0),
            )
            .expect("lp pnl")
    }

    fn position(&self, prediction_id: i64, discord_id: i64) -> Position {
        self.orderbook
            .get_position(prediction_id, discord_id)
            .expect("position lookup")
            .expect("position")
    }

    fn deductions(&self, prediction_id: i64, discord_id: i64) -> (i64, i64, i64) {
        self.connection()
            .query_row(
                "SELECT bankruptcy_penalty,vanity_tax,low_priority_tax
                 FROM prediction_positions
                 WHERE prediction_id=?1 AND discord_id=?2",
                params![prediction_id, discord_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("deductions")
    }

    fn book(&self, prediction_id: i64) -> Book {
        self.orderbook.get_book(prediction_id).expect("book")
    }

    fn trades(&self, prediction_id: i64) -> Vec<Trade> {
        self.orderbook
            .recent_trades(prediction_id, 20)
            .expect("trades")
    }

    fn ledger_count(&self, prediction_id: i64, source: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND source=?2 AND related_type='prediction'
                   AND related_id=?3",
                params![GUILD, source, prediction_id.to_string()],
                |row| row.get(0),
            )
            .expect("ledger count")
    }
}

fn initial_levels() -> Vec<NewLevel> {
    [50, 50, 50, 20, 15, 10, 5]
        .into_iter()
        .enumerate()
        .flat_map(|(index, size)| {
            let offset = 2 + i64::try_from(index).expect("small index");
            [
                NewLevel::ask(50 + offset, size),
                NewLevel::bid(50 - offset, size),
            ]
        })
        .collect()
}

fn penalty_adjustments() -> SettlementAdjustments {
    SettlementAdjustments {
        bankruptcy_credit_bps: Some(5_000),
        ..SettlementAdjustments::default()
    }
}

#[test]
fn configured_contract_value_drives_gross_settlement_and_ledger_metadata() {
    let fixture = Fixture::new();
    fixture.player(6_111, 1_000);
    let prediction_id = fixture.market("Configured contract payout?");
    fixture.buy(prediction_id, 6_111, ContractSide::Yes, 3);
    let before = fixture.balance(6_111);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                contract_value: 17,
                ..SettlementAdjustments::default()
            },
        )
        .expect("configured settlement");
    assert_eq!(result.total_payout, 51);
    assert_eq!(fixture.balance(6_111) - before, 51);
}

fn vanity_adjustments(discord_id: i64) -> SettlementAdjustments {
    SettlementAdjustments {
        payout_multiplier_bps: 100_000,
        vanity_tax_bps: 1_000,
        vanity_taxable_ids: BTreeSet::from([discord_id]),
        ..SettlementAdjustments::default()
    }
}

fn low_priority_adjustments(discord_id: i64) -> SettlementAdjustments {
    SettlementAdjustments {
        payout_multiplier_bps: 100_000,
        low_priority_tax_bps: 1_000,
        low_priority_taxable_ids: BTreeSet::from([discord_id]),
        ..SettlementAdjustments::default()
    }
}

#[test]
fn test_lock_expired_predictions_is_atomic_ordered_and_guild_scoped() {
    let fixture = Fixture::new();
    let old = fixture.bare_market(GUILD, "open", 100, NOW - 30);
    let newest = fixture.bare_market(GUILD, "open", 300, NOW);
    let middle = fixture.bare_market(GUILD, "open", 200, NOW - 10);
    let future = fixture.bare_market(GUILD, "open", 400, NOW + 1);
    let already_locked = fixture.bare_market(GUILD, "locked", 500, NOW - 20);
    let other = fixture.bare_market(OTHER_GUILD, "open", 600, NOW - 20);

    assert_eq!(
        fixture
            .repository
            .lock_expired_predictions(GUILD, NOW)
            .expect("bulk lock"),
        vec![newest, middle, old]
    );
    assert_eq!(fixture.status(old), "locked");
    assert_eq!(fixture.status(middle), "locked");
    assert_eq!(fixture.status(newest), "locked");
    assert_eq!(fixture.status(future), "open");
    assert_eq!(fixture.status(already_locked), "locked");
    assert_eq!(fixture.status(other), "open");
}

#[test]
fn test_check_and_lock_expired_locks_only_past_close() {
    let fixture = Fixture::new();
    let expired = fixture.bare_market(GUILD, "open", 1, NOW - 10);
    let future = fixture.bare_market(GUILD, "open", 2, NOW + 3_600);
    let locked = fixture
        .repository
        .lock_expired_predictions(GUILD, NOW)
        .expect("lock expired");
    assert!(locked.contains(&expired));
    assert!(!locked.contains(&future));
    assert_eq!(fixture.status(expired), "locked");
    assert_eq!(fixture.status(future), "open");
}

#[test]
fn test_check_and_lock_expired_ignores_already_locked() {
    let fixture = Fixture::new();
    let prediction_id = fixture.bare_market(GUILD, "locked", 1, NOW - 10);
    assert!(
        fixture
            .repository
            .lock_expired_predictions(GUILD, NOW)
            .expect("lock expired")
            .is_empty()
    );
    assert_eq!(fixture.status(prediction_id), "locked");
}

#[test]
fn test_resolve_orderbook_pays_no_side_when_no_wins() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.player(2, 1_000);
    let prediction_id = fixture.market("who wins side?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    fixture.buy(prediction_id, 2, ContractSide::No, 4);
    let yes_before = fixture.balance(1);
    let no_before = fixture.balance(2);

    let result = fixture
        .settle(
            prediction_id,
            ContractSide::No,
            &SettlementAdjustments::default(),
        )
        .expect("settle no");
    assert_eq!(result.total_payout, 4 * CONTRACT_VALUE);
    assert_eq!(fixture.balance(2) - no_before, 4 * CONTRACT_VALUE);
    assert_eq!(fixture.balance(1), yes_before);
}

#[test]
fn test_resolve_orderbook_two_sided_holder_profit_nets_both_bases() {
    let fixture = Fixture::new();
    fixture.player(1, 10_000);
    let prediction_id = fixture.market("hedger?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    fixture.buy(prediction_id, 1, ContractSide::No, 3);
    let position = fixture.position(prediction_id, 1);
    assert!(position.no_cost_basis_total > 0);
    let before = fixture.balance(1);

    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle hedger");
    let payout = 5 * CONTRACT_VALUE;
    assert_eq!(fixture.balance(1) - before, payout);
    assert_eq!(result.winners[0].payout, payout);
    assert_eq!(
        result.winners[0].profit,
        payout - position.yes_cost_basis_total - position.no_cost_basis_total
    );
    assert_eq!(result.participants.len(), 1);
    assert_eq!(result.participants[0].yes_contracts, 5);
    assert_eq!(result.participants[0].no_contracts, 3);
    assert_eq!(
        result.participants[0].cost_basis,
        position.yes_cost_basis_total + position.no_cost_basis_total
    );
}

#[test]
fn test_resolve_orderbook_participant_pure_loser_reports_full_loss() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.player(2, 1_000);
    let prediction_id = fixture.market("pure loser?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    fixture.buy(prediction_id, 2, ContractSide::No, 4);
    let no_cost = fixture.position(prediction_id, 2).no_cost_basis_total;
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    let loser = result
        .participants
        .iter()
        .find(|participant| participant.discord_id == 2)
        .expect("loser participant");
    assert_eq!(loser.yes_contracts, 0);
    assert_eq!(loser.no_contracts, 4);
    assert_eq!(loser.cost_basis, no_cost);
    assert_eq!(loser.payout, 0);
    assert_eq!(loser.profit, -no_cost);
}

#[test]
fn test_resolve_orderbook_bankruptcy_penalty_is_netted_in_txn() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("penalty?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    let result = fixture
        .settle(prediction_id, ContractSide::Yes, &penalty_adjustments())
        .expect("penalized settlement");
    let payout = 5 * CONTRACT_VALUE;
    let penalty = (payout - cost) / 2;
    assert!(penalty > 0);
    assert_eq!(result.winners[0].bankruptcy_penalty, penalty);
    assert_eq!(fixture.balance(1) - before, payout - penalty);
    assert_eq!(fixture.deductions(prediction_id, 1).0, penalty);
}

#[test]
fn test_bankruptcy_debuff_orderbook_penalizes_profit_and_returns_cost_basis() {
    let fixture = Fixture::new();
    fixture.player(6_001, 1_000);
    fixture.set_bankruptcy_penalty(6_001);
    let prediction_id = fixture.market("75 percent penalty?");
    fixture.buy(prediction_id, 6_001, ContractSide::Yes, 5);
    let cost_basis = fixture.position(prediction_id, 6_001).yes_cost_basis_total;
    let before = fixture.balance(6_001);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                bankruptcy_credit_bps: Some(7_500),
                ..SettlementAdjustments::default()
            },
        )
        .expect("75 percent settlement");
    let winner = &result.winners[0];
    let gross_payout = 5 * CONTRACT_VALUE;
    let gross_profit = gross_payout - cost_basis;
    let expected_penalty = gross_profit * 2_500 / 10_000;
    assert!(gross_profit > 0);
    assert_eq!(winner.bankruptcy_penalty, expected_penalty);
    assert_eq!(winner.profit + expected_penalty, gross_profit);
    assert_eq!(winner.payout, gross_payout - expected_penalty);
    assert_eq!(fixture.balance(6_001) - before, winner.payout);
    assert!(winner.payout > 0);
}

#[test]
fn test_resolve_orderbook_vanity_tax_is_audited_and_rollback_safe() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("vanity tax?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    let result = fixture
        .settle(prediction_id, ContractSide::Yes, &vanity_adjustments(1))
        .expect("taxed settlement");
    let gross = 5 * CONTRACT_VALUE * 10;
    let tax = (gross - cost) / 10;
    assert_eq!(result.winners[0].vanity_tax, tax);
    assert_eq!(result.winners[0].payout, gross - tax);
    assert_eq!(fixture.balance(1) - before, gross - tax);
    assert_eq!(
        fixture
            .repository
            .user_orderbook_stats(1, GUILD)
            .expect("stats")
            .realized_pnl,
        gross - cost - tax
    );
    let tax_row: (i64, String, String) = fixture
        .connection()
        .query_row(
            "SELECT delta,related_type,related_id FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=1 AND source='vanity_tax'",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("tax ledger");
    assert_eq!(
        tax_row,
        (-tax, "prediction".to_owned(), prediction_id.to_string())
    );

    let rollback = fixture.rollback(prediction_id).expect("rollback");
    assert_eq!(rollback.total_reversed, gross - tax);
    assert_eq!(fixture.balance(1), before);
    assert_eq!(fixture.deductions(prediction_id, 1).1, 0);
}

#[test]
fn test_resolve_orderbook_low_priority_tax_is_audited_and_rollback_safe() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("low priority tax?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &low_priority_adjustments(1),
        )
        .expect("taxed settlement");
    let gross = 5 * CONTRACT_VALUE * 10;
    let tax = (gross - cost) / 10;
    assert!(tax > 0);
    assert_eq!(result.winners[0].low_priority_tax, tax);
    assert_eq!(result.winners[0].vanity_tax, 0);
    assert_eq!(result.winners[0].payout, gross - tax);
    assert_eq!(fixture.balance(1) - before, gross - tax);
    assert_eq!(
        fixture
            .repository
            .user_orderbook_stats(1, GUILD)
            .expect("stats")
            .realized_pnl,
        gross - cost - tax
    );
    let tax_row: (i64, String, String, String) = fixture
        .connection()
        .query_row(
            "SELECT delta,related_type,related_id,reason FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=1 AND source='low_priority_tax'",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("tax ledger");
    assert_eq!(
        tax_row,
        (
            -tax,
            "prediction".to_owned(),
            prediction_id.to_string(),
            "low priority tax on JC profit".to_owned()
        )
    );
    assert_eq!(fixture.ledger_count(prediction_id, "vanity_tax"), 0);
    assert_eq!(fixture.deductions(prediction_id, 1).2, tax);

    let rollback = fixture.rollback(prediction_id).expect("rollback");
    assert_eq!(rollback.total_reversed, gross - tax);
    assert_eq!(fixture.balance(1), before);
    assert_eq!(fixture.deductions(prediction_id, 1).2, 0);
}

#[test]
fn test_resolve_orderbook_stacks_low_priority_tax_beside_the_vanity_row() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("stacked withholding?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                bankruptcy_credit_bps: Some(5_000),
                vanity_tax_bps: 1_000,
                vanity_taxable_ids: BTreeSet::from([1]),
                ..low_priority_adjustments(1)
            },
        )
        .expect("stacked settlement");
    let gross = 5 * CONTRACT_VALUE * 10;
    let profit = gross - cost;
    let penalty = profit / 2;
    let tax = profit / 10;
    assert!(tax > 0);
    // Both taxes bill the same gross profit, not a post-vanity remainder.
    assert_eq!(result.winners[0].bankruptcy_penalty, penalty);
    assert_eq!(result.winners[0].vanity_tax, tax);
    assert_eq!(result.winners[0].low_priority_tax, tax);
    assert_eq!(result.winners[0].payout, gross - penalty - tax - tax);
    assert_eq!(fixture.balance(1) - before, gross - penalty - tax - tax);
    assert_eq!(fixture.deductions(prediction_id, 1), (penalty, tax, tax));
    assert_eq!(fixture.ledger_count(prediction_id, "vanity_tax"), 1);
    assert_eq!(fixture.ledger_count(prediction_id, "low_priority_tax"), 1);
    let settlement_delta: i64 = fixture
        .connection()
        .query_row(
            "SELECT delta FROM economy_ledger_entries
             WHERE guild_id=?1 AND source='prediction_resolution' AND related_id=?2",
            params![GUILD, prediction_id.to_string()],
            |row| row.get(0),
        )
        .expect("settlement ledger");
    assert_eq!(settlement_delta, gross - penalty);

    let rollback = fixture.rollback(prediction_id).expect("rollback");
    assert_eq!(rollback.total_reversed, gross - penalty - tax - tax);
    assert_eq!(fixture.balance(1), before);
    assert_eq!(fixture.deductions(prediction_id, 1), (0, 0, 0));
}

#[test]
fn test_low_priority_tax_is_capped_by_the_profit_earlier_withholdings_leave() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("fully withheld profit?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                bankruptcy_credit_bps: Some(0),
                ..low_priority_adjustments(1)
            },
        )
        .expect("fully withheld settlement");
    let gross = 5 * CONTRACT_VALUE * 10;
    assert_eq!(result.winners[0].bankruptcy_penalty, gross - cost);
    assert_eq!(result.winners[0].low_priority_tax, 0);
    assert_eq!(result.winners[0].payout, cost);
    assert_eq!(fixture.balance(1) - before, cost);
    assert_eq!(fixture.ledger_count(prediction_id, "low_priority_tax"), 0);
}

#[test]
fn test_low_priority_tax_reaches_only_listed_players_with_positive_profit() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.player(2, 1_000);
    let listed_market = fixture.market("listed player only?");
    fixture.buy(listed_market, 1, ContractSide::Yes, 5);
    fixture.buy(listed_market, 2, ContractSide::Yes, 5);
    let listed_cost = fixture.position(listed_market, 1).yes_cost_basis_total;
    let result = fixture
        .settle(
            listed_market,
            ContractSide::Yes,
            &low_priority_adjustments(1),
        )
        .expect("listed settlement");
    let gross = 5 * CONTRACT_VALUE * 10;
    assert_eq!(
        result.winners[0].low_priority_tax,
        (gross - listed_cost) / 10
    );
    assert_eq!(result.winners[1].low_priority_tax, 0);
    assert_eq!(fixture.deductions(listed_market, 2).2, 0);
    assert_eq!(fixture.ledger_count(listed_market, "low_priority_tax"), 1);

    let unprofitable_market = fixture.market("no profit to tax?");
    fixture.buy(unprofitable_market, 1, ContractSide::Yes, 5);
    let unprofitable = fixture
        .settle(
            unprofitable_market,
            ContractSide::Yes,
            &SettlementAdjustments {
                payout_multiplier_bps: 2_500,
                ..low_priority_adjustments(1)
            },
        )
        .expect("unprofitable settlement");
    assert!(unprofitable.winners[0].profit < 0);
    assert_eq!(unprofitable.winners[0].low_priority_tax, 0);
    assert_eq!(
        fixture.ledger_count(unprofitable_market, "low_priority_tax"),
        0
    );
}

#[test]
fn test_orderbook_stats_net_bankruptcy_penalty_for_winner() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("stats net penalty?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let cost = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let before = fixture.balance(1);
    fixture
        .settle(prediction_id, ContractSide::Yes, &penalty_adjustments())
        .expect("settle");
    let payout = 5 * CONTRACT_VALUE;
    let penalty = (payout - cost) / 2;
    let credited = fixture.balance(1) - before;
    assert_eq!(credited, payout - penalty);
    let stats = fixture
        .repository
        .user_orderbook_stats(1, GUILD)
        .expect("stats");
    assert_eq!(stats.realized_pnl, payout - cost - penalty);
    assert_eq!(stats.wins, 1);
    let history = fixture
        .repository
        .player_pnl_history(1, GUILD)
        .expect("history");
    assert_eq!(history[0].prediction_id, prediction_id);
    assert_eq!(history[0].delta, payout - cost - penalty);
}

#[test]
fn test_resolve_orderbook_penalty_uses_net_profit_for_hedger() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("hedge under penalty?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    fixture.buy(prediction_id, 1, ContractSide::No, 3);
    let position = fixture.position(prediction_id, 1);
    let before = fixture.balance(1);
    let result = fixture
        .settle(prediction_id, ContractSide::Yes, &penalty_adjustments())
        .expect("settle");
    let payout = 5 * CONTRACT_VALUE;
    let net_profit = payout - position.yes_cost_basis_total - position.no_cost_basis_total;
    let penalty = net_profit / 2;
    assert!(penalty > 0);
    assert_eq!(result.winners[0].bankruptcy_penalty, penalty);
    assert_eq!(fixture.balance(1) - before, payout - penalty);
}

#[test]
fn test_resolve_orderbook_settles_locked_market() {
    let fixture = Fixture::new();
    fixture.player(10, 1_000);
    let prediction_id = fixture.market("locked resolve?");
    fixture.buy(prediction_id, 10, ContractSide::Yes, 3);
    fixture
        .connection()
        .execute(
            "UPDATE predictions SET status='locked' WHERE prediction_id=?1",
            [prediction_id],
        )
        .expect("lock market");
    let before = fixture.balance(10);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle locked");
    assert_eq!(result.outcome, ContractSide::Yes);
    assert_eq!(result.total_payout, 3 * CONTRACT_VALUE);
    assert_eq!(fixture.balance(10) - before, 3 * CONTRACT_VALUE);
}

#[test]
fn test_resolve_orderbook_rejects_deleted_position_holder() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.player(2, 1_000);
    let prediction_id = fixture.market("deleted holder?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture.buy(prediction_id, 2, ContractSide::No, 1);
    let winner_basis = fixture.position(prediction_id, 1).yes_cost_basis_total;
    let survivor_balance = fixture.balance(2);
    let lp_before = fixture.lp_pnl(prediction_id);
    fixture.delete_player(1);

    assert!(matches!(
        fixture.settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default()
        ),
        Err(PredictionResolutionError::WinningPlayerNotFound)
    ));
    assert_eq!(fixture.status(prediction_id), "open");
    assert_eq!(fixture.lp_pnl(prediction_id), lp_before);
    assert_eq!(fixture.balance(2), survivor_balance);
    assert_eq!(fixture.position(prediction_id, 1).yes_contracts, 2);
    assert_eq!(
        fixture.ledger_count(prediction_id, "prediction_resolution"),
        0
    );

    fixture.player(1, 0);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("retry settlement");
    assert_eq!(result.winners[0].payout, 2 * CONTRACT_VALUE);
    assert_eq!(result.winners[0].profit, 2 * CONTRACT_VALUE - winner_basis);
    fixture.rollback(prediction_id).expect("rollback retry");
    assert_eq!(fixture.balance(1), 0);
    assert_eq!(fixture.status(prediction_id), "open");
}

#[test]
fn test_rollback_orderbook_restores_market_balances_positions_and_book() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.player(2, 1_000);
    let prediction_id = fixture.market("rollback this?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    fixture.buy(prediction_id, 2, ContractSide::No, 4);
    let balances = (fixture.balance(1), fixture.balance(2));
    let positions = (
        fixture.position(prediction_id, 1),
        fixture.position(prediction_id, 2),
    );
    let trades = fixture.trades(prediction_id);
    let lp_before = fixture.lp_pnl(prediction_id);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    assert!(fixture.book(prediction_id).yes_asks.is_empty());

    let rollback = fixture.rollback(prediction_id).expect("rollback");
    assert_eq!(
        rollback,
        RollbackResult {
            prediction_id,
            guild_id: GUILD,
            previous_outcome: ContractSide::Yes,
            total_reversed: 5 * CONTRACT_VALUE,
            affected_players: 1,
            lp_pnl: lp_before,
            current_price: 50,
        }
    );
    assert_eq!((fixture.balance(1), fixture.balance(2)), balances);
    assert_eq!(fixture.position(prediction_id, 1), positions.0);
    assert_eq!(fixture.position(prediction_id, 2), positions.1);
    assert_eq!(fixture.trades(prediction_id), trades);
    assert_eq!(fixture.status(prediction_id), "open");
    assert_eq!(fixture.lp_pnl(prediction_id), lp_before);
    let restored = fixture.book(prediction_id);
    assert_eq!(
        restored.yes_asks,
        initial_levels()
            .iter()
            .filter(|level| level.side.as_str() == "yes_ask")
            .map(|level| (level.price, level.size))
            .collect::<Vec<_>>()
    );
    let (reason, delta, actor): (String, i64, Option<i64>) = fixture
        .connection()
        .query_row(
            "SELECT s.reason,l.delta,l.actor_id
             FROM prediction_fair_snapshots s
             JOIN economy_ledger_entries l ON l.related_id=CAST(s.market_id AS TEXT)
             WHERE s.market_id=?1 AND l.source='prediction_resolution_rollback'
             ORDER BY s.snapshot_id DESC,l.ledger_id DESC LIMIT 1",
            [prediction_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("rollback audit");
    assert_eq!(
        (reason, delta, actor),
        ("rollback".to_owned(), -50, Some(ADMIN))
    );
    assert!(matches!(
        fixture.rollback(prediction_id),
        Err(PredictionResolutionError::CannotRollback(status)) if status == "open"
    ));
    fixture.buy(prediction_id, 2, ContractSide::Yes, 1);
}

#[test]
fn test_rollback_orderbook_uses_only_current_resolution_ledger_cycle() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("rollback twice?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    let before = fixture.balance(1);
    for _ in 0..2 {
        fixture
            .settle(
                prediction_id,
                ContractSide::Yes,
                &SettlementAdjustments::default(),
            )
            .expect("settle cycle");
        assert_eq!(
            fixture
                .rollback(prediction_id)
                .expect("rollback cycle")
                .total_reversed,
            2 * CONTRACT_VALUE
        );
        assert_eq!(fixture.balance(1), before);
    }
    assert_eq!(
        fixture.ledger_count(prediction_id, "prediction_resolution_rollback"),
        2
    );
}

#[test]
fn test_rollback_orderbook_rejects_market_from_another_guild() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("guild isolated?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    let balance = fixture.balance(1);
    assert!(matches!(
        fixture.repository.rollback_orderbook_at(
            prediction_id,
            OTHER_GUILD,
            &initial_levels(),
            Some(ADMIN),
            NOW + 1
        ),
        Err(PredictionResolutionError::PredictionNotFound)
    ));
    assert_eq!(fixture.balance(1), balance);
    assert_eq!(fixture.status(prediction_id), "resolved");
    assert_eq!(
        fixture.ledger_count(prediction_id, "prediction_resolution_rollback"),
        0
    );
}

#[test]
fn test_rollback_orderbook_rejects_deleted_winning_player() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("deleted winner rollback?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    fixture.delete_player(1);
    assert!(matches!(
        fixture.rollback(prediction_id),
        Err(PredictionResolutionError::WinningPlayerDeleted(1))
    ));
    assert_eq!(fixture.status(prediction_id), "resolved");
    assert_eq!(
        fixture.ledger_count(prediction_id, "prediction_resolution_rollback"),
        0
    );
}

#[test]
fn test_rollback_orderbook_rejects_deleted_and_recreated_winning_account() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("recreated winner rollback?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    fixture.delete_player(1);
    fixture.player(1, 250);
    assert!(matches!(
        fixture.rollback(prediction_id),
        Err(PredictionResolutionError::WinningPlayerRecreated(1))
    ));
    assert_eq!(fixture.balance(1), 250);
    assert_eq!(fixture.status(prediction_id), "resolved");
}

#[test]
fn test_rollback_orderbook_rejects_settlement_ledger_mismatch() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("mismatched ledger?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    let balance = fixture.balance(1);
    fixture
        .connection()
        .execute(
            "UPDATE economy_ledger_entries SET delta=delta+1
             WHERE guild_id=?1 AND source='prediction_resolution'
               AND related_id=?2",
            params![GUILD, prediction_id.to_string()],
        )
        .expect("corrupt ledger");
    assert!(matches!(
        fixture.rollback(prediction_id),
        Err(PredictionResolutionError::SettlementLedgerInconsistent)
    ));
    assert_eq!(fixture.balance(1), balance);
    assert_eq!(fixture.status(prediction_id), "resolved");
}

#[test]
fn test_rollback_orderbook_reverses_net_penalty_credit_and_allows_negative_balance() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    fixture.set_bankruptcy_penalty(1);
    let prediction_id = fixture.market("penalized rollback?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let before = fixture.balance(1);
    fixture
        .settle(prediction_id, ContractSide::Yes, &penalty_adjustments())
        .expect("settle");
    let penalty = fixture.deductions(prediction_id, 1).0;
    let net_credit = 5 * CONTRACT_VALUE - penalty;
    assert_eq!(fixture.balance(1), before + net_credit);
    fixture.set_balance(1, 10);
    let rollback = fixture.rollback(prediction_id).expect("rollback");
    assert_eq!(rollback.total_reversed, net_credit);
    assert_eq!(fixture.balance(1), 10 - net_credit);
    assert_eq!(fixture.deductions(prediction_id, 1).0, 0);
}

fn assert_non_resolved_rejected(status: &str) {
    let fixture = Fixture::new();
    let prediction_id = fixture.market("not resolved?");
    fixture
        .connection()
        .execute(
            "UPDATE predictions SET status=?1 WHERE prediction_id=?2",
            params![status, prediction_id],
        )
        .expect("set status");
    assert!(matches!(
        fixture.rollback(prediction_id),
        Err(PredictionResolutionError::CannotRollback(actual)) if actual == status
    ));
    assert_eq!(fixture.status(prediction_id), status);
}

#[test]
fn test_rollback_orderbook_rejects_market_that_is_not_resolved_open() {
    assert_non_resolved_rejected("open");
}

#[test]
fn test_rollback_orderbook_rejects_market_that_is_not_resolved_locked() {
    assert_non_resolved_rejected("locked");
}

#[test]
fn test_rollback_orderbook_rejects_market_that_is_not_resolved_cancelled() {
    assert_non_resolved_rejected("cancelled");
}

#[test]
fn test_rollback_orderbook_rejects_missing_market() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.rollback(999_999),
        Err(PredictionResolutionError::PredictionNotFound)
    ));
}

#[test]
fn test_rollback_orderbook_is_atomic_when_ladder_insert_fails() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("atomic rollback?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 2);
    fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle");
    let balance = fixture.balance(1);
    let status = fixture.status(prediction_id);
    fixture
        .connection()
        .execute_batch(&format!(
            "CREATE TRIGGER fail_rollback_ladder
             BEFORE INSERT ON prediction_levels
             WHEN NEW.prediction_id={prediction_id}
             BEGIN SELECT RAISE(ABORT,'forced ladder failure'); END;"
        ))
        .expect("failure trigger");
    let error = fixture.repository.rollback_orderbook_at(
        prediction_id,
        GUILD,
        &[NewLevel::ask(55, 1)],
        Some(ADMIN),
        NOW + 1,
    );
    assert!(matches!(error, Err(PredictionResolutionError::Sqlite(_))));
    assert_eq!(fixture.balance(1), balance);
    assert_eq!(fixture.status(prediction_id), status);
    assert!(fixture.book(prediction_id).yes_asks.is_empty());
    assert_eq!(
        fixture.ledger_count(prediction_id, "prediction_resolution_rollback"),
        0
    );
}

#[test]
fn test_event_multiplier_changes_atomic_resolution_and_audit() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("event-adjusted payout?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let position = fixture.position(prediction_id, 1);
    let balance_before = fixture.balance(1);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                payout_multiplier_bps: 7_500,
                ..SettlementAdjustments::default()
            },
        )
        .expect("event-adjusted settlement");

    let base_payout = 5 * CONTRACT_VALUE;
    // Python's round(37.5) and the Rust settlement both use bankers rounding.
    let expected_payout = 38;
    assert_eq!(result.total_payout, expected_payout);
    assert_eq!(result.payout_multiplier_bps, 7_500);
    assert_eq!(fixture.balance(1), balance_before + expected_payout);
    assert_eq!(
        result.lp_pnl,
        position.yes_cost_basis_total - expected_payout
    );

    let (delta, metadata): (i64, String) = fixture
        .connection()
        .query_row(
            "SELECT delta,metadata FROM economy_ledger_entries
             WHERE source='prediction_resolution' AND related_id=?1",
            [prediction_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("event settlement ledger");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata).expect("valid settlement metadata");
    assert_eq!(delta, expected_payout);
    assert_eq!(metadata["base_gross_payout"], base_payout);
    assert_eq!(metadata["gross_payout"], expected_payout);
    assert_eq!(metadata["payout_multiplier_bps"], 7_500);

    assert_eq!(
        fixture
            .repository
            .user_orderbook_stats(1, GUILD)
            .expect("event-adjusted stats")
            .realized_pnl,
        expected_payout - position.yes_cost_basis_total
    );
    assert_eq!(
        fixture
            .repository
            .player_pnl_history(1, GUILD)
            .expect("event-adjusted history")[0]
            .delta,
        expected_payout - position.yes_cost_basis_total
    );
}

#[test]
fn test_rollback_reverses_event_modified_payout() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("event payout rollback?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 5);
    let balance_before = fixture.balance(1);
    let lp_before = fixture.lp_pnl(prediction_id);
    let resolution = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                payout_multiplier_bps: 7_500,
                ..SettlementAdjustments::default()
            },
        )
        .expect("event-adjusted settlement");

    // Rollback reconstructs the exact historical payout from durable ledger
    // metadata; it does not consult whatever economy event is active now.
    let rolled_back = fixture.rollback(prediction_id).expect("event rollback");
    assert_eq!(rolled_back.total_reversed, resolution.total_payout);
    assert_eq!(fixture.balance(1), balance_before);
    assert_eq!(fixture.lp_pnl(prediction_id), lp_before);
    assert_eq!(fixture.status(prediction_id), "open");
}

#[test]
fn test_zero_payout_event_remains_auditable_and_rollback_safe() {
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("zero payout event?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 1);
    let position = fixture.position(prediction_id, 1);
    let balance_before = fixture.balance(1);
    let result = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments {
                payout_multiplier_bps: 0,
                ..SettlementAdjustments::default()
            },
        )
        .expect("zero-payout settlement");

    assert_eq!(result.total_payout, 0);
    assert_eq!(fixture.balance(1), balance_before);
    assert_eq!(
        fixture
            .repository
            .user_orderbook_stats(1, GUILD)
            .expect("zero-payout stats")
            .realized_pnl,
        -position.yes_cost_basis_total
    );
    let (delta, metadata): (i64, String) = fixture
        .connection()
        .query_row(
            "SELECT delta,metadata FROM economy_ledger_entries
             WHERE source='prediction_resolution' AND related_id=?1",
            [prediction_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("zero-payout audit row");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata).expect("valid zero-payout metadata");
    assert_eq!(delta, 0);
    assert_eq!(metadata["payout_multiplier_bps"], 0);

    let rolled_back = fixture
        .rollback(prediction_id)
        .expect("zero payout rollback");
    assert_eq!(rolled_back.total_reversed, 0);
    assert_eq!(fixture.balance(1), balance_before);
    assert_eq!(fixture.status(prediction_id), "open");
}

const FIXTURE_SCHEMA: &str = "
PRAGMA journal_mode=WAL;
CREATE TABLE players (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    discord_username TEXT NOT NULL,
    jopacoin_balance INTEGER DEFAULT 3,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(discord_id,guild_id)
);
CREATE TABLE predictions (
    prediction_id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL DEFAULT 0,
    creator_id INTEGER NOT NULL,
    question TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    outcome TEXT,
    channel_id INTEGER,
    thread_id INTEGER,
    embed_message_id INTEGER,
    channel_message_id INTEGER,
    close_message_id INTEGER,
    resolution_votes TEXT,
    created_at INTEGER NOT NULL,
    closes_at INTEGER NOT NULL,
    resolved_at INTEGER,
    resolved_by INTEGER,
    current_price INTEGER,
    initial_fair INTEGER,
    prev_price INTEGER,
    last_refresh_at INTEGER,
    lp_pnl INTEGER DEFAULT 0
);
CREATE INDEX idx_predictions_guild_status ON predictions(guild_id,status);
CREATE TABLE prediction_levels (
    level_id INTEGER PRIMARY KEY AUTOINCREMENT,
    prediction_id INTEGER NOT NULL,
    side TEXT NOT NULL,
    price INTEGER NOT NULL,
    remaining_size INTEGER NOT NULL,
    posted_at INTEGER NOT NULL
);
CREATE TABLE prediction_positions (
    prediction_id INTEGER NOT NULL,
    discord_id INTEGER NOT NULL,
    yes_contracts INTEGER NOT NULL DEFAULT 0,
    yes_cost_basis_total INTEGER NOT NULL DEFAULT 0,
    no_contracts INTEGER NOT NULL DEFAULT 0,
    no_cost_basis_total INTEGER NOT NULL DEFAULT 0,
    bankruptcy_penalty INTEGER NOT NULL DEFAULT 0,
    vanity_tax INTEGER NOT NULL DEFAULT 0,
    low_priority_tax INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(prediction_id,discord_id)
);
CREATE TABLE prediction_trades (
    trade_id INTEGER PRIMARY KEY AUTOINCREMENT,
    prediction_id INTEGER NOT NULL,
    discord_id INTEGER NOT NULL,
    action TEXT NOT NULL,
    contracts INTEGER NOT NULL,
    jopacoins INTEGER NOT NULL,
    vwap_x100 INTEGER NOT NULL,
    last_fill_price INTEGER,
    trade_time INTEGER NOT NULL
);
CREATE TABLE prediction_fair_snapshots (
    snapshot_id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL,
    snapshot_at INTEGER NOT NULL,
    fair_pct INTEGER NOT NULL,
    reason TEXT NOT NULL
);
CREATE TABLE bankruptcy_state (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    penalty_games_remaining INTEGER DEFAULT 0,
    PRIMARY KEY(discord_id,guild_id)
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
    created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
);
CREATE TABLE economy_ledger_context (
    id INTEGER PRIMARY KEY CHECK(id=1),
    source TEXT,
    actor_id INTEGER,
    related_type TEXT,
    related_id TEXT,
    reason TEXT,
    metadata TEXT
);
CREATE TRIGGER trg_economy_ledger_players_insert
AFTER INSERT ON players
WHEN COALESCE(NEW.jopacoin_balance,0)!=0
BEGIN
    INSERT INTO economy_ledger_entries
        (guild_id,account_type,account_id,delta,balance_before,balance_after,
         source,actor_id,related_type,related_id,reason,metadata)
    VALUES
        (NEW.guild_id,'player',NEW.discord_id,COALESCE(NEW.jopacoin_balance,0),0,
         COALESCE(NEW.jopacoin_balance,0),
         COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'player_insert'),
         (SELECT actor_id FROM economy_ledger_context WHERE id=1),
         (SELECT related_type FROM economy_ledger_context WHERE id=1),
         (SELECT related_id FROM economy_ledger_context WHERE id=1),
         (SELECT reason FROM economy_ledger_context WHERE id=1),
         (SELECT metadata FROM economy_ledger_context WHERE id=1));
END;
CREATE TRIGGER trg_economy_ledger_players_update
AFTER UPDATE OF jopacoin_balance ON players
WHEN COALESCE(OLD.jopacoin_balance,0)!=COALESCE(NEW.jopacoin_balance,0)
BEGIN
    INSERT INTO economy_ledger_entries
        (guild_id,account_type,account_id,delta,balance_before,balance_after,
         source,actor_id,related_type,related_id,reason,metadata)
    VALUES
        (NEW.guild_id,'player',NEW.discord_id,
         COALESCE(NEW.jopacoin_balance,0)-COALESCE(OLD.jopacoin_balance,0),
         COALESCE(OLD.jopacoin_balance,0),COALESCE(NEW.jopacoin_balance,0),
         COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'),
         (SELECT actor_id FROM economy_ledger_context WHERE id=1),
         (SELECT related_type FROM economy_ledger_context WHERE id=1),
         (SELECT related_id FROM economy_ledger_context WHERE id=1),
         (SELECT reason FROM economy_ledger_context WHERE id=1),
         (SELECT metadata FROM economy_ledger_context WHERE id=1));
END;
";

#[test]
fn rollback_succeeds_on_a_market_that_contains_a_sell_trade() {
    // prediction_trades stores sells with negated jopacoins, so the signed
    // trade cash flow is a plain sum. Negating sells a second time inflates the
    // expected gross, the settlement audit then fails, and the admin can never
    // roll the market back.
    let fixture = Fixture::new();
    fixture.player(1, 1_000);
    let prediction_id = fixture.market("sell then resolve?");
    fixture.buy(prediction_id, 1, ContractSide::Yes, 10);
    fixture
        .orderbook
        .sell_contracts_atomic(prediction_id, 1, ContractSide::Yes, 4)
        .expect("sell contracts");
    let before = fixture.balance(1);

    let settled = fixture
        .settle(
            prediction_id,
            ContractSide::Yes,
            &SettlementAdjustments::default(),
        )
        .expect("settle a market containing a sell");
    assert!(settled.winners[0].payout > 0);
    assert_eq!(fixture.balance(1) - before, settled.winners[0].payout);

    let rollback = fixture
        .rollback(prediction_id)
        .expect("a market with a sell trade can still be rolled back");
    assert_eq!(rollback.total_reversed, settled.winners[0].payout);
    assert_eq!(fixture.balance(1), before);
}
