//! Public, provider-free counterparts for `tests/test_dig_quest_service.py`.
//!
//! The Python service composes repository I/O, event resolution, and Discord
//! delivery.  These tests pin the admitted policy/service boundary: injected
//! quest catalogs, exact eligibility/progression semantics, typed finale
//! payloads, and the already-migrated quest-state table.  Event-provider and
//! Discord delivery cases remain in their dedicated runtime audit target.

use std::collections::BTreeSet;

use cama_app::dig_loot::{
    CanonicalQuestDef, CanonicalQuestFinale, CanonicalQuestFinaleOutcome,
    CanonicalQuestPrerequisite, CanonicalQuestProgress,
};
use cama_app::dig_quest_policy::{
    QUEST_RECENT_BET_WINDOW_SECONDS, QuestProgressInput, advance_quest_on_desperate_success,
    eligible_quest_event_ids, quest_for_event, quest_system_predicate_satisfied,
    resolve_quest_finale_for_service,
};
use cama_db::dig_quest_repository::DigQuestRepository;
use cama_db::schema_manager;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

const ACTOR: i64 = 84_001;
const GUILD: i64 = 84_002;
const NOW: i64 = 2_000_000_000;

fn quest(
    quest_id: &str,
    min_depth: i64,
    min_prestige: i64,
    system_predicate: Option<&str>,
    finale: CanonicalQuestFinale,
) -> CanonicalQuestDef {
    CanonicalQuestDef {
        quest_id: quest_id.to_owned(),
        name: quest_id.to_owned(),
        starter_prereq: CanonicalQuestPrerequisite {
            min_depth,
            min_prestige,
            system_predicate: system_predicate.map(ToOwned::to_owned),
        },
        step_event_ids: (1..=5).map(|step| format!("{quest_id}_s{step}")).collect(),
        finale,
    }
}

fn relic_quest(quest_id: &str, min_depth: i64, min_prestige: i64) -> CanonicalQuestDef {
    quest(
        quest_id,
        min_depth,
        min_prestige,
        None,
        CanonicalQuestFinale::RelicGrant {
            relic_base: "Test Relic".to_owned(),
            relic_suffix: "Trials".to_owned(),
            stat_pool: vec![
                "hp_plus_1".to_owned(),
                "hit_plus_002".to_owned(),
                "boss_hit_minus".to_owned(),
            ],
            roll_count: 2,
        },
    )
}

fn jc_quest(quest_id: &str) -> CanonicalQuestDef {
    quest(
        quest_id,
        0,
        0,
        None,
        CanonicalQuestFinale::JcPlusGuildModifier {
            personal_jc: 75,
            modifier_id: "reagent_spill".to_owned(),
            duration_seconds: 1_800,
            modifier_payload: serde_json::json!({"jc_event_bonus_pct": 25}),
        },
    )
}

fn migrated_database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary quest database");
    schema_manager::initialize_or_migrate(file.path()).expect("canonical migrated schema");
    file
}

#[test]
fn test_no_quests_means_no_eligible_events() {
    let empty = BTreeSet::new();
    assert!(eligible_quest_event_ids(&[], 10, 0, None, None, &empty, &empty).is_empty());
}

#[test]
fn test_starter_eligible_with_default_prereqs() {
    let quests = [relic_quest("qtest", 0, 0)];
    let empty = BTreeSet::new();
    assert_eq!(
        eligible_quest_event_ids(&quests, 0, 0, None, None, &empty, &empty),
        BTreeSet::from(["qtest_s1".to_owned()])
    );
}

#[test]
fn test_depth_gate_filters_starter() {
    let quests = [relic_quest("qtest", 25, 0)];
    let empty = BTreeSet::new();
    assert!(eligible_quest_event_ids(&quests, 10, 0, None, None, &empty, &empty).is_empty());
    assert_eq!(
        eligible_quest_event_ids(&quests, 25, 0, None, None, &empty, &empty),
        BTreeSet::from(["qtest_s1".to_owned()])
    );
}

#[test]
fn test_prestige_gate_filters_starter() {
    let quests = [relic_quest("qtest", 0, 2)];
    let empty = BTreeSet::new();
    assert!(eligible_quest_event_ids(&quests, 0, 1, None, None, &empty, &empty).is_empty());
    assert_eq!(
        eligible_quest_event_ids(&quests, 0, 2, None, None, &empty, &empty),
        BTreeSet::from(["qtest_s1".to_owned()])
    );
}

#[test]
fn test_system_predicate_bet_within_7d_negative() {
    let quests = [quest(
        "qtest",
        0,
        0,
        Some("bet_within_7d"),
        CanonicalQuestFinale::RelicGrant {
            relic_base: "Relic".to_owned(),
            relic_suffix: "Test".to_owned(),
            stat_pool: vec!["a".to_owned(), "b".to_owned()],
            roll_count: 2,
        },
    )];
    let empty = BTreeSet::new();
    assert!(!quest_system_predicate_satisfied(
        "bet_within_7d",
        NOW,
        None
    ));
    assert!(eligible_quest_event_ids(&quests, 0, 0, None, None, &empty, &empty).is_empty());
}

#[test]
fn test_system_predicate_bet_within_7d_positive() {
    let quests = [quest(
        "qtest",
        0,
        0,
        Some("bet_within_7d"),
        CanonicalQuestFinale::RelicGrant {
            relic_base: "Relic".to_owned(),
            relic_suffix: "Test".to_owned(),
            stat_pool: vec!["a".to_owned(), "b".to_owned()],
            roll_count: 2,
        },
    )];
    let predicates = BTreeSet::from(["bet_within_7d".to_owned()]);
    assert!(quest_system_predicate_satisfied(
        "bet_within_7d",
        NOW,
        Some(NOW)
    ));
    assert_eq!(
        eligible_quest_event_ids(&quests, 0, 0, None, None, &BTreeSet::new(), &predicates),
        BTreeSet::from(["qtest_s1".to_owned()])
    );
}

#[test]
fn test_system_predicate_bet_outside_window_filters_out() {
    let old_bet = NOW - QUEST_RECENT_BET_WINDOW_SECONDS - 1;
    assert!(!quest_system_predicate_satisfied(
        "bet_within_7d",
        NOW,
        Some(old_bet)
    ));
}

#[test]
fn test_active_quest_only_exposes_current_stage() {
    let quests = [relic_quest("qtest", 25, 2)];
    let empty = BTreeSet::new();
    assert_eq!(
        eligible_quest_event_ids(&quests, 0, 0, Some("qtest"), Some(3), &empty, &empty,),
        BTreeSet::from(["qtest_s3".to_owned()])
    );
}

#[test]
fn test_completed_quest_starter_not_eligible() {
    let quests = [relic_quest("qtest", 0, 0)];
    let completed = BTreeSet::from(["qtest".to_owned()]);
    let empty = BTreeSet::new();
    assert!(eligible_quest_event_ids(&quests, 0, 0, None, None, &completed, &empty).is_empty());
}

#[test]
fn test_one_at_a_time_concurrency_blocks_other_starters() {
    let quests = [relic_quest("q_a", 0, 0), relic_quest("q_b", 0, 0)];
    let empty = BTreeSet::new();
    assert_eq!(
        eligible_quest_event_ids(&quests, 0, 0, Some("q_a"), Some(2), &empty, &empty,),
        BTreeSet::from(["q_a_s2".to_owned()])
    );
}

#[test]
fn test_advance_on_desperate_success_starts_quest_at_stage_2() {
    let quests = [relic_quest("qtest", 0, 0)];
    let empty = BTreeSet::new();
    assert_eq!(
        advance_quest_on_desperate_success(
            &quests,
            QuestProgressInput {
                event_id: "qtest_s1",
                active_quest_id: None,
                active_quest_step: None,
                completed_quest_ids: &empty,
                depth: 0,
                prestige_level: 0,
                satisfied_system_predicates: &empty,
            },
        ),
        CanonicalQuestProgress::Activated {
            quest_id: "qtest".to_owned(),
            next_step: 2,
        }
    );
}

#[test]
fn test_advance_ignores_event_mismatched_to_active_stage() {
    let quests = [relic_quest("qtest", 0, 0)];
    let empty = BTreeSet::new();
    assert_eq!(
        advance_quest_on_desperate_success(
            &quests,
            QuestProgressInput {
                event_id: "qtest_s4",
                active_quest_id: Some("qtest"),
                active_quest_step: Some(2),
                completed_quest_ids: &empty,
                depth: 0,
                prestige_level: 0,
                satisfied_system_predicates: &empty,
            },
        ),
        CanonicalQuestProgress::Noop
    );
}

#[test]
fn test_advance_through_to_finale_dispatches_relic() {
    let quests = [relic_quest("qtest", 0, 0)];
    let empty = BTreeSet::new();
    let mut active_id = None;
    let mut active_step = None;
    let mut completed = BTreeSet::new();
    let mut finale = None;
    for step in 1..=5 {
        let progress = advance_quest_on_desperate_success(
            &quests,
            QuestProgressInput {
                event_id: &format!("qtest_s{step}"),
                active_quest_id: active_id.as_deref(),
                active_quest_step: active_step,
                completed_quest_ids: &completed,
                depth: 0,
                prestige_level: 0,
                satisfied_system_predicates: &empty,
            },
        );
        match progress {
            CanonicalQuestProgress::Activated {
                quest_id,
                next_step,
            }
            | CanonicalQuestProgress::Advanced {
                quest_id,
                next_step,
            } => {
                active_id = Some(quest_id);
                active_step = Some(next_step);
            }
            CanonicalQuestProgress::Completed {
                quest_id,
                finale: value,
            } => {
                completed.insert(quest_id);
                active_id = None;
                active_step = None;
                finale = Some(value);
            }
            CanonicalQuestProgress::Noop => panic!("quest progression stopped at stage {step}"),
        }
    }
    let outcome = resolve_quest_finale_for_service(&finale.expect("relic finale"), &[0.0, 0.99])
        .expect("resolve relic finale");
    let CanonicalQuestFinaleOutcome::RelicGrant {
        relic_name,
        artifact_id,
        relic_stat_ids,
    } = outcome
    else {
        panic!("expected relic finale");
    };
    assert_eq!(relic_name, "Test Relic of Trials");
    assert!(artifact_id.starts_with("pinnacle:Test Relic:Trials:"));
    assert_eq!(relic_stat_ids.len(), 2);
    assert_ne!(relic_stat_ids[0], relic_stat_ids[1]);
    assert!(active_id.is_none());
    assert!(active_step.is_none());

    // The repository boundary owns the actual artifact insert; this test
    // pins the exact identifier consumed by that boundary.
    let database = migrated_database();
    let repository = DigQuestRepository::new(database.path());
    repository
        .add_relic_artifact(ACTOR, Some(GUILD), &artifact_id, NOW)
        .expect("persist relic");
    let connection = Connection::open(database.path()).expect("open database");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2 AND artifact_id=?3 AND is_relic=1",
                params![ACTOR, GUILD, artifact_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count relic"),
        1
    );
}

#[test]
fn test_finale_jc_plus_modifier_grants_jc_and_sets_modifier() {
    let quest = jc_quest("agh");
    let outcome = resolve_quest_finale_for_service(&quest.finale, &[]).expect("JC finale");
    let CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
        personal_jc,
        modifier_id,
        duration_seconds,
        modifier_payload,
    } = outcome
    else {
        panic!("expected JC finale");
    };
    assert_eq!(personal_jc, 49);
    assert_eq!(modifier_id, "reagent_spill");
    assert_eq!(duration_seconds, 1_800);
    assert_eq!(
        modifier_payload,
        serde_json::json!({"jc_event_bonus_pct": 25})
    );
}

#[test]
fn test_completed_quest_not_re_eligible_after_finale() {
    let quests = [relic_quest("qtest", 0, 0)];
    let completed = BTreeSet::from(["qtest".to_owned()]);
    let empty = BTreeSet::new();
    assert!(eligible_quest_event_ids(&quests, 0, 0, None, None, &completed, &empty).is_empty());
}

#[test]
fn test_progression_persists_across_prestige_reset() {
    let database = migrated_database();
    let repository = DigQuestRepository::new(database.path());
    repository
        .set_active(ACTOR, Some(GUILD), "qtest", 3, NOW)
        .expect("set active quest");
    let connection = Connection::open(database.path()).expect("open database");
    connection
        .execute(
            "INSERT INTO tunnels(discord_id,guild_id,depth,prestige_level,tunnel_name)
             VALUES(?1,?2,10,1,'Test')",
            params![ACTOR, GUILD],
        )
        .expect("seed tunnel");
    connection
        .execute(
            "UPDATE tunnels SET depth=0, prestige_level=2
              WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("reset tunnel");
    let state = repository
        .get_state(ACTOR, Some(GUILD))
        .expect("read quest state");
    assert_eq!(state.active_quest_id.as_deref(), Some("qtest"));
    assert_eq!(state.active_quest_step, Some(3));
}

#[test]
fn test_quest_for_event_lookup() {
    let quests = [relic_quest("qtest", 0, 0)];
    let (found, step) = quest_for_event(&quests, "qtest_s3").expect("quest event");
    assert_eq!(found.quest_id, "qtest");
    assert_eq!(step, 3);
    assert!(quest_for_event(&quests, "not_a_quest_event").is_none());
}
