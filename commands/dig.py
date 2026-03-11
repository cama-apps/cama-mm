"""
Tunnel digging minigame commands.
"""

from __future__ import annotations

import asyncio
import logging
import random
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_gamba_channel
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

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
}

PROGRESSIVE_TIPS = [
    "Tip: Use /dig_shop to buy consumables that help you dig faster.",
    "Tip: /dig_help <user> lets you assist a friend's tunnel.",
    "Tip: Set a /dig_trap to punish would-be saboteurs.",
    "Tip: /dig_insure protects you from catastrophic cave-ins.",
    "Tip: Prestige resets depth but unlocks powerful perks.",
    "Tip: Bosses guard layer transitions. Bring friends to cheer!",
    "Tip: Relics from /dig_museum are rare — gift duplicates to friends.",
    "Tip: Higher pickaxe tiers dig more blocks per action.",
    "Tip: Streaks grant bonus JC — keep digging daily!",
    "Tip: /dig_flex shows off your achievements and titles.",
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
            "The mine has five layers: **Dirt**, **Stone**, **Crystal**, **Magma**, "
            "and **Abyss**. Each layer is harder but more rewarding.\n\n"
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
            "Buy consumables from `/dig_shop` and queue them with `/dig_use`. "
            "You can hold up to 5 items at a time. Queued items are used on "
            "your next dig.\n\n"
            "**Pickaxes**\n"
            "Upgrade your pickaxe with `/dig_upgrade`. Higher tiers require depth "
            "milestones, JC, and prestige levels. Better pickaxes dig more blocks "
            "per action.\n\n"
            "**Relics**\n"
            "Rare artifacts found while digging. Equip them for passive bonuses. "
            "Gift duplicates to friends with `/dig_gift`."
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
            "them all and fill the `/dig_museum`!"
        ),
        color=LAYER_COLORS["Magma"],
    ),
]


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

def _layer_color(layer: str | None) -> int:
    """Return embed color for a layer name, defaulting to Dirt brown."""
    if layer is None:
        return LAYER_COLORS["Dirt"]
    return LAYER_COLORS.get(layer, LAYER_COLORS["Dirt"])


def _tip(index: int) -> str:
    """Return a rotating progressive tip."""
    return PROGRESSIVE_TIPS[index % len(PROGRESSIVE_TIPS)]


async def _check_registered(interaction: discord.Interaction, bot: commands.Bot):
    """Return the Player if registered, else send an ephemeral error and return None."""
    guild_id = interaction.guild.id if interaction.guild else None
    player = await asyncio.to_thread(bot.player_service.get_player, interaction.user.id, guild_id)
    if not player:
        await interaction.response.send_message(
            "You must be registered first. Use `/register`.", ephemeral=True
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

        await interaction.response.defer()
        try:
            self.result = await asyncio.to_thread(
                self.dig_service.fight_boss,
                self.user_id,
                self.guild_id,
                tier,
                amount,
            )
            embed = discord.Embed(
                title="Boss Fight Result",
                color=0x00FF00 if getattr(self.result, "won", False) else 0xFF0000,
            )
            if getattr(self.result, "won", False):
                embed.description = (
                    f"Victory! You defeated the boss and earned "
                    f"**{getattr(self.result, 'reward', 0)}** {JOPACOIN_EMOTE}!"
                )
            else:
                embed.description = (
                    f"Defeat! The boss overpowered you. "
                    f"You lost **{getattr(self.result, 'loss', amount)}** {JOPACOIN_EMOTE}."
                )
            await interaction.followup.send(embed=embed)
        except ValueError as e:
            await interaction.followup.send(str(e), ephemeral=True)
        except Exception as e:
            logger.error("Boss fight error: %s", e)
            await interaction.followup.send("Boss fight failed. Try again.", ephemeral=True)


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
            result = await asyncio.to_thread(
                self.dig_service.retreat_boss, self.user_id, self.guild_id
            )
            await interaction.followup.send(
                f"You retreated safely. {getattr(result, 'message', 'The boss waits...')}"
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
            info = await asyncio.to_thread(
                self.dig_service.scout_boss, self.user_id, self.guild_id
            )
            embed = discord.Embed(
                title="Boss Scouted",
                description=getattr(info, "description", str(info)),
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
            result = await asyncio.to_thread(
                self.dig_service.cheer_boss,
                self.user_id,
                interaction.user.id,
                self.guild_id,
            )
            await interaction.followup.send(
                f"{interaction.user.display_name} cheers for the fighter! "
                f"{getattr(result, 'message', 'Morale boosted!')}"
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
                result = await asyncio.to_thread(
                    self.dig_service.prestige,
                    self.user_id,
                    self.guild_id,
                    perk.get("id", index),
                )
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
            result = await asyncio.to_thread(
                self.dig_service.upgrade_pickaxe, self.user_id, self.guild_id
            )
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
                app_commands.Choice(name=item.get("name", str(item)), value=item.get("name", str(item)))
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

    @app_commands.command(name="dig", description="Dig deeper into your tunnel")
    async def dig(self, interaction: discord.Interaction):
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
            result = await asyncio.to_thread(
                self.dig_service.dig, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Dig error: %s", e)
            await safe_followup(interaction, content="Dig failed. Try again later.", ephemeral=True)
            return

        # First dig welcome
        if getattr(result, "first_dig", False):
            embed = discord.Embed(
                title="Welcome to the Mines!",
                description=(
                    "You've started digging your very own tunnel!\n\n"
                    "Use `/dig` to advance deeper, `/dig_shop` to buy items, "
                    "and `/dig_guide` for a full tutorial.\n\n"
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
                title=f"Boss Encountered!",
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
        if getattr(result, "paid_dig", False):
            cost = getattr(result, "cost", 0)
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
                        paid_result = await asyncio.to_thread(
                            self.dig_service.dig, interaction.user.id, guild_id, paid=True
                        )
                        paid_embed = _build_dig_embed(paid_result, interaction.user)
                        await msg.edit(embed=paid_embed, view=None)
                    except Exception as e:
                        logger.error("Paid dig error: %s", e)
                        await msg.edit(content="Paid dig failed.", embed=None, view=None)
                else:
                    await msg.edit(content="Dig cancelled.", embed=None, view=None)
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

    @app_commands.command(name="dig_help", description="Help another player's tunnel")
    @app_commands.describe(user="The player to help")
    async def dig_help(self, interaction: discord.Interaction, user: discord.Member):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = await asyncio.to_thread(
                self.dig_service.help_tunnel,
                interaction.user.id,
                user.id,
                guild_id,
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Dig help error: %s", e)
            await safe_followup(interaction, content="Help failed.", ephemeral=True)
            return

        embed = discord.Embed(
            title="Tunnel Assistance",
            description=(
                f"You helped **{user.display_name}**'s tunnel!\n"
                f"Blocks added: **{getattr(result, 'blocks_added', '?')}**"
            ),
            color=0x2ECC71,
        )
        cooldown = getattr(result, "cooldown_remaining", None)
        if cooldown:
            embed.set_footer(text=f"Help cooldown: {cooldown}s")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 3. /dig_sabotage — Sabotage another player
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_sabotage", description="Sabotage another player's tunnel")
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
            preview = await asyncio.to_thread(
                self.dig_service.preview_sabotage,
                interaction.user.id,
                user.id,
                guild_id,
            )
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Sabotage preview error: %s", e)
            await interaction.response.send_message("Sabotage failed.", ephemeral=True)
            return

        cost = getattr(preview, "cost", 0)
        damage_range = getattr(preview, "damage_range", "unknown")

        view = ConfirmSabotageView(interaction.user.id, user, cost, str(damage_range))
        embed = view.build_embed()
        await interaction.response.send_message(embed=embed, view=view)
        await view.wait()

        if view.value:
            try:
                result = await asyncio.to_thread(
                    self.dig_service.sabotage_tunnel,
                    interaction.user.id,
                    user.id,
                    guild_id,
                )
                result_embed = discord.Embed(color=0x2C2F33)
                if getattr(result, "trap_triggered", False):
                    result_embed.title = "Trap Triggered!"
                    result_embed.description = (
                        f"Your sabotage attempt backfired! "
                        f"You took **{getattr(result, 'trap_damage', '?')}** damage."
                    )
                    result_embed.color = 0xFF0000
                else:
                    result_embed.title = "Sabotage Successful"
                    result_embed.description = (
                        f"You sabotaged **{user.display_name}**'s tunnel!\n"
                        f"Damage dealt: **{getattr(result, 'damage', '?')}** blocks"
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
    # 4. /dig_info — View tunnel info
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_info", description="View tunnel information")
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

        layer = getattr(info, "layer", "Dirt")
        display_user = user or interaction.user
        embed = discord.Embed(
            title=f"{display_user.display_name}'s Tunnel",
            color=_layer_color(layer),
        )

        # Core stats
        depth = getattr(info, "depth", 0)
        prestige = getattr(info, "prestige_level", 0)
        pickaxe = getattr(info, "pickaxe_tier", "Wooden")
        prestige_text = f" (Prestige {prestige})" if prestige else ""
        embed.add_field(
            name="Depth",
            value=f"**{depth}** blocks — {layer}{prestige_text}",
            inline=True,
        )
        embed.add_field(name="Pickaxe", value=pickaxe, inline=True)

        # Balance
        balance = getattr(info, "balance", None)
        if balance is not None:
            embed.add_field(name="Balance", value=f"{balance} {JOPACOIN_EMOTE}", inline=True)

        # Equipped relics
        relics = getattr(info, "relics", None)
        if relics:
            relic_text = ", ".join(r.get("name", "?") for r in relics)
            embed.add_field(name="Relics", value=relic_text, inline=False)

        # Queued items
        queued = getattr(info, "queued_items", None)
        if queued:
            item_text = ", ".join(i.get("name", "?") for i in queued)
            embed.add_field(name="Queued Items", value=item_text, inline=False)

        # Boss status
        boss = getattr(info, "boss_status", None)
        if boss:
            embed.add_field(name="Boss", value=str(boss), inline=True)

        # Insurance / reinforcement
        insurance = getattr(info, "insurance_active", False)
        reinforcement = getattr(info, "reinforcement_active", False)
        status_parts = []
        if insurance:
            status_parts.append("Insured")
        if reinforcement:
            status_parts.append("Reinforced")
        if status_parts:
            embed.add_field(name="Protection", value=", ".join(status_parts), inline=True)

        # Trap status
        trap = getattr(info, "trap_set", False)
        if is_own and trap:
            embed.add_field(name="Trap", value="Armed", inline=True)
        elif not is_own and trap:
            embed.add_field(name="Trap", value="Something feels off...", inline=True)

        # Recent helpers
        helpers = getattr(info, "recent_helpers", None)
        if helpers:
            helper_text = ", ".join(str(h) for h in helpers[:5])
            embed.add_field(name="Recent Helpers", value=helper_text, inline=False)

        # Streak
        streak = getattr(info, "streak", 0)
        if streak:
            embed.add_field(name="Streak", value=f"{streak} digs", inline=True)

        # Recent events
        events = getattr(info, "recent_events", None)
        if events:
            event_text = "\n".join(str(e) for e in events[:5])
            embed.add_field(name="Recent Events", value=event_text, inline=False)

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

    @app_commands.command(name="dig_leaderboard", description="View top tunnels")
    async def dig_leaderboard(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            lb = await asyncio.to_thread(
                self.dig_service.get_leaderboard, guild_id
            )
        except Exception as e:
            logger.error("Leaderboard error: %s", e)
            await safe_followup(interaction, content="Leaderboard unavailable.", ephemeral=True)
            return

        entries = getattr(lb, "entries", lb) if lb else []
        if not entries:
            await safe_followup(interaction, content="No tunnels yet! Use `/dig` to start.", ephemeral=True)
            return

        # Build leaderboard text
        lines = []
        max_depth = max(getattr(e, "depth", 0) for e in entries[:10]) or 1
        for i, entry in enumerate(entries[:10], 1):
            name = getattr(entry, "name", f"Player {getattr(entry, 'user_id', '?')}")
            depth = getattr(entry, "depth", 0)
            layer = getattr(entry, "layer", "Dirt")
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
    # 6. /dig_use — Queue consumable
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_use", description="Queue a consumable for your next dig")
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
            result = await asyncio.to_thread(
                self.dig_service.use_item, interaction.user.id, guild_id, item
            )
            await safe_followup(
                interaction,
                content=f"**{item}** queued for your next dig. {getattr(result, 'message', '')}",
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Dig use error: %s", e)
            await safe_followup(interaction, content="Failed to queue item.", ephemeral=True)

    # ------------------------------------------------------------------
    # 7. /dig_gift — Gift a relic
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_gift", description="Gift a relic to another player")
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
            result = await asyncio.to_thread(
                self.dig_service.gift_relic,
                interaction.user.id,
                user.id,
                guild_id,
                artifact,
            )
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
    # 8. /dig_shop — Show dig-specific items
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_shop", description="Browse the mining shop")
    async def dig_shop(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            shop = await asyncio.to_thread(
                self.dig_service.get_shop, interaction.user.id, guild_id
            )
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
        embed.set_footer(text=f"Your inventory: {inv_count}/5 items | Use /dig_use <item> to queue")

        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 9. /dig_museum — Guild artifact museum
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_museum", description="View the guild artifact museum")
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
        artifacts_by_layer = getattr(museum, "artifacts_by_layer", {})

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

    @app_commands.command(name="dig_flex", description="Show off your mining achievements")
    async def dig_flex(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            flex = await asyncio.to_thread(
                self.dig_service.get_flex_data, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Flex error: %s", e)
            await safe_followup(interaction, content="Flex unavailable.", ephemeral=True)
            return

        embed = discord.Embed(
            title=f"{interaction.user.display_name}'s Mining Profile",
            color=0xFFD700,
        )

        # Title
        title = getattr(flex, "title", None)
        if title:
            embed.description = f"*\"{title}\"*"

        # Prestige
        prestige = getattr(flex, "prestige_level", 0)
        if prestige:
            embed.add_field(name="Prestige Level", value=str(prestige), inline=True)

        # Stats
        stats = getattr(flex, "stats", {})
        if stats:
            stats_text = (
                f"Total digs: {stats.get('total_digs', 0)}\n"
                f"Max depth: {stats.get('max_depth', 0)}\n"
                f"Total JC earned: {stats.get('total_jc_earned', 0)}\n"
                f"Cave-ins survived: {stats.get('cave_ins_survived', 0)}\n"
                f"Bosses defeated: {stats.get('bosses_defeated', 0)}\n"
                f"Sabotages dealt: {stats.get('sabotages_dealt', 0)}\n"
                f"Sabotages received: {stats.get('sabotages_received', 0)}"
            )
            embed.add_field(name="Stats", value=stats_text, inline=False)

        # Achievements
        achievements = getattr(flex, "achievements", [])
        if achievements:
            ach_text = "\n".join(f"- {a}" for a in achievements[:10])
            embed.add_field(name="Achievements", value=ach_text, inline=False)

        embed.set_thumbnail(url=interaction.user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 11. /dig_abandon — Abandon tunnel
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_abandon", description="Abandon your tunnel (partial refund)")
    async def dig_abandon(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id if interaction.guild else None

        try:
            preview = await asyncio.to_thread(
                self.dig_service.preview_abandon, interaction.user.id, guild_id
            )
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
                result = await asyncio.to_thread(
                    self.dig_service.abandon_tunnel, interaction.user.id, guild_id
                )
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
    # 12. /dig_upgrade — View/buy pickaxe upgrades
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_upgrade", description="View or buy pickaxe upgrades")
    async def dig_upgrade(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            info = await asyncio.to_thread(
                self.dig_service.get_upgrade_info, interaction.user.id, guild_id
            )
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

    @app_commands.command(name="dig_trap", description="Set a trap in your tunnel")
    async def dig_trap(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = await asyncio.to_thread(
                self.dig_service.set_trap, interaction.user.id, guild_id
            )
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

    @app_commands.command(name="dig_insure", description="Buy cave-in insurance")
    async def dig_insure(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = await asyncio.to_thread(
                self.dig_service.buy_insurance, interaction.user.id, guild_id
            )
            cost = getattr(result, "cost", 0)
            duration = getattr(result, "duration", "?")
            await safe_followup(
                interaction,
                content=(
                    f"Insurance purchased for **{cost}** {JOPACOIN_EMOTE}! "
                    f"Duration: {duration}."
                ),
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Insurance error: %s", e)
            await safe_followup(interaction, content="Failed to buy insurance.", ephemeral=True)

    # ------------------------------------------------------------------
    # 15. /dig_inventory — View items
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_inventory", description="View your mining inventory")
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
            embed.set_footer(text=f"{len(items)}/5 slots used")
        else:
            embed.description = "Your inventory is empty. Visit `/dig_shop` to buy items."
            embed.set_footer(text="0/5 slots used")

        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 16. /dig_guide — Paginated help
    # ------------------------------------------------------------------

    @app_commands.command(name="dig_guide", description="Learn how to dig")
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
    layer = getattr(result, "layer", "Dirt")
    depth = getattr(result, "depth", 0)
    tunnel_name = getattr(result, "tunnel_name", "Tunnel")

    # ~20% chance of a "Dig Dug" themed title
    if random.random() < 0.20:
        title = f"\u26cf\ufe0f {random.choice(DIG_DUG_TITLES)} \u2014 Depth {depth}"
    else:
        title = f"\u26cf\ufe0f {tunnel_name} \u2014 Depth {depth}"

    embed = discord.Embed(
        title=title,
        color=_layer_color(layer),
    )

    # Blocks gained and JC earned
    blocks = getattr(result, "blocks_gained", 0)
    jc = getattr(result, "jc_earned", 0)
    embed.add_field(
        name="Progress",
        value=f"+{blocks} blocks | +{jc} {JOPACOIN_EMOTE}",
        inline=False,
    )

    # Cave-in
    cave_in = getattr(result, "cave_in", None)
    if cave_in:
        embed.add_field(
            name="Cave-in!",
            value=f"Lost **{getattr(cave_in, 'blocks_lost', '?')}** blocks. {getattr(cave_in, 'message', '')}",
            inline=False,
        )

    # Milestone bonus
    milestone = getattr(result, "milestone_bonus", None)
    if milestone:
        embed.add_field(
            name="DIG DUG! Milestone!",
            value=f"+{getattr(milestone, 'bonus_jc', '?')} {JOPACOIN_EMOTE} — {getattr(milestone, 'description', '')}",
            inline=False,
        )

    # Streak bonus
    streak = getattr(result, "streak_bonus", None)
    if streak:
        embed.add_field(
            name="Streak Bonus",
            value=f"+{getattr(streak, 'bonus_jc', '?')} {JOPACOIN_EMOTE} (x{getattr(streak, 'streak', '?')})",
            inline=True,
        )

    # Artifact found
    artifact = getattr(result, "artifact", None)
    if artifact:
        embed.add_field(
            name="Artifact Found!",
            value=f"**{getattr(artifact, 'name', '?')}** — {getattr(artifact, 'description', '')}",
            inline=False,
        )

    # Event
    event = getattr(result, "event", None)
    if event:
        embed.add_field(
            name="Event",
            value=getattr(event, "description", str(event)),
            inline=False,
        )

    # Items used
    items_used = getattr(result, "items_used", None)
    if items_used:
        item_names = ", ".join(getattr(i, "name", str(i)) for i in items_used)
        embed.add_field(name="Items Used", value=item_names, inline=True)

    # Footer tip — ~25% chance of a Dig Dug footer instead
    dig_count = getattr(result, "total_digs", 0)
    if random.random() < 0.25:
        embed.set_footer(text=random.choice(DIG_DUG_FOOTERS))
    else:
        embed.set_footer(text=_tip(dig_count))
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
