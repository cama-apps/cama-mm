"""JOPA-T post-match protocol selection and league-safe fallback copy."""

from __future__ import annotations

import json
import random
import re
import unicodedata
from dataclasses import dataclass
from enum import Enum
from string import Formatter

from utils.neon_terminal import ansi_block


class JopatProtocol(str, Enum):
    COMBAT_TELEMETRY = "combat_telemetry"
    MARKET_SETTLEMENT = "market_settlement"
    CLIENT_ASCENSION = "client_ascension"
    PERFORMANCE_REVIEW = "performance_review"
    PATCH_DEPLOYMENT = "patch_deployment"
    COMPLIANCE_INCIDENT = "compliance_incident"
    CHAT_INGEST = "chat_ingest"


@dataclass(frozen=True)
class JopatPostMatchContext:
    winner_name: str | None = None
    loser_name: str | None = None
    hero: str | None = None
    kills: int | None = None
    deaths: int | None = None
    assists: int | None = None
    gpm: int | None = None
    xpm: int | None = None
    rating_change: float | None = None
    expected_win_prob: float | None = None
    payout: int = 0
    loss: int = 0
    leverage: int = 1
    bankruptcy_count: int = 0
    degen_score: int | None = None
    streak: int = 0

    def prompt_context(self) -> dict[str, object]:
        """Return only supplied or non-neutral facts for prompt construction."""
        facts: dict[str, object] = {}
        optional_fields = (
            "winner_name",
            "loser_name",
            "hero",
            "kills",
            "deaths",
            "assists",
            "gpm",
            "xpm",
            "rating_change",
            "expected_win_prob",
            "degen_score",
        )
        for name in optional_fields:
            value = getattr(self, name)
            if value is not None:
                facts[name] = value

        neutral_defaults = {
            "payout": 0,
            "loss": 0,
            "leverage": 1,
            "bankruptcy_count": 0,
            "streak": 0,
        }
        for name, neutral in neutral_defaults.items():
            value = getattr(self, name)
            if value != neutral:
                facts[name] = value
        return facts


@dataclass(frozen=True)
class JopatProtocolSpec:
    heading: str
    instruction: str
    hype_fallbacks: tuple[str, ...] = ()
    roast_fallbacks: tuple[str, ...] = ()
    neutral_fallbacks: tuple[str, ...] = ()

    @property
    def fallbacks(self) -> tuple[str, ...]:
        """Expose the complete copy catalog for validation and discovery."""
        return self.hype_fallbacks + self.roast_fallbacks + self.neutral_fallbacks


PROTOCOL_SPECS: dict[JopatProtocol, JopatProtocolSpec] = {
    JopatProtocol.COMBAT_TELEMETRY: JopatProtocolSpec(
        heading="COMBAT TELEMETRY",
        instruction=(
            "Issue a terse combat audit in JOPA-T's deadpan corporate voice; "
            "judge only the reported gameplay metrics."
        ),
        hype_fallbacks=(
            "Combat output accepted. Hero {hero} has been cleared of responsibility pending replay review.",
            "K/D/A {kills}/{deaths}/{assists} approved. The replay parser has upgraded skepticism to applause.",
        ),
        roast_fallbacks=(
            "Hero: {hero} | K/D/A: {kills}/{deaths}/{assists}\nEfficiency review: courage exceeded accuracy.",
            "Combat sample archived. {deaths} deaths produced {assists} assists. Conversion remains theoretical.",
            "Hero telemetry: {hero}. GPM {gpm}, XPM {xpm}. The map filed a noise complaint.",
            "Client {loser_name} completed the match with {kills} kills. Completion is the strongest available metric.",
            "K/D/A received: {kills}/{deaths}/{assists}. The slash marks are doing structural work.",
        ),
        neutral_fallbacks=(
            "Combat sample logged. Conclusions remain queued behind the replay parser.",
        ),
    ),
    JopatProtocol.MARKET_SETTLEMENT: JopatProtocolSpec(
        heading="MARKET SETTLEMENT",
        instruction=(
            "Deliver a compact settlement notice in JOPA-T's corporate-terminal voice; "
            "mock only the reported wager result."
        ),
        hype_fallbacks=(
            "Settlement posted. Payout: {payout} JC. The client briefly outperformed the house spreadsheet.",
            "Payout {payout} JC approved. Future donations to the market remain anticipated.",
        ),
        roast_fallbacks=(
            "Ledger closed. Loss: {loss} JC. Confidence remains the most expensive position.",
            "Loss {loss} JC recorded. The wager had conviction; evidence was unavailable.",
            "Account settled. The market thanks the client for confusing volatility with a plan.",
        ),
        neutral_fallbacks=(
            "Exposure: {leverage}x. Result processed without congratulating the risk model.",
        ),
    ),
    JopatProtocol.CLIENT_ASCENSION: JopatProtocolSpec(
        heading="CLIENT ASCENSION",
        instruction=(
            "Announce a narrowly verified rise in status in JOPA-T's dry corporate voice; "
            "treat success as a temporary anomaly."
        ),
        hype_fallbacks=(
            "Rating change: {rating_change}. Promotion paperwork opened; permanence not implied.",
            "Underdog probability: {expected_win_prob}. Forecast rejected by one inconvenient result.",
            "Win streak: {streak}. The correction model has been notified.",
            "Client {winner_name} ascended. Ceiling inspection is now scheduled.",
            "Performance tier increased. Sample size remains the department's preferred objection.",
            "Success confirmed. JOPA-T has provisionally upgraded the client from liability to anomaly.",
        ),
    ),
    JopatProtocol.PERFORMANCE_REVIEW: JopatProtocolSpec(
        heading="PERFORMANCE REVIEW",
        instruction=(
            "Write a short performance review in JOPA-T's unified corporate voice; "
            "criticize only supplied match performance."
        ),
        hype_fallbacks=(
            "Client {winner_name} delivered a result. Management requests fewer dramatic intermediate steps.",
            "Review approved. Objective conversion exceeded the department's preferred forecast.",
        ),
        roast_fallbacks=(
            "Employee: {loser_name} | Hero: {hero}\nReview outcome: further supervision recommended.",
            "K/D/A {kills}/{deaths}/{assists}. Meets attendance expectations; misses several others.",
            "GPM {gpm}, XPM {xpm}. Resource acquisition did not become resource usefulness.",
            "Review filed. Core competency: making routine objectives look like emergency projects.",
            "Performance accepted at the minimum viable standard. The bar has requested relocation.",
        ),
        neutral_fallbacks=(
            "Performance sample archived. Replay conclusions remain pending.",
        ),
    ),
    JopatProtocol.PATCH_DEPLOYMENT: JopatProtocolSpec(
        heading="PATCH DEPLOYMENT",
        instruction=(
            "Frame the reported gameplay as a compact software patch note in JOPA-T's "
            "deadpan terminal voice."
        ),
        hype_fallbacks=(
            "Build notes: {kills} kills, {assists} assists. Team compatibility passed field testing.",
            "Patch deployed. Decision latency improved from eventually to slightly earlier.",
        ),
        roast_fallbacks=(
            "Patch target: {hero}. Reduced confidence scaling after {deaths} field failures.",
            "Hotfix queued: convert {gpm} GPM into one timely objective.",
            "Client update staged. Removed one excuse; known issue list unchanged.",
            "Version review complete. Gameplay remains backward-compatible with disappointment.",
        ),
        neutral_fallbacks=(
            "Build archived. Compatibility conclusions remain with the replay parser.",
        ),
    ),
    JopatProtocol.COMPLIANCE_INCIDENT: JopatProtocolSpec(
        heading="COMPLIANCE INCIDENT",
        instruction=(
            "Issue a brief compliance incident report in JOPA-T's corporate gambling-terminal "
            "voice; cite only supplied betting behavior."
        ),
        hype_fallbacks=(
            "Payout {payout} JC triggered enhanced monitoring for sudden strategic confidence.",
        ),
        roast_fallbacks=(
            "Incident: {leverage}x exposure. Risk controls were present and respectfully ignored.",
            "Bankruptcy filings: {bankruptcy_count}. Client onboarding has become a recurring ceremony.",
            "Degen score: {degen_score}. Compliance recommends a strategy; client recommends another wager.",
            "Loss {loss} JC logged. Internal controls confirm the button worked exactly as pressed.",
        ),
        neutral_fallbacks=(
            "Compliance review closed. No rules broken; several lessons declined.",
        ),
    ),
    JopatProtocol.CHAT_INGEST: JopatProtocolSpec(
        heading="CHAT INGEST",
        instruction=(
            "Summarize the match as a terse chat-ingest finding in JOPA-T's single corporate "
            "identity; roast only gameplay claims."
        ),
        hype_fallbacks=(
            "Client {winner_name} has entered chat. Result-based expertise detected.",
        ),
        roast_fallbacks=(
            "Chat signal: {hero} was 'online.' Telemetry has submitted a clarification.",
            "Claim received: {kills} kills. Counterpoint received: {deaths} deaths.",
            "Post-match confidence exceeds {gpm} GPM. Analytics is investigating the spread.",
            "Client {loser_name} has supplied feedback. Replay evidence remains read-only.",
        ),
        neutral_fallbacks=(
            "Simulated chat ingest complete. Actionable information remains below threshold.",
        ),
    ),
}


def redact_post_match_prompt_context(
    facts: dict[str, object],
) -> dict[str, object]:
    """Keep player-controlled labels out of the model's instruction surface."""
    redacted = dict(facts)
    if redacted.get("winner_name"):
        redacted["winner_name"] = "[verified winner client]"
    if redacted.get("loser_name"):
        redacted["loser_name"] = "[verified loser client]"
    return redacted

_MISSING_CONTEXT_FALLBACKS: dict[JopatProtocol, str] = {
    JopatProtocol.COMBAT_TELEMETRY: "No verified combat telemetry was supplied.",
    JopatProtocol.MARKET_SETTLEMENT: "No verified settlement facts were supplied.",
    JopatProtocol.CLIENT_ASCENSION: "No verified upward movement was supplied.",
    JopatProtocol.PERFORMANCE_REVIEW: "No verified performance telemetry was supplied.",
    JopatProtocol.PATCH_DEPLOYMENT: "No verified gameplay delta was supplied.",
    JopatProtocol.COMPLIANCE_INCIDENT: "No verified wager facts were supplied.",
    JopatProtocol.CHAT_INGEST: "No verified chat or gameplay facts were supplied.",
}


def choose_protocol(
    context: JopatPostMatchContext,
    rng: random.Random | None = None,
) -> JopatProtocol:
    """Choose from protocols relevant to the facts, or generic-safe modes."""
    candidates: list[JopatProtocol] = []

    has_market_context = any(
        (
            context.payout != 0,
            context.loss != 0,
            context.leverage != 1,
            context.bankruptcy_count != 0,
            context.degen_score is not None,
        )
    )
    if has_market_context:
        candidates.extend(
            (JopatProtocol.MARKET_SETTLEMENT, JopatProtocol.COMPLIANCE_INCIDENT)
        )

    has_gameplay_context = any(
        value is not None
        for value in (
            context.hero,
            context.kills,
            context.deaths,
            context.assists,
            context.gpm,
            context.xpm,
        )
    )
    if has_gameplay_context:
        candidates.extend(
            (
                JopatProtocol.COMBAT_TELEMETRY,
                JopatProtocol.PERFORMANCE_REVIEW,
                JopatProtocol.PATCH_DEPLOYMENT,
                JopatProtocol.CHAT_INGEST,
            )
        )

    has_ascension_context = (
        (context.rating_change is not None and context.rating_change > 0)
        or (
            context.winner_name is not None
            and context.expected_win_prob is not None
            and context.expected_win_prob < 0.5
        )
        or context.streak > 0
    )
    if has_ascension_context:
        candidates.append(JopatProtocol.CLIENT_ASCENSION)

    if not candidates:
        candidates = [
            JopatProtocol.PERFORMANCE_REVIEW,
            JopatProtocol.PATCH_DEPLOYMENT,
            JopatProtocol.CHAT_INGEST,
        ]
    return (rng or random.Random()).choice(candidates)


def build_event_description(
    protocol: JopatProtocol,
    context: JopatPostMatchContext,
) -> str:
    """Build the bounded prompt input for a selected post-match protocol."""
    spec = PROTOCOL_SPECS[protocol]
    facts = redact_post_match_prompt_context(context.prompt_context())
    fact_text = json.dumps(facts, ensure_ascii=True, sort_keys=True)
    return (
        f"Protocol: {spec.heading}\n"
        f"Instruction: {spec.instruction}\n"
        f"Known facts: {fact_text}\n"
        "Use only these known facts. Do not invent heroes, stats, or outcomes."
    )


def _safe_fallback_facts(context: JopatPostMatchContext) -> dict[str, object]:
    """Keep user-controlled labels inside the ANSI block they are rendered into."""
    safe: dict[str, object] = {}
    for key, value in context.prompt_context().items():
        if isinstance(value, str):
            value = unicodedata.normalize("NFKC", value)
            value = re.sub(
                r"(?:\x1b\]|\x9d)[^\x07\x1b\x9c]*(?:\x07|\x1b\\|\x9c)",
                "",
                value,
            )
            value = re.sub(r"(?:\x1b\[|\x9b)[0-?]*[ -/]*[@-~]", "", value)
            value = "".join(
                character
                for character in value
                if unicodedata.category(character) not in {"Cc", "Cf", "Cs"}
            )
            value = " ".join(value.replace("`", "'").split())
            value = value[:80]
        elif isinstance(value, (int, float)) and not isinstance(value, bool):
            displayed = str(value)
            if len(displayed) > 32:
                value = f"{displayed[:29]}..."
        safe[key] = value
    return safe


def _fallback_pool(
    spec: JopatProtocolSpec,
    context: JopatPostMatchContext,
) -> tuple[str, ...]:
    """Select copy with tone matching the verified outcome polarity."""
    is_winner = bool(
        context.winner_name
        or context.payout > 0
        or (context.rating_change is not None and context.rating_change > 0)
        or context.streak > 0
    )
    is_loser = bool(
        context.loser_name
        or context.loss > 0
        or (context.rating_change is not None and context.rating_change < 0)
        or context.streak < 0
    )
    if is_winner and not is_loser:
        return spec.hype_fallbacks + spec.neutral_fallbacks
    if is_loser and not is_winner:
        return spec.roast_fallbacks + spec.neutral_fallbacks
    return spec.fallbacks


def _render_bounded_ansi(payload: str) -> str:
    """Leave ample room under Discord's message limit for the ANSI wrapper."""
    if len(payload) > 1800:
        payload = payload[:1797].rstrip() + "..."
    return ansi_block(payload)


def render_fallback(
    protocol: JopatProtocol,
    context: JopatPostMatchContext,
    rng: random.Random | None = None,
) -> str:
    """Render one compact, missing-safe fallback as a Discord ANSI code block."""
    spec = PROTOCOL_SPECS[protocol]
    if not context.prompt_context():
        return _render_bounded_ansi(
            f"[JOPA-T] {spec.heading}\n{_MISSING_CONTEXT_FALLBACKS[protocol]}"
        )
    facts = _safe_fallback_facts(context)
    templates = [
        template
        for template in _fallback_pool(spec, context)
        if {
            field_name
            for _literal, field_name, _format_spec, _conversion in Formatter().parse(
                template
            )
            if field_name
        }
        <= facts.keys()
    ]
    if not templates:
        return _render_bounded_ansi(
            f"[JOPA-T] {spec.heading}\n{_MISSING_CONTEXT_FALLBACKS[protocol]}"
        )
    template = (rng or random.Random()).choice(templates)
    body = template.format_map(facts)
    return _render_bounded_ansi(f"[JOPA-T] {spec.heading}\n{body}")
