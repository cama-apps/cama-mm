"""
Main Discord bot entry for Cama Balanced Shuffle.
"""

import asyncio
import logging
import os

from utils.debug_logging import debug_log as _debug_log

# Configure logging BEFORE importing discord to prevent duplicate handlers
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
    force=True,  # Override any existing handlers (e.g., from discord.py)
)
logger = logging.getLogger("cama_bot")


# Suppress PyNaCl warning since voice support isn't needed
class _PyNaClFilter(logging.Filter):
    """Filter out the PyNaCl warning from discord.py."""

    def filter(self, record):
        return "PyNaCl is not installed" not in record.getMessage()


# Apply filter to discord.client logger to suppress PyNaCl warning
logging.getLogger("discord.client").addFilter(_PyNaClFilter())

# Now import discord after logging is configured
import discord
from discord.ext import commands

# Remove any handlers discord.py added to prevent duplicate output
# discord.py adds its own handler to the 'discord' logger on import
_discord_logger = logging.getLogger("discord")
_discord_logger.handlers.clear()  # Remove discord.py's default handler
_discord_logger.setLevel(logging.INFO)  # Ensure it logs at INFO level

from config import (
    ADMIN_USER_IDS,
    AI_MODEL,
    AI_TIMEOUT_SECONDS,
    AI_MAX_TOKENS,
    CEREBRAS_API_KEY,
    DB_PATH,
    GARNISHMENT_PERCENTAGE,
    LEVERAGE_TIERS,
    LOBBY_MAX_PLAYERS,
    LOBBY_READY_THRESHOLD,
    MAX_DEBT,
    PLAYER_STAKE_ENABLED,
    PLAYER_STAKE_PER_PLAYER,
    PLAYER_STAKE_POOL_SIZE,
    SPECTATOR_POOL_PLAYER_CUT,
    STAKE_WIN_PROB_MAX,
    STAKE_WIN_PROB_MIN,
    USE_GLICKO,
)
from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from repositories.bet_repository import BetRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.bankruptcy_service import BankruptcyRepository, BankruptcyService
from services.betting_service import BettingService
from services.gambling_stats_service import GamblingStatsService
from services.garnishment_service import GarnishmentService
from services.disburse_service import DisburseService
from services.loan_service import LoanRepository, LoanService
from services.lobby_service import LobbyService
from services.recalibration_service import RecalibrationService
from repositories.recalibration_repository import RecalibrationRepository
from repositories.disburse_repository import DisburseRepository
from repositories.prediction_repository import PredictionRepository
from repositories.stake_repository import StakeRepository
from repositories.spectator_bet_repository import SpectatorBetRepository
from repositories.player_pool_bet_repository import PlayerPoolBetRepository
from services.match_service import MatchService
from services.spectator_pool_service import SpectatorPoolConfig, SpectatorPoolService
from services.prediction_service import PredictionService
from services.stake_service import StakePoolConfig, StakeService
from services.permissions import has_admin_permission  # noqa: F401 - used by tests
from services.player_service import PlayerService
from services.opendota_player_service import OpenDotaPlayerService
from utils.formatting import FROGLING_EMOJI_ID, FROGLING_EMOTE, ROLE_EMOJIS, ROLE_NAMES, format_role_display

# Bot setup

intents = discord.Intents.default()
intents.message_content = True
intents.members = True

bot = commands.Bot(command_prefix="!", intents=intents)

# Lazy-initialized services (created on first access to avoid blocking test collection)
_services_initialized = False
db = None
lobby_manager = None
player_service = None
lobby_service = None
player_repo = None
bet_repo = None
betting_service = None
match_service = None


def _init_services():
    """Initialize database and services lazily (on first use, not at import time)."""
    global _services_initialized, db, lobby_manager, player_service, lobby_service
    global player_repo, bet_repo, betting_service, match_service

    # region agent log
    _debug_log(
        "H2",
        "bot.py:_init_services",
        "entering _init_services",
        {"initialized": _services_initialized},
    )
    # endregion agent log

    if _services_initialized:
        return

    db = Database(db_path=DB_PATH)
    lobby_repo = LobbyRepository(DB_PATH)
    lobby_manager = LobbyManager(lobby_repo)
    player_repo = PlayerRepository(DB_PATH)
    bet_repo = BetRepository(DB_PATH)
    match_repo = MatchRepository(DB_PATH)
    pairings_repo = PairingsRepository(DB_PATH)
    guild_config_repo = GuildConfigRepository(DB_PATH)

    # Create garnishment service for debt repayment
    garnishment_service = GarnishmentService(player_repo, GARNISHMENT_PERCENTAGE)

    # Create bankruptcy service for debt clearing with penalties
    bankruptcy_repo = BankruptcyRepository(DB_PATH)
    bankruptcy_service = BankruptcyService(bankruptcy_repo, player_repo)

    # Create loan service for borrowing jopacoin
    loan_repo = LoanRepository(DB_PATH)
    loan_service = LoanService(loan_repo, player_repo)

    # Create recalibration service for rating uncertainty reset
    recalibration_repo = RecalibrationRepository(DB_PATH)
    recalibration_service = RecalibrationService(recalibration_repo, player_repo)

    # Create disburse service for nonprofit fund distribution
    disburse_repo = DisburseRepository(DB_PATH)
    disburse_service = DisburseService(disburse_repo, player_repo, loan_repo)

    # Create betting service with garnishment and bankruptcy support
    betting_service = BettingService(
        bet_repo,
        player_repo,
        garnishment_service=garnishment_service,
        leverage_tiers=LEVERAGE_TIERS,
        max_debt=MAX_DEBT,
        bankruptcy_service=bankruptcy_service,
    )

    player_service = PlayerService(player_repo)
    lobby_service = LobbyService(
        lobby_manager,
        player_repo,
        ready_threshold=LOBBY_READY_THRESHOLD,
        max_players=LOBBY_MAX_PLAYERS,
        bankruptcy_repo=bankruptcy_repo,
    )

    # Create match service
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=USE_GLICKO,
        betting_service=betting_service,
        pairings_repo=pairings_repo,
        loan_service=loan_service,
    )

    # Expose on bot for cogs
    bot.db = db
    bot.lobby_manager = lobby_manager
    bot.player_service = player_service
    bot.lobby_service = lobby_service
    bot.match_service = match_service
    bot.player_repo = player_repo
    bot.match_repo = match_repo
    bot.pairings_repo = pairings_repo
    bot.guild_config_repo = guild_config_repo
    bot.bankruptcy_repo = bankruptcy_repo
    bot.role_emojis = ROLE_EMOJIS
    bot.role_names = ROLE_NAMES
    bot.format_role_display = format_role_display
    bot.ADMIN_USER_IDS = ADMIN_USER_IDS
    bot.betting_service = betting_service
    bot.bankruptcy_service = bankruptcy_service
    bot.loan_service = loan_service
    bot.disburse_service = disburse_service
    bot.recalibration_service = recalibration_service

    # Create gambling stats service for degen score and leaderboards
    gambling_stats_service = GamblingStatsService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        match_repo=match_repo,
        bankruptcy_service=bankruptcy_service,
        loan_service=loan_service,
    )
    bot.gambling_stats_service = gambling_stats_service

    # Create prediction service for prediction markets
    prediction_repo = PredictionRepository(DB_PATH)
    prediction_service = PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repo,
        admin_user_ids=ADMIN_USER_IDS,
    )
    bot.prediction_service = prediction_service
    bot.prediction_repo = prediction_repo

    # Create OpenDota player service for profile stats
    opendota_player_service = OpenDotaPlayerService(player_repo)
    bot.opendota_player_service = opendota_player_service

    # Create stake service for draft mode player pool
    stake_repo = StakeRepository(DB_PATH)
    player_pool_bet_repo = PlayerPoolBetRepository(DB_PATH)
    stake_config = StakePoolConfig(
        pool_size=PLAYER_STAKE_POOL_SIZE,
        stake_per_player=PLAYER_STAKE_PER_PLAYER,
        enabled=PLAYER_STAKE_ENABLED,
        win_prob_min=STAKE_WIN_PROB_MIN,
        win_prob_max=STAKE_WIN_PROB_MAX,
    )
    stake_service = StakeService(
        stake_repo, player_repo, player_pool_bet_repo, stake_config
    )
    bot.stake_service = stake_service
    bot.stake_repo = stake_repo
    bot.player_pool_bet_repo = player_pool_bet_repo

    # Create spectator pool service for non-participant betting
    spectator_bet_repo = SpectatorBetRepository(DB_PATH)
    spectator_pool_config = SpectatorPoolConfig(
        enabled=True,
        player_cut=SPECTATOR_POOL_PLAYER_CUT,
    )
    spectator_pool_service = SpectatorPoolService(
        spectator_bet_repo, player_repo, spectator_pool_config
    )
    bot.spectator_pool_service = spectator_pool_service
    bot.spectator_bet_repo = spectator_bet_repo

    # Update match_service with stake and spectator services
    match_service.stake_service = stake_service
    match_service.spectator_pool_service = spectator_pool_service

    # Create AI services (optional - only if CEREBRAS_API_KEY is set)
    ai_service = None
    sql_query_service = None
    flavor_text_service = None

    if CEREBRAS_API_KEY:
        try:
            from services.ai_service import AIService
            from services.sql_query_service import SQLQueryService
            from services.flavor_text_service import FlavorTextService
            from repositories.ai_query_repository import AIQueryRepository

            ai_service = AIService(
                model=AI_MODEL,
                api_key=CEREBRAS_API_KEY,
                timeout=AI_TIMEOUT_SECONDS,
                max_tokens=AI_MAX_TOKENS,
            )

            ai_query_repo = AIQueryRepository(DB_PATH)
            sql_query_service = SQLQueryService(
                ai_service=ai_service,
                ai_query_repo=ai_query_repo,
                guild_config_repo=guild_config_repo,
            )

            flavor_text_service = FlavorTextService(
                ai_service=ai_service,
                player_repo=player_repo,
                bankruptcy_service=bankruptcy_service,
                loan_service=loan_service,
                gambling_stats_service=gambling_stats_service,
                guild_config_repo=guild_config_repo,
            )

            logger.info(f"AI services initialized with model: {AI_MODEL}")
        except Exception as e:
            logger.warning(f"Failed to initialize AI services: {e}")
            ai_service = None
            sql_query_service = None
            flavor_text_service = None
    else:
        logger.info("AI services not initialized (CEREBRAS_API_KEY not set)")

    bot.ai_service = ai_service
    bot.sql_query_service = sql_query_service
    bot.flavor_text_service = flavor_text_service

    _services_initialized = True


# Set non-database attributes on bot immediately (these are safe at import time)
bot.role_emojis = ROLE_EMOJIS
bot.role_names = ROLE_NAMES
bot.format_role_display = format_role_display
bot.ADMIN_USER_IDS = ADMIN_USER_IDS

EXTENSIONS = [
    "commands.registration",
    "commands.info",
    "commands.lobby",
    "commands.match",
    "commands.admin",
    "commands.betting",
    "commands.advstats",
    "commands.enrichment",
    "commands.dota_info",
    "commands.shop",
    "commands.predictions",
    "commands.ask",
    "commands.profile",
    "commands.draft",
]


async def _load_extensions():
    """Load command extensions if not already loaded."""
    # Ensure services are initialized before loading extensions
    _init_services()

    # region agent log
    _debug_log(
        "H1", "bot.py:_load_extensions", "starting extension load loop", {"extensions": EXTENSIONS}
    )
    # endregion agent log

    loaded_extensions = []
    skipped_extensions = []
    failed_extensions = []

    for ext in EXTENSIONS:
        if ext in bot.extensions:
            skipped_extensions.append(ext)
            logger.debug(f"Extension {ext} already loaded, skipping")
            continue
        try:
            await bot.load_extension(ext)
            loaded_extensions.append(ext)
            logger.info(f"Loaded extension: {ext}")
        except Exception as exc:
            failed_extensions.append(ext)
            logger.error(f"Failed to load extension {ext}: {exc}", exc_info=True)

    # Log summary
    logger.info(
        f"Extension loading complete: {len(loaded_extensions)} loaded, "
        f"{len(skipped_extensions)} skipped, {len(failed_extensions)} failed"
    )

    # Diagnostic: Log all registered commands
    all_commands = list(bot.tree.walk_commands())
    command_counts = {}
    for cmd in all_commands:
        command_counts[cmd.name] = command_counts.get(cmd.name, 0) + 1

    # Log duplicate commands if any
    duplicates = {name: count for name, count in command_counts.items() if count > 1}
    if duplicates:
        logger.warning(f"Found duplicate command registrations: {duplicates}")

    logger.info(
        f"Total registered commands: {len(all_commands)}. "
        f"Unique command names: {len(command_counts)}"
    )


def _ensure_extensions_loaded_for_import():
    """
    When the module is imported in tests (without running the bot),
    load extensions so command definitions exist on the command tree.
    """
    # region agent log
    _debug_log(
        "H1",
        "bot.py:_ensure_extensions_loaded_for_import",
        "called to ensure extensions loaded",
        {},
    )
    # endregion agent log
    try:
        loop = asyncio.get_event_loop()
        if loop.is_running():
            return
        loop.run_until_complete(_load_extensions())
    except RuntimeError:
        asyncio.run(_load_extensions())


def get_existing_command_names():
    """Return the set of command names currently registered on the bot."""
    # region agent log
    _debug_log("H3", "bot.py:get_existing_command_names", "function invoked", {})
    # endregion agent log
    return {command.name for command in bot.tree.walk_commands()}


async def update_lobby_message(message, lobby):
    """Refresh lobby embed on the pinned lobby message (also updates thread since msg is thread starter)."""
    _init_services()  # Ensure services are initialized
    try:
        embed = lobby_service.build_lobby_embed(lobby)
        if embed:
            await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
            logger.info(f"Updated lobby embed: {lobby.get_player_count()} players")
    except Exception as exc:
        logger.error(f"Error updating lobby message: {exc}", exc_info=True)


async def notify_lobby_ready(channel, lobby):
    """Notify that lobby is ready to shuffle."""
    try:
        embed = discord.Embed(
            title="🎮 Lobby Ready!",
            description="The lobby now has 10 players!",
            color=discord.Color.green(),
        )
        embed.add_field(
            name="Next Step",
            value="Anyone can use `/shuffle` to create balanced teams!",
            inline=False,
        )
        await channel.send(embed=embed)
    except Exception as exc:
        logger.error(f"Error notifying lobby ready: {exc}", exc_info=True)


@bot.event
async def setup_hook():
    """Load command cogs."""
    # Initialize database and services before loading extensions
    _init_services()
    await _load_extensions()


@bot.event
async def on_ready():
    """Called when bot is ready."""
    logger.info(f"{bot.user} connected. Guilds: {len(bot.guilds)}")

    # Diagnostic: Log all registered commands before sync
    all_commands = list(bot.tree.walk_commands())
    command_counts = {}
    for cmd in all_commands:
        command_counts[cmd.name] = command_counts.get(cmd.name, 0) + 1

    # Log duplicate commands if any
    duplicates = {name: count for name, count in command_counts.items() if count > 1}
    if duplicates:
        logger.warning(f"Found duplicate command registrations before sync: {duplicates}")
        # Log details for addfake specifically
        addfake_cmds = [cmd for cmd in all_commands if cmd.name == "addfake"]
        if len(addfake_cmds) > 1:
            logger.warning(
                f"Found {len(addfake_cmds)} addfake command registrations. "
                f"Details: {[{'cog': cmd.cog.__class__.__name__ if cmd.cog else None, 'qualified_name': cmd.qualified_name} for cmd in addfake_cmds]}"
            )

    logger.info(
        f"Pre-sync: {len(all_commands)} total commands, {len(command_counts)} unique names. "
        f"Loaded cogs: {list(bot.cogs.keys())}"
    )

    try:
        await bot.tree.sync()
        logger.info("Slash commands synced globally.")

        # Diagnostic: Log commands after sync
        post_sync_commands = list(bot.tree.walk_commands())
        logger.info(f"Post-sync: {len(post_sync_commands)} commands available")
    except Exception as exc:
        logger.error(f"Failed to sync commands: {exc}", exc_info=True)


@bot.tree.error
async def on_app_command_error(interaction: discord.Interaction, error: discord.app_commands.AppCommandError):
    """Global error handler for app commands - prevents infinite 'thinking...' state."""
    logger.error(f"App command error in '{interaction.command.name if interaction.command else 'unknown'}': {error}", exc_info=error)

    # Try to send error message to user
    error_msg = "An error occurred while processing your command. Please try again."

    try:
        if interaction.response.is_done():
            # Interaction was deferred, use followup
            await interaction.followup.send(content=f"❌ {error_msg}", ephemeral=True)
        else:
            # Interaction not yet responded, use response
            await interaction.response.send_message(content=f"❌ {error_msg}", ephemeral=True)
    except Exception as followup_error:
        logger.error(f"Failed to send error message to user: {followup_error}")


def _is_sword_emoji(emoji) -> bool:
    """Check if the emoji is the sword emoji for regular lobby joining."""
    return emoji.name == "⚔️"


def _is_frogling_emoji(emoji) -> bool:
    """Check if the emoji is the frogling emoji for conditional lobby joining."""
    # Custom emoji: check by ID or name
    return emoji.id == FROGLING_EMOJI_ID or emoji.name == "frogling"


@bot.event
async def on_raw_reaction_add(payload):
    """Handle reaction adds for lobby joining (⚔️ for regular, :frogling: for conditional)."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    is_sword = _is_sword_emoji(payload.emoji)
    is_frogling = _is_frogling_emoji(payload.emoji)

    if not is_sword and not is_frogling:
        return

    _init_services()  # Ensure services are initialized
    try:
        channel = bot.get_channel(payload.channel_id)
        if not channel:
            return

        message = await channel.fetch_message(payload.message_id)
        if message.id != lobby_service.get_lobby_message_id():
            return

        lobby = lobby_service.get_lobby()
        if not lobby or lobby.status != "open":
            return

        user = await bot.fetch_user(payload.user_id)
        player = player_service.get_player(payload.user_id)
        if not player:
            try:
                await message.remove_reaction(payload.emoji, user)
            except Exception:
                pass
            try:
                await channel.send(
                    f"{user.mention} ❌ You're not registered! Use `/register` first to join the lobby.",
                    delete_after=10,
                )
            except Exception:
                pass
            return

        if not player.preferred_roles:
            try:
                await message.remove_reaction(payload.emoji, user)
            except Exception:
                pass
            try:
                await channel.send(
                    f"{user.mention} ❌ Set your preferred roles first! Use `/setroles` (e.g., `/setroles 123`).",
                    delete_after=10,
                )
            except Exception:
                pass
            return

        # Handle mutual exclusivity: remove the other reaction if present
        if is_sword:
            # Joining as regular player - remove frogling if present
            try:
                frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                await message.remove_reaction(frogling_emoji, user)
            except Exception:
                pass
            success, reason = lobby_service.join_lobby(payload.user_id)
            join_type = "regular"
        else:
            # Joining as conditional (frogling) - remove sword if present
            try:
                await message.remove_reaction("⚔️", user)
            except Exception:
                pass
            success, reason = lobby_service.join_lobby_conditional(payload.user_id)
            join_type = "conditional"

        if not success:
            try:
                await message.remove_reaction(payload.emoji, user)
            except Exception:
                pass
            try:
                await channel.send(f"{user.mention} ❌ {reason}", delete_after=10)
            except Exception:
                pass
            return

        await update_lobby_message(message, lobby)

        # Mention user in thread to subscribe them
        thread_id = lobby_service.get_lobby_thread_id()
        if thread_id:
            try:
                thread = bot.get_channel(thread_id)
                if not thread:
                    thread = await bot.fetch_channel(thread_id)
                if join_type == "conditional":
                    await thread.send(f"{FROGLING_EMOTE} {user.mention} joined as conditional!")
                else:
                    await thread.send(f"✅ {user.mention} joined the lobby!")
            except Exception as exc:
                logger.warning(f"Failed to post join activity in thread: {exc}")

        if lobby_service.is_ready(lobby):
            await notify_lobby_ready(channel, lobby)
    except Exception as exc:
        logger.error(f"Error handling reaction add: {exc}", exc_info=True)


@bot.event
async def on_raw_reaction_remove(payload):
    """Handle reaction removes for lobby leaving (⚔️ for regular, :frogling: for conditional)."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    is_sword = _is_sword_emoji(payload.emoji)
    is_frogling = _is_frogling_emoji(payload.emoji)

    if not is_sword and not is_frogling:
        return

    _init_services()  # Ensure services are initialized
    try:
        channel = bot.get_channel(payload.channel_id)
        if not channel:
            return
        message = await channel.fetch_message(payload.message_id)
        if message.id != lobby_service.get_lobby_message_id():
            return

        lobby = lobby_service.get_lobby()
        if not lobby or lobby.status != "open":
            return

        # Remove from appropriate set based on which emoji was removed
        if is_sword:
            left = lobby_service.leave_lobby(payload.user_id)
        else:
            left = lobby_service.leave_lobby_conditional(payload.user_id)

        if left:
            await update_lobby_message(message, lobby)

            # Post leave message in thread
            thread_id = lobby_service.get_lobby_thread_id()
            if thread_id:
                try:
                    thread = bot.get_channel(thread_id)
                    if not thread:
                        thread = await bot.fetch_channel(thread_id)
                    user = bot.get_user(payload.user_id)
                    if not user:
                        user = await bot.fetch_user(payload.user_id)
                    await thread.send(f"🚪 {user.display_name} left the lobby.")
                except Exception as exc:
                    logger.warning(f"Failed to post leave activity in thread: {exc}")
    except Exception as exc:
        logger.error(f"Error handling reaction remove: {exc}", exc_info=True)


def main():
    """Run the bot."""
    token = os.getenv("DISCORD_BOT_TOKEN")
    if not token:
        from dotenv import load_dotenv

        load_dotenv()
        token = os.getenv("DISCORD_BOT_TOKEN")

    if not token:
        print("ERROR: DISCORD_BOT_TOKEN not found!")
        return

    try:
        # Pass log_handler=None to prevent discord.py from adding its own handler
        # We've already configured logging above with our preferred format
        bot.run(token, log_handler=None)
    except KeyboardInterrupt:
        logger.info("Bot stopped by user (Ctrl+C)")
        print("\nBot stopped. Goodbye!")
    except Exception as exc:
        logger.error(f"Bot crashed: {exc}", exc_info=True)
        print(f"\nBot crashed: {exc}")


if __name__ == "__main__":
    main()
