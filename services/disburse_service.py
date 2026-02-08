"""
Service for handling nonprofit fund disbursement voting and distribution.

Players vote on how to distribute collected loan fees to players with negative balances.
"""

from __future__ import annotations

import math
import random
import time
from dataclasses import dataclass, field

from config import DISBURSE_MIN_FUND, DISBURSE_QUORUM_PERCENTAGE
from repositories.disburse_repository import DisburseRepository
from repositories.player_repository import PlayerRepository
from services.loan_service import LoanRepository


@dataclass
class DisburseProposal:
    """Active disbursement proposal."""

    guild_id: int
    proposal_id: int
    message_id: int | None
    channel_id: int | None
    fund_amount: int
    quorum_required: int
    status: str
    votes: dict[str, int] = field(default_factory=lambda: {
        "even": 0, "proportional": 0, "neediest": 0, "stimulus": 0,
        "lottery": 0, "social_security": 0, "cancel": 0
    })

    @property
    def total_votes(self) -> int:
        return sum(self.votes.values())

    @property
    def quorum_reached(self) -> bool:
        return self.total_votes >= self.quorum_required

    @property
    def quorum_progress(self) -> float:
        if self.quorum_required == 0:
            return 1.0
        return self.total_votes / self.quorum_required


class DisburseService:
    """
    Handles nonprofit fund disbursement voting and distribution.

    Voting Methods:
    - even: Split equally among all negative balance players (capped at debt)
    - proportional: Split based on debt amount (more debt = more funds, capped)
    - neediest: All funds go to player with most debt (capped)
    """

    METHODS = ("even", "proportional", "neediest", "stimulus", "lottery", "social_security", "cancel")
    METHOD_LABELS = {
        "even": "Even Split",
        "proportional": "Proportional",
        "neediest": "Neediest First",
        "stimulus": "Stimulus",
        "lottery": "Lottery",
        "social_security": "Social Security",
        "cancel": "Cancel",
    }

    def __init__(
        self,
        disburse_repo: DisburseRepository,
        player_repo: PlayerRepository,
        loan_repo: LoanRepository,
        min_fund: int | None = None,
        quorum_percentage: float | None = None,
    ):
        self.disburse_repo = disburse_repo
        self.player_repo = player_repo
        self.loan_repo = loan_repo
        self.min_fund = min_fund if min_fund is not None else DISBURSE_MIN_FUND
        self.quorum_percentage = (
            quorum_percentage if quorum_percentage is not None else DISBURSE_QUORUM_PERCENTAGE
        )

    def can_propose(self, guild_id: int | None) -> tuple[bool, str]:
        """
        Check if a disbursement proposal can be created.

        Returns:
            (allowed, reason) - reason is empty string if allowed
        """
        # Check for existing active proposal
        existing = self.disburse_repo.get_active_proposal(guild_id)
        if existing:
            return False, "active_proposal_exists"

        # Check minimum fund requirement
        fund = self.loan_repo.get_nonprofit_fund(guild_id)
        if fund < self.min_fund:
            return False, f"insufficient_fund:{fund}:{self.min_fund}"

        # Check for players with negative balance OR stimulus-eligible players
        debtors = self.player_repo.get_players_with_negative_balance(guild_id)
        stimulus_eligible = self.player_repo.get_stimulus_eligible_players(guild_id)
        if not debtors and not stimulus_eligible:
            return False, "no_eligible_recipients"

        return True, ""

    def create_proposal(self, guild_id: int | None) -> DisburseProposal:
        """
        Create a new disbursement proposal.

        Raises:
            ValueError if proposal cannot be created
        """
        can, reason = self.can_propose(guild_id)
        if not can:
            raise ValueError(f"Cannot create proposal: {reason}")

        proposal_id = int(time.time())
        fund_amount = self.loan_repo.get_nonprofit_fund(guild_id)
        player_count = self.player_repo.get_registered_player_count(guild_id)
        quorum_required = max(1, math.ceil(player_count * self.quorum_percentage))

        self.disburse_repo.create_proposal(
            guild_id=guild_id,
            proposal_id=proposal_id,
            fund_amount=fund_amount,
            quorum_required=quorum_required,
        )

        return DisburseProposal(
            guild_id=guild_id if guild_id is not None else 0,
            proposal_id=proposal_id,
            message_id=None,
            channel_id=None,
            fund_amount=fund_amount,
            quorum_required=quorum_required,
            status="active",
            votes={
                "even": 0, "proportional": 0, "neediest": 0, "stimulus": 0,
                "lottery": 0, "social_security": 0, "cancel": 0
            },
        )

    def set_proposal_message(
        self, guild_id: int | None, message_id: int, channel_id: int
    ) -> None:
        """Set the Discord message ID for an active proposal."""
        self.disburse_repo.set_proposal_message(guild_id, message_id, channel_id)

    def get_proposal(self, guild_id: int | None) -> DisburseProposal | None:
        """Get the active proposal for a guild."""
        data = self.disburse_repo.get_active_proposal(guild_id)
        if not data:
            return None

        votes = self.disburse_repo.get_vote_counts(guild_id)

        return DisburseProposal(
            guild_id=data["guild_id"],
            proposal_id=data["proposal_id"],
            message_id=data["message_id"],
            channel_id=data["channel_id"],
            fund_amount=data["fund_amount"],
            quorum_required=data["quorum_required"],
            status=data["status"],
            votes=votes,
        )

    def add_vote(
        self, guild_id: int | None, discord_id: int, method: str
    ) -> dict:
        """
        Add or change a vote on a disbursement proposal.

        Args:
            guild_id: Guild ID
            discord_id: Voter's Discord ID
            method: 'even', 'proportional', or 'neediest'

        Returns:
            dict with vote state and quorum info
        """
        if method not in self.METHODS:
            raise ValueError(f"Invalid method: {method}")

        proposal = self.get_proposal(guild_id)
        if not proposal:
            raise ValueError("No active proposal")

        # Record vote
        self.disburse_repo.add_vote(
            guild_id=guild_id,
            proposal_id=proposal.proposal_id,
            discord_id=discord_id,
            method=method,
        )

        # Get updated state
        votes = self.disburse_repo.get_vote_counts(guild_id)
        total = sum(votes.values())

        return {
            "votes": votes,
            "total_votes": total,
            "quorum_required": proposal.quorum_required,
            "quorum_reached": total >= proposal.quorum_required,
            "quorum_progress": total / proposal.quorum_required if proposal.quorum_required > 0 else 1.0,
        }

    def check_quorum(self, guild_id: int | None) -> tuple[bool, str | None]:
        """
        Check if quorum has been reached and determine winning method.

        Returns:
            (quorum_reached, winning_method) - method is None if quorum not reached
        """
        proposal = self.get_proposal(guild_id)
        if not proposal:
            return False, None

        if not proposal.quorum_reached:
            return False, None

        # Determine winner (with even as tie-breaker)
        votes = proposal.votes
        max_votes = max(votes.values())

        # Get all methods with max votes
        winners = [m for m, v in votes.items() if v == max_votes]

        # Tie-breaker order: even > proportional > neediest > stimulus > lottery > social_security > cancel
        for method in ("even", "proportional", "neediest", "stimulus", "lottery", "social_security", "cancel"):
            if method in winners:
                return True, method

        # Fallback (shouldn't happen)
        return True, winners[0]

    def execute_disbursement(self, guild_id: int | None) -> dict:
        """
        Execute the disbursement based on winning method.

        Returns:
            dict with disbursement details
        """
        quorum_reached, method = self.check_quorum(guild_id)
        if not quorum_reached:
            raise ValueError("Quorum not reached")

        proposal = self.get_proposal(guild_id)
        if not proposal:
            raise ValueError("No active proposal")

        fund_amount = proposal.fund_amount

        # Handle cancel specially - reset proposal instead of distributing
        if method == "cancel":
            self.disburse_repo.reset_proposal(guild_id)
            return {
                "success": True,
                "method": "cancel",
                "method_label": "Cancel",
                "total_disbursed": 0,
                "distributions": [],
                "cancelled": True,
                "message": "Proposal cancelled by vote. Funds remain in nonprofit.",
            }

        # Handle stimulus separately - different eligibility criteria
        if method == "stimulus":
            eligible = self.player_repo.get_stimulus_eligible_players(guild_id)
            if not eligible:
                self.disburse_repo.complete_proposal(guild_id)
                return {
                    "success": True,
                    "method": method,
                    "method_label": self.METHOD_LABELS[method],
                    "total_disbursed": 0,
                    "distributions": [],
                    "message": "No eligible players for stimulus (need 4+ non-debtor players).",
                }
            distributions = self._calculate_stimulus_distribution(fund_amount, eligible)
        elif method == "lottery":
            # Lottery: pick one random registered player, winner takes all
            eligible = self.player_repo.get_all_registered_players_for_lottery(guild_id)
            if not eligible:
                self.disburse_repo.complete_proposal(guild_id)
                return {
                    "success": True,
                    "method": method,
                    "method_label": self.METHOD_LABELS[method],
                    "total_disbursed": 0,
                    "distributions": [],
                    "message": "No registered players for lottery.",
                }
            distributions = self._calculate_lottery_distribution(fund_amount, eligible)
        elif method == "social_security":
            # Social security: distribute proportional to games played
            eligible = self.player_repo.get_players_by_games_played(guild_id)
            if not eligible:
                self.disburse_repo.complete_proposal(guild_id)
                return {
                    "success": True,
                    "method": method,
                    "method_label": self.METHOD_LABELS[method],
                    "total_disbursed": 0,
                    "distributions": [],
                    "message": "No players with games played for social security.",
                }
            distributions = self._calculate_social_security_distribution(fund_amount, eligible)
        else:
            # Debtor-based methods: even, proportional, neediest
            debtors = self.player_repo.get_players_with_negative_balance(guild_id)
            if not debtors:
                self.disburse_repo.complete_proposal(guild_id)
                return {
                    "success": True,
                    "method": method,
                    "method_label": self.METHOD_LABELS[method],
                    "total_disbursed": 0,
                    "distributions": [],
                    "message": "No players with negative balance to receive funds.",
                }

            if method == "even":
                distributions = self._calculate_even_distribution(fund_amount, debtors)
            elif method == "proportional":
                distributions = self._calculate_proportional_distribution(fund_amount, debtors)
            else:  # neediest
                distributions = self._calculate_neediest_distribution(fund_amount, debtors)

        # Execute atomic disbursement
        total_disbursed = self.loan_repo.disburse_fund_atomic(guild_id, distributions)

        # Record history
        self.disburse_repo.record_disbursement(
            guild_id=guild_id,
            total_amount=total_disbursed,
            method=method,
            distributions=distributions,
        )

        # Mark proposal as completed
        self.disburse_repo.complete_proposal(guild_id)

        return {
            "success": True,
            "method": method,
            "method_label": self.METHOD_LABELS[method],
            "total_disbursed": total_disbursed,
            "distributions": distributions,
            "recipient_count": len(distributions),
        }

    def reset_proposal(self, guild_id: int | None) -> bool:
        """
        Reset (cancel) the active proposal. Admin only.

        Returns:
            True if a proposal was reset, False if none active
        """
        return self.disburse_repo.reset_proposal(guild_id)

    def get_last_disbursement(self, guild_id: int | None) -> dict | None:
        """Get the most recent disbursement for display in /nonprofit."""
        return self.disburse_repo.get_last_disbursement(guild_id)

    def _calculate_even_distribution(
        self, fund: int, debtors: list[dict]
    ) -> list[tuple[int, int]]:
        """
        Split funds equally among debtors, capped at each player's debt.

        Excess from capped players is redistributed to those with more debt.
        """
        if not debtors:
            return []

        # Create mutable tracking dict: {discord_id: {"debt": abs(balance), "received": 0}}
        tracking = {
            d["discord_id"]: {"debt": abs(d["balance"]), "received": 0}
            for d in debtors
        }

        remaining = fund
        unfilled_ids = set(tracking.keys())

        while remaining > 0 and unfilled_ids:
            per_player = remaining // len(unfilled_ids)
            if per_player == 0:
                # Distribute remaining 1 at a time
                per_player = 1

            still_unfilled = set()
            distributed_this_round = 0

            for discord_id in unfilled_ids:
                info = tracking[discord_id]
                need = info["debt"] - info["received"]

                if need <= 0:
                    continue  # Already filled

                give = min(per_player, need, remaining - distributed_this_round)
                if give > 0:
                    info["received"] += give
                    distributed_this_round += give

                    # Check if still needs more
                    if info["received"] < info["debt"]:
                        still_unfilled.add(discord_id)

            remaining -= distributed_this_round
            unfilled_ids = still_unfilled

            # Safety: if no progress made, break
            if distributed_this_round == 0:
                break

        # Build result
        return [
            (discord_id, info["received"])
            for discord_id, info in tracking.items()
            if info["received"] > 0
        ]

    def _calculate_proportional_distribution(
        self, fund: int, debtors: list[dict]
    ) -> list[tuple[int, int]]:
        """
        Split funds proportionally by debt amount, capped at each player's debt.
        """
        if not debtors:
            return []

        total_debt = sum(abs(d["balance"]) for d in debtors)
        if total_debt == 0:
            return self._calculate_even_distribution(fund, debtors)

        # Calculate proportional shares (capped at debt)
        distributions = []
        remaining = fund

        # Sort by debt descending for consistent allocation
        sorted_debtors = sorted(debtors, key=lambda d: d["balance"])

        for i, debtor in enumerate(sorted_debtors):
            debt = abs(debtor["balance"])

            if i == len(sorted_debtors) - 1:
                # Last player gets all remaining (up to their debt)
                amount = min(remaining, debt)
            else:
                # Calculate proportional share
                share = int((debt / total_debt) * fund)
                amount = min(share, debt, remaining)

            if amount > 0:
                distributions.append((debtor["discord_id"], amount))
                remaining -= amount

        return distributions

    def _calculate_neediest_distribution(
        self, fund: int, debtors: list[dict]
    ) -> list[tuple[int, int]]:
        """
        All funds go to the player with the most debt, capped at their debt.
        """
        if not debtors:
            return []

        # Find player with lowest (most negative) balance
        neediest = min(debtors, key=lambda d: d["balance"])
        debt = abs(neediest["balance"])
        amount = min(fund, debt)

        return [(neediest["discord_id"], amount)]

    def _calculate_stimulus_distribution(
        self, fund: int, eligible: list[dict]
    ) -> list[tuple[int, int]]:
        """
        Split funds evenly among stimulus-eligible players (non-debtors excluding top 3).

        Unlike debt-based methods, there's no cap - all funds are distributed.
        """
        if not eligible:
            return []

        # Even split - integer division
        per_player = fund // len(eligible)
        remainder = fund % len(eligible)

        distributions = []
        for i, player in enumerate(eligible):
            # Give 1 extra to first 'remainder' players to distribute remainder
            amount = per_player + (1 if i < remainder else 0)
            if amount > 0:
                distributions.append((player["discord_id"], amount))

        return distributions

    def _calculate_lottery_distribution(
        self, fund: int, players: list[dict]
    ) -> list[tuple[int, int]]:
        """
        Randomly select one registered player to receive the entire fund.

        Winner takes all - no debt cap. Any registered player can win.
        """
        if not players:
            return []

        winner = random.choice(players)
        return [(winner["discord_id"], fund)]

    def _calculate_social_security_distribution(
        self, fund: int, players: list[dict]
    ) -> list[tuple[int, int]]:
        """
        Distribute funds proportionally to games played.

        Players with more games played receive more funds.
        No cap - all funds are distributed. Rewards veterans.
        """
        if not players:
            return []

        total_games = sum(p["games_played"] for p in players)
        if total_games == 0:
            return []

        distributions = []
        remaining = fund

        # Sort by games played descending for consistent allocation
        sorted_players = sorted(players, key=lambda p: -p["games_played"])

        for i, player in enumerate(sorted_players):
            if i == len(sorted_players) - 1:
                # Last player gets all remaining
                amount = remaining
            else:
                # Calculate proportional share
                amount = int((player["games_played"] / total_games) * fund)

            if amount > 0:
                distributions.append((player["discord_id"], amount))
                remaining -= amount

        return distributions

    def get_individual_votes(self, guild_id: int | None) -> dict[int, str]:
        """
        Get individual votes for the active proposal in a guild.

        Args:
            guild_id: Guild ID

        Returns:
            Dict mapping discord_id -> vote method
        """
        return self.disburse_repo.get_individual_votes(guild_id)
