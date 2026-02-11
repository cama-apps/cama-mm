"""
Match voting service.

Handles vote tracking for match result recording and aborting.
"""

from typing import Any

from services.match_state_service import MatchStateService


class MatchVotingService:
    """
    Manages voting for match results and abort requests.

    This service handles:
    - Adding record submissions (radiant/dire votes)
    - Adding abort submissions
    - Checking if enough votes to record or abort
    - Vote counting and threshold logic
    """

    MIN_NON_ADMIN_SUBMISSIONS = 3

    def __init__(self, state_service: MatchStateService):
        """
        Initialize MatchVotingService.

        Args:
            state_service: MatchStateService for state access
        """
        self.state_service = state_service

    def has_admin_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """
        Check if an admin has submitted a result vote (radiant/dire).

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            True if an admin has voted for radiant or dire
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return False
        submissions = state.get("record_submissions", {})
        return any(
            sub.get("is_admin") and sub.get("result") in ("radiant", "dire")
            for sub in submissions.values()
        )

    def has_admin_abort_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """
        Check if an admin has submitted an abort vote.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            True if an admin has voted to abort
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return False
        submissions = state.get("record_submissions", {})
        return any(
            sub.get("is_admin") and sub.get("result") == "abort" for sub in submissions.values()
        )

    def add_record_submission(
        self, guild_id: int | None, user_id: int, result: str, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """
        Add a vote for the match result.

        Thread-safe: Uses state_lock() for atomic read-modify-write.

        Args:
            guild_id: Guild ID
            user_id: Discord user ID of the voter
            result: "radiant" or "dire"
            is_admin: Whether the voter is an admin
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            Dict with vote counts, readiness status, and current result

        Raises:
            ValueError: If result is invalid or user already voted differently
        """
        if result not in ("radiant", "dire"):
            raise ValueError("Result must be 'radiant' or 'dire'.")

        # Acquire lock for entire read-modify-write cycle
        with self.state_service.state_lock():
            state = self.state_service.ensure_pending_state(guild_id, pending_match_id)
            submissions = self.state_service.ensure_record_submissions(state)
            existing = submissions.get(user_id)
            if existing and existing["result"] != result:
                raise ValueError("You already submitted a different result.")
            # Allow conflicting votes - requires MIN_NON_ADMIN_SUBMISSIONS matching submissions
            submissions[user_id] = {"result": result, "is_admin": is_admin}
            self.state_service.persist_state(guild_id, state)
            pmid = state.get("pending_match_id")
            vote_counts = self.get_vote_counts(guild_id, pmid)
            return {
                "non_admin_count": self.get_non_admin_submission_count(guild_id, pmid),
                "total_count": len(submissions),
                "result": self.get_pending_record_result(guild_id, pmid),
                "is_ready": self.can_record_match(guild_id, pmid),
                "vote_counts": vote_counts,
                "pending_match_id": pmid,
            }

    def get_non_admin_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """
        Get count of non-admin votes for radiant or dire.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            Number of non-admin result votes
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return 0
        submissions = state.get("record_submissions", {})
        return sum(
            1
            for sub in submissions.values()
            if not sub.get("is_admin") and sub.get("result") in ("radiant", "dire")
        )

    def get_abort_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """
        Get count of non-admin votes to abort.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            Number of non-admin abort votes
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return 0
        submissions = state.get("record_submissions", {})
        return sum(
            1
            for sub in submissions.values()
            if not sub.get("is_admin") and sub.get("result") == "abort"
        )

    def can_abort_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """
        Check if there are enough votes to abort the match.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            True if abort threshold is met (admin vote or MIN_NON_ADMIN_SUBMISSIONS)
        """
        if self.has_admin_abort_submission(guild_id, pending_match_id):
            return True
        return self.get_abort_submission_count(guild_id, pending_match_id) >= self.MIN_NON_ADMIN_SUBMISSIONS

    def add_abort_submission(
        self, guild_id: int | None, user_id: int, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """
        Add a vote to abort the match.

        Thread-safe: Uses state_lock() for atomic read-modify-write.

        Args:
            guild_id: Guild ID
            user_id: Discord user ID of the voter
            is_admin: Whether the voter is an admin
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            Dict with vote counts and readiness status

        Raises:
            ValueError: If user already voted for a different result
        """
        # Acquire lock for entire read-modify-write cycle
        with self.state_service.state_lock():
            state = self.state_service.ensure_pending_state(guild_id, pending_match_id)
            submissions = self.state_service.ensure_record_submissions(state)
            existing = submissions.get(user_id)
            if existing and existing["result"] != "abort":
                raise ValueError("You already submitted a different result.")
            submissions[user_id] = {"result": "abort", "is_admin": is_admin}
            self.state_service.persist_state(guild_id, state)
            pmid = state.get("pending_match_id")
            return {
                "non_admin_count": self.get_abort_submission_count(guild_id, pmid),
                "total_count": len(submissions),
                "is_ready": self.can_abort_match(guild_id, pmid),
                "pending_match_id": pmid,
            }

    def get_vote_counts(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int]:
        """
        Get vote counts for radiant and dire (non-admin only).

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            Dict with radiant and dire vote counts
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return {"radiant": 0, "dire": 0}
        submissions = state.get("record_submissions", {})
        counts = {"radiant": 0, "dire": 0}
        for sub in submissions.values():
            if not sub.get("is_admin"):
                result = sub.get("result")
                if result in counts:
                    counts[result] += 1
        return counts

    def get_pending_record_result(self, guild_id: int | None, pending_match_id: int | None = None) -> str | None:
        """
        Get the result to record if voting threshold is met.

        For admin submissions: returns the admin's vote immediately.
        For non-admin: returns the first result to reach MIN_NON_ADMIN_SUBMISSIONS votes.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            "radiant", "dire", or None if threshold not met
        """
        state = self.state_service.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return None
        submissions = state.get("record_submissions", {})

        # If there's an admin submission (radiant/dire), use that result
        for sub in submissions.values():
            result = sub.get("result")
            if sub.get("is_admin") and result in ("radiant", "dire"):
                return result

        # For non-admin: requires MIN_NON_ADMIN_SUBMISSIONS matching submissions
        vote_counts = self.get_vote_counts(guild_id, pending_match_id)
        if vote_counts["radiant"] >= self.MIN_NON_ADMIN_SUBMISSIONS:
            return "radiant"
        if vote_counts["dire"] >= self.MIN_NON_ADMIN_SUBMISSIONS:
            return "dire"
        return None

    def can_record_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """
        Check if there are enough votes to record the match.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support

        Returns:
            True if voting threshold is met
        """
        if self.has_admin_submission(guild_id, pending_match_id):
            return True
        # Requires MIN_NON_ADMIN_SUBMISSIONS matching submissions
        return self.get_pending_record_result(guild_id, pending_match_id) is not None
