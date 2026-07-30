"""Public, paid Luke-blaming apparatus."""

from __future__ import annotations

import asyncio
import logging
import random

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from services.player_service import PlayerService
from utils.blame_luke_drawing import BLAME_LUKE_REASONS, create_blame_luke_gif
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.blame_luke")

BLAME_LUKE_COST = 10


class BlameLukeView(discord.ui.View):
    """A public launcher where each clicker pays for their own accusation."""

    def __init__(self, *, player_service: PlayerService):
        super().__init__(timeout=None)
        self.player_service = player_service

    async def _refund(self, *, user_id: int, guild_id: int) -> None:
        await asyncio.to_thread(
            self.player_service.adjust_balance,
            user_id,
            guild_id,
            BLAME_LUKE_COST,
            source="blame_luke_refund",
            actor_id=user_id,
            related_type="blame_luke",
            reason="Blame Luke delivery refund",
        )

    @discord.ui.button(
        label="Blame Luke",
        style=discord.ButtonStyle.danger,
        emoji="📌",
        custom_id="blame_luke:investigate",
    )
    async def blame_luke(
        self,
        interaction: discord.Interaction,
        button: discord.ui.Button,
    ):
        user_id = interaction.user.id
        guild_id = interaction.guild.id
        selected_index = random.randrange(len(BLAME_LUKE_REASONS))
        reason = BLAME_LUKE_REASONS[selected_index]

        try:
            charged = await asyncio.to_thread(
                self.player_service.try_spend,
                user_id,
                guild_id,
                BLAME_LUKE_COST,
                source="blame_luke",
                actor_id=user_id,
                related_type="blame_luke",
                related_id=selected_index,
                reason="Blame Luke animation",
            )
        except Exception:
            logger.exception(
                "Failed to charge Blame Luke user_id=%s guild_id=%s",
                user_id,
                guild_id,
            )
            await interaction.response.send_message(
                "The blame apparatus couldn't reach your wallet. You were not charged.",
                ephemeral=True,
            )
            return

        if not charged:
            player = await asyncio.to_thread(
                self.player_service.get_player,
                user_id,
                guild_id,
            )
            if player is None:
                content = "Register first with `/player register` to fund an investigation."
            else:
                content = "The Ministry rejected your funding request."
            await interaction.response.send_message(content, ephemeral=True)
            return

        if not await safe_defer(interaction, ephemeral=False, thinking=True):
            await self._refund(user_id=user_id, guild_id=guild_id)
            return

        try:
            buffer = await asyncio.to_thread(create_blame_luke_gif, reason)
        except Exception:
            logger.exception("Failed to render the Luke blame apparatus")
            await self._refund(user_id=user_id, guild_id=guild_id)
            await safe_followup(
                interaction,
                content=(
                    "The blame apparatus jammed. Luke is probably responsible. "
                    "Your payment was refunded."
                ),
                ephemeral=False,
            )
            return

        embed = discord.Embed(
            title="📌 Finding of the Ministry of Accountability",
            description=f"**{reason}**",
            color=0xC43036,
        )
        embed.set_image(url="attachment://blame_luke.gif")
        embed.set_footer(text=f"Filed by {interaction.user.display_name}")
        try:
            await safe_followup(
                interaction,
                embed=embed,
                file=discord.File(buffer, filename="blame_luke.gif"),
                ephemeral=False,
                allowed_mentions=discord.AllowedMentions.none(),
            )
        except Exception:
            logger.exception(
                "Failed to deliver Blame Luke result user_id=%s guild_id=%s",
                user_id,
                guild_id,
            )
            await self._refund(user_id=user_id, guild_id=guild_id)


class BlameLukeCommands(commands.Cog):
    """Public launcher for the Jopacoin Accountability Office."""

    def __init__(self, bot: commands.Bot, player_service: PlayerService):
        self.bot = bot
        self.player_service = player_service

    @app_commands.command(
        name="blameluke",
        description="Discover why this is Luke's fault",
    )
    @require_guild
    async def blameluke(self, interaction: discord.Interaction):
        embed = discord.Embed(
            title="Ministry of Accountability",
            description=(
                "Need a culprit? Press the button and the impartial blame apparatus "
                "will investigate."
            ),
            color=0xD9A62E,
        )
        embed.set_footer(text="Anyone can press it. The clicker pays.")
        await interaction.response.send_message(
            embed=embed,
            view=BlameLukeView(player_service=self.player_service),
            allowed_mentions=discord.AllowedMentions.none(),
        )


async def setup(bot: commands.Bot):
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")
    cog = BlameLukeCommands(bot, player_service)
    await bot.add_cog(cog)
    bot.add_view(BlameLukeView(player_service=player_service))
