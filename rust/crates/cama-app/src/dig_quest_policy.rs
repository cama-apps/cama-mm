//! Pure validation policy for the Python `/dig` quest registry.
//!
//! This is a small adapter seam for the quest catalog port: the validator only
//! needs the tags that Python validates and therefore does not depend on the
//! event renderer, SQLite, Discord, or the generated Dig catalog.  The runtime
//! owner can implement [`QuestDefinitionRef`] and [`QuestEventRef`] for its
//! canonical catalog types and call [`validate_quests`] during startup/catalog
//! generation.
//!
//! Python's validator calls rarity ranks `common=0` through `legendary=3`
//! and rejects a later stage whose rank is lower than its predecessor.  The
//! wording in the Python source calls this "monotonic non-increasing" even
//! though the rank sequence is non-decreasing; this module preserves the
//! actual behavior rather than correcting the historical terminology.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::dig_loot::{CanonicalQuestDef, CanonicalQuestFinale, CanonicalQuestProgress};

/// Python's cross-system starter predicate considers bets from this inclusive
/// look-back window.  The repository/service adapter supplies the timestamp;
/// this policy owns only the boundary and comparison.
pub const QUEST_RECENT_BET_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The number of event stages required by every Dig quest.
pub const QUEST_STAGE_COUNT: usize = 5;

/// Finale kinds accepted by the Python quest registry.
pub const VALID_FINALE_KINDS: [&str; 2] = ["jc_plus_guild_modifier", "relic_grant"];

/// Read-only view of a quest definition consumed by [`validate_quests`].
///
/// A trait keeps this policy independent from the catalog representation. In
/// particular, the generated Rust catalog uses a typed finale enum while the
/// Python-compatible snapshot still stores `finale_kind` as a string.
pub trait QuestDefinitionRef {
    /// Stable quest identifier used by event tags.
    fn quest_id(&self) -> &str;

    /// Ordered event identifiers for stages one through five.
    fn step_event_ids(&self) -> &[String];

    /// String discriminator for the terminal reward handler.
    fn finale_kind(&self) -> &str;
}

/// Read-only view of the event tags required by quest validation.
pub trait QuestEventRef {
    /// Event identifier used by the quest's ordered stage list.
    fn event_id(&self) -> &str;

    /// Optional quest tag on the event.
    fn quest_id(&self) -> Option<&str>;

    /// Optional one-indexed quest stage tag.
    fn quest_step(&self) -> Option<i64>;

    /// Authored rarity string.
    fn rarity(&self) -> &str;

    /// Whether the event offers the desperate advancement choice.
    fn has_desperate_option(&self) -> bool;
}

/// A precise validation failure.  `Display` deliberately retains the
/// important fragments of Python's `ValueError` messages so operators and
/// parity tests can identify the same failed invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestValidationError {
    WrongStageCount {
        quest_id: String,
        count: usize,
    },
    MissingEvent {
        quest_id: String,
        stage: usize,
        event_id: String,
    },
    MismatchedQuestId {
        quest_id: String,
        stage: usize,
        event_id: String,
        actual: Option<String>,
    },
    MismatchedQuestStep {
        quest_id: String,
        stage: usize,
        event_id: String,
        actual: Option<i64>,
    },
    MissingDesperateOption {
        quest_id: String,
        stage: usize,
        event_id: String,
    },
    UnknownRarity {
        quest_id: String,
        stage: usize,
        event_id: String,
        rarity: String,
    },
    NonMonotonicRarity {
        quest_id: String,
        stage: usize,
        event_id: String,
        rarity: String,
    },
    UnknownFinaleKind {
        quest_id: String,
        finale_kind: String,
    },
}

impl fmt::Display for QuestValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongStageCount { quest_id, count } => write!(
                formatter,
                "Quest {quest_id:?}: must have exactly {QUEST_STAGE_COUNT} stages, got {count}"
            ),
            Self::MissingEvent {
                quest_id,
                stage,
                event_id,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event id {event_id:?} not found in RANDOM_EVENTS"
            ),
            Self::MismatchedQuestId {
                quest_id,
                stage,
                event_id,
                actual,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event {event_id:?}.quest_id is {actual:?}, expected {quest_id:?}"
            ),
            Self::MismatchedQuestStep {
                quest_id,
                stage,
                event_id,
                actual,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event {event_id:?}.quest_step is {actual:?}, expected {stage}"
            ),
            Self::MissingDesperateOption {
                quest_id,
                stage,
                event_id,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event {event_id:?} has no desperate_option; quest events advance only on desperate-success and must offer it"
            ),
            Self::UnknownRarity {
                quest_id,
                stage,
                event_id,
                rarity,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event {event_id:?} has unrecognized rarity {rarity:?}"
            ),
            Self::NonMonotonicRarity {
                quest_id,
                stage,
                event_id,
                rarity,
            } => write!(
                formatter,
                "Quest {quest_id:?} stage {stage}: event {event_id:?} rarity {rarity:?} is more common than previous stage; rarity must be monotonic non-increasing across stages"
            ),
            Self::UnknownFinaleKind {
                quest_id,
                finale_kind,
            } => write!(
                formatter,
                "Quest {quest_id:?}: unknown finale_kind {finale_kind:?}"
            ),
        }
    }
}

impl std::error::Error for QuestValidationError {}

/// Validate quest definitions against the event registry.
///
/// The behavior mirrors `services.dig_data.quests.validate_quests`:
/// exactly five stages, matching quest and one-indexed step tags, a desperate
/// option on every stage, recognized monotonic rarity, and a recognized
/// finale kind. An empty quest slice is a successful no-op.
pub fn validate_quests<Q, E>(quests: &[Q], events: &[E]) -> Result<(), QuestValidationError>
where
    Q: QuestDefinitionRef,
    E: QuestEventRef,
{
    // Python builds a dict comprehension, so a duplicate event id resolves
    // to the last definition. HashMap::insert has the same behavior.
    let event_by_id: HashMap<&str, &E> = events
        .iter()
        .map(|event| (event.event_id(), event))
        .collect();

    for quest in quests {
        let quest_id = quest.quest_id();
        let step_event_ids = quest.step_event_ids();
        if step_event_ids.len() != QUEST_STAGE_COUNT {
            return Err(QuestValidationError::WrongStageCount {
                quest_id: quest_id.to_owned(),
                count: step_event_ids.len(),
            });
        }

        // Rank -1 is Python's initial sentinel. Every valid rarity has rank
        // >= 0, so the first stage is always accepted.
        let mut previous_rank = -1_i8;
        for (index, event_id) in step_event_ids.iter().enumerate() {
            let stage = index + 1;
            let Some(event) = event_by_id.get(event_id.as_str()).copied() else {
                return Err(QuestValidationError::MissingEvent {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                });
            };

            let actual_quest_id = event.quest_id();
            if actual_quest_id != Some(quest_id) {
                return Err(QuestValidationError::MismatchedQuestId {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                    actual: actual_quest_id.map(ToOwned::to_owned),
                });
            }

            let expected_step = stage as i64;
            if event.quest_step() != Some(expected_step) {
                return Err(QuestValidationError::MismatchedQuestStep {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                    actual: event.quest_step(),
                });
            }

            if !event.has_desperate_option() {
                return Err(QuestValidationError::MissingDesperateOption {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                });
            }

            let Some(rank) = rarity_rank(event.rarity()) else {
                return Err(QuestValidationError::UnknownRarity {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                    rarity: event.rarity().to_owned(),
                });
            };
            if rank < previous_rank {
                return Err(QuestValidationError::NonMonotonicRarity {
                    quest_id: quest_id.to_owned(),
                    stage,
                    event_id: event_id.clone(),
                    rarity: event.rarity().to_owned(),
                });
            }
            previous_rank = rank;
        }

        if !VALID_FINALE_KINDS.contains(&quest.finale_kind()) {
            return Err(QuestValidationError::UnknownFinaleKind {
                quest_id: quest_id.to_owned(),
                finale_kind: quest.finale_kind().to_owned(),
            });
        }
    }
    Ok(())
}

/// Resolve the one cross-system predicate currently used by the quest
/// registry.  Unknown predicates fail closed, matching Python's service
/// warning-and-deny behavior.  A recent bet is accepted when its timestamp is
/// at or after `now - QUEST_RECENT_BET_WINDOW_SECONDS`; this intentionally does
/// not reject future-dated rows because the Python repository uses the same
/// lower-bound SQL predicate.
#[must_use]
pub fn quest_system_predicate_satisfied(
    predicate: &str,
    now: i64,
    most_recent_bet_at: Option<i64>,
) -> bool {
    match predicate {
        "bet_within_7d" => most_recent_bet_at
            .is_some_and(|bet_at| bet_at >= now.saturating_sub(QUEST_RECENT_BET_WINDOW_SECONDS)),
        _ => false,
    }
}

/// Find a quest stage in an injected quest set.  Keeping the set as an
/// argument is important: Python's `DigQuestService` accepts a test quest
/// tuple, while the generated catalog helpers use the production tuple.
#[must_use]
pub fn quest_for_event<'a>(
    quests: &'a [CanonicalQuestDef],
    event_id: &str,
) -> Option<(&'a CanonicalQuestDef, i64)> {
    quests.iter().find_map(|quest| {
        quest
            .step_event_ids
            .iter()
            .position(|candidate| candidate == event_id)
            .map(|index| (quest, index as i64 + 1))
    })
}

/// Evaluate one quest's starter prerequisites using already-loaded caller
/// context.  Active quests deliberately do not call this helper; Python only
/// checks depth/prestige/predicates while starting a new arc.
#[must_use]
pub fn quest_starter_prerequisite_met(
    quest: &CanonicalQuestDef,
    depth: i64,
    prestige_level: i64,
    satisfied_system_predicates: &BTreeSet<String>,
) -> bool {
    depth >= quest.starter_prereq.min_depth
        && prestige_level >= quest.starter_prereq.min_prestige
        && quest
            .starter_prereq
            .system_predicate
            .as_ref()
            .is_none_or(|predicate| satisfied_system_predicates.contains(predicate))
}

/// Return eligible quest event ids for an injected quest set.
///
/// This is the pure service half of `DigQuestService.eligible_quest_event_ids`.
/// The caller resolves repository-backed predicates into the supplied set and
/// persists no state here.  An active quest exposes exactly its current stage;
/// starter depth, prestige, and predicate gates apply only without an active
/// quest.
#[must_use]
pub fn eligible_quest_event_ids(
    quests: &[CanonicalQuestDef],
    depth: i64,
    prestige_level: i64,
    active_quest_id: Option<&str>,
    active_quest_step: Option<i64>,
    completed_quest_ids: &BTreeSet<String>,
    satisfied_system_predicates: &BTreeSet<String>,
) -> BTreeSet<String> {
    // Python's state guard requires both fields to be truthy.  Valid rows use
    // positive stages, but retaining the zero check keeps this adapter exact
    // for malformed/admin-injected state too.
    if let (Some(active_id), Some(active_step)) = (active_quest_id, active_quest_step)
        && !active_id.is_empty()
        && active_step != 0
    {
        let Some(quest) = quests.iter().find(|quest| quest.quest_id == active_id) else {
            return BTreeSet::new();
        };
        let index = active_step - 1;
        return (index >= 0)
            .then(|| quest.step_event_ids.get(index as usize).cloned())
            .flatten()
            .into_iter()
            .collect();
    }

    quests
        .iter()
        .filter(|quest| !completed_quest_ids.contains(&quest.quest_id))
        .filter(|quest| {
            quest_starter_prerequisite_met(
                quest,
                depth,
                prestige_level,
                satisfied_system_predicates,
            )
        })
        .filter_map(|quest| quest.step_event_ids.first().cloned())
        .collect()
}

/// Inputs for one injected-catalog desperate-success transition.
#[derive(Clone, Copy, Debug)]
pub struct QuestProgressInput<'a> {
    pub event_id: &'a str,
    pub active_quest_id: Option<&'a str>,
    pub active_quest_step: Option<i64>,
    pub completed_quest_ids: &'a BTreeSet<String>,
    pub depth: i64,
    pub prestige_level: i64,
    pub satisfied_system_predicates: &'a BTreeSet<String>,
}

/// Apply the service's desperate-success state transition against an injected
/// quest set.  Repository adapters persist the returned descriptor.  The
/// caller must have already established that the desperate outcome succeeded;
/// this function intentionally does not inspect event choice payloads.
#[must_use]
pub fn advance_quest_on_desperate_success(
    quests: &[CanonicalQuestDef],
    input: QuestProgressInput<'_>,
) -> CanonicalQuestProgress {
    let Some((quest, event_step)) = quest_for_event(quests, input.event_id) else {
        return CanonicalQuestProgress::Noop;
    };

    // Python treats a missing active quest as a starter attempt.  A completed
    // quest, non-stage-one event, or failed prerequisite is a no-op.
    if input.active_quest_id.is_none() {
        if input.completed_quest_ids.contains(&quest.quest_id)
            || event_step != 1
            || !quest_starter_prerequisite_met(
                quest,
                input.depth,
                input.prestige_level,
                input.satisfied_system_predicates,
            )
        {
            return CanonicalQuestProgress::Noop;
        }
        return CanonicalQuestProgress::Activated {
            quest_id: quest.quest_id.clone(),
            next_step: 2,
        };
    }

    if input.active_quest_id != Some(quest.quest_id.as_str())
        || input.active_quest_step != Some(event_step)
    {
        return CanonicalQuestProgress::Noop;
    }
    if event_step >= quest.step_event_ids.len() as i64 {
        return CanonicalQuestProgress::Completed {
            quest_id: quest.quest_id.clone(),
            finale: quest.finale.clone(),
        };
    }
    CanonicalQuestProgress::Advanced {
        quest_id: quest.quest_id.clone(),
        next_step: event_step + 1,
    }
}

/// Resolve a quest finale for the service's economy boundary.  The lower
/// level resolver preserves authored gross JC for catalog/audit consumers;
/// this adapter applies Python's positive Dig minting scale before the value
/// is handed to a balance repository.  Tax callbacks remain an outer service
/// concern, just as in Python's injected `tax_fn`.
pub fn resolve_quest_finale_for_service(
    finale: &CanonicalQuestFinale,
    selection_rolls: &[f64],
) -> Result<crate::dig_loot::CanonicalQuestFinaleOutcome, String> {
    let outcome = crate::dig_loot::resolve_canonical_quest_finale(finale, selection_rolls)?;
    Ok(match outcome {
        crate::dig_loot::CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
            personal_jc,
            modifier_id,
            duration_seconds,
            modifier_payload,
        } => crate::dig_loot::CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
            personal_jc: cama_domain::dig_economy::scale_positive_dig_jc(personal_jc),
            modifier_id,
            duration_seconds,
            modifier_payload,
        },
        relic @ crate::dig_loot::CanonicalQuestFinaleOutcome::RelicGrant { .. } => relic,
    })
}

fn rarity_rank(rarity: &str) -> Option<i8> {
    match rarity {
        "common" => Some(0),
        "uncommon" => Some(1),
        "rare" => Some(2),
        "legendary" => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct FakeEvent {
        id: String,
        quest_id: Option<String>,
        quest_step: Option<i64>,
        rarity: String,
        desperate: bool,
    }

    impl QuestEventRef for FakeEvent {
        fn event_id(&self) -> &str {
            &self.id
        }

        fn quest_id(&self) -> Option<&str> {
            self.quest_id.as_deref()
        }

        fn quest_step(&self) -> Option<i64> {
            self.quest_step
        }

        fn rarity(&self) -> &str {
            &self.rarity
        }

        fn has_desperate_option(&self) -> bool {
            self.desperate
        }
    }

    #[derive(Clone, Debug)]
    struct FakeQuest {
        id: String,
        steps: Vec<String>,
        finale: String,
    }

    impl QuestDefinitionRef for FakeQuest {
        fn quest_id(&self) -> &str {
            &self.id
        }

        fn step_event_ids(&self) -> &[String] {
            &self.steps
        }

        fn finale_kind(&self) -> &str {
            &self.finale
        }
    }

    fn make_event(
        id: &str,
        quest_id: Option<&str>,
        quest_step: Option<i64>,
        rarity: &str,
        desperate: bool,
    ) -> FakeEvent {
        FakeEvent {
            id: id.to_owned(),
            quest_id: quest_id.map(ToOwned::to_owned),
            quest_step,
            rarity: rarity.to_owned(),
            desperate,
        }
    }

    fn make_quest(step_ids: &[&str], finale: &str) -> FakeQuest {
        FakeQuest {
            id: "fakequest".to_owned(),
            steps: step_ids.iter().map(|id| (*id).to_owned()).collect(),
            finale: finale.to_owned(),
        }
    }

    fn valid_events(rarities: &[&str]) -> Vec<FakeEvent> {
        rarities
            .iter()
            .enumerate()
            .map(|(index, rarity)| {
                let stage = index + 1;
                make_event(
                    &format!("s{stage}"),
                    Some("fakequest"),
                    Some(stage as i64),
                    rarity,
                    true,
                )
            })
            .collect()
    }

    fn assert_error_contains(result: Result<(), QuestValidationError>, needle: &str) {
        let error = result.expect_err("validator should reject the fixture");
        assert!(
            error.to_string().contains(needle),
            "{error} does not contain {needle:?}"
        );
    }

    #[test]
    fn empty_quests_is_noop() {
        validate_quests::<FakeQuest, FakeEvent>(&[], &[]).expect("empty registry is valid");
    }

    #[test]
    fn valid_quest_passes() {
        let events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        validate_quests(
            &[make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant")],
            &events,
        )
        .expect("valid quest");
    }

    #[test]
    fn wrong_stage_count_fails() {
        let events = valid_events(&["common", "common", "uncommon", "uncommon"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "exactly 5 stages");
    }

    #[test]
    fn missing_event_fails() {
        let events = valid_events(&["common", "common", "uncommon", "uncommon"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(
            validate_quests(&[quest], &events),
            "not found in RANDOM_EVENTS",
        );
    }

    #[test]
    fn mismatched_quest_id_tag_fails() {
        let mut events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        events[2] = make_event("s3", Some("wrong_quest"), Some(3), "uncommon", true);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "quest_id");
    }

    #[test]
    fn mismatched_quest_step_tag_fails() {
        let mut events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        events[2] = make_event("s3", Some("fakequest"), Some(99), "uncommon", true);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "quest_step");
    }

    #[test]
    fn missing_desperate_option_fails() {
        let mut events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        events[1] = make_event("s2", Some("fakequest"), Some(2), "common", false);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "desperate_option");
    }

    #[test]
    fn monotonic_rarity_violation_fails() {
        let events = valid_events(&["common", "uncommon", "common", "rare", "legendary"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "monotonic");
    }

    #[test]
    fn monotonic_flat_rarity_passes() {
        let events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        validate_quests(&[quest], &events).expect("flat rarity stages are valid");
    }

    #[test]
    fn unknown_rarity_fails() {
        let events = valid_events(&["ultraweird", "common", "uncommon", "uncommon", "rare"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "relic_grant");
        assert_error_contains(validate_quests(&[quest], &events), "unrecognized rarity");
    }

    #[test]
    fn unknown_finale_kind_fails() {
        let events = valid_events(&["common", "common", "uncommon", "uncommon", "rare"]);
        let quest = make_quest(&["s1", "s2", "s3", "s4", "s5"], "banana");
        assert_error_contains(validate_quests(&[quest], &events), "unknown finale_kind");
    }

    #[test]
    fn real_registry_validates_at_import() {
        // The admitted generated catalog performs its own import/startup
        // validation. This stand-alone policy test exercises the same shape
        // with all three production quest arcs and their five-stage tags.
        let quest_specs = [
            ("agh_lost_trial", "jc_plus_guild_modifier", "agh_s"),
            ("necropolis_below", "relic_grant", "necro_s"),
            ("bolas_hidden_vault", "relic_grant", "bolas_s"),
        ];
        let mut quests = Vec::new();
        let mut events = Vec::new();
        for (quest_id, finale, prefix) in quest_specs {
            let ids: Vec<String> = (1..=5).map(|stage| format!("{prefix}{stage}")).collect();
            for (index, id) in ids.iter().enumerate() {
                events.push(make_event(
                    id,
                    Some(quest_id),
                    Some(index as i64 + 1),
                    ["common", "common", "uncommon", "uncommon", "rare"][index],
                    true,
                ));
            }
            quests.push(FakeQuest {
                id: quest_id.to_owned(),
                steps: ids,
                finale: finale.to_owned(),
            });
        }
        assert_eq!(quests.len(), 3);
        validate_quests(&quests, &events).expect("production quest shape is valid");
    }
}
