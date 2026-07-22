"""
Shop commands for spending jopacoin.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import random
import time
import uuid
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from config import (
    DOUBLE_OR_NOTHING_COOLDOWN_SECONDS,
    HOSTILE_LOSS_MIN_BALANCE,
    PACKAGE_DEAL_GAMES_DURATION,
    PINGEDASH_COOLDOWN_SECONDS,
    PINGEDASH_COST,
    PINGEDASH_TARGET_USER_ID,
    SHOP_ANNOUNCE_COST,
    SHOP_ANNOUNCE_TARGET_COST,
    SHOP_DOUBLE_OR_NOTHING_COST,
    SHOP_JOPA_COIN_COST,
    SHOP_NEW_MYSTERY_GIFT_COST,
    SHOP_PACKAGE_DEAL_BASE_COST,
    SHOP_PACKAGE_DEAL_RATING_DIVISOR,
    SHOP_RECALIBRATE_COST,
    SHOP_SOFT_AVOID_COST,
    SHOP_WITCHS_CURSE_COST,
    WITCHS_CURSE_DURATION_DAYS,
)
from domain.soft_avoid_constants import SOFT_AVOID_GAMES
from services.flavor_text_service import EVENT_EXAMPLES, FlavorEvent
from services.permissions import has_admin_permission
from services.player_service import PlayerService
from services.protection_service import ProtectionService
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)
from utils.formatting import JOPACOIN_EMOTE
from utils.hero_lookup import get_hero_color, get_hero_image_url
from utils.interaction_safety import safe_defer, safe_followup
from utils.neon_helpers import get_neon_service
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.curse_service import CurseService
    from services.dig_service import DigService
    from services.flavor_text_service import FlavorTextService
    from services.gambling_stats_service import GamblingStatsService
    from services.match_service import MatchService
    from services.recalibration_service import RecalibrationService

logger = logging.getLogger("cama_bot.commands.shop")

SOUL_HARVEST_COST = 25
SOUL_HARVEST_DRAIN_PER_TARGET = 2
SOUL_HARVEST_BONUS_DRAIN_CHANCE = 0.20
PINGEDASH_TENOR_URL = "https://tenor.com/view/hiash-gif-25282310"
SOFT_AVOID_MIN_TEAMMATE_GAMES = 3
SOFT_AVOID_MIN_COST = 250
SOFT_AVOID_WINRATE_COST_SCALE = 1500
PACKAGE_DEAL_NO_ACTIVE_COST = 1


def _protection_result_int(result, field: str, default: int = 0) -> int:
    """Read a numeric field from a protection result object or mapping."""
    if result is None:
        return default
    aliases = {
        "attempted_loss": "attempted",
        "absorbed_amount": "absorbed",
        "applied_loss": "applied",
        "victim_new_balance": "victim_balance_after",
        "recipient_new_balance": "destination_balance_after",
    }
    if isinstance(result, dict):
        value = result.get(field, result.get(aliases.get(field, ""), default))
    else:
        value = getattr(result, field, getattr(result, aliases.get(field, ""), default))
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _calculate_soft_avoid_cost(pairing: dict | None) -> int:
    default_cost = max(SOFT_AVOID_MIN_COST, SHOP_SOFT_AVOID_COST)
    if not pairing:
        return default_cost

    games_together = pairing.get("games_together", 0)
    if games_together < SOFT_AVOID_MIN_TEAMMATE_GAMES:
        return default_cost

    wins_together = pairing.get("wins_together", 0)
    winrate_cost = (SOFT_AVOID_WINRATE_COST_SCALE * wins_together + games_together - 1) // games_together
    return max(SOFT_AVOID_MIN_COST, winrate_cost)


def _calculate_package_deal_cost(
    active_deal_count: int,
    is_extend: bool,
    buyer_rating: float | None,
    partner_rating: float | None,
) -> int:
    if active_deal_count == 0 and not is_extend:
        return PACKAGE_DEAL_NO_ACTIVE_COST

    buyer_rating = buyer_rating or 1500
    partner_rating = partner_rating or 1500
    return SHOP_PACKAGE_DEAL_BASE_COST + int(
        (buyer_rating + partner_rating) / SHOP_PACKAGE_DEAL_RATING_DIVISOR
    )


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

    shop_group = app_commands.Group(
        name="shop",
        description="Jopacoin and mana shop commands",
    )

    def __init__(
        self,
        bot: commands.Bot,
        player_service: PlayerService,
        match_service: MatchService | None = None,
        flavor_text_service: FlavorTextService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        recalibration_service: RecalibrationService | None = None,
        dig_service: DigService | None = None,
        curse_service: CurseService | None = None,
    ):
        self.bot = bot
        self.player_service = player_service
        self.match_service = match_service
        self.flavor_text_service = flavor_text_service
        self.gambling_stats_service = gambling_stats_service
        self.recalibration_service = recalibration_service
        self.dig_service = dig_service
        self.curse_service = curse_service

    async def _adjust_generated_mana_reward(
        self, guild_id: int | None, amount: int
    ) -> int:
        """Scale minted mana JC, then apply today's server-wide reward event.

        This is deliberately limited to newly generated rewards. Purchase
        refunds, Reserve/player transfers, hostile harvests, and debt-backed
        credits retain their principal semantics and do not pass through here.
        """
        adjusted = scale_minigame_jc_delta(amount)
        if adjusted <= 0:
            return adjusted

        bot_attrs = getattr(self.bot, "__dict__", {})
        economy_event_service = (
            bot_attrs.get("economy_event_service")
            if isinstance(bot_attrs, dict)
            else getattr(self.bot, "economy_event_service", None)
        )
        if economy_event_service is None:
            return adjusted
        try:
            return int(
                await asyncio.to_thread(
                    economy_event_service.adjust_reward, guild_id, adjusted
                )
            )
        except Exception:
            logger.exception("Daily economy event failed to adjust a mana reward")
            return adjusted

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
                name=f"Jopa Coin(TM) ({SHOP_JOPA_COIN_COST} jopacoin)",
                value="jopa_coin",
            ),
            app_commands.Choice(
                name=f"Mystery Gift ({SHOP_NEW_MYSTERY_GIFT_COST} jopacoin)",
                value="mystery_gift",
            ),
            app_commands.Choice(
                name=f"Witch's Curse ({SHOP_WITCHS_CURSE_COST} jopacoin)",
                value="witchs_curse",
            ),
            app_commands.Choice(
                name=f"Double or Nothing ({SHOP_DOUBLE_OR_NOTHING_COST} jopacoin)",
                value="double_or_nothing",
            ),
            app_commands.Choice(
                name=(
                    f"Soft Avoid (dynamic, minimum {SOFT_AVOID_MIN_COST}, "
                    f"default {max(SOFT_AVOID_MIN_COST, SHOP_SOFT_AVOID_COST)} jopacoin "
                    f"for {SOFT_AVOID_GAMES} games)"
                ),
                value="soft_avoid",
            ),
            app_commands.Choice(
                name=(
                    f"Package Deal ({PACKAGE_DEAL_NO_ACTIVE_COST} JC at 0 active, "
                    f"{SHOP_PACKAGE_DEAL_BASE_COST}+ after for {PACKAGE_DEAL_GAMES_DURATION} games)"
                ),
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

        all_items = static_items + [recal_choice]

        if current:
            all_items = [c for c in all_items if current.lower() in c.name.lower()]
        return all_items[:25]

    @shop_group.command(name="buy", description="Spend jopacoin in the shop")
    @app_commands.describe(
        item="What to buy",
        target="User to interact with (required for 'Announce + Tag', 'Soft Avoid', and 'Package Deal' options)",
    )
    @app_commands.autocomplete(item=item_autocomplete)
    @require_guild
    async def shop(
        self,
        interaction: discord.Interaction,
        item: str,
        target: discord.Member | None = None,
    ):
        """Buy items from the shop with jopacoin."""
        guild_id = interaction.guild.id
        rl = GLOBAL_RATE_LIMITER.check(
            scope="shop",
            guild_id=guild_id,
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
        elif item == "jopa_coin":
            await self._handle_jopa_coin(interaction)
        elif item == "mystery_gift":
            await self._handle_mystery_gift(interaction)
        elif item == "witchs_curse":
            if not target:
                await interaction.response.send_message(
                    "You selected 'Witch's Curse' but didn't specify a target. "
                    "The hex needs a victim.",
                    ephemeral=True,
                )
                return
            await self._handle_witchs_curse(interaction, target=target)
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
        elif item == "recalibrate_cooldown":
            # User selected the ON COOLDOWN item — block with cooldown info
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
        else:
            await interaction.response.send_message(
                "That shop item is no longer available.",
                ephemeral=True,
            )

    @shop_group.command(
        name="pingedash",
        description=f"Spend {PINGEDASH_COST} jopacoin to send the Pingedash",
    )
    @require_guild
    async def pingedash(self, interaction: discord.Interaction):
        """Buy and send the Pingedash."""
        await self._handle_pingedash(interaction)

    async def _handle_pingedash(self, interaction: discord.Interaction) -> None:
        """Charge for Pingedash, claim its cooldown, and send the Tenor embed."""
        target_user_id = PINGEDASH_TARGET_USER_ID
        if target_user_id is None or target_user_id <= 0:
            await interaction.response.send_message(
                "Pingedash is unavailable because its target user is not configured.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id
        now = int(time.time())
        result = await asyncio.to_thread(
            self.player_service.try_purchase_pingedash,
            user_id,
            guild_id,
            cost=PINGEDASH_COST,
            now=now,
            cooldown_seconds=PINGEDASH_COOLDOWN_SECONDS,
        )

        if not result["success"]:
            reason = result["reason"]
            if reason == "not_registered":
                message = "You need to `/player register` before using `/shop pingedash`."
            elif reason == "on_cooldown":
                message = (
                    "Pingedash is on cooldown. You can use it again "
                    f"<t:{result['cooldown_ends_at']}:R>."
                )
            elif reason == "insufficient_balance":
                message = (
                    f"You need {PINGEDASH_COST} {JOPACOIN_EMOTE} for Pingedash, "
                    f"but you only have {result['balance']}."
                )
            else:
                message = "Pingedash is unavailable right now."
            await safe_followup(interaction, content=message, ephemeral=True)
            return

        allowed_mentions = discord.AllowedMentions(
            everyone=False,
            users=[discord.Object(id=target_user_id)],
            roles=False,
            replied_user=False,
        )
        await safe_followup(
            interaction,
            content=f"<@{target_user_id}>\n{PINGEDASH_TENOR_URL}",
            allowed_mentions=allowed_mentions,
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
                msg = "You need to `/player register` before you can recalibrate."
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

        # Apply mana discount on info-style items.
        recal_cost = SHOP_RECALIBRATE_COST
        _mana_fx_recal = getattr(self.bot, "mana_effects_service", None)
        if _mana_fx_recal is not None:
            try:
                _discounted = await asyncio.to_thread(
                    _mana_fx_recal.apply_shop_discount,
                    user_id, guild_id, SHOP_RECALIBRATE_COST,
                    kind="info",
                )
                if isinstance(_discounted, int):
                    recal_cost = _discounted
            except Exception:
                logger.debug("Mana discount lookup failed for recalibrate", exc_info=True)

        # Check balance
        balance = await asyncio.to_thread(
            self.player_service.get_balance, user_id, guild_id
        )
        if balance < recal_cost:
            await interaction.response.send_message(
                f"You need **{recal_cost}** {JOPACOIN_EMOTE} to recalibrate "
                f"but only have **{balance}** {JOPACOIN_EMOTE}.",
                ephemeral=True,
            )
            return

        # Defer (public) — recalibration is a notable event
        await safe_defer(interaction)

        # Deduct cost
        await asyncio.to_thread(
            self.player_service.adjust_balance, user_id, guild_id, -recal_cost
        )

        # Execute recalibration
        result = await asyncio.to_thread(
            self.recalibration_service.recalibrate, user_id, guild_id
        )

        if not result["success"]:
            # Refund on unexpected failure
            await asyncio.to_thread(
                self.player_service.adjust_balance, user_id, guild_id, recal_cost
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
                "You need to `/player register` before you can shop. "
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
                comparison["target_wins"].append("better win rate")

        # Rating comparison
        if buyer["rating"] and target["rating"]:
            if buyer["rating"] > target["rating"]:
                diff = int(buyer["rating"] - target["rating"])
                comparison["buyer_wins"].append(f"{diff} higher rating")
            elif target["rating"] > buyer["rating"]:
                comparison["target_wins"].append("higher rating")

        # P&L comparison
        if buyer["net_pnl"] > target["net_pnl"]:
            comparison["buyer_wins"].append(f"{buyer['net_pnl'] - target['net_pnl']} better P&L")

        # Bankruptcies (fewer is better)
        if buyer["bankruptcies"] < target["bankruptcies"]:
            comparison["buyer_wins"].append(f"fewer bankruptcies ({buyer['bankruptcies']} vs {target['bankruptcies']})")
        elif target["bankruptcies"] < buyer["bankruptcies"]:
            comparison["target_wins"].append("fewer bankruptcies")

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

    async def _handle_jopa_coin(
        self,
        interaction: discord.Interaction,
    ):
        """Pure-flex Jopa Coin(TM) announcement. The mechanic is just the flex."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_JOPA_COIN_COST

        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can shop.",
                ephemeral=True,
            )
            return

        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        embed = discord.Embed(
            title="🪙 Jopa Coin(TM) Minted!",
            description=f"{interaction.user.mention} has minted a **Jopa Coin(TM)**!",
            color=0xD4AF37,  # Gold
        )
        embed.set_footer(text=f"Cost: {cost} jopacoin")

        await safe_followup(interaction, embed=embed)
        self._maybe_curse_flame_for_shop(interaction, user_id, guild_id, item="jopa_coin")

    async def _handle_mystery_gift(
        self,
        interaction: discord.Interaction,
    ):
        """20k Mystery Gift. Bot side is a flex announcement; the real gift is fulfilled IRL."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_NEW_MYSTERY_GIFT_COST

        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can shop.",
                ephemeral=True,
            )
            return

        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        embed = discord.Embed(
            title="🎁 Mystery Gift Redeemed!",
            description=f"{interaction.user.mention} has redeemed a **Mystery Gift**!",
            color=0x9B59B6,  # Purple
        )
        embed.set_footer(text=f"Cost: {cost} jopacoin")

        await safe_followup(interaction, embed=embed)
        self._maybe_curse_flame_for_shop(interaction, user_id, guild_id, item="mystery_gift")

    async def _handle_witchs_curse(
        self,
        interaction: discord.Interaction,
        target: discord.Member,
    ):
        """Cast a 7-day Witch's Curse on the target. Anonymous; only ephemeral confirmation."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        cost = SHOP_WITCHS_CURSE_COST

        if self.curse_service is None:
            await interaction.response.send_message(
                "The witch's craft is currently unavailable.", ephemeral=True
            )
            return

        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can shop.",
                ephemeral=True,
            )
            return

        target_player = await asyncio.to_thread(
            self.player_service.get_player, target.id, guild_id
        )
        if not target_player:
            await interaction.response.send_message(
                f"{target.display_name} is not registered. The hex needs a willing victim of the league.",
                ephemeral=True,
            )
            return

        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for the hex, but you only have {balance}.",
                ephemeral=True,
            )
            return

        # Ephemeral defer — the curse is anonymous, only the caster gets confirmation.
        if not await safe_defer(interaction, ephemeral=True):
            return

        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)
        try:
            await self.curse_service.cast_curse(
                caster_id=user_id,
                target_id=target.id,
                guild_id=guild_id,
                days=WITCHS_CURSE_DURATION_DAYS,
            )
        except Exception:
            # Refund on unexpected failure so the buyer isn't charged for a hex that didn't land.
            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, cost)
            await safe_followup(
                interaction,
                content="The hex faltered unexpectedly. You have been refunded.",
                ephemeral=True,
            )
            return

        await safe_followup(
            interaction,
            content=(
                f"🧙‍♀️ Your hex on **{target.display_name}** is sealed for "
                f"{WITCHS_CURSE_DURATION_DAYS} days. The chat will see — they will not see you."
            ),
            ephemeral=True,
        )
        # Buying a curse still counts as the buyer engaging with the shop — if
        # the buyer is themselves cursed, fire the roll (neutral 5%).
        self._maybe_curse_flame_for_shop(interaction, user_id, guild_id, item="witchs_curse")

    def _maybe_curse_flame_for_shop(
        self,
        interaction: discord.Interaction,
        user_id: int,
        guild_id: int | None,
        *,
        item: str,
    ) -> None:
        """Spawn a fire-and-forget curse roll for a shop purchase by a (possibly cursed) buyer."""
        from services.curse_service import spawn_curse_flame

        target_display_name = (
            interaction.user.display_name if hasattr(interaction.user, "display_name") else None
        )
        spawn_curse_flame(
            self.curse_service,
            interaction.channel,
            target_id=user_id,
            guild_id=guild_id,
            system="shop",
            outcome="neutral",
            event_context={"item": item},
            target_display_name=target_display_name,
        )

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
                "You need to `/player register` before you can gamble. "
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

        if balance == cost:
            await interaction.response.send_message(
                f"You only have {balance} {JOPACOIN_EMOTE} — exactly the ante cost. "
                "Nothing left to double! Earn more first.",
                ephemeral=True,
            )
            return

        # Atomically claim the cooldown before any money moves (admins bypass).
        # The read-based check above is only a fast path for the friendly
        # remaining-time message; concurrent presses could both pass it, so
        # this check-and-set is the authoritative gate against double payouts.
        if not is_admin:
            claimed = await asyncio.to_thread(
                self.player_service.player_repo.try_claim_double_or_nothing,
                user_id, guild_id, now, DOUBLE_OR_NOTHING_COOLDOWN_SECONDS,
            )
            if not claimed:
                await interaction.response.send_message(
                    "You already tempted fate recently. "
                    "Wait for your Double or Nothing cooldown before trying again.",
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

        if won:
            # WIN: Double the remaining balance
            winnings = balance_after_cost
            final_balance = await asyncio.to_thread(
                self.player_service.adjust_balance, user_id, guild_id, winnings
            )
            result_title = "DOUBLE!"
            result_color = 0x00FF00  # Green
            flavor_event = FlavorEvent.DOUBLE_OR_NOTHING_WIN
        else:
            # LOSE: forfeit the staked remainder. Debit relatively (not
            # set_balance(0)) so a credit that landed after the balance read
            # (e.g. a tip mid-flip) isn't clobbered — mirrors the additive win
            # path. adjust_balance returns the true post-debit balance, so the
            # display stays correct even if a concurrent credit landed.
            final_balance = await asyncio.to_thread(
                self.player_service.adjust_balance, user_id, guild_id, -balance_after_cost
            )
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
                "You need to `/player register` before you can shop.",
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

        active_avoids = await asyncio.to_thread(
            soft_avoid_service.get_user_avoids,
            guild_id,
            user_id,
        )
        existing_avoid = next(
            (avoid for avoid in active_avoids if avoid.avoided_discord_id == target.id),
            None,
        )
        if existing_avoid:
            await interaction.response.send_message(
                (
                    f"Your soft avoid for **{target.display_name}** is already active "
                    f"with {existing_avoid.games_remaining} games remaining."
                ),
                ephemeral=True,
            )
            return

        pairings_service = getattr(self.bot, "pairings_service", None)
        pairing = None
        if pairings_service:
            pairing = await asyncio.to_thread(
                pairings_service.get_head_to_head,
                user_id,
                target.id,
                guild_id,
            )
        cost = _calculate_soft_avoid_cost(pairing)

        # Defer before the write so a slow DB call cannot leave a created avoid
        # behind a failed Discord interaction.
        if not await safe_defer(interaction, ephemeral=True):
            return

        try:
            purchase = await asyncio.to_thread(
                functools.partial(
                    soft_avoid_service.purchase_avoid,
                    guild_id=guild_id,
                    avoider_id=user_id,
                    avoided_id=target.id,
                    cost=cost,
                    games=SOFT_AVOID_GAMES,
                )
            )
        except Exception:
            logger.exception("Failed to complete soft avoid purchase")
            await safe_followup(
                interaction,
                content=(
                    "Soft avoid purchase failed before it could be activated. "
                    "You were not charged."
                ),
                ephemeral=True,
            )
            return

        if not purchase.success:
            if purchase.reason == "already_active" and purchase.avoid is not None:
                content = (
                    f"Your soft avoid for **{target.display_name}** is already active "
                    f"with {purchase.avoid.games_remaining} games remaining."
                )
            elif purchase.reason == "insufficient_balance":
                content = (
                    f"You need {cost} {JOPACOIN_EMOTE} for this, "
                    f"but you only have {purchase.balance}."
                )
            else:
                content = "Soft avoid purchase could not be completed. You were not charged."
            await safe_followup(interaction, content=content, ephemeral=True)
            return

        avoid = purchase.avoid
        if avoid is None:
            raise RuntimeError("Successful soft avoid purchase did not return an activation")

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
        await safe_followup(interaction, embed=embed, ephemeral=True)

        # Neon Degen Terminal hook (soft avoid purchase)
        try:
            neon = get_neon_service(self.bot)
            if neon:
                neon_result = await neon.on_soft_avoid(
                    user_id, guild_id,
                    cost=cost,
                    games=SOFT_AVOID_GAMES,
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
                "You need to `/player register` before you can shop.",
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

        # Check active deals to determine pricing
        active_deals = await asyncio.to_thread(package_deal_service.get_user_deals, guild_id, user_id)
        is_extend = any(d.partner_discord_id == target.id for d in active_deals)
        cost = _calculate_package_deal_cost(
            len(active_deals),
            is_extend,
            getattr(player, "glicko_rating", None),
            getattr(target_player, "glicko_rating", None),
        )

        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            if cost == PACKAGE_DEAL_NO_ACTIVE_COST and len(active_deals) == 0 and not is_extend:
                cost_detail = "*(No active package deals: 1 jopacoin)*"
            else:
                cost_detail = (
                    f"*(Base: {SHOP_PACKAGE_DEAL_BASE_COST} + "
                    f"Rating bonus: {cost - SHOP_PACKAGE_DEAL_BASE_COST})*"
                )
            await interaction.response.send_message(
                f"You need {cost} {JOPACOIN_EMOTE} for this, but you only have {balance}.\n"
                f"{cost_detail}",
                ephemeral=True,
            )
            return
        # Defer before deduction so we can't lose the response window after charging.
        if not await safe_defer(interaction, ephemeral=True):
            return
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        try:
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
        except Exception:
            logger.exception("Failed to create package deal purchase")
            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, cost)
            await safe_followup(
                interaction,
                content=(
                    "Package deal purchase failed before it could be activated. "
                    "Your jopacoin was refunded."
                ),
                ephemeral=True,
            )
            return

        # Build confirmation embed (ephemeral)
        cost_display = f"{cost} {JOPACOIN_EMOTE}"
        embed = discord.Embed(
            title="Package Deal Active",
            description=(
                f"You have a Package Deal with **{target.display_name}**.\n\n"
                f"**Games remaining:** {deal.games_remaining}\n"
                f"**Cost:** {cost_display}\n\n"
                f"When shuffling, the system will try to place you on the **same team**. "
                f"The deal count decreases each game where you're both playing "
                f"and successfully placed on the same team."
            ),
            color=0x2ECC71,  # Green for partnership
        )
        if cost == PACKAGE_DEAL_NO_ACTIVE_COST and len(active_deals) == 0 and not is_extend:
            embed.set_footer(text="No active package deals: 1 jopacoin")
        else:
            embed.set_footer(
                text=f"Base cost: {SHOP_PACKAGE_DEAL_BASE_COST} + Rating bonus: {cost - SHOP_PACKAGE_DEAL_BASE_COST}"
            )

        # Ephemeral response (private - target not notified).
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @shop_group.command(name="avoids", description="View your active soft avoids")
    @require_guild
    async def myavoids(self, interaction: discord.Interaction):
        """View your active soft avoids."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id

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

    @shop_group.command(name="deals", description="View your active package deals")
    @require_guild
    async def mydeals(self, interaction: discord.Interaction):
        """View your active package deals."""
        user_id = interaction.user.id
        guild_id = interaction.guild.id

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

    @shop_group.command(name="mana", description="Spend mana on color-exclusive items")
    @app_commands.describe(
        item="The mana item to purchase",
        target="Target player (Sanctuary, Blood Pact, Insight)",
    )
    @app_commands.choices(item=[
        # Red
        app_commands.Choice(name="Red • Cheap • Pyroclasm (25 JC) — burn JC from 3 players, claim a bounty", value="pyroclasm"),
        app_commands.Choice(name="Red • Mid • Dynamite Cache (35 JC) — next 3 digs +75% yield", value="dynamite_cache"),
        app_commands.Choice(name="Red • Ult • Wildfire (150 JC, taps mana) — drain JC from every positive player", value="wildfire"),
        # Blue
        app_commands.Choice(name="Blue • Cheap • Insight (10 JC) — peek at any digger's stats", value="insight"),
        app_commands.Choice(name="Blue • Mid • Mana Shield (35 JC) — refund 60% of largest 24h loss", value="mana_shield"),
        app_commands.Choice(name="Blue • Ult • Counterspell (75 JC, taps mana) — 24h PvP immunity", value="counterspell"),
        # Green
        app_commands.Choice(name="Green • Cheap • Sapling (10 JC) — shave 45min off /dig cooldown", value="sapling"),
        app_commands.Choice(name="Green • Mid • Regrowth (25 JC) — recover 35% of last 24h losses", value="regrowth"),
        app_commands.Choice(name="Green • Ult • Overgrowth (90 JC, taps mana) — 12h dig overdrive", value="overgrowth"),
        # White
        app_commands.Choice(name="White • Cheap • Reprieve (15 JC) — 50% shield, 25 JC pool, rolling 24h recovery", value="reprieve"),
        app_commands.Choice(name="White • Mid • Aegis (35 JC) — fully absorb 75 JC of hostile losses for 24h", value="aegis"),
        app_commands.Choice(name="White • Ult • Sanctuary (90 JC, taps mana) — shared 150 JC shield for you + an ally", value="sanctuary"),
        # Black
        app_commands.Choice(
            name=(
                "Black • Cheap • Soul Harvest (25 JC) — drain 2 JC (20% chance 3) from each "
                f"{HOSTILE_LOSS_MIN_BALANCE}+ JC player"
            ),
            value="soul_harvest",
        ),
        app_commands.Choice(name="Black • Mid • Blood Pact (75 JC) — skim 25% of target's earnings 24h", value="blood_pact"),
        app_commands.Choice(name="Black • Ult • Dark Bargain (150 JC, taps mana) — 800 JC now, 700 due later", value="dark_bargain"),
    ])
    @app_commands.checks.cooldown(1, 10)
    @require_guild
    async def manashop(
        self,
        interaction: discord.Interaction,
        item: app_commands.Choice[str],
        target: discord.Member = None,
    ):
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id
        user_id = interaction.user.id

        # Check registration
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.followup.send("You need to `/player register` first.", ephemeral=True)
            return

        mana_effects_service = getattr(self.bot, "mana_effects_service", None)
        mana_service = getattr(self.bot, "mana_service", None)
        mana_repo = getattr(self.bot, "mana_repo", None)
        buff_service = getattr(self.bot, "buff_service", None)
        # ``MagicMock`` fabricates arbitrary attributes, so prefer the bot's
        # concrete attribute dictionary. Explicitly configured test doubles and
        # the real bot both store services there.
        bot_attrs = getattr(self.bot, "__dict__", {})
        protection_service = (
            bot_attrs.get("protection_service")
            if isinstance(bot_attrs, dict)
            else getattr(self.bot, "protection_service", None)
        )
        if not (mana_effects_service and mana_service and mana_repo):
            await interaction.followup.send("Mana system not available.", ephemeral=True)
            return

        # Tap-state check: a tapped player has no active color, /shop mana locked.
        tapped = await asyncio.to_thread(mana_service.is_mana_consumed, user_id, guild_id)
        if tapped:
            await interaction.followup.send(
                "Your mana is spent — today's ultimate consumed your color. "
                "Come back tomorrow.",
                ephemeral=True,
            )
            return

        # Resolve effects (returns defaults if mana not assigned today).
        effects = await asyncio.to_thread(mana_effects_service.get_effects, user_id, guild_id)
        if not effects.color:
            await interaction.followup.send(
                "You have no active mana today. Use `/mana` first.",
                ephemeral=True,
            )
            return

        # Item catalog:
        # tier:    "cheap" / "mid" / "ult"
        # color:   required mana color
        # cost:    JC cost
        MANA_ITEMS: dict[str, dict] = {
            # Cheap
            "pyroclasm": {"tier": "cheap", "color": "Red", "cost": 25, "name": "Pyroclasm"},
            "insight": {"tier": "cheap", "color": "Blue", "cost": 10, "name": "Insight"},
            "sapling": {"tier": "cheap", "color": "Green", "cost": 10, "name": "Sapling"},
            "reprieve": {"tier": "cheap", "color": "White", "cost": 15, "name": "Reprieve"},
            "soul_harvest": {"tier": "cheap", "color": "Black", "cost": SOUL_HARVEST_COST, "name": "Soul Harvest"},
            # Mid
            "dynamite_cache": {"tier": "mid", "color": "Red", "cost": 35, "name": "Dynamite Cache"},
            "mana_shield": {"tier": "mid", "color": "Blue", "cost": 35, "name": "Mana Shield"},
            "regrowth": {"tier": "mid", "color": "Green", "cost": 25, "name": "Regrowth"},
            "aegis": {"tier": "mid", "color": "White", "cost": 35, "name": "Aegis"},
            "blood_pact": {"tier": "mid", "color": "Black", "cost": 75, "name": "Blood Pact"},
            # Ultimate (taps mana)
            "wildfire": {"tier": "ult", "color": "Red", "cost": 150, "name": "Wildfire"},
            "counterspell": {"tier": "ult", "color": "Blue", "cost": 75, "name": "Counterspell"},
            "overgrowth": {"tier": "ult", "color": "Green", "cost": 90, "name": "Overgrowth"},
            "sanctuary": {"tier": "ult", "color": "White", "cost": 90, "name": "Sanctuary"},
            "dark_bargain": {"tier": "ult", "color": "Black", "cost": 150, "name": "Dark Bargain"},
        }

        item_key = item.value
        if item_key not in MANA_ITEMS:
            await interaction.followup.send("Unknown item.", ephemeral=True)
            return

        spec = MANA_ITEMS[item_key]
        required_color = spec["color"]
        cost = int(spec["cost"])
        display_name = spec["name"]
        tier = spec["tier"]

        if effects.color != required_color:
            color_to_land = {"Red": "Mountain", "Blue": "Island", "Green": "Forest", "White": "Plains", "Black": "Swamp"}
            await interaction.followup.send(
                f"**{display_name}** requires **{required_color}** mana ({color_to_land.get(required_color, '?')}). "
                f"Your current mana is **{effects.color}**.",
                ephemeral=True,
            )
            return

        # Validate target requirements upfront so we never charge for an
        # invalid use. ``insight`` permits self-target (peeking at your own
        # stats is a no-op); ``sanctuary`` and ``blood_pact`` require an
        # *other* player so the ally / victim role is meaningful.
        target_required_for = {"sanctuary", "blood_pact", "insight"}
        no_self_target_items = {"sanctuary", "blood_pact"}
        if item_key in target_required_for:
            if not target:
                await interaction.followup.send(
                    f"**{display_name}** requires a `target` player.",
                    ephemeral=True,
                )
                return
            if target.id == user_id and item_key in no_self_target_items:
                await interaction.followup.send(
                    f"**{display_name}** must target another player.",
                    ephemeral=True,
                )
                return

        # Check balance up-front — we still want a clean "not enough JC"
        # message before any side effect runs.
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < cost:
            await interaction.followup.send(
                f"You need {cost} {JOPACOIN_EMOTE} for {display_name}. You have {balance}.",
                ephemeral=True,
            )
            return

        # Item claim BEFORE charging so every manashop item is once-per-day and
        # concurrent calls don't briefly deduct then refund.
        from services.mana_service import get_today_pst as _today_pst
        today = _today_pst()
        claimed = await asyncio.to_thread(
            mana_repo.mark_item_used_atomic, user_id, guild_id, item_key, today,
        )
        if not claimed:
            await interaction.followup.send(
                f"**{display_name}** is once-per-day. Already used today.",
                ephemeral=True,
            )
            return

        if tier == "ult":
            tapped_now = await asyncio.to_thread(
                mana_repo.mark_mana_consumed_atomic, user_id, guild_id,
            )
            if not tapped_now:
                try:
                    await asyncio.to_thread(
                        mana_repo.unmark_item_used, user_id, guild_id, item_key, today,
                    )
                except Exception:
                    logger.exception("Failed to release daily-use slot for %s", item_key)
                await interaction.followup.send(
                    "Your mana was already tapped this turn. Try again tomorrow.",
                    ephemeral=True,
                )
                return

        # Charge cost AFTER the claim won so failure paths don't show a
        # charge-then-refund flicker.
        await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -cost)

        async def _refund(reason: str) -> None:
            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, cost)
            # Release the daily-use slot so the player can retry after the failure.
            try:
                await asyncio.to_thread(
                    mana_repo.unmark_item_used, user_id, guild_id, item_key, today,
                )
            except Exception:
                logger.exception("Failed to release daily-use slot for %s", item_key)
            await interaction.followup.send(reason, ephemeral=True)

        async def _apply_hostile_loss(
            victim_id: int,
            amount: int,
            *,
            kind: str,
            event_key: str,
            destination: str,
            recipient_id: int | None = None,
            clamp_to_balance: bool = True,
        ):
            """Use the protection gateway, with a tagged legacy fallback."""
            if protection_service is not None:
                return await asyncio.to_thread(
                    protection_service.apply_hostile_loss,
                    victim_id,
                    guild_id,
                    amount,
                    kind=kind,
                    actor_id=user_id,
                    event_key=event_key,
                    destination=destination,
                    recipient_id=recipient_id,
                    clamp_to_balance=clamp_to_balance,
                    min_balance=HOSTILE_LOSS_MIN_BALANCE,
                )

            await asyncio.to_thread(
                self.player_service.adjust_balance,
                victim_id,
                guild_id,
                -amount,
                source="manashop",
                actor_id=user_id,
                related_type="hostile_loss",
                related_id=event_key,
                reason=f"manashop {kind} victim debit",
                metadata={
                    "kind": kind,
                    "attempted_loss": amount,
                    "destination": destination,
                    "recipient_id": recipient_id,
                },
            )
            return {
                "attempted_loss": amount,
                "absorbed_amount": 0,
                "applied_loss": amount,
            }

        async def _apply_hostile_losses(losses: list[dict]) -> list:
            """Batch real gateway calls; preserve lightweight-test fallback."""
            if type(protection_service) is ProtectionService:
                requests = [
                    {
                        "victim_id": loss["victim_id"],
                        "guild_id": guild_id,
                        "amount": loss["amount"],
                        "kind": loss["kind"],
                        "actor_id": user_id,
                        "event_key": loss["event_key"],
                        "destination": loss["destination"],
                        "recipient_id": loss.get("recipient_id"),
                        "clamp_to_balance": loss.get("clamp_to_balance", True),
                        "min_balance": HOSTILE_LOSS_MIN_BALANCE,
                    }
                    for loss in losses
                ]
                return await asyncio.to_thread(protection_service.apply_hostile_losses, requests)

            outcomes = []
            for loss in losses:
                try:
                    outcome = await _apply_hostile_loss(**loss)
                except Exception as exc:
                    outcomes.append(exc)
                else:
                    outcomes.append(outcome)
            return outcomes

        # Mana Conduit relic: refund 25% of tap-mana ultimate cost.
        ult_refund = 0
        if tier == "ult":
            dig_service = getattr(self.bot, "dig_service", None)
            if dig_service is not None:
                try:
                    has_conduit = await asyncio.to_thread(
                        dig_service._has_relic, user_id, guild_id, "mana_conduit",
                    )
                    if has_conduit:
                        ult_refund = max(1, int(cost * 0.25))
                        await asyncio.to_thread(
                            self.player_service.adjust_balance, user_id, guild_id, ult_refund,
                        )
                except Exception:
                    ult_refund = 0

        # ── Execute item effect ───────────────────────────────────────
        new_balance = balance - cost + ult_refund

        if item_key == "pyroclasm":
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            eligible = [
                p for p in all_players
                if p.discord_id != user_id
                and p.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
            ]
            if not eligible:
                await _refund("No eligible targets to scorch; refunded.")
                return
            targets = random.sample(eligible, min(3, len(eligible)))
            event_prefix = f"pyroclasm:{uuid.uuid4().hex}"
            total_destroyed = 0
            total_absorbed = 0
            victim_lines = []
            loss_targets = []
            losses = []
            for t in targets:
                destroy_amt = scale_deflationary_minigame_jc_delta(
                    random.randint(12, 28)
                )
                destroy_amt = min(destroy_amt, t.jopacoin_balance)
                if destroy_amt > 0:
                    loss_targets.append((t, destroy_amt))
                    losses.append(
                        {
                            "victim_id": t.discord_id,
                            "amount": destroy_amt,
                            "kind": "pyroclasm",
                            "event_key": f"{event_prefix}:{t.discord_id}",
                            "destination": "burn",
                        }
                    )
            outcomes = await _apply_hostile_losses(losses)
            for (target_player, destroy_amt), outcome in zip(loss_targets, outcomes, strict=True):
                if isinstance(outcome, Exception):
                    logger.warning(
                        "Pyroclasm failed for victim %s: %s",
                        target_player.discord_id,
                        outcome,
                    )
                    continue
                applied = _protection_result_int(outcome, "applied_loss", destroy_amt)
                absorbed = _protection_result_int(outcome, "absorbed_amount")
                total_destroyed += applied
                total_absorbed += absorbed
                shield_note = f" (shield absorbed {absorbed})" if absorbed else ""
                victim_lines.append(
                    f"  - {target_player.name}: -{applied} {JOPACOIN_EMOTE}{shield_note}"
                )
            bounty = min(35, total_destroyed // 2)
            if bounty > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance,
                    user_id,
                    guild_id,
                    bounty,
                    source="manashop",
                    actor_id=user_id,
                    related_type="hostile_loss_reward",
                    related_id=event_prefix,
                    reason="manashop pyroclasm bounty",
                    metadata={
                        "kind": "pyroclasm",
                        "applied_loss": total_destroyed,
                        "absorbed_amount": total_absorbed,
                    },
                )
            victims_text = "\n".join(victim_lines) if victim_lines else "  No eligible targets."
            shield_text = (
                f" Shields absorbed **{total_absorbed} {JOPACOIN_EMOTE}**."
                if total_absorbed
                else ""
            )
            await interaction.followup.send(
                f"⛰️🔥 **PYROCLASM** — {interaction.user.mention} unleashes chaos!\n"
                f"{victims_text}\n"
                f"**{total_destroyed} {JOPACOIN_EMOTE} burned**.{shield_text} "
                f"You claim **{bounty} {JOPACOIN_EMOTE}** from the ash.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {balance - cost + bounty})"
            )

        elif item_key == "insight":
            recipient = await asyncio.to_thread(self.player_service.get_player, target.id, guild_id)
            if not recipient:
                await _refund(f"{target.mention} is not registered.")
                return
            dig_service = getattr(self.bot, "dig_service", None)
            tunnel_info = None
            if dig_service is not None:
                try:
                    tunnel_info = await asyncio.to_thread(
                        dig_service.get_tunnel_info, target.id, guild_id,
                    )
                except Exception:
                    tunnel_info = None
            if tunnel_info is None:
                await interaction.followup.send(
                    f"🏝️🔍 **INSIGHT** — {target.mention} hasn't started digging.\n"
                    f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
                )
                return
            tunnel = tunnel_info.get("tunnel") or tunnel_info
            relics = tunnel_info.get("relics") or []
            relic_names = ", ".join(
                r.get("name", r.get("artifact_id", "?")) for r in relics if r
            ) or "—"
            await interaction.followup.send(
                f"🏝️🔍 **INSIGHT** — {interaction.user.mention} reads the threads of fate.\n"
                f"**{target.display_name}** depth `{tunnel.get('depth', 0)}` "
                f"luminosity `{tunnel.get('luminosity', 0)}` "
                f"prestige `{tunnel.get('prestige_level', 0)}`\n"
                f"Equipped relics: {relic_names}\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "sapling":
            dig_service = getattr(self.bot, "dig_service", None)
            shaved_seconds = 0
            if dig_service is not None:
                try:
                    shaved_seconds = await asyncio.to_thread(
                        dig_service.dig_repo.shave_cooldown, user_id, guild_id, 45 * 60,
                    )
                except AttributeError:
                    # Fallback: rewind last_dig_at on the tunnel directly.
                    try:
                        tunnel = await asyncio.to_thread(
                            dig_service.dig_repo.get_tunnel, user_id, guild_id,
                        )
                        if tunnel and tunnel.get("last_dig_at"):
                            new_last = max(0, int(tunnel["last_dig_at"]) - 45 * 60)
                            await asyncio.to_thread(
                                dig_service.dig_repo.update_tunnel,
                                user_id, guild_id, last_dig_at=new_last,
                            )
                            shaved_seconds = 45 * 60
                    except Exception:
                        shaved_seconds = 0
                except Exception:
                    shaved_seconds = 0
            await interaction.followup.send(
                f"🌲🌱 **SAPLING** — {interaction.user.mention} accelerates growth.\n"
                f"`/dig` cooldown reduced by {shaved_seconds // 60}m.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "reprieve":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            try:
                buff_id = await asyncio.to_thread(
                    buff_service.grant_reprieve, user_id, guild_id
                )
            except Exception:
                logger.exception("Reprieve grant failed")
                await _refund("Could not raise the reprieve; refunded.")
                return

            recovered = 0
            if protection_service is not None:
                try:
                    recovered = int(
                        await asyncio.to_thread(
                            protection_service.reconcile_purchased_pool,
                            user_id,
                            guild_id,
                            buff_id,
                            24 * 3600,
                        )
                    )
                except Exception:
                    logger.exception("Reprieve retroactive reconciliation failed")

            await interaction.followup.send(
                f"🌾🕊️ **REPRIEVE** — {interaction.user.mention} raises a rolling ward.\n"
                f"Recovered **{recovered} {JOPACOIN_EMOTE}** from eligible losses "
                f"in the past 24h. Remaining capacity protects 50% of hostile "
                f"losses for 24h, up to **25 {JOPACOIN_EMOTE}** total.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance + recovered})"
            )

        elif item_key == "soul_harvest":
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            eligible = [
                p for p in all_players
                if p.discord_id != user_id
                and p.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
            ]
            if not eligible:
                await _refund("No living souls to drain; refunded.")
                return
            event_prefix = f"soul_harvest:{uuid.uuid4().hex}"
            total_drained = 0
            total_absorbed = 0
            scaled_drain = scale_minigame_jc_delta(SOUL_HARVEST_DRAIN_PER_TARGET)
            requested_drains = []
            losses = []
            for p in eligible:
                bonus_drain = 1 if random.random() < SOUL_HARVEST_BONUS_DRAIN_CHANCE else 0
                drain = min(
                    scaled_drain + bonus_drain,
                    p.jopacoin_balance,
                )
                requested_drains.append((p, drain))
                losses.append(
                    {
                        "victim_id": p.discord_id,
                        "amount": drain,
                        "kind": "soul_harvest",
                        "event_key": f"{event_prefix}:{p.discord_id}",
                        "destination": "player",
                        "recipient_id": user_id,
                    }
                )
            outcomes = await _apply_hostile_losses(losses)
            for (victim, drain), outcome in zip(requested_drains, outcomes, strict=True):
                if isinstance(outcome, Exception):
                    logger.warning(
                        "Soul Harvest failed for victim %s: %s",
                        victim.discord_id,
                        outcome,
                    )
                    continue
                total_drained += _protection_result_int(outcome, "applied_loss", drain)
                total_absorbed += _protection_result_int(outcome, "absorbed_amount")
            # ProtectionService moves each applied loss to the caster inside the
            # hostile-loss transaction. The legacy fallback debits victims only,
            # so preserve the old aggregate credit when no gateway is wired.
            if protection_service is None and total_drained > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance,
                    user_id,
                    guild_id,
                    total_drained,
                    source="manashop",
                    actor_id=user_id,
                    related_type="hostile_loss_reward",
                    related_id=event_prefix,
                    reason="manashop soul harvest collected drain",
                    metadata={
                        "kind": "soul_harvest",
                        "applied_loss": total_drained,
                        "absorbed_amount": total_absorbed,
                    },
                )
            shield_text = (
                f" Shields absorbed **{total_absorbed} {JOPACOIN_EMOTE}**."
                if total_absorbed
                else ""
            )
            await interaction.followup.send(
                f"🌿💀 **SOUL HARVEST** — {interaction.user.mention} drains the living!\n"
                f"Drained up to **3 {JOPACOIN_EMOTE}** from **{len(eligible)}** players. "
                f"Gained **{total_drained} {JOPACOIN_EMOTE}**.{shield_text}\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {balance - cost + total_drained})"
            )

        elif item_key == "dynamite_cache":
            # Mid: stash a 3-charge yield buff on the dig service.
            dig_service = getattr(self.bot, "dig_service", None)
            if dig_service is None:
                await _refund("Dig system unavailable; refunded.")
                return
            try:
                await asyncio.to_thread(
                    dig_service.set_temp_buff, user_id, guild_id,
                    {
                        "id": "dynamite_cache",
                        "name": "Dynamite Cache",
                        "duration_digs": 3,
                        "effect": {"yield_multiplier": 1.75},
                    },
                )
            except Exception:
                logger.exception("Dynamite Cache buff write failed")
                await _refund("Could not pack the cache; refunded.")
                return
            await interaction.followup.send(
                f"⛰️🧨 **DYNAMITE CACHE** — {interaction.user.mention} packs the next 3 digs hot.\n"
                f"Yield +75% on your next 3 swings.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "mana_shield":
            now_ts = int(time.time())
            cutoff = now_ts - 24 * 3600
            largest_loss = await asyncio.to_thread(
                self._compute_largest_recent_loss, user_id, guild_id, cutoff,
            )
            base_refund = min(120, int(largest_loss * 0.60))
            refund = await self._adjust_generated_mana_reward(
                guild_id, base_refund
            )
            if refund > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance,
                    user_id,
                    guild_id,
                    refund,
                    source="mana_reward",
                    actor_id=user_id,
                    related_type="manashop_mana_shield",
                    related_id=today,
                    reason="scaled manashop Mana Shield recovery",
                    metadata={
                        "base_reward": base_refund,
                        "adjusted_reward": refund,
                    },
                )
            await interaction.followup.send(
                f"🏝️🛡️ **MANA SHIELD** — {interaction.user.mention} reclaims {refund} {JOPACOIN_EMOTE}.\n"
                f"(60% of your largest loss in the past 24h, capped at 120. "
                f"Cost: {cost} {JOPACOIN_EMOTE}, balance: {balance - cost + refund})"
            )

        elif item_key == "regrowth":
            # Rolling 24h window (matching Mana Shield). A 4 AM-PST calendar
            # bucket dropped late-night losses once a player crossed the reset,
            # so anyone checking the morning after a losing session saw 0.
            cutoff = int(time.time()) - 24 * 3600
            total_lost = await asyncio.to_thread(
                self._compute_cumulative_recent_losses, user_id, guild_id, cutoff,
            )
            base_recovery = min(120, int(total_lost * 0.35))
            recovery = await self._adjust_generated_mana_reward(
                guild_id, base_recovery
            )
            if recovery > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance,
                    user_id,
                    guild_id,
                    recovery,
                    source="mana_reward",
                    actor_id=user_id,
                    related_type="manashop_regrowth",
                    related_id=today,
                    reason="scaled manashop Regrowth recovery",
                    metadata={
                        "base_reward": base_recovery,
                        "adjusted_reward": recovery,
                    },
                )
            await interaction.followup.send(
                f"🌲💚 **REGROWTH** — {interaction.user.mention} recovers {recovery} {JOPACOIN_EMOTE}.\n"
                f"(35% of your last 24h losses, capped at 120. "
                f"Cost: {cost} {JOPACOIN_EMOTE}, balance: {balance - cost + recovery})"
            )

        elif item_key == "aegis":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            try:
                await asyncio.to_thread(buff_service.grant_aegis, user_id, guild_id)
            except Exception:
                logger.exception("Aegis grant failed")
                await _refund("Could not raise the ward; refunded.")
                return
            await interaction.followup.send(
                f"🌾🛡️ **AEGIS** — {interaction.user.mention} raises a fortified ward.\n"
                f"For 24h, it fully absorbs up to **75 {JOPACOIN_EMOTE}** of "
                f"hostile losses (or one non-JC sabotage).\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "blood_pact":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            try:
                await asyncio.to_thread(
                    buff_service.grant_blood_pact, user_id, guild_id, target.id,
                )
            except Exception:
                logger.exception("Blood Pact grant failed")
                await _refund("Could not seal the pact; refunded.")
                return
            await interaction.followup.send(
                f"🌿🩸 **BLOOD PACT** — {interaction.user.mention} marks {target.mention} for skim.\n"
                f"For 24h, you receive 25% of {target.display_name}'s match/wheel/dig earnings (cap 150 JC).\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, balance: {new_balance})"
            )

        elif item_key == "wildfire":
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            eligible = [
                p for p in all_players
                if p.discord_id != user_id
                and p.jopacoin_balance >= HOSTILE_LOSS_MIN_BALANCE
            ]
            event_prefix = f"wildfire:{uuid.uuid4().hex}"
            total_drained = 0
            total_absorbed = 0
            requested_drains = []
            losses = []
            for p in eligible:
                drain = scale_deflationary_minigame_jc_delta(random.randint(4, 14))
                drain = min(drain, p.jopacoin_balance)
                if drain > 0:
                    requested_drains.append((p, drain))
                    losses.append(
                        {
                            "victim_id": p.discord_id,
                            "amount": drain,
                            "kind": "wildfire",
                            "event_key": f"{event_prefix}:{p.discord_id}",
                            "destination": "burn",
                        }
                    )
            outcomes = await _apply_hostile_losses(losses)
            for (victim, drain), outcome in zip(requested_drains, outcomes, strict=True):
                if isinstance(outcome, Exception):
                    logger.warning(
                        "Wildfire failed for victim %s: %s",
                        victim.discord_id,
                        outcome,
                    )
                    continue
                total_drained += _protection_result_int(outcome, "applied_loss", drain)
                total_absorbed += _protection_result_int(outcome, "absorbed_amount")
            user_gain = int(total_drained * 0.45)
            if user_gain > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance,
                    user_id,
                    guild_id,
                    user_gain,
                    source="manashop",
                    actor_id=user_id,
                    related_type="hostile_loss_reward",
                    related_id=event_prefix,
                    reason="manashop wildfire harvest",
                    metadata={
                        "kind": "wildfire",
                        "applied_loss": total_drained,
                        "absorbed_amount": total_absorbed,
                    },
                )
            shield_text = (
                f" Shields absorbed **{total_absorbed} {JOPACOIN_EMOTE}**."
                if total_absorbed
                else ""
            )
            await interaction.followup.send(
                f"⛰️🔥🔥 **WILDFIRE** — {interaction.user.mention} consumes the day's color in flame.\n"
                f"Drained **{total_drained} {JOPACOIN_EMOTE}** from {len(eligible)} players. "
                f"You claim **{user_gain} {JOPACOIN_EMOTE}** of the harvest."
                f"{shield_text}\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, mana spent, balance: {balance - cost + user_gain + ult_refund})"
            )

        elif item_key == "counterspell":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            try:
                await asyncio.to_thread(buff_service.grant_counterspell, user_id, guild_id)
            except Exception:
                logger.exception("Counterspell grant failed")
                await _refund("Could not weave the ward; refunded.")
                return
            await interaction.followup.send(
                f"🏝️🜨 **COUNTERSPELL** — {interaction.user.mention} weaves a 24h ward.\n"
                f"All Pyroclasm / Soul Harvest / Sabotage / Blood Pact / Wildfire targeting you "
                f"is repelled for the next 24 hours.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})"
            )

        elif item_key == "overgrowth":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            try:
                await asyncio.to_thread(buff_service.grant_overgrowth, user_id, guild_id)
            except Exception:
                logger.exception("Overgrowth grant failed")
                await _refund("Could not seed the overgrowth; refunded.")
                return
            await interaction.followup.send(
                f"🌲🌳 **OVERGROWTH** — {interaction.user.mention} burns the day's mana into the soil.\n"
                f"For 12h: your next 10 digs get +10 JC and cave-in chance halved.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})"
            )

        elif item_key == "sanctuary":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            recipient = await asyncio.to_thread(self.player_service.get_player, target.id, guild_id)
            if not recipient:
                await _refund(f"{target.mention} is not registered; refunded.")
                return
            try:
                await asyncio.to_thread(
                    buff_service.grant_sanctuary, user_id, guild_id, target.id,
                )
            except Exception:
                logger.exception("Sanctuary grant failed")
                await _refund("Could not bind the sanctuary; refunded.")
                return
            await interaction.followup.send(
                f"🌾🕊️ **SANCTUARY** — {interaction.user.mention} shelters {target.mention}.\n"
                f"For 24h, both of you share **150 {JOPACOIN_EMOTE}** of full "
                f"hostile-loss protection and non-JC PvP immunity.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, mana spent, balance: {new_balance})"
            )

        elif item_key == "dark_bargain":
            if buff_service is None:
                await _refund("Buff system unavailable; refunded.")
                return
            # Write the debt FIRST. If the credit fails after the debt is
            # recorded, the player is no worse off than skipping the item; if
            # we credited first and the debt grant raised, the +800 would be
            # leaked with no obligation to repay.
            try:
                await asyncio.to_thread(
                    buff_service.grant_dark_bargain_debt,
                    user_id, guild_id, amount_due=700, due_in_days=7,
                )
            except Exception:
                logger.exception("Dark Bargain debt grant failed")
                await _refund("Could not strike the bargain; refunded.")
                return
            try:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, 800,
                )
            except Exception:
                logger.exception("Dark Bargain credit failed; debt remains active")
                await _refund(
                    "Could not credit the bargain; refunded. Debt note has been "
                    "recorded — contact an admin if it doesn't clear.",
                )
                return
            await interaction.followup.send(
                f"🌿💀 **DARK BARGAIN** — {interaction.user.mention} signs in red ink.\n"
                f"+800 {JOPACOIN_EMOTE} now. 700 due in 7 days. Default: -1600 + bankruptcy +5 matches.\n"
                f"(cost: {cost} {JOPACOIN_EMOTE}, mana spent, balance: {balance - cost + 800 + ult_refund})"
            )


    def _compute_largest_recent_loss(
        self, discord_id: int, guild_id: int | None, cutoff_ts: int
    ) -> int:
        """Return the largest single JC loss for the player since `cutoff_ts`.

        Match-bet and wheel losses are aggregated in SQL without materializing
        the player's lifetime histories.
        """
        if not self.gambling_stats_service:
            return 0
        aggregates = self.gambling_stats_service.bet_repo.get_recent_loss_aggregates(
            discord_id, guild_id, cutoff_ts
        )
        return aggregates["largest"]

    def _compute_cumulative_recent_losses(
        self, discord_id: int, guild_id: int | None, cutoff_ts: int
    ) -> int:
        """Return the total JC lost by the player since `cutoff_ts`.

        Sums match-bet and wheel losses in SQL. Returns 0 if no losses or no
        signal source is available.
        """
        if not self.gambling_stats_service:
            return 0
        aggregates = self.gambling_stats_service.bet_repo.get_recent_loss_aggregates(
            discord_id, guild_id, cutoff_ts
        )
        return aggregates["total"]

async def setup(bot: commands.Bot):
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    match_service = getattr(bot, "match_service", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)
    recalibration_service = getattr(bot, "recalibration_service", None)
    dig_service = getattr(bot, "dig_service", None)
    curse_service = getattr(bot, "curse_service", None)

    await bot.add_cog(ShopCommands(
        bot, player_service, match_service, flavor_text_service, gambling_stats_service,
        recalibration_service, dig_service, curse_service,
    ))
