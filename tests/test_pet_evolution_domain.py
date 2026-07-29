"""Pure-domain coverage for Camagotchi upbringing and adult callings."""

from __future__ import annotations

import pytest

from domain.pet_evolution import (
    CALLING_PROFILES,
    PetActivity,
    PetCalling,
    PetInstinct,
    activity_rule,
    calling_profile,
    hint_key_for_scores,
    hint_text,
    resolve_evolution,
)


def scores(**overrides: int) -> dict[PetInstinct, int]:
    values = dict.fromkeys(PetInstinct, 0)
    values.update({PetInstinct(key): value for key, value in overrides.items()})
    return values


@pytest.mark.parametrize(
    ("instinct", "calling"),
    [
        (PetInstinct.DELVING, PetCalling.DELVER),
        (PetInstinct.COMPETITION, PetCalling.CHAMPION),
        (PetInstinct.FORTUNE, PetCalling.TRICKSTER),
        (PetInstinct.FELLOWSHIP, PetCalling.COMPANION),
        (PetInstinct.WISDOM, PetCalling.SAGE),
    ],
)
def test_one_dominant_instinct_resolves_to_its_pure_calling(instinct, calling):
    upbringing = scores()
    upbringing[instinct] = 10
    upbringing[PetInstinct.FELLOWSHIP if instinct is not PetInstinct.FELLOWSHIP else PetInstinct.DELVING] = 4

    decision = resolve_evolution(upbringing, pet_id=101)

    assert decision.calling is calling
    assert decision.primary is instinct
    assert decision.secondary is None


@pytest.mark.parametrize(
    ("left", "right", "calling"),
    [
        (PetInstinct.DELVING, PetInstinct.COMPETITION, PetCalling.PITFIGHTER),
        (PetInstinct.DELVING, PetInstinct.FORTUNE, PetCalling.PROSPECTOR),
        (PetInstinct.DELVING, PetInstinct.FELLOWSHIP, PetCalling.TRAILKEEPER),
        (PetInstinct.DELVING, PetInstinct.WISDOM, PetCalling.ARCHAEOLOGIST),
        (PetInstinct.COMPETITION, PetInstinct.FORTUNE, PetCalling.DAREDEVIL),
        (PetInstinct.COMPETITION, PetInstinct.FELLOWSHIP, PetCalling.GUARDIAN),
        (PetInstinct.COMPETITION, PetInstinct.WISDOM, PetCalling.TACTICIAN),
        (PetInstinct.FORTUNE, PetInstinct.FELLOWSHIP, PetCalling.CHARMER),
        (PetInstinct.FORTUNE, PetInstinct.WISDOM, PetCalling.ORACLE),
        (PetInstinct.FELLOWSHIP, PetInstinct.WISDOM, PetCalling.MENTOR),
    ],
)
def test_two_substantial_instincts_resolve_to_their_hybrid_calling(
    left, right, calling
):
    upbringing = scores()
    upbringing[left] = 10
    upbringing[right] = 6

    decision = resolve_evolution(upbringing, pet_id=202)

    assert decision.calling is calling
    assert {decision.primary, decision.secondary} == {left, right}


def test_no_meaningful_activity_resolves_to_wayfarer():
    assert resolve_evolution(scores(), pet_id=1).calling is PetCalling.WAYFARER
    assert (
        resolve_evolution(scores(delving=2), pet_id=1).calling
        is PetCalling.WAYFARER
    )


def test_three_balanced_instincts_resolve_to_wayfarer():
    decision = resolve_evolution(
        scores(delving=8, competition=7, wisdom=6),
        pet_id=303,
    )

    assert decision.calling is PetCalling.WAYFARER
    assert decision.primary is None
    assert decision.secondary is None


def test_exact_two_way_tie_is_stable_and_remains_a_hybrid():
    upbringing = scores(delving=6, fortune=6)

    first = resolve_evolution(upbringing, pet_id=404)
    retried = resolve_evolution(upbringing, pet_id=404)

    assert first == retried
    assert first.calling is PetCalling.PROSPECTOR
    assert {first.primary, first.secondary} == {
        PetInstinct.DELVING,
        PetInstinct.FORTUNE,
    }


@pytest.mark.parametrize(
    ("activity", "instinct", "points", "once_per_day"),
    [
        (PetActivity.DIG_COMPLETED, PetInstinct.DELVING, 2, False),
        (PetActivity.MATCH_RECORDED, PetInstinct.COMPETITION, 2, False),
        (PetActivity.PET_BRAWL_COMPLETED, PetInstinct.COMPETITION, 2, False),
        (PetActivity.DUEL_COMPLETED, PetInstinct.COMPETITION, 2, False),
        (PetActivity.WHEEL_SPUN, PetInstinct.FORTUNE, 2, False),
        (PetActivity.BET_PLACED, PetInstinct.FORTUNE, 2, False),
        (PetActivity.PREDICTION_TRADED, PetInstinct.FORTUNE, 2, False),
        (PetActivity.PET_CARED, PetInstinct.FELLOWSHIP, 1, True),
        (PetActivity.TIP_SENT, PetInstinct.FELLOWSHIP, 2, False),
        (PetActivity.TRIVIA_COMPLETED, PetInstinct.WISDOM, 2, False),
    ],
)
def test_activity_rules_are_normalized_by_participation(
    activity, instinct, points, once_per_day
):
    rule = activity_rule(activity)

    assert rule.instinct is instinct
    assert rule.points == points
    assert rule.once_per_day is once_per_day


def test_hints_stay_hidden_until_activity_is_meaningful():
    assert hint_key_for_scores(scores(delving=2)) is None
    assert hint_key_for_scores(scores(delving=3)) == "delving"
    assert hint_key_for_scores(scores(delving=8, competition=1)) == "delving_strong"
    assert (
        hint_key_for_scores(scores(delving=8, competition=7, wisdom=6))
        == "balanced"
    )


def test_every_calling_has_distinct_player_facing_identity():
    assert set(CALLING_PROFILES) == set(PetCalling)
    labels = {profile.label for profile in CALLING_PROFILES.values()}
    assert len(labels) == len(PetCalling)
    for calling in PetCalling:
        profile = calling_profile(calling)
        assert profile.label
        assert profile.emoji
        assert profile.blurb
        assert profile.color > 0


@pytest.mark.parametrize(
    "key",
    [
        "balanced",
        "delving",
        "delving_strong",
        "competition",
        "competition_strong",
        "fortune",
        "fortune_strong",
        "fellowship",
        "fellowship_strong",
        "wisdom",
        "wisdom_strong",
    ],
)
def test_upbringing_hints_are_narrative_and_hide_scores(key):
    text = hint_text(key)
    assert text
    assert not any(character.isdigit() for character in text)
