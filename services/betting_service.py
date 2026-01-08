"""
Handles betting-related business logic.
"""

import math
import time
from typing import TYPE_CHECKING, Any, Optional

from config import (
    AUTO_BLIND_ENABLED,
    AUTO_BLIND_PERCENTAGE,
    AUTO_BLIND_THRESHOLD,
    HOUSE_PAYOUT_MULTIPLIER,
    JOPACOIN_EXCLUSION_REWARD,
    JOPACOIN_WIN_REWARD,
    LEVERAGE_TIERS,
    MAX_DEBT,
)
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository

if TYPE_CHECKING:
    from services.bankruptcy_service import BankruptcyService
    from services.charity_service import CharityService
    from services.garnishment_service import GarnishmentService


class BettingService:
    """Encapsulates jopacoin wagering, timing, and house payouts."""

    def __init__(
        self,
        bet_repo: BetRepository,
        player_repo: PlayerRepository,
        garnishment_service: Optional["GarnishmentService"] = None,
        leverage_tiers: list[int] | None = None,
        max_debt: int | None = None,
        bankruptcy_service: Optional["BankruptcyService"] = None,
        charity_service: Optional["CharityService"] = None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.garnishment_service = garnishment_service
        self.leverage_tiers = leverage_tiers if leverage_tiers is not None else LEVERAGE_TIERS
        self.max_debt = max_debt if max_debt is not None else MAX_DEBT
        self.bankruptcy_service = bankruptcy_service
        self.charity_service = charity_service

    def _since_ts(self, pending_state: dict[str, Any] | None) -> int | None:
        """Derive the start timestamp for the current pending match window."""
        if not pending_state:
            return None
        return pending_state.get("shuffle_timestamp")

    def place_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: dict[str, Any],
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

        # Calculate odds at placement
        current_totals = self.bet_repo.get_total_bets_by_guild(guild_id, since_ts=int(since_ts))
        total_pool = current_totals["radiant"] + current_totals["dire"]
        team_total = current_totals[team]
        odds_at_placement = total_pool / team_total if team_total > 0 and total_pool > 0 else None

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
                odds_at_placement=odds_at_placement,
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
                odds_at_placement=odds_at_placement,
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

    def award_participation(self, player_ids: list[int]) -> dict[int, dict[str, int]]:
        """
        Give each participant 1 jopacoin for playing.

        Also decrements bankruptcy penalty games for each participant.

        Returns dict of {discord_id: {gross, garnished, net}} for each player.
        """
        results: dict[int, dict[str, int]] = {}
        if not player_ids:
            return results

        # Decrement bankruptcy penalty games for all participants
        if self.bankruptcy_service:
            for pid in player_ids:
                self.bankruptcy_service.on_game_played(pid)

        # If garnishment service is available, use it for individual processing
        if self.garnishment_service:
            for pid in player_ids:
                result = self.garnishment_service.add_income(pid, 1)
                results[pid] = result
            return results

        # Otherwise, bulk add without garnishment tracking
        deltas = dict.fromkeys(player_ids, 1)
        if hasattr(self.player_repo, "add_balance_many"):
            self.player_repo.add_balance_many(deltas)  # type: ignore[attr-defined]
        else:
            for pid in player_ids:
                self.player_repo.add_balance(pid, 1)

        for pid in player_ids:
            results[pid] = {"gross": 1, "garnished": 0, "net": 1}
        return results

    def settle_bets(
        self, match_id: int, guild_id: int | None, winning_team: str, pending_state: dict[str, Any]
    ) -> dict[str, list[dict]]:
        """
        Settle bets based on betting mode.

        House mode: Pay winners 1:1 against the house.
        Pool mode: Winners split the total pool proportionally.
        """
        since_ts = self._since_ts(pending_state)
        if since_ts is None:
            # If no pending state, treat as no bets to avoid pulling stale wagers.
            return {"winners": [], "losers": []}

        betting_mode = pending_state.get("betting_mode", "pool")

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
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}
        if not bets:
            return distributions

        self.bet_repo.assign_match_id(guild_id, match_id, since_ts=since_ts)

        if betting_mode == "pool":
            return self._settle_pool_bets_fallback(bets, winning_team)
        else:
            return self._settle_house_bets_fallback(bets, winning_team)

    def _settle_house_bets_fallback(
        self, bets: list[dict], winning_team: str
    ) -> dict[str, list[dict]]:
        """House mode fallback: 1:1 payouts."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

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
        self, bets: list[dict], winning_team: str
    ) -> dict[str, list[dict]]:
        """Pool mode fallback: proportional payouts from total pool."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

        # Calculate totals
        total_pool = sum(bet["amount"] for bet in bets)
        winner_pool = sum(bet["amount"] for bet in bets if bet["team_bet_on"] == winning_team)

        # Edge case: no bets on winning side - refund all bets
        if winner_pool == 0:
            for bet in bets:
                self.player_repo.add_balance(bet["discord_id"], bet["amount"])
                distributions["losers"].append(
                    {
                        "discord_id": bet["discord_id"],
                        "amount": bet["amount"],
                        "team": bet["team_bet_on"],
                        "refunded": True,
                    }
                )
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

    def award_win_bonus(self, winning_ids: list[int]) -> dict[int, dict[str, int]]:
        """
        Reward winners with additional jopacoins.

        Applies bankruptcy penalty if applicable (reduced reward for players
        who declared bankruptcy).

        Returns dict of {discord_id: {gross, garnished, net, bankruptcy_penalty}} for each player.
        """
        results: dict[int, dict[str, int]] = {}
        if not winning_ids:
            return results

        for pid in winning_ids:
            reward = JOPACOIN_WIN_REWARD
            bankruptcy_penalty = 0

            # Apply bankruptcy penalty if applicable
            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(pid, reward)
                reward = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]

            # Apply garnishment if player has debt
            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, reward)
                result["bankruptcy_penalty"] = bankruptcy_penalty
                result["gross"] = JOPACOIN_WIN_REWARD  # Original before penalty
                results[pid] = result
            else:
                self.player_repo.add_balance(pid, reward)
                results[pid] = {
                    "gross": JOPACOIN_WIN_REWARD,
                    "garnished": 0,
                    "net": reward,
                    "bankruptcy_penalty": bankruptcy_penalty,
                }

        return results

    def award_exclusion_bonus(self, excluded_ids: list[int]) -> dict[int, dict[str, int]]:
        """
        Reward excluded players with a small consolation bonus.

        Mirrors win bonus processing so bankruptcy and garnishment rules still apply.
        """
        results: dict[int, dict[str, int]] = {}
        if not excluded_ids:
            return results

        for pid in excluded_ids:
            reward = JOPACOIN_EXCLUSION_REWARD
            bankruptcy_penalty = 0

            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(pid, reward)
                reward = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]

            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, reward)
                result["bankruptcy_penalty"] = bankruptcy_penalty
                result["gross"] = JOPACOIN_EXCLUSION_REWARD
                results[pid] = result
            else:
                self.player_repo.add_balance(pid, reward)
                results[pid] = {
                    "gross": JOPACOIN_EXCLUSION_REWARD,
                    "garnished": 0,
                    "net": reward,
                    "bankruptcy_penalty": bankruptcy_penalty,
                }

        return results

    def get_pot_odds(
        self, guild_id: int | None, pending_state: dict[str, Any] | None = None
    ) -> dict[str, int]:
        """Return current bet totals by team for odds calculation."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return dict.fromkeys(self.bet_repo.VALID_TEAMS, 0)
        return self.bet_repo.get_total_bets_by_guild(guild_id, since_ts=since_ts)

    def get_pending_bet(
        self, guild_id: int | None, discord_id: int, pending_state: dict[str, Any] | None = None
    ) -> dict | None:
        """Get the pending bet for a player."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return None
        return self.bet_repo.get_player_pending_bet(guild_id, discord_id, since_ts=since_ts)

    def get_pending_bets(
        self, guild_id: int | None, discord_id: int, pending_state: dict[str, Any] | None = None
    ) -> list[dict]:
        """Get all pending bets for a player, ordered by bet_time."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        return self.bet_repo.get_player_pending_bets(guild_id, discord_id, since_ts=since_ts)

    def refund_pending_bets(
        self, guild_id: int | None, pending_state: dict[str, Any] | None
    ) -> int:
        """
        Refund all pending bets for the current match window.

        Returns the number of bets refunded.
        """
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return 0
        if hasattr(self.bet_repo, "refund_pending_bets_atomic"):
            return self.bet_repo.refund_pending_bets_atomic(
                guild_id=guild_id, since_ts=int(since_ts)
            )

        bets = self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts)
        if not bets:
            return 0

        for bet in bets:
            self.player_repo.add_balance(bet["discord_id"], bet["amount"])

        return self.bet_repo.delete_pending_bets(guild_id, since_ts=since_ts)

    def _enforce_team_restriction(self, discord_id: int, team: str, state: dict[str, Any]) -> None:
        radiant = set(state.get("radiant_team_ids", []))
        dire = set(state.get("dire_team_ids", []))
        if discord_id in radiant and team != "radiant":
            raise ValueError("Participants on Radiant can only bet on Radiant.")
        if discord_id in dire and team != "dire":
            raise ValueError("Participants on Dire can only bet on Dire.")

    def create_auto_blind_bets(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        shuffle_timestamp: int,
    ) -> dict[str, Any]:
        """
        Create auto-liquidity blind bets for all eligible players after shuffle.

        Eligible players are those with balance >= AUTO_BLIND_THRESHOLD.
        Each eligible player bets a percentage of their balance (rounded to nearest int)
        on their own team with 1x leverage.

        The percentage is normally AUTO_BLIND_PERCENTAGE (5%), but can be reduced
        to CHARITY_REDUCED_RATE (1%) for players who have earned the charity bonus
        by helping others pay off their debt.

        Args:
            guild_id: The guild ID (or None for DMs)
            radiant_ids: List of Discord IDs on Radiant team
            dire_ids: List of Discord IDs on Dire team
            shuffle_timestamp: The shuffle timestamp for bet timing

        Returns:
            {
                "created": int,
                "total_radiant": int,
                "total_dire": int,
                "bets": [{"discord_id": int, "team": str, "amount": int, "is_reduced_rate": bool}, ...],
                "skipped": [{"discord_id": int, "reason": str}, ...]
            }
        """
        if not AUTO_BLIND_ENABLED:
            return {
                "created": 0,
                "total_radiant": 0,
                "total_dire": 0,
                "bets": [],
                "skipped": [],
            }

        result: dict[str, Any] = {
            "created": 0,
            "total_radiant": 0,
            "total_dire": 0,
            "bets": [],
            "skipped": [],
        }

        # Process each team
        for team, player_ids in [("radiant", radiant_ids), ("dire", dire_ids)]:
            for discord_id in player_ids:
                try:
                    balance = self.player_repo.get_balance(discord_id)

                    # Skip players below threshold
                    if balance < AUTO_BLIND_THRESHOLD:
                        result["skipped"].append({
                            "discord_id": discord_id,
                            "reason": f"balance {balance} < threshold {AUTO_BLIND_THRESHOLD}",
                        })
                        continue

                    # Get the blind rate for this player (reduced if they have charity bonus)
                    if self.charity_service:
                        blind_rate = self.charity_service.get_blind_rate_for_player(discord_id)
                        is_reduced_rate = self.charity_service.has_reduced_rate(discord_id)
                    else:
                        blind_rate = AUTO_BLIND_PERCENTAGE
                        is_reduced_rate = False

                    # Calculate blind amount (round to nearest integer)
                    blind_amount = round(balance * blind_rate)

                    # Skip if rounded amount is less than 1
                    if blind_amount < 1:
                        result["skipped"].append({
                            "discord_id": discord_id,
                            "reason": f"blind amount {blind_amount} < 1",
                        })
                        continue

                    # Calculate current odds for this team before placing bet
                    current_totals = self.bet_repo.get_total_bets_by_guild(
                        guild_id, since_ts=shuffle_timestamp
                    )
                    total_pool = current_totals["radiant"] + current_totals["dire"]
                    team_total = current_totals[team]

                    # Odds at placement: what multiplier you'd get if you win
                    # If no bets yet, odds are undefined (will be calculated when more bets come in)
                    if team_total > 0:
                        # After this bet, total_pool increases and team_total increases
                        # Show the odds that existed before this bet
                        odds_at_placement = total_pool / team_total if total_pool > 0 else None
                    else:
                        # First bet on this team - no meaningful odds yet
                        odds_at_placement = None

                    # Place the blind bet
                    self.bet_repo.place_bet_atomic(
                        guild_id=guild_id,
                        discord_id=discord_id,
                        team=team,
                        amount=blind_amount,
                        bet_time=shuffle_timestamp,
                        since_ts=shuffle_timestamp,
                        leverage=1,
                        max_debt=self.max_debt,
                        is_blind=True,
                        is_reduced_rate=is_reduced_rate,
                        odds_at_placement=odds_at_placement,
                    )

                    # Decrement charity games remaining after blind bet created
                    if is_reduced_rate and self.charity_service:
                        self.charity_service.on_blind_bet_created(discord_id)

                    result["created"] += 1
                    result["bets"].append({
                        "discord_id": discord_id,
                        "team": team,
                        "amount": blind_amount,
                        "is_reduced_rate": is_reduced_rate,
                    })
                    if team == "radiant":
                        result["total_radiant"] += blind_amount
                    else:
                        result["total_dire"] += blind_amount

                except ValueError as e:
                    result["skipped"].append({
                        "discord_id": discord_id,
                        "reason": str(e),
                    })

        return result

    def get_all_pending_bets(
        self, guild_id: int | None, pending_state: dict[str, Any] | None = None
    ) -> list[dict]:
        """Get all pending bets for a guild (for /bets command)."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        return self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts)
