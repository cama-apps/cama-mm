"""Tests for the QuestDef registry validator (validate_quests).

Quest definitions are checked at module-import time. These tests exercise
the validator function directly with fake QuestDefs + fake events so we
can assert every error path without touching the real registry.
"""

import pytest

from services.dig_constants import (
    EventChoice,
    EventOutcome,
    QuestDef,
    QuestStarterPrereq,
    RandomEvent,
    validate_quests,
)


def _make_event(
    eid: str,
    *,
    quest_id: str | None = "fakequest",
    quest_step: int | None = 1,
    rarity: str = "common",
    with_desperate: bool = True,
) -> RandomEvent:
    safe_choice = EventChoice(
        "safe", EventOutcome("ok", 0, 1, False), None, 1.0,
    )
    risky_choice = EventChoice(
        "risky",
        EventOutcome("good", 0, 5, False),
        EventOutcome("bad", -2, 0, False),
        0.5,
    )
    desperate_choice = EventChoice(
        "desperate",
        EventOutcome("great", 0, 10, False),
        EventOutcome("terrible", -5, 0, False),
        0.25,
    ) if with_desperate else None
    return RandomEvent(
        id=eid, name=eid, description=("flavor",),
        min_depth=None, max_depth=None,
        safe_option=safe_choice, risky_option=risky_choice,
        rarity=rarity,
        desperate_option=desperate_choice,
        quest_id=quest_id, quest_step=quest_step,
    )


def _make_quest(step_ids=("s1", "s2", "s3", "s4", "s5"), finale_kind="relic_grant"):
    return QuestDef(
        quest_id="fakequest",
        name="Fake Quest",
        starter_prereq=QuestStarterPrereq(),
        step_event_ids=step_ids,
        finale_kind=finale_kind,
        finale_payload={},
    )


def test_empty_quests_is_noop():
    validate_quests((), [])


def test_valid_quest_passes():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    validate_quests((_make_quest(),), events)


def test_wrong_stage_count_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 5)]
    quest = _make_quest(step_ids=("s1", "s2", "s3", "s4"))
    with pytest.raises(ValueError, match="exactly 5 stages"):
        validate_quests((quest,), events)


def test_missing_event_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 5)]  # s5 missing
    quest = _make_quest()
    with pytest.raises(ValueError, match="not found in RANDOM_EVENTS"):
        validate_quests((quest,), events)


def test_mismatched_quest_id_tag_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    events[2] = _make_event("s3", quest_id="wrong_quest", quest_step=3)
    with pytest.raises(ValueError, match="quest_id"):
        validate_quests((_make_quest(),), events)


def test_mismatched_quest_step_tag_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    events[2] = _make_event("s3", quest_step=99)
    with pytest.raises(ValueError, match="quest_step"):
        validate_quests((_make_quest(),), events)


def test_missing_desperate_option_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    events[1] = _make_event("s2", quest_step=2, with_desperate=False)
    with pytest.raises(ValueError, match="desperate_option"):
        validate_quests((_make_quest(),), events)


def test_monotonic_rarity_violation_fails():
    events = [
        _make_event("s1", quest_step=1, rarity="common"),
        _make_event("s2", quest_step=2, rarity="uncommon"),
        _make_event("s3", quest_step=3, rarity="common"),  # goes back to common — violates
        _make_event("s4", quest_step=4, rarity="rare"),
        _make_event("s5", quest_step=5, rarity="legendary"),
    ]
    with pytest.raises(ValueError, match="monotonic"):
        validate_quests((_make_quest(),), events)


def test_monotonic_flat_rarity_passes():
    """User said not every stage has to slide; same rarity twice is fine."""
    events = [
        _make_event("s1", quest_step=1, rarity="common"),
        _make_event("s2", quest_step=2, rarity="common"),
        _make_event("s3", quest_step=3, rarity="uncommon"),
        _make_event("s4", quest_step=4, rarity="uncommon"),
        _make_event("s5", quest_step=5, rarity="rare"),
    ]
    validate_quests((_make_quest(),), events)


def test_unknown_rarity_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    events[0] = _make_event("s1", quest_step=1, rarity="ultraweird")
    with pytest.raises(ValueError, match="unrecognized rarity"):
        validate_quests((_make_quest(),), events)


def test_unknown_finale_kind_fails():
    events = [_make_event(f"s{i}", quest_step=i) for i in range(1, 6)]
    quest = _make_quest(finale_kind="banana")
    with pytest.raises(ValueError, match="unknown finale_kind"):
        validate_quests((quest,), events)


def test_real_registry_validates_at_import():
    """The real QUESTS + RANDOM_EVENTS are validated when the module imports.

    This test just imports them — if anything is wrong the import would
    raise. The assertion is that we get here.
    """
    from services.dig_constants import QUESTS, RANDOM_EVENTS
    assert isinstance(QUESTS, tuple)
    assert isinstance(RANDOM_EVENTS, list)
