"""Track Discord nickname eligibility for the vanity tax."""

from collections.abc import Iterable


class VanityTaxService:
    """Apply a 5% profit tax to guild members without a server nickname."""

    TAX_RATE = 0.05

    def __init__(self) -> None:
        self._taxable_by_guild: dict[int, frozenset[int]] = {}

    def refresh_guild(self, guild_id: int, members: Iterable[object]) -> None:
        self._taxable_by_guild[guild_id] = frozenset(
            int(member.id)
            for member in members
            if getattr(member, "nick", None) is None
        )

    def update_member(
        self,
        guild_id: int,
        discord_id: int,
        nickname: str | None,
    ) -> None:
        taxable = set(self._taxable_by_guild.get(guild_id, ()))
        if nickname is None:
            taxable.add(discord_id)
        else:
            taxable.discard(discord_id)
        self._taxable_by_guild[guild_id] = frozenset(taxable)

    def remove_member(self, guild_id: int, discord_id: int) -> None:
        taxable = set(self._taxable_by_guild.get(guild_id, ()))
        taxable.discard(discord_id)
        self._taxable_by_guild[guild_id] = frozenset(taxable)

    def taxable_ids(self, guild_id: int | None) -> frozenset[int]:
        """Return the latest known taxable members, failing open if unknown."""
        if guild_id is None:
            return frozenset()
        return self._taxable_by_guild.get(guild_id, frozenset())

    def calculate_tax(
        self,
        discord_id: int,
        guild_id: int | None,
        profit: int,
    ) -> int:
        if guild_id is None or profit <= 0:
            return 0
        if discord_id not in self._taxable_by_guild.get(guild_id, ()):
            return 0
        # Floor keeps tiny profits untaxed; the 5% rate (up from 1%) is what
        # makes the tax visible at this economy's payout sizes.
        return int(profit * self.TAX_RATE)
