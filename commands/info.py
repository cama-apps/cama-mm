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
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.rating_insights import compute_calibration_stats

logger = logging.getLogger("cama_bot.commands.info")


class InfoCommands(commands.Cog):
    """Commands for viewing information and leaderboards."""

    def __init__(
        self, bot: commands.Bot, player_repo, match_repo, role_emojis: dict, role_names: dict
    ):
        self.bot = bot
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.role_emojis = role_emojis
        self.role_names = role_names

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
                "React with ⚔️ to join/leave the lobby"
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
        name="calibration", description="View server rating system health and calibration stats"
    )
    async def calibration(self, interaction: discord.Interaction):
        """Show rating system health and calibration stats."""
        logger.info(f"Calibration command: User {interaction.user.id} ({interaction.user})")
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

            stats = compute_calibration_stats(
                players=players,
                rating_system=rating_system,
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
            avg_uncertainty_text = (
                f"{stats['avg_uncertainty']:.1f}%"
                if stats["avg_uncertainty"] is not None
                else "n/a"
            )
            calibration_progress = (
                f"Locked In (≤75): {rd_tiers['Locked In']} | Settling (76-150): {rd_tiers['Settling']}\n"
                f"Developing (151-250): {rd_tiers['Developing']} | Fresh (251+): {rd_tiers['Fresh']}\n"
                f"Avg Uncertainty: {avg_uncertainty_text}"
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
                prediction_text = (
                    f"Matches Analyzed: {prediction_quality['count']}\n"
                    f"Brier: {brier:.3f} ({brier_quality}) | Pick Accuracy: {prediction_quality['accuracy']:.0%}\n"
                    f"Balance Rate (45-55%): {prediction_quality['balance_rate']:.0%} | Upset Rate (60%+): {upset_rate}"
                )
            else:
                prediction_text = "No prediction data yet."

            rating_movement = stats["rating_movement"]
            if rating_movement["count"]:
                movement_text = (
                    f"Entries: {rating_movement['count']} | Avg Δ: {rating_movement['avg_delta']:.1f}\n"
                    f"Median Δ: {rating_movement['median_delta']:.1f}"
                )
            else:
                movement_text = "No rating history yet."

            if stats["avg_drift"] is not None and stats["median_drift"] is not None:
                drift_text = (
                    f"Avg Drift: {stats['avg_drift']:+.0f} | Median Drift: {stats['median_drift']:+.0f}\n"
                    f"📈 Biggest Gainers: {format_drift(stats['biggest_gainers'])}\n"
                    f"📉 Biggest Drops: {format_drift(stats['biggest_drops'])}"
                )
            else:
                drift_text = "No seed MMR data yet."

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
            embed.add_field(name="Calibration Progress", value=calibration_progress, inline=False)
            embed.add_field(name="Prediction Quality", value=prediction_text, inline=False)
            embed.add_field(name="Rating Movement", value=movement_text, inline=False)
            embed.add_field(name="Rating Drift", value=drift_text, inline=False)

            embed.add_field(
                name="Highest Rated",
                value=format_ranked(
                    stats["top_rated"],
                    lambda p: rating_system.rating_to_display(p.glicko_rating or 0),
                    "{:.0f}",
                ),
                inline=True,
            )
            embed.add_field(
                name="Most Calibrated",
                value=format_ranked(
                    stats["most_calibrated"],
                    lambda p: rating_system.get_rating_uncertainty_percentage(
                        p.glicko_rd if p.glicko_rd is not None else 350
                    ),
                    "{:.1f}%",
                ),
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
                value=format_ranked(
                    stats["least_calibrated"],
                    lambda p: rating_system.get_rating_uncertainty_percentage(
                        p.glicko_rd if p.glicko_rd is not None else 350
                    ),
                    "{:.1f}%",
                ),
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

            embed.set_footer(text="RD = Rating Deviation | Drift = Current - Seed | Brier: 0=perfect, 0.25=coin flip")

            await safe_followup(
                interaction,
                embed=embed,
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


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    # Get player_repo and config from bot
    player_repo = getattr(bot, "player_repo", None)
    match_repo = getattr(bot, "match_repo", None)
    role_emojis = getattr(bot, "role_emojis", {})
    role_names = getattr(bot, "role_names", {})

    await bot.add_cog(InfoCommands(bot, player_repo, match_repo, role_emojis, role_names))
