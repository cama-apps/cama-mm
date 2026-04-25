"""Splash event resolver for the dig minigame.

When a tagged dig event fires its splash outcome, jopacoin is either
**burned** from a pool of other players in the guild (default, a true
deflation lever since coins are destroyed not transferred),
**granted** to a cooperative target (positive splash, e.g. the Io
tether pact sharing spoils with a partner), or **stolen** from victims
and transferred to the digger (zero-sum, via ``steal_atomic``).

Three victim pools are supported (see :class:`SplashConfig`):

* ``random_active``  - random sample of recently-active guild members
* ``richest_n``      - top-N positive-balance players (anti-whale)
* ``active_diggers`` - random sample of players who have dug in the
  last ``active_diggers_days`` days

For ``mode="burn"`` the resolver clamps each victim's debit so
non-debtors cannot be pushed below zero. For ``mode="grant"`` the
credit is unclamped (adds JC to the recipient). For ``mode="steal"``
the resolver calls ``player_repo.steal_atomic`` per victim, which
transfers the amount to the digger and may push the victim below 0
down to ``MAX_DEBT`` (intentional, matches Red/Blue Shell semantics).
Every actual balance change is recorded as a ``splash_victim`` row in
``dig_actions`` for auditing.
"""

from __future__ import annotations

import json
import logging
import random
from dataclasses import dataclass

logger = logging.getLogger("cama_bot.services.dig_splash")

ACTIVE_DIGGERS_LOOKBACK_DAYS = 7


@dataclass(frozen=True)
class SplashResult:
    """Outcome of a splash event.

    ``victims`` is a list of ``(discord_id, amount)`` tuples. For
    ``mode="burn"`` amount is the positive integer actually debited;
    for ``mode="grant"`` it is the positive integer credited; for
    ``mode="steal"`` it is the positive integer transferred from the
    victim to the digger.
    ``total_burned`` is the sum (regardless of mode — it's the
    magnitude moved). ``mode`` is copied in so the broadcast layer
    can render "burned", "shared", or "stolen" flavor.
    """

    strategy: str
    event_name: str
    victims: list[tuple[int, int]]
    total_burned: int
    mode: str = "burn"


def _select_random_active(repos, guild_id: int, digger_id: int, count: int) -> list[int]:
    players = repos.player_repo.get_all_registered_players_for_lottery(guild_id)
    pool = [p["discord_id"] for p in players if p["discord_id"] != digger_id]
    if not pool:
        return []
    return random.sample(pool, min(count, len(pool)))


def _select_richest_n(repos, guild_id: int, digger_id: int, count: int) -> list[int]:
    players = repos.player_repo.get_richest_players(guild_id, limit=count + 1)
    return [p["discord_id"] for p in players if p["discord_id"] != digger_id][:count]


def _select_active_diggers(repos, guild_id: int, digger_id: int, count: int) -> list[int]:
    # Over-fetch enough to keep the local sample uniform but bounded; an
    # unbounded pull could return thousands of distinct actors in a
    # long-running guild.
    pool_cap = max(count * 4, count + 8)
    ids = repos.dig_repo.get_recent_diggers(
        guild_id,
        days=ACTIVE_DIGGERS_LOOKBACK_DAYS,
        exclude_id=digger_id,
        limit=pool_cap,
    )
    if not ids:
        return []
    return random.sample(ids, min(count, len(ids)))


_SELECTORS = {
    "random_active": _select_random_active,
    "richest_n": _select_richest_n,
    "active_diggers": _select_active_diggers,
}


class _ReposBundle:
    """Tiny adapter so resolve_splash works with either a DigService or
    a raw pair of repos."""

    __slots__ = ("player_repo", "dig_repo")

    def __init__(self, player_repo, dig_repo):
        self.player_repo = player_repo
        self.dig_repo = dig_repo


def resolve_splash(
    *,
    player_repo,
    dig_repo,
    guild_id: int,
    digger_id: int,
    event_name: str,
    strategy: str,
    victim_count: int,
    penalty_jc: int,
    mode: str = "burn",
) -> SplashResult:
    """Select targets and move JC — burn from each (default) or grant to each.

    Returns a :class:`SplashResult` with the actual amount moved per target.
    In ``burn`` mode debits are clamped to the target's balance so
    non-debtors stay >= 0; debtors are skipped. In ``grant`` mode each
    target is credited unconditionally. On an empty pool or invalid
    strategy the returned result has an empty ``victims`` list and
    ``total_burned=0``.
    """
    selector = _SELECTORS.get(strategy)
    if selector is None:
        logger.warning("Unknown splash strategy %r for event %r", strategy, event_name)
        return SplashResult(
            strategy=strategy, event_name=event_name, victims=[],
            total_burned=0, mode=mode,
        )

    if victim_count <= 0 or penalty_jc <= 0:
        return SplashResult(
            strategy=strategy, event_name=event_name, victims=[],
            total_burned=0, mode=mode,
        )

    repos = _ReposBundle(player_repo, dig_repo)
    victim_ids = selector(repos, guild_id, digger_id, victim_count)

    audit_detail = json.dumps({
        "event_name": event_name,
        "strategy": strategy,
        "digger_id": digger_id,
        "penalty_requested": penalty_jc,
        "mode": mode,
    })

    victims: list[tuple[int, int]] = []
    for vid in victim_ids:
        if mode == "grant":
            actual = int(penalty_jc)
            player_repo.add_balance(vid, guild_id, actual)
            dig_repo.log_action(
                discord_id=vid,
                guild_id=guild_id,
                action_type="splash_victim",
                jc_delta=actual,
                details=audit_detail,
            )
            victims.append((vid, actual))
            continue
        if mode == "steal":
            actual = int(penalty_jc)
            try:
                player_repo.steal_atomic(
                    thief_discord_id=digger_id,
                    victim_discord_id=vid,
                    guild_id=guild_id,
                    amount=actual,
                )
            except ValueError:
                logger.exception(
                    "Splash steal: steal_atomic failed for victim %s in guild %s", vid, guild_id,
                )
                continue
            dig_repo.log_action(
                discord_id=vid,
                guild_id=guild_id,
                action_type="splash_victim",
                jc_delta=-actual,
                details=audit_detail,
            )
            # Also log the digger-side credit so the audit trail attributes the
            # JC the thief gained to this splash event (steal_atomic itself only
            # touches balances).
            dig_repo.log_action(
                discord_id=digger_id,
                guild_id=guild_id,
                action_type="splash_thief",
                jc_delta=actual,
                details=audit_detail,
            )
            victims.append((vid, actual))
            continue
        try:
            current_balance = player_repo.get_balance(vid, guild_id)
        except Exception:
            logger.exception("Splash: get_balance failed for victim %s in guild %s", vid, guild_id)
            continue
        if current_balance <= 0:
            continue
        actual = int(min(penalty_jc, current_balance))
        if actual <= 0:
            continue
        player_repo.add_balance(vid, guild_id, -actual)
        dig_repo.log_action(
            discord_id=vid,
            guild_id=guild_id,
            action_type="splash_victim",
            jc_delta=-actual,
            details=audit_detail,
        )
        victims.append((vid, actual))

    total = sum(amount for _, amount in victims)
    return SplashResult(
        strategy=strategy, event_name=event_name, victims=victims,
        total_burned=total, mode=mode,
    )
