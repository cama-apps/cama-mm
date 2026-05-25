"""
Dota server-region preferences, inference, and aggregation.

Players pick a preferred server (US East / US West); when unset we infer one from
their OpenDota play counts. The logic is kept pure here so it can be unit-tested
without a database or network, and is shared by the service, commands, and embeds.
"""

# Region codes used in storage and the /player region command.
REGION_NAMES = {"USE": "US East", "USW": "US West"}

# OpenDota /players/{id}/counts "region" map keys -> our codes.
# OpenDota region numbers: 1 = US West, 2 = US East.
OPENDOTA_REGION_TO_CODE = {"1": "USW", "2": "USE"}

# Stored in inferred_region to mean "we checked OpenDota and found no US play".
# Distinct from NULL (= not yet checked) so the startup backfill converges to a no-op.
SENTINEL_NONE = "NONE"

# Shown on the embed when nobody in the group has a region (drives adoption).
NO_REGION_NUDGE = "No regions set — use `/player region`"


def infer_region_from_counts(counts: dict | None) -> str | None:
    """Infer ``"USE"``/``"USW"`` from an OpenDota /counts payload.

    Compares games played on US East (region ``"2"``) vs US West (region ``"1"``) and
    returns whichever is larger, regardless of magnitude — an explicit pick always
    overrides this. An exact tie leans US West (matching the lobby tie-break).

    Returns ``SENTINEL_NONE`` when a real payload shows no US play (checked, nothing to
    infer), but ``None`` when there is no payload at all (``counts is None`` — the API
    call failed or was rate-limited). The caller must treat ``None`` as "not checked yet"
    and leave the row ``NULL`` so a later run retries, rather than permanently recording
    a non-answer as "no US play".
    """
    if counts is None:
        return None
    region_counts = counts.get("region") or {}

    def games(region_key: str) -> int:
        entry = region_counts.get(region_key)
        value = entry.get("games", 0) if isinstance(entry, dict) else entry
        try:
            return int(value or 0)
        except (TypeError, ValueError):
            return 0

    usw_games = games("1")
    use_games = games("2")
    if use_games == 0 and usw_games == 0:
        return SENTINEL_NONE
    return "USE" if use_games > usw_games else "USW"  # tie -> USW


def resolve_region(player) -> str | None:
    """Return the player's effective region code, or ``None`` if they don't vote.

    The explicit pick wins over the inferred value; only real codes count, so the
    ``SENTINEL_NONE`` marker and an unset/``NULL`` value both resolve to ``None``.
    """
    for value in (getattr(player, "preferred_region", None), getattr(player, "inferred_region", None)):
        if value in REGION_NAMES:
            return value
    return None


def summarize_region(players: list) -> str:
    """Recommend a server for a group of players' region votes.

    The region with the most resolved votes wins; a tie defaults to US West (matching
    the inference tie-break). Returns the display name (e.g. ``"US East"``), or the
    adoption nudge when nobody in the group has a region.
    """
    votes = [resolve_region(p) for p in players]
    use = votes.count("USE")
    usw = votes.count("USW")
    if use == 0 and usw == 0:
        return NO_REGION_NUDGE
    return REGION_NAMES["USE" if use > usw else "USW"]  # tie -> USW
