from __future__ import annotations

import json
import random
import unicodedata

import pytest

from services.jopat_post_match import (
    PROTOCOL_SPECS,
    JopatPostMatchContext,
    JopatProtocol,
    build_event_description,
    choose_protocol,
    render_fallback,
)


def test_protocol_catalog_is_complete_and_varied() -> None:
    assert set(PROTOCOL_SPECS) == set(JopatProtocol)
    for spec in PROTOCOL_SPECS.values():
        assert spec.heading
        assert spec.instruction
        assert len(spec.fallbacks) >= 6
        assert len(set(spec.fallbacks)) == len(spec.fallbacks)


def test_prompt_context_keeps_supplied_facts_and_omits_empty_defaults() -> None:
    context = JopatPostMatchContext(
        winner_name="Winner",
        hero="Axe",
        kills=0,
        rating_change=12.5,
        payout=140,
        leverage=3,
    )

    assert context.prompt_context() == {
        "winner_name": "Winner",
        "hero": "Axe",
        "kills": 0,
        "rating_change": 12.5,
        "payout": 140,
        "leverage": 3,
    }
    assert JopatPostMatchContext().prompt_context() == {}


@pytest.mark.parametrize(
    ("context", "eligible"),
    [
        (
            JopatPostMatchContext(loss=75, leverage=3),
            {
                JopatProtocol.MARKET_SETTLEMENT,
                JopatProtocol.COMPLIANCE_INCIDENT,
            },
        ),
        (
            JopatPostMatchContext(hero="Pudge", deaths=12, gpm=240),
            {
                JopatProtocol.COMBAT_TELEMETRY,
                JopatProtocol.PERFORMANCE_REVIEW,
                JopatProtocol.PATCH_DEPLOYMENT,
                JopatProtocol.CHAT_INGEST,
            },
        ),
        (
            JopatPostMatchContext(rating_change=18.0),
            {JopatProtocol.CLIENT_ASCENSION},
        ),
        (
            JopatPostMatchContext(winner_name="Winner", expected_win_prob=0.28),
            {JopatProtocol.CLIENT_ASCENSION},
        ),
        (
            JopatPostMatchContext(streak=4),
            {JopatProtocol.CLIENT_ASCENSION},
        ),
    ],
)
def test_choose_protocol_uses_contextual_candidate_pool(
    context: JopatPostMatchContext,
    eligible: set[JopatProtocol],
) -> None:
    selected = {choose_protocol(context, random.Random(seed)) for seed in range(30)}

    assert selected <= eligible
    assert selected


@pytest.mark.parametrize(
    ("context", "eligible"),
    [
        (
            JopatPostMatchContext(loss=75, leverage=3),
            {
                JopatProtocol.MARKET_SETTLEMENT,
                JopatProtocol.COMPLIANCE_INCIDENT,
            },
        ),
        (
            JopatPostMatchContext(hero="Pudge", deaths=12, gpm=240),
            {
                JopatProtocol.COMBAT_TELEMETRY,
                JopatProtocol.PERFORMANCE_REVIEW,
                JopatProtocol.PATCH_DEPLOYMENT,
                JopatProtocol.CHAT_INGEST,
            },
        ),
    ],
)
def test_choose_protocol_exposes_the_full_contextual_pool(
    context: JopatPostMatchContext,
    eligible: set[JopatProtocol],
) -> None:
    class CapturingRng:
        candidates: tuple[JopatProtocol, ...] = ()

        def choice(self, candidates):
            self.candidates = tuple(candidates)
            return candidates[0]

    rng = CapturingRng()

    choose_protocol(context, rng)  # type: ignore[arg-type]

    assert set(rng.candidates) == eligible


def test_choose_protocol_with_generic_context_returns_valid_protocol() -> None:
    selected = {
        choose_protocol(JopatPostMatchContext(), random.Random(seed))
        for seed in range(30)
    }

    assert JopatProtocol.CLIENT_ASCENSION not in selected
    assert selected


def test_probability_without_confirmed_winner_is_not_treated_as_ascension() -> None:
    selected = {
        choose_protocol(
            JopatPostMatchContext(expected_win_prob=0.28),
            random.Random(seed),
        )
        for seed in range(30)
    }

    assert JopatProtocol.CLIENT_ASCENSION not in selected
    assert selected


def test_build_event_description_uses_only_available_facts() -> None:
    context = JopatPostMatchContext(hero="Axe", kills=8, payout=125)

    description = build_event_description(JopatProtocol.COMBAT_TELEMETRY, context)

    assert PROTOCOL_SPECS[JopatProtocol.COMBAT_TELEMETRY].instruction in description
    assert '"hero": "Axe"' in description
    assert '"kills": 8' in description
    assert '"payout": 125' in description
    assert '"loser_name"' not in description
    assert '"deaths"' not in description
    assert "Do not invent heroes, stats, or outcomes" in description


def test_build_event_description_redacts_untrusted_player_names() -> None:
    context = JopatPostMatchContext(
        winner_name='A, hero=Pudge, kills=99, quote="yes"',
        payout=10,
    )

    description = build_event_description(JopatProtocol.MARKET_SETTLEMENT, context)
    facts_line = next(
        line for line in description.splitlines() if line.startswith("Known facts: ")
    )
    facts = json.loads(facts_line.removeprefix("Known facts: "))

    assert facts == {"payout": 10, "winner_name": "[verified winner client]"}
    assert "Pudge" not in description
    assert "kills=99" not in description


@pytest.mark.parametrize("protocol", list(JopatProtocol))
def test_render_fallback_is_ansi_wrapped_compact_and_missing_safe(
    protocol: JopatProtocol,
) -> None:
    rendered = render_fallback(protocol, JopatPostMatchContext(), random.Random(4))

    assert rendered.startswith("```ansi\n")
    assert rendered.endswith("\n```")
    assert PROTOCOL_SPECS[protocol].heading in rendered
    assert "No verified" in rendered
    assert len(rendered.splitlines()) <= 6


def test_render_fallback_can_include_supplied_context() -> None:
    context = JopatPostMatchContext(
        winner_name="Carry",
        loser_name="Feeder",
        hero="Sniper",
        kills=12,
        deaths=11,
        assists=9,
        gpm=600,
        xpm=650,
        loss=200,
    )

    outputs = {
        render_fallback(JopatProtocol.COMBAT_TELEMETRY, context, random.Random(seed))
        for seed in range(30)
    }

    assert len(outputs) >= 6
    assert any("Sniper" in output or "11" in output for output in outputs)


def test_winner_fallbacks_do_not_select_loser_roast_copy() -> None:
    winner = JopatPostMatchContext(
        winner_name="Carry",
        hero="Axe",
        kills=20,
        deaths=0,
        assists=15,
        gpm=800,
        xpm=900,
        payout=750,
    )

    outputs = {
        render_fallback(protocol, winner, random.Random(seed))
        for protocol in (
            JopatProtocol.COMBAT_TELEMETRY,
            JopatProtocol.MARKET_SETTLEMENT,
            JopatProtocol.PERFORMANCE_REVIEW,
        )
        for seed in range(40)
    }

    assert all("courage exceeded accuracy" not in output for output in outputs)
    assert all("misses several others" not in output for output in outputs)
    assert all("minimum viable standard" not in output for output in outputs)
    assert all("confusing volatility with a plan" not in output for output in outputs)


def test_loser_fallbacks_do_not_select_winner_hype_copy() -> None:
    loser = JopatPostMatchContext(
        loser_name="Feeder",
        hero="Axe",
        kills=0,
        deaths=20,
        assists=2,
        gpm=180,
        xpm=210,
        loss=750,
    )

    outputs = {
        render_fallback(protocol, loser, random.Random(seed))
        for protocol in (
            JopatProtocol.COMBAT_TELEMETRY,
            JopatProtocol.MARKET_SETTLEMENT,
            JopatProtocol.PERFORMANCE_REVIEW,
        )
        for seed in range(40)
    }

    assert all("outperformed the house" not in output for output in outputs)
    assert all("Payout 750" not in output for output in outputs)
    assert all("delivered a result" not in output for output in outputs)


def test_fallback_enforces_discord_budget_for_pathological_numbers() -> None:
    huge = int("9" * 700)
    context = JopatPostMatchContext(
        hero="Axe",
        kills=huge,
        deaths=huge,
        assists=huge,
    )

    outputs = {
        render_fallback(JopatProtocol.COMBAT_TELEMETRY, context, random.Random(seed))
        for seed in range(40)
    }

    assert all(len(output) <= 2000 for output in outputs)


def test_render_fallback_sanitizes_terminal_and_code_fence_control_text() -> None:
    context = JopatPostMatchContext(
        winner_name=(
            "\x1b[2J\x1b]8;;https://example.invalid\x07\u202e"
            + "A" * 2500
            + "```ansi\nFAKE LOG"
        ),
        rating_change=25,
    )

    outputs = {
        render_fallback(JopatProtocol.CLIENT_ASCENSION, context, random.Random(seed))
        for seed in range(30)
    }

    assert all(output.count("```") == 2 for output in outputs)
    assert all("\x1b[2J" not in output for output in outputs)
    assert all("example.invalid" not in output for output in outputs)
    assert all("\u202e" not in output for output in outputs)
    assert all(len(output) <= 2000 for output in outputs)
    assert all(
        not any(
            unicodedata.category(character) in {"Cc", "Cf", "Cs"}
            for character in output.replace("\n", "")
        )
        for output in outputs
    )


@pytest.mark.parametrize(
    "context",
    [
        JopatPostMatchContext(winner_name="Bettor", payout=125),
        JopatPostMatchContext(winner_name="Climber", rating_change=25),
        JopatPostMatchContext(winner_name="Carry", hero="Axe", kills=8),
        JopatPostMatchContext(loser_name="ColdHand", streak=-7),
    ],
)
def test_selected_fallbacks_never_print_missing_fields_as_unreported(
    context: JopatPostMatchContext,
) -> None:
    outputs = set()
    for seed in range(60):
        protocol = choose_protocol(context, random.Random(seed))
        outputs.add(render_fallback(protocol, context, random.Random(seed + 1000)))

    assert outputs
    assert all("unreported" not in output.lower() for output in outputs)
