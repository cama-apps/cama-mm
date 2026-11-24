use rusqlite::{Transaction, params};

/// Debit a funded shop purchase and attribute its ledger entry inside the
/// same transaction that delivers the item.
pub fn try_spend(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    cost: i64,
    source: &str,
    reason: &str,
) -> rusqlite::Result<bool> {
    transaction.execute(
        "INSERT OR REPLACE INTO economy_ledger_context(id,source,actor_id,reason)
         VALUES(1,?1,?2,?3)",
        params![source, discord_id, reason],
    )?;
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
             lowest_balance_ever=MIN(COALESCE(lowest_balance_ever,COALESCE(jopacoin_balance,0)-?1),
                                     COALESCE(jopacoin_balance,0)-?1),
             updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3 AND ?1>0 AND COALESCE(jopacoin_balance,0)>=?1",
        params![cost, discord_id, guild_id],
    )?;
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(changed == 1)
}
