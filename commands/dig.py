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
from discord.ext import commands, tasks

from commands.checks import require_dig_channel
from config import DIG_CHANNEL_ID
from services.dig_constants import (
    ASCENSION_MODIFIERS,
    LUMINOSITY_DEEP_DRAIN_START_DEPTH,
    LUMINOSITY_PITCH_BLACK,
    MAX_INVENTORY_SLOTS,
    PICKAXE_TIERS,
    pick_description,
)
from services.dig_constants import get_layer as get_layer_def
from services.permissions import has_admin_permission
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.dig_llm_service import DigLLMService
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")


def _splash_aftermath_lines(splash: dict) -> list[str]:
    """Format splash victim lines for display in embeds.

    Returned as a list so callers can join with newlines or slice for the
    public broadcast vs. the digger's private reply. If an LLM narrative
    is present on the splash dict, it is rendered as the first (italic)
    line above the deterministic per-victim lines.
    """
    victims = splash.get("victims", []) if splash else []
    mode = (splash.get("mode") or "burn") if splash else "burn"
    sign = "+" if mode == "grant" else "-"
    lines: list[str] = []
    narrative = (splash.get("llm_narrative") or "").strip() if splash else ""
    if narrative:
        lines.append(f"*{narrative}*")
    lines.extend(
        f"<@{v['discord_id']}>: {sign}{v['amount']} {JOPACOIN_EMOTE}"
        for v in victims
    )
    return lines


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
        except KeyError as err:
            raise AttributeError(name) from err
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


def _format_s_stats(stats: dict, effects: dict) -> str:
    strength = stats.get("strength", 0)
    smarts = stats.get("smarts", 0)
    stamina = stats.get("stamina", 0)
    total = stats.get("stat_points", 5)
    unspent = stats.get("unspent_points", 0)
    cooldown_multiplier = effects.get("cooldown_multiplier", 1.0)
    reduction = max(0.0, 1.0 - cooldown_multiplier)
    return (
        f"Strength **{strength}** | Smarts **{smarts}** | Stamina **{stamina}**\n"
        f"Points: **{total}** total, **{unspent}** unspent\n"
        f"Effects: +{effects.get('advance_min_bonus', 0)}/"
        f"+{effects.get('advance_max_bonus', 0)} advance range, "
        f"-{effects.get('cave_in_reduction', 0):.0%} cave-in, "
        f"-{reduction:.0%} cooldown/paid costs"
    )


def _backstory_text(result: dict) -> str:
    return result.get("backstory") or "Backstory not set."


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

    def __init__(self, dig_service: DigService, user_id: int, guild_id: int | None, cost: int):
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

    def __init__(self, dig_service: DigService, user_id: int, guild_id: int | None, dig_llm_service=None):
        super().__init__(title="Boss Fight Wager")
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.dig_llm_service = dig_llm_service
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
                self.dig_service.start_boss_duel,
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

            # Mid-fight prompt: boss's rolled mechanic triggered at its round.
            # Hand off to the BossDuelView so the player picks one of 3
            # reactive options before the duel resumes.
            if getattr(self.result, "pending_prompt", None):
                view = BossDuelView(
                    dig_service=self.dig_service,
                    user_id=self.user_id,
                    guild_id=self.guild_id,
                    initial_result=self.result,
                    risk_tier=tier,
                    wager=amount,
                    dig_llm_service=self.dig_llm_service,
                )
                embed = _build_duel_prompt_embed(self.result)
                msg = await interaction.followup.send(embed=embed, view=view, wait=True)
                view.message = msg
                self.stop()
                return

            # Phase 2 incoming — fake victory then reveal transform
            if getattr(self.result, "phase2_incoming", False):
                phase2_name = getattr(self.result, "phase2_name", "???")
                phase2_title = getattr(self.result, "phase2_title", "")
                p2_dialogue = getattr(self.result, "dialogue", "...")
                boss_name = getattr(self.result, "boss_name", "the boss")

                victory_embed = discord.Embed(
                    title="Victory!",
                    description=f"You defeated **{boss_name}**!",
                    color=0x00FF00,
                )
                msg = await interaction.followup.send(embed=victory_embed, wait=True)

                await asyncio.sleep(2)

                phase2_embed = discord.Embed(
                    title=f"{phase2_title or phase2_name} Emerges!",
                    description=(
                        f"Wait... **{boss_name}** is transforming!\n\n"
                        f"**{phase2_name}**\n"
                        f"*{p2_dialogue}*\n\n"
                        "The fight continues!"
                    ),
                    color=0x8B0000,
                )
                phase2_embed.set_footer(text="Use /dig go to encounter the boss again")

                # Try to load phase 2 boss art — prefer the locked boss_id,
                # fall back to the depth boundary for the grandfathered slug.
                p2_file = None
                boundary = getattr(self.result, "boundary", None)
                boss_id = getattr(self.result, "boss_id", "") or boundary
                if boss_id:
                    try:
                        from utils.dig_assets import get_boss_art
                        new_depth = getattr(self.result, "new_depth", 0)
                        ld = get_layer_def(new_depth or boundary)
                        ln = ld.name if ld else "Dirt"
                        p2_file = await asyncio.to_thread(get_boss_art, boss_id, "phase2", ln)
                    except Exception:
                        pass

                if p2_file:
                    phase2_embed.set_image(url=f"attachment://{p2_file.filename}")
                    await msg.edit(embed=phase2_embed, attachments=[p2_file])
                else:
                    await msg.edit(embed=phase2_embed)
                return

            # LLM narrative for the boss fight (best-effort)
            boss_narrative = None
            result_dict = self.result._d if hasattr(self.result, "_d") else {}
            if self.dig_llm_service and result_dict:
                try:
                    engine_mode = await asyncio.to_thread(
                        self.dig_service.dig_repo.get_engine_mode,
                        self.user_id, self.guild_id,
                    )
                    if engine_mode == "llm":
                        boss_narrative = await self.dig_llm_service.narrate_boss_fight(
                            result_dict, self.user_id, self.guild_id,
                        )
                except Exception:
                    logger.debug("Boss fight narration failed", exc_info=True)

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
                if boss_narrative:
                    embed.add_field(name="\u200b", value=f"*{boss_narrative}*", inline=False)
                if getattr(self.result, "stat_point_awarded", False):
                    embed.add_field(
                        name="S Point Earned",
                        value="First clear bonus: use `/dig miner build` to allocate it.",
                        inline=False,
                    )
            else:
                loss = abs(getattr(self.result, "jc_delta", 0)) or amount
                knockback = getattr(self.result, "knockback", 0)
                embed.description = (
                    f"Defeat! **{boss_name}** overpowered you. "
                    f"You lost **{loss}** {JOPACOIN_EMOTE}"
                    f" and were knocked back {knockback} blocks."
                )
                if boss_narrative:
                    embed.add_field(name="\u200b", value=f"*{boss_narrative}*", inline=False)
            embed.add_field(
                name="Details",
                value=(
                    f"Risk: {tier.title()} | Pre-fight win chance: "
                    f"{int(win_chance * 100)}%"
                ),
                inline=False,
            )

            if getattr(self.result, "echo_applied", False):
                killer_id = getattr(self.result, "echo_killer_id", None)
                killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
                embed.add_field(
                    name="Echoing in the Tunnels",
                    value=(
                        f"{killer_mention} killed this boss recently. "
                        "It came in weakened (-25% HP, -30% payout)."
                    ),
                    inline=False,
                )

            # Try to load boss fight result art — prefer the locked boss_id,
            # fall back to the depth boundary for the grandfathered slug.
            boss_file = None
            boundary = getattr(self.result, "boundary", None)
            boss_id = getattr(self.result, "boss_id", "") or boundary
            won = getattr(self.result, "won", False)
            if boss_id:
                try:
                    from utils.dig_assets import get_boss_art
                    new_depth = getattr(self.result, "new_depth", 0)
                    ld = get_layer_def(new_depth or boundary)
                    ln = ld.name if ld else "Dirt"
                    scene = "victory" if won else "defeat"
                    boss_file = await asyncio.to_thread(get_boss_art, boss_id, scene, ln)
                except Exception as e:
                    logger.debug("Boss fight art failed: %s", e)

            if boss_file:
                embed.set_image(url=f"attachment://{boss_file.filename}")
                await interaction.followup.send(embed=embed, file=boss_file)
            else:
                await interaction.followup.send(embed=embed)
        except Exception as e:
            logger.error("Boss fight error: %s", e, exc_info=True)
            await interaction.followup.send("Boss fight failed. Try again.", ephemeral=True)


def _build_duel_prompt_embed(result) -> discord.Embed:
    """Embed rendered alongside a BossDuelView's three reactive option buttons.

    Accepts either the raw dict returned by ``start_boss_duel`` /
    ``resume_boss_duel`` or a ``_wrap``'d object.
    """
    raw = result._d if hasattr(result, "_d") else (result if isinstance(result, dict) else {})
    pp = raw.get("pending_prompt") or {}
    boss_name = raw.get("boss_name") or "the boss"
    player_hp = raw.get("player_hp", 0)
    boss_hp = raw.get("boss_hp", 0)
    round_num = raw.get("round_num", 0)
    is_pinnacle = bool(raw.get("is_pinnacle"))
    phase = raw.get("phase")
    phase_total = raw.get("phase_total")

    title = pp.get("prompt_title") or f"{boss_name} acts"
    description = pp.get("prompt_description") or ""

    header_parts = [boss_name, f"Round {round_num}"]
    if is_pinnacle and phase and phase_total:
        header_parts.insert(1, f"Phase {phase}/{phase_total}")
    header = " — ".join(header_parts)

    embed = discord.Embed(
        title=header,
        description=f"**{title}**\n*{description}*",
        color=0xB22222 if is_pinnacle else 0xFFD700,
    )
    embed.add_field(
        name="State",
        value=f"You: **{player_hp}** HP  |  {boss_name}: **{boss_hp}** HP",
        inline=False,
    )
    lum_line = raw.get("luminosity_display")
    if lum_line:
        embed.add_field(name="​", value=lum_line, inline=False)
    challenge = raw.get("timed_challenge")
    if challenge:
        kind = challenge.get("kind", "challenge")
        question = challenge.get("question", "?")
        window = int(challenge.get("time_window_seconds", 0))
        embed.add_field(
            name=("Math" if kind == "arithmetic" else ("Riddle" if kind == "riddle" else "Challenge")),
            value=(
                f"```{question}```\n"
                f"Click **Solve** and type your answer. Within **{window}s** for the best outcome."
            ),
            inline=False,
        )
    else:
        opts = pp.get("options") or []
        if opts:
            lines = [f"**{o['option_idx'] + 1}.** {o['label']}" for o in opts]
            embed.add_field(
                name="Your choice",
                value=(
                    "\n".join(lines)
                    + "\n\n*(120s before the safe option is auto-picked.)*"
                ),
                inline=False,
            )
    return embed


async def _load_boss_result_art(result) -> discord.File | None:
    """Resolve the victory/defeat art for a boss-fight result, or None.

    Mirrors the boss_id → boundary fallback used elsewhere so grandfathered
    bosses without a locked id still resolve their slug.
    """
    boundary = getattr(result, "boundary", None)
    boss_id = getattr(result, "boss_id", "") or boundary
    if not boss_id:
        return None
    try:
        from utils.dig_assets import get_boss_art
        # boss_progress JSON keys are stored as strings; coerce so
        # get_layer_def's int comparison doesn't blow up on a "25"-shaped
        # boundary and silently fall back to no art.
        try:
            depth_for_layer = (
                int(getattr(result, "new_depth", 0) or 0)
                or int(boundary or 0)
            )
        except (TypeError, ValueError):
            depth_for_layer = 0
        ld = get_layer_def(depth_for_layer)
        ln = ld.name if ld else "Dirt"
        scene = "victory" if getattr(result, "won", False) else "defeat"
        return await asyncio.to_thread(get_boss_art, boss_id, scene, ln)
    except Exception as e:
        logger.debug("Boss fight art failed: %s", e)
        return None


def _build_boss_fight_result_embed(*, result, risk_tier: str, amount: int) -> discord.Embed:
    """Shared post-duel result embed — used by the modal's no-prompt path and
    by ``BossDuelView`` after the final option click / timeout."""
    won = getattr(result, "won", False)
    boss_name = getattr(result, "boss_name", "the boss")
    win_chance = getattr(result, "win_chance", 0) or 0
    embed = discord.Embed(
        title="Boss Fight Result",
        color=0x00FF00 if won else 0xFF0000,
    )
    if won:
        payout = getattr(result, "payout", 0) or getattr(result, "jc_delta", 0)
        embed.description = (
            f"Victory! You defeated **{boss_name}** and earned "
            f"**{payout}** {JOPACOIN_EMOTE}!"
        )
        if getattr(result, "stat_point_awarded", False):
            embed.add_field(
                name="S Point Earned",
                value="First clear bonus: use `/dig miner build` to allocate it.",
                inline=False,
            )
    else:
        loss = abs(getattr(result, "jc_delta", 0)) or amount
        knockback = getattr(result, "knockback", 0)
        embed.description = (
            f"Defeat! **{boss_name}** overpowered you. "
            f"You lost **{loss}** {JOPACOIN_EMOTE} and were knocked back {knockback} blocks."
        )

    # Surface the mid-fight option the player picked plus its rolled narrative.
    # Without this, picking a button on the reactive prompt jumps straight to
    # a generic "Defeat!" and the player has no idea what happened.
    raw = result._d if hasattr(result, "_d") else (result if isinstance(result, dict) else {})
    round_log = raw.get("round_log") or []
    mechanic_entry = next(
        (e for e in reversed(round_log) if isinstance(e, dict) and e.get("mechanic_id")),
        None,
    )
    if mechanic_entry:
        option_label = mechanic_entry.get("option_label") or "Your choice"
        narrative = mechanic_entry.get("narrative") or ""
        if narrative:
            embed.add_field(
                name=f"You chose: {option_label}",
                value=narrative,
                inline=False,
            )
        extra_kb = getattr(result, "extra_knockback", 0)
        extra_cd = getattr(result, "extra_cooldown_s", 0)
        if extra_kb or extra_cd:
            parts = []
            if extra_kb:
                parts.append(f"+{extra_kb} extra knockback")
            if extra_cd:
                parts.append(f"+{extra_cd // 60}m extra cooldown")
            embed.add_field(
                name="Loss Penalty", value="; ".join(parts), inline=False,
            )
    embed.add_field(
        name="Details",
        value=(
            f"Risk: {risk_tier.title()} | Pre-fight win chance: "
            f"{int(win_chance * 100)}%"
        ),
        inline=False,
    )
    lum_line = getattr(result, "luminosity_display", None)
    if lum_line:
        embed.add_field(name="​", value=lum_line, inline=False)
    if getattr(result, "echo_applied", False):
        killer_id = getattr(result, "echo_killer_id", None)
        killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
        embed.add_field(
            name="Echoing in the Tunnels",
            value=(
                f"{killer_mention} killed this boss recently. "
                "It came in weakened (-25% HP, -30% payout)."
            ),
            inline=False,
        )
    # Pinnacle-specific surfaces: phase transition or relic drop.
    if getattr(result, "is_pinnacle", False):
        if getattr(result, "phase2_incoming", False) or getattr(result, "phase3_incoming", False):
            next_title = getattr(result, "next_phase_title", None)
            event_flavor = getattr(result, "phase_event_flavor", "")
            event_desc = getattr(result, "phase_event_description", "")
            value_lines = []
            if next_title:
                value_lines.append(f"Next: **{next_title}**")
            if event_flavor:
                value_lines.append(f"*{event_flavor}*")
            if event_desc:
                value_lines.append(event_desc)
            if value_lines:
                embed.add_field(
                    name="Phase shift",
                    value="\n".join(value_lines),
                    inline=False,
                )
        if getattr(result, "pinnacle_defeated", False):
            relic = getattr(result, "pinnacle_relic", None) or {}
            if relic:
                embed.add_field(
                    name=f"Relic: {relic.get('name', '?')}",
                    value="\n".join(f"• {s}" for s in (relic.get("stats") or [])) or "—",
                    inline=False,
                )
    # Timed-input resolution surface (which option the answer mapped to).
    challenge_res = getattr(result, "timed_challenge_resolution", None)
    if challenge_res:
        if challenge_res.get("correct"):
            tag = "in time" if challenge_res.get("in_time") else "late"
            embed.add_field(
                name="Answer",
                value=f"`{challenge_res.get('submitted')}` — correct, {tag}.",
                inline=False,
            )
        else:
            expected = challenge_res.get("expected_answer")
            embed.add_field(
                name="Answer",
                value=(
                    f"`{challenge_res.get('submitted')}` — wrong."
                    + (f" The right answer was `{expected}`." if expected else "")
                ),
                inline=False,
            )
    return embed


class _TimedAnswerModal(discord.ui.Modal):
    """Text-input modal for arithmetic / riddle pinnacle prompts.

    The user types their answer; on submit, we forward it to the
    enclosing ``BossDuelView`` which calls ``submit_timed_answer`` on the
    service. The service evaluates correctness + elapsed time and maps
    that to one of the mechanic's three options.
    """

    def __init__(self, *, view: BossDuelView, question: str, kind: str, window_seconds: int):
        title_kind = "Math" if kind == "arithmetic" else ("Riddle" if kind == "riddle" else "Answer")
        super().__init__(title=f"{title_kind} ({window_seconds}s window)"[:45])
        self._view = view
        self._answer = discord.ui.TextInput(
            label=question[:45] or "Your answer",
            placeholder=question[:100] if len(question) > 45 else "Type your answer here",
            required=True,
            max_length=80,
        )
        self.add_item(self._answer)

    async def on_submit(self, interaction: discord.Interaction) -> None:
        await interaction.response.defer()
        await self._view._submit_timed(self._answer.value or "")


class BossDuelView(discord.ui.View):
    """Interactive view for a paused boss duel.

    Rendered when ``start_boss_duel`` (or ``resume_boss_duel``) returns a
    ``pending_prompt``. Creates one button per option in the mechanic's
    three-reactive-option prompt. Click rolls the option's distribution and
    resumes the duel. Timeout (120s) auto-picks the mechanic's designated
    safe option.
    """

    def __init__(
        self,
        *,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        initial_result,
        risk_tier: str,
        wager: int,
        dig_llm_service=None,
    ):
        super().__init__(timeout=120)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.risk_tier = risk_tier
        self.wager = wager
        self.dig_llm_service = dig_llm_service
        self.message: discord.Message | None = None
        self._resolved = False

        raw = (
            initial_result._d if hasattr(initial_result, "_d")
            else (initial_result if isinstance(initial_result, dict) else {})
        )
        pp = raw.get("pending_prompt") or {}
        self._safe_option_idx = int(pp.get("safe_option_idx", 0))
        # Timed-input prompts (arithmetic / riddle) render a single
        # "Solve" button that pops a modal — not three option buttons.
        challenge = raw.get("timed_challenge")
        self._timed_challenge = challenge
        if challenge:
            kind = challenge.get("kind", "challenge")
            window = int(challenge.get("time_window_seconds", 0))
            label = f"Solve ({window}s window)"
            btn = discord.ui.Button(
                label=label[:80],
                style=discord.ButtonStyle.success,
                custom_id="duel_timed_open",
            )
            btn.callback = self._make_timed_callback(kind)
            self.add_item(btn)
        else:
            for opt in pp.get("options", []):
                idx = int(opt.get("option_idx", 0))
                label = (opt.get("label") or f"Option {idx + 1}")[:80]
                btn = discord.ui.Button(
                    label=label,
                    style=discord.ButtonStyle.primary,
                    custom_id=f"duel_opt_{idx}",
                )
                btn.callback = self._make_callback(idx)
                self.add_item(btn)

    def _make_callback(self, option_idx: int):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message(
                    "Only the duelist can choose.", ephemeral=True,
                )
                return
            await interaction.response.defer()
            await self._submit(option_idx)
        return callback

    def _make_timed_callback(self, kind: str):
        """Open a TextInput modal for arithmetic / riddle prompts."""
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message(
                    "Only the duelist can answer.", ephemeral=True,
                )
                return
            challenge = self._timed_challenge or {}
            modal = _TimedAnswerModal(
                view=self,
                question=challenge.get("question", "?"),
                kind=kind,
                window_seconds=int(challenge.get("time_window_seconds", 0)),
            )
            await interaction.response.send_modal(modal)
        return callback

    async def _submit_timed(self, answer_text: str) -> None:
        if self._resolved:
            return
        self._resolved = True
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.submit_timed_answer,
                self.user_id, self.guild_id, answer_text,
            ))
        except Exception as e:
            logger.error("Timed answer submit failed: %s", e, exc_info=True)
            await self._edit_message(content="Timed answer failed.", embed=None, view=None)
            return
        if not getattr(result, "success", True):
            err = getattr(result, "error", "Timed answer failed.")
            await self._edit_message(content=err, embed=None, view=None)
            return
        await self._render_resolution(result)

    async def on_timeout(self):
        if self._resolved:
            return
        await self._submit(self._safe_option_idx)

    async def _submit(self, option_idx: int) -> None:
        if self._resolved:
            return
        self._resolved = True
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.resume_boss_duel,
                self.user_id, self.guild_id, option_idx,
            ))
        except Exception as e:
            logger.error("Boss duel resume failed: %s", e, exc_info=True)
            await self._edit_message(content="Duel resume failed.", embed=None, view=None)
            return
        if not getattr(result, "success", True):
            err = getattr(result, "error", "Duel resume failed.")
            await self._edit_message(content=err, embed=None, view=None)
            return
        await self._render_resolution(result)

    async def _render_resolution(self, result) -> None:
        """Render either a follow-up prompt or the final fight outcome."""
        if getattr(result, "pending_prompt", None):
            new_view = BossDuelView(
                dig_service=self.dig_service,
                user_id=self.user_id, guild_id=self.guild_id,
                initial_result=result,
                risk_tier=self.risk_tier, wager=self.wager,
                dig_llm_service=self.dig_llm_service,
            )
            embed = _build_duel_prompt_embed(result)
            await self._edit_message(embed=embed, view=new_view)
            new_view.message = self.message
            self.stop()
            return

        embed = _build_boss_fight_result_embed(
            result=result, risk_tier=self.risk_tier, amount=self.wager,
        )
        boss_file = await _load_boss_result_art(result)
        if boss_file:
            embed.set_image(url=f"attachment://{boss_file.filename}")
            await self._edit_message(embed=embed, view=None, attachments=[boss_file])
        else:
            await self._edit_message(embed=embed, view=None)
        self.stop()

    async def _edit_message(self, **kwargs) -> None:
        """Edit the duel's original message, falling back to channel.send().

        The timeout path fires after Discord's interaction token is already
        expired (>15 min), at which point ``message.edit`` raises
        ``discord.HTTPException``. Without the fallback the user sees stale
        buttons forever. Logged at WARNING so recurring failures are visible.
        """
        if self.message is None:
            return
        try:
            await self.message.edit(**kwargs)
            return
        except Exception as e:
            logger.warning("BossDuelView message edit failed: %s", e)
        channel = getattr(self.message, "channel", None)
        if channel is None:
            return
        embed = kwargs.get("embed")
        content = kwargs.get("content")
        attachments = kwargs.get("attachments") or []
        files = [a for a in attachments if isinstance(a, discord.File)]
        try:
            if embed is not None:
                if files:
                    await channel.send(embed=embed, files=files)
                else:
                    await channel.send(embed=embed)
            elif content:
                await channel.send(content=content)
        except Exception as e:
            logger.warning("BossDuelView channel fallback also failed: %s", e)


class EventEncounterView(discord.ui.View):
    """Interactive view for choice/complex events with safe and risky buttons."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        event_data: dict,
        luminosity: int = 100,
        target_channel: discord.abc.Messageable | None = None,
        dig_llm_service: DigLLMService | None = None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.event_data = event_data
        self.target_channel = target_channel
        self.dig_llm_service = dig_llm_service
        safe_label = "Play it safe"
        risky_label = "Take the risk"
        if isinstance(event_data, dict):
            safe_opt = event_data.get("safe_option")
            risky_opt = event_data.get("risky_option")
            if isinstance(safe_opt, dict):
                safe_label = safe_opt.get("label", safe_label)
            if isinstance(risky_opt, dict):
                risky_label = risky_opt.get("label", risky_label)
            # Add desperate button if event has a desperate option
            desperate_opt = event_data.get("desperate_option")
            if isinstance(desperate_opt, dict) and desperate_opt:
                desperate_label = desperate_opt.get("label", "Desperate gamble")[:80]
                desperate_btn = discord.ui.Button(
                    label=desperate_label,
                    style=discord.ButtonStyle.danger,
                    emoji="\U0001f480",
                    custom_id="event_desperate",
                )
                desperate_btn.callback = self._desperate_callback
                self.add_item(desperate_btn)
        # Pitch black: disable safe option, force risky
        if luminosity <= LUMINOSITY_PITCH_BLACK:
            safe_label = "Darkness consumes safety"
            self.safe_btn.disabled = True
            self.safe_btn.style = discord.ButtonStyle.secondary
        self.safe_btn.label = safe_label[:80]
        self.risky_btn.label = risky_label[:80]

    async def _send_result(self, interaction: discord.Interaction, embed: discord.Embed) -> None:
        if self.target_channel is not None:
            try:
                await self.target_channel.send(embed=embed)
                return
            except Exception as exc:
                logger.warning("Choice-event result send to dig channel failed: %s", exc)
        await interaction.followup.send(embed=embed)

    async def _desperate_callback(self, interaction: discord.Interaction):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        await interaction.response.defer()
        result = await self._resolve("desperate")
        await self._send_result(interaction, result)
        self.stop()

    @discord.ui.button(label="Safe", style=discord.ButtonStyle.secondary, emoji="\U0001f6e1\ufe0f")
    async def safe_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        await interaction.response.defer()
        result = await self._resolve("safe")
        await self._send_result(interaction, result)
        self.stop()

    @discord.ui.button(label="Risky", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def risky_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        await interaction.response.defer()
        result = await self._resolve("risky")
        await self._send_result(interaction, result)
        self.stop()

    async def _resolve(self, choice: str) -> discord.Embed:
        """Resolve event choice via service layer (handles chaining, cruel echoes, logging)."""
        event = self.event_data
        if not isinstance(event, dict):
            return discord.Embed(title="Event", description="Nothing happened.", color=0x808080)

        event_id = event.get("id", "")
        result = _wrap(await asyncio.to_thread(
            self.dig_service.resolve_event, self.user_id, self.guild_id, event_id, choice
        ))
        if not getattr(result, "success", True):
            return discord.Embed(
                title="Event Failed",
                description=getattr(result, "error", "Something went wrong."),
                color=0xFF4444,
            )

        jc = getattr(result, "jc_delta", 0)
        advance = getattr(result, "depth_delta", 0)
        msg = getattr(result, "message", "Something happened.")
        succeeded = getattr(result, "succeeded", True)
        cave_in = getattr(result, "cave_in", False)
        cruel = getattr(result, "cruel_echoes", False)

        color = 0xFF4444 if (not succeeded or cruel or cave_in) else 0x00FF00
        embed = discord.Embed(
            description=msg,
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

        boss_encounter = getattr(result, "boss_encounter", False)
        boss_info = getattr(result, "boss_info", None)
        if boss_encounter:
            boss_name = getattr(boss_info, "name", "Unknown Boss") if boss_info else "Unknown Boss"
            boundary = getattr(boss_info, "boundary", None) if boss_info else None
            path_text = f"Depth {boundary}" if boundary is not None else "the next layer"
            embed.add_field(
                name="Boss Encountered",
                value=f"**{boss_name}** blocks {path_text}. Use `/dig` to fight, scout, or retreat.",
                inline=False,
            )

        # Show buff if granted
        buff = getattr(result, "buff_applied", None)
        if buff:
            buff_d = buff if isinstance(buff, dict) else (buff._d if hasattr(buff, "_d") else {})
            embed.add_field(
                name=f"Buff: {buff_d.get('name', '?')}",
                value=f"Active for {buff_d.get('duration_digs', 0)} digs",
                inline=True,
            )

        # Show chain event if triggered (P7+)
        chain = getattr(result, "chain_event", None)
        if chain:
            chain_d = chain if isinstance(chain, dict) else (chain._d if hasattr(chain, "_d") else {})
            if chain_d:
                embed.add_field(
                    name="\u200b",
                    value=pick_description(chain_d) or "Another event triggers!",
                    inline=False,
                )

        # Splash: surface the aftermath inline on the digger's embed.
        # The digger's result is itself public, so victims who follow
        # the dig channel see the Aftermath field — no separate broadcast.
        splash_obj = getattr(result, "splash", None)
        splash_d = splash_obj._d if hasattr(splash_obj, "_d") else splash_obj
        if isinstance(splash_d, dict) and splash_d.get("victims"):
            if self.dig_llm_service is not None:
                depth_after = getattr(result, "depth_after", 0) or getattr(result, "depth", 0)
                layer_def = get_layer_def(int(depth_after) if depth_after else 0)
                digger_layer = layer_def.name if layer_def else "Dirt"
                narrative = await self.dig_llm_service.narrate_splash(
                    digger_id=self.user_id,
                    guild_id=self.guild_id or 0,
                    event_name=event.get("name", "Unknown Event"),
                    event_description=msg,
                    splash_mode=splash_d.get("mode", "burn"),
                    victims=splash_d.get("victims", []),
                    digger_layer=digger_layer,
                )
                if narrative:
                    splash_d["llm_narrative"] = narrative
            aftermath_lines = _splash_aftermath_lines(splash_d)
            if aftermath_lines:
                embed.add_field(
                    name="Aftermath",
                    value="\n".join(aftermath_lines),
                    inline=False,
                )

        return embed


class BoonSelectionView(discord.ui.View):
    """View for boon events — player picks one of 2-3 buffs."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        event_data: dict,
        target_channel: discord.abc.Messageable | None = None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.event_data = event_data
        self.target_channel = target_channel
        boons = event_data.get("boon_options", []) if isinstance(event_data, dict) else []
        for i, boon in enumerate(boons[:5]):
            label = boon.get("name", f"Boon {i + 1}")[:80]
            btn = discord.ui.Button(
                label=label,
                style=discord.ButtonStyle.primary,
                custom_id=f"boon_select_{i}",
            )
            btn.callback = self._make_callback(i, boon)
            self.add_item(btn)

    def _make_callback(self, index: int, boon: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your event.", ephemeral=True)
                return
            await interaction.response.defer()
            event_id = self.event_data.get("id", "") if isinstance(self.event_data, dict) else ""
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.resolve_event,
                    self.user_id, self.guild_id, event_id, f"boon_{index}",
                ))
                if not getattr(result, "success", True):
                    await interaction.followup.send(
                        getattr(result, "error", "Boon selection failed."), ephemeral=True
                    )
                    return
                embed = discord.Embed(
                    title=self.event_data.get("name", "Boon") if isinstance(self.event_data, dict) else "Boon",
                    description=getattr(result, "message", f"You chose {boon.get('name', 'a boon')}!"),
                    color=0x5865F2,
                )
                buff = getattr(result, "buff_applied", None)
                if buff:
                    buff_d = buff if isinstance(buff, dict) else (buff._d if hasattr(buff, "_d") else {})
                    embed.add_field(
                        name=f"Buff: {buff_d.get('name', '?')}",
                        value=f"Active for {buff_d.get('duration_digs', 0)} digs",
                        inline=True,
                    )
                if self.target_channel is not None:
                    try:
                        await self.target_channel.send(embed=embed)
                    except Exception as exc:
                        logger.warning("Boon result send to dig channel failed: %s", exc)
                        await interaction.followup.send(embed=embed)
                else:
                    await interaction.followup.send(embed=embed)
            except Exception as e:
                logger.error("Boon selection error: %s", e, exc_info=True)
                await interaction.followup.send("Boon selection failed.", ephemeral=True)
            self.stop()
        return callback


class BossEncounterView(discord.ui.View):
    """View for boss encounter interactions."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        boss_info: object,
        has_lantern: bool = False,
        dig_llm_service=None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.boss_info = boss_info
        self.has_lantern = has_lantern
        self.dig_llm_service = dig_llm_service
        if not has_lantern:
            self.scout.disabled = True

    @discord.ui.button(label="Fight", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def fight(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            # Others can cheer, not fight
            await interaction.response.send_message("Only the tunnel owner can fight.", ephemeral=True)
            return
        modal = BossWagerModal(self.dig_service, self.user_id, self.guild_id, dig_llm_service=self.dig_llm_service)
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
            logger.error("Boss retreat error: %s", e, exc_info=True)
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
            if getattr(info, "echo_applied", False):
                killer_id = getattr(info, "echo_killer_id", None)
                killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
                lines.append(
                    f"*Weakened — {killer_mention} killed this boss in the last 24h. "
                    "(-25% HP, -30% payout)*\n"
                )
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

            # Great Lantern tier: additionally show the mechanic pool + stinger
            # warning so the player can plan counters and inventory.
            enhanced = getattr(info, "enhanced", False)
            mech_pool = getattr(info, "mechanic_pool", None)
            stinger = getattr(info, "stinger", None)
            if enhanced and (mech_pool or stinger):
                lines.append("\n_Great Lantern reveal_")
                if mech_pool:
                    lines.append("**Possible mid-fight mechanics** (one rolls per fight):")
                    for m in mech_pool:
                        lines.append(f"  • _{m.get('prompt_title', m.get('id', ''))}_")
                if stinger:
                    kb = stinger.get("extra_knockback", 0)
                    cd = stinger.get("extended_cooldown_s", 0)
                    curse = stinger.get("cursed_status")
                    bits = []
                    if kb:
                        bits.append(f"+{kb} extra knockback")
                    if cd:
                        bits.append(f"+{cd // 60}m extra cooldown")
                    if curse:
                        bits.append(f"curse: `{curse}`")
                    tail = f" ({'; '.join(bits)})" if bits else ""
                    lines.append(
                        "**On-loss stinger:** "
                        f"_{stinger.get('flavor_on_loss', '')}_" + tail
                    )
            embed = discord.Embed(
                title="Boss Scouted" + (" (Great Lantern)" if enhanced else ""),
                description="\n".join(lines),
                color=0xFFD700,
            )
            await interaction.followup.send(embed=embed, ephemeral=True)
        except Exception as e:
            logger.error("Boss scout error: %s", e, exc_info=True)
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
            logger.error("Boss cheer error: %s", e, exc_info=True)
            await interaction.followup.send("Cheer failed.", ephemeral=True)


class PrestigePerksView(discord.ui.View):
    """View for selecting prestige perks."""

    def __init__(
        self,
        dig_service: DigService,
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
                mutation_choice = getattr(self, "_mutation_choice", None)
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.prestige,
                    self.user_id,
                    self.guild_id,
                    perk.get("id", index),
                    mutation_choice,
                ))
                new_level = getattr(result, "prestige_level", 0)
                run_score = getattr(result, "run_score", 0)
                best_score = getattr(result, "best_run_score", 0)
                desc_parts = [
                    f"You selected **{perk.get('name', 'Unknown')}**.",
                    f"Run Score: **{run_score}** (Best: {best_score})",
                    getattr(result, "message", "Your tunnel has been reset. Dig deeper!"),
                ]
                embed = discord.Embed(
                    title=f"Prestige {new_level} Complete!",
                    description="\n".join(desc_parts),
                    color=0xFFD700,
                )

                # Show ascension modifier unlocked at this level
                ascension = getattr(result, "ascension_unlocked", None)
                if ascension:
                    asc_d = ascension if isinstance(ascension, dict) else (ascension._d if hasattr(ascension, "_d") else {})
                    embed.add_field(
                        name=f"Ascension Unlocked: {asc_d.get('name', '?')}",
                        value=f"Penalty: {asc_d.get('penalty', '?')}\nReward: {asc_d.get('reward', '?')}",
                        inline=False,
                    )

                # Show mutation info for P8+
                mutations = getattr(result, "mutations", None)
                if mutations:
                    mut_d = mutations if isinstance(mutations, dict) else (mutations._d if hasattr(mutations, "_d") else {})
                    forced = mut_d.get("forced") if isinstance(mut_d, dict) else None
                    chosen = mut_d.get("chosen") if isinstance(mut_d, dict) else None
                    mut_lines = []
                    if forced:
                        f_d = forced if isinstance(forced, dict) else (forced._d if hasattr(forced, "_d") else {})
                        mut_lines.append(f"Forced: **{f_d.get('name', '?')}** — {f_d.get('description', '')}")
                    if chosen:
                        c_d = chosen if isinstance(chosen, dict) else (chosen._d if hasattr(chosen, "_d") else {})
                        mut_lines.append(f"Chosen: **{c_d.get('name', '?')}** — {c_d.get('description', '')}")
                    if mut_lines:
                        embed.add_field(name="Mutations", value="\n".join(mut_lines), inline=False)

                # Detailed embed (perk + run score + ascension unlock) is
                # for the prestiger only — keeps progression details private.
                await interaction.followup.send(embed=embed, ephemeral=True)
                # Public ascension announcement: terse, atmospheric, no
                # perk or score reveal. Routes to dig channel when set.
                await self._announce_ascension_publicly(interaction)
            except ValueError as e:
                await interaction.followup.send(str(e), ephemeral=True)
            except Exception as e:
                logger.error("Prestige error: %s", e)
                await interaction.followup.send("Prestige failed.", ephemeral=True)
            self.stop()

        return callback

    async def _announce_ascension_publicly(
        self, interaction: discord.Interaction,
    ) -> None:
        """Post a terse, flavor-only ascension line to the dig channel."""
        member = None
        if interaction.guild is not None:
            member = interaction.guild.get_member(self.user_id)
        name = member.display_name if member else f"<@{self.user_id}>"
        text = f"*{name} has ascended.*"

        target: discord.abc.Messageable | None = None
        if DIG_CHANNEL_ID:
            try:
                channel = interaction.client.get_channel(DIG_CHANNEL_ID)
                if channel is None:
                    channel = await interaction.client.fetch_channel(DIG_CHANNEL_ID)
                if isinstance(channel, discord.TextChannel) and (
                    interaction.guild is None
                    or channel.guild.id == interaction.guild.id
                ):
                    perms = channel.permissions_for(channel.guild.me)
                    if perms.send_messages:
                        target = channel
            except Exception as exc:
                logger.warning("Cannot fetch dig channel for ascension: %s", exc)
        if target is None:
            target = interaction.channel
        if target is None:
            return
        try:
            await target.send(text)
        except Exception:
            logger.warning("Ascension announcement failed", exc_info=True)


class MutationSelectionView(discord.ui.View):
    """View for choosing a mutation during P8+ prestige.

    After the player picks a mutation, this view sets the choice on the
    paired PrestigePerksView and sends that view for the perk selection
    step.
    """

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        forced: dict,
        choices: list[dict],
        perks_view: PrestigePerksView,
        perks_embed: discord.Embed,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.forced = forced
        self.choices = choices
        self.perks_view = perks_view
        self.perks_embed = perks_embed
        for i, mut in enumerate(choices[:5]):
            label = mut.get("name", f"Mutation {i + 1}")[:80]
            btn = discord.ui.Button(
                label=label,
                style=discord.ButtonStyle.primary,
                custom_id=f"mutation_select_{i}",
            )
            btn.callback = self._make_callback(i, mut)
            self.add_item(btn)

    def _make_callback(self, index: int, mut: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your prestige.", ephemeral=True)
                return
            await interaction.response.defer()
            # Store the mutation choice on the perk view so it can pass it to prestige()
            self.perks_view._mutation_choice = mut.get("id")
            await interaction.followup.send(embed=self.perks_embed, view=self.perks_view)
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


def _build_gear_embed(
    loadout: dict,
    inventory: list[dict],
    damaged: list[dict],
    dig_service: DigService | None = None,
) -> discord.Embed:
    """Build the /dig gear embed: equipped slots + summary footer.

    When ``dig_service`` is provided, damaged equipped pieces also show
    their repair cost so the player can decide before clicking Repair.
    """
    embed = discord.Embed(title="Your Loadout", color=0x8B4513)

    def _slot_value(slot_name: str) -> str:
        piece = loadout.get(slot_name)
        if piece is None:
            return "_— Empty —_"
        line = f"**{piece['name']}** ({piece['durability']}/{piece['max_durability']})"
        if (
            dig_service is not None
            and piece["durability"] < piece["max_durability"]
        ):
            cost = dig_service.compute_repair_cost(piece["slot"], piece["tier"])
            if cost > 0:
                line += f"\nRepair: {cost} {JOPACOIN_EMOTE}"
        return line

    embed.add_field(name="Weapon", value=_slot_value("weapon"), inline=False)
    embed.add_field(name="Armor",  value=_slot_value("armor"),  inline=False)
    embed.add_field(name="Boots",  value=_slot_value("boots"),  inline=False)

    relics = loadout.get("relics") or []
    if relics:
        names = ", ".join(r.get("artifact_id", "?") for r in relics)
        relic_value = f"**{len(relics)}** equipped: {names}"
    else:
        relic_value = "_None equipped_"
    embed.add_field(name="Relics", value=relic_value, inline=False)

    inv_count = len(inventory)
    damaged_count = len(damaged)
    footer_parts = [f"{inv_count} owned"]
    if damaged_count:
        footer_parts.append(f"{damaged_count} damaged")
    footer_parts.append("Buy gear via /dig shop")
    embed.set_footer(text=" • ".join(footer_parts))
    return embed


class GearPanelView(discord.ui.View):
    """Top-level /dig gear panel: equip, unequip, repair via Selects."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        repair_all_cost: int = 0,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        # Surface the total repair cost on the button so players see the
        # bill before they click. Disable when nothing is damaged.
        if repair_all_cost > 0:
            self.repair_all_btn.label = f"Repair All ({repair_all_cost} JC)"
            self.repair_all_btn.disabled = False
        else:
            self.repair_all_btn.label = "Repair All"
            self.repair_all_btn.disabled = True

    async def _refresh(self, interaction: discord.Interaction) -> None:
        """Reload loadout + inventory and rebuild the panel embed in place."""
        loadout = await asyncio.to_thread(
            self.dig_service.get_loadout, self.user_id, self.guild_id
        )
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        total_cost = sum(
            self.dig_service.compute_repair_cost(g["slot"], g["tier"])
            for g in damaged
        )
        embed = _build_gear_embed(loadout, inventory, damaged, self.dig_service)
        # Reset to the main panel buttons (in case we're being called from a sub-view).
        view = GearPanelView(
            self.dig_service, self.user_id, self.guild_id, repair_all_cost=total_cost,
        )
        await interaction.edit_original_response(embed=embed, view=view)

    def _check_owner(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.user_id

    @discord.ui.button(label="Equip", style=discord.ButtonStyle.primary, row=0)
    async def equip_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await interaction.response.defer()
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        relics = await asyncio.to_thread(
            self.dig_service.get_owned_relics, self.user_id, self.guild_id
        )
        unequipped_gear = [
            g for g in inventory
            if not g["equipped"] and g["durability"] > 0
        ]
        unequipped_relics = [r for r in relics if not r.get("equipped")]
        if not unequipped_gear and not unequipped_relics:
            await interaction.followup.send("Nothing to equip.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="equip",
            gear_items=unequipped_gear,
            relics=unequipped_relics,
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Unequip", style=discord.ButtonStyle.secondary, row=0)
    async def unequip_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await interaction.response.defer()
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        relics = await asyncio.to_thread(
            self.dig_service.get_owned_relics, self.user_id, self.guild_id
        )
        equipped_gear = [g for g in inventory if g["equipped"]]
        equipped_relics = [r for r in relics if r.get("equipped")]
        if not equipped_gear and not equipped_relics:
            await interaction.followup.send("Nothing equipped to unequip.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="unequip",
            gear_items=equipped_gear,
            relics=equipped_relics,
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Repair", style=discord.ButtonStyle.success, row=0)
    async def repair_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await interaction.response.defer()
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        if not damaged:
            await interaction.followup.send("Nothing damaged to repair.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="repair",
            gear_items=damaged,
            relics=[],
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Repair All", style=discord.ButtonStyle.danger, row=0)
    async def repair_all_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await interaction.response.defer()
        # Run repair_all directly — the cost is shown in the result.
        result = _wrap(await asyncio.to_thread(
            self.dig_service.repair_all_gear, self.user_id, self.guild_id
        ))
        if not getattr(result, "success", True):
            await interaction.followup.send(
                getattr(result, "error", "Repair failed."), ephemeral=True
            )
        else:
            count = getattr(result, "repaired", 0)
            cost = getattr(result, "cost", 0)
            await interaction.followup.send(
                f"Repaired **{count}** piece(s) for **{cost}** {JOPACOIN_EMOTE}.",
                ephemeral=True,
            )
        await self._refresh(interaction)


class GearSelectView(discord.ui.View):
    """Sub-view: dropdown of gear / relic items + Back button."""

    def __init__(
        self,
        *,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        mode: str,                 # "equip" | "unequip" | "repair"
        gear_items: list[dict],
        relics: list[dict],
        parent: GearPanelView,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.mode = mode
        self.parent = parent

        options: list[discord.SelectOption] = []
        for g in gear_items[:20]:
            slot_label = g["slot"].title()
            label = f"[{slot_label}] {g['name']} ({g['durability']}/{g['max_durability']})"
            if mode == "repair":
                cost = dig_service.compute_repair_cost(g["slot"], g["tier"])
                if cost > 0:
                    label += f" — {cost} JC"
            options.append(discord.SelectOption(
                label=label[:100],
                value=f"gear:{g['id']}",
            ))
        for r in relics[:25 - len(options)]:
            db_id = r.get("db_id")
            if db_id is None:
                # Skip malformed relic rows rather than encoding a string
                # artifact_id that would later silently parse to id=0.
                logger.warning("Relic %s missing db_id; skipping", r)
                continue
            options.append(discord.SelectOption(
                label=f"[Relic] {r.get('name', r.get('artifact_id', '?'))}"[:100],
                value=f"relic:{db_id}",
            ))
        if not options:
            options = [discord.SelectOption(label="(nothing here)", value="noop")]

        verb = {"equip": "Equip", "unequip": "Unequip", "repair": "Repair"}.get(mode, "Choose")
        self.select = discord.ui.Select(
            placeholder=f"{verb} which piece?",
            options=options,
            min_values=1,
            max_values=1,
        )
        self.select.callback = self._on_select
        self.add_item(self.select)

    async def _on_select(self, interaction: discord.Interaction):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await interaction.response.defer()
        value = self.select.values[0]
        if value == "noop":
            await self.parent._refresh(interaction)
            return
        kind, _, raw = value.partition(":")
        try:
            target_id = int(raw)
        except ValueError:
            await interaction.followup.send("Invalid selection.", ephemeral=True)
            await self.parent._refresh(interaction)
            return

        gear_fns = {
            "equip": self.dig_service.equip_gear,
            "unequip": self.dig_service.unequip_gear,
            "repair": self.dig_service.repair_gear,
        }
        relic_fns = {
            "equip": self.dig_service.equip_relic_for_player,
            "unequip": self.dig_service.unequip_relic_for_player,
        }
        if kind == "gear":
            fn = gear_fns.get(self.mode)
            if fn is None:
                result = _wrap({"success": False, "error": "Action not supported."})
            else:
                result = _wrap(await asyncio.to_thread(
                    fn, self.user_id, self.guild_id, target_id,
                ))
        elif kind == "relic":
            fn = relic_fns.get(self.mode)
            if fn is None:
                result = _wrap({"success": False, "error": "Relics can't be repaired."})
            else:
                result = _wrap(await asyncio.to_thread(
                    fn, self.user_id, self.guild_id, target_id,
                ))
        else:
            result = _wrap({"success": False, "error": "Unknown selection."})

        if not getattr(result, "success", True):
            await interaction.followup.send(
                getattr(result, "error", "Action failed."), ephemeral=True
            )
        else:
            verb_past = {"equip": "Equipped", "unequip": "Unequipped", "repair": "Repaired"}.get(self.mode, "Done")
            cost = getattr(result, "cost", 0)
            cost_part = f" for {cost} {JOPACOIN_EMOTE}" if cost else ""
            await interaction.followup.send(f"{verb_past}{cost_part}.", ephemeral=True)
        await self.parent._refresh(interaction)

    @discord.ui.button(label="Back", style=discord.ButtonStyle.secondary, row=1)
    async def back_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await interaction.response.defer()
        await self.parent._refresh(interaction)


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


# ---------------------------------------------------------------------------
# Cog
# ---------------------------------------------------------------------------

class DigCommands(commands.Cog):
    dig = app_commands.Group(name="dig", description="Tunnel digging minigame")

    def __init__(self, bot: commands.Bot, dig_service: DigService, dig_llm_service: DigLLMService | None = None):
        self.bot = bot
        self.dig_service = dig_service
        self.dig_llm_service = dig_llm_service
        self._last_weather_date: str | None = None

    async def cog_load(self) -> None:
        self._weather_broadcast_loop.start()

    async def cog_unload(self) -> None:
        self._weather_broadcast_loop.cancel()

    @tasks.loop(minutes=10)
    async def _weather_broadcast_loop(self) -> None:
        """Check every 10 min if the game day rolled over; if so, post weather."""
        today = await asyncio.to_thread(self.dig_service._get_game_date)
        if today == self._last_weather_date:
            return
        self._last_weather_date = today

        for guild in self.bot.guilds:
            try:
                weather = await asyncio.to_thread(self.dig_service.get_weather, guild.id)
                if not weather:
                    continue

                embed = discord.Embed(
                    title="\u26c5 Daily Layer Weather",
                    description="New conditions have settled across the depths.",
                    color=0x5865F2,
                )
                for w in weather:
                    layer = w.get("layer", "Unknown")
                    name = w.get("name", "Unknown")
                    desc = w.get("description", "")
                    embed.add_field(
                        name=f"{layer} — {name}",
                        value=f"*{desc}*",
                        inline=False,
                    )
                embed.set_footer(text="Weather affects all diggers in that layer today. Use /dig weather for details.")

                target = None
                if DIG_CHANNEL_ID is not None:
                    target = guild.get_channel(DIG_CHANNEL_ID)
                if target is None:
                    for channel in guild.text_channels:
                        if "gamba" in channel.name.lower():
                            target = channel
                            break
                if target is not None:
                    await target.send(embed=embed)
            except Exception:
                logger.exception("Failed to broadcast weather for guild %s", guild.id)

    @_weather_broadcast_loop.before_loop
    async def _before_weather_loop(self) -> None:
        await self.bot.wait_until_ready()
        self._last_weather_date = await asyncio.to_thread(self.dig_service._get_game_date)

    # ------------------------------------------------------------------
    # Channel routing
    # ------------------------------------------------------------------

    async def _send_public_dig(
        self,
        interaction: discord.Interaction,
        *,
        embed: discord.Embed | None = None,
        view: discord.ui.View | None = None,
        file: discord.File | None = None,
        files: list[discord.File] | None = None,
    ) -> discord.Message | None:
        """Send a public dig embed to the dedicated dig channel when set,
        otherwise fall through to ``safe_followup``. When routed away from
        the invocation channel, also post a one-line ephemeral pointer so
        the user knows where their result landed.
        """
        target = await self._get_dig_target_channel(interaction)
        invocation_channel = interaction.channel
        send_kwargs: dict = {}
        if embed is not None:
            send_kwargs["embed"] = embed
        if view is not None:
            send_kwargs["view"] = view
        if files:
            send_kwargs["files"] = files
        elif file is not None:
            send_kwargs["file"] = file

        if target is None or (
            invocation_channel is not None
            and getattr(target, "id", None) == getattr(invocation_channel, "id", None)
        ):
            return await safe_followup(interaction, **send_kwargs)

        try:
            msg = await target.send(**send_kwargs)
        except Exception:
            logger.exception("Dig channel send failed; falling back to invocation channel")
            return await safe_followup(interaction, **send_kwargs)

        # Acknowledge the deferred interaction with a quiet pointer so the
        # user isn't left with a perpetual "thinking..." indicator.
        try:
            mention = getattr(target, "mention", None) or "the dig channel"
            await interaction.followup.send(
                f"Posted in {mention}.", ephemeral=True,
            )
        except Exception:
            pass
        return msg

    async def _get_dig_target_channel(
        self, interaction: discord.Interaction,
    ) -> discord.abc.Messageable | None:
        """Resolve the channel where public /dig embeds should land.

        Returns the dedicated dig channel when ``DIG_CHANNEL_ID`` is set,
        accessible, in the same guild as the interaction, and the bot
        has send permission. Otherwise falls back to ``interaction.channel``.
        Mirrors the lobby-channel pattern.
        """
        if not DIG_CHANNEL_ID:
            return interaction.channel
        if interaction.guild is None:
            return interaction.channel
        try:
            channel = self.bot.get_channel(DIG_CHANNEL_ID)
            if not channel:
                channel = await self.bot.fetch_channel(DIG_CHANNEL_ID)
            if isinstance(channel, discord.TextChannel):
                if channel.guild.id != interaction.guild.id:
                    logger.warning(
                        "Dedicated dig channel %s is in different guild", DIG_CHANNEL_ID,
                    )
                    return interaction.channel
                perms = channel.permissions_for(channel.guild.me)
                if not perms.send_messages:
                    logger.warning(
                        "Bot lacks send_messages in dedicated dig channel %s", DIG_CHANNEL_ID,
                    )
                    return interaction.channel
            return channel
        except (discord.NotFound, discord.Forbidden) as exc:
            logger.warning("Cannot access dedicated dig channel %s: %s", DIG_CHANNEL_ID, exc)
            return interaction.channel
        except Exception as exc:
            logger.warning("Error fetching dedicated dig channel: %s", exc)
            return interaction.channel

    # ------------------------------------------------------------------
    # DM Mode helpers
    # ------------------------------------------------------------------

    def _is_dm_mode(self) -> bool:
        """Whether the DM engine service is available."""
        return self.dig_llm_service is not None and self.dig_llm_service.dig_service is not None

    async def _run_dm_dig(
        self, user_id: int, guild_id: int | None, paid: bool = False,
    ):
        """Execute a DM-powered dig if the player opted in.

        Returns ``(result, used_dm)`` — *result* is the wrapped result
        ready for embed building, *used_dm* is True if the DM path was
        taken (or its fallback).  Returns ``(None, False)`` if the player
        is in legacy mode so the caller can fall through to ``dig()``.
        """
        if not self._is_dm_mode():
            return None, False
        try:
            engine_mode = await asyncio.to_thread(
                self.dig_service.dig_repo.get_engine_mode, user_id, guild_id,
            )
            if engine_mode != "llm":
                return None, False

            terminal, preconditions = await asyncio.to_thread(
                self.dig_service.dig_with_preconditions, user_id, guild_id, paid,
            )
            if terminal is not None:
                return _wrap(terminal), True
            # DM decides the outcome (falls back to deterministic internally)
            result = await self.dig_llm_service.run_dig(user_id, guild_id, preconditions)
            return _wrap(result), True
        except Exception:
            logger.warning("DM dig failed, falling through to legacy", exc_info=True)
            return None, False

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
        if not await require_dig_channel(interaction):
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

        # DM-first flow: if player opted into DM mode, use preconditions + LLM
        result, used_dm = await self._run_dm_dig(interaction.user.id, guild_id)
        if not used_dm:
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.dig, interaction.user.id, guild_id
                ))
            except Exception as e:
                logger.error("Dig error: %s", e, exc_info=True)
                await safe_followup(interaction, content="Dig failed. Try again later.", ephemeral=True)
                return

        # Non-cooldown errors (cooldown is handled by the paid_dig_available branch)
        if not getattr(result, "success", False) and not getattr(result, "paid_dig_available", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Dig failed."),
                ephemeral=True,
            )
            return

        # Dispatch by result shape. Each branch handles its own reply + reactions.
        if getattr(result, "is_first_dig", False):
            await self._send_first_dig_welcome(interaction)
            return
        if getattr(result, "boss_encounter", False):
            await self._handle_boss_encounter(interaction, guild_id, result)
            return
        if getattr(result, "paid_dig_available", False):
            await self._handle_paid_dig_confirmation(interaction, guild_id, result)
            return

        event = getattr(result, "event", None)
        event_data = None
        if event is not None:
            event_data = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
        if isinstance(event_data, dict):
            complexity = event_data.get("complexity", "choice")
            if complexity == "boon" and event_data.get("boon_options"):
                await self._handle_boon_encounter(interaction, guild_id, result, event_data)
                return
            if event_data.get("safe_option"):
                await self._handle_choice_encounter(interaction, guild_id, result, event_data)
                return

        await self._send_normal_dig_result(interaction, result)

    # ── dig_go helpers (one per result shape) ───────────────────────────

    async def _send_first_dig_welcome(self, interaction: discord.Interaction) -> None:
        """Reply to the very first dig with a welcome embed."""
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
        await self._send_public_dig(interaction, embed=embed)

    async def _handle_boss_encounter(
        self, interaction: discord.Interaction, guild_id: int | None, result
    ) -> None:
        """Render a boss encounter embed and attach the interactive view."""
        boss_info = getattr(result, "boss_info", None)
        has_lantern = getattr(result, "has_lantern", False)
        embed = discord.Embed(
            title=f"Boss Encountered: {getattr(boss_info, 'name', 'Unknown Boss')}!",
            description=getattr(boss_info, "dialogue", "A fearsome guardian blocks your path!"),
            color=0xFF0000,
        )

        boss_file = None
        boundary = getattr(boss_info, "boundary", None)
        boss_id = getattr(boss_info, "boss_id", "") or boundary
        if boss_id:
            try:
                from utils.dig_assets import get_boss_art
                depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
                ld = get_layer_def(depth or boundary)
                ln = ld.name if ld else "Dirt"
                boss_file = await asyncio.to_thread(get_boss_art, boss_id, "encounter", ln)
            except Exception as e:
                logger.debug("Boss encounter art failed: %s", e)

        if boss_file:
            embed.set_image(url=f"attachment://{boss_file.filename}")
        elif hasattr(boss_info, "ascii_art"):
            embed.add_field(name="\u200b", value=f"```\n{boss_info.ascii_art}\n```", inline=False)

        lum_line = getattr(boss_info, "luminosity_display", None)
        if lum_line:
            embed.add_field(name="\u200b", value=lum_line, inline=False)

        view = BossEncounterView(self.dig_service, interaction.user.id, guild_id, boss_info, has_lantern, dig_llm_service=self.dig_llm_service)
        msg = await self._send_public_dig(interaction, embed=embed, view=view, file=boss_file)
        if msg:
            try:
                await msg.add_reaction("\U0001f480")
            except Exception:
                pass

    async def _handle_paid_dig_confirmation(
        self, interaction: discord.Interaction, guild_id: int | None, result
    ) -> None:
        """Prompt the player for a paid dig and execute it on confirm.

        Also routes any event the paid dig rolled (the bug-fix brought in
        from ``feat/dig-more``): free and paid digs should both surface
        interactive boon/choice encounters.
        """
        cost = getattr(result, "paid_dig_cost", 0)
        embed = discord.Embed(
            title="Paid Dig Required",
            description=f"Continuing costs **{cost}** {JOPACOIN_EMOTE}. Proceed?",
            color=0xFFA500,
        )
        view = PaidDigView(self.dig_service, interaction.user.id, guild_id, cost)
        msg = await safe_followup(interaction, embed=embed, view=view)
        if not msg:
            return
        await view.wait()
        if not view.value:
            await msg.edit(content="Dig cancelled.", embed=None, view=None)
            return
        # Show immediate feedback while the dig runs
        await msg.edit(
            embed=discord.Embed(title="Digging...", description="Your pickaxe swings.", color=0xFFA500),
            view=None,
        )
        # DM-first flow for paid dig
        paid_result, used_dm = await self._run_dm_dig(
            interaction.user.id, guild_id, paid=True,
        )
        if not used_dm:
            try:
                paid_result = _wrap(await asyncio.to_thread(
                    self.dig_service.dig, interaction.user.id, guild_id, paid=True
                ))
            except Exception as e:
                logger.error("Paid dig error: %s", e)
                await msg.edit(content="Paid dig failed.", embed=None, view=None)
                return
        if not getattr(paid_result, "success", False):
            err = getattr(paid_result, "error", "Paid dig failed.")
            await msg.edit(content=err, embed=None, view=None)
            return
        paid_embed, paid_layer_name, paid_pick_tier, paid_items_ids = _build_dig_embed(paid_result, interaction.user)
        paid_layer_file = await _attach_layer_thumbnail(paid_embed, paid_layer_name)
        paid_pick_file = await _attach_pickaxe_footer(paid_embed, paid_pick_tier)
        paid_items_strip = await _attach_items_strip(paid_embed, paid_items_ids)
        paid_files = [f for f in (paid_layer_file, paid_pick_file, paid_items_strip) if f]
        if paid_files:
            await msg.edit(embed=paid_embed, view=None, attachments=paid_files)
        else:
            await msg.edit(embed=paid_embed, view=None)

        # Route any event the paid dig rolled. The stats card is already
        # posted via msg.edit above, so we only need the event UI (no
        # second stats-card send).
        event = getattr(paid_result, "event", None)
        if event is not None:
            event_data = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
            if isinstance(event_data, dict):
                complexity = event_data.get("complexity", "choice")
                if complexity == "boon" and event_data.get("boon_options"):
                    await self._send_boon_event_ui(interaction, guild_id, paid_result, event_data)
                elif event_data.get("safe_option"):
                    await self._send_choice_event_ui(interaction, guild_id, paid_result, event_data)

    async def _resolve_event_art(self, event_id: str, result) -> discord.File | None:
        """Attempt to load the event art attachment, returning ``None`` on failure."""
        try:
            from utils.dig_assets import get_event_art
            depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
            layer_def = get_layer_def(depth)
            ev_layer = layer_def.name if layer_def else "Dirt"
            return await asyncio.to_thread(get_event_art, event_id, ev_layer)
        except Exception as e:
            logger.debug("Event art failed: %s", e)
            return None

    async def _send_dig_result_with_attachments(
        self, interaction: discord.Interaction, result
    ) -> None:
        """Send the main dig result embed (stats card) with layer/pickaxe/items files."""
        embed, layer_name, pickaxe_tier, items_ids = _build_dig_embed(result, interaction.user)
        layer_file = await _attach_layer_thumbnail(embed, layer_name)
        pickaxe_file = await _attach_pickaxe_footer(embed, pickaxe_tier)
        items_strip = await _attach_items_strip(embed, items_ids)
        dig_files = [f for f in (layer_file, pickaxe_file, items_strip) if f]
        if len(dig_files) > 1:
            await self._send_public_dig(interaction, embed=embed, files=dig_files)
        elif dig_files:
            await self._send_public_dig(interaction, embed=embed, file=dig_files[0])
        else:
            await self._send_public_dig(interaction, embed=embed)

    async def _send_boon_event_ui(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Send just the boon-selection embed + view (no stats card)."""
        boon_options = event_data["boon_options"]
        boon_lines = [f"**{b.get('name', '?')}** — {b.get('description', '')}" for b in boon_options]
        boon_flavor = pick_description(event_data) or "Choose a boon:"
        event_embed = discord.Embed(
            description=boon_flavor + "\n\n" + "\n".join(boon_lines),
            color=0x5865F2,
        )
        boon_event_file = await self._resolve_event_art(event_data.get("id", ""), result)
        if boon_event_file:
            event_embed.set_image(url=f"attachment://{boon_event_file.filename}")

        target_channel = await self._get_dig_target_channel(interaction)
        view = BoonSelectionView(
            self.dig_service, interaction.user.id, guild_id, event_data,
            target_channel=target_channel,
        )
        if boon_event_file:
            await self._send_public_dig(interaction, embed=event_embed, view=view, file=boon_event_file)
        else:
            await self._send_public_dig(interaction, embed=event_embed, view=view)

    async def _send_choice_event_ui(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Send just the choice-event embed + encounter view (no stats card)."""
        event_embed = discord.Embed(
            description=pick_description(event_data) or "Something happens...",
            color=0xDAA520,
        )
        ascii_art = event_data.get("ascii_art")
        if ascii_art:
            event_embed.add_field(name="\u200b", value=f"```\n{ascii_art}\n```", inline=False)

        event_file = await self._resolve_event_art(event_data.get("id", ""), result)
        if event_file:
            event_embed.set_image(url=f"attachment://{event_file.filename}")

        _lum_info = getattr(result, "luminosity_info", None)
        _lum_val = (_lum_info.get("luminosity_after", 100) if isinstance(_lum_info, dict)
                    else getattr(_lum_info, "luminosity_after", 100)) if _lum_info else 100
        target_channel = await self._get_dig_target_channel(interaction)
        view = EventEncounterView(
            self.dig_service, interaction.user.id, guild_id, event_data,
            luminosity=_lum_val, target_channel=target_channel,
            dig_llm_service=self.dig_llm_service,
        )
        if event_file:
            await self._send_public_dig(interaction, embed=event_embed, view=view, file=event_file)
        else:
            await self._send_public_dig(interaction, embed=event_embed, view=view)

    async def _handle_boon_encounter(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Render a boon-pick encounter: stats card + boon selection view."""
        await self._send_dig_result_with_attachments(interaction, result)
        await self._send_boon_event_ui(interaction, guild_id, result, event_data)

    async def _handle_choice_encounter(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Render a choice/complex event: stats card + encounter view with safe/risky buttons."""
        await self._send_dig_result_with_attachments(interaction, result)
        await self._send_choice_event_ui(interaction, guild_id, result, event_data)

    async def _send_normal_dig_result(
        self, interaction: discord.Interaction, result
    ) -> None:
        """Send the plain dig result (no boss/event/boon) and attach post-dig reactions."""
        embed, layer_name, pickaxe_tier, items_ids = _build_dig_embed(result, interaction.user)
        layer_file = await _attach_layer_thumbnail(embed, layer_name)
        pickaxe_file = await _attach_pickaxe_footer(embed, pickaxe_tier)
        items_strip = await _attach_items_strip(embed, items_ids)
        dig_files = [f for f in (layer_file, pickaxe_file, items_strip) if f]
        if len(dig_files) > 1:
            msg = await self._send_public_dig(interaction, embed=embed, files=dig_files)
        elif dig_files:
            msg = await self._send_public_dig(interaction, embed=embed, file=dig_files[0])
        else:
            msg = await self._send_public_dig(interaction, embed=embed)

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
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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

        # Pinnacle foreshadow — fires only after all 7 tier bosses cleared
        # and pinnacle still pending. Subtle, no explicit depth.
        foreshadow = info.get("pinnacle_foreshadow") if isinstance(info, dict) else None
        if foreshadow:
            embed.add_field(name="​", value=f"*{foreshadow}*", inline=False)
        elif depth > LUMINOSITY_DEEP_DRAIN_START_DEPTH:
            embed.add_field(name="​", value="*The deep grows hungry.*", inline=False)

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
                        event_lines.append("Sabotage attempt — trap triggered!")
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

        # Active ascension modifiers (prestige > 0)
        if prestige > 0:
            asc_lines = []
            for level in range(1, prestige + 1):
                mod = ASCENSION_MODIFIERS.get(level)
                if mod:
                    asc_lines.append(f"**P{level} {mod.name}**: {mod.penalty} / {mod.reward}")
            if asc_lines:
                # Truncate to fit embed field limit
                asc_text = "\n".join(asc_lines[:10])
                embed.add_field(name="Ascension Modifiers", value=asc_text, inline=False)

        embed.set_thumbnail(url=display_user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 5. /dig_leaderboard — Top tunnels
    # ------------------------------------------------------------------

    @dig.command(name="leaderboard", description="View top tunnels")
    async def dig_leaderboard(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
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
            name = _get(entry, "tunnel_name", None) or f"Tunnel #{_get(entry, 'discord_id', '?')}"
            depth = _get(entry, "depth", 0)
            layer = self.dig_service.get_layer(depth).get("name", "Dirt")
            bar_len = max(1, int(20 * depth / max_depth))
            bar = "\u2588" * bar_len
            lines.append(f"`{i:>2}.` **{name}** — {depth} ({layer})\n`{bar}`")

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
    # 5b. /dig halloffame — Best prestige run scores
    # ------------------------------------------------------------------

    @dig.command(name="halloffame", description="View the hall of fame (best prestige run scores)")
    async def dig_halloffame(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.get_hall_of_fame, guild_id
            ))
        except Exception as e:
            logger.error("Hall of fame error: %s", e)
            await safe_followup(interaction, content="Hall of fame unavailable.", ephemeral=True)
            return

        entries = getattr(result, "entries", []) or []
        if not entries:
            await safe_followup(
                interaction,
                content="The hall of fame is empty. Prestige to earn a spot!",
                ephemeral=True,
            )
            return

        lines = []
        for i, entry in enumerate(entries[:10], 1):
            def _g(obj, key, default=None):
                return obj.get(key, default) if isinstance(obj, dict) else getattr(obj, key, default)
            name = _g(entry, "tunnel_name", "Unknown")
            discord_id = _g(entry, "discord_id", None)
            prestige = _g(entry, "prestige_level", 0)
            score = _g(entry, "best_run_score", 0)
            player_mention = f"<@{discord_id}>" if discord_id else "Unknown"
            medal = {1: "\U0001f947", 2: "\U0001f948", 3: "\U0001f949"}.get(i, f"`#{i}`")
            lines.append(f"{medal} **{name}** ({player_mention}) - Score: {score} (P{prestige})")

        embed = discord.Embed(
            title="\U0001f3c6 Hall of Fame",
            description="\n".join(lines),
            color=0xFFD700,
        )
        embed.set_footer(text="Best prestige run scores")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 6. /dig use — Queue consumable
    # ------------------------------------------------------------------

    @dig.command(name="use", description="Queue a consumable for your next dig")
    @app_commands.describe(item="The item to use")
    @app_commands.autocomplete(item=item_autocomplete)
    async def dig_use(self, interaction: discord.Interaction, item: str):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction, ephemeral=True)

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
            use_embed = discord.Embed(
                title=f"{item_name} Queued",
                description="Ready for your next dig.",
                color=0xD4AF37,
            )
            item_file = None
            try:
                from utils.dig_assets import get_item_art
                item_file = get_item_art(item)
                if item_file:
                    use_embed.set_thumbnail(url=f"attachment://{item_file.filename}")
            except Exception:
                pass
            await safe_followup(interaction, embed=use_embed, file=item_file, ephemeral=True)
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
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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

        # Boss-combat gear (Armor / Boots; weapons are the pickaxe row above)
        gear_for_sale = getattr(shop, "gear_for_sale", [])
        if gear_for_sale:
            gear_text = "\n".join(
                f"**{g.get('name', '?')}** — {g.get('price', '?')} {JOPACOIN_EMOTE} "
                f"(Depth {g.get('depth_req', '?')}"
                + (f", Prestige {g.get('prestige_req', 0)}" if g.get('prestige_req', 0) else "")
                + ")"
                for g in gear_for_sale
            )
            embed.add_field(name="Boss Gear", value=gear_text, inline=False)

        # Inventory count
        inv_count = getattr(shop, "inventory_count", 0)
        embed.set_footer(text=f"Your inventory: {inv_count}/{MAX_INVENTORY_SLOTS} items | Use /dig buy <item> to purchase, /dig use <item> to queue")

        shop_file = None
        try:
            from utils.dig_assets import compose_shop_grid
            shop_file = await asyncio.to_thread(compose_shop_grid)
            if shop_file:
                embed.set_image(url=f"attachment://{shop_file.filename}")
        except Exception:
            pass
        await safe_followup(interaction, embed=embed, file=shop_file)

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
        app_commands.Choice(name="Stone Plate (20 JC)",   value="armor:1"),
        app_commands.Choice(name="Iron Plate (60 JC)",    value="armor:2"),
        app_commands.Choice(name="Diamond Plate (180 JC)", value="armor:3"),
        app_commands.Choice(name="Stone Boots (25 JC)",   value="boots:1"),
        app_commands.Choice(name="Iron Boots (70 JC)",    value="boots:2"),
        app_commands.Choice(name="Diamond Boots (200 JC)", value="boots:3"),
    ])
    async def dig_buy(self, interaction: discord.Interaction, item: str):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id if interaction.guild else None

        # Gear choice values are encoded as "<slot>:<tier>"; route them to
        # the gear-buy path. Everything else is a consumable.
        if ":" in item:
            slot, _, tier_str = item.partition(":")
            try:
                tier = int(tier_str)
            except ValueError:
                tier = -1
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.buy_gear,
                    interaction.user.id, guild_id, slot, tier,
                ))
            except Exception as e:
                logger.error("Dig buy_gear error: %s", e)
                await safe_followup(interaction, content="Purchase failed.", ephemeral=True)
                return
            if not getattr(result, "success", False):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Purchase failed."),
                    ephemeral=True,
                )
                return
            name = getattr(result, "name", item)
            cost = getattr(result, "cost", 0)
            await safe_followup(
                interaction,
                content=(
                    f"Bought **{name}** for **{cost}** {JOPACOIN_EMOTE}.\n"
                    f"Equip it via `/dig gear`."
                ),
                ephemeral=True,
            )
            return

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
        buy_embed = discord.Embed(
            title=f"Purchased: {item_name}",
            description=(
                f"Cost: **{cost}** {JOPACOIN_EMOTE}\n"
                f"Balance: **{balance_after}** {JOPACOIN_EMOTE}\n\n"
                f"Use `/dig use {item}` to queue it."
            ),
            color=0xD4AF37,
        )
        item_file = None
        try:
            from utils.dig_assets import get_item_art
            item_file = get_item_art(item)
            if item_file:
                buy_embed.set_thumbnail(url=f"attachment://{item_file.filename}")
        except Exception:
            pass
        await safe_followup(interaction, embed=buy_embed, file=item_file, ephemeral=True)

    # ------------------------------------------------------------------
    # 9. /dig museum — Guild artifact museum
    # ------------------------------------------------------------------

    @dig.command(name="museum", description="View the guild artifact museum")
    async def dig_museum(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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
    # 10b. /dig prestige — Prestige your tunnel
    # ------------------------------------------------------------------

    @dig.command(name="prestige", description="Prestige your tunnel (reset depth, gain a perk)")
    async def dig_prestige(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            check = _wrap(await asyncio.to_thread(
                self.dig_service.can_prestige, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Prestige check error: %s", e, exc_info=True)
            await safe_followup(interaction, content="Prestige check failed.", ephemeral=True)
            return

        can = getattr(check, "can_prestige", False)
        if not can:
            reason = getattr(check, "reason", "You cannot prestige yet.")
            await safe_followup(interaction, content=reason, ephemeral=True)
            return

        prestige_level = getattr(check, "prestige_level", 0)
        new_level = prestige_level + 1
        run_score = getattr(check, "run_score", 0)

        # Build the prestige preview embed
        embed = discord.Embed(
            title=f"Prestige to P{new_level}?",
            description=(
                "This will **reset your tunnel depth to 0** but grant a permanent perk.\n\n"
                f"**Run Score:** {run_score}\n"
            ),
            color=0xFFD700,
        )

        # Show the ascension modifier that will be unlocked at this level
        asc_mod = ASCENSION_MODIFIERS.get(new_level)
        if asc_mod:
            embed.add_field(
                name=f"Ascension Unlock: {asc_mod.name}",
                value=f"Penalty: {asc_mod.penalty}\nReward: {asc_mod.reward}",
                inline=False,
            )

        # Build perk list for the view
        available_perks_raw = getattr(check, "available_perks", []) or []
        if isinstance(available_perks_raw, _DictObj):
            available_perks_raw = available_perks_raw._d if hasattr(available_perks_raw, "_d") else []
        perk_dicts = [
            {"id": p, "name": p.replace("_", " ").title()}
            for p in available_perks_raw
        ]

        if not perk_dicts:
            await safe_followup(
                interaction, content="No perks available. You may have unlocked them all.",
                ephemeral=True,
            )
            return

        perks_view = PrestigePerksView(self.dig_service, interaction.user.id, guild_id, perk_dicts)

        # P8+ mutation selection step
        mutation_info = getattr(check, "mutation_info", None)
        if mutation_info:
            mut_d = mutation_info if isinstance(mutation_info, dict) else (
                mutation_info._d if hasattr(mutation_info, "_d") else {}
            )
            forced = mut_d.get("forced") if isinstance(mut_d, dict) else None
            choices = mut_d.get("choices") if isinstance(mut_d, dict) else None

            if forced and choices:
                forced_d = forced if isinstance(forced, dict) else (
                    forced._d if hasattr(forced, "_d") else {}
                )
                choices_list = choices if isinstance(choices, list) else (
                    choices._d if hasattr(choices, "_d") else []
                )
                # Unwrap _DictObj items in choices_list
                unwrapped_choices = []
                for c in choices_list:
                    if isinstance(c, dict):
                        unwrapped_choices.append(c)
                    elif hasattr(c, "_d"):
                        unwrapped_choices.append(c._d)
                    else:
                        unwrapped_choices.append({"id": str(c), "name": str(c)})

                embed.add_field(
                    name=f"Forced Mutation: {forced_d.get('name', '?')}",
                    value=forced_d.get("description", ""),
                    inline=False,
                )
                mut_lines = [
                    f"**{m.get('name', '?')}** — {m.get('description', '')}"
                    for m in unwrapped_choices
                ]
                embed.add_field(
                    name="Choose a Mutation",
                    value="\n".join(mut_lines) or "No choices available",
                    inline=False,
                )

                # Build a perk selection embed for after mutation choice
                perks_embed = discord.Embed(
                    title="Choose a Prestige Perk",
                    description="\n".join(
                        f"**{p.get('name', '?')}**" for p in perk_dicts
                    ),
                    color=0xFFD700,
                )

                mutation_view = MutationSelectionView(
                    self.dig_service, interaction.user.id, guild_id,
                    forced_d, unwrapped_choices, perks_view, perks_embed,
                )
                await safe_followup(interaction, embed=embed, view=mutation_view)
                return

        # No mutations — go straight to perk selection
        embed.add_field(
            name="Choose a Perk",
            value="\n".join(f"**{p.get('name', '?')}**" for p in perk_dicts),
            inline=False,
        )
        await safe_followup(interaction, embed=embed, view=perks_view)

    # ------------------------------------------------------------------
    # 11. /dig_abandon — Abandon tunnel
    # ------------------------------------------------------------------

    @dig.command(name="abandon", description="Abandon your tunnel (partial refund)")
    async def dig_abandon(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
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
    # 12. /dig upgrade — REMOVED. Pickaxes are now part of the boss-gear
    # system; buy them via `/dig shop` (or, soon, the gear-buy UI) and
    # view/equip them via `/dig gear`. The upgrade_pickaxe service is
    # kept and used by upcoming shop integration.
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # 13. /dig_trap — Set a trap
    # ------------------------------------------------------------------

    @dig.command(name="trap", description="Set a trap in your tunnel")
    async def dig_trap(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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
        if not await require_dig_channel(interaction):
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
        # Pickaxe thumbnail
        inv_pickaxe_file = None
        try:
            from utils.dig_assets import get_pickaxe_art
            tunnel_info = await asyncio.to_thread(
                self.dig_service.dig_repo.get_tunnel, interaction.user.id, guild_id
            )
            tier_idx = dict(tunnel_info).get("pickaxe_tier", 0) if tunnel_info else 0
            inv_pickaxe_file = get_pickaxe_art(tier_idx)
            if inv_pickaxe_file:
                embed.set_thumbnail(url=f"attachment://{inv_pickaxe_file.filename}")
        except Exception:
            pass
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

        await safe_followup(interaction, embed=embed, file=inv_pickaxe_file)

    # ------------------------------------------------------------------
    # 16a. /dig gear — Manage boss-combat gear loadout
    # ------------------------------------------------------------------

    @dig.command(name="gear", description="Manage your boss-combat gear")
    async def dig_gear(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return
        player = await _check_registered(interaction, self.bot)
        if not player:
            return
        await safe_defer(interaction)
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            loadout = await asyncio.to_thread(
                self.dig_service.get_loadout, interaction.user.id, guild_id
            )
            inventory = await asyncio.to_thread(
                self.dig_service.get_inventory_gear, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Gear panel error: %s", e)
            await safe_followup(interaction, content="Gear panel unavailable.", ephemeral=True)
            return
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        total_cost = sum(
            self.dig_service.compute_repair_cost(g["slot"], g["tier"])
            for g in damaged
        )
        embed = _build_gear_embed(loadout, inventory, damaged, self.dig_service)
        view = GearPanelView(
            self.dig_service, interaction.user.id, guild_id, repair_all_cost=total_cost,
        )
        await safe_followup(interaction, embed=embed, view=view)

    # ------------------------------------------------------------------
    # 16b. /dig weather — View today's layer weather
    # ------------------------------------------------------------------

    @dig.command(name="weather", description="View today's layer weather conditions")
    async def dig_weather(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        weather = await asyncio.to_thread(self.dig_service.get_weather, guild_id)

        if not weather:
            await interaction.response.send_message("No weather today — skies are clear.", ephemeral=True)
            return

        embed = discord.Embed(
            title="Today's Layer Weather",
            description="Conditions shift daily.",
            color=0x5865F2,
        )
        for w in weather:
            layer = w.get("layer", "Unknown")
            name = w.get("name", "Unknown")
            desc = w.get("description", "")
            effects = w.get("effects", {})

            # Build a short mechanical summary
            fx_parts = []
            if effects.get("cave_in_bonus"):
                val = effects["cave_in_bonus"]
                fx_parts.append(f"{'+'if val > 0 else ''}{val:.0%} cave-in")
            if effects.get("jc_multiplier"):
                val = effects["jc_multiplier"]
                fx_parts.append(f"{'+'if val > 0 else ''}{val:.0%} JC")
            if effects.get("jc_bonus"):
                val = effects["jc_bonus"]
                fx_parts.append(f"{'+'if val > 0 else ''}{val} JC/dig")
            if effects.get("advance_bonus"):
                val = effects["advance_bonus"]
                fx_parts.append(f"{'+'if val > 0 else ''}{val} advance")
            if effects.get("event_chance_multiplier"):
                val = effects["event_chance_multiplier"]
                fx_parts.append(f"{'+'if val > 0 else ''}{val:.0%} events")
            if effects.get("artifact_multiplier") and effects["artifact_multiplier"] != 1.0:
                fx_parts.append(f"{effects['artifact_multiplier']:.1f}x artifacts")
            if effects.get("luminosity_drain_multiplier"):
                val = effects["luminosity_drain_multiplier"]
                fx_parts.append(f"+{val:.0%} lum drain")

            fx_str = " | ".join(fx_parts) if fx_parts else "No mechanical effect"

            embed.add_field(
                name=f"{layer} — {name}",
                value=f"*{desc}*\n`{fx_str}`",
                inline=False,
            )

        embed.set_footer(text="Weather affects all diggers in that layer today.")
        await interaction.response.send_message(embed=embed)

    # ------------------------------------------------------------------
    # 17. /dig resetcooldown — Admin: reset a player's free dig
    # ------------------------------------------------------------------

    @dig.command(name="resetcooldown", description="Reset a player's free dig cooldown (Admin only)")
    @app_commands.describe(user="The player whose cooldown to reset")
    async def dig_resetcooldown(self, interaction: discord.Interaction, user: discord.User):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(self.dig_service.reset_dig_cooldown, user.id, guild_id)

        if not result.get("success"):
            await interaction.response.send_message(result.get("error", "Failed."), ephemeral=True)
            return

        await interaction.response.send_message(f"Reset free dig cooldown for {user.mention}.", ephemeral=True)

    @dig.command(name="forceevent", description="Force next dig to trigger an event (Admin only)")
    @app_commands.describe(user="The player whose next dig gets an event")
    async def dig_forceevent(self, interaction: discord.Interaction, user: discord.User):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return
        # Store on the service so the next dig() for this user forces an event
        if not hasattr(self.dig_service, "_force_event_for"):
            self.dig_service._force_event_for = set()
        guild_id = interaction.guild.id if interaction.guild else None
        self.dig_service._force_event_for.add((user.id, guild_id))
        await interaction.response.send_message(f"Next dig for {user.mention} will force an event.", ephemeral=True)

    @dig.command(name="setdepth", description="Set a player's tunnel depth (Admin only)")
    @app_commands.describe(user="The player", depth="New depth value")
    async def dig_setdepth(self, interaction: discord.Interaction, user: discord.User, depth: int):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None
        tunnel = await asyncio.to_thread(self.dig_service.dig_repo.get_tunnel, user.id, guild_id)
        if not tunnel:
            await interaction.response.send_message("That player doesn't have a tunnel.", ephemeral=True)
            return

        depth = max(0, depth)
        await asyncio.to_thread(self.dig_service.dig_repo.update_tunnel, user.id, guild_id, depth=depth)
        await asyncio.to_thread(self.dig_service.dig_repo.update_tunnel, user.id, guild_id, last_dig_at=0)
        await interaction.response.send_message(
            f"Set {user.mention} to depth **{depth}** and reset cooldown.", ephemeral=True,
        )

    # ------------------------------------------------------------------
    # 18. /dig profile/about/build — Miner customization
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # /dig miner subgroup — profile, about, build
    # ------------------------------------------------------------------
    miner = app_commands.Group(name="miner", description="Miner profile and S stats", parent=dig)

    @miner.command(name="profile", description="View your miner profile and S stats")
    async def dig_profile(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.dig_service.get_miner_profile,
            interaction.user.id,
            guild_id,
        )
        if not result.get("success"):
            await interaction.response.send_message(result.get("error", "No profile."), ephemeral=True)
            return

        embed = discord.Embed(
            title=f"{interaction.user.display_name} - Miner Profile",
            color=0x5865F2,
        )
        embed.description = _backstory_text(result)
        embed.add_field(
            name="S Stats",
            value=_format_s_stats(result.get("stats", {}), result.get("effects", {})),
            inline=False,
        )
        embed.set_footer(text="Backstory locks after you set it. Boss first clears grant one extra S point.")
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @miner.command(name="about", description="Set your miner backstory once")
    @app_commands.describe(
        backstory="Short backstory blurb for the AI Dungeon Master",
    )
    async def dig_about(
        self,
        interaction: discord.Interaction,
        backstory: str,
    ):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.dig_service.set_miner_profile,
            interaction.user.id,
            guild_id,
            backstory=backstory,
        )
        if not result.get("success"):
            await interaction.response.send_message(result.get("error", "Profile update failed."), ephemeral=True)
            return

        embed = discord.Embed(
            title="Backstory Locked In",
            description=_backstory_text(result),
            color=0x5865F2,
        )
        embed.set_footer(text="This cannot be changed later.")
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @miner.command(name="build", description="Spend unallocated points on Strength, Smarts, and Stamina")
    @app_commands.describe(
        strength="Points to add. Every 2 total points raises max advance; every 5 raises min.",
        smarts="Points to add. Each total point reduces cave-in chance by 2%.",
        stamina="Points to add. Each total point reduces cooldowns and paid digs by 4%.",
    )
    async def dig_build(
        self,
        interaction: discord.Interaction,
        strength: int = 0,
        smarts: int = 0,
        stamina: int = 0,
    ):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.dig_service.set_miner_stats,
            interaction.user.id,
            guild_id,
            strength=strength,
            smarts=smarts,
            stamina=stamina,
        )
        if not result.get("success"):
            await interaction.response.send_message(result.get("error", "Build update failed."), ephemeral=True)
            return

        embed = discord.Embed(
            title="S Points Spent",
            description=_format_s_stats(result.get("stats", {}), result.get("effects", {})),
            color=0x5865F2,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    # ------------------------------------------------------------------
    # 19. /dig guide — Paginated help
    # ------------------------------------------------------------------

    @dig.command(name="mode", description="Switch between legacy and DM Mode (AI-narrated) dig")
    @app_commands.describe(mode="Choose your dig experience")
    @app_commands.choices(mode=[
        app_commands.Choice(name="Legacy (deterministic)", value="legacy"),
        app_commands.Choice(name="DM Mode (AI-narrated)", value="llm"),
    ])
    async def dig_mode(self, interaction: discord.Interaction, mode: app_commands.Choice[str]):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None

        if mode.value == "llm" and not self.dig_llm_service:
            await interaction.response.send_message(
                "DM Mode is not available (AI service not configured).", ephemeral=True,
            )
            return

        # Ensure tunnel exists before setting mode (create one if needed)
        tunnel = await asyncio.to_thread(
            self.dig_service.dig_repo.get_tunnel,
            interaction.user.id, guild_id,
        )
        if not tunnel:
            await asyncio.to_thread(
                self.dig_service.dig_repo.create_tunnel,
                interaction.user.id, guild_id, f"{interaction.user.display_name}'s Tunnel",
            )

        await asyncio.to_thread(
            self.dig_service.dig_repo.set_engine_mode,
            interaction.user.id, guild_id, mode.value,
        )

        if mode.value == "llm":
            desc = "**DM Mode enabled.** Your digs will now be narrated by an AI Dungeon Master with personalized storytelling."
        else:
            desc = "**Legacy mode enabled.** Your digs will use the standard deterministic engine."

        embed = discord.Embed(
            title="Dig Mode Updated",
            description=desc,
            color=0x5865F2,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @dig.command(name="guide", description="Learn how to dig")
    async def dig_guide(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        view = DigGuideView()
        await interaction.response.send_message(embed=GUIDE_PAGES[0], view=view)


# ---------------------------------------------------------------------------
# Embed builder for normal dig results
# ---------------------------------------------------------------------------

def _build_dig_embed(result: object, user: discord.User | discord.Member) -> tuple[discord.Embed, str | None, int, list[str]]:
    """Build a rich embed for a normal dig result. Returns (embed, layer_name, pickaxe_tier, items_used_ids)."""
    depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
    tunnel_name = getattr(result, "tunnel_name", "Tunnel")
    pickaxe_tier = getattr(result, "pickaxe_tier", 0) or 0

    # Determine layer for embed color
    layer_def = get_layer_def(depth)
    layer_name = layer_def.name if layer_def else None

    # ~20% chance of a "Dig Dug" themed title
    if random.random() < 0.20:
        title = f"{random.choice(DIG_DUG_TITLES)} \u2014 Depth {depth}"
    else:
        title = f"{tunnel_name} \u2014 Depth {depth}"

    embed = discord.Embed(
        title=title,
        color=_layer_color(layer_name),
    )

    # LLM narrative (DM Mode) — shown as first field when present
    llm_narrative = getattr(result, "llm_narrative", None)
    if llm_narrative:
        embed.add_field(name="\u200b", value=f"*{llm_narrative}*", inline=False)

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
        jc_lost = getattr(cave_in_detail, "jc_lost", 0)
        llm_cave_in = getattr(result, "llm_cave_in_flavor", None)
        cave_in_type = getattr(cave_in_detail, "type", "") if not isinstance(cave_in_detail, dict) else cave_in_detail.get("type", "")
        if llm_cave_in:
            message = llm_cave_in
        elif llm_narrative:
            # DM narrative tells the story — show only the mechanical consequence
            consequence = {
                "stun": "Stunned — next dig has longer cooldown!",
                "injury": "Injured — reduced digging for several digs!",
                "medical_bill": "",  # JC loss shown via jc_lost below
            }.get(cave_in_type, "")
            message = consequence
        else:
            message = getattr(cave_in_detail, "message", "")
        cave_in_text = f"Lost **{block_loss}** blocks"
        if jc_lost:
            cave_in_text += f" and **{jc_lost}** {JOPACOIN_EMOTE}"
        if message:
            cave_in_text += f". {message}"
        embed.add_field(
            name="Cave-in!",
            value=cave_in_text,
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

    # Event (with ASCII art for simple events).
    # Skip this field for choice/boon events — they get their own encounter
    # embed with art, so showing the text here would be a duplicate.
    event = getattr(result, "event", None)
    if event:
        event_dict = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
        has_encounter_ui = isinstance(event_dict, dict) and (
            event_dict.get("safe_option") or event_dict.get("boon_options")
        )
        if has_encounter_ui:
            event = None  # suppress from stats embed — encounter UI will show it

    if event:
        # Use LLM event flavor if available, otherwise stock description
        llm_event_flavor = getattr(result, "llm_event_flavor", None)
        if isinstance(event, str):
            e_desc = llm_event_flavor or event
            e_art = None
        else:
            e_desc = llm_event_flavor or pick_description(event) or "Something happens..."
            if isinstance(event, dict):
                e_art = event.get("ascii_art")
            elif hasattr(event, "_d") and isinstance(event._d, dict):
                e_art = event._d.get("ascii_art")
            else:
                e_art = getattr(event, "ascii_art", None)
        event_text = e_desc
        if e_art:
            event_text = f"```\n{e_art}\n```\n{e_desc}"
        embed.add_field(
            name="\u200b",
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

    # Corruption effect (P6+)
    corruption = getattr(result, "corruption", None)
    if corruption:
        corr_d = corruption if isinstance(corruption, dict) else (corruption._d if hasattr(corruption, "_d") else {})
        corr_desc = corr_d.get("description", "") if isinstance(corr_d, dict) else ""
        if corr_desc:
            embed.add_field(name="Corruption", value=corr_desc, inline=False)

    # Footer — user + tip
    tip = ""
    if depth == 69:
        tip = "Nice."
    elif random.random() < 0.25:
        tip = random.choice(DIG_DUG_FOOTERS)
    else:
        tip = getattr(result, "tip", "") or _tip(0)

    # Active mutations footer (P8+)
    mutations = getattr(result, "mutations", None)
    if mutations and isinstance(mutations, (list, tuple)):
        mut_names = [str(m) for m in mutations]
        tip = f"Mutations: {', '.join(mut_names)}" + (f" | {tip}" if tip else "")

    # LLM callback reference (appended to footer)
    llm_callback = getattr(result, "llm_callback", None)
    if llm_callback:
        tip = f"{tip} | {llm_callback}" if tip else llm_callback

    embed.set_footer(text=tip)
    embed.set_author(name=user.display_name, icon_url=user.display_avatar.url)

    items_ids = list(getattr(result, "items_used_ids", None) or [])
    return embed, layer_name, pickaxe_tier, items_ids


async def _attach_layer_thumbnail(embed: discord.Embed, layer_name: str | None) -> discord.File | None:
    """Fetch layer thumbnail in a thread and attach to embed."""
    if not layer_name:
        return None
    try:
        from utils.dig_assets import get_layer_thumbnail
        layer_file = await asyncio.to_thread(get_layer_thumbnail, layer_name)
        if layer_file:
            embed.set_thumbnail(url=f"attachment://{layer_file.filename}")
            return layer_file
    except Exception:
        logger.debug("Layer thumbnail failed for %s", layer_name)
    return None


async def _attach_pickaxe_footer(embed: discord.Embed, pickaxe_tier: int) -> discord.File | None:
    """Add the pickaxe icon to the embed footer (preserving existing footer text)."""
    try:
        from utils.dig_assets import get_pickaxe_art
        pickaxe_file = await asyncio.to_thread(get_pickaxe_art, pickaxe_tier)
        if pickaxe_file:
            footer_text = embed.footer.text if embed.footer else ""
            embed.set_footer(text=footer_text, icon_url=f"attachment://{pickaxe_file.filename}")
            return pickaxe_file
    except Exception:
        pass
    return None


async def _attach_items_strip(embed: discord.Embed, items_ids: list[str]) -> discord.File | None:
    """Compose item icons into a strip and attach as embed image."""
    if not items_ids:
        return None
    try:
        from utils.dig_assets import compose_items_used
        items_file = await asyncio.to_thread(compose_items_used, items_ids)
        if items_file:
            embed.set_image(url=f"attachment://{items_file.filename}")
            return items_file
    except Exception:
        pass
    return None


async def _build_items_used_embed(result: object) -> tuple[discord.Embed, discord.File] | None:
    """Build a tiny embed showing consumed item icons, or None if no items used."""
    items_used_ids = getattr(result, "items_used_ids", None)
    items_used = getattr(result, "items_used", None)
    if not items_used_ids:
        return None
    try:
        from services.dig_constants import CONSUMABLE_ITEMS
        from utils.dig_assets import compose_items_used
        items_file = await asyncio.to_thread(compose_items_used, list(items_used_ids))
        if not items_file:
            # No art files — fall back to text-only if we have display names
            if not items_used:
                return None
            label = ", ".join(str(i) for i in items_used)
            embed = discord.Embed(description=f"**Items Used:** {label}", color=0x2F3136)
            return embed, None  # type: ignore[return-value]
        # Derive label from display names or constants
        if items_used:
            label = ", ".join(str(i) for i in items_used)
        else:
            label = ", ".join(
                CONSUMABLE_ITEMS.get(iid, {}).get("name", iid) for iid in items_used_ids
            )
        embed = discord.Embed(
            description=f"**Items Used:** {label}",
            color=0x2F3136,
        )
        embed.set_thumbnail(url=f"attachment://{items_file.filename}")
        return embed, items_file
    except Exception:
        logger.debug("Items used embed failed")
        return None


# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

async def setup(bot: commands.Bot):
    dig_service = getattr(bot, "dig_service", None)
    if dig_service is None:
        raise RuntimeError("Dig service not registered on bot.")
    dig_llm_service = getattr(bot, "dig_llm_service", None)
    await bot.add_cog(DigCommands(bot, dig_service, dig_llm_service))
