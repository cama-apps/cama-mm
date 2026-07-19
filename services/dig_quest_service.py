"""Service layer for /dig quests — multi-dig narrative arcs.

A quest is a 5-stage chain of events. The player advances by picking the
*desperate* option and succeeding; any other outcome leaves them parked.
One active quest per (player, guild) at a time. Completed quests never
re-fire in the same guild. State persists across prestige resets.

Quests live in ``services.dig_constants.QUESTS``. The registry is
validated at module-import time (rarity monotonicity, desperate-option
presence, event tagging).
"""

from __future__ import annotations

import logging
import random
import time
from collections.abc import Callable

from repositories.dig_quest_repository import DigQuestRepository, QuestState
from services.dig_constants import QUESTS, QuestDef
from services.dig_data.balance import (
    DIG_POSITIVE_JC_MULTIPLIER,
    scale_positive_dig_jc,
)

logger = logging.getLogger("cama_bot.services.dig_quest")


# Window for the Bolas system-tied prereq ("any recent bet in this guild").
_BOLAS_RECENT_BET_WINDOW_SECONDS = 7 * 24 * 60 * 60


class DigQuestService:
    """Tracks quest progression and resolves finale rewards."""

    def __init__(
        self,
        quest_repo: DigQuestRepository,
        dig_repo,
        bet_repo,
        guild_modifier_repo,
        *,
        quests: tuple[QuestDef, ...] | None = None,
        tax_fn: Callable[[int, int | None, int], int] | None = None,
    ):
        self.quest_repo = quest_repo
        self.dig_repo = dig_repo
        self.bet_repo = bet_repo
        self.guild_modifier_repo = guild_modifier_repo
        # Tax application for finale JC — Plains tithe + Blue tax. Back-filled
        # by the service container post-DigService construction (the function
        # lives on DigService and has the Plains tithe → nonprofit-fund side
        # effect). When None, JC is credited gross.
        self._tax_fn = tax_fn
        # Allow tests to inject a fake quest set; production uses the
        # validated registry from dig_constants.
        self._quests: tuple[QuestDef, ...] = tuple(quests) if quests is not None else tuple(QUESTS)
        self._by_quest_id: dict[str, QuestDef] = {q.quest_id: q for q in self._quests}
        # Reverse index: event_id -> (quest_id, step). Populated at construction
        # so the lookup in resolve_event is O(1).
        self._event_to_quest: dict[str, tuple[str, int]] = {}
        for q in self._quests:
            for step_idx, eid in enumerate(q.step_event_ids, start=1):
                self._event_to_quest[eid] = (q.quest_id, step_idx)

    def set_tax_fn(
        self, tax_fn: Callable[[int, int | None, int], int] | None,
    ) -> None:
        """Back-fill the finale-JC tax function after construction.

        DigService and DigQuestService cross-reference each other in the
        service container, so the tax callable (which lives on DigService)
        can only be wired up after both are built.
        """
        self._tax_fn = tax_fn

    # ── State accessors ────────────────────────────────────────────────────

    def get_state(self, discord_id: int, guild_id: int | None) -> QuestState:
        return self.quest_repo.get_state(discord_id, guild_id)

    def quest_for_event(self, event_id: str) -> tuple[str, int] | None:
        """If ``event_id`` is tagged as a quest stage, return (quest_id, step).

        Returns None for non-quest events.
        """
        return self._event_to_quest.get(event_id)

    # ── Eligibility ────────────────────────────────────────────────────────

    def is_starter_prereq_met(
        self,
        quest: QuestDef,
        tunnel: dict,
        discord_id: int,
        guild_id: int | None,
    ) -> bool:
        """Check whether the player satisfies the starter prerequisites."""
        depth = int(tunnel.get("depth", 0) or 0)
        prestige = int(tunnel.get("prestige_level", 0) or 0)
        prereq = quest.starter_prereq
        if depth < prereq.min_depth:
            return False
        if prestige < prereq.min_prestige:
            return False
        return not (
            prereq.system_predicate is not None
            and not self._resolve_system_predicate(
                prereq.system_predicate, discord_id, guild_id,
            )
        )

    def _resolve_system_predicate(
        self, predicate_id: str, discord_id: int, guild_id: int | None,
    ) -> bool:
        """Dispatch table for cross-system quest prerequisites."""
        if predicate_id == "bet_within_7d":
            since = int(time.time()) - _BOLAS_RECENT_BET_WINDOW_SECONDS
            return bool(self.bet_repo.has_recent_bet(discord_id, guild_id, since))
        logger.warning("unknown quest system_predicate %r — denying eligibility", predicate_id)
        return False

    def eligible_quest_event_ids(
        self,
        discord_id: int,
        guild_id: int | None,
        tunnel: dict | None = None,
    ) -> set[str]:
        """Return the set of quest event ids the player may currently roll.

        - If the player has an active quest, only its current-stage event is
          eligible.
        - Otherwise, every starter (stage 1) event of a quest the player is
          eligible for and hasn't completed is eligible.
        - Non-quest events are not in this set; callers always allow them.
        """
        if not self._quests:
            return set()
        if tunnel is None:
            tunnel = self.dig_repo.get_tunnel(discord_id, guild_id) or {}
        state = self.get_state(discord_id, guild_id)

        # Active quest path: only the matching stage event is in play.
        if state.active_quest_id and state.active_quest_step:
            quest = self._by_quest_id.get(state.active_quest_id)
            if quest is None:
                # Active quest references a quest no longer in the registry;
                # leave the player stuck rather than silently choosing for
                # them. Admin can abandon.
                return set()
            idx = state.active_quest_step - 1
            if 0 <= idx < len(quest.step_event_ids):
                return {quest.step_event_ids[idx]}
            return set()

        # No active quest: surface every eligible starter (stage 1) the
        # player hasn't already completed.
        eligible: set[str] = set()
        for quest in self._quests:
            if state.has_completed(quest.quest_id):
                continue
            if not self.is_starter_prereq_met(quest, tunnel, discord_id, guild_id):
                continue
            if quest.step_event_ids:
                eligible.add(quest.step_event_ids[0])
        return eligible

    # ── Progression ────────────────────────────────────────────────────────

    def advance_on_desperate_success(
        self,
        discord_id: int,
        guild_id: int | None,
        event_id: str,
    ) -> dict | None:
        """Advance the player's quest after a successful desperate choice.

        Returns a finale-result dict when the final stage just completed
        (None for intermediate advances and no-op cases).

        The caller is responsible for ensuring the event resolved
        successfully via the desperate option. We re-validate locally that
        the event is actually a quest stage and matches the player's
        current state — defense in depth.
        """
        mapping = self._event_to_quest.get(event_id)
        if mapping is None:
            return None  # non-quest event
        quest_id, event_step = mapping
        quest = self._by_quest_id.get(quest_id)
        if quest is None:
            return None

        state = self.get_state(discord_id, guild_id)

        # Stage 1 case: player has no active quest, must be eligible to start.
        # The validator enforces ``step_event_ids`` length == 5, so stage 1 is
        # never the final stage; we always move to stage 2.
        if state.active_quest_id is None:
            if state.has_completed(quest_id):
                return None
            if event_step != 1:
                return None
            tunnel = self.dig_repo.get_tunnel(discord_id, guild_id) or {}
            if not self.is_starter_prereq_met(quest, tunnel, discord_id, guild_id):
                return None
            self.quest_repo.set_active(discord_id, guild_id, quest_id, 2)
            return None

        # Active-quest path: ensure the resolved event matches the current
        # stage. If not, ignore (player rolled stale state or admin-injected).
        if state.active_quest_id != quest_id:
            return None
        if state.active_quest_step != event_step:
            return None

        if event_step >= len(quest.step_event_ids):
            return self._complete_and_dispatch_finale(quest, discord_id, guild_id)
        self.quest_repo.set_active(discord_id, guild_id, quest_id, event_step + 1)
        return None

    def _complete_and_dispatch_finale(
        self, quest: QuestDef, discord_id: int, guild_id: int | None,
    ) -> dict:
        """Mark quest completed, then run its finale handler. Returns a
        descriptor for the embed.

        Order matters: ``complete_quest`` runs first so a partial failure in
        the finale dispatch can't leave the player stuck on stage 5 with the
        reward already granted (i.e. eligible to fire the finale again). If
        the dispatch raises, the quest is recorded complete-with-no-reward
        and the exception propagates so the caller can log and an admin can
        grant manually — strictly preferable to a silent double-grant.
        """
        self.quest_repo.complete_quest(discord_id, guild_id, quest.quest_id)
        finale_result = self._dispatch_finale(quest, discord_id, guild_id)
        return {
            "quest_id": quest.quest_id,
            "quest_name": quest.name,
            "finale_kind": quest.finale_kind,
            **finale_result,
        }

    # ── Finale dispatch ────────────────────────────────────────────────────

    def _dispatch_finale(
        self, quest: QuestDef, discord_id: int, guild_id: int | None,
    ) -> dict:
        """Apply the quest's terminal reward and return a result dict."""
        kind = quest.finale_kind
        if kind == "jc_plus_guild_modifier":
            return self._finale_jc_plus_modifier(quest, discord_id, guild_id)
        if kind == "relic_grant":
            return self._finale_relic_grant(quest, discord_id, guild_id)
        raise ValueError(f"unhandled finale_kind {kind!r} for quest {quest.quest_id!r}")

    def _finale_jc_plus_modifier(
        self, quest: QuestDef, discord_id: int, guild_id: int | None,
    ) -> dict:
        """Aghanim-style finale: personal JC slug + guild-wide modifier window.

        Payload schema:
            {
                "personal_jc": int,
                "modifier_id": str,
                "duration_seconds": int,
                "modifier_payload": dict,
            }

        The gross JC first uses the dig reward policy, then runs through
        ``self._tax_fn`` (Plains tithe + Blue tax) so mana-region effects
        apply consistently with the rest of the dig flow. The returned
        ``personal_jc`` is the net amount actually credited.
        """
        payload = quest.finale_payload
        gross_jc = int(payload.get("personal_jc", 0))
        modifier_id = str(payload.get("modifier_id", ""))
        duration = int(payload.get("duration_seconds", 0))
        modifier_payload = dict(payload.get("modifier_payload") or {})

        scaled_jc = scale_positive_dig_jc(gross_jc)
        net_jc = scaled_jc
        if self._tax_fn is not None and scaled_jc > 0:
            net_jc = self._tax_fn(discord_id, guild_id, scaled_jc)

        if net_jc != 0:
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id=discord_id,
                guild_id=guild_id,
                balance_delta=net_jc,
                log_action_type="quest_finale_jc",
                log_detail={
                    "quest_id": quest.quest_id,
                    "gross_jc": gross_jc,
                    "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
                    "net_jc": net_jc,
                },
            )

        expires_at = None
        if modifier_id and duration > 0 and self.guild_modifier_repo is not None:
            expires_at = self.guild_modifier_repo.set_modifier(
                guild_id=guild_id,
                modifier_id=modifier_id,
                duration_seconds=duration,
                payload=modifier_payload,
            )

        return {
            "personal_jc": net_jc,
            "personal_jc_gross": gross_jc,
            "modifier_id": modifier_id,
            "modifier_duration": duration,
            "modifier_payload": modifier_payload,
            "modifier_expires_at": expires_at,
        }

    def _finale_relic_grant(
        self, quest: QuestDef, discord_id: int, guild_id: int | None,
    ) -> dict:
        """Necropolis/Bolas-style finale: grant a relic with N rolls drawn
        from a curated sub-pool of pinnacle stats.

        Payload schema:
            {
                "relic_base": str,           # e.g. "Cloak of the Necropolis"
                "relic_suffix": str,         # static suffix; the quest-themed
                                             # name is "<relic_base> of <suffix>"
                "stat_pool": tuple[str, ...],# subset of pinnacle stat ids
                "roll_count": int,           # default 2
            }

        Encodes the artifact_id as ``pinnacle:<base>:<suffix>:<stat1>:<stat2>``
        so it composes with the existing pinnacle-relic stat decoder
        without any new decoding code.
        """
        payload = quest.finale_payload
        base = str(payload.get("relic_base", "Relic"))
        suffix = str(payload.get("relic_suffix", "the Quest"))
        roll_count = int(payload.get("roll_count", 2))
        stat_pool = list(payload.get("stat_pool") or ())

        if len(stat_pool) < roll_count:
            raise ValueError(
                f"quest {quest.quest_id!r} stat_pool has {len(stat_pool)} stats; "
                f"need at least {roll_count}"
            )
        chosen = random.sample(stat_pool, k=roll_count)
        # Encode in the same shape as pinnacle relics so the existing
        # stat decoder applies. Joiner is ':'.
        artifact_id = "pinnacle:" + base + ":" + suffix + ":" + ":".join(chosen)
        relic_db_id = self.dig_repo.add_artifact(
            discord_id, guild_id, artifact_id, is_relic=True,
        )

        return {
            "relic_name": f"{base} of {suffix}",
            "relic_stat_ids": list(chosen),
            "artifact_id": artifact_id,
            "db_id": relic_db_id,
        }
