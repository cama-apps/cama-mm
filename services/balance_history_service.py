"""Unified jopacoin balance-history series for the /profile Economy tab chart."""

from __future__ import annotations

import json
from dataclasses import dataclass, field

from config import JOPACOIN_PER_GAME, JOPACOIN_WIN_REWARD

# Source identifiers — must stay in sync with ``utils/drawing/balance_history.py``.
SOURCE_BETS = "bets"
SOURCE_PREDICTIONS = "predictions"
SOURCE_WHEEL = "wheel"
SOURCE_DOUBLE_OR_NOTHING = "double_or_nothing"
SOURCE_TIPS = "tips"
SOURCE_DISBURSE = "disburse"
SOURCE_BONUS = "bonus"
SOURCE_DIG = "dig"


@dataclass
class _Event:
    time: int
    delta: int
    source: str
    detail: dict = field(default_factory=dict)


class BalanceHistoryService:
    """Merge all persisted balance-impacting events for a player into one series.

    Eight sources are chartable today:
      - ``bets``              settled bets (profit)
      - ``predictions``       resolved prediction bets (payout − staked)
      - ``wheel``             Wheel of Fortune spins (result)
      - ``double_or_nothing`` Double or Nothing spins (balance_after − (balance_before + cost))
      - ``tips``              sent (-(amount+fee)) and received (+amount)
      - ``disburse``          nonprofit disbursements received (+amount)
      - ``bonus``             per-match participation + win bonus reconstruction
      - ``dig``               dig actions: earnings, event JC, boss fight wins/losses,
                              abandon refunds, retreat losses, sabotage stake/trap-steal,
                              help rewards (parsed from the ``dig_actions.detail`` JSON)

    Un-persisted sources (shop, admin /givecoin, bankruptcy penalty, garnishment,
    streaming / first-game / exclusion / bomb-pot bonuses, cancelled-match refunds,
    dig cave-in losses, per-dig paid costs) are silently omitted — the series starts
    at 0 and does not pretend to equal live balance.
    """

    def __init__(
        self,
        bet_repo,
        match_repo,
        player_repo,
        prediction_repo,
        disburse_repo,
        tip_repo,
        dig_repo=None,
    ):
        self.bet_repo = bet_repo
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.prediction_repo = prediction_repo
        self.disburse_repo = disburse_repo
        self.tip_repo = tip_repo
        self.dig_repo = dig_repo

    def get_balance_event_series(
        self, discord_id: int, guild_id: int | None = None
    ) -> tuple[list[tuple[int, int, dict]], dict[str, int]]:
        """
        Return ``(series, per_source_totals)``.

        ``series``: list of ``(event_number, cumulative_delta, event_info)`` starting
        at ``event_number=1`` and ``cumulative_delta`` starting at 0. Sorted by time.

        ``event_info``: ``{"time": int, "delta": int, "source": str, "detail": dict}``.

        ``per_source_totals``: ``{source: net_delta}`` for sources with a non-zero
        total. Matches are empty if the player has no recorded activity anywhere.
        """
        events: list[_Event] = []
        events.extend(self._bet_events(discord_id, guild_id))
        events.extend(self._prediction_events(discord_id, guild_id))
        events.extend(self._wheel_events(discord_id, guild_id))
        events.extend(self._double_or_nothing_events(discord_id, guild_id))
        events.extend(self._tip_events(discord_id, guild_id))
        events.extend(self._disburse_events(discord_id, guild_id))
        events.extend(self._bonus_events(discord_id, guild_id))
        events.extend(self._dig_events(discord_id, guild_id))

        if not events:
            return [], {}

        events.sort(key=lambda e: e.time)

        series: list[tuple[int, int, dict]] = []
        totals: dict[str, int] = {}
        cumulative = 0
        for idx, ev in enumerate(events, start=1):
            cumulative += ev.delta
            series.append(
                (
                    idx,
                    cumulative,
                    {
                        "time": ev.time,
                        "delta": ev.delta,
                        "source": ev.source,
                        "detail": ev.detail,
                    },
                )
            )
            totals[ev.source] = totals.get(ev.source, 0) + ev.delta

        per_source_totals = {src: total for src, total in totals.items() if total != 0}
        return series, per_source_totals

    # ── Per-source collectors ────────────────────────────────────────────────

    def _bet_events(self, discord_id: int, guild_id: int | None) -> list[_Event]:
        rows = self.bet_repo.get_player_bet_history(discord_id, guild_id)
        return [
            _Event(
                time=int(row["bet_time"]),
                delta=int(row["profit"]),
                source=SOURCE_BETS,
                detail={
                    "outcome": row["outcome"],
                    "amount": row["amount"],
                    "leverage": row["leverage"],
                    "match_id": row["match_id"],
                },
            )
            for row in rows
        ]

    def _prediction_events(
        self, discord_id: int, guild_id: int | None
    ) -> list[_Event]:
        rows = self.prediction_repo.get_player_prediction_history(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            staked = int(row["total_amount"] or 0)
            payout = int(row["payout"] or 0)
            delta = payout - staked
            # Drop perfect net-zero events so they don't clutter the chart
            # (cancelled predictions with full refund fall out this way).
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(row["settle_time"] or 0),
                    delta=delta,
                    source=SOURCE_PREDICTIONS,
                    detail={
                        "outcome": "won" if delta > 0 else "lost",
                        "position": row["position"],
                        "status": row["status"],
                        "prediction_id": row["prediction_id"],
                    },
                )
            )
        return out

    def _wheel_events(self, discord_id: int, guild_id: int | None) -> list[_Event]:
        rows = self.player_repo.get_wheel_spin_history(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            result = int(row["result"])
            if result == 0:
                continue  # "lose a turn" — no balance change, skip
            out.append(
                _Event(
                    time=int(row["spin_time"]),
                    delta=result,
                    source=SOURCE_WHEEL,
                    detail={"outcome": "won" if result > 0 else "lost"},
                )
            )
        return out

    def _double_or_nothing_events(
        self, discord_id: int, guild_id: int | None
    ) -> list[_Event]:
        # The repo method's annotation says ``guild_id: int`` but it routes through
        # ``normalize_guild_id`` which accepts None, matching every other repo here.
        rows = self.player_repo.get_double_or_nothing_history(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            balance_before = int(row["balance_before"])
            balance_after = int(row["balance_after"])
            cost = int(row["cost"])
            # Original balance = balance_before + cost (cost was already deducted).
            original = balance_before + cost
            delta = balance_after - original
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(row["spin_time"]),
                    delta=delta,
                    source=SOURCE_DOUBLE_OR_NOTHING,
                    detail={
                        "outcome": "won" if bool(row["won"]) else "lost",
                        "risked": balance_before,
                    },
                )
            )
        return out

    def _tip_events(self, discord_id: int, guild_id: int | None) -> list[_Event]:
        rows = self.tip_repo.get_all_tips_for_user(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            amount = int(row["amount"])
            fee = int(row["fee"] or 0)
            direction = row["direction"]
            # Self-tips (sender == recipient) collapse the two sides into one row,
            # so the balance only moves by the fee.
            if row["sender_id"] == row["recipient_id"]:
                delta = -fee
            elif direction == "sent":
                delta = -(amount + fee)
            else:  # received
                delta = amount
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(row["timestamp"]),
                    delta=delta,
                    source=SOURCE_TIPS,
                    detail={"direction": direction, "amount": amount, "fee": fee},
                )
            )
        return out

    def _disburse_events(
        self, discord_id: int, guild_id: int | None
    ) -> list[_Event]:
        rows = self.disburse_repo.get_recipient_history(discord_id, guild_id)
        return [
            _Event(
                time=int(row["disbursed_at"]),
                delta=int(row["amount"]),
                source=SOURCE_DISBURSE,
                detail={"method": row["method"]},
            )
            for row in rows
            if int(row["amount"]) != 0
        ]

    def _bonus_events(self, discord_id: int, guild_id: int | None) -> list[_Event]:
        rows = self.match_repo.get_player_bonus_events(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            match_time = row.get("match_time")
            if match_time is None:
                continue
            persisted = row.get("bonus_jc")
            if persisted is not None:
                # Use the actual net JC credited at match time so the chart
                # reflects garnishment / bankruptcy penalties accurately.
                delta = int(persisted)
                detail_components = {"persisted_net": delta}
            else:
                # Pre-migration rows fall back to current-config reconstruction
                # (loser gets participation, winner gets win-bonus only).
                participation = 0 if row["won"] else JOPACOIN_PER_GAME
                win = JOPACOIN_WIN_REWARD if row["won"] else 0
                delta = participation + win
                detail_components = {"participation": participation, "win": win}
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(match_time),
                    delta=delta,
                    source=SOURCE_BONUS,
                    detail={
                        "match_id": row["match_id"],
                        "components": detail_components,
                        "won": row["won"],
                    },
                )
            )
        return out

    def _dig_events(self, discord_id: int, guild_id: int | None) -> list[_Event]:
        """Derive per-action JC deltas from the ``dig_actions`` detail JSON.

        The ``dig_actions.jc_delta`` column is honoured if populated (future-proofing
        — no code currently writes it). Otherwise we parse ``detail`` per action
        type. Known gaps (documented in the class docstring): cave-in JC losses,
        per-dig paid costs, sabotage target-side gains beyond trap-triggered returns.
        """
        if self.dig_repo is None:
            return []
        rows = self.dig_repo.get_player_jc_events(discord_id, guild_id)
        out: list[_Event] = []
        for row in rows:
            delta = _dig_row_delta(discord_id, row)
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(row["created_at"]),
                    delta=delta,
                    source=SOURCE_DIG,
                    detail={"action_type": row["action_type"]},
                )
            )
        return out


def _dig_row_delta(discord_id: int, row: dict) -> int:
    """Best-effort extraction of a user's JC delta from one ``dig_actions`` row.

    Returns 0 when no reconstructable delta applies (unknown action type, cave-in
    dig log, target-side sabotage without trap, etc.).
    """
    is_actor = row["actor_id"] == discord_id
    is_target = row["target_id"] == discord_id

    column_delta = row.get("jc_delta") or 0
    if column_delta and is_actor:
        return int(column_delta)

    try:
        detail = json.loads(row["detail"] or "{}")
    except (json.JSONDecodeError, TypeError):
        return 0

    action = row["action_type"]

    if action == "dig" and is_actor:
        # Regular dig: ``detail.jc`` is positive earnings. Cave-in logs don't set
        # this key — the JC loss there is a separate ``add_balance`` call that
        # isn't persisted anywhere, so it's a silent gap.
        return int(detail.get("jc", 0) or 0)

    if action == "help" and is_actor:
        # Hardcoded +1 JC to the helper in ``dig_service.help_tunnel``.
        return 1

    if action == "sabotage":
        trap_triggered = bool(detail.get("trap_triggered", False))
        if is_actor:
            if trap_triggered:
                return -int(detail.get("jc_lost", 0) or 0)
            return -int(detail.get("cost", 0) or 0)
        if is_target and trap_triggered:
            # Trap-triggered sabotage: actor loses ``trap_steal = cost * 2`` and the
            # target is refunded ``cost`` (the attacker's stake). So the target's
            # gain is ``jc_lost / 2``. Non-trap sabotage doesn't move the target's
            # JC at all (only blocks), which correctly returns 0 below.
            return int(detail.get("jc_lost", 0) or 0) // 2
        return 0

    if action == "boss_fight" and is_actor:
        if detail.get("won"):
            # ``detail.jc_delta`` is the *gross* payout. For wagered wins the wager
            # is never debited elsewhere, so the credited amount is ``gross - wager``
            # (see dig_service line 3608-3614). Non-wagered wins credit the full
            # random roll. Phase-1 transitions log no ``jc_delta`` → correctly 0.
            gross = int(detail.get("jc_delta", 0) or 0)
            wager = int(detail.get("wager", 0) or 0)
            if wager > 0 and gross > 0:
                return gross - wager
            return gross
        # Loss: ``add_balance(-wager)`` is issued at loss time alongside the log,
        # so the wager is the negative delta.
        return -int(detail.get("wager", 0) or 0)

    if action == "boss_retreat" and is_actor:
        return -int(detail.get("loss", 0) or 0)

    if action == "event" and is_actor:
        # ``detail.jc_delta`` (numeric events), ``detail.jc`` (success/fail events),
        # or ``cruel_echoes`` flag (-1).
        if detail.get("cruel_echoes"):
            return -1
        if "jc_delta" in detail:
            return int(detail.get("jc_delta", 0) or 0)
        if "jc" in detail:
            return int(detail.get("jc", 0) or 0)
        return 0

    if action == "abandon" and is_actor:
        return int(detail.get("refund", 0) or 0)

    # ``prestige`` and unknown action types: no direct balance change.
    return 0
