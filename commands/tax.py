"""Tax Man economy audit commands."""

from __future__ import annotations

import asyncio
import logging

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from services.permissions import has_tax_man_permission
from services.tax_service import TaxService
from utils.embed_safety import EMBED_LIMITS, add_lines_field, truncate_field
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.tax")

LEDGER_DEFAULT_LIMIT = 10
LEDGER_MAX_LIMIT = 25
LEDGER_MAX_PAGE = 10_000
LEDGER_VIEW_TIMEOUT_SECONDS = 300


class TaxLedgerView(discord.ui.View):
    """Ephemeral pagination for Tax Man central ledger audits."""

    def __init__(
        self,
        *,
        tax_service: TaxService,
        guild_id: int,
        requester_id: int,
        user: discord.User | None,
        limit: int,
        current_page: int,
        total_entries: int,
    ):
        super().__init__(timeout=LEDGER_VIEW_TIMEOUT_SECONDS)
        self.tax_service = tax_service
        self.guild_id = guild_id
        self.requester_id = requester_id
        self.user = user
        self.limit = limit
        self.total_entries = total_entries
        self.current_page = _clamp_ledger_page(current_page, limit, total_entries)
        self.total_pages = _ledger_total_pages(total_entries, limit)
        self._sync_buttons()

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id == self.requester_id:
            return True
        await interaction.response.send_message(
            "This ledger view belongs to another Tax Man.",
            ephemeral=True,
        )
        return False

    async def _load_page(self, page: int) -> discord.Embed:
        self.total_entries = await asyncio.to_thread(
            self.tax_service.count_ledger_entries,
            self.guild_id,
            user_id=self.user.id if self.user else None,
        )
        self.total_pages = _ledger_total_pages(self.total_entries, self.limit)
        self.current_page = _clamp_ledger_page(page, self.limit, self.total_entries)
        rows = await asyncio.to_thread(
            self.tax_service.get_recent_ledger,
            self.guild_id,
            limit=self.limit,
            offset=_ledger_offset(self.current_page, self.limit),
            user_id=self.user.id if self.user else None,
        )
        self._sync_buttons()
        return _build_ledger_embed(
            rows,
            user=self.user,
            page=self.current_page,
            total_entries=self.total_entries,
            limit=self.limit,
        )

    async def _show_page(
        self,
        interaction: discord.Interaction,
        page: int,
    ) -> None:
        try:
            embed = await self._load_page(page)
        except Exception:
            logger.exception(
                "Failed to paginate tax ledger for guild_id=%s",
                self.guild_id,
            )
            await interaction.response.send_message(
                "Couldn't load that ledger page right now.",
                ephemeral=True,
            )
            return
        await interaction.response.edit_message(embed=embed, view=self)

    def _sync_buttons(self) -> None:
        self.previous_page.disabled = self.current_page <= 1
        self.next_page.disabled = self.current_page >= self.total_pages

    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary)
    async def previous_page(
        self,
        interaction: discord.Interaction,
        button: discord.ui.Button,
    ):
        await self._show_page(interaction, self.current_page - 1)

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next_page(
        self,
        interaction: discord.Interaction,
        button: discord.ui.Button,
    ):
        await self._show_page(interaction, self.current_page + 1)


class TaxCommands(commands.Cog):
    tax = app_commands.Group(name="tax", description="Tax Man economy audit")

    def __init__(self, bot: commands.Bot, tax_service: TaxService):
        self.bot = bot
        self.tax_service = tax_service

    async def _require_tax_man(self, interaction: discord.Interaction) -> bool:
        if has_tax_man_permission(interaction):
            return True
        await interaction.response.send_message(
            "Only Tax Men can use this command.",
            ephemeral=True,
        )
        return False

    @tax.command(name="audit", description="View guild-wide monetary exposure")
    @require_guild
    async def audit(self, interaction: discord.Interaction):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            snapshot, source_totals = await asyncio.gather(
                asyncio.to_thread(self.tax_service.get_guild_snapshot, guild_id),
                asyncio.to_thread(self.tax_service.get_source_totals, guild_id, limit=8),
            )
        except Exception:
            logger.exception("Failed to build tax audit for guild_id=%s", guild_id)
            await safe_followup(
                interaction,
                content="Couldn't load the Tax Man audit right now.",
                ephemeral=True,
            )
            return

        embed = _build_audit_embed(snapshot, source_totals)
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @tax.command(name="player", description="View one player's full monetary exposure")
    @app_commands.describe(user="Player to audit")
    @require_guild
    async def player(self, interaction: discord.Interaction, user: discord.User):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            snapshot = await asyncio.to_thread(
                self.tax_service.get_player_snapshot,
                user.id,
                guild_id,
            )
        except ValueError as exc:
            await safe_followup(
                interaction,
                content=_format_tax_error(str(exc)),
                ephemeral=True,
            )
            return
        except Exception:
            logger.exception(
                "Failed to build tax player audit guild_id=%s user_id=%s",
                guild_id,
                user.id,
            )
            await safe_followup(
                interaction,
                content="Couldn't load that Tax Man player audit right now.",
                ephemeral=True,
            )
            return

        embed = _build_player_embed(user, snapshot, self.tax_service)
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @tax.command(name="ledger", description="View recent central ledger entries")
    @app_commands.describe(
        user="Limit to one player",
        limit="Number of ledger entries to show",
        page="Ledger page to show",
    )
    @require_guild
    async def ledger(
        self,
        interaction: discord.Interaction,
        user: discord.User | None = None,
        limit: app_commands.Range[int, 1, LEDGER_MAX_LIMIT] = LEDGER_DEFAULT_LIMIT,
        page: app_commands.Range[int, 1, LEDGER_MAX_PAGE] = 1,
    ):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            total_entries = await asyncio.to_thread(
                self.tax_service.count_ledger_entries,
                guild_id,
                user_id=user.id if user else None,
            )
            page = _clamp_ledger_page(page, limit, total_entries)
            rows = await asyncio.to_thread(
                self.tax_service.get_recent_ledger,
                guild_id,
                limit=limit,
                offset=_ledger_offset(page, limit),
                user_id=user.id if user else None,
            )
        except Exception:
            logger.exception("Failed to load tax ledger for guild_id=%s", guild_id)
            await safe_followup(
                interaction,
                content="Couldn't load the central ledger right now.",
                ephemeral=True,
            )
            return

        embed = _build_ledger_embed(
            rows,
            user=user,
            page=page,
            total_entries=total_entries,
            limit=limit,
        )
        view = None
        if _ledger_total_pages(total_entries, limit) > 1:
            view = TaxLedgerView(
                tax_service=self.tax_service,
                guild_id=guild_id,
                requester_id=interaction.user.id,
                user=user,
                limit=limit,
                current_page=page,
                total_entries=total_entries,
            )
        await safe_followup(interaction, embed=embed, view=view, ephemeral=True)


def _ledger_total_pages(total_entries: int, limit: int) -> int:
    limit = max(1, int(limit))
    total_entries = max(0, int(total_entries))
    return max(1, (total_entries + limit - 1) // limit)


def _clamp_ledger_page(page: int, limit: int, total_entries: int) -> int:
    total_pages = _ledger_total_pages(total_entries, limit)
    return min(max(1, int(page)), total_pages)


def _ledger_offset(page: int, limit: int) -> int:
    return (max(1, int(page)) - 1) * max(1, int(limit))


def _ledger_footer_text(page: int, total_entries: int, limit: int) -> str:
    page = _clamp_ledger_page(page, limit, total_entries)
    total_pages = _ledger_total_pages(total_entries, limit)
    if total_entries <= 0:
        return "Tax Man ledger | Page 1/1 | 0 entries"
    first_entry = _ledger_offset(page, limit) + 1
    last_entry = min(int(total_entries), page * int(limit))
    return (
        f"Tax Man ledger | Page {page}/{total_pages} | "
        f"Entries {first_entry}-{last_entry} of {int(total_entries):,}"
    )


def _format_jc(amount: int) -> str:
    return f"{amount:,} {JOPACOIN_EMOTE}"


def _format_signed_jc(amount: int) -> str:
    prefix = "+" if amount > 0 else ""
    return f"{prefix}{amount:,} {JOPACOIN_EMOTE}"


def _format_tax_error(error: str) -> str:
    if error == "target_not_registered":
        return "That user is not registered in this guild."
    return "That player audit could not be loaded."


def _build_audit_embed(
    snapshot: dict,
    source_totals: list[dict],
) -> discord.Embed:
    embed = discord.Embed(
        title="Tax Man Guild Audit",
        color=discord.Color.gold(),
    )
    embed.add_field(
        name="Balances",
        value=(
            f"Players: {snapshot['players']:,}\n"
            f"Total balance: {_format_jc(snapshot['total_balance'])}\n"
            f"Positive balance: {_format_jc(snapshot['positive_balance'])}\n"
            f"Visible debt: {_format_jc(snapshot['visible_debt'])}\n"
            f"Broke players: {snapshot['broke_players']:,}"
        ),
        inline=True,
    )
    embed.add_field(
        name="Nonprofit",
        value=(
            f"Available: {_format_jc(snapshot['nonprofit_available'])}\n"
            f"Reserved: {_format_jc(snapshot['nonprofit_reserved'])}"
        ),
        inline=True,
    )
    embed.add_field(
        name="Obligations",
        value=(
            f"Loans: {_format_jc(snapshot['loan_principal'] + snapshot['loan_fee'])} "
            f"({snapshot['loan_borrowers']:,} borrowers)\n"
            f"Dark Bargains: {_format_jc(snapshot['dark_bargain_due'])} "
            f"({snapshot['dark_bargain_count']:,} active)\n"
            f"Pending bets: {_format_jc(snapshot['pending_bet_effective_stake'])} "
            f"({snapshot['pending_bet_count']:,} bets)\n"
            f"Open markets: {snapshot['open_prediction_markets']:,}"
        ),
        inline=False,
    )
    embed.add_field(
        name="Prediction Markets",
        value=_format_prediction_summary(snapshot["prediction_exposure"]["summary"]),
        inline=False,
    )
    embed.add_field(
        name="Prediction Detail",
        value=_format_prediction_markets(snapshot["prediction_exposure"]["markets"]),
        inline=False,
    )
    embed.add_field(
        name="Ledger Sources",
        value=_format_source_totals(source_totals),
        inline=False,
    )
    embed.set_footer(text="Tax Man audit only")
    return embed


def _build_player_embed(
    user: discord.User,
    snapshot: dict,
    tax_service: TaxService,
) -> discord.Embed:
    embed = discord.Embed(
        title=f"Tax Man Player Audit - {getattr(user, 'display_name', user.name)}",
        color=discord.Color.dark_gold(),
    )
    embed.add_field(
        name="Balance",
        value=(
            f"Current: {_format_jc(snapshot['balance'])}\n"
            f"Visible debt: {_format_jc(snapshot['visible_debt'])}"
        ),
        inline=True,
    )
    embed.add_field(
        name="Loans",
        value=(
            f"Principal: {_format_jc(snapshot['loan_principal'])}\n"
            f"Fee: {_format_jc(snapshot['loan_fee'])}\n"
            f"Total owed: {_format_jc(snapshot['loan_total'])}\n"
            f"Taken: {snapshot['total_loans_taken']:,}"
        ),
        inline=True,
    )
    embed.add_field(
        name="Duration Effects",
        value=(
            f"Bankruptcies: {snapshot['bankruptcy_count']:,}\n"
            f"Penalty games: {snapshot['penalty_games_remaining']:,}\n"
            f"Dark Bargains: {snapshot['dark_bargain_count']:,}\n"
            f"Dark Bargain due: {_format_jc(snapshot['dark_bargain_due'])}"
        ),
        inline=True,
    )
    embed.add_field(
        name="Total Obligations",
        value=_format_jc(snapshot["effective_obligations"]),
        inline=False,
    )
    embed.add_field(
        name="Prediction Positions",
        value=_format_prediction_positions(snapshot["prediction_exposure"]),
        inline=False,
    )
    add_lines_field(
        embed,
        "Recent Ledger",
        _format_ledger_lines(snapshot["recent_ledger"]),
        inline=False,
    )
    embed.set_footer(text="Tax Man audit only")
    return embed


def _build_ledger_embed(
    rows: list[dict],
    *,
    user: discord.User | None,
    page: int = 1,
    total_entries: int | None = None,
    limit: int = LEDGER_DEFAULT_LIMIT,
) -> discord.Embed:
    title = "Central Economy Ledger"
    if user is not None:
        title = f"Central Economy Ledger - {getattr(user, 'display_name', user.name)}"
    embed = discord.Embed(title=title, color=discord.Color.blurple())
    description = _format_ledger_rows(rows)
    if not rows and total_entries:
        description = "No ledger entries on this page."
    embed.description = truncate_field(
        description,
        max_len=EMBED_LIMITS["description"],
    )
    if total_entries is not None:
        embed.set_footer(text=_ledger_footer_text(page, total_entries, limit))
    return embed


def _short_question(question: str, *, limit: int = 48) -> str:
    text = " ".join((question or "").split())
    if len(text) <= limit:
        return text
    return f"{text[: limit - 1]}..."


def _format_prediction_summary(summary: dict) -> str:
    return (
        f"Markets: {summary['open_markets']:,}\n"
        f"Holders: {summary['holder_count']:,}\n"
        f"Cost basis: {_format_jc(summary['cost_basis'])}\n"
        f"Expected payout: {_format_jc(summary['expected_payout'])}\n"
        f"EV to holders: {_format_signed_jc(summary['ev_to_holders'])}\n"
        f"Worst-case payout: {_format_jc(summary['worst_case_payout'])}\n"
        f"Book depth: {summary['book_contracts']:,} contracts"
    )


def _format_prediction_markets(markets: list[dict]) -> str:
    if not markets:
        return "No open prediction markets."
    lines = []
    for market in markets[:5]:
        ask = market["top_yes_ask"] if market["top_yes_ask"] is not None else "-"
        bid = market["top_yes_bid"] if market["top_yes_bid"] is not None else "-"
        lines.append(
            f"#{market['prediction_id']} `{market['current_price']}%` "
            f"{_short_question(market['question'])}\n"
            f"  YES {market['yes_contracts']:,} / NO {market['no_contracts']:,}, "
            f"cost {_format_jc(market['cost_basis'])}, "
            f"EV {_format_signed_jc(market['ev_to_holders'])}, "
            f"worst {_format_jc(market['worst_case_payout'])}, "
            f"book {bid}/{ask}"
        )
    return "\n".join(lines)


def _format_prediction_positions(exposure: dict) -> str:
    summary = exposure["summary"]
    positions = exposure["positions"]
    if not positions:
        return "No open prediction positions."

    lines = [
        (
            f"Cost basis: {_format_jc(summary['cost_basis'])} | "
            f"Expected payout: {_format_jc(summary['expected_payout'])} | "
            f"EV: {_format_signed_jc(summary['ev'])} | "
            f"Max payout: {_format_jc(summary['max_payout'])}"
        )
    ]
    for pos in positions[:5]:
        lines.append(
            f"#{pos['prediction_id']} `{pos['current_price']}%` "
            f"{_short_question(pos['question'])}\n"
            f"  YES {pos['yes_contracts']:,} "
            f"({_format_jc(pos['yes_cost_basis'])}) / "
            f"NO {pos['no_contracts']:,} "
            f"({_format_jc(pos['no_cost_basis'])}); "
            f"EV {_format_signed_jc(pos['ev'])}"
        )
    return "\n".join(lines)


def _format_source_totals(rows: list[dict]) -> str:
    if not rows:
        return "No ledger entries yet."
    lines = []
    for row in rows[:8]:
        lines.append(
            f"`{row['source']}`: {row['entry_count']:,} entries, "
            f"net {_format_signed_jc(int(row['net_delta']))}"
        )
    return "\n".join(lines)


def _format_ledger_detail(row: dict) -> str:
    reason = " ".join(str(row.get("reason") or "").split())
    if reason:
        return reason

    source = str(row.get("source") or "balance_update")
    source_labels = {
        "balance_update": "balance adjustment",
        "player_insert": "registration starting balance",
        "nonprofit_insert": "nonprofit fund created",
        "nonprofit_update": "nonprofit fund update",
        "ledger_backfill": "opening balance backfill",
        "dig": "dig balance change",
        "gamba": "gamba wheel balance change",
    }
    label = source_labels.get(source, source.replace("_", " "))

    related_type = " ".join(str(row.get("related_type") or "").split())
    related_id = " ".join(str(row.get("related_id") or "").split())
    if related_type and related_id:
        return f"{label} ({related_type} #{related_id})"
    if related_type:
        return f"{label} ({related_type})"
    return label


def _format_ledger_lines(rows: list[dict], *, limit: int = 25) -> list[str]:
    if not rows:
        return ["No ledger entries yet."]
    lines = []
    for row in rows[:limit]:
        account = (
            "nonprofit"
            if row["account_type"] == "nonprofit"
            else f"<@{row['account_id']}>"
        )
        lines.append(
            f"<t:{int(row['created_at'])}:R> - {account}: "
            f"{_format_signed_jc(int(row['delta']))} - {_format_ledger_detail(row)} "
            f"-> {_format_jc(int(row['balance_after']))}"
        )
    return lines


def _format_ledger_rows(rows: list[dict]) -> str:
    return "\n".join(_format_ledger_lines(rows))


async def setup(bot: commands.Bot):
    tax_service = getattr(bot, "tax_service", None)
    if tax_service is None:
        raise RuntimeError("Tax service not registered on bot.")
    await bot.add_cog(TaxCommands(bot, tax_service))
