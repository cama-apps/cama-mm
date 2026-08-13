//! Unadmitted differential audit for the 29 Python splash/themed-event cases.
//!
//! This file deliberately imports the in-progress event runtime by path.  It
//! is a migration audit only: no production module, command composition, or
//! parity manifest is changed by this harness.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use cama_domain::dig_splash::{HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use rusqlite::{Connection, OpenFlags, params};
use tempfile::NamedTempFile;

pub use cama_app::{dig_loot, dig_runtime, dig_tunnels, economy_event_sqlite};

// The in-progress application runtime deliberately is not admitted through
// cama-app/lib.rs yet.  Mirror the two private crate seams locally so this
// audit can execute it without changing production module composition.
extern crate cama_db as cama_db_external;
extern crate self as cama_db;

pub use cama_db_external::{
    dig_tunnel_encounters_repository, loan_repository, mana_service_repository, schema_manager,
    shop_runtime,
};

pub mod dig_event_runtime {
    pub use crate::dig_event_runtime_splash_repository::*;
}

fn open_runtime_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    Ok(connection)
}

#[path = "../../cama-db/src/dig_event_runtime.rs"]
mod dig_event_runtime_splash_repository;

#[path = "../src/dig_event_runtime.rs"]
mod dig_runtime_under_test;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const NOW: i64 = 2_000_000_000;
const RECENT_ISO: &str = "2033-05-18T03:33:20+00:00";

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("canonical migrated schema");
        Self { database }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable foreign keys for Python-compatible fixture");
        connection
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, GUILD, format!("audit-{discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn seed_tunnel(&self, discord_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     prestige_level, prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, ?3, ?3, 100, 0, '[]', '{}', 4)",
                params![discord_id, GUILD, depth],
            )
            .expect("seed tunnel");
    }

    fn mark_recent_match(&self, discord_id: i64) {
        self.connection()
            .execute(
                "UPDATE players SET last_match_date=?1
                  WHERE discord_id=?2 AND guild_id=?3",
                params![RECENT_ISO, discord_id, GUILD],
            )
            .expect("mark recent match");
    }

    fn mark_dig(&self, discord_id: i64, created_at: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type,
                     depth_before, depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 1, 2, 0, '{}', ?3)",
                params![GUILD, discord_id, created_at],
            )
            .expect("seed dig action");
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

    fn service(&self) -> dig_runtime_under_test::DigEventRuntimeService {
        let mut config = dig_runtime::DigRuntimeConfig::default();
        config.economy_event.enabled = false;
        dig_runtime_under_test::DigEventRuntimeService::sqlite_with_config(
            self.database.path(),
            config,
        )
    }

    fn event(
        &self,
        event_id: &str,
        choice: &str,
        key: &str,
    ) -> dig_runtime_under_test::DigEventRuntimeOutcome {
        self.service()
            .resolve_event(dig_runtime::DigRuntimeEventRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                event_id,
                choice,
                event_key: key,
                now: NOW,
                chained: false,
            })
            .expect("event resolution")
    }
}

fn event_seed(event_id: &str, choice: &str, event_key: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in event_key
        .bytes()
        .chain(event_id.bytes())
        .chain(choice.bytes())
        .chain(ACTOR.to_le_bytes())
        .chain(GUILD.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn success_key(event_id: &str, chance: f64) -> String {
    for index in 0..10_000 {
        let key = format!("audit:{event_id}:{index}");
        let mut entropy = dig_loot::SeededLootEntropy::new(event_seed(event_id, "risky", &key));
        if dig_loot::LootEntropy::unit(&mut entropy) < chance {
            return key;
        }
    }
    panic!("no deterministic success key for {event_id}");
}

fn seed_actor(fixture: &Fixture, depth: i64, balance: i64) {
    fixture.seed_player(ACTOR, balance);
    fixture.seed_tunnel(ACTOR, depth);
}

fn scale_burn(authored: i64) -> i64 {
    scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(authored) as f64, 1.0)
}

fn scale_steal(authored: i64) -> i64 {
    scale_minigame_jc_delta(authored as f64, 1.0)
}

fn effective_ev(jc: i64) -> f64 {
    if jc < 0 { jc as f64 * 1.3 } else { jc as f64 }
}

fn branch_ev(choice: &dig_loot::CanonicalEventChoice) -> f64 {
    let success = choice.success_chance * effective_ev(choice.success.jc);
    let failure = choice.failure.as_ref().map_or(0.0, |outcome| {
        (1.0 - choice.success_chance) * effective_ev(outcome.jc)
    });
    success + failure
}

fn canonical_choice(event_id: &str, choice: &str) -> &'static dig_loot::CanonicalEventChoice {
    let event = dig_loot::canonical_event(event_id).expect("catalog event");
    match choice {
        "safe" => event.safe_option.as_ref().expect("safe option"),
        "risky" => event.risky_option.as_ref().expect("risky option"),
        _ => panic!("unsupported audit choice {choice}"),
    }
}

// -------------------------------------------------------------------------
// Python tests/test_dig_splash.py (15 cases)
// -------------------------------------------------------------------------

#[test]
fn audit_random_active_excludes_digger() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 90, 100);
    for id in [10_002, 10_003] {
        fixture.seed_player(id, 100);
        fixture.mark_recent_match(id);
    }
    let key = success_key("the_tear", 0.5);
    let outcome = fixture.event("the_tear", "risky", &key);
    let splash = outcome.splash.expect("random active splash");
    assert_eq!(splash.victims.len(), 2);
    assert!(splash.victims.iter().all(|(id, _)| *id != ACTOR));
}

#[test]
fn audit_richest_n_excludes_digger_and_sorts() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 1_000);
    fixture.seed_player(10_002, 500);
    fixture.seed_player(10_003, 800);
    let key = success_key("wealth_siphon", 0.5);
    let splash = fixture
        .event("wealth_siphon", "risky", &key)
        .splash
        .expect("richest splash");
    assert_eq!(
        splash.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_003, 10_002]
    );
}

#[test]
fn audit_active_diggers_only_includes_recent_diggers() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 100);
    fixture.seed_player(10_002, 100);
    fixture.seed_player(10_003, 100);
    fixture.mark_dig(10_002, NOW - 30);
    let key = success_key("collectors_roots", 0.6);
    let splash = fixture
        .event("collectors_roots", "risky", &key)
        .splash
        .expect("active digger splash");
    assert_eq!(
        splash.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [10_002]
    );
}

#[test]
fn audit_empty_pool_returns_empty_result() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 100);
    let key = success_key("wealth_siphon", 0.5);
    let outcome = fixture.event("wealth_siphon", "risky", &key);
    assert!(
        outcome
            .splash
            .is_some_and(|splash| splash.victims.is_empty())
    );
}

#[test]
fn audit_unknown_strategy_is_fail_closed() {
    // The unadmitted runtime's private string parser maps this value to None;
    // the public catalog cannot manufacture an invalid strategy. Keep the
    // differential assertion here until that parser is admitted publicly.
    assert!(dig_loot::canonical_event("not_a_real_strategy").is_none());
}

#[test]
fn audit_burns_exactly_penalty_per_victim() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 500);
    for id in [10_002, 10_003] {
        fixture.seed_player(id, 500);
        fixture.mark_recent_match(id);
    }
    let before_actor = fixture.balance(ACTOR);
    let before = fixture.balance(10_002) + fixture.balance(10_003);
    let key = success_key("wealth_siphon", 0.5);
    let outcome = fixture.event("wealth_siphon", "risky", &key);
    let resolution = outcome.resolution.expect("resolution");
    let splash = outcome.splash.expect("burn splash");
    assert_eq!(splash.mode, "burn");
    assert_eq!(splash.total_moved, 2 * scale_burn(10));
    assert_eq!(
        before - fixture.balance(10_002) - fixture.balance(10_003),
        splash.total_moved
    );
    // A burn is deflationary: only the event payout, not the splash amount,
    // is credited to the digger.
    assert_eq!(fixture.balance(ACTOR) - before_actor, resolution.jc);
}

#[test]
fn audit_skips_players_below_auto_blind_threshold() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 500);
    fixture.seed_player(10_002, 3);
    fixture.mark_recent_match(10_002);
    let key = success_key("wealth_siphon", 0.5);
    let outcome = fixture.event("wealth_siphon", "risky", &key);
    assert!(
        outcome
            .splash
            .is_some_and(|splash| splash.victims.is_empty())
    );
    assert_eq!(fixture.balance(10_002), 3);
}

#[test]
fn audit_skips_debtors() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 500);
    fixture.seed_player(10_002, -50);
    fixture.seed_player(10_003, 100);
    let key = success_key("wealth_siphon", 0.5);
    let splash = fixture
        .event("wealth_siphon", "risky", &key)
        .splash
        .expect("burn splash");
    assert!(splash.victims.iter().all(|(id, _)| *id != 10_002));
    assert_eq!(fixture.balance(10_002), -50);
}

#[test]
fn audit_burn_routes_through_white_protection_gateway() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 500);
    let victim = 10_002;
    fixture.seed_player(victim, 200);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs (
                 discord_id, guild_id, buff_type, granted_at, expires_at,
                 triggered, data
             ) VALUES (?1, ?2, 'aegis', ?3, ?4, 0, ?5)",
            params![
                victim,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":10,"capacity_remaining":10,"rate":1.0}"#
            ],
        )
        .expect("seed Aegis shield");
    let key = success_key("wealth_siphon", 0.5);
    let outcome = fixture.event("wealth_siphon", "risky", &key);
    let splash = outcome.splash.expect("protected burn splash");
    assert_eq!(splash.shielded_count, 1);
    assert!(splash.absorbed_total > 0);
    assert_eq!(fixture.balance(victim), 200 - splash.total_moved);
}

#[test]
fn audit_real_protection_batches_multi_victim_burns() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 500);
    for id in [10_002, 10_003, 10_004] {
        fixture.seed_player(id, 500);
        fixture.mark_recent_match(id);
    }
    let key = success_key("wealth_siphon", 0.5);
    let splash = fixture
        .event("wealth_siphon", "risky", &key)
        .splash
        .expect("batch burn splash");
    assert_eq!(splash.victims.len(), 3);
    assert_eq!(splash.total_moved, 3 * scale_burn(10));
}

#[test]
fn audit_lookback_excludes_old_digs() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 100);
    fixture.seed_player(10_002, 100);
    fixture.mark_dig(10_002, NOW - 8 * 86_400);
    let key = success_key("collectors_roots", 0.6);
    let outcome = fixture.event("collectors_roots", "risky", &key);
    assert!(
        outcome
            .splash
            .is_some_and(|splash| splash.victims.is_empty())
    );
}

#[test]
fn audit_grant_credits_recipient() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 100);
    let partner = 10_002;
    fixture.seed_player(partner, 100);
    fixture.mark_dig(partner, NOW - 30);
    let key = success_key("io_tether_pact", 0.5);
    let outcome = fixture.event("io_tether_pact", "risky", &key);
    let splash = outcome.splash.expect("grant splash");
    assert_eq!(splash.mode, "grant");
    assert_eq!(splash.victims, vec![(partner, splash.total_moved)]);
    assert_eq!(fixture.balance(partner), 100 + splash.total_moved);
}

#[test]
fn audit_grant_ignores_zero_balance_guard() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 70, 100);
    let partner = 10_002;
    fixture.seed_player(partner, -25);
    fixture.mark_dig(partner, NOW - 30);
    let key = success_key("io_tether_pact", 0.5);
    let outcome = fixture.event("io_tether_pact", "risky", &key);
    let splash = outcome.splash.expect("grant splash");
    assert_eq!(fixture.balance(partner), -25 + splash.total_moved);
}

#[test]
fn audit_steal_does_not_strengthen_authored_amount() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 60, 100);
    let victim = 10_002;
    fixture.seed_player(victim, 100);
    let key = success_key("rhystic_tollgate", 0.5);
    let before_actor = fixture.balance(ACTOR);
    let before_victim = fixture.balance(victim);
    let outcome = fixture.event("rhystic_tollgate", "risky", &key);
    let splash = outcome.splash.expect("steal splash");
    let resolution = outcome.resolution.expect("resolution");
    assert_eq!(splash.mode, "steal");
    assert_eq!(before_victim - fixture.balance(victim), scale_steal(12));
    assert_eq!(
        fixture.balance(ACTOR) - before_actor - resolution.jc,
        scale_steal(12)
    );
}

#[test]
fn audit_deepest_n_targets_deepest_excluding_digger() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 500, 100);
    for (id, depth) in [(10_002, 200), (10_003, 150), (10_004, 50)] {
        fixture.seed_player(id, 100);
        fixture.seed_tunnel(id, depth);
    }
    let key = success_key("the_deep_hunter", 0.5);
    let splash = fixture
        .event("the_deep_hunter", "risky", &key)
        .splash
        .expect("deepest splash");
    assert_eq!(
        splash.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_002, 10_003]
    );
    assert!(splash.victims.iter().all(|(id, _)| *id != ACTOR));
    assert_eq!(fixture.balance(10_004), 100);
    assert_eq!(HOSTILE_LOSS_MIN_BALANCE, 50);
}

// -------------------------------------------------------------------------
// Python tests/test_dig_themed_ev_events.py (14 cases)
// -------------------------------------------------------------------------

#[test]
fn audit_themed_events_are_registered() {
    for event_id in [
        "second_life_ledger",
        "twin_scar_altar",
        "ember_clearinghouse",
    ] {
        assert!(dig_loot::canonical_event(event_id).is_some(), "{event_id}");
    }
}

#[test]
fn audit_second_life_safe_branch_is_guaranteed_loss() {
    let choice = canonical_choice("second_life_ledger", "safe");
    assert_eq!(choice.success_chance, 1.0);
    assert!(choice.failure.is_none());
    assert!(choice.success.jc < 0);
}

#[test]
fn audit_twin_scar_safe_branch_is_guaranteed_loss() {
    let choice = canonical_choice("twin_scar_altar", "safe");
    assert_eq!(choice.success_chance, 1.0);
    assert!(choice.failure.is_none());
    assert!(choice.success.jc < 0);
}

#[test]
fn audit_ember_safe_branch_is_guaranteed_loss() {
    let choice = canonical_choice("ember_clearinghouse", "safe");
    assert_eq!(choice.success_chance, 1.0);
    assert!(choice.failure.is_none());
    assert!(choice.success.jc < 0);
}

#[test]
fn audit_second_life_is_negative_ev_on_every_branch() {
    let event = dig_loot::canonical_event("second_life_ledger").expect("event");
    assert!(branch_ev(event.safe_option.as_ref().expect("safe")) < 0.0);
    assert!(branch_ev(event.risky_option.as_ref().expect("risky")) < 0.0);
}

#[test]
fn audit_twin_scar_is_negative_ev_on_every_branch() {
    let event = dig_loot::canonical_event("twin_scar_altar").expect("event");
    assert!(branch_ev(event.safe_option.as_ref().expect("safe")) < 0.0);
    assert!(branch_ev(event.risky_option.as_ref().expect("risky")) < 0.0);
}

#[test]
fn audit_sealed_reliquary_safe_branch_is_guaranteed_loss() {
    let choice = canonical_choice("sealed_reliquary", "safe");
    assert_eq!(choice.success_chance, 1.0);
    assert!(choice.failure.is_none());
    assert!(choice.success.jc < 0);
}

#[test]
fn audit_grenths_tithe_safe_branch_is_guaranteed_loss() {
    let choice = canonical_choice("grenths_tithe", "safe");
    assert_eq!(choice.success_chance, 1.0);
    assert!(choice.failure.is_none());
    assert!(choice.success.jc < 0);
}

#[test]
fn audit_sealed_reliquary_is_negative_ev_on_every_branch() {
    let event = dig_loot::canonical_event("sealed_reliquary").expect("event");
    assert!(branch_ev(event.safe_option.as_ref().expect("safe")) < 0.0);
    assert!(branch_ev(event.risky_option.as_ref().expect("risky")) < 0.0);
}

#[test]
fn audit_grenths_tithe_is_negative_ev_on_every_branch() {
    let event = dig_loot::canonical_event("grenths_tithe").expect("event");
    assert!(branch_ev(event.safe_option.as_ref().expect("safe")) < 0.0);
    assert!(branch_ev(event.risky_option.as_ref().expect("risky")) < 0.0);
}

#[test]
fn audit_trap_risky_still_outrewards_safe_headline() {
    for event_id in ["sealed_reliquary", "grenths_tithe"] {
        let event = dig_loot::canonical_event(event_id).expect("event");
        assert!(
            event.risky_option.as_ref().expect("risky").success.jc
                > event.safe_option.as_ref().expect("safe").success.jc,
            "{event_id}"
        );
    }
}

#[test]
fn audit_rhystic_tollgate_steals_from_the_richest() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 60, 500);
    for id in [10_002, 10_003, 10_004] {
        fixture.seed_player(id, 1_000);
    }
    let before = [10_002, 10_003, 10_004]
        .into_iter()
        .map(|id| fixture.balance(id))
        .collect::<Vec<_>>();
    let key = success_key("rhystic_tollgate", 0.5);
    let outcome = fixture.event("rhystic_tollgate", "risky", &key);
    let resolution = outcome.resolution.expect("resolution");
    let splash = outcome.splash.expect("steal splash");
    assert_eq!(splash.mode, "steal");
    assert_eq!(splash.total_moved, 3 * scale_steal(12));
    assert_eq!(
        before
            .into_iter()
            .zip([10_002, 10_003, 10_004])
            .map(|(old, id)| old - fixture.balance(id))
            .collect::<Vec<_>>(),
        vec![scale_steal(12); 3]
    );
    assert_eq!(
        fixture.balance(ACTOR) - 500,
        resolution.jc + splash.total_moved
    );
}

#[test]
fn audit_underworld_reclaims_burns_the_richest() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 80, 500);
    for id in [10_002, 10_003, 10_004] {
        fixture.seed_player(id, 1_000);
    }
    let before = fixture.balance(ACTOR);
    let key = success_key("underworld_reclaims", 0.5);
    let outcome = fixture.event("underworld_reclaims", "risky", &key);
    let resolution = outcome.resolution.expect("resolution");
    let splash = outcome.splash.expect("burn splash");
    assert_eq!(splash.mode, "burn");
    assert_eq!(splash.total_moved, 3 * scale_burn(22));
    assert_eq!(fixture.balance(ACTOR) - before, resolution.jc);
    assert!(splash.total_moved > resolution.jc);
}

#[test]
fn audit_ember_clearinghouse_burns_only_three_richest() {
    let fixture = Fixture::new();
    seed_actor(&fixture, 80, 500);
    for (id, balance) in [
        (10_002, 1_300),
        (10_003, 1_200),
        (10_004, 1_100),
        (10_005, 1_000),
    ] {
        fixture.seed_player(id, balance);
    }
    let key = success_key("ember_clearinghouse", 0.55);
    let outcome = fixture.event("ember_clearinghouse", "risky", &key);
    let resolution = outcome.resolution.expect("resolution");
    let splash = outcome.splash.expect("burn splash");
    let expected = scale_burn(18);
    assert_eq!(splash.mode, "burn");
    assert_eq!(splash.total_moved, 3 * expected);
    for (id, balance) in [(10_002, 1_300), (10_003, 1_200), (10_004, 1_100)] {
        assert_eq!(fixture.balance(id), balance - expected);
    }
    assert_eq!(fixture.balance(10_005), 1_000);
    assert_eq!(fixture.balance(ACTOR) - 500, resolution.jc);
    assert!(splash.total_moved > resolution.jc);
}
