"""Unified jopacoin balance-history series for the /profile Economy tab chart."""

from __future__ import annotations

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


@dataclass
class _Event:
    time: int
    delta: int
    source: str
    detail: dict = field(default_factory=dict)


class BalanceHistoryService:
    """Merge all persisted balance-impacting events for a player into one series.

    Seven sources are chartable today:
      - ``bets``              settled bets (profit)
      - ``predictions``       resolved / cancelled prediction bets (payout − staked)
      - ``wheel``             Wheel of Fortune spins (result)
      - ``double_or_nothing`` Double or Nothing spins (balance_after − (balance_before + cost))
      - ``tips``              sent (-(amount+fee)) and received (+amount)
      - ``disburse``          nonprofit disbursements received (+amount)
      - ``bonus``             per-match participation + win bonus reconstruction

    Un-persisted sources (shop, admin /givecoin, bankruptcy penalty, garnishment,
    streaming / first-game / exclusion / bomb-pot bonuses, cancelled-match refunds)
    are silently omitted — the series starts at 0 and does not pretend to equal
    live balance.
    """

    def __init__(
        self,
        bet_repo,
        match_repo,
        player_repo,
        prediction_repo,
        disburse_repo,
        tip_repo,
    ):
        self.bet_repo = bet_repo
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.prediction_repo = prediction_repo
        self.disburse_repo = disburse_repo
        self.tip_repo = tip_repo

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
            participation = JOPACOIN_PER_GAME
            win = JOPACOIN_WIN_REWARD if row["won"] else 0
            delta = participation + win
            if delta == 0:
                continue
            out.append(
                _Event(
                    time=int(match_time),
                    delta=delta,
                    source=SOURCE_BONUS,
                    detail={
                        "match_id": row["match_id"],
                        "components": {
                            "participation": participation,
                            "win": win,
                        },
                        "won": row["won"],
                    },
                )
            )
        return out
