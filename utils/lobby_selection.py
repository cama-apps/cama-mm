"""Resolve which lobby a command should act on.

A player may queue in both lobby kinds at once, so a command that used to
read "the lobby you're in" now has three cases: one seat (infer it), two
seats (ambiguous — make the caller say), or none.
"""

from domain.models.lobby import LobbyKind

AMBIGUOUS_LOBBY_MESSAGE = (
    f"❓ You're queued in both {LobbyKind.OPEN.label} and "
    f"{LobbyKind.LOWSKILL.label} — add the `lobby` option to pick one."
)


class AmbiguousLobbyError(Exception):
    """The caller is queued in both lobbies and did not say which one to use.

    For commands where the lobby is only one branch of a priority chain
    (``/scout``, ``/herogrid``), raising lets the ambiguity surface at the
    point the lobby is actually consulted, so a caller whose answer came from
    a pending match or a live draft is never asked to disambiguate.
    """

    def __init__(self, message: str = AMBIGUOUS_LOBBY_MESSAGE):
        super().__init__(message)
        self.message = message


def format_lobby_labels(kinds) -> str:
    """Render one or more lobby kinds as prose, e.g. ``"🍽️ A and 🧀 B"``."""
    labels = [kind.label for kind in kinds]
    if len(labels) <= 1:
        return "".join(labels)
    return f"{', '.join(labels[:-1])} and {labels[-1]}"


def choice_to_lobby_kind(choice) -> LobbyKind | None:
    """Read an ``app_commands.Choice`` (or any ``.value`` carrier) as a kind.

    Returns ``None`` when the option was omitted, which callers treat as
    "infer it" rather than as :data:`LobbyKind.OPEN`.
    """
    if choice is None:
        return None
    return LobbyKind.normalize(getattr(choice, "value", choice))


def resolve_lobby_kind(
    member_kinds: list[LobbyKind],
    choice=None,
    *,
    no_lobby_message: str,
) -> tuple[LobbyKind | None, str | None]:
    """Pick the lobby to act on. Returns ``(kind, error_message)``.

    Exactly one of the two is ever set. An explicit ``choice`` always wins —
    including for lobbies the caller isn't seated in, which is what admin
    commands like ``/resetlobby`` need.
    """
    explicit = choice_to_lobby_kind(choice)
    if explicit is not None:
        return explicit, None
    if not member_kinds:
        return None, no_lobby_message
    if len(member_kinds) > 1:
        return None, AMBIGUOUS_LOBBY_MESSAGE
    return member_kinds[0], None
