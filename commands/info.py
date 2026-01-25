"""
Information commands for the bot: /help, /leaderboard
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from config import LEVERAGE_TIERS
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from services.permissions import has_admin_permission
from utils.debug_logging import debug_log as _dbg_log
from utils.drawing import draw_rating_distribution
from utils.formatting import JOPACOIN_EMOTE
from utils.hero_lookup import get_hero_short_name, classify_hero_role
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.rating_insights import compute_calibration_stats, rd_to_certainty

logger = logging.getLogger("cama_bot.commands.info")

LEADERBOARD_PAGE_SIZE = 20  # Players per page (fits within 4096 char embed limit)


class LeaderboardView(discord.ui.View):
    """Paginated view for leaderboard with Previous/Next buttons."""

    def __init__(
        self,
        players_with_stats: list[dict],
        total_player_count: int,
        rating_system: "CamaRatingSystem",
        debtors: list[dict] | None = None,
        timeout: float = 840.0,  # 14 minutes (max is 15)
    ):
        super().__init__(timeout=timeout)
        self.players = players_with_stats
        self.total_player_count = total_player_count
        self.rating_system = rating_system
        self.debtors = debtors or []
        self.current_page = 0
        self.max_page = (len(players_with_stats) - 1) // LEADERBOARD_PAGE_SIZE
        self.message: discord.Message | None = None  # Store message reference for deletion
        self._update_buttons()

    def _update_buttons(self) -> None:
        """Enable/disable buttons based on current page."""
        self.prev_button.disabled = self.current_page == 0
        self.next_button.disabled = self.current_page >= self.max_page

    def build_embed(self) -> discord.Embed:
        """Build the embed for the current page."""
        embed = discord.Embed(title="🏆 Leaderboard", color=discord.Color.gold())

        start_idx = self.current_page * LEADERBOARD_PAGE_SIZE
        end_idx = start_idx + LEADERBOARD_PAGE_SIZE
        page_players = self.players[start_idx:end_idx]

        leaderboard_text = ""
        for i, entry in enumerate(page_players, start=start_idx + 1):
            medal = "🥇" if i == 1 else "🥈" if i == 2 else "🥉" if i == 3 else f"{i}."
            stats = f"{entry['wins']}-{entry['losses']}"
            if entry["wins"] + entry["losses"] > 0:
                stats += f" ({entry['win_rate']:.0f}%)"
            rating_display = f" [{entry['rating']}]" if entry["rating"] is not None else ""
            is_real_user = entry["discord_id"] and entry["discord_id"] > 0
            display_name = f"<@{entry['discord_id']}>" if is_real_user else entry["username"]
            jopacoin_balance = entry.get("jopacoin_balance", 0) or 0
            jopacoin_display = f"{jopacoin_balance} {JOPACOIN_EMOTE}"
            line = f"{medal} **{display_name}** - {jopacoin_display} - {stats}{rating_display}\n"
            leaderboard_text += line

        embed.description = leaderboard_text

        # Footer with page info
        page_info = f"Page {self.current_page + 1}/{self.max_page + 1}"
        if self.total_player_count > len(self.players):
            page_info += f" • Showing {len(self.players)} of {self.total_player_count} players"
        embed.set_footer(text=page_info)

        # Add Wall of Shame on first page only (uses separately-fetched debtors)
        if self.current_page == 0 and self.debtors:
            shame_text = ""
            for i, debtor in enumerate(self.debtors[:10], 1):
                is_real_user = debtor["discord_id"] and debtor["discord_id"] > 0
                display_name = (
                    f"<@{debtor['discord_id']}>" if is_real_user else debtor["username"]
                )
                shame_text += (
                    f"{i}. {display_name} - {debtor['balance']} {JOPACOIN_EMOTE}\n"
                )
            embed.add_field(name="Wall of Shame", value=shame_text, inline=False)

        return embed

    @discord.ui.button(label="◀ Previous", style=discord.ButtonStyle.secondary)
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        """Go to previous page."""
        self.current_page = max(0, self.current_page - 1)
        self._update_buttons()
        await interaction.response.edit_message(embed=self.build_embed(), view=self)

    @discord.ui.button(label="Next ▶", style=discord.ButtonStyle.secondary)
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        """Go to next page."""
        self.current_page = min(self.max_page, self.current_page + 1)
        self._update_buttons()
        await interaction.response.edit_message(embed=self.build_embed(), view=self)

    async def on_timeout(self) -> None:
        """Delete the message when view times out."""
        if self.message:
            try:
                await self.message.delete()
                logger.info(f"Leaderboard message {self.message.id} deleted after timeout")
            except discord.NotFound:
                pass  # Already deleted
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
                "`/leaderboard type:gambling` - Gambling rankings & Hall of Degen\n"
                "`/leaderboard type:predictions` - Prediction market rankings\n"
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
        limit="Number of entries to show (default: 20, max: 100)",
    )
    @app_commands.choices(type=[
        app_commands.Choice(name="Balance", value="balance"),
        app_commands.Choice(name="Gambling", value="gambling"),
        app_commands.Choice(name="Predictions", value="predictions"),
    ])
    async def leaderboard(
        self,
        interaction: discord.Interaction,
        type: app_commands.Choice[str] | None = None,
        limit: int = 20,
    ):
        """Show leaderboard - balance (default), gambling, or predictions."""
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
        if limit < 1 or limit > 100:
            await safe_followup(
                interaction,
                content="Please provide a limit between 1 and 100.",
                ephemeral=True,
            )
            return

        # Route to appropriate leaderboard handler
        if leaderboard_type == "gambling":
            await self._show_gambling_leaderboard(interaction, limit)
            return
        elif leaderboard_type == "predictions":
            await self._show_predictions_leaderboard(interaction, limit)
            return

        # Default: balance leaderboard
        try:
            rating_system = CamaRatingSystem()

            # Use optimized leaderboard query with SQL sorting
            # Fetch limit + extra for Wall of Shame section (debtors)
            leaderboard_players = self.player_repo.get_leaderboard(limit=limit)
            total_player_count = self.player_repo.get_player_count()

            logger.info(f"Leaderboard query returned {len(leaderboard_players)} players (total: {total_player_count})")
            # Log sample jopacoin values
            if leaderboard_players:
                sample = leaderboard_players[:3]
                for player in sample:
                    logger.info(f"  Sample: {player.name} - jopacoin={player.jopacoin_balance}")
            _dbg_log(
                "H2",
                "commands/info.py:leaderboard:query",
                "query rows",
                {
                    "row_count": len(leaderboard_players),
                    "total_players": total_player_count,
                    "samples": [
                        {
                            "id": int(p.discord_id) if p.discord_id else 0,
                            "name": p.name,
                            "jopacoin": int(p.jopacoin_balance),
                            "wins": int(p.wins),
                            "losses": int(p.losses),
                        }
                        for p in leaderboard_players[:3]
                    ],
                },
                run_id="run1",
            )

            if not leaderboard_players:
                await safe_followup(
                    interaction,
                    content="No players registered yet!",
                    ephemeral=True,
                )
                return

            # Build stats from already-sorted player objects
            players_with_stats = []
            for player in leaderboard_players:
                discord_id = player.discord_id
                if discord_id is None:
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
                jopacoin_balance = player.jopacoin_balance or 0

                players_with_stats.append(
                    {
                        "discord_id": discord_id,
                        "username": player.name,
                        "wins": wins,
                        "losses": losses,
                        "win_rate": win_rate,
                        "rating": cama_rating,
                        "jopacoin_balance": jopacoin_balance,
                    }
                )
            _dbg_log(
                "H3",
                "commands/info.py:leaderboard:stats_built",
                "built stats",
                {
                    "count": len(players_with_stats),
                    "first": players_with_stats[:1],
                },
            )

            # No need to sort - already sorted by SQL query

            # Log top 3 players
            logger.info("Top 3 players from SQL-sorted leaderboard:")
            for i, entry in enumerate(players_with_stats[:3], 1):
                logger.info(
                    f"  {i}. {entry['username']} - jopacoin={entry['jopacoin_balance']}, wins={entry['wins']}, rating={entry['rating']}"
                )
            _dbg_log(
                "H4",
                "commands/info.py:leaderboard:sorted",
                "sorted stats",
                {
                    "top3": players_with_stats[:3],
                },
            )

            if not players_with_stats:
                await safe_followup(
                    interaction,
                    content="No players registered yet!",
                    ephemeral=True,
                )
                return

            # Fetch debtors separately for Wall of Shame (always shown regardless of limit)
            debtors = self.player_repo.get_players_with_negative_balance()

            # Use paginated view for leaderboard
            view = LeaderboardView(
                players_with_stats=players_with_stats,
                total_player_count=total_player_count,
                rating_system=rating_system,
                debtors=debtors,
            )
            embed = view.build_embed()

            logger.info(f"Leaderboard embed created with {len(players_with_stats)} entries, {view.max_page + 1} pages")

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

    async def _show_gambling_leaderboard(self, interaction: discord.Interaction, limit: int):
        """Show the gambling leaderboard."""
        if not self.gambling_stats_service:
            await safe_followup(
                interaction,
                content="Gambling stats service is not available.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        limit = max(1, min(limit, 20))  # Clamp between 1 and 20

        leaderboard = self.gambling_stats_service.get_leaderboard(guild_id, limit=limit)

        if not leaderboard.top_earners and not leaderboard.hall_of_degen:
            await safe_followup(
                interaction,
                content="No gambling data yet! Players need at least 3 settled bets to appear.",
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title="🏆 GAMBLING LEADERBOARD",
            color=0xFFD700,  # Gold
        )

        # Pre-fetch guild members to avoid individual API calls
        guild_members = {m.id: m for m in interaction.guild.members} if interaction.guild else {}

        # Collect all unique discord_ids from all leaderboard sections
        all_discord_ids = set()
        for entry in leaderboard.top_earners:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.down_bad:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.hall_of_degen:
            all_discord_ids.add(entry.discord_id)
        for entry in leaderboard.biggest_gamblers:
            all_discord_ids.add(entry.discord_id)

        # Batch fetch bankruptcy states ONCE (replaces up to 80 individual calls)
        bankruptcy_states = {}
        if self.bankruptcy_service and all_discord_ids:
            bankruptcy_states = self.bankruptcy_service.get_bulk_states(list(all_discord_ids))

        # Helper to get username with tombstone if bankrupt
        def get_name(discord_id: int) -> str:
            member = guild_members.get(discord_id)
            if member:
                name = member.display_name
            else:
                name = f"User {discord_id}"

            # Use pre-fetched bankruptcy state instead of individual DB call
            state = bankruptcy_states.get(discord_id)
            if state and state.penalty_games_remaining > 0:
                from utils.formatting import TOMBSTONE_EMOJI
                name = f"{TOMBSTONE_EMOJI} {name}"

            return name

        # Top earners
        if leaderboard.top_earners:
            lines = []
            for i, entry in enumerate(leaderboard.top_earners, 1):
                name = get_name(entry.discord_id)
                pnl = entry.net_pnl
                pnl_str = f"+{pnl}" if pnl >= 0 else str(pnl)
                lines.append(f"{i}. **{name}** {pnl_str} {JOPACOIN_EMOTE} ({entry.win_rate:.0%})")
            embed.add_field(
                name="💰 Top Earners",
                value="\n".join(lines),
                inline=False,
            )

        # Down bad (only show if negative)
        down_bad = [e for e in leaderboard.down_bad if e.net_pnl < 0]
        if down_bad:
            lines = []
            for i, entry in enumerate(down_bad[:limit], 1):
                name = get_name(entry.discord_id)
                lines.append(f"{i}. **{name}** {entry.net_pnl} {JOPACOIN_EMOTE} ({entry.win_rate:.0%})")
            embed.add_field(
                name="📉 Down Bad",
                value="\n".join(lines),
                inline=False,
            )

        # Hall of Degen (highest degen scores)
        if leaderboard.hall_of_degen:
            lines = []
            for i, entry in enumerate(leaderboard.hall_of_degen, 1):
                name = get_name(entry.discord_id)
                lines.append(f"{i}. **{name}** {entry.degen_score} {entry.degen_emoji} {entry.degen_title}")
            embed.add_field(
                name="🎰 Hall of Degen",
                value="\n".join(lines),
                inline=False,
            )

        # Biggest gamblers (sorted by total wagered)
        if leaderboard.biggest_gamblers:
            lines = []
            for i, entry in enumerate(leaderboard.biggest_gamblers, 1):
                name = get_name(entry.discord_id)
                lines.append(f"{i}. **{name}** {entry.total_wagered}{JOPACOIN_EMOTE} wagered")
            embed.add_field(
                name="🎰 Biggest Gamblers",
                value="\n".join(lines),
                inline=False,
            )

        # Server stats footer (compact single line)
        if leaderboard.server_stats:
            embed.set_footer(
                text=f"📊 {leaderboard.server_stats['total_bets']} bets • "
                f"{leaderboard.server_stats['total_wagered']}{JOPACOIN_EMOTE} wagered • "
                f"{leaderboard.server_stats['unique_gamblers']} players • "
                f"{leaderboard.server_stats['avg_bet_size']}{JOPACOIN_EMOTE} avg • "
                f"{leaderboard.server_stats['total_bankruptcies']} bankruptcies"
            )

        await safe_followup(
            interaction,
            embed=embed,
            allowed_mentions=discord.AllowedMentions(users=True),
        )

    async def _show_predictions_leaderboard(self, interaction: discord.Interaction, limit: int):
        """Show the predictions leaderboard."""
        if not self.prediction_service:
            await safe_followup(
                interaction,
                content="Prediction service is not available.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        limit = max(1, min(limit, 20))  # Clamp between 1 and 20

        leaderboard = self.prediction_service.prediction_repo.get_prediction_leaderboard(
            guild_id, limit
        )

        if not leaderboard["top_earners"] and not leaderboard["most_accurate"]:
            await safe_followup(
                interaction,
                content="No prediction data yet! Users need at least 2 resolved predictions to appear.",
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title="🔮 PREDICTION LEADERBOARD",
            color=0xFFD700,  # Gold
        )

        async def get_name(discord_id: int) -> str:
            try:
                member = interaction.guild.get_member(discord_id) if interaction.guild else None
                if member:
                    return member.display_name
                fetched = await self.bot.fetch_user(discord_id)
                return fetched.display_name if fetched else f"User {discord_id}"
            except Exception:
                return f"User {discord_id}"

        # Top earners
        if leaderboard["top_earners"]:
            lines = []
            for i, entry in enumerate(leaderboard["top_earners"], 1):
                name = await get_name(entry["discord_id"])
                pnl = entry["net_pnl"]
                pnl_str = f"+{pnl}" if pnl >= 0 else str(pnl)
                lines.append(f"{i}. **{name}** {pnl_str} {JOPACOIN_EMOTE} ({entry['win_rate']:.0%})")
            embed.add_field(
                name="💰 Top Earners",
                value="\n".join(lines),
                inline=False,
            )

        # Down bad (only show if negative)
        down_bad = [e for e in leaderboard["down_bad"] if e["net_pnl"] < 0]
        if down_bad:
            lines = []
            for i, entry in enumerate(down_bad[:limit], 1):
                name = await get_name(entry["discord_id"])
                lines.append(f"{i}. **{name}** {entry['net_pnl']} {JOPACOIN_EMOTE} ({entry['win_rate']:.0%})")
            embed.add_field(
                name="📉 Down Bad",
                value="\n".join(lines),
                inline=False,
            )

        # Most accurate
        if leaderboard["most_accurate"]:
            lines = []
            for i, entry in enumerate(leaderboard["most_accurate"], 1):
                name = await get_name(entry["discord_id"])
                lines.append(f"{i}. **{name}** {entry['win_rate']:.0%} ({entry['wins']}W-{entry['losses']}L)")
            embed.add_field(
                name="🎯 Most Accurate",
                value="\n".join(lines),
                inline=False,
            )

        # Server stats
        server_stats = self.prediction_service.prediction_repo.get_server_prediction_stats(guild_id)
        if server_stats["total_predictions"]:
            embed.set_footer(
                text=f"📊 {server_stats['total_predictions']} predictions • "
                f"{server_stats['total_bets'] or 0} bets • "
                f"{server_stats['total_wagered'] or 0} wagered"
            )

        await safe_followup(
            interaction,
            embed=embed,
            allowed_mentions=discord.AllowedMentions(users=True),
        )

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
