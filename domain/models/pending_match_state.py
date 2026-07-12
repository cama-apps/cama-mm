"""
Typed state for the pending-match payload.

Replaces the prior magic-key dict that lived in ``pending_matches.payload``.
Storage is unchanged (the repo still serializes/deserializes JSON); the
service layer maps the loaded JSON onto this dataclass once and passes
typed instances around so consumers can use attribute access.

Read tolerance: ``from_dict`` ignores unknown keys (older shapes still in
production data shouldn't crash) and tolerates missing keys (defaults
fill in). Write strictness: ``to_dict`` only emits known fields.
"""

from __future__ import annotations

from dataclasses import dataclass, field, fields
from typing import Any


@dataclass
class PendingMatchState:
    """Pending match state — payload stored as JSON in pending_matches.payload.

    The pending_match_id is the row PK and is NOT in the JSON payload; the
    service layer attaches it to the dataclass after loading the row.
    """

    # Team composition
    radiant_team_ids: list[int] = field(default_factory=list)
    dire_team_ids: list[int] = field(default_factory=list)
    excluded_player_ids: list[int] = field(default_factory=list)
    excluded_conditional_player_ids: list[int] = field(default_factory=list)

    # Role assignments + balance display fields
    radiant_roles: list[str] = field(default_factory=list)
    dire_roles: list[str] = field(default_factory=list)
    radiant_value: float = 0.0
    dire_value: float = 0.0
    value_diff: float = 0.0
    first_pick_team: str | None = None

    # Voting state
    record_submissions: dict[int, dict[str, Any]] = field(default_factory=dict)

    # Timestamps + betting window
    shuffle_timestamp: int | None = None
    lock_time: int | None = None
    bet_lock_until: int | None = None

    # Mode flags
    betting_mode: str = "pool"
    is_bomb_pot: bool = False
    is_openskill_shuffle: bool = False
    is_draft: bool = False
    balancing_rating_system: str = "glicko"

    # Reserve-backed betting seed. Pool mode uses radiant/dire seed shares;
    # house mode uses a neutral bonus pool.
    bet_seed_reserved: int = 0
    bet_seed_radiant: int = 0
    bet_seed_dire: int = 0
    bet_seed_bonus: int = 0

    # Auto-blind betting result snapshot (draft display only)
    blind_bets_result: dict | None = None

    # Decrement-on-record bookkeeping for shuffle mode
    effective_avoid_ids: list[int] = field(default_factory=list)
    effective_deal_ids: list[int] = field(default_factory=list)

    # Exclusion-factor changes for new matches are deferred until record.
    # False preserves compatibility with pending matches created before this
    # bookkeeping existed, whose factors were already changed at shuffle time.
    exclusion_updates_deferred: bool = False
    full_exclusion_increment_ids: list[int] = field(default_factory=list)
    half_exclusion_increment_ids: list[int] = field(default_factory=list)

    # Discord message metadata for embed updates / reminders
    shuffle_channel_id: int | None = None
    shuffle_message_id: int | None = None
    shuffle_message_jump_url: str | None = None
    cmd_shuffle_channel_id: int | None = None
    cmd_shuffle_message_id: int | None = None
    thread_shuffle_message_id: int | None = None
    thread_shuffle_thread_id: int | None = None
    origin_channel_id: int | None = None

    # PK from pending_matches table (not stored in JSON; set by service after load).
    pending_match_id: int | None = None

    # Field names that are part of the JSON payload (excludes pending_match_id,
    # which is the row PK, and any debug-only annotations like guild_id/created_at).
    @classmethod
    def _payload_field_names(cls) -> set[str]:
        return {f.name for f in fields(cls) if f.name != "pending_match_id"}

    @classmethod
    def from_dict(cls, payload: dict[str, Any]) -> PendingMatchState:
        """Build a typed state from a payload dict.

        Tolerant on read: unknown keys are ignored, missing keys take the
        dataclass default. The pending_match_id (row PK) is also accepted
        here so callers that already merged it into the dict don't need a
        separate step.
        """
        if payload is None:
            return cls()
        known = {f.name for f in fields(cls)}
        kwargs: dict[str, Any] = {}
        for k, v in payload.items():
            if k in known:
                kwargs[k] = v
        # record_submissions: JSON converts integer keys to strings, normalize back
        subs = kwargs.get("record_submissions")
        if isinstance(subs, dict):
            normalized: dict[int, dict[str, Any]] = {}
            for sk, sv in subs.items():
                try:
                    int_key = int(sk) if isinstance(sk, str) else sk
                except (ValueError, TypeError):
                    int_key = sk
                normalized[int_key] = sv
            kwargs["record_submissions"] = normalized
        return cls(**kwargs)

    def to_dict(self) -> dict[str, Any]:
        """Serialize to a plain dict suitable for JSON storage.

        Strict on write: only known fields. Excludes pending_match_id (row PK).
        """
        result: dict[str, Any] = {}
        for f in fields(self):
            if f.name == "pending_match_id":
                continue
            result[f.name] = getattr(self, f.name)
        return result
