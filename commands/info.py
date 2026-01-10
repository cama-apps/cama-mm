"""
Information commands for the bot: /help, /leaderboard
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from config import LEVERAGE_TIERS
from rating_system import CamaRatingSystem
from services.permissions import has_admin_permission
from utils.debug_logging import debug_log as _dbg_log
from utils.drawing import draw_rating_distribution
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.rating_insights import compute_calibration_stats, rd_to_certainty

logger = logging.getLogger("cama_bot.commands.info")


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
        guild_config_repo=None,
    ):
        self.bot = bot
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.role_emojis = role_emojis
        self.role_names = role_names
        self.flavor_text_service = flavor_text_service
        self.guild_config_repo = guild_config_repo

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
                "`/stats` - View your personal statistics\n"
                "`/pairwise` - View pairwise stats (best/worst teammates & matchups)\n"
                "`/matchup` - Head-to-head comparison between two players"
            ),
            inline=False,
        )

        # Dota 2 Stats
        embed.add_field(
            name="📊 Dota 2 Stats (OpenDota)",
            value=(
                "`/dotastats` - Comprehensive stats (W/L, KDA, heroes, distributions)\n"
                "`/matchhistory` - Recent matches with heroes and stats\n"
                "`/viewmatch` - View detailed match embed\n"
                "`/recent` - Recent matches as image table\n"
                "`/rolesgraph` - Hero role distribution radar graph\n"
                "`/lanegraph` - Lane distribution bar chart"
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

        # Gamba Stats
        embed.add_field(
            name="📈 Gamba Stats",
            value=(
                "`/gambastats` - View gambling stats and Degen Score™\n"
                "`/gambachart` - View P&L history as a chart\n"
                "`/gambaleaderboard` - Server gambling rankings & Hall of Degen"
            ),
            inline=False,
        )

        # Leaderboard
        embed.add_field(
            name="🏆 Leaderboard",
            value=("`/leaderboard` - View leaderboard sorted by jopacoin\n"
                   "`/calibration` - Rating system health & calibration stats"),
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

    @app_commands.command(name="leaderboard", description="View leaderboard sorted by jopacoin")
    @app_commands.describe(limit="Number of players to show (default: 20, max: 100)")
    async def leaderboard(self, interaction: discord.Interaction, limit: int = 20):
        """Show leaderboard of all players sorted by jopacoin balance."""
        logger.info(f"Leaderboard command: User {interaction.user.id} ({interaction.user})")
        logger.info("LEADERBOARD V2: Using jopacoin sorting and display")
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

        try:
            rating_system = CamaRatingSystem()

            all_players = self.player_repo.get_all()
            logger.info(f"Leaderboard query returned {len(all_players)} players")
            # Log sample jopacoin values
            if all_players:
                sample = all_players[:3]
                for player in sample:
                    logger.info(f"  Sample: {player.name} - jopacoin={player.jopacoin_balance}")
            _dbg_log(
                "H2",
                "commands/info.py:leaderboard:query",
                "query rows",
                {
                    "row_count": len(all_players),
                    "samples": [
                        {
                            "id": int(p.discord_id) if p.discord_id else 0,
                            "name": p.name,
                            "jopacoin": int(p.jopacoin_balance),
                            "wins": int(p.wins),
                            "losses": int(p.losses),
                        }
                        for p in all_players[:3]
                    ],
                },
                run_id="run1",
            )

            if not all_players:
                await safe_followup(
                    interaction,
                    content="No players registered yet!",
                    ephemeral=True,
                )
                return

            # Build unique player stats from player objects
            players_by_id = {}
            for player in all_players:
                discord_id = player.discord_id
                if discord_id is None:
                    continue
                if discord_id in players_by_id:
                    logger.warning("Duplicate player entry found for discord_id=%s", discord_id)
                    continue
                players_by_id[discord_id] = player

            players_with_stats = []
            for discord_id, player in players_by_id.items():
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

            players_with_stats.sort(
                key=lambda x: (
                    x["jopacoin_balance"],
                    x["wins"],
                    x["rating"] if x["rating"] is not None else 0,
                ),
                reverse=True,
            )

            # Log top 3 players after sorting
            logger.info("Top 3 players after jopacoin sort:")
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

            embed = discord.Embed(title="🏆 Leaderboard", color=discord.Color.gold())

            leaderboard_text = ""
            for i, entry in enumerate(players_with_stats[:limit], 1):
                medal = "🥇" if i == 1 else "🥈" if i == 2 else "🥉" if i == 3 else f"{i}."
                stats = f"{entry['wins']}-{entry['losses']}"
                if entry["wins"] + entry["losses"] > 0:
                    stats += f" ({entry['win_rate']:.0f}%)"
                rating_display = f" [{entry['rating']}]" if entry["rating"] is not None else ""
                is_real_user = entry["discord_id"] and entry["discord_id"] > 0
                display_name = f"<@{entry['discord_id']}>" if is_real_user else entry["username"]
                jopacoin_balance = entry.get("jopacoin_balance", 0) or 0
                jopacoin_display = f"{jopacoin_balance} {JOPACOIN_EMOTE}"
                line = (
                    f"{medal} **{display_name}** - {jopacoin_display} - {stats}{rating_display}\n"
                )
                leaderboard_text += line
                # Log first entry to verify format
                if i == 1:
                    logger.info(f"First leaderboard line format: {line.strip()}")
                    _dbg_log(
                        "H5",
                        "commands/info.py:leaderboard:first_line",
                        "first line formatted",
                        {"line": line.strip()},
                    )

            embed.description = leaderboard_text
            displayed_count = len(players_with_stats[:limit])
            logger.info(f"Leaderboard embed created with {displayed_count} entries (limit={limit})")
            _dbg_log(
                "H6",
                "commands/info.py:leaderboard:embed",
                "embed ready",
                {
                    "entries": displayed_count,
                    "limit": limit,
                    "first_line": leaderboard_text.splitlines()[0]
                    if leaderboard_text.splitlines()
                    else "",
                },
            )

            if len(players_with_stats) > limit:
                shown = min(limit, len(players_with_stats))
                embed.set_footer(text=f"Showing top {shown} of {len(players_with_stats)} players")

            # Add Wall of Shame section for players with negative balances
            debtors = [p for p in players_with_stats if p["jopacoin_balance"] < 0]
            if debtors:
                # Sort by most debt (most negative first)
                debtors.sort(key=lambda x: x["jopacoin_balance"])
                shame_text = ""
                for i, debtor in enumerate(debtors[:10], 1):  # Cap at 10 debtors
                    is_real_user = debtor["discord_id"] and debtor["discord_id"] > 0
                    display_name = (
                        f"<@{debtor['discord_id']}>" if is_real_user else debtor["username"]
                    )
                    shame_text += (
                        f"{i}. {display_name} - {debtor['jopacoin_balance']} {JOPACOIN_EMOTE}\n"
                    )

                embed.add_field(
                    name="Wall of Shame",
                    value=shame_text,
                    inline=False,
                )

            # AI insight for leaderboard - disabled
            # guild_id = guild.id if guild else None
            # if self.flavor_text_service and players_with_stats:
            #     try:
            #         leaderboard_data = {
            #             "top_players": [
            #                 {"name": p["username"], "balance": p["jopacoin_balance"], "win_rate": p["win_rate"]}
            #                 for p in players_with_stats[:5]
            #             ],
            #             "bottom_players": [
            #                 {"name": p["username"], "balance": p["jopacoin_balance"]}
            #                 for p in debtors[:3]
            #             ] if debtors else [],
            #             "total_players": len(players_with_stats),
            #         }
            #         ai_insight = await self.flavor_text_service.generate_data_insight(
            #             guild_id=guild_id,
            #             data_type="leaderboard",
            #             data=leaderboard_data,
            #         )
            #         if ai_insight:
            #             embed.add_field(name="💬 AI Insight", value=ai_insight, inline=False)
            #     except Exception as e:
            #         logger.warning(f"Failed to generate AI insight for leaderboard: {e}")

            await safe_followup(
                interaction,
                embed=embed,
                allowed_mentions=discord.AllowedMentions(users=True),
            )

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
        name="calibration", description="View rating system stats (server-wide or individual)"
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

        # Recent matches (last 5)
        recent_matches = []
        for h in matches_with_predictions[:5]:
            prob = h.get("expected_team_win_prob", 0.5)
            won = h.get("won")
            expected_win = prob >= 0.5
            if won:
                emoji = "✅" if expected_win else "🔥"  # expected win or upset
            else:
                emoji = "💀" if expected_win else "❌"  # choke (favored) or expected loss
            recent_matches.append(f"{emoji} {prob:.0%} → {'W' if won else 'L'}")

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
        uncertainty = rating_system.get_rating_uncertainty_percentage(rd)
        percentile_text = f"Top {100 - percentile:.0f}%" if percentile else "N/A"

        profile_text = (
            f"**Rating:** {rating_display} ({uncertainty:.1f}% uncertainty)\n"
            f"**Tier:** {calibration_tier} | **Percentile:** {percentile_text}\n"
            f"**Volatility:** {player.glicko_volatility:.3f}" if player.glicko_volatility else f"**Rating:** {rating_display} ({uncertainty:.1f}% uncertainty)\n**Tier:** {calibration_tier} | **Percentile:** {percentile_text}"
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

        # Recent matches
        if recent_matches:
            embed.add_field(
                name=f"🕐 Recent ({len(recent_matches)} games)",
                value="\n".join(recent_matches),
                inline=True,
            )

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

        # Record
        record_text = f"**W-L:** {player.wins}-{player.losses}"
        if player.wins + player.losses > 0:
            record_text += f" ({player.wins / (player.wins + player.losses):.0%})"
        embed.add_field(name="📋 Record", value=record_text, inline=True)

        embed.set_footer(text="✅=expected W | 🔥=upset W | ❌=expected L | 💀=choke L")

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
    guild_config_repo = getattr(bot, "guild_config_repo", None)

    await bot.add_cog(
        InfoCommands(
            bot,
            player_repo,
            match_repo,
            role_emojis,
            role_names,
            flavor_text_service=flavor_text_service,
            guild_config_repo=guild_config_repo,
        )
    )
