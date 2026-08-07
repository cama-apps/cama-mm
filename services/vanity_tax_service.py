"""Track Discord nickname eligibility for the vanity tax."""

from __future__ import annotations

from collections.abc import Iterable
from threading import Lock, RLock
from typing import TYPE_CHECKING

from config import VANITY_TAX_RATE

if TYPE_CHECKING:
    from repositories.tax_repository import TaxRepository


class VanityTaxService:
    """Apply a 10% profit tax to guild members without a server nickname."""

    TAX_RATE = VANITY_TAX_RATE

    def __init__(self, repository: TaxRepository | None = None) -> None:
        self._repository = repository
        # ``_lock`` guards only the in-memory caches and is acquired by sync
        # event handlers on the event loop (on_member_update etc.), so it must
        # never be held across repository I/O. ``_io_lock`` serializes the
        # repository read/write + cache swap so a stale refresh cannot clobber
        # a newer manual-exemption write.
        self._lock = RLock()
        self._io_lock = Lock()
        self._known_members_by_guild: dict[int, frozenset[int]] = {}
        self._nickname_taxable_by_guild: dict[int, frozenset[int]] = {}
        self._manual_exemptions_by_guild: dict[int, frozenset[int]] = {}
        # Ids touched by update_member/remove_member since the last completed
        # refresh. ``refresh_guild`` runs off-loop from a snapshot taken on
        # the event loop; a member event landing before ``_store_refresh``
        # would otherwise be clobbered by the (older) wholesale store, so the
        # store re-applies the live state of these ids on top of the snapshot.
        self._mutated_members_by_guild: dict[int, set[int]] = {}

    def _store_refresh(
        self,
        guild_id: int,
        known_members: set[int],
        nickname_taxable: set[int],
        manual_exemptions: frozenset[int],
    ) -> None:
        with self._lock:
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
            self._manual_exemptions_by_guild[guild_id] = manual_exemptions

    def refresh_guild(self, guild_id: int, members: Iterable[object]) -> None:
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
                manual_exemptions = (
                    self._repository.get_vanity_tax_exemptions(guild_id)
                )
                self._store_refresh(
                    guild_id,
                    known_members,
                    nickname_taxable,
                    manual_exemptions,
                )
        else:
            with self._lock:
                manual_exemptions = self._manual_exemptions_by_guild.get(
                    guild_id,
                    frozenset(),
                )
                self._store_refresh(
                    guild_id,
                    known_members,
                    nickname_taxable,
                    manual_exemptions,
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

    def set_manual_exemption(
        self,
        guild_id: int,
        discord_id: int,
        *,
        exempt: bool,
        actor_id: int,
    ) -> None:
        """Grant or revoke a persistent exemption from the nickname rule."""
        if self._repository is not None:
            # The blocking SQLite write happens outside the cache lock so
            # event-loop callers never wait on DB I/O.
            with self._io_lock:
                exemptions = self._repository.set_vanity_tax_exemption(
                    guild_id,
                    discord_id,
                    exempt=exempt,
                    actor_id=actor_id,
                )
                with self._lock:
                    self._manual_exemptions_by_guild[guild_id] = exemptions
        else:
            with self._lock:
                updated = set(
                    self._manual_exemptions_by_guild.get(guild_id, ())
                )
                if exempt:
                    updated.add(discord_id)
                else:
                    updated.discard(discord_id)
                self._manual_exemptions_by_guild[guild_id] = frozenset(updated)

    def eligibility_status(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> str:
        """Return the current manual, nickname, taxable, or unknown status."""
        if guild_id is None:
            return "unknown"
        with self._lock:
            if discord_id in self._manual_exemptions_by_guild.get(guild_id, ()):
                return "manual_exemption"
            known_members = self._known_members_by_guild.get(guild_id)
            if known_members is None or discord_id not in known_members:
                return "unknown"
            if discord_id in self._nickname_taxable_by_guild.get(guild_id, ()):
                return "taxable"
            return "nickname_exemption"

    def is_manually_exempt(self, guild_id: int | None, discord_id: int) -> bool:
        """Return whether an admin override exempts this member."""
        if guild_id is None:
            return False
        with self._lock:
            return discord_id in self._manual_exemptions_by_guild.get(
                guild_id,
                (),
            )

    def taxable_ids(self, guild_id: int | None) -> frozenset[int]:
        """Return the latest known taxable members, failing open if unknown."""
        if guild_id is None:
            return frozenset()
        with self._lock:
            return self._nickname_taxable_by_guild.get(
                guild_id,
                frozenset(),
            ) - self._manual_exemptions_by_guild.get(guild_id, frozenset())

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
