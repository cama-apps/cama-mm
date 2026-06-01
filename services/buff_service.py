"""Service orchestrating manashop time-limited buffs.

A thin facade over ``BuffRepository`` that names each buff lifecycle for
clarity and centralises the durations / payloads. Manashop ultimates write
through this service; consumers (sabotage/match/dig handlers) read through
``has_active`` / ``active_for`` and call ``consume`` to spend single-charge
buffs (e.g. Aegis absorbing one PvP attack).
"""

import logging
import time
from typing import TYPE_CHECKING

logger = logging.getLogger("cama_bot.services.buff")

if TYPE_CHECKING:
    from repositories.buff_repository import BuffRepository

# Buff type keys
BUFF_COUNTERSPELL = "counterspell"
BUFF_AEGIS = "aegis"
BUFF_OVERGROWTH = "overgrowth"
BUFF_SANCTUARY = "sanctuary"
BUFF_BLOOD_PACT = "blood_pact"
BUFF_DARK_BARGAIN = "dark_bargain"
BUFF_FIRST_AEGIS_TODAY = "first_aegis_today"  # Auto-granted by White mana
BUFF_COMMUNION_BLESSING = "communion_blessing"  # Single-charge +10% next match win

# All PvP-defending buff types (any of these blocks Pyroclasm/Soul-Harvest/Sabotage/etc.)
PVP_DEFENSE_BUFFS = (BUFF_COUNTERSPELL, BUFF_AEGIS, BUFF_SANCTUARY, BUFF_FIRST_AEGIS_TODAY)

# Hours
HOURS = 3600


class BuffService:
    """Manages 24h manashop buffs via BuffRepository."""

    def __init__(self, buff_repo: "BuffRepository"):
        self.buff_repo = buff_repo

    # ------------------------------------------------------------------
    # Grant helpers
    # ------------------------------------------------------------------

    def _expires(self, hours: int) -> int:
        return int(time.time()) + hours * HOURS

    def grant_counterspell(self, discord_id: int, guild_id: int | None) -> int:
        """24h immunity to all PvP manashop targeting. Multi-charge (no triggered=1)."""
        return self.buff_repo.grant(
            discord_id, guild_id, BUFF_COUNTERSPELL, self._expires(24)
        )

    def grant_aegis(self, discord_id: int, guild_id: int | None) -> int:
        """Single-charge: absorbs the next PvP attack. Expires after 24h."""
        return self.buff_repo.grant(
            discord_id, guild_id, BUFF_AEGIS, self._expires(24)
        )

    def grant_overgrowth(self, discord_id: int, guild_id: int | None) -> int:
        """12h dig boost (read by dig service).

        Re-granting refreshes the timer rather than extending: any existing
        active overgrowth row is expired in the same transaction as the new
        grant so concurrent re-purchases can't both leave a row alive.
        """
        return self.buff_repo.refresh_atomic(
            discord_id,
            guild_id,
            BUFF_OVERGROWTH,
            self._expires(12),
            data={"charges_remaining": 10},
        )

    def grant_sanctuary(
        self, caster_id: int, guild_id: int | None, ally_id: int
    ) -> int:
        """24h: caster + ally both gain PvP immunity and +15% match-win bonus."""
        return self.buff_repo.grant(
            caster_id,
            guild_id,
            BUFF_SANCTUARY,
            self._expires(24),
            target_id=ally_id,
        )

    def grant_blood_pact(
        self, caster_id: int, guild_id: int | None, target_id: int
    ) -> int:
        """24h: caster skims 25% of target's earnings (cap 150 JC total)."""
        return self.buff_repo.grant(
            caster_id,
            guild_id,
            BUFF_BLOOD_PACT,
            self._expires(24),
            target_id=target_id,
            data={"skimmed_total": 0, "cap": 150, "skim_rate": 0.25},
        )

    def grant_dark_bargain_debt(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        amount_due: int,
        due_in_days: int = 7,
    ) -> int:
        """7-day debt obligation tracked alongside normal loans."""
        return self.buff_repo.grant(
            discord_id,
            guild_id,
            BUFF_DARK_BARGAIN,
            self._expires(24 * due_in_days),
            data={
                "amount_due": amount_due,
                "amount_paid": 0,
                "default_penalty": 1600,
                "default_penalty_games": 5,
            },
        )

    def grant_first_aegis_today(
        self, discord_id: int, guild_id: int | None, *, hours: int = 24
    ) -> int:
        """White mana passive: free aegis vs first sabotage of the day.
        Caller (mana assignment) is expected to grant once per day."""
        return self.buff_repo.grant(
            discord_id, guild_id, BUFF_FIRST_AEGIS_TODAY, self._expires(hours)
        )

    def grant_communion_blessing(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        match_win_bonus_pct: float = 0.10,
    ) -> int:
        """Manashop Communion: single-charge +10% next match-win bonus.

        Consumed atomically by ``BettingService.award_win_bonus`` via
        ``buff_repo.consume_atomic`` so concurrent match finalizations only
        pay the bonus once.
        """
        return self.buff_repo.grant(
            discord_id, guild_id, BUFF_COMMUNION_BLESSING,
            self._expires(24),
            data={"match_win_bonus_pct": match_win_bonus_pct},
        )

    # ------------------------------------------------------------------
    # Read / consume helpers
    # ------------------------------------------------------------------

    def has_pvp_immunity(self, discord_id: int, guild_id: int | None) -> bool:
        """Counterspell or Sanctuary covers ALL PvP attacks for 24h."""
        if self.buff_repo.has_active(discord_id, guild_id, BUFF_COUNTERSPELL):
            return True
        # Sanctuary protects both caster and ally
        if self.buff_repo.has_active(discord_id, guild_id, BUFF_SANCTUARY):
            return True
        return bool(self.buff_repo.active_targeted_at(discord_id, guild_id, BUFF_SANCTUARY))

    def consume_aegis_charge(self, discord_id: int, guild_id: int | None) -> bool:
        """If the player has any Aegis charge (manual or first-sabotage-today),
        consume the most recent one and return True."""
        for buff_type in (BUFF_AEGIS, BUFF_FIRST_AEGIS_TODAY):
            buffs = self.buff_repo.active_for(discord_id, guild_id, buff_type)
            if not buffs:
                continue
            # Most recent first; consume one charge
            for buff in buffs:
                if self.buff_repo.consume_atomic(buff["id"]):
                    return True
        return False

    def has_overgrowth(self, discord_id: int, guild_id: int | None) -> bool:
        return self.buff_repo.has_active(discord_id, guild_id, BUFF_OVERGROWTH)

    def consume_overgrowth_charge(self, discord_id: int, guild_id: int | None) -> bool:
        return self.buff_repo.consume_data_charge_atomic(
            discord_id, guild_id, BUFF_OVERGROWTH, "charges_remaining"
        )

    def has_sanctuary_match_bonus(
        self, discord_id: int, guild_id: int | None
    ) -> bool:
        """True if either caster or ally has an active Sanctuary."""
        if self.buff_repo.has_active(discord_id, guild_id, BUFF_SANCTUARY):
            return True
        return bool(self.buff_repo.active_targeted_at(discord_id, guild_id, BUFF_SANCTUARY))

    def get_blood_pact_skimmer(
        self, target_id: int, guild_id: int | None
    ) -> dict | None:
        """If the player is the *target* of a Blood Pact, return the most
        recent active pact dict (with caster discord_id stored as
        ``discord_id`` and accumulated state in ``data``)."""
        active = self.buff_repo.active_targeted_at(target_id, guild_id, BUFF_BLOOD_PACT)
        return active[0] if active else None

    def record_blood_pact_skim(
        self, buff_id: int, current_data: dict, new_total: int
    ) -> None:
        """Update the running skim total on a Blood Pact buff.

        ``current_data`` is the buff's existing ``data`` blob (as returned by
        ``get_blood_pact_skimmer``). Only ``skimmed_total`` is updated so the
        stored ``cap`` / ``skim_rate`` are preserved rather than overwritten
        with hardcoded defaults.
        """
        data = dict(current_data)
        data["skimmed_total"] = new_total
        self.buff_repo.update_data(buff_id, data)

    def claim_blood_pact_skim(
        self, target_id: int, guild_id: int | None, earning: int
    ) -> dict | None:
        """Claim the active Blood Pact skim for one positive earning event.

        Returns ``{buff_id, skimmer_id, amount, new_total}`` for the caller to
        transfer from the target to the skimmer. The transfer itself stays at
        the earning source so it can share that source's balance transaction
        rules.
        """
        if earning <= 0:
            return None
        pact = self.get_blood_pact_skimmer(target_id, guild_id)
        if not pact:
            return None
        data = pact.get("data") or {}
        cap = int(data.get("cap") or 0)
        skimmed_total = int(data.get("skimmed_total") or 0)
        remaining = cap - skimmed_total
        if remaining <= 0:
            return None
        skim_rate = float(data.get("skim_rate") or 0)
        amount = min(remaining, max(1, int(earning * skim_rate)))
        if amount <= 0:
            return None
        new_total = skimmed_total + amount
        self.record_blood_pact_skim(pact["id"], data, new_total)
        return {
            "buff_id": pact["id"],
            "skimmer_id": pact["discord_id"],
            "amount": amount,
            "new_total": new_total,
        }

    def has_dark_bargain_debt(
        self, discord_id: int, guild_id: int | None
    ) -> dict | None:
        """Return the active Dark Bargain debt row or None."""
        active = self.buff_repo.active_for(discord_id, guild_id, BUFF_DARK_BARGAIN)
        return active[0] if active else None

    def settle_due_dark_bargains(self, *, player_repo, bankruptcy_repo) -> list[dict]:
        _ = (player_repo, bankruptcy_repo)
        return self.buff_repo.settle_due_dark_bargains(
            now=int(time.time()),
        )

    def cleanup_expired(self) -> int:
        return self.buff_repo.cleanup_expired()
