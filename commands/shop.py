"""
Shop commands for spending jopacoin.
"""

from __future__ import annotations

import logging
import random
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from config import SHOP_ANNOUNCE_COST, SHOP_ANNOUNCE_TARGET_COST, SHOP_PROTECT_HERO_COST
from services.flavor_text_service import FlavorEvent
from services.player_service import PlayerService
from utils.formatting import JOPACOIN_EMOTE
from utils.hero_lookup import get_all_heroes, get_hero_color, get_hero_image_url, get_hero_name
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.match_service import MatchService
    from services.flavor_text_service import FlavorTextService
    from services.gambling_stats_service import GamblingStatsService

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

    def __init__(
        self,
        bot: commands.Bot,
        player_service: PlayerService,
        match_service: MatchService | None = None,
        flavor_text_service: FlavorTextService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
    ):
        self.bot = bot
        self.player_service = player_service
        self.match_service = match_service
        self.flavor_text_service = flavor_text_service
        self.gambling_stats_service = gambling_stats_service

    async def hero_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for hero names."""
        all_heroes = get_all_heroes()
        matches = [
            app_commands.Choice(name=hero_name, value=hero_id)
            for hero_id, hero_name in all_heroes.items()
            if current.lower() in hero_name.lower()
        ]
        return matches[:25]  # Discord limit

    @app_commands.command(name="shop", description="Spend jopacoin in the shop")
    @app_commands.describe(
        item="What to buy",
        target="User to tag (required for 'Announce + Tag' option)",
        hero="Hero to protect from bans (required for 'Protect Hero' option)",
    )
    @app_commands.choices(
        item=[
            app_commands.Choice(
                name=f"Announce Balance ({SHOP_ANNOUNCE_COST} jopacoin)",
                value="announce",
            ),
            app_commands.Choice(
                name=f"Announce Balance + Tag User ({SHOP_ANNOUNCE_TARGET_COST} jopacoin)",
                value="announce_target",
            ),
            app_commands.Choice(
                name=f"Protect Hero ({SHOP_PROTECT_HERO_COST} jopacoin)",
                value="protect_hero",
            ),
        ]
    )
    @app_commands.autocomplete(hero=hero_autocomplete)
    async def shop(
        self,
        interaction: discord.Interaction,
        item: app_commands.Choice[str],
        target: discord.Member | None = None,
        hero: str | None = None,
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
            # Basic announcement - ignore target if provided
            await self._handle_announce(interaction, target=None)
        elif item.value == "announce_target":
            # Targeted announcement - require target
            if not target:
                await interaction.response.send_message(
                    "You selected 'Announce + Tag User' but didn't specify a target. "
                    "Please provide a user to tag!",
                    ephemeral=True,
                )
                return
            await self._handle_announce(interaction, target=target)
        elif item.value == "protect_hero":
            # Protect hero from bans - require hero selection
            if not hero:
                await interaction.response.send_message(
                    "You selected 'Protect Hero' but didn't specify a hero. "
                    "Please provide a hero to protect!",
                    ephemeral=True,
                )
                return
            await self._handle_protect_hero(interaction, hero=hero)

    async def _handle_announce(
        self,
        interaction: discord.Interaction,
        target: discord.Member | None,
    ):
        """Handle the balance announcement purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

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

        # Defer now - AI flavor text can take a while
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Deduct cost
        self.player_service.player_repo.add_balance(user_id, -cost)
        new_balance = balance - cost

        # Build stats comparison for targeted flex
        buyer_stats = self._get_flex_stats(user_id)
        target_stats = self._get_flex_stats(target.id) if target else None

        # Generate AI flavor text
        ai_flavor = None
        if self.flavor_text_service:
            try:
                event_type = FlavorEvent.SHOP_ANNOUNCE_TARGET if target else FlavorEvent.SHOP_ANNOUNCE
                event_details = {
                    "buyer_name": interaction.user.display_name,
                    "buyer_balance": new_balance,
                    "cost_paid": cost,
                    "buyer_stats": buyer_stats,
                }
                if target and target_stats:
                    event_details["target_name"] = target.display_name
                    event_details["target_stats"] = target_stats
                    # Add comparison highlights
                    event_details["comparison"] = self._build_comparison(buyer_stats, target_stats)

                ai_flavor = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=event_type,
                    discord_id=user_id,
                    event_details=event_details,
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for shop: {e}")

        # Build the beautiful embed
        embed = self._create_announce_embed(
            interaction.user, new_balance, cost, target, ai_flavor, buyer_stats, target_stats
        )

        # Send public message (using followup since we deferred)
        if target:
            # Ping target in message content, embed for the flex
            await safe_followup(
                interaction,
                content=target.mention,
                embed=embed,
            )
        else:
            await safe_followup(interaction, embed=embed)

    def _get_flex_stats(self, discord_id: int) -> dict:
        """Get stats for flex comparison."""
        stats = {
            "balance": self.player_service.get_balance(discord_id),
            "wins": 0,
            "losses": 0,
            "win_rate": None,
            "rating": None,
            "total_bets": 0,
            "net_pnl": 0,
            "degen_score": None,
            "bankruptcies": 0,
        }

        player = self.player_service.get_player(discord_id)
        if player:
            stats["wins"] = player.wins or 0
            stats["losses"] = player.losses or 0
            total = stats["wins"] + stats["losses"]
            if total > 0:
                stats["win_rate"] = stats["wins"] / total * 100
            stats["rating"] = player.glicko_rating

        if self.gambling_stats_service:
            try:
                gamba_stats = self.gambling_stats_service.get_player_stats(discord_id)
                if gamba_stats:
                    stats["total_bets"] = gamba_stats.total_bets
                    stats["net_pnl"] = gamba_stats.net_pnl
                    stats["degen_score"] = gamba_stats.degen_score.total if gamba_stats.degen_score else None
                stats["bankruptcies"] = self.gambling_stats_service.bet_repo.get_player_bankruptcy_count(
                    discord_id
                )
            except Exception:
                pass

        return stats

    def _build_comparison(self, buyer: dict, target: dict) -> dict:
        """Build comparison highlights between buyer and target."""
        comparison = {"buyer_wins": [], "target_wins": []}

        # Balance comparison
        if buyer["balance"] > target["balance"]:
            diff = buyer["balance"] - target["balance"]
            comparison["buyer_wins"].append(f"{diff} more jopacoin")
        elif target["balance"] > buyer["balance"]:
            diff = target["balance"] - buyer["balance"]
            comparison["target_wins"].append(f"{diff} more jopacoin")

        # Win rate comparison
        if buyer["win_rate"] and target["win_rate"]:
            if buyer["win_rate"] > target["win_rate"]:
                comparison["buyer_wins"].append(f"{buyer['win_rate']:.0f}% vs {target['win_rate']:.0f}% win rate")
            elif target["win_rate"] > buyer["win_rate"]:
                comparison["target_wins"].append(f"better win rate")

        # Rating comparison
        if buyer["rating"] and target["rating"]:
            if buyer["rating"] > target["rating"]:
                diff = int(buyer["rating"] - target["rating"])
                comparison["buyer_wins"].append(f"{diff} higher rating")
            elif target["rating"] > buyer["rating"]:
                comparison["target_wins"].append(f"higher rating")

        # P&L comparison
        if buyer["net_pnl"] > target["net_pnl"]:
            comparison["buyer_wins"].append(f"{buyer['net_pnl'] - target['net_pnl']} better P&L")

        # Bankruptcies (fewer is better)
        if buyer["bankruptcies"] < target["bankruptcies"]:
            comparison["buyer_wins"].append(f"fewer bankruptcies ({buyer['bankruptcies']} vs {target['bankruptcies']})")
        elif target["bankruptcies"] < buyer["bankruptcies"]:
            comparison["target_wins"].append(f"fewer bankruptcies")

        return comparison

    def _create_announce_embed(
        self,
        user: discord.User | discord.Member,
        balance: int,
        cost: int,
        target: discord.Member | None,
        ai_flavor: str | None,
        buyer_stats: dict | None,
        target_stats: dict | None,
    ) -> discord.Embed:
        """Create a beautiful wealth announcement embed with cherry-picked stats."""
        # Get Bounty Hunter color (or gold fallback)
        bh_color = get_hero_color(BOUNTY_HUNTER_ID) or BOUNTY_HUNTER_COLOR

        embed = discord.Embed(color=bh_color)

        # Set Bounty Hunter as thumbnail
        bh_image = get_hero_image_url(BOUNTY_HUNTER_ID)
        if bh_image:
            embed.set_thumbnail(url=bh_image)

        # Title with gold emojis
        embed.title = "WEALTH ANNOUNCEMENT"

        # Build description with AI flavor or fallback
        if ai_flavor:
            description = f"*{ai_flavor}*"
        elif target:
            # Fallback to static messages
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

        # Add cherry-picked stats comparison for targeted flex
        if target and buyer_stats and target_stats:
            flex_lines = self._cherry_pick_flex_stats(buyer_stats, target_stats, target.display_name)
            if flex_lines:
                embed.add_field(
                    name="The Numbers Don't Lie",
                    value="\n".join(flex_lines),
                    inline=False,
                )

        # Footer showing cost
        embed.set_footer(text=f"This flex cost {cost} jopacoin")

        return embed

    def _cherry_pick_flex_stats(
        self, buyer: dict, target: dict, target_name: str
    ) -> list[str]:
        """Cherry-pick stats that make the buyer look good."""
        flex_lines = []

        # Only show stats where buyer wins
        if buyer["balance"] > target["balance"]:
            diff = buyer["balance"] - target["balance"]
            flex_lines.append(f"**+{diff}** more jopacoin than {target_name}")

        if buyer["rating"] and target["rating"] and buyer["rating"] > target["rating"]:
            diff = int(buyer["rating"] - target["rating"])
            flex_lines.append(f"**+{diff}** higher rating")

        if buyer["win_rate"] and target["win_rate"] and buyer["win_rate"] > target["win_rate"]:
            flex_lines.append(f"**{buyer['win_rate']:.0f}%** win rate vs {target['win_rate']:.0f}%")

        if buyer["net_pnl"] > target["net_pnl"]:
            diff = buyer["net_pnl"] - target["net_pnl"]
            flex_lines.append(f"**+{diff}** better gambling P&L")

        if buyer["bankruptcies"] < target["bankruptcies"]:
            flex_lines.append(f"Only **{buyer['bankruptcies']}** bankruptcies vs {target['bankruptcies']}")

        if buyer["total_bets"] > target["total_bets"]:
            flex_lines.append(f"**{buyer['total_bets']}** bets placed (more action)")

        # If buyer has no advantages, generate some cope
        if not flex_lines:
            flex_lines.append("*(Stats were cherry-picked but... we couldn't find any advantages)*")
            flex_lines.append(f"*At least you spent {SHOP_ANNOUNCE_TARGET_COST} to flex on them*")

        return flex_lines

    async def _handle_protect_hero(
        self,
        interaction: discord.Interaction,
        hero: str,
    ):
        """Handle the protect hero purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_PROTECT_HERO_COST

        # Check if registered
        player = self.player_service.get_player(user_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop.",
                ephemeral=True,
            )
            return

        # Check if match_service is available
        if not self.match_service:
            await interaction.response.send_message(
                "This feature is currently unavailable.",
                ephemeral=True,
            )
            return

        # Check if there's an active shuffle
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            await interaction.response.send_message(
                "There's no active shuffle. You can only protect a hero during an active game.",
                ephemeral=True,
            )
            return

        # Check if player is in the shuffle
        radiant_ids = pending_state.get("radiant_team_ids", [])
        dire_ids = pending_state.get("dire_team_ids", [])
        all_player_ids = radiant_ids + dire_ids

        if user_id not in all_player_ids:
            await interaction.response.send_message(
                "You're not in the current shuffle. Only players in the game can protect heroes.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = self.player_service.get_balance(user_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.",
                ephemeral=True,
            )
            return

        # Defer - posting to multiple places
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Deduct cost
        self.player_service.player_repo.add_balance(user_id, -cost)

        # Get hero info
        hero_id = int(hero)
        hero_name = get_hero_name(hero_id)
        hero_image = get_hero_image_url(hero_id)
        hero_color = get_hero_color(hero_id) or 0xFFD700  # Gold fallback

        # Determine which team the player is on
        team_name = "Radiant" if user_id in radiant_ids else "Dire"

        # Build mentions for all other players in the shuffle
        other_player_ids = [pid for pid in all_player_ids if pid != user_id]
        mentions = " ".join(f"<@{pid}>" for pid in other_player_ids)

        # Build the embed
        embed = discord.Embed(
            title=f"Hero Protected: {hero_name}",
            description=(
                f"{interaction.user.mention} ({team_name}) has protected "
                f"**{hero_name}** from being banned!\n\n"
                f"*This hero cannot be banned during the draft (honor system).*"
            ),
            color=hero_color,
        )
        if hero_image:
            embed.set_thumbnail(url=hero_image)
        embed.set_footer(text=f"Cost: {cost} jopacoin")

        # Build the message content with mentions
        content = f"{mentions}"

        # Post to the shuffle thread if it exists
        thread_id = pending_state.get("thread_shuffle_thread_id")
        if thread_id:
            try:
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)
                if thread:
                    await thread.send(content=content, embed=embed)
            except Exception as e:
                logger.warning(f"Failed to post protect hero to thread {thread_id}: {e}")

        # Post to the shuffle channel if it's different from both the thread
        # and the channel where the command was invoked (to avoid double-posting)
        channel_id = pending_state.get("shuffle_channel_id")
        interaction_channel_id = interaction.channel.id if interaction.channel else None
        if channel_id and channel_id != thread_id and channel_id != interaction_channel_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                if channel:
                    await channel.send(content=content, embed=embed)
            except Exception as e:
                logger.warning(f"Failed to post protect hero to channel {channel_id}: {e}")

        # Confirm to the user (this posts to where the command was invoked)
        await safe_followup(interaction, content=content, embed=embed)


async def setup(bot: commands.Bot):
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    match_service = getattr(bot, "match_service", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)

    await bot.add_cog(ShopCommands(
        bot, player_service, match_service, flavor_text_service, gambling_stats_service
    ))
