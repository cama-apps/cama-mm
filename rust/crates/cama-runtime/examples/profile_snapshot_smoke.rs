//! Read-only profile/Info adapter smoke for an explicitly disposable DB copy.
//!
//! The caller supplies a checkpointed copy of the current production SQLite
//! database.  This binary performs only Rust reads after the copy is handed
//! to it: no Python helper, schema migration, Discord transport, or write is
//! involved.  The confirmation flag is deliberately required because the
//! smoke is intended for cutover evidence, not for an unattended live DB.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use cama_db::audit_database;
use cama_runtime::{read_balance_history_snapshot, read_info_analytics_snapshot};
use rusqlite::{Connection, OpenFlags, params};

const CONFIRMATION: &str = "--disposable-copy";
const MAX_ROWS: usize = 100;

const DIRECT_READ_TABLES: &[&str] = &[
    "players",
    "matches",
    "match_participants",
    "bets",
    "prediction_positions",
    "predictions",
    "economy_ledger_entries",
    "wheel_spins",
    "double_or_nothing_spins",
    "tip_transactions",
    "disburse_history",
    "dig_actions",
    "rating_history",
    "trivia_sessions",
];

fn main() -> Result<(), Box<dyn Error>> {
    run(env::args_os().skip(1))
}

fn run<I>(mut arguments: I) -> Result<(), Box<dyn Error>>
where
    I: Iterator<Item = OsString>,
{
    let path = arguments.next().map(PathBuf::from).ok_or(
        "usage: profile_snapshot_smoke <db-path> <discord-id> <guild-id> --disposable-copy",
    )?;
    let discord_id = parse_id(arguments.next(), "discord-id")?;
    let guild_id = parse_id(arguments.next(), "guild-id")?;
    if arguments
        .next()
        .and_then(|argument| argument.into_string().ok())
        .as_deref()
        != Some(CONFIRMATION)
        || arguments.next().is_some()
    {
        return Err("refusing profile read smoke without exactly --disposable-copy".into());
    }

    let audit = audit_database(&path)?;
    if !audit.is_compatible() {
        return Err(format!("snapshot is incompatible: {}", audit.issues().join("; ")).into());
    }
    assert_direct_read_tables(&path)?;

    let profile = read_balance_history_snapshot(&path, discord_id, guild_id)
        .map_err(|error| format!("profile balance-history read failed: {error}"))?;
    let info = read_info_analytics_snapshot(&path, guild_id, MAX_ROWS)
        .map_err(|error| format!("Info/profile analytics read failed: {error}"))?;
    if profile.event_count == 0 {
        return Err("profile balance-history read returned no seeded events".into());
    }
    if info.player_count == 0 {
        return Err("Info analytics read returned no guild players".into());
    }

    println!(
        "profile_snapshot_smoke=ok profile_events={} profile_sources={} info_players={} info_balance={} info_glicko={} info_openskill={} info_gambling={} info_tip_senders={} info_tip_receivers={} info_trivia={}",
        profile.event_count,
        profile.source_event_counts.len(),
        info.player_count,
        info.balance_candidates,
        info.glicko_candidates,
        info.openskill_candidates,
        info.gambling_entries,
        info.tip_sender_entries,
        info.tip_receiver_entries,
        info.trivia_entries,
    );
    Ok(())
}

fn parse_id(value: Option<OsString>, name: &str) -> Result<i64, Box<dyn Error>> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let value = value
        .into_string()
        .map_err(|_| format!("{name} must be UTF-8"))?;
    value
        .parse::<i64>()
        .map_err(|error| format!("invalid {name} {value:?}: {error}").into())
}

fn assert_direct_read_tables(path: &Path) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    let mut statement = connection.prepare(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
    )?;
    let missing = DIRECT_READ_TABLES
        .iter()
        .filter_map(|table| {
            let exists = statement
                .query_row(params![table], |row| row.get::<_, i64>(0))
                .ok()?;
            (exists == 0).then_some(*table)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "snapshot is missing direct-read tables: {}",
            missing.join(", ")
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cama_db::schema_manager::initialize_or_migrate;
    use tempfile::tempdir;

    #[test]
    fn smoke_requires_the_disposable_copy_confirmation() {
        let error = run([
            OsString::from("/tmp/cama-profile.db"),
            OsString::from("1"),
            OsString::from("2"),
        ]
        .into_iter())
        .expect_err("live-read smoke must require explicit disposable-copy confirmation");
        assert!(error.to_string().contains("--disposable-copy"));
    }

    #[test]
    fn smoke_reads_a_rust_migrated_current_schema_without_python() {
        let directory = tempdir().expect("create disposable profile snapshot");
        let path = directory.path().join("profile.db");
        initialize_or_migrate(&path).expect("migrate disposable current schema");

        let error = run([
            path.into_os_string(),
            OsString::from("100"),
            OsString::from("42"),
            OsString::from(CONFIRMATION),
        ]
        .into_iter())
        .expect_err("empty profile history must fail closed");
        assert!(error.to_string().contains("no seeded events"));
    }
}
