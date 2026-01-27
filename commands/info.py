"""
Information commands for the bot: /help, /leaderboard
"""

import asyncio
import logging
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

import discord
from discord import app_commands
from discord.ext import commands

from config import LEVERAGE_TIERS
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from services.permissions import has_admin_permission
from utils.debug_logging import debug_log as _dbg_log
from utils.drawing import draw_rating_distribution
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI
from utils.hero_lookup import get_hero_short_name, classify_hero_role
from utils.interaction_safety import safe_defer, safe_followup
from utils.embed_safety import truncate_field
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.rating_insights import compute_calibration_stats, rd_to_certainty

logger = logging.getLogger("cama_bot.commands.info")

# Page sizes differ by tab type due to Discord embed field limits (1024 chars)
SINGLE_SECTION_PAGE_SIZE = 20  # Balance, Glicko, OpenSkill (single list)
MULTI_SECTION_PAGE_SIZE = 8  # Gambling (4 sections), Predictions (3 sections)
LEADERBOARD_PAGE_SIZE = 20  # Legacy alias
GAMBLING_PAGE_SIZE = 8  # Legacy alias


class LeaderboardTab(Enum):
    """Available leaderboard tabs."""
    BALANCE = "balance"
    GAMBLING = "gambling"
    PREDICTIONS = "predictions"
    GLICKO = "glicko"
    OPENSKILL = "openskill"
    TIPS = "tips"


@dataclass
class TabState:
    """Per-tab state for lazy loading and pagination."""
    data: Any = None
    current_page: int = 0
    max_page: int = 0
    loaded: bool = False
    # For gambling tab, we need to store extra metadata
    extra: dict = field(default_factory=dict)


class UnifiedLeaderboardView(discord.ui.View):
    """Unified tabbed leaderboard view with pagination per tab."""

    def __init__(
        self,
        cog: "InfoCommands",
        guild_id: int | None,
        interaction: discord.Interaction,
        initial_tab: LeaderboardTab = LeaderboardTab.BALANCE,
        limit: int = 100,
        timeout: float = 840.0,  # 14 minutes
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.interaction = interaction
        self.current_tab = initial_tab
        self.limit = limit
        self.message: discord.Message | None = None

        # Per-tab state for lazy loading and independent pagination
        self._tab_states: dict[LeaderboardTab, TabState] = {
            tab: TabState() for tab in LeaderboardTab
        }

        # Concurrency control
        self._update_lock = asyncio.Lock()
        self._last_interaction_time: dict[int, float] = {}

        self._update_button_styles()

    def _update_button_styles(self) -> None:
        """Update button styles based on current tab."""
        tab_buttons = {
            LeaderboardTab.BALANCE: self.balance_btn,
            LeaderboardTab.GAMBLING: self.gambling_btn,
            LeaderboardTab.PREDICTIONS: self.predictions_btn,
            LeaderboardTab.GLICKO: self.glicko_btn,
            LeaderboardTab.OPENSKILL: self.openskill_btn,
            LeaderboardTab.TIPS: self.tips_btn,
        }
        for tab, button in tab_buttons.items():
            if tab == self.current_tab:
                button.style = discord.ButtonStyle.primary
            else:
                button.style = discord.ButtonStyle.secondary

    def _update_pagination_buttons(self) -> None:
        """Enable/disable pagination buttons based on current tab's page state."""
        state = self._tab_states[self.current_tab]
        self.prev_button.disabled = state.current_page == 0
        self.next_button.disabled = state.current_page >= state.max_page

    async def _load_tab_data(self, tab: LeaderboardTab) -> None:
        """Load data for a tab if not already loaded."""
        state = self._tab_states[tab]
        if state.loaded:
            return

        if tab == LeaderboardTab.BALANCE:
            await self._fetch_balance_data(state)
        elif tab == LeaderboardTab.GAMBLING:
            await self._fetch_gambling_data(state)
        elif tab == LeaderboardTab.PREDICTIONS:
            await self._fetch_predictions_data(state)
        elif tab == LeaderboardTab.GLICKO:
            await self._fetch_glicko_data(state)
        elif tab == LeaderboardTab.OPENSKILL:
            await self._fetch_openskill_data(state)
        elif tab == LeaderboardTab.TIPS:
            await self._fetch_tips_data(state)

        state.loaded = True

    async def _fetch_balance_data(self, state: TabState) -> None:
        """Fetch balance leaderboard data."""
        rating_system = CamaRatingSystem()
        players = self.cog.player_repo.get_leaderboard(limit=self.limit)
        total_count = self.cog.player_repo.get_player_count()
        debtors = self.cog.player_repo.get_players_with_negative_balance()

        players_with_stats = []
        for player in players:
            if player.discord_id is None:
                continue
            wins = player.wins or 0
            losses = player.losses or 0
            total_games = wins + losses
            win_rate = (wins / total_games * 100) if total_games > 0 else 0.0
            rating_value = player.glicko_rating
            cama_rating = (
                rating_system.rating_to_display(rating_value)
                if rating_value is not None
                else None
            )
            players_with_stats.append({
                "discord_id": player.discord_id,
                "username": player.name,
                "wins": wins,
                "losses": losses,
                "win_rate": win_rate,
                "rating": cama_rating,
                "jopacoin_balance": player.jopacoin_balance or 0,
            })

        state.data = {
            "players": players_with_stats,
            "total_count": total_count,
            "debtors": debtors,
        }
        state.max_page = max(0, (len(players_with_stats) - 1) // SINGLE_SECTION_PAGE_SIZE)

    async def _fetch_gambling_data(self, state: TabState) -> None:
        """Fetch gambling leaderboard data."""
        if not self.cog.gambling_stats_service:
            state.data = None
            return

        leaderboard = self.cog.gambling_stats_service.get_leaderboard(
            self.guild_id, limit=self.limit
        )

        # Pre-fetch guild members
        guild = self.interaction.guild
        guild_members = {m.id: m for m in guild.members} if guild else {}

        # Collect all unique discord_ids
        all_discord_ids = set()
        for entry in leaderboard.top_earners:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.down_bad:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.hall_of_degen:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.biggest_gamblers:
            all_discord_ids.add(entry.discord_id)

        # Batch fetch bankruptcy states
        bankruptcy_states = {}
        if self.cog.bankruptcy_service and all_discord_ids:
            bankruptcy_states = self.cog.bankruptcy_service.get_bulk_states(list(all_discord_ids))

        state.data = leaderboard
        state.extra = {
            "guild_members": guild_members,
            "bankruptcy_states": bankruptcy_states,
        }

        # Calculate max pages based on longest section
        max_entries = max(
            len(leaderboard.top_earners),
            len(leaderboard.down_bad),
            len(leaderboard.hall_of_degen),
            len(leaderboard.biggest_gamblers),
            1,  # Prevent division by zero
        )
        state.max_page = max(0, (max_entries - 1) // MULTI_SECTION_PAGE_SIZE)

    async def _fetch_predictions_data(self, state: TabState) -> None:
        """Fetch predictions leaderboard data."""
        if not self.cog.prediction_service:
            state.data = None
            return

        leaderboard = self.cog.prediction_service.prediction_repo.get_prediction_leaderboard(
            self.guild_id, self.limit
        )
        server_stats = self.cog.prediction_service.prediction_repo.get_server_prediction_stats(
            self.guild_id
        )

        # Pre-fetch guild members
        guild = self.interaction.guild
        guild_members = {m.id: m for m in guild.members} if guild else {}

        state.data = {
            "leaderboard": leaderboard,
            "server_stats": server_stats,
        }
        state.extra = {"guild_members": guild_members}

        # Calculate max pages based on longest section
        max_entries = max(
            len(leaderboard.get("top_earners", [])),
            len(leaderboard.get("down_bad", [])),
            len(leaderboard.get("most_accurate", [])),
            1,
        )
        state.max_page = max(0, (max_entries - 1) // MULTI_SECTION_PAGE_SIZE)

    async def _fetch_glicko_data(self, state: TabState) -> None:
        """Fetch Glicko-2 rating leaderboard data."""
        rating_system = CamaRatingSystem()
        players = self.cog.player_repo.get_leaderboard_by_glicko(limit=self.limit)
        total_rated = self.cog.player_repo.get_rated_player_count(rating_type="glicko")

        players_with_stats = []
        for player in players:
            if player.glicko_rating is None:
                continue
            rating_display = rating_system.rating_to_display(player.glicko_rating)
            rd = player.glicko_rd or 350.0
            certainty = rd_to_certainty(rd)
            players_with_stats.append({
                "discord_id": player.discord_id,
                "username": player.name,
                "rating_display": rating_display,
                "certainty": certainty,
                "wins": player.wins or 0,
                "losses": player.losses or 0,
            })

        state.data = {
            "players": players_with_stats,
            "total_rated": total_rated,
        }
        state.max_page = max(0, (len(players_with_stats) - 1) // SINGLE_SECTION_PAGE_SIZE)

    async def _fetch_openskill_data(self, state: TabState) -> None:
        """Fetch OpenSkill rating leaderboard data."""
        os_system = CamaOpenSkillSystem()
        players = self.cog.player_repo.get_leaderboard_by_openskill(limit=self.limit)
        total_rated = self.cog.player_repo.get_rated_player_count(rating_type="openskill")

        players_with_stats = []
        for player in players:
            if player.os_mu is None:
                continue
            rating_display = os_system.mu_to_display(player.os_mu)
            sigma = player.os_sigma or os_system.DEFAULT_SIGMA
            certainty = os_system.get_certainty_percentage(sigma)
            players_with_stats.append({
                "discord_id": player.discord_id,
                "username": player.name,
                "rating_display": rating_display,
                "certainty": certainty,
                "wins": player.wins or 0,
                "losses": player.losses or 0,
            })

        state.data = {
            "players": players_with_stats,
            "total_rated": total_rated,
        }
        state.max_page = max(0, (len(players_with_stats) - 1) // SINGLE_SECTION_PAGE_SIZE)

    async def _fetch_tips_data(self, state: TabState) -> None:
        """Fetch tip leaderboard data."""
        tip_repo = getattr(self.cog.bot, "tip_repository", None)
        if not tip_repo:
            state.data = None
            return

        top_senders = tip_repo.get_top_senders(self.guild_id, limit=self.limit)
        top_receivers = tip_repo.get_top_receivers(self.guild_id, limit=self.limit)
        total_volume = tip_repo.get_total_tip_volume(self.guild_id)

        # Pre-fetch guild members
        guild = self.interaction.guild
        guild_members = {m.id: m for m in guild.members} if guild else {}

        state.data = {
            "top_senders": top_senders,
            "top_receivers": top_receivers,
            "total_volume": total_volume,
        }
        state.extra = {"guild_members": guild_members}

        # Calculate max pages based on longest section
        max_entries = max(
            len(top_senders),
            len(top_receivers),
            1,
        )
        state.max_page = max(0, (max_entries - 1) // MULTI_SECTION_PAGE_SIZE)

    def _get_name_for_gambling(self, discord_id: int) -> str:
        """Get display name for gambling leaderboard entries."""
        state = self._tab_states[LeaderboardTab.GAMBLING]
        guild_members = state.extra.get("guild_members", {})
        bankruptcy_states = state.extra.get("bankruptcy_states", {})

        member = guild_members.get(discord_id)
        if member:
            name = member.display_name
        else:
            name = f"User {discord_id}"

        # Add tombstone if in bankruptcy penalty
        bankruptcy_state = bankruptcy_states.get(discord_id)
        if bankruptcy_state and bankruptcy_state.penalty_games_remaining > 0:
            name = f"{TOMBSTONE_EMOJI} {name}"

        return name

    def _get_name_for_predictions(self, discord_id: int) -> str:
        """Get display name for predictions leaderboard entries."""
        state = self._tab_states[LeaderboardTab.PREDICTIONS]
        guild_members = state.extra.get("guild_members", {})
        member = guild_members.get(discord_id)
        return member.display_name if member else f"User {discord_id}"

    def _get_name_for_tips(self, discord_id: int) -> str:
        """Get display name for tips leaderboard entries."""
        state = self._tab_states[LeaderboardTab.TIPS]
        guild_members = state.extra.get("guild_members", {})
        member = guild_members.get(discord_id)
        return member.display_name if member else f"User {discord_id}"

    def build_embed(self) -> discord.Embed:
        """Build embed for current tab and page."""
        state = self._tab_states[self.current_tab]

        if self.current_tab == LeaderboardTab.BALANCE:
            return self._build_balance_embed(state)
        elif self.current_tab == LeaderboardTab.GAMBLING:
            return self._build_gambling_embed(state)
        elif self.current_tab == LeaderboardTab.PREDICTIONS:
            return self._build_predictions_embed(state)
        elif self.current_tab == LeaderboardTab.GLICKO:
            return self._build_glicko_embed(state)
        elif self.current_tab == LeaderboardTab.OPENSKILL:
            return self._build_openskill_embed(state)
        elif self.current_tab == LeaderboardTab.TIPS:
            return self._build_tips_embed(state)

        # Fallback
        return discord.Embed(title="Leaderboard", description="Unknown tab")

    def _build_balance_embed(self, state: TabState) -> discord.Embed:
        """Build Balance tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > Balance",
            color=discord.Color.gold(),
        )

        if not state.data or not state.data.get("players"):
            embed.description = "No players registered yet!"
            return embed

        players = state.data["players"]
        total_count = state.data["total_count"]
        debtors = state.data.get("debtors", [])

        start_idx = state.current_page * SINGLE_SECTION_PAGE_SIZE
        end_idx = start_idx + SINGLE_SECTION_PAGE_SIZE
        page_players = players[start_idx:end_idx]

        lines = []
        for i, entry in enumerate(page_players, start=start_idx + 1):
            medal = "🥇" if i == 1 else "🥈" if i == 2 else "🥉" if i == 3 else f"{i}."
            stats = f"{entry['wins']}-{entry['losses']}"
            if entry["wins"] + entry["losses"] > 0:
                stats += f" ({entry['win_rate']:.0f}%)"
            rating_display = f" [{entry['rating']}]" if entry["rating"] is not None else ""
            is_real_user = entry["discord_id"] and entry["discord_id"] > 0
            display_name = f"<@{entry['discord_id']}>" if is_real_user else entry["username"]
            jopacoin = entry.get("jopacoin_balance", 0)
            lines.append(f"{medal} **{display_name}** - {jopacoin} {JOPACOIN_EMOTE} - {stats}{rating_display}")

        embed.description = "\n".join(lines) if lines else "No players on this page."

        # Wall of Shame on first page only
        if state.current_page == 0 and debtors:
            shame_lines = []
            for i, debtor in enumerate(debtors[:10], 1):
                is_real_user = debtor["discord_id"] and debtor["discord_id"] > 0
                display_name = f"<@{debtor['discord_id']}>" if is_real_user else debtor["username"]
                shame_lines.append(f"{i}. {display_name} - {debtor['balance']} {JOPACOIN_EMOTE}")
            embed.add_field(name="Wall of Shame", value="\n".join(shame_lines), inline=False)

        # Footer
        page_info = f"Page {state.current_page + 1}/{state.max_page + 1}"
        if total_count > len(players):
            page_info += f" • Showing {len(players)} of {total_count} players"
        embed.set_footer(text=page_info)

        return embed

    def _build_gambling_embed(self, state: TabState) -> discord.Embed:
        """Build Gambling tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > Gambling",
            color=0xFFD700,
        )

        if not state.data:
            embed.description = "Gambling stats service is not available."
            return embed

        leaderboard = state.data

        if not leaderboard.top_earners and not leaderboard.hall_of_degen:
            embed.description = "No gambling data yet! Players need at least 3 settled bets to appear."
            return embed

        start = state.current_page * MULTI_SECTION_PAGE_SIZE
        end = start + MULTI_SECTION_PAGE_SIZE

        # Top earners
        if leaderboard.top_earners:
            page_entries = leaderboard.top_earners[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_gambling(entry.discord_id)
                    pnl = entry.net_pnl
                    pnl_str = f"+{pnl}" if pnl >= 0 else str(pnl)
                    lines.append(f"{i}. **{name}** {pnl_str} {JOPACOIN_EMOTE} ({entry.win_rate:.0%})")
                embed.add_field(name=" Top Earners", value="\n".join(lines), inline=False)

        # Down bad
        down_bad = [e for e in leaderboard.down_bad if e.net_pnl < 0]
        if down_bad:
            page_entries = down_bad[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_gambling(entry.discord_id)
                    lines.append(f"{i}. **{name}** {entry.net_pnl} {JOPACOIN_EMOTE} ({entry.win_rate:.0%})")
                embed.add_field(name=" Down Bad", value="\n".join(lines), inline=False)

        # Hall of Degen
        if leaderboard.hall_of_degen:
            page_entries = leaderboard.hall_of_degen[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_gambling(entry.discord_id)
                    lines.append(f"{i}. **{name}** {entry.degen_score} {entry.degen_emoji} {entry.degen_title}")
                embed.add_field(name=" Hall of Degen", value="\n".join(lines), inline=False)

        # Biggest gamblers
        if leaderboard.biggest_gamblers:
            page_entries = leaderboard.biggest_gamblers[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_gambling(entry.discord_id)
                    lines.append(f"{i}. **{name}** {entry.total_wagered} {JOPACOIN_EMOTE} wagered")
                embed.add_field(name=" Biggest Gamblers", value="\n".join(lines), inline=False)

        # Footer
        footer_parts = []
        if leaderboard.server_stats:
            s = leaderboard.server_stats
            footer_parts.append(
                f" {s['total_bets']} bets • {s['total_wagered']} {JOPACOIN_EMOTE} wagered • "
                f"{s['unique_gamblers']} players • {s['total_bankruptcies']} bankruptcies"
            )
        footer_parts.append(f"Page {state.current_page + 1}/{state.max_page + 1}")
        embed.set_footer(text=" | ".join(footer_parts))

        return embed

    def _build_predictions_embed(self, state: TabState) -> discord.Embed:
        """Build Predictions tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > Predictions",
            color=0xFFD700,
        )

        if not state.data:
            embed.description = "Prediction service is not available."
            return embed

        leaderboard = state.data["leaderboard"]
        server_stats = state.data["server_stats"]

        if not leaderboard.get("top_earners") and not leaderboard.get("most_accurate"):
            embed.description = "No prediction data yet! Users need at least 2 resolved predictions to appear."
            return embed

        start = state.current_page * MULTI_SECTION_PAGE_SIZE
        end = start + MULTI_SECTION_PAGE_SIZE

        # Top earners
        if leaderboard.get("top_earners"):
            page_entries = leaderboard["top_earners"][start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_predictions(entry["discord_id"])
                    pnl = entry["net_pnl"]
                    pnl_str = f"+{pnl}" if pnl >= 0 else str(pnl)
                    lines.append(f"{i}. **{name}** {pnl_str} {JOPACOIN_EMOTE} ({entry['win_rate']:.0%})")
                embed.add_field(name=" Top Earners", value="\n".join(lines), inline=False)

        # Down bad
        down_bad = [e for e in leaderboard.get("down_bad", []) if e["net_pnl"] < 0]
        if down_bad:
            page_entries = down_bad[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_predictions(entry["discord_id"])
                    lines.append(f"{i}. **{name}** {entry['net_pnl']} {JOPACOIN_EMOTE} ({entry['win_rate']:.0%})")
                embed.add_field(name=" Down Bad", value="\n".join(lines), inline=False)

        # Most accurate
        if leaderboard.get("most_accurate"):
            page_entries = leaderboard["most_accurate"][start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_predictions(entry["discord_id"])
                    lines.append(f"{i}. **{name}** {entry['win_rate']:.0%} ({entry['wins']}W-{entry['losses']}L)")
                embed.add_field(name=" Most Accurate", value="\n".join(lines), inline=False)

        # Footer
        footer_parts = []
        if server_stats and server_stats.get("total_predictions"):
            footer_parts.append(
                f" {server_stats['total_predictions']} predictions • "
                f"{server_stats['total_bets'] or 0} bets • "
                f"{server_stats['total_wagered'] or 0} wagered"
            )
        footer_parts.append(f"Page {state.current_page + 1}/{state.max_page + 1}")
        embed.set_footer(text=" | ".join(footer_parts))

        return embed

    def _build_glicko_embed(self, state: TabState) -> discord.Embed:
        """Build Glicko-2 tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > Glicko-2 Rating",
            description="Primary inhouse skill rating",
            color=discord.Color.gold(),
        )

        if not state.data or not state.data.get("players"):
            embed.description = "No players with Glicko-2 ratings yet!"
            return embed

        players = state.data["players"]
        total_rated = state.data["total_rated"]

        start_idx = state.current_page * SINGLE_SECTION_PAGE_SIZE
        end_idx = start_idx + SINGLE_SECTION_PAGE_SIZE
        page_players = players[start_idx:end_idx]

        lines = []
        for i, entry in enumerate(page_players, start=start_idx + 1):
            medal = "🥇" if i == 1 else "🥈" if i == 2 else "🥉" if i == 3 else f"{i}."
            is_real_user = entry["discord_id"] and entry["discord_id"] > 0
            display_name = f"<@{entry['discord_id']}>" if is_real_user else entry["username"]
            rating = entry["rating_display"]
            certainty = entry["certainty"]
            record = f"{entry['wins']}-{entry['losses']}"
            lines.append(f"{medal} **{display_name}** {rating} ({certainty:.0f}%) {record}")

        field_content = "\n".join(lines) if lines else "No players."
        embed.add_field(name="Rankings", value=truncate_field(field_content), inline=False)

        # Footer
        page_info = f"Page {state.current_page + 1}/{state.max_page + 1}"
        if total_rated > len(players):
            page_info += f" • Showing {len(players)} of {total_rated} rated players"
        embed.set_footer(text=page_info)

        return embed

    def _build_openskill_embed(self, state: TabState) -> discord.Embed:
        """Build OpenSkill tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > OpenSkill Rating",
            description="Fantasy-weighted performance rating",
            color=discord.Color.gold(),
        )

        if not state.data or not state.data.get("players"):
            embed.description = "No players with OpenSkill ratings yet!"
            return embed

        players = state.data["players"]
        total_rated = state.data["total_rated"]

        start_idx = state.current_page * SINGLE_SECTION_PAGE_SIZE
        end_idx = start_idx + SINGLE_SECTION_PAGE_SIZE
        page_players = players[start_idx:end_idx]

        lines = []
        for i, entry in enumerate(page_players, start=start_idx + 1):
            medal = "🥇" if i == 1 else "🥈" if i == 2 else "🥉" if i == 3 else f"{i}."
            is_real_user = entry["discord_id"] and entry["discord_id"] > 0
            display_name = f"<@{entry['discord_id']}>" if is_real_user else entry["username"]
            rating = entry["rating_display"]
            certainty = entry["certainty"]
            record = f"{entry['wins']}-{entry['losses']}"
            lines.append(f"{medal} **{display_name}** {rating} ({certainty:.0f}%) {record}")

        field_content = "\n".join(lines) if lines else "No players."
        embed.add_field(name="Rankings", value=truncate_field(field_content), inline=False)

        # Footer
        page_info = f"Page {state.current_page + 1}/{state.max_page + 1}"
        if total_rated > len(players):
            page_info += f" • Showing {len(players)} of {total_rated} rated players"
        embed.set_footer(text=page_info)

        return embed

    def _build_tips_embed(self, state: TabState) -> discord.Embed:
        """Build Tips tab embed."""
        embed = discord.Embed(
            title="LEADERBOARD > Tips",
            description="Jopacoin tipping rankings",
            color=0xE91E63,  # Pink color for generosity theme
        )

        if not state.data:
            embed.description = "Tip tracking service is not available."
            return embed

        top_senders = state.data.get("top_senders", [])
        top_receivers = state.data.get("top_receivers", [])
        total_volume = state.data.get("total_volume", {})

        if not top_senders and not top_receivers:
            embed.description = "No tips yet! Use `/tip` to spread some jopacoin love."
            return embed

        start = state.current_page * MULTI_SECTION_PAGE_SIZE
        end = start + MULTI_SECTION_PAGE_SIZE

        # Most Generous (top senders)
        if top_senders:
            page_entries = top_senders[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_tips(entry["discord_id"])
                    lines.append(
                        f"{i}. **{name}** {entry['total_amount']} {JOPACOIN_EMOTE} ({entry['tip_count']} tips)"
                    )
                embed.add_field(name="💝 Most Generous", value="\n".join(lines), inline=False)

        # Fan Favorites (top receivers)
        if top_receivers:
            page_entries = top_receivers[start:end]
            if page_entries:
                lines = []
                for i, entry in enumerate(page_entries, start + 1):
                    name = self._get_name_for_tips(entry["discord_id"])
                    lines.append(
                        f"{i}. **{name}** {entry['total_amount']} {JOPACOIN_EMOTE} ({entry['tip_count']} tips)"
                    )
                embed.add_field(name="⭐ Fan Favorites", value="\n".join(lines), inline=False)

        # Server Stats
        if total_volume:
            stats_lines = [
                f"**Total Tips:** {total_volume.get('total_transactions', 0)}",
                f"**Total Tipped:** {total_volume.get('total_amount', 0)} {JOPACOIN_EMOTE}",
                f"**Fees Collected:** {total_volume.get('total_fees', 0)} {JOPACOIN_EMOTE}",
            ]
            embed.add_field(name="📊 Server Stats", value="\n".join(stats_lines), inline=False)

        # Footer
        embed.set_footer(text=f"Page {state.current_page + 1}/{state.max_page + 1}")

        return embed

    async def _handle_tab_switch(self, interaction: discord.Interaction, tab: LeaderboardTab) -> None:
        """Handle tab button click with rate limiting and concurrency control."""
        # Rate limiting: 1.5s cooldown between tab switches per user
        now = time.time()
        last_time = self._last_interaction_time.get(interaction.user.id, 0)
        if now - last_time < 1.5:
            await interaction.response.defer()
            return
        self._last_interaction_time[interaction.user.id] = now

        # Defer if data not loaded yet (may take a moment)
        if not self._tab_states[tab].loaded:
            await interaction.response.defer()

        async with self._update_lock:
            self.current_tab = tab
            self._update_button_styles()

            # Load data if not already loaded
            await self._load_tab_data(tab)

            self._update_pagination_buttons()
            embed = self.build_embed()

            if interaction.response.is_done():
                await interaction.edit_original_response(embed=embed, view=self)
            else:
                await interaction.response.edit_message(embed=embed, view=self)

    async def _handle_pagination(self, interaction: discord.Interaction, delta: int) -> None:
        """Handle pagination button click."""
        async with self._update_lock:
            state = self._tab_states[self.current_tab]
            new_page = state.current_page + delta
            new_page = max(0, min(new_page, state.max_page))

            if new_page == state.current_page:
                await interaction.response.defer()
                return

            state.current_page = new_page
            self._update_pagination_buttons()
            embed = self.build_embed()
            await interaction.response.edit_message(embed=embed, view=self)

    # Row 0: Tab buttons
    @discord.ui.button(label="Balance", style=discord.ButtonStyle.primary, row=0)
    async def balance_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.BALANCE)

    @discord.ui.button(label="Gambling", style=discord.ButtonStyle.secondary, row=0)
    async def gambling_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.GAMBLING)

    @discord.ui.button(label="Predictions", style=discord.ButtonStyle.secondary, row=0)
    async def predictions_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.PREDICTIONS)

    @discord.ui.button(label="Glicko", style=discord.ButtonStyle.secondary, row=0)
    async def glicko_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.GLICKO)

    @discord.ui.button(label="OpenSkill", style=discord.ButtonStyle.secondary, row=0)
    async def openskill_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.OPENSKILL)

    # Row 1: Tips tab and pagination buttons
    @discord.ui.button(label="Tips", style=discord.ButtonStyle.secondary, row=1)
    async def tips_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_tab_switch(interaction, LeaderboardTab.TIPS)

    @discord.ui.button(label=" Previous", style=discord.ButtonStyle.secondary, row=1)
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_pagination(interaction, -1)

    @discord.ui.button(label="Next ", style=discord.ButtonStyle.secondary, row=1)
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_pagination(interaction, 1)

    async def on_timeout(self) -> None:
        """Delete the message when view times out."""
        if self.message:
            try:
                await self.message.delete()
                logger.info(f"Leaderboard message {self.message.id} deleted after timeout")
            except discord.NotFound:
                pass
            except discord.HTTPException as e:
                logger.warning(f"Failed to delete leaderboard message: {e}")


class InfoCommands(commands.Cog):
    """Commands for viewing information and leaderboards."""

    def __init__(
        self,
        bot: commands.Bot,
        player_repo,
        match_repo,
        role_emojis: dict,
        role_names: dict,
        *,
        flavor_text_service=None,
        guild_config_service=None,
        gambling_stats_service=None,
        prediction_service=None,
        bankruptcy_service=None,
    ):
        self.bot = bot
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.role_emojis = role_emojis
        self.role_names = role_names
        self.flavor_text_service = flavor_text_service
        self.guild_config_service = guild_config_service
        self.gambling_stats_service = gambling_stats_service
        self.prediction_service = prediction_service
        self.bankruptcy_service = bankruptcy_service

    @app_commands.command(name="help", description="List all available commands")
    async def help_command(self, interaction: discord.Interaction):
        """Show all available commands."""
        logger.info(f"Help command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        embed = discord.Embed(
            title="📚 Cama Shuffle Bot Commands",
            description="All available commands for the matchmaking bot",
            color=discord.Color.blue(),
        )

        # Registration & Profile
        embed.add_field(
            name="👤 Registration & Profile",
            value=(
                "`/register` - Register yourself as a player\n"
                "`/setroles` - Set your preferred roles (1-5)\n"
                "`/profile` - View unified profile (stats, rating, economy, gambling, predictions, Dota, teammates)\n"
                "`/matchup` - Head-to-head comparison between two players"
            ),
            inline=False,
        )

        # Dota 2 Stats
        embed.add_field(
            name="📊 Dota 2 Stats (OpenDota)",
            value=(
                "`/matchhistory` - Recent matches with heroes and stats\n"
                "`/viewmatch` - View detailed match embed\n"
                "`/recent` - Recent matches as image table\n"
                "*Use `/profile` Dota tab for role & lane graphs*"
            ),
            inline=False,
        )

        # Dota 2 Reference
        embed.add_field(
            name="📖 Dota 2 Reference",
            value=(
                "`/hero` - Look up hero stats, abilities, talents\n"
                "`/ability` - Look up ability details"
            ),
            inline=False,
        )

        # Lobby Management
        embed.add_field(
            name="🎮 Lobby Management",
            value=(
                "`/lobby` - Create or view the matchmaking lobby\n"
                "`/kick` - Kick a player (Admin or lobby creator only)\n"
                "`/resetlobby` - Reset the current lobby (Admin or lobby creator only)\n"
                "Use buttons in the thread to join/leave"
            ),
            inline=False,
        )

        # Match Management
        leverage_str = ", ".join(f"{x}x" for x in LEVERAGE_TIERS)
        embed.add_field(
            name="⚔️ Match Management",
            value=(
                "`/shuffle` - Create balanced teams from lobby (pool betting)\n"
                "`/record` - Record a match result"
            ),
            inline=False,
        )

        # Betting
        embed.add_field(
            name=f"🎰 Betting ({JOPACOIN_EMOTE} Jopacoin)",
            value=(
                f"`/bet` - Bet on Radiant or Dire (leverage: {leverage_str})\n"
                "  • Can place multiple bets on the same team\n"
                "  • Leverage can push you into debt\n"
                "  • Cannot bet while in debt\n"
                "`/mybets` - View your active bets and potential payout\n"
                "`/balance` - Check your jopacoin balance and debt\n"
                "`/paydebt` - Help another player pay off their debt (be a philanthropist!)\n"
                "`/bankruptcy` - Declare bankruptcy (clears debt, 1 week cooldown, 5 game penalty)"
            ),
            inline=False,
        )

        # Leaderboard
        embed.add_field(
            name="🏆 Leaderboard",
            value=(
                "`/leaderboard` - View leaderboard (default: balance)\n"
                "`/leaderboard type:glicko` - Glicko-2 rating rankings\n"
                "`/leaderboard type:openskill` - OpenSkill (fantasy-weighted) rankings\n"
                "`/leaderboard type:gambling` - Gambling rankings & Hall of Degen\n"
                "`/leaderboard type:predictions` - Prediction market rankings\n"
                "`/leaderboard type:tips` - Tipping rankings (generous/popular)\n"
                "`/calibration` - Rating system health & calibration stats"
            ),
            inline=False,
        )

        # Admin Commands (only show to admins)
        if has_admin_permission(interaction):
            embed.add_field(
                name="🔧 Admin Commands",
                value=(
                    "`/addfake` - Add fake users to lobby for testing\n"
                    "`/resetuser` - Reset a specific user's account\n"
                    "`/setleague` - Set Valve league ID for this server\n"
                    "`/enrichmatch` - Enrich match with Valve API data\n"
                    "`/backfillsteamid` - Backfill steam IDs from Dotabuff URLs\n"
                    "`/showconfig` - View server configuration\n"
                    "`/rebuildpairings` - Rebuild pairwise stats from match history"
                ),
                inline=False,
            )

        embed.set_footer(text="Tip: Type / and use Discord's autocomplete to see command details!")

        await interaction.followup.send(embed=embed, ephemeral=True)

    @app_commands.command(name="leaderboard", description="View leaderboard (balance, gambling, or predictions)")
    @app_commands.describe(
        type="Leaderboard type (default: balance)",
        limit="Number of entries to show (default: 100, max: 100)",
    )
    @app_commands.choices(type=[
        app_commands.Choice(name="Balance", value="balance"),
        app_commands.Choice(name="Glicko-2 Rating", value="glicko"),
        app_commands.Choice(name="OpenSkill Rating", value="openskill"),
        app_commands.Choice(name="Gambling", value="gambling"),
        app_commands.Choice(name="Predictions", value="predictions"),
        app_commands.Choice(name="Tips", value="tips"),
    ])
    async def leaderboard(
        self,
        interaction: discord.Interaction,
        type: app_commands.Choice[str] | None = None,
        limit: int = 100,
    ):
        """Show unified leaderboard with tabs for all leaderboard types."""
        leaderboard_type = type.value if type else "balance"
        logger.info(f"Leaderboard command: User {interaction.user.id} ({interaction.user}), type={leaderboard_type}")
        guild = interaction.guild if hasattr(interaction, "guild") else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="leaderboard",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=3,
            per_seconds=20,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/leaderboard` again.",
                ephemeral=True,
            )
            return
        _dbg_log(
            "H1",
            "commands/info.py:leaderboard:start",
            "leaderboard invoked",
            {"user_id": interaction.user.id, "user": str(interaction.user)},
            run_id="run1",
        )
        # Defer response immediately to prevent interaction timeout
        if not await safe_defer(interaction, ephemeral=False):
            logger.warning("Leaderboard: defer failed, proceeding to send fallback response")

        # Validate limit to stay within safe Discord embed boundaries
        limit = max(1, min(limit, 100))

        try:
            # Map type string to LeaderboardTab enum
            tab_mapping = {
                "balance": LeaderboardTab.BALANCE,
                "gambling": LeaderboardTab.GAMBLING,
                "predictions": LeaderboardTab.PREDICTIONS,
                "glicko": LeaderboardTab.GLICKO,
                "openskill": LeaderboardTab.OPENSKILL,
                "tips": LeaderboardTab.TIPS,
            }
            initial_tab = tab_mapping.get(leaderboard_type, LeaderboardTab.BALANCE)

            # Create unified view
            view = UnifiedLeaderboardView(
                cog=self,
                guild_id=guild.id if guild else None,
                interaction=interaction,
                initial_tab=initial_tab,
                limit=limit,
            )

            # Load initial tab data
            await view._load_tab_data(initial_tab)
            view._update_pagination_buttons()

            # Build initial embed
            embed = view.build_embed()

            logger.info(f"Leaderboard embed created for tab={leaderboard_type}")

            message = await safe_followup(
                interaction,
                embed=embed,
                view=view,
                allowed_mentions=discord.AllowedMentions(users=True),
            )
            # Store message reference for deletion on timeout
            if message:
                view.message = message

        except Exception as e:
            logger.error(f"Error in leaderboard command: {str(e)}", exc_info=True)
            try:
                await safe_followup(
                    interaction,
                    content=f"❌ Error: {str(e)}",
                    ephemeral=True,
                )
            except Exception:
                logger.error("Failed to send error message for leaderboard command")

    @app_commands.command(
        name="calibration", description="View rating system stats and calibration progress"
    )
    @app_commands.describe(user="Optional: View detailed stats for a specific player")
    async def calibration(
        self, interaction: discord.Interaction, user: discord.Member | None = None
    ):
        """Show rating system health and calibration stats."""
        target_user = user or interaction.user
        logger.info(f"Calibration command: User {interaction.user.id}, target={target_user.id}")
        guild = interaction.guild if hasattr(interaction, "guild") else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="calibration",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/calibration` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        try:
            rating_system = CamaRatingSystem()

            # If user specified, show individual stats
            if user is not None:
                await self._show_individual_calibration(interaction, user, rating_system)
                return

            # Otherwise show server-wide stats
            players = self.player_repo.get_all() if self.player_repo else []
            match_count = self.match_repo.get_match_count() if self.match_repo else 0
            match_predictions = (
                self.match_repo.get_recent_match_predictions(limit=200)
                if self.match_repo
                else []
            )
            rating_history_entries = (
                self.match_repo.get_recent_rating_history(limit=500) if self.match_repo else []
            )
            biggest_upsets = (
                self.match_repo.get_biggest_upsets(limit=5) if self.match_repo else []
            )
            player_performance = (
                self.match_repo.get_player_performance_stats() if self.match_repo else []
            )

            stats = compute_calibration_stats(
                players=players,
                match_count=match_count,
                match_predictions=match_predictions,
                rating_history_entries=rating_history_entries,
            )

            def display_name(player) -> str:
                if player.discord_id and player.discord_id > 0:
                    return f"<@{player.discord_id}>"
                return player.name

            def format_ranked(players_list, value_fn, value_fmt: str) -> str:
                lines = []
                for idx, player in enumerate(players_list[:3], 1):
                    value = value_fn(player)
                    lines.append(f"{idx}. {display_name(player)} ({value_fmt.format(value)})")
                return "\n".join(lines) if lines else "n/a"

            def format_drift(entries) -> str:
                if not entries:
                    return "n/a"
                parts = []
                for player, drift in entries[:3]:
                    parts.append(f"{display_name(player)} ({drift:+.0f})")
                return ", ".join(parts)

            buckets = stats["rating_buckets"]
            avg_rating_text = (
                f"{stats['avg_rating']:.0f}" if stats["avg_rating"] is not None else "n/a"
            )
            median_rating_text = (
                f"{stats['median_rating']:.0f}" if stats["median_rating"] is not None else "n/a"
            )
            rating_distribution = (
                f"Immortal (1355+): {buckets['Immortal']} | Divine (1155-1354): {buckets['Divine']}\n"
                f"Ancient (962-1154): {buckets['Ancient']} | Legend (770-961): {buckets['Legend']}\n"
                f"Archon (578-769): {buckets['Archon']} | Crusader (385-577): {buckets['Crusader']}\n"
                f"Guardian (192-384): {buckets['Guardian']} | Herald (0-191): {buckets['Herald']}\n"
                f"Avg: {avg_rating_text} | Median: {median_rating_text}"
            )

            rd_tiers = stats["rd_tiers"]
            avg_certainty_text = (
                f"{stats['avg_certainty']:.0f}%"
                if stats["avg_certainty"] is not None
                else "n/a"
            )
            avg_rd_text = f"{stats['avg_rd']:.0f}" if stats["avg_rd"] is not None else "n/a"
            calibration_progress = (
                f"**Locked In** (RD ≤75, 79-100% certain): {rd_tiers['Locked In']}\n"
                f"**Settling** (RD 76-150, 57-79% certain): {rd_tiers['Settling']}\n"
                f"**Developing** (RD 151-250, 29-57% certain): {rd_tiers['Developing']}\n"
                f"**Fresh** (RD 251+, 0-29% certain): {rd_tiers['Fresh']}\n"
                f"\nAvg: RD {avg_rd_text} ({avg_certainty_text} certain)"
            )

            prediction_quality = stats["prediction_quality"]
            if prediction_quality["count"]:
                upset_rate = (
                    f"{prediction_quality['upset_rate']:.0%}"
                    if prediction_quality["upset_rate"] is not None
                    else "n/a"
                )
                # Brier score: 0 = perfect, 0.25 = coin flip, lower is better
                brier = prediction_quality["brier"]
                brier_quality = "excellent" if brier < 0.15 else "good" if brier < 0.20 else "fair" if brier < 0.25 else "poor"
                balance_rate = prediction_quality["balance_rate"]
                balance_desc = "very balanced" if balance_rate >= 0.8 else "balanced" if balance_rate >= 0.5 else "unbalanced"
                prediction_text = (
                    f"**{prediction_quality['count']}** matches analyzed\n"
                    f"Brier Score: **{brier:.3f}** ({brier_quality})\n"
                    f"Pick Accuracy: **{prediction_quality['accuracy']:.0%}** of favorites won\n"
                    f"Balance Rate: **{balance_rate:.0%}** were close games ({balance_desc})\n"
                    f"Upset Rate: **{upset_rate}** underdogs won"
                )
            else:
                prediction_text = "No prediction data yet."

            rating_movement = stats["rating_movement"]
            if rating_movement["count"]:
                avg_delta = rating_movement["avg_delta"]
                median_delta = rating_movement["median_delta"]
                movement_text = (
                    f"**{rating_movement['count']}** rating changes recorded\n"
                    f"Avg change per game: **±{avg_delta:.1f}** points\n"
                    f"Median change: **±{median_delta:.1f}** points\n"
                    f"*Higher = more volatile matches*"
                )
            else:
                movement_text = "No rating history yet."

            if stats["avg_drift"] is not None and stats["median_drift"] is not None:
                avg_drift = stats["avg_drift"]
                median_drift = stats["median_drift"]
                drift_direction = "outperforming" if avg_drift > 0 else "underperforming" if avg_drift < 0 else "matching"
                drift_text = (
                    f"*Current rating vs initial MMR seed*\n"
                    f"Avg: **{avg_drift:+.0f}** | Median: **{median_drift:+.0f}**\n"
                    f"Players are {drift_direction} their pub MMR\n"
                    f"📈 Gainers: {format_drift(stats['biggest_gainers'])}\n"
                    f"📉 Drops: {format_drift(stats['biggest_drops'])}"
                )
            else:
                drift_text = "No seed MMR data yet."

            # Side balance (Radiant vs Dire)
            side_balance = stats["side_balance"]
            if side_balance["total"]:
                radiant_rate = side_balance["radiant_rate"]
                dire_rate = side_balance["dire_rate"]
                balance_status = (
                    "perfectly balanced" if abs(radiant_rate - 0.5) < 0.05
                    else "slightly favored" if abs(radiant_rate - 0.5) < 0.1
                    else "noticeably favored"
                )
                favored_side = "Radiant" if radiant_rate > 0.5 else "Dire" if radiant_rate < 0.5 else "Neither"
                side_text = (
                    f"**Radiant**: {side_balance['radiant_wins']}W ({radiant_rate:.0%})\n"
                    f"**Dire**: {side_balance['dire_wins']}W ({dire_rate:.0%})\n"
                    f"*{favored_side} {balance_status}*" if favored_side != "Neither" else f"*{balance_status}*"
                )
            else:
                side_text = "No match data yet."

            # Rating stability (calibrated vs uncalibrated)
            stability = stats["rating_stability"]
            if stability["calibrated_count"] and stability["uncalibrated_count"]:
                cal_avg = stability["calibrated_avg_delta"]
                uncal_avg = stability["uncalibrated_avg_delta"]
                ratio = stability["stability_ratio"]
                # Describe the stability
                if ratio < 0.7:
                    stability_desc = "excellent - ratings converging well"
                elif ratio < 0.9:
                    stability_desc = "good - system stabilizing"
                elif ratio < 1.1:
                    stability_desc = "fair - similar volatility across players"
                else:
                    stability_desc = "poor - calibrated players still volatile"
                stability_text = (
                    f"**Calibrated** (57%+ certain): ±{cal_avg:.1f} avg swing ({stability['calibrated_count']} games)\n"
                    f"**Uncalibrated** (<57% certain): ±{uncal_avg:.1f} avg swing ({stability['uncalibrated_count']} games)\n"
                    f"Stability: **{ratio:.2f}x** ({stability_desc})\n"
                    f"*<1.0 = calibrated swing less (good)*"
                )
            elif stability["calibrated_count"] or stability["uncalibrated_count"]:
                # Only one category has data
                if stability["calibrated_count"]:
                    stability_text = f"Only calibrated data: ±{stability['calibrated_avg_delta']:.1f} avg swing"
                else:
                    stability_text = f"Only uncalibrated data: ±{stability['uncalibrated_avg_delta']:.1f} avg swing"
            else:
                stability_text = "No rating history with RD data yet."

            embed = discord.Embed(title="Rating System Health", color=discord.Color.blue())
            avg_games_text = f"{stats['avg_games']:.1f}" if stats["avg_games"] is not None else "n/a"
            embed.add_field(
                name="System Overview",
                value=(
                    f"Total Players: {stats['total_players']} | Matches Recorded: {stats['match_count']}\n"
                    f"Players with Ratings: {stats['rated_players']} | Avg Games/Player: {avg_games_text}"
                ),
                inline=False,
            )
            embed.add_field(name="Rating Distribution", value=rating_distribution, inline=False)
            embed.add_field(name="📈 Calibration Progress", value=calibration_progress, inline=False)
            embed.add_field(name="⚔️ Side Balance", value=side_text, inline=True)
            embed.add_field(name="🎯 Prediction Quality", value=prediction_text, inline=True)
            embed.add_field(name="📊 Rating Movement", value=movement_text, inline=False)
            embed.add_field(name="🔄 Rating Drift (Seed vs Current)", value=drift_text, inline=False)
            embed.add_field(name="⚖️ Rating Stability", value=stability_text, inline=False)

            # Lobby type impact
            lobby_stats = self.match_repo.get_lobby_type_stats() if self.match_repo else []
            if lobby_stats:
                lobby_lines = []
                shuffle_stats = next((s for s in lobby_stats if s["lobby_type"] == "shuffle"), None)
                draft_stats = next((s for s in lobby_stats if s["lobby_type"] == "draft"), None)

                if shuffle_stats:
                    avg_swing = shuffle_stats["avg_swing"] or 0
                    games = shuffle_stats["games"]
                    actual = (shuffle_stats["actual_win_rate"] or 0) * 100
                    expected = (shuffle_stats["expected_win_rate"] or 0.5) * 100
                    lobby_lines.append(f"🎲 **Shuffle**: ±{avg_swing:.1f} avg swing ({games} games) | {actual:.0f}% actual vs {expected:.0f}% exp")

                if draft_stats:
                    avg_swing = draft_stats["avg_swing"] or 0
                    games = draft_stats["games"]
                    actual = (draft_stats["actual_win_rate"] or 0) * 100
                    expected = (draft_stats["expected_win_rate"] or 0.5) * 100
                    lobby_lines.append(f"👑 **Draft**: ±{avg_swing:.1f} avg swing ({games} games) | {actual:.0f}% actual vs {expected:.0f}% exp")

                # Add comparison insight if both exist
                if shuffle_stats and draft_stats and shuffle_stats["avg_swing"] and draft_stats["avg_swing"]:
                    shuffle_swing = shuffle_stats["avg_swing"]
                    draft_swing = draft_stats["avg_swing"]
                    if shuffle_swing > 0:
                        diff_pct = ((draft_swing - shuffle_swing) / shuffle_swing) * 100
                        if abs(diff_pct) >= 5:
                            more_volatile = "Draft" if diff_pct > 0 else "Shuffle"
                            lobby_lines.append(f"*{more_volatile} shows {abs(diff_pct):.0f}% larger swings - more volatile outcomes*")

                if lobby_lines:
                    embed.add_field(name="🎲 Lobby Type Impact", value="\n".join(lobby_lines), inline=False)

            embed.add_field(
                name="Highest Rated",
                value=format_ranked(
                    stats["top_rated"],
                    lambda p: rating_system.rating_to_display(p.glicko_rating or 0),
                    "{:.0f}",
                ),
                inline=True,
            )
            # Custom formatter for calibration showing both RD and certainty
            def format_calibration(players_list, most_calibrated: bool = True) -> str:
                lines = []
                for idx, player in enumerate(players_list[:3], 1):
                    rd = player.glicko_rd if player.glicko_rd is not None else 350
                    certainty = rd_to_certainty(rd)
                    lines.append(f"{idx}. {display_name(player)} (RD {rd:.0f}, {certainty:.0f}%)")
                return "\n".join(lines) if lines else "n/a"

            embed.add_field(
                name="Most Calibrated",
                value=format_calibration(stats["most_calibrated"]),
                inline=True,
            )
            embed.add_field(
                name="Most Volatile",
                value=format_ranked(
                    stats["highest_volatility"],
                    lambda p: p.glicko_volatility or 0.0,
                    "{:.3f}",
                ),
                inline=True,
            )
            embed.add_field(
                name="Lowest Rated",
                value=format_ranked(
                    stats["lowest_rated"],
                    lambda p: rating_system.rating_to_display(p.glicko_rating or 0),
                    "{:.0f}",
                ),
                inline=True,
            )
            embed.add_field(
                name="Least Calibrated",
                value=format_calibration(stats["least_calibrated"]),
                inline=True,
            )
            embed.add_field(
                name="Most Experienced",
                value=format_ranked(
                    stats["most_experienced"],
                    lambda p: p.wins + p.losses,
                    "{:.0f} games",
                ),
                inline=True,
            )

            # Last match prediction vs result
            if match_predictions:
                last_match = match_predictions[0]
                prob = last_match["expected_radiant_win_prob"]
                winner = last_match["winning_team"]
                if winner == 1:
                    result_text = f"Radiant won (had {prob:.0%} chance)"
                    outcome = "expected" if prob >= 0.5 else "upset"
                elif winner == 2:
                    result_text = f"Dire won (had {1-prob:.0%} chance)"
                    outcome = "expected" if prob <= 0.5 else "upset"
                else:
                    result_text = "Pending..."
                    outcome = ""
                outcome_emoji = "✅" if outcome == "expected" else "🔥" if outcome == "upset" else ""
                embed.add_field(
                    name="Last Match",
                    value=f"{outcome_emoji} {result_text}",
                    inline=False,
                )

            # Top 5 biggest upsets
            if biggest_upsets:
                upset_lines = []
                for upset in biggest_upsets[:5]:
                    prob = upset["underdog_win_prob"]
                    match_id = upset["match_id"]
                    winner = "Radiant" if upset["winning_team"] == 1 else "Dire"
                    upset_lines.append(f"Match #{match_id}: {winner} won ({prob:.0%} chance)")
                embed.add_field(
                    name="🔥 Biggest Upsets",
                    value="\n".join(upset_lines) if upset_lines else "No upsets yet",
                    inline=False,
                )

            # Top 3 outperformers
            if player_performance:
                outperformer_lines = []
                # Create a lookup for player names
                player_lookup = {p.discord_id: p for p in players}
                for perf in player_performance[:3]:
                    discord_id = perf["discord_id"]
                    over = perf["overperformance"]
                    matches = perf["total_matches"]
                    if discord_id in player_lookup:
                        name = f"<@{discord_id}>"
                    else:
                        name = f"ID:{discord_id}"
                    outperformer_lines.append(f"{name}: +{over:.1f} wins over expected ({matches} games)")
                if outperformer_lines:
                    embed.add_field(
                        name="🎯 Top Outperformers",
                        value="\n".join(outperformer_lines),
                        inline=False,
                    )

            embed.set_footer(text="RD = Rating Deviation | Drift = Current - Seed | Brier: 0=perfect, 0.25=coin flip")

            # Generate rating distribution chart
            rating_values = [p.glicko_rating for p in players if p.glicko_rating is not None]
            chart_file = None
            if rating_values:
                chart_buffer = draw_rating_distribution(
                    rating_values,
                    avg_rating=stats["avg_rating"],
                    median_rating=stats["median_rating"],
                )
                chart_file = discord.File(chart_buffer, filename="rating_distribution.png")
                embed.set_image(url="attachment://rating_distribution.png")

            await safe_followup(
                interaction,
                embed=embed,
                file=chart_file,
                ephemeral=True,
                allowed_mentions=discord.AllowedMentions(users=True),
            )
        except Exception as e:
            logger.error(f"Error in calibration command: {str(e)}", exc_info=True)
            await safe_followup(
                interaction,
                content=f"❌ Error: {str(e)}",
                ephemeral=True,
            )

    async def _show_individual_calibration(
        self,
        interaction: discord.Interaction,
        user: discord.Member,
        rating_system: CamaRatingSystem,
    ):
        """Show detailed calibration stats for an individual player."""
        # Get player data
        player = self.player_repo.get_by_id(user.id) if self.player_repo else None
        if not player:
            await safe_followup(
                interaction,
                content=f"❌ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Get detailed rating history with predictions
        history = (
            self.match_repo.get_player_rating_history_detailed(user.id, limit=50)
            if self.match_repo
            else []
        )

        # Get all players for percentile calculation
        all_players = self.player_repo.get_all() if self.player_repo else []
        rated_players = [p for p in all_players if p.glicko_rating is not None]

        # Calculate percentile
        if player.glicko_rating and rated_players:
            lower_count = sum(1 for p in rated_players if (p.glicko_rating or 0) < player.glicko_rating)
            percentile = (lower_count / len(rated_players)) * 100
        else:
            percentile = None

        # Calculate calibration tier
        rd = player.glicko_rd or 350
        if rd <= 75:
            calibration_tier = "Locked In"
        elif rd <= 150:
            calibration_tier = "Settling"
        elif rd <= 250:
            calibration_tier = "Developing"
        else:
            calibration_tier = "Fresh"

        # Calculate drift
        drift = None
        if player.initial_mmr and player.glicko_rating:
            seed_rating = rating_system.mmr_to_rating(player.initial_mmr)
            drift = player.glicko_rating - seed_rating

        # Analyze match history
        matches_with_predictions = [h for h in history if h.get("expected_team_win_prob") is not None]

        actual_wins = sum(1 for h in matches_with_predictions if h.get("won"))
        expected_wins = sum(h.get("expected_team_win_prob", 0) for h in matches_with_predictions)
        overperformance = actual_wins - expected_wins if matches_with_predictions else None

        # Win rate when favored vs underdog
        favored_matches = [h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) >= 0.55]
        underdog_matches = [h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) <= 0.45]
        favored_wins = sum(1 for h in favored_matches if h.get("won"))
        underdog_wins = sum(1 for h in underdog_matches if h.get("won"))

        # Rating trend (last 5 games)
        if len(history) >= 2:
            recent_delta = (history[0].get("rating") or 0) - (history[-1].get("rating") or 0)
            if len(history) > 5:
                last_5_delta = (history[0].get("rating") or 0) - (history[4].get("rating") or 0)
            else:
                last_5_delta = recent_delta
        else:
            recent_delta = None
            last_5_delta = None

        # Recent matches with predictions comparison (last 5)
        # Shows Glicko-2 vs OpenSkill expected outcomes vs actual result
        os_system = CamaOpenSkillSystem()
        recent_game_details = []
        for h in history[:5]:
            rating_after = h.get("rating")
            rating_before = h.get("rating_before")
            won = h.get("won")
            match_id = h.get("match_id")
            lobby_type = h.get("lobby_type", "shuffle")
            lobby_emoji = "👑" if lobby_type == "draft" else "🎲"
            glicko_expected = h.get("expected_team_win_prob")

            # Calculate rating delta
            if rating_after is not None and rating_before is not None:
                rating_delta = rating_after - rating_before
                delta_str = f"{rating_delta:+.0f}"
            else:
                delta_str = "?"

            result = "W" if won else "L"
            result_emoji = "✅" if won else "❌"

            # Get OpenSkill expected outcome for this match
            os_expected = None
            if match_id and self.match_repo:
                os_ratings = self.match_repo.get_os_ratings_for_match(match_id)
                if os_ratings["team1"] and os_ratings["team2"]:
                    team_num = h.get("team_number")
                    if team_num == 1:
                        os_expected = os_system.os_predict_win_probability(
                            os_ratings["team1"], os_ratings["team2"]
                        )
                    elif team_num == 2:
                        os_expected = os_system.os_predict_win_probability(
                            os_ratings["team2"], os_ratings["team1"]
                        )

            # Build compact prediction string: G=Glicko, O=OpenSkill
            pred_parts = []
            if glicko_expected is not None:
                pred_parts.append(f"G:{glicko_expected:.0%}")
            if os_expected is not None:
                pred_parts.append(f"O:{os_expected:.0%}")
            pred_str = " ".join(pred_parts) if pred_parts else "no pred"

            recent_game_details.append(
                f"{lobby_emoji}#{match_id}: {result_emoji}{result} ({pred_str}) → **{delta_str}**"
            )

        # Find biggest upset (win as underdog) and biggest choke (loss as favorite)
        upsets = [(h, h.get("expected_team_win_prob", 0.5)) for h in matches_with_predictions
                  if h.get("won") and (h.get("expected_team_win_prob") or 0.5) < 0.45]
        chokes = [(h, h.get("expected_team_win_prob", 0.5)) for h in matches_with_predictions
                  if not h.get("won") and (h.get("expected_team_win_prob") or 0.5) > 0.55]
        upsets.sort(key=lambda x: x[1])  # lowest prob first
        chokes.sort(key=lambda x: x[1], reverse=True)  # highest prob first

        # Current streak
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

        # Build embed
        embed = discord.Embed(
            title=f"Calibration Stats: {user.display_name}",
            color=discord.Color.blue(),
        )

        # Rating profile
        rating_display = rating_system.rating_to_display(player.glicko_rating) if player.glicko_rating else "N/A"
        certainty = 100 - rating_system.get_rating_uncertainty_percentage(rd)
        percentile_text = f"Top {100 - percentile:.0f}%" if percentile else "N/A"

        profile_text = (
            f"**Rating:** {rating_display} ({certainty:.0f}% certain)\n"
            f"**Tier:** {calibration_tier} | **Percentile:** {percentile_text}\n"
            f"**Volatility:** {player.glicko_volatility:.3f}" if player.glicko_volatility else f"**Rating:** {rating_display} ({certainty:.0f}% certain)\n**Tier:** {calibration_tier} | **Percentile:** {percentile_text}"
        )
        embed.add_field(name="📊 Rating Profile", value=profile_text, inline=False)

        # Drift
        if drift is not None:
            drift_emoji = "📈" if drift > 0 else "📉" if drift < 0 else "➡️"
            drift_text = f"{drift_emoji} **{drift:+.0f}** rating vs initial seed ({player.initial_mmr} MMR)"
            embed.add_field(name="🎯 Rating Drift", value=drift_text, inline=False)

        # Performance vs expectations
        if matches_with_predictions:
            perf_text = f"**Actual Wins:** {actual_wins} | **Expected:** {expected_wins:.1f}\n"
            if overperformance is not None:
                over_emoji = "🔥" if overperformance > 0 else "💀" if overperformance < 0 else "➡️"
                perf_text += f"**Over/Under:** {over_emoji} {overperformance:+.1f} wins"
            embed.add_field(name="📈 Performance", value=perf_text, inline=True)

            # Win rates
            winrate_text = ""
            if favored_matches:
                winrate_text += f"**When Favored (55%+):** {favored_wins}/{len(favored_matches)} ({favored_wins/len(favored_matches):.0%})\n"
            if underdog_matches:
                winrate_text += f"**As Underdog (45%-):** {underdog_wins}/{len(underdog_matches)} ({underdog_wins/len(underdog_matches):.0%})"
            if winrate_text:
                embed.add_field(name="🎲 Situational", value=winrate_text, inline=True)

        # Trend
        if last_5_delta is not None:
            trend_emoji = "📈" if last_5_delta > 0 else "📉" if last_5_delta < 0 else "➡️"
            trend_text = f"{trend_emoji} **{last_5_delta:+.0f}** over last {min(5, len(history))} games"
            if streak and streak_type:
                trend_text += f"\n🔥 Current: **{streak}{streak_type}** streak"
            embed.add_field(name="📉 Trend", value=trend_text, inline=True)

        # Recent matches with predictions comparison
        if recent_game_details:
            embed.add_field(
                name=f"📊 Last {len(recent_game_details)} Matches (G=Glicko O=OpenSkill)",
                value="\n".join(recent_game_details),
                inline=False,
            )

        # Lobby type breakdown for this player
        player_lobby_stats = self.match_repo.get_player_lobby_type_stats(user.id) if self.match_repo else []
        if player_lobby_stats and len(player_lobby_stats) > 1:
            lobby_lines = []
            shuffle_stats = next((s for s in player_lobby_stats if s["lobby_type"] == "shuffle"), None)
            draft_stats = next((s for s in player_lobby_stats if s["lobby_type"] == "draft"), None)

            if shuffle_stats:
                avg_swing = shuffle_stats["avg_swing"] or 0
                games = shuffle_stats["games"]
                actual = (shuffle_stats["actual_win_rate"] or 0) * 100
                expected = (shuffle_stats["expected_win_rate"] or 0.5) * 100
                lobby_lines.append(f"🎲 **Shuffle**: ±{avg_swing:.1f} avg ({games} games) | W: {actual:.0f}% vs {expected:.0f}% exp")

            if draft_stats:
                avg_swing = draft_stats["avg_swing"] or 0
                games = draft_stats["games"]
                actual = (draft_stats["actual_win_rate"] or 0) * 100
                expected = (draft_stats["expected_win_rate"] or 0.5) * 100
                lobby_lines.append(f"👑 **Draft**: ±{avg_swing:.1f} avg ({games} games) | W: {actual:.0f}% vs {expected:.0f}% exp")

            # Add comparison insight if both exist
            if shuffle_stats and draft_stats and shuffle_stats["avg_swing"] and draft_stats["avg_swing"]:
                shuffle_swing = shuffle_stats["avg_swing"]
                draft_swing = draft_stats["avg_swing"]
                if shuffle_swing > 0:
                    diff_pct = ((draft_swing - shuffle_swing) / shuffle_swing) * 100
                    if abs(diff_pct) >= 5:
                        more_volatile = "drafts" if diff_pct > 0 else "shuffles"
                        lobby_lines.append(f"*You swing {abs(diff_pct):.0f}% more in {more_volatile}*")

            if lobby_lines:
                embed.add_field(name="🎲 Rating Swings by Lobby Type", value="\n".join(lobby_lines), inline=False)

        # RD trend analysis - show how rating changes relate to RD
        if len(history) >= 2:
            # Calculate average rating swing and RD change over recent games
            rating_swings = []
            rd_changes = []
            for h in history[:5]:
                r_before = h.get("rating_before")
                r_after = h.get("rating")
                rd_b = h.get("rd_before")
                rd_a = h.get("rd_after")
                if r_before is not None and r_after is not None:
                    rating_swings.append(abs(r_after - r_before))
                if rd_b is not None and rd_a is not None:
                    rd_changes.append(rd_a - rd_b)

            if rating_swings and rd_changes:
                avg_swing = sum(rating_swings) / len(rating_swings)
                total_rd_change = sum(rd_changes)

                # Determine trend direction
                if total_rd_change < -5:
                    rd_trend = "📉 RD decreasing (converging)"
                    rating_expectation = "Expect smaller rating swings"
                elif total_rd_change > 5:
                    rd_trend = "📈 RD increasing (uncertain)"
                    rating_expectation = "Expect larger rating swings"
                else:
                    rd_trend = "➡️ RD stable"
                    rating_expectation = "Rating swings should be consistent"

                trend_analysis = (
                    f"{rd_trend}\n"
                    f"Avg swing: **±{avg_swing:.0f}** per game\n"
                    f"*{rating_expectation}*"
                )
                embed.add_field(name="🔄 Convergence Trend", value=trend_analysis, inline=True)

        # Biggest upset and choke
        highlights = []
        if upsets:
            best_upset = upsets[0]
            highlights.append(f"🔥 **Best Upset:** Won with {best_upset[1]:.0%} chance (Match #{best_upset[0].get('match_id')})")
        if chokes:
            worst_choke = chokes[0]
            highlights.append(f"💀 **Worst Choke:** Lost with {worst_choke[1]:.0%} chance (Match #{worst_choke[0].get('match_id')})")
        if highlights:
            embed.add_field(name="⚡ Highlights", value="\n".join(highlights), inline=False)

        # Hero performance from enriched matches
        hero_stats = self.match_repo.get_player_hero_stats(user.id, limit=8) if self.match_repo else []
        if hero_stats:
            # Calculate role alignment
            hero_breakdown = self.match_repo.get_player_hero_role_breakdown(user.id) if self.match_repo else []
            total_hero_games = sum(h["games"] for h in hero_breakdown)
            core_games = sum(h["games"] for h in hero_breakdown if classify_hero_role(h["hero_id"]) == "Core")
            support_games = total_hero_games - core_games

            # Check for role mismatch
            preferred_roles = player.preferred_roles or []
            prefers_support = any(r in ["4", "5"] for r in preferred_roles) and not any(r in ["1", "2", "3"] for r in preferred_roles)
            prefers_core = any(r in ["1", "2", "3"] for r in preferred_roles) and not any(r in ["4", "5"] for r in preferred_roles)

            role_mismatch = None
            if total_hero_games >= 5:
                core_pct = core_games / total_hero_games if total_hero_games > 0 else 0
                if prefers_support and core_pct > 0.6:
                    role_mismatch = f"⚠️ Prefers Support but plays {core_pct:.0%} Core heroes"
                elif prefers_core and core_pct < 0.4:
                    role_mismatch = f"⚠️ Prefers Core but plays {(1 - core_pct):.0%} Support heroes"

            # Build hero table
            hero_lines = []
            for h in hero_stats[:6]:
                hero_name = get_hero_short_name(h["hero_id"])
                wl = f"{h['wins']}-{h['losses']}"
                kda = f"{h['avg_kills']:.0f}/{h['avg_deaths']:.0f}/{h['avg_assists']:.0f}"
                gpm = f"{h['avg_gpm']:.0f}"
                dmg = f"{h['avg_damage'] / 1000:.1f}k" if h['avg_damage'] else "-"
                hero_lines.append(f"`{hero_name:<8}` {wl:<5} {kda:<9} {gpm:<4} {dmg}")

            hero_text = "```\nHero     W-L   KDA       GPM  Dmg\n"
            hero_text += "\n".join(hero_lines)
            hero_text += "\n```"

            if role_mismatch:
                hero_text += f"\n{role_mismatch}"

            embed.add_field(name="🦸 Recent Heroes", value=hero_text, inline=False)

        # Fantasy stats from enriched matches
        fantasy_stats = self.match_repo.get_player_fantasy_stats(user.id) if self.match_repo else None
        if fantasy_stats and fantasy_stats["total_games"] > 0:
            fp_text = (
                f"**Avg FP:** {fantasy_stats['avg_fp']:.1f} | "
                f"**Best:** {fantasy_stats['best_fp']:.1f} (Match #{fantasy_stats['best_match_id']})\n"
                f"**Total:** {fantasy_stats['total_fp']:.1f} FP over {fantasy_stats['total_games']} enriched games"
            )

            # Recent games with FP
            recent_fp = fantasy_stats.get("recent_games", [])[:5]
            if recent_fp:
                fp_details = []
                for g in recent_fp:
                    result = "W" if g["won"] else "L"
                    hero_name = get_hero_short_name(g["hero_id"]) if g.get("hero_id") else "?"
                    fp_details.append(f"#{g['match_id']}: {result} {hero_name} **{g['fantasy_points']:.1f}**")
                fp_text += "\n" + " | ".join(fp_details)

            embed.add_field(name="⭐ Fantasy Points", value=fp_text, inline=False)

        # OpenSkill Rating (Fantasy-Weighted)
        os_data = self.player_repo.get_openskill_rating(user.id) if self.player_repo else None
        if os_data:
            os_mu, os_sigma = os_data
            os_ordinal = os_system.ordinal(os_mu, os_sigma)
            os_calibrated = os_system.is_calibrated(os_sigma)
            os_certainty = os_system.get_certainty_percentage(os_sigma)
            os_display = os_system.mu_to_display(os_mu)

            os_text = (
                f"**Skill (μ):** {os_mu:.2f} → **{os_display}** display\n"
                f"**Uncertainty (σ):** {os_sigma:.3f} ({os_certainty:.0f}% certain)\n"
                f"**Ordinal** (μ-3σ): {os_ordinal:.2f}\n"
                f"**Calibrated:** {'Yes' if os_calibrated else 'No'}"
            )
            embed.add_field(name="🎲 OpenSkill Rating (Fantasy-Weighted)", value=os_text, inline=False)

        # Record
        record_text = f"**W-L:** {player.wins}-{player.losses}"
        if player.wins + player.losses > 0:
            record_text += f" ({player.wins / (player.wins + player.losses):.0%})"
        embed.add_field(name="📋 Record", value=record_text, inline=True)

        embed.set_footer(text="Rating delta shown per game | RD decrease = more stable rating")

        await safe_followup(
            interaction,
            embed=embed,
            ephemeral=True,
            allowed_mentions=discord.AllowedMentions(users=True),
        )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    # Get player_repo and config from bot
    player_repo = getattr(bot, "player_repo", None)
    match_repo = getattr(bot, "match_repo", None)
    role_emojis = getattr(bot, "role_emojis", {})
    role_names = getattr(bot, "role_names", {})
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    guild_config_service = getattr(bot, "guild_config_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)
    prediction_service = getattr(bot, "prediction_service", None)
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)

    await bot.add_cog(
        InfoCommands(
            bot,
            player_repo,
            match_repo,
            role_emojis,
            role_names,
            flavor_text_service=flavor_text_service,
            guild_config_service=guild_config_service,
            gambling_stats_service=gambling_stats_service,
            prediction_service=prediction_service,
            bankruptcy_service=bankruptcy_service,
        )
    )
