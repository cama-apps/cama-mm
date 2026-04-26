"""
Discord commands for prediction markets (continuous-quote order-book mechanic).

Each market has a `current_price` (integer 1..99 = % implied probability), and
the bot maintains a small ladder of bids and asks around it. Trades sweep the
relevant book side and lock in at level prices. A periodic refresh worker
nudges fair toward the observed mid + small uniform drift, then reposts the
ladder. Settlement pays each winning contract `PREDICTION_CONTRACT_VALUE` jopa.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import re
from typing import Any

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_gamba_channel
from config import (
    PREDICTION_CONTRACT_VALUE,
    PREDICTION_INITIAL_FAIR_DEFAULT,
    PREDICTION_PRICE_HIGH,
    PREDICTION_PRICE_LOW,
    PREDICTION_RECENT_TRADES_SHOWN,
)
from services.permissions import has_admin_permission
from services.prediction_service import PredictionService
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.predictions")

MARKET_TITLE_PREFIX = "📈 Market #"
MARKET_TITLE_RE = re.compile(rf"{re.escape(MARKET_TITLE_PREFIX)}(\d+)")


# --------------------------------------------------------------------------- #
# Embed rendering
# --------------------------------------------------------------------------- #

def _build_ladder_fields(book: dict) -> list[tuple[str, str, bool]]:
    """Return four inline embed fields for the ladder: Buy YES, Buy NO, Sell YES, Sell NO.

    Returned shape: [(field_name, field_value, inline), ...]. Each value is a
    short one-liner like ``"18 x5 / 19 x5 / 20 x5"`` showing top-of-book first
    then deeper levels, separated by ``/``. NO sides use the mirror: NO ask =
    100 - YES bid, NO bid = 100 - YES ask. Empty sides render as ``"(none)"``.
    """
    asks = book.get("yes_asks", [])  # cheapest first
    bids = book.get("yes_bids", [])  # highest first

    def _fmt(levels: list[tuple[int, int]], mirror: bool) -> str:
        if not levels:
            return "(none)"
        return " / ".join(
            f"{(100 - p) if mirror else p} x{s}" for p, s in levels
        )

    return [
        ("Buy YES",  _fmt(asks, mirror=False), True),
        ("Buy NO",   _fmt(bids, mirror=True),  True),
        ("Sell YES", _fmt(bids, mirror=False), True),
        ("Sell NO",  _fmt(asks, mirror=True),  True),
    ]


def _trade_link(prediction: dict) -> str | None:
    """Build a Discord deep link to the embed message inside a market thread.

    Falls back from the message URL to the thread URL if ``embed_message_id``
    is missing, and returns None when neither is known.
    """
    guild_id = prediction.get("guild_id")
    thread_id = prediction.get("thread_id")
    embed_message_id = prediction.get("embed_message_id")
    if guild_id is None or thread_id is None:
        return None
    base = f"https://discord.com/channels/{guild_id}/{thread_id}"
    if embed_message_id is not None:
        return f"{base}/{embed_message_id}"
    return base


def _format_market_field(prediction: dict, *, with_delta: bool = False) -> tuple[str, str]:
    """Build (field_name, field_value) for one market in /predict list or the digest.

    Field name carries the market id and current price (and optional delta arrow);
    field value carries the question text (mentions resolve here since this is
    not a code block) and a [Trade →] markdown link to the trade buttons.
    """
    pid = prediction.get("prediction_id", "?")
    price = prediction.get("current_price")
    price_str = str(price) if price is not None else "?"

    delta_str = ""
    if with_delta and price is not None:
        prev = prediction.get("prev_price")
        if prev is not None and prev != price:
            arrow = "↑" if price > prev else "↓"
            delta_str = f"  ({arrow}{abs(price - prev)} today)"

    name = f"📈 #{pid}  ·  price {price_str}{delta_str}"

    question = (prediction.get("question") or "").strip()
    if len(question) > 200:
        question = question[:197] + "..."
    quoted = f'"{question}"' if question else ""

    link = _trade_link(prediction)
    link_line = f"\n[Trade →]({link})" if link else ""

    value = f"{quoted}{link_line}".strip() or "—"
    return (name, value)


def _format_recent_trades(trades: list[dict], cog: PredictionCommands) -> str:
    """One-line per recent trade. Uses display name when available, falls back to id."""
    if not trades:
        return "_no trades yet_"

    lines = []
    for t in trades[:PREDICTION_RECENT_TRADES_SHOWN]:
        action = t["action"]
        side_word = "YES" if action.endswith("yes") else "NO"
        verb = "bought" if action.startswith("buy") else "sold"
        avg_price = (int(t["vwap_x100"]) + 50) // 100  # rounded display
        name = cog._display_name_for(int(t["discord_id"]))
        lines.append(f"  {name} {verb} {t['contracts']} {side_word} @ {avg_price}")
    return "\n".join(lines)


def _format_position_card(pos: dict, book: dict) -> str:
    """Compact position card for the embed (caller's own holdings)."""
    yes_c = int(pos.get("yes_contracts", 0))
    no_c = int(pos.get("no_contracts", 0))
    if yes_c == 0 and no_c == 0:
        return ""
    parts = []
    if yes_c > 0:
        avg = int(pos["yes_cost_basis_total"]) / yes_c
        mark = PredictionService.position_mark(book, "yes")
        upnl = ((mark - avg) * yes_c) if mark is not None else 0
        parts.append(f"YES {yes_c} @ avg {avg:.1f} (mark {mark if mark is not None else '-'}, uPnL {int(upnl):+d})")
    if no_c > 0:
        avg = int(pos["no_cost_basis_total"]) / no_c
        mark = PredictionService.position_mark(book, "no")
        upnl = ((mark - avg) * no_c) if mark is not None else 0
        parts.append(f"NO  {no_c} @ avg {avg:.1f} (mark {mark if mark is not None else '-'}, uPnL {int(upnl):+d})")
    return "Your position: " + " | ".join(parts)


# --------------------------------------------------------------------------- #
# Modals
# --------------------------------------------------------------------------- #

class BuyContractsModal(discord.ui.Modal):
    contracts = discord.ui.TextInput(
        label="Contracts to buy (numbers only)",
        placeholder="e.g. 5",
        min_length=1,
        max_length=6,
        required=True,
    )

    def __init__(
        self,
        cog: PredictionCommands,
        prediction_id: int,
        side: str,
        question: str,
        max_available: int,
        unit_price: int,
        balance: int,
    ):
        side_label = side.upper()
        super().__init__(title=f"Buy {side_label} ({balance} jopa)")
        self.cog = cog
        self.prediction_id = prediction_id
        self.side = side  # 'yes' or 'no'
        self.contracts.placeholder = (
            f"max {max_available} avail @ price {unit_price}"
        )
        # keep question for context if we ever need it in followups
        self._question = question

    async def on_submit(self, interaction: discord.Interaction):
        try:
            qty = int(self.contracts.value)
        except (TypeError, ValueError):
            await interaction.response.send_message(
                "Invalid number of contracts.", ephemeral=True
            )
            return
        if qty <= 0:
            await interaction.response.send_message(
                "Contracts must be positive.", ephemeral=True
            )
            return

        try:
            await interaction.response.defer(ephemeral=True)
            result = await asyncio.to_thread(
                functools.partial(
                    self.cog.prediction_service.buy_contracts,
                    prediction_id=self.prediction_id,
                    discord_id=interaction.user.id,
                    side=self.side,
                    contracts=qty,
                )
            )
        except ValueError as e:
            await interaction.followup.send(f"❌ {e}", ephemeral=True)
            return
        except Exception as e:
            logger.exception(f"Error buying contracts: {e}")
            await interaction.followup.send(
                "An error occurred placing your order.", ephemeral=True
            )
            return

        avg_price = (int(result["vwap_x100"]) + 50) // 100
        await interaction.followup.send(
            f"✅ Bought **{result['contracts']} {self.side.upper()}** @ avg "
            f"price **{avg_price}** — paid **{result['total_cost']}** {JOPACOIN_EMOTE}. "
            f"New balance: **{result['new_balance']}**.",
            ephemeral=True,
        )

        await self.cog.refresh_market_embed(self.prediction_id)


class SellContractsModal(discord.ui.Modal):
    contracts = discord.ui.TextInput(
        label="Contracts to sell (numbers only)",
        placeholder="e.g. 5",
        min_length=1,
        max_length=6,
        required=True,
    )

    def __init__(
        self,
        cog: PredictionCommands,
        prediction_id: int,
        side: str,
        held: int,
        unit_price: int | None,
    ):
        super().__init__(title=f"Sell {side.upper()} ({held} held)")
        self.cog = cog
        self.prediction_id = prediction_id
        self.side = side
        self.contracts.placeholder = (
            f"max {held}, top bid price {unit_price}"
            if unit_price is not None
            else f"max {held}"
        )

    async def on_submit(self, interaction: discord.Interaction):
        try:
            qty = int(self.contracts.value)
        except (TypeError, ValueError):
            await interaction.response.send_message(
                "Invalid number of contracts.", ephemeral=True
            )
            return
        if qty <= 0:
            await interaction.response.send_message(
                "Contracts must be positive.", ephemeral=True
            )
            return

        try:
            await interaction.response.defer(ephemeral=True)
            result = await asyncio.to_thread(
                functools.partial(
                    self.cog.prediction_service.sell_contracts,
                    prediction_id=self.prediction_id,
                    discord_id=interaction.user.id,
                    side=self.side,
                    contracts=qty,
                )
            )
        except ValueError as e:
            await interaction.followup.send(f"❌ {e}", ephemeral=True)
            return
        except Exception as e:
            logger.exception(f"Error selling contracts: {e}")
            await interaction.followup.send(
                "An error occurred selling your contracts.", ephemeral=True
            )
            return

        avg_price = (int(result["vwap_x100"]) + 50) // 100
        await interaction.followup.send(
            f"✅ Sold **{result['contracts']} {self.side.upper()}** @ avg "
            f"price **{avg_price}** — received **{result['total_proceeds']}** "
            f"{JOPACOIN_EMOTE}. New balance: **{result['new_balance']}**.",
            ephemeral=True,
        )

        await self.cog.refresh_market_embed(self.prediction_id)


# --------------------------------------------------------------------------- #
# Persistent buy/buy/position view
# --------------------------------------------------------------------------- #

class PersistentMarketView(discord.ui.View):
    """Buttons: BUY YES, BUY NO, MY POSITION. SELL lives inside MY POSITION."""

    def __init__(self, cog: PredictionCommands):
        super().__init__(timeout=None)
        self.cog = cog

    @discord.ui.button(
        label="BUY YES",
        emoji="🟢",
        style=discord.ButtonStyle.success,
        custom_id="predict:buy:yes",
    )
    async def buy_yes(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._open_buy_modal(interaction, "yes")

    @discord.ui.button(
        label="BUY NO",
        emoji="🔴",
        style=discord.ButtonStyle.danger,
        custom_id="predict:buy:no",
    )
    async def buy_no(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._open_buy_modal(interaction, "no")

    @discord.ui.button(
        label="MY POSITION",
        emoji="📊",
        style=discord.ButtonStyle.secondary,
        custom_id="predict:mypos",
    )
    async def my_position(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._open_position_card(interaction)

    @staticmethod
    def _extract_prediction_id(message: discord.Message) -> int | None:
        if not message.embeds:
            return None
        title = message.embeds[0].title or ""
        m = MARKET_TITLE_RE.search(title)
        return int(m.group(1)) if m else None

    async def _open_buy_modal(self, interaction: discord.Interaction, side: str):
        if not await require_gamba_channel(interaction):
            return

        prediction_id = self._extract_prediction_id(interaction.message)
        if prediction_id is None:
            await interaction.response.send_message(
                "Could not identify market.", ephemeral=True
            )
            return

        view = await asyncio.to_thread(
            self.cog.prediction_service.get_market_view, prediction_id, interaction.user.id
        )
        if not view or view["status"] != "open":
            await interaction.response.send_message(
                "Market is not open for trading.", ephemeral=True
            )
            return

        book = view["book"]
        if side == "yes":
            asks = book["yes_asks"]
            if not asks:
                await interaction.response.send_message(
                    "No asks available right now (book consumed). Wait for next refresh.",
                    ephemeral=True,
                )
                return
            unit_price = asks[0][0]
            max_avail = sum(size for _, size in asks)
        else:
            bids = book["yes_bids"]
            if not bids:
                await interaction.response.send_message(
                    "No NO asks available right now (book consumed). Wait for next refresh.",
                    ephemeral=True,
                )
                return
            unit_price = 100 - bids[0][0]  # NO ask = 100 - top YES bid
            max_avail = sum(size for _, size in bids)

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(
            self.cog.player_service.get_player, interaction.user.id, guild_id
        )
        if not player:
            await interaction.response.send_message(
                "You must be registered to trade. Use `/player register` first.",
                ephemeral=True,
            )
            return

        balance = int(player.jopacoin_balance or 0)
        if balance <= 0:
            await interaction.response.send_message(
                "You have no jopacoins to trade with.", ephemeral=True
            )
            return

        modal = BuyContractsModal(
            cog=self.cog,
            prediction_id=prediction_id,
            side=side,
            question=view.get("question", ""),
            max_available=max_avail,
            unit_price=unit_price,
            balance=balance,
        )
        await interaction.response.send_modal(modal)

    async def _open_position_card(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        prediction_id = self._extract_prediction_id(interaction.message)
        if prediction_id is None:
            await interaction.response.send_message(
                "Could not identify market.", ephemeral=True
            )
            return

        position = await asyncio.to_thread(
            self.cog.prediction_service.get_user_position,
            prediction_id, interaction.user.id,
        )
        if not position or (
            int(position["yes_contracts"]) == 0 and int(position["no_contracts"]) == 0
        ):
            await interaction.response.send_message(
                "You don't hold any contracts in this market.", ephemeral=True
            )
            return

        book = await asyncio.to_thread(
            self.cog.prediction_service.prediction_repo.get_book, prediction_id
        )

        embed = self.cog._build_position_embed(prediction_id, position, book)
        sell_view = PositionEphemeralView(self.cog, prediction_id, position, book)
        await interaction.response.send_message(embed=embed, view=sell_view, ephemeral=True)


class PositionEphemeralView(discord.ui.View):
    """Ephemeral view shown when the user clicks MY POSITION; offers SELL buttons."""

    def __init__(self, cog: PredictionCommands, prediction_id: int, position: dict, book: dict):
        super().__init__(timeout=120)
        self.cog = cog
        self.prediction_id = prediction_id
        yes_c = int(position.get("yes_contracts", 0))
        no_c = int(position.get("no_contracts", 0))
        if yes_c > 0:
            self.add_item(_SellButton(cog, prediction_id, "yes", yes_c, book))
        if no_c > 0:
            self.add_item(_SellButton(cog, prediction_id, "no", no_c, book))


class _SellButton(discord.ui.Button):
    def __init__(self, cog: PredictionCommands, prediction_id: int, side: str, held: int, book: dict):
        if side == "yes":
            unit_price = book["yes_bids"][0][0] if book["yes_bids"] else None
            label = f"SELL YES ({held})"
            style = discord.ButtonStyle.success
        else:
            unit_price = (100 - book["yes_asks"][0][0]) if book["yes_asks"] else None
            label = f"SELL NO ({held})"
            style = discord.ButtonStyle.danger
        super().__init__(label=label, style=style)
        self.cog = cog
        self.prediction_id = prediction_id
        self.side = side
        self.held = held
        self.unit_price = unit_price

    async def callback(self, interaction: discord.Interaction):
        modal = SellContractsModal(
            cog=self.cog,
            prediction_id=self.prediction_id,
            side=self.side,
            held=self.held,
            unit_price=self.unit_price,
        )
        await interaction.response.send_modal(modal)


# --------------------------------------------------------------------------- #
# The cog
# --------------------------------------------------------------------------- #

class PredictionCommands(commands.Cog):
    """Slash commands for the order-book prediction market."""

    predict = app_commands.Group(name="predict", description="Prediction markets")

    def __init__(
        self,
        bot: commands.Bot,
        prediction_service: PredictionService,
        player_service: Any,
    ):
        self.bot = bot
        self.prediction_service = prediction_service
        self.player_service = player_service
        self._name_cache: dict[int, str] = {}

    # -- name caching helper for trade tape ---

    def _display_name_for(self, discord_id: int) -> str:
        name = self._name_cache.get(discord_id)
        if name:
            return name
        user = self.bot.get_user(discord_id)
        if user:
            self._name_cache[discord_id] = user.display_name
            return user.display_name
        return f"<@{discord_id}>"

    # -- embed builders ---

    def _build_market_embed(self, view: dict) -> discord.Embed:
        question = view.get("question", "")
        price = view.get("current_price")
        vol = view.get("volume_since_refresh", 0)
        book = view.get("book", {"yes_asks": [], "yes_bids": [], "current_price": price})
        recent = view.get("recent_trades", [])
        position = view.get("viewer_position")

        embed = discord.Embed(
            title=f"{MARKET_TITLE_PREFIX}{view['prediction_id']}",
            description=f'"{question}"',
            color=0x3498DB,
        )
        embed.add_field(
            name="Current",
            value=(
                f"price **{price if price is not None else '?'}**\n"
                f"vol since refresh: **{vol}**"
            ),
            inline=False,
        )
        # Ladder as 4 inline fields: Buy YES + Buy NO side-by-side, Sell YES + Sell NO below.
        for fname, fvalue, finline in _build_ladder_fields(book):
            embed.add_field(name=fname, value=fvalue, inline=finline)
        embed.add_field(
            name="Recent",
            value=_format_recent_trades(recent, self),
            inline=False,
        )
        if position:
            position_text = _format_position_card(position, book)
            if position_text:
                embed.add_field(name="​", value=position_text, inline=False)
        embed.set_footer(text=f"{PREDICTION_CONTRACT_VALUE} jopa per winning contract")
        return embed

    def _build_resolved_embed(self, view: dict) -> discord.Embed:
        outcome = (view.get("outcome") or "?").upper()
        color = 0x2ECC71 if outcome == "YES" else 0xE74C3C
        embed = discord.Embed(
            title=f"{MARKET_TITLE_PREFIX}{view['prediction_id']}",
            description=f'"{view.get("question", "")}"',
            color=color,
        )
        embed.add_field(name="Outcome", value=f"**{outcome}**", inline=False)
        return embed

    def _build_cancelled_embed(self, view: dict) -> discord.Embed:
        embed = discord.Embed(
            title=f"{MARKET_TITLE_PREFIX}{view['prediction_id']}",
            description=f'"{view.get("question", "")}"',
            color=0x95A5A6,
        )
        embed.add_field(name="Status", value="**CANCELLED** — cost basis refunded.", inline=False)
        return embed

    def _build_position_embed(self, prediction_id: int, position: dict, book: dict) -> discord.Embed:
        embed = discord.Embed(
            title=f"Your position in market #{prediction_id}",
            color=0x3498DB,
        )
        yes_c = int(position.get("yes_contracts", 0))
        no_c = int(position.get("no_contracts", 0))
        if yes_c > 0:
            avg = int(position["yes_cost_basis_total"]) / yes_c
            mark = PredictionService.position_mark(book, "yes")
            upnl = ((mark - avg) * yes_c) if mark is not None else 0
            embed.add_field(
                name="YES",
                value=(
                    f"contracts: **{yes_c}**\n"
                    f"avg cost: {avg:.1f}\n"
                    f"mark: {mark if mark is not None else '-'}\n"
                    f"uPnL: **{int(upnl):+d}**"
                ),
                inline=True,
            )
        if no_c > 0:
            avg = int(position["no_cost_basis_total"]) / no_c
            mark = PredictionService.position_mark(book, "no")
            upnl = ((mark - avg) * no_c) if mark is not None else 0
            embed.add_field(
                name="NO",
                value=(
                    f"contracts: **{no_c}**\n"
                    f"avg cost: {avg:.1f}\n"
                    f"mark: {mark if mark is not None else '-'}\n"
                    f"uPnL: **{int(upnl):+d}**"
                ),
                inline=True,
            )
        return embed

    # -- embed update helpers ---

    async def refresh_market_embed(self, prediction_id: int):
        """Re-render the market embed in its thread after a state change."""
        try:
            view = await asyncio.to_thread(
                self.prediction_service.get_market_view, prediction_id
            )
            if not view:
                return
            thread_id = view.get("thread_id")
            embed_msg_id = view.get("embed_message_id")
            if not thread_id or not embed_msg_id:
                return
            channel = self.bot.get_channel(thread_id) or await self.bot.fetch_channel(thread_id)
            msg = await channel.fetch_message(embed_msg_id)

            if view["status"] == "resolved":
                embed = self._build_resolved_embed(view)
                await msg.edit(embed=embed, view=None)
            elif view["status"] == "cancelled":
                embed = self._build_cancelled_embed(view)
                await msg.edit(embed=embed, view=None)
            else:
                embed = self._build_market_embed(view)
                await msg.edit(embed=embed)
        except discord.NotFound:
            logger.debug("Market embed message vanished for %s", prediction_id)
        except Exception as e:
            logger.warning(f"Failed to refresh market embed: {e}")

    # -- guild gamba channel discovery (for digest / announcements) ---

    def _gamba_channel_for_guild(self, guild: discord.Guild) -> discord.TextChannel | None:
        for ch in guild.text_channels:
            name = (ch.name or "").lower()
            if "gamba" in name:
                return ch
        return None

    async def announce_to_gamba(
        self,
        guild: discord.Guild,
        content: str | None = None,
        embed: discord.Embed | None = None,
    ) -> None:
        if content is None and embed is None:
            return
        ch = self._gamba_channel_for_guild(guild)
        if ch is None:
            return
        try:
            await ch.send(content=content, embed=embed)
        except discord.Forbidden:
            logger.debug("No permission to post in #%s", ch.name)
        except Exception as e:
            logger.warning(f"Failed to announce to gamba channel: {e}")

    # -- /predict create ---

    @predict.command(name="create", description="Create a new prediction market (admin)")
    @app_commands.describe(
        question="The question to predict on",
        initial_fair=f"Starting price (= % implied probability), default {PREDICTION_INITIAL_FAIR_DEFAULT}",
    )
    async def create(
        self,
        interaction: discord.Interaction,
        question: str,
        initial_fair: int = PREDICTION_INITIAL_FAIR_DEFAULT,
    ):
        if not await require_gamba_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can create prediction markets.", ephemeral=True
            )
            return
        if not (PREDICTION_PRICE_LOW <= initial_fair <= PREDICTION_PRICE_HIGH):
            await interaction.response.send_message(
                f"initial_fair must be in [{PREDICTION_PRICE_LOW}, {PREDICTION_PRICE_HIGH}].",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        try:
            result = await asyncio.to_thread(
                functools.partial(
                    self.prediction_service.create_orderbook_prediction,
                    guild_id=guild_id,
                    creator_id=interaction.user.id,
                    question=question,
                    initial_fair=initial_fair,
                    channel_id=interaction.channel_id,
                )
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        prediction_id = result["prediction_id"]
        await safe_followup(
            interaction,
            content=f"✅ Market #{prediction_id} created at price {initial_fair}.",
            ephemeral=True,
        )

        # Channel announcement + thread
        try:
            channel_msg = await interaction.channel.send(
                f"📈 **New market #{prediction_id}** opened by {interaction.user.mention}\n"
                f'"{question}" — starting price {initial_fair}',
            )
            thread_name = f"Market #{prediction_id}: {question[:60]}"
            thread = await channel_msg.create_thread(name=thread_name)

            view_data = await asyncio.to_thread(
                self.prediction_service.get_market_view, prediction_id
            )
            embed = self._build_market_embed(view_data)
            view = PersistentMarketView(self)
            embed_msg = await thread.send(embed=embed, view=view)
            try:
                await embed_msg.pin()
            except discord.Forbidden:
                pass

            await asyncio.to_thread(
                functools.partial(
                    self.prediction_service.update_discord_ids,
                    prediction_id=prediction_id,
                    thread_id=thread.id,
                    embed_message_id=embed_msg.id,
                    channel_message_id=channel_msg.id,
                )
            )
        except Exception as e:
            logger.exception(f"Failed to set up market thread: {e}")

    # -- /predict resolve ---

    @predict.command(name="resolve", description="Resolve a market YES or NO (admin)")
    @app_commands.describe(prediction_id="Market ID", outcome="YES or NO")
    @app_commands.choices(
        outcome=[
            app_commands.Choice(name="YES", value="yes"),
            app_commands.Choice(name="NO", value="no"),
        ]
    )
    async def resolve(
        self,
        interaction: discord.Interaction,
        prediction_id: int,
        outcome: app_commands.Choice[str],
    ):
        if not await require_gamba_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can resolve markets.", ephemeral=True
            )
            return
        if not await safe_defer(interaction):
            return

        try:
            result = await asyncio.to_thread(
                self.prediction_service.resolve_orderbook,
                prediction_id, outcome.value, interaction.user.id,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        winners = result.get("winners", [])
        biggest = max(winners, key=lambda w: w["payout"], default=None)
        biggest_str = (
            f"biggest +{biggest['payout']} to <@{biggest['discord_id']}>"
            if biggest
            else "no winners"
        )
        announce = (
            f"📈 Market #{prediction_id} resolved **{outcome.value.upper()}** — "
            f"{len(winners)} winners, {biggest_str}."
        )
        await safe_followup(interaction, content=announce)

        # Update market embed + archive thread
        pred = await asyncio.to_thread(
            self.prediction_service.get_prediction, prediction_id
        )
        if pred and pred.get("thread_id"):
            try:
                thread = self.bot.get_channel(pred["thread_id"]) or await self.bot.fetch_channel(pred["thread_id"])
                await thread.send(announce)
                await self.refresh_market_embed(prediction_id)
                try:
                    await thread.edit(locked=True, archived=True)
                except discord.Forbidden:
                    await thread.edit(archived=True)
            except Exception as e:
                logger.warning(f"Failed to archive market thread: {e}")

        # Gamba channel announcement
        if interaction.guild:
            await self.announce_to_gamba(interaction.guild, announce)

    # -- /predict set_fair ---

    @predict.command(
        name="set_fair",
        description="Manually override a market's fair price (admin)",
    )
    @app_commands.describe(
        prediction_id="Market ID",
        new_price=f"New fair price, integer in [{PREDICTION_PRICE_LOW}, {PREDICTION_PRICE_HIGH}]",
    )
    async def set_fair(
        self, interaction: discord.Interaction, prediction_id: int, new_price: int,
    ):
        if not await require_gamba_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can override the fair price.", ephemeral=True
            )
            return
        if not await safe_defer(interaction):
            return

        try:
            result = await asyncio.to_thread(
                self.prediction_service.set_fair_manual, prediction_id, new_price,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        announce = (
            f"📈 Market #{prediction_id} fair manually set by <@{interaction.user.id}>: "
            f"{result['old_price']} → {result['new_price']}"
        )
        await safe_followup(interaction, content=announce)

        pred = await asyncio.to_thread(
            self.prediction_service.get_prediction, prediction_id
        )
        if pred and pred.get("thread_id"):
            try:
                thread = self.bot.get_channel(pred["thread_id"]) or await self.bot.fetch_channel(pred["thread_id"])
                await thread.send(announce)
            except Exception as e:
                logger.warning(f"Failed to post set_fair announcement: {e}")
        await self.refresh_market_embed(prediction_id)

    # -- /predict cancel ---

    @predict.command(name="cancel", description="Cancel a market and refund cost basis (admin)")
    @app_commands.describe(prediction_id="Market ID to cancel")
    async def cancel(self, interaction: discord.Interaction, prediction_id: int):
        if not await require_gamba_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can cancel markets.", ephemeral=True
            )
            return
        if not await safe_defer(interaction):
            return

        try:
            result = await asyncio.to_thread(
                self.prediction_service.cancel_orderbook, prediction_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        announce = (
            f"🚫 Market #{prediction_id} cancelled by <@{interaction.user.id}>. "
            f"Refunded {result['total_refunded']} {JOPACOIN_EMOTE} across "
            f"{len(result['refunded'])} holder(s)."
        )
        await safe_followup(interaction, content=announce)

        pred = await asyncio.to_thread(
            self.prediction_service.get_prediction, prediction_id
        )
        if pred and pred.get("thread_id"):
            try:
                thread = self.bot.get_channel(pred["thread_id"]) or await self.bot.fetch_channel(pred["thread_id"])
                await thread.send(announce)
                await self.refresh_market_embed(prediction_id)
                try:
                    await thread.edit(locked=True, archived=True)
                except discord.Forbidden:
                    await thread.edit(archived=True)
            except Exception as e:
                logger.warning(f"Failed to archive cancelled thread: {e}")

        if interaction.guild:
            await self.announce_to_gamba(interaction.guild, announce)

    # -- /predict list ---

    @predict.command(name="list", description="List open markets (or all with show_all)")
    @app_commands.describe(
        show_all="Include resolved/cancelled markets",
    )
    async def list_markets(
        self,
        interaction: discord.Interaction,
        show_all: bool = False,
    ):
        if not await require_gamba_channel(interaction):
            return
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        open_preds = await asyncio.to_thread(
            self.prediction_service.list_open_orderbook_markets, guild_id
        )
        # Recently active first; quiet markets sink to the bottom.
        open_preds.sort(key=lambda p: p.get("volume_recent", 0) or 0, reverse=True)

        if not open_preds and not show_all:
            await safe_followup(
                interaction,
                content=(
                    "No open markets right now. "
                    "Admins can create one with `/predict create <question> [initial_fair=50]`."
                ),
            )
            return

        resolved_preds: list[dict] = []
        cancelled_preds: list[dict] = []
        if show_all:
            resolved_preds = await asyncio.to_thread(
                self.prediction_service.get_predictions_by_status, guild_id, "resolved"
            )
            cancelled_preds = await asyncio.to_thread(
                self.prediction_service.get_predictions_by_status, guild_id, "cancelled"
            )

        if show_all:
            title = (
                f"📈 Markets ({len(open_preds)} open · "
                f"{len(resolved_preds)} resolved · {len(cancelled_preds)} cancelled)"
            )
        else:
            title = f"📈 Open markets ({len(open_preds)})"

        embed = discord.Embed(title=title, color=0x3498DB)

        # Discord caps an embed at 25 fields. Reserve one slot for resolved/cancelled
        # entries when show_all is on; otherwise use the full 25 for open markets.
        FIELD_CAP = 25
        open_slot_cap = FIELD_CAP - 1 if show_all else FIELD_CAP
        added = 0
        skipped_open = 0
        for p in open_preds:
            if added >= open_slot_cap:
                skipped_open = len(open_preds) - added
                break
            name, value = _format_market_field(p, with_delta=False)
            embed.add_field(name=name, value=value, inline=False)
            added += 1

        if show_all:
            for p in resolved_preds[:10]:
                if added >= FIELD_CAP:
                    break
                outcome = (p.get("outcome") or "?").upper()
                question = (p.get("question") or "")[:200]
                embed.add_field(
                    name=f"📈 #{p['prediction_id']}  ·  RESOLVED {outcome}",
                    value=f'"{question}"',
                    inline=False,
                )
                added += 1
            for p in cancelled_preds[:10]:
                if added >= FIELD_CAP:
                    break
                question = (p.get("question") or "")[:200]
                embed.add_field(
                    name=f"📈 #{p['prediction_id']}  ·  CANCELLED",
                    value=f'"{question}"',
                    inline=False,
                )
                added += 1

        footer_bits = ["/predict view <id> for the full ladder", "/predict help for how it works"]
        if skipped_open:
            footer_bits.insert(0, f"+{skipped_open} more open markets not shown")
        embed.set_footer(text="  ·  ".join(footer_bits))

        await safe_followup(interaction, embed=embed)

    # -- /predict view ---

    @predict.command(name="view", description="Show a market's embed (price, ladder, recent trades)")
    @app_commands.describe(prediction_id="Market ID")
    async def view(self, interaction: discord.Interaction, prediction_id: int):
        if not await require_gamba_channel(interaction):
            return
        if not await safe_defer(interaction):
            return

        view_data = await asyncio.to_thread(
            self.prediction_service.get_market_view,
            prediction_id, interaction.user.id,
        )
        if not view_data:
            await safe_followup(interaction, content="❌ Market not found.")
            return

        if view_data["status"] == "resolved":
            embed = self._build_resolved_embed(view_data)
        elif view_data["status"] == "cancelled":
            embed = self._build_cancelled_embed(view_data)
        else:
            embed = self._build_market_embed(view_data)
        await safe_followup(interaction, embed=embed)

    # -- /predict help ---

    @predict.command(
        name="help",
        description="Explainer: how prediction markets work in this bot",
    )
    @app_commands.checks.cooldown(1, 10)
    async def help_cmd(self, interaction: discord.Interaction):
        # Intentionally NOT gamba-channel gated: a help command should work
        # anywhere a user can type a slash command. Charging someone a JC for
        # asking how to play would be a hostile UX.
        if not await safe_defer(interaction, ephemeral=True):
            return

        body = (
            "**How prediction markets work here**\n\n"
            "Each market is a YES/NO question. Each *contract* pays "
            f"**{PREDICTION_CONTRACT_VALUE} jopa** if your side wins, **0** if it loses.\n\n"
            "**Price = probability.** A YES priced at 17 means the market thinks "
            "YES has a 17% chance. NO priced at 84 means NO has ~84%. They sum to ~100 "
            "(plus a tiny LP spread).\n\n"
            "**Buying** (open a position):\n"
            "• Buy YES at the YES price → win 100 if YES, 0 if NO.\n"
            "• Buy NO at the NO price → win 100 if NO, 0 if YES.\n\n"
            "**Example.** \"Will Luke hit immortal?\" market price 17 (~17% YES).\n"
            "• Buy YES @ 18 → if Luke makes it, +82. If not, −18.\n"
            "• Buy NO @ 84 → if Luke fails, +16. If he makes it, −84.\n\n"
            "**Selling** (close a position): you can sell back any time before the market "
            "resolves, at the current sell price. Cuts losses or banks profits early.\n\n"
            "**The bot is your counterparty** for every trade (call it the Cama central bank). "
            "It posts a small ladder of prices each day; trades sweep top-of-book first. "
            "Price drifts daily based on order flow + a small random walk. If one side gets "
            "fully bought out, the price fades toward that side on the next refresh.\n\n"
            "**Admins** create markets and resolve them. Anyone can trade. "
            "Use `/predict list` for open markets, `/predict view <id>` for a specific market, "
            "and `/predict mine` for your positions."
        )

        embed = discord.Embed(
            title="📈 Prediction markets — quick guide",
            description=body,
            color=0x3498DB,
        )
        await safe_followup(interaction, embed=embed, ephemeral=True)

    # -- /predict mine ---

    @predict.command(name="mine", description="Your open positions across markets")
    async def mine(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        positions = await asyncio.to_thread(
            self.prediction_service.get_user_open_positions,
            interaction.user.id, guild_id,
        )
        if not positions:
            await safe_followup(interaction, content="You don't hold any open positions.")
            return

        # Build a code-block table per (market, side) row
        rows = []
        rows.append(f"{'Market':<28}{'Side':<5}{'Qty':>5}{'Cost':>7}{'Mark':>6}{'uPnL':>7}")
        rows.append("-" * 58)
        total_cost = 0
        total_mark = 0
        for p in positions:
            book = await asyncio.to_thread(
                self.prediction_service.prediction_repo.get_book, p["prediction_id"]
            )
            question = (p["question"] or "")[:26]
            for side in ("yes", "no"):
                qty = int(p[f"{side}_contracts"])
                if qty == 0:
                    continue
                basis = int(p[f"{side}_cost_basis_total"])
                avg = basis / qty
                mark_p = PredictionService.position_mark(book, side)
                mark_value = (mark_p * qty) if mark_p is not None else 0
                upnl = mark_value - basis
                total_cost += basis
                total_mark += mark_value
                rows.append(
                    f"{question:<28}{side.upper():<5}{qty:>5}{avg:>7.1f}"
                    f"{(mark_p if mark_p is not None else 0):>6}{upnl:>+7d}"
                )

        body = "```\n" + "\n".join(rows) + "\n```"
        total_pnl = total_mark - total_cost
        embed = discord.Embed(
            title="📈 Your open positions",
            description=body,
            color=0x3498DB,
        )
        embed.set_footer(
            text=(
                f"Total cost: {total_cost} | mark {total_mark} | uPnL {total_pnl:+d}"
            )
        )
        await safe_followup(interaction, embed=embed)


# --------------------------------------------------------------------------- #
# Cog setup
# --------------------------------------------------------------------------- #

async def setup(bot: commands.Bot):
    prediction_service = getattr(bot, "prediction_service", None)
    if prediction_service is None:
        raise RuntimeError("Prediction service not registered on bot.")
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    cog = PredictionCommands(bot, prediction_service, player_service)
    await bot.add_cog(cog)
    bot.add_view(PersistentMarketView(cog))
