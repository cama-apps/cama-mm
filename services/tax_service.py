"""Service layer for Tax Man audit operations."""

from __future__ import annotations

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.economy_ledger_repository import EconomyLedgerRepository
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from repositories.tax_repository import TaxRepository


class TaxService:
    """Tax Man policy and audit summaries over the central economy ledger."""

    def __init__(
        self,
        *,
        tax_repo: TaxRepository,
        ledger_repo: EconomyLedgerRepository,
        player_repo: PlayerRepository,
        loan_repo: LoanRepository,
        bankruptcy_repo: BankruptcyRepository,
    ):
        self.tax_repo = tax_repo
        self.ledger_repo = ledger_repo
        self.player_repo = player_repo
        self.loan_repo = loan_repo
        self.bankruptcy_repo = bankruptcy_repo

    def get_guild_snapshot(self, guild_id: int | None) -> dict:
        return self.tax_repo.get_guild_tax_snapshot(guild_id)

    def get_recent_ledger(
        self,
        guild_id: int | None,
        *,
        limit: int = 20,
        offset: int = 0,
        user_id: int | None = None,
    ) -> list[dict]:
        return self.ledger_repo.get_recent_entries(
            guild_id,
            limit=limit,
            offset=offset,
            account_type="player" if user_id is not None else None,
            account_id=user_id,
        )

    def count_ledger_entries(
        self,
        guild_id: int | None,
        *,
        user_id: int | None = None,
    ) -> int:
        return self.ledger_repo.count_entries(
            guild_id,
            account_type="player" if user_id is not None else None,
            account_id=user_id,
        )

    def get_source_totals(self, guild_id: int | None, *, limit: int = 20) -> list[dict]:
        return self.ledger_repo.get_source_totals(guild_id, limit=limit)

    def get_player_snapshot(self, discord_id: int, guild_id: int | None) -> dict:
        """Return the monetary audit view for one guild-scoped player."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if player is None:
            raise ValueError("target_not_registered")

        balance = int(player.jopacoin_balance or 0)
        loan = self.loan_repo.get_state(discord_id, guild_id) or {}
        bankruptcy = self.bankruptcy_repo.get_state(discord_id, guild_id) or {}
        dark_bargains = [
            debt
            for debt in self.tax_repo.get_active_dark_bargain_debts(guild_id)
            if int(debt.get("discord_id") or 0) == discord_id
        ]
        prediction_exposure = self.tax_repo.get_player_prediction_exposure(
            discord_id,
            guild_id,
        )

        loan_principal = int(loan.get("outstanding_principal") or 0)
        loan_fee = int(loan.get("outstanding_fee") or 0)
        dark_bargain_due = sum(int(debt.get("amount_due") or 0) for debt in dark_bargains)
        visible_debt = max(0, -balance)

        return {
            "discord_id": discord_id,
            "guild_id": player.guild_id,
            "name": player.name,
            "balance": balance,
            "visible_debt": visible_debt,
            "loan_principal": loan_principal,
            "loan_fee": loan_fee,
            "loan_total": loan_principal + loan_fee,
            "total_loans_taken": int(loan.get("total_loans_taken") or 0),
            "total_fees_paid": int(loan.get("total_fees_paid") or 0),
            "bankruptcy_count": int(bankruptcy.get("bankruptcy_count") or 0),
            "penalty_games_remaining": int(
                bankruptcy.get("penalty_games_remaining") or 0
            ),
            "dark_bargain_count": len(dark_bargains),
            "dark_bargain_due": dark_bargain_due,
            "dark_bargains": dark_bargains,
            "prediction_exposure": prediction_exposure,
            "effective_obligations": visible_debt + loan_principal + loan_fee + dark_bargain_due,
            "recent_ledger": self.get_recent_ledger(
                guild_id,
                limit=8,
                user_id=discord_id,
            ),
        }
