"""Views for dig events: choice/complex encounters and boon selection."""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from commands.dig_helpers._shared import _splash_aftermath_lines, _wrap
from services.dig_constants import LUMINOSITY_PITCH_BLACK, pick_description
from services.dig_constants import get_layer as get_layer_def
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.dig_flavor_service import DigFlavorService
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")


class EventEncounterView(discord.ui.View):
    """Interactive view for choice/complex events with safe and risky buttons."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        event_data: dict,
        luminosity: int = 100,
        target_channel: discord.abc.Messageable | None = None,
        dig_flavor_service: DigFlavorService | None = None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.event_data = event_data
        self.target_channel = target_channel
        self.dig_flavor_service = dig_flavor_service
        # Guards against a second resolution: resolve_event applies the outcome
        # (crediting JC) with no consumed-state of its own, so without this a user
        # could click a guaranteed-success option repeatedly within the 60s window
        # and bank the reward N times.
        self._resolved = False
        safe_label = "Play it safe"
        risky_label = "Take the risk"
        if isinstance(event_data, dict):
            safe_opt = event_data.get("safe_option")
            risky_opt = event_data.get("risky_option")
            if isinstance(safe_opt, dict):
                safe_label = safe_opt.get("label", safe_label)
            if isinstance(risky_opt, dict):
                risky_label = risky_opt.get("label", risky_label)
            # Add desperate button if event has a desperate option
            desperate_opt = event_data.get("desperate_option")
            if isinstance(desperate_opt, dict) and desperate_opt:
                desperate_label = desperate_opt.get("label", "Desperate gamble")[:80]
                desperate_btn = discord.ui.Button(
                    label=desperate_label,
                    style=discord.ButtonStyle.danger,
                    emoji="\U0001f480",
                    custom_id="event_desperate",
                )
                desperate_btn.callback = self._desperate_callback
                self.add_item(desperate_btn)
        # Pitch black: disable safe option, force risky
        if luminosity <= LUMINOSITY_PITCH_BLACK:
            safe_label = "Darkness consumes safety"
            self.safe_btn.disabled = True
            self.safe_btn.style = discord.ButtonStyle.secondary
        self.safe_btn.label = safe_label[:80]
        self.risky_btn.label = risky_label[:80]

    async def on_timeout(self) -> None:
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        try:
            if hasattr(self, "message") and self.message is not None:
                await self.message.edit(content="*The moment passed.*", view=self)
        except (discord.NotFound, discord.HTTPException):
            pass

    async def _send_result(self, interaction: discord.Interaction, embed: discord.Embed) -> None:
        if self.target_channel is not None:
            try:
                await self.target_channel.send(embed=embed)
                return
            except Exception as exc:
                logger.warning("Choice-event result send to dig channel failed: %s", exc)
        await interaction.followup.send(embed=embed)

    async def _handle_choice(self, interaction: discord.Interaction, choice: str) -> None:
        """Resolve one event choice, guarding against re-entry.

        The ownership check and the ``_resolved`` check-and-set run with no
        ``await`` between them, so under asyncio's single-threaded loop a burst of
        rapid clicks can never each reach ``_resolve`` \u2014 only the first does.
        """
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your event.", ephemeral=True)
            return
        if self._resolved:
            await interaction.response.send_message(
                "You've already resolved this event.", ephemeral=True
            )
            return
        self._resolved = True
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        await safe_defer(interaction)
        result = await self._resolve(choice)
        await self._send_result(interaction, result)
        self.stop()

    async def _desperate_callback(self, interaction: discord.Interaction):
        await self._handle_choice(interaction, "desperate")

    @discord.ui.button(label="Safe", style=discord.ButtonStyle.secondary, emoji="\U0001f6e1\ufe0f")
    async def safe_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_choice(interaction, "safe")

    @discord.ui.button(label="Risky", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def risky_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_choice(interaction, "risky")

    async def _resolve(self, choice: str) -> discord.Embed:
        """Resolve event choice via service layer (handles chaining, cruel echoes, logging)."""
        event = self.event_data
        if not isinstance(event, dict):
            return discord.Embed(title="Event", description="Nothing happened.", color=0x808080)

        event_id = event.get("id", "")
        result = _wrap(await asyncio.to_thread(
            self.dig_service.resolve_event, self.user_id, self.guild_id, event_id, choice
        ))
        if not getattr(result, "success", True):
            return discord.Embed(
                title="Event Failed",
                description=getattr(result, "error", "Something went wrong."),
                color=0xFF4444,
            )

        jc = getattr(result, "jc_delta", 0)
        advance = getattr(result, "depth_delta", 0)
        msg = getattr(result, "message", "Something happened.")
        succeeded = getattr(result, "succeeded", True)
        cave_in = getattr(result, "cave_in", False)
        cruel = getattr(result, "cruel_echoes", False)
        streak_loss = getattr(result, "streak_loss", 0) or 0
        curse_applied = getattr(result, "curse_applied", None)
        balance_after = getattr(result, "balance_after", None)

        color = 0xFF4444 if (not succeeded or cruel or cave_in) else 0x00FF00
        embed = discord.Embed(
            description=msg,
            color=color,
        )
        parts = []
        if advance != 0:
            parts.append(f"{'+'if advance > 0 else ''}{advance} blocks")
        if jc != 0:
            parts.append(f"{'+'if jc > 0 else ''}{jc} {JOPACOIN_EMOTE}")
        if cave_in:
            parts.append("Cave-in triggered!")
        if streak_loss > 0:
            day_word = "day" if streak_loss == 1 else "days"
            parts.append(f"-{streak_loss} streak {day_word}")
        if parts:
            embed.add_field(name="Outcome", value=" | ".join(parts), inline=False)

        gear_drop = getattr(result, "gear_drop", None)
        if gear_drop:
            gear_d = getattr(gear_drop, "_d", gear_drop)
            if isinstance(gear_d, dict):
                slot = str(gear_d.get("slot", "gear")).replace("_", " ").title()
                durability = str(gear_d.get("durability", 0))
                if gear_d.get("max_durability") is not None:
                    durability += f"/{gear_d['max_durability']}"
                effect = gear_d.get("effect")
                effect_line = f"\n{effect}" if effect else ""
                embed.add_field(
                    name="Gear Drop",
                    value=(
                        f"**{gear_d.get('name', 'Gear')}**\n"
                        f"Slot: {slot}\n"
                        f"Durability: {durability}{effect_line}"
                    ),
                    inline=False,
                )

        # Fail loud — the player must see what a failed risky pick cost.
        # Curse: surface the hex name + how many digs it lingers.
        if curse_applied:
            # ``result`` is _wrap'd, so curse_applied is a _DictObj — unwrap it.
            curse_d = getattr(curse_applied, "_d", curse_applied)
            if not isinstance(curse_d, dict):
                curse_d = {}
            digs = curse_d.get("duration_digs", 0)
            dig_word = "dig" if digs == 1 else "digs"
            embed.add_field(
                name=f"Curse: {curse_d.get('name', 'a hex')}",
                value=f"A hex clings to you for the next {digs} {dig_word}.",
                inline=False,
            )
        # JC threat: a negative outcome that left the player below zero.
        if balance_after is not None and balance_after < 0:
            embed.add_field(
                name="In Debt",
                value=(
                    f"That cost dropped you to {balance_after} {JOPACOIN_EMOTE}. "
                    "You're in the red."
                ),
                inline=False,
            )

        boss_encounter = getattr(result, "boss_encounter", False)
        boss_info = getattr(result, "boss_info", None)
        if boss_encounter:
            boss_name = getattr(boss_info, "name", "Unknown Boss") if boss_info else "Unknown Boss"
            boundary = getattr(boss_info, "boundary", None) if boss_info else None
            path_text = f"Depth {boundary}" if boundary is not None else "the next layer"
            embed.add_field(
                name="Boss Encountered",
                value=f"**{boss_name}** blocks {path_text}. Use `/dig` to fight, scout, or retreat.",
                inline=False,
            )

        # Show buff if granted
        buff = getattr(result, "buff_applied", None)
        if buff:
            buff_d = buff if isinstance(buff, dict) else (buff._d if hasattr(buff, "_d") else {})
            embed.add_field(
                name=f"Buff: {buff_d.get('name', '?')}",
                value=f"Active for {buff_d.get('duration_digs', 0)} digs",
                inline=True,
            )

        # Show chain event if triggered (P7+)
        chain = getattr(result, "chain_event", None)
        if chain:
            chain_d = chain if isinstance(chain, dict) else (chain._d if hasattr(chain, "_d") else {})
            if chain_d:
                embed.add_field(
                    name="​",
                    value=pick_description(chain_d) or "Another event triggers!",
                    inline=False,
                )

        # Splash: surface the aftermath inline on the digger's embed.
        # The digger's result is itself public, so victims who follow
        # the dig channel see the Aftermath field — no separate broadcast.
        splash_obj = getattr(result, "splash", None)
        splash_d = splash_obj._d if hasattr(splash_obj, "_d") else splash_obj
        if isinstance(splash_d, dict) and splash_d.get("victims"):
            if self.dig_flavor_service is not None:
                depth_after = getattr(result, "depth_after", 0) or getattr(result, "depth", 0)
                layer_def = get_layer_def(int(depth_after) if depth_after else 0)
                digger_layer = layer_def.name if layer_def else "Dirt"
                narrative = await self.dig_flavor_service.narrate_splash(
                    digger_id=self.user_id,
                    guild_id=self.guild_id or 0,
                    event_name=event.get("name", "Unknown Event"),
                    event_description=msg,
                    splash_mode=splash_d.get("mode", "burn"),
                    victims=splash_d.get("victims", []),
                    digger_layer=digger_layer,
                )
                if narrative:
                    splash_d["llm_narrative"] = narrative
            aftermath_lines = _splash_aftermath_lines(splash_d)
            if aftermath_lines:
                embed.add_field(
                    name="Aftermath",
                    value="\n".join(aftermath_lines),
                    inline=False,
                )

        return embed


class BoonSelectionView(discord.ui.View):
    """View for boon events — player picks one of 2-3 buffs."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        event_data: dict,
        target_channel: discord.abc.Messageable | None = None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.event_data = event_data
        self.target_channel = target_channel
        boons = event_data.get("boon_options", []) if isinstance(event_data, dict) else []
        for i, boon in enumerate(boons[:5]):
            label = boon.get("name", f"Boon {i + 1}")[:80]
            btn = discord.ui.Button(
                label=label,
                style=discord.ButtonStyle.primary,
                custom_id=f"boon_select_{i}",
            )
            btn.callback = self._make_callback(i, boon)
            self.add_item(btn)

    def _make_callback(self, index: int, boon: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your event.", ephemeral=True)
                return
            await safe_defer(interaction)
            event_id = self.event_data.get("id", "") if isinstance(self.event_data, dict) else ""
            try:
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.resolve_event,
                    self.user_id, self.guild_id, event_id, f"boon_{index}",
                ))
                if not getattr(result, "success", True):
                    await interaction.followup.send(
                        getattr(result, "error", "Boon selection failed."), ephemeral=True
                    )
                    return
                embed = discord.Embed(
                    title=self.event_data.get("name", "Boon") if isinstance(self.event_data, dict) else "Boon",
                    description=getattr(result, "message", f"You chose {boon.get('name', 'a boon')}!"),
                    color=0x5865F2,
                )
                buff = getattr(result, "buff_applied", None)
                if buff:
                    buff_d = buff if isinstance(buff, dict) else (buff._d if hasattr(buff, "_d") else {})
                    embed.add_field(
                        name=f"Buff: {buff_d.get('name', '?')}",
                        value=f"Active for {buff_d.get('duration_digs', 0)} digs",
                        inline=True,
                    )
                if self.target_channel is not None:
                    try:
                        await self.target_channel.send(embed=embed)
                    except Exception as exc:
                        logger.warning("Boon result send to dig channel failed: %s", exc)
                        await interaction.followup.send(embed=embed)
                else:
                    await interaction.followup.send(embed=embed)
            except Exception as e:
                logger.error("Boon selection error: %s", e, exc_info=True)
                await interaction.followup.send("Boon selection failed.", ephemeral=True)
            self.stop()
        return callback

    async def on_timeout(self) -> None:
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        try:
            if hasattr(self, "message") and self.message is not None:
                await self.message.edit(content="*The moment passed.*", view=self)
        except (discord.NotFound, discord.HTTPException):
            pass
