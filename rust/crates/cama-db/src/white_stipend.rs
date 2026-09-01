//! Transaction-scoped core of the Plains White bankruptcy stipend.
//!
//! This file is mounted with `#[path]` into both the loan repository
//! (`cama-db-economy`) and the mana repository (`cama-db-gameplay`) so the
//! reserve debit, recipient credit, and economy-ledger-context strings stay
//! identical between the two payment paths; audit queries key on the exact
//! reason strings.

use rusqlite::{Connection, Transaction, params};

/// Which guarded stipend mutation changed no row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WhiteStipendFailure {
    /// The nonprofit reserve no longer covered the debit.
    Reserve,
    /// The recipient was no longer bankrupt.
    Recipient,
}

/// Pay one White stipend inside the caller's transaction: debit the nonprofit
/// reserve, credit the bankrupt recipient, and wrap each mutation in the
/// economy ledger context. The inner `Err` reports a guard rejection so each
/// caller can map it onto its own repository error and roll the transaction
/// back.
pub(crate) fn pay_white_stipend(
    transaction: &Transaction<'_>,
    guild_id: i64,
    discord_id: i64,
    amount: i64,
) -> Result<Result<(), WhiteStipendFailure>, rusqlite::Error> {
    set_stipend_ledger_context(
        transaction,
        Some(discord_id),
        "white mana bankruptcy stipend reserve debit",
        amount,
    )?;
    let reserve_changed = transaction.execute(
        "UPDATE nonprofit_fund
         SET total_collected = total_collected - ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE guild_id = ?2 AND total_collected >= ?1",
        params![amount, guild_id],
    )?;
    clear_stipend_ledger_context(transaction)?;
    if reserve_changed != 1 {
        return Ok(Err(WhiteStipendFailure::Reserve));
    }

    set_stipend_ledger_context(transaction, None, "white mana bankruptcy stipend", amount)?;
    let recipient_changed = transaction.execute(
        "UPDATE players
         SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3
           AND COALESCE(jopacoin_balance, 0) <= 0",
        params![amount, discord_id, guild_id],
    )?;
    clear_stipend_ledger_context(transaction)?;
    if recipient_changed != 1 {
        return Ok(Err(WhiteStipendFailure::Recipient));
    }
    Ok(Ok(()))
}

fn set_stipend_ledger_context(
    connection: &Connection,
    related_id: Option<i64>,
    reason: &str,
    amount: i64,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'mana', NULL, 'bankruptcy_stipend', ?1, ?2, ?3)",
        params![
            related_id.map(|id| id.to_string()),
            reason,
            format!(r#"{{"amount": {amount}, "land": "Plains"}}"#),
        ],
    )?;
    Ok(())
}

fn clear_stipend_ledger_context(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}
