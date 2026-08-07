"""Shared rendering for lobby-suspension scope and remaining-term text.

Single source for the scope labels and term wording shown to players
(DMs, `/player lobby status`) and admins (`/admin moderation ...`), so the
copies cannot drift.
"""

from __future__ import annotations

from domain.models.lobby import LobbyKind


def _enum_value(value) -> str:
    return str(getattr(value, "value", value))


def format_suspension_scope(state) -> str:
    """Return the human label for a suspension's lobby scope."""
    scope = _enum_value(getattr(state, "scope", "all"))
    return {
        "all": "all matchmaking lobbies",
        "open": LobbyKind.OPEN.label,
        "lowskill": LobbyKind.LOWSKILL.label,
    }.get(scope, scope)


def format_suspension_terms(state, *, empty: str = "no remaining term") -> str:
    """Render a suspension's remaining term(s) joined per its completion mode.

    ``empty`` is the fallback when the state carries neither a time nor a
    matches term (callers word this differently for players vs admins).
    """
    parts: list[str] = []
    expires_at = getattr(state, "expires_at", None)
    if expires_at is not None:
        expiry = int(expires_at)
        parts.append(f"until <t:{expiry}:F> (<t:{expiry}:R>)")
    matches_remaining = getattr(state, "matches_remaining", None)
    if matches_remaining is not None:
        noun = "match" if matches_remaining == 1 else "matches"
        parts.append(f"{matches_remaining} completed {noun} remaining")
    if not parts:
        return empty
    if len(parts) == 1:
        return parts[0]
    completion = _enum_value(getattr(state, "completion", "either"))
    joiner = " or " if completion == "either" else " and "
    return joiner.join(parts)
