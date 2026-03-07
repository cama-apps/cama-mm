"""
/roll command — Roll the cosmic dice and risk your jopacoins.

Mechanics:
- value="doggeh"    → deduct 1 coin, display "1", show a random doggeh prophecy mentioning a random guild member
- 1 ≤ N < 100       → deduct 1 coin, display random.randint(0, N)
- N ≥ 100           → display random.randint(0, N); separately 1-in-(N+1) chance to win +20 coins, else lose 1 coin
- anything else     → ephemeral error
"""

import asyncio
import random

import discord
from discord import app_commands
from discord.ext import commands

from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup

DOGGEH_MESSAGES = [
    "*sniffs the cosmic winds* {mention} carries the scent of your destiny.",
    "*tilts head at the stars* The ancient kibble speaks: {mention} holds the answer.",
    "*barks at the void* {mention} is the one the universe has chosen. Woof.",
    "*digs frantically* Beneath the earth I find your fate — it smells like {mention}.",
    "*sits very good* The prophecy is clear. {mention} shall guide your paw.",
    "*stares at the wall* There is something there. It is {mention}. Only I can see.",
    "*chases tail in sacred spiral* The loop reveals: {mention}.",
    "*howls at the moon* The moon says {mention}. I am only the messenger.",
    "*rolls in the grass* The sacred patch of grass whispers: {mention}.",
    "*drops wet ball at your feet* Accept this gift. Accept {mention}.",
    "*cocks head left, then right* Left means {mention}. Right also means {mention}.",
    "*sneezes on the oracle stone* The sneeze has spoken: {mention}.",
    "*presses cold nose to your cheek* You are the chosen one. {mention} agrees.",
    "*makes intense eye contact* {mention}. That is all. That is the whole prophecy.",
    "*steals your sock* The sock has been taken. {mention} is responsible for your future.",
    "*zooms around the room* The zoomies have aligned. {mention} is your destiny.",
    "*stares at the fridge* The fridge is silent. But {mention} is not.",
    "*falls asleep mid-prophecy* ...{mention}... *snores*",
    "*sits on the sacred rug* You may not pass until you acknowledge {mention}.",
    "*heckin' borks at the sky* Sky said {mention}. I heard it clearly.",
    "*waits patiently by the door* The door will open. {mention} holds the key.",
    "*brings you a leaf* This leaf means {mention}. Take it. It is important.",
    "*does a big stretch* The stretch reveals: {mention} is in your future.",
    "*licks your hand in cosmic acknowledgement* {mention} has been chosen.",
    "*shakes water everywhere* The droplets form the name: {mention}.",
    "*buries the prophecy in the garden* It smells like {mention} now. That's the prophecy.",
    "*twirls three times before lying down* Three twirls for {mention}.",
    "*nudges bowl* The empty bowl foretells: seek {mention}.",
    "*looks very serious for a dog* I have seen things. {mention} is one of them.",
    "*whimpers at the ancient couch* The couch has absorbed the wisdom of {mention}.",
    "*paw on your knee* The paw rests on you. The cosmos point to {mention}.",
    "*finds a stick* I have found the Stick of Fate. It belongs to {mention}.",
    "*refuses to move from this spot* The spot is sacred. {mention} knows why.",
    "*judges you with ancient eyes* {mention} has already figured this out.",
    "*dreams twitchily* In the dream there was a squirrel. It was {mention}.",
    "*perks ears at nothing* I hear it. You cannot. It says {mention}.",
    "*carries shoe across room solemnly* The shoe prophecy: {mention}.",
    "*rolls onto back* The belly-up position of surrender reveals {mention}.",
    "*watches you type* You already know the answer. It's {mention}.",
    "*hiccups cosmically* Excuse me. That was {mention} passing through.",
    "*very good sit* I am a very good dog. {mention} is a very good omen.",
    "*pretends not to hear then looks* I heard. The answer is {mention}.",
    "*eyes dart rapidly* I sensed a treat and also {mention}'s significance.",
    "*full body wag* Great news! The wag indicates: {mention}!",
    "*sniffs your shoes extensively* You have been near {mention}. This matters.",
    "*stands at window watching rain* The rain spells out {mention} in puddles.",
    "*head on paws, eyes up* I am thinking about {mention} and also cheese.",
    "*slow blink* The slow blink of cosmic wisdom confirms: {mention}.",
    "*aggressively demands pets* The pets must flow. And also: {mention}.",
    "*circles the prophecy circle* Around and around... the center is {mention}.",
    "*big yawn* I have seen the future between these teeth. It contains {mention}.",
    "*has been a good dog this whole time* The good dog energy confirms {mention}.",
]


class RollCommands(commands.Cog):
    def __init__(self, bot, player_service):
        self.bot = bot
        self.player_service = player_service

    @app_commands.command(name="roll", description="Roll the cosmic dice and risk your jopacoins!")
    @app_commands.describe(value="A positive integer to roll up to, or 'doggeh' for mystical prophecy")
    async def roll(self, interaction: discord.Interaction, value: str):
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Registration check
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/register` before rolling. The dice don't know who you are yet.",
                ephemeral=True,
            )
            return

        # Get mana effects
        mana_effects_service = getattr(self.bot, "mana_effects_service", None)
        effects = None
        if mana_effects_service:
            try:
                from domain.models.mana_effects import ManaEffects as _MERoll
                _fx = await asyncio.to_thread(mana_effects_service.get_effects, user_id, guild_id)
                if isinstance(_fx, _MERoll):
                    effects = _fx
            except Exception:
                pass

        # --- doggeh easter egg ---
        if value.strip().lower() == "doggeh":
            balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
            if balance < 1:
                await interaction.response.send_message(
                    f"The doggeh demands 1 {JOPACOIN_EMOTE} for prophecy, but your pockets are empty. Woof.",
                    ephemeral=True,
                )
                return

            if not await safe_defer(interaction, ephemeral=False):
                return

            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -1)
            new_balance = balance - 1

            members = [m for m in interaction.guild.members if not m.bot] if interaction.guild else []
            target = random.choice(members) if members else interaction.user
            prophecy = random.choice(DOGGEH_MESSAGES).format(mention=target.mention)

            await safe_followup(
                interaction,
                content=(
                    f"{interaction.user.mention} rolled **1**\n"
                    f"{prophecy}\n"
                    f"-1 {JOPACOIN_EMOTE} — new balance: **{new_balance}**"
                ),
            )
            return

        # --- integer parse ---
        try:
            n = int(value)
        except ValueError:
            await interaction.response.send_message(
                f"'{value}' isn't a valid roll. Use a positive integer or `doggeh`.",
                ephemeral=True,
            )
            return

        if n < 1:
            await interaction.response.send_message(
                "The dice need somewhere to land. Use a positive integer (≥ 1).",
                ephemeral=True,
            )
            return

        # Balance check
        roll_cost = effects.red_roll_cost if effects else 1
        jackpot_amount = effects.red_roll_jackpot if effects else 20

        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        if balance < roll_cost:
            await interaction.response.send_message(
                f"You need at least {roll_cost} {JOPACOIN_EMOTE} to roll. The cosmos charge a fee.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        result = random.randint(0, n)

        if n >= 100:
            won = random.randint(0, n) == 0
            if won:
                win_amount = jackpot_amount
                # Green gain cap
                if effects and effects.green_gain_cap is not None:
                    win_amount = min(win_amount, effects.green_gain_cap)
                # Green steady bonus
                if effects and effects.green_steady_bonus > 0:
                    win_amount += effects.green_steady_bonus
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, win_amount)
                new_balance = balance + win_amount
                outcome = f"+{win_amount} {JOPACOIN_EMOTE} **JACKPOT!** New balance: **{new_balance}**"
            else:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -roll_cost)
                new_balance = balance - roll_cost
                outcome = f"-{roll_cost} {JOPACOIN_EMOTE} — new balance: **{new_balance}**"
        else:
            await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -roll_cost)
            new_balance = balance - roll_cost
            outcome = f"-{roll_cost} {JOPACOIN_EMOTE} — new balance: **{new_balance}**"

        # Mana post-effects
        mana_notes = []
        if effects and mana_effects_service:
            # Swamp self-tax
            if effects.swamp_self_tax > 0:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -effects.swamp_self_tax)
                new_balance -= effects.swamp_self_tax
                mana_notes.append(f"🌿 Swamp tax: -{effects.swamp_self_tax} {JOPACOIN_EMOTE}")

            # Swamp siphon
            if effects.swamp_siphon:
                siphon = await asyncio.to_thread(mana_effects_service.execute_siphon, user_id, guild_id)
                if siphon:
                    new_balance += siphon["amount"]
                    mana_notes.append(f"🌿 Siphon: +{siphon['amount']} {JOPACOIN_EMOTE}")

            # Blue cashback on loss
            if roll_cost > 0 and not (n >= 100 and won):
                cashback = await asyncio.to_thread(mana_effects_service.apply_blue_cashback, user_id, guild_id, roll_cost)
                if cashback > 0:
                    new_balance += cashback
                    mana_notes.append(f"🏝️ Cashback: +{cashback} {JOPACOIN_EMOTE}")

            # Blue tax on gains (jackpot win)
            if n >= 100 and won:
                tax = await asyncio.to_thread(mana_effects_service.apply_blue_tax, user_id, guild_id, win_amount)
                if tax > 0:
                    new_balance -= tax
                    mana_notes.append(f"🏝️ Tax: -{tax} {JOPACOIN_EMOTE}")

            # Plains tithe on gains
            if n >= 100 and won:
                tithe = await asyncio.to_thread(mana_effects_service.apply_plains_tithe, user_id, guild_id, win_amount)
                if tithe > 0:
                    new_balance -= tithe
                    mana_notes.append(f"🌾 Tithe: -{tithe} {JOPACOIN_EMOTE}")

        mana_suffix = ""
        if mana_notes:
            mana_suffix = "\n" + " | ".join(mana_notes)

        await safe_followup(
            interaction,
            content=f"{interaction.user.mention} rolled **{result}** (0–{n})\n{outcome}{mana_suffix}",
        )


async def setup(bot):
    player_service = bot.player_service
    await bot.add_cog(RollCommands(bot, player_service))
