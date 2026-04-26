"""
Repository for managing nonprofit fund disbursement proposals and votes.
"""

from __future__ import annotations

import json
import time

from repositories.base_repository import BaseRepository, safe_json_loads
from repositories.interfaces import IDisburseRepository


class DisburseRepository(BaseRepository, IDisburseRepository):
    """
    Handles CRUD operations for disburse_proposals and disburse_votes tables.
    """

    def get_active_proposal(self, guild_id: int | None) -> dict | None:
        """
        Get the active proposal for a guild, if any.

        Returns:
            dict with proposal data or None if no active proposal
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT guild_id, proposal_id, message_id, channel_id,
                       fund_amount, quorum_required, status, created_at
                FROM disburse_proposals
                WHERE guild_id = ? AND status = 'active'
                """,
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "guild_id": row["guild_id"],
                "proposal_id": row["proposal_id"],
                "message_id": row["message_id"],
                "channel_id": row["channel_id"],
                "fund_amount": row["fund_amount"],
                "quorum_required": row["quorum_required"],
                "status": row["status"],
                "created_at": row["created_at"],
            }

    def create_proposal(
        self,
        guild_id: int | None,
        proposal_id: int,
        fund_amount: int,
        quorum_required: int,
    ) -> None:
        """
        Create a new disbursement proposal.

        Args:
            guild_id: Guild ID
            proposal_id: Unix timestamp of proposal creation
            fund_amount: Snapshot of fund at proposal time
            quorum_required: Number of votes needed (40% of players)
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Replace any existing proposal for this guild
            cursor.execute(
                """
                INSERT OR REPLACE INTO disburse_proposals
                    (guild_id, proposal_id, fund_amount, quorum_required, status)
                VALUES (?, ?, ?, ?, 'active')
                """,
                (normalized_guild, proposal_id, fund_amount, quorum_required),
            )
            # Clear any votes from previous proposals
            cursor.execute(
                "DELETE FROM disburse_votes WHERE guild_id = ? AND proposal_id != ?",
                (normalized_guild, proposal_id),
            )

    def set_proposal_message(
        self, guild_id: int | None, message_id: int, channel_id: int
    ) -> None:
        """
        Set the Discord message ID for an active proposal.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE disburse_proposals
                SET message_id = ?, channel_id = ?
                WHERE guild_id = ? AND status = 'active'
                """,
                (message_id, channel_id, normalized_guild),
            )

    def add_vote(
        self,
        guild_id: int | None,
        proposal_id: int,
        discord_id: int,
        method: str,
    ) -> None:
        """
        Add or update a vote for a disbursement proposal.

        Args:
            guild_id: Guild ID
            proposal_id: Proposal ID to vote on
            discord_id: Voter's Discord ID
            method: 'even', 'proportional', 'neediest', 'stimulus', 'lottery', 'social_security', 'richest', or 'cancel'
        """
        if method not in ("even", "proportional", "neediest", "stimulus", "lottery", "social_security", "richest", "cancel"):
            raise ValueError(f"Invalid vote method: {method}")

        normalized_guild = self.normalize_guild_id(guild_id)
        voted_at = int(time.time())

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT OR REPLACE INTO disburse_votes
                    (guild_id, proposal_id, discord_id, vote_method, voted_at)
                VALUES (?, ?, ?, ?, ?)
                """,
                (normalized_guild, proposal_id, discord_id, method, voted_at),
            )

    def get_vote_counts(self, guild_id: int | None) -> dict[str, int]:
        """
        Get vote counts for each method for the active proposal.

        Returns:
            dict with counts for all methods
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get the active proposal ID
            cursor.execute(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id = ? AND status = 'active'",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return {
                    "even": 0, "proportional": 0, "neediest": 0, "stimulus": 0,
                    "lottery": 0, "social_security": 0, "richest": 0, "cancel": 0
                }

            proposal_id = row["proposal_id"]

            cursor.execute(
                """
                SELECT vote_method, COUNT(*) as count
                FROM disburse_votes
                WHERE guild_id = ? AND proposal_id = ?
                GROUP BY vote_method
                """,
                (normalized_guild, proposal_id),
            )

            counts = {
                "even": 0, "proportional": 0, "neediest": 0, "stimulus": 0,
                "lottery": 0, "social_security": 0, "richest": 0, "cancel": 0
            }
            for row in cursor.fetchall():
                counts[row["vote_method"]] = row["count"]
            return counts

    def get_total_votes(self, guild_id: int | None) -> int:
        """
        Get total number of votes for the active proposal.
        """
        counts = self.get_vote_counts(guild_id)
        return sum(counts.values())

    def get_voter_ids(self, guild_id: int | None) -> list[int]:
        """
        Get list of discord_ids who have voted on the active proposal.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id = ? AND status = 'active'",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return []

            proposal_id = row["proposal_id"]
            cursor.execute(
                "SELECT discord_id FROM disburse_votes WHERE guild_id = ? AND proposal_id = ?",
                (normalized_guild, proposal_id),
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    def get_individual_votes(self, guild_id: int | None) -> list[dict]:
        """
        Get individual vote details for the active proposal.

        Returns:
            List of dicts with vote details:
            [
                {
                    "discord_id": int,
                    "vote_method": str,  # "even", "proportional", "neediest", "stimulus"
                    "voted_at": int,     # Unix timestamp
                },
                ...
            ]
            Returns empty list if no active proposal.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get the active proposal ID first
            cursor.execute(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id = ? AND status = 'active'",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return []

            proposal_id = row["proposal_id"]

            # Get all individual votes, ordered by voted_at
            cursor.execute(
                """
                SELECT discord_id, vote_method, voted_at
                FROM disburse_votes
                WHERE guild_id = ? AND proposal_id = ?
                ORDER BY voted_at ASC
                """,
                (normalized_guild, proposal_id),
            )
            return [
                {
                    "discord_id": row["discord_id"],
                    "vote_method": row["vote_method"],
                    "voted_at": row["voted_at"],
                }
                for row in cursor.fetchall()
            ]

    def complete_proposal(self, guild_id: int | None) -> None:
        """
        Mark the active proposal as completed.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE disburse_proposals
                SET status = 'completed'
                WHERE guild_id = ? AND status = 'active'
                """,
                (normalized_guild,),
            )

    def complete_and_disburse_atomic(
        self,
        guild_id: int | None,
        fund_amount_to_return: int,
        distributions: list[tuple[int, int]],
        method: str,
    ) -> int:
        """
        Atomically finalize a disbursement: mark proposal completed, return the
        reserved fund to the nonprofit pool, deduct the distribution total, credit
        players, and insert the history row — all inside one ``BEGIN IMMEDIATE``.

        Previously the disburse service called these four writes as separate
        repo operations; a process crash between any pair would leave the proposal
        flagged completed while the fund was silently over-restored or no history
        was recorded. Folding them into a single transaction closes that window.

        Args:
            guild_id: Guild ID (``None`` normalized to 0).
            fund_amount_to_return: The reserve to credit back into the nonprofit
                fund (typically equal to the proposal's ``fund_amount``).
            distributions: List of ``(discord_id, amount)`` tuples to credit.
            method: Disbursement method label for the history row.

        Returns:
            Total amount actually distributed to players (sum of distribution
            amounts; 0 if ``distributions`` is empty).

        Raises:
            ValueError: If the resulting nonprofit fund cannot cover the
                distribution total (should not happen when ``fund_amount_to_return``
                equals the proposal's snapshot, but guarded defensively).
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        total = sum(amount for _, amount in distributions)
        now = int(time.time())
        recipients_json = json.dumps(distributions)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # 1) Mark the proposal completed so a concurrent/rerun caller can't
            #    re-execute it — even cross-process.
            cursor.execute(
                """
                UPDATE disburse_proposals
                SET status = 'completed'
                WHERE guild_id = ? AND status = 'active'
                """,
                (normalized_guild,),
            )

            # 2) Return the reserve to the nonprofit fund.
            if fund_amount_to_return:
                cursor.execute(
                    """
                    INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                    VALUES (?, ?, CURRENT_TIMESTAMP)
                    ON CONFLICT(guild_id) DO UPDATE SET
                        total_collected = total_collected + excluded.total_collected,
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    (normalized_guild, fund_amount_to_return),
                )

            # 3) Deduct the distribution total and credit recipients. We verify
            #    sufficiency against the post-credit pool so a stale snapshot
            #    can't over-distribute.
            if total > 0:
                cursor.execute(
                    "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                    (normalized_guild,),
                )
                row = cursor.fetchone()
                available = row["total_collected"] if row else 0
                if available < total:
                    raise ValueError(
                        f"Insufficient funds in nonprofit. Available: {available}, needed: {total}"
                    )
                cursor.execute(
                    """
                    UPDATE nonprofit_fund
                    SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE guild_id = ?
                    """,
                    (total, normalized_guild),
                )
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [
                        (amount, discord_id, normalized_guild)
                        for discord_id, amount in distributions
                    ],
                )

            # 4) Record history in the same txn — no silent "completed but no log"
            #    state is possible if this fails, because it rolls the whole op back.
            if distributions:
                cursor.execute(
                    """
                    INSERT INTO disburse_history
                        (guild_id, disbursed_at, total_amount, method, recipient_count, recipients)
                    VALUES (?, ?, ?, ?, ?, ?)
                    """,
                    (
                        normalized_guild,
                        now,
                        total,
                        method,
                        len(distributions),
                        recipients_json,
                    ),
                )

            return total

    def reset_proposal(self, guild_id: int | None) -> bool:
        """
        Reset (cancel) the active proposal.

        Returns:
            True if a proposal was reset, False if no active proposal
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get proposal_id first
            cursor.execute(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id = ? AND status = 'active'",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return False

            proposal_id = row["proposal_id"]

            # Mark as reset
            cursor.execute(
                """
                UPDATE disburse_proposals
                SET status = 'reset'
                WHERE guild_id = ? AND status = 'active'
                """,
                (normalized_guild,),
            )

            # Delete votes for this proposal
            cursor.execute(
                "DELETE FROM disburse_votes WHERE guild_id = ? AND proposal_id = ?",
                (normalized_guild, proposal_id),
            )

            return True

    def reset_and_return_fund_atomic(
        self, guild_id: int | None, fund_amount: int
    ) -> bool:
        """
        Atomically reset the active proposal and credit ``fund_amount`` back to
        the nonprofit fund — all in one ``BEGIN IMMEDIATE``. Mirrors
        ``complete_and_disburse_atomic`` for the cancel/reset path so a crash
        between status flip and fund credit cannot strand the reserve.

        Returns True if a proposal was reset, False if none was active.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT proposal_id FROM disburse_proposals WHERE guild_id = ? AND status = 'active'",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return False

            proposal_id = row["proposal_id"]

            cursor.execute(
                """
                UPDATE disburse_proposals
                SET status = 'reset'
                WHERE guild_id = ? AND status = 'active'
                """,
                (normalized_guild,),
            )

            cursor.execute(
                "DELETE FROM disburse_votes WHERE guild_id = ? AND proposal_id = ?",
                (normalized_guild, proposal_id),
            )

            if fund_amount:
                cursor.execute(
                    """
                    INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                    VALUES (?, ?, CURRENT_TIMESTAMP)
                    ON CONFLICT(guild_id) DO UPDATE SET
                        total_collected = total_collected + excluded.total_collected,
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    (normalized_guild, fund_amount),
                )

            return True

    def record_disbursement(
        self,
        guild_id: int | None,
        total_amount: int,
        method: str,
        distributions: list[tuple[int, int]],
    ) -> int:
        """
        Record a completed disbursement for history.

        Args:
            guild_id: Guild ID
            total_amount: Total amount disbursed
            method: 'even', 'proportional', or 'neediest'
            distributions: List of (discord_id, amount) tuples

        Returns:
            The history record ID
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())
        recipients_json = json.dumps(distributions)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO disburse_history
                    (guild_id, disbursed_at, total_amount, method, recipient_count, recipients)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    normalized_guild,
                    now,
                    total_amount,
                    method,
                    len(distributions),
                    recipients_json,
                ),
            )
            return cursor.lastrowid

    def get_last_disbursement(self, guild_id: int | None) -> dict | None:
        """
        Get the most recent disbursement for a guild.

        Returns:
            dict with disbursement info or None if no history
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, guild_id, disbursed_at, total_amount, method,
                       recipient_count, recipients
                FROM disburse_history
                WHERE guild_id = ?
                ORDER BY disbursed_at DESC
                LIMIT 1
                """,
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                return None

            return {
                "id": row["id"],
                "guild_id": row["guild_id"],
                "disbursed_at": row["disbursed_at"],
                "total_amount": row["total_amount"],
                "method": row["method"],
                "recipient_count": row["recipient_count"],
                "recipients": safe_json_loads(
                    row["recipients"],
                    default=[],
                    context=f"disbursements.recipients id={row['id']}",
                ),
            }

    def get_recipient_history(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """
        Return every disbursement in which ``discord_id`` was a recipient, newest first.

        Each entry: ``{disbursed_at, amount, method}`` where ``amount`` is the
        portion credited to this user in that disbursement.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, disbursed_at, method, recipients
                FROM disburse_history
                WHERE guild_id = ?
                ORDER BY disbursed_at DESC
                """,
                (normalized_guild,),
            )

            results: list[dict] = []
            for row in cursor.fetchall():
                distributions = safe_json_loads(
                    row["recipients"],
                    default=[],
                    context=f"disbursements.recipients id={row['id']}",
                )
                for entry in distributions:
                    # Entries are stored as [discord_id, amount] tuples (JSON arrays).
                    if not entry or len(entry) < 2:
                        continue
                    recipient_id, amount = entry[0], entry[1]
                    if recipient_id == discord_id and amount:
                        results.append(
                            {
                                "disbursed_at": row["disbursed_at"],
                                "amount": amount,
                                "method": row["method"],
                            }
                        )
                        break
            return results
