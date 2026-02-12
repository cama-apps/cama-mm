"""
Shop commands for spending jopacoin.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from config import (
    DOUBLE_OR_NOTHING_COOLDOWN_SECONDS,
    PACKAGE_DEAL_GAMES_DURATION,
    SHOP_ANNOUNCE_COST,
    SHOP_ANNOUNCE_TARGET_COST,
    SHOP_DOUBLE_OR_NOTHING_COST,
    SHOP_MYSTERY_GIFT_COST,
    SHOP_PACKAGE_DEAL_BASE_COST,
    SHOP_PACKAGE_DEAL_RATING_DIVISOR,
    SHOP_PROTECT_HERO_COST,
    SHOP_SOFT_AVOID_COST,
    SOFT_AVOID_GAMES_DURATION,
)
from services.flavor_text_service import EVENT_EXAMPLES, FlavorEvent
from services.permissions import has_admin_permission
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
        target="User to interact with (required for 'Announce + Tag', 'Soft Avoid', and 'Package Deal' options)",
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
            app_commands.Choice(
                name=f"Mystery Gift ({SHOP_MYSTERY_GIFT_COST} jopacoin)",
                value="mystery_gift",
            ),
            app_commands.Choice(
                name=f"Double or Nothing ({SHOP_DOUBLE_OR_NOTHING_COST} jopacoin)",
                value="double_or_nothing",
            ),
            app_commands.Choice(
                name=f"Soft Avoid ({SHOP_SOFT_AVOID_COST} jopacoin for {SOFT_AVOID_GAMES_DURATION} games)",
                value="soft_avoid",
            ),
            app_commands.Choice(
                name=f"Package Deal ({SHOP_PACKAGE_DEAL_BASE_COST}+ jopacoin for {PACKAGE_DEAL_GAMES_DURATION} games)",
                value="package_deal",
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
        elif item.value == "mystery_gift":
            await self._handle_mystery_gift(interaction)
        elif item.value == "double_or_nothing":
            await self._handle_double_or_nothing(interaction)
        elif item.value == "soft_avoid":
            if not target:
                await interaction.response.send_message(
                    "You selected 'Soft Avoid' but didn't specify a target. "
                    "Please provide a user to avoid being teamed with!",
                    ephemeral=True,
                )
                return
            await self._handle_soft_avoid(interaction, target=target)
        elif item.value == "package_deal":
            if not target:
                await interaction.response.send_message(
                    "You selected 'Package Deal' but didn't specify a target. "
                    "Please provide a user you want to be teamed with!",
                    ephemeral=True,
                )
                return
            await self._handle_package_deal(interaction, target=target)

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
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop. "
                "Hard to flex wealth you don't have.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
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
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)
        new_balance = balance - cost

        # Build stats comparison for targeted flex
        buyer_stats = await self._get_flex_stats(user_id, guild_id)
        target_stats = (await self._get_flex_stats(target.id, guild_id)) if target else None

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

    async def _get_flex_stats(self, discord_id: int, guild_id: int | None = None) -> dict:
        """Get stats for flex comparison."""
        balance = await asyncio.to_thread(self.player_service.get_balance, discord_id, guild_id)
        stats = {
            "balance": balance,
            "wins": 0,
            "losses": 0,
            "win_rate": None,
            "rating": None,
            "total_bets": 0,
            "net_pnl": 0,
            "degen_score": None,
            "bankruptcies": 0,
        }

        player = await asyncio.to_thread(self.player_service.get_player, discord_id, guild_id)
        if player:
            stats["wins"] = player.wins or 0
            stats["losses"] = player.losses or 0
            total = stats["wins"] + stats["losses"]
            if total > 0:
                stats["win_rate"] = stats["wins"] / total * 100
            stats["rating"] = player.glicko_rating

        if self.gambling_stats_service:
            try:
                gamba_stats = await asyncio.to_thread(
                    self.gambling_stats_service.get_player_stats, discord_id, guild_id
                )
                if gamba_stats:
                    stats["total_bets"] = gamba_stats.total_bets
                    stats["net_pnl"] = gamba_stats.net_pnl
                    stats["degen_score"] = gamba_stats.degen_score.total if gamba_stats.degen_score else None
                stats["bankruptcies"] = await asyncio.to_thread(
                    self.gambling_stats_service.get_player_bankruptcy_count,
                    discord_id, guild_id,
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
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
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
        pending_state = await asyncio.to_thread(self.match_service.get_last_shuffle, guild_id)
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
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
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
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

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

    async def _handle_mystery_gift(
        self,
        interaction: discord.Interaction,
    ):
        """Handle the mystery gift purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_MYSTERY_GIFT_COST

        # Check if registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.",
                ephemeral=True,
            )
            return

        # Deduct cost
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        # Build the announcement embed
        embed = discord.Embed(
            title="🎁 Mystery Gift Redeemed!",
            description=f"{interaction.user.mention} has redeemed a **Mystery Gift**!",
            color=0x9B59B6,  # Purple for mystery
        )
        embed.set_footer(text=f"Cost: {cost} jopacoin")

        # Send public announcement
        await interaction.response.send_message(embed=embed)

    async def _handle_double_or_nothing(
        self,
        interaction: discord.Interaction,
    ):
        """Handle the Double or Nothing gamble."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_DOUBLE_OR_NOTHING_COST

        # Check if registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can gamble. "
                "Can't double nothing if you have nothing.",
                ephemeral=True,
            )
            return

        # Check cooldown (admins bypass)
        is_admin = has_admin_permission(interaction)
        last_spin = await asyncio.to_thread(
            self.player_service.get_last_double_or_nothing, user_id, guild_id
        )
        now = int(time.time())
        if last_spin is not None and not is_admin:
            elapsed = now - last_spin
            if elapsed < DOUBLE_OR_NOTHING_COOLDOWN_SECONDS:
                remaining = DOUBLE_OR_NOTHING_COOLDOWN_SECONDS - elapsed
                days = remaining // 86400
                hours = (remaining % 86400) // 3600
                minutes = (remaining % 3600) // 60
                time_str = ""
                if days > 0:
                    time_str += f"{days}d "
                if hours > 0:
                    time_str += f"{hours}h "
                if minutes > 0 or not time_str:
                    time_str += f"{minutes}m"
                await interaction.response.send_message(
                    f"You already tempted fate recently. "
                    f"Wait **{time_str.strip()}** before your next Double or Nothing.",
                    ephemeral=True,
                )
                # Neon Degen Terminal hook (cooldown hit)
                try:
                    from services.neon_degen_service import NeonDegenService
                    _neon_svc = getattr(self.bot, "neon_degen_service", None)
                    neon = _neon_svc if isinstance(_neon_svc, NeonDegenService) else None
                    if neon:
                        neon_result = await neon.on_cooldown_hit(user_id, guild_id, "double_or_nothing")
                        if neon_result:
                            msg = None
                            if neon_result.text_block:
                                msg = await interaction.channel.send(neon_result.text_block)
                            elif neon_result.footer_text:
                                msg = await interaction.channel.send(neon_result.footer_text)
                            if msg:
                                async def _del(m, d):
                                    try:
                                        await asyncio.sleep(d)
                                        await m.delete()
                                    except Exception:
                                        pass
                                asyncio.create_task(_del(msg, 60))
                except Exception:
                    pass
                return

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        if balance < 0:
            await interaction.response.send_message(
                f"You're in debt ({balance} {JOPACOIN_EMOTE}). "
                "You can't double debt. Pay it off first!",
                ephemeral=True,
            )
            return

        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}. "
                "Can't afford the ante.",
                ephemeral=True,
            )
            return

        # Defer - we'll send the result publicly
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Deduct cost first
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)
        balance_after_cost = balance - cost

        # 50/50 flip
        won = random.random() < 0.5

        if balance_after_cost == 0:
            # Special case: paid exactly the cost, balance is 0
            # Both win and lose result in 0
            final_balance = 0
            if won:
                result_title = "DOUBLE... NOTHING!"
                result_color = 0xFFFF00  # Yellow for irony
                flavor_event = FlavorEvent.DOUBLE_OR_NOTHING_ZERO
            else:
                result_title = "NOTHING!"
                result_color = 0xFF0000  # Red
                flavor_event = FlavorEvent.DOUBLE_OR_NOTHING_LOSE
        elif won:
            # WIN: Double the remaining balance
            winnings = balance_after_cost
            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, winnings)
            final_balance = balance_after_cost * 2
            result_title = "DOUBLE!"
            result_color = 0x00FF00  # Green
            flavor_event = FlavorEvent.DOUBLE_OR_NOTHING_WIN
        else:
            # LOSE: Zero out the balance
            await asyncio.to_thread(self.player_service.set_balance, user_id, guild_id, 0)
            final_balance = 0
            result_title = "NOTHING!"
            result_color = 0xFF0000  # Red
            flavor_event = FlavorEvent.DOUBLE_OR_NOTHING_LOSE

        # Generate AI flavor text (falls back to examples if AI disabled)
        result_message = None
        if self.flavor_text_service:
            try:
                event_details = {
                    "starting_balance": balance,
                    "cost": cost,
                    "balance_at_risk": balance_after_cost,
                    "final_balance": final_balance,
                    "won": won,
                    "net_change": final_balance - balance,
                }
                result_message = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=flavor_event,
                    discord_id=user_id,
                    event_details=event_details,
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for double or nothing: {e}")

        # Fallback to random example if AI failed or returned None
        if not result_message:
            examples = EVENT_EXAMPLES.get(flavor_event, [])
            if examples:
                result_message = random.choice(examples)
            else:
                result_message = "The coin has decided your fate."

        # Log the result
        await asyncio.to_thread(
            functools.partial(
                self.player_service.log_double_or_nothing,
                discord_id=user_id,
                guild_id=guild_id,
                cost=cost,
                balance_before=balance_after_cost,
                balance_after=final_balance,
                won=won,
                spin_time=now,
            )
        )

        # Build result embed
        embed = discord.Embed(
            title=f"Double or Nothing: {result_title}",
            color=result_color,
        )

        embed.description = f"*{result_message}*\n\n"
        embed.description += "━" * 25 + "\n\n"

        # Show the math
        embed.description += f"**Starting Balance:** {balance} {JOPACOIN_EMOTE}\n"
        embed.description += f"**Entry Cost:** -{cost} {JOPACOIN_EMOTE}\n"
        embed.description += f"**At Risk:** {balance_after_cost} {JOPACOIN_EMOTE}\n\n"

        if won and balance_after_cost > 0:
            embed.description += f"**Result:** {balance_after_cost} x 2 = **{final_balance}** {JOPACOIN_EMOTE}\n"
            net = final_balance - balance
            embed.description += f"**Net Gain:** +{net} {JOPACOIN_EMOTE}"
        else:
            net = final_balance - balance
            embed.description += f"**Result:** **{final_balance}** {JOPACOIN_EMOTE}\n"
            embed.description += f"**Net Loss:** {net} {JOPACOIN_EMOTE}"

        embed.set_footer(text=f"Entry: {cost} JC | Cooldown: 30 days")

        # Set user avatar as thumbnail
        if interaction.user.avatar:
            embed.set_thumbnail(url=interaction.user.avatar.url)

        await safe_followup(interaction, content=interaction.user.mention, embed=embed)

        # Neon Degen Terminal hook (double or nothing result)
        try:
            from services.neon_degen_service import NeonDegenService
            _neon_svc = getattr(self.bot, "neon_degen_service", None)
            neon = _neon_svc if isinstance(_neon_svc, NeonDegenService) else None
            if neon:
                neon_result = await neon.on_double_or_nothing(
                    user_id, guild_id,
                    won=won,
                    balance_at_risk=balance_after_cost,
                    final_balance=final_balance,
                )
                if neon_result:
                    msg = None
                    if neon_result.gif_file:
                        gif_file = discord.File(neon_result.gif_file, filename="jopat_terminal.gif")
                        if neon_result.text_block:
                            msg = await interaction.channel.send(neon_result.text_block, file=gif_file)
                        else:
                            msg = await interaction.channel.send(file=gif_file)
                    elif neon_result.text_block:
                        msg = await interaction.channel.send(neon_result.text_block)
                    elif neon_result.footer_text:
                        msg = await interaction.channel.send(neon_result.footer_text)
                    if msg:
                        async def _del_neon(m, d):
                            try:
                                await asyncio.sleep(d)
                                await m.delete()
                            except Exception:
                                pass
                        asyncio.create_task(_del_neon(msg, 60))
        except Exception:
            pass

    async def _handle_soft_avoid(
        self,
        interaction: discord.Interaction,
        target: discord.Member,
    ):
        """Handle the soft avoid purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_SOFT_AVOID_COST

        # Can't avoid yourself
        if target.id == user_id:
            await interaction.response.send_message(
                "You can't soft avoid yourself.",
                ephemeral=True,
            )
            return

        # Check if registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop.",
                ephemeral=True,
            )
            return

        # Check if target is registered
        target_player = await asyncio.to_thread(self.player_service.get_player, target.id, guild_id)
        if not target_player:
            await interaction.response.send_message(
                "The target player is not registered.",
                ephemeral=True,
            )
            return

        # Check if soft_avoid_service is available
        soft_avoid_service = getattr(self.bot, "soft_avoid_service", None)
        if not soft_avoid_service:
            await interaction.response.send_message(
                "Soft avoid feature is currently unavailable.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.",
                ephemeral=True,
            )
            return

        # Deduct cost
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        # Create or extend avoid
        avoid = await asyncio.to_thread(
            functools.partial(
                soft_avoid_service.create_or_extend_avoid,
                guild_id=guild_id,
                avoider_id=user_id,
                avoided_id=target.id,
                games=SOFT_AVOID_GAMES_DURATION,
            )
        )

        # Build confirmation embed (ephemeral)
        embed = discord.Embed(
            title="Soft Avoid Active",
            description=(
                f"You are now soft-avoiding **{target.display_name}**.\n\n"
                f"**Games remaining:** {avoid.games_remaining}\n\n"
                f"When shuffling, the system will try to place you on opposite teams. "
                f"The avoid count decreases each game where you're both playing "
                f"and successfully placed on opposite teams."
            ),
            color=0x7289DA,
        )
        embed.set_footer(text=f"Cost: {cost} jopacoin")

        # Ephemeral response (private)
        await interaction.response.send_message(embed=embed, ephemeral=True)

        # Neon Degen Terminal hook (soft avoid purchase)
        try:
            from services.neon_degen_service import NeonDegenService
            _neon_svc = getattr(self.bot, "neon_degen_service", None)
            neon = _neon_svc if isinstance(_neon_svc, NeonDegenService) else None
            if neon:
                neon_result = await neon.on_soft_avoid(
                    user_id, guild_id,
                    cost=cost,
                    games=SOFT_AVOID_GAMES_DURATION,
                )
                if neon_result:
                    msg = None
                    if neon_result.text_block:
                        msg = await interaction.channel.send(neon_result.text_block)
                    elif neon_result.footer_text:
                        msg = await interaction.channel.send(neon_result.footer_text)
                    if msg:
                        async def _del_neon(m, d):
                            try:
                                await asyncio.sleep(d)
                                await m.delete()
                            except Exception:
                                pass
                        asyncio.create_task(_del_neon(msg, 60))
        except Exception:
            pass

    async def _handle_package_deal(
        self,
        interaction: discord.Interaction,
        target: discord.Member,
    ):
        """Handle the package deal purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Can't package deal with yourself
        if target.id == user_id:
            await interaction.response.send_message(
                "You can't package deal with yourself.",
                ephemeral=True,
            )
            return

        # Check if registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can shop.",
                ephemeral=True,
            )
            return

        # Check if target is registered (required for package deal)
        target_player = await asyncio.to_thread(self.player_service.get_player, target.id, guild_id)
        if not target_player:
            await interaction.response.send_message(
                "The target player is not registered.",
                ephemeral=True,
            )
            return

        # Check if package_deal_service is available
        package_deal_service = getattr(self.bot, "package_deal_service", None)
        if not package_deal_service:
            await interaction.response.send_message(
                "Package deal feature is currently unavailable.",
                ephemeral=True,
            )
            return

        # Calculate dynamic cost: base + (sum of ratings / divisor)
        buyer_rating = player.glicko_rating or 1500
        partner_rating = target_player.glicko_rating or 1500
        cost = SHOP_PACKAGE_DEAL_BASE_COST + int(
            (buyer_rating + partner_rating) / SHOP_PACKAGE_DEAL_RATING_DIVISOR
        )

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.\n"
                f"*(Base: {SHOP_PACKAGE_DEAL_BASE_COST} + Rating bonus: {cost - SHOP_PACKAGE_DEAL_BASE_COST})*",
                ephemeral=True,
            )
            return

        # Deduct cost
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        # Create or extend deal
        deal = await asyncio.to_thread(
            functools.partial(
                package_deal_service.create_or_extend_deal,
                guild_id=guild_id,
                buyer_id=user_id,
                partner_id=target.id,
                games=PACKAGE_DEAL_GAMES_DURATION,
                cost=cost,
            )
        )

        # Build confirmation embed (ephemeral)
        embed = discord.Embed(
            title="Package Deal Active",
            description=(
                f"You have a Package Deal with **{target.display_name}**.\n\n"
                f"**Games remaining:** {deal.games_remaining}\n"
                f"**Cost:** {cost} {JOPACOIN_EMOTE}\n\n"
                f"When shuffling, the system will try to place you on the **same team**. "
                f"The deal count decreases each game where you're both playing "
                f"and successfully placed on the same team."
            ),
            color=0x2ECC71,  # Green for partnership
        )
        embed.set_footer(
            text=f"Base cost: {SHOP_PACKAGE_DEAL_BASE_COST} + Rating bonus: {cost - SHOP_PACKAGE_DEAL_BASE_COST}"
        )

        # Ephemeral response (private - target not notified)
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @app_commands.command(name="myavoids", description="View your active soft avoids")
    async def myavoids(self, interaction: discord.Interaction):
        """View your active soft avoids."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if soft_avoid_service is available
        soft_avoid_service = getattr(self.bot, "soft_avoid_service", None)
        if not soft_avoid_service:
            await interaction.response.send_message(
                "Soft avoid feature is currently unavailable.",
                ephemeral=True,
            )
            return

        # Get user's avoids
        avoids = await asyncio.to_thread(soft_avoid_service.get_user_avoids, guild_id, user_id)

        if not avoids:
            await interaction.response.send_message(
                "You have no active soft avoids.",
                ephemeral=True,
            )
            return

        # Build the list
        lines = []
        for avoid in avoids:
            lines.append(f"<@{avoid.avoided_discord_id}> - **{avoid.games_remaining}** games")

        embed = discord.Embed(
            title="Your Active Soft Avoids",
            description="\n".join(lines),
            color=0x7289DA,
        )

        # Ephemeral response (private)
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @app_commands.command(name="mydeals", description="View your active package deals")
    async def mydeals(self, interaction: discord.Interaction):
        """View your active package deals."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if package_deal_service is available
        package_deal_service = getattr(self.bot, "package_deal_service", None)
        if not package_deal_service:
            await interaction.response.send_message(
                "Package deal feature is currently unavailable.",
                ephemeral=True,
            )
            return

        # Get user's deals
        deals = await asyncio.to_thread(package_deal_service.get_user_deals, guild_id, user_id)

        if not deals:
            await interaction.response.send_message(
                "You have no active package deals.",
                ephemeral=True,
            )
            return

        # Build the list
        lines = []
        for deal in deals:
            lines.append(f"<@{deal.partner_discord_id}> - **{deal.games_remaining}** games")

        embed = discord.Embed(
            title="Your Active Package Deals",
            description="\n".join(lines),
            color=0x2ECC71,  # Green for partnership
        )

        # Ephemeral response (private)
        await interaction.response.send_message(embed=embed, ephemeral=True)


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
