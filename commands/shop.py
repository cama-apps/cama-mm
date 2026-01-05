"""
Shop commands for spending jopacoin.
"""

import logging
import random

import discord
from discord import app_commands
from discord.ext import commands

from config import SHOP_ANNOUNCE_COST, SHOP_ANNOUNCE_TARGET_COST
from services.player_service import PlayerService
from utils.formatting import JOPACOIN_EMOTE
from utils.hero_lookup import get_hero_color, get_hero_image_url
from utils.interaction_safety import safe_defer
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.shop")

# Bounty Hunter theme
BOUNTY_HUNTER_ID = 62
BOUNTY_HUNTER_COLOR = 0xD4AF37  # Gold fallback

# Snarky messages for balance announcements (cost is appended dynamically)
ANNOUNCE_MESSAGES = [
    "BEHOLD! A being of IMMENSE wealth walks among you!",
    "Everyone stop what you're doing. This is important.",
    "Let the record show that on this day, wealth was flexed.",
    "I didn't want to brag, but actually yes I did.",
    "This announcement brought to you by poor financial decisions.",
    "Witness me.",
    "They said money can't buy happiness. They lied.",
    "Is this obnoxious? Yes. Do I care? I paid for this.",
    "POV: You're about to feel poor.",
    "I could have saved this. But I'm built different.",
    "Track THIS, Bounty Hunter.",
    "The jingling of coins is my love language.",
]

# Maximum petty messages when targeting someone
ANNOUNCE_TARGET_MESSAGES = [
    "{user} paid {cost} {emote} specifically to flex on {target}. Worth it.",
    "Attention {target}: {user} has money and you need to know about it.",
    "{target}, you've been summoned to witness {user}'s financial superiority.",
    "{user} wanted {target} to see this. Petty? Absolutely. Expensive? Very.",
    "HEY {target}! {user} spent {cost} {emote} just to get your attention. Feel special.",
    "{user} could have bought {ratio} announcements. Instead, they bought one that bothers {target}.",
    "{target}: You're witnessing a {cost} {emote} flex from {user}. Congratulations?",
    "A moment of silence for {target}, who must now acknowledge {user}'s wealth.",
]


class ShopCommands(commands.Cog):
    """Slash commands to spend jopacoin in the shop."""

    def __init__(self, bot: commands.Bot, player_service: PlayerService):
        self.bot = bot
        self.player_service = player_service

    @app_commands.command(name="shop", description="Spend jopacoin in the shop")
    @app_commands.describe(
        item="What to buy",
        target="User to tag in your announcement (costs more!)",
    )
    @app_commands.choices(
        item=[
            app_commands.Choice(
                name=f"Announce Balance ({SHOP_ANNOUNCE_COST} jopacoin)",
                value="announce",
            ),
        ]
    )
    async def shop(
        self,
        interaction: discord.Interaction,
        item: app_commands.Choice[str],
        target: discord.Member | None = None,
    ):
        """Buy items from the shop with jopacoin."""
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="shop",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=3,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Slow down there, big spender. Wait {rl.retry_after_seconds}s before your next purchase.",
                ephemeral=True,
            )
            return

        if item.value == "announce":
            await self._handle_announce(interaction, target)

    async def _handle_announce(
        self,
        interaction: discord.Interaction,
        target: discord.Member | None,
    ):
        """Handle the balance announcement purchase."""
        user_id = interaction.user.id

        # Determine cost
        cost = SHOP_ANNOUNCE_TARGET_COST if target else SHOP_ANNOUNCE_COST

        # Check if registered
        player = self.player_service.get_player(user_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop. "
                "Hard to flex wealth you don't have.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = self.player_service.get_balance(user_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}. "
                "Maybe try earning some money before flexing?",
                ephemeral=True,
            )
            return

        # Deduct cost
        self.player_service.player_repo.add_balance(user_id, -cost)
        new_balance = balance - cost

        # Build the beautiful embed
        embed = self._create_announce_embed(interaction.user, new_balance, cost, target)

        # Send public message
        if target:
            # Ping target in message content, embed for the flex
            await interaction.response.send_message(
                content=target.mention,
                embed=embed,
            )
        else:
            await interaction.response.send_message(embed=embed)

    def _create_announce_embed(
        self,
        user: discord.User | discord.Member,
        balance: int,
        cost: int,
        target: discord.Member | None,
    ) -> discord.Embed:
        """Create a beautiful wealth announcement embed."""
        # Get Bounty Hunter color (or gold fallback)
        bh_color = get_hero_color(BOUNTY_HUNTER_ID) or BOUNTY_HUNTER_COLOR

        embed = discord.Embed(color=bh_color)

        # Set Bounty Hunter as thumbnail
        bh_image = get_hero_image_url(BOUNTY_HUNTER_ID)
        if bh_image:
            embed.set_thumbnail(url=bh_image)

        # Title with gold emojis
        embed.title = "WEALTH ANNOUNCEMENT"

        # Build description with snarky message
        if target:
            # Calculate ratio for one message variant
            ratio = SHOP_ANNOUNCE_TARGET_COST // SHOP_ANNOUNCE_COST if SHOP_ANNOUNCE_COST > 0 else 10
            message = random.choice(ANNOUNCE_TARGET_MESSAGES).format(
                user=user.mention,
                target=target.mention,
                cost=cost,
                emote=JOPACOIN_EMOTE,
                ratio=ratio,
            )
            description = f"*{message}*"
        else:
            message = random.choice(ANNOUNCE_MESSAGES)
            description = f"*\"{message}\"*"

        # Add visual separator and balance display
        description += "\n\n" + "━" * 25 + "\n\n"
        description += f"{user.mention} has\n\n"
        description += f"**{balance}** {JOPACOIN_EMOTE}\n\n"
        description += "━" * 25

        embed.description = description

        # Footer showing cost
        embed.set_footer(text=f"This flex cost {cost} jopacoin")

        return embed


async def setup(bot: commands.Bot):
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    await bot.add_cog(ShopCommands(bot, player_service))
