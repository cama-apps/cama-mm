"""Paginated view for enriched match embeds with advantage graph."""

import asyncio
import logging
from io import BytesIO

import discord

from utils.drawing import draw_advantage_graph

logger = logging.getLogger("cama_bot.utils.match_views")


class EnrichedMatchView(discord.ui.View):
    """Two-page view: page 1 = enriched match embed, page 2 = advantage graph."""

    def __init__(
        self,
        embed: discord.Embed,
        enrichment_data: dict | None,
        match_id: int,
        *,
        timeout: int = 120,
    ):
        super().__init__(timeout=timeout)
        self.embed = embed
        self.enrichment_data = enrichment_data
        self.match_id = match_id
        self.page = 0  # 0 = embed, 1 = graph
        self.message: discord.Message | None = None
        self._graph_cache: bytes | None = None

        # Only show buttons if we have advantage data
        has_graph = (
            enrichment_data
            and (enrichment_data.get("radiant_gold_adv") or enrichment_data.get("radiant_xp_adv"))
        )
        if not has_graph:
            self.clear_items()

        self._update_buttons()

    def _update_buttons(self):
        self.prev_button.disabled = self.page == 0
        self.next_button.disabled = self.page >= 1

    async def _render_graph(self) -> discord.File | None:
        """Render the advantage graph, caching the bytes."""
        if self._graph_cache is not None:
            return discord.File(BytesIO(self._graph_cache), filename="advantage.png")
        try:
            buf = await asyncio.to_thread(
                draw_advantage_graph, self.enrichment_data, self.match_id
            )
            if buf:
                self._graph_cache = buf.read()
                return discord.File(BytesIO(self._graph_cache), filename="advantage.png")
        except Exception as exc:
            logger.warning(
                "Failed to generate advantage graph for match %s: %s", self.match_id, exc,
            )
        return None

    async def _safe_edit_response(self, interaction: discord.Interaction, **kwargs) -> None:
        """Edit the response, swallowing expired-token / unknown-interaction failures."""
        try:
            await interaction.response.edit_message(**kwargs)
        except (discord.NotFound, discord.HTTPException) as exc:
            logger.warning("Match view edit failed: %s", exc)

    @discord.ui.button(label="< Prev", style=discord.ButtonStyle.secondary)
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        if self.page > 0:
            self.page = 0
            self._update_buttons()
            await self._safe_edit_response(
                interaction, embed=self.embed, attachments=[], view=self,
            )

    @discord.ui.button(label="Next >", style=discord.ButtonStyle.primary)
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        if self.page < 1:
            self.page = 1
            self._update_buttons()
            graph_file = await self._render_graph()
            if graph_file:
                # Minimal embed to hold the image
                graph_embed = discord.Embed(color=0x2F3136)
                graph_embed.set_image(url="attachment://advantage.png")
                graph_embed.set_footer(text=f"Match #{self.match_id} — Team Advantages Per Minute")
                await self._safe_edit_response(
                    interaction, embed=graph_embed, attachments=[graph_file], view=self,
                )
            else:
                # Fallback — shouldn't happen since we hide buttons when no data
                try:
                    await interaction.response.defer()
                except (discord.NotFound, discord.HTTPException) as exc:
                    logger.warning("Match view fallback defer failed: %s", exc)

    async def on_timeout(self):
        """Disable buttons on timeout."""
        if self.message:
            try:
                self.prev_button.disabled = True
                self.next_button.disabled = True
                await self.message.edit(view=self)
            except discord.HTTPException:
                pass
