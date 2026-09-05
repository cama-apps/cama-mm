//! Shared bankruptcy, vanity, and low-priority withholding for profit payouts.
//!
//! Every generated-profit surface withholds the same three shares from the
//! same basis: the bankruptcy penalty scales with the games a player still has
//! to win, the vanity tax and low-priority tax each take a flat share, and the
//! combined withholding never exceeds the profit itself (later shares are
//! trimmed to what the earlier ones left). Repositories call
//! [`withhold_profit_deductions`] inside their own `BEGIN IMMEDIATE`
//! transaction after crediting the gross amount so each withheld share lands
//! as its own ledger row; [`compute_profit_deductions`] and
//! [`debit_profit_deductions`] remain available for callers that must split
//! the two steps.

use std::collections::BTreeSet;

use cama_domain::bankruptcy::BankruptcyPenaltyPolicy;
use rusqlite::{Connection, OptionalExtension, params};

/// Highest vanity or low-priority share a deployment may configure.
const MAX_TAX_RATE: f64 = 0.5;

static EMPTY_ROSTER: BTreeSet<i64> = BTreeSet::new();

/// Rates and rosters that decide what is withheld from one profit payout.
#[derive(Clone, Copy, Debug)]
pub struct ProfitDeductionPolicy<'a> {
    /// `None` skips the bankruptcy-state lookup entirely.
    pub bankruptcy: Option<BankruptcyPenaltyPolicy>,
    pub vanity_tax_rate: f64,
    pub vanity_taxable_ids: &'a BTreeSet<i64>,
    pub low_priority_tax_rate: f64,
    pub low_priority_taxable_ids: &'a BTreeSet<i64>,
}

impl ProfitDeductionPolicy<'static> {
    /// A policy that withholds nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            bankruptcy: None,
            vanity_tax_rate: 0.0,
            vanity_taxable_ids: &EMPTY_ROSTER,
            low_priority_tax_rate: 0.0,
            low_priority_taxable_ids: &EMPTY_ROSTER,
        }
    }
}

/// [`ProfitDeductionPolicy`] with owned rosters, for services that resolve
/// the rosters once and hold the policy across calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnedProfitDeductionPolicy {
    pub bankruptcy: Option<BankruptcyPenaltyPolicy>,
    pub vanity_tax_rate: f64,
    pub vanity_taxable_ids: BTreeSet<i64>,
    pub low_priority_tax_rate: f64,
    pub low_priority_taxable_ids: BTreeSet<i64>,
}

impl OwnedProfitDeductionPolicy {
    /// Borrow as the policy the settlement helpers take.
    #[must_use]
    pub const fn as_policy(&self) -> ProfitDeductionPolicy<'_> {
        ProfitDeductionPolicy {
            bankruptcy: self.bankruptcy,
            vanity_tax_rate: self.vanity_tax_rate,
            vanity_taxable_ids: &self.vanity_taxable_ids,
            low_priority_tax_rate: self.low_priority_tax_rate,
            low_priority_taxable_ids: &self.low_priority_taxable_ids,
        }
    }
}

/// The shares withheld from one profit payout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProfitDeductions {
    /// The profit basis every share was computed from.
    pub profit: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
}

impl ProfitDeductions {
    /// Everything withheld together.
    #[must_use]
    pub const fn total(self) -> i64 {
        self.bankruptcy_penalty + self.vanity_tax + self.low_priority_tax
    }

    /// Profit left after every share is withheld.
    #[must_use]
    pub const fn net(self) -> i64 {
        self.profit - self.total()
    }

    /// JSON metadata for ledger rows.
    #[must_use]
    pub fn metadata(self) -> String {
        format!(
            "{{\"profit\":{},\"bankruptcy_penalty\":{},\"vanity_tax\":{},\"low_priority_tax\":{}}}",
            self.profit, self.bankruptcy_penalty, self.vanity_tax, self.low_priority_tax
        )
    }
}

/// Ledger attribution for the withholding rows.
#[derive(Clone, Copy, Debug)]
pub struct DeductionLedgerContext<'a> {
    /// Ledger source of the bankruptcy row, e.g. `duel`. The tax rows use
    /// their fixed `vanity_tax` / `low_priority_tax` sources.
    pub source: &'a str,
    pub related_type: Option<&'a str>,
    pub related_id: Option<i64>,
    /// Reason prefix; the bankruptcy row reads `"<reason> bankruptcy penalty"`.
    pub reason: &'a str,
}

fn clamped_rate(rate: f64, maximum: f64) -> f64 {
    if rate.is_nan() {
        0.0
    } else {
        rate.clamp(0.0, maximum)
    }
}

fn truncated(amount: i64, rate: f64) -> i64 {
    (amount as f64 * rate) as i64
}

/// Read the outstanding penalty games for one player, `0` without a row.
pub fn penalty_games_remaining(
    connection: &Connection,
    guild_id: i64,
    discord_id: i64,
) -> rusqlite::Result<i64> {
    Ok(connection
        .query_row(
            "SELECT COALESCE(penalty_games_remaining,0) FROM bankruptcy_state
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Compute every share withheld from `profit` without touching balances.
///
/// A non-positive profit withholds nothing. The bankruptcy share scales with
/// the player's outstanding penalty games; the vanity and low-priority shares
/// read the same profit basis. Each later share is trimmed to what the earlier
/// ones left, so the three together never exceed the profit.
pub fn compute_profit_deductions(
    connection: &Connection,
    guild_id: i64,
    discord_id: i64,
    profit: i64,
    policy: &ProfitDeductionPolicy<'_>,
) -> rusqlite::Result<ProfitDeductions> {
    if profit <= 0 {
        return Ok(ProfitDeductions {
            profit: profit.max(0),
            ..ProfitDeductions::default()
        });
    }
    let bankruptcy_penalty = match policy.bankruptcy {
        Some(bankruptcy) => {
            let remaining = penalty_games_remaining(connection, guild_id, discord_id)?;
            truncated(profit, bankruptcy.withheld_rate(remaining))
        }
        None => 0,
    };
    let vanity_tax = if policy.vanity_taxable_ids.contains(&discord_id) {
        truncated(profit, clamped_rate(policy.vanity_tax_rate, MAX_TAX_RATE))
            .min(profit.saturating_sub(bankruptcy_penalty).max(0))
    } else {
        0
    };
    let low_priority_tax = if policy.low_priority_taxable_ids.contains(&discord_id) {
        truncated(
            profit,
            clamped_rate(policy.low_priority_tax_rate, MAX_TAX_RATE),
        )
        .min(
            profit
                .saturating_sub(bankruptcy_penalty)
                .saturating_sub(vanity_tax)
                .max(0),
        )
    } else {
        0
    };
    Ok(ProfitDeductions {
        profit,
        bankruptcy_penalty,
        vanity_tax,
        low_priority_tax,
    })
}

/// Debit each non-zero share as its own attributed balance movement.
///
/// Returns `false` when the player row is missing so callers can fail their
/// transaction the same way their credit path does.
pub fn debit_profit_deductions(
    connection: &Connection,
    guild_id: i64,
    discord_id: i64,
    deductions: ProfitDeductions,
    ledger: &DeductionLedgerContext<'_>,
) -> rusqlite::Result<bool> {
    let metadata = deductions.metadata();
    let bankruptcy_reason = format!("{} bankruptcy penalty", ledger.reason);
    let rows = [
        (
            deductions.bankruptcy_penalty,
            ledger.source,
            bankruptcy_reason.as_str(),
        ),
        (
            deductions.vanity_tax,
            "vanity_tax",
            "vanity tax on JC profit",
        ),
        (
            deductions.low_priority_tax,
            "low_priority_tax",
            "low priority tax on JC profit",
        ),
    ];
    for (amount, source, reason) in rows {
        if amount <= 0 {
            continue;
        }
        connection.execute("DELETE FROM economy_ledger_context", [])?;
        connection.execute(
            "INSERT INTO economy_ledger_context (
                 id,source,actor_id,related_type,related_id,reason,metadata
             ) VALUES (1,?1,?2,?3,?4,?5,?6)",
            params![
                source,
                discord_id,
                ledger.related_type,
                ledger.related_id.map(|value| value.to_string()),
                reason,
                metadata,
            ],
        )?;
        let changed = connection.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![amount, discord_id, guild_id],
        );
        let cleared = connection.execute("DELETE FROM economy_ledger_context", []);
        let changed = changed?;
        cleared?;
        if changed != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Compute and debit every share in one step.
///
/// Skips the balance writes entirely when nothing is withheld. Returns `None`
/// when the player row is missing so callers can fail their transaction the
/// same way their credit path does.
pub fn withhold_profit_deductions(
    connection: &Connection,
    guild_id: i64,
    discord_id: i64,
    profit: i64,
    policy: &ProfitDeductionPolicy<'_>,
    ledger: &DeductionLedgerContext<'_>,
) -> rusqlite::Result<Option<ProfitDeductions>> {
    let deductions = compute_profit_deductions(connection, guild_id, discord_id, profit, policy)?;
    if deductions.total() > 0
        && !debit_profit_deductions(connection, guild_id, discord_id, deductions, ledger)?
    {
        return Ok(None);
    }
    Ok(Some(deductions))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUILD: i64 = 77;
    const PLAYER: i64 = 4_001;

    fn database() -> Connection {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     jopacoin_balance INTEGER,
                     updated_at TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE bankruptcy_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     last_bankruptcy_at INTEGER,
                     penalty_games_remaining INTEGER,
                     bankruptcy_count INTEGER,
                     updated_at TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TABLE ledger_log (
                     source TEXT,
                     reason TEXT,
                     delta INTEGER
                 );
                 CREATE TRIGGER log_balance AFTER UPDATE OF jopacoin_balance ON players
                 BEGIN
                     INSERT INTO ledger_log(source,reason,delta)
                     SELECT source,reason,NEW.jopacoin_balance-OLD.jopacoin_balance
                       FROM economy_ledger_context WHERE id=1;
                 END;
                 INSERT INTO players(discord_id,guild_id,jopacoin_balance)
                 VALUES (4001,77,1000);",
            )
            .expect("schema");
        connection
    }

    fn penalty(connection: &Connection, remaining: i64) {
        connection
            .execute(
                "INSERT INTO bankruptcy_state(discord_id,guild_id,penalty_games_remaining,
                     bankruptcy_count) VALUES (?1,?2,?3,1)",
                params![PLAYER, GUILD, remaining],
            )
            .expect("penalty row");
    }

    fn policy<'a>(
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

    #[test]
    fn bankruptcy_share_scales_with_games_remaining() {
        let connection = database();
        penalty(&connection, 1);
        let empty = BTreeSet::new();
        // 5% per outstanding game: 1 game withholds 6 of 120, 3 games 18,
        // 6 games 36, and 20 games cap at the whole profit.
        let deductions =
            compute_profit_deductions(&connection, GUILD, PLAYER, 120, &policy(&empty, &empty))
                .expect("compute");
        assert_eq!(deductions.bankruptcy_penalty, 6);
        assert_eq!(deductions.net(), 114);

        for (remaining, expected) in [(3, 18), (6, 36), (20, 120)] {
            connection
                .execute(
                    "UPDATE bankruptcy_state SET penalty_games_remaining=?1",
                    [remaining],
                )
                .expect("extend");
            let deductions =
                compute_profit_deductions(&connection, GUILD, PLAYER, 120, &policy(&empty, &empty))
                    .expect("compute extended");
            assert_eq!(deductions.bankruptcy_penalty, expected, "{remaining} games");
        }
    }

    #[test]
    fn taxes_read_the_same_basis_and_never_exceed_the_profit() {
        let connection = database();
        // 20 games at 5% each withhold the whole profit.
        penalty(&connection, 20);
        let roster = BTreeSet::from([PLAYER]);
        let deductions =
            compute_profit_deductions(&connection, GUILD, PLAYER, 100, &policy(&roster, &roster))
                .expect("compute");
        assert_eq!(
            deductions,
            ProfitDeductions {
                profit: 100,
                bankruptcy_penalty: 100,
                vanity_tax: 0,
                low_priority_tax: 0,
            }
        );
        assert_eq!(deductions.total(), 100);
        assert_eq!(deductions.net(), 0);

        // A partial penalty (19 games, 95%) leaves room for part of the
        // vanity share only.
        connection
            .execute("UPDATE bankruptcy_state SET penalty_games_remaining=19", [])
            .expect("shrink penalty");
        let deductions =
            compute_profit_deductions(&connection, GUILD, PLAYER, 100, &policy(&roster, &roster))
                .expect("compute partial");
        assert_eq!(
            deductions,
            ProfitDeductions {
                profit: 100,
                bankruptcy_penalty: 95,
                vanity_tax: 5,
                low_priority_tax: 0,
            }
        );
    }

    #[test]
    fn withhold_combines_compute_and_debit_and_skips_zero_debits() {
        let connection = database();
        let roster = BTreeSet::from([PLAYER, PLAYER + 1]);
        let ledger = DeductionLedgerContext {
            source: "duel",
            related_type: None,
            related_id: None,
            reason: "duel winnings",
        };
        let untaxed = withhold_profit_deductions(
            &connection,
            GUILD,
            PLAYER,
            100,
            &ProfitDeductionPolicy::none(),
            &ledger,
        )
        .expect("withhold nothing");
        assert_eq!(
            untaxed,
            Some(ProfitDeductions {
                profit: 100,
                ..ProfitDeductions::default()
            })
        );
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM ledger_log", [], |row| row.get(0))
            .expect("row count");
        assert_eq!(rows, 0);

        // Five games at 5% each withhold 25 of the 100 JC profit.
        penalty(&connection, 5);
        let taxed = withhold_profit_deductions(
            &connection,
            GUILD,
            PLAYER,
            100,
            &policy(&roster, &BTreeSet::new()),
            &ledger,
        )
        .expect("withhold");
        assert_eq!(
            taxed,
            Some(ProfitDeductions {
                profit: 100,
                bankruptcy_penalty: 25,
                vanity_tax: 10,
                low_priority_tax: 0,
            })
        );
        let balance: i64 = connection
            .query_row("SELECT jopacoin_balance FROM players", [], |row| row.get(0))
            .expect("balance");
        assert_eq!(balance, 1000 - 35);

        let missing = withhold_profit_deductions(
            &connection,
            GUILD,
            PLAYER + 1,
            100,
            &policy(&roster, &BTreeSet::new()),
            &ledger,
        )
        .expect("withhold missing");
        assert_eq!(missing, None);
    }

    #[test]
    fn untaxed_player_and_non_positive_profit_withhold_nothing() {
        let connection = database();
        let empty = BTreeSet::new();
        let none =
            compute_profit_deductions(&connection, GUILD, PLAYER, 100, &policy(&empty, &empty))
                .expect("compute");
        assert_eq!(
            none,
            ProfitDeductions {
                profit: 100,
                ..ProfitDeductions::default()
            }
        );
        let zero =
            compute_profit_deductions(&connection, GUILD, PLAYER, -5, &policy(&empty, &empty))
                .expect("compute negative");
        assert_eq!(zero, ProfitDeductions::default());
        let skipped = compute_profit_deductions(
            &connection,
            GUILD,
            PLAYER,
            100,
            &ProfitDeductionPolicy::none(),
        )
        .expect("compute none");
        assert_eq!(skipped.total(), 0);
    }

    #[test]
    fn debit_writes_one_attributed_row_per_share() {
        let connection = database();
        // Five games at 5% each withhold 25 of the 100 JC profit.
        penalty(&connection, 5);
        let roster = BTreeSet::from([PLAYER]);
        let deductions =
            compute_profit_deductions(&connection, GUILD, PLAYER, 100, &policy(&roster, &roster))
                .expect("compute");
        assert_eq!(
            deductions,
            ProfitDeductions {
                profit: 100,
                bankruptcy_penalty: 25,
                vanity_tax: 10,
                low_priority_tax: 25
            }
        );
        let applied = debit_profit_deductions(
            &connection,
            GUILD,
            PLAYER,
            deductions,
            &DeductionLedgerContext {
                source: "duel",
                related_type: Some("duel"),
                related_id: Some(9),
                reason: "duel winnings",
            },
        )
        .expect("debit");
        assert!(applied);
        let balance: i64 = connection
            .query_row("SELECT jopacoin_balance FROM players", [], |row| row.get(0))
            .expect("balance");
        assert_eq!(balance, 1000 - 60);
        let mut statement = connection
            .prepare("SELECT source,reason,delta FROM ledger_log ORDER BY rowid")
            .expect("prepare");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                (
                    "duel".to_owned(),
                    "duel winnings bankruptcy penalty".to_owned(),
                    -25
                ),
                (
                    "vanity_tax".to_owned(),
                    "vanity tax on JC profit".to_owned(),
                    -10
                ),
                (
                    "low_priority_tax".to_owned(),
                    "low priority tax on JC profit".to_owned(),
                    -25
                ),
            ]
        );
        let leftover: i64 = connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get(0)
            })
            .expect("context count");
        assert_eq!(leftover, 0);
    }

    #[test]
    fn debit_reports_a_missing_player() {
        let connection = database();
        let applied = debit_profit_deductions(
            &connection,
            GUILD,
            PLAYER + 1,
            ProfitDeductions {
                profit: 10,
                bankruptcy_penalty: 1,
                ..ProfitDeductions::default()
            },
            &DeductionLedgerContext {
                source: "duel",
                related_type: None,
                related_id: None,
                reason: "duel winnings",
            },
        )
        .expect("debit");
        assert!(!applied);
    }
}
