import asyncio
import logging

import discord
from discord import app_commands
from discord.ext import commands

from services.reminder_service import ReminderService

logger = logging.getLogger("cama_bot.reminders")

_TYPE_LABELS = {
    "wheel": ("Gamba/Wheel", "DM when your wheel cooldown expires"),
    "trivia": ("Trivia", "DM when your trivia cooldown expires"),
    "betting": ("Betting", "DM when a new match opens for betting"),
    "dig": ("Dig", "DM when your free dig cooldown expires"),
}


def _status_text(prefs: dict) -> str:
    lines = ["**Your reminder settings:**\n"]
    for rtype, (label, desc) in _TYPE_LABELS.items():
        enabled = prefs.get(f"{rtype}_enabled", False)
        icon = "🔔" if enabled else "🔕"
        lines.append(f"{icon} **{label}** — {desc}")
    return "\n".join(lines)


class ReminderToggleView(discord.ui.View):
    def __init__(
        self,
        cog: "ReminderCommands",
        prefs: dict,
        discord_id: int,
        guild_id: int,
    ):
        super().__init__(timeout=120)
        self.cog = cog
        self.discord_id = discord_id
        self.guild_id = guild_id
        self._rebuild_buttons(prefs)

    def _rebuild_buttons(self, prefs: dict) -> None:
        self.clear_items()
        for rtype, (label, _desc) in _TYPE_LABELS.items():
            enabled = prefs.get(f"{rtype}_enabled", False)
            btn = discord.ui.Button(
                label=f"{label}: {'ON' if enabled else 'OFF'}",
                emoji="🔔" if enabled else "🔕",
                style=discord.ButtonStyle.success if enabled else discord.ButtonStyle.secondary,
                custom_id=f"reminder_{rtype}",
            )
            btn.callback = self._make_callback(rtype)
            self.add_item(btn)

    def _make_callback(self, rtype: str):
        async def callback(interaction: discord.Interaction) -> None:
            await asyncio.to_thread(
                self.cog.reminder_service.toggle_preference,
                self.discord_id,
                self.guild_id,
                rtype,
            )
            prefs = await asyncio.to_thread(
                self.cog.reminder_service.get_preferences,
                self.discord_id,
                self.guild_id,
            )
            self._rebuild_buttons(prefs)
            await interaction.response.edit_message(content=_status_text(prefs), view=self)

        return callback


class ReminderCommands(commands.Cog):
    def __init__(self, bot: commands.Bot, reminder_service: ReminderService):
        self.bot = bot
        self.reminder_service = reminder_service

    @app_commands.command(
        name="setreminder",
        description="Configure DM reminders for cooldowns and match betting windows.",
    )
    async def setreminder(self, interaction: discord.Interaction) -> None:
        discord_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else 0
        prefs = await asyncio.to_thread(
            self.reminder_service.get_preferences, discord_id, guild_id
        )
        view = ReminderToggleView(self, prefs, discord_id, guild_id)
        await interaction.response.send_message(
            content=_status_text(prefs), view=view, ephemeral=True
        )


async def setup(bot: commands.Bot) -> None:
    reminder_service = getattr(bot, "reminder_service", None)
    if reminder_service is None:
        logger.warning("ReminderService not found on bot; skipping ReminderCommands cog")
        return
    await bot.add_cog(ReminderCommands(bot, reminder_service))
