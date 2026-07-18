"""Wheel outcome dispatch and mutable result state for ``/gamba``."""

from __future__ import annotations

import asyncio
import functools
import logging
import random
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any

import discord

from commands.betting_helpers.economy_actions import apply_gamba_event_multiplier
from config import (
    HOSTILE_LOSS_MIN_BALANCE,
    LIGHTNING_BOLT_MIN_TAX,
    LIGHTNING_BOLT_PCT_MAX,
    LIGHTNING_BOLT_PCT_MIN,
    WHEEL_BANANA_PEEL_LOSS_MAX,
    WHEEL_BANANA_PEEL_LOSS_MIN,
    WHEEL_BOMB_OMB_VICTIM_COUNT,
    WHEEL_BOMB_OMB_VICTIM_LOSS_MAX,
    WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
    WHEEL_GOLDEN_RECESSION_MID_PCT,
    WHEEL_GOLDEN_RECESSION_MID_RANK_END,
    WHEEL_GOLDEN_RECESSION_REST_PCT,
    WHEEL_GOLDEN_RECESSION_TOP_PCT,
    WHEEL_GOLDEN_TOP_N,
    WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MAX,
    WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MIN,
    WHEEL_GREEN_SHELL_STEAL_MAX,
    WHEEL_GREEN_SHELL_STEAL_MIN,
)
from services.dig_data.balance import scale_positive_dig_jc
from utils.economy_scaling import scale_minigame_jc_delta

logger = logging.getLogger("cama_bot.commands.betting.wheel_outcomes")

# SQLite accepts a signed 32-bit LIMIT; this mirrors the unified leaderboard's
# full candidate fetch before Discord membership filtering.
ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT = 2**31 - 1


def has_guild_member_snapshot(guild: discord.Guild | None) -> bool:
    """Return whether a concrete guild member cache is available."""
    return guild is not None and isinstance(guild.members, Sequence)


def filter_visible_leaderboard(
    players: list[Any],
    guild: discord.Guild | None,
    *,
    limit: int,
) -> list[Any]:
    """Return leaderboard rows visible in the current Discord guild.

    The balance leaderboard omits registered players who are no longer guild
    members. Rank-based wheel mechanics must apply the same rule or Golden
    Wheel eligibility and victims can disagree with ``/leaderboard``.

    When guild membership cannot be verified (for example, lightweight tests),
    preserve the repository ordering.
    """
    if has_guild_member_snapshot(guild):
        member_ids = {member.id for member in guild.members}
        players = [player for player in players if player.discord_id in member_ids]
    return players[:limit]


def eruption_reward(last_spin: dict | None) -> int:
    """Return Eruption's reward without scaling an already-settled spin twice."""
    if last_spin and isinstance(last_spin.get("result"), int):
        copied_amount = abs(last_spin["result"])
        if copied_amount > 0:
            return copied_amount * 2
    return scale_minigame_jc_delta(50)


@dataclass
class WheelOutcomeState:
    """Standardized output shared by outcome handlers and result rendering."""

    result_wedge: tuple[str, int | str, str]
    new_balance: int
    result_value: int | str = field(init=False)
    garnished_amount: int = 0

    shell_victim: discord.Member | None = None
    shell_victim_new_balance: int | None = None
    shell_amount: int = 0
    shell_self_hit: bool = False
    shell_missed: bool = False

    lightning_total: int = 0
    lightning_count: int = 0
    lightning_victims: list[tuple[str, int, int]] = field(default_factory=list)

    jailbreak_new_total: int = 0
    chain_value: int | None = None
    chain_username: str = "someone"
    emergency_count: int = 0
    emergency_total: int = 0
    commune_total: int = 0
    commune_count: int = 0
    pardon_consumed: bool = False

    heist_total: int = 0
    heist_count: int = 0
    market_crash_total: int = 0
    market_crash_count: int = 0
    compound_amount: int = 0
    trickle_total: int = 0
    trickle_count: int = 0
    dividend_amount: int = 0
    takeover_amount: int = 0
    takeover_victim_name: str = "rank #4"
    takeover_missed: bool = False
    recession_total: int = 0
    recession_count: int = 0
    recession_self_loss: int = 0

    banana_victim: discord.Member | None = None
    banana_victim_name: str = "the player behind you"
    banana_victim_loss: int = 0
    banana_missed: bool = False
    green_shell_victim: discord.Member | None = None
    green_shell_victim_name: str = "someone"
    green_shell_amount: int = 0
    green_shell_missed: bool = False
    bomb_omb_victims: list[tuple[str, int, int]] = field(default_factory=list)
    bomb_omb_burn_total: int = 0
    bomb_omb_missed: bool = False

    shield_absorbed_total: int = 0
    shielded_count: int = 0
    extend_games_added: int = 0
    extend_new_total: int = 0

    def __post_init__(self) -> None:
        self.result_value = self.result_wedge[1]

    def replace_result(self, result_wedge: tuple[str, int | str, str]) -> None:
        self.result_wedge = result_wedge
        self.result_value = result_wedge[1]

    def record_shield(self, settled) -> None:
        if settled.absorbed > 0:
            self.shield_absorbed_total += settled.absorbed
            self.shielded_count += 1

    def log_result(self) -> int:
        """Return the numeric history value for the resolved outcome."""
        value = self.result_value
        if value == "RED_SHELL":
            return self.shell_amount if not self.shell_missed else 0
        if value == "BLUE_SHELL":
            if self.shell_missed:
                return 0
            return -self.shell_amount if self.shell_self_hit else self.shell_amount
        if value in {
            "LIGHTNING_BOLT",
            "EXTEND_1",
            "EXTEND_2",
            "JAILBREAK",
            "CHAIN_REACTION",
            "TOWN_TRIAL",
            "DISCOVER",
            "EMERGENCY",
            "COMEBACK",
        }:
            return 0
        totals = {
            "COMMUNE": self.commune_total,
            "HEIST": self.heist_total,
            "MARKET_CRASH": self.market_crash_total,
            "COMPOUND_INTEREST": self.compound_amount,
            "TRICKLE_DOWN": self.trickle_total,
            "DIVIDEND": self.dividend_amount,
            "HOSTILE_TAKEOVER": 0 if self.takeover_missed else self.takeover_amount,
            "RECESSION": -self.recession_self_loss,
            "BANANA_PEEL": 0,
            "GREEN_SHELL": self.green_shell_amount,
            "BOMB_OMB": 0,
        }
        if isinstance(value, str):
            return totals.get(value, 0)
        return value

    def embed_kwargs(self) -> dict[str, Any]:
        """Return the outcome-specific arguments consumed by the embed builder."""
        return {
            "shell_victim": self.shell_victim,
            "shell_victim_new_balance": self.shell_victim_new_balance,
            "shell_amount": self.shell_amount,
            "shell_self_hit": self.shell_self_hit,
            "shell_missed": self.shell_missed,
            "lightning_total": self.lightning_total,
            "lightning_count": self.lightning_count,
            "lightning_victims": self.lightning_victims or None,
            "extend_games_added": self.extend_games_added,
            "extend_new_total": self.extend_new_total,
            "jailbreak_new_total": self.jailbreak_new_total,
            "chain_value": self.chain_value,
            "chain_username": self.chain_username,
            "emergency_count": self.emergency_count,
            "emergency_total": self.emergency_total,
            "commune_total": self.commune_total,
            "commune_count": self.commune_count,
            "pardon_consumed": self.pardon_consumed,
            "heist_total": self.heist_total,
            "heist_count": self.heist_count,
            "market_crash_total": self.market_crash_total,
            "market_crash_count": self.market_crash_count,
            "compound_amount": self.compound_amount,
            "trickle_total": self.trickle_total,
            "trickle_count": self.trickle_count,
            "dividend_amount": self.dividend_amount,
            "takeover_amount": self.takeover_amount,
            "takeover_victim_name": self.takeover_victim_name,
            "takeover_missed": self.takeover_missed,
            "recession_total": self.recession_total,
            "recession_count": self.recession_count,
            "recession_self_loss": self.recession_self_loss,
            "banana_victim": self.banana_victim,
            "banana_victim_name": self.banana_victim_name,
            "banana_victim_loss": self.banana_victim_loss,
            "banana_missed": self.banana_missed,
            "green_shell_victim": self.green_shell_victim,
            "green_shell_victim_name": self.green_shell_victim_name,
            "green_shell_amount": self.green_shell_amount,
            "green_shell_missed": self.green_shell_missed,
            "bomb_omb_victims": self.bomb_omb_victims,
            "bomb_omb_burn_total": self.bomb_omb_burn_total,
            "bomb_omb_missed": self.bomb_omb_missed,
            "shield_absorbed_total": self.shield_absorbed_total,
            "shielded_count": self.shielded_count,
        }


@dataclass(frozen=True)
class WheelOutcomeContext:
    command: Any
    interaction: discord.Interaction
    user_id: int
    guild_id: int
    bankruptcy_service: Any
    penalty_games_remaining: int
    effects: Any
    mana_effects_service: Any
    is_bad_gamba: bool
    hostile_event_prefix: str
    gamba_win_multiplier: float = 1.0
    gamba_loss_multiplier: float = 1.0
    is_dig_bonus: bool = False


class WheelOutcomeProcessor:
    """Dispatch a resolved wedge to one focused handler."""

    HANDLERS = {
        "JAILBREAK": "_jailbreak",
        "CHAIN_REACTION": "_chain_reaction",
        "EMERGENCY": "_emergency",
        "COMMUNE": "_commune",
        "COMEBACK": "_comeback",
        "ERUPTION": "_eruption",
        "OVERGROWTH": "_overgrowth",
        "DECAY": "_decay",
        "RED_SHELL": "_red_shell",
        "BLUE_SHELL": "_blue_shell",
        "LIGHTNING_BOLT": "_lightning_bolt",
        "BANANA_PEEL": "_banana_peel",
        "GREEN_SHELL": "_green_shell",
        "BOMB_OMB": "_bomb_omb",
        "HEIST": "_heist",
        "MARKET_CRASH": "_market_crash",
        "COMPOUND_INTEREST": "_compound_interest",
        "TRICKLE_DOWN": "_trickle_down",
        "DIVIDEND": "_dividend",
        "HOSTILE_TAKEOVER": "_hostile_takeover",
        "RECESSION": "_recession",
        "EXTEND_1": "_extend_penalty",
        "EXTEND_2": "_extend_penalty",
    }

    def __init__(self, context: WheelOutcomeContext, state: WheelOutcomeState):
        self.context = context
        self.state = state
        self.command = context.command
        self.player_service = self.command.player_service

    async def process(self) -> WheelOutcomeState:
        handler_name = self.HANDLERS.get(self.state.result_value)
        if handler_name:
            await getattr(self, handler_name)()
        elif isinstance(self.state.result_value, int):
            await self._numeric_result()
        return self.state

    def _minted_reward(self, amount: int) -> int:
        """Apply the daily event only to new JC created by this wheel result.

        Callers must pass an amount after the normal minigame scaling. Player
        transfers, Reserve transfers, refunds, and numeric wedges deliberately
        bypass this helper so their already-settled value is not scaled twice.
        """
        if amount <= 0:
            return amount
        reward = apply_gamba_event_multiplier(
            amount,
            win_multiplier=self.context.gamba_win_multiplier,
            loss_multiplier=self.context.gamba_loss_multiplier,
        )
        if self.context.is_dig_bonus:
            return scale_positive_dig_jc(reward)
        return reward

    async def _jailbreak(self) -> None:
        if self.context.bankruptcy_service:
            self.state.jailbreak_new_total = await asyncio.to_thread(
                self.context.bankruptcy_service.add_penalty_games,
                self.context.user_id,
                self.context.guild_id,
                -1,
            )

    async def _chain_reaction(self) -> None:
        last_spin = await asyncio.to_thread(
            self.player_service.get_last_normal_wheel_spin,
            self.context.guild_id,
        )
        if not last_spin:
            return
        self.state.chain_value = last_spin["result"]
        chained_uid = last_spin["discord_id"]
        guild = self.context.interaction.guild
        if guild:
            member = guild.get_member(chained_uid)
            self.state.chain_username = member.display_name if member else f"<@{chained_uid}>"
        else:
            self.state.chain_username = f"<@{chained_uid}>"

        if isinstance(self.state.chain_value, int) and self.state.chain_value > 0:
            # Copying a prior result creates a fresh reward. The prior spin's
            # debit/credit stays untouched; only this newly minted copy is scaled.
            self.state.chain_value = self._minted_reward(self.state.chain_value)
            self.state.new_balance, self.state.garnished_amount = (
                await self.command._credit_gamba_outcome(
                    self.context.user_id,
                    self.context.guild_id,
                    self.state.new_balance,
                    self.state.chain_value,
                    "CHAIN_REACTION",
                    "gamba chain reaction credit",
                    {"copied_spin_user_id": chained_uid},
                )
            )
        elif isinstance(self.state.chain_value, int) and self.state.chain_value < 0:
            await asyncio.to_thread(
                self.command._adjust_gamba_balance,
                self.context.user_id,
                self.context.user_id,
                self.context.guild_id,
                self.state.chain_value,
                "gamba chain reaction debit",
                "CHAIN_REACTION",
                {"copied_spin_user_id": chained_uid},
            )
            await self._refresh_balance()

    async def _emergency(self) -> None:
        players = await self._leaderboard(limit=9999)
        for player in players:
            if player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
                continue
            loss = min(player.jopacoin_balance, scale_minigame_jc_delta(20))
            settled = await self._hostile_loss(
                player,
                loss,
                "EMERGENCY",
                clamp_to_balance=True,
            )
            self.state.emergency_total += settled.applied
            self.state.emergency_count += int(settled.applied > 0)
            self.state.record_shield(settled)
        await self._refresh_balance()

    async def _commune(self) -> None:
        centralized = False
        for player in await self._leaderboard(limit=9999):
            if (
                player.discord_id == self.context.user_id
                or player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE
            ):
                continue
            settled = await self._hostile_loss(
                player,
                1,
                "COMMUNE",
                destination="player",
                recipient_id=self.context.user_id,
                clamp_to_balance=True,
                legacy_aggregate_transfer=True,
            )
            centralized = centralized or settled.centralized
            self.state.commune_total += settled.applied
            self.state.commune_count += int(settled.applied > 0)
            self.state.record_shield(settled)

        if self.state.commune_total <= 0:
            return
        if centralized:
            await self._refresh_balance()
        else:
            self.state.new_balance, self.state.garnished_amount = (
                await self.command._credit_gamba_outcome(
                    self.context.user_id,
                    self.context.guild_id,
                    self.state.new_balance,
                    self.state.commune_total,
                    "COMMUNE",
                    "gamba commune collected donations",
                )
            )

    async def _comeback(self) -> None:
        await asyncio.to_thread(
            self.player_service.set_wheel_pardon,
            self.context.user_id,
            self.context.guild_id,
            1,
        )

    async def _eruption(self) -> None:
        last_spin = await asyncio.to_thread(
            self.player_service.get_last_normal_wheel_spin,
            self.context.guild_id,
        )
        amount = self._minted_reward(eruption_reward(last_spin))
        self.state.new_balance, self.state.garnished_amount = (
            await self.command._credit_gamba_outcome(
                self.context.user_id,
                self.context.guild_id,
                self.state.new_balance,
                amount,
                "ERUPTION",
                "gamba eruption credit",
                {"last_spin": last_spin},
            )
        )

    async def _overgrowth(self) -> None:
        player = await asyncio.to_thread(
            self.player_service.get_player,
            self.context.user_id,
            self.context.guild_id,
        )
        games_this_week = 0
        if player:
            import time

            week_ago = int(time.time()) - 7 * 24 * 3600
            try:
                recent = await asyncio.to_thread(
                    functools.partial(
                        self.player_service.get_recent_matches,
                        self.context.user_id,
                        self.context.guild_id,
                        since=week_ago,
                    )
                )
                games_this_week = len(recent) if recent else 0
            except Exception:
                games_this_week = max(1, (player.wins + player.losses) // 10)
        amount = max(10, games_this_week * 10)
        if self.context.effects and self.context.mana_effects_service:
            amount = await asyncio.to_thread(
                self.context.mana_effects_service.apply_green_cap,
                self.context.effects,
                amount,
            )
        amount = scale_minigame_jc_delta(amount)
        amount = self._minted_reward(amount)
        self.state.new_balance, self.state.garnished_amount = (
            await self.command._credit_gamba_outcome(
                self.context.user_id,
                self.context.guild_id,
                self.state.new_balance,
                amount,
                "OVERGROWTH",
                "gamba overgrowth credit",
                {"games_this_week": games_this_week},
            )
        )

    async def _decay(self) -> None:
        total = 0
        centralized = False
        for index, player in enumerate(await self._leaderboard(limit=4)):
            if (
                player.discord_id == self.context.user_id
                or player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE
            ):
                continue
            loss = min(
                scale_minigame_jc_delta(80 if index == 3 else 60),
                max(0, player.jopacoin_balance),
            )
            if loss <= 0:
                continue
            settled = await self._hostile_loss(
                player,
                loss,
                "DECAY",
                destination="player",
                recipient_id=self.context.user_id,
                clamp_to_balance=True,
                legacy_aggregate_transfer=True,
            )
            centralized = centralized or settled.centralized
            total += settled.applied
            self.state.record_shield(settled)
        if total <= 0:
            return
        if centralized:
            await self._refresh_balance()
        else:
            self.state.new_balance, self.state.garnished_amount = (
                await self.command._credit_gamba_outcome(
                    self.context.user_id,
                    self.context.guild_id,
                    self.state.new_balance,
                    total,
                    "DECAY",
                    "gamba decay collected taxes",
                )
            )

    async def _red_shell(self) -> None:
        player_above = await asyncio.to_thread(
            self.player_service.get_player_above,
            self.context.user_id,
            self.context.guild_id,
            min_balance=HOSTILE_LOSS_MIN_BALANCE,
        )
        if not player_above or player_above.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
            self.state.shell_missed = True
            return

        percentage = max(1, int(player_above.jopacoin_balance * random.uniform(0.02, 0.07)))
        flat = random.randint(2, 10)
        requested = scale_minigame_jc_delta(max(percentage, flat))
        settled = await self._hostile_loss(
            player_above,
            requested,
            "RED_SHELL",
            destination="player",
            recipient_id=self.context.user_id,
        )
        self.state.shell_amount = settled.applied
        self.state.shell_victim_new_balance = settled.victim_balance_after
        if settled.destination_balance_after is not None:
            self.state.new_balance = settled.destination_balance_after
        self.state.record_shield(settled)
        if self.context.interaction.guild:
            self.state.shell_victim = self.context.interaction.guild.get_member(
                player_above.discord_id
            )

    async def _blue_shell(self) -> None:
        leaderboard = await self._leaderboard(limit=1)
        if leaderboard and leaderboard[0].discord_id == self.context.user_id:
            self.state.shell_self_hit = True
            percentage = max(1, int(self.state.new_balance * random.uniform(0.02, 0.07)))
            flat = random.randint(4, 20)
            requested = scale_minigame_jc_delta(max(percentage, flat))
            settled = await self.command._apply_hostile_gamba_loss(
                victim_id=self.context.user_id,
                guild_id=self.context.guild_id,
                amount=requested,
                actor_id=self.context.user_id,
                event_key=f"{self.context.hostile_event_prefix}:{self.context.user_id}",
                outcome="BLUE_SHELL",
                destination="reserve",
                victim_balance=self.state.new_balance,
            )
            self.state.shell_amount = settled.applied
            self.state.new_balance = settled.victim_balance_after
            return

        if not leaderboard or leaderboard[0].jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
            self.state.shell_missed = True
            return

        richest = leaderboard[0]
        percentage = max(1, int(richest.jopacoin_balance * random.uniform(0.02, 0.07)))
        flat = random.randint(4, 20)
        requested = scale_minigame_jc_delta(max(percentage, flat))
        settled = await self._hostile_loss(
            richest,
            requested,
            "BLUE_SHELL",
            destination="player",
            recipient_id=self.context.user_id,
        )
        self.state.shell_amount = settled.applied
        self.state.shell_victim_new_balance = settled.victim_balance_after
        if settled.destination_balance_after is not None:
            self.state.new_balance = settled.destination_balance_after
        self.state.record_shield(settled)
        if self.context.interaction.guild:
            self.state.shell_victim = self.context.interaction.guild.get_member(
                richest.discord_id
            )

    async def _lightning_bolt(self) -> None:
        tax_rate = random.uniform(LIGHTNING_BOLT_PCT_MIN, LIGHTNING_BOLT_PCT_MAX)
        centralized = False
        for player in await self._leaderboard(limit=9999):
            if player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
                continue
            tax = scale_minigame_jc_delta(
                max(LIGHTNING_BOLT_MIN_TAX, int(player.jopacoin_balance * tax_rate))
            )
            settled = await self._hostile_loss(
                player,
                tax,
                "LIGHTNING_BOLT",
                destination="reserve",
                clamp_to_balance=True,
                legacy_aggregate_transfer=True,
                metadata={"tax_pct": tax_rate},
            )
            centralized = centralized or settled.centralized
            self.state.lightning_total += settled.applied
            self.state.lightning_count += int(settled.applied > 0)
            if settled.applied > 0:
                self.state.lightning_victims.append(
                    (player.name, settled.applied, player.discord_id)
                )
            self.state.record_shield(settled)

        loan_service = self.command.loan_service
        if loan_service and self.state.lightning_total > 0 and not centralized:
            try:
                await asyncio.to_thread(
                    loan_service.add_to_nonprofit_fund,
                    self.context.guild_id,
                    self.state.lightning_total,
                    source="gamba",
                    actor_id=self.context.user_id,
                    related_type="wheel_spin",
                    related_id="LIGHTNING_BOLT",
                    reason="gamba lightning bolt reserve credit",
                    metadata={
                        "victim_count": self.state.lightning_count,
                        "tax_pct": tax_rate,
                        "total": self.state.lightning_total,
                    },
                )
            except Exception:
                logger.warning("Failed to add lightning bolt tax to nonprofit fund")
        self.state.lightning_victims.sort(key=lambda row: row[1], reverse=True)
        await self._refresh_balance()

    async def _banana_peel(self) -> None:
        victim = await asyncio.to_thread(
            self.player_service.get_player_below,
            self.context.user_id,
            self.context.guild_id,
        )
        if not victim or victim.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
            self.state.banana_missed = True
            return
        requested = min(
            scale_minigame_jc_delta(
                random.randint(WHEEL_BANANA_PEEL_LOSS_MIN, WHEEL_BANANA_PEEL_LOSS_MAX)
            ),
            victim.jopacoin_balance,
        )
        try:
            settled = await self._hostile_loss(
                victim,
                requested,
                "BANANA_PEEL",
                clamp_to_balance=True,
            )
        except Exception as exc:
            logger.warning(
                "BANANA_PEEL: failed to burn from victim %s in guild %s: %s",
                victim.discord_id,
                self.context.guild_id,
                exc,
            )
            self.state.banana_missed = True
            return

        self.state.banana_victim_loss = settled.applied
        self.state.record_shield(settled)
        self.state.banana_victim_name = victim.name
        if self.context.interaction.guild:
            self.state.banana_victim = self.context.interaction.guild.get_member(
                victim.discord_id
            )
        logger.info(
            "BANANA_PEEL: spinner=%s burned %s JC from victim=%s in guild=%s",
            self.context.user_id,
            self.state.banana_victim_loss,
            victim.discord_id,
            self.context.guild_id,
        )

    async def _green_shell(self) -> None:
        eligible = [
            player
            for player in await self._leaderboard(limit=9999)
            if player.discord_id != self.context.user_id
            and player.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
        ]
        if not eligible:
            self.state.green_shell_missed = True
            return
        victim = random.choice(eligible)
        requested = min(
            scale_minigame_jc_delta(
                random.randint(WHEEL_GREEN_SHELL_STEAL_MIN, WHEEL_GREEN_SHELL_STEAL_MAX)
            ),
            victim.jopacoin_balance,
        )
        if requested <= 0:
            self.state.green_shell_missed = True
            return
        try:
            settled = await self._hostile_loss(
                victim,
                requested,
                "GREEN_SHELL",
                destination="player",
                recipient_id=self.context.user_id,
            )
        except Exception as exc:
            logger.warning(
                "GREEN_SHELL: transfer failed (spinner=%s victim=%s guild=%s): %s",
                self.context.user_id,
                victim.discord_id,
                self.context.guild_id,
                exc,
            )
            self.state.green_shell_missed = True
            return

        self.state.green_shell_amount = settled.applied
        if settled.destination_balance_after is not None:
            self.state.new_balance = settled.destination_balance_after
        self.state.record_shield(settled)
        self.state.green_shell_victim_name = victim.name
        if self.context.interaction.guild:
            self.state.green_shell_victim = self.context.interaction.guild.get_member(
                victim.discord_id
            )
        logger.info(
            "GREEN_SHELL: spinner=%s stole %s JC from victim=%s in guild=%s",
            self.context.user_id,
            self.state.green_shell_amount,
            victim.discord_id,
            self.context.guild_id,
        )

    async def _bomb_omb(self) -> None:
        eligible = [
            player
            for player in await self._leaderboard(limit=9999)
            if player.discord_id != self.context.user_id
            and player.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
        ]
        sample_size = min(WHEEL_BOMB_OMB_VICTIM_COUNT, len(eligible))
        if sample_size <= 0:
            self.state.bomb_omb_missed = True
            return
        for victim in random.sample(eligible, sample_size):
            requested = min(
                scale_minigame_jc_delta(
                    random.randint(
                        WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
                        WHEEL_BOMB_OMB_VICTIM_LOSS_MAX,
                    )
                ),
                victim.jopacoin_balance,
            )
            if requested <= 0:
                continue
            try:
                settled = await self._hostile_loss(
                    victim,
                    requested,
                    "BOMB_OMB",
                    clamp_to_balance=True,
                )
            except Exception as exc:
                logger.warning(
                    "BOMB_OMB: failed victim=%s (spinner=%s guild=%s): %s",
                    victim.discord_id,
                    self.context.user_id,
                    self.context.guild_id,
                    exc,
                )
                continue
            if settled.applied > 0:
                self.state.bomb_omb_victims.append(
                    (victim.name, settled.applied, victim.discord_id)
                )
                self.state.bomb_omb_burn_total += settled.applied
            self.state.record_shield(settled)
        self.state.bomb_omb_missed = self.state.bomb_omb_burn_total == 0

    async def _heist(self) -> None:
        if has_guild_member_snapshot(self.context.interaction.guild):
            visible_players = await self._leaderboard(
                limit=ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
            )
            bottom_players = [
                player
                for player in reversed(visible_players)
                if player.jopacoin_balance >= 1
            ][:30]
        else:
            bottom_players = await asyncio.to_thread(
                functools.partial(
                    self.player_service.get_leaderboard_bottom,
                    self.context.guild_id,
                    limit=30,
                    min_balance=1,
                )
            )
        victims = [
            player
            for player in bottom_players
            if player.discord_id != self.context.user_id
            and player.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
        ][:30]
        for victim in victims:
            requested = scale_minigame_jc_delta(
                max(1, int(victim.jopacoin_balance * random.uniform(0.05, 0.12)))
            )
            try:
                settled = await self._hostile_loss(
                    victim,
                    requested,
                    "HEIST",
                    destination="player",
                    recipient_id=self.context.user_id,
                )
                self.state.heist_total += settled.applied
                self.state.heist_count += int(settled.attempted > 0)
                self.state.record_shield(settled)
            except Exception as exc:
                logger.warning(
                    "Failed to execute heist steal from victim %s: %s",
                    victim.discord_id,
                    exc,
                )
        if not victims:
            self.state.heist_total = self._minted_reward(
                scale_minigame_jc_delta(20)
            )
            await self._adjust_spinner(
                self.state.heist_total,
                "gamba heist fallback credit",
                "HEIST",
            )
        await self._refresh_balance()

    async def _market_crash(self) -> None:
        victims = [
            player
            for player in await self._leaderboard(limit=WHEEL_GOLDEN_TOP_N)
            if player.discord_id != self.context.user_id
            and player.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
        ]
        for victim in victims:
            requested = scale_minigame_jc_delta(
                max(1, int(victim.jopacoin_balance * random.uniform(0.08, 0.15)))
            )
            try:
                settled = await self._hostile_loss(
                    victim,
                    requested,
                    "MARKET_CRASH",
                    destination="player",
                    recipient_id=self.context.user_id,
                )
                self.state.market_crash_total += settled.applied
                self.state.market_crash_count += int(settled.attempted > 0)
                self.state.record_shield(settled)
            except Exception as exc:
                logger.warning(
                    "Failed to execute market crash tax on victim %s: %s",
                    victim.discord_id,
                    exc,
                )
        if not victims:
            self.state.market_crash_total = self._minted_reward(
                scale_minigame_jc_delta(25)
            )
            await self._adjust_spinner(
                self.state.market_crash_total,
                "gamba market crash fallback credit",
                "MARKET_CRASH",
            )
        await self._refresh_balance()

    async def _compound_interest(self) -> None:
        self.state.compound_amount = self._minted_reward(
            scale_minigame_jc_delta(100)
        )
        self.state.new_balance, self.state.garnished_amount = (
            await self.command._credit_gamba_outcome(
                self.context.user_id,
                self.context.guild_id,
                self.state.new_balance,
                self.state.compound_amount,
                "COMPOUND_INTEREST",
                "gamba compound interest credit",
            )
        )

    async def _trickle_down(self) -> None:
        tax_rate = random.uniform(
            WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MIN,
            WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MAX,
        )
        centralized = False
        for player in await self._leaderboard(limit=9999):
            if (
                player.discord_id == self.context.user_id
                or player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE
            ):
                continue
            tax = scale_minigame_jc_delta(
                max(1, int(player.jopacoin_balance * tax_rate))
            )
            settled = await self._hostile_loss(
                player,
                tax,
                "TRICKLE_DOWN",
                destination="player",
                recipient_id=self.context.user_id,
                clamp_to_balance=True,
                legacy_aggregate_transfer=True,
                metadata={"tax_pct": tax_rate},
            )
            centralized = centralized or settled.centralized
            self.state.trickle_total += settled.applied
            self.state.trickle_count += int(settled.applied > 0)
            self.state.record_shield(settled)
        if self.state.trickle_total <= 0:
            return
        if centralized:
            await self._refresh_balance()
        else:
            self.state.new_balance, self.state.garnished_amount = (
                await self.command._credit_gamba_outcome(
                    self.context.user_id,
                    self.context.guild_id,
                    self.state.new_balance,
                    self.state.trickle_total,
                    "TRICKLE_DOWN",
                    "gamba trickle down collected taxes",
                )
            )

    async def _dividend(self) -> None:
        if has_guild_member_snapshot(self.context.interaction.guild):
            total_wealth = sum(
                max(0, player.jopacoin_balance)
                for player in await self._leaderboard(
                    limit=ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
                )
            )
        else:
            total_wealth = await asyncio.to_thread(
                self.player_service.get_total_positive_balance,
                self.context.guild_id,
            )
        self.state.dividend_amount = self._minted_reward(
            scale_minigame_jc_delta(max(10, int(total_wealth * 0.005)))
        )
        self.state.new_balance, self.state.garnished_amount = (
            await self.command._credit_gamba_outcome(
                self.context.user_id,
                self.context.guild_id,
                self.state.new_balance,
                self.state.dividend_amount,
                "DIVIDEND",
                "gamba dividend credit",
                {"total_positive_balance": total_wealth},
            )
        )

    async def _hostile_takeover(self) -> None:
        leaderboard = await self._leaderboard(limit=WHEEL_GOLDEN_TOP_N + 1)
        victim = (
            leaderboard[WHEEL_GOLDEN_TOP_N]
            if len(leaderboard) > WHEEL_GOLDEN_TOP_N
            else None
        )
        if victim and victim.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE:
            requested = scale_minigame_jc_delta(
                max(1, int(victim.jopacoin_balance * random.uniform(0.08, 0.15)))
            )
            try:
                settled = await self._hostile_loss(
                    victim,
                    requested,
                    "HOSTILE_TAKEOVER",
                    destination="player",
                    recipient_id=self.context.user_id,
                )
                self.state.takeover_amount = settled.applied
                if settled.destination_balance_after is not None:
                    self.state.new_balance = settled.destination_balance_after
                self.state.record_shield(settled)
                if self.context.interaction.guild:
                    member = self.context.interaction.guild.get_member(victim.discord_id)
                    self.state.takeover_victim_name = (
                        member.mention if member else victim.name
                    )
                return
            except Exception:
                self.state.takeover_missed = True
                self.state.takeover_amount = 0
                return
        else:
            self.state.takeover_missed = True

        self.state.takeover_amount = self._minted_reward(
            scale_minigame_jc_delta(40)
        )
        await self._adjust_spinner(
            self.state.takeover_amount,
            "gamba hostile takeover fallback credit",
            "HOSTILE_TAKEOVER",
        )
        await self._refresh_balance()

    async def _recession(self) -> None:
        spinner_balance_before = self.state.new_balance
        centralized = False
        for rank, player in enumerate(await self._leaderboard(limit=9999)):
            if player.jopacoin_balance < HOSTILE_LOSS_MIN_BALANCE:
                continue
            if rank < WHEEL_GOLDEN_TOP_N:
                percentage, minimum = WHEEL_GOLDEN_RECESSION_TOP_PCT, 50
            elif rank < WHEEL_GOLDEN_RECESSION_MID_RANK_END:
                percentage, minimum = WHEEL_GOLDEN_RECESSION_MID_PCT, 10
            else:
                percentage, minimum = WHEEL_GOLDEN_RECESSION_REST_PCT, 1
            loss = min(
                player.jopacoin_balance,
                scale_minigame_jc_delta(
                    max(minimum, int(player.jopacoin_balance * percentage))
                ),
            )
            if loss <= 0:
                continue
            settled = await self._hostile_loss(
                player,
                loss,
                "RECESSION",
                destination="reserve",
                clamp_to_balance=True,
                legacy_aggregate_transfer=True,
                metadata={"rank_index": rank},
            )
            centralized = centralized or settled.centralized
            self.state.recession_total += settled.applied
            self.state.recession_count += int(settled.applied > 0)
            self.state.record_shield(settled)

        loan_service = self.command.loan_service
        if loan_service and self.state.recession_total > 0 and not centralized:
            try:
                await asyncio.to_thread(
                    loan_service.add_to_nonprofit_fund,
                    self.context.guild_id,
                    self.state.recession_total,
                    source="gamba",
                    actor_id=self.context.user_id,
                    related_type="wheel_spin",
                    related_id="RECESSION",
                    reason="gamba recession reserve credit",
                    metadata={
                        "victim_count": self.state.recession_count,
                        "total": self.state.recession_total,
                    },
                )
            except Exception:
                logger.warning("Failed to add recession losses to nonprofit fund")
        await self._refresh_balance()
        self.state.recession_self_loss = max(
            0,
            spinner_balance_before - self.state.new_balance,
        )

    async def _extend_penalty(self) -> None:
        games = 1 if self.state.result_value == "EXTEND_1" else 2
        if (
            self.context.bankruptcy_service
            and self.context.penalty_games_remaining > 0
        ):
            total = await asyncio.to_thread(
                self.context.bankruptcy_service.add_penalty_games,
                self.context.user_id,
                self.context.guild_id,
                games,
            )
            self.state.extend_games_added = games
            self.state.extend_new_total = total

    async def _numeric_result(self) -> None:
        value = self.state.result_value
        if value > 0:
            if self.context.is_dig_bonus:
                value = scale_positive_dig_jc(value)
                self.state.replace_result(
                    (str(value), value, self.state.result_wedge[2])
                )
            if self.context.effects and self.context.effects.blue_gamba_reduction > 0:
                value -= int(value * self.context.effects.blue_gamba_reduction)
                self.state.replace_result(
                    (str(value), value, self.state.result_wedge[2])
                )
            self.state.new_balance, self.state.garnished_amount = (
                await self.command._credit_gamba_outcome(
                    self.context.user_id,
                    self.context.guild_id,
                    self.state.new_balance,
                    value,
                    str(self.state.result_wedge[0]),
                    "gamba wheel payout",
                )
            )
            return
        if value >= 0:
            return

        if self.context.is_bad_gamba:
            pardon_active = await asyncio.to_thread(
                self.player_service.get_wheel_pardon,
                self.context.user_id,
                self.context.guild_id,
            )
            if pardon_active:
                await asyncio.to_thread(
                    self.player_service.set_wheel_pardon,
                    self.context.user_id,
                    self.context.guild_id,
                    0,
                )
                self.state.replace_result(
                    (self.state.result_wedge[0], 0, self.state.result_wedge[2])
                )
                self.state.pardon_consumed = True
                return

        await self._adjust_spinner(
            value,
            "gamba wheel loss",
            str(self.state.result_wedge[0]),
        )
        await self._refresh_balance()
        if self.command.loan_service:
            try:
                await asyncio.to_thread(
                    self.command.loan_service.add_to_nonprofit_fund,
                    self.context.guild_id,
                    abs(value),
                    source="gamba",
                    actor_id=self.context.user_id,
                    related_type="wheel_spin",
                    related_id=str(self.state.result_wedge[0]),
                    reason="gamba bankrupt wheel loss reserve credit",
                    metadata={"wedge": str(self.state.result_wedge[0])},
                )
            except Exception:
                logger.warning("Failed to add wheel loss to nonprofit fund")

    async def _leaderboard(self, *, limit: int):
        # Filter after fetching all rows so a departed raw top-N player cannot
        # displace an active member from the visible rank.
        query_limit = (
            ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
            if has_guild_member_snapshot(self.context.interaction.guild)
            else limit
        )
        players = await asyncio.to_thread(
            functools.partial(
                self.player_service.get_leaderboard,
                self.context.guild_id,
                limit=query_limit,
            )
        )
        return filter_visible_leaderboard(
            players,
            self.context.interaction.guild,
            limit=limit,
        )

    async def _refresh_balance(self) -> None:
        self.state.new_balance = await asyncio.to_thread(
            self.player_service.get_balance,
            self.context.user_id,
            self.context.guild_id,
        )

    async def _adjust_spinner(
        self,
        delta: int,
        reason: str,
        outcome: str,
    ) -> None:
        await asyncio.to_thread(
            self.command._adjust_gamba_balance,
            self.context.user_id,
            self.context.user_id,
            self.context.guild_id,
            delta,
            reason,
            outcome,
        )

    async def _hostile_loss(
        self,
        victim,
        amount: int,
        outcome: str,
        **kwargs,
    ):
        return await self.command._apply_hostile_gamba_loss(
            victim_id=victim.discord_id,
            guild_id=self.context.guild_id,
            amount=amount,
            actor_id=self.context.user_id,
            event_key=f"{self.context.hostile_event_prefix}:{victim.discord_id}",
            outcome=outcome,
            min_balance=HOSTILE_LOSS_MIN_BALANCE,
            victim_balance=victim.jopacoin_balance,
            **kwargs,
        )
