//! Unadmitted differential audit for the 31 Python quest service/integration cases.
//!
//! This harness is deliberately standalone.  It imports the generated quest
//! policy, the in-progress SQLite event runtime, and the unadmitted quest
//! repository by path so quest behavior can be checked without changing
//! production module admission, schema ownership, command wiring, or parity
//! manifests.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use cama_app::dig_loot::{
    CanonicalQuestFinaleOutcome, CanonicalQuestProgress, LootEntropy,
    advance_canonical_quest_on_desperate_success, canonical_eligible_events,
    canonical_eligible_quest_event_ids, canonical_quest_catalog, canonical_quest_for_event,
    resolve_canonical_quest_finale,
};
use rusqlite::{Connection, OpenFlags, params};
use tempfile::NamedTempFile;

pub use cama_app::{
    dig_bosses, dig_loot, dig_runtime, dig_service, dig_tunnels, economy_event_service,
    economy_event_sqlite,
};

// The event runtime is intentionally not admitted through cama-app/lib.rs yet.
// Re-export the exact dependency seams that its source imports, as the other
// migration audit harnesses do.
extern crate cama_db as cama_db_external;
extern crate self as cama_db;

pub use cama_db_external::{
    dig_event_threats, dig_tunnel_encounters_repository, economy_event_repository, loan_repository,
    mana_service_repository, schema_manager, shop_runtime,
};

pub mod dig_event_runtime {
    pub use crate::dig_event_runtime_quest_repository::*;
}

#[path = "../../cama-db/src/dig_event_runtime.rs"]
mod dig_event_runtime_quest_repository;
#[path = "../src/dig_event_runtime.rs"]
mod dig_event_runtime_quest_under_test;
#[path = "../../cama-db/src/dig_quest_repository.rs"]
mod dig_quest_repository_under_test;

const ACTOR: i64 = 83_001;
const GUILD: i64 = 83_002;
const OTHER_GUILD: i64 = 83_003;
const NOW: i64 = 2_000_000_000;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

fn migrated_database_template() -> &'static [u8] {
    MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("temporary schema template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("canonical migrated schema template");
        std::fs::read(database.path()).expect("read migrated schema template")
    })
}

pub(crate) fn copy_migrated_database(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, migrated_database_template())
}

pub mod test_support {
    pub(crate) use super::copy_migrated_database;
}

fn migrated_database() -> NamedTempFile {
    let mut database = NamedTempFile::new().expect("temporary SQLite database");
    database
        .as_file_mut()
        .write_all(migrated_database_template())
        .expect("copy migrated schema template");
    database
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
    connection.pragma_update(None, "synchronous", "OFF")?;
    Ok(connection)
}

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = migrated_database();
        Self { database }
    }

    fn connection(&self) -> Connection {
        open_runtime_connection(self.database.path()).expect("fixture connection")
    }

    fn seed_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    discord_id,
                    guild_id,
                    format!("quest-audit-{discord_id}-{guild_id}"),
                    balance
                ],
            )
            .expect("seed player");
    }

    fn seed_tunnel(&self, discord_id: i64, guild_id: i64, depth: i64, prestige: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     prestige_level, prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, ?3, ?3, 100, ?4, '[]', '{}', 4)",
                params![discord_id, guild_id, depth, prestige],
            )
            .expect("seed tunnel");
    }

    fn seed_actor(&self, depth: i64, prestige: i64, balance: i64) {
        self.seed_player(ACTOR, GUILD, balance);
        self.seed_tunnel(ACTOR, GUILD, depth, prestige);
    }

    fn seed_bet(&self, bet_time: i64) {
        self.connection()
            .execute(
                "INSERT INTO bets (
                     guild_id, discord_id, team_bet_on, amount, bet_time
                 ) VALUES (?1, ?2, 'radiant', 10, ?3)",
                params![GUILD, ACTOR, bet_time],
            )
            .expect("seed bet");
    }

    fn set_active(&self, quest_id: &str, step: i64) {
        dig_quest_repository_under_test::DigQuestRepository::new(self.database.path())
            .set_active(ACTOR, Some(GUILD), quest_id, step, NOW)
            .expect("set active quest");
    }

    fn quest_state(&self) -> dig_event_runtime_quest_repository::DigEventQuestSnapshot {
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(self.database.path())
            .quest_snapshot(
                dig_event_runtime_quest_repository::DigEventActorKey {
                    discord_id: ACTOR,
                    guild_id: Some(GUILD),
                },
                NOW,
            )
            .expect("quest snapshot")
    }

    fn service(&self) -> dig_event_runtime_quest_under_test::DigEventRuntimeService {
        let mut config = dig_runtime::DigRuntimeConfig::default();
        config.economy_event.enabled = false;
        dig_event_runtime_quest_under_test::DigEventRuntimeService::sqlite_with_config(
            self.database.path(),
            config,
        )
    }

    fn event(
        &self,
        event_id: &str,
        choice: &str,
        event_key: &str,
    ) -> dig_event_runtime_quest_under_test::DigEventRuntimeOutcome {
        self.service()
            .resolve_event(dig_runtime::DigRuntimeEventRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                event_id,
                choice,
                event_key,
                now: NOW,
                chained: false,
            })
            .expect("event resolution")
    }

    fn count_actions(&self, action_type: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 AND action_type=?3",
                params![ACTOR, GUILD, action_type],
                |row| row.get(0),
            )
            .expect("action count")
    }

    fn balance(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get(0),
            )
            .expect("balance")
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

fn event_key_for(event_id: &str, choice: &str, chance: f64, success: bool) -> String {
    for index in 0..10_000 {
        let key = format!("quest-audit:{event_id}:{choice}:{index}");
        let mut entropy = dig_loot::SeededLootEntropy::new(event_seed(event_id, choice, &key));
        let roll = entropy.unit();
        if (success && roll < chance) || (!success && roll >= chance) {
            return key;
        }
    }
    panic!("no deterministic event key for {event_id}/{choice}");
}

fn predicate_set(predicate: Option<&str>) -> BTreeSet<String> {
    predicate.into_iter().map(ToOwned::to_owned).collect()
}

fn eligible(
    depth: i64,
    prestige: i64,
    active_quest_id: Option<&str>,
    active_quest_step: Option<i64>,
    completed: &[&str],
    predicates: &[&str],
) -> BTreeSet<String> {
    canonical_eligible_quest_event_ids(
        depth,
        prestige,
        active_quest_id,
        active_quest_step,
        &completed
            .iter()
            .map(|quest_id| (*quest_id).to_owned())
            .collect(),
        &predicate_set(predicates.first().copied()),
    )
}

fn assert_only(ids: &BTreeSet<String>, expected: &str) {
    assert_eq!(ids, &BTreeSet::from([expected.to_owned()]));
}

fn subtract_five_tax(value: i64) -> i64 {
    value - 5
}

// -------------------------------------------------------------------------
// tests/test_dig_quest_service.py (17 cases)
// -------------------------------------------------------------------------

#[test]
fn audit_no_quests_or_completed_catalog_has_no_eligible_events() {
    let completed = canonical_quest_catalog()
        .iter()
        .map(|quest| quest.quest_id.as_str())
        .collect::<Vec<_>>();
    assert!(eligible(100, 10, None, None, &completed, &[]).is_empty());
}

#[test]
fn audit_agh_starter_is_eligible_at_default_prerequisites() {
    assert_only(&eligible(25, 0, None, None, &[], &[]), "agh_s1");
}

#[test]
fn audit_agh_depth_gate_filters_starter() {
    assert!(eligible(24, 0, None, None, &[], &[]).is_empty());
    assert_only(&eligible(25, 0, None, None, &[], &[]), "agh_s1");
}

#[test]
fn audit_necropolis_prestige_gate_filters_starter() {
    assert!(eligible(0, 1, None, None, &[], &[]).is_empty());
    assert_only(&eligible(0, 2, None, None, &[], &[]), "necro_s1");
}

#[test]
fn audit_bolas_bet_predicate_is_negative_without_recent_bet() {
    assert!(!eligible(0, 0, None, None, &[], &[]).contains("bolas_s1"));
}

#[test]
fn audit_bolas_bet_predicate_is_positive_when_satisfied() {
    assert!(eligible(0, 0, None, None, &[], &["bet_within_7d"]).contains("bolas_s1"));
}

#[test]
fn audit_bolas_bet_predicate_expires_outside_window() {
    let fixture = Fixture::new();
    fixture.seed_actor(0, 0, 100);
    fixture.seed_bet(NOW - 8 * 86_400);
    let snapshot = fixture.quest_state();
    assert!(!snapshot.recent_bet);
    assert!(!eligible(0, 0, None, None, &[], &[]).contains("bolas_s1"));
}

#[test]
fn audit_active_quest_exposes_only_current_stage() {
    assert_only(
        &eligible(0, 0, Some("agh_lost_trial"), Some(3), &[], &[]),
        "agh_s3",
    );
}

#[test]
fn audit_completed_quest_starter_is_not_eligible() {
    assert!(!eligible(25, 0, None, None, &["agh_lost_trial"], &[]).contains("agh_s1"));
}

#[test]
fn audit_one_active_quest_blocks_other_starters() {
    let ids = eligible(
        25,
        2,
        Some("agh_lost_trial"),
        Some(2),
        &[],
        &["bet_within_7d"],
    );
    assert_only(&ids, "agh_s2");
}

#[test]
fn audit_desperate_stage_one_success_activates_stage_two() {
    let progress = advance_canonical_quest_on_desperate_success(
        "agh_s1",
        None,
        None,
        &BTreeSet::new(),
        25,
        0,
        &BTreeSet::new(),
    );
    assert_eq!(
        progress,
        CanonicalQuestProgress::Activated {
            quest_id: "agh_lost_trial".to_owned(),
            next_step: 2,
        }
    );
}

#[test]
fn audit_desperate_success_for_mismatched_stage_is_noop() {
    let progress = advance_canonical_quest_on_desperate_success(
        "agh_s4",
        Some("agh_lost_trial"),
        Some(2),
        &BTreeSet::new(),
        25,
        0,
        &BTreeSet::new(),
    );
    assert_eq!(progress, CanonicalQuestProgress::Noop);
}

#[test]
fn audit_progression_reaches_necropolis_relic_finale_descriptor() {
    let quest = canonical_quest_catalog()
        .iter()
        .find(|quest| quest.quest_id == "necropolis_below")
        .expect("Necropolis quest");
    let mut active = None;
    let mut step = None;
    let completed = BTreeSet::new();
    let predicates = BTreeSet::new();
    for event_id in &quest.step_event_ids {
        match advance_canonical_quest_on_desperate_success(
            event_id,
            active.as_deref(),
            step,
            &completed,
            0,
            2,
            &predicates,
        ) {
            CanonicalQuestProgress::Activated {
                quest_id,
                next_step,
            }
            | CanonicalQuestProgress::Advanced {
                quest_id,
                next_step,
            } => {
                active = Some(quest_id);
                step = Some(next_step);
            }
            CanonicalQuestProgress::Completed { quest_id, finale } => {
                assert_eq!(quest_id, "necropolis_below");
                let outcome =
                    resolve_canonical_quest_finale(&finale, &[0.0, 0.1]).expect("relic finale");
                let CanonicalQuestFinaleOutcome::RelicGrant {
                    relic_name,
                    artifact_id,
                    relic_stat_ids,
                } = outcome
                else {
                    panic!("expected relic finale");
                };
                assert_eq!(relic_name, "Cloak of the Necropolis of Long Silence");
                assert!(artifact_id.starts_with("pinnacle:Cloak of the Necropolis:Long Silence:"));
                assert_eq!(relic_stat_ids.len(), 2);
                assert_ne!(relic_stat_ids[0], relic_stat_ids[1]);
            }
            CanonicalQuestProgress::Noop => panic!("quest progression stopped at {event_id}"),
        }
    }
}

#[test]
fn audit_agh_finale_credits_jc_and_sets_guild_modifier() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    fixture.set_active("agh_lost_trial", 5);
    let key = event_key_for("agh_s5", "desperate", 0.40, true);
    let outcome = fixture.event("agh_s5", "desperate", &key);
    let Some(dig_event_runtime_quest_under_test::DigEventQuestFinale::JcAndModifier {
        quest_id,
        gross_jc,
        net_jc,
        modifier,
        ..
    }) = outcome.quest_finale
    else {
        panic!("expected Aghanim JC finale");
    };
    assert_eq!(quest_id, "agh_lost_trial");
    assert_eq!(gross_jc, 75);
    assert_eq!(net_jc, 49);
    assert_eq!(
        fixture.balance(),
        100 + outcome.resolution.expect("resolution").jc + 49
    );
    assert_eq!(
        modifier.expect("guild modifier").modifier_id,
        "reagent_spill"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_guild_modifiers
                  WHERE guild_id=?1 AND modifier_id='reagent_spill'",
                params![GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("modifier row"),
        1
    );
}

#[test]
fn audit_completed_quest_is_not_reeligible_after_completion() {
    let fixture = Fixture::new();
    let repository =
        dig_quest_repository_under_test::DigQuestRepository::new(fixture.database.path());
    repository
        .complete_quest(ACTOR, Some(GUILD), "agh_lost_trial", NOW)
        .expect("complete quest");
    let state = repository
        .get_state(ACTOR, Some(GUILD))
        .expect("quest state");
    assert!(
        !eligible(
            25,
            0,
            state.active_quest_id.as_deref(),
            state.active_quest_step,
            &["agh_lost_trial"],
            &[],
        )
        .contains("agh_s1")
    );
}

#[test]
fn audit_quest_state_survives_tunnel_prestige_reset_and_is_guild_isolated() {
    let fixture = Fixture::new();
    fixture.seed_actor(10, 1, 100);
    fixture.seed_player(ACTOR, OTHER_GUILD, 100);
    fixture.seed_tunnel(ACTOR, OTHER_GUILD, 10, 1);
    let repository =
        dig_quest_repository_under_test::DigQuestRepository::new(fixture.database.path());
    repository
        .set_active(ACTOR, Some(GUILD), "agh_lost_trial", 3, NOW)
        .expect("set active");
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET depth=0, prestige_level=2
              WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("prestige reset");
    let state = repository
        .get_state(ACTOR, Some(GUILD))
        .expect("source state");
    assert_eq!(state.active_quest_id.as_deref(), Some("agh_lost_trial"));
    assert_eq!(state.active_quest_step, Some(3));
    let other = repository
        .get_state(ACTOR, Some(OTHER_GUILD))
        .expect("other guild state");
    assert!(other.active_quest_id.is_none());
    repository
        .set_active(ACTOR, Some(OTHER_GUILD), "agh_lost_trial", 1, NOW)
        .expect("same quest in other guild");
    assert_eq!(
        repository
            .get_state(ACTOR, Some(OTHER_GUILD))
            .expect("other state")
            .active_quest_step,
        Some(1)
    );
}

#[test]
fn audit_quest_for_event_lookup_returns_canonical_stage() {
    let (quest, step) = canonical_quest_for_event("agh_s3").expect("quest event");
    assert_eq!(quest.quest_id, "agh_lost_trial");
    assert_eq!(step, 3);
    assert!(canonical_quest_for_event("plain_event").is_none());
}

// -------------------------------------------------------------------------
// tests/test_dig_quest_integration.py (14 cases)
// -------------------------------------------------------------------------

#[test]
fn roll_event_excludes_quest_events_without_player_context() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let actor =
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(fixture.database.path())
            .actor_snapshot_for_event(dig_event_runtime_quest_repository::DigEventActorKey {
                discord_id: ACTOR,
                guild_id: Some(GUILD),
            })
            .expect("actor snapshot")
            .expect("seeded actor");
    let events = fixture.service().eligible_event_presentations_for_snapshot(
        &actor,
        &dig_event_runtime_quest_repository::DigEventQuestSnapshot::default(),
        false,
        false,
    );
    assert!(
        events
            .iter()
            .all(|event| canonical_quest_for_event(&event.event_id).is_none())
    );
    assert!(!events.iter().any(|event| event.event_id == "agh_s1"));
}

#[test]
fn roll_event_includes_quest_starter_when_eligible() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let repository =
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(fixture.database.path());
    let actor = repository
        .actor_snapshot_for_event(dig_event_runtime_quest_repository::DigEventActorKey {
            discord_id: ACTOR,
            guild_id: Some(GUILD),
        })
        .expect("actor snapshot")
        .expect("seeded actor");
    let events = fixture.service().eligible_event_presentations_for_snapshot(
        &actor,
        &fixture.quest_state(),
        true,
        false,
    );
    assert!(events.iter().any(|event| event.event_id == "agh_s1"));
}

#[test]
fn roll_event_excludes_quest_event_when_player_on_different_step() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    fixture.set_active("agh_lost_trial", 3);
    let repository =
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(fixture.database.path());
    let actor = repository
        .actor_snapshot_for_event(dig_event_runtime_quest_repository::DigEventActorKey {
            discord_id: ACTOR,
            guild_id: Some(GUILD),
        })
        .expect("actor snapshot")
        .expect("seeded actor");
    let events = fixture.service().eligible_event_presentations_for_snapshot(
        &actor,
        &fixture.quest_state(),
        true,
        false,
    );
    assert!(!events.iter().any(|event| event.event_id == "agh_s1"));
    assert!(events.iter().any(|event| event.event_id == "agh_s3"));
}

#[test]
fn roll_event_excludes_quest_event_during_boss_combat() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let repository =
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(fixture.database.path());
    let actor = repository
        .actor_snapshot_for_event(dig_event_runtime_quest_repository::DigEventActorKey {
            discord_id: ACTOR,
            guild_id: Some(GUILD),
        })
        .expect("actor snapshot")
        .expect("seeded actor");
    let events = fixture.service().eligible_event_presentations_for_snapshot(
        &actor,
        &fixture.quest_state(),
        true,
        true,
    );
    assert!(
        events
            .iter()
            .all(|event| canonical_quest_for_event(&event.event_id).is_none())
    );
}

#[test]
fn resolve_event_advances_on_desperate_success_only() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let key = event_key_for("agh_s1", "desperate", 0.40, true);
    let outcome = fixture.event("agh_s1", "desperate", &key);
    assert!(outcome.success);
    assert!(outcome.resolution.expect("resolution").succeeded);
    let state = fixture.quest_state();
    assert_eq!(state.active_quest_id.as_deref(), Some("agh_lost_trial"));
    assert_eq!(state.active_quest_step, Some(2));
}

#[test]
fn resolve_event_no_advance_on_safe() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let outcome = fixture.event("agh_s1", "safe", "quest-audit:safe");
    assert!(outcome.success);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn resolve_event_no_advance_on_risky_success() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let key = event_key_for("agh_s1", "risky", 0.62, true);
    let outcome = fixture.event("agh_s1", "risky", &key);
    assert!(outcome.resolution.expect("resolution").succeeded);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn resolve_event_no_advance_on_desperate_failure() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let key = event_key_for("agh_s1", "desperate", 0.40, false);
    let outcome = fixture.event("agh_s1", "desperate", &key);
    assert!(!outcome.resolution.expect("resolution").succeeded);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn resolve_event_final_stage_returns_finale() {
    let fixture = Fixture::new();
    fixture.seed_actor(0, 2, 100);
    fixture.set_active("necropolis_below", 5);
    let key = event_key_for("necro_s5", "desperate", 0.40, true);
    let outcome = fixture.event("necro_s5", "desperate", &key);
    let Some(dig_event_runtime_quest_under_test::DigEventQuestFinale::Relic {
        quest_id,
        relic_name,
        artifact_id,
        relic_stat_ids,
        reward_row_id,
    }) = outcome.quest_finale
    else {
        panic!("expected Necropolis relic finale");
    };
    assert_eq!(quest_id, "necropolis_below");
    assert_eq!(relic_name, "Cloak of the Necropolis of Long Silence");
    assert!(artifact_id.starts_with("pinnacle:Cloak of the Necropolis:Long Silence:"));
    assert_eq!(relic_stat_ids.len(), 2);
    assert_ne!(relic_stat_ids[0], relic_stat_ids[1]);
    assert!(reward_row_id.is_some());
    assert!(
        fixture
            .quest_state()
            .completed_quest_ids
            .contains("necropolis_below")
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2 AND is_relic=1",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("relic row"),
        1
    );
}

#[test]
fn chain_event_filter_excludes_quest_events() {
    let fixture = Fixture::new();
    fixture.seed_actor(50, 7, 100);
    // `enchanting_table` has a zero-payout safe branch, so after the success
    // draw the next seeded draw is the production chain roll. Search durable
    // keys until that roll takes the P7+ chain path, then resolve through the
    // full SQLite event service (including actor settlement and chain output).
    let key = (0..10_000)
        .map(|index| format!("quest-chain:{index}"))
        .find(|key| {
            let mut entropy =
                dig_loot::SeededLootEntropy::new(event_seed("enchanting_table", "safe", key));
            let _success_roll = entropy.unit();
            entropy.unit() < dig_loot::CANONICAL_EVENT_CHAIN_CHANCE
        })
        .expect("deterministic chain key");
    let outcome = fixture
        .service()
        .resolve_event(dig_runtime::DigRuntimeEventRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            event_id: "enchanting_table",
            choice: "safe",
            event_key: &key,
            now: NOW,
            chained: false,
        })
        .expect("resolved chained event");
    let event = outcome
        .chain_event
        .expect("P7 chain roll should select an event");
    assert!(canonical_quest_for_event(&event.event_id).is_none());
}

#[test]
fn audit_available_event_selector_excludes_quest_events() {
    let allowed = BTreeSet::from(["agh_s1".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, false, false);
    assert!(events.iter().all(|event| event.quest_id.is_none()));
}

#[test]
fn finale_jc_applies_tax_fn() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    fixture.set_active("agh_lost_trial", 5);
    let key = event_key_for("agh_s5", "desperate", 0.40, true);
    let outcome = fixture
        .service()
        .with_finale_tax_hook(subtract_five_tax)
        .resolve_event(dig_runtime::DigRuntimeEventRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            event_id: "agh_s5",
            choice: "desperate",
            event_key: &key,
            now: NOW,
            chained: false,
        })
        .expect("taxed finale");
    let Some(dig_event_runtime_quest_under_test::DigEventQuestFinale::JcAndModifier {
        gross_jc,
        net_jc,
        ..
    }) = outcome.quest_finale
    else {
        panic!("expected Aghanim JC finale");
    };
    assert_eq!(gross_jc, 75);
    assert_eq!(net_jc, 44);
    assert_eq!(
        fixture.balance(),
        100 + outcome.resolution.expect("resolution").jc + 44
    );
}

#[test]
fn finale_completes_quest_before_dispatch() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    fixture.set_active("agh_lost_trial", 5);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_event_actor
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'event'
             BEGIN SELECT RAISE(ABORT, 'forced actor failure'); END;",
        )
        .expect("install actor rollback trigger");
    let actor_key = event_key_for("agh_s5", "desperate", 0.40, true);
    assert!(
        fixture
            .service()
            .resolve_event(dig_runtime::DigRuntimeEventRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                event_id: "agh_s5",
                choice: "desperate",
                event_key: &actor_key,
                now: NOW,
                chained: false,
            })
            .is_err()
    );
    assert_eq!(fixture.quest_state().active_quest_step, Some(5));
    fixture
        .connection()
        .execute("DROP TRIGGER reject_event_actor", [])
        .expect("drop actor trigger");
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_quest_finale
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'quest_finale_jc'
             BEGIN SELECT RAISE(ABORT, 'forced finale failure'); END;",
        )
        .expect("install finale failure trigger");
    let first = fixture.event("agh_s5", "desperate", &actor_key);
    assert!(first.success);
    assert!(first.quest_finale.is_none());
    assert!(
        fixture
            .quest_state()
            .completed_quest_ids
            .contains("agh_lost_trial")
    );
    fixture
        .connection()
        .execute("DROP TRIGGER reject_quest_finale", [])
        .expect("drop finale trigger");
    let recovered = fixture.event("agh_s5", "desperate", &actor_key);
    assert!(!recovered.applied_now);
    assert!(recovered.quest_finale.is_some());
    assert_eq!(fixture.count_actions("event"), 1);
    assert_eq!(fixture.count_actions("quest_finale_jc"), 1);
}

#[test]
fn roll_event_passes_tunnel_through_to_quest_filter() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let repository =
        dig_event_runtime_quest_repository::DigEventRuntimeRepository::new(fixture.database.path());
    let key = dig_event_runtime_quest_repository::DigEventActorKey {
        discord_id: ACTOR,
        guild_id: Some(GUILD),
    };
    let actor = repository
        .actor_snapshot_for_event(key)
        .expect("actor snapshot")
        .expect("seeded actor");
    let quest = fixture.quest_state();
    let eligible = fixture
        .service()
        .eligible_event_presentations_for_snapshot(&actor, &quest, true, false);
    assert!(eligible.iter().any(|event| event.event_id == "agh_s1"));

    let mut entropy = dig_loot::SeededLootEntropy::new(0);
    let rolled = fixture
        .service()
        .roll_event_for_snapshot(&actor, &quest, true, false, &mut entropy)
        .expect("one eligible event");
    assert!(canonical_quest_for_event(&rolled.event_id).is_none() || rolled.event_id == "agh_s1");
}
