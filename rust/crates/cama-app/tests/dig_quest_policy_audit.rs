//! Unadmitted differential audit for the 31 Python quest service/integration cases.
//!
//! This harness is deliberately standalone.  It imports the generated quest
//! policy, the in-progress SQLite event runtime, and the unadmitted quest
//! repository by path so quest behavior can be checked without changing
//! production module admission, schema ownership, command wiring, or parity
//! manifests.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use cama_app::dig_loot::{
    CanonicalQuestFinale, CanonicalQuestFinaleOutcome, CanonicalQuestProgress, LootEntropy,
    advance_canonical_quest_on_desperate_success, canonical_chain_event, canonical_eligible_events,
    canonical_eligible_quest_event_ids, canonical_quest_catalog, canonical_quest_for_event,
    resolve_canonical_quest_finale,
};
use rusqlite::{Connection, OpenFlags, params};
use tempfile::NamedTempFile;

pub use cama_app::{
    dig_bosses, dig_loot, dig_runtime, dig_service, dig_tunnels, economy_event_sqlite,
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
fn audit_event_picker_excludes_quest_events_without_player_context() {
    let allowed = BTreeSet::from(["agh_s1".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, false, false);
    assert!(events.iter().all(|event| event.quest_id.is_none()));
    assert!(!events.iter().any(|event| event.id == "agh_s1"));
}

#[test]
fn audit_event_picker_includes_eligible_quest_starter() {
    let allowed = BTreeSet::from(["agh_s1".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, true, false);
    assert!(events.iter().any(|event| event.id == "agh_s1"));
}

#[test]
fn audit_event_picker_excludes_quest_event_on_different_stage() {
    let allowed = BTreeSet::from(["agh_s3".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, true, false);
    assert!(!events.iter().any(|event| event.id == "agh_s1"));
    assert!(events.iter().any(|event| event.id == "agh_s3"));
}

#[test]
fn audit_event_picker_excludes_quest_event_during_boss_combat() {
    let allowed = BTreeSet::from(["agh_s1".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, true, true);
    assert!(!events.iter().any(|event| event.quest_id.is_some()));
}

#[test]
fn audit_event_runtime_advances_on_desperate_success_only() {
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
fn audit_event_runtime_safe_success_does_not_advance() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let outcome = fixture.event("agh_s1", "safe", "quest-audit:safe");
    assert!(outcome.success);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn audit_event_runtime_risky_success_does_not_advance() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let key = event_key_for("agh_s1", "risky", 0.62, true);
    let outcome = fixture.event("agh_s1", "risky", &key);
    assert!(outcome.resolution.expect("resolution").succeeded);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn audit_event_runtime_desperate_failure_does_not_advance() {
    let fixture = Fixture::new();
    fixture.seed_actor(25, 0, 100);
    let key = event_key_for("agh_s1", "desperate", 0.40, false);
    let outcome = fixture.event("agh_s1", "desperate", &key);
    assert!(!outcome.resolution.expect("resolution").succeeded);
    assert!(fixture.quest_state().active_quest_id.is_none());
}

#[test]
fn audit_event_runtime_final_stage_returns_necropolis_relic_and_persists_it() {
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
fn audit_chain_selector_excludes_quest_events() {
    let event = canonical_chain_event("agh_s1", 50, 7, Some(0.0), Some(0.0));
    assert!(event.is_none_or(|event| event.quest_id.is_none()));
}

#[test]
fn audit_available_event_selector_excludes_quest_events() {
    let allowed = BTreeSet::from(["agh_s1".to_owned()]);
    let events = canonical_eligible_events(25, 100, 0, &allowed, false, false);
    assert!(events.iter().all(|event| event.quest_id.is_none()));
}

#[test]
fn audit_finale_policy_exposes_gross_jc_without_a_tax_fn_seam() {
    let quest = canonical_quest_catalog()
        .iter()
        .find(|quest| quest.quest_id == "agh_lost_trial")
        .expect("Aghanim quest");
    let CanonicalQuestFinale::JcPlusGuildModifier { personal_jc, .. } = &quest.finale else {
        panic!("expected JC finale");
    };
    let outcome = resolve_canonical_quest_finale(&quest.finale, &[]).expect("JC finale");
    let CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
        personal_jc: resolved,
        ..
    } = outcome
    else {
        panic!("expected JC finale outcome");
    };
    assert_eq!(resolved, *personal_jc);
    // The pure seam has no injected Python tax callback; runtime mana taxes
    // are a separate SQLite adapter concern and remain an integration gap.
}

#[test]
fn audit_finale_completion_precedes_dispatch_failure_and_retries_once() {
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
fn audit_quest_eligibility_accepts_caller_state_but_has_no_tunnel_forwarding_adapter() {
    // This is the production pure filter used by the eventual picker.  It
    // accepts the already-loaded scalar state, so the Python no-second-fetch
    // contract is not observable until an admitted roll_event adapter threads
    // a tunnel snapshot through it.
    let ids =
        canonical_eligible_quest_event_ids(25, 0, None, None, &BTreeSet::new(), &BTreeSet::new());
    assert!(ids.contains("agh_s1"));
}
