"""Discord UI views used inside the wheel-spin (`/gamba`) flows."""

from __future__ import annotations

import random

import discord


class TownTrialView(discord.ui.View):
    """Server-wide vote view for the TOWN_TRIAL bankrupt wheel mechanic."""

    def __init__(self, options: list[tuple], *, timeout: float = 300.0):
        super().__init__(timeout=timeout)
        self.options = options
        self.votes: dict[int, int] = {}  # discord_id -> option index

        for i, (label, value, _color) in enumerate(options):
            display = label if isinstance(value, str) else f"{label} JC" if isinstance(value, int) and value > 0 else label
            button = discord.ui.Button(
                label=display,
                style=discord.ButtonStyle.secondary,
                custom_id=f"tt_{i}",
            )
            button.callback = self._make_callback(i)
            self.add_item(button)

    def _make_callback(self, idx: int):
        async def callback(interaction: discord.Interaction):
            self.votes[interaction.user.id] = idx
            label = self.options[idx][0]
            await interaction.response.send_message(
                f"Voted for **{label}**!", ephemeral=True
            )
        return callback

    def get_winner(self) -> int | None:
        """Return the winning option index, or None if no votes."""
        if not self.votes:
            return None
        from collections import Counter
        counts = Counter(self.votes.values())
        max_votes = max(counts.values())
        winners = [idx for idx, cnt in counts.items() if cnt == max_votes]
        return random.choice(winners)


class DiscoverView(discord.ui.View):
    """Spinner-choice view for the DISCOVER bankrupt wheel mechanic."""

    def __init__(self, options: list[tuple], spinner_id: int, *, timeout: float = 60.0):
        super().__init__(timeout=timeout)
        self.options = options
        self.spinner_id = spinner_id
        self.chosen_idx: int | None = None

        for i, (label, value, _color) in enumerate(options):
            display = label if isinstance(value, str) else f"{label} JC" if isinstance(value, int) and value > 0 else label
            button = discord.ui.Button(
                label=display,
                style=discord.ButtonStyle.secondary,
                custom_id=f"disc_{i}",
            )
            button.callback = self._make_callback(i)
            self.add_item(button)

    def _make_callback(self, idx: int):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.spinner_id:
                await interaction.response.send_message(
                    "This choice isn't yours to make.", ephemeral=True
                )
                return
            self.chosen_idx = idx
            self.stop()
            label = self.options[idx][0]
            await interaction.response.send_message(
                f"You chose **{label}**!", ephemeral=True
            )
        return callback


class ScryingView(discord.ui.View):
    """Blue mana scrying: choose between two wheel outcomes."""

    def __init__(self, option_a: str, option_b: str, user_id: int, **kwargs):
        super().__init__(**kwargs)
        self.option_a = option_a
        self.option_b = option_b
        self.user_id = user_id
        self.chosen: str | None = None
        # Update button labels
        self.children[0].label = f"A: {option_a}"
        self.children[1].label = f"B: {option_b}"

    @discord.ui.button(label="A", style=discord.ButtonStyle.primary)
    async def choose_a(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your scrying!", ephemeral=True)
            return
        self.chosen = "A"
        await interaction.response.defer()
        self.stop()

    @discord.ui.button(label="B", style=discord.ButtonStyle.primary)
    async def choose_b(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your scrying!", ephemeral=True)
            return
        self.chosen = "B"
        await interaction.response.defer()
        self.stop()


class WheelRerollView(discord.ui.View):
    """Red mana bankrupt re-roll: one click to re-spin on LOSE/EXTEND."""

    def __init__(self, user_id: int, timeout: float = 30.0):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.clicked = False
        self.message: discord.Message | None = None

    @discord.ui.button(label="Re-roll", style=discord.ButtonStyle.danger)
    async def reroll_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your spin!", ephemeral=True)
            return
        self.clicked = True
        button.disabled = True
        try:
            await interaction.response.edit_message(view=self)
        except discord.HTTPException:
            pass
        self.stop()

    async def on_timeout(self):
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        if self.message is None:
            return
        try:
            await self.message.edit(view=self)
        except (discord.NotFound, discord.HTTPException):
            pass
