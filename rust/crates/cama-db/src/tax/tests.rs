use crate::test_support::copy_migrated_database;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42;
const NOW: i64 = 1_800_000_000;

struct Fixture {
    file: NamedTempFile,
    repository: TaxRepository,
}

impl Fixture {
    fn migrated() -> Self {
        let file = NamedTempFile::new().expect("temporary tax database");
        copy_migrated_database(file.path()).expect("migrate tax database");
        Self {
            repository: TaxRepository::new(file.path(), 10, 50),
            file,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.file.path()).expect("open tax database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("production foreign-key mode");
        connection
    }

    fn player(&self, id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever
                 ) VALUES (?1,?2,?3,?4,?4)",
                params![id, GUILD, format!("Player {id}"), balance],
            )
            .expect("insert tax player");
    }

    fn clear_ledger(&self) {
        self.connection()
            .execute_batch(
                "DELETE FROM economy_ledger_entries; DELETE FROM economy_ledger_context;",
            )
            .expect("clear fixture ledger");
    }

    fn fine(&self, id: i64, amount: i64, now: i64) -> TaxFineOutcome {
        self.fine_with_cap(id, amount, i64::MAX, now)
    }

    fn fine_with_cap(&self, id: i64, amount: i64, max_amount: i64, now: i64) -> TaxFineOutcome {
        self.repository
            .levy_fine_atomic(&TaxFineRequest {
                discord_id: id,
                guild_id: GUILD,
                amount,
                max_amount,
                actor_id: 99,
                reason: Some("  overdue   obligations ".to_owned()),
                cooldown_seconds: 100,
                now,
            })
            .expect("levy fixture fine")
    }
}

#[test]
fn test_recent_ledger_supports_offset_and_count() {
    let fixture = Fixture::migrated();
    fixture.player(1, 0);
    fixture.clear_ledger();
    let connection = fixture.connection();
    for index in 1..=5_i64 {
        connection
            .execute(
                "INSERT INTO economy_ledger_entries(
                     guild_id,account_type,account_id,delta,balance_before,
                     balance_after,source,created_at
                 ) VALUES (?1,'player',1,?2,0,?2,'fixture',?2)",
                params![GUILD, index],
            )
            .expect("insert ledger entry");
    }
    assert_eq!(
        fixture
            .repository
            .count_ledger_entries(GUILD, Some(1))
            .expect("count ledger"),
        5
    );
    let rows = fixture
        .repository
        .recent_ledger(GUILD, 2, 2, Some(1))
        .expect("paged ledger");
    assert_eq!(rows.iter().map(|row| row.delta).collect::<Vec<_>>(), [3, 2]);
}

#[test]
fn test_player_snapshot_includes_loans_and_dark_bargains() {
    let fixture = Fixture::migrated();
    fixture.player(1, -20);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO loan_state(
                 discord_id,guild_id,total_loans_taken,total_fees_paid,
                 outstanding_principal,outstanding_fee
             ) VALUES (1,?1,2,7,30,5)",
            [GUILD],
        )
        .expect("seed loan");
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data
             ) VALUES (1,?1,'dark_bargain',?2,?3,0,'{\"amount_due\":12}')",
            params![GUILD, NOW - 10, NOW + 10],
        )
        .expect("seed dark bargain");
    connection
        .execute(
            "INSERT INTO predictions(
                 prediction_id,guild_id,creator_id,question,status,created_at,closes_at,
                 current_price,initial_fair
             ) VALUES (10,?1,1,'Will it happen?','open',?2,?3,50,50)",
            params![GUILD, NOW - 10, NOW + 10],
        )
        .expect("seed prediction");
    connection
        .execute(
            "INSERT INTO prediction_positions(
                 prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                 no_contracts,no_cost_basis_total
             ) VALUES (10,1,2,8,1,3)",
            [],
        )
        .expect("seed position");
    let snapshot = fixture
        .repository
        .player_snapshot(1, GUILD, 8, 0, NOW)
        .expect("player snapshot")
        .expect("registered player");
    assert_eq!(snapshot.visible_debt, 20);
    assert_eq!(snapshot.loan_total, 35);
    assert_eq!(snapshot.total_loans_taken, 2);
    assert_eq!(snapshot.total_fees_paid, 7);
    assert_eq!(snapshot.dark_bargain_count, 1);
    assert_eq!(snapshot.dark_bargain_due, 12);
    assert_eq!(snapshot.prediction_cost_basis, 11);
    assert_eq!(snapshot.effective_obligations, 78);
}

#[test]
fn test_levy_fine_caps_at_obligations_and_can_make_balance_negative() {
    let fixture = Fixture::migrated();
    fixture.player(1, -40);
    fixture.clear_ledger();
    assert_eq!(
        fixture.fine_with_cap(1, 100, 100, NOW),
        TaxFineOutcome::Ok {
            requested_amount: 100,
            applied_amount: 40,
            outstanding_obligations: 40,
            balance_before: -40,
            balance_after: -80,
            next_fine_at: NOW + 100,
        }
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=1 AND guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        -80
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("reserve"),
        40
    );
    let rows = fixture
        .repository
        .recent_ledger(GUILD, 10, 0, None)
        .expect("fine ledger");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.source == "tax_fine"));
    assert_eq!(
        rows.iter()
            .find(|row| row.account_type == "player")
            .and_then(|row| row.metadata.as_deref()),
        Some(
            "{\"requested_amount\": 100, \"applied_amount\": 40, \"outstanding_obligations\": 40, \"cooldown_seconds\": 100}"
        )
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context count"),
        0
    );
}

#[test]
fn test_levy_fine_enforces_per_player_cooldown_without_new_ledger() {
    let fixture = Fixture::migrated();
    fixture.player(1, -40);
    fixture.clear_ledger();
    assert!(matches!(
        fixture.fine(1, 10, NOW),
        TaxFineOutcome::Ok { .. }
    ));
    let before = fixture
        .repository
        .count_ledger_entries(GUILD, None)
        .expect("ledger before cooldown");
    assert_eq!(
        fixture.fine(1, 10, NOW + 50),
        TaxFineOutcome::Cooldown {
            next_fine_at: NOW + 100
        }
    );
    assert_eq!(
        fixture
            .repository
            .count_ledger_entries(GUILD, None)
            .expect("ledger after cooldown"),
        before
    );
}

#[test]
fn test_reset_fine_cooldown_allows_refine_inside_original_window() {
    let fixture = Fixture::migrated();
    fixture.player(1, -40);
    fixture.clear_ledger();
    assert!(matches!(
        fixture.fine(1, 10, NOW),
        TaxFineOutcome::Ok { .. }
    ));
    assert_eq!(
        fixture
            .repository
            .reset_fine_cooldown_atomic(1, GUILD)
            .expect("reset cooldown"),
        Some(true)
    );
    assert!(matches!(
        fixture.fine(1, 10, NOW + 50),
        TaxFineOutcome::Ok { .. }
    ));
}

#[test]
fn test_reset_fine_cooldown_reports_missing_row_or_target() {
    let fixture = Fixture::migrated();
    fixture.player(1, -10);
    assert_eq!(
        fixture
            .repository
            .reset_fine_cooldown_atomic(1, GUILD)
            .expect("clear absent cooldown"),
        Some(false)
    );
    assert_eq!(
        fixture
            .repository
            .reset_fine_cooldown_atomic(999, GUILD)
            .expect("missing target"),
        None
    );
}

#[test]
fn test_levy_fine_allows_different_players_during_cooldown() {
    let fixture = Fixture::migrated();
    fixture.player(1, -10);
    fixture.player(2, -10);
    assert!(matches!(fixture.fine(1, 5, NOW), TaxFineOutcome::Ok { .. }));
    assert!(matches!(fixture.fine(2, 5, NOW), TaxFineOutcome::Ok { .. }));
}

#[test]
fn test_levy_fine_rejects_player_without_outstanding_obligations() {
    let fixture = Fixture::migrated();
    fixture.player(1, 20);
    assert_eq!(
        fixture.fine(1, 10, NOW),
        TaxFineOutcome::NoOutstandingObligations
    );
    assert_eq!(
        fixture.fine(999, 10, NOW),
        TaxFineOutcome::TargetNotRegistered
    );
    assert_eq!(fixture.fine(1, 0, NOW), TaxFineOutcome::InvalidAmount);
}

#[test]
fn test_levy_fine_atomic_reclamps_stale_cap_in_lock() {
    let fixture = Fixture::migrated();
    fixture.player(1, 20);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO loan_state(discord_id,guild_id,outstanding_principal,outstanding_fee)
             VALUES (1,?1,100,0)",
            [GUILD],
        )
        .expect("seed original obligation");
    connection
        .execute(
            "UPDATE loan_state SET outstanding_principal=10 WHERE discord_id=1 AND guild_id=?1",
            [GUILD],
        )
        .expect("reduce obligation before lock");
    assert!(matches!(
        fixture.fine_with_cap(1, 100, 100, NOW),
        TaxFineOutcome::Ok {
            applied_amount: 10,
            outstanding_obligations: 10,
            ..
        }
    ));
}

#[test]
fn test_levy_fine_never_exceeds_caller_audited_cap() {
    let fixture = Fixture::migrated();
    fixture.player(1, -40);
    assert!(matches!(
        fixture.fine_with_cap(1, 100, 15, NOW),
        TaxFineOutcome::Ok {
            applied_amount: 15,
            outstanding_obligations: 15,
            ..
        }
    ));
}

#[test]
fn test_levy_fine_atomic_rejects_when_obligations_vanish_in_lock() {
    let fixture = Fixture::migrated();
    fixture.player(1, 20);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO loan_state(discord_id,guild_id,outstanding_principal,outstanding_fee)
             VALUES (1,?1,100,0)",
            [GUILD],
        )
        .expect("seed original obligation");
    connection
        .execute(
            "UPDATE loan_state SET outstanding_principal=0 WHERE discord_id=1 AND guild_id=?1",
            [GUILD],
        )
        .expect("clear obligation before lock");
    assert_eq!(
        fixture.fine_with_cap(1, 100, 100, NOW),
        TaxFineOutcome::NoOutstandingObligations
    );
}

#[test]
fn test_prediction_market_exposure_includes_cost_basis_ev_and_liability() {
    let fixture = Fixture::migrated();
    fixture.player(1, 10);
    fixture.player(2, 10);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO predictions(
                 prediction_id,guild_id,creator_id,question,status,created_at,closes_at,
                 current_price,initial_fair,lp_pnl
             ) VALUES (10,?1,1,'Market?','open',?2,?3,60,50,4)",
            params![GUILD, NOW - 10, NOW + 10],
        )
        .expect("seed market");
    connection
        .execute(
            "INSERT INTO prediction_positions(
                 prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                 no_contracts,no_cost_basis_total
             ) VALUES (10,1,2,9,1,3),(10,2,1,5,2,8)",
            [],
        )
        .expect("seed positions");
    connection
        .execute(
            "INSERT INTO prediction_levels(prediction_id,side,price,remaining_size,posted_at)
             VALUES (10,'yes_bid',59,5,?1),(10,'yes_ask',61,7,?1)",
            [NOW],
        )
        .expect("seed levels");
    let snapshot = fixture
        .repository
        .guild_snapshot(GUILD, NOW)
        .expect("guild snapshot");
    let summary = snapshot.prediction_exposure.summary;
    assert_eq!(summary.open_markets, 1);
    assert_eq!(summary.holder_count, 2);
    assert_eq!(summary.cost_basis, 25);
    assert_eq!(summary.expected_payout, 30);
    assert_eq!(summary.ev_to_holders, 5);
    assert_eq!(summary.yes_liability, 30);
    assert_eq!(summary.no_liability, 30);
    assert_eq!(summary.worst_case_payout, 30);
    assert_eq!(summary.book_contracts, 12);
}

#[test]
fn test_player_prediction_expected_payout_floors_each_side_like_python() {
    let fixture = Fixture::migrated();
    fixture.player(1, 10);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO predictions(
                 prediction_id,guild_id,creator_id,question,status,created_at,closes_at,
                 current_price,initial_fair
             ) VALUES (10,?1,1,'Rounding market?','open',?2,?3,55,50)",
            params![GUILD, NOW - 10, NOW + 10],
        )
        .expect("seed rounding market");
    connection
        .execute(
            "INSERT INTO prediction_positions(
                 prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                 no_contracts,no_cost_basis_total
             ) VALUES (10,1,1,5,1,4)",
            [],
        )
        .expect("seed rounding position");
    let snapshot = fixture
        .repository
        .player_snapshot(1, GUILD, 8, 0, NOW)
        .expect("player snapshot")
        .expect("registered player");
    assert_eq!(snapshot.prediction_exposure.summary.expected_payout, 9);
    assert_eq!(snapshot.prediction_exposure.summary.ev, 0);
}

#[test]
fn test_levy_fine_can_use_prediction_cost_basis_as_obligation() {
    let fixture = Fixture::migrated();
    fixture.player(1, 20);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO predictions(
                 prediction_id,guild_id,creator_id,question,status,created_at,closes_at
             ) VALUES (10,?1,1,'Market?','open',?2,?3)",
            params![GUILD, NOW - 10, NOW + 10],
        )
        .expect("seed market");
    connection
        .execute(
            "INSERT INTO prediction_positions(
                 prediction_id,discord_id,yes_contracts,yes_cost_basis_total
             ) VALUES (10,1,2,17)",
            [],
        )
        .expect("seed prediction obligation");
    assert!(matches!(
        fixture.fine(1, 99, NOW),
        TaxFineOutcome::Ok {
            applied_amount: 17,
            ..
        }
    ));
}

#[test]
fn test_tax_man_can_add_and_remove_bankruptcy_modifier() {
    let fixture = Fixture::migrated();
    fixture.player(1, 10);
    assert_eq!(
        fixture
            .repository
            .change_bankruptcy_modifier_atomic(TaxBankruptcyRequest {
                discord_id: 1,
                guild_id: GUILD,
                action: TaxBankruptcyAction::Add { games: 3 },
            })
            .expect("add modifier"),
        TaxBankruptcyOutcome::Added {
            games: 3,
            previous_games: 0,
            penalty_games_remaining: 3,
        }
    );
    assert_eq!(
        fixture
            .repository
            .change_bankruptcy_modifier_atomic(TaxBankruptcyRequest {
                discord_id: 1,
                guild_id: GUILD,
                action: TaxBankruptcyAction::Remove,
            })
            .expect("remove modifier"),
        TaxBankruptcyOutcome::Removed {
            previous_games: 3,
            penalty_games_remaining: 0,
        }
    );
}

#[test]
fn test_tax_man_bankruptcy_modifier_rejects_invalid_or_unknown_target() {
    let fixture = Fixture::migrated();
    fixture.player(1, 10);
    assert_eq!(
        fixture
            .repository
            .change_bankruptcy_modifier_atomic(TaxBankruptcyRequest {
                discord_id: 1,
                guild_id: GUILD,
                action: TaxBankruptcyAction::Add { games: 0 },
            })
            .expect("invalid games"),
        TaxBankruptcyOutcome::InvalidGames
    );
    assert_eq!(
        fixture
            .repository
            .change_bankruptcy_modifier_atomic(TaxBankruptcyRequest {
                discord_id: 999,
                guild_id: GUILD,
                action: TaxBankruptcyAction::Remove,
            })
            .expect("missing player"),
        TaxBankruptcyOutcome::TargetNotRegistered
    );
}
