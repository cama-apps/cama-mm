"""Service orchestrating manashop time-limited buffs.

A thin facade over ``BuffRepository`` that names each buff lifecycle for
clarity and centralises the durations / payloads. Manashop ultimates write
through this service; consumers (sabotage/match/dig handlers) read through
``has_active`` / ``active_for`` and call ``consume`` to spend single-charge
buffs (e.g. Aegis absorbing one PvP attack).
"""

import logging
import time
import uuid
from typing import TYPE_CHECKING

from utils.economy_scaling import scale_minigame_jc_delta

logger = logging.getLogger("cama_bot.services.buff")

if TYPE_CHECKING:
    from repositories.buff_repository import BuffRepository
    from repositories.player_repository import PlayerRepository

# Buff type keys
BUFF_COUNTERSPELL = "counterspell"
BUFF_REPRIEVE = "reprieve"
BUFF_AEGIS = "aegis"
BUFF_OVERGROWTH = "overgrowth"
BUFF_SANCTUARY = "sanctuary"
BUFF_BLOOD_PACT = "blood_pact"
BUFF_DARK_BARGAIN = "dark_bargain"
BUFF_FIRST_AEGIS_TODAY = "first_aegis_today"  # Auto-granted by White mana
BUFF_COMMUNION_BLESSING = "communion_blessing"  # Single-charge +10% next match win


# Hours
HOURS = 3600


class BuffService:
    """Manages 24h manashop buffs via BuffRepository."""

    def __init__(self, buff_repo: "BuffRepository", protection_service=None):
        self.buff_repo = buff_repo
        # Back-filled by the service container because ProtectionService itself
        # depends on the buff repository.  Keeping the dependency optional also
        # preserves the lightweight service construction used by tests.
        self.protection_service = protection_service

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

    @staticmethod
    def _protection_pool_data(
        capacity: int,
        rate: float,
        *,
        shared: bool = False,
        rolling_retroactive: bool = False,
        **extra,
    ) -> dict:
        """Return the stable payload consumed by ``ProtectionService``."""
        return {
            "capacity": capacity,
            "capacity_remaining": capacity,
            "rate": rate,
            "shared": shared,
            "rolling_retroactive": rolling_retroactive,
            **extra,
        }

    def grant_reprieve(self, discord_id: int, guild_id: int | None) -> int:
        """24h personal pool: absorb 50% of hostile losses, up to 25 JC.

        Reprieve is the retroactive tier.  The command reconciles eligible
        losses from the preceding rolling 24-hour window immediately after the
        row is granted, then the remaining capacity protects future losses.
        """
        return self.buff_repo.grant(
            discord_id,
            guild_id,
            BUFF_REPRIEVE,
            self._expires(24),
            data=self._protection_pool_data(
                25,
                0.50,
                rolling_retroactive=True,
            ),
        )

    def grant_aegis(self, discord_id: int, guild_id: int | None) -> int:
        """24h personal pool: fully absorb up to 75 JC of hostile losses."""
        return self.buff_repo.grant(
            discord_id,
            guild_id,
            BUFF_AEGIS,
            self._expires(24),
            data=self._protection_pool_data(75, 1.0),
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
        """24h shared pool for caster + ally: fully absorb up to 150 JC."""
        return self.buff_repo.grant(
            caster_id,
            guild_id,
            BUFF_SANCTUARY,
            self._expires(24),
            target_id=ally_id,
            data=self._protection_pool_data(
                150,
                1.0,
                shared=True,
                caster_id=caster_id,
                ally_id=ally_id,
                protected_user_ids=[caster_id, ally_id],
            ),
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



    # ------------------------------------------------------------------
    # Read / consume helpers
    # ------------------------------------------------------------------

    def has_pvp_immunity(self, discord_id: int, guild_id: int | None) -> bool:
        """Counterspell or Sanctuary covers non-JC PvP attacks for 24h."""
        if self.buff_repo.has_active(discord_id, guild_id, BUFF_COUNTERSPELL):
            return True
        # Sanctuary protects both caster and ally. Its JC capacity is consumed
        # by ProtectionService; non-JC sabotage remains part of its immunity.
        if self.buff_repo.has_active(discord_id, guild_id, BUFF_SANCTUARY):
            return True
        return bool(
            self.buff_repo.active_targeted_at(
                discord_id, guild_id, BUFF_SANCTUARY
            )
        )

    def consume_aegis_charge(self, discord_id: int, guild_id: int | None) -> bool:
        """Consume Aegis to absorb one non-JC attack such as sabotage.

        A capacity-backed Aegis may protect multiple JC losses, or it may be
        spent wholesale to stop one non-JC attack. The latter intentionally
        marks the complete buff row triggered.
        """
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
        """Sanctuary no longer copies a match-win bonus."""
        _ = (discord_id, guild_id)
        return False

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
        reserved_amount = int(skim["amount"])
        amount = scale_minigame_jc_delta(reserved_amount)
        skimmer_id = int(skim["skimmer_id"])
        buff_id = int(skim["buff_id"])
        try:
            if self.protection_service is not None:
                settlement = self.protection_service.apply_hostile_loss(
                    target_id,
                    guild_id,
                    amount,
                    "blood_pact",
                    actor_id=skimmer_id,
                    event_key=f"blood-pact:{buff_id}:{uuid.uuid4().hex}",
                    destination="player",
                    recipient_id=skimmer_id,
                    clamp_to_balance=False,
                    metadata={"buff_id": buff_id, "earning": earning},
                )
                applied = int(settlement.applied)
                if applied < reserved_amount:
                    # Capacity is based on what the pact actually collected,
                    # not the portion White mana prevented.
                    try:
                        self.buff_repo.revert_blood_pact_skim(
                            buff_id, reserved_amount - applied
                        )
                    except Exception:
                        logger.exception(
                            "Failed to release shielded Blood Pact capacity for buff %d",
                            buff_id,
                        )
                return applied
            player_repo.add_balance_many(
                {target_id: -amount, skimmer_id: amount},
                guild_id,
            )
            if amount < reserved_amount:
                try:
                    self.buff_repo.revert_blood_pact_skim(
                        buff_id, reserved_amount - amount
                    )
                except Exception:
                    logger.exception(
                        "Failed to release scaled Blood Pact capacity for buff %d",
                        buff_id,
                    )
        except Exception:
            logger.exception(
                "Failed to transfer Blood Pact skim for player %d", target_id
            )
            try:
                self.buff_repo.revert_blood_pact_skim(buff_id, reserved_amount)
            except Exception:
                logger.exception(
                    "Failed to revert Blood Pact skim for buff %d", buff_id
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
