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

from commands.checks import require_guild, require_pet_channel
from commands.pet_helpers import brawl_embeds
from commands.pet_helpers import embeds as pet_embeds
from commands.pet_helpers.brawl_views import PetBrawlChallengeView, PetBrawlSession
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
    UNHATCHED_SPECIES,
    get_accessory,
)
from services.pet_flavor_service import PetFlavorEvent
from utils.formatting import JOPACOIN_EMOTE
from utils.game_date import game_date_for_timestamp
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from domain.models.pet import DeathNotice, HatchNotice, RefundNotice
    from services.pet_brawl_service import PetBrawlService
    from services.pet_flavor_service import PetFlavorService
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

    def __init__(
        self,
        bot: commands.Bot,
        pet_service: PetService,
        pet_brawl_service: PetBrawlService,
        pet_flavor_service: PetFlavorService | None = None,
    ):
        self.bot = bot
        self.pet_service = pet_service
        self.pet_brawl_service = pet_brawl_service
        self.pet_flavor_service = pet_flavor_service
        # In-memory battle state per active brawl (see brawl_views docstring).
        self._brawl_sessions: dict[int, PetBrawlSession] = {}

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
        flavor_event: PetFlavorEvent = PetFlavorEvent.STATUS,
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
        flavor_text = None
        flavor_pet = status.pet or status.last_dead
        if flavor_pet is not None:
            flavor_event = flavor_event if status.pet is not None else PetFlavorEvent.DIED
            flavor_text = await self._generate_pet_flavor(
                flavor_event,
                flavor_pet,
                status=status,
            )
        brawl_record = None
        if (
            status.pet is not None
            and status.stage != PetStage.EGG
            and self.pet_brawl_service is not None
        ):
            brawl_record = await asyncio.to_thread(
                self.pet_brawl_service.record, status.pet.pet_id, guild_id
            )
        embed, file = await asyncio.to_thread(
            pet_embeds.build_status_embed,
            status,
            self.pet_service.decay_per_day,
            now,
            owner_name=owner_name,
            next_fee=next_fee,
            flavor_text=flavor_text,
            brawl_record=brawl_record,
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

    async def _generate_pet_flavor(
        self,
        event: PetFlavorEvent,
        pet,
        *,
        status=None,
    ) -> str | None:
        flavor_service = getattr(self, "pet_flavor_service", None)
        if flavor_service is None:
            return None
        try:
            return await flavor_service.generate(event, pet=pet, status=status)
        except Exception:
            logger.warning(
                "Pet flavor generation failed for event=%s pet=%s",
                event.value,
                pet.pet_id,
                exc_info=True,
            )
            return None

    async def _rearm_warning(self, pet) -> None:
        """(Re)schedule the opt-in hungry-DM for the pet's next warning crossing.

        The preference read happens off-loop; schedule_pet_reminder itself must
        run on the loop (it creates the asyncio task).
        """
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if (
            reminder_svc is None
            or pet is None
            or pet.species == UNHATCHED_SPECIES
        ):
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
        upgraded = result.value["upgraded"]
        flair = "a **gilded egg**" if gilded else "an egg"
        pity_line = (
            "\n✨ The nonprofit took pity: this egg can't be Common."
            if result.value["pity_active"]
            else ""
        )
        if upgraded:
            description = (
                f"**{interaction.user.display_name}** upgraded their egg to "
                f"a **gilded egg** and named it **{adopted.name}** for "
                f"{GILDED_EGG_PREMIUM} {JOPACOIN_EMOTE}.\n"
                f"It still hatches <t:{adopted.hatched_at}:R>. What's inside? "
                "Nobody knows. Not even the egg."
            )
        else:
            description = (
                f"**{interaction.user.display_name}** adopted {flair} and named it "
                f"**{adopted.name}** for {adopted.adopt_fee} {JOPACOIN_EMOTE}.\n"
                f"It hatches <t:{adopted.hatched_at}:R>. What's inside? "
                "Nobody knows. Not even the egg." + pity_line
            )
        embed = discord.Embed(
            title="🥚 A gilded egg!" if gilded else "🥚 A mysterious egg!",
            description=description,
            color=pet_embeds.COLOR_EGG,
        )
        flavor_text = await self._generate_pet_flavor(
            PetFlavorEvent.ADOPTED,
            adopted,
        )
        if flavor_text:
            embed.add_field(
                name="💬 Cama chatter",
                value=flavor_text,
                inline=False,
            )
        file = await asyncio.to_thread(
            pet_embeds.get_egg_card, adopted.pet_id
        )
        if file:
            embed.set_image(url=f"attachment://{file.filename}")
        await safe_followup(interaction, embed=embed, file=file)

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
        rl = GLOBAL_RATE_LIMITER.check(
            scope="pet_status",
            guild_id=guild_id or 0,
            user_id=interaction.user.id,
            limit=6,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s.",
                ephemeral=True,
            )
            return
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
        flavor_status = None
        if getattr(self, "pet_flavor_service", None) is not None:
            flavor_status = (
                await asyncio.to_thread(
                    self.pet_service.get_status,
                    interaction.user.id,
                    guild_id,
                )
            ).value
        if outcome.spat:
            flavor_text = await self._generate_pet_flavor(
                PetFlavorEvent.SPAT,
                outcome.pet,
                status=flavor_status,
            )
            flavor_line = f"\n💬 {flavor_text}" if flavor_text else ""
            await safe_followup(
                interaction,
                content=(
                    f"💢 **{outcome.pet.name}** spat the {food.display_name} "
                    "straight back at you. The temperament of legends. "
                    f"({outcome.remaining_qty} left)"
                    f"{flavor_line}"
                ),
                ephemeral=True,
            )
            return
        flavor_text = await self._generate_pet_flavor(
            PetFlavorEvent.FED,
            outcome.pet,
            status=flavor_status,
        )
        flavor_line = f"\n💬 {flavor_text}" if flavor_text else ""
        bar = pet_embeds.hunger_bar(outcome.new_hunger)
        await safe_followup(
            interaction,
            content=(
                f"{food.emoji} **{outcome.pet.name}** munches the "
                f"{food.display_name}. Hunger {outcome.old_hunger} → "
                f"**{outcome.new_hunger}** `{bar}` · {outcome.remaining_qty} left · "
                f"{outcome.feeds_left_today} feeds left today"
                f"{flavor_line}"
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

    @pet.command(
        name="brawl",
        description="Challenge someone to a pet brawl (no coin — hunger and honor)",
    )
    @app_commands.describe(user="Whose cama to challenge")
    @require_guild
    async def brawl(self, interaction: discord.Interaction, user: discord.Member):
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
        if user.bot:
            await interaction.response.send_message(
                "Bots keep no pets. Yet.", ephemeral=True
            )
            return
        if not await require_pet_channel(interaction):
            return
        if not await safe_defer(interaction, ephemeral=False):
            return
        result = await asyncio.to_thread(
            self.pet_brawl_service.challenge,
            interaction.user.id,
            user.id,
            guild_id,
            interaction.channel_id or 0,
        )
        if not result.success:
            await safe_followup(interaction, content=f"❌ {result.error}", ephemeral=True)
            return
        challenge = result.value
        embed, file = await asyncio.to_thread(
            brawl_embeds.build_challenge_embed,
            challenge["brawl"],
            challenge["challenger_pet"],
            challenge["recipient_pet"],
            self.pet_service._now(),
        )
        view = PetBrawlChallengeView(self, challenge["brawl"])
        message = await safe_followup(
            interaction,
            content=f"<@{user.id}> — you've been challenged!",
            embed=embed,
            file=file,
            view=view,
            allowed_mentions=discord.AllowedMentions(
                everyone=False, roles=False, users=[user]
            ),
        )
        view.message = message

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
        records = {}
        if self.pet_brawl_service is not None and result.value:
            records = await asyncio.to_thread(
                self.pet_brawl_service.records_for,
                [pet.pet_id for pet in result.value],
                guild_id,
            )
        embed = pet_embeds.build_leaderboard_embed(
            result.value,
            self.pet_service.decay_per_day,
            self.pet_service._now(),
            records=records,
        )
        await safe_followup(interaction, embed=embed)

    # ────────────────────────────────────────────────────────────────────
    # Background sweep
    # ────────────────────────────────────────────────────────────────────

    @tasks.loop(minutes=10)
    async def _pet_sweep_loop(self):
        if self.pet_brawl_service is not None:
            try:
                await asyncio.to_thread(self.pet_brawl_service.sweep_stale)
            except Exception:
                logger.exception("Pet brawl sweep failed")
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
            flavor_text = await self._generate_pet_flavor(
                PetFlavorEvent.HATCHED,
                pet,
            )
            embed, file = await asyncio.to_thread(
                pet_embeds.build_hatch_embed,
                pet,
                flavor_text=flavor_text,
            )
            try:
                await channel.send(embed=embed, file=file)
            except discord.Forbidden:
                pass  # permanent: mark below so we don't loop forever
        await asyncio.to_thread(self.pet_service.mark_hatch_announced, pet)
        await self._rearm_warning(pet)

    async def _deliver_death(self, notice: DeathNotice) -> None:
        pet = notice.pet
        channel = self._pet_channel(pet.guild_id)
        flavor_text = None
        if channel is not None:
            flavor_text = await self._generate_pet_flavor(
                PetFlavorEvent.DIED,
                pet,
            )
            embed, file = await asyncio.to_thread(
                pet_embeds.build_death_embed,
                pet,
                flavor_text=flavor_text,
            )
            try:
                await channel.send(embed=embed, file=file)
            except discord.Forbidden:
                pass
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc is not None:
            reminder_svc.cancel_pet_reminder(pet.discord_id, pet.guild_id)
        await self._dm_death_notice(pet, flavor_text=flavor_text)
        await asyncio.to_thread(self.pet_service.mark_death_announced, pet)

    async def _dm_death_notice(self, pet, *, flavor_text: str | None = None) -> None:
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
            if flavor_text is None:
                flavor_text = await self._generate_pet_flavor(
                    PetFlavorEvent.DIED,
                    pet,
                )
            embed, file = await asyncio.to_thread(
                pet_embeds.build_death_embed,
                pet,
                flavor_text=flavor_text,
            )
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
    pet_brawl_service = getattr(bot, "pet_brawl_service", None)
    if pet_brawl_service is None:
        # Wiring bug: the container creates both services behind one gate.
        raise RuntimeError("pet_service present but pet_brawl_service missing")
    await bot.add_cog(
        PetCommands(
            bot,
            pet_service,
            pet_brawl_service,
            getattr(bot, "pet_flavor_service", None),
        )
    )
