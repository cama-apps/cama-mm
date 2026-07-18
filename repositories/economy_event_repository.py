"""Persistence and atomic balance-sheet actions for daily economy events."""

from __future__ import annotations

import json
import math
import time
from typing import Any

from repositories.base_repository import BaseRepository, safe_json_loads


class EconomyEventRepository(BaseRepository):
    """Guild-scoped policy state, snapshots, event cards, and direct actions."""

    _DIRECT_FLOW_SOURCES = (
        "dig",
        "gamba",
        "hostile_loss",
        "balance_update",
        "nonprofit_update",
        "bankruptcy",
        "manashop",
        "manashop_buff",
        "loan",
        "loan_repayment",
        "shop",
        "trivia",
        "player_trivia",
        "mana_reward",
        "match_streak",
        "pingedash",
        "mana_protection",
        "bet",
        "bet_settlement",
        "bet_refund",
        "dota_bet_seed",
        "dota_bet_seed_return",
    )

    def ensure_policy_state(
        self,
        guild_id: int | None,
        *,
        mode: str,
        target_annual_rate: float,
        inflation_ceiling: float,
        now: int | None = None,
    ) -> dict[str, Any]:
        gid = self.normalize_guild_id(guild_id)
        now = int(now if now is not None else time.time())
        with self.connection() as conn:
            conn.execute(
                """
                INSERT INTO economy_policy_state (
                    guild_id, mode, target_annual_rate, inflation_ceiling,
                    recovery_started_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET
                    mode = excluded.mode,
                    target_annual_rate = excluded.target_annual_rate,
                    inflation_ceiling = excluded.inflation_ceiling,
                    recovery_started_at = CASE
                        WHEN excluded.mode = 'recovery'
                         AND economy_policy_state.mode != 'recovery'
                            THEN excluded.recovery_started_at
                        ELSE economy_policy_state.recovery_started_at
                    END,
                    updated_at = excluded.updated_at
                """,
                (
                    gid,
                    mode,
                    float(target_annual_rate),
                    float(inflation_ceiling),
                    now if mode == "recovery" else None,
                    now,
                ),
            )
        return self.get_policy_state(gid)

    def get_policy_state(self, guild_id: int | None) -> dict[str, Any]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT * FROM economy_policy_state WHERE guild_id = ?", (gid,)
            ).fetchone()
        return dict(row) if row else {}

    def set_policy_mode(
        self,
        guild_id: int | None,
        *,
        mode: str,
        target_annual_rate: float,
        now: int | None = None,
    ) -> None:
        if mode not in {"recovery", "normal", "disabled"}:
            raise ValueError(f"Invalid economy policy mode: {mode}")
        gid = self.normalize_guild_id(guild_id)
        now = int(now if now is not None else time.time())
        with self.connection() as conn:
            conn.execute(
                """
                UPDATE economy_policy_state
                SET mode = ?, target_annual_rate = ?,
                    recovery_started_at = CASE
                        WHEN ? = 'recovery' THEN COALESCE(recovery_started_at, ?)
                        ELSE recovery_started_at
                    END,
                    updated_at = ?
                WHERE guild_id = ?
                """,
                (mode, float(target_annual_rate), mode, now, now, gid),
            )

    def capture_balance_sheet(self, guild_id: int | None) -> dict[str, int | float]:
        """Return the reconciled stock used by the monetary controller."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                WITH player_totals AS (
                    SELECT
                        COUNT(*) AS player_count,
                        COALESCE(SUM(jopacoin_balance), 0) AS player_wallets,
                        COALESCE(SUM(CASE WHEN jopacoin_balance > 0
                            THEN jopacoin_balance ELSE 0 END), 0) AS positive_wallets,
                        COALESCE(SUM(CASE WHEN jopacoin_balance < 0
                            THEN -jopacoin_balance ELSE 0 END), 0) AS visible_debt
                    FROM players WHERE guild_id = :gid
                ), reserve_totals AS (
                    SELECT
                        COALESCE(total_collected, 0) AS reserve_available,
                        COALESCE(next_match_pot, 0) AS reserve_next_match_pot
                    FROM nonprofit_fund WHERE guild_id = :gid
                ), locked_totals AS (
                    SELECT COALESCE(SUM(fund_amount), 0) AS reserve_locked
                    FROM disburse_proposals
                    WHERE guild_id = :gid AND status = 'active'
                ), prediction_totals AS (
                    SELECT COALESCE(SUM(lp_pnl), 0) AS prediction_open_cash
                    FROM predictions
                    WHERE guild_id = :gid AND status IN ('open', 'locked')
                ), duel_totals AS (
                    SELECT COALESCE(SUM(CASE
                        WHEN status = 'accepted' THEN wager * 2
                        WHEN status = 'pending' THEN wager
                        ELSE 0 END), 0) AS duel_escrow
                    FROM duel_challenges WHERE guild_id = :gid
                ), mafia_totals AS (
                    SELECT COALESCE(SUM(entry_fee * roster_size), 0) AS mafia_escrow
                    FROM mafia_games
                    WHERE guild_id = :gid AND status = 'ACTIVE' AND phase != 'RESOLVED'
                ), bet_totals AS (
                    SELECT COALESCE(SUM(b.amount * COALESCE(b.leverage, 1)), 0)
                        AS bet_escrow
                    FROM bets b
                    JOIN pending_matches pm
                      ON pm.pending_match_id = b.pending_match_id
                     AND pm.guild_id = b.guild_id
                    WHERE b.guild_id = :gid
                )
                SELECT
                    p.player_count, p.player_wallets, p.positive_wallets,
                    p.visible_debt,
                    COALESCE(r.reserve_available, 0) AS reserve_available,
                    l.reserve_locked,
                    COALESCE(r.reserve_next_match_pot, 0) AS reserve_next_match_pot,
                    pr.prediction_open_cash,
                    d.duel_escrow + m.mafia_escrow + b.bet_escrow AS wager_escrow
                FROM player_totals p
                CROSS JOIN locked_totals l
                CROSS JOIN prediction_totals pr
                CROSS JOIN duel_totals d
                CROSS JOIN mafia_totals m
                CROSS JOIN bet_totals b
                LEFT JOIN reserve_totals r ON 1 = 1
                """,
                {"gid": gid},
            ).fetchone()

        data = {key: int(row[key] or 0) for key in row.keys()}
        player_count = data["player_count"]
        data["average_wallet"] = (
            data["player_wallets"] / player_count if player_count else 0.0
        )
        data["monetary_stock"] = (
            data["player_wallets"]
            + data["reserve_available"]
            + data["reserve_locked"]
            + data["reserve_next_match_pot"]
            + data["prediction_open_cash"]
            + data["wager_escrow"]
        )
        return data

    def save_snapshot(
        self,
        guild_id: int | None,
        snapshot_date: str,
        snapshot: dict[str, int | float],
        *,
        captured_at: int | None = None,
    ) -> dict[str, Any]:
        gid = self.normalize_guild_id(guild_id)
        captured_at = int(captured_at if captured_at is not None else time.time())
        annualized_30d = self._annualized_change(gid, snapshot_date, snapshot, 30)
        annualized_90d = self._annualized_change(gid, snapshot_date, snapshot, 90)
        with self.connection() as conn:
            conn.execute(
                """
                INSERT INTO economy_daily_snapshots (
                    guild_id, snapshot_date, captured_at, player_wallets,
                    positive_wallets, visible_debt, player_count, average_wallet,
                    reserve_available, reserve_locked, reserve_next_match_pot,
                    prediction_open_cash, wager_escrow, monetary_stock,
                    annualized_30d, annualized_90d
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(guild_id, snapshot_date) DO UPDATE SET
                    captured_at = excluded.captured_at,
                    player_wallets = excluded.player_wallets,
                    positive_wallets = excluded.positive_wallets,
                    visible_debt = excluded.visible_debt,
                    player_count = excluded.player_count,
                    average_wallet = excluded.average_wallet,
                    reserve_available = excluded.reserve_available,
                    reserve_locked = excluded.reserve_locked,
                    reserve_next_match_pot = excluded.reserve_next_match_pot,
                    prediction_open_cash = excluded.prediction_open_cash,
                    wager_escrow = excluded.wager_escrow,
                    monetary_stock = excluded.monetary_stock,
                    annualized_30d = excluded.annualized_30d,
                    annualized_90d = excluded.annualized_90d
                """,
                (
                    gid,
                    snapshot_date,
                    captured_at,
                    int(snapshot["player_wallets"]),
                    int(snapshot["positive_wallets"]),
                    int(snapshot["visible_debt"]),
                    int(snapshot["player_count"]),
                    float(snapshot["average_wallet"]),
                    int(snapshot["reserve_available"]),
                    int(snapshot["reserve_locked"]),
                    int(snapshot["reserve_next_match_pot"]),
                    int(snapshot["prediction_open_cash"]),
                    int(snapshot["wager_escrow"]),
                    int(snapshot["monetary_stock"]),
                    annualized_30d,
                    annualized_90d,
                ),
            )
        return {
            **snapshot,
            "snapshot_date": snapshot_date,
            "captured_at": captured_at,
            "annualized_30d": annualized_30d,
            "annualized_90d": annualized_90d,
        }

    def _annualized_change(
        self,
        guild_id: int,
        snapshot_date: str,
        snapshot: dict[str, int | float],
        days: int,
    ) -> float | None:
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT monetary_stock,
                       julianday(?) - julianday(snapshot_date) AS elapsed
                FROM economy_daily_snapshots
                WHERE guild_id = ?
                  AND snapshot_date <= date(?, ?)
                ORDER BY snapshot_date DESC
                LIMIT 1
                """,
                (snapshot_date, guild_id, snapshot_date, f"-{days} days"),
            ).fetchone()
        if not row:
            return None
        old = int(row["monetary_stock"] or 0)
        current = int(snapshot["monetary_stock"])
        elapsed = float(row["elapsed"] or 0)
        if old <= 0 or current <= 0 or elapsed < max(1, days * 0.8):
            return None
        return math.pow(current / old, 365.0 / elapsed) - 1.0

    def get_latest_snapshot(self, guild_id: int | None) -> dict[str, Any] | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT * FROM economy_daily_snapshots
                WHERE guild_id = ? ORDER BY snapshot_date DESC LIMIT 1
                """,
                (gid,),
            ).fetchone()
        return dict(row) if row else None

    def forecast_daily_flow(
        self, guild_id: int | None, *, lookback_days: int, now: int | None = None
    ) -> int:
        """Estimate unmanaged daily creation from attributed recent flows."""
        gid = self.normalize_guild_id(guild_id)
        now = int(now if now is not None else time.time())
        cutoff = now - max(1, int(lookback_days)) * 86400
        placeholders = ",".join("?" for _ in self._DIRECT_FLOW_SOURCES)
        with self.connection() as conn:
            ledger = conn.execute(
                f"""
                SELECT COALESCE(SUM(delta), 0) AS net
                FROM economy_ledger_entries
                WHERE guild_id = ? AND created_at >= ?
                  AND source IN ({placeholders})
                """,
                (gid, cutoff, *self._DIRECT_FLOW_SOURCES),
            ).fetchone()
            prediction = conn.execute(
                """
                SELECT COALESCE(SUM(-lp_pnl), 0) AS net
                FROM predictions
                WHERE guild_id = ? AND status = 'resolved'
                  AND resolved_at >= ?
                """,
                (gid, cutoff),
            ).fetchone()
        total = int(ledger["net"] or 0) + int(prediction["net"] or 0)
        return int(round(total / max(1, int(lookback_days))))

    def get_surface_daily_volumes(
        self, guild_id: int | None, *, lookback_days: int, now: int | None = None
    ) -> dict[str, float]:
        gid = self.normalize_guild_id(guild_id)
        days = max(1, int(lookback_days))
        now = int(now if now is not None else time.time())
        cutoff = now - days * 86400
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT source,
                    COALESCE(SUM(CASE WHEN delta > 0 THEN delta ELSE 0 END), 0)
                        AS credits,
                    COALESCE(-SUM(CASE WHEN delta < 0 THEN delta ELSE 0 END), 0)
                        AS debits
                FROM economy_ledger_entries
                WHERE guild_id = ? AND created_at >= ?
                  AND source IN (
                    'dig', 'trivia', 'player_trivia', 'mana_reward', 'manashop_buff',
                    'gamba', 'bet_settlement', 'prediction_resolution'
                  )
                GROUP BY source
                """,
                (gid, cutoff),
            ).fetchall()
        result = {
            "reward_credits": 0.0,
            "gamba_credits": 0.0,
            "gamba_debits": 0.0,
            "bet_payouts": 0.0,
            "prediction_payouts": 0.0,
        }
        for row in rows:
            source = row["source"]
            if source in {
                "dig",
                "trivia",
                "player_trivia",
                "mana_reward",
                "manashop_buff",
            }:
                result["reward_credits"] += int(row["credits"] or 0) / days
            elif source == "gamba":
                result["gamba_credits"] += int(row["credits"] or 0) / days
                result["gamba_debits"] += int(row["debits"] or 0) / days
            elif source == "bet_settlement":
                result["bet_payouts"] += int(row["credits"] or 0) / days
            elif source == "prediction_resolution":
                result["prediction_payouts"] += int(row["credits"] or 0) / days
        return result

    def get_event_for_date(
        self, guild_id: int | None, event_date: str
    ) -> dict[str, Any] | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT * FROM economy_daily_events
                WHERE guild_id = ? AND event_date = ?
                """,
                (gid, event_date),
            ).fetchone()
        return self._event_row(row) if row else None

    def activate_event_atomic(
        self,
        guild_id: int | None,
        event: dict[str, Any],
    ) -> tuple[dict[str, Any], bool]:
        """Insert one event and apply all direct Reserve/wallet legs once."""
        gid = self.normalize_guild_id(guild_id)
        effects = dict(event["effects"])
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            existing = cursor.execute(
                """
                SELECT * FROM economy_daily_events
                WHERE guild_id = ? AND event_date = ?
                """,
                (gid, event["event_date"]),
            ).fetchone()
            if existing:
                return self._event_row(existing), False

            cursor.execute(
                """
                INSERT INTO economy_daily_events (
                    guild_id, event_date, name, hero, direction, severity,
                    target_effect_jc, forecast_flow_jc, expected_effect_jc,
                    direct_effect_jc, monetary_stock_before, effects,
                    announcement, starts_at, ends_at, created_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)
                """,
                (
                    gid,
                    event["event_date"],
                    event["name"],
                    event["hero"],
                    event["direction"],
                    int(event["severity"]),
                    int(event["target_effect_jc"]),
                    int(event["forecast_flow_jc"]),
                    int(event["expected_effect_jc"]),
                    int(event["monetary_stock_before"]),
                    json.dumps(effects, sort_keys=True),
                    event["announcement"],
                    int(event["starts_at"]),
                    int(event["ends_at"]),
                    int(event["created_at"]),
                ),
            )
            event_id = int(cursor.lastrowid)
            direct_effect = 0

            reserve = cursor.execute(
                """
                SELECT COALESCE(total_collected, 0) AS available
                FROM nonprofit_fund WHERE guild_id = ?
                """,
                (gid,),
            ).fetchone()
            available = int(reserve["available"] if reserve else 0)

            requested_burn = max(0, int(effects.get("reserve_burn_jc", 0)))
            reserve_burn = min(available, requested_burn)
            if reserve_burn:
                self._set_economy_ledger_context(
                    cursor,
                    source="economy_event",
                    related_type="daily_economy_event",
                    related_id=event_id,
                    reason=f"{event['name']} reserve burn",
                    metadata={"event_date": event["event_date"]},
                )
                try:
                    cursor.execute(
                        """
                        UPDATE nonprofit_fund
                        SET total_collected = total_collected - ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE guild_id = ?
                        """,
                        (reserve_burn, gid),
                    )
                finally:
                    self._clear_economy_ledger_context(cursor)
                available -= reserve_burn
                direct_effect -= reserve_burn

            wallet_burn_rate = max(0.0, float(effects.get("wallet_burn_rate", 0.0)))
            wallet_burn = 0
            if wallet_burn_rate:
                players = cursor.execute(
                    """
                    SELECT discord_id, jopacoin_balance FROM players
                    WHERE guild_id = ? AND jopacoin_balance > 0
                    ORDER BY discord_id
                    """,
                    (gid,),
                ).fetchall()
                self._set_economy_ledger_context(
                    cursor,
                    source="economy_event",
                    related_type="daily_economy_event",
                    related_id=event_id,
                    reason=f"{event['name']} wallet burn",
                    metadata={
                        "event_date": event["event_date"],
                        "rate": wallet_burn_rate,
                    },
                )
                try:
                    for player in players:
                        debit = int(int(player["jopacoin_balance"]) * wallet_burn_rate)
                        if debit <= 0:
                            continue
                        cursor.execute(
                            """
                            UPDATE players SET jopacoin_balance = jopacoin_balance - ?,
                                updated_at = CURRENT_TIMESTAMP
                            WHERE guild_id = ? AND discord_id = ?
                            """,
                            (debit, gid, int(player["discord_id"])),
                        )
                        wallet_burn += debit
                finally:
                    self._clear_economy_ledger_context(cursor)
                direct_effect -= wallet_burn

            requested_release = max(0, int(effects.get("reserve_release_jc", 0)))
            reserve_release = min(available, requested_release)
            released = 0
            if reserve_release:
                player_ids = [
                    int(row["discord_id"])
                    for row in cursor.execute(
                        "SELECT discord_id FROM players WHERE guild_id = ? ORDER BY discord_id",
                        (gid,),
                    ).fetchall()
                ]
                if player_ids:
                    quotient, remainder = divmod(reserve_release, len(player_ids))
                    self._set_economy_ledger_context(
                        cursor,
                        source="economy_event",
                        related_type="daily_economy_event",
                        related_id=event_id,
                        reason=f"{event['name']} reserve release",
                        metadata={"event_date": event["event_date"]},
                    )
                    try:
                        cursor.execute(
                            """
                            UPDATE nonprofit_fund
                            SET total_collected = total_collected - ?,
                                updated_at = CURRENT_TIMESTAMP
                            WHERE guild_id = ?
                            """,
                            (reserve_release, gid),
                        )
                        for index, player_id in enumerate(player_ids):
                            credit = quotient + (1 if index < remainder else 0)
                            if credit <= 0:
                                continue
                            cursor.execute(
                                """
                                UPDATE players SET jopacoin_balance = jopacoin_balance + ?,
                                    updated_at = CURRENT_TIMESTAMP
                                WHERE guild_id = ? AND discord_id = ?
                                """,
                                (credit, gid, player_id),
                            )
                            released += credit
                    finally:
                        self._clear_economy_ledger_context(cursor)

            effects["reserve_burn_jc"] = reserve_burn
            effects["wallet_burn_jc"] = wallet_burn
            effects["reserve_release_jc"] = released
            cursor.execute(
                """
                UPDATE economy_daily_events
                SET effects = ?, direct_effect_jc = ?
                WHERE event_id = ?
                """,
                (json.dumps(effects, sort_keys=True), direct_effect, event_id),
            )
            row = cursor.execute(
                "SELECT * FROM economy_daily_events WHERE event_id = ?", (event_id,)
            ).fetchone()
            return self._event_row(row), True

    def reconcile_prior_event(
        self, guild_id: int | None, event_date: str, current_stock: int
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                """
                UPDATE economy_daily_events
                SET actual_stock_change_jc = ? - monetary_stock_before
                WHERE event_id = (
                    SELECT event_id FROM economy_daily_events
                    WHERE guild_id = ? AND event_date < ?
                    ORDER BY event_date DESC LIMIT 1
                ) AND actual_stock_change_jc IS NULL
                """,
                (int(current_stock), gid, event_date),
            )

    @staticmethod
    def _event_row(row) -> dict[str, Any]:
        data = dict(row)
        data["effects"] = safe_json_loads(
            data.get("effects"), {}, context=f"economy_daily_events id={data.get('event_id')}"
        )
        return data
