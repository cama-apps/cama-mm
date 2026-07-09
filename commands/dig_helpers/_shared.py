"""Shared helpers and constants for the ``/dig`` cog and its views.

Lives in its own module so both ``commands/dig.py`` and the
``commands/dig_helpers/*_views.py`` modules can import these without
creating an import cycle through the cog.
"""

from __future__ import annotations

import asyncio
import random

import discord
from discord.ext import commands

from utils.formatting import JOPACOIN_EMOTE

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
    "Tip: Higher pickaxe tiers dig more blocks per action.",
    "Tip: Streaks grant bonus JC — keep digging daily!",
    "Tip: /dig flex shows off your mining stats and titles.",
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
            "Upgrade your pickaxe from `/dig shop` using `/dig buy`. Higher tiers require depth "
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
            "Some relics are only available at higher prestige levels."
        ),
        color=LAYER_COLORS["Magma"],
    ),
]


# ---------------------------------------------------------------------------
# Result wrapping
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
    # Nested dict values are recursively wrapped, so embed-builder code that
    # calls .get() on what it expects to be a plain dict (e.g. pinnacle_relic)
    # would otherwise hit AttributeError and crash render mid-resolution after
    # the service had already persisted rewards.
    def get(self, key, default=None):
        v = self._d.get(key, default)
        return _DictObj(v) if isinstance(v, dict) else v
    def __repr__(self):
        return repr(self._d)


def _wrap(result):
    """Wrap a service result dict so getattr access works throughout the cog."""
    if isinstance(result, dict):
        return _DictObj(result)
    return result


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------

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
    absorbed = int(splash.get("absorbed_total", 0) or 0) if splash else 0
    shielded = int(splash.get("shielded_count", 0) or 0) if splash else 0
    if absorbed > 0:
        lines.append(
            f"🌾 {shielded} White shield activation(s) absorbed "
            f"{absorbed} {JOPACOIN_EMOTE}."
        )
    return lines


# ---------------------------------------------------------------------------
# Reading the Stone
# ---------------------------------------------------------------------------

_READING_HINTS = {
    "safe": [
        "The walls whisper of patience here.",
        "A familiar rhythm — caution holds today.",
        "Stillness gathers along the safer passage.",
    ],
    "risky": [
        "The stones hum louder beside the bolder path.",
        "Something glints just past the edge of the dark.",
        "An unseen pull tugs you onward.",
    ],
    "desperate": [
        "Old bones remember reckless feet.",
        "The rock itself seems to dare you forward.",
        "A wild current beckons from the deepest dark.",
    ],
}


def _reading_the_stone_hint(event_data: dict) -> str | None:
    """Return an atmospheric whisper toward the highest-EV option, or None.

    Computes a rough EV per option using the authored success_chance and
    success/failure JC values. Picks the best option's direction (safe /
    risky / desperate) and returns one of the flavor lines for that
    direction. Players see only the line, never the math.
    """
    if not isinstance(event_data, dict):
        return None

    def _avg(jc):
        if isinstance(jc, list) and jc:
            return sum(jc) / len(jc)
        if isinstance(jc, (int, float)):
            return float(jc)
        return 0.0

    best_dir = None
    best_ev = None
    for key, direction in (
        ("safe_option", "safe"),
        ("risky_option", "risky"),
        ("desperate_option", "desperate"),
    ):
        opt = event_data.get(key)
        if not isinstance(opt, dict):
            continue
        sc = opt.get("success_chance", 1.0)
        s = opt.get("success") or {}
        f = opt.get("failure") or {}
        ev = sc * _avg(s.get("jc", 0)) + (1.0 - sc) * _avg(f.get("jc", 0))
        if best_ev is None or ev > best_ev:
            best_ev = ev
            best_dir = direction

    if best_dir is None:
        return None
    return random.choice(_READING_HINTS[best_dir])


async def _check_registered(interaction: discord.Interaction, bot: commands.Bot):
    """Return the Player if registered, else send an ephemeral error and return None."""
    guild_id = interaction.guild.id if interaction.guild else None
    player = await asyncio.to_thread(bot.player_service.get_player, interaction.user.id, guild_id)
    if not player:
        await interaction.response.send_message(
            "You must be registered first. Use `/player register`.", ephemeral=True
        )
    return player
