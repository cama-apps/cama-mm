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
    from repositories.player_repository import PlayerRepository

# Buff type keys
BUFF_COUNTERSPELL = "counterspell"
BUFF_AEGIS = "aegis"
BUFF_OVERGROWTH = "overgrowth"
BUFF_SANCTUARY = "sanctuary"
BUFF_BLOOD_PACT = "blood_pact"
BUFF_DARK_BARGAIN = "dark_bargain"
BUFF_FIRST_AEGIS_TODAY = "first_aegis_today"  # Auto-granted by White mana
BUFF_COMMUNION_BLESSING = "communion_blessing"  # Single-charge +10% next match win
BUFF_RECKLESS_RITUAL = "reckless_ritual"  # One 7x match bet today
BUFF_TRANSMUTE = "transmute"  # Current mana counts as any non-ultimate color
BUFF_VERDANT_RESERVE = "verdant_reserve"  # +10 on next 3 positive gains, capped at 75
BUFF_ALMS_ROUND = "alms_round"  # +1 on next 10 positive match/dig/trivia gains
BUFF_GRAVE_CONTRACT = "grave_contract"  # 15% skim from target gains, cap 100

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

    def grant_reckless_ritual(self, discord_id: int, guild_id: int | None) -> int:
        """24h: unlock one 7x match bet. Consumed when a 7x bet is placed."""
        return self.buff_repo.refresh_atomic(
            discord_id, guild_id, BUFF_RECKLESS_RITUAL, self._expires(24)
        )

    def grant_transmute(self, discord_id: int, guild_id: int | None) -> int:
        """24h: satisfy manashop color checks for Cheap/Mid/High items."""
        return self.buff_repo.refresh_atomic(
            discord_id, guild_id, BUFF_TRANSMUTE, self._expires(24)
        )

    def grant_verdant_reserve(self, discord_id: int, guild_id: int | None) -> int:
        """24h: +10 on next three positive gains, each capped at 75 total."""
        return self.buff_repo.refresh_atomic(
            discord_id,
            guild_id,
            BUFF_VERDANT_RESERVE,
            self._expires(24),
            data={"charges_remaining": 3, "bonus": 10, "gain_cap": 75},
        )

    def grant_alms_round(self, discord_id: int, guild_id: int | None) -> int:
        """24h: +1 on next ten positive match/dig/trivia gains."""
        return self.buff_repo.refresh_atomic(
            discord_id,
            guild_id,
            BUFF_ALMS_ROUND,
            self._expires(24),
            data={"charges_remaining": 10, "bonus": 1},
        )

    def grant_grave_contract(
        self, caster_id: int, guild_id: int | None, target_id: int
    ) -> int:
        """24h: caster skims 15% of target's earnings (cap 100 JC total)."""
        return self.buff_repo.grant(
            caster_id,
            guild_id,
            BUFF_GRAVE_CONTRACT,
            self._expires(24),
            target_id=target_id,
            data={"skimmed_total": 0, "cap": 100, "skim_rate": 0.15},
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

    def has_transmute(self, discord_id: int, guild_id: int | None) -> bool:
        return self.buff_repo.has_active(discord_id, guild_id, BUFF_TRANSMUTE)

    def has_reckless_ritual(self, discord_id: int, guild_id: int | None) -> bool:
        return self.buff_repo.has_active(discord_id, guild_id, BUFF_RECKLESS_RITUAL)

    def consume_reckless_ritual(self, discord_id: int, guild_id: int | None) -> bool:
        active = self.buff_repo.active_for(discord_id, guild_id, BUFF_RECKLESS_RITUAL)
        if not active:
            return False
        return self.buff_repo.consume_atomic(active[0]["id"])

    def consume_overgrowth_charge(self, discord_id: int, guild_id: int | None) -> bool:
        return self.buff_repo.consume_data_charge_atomic(
            discord_id, guild_id, BUFF_OVERGROWTH, "charges_remaining"
        )

    def apply_positive_gain_bonuses(
        self,
        discord_id: int,
        guild_id: int | None,
        gain: int,
        player_repo: "PlayerRepository",
    ) -> int:
        """Credit high-tier Green/White gain bonuses and return the extra JC.

        Verdant Reserve applies first: +10 while keeping that individual gain
        at or below its stored cap. Alms Round then adds +1. Each successful
        bonus consumes one charge from its corresponding buff.
        """
        if gain <= 0:
            return 0
        credited = 0
        verdant = self.buff_repo.active_for(discord_id, guild_id, BUFF_VERDANT_RESERVE)
        if verdant:
            data = verdant[0].get("data") or {}
            bonus = int(data.get("bonus") or 0)
            gain_cap = int(data.get("gain_cap") or 0)
            bonus = max(0, min(bonus, gain_cap - gain if gain_cap > 0 else bonus))
            consumed = self.buff_repo.consume_data_charge_atomic(
                discord_id, guild_id, BUFF_VERDANT_RESERVE, "charges_remaining"
            )
            if bonus > 0 and consumed:
                player_repo.add_balance(
                    discord_id,
                    guild_id,
                    bonus,
                    source="manashop_buff",
                    related_type=BUFF_VERDANT_RESERVE,
                    reason="verdant reserve gain bonus",
                    metadata={"base_gain": gain, "bonus": bonus},
                )
                credited += bonus

        alms = self.buff_repo.active_for(discord_id, guild_id, BUFF_ALMS_ROUND)
        if alms:
            data = alms[0].get("data") or {}
            bonus = int(data.get("bonus") or 0)
            if bonus > 0 and self.buff_repo.consume_data_charge_atomic(
                discord_id, guild_id, BUFF_ALMS_ROUND, "charges_remaining"
            ):
                player_repo.add_balance(
                    discord_id,
                    guild_id,
                    bonus,
                    source="manashop_buff",
                    related_type=BUFF_ALMS_ROUND,
                    reason="alms round gain bonus",
                    metadata={"base_gain": gain, "bonus": bonus},
                )
                credited += bonus
        return credited

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
        """Atomically reserve a Blood Pact skim without transferring balances."""
        return self.buff_repo.claim_blood_pact_skim_atomic(
            target_id, guild_id, earning
        )

    def apply_blood_pact_skim(
        self,
        target_id: int,
        guild_id: int | None,
        earning: int,
        player_repo: "PlayerRepository",
    ) -> int:
        """Reserve, transfer, and roll back the skim counter on transfer failure."""
        if earning <= 0:
            return 0
        try:
            skim = self.buff_repo.claim_blood_pact_skim_atomic(
                target_id, guild_id, earning
            )
        except Exception:
            logger.exception(
                "Failed to claim Blood Pact skim for player %d", target_id
            )
            return 0
        if not skim:
            return 0
        amount = int(skim["amount"])
        skimmer_id = int(skim["skimmer_id"])
        buff_id = int(skim["buff_id"])
        try:
            player_repo.add_balance_many(
                {target_id: -amount, skimmer_id: amount},
                guild_id,
            )
        except Exception:
            logger.exception(
                "Failed to transfer Blood Pact skim for player %d", target_id
            )
            try:
                self.buff_repo.revert_blood_pact_skim(buff_id, amount)
            except Exception:
                logger.exception(
                    "Failed to revert Blood Pact skim for buff %d", buff_id
                )
            return 0
        return amount

    def apply_grave_contract_skim(
        self,
        target_id: int,
        guild_id: int | None,
        earning: int,
        player_repo: "PlayerRepository",
    ) -> int:
        """Reserve, transfer, and roll back the Grave Contract skim on failure."""
        if earning <= 0:
            return 0
        try:
            skim = self.buff_repo.claim_contract_skim_atomic(
                target_id, guild_id, earning, BUFF_GRAVE_CONTRACT
            )
        except Exception:
            logger.exception(
                "Failed to claim Grave Contract skim for player %d", target_id
            )
            return 0
        if not skim:
            return 0
        amount = int(skim["amount"])
        skimmer_id = int(skim["skimmer_id"])
        buff_id = int(skim["buff_id"])
        try:
            player_repo.add_balance_many(
                {target_id: -amount, skimmer_id: amount},
                guild_id,
            )
        except Exception:
            logger.exception(
                "Failed to transfer Grave Contract skim for player %d", target_id
            )
            try:
                self.buff_repo.revert_blood_pact_skim(buff_id, amount)
            except Exception:
                logger.exception(
                    "Failed to revert Grave Contract skim for buff %d", buff_id
                )
            return 0
        return amount

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
