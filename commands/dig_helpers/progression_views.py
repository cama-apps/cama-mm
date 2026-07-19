"""Views for prestige, mutations, and the paginated dig guide."""

from __future__ import annotations

import asyncio
import logging
import random
from typing import TYPE_CHECKING

import discord

from commands.dig_helpers._shared import GUIDE_PAGES, _wrap
from config import DIG_CHANNEL_ID
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer
from utils.neon_helpers import get_neon_service, send_neon_result

if TYPE_CHECKING:
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")


class PrestigePerksView(discord.ui.View):
    """View for selecting prestige perks."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        perks: list[dict],
        new_level: int = 0,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        # Guards against a second resolution: two fast clicks would otherwise
        # each reach the service call before stop() fires at the end of the
        # first callback's awaits.
        self._resolved = False
        # Sample 4 random perks from the eligible pool, seeded by (user, level)
        # so that closing and re-opening the picker shows the same options —
        # otherwise players could re-roll until they got the perks they want.
        # Use hashlib (not Python's salted built-in hash()) so the seed is
        # stable across bot restarts; otherwise a deploy would silently
        # re-roll mid-prestige.
        import hashlib
        import struct
        digest = hashlib.sha256(f"{user_id}:{new_level}".encode()).digest()
        seed = struct.unpack_from("<Q", digest)[0]
        rng = random.Random(seed)
        sample_size = min(4, len(perks))
        self.perks = rng.sample(perks, sample_size) if sample_size else []
        for i, perk in enumerate(self.perks):
            button = discord.ui.Button(
                label=perk.get("name", f"Perk {i+1}"),
                style=discord.ButtonStyle.primary,
                custom_id=f"prestige_perk_{i}",
            )
            button.callback = self._make_callback(i, perk)
            self.add_item(button)

    def _make_callback(self, index: int, perk: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your prestige.", ephemeral=True)
                return
            # Check-and-set with no await in between, so a burst of rapid
            # clicks can never each reach the service call — only the first.
            if self._resolved:
                await interaction.response.send_message(
                    "You've already made your selection.", ephemeral=True
                )
                return
            self._resolved = True
            await safe_defer(interaction)
            try:
                mutation_choice = getattr(self, "_mutation_choice", None)
                result = _wrap(await asyncio.to_thread(
                    self.dig_service.prestige,
                    self.user_id,
                    self.guild_id,
                    perk.get("id", index),
                    mutation_choice,
                ))
                new_level = getattr(result, "prestige_level", 0)
                run_score = getattr(result, "run_score", 0)
                best_score = getattr(result, "best_run_score", 0)
                desc_parts = [
                    f"You selected **{perk.get('name', 'Unknown')}**.",
                    f"Run Score: **{run_score}** (Best: {best_score})",
                    getattr(result, "message", "Your tunnel has been reset. Dig deeper!"),
                ]
                embed = discord.Embed(
                    title=f"Prestige {new_level} Complete!",
                    description="\n".join(desc_parts),
                    color=0xFFD700,
                )

                # Show ascension modifier unlocked at this level
                ascension = getattr(result, "ascension_unlocked", None)
                if ascension:
                    asc_d = ascension if isinstance(ascension, dict) else (ascension._d if hasattr(ascension, "_d") else {})
                    embed.add_field(
                        name=f"Ascension Unlocked: {asc_d.get('name', '?')}",
                        value=f"Penalty: {asc_d.get('penalty', '?')}\nReward: {asc_d.get('reward', '?')}",
                        inline=False,
                    )

                # Prestige flat-grant: show JC + relic
                grant = getattr(result, "prestige_grant", None)
                if grant:
                    grant_d = grant if isinstance(grant, dict) else (
                        grant._d if hasattr(grant, "_d") else {}
                    )
                    jc_amt = grant_d.get("jc", 0) if isinstance(grant_d, dict) else 0
                    relic = grant_d.get("relic") if isinstance(grant_d, dict) else None
                    relic_d = relic if isinstance(relic, dict) else (
                        relic._d if hasattr(relic, "_d") else None
                    )
                    grant_parts = [f"+{jc_amt} {JOPACOIN_EMOTE}"]
                    if relic_d:
                        grant_parts.append(f"**{relic_d.get('name', 'Relic')}** ({relic_d.get('rarity', '?')})")
                    embed.add_field(
                        name="Prestige Grant",
                        value=" · ".join(grant_parts),
                        inline=False,
                    )

                # Show mutation info for P8+
                mutations = getattr(result, "mutations", None)
                if mutations:
                    mut_d = mutations if isinstance(mutations, dict) else (mutations._d if hasattr(mutations, "_d") else {})
                    forced = mut_d.get("forced") if isinstance(mut_d, dict) else None
                    chosen = mut_d.get("chosen") if isinstance(mut_d, dict) else None
                    mut_lines = []
                    if forced:
                        f_d = forced if isinstance(forced, dict) else (forced._d if hasattr(forced, "_d") else {})
                        mut_lines.append(f"Forced: **{f_d.get('name', '?')}** — {f_d.get('description', '')}")
                    if chosen:
                        c_d = chosen if isinstance(chosen, dict) else (chosen._d if hasattr(chosen, "_d") else {})
                        mut_lines.append(f"Chosen: **{c_d.get('name', '?')}** — {c_d.get('description', '')}")
                    if mut_lines:
                        embed.add_field(name="Mutations", value="\n".join(mut_lines), inline=False)

                # Detailed embed (perk + run score + ascension unlock) is
                # for the prestiger only — keeps progression details private.
                await interaction.followup.send(embed=embed, ephemeral=True)
                # Public ascension announcement: terse, atmospheric, no
                # perk or score reveal. Routes to dig channel when set.
                await self._announce_ascension_publicly(interaction)
                # Rare neon ascension GIF (best-effort).
                try:
                    neon = get_neon_service(interaction.client)
                    if neon:
                        pr = await neon.on_dig_prestige(self.user_id, self.guild_id)
                        await send_neon_result(interaction, pr)
                except Exception:
                    logger.debug("prestige neon failed", exc_info=True)
            except ValueError as e:
                await interaction.followup.send(str(e), ephemeral=True)
            except Exception as e:
                logger.error("Prestige error: %s", e)
                await interaction.followup.send("Prestige failed.", ephemeral=True)
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

    async def _announce_ascension_publicly(
        self, interaction: discord.Interaction,
    ) -> None:
        """Post a terse, flavor-only ascension line to the dig channel."""
        member = None
        if interaction.guild is not None:
            member = interaction.guild.get_member(self.user_id)
        name = member.display_name if member else f"<@{self.user_id}>"
        text = f"*{name} has ascended.*"

        target: discord.abc.Messageable | None = None
        if DIG_CHANNEL_ID:
            try:
                channel = interaction.client.get_channel(DIG_CHANNEL_ID)
                if channel is None:
                    channel = await interaction.client.fetch_channel(DIG_CHANNEL_ID)
                if isinstance(channel, discord.TextChannel) and (
                    interaction.guild is None
                    or channel.guild.id == interaction.guild.id
                ):
                    perms = channel.permissions_for(channel.guild.me)
                    if perms.send_messages:
                        target = channel
            except Exception as exc:
                logger.warning("Cannot fetch dig channel for ascension: %s", exc)
        if target is None:
            target = interaction.channel
        if target is None:
            return
        try:
            await target.send(text)
        except Exception:
            logger.warning("Ascension announcement failed", exc_info=True)


class MutationSelectionView(discord.ui.View):
    """View for choosing a mutation during P8+ prestige.

    After the player picks a mutation, this view sets the choice on the
    paired PrestigePerksView and sends that view for the perk selection
    step.
    """

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        forced: dict,
        choices: list[dict],
        perks_view: PrestigePerksView,
        perks_embed: discord.Embed,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.forced = forced
        self.choices = choices
        self.perks_view = perks_view
        self.perks_embed = perks_embed
        for i, mut in enumerate(choices[:5]):
            label = mut.get("name", f"Mutation {i + 1}")[:80]
            btn = discord.ui.Button(
                label=label,
                style=discord.ButtonStyle.primary,
                custom_id=f"mutation_select_{i}",
            )
            btn.callback = self._make_callback(mut)
            self.add_item(btn)

    def _make_callback(self, mut: dict):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message("This isn't your prestige.", ephemeral=True)
                return
            await safe_defer(interaction)
            # Store the mutation choice on the perk view so it can pass it to prestige()
            self.perks_view._mutation_choice = mut.get("id")
            msg = await interaction.followup.send(
                embed=self.perks_embed, view=self.perks_view, wait=True
            )
            self.perks_view.message = msg
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


class DigGuideView(discord.ui.View):
    """Paginated guide with Previous/Next buttons."""

    def __init__(self):
        super().__init__(timeout=180)
        self.current = 0
        self._sync_buttons()

    def _sync_buttons(self) -> None:
        self.prev_btn.disabled = self.current == 0
        self.next_btn.disabled = self.current >= len(GUIDE_PAGES) - 1

    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary)
    async def prev_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current -= 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=GUIDE_PAGES[self.current], view=self)

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current += 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=GUIDE_PAGES[self.current], view=self)
