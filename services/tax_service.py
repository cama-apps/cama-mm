"""Service layer for Tax Man audit operations."""

from __future__ import annotations

from config import TAX_FINE_COOLDOWN_SECONDS
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

    def levy_fine(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        amount: int,
        actor_id: int,
        reason: str | None = None,
        now: int | None = None,
        cooldown_seconds: int = TAX_FINE_COOLDOWN_SECONDS,
    ) -> dict:
        """Apply a Tax Man fine capped by audited player obligations."""
        amount = int(amount)
        if amount <= 0:
            return {"status": "invalid_amount"}

        clean_reason = " ".join(reason.split()) if reason else None
        if clean_reason == "":
            clean_reason = None

        try:
            snapshot = self.get_player_snapshot(discord_id, guild_id)
        except ValueError:
            return {"status": "target_not_registered"}

        outstanding_obligations = int(snapshot["effective_obligations"])
        if outstanding_obligations <= 0:
            return {
                "status": "no_outstanding_obligations",
                "discord_id": discord_id,
                "guild_id": guild_id,
                "balance": int(snapshot["balance"]),
            }

        return self.tax_repo.levy_fine_atomic(
            discord_id,
            guild_id,
            amount=amount,
            actor_id=actor_id,
            reason=clean_reason,
            cooldown_seconds=cooldown_seconds,
            max_amount=outstanding_obligations,
            now=now,
        )

    def reset_fine_cooldown(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
    ) -> dict:
        """Reset a player's Tax Man fine cooldown."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if player is None:
            return {"status": "target_not_registered"}

        had_cooldown = self.tax_repo.reset_fine_cooldown(discord_id, guild_id)
        return {
            "status": "ok",
            "discord_id": discord_id,
            "guild_id": player.guild_id,
            "actor_id": actor_id,
            "had_cooldown": had_cooldown,
        }

    def add_bankruptcy_modifier(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        games: int,
        actor_id: int,
        reason: str | None = None,
    ) -> dict:
        """Add bankruptcy penalty games without declaring bankruptcy."""
        games = int(games)
        if games <= 0:
            return {"status": "invalid_games"}
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if player is None:
            return {"status": "target_not_registered"}

        before = self.bankruptcy_repo.get_penalty_games(discord_id, guild_id)
        after = self.bankruptcy_repo.adjust_penalty_games(discord_id, guild_id, games)
        return {
            "status": "ok",
            "action": "add",
            "discord_id": discord_id,
            "guild_id": player.guild_id,
            "games": games,
            "previous_games": before,
            "penalty_games_remaining": after,
            "actor_id": actor_id,
            "reason": " ".join(reason.split()) if reason else None,
        }

    def remove_bankruptcy_modifier(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
        reason: str | None = None,
    ) -> dict:
        """Clear bankruptcy penalty games without touching bankruptcy count."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if player is None:
            return {"status": "target_not_registered"}

        before = self.bankruptcy_repo.get_penalty_games(discord_id, guild_id)
        after = self.bankruptcy_repo.set_penalty_games(discord_id, guild_id, 0)
        return {
            "status": "ok",
            "action": "remove",
            "discord_id": discord_id,
            "guild_id": player.guild_id,
            "previous_games": before,
            "penalty_games_remaining": after,
            "actor_id": actor_id,
            "reason": " ".join(reason.split()) if reason else None,
        }

    def get_player_snapshot(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        ledger_limit: int = 8,
        ledger_offset: int = 0,
    ) -> dict:
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
        prediction_cost_basis = int(
            prediction_exposure.get("summary", {}).get("cost_basis") or 0
        )
        visible_debt = max(0, -balance)
        effective_obligations = (
            visible_debt
            + loan_principal
            + loan_fee
            + dark_bargain_due
            + prediction_cost_basis
        )

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
            "prediction_cost_basis": prediction_cost_basis,
            "effective_obligations": effective_obligations,
            "recent_ledger_total": self.count_ledger_entries(guild_id, user_id=discord_id),
            "recent_ledger_limit": ledger_limit,
            "recent_ledger_offset": ledger_offset,
            "recent_ledger": self.get_recent_ledger(
                guild_id,
                limit=ledger_limit,
                offset=ledger_offset,
                user_id=discord_id,
            ),
        }
