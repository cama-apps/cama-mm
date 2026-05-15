import logging
import random

import discord
from discord.ext import commands

import config

logger = logging.getLogger("cama_bot.ping_rewards")

PING_REWARD_JC = 100


class PingRewardsCog(commands.Cog):
    def __init__(self, bot: commands.Bot) -> None:
        self.bot = bot

    @commands.Cog.listener()
    async def on_message(self, message: discord.Message) -> None:
        if message.author.bot or message.guild is None:
            return

        mentioned_ids = {u.id for u in message.mentions}
        guild_id = message.guild.id
        pinger_id = message.author.id

        if config.LUKE_DISCORD_ID and config.LUKE_DISCORD_ID in mentioned_ids:
            await self._maybe_reward_luke_ping(pinger_id, guild_id, message.channel)

        if config.ASH_DISCORD_ID and config.ASH_DISCORD_ID in mentioned_ids:
            await self._maybe_reward_ash_ping(pinger_id, guild_id, message.channel)

    async def _maybe_reward_luke_ping(
        self,
        pinger_id: int,
        guild_id: int,
        channel: discord.abc.Messageable,
    ) -> None:
        if random.random() >= 0.05:
            return
        player_svc = getattr(self.bot, "player_service", None)
        if player_svc is None:
            return
        try:
            player_svc.adjust_balance(pinger_id, guild_id, PING_REWARD_JC)
            await channel.send(
                f"<@{pinger_id}> pinged Luke and got lucky — **+{PING_REWARD_JC} JC**! 🎰"
            )
        except Exception:
            logger.exception("ping_rewards: failed to award JC for luke ping (pinger=%d)", pinger_id)

    async def _maybe_reward_ash_ping(
        self,
        pinger_id: int,
        guild_id: int,
        channel: discord.abc.Messageable,
    ) -> None:
        player_svc = getattr(self.bot, "player_service", None)

        if random.random() < 0.01 and player_svc is not None:
            try:
                player_svc.adjust_balance(pinger_id, guild_id, PING_REWARD_JC)
                await channel.send(
                    f"<@{pinger_id}> pinged Ash and got lucky — **+{PING_REWARD_JC} JC**! 🎰"
                )
            except Exception:
                logger.exception("ping_rewards: failed to award JC for ash ping (pinger=%d)", pinger_id)

        if random.random() < 0.01:
            await self._dm_luke(pinger_id)

    async def _dm_luke(self, pinger_id: int) -> None:
        try:
            pinger = self.bot.get_user(pinger_id) or await self.bot.fetch_user(pinger_id)
            luke = self.bot.get_user(config.LUKE_DISCORD_ID) or await self.bot.fetch_user(config.LUKE_DISCORD_ID)
            await luke.send(
                f"Hey Luke, {pinger.display_name} gamba'd and won access to text in ash-tivity. "
                f"Please be a goodmin and give them the role."
            )
        except Exception:
            logger.debug("ping_rewards: failed to DM luke (pinger=%d)", pinger_id)


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(PingRewardsCog(bot))
