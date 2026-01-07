"""
Registration commands for the bot: /register, /setroles, /stats
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from config import (
    BANKRUPTCY_PENALTY_RATE,
    MMR_MODAL_RETRY_LIMIT,
    MMR_MODAL_TIMEOUT_MINUTES,
)
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI, format_role_display
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.registration")


class RegistrationCommands(commands.Cog):
    """Commands for player registration and profile management."""

    def __init__(
        self,
        bot: commands.Bot,
        db,
        player_service,
        role_emojis: dict,
        role_names: dict,
        bankruptcy_service=None,
    ):
        self.bot = bot
        self.db = db
        self.player_service = player_service
        self.role_emojis = role_emojis
        self.role_names = role_names
        self.bankruptcy_service = bankruptcy_service

    @app_commands.command(name="register", description="Register yourself as a player")
    @app_commands.describe(steam_id="Steam32 ID (found in your Dotabuff URL)")
    async def register(self, interaction: discord.Interaction, steam_id: int):
        """Register a new player."""
        logger.info(
            f"Register command: User {interaction.user.id} ({interaction.user}) registering with Steam ID {steam_id}"
        )

        # Defer response since OpenDota API call might take time
        if not await safe_defer(interaction, ephemeral=True):
            return

        async def _finalize_register(mmr_override: int | None = None):
            result = self.player_service.register_player(
                discord_id=interaction.user.id,
                discord_username=str(interaction.user),
                steam_id=steam_id,
                mmr_override=mmr_override,
            )
            await interaction.followup.send(
                f"✅ Registered {interaction.user.mention}!\n"
                f"Cama Rating: {result['cama_rating']} (±{result['uncertainty']:.0f}% uncertainty)\n"
                f"Use `/setroles` to set your preferred roles."
            )

        try:
            await _finalize_register()
            return
        except ValueError as e:
            error_msg = str(e)
            if "MMR not available" not in error_msg:
                await interaction.followup.send(f"❌ {error_msg}", ephemeral=True)
                return
            # Otherwise prompt for MMR below
        except Exception as e:
            logger.error(
                f"Error in register command for user {interaction.user.id}: {str(e)}", exc_info=True
            )
            await interaction.followup.send(
                "❌ Unexpected error registering you. Try again later.", ephemeral=True
            )
            return

        # Prompt for MMR via a button -> modal flow.
        # Modals can't be shown from a deferred interaction response directly, so we attach a view with a button.
        class MMRModal(discord.ui.Modal):
            def __init__(self, retries_remaining: int):
                super().__init__(title="Enter MMR", timeout=MMR_MODAL_TIMEOUT_MINUTES * 60)
                self.retries_remaining = retries_remaining
                self.mmr_input = discord.ui.TextInput(
                    label="Enter your MMR",
                    placeholder=None,
                    required=False,
                    style=discord.TextStyle.short,
                )
                self.add_item(self.mmr_input)
                self.value: int | None = None
                self.error: str | None = None

            async def on_submit(self, interaction_modal: discord.Interaction):
                raw = self.mmr_input.value.strip() if self.mmr_input.value else ""
                if not raw:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                try:
                    mmr_val = int(raw)
                except ValueError:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                if mmr_val < 0 or mmr_val > 12000:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                self.value = mmr_val
                await interaction_modal.response.send_message("✅ MMR received", ephemeral=True)

        class MMRPromptView(discord.ui.View):
            def __init__(self):
                super().__init__(timeout=MMR_MODAL_TIMEOUT_MINUTES * 60)
                self.attempts_left = MMR_MODAL_RETRY_LIMIT

            @discord.ui.button(label="Enter MMR", style=discord.ButtonStyle.primary)
            async def enter_mmr(  # type: ignore[override]
                self, button: discord.ui.Button, interaction_btn: discord.Interaction
            ):
                if self.attempts_left <= 0:
                    await interaction_btn.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return

                modal = MMRModal(retries_remaining=self.attempts_left)
                await interaction_btn.response.send_modal(modal)
                await modal.wait()

                if modal.value is None:
                    # cancelled/invalid/timeout treated as invalid attempt (per our "require user input" flow)
                    self.attempts_left -= 1
                    if self.attempts_left <= 0:
                        button.disabled = True
                        await interaction_btn.followup.send("❌ Invalid MMR", ephemeral=True)
                    return

                try:
                    await _finalize_register(mmr_override=modal.value)
                except Exception as e:
                    logger.error(
                        f"Error finalizing register after modal for user {interaction.user.id}: {e}",
                        exc_info=True,
                    )
                    await interaction_btn.followup.send(
                        "❌ Error finalizing registration. Try again later.", ephemeral=True
                    )
                    return

                # Success -> disable button
                button.disabled = True
                self.stop()

        await interaction.followup.send(
            "⚠️ OpenDota could not find your MMR. Click **Enter MMR** to finish registering.",
            ephemeral=True,
            view=MMRPromptView(),
        )
        return

    @app_commands.command(name="setroles", description="Set your preferred roles")
    @app_commands.describe(roles="Roles (1-5, e.g., '123' or '1,2,3' for carry, mid, offlane)")
    async def set_roles(self, interaction: discord.Interaction, roles: str):
        """Set player's preferred roles."""
        logger.info(
            f"SetRoles command: User {interaction.user.id} ({interaction.user}) setting roles: {roles}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        try:
            # Parse roles and validate (commas optional)
            cleaned = roles.replace(",", "").replace(" ", "")
            role_list = list(cleaned)

            valid_choices = ["1", "2", "3", "4", "5"]
            for r in role_list:
                if r not in valid_choices:
                    valid_roles = ", ".join([format_role_display(role) for role in valid_choices])
                    await safe_followup(
                        interaction,
                        content=f"❌ Invalid role: {r}. Roles must be 1-5:\n{valid_roles}",
                        ephemeral=True,
                    )
                    return

            if not role_list:
                await safe_followup(
                    interaction, content="❌ Please provide at least one role.", ephemeral=True
                )
                return

            # Deduplicate roles while preserving order
            role_list = list(dict.fromkeys(role_list))

            self.player_service.set_roles(interaction.user.id, role_list)

            role_display = ", ".join([format_role_display(r) for r in role_list])
            await interaction.followup.send(f"✅ Set your preferred roles to: {role_display}")
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
        except Exception as e:
            logger.error(f"Error setting roles for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting roles. Try again later.",
                ephemeral=True,
            )

    @app_commands.command(name="stats", description="View player stats (your own by default)")
    @app_commands.describe(user="Optional Discord user to look up (mention or select)")
    async def stats(self, interaction: discord.Interaction, user: discord.Member | None = None):
        """View player statistics for yourself or another registered player."""
        logger.info(
            "Stats command: User %s (%s) requested stats for %s",
            interaction.user.id,
            interaction.user,
            user.id if user else "self",
        )
        if not await safe_defer(interaction, ephemeral=False):
            return

        target_discord_id = user.id if user else interaction.user.id

        try:
            stats = self.player_service.get_stats(target_discord_id)
            player = stats["player"]

            # Check for bankruptcy penalty
            penalty_games = 0
            if self.bankruptcy_service:
                state = self.bankruptcy_service.get_state(target_discord_id)
                penalty_games = state.penalty_games_remaining

            # Add tombstone to title if player has active bankruptcy penalty
            title_prefix = f"{TOMBSTONE_EMOJI} " if penalty_games > 0 else ""
            embed = discord.Embed(
                title=f"📊 Stats for {title_prefix}{player.name}", color=discord.Color.green()
            )

            if stats["cama_rating"] is not None:
                embed.add_field(
                    name="Cama Rating",
                    value=f"{stats['cama_rating']} (±{stats['uncertainty']:.0f}%)",
                    inline=True,
                )
            else:
                embed.add_field(name="Cama Rating", value="Not set", inline=True)

            embed.add_field(name="Wins", value=str(player.wins), inline=True)
            embed.add_field(name="Losses", value=str(player.losses), inline=True)
            win_rate = stats["win_rate"]
            embed.add_field(
                name="Win Rate",
                value=f"{win_rate:.1f}%" if win_rate is not None else "N/A",
                inline=True,
            )
            embed.add_field(
                name="Jopacoin Balance",
                value=f"{stats['jopacoin_balance']} {JOPACOIN_EMOTE}",
                inline=True,
            )

            if player.main_role:
                embed.add_field(name="Main Role", value=player.main_role, inline=True)

            if player.preferred_roles:
                role_display = ", ".join([format_role_display(r) for r in player.preferred_roles])
                embed.add_field(name="Preferred Roles", value=role_display, inline=False)

            # Add hero stats from enriched matches (if available)
            match_repo = getattr(self.bot, "match_repo", None)
            if match_repo and hasattr(match_repo, "get_player_hero_stats"):
                try:
                    from utils.hero_lookup import get_hero_name

                    hero_stats = match_repo.get_player_hero_stats(target_discord_id)

                    # Only process if we got a valid dict back
                    if isinstance(hero_stats, dict):
                        # Last played hero
                        if hero_stats.get("last_hero_id"):
                            last_hero = get_hero_name(hero_stats["last_hero_id"])
                            embed.add_field(name="Last Played", value=last_hero, inline=True)

                        # Most played heroes
                        if hero_stats.get("hero_counts"):
                            hero_lines = []
                            for hero_id, games, wins in hero_stats["hero_counts"][:3]:
                                hero_name = get_hero_name(hero_id)
                                winrate = (wins / games * 100) if games > 0 else 0
                                hero_lines.append(f"{hero_name}: {games}g ({winrate:.0f}%)")
                            if hero_lines:
                                embed.add_field(
                                    name="Top Heroes", value="\n".join(hero_lines), inline=True
                                )
                except Exception as e:
                    # Hero stats are optional, don't fail the whole command
                    logger.debug(f"Could not fetch hero stats: {e}")

            # Show bankruptcy penalty info if active
            if penalty_games > 0:
                penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
                embed.add_field(
                    name=f"{TOMBSTONE_EMOJI} Bankruptcy Penalty",
                    value=f"{penalty_rate_pct}% win bonus for {penalty_games} more game(s)",
                    inline=False,
                )

            await interaction.followup.send(embed=embed)

        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
        except Exception as e:
            logger.error(f"Error fetching stats for {target_discord_id}: {e}", exc_info=True)
            await safe_followup(
                interaction, content="❌ Failed to fetch stats. Try again later.", ephemeral=True
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    # Get db and config from bot
    db = getattr(bot, "db", None)
    player_service = getattr(bot, "player_service", None)
    role_emojis = getattr(bot, "role_emojis", {})
    role_names = getattr(bot, "role_names", {})
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)

    await bot.add_cog(
        RegistrationCommands(
            bot, db, player_service, role_emojis, role_names, bankruptcy_service
        )
    )
