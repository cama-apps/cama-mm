"""
Tunnel digging minigame commands.
"""

from __future__ import annotations

import asyncio
import json
import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands, tasks

from commands.checks import require_dig_channel, require_guild

# View/modal classes and shared helpers live in commands/dig_helpers/ so this
# file stays focused on the cog. They are re-exported below because tests and
# the cog reference them by their original ``commands.dig`` names.
from commands.dig_helpers._shared import (
    _READING_HINTS,
    DIG_DUG_FOOTERS,
    DIG_DUG_TITLES,
    GUIDE_PAGES,
    LAYER_COLORS,
    _backstory_text,
    _check_registered,
    _DictObj,
    _fmt_duration,
    _format_s_stats,
    _layer_color,
    _reading_the_stone_hint,
    _splash_aftermath_lines,
    _tip,
    _wrap,
)
from commands.dig_helpers.artifact_embeds import (
    build_artifact_catalog_embeds as _build_artifact_catalog_embeds,
)
from commands.dig_helpers.bonus_events import maybe_send_dig_bonus
from commands.dig_helpers.boss_views import (
    BossDuelView,
    BossEncounterView,
    BossWagerModal,
    _build_boss_fight_result_embed,
)
from commands.dig_helpers.dig_views import (
    ConfirmAbandonView,
    ConfirmSabotageView,
    PaidDigView,
)
from commands.dig_helpers.event_views import BoonSelectionView, EventEncounterView
from commands.dig_helpers.gear_views import (
    GearPanelView,
    GearSelectView,
    _build_gear_embed,
)
from commands.dig_helpers.progression_views import (
    DigGuideView,
    MutationSelectionView,
    PrestigePerksView,
)
from config import DIG_CHANNEL_ID
from services.dig_constants import (
    ASCENSION_MODIFIERS,
    LUMINOSITY_DEEP_DRAIN_START_DEPTH,
    MAX_INVENTORY_SLOTS,
    PICKAXE_TIERS,
    format_relic_label,
    pick_description,
)
from services.dig_constants import get_layer as get_layer_def
from services.permissions import has_admin_permission
from utils.embed_safety import add_lines_field, truncate_field
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup, send_public_or_ephemeral
from utils.neon_helpers import get_neon_service, send_neon_result
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.dig_flavor_service import DigFlavorService
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")


def _append_sabotage_prediction_steal_line(embed: discord.Embed, result) -> None:
    sabotage_hit = getattr(result, "sabotage_hit", True)
    if isinstance(result, dict):
        sabotage_hit = result.get("sabotage_hit", True)
    if sabotage_hit is False:
        return

    steal = getattr(result, "prediction_contract_steal", None)
    if isinstance(result, dict):
        steal = result.get("prediction_contract_steal")
    if not steal:
        return

    prediction_id = getattr(steal, "prediction_id", None)
    side = getattr(steal, "side", None)
    contracts = getattr(steal, "contracts", None)
    if isinstance(steal, dict):
        prediction_id = steal.get("prediction_id")
        side = steal.get("side")
        contracts = steal.get("contracts")
    if prediction_id is None or not side or not contracts:
        return

    embed.description = (
        f"{embed.description or ''}\n"
        f"Stole **{contracts} {str(side).upper()}** prediction contracts from "
        f"market **#{prediction_id}**."
    )


def _format_auto_buy_settings(settings: dict | None) -> str:
    settings = settings or {}
    torch = "ON" if settings.get("torch") else "OFF"
    hard_hat = "ON" if settings.get("hard_hat") else "OFF"
    return f"Torch: **{torch}**\nHard Hat: **{hard_hat}**"

__all__ = [
    "DigCommands",
    "setup",
    # Re-exported view/modal classes (preserve commands.dig import paths).
    "BossDuelView",
    "BossEncounterView",
    "BossWagerModal",
    "BoonSelectionView",
    "ConfirmAbandonView",
    "ConfirmSabotageView",
    "DigGuideView",
    "EventEncounterView",
    "GearPanelView",
    "GearSelectView",
    "MutationSelectionView",
    "PaidDigView",
    "PrestigePerksView",
    # Re-exported helpers used by tests / embed builders.
    "_DictObj",
    "_wrap",
    "_build_boss_fight_result_embed",
    "_build_gear_embed",
    "_append_sabotage_prediction_steal_line",
    "_build_artifact_catalog_embeds",
    "_splash_aftermath_lines",
    "_reading_the_stone_hint",
    "_READING_HINTS",
]


# ---------------------------------------------------------------------------
# Cog
# ---------------------------------------------------------------------------

class DigCommands(commands.Cog):
    dig = app_commands.Group(name="dig", description="Tunnel digging minigame")
    admin = app_commands.Group(
        name="admin",
        description="Dig maintenance commands",
        parent=dig,
    )

    def __init__(self, bot: commands.Bot, dig_service: DigService, dig_flavor_service: DigFlavorService | None = None):
        self.bot = bot
        self.dig_service = dig_service
        self.dig_flavor_service = dig_flavor_service
        self._last_weather_date: str | None = None

    async def cog_load(self) -> None:
        self._weather_broadcast_loop.start()

    async def cog_unload(self) -> None:
        self._weather_broadcast_loop.cancel()

    @tasks.loop(minutes=10)
    async def _weather_broadcast_loop(self) -> None:
        """Check every 10 min if the game day rolled over; if so, post weather."""
        today = await asyncio.to_thread(self.dig_service._get_game_date)
        if today == self._last_weather_date:
            return
        self._last_weather_date = today

        for guild in self.bot.guilds:
            try:
                weather = await asyncio.to_thread(self.dig_service.get_weather, guild.id)
                if not weather:
                    continue

                embed = discord.Embed(
                    title="\u26c5 Daily Layer Weather",
                    description="New conditions have settled across the depths.",
                    color=0x5865F2,
                )
                for w in weather:
                    layer = w.get("layer", "Unknown")
                    name = w.get("name", "Unknown")
                    desc = w.get("description", "")
                    embed.add_field(
                        name=f"{layer} — {name}",
                        value=f"*{desc}*",
                        inline=False,
                    )
                embed.set_footer(text="Weather affects all diggers in that layer today. Use /dig weather for details.")

                target = None
                if DIG_CHANNEL_ID is not None:
                    target = guild.get_channel(DIG_CHANNEL_ID)
                if target is None:
                    for channel in guild.text_channels:
                        if "gamba" in channel.name.lower():
                            target = channel
                            break
                if target is not None:
                    await target.send(embed=embed)
            except Exception:
                logger.exception("Failed to broadcast weather for guild %s", guild.id)

    @_weather_broadcast_loop.before_loop
    async def _before_weather_loop(self) -> None:
        await self.bot.wait_until_ready()
        self._last_weather_date = await asyncio.to_thread(self.dig_service._get_game_date)

    # ------------------------------------------------------------------
    # Channel routing
    # ------------------------------------------------------------------

    async def _send_public_dig(
        self,
        interaction: discord.Interaction,
        *,
        embed: discord.Embed | None = None,
        view: discord.ui.View | None = None,
        file: discord.File | None = None,
        files: list[discord.File] | None = None,
    ) -> discord.Message | None:
        """Send a public dig embed to the dedicated dig channel when set,
        otherwise fall through to ``safe_followup``. When routed away from
        the invocation channel, also post a one-line ephemeral pointer so
        the user knows where their result landed.
        """
        target = await self._get_dig_target_channel(interaction)
        invocation_channel = interaction.channel
        send_kwargs: dict = {}
        if embed is not None:
            send_kwargs["embed"] = embed
        if view is not None:
            send_kwargs["view"] = view
        if files:
            send_kwargs["files"] = files
        elif file is not None:
            send_kwargs["file"] = file

        if target is None or (
            invocation_channel is not None
            and getattr(target, "id", None) == getattr(invocation_channel, "id", None)
        ):
            return await safe_followup(interaction, **send_kwargs)

        try:
            msg = await target.send(**send_kwargs)
        except Exception:
            logger.exception("Dig channel send failed; falling back to invocation channel")
            return await safe_followup(interaction, **send_kwargs)

        # Acknowledge the deferred interaction with a quiet pointer so the
        # user isn't left with a perpetual "thinking..." indicator.
        try:
            mention = getattr(target, "mention", None) or "the dig channel"
            await interaction.followup.send(
                f"Posted in {mention}.", ephemeral=True,
            )
        except Exception:
            pass
        return msg

    async def _get_dig_target_channel(
        self, interaction: discord.Interaction,
    ) -> discord.abc.Messageable | None:
        """Resolve the channel where public /dig embeds should land.

        Returns the dedicated dig channel when ``DIG_CHANNEL_ID`` is set,
        accessible, in the same guild as the interaction, and the bot
        has send permission. Otherwise falls back to ``interaction.channel``.
        Mirrors the lobby-channel pattern.
        """
        if not DIG_CHANNEL_ID:
            return interaction.channel
        if interaction.guild is None:
            return interaction.channel
        try:
            channel = self.bot.get_channel(DIG_CHANNEL_ID)
            if not channel:
                channel = await self.bot.fetch_channel(DIG_CHANNEL_ID)
            if isinstance(channel, discord.TextChannel):
                if channel.guild.id != interaction.guild.id:
                    logger.warning(
                        "Dedicated dig channel %s is in different guild", DIG_CHANNEL_ID,
                    )
                    return interaction.channel
                perms = channel.permissions_for(channel.guild.me)
                if not perms.send_messages:
                    logger.warning(
                        "Bot lacks send_messages in dedicated dig channel %s", DIG_CHANNEL_ID,
                    )
                    return interaction.channel
            return channel
        except (discord.NotFound, discord.Forbidden) as exc:
            logger.warning("Cannot access dedicated dig channel %s: %s", DIG_CHANNEL_ID, exc)
            return interaction.channel
        except Exception as exc:
            logger.warning("Error fetching dedicated dig channel: %s", exc)
            return interaction.channel

    # ------------------------------------------------------------------
    # Dig flavor helper
    # ------------------------------------------------------------------

    async def _run_dig(self, user_id: int, guild_id: int | None, paid: bool = False):
        """Run a deterministic dig, then layer LLM flavor on top.

        Mechanics are decided by ``DigService.dig`` and persisted before
        flavor runs. The flavor pass mutates the raw result dict to add
        narrative/NPC fields and (rarely) a small JC delta. On any flavor
        failure, the raw result is returned unchanged.
        """
        raw = await asyncio.to_thread(
            self.dig_service.dig, user_id, guild_id, paid=paid,
        )
        if isinstance(raw, dict) and raw.get("success", False):
            try:
                if await asyncio.to_thread(
                    self.dig_service.pop_relic_trim_notice, user_id, guild_id,
                ):
                    raw["relic_trim_notice"] = True
            except Exception:
                logger.warning("relic trim notice check failed", exc_info=True)
        if (
            self.dig_flavor_service is not None
            and isinstance(raw, dict)
            and raw.get("success", False)
        ):
            try:
                await self.dig_flavor_service.flavor(raw, user_id, guild_id)
            except Exception:
                logger.debug("dig flavor failed", exc_info=True)
        return _wrap(raw)

    async def _schedule_dig_reminder(self, user_id: int, guild_id: int | None) -> None:
        """Best-effort reconciliation against the persisted dig cooldown."""
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc is None:
            return
        try:
            await reminder_svc.reconcile_dig_reminder(
                self.bot, user_id, guild_id,
            )
        except Exception:
            logger.warning(
                "dig reminder scheduling failed for user %s in guild %s",
                user_id,
                guild_id,
                exc_info=True,
            )

    # ------------------------------------------------------------------
    # Autocomplete helpers
    # ------------------------------------------------------------------

    async def item_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for owned consumable items."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            items = await asyncio.to_thread(
                self.dig_service.get_inventory, interaction.user.id, guild_id
            )
            choices: list[app_commands.Choice[str]] = []
            for item in items or []:
                name = item.get("name") or ""
                if name and current.lower() in name.lower():
                    choices.append(app_commands.Choice(
                        name=name, value=item.get("type") or name,
                    ))
            return choices[:25]
        except Exception:
            return []

    async def relic_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for owned relics."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            relics = await asyncio.to_thread(
                self.dig_service.get_owned_relics, interaction.user.id, guild_id
            )
            # value must be the artifact id (what gift_relic matches on),
            # not the display label. Discord caps choice values at 100 chars.
            choices = [
                app_commands.Choice(
                    name=r.get("name", str(r))[:100],
                    value=str(r.get("id", ""))[:100],
                )
                for r in (relics or [])
                if current.lower() in r.get("name", "").lower()
            ]
            return choices[:25]
        except Exception:
            return []

    async def buy_autocomplete(
        self,
        interaction: discord.Interaction,
        current: str,
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete every item currently represented by the mining shop."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            shop = await asyncio.to_thread(
                self.dig_service.get_shop,
                interaction.user.id,
                guild_id,
            )
        except Exception:
            return []

        candidates: list[tuple[str, str]] = []
        for item in shop.get("consumables", []):
            candidates.append((
                f"{item['name']} ({item['price']} JC)",
                item["id"],
            ))
        for item in shop.get("pickaxe_upgrades", []):
            candidates.append((
                f"{item['name']} ({item['price']} JC)",
                f"weapon:{item['tier']}",
            ))
        for item in shop.get("gear_for_sale", []):
            candidates.append((
                f"{item['name']} ({item['price']} JC)",
                f"{item['slot']}:{item['tier']}",
            ))

        needle = current.casefold()
        return [
            app_commands.Choice(name=label, value=value)
            for label, value in candidates
            if needle in label.casefold() or needle in value.casefold()
        ][:25]

    # ------------------------------------------------------------------
    # 1. /dig — Main dig command
    # ------------------------------------------------------------------

    @dig.command(name="go", description="Dig deeper into your tunnel")
    @require_guild
    async def dig_go(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        guild_id = interaction.guild.id
        rl = GLOBAL_RATE_LIMITER.check(
            scope="dig", guild_id=guild_id, user_id=interaction.user.id, limit=2, per_seconds=30
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Slow down! Wait {rl.retry_after_seconds}s.", ephemeral=True
            )
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        try:
            result = await self._run_dig(interaction.user.id, guild_id)
        except Exception as e:
            logger.error("Dig error: %s", e, exc_info=True)
            await safe_followup(interaction, content="The earth resists. Try again in a moment.", ephemeral=True)
            return

        # Non-cooldown errors (cooldown is handled by the paid_dig_available branch)
        if not getattr(result, "success", False) and not getattr(result, "paid_dig_available", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Dig failed."),
                ephemeral=True,
            )
            return

        if getattr(result, "success", False):
            await self._schedule_dig_reminder(interaction.user.id, guild_id)

        # Witch's Curse: roll on successful dig outcomes only — skip the first-dig welcome
        # and the paid_dig_available cooldown prompt (no actual dig happened on those paths).
        if (
            not getattr(result, "is_first_dig", False)
            and not getattr(result, "paid_dig_available", False)
        ):
            curse_service = getattr(self.bot, "curse_service", None)
            if curse_service is not None and interaction.channel is not None:
                from services.curse_service import spawn_curse_flame
                jc_earned = getattr(result, "jc_earned", 0) or 0
                advance = getattr(result, "advance", 0) or 0
                if getattr(result, "cave_in", False):
                    dig_outcome = "loss"
                elif jc_earned > 0 or advance > 0 or getattr(result, "boss_encounter", False):
                    dig_outcome = "win"
                else:
                    dig_outcome = "neutral"
                spawn_curse_flame(
                    curse_service,
                    interaction.channel,
                    target_id=interaction.user.id,
                    guild_id=guild_id,
                    system="dig",
                    outcome=dig_outcome,
                    event_context={
                        "depth_after": getattr(result, "depth_after", None),
                        "advance": advance,
                        "jc_earned": jc_earned,
                        "cave_in": getattr(result, "cave_in", False),
                        "boss_encounter": getattr(result, "boss_encounter", False),
                        "tunnel_name": getattr(result, "tunnel_name", None),
                    },
                    target_display_name=getattr(interaction.user, "display_name", None),
                )

            # Catastrophic cave-in: atmospheric public flame.
            cave_in_detail_obj = getattr(result, "cave_in_detail", None)
            cave_in_type_for_flame = ""
            if cave_in_detail_obj is not None:
                cave_in_type_for_flame = (
                    cave_in_detail_obj.get("type", "")
                    if isinstance(cave_in_detail_obj, dict)
                    else getattr(cave_in_detail_obj, "type", "")
                )
            if cave_in_type_for_flame == "catastrophic" and interaction.channel is not None:
                from services.dig_flame import post_catastrophic
                post_catastrophic(interaction.channel)

            # Rare animated dig moment (relic unearthed / catastrophic cave-in).
            await self._maybe_send_dig_neon(interaction, result, guild_id)

        await self._dispatch_dig_result(interaction, guild_id, result)

    async def _dispatch_dig_result(
        self, interaction: discord.Interaction, guild_id: int, result,
    ) -> None:
        """Send the existing dig UI, then offer any rare cross-system bonus."""
        if getattr(result, "is_first_dig", False):
            await self._send_first_dig_welcome(interaction)
        elif getattr(result, "boss_encounter", False):
            await self._handle_boss_encounter(interaction, guild_id, result)
        elif getattr(result, "paid_dig_available", False):
            await self._handle_paid_dig_confirmation(interaction, guild_id, result)
        else:
            event = getattr(result, "event", None)
            event_data = None
            if event is not None:
                event_data = (
                    event
                    if isinstance(event, dict)
                    else (event._d if hasattr(event, "_d") else None)
                )
            if (
                isinstance(event_data, dict)
                and event_data.get("complexity", "choice") == "boon"
                and event_data.get("boon_options")
            ):
                await self._handle_boon_encounter(
                    interaction, guild_id, result, event_data,
                )
            elif isinstance(event_data, dict) and event_data.get("safe_option"):
                await self._handle_choice_encounter(
                    interaction, guild_id, result, event_data,
                )
            else:
                await self._send_normal_dig_result(interaction, result)

        await maybe_send_dig_bonus(self.bot, interaction, result)

    # ── dig_go helpers (one per result shape) ───────────────────────────

    async def _send_first_dig_welcome(self, interaction: discord.Interaction) -> None:
        """Reply to the very first dig with a welcome embed."""
        embed = discord.Embed(
            title="Welcome to the Mines!",
            description=(
                "You've started digging your very own tunnel!\n\n"
                "Use `/dig` to advance deeper, `/dig shop` to buy items, "
                "and `/dig guide` for a full tutorial.\n\n"
                "Good luck, miner! **DIG DUG!**"
            ),
            color=LAYER_COLORS["Dirt"],
        )
        await self._send_public_dig(interaction, embed=embed)

    async def _handle_boss_encounter(
        self, interaction: discord.Interaction, guild_id: int | None, result
    ) -> None:
        """Render a boss encounter embed and attach the interactive view."""
        boss_info = getattr(result, "boss_info", None)
        # Scout button enables on lantern *ownership*, not on whether the
        # player queued one this dig — otherwise owners who didn't /dig use
        # the lantern see Scout greyed out on a freshly-encountered boss.
        has_lantern = await asyncio.to_thread(
            self.dig_service.has_scout_lantern, interaction.user.id, guild_id,
        )
        embed = discord.Embed(
            title=f"Boss Encountered: {getattr(boss_info, 'name', 'Unknown Boss')}!",
            description=getattr(boss_info, "dialogue", "A fearsome guardian blocks your path!"),
            color=0xFF0000,
        )

        boss_file = None
        boundary = getattr(boss_info, "boundary", None)
        boss_id = getattr(boss_info, "boss_id", "") or boundary
        if boss_id:
            try:
                from utils.dig_assets import get_boss_art
                depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
                ld = get_layer_def(depth or boundary)
                ln = ld.name if ld else "Dirt"
                boss_file = await asyncio.to_thread(get_boss_art, boss_id, "encounter", ln)
            except Exception as e:
                logger.debug("Boss encounter art failed: %s", e)

        if boss_file:
            embed.set_image(url=f"attachment://{boss_file.filename}")
        elif hasattr(boss_info, "ascii_art"):
            embed.add_field(name="\u200b", value=f"```\n{boss_info.ascii_art}\n```", inline=False)

        lum_line = getattr(boss_info, "luminosity_display", None)
        if lum_line:
            embed.add_field(name="\u200b", value=lum_line, inline=False)

        view = BossEncounterView(
            self.dig_service,
            interaction.user.id,
            guild_id,
            boss_info,
            has_lantern,
            dig_flavor_service=self.dig_flavor_service,
            on_boss_resolved=self._schedule_dig_reminder,
        )
        msg = await self._send_public_dig(interaction, embed=embed, view=view, file=boss_file)
        if msg:
            view.message = msg
            try:
                await msg.add_reaction("\U0001f480")
            except Exception:
                pass

    async def _handle_paid_dig_confirmation(
        self, interaction: discord.Interaction, guild_id: int | None, result
    ) -> None:
        """Prompt the player for a paid dig and execute it on confirm.

        Also routes any event the paid dig rolled (the bug-fix brought in
        from ``feat/dig-more``): free and paid digs should both surface
        interactive boon/choice encounters.
        """
        cost = getattr(result, "paid_dig_cost", 0)
        cooldown_remaining = getattr(result, "cooldown_remaining", 0)
        cooldown_str = _fmt_duration(int(cooldown_remaining))
        embed = discord.Embed(
            title="Paid Dig Required",
            description=f"Free dig on cooldown for **{cooldown_str}**.\nContinuing costs **{cost}** {JOPACOIN_EMOTE}. Proceed?",
            color=0xFFA500,
        )
        view = PaidDigView(self.dig_service, interaction.user.id, guild_id, cost)
        msg = await safe_followup(interaction, embed=embed, view=view)
        if not msg:
            return
        await view.wait()
        if not view.value:
            await msg.edit(content="Dig cancelled.", embed=None, view=None)
            return
        # Show immediate feedback while the dig runs
        await msg.edit(
            embed=discord.Embed(title="Digging...", description="Your pickaxe swings.", color=0xFFA500),
            view=None,
        )
        try:
            paid_result = await self._run_dig(
                interaction.user.id, guild_id, paid=True,
            )
        except Exception as e:
            logger.error("Paid dig error: %s", e)
            await msg.edit(content="Paid dig failed.", embed=None, view=None)
            return
        if not getattr(paid_result, "success", False):
            err = getattr(paid_result, "error", "Paid dig failed.")
            await msg.edit(content=err, embed=None, view=None)
            return
        await self._schedule_dig_reminder(interaction.user.id, guild_id)
        paid_embed, paid_layer_name, paid_pick_tier, paid_items_ids = _build_dig_embed(paid_result, interaction.user)
        paid_layer_file = await _attach_layer_thumbnail(paid_embed, paid_layer_name)
        paid_pick_file = await _attach_pickaxe_footer(paid_embed, paid_pick_tier)
        paid_items_strip = await _attach_items_strip(paid_embed, paid_items_ids)
        paid_files = [f for f in (paid_layer_file, paid_pick_file, paid_items_strip) if f]
        if paid_files:
            await msg.edit(embed=paid_embed, view=None, attachments=paid_files)
        else:
            await msg.edit(embed=paid_embed, view=None)

        # Route any event the paid dig rolled. The stats card is already
        # posted via msg.edit above, so we only need the event UI (no
        # second stats-card send).
        event = getattr(paid_result, "event", None)
        if event is not None:
            event_data = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
            if isinstance(event_data, dict):
                complexity = event_data.get("complexity", "choice")
                if complexity == "boon" and event_data.get("boon_options"):
                    await self._send_boon_event_ui(interaction, guild_id, paid_result, event_data)
                elif event_data.get("safe_option"):
                    await self._send_choice_event_ui(interaction, guild_id, paid_result, event_data)

        await maybe_send_dig_bonus(self.bot, interaction, paid_result)

    async def _resolve_event_art(self, event_id: str, result) -> discord.File | None:
        """Attempt to load the event art attachment, returning ``None`` on failure."""
        try:
            from utils.dig_assets import get_event_art
            depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
            layer_def = get_layer_def(depth)
            ev_layer = layer_def.name if layer_def else "Dirt"
            return await asyncio.to_thread(get_event_art, event_id, ev_layer)
        except Exception as e:
            logger.debug("Event art failed: %s", e)
            return None

    async def _maybe_send_dig_neon(self, interaction, result, guild_id) -> None:
        """Best-effort: a rare neon GIF for a relic unearthed or a catastrophic cave-in."""
        try:
            neon = get_neon_service(self.bot)
            if not neon:
                return
            depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
            ld = get_layer_def(depth)
            layer_name = ld.name if ld else "Dirt"

            cave_in_detail = getattr(result, "cave_in_detail", None)
            cave_type = ""
            if cave_in_detail is not None:
                cave_type = (
                    cave_in_detail.get("type", "")
                    if isinstance(cave_in_detail, dict)
                    else getattr(cave_in_detail, "type", "")
                )

            nr = None
            if cave_type == "catastrophic":
                depth_after = getattr(result, "depth_after", 0) or depth
                block_loss = (
                    cave_in_detail.get("block_loss", 0)
                    if isinstance(cave_in_detail, dict)
                    else getattr(cave_in_detail, "block_loss", 0)
                ) or 0
                nr = await neon.on_dig_cave_in(
                    interaction.user.id,
                    guild_id,
                    depth_before=depth_after + int(block_loss),
                    depth_after=depth_after,
                    layer_name=layer_name,
                )
            else:
                artifact = getattr(result, "artifact", None)
                if artifact and not isinstance(artifact, str):
                    rarity = (getattr(artifact, "rarity", "") or "").lower()
                    if rarity in ("rare", "legendary"):
                        nr = await neon.on_dig_relic_found(
                            interaction.user.id,
                            guild_id,
                            relic_name=getattr(artifact, "name", "a relic"),
                            rarity=rarity,
                            layer_name=layer_name,
                        )
            await send_neon_result(interaction, nr)
        except Exception as e:
            logger.debug("dig neon hook failed: %s", e)

    async def _send_dig_result_with_attachments(
        self, interaction: discord.Interaction, result
    ) -> None:
        """Send the main dig result embed (stats card) with layer/pickaxe/items files."""
        embed, layer_name, pickaxe_tier, items_ids = _build_dig_embed(result, interaction.user)
        layer_file = await _attach_layer_thumbnail(embed, layer_name)
        pickaxe_file = await _attach_pickaxe_footer(embed, pickaxe_tier)
        items_strip = await _attach_items_strip(embed, items_ids)
        dig_files = [f for f in (layer_file, pickaxe_file, items_strip) if f]
        if len(dig_files) > 1:
            await self._send_public_dig(interaction, embed=embed, files=dig_files)
        elif dig_files:
            await self._send_public_dig(interaction, embed=embed, file=dig_files[0])
        else:
            await self._send_public_dig(interaction, embed=embed)

    async def _send_boon_event_ui(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Send just the boon-selection embed + view (no stats card)."""
        boon_options = event_data["boon_options"]
        boon_lines = [f"**{b.get('name', '?')}** — {b.get('description', '')}" for b in boon_options]
        boon_flavor = pick_description(event_data) or "Choose a boon:"
        event_embed = discord.Embed(
            description=boon_flavor + "\n\n" + "\n".join(boon_lines),
            color=0x5865F2,
        )
        boon_event_file = await self._resolve_event_art(event_data.get("id", ""), result)
        if boon_event_file:
            event_embed.set_image(url=f"attachment://{boon_event_file.filename}")

        target_channel = await self._get_dig_target_channel(interaction)
        view = BoonSelectionView(
            self.dig_service, interaction.user.id, guild_id, event_data,
            target_channel=target_channel,
        )
        if boon_event_file:
            msg = await self._send_public_dig(interaction, embed=event_embed, view=view, file=boon_event_file)
        else:
            msg = await self._send_public_dig(interaction, embed=event_embed, view=view)
        view.message = msg

    async def _send_choice_event_ui(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Send just the choice-event embed + encounter view (no stats card)."""
        event_embed = discord.Embed(
            description=pick_description(event_data) or "Something happens...",
            color=0xDAA520,
        )
        ascii_art = event_data.get("ascii_art")
        if ascii_art:
            event_embed.add_field(name="\u200b", value=f"```\n{ascii_art}\n```", inline=False)

        event_file = await self._resolve_event_art(event_data.get("id", ""), result)
        if event_file:
            event_embed.set_image(url=f"attachment://{event_file.filename}")

        # Perk: reading_the_stone — atmospheric whisper toward the best-EV
        # option. No numbers; relies on flavor so the mechanic stays in-fiction.
        has_reveal_perk = await asyncio.to_thread(
            self.dig_service.has_perk, interaction.user.id, guild_id, "reading_the_stone",
        )
        if has_reveal_perk:
            hint = _reading_the_stone_hint(event_data)
            if hint:
                event_embed.add_field(name="​", value=f"_{hint}_", inline=False)

        _lum_info = getattr(result, "luminosity_info", None)
        _lum_val = (_lum_info.get("luminosity_after", 100) if isinstance(_lum_info, dict)
                    else getattr(_lum_info, "luminosity_after", 100)) if _lum_info else 100
        target_channel = await self._get_dig_target_channel(interaction)
        view = EventEncounterView(
            self.dig_service, interaction.user.id, guild_id, event_data,
            luminosity=_lum_val, target_channel=target_channel,
            dig_flavor_service=self.dig_flavor_service,
        )
        if event_file:
            msg = await self._send_public_dig(interaction, embed=event_embed, view=view, file=event_file)
        else:
            msg = await self._send_public_dig(interaction, embed=event_embed, view=view)
        view.message = msg

    async def _handle_boon_encounter(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Render a boon-pick encounter: stats card + boon selection view."""
        await self._send_dig_result_with_attachments(interaction, result)
        await self._send_boon_event_ui(interaction, guild_id, result, event_data)

    async def _handle_choice_encounter(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        result,
        event_data: dict,
    ) -> None:
        """Render a choice/complex event: stats card + encounter view with safe/risky buttons."""
        await self._send_dig_result_with_attachments(interaction, result)
        await self._send_choice_event_ui(interaction, guild_id, result, event_data)

    async def _send_normal_dig_result(
        self, interaction: discord.Interaction, result
    ) -> None:
        """Send the plain dig result (no boss/event/boon) and attach post-dig reactions."""
        embed, layer_name, pickaxe_tier, items_ids = _build_dig_embed(result, interaction.user)
        layer_file = await _attach_layer_thumbnail(embed, layer_name)
        pickaxe_file = await _attach_pickaxe_footer(embed, pickaxe_tier)
        items_strip = await _attach_items_strip(embed, items_ids)
        dig_files = [f for f in (layer_file, pickaxe_file, items_strip) if f]
        if len(dig_files) > 1:
            msg = await self._send_public_dig(interaction, embed=embed, files=dig_files)
        elif dig_files:
            msg = await self._send_public_dig(interaction, embed=embed, file=dig_files[0])
        else:
            msg = await self._send_public_dig(interaction, embed=embed)

        if msg:
            reactions = ["\u26cf\ufe0f"]  # pickaxe
            if getattr(result, "cave_in", None):
                reactions.append("\U0001f4a5")
            if getattr(result, "artifact", None):
                reactions.append("\U0001f48e")
            for r in reactions:
                try:
                    await msg.add_reaction(r)
                except Exception:
                    pass

    # ------------------------------------------------------------------
    # 2. /dig_help — Help another player
    # ------------------------------------------------------------------

    @dig.command(name="help", description="Help another player's tunnel")
    @app_commands.describe(user="The player to help")
    @require_guild
    async def dig_help(self, interaction: discord.Interaction, user: discord.Member):
        if not await require_dig_channel(interaction):
            return

        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        player = await asyncio.to_thread(
            self.bot.player_service.get_player, interaction.user.id, guild_id
        )
        if not player:
            await safe_followup(
                interaction,
                content="You must be registered first. Use `/player register`.",
                ephemeral=True,
            )
            return

        if user.id == interaction.user.id:
            self_help_lines = [
                "You tried to help yourself. The pickaxe is confused.",
                "That's not how teamwork works, chief.",
                "Mining solo is fine, but helping yourself is just sad.",
                "Your tunnel filed a restraining order against your own help.",
                "You can't pat your own back with a pickaxe. Well, you can, but you shouldn't.",
                "Self-help books are in aisle 3. This is a mine.",
            ]
            await safe_followup(
                interaction,
                content=random.choice(self_help_lines),
                ephemeral=True,
            )
            return

        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.help_tunnel,
                interaction.user.id,
                user.id,
                guild_id,
            ))
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Dig help error: %s", e)
            await safe_followup(interaction, content="Help failed.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Help failed."),
                ephemeral=True,
            )
            return

        blocks = getattr(result, "advance", 0)
        embed = discord.Embed(
            title="Tunnel Assistance",
            description=(
                f"You helped **{user.display_name}**'s tunnel!\n"
                f"Blocks added: **{blocks}**"
            ),
            color=0x2ECC71,
        )
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 3. /dig_sabotage — Sabotage another player
    # ------------------------------------------------------------------

    @dig.command(name="sabotage", description="Sabotage another player's tunnel")
    @app_commands.describe(user="The player to sabotage")
    @require_guild
    async def dig_sabotage(self, interaction: discord.Interaction, user: discord.Member):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id

        # Get sabotage preview info
        try:
            preview = _wrap(await asyncio.to_thread(
                self.dig_service.preview_sabotage,
                interaction.user.id,
                user.id,
                guild_id,
            ))
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Sabotage preview error: %s", e)
            await interaction.response.send_message("Sabotage failed.", ephemeral=True)
            return

        if not getattr(preview, "success", False):
            await interaction.response.send_message(
                getattr(preview, "error", "Sabotage failed."),
                ephemeral=True,
            )
            return

        cost = getattr(preview, "cost", 0)
        damage_range = getattr(preview, "damage_range", "unknown")

        view = ConfirmSabotageView(interaction.user.id, user, cost, str(damage_range))
        embed = view.build_embed()
        await interaction.response.send_message(embed=embed, view=view)
        await view.wait()

        if view.value:
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.sabotage_tunnel,
                    interaction.user.id,
                    user.id,
                    guild_id,
                ))
                if not getattr(result, "success", False):
                    await interaction.edit_original_response(
                        content=getattr(result, "error", "Sabotage failed."),
                        embed=None, view=None,
                    )
                    return
                result_embed = discord.Embed(color=0x2C2F33)
                if getattr(result, "trap_triggered", False):
                    trap = getattr(result, "trap_detail", None)
                    trap_msg = getattr(trap, "message", "") if trap else ""
                    result_embed.title = "Trap Triggered!"
                    result_embed.description = (
                        f"Your sabotage attempt backfired!\n{trap_msg}"
                    )
                    result_embed.color = 0xFF0000
                else:
                    damage = getattr(result, "damage", 0)
                    if getattr(result, "sabotage_hit", True) is False:
                        result_embed.title = "Sabotage Missed"
                        result_embed.description = (
                            f"You tried to sabotage **{user.display_name}**'s tunnel, "
                            "but the strike missed.\n"
                            f"Damage dealt: **{damage}** blocks"
                        )
                    else:
                        result_embed.title = "Sabotage Successful"
                        result_embed.description = (
                            f"You sabotaged **{user.display_name}**'s tunnel!\n"
                            f"Damage dealt: **{damage}** blocks"
                        )
                        _append_sabotage_prediction_steal_line(result_embed, result)
                await interaction.edit_original_response(embed=result_embed, view=None)
            except ValueError as e:
                await interaction.edit_original_response(content=str(e), embed=None, view=None)
            except Exception as e:
                logger.error("Sabotage error: %s", e)
                await interaction.edit_original_response(content="Sabotage failed.", embed=None, view=None)
        else:
            await interaction.edit_original_response(content="Sabotage cancelled.", embed=None, view=None)

    # ------------------------------------------------------------------
    # 4. /dig info — View tunnel info
    # ------------------------------------------------------------------

    @dig.command(name="info", description="View tunnel information")
    @app_commands.describe(user="View another player's tunnel (optional)")
    @require_guild
    async def dig_info(self, interaction: discord.Interaction, user: discord.Member | None = None):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        target_id = user.id if user else interaction.user.id
        is_own = target_id == interaction.user.id

        try:
            info = await asyncio.to_thread(
                self.dig_service.get_tunnel_info, target_id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Dig info error: %s", e)
            await safe_followup(interaction, content="Failed to fetch tunnel info.", ephemeral=True)
            return

        if info is None:
            target_name = (user.display_name if user else interaction.user.display_name)
            await safe_followup(
                interaction,
                content=f"{target_name} hasn't started digging yet.",
                ephemeral=True,
            )
            return

        # Service returns a raw dict — don't wrap, use .get() directly
        layer_info = info.get("layer", {}) if isinstance(info, dict) else {}
        layer_name = layer_info.get("name", "Dirt") if isinstance(layer_info, dict) else "Dirt"
        tunnel = info.get("tunnel", {}) if isinstance(info, dict) else {}

        display_user = user or interaction.user
        embed = discord.Embed(
            title=f"{display_user.display_name}'s Tunnel",
            color=_layer_color(layer_name),
        )

        # Core stats
        depth = info.get("depth", 0) if isinstance(info, dict) else 0
        prestige = info.get("prestige_level", 0) if isinstance(info, dict) else 0
        pickaxe_idx = tunnel.get("pickaxe_tier", 0) or 0
        pickaxe_name = PICKAXE_TIERS[pickaxe_idx]["name"] if pickaxe_idx < len(PICKAXE_TIERS) else "Wooden"
        prestige_text = f" (Prestige {prestige})" if prestige else ""
        embed.add_field(
            name="Depth",
            value=f"**{depth}** blocks — {layer_name}{prestige_text}",
            inline=True,
        )
        embed.add_field(name="Pickaxe", value=pickaxe_name, inline=True)

        # Equipped relics
        relics = info.get("relics", []) if isinstance(info, dict) else []
        if relics:
            relic_text = ", ".join(
                format_relic_label(r.get("artifact_id", ""))
                if isinstance(r, dict) else str(r)
                for r in relics
            )
            embed.add_field(name="Relics", value=truncate_field(relic_text), inline=False)

        # Queued items
        queued = info.get("queued_items", []) if isinstance(info, dict) else []
        if queued:
            item_text = ", ".join(i.get("name", "?") if isinstance(i, dict) else str(i) for i in queued)
            embed.add_field(name="Queued Items", value=item_text, inline=False)

        # Boss status
        at_boss = info.get("at_boss", False) if isinstance(info, dict) else False
        next_boss = info.get("next_boss", None) if isinstance(info, dict) else None
        if at_boss:
            embed.add_field(name="Boss", value="A boss blocks your path!", inline=True)
        elif next_boss:
            embed.add_field(name="Next Boss", value=f"Depth {next_boss}", inline=True)

        # Pinnacle foreshadow — fires only after all 7 tier bosses cleared
        # and pinnacle still pending. Subtle, no explicit depth.
        foreshadow = info.get("pinnacle_foreshadow") if isinstance(info, dict) else None
        if foreshadow:
            embed.add_field(name="​", value=f"*{foreshadow}*", inline=False)
        elif depth > LUMINOSITY_DEEP_DRAIN_START_DEPTH:
            embed.add_field(name="​", value="*The deep grows hungry.*", inline=False)

        # Insurance / reinforcement
        now = int(time.time())
        insured_until = tunnel.get("insured_until", 0) or 0
        reinforced_until = tunnel.get("reinforced_until", 0) or 0
        status_parts = []
        if now < insured_until:
            status_parts.append("Insured")
        if now < reinforced_until:
            status_parts.append("Reinforced")
        hard_hat_charges = tunnel.get("hard_hat_charges", 0) or 0
        if hard_hat_charges > 0:
            status_parts.append(f"Hard Hat ({hard_hat_charges})")
        if status_parts:
            embed.add_field(name="Protection", value=", ".join(status_parts), inline=True)

        # Trap status
        trap = tunnel.get("trap_active", False)
        if is_own and trap:
            embed.add_field(name="Trap", value="Armed", inline=True)
        elif not is_own and trap:
            embed.add_field(name="Trap", value="Something feels off...", inline=True)

        # Streak
        streak = info.get("streak", 0) if isinstance(info, dict) else 0
        if streak:
            embed.add_field(name="Streak", value=f"{streak} days", inline=True)

        # Cooldown
        cooldown = info.get("cooldown_remaining", 0) if isinstance(info, dict) else 0
        if cooldown and cooldown > 0:
            embed.add_field(name="Cooldown", value=_fmt_duration(cooldown), inline=True)

        # Next milestone
        milestone = info.get("next_milestone", None) if isinstance(info, dict) else None
        if milestone and isinstance(milestone, dict):
            embed.add_field(
                name="Next Milestone",
                value=f"Depth {milestone.get('depth', '?')} (+{milestone.get('reward', '?')} JC)",
                inline=True,
            )

        # Recent events — parse the JSON detail for a readable summary
        events = info.get("recent_events", []) if isinstance(info, dict) else []
        if events:
            event_lines = []
            for ev in events[:5]:
                if not isinstance(ev, dict):
                    continue
                action = ev.get("action_type", "?")
                detail_raw = ev.get("detail") or ev.get("details") or "{}"
                try:
                    detail = json.loads(detail_raw) if isinstance(detail_raw, str) else detail_raw
                except (json.JSONDecodeError, TypeError):
                    detail = {}
                if action == "dig":
                    adv = detail.get("advance", 0)
                    jc = detail.get("jc", 0)
                    if detail.get("cave_in"):
                        event_lines.append(f"Cave-in! Lost {detail.get('block_loss', '?')} blocks")
                    else:
                        event_lines.append(f"Dug +{adv} blocks, +{jc} JC")
                elif action == "sabotage":
                    dmg = detail.get("damage", "?")
                    if detail.get("trap_triggered"):
                        event_lines.append("Sabotage attempt — trap triggered!")
                    else:
                        event_lines.append(f"Sabotaged — lost {dmg} blocks")
                elif action == "help":
                    adv = detail.get("advance", "?")
                    event_lines.append(f"Helped — +{adv} blocks")
                else:
                    event_lines.append(action.replace("_", " ").title())
            if event_lines:
                embed.add_field(name="Recent Events", value="\n".join(event_lines), inline=False)

        # Active ascension modifiers (prestige > 0)
        if prestige > 0:
            asc_lines = []
            for level in range(1, prestige + 1):
                mod = ASCENSION_MODIFIERS.get(level)
                if mod:
                    asc_lines.append(f"**P{level} {mod.name}**: {mod.penalty} / {mod.reward}")
            if asc_lines:
                # Truncate to fit embed field limit
                asc_text = "\n".join(asc_lines[:10])
                embed.add_field(name="Ascension Modifiers", value=asc_text, inline=False)

        embed.set_thumbnail(url=display_user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 5. /dig_leaderboard — Top tunnels
    # ------------------------------------------------------------------

    @dig.command(name="leaderboard", description="View top tunnels")
    @require_guild
    async def dig_leaderboard(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            lb = _wrap(await asyncio.to_thread(
                self.dig_service.get_leaderboard, guild_id
            ))
        except Exception as e:
            logger.error("Leaderboard error: %s", e)
            await safe_followup(interaction, content="Leaderboard unavailable.", ephemeral=True)
            return

        entries = getattr(lb, "tunnels", []) or []
        if not entries:
            await safe_followup(interaction, content="No tunnels yet! Use `/dig` to start.", ephemeral=True)
            return

        # Build leaderboard text
        lines = []
        def _get(obj, key, default=None):
            return obj.get(key, default) if isinstance(obj, dict) else getattr(obj, key, default)

        def _resolve_name(entry) -> str:
            discord_id = _get(entry, "discord_id", None)
            tunnel_name = _get(entry, "tunnel_name", None)
            display = None
            if discord_id is not None and interaction.guild is not None:
                member = interaction.guild.get_member(int(discord_id))
                if member is not None:
                    display = member.display_name or member.name
            display = display or tunnel_name or (
                f"Tunnel #{discord_id}" if discord_id is not None else "Unknown"
            )
            if len(display) > 20:
                display = display[:19] + "…"
            return display

        max_depth = max(_get(e, "depth", 0) for e in entries[:10]) or 1
        for i, entry in enumerate(entries[:10], 1):
            name = _resolve_name(entry)
            depth = _get(entry, "depth", 0)
            prestige = _get(entry, "prestige_level", 0) or 0
            prestige_tag = f" (P{prestige})" if prestige > 0 else ""
            bar_len = max(1, int(20 * depth / max_depth))
            bar = "\u2588" * bar_len
            lines.append(f"`{i:>2}.` **{name}** — Depth {depth}{prestige_tag}\n`{bar}`")

        # Requester's position
        user_pos = getattr(lb, "user_position", None)
        if user_pos and user_pos > 10:
            lines.append(f"\n---\n`{user_pos}.` **You** — {getattr(lb, 'user_depth', '?')}")

        embed = discord.Embed(
            title="Tunnel Leaderboard",
            description="\n".join(lines),
            color=0xFFD700,
        )
        embed.set_footer(text="Community Mine")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 5b. /dig halloffame — Best prestige run scores
    # ------------------------------------------------------------------

    @dig.command(name="halloffame", description="View the hall of fame (best prestige run scores)")
    @require_guild
    async def dig_halloffame(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.get_hall_of_fame, guild_id
            ))
        except Exception as e:
            logger.error("Hall of fame error: %s", e)
            await safe_followup(interaction, content="Hall of fame unavailable.", ephemeral=True)
            return

        entries = getattr(result, "entries", []) or []
        if not entries:
            await safe_followup(
                interaction,
                content="The hall of fame is empty. Prestige to earn a spot!",
                ephemeral=True,
            )
            return

        lines = []
        for i, entry in enumerate(entries[:10], 1):
            def _g(obj, key, default=None):
                return obj.get(key, default) if isinstance(obj, dict) else getattr(obj, key, default)
            name = _g(entry, "tunnel_name", "Unknown")
            discord_id = _g(entry, "discord_id", None)
            prestige = _g(entry, "prestige_level", 0)
            score = _g(entry, "best_run_score", 0)
            player_mention = f"<@{discord_id}>" if discord_id else "Unknown"
            medal = {1: "\U0001f947", 2: "\U0001f948", 3: "\U0001f949"}.get(i, f"`#{i}`")
            lines.append(f"{medal} **{name}** ({player_mention}) - Score: {score} (P{prestige})")

        embed = discord.Embed(
            title="\U0001f3c6 Hall of Fame",
            description="\n".join(lines),
            color=0xFFD700,
        )
        embed.set_footer(text="Best prestige run scores")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 6. /dig use — Queue consumable
    # ------------------------------------------------------------------

    @dig.command(name="use", description="Queue a consumable for your next dig")
    @app_commands.describe(item="The item to use")
    @app_commands.autocomplete(item=item_autocomplete)
    @require_guild
    async def dig_use(self, interaction: discord.Interaction, item: str):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.use_item, interaction.user.id, guild_id, item
            ))
            if not getattr(result, "success", False):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Failed to queue item."),
                    ephemeral=True,
                )
                return
            item_name = getattr(result, "item", item)
            use_embed = discord.Embed(
                title=f"{item_name} Queued",
                description="Ready for your next dig.",
                color=0xD4AF37,
            )
            item_file = None
            try:
                from utils.dig_assets import get_item_art
                item_file = await asyncio.to_thread(get_item_art, item)
                if item_file:
                    use_embed.set_thumbnail(url=f"attachment://{item_file.filename}")
            except Exception:
                pass
            await safe_followup(interaction, embed=use_embed, file=item_file, ephemeral=True)
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Dig use error: %s", e)
            await safe_followup(interaction, content="Failed to queue item.", ephemeral=True)

    # ------------------------------------------------------------------
    # 7. /dig gift — Gift a relic
    # ------------------------------------------------------------------

    @dig.command(name="gift", description="Gift a relic to another player")
    @app_commands.describe(user="The player to gift to", artifact="The relic to gift")
    @app_commands.autocomplete(artifact=relic_autocomplete)
    @require_guild
    async def dig_gift(self, interaction: discord.Interaction, user: discord.Member, artifact: str):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.gift_relic,
                interaction.user.id,
                user.id,
                guild_id,
                artifact,
            ))
            if not getattr(result, "success", True):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Gift failed."),
                    ephemeral=True,
                )
                return
            gifted_name = getattr(result, "artifact_name", artifact)
            await safe_followup(
                interaction,
                content=(
                    f"You gifted **{gifted_name}** to **{user.display_name}**! "
                    f"{getattr(result, 'message', '')}"
                ),
            )
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Dig gift error: %s", e)
            await safe_followup(interaction, content="Gift failed.", ephemeral=True)

    # ------------------------------------------------------------------
    # 8. /dig shop — Show dig-specific items
    # ------------------------------------------------------------------

    @dig.command(name="shop", description="Browse the mining shop")
    @require_guild
    async def dig_shop(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            shop = _wrap(await asyncio.to_thread(
                self.dig_service.get_shop, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Dig shop error: %s", e)
            await safe_followup(interaction, content="Shop unavailable.", ephemeral=True)
            return

        embed = discord.Embed(title="Mining Shop", color=0xD4AF37)

        # Consumables — split across fields so the list never exceeds Discord's
        # 1024-char field-value limit (it grows past it on its own).
        consumables = getattr(shop, "consumables", [])
        add_lines_field(embed, "Consumables", [
            f"**{c.get('name', '?')}** — {c.get('price', '?')} {JOPACOIN_EMOTE}: {c.get('description', '')}"
            for c in consumables
        ])

        # Pickaxe upgrades
        upgrades = getattr(shop, "pickaxe_upgrades", [])
        add_lines_field(embed, "Pickaxe Upgrades", [
            f"**{u.get('name', '?')}** — {u.get('price', '?')} {JOPACOIN_EMOTE} "
            f"(Depth {u.get('depth_req', '?')}, Prestige {u.get('prestige_req', 0)})"
            for u in upgrades
        ])

        # Boss-combat gear (Armor / Boots; weapons are the pickaxe row above)
        gear_for_sale = getattr(shop, "gear_for_sale", [])
        add_lines_field(embed, "Boss Gear", [
            f"**{g.get('name', '?')}** — {g.get('price', '?')} {JOPACOIN_EMOTE} "
            f"(Depth {g.get('depth_req', '?')}"
            + (f", Prestige {g.get('prestige_req', 0)}" if g.get('prestige_req', 0) else "")
            + ")"
            for g in gear_for_sale
        ])

        # Inventory count
        inv_count = getattr(shop, "inventory_count", 0)
        embed.set_footer(
            text=(
                f"Your inventory: {inv_count}/{MAX_INVENTORY_SLOTS} items | "
                "Hard Hat/Torch auto-queue; use /dig use <item> for other active items"
            )
        )

        shop_file = None
        try:
            from utils.dig_assets import compose_shop_grid
            shop_file = await asyncio.to_thread(compose_shop_grid)
            if shop_file:
                embed.set_image(url=f"attachment://{shop_file.filename}")
        except Exception:
            pass
        await send_public_or_ephemeral(interaction, embed=embed, file=shop_file)

    # ------------------------------------------------------------------
    # 8b. /dig buy — Buy an item from the shop
    # ------------------------------------------------------------------

    @dig.command(name="buy", description="Buy an item from the mining shop")
    @app_commands.describe(item="Item to buy")
    @app_commands.autocomplete(item=buy_autocomplete)
    @require_guild
    async def dig_buy(self, interaction: discord.Interaction, item: str):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id

        # Gear choice values are encoded as "<slot>:<tier>"; route them to
        # the gear-buy path. Everything else is a consumable.
        if ":" in item:
            slot, _, tier_str = item.partition(":")
            try:
                tier = int(tier_str)
            except ValueError:
                tier = -1
            try:
                if slot in ("weapon", "pickaxe"):
                    result = _wrap(await asyncio.to_thread(
                        self.dig_service.upgrade_pickaxe_to_tier,
                        interaction.user.id, guild_id, tier,
                    ))
                else:
                    result = _wrap(await asyncio.to_thread(
                        self.dig_service.buy_gear,
                        interaction.user.id, guild_id, slot, tier,
                    ))
            except Exception as e:
                logger.error("Dig buy_gear error: %s", e)
                await safe_followup(interaction, content="Purchase failed.", ephemeral=True)
                return
            if not getattr(result, "success", False):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Purchase failed."),
                    ephemeral=True,
                )
                return
            name = getattr(result, "name", item)
            cost = getattr(result, "cost", 0)
            if slot in ("weapon", "pickaxe"):
                await safe_followup(
                    interaction,
                    content=(
                        f"Upgraded your pickaxe to **{name}** for **{cost}** "
                        f"{JOPACOIN_EMOTE}. It is equipped."
                    ),
                    ephemeral=True,
                )
                return
            await safe_followup(
                interaction,
                content=(
                    f"Bought **{name}** for **{cost}** {JOPACOIN_EMOTE}.\n"
                    f"Equip it via `/dig gear`."
                ),
                ephemeral=True,
            )
            return

        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.buy_item, interaction.user.id, guild_id, item
            ))
        except Exception as e:
            logger.error("Dig buy error: %s", e)
            await safe_followup(interaction, content="Purchase failed.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Purchase failed."),
                ephemeral=True,
            )
            return

        item_name = getattr(result, "item", item)
        cost = getattr(result, "cost", 0)
        balance_after = getattr(result, "balance_after", "?")
        auto_queued = bool(getattr(result, "queued", False))
        if item == "streak_charm":
            item_hint = "This charm is passive and triggers automatically."
        elif auto_queued:
            item_hint = "Queued automatically for your next dig."
        else:
            item_hint = f"Use `/dig use {item}` to queue it."
        buy_embed = discord.Embed(
            title=f"Purchased: {item_name}",
            description=(
                f"Cost: **{cost}** {JOPACOIN_EMOTE}\n"
                f"Balance: **{balance_after}** {JOPACOIN_EMOTE}\n\n"
                + item_hint
            ),
            color=0xD4AF37,
        )
        item_file = None
        try:
            from utils.dig_assets import get_item_art
            item_file = await asyncio.to_thread(get_item_art, item)
            if item_file:
                buy_embed.set_thumbnail(url=f"attachment://{item_file.filename}")
        except Exception:
            pass
        await safe_followup(interaction, embed=buy_embed, file=item_file, ephemeral=True)

    # ------------------------------------------------------------------
    # 10. /dig_flex — Show stats and titles
    # ------------------------------------------------------------------

    @dig.command(name="flex", description="Show off your mining stats")
    @require_guild
    async def dig_flex(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            flex = _wrap(await asyncio.to_thread(
                self.dig_service.get_flex_data, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Flex error: %s", e)
            await safe_followup(interaction, content="Flex unavailable.", ephemeral=True)
            return

        if not getattr(flex, "success", False):
            await safe_followup(
                interaction,
                content="You don't have a tunnel yet. Use `/dig go` to start!",
                ephemeral=True,
            )
            return

        depth = getattr(flex, "depth", 0)
        total_digs = getattr(flex, "total_digs", 0)
        total_jc = getattr(flex, "total_jc_earned", 0)
        prestige = getattr(flex, "prestige_level", 0)
        streak = getattr(flex, "streak", 0)
        tunnel_name = getattr(flex, "tunnel_name", "Unknown")
        layer = getattr(flex, "layer", "Dirt")
        titles = getattr(flex, "titles", [])
        prestige_emoji = getattr(flex, "prestige_emoji", "")

        has_anything = depth > 0 or total_digs > 1

        embed = discord.Embed(
            title=f"{interaction.user.display_name}'s Mining Profile",
            color=0xFFD700,
        )

        if not has_anything:
            sad_lines = [
                "Dug once, found nothing but regret.",
                "The tunnel is so shallow a worm filed a noise complaint.",
                "Achievement unlocked: Owning a shovel.",
                "Your tunnel has more cobwebs than depth.",
                "Even the dirt feels sorry for you.",
                "Depth: yes. Impressive: no.",
                "The mine safety inspector gave you a participation trophy.",
                "Your pickaxe is still in the shrinkwrap.",
            ]
            embed.description = f"*{random.choice(sad_lines)}*"
            embed.set_thumbnail(url=interaction.user.display_avatar.url)
            await safe_followup(interaction, embed=embed)
            return

        # Title(s)
        if titles:
            embed.description = f"*\"{' | '.join(titles)}\"*"
        if prestige_emoji:
            embed.description = (embed.description or "") + f"  {prestige_emoji}"

        # Stats
        stats_text = (
            f"Tunnel: **{tunnel_name}**\n"
            f"Depth: **{depth}** ({layer})\n"
            f"Total digs: **{total_digs}**\n"
            f"Total JC earned: **{total_jc}**\n"
            f"Streak: **{streak}** days"
        )
        if prestige:
            stats_text += f"\nPrestige: **{prestige}**"
        embed.add_field(name="Stats", value=stats_text, inline=False)

        embed.set_thumbnail(url=interaction.user.display_avatar.url)
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 10b. /dig prestige — Prestige your tunnel
    # ------------------------------------------------------------------

    @dig.command(name="prestige", description="Prestige your tunnel (reset depth, gain a perk)")
    @require_guild
    async def dig_prestige(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            check = _wrap(await asyncio.to_thread(
                self.dig_service.can_prestige, interaction.user.id, guild_id
            ))
        except Exception as e:
            logger.error("Prestige check error: %s", e, exc_info=True)
            await safe_followup(interaction, content="Prestige check failed.", ephemeral=True)
            return

        can = getattr(check, "can_prestige", False)
        if not can:
            reason = getattr(check, "reason", "You cannot prestige yet.")
            await safe_followup(interaction, content=reason, ephemeral=True)
            return

        prestige_level = getattr(check, "prestige_level", 0)
        new_level = prestige_level + 1
        run_score = getattr(check, "run_score", 0)

        # Build the prestige preview embed
        embed = discord.Embed(
            title=f"Prestige to P{new_level}?",
            description=(
                "This will **reset your tunnel depth to 0** but grant a permanent perk.\n\n"
                f"**Run Score:** {run_score}\n"
            ),
            color=0xFFD700,
        )

        # Show the ascension modifier that will be unlocked at this level
        asc_mod = ASCENSION_MODIFIERS.get(new_level)
        if asc_mod:
            embed.add_field(
                name=f"Ascension Unlock: {asc_mod.name}",
                value=f"Penalty: {asc_mod.penalty}\nReward: {asc_mod.reward}",
                inline=False,
            )

        # Build perk list for the view
        available_perks_raw = getattr(check, "available_perks", []) or []
        if isinstance(available_perks_raw, _DictObj):
            available_perks_raw = available_perks_raw._d if hasattr(available_perks_raw, "_d") else []
        from services.dig_constants import perk_display_name

        perk_dicts = [
            {"id": p, "name": perk_display_name(p)}
            for p in available_perks_raw
        ]

        if not perk_dicts:
            await safe_followup(
                interaction, content="No perks available. You may have unlocked them all.",
                ephemeral=True,
            )
            return

        perks_view = PrestigePerksView(
            self.dig_service, interaction.user.id, guild_id, perk_dicts, new_level=new_level,
        )

        # P8+ mutation selection step
        mutation_info = getattr(check, "mutation_info", None)
        if mutation_info:
            mut_d = mutation_info if isinstance(mutation_info, dict) else (
                mutation_info._d if hasattr(mutation_info, "_d") else {}
            )
            forced = mut_d.get("forced") if isinstance(mut_d, dict) else None
            choices = mut_d.get("choices") if isinstance(mut_d, dict) else None

            if forced and choices:
                forced_d = forced if isinstance(forced, dict) else (
                    forced._d if hasattr(forced, "_d") else {}
                )
                choices_list = choices if isinstance(choices, list) else (
                    choices._d if hasattr(choices, "_d") else []
                )
                # Unwrap _DictObj items in choices_list
                unwrapped_choices = []
                for c in choices_list:
                    if isinstance(c, dict):
                        unwrapped_choices.append(c)
                    elif hasattr(c, "_d"):
                        unwrapped_choices.append(c._d)
                    else:
                        unwrapped_choices.append({"id": str(c), "name": str(c)})

                embed.add_field(
                    name=f"Forced Mutation: {forced_d.get('name', '?')}",
                    value=forced_d.get("description", ""),
                    inline=False,
                )
                mut_lines = [
                    f"**{m.get('name', '?')}** — {m.get('description', '')}"
                    for m in unwrapped_choices
                ]
                embed.add_field(
                    name="Choose a Mutation",
                    value="\n".join(mut_lines) or "No choices available",
                    inline=False,
                )

                # Build a perk selection embed for after mutation choice.
                # Only list the 4 perks the picker will actually offer.
                perks_embed = discord.Embed(
                    title="Choose a Prestige Perk",
                    description="\n".join(
                        f"**{p.get('name', '?')}**" for p in perks_view.perks
                    ),
                    color=0xFFD700,
                )

                mutation_view = MutationSelectionView(
                    self.dig_service, interaction.user.id, guild_id,
                    forced_d, unwrapped_choices, perks_view, perks_embed,
                )
                mutation_view.message = await safe_followup(
                    interaction, embed=embed, view=mutation_view
                )
                return

        # No mutations — go straight to perk selection. Only list the 4
        # perks the picker will actually offer.
        embed.add_field(
            name="Choose a Perk",
            value="\n".join(f"**{p.get('name', '?')}**" for p in perks_view.perks),
            inline=False,
        )
        perks_view.message = await safe_followup(interaction, embed=embed, view=perks_view)

    # ------------------------------------------------------------------
    # 11. /dig_abandon — Abandon tunnel
    # ------------------------------------------------------------------

    @dig.command(name="abandon", description="Abandon your tunnel (partial refund)")
    @require_guild
    async def dig_abandon(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id

        try:
            preview = _wrap(await asyncio.to_thread(
                self.dig_service.preview_abandon, interaction.user.id, guild_id
            ))
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Abandon preview error: %s", e)
            await interaction.response.send_message("Failed.", ephemeral=True)
            return

        refund = getattr(preview, "refund", 0)
        embed = discord.Embed(
            title="Abandon Tunnel?",
            description=(
                f"This will **permanently destroy** your tunnel.\n"
                f"Refund: **{refund}** {JOPACOIN_EMOTE}\n\n"
                "Are you sure?"
            ),
            color=0xFF0000,
        )
        view = ConfirmAbandonView(interaction.user.id, refund)
        await interaction.response.send_message(embed=embed, view=view)
        await view.wait()

        if view.value:
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.abandon_tunnel, interaction.user.id, guild_id
                ))
                actual_refund = getattr(result, "refund", refund)
                await interaction.edit_original_response(
                    content=f"Tunnel abandoned. You received **{actual_refund}** {JOPACOIN_EMOTE}.",
                    embed=None,
                    view=None,
                )
            except Exception as e:
                logger.error("Abandon error: %s", e)
                await interaction.edit_original_response(
                    content="Abandon failed.", embed=None, view=None
                )
        else:
            await interaction.edit_original_response(
                content="Abandon cancelled.", embed=None, view=None
            )

    # ------------------------------------------------------------------
    # 12. /dig upgrade — REMOVED. Pickaxes are now part of the boss-gear
    # system; buy them via `/dig shop` (or, soon, the gear-buy UI) and
    # view/equip them via `/dig gear`. The upgrade_pickaxe service is
    # kept and used by upcoming shop integration.
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # 13. /dig_trap — Set a trap
    # ------------------------------------------------------------------

    @dig.command(name="trap", description="Set a trap in your tunnel")
    @require_guild
    async def dig_trap(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.set_trap, interaction.user.id, guild_id
            ))
            if not getattr(result, "success", True):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Failed to set trap."),
                    ephemeral=True,
                )
                return
            cost = getattr(result, "cost", 0)
            msg = "Trap set!"
            if cost:
                msg += f" (Cost: {cost} {JOPACOIN_EMOTE})"
            await safe_followup(interaction, content=msg)
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
        except Exception as e:
            logger.error("Trap error: %s", e)
            await safe_followup(interaction, content="Failed to set trap.", ephemeral=True)

    # ------------------------------------------------------------------
    # 14. /dig_insure — Buy insurance
    # ------------------------------------------------------------------

    @dig.command(name="insure", description="Buy cave-in insurance")
    @require_guild
    async def dig_insure(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.buy_insurance, interaction.user.id, guild_id
            ))
        except ValueError as e:
            await safe_followup(interaction, content=str(e), ephemeral=True)
            return
        except Exception as e:
            logger.error("Insurance error: %s", e)
            await safe_followup(interaction, content="Failed to buy insurance.", ephemeral=True)
            return

        if not getattr(result, "success", False):
            await safe_followup(
                interaction,
                content=getattr(result, "error", "Failed to buy insurance."),
                ephemeral=True,
            )
            return

        cost = getattr(result, "cost", 0)
        await safe_followup(
            interaction,
            content=(
                f"Insurance purchased for **{cost}** {JOPACOIN_EMOTE}! "
                f"Duration: 24 hours."
            ),
        )

    # ------------------------------------------------------------------
    # 15. /dig_inventory — View items
    # ------------------------------------------------------------------

    @dig.command(name="inventory", description="View your mining inventory")
    @require_guild
    async def dig_inventory(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        await safe_defer(interaction)

        guild_id = interaction.guild.id
        try:
            items = await asyncio.to_thread(
                self.dig_service.get_inventory, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Inventory error: %s", e)
            await safe_followup(interaction, content="Inventory unavailable.", ephemeral=True)
            return

        embed = discord.Embed(title="Mining Inventory", color=0x8B4513)
        # Pickaxe thumbnail
        inv_pickaxe_file = None
        try:
            from utils.dig_assets import get_pickaxe_art
            tunnel_info = await asyncio.to_thread(
                self.dig_service.dig_repo.get_tunnel, interaction.user.id, guild_id
            )
            tier_idx = dict(tunnel_info).get("pickaxe_tier", 0) if tunnel_info else 0
            inv_pickaxe_file = await asyncio.to_thread(get_pickaxe_art, tier_idx)
            if inv_pickaxe_file:
                embed.set_thumbnail(url=f"attachment://{inv_pickaxe_file.filename}")
        except Exception:
            pass
        if items:
            for item in items[:5]:
                name = item.get("name", "Unknown")
                queued = item.get("queued", False)
                desc = item.get("description", "")
                status = " [QUEUED]" if queued else ""
                embed.add_field(
                    name=f"{name}{status}",
                    value=desc or "No description",
                    inline=False,
                )
            embed.set_footer(text=f"{len(items)}/{MAX_INVENTORY_SLOTS} slots used")
        else:
            embed.description = "Your inventory is empty. Visit `/dig shop` to buy items."
            embed.set_footer(text=f"0/{MAX_INVENTORY_SLOTS} slots used")

        await send_public_or_ephemeral(interaction, embed=embed, file=inv_pickaxe_file)

    # ------------------------------------------------------------------
    # 16. /dig artifacts — View the artifact catalog and your collection
    # ------------------------------------------------------------------

    @dig.command(name="artifacts", description="View all artifacts and the ones you own")
    @app_commands.checks.cooldown(1, 10)
    @require_guild
    async def dig_artifacts(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        try:
            owned_rows = await asyncio.to_thread(
                self.dig_service.get_artifacts_for_catalog,
                interaction.user.id,
                guild_id,
            )
            embeds = _build_artifact_catalog_embeds(owned_rows)
        except Exception as e:
            logger.error("Artifact catalog error: %s", e)
            await safe_followup(
                interaction,
                content="Artifact catalog unavailable.",
                ephemeral=True,
            )
            return

        for embed in embeds:
            await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 16a. /dig gear — Manage boss-combat gear loadout
    # ------------------------------------------------------------------

    @dig.command(name="gear", description="Manage your boss-combat gear")
    @require_guild
    async def dig_gear(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return
        player = await _check_registered(interaction, self.bot)
        if not player:
            return
        await safe_defer(interaction)
        guild_id = interaction.guild.id
        try:
            loadout = await asyncio.to_thread(
                self.dig_service.get_loadout, interaction.user.id, guild_id
            )
            inventory = await asyncio.to_thread(
                self.dig_service.get_inventory_gear, interaction.user.id, guild_id
            )
        except Exception as e:
            logger.error("Gear panel error: %s", e)
            await safe_followup(interaction, content="Gear panel unavailable.", ephemeral=True)
            return
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        total_cost = await asyncio.to_thread(
            self.dig_service.compute_repair_all_cost,
            interaction.user.id,
            guild_id,
        )
        embed = _build_gear_embed(loadout, inventory, damaged, self.dig_service)
        view = GearPanelView(
            self.dig_service, interaction.user.id, guild_id,
            repair_all_cost=total_cost, has_damaged_gear=bool(damaged),
        )
        await safe_followup(interaction, embed=embed, view=view)

    # ------------------------------------------------------------------
    # 16b. /dig weather — View today's layer weather
    # ------------------------------------------------------------------

    @dig.command(name="weather", description="View today's layer weather conditions")
    @require_guild
    async def dig_weather(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        weather = await asyncio.to_thread(self.dig_service.get_weather, guild_id)

        if not weather:
            await safe_followup(interaction, content="No weather today — skies are clear.", ephemeral=True)
            return

        embed = discord.Embed(
            title="Today's Layer Weather",
            description="Conditions shift daily.",
            color=0x5865F2,
        )
        for w in weather:
            layer = w.get("layer", "Unknown")
            name = w.get("name", "Unknown")
            desc = w.get("description", "")
            effects = w.get("effects", {})

            fx_lines = []
            if effects.get("cave_in_bonus"):
                val = effects["cave_in_bonus"]
                fx_lines.append("cave-in risk surges" if val > 0 else "cave-in risk eases")
            if effects.get("jc_multiplier"):
                val = effects["jc_multiplier"]
                fx_lines.append("ore veins are rich" if val > 0 else "ore veins are thin")
            if effects.get("jc_bonus"):
                val = effects["jc_bonus"]
                fx_lines.append("seams glitter" if val > 0 else "seams run dry")
            if effects.get("advance_bonus"):
                val = effects["advance_bonus"]
                fx_lines.append("ground is soft" if val > 0 else "ground is dense")
            if effects.get("event_chance_multiplier"):
                val = effects["event_chance_multiplier"]
                fx_lines.append("the deep stirs" if val > 0 else "the deep is quiet")
            if effects.get("artifact_multiplier") and effects["artifact_multiplier"] != 1.0:
                val = effects["artifact_multiplier"]
                fx_lines.append("relics surface more often" if val > 1.0 else "relics are scarce")
            if effects.get("luminosity_drain_multiplier"):
                fx_lines.append("darkness drains lanterns quickly")

            fx_str = ", ".join(fx_lines) if fx_lines else "no notable effect"

            embed.add_field(
                name=f"{layer} — {name}",
                value=f"*{desc}*\n*{fx_str}*",
                inline=False,
            )

        embed.set_footer(text="Weather affects all diggers in that layer today.")
        await safe_followup(interaction, embed=embed)

    # ------------------------------------------------------------------
    # 17. /dig admin — Admin maintenance
    # ------------------------------------------------------------------

    @admin.command(name="resetcooldown", description="Reset a player's free dig cooldown (Admin only)")
    @app_commands.describe(user="The player whose cooldown to reset")
    @require_guild
    async def dig_resetcooldown(self, interaction: discord.Interaction, user: discord.User):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        result = await asyncio.to_thread(self.dig_service.reset_dig_cooldown, user.id, guild_id)

        if not result.get("success"):
            await safe_followup(interaction, content=result.get("error", "Failed."), ephemeral=True)
            return

        await safe_followup(interaction, content=f"Reset free dig cooldown for {user.mention}.", ephemeral=True)

    @admin.command(name="forceevent", description="Force next dig to trigger an event (Admin only)")
    @app_commands.describe(user="The player whose next dig gets an event")
    @require_guild
    async def dig_forceevent(self, interaction: discord.Interaction, user: discord.User):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return
        # Store on the service so the next dig() for this user forces an event
        if not hasattr(self.dig_service, "_force_event_for"):
            self.dig_service._force_event_for = set()
        guild_id = interaction.guild.id
        self.dig_service._force_event_for.add((user.id, guild_id))
        await interaction.response.send_message(f"Next dig for {user.mention} will force an event.", ephemeral=True)

    @admin.command(name="setdepth", description="Set a player's tunnel depth (Admin only)")
    @app_commands.describe(user="The player", depth="New depth value")
    @require_guild
    async def dig_setdepth(self, interaction: discord.Interaction, user: discord.User, depth: int):
        if not has_admin_permission(interaction):
            await interaction.response.send_message("Admin only.", ephemeral=True)
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        tunnel = await asyncio.to_thread(self.dig_service.dig_repo.get_tunnel, user.id, guild_id)
        if not tunnel:
            await safe_followup(interaction, content="That player doesn't have a tunnel.", ephemeral=True)
            return

        depth = max(0, depth)
        await asyncio.to_thread(self.dig_service.dig_repo.update_tunnel, user.id, guild_id, depth=depth)
        await asyncio.to_thread(self.dig_service.dig_repo.update_tunnel, user.id, guild_id, last_dig_at=0)
        await safe_followup(
            interaction,
            content=f"Set {user.mention} to depth **{depth}** and reset cooldown.",
            ephemeral=True,
        )

    # ------------------------------------------------------------------
    # 18. /dig miner profile/about/build/autobuy — Miner customization
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # /dig miner subgroup — profile, about, build, autobuy
    # ------------------------------------------------------------------
    miner = app_commands.Group(name="miner", description="Miner profile and S stats", parent=dig)

    @miner.command(name="profile", description="View your miner profile and S stats")
    @require_guild
    async def dig_profile(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        result = await asyncio.to_thread(
            self.dig_service.get_miner_profile,
            interaction.user.id,
            guild_id,
        )
        if not result.get("success"):
            await safe_followup(interaction, content=result.get("error", "No profile."), ephemeral=True)
            return

        embed = discord.Embed(
            title=f"{interaction.user.display_name} - Miner Profile",
            color=0x5865F2,
        )
        embed.description = _backstory_text(result)
        embed.add_field(
            name="S Stats",
            value=_format_s_stats(result.get("stats", {}), result.get("effects", {})),
            inline=False,
        )
        embed.add_field(
            name="Auto-Buy",
            value=_format_auto_buy_settings(result.get("auto_buy", {})),
            inline=False,
        )
        embed.set_footer(text="Backstory locks after you set it. Boss first clears grant one extra S point.")
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @miner.command(name="about", description="Set your miner backstory once")
    @app_commands.describe(
        backstory="Short backstory blurb for the AI Dungeon Master",
    )
    @require_guild
    async def dig_about(
        self,
        interaction: discord.Interaction,
        backstory: str,
    ):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        guild_id = interaction.guild.id
        result = await asyncio.to_thread(
            self.dig_service.set_miner_profile,
            interaction.user.id,
            guild_id,
            backstory=backstory,
        )
        if not result.get("success"):
            await interaction.response.send_message(result.get("error", "Profile update failed."), ephemeral=True)
            return

        embed = discord.Embed(
            title="Backstory Locked In",
            description=_backstory_text(result),
            color=0x5865F2,
        )
        embed.set_footer(text="This cannot be changed later.")
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @miner.command(name="build", description="Spend unallocated points on Strength, Smarts, and Stamina")
    @app_commands.describe(
        strength="Points to add. Increases how far you dig each action.",
        smarts="Points to add. Helps you read the stone and avoid collapses.",
        stamina="Points to add. Keeps you digging longer between rests.",
    )
    @require_guild
    async def dig_build(
        self,
        interaction: discord.Interaction,
        strength: int = 0,
        smarts: int = 0,
        stamina: int = 0,
    ):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        result = await asyncio.to_thread(
            self.dig_service.set_miner_stats,
            interaction.user.id,
            guild_id,
            strength=strength,
            smarts=smarts,
            stamina=stamina,
        )
        if not result.get("success"):
            await safe_followup(interaction, content=result.get("error", "Build update failed."), ephemeral=True)
            return

        embed = discord.Embed(
            title="S Points Spent",
            description=_format_s_stats(result.get("stats", {}), result.get("effects", {})),
            color=0x5865F2,
        )
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @miner.command(name="autobuy", description="Auto-buy Torch and/or Hard Hat for each dig")
    @app_commands.describe(
        item="Which auto-buy setting to update",
        enabled="Whether to auto-buy this item on each real dig",
    )
    @app_commands.choices(item=[
        app_commands.Choice(name="Torch", value="torch"),
        app_commands.Choice(name="Hard Hat", value="hard_hat"),
        app_commands.Choice(name="Both", value="both"),
    ])
    @require_guild
    async def dig_autobuy(
        self,
        interaction: discord.Interaction,
        item: str,
        enabled: bool,
    ):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        updates = {
            "torch": enabled if item in ("torch", "both") else None,
            "hard_hat": enabled if item in ("hard_hat", "both") else None,
        }
        result = await asyncio.to_thread(
            self.dig_service.set_miner_auto_buy,
            interaction.user.id,
            guild_id,
            **updates,
        )
        if not result.get("success"):
            await safe_followup(
                interaction,
                content=result.get("error", "Auto-buy update failed."),
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title="Dig Auto-Buy Updated",
            description=_format_auto_buy_settings(result.get("auto_buy", {})),
            color=0x5865F2,
        )
        embed.set_footer(text="Auto-buy spends JC only when an actual dig starts.")
        await safe_followup(interaction, embed=embed, ephemeral=True)

    # ------------------------------------------------------------------
    # 19. /dig guide — Paginated help
    # ------------------------------------------------------------------

    @dig.command(name="guide", description="Learn how to dig")
    async def dig_guide(self, interaction: discord.Interaction):
        if not await require_dig_channel(interaction):
            return

        player = await _check_registered(interaction, self.bot)
        if not player:
            return

        view = DigGuideView()
        await interaction.response.send_message(embed=GUIDE_PAGES[0], view=view)


# ---------------------------------------------------------------------------
# Embed builder for normal dig results
# ---------------------------------------------------------------------------

def _build_dig_embed(result: object, user: discord.User | discord.Member) -> tuple[discord.Embed, str | None, int, list[str]]:
    """Build a rich embed for a normal dig result. Returns (embed, layer_name, pickaxe_tier, items_used_ids)."""
    depth = getattr(result, "depth", 0) or getattr(result, "depth_after", 0)
    tunnel_name = getattr(result, "tunnel_name", "Tunnel")
    pickaxe_tier = getattr(result, "pickaxe_tier", 0) or 0

    # Determine layer for embed color
    layer_def = get_layer_def(depth)
    layer_name = layer_def.name if layer_def else None

    # ~20% chance of a "Dig Dug" themed title
    if random.random() < 0.20:
        title = f"{random.choice(DIG_DUG_TITLES)} \u2014 Depth {depth}"
    else:
        title = f"{tunnel_name} \u2014 Depth {depth}"

    embed = discord.Embed(
        title=title,
        color=_layer_color(layer_name),
    )

    # LLM narrative (DM Mode) — shown as first field when present
    llm_narrative = getattr(result, "llm_narrative", None)
    if llm_narrative:
        embed.add_field(name="\u200b", value=f"*{llm_narrative}*", inline=False)

    # Blocks gained and JC earned (skip misleading "+0" during cave-ins)
    cave_in = getattr(result, "cave_in", False)
    blocks = getattr(result, "advance", 0)
    jc = getattr(result, "jc_earned", 0)
    bankruptcy_penalty = getattr(result, "bankruptcy_penalty", 0) or 0
    if not cave_in or blocks > 0 or jc > 0:
        progress_value = f"+{blocks} blocks | +{jc} {JOPACOIN_EMOTE}"
        if bankruptcy_penalty > 0:
            progress_value += f"\n−{bankruptcy_penalty} {JOPACOIN_EMOTE} withheld while bankrupt"
        embed.add_field(
            name="Progress",
            value=progress_value,
            inline=False,
        )

    if getattr(result, "relic_trim_notice", False):
        embed.add_field(
            name="Relic slots capped",
            value=(
                "Relics are now capped at **6**. Your extra relics were unequipped "
                "and are safe in your inventory — re-pick with `/dig gear`."
            ),
            inline=False,
        )

    # Cave-in
    cave_in_detail = getattr(result, "cave_in_detail", None)
    if cave_in and cave_in_detail:
        if isinstance(cave_in_detail, dict):
            block_loss = cave_in_detail.get("block_loss", "?")
            jc_lost = cave_in_detail.get("jc_lost", 0)
            gear_broken = cave_in_detail.get("gear_broken") or []
        else:
            block_loss = getattr(cave_in_detail, "block_loss", "?")
            jc_lost = getattr(cave_in_detail, "jc_lost", 0)
            gear_broken = getattr(cave_in_detail, "gear_broken", None) or []
        llm_cave_in = getattr(result, "llm_cave_in_flavor", None)
        cave_in_type = getattr(cave_in_detail, "type", "") if not isinstance(cave_in_detail, dict) else cave_in_detail.get("type", "")
        if llm_cave_in:
            message = llm_cave_in
        elif llm_narrative:
            # DM narrative tells the story — show only the mechanical consequence
            consequence = {
                "stun": "Stunned — next dig has longer cooldown!",
                "injury": "Injured — reduced digging for several digs!",
                "medical_bill": "",  # JC loss shown via jc_lost below
                "gear_nick": "Gear took a hit — durability lost.",
                "spilled_satchel": "An item slipped from your pack.",
                "snuffed_light": "The light dimmed.",
                "cracked_hat": "Hard hat charge damaged.",
                "catastrophic": "Tunnel collapsed — milestone fallback.",
            }.get(cave_in_type, "")
            message = consequence
        else:
            message = getattr(cave_in_detail, "message", "")
        cave_in_text = f"Lost **{block_loss}** blocks"
        if jc_lost:
            cave_in_text += f" and **{jc_lost}** {JOPACOIN_EMOTE}"
        if message:
            cave_in_text += f". {message}"
        embed.add_field(
            name="CATASTROPHIC CAVE-IN!" if cave_in_type == "catastrophic" else "Cave-in!",
            value=cave_in_text,
            inline=False,
        )
        if gear_broken:
            embed.add_field(
                name="Gear Broken",
                value=(
                    "\n".join(f"• **{name}**" for name in gear_broken)
                    + "\nThese items stay equipped with their effects disabled until repaired. "
                    "Use **Repair All** in `/dig gear`."
                ),
                inline=False,
            )

    # Milestone bonus
    milestone = getattr(result, "milestone_bonus", 0)
    if milestone:
        embed.add_field(
            name="DIG DUG! Milestone!",
            value=f"+{milestone} {JOPACOIN_EMOTE}",
            inline=False,
        )

    # Streak bonus
    streak_bonus = getattr(result, "streak_bonus", 0)
    if streak_bonus:
        embed.add_field(
            name="Streak Bonus",
            value=f"+{streak_bonus} {JOPACOIN_EMOTE}",
            inline=True,
        )

    if getattr(result, "streak_charm_used", False):
        embed.add_field(
            name="Streak Charm",
            value="Saved your daily streak after the missed day.",
            inline=False,
        )

    # Artifact found
    artifact = getattr(result, "artifact", None)
    if artifact:
        a_name = getattr(artifact, "name", "?") if not isinstance(artifact, str) else artifact
        a_desc = getattr(artifact, "description", "") if not isinstance(artifact, str) else ""
        embed.add_field(
            name="Artifact Found!",
            value=f"**{a_name}**" + (f" — {a_desc}" if a_desc else ""),
            inline=False,
        )

    # Event (with ASCII art for simple events).
    # Skip this field for choice/boon events — they get their own encounter
    # embed with art, so showing the text here would be a duplicate.
    event = getattr(result, "event", None)
    if event:
        event_dict = event if isinstance(event, dict) else (event._d if hasattr(event, "_d") else None)
        has_encounter_ui = isinstance(event_dict, dict) and (
            event_dict.get("safe_option") or event_dict.get("boon_options")
        )
        if has_encounter_ui:
            event = None  # suppress from stats embed — encounter UI will show it

    if event:
        # Use LLM event flavor if available, otherwise stock description
        llm_event_flavor = getattr(result, "llm_event_flavor", None)
        if isinstance(event, str):
            e_desc = llm_event_flavor or event
            e_art = None
        else:
            e_desc = llm_event_flavor or pick_description(event) or "Something happens..."
            if isinstance(event, dict):
                e_art = event.get("ascii_art")
            elif hasattr(event, "_d") and isinstance(event._d, dict):
                e_art = event._d.get("ascii_art")
            else:
                e_art = getattr(event, "ascii_art", None)
        event_text = e_desc
        if e_art:
            event_text = f"```\n{e_art}\n```\n{e_desc}"
        embed.add_field(
            name="\u200b",
            value=event_text,
            inline=False,
        )

    # Sonar Pulse: surface the skipped-event flavor.
    if getattr(result, "sonar_skipped", False):
        skipped = getattr(result, "event_preview", None)
        skipped_d = skipped if isinstance(skipped, dict) else (
            skipped._d if hasattr(skipped, "_d") else None
        )
        if isinstance(skipped_d, dict) and skipped_d.get("name"):
            embed.add_field(
                name="​",
                value=(
                    f"The rumble of *{skipped_d['name']}* passed you by, "
                    "harmless this time."
                ),
                inline=False,
            )
        else:
            embed.add_field(
                name="​",
                value="The cavern stirred but settled without incident.",
                inline=False,
            )
    else:
        # Lantern / Sonar Pulse preview of what stirs ahead.
        preview = getattr(result, "event_preview", None)
        preview_d = preview if isinstance(preview, dict) else (
            preview._d if hasattr(preview, "_d") else None
        )
        if isinstance(preview_d, dict) and preview_d.get("name"):
            embed.add_field(
                name="​",
                value=f"Stirring ahead: *{preview_d['name']}*.",
                inline=False,
            )

    # Lantern boss-approach scout
    scout = getattr(result, "boss_scout", None)
    scout_d = scout if isinstance(scout, dict) else (
        scout._d if hasattr(scout, "_d") else None
    )
    if isinstance(scout_d, dict) and scout_d.get("blocks_until"):
        embed.add_field(
            name="​",
            value=(
                f"Something looms {scout_d['blocks_until']} blocks deeper."
            ),
            inline=False,
        )

    # Items used
    items_used = getattr(result, "items_used", None)
    if items_used:
        item_names = ", ".join(str(i) for i in items_used)
        embed.add_field(name="Items Used", value=item_names, inline=True)

    auto_purchases = getattr(result, "auto_purchases", None) or []
    auto_lines = []
    for entry in auto_purchases:
        data = entry if isinstance(entry, dict) else (
            entry._d if hasattr(entry, "_d") else {}
        )
        status = data.get("status")
        item = data.get("item", data.get("type", "Item"))
        if status == "purchased":
            auto_lines.append(
                f"{item}: bought ({data.get('cost', 0)} {JOPACOIN_EMOTE})"
            )
        elif status == "queued_from_inventory":
            auto_lines.append(f"{item}: from inventory")
        elif status == "skipped_insufficient_balance":
            auto_lines.append(f"{item}: skipped (need {data.get('cost', 0)} JC)")
        elif status == "skipped_inventory_full":
            auto_lines.append(f"{item}: skipped (inventory full)")
        elif status == "skipped_error":
            auto_lines.append(f"{item}: skipped")
    if auto_lines:
        embed.add_field(name="Auto-Buy", value="\n".join(auto_lines), inline=True)

    # Luminosity bar (only shown when draining / below max)
    lum_info = getattr(result, "luminosity_info", None)
    if lum_info:
        lum_after = lum_info.get("luminosity_after", 100) if isinstance(lum_info, dict) else getattr(lum_info, "luminosity_after", 100)
        lum_drained = lum_info.get("drained", 0) if isinstance(lum_info, dict) else getattr(lum_info, "drained", 0)
        if lum_drained > 0 or lum_after < 100:
            filled = max(0, lum_after // 10)
            empty = 10 - filled
            bar = "\u2588" * filled + "\u2591" * empty
            level_name = lum_info.get("level", "bright") if isinstance(lum_info, dict) else getattr(lum_info, "level", "bright")
            level_label = {"bright": "Bright", "dim": "Dim", "dark": "Dark", "pitch_black": "Pitch Black"}.get(level_name, "")
            lum_text = f"`[{bar}]` {lum_after}% — {level_label}"
            if lum_drained > 0:
                lum_text += f" (-{lum_drained})"
            embed.add_field(name="Luminosity", value=lum_text, inline=False)

    # Corruption effect (P6+)
    corruption = getattr(result, "corruption", None)
    if corruption:
        corr_d = corruption if isinstance(corruption, dict) else (corruption._d if hasattr(corruption, "_d") else {})
        corr_desc = corr_d.get("description", "") if isinstance(corr_d, dict) else ""
        if corr_desc:
            embed.add_field(name="Corruption", value=corr_desc, inline=False)

    # Footer — user + tip
    tip = ""
    if depth == 69:
        tip = "Nice."
    elif random.random() < 0.25:
        tip = random.choice(DIG_DUG_FOOTERS)
    else:
        tip = getattr(result, "tip", "") or _tip(0)

    # Active mutations footer (P8+)
    mutations = getattr(result, "mutations", None)
    if mutations and isinstance(mutations, (list, tuple)):
        mut_names = [str(m) for m in mutations]
        tip = f"Mutations: {', '.join(mut_names)}" + (f" | {tip}" if tip else "")

    # LLM callback reference (appended to footer)
    llm_callback = getattr(result, "llm_callback", None)
    if llm_callback:
        tip = f"{tip} | {llm_callback}" if tip else llm_callback

    embed.set_footer(text=tip)
    embed.set_author(name=user.display_name, icon_url=user.display_avatar.url)

    items_ids = list(getattr(result, "items_used_ids", None) or [])
    return embed, layer_name, pickaxe_tier, items_ids


async def _attach_layer_thumbnail(embed: discord.Embed, layer_name: str | None) -> discord.File | None:
    """Fetch layer thumbnail in a thread and attach to embed."""
    if not layer_name:
        return None
    try:
        from utils.dig_assets import get_layer_thumbnail
        layer_file = await asyncio.to_thread(get_layer_thumbnail, layer_name)
        if layer_file:
            embed.set_thumbnail(url=f"attachment://{layer_file.filename}")
            return layer_file
    except Exception:
        logger.debug("Layer thumbnail failed for %s", layer_name)
    return None


async def _attach_pickaxe_footer(embed: discord.Embed, pickaxe_tier: int) -> discord.File | None:
    """Add the pickaxe icon to the embed footer (preserving existing footer text)."""
    try:
        from utils.dig_assets import get_pickaxe_art
        pickaxe_file = await asyncio.to_thread(get_pickaxe_art, pickaxe_tier)
        if pickaxe_file:
            footer_text = embed.footer.text if embed.footer else ""
            embed.set_footer(text=footer_text, icon_url=f"attachment://{pickaxe_file.filename}")
            return pickaxe_file
    except Exception:
        pass
    return None


async def _attach_items_strip(embed: discord.Embed, items_ids: list[str]) -> discord.File | None:
    """Compose item icons into a strip and attach as embed image."""
    if not items_ids:
        return None
    try:
        from utils.dig_assets import compose_items_used
        items_file = await asyncio.to_thread(compose_items_used, items_ids)
        if items_file:
            embed.set_image(url=f"attachment://{items_file.filename}")
            return items_file
    except Exception:
        pass
    return None


# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

async def setup(bot: commands.Bot):
    dig_service = getattr(bot, "dig_service", None)
    if dig_service is None:
        raise RuntimeError("Dig service not registered on bot.")
    dig_flavor_service = getattr(bot, "dig_flavor_service", None)
    await bot.add_cog(DigCommands(bot, dig_service, dig_flavor_service))
