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

from utils.neon_helpers import get_neon_service
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
    SHOP_RECALIBRATE_COST,
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
    from services.recalibration_service import RecalibrationService
    from services.dig_service import DigService

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
        recalibration_service: RecalibrationService | None = None,
        dig_service: DigService | None = None,
    ):
        self.bot = bot
        self.player_service = player_service
        self.match_service = match_service
        self.flavor_text_service = flavor_text_service
        self.gambling_stats_service = gambling_stats_service
        self.recalibration_service = recalibration_service
        self.dig_service = dig_service

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

    async def item_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for shop items (dynamic per-user for recalibrate cooldown)."""
        static_items = [
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

        # Build recalibrate choice — dynamic based on cooldown
        recal_choice = app_commands.Choice(
            name=f"Recalibrate ({SHOP_RECALIBRATE_COST} jopacoin)",
            value="recalibrate",
        )
        if self.recalibration_service:
            try:
                guild_id = interaction.guild.id if interaction.guild else None
                check = await asyncio.to_thread(
                    self.recalibration_service.can_recalibrate,
                    interaction.user.id,
                    guild_id,
                )
                if not check["allowed"] and check.get("reason") == "on_cooldown":
                    recal_choice = app_commands.Choice(
                        name="Recalibrate (ON COOLDOWN)",
                        value="recalibrate_cooldown",
                    )
            except Exception:
                pass  # Fall back to default label

        # Dig items
        dig_items = []
        if self.dig_service:
            dig_items = [
                app_commands.Choice(name="Dynamite (5 jopacoin) — +5 blocks", value="dig_dynamite"),
                app_commands.Choice(name="Hard Hat (8 jopacoin) — cave-in shield", value="dig_hard_hat"),
                app_commands.Choice(name="Lantern (4 jopacoin) — scan + protection", value="dig_lantern"),
                app_commands.Choice(name="Reinforcement (6 jopacoin) — decay + sabotage shield", value="dig_reinforcement"),
                app_commands.Choice(name="Pickaxe Upgrade — upgrade your pickaxe", value="dig_upgrade"),
            ]

        all_items = static_items + [recal_choice] + dig_items

        if current:
            all_items = [c for c in all_items if current.lower() in c.name.lower()]
        return all_items[:25]

    @app_commands.command(name="shop", description="Spend jopacoin in the shop")
    @app_commands.describe(
        item="What to buy",
        target="User to interact with (required for 'Announce + Tag', 'Soft Avoid', and 'Package Deal' options)",
        hero="Hero to protect from bans (required for 'Protect Hero' option)",
    )
    @app_commands.autocomplete(item=item_autocomplete, hero=hero_autocomplete)
    async def shop(
        self,
        interaction: discord.Interaction,
        item: str,
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

        if item == "announce":
            # Basic announcement - ignore target if provided
            await self._handle_announce(interaction, target=None)
        elif item == "announce_target":
            # Targeted announcement - require target
            if not target:
                await interaction.response.send_message(
                    "You selected 'Announce + Tag User' but didn't specify a target. "
                    "Please provide a user to tag!",
                    ephemeral=True,
                )
                return
            await self._handle_announce(interaction, target=target)
        elif item == "protect_hero":
            # Protect hero from bans - require hero selection
            if not hero:
                await interaction.response.send_message(
                    "You selected 'Protect Hero' but didn't specify a hero. "
                    "Please provide a hero to protect!",
                    ephemeral=True,
                )
                return
            await self._handle_protect_hero(interaction, hero=hero)
        elif item == "mystery_gift":
            await self._handle_mystery_gift(interaction)
        elif item == "double_or_nothing":
            await self._handle_double_or_nothing(interaction)
        elif item == "soft_avoid":
            if not target:
                await interaction.response.send_message(
                    "You selected 'Soft Avoid' but didn't specify a target. "
                    "Please provide a user to avoid being teamed with!",
                    ephemeral=True,
                )
                return
            await self._handle_soft_avoid(interaction, target=target)
        elif item == "package_deal":
            if not target:
                await interaction.response.send_message(
                    "You selected 'Package Deal' but didn't specify a target. "
                    "Please provide a user you want to be teamed with!",
                    ephemeral=True,
                )
                return
            await self._handle_package_deal(interaction, target=target)
        elif item == "recalibrate":
            await self._handle_recalibrate(interaction)
        elif item.startswith("dig_"):
            await self._handle_dig_item(interaction, item)
        elif item == "recalibrate_cooldown":
            # User selected the ON COOLDOWN item — block with cooldown info
            guild_id = interaction.guild.id if interaction.guild else None
            if self.recalibration_service:
                check = await asyncio.to_thread(
                    self.recalibration_service.can_recalibrate,
                    interaction.user.id,
                    guild_id,
                )
                ends_at = check.get("cooldown_ends_at")
                if ends_at:
                    await interaction.response.send_message(
                        f"Recalibration is on cooldown. You can recalibrate again <t:{ends_at}:R>.",
                        ephemeral=True,
                    )
                    return
            await interaction.response.send_message(
                "Recalibration is on cooldown.", ephemeral=True
            )

    async def _handle_recalibrate(self, interaction: discord.Interaction):
        """Handle the recalibration purchase."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        if not self.recalibration_service:
            await interaction.response.send_message(
                "Recalibration is not available.", ephemeral=True
            )
            return

        # Server-side validation (handles not_registered, no_rating, insufficient_games, on_cooldown)
        check = await asyncio.to_thread(
            self.recalibration_service.can_recalibrate, user_id, guild_id
        )
        if not check["allowed"]:
            reason = check["reason"]
            if reason == "not_registered":
                msg = "You need to `/register` before you can recalibrate."
            elif reason == "no_rating":
                msg = "You don't have a rating yet. Play some games first!"
            elif reason == "insufficient_games":
                msg = (
                    f"You need at least {check['min_games']} games to recalibrate "
                    f"(you have {check['games_played']})."
                )
            elif reason == "on_cooldown":
                ends_at = check.get("cooldown_ends_at")
                msg = f"Recalibration is on cooldown. You can recalibrate again <t:{ends_at}:R>."
            else:
                msg = "You cannot recalibrate right now."
            await interaction.response.send_message(msg, ephemeral=True)
            return

        # Check balance
        balance = await asyncio.to_thread(
            self.player_service.get_balance, user_id, guild_id
        )
        if balance < SHOP_RECALIBRATE_COST:
            await interaction.response.send_message(
                f"You need **{SHOP_RECALIBRATE_COST}** {JOPACOIN_EMOTE} to recalibrate "
                f"but only have **{balance}** {JOPACOIN_EMOTE}.",
                ephemeral=True,
            )
            return

        # Defer (public) — recalibration is a notable event
        await safe_defer(interaction)

        # Deduct cost
        await asyncio.to_thread(
            self.player_service.adjust_balance, user_id, guild_id, -SHOP_RECALIBRATE_COST
        )

        # Execute recalibration
        result = await asyncio.to_thread(
            self.recalibration_service.recalibrate, user_id, guild_id
        )

        if not result["success"]:
            # Refund on unexpected failure
            await asyncio.to_thread(
                self.player_service.adjust_balance, user_id, guild_id, SHOP_RECALIBRATE_COST
            )
            await safe_followup(
                interaction, content="Recalibration failed unexpectedly. You have been refunded."
            )
            return

        # Build public embed
        embed = discord.Embed(
            title="Rating Recalibration",
            description=(
                f"{interaction.user.mention} has recalibrated their rating!\n\n"
                f"Their rating deviation has been reset — expect bigger rating swings "
                f"for the next ~20 games."
            ),
            color=0xE74C3C,  # Red for dramatic effect
        )
        embed.add_field(name="Rating", value=f"{result['old_rating']:.0f} (unchanged)", inline=True)
        embed.add_field(
            name="RD",
            value=f"{result['old_rd']:.0f} → {result['new_rd']:.0f}",
            inline=True,
        )
        embed.add_field(
            name="Next Recalibration",
            value=f"<t:{result['cooldown_ends_at']}:R>",
            inline=True,
        )
        embed.add_field(
            name="Cost",
            value=f"{SHOP_RECALIBRATE_COST} {JOPACOIN_EMOTE}",
            inline=True,
        )
        embed.set_thumbnail(url=get_hero_image_url(str(BOUNTY_HUNTER_ID)))

        await safe_followup(interaction, embed=embed)

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
            except Exception as e:
                logger.warning("Failed to fetch gambling stats for shop profile: %s", e)

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
            title=f"First Pick Reserved: {hero_name}",
            description=(
                f"{interaction.user.mention} has protected **{hero_name}** for **{team_name}**!\n\n"
                f"**{team_name}** should first-pick this hero as early as possible "
                f"to avoid draft complications."
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
                    neon = get_neon_service(self.bot)
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
                                    except Exception as e:
                                        logger.debug("Failed to delete neon message: %s", e)
                                asyncio.create_task(_del(msg, 60))
                except Exception as e:
                    logger.debug("Failed to send neon cooldown result: %s", e)
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
            neon = get_neon_service(self.bot)
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
                            except Exception as e:
                                logger.debug("Failed to delete neon message: %s", e)
                        asyncio.create_task(_del_neon(msg, 60))
        except Exception as e:
            logger.debug("Failed to send neon shop purchase result: %s", e)

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
            neon = get_neon_service(self.bot)
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
                            except Exception as e:
                                logger.debug("Failed to delete neon message: %s", e)
                        asyncio.create_task(_del_neon(msg, 60))
        except Exception as e:
            logger.debug("Failed to send neon soft avoid result: %s", e)

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

    @app_commands.command(name="manashop", description="Buy exclusive items from your mana color's shop")
    @app_commands.describe(
        item="The mana item to purchase",
        target="Target player (for Guardian Angel only)",
    )
    @app_commands.choices(item=[
        app_commands.Choice(name="Pyroclasm (Red, 100 JC) - Destroy 10-30 JC from 3 random players", value="pyroclasm"),
        app_commands.Choice(name="Mana Shield (Blue, 200 JC) - Next loss today halved", value="mana_shield"),
        app_commands.Choice(name="Regrowth (Green, 100 JC) - Recover 25% of today's losses", value="regrowth"),
        app_commands.Choice(name="Guardian Angel (Plains, 300 JC) - Protect a player from BANKRUPT", value="guardian_angel"),
        app_commands.Choice(name="Soul Harvest (Swamp, 75 JC) - Drain 1 JC from all positive players", value="soul_harvest"),
    ])
    @app_commands.checks.cooldown(1, 10)
    async def manashop(
        self,
        interaction: discord.Interaction,
        item: app_commands.Choice[str],
        target: discord.Member = None,
    ):
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        user_id = interaction.user.id

        # Check registration
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.followup.send("You need to `/register` first.", ephemeral=True)
            return

        # Get mana effects
        mana_effects_service = getattr(self.bot, "mana_effects_service", None)
        if not mana_effects_service:
            await interaction.followup.send("Mana system not available.", ephemeral=True)
            return
        effects = await asyncio.to_thread(mana_effects_service.get_effects, user_id, guild_id)
        if not effects.color:
            await interaction.followup.send("You have no active mana today. Use `/mana` first!", ephemeral=True)
            return

        # Item definitions: {item_key: (required_color, cost, display_name)}
        MANA_ITEMS = {
            "pyroclasm": ("Red", 100, "Pyroclasm"),
            "mana_shield": ("Blue", 200, "Mana Shield"),
            "regrowth": ("Green", 100, "Regrowth"),
            "guardian_angel": ("White", 300, "Guardian Angel"),
            "soul_harvest": ("Black", 75, "Soul Harvest"),
        }

        item_key = item.value
        if item_key not in MANA_ITEMS:
            await interaction.followup.send("Unknown item.", ephemeral=True)
            return

        required_color, cost, display_name = MANA_ITEMS[item_key]
        if effects.color != required_color:
            color_to_land = {"Red": "Mountain", "Blue": "Island", "Green": "Forest", "White": "Plains", "Black": "Swamp"}
            await interaction.followup.send(
                f"**{display_name}** requires **{required_color}** mana ({color_to_land.get(required_color, '?')}). "
                f"Your current mana is **{effects.color}**.",
                ephemeral=True,
            )
            return

        # Check balance
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.followup.send(
                f"You need {cost} {JOPACOIN_EMOTE} for {display_name}. You have {balance}.",
                ephemeral=True,
            )
            return

        # Deduct cost
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        # Execute item effect
        if item_key == "pyroclasm":
            # Destroy 10-30 JC from 3 random players (JC destroyed, not transferred)
            import random as _rand
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            eligible = [p for p in all_players if p.discord_id != user_id and p.jopacoin_balance > 0]
            targets = _rand.sample(eligible, min(3, len(eligible)))
            total_destroyed = 0
            victim_lines = []
            for t in targets:
                destroy_amt = _rand.randint(10, 30)
                destroy_amt = min(destroy_amt, t.jopacoin_balance)
                if destroy_amt > 0:
                    await asyncio.to_thread(self.player_service.adjust_balance, t.discord_id, guild_id, -destroy_amt)
                    total_destroyed += destroy_amt
                    victim_lines.append(f"  - {t.name}: -{destroy_amt} {JOPACOIN_EMOTE}")
            victims_text = "\n".join(victim_lines) if victim_lines else "  No eligible targets."
            await interaction.followup.send(
                f"⛰️🔥 **PYROCLASM** — {interaction.user.mention} unleashes chaos!\n"
                f"{victims_text}\n"
                f"**{total_destroyed} {JOPACOIN_EMOTE} destroyed** (cost: {cost} {JOPACOIN_EMOTE})"
            )

        elif item_key == "mana_shield":
            # Store shield state (expires at 4 AM PST)
            import time as _time
            from services.mana_service import get_today_pst
            # Shield persists until next reset (4 AM PST)
            # For simplicity, store in mana_shop_items table
            db = getattr(self.bot, "db", None)
            if db:
                now_ts = int(_time.time())
                await asyncio.to_thread(
                    lambda: db.execute_write(
                        "INSERT INTO mana_shop_items (discord_id, guild_id, item_type, purchased_at, data) VALUES (?, ?, ?, ?, ?)",
                        (user_id, interaction.guild.id if interaction.guild else 0, "mana_shield", now_ts, get_today_pst()),
                    )
                )
            new_balance = balance - cost
            await interaction.followup.send(
                f"🏝️🛡️ **MANA SHIELD** — {interaction.user.mention}'s next JC loss today is reduced by 50%!\n"
                f"Expires at 4 AM PST. (cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "regrowth":
            # Recover 25% of JC lost today (capped at 400 JC)
            from services.mana_service import get_today_pst
            db = getattr(self.bot, "db", None)
            recovery = 0
            if db:
                today = get_today_pst()
                row = await asyncio.to_thread(
                    lambda: db.execute_read(
                        "SELECT total_lost FROM mana_daily_losses WHERE discord_id=? AND guild_id=? AND loss_date=?",
                        (user_id, interaction.guild.id if interaction.guild else 0, today),
                    )
                )
                total_lost = row[0]["total_lost"] if row else 0
                recovery = min(400, int(total_lost * 0.25))
            if recovery > 0:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, recovery)
            new_balance = balance - cost + recovery
            await interaction.followup.send(
                f"🌲💚 **REGROWTH** — {interaction.user.mention} recovers {recovery} {JOPACOIN_EMOTE}!\n"
                f"(25% of today's losses, capped at 400. Cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "guardian_angel":
            if not target:
                # Refund
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, cost)
                await interaction.followup.send(
                    "Guardian Angel requires a `target` player. Usage: `/manashop item:Guardian Angel target:@player`",
                    ephemeral=True,
                )
                return
            import time as _time
            db = getattr(self.bot, "db", None)
            if db:
                now_ts = int(_time.time())
                expires = now_ts + 7 * 24 * 3600  # 7 days
                await asyncio.to_thread(
                    lambda: db.execute_write(
                        "INSERT INTO mana_shop_items (discord_id, guild_id, item_type, target_id, purchased_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
                        (user_id, interaction.guild.id if interaction.guild else 0, "guardian_angel", target.id, now_ts, expires),
                    )
                )
            new_balance = balance - cost
            await interaction.followup.send(
                f"🌾👼 **GUARDIAN ANGEL** — {interaction.user.mention} protects {target.mention}!\n"
                f"Their next BANKRUPT spin will convert to LOSE. Expires in 7 days.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "soul_harvest":
            # Drain 1 JC from every positive-balance player
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            eligible = [p for p in all_players if p.discord_id != user_id and p.jopacoin_balance > 0]
            total_drained = 0
            for p in eligible:
                await asyncio.to_thread(self.player_service.adjust_balance, p.discord_id, guild_id, -1)
                total_drained += 1
            if total_drained > 0:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, total_drained)
            new_balance = balance - cost + total_drained
            await interaction.followup.send(
                f"🌿💀 **SOUL HARVEST** — {interaction.user.mention} drains the living!\n"
                f"Drained **1 {JOPACOIN_EMOTE}** from **{total_drained}** players. "
                f"Gained **{total_drained} {JOPACOIN_EMOTE}**.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )


    async def _handle_dig_item(self, interaction: discord.Interaction, item: str):
        """Handle dig consumable and pickaxe upgrade purchases from /shop."""
        if not self.dig_service:
            await interaction.response.send_message(
                "Mining system is not available.", ephemeral=True
            )
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check registration
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before you can buy dig items.", ephemeral=True
            )
            return

        if item == "dig_upgrade":
            await safe_defer(interaction)
            result = await asyncio.to_thread(
                self.dig_service.upgrade_pickaxe, user_id, guild_id
            )
            if not result.get("success"):
                await safe_followup(
                    interaction,
                    content=result.get("error", "Upgrade failed."),
                    ephemeral=True,
                )
                return
            embed = discord.Embed(
                title="Pickaxe Upgraded!",
                description=(
                    f"You upgraded to **{result.get('new_tier', 'next tier')}**!\n"
                    f"Cost: **{result.get('cost', 0)}** {JOPACOIN_EMOTE}"
                ),
                color=0xB0BEC5,
            )
            await safe_followup(interaction, embed=embed)
            return

        # Consumable purchase: dig_dynamite -> dynamite, etc.
        item_type = item.removeprefix("dig_")
        await safe_defer(interaction)
        result = await asyncio.to_thread(
            self.dig_service.buy_item, user_id, guild_id, item_type
        )
        if not result.get("success"):
            await safe_followup(
                interaction,
                content=result.get("error", "Purchase failed."),
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title="Item Purchased!",
            description=(
                f"Bought **{result.get('item', item_type)}** for "
                f"**{result.get('cost', 0)}** {JOPACOIN_EMOTE}\n"
                f"Balance: **{result.get('balance_after', '?')}** {JOPACOIN_EMOTE}\n\n"
                f"Use `/dig_use {item_type}` to queue it for your next dig."
            ),
            color=0xD4AF37,
        )
        await safe_followup(interaction, embed=embed)


async def setup(bot: commands.Bot):
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    match_service = getattr(bot, "match_service", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)
    recalibration_service = getattr(bot, "recalibration_service", None)
    dig_service = getattr(bot, "dig_service", None)

    await bot.add_cog(ShopCommands(
        bot, player_service, match_service, flavor_text_service, gambling_stats_service,
        recalibration_service, dig_service,
    ))
