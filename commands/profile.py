"""
Unified profile command with tabbed navigation.

Provides a comprehensive view of player stats across all systems:
- Overview: Basic stats (rating, W/L, balance, roles)
- Rating: Detailed calibration and performance analysis
- Economy: Balance, loans, bankruptcy status
- Gambling: Degen score, P&L, betting patterns
- Predictions: Prediction market performance
"""

import asyncio
import functools
import logging
import time

import discord
from discord import app_commands
from discord.ext import commands

from config import BANKRUPTCY_PENALTY_RATE
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from utils.drawing import (
    draw_gamba_chart,
    draw_hero_performance_chart,
    draw_lane_distribution,
    draw_role_graph,
)
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI, format_role_display
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.rating_insights import rd_to_certainty, get_rd_tier_name

logger = logging.getLogger("cama_bot.commands.profile")

# Embed colors
COLOR_BLUE = 0x3498DB
COLOR_GREEN = 0x57F287
COLOR_RED = 0xED4245
COLOR_ORANGE = 0xF39C12


class ProfileView(discord.ui.View):
    """View with tab buttons for navigating profile sections."""

    # Tabs that require expensive operations (OpenDota API, chart generation)
    EXPENSIVE_TABS = {"dota", "gambling", "heroes"}

    def __init__(
        self,
        cog: "ProfileCommands",
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ):
        super().__init__(timeout=840)  # 14 minute timeout (interaction tokens expire at 15min)
        self.cog = cog
        self.target_user = target_user
        self.target_discord_id = target_discord_id
        self.guild_id = guild_id
        self.current_tab = "overview"
        self._last_interaction_time: dict[int, float] = {}  # user_id -> timestamp
        self.message: discord.Message | None = None  # Set after sending
        # Cache for expensive tab results: tab_name -> (embed, list of (bytes, filename) tuples)
        # Store raw bytes instead of File since File objects are single-use
        self._tab_cache: dict[str, tuple[discord.Embed, list[tuple[bytes, str]]]] = {}
        # Lock to prevent concurrent tab switches from interleaving
        self._update_lock = asyncio.Lock()
        self._update_button_styles()

    async def on_timeout(self):
        """Delete the message when the view times out."""
        logger.info(f"ProfileView timeout triggered. Message ref: {self.message}")
        if self.message:
            try:
                await self.message.delete()
                logger.info("Profile message deleted successfully on timeout")
            except discord.NotFound:
                logger.debug("Profile message was already deleted")
            except discord.HTTPException as e:
                logger.warning(f"Failed to delete expired profile message: {e}")
        else:
            logger.warning("on_timeout called but no message reference stored")

    def _update_button_styles(self):
        """Update button styles based on current tab."""
        tab_buttons = {
            "overview": self.overview_btn,
            "rating": self.rating_btn,
            "economy": self.economy_btn,
            "gambling": self.gambling_btn,
            "predictions": self.predictions_btn,
            "dota": self.dota_btn,
            "teammates": self.teammates_btn,
            "heroes": self.heroes_btn,
        }
        for tab_name, button in tab_buttons.items():
            if tab_name == self.current_tab:
                button.style = discord.ButtonStyle.primary
            else:
                button.style = discord.ButtonStyle.secondary

    async def _handle_tab_click(
        self, interaction: discord.Interaction, tab_name: str
    ):
        """Handle tab button click with rate limiting, caching, and concurrency control."""
        from io import BytesIO

        # Simple rate limit: 2 seconds between clicks per user
        now = time.time()
        last_time = self._last_interaction_time.get(interaction.user.id, 0)
        if now - last_time < 2.0:
            await interaction.response.defer()
            return
        self._last_interaction_time[interaction.user.id] = now

        # For expensive tabs, defer first to avoid 3s timeout (only if not cached)
        is_expensive = tab_name in self.EXPENSIVE_TABS
        if is_expensive and tab_name not in self._tab_cache:
            await interaction.response.defer()

        # Use lock to prevent concurrent tab switches from interleaving
        async with self._update_lock:
            self.current_tab = tab_name
            self._update_button_styles()

            files = []
            # Check cache for expensive tabs
            if tab_name in self._tab_cache:
                embed, cached_files = self._tab_cache[tab_name]
                # Create fresh Files from cached bytes
                for cached_bytes, filename in cached_files:
                    files.append(discord.File(BytesIO(cached_bytes), filename=filename))
            else:
                embed, files = await self.cog.build_tab_embed(
                    tab_name, self.target_user, self.target_discord_id, self.guild_id
                )
                # Cache expensive tab results - extract bytes from Files before they're consumed
                if is_expensive:
                    cached_files = []
                    for file in files:
                        file.fp.seek(0)
                        cached_bytes = file.fp.read()
                        file.fp.seek(0)  # Reset for use
                        cached_files.append((cached_bytes, file.filename))
                    self._tab_cache[tab_name] = (embed, cached_files)

            # Pass attachments to handle chart files - empty list clears previous chart
            attachments = files

            # Use edit_original_response if we deferred, otherwise edit_message
            if interaction.response.is_done():
                await interaction.edit_original_response(embed=embed, attachments=attachments, view=self)
            else:
                await interaction.response.edit_message(embed=embed, attachments=attachments, view=self)

    @discord.ui.button(label="Overview", style=discord.ButtonStyle.primary, row=0)
    async def overview_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "overview")

    @discord.ui.button(label="Rating", style=discord.ButtonStyle.secondary, row=0)
    async def rating_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "rating")

    @discord.ui.button(label="Economy", style=discord.ButtonStyle.secondary, row=0)
    async def economy_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "economy")

    @discord.ui.button(label="Gambling", style=discord.ButtonStyle.secondary, row=0)
    async def gambling_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "gambling")

    @discord.ui.button(label="Predictions", style=discord.ButtonStyle.secondary, row=0)
    async def predictions_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "predictions")

    @discord.ui.button(label="Dota", style=discord.ButtonStyle.secondary, row=1)
    async def dota_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "dota")

    @discord.ui.button(label="Teammates", style=discord.ButtonStyle.secondary, row=1)
    async def teammates_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "teammates")

    @discord.ui.button(label="Heroes", style=discord.ButtonStyle.secondary, row=1)
    async def heroes_btn(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_tab_click(interaction, "heroes")


class ProfileCommands(commands.Cog):
    """Unified profile command with tabbed navigation."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    def _get_player_service(self):
        return getattr(self.bot, "player_service", None)

    def _get_player_repo(self):
        return getattr(self.bot, "player_repo", None)

    def _get_match_repo(self):
        return getattr(self.bot, "match_repo", None)

    def _get_bankruptcy_service(self):
        return getattr(self.bot, "bankruptcy_service", None)

    def _get_loan_service(self):
        return getattr(self.bot, "loan_service", None)

    def _get_gambling_stats_service(self):
        return getattr(self.bot, "gambling_stats_service", None)

    def _get_prediction_service(self):
        return getattr(self.bot, "prediction_service", None)

    def _get_pairings_repo(self):
        return getattr(self.bot, "pairings_repo", None)

    def _get_tip_repository(self):
        return getattr(self.bot, "tip_repository", None)

    def _get_rating_system(self):
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            return match_service.rating_system
        return CamaRatingSystem()

    async def build_tab_embed(
        self,
        tab_name: str,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, list[discord.File]]:
        """Build the embed and optional chart files for a specific tab."""
        builders = {
            "overview": self._build_overview_embed,
            "rating": self._build_rating_embed,
            "economy": self._build_economy_embed,
            "gambling": self._build_gambling_embed,
            "predictions": self._build_predictions_embed,
            "dota": self._build_dota_embed,
            "teammates": self._build_teammates_embed,
            "heroes": self._build_heroes_embed,
        }
        builder = builders.get(tab_name, self._build_overview_embed)
        result = await builder(target_user, target_discord_id, guild_id)
        # Normalize return value to always return a list of files
        embed = result[0]
        file_or_files = result[1]
        if file_or_files is None:
            return embed, []
        elif isinstance(file_or_files, list):
            return embed, file_or_files
        else:
            return embed, [file_or_files]

    async def _build_overview_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Overview tab embed."""
        player_service = self._get_player_service()
        bankruptcy_service = self._get_bankruptcy_service()
        player_repo = self._get_player_repo()

        if not player_service:
            return discord.Embed(
                title="Error", description="Player service unavailable", color=COLOR_RED
            ), None

        try:
            stats = player_service.get_stats(target_discord_id, guild_id)
        except ValueError:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.\nUse `/register` to get started.",
                color=COLOR_RED,
            ), None

        player = stats["player"]

        # Check for bankruptcy penalty
        penalty_games = 0
        if bankruptcy_service:
            state = bankruptcy_service.get_state(target_discord_id, guild_id)
            penalty_games = state.penalty_games_remaining

        # Title with tombstone if penalized
        title_prefix = f"{TOMBSTONE_EMOJI} " if penalty_games > 0 else ""
        embed = discord.Embed(
            title=f"{title_prefix}Profile: {target_user.display_name}",
            color=COLOR_BLUE,
        )

        # Rating
        if stats["cama_rating"] is not None:
            certainty = 100 - stats["uncertainty"]
            rating_system = self._get_rating_system()
            rating_display = rating_system.rating_to_display(stats["cama_rating"])
            embed.add_field(
                name="Rating",
                value=f"{rating_display} ({certainty:.0f}% certain)",
                inline=True,
            )
        else:
            embed.add_field(name="Rating", value="Not set", inline=True)

        # Record
        total_games = player.wins + player.losses
        win_rate = stats["win_rate"]
        if total_games > 0:
            embed.add_field(
                name="Record",
                value=f"{player.wins}W-{player.losses}L ({win_rate:.0f}%)",
                inline=True,
            )
        else:
            embed.add_field(name="Record", value="No games", inline=True)

        # Balance
        balance = stats["jopacoin_balance"]
        balance_emoji = "" if balance >= 0 else "⚠️ "
        embed.add_field(
            name="Balance",
            value=f"{balance_emoji}{balance} {JOPACOIN_EMOTE}",
            inline=True,
        )

        # Roles
        if player.preferred_roles:
            role_display = ", ".join([format_role_display(r) for r in player.preferred_roles])
            embed.add_field(name="Roles", value=role_display, inline=True)
        else:
            embed.add_field(name="Roles", value="Not set", inline=True)

        # Main role if different
        if player.main_role:
            embed.add_field(name="Main", value=format_role_display(player.main_role), inline=True)

        # Hero stats from enriched matches
        match_repo = self._get_match_repo()
        if match_repo and hasattr(match_repo, "get_player_hero_stats"):
            try:
                from utils.hero_lookup import get_hero_name

                hero_stats = match_repo.get_player_hero_stats(target_discord_id, guild_id)
                if isinstance(hero_stats, dict):
                    hero_lines = []
                    if hero_stats.get("last_hero_id"):
                        last_hero = get_hero_name(hero_stats["last_hero_id"])
                        hero_lines.append(f"**Last:** {last_hero}")
                    if hero_stats.get("hero_counts"):
                        top_heroes = []
                        for hero_id, games, wins in hero_stats["hero_counts"][:3]:
                            hero_name = get_hero_name(hero_id)
                            winrate = (wins / games * 100) if games > 0 else 0
                            top_heroes.append(f"{hero_name} ({games}g, {winrate:.0f}%)")
                        if top_heroes:
                            hero_lines.append(f"**Top:** {', '.join(top_heroes)}")
                    if hero_lines:
                        embed.add_field(name="Heroes", value="\n".join(hero_lines), inline=False)
            except Exception as e:
                logger.debug(f"Could not fetch hero stats: {e}")

        # Bankruptcy penalty warning
        if penalty_games > 0:
            penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
            embed.add_field(
                name=f"{TOMBSTONE_EMOJI} Bankruptcy Penalty",
                value=f"{penalty_rate_pct}% win bonus for {penalty_games} more game(s)",
                inline=False,
            )

        return embed, None

    async def _build_rating_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Rating tab embed with detailed calibration stats."""
        player_repo = self._get_player_repo()
        match_repo = self._get_match_repo()
        rating_system = self._get_rating_system()

        if not player_repo:
            return discord.Embed(
                title="Error", description="Player repository unavailable", color=COLOR_RED
            ), None

        player = player_repo.get_by_id(target_discord_id, guild_id)
        if not player:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ), None

        # Get rating history for trend analysis
        history = []
        if match_repo and hasattr(match_repo, "get_player_rating_history_detailed"):
            history = match_repo.get_player_rating_history_detailed(target_discord_id, guild_id, limit=50)

        # Calculate percentile
        all_players = player_repo.get_all(guild_id)
        rated_players = [p for p in all_players if p.glicko_rating is not None]
        percentile = None
        if player.glicko_rating and rated_players:
            lower_count = sum(1 for p in rated_players if (p.glicko_rating or 0) < player.glicko_rating)
            percentile = (lower_count / len(rated_players)) * 100

        # Calculate calibration tier and trend color
        rd = player.glicko_rd or 350
        calibration_tier = get_rd_tier_name(rd)

        # Determine color based on recent trend
        color = COLOR_BLUE
        last_5_delta = None
        if len(history) >= 2:
            if len(history) > 5:
                last_5_delta = (history[0].get("rating") or 0) - (history[4].get("rating") or 0)
            else:
                last_5_delta = (history[0].get("rating") or 0) - (history[-1].get("rating") or 0)
            if last_5_delta and last_5_delta > 10:
                color = COLOR_GREEN
            elif last_5_delta and last_5_delta < -10:
                color = COLOR_RED

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Rating",
            color=color,
        )

        # Rating profile
        rating_display = rating_system.rating_to_display(player.glicko_rating) if player.glicko_rating else "N/A"
        certainty = rd_to_certainty(rd)
        percentile_text = f"Top {100 - percentile:.0f}%" if percentile is not None else "N/A"

        profile_lines = [
            f"**Rating:** {rating_display} ({certainty:.0f}% certain)",
            f"**Tier:** {calibration_tier} | **Percentile:** {percentile_text}",
        ]
        if player.glicko_volatility:
            profile_lines.append(f"**Volatility:** {player.glicko_volatility:.3f}")
        embed.add_field(name="Rating Profile", value="\n".join(profile_lines), inline=False)

        # Drift from initial seed
        if player.initial_mmr and player.glicko_rating:
            seed_rating = rating_system.mmr_to_rating(player.initial_mmr)
            drift = player.glicko_rating - seed_rating
            drift_emoji = "+" if drift > 0 else "" if drift < 0 else ""
            arrow = "📈" if drift > 0 else "📉" if drift < 0 else "➡️"
            embed.add_field(
                name="Rating Drift",
                value=f"{arrow} **{drift_emoji}{drift:.0f}** rating vs initial seed ({player.initial_mmr} MMR)",
                inline=False,
            )

        # Performance vs expectations
        matches_with_predictions = [h for h in history if h.get("expected_team_win_prob") is not None]
        if matches_with_predictions:
            actual_wins = sum(1 for h in matches_with_predictions if h.get("won"))
            expected_wins = sum(h.get("expected_team_win_prob", 0) for h in matches_with_predictions)
            overperformance = actual_wins - expected_wins

            over_emoji = "🔥" if overperformance > 0 else "💀" if overperformance < 0 else "➡️"
            embed.add_field(
                name="Performance",
                value=(
                    f"**Actual Wins:** {actual_wins} | **Expected:** {expected_wins:.1f}\n"
                    f"**Over/Under:** {over_emoji} {overperformance:+.1f} wins"
                ),
                inline=True,
            )

            # Win rates when favored vs underdog
            favored_matches = [h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) >= 0.55]
            underdog_matches = [h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) <= 0.45]
            favored_wins = sum(1 for h in favored_matches if h.get("won"))
            underdog_wins = sum(1 for h in underdog_matches if h.get("won"))

            winrate_lines = []
            if favored_matches:
                winrate_lines.append(f"**Favored (55%+):** {favored_wins}/{len(favored_matches)} ({favored_wins/len(favored_matches):.0%})")
            if underdog_matches:
                winrate_lines.append(f"**Underdog (45%-):** {underdog_wins}/{len(underdog_matches)} ({underdog_wins/len(underdog_matches):.0%})")
            if winrate_lines:
                embed.add_field(name="Situational", value="\n".join(winrate_lines), inline=True)

        # Trend and streak
        if last_5_delta is not None:
            trend_emoji = "📈" if last_5_delta > 0 else "📉" if last_5_delta < 0 else "➡️"
            trend_text = f"{trend_emoji} **{last_5_delta:+.0f}** over last {min(5, len(history))} games"

            # Calculate current streak
            streak = 0
            streak_type = None
            for h in matches_with_predictions:
                won = h.get("won")
                if streak_type is None:
                    streak_type = "W" if won else "L"
                    streak = 1
                elif (won and streak_type == "W") or (not won and streak_type == "L"):
                    streak += 1
                else:
                    break

            if streak and streak_type:
                streak_emoji = "🔥" if streak_type == "W" else "💀"
                trend_text += f"\n{streak_emoji} Current: **{streak_type}{streak}** streak"

            embed.add_field(name="Trend", value=trend_text, inline=True)

        # Recent matches (last 5) with Glicko-2 and OpenSkill predictions
        if matches_with_predictions:
            os_system = CamaOpenSkillSystem()
            recent_lines = []
            for h in matches_with_predictions[:5]:
                glicko_prob = h.get("expected_team_win_prob", 0.5)
                won = h.get("won")
                match_id = h.get("match_id")
                expected_win = glicko_prob >= 0.5

                # Get OpenSkill expected outcome
                os_prob = None
                if match_id and match_repo:
                    os_ratings = match_repo.get_os_ratings_for_match(match_id)
                    if os_ratings["team1"] and os_ratings["team2"]:
                        team_num = h.get("team_number")
                        if team_num == 1:
                            os_prob = os_system.os_predict_win_probability(
                                os_ratings["team1"], os_ratings["team2"]
                            )
                        elif team_num == 2:
                            os_prob = os_system.os_predict_win_probability(
                                os_ratings["team2"], os_ratings["team1"]
                            )

                if won:
                    emoji = "✅" if expected_win else "🔥"  # expected win or upset
                else:
                    emoji = "💀" if expected_win else "❌"  # choke or expected loss

                # Build prediction string with both systems
                pred_str = f"G:{glicko_prob:.0%}"
                if os_prob is not None:
                    pred_str += f" O:{os_prob:.0%}"
                recent_lines.append(f"{emoji} {pred_str} → {'W' if won else 'L'}")

            embed.add_field(
                name=f"Recent ({len(recent_lines)}) G=Glicko O=OS",
                value="\n".join(recent_lines),
                inline=True,
            )

        # Highlights (biggest upset and choke)
        upsets = [(h, h.get("expected_team_win_prob", 0.5)) for h in matches_with_predictions
                  if h.get("won") and (h.get("expected_team_win_prob") or 0.5) < 0.45]
        chokes = [(h, h.get("expected_team_win_prob", 0.5)) for h in matches_with_predictions
                  if not h.get("won") and (h.get("expected_team_win_prob") or 0.5) > 0.55]
        upsets.sort(key=lambda x: x[1])
        chokes.sort(key=lambda x: x[1], reverse=True)

        highlights = []
        if upsets:
            best_upset = upsets[0]
            highlights.append(f"🔥 **Best Upset:** Won @ {best_upset[1]:.0%} (Match #{best_upset[0].get('match_id')})")
        if chokes:
            worst_choke = chokes[0]
            highlights.append(f"💀 **Worst Choke:** Lost @ {worst_choke[1]:.0%} (Match #{worst_choke[0].get('match_id')})")
        if highlights:
            embed.add_field(name="Highlights", value="\n".join(highlights), inline=False)

        # Record
        total_games = player.wins + player.losses
        if total_games > 0:
            embed.add_field(
                name="Record",
                value=f"**W-L:** {player.wins}-{player.losses} ({player.wins / total_games:.0%})",
                inline=True,
            )

        embed.set_footer(text="Tip: Use /calibration for full analysis | ✅=expected W | 🔥=upset | ❌=expected L | 💀=choke")

        return embed, None

    async def _build_economy_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Economy tab embed with balance, loans, and bankruptcy."""
        player_repo = self._get_player_repo()
        loan_service = self._get_loan_service()
        bankruptcy_service = self._get_bankruptcy_service()

        if not player_repo:
            return discord.Embed(
                title="Error", description="Player repository unavailable", color=COLOR_RED
            ), None

        player = player_repo.get_by_id(target_discord_id, guild_id)
        if not player:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ), None

        balance = player.jopacoin_balance or 0

        # Determine color based on balance
        if balance > 0:
            color = COLOR_GREEN
        elif balance < 0:
            color = COLOR_RED
        else:
            color = COLOR_ORANGE

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Economy",
            color=color,
        )

        # Balance with visual indicator
        balance_emoji = "💰" if balance > 0 else "⚠️" if balance < 0 else "📭"
        embed.add_field(
            name=f"{balance_emoji} Balance",
            value=f"**{balance}** {JOPACOIN_EMOTE}",
            inline=False,
        )

        # Loan information
        if loan_service:
            loan_state = loan_service.get_state(target_discord_id, guild_id)

            loan_lines = []
            loan_lines.append(f"**Loans Taken:** {loan_state.total_loans_taken}")
            loan_lines.append(f"**Fees Paid:** {loan_state.total_fees_paid} {JOPACOIN_EMOTE}")

            if loan_state.negative_loans_taken > 0:
                loan_lines.append(f"🔥 **Borrowed While Broke:** {loan_state.negative_loans_taken}x")

            if loan_state.has_outstanding_loan:
                loan_lines.append(f"\n⚠️ **Outstanding Loan:**")
                loan_lines.append(f"  Principal: {loan_state.outstanding_principal} {JOPACOIN_EMOTE}")
                loan_lines.append(f"  Fee: {loan_state.outstanding_fee} {JOPACOIN_EMOTE}")
                loan_lines.append(f"  **Total Owed:** {loan_state.outstanding_total} {JOPACOIN_EMOTE}")
                loan_lines.append(f"  *(Repaid on next match)*")

            if loan_state.is_on_cooldown and loan_state.cooldown_ends_at:
                loan_lines.append(f"\n⏳ **Cooldown:** <t:{loan_state.cooldown_ends_at}:R>")
            elif not loan_state.has_outstanding_loan:
                loan_lines.append(f"\n✅ **Loan Available**")

            embed.add_field(
                name="🏦 Loans",
                value="\n".join(loan_lines),
                inline=True,
            )

        # Bankruptcy information
        if bankruptcy_service:
            bankruptcy_repo = bankruptcy_service.bankruptcy_repo
            state_data = bankruptcy_repo.get_state(target_discord_id, guild_id)
            bankruptcy_state = bankruptcy_service.get_state(target_discord_id, guild_id)

            bankruptcy_lines = []
            bankruptcy_count = state_data["bankruptcy_count"] if state_data else 0
            bankruptcy_lines.append(f"**Declarations:** {bankruptcy_count}")

            if bankruptcy_state.penalty_games_remaining > 0:
                penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
                bankruptcy_lines.append(f"\n{TOMBSTONE_EMOJI} **Active Penalty:**")
                bankruptcy_lines.append(f"  {penalty_rate_pct}% win bonus")
                bankruptcy_lines.append(f"  {bankruptcy_state.penalty_games_remaining} game(s) remaining")

            if bankruptcy_state.is_on_cooldown and bankruptcy_state.cooldown_ends_at:
                bankruptcy_lines.append(f"\n⏳ **Cooldown:** <t:{bankruptcy_state.cooldown_ends_at}:R>")
            elif balance < 0:
                bankruptcy_lines.append(f"\n⚠️ **Bankruptcy Available**")

            embed.add_field(
                name=f"{TOMBSTONE_EMOJI} Bankruptcy",
                value="\n".join(bankruptcy_lines),
                inline=True,
            )

        # Lowest balance ever reached
        lowest_balance = player_repo.get_lowest_balance(target_discord_id, guild_id)
        if lowest_balance is not None and lowest_balance < 0:
            embed.add_field(
                name="📉 Lowest Balance",
                value=f"**{lowest_balance}** {JOPACOIN_EMOTE}",
                inline=True,
            )

        # Tipping statistics
        tip_repo = self._get_tip_repository()
        if tip_repo:
            tip_stats = tip_repo.get_user_tip_stats(target_discord_id, guild_id)

            # Only show if user has tip history
            if tip_stats["tips_sent_count"] > 0 or tip_stats["tips_received_count"] > 0:
                tip_lines = []
                if tip_stats["tips_sent_count"] > 0:
                    tip_lines.append(
                        f"**Sent:** {tip_stats['total_sent']} {JOPACOIN_EMOTE} ({tip_stats['tips_sent_count']} tips)"
                    )
                if tip_stats["tips_received_count"] > 0:
                    tip_lines.append(
                        f"**Received:** {tip_stats['total_received']} {JOPACOIN_EMOTE} ({tip_stats['tips_received_count']} tips)"
                    )
                if tip_stats["fees_paid"] > 0:
                    tip_lines.append(f"**Fees Paid:** {tip_stats['fees_paid']} {JOPACOIN_EMOTE}")

                embed.add_field(
                    name="💝 Tipping",
                    value="\n".join(tip_lines),
                    inline=True,
                )

        embed.set_footer(text="Tip: Use /balance for quick check, /loan to borrow")

        return embed, None

    async def _build_gambling_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Gambling tab embed with degen score, stats, and P&L chart."""
        gambling_stats_service = self._get_gambling_stats_service()
        player_service = self._get_player_service()
        loan_service = self._get_loan_service()
        bankruptcy_service = self._get_bankruptcy_service()

        if not gambling_stats_service:
            return discord.Embed(
                title="Error", description="Gambling stats service unavailable", color=COLOR_RED
            ), None

        stats = gambling_stats_service.get_player_stats(target_discord_id, guild_id)

        if not stats:
            return discord.Embed(
                title=f"Profile: {target_user.display_name} > Gambling",
                description="No betting history yet.\n\nPlay matches and use `/bet` to get started!",
                color=COLOR_BLUE,
            ), None

        # Color based on P&L
        color = COLOR_GREEN if stats.net_pnl >= 0 else COLOR_RED

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Gambling",
            color=color,
        )

        # Degen score header
        degen = stats.degen_score
        flavor_text = " • ".join(degen.flavor_texts) if degen.flavor_texts else degen.tagline
        embed.description = (
            f"**Degen Score: {degen.total}** {degen.emoji} {degen.title}\n"
            f"*{flavor_text}*"
        )

        # Performance
        pnl_str = f"+{stats.net_pnl}" if stats.net_pnl >= 0 else str(stats.net_pnl)
        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"

        # Get current balance
        player = player_service.get_player(target_discord_id, guild_id) if player_service else None
        balance = player.jopacoin_balance if player else 0

        embed.add_field(
            name="Performance",
            value=(
                f"**Balance:** {balance} {JOPACOIN_EMOTE}\n"
                f"**Net P&L:** {pnl_str} {JOPACOIN_EMOTE}\n"
                f"**ROI:** {roi_str}\n"
                f"**Record:** {stats.wins}W-{stats.losses}L ({stats.win_rate:.0%})"
            ),
            inline=True,
        )

        # Volume
        embed.add_field(
            name="Volume",
            value=(
                f"**Total Bets:** {stats.total_bets}\n"
                f"**Wagered:** {stats.total_wagered} {JOPACOIN_EMOTE}\n"
                f"**Avg Bet:** {stats.avg_bet_size:.1f} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Leverage distribution
        lev_parts = []
        for lev in [1, 2, 3, 5]:
            count = stats.leverage_distribution.get(lev, 0)
            if count > 0:
                pct = count / stats.total_bets * 100
                lev_parts.append(f"{lev}×({pct:.0f}%)")
        lev_str = " ".join(lev_parts) if lev_parts else "None"

        # Streaks
        streak_emoji = "🔥" if stats.current_streak > 0 else "💀" if stats.current_streak < 0 else "➖"
        streak_val = abs(stats.current_streak)
        streak_type = "W" if stats.current_streak >= 0 else "L"

        embed.add_field(
            name="Risk Profile",
            value=(
                f"**Leverage:** {lev_str}\n"
                f"**Streak:** {streak_emoji} {streak_type}{streak_val}\n"
                f"**Best/Worst:** W{stats.best_streak} / L{abs(stats.worst_streak)}"
            ),
            inline=True,
        )

        # Extremes
        peak_str = f"+{stats.peak_pnl}" if stats.peak_pnl > 0 else str(stats.peak_pnl)
        trough_str = str(stats.trough_pnl)
        biggest_win_str = f"+{stats.biggest_win}" if stats.biggest_win > 0 else "None"
        biggest_loss_str = str(stats.biggest_loss) if stats.biggest_loss < 0 else "None"

        embed.add_field(
            name="Extremes",
            value=(
                f"**Peak:** {peak_str} {JOPACOIN_EMOTE}\n"
                f"**Trough:** {trough_str} {JOPACOIN_EMOTE}\n"
                f"**Best Win:** {biggest_win_str} {JOPACOIN_EMOTE}\n"
                f"**Worst Loss:** {biggest_loss_str} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Paper hands
        if stats.matches_played > 0:
            paper_rate = stats.paper_hands_count / stats.matches_played * 100
            paper_emoji = "📄" if paper_rate >= 30 else "🤔" if paper_rate >= 10 else "💎"
            embed.add_field(
                name=f"{paper_emoji} Paper Hands",
                value=(
                    f"**Played:** {stats.matches_played} matches\n"
                    f"**No Self-Bet:** {stats.paper_hands_count} ({paper_rate:.0f}%)"
                ),
                inline=True,
            )

        # Degen score breakdown
        breakdown_lines = []
        if degen.max_leverage_score > 0:
            breakdown_lines.append(f"5x Addiction: {degen.max_leverage_score}/25")
        if degen.bet_size_score > 0:
            breakdown_lines.append(f"Bet Size: {degen.bet_size_score}/25")
        if degen.debt_depth_score > 0:
            breakdown_lines.append(f"Debt Depth: {degen.debt_depth_score}/20")
        if degen.bankruptcy_score > 0:
            breakdown_lines.append(f"Bankruptcies: {degen.bankruptcy_score}/15")
        if degen.frequency_score > 0:
            breakdown_lines.append(f"Frequency: {degen.frequency_score}/10")
        if degen.loss_chase_score > 0:
            breakdown_lines.append(f"Loss Chasing: {degen.loss_chase_score}/5")
        if degen.negative_loan_bonus > 0:
            breakdown_lines.append(f"🔥 Negative Loans: +{degen.negative_loan_bonus}")

        if breakdown_lines:
            embed.add_field(
                name="Degen Breakdown",
                value="\n".join(breakdown_lines),
                inline=True,
            )

        # Betting Impact section - how others bet on this player
        impact_stats = gambling_stats_service.get_betting_impact_stats(target_discord_id, guild_id)
        if impact_stats:
            embed.add_field(
                name="\u200b",  # Separator
                value="━━━━━━━━━━━━━━━━━━━━━━",
                inline=False,
            )

            # Compact supporter/hater lines with emojis
            supporter_pnl = impact_stats.supporters_net_pnl
            supporter_pnl_str = f"+{supporter_pnl}" if supporter_pnl >= 0 else str(supporter_pnl)
            supporter_emoji = "✅" if supporter_pnl >= 0 else "💀"

            hater_pnl = impact_stats.haters_net_pnl
            hater_pnl_str = f"+{hater_pnl}" if hater_pnl >= 0 else str(hater_pnl)
            hater_emoji = "✅" if hater_pnl >= 0 else "💀"

            embed.add_field(
                name="Betting Impact",
                value=(
                    f"**Supporters:** {impact_stats.total_wagered_for} {JOPACOIN_EMOTE} wagered "
                    f"→ {supporter_pnl_str} ({impact_stats.supporter_roi:+.0%}) {supporter_emoji}\n"
                    f"**Haters:** {impact_stats.total_wagered_against} {JOPACOIN_EMOTE} wagered "
                    f"→ {hater_pnl_str} ({impact_stats.hater_roi:+.0%}) {hater_emoji}\n"
                    f"**Win Rates:** Supporters {impact_stats.supporter_win_rate:.0%} | "
                    f"Haters {impact_stats.hater_win_rate:.0%}"
                ),
                inline=False,
            )

            # Notable bettors - show the most interesting ones
            notable_lines = []

            # Biggest fan/hater by $ wagered
            if impact_stats.biggest_fan:
                fan = impact_stats.biggest_fan
                notable_lines.append(
                    f"💰 **Biggest Fan:** <@{fan.discord_id}> ({fan.total_wagered_for} {JOPACOIN_EMOTE})"
                )
            if impact_stats.biggest_hater:
                hater = impact_stats.biggest_hater
                notable_lines.append(
                    f"💸 **Biggest Hater:** <@{hater.discord_id}> ({hater.total_wagered_against} {JOPACOIN_EMOTE})"
                )

            # Good/bad luck charms
            if impact_stats.blessing:
                b = impact_stats.blessing
                notable_lines.append(
                    f"🍀 **Blessing:** <@{b.discord_id}> (+{b.net_pnl_for} betting on you)"
                )
            if impact_stats.jinx:
                j = impact_stats.jinx
                notable_lines.append(
                    f"🪦 **Jinx:** <@{j.discord_id}> ({j.net_pnl_for} betting on you)"
                )

            # Luckiest hater
            if impact_stats.luckiest_hater:
                h = impact_stats.luckiest_hater
                notable_lines.append(
                    f"😈 **Luckiest Hater:** <@{h.discord_id}> (+{h.net_pnl_against} against you)"
                )

            if notable_lines:
                embed.add_field(
                    name="Notable Bettors",
                    value="\n".join(notable_lines),
                    inline=False,
                )

            # Extremes and star power
            extremes = []
            if impact_stats.biggest_single_win > 0:
                extremes.append(f"**Best Single Bet:** +{impact_stats.biggest_single_win} {JOPACOIN_EMOTE}")
            if impact_stats.biggest_single_loss < 0:
                extremes.append(f"**Worst Single Bet:** {impact_stats.biggest_single_loss} {JOPACOIN_EMOTE}")
            extremes.append(f"**Total Bets:** {impact_stats.total_bets} across {impact_stats.matches_with_bets} matches")

            embed.add_field(
                name="Bet Extremes",
                value="\n".join(extremes),
                inline=False,
            )

        # Generate P&L chart
        chart_file = None
        try:
            pnl_series = gambling_stats_service.get_cumulative_pnl_series(target_discord_id, guild_id)
            if pnl_series and len(pnl_series) >= 2:
                degen = stats.degen_score
                chart_bytes = await asyncio.to_thread(
                    functools.partial(draw_gamba_chart,
                        username=target_user.display_name,
                        degen_score=degen.total,
                        degen_title=degen.title,
                        degen_emoji=degen.emoji,
                        pnl_series=pnl_series,
                        stats={
                            "total_bets": stats.total_bets,
                            "win_rate": stats.win_rate,
                            "net_pnl": stats.net_pnl,
                            "roi": stats.roi,
                        },
                    )
                )
                chart_file = discord.File(chart_bytes, filename="gamba_chart.png")
                embed.set_image(url="attachment://gamba_chart.png")
        except Exception as e:
            logger.debug(f"Could not generate gamba chart: {e}")

        return embed, chart_file

    async def _build_predictions_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Predictions tab embed with prediction market stats."""
        prediction_service = self._get_prediction_service()

        if not prediction_service:
            return discord.Embed(
                title="Error", description="Prediction service unavailable", color=COLOR_RED
            ), None

        # Get prediction stats
        stats = prediction_service.prediction_repo.get_user_prediction_stats(target_discord_id, guild_id)

        if not stats:
            return discord.Embed(
                title=f"Profile: {target_user.display_name} > Predictions",
                description="No prediction bets yet.\n\nUse `/predictions` to find active markets!",
                color=COLOR_BLUE,
            ), None

        # Color based on P&L
        pnl = stats["net_pnl"] or 0
        color = COLOR_GREEN if pnl >= 0 else COLOR_RED

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Predictions",
            color=color,
        )

        # Performance
        pnl_str = f"+{pnl}" if pnl >= 0 else str(pnl)
        wins = stats["wins"] or 0
        losses = stats["losses"] or 0
        win_rate = stats["win_rate"] or 0

        embed.add_field(
            name="Performance",
            value=(
                f"**Net P&L:** {pnl_str} {JOPACOIN_EMOTE}\n"
                f"**Record:** {wins}W-{losses}L ({win_rate:.0%})\n"
                f"**Best Win:** +{stats['best_win'] or 0} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Volume
        total_wagered = stats["total_wagered"] or 0
        total_bets = stats["total_bets"] or 0
        avg_bet = total_wagered / total_bets if total_bets > 0 else 0

        embed.add_field(
            name="Volume",
            value=(
                f"**Total Bets:** {total_bets}\n"
                f"**Wagered:** {total_wagered} {JOPACOIN_EMOTE}\n"
                f"**Avg Bet:** {avg_bet:.1f} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Active positions
        positions = prediction_service.get_user_active_positions(target_discord_id, guild_id)
        if positions:
            position_lines = []
            for pos in positions[:3]:
                emoji = "✅" if pos["position"] == "yes" else "❌"
                odds_info = prediction_service.get_odds(pos["prediction_id"])
                current_odds = odds_info["odds"].get(pos["position"], 0)
                pool = odds_info["total_pool"]
                yes_total = odds_info["yes_total"]
                pct = round(100 * yes_total / pool) if pool > 0 else 50
                my_pct = pct if pos["position"] == "yes" else 100 - pct

                potential = int(pos["total_amount"] * current_odds) if current_odds > 0 else 0
                question_short = pos["question"][:35] + "..." if len(pos["question"]) > 35 else pos["question"]

                position_lines.append(
                    f"{emoji} **#{pos['prediction_id']}:** {question_short}\n"
                    f"   {pos['position'].upper()} @ {my_pct}% ({current_odds:.2f}x) | "
                    f"{pos['total_amount']} → {potential} {JOPACOIN_EMOTE}"
                )

            if len(positions) > 3:
                position_lines.append(f"*+{len(positions) - 3} more positions*")

            embed.add_field(
                name=f"🟢 Active Positions ({len(positions)})",
                value="\n".join(position_lines),
                inline=False,
            )
        else:
            embed.add_field(
                name="Active Positions",
                value="No active positions",
                inline=False,
            )

        # Recent resolved
        resolved = prediction_service.get_user_resolved_positions(target_discord_id, guild_id)
        if resolved:
            recent_lines = []
            for pos in resolved[:3]:
                bet_emoji = "✅" if pos["position"] == "yes" else "❌"
                won = pos["position"] == pos["outcome"]
                result_emoji = "🏆" if won else "💀"
                amount = pos["total_amount"]
                payout = pos["payout"] or 0
                profit = payout - amount if won else -amount
                profit_str = f"+{profit}" if profit > 0 else str(profit)

                question_short = pos["question"][:30] + "..." if len(pos["question"]) > 30 else pos["question"]
                recent_lines.append(
                    f"{result_emoji} {bet_emoji} {question_short}: {profit_str} {JOPACOIN_EMOTE}"
                )

            embed.add_field(
                name="Recent Results",
                value="\n".join(recent_lines),
                inline=False,
            )

        embed.set_footer(text="Tip: Use /mypredictions for all positions")

        return embed, None

    async def _build_dota_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, list[discord.File]]:
        """Build the Dota tab embed with OpenDota stats (roles/lanes)."""
        player_repo = self._get_player_repo()
        opendota_service = self._get_opendota_player_service()

        if not player_repo:
            return discord.Embed(
                title="Error", description="Player repository unavailable", color=COLOR_RED
            ), []

        player = player_repo.get_by_id(target_discord_id, guild_id)
        if not player:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ), []

        # Get steam_id from repository (not a Player attribute)
        steam_id = player_repo.get_steam_id(target_discord_id)
        if not steam_id:
            return discord.Embed(
                title=f"Profile: {target_user.display_name} > Dota Stats",
                description="No Steam account linked.\n\nUse `/linksteam` to link your Steam ID, or `/register` if you're new.",
                color=COLOR_ORANGE,
            ), []

        if not opendota_service:
            return discord.Embed(
                title="Error",
                description="OpenDota service unavailable.",
                color=COLOR_RED,
            ), []

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Dota Stats",
            color=COLOR_BLUE,
        )

        files = []

        # Get role distribution for chart - wrap in try/except for API errors
        role_dist = None
        try:
            role_dist = opendota_service.get_hero_role_distribution(target_discord_id, match_limit=50)
        except Exception as e:
            logger.warning(f"Failed to fetch role distribution from OpenDota: {e}")

        if role_dist:
            try:
                chart_bytes = await asyncio.to_thread(
                    functools.partial(draw_role_graph, role_dist, title=f"Roles: {target_user.display_name}")
                )
                role_file = discord.File(chart_bytes, filename="role_graph.png")
                files.append(role_file)
                embed.set_image(url="attachment://role_graph.png")
            except Exception as e:
                logger.debug(f"Could not generate role graph: {e}")

        # Get full stats for additional info - wrap in try/except for API errors
        full_stats = None
        try:
            full_stats = opendota_service.get_full_stats(target_discord_id, match_limit=50)
        except Exception as e:
            logger.warning(f"Failed to fetch full stats from OpenDota: {e}")

        if full_stats:
            # Generate lane distribution chart
            lane_dist = full_stats.get("lane_distribution", {})
            # Filter out lanes with 0% to reduce clutter
            lane_dist_filtered = {k: v for k, v in lane_dist.items() if v > 0}

            if lane_dist_filtered:
                try:
                    lane_bytes = await asyncio.to_thread(draw_lane_distribution, lane_dist_filtered)
                    lane_file = discord.File(lane_bytes, filename="lane_graph.png")
                    files.append(lane_file)
                    # Add lane chart info to embed (image will appear as second attachment)
                    lane_parsed = full_stats.get("lane_parsed_count", 0)
                    embed.add_field(
                        name="Lane Distribution",
                        value=f"Based on {lane_parsed} parsed matches\n*(see lane_graph.png below)*",
                        inline=True,
                    )
                except Exception as e:
                    logger.debug(f"Could not generate lane graph: {e}")
                    # Fall back to text display
                    lane_lines = []
                    for lane, pct in sorted(lane_dist.items(), key=lambda x: -x[1]):
                        if pct > 0:
                            bar_len = int(pct / 10)
                            bar = "█" * bar_len + "░" * (10 - bar_len)
                            lane_lines.append(f"`{bar}` {lane}: {pct:.0f}%")
                    if lane_lines:
                        embed.add_field(
                            name="Lane Distribution",
                            value="\n".join(lane_lines[:5]),
                            inline=True,
                        )

            # Win rate
            if full_stats.get("win_rate") is not None:
                embed.add_field(
                    name="OpenDota Win Rate",
                    value=f"{full_stats['win_rate']:.1%} (last 50)",
                    inline=True,
                )

            # Average stats
            avg_kills = full_stats.get("avg_kills", 0)
            avg_deaths = full_stats.get("avg_deaths", 0)
            avg_assists = full_stats.get("avg_assists", 0)
            if avg_kills or avg_deaths or avg_assists:
                embed.add_field(
                    name="Avg KDA",
                    value=f"{avg_kills:.1f} / {avg_deaths:.1f} / {avg_assists:.1f}",
                    inline=True,
                )

            # Top heroes
            hero_counts = full_stats.get("hero_counts", [])
            if hero_counts:
                from utils.hero_lookup import get_hero_name
                hero_lines = []
                for hero_id, games, wins in hero_counts[:5]:
                    hero_name = get_hero_name(hero_id)
                    wr = (wins / games * 100) if games > 0 else 0
                    hero_lines.append(f"**{hero_name}** - {games}g ({wr:.0f}%)")
                embed.add_field(
                    name="Top Heroes",
                    value="\n".join(hero_lines),
                    inline=False,
                )
        else:
            embed.description = "Could not fetch OpenDota stats. Try again later."

        embed.set_footer(text="Data from OpenDota | Based on last 50 matches")

        return embed, files

    async def _build_teammates_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, discord.File | None]:
        """Build the Teammates tab embed with pairwise statistics."""
        pairings_repo = self._get_pairings_repo()
        player_repo = self._get_player_repo()

        if not pairings_repo or not player_repo:
            return discord.Embed(
                title="Error", description="Pairings data unavailable", color=COLOR_RED
            ), None

        player = player_repo.get_by_id(target_discord_id, guild_id)
        if not player:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ), None

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Teammates",
            color=0x9B59B6,  # Purple
        )

        min_games = 3
        limit = 5

        def get_player_mention(discord_id: int) -> str:
            """Get a mention string for a player."""
            if discord_id and discord_id > 0:
                return f"<@{discord_id}>"
            p = player_repo.get_by_id(discord_id, guild_id)
            return p.name if p else f"Unknown ({discord_id})"

        # Best Teammates (highest win rate with)
        best_teammates = pairings_repo.get_best_teammates(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if best_teammates:
            lines = []
            for tm in best_teammates:
                name = get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="🏆 Best Teammates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="🏆 Best Teammates", value="No data yet", inline=True)

        # Worst Teammates (lowest win rate with)
        worst_teammates = pairings_repo.get_worst_teammates(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if worst_teammates:
            lines = []
            for tm in worst_teammates:
                name = get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="💀 Worst Teammates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="💀 Worst Teammates", value="No data yet", inline=True)

        # Spacer
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Dominates (best matchups against)
        best_matchups = pairings_repo.get_best_matchups(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if best_matchups:
            lines = []
            for m in best_matchups:
                name = get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="😈 Dominates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="😈 Dominates", value="No data yet", inline=True)

        # Struggles Against (worst matchups)
        worst_matchups = pairings_repo.get_worst_matchups(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if worst_matchups:
            lines = []
            for m in worst_matchups:
                name = get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="😰 Struggles Against",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="😰 Struggles Against", value="No data yet", inline=True)

        # Spacer
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Most Played With
        most_played_with = pairings_repo.get_most_played_with(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if most_played_with:
            lines = []
            for tm in most_played_with:
                name = get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{name} - {games}g ({rate:.0f}%)")
            embed.add_field(
                name="👥 Most Played With",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="👥 Most Played With", value="No data yet", inline=True)

        # Most Played Against
        most_played_against = pairings_repo.get_most_played_against(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if most_played_against:
            lines = []
            for m in most_played_against:
                name = get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{name} - {games}g ({rate:.0f}%)")
            embed.add_field(
                name="⚔️ Most Played Against",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="⚔️ Most Played Against", value="No data yet", inline=True)

        # Spacer to complete row and force Even sections to new row
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Evenly Matched (teammates with ~50% win rate)
        even_teammates = pairings_repo.get_evenly_matched_teammates(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if even_teammates:
            lines = []
            for tm in even_teammates:
                name = get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                lines.append(f"{name} ({wins}W/{games - wins}L)")
            embed.add_field(
                name="⚖️ Even Teammates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="⚖️ Even Teammates", value="No data yet", inline=True)

        # Evenly Matched Opponents
        even_opponents = pairings_repo.get_evenly_matched_opponents(
            target_discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )
        if even_opponents:
            lines = []
            for m in even_opponents:
                name = get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                lines.append(f"{name} ({wins}W/{games - wins}L)")
            embed.add_field(
                name="⚖️ Even Opponents",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="⚖️ Even Opponents", value="No data yet", inline=True)

        # Spacer to complete the Even row
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Get totals for footer
        counts = pairings_repo.get_pairing_counts(target_discord_id, guild_id=guild_id, min_games=min_games)
        footer_parts = [f"Min {min_games} games"]
        if counts["unique_teammates"] > 0 or counts["unique_opponents"] > 0:
            footer_parts.append(
                f"{counts['unique_teammates']} teammates, {counts['unique_opponents']} opponents tracked"
            )
        embed.set_footer(text=" | ".join(footer_parts))

        return embed, None

    def _get_opendota_player_service(self):
        """Get OpenDotaPlayerService from bot."""
        return getattr(self.bot, "opendota_player_service", None)

    async def _build_heroes_embed(
        self,
        target_user: discord.Member | discord.User,
        target_discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[discord.Embed, list[discord.File]]:
        """Build the Heroes tab embed with comprehensive hero statistics."""
        from utils.hero_lookup import get_hero_name

        match_repo = self._get_match_repo()
        player_repo = self._get_player_repo()

        if not match_repo or not player_repo:
            return discord.Embed(
                title="Error", description="Repository unavailable", color=COLOR_RED
            ), []

        player = player_repo.get_by_id(target_discord_id, guild_id)
        if not player:
            return discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ), []

        # Get enriched match count
        enriched_count = match_repo.get_player_enriched_match_count(target_discord_id, guild_id)

        if enriched_count == 0:
            return discord.Embed(
                title=f"Profile: {target_user.display_name} > Heroes",
                description=(
                    "No enriched match data available.\n\n"
                    "Hero stats require matches to be enriched with Dota 2 data.\n"
                    "Ask an admin to run `/enrichmatch` or `/autodiscover` to add match data."
                ),
                color=COLOR_ORANGE,
            ), []

        embed = discord.Embed(
            title=f"Profile: {target_user.display_name} > Heroes",
            color=COLOR_BLUE,
        )

        files = []

        # Get hero stats for chart
        hero_stats = match_repo.get_player_hero_detailed_stats(target_discord_id, guild_id, limit=20)

        # Generate hero performance chart
        if hero_stats:
            try:
                chart_bytes = await asyncio.to_thread(draw_hero_performance_chart, hero_stats, target_user.display_name)
                chart_file = discord.File(chart_bytes, filename="hero_chart.png")
                files.append(chart_file)
                embed.set_image(url="attachment://hero_chart.png")
            except Exception as e:
                logger.debug(f"Could not generate hero chart: {e}")

        # Get overall stats for header
        overall = match_repo.get_player_overall_hero_stats(target_discord_id, guild_id)
        avg_kda = f"{overall['avg_kills']:.1f}/{overall['avg_deaths']:.1f}/{overall['avg_assists']:.1f}"
        embed.add_field(
            name="Overview",
            value=(
                f"**Enriched Games:** {overall['total_games']}\n"
                f"**Avg KDA:** {avg_kda}\n"
                f"**Avg GPM:** {overall['avg_gpm']:.0f}"
            ),
            inline=True,
        )

        # Top heroes text (supplement to chart)
        if hero_stats:
            top_lines = []
            for stat in hero_stats[:5]:
                hero_name = get_hero_name(stat["hero_id"])
                games = stat["games"]
                wins = stat["wins"]
                wr = wins / games if games > 0 else 0
                kda = f"{stat['avg_kills']:.1f}/{stat['avg_deaths']:.1f}/{stat['avg_assists']:.1f}"
                gpm = stat["avg_gpm"]
                top_lines.append(f"**{hero_name}** - {games}g ({wr:.0%}) | {kda} | {gpm:.0f}g")
            embed.add_field(
                name="Top Heroes",
                value="\n".join(top_lines),
                inline=False,
            )

        # Average heroes (exactly 50% winrate)
        if hero_stats:
            avg_heroes = [
                s for s in hero_stats
                if s["games"] >= 2 and s["wins"] * 2 == s["games"]
            ]
            if avg_heroes:
                avg_lines = []
                for stat in avg_heroes[:5]:
                    hero_name = get_hero_name(stat["hero_id"])
                    games = stat["games"]
                    wins = stat["wins"]
                    kda = f"{stat['avg_kills']:.1f}/{stat['avg_deaths']:.1f}/{stat['avg_assists']:.1f}"
                    gpm = stat["avg_gpm"]
                    avg_lines.append(f"**{hero_name}** - {games}g ({wins}-{wins}) | {kda} | {gpm:.0f}g")
                embed.add_field(
                    name="😐 Average Heroes",
                    value="\n".join(avg_lines),
                    inline=False,
                )

        # Lane performance
        lane_stats = match_repo.get_player_lane_stats(target_discord_id, guild_id)
        if lane_stats:
            lane_names = {1: "Safe", 2: "Mid", 3: "Off", 4: "Jungle"}
            lane_lines = []
            for ls in lane_stats[:4]:
                lane_name = lane_names.get(ls["lane_role"], f"Lane {ls['lane_role']}")
                games = ls["games"]
                wins = ls["wins"]
                wr = wins / games if games > 0 else 0
                kda = f"{ls['avg_kills']:.1f}/{ls['avg_deaths']:.1f}/{ls['avg_assists']:.1f}"
                gpm = ls["avg_gpm"]
                eff = f"{ls['avg_lane_eff']:.0f}% eff" if ls["avg_lane_eff"] else "-"
                lane_lines.append(f"**{lane_name}** ({games}g): {wr:.0%} WR | {kda} | {gpm:.0f}g | {eff}")
            embed.add_field(
                name="Performance by Lane",
                value="\n".join(lane_lines),
                inline=False,
            )

        # Ward stats
        ward_stats = match_repo.get_player_ward_stats_by_lane(target_discord_id, guild_id)
        if ward_stats and overall["total_obs"] + overall["total_sens"] > 0:
            ward_lines = [f"**Totals:** {overall['total_obs']} obs | {overall['total_sens']} sens"]
            # Aggregate by role type (support lanes vs core lanes)
            support_obs = support_sens = support_games = 0
            core_obs = core_sens = core_games = 0
            for ws in ward_stats:
                if ws["lane_role"] in [4, 5]:  # Support lanes
                    support_obs += ws["total_obs"]
                    support_sens += ws["total_sens"]
                    support_games += ws["games"]
                else:
                    core_obs += ws["total_obs"]
                    core_sens += ws["total_sens"]
                    core_games += ws["games"]
            if support_games > 0:
                ward_lines.append(
                    f"**Support avg:** {support_obs/support_games:.1f} obs/g | {support_sens/support_games:.1f} sens/g"
                )
            if core_games > 0:
                ward_lines.append(
                    f"**Core avg:** {core_obs/core_games:.1f} obs/g | {core_sens/core_games:.1f} sens/g"
                )
            embed.add_field(
                name="Ward Stats",
                value="\n".join(ward_lines),
                inline=True,
            )

        # Hero pairwise stats section
        embed.add_field(name="\u200b", value="━━━ **Hero Pairwise Stats** ━━━", inline=False)

        # Nemesis heroes (lose to)
        nemesis = match_repo.get_player_nemesis_heroes(target_discord_id, guild_id, min_games=2)
        if nemesis:
            # Filter to actually high loss rate (>50%)
            nemesis_bad = [n for n in nemesis if n["loss_rate"] > 0.5][:3]
            if nemesis_bad:
                nem_lines = []
                for n in nemesis_bad:
                    hero_name = get_hero_name(n["enemy_hero"])
                    nem_lines.append(f"{hero_name} ({n['games']}g, {n['wins']}-{n['losses']})")
                embed.add_field(
                    name="💀 Nemesis Heroes",
                    value="\n".join(nem_lines),
                    inline=True,
                )

        # Easy prey (beat)
        easy = match_repo.get_player_easiest_opponents(target_discord_id, guild_id, min_games=2)
        if easy:
            # Filter to actually high win rate (>50%)
            easy_good = [e for e in easy if e["win_rate"] > 0.5][:3]
            if easy_good:
                easy_lines = []
                for e in easy_good:
                    hero_name = get_hero_name(e["enemy_hero"])
                    easy_lines.append(f"{hero_name} ({e['games']}g, {e['wins']}-{e['losses']})")
                embed.add_field(
                    name="😈 Easy Prey",
                    value="\n".join(easy_lines),
                    inline=True,
                )

        # Best ally heroes
        synergies = match_repo.get_player_best_hero_synergies(target_discord_id, guild_id, min_games=2)
        if synergies:
            # Filter to high win rate
            synergies_good = [s for s in synergies if s["win_rate"] > 0.5][:3]
            if synergies_good:
                syn_lines = []
                for s in synergies_good:
                    hero_name = get_hero_name(s["ally_hero"])
                    losses = s["games"] - s["wins"]
                    syn_lines.append(f"{hero_name} ({s['games']}g, {s['wins']}-{losses})")
                embed.add_field(
                    name="🤝 Best Ally Heroes",
                    value="\n".join(syn_lines),
                    inline=True,
                )

        # Hero vs opponent hero matchups
        hero_vs_hero = match_repo.get_player_hero_vs_opponent_heroes(target_discord_id, guild_id, min_games=2)
        if hero_vs_hero:
            # Group by player's hero and find best/worst matchups
            by_my_hero: dict[int, list] = {}
            for m in hero_vs_hero:
                my_hero = m["my_hero"]
                if my_hero not in by_my_hero:
                    by_my_hero[my_hero] = []
                by_my_hero[my_hero].append(m)

            matchup_lines = []
            for my_hero_id, matchups in list(by_my_hero.items())[:3]:
                my_hero_name = get_hero_name(my_hero_id)
                # Sort by win rate
                sorted_matchups = sorted(matchups, key=lambda x: x["wins"] / x["games"] if x["games"] > 0 else 0)
                worst = sorted_matchups[0] if sorted_matchups else None
                best = sorted_matchups[-1] if sorted_matchups else None

                parts = []
                if best and best["wins"] / best["games"] > 0.5:
                    best_name = get_hero_name(best["opponent_hero"])
                    parts.append(f"good vs {best_name} ({best['wins']}-{best['games']-best['wins']})")
                if worst and worst["wins"] / worst["games"] < 0.5:
                    worst_name = get_hero_name(worst["opponent_hero"])
                    parts.append(f"bad vs {worst_name} ({worst['wins']}-{worst['games']-worst['wins']})")

                if parts:
                    matchup_lines.append(f"**{my_hero_name}:** {', '.join(parts)}")

            if matchup_lines:
                embed.add_field(
                    name="Your Hero vs Enemy Heroes",
                    value="\n".join(matchup_lines),
                    inline=False,
                )

        # Best heroes by lane
        hero_lane = match_repo.get_player_hero_lane_performance(target_discord_id, guild_id)
        if hero_lane:
            # Group by lane and find best hero for each
            by_lane: dict[int, list] = {}
            for hl in hero_lane:
                lane = hl["lane_role"]
                if lane not in by_lane:
                    by_lane[lane] = []
                by_lane[lane].append(hl)

            lane_names = {1: "Safe", 2: "Mid", 3: "Off", 4: "Jungle"}
            best_by_lane = []
            for lane_role in [1, 2, 3]:  # Safe, Mid, Off
                if lane_role in by_lane:
                    # Find best hero by win rate with minimum 2 games
                    candidates = [h for h in by_lane[lane_role] if h["games"] >= 2]
                    if candidates:
                        best = max(candidates, key=lambda x: x["wins"] / x["games"])
                        hero_name = get_hero_name(best["hero_id"])
                        wr = best["wins"] / best["games"] if best["games"] > 0 else 0
                        lane_name = lane_names.get(lane_role, f"Lane {lane_role}")
                        best_by_lane.append(f"**{lane_name}:** {hero_name} ({best['games']}g, {wr:.0%})")

            if best_by_lane:
                embed.add_field(
                    name="Best Heroes by Lane",
                    value="\n".join(best_by_lane),
                    inline=True,
                )

        # Footer
        embed.set_footer(text=f"Based on {enriched_count} enriched matches | Use /enrichmatch to add more")

        return embed, files

    async def _delete_after_timeout(
        self, message: discord.Message, view: "ProfileView", timeout: int
    ):
        """Delete the profile message after timeout seconds."""
        await asyncio.sleep(timeout)
        view.stop()  # Stop the view to disable buttons
        try:
            await message.delete()
            logger.info(f"Profile message {message.id} deleted after {timeout // 60}min timeout")
        except discord.NotFound:
            logger.debug(f"Profile message {message.id} was already deleted")
        except discord.HTTPException as e:
            logger.warning(f"Failed to delete profile message {message.id}: {e}")

    @app_commands.command(name="profile", description="View unified player profile with tabbed stats")
    @app_commands.describe(user="Player to view profile for (defaults to yourself)")
    async def profile(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
    ):
        """Display unified player profile with tabbed navigation."""
        # Rate limiting
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="profile",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/profile` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        target_user = user or interaction.user
        target_discord_id = target_user.id
        guild_id = guild.id if guild else None

        # Check if player is registered
        player_repo = self._get_player_repo()
        if player_repo:
            player = player_repo.get_by_id(target_discord_id, guild_id)
            if not player:
                await safe_followup(
                    interaction,
                    content=f"❌ {target_user.display_name} is not registered. Use `/register` to get started.",
                )
                return

        # Build initial embed (overview)
        embed, files = await self.build_tab_embed("overview", target_user, target_discord_id, guild_id)

        # Create view with tab buttons
        view = ProfileView(self, target_user, target_discord_id, guild_id)

        # Send and store message reference for timeout cleanup
        # safe_followup accepts 'files' kwarg for multiple files
        message = await safe_followup(interaction, embed=embed, view=view, files=files if files else None)
        view.message = message
        logger.info(f"Profile sent. Message ref stored: {message is not None}, msg_id: {getattr(message, 'id', None)}")

        # Schedule message deletion after timeout (more reliable than view.on_timeout)
        if message:
            asyncio.create_task(self._delete_after_timeout(message, view, timeout=840))


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    await bot.add_cog(ProfileCommands(bot))
