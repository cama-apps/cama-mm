"""Central economy ledger read helpers."""

from __future__ import annotations

from repositories.base_repository import BaseRepository


class EconomyLedgerRepository(BaseRepository):
    """Read access for central money-movement ledger entries."""

    def get_recent_entries(
        self,
        guild_id: int | None,
        *,
        limit: int = 20,
        account_type: str | None = None,
        account_id: int | None = None,
    ) -> list[dict]:
        gid = self.normalize_guild_id(guild_id)
        limit = max(1, min(int(limit), 100))
        clauses = ["guild_id = ?"]
        params: list[object] = [gid]
        if account_type is not None:
            clauses.append("account_type = ?")
            params.append(account_type)
        if account_id is not None:
            clauses.append("account_id = ?")
            params.append(account_id)
        params.append(limit)
        where = " AND ".join(clauses)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT ledger_id, guild_id, account_type, account_id, delta,
                       balance_before, balance_after, source, actor_id,
                       related_type, related_id, reason, metadata, created_at
                FROM economy_ledger_entries
                WHERE {where}
                ORDER BY created_at DESC, ledger_id DESC
                LIMIT ?
                """,
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_source_totals(self, guild_id: int | None, *, limit: int = 20) -> list[dict]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT source,
                       COUNT(*) AS entry_count,
                       COALESCE(SUM(delta), 0) AS net_delta,
                       COALESCE(SUM(CASE WHEN delta > 0 THEN delta ELSE 0 END), 0) AS inflow,
                       COALESCE(SUM(CASE WHEN delta < 0 THEN -delta ELSE 0 END), 0) AS outflow
                FROM economy_ledger_entries
                WHERE guild_id = ?
                GROUP BY source
                ORDER BY entry_count DESC, source ASC
                LIMIT ?
                """,
                (gid, max(1, min(int(limit), 100))),
            )
            return [dict(row) for row in cursor.fetchall()]
