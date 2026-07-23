"""Discord presentation for persisted Dig route junctions."""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from commands.dig_helpers._shared import LAYER_COLORS
from utils.interaction_safety import safe_defer, safe_followup

if TYPE_CHECKING:
    from services.dig_service import DigService


logger = logging.getLogger("cama_bot.commands.dig")


def _as_dict(value) -> dict:
    if isinstance(value, dict):
        return value
    raw = getattr(value, "_d", None)
    return raw if isinstance(raw, dict) else {}


def get_route_choice(result) -> dict | None:
    raw = _as_dict(result)
    choice = raw.get("route_choice") if raw else getattr(result, "route_choice", None)
    if choice is None and (
        raw.get("route_choice_required") or raw.get("choice_required")
    ):
        choice = raw
    choice = _as_dict(choice)
    if not choice.get("choice_required") or not choice.get("offered_routes"):
        return None
    return choice


def build_route_choice_embed(route_choice) -> discord.Embed:
    choice = _as_dict(route_choice)
    layer = choice.get("layer") or "Unknown Layer"
    start_depth = choice.get("start_depth", "?")
    end_depth = choice.get("end_depth", "?")
    embed = discord.Embed(
        title=f"{layer} Junction",
        description=(
            f"The way forward splits after depth **{start_depth}**. "
            f"Choose the passage that will shape your descent toward **{end_depth}**.\n\n"
            "This choice remains active until another junction replaces it."
        ),
        color=LAYER_COLORS.get(layer, LAYER_COLORS["Dirt"]),
    )
    for route_value in choice.get("offered_routes", []):
        route = _as_dict(route_value)
        embed.add_field(
            name=route.get("name", "Unknown Route"),
            value=route.get("description", "The passage reveals nothing."),
            inline=False,
        )
    embed.set_footer(text="No rerolls. If this view expires, use /dig go to reopen it.")
    return embed


def build_locked_route_embed(route, *, layer: str | None = None) -> discord.Embed:
    route_data = _as_dict(route)
    route_name = route_data.get("name", "Unknown Route")
    route_layer = layer or route_data.get("layer") or "the next layer"
    return discord.Embed(
        title=f"Route Locked: {route_name}",
        description=(
            f"**{route_name}** will guide you through **{route_layer}**.\n\n"
            f"{route_data.get('description', '')}"
        ),
        color=LAYER_COLORS.get(route_layer, LAYER_COLORS["Dirt"]),
    )


def add_route_choice_fields(embed: discord.Embed, route_choice) -> None:
    choice = _as_dict(route_choice)
    if not choice.get("choice_required"):
        return
    layer = choice.get("layer", "the next layer")
    embed.title = f"{layer} Junction"
    embed.color = LAYER_COLORS.get(layer, LAYER_COLORS["Dirt"])
    embed.add_field(
        name=f"The path splits toward {layer}",
        value="Choose one route below. It remains active until another junction replaces it.",
        inline=False,
    )
    for route_value in choice.get("offered_routes", []):
        route = _as_dict(route_value)
        embed.add_field(
            name=route.get("name", "Unknown Route"),
            value=route.get("description", "The passage reveals nothing."),
            inline=False,
        )


class RouteChoiceView(discord.ui.View):
    """Three persisted route buttons; timeout never selects on the player's behalf."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        route_choice,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.route_choice = _as_dict(route_choice)
        self.message: discord.Message | None = None
        self._resolved = False

        for route_value in self.route_choice.get("offered_routes", []):
            route = _as_dict(route_value)
            route_id = route.get("id")
            if not route_id:
                continue
            button = discord.ui.Button(
                label=str(route.get("name") or route_id)[:80],
                style=(
                    discord.ButtonStyle.secondary
                    if route.get("layer") is None
                    else discord.ButtonStyle.primary
                ),
                custom_id=f"dig_route_{route_id}",
            )
            button.callback = self._make_callback(str(route_id))
            self.add_item(button)

    def _make_callback(self, route_id: str):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message(
                    "Only the tunnel owner can choose this route.",
                    ephemeral=True,
                )
                return
            if self._resolved:
                await safe_defer(interaction)
                return
            self._resolved = True
            await safe_defer(interaction)
            try:
                result = await asyncio.to_thread(
                    self.dig_service.choose_route,
                    self.user_id,
                    self.guild_id,
                    route_id,
                )
            except Exception:
                self._resolved = False
                logger.exception("Dig route selection failed")
                await safe_followup(
                    interaction,
                    content="The passage shifted before it could be marked. Try again.",
                    ephemeral=True,
                )
                return
            result_data = _as_dict(result)
            if not result_data.get("success"):
                self._resolved = False
                await safe_followup(
                    interaction,
                    content=result_data.get("error", "Route selection failed."),
                    ephemeral=True,
                )
                return

            embed = build_locked_route_embed(
                result_data.get("route"),
                layer=self.route_choice.get("layer"),
            )
            try:
                await interaction.edit_original_response(embed=embed, view=None)
            except Exception:
                logger.warning("Dig route message edit failed", exc_info=True)
                await safe_followup(interaction, embed=embed)
            self.stop()

        return callback

    async def on_timeout(self):
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        if self.message is None:
            return
        try:
            await self.message.edit(view=self)
        except (discord.NotFound, discord.HTTPException) as exc:
            logger.warning("Dig route timeout edit failed: %s", exc)
