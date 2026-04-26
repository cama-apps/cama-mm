"""
/russianroulette — Spin the barrel and randomly run a no-arg slash command.

Picks uniformly from a curated whitelist of non-admin commands that take no
mandatory parameters (read-only lookups plus no-arg actions like /gamba or
/trivia). Some entries prefill a required arg to expose specific sub-actions
(e.g. /shop recalibrate). Announces the roll in-channel, then delegates to
the chosen command's callback so it responds to the original interaction
normally.
"""

from __future__ import annotations

import logging
import random
import time
from dataclasses import dataclass, field
from typing import Any

import discord
from discord import app_commands
from discord.ext import commands

logger = logging.getLogger("cama_bot.commands.russianroulette")

# Per-(guild, user) in-memory cooldown for /russianroulette. Resets on bot
# restart by design — the cooldown is just friction, not durable state.
SPIN_COOLDOWN_SECONDS = 60 * 60
_spin_cooldowns: dict[tuple[int, int], float] = {}  # (guild_id, user_id) -> last spin ts


@dataclass(frozen=True)
class RouletteEntry:
    """One item in the roulette pool.

    ``command`` is the qualified command name (e.g. ``"dig info"`` or ``"shop"``).
    ``kwargs`` are passed to the command's callback, used to prefill a
    required param so a single underlying command can expose multiple
    roulette entries (e.g. /shop recalibrate vs /shop mystery_gift).
    ``label`` is the user-facing name announced in-channel; defaults to
    ``command`` when no kwargs are prefilled.
    """

    command: str
    kwargs: dict[str, Any] = field(default_factory=dict)
    label: str | None = None

    @property
    def display(self) -> str:
        return self.label or self.command


# Entries eligible for the roulette. All commands:
#   - have no mandatory parameters after prefill (safe to invoke with just
#     an interaction plus the kwargs listed here)
#   - are not admin-gated
#   - do not permanently destroy state without a confirmation prompt
ROULETTE_ENTRIES: list[RouletteEntry] = [
    # --- read-only lookups ---
    RouletteEntry("balance"),
    RouletteEntry("mybets"),
    RouletteEntry("bets"),
    RouletteEntry("nonprofit"),
    RouletteEntry("disburse"),
    RouletteEntry("leaderboard"),
    RouletteEntry("calibration"),
    RouletteEntry("help"),
    RouletteEntry("profile"),
    RouletteEntry("wrapped"),
    RouletteEntry("mana"),
    RouletteEntry("scout"),
    RouletteEntry("herogrid"),
    RouletteEntry("matchhistory"),
    RouletteEntry("recent"),
    RouletteEntry("myavoids"),
    RouletteEntry("mydeals"),
    RouletteEntry("predict list"),
    RouletteEntry("predict mine"),
    RouletteEntry("player steamids"),
    RouletteEntry("player exclusion"),
    RouletteEntry("dig info"),
    RouletteEntry("dig leaderboard"),
    RouletteEntry("dig halloffame"),
    RouletteEntry("dig shop"),
    RouletteEntry("dig museum"),
    RouletteEntry("dig flex"),
    RouletteEntry("dig inventory"),
    RouletteEntry("dig weather"),
    RouletteEntry("dig guide"),
    RouletteEntry("dig miner profile"),
    # --- actions with no mandatory params ---
    RouletteEntry("lobby"),
    RouletteEntry("join"),
    RouletteEntry("leave"),
    RouletteEntry("readycheck"),
    RouletteEntry("shuffle"),
    RouletteEntry("draft start"),
    RouletteEntry("trivia"),
    RouletteEntry("gamba"),
    RouletteEntry("incite"),
    RouletteEntry("dig go"),
    RouletteEntry("dig prestige"),
    RouletteEntry("dig abandon"),
    RouletteEntry("dig gear"),
    RouletteEntry("dig trap"),
    RouletteEntry("dig insure"),
    # --- shop actions with prefilled item (no target/hero needed) ---
    RouletteEntry("shop", {"item": "announce"}, label="shop flex"),
    RouletteEntry("shop", {"item": "mystery_gift"}, label="shop mystery_gift"),
    RouletteEntry("shop", {"item": "double_or_nothing"}, label="shop double_or_nothing"),
    RouletteEntry("shop", {"item": "recalibrate"}, label="shop recalibrate"),
    RouletteEntry("shop", {"item": "dig_dynamite"}, label="shop dig_dynamite"),
    RouletteEntry("shop", {"item": "dig_hard_hat"}, label="shop dig_hard_hat"),
    RouletteEntry("shop", {"item": "dig_lantern"}, label="shop dig_lantern"),
    RouletteEntry("shop", {"item": "dig_reinforcement"}, label="shop dig_reinforcement"),
    RouletteEntry("shop", {"item": "dig_upgrade"}, label="shop dig_upgrade"),
]


def _resolve_command(
    bot: commands.Bot, qualified_name: str
) -> app_commands.Command | None:
    """Find the registered app command by qualified name."""
    for cmd in bot.tree.walk_commands():
        if cmd.qualified_name == qualified_name:
            return cmd
    return None


async def _invoke_callback(
    cmd: app_commands.Command,
    interaction: discord.Interaction,
    **kwargs: Any,
) -> None:
    """Invoke an app command's callback directly, handling cog binding."""
    if cmd.binding is not None:
        await cmd._callback(cmd.binding, interaction, **kwargs)
    else:
        await cmd._callback(interaction, **kwargs)


class RussianRouletteCommands(commands.Cog):
    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @app_commands.command(
        name="russianroulette",
        description="Spin the barrel — runs a random no-arg slash command. Anything can happen.",
    )
    async def russianroulette(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else 0
        cooldown_key = (guild_id, interaction.user.id)
        now = time.time()
        last_spin = _spin_cooldowns.get(cooldown_key)
        if last_spin is not None and now - last_spin < SPIN_COOLDOWN_SECONDS:
            ready_at = int(last_spin + SPIN_COOLDOWN_SECONDS)
            await interaction.response.send_message(
                f"🎰 The barrel is still smoking. Reload <t:{ready_at}:R>.",
                ephemeral=True,
            )
            return

        pool: list[tuple[RouletteEntry, app_commands.Command]] = []
        for entry in ROULETTE_ENTRIES:
            cmd = _resolve_command(self.bot, entry.command)
            if cmd is not None:
                pool.append((entry, cmd))

        if not pool:
            await interaction.response.send_message(
                "🎰 The barrel is empty — no commands available to spin.",
                ephemeral=True,
            )
            return

        # Claim the cooldown slot before rolling so a misfire can't be
        # immediately retried. The spin itself is the commitment.
        _spin_cooldowns[cooldown_key] = now

        entry, chosen_cmd = random.choice(pool)
        logger.info(
            "russianroulette: user %s (%s) spun /%s",
            interaction.user.id,
            interaction.user,
            entry.display,
        )

        if interaction.channel is not None:
            try:
                await interaction.channel.send(
                    f"🎰 **Russian Roulette** — {interaction.user.mention} pulls the trigger... "
                    f"the chamber clicks on `/{entry.display}`!"
                )
            except discord.HTTPException as exc:
                logger.warning("Failed to announce russianroulette roll: %s", exc)

        try:
            await _invoke_callback(chosen_cmd, interaction, **entry.kwargs)
        except Exception as exc:
            logger.error(
                "russianroulette delegation to /%s failed: %s",
                entry.display,
                exc,
                exc_info=True,
            )
            message = (
                f"❌ `/{entry.display}` misfired in the chamber. The barrel jammed."
            )
            try:
                if interaction.response.is_done():
                    await interaction.followup.send(message, ephemeral=True)
                else:
                    await interaction.response.send_message(message, ephemeral=True)
            except discord.HTTPException:
                pass


async def setup(bot: commands.Bot):
    await bot.add_cog(RussianRouletteCommands(bot))
