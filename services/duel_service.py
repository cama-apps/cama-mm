"""Application facade for duel challenge workflows."""

import time
from collections.abc import Callable

from domain.models.duel import (
    DuelDueKind,
    DuelDueResult,
    DuelResolution,
    DuelStatus,
    DuelTrial,
)
from domain.pet_evolution import PetActivity

RESPONSE_SECONDS = 7 * 86400
CHALLENGER_COOLDOWN_SECONDS = 30 * 86400
RECIPIENT_COOLDOWN_SECONDS = 7 * 86400
REMINDER_RETRY_BACKOFF_SECONDS = 600


class DuelService:
    """Route duel use cases through the atomic challenge repository."""

    def __init__(
        self,
        repo,
        clock: Callable[[], float] = time.time,
        pet_evolution_service=None,
    ):
        self.repo = repo
        self._clock = clock
        self.pet_evolution_service = pet_evolution_service

    def issue(
        self,
        guild_id,
        channel_id,
        challenger_id,
        recipient_id,
        wager,
        *,
        recipient_is_bot=False,
    ):
        if recipient_is_bot:
            raise ValueError("Bots cannot answer a challenge of honor.")
        now = int(self._clock())
        return self.repo.create_challenge_atomic(
            guild_id,
            channel_id,
            challenger_id,
            recipient_id,
            wager,
            now,
            CHALLENGER_COOLDOWN_SECONDS,
            RECIPIENT_COOLDOWN_SECONDS,
            RESPONSE_SECONDS,
            challenger_id,
        )

    def respond(self, guild_id, recipient_id, choice):
        challenge = self.repo.get_pending_for_recipient(recipient_id, guild_id)
        if challenge is None:
            raise ValueError("You have no pending duel challenge.")
        now = int(self._clock())
        if choice == "decline":
            return self.repo.decline_atomic(
                challenge.challenge_id,
                guild_id,
                recipient_id,
                now,
                recipient_id,
            )
        trial = DuelTrial(choice)
        return self.repo.accept_atomic(
            challenge.challenge_id,
            guild_id,
            recipient_id,
            trial,
            now,
            recipient_id,
        )

    def resolve(self, guild_id, actor_id, challenge_id, outcome):
        challenge = self.repo.get_challenge(challenge_id, guild_id)
        if challenge is None:
            raise ValueError("Challenge not found.")
        resolution = DuelResolution(outcome)
        winner_id = {
            DuelResolution.CHALLENGER_VICTORY: challenge.challenger_id,
            DuelResolution.RECIPIENT_VICTORY: challenge.recipient_id,
            DuelResolution.VOID: None,
        }[resolution]
        now = int(self._clock())
        resolved = self.repo.resolve_atomic(
            challenge_id,
            guild_id,
            winner_id,
            now,
            actor_id,
        )
        if (
            resolution is not DuelResolution.VOID
            and self.pet_evolution_service is not None
        ):
            for participant_id in (
                challenge.challenger_id,
                challenge.recipient_id,
            ):
                self.pet_evolution_service.record_activity(
                    participant_id,
                    guild_id,
                    PetActivity.DUEL_COMPLETED,
                    f"duel:{challenge.challenge_id}",
                    occurred_at=now,
                )
        return resolved

    def bind_message(self, challenge_id, guild_id, message_id):
        return self.repo.bind_message(challenge_id, guild_id, message_id)

    def get_challenge(self, challenge_id, guild_id):
        return self.repo.get_challenge(challenge_id, guild_id)

    def mark_delivery_failed(self, challenge_id, guild_id, actor_id):
        return self.repo.mark_delivery_failed_atomic(
            challenge_id,
            guild_id,
            int(self._clock()),
            actor_id,
        )

    def list_outstanding(self, guild_id):
        return self.repo.list_outstanding(guild_id)

    def list_pending_all(self):
        return self.repo.list_pending_all()

    def get_due_challenge_ids(self, now):
        return self.repo.get_due_challenge_ids(now)

    def process_due(self, challenge_id, guild_id, now, *, claim_reminders=True):
        if claim_reminders:
            reminder = self.repo.claim_reminder_atomic(challenge_id, guild_id, now)
            if reminder is not None:
                return reminder
            unresolved = self.repo.claim_unresolved_reminder_atomic(
                challenge_id, guild_id, now
            )
            if unresolved is not None:
                return unresolved
        # Claim expired announcements even when reminder claims are deferred:
        # the expiry is already settled, and advancing the marker engages the
        # retry backoff so a challenge with no sendable channel cannot re-run
        # channel resolution on every worker wake.
        announcement = self.repo.claim_expired_announcement_atomic(
            challenge_id,
            guild_id,
            now,
            now + REMINDER_RETRY_BACKOFF_SECONDS,
        )
        if announcement is not None:
            return announcement
        try:
            expired = self.repo.expire_atomic(challenge_id, guild_id, now)
        except ValueError:
            return None
        return DuelDueResult(DuelDueKind.EXPIRED, expired)

    def confirm_expiry_announced(self, result: DuelDueResult) -> bool:
        """Retire an expired duel's pending-announcement marker after a send."""
        if (
            result.kind is not DuelDueKind.EXPIRED
            or result.challenge.next_reminder_at is None
        ):
            return False
        return self.repo.clear_expired_announcement_atomic(
            result.challenge.challenge_id,
            result.challenge.guild_id,
            result.challenge.next_reminder_at,
        )

    def rearm_reminder(self, result: DuelDueResult) -> bool:
        expected_status = {
            DuelDueKind.REMINDER: DuelStatus.PENDING,
            DuelDueKind.UNRESOLVED: DuelStatus.ACCEPTED,
        }.get(result.kind)
        if (
            expected_status is None
            or result.claimed_reminder_at is None
            or result.challenge.next_reminder_at is None
        ):
            return False
        # Retry after a backoff rather than at the original (past) claim
        # time, so a failing delivery target cannot hot-loop a claim and
        # flavor-generation cycle on every worker wake. Never retry later
        # than the regularly scheduled next reminder.
        retry_at = min(
            result.challenge.next_reminder_at,
            int(self._clock()) + REMINDER_RETRY_BACKOFF_SECONDS,
        )
        return self.repo.rearm_reminder_atomic(
            result.challenge.challenge_id,
            result.challenge.guild_id,
            expected_status,
            result.challenge.next_reminder_at,
            retry_at,
        )
