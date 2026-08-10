"""Track Discord nickname eligibility for the vanity tax."""

from __future__ import annotations

from collections.abc import Iterable
from threading import Lock, RLock
from typing import TYPE_CHECKING

from config import VANITY_TAX_RATE

if TYPE_CHECKING:
    from repositories.tax_repository import TaxRepository


class VanityTaxService:
    """Apply profit tax by nickname status and persistent admin enforcement."""

    TAX_RATE = VANITY_TAX_RATE

    def __init__(self, repository: TaxRepository | None = None) -> None:
        self._repository = repository
        # ``_lock`` guards only the in-memory caches and is acquired by sync
        # event handlers on the event loop (on_member_update etc.), so it must
        # never be held across repository I/O. ``_io_lock`` serializes the
        # repository read/write + cache swap so a stale refresh cannot clobber
        # a newer manual-taxation write.
        self._lock = RLock()
        self._io_lock = Lock()
        self._known_members_by_guild: dict[int, frozenset[int]] = {}
        self._nickname_taxable_by_guild: dict[int, frozenset[int]] = {}
        self._manual_taxable_by_guild: dict[int, frozenset[int]] = {}
        # Ids touched by update_member/remove_member since the current
        # refresh's snapshot was taken (``begin_refresh``). ``refresh_guild``
        # runs off-loop from a snapshot taken on the event loop; a member
        # event landing before ``_store_refresh`` would otherwise be clobbered
        # by the (older) wholesale store, so the store re-applies the live
        # state of these ids on top of the snapshot. Entries older than the
        # snapshot must NOT survive into the store — on the on_ready resync
        # path they would re-apply pre-outage state over the fresh snapshot —
        # so snapshot builders call ``begin_refresh`` first.
        self._mutated_members_by_guild: dict[int, set[int]] = {}
        # Monotonic per-guild refresh generation: a store whose snapshot
        # predates the newest begin_refresh must be discarded, or an
        # overlapping resync could consume the journal and then overwrite
        # the newer snapshot's state with its older one.
        self._refresh_generation_by_guild: dict[int, int] = {}

    def begin_refresh(self, guild_id: int) -> int:
        """Mark the start of a refresh snapshot's authority.

        Call on the event loop in the same synchronous block that copies the
        member list, so the journal holds exactly the events that arrive
        after the snapshot. Returns the refresh generation to pass through to
        :meth:`refresh_guild`; a store from a superseded generation is
        dropped.
        """
        with self._lock:
            self._mutated_members_by_guild.pop(guild_id, None)
            generation = self._refresh_generation_by_guild.get(guild_id, 0) + 1
            self._refresh_generation_by_guild[guild_id] = generation
            return generation

    def _store_refresh(
        self,
        guild_id: int,
        known_members: set[int],
        nickname_taxable: set[int],
        manual_taxable: frozenset[int],
        generation: int | None = None,
    ) -> None:
        with self._lock:
            if (
                generation is not None
                and generation != self._refresh_generation_by_guild.get(guild_id)
            ):
                # A newer refresh began after this snapshot was taken: its
                # begin_refresh owns the journal now, and this older snapshot
                # must not overwrite the newer state.
                return
            # Member events that raced this refresh are newer than the
            # snapshot: re-apply their live outcome on top of it. (An event
            # older than the snapshot merges to the same result — snapshot
            # and cache both reflect it — so over-recording is harmless.)
            mutated = self._mutated_members_by_guild.pop(guild_id, set())
            if mutated:
                live_known = self._known_members_by_guild.get(
                    guild_id, frozenset()
                )
                live_taxable = self._nickname_taxable_by_guild.get(
                    guild_id, frozenset()
                )
                for discord_id in mutated:
                    if discord_id in live_known:
                        known_members.add(discord_id)
                        if discord_id in live_taxable:
                            nickname_taxable.add(discord_id)
                        else:
                            nickname_taxable.discard(discord_id)
                    else:
                        known_members.discard(discord_id)
                        nickname_taxable.discard(discord_id)
            self._known_members_by_guild[guild_id] = frozenset(known_members)
            self._nickname_taxable_by_guild[guild_id] = frozenset(
                nickname_taxable
            )
            self._manual_taxable_by_guild[guild_id] = manual_taxable

    def refresh_guild(
        self,
        guild_id: int,
        members: Iterable[object],
        generation: int | None = None,
    ) -> None:
        known_members: set[int] = set()
        nickname_taxable: set[int] = set()
        for member in members:
            discord_id = int(member.id)
            known_members.add(discord_id)
            if getattr(member, "nick", None) is None:
                nickname_taxable.add(discord_id)
        if self._repository is not None:
            # Blocking SQLite read happens outside the cache lock so event-loop
            # callers never wait on DB I/O.
            with self._io_lock:
                manual_taxable = (
                    self._repository.get_vanity_tax_enforcements(guild_id)
                )
                self._store_refresh(
                    guild_id,
                    known_members,
                    nickname_taxable,
                    manual_taxable,
                    generation=generation,
                )
        else:
            with self._lock:
                manual_taxable = self._manual_taxable_by_guild.get(
                    guild_id,
                    frozenset(),
                )
                self._store_refresh(
                    guild_id,
                    known_members,
                    nickname_taxable,
                    manual_taxable,
                    generation=generation,
                )

    def update_member(
        self,
        guild_id: int,
        discord_id: int,
        nickname: str | None,
    ) -> None:
        with self._lock:
            known_members = set(
                self._known_members_by_guild.get(guild_id, ())
            )
            known_members.add(discord_id)
            taxable = set(self._nickname_taxable_by_guild.get(guild_id, ()))
            if nickname is None:
                taxable.add(discord_id)
            else:
                taxable.discard(discord_id)
            self._known_members_by_guild[guild_id] = frozenset(known_members)
            self._nickname_taxable_by_guild[guild_id] = frozenset(taxable)
            self._mutated_members_by_guild.setdefault(guild_id, set()).add(
                discord_id
            )

    def remove_member(self, guild_id: int, discord_id: int) -> None:
        with self._lock:
            known_members = set(
                self._known_members_by_guild.get(guild_id, ())
            )
            known_members.discard(discord_id)
            taxable = set(self._nickname_taxable_by_guild.get(guild_id, ()))
            taxable.discard(discord_id)
            self._known_members_by_guild[guild_id] = frozenset(known_members)
            self._nickname_taxable_by_guild[guild_id] = frozenset(taxable)
            self._mutated_members_by_guild.setdefault(guild_id, set()).add(
                discord_id
            )

    def set_manual_taxation(
        self,
        guild_id: int,
        discord_id: int,
        *,
        enforced: bool,
        actor_id: int,
    ) -> None:
        """Force taxation or restore the automatic nickname rule."""
        if self._repository is not None:
            # The blocking SQLite write happens outside the cache lock so
            # event-loop callers never wait on DB I/O.
            with self._io_lock:
                enforced_ids = self._repository.set_vanity_tax_enforcement(
                    guild_id,
                    discord_id,
                    enforced=enforced,
                    actor_id=actor_id,
                )
                with self._lock:
                    self._manual_taxable_by_guild[guild_id] = enforced_ids
        else:
            with self._lock:
                updated = set(
                    self._manual_taxable_by_guild.get(guild_id, ())
                )
                if enforced:
                    updated.add(discord_id)
                else:
                    updated.discard(discord_id)
                self._manual_taxable_by_guild[guild_id] = frozenset(updated)

    def eligibility_status(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> str:
        """Return the current manual, nickname, taxable, or unknown status."""
        if guild_id is None:
            return "unknown"
        with self._lock:
            if discord_id in self._manual_taxable_by_guild.get(guild_id, ()):
                return "manual_taxation"
            known_members = self._known_members_by_guild.get(guild_id)
            if known_members is None or discord_id not in known_members:
                return "unknown"
            if discord_id in self._nickname_taxable_by_guild.get(guild_id, ()):
                return "taxable"
            return "nickname_exemption"

    def is_manually_taxed(self, guild_id: int | None, discord_id: int) -> bool:
        """Return whether an admin override taxes this member."""
        if guild_id is None:
            return False
        with self._lock:
            return discord_id in self._manual_taxable_by_guild.get(
                guild_id,
                (),
            )

    def taxable_ids(self, guild_id: int | None) -> frozenset[int]:
        """Return nickname-taxable and manually enforced member ids."""
        if guild_id is None:
            return frozenset()
        with self._lock:
            return self._nickname_taxable_by_guild.get(
                guild_id,
                frozenset(),
            ) | self._manual_taxable_by_guild.get(guild_id, frozenset())

    def calculate_tax(
        self,
        discord_id: int,
        guild_id: int | None,
        profit: int,
    ) -> int:
        if guild_id is None or profit <= 0:
            return 0
        if discord_id not in self.taxable_ids(guild_id):
            return 0
        # Floor keeps profits under 10 JC untaxed at the default 10% rate.
        return int(profit * self.TAX_RATE)
