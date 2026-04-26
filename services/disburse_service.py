"""
Service for handling nonprofit fund disbursement voting and distribution.

Players vote on how to distribute collected loan fees to players with negative balances.
"""

from __future__ import annotations

import math
import random
import threading
import time
from dataclasses import dataclass, field

from config import DISBURSE_MIN_FUND, DISBURSE_QUORUM_PERCENTAGE, LOTTERY_ACTIVITY_DAYS
from repositories.disburse_repository import DisburseRepository
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository


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
        "lottery": 0, "social_security": 0, "richest": 0, "cancel": 0
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

    METHODS = ("even", "proportional", "neediest", "stimulus", "lottery", "social_security", "richest", "cancel")
    METHOD_LABELS = {
        "even": "Even Split",
        "proportional": "Proportional",
        "neediest": "Neediest First",
        "stimulus": "Stimulus",
        "lottery": "Lottery",
        "social_security": "Social Security",
        "richest": "Richest",
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
        # Per-guild serialization lock for execute/force_execute/reset paths.
        # Prevents the reserve-then-distribute sequence (add_to_nonprofit_fund
        # followed by disburse_fund_atomic) from interleaving between two
        # concurrent callers, which would otherwise over-refund the pool.
        # Also closes the quorum-check -> execute TOCTOU window: once a caller
        # holds the lock, a late "cancel" vote cannot change the observed
        # state between the check and the atomic distribution.
        self._guild_locks_mutex = threading.Lock()
        self._guild_locks: dict[int, threading.RLock] = {}

    def _get_guild_lock(self, guild_id: int | None) -> threading.RLock:
        """Return the (lazily created) lock for a guild, keyed on normalized id."""
        normalized = guild_id if guild_id is not None else 0
        with self._guild_locks_mutex:
            lock = self._guild_locks.get(normalized)
            if lock is None:
                lock = threading.RLock()
                self._guild_locks[normalized] = lock
            return lock

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
        player_count = self.player_repo.get_registered_player_count(guild_id)
        quorum_required = max(1, math.ceil(player_count * self.quorum_percentage))

        # Atomically read and deduct the entire nonprofit fund to prevent race
        # conditions where fund_amount could change between read and deduct.
        fund_amount = self.loan_repo.get_and_deduct_nonprofit_fund_atomic(
            guild_id, min_amount=self.min_fund
        )

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
                "lottery": 0, "social_security": 0, "richest": 0, "cancel": 0
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

    def _determine_winner(self, votes: dict[str, int]) -> str | None:
        """
        Determine the winning method from vote counts.

        Tie-breaker order: even > proportional > neediest > stimulus > lottery > social_security > cancel

        Returns:
            Winning method name, or None if no votes at all.
        """
        if sum(votes.values()) == 0:
            return None

        max_votes = max(votes.values())
        winners = [m for m, v in votes.items() if v == max_votes]

        for method in ("even", "proportional", "neediest", "stimulus", "lottery", "social_security", "richest", "cancel"):
            if method in winners:
                return method

        return winners[0]  # Fallback (shouldn't happen)

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

        winner = self._determine_winner(proposal.votes)
        return True, winner

    def execute_disbursement(self, guild_id: int | None) -> dict:
        """
        Execute the disbursement based on winning method.

        Serialized per-guild to prevent the reserve-restore + distribute race
        (two concurrent callers returning the reserved fund twice) and the
        quorum-check TOCTOU window (a late vote changing the observed state
        between the check and the execution).

        Returns:
            dict with disbursement details
        """
        with self._get_guild_lock(guild_id):
            # Re-check quorum INSIDE the lock so a late vote cannot cause a
            # second caller to re-enter the distribution path after the first
            # one has already reserved / started executing.
            quorum_reached, method = self.check_quorum(guild_id)
            if not quorum_reached:
                raise ValueError("Quorum not reached")

            proposal = self.get_proposal(guild_id)
            if not proposal:
                raise ValueError("No active proposal")

            return self._execute_locked(guild_id, proposal, method, forced=False)

    def _execute_locked(
        self, guild_id: int | None, proposal: DisburseProposal, method: str, *, forced: bool
    ) -> dict:
        """
        Perform the reserve-restore + distribute sequence under the guild lock.

        Assumes ``self._get_guild_lock(guild_id)`` is already held. Atomically
        transitions the proposal out of the 'active' state before touching
        balances so a concurrent caller will find no active proposal to act on.
        """
        fund_amount = proposal.fund_amount

        # Handle cancel specially - reset proposal instead of distributing.
        # The reset + fund credit happen in a single BEGIN IMMEDIATE so a crash
        # between them cannot strand the reserve.
        if method == "cancel":
            was_active = self.disburse_repo.reset_and_return_fund_atomic(
                guild_id, fund_amount,
            )
            if not was_active:
                # Another caller already handled this proposal. No-op.
                raise ValueError("No active proposal")
            return {
                "success": True,
                "method": "cancel",
                "method_label": "Cancel",
                "total_disbursed": 0,
                "distributions": [],
                "cancelled": True,
                "message": (
                    "Proposal cancelled by admin. Funds returned to nonprofit."
                    if forced
                    else "Proposal cancelled by vote. Funds returned to nonprofit."
                ),
            }

        # Calculate distributions based on winning method
        if method == "stimulus":
            eligible = self.player_repo.get_stimulus_eligible_players(guild_id)
            if not eligible:
                self._finalize_noop(guild_id, fund_amount)
                return self._empty_result(
                    method,
                    message=(
                        "No eligible players for stimulus."
                        if forced
                        else "No eligible players for stimulus (need 4+ non-debtor players)."
                    ),
                )
            distributions = self._calculate_stimulus_distribution(fund_amount, eligible)
        elif method == "lottery":
            eligible = self.player_repo.get_all_registered_players_for_lottery(
                guild_id, activity_days=LOTTERY_ACTIVITY_DAYS
            )
            if not eligible:
                self._finalize_noop(guild_id, fund_amount)
                return self._empty_result(
                    method,
                    message=(
                        f"No active players for lottery (last {LOTTERY_ACTIVITY_DAYS} days)."
                        if forced
                        else f"No active players for lottery (must have played in the last {LOTTERY_ACTIVITY_DAYS} days)."
                    ),
                )
            distributions = self._calculate_lottery_distribution(fund_amount, eligible)
        elif method == "social_security":
            eligible = self.player_repo.get_players_by_games_played(guild_id)
            if not eligible:
                self._finalize_noop(guild_id, fund_amount)
                return self._empty_result(
                    method, message="No players with games played for social security."
                )
            distributions = self._calculate_social_security_distribution(fund_amount, eligible)
        elif method == "richest":
            richest = self.player_repo.get_richest_player(guild_id)
            if not richest:
                self._finalize_noop(guild_id, fund_amount)
                return self._empty_result(
                    method, message="No players found for richest distribution."
                )
            distributions = self._calculate_richest_distribution(fund_amount, richest)
        else:
            # Debtor-based methods: even, proportional, neediest
            debtors = self.player_repo.get_players_with_negative_balance(guild_id)
            if not debtors:
                self._finalize_noop(guild_id, fund_amount)
                return self._empty_result(
                    method, message="No players with negative balance to receive funds."
                )

            if method == "even":
                distributions = self._calculate_even_distribution(fund_amount, debtors)
            elif method == "proportional":
                distributions = self._calculate_proportional_distribution(fund_amount, debtors)
            else:  # neediest
                distributions = self._calculate_neediest_distribution(fund_amount, debtors)

        # Finalize everything in one BEGIN IMMEDIATE: complete the proposal,
        # return the reserve, deduct the distribution total, credit recipients,
        # and record history. This closes the crash-window where the proposal
        # was marked complete but a crash between fund-return and distribution
        # left the pool over-restored and no history recorded. Combined with the
        # per-guild RLock above, it also blocks in-process races.
        total_disbursed = self.disburse_repo.complete_and_disburse_atomic(
            guild_id=guild_id,
            fund_amount_to_return=fund_amount,
            distributions=distributions,
            method=method,
        )

        return {
            "success": True,
            "method": method,
            "method_label": self.METHOD_LABELS[method],
            "total_disbursed": total_disbursed,
            "distributions": distributions,
            "recipient_count": len(distributions),
        }

    def _finalize_noop(self, guild_id: int | None, fund_amount: int) -> None:
        """Return reserved fund to pool and mark proposal complete (empty distribution).

        Routes through the same atomic path as the happy case so a crash
        between completing the proposal and crediting the fund cannot leave
        the reserve permanently stranded.
        """
        self.disburse_repo.complete_and_disburse_atomic(
            guild_id=guild_id,
            fund_amount_to_return=fund_amount,
            distributions=[],
            method="noop",
        )

    def _empty_result(self, method: str, *, message: str) -> dict:
        return {
            "success": True,
            "method": method,
            "method_label": self.METHOD_LABELS[method],
            "total_disbursed": 0,
            "distributions": [],
            "message": message,
        }

    def force_execute(self, guild_id: int | None) -> dict:
        """
        Admin-only: force-execute the active proposal using the current leading method.

        Bypasses quorum requirement. Requires at least one vote.

        Serialized per-guild (shares the lock with ``execute_disbursement`` and
        ``reset_proposal``) so admin force-execute cannot race vote-triggered
        execution on the same proposal.

        Returns:
            dict with disbursement details (same format as execute_disbursement)

        Raises:
            ValueError if no active proposal or no votes cast
        """
        with self._get_guild_lock(guild_id):
            proposal = self.get_proposal(guild_id)
            if not proposal:
                raise ValueError("No active proposal")

            method = self._determine_winner(proposal.votes)
            if not method:
                raise ValueError("No votes have been cast yet")

            return self._execute_locked(guild_id, proposal, method, forced=True)

    def reset_proposal(self, guild_id: int | None) -> bool:
        """
        Reset (cancel) the active proposal. Admin only.

        Returns reserved funds to the nonprofit fund.

        Serialized per-guild so admin reset cannot race vote-triggered
        execution on the same proposal (which would double-refund the reserve).

        Returns:
            True if a proposal was reset, False if none active
        """
        with self._get_guild_lock(guild_id):
            proposal_data = self.disburse_repo.get_active_proposal(guild_id)
            if not proposal_data:
                return False
            fund_amount = proposal_data["fund_amount"]
            # Single BEGIN IMMEDIATE: status flip + nonprofit credit. A crash
            # between the two would otherwise destroy ``fund_amount`` JC.
            return self.disburse_repo.reset_and_return_fund_atomic(
                guild_id, fund_amount,
            )

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

    def _calculate_richest_distribution(
        self, fund: int, richest: dict
    ) -> list[tuple[int, int]]:
        """
        All funds go to the richest player (reverse of neediest).

        Winner takes all - no cap. The rich get richer.
        """
        if not richest:
            return []

        return [(richest["discord_id"], fund)]

    def get_individual_votes(self, guild_id: int | None) -> dict[int, str]:
        """
        Get individual votes for the active proposal in a guild.

        Args:
            guild_id: Guild ID

        Returns:
            Dict mapping discord_id -> vote method
        """
        return self.disburse_repo.get_individual_votes(guild_id)
