//! Shared assertions for the gamba-channel penance every gamba command charges.

use std::path::Path;

/// The ledger rows every gamba-channel penance must leave: one coin debited
/// from the player into the reserve under one shared attribution.
pub(crate) fn gamba_channel_penance_ledger(path: &Path, guild_id: i64) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).expect("open penance ledger");
    let mut statement = connection
        .prepare(
            "SELECT account_type||':'||source||':'||COALESCE(related_type,'')||':'||delta
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND related_type='wrong_channel_penalty'
             ORDER BY ledger_id",
        )
        .expect("prepare penance ledger");
    statement
        .query_map([guild_id], |row| row.get(0))
        .expect("query penance ledger")
        .collect::<Result<_, _>>()
        .expect("read penance ledger")
}

/// What [`gamba_channel_penance_ledger`] returns for one charged penance.
pub(crate) const GAMBA_CHANNEL_PENANCE_LEDGER: [&str; 2] = [
    "player:gamba:wrong_channel_penalty:-1",
    "nonprofit:gamba:wrong_channel_penalty:1",
];
