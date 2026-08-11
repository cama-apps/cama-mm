"""Tax Man economy audit commands."""

from __future__ import annotations

import asyncio
import logging
import math

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from config import (
    BANKRUPTCY_PENALTY_GAMES,
    MINIGAME_JC_DELTA_SCALE,
    VANITY_TAX_RATE,
)
from services import trivia_data
from services.permissions import has_admin_permission, has_tax_man_permission
from services.tax_service import TaxService
from utils.economy_event_display import build_public_economy_event_embed
from utils.embed_safety import EMBED_LIMITS, add_lines_field, truncate_field
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.tax")

LEDGER_DEFAULT_LIMIT = 10
LEDGER_MAX_LIMIT = 25
LEDGER_MAX_PAGE = 10_000
LEDGER_VIEW_TIMEOUT_SECONDS = 300
PLAYER_LEDGER_DEFAULT_LIMIT = 8


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
        # Compute into locals and only commit the page state once every query
        # has succeeded, so a failed load leaves the view on its current page.
        total_entries = await asyncio.to_thread(
            self.tax_service.count_ledger_entries,
            self.guild_id,
            user_id=self.user.id if self.user else None,
        )
        current_page = _clamp_ledger_page(page, self.limit, total_entries)
        rows = await asyncio.to_thread(
            self.tax_service.get_recent_ledger,
            self.guild_id,
            limit=self.limit,
            offset=_ledger_offset(current_page, self.limit),
            user_id=self.user.id if self.user else None,
        )
        self.total_entries = total_entries
        self.total_pages = _ledger_total_pages(total_entries, self.limit)
        self.current_page = current_page
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
        # Acknowledge within Discord's 3-second component window before the
        # ledger queries run.
        await interaction.response.defer()
        try:
            embed = await self._load_page(page)
        except Exception:
            logger.exception(
                "Failed to paginate tax ledger for guild_id=%s",
                self.guild_id,
            )
            await interaction.followup.send(
                "Couldn't load that ledger page right now.",
                ephemeral=True,
            )
            return
        await interaction.edit_original_response(embed=embed, view=self)

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


class TaxPlayerLedgerView(discord.ui.View):
    """Ephemeral pagination for the recent ledger slice in player audits."""

    def __init__(
        self,
        *,
        tax_service: TaxService,
        guild_id: int,
        requester_id: int,
        user: discord.User,
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
            "This player audit view belongs to another Tax Man.",
            ephemeral=True,
        )
        return False

    async def _load_page(self, page: int) -> discord.Embed:
        # Compute into locals and only commit the page state once every query
        # has succeeded, so a failed load leaves the view on its current page.
        total_entries = await asyncio.to_thread(
            self.tax_service.count_ledger_entries,
            self.guild_id,
            user_id=self.user.id,
        )
        current_page = _clamp_ledger_page(page, self.limit, total_entries)
        snapshot = await asyncio.to_thread(
            self.tax_service.get_player_snapshot,
            self.user.id,
            self.guild_id,
            ledger_limit=self.limit,
            ledger_offset=_ledger_offset(current_page, self.limit),
        )
        self.total_entries = total_entries
        self.total_pages = _ledger_total_pages(total_entries, self.limit)
        self.current_page = current_page
        self._sync_buttons()
        return _build_player_embed(
            self.user,
            snapshot,
            ledger_page=self.current_page,
            total_ledger_entries=self.total_entries,
            ledger_limit=self.limit,
        )

    async def _show_page(
        self,
        interaction: discord.Interaction,
        page: int,
    ) -> None:
        # Acknowledge within Discord's 3-second component window before the
        # snapshot queries run.
        await interaction.response.defer()
        try:
            embed = await self._load_page(page)
        except ValueError as exc:
            await interaction.followup.send(
                _format_tax_error(str(exc)),
                ephemeral=True,
            )
            return
        except Exception:
            logger.exception(
                "Failed to paginate tax player audit guild_id=%s user_id=%s",
                self.guild_id,
                self.user.id,
            )
            await interaction.followup.send(
                "Couldn't load that player ledger page right now.",
                ephemeral=True,
            )
            return
        await interaction.edit_original_response(embed=embed, view=self)

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
    tax = app_commands.Group(
        name="tax",
        description="Jopacoin economy and Tax Man tools",
    )

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

    @tax.command(name="policy", description="View daily Jopacoin monetary policy")
    @require_guild
    async def policy(self, interaction: discord.Interaction):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)
        service = getattr(self.bot, "economy_event_service", None)
        if service is None:
            await safe_followup(
                interaction,
                content="The daily economy controller is not configured.",
                ephemeral=True,
            )
            return
        try:
            status = await asyncio.to_thread(
                service.get_policy_status, interaction.guild.id
            )
        except Exception:
            logger.exception(
                "Failed to load economy policy for guild_id=%s", interaction.guild.id
            )
            await safe_followup(
                interaction,
                content="Couldn't load the monetary-policy status right now.",
                ephemeral=True,
            )
            return
        await safe_followup(
            interaction,
            embed=_build_policy_embed(status),
            ephemeral=True,
        )

    @tax.command(
        name="event",
        description="Reveal today's server-wide Jopacoin event",
    )
    @require_guild
    async def event(self, interaction: discord.Interaction):
        """Public, player-facing view of the current monetary spell."""
        await safe_defer(interaction, ephemeral=False)
        service = getattr(self.bot, "economy_event_service", None)
        if service is None:
            await safe_followup(
                interaction,
                content="The daily economy controller is not configured.",
                ephemeral=False,
            )
            return

        try:
            status = await asyncio.to_thread(
                service.get_policy_status,
                interaction.guild.id,
            )
        except Exception:
            logger.exception(
                "Failed to load public economy event for guild_id=%s",
                interaction.guild.id,
            )
            await safe_followup(
                interaction,
                content="The current economic edict is obscured. Try again shortly.",
                ephemeral=False,
            )
            return

        icon_url = None
        event = status.get("event")
        if event and event.get("name"):
            try:
                icon_url = await asyncio.to_thread(
                    trivia_data.get_ability_icon_url_by_name,
                    event["name"],
                )
            except Exception:  # noqa: BLE001
                logger.warning(
                    "Public economy event icon lookup failed for event=%s guild=%s",
                    event["name"],
                    interaction.guild.id,
                    exc_info=True,
                )

        await safe_followup(
            interaction,
            embed=_build_public_event_embed(status, icon_url=icon_url),
            ephemeral=False,
        )

    @tax.command(
        name="vanity",
        # Static command metadata can't read the live service, so derive the
        # advertised rate from config at import time.
        description=(
            f"Check the nickname vanity tax ({VANITY_TAX_RATE:.0%} of profits)"
        ),
    )
    @app_commands.describe(
        user="Player to check (defaults to you)",
        taxable="ADMIN_USER_IDS only: force tax or restore the nickname rule",
    )
    @require_guild
    async def vanity(
        self,
        interaction: discord.Interaction,
        user: discord.User | None = None,
        taxable: bool | None = None,
    ):
        target = user or interaction.user
        guild_id = interaction.guild.id
        vanity_service = getattr(self.bot, "vanity_tax_service", None)
        if vanity_service is None:
            await interaction.response.send_message(
                "Vanity tax is not active.", ephemeral=True
            )
            return
        if taxable is not None:
            if not has_admin_permission(interaction):
                await interaction.response.send_message(
                    "Only admins can change vanity-tax enforcement.",
                    ephemeral=True,
                )
                return
            if not await safe_defer(interaction, ephemeral=True):
                return
            await asyncio.to_thread(
                vanity_service.set_manual_taxation,
                guild_id,
                target.id,
                enforced=taxable,
                actor_id=interaction.user.id,
            )
            if taxable:
                message = (
                    f"**{target.display_name}** is now manually subject to "
                    "the vanity tax, regardless of their server nickname."
                )
            else:
                eligibility = await asyncio.to_thread(
                    vanity_service.eligibility_status,
                    guild_id,
                    target.id,
                )
                if eligibility == "taxable":
                    automatic_status = (
                        "taxable because they have no server nickname"
                    )
                elif eligibility == "nickname_exemption":
                    automatic_status = (
                        "exempt because they have a server nickname"
                    )
                elif eligibility == "manual_taxation":
                    automatic_status = "taxable by a newer admin override"
                else:
                    automatic_status = (
                        "not currently in the server member cache"
                    )
                message = (
                    f"**{target.display_name}** returned to the automatic "
                    f"nickname rule and is {automatic_status}."
                )
            await safe_followup(
                interaction,
                content=message,
                ephemeral=True,
            )
            return
        rate_pct = vanity_service.TAX_RATE * 100
        eligibility = await asyncio.to_thread(
            vanity_service.eligibility_status,
            guild_id,
            target.id,
        )
        if eligibility == "manual_taxation":
            message = (
                f"**{target.display_name}** is subject to the "
                f"{rate_pct:g}% vanity tax by an admin override, regardless "
                "of their server nickname."
            )
        elif eligibility == "taxable":
            message = (
                f"🏷️ **{target.display_name}** has no server nickname, so a "
                f"**{rate_pct:g}% vanity tax** is "
                "levied on profits — bets, digs, wheel wins, predictions, "
                "mafia, match bonuses.\n"
                "Exemption: right-click your name → **Edit Server Profile** → "
                "set a nickname. The taxman notices immediately."
            )
        elif eligibility == "nickname_exemption":
            message = (
                f"✅ **{target.display_name}** has a server nickname and is "
                f"exempt from the {rate_pct:g}% vanity tax. Remove it and the "
                "taxman returns."
            )
        else:
            message = (
                f"**{target.display_name}** is not currently in the server "
                "member cache, so their vanity-tax status cannot be determined."
            )
        await interaction.response.send_message(message, ephemeral=True)

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
                ledger_limit=PLAYER_LEDGER_DEFAULT_LIMIT,
                ledger_offset=0,
            )
            total_ledger_entries = await asyncio.to_thread(
                self.tax_service.count_ledger_entries,
                guild_id,
                user_id=user.id,
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

        embed = _build_player_embed(
            user,
            snapshot,
            ledger_page=1,
            total_ledger_entries=total_ledger_entries,
            ledger_limit=PLAYER_LEDGER_DEFAULT_LIMIT,
        )
        view = None
        if _ledger_total_pages(total_ledger_entries, PLAYER_LEDGER_DEFAULT_LIMIT) > 1:
            view = TaxPlayerLedgerView(
                tax_service=self.tax_service,
                guild_id=guild_id,
                requester_id=interaction.user.id,
                user=user,
                limit=PLAYER_LEDGER_DEFAULT_LIMIT,
                current_page=1,
                total_entries=total_ledger_entries,
            )
        await safe_followup(interaction, embed=embed, view=view, ephemeral=True)

    @tax.command(name="fine", description="Levy a Tax Man fine against a player")
    @app_commands.describe(
        user="Player to fine",
        amount="Jopacoin amount to fine",
        reason="Reason recorded in the central ledger",
    )
    @require_guild
    async def fine(
        self,
        interaction: discord.Interaction,
        user: discord.User,
        amount: int,
        reason: str | None = None,
    ):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            result = await asyncio.to_thread(
                self.tax_service.levy_fine,
                user.id,
                guild_id,
                amount=amount,
                actor_id=interaction.user.id,
                reason=reason,
            )
        except Exception:
            logger.exception(
                "Failed to levy tax fine guild_id=%s user_id=%s actor_id=%s amount=%s",
                guild_id,
                user.id,
                interaction.user.id,
                amount,
            )
            await safe_followup(
                interaction,
                content="Couldn't levy that Tax Man fine right now.",
                ephemeral=True,
            )
            return

        await safe_followup(
            interaction,
            content=_format_fine_result(user, result),
            ephemeral=True,
        )

    @tax.command(
        name="resetcooldown",
        description="Reset a player's Tax Man fine cooldown",
    )
    @app_commands.describe(user="Player whose Tax Man fine cooldown should reset")
    @require_guild
    async def resetcooldown(
        self,
        interaction: discord.Interaction,
        user: discord.User,
    ):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            result = await asyncio.to_thread(
                self.tax_service.reset_fine_cooldown,
                user.id,
                guild_id,
                actor_id=interaction.user.id,
            )
        except Exception:
            logger.exception(
                "Failed to reset tax fine cooldown guild_id=%s user_id=%s actor_id=%s",
                guild_id,
                user.id,
                interaction.user.id,
            )
            await safe_followup(
                interaction,
                content="Couldn't reset that Tax Man fine cooldown right now.",
                ephemeral=True,
            )
            return

        await safe_followup(
            interaction,
            content=_format_reset_fine_cooldown_result(user, result),
            ephemeral=True,
        )

    @tax.command(
        name="bankruptcy",
        description="Add or remove a player's bankruptcy modifier",
    )
    @app_commands.describe(
        user="Player to modify",
        action="Add or remove the bankruptcy modifier",
        games="Penalty games to add; ignored when removing",
        reason="Reason for the modifier change",
    )
    @app_commands.choices(
        action=[
            app_commands.Choice(name="add", value="add"),
            app_commands.Choice(name="remove", value="remove"),
        ]
    )
    @require_guild
    async def bankruptcy(
        self,
        interaction: discord.Interaction,
        user: discord.User,
        action: app_commands.Choice[str],
        games: app_commands.Range[int, 0, 100] = BANKRUPTCY_PENALTY_GAMES,
        reason: str | None = None,
    ):
        if not await self._require_tax_man(interaction):
            return
        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        action_value = action.value if hasattr(action, "value") else str(action)
        try:
            if action_value == "add":
                result = await asyncio.to_thread(
                    self.tax_service.add_bankruptcy_modifier,
                    user.id,
                    guild_id,
                    games=games,
                    actor_id=interaction.user.id,
                    reason=reason,
                )
            elif action_value == "remove":
                result = await asyncio.to_thread(
                    self.tax_service.remove_bankruptcy_modifier,
                    user.id,
                    guild_id,
                    actor_id=interaction.user.id,
                    reason=reason,
                )
            else:
                result = {"status": "invalid_action"}
        except Exception:
            logger.exception(
                "Failed to update bankruptcy modifier guild_id=%s user_id=%s actor_id=%s action=%s",
                guild_id,
                user.id,
                interaction.user.id,
                action_value,
            )
            await safe_followup(
                interaction,
                content="Couldn't update that bankruptcy modifier right now.",
                ephemeral=True,
            )
            return

        await safe_followup(
            interaction,
            content=_format_bankruptcy_modifier_result(user, result),
            ephemeral=True,
        )

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


def _player_ledger_footer_text(page: int, total_entries: int, limit: int) -> str:
    ledger_text = _ledger_footer_text(page, total_entries, limit)
    return f"Tax Man audit only | Recent ledger {ledger_text.removeprefix('Tax Man ledger | ')}"


def _format_jc(amount: int) -> str:
    return f"{amount:,} {JOPACOIN_EMOTE}"


def _format_signed_jc(amount: int) -> str:
    prefix = "+" if amount > 0 else ""
    return f"{prefix}{amount:,} {JOPACOIN_EMOTE}"


def _build_public_event_embed(
    status: dict,
    *,
    icon_url: str | None = None,
) -> discord.Embed:
    return build_public_economy_event_embed(
        status.get("event"),
        icon_url=icon_url,
    )


def _build_policy_embed(status: dict) -> discord.Embed:
    policy = status["policy"]
    sheet = status["balance_sheet"]
    latest = status.get("latest_snapshot") or {}
    trends = status.get("monetary_trends") or {}
    flows = status.get("flow_indicators") or {}
    distribution = status.get("distribution") or {}
    event = status.get("event")
    mode = str(policy.get("mode", "unknown")).replace("_", " ").title()
    target = float(policy.get("target_annual_rate", 0.0)) * 100
    ceiling = float(policy.get("inflation_ceiling", 0.0)) * 100
    embed = discord.Embed(
        title="Jopacoin Monetary Policy",
        description=(
            f"**Mode:** {mode}\n"
            f"**Annual target:** {target:+.2f}%\n"
            f"**Inflation ceiling:** {ceiling:.2f}%\n"
            f"**Generated payout baseline:** {MINIGAME_JC_DELTA_SCALE:.0%}\n"
            f"**Reserve voting:** {'Paused' if mode == 'Recovery' else 'Enabled'}"
        ),
        color=0xD94B4B if mode == "Recovery" else 0x43B581,
    )
    embed.add_field(
        name="Reconciled stock",
        value=(
            f"Total: **{int(sheet['monetary_stock']):,} JC**\n"
            f"Wallets: **{int(sheet['player_wallets']):,} JC** "
            f"(avg **{float(sheet['average_wallet']):,.2f}**)\n"
            f"Positive wallets: **{int(sheet.get('positive_wallets', 0)):,} JC** · "
            f"visible debt: **{int(sheet.get('visible_debt', 0)):,} JC**\n"
            f"Reserve: **{int(sheet['reserve_available']):,} available** + "
            f"**{int(sheet['reserve_locked']):,} locked**\n"
            f"Next-match pot: **{int(sheet.get('reserve_next_match_pot', 0)):,} JC**\n"
            f"Prediction cash: **{int(sheet['prediction_open_cash']):,} JC**\n"
            f"Wager escrow: **{int(sheet['wager_escrow']):,} JC**"
        ),
        inline=False,
    )

    trend_lines = [
        _format_supply_trend("Live ~24h", trends.get("1d")),
        _format_supply_trend("Rolling 3d", trends.get("3d")),
        _format_supply_trend("Rolling 7d", trends.get("7d")),
    ]
    momentum = _format_supply_momentum(trends)
    if momentum:
        trend_lines.append(momentum)
    annualized_30d = latest.get("annualized_30d")
    annualized_90d = latest.get("annualized_90d")
    trend_lines.extend(
        [
            "30-day annualized: "
            + (
                f"**{float(annualized_30d) * 100:+.2f}%**"
                if annualized_30d is not None
                else "collecting data"
            ),
            "90-day annualized: "
            + (
                f"**{float(annualized_90d) * 100:+.2f}%**"
                if annualized_90d is not None
                else "collecting data"
            ),
        ]
    )
    embed.add_field(
        name="Money-supply inflation proxy",
        value="\n".join(trend_lines),
        inline=False,
    )

    flow_lines = [
        _format_flow_indicator("3d", flows.get("3d")),
        _format_flow_indicator("7d", flows.get("7d")),
    ]
    seven_day_flow = flows.get("7d")
    if seven_day_flow:
        flow_lines.append(
            "7d participation: "
            f"**{int(seven_day_flow.get('active_players', 0)):,}/"
            f"{int(sheet.get('player_count', 0)):,} players** "
            f"({float(seven_day_flow.get('active_player_rate', 0.0)):.1%})"
        )
    embed.add_field(
        name="Flows & activity",
        value="\n".join(flow_lines),
        inline=False,
    )

    stock = int(sheet.get("monetary_stock", 0))
    positive_wallets = int(sheet.get("positive_wallets", 0))
    visible_debt = int(sheet.get("visible_debt", 0))
    reserve_available = int(sheet.get("reserve_available", 0))
    committed = sum(
        int(sheet.get(key, 0))
        for key in (
            "reserve_locked",
            "reserve_next_match_pot",
            "prediction_open_cash",
            "wager_escrow",
        )
    )
    player_count = int(sheet.get("player_count", 0))
    median_wallet = float(distribution.get("median_positive_wallet", 0))
    health_lines = [
        "Supply per registered player: "
        + (
            f"**{stock / player_count:,.2f} JC**"
            if player_count
            else "**n/a**"
        ),
        "Visible debt / positive wallets: "
        + (
            f"**{visible_debt / positive_wallets:.1%}**"
            if positive_wallets
            else "**n/a**"
        ),
        "Available Reserve / positive wallets: "
        + (
            f"**{reserve_available / positive_wallets:.1%}**"
            if positive_wallets
            else "**n/a**"
        ),
        "Committed or escrowed supply: "
        + (f"**{committed / stock:.1%}**" if stock > 0 else "**n/a**"),
        f"Median positive wallet: **{median_wallet:,.2f} JC**",
        "Positive-wallet top 10% share: "
        f"**{float(distribution.get('top_decile_share', 0.0)):.1%}** · "
        f"positive-wallet Gini: **{float(distribution.get('gini', 0.0)):.3f}**",
    ]
    embed.add_field(
        name="Player-economy health",
        value="\n".join(health_lines),
        inline=False,
    )
    if event:
        direct_effect = int(event["direct_effect_jc"])
        immediate = (
            f"**{direct_effect:+,} JC**"
            if direct_effect
            else "**None** — this event works through adjusted outcomes"
        )
        embed.add_field(
            name=f"Active event: {event['name']} (Level {event['severity']})",
            value=(
                "Observed 7-day stock movement: "
                f"**{int(event['forecast_flow_jc']):+,} JC/day**\n"
                f"Today's target correction: **{int(event['target_effect_jc']):+,} JC**\n"
                f"Projected daily impact: **{int(event['expected_effect_jc']):+,} JC**\n"
                f"Immediate supply change: {immediate}"
            ),
            inline=False,
        )
    else:
        embed.add_field(
            name="Active event",
            value="No event has activated today.",
            inline=False,
        )
    embed.set_footer(
        text=(
            "Supply growth is a best-effort inflation proxy, not a price index. "
            "Windows use the nearest daily stock snapshot; turnover is gross "
            "ledger balance movement."
        )
    )
    return embed


def _format_supply_trend(label: str, trend: dict | None) -> str:
    if not trend:
        return f"{label}: **collecting data**"
    elapsed = float(trend["elapsed_days"])
    period_rate = float(trend["period_rate"])
    daily_rate = float(trend["daily_rate"])
    average_daily_change = float(trend["average_daily_change_jc"])
    arrow = "↗" if daily_rate > 0 else "↘" if daily_rate < 0 else "→"
    return (
        f"{label}: **{period_rate:+.2%}** ({int(trend['change_jc']):+,} JC) "
        f"{arrow} **{daily_rate:+.2%}/day**, "
        f"**{average_daily_change:+,.0f} JC/day** over {elapsed:.1f}d"
    )


def _format_supply_momentum(trends: dict) -> str | None:
    three_day = trends.get("3d")
    seven_day = trends.get("7d")
    if not three_day or not seven_day:
        return None
    short_rate = float(three_day["daily_rate"])
    long_rate = float(seven_day["daily_rate"])
    difference = short_rate - long_rate
    if math.isclose(difference, 0.0, abs_tol=0.0001):
        label = "steady"
    elif difference > 0:
        label = "heating"
    else:
        label = "cooling"
    return (
        f"Momentum: **{label}** "
        f"(3d pace vs 7d: **{difference:+.2%}/day**)"
    )


def _format_flow_indicator(label: str, flow: dict | None) -> str:
    if not flow:
        return f"{label}: **collecting data**"
    return (
        f"{label}: net **{float(flow['average_daily_net_jc']):+,.0f} JC/day** · "
        f"ledger movement **{float(flow['average_daily_gross_jc']):,.0f} JC/day** · "
        f"turnover **{float(flow['daily_turnover_rate']):.2%}/day**"
    )


def _format_tax_error(error: str) -> str:
    if error == "target_not_registered":
        return "That user is not registered in this guild."
    return "That player audit could not be loaded."


def _format_fine_result(user: discord.User, result: dict) -> str:
    display_name = getattr(user, "display_name", user.name)
    status = result.get("status")
    if status == "ok":
        applied = int(result["applied_amount"])
        requested = int(result["requested_amount"])
        before = int(result["balance_before"])
        after = int(result["balance_after"])
        line = (
            f"Levied a {_format_jc(applied)} fine against {display_name}. "
            f"Balance: {_format_jc(before)} -> {_format_jc(after)}. "
            f"Credited {_format_jc(applied)} to the Jopacoin Reserve."
        )
        if applied < requested:
            line += f" Requested {_format_jc(requested)}, capped to audited obligations."
        next_fine_at = result.get("next_fine_at")
        if next_fine_at is not None:
            line += f" Next fine available <t:{int(next_fine_at)}:R>."
        return line
    if status == "cooldown":
        return (
            f"{display_name} is still on Tax Man fine cooldown until "
            f"<t:{int(result['next_fine_at'])}:f> (<t:{int(result['next_fine_at'])}:R>)."
        )
    if status == "no_outstanding_obligations":
        return f"{display_name} has no audited outstanding obligations to fine."
    if status == "target_not_registered":
        return "That user is not registered in this guild."
    if status == "invalid_amount":
        return "Fine amount must be positive."
    return "That fine could not be levied."


def _format_reset_fine_cooldown_result(user: discord.User, result: dict) -> str:
    display_name = getattr(user, "display_name", user.name)
    status = result.get("status")
    if status == "ok":
        if result.get("had_cooldown"):
            return f"Reset Tax Man fine cooldown for {display_name}."
        return f"{display_name} did not have an active Tax Man fine cooldown."
    if status == "target_not_registered":
        return "That user is not registered in this guild."
    return "That Tax Man fine cooldown could not be reset."


def _format_bankruptcy_modifier_result(user: discord.User, result: dict) -> str:
    display_name = getattr(user, "display_name", user.name)
    status = result.get("status")
    if status == "ok":
        previous = int(result.get("previous_games") or 0)
        current = int(result.get("penalty_games_remaining") or 0)
        if result.get("action") == "add":
            games = int(result.get("games") or 0)
            return (
                f"Added {games:,} bankruptcy modifier game(s) to {display_name}. "
                f"Penalty games: {previous:,} -> {current:,}."
            )
        return (
            f"Removed bankruptcy modifier from {display_name}. "
            f"Penalty games: {previous:,} -> {current:,}."
        )
    if status == "target_not_registered":
        return "That user is not registered in this guild."
    if status == "invalid_games":
        return "Bankruptcy modifier games must be positive when adding."
    if status == "invalid_action":
        return "Unknown bankruptcy modifier action."
    return "That bankruptcy modifier could not be updated."


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
        name="Jopacoin Reserve",
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
    *,
    ledger_page: int = 1,
    total_ledger_entries: int | None = None,
    ledger_limit: int = PLAYER_LEDGER_DEFAULT_LIMIT,
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
    footer = "Tax Man audit only"
    if total_ledger_entries is not None:
        footer = _player_ledger_footer_text(
            ledger_page,
            total_ledger_entries,
            ledger_limit,
        )
    embed.set_footer(text=footer)
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
        "nonprofit_insert": "Jopacoin Reserve created",
        "nonprofit_update": "Jopacoin Reserve update",
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
            "Jopacoin Reserve"
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
