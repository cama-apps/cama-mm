"""Service layer for cama pets.

All methods are synchronous (commands wrap them in asyncio.to_thread) and
return Result values with machine-readable codes from services.error_codes.

Core invariants enforced here:
- Lazy death: every read path that touches a pet first resolves starvation
  with the pet's COMPUTED starvation moment (never discovery time), so bot
  downtime only delays announcements, never corrupts state.
- The Shellback's Aegis revive is checked before any death claim, exactly
  once per pet lifetime; a revived pet can still starve again afterwards.
- Refunds are redistribution, not minting: paid from the nonprofit fund's
  existing balance, pro-rata scaled down when the fund can't cover the week.
"""

from __future__ import annotations

import dataclasses
import datetime
import logging
import random
import time

from domain.models.pet import (
    DeathNotice,
    FeedOutcome,
    HatchNotice,
    Pet,
    PetStatus,
    RefundNotice,
    RefundPayout,
    TrinketOutcome,
)
from domain.pet_constants import (
    ACCESSORIES,
    AEGIS_REVIVE_HUNGER,
    EGG_HATCH_SECONDS,
    FEED_CAP_PER_DAY,
    FOOD_ITEMS,
    GILDED_EGG_PREMIUM,
    GILDED_TIER_WEIGHTS,
    MATCH_WIN_HUNGER,
    MAX_BUY_QTY,
    MAX_HUNGER,
    PET_NAME_MAX_LEN,
    PITY_THRESHOLD,
    PITY_TIER_WEIGHTS,
    REFUND_MULT_MAX,
    REFUND_MULT_MIN,
    RENAME_COST,
    SALT_LICK,
    SALT_LICK_DURATION_SECONDS,
    SPECIES,
    SUPPLY_STACK_CAP,
    TRINKET_COST,
    TRINKET_DUPE_REFUND,
    WARNING_HUNGER,
    adoption_fee,
    food_cost,
    get_species,
    species_ids_by_tier,
)
from services import error_codes
from services.result import Result
from utils.game_date import game_date_for_timestamp

logger = logging.getLogger("cama_bot.services.pet")

# Species decay multipliers are bounded; the starvation sweep uses the fastest
# to build a candidate superset that the exact per-species check then filters.
_MAX_DECAY_PCT = max(s.decay_pct for s in SPECIES.values())

_FORBIDDEN_NAME_FRAGMENTS = ("@", "http://", "https://", "discord.gg")


def _week_key_for_timestamp(ts: int) -> str:
    """ISO week key of the game date (weeks roll on game-date Mondays)."""
    date_str = game_date_for_timestamp(ts)
    iso = datetime.datetime.strptime(date_str, "%Y-%m-%d").isocalendar()
    return f"{iso.year}-W{iso.week:02d}"


def _prev_week_key_for_timestamp(ts: int) -> str:
    return _week_key_for_timestamp(ts - 7 * 86400)


class PetService:
    def __init__(self, pet_repo, player_repo, *, decay_per_day: int):
        self.pet_repo = pet_repo
        self.player_repo = player_repo
        self.decay_per_day = decay_per_day

    # --- time helpers (patchable in tests) ---

    def _now(self) -> int:
        return int(time.time())

    # --- lazy starvation resolution ---

    def _resolve_starvation(self, pet: Pet, now: int) -> Pet | None:
        """Return the (possibly Aegis-revived) living pet, or None if it died.

        Death is claimed with the computed starvation moment. A Shellback
        burns its Aegis instead the first time, reviving at low hunger at
        that same moment — and may then starve again before `now`.
        """
        current = pet
        for _ in range(4):  # aegis/lost-claim re-reads can chain
            if current is None:
                return None
            if current.died_at is not None:
                return None
            if not current.is_starved(now, self.decay_per_day):
                return current
            died_at = current.starvation_time(self.decay_per_day)
            species = get_species(current.species)
            if species.has_aegis and not current.aegis_used:
                if self.pet_repo.revive_with_aegis(
                    current.pet_id,
                    current.guild_id,
                    revived_last_fed_at=died_at,
                    revive_hunger=AEGIS_REVIVE_HUNGER,
                    expected_last_fed_at=current.last_fed_at,
                    expected_hunger=current.hunger_at_last_fed,
                ):
                    logger.info(
                        "Pet %s burned its Aegis at %s", current.pet_id, died_at
                    )
                # Re-read either way: on a lost race the other path resolved it.
                current = self.pet_repo.get_pet_by_id(
                    current.pet_id, current.guild_id
                )
                continue
            if self.pet_repo.claim_death(
                current.pet_id,
                current.guild_id,
                died_at=died_at,
                expected_last_fed_at=current.last_fed_at,
                expected_hunger=current.hunger_at_last_fed,
            ):
                return None
            # Lost the claim: the pet was fed/topped-up (or killed) after our
            # snapshot. Re-read and re-derive rather than trusting stale math.
            current = self.pet_repo.get_pet_by_id(current.pet_id, current.guild_id)
        return None

    def _living_pet(self, discord_id: int, guild_id: int | None, now: int) -> Pet | None:
        pet = self.pet_repo.get_active_pet(discord_id, guild_id)
        if pet is None:
            return None
        return self._resolve_starvation(pet, now)

    # --- status ---

    def get_status(self, discord_id: int, guild_id: int | None) -> Result[PetStatus]:
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        supplies = self.pet_repo.get_supplies(discord_id, guild_id)
        if pet is None:
            last_dead = self.pet_repo.get_latest_dead_pet(discord_id, guild_id)
            return Result.ok(
                PetStatus(pet=None, supplies=supplies, last_dead=last_dead)
            )
        return Result.ok(
            PetStatus(
                pet=pet,
                hunger=pet.current_hunger(now, self.decay_per_day),
                stage=pet.stage(now),
                mood=pet.mood(now, self.decay_per_day),
                age_seconds=pet.age_seconds(now),
                supplies=supplies,
            )
        )

    def next_adoption_fee(self, discord_id: int, guild_id: int | None) -> int:
        return adoption_fee(self.pet_repo.count_dead_pets(discord_id, guild_id))

    def warning_crossing_for(self, pet: Pet) -> int:
        """Authoritative warning-DM moment for a pet (single source of truth
        so callers never re-derive it against their own decay value)."""
        return pet.hunger_crossing_time(WARNING_HUNGER, self.decay_per_day)

    def get_warning_crossing(
        self, discord_id: int, guild_id: int | None
    ) -> tuple[int, str] | None:
        """(crossing_ts, pet_name) for the living pet, or None.

        Used by restart recovery so the reminder service never reaches into
        the pet repository or duplicates hunger math.
        """
        pet = self.pet_repo.get_active_pet(discord_id, guild_id)
        if pet is None:
            return None
        return self.warning_crossing_for(pet), pet.name

    # --- adopt / rename ---

    def adopt(
        self,
        discord_id: int,
        guild_id: int | None,
        name: str,
        egg_tier: str = "standard",
    ) -> Result[dict]:
        """Adopt an egg. Returns {"pet", "egg_tier", "pity_active"}.

        Standard eggs use the flat species weights, upgraded to a no-common
        pool when pity is active (PITY_THRESHOLD consecutive common hatches).
        Gilded eggs pay a premium for the boosted pool and ignore pity.
        """
        # NB: a failed Result is falsy, so the sentinel check must be explicit.
        name_error = self._validate_name(name)
        if name_error is not None:
            return name_error
        if egg_tier not in ("standard", "gilded"):
            return Result.fail("Unknown egg tier.", code=error_codes.VALIDATION_ERROR)
        if not self.player_repo.exists(
            discord_id, self.pet_repo.normalize_guild_id(guild_id)
        ):
            return Result.fail(
                "Register with the league before adopting a pet.",
                code=error_codes.PLAYER_NOT_FOUND,
            )
        now = self._now()
        # Resolve a pending starvation first so a long-dead-on-paper pet does
        # not block adoption, and the fee tier reflects that death.
        if self._living_pet(discord_id, guild_id, now) is not None:
            return Result.fail(
                "You already have a pet. One cama per household.",
                code=error_codes.ALREADY_HAS_PET,
            )
        prior_deaths = self.pet_repo.count_dead_pets(discord_id, guild_id)
        fee = adoption_fee(prior_deaths)
        pity_active = False
        if egg_tier == "gilded":
            fee += GILDED_EGG_PREMIUM
            species = self._roll_species_tiered(GILDED_TIER_WEIGHTS)
        else:
            pity_active = self._pity_active(discord_id, guild_id)
            if pity_active:
                species = self._roll_species_tiered(PITY_TIER_WEIGHTS)
            else:
                species_ids = list(SPECIES)
                weights = [SPECIES[s].weight for s in species_ids]
                species = random.choices(species_ids, weights=weights, k=1)[0]
        try:
            pet = self.pet_repo.adopt_pet(
                discord_id,
                guild_id,
                name=name.strip(),
                species=species,
                fee=fee,
                now=now,
                hatch_seconds=EGG_HATCH_SECONDS,
            )
        except ValueError as exc:
            return self._map_repo_error(exc, fee=fee)
        return Result.ok(
            {"pet": pet, "egg_tier": egg_tier, "pity_active": pity_active}
        )

    def _roll_species_tiered(self, tier_weights: dict[str, int]) -> str:
        tiers = list(tier_weights)
        tier = random.choices(tiers, weights=list(tier_weights.values()), k=1)[0]
        return random.choice(species_ids_by_tier(tier))

    def _pity_active(self, discord_id: int, guild_id: int | None) -> bool:
        recent = self.pet_repo.recent_dead_species(
            discord_id, guild_id, PITY_THRESHOLD
        )
        return len(recent) >= PITY_THRESHOLD and all(
            get_species(sp).tier == "common" for sp in recent
        )

    def rename(self, discord_id: int, guild_id: int | None, name: str) -> Result[Pet]:
        name_error = self._validate_name(name)
        if name_error is not None:
            return name_error
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail("You have no living pet.", code=error_codes.NO_PET)
        try:
            self.pet_repo.rename_pet(
                pet.pet_id, discord_id, pet.guild_id,
                new_name=name.strip(), cost=RENAME_COST,
            )
        except ValueError as exc:
            return self._map_repo_error(exc, fee=RENAME_COST)
        return Result.ok(self.pet_repo.get_pet_by_id(pet.pet_id, pet.guild_id))

    # --- shop ---

    def buy(
        self, discord_id: int, guild_id: int | None, item_id: str, qty: int
    ) -> Result[dict]:
        if item_id == SALT_LICK.item_id:
            return self._buy_salt_lick(discord_id, guild_id, qty)
        if item_id not in FOOD_ITEMS:
            return Result.fail("Unknown item.", code=error_codes.VALIDATION_ERROR)
        if not 1 <= qty <= MAX_BUY_QTY:
            return Result.fail(
                f"Quantity must be between 1 and {MAX_BUY_QTY}.",
                code=error_codes.VALIDATION_ERROR,
            )
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        species_id = pet.species if pet is not None and now >= pet.hatched_at else ""
        unit_cost = food_cost(species_id, item_id)
        total = unit_cost * qty
        try:
            new_qty = self.pet_repo.buy_supplies(
                discord_id,
                guild_id,
                item_id=item_id,
                qty=qty,
                total_cost=total,
                now=now,
                stack_cap=SUPPLY_STACK_CAP,
            )
        except ValueError as exc:
            return self._map_repo_error(exc, fee=total)
        return Result.ok(
            {"item_id": item_id, "qty": qty, "total_cost": total, "new_qty": new_qty}
        )

    def _buy_salt_lick(
        self, discord_id: int, guild_id: int | None, qty: int
    ) -> Result[dict]:
        """Salt licks apply immediately (no stockpiling a mood buff)."""
        if qty != 1:
            return Result.fail(
                "Salt licks are applied immediately — buy one at a time.",
                code=error_codes.VALIDATION_ERROR,
            )
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail("You have no living pet.", code=error_codes.NO_PET)
        if now < pet.hatched_at:
            return Result.fail(
                "It's still an egg. Eggs are beyond flattery.",
                code=error_codes.PET_EGG,
            )
        species = get_species(pet.species)
        duration = SALT_LICK_DURATION_SECONDS * species.salt_lick_pct // 100
        cost = food_cost(pet.species, SALT_LICK.item_id)
        try:
            fresh = self.pet_repo.use_salt_lick(
                pet.pet_id,
                discord_id,
                pet.guild_id,
                cost=cost,
                now=now,
                pampered_until=now + duration,
                week_key=_week_key_for_timestamp(now),
            )
        except ValueError as exc:
            return self._map_repo_error(exc, fee=cost)
        return Result.ok(
            {
                "item_id": SALT_LICK.item_id,
                "qty": 1,
                "total_cost": cost,
                "pampered_until": fresh.pampered_until,
                "pet": fresh,
            }
        )

    # --- trinket gacha ---

    def roll_trinket(self, discord_id: int, guild_id: int | None) -> Result[TrinketOutcome]:
        """Buy a Mystery Trinket: a weighted accessory roll for the living,
        hatched pet. Duplicates dissolve for a partial refund (net sink)."""
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail(
                "Trinkets are gifts — you need a living pet first.",
                code=error_codes.NO_PET,
            )
        if now < pet.hatched_at:
            return Result.fail(
                "It's still an egg. Eggs have no fashion sense.",
                code=error_codes.PET_EGG,
            )
        ids = list(ACCESSORIES)
        weights = [ACCESSORIES[a].weight for a in ids]
        accessory_id = random.choices(ids, weights=weights, k=1)[0]
        try:
            outcome = self.pet_repo.buy_trinket_atomic(
                pet.pet_id,
                discord_id,
                guild_id,
                accessory_id=accessory_id,
                cost=TRINKET_COST,
                refund=TRINKET_DUPE_REFUND,
                now=now,
            )
        except ValueError as exc:
            return self._map_repo_error(exc, fee=TRINKET_COST)
        return Result.ok(
            TrinketOutcome(
                accessory_id=accessory_id,
                duplicate=outcome["duplicate"],
                net_cost=outcome["net_cost"],
                owned_count=outcome["owned_count"],
            )
        )

    def wear_trinket(
        self, discord_id: int, guild_id: int | None, accessory_id: str
    ) -> Result[str]:
        if accessory_id not in ACCESSORIES:
            return Result.fail("Unknown trinket.", code=error_codes.VALIDATION_ERROR)
        now = self._now()
        pet = self._living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail("You have no living pet.", code=error_codes.NO_PET)
        try:
            self.pet_repo.equip_accessory(
                pet.pet_id, discord_id, guild_id, accessory_id
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        return Result.ok(accessory_id)

    def owned_trinkets(self, discord_id: int, guild_id: int | None) -> list[str]:
        return self.pet_repo.get_accessories(discord_id, guild_id)

    # --- camadex ---

    def camadex(self, discord_id: int, guild_id: int | None) -> tuple[list[str], int]:
        """(species ids hatched by this player, roster size)."""
        raised = self.pet_repo.species_raised(discord_id, guild_id, self._now())
        return raised, len(SPECIES)

    # --- feeding ---

    def feed(
        self, discord_id: int, guild_id: int | None, item_id: str
    ) -> Result[FeedOutcome]:
        if item_id not in FOOD_ITEMS:
            return Result.fail("Unknown food.", code=error_codes.VALIDATION_ERROR)
        item = FOOD_ITEMS[item_id]
        last_error: Result[FeedOutcome] | None = None
        for _ in range(2):  # one retry on a concurrent anchor change
            now = self._now()
            pet = self._living_pet(discord_id, guild_id, now)
            if pet is None:
                return Result.fail(
                    "You have no living pet.", code=error_codes.NO_PET
                )
            if now < pet.hatched_at:
                return Result.fail(
                    "It's still an egg. It absorbs patience, not tangos.",
                    code=error_codes.PET_EGG,
                )
            current = pet.current_hunger(now, self.decay_per_day)
            if current >= MAX_HUNGER:
                return Result.fail(
                    f"{pet.name} is completely full.", code=error_codes.PET_FULL
                )
            species = get_species(pet.species)
            spat = (
                species.spit_one_in > 0
                and random.randint(1, species.spit_one_in) == 1
            )
            new_hunger = (
                pet.hunger_at_last_fed
                if spat
                else pet.restored_hunger(now, self.decay_per_day, item.restore)
            )
            try:
                fresh = self.pet_repo.feed_pet(
                    pet.pet_id,
                    discord_id,
                    pet.guild_id,
                    item_id=item_id,
                    expected_last_fed_at=pet.last_fed_at,
                    expected_hunger=pet.hunger_at_last_fed,
                    new_last_fed_at=now,
                    new_hunger=new_hunger,
                    feed_date=game_date_for_timestamp(now),
                    feed_cap=FEED_CAP_PER_DAY,
                    week_key=_week_key_for_timestamp(now),
                    consumed_jc=food_cost(pet.species, item_id),
                    spat=spat,
                )
            except ValueError as exc:
                if str(exc) == "stale_pet":
                    last_error = Result.fail(
                        "Your pet was busy — try again.",
                        code=error_codes.VALIDATION_ERROR,
                    )
                    continue
                return self._map_repo_error(exc)
            supplies = self.pet_repo.get_supplies(discord_id, guild_id)
            return Result.ok(
                FeedOutcome(
                    pet=fresh,
                    item_id=item_id,
                    old_hunger=current,
                    new_hunger=fresh.current_hunger(now, self.decay_per_day),
                    remaining_qty=supplies.get(item_id, 0),
                    feeds_left_today=max(0, FEED_CAP_PER_DAY - fresh.feeds_today),
                    spat=spat,
                )
            )
        return last_error

    # --- match hook ---

    def on_match_win(
        self, winner_ids: list[int], guild_id: int | None
    ) -> list[tuple[Pet, int]]:
        """Feed each winner's living hatched pet. Returns (pet, amount) pairs
        for embed flavor. Never raises — match recording must not fail on pets."""
        results: list[tuple[Pet, int]] = []
        try:
            now = self._now()
            pets = self.pet_repo.get_active_pets_for(winner_ids, guild_id)
            for pet in pets:
                living = self._resolve_starvation(pet, now)
                if living is None or now < living.hatched_at:
                    continue
                amount = MATCH_WIN_HUNGER + get_species(living.species).match_feed_bonus
                current = living.current_hunger(now, self.decay_per_day)
                new_hunger = min(MAX_HUNGER, current + amount)
                applied = self.pet_repo.apply_hunger_top_up(
                    living.pet_id,
                    living.guild_id,
                    expected_last_fed_at=living.last_fed_at,
                    expected_hunger=living.hunger_at_last_fed,
                    new_last_fed_at=now,
                    new_hunger=new_hunger,
                )
                if applied:
                    # Return the post-top-up anchors so callers (warning
                    # re-arm) see the pet's real state.
                    results.append(
                        (
                            dataclasses.replace(
                                living,
                                last_fed_at=now,
                                hunger_at_last_fed=new_hunger,
                            ),
                            amount,
                        )
                    )
        except Exception:
            logger.exception("Pet match-win hook failed; match flow unaffected")
        return results

    # --- lists ---

    def get_graveyard(
        self, discord_id: int, guild_id: int | None, limit: int = 10
    ) -> Result[list[Pet]]:
        return Result.ok(self.pet_repo.get_graveyard(discord_id, guild_id, limit))

    def get_leaderboard(self, guild_id: int | None, limit: int = 10) -> Result[list[Pet]]:
        now = self._now()
        pets = self.pet_repo.get_oldest_living(guild_id, limit * 2)
        living: list[Pet] = []
        for pet in pets:
            resolved = self._resolve_starvation(pet, now)
            if resolved is not None and now >= resolved.hatched_at:
                living.append(resolved)
            if len(living) >= limit:
                break
        return Result.ok(living)

    # --- background sweep ---

    def sweep(self) -> dict:
        """One global pass: starvation, hatch reveals, death notices, refunds.

        Returns {"deaths": [DeathNotice], "hatches": [HatchNotice],
        "refunds": [RefundNotice]}. Delivery bookkeeping (mark_* methods) is
        the caller's job so failed Discord sends retry next tick.
        """
        now = self._now()
        candidates = self.pet_repo.find_starvation_candidates(
            now, decay_per_day=self.decay_per_day, max_decay_pct=_MAX_DECAY_PCT
        )
        for pet in candidates:
            self._resolve_starvation(pet, now)

        deaths = [DeathNotice(pet=p) for p in self.pet_repo.get_unannounced_deaths()]
        hatches = [
            HatchNotice(pet=p) for p in self.pet_repo.find_unannounced_hatches(now)
        ]
        refunds = self._sweep_refunds(now)
        return {"deaths": deaths, "hatches": hatches, "refunds": refunds}

    def mark_death_announced(self, pet: Pet) -> None:
        self.pet_repo.mark_death_announced(pet.pet_id, pet.guild_id, self._now())

    def mark_hatch_announced(self, pet: Pet) -> None:
        self.pet_repo.mark_hatch_announced(pet.pet_id, pet.guild_id, self._now())

    def mark_refund_announced(self, notice: RefundNotice) -> None:
        self.pet_repo.mark_refund_announced(
            notice.guild_id, notice.week_key, self._now()
        )

    def _sweep_refunds(self, now: int) -> list[RefundNotice]:
        # Look back two weeks, oldest first: the two-slot care accumulator can
        # still substantiate both, so an outage spanning a week boundary does
        # not silently forfeit the older window.
        week_keys = [
            _week_key_for_timestamp(now - 14 * 86400),
            _week_key_for_timestamp(now - 7 * 86400),
        ]
        for guild_id in self.pet_repo.list_pet_guild_ids():
            for week_key in week_keys:
                try:
                    self._pay_guild_refunds(guild_id, week_key, now)
                except ValueError as exc:
                    if str(exc) == "fund_short":
                        logger.warning(
                            "Pet refund fund_short for guild %s %s; retrying next tick",
                            guild_id, week_key,
                        )
                        continue
                    raise
        return self.pet_repo.get_unannounced_refunds()

    def _pay_guild_refunds(
        self, guild_id: int, week_key: str, now: int
    ) -> RefundNotice | None:
        if self.pet_repo.is_refund_window_paid(guild_id, week_key):
            return None
        care = self.pet_repo.list_week_care(guild_id, week_key)
        payouts: list[RefundPayout] = []
        for pet in care:
            consumed = pet.consumed_in_week(week_key)
            if consumed <= 0:
                continue
            mult = (
                random.randint(REFUND_MULT_MIN, REFUND_MULT_MAX)
                + get_species(pet.species).refund_bonus_pp
            )
            payouts.append(
                RefundPayout(
                    discord_id=pet.discord_id,
                    consumed_jc=consumed,
                    multiplier_pct=mult,
                    amount=consumed * mult // 100,
                )
            )
        total = sum(p.amount for p in payouts)
        fund = self.pet_repo.get_nonprofit_balance(guild_id)
        scaled = total > fund
        if scaled and total > 0:
            payouts = [
                RefundPayout(
                    discord_id=p.discord_id,
                    consumed_jc=p.consumed_jc,
                    multiplier_pct=p.multiplier_pct,
                    amount=p.amount * fund // total,
                )
                for p in payouts
            ]
            payouts = [p for p in payouts if p.amount > 0]
            if not payouts:
                # Fund is empty: leave the window UNCLAIMED so a later top-up
                # can still pay this week (mirrors the fund_short retry path).
                logger.warning(
                    "Pet refund fund empty for guild %s %s; retrying next tick",
                    guild_id, week_key,
                )
                return None
        notice = RefundNotice(
            guild_id=guild_id,
            week_key=week_key,
            payouts=tuple(payouts),
            total_paid=sum(p.amount for p in payouts),
            scaled_down=scaled,
        )
        paid = self.pet_repo.pay_refunds_atomic(
            guild_id,
            week_key,
            [(p.discord_id, p.amount) for p in payouts],
            now,
            announcement=notice if payouts else None,
        )
        if not paid or not payouts:
            return None
        return notice

    # --- internals ---

    def _validate_name(self, name: str) -> Result | None:
        stripped = (name or "").strip()
        if not 1 <= len(stripped) <= PET_NAME_MAX_LEN:
            return Result.fail(
                f"Name must be 1-{PET_NAME_MAX_LEN} characters.",
                code=error_codes.VALIDATION_ERROR,
            )
        if not stripped.isprintable():
            return Result.fail(
                "Name contains unprintable characters.",
                code=error_codes.VALIDATION_ERROR,
            )
        lowered = stripped.lower()
        if any(fragment in lowered for fragment in _FORBIDDEN_NAME_FRAGMENTS):
            return Result.fail(
                "No mentions or links in pet names.",
                code=error_codes.VALIDATION_ERROR,
            )
        return None

    @staticmethod
    def _map_repo_error(exc: ValueError, *, fee: int | None = None) -> Result:
        code = str(exc)
        messages = {
            "insufficient_funds": (
                f"Not enough jopacoin{f' (need {fee})' if fee else ''}.",
                error_codes.INSUFFICIENT_FUNDS,
            ),
            "already_has_pet": (
                "You already have a pet. One cama per household.",
                error_codes.ALREADY_HAS_PET,
            ),
            "no_pet": ("You have no living pet.", error_codes.NO_PET),
            "pet_dead": ("Your pet has passed away.", error_codes.PET_DEAD),
            "no_supplies": (
                "You're out of that food. Visit /pet shop.",
                error_codes.NO_SUPPLIES,
            ),
            "feed_cap": (
                "Your pet has eaten enough today (4 feeds per day).",
                error_codes.FEED_CAP,
            ),
            "already_pampered": (
                "Your pet is already thoroughly pampered.",
                error_codes.ALREADY_PAMPERED,
            ),
            "stack_cap": (
                "You can't hoard more of that item.",
                error_codes.STACK_CAP,
            ),
            "not_owned": (
                "You don't own that trinket (roll one with `/pet trinket`).",
                error_codes.VALIDATION_ERROR,
            ),
        }
        message, mapped = messages.get(code, (f"Pet error: {code}", None))
        return Result.fail(message, code=mapped or error_codes.VALIDATION_ERROR)
