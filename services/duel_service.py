"""Application facade for duel challenge workflows."""

import time
from collections.abc import Callable

from domain.models.duel import (
    DuelDueKind,
    DuelDueResult,
    DuelResolution,
    DuelTrial,
)

MIN_WAGER = 500
MAX_WAGER = 1000
RESPONSE_SECONDS = 7 * 86400
CHALLENGER_COOLDOWN_SECONDS = 30 * 86400
RECIPIENT_COOLDOWN_SECONDS = 7 * 86400


class DuelService:
    """Route duel use cases through the atomic challenge repository."""

    def __init__(self, repo, clock: Callable[[], float] = time.time):
        self.repo = repo
        self._clock = clock

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
        return self.repo.resolve_atomic(
            challenge_id,
            guild_id,
            winner_id,
            int(self._clock()),
            actor_id,
        )

    def bind_message(self, challenge_id, guild_id, message_id):
        return self.repo.bind_message(challenge_id, guild_id, message_id)

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

    def process_due(self, challenge_id, guild_id, now):
        expired = self.repo.expire_atomic(challenge_id, guild_id, now)
        if expired is not None:
            return DuelDueResult(DuelDueKind.EXPIRED, expired)
        return self.repo.claim_reminder_atomic(challenge_id, guild_id, now)
