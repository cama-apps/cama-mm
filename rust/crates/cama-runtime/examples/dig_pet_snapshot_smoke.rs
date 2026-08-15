//! Rust-only production-read smoke for an explicitly disposable SQLite copy.
//!
//! The caller supplies a copied, current-schema database. This probe audits
//! the copy, seeds isolated sentinels through Rust's existing repositories,
//! then exercises the live Dig bonus reward source and both production pet
//! flavor finance adapters. It never invokes Python and refuses to run
//! without the explicit disposable-copy confirmation.

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use cama_app::economy_event_service::EconomyEventConfig;
use cama_db::audit_database;
use cama_db::core_repositories::{BalanceLedgerContext, NewPlayer, PlayerRepository};
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_runtime::dig_bonus_runtime::{DigBonusRewardSource, SqliteDigBonusRewardSource};
use cama_runtime::{pet_provider, pet_sweep_worker};
use chrono::{Datelike, Duration, NaiveDate, Timelike, Utc, Weekday};
use rusqlite::{Connection, params};

const CONFIRMATION: &str = "--disposable-copy";
const GUILD_ID: i64 = -8_888_888_888_888_741;
const OTHER_GUILD_ID: i64 = -8_888_888_888_888_740;
const USER_ID: i64 = -8_888_888_888_888_739;
const DIG_ACTION_ID: i64 = -8_888_888_888_888_738;
const EVENT_NAME: &str = "Rust disposable Dig/pet flavor read smoke";

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: dig_pet_snapshot_smoke <db-path> --disposable-copy")?;
    if arguments
        .next()
        .and_then(|argument| argument.into_string().ok())
        .as_deref()
        != Some(CONFIRMATION)
        || arguments.next().is_some()
    {
        return Err("refusing Dig/pet read smoke without exactly --disposable-copy".into());
    }

    let audit = audit_database(&path)?;
    if !audit.is_compatible() {
        return Err(format!("snapshot is incompatible: {}", audit.issues().join("; ")).into());
    }

    let now = Utc::now().timestamp();
    let event_date = event_date_for_timestamp(now);
    reject_existing_sentinels(&path, &event_date)?;
    seed_players(&path)?;
    seed_active_event(&path, &event_date, now)?;
    seed_dig_action(&path, now)?;
    let before_reads = relevant_counts(&path)?;

    let reward_source = SqliteDigBonusRewardSource::new(&path, EconomyEventConfig::default());
    let adjusted =
        reward_source.event_adjusted_gross_jc_at(DIG_ACTION_ID, USER_ID, GUILD_ID, now)?;
    if adjusted != 23 {
        return Err(
            format!("Dig bonus reward read mismatch: expected 23, found {adjusted}").into(),
        );
    }
    if reward_source
        .event_adjusted_gross_jc_at(DIG_ACTION_ID, USER_ID, OTHER_GUILD_ID, now)
        .is_ok()
    {
        return Err("Dig bonus source ignored guild ownership".into());
    }

    let expected_entries =
        |context: (i64, Vec<cama_app::pet_flavor::LedgerEntry>)| -> Result<(), Box<dyn Error>> {
            let (balance, entries) = context;
            if balance != 321
                || entries.len() != 2
                || entries[0].delta != "318"
                || entries[0].source != "rust_disposable_snapshot"
                || entries[0].reason != "pet flavor read smoke"
            {
                return Err(std::io::Error::other(format!(
                    "unexpected pet flavor finance context: balance={balance}, entries={entries:?}"
                ))
                .into());
            }
            Ok(())
        };
    expected_entries(pet_sweep_worker::read_production_pet_flavor_context(
        &path, USER_ID, GUILD_ID, 8,
    )?)?;
    expected_entries(pet_provider::read_production_pet_flavor_context(
        &path, USER_ID, GUILD_ID, 8,
    )?)?;

    let other_context =
        pet_sweep_worker::read_production_pet_flavor_context(&path, USER_ID, OTHER_GUILD_ID, 8)?;
    if other_context.0 != 17
        || other_context.1.len() != 2
        || other_context.1[0].delta != "14"
        || other_context.1[0].source != "rust_disposable_snapshot"
    {
        return Err(format!("pet flavor guild isolation mismatch: {other_context:?}").into());
    }
    let after_reads = relevant_counts(&path)?;
    if before_reads != after_reads {
        return Err(format!(
            "production read adapters unexpectedly mutated the snapshot: before={before_reads:?} after={after_reads:?}"
        )
        .into());
    }

    println!(
        "dig_pet_snapshot_smoke=ok guild_id={GUILD_ID} action_id={DIG_ACTION_ID} adjusted_jc=23 worker_entries=2 provider_entries=2 guild_isolation=true read_only=true"
    );
    Ok(())
}

fn reject_existing_sentinels(path: &Path, event_date: &str) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open(path)?;
    let action_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM dig_actions WHERE id=?1)",
        [DIG_ACTION_ID],
        |row| row.get(0),
    )?;
    let player_exists: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM players
             WHERE discord_id=?1 AND guild_id IN (?2,?3)
         )",
        params![USER_ID, GUILD_ID, OTHER_GUILD_ID],
        |row| row.get(0),
    )?;
    let event_exists: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM economy_daily_events
             WHERE guild_id=?1 AND event_date=?2
         )",
        params![GUILD_ID, event_date],
        |row| row.get(0),
    )?;
    if action_exists || player_exists || event_exists {
        return Err("Dig/pet smoke sentinel already exists; use a fresh disposable copy".into());
    }
    Ok(())
}

fn seed_players(path: &Path) -> Result<(), Box<dyn Error>> {
    let repository = PlayerRepository::new(path);
    for guild_id in [GUILD_ID, OTHER_GUILD_ID] {
        repository.add(&NewPlayer::new(
            USER_ID,
            format!("Rust snapshot pet flavor {guild_id}"),
            Some(guild_id),
        ))?;
        let context = BalanceLedgerContext {
            source: Some("rust_disposable_snapshot".to_owned()),
            reason: Some("pet flavor read smoke".to_owned()),
            metadata: Some(format!("{{\"guild_id\":{guild_id}}}")),
            ..BalanceLedgerContext::default()
        };
        repository.add_balance_with_context(
            USER_ID,
            Some(guild_id),
            if guild_id == GUILD_ID { 318 } else { 14 },
            Some(&context),
        )?;
    }
    Ok(())
}

fn seed_active_event(path: &Path, event_date: &str, now: i64) -> Result<(), Box<dyn Error>> {
    EconomyEventRepository::new(path).activate_event_atomic(
        Some(GUILD_ID),
        &EventDraft {
            event_date: event_date.to_owned(),
            name: EVENT_NAME.to_owned(),
            hero: "Axe".to_owned(),
            direction: EventDirection::Boon,
            severity: 1,
            target_effect_jc: 0,
            forecast_flow_jc: 0,
            expected_effect_jc: 0,
            monetary_stock_before: 0,
            effects: EventEffects {
                reward_multiplier: 1.5,
                ..EventEffects::default()
            },
            announcement: EVENT_NAME.to_owned(),
            starts_at: now - 60,
            ends_at: now + 60,
            created_at: now,
        },
    )?;
    Ok(())
}

fn seed_dig_action(path: &Path, now: i64) -> Result<(), Box<dyn Error>> {
    Connection::open(path)?.execute(
        "INSERT INTO dig_actions (
             id,guild_id,actor_id,action_type,depth_before,depth_after,jc_delta,detail,created_at
         ) VALUES (?1,?2,?3,'dig',1,2,7,'{}',?4)",
        params![DIG_ACTION_ID, GUILD_ID, USER_ID, now - 1],
    )?;
    Ok(())
}

fn relevant_counts(path: &Path) -> Result<(i64, i64, i64, i64), Box<dyn Error>> {
    let connection = Connection::open(path)?;
    Ok((
        connection.query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row.get(0))?,
        connection.query_row("SELECT COUNT(*) FROM players", [], |row| row.get(0))?,
        connection.query_row("SELECT COUNT(*) FROM economy_daily_events", [], |row| {
            row.get(0)
        })?,
        connection.query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
            row.get(0)
        })?,
    ))
}

fn event_date_for_timestamp(timestamp: i64) -> String {
    let utc = chrono::DateTime::from_timestamp(timestamp, 0).expect("valid timestamp");
    let year = utc.year();
    let start_date = nth_weekday(year, 3, Weekday::Sun, 2);
    let end_date = nth_weekday(year, 11, Weekday::Sun, 1);
    let start_utc = start_date
        .and_hms_opt(10, 0, 0)
        .expect("valid transition hour")
        .and_utc()
        .timestamp();
    let end_utc = end_date
        .and_hms_opt(9, 0, 0)
        .expect("valid transition hour")
        .and_utc()
        .timestamp();
    let offset = if (start_utc..end_utc).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    };
    let local = chrono::DateTime::from_timestamp(timestamp + offset, 0).expect("valid local time");
    let mut date = local.date_naive();
    if local.hour() < 10 {
        date -= Duration::days(1);
    }
    date.format("%Y-%m-%d").to_string()
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, occurrence: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid fixed calendar month");
    let delta = (7 + weekday.num_days_from_sunday() as i64
        - first.weekday().num_days_from_sunday() as i64)
        % 7;
    first + Duration::days(delta + 7 * i64::from(occurrence - 1))
}
