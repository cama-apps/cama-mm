//! One-way referral settlement smoke for a disposable Python-era DB clone.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;

use cama_db::audit_database;
use cama_db::core_repositories::{CoreMatchRecord, MatchRecord, MatchRepository};
use cama_db::referrals::ReferralRepository;
use rusqlite::{Connection, params};
use serde_json::Value;

const CONFIRMATION: &str = "--disposable-copy";
const REFERRAL_GUILD_ID: i64 = -8_888_888_888_888_867;
const REFERRAL_OTHER_GUILD_ID: i64 = -8_888_888_888_888_863;
const REFERRER_ID: i64 = -8_888_888_888_888_866;
const REFERRED_ID: i64 = -8_888_888_888_888_865;
const REFERRAL_OPPONENT_ID: i64 = -8_888_888_888_888_864;
const REFERRAL_PENDING_MATCH_ID: i64 = 8_888_888_888_888_001;
const REFERRAL_REWARDED_AT: i64 = 1_800_000_000;

fn seed_players(path: &PathBuf) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    for guild_id in [REFERRAL_GUILD_ID, REFERRAL_OTHER_GUILD_ID] {
        for (discord_id, username, balance) in [
            (REFERRER_ID, "Rust snapshot referrer", 10_i64),
            (REFERRED_ID, "Rust snapshot referred", 20_i64),
            (REFERRAL_OPPONENT_ID, "Rust snapshot opponent", 30_i64),
        ] {
            if connection.query_row(
                "SELECT COUNT(*) FROM players WHERE guild_id = ?1 AND discord_id = ?2",
                params![guild_id, discord_id],
                |row| row.get::<_, i64>(0),
            )? != 0
            {
                return Err("referral sentinel player already exists".into());
            }
            connection.execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance,
                     wins, losses, glicko_rating, glicko_rd, glicko_volatility
                 ) VALUES (?1, ?2, ?3, ?4, 0, 0, NULL, NULL, NULL)",
                params![discord_id, guild_id, username, balance],
            )?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    run(env::args_os().skip(1))
}

fn run<I>(mut arguments: I) -> Result<(), Box<dyn Error>>
where
    I: Iterator<Item = OsString>,
{
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: referral_snapshot_smoke <db-path> --disposable-copy")?;
    if arguments
        .next()
        .and_then(|argument| argument.into_string().ok())
        .as_deref()
        != Some(CONFIRMATION)
        || arguments.next().is_some()
    {
        return Err("refusing referral write smoke without exactly --disposable-copy".into());
    }

    let audit = audit_database(&path)?;
    if !audit.is_compatible() {
        return Err(format!("snapshot is incompatible: {}", audit.issues().join("; ")).into());
    }
    seed_players(&path)?;

    ReferralRepository::new(&path).create_referral(
        REFERRER_ID,
        REFERRED_ID,
        Some(REFERRAL_GUILD_ID),
    )?;
    let matches = MatchRepository::new(&path);
    let mut core = CoreMatchRecord::new(
        MatchRecord::new(
            vec![REFERRED_ID],
            vec![REFERRAL_OPPONENT_ID],
            1,
            Some(REFERRAL_GUILD_ID),
        ),
        "2026-08-07T00:00:00+00:00",
    );
    core.pending_match_id = Some(REFERRAL_PENDING_MATCH_ID);
    core.winning_ids = vec![REFERRED_ID];
    core.losing_ids = vec![REFERRAL_OPPONENT_ID];
    core.settle_referrals_at = Some(REFERRAL_REWARDED_AT);
    let match_id = matches.record_match_core_atomic(&core)?;
    if matches.record_match_core_atomic(&core)? != match_id {
        return Err("referral retry returned a different match id".into());
    }

    let connection = Connection::open(&path)?;
    let referral: (i64, i64, i64) = connection.query_row(
        "SELECT rewarded_match_id, reward_amount, rewarded_at
         FROM referrals WHERE guild_id = ?1 AND referred_id = ?2",
        params![REFERRAL_GUILD_ID, REFERRED_ID],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if referral != (match_id, 100, REFERRAL_REWARDED_AT) {
        return Err(format!("referral claim mismatch: {referral:?}").into());
    }
    let balances: Vec<(i64, i64, i64, i64)> = connection
        .prepare(
            "SELECT discord_id, jopacoin_balance, wins, losses FROM players
             WHERE guild_id = ?1 ORDER BY discord_id",
        )?
        .query_map([REFERRAL_GUILD_ID], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if balances
        != [
            (REFERRER_ID, 110, 0, 0),
            (REFERRED_ID, 120, 1, 0),
            (REFERRAL_OPPONENT_ID, 30, 0, 1),
        ]
    {
        return Err(format!("referral balances mismatch: {balances:?}").into());
    }
    for (discord_id, balance) in [
        (REFERRER_ID, 10_i64),
        (REFERRED_ID, 20_i64),
        (REFERRAL_OPPONENT_ID, 30_i64),
    ] {
        let other: (i64, i64, i64) = connection.query_row(
            "SELECT jopacoin_balance, wins, losses FROM players
             WHERE guild_id = ?1 AND discord_id = ?2",
            params![REFERRAL_OTHER_GUILD_ID, discord_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if other != (balance, 0, 0) {
            return Err(
                format!("cross-guild isolation mismatch for {discord_id}: {other:?}").into(),
            );
        }
    }
    let other_guild_counts: (i64, i64, i64) = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM referrals WHERE guild_id = ?1),
             (SELECT COUNT(*) FROM matches
              WHERE guild_id = ?1 AND pending_match_id = ?2),
             (SELECT COUNT(*) FROM economy_ledger_entries
              WHERE guild_id = ?1 AND source = 'referral_reward')",
        params![REFERRAL_OTHER_GUILD_ID, REFERRAL_PENDING_MATCH_ID],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if other_guild_counts != (0, 0, 0) {
        return Err(format!(
            "cross-guild referral rows unexpectedly changed: {other_guild_counts:?}"
        )
        .into());
    }
    let jc_changes: String = connection.query_row(
        "SELECT jc_changes FROM matches
         WHERE guild_id = ?1 AND pending_match_id = ?2",
        params![REFERRAL_GUILD_ID, REFERRAL_PENDING_MATCH_ID],
        |row| row.get(0),
    )?;
    let expected_jc_changes: Value = serde_json::json!({
        REFERRED_ID.to_string(): {"referral": 100},
        REFERRER_ID.to_string(): {"referral": 100},
    });
    if serde_json::from_str::<Value>(&jc_changes)? != expected_jc_changes {
        return Err(format!("referral JC summary mismatch: {jc_changes}").into());
    }
    let counts: (i64, i64, i64, i64) = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM matches
              WHERE guild_id = ?1 AND pending_match_id = ?2),
             (SELECT COUNT(*) FROM match_participants
              WHERE guild_id = ?1 AND match_id = ?3),
             (SELECT COUNT(*) FROM economy_ledger_entries
              WHERE guild_id = ?1 AND source = 'referral_reward'),
             (SELECT COUNT(*) FROM economy_ledger_context)",
        params![REFERRAL_GUILD_ID, REFERRAL_PENDING_MATCH_ID, match_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if counts != (1, 2, 2, 0) {
        return Err(format!("referral retry/count invariant mismatch: {counts:?}").into());
    }
    let ledger_rows = connection
        .prepare(
            "SELECT account_id, delta, source, related_type, related_id, metadata
             FROM economy_ledger_entries
             WHERE guild_id = ?1 AND source = 'referral_reward' ORDER BY ledger_id",
        )?
        .query_map([REFERRAL_GUILD_ID], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ledger_rows.len() != 2
        || ledger_rows[0].0 != REFERRED_ID
        || ledger_rows[1].0 != REFERRER_ID
        || ledger_rows.iter().any(|row| {
            row.1 != 100
                || row.2 != "referral_reward"
                || row.3 != "referral"
                || row.4 != REFERRED_ID.to_string()
        })
    {
        return Err(format!("referral ledger mismatch: {ledger_rows:?}").into());
    }
    for (row, role) in ledger_rows.iter().zip(["referred", "referrer"]) {
        let metadata: Value = serde_json::from_str(&row.5)?;
        let expected_metadata = serde_json::json!({
            "match_id": match_id,
            "referrer_id": REFERRER_ID,
            "referred_id": REFERRED_ID,
            "beneficiary_role": role,
        });
        if metadata != expected_metadata {
            return Err(format!("referral ledger metadata mismatch: {metadata}").into());
        }
    }

    println!(
        "referral_snapshot_smoke=ok guild_id={} match_id={} retry_same=true ledgers=2 balances=exact",
        REFERRAL_GUILD_ID, match_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cama_db::schema_manager::initialize_or_migrate;
    use std::path::Path;
    use tempfile::NamedTempFile;

    const TABLES: [&str; 7] = [
        "players",
        "referrals",
        "matches",
        "match_participants",
        "match_predictions",
        "player_pairings",
        "economy_ledger_entries",
    ];

    fn table_counts(path: &Path) -> Result<Vec<i64>, Box<dyn Error>> {
        let connection = Connection::open(path)?;
        TABLES
            .iter()
            .map(|table| {
                let sql = format!("SELECT COUNT(*) FROM {table}");
                Ok(connection.query_row(&sql, [], |row| row.get(0))?)
            })
            .collect()
    }

    #[test]
    fn guard_and_live_referral_settlement_use_fresh_migrated_database() -> Result<(), Box<dyn Error>>
    {
        let database = NamedTempFile::new()?;
        let path = database.path().to_owned();
        let path_argument = path.as_os_str().to_os_string();

        let error = run(vec![
            path_argument.clone(),
            OsString::from("--not-disposable-copy"),
        ]
        .into_iter())
        .expect_err("the write smoke must reject a non-exact confirmation flag");
        assert!(error.to_string().contains(CONFIRMATION));
        let error = run(vec![
            path_argument.clone(),
            OsString::from(CONFIRMATION),
            OsString::from("unexpected-extra-argument"),
        ]
        .into_iter())
        .expect_err("the write smoke must reject extra arguments after confirmation");
        assert!(error.to_string().contains(CONFIRMATION));
        let table_count: i64 = Connection::open(&path)?.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            table_count, 0,
            "the guard must run before opening the database"
        );

        initialize_or_migrate(&path)?;
        let before = table_counts(&path)?;
        run(vec![path_argument, OsString::from(CONFIRMATION)].into_iter())?;
        let after = table_counts(&path)?;
        let deltas: Vec<i64> = after
            .iter()
            .zip(before.iter())
            .map(|(after, before)| after - before)
            .collect();
        assert_eq!(deltas, [6, 1, 1, 2, 1, 1, 8]);
        Ok(())
    }
}
