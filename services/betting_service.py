"""
Handles betting-related business logic.
"""

import math
from typing import Any, Dict, List, Optional, TYPE_CHECKING
import time

from config import HOUSE_PAYOUT_MULTIPLIER, JOPACOIN_WIN_REWARD, LEVERAGE_TIERS, MAX_DEBT
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository

if TYPE_CHECKING:
    from services.garnishment_service import GarnishmentService


class BettingService:
    """Encapsulates jopacoin wagering, timing, and house payouts."""

    def __init__(
        self,
        bet_repo: BetRepository,
        player_repo: PlayerRepository,
        garnishment_service: Optional["GarnishmentService"] = None,
        leverage_tiers: Optional[List[int]] = None,
        max_debt: Optional[int] = None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.garnishment_service = garnishment_service
        self.leverage_tiers = leverage_tiers if leverage_tiers is not None else LEVERAGE_TIERS
        self.max_debt = max_debt if max_debt is not None else MAX_DEBT

    def _since_ts(self, pending_state: Optional[Dict[str, Any]]) -> Optional[int]:
        """Derive the start timestamp for the current pending match window."""
        if not pending_state:
            return None
        return pending_state.get("shuffle_timestamp")

    def place_bet(
        self,
        guild_id: Optional[int],
        discord_id: int,
        team: str,
        amount: int,
        pending_state: Dict[str, Any],
        leverage: int = 1,
    ) -> None:
        """Place a bet after verifying timing and participant/team rules."""
        if pending_state is None:
            raise ValueError("No pending match to bet on.")

        now_ts = int(time.time())
        # Fast/strict check first (also preserves test behavior where pending_state is mutated).
        lock_until = pending_state.get("bet_lock_until")
        if lock_until is None or now_ts >= lock_until:
            raise ValueError("Betting is closed for the current match.")

        since_ts = self._since_ts(pending_state)
        if since_ts is None:
            raise ValueError("No pending match to bet on.")
        if team not in self.bet_repo.VALID_TEAMS:
            raise ValueError("Invalid team selection.")

        if amount <= 0:
            raise ValueError("Bet amount must be positive.")

        # Validate leverage tier
        if leverage != 1 and leverage not in self.leverage_tiers:
            valid_tiers = ", ".join(str(t) for t in self.leverage_tiers)
            raise ValueError(f"Invalid leverage. Valid tiers: 1 (none), {valid_tiers}")

        # Prefer atomic placement using DB pending match payload (also enforces lock + team restriction).
        if hasattr(self.bet_repo, "place_bet_against_pending_match_atomic"):
            self.bet_repo.place_bet_against_pending_match_atomic(
                guild_id=guild_id,
                discord_id=discord_id,
                team=team,
                amount=amount,
                bet_time=now_ts,
                leverage=leverage,
                max_debt=self.max_debt,
            )
            return

        # Fallback: enforce timing and team restriction using in-memory pending_state.
        self._enforce_team_restriction(discord_id, team, pending_state)

        # Prefer atomic placement (balance debit + bet insert in one transaction).
        if hasattr(self.bet_repo, "place_bet_atomic"):
            self.bet_repo.place_bet_atomic(
                guild_id=guild_id,
                discord_id=discord_id,
                team=team,
                amount=amount,
                bet_time=now_ts,
                since_ts=int(since_ts),
                leverage=leverage,
                max_debt=self.max_debt,
            )
            return

        # Fallback (older behavior) - doesn't support leverage
        effective_bet = amount * leverage
        balance = self.player_repo.get_balance(discord_id)
        if balance - effective_bet < -self.max_debt:
            raise ValueError(f"Bet would exceed maximum debt limit of {self.max_debt} jopacoin.")

        # Check for existing bets - allow additional bets only on the same team
        existing_bet = self.bet_repo.get_player_pending_bet(guild_id, discord_id, since_ts=since_ts)
        if existing_bet and existing_bet["team_bet_on"] != team:
            existing_team = existing_bet["team_bet_on"].title()
            raise ValueError(
                f"You already have bets on {existing_team}. "
                "You can only add more bets on the same team."
            )

        self.player_repo.add_balance(discord_id, -effective_bet)
        self.bet_repo.create_bet(guild_id, discord_id, team, amount, now_ts)

    def award_participation(self, player_ids: List[int]) -> Dict[int, Dict[str, int]]:
        """
        Give each participant 1 jopacoin for playing.

        Returns dict of {discord_id: {gross, garnished, net}} for each player.
        """
        results: Dict[int, Dict[str, int]] = {}
        if not player_ids:
            return results

        # If garnishment service is available, use it for individual processing
        if self.garnishment_service:
            for pid in player_ids:
                result = self.garnishment_service.add_income(pid, 1)
                results[pid] = result
            return results

        # Otherwise, bulk add without garnishment tracking
        deltas = {pid: 1 for pid in player_ids}
        if hasattr(self.player_repo, "add_balance_many"):
            self.player_repo.add_balance_many(deltas)  # type: ignore[attr-defined]
        else:
            for pid in player_ids:
                self.player_repo.add_balance(pid, 1)

        for pid in player_ids:
            results[pid] = {"gross": 1, "garnished": 0, "net": 1}
        return results

    def settle_bets(
        self, match_id: int, guild_id: Optional[int], winning_team: str, pending_state: Dict[str, Any]
    ) -> Dict[str, List[Dict]]:
        """
        Settle bets based on betting mode.

        House mode: Pay winners 1:1 against the house.
        Pool mode: Winners split the total pool proportionally.
        """
        since_ts = self._since_ts(pending_state)
        if since_ts is None:
            # If no pending state, treat as no bets to avoid pulling stale wagers.
            return {"winners": [], "losers": []}

        betting_mode = pending_state.get("betting_mode", "house")

        # Prefer atomic settlement (payouts + bet tagging in one DB transaction)
        if hasattr(self.bet_repo, "settle_pending_bets_atomic"):
            return self.bet_repo.settle_pending_bets_atomic(
                match_id=match_id,
                guild_id=guild_id,
                since_ts=int(since_ts),
                winning_team=winning_team,
                house_payout_multiplier=HOUSE_PAYOUT_MULTIPLIER,
                betting_mode=betting_mode,
            )

        # Fallback (older behavior) - only supports house mode
        bets = self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts)
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}
        if not bets:
            return distributions

        self.bet_repo.assign_match_id(guild_id, match_id, since_ts=since_ts)

        if betting_mode == "pool":
            return self._settle_pool_bets_fallback(bets, winning_team)
        else:
            return self._settle_house_bets_fallback(bets, winning_team)

    def _settle_house_bets_fallback(
        self, bets: List[Dict], winning_team: str
    ) -> Dict[str, List[Dict]]:
        """House mode fallback: 1:1 payouts."""
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}

        for bet in bets:
            outcome_entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "team": bet["team_bet_on"],
            }
            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(outcome_entry)
                continue

            payout = int(bet["amount"] * (1 + HOUSE_PAYOUT_MULTIPLIER))
            self.player_repo.add_balance(bet["discord_id"], payout)
            outcome_entry["payout"] = payout
            distributions["winners"].append(outcome_entry)

        return distributions

    def _settle_pool_bets_fallback(
        self, bets: List[Dict], winning_team: str
    ) -> Dict[str, List[Dict]]:
        """Pool mode fallback: proportional payouts from total pool."""
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}

        # Calculate totals
        total_pool = sum(bet["amount"] for bet in bets)
        winner_pool = sum(bet["amount"] for bet in bets if bet["team_bet_on"] == winning_team)

        # Edge case: no bets on winning side - refund all bets
        if winner_pool == 0:
            for bet in bets:
                self.player_repo.add_balance(bet["discord_id"], bet["amount"])
                distributions["losers"].append({
                    "discord_id": bet["discord_id"],
                    "amount": bet["amount"],
                    "team": bet["team_bet_on"],
                    "refunded": True,
                })
            return distributions

        for bet in bets:
            outcome_entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "team": bet["team_bet_on"],
            }
            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(outcome_entry)
                continue

            # Proportional payout: (bet_amount / winner_pool) * total_pool
            # Round up to ensure winners never lose fractional coins
            payout = math.ceil((bet["amount"] / winner_pool) * total_pool)
            multiplier = total_pool / winner_pool
            self.player_repo.add_balance(bet["discord_id"], payout)
            outcome_entry["payout"] = payout
            outcome_entry["multiplier"] = multiplier
            distributions["winners"].append(outcome_entry)

        return distributions

    def award_win_bonus(self, winning_ids: List[int]) -> Dict[int, Dict[str, int]]:
        """
        Reward winners with additional jopacoins.

        Returns dict of {discord_id: {gross, garnished, net}} for each player.
        """
        results: Dict[int, Dict[str, int]] = {}
        if not winning_ids:
            return results

        # If garnishment service is available, use it for individual processing
        if self.garnishment_service:
            for pid in winning_ids:
                result = self.garnishment_service.add_income(pid, JOPACOIN_WIN_REWARD)
                results[pid] = result
            return results

        # Otherwise, bulk add without garnishment tracking
        deltas = {pid: JOPACOIN_WIN_REWARD for pid in winning_ids}
        if hasattr(self.player_repo, "add_balance_many"):
            self.player_repo.add_balance_many(deltas)  # type: ignore[attr-defined]
        else:
            for pid in winning_ids:
                self.player_repo.add_balance(pid, JOPACOIN_WIN_REWARD)

        for pid in winning_ids:
            results[pid] = {"gross": JOPACOIN_WIN_REWARD, "garnished": 0, "net": JOPACOIN_WIN_REWARD}
        return results

    def get_pot_odds(self, guild_id: Optional[int], pending_state: Optional[Dict[str, Any]] = None) -> Dict[str, int]:
        """Return current bet totals by team for odds calculation."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return {team: 0 for team in self.bet_repo.VALID_TEAMS}
        return self.bet_repo.get_total_bets_by_guild(guild_id, since_ts=since_ts)

    def get_pending_bet(
        self, guild_id: Optional[int], discord_id: int, pending_state: Optional[Dict[str, Any]] = None
    ) -> Optional[Dict]:
        """Get the pending bet for a player."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return None
        return self.bet_repo.get_player_pending_bet(guild_id, discord_id, since_ts=since_ts)

    def get_pending_bets(
        self, guild_id: Optional[int], discord_id: int, pending_state: Optional[Dict[str, Any]] = None
    ) -> List[Dict]:
        """Get all pending bets for a player, ordered by bet_time."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        return self.bet_repo.get_player_pending_bets(guild_id, discord_id, since_ts=since_ts)

    def refund_pending_bets(self, guild_id: Optional[int], pending_state: Optional[Dict[str, Any]]) -> int:
        """
        Refund all pending bets for the current match window.

        Returns the number of bets refunded.
        """
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return 0
        if hasattr(self.bet_repo, "refund_pending_bets_atomic"):
            return self.bet_repo.refund_pending_bets_atomic(guild_id=guild_id, since_ts=int(since_ts))

        bets = self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts)
        if not bets:
            return 0

        for bet in bets:
            self.player_repo.add_balance(bet["discord_id"], bet["amount"])

        return self.bet_repo.delete_pending_bets(guild_id, since_ts=since_ts)

    def _enforce_team_restriction(self, discord_id: int, team: str, state: Dict[str, Any]) -> None:
        radiant = set(state.get("radiant_team_ids", []))
        dire = set(state.get("dire_team_ids", []))
        if discord_id in radiant and team != "radiant":
            raise ValueError("Participants on Radiant can only bet on Radiant.")
        if discord_id in dire and team != "dire":
            raise ValueError("Participants on Dire can only bet on Dire.")

