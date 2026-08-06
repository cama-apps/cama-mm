"""Track Discord nickname eligibility for the vanity tax."""

from __future__ import annotations

from collections.abc import Iterable
from threading import RLock
from typing import TYPE_CHECKING

from config import VANITY_TAX_RATE

if TYPE_CHECKING:
    from repositories.tax_repository import TaxRepository


class VanityTaxService:
    """Apply a 10% profit tax to guild members without a server nickname."""

    TAX_RATE = VANITY_TAX_RATE

    def __init__(self, repository: TaxRepository | None = None) -> None:
        self._repository = repository
        self._lock = RLock()
        self._known_members_by_guild: dict[int, frozenset[int]] = {}
        self._nickname_taxable_by_guild: dict[int, frozenset[int]] = {}
        self._manual_exemptions_by_guild: dict[int, frozenset[int]] = {}

    def refresh_guild(self, guild_id: int, members: Iterable[object]) -> None:
        known_members: set[int] = set()
        nickname_taxable: set[int] = set()
        for member in members:
            discord_id = int(member.id)
            known_members.add(discord_id)
            if getattr(member, "nick", None) is None:
                nickname_taxable.add(discord_id)
        with self._lock:
            if self._repository is not None:
                manual_exemptions = (
                    self._repository.get_vanity_tax_exemptions(guild_id)
                )
            else:
                manual_exemptions = self._manual_exemptions_by_guild.get(
                    guild_id,
                    frozenset(),
                )
            self._known_members_by_guild[guild_id] = frozenset(known_members)
            self._nickname_taxable_by_guild[guild_id] = frozenset(
                nickname_taxable
            )
            self._manual_exemptions_by_guild[guild_id] = manual_exemptions

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

    def set_manual_exemption(
        self,
        guild_id: int,
        discord_id: int,
        *,
        exempt: bool,
        actor_id: int,
    ) -> None:
        """Grant or revoke a persistent exemption from the nickname rule."""
        with self._lock:
            if self._repository is not None:
                exemptions = self._repository.set_vanity_tax_exemption(
                    guild_id,
                    discord_id,
                    exempt=exempt,
                    actor_id=actor_id,
                )
            else:
                updated = set(
                    self._manual_exemptions_by_guild.get(guild_id, ())
                )
                if exempt:
                    updated.add(discord_id)
                else:
                    updated.discard(discord_id)
                exemptions = frozenset(updated)
            self._manual_exemptions_by_guild[guild_id] = exemptions

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
