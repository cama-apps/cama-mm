"""Quest arc definitions and quest validation for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

from services.dig_data.event_definitions import RANDOM_EVENTS

# ---------------------------------------------------------------------------
# Quests
#
# Multi-dig narrative arcs of exactly 5 stages each. Each stage is a normal
# RandomEvent tagged with ``quest_id`` and ``quest_step`` (1-indexed). The
# player advances to the next stage by picking the *desperate* option and
# succeeding; any other outcome leaves them parked. Stage rarity must be
# monotonic non-increasing across stages. Completion (per-guild, lifetime
# one-shot) triggers a finale handler in DigQuestService.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class QuestStarterPrereq:
    """Eligibility criteria for a quest's stage-1 event.

    ``min_depth`` and ``min_prestige`` are checked against the player's
    tunnel state. ``system_predicate`` is an optional key resolved by
    DigQuestService against other services (e.g. ``"bet_within_7d"``
    looks up recent betting activity). Multiple criteria are AND-ed.
    """
    min_depth: int = 0
    min_prestige: int = 0
    system_predicate: str | None = None


@dataclass(frozen=True)
class QuestDef:
    """Immutable definition of a 5-stage quest arc."""
    quest_id: str
    name: str                              # internal label (logs/admin); not shown to players
    starter_prereq: QuestStarterPrereq
    step_event_ids: tuple[str, ...]        # ordered; must be length 5
    finale_kind: str                       # "jc_plus_guild_modifier" | "relic_grant"
    finale_payload: dict                   # kind-specific finale config


# Curated, weaker sub-pool of PINNACLE_RELIC_STAT_POOL ids for quest relic
# finales. Tuned modest so quest relics feel rewarding without eclipsing
# pinnacle drops (which roll across the full pool).
_NECROPOLIS_RELIC_STAT_POOL: tuple[str, ...] = (
    "boss_hp_minus_10",   # bosses arrive weakened
    "dmg_plus_per_100",   # damage scales with depth
    "boss_hit_minus",     # bosses miss more often
    "hp_plus_1",          # tougher skin (survive longer)
    "streak_immunity",    # streak persists once per delve
)

_BOLAS_RELIC_STAT_POOL: tuple[str, ...] = (
    "jc_plus_5",          # richer veins
    "boss_payout_5",      # bosses pay better
    "inventory_plus_1",   # roomier pack
    "cheer_buff",         # cheers ring louder
    "lum_refill_2",       # brighter mornings (more digs per delve)
)


QUESTS: tuple[QuestDef, ...] = (
    QuestDef(
        quest_id="agh_lost_trial",
        name="Aghanim's Lost Trial",
        starter_prereq=QuestStarterPrereq(min_depth=25),
        step_event_ids=("agh_s1", "agh_s2", "agh_s3", "agh_s4", "agh_s5"),
        finale_kind="jc_plus_guild_modifier",
        finale_payload={
            "personal_jc": 75,
            "modifier_id": "reagent_spill",
            "duration_seconds": 1800,
            "modifier_payload": {"jc_event_bonus_pct": 25},
        },
    ),
    QuestDef(
        quest_id="necropolis_below",
        name="The Necropolis Below",
        starter_prereq=QuestStarterPrereq(min_prestige=2),
        step_event_ids=("necro_s1", "necro_s2", "necro_s3", "necro_s4", "necro_s5"),
        finale_kind="relic_grant",
        finale_payload={
            "relic_base": "Cloak of the Necropolis",
            "relic_suffix": "Long Silence",
            "stat_pool": _NECROPOLIS_RELIC_STAT_POOL,
            "roll_count": 2,
        },
    ),
    QuestDef(
        quest_id="bolas_hidden_vault",
        name="Bolas' Hidden Vault",
        starter_prereq=QuestStarterPrereq(system_predicate="bet_within_7d"),
        step_event_ids=("bolas_s1", "bolas_s2", "bolas_s3", "bolas_s4", "bolas_s5"),
        finale_kind="relic_grant",
        finale_payload={
            "relic_base": "Hoard of Bolas",
            "relic_suffix": "Scheming Hand",
            "stat_pool": _BOLAS_RELIC_STAT_POOL,
            "roll_count": 2,
        },
    ),
)


_QUEST_RARITY_RANK = {"common": 0, "uncommon": 1, "rare": 2, "legendary": 3}
_QUEST_VALID_FINALE_KINDS = frozenset({"jc_plus_guild_modifier", "relic_grant"})
_QUEST_STAGE_COUNT = 5


def validate_quests(quests, events) -> None:
    """Validate every QuestDef against the event registry.

    Checks per quest:
    - Stage count is exactly 5.
    - Every step_event_id exists in ``events``.
    - Each referenced event is tagged with the matching quest_id/quest_step.
    - Each referenced event has a ``desperate_option`` (the advancement path).
    - Stage rarity is monotonic non-increasing across stages.
    - ``finale_kind`` is recognized.

    Raises ValueError on the first violation. Empty quest tuple is a no-op.
    """
    event_by_id = {e.id: e for e in events}
    for q in quests:
        if len(q.step_event_ids) != _QUEST_STAGE_COUNT:
            raise ValueError(
                f"Quest {q.quest_id!r}: must have exactly "
                f"{_QUEST_STAGE_COUNT} stages, got {len(q.step_event_ids)}"
            )
        prev_rank = -1
        for i, eid in enumerate(q.step_event_ids, start=1):
            event = event_by_id.get(eid)
            if event is None:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: event id {eid!r} "
                    "not found in RANDOM_EVENTS"
                )
            if event.quest_id != q.quest_id:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: event {eid!r}.quest_id "
                    f"is {event.quest_id!r}, expected {q.quest_id!r}"
                )
            if event.quest_step != i:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: event {eid!r}.quest_step "
                    f"is {event.quest_step!r}, expected {i}"
                )
            if event.desperate_option is None:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: event {eid!r} has no "
                    "desperate_option; quest events advance only on "
                    "desperate-success and must offer it"
                )
            rank = _QUEST_RARITY_RANK.get(event.rarity, -1)
            if rank < 0:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: event {eid!r} has "
                    f"unrecognized rarity {event.rarity!r}"
                )
            if rank < prev_rank:
                raise ValueError(
                    f"Quest {q.quest_id!r} stage {i}: rarity {event.rarity!r} "
                    f"is more common than previous stage; rarity must be "
                    "monotonic non-increasing across stages"
                )
            prev_rank = rank
        if q.finale_kind not in _QUEST_VALID_FINALE_KINDS:
            raise ValueError(
                f"Quest {q.quest_id!r}: unknown finale_kind {q.finale_kind!r}"
            )


validate_quests(QUESTS, RANDOM_EVENTS)
