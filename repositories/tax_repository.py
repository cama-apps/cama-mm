"""Repository for Tax Man audit summaries."""

from __future__ import annotations

import time

from repositories.base_repository import BaseRepository, safe_json_loads


class TaxRepository(BaseRepository):
    """Read helpers for guild and player monetary exposure."""

    def get_active_dark_bargain_debts(self, guild_id: int | None) -> list[dict]:
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, granted_at, expires_at, data
                FROM manashop_buffs
                WHERE guild_id = ?
                  AND buff_type = 'dark_bargain'
                  AND triggered = 0
                  AND expires_at > ?
                ORDER BY expires_at ASC
                """,
                (gid, now),
            )
            debts: list[dict] = []
            for row in cursor.fetchall():
                data = safe_json_loads(row["data"], {}, context="manashop_buffs.data")
                debts.append(
                    {
                        "id": row["id"],
                        "discord_id": row["discord_id"],
                        "guild_id": row["guild_id"],
                        "granted_at": row["granted_at"],
                        "expires_at": row["expires_at"],
                        "seconds_until_due": int(row["expires_at"] or 0) - now,
                        "amount_due": int(data.get("amount_due") or 0),
                        "default_penalty": int(data.get("default_penalty") or 0),
                        "default_penalty_games": int(data.get("default_penalty_games") or 0),
                    }
                )
            return debts

    def get_prediction_market_exposure(
        self,
        guild_id: int | None,
        *,
        limit: int = 5,
    ) -> dict:
        from config import PREDICTION_CONTRACT_VALUE, PREDICTION_INITIAL_FAIR_DEFAULT

        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                WITH pos AS (
                    SELECT prediction_id,
                           COUNT(DISTINCT discord_id) AS holder_count,
                           COALESCE(SUM(yes_contracts), 0) AS yes_contracts,
                           COALESCE(SUM(no_contracts), 0) AS no_contracts,
                           COALESCE(SUM(yes_cost_basis_total), 0) AS yes_cost_basis,
                           COALESCE(SUM(no_cost_basis_total), 0) AS no_cost_basis
                    FROM prediction_positions
                    GROUP BY prediction_id
                ),
                lvl AS (
                    SELECT prediction_id,
                           MIN(CASE WHEN side = 'yes_ask' THEN price END) AS top_yes_ask,
                           MAX(CASE WHEN side = 'yes_bid' THEN price END) AS top_yes_bid,
                           COALESCE(SUM(remaining_size), 0) AS book_contracts
                    FROM prediction_levels
                    WHERE remaining_size > 0
                    GROUP BY prediction_id
                )
                SELECT p.prediction_id, p.question, p.status,
                       COALESCE(
                           p.current_price,
                           p.initial_fair,
                           ?
                       ) AS current_price,
                       COALESCE(p.lp_pnl, 0) AS lp_pnl,
                       COALESCE(pos.holder_count, 0) AS holder_count,
                       COALESCE(pos.yes_contracts, 0) AS yes_contracts,
                       COALESCE(pos.no_contracts, 0) AS no_contracts,
                       COALESCE(pos.yes_cost_basis, 0) AS yes_cost_basis,
                       COALESCE(pos.no_cost_basis, 0) AS no_cost_basis,
                       lvl.top_yes_ask,
                       lvl.top_yes_bid,
                       COALESCE(lvl.book_contracts, 0) AS book_contracts
                FROM predictions p
                LEFT JOIN pos ON pos.prediction_id = p.prediction_id
                LEFT JOIN lvl ON lvl.prediction_id = p.prediction_id
                WHERE p.guild_id = ?
                  AND p.status IN ('open', 'locked')
                ORDER BY p.created_at DESC, p.prediction_id DESC
                """,
                (PREDICTION_INITIAL_FAIR_DEFAULT, gid),
            )
            rows = [dict(row) for row in cursor.fetchall()]

        markets = [
            self._prediction_market_metrics(row, PREDICTION_CONTRACT_VALUE)
            for row in rows
        ]
        summary = {
            "open_markets": len(markets),
            "holder_count": sum(int(m["holder_count"]) for m in markets),
            "yes_contracts": sum(int(m["yes_contracts"]) for m in markets),
            "no_contracts": sum(int(m["no_contracts"]) for m in markets),
            "cost_basis": sum(int(m["cost_basis"]) for m in markets),
            "expected_payout": sum(int(m["expected_payout"]) for m in markets),
            "ev_to_holders": sum(int(m["ev_to_holders"]) for m in markets),
            "yes_liability": sum(int(m["yes_liability"]) for m in markets),
            "no_liability": sum(int(m["no_liability"]) for m in markets),
            "worst_case_payout": sum(int(m["worst_case_payout"]) for m in markets),
            "lp_pnl": sum(int(m["lp_pnl"]) for m in markets),
            "book_contracts": sum(int(m["book_contracts"]) for m in markets),
        }
        return {
            "summary": summary,
            "markets": markets[: max(1, min(int(limit), 25))],
        }

    def get_player_prediction_exposure(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> dict:
        from config import PREDICTION_CONTRACT_VALUE, PREDICTION_INITIAL_FAIR_DEFAULT

        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT p.prediction_id, p.question, p.status,
                       COALESCE(
                           p.current_price,
                           p.initial_fair,
                           ?
                       ) AS current_price,
                       pp.yes_contracts,
                       pp.yes_cost_basis_total,
                       pp.no_contracts,
                       pp.no_cost_basis_total
                FROM prediction_positions pp
                JOIN predictions p ON p.prediction_id = pp.prediction_id
                WHERE pp.discord_id = ?
                  AND p.guild_id = ?
                  AND p.status IN ('open', 'locked')
                  AND (pp.yes_contracts > 0 OR pp.no_contracts > 0)
                ORDER BY p.created_at DESC, p.prediction_id DESC
                """,
                (PREDICTION_INITIAL_FAIR_DEFAULT, discord_id, gid),
            )
            rows = [dict(row) for row in cursor.fetchall()]

        positions = [
            self._prediction_player_metrics(row, PREDICTION_CONTRACT_VALUE)
            for row in rows
        ]
        summary = {
            "markets": len(positions),
            "yes_contracts": sum(int(p["yes_contracts"]) for p in positions),
            "no_contracts": sum(int(p["no_contracts"]) for p in positions),
            "cost_basis": sum(int(p["cost_basis"]) for p in positions),
            "expected_payout": sum(int(p["expected_payout"]) for p in positions),
            "ev": sum(int(p["ev"]) for p in positions),
            "max_payout": sum(int(p["max_payout"]) for p in positions),
        }
        return {"summary": summary, "positions": positions}

    def _prediction_market_metrics(self, row: dict, contract_value: int) -> dict:
        price = int(row["current_price"] or 0)
        yes_contracts = int(row["yes_contracts"] or 0)
        no_contracts = int(row["no_contracts"] or 0)
        yes_cost = int(row["yes_cost_basis"] or 0)
        no_cost = int(row["no_cost_basis"] or 0)
        yes_liability = yes_contracts * contract_value
        no_liability = no_contracts * contract_value
        expected_payout = (
            yes_contracts * contract_value * price
            + no_contracts * contract_value * (100 - price)
        ) // 100
        cost_basis = yes_cost + no_cost
        return {
            "prediction_id": int(row["prediction_id"]),
            "question": row["question"],
            "status": row["status"],
            "current_price": price,
            "holder_count": int(row["holder_count"] or 0),
            "yes_contracts": yes_contracts,
            "no_contracts": no_contracts,
            "yes_cost_basis": yes_cost,
            "no_cost_basis": no_cost,
            "cost_basis": cost_basis,
            "expected_payout": expected_payout,
            "ev_to_holders": expected_payout - cost_basis,
            "yes_liability": yes_liability,
            "no_liability": no_liability,
            "worst_case_payout": max(yes_liability, no_liability),
            "lp_pnl": int(row["lp_pnl"] or 0),
            "top_yes_ask": row["top_yes_ask"],
            "top_yes_bid": row["top_yes_bid"],
            "book_contracts": int(row["book_contracts"] or 0),
        }

    def _prediction_player_metrics(self, row: dict, contract_value: int) -> dict:
        price = int(row["current_price"] or 0)
        yes_contracts = int(row["yes_contracts"] or 0)
        no_contracts = int(row["no_contracts"] or 0)
        yes_cost = int(row["yes_cost_basis_total"] or 0)
        no_cost = int(row["no_cost_basis_total"] or 0)
        yes_expected = yes_contracts * contract_value * price // 100
        no_expected = no_contracts * contract_value * (100 - price) // 100
        cost_basis = yes_cost + no_cost
        return {
            "prediction_id": int(row["prediction_id"]),
            "question": row["question"],
            "status": row["status"],
            "current_price": price,
            "yes_contracts": yes_contracts,
            "no_contracts": no_contracts,
            "yes_cost_basis": yes_cost,
            "no_cost_basis": no_cost,
            "cost_basis": cost_basis,
            "expected_payout": yes_expected + no_expected,
            "ev": yes_expected + no_expected - cost_basis,
            "yes_max_payout": yes_contracts * contract_value,
            "no_max_payout": no_contracts * contract_value,
            "max_payout": (
                yes_contracts * contract_value
                + no_contracts * contract_value
            ),
        }

    def get_guild_tax_snapshot(self, guild_id: int | None) -> dict:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            player_row = cursor.execute(
                """
                SELECT COUNT(*) AS players,
                       COALESCE(SUM(jopacoin_balance), 0) AS total_balance,
                       COALESCE(SUM(CASE WHEN jopacoin_balance > 0 THEN jopacoin_balance ELSE 0 END), 0) AS positive_balance,
                       COALESCE(SUM(CASE WHEN jopacoin_balance < 0 THEN -jopacoin_balance ELSE 0 END), 0) AS visible_debt,
                       COALESCE(SUM(CASE WHEN jopacoin_balance <= 0 THEN 1 ELSE 0 END), 0) AS broke_players
                FROM players
                WHERE guild_id = ?
                """,
                (gid,),
            ).fetchone()
            nonprofit_row = cursor.execute(
                "SELECT COALESCE(total_collected, 0) AS total FROM nonprofit_fund WHERE guild_id = ?",
                (gid,),
            ).fetchone()
            loans_row = cursor.execute(
                """
                SELECT COALESCE(SUM(outstanding_principal), 0) AS principal,
                       COALESCE(SUM(outstanding_fee), 0) AS fee,
                       COUNT(CASE WHEN outstanding_principal > 0 THEN 1 END) AS borrowers
                FROM loan_state
                WHERE guild_id = ?
                """,
                (gid,),
            ).fetchone()
            disburse_row = cursor.execute(
                """
                SELECT COALESCE(SUM(fund_amount), 0) AS reserved
                FROM disburse_proposals
                WHERE guild_id = ? AND status = 'active'
                """,
                (gid,),
            ).fetchone()
            pending_bets_row = cursor.execute(
                """
                SELECT COALESCE(SUM(amount * COALESCE(leverage, 1)), 0) AS effective_stake,
                       COUNT(*) AS bet_count
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL
                """,
                (gid,),
            ).fetchone()

        dark_bargains = self.get_active_dark_bargain_debts(gid)
        dark_bargain_due = sum(d["amount_due"] for d in dark_bargains)
        prediction_exposure = self.get_prediction_market_exposure(gid)
        prediction_summary = prediction_exposure["summary"]
        return {
            "guild_id": gid,
            "players": int(player_row["players"] or 0),
            "total_balance": int(player_row["total_balance"] or 0),
            "positive_balance": int(player_row["positive_balance"] or 0),
            "visible_debt": int(player_row["visible_debt"] or 0),
            "broke_players": int(player_row["broke_players"] or 0),
            "nonprofit_available": int(nonprofit_row["total"] if nonprofit_row else 0),
            "nonprofit_reserved": int(disburse_row["reserved"] or 0),
            "loan_principal": int(loans_row["principal"] or 0),
            "loan_fee": int(loans_row["fee"] or 0),
            "loan_borrowers": int(loans_row["borrowers"] or 0),
            "dark_bargain_count": len(dark_bargains),
            "dark_bargain_due": dark_bargain_due,
            "pending_bet_effective_stake": int(pending_bets_row["effective_stake"] or 0),
            "pending_bet_count": int(pending_bets_row["bet_count"] or 0),
            "open_prediction_markets": int(prediction_summary["open_markets"]),
            "prediction_exposure": prediction_exposure,
        }
