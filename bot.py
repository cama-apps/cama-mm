"""
Main Discord bot entry for Cama Balanced Shuffle.
"""

import asyncio
import atexit
import logging
import os

from utils.debug_logging import debug_log as _debug_log

try:
    import msvcrt  # Windows-only
except Exception:  # pragma: no cover
    msvcrt = None

try:
    import fcntl  # Unix-only
except Exception:  # pragma: no cover
    fcntl = None

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
    DB_PATH,
    GARNISHMENT_PERCENTAGE,
    LEVERAGE_TIERS,
    LOBBY_MAX_PLAYERS,
    LOBBY_READY_THRESHOLD,
    MAX_DEBT,
    USE_GLICKO,
)
from database import Database
from domain.models.lobby import LobbyManager
from repositories.bet_repository import BetRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.bankruptcy_service import BankruptcyRepository, BankruptcyService
from services.betting_service import BettingService
from services.garnishment_service import GarnishmentService
from services.lobby_service import LobbyService
from services.match_service import MatchService
from services.permissions import has_admin_permission  # noqa: F401 - used by tests
from services.player_service import PlayerService
from utils.formatting import ROLE_EMOJIS, ROLE_NAMES, format_role_display

# Bot setup
_instance_lock_handle = None
_instance_lock_method = None
_instance_lock_path = os.path.join(os.path.dirname(__file__), ".bot.lock")


def _pid_is_running(pid: int) -> bool:
    """Return True if a process with the given PID is running."""
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    else:
        return True


def _cleanup_stale_lock(lock_path: str) -> None:
    """Remove the lock file if it belongs to a dead process."""
    try:
        if not os.path.exists(lock_path):
            return
        with open(lock_path, encoding="utf-8") as lock_file:
            content = lock_file.read().strip()
        try:
            pid = int(content) if content else -1
        except ValueError:
            pid = -1
        if pid == -1 or not _pid_is_running(pid):
            os.remove(lock_path)
    except Exception:
        # Swallow cleanup errors; we'll attempt locking regardless
        pass


def _acquire_single_instance_lock() -> bool:
    """
    Prevent multiple bot.py processes from running at the same time.
    This avoids duplicated slash command handling (multiple processes receive the same interaction).
    """
    global _instance_lock_handle, _instance_lock_method
    try:
        _cleanup_stale_lock(_instance_lock_path)
        _instance_lock_handle = open(_instance_lock_path, "a+", encoding="utf-8")
        if msvcrt is not None:
            msvcrt.locking(_instance_lock_handle.fileno(), msvcrt.LK_NBLCK, 1)
            _instance_lock_method = "msvcrt"
        elif fcntl is not None:
            fcntl.flock(_instance_lock_handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            _instance_lock_method = "fcntl"
        else:  # pragma: no cover
            logger.warning(
                "No platform lock mechanism available; continuing without single-instance protection."
            )
            return True

        _instance_lock_handle.seek(0)
        _instance_lock_handle.truncate()
        _instance_lock_handle.write(str(os.getpid()))
        _instance_lock_handle.flush()
        try:
            os.fsync(_instance_lock_handle.fileno())
        except Exception:
            pass

        # region agent log
        _debug_log(
            "H1",
            "bot.py:_acquire_single_instance_lock",
            "single-instance lock acquired",
            {"pid": os.getpid(), "lock_path": _instance_lock_path},
            run_id="pre-fix",
        )
        # endregion agent log
        return True
    except Exception as exc:
        # region agent log
        _debug_log(
            "H1",
            "bot.py:_acquire_single_instance_lock",
            "failed to acquire single-instance lock",
            {"pid": os.getpid(), "error": str(exc)},
            run_id="pre-fix",
        )
        # endregion agent log
        try:
            if _instance_lock_handle:
                _instance_lock_handle.close()
        except Exception:
            pass
        _instance_lock_handle = None
        _instance_lock_method = None
        logger.error(
            "Another bot instance appears to be running. "
            "Stop other 'python bot.py' processes/terminals and retry."
        )
        return False


def _release_single_instance_lock() -> None:
    """Release and remove the single-instance lock file."""
    global _instance_lock_handle, _instance_lock_method
    try:
        if _instance_lock_handle:
            try:
                if _instance_lock_method == "msvcrt" and msvcrt is not None:
                    msvcrt.locking(_instance_lock_handle.fileno(), msvcrt.LK_UNLCK, 1)
                elif _instance_lock_method == "fcntl" and fcntl is not None:
                    fcntl.flock(_instance_lock_handle, fcntl.LOCK_UN)
            except Exception:
                pass
            try:
                _instance_lock_handle.close()
            except Exception:
                pass
        _instance_lock_handle = None
        _instance_lock_method = None
        try:
            if os.path.exists(_instance_lock_path):
                os.remove(_instance_lock_path)
        except Exception:
            pass
    except Exception:
        pass


atexit.register(_release_single_instance_lock)

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
    )

    # Create match service
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=USE_GLICKO,
        betting_service=betting_service,
        pairings_repo=pairings_repo,
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
    bot.role_emojis = ROLE_EMOJIS
    bot.role_names = ROLE_NAMES
    bot.format_role_display = format_role_display
    bot.ADMIN_USER_IDS = ADMIN_USER_IDS
    bot.betting_service = betting_service
    bot.bankruptcy_service = bankruptcy_service

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
    """Refresh lobby embed on the pinned lobby message."""
    _init_services()  # Ensure services are initialized
    try:
        embed = lobby_service.build_lobby_embed(lobby)
        if embed:
            await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
            if lobby.get_player_count() < lobby_service.ready_threshold:
                try:
                    await message.add_reaction("⚔️")
                except Exception:
                    pass
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
    if not _acquire_single_instance_lock():
        raise SystemExit(2)
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


@bot.event
async def on_raw_reaction_add(payload):
    """Handle reaction adds for lobby joining."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    if payload.emoji.name != "⚔️":
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

        success, reason = lobby_service.join_lobby(payload.user_id)
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
        if lobby_service.is_ready(lobby):
            await notify_lobby_ready(channel, lobby)
    except Exception as exc:
        logger.error(f"Error handling reaction add: {exc}", exc_info=True)


@bot.event
async def on_raw_reaction_remove(payload):
    """Handle reaction removes for lobby leaving."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    if payload.emoji.name != "⚔️":
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

        if lobby_service.leave_lobby(payload.user_id):
            await update_lobby_message(message, lobby)
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
