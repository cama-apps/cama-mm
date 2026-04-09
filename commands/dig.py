"""
Tunnel digging minigame commands.
"""

from __future__ import annotations

import asyncio
import json
import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_gamba_channel
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from services.dig_constants import MAX_INVENTORY_SLOTS, PICKAXE_TIERS, get_layer as get_layer_def

if TYPE_CHECKING:
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

LAYER_COLORS = {
    "Dirt": 0x8B4513,
    "Stone": 0x808080,
    "Crystal": 0x00CED1,
    "Magma": 0xFF4500,
    "Abyss": 0x2F0047,
    "Fungal Depths": 0x7CFC00,
    "Frozen Core": 0x87CEEB,
    "The Hollow": 0x0D0D0D,
}

PROGRESSIVE_TIPS = [
    "Tip: Use /dig shop to buy consumables that help you dig faster.",
    "Tip: /dig help <user> lets you assist a friend's tunnel.",
    "Tip: Set a /dig trap to punish would-be saboteurs.",
    "Tip: /dig insure protects you from catastrophic cave-ins.",
    "Tip: Prestige resets depth but unlocks powerful perks.",
    "Tip: Bosses guard layer transitions. Bring friends to cheer!",
    "Tip: Relics from /dig museum are rare — gift duplicates to friends.",
    "Tip: Higher pickaxe tiers dig more blocks per action.",
    "Tip: Streaks grant bonus JC — keep digging daily!",
    "Tip: /dig flex shows off your achievements and titles.",
]

# "Dig Dug" flavor — classic arcade game references sprinkled in
DIG_DUG_TITLES = [
    "DIG DUG!",
    "Dig Dug would be proud.",
    "Another layer conquered!",
    "Dig Dug: Underground Champion",
    "You really dug that!",
]

DIG_DUG_FOOTERS = [
    "Dig Dug (1982) approves this tunnel.",
    "Pump it up! ...wait, wrong game.",
    "No Pookas or Fygars were harmed in this dig.",
    "Taizo Hori sends his regards.",
    "Round clear!",
    "Dig Dug high score: your tunnel depth.",
]

GUIDE_PAGES = [
    # Page 1: Basics
    discord.Embed(
        title="Dig Guide — Basics",
        description=(
            "**How Digging Works**\n"
            "Use `/dig` to advance your tunnel deeper. Each dig action advances "
            "you a number of blocks based on your pickaxe tier, active items, "
            "and a bit of luck.\n\n"
            "**Layers**\n"
            "The mine has eight layers: **Dirt**, **Stone**, **Crystal**, **Magma**, "
            "**Abyss**, **Fungal Depths**, **Frozen Core**, and **The Hollow**. "
            "Each layer is harder but more rewarding.\n\n"
            "**Cave-ins**\n"
            "Random cave-ins can collapse part of your tunnel, costing you depth. "
            "Insurance and reinforcements reduce the damage.\n\n"
            "**Decay**\n"
            "Inactive tunnels slowly decay over time. Keep digging to stay deep!"
        ),
        color=LAYER_COLORS["Dirt"],
    ),
    # Page 2: Items
    discord.Embed(
        title="Dig Guide — Items & Pickaxes",
        description=(
            "**Consumables**\n"
            "Buy consumables from `/dig shop` and queue them with `/dig use`. "
            "You can hold up to 8 items at a time. Queued items are used on "
            "your next dig.\n\n"
            "**Pickaxes**\n"
            "Upgrade your pickaxe with `/dig upgrade`. Higher tiers require depth "
            "milestones, JC, and prestige levels. Better pickaxes dig more blocks "
            "per action.\n\n"
            "**Relics**\n"
            "Rare artifacts found while digging. Equip them for passive bonuses. "
            "Gift duplicates to friends with `/dig gift`."
        ),
        color=LAYER_COLORS["Stone"],
    ),
    # Page 3: Bosses
    discord.Embed(
        title="Dig Guide — Bosses",
        description=(
            "**Boss Encounters**\n"
            "Bosses guard layer transitions. When you encounter one, you can:\n"
            "- **Fight**: Wager JC and choose a risk tier (Cautious/Bold/Reckless)\n"
            "- **Retreat**: Back away safely, keeping your depth\n"
            "- **Scout**: Use a lantern to reveal boss stats first\n\n"
            "**Cheering**\n"
            "Other players can cheer for you during boss fights, boosting your "
            "success chance. Rally your friends!\n\n"
            "**Risk Tiers**\n"
            "- **Cautious**: Lower wager multiplier, higher success chance\n"
            "- **Bold**: Balanced risk and reward\n"
            "- **Reckless**: Huge payoff potential, but high failure risk"
        ),
        color=LAYER_COLORS["Crystal"],
    ),
    # Page 4: Prestige
    discord.Embed(
        title="Dig Guide — Prestige",
        description=(
            "**Prestige System**\n"
            "Once you reach a deep enough depth, you can prestige. This resets "
            "your tunnel depth to zero but grants:\n"
            "- A permanent prestige level\n"
            "- A choice of prestige perks\n"
            "- Access to higher pickaxe tiers\n"
            "- Bragging rights\n\n"
            "**Perks**\n"
            "Each prestige lets you choose one perk that persists across resets. "
            "Choose wisely — they shape your digging strategy.\n\n"
            "**Relics**\n"
            "Some relics are only available at higher prestige levels. Collect "
            "them all and fill the `/dig museum`!"
        ),
        color=LAYER_COLORS["Magma"],
    ),
]


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

class _DictObj:
    """Thin wrapper so ``getattr(obj, key, default)`` works on dicts."""
    __slots__ = ("_d",)
    def __init__(self, d):
        self._d = d if isinstance(d, dict) else {}
    def __getattr__(self, name):
        try:
            v = self._d[name]
            return _DictObj(v) if isinstance(v, dict) else v
        except KeyError:
            raise AttributeError(name)
    def __repr__(self):
        return repr(self._d)


def _wrap(result):
    """Wrap a service result dict so getattr access works throughout the cog."""
    if isinstance(result, dict):
        return _DictObj(result)
    return result


def _layer_color(layer: str | None) -> int:
    """Return embed color for a layer name, defaulting to Dirt brown."""
    if layer is None:
        return LAYER_COLORS["Dirt"]
    return LAYER_COLORS.get(layer, LAYER_COLORS["Dirt"])


def _tip(index: int) -> str:
    """Return a rotating progressive tip."""
    return PROGRESSIVE_TIPS[index % len(PROGRESSIVE_TIPS)]


def _fmt_duration(seconds: int) -> str:
    """Format seconds into a human-readable duration."""
    if seconds < 60:
        return f"{seconds}s"
    if seconds < 3600:
        m, s = divmod(seconds, 60)
        return f"{m}m {s}s" if s else f"{m}m"
    if seconds < 86400:
        h, rem = divmod(seconds, 3600)
        m = rem // 60
        return f"{h}h {m}m" if m else f"{h}h"
    d, rem = divmod(seconds, 86400)
    h = rem // 3600
    return f"{d}d {h}h" if h else f"{d}d"


async def _check_registered(interaction: discord.Interaction, bot: commands.Bot):
    """Return the Player if registered, else send an ephemeral error and return None."""
    guild_id = interaction.guild.id if interaction.guild else None
    player = await asyncio.to_thread(bot.player_service.get_player, interaction.user.id, guild_id)
    if not player:
        await interaction.response.send_message(
            "You must be registered first. Use `/player register`.", ephemeral=True
        )
    return player


# ---------------------------------------------------------------------------
# Views
# ---------------------------------------------------------------------------

class PaidDigView(discord.ui.View):
    """Simple confirm/cancel for paid digs."""

    def __init__(self, dig_service: "DigService", user_id: int, guild_id: int | None, cost: int):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.cost = cost
        self.value: bool | None = None

    @discord.ui.button(label="Confirm", style=discord.ButtonStyle.green)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your dig.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await interaction.response.defer()

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your dig.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await interaction.response.defer()


class ConfirmSabotageView(discord.ui.View):
    """Dark-themed confirmation for sabotage actions."""

    def __init__(self, user_id: int, target: discord.Member, cost: int, damage_range: str):
        super().__init__(timeout=30)
        self.user_id = user_id
        self.target = target
        self.cost = cost
        self.damage_range = damage_range
        self.value: bool | None = None

    def build_embed(self) -> discord.Embed:
        embed = discord.Embed(
            title="Confirm Sabotage",
            description=(
                f"**Target:** {self.target.display_name}\n"
                f"**Cost:** {self.cost} {JOPACOIN_EMOTE}\n"
                f"**Potential damage:** {self.damage_range} blocks\n\n"
                "Are you sure? If they have a trap set, you could take damage instead."
            ),
            color=0x2C2F33,
        )
        return embed

    @discord.ui.button(label="Sabotage", style=discord.ButtonStyle.danger)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your sabotage.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await interaction.response.defer()

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your sabotage.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await interaction.response.defer()


class BossWagerModal(discord.ui.Modal):
    """Modal for entering boss fight wager details."""

    risk_tier = discord.ui.TextInput(
        label="Risk Tier (cautious / bold / reckless)",
        placeholder="bold",
        min_length=1,
        max_length=10,
        required=True,
    )

    wager = discord.ui.TextInput(
        label="Wager Amount",
        placeholder="e.g., 50",
        min_length=1,
        max_length=10,
        required=True,
    )

    def __init__(self, dig_service: "DigService", user_id: int, guild_id: int | None):
        super().__init__(title="Boss Fight Wager")
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.result = None

    async def on_submit(self, interaction: discord.Interaction):
        tier = self.risk_tier.value.strip().lower()
        if tier not in ("cautious", "bold", "reckless"):
            await interaction.response.send_message(
                "Invalid risk tier. Choose: cautious, bold, or reckless.", ephemeral=True
            )
            return

        try:
            amount = int(self.wager.value)
        except ValueError:
            await interaction.response.send_message(
                "Invalid wager amount. Please enter a number.", ephemeral=True
            )
            return

        if amount < 0:
            await interaction.response.send_message(
                "Wager must be non-negative.", ephemeral=True
            )
            return

        await interaction.response.defer()
        try:
            self.result = _wrap(await asyncio.to_thread(
                self.dig_service.fight_boss,
                self.user_id,
                self.guild_id,
                tier,
                amount,
            ))

            if not getattr(self.result, "success", True):
                error_msg = getattr(self.result, "error", "Boss fight failed.")
                embed = discord.Embed(
                    title="Boss Fight Error",
                    description=error_msg,
                    color=0xFFA500,
                )
                await interaction.followup.send(embed=embed, ephemeral=True)
                return

            embed = discord.Embed(
                title="Boss Fight Result",
                color=0x00FF00 if getattr(self.result, "won", False) else 0xFF0000,
            )
            boss_name = getattr(self.result, "boss_name", "the boss")
            win_chance = getattr(self.result, "win_chance", 0)
            if getattr(self.result, "won", False):
                payout = getattr(self.result, "payout", 0) or getattr(self.result, "jc_delta", 0)
                embed.description = (
                    f"Victory! You defeated **{boss_name}** and earned "
                    f"**{payout}** {JOPACOIN_EMOTE}!"
                )
            else:
                loss = abs(getattr(self.result, "jc_delta", 0)) or amount
                knockback = getattr(self.result, "knockback", 0)
                embed.description = (
                    f"Defeat! **{boss_name}** overpowered you. "
                    f"You lost **{loss}** {JOPACOIN_EMOTE}"
                    f" and were knocked back {knockback} blocks."
                )
            embed.add_field(
                name="Details",
                value=f"Risk: {tier.title()} | Win chance: {int(win_chance * 100)}%",
                inline=False,
            )
            await interaction.followup.send(embed=embed)
        except Exception as e:
            logger.error("Boss fight error: %s", e)
            await interaction.followup.send("Boss fight failed. Try again.", ephemeral=True)


class EventEncounterView(discord.ui.View):
    """Interactive view for choice/complex events with safe and risky buttons."""

    def __init__(
        self,
        dig_service: "DigService",
        user_id: int,
        guild_id: int | None,
        event_data: dict,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.event_data = event_data
        safe_label = "Play it safe"
        risky_label = "Take the risk"
        if isinstance(event_data, dict):
            safe_opt = event_data.get("safe_option")
            risky_opt = event_data.get("risky_option")
            if isinstance(safe_opt, dict):
                safe_label = safe_opt.get("label", safe_label)
            if isinstance(risky_opt, dict):
                risky_label = risky_opt.get("label", risky_label)
        self.safe_btn.label = safe_label[:80]
        self.risky_btn.label = risky_label[:80]

    @discord.ui.button(label="Safe", style=discord.ButtonStyle.secondary, emoji="\U0001f6e1\ufe0f")
    async def safe_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        await interaction.response.defer()
        result = await self._resolve("safe")
        await interaction.followup.send(embed=result)
        self.stop()

    @discord.ui.button(label="Risky", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def risky_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        await interaction.response.defer()
        result = await self._resolve("risky")
        await interaction.followup.send(embed=result)
        self.stop()

    async def _resolve(self, choice: str) -> discord.Embed:
        """Resolve the event choice and return an embed with the outcome."""
        event = self.event_data
        option = event.get(f"{choice}_option", {}) if isinstance(event, dict) else {}
        success_chance = option.get("success_chance", 1.0) if isinstance(option, dict) else 1.0

        won = random.random() < success_chance
        outcome = option.get("success") if won else option.get("failure")
        if outcome is None:
            outcome = option.get("success", {})
            won = True

        desc = outcome.get("description", "Nothing happened.") if isinstance(outcome, dict) else "Nothing happened."
        advance = outcome.get("advance", 0) if isinstance(outcome, dict) else 0
        jc = outcome.get("jc", 0) if isinstance(outcome, dict) else 0
        cave_in = outcome.get("cave_in", False) if isinstance(outcome, dict) else False

        # Apply effects
        if jc != 0:
            await asyncio.to_thread(
                self.dig_service.player_repo.add_balance, self.user_id, self.guild_id, jc
            )
        if advance != 0:
            tunnel = await asyncio.to_thread(self.dig_service.dig_repo.get_tunnel, self.user_id, self.guild_id)
            if tunnel:
                depth = dict(tunnel).get("depth", 0)
                new_depth = max(0, depth + advance)
                await asyncio.to_thread(
                    self.dig_service.dig_repo.update_tunnel, self.user_id, self.guild_id, depth=new_depth
                )

        # Apply temp buff if risky success
        if won and choice == "risky" and isinstance(event, dict) and event.get("buff_on_success"):
            await asyncio.to_thread(
                self.dig_service.set_temp_buff, self.user_id, self.guild_id, event["buff_on_success"]
            )

        # Build result embed
        color = 0x00FF00 if won else 0xFF4444
        embed = discord.Embed(
            title=event.get("name", "Event") if isinstance(event, dict) else "Event",
            description=desc,
            color=color,
        )
        parts = []
        if advance != 0:
            parts.append(f"{'+'if advance > 0 else ''}{advance} blocks")
        if jc != 0:
            parts.append(f"{'+'if jc > 0 else ''}{jc} {JOPACOIN_EMOTE}")
        if cave_in:
            parts.append("Cave-in triggered!")
        if parts:
            embed.add_field(name="Outcome", value=" | ".join(parts), inline=False)

        # Show buff if granted
        if won and choice == "risky" and isinstance(event, dict) and event.get("buff_on_success"):
            buff = event["buff_on_success"]
            embed.add_field(
                name=f"Buff: {buff.get('name', '?')}",
                value=f"Active for {buff.get('duration_digs', 0)} digs",
                inline=True,
            )

        return embed


class BossEncounterView(discord.ui.View):
    """View for boss encounter interactions."""

    def __init__(
        self,
        dig_service: "DigService",
        user_id: int,
        guild_id: int | None,
        boss_info: object,
        has_lantern: bool = False,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.boss_info = boss_info
        self.has_lantern = has_lantern
        if not has_lantern:
            self.scout.disabled = True

    @discord.ui.button(label="Fight", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def fight(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            # Others can cheer, not fight
            await interaction.response.send_message("Only the tunnel owner can fight.", ephemeral=True)
            return
        modal = BossWagerModal(self.dig_service, self.user_id, self.guild_id)
        await interaction.response.send_modal(modal)
        self.stop()

    @discord.ui.button(label="Retreat", style=discord.ButtonStyle.secondary, emoji="\U0001f3c3")
    async def retreat(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the tunnel owner can retreat.", ephemeral=True)
            return
        await interaction.response.defer()
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.retreat_boss, self.user_id, self.guild_id
            ))
            if not getattr(result, "success", True):
                await interaction.followup.send(
                    getattr(result, "error", "Retreat failed."), ephemeral=True
                )
            else:
                loss = getattr(result, "loss", 0)
                new_depth = getattr(result, "new_depth", 0)
                await interaction.followup.send(
                    f"You retreated safely, losing {loss} blocks. Now at depth {new_depth}."
                )
        except Exception as e:
            logger.error("Boss retreat error: %s", e)
            await interaction.followup.send("Retreat failed.", ephemeral=True)
        self.stop()

    @discord.ui.button(label="Scout", style=discord.ButtonStyle.primary, emoji="\U0001f526")
    async def scout(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the tunnel owner can scout.", ephemeral=True)
            return
        await interaction.response.defer()
        try:
            info = _wrap(await asyncio.to_thread(
                self.dig_service.scout_boss, self.user_id, self.guild_id
            ))
            if not getattr(info, "success", True):
                await interaction.followup.send(
                    getattr(info, "error", "Scouting failed."), ephemeral=True
                )
                return
            boss_name = getattr(info, "boss_name", "Unknown Boss")
            odds = getattr(info, "odds", None)
            if odds and hasattr(odds, "_d"):
                odds = odds._d
            lines = [f"**{boss_name}** — Intel Report\n"]
            if isinstance(odds, dict):
                for tier in ("cautious", "bold", "reckless"):
                    t = odds.get(tier)
                    if not t:
                        continue
                    win = int(t.get("win_pct", 0) * 100)
                    free = int(t.get("free_fight_pct", 0) * 100)
                    mult = t.get("multiplier", 1)
                    lines.append(
                        f"**{tier.title()}** — {win}% win"
                        f" ({free}% free) | {mult}x payout"
                    )
            else:
                lines.append("Could not read odds data.")
            embed = discord.Embed(
                title="Boss Scouted",
                description="\n".join(lines),
                color=0xFFD700,
            )
            await interaction.followup.send(embed=embed, ephemeral=True)
        except Exception as e:
            logger.error("Boss scout error: %s", e)
            await interaction.followup.send("Scouting failed.", ephemeral=True)

    @discord.ui.button(label="Cheer", style=discord.ButtonStyle.success, emoji="\U0001f4e3")
    async def cheer(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id == self.user_id:
            await interaction.response.send_message("You can't cheer for yourself!", ephemeral=True)
            return
        await interaction.response.defer()
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.cheer_boss,
                interaction.user.id,
                self.user_id,
                self.guild_id,
            ))
            if not getattr(result, "success", True):
                error_msg = getattr(result, "error", "Cheer failed.")
                await interaction.followup.send(error_msg, ephemeral=True)
                return
            boost_pct = int(getattr(result, "total_boost", 0) * 100)
            cheer_count = getattr(result, "cheer_count", 0)
            await interaction.followup.send(
                f"{interaction.user.display_name} cheers for the fighter! "
                f"Boss odds boosted by +{boost_pct}% ({cheer_count}/3 cheers)"
            )
        except Exception as e:
            logger.error("Boss cheer error: %s", e)
            await interaction.followup.send("Cheer failed.", ephemeral=True)


class PrestigePerksView(discord.ui.View):
    """View for selecting prestige perks."""

    def __init__(
        self,
        dig_service: "DigService",
        user_id: int,
        guild_id: int | None,
        perks: list[dict],
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.perks = perks
        for i, perk in enumerate(perks[:5]):
            button = discord.ui.Button(
                label=perk.get("name", f"Perk {i+1}"),
                style=discord.ButtonStyle.primary,
                custom_id=f"prestige_perk_{i}",
            )
            button.callback = self._make_callback(i, perk)
            self.add_item(button)

    def _make_callback(self, index: int, perk: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your prestige.", ephemeral=True)
                return
            await interaction.response.defer()
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.prestige,
                    self.user_id,
                    self.guild_id,
                    perk.get("id", index),
                ))
                embed = discord.Embed(
                    title="Prestige Complete!",
                    description=(
                        f"You selected **{perk.get('name', 'Unknown')}**.\n"
                        f"{getattr(result, 'message', 'Your tunnel has been reset. Dig deeper!')}"
                    ),
                    color=0xFFD700,
                )
                await interaction.followup.send(embed=embed)
            except ValueError as e:
                await interaction.followup.send(str(e), ephemeral=True)
            except Exception as e:
                logger.error("Prestige error: %s", e)
                await interaction.followup.send("Prestige failed.", ephemeral=True)
            self.stop()

        return callback


class DigGuideView(discord.ui.View):
    """Paginated guide with Previous/Next buttons."""

    def __init__(self):
        super().__init__(timeout=180)
        self.current = 0
        self._sync_buttons()

    def _sync_buttons(self) -> None:
        self.prev_btn.disabled = self.current == 0
        self.next_btn.disabled = self.current >= len(GUIDE_PAGES) - 1

    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary)
    async def prev_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current -= 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=GUIDE_PAGES[self.current], view=self)

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current += 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=GUIDE_PAGES[self.current], view=self)


class MuseumView(discord.ui.View):
    """Paginated museum display by layer."""

    def __init__(self, pages: list[discord.Embed]):
        super().__init__(timeout=180)
        self.pages = pages
        self.current = 0
        self._sync_buttons()

    def _sync_buttons(self) -> None:
        self.prev_btn.disabled = self.current == 0
        self.next_btn.disabled = self.current >= len(self.pages) - 1

    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary)
    async def prev_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current -= 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=self.pages[self.current], view=self)

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current += 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=self.pages[self.current], view=self)


class ConfirmAbandonView(discord.ui.View):
    """Confirm tunnel abandonment with refund info."""

    def __init__(self, user_id: int, refund: int):
        super().__init__(timeout=60)
        self.user_id = user_id
        self.refund = refund
        self.value: bool | None = None

    @discord.ui.button(label="Abandon Tunnel", style=discord.ButtonStyle.danger)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your tunnel.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await interaction.response.defer()

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your tunnel.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await interaction.response.defer()


class UpgradeView(discord.ui.View):
    """View for purchasing pickaxe upgrades."""

    def __init__(self, dig_service: "DigService", user_id: int, guild_id: int | None, upgrade_info: dict):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.upgrade_info = upgrade_info
        if not upgrade_info.get("eligible", False):
            self.buy_btn.disabled = True

    @discord.ui.button(label="Buy Upgrade", style=discord.ButtonStyle.green, emoji="\u2b06\ufe0f")
    async def buy_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your upgrade.", ephemeral=True)
            return
        await interaction.response.defer()
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.upgrade_pickaxe, self.user_id, self.guild_id
            ))
            embed = discord.Embed(
                title="Pickaxe Upgraded!",
                description=getattr(result, "message", "Your pickaxe has been upgraded!"),
                color=0x00FF00,
            )
            await interaction.followup.send(embed=embed)
        except ValueError as e:
            await interaction.followup.send(str(e), ephemeral=True)
        except Exception as e:
            logger.error("Upgrade error: %s", e)
            await interaction.followup.send("Upgrade failed.", ephemeral=True)
        self.stop()


# ---------------------------------------------------------------------------
# Cog
# ---------------------------------------------------------------------------

class DigCommands(commands.Cog):
    dig = app_commands.Group(name="dig", description="Tunnel digging minigame")

    def __init__(self, bot: commands.Bot, dig_service: "DigService"):
        self.bot = bot
        self.dig_service = dig_service

    # ------------------------------------------------------------------
    # Autocomplete helpers
    # ------------------------------------------------------------------

    async def item_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for owned consumable items."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            items = await asyncio.to_thread(
                self.dig_service.get_inventory, interaction.user.id, guild_id
            )
            choices = [
                app_commands.Choice(name=item.get("name", str(item)), value=item.get("type", item.get("name", str(item))))
                for item in (items or [])
                if current.lower() in item.get("name", "").lower()
            ]
            return choices[:25]
        except Exception:
            return []

    async def relic_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for owned relics."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            relics = await asyncio.to_thread(
                self.dig_service.get_owned_relics, interaction.user.id, guild_id
            )
            choices = [
                app_commands.Choice(name=r.get("name", str(r)), value=r.get("name", str(r)))
                for r in (relics or [])
                if current.lower() in r.get("name", "").lower()
            ]
            return choices[:25]
        except Exception:
            return []

    # ------------------------------------------------------------------
    # 1. /dig — Main dig command
    # ------------------------------------------------------------------

    @dig.command(name="go", description="Dig deeper into your tunnel")
    async def dig_go(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        rl_gid = guild_id if guild_id else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="dig", guild_id=rl_gid, user_id=interaction.user.id, limit=2, per_seconds=30
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Slow down! Wait {rl.retry_after_seconds}s.", ephemeral=True
            )
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.dig, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Dig error: %s", e)
            await safe_followup(interaction, content="Dig failed. Try again later.", ephemeral=True)
            return

        # Check for non-cooldown errors (cooldown handled below via paid_dig_available)
        if not getattr(result, "success", False) and not getattr(result, "paid_dig_available", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Dig failed."),
                ephemeral=True,
            )
            return

        # First dig welcome
        if getattr(result, "is_first_dig", False):
            embed = discord.Embed(
                title="Welcome to the Mines!",
                description=(
                    "You've started digging your very own tunnel!\n\n"
                    "Use `/dig` to advance deeper, `/dig shop` to buy items, "
                    "and `/dig guide` for a full tutorial.\n\n"
                    "Good luck, miner! **DIG DUG!**"
                ),
                color=LAYER_COLORS["Dirt"],
            )
            await safe_followup(interaction, embed=embed)
            return

        # Boss encounter
        if getattr(result, "boss_encounter", False):
            boss_info = getattr(result, "boss_info", None)
            has_lantern = getattr(result, "has_lantern", False)
            embed = discord.Embed(
                title=f"Boss Encountered: {getattr(boss_info, 'name', 'Unknown Boss')}!",
                description=getattr(boss_info, "dialogue", "A fearsome guardian blocks your path!"),
                color=0xFF0000,
            )
            if hasattr(boss_info, "ascii_art"):
                embed.add_field(name="\u200b", value=f"```\n{boss_info.ascii_art}\n```", inline=False)
            view = BossEncounterView(self.dig_service, interaction.user.id, guild_id, boss_info, has_lantern)
            msg = await safe_followup(interaction, embed=embed, view=view)
            if msg:
                try:
                    await msg.add_reaction("\U0001f480")
                except Exception:
                    pass
            return

        # Paid dig confirmation
        if getattr(result, "paid_dig_available", False):
            cost = getattr(result, "paid_dig_cost", 0)
            embed = discord.Embed(
                title="Paid Dig Required",
                description=f"Continuing costs **{cost}** {JOPACOIN_EMOTE}. Proceed?",
                color=0xFFA500,
            )
            view = PaidDigView(self.dig_service, interaction.user.id, guild_id, cost)
            msg = await safe_followup(interaction, embed=embed, view=view)
            if msg:
                await view.wait()
                if view.value:
                    try:
                        paid_result = _wrap(await asyncio.to_thread(
                            self.dig_service.dig, interaction.user.id, guild_id, paid=True
                        ))
                        if not getattr(paid_result, "success", False):
                            err = getattr(paid_result, "error", "Paid dig failed.")
                            await msg.edit(content=err, embed=None, view=None)
                        else:
                            paid_embed = _build_dig_embed(paid_result, interaction.user)
                            await msg.edit(embed=paid_embed, view=None)
                    except Exception as e:
                        logger.error("Paid dig error: %s", e)
                        await msg.edit(content="Paid dig failed.", embed=None, view=None)
                else:
                    await msg.edit(content="Dig cancelled.", embed=None, view=None)
            return

        # Complex event encounter — show interactive buttons with visuals
        event = getattr(result, "event", None)
        if event:
            event_data = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
            complexity = event_data.get("complexity", "choice") if isinstance(event_data, dict) else "choice"
            if complexity in ("complex", "choice") and isinstance(event_data, dict) and event_data.get("safe_option"):
                embed = _build_dig_embed(result, interaction.user)
                event_embed = discord.Embed(
                    title=event_data.get("name", "Event"),
                    description=event_data.get("description", "Something happens..."),
                    color=0xDAA520,
                )
                # ASCII art in code block
                ascii_art = event_data.get("ascii_art") if isinstance(event_data, dict) else None
                if ascii_art:
                    event_embed.add_field(name="\u200b", value=f"```\n{ascii_art}\n```", inline=False)
                rarity = event_data.get("rarity", "common")
                if rarity != "common":
                    event_embed.set_footer(text=f"{rarity.title()} encounter")
                # Pixel art scene for complex events
                event_file = None
                event_id = event_data.get("id", "") if isinstance(event_data, dict) else ""
                if complexity == "complex":
                    try:
                        from utils.dig_drawing import draw_event_scene, has_event_scene
                        if has_event_scene(event_id):
                            depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
                            layer_def = get_layer_def(depth)
                            layer_name = layer_def.name if layer_def else "Dirt"
                            scene_buf = await asyncio.to_thread(draw_event_scene, layer_name, event_id)
                            event_file = discord.File(scene_buf, filename="event_scene.png")
                            event_embed.set_image(url="attachment://event_scene.png")
                    except Exception as e:
                        logger.debug(f"Event scene generation failed: {e}")
                view = EventEncounterView(self.dig_service, interaction.user.id, guild_id, event_data)
                await safe_followup(interaction, embed=embed)
                if event_file:
                    await safe_followup(interaction, embed=event_embed, view=view, file=event_file)
                else:
                    await safe_followup(interaction, embed=event_embed, view=view)
                return

        # Normal dig result
        embed = _build_dig_embed(result, interaction.user)
        msg = await safe_followup(interaction, embed=embed)

        # Add reactions
        if msg:
            reactions = ["\u26cf\ufe0f"]  # pickaxe
            if getattr(result, "cave_in", None):
                reactions.append("\U0001f4a5")
            if getattr(result, "artifact", None):
                reactions.append("\U0001f48e")
            for r in reactions:
                try:
                    await msg.add_reaction(r)
                except Exception:
                    pass

    # ------------------------------------------------------------------
    # 2. /dig_help — Help another player
    # ------------------------------------------------------------------

    @dig.command(name="help", description="Help another player's tunnel")
    @app_commands.describe(user="The player to help")
    async def dig_help(self, interaction: discord.Interaction, user: discord.Member):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        if user.id == interaction.user.id:
            self_help_lines = [
                "You tried to help yourself. The pickaxe is confused.",
                "That's not how teamwork works, chief.",
                "Mining solo is fine, but helping yourself is just sad.",
                "Your tunnel filed a restraining order against your own help.",
                "You can't pat your own back with a pickaxe. Well, you can, but you shouldn't.",
                "Self-help books are in aisle 3. This is a mine.",
            ]
            await safe_followup(
                interaction,
                content=random.choice(self_help_lines),
                ephemeral=True,
            )
            return

        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.help_tunnel,
                interaction.user.id,
                user.id,
                guild_id,
            ))
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Dig help error: %s", e)
            await safe_followup(interaction, content="Help failed.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Help failed."),
                ephemeral=True,
            )
            return

        blocks = getattr(result, "advance", 0)
        embed = discord.Embed(
            title="Tunnel Assistance",
            description=(
                f"You helped **{user.display_name}**'s tunnel!\n"
                f"Blocks added: **{blocks}**"
            ),
            color=0x2ECC71,
        )
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 3. /dig_sabotage — Sabotage another player
    # ------------------------------------------------------------------

    @dig.command(name="sabotage", description="Sabotage another player's tunnel")
    @app_commands.describe(user="The player to sabotage")
    async def dig_sabotage(self, interaction: discord.Interaction, user: discord.Member):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Get sabotage preview info
        try:
            preview = _wrap(await asyncio.to_thread(
                self.dig_service.preview_sabotage,
                interaction.user.id,
                user.id,
                guild_id,
            ))
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Sabotage preview error: %s", e)
            await interaction.response.send_message("Sabotage failed.", ephemeral=True)
            return

        if not getattr(preview, "success", False):
            await interaction.response.send_message(
                getattr(preview, "error", "Sabotage failed."),
                ephemeral=True,
            )
            return

        cost = getattr(preview, "cost", 0)
        damage_range = getattr(preview, "damage_range", "unknown")

        view = ConfirmSabotageView(interaction.user.id, user, cost, str(damage_range))
        embed = view.build_embed()
        await interaction.response.send_message(embed=embed, view=view)
        await view.wait()

        if view.value:
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.sabotage_tunnel,
                    interaction.user.id,
                    user.id,
                    guild_id,
                ))
                if not getattr(result, "success", False):
                    await interaction.edit_original_response(
                        content=getattr(result, "error", "Sabotage failed."),
                        embed=None, view=None,
                    )
                    return
                result_embed = discord.Embed(color=0x2C2F33)
                if getattr(result, "trap_triggered", False):
                    trap = getattr(result, "trap_detail", None)
                    trap_msg = getattr(trap, "message", "") if trap else ""
                    result_embed.title = "Trap Triggered!"
                    result_embed.description = (
                        f"Your sabotage attempt backfired!\n{trap_msg}"
                    )
                    result_embed.color = 0xFF0000
                else:
                    damage = getattr(result, "damage", 0)
                    result_embed.title = "Sabotage Successful"
                    result_embed.description = (
                        f"You sabotaged **{user.display_name}**'s tunnel!\n"
                        f"Damage dealt: **{damage}** blocks"
                    )
                await interaction.edit_original_response(embed=result_embed, view=None)
            except ValueError as e:
                await interaction.edit_original_response(content=str(e), embed=None, view=None)
            except Exception as e:
                logger.error("Sabotage error: %s", e)
                await interaction.edit_original_response(content="Sabotage failed.", embed=None, view=None)
        else:
            await interaction.edit_original_response(content="Sabotage cancelled.", embed=None, view=None)

    # ------------------------------------------------------------------
    # 4. /dig info — View tunnel info
    # ------------------------------------------------------------------

    @dig.command(name="info", description="View tunnel information")
    @app_commands.describe(user="View another player's tunnel (optional)")
    async def dig_info(self, interaction: discord.Interaction, user: discord.Member | None = None):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        target_id = user.id if user else interaction.user.id
        is_own = target_id == interaction.user.id

        try:
            info = await asyncio.to_thread(
                self.dig_service.get_tunnel_info, target_id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Dig info error: %s", e)
            await safe_followup(interaction, content="Failed to fetch tunnel info.", ephemeral=True)
            return

        if info is None:
            target_name = (user.display_name if user else interaction.user.display_name)
            await safe_followup(
                interaction,
                content=f"{target_name} hasn't started digging yet.",
                ephemeral=True,
            )
            return

        # Service returns a raw dict — don't wrap, use .get() directly
        layer_info = info.get("layer", {}) if isinstance(info, dict) else {}
        layer_name = layer_info.get("name", "Dirt") if isinstance(layer_info, dict) else "Dirt"
        tunnel = info.get("tunnel", {}) if isinstance(info, dict) else {}

        display_user = user or interaction.user
        embed = discord.Embed(
            title=f"{display_user.display_name}'s Tunnel",
            color=_layer_color(layer_name),
        )

        # Core stats
        depth = info.get("depth", 0) if isinstance(info, dict) else 0
        prestige = info.get("prestige_level", 0) if isinstance(info, dict) else 0
        pickaxe_idx = tunnel.get("pickaxe_tier", 0) or 0
        pickaxe_name = PICKAXE_TIERS[pickaxe_idx]["name"] if pickaxe_idx < len(PICKAXE_TIERS) else "Wooden"
        prestige_text = f" (Prestige {prestige})" if prestige else ""
        embed.add_field(
            name="Depth",
            value=f"**{depth}** blocks — {layer_name}{prestige_text}",
            inline=True,
        )
        embed.add_field(name="Pickaxe", value=pickaxe_name, inline=True)

        # Equipped relics
        relics = info.get("relics", []) if isinstance(info, dict) else []
        if relics:
            relic_text = ", ".join(r.get("name", "?") if isinstance(r, dict) else str(r) for r in relics)
            embed.add_field(name="Relics", value=relic_text, inline=False)

        # Queued items
        queued = info.get("queued_items", []) if isinstance(info, dict) else []
        if queued:
            item_text = ", ".join(i.get("name", "?") if isinstance(i, dict) else str(i) for i in queued)
            embed.add_field(name="Queued Items", value=item_text, inline=False)

        # Boss status
        at_boss = info.get("at_boss", False) if isinstance(info, dict) else False
        next_boss = info.get("next_boss", None) if isinstance(info, dict) else None
        if at_boss:
            embed.add_field(name="Boss", value="A boss blocks your path!", inline=True)
        elif next_boss:
            embed.add_field(name="Next Boss", value=f"Depth {next_boss}", inline=True)

        # Insurance / reinforcement
        now = int(time.time())
        insured_until = tunnel.get("insured_until", 0) or 0
        reinforced_until = tunnel.get("reinforced_until", 0) or 0
        status_parts = []
        if now < insured_until:
            status_parts.append("Insured")
        if now < reinforced_until:
            status_parts.append("Reinforced")
        if status_parts:
            embed.add_field(name="Protection", value=", ".join(status_parts), inline=True)

        # Trap status
        trap = tunnel.get("trap_active", False)
        if is_own and trap:
            embed.add_field(name="Trap", value="Armed", inline=True)
        elif not is_own and trap:
            embed.add_field(name="Trap", value="Something feels off...", inline=True)

        # Streak
        streak = info.get("streak", 0) if isinstance(info, dict) else 0
        if streak:
            embed.add_field(name="Streak", value=f"{streak} days", inline=True)

        # Cooldown
        cooldown = info.get("cooldown_remaining", 0) if isinstance(info, dict) else 0
        if cooldown and cooldown > 0:
            embed.add_field(name="Cooldown", value=_fmt_duration(cooldown), inline=True)

        # Next milestone
        milestone = info.get("next_milestone", None) if isinstance(info, dict) else None
        if milestone and isinstance(milestone, dict):
            embed.add_field(
                name="Next Milestone",
                value=f"Depth {milestone.get('depth', '?')} (+{milestone.get('reward', '?')} JC)",
                inline=True,
            )

        # Recent events — parse the JSON detail for a readable summary
        events = info.get("recent_events", []) if isinstance(info, dict) else []
        if events:
            event_lines = []
            for ev in events[:5]:
                if not isinstance(ev, dict):
                    continue
                action = ev.get("action_type", "?")
                detail_raw = ev.get("detail") or ev.get("details") or "{}"
                try:
                    detail = json.loads(detail_raw) if isinstance(detail_raw, str) else detail_raw
                except (json.JSONDecodeError, TypeError):
                    detail = {}
                if action == "dig":
                    adv = detail.get("advance", 0)
                    jc = detail.get("jc", 0)
                    if detail.get("cave_in"):
                        event_lines.append(f"Cave-in! Lost {detail.get('block_loss', '?')} blocks")
                    else:
                        event_lines.append(f"Dug +{adv} blocks, +{jc} JC")
                elif action == "sabotage":
                    dmg = detail.get("damage", "?")
                    if detail.get("trap_triggered"):
                        event_lines.append(f"Sabotage attempt — trap triggered!")
                    else:
                        event_lines.append(f"Sabotaged — lost {dmg} blocks")
                elif action == "help":
                    adv = detail.get("advance", "?")
                    event_lines.append(f"Helped — +{adv} blocks")
                else:
                    event_lines.append(action.replace("_", " ").title())
            if event_lines:
                embed.add_field(name="Recent Events", value="\n".join(event_lines), inline=False)

        # Stats
        stats = getattr(info, "stats", None)
        if stats:
            stats_text = (
                f"Total digs: {stats.get('total_digs', 0)}\n"
                f"Max depth: {stats.get('max_depth', 0)}\n"
                f"Total JC earned: {stats.get('total_jc_earned', 0)}\n"
                f"Cave-ins survived: {stats.get('cave_ins_survived', 0)}\n"
                f"Bosses defeated: {stats.get('bosses_defeated', 0)}"
            )
            embed.add_field(name="Stats", value=stats_text, inline=False)

        embed.set_thumbnail(url=display_user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 5. /dig_leaderboard — Top tunnels
    # ------------------------------------------------------------------

    @dig.command(name="leaderboard", description="View top tunnels")
    async def dig_leaderboard(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            lb = _wrap(await asyncio.to_thread(
                self.dig_service.get_leaderboard, guild_id
            ))
        except Exception as e:
            logger.error("Leaderboard error: %s", e)
            await safe_followup(interaction, content="Leaderboard unavailable.", ephemeral=True)
            return

        entries = getattr(lb, "tunnels", []) or []
        if not entries:
            await safe_followup(interaction, content="No tunnels yet! Use `/dig` to start.", ephemeral=True)
            return

        # Build leaderboard text
        lines = []
        def _get(obj, key, default=None):
            return obj.get(key, default) if isinstance(obj, dict) else getattr(obj, key, default)

        max_depth = max(_get(e, "depth", 0) for e in entries[:10]) or 1
        for i, entry in enumerate(entries[:10], 1):
            name = _get(entry, "name", f"Player {_get(entry, 'user_id', '?')}")
            depth = _get(entry, "depth", 0)
            layer = _get(entry, "layer", "Dirt")
            bar_len = max(1, int(20 * depth / max_depth))
            bar = "\u2588" * bar_len
            medal = {1: "\U0001f947", 2: "\U0001f948", 3: "\U0001f949"}.get(i, f"`{i}.`")
            lines.append(f"{medal} **{name}** — {depth} ({layer})\n`{bar}`")

        # Requester's position
        user_pos = getattr(lb, "user_position", None)
        if user_pos and user_pos > 10:
            lines.append(f"\n---\n`{user_pos}.` **You** — {getattr(lb, 'user_depth', '?')}")

        embed = discord.Embed(
            title="Tunnel Leaderboard",
            description="\n".join(lines),
            color=0xFFD700,
        )
        embed.set_footer(text="Community Mine")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 6. /dig use — Queue consumable
    # ------------------------------------------------------------------

    @dig.command(name="use", description="Queue a consumable for your next dig")
    @app_commands.describe(item="The item to use")
    @app_commands.autocomplete(item=item_autocomplete)
    async def dig_use(self, interaction: discord.Interaction, item: str):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.use_item, interaction.user.id, guild_id, item
            ))
            if not getattr(result, "success", False):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Failed to queue item."),
                    ephemeral=True,
                )
                return
            item_name = getattr(result, "item", item)
            await safe_followup(
                interaction,
                content=f"**{item_name}** queued for your next dig.",
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Dig use error: %s", e)
            await safe_followup(interaction, content="Failed to queue item.", ephemeral=True)

    # ------------------------------------------------------------------
    # 7. /dig gift — Gift a relic
    # ------------------------------------------------------------------

    @dig.command(name="gift", description="Gift a relic to another player")
    @app_commands.describe(user="The player to gift to", artifact="The relic to gift")
    @app_commands.autocomplete(artifact=relic_autocomplete)
    async def dig_gift(self, interaction: discord.Interaction, user: discord.Member, artifact: str):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.gift_relic,
                interaction.user.id,
                user.id,
                guild_id,
                artifact,
            ))
            await safe_followup(
                interaction,
                content=(
                    f"You gifted **{artifact}** to **{user.display_name}**! "
                    f"{getattr(result, 'message', '')}"
                ),
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Dig gift error: %s", e)
            await safe_followup(interaction, content="Gift failed.", ephemeral=True)

    # ------------------------------------------------------------------
    # 8. /dig shop — Show dig-specific items
    # ------------------------------------------------------------------

    @dig.command(name="shop", description="Browse the mining shop")
    async def dig_shop(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            shop = _wrap(await asyncio.to_thread(
                self.dig_service.get_shop, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Dig shop error: %s", e)
            await safe_followup(interaction, content="Shop unavailable.", ephemeral=True)
            return

        embed = discord.Embed(title="Mining Shop", color=0xD4AF37)

        # Consumables
        consumables = getattr(shop, "consumables", [])
        if consumables:
            cons_text = "\n".join(
                f"**{c.get('name', '?')}** — {c.get('price', '?')} {JOPACOIN_EMOTE}: {c.get('description', '')}"
                for c in consumables
            )
            embed.add_field(name="Consumables", value=cons_text, inline=False)

        # Pickaxe upgrades
        upgrades = getattr(shop, "pickaxe_upgrades", [])
        if upgrades:
            upg_text = "\n".join(
                f"**{u.get('name', '?')}** — {u.get('price', '?')} {JOPACOIN_EMOTE} "
                f"(Depth {u.get('depth_req', '?')}, Prestige {u.get('prestige_req', 0)})"
                for u in upgrades
            )
            embed.add_field(name="Pickaxe Upgrades", value=upg_text, inline=False)

        # Inventory count
        inv_count = getattr(shop, "inventory_count", 0)
        embed.set_footer(text=f"Your inventory: {inv_count}/{MAX_INVENTORY_SLOTS} items | Use /dig buy <item> to purchase, /dig use <item> to queue")

        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 8b. /dig buy — Buy an item from the shop
    # ------------------------------------------------------------------

    @dig.command(name="buy", description="Buy an item from the mining shop")
    @app_commands.describe(item="Item to buy")
    @app_commands.choices(item=[
        app_commands.Choice(name="Dynamite (5 JC)", value="dynamite"),
        app_commands.Choice(name="Hard Hat (8 JC)", value="hard_hat"),
        app_commands.Choice(name="Lantern (4 JC)", value="lantern"),
        app_commands.Choice(name="Reinforcement (6 JC)", value="reinforcement"),
        app_commands.Choice(name="Torch (6 JC)", value="torch"),
        app_commands.Choice(name="Grappling Hook (10 JC)", value="grappling_hook"),
        app_commands.Choice(name="Sonar Pulse (8 JC)", value="sonar_pulse"),
        app_commands.Choice(name="Depth Charge (15 JC)", value="depth_charge"),
        app_commands.Choice(name="Void Bait (20 JC)", value="void_bait"),
    ])
    async def dig_buy(self, interaction: discord.Interaction, item: str):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.buy_item, interaction.user.id, guild_id, item
            ))
        except Exception as e:
            logger.error("Dig buy error: %s", e)
            await safe_followup(interaction, content="Purchase failed.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Purchase failed."),
                ephemeral=True,
            )
            return

        item_name = getattr(result, "item", item)
        cost = getattr(result, "cost", 0)
        balance_after = getattr(result, "balance_after", "?")
        await safe_followup(
            interaction,
            content=(
                f"Purchased **{item_name}** for **{cost}** {JOPACOIN_EMOTE}! "
                f"Balance: **{balance_after}** {JOPACOIN_EMOTE}\n"
                f"Use `/dig use {item}` to queue it for your next dig."
            ),
        )

    # ------------------------------------------------------------------
    # 9. /dig museum — Guild artifact museum
    # ------------------------------------------------------------------

    @dig.command(name="museum", description="View the guild artifact museum")
    async def dig_museum(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            museum = await asyncio.to_thread(
                self.dig_service.get_museum, guild_id
            )
        except Exception as e:
            logger.error("Museum error: %s", e)
            await safe_followup(interaction, content="Museum unavailable.", ephemeral=True)
            return

        # Build pages by layer
        layer_order = ["Dirt", "Stone", "Crystal", "Magma", "Abyss"]
        pages = []
        artifacts_by_layer = museum.get("by_layer", {}) if isinstance(museum, dict) else {}

        for layer_name in layer_order:
            artifacts = artifacts_by_layer.get(layer_name, [])
            embed = discord.Embed(
                title=f"Museum — {layer_name} Layer",
                color=_layer_color(layer_name),
            )
            if artifacts:
                lines = []
                for a in artifacts:
                    if a.get("discovered", False):
                        finder = a.get("first_finder", "Unknown")
                        count = a.get("total_found", 1)
                        lines.append(f"**{a.get('name', '?')}** — First found by {finder} (x{count})")
                    else:
                        lines.append("**???** — Undiscovered")
                embed.description = "\n".join(lines)
            else:
                embed.description = "No artifacts catalogued for this layer yet."
            pages.append(embed)

        if not pages:
            await safe_followup(interaction, content="Museum is empty.", ephemeral=True)
            return

        if len(pages) == 1:
            await safe_followup(interaction, embed=pages[0])
        else:
            view = MuseumView(pages)
            await safe_followup(interaction, embed=pages[0], view=view)

    # ------------------------------------------------------------------
    # 10. /dig_flex — Show stats and titles
    # ------------------------------------------------------------------

    @dig.command(name="flex", description="Show off your mining achievements")
    async def dig_flex(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            flex = _wrap(await asyncio.to_thread(
                self.dig_service.get_flex_data, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Flex error: %s", e)
            await safe_followup(interaction, content="Flex unavailable.", ephemeral=True)
            return

        if not getattr(flex, "success", False):
            await safe_followup(
                interaction,
                content="You don't have a tunnel yet. Use `/dig go` to start!",
                ephemeral=True,
            )
            return

        depth = getattr(flex, "depth", 0)
        total_digs = getattr(flex, "total_digs", 0)
        total_jc = getattr(flex, "total_jc_earned", 0)
        prestige = getattr(flex, "prestige_level", 0)
        streak = getattr(flex, "streak", 0)
        tunnel_name = getattr(flex, "tunnel_name", "Unknown")
        layer = getattr(flex, "layer", "Dirt")
        titles = getattr(flex, "titles", [])
        achievement_count = getattr(flex, "achievement_count", 0)
        prestige_emoji = getattr(flex, "prestige_emoji", "")

        has_anything = depth > 0 or total_digs > 1

        embed = discord.Embed(
            title=f"{interaction.user.display_name}'s Mining Profile",
            color=0xFFD700,
        )

        if not has_anything:
            sad_lines = [
                "Dug once, found nothing but regret.",
                "The tunnel is so shallow a worm filed a noise complaint.",
                "Achievement unlocked: Owning a shovel.",
                "Your tunnel has more cobwebs than depth.",
                "Even the dirt feels sorry for you.",
                "Depth: yes. Impressive: no.",
                "The mine safety inspector gave you a participation trophy.",
                "Your pickaxe is still in the shrinkwrap.",
            ]
            embed.description = f"*{random.choice(sad_lines)}*"
            embed.set_thumbnail(url=interaction.user.display_avatar.url)
            await safe_followup(interaction, embed=embed)
            return

        # Title(s)
        if titles:
            embed.description = f"*\"{' | '.join(titles)}\"*"
        if prestige_emoji:
            embed.description = (embed.description or "") + f"  {prestige_emoji}"

        # Stats
        stats_text = (
            f"Tunnel: **{tunnel_name}**\n"
            f"Depth: **{depth}** ({layer})\n"
            f"Total digs: **{total_digs}**\n"
            f"Total JC earned: **{total_jc}**\n"
            f"Streak: **{streak}** days\n"
            f"Achievements: **{achievement_count}**"
        )
        if prestige:
            stats_text += f"\nPrestige: **{prestige}**"
        embed.add_field(name="Stats", value=stats_text, inline=False)

        embed.set_thumbnail(url=interaction.user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 11. /dig_abandon — Abandon tunnel
    # ------------------------------------------------------------------

    @dig.command(name="abandon", description="Abandon your tunnel (partial refund)")
    async def dig_abandon(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None

        try:
            preview = _wrap(await asyncio.to_thread(
                self.dig_service.preview_abandon, interaction.user.id, guild_id
            ))
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Abandon preview error: %s", e)
            await interaction.response.send_message("Failed.", ephemeral=True)
            return

        refund = getattr(preview, "refund", 0)
        embed = discord.Embed(
            title="Abandon Tunnel?",
            description=(
                f"This will **permanently destroy** your tunnel.\n"
                f"Refund: **{refund}** {JOPACOIN_EMOTE}\n\n"
                "Are you sure?"
            ),
            color=0xFF0000,
        )
        view = ConfirmAbandonView(interaction.user.id, refund)
        await interaction.response.send_message(embed=embed, view=view)
        await view.wait()

        if view.value:
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.abandon_tunnel, interaction.user.id, guild_id
                ))
                actual_refund = getattr(result, "refund", refund)
                await interaction.edit_original_response(
                    content=f"Tunnel abandoned. You received **{actual_refund}** {JOPACOIN_EMOTE}.",
                    embed=None,
                    view=None,
                )
            except Exception as e:
                logger.error("Abandon error: %s", e)
                await interaction.edit_original_response(
                    content="Abandon failed.", embed=None, view=None
                )
        else:
            await interaction.edit_original_response(
                content="Abandon cancelled.", embed=None, view=None
            )

    # ------------------------------------------------------------------
    # 12. /dig upgrade — View/buy pickaxe upgrades
    # ------------------------------------------------------------------

    @dig.command(name="upgrade", description="View or buy pickaxe upgrades")
    async def dig_upgrade(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            info = _wrap(await asyncio.to_thread(
                self.dig_service.get_upgrade_info, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Upgrade info error: %s", e)
            await safe_followup(interaction, content="Upgrade info unavailable.", ephemeral=True)
            return

        current = getattr(info, "current_tier", "Wooden")
        next_tier = getattr(info, "next_tier", None)

        embed = discord.Embed(title="Pickaxe Upgrades", color=0xB0BEC5)
        embed.add_field(name="Current Pickaxe", value=current, inline=True)

        if next_tier:
            reqs = []
            cost = getattr(info, "cost", 0)
            depth_req = getattr(info, "depth_required", 0)
            prestige_req = getattr(info, "prestige_required", 0)
            reqs.append(f"Cost: {cost} {JOPACOIN_EMOTE}")
            reqs.append(f"Depth: {depth_req}")
            if prestige_req:
                reqs.append(f"Prestige: {prestige_req}")
            embed.add_field(
                name=f"Next: {next_tier}",
                value="\n".join(reqs),
                inline=True,
            )
            eligible = getattr(info, "eligible", False)
            if eligible:
                embed.set_footer(text="You meet all requirements!")
            else:
                missing = getattr(info, "missing_requirements", [])
                if missing:
                    embed.set_footer(text=f"Missing: {', '.join(missing)}")
        else:
            embed.add_field(name="Next", value="Max tier reached!", inline=True)

        upgrade_info_dict = {
            "eligible": getattr(info, "eligible", False),
            "next_tier": next_tier,
        }
        view = UpgradeView(self.dig_service, interaction.user.id, guild_id, upgrade_info_dict)
        await safe_followup(interaction, embed=embed, view=view)

    # ------------------------------------------------------------------
    # 13. /dig_trap — Set a trap
    # ------------------------------------------------------------------

    @dig.command(name="trap", description="Set a trap in your tunnel")
    async def dig_trap(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.set_trap, interaction.user.id, guild_id
            ))
            cost = getattr(result, "cost", 0)
            msg = "Trap set!"
            if cost:
                msg += f" (Cost: {cost} {JOPACOIN_EMOTE})"
            await safe_followup(interaction, content=msg)
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Trap error: %s", e)
            await safe_followup(interaction, content="Failed to set trap.", ephemeral=True)

    # ------------------------------------------------------------------
    # 14. /dig_insure — Buy insurance
    # ------------------------------------------------------------------

    @dig.command(name="insure", description="Buy cave-in insurance")
    async def dig_insure(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.buy_insurance, interaction.user.id, guild_id
            ))
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Insurance error: %s", e)
            await safe_followup(interaction, content="Failed to buy insurance.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Failed to buy insurance."),
                ephemeral=True,
            )
            return

        cost = getattr(result, "cost", 0)
        await safe_followup(
            interaction,
            content=(
                f"Insurance purchased for **{cost}** {JOPACOIN_EMOTE}! "
                f"Duration: 24 hours."
            ),
        )

    # ------------------------------------------------------------------
    # 15. /dig_inventory — View items
    # ------------------------------------------------------------------

    @dig.command(name="inventory", description="View your mining inventory")
    async def dig_inventory(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            items = await asyncio.to_thread(
                self.dig_service.get_inventory, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Inventory error: %s", e)
            await safe_followup(interaction, content="Inventory unavailable.", ephemeral=True)
            return

        embed = discord.Embed(title="Mining Inventory", color=0x8B4513)
        if items:
            for item in items[:5]:
                name = item.get("name", "Unknown")
                queued = item.get("queued", False)
                desc = item.get("description", "")
                status = " [QUEUED]" if queued else ""
                embed.add_field(
                    name=f"{name}{status}",
                    value=desc or "No description",
                    inline=False,
                )
            embed.set_footer(text=f"{len(items)}/{MAX_INVENTORY_SLOTS} slots used")
        else:
            embed.description = "Your inventory is empty. Visit `/dig shop` to buy items."
            embed.set_footer(text="0/{MAX_INVENTORY_SLOTS} slots used")

        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 16. /dig guide — Paginated help
    # ------------------------------------------------------------------

    @dig.command(name="guide", description="Learn how to dig")
    async def dig_guide(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        view = DigGuideView()
        await interaction.response.send_message(embed=GUIDE_PAGES[0], view=view)


# ---------------------------------------------------------------------------
# Embed builder for normal dig results
# ---------------------------------------------------------------------------

def _build_dig_embed(result: object, user: discord.User | discord.Member) -> discord.Embed:
    """Build a rich embed for a normal dig result."""
    depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
    tunnel_name = getattr(result, "tunnel_name", "Tunnel")

    # Determine layer for embed color
    layer_def = get_layer_def(depth)
    layer_name = layer_def.name if layer_def else None

    # ~20% chance of a "Dig Dug" themed title
    if random.random() < 0.20:
        title = f"\u26cf\ufe0f {random.choice(DIG_DUG_TITLES)} \u2014 Depth {depth}"
    else:
        title = f"\u26cf\ufe0f {tunnel_name} \u2014 Depth {depth}"

    embed = discord.Embed(
        title=title,
        color=_layer_color(layer_name),
    )

    # Blocks gained and JC earned (skip misleading "+0" during cave-ins)
    cave_in = getattr(result, "cave_in", False)
    blocks = getattr(result, "advance", 0)
    jc = getattr(result, "jc_earned", 0)
    if not cave_in or blocks > 0 or jc > 0:
        embed.add_field(
            name="Progress",
            value=f"+{blocks} blocks | +{jc} {JOPACOIN_EMOTE}",
            inline=False,
        )

    # Cave-in
    cave_in_detail = getattr(result, "cave_in_detail", None)
    if cave_in and cave_in_detail:
        block_loss = getattr(cave_in_detail, "block_loss", "?")
        message = getattr(cave_in_detail, "message", "")
        embed.add_field(
            name="Cave-in!",
            value=f"Lost **{block_loss}** blocks. {message}",
            inline=False,
        )

    # Milestone bonus
    milestone = getattr(result, "milestone_bonus", 0)
    if milestone:
        embed.add_field(
            name="DIG DUG! Milestone!",
            value=f"+{milestone} {JOPACOIN_EMOTE}",
            inline=False,
        )

    # Streak bonus
    streak_bonus = getattr(result, "streak_bonus", 0)
    if streak_bonus:
        embed.add_field(
            name="Streak Bonus",
            value=f"+{streak_bonus} {JOPACOIN_EMOTE}",
            inline=True,
        )

    # Artifact found
    artifact = getattr(result, "artifact", None)
    if artifact:
        a_name = getattr(artifact, "name", "?") if not isinstance(artifact, str) else artifact
        a_desc = getattr(artifact, "description", "") if not isinstance(artifact, str) else ""
        embed.add_field(
            name="Artifact Found!",
            value=f"**{a_name}**" + (f" — {a_desc}" if a_desc else ""),
            inline=False,
        )

    # Event (with ASCII art for simple events)
    event = getattr(result, "event", None)
    if event:
        e_desc = getattr(event, "description", str(event)) if not isinstance(event, str) else event
        e_name = getattr(event, "name", "Event") if not isinstance(event, str) else "Event"
        # Check for ASCII art
        e_art = None
        if isinstance(event, dict):
            e_art = event.get("ascii_art")
        elif hasattr(event, "_d") and isinstance(event._d, dict):
            e_art = event._d.get("ascii_art")
        event_text = e_desc
        if e_art:
            event_text = f"```\n{e_art}\n```\n{e_desc}"
        embed.add_field(
            name=e_name,
            value=event_text,
            inline=False,
        )

    # Items used
    items_used = getattr(result, "items_used", None)
    if items_used:
        item_names = ", ".join(str(i) for i in items_used)
        embed.add_field(name="Items Used", value=item_names, inline=True)

    # Luminosity bar (only shown when draining / below max)
    lum_info = getattr(result, "luminosity_info", None)
    if lum_info:
        lum_after = lum_info.get("luminosity_after", 100) if isinstance(lum_info, dict) else getattr(lum_info, "luminosity_after", 100)
        lum_drained = lum_info.get("drained", 0) if isinstance(lum_info, dict) else getattr(lum_info, "drained", 0)
        if lum_drained > 0 or lum_after < 100:
            filled = max(0, lum_after // 10)
            empty = 10 - filled
            bar = "\u2588" * filled + "\u2591" * empty
            level_name = lum_info.get("level", "bright") if isinstance(lum_info, dict) else getattr(lum_info, "level", "bright")
            level_label = {"bright": "Bright", "dim": "Dim", "dark": "Dark", "pitch_black": "Pitch Black"}.get(level_name, "")
            lum_text = f"`[{bar}]` {lum_after}% — {level_label}"
            if lum_drained > 0:
                lum_text += f" (-{lum_drained})"
            embed.add_field(name="Luminosity", value=lum_text, inline=False)

    # Footer tip — ~25% chance of a Dig Dug footer instead
    if depth == 69:
        embed.set_footer(text="Nice.")
    elif random.random() < 0.25:
        embed.set_footer(text=random.choice(DIG_DUG_FOOTERS))
    else:
        embed.set_footer(text=getattr(result, "tip", "") or _tip(0))
    embed.set_author(name=user.display_name, icon_url=user.display_avatar.url)

    return embed


# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

async def setup(bot: commands.Bot):
    dig_service = getattr(bot, "dig_service", None)
    if dig_service is None:
        raise RuntimeError("Dig service not registered on bot.")
    await bot.add_cog(DigCommands(bot, dig_service))
