"""Cama pets: adopt, feed, and try to keep a camel-llama hybrid alive.

Command surface is the /pet group (9 subcommands). A 10-minute background
sweep detects hatches and starvation deaths (both computed lazily from
anchors — the loop only announces) and pays the weekly nonprofit care refund.
Public posts go to PET_CHANNEL_ID when configured; otherwise pets stay quiet
and everything is revealed on the owner's next interaction.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands, tasks

from commands.checks import require_guild
from commands.pet_helpers import embeds as pet_embeds
from commands.pet_helpers.views import PetStatusView
from config import PET_CHANNEL_ID
from domain.models.pet import PetStage
from domain.pet_constants import (
    ACCESSORIES,
    FEED_CAP_PER_DAY,
    FOOD_ITEMS,
    GILDED_EGG_PREMIUM,
    MAX_BUY_QTY,
    RENAME_COST,
    SALT_LICK,
    TRINKET_COST,
    get_accessory,
)
from utils.formatting import JOPACOIN_EMOTE
from utils.game_date import game_date_for_timestamp
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from domain.models.pet import DeathNotice, HatchNotice, RefundNotice
    from services.pet_service import PetService

logger = logging.getLogger("cama_bot.commands.pet")

FOOD_CHOICES = [
    app_commands.Choice(
        name=f"{food.display_name} ({food.cost} JC, +{food.restore} hunger)",
        value=item_id,
    )
    for item_id, food in FOOD_ITEMS.items()
]
BUY_CHOICES = FOOD_CHOICES + [
    app_commands.Choice(
        name=f"{SALT_LICK.display_name} ({SALT_LICK.cost} JC, pampers instantly)",
        value=SALT_LICK.item_id,
    )
]


class PetCommands(commands.Cog):
    pet = app_commands.Group(
        name="pet", description="Adopt and care for your cama (camel-llama hybrid)"
    )

    def __init__(self, bot: commands.Bot, pet_service: PetService):
        self.bot = bot
        self.pet_service = pet_service

    async def cog_load(self) -> None:
        self._pet_sweep_loop.start()

    async def cog_unload(self) -> None:
        self._pet_sweep_loop.cancel()

    # ────────────────────────────────────────────────────────────────────
    # Shared composition
    # ────────────────────────────────────────────────────────────────────

    async def compose_status(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        owner_name: str,
        with_view: bool = True,
    ) -> tuple[discord.Embed, discord.File | None, PetStatusView | None]:
        status = (
            await asyncio.to_thread(
                self.pet_service.get_status, discord_id, guild_id
            )
        ).value
        next_fee = await asyncio.to_thread(
            self.pet_service.next_adoption_fee, discord_id, guild_id
        )
        now = self.pet_service._now()
        embed, file = await asyncio.to_thread(
            pet_embeds.build_status_embed,
            status,
            self.pet_service.decay_per_day,
            now,
            owner_name=owner_name,
            next_fee=next_fee,
        )
        view = None
        if with_view and status.pet is not None:
            can_feed = (
                status.stage != PetStage.EGG
                and status.pet.feeds_used_on(game_date_for_timestamp(now))
                < FEED_CAP_PER_DAY
            )
            view = PetStatusView(
                self,
                discord_id,
                guild_id,
                supplies=status.supplies,
                species_id=(
                    status.pet.species if status.stage != PetStage.EGG else ""
                ),
                can_feed=can_feed,
            )
            await self._rearm_warning(status.pet)
        return embed, file, view

    async def _rearm_warning(self, pet) -> None:
        """(Re)schedule the opt-in hungry-DM for the pet's next warning crossing.

        The preference read happens off-loop; schedule_pet_reminder itself must
        run on the loop (it creates the asyncio task).
        """
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc is None or pet is None:
            return
        try:
            crossing = self.pet_service.warning_crossing_for(pet)
            prefs = await asyncio.to_thread(
                reminder_svc.get_preferences, pet.discord_id, pet.guild_id or 0
            )
            reminder_svc.schedule_pet_reminder(
                self.bot,
                pet.discord_id,
                pet.guild_id,
                crossing,
                pet_name=pet.name,
                preference_enabled=bool(prefs.get("pet_enabled")),
            )
        except Exception:
            logger.debug("Pet warning re-arm failed", exc_info=True)

    # ────────────────────────────────────────────────────────────────────
    # Subcommands
    # ────────────────────────────────────────────────────────────────────

    @pet.command(name="adopt", description="Adopt a mysterious cama egg")
    @app_commands.describe(
        name="Your pet's name (you're naming an egg, brave)",
        egg=f"Gilded Egg: +{GILDED_EGG_PREMIUM} JC, no commons in the pool",
    )
    @app_commands.choices(egg=[
        app_commands.Choice(name="Standard Egg", value="standard"),
        app_commands.Choice(
            name=f"Gilded Egg (+{GILDED_EGG_PREMIUM} JC, uncommon or better)",
            value="gilded",
        ),
    ])
    @require_guild
    async def adopt(
        self, interaction: discord.Interaction, name: str, egg: str = "standard"
    ):
        guild_id = interaction.guild.id if interaction.guild else None
        rl = GLOBAL_RATE_LIMITER.check(
            scope="pet", guild_id=guild_id or 0, user_id=interaction.user.id,
            limit=6, per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s.", ephemeral=True
            )
            return
        if not await safe_defer(interaction, ephemeral=False):
            return
        result = await asyncio.to_thread(
            self.pet_service.adopt, interaction.user.id, guild_id, name, egg
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        adopted = result.value["pet"]
        gilded = result.value["egg_tier"] == "gilded"
        flair = "a **gilded egg**" if gilded else "an egg"
        pity_line = (
            "\n✨ The nonprofit took pity: this egg can't be Common."
            if result.value["pity_active"]
            else ""
        )
        embed = discord.Embed(
            title="🥚 A gilded egg!" if gilded else "🥚 A mysterious egg!",
            description=(
                f"**{interaction.user.display_name}** adopted {flair} and named it "
                f"**{adopted.name}** for {adopted.adopt_fee} {JOPACOIN_EMOTE}.\n"
                f"It hatches <t:{adopted.hatched_at}:R>. What's inside? "
                "Nobody knows. Not even the egg." + pity_line
            ),
            color=pet_embeds.COLOR_EGG,
        )
        file = await asyncio.to_thread(
            pet_embeds.get_egg_card, adopted.pet_id
        )
        if file:
            embed.set_image(url=f"attachment://{file.filename}")
        await safe_followup(interaction, embed=embed, file=file)
        await self._rearm_warning(adopted)

    @pet.command(name="status", description="Check on your cama (art, hunger, mood)")
    @app_commands.describe(
        user="Peek at someone else's pet", public="Show to the whole channel"
    )
    @require_guild
    async def status(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
        public: bool = False,
    ):
        guild_id = interaction.guild.id if interaction.guild else None
        target = user or interaction.user
        own = target.id == interaction.user.id
        if not await safe_defer(interaction, ephemeral=not public):
            return
        embed, file, view = await self.compose_status(
            target.id, guild_id, owner_name=target.display_name,
            with_view=own,
        )
        message = await safe_followup(
            interaction, embed=embed,
            file=file, view=view, ephemeral=not public,
        )
        if view is not None:
            view.message = message

    @pet.command(name="feed", description="Feed your cama from your supplies")
    @app_commands.describe(item="Which food to serve")
    @app_commands.choices(item=FOOD_CHOICES)
    @require_guild
    async def feed(self, interaction: discord.Interaction, item: str):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=True):
            return
        result = await asyncio.to_thread(
            self.pet_service.feed, interaction.user.id, guild_id, item
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        outcome = result.value
        food = FOOD_ITEMS[item]
        if outcome.spat:
            await safe_followup(
                interaction,
                content=(
                    f"💢 **{outcome.pet.name}** spat the {food.display_name} "
                    "straight back at you. The temperament of legends. "
                    f"({outcome.remaining_qty} left)"
                ),
                ephemeral=True,
            )
            return
        bar = pet_embeds.hunger_bar(outcome.new_hunger)
        await safe_followup(
            interaction,
            content=(
                f"{food.emoji} **{outcome.pet.name}** munches the "
                f"{food.display_name}. Hunger {outcome.old_hunger} → "
                f"**{outcome.new_hunger}** `{bar}` · {outcome.remaining_qty} left · "
                f"{outcome.feeds_left_today} feeds left today"
            ),
            ephemeral=True,
        )
        await self._rearm_warning(outcome.pet)

    @pet.command(name="shop", description="Browse cama food and treats")
    @require_guild
    async def shop(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=True):
            return
        status = (
            await asyncio.to_thread(
                self.pet_service.get_status, interaction.user.id, guild_id
            )
        ).value
        species_id = (
            status.pet.species
            if status.pet is not None and status.stage != PetStage.EGG
            else ""
        )
        balance = await asyncio.to_thread(
            self.pet_service.player_repo.get_balance, interaction.user.id, guild_id
        )
        embed = pet_embeds.build_shop_embed(status.supplies, species_id, balance)
        await safe_followup(interaction, embed=embed, ephemeral=True)

    @pet.command(name="buy", description="Buy cama supplies")
    @app_commands.describe(item="What to buy", qty="How many (salt lick: 1)")
    @app_commands.choices(item=BUY_CHOICES)
    @require_guild
    async def buy(
        self,
        interaction: discord.Interaction,
        item: str,
        qty: app_commands.Range[int, 1, MAX_BUY_QTY] = 1,
    ):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=True):
            return
        result = await asyncio.to_thread(
            self.pet_service.buy, interaction.user.id, guild_id, item, qty
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        purchase = result.value
        if item == SALT_LICK.item_id:
            await safe_followup(
                interaction,
                content=(
                    f"🧂 **{purchase['pet'].name}** is thoroughly pampered until "
                    f"<t:{purchase['pampered_until']}:t> "
                    f"(-{purchase['total_cost']} {JOPACOIN_EMOTE})"
                ),
                ephemeral=True,
            )
            return
        food = FOOD_ITEMS[item]
        await safe_followup(
            interaction,
            content=(
                f"{food.emoji} Bought {purchase['qty']}× {food.display_name} for "
                f"{purchase['total_cost']} {JOPACOIN_EMOTE} — you now have "
                f"×{purchase['new_qty']}."
            ),
            ephemeral=True,
        )

    @pet.command(name="rename", description=f"Rename your cama ({RENAME_COST} JC)")
    @app_commands.describe(name="The new name")
    @require_guild
    async def rename(self, interaction: discord.Interaction, name: str):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=True):
            return
        result = await asyncio.to_thread(
            self.pet_service.rename, interaction.user.id, guild_id, name
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        await safe_followup(
            interaction,
            content=(
                f"✏️ Henceforth known as **{result.value.name}** "
                f"(-{RENAME_COST} {JOPACOIN_EMOTE})"
            ),
            ephemeral=True,
        )

    async def _trinket_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        guild_id = interaction.guild.id if interaction.guild else None
        owned = await asyncio.to_thread(
            self.pet_service.owned_trinkets, interaction.user.id, guild_id
        )
        choices = []
        for accessory_id in owned:
            accessory = get_accessory(accessory_id)
            if current.lower() in accessory.display_name.lower():
                choices.append(
                    app_commands.Choice(
                        name=f"{accessory.display_name} ({accessory.tier})",
                        value=accessory_id,
                    )
                )
        return choices[:25]

    @pet.command(
        name="trinket",
        description=f"Roll a Mystery Trinket ({TRINKET_COST} JC) or wear one you own",
    )
    @app_commands.describe(wear="Wear an owned trinket instead of rolling")
    @app_commands.autocomplete(wear=_trinket_autocomplete)
    @require_guild
    async def trinket(
        self, interaction: discord.Interaction, wear: str | None = None
    ):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=True):
            return
        if wear is not None:
            result = await asyncio.to_thread(
                self.pet_service.wear_trinket, interaction.user.id, guild_id, wear
            )
            if not result.success:
                await safe_followup(
                    interaction, content=f"❌ {result.error}", ephemeral=True
                )
                return
            accessory = get_accessory(result.value)
            await safe_followup(
                interaction,
                content=(
                    f"{accessory.emoji} Now wearing the "
                    f"**{accessory.display_name}**."
                ),
                ephemeral=True,
            )
            return
        result = await asyncio.to_thread(
            self.pet_service.roll_trinket, interaction.user.id, guild_id
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        outcome = result.value
        accessory = get_accessory(outcome.accessory_id)
        if outcome.duplicate:
            await safe_followup(
                interaction,
                content=(
                    f"{accessory.emoji} A duplicate **{accessory.display_name}** "
                    f"— it dissolves into a partial refund (net "
                    f"-{outcome.net_cost} {JOPACOIN_EMOTE}). "
                    f"Collection: {outcome.owned_count}/{len(ACCESSORIES)}"
                ),
                ephemeral=True,
            )
            return
        tier_flair = {
            "common": "", "uncommon": "🔹 Uncommon! ",
            "rare": "🔮 RARE! ", "legendary": "⚡ LEGENDARY!!! ",
        }.get(accessory.tier, "")
        await safe_followup(
            interaction,
            content=(
                f"🎁 {tier_flair}**{accessory.display_name}** {accessory.emoji} — "
                f"_{accessory.blurb}_ (-{outcome.net_cost} {JOPACOIN_EMOTE})\n"
                f"Equipped! Collection: {outcome.owned_count}/{len(ACCESSORIES)}"
            ),
            ephemeral=True,
        )

    @pet.command(name="graveyard", description="Visit the cama memorial garden")
    @app_commands.describe(user="Whose graveyard to visit")
    @require_guild
    async def graveyard(
        self, interaction: discord.Interaction, user: discord.Member | None = None
    ):
        guild_id = interaction.guild.id if interaction.guild else None
        target = user or interaction.user
        if not await safe_defer(interaction, ephemeral=False):
            return
        result = await asyncio.to_thread(
            self.pet_service.get_graveyard, target.id, guild_id
        )
        camadex = await asyncio.to_thread(
            self.pet_service.camadex, target.id, guild_id
        )
        embed = pet_embeds.build_graveyard_embed(
            result.value, target.display_name, camadex=camadex
        )
        await safe_followup(interaction, embed=embed)

    @pet.command(name="leaderboard", description="The oldest living camas")
    @require_guild
    async def leaderboard(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else None
        if not await safe_defer(interaction, ephemeral=False):
            return
        result = await asyncio.to_thread(self.pet_service.get_leaderboard, guild_id)
        embed = pet_embeds.build_leaderboard_embed(
            result.value, self.pet_service.decay_per_day, self.pet_service._now()
        )
        await safe_followup(interaction, embed=embed)

    # ────────────────────────────────────────────────────────────────────
    # Background sweep
    # ────────────────────────────────────────────────────────────────────

    @tasks.loop(minutes=10)
    async def _pet_sweep_loop(self):
        try:
            result = await asyncio.to_thread(self.pet_service.sweep)
        except Exception:
            logger.exception("Pet sweep failed")
            return
        for hatch in result["hatches"]:
            try:
                await self._deliver_hatch(hatch)
            except Exception:
                logger.exception(
                    "Hatch delivery failed for pet %s; will retry", hatch.pet.pet_id
                )
        for death in result["deaths"]:
            try:
                await self._deliver_death(death)
            except Exception:
                logger.exception(
                    "Death delivery failed for pet %s; will retry", death.pet.pet_id
                )
        for refund in result["refunds"]:
            try:
                await self._deliver_refund(refund)
            except Exception:
                logger.exception(
                    "Refund summary failed for guild %s", refund.guild_id
                )

    @_pet_sweep_loop.before_loop
    async def _before_sweep(self):
        await self.bot.wait_until_ready()

    def _pet_channel(self, guild_id: int) -> discord.TextChannel | None:
        if not PET_CHANNEL_ID:
            return None
        guild = self.bot.get_guild(guild_id)
        if guild is None:
            return None
        channel = guild.get_channel(PET_CHANNEL_ID)
        return channel if isinstance(channel, discord.TextChannel) else None

    async def _deliver_hatch(self, notice: HatchNotice) -> None:
        pet = notice.pet
        channel = self._pet_channel(pet.guild_id)
        if channel is not None:
            embed, file = await asyncio.to_thread(pet_embeds.build_hatch_embed, pet)
            try:
                await channel.send(embed=embed, file=file)
            except discord.Forbidden:
                pass  # permanent: mark below so we don't loop forever
        await asyncio.to_thread(self.pet_service.mark_hatch_announced, pet)

    async def _deliver_death(self, notice: DeathNotice) -> None:
        pet = notice.pet
        channel = self._pet_channel(pet.guild_id)
        if channel is not None:
            embed, file = await asyncio.to_thread(pet_embeds.build_death_embed, pet)
            try:
                await channel.send(embed=embed, file=file)
            except discord.Forbidden:
                pass
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc is not None:
            reminder_svc.cancel_pet_reminder(pet.discord_id, pet.guild_id)
        await self._dm_death_notice(pet)
        await asyncio.to_thread(self.pet_service.mark_death_announced, pet)

    async def _dm_death_notice(self, pet) -> None:
        """Best-effort opt-in death DM; never blocks announcement bookkeeping."""
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc is None:
            return
        try:
            prefs = await asyncio.to_thread(
                reminder_svc.get_preferences, pet.discord_id, pet.guild_id
            )
            if not prefs.get("pet_enabled"):
                return
            user = self.bot.get_user(pet.discord_id) or await self.bot.fetch_user(
                pet.discord_id
            )
            embed, file = await asyncio.to_thread(pet_embeds.build_death_embed, pet)
            await user.send(embed=embed, file=file)
        except Exception:
            logger.debug(
                "Pet death DM failed for user %s", pet.discord_id, exc_info=True
            )

    async def _deliver_refund(self, notice: RefundNotice) -> None:
        channel = self._pet_channel(notice.guild_id)
        if channel is not None:
            embed = pet_embeds.build_refund_embed(notice)
            try:
                await channel.send(embed=embed)
            except discord.Forbidden:
                pass
        await asyncio.to_thread(self.pet_service.mark_refund_announced, notice)


async def setup(bot: commands.Bot):
    pet_service = getattr(bot, "pet_service", None)
    if pet_service is None:
        # Feature is channel-gated: without PET_CHANNEL_ID the container
        # leaves the service unset and pets stay entirely off — no commands,
        # no sweep loop, no announcements.
        logger.info("Pets disabled (PET_CHANNEL_ID not configured); skipping cog")
        return
    await bot.add_cog(PetCommands(bot, pet_service))
