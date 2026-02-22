"""
Main Discord bot entry for Cama Balanced Shuffle.
"""

import asyncio
import logging
import os
import time

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
from discord.app_commands.errors import TransformerError
from discord.ext import commands

# Remove any handlers discord.py added to prevent duplicate output
# discord.py adds its own handler to the 'discord' logger on import
_discord_logger = logging.getLogger("discord")
_discord_logger.handlers.clear()  # Remove discord.py's default handler
_discord_logger.setLevel(logging.INFO)  # Ensure it logs at INFO level

from config import (
    ADMIN_USER_IDS,
    AI_MAX_TOKENS,
    AI_MODEL,
    AI_TIMEOUT_SECONDS,
    CEREBRAS_API_KEY,
    DB_PATH,
    GARNISHMENT_PERCENTAGE,
    LEVERAGE_TIERS,
    LOBBY_MAX_PLAYERS,
    LOBBY_RALLY_COOLDOWN_SECONDS,
    LOBBY_READY_THRESHOLD,
    MAX_DEBT,
    USE_GLICKO,
)
from infrastructure.service_container import ServiceContainer
from services.permissions import has_admin_permission  # noqa: F401 - used by tests
from utils.formatting import FROGLING_EMOJI_ID, FROGLING_EMOTE, JOPACOIN_EMOJI_ID, JOPACOIN_EMOTE

# Bot setup

intents = discord.Intents.default()
intents.message_content = True
intents.members = True
intents.presences = True

bot = commands.Bot(command_prefix="!", intents=intents)

# Lazy-initialized service container
_container: ServiceContainer | None = None

# Lobby rally notification cooldowns
# Key: (guild_id, needed_count) -> timestamp
# Allows independent cooldowns for +2 and +1 notifications
_lobby_rally_cooldowns: dict[tuple[int, int], float] = {}


def _init_services():
    """Initialize all services via ServiceContainer (lazy, idempotent)."""
    global _container

    # region agent log
    _debug_log(
        "H2",
        "bot.py:_init_services",
        "entering _init_services",
        {"initialized": _container is not None},
    )
    # endregion agent log

    if _container is not None:
        return

    _container = ServiceContainer(
        db_path=DB_PATH,
        admin_user_ids=ADMIN_USER_IDS,
        lobby_ready_threshold=LOBBY_READY_THRESHOLD,
        lobby_max_players=LOBBY_MAX_PLAYERS,
        use_glicko=USE_GLICKO,
        max_debt=MAX_DEBT,
        leverage_tiers=LEVERAGE_TIERS,
        garnishment_percentage=GARNISHMENT_PERCENTAGE,
        cerebras_api_key=CEREBRAS_API_KEY,
        ai_model=AI_MODEL,
        ai_timeout_seconds=AI_TIMEOUT_SECONDS,
        ai_max_tokens=AI_MAX_TOKENS,
    )
    _container.initialize()
    _container.expose_to_bot(bot)



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
    "commands.rating_analysis",
    "commands.herogrid",
    "commands.scout",
    "commands.wrapped",
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


async def update_lobby_message(message, lobby, guild_id=None):
    """Refresh lobby embed on the pinned lobby message (also updates thread since msg is thread starter)."""
    _init_services()  # Ensure services are initialized
    try:
        embed = bot.lobby_service.build_lobby_embed(lobby, guild_id)
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

        # Add jump link to lobby embed
        lobby_message_id = bot.lobby_service.get_lobby_message_id() if bot.lobby_service else None
        lobby_channel_id = bot.lobby_service.get_lobby_channel_id() if bot.lobby_service else None
        if lobby_message_id and lobby_channel_id:
            guild_id = channel.guild.id if channel.guild else 0
            jump_url = f"https://discord.com/channels/{guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Use origin channel if available (where /lobby was run), otherwise fallback to reaction channel
        origin_channel_id = bot.lobby_service.get_origin_channel_id() if bot.lobby_service else None
        target_channel = channel  # Default to reaction channel

        if origin_channel_id and origin_channel_id != channel.id:
            try:
                target_channel = bot.get_channel(origin_channel_id)
                if not target_channel:
                    target_channel = await bot.fetch_channel(origin_channel_id)
            except Exception as exc:
                logger.warning(f"Could not fetch origin channel {origin_channel_id}: {exc}")
                target_channel = channel  # Fallback

        await target_channel.send(embed=embed)
    except Exception as exc:
        logger.error(f"Error notifying lobby ready: {exc}", exc_info=True)


async def notify_lobby_rally(channel, thread, lobby, guild_id: int) -> bool:
    """
    Notify that lobby is almost ready. Returns True if notification was sent.
    Each threshold (+2, +1) has an independent cooldown.

    If a dedicated lobby channel is configured, rally notifications go to the
    origin channel (where /lobby was run) instead of the reaction channel.
    """
    total = lobby.get_total_count()
    needed = LOBBY_READY_THRESHOLD - total

    if needed < 1 or needed > 2:
        return False  # Only notify for +1 or +2

    now = time.time()
    cooldown_key = (guild_id, needed)
    last_sent = _lobby_rally_cooldowns.get(cooldown_key, 0)

    if now - last_sent < LOBBY_RALLY_COOLDOWN_SECONDS:
        return False  # Still in cooldown for this threshold

    try:
        embed = discord.Embed(
            title="📢 Almost Ready!",
            description=f"The lobby has **{total}** players — just **+{needed}** more needed!",
            color=discord.Color.orange(),
        )

        # Add jump link to lobby embed
        lobby_message_id = bot.lobby_service.get_lobby_message_id() if bot.lobby_service else None
        lobby_channel_id = bot.lobby_service.get_lobby_channel_id() if bot.lobby_service else None
        if lobby_message_id and lobby_channel_id:
            jump_url = f"https://discord.com/channels/{guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Use origin channel if available (where /lobby was run), otherwise fallback to reaction channel
        origin_channel_id = bot.lobby_service.get_origin_channel_id() if bot.lobby_service else None
        target_channel = channel  # Default to reaction channel

        if origin_channel_id and origin_channel_id != channel.id:
            try:
                target_channel = bot.get_channel(origin_channel_id)
                if not target_channel:
                    target_channel = await bot.fetch_channel(origin_channel_id)
            except Exception as exc:
                logger.warning(f"Could not fetch origin channel {origin_channel_id}: {exc}")
                target_channel = channel  # Fallback

        # Send to origin channel (or reaction channel as fallback)
        await target_channel.send(embed=embed)

        # Send to thread
        if thread:
            await thread.send(f"📢 **+{needed}** more player{'s' if needed > 1 else ''} needed!")

        _lobby_rally_cooldowns[cooldown_key] = now
        return True
    except Exception as exc:
        logger.error(f"Error sending rally notification: {exc}", exc_info=True)
        return False


def clear_lobby_rally_cooldowns(guild_id: int) -> None:
    """Clear lobby rally cooldowns for a guild. Called on /resetlobby and shuffle."""
    keys_to_remove = [k for k in _lobby_rally_cooldowns if k[0] == guild_id]
    for key in keys_to_remove:
        del _lobby_rally_cooldowns[key]


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

    # Handle TransformerError (e.g., typing a username instead of selecting from Discord's picker)
    if isinstance(error, TransformerError):
        value = getattr(error, 'value', None)
        error_msg = (
            f"Could not find user `{value}`. "
            "Please use @mention or select from Discord's user picker when typing."
        )
    else:
        # Generic error message
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


def _is_jopacoin_emoji(emoji) -> bool:
    """Check if the emoji is the jopacoin emoji for gamba notifications."""
    # Custom emoji: check by ID or name
    return emoji.id == JOPACOIN_EMOJI_ID or emoji.name == "jopacoin"


@bot.event
async def on_raw_reaction_add(payload):
    """Handle reaction adds for lobby joining, readycheck confirmations, and gamba notifications."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    # Handle readycheck ✅ reactions
    if payload.emoji.name == "✅":
        _init_services()
        rc_msg_id = bot.lobby_service.get_readycheck_message_id()
        if rc_msg_id and payload.message_id == rc_msg_id:
            try:
                added = bot.lobby_service.add_readycheck_reaction(
                    payload.user_id, f"<@{payload.user_id}>"
                )
                if added:
                    cog = bot.get_cog("LobbyCommands")
                    embed = cog.rebuild_readycheck_embed() if cog else None
                    if embed:
                        channel = bot.get_channel(payload.channel_id)
                        if not channel:
                            channel = await bot.fetch_channel(payload.channel_id)
                        message = await channel.fetch_message(payload.message_id)
                        await message.edit(embed=embed)
            except Exception as exc:
                logger.error(f"Error handling readycheck reaction: {exc}", exc_info=True)
        return

    is_sword = _is_sword_emoji(payload.emoji)
    is_frogling = _is_frogling_emoji(payload.emoji)
    is_jopacoin = _is_jopacoin_emoji(payload.emoji)

    if not is_sword and not is_frogling and not is_jopacoin:
        return

    _init_services()  # Ensure services are initialized
    try:
        channel = bot.get_channel(payload.channel_id)
        if not channel:
            return

        message = await channel.fetch_message(payload.message_id)
        if message.id != bot.lobby_service.get_lobby_message_id():
            return

        lobby = bot.lobby_service.get_lobby()
        if not lobby or lobby.status != "open":
            return

        user = await bot.fetch_user(payload.user_id)

        # Handle jopacoin reaction for gamba notifications
        if is_jopacoin:
            # Only ping if user is NOT already in the lobby (regular or conditional)
            already_in_lobby = payload.user_id in lobby.players or payload.user_id in lobby.conditional_players
            if not already_in_lobby:
                thread_id = bot.lobby_service.get_lobby_thread_id()
                if thread_id:
                    try:
                        thread = bot.get_channel(thread_id)
                        if not thread:
                            thread = await bot.fetch_channel(thread_id)
                        await thread.send(f"{JOPACOIN_EMOTE} {user.mention} is here for the gamba!")
                    except Exception as exc:
                        logger.warning(f"Failed to post gamba subscription in thread: {exc}")

                # Neon Degen Terminal hook (~35% chance, auto-deletes)
                try:
                    from services.neon_degen_service import NeonDegenService
                    neon = getattr(bot, "neon_degen_service", None)
                    if isinstance(neon, NeonDegenService):
                        neon_result = await neon.on_gamba_spectator(
                            payload.user_id, payload.guild_id, user.display_name
                        )
                        if neon_result and neon_result.text_block:
                            neon_msg = await channel.send(neon_result.text_block)
                            async def _delete_after(m, delay):
                                try:
                                    await asyncio.sleep(delay)
                                    await m.delete()
                                except Exception:
                                    pass
                            asyncio.create_task(_delete_after(neon_msg, 60))
                except Exception as exc:
                    logger.debug(f"Neon gamba spectator hook failed: {exc}")
            return

        # Rest of the handler is for sword/frogling (lobby joining)
        guild_id = payload.guild_id
        player = await asyncio.to_thread(bot.player_service.get_player, payload.user_id, guild_id)
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
            success, reason, pending_info = await asyncio.to_thread(
                bot.lobby_service.join_lobby, payload.user_id, guild_id
            )
            join_type = "regular"
        else:
            # Joining as conditional (frogling) - remove sword if present
            try:
                await message.remove_reaction("⚔️", user)
            except Exception:
                pass
            success, reason, pending_info = await asyncio.to_thread(
                bot.lobby_service.join_lobby_conditional, payload.user_id, guild_id
            )
            join_type = "conditional"

        if not success:
            try:
                await message.remove_reaction(payload.emoji, user)
            except Exception:
                pass
            if reason == "in_pending_match" and pending_info:
                try:
                    pending_match_id = pending_info.get("pending_match_id")
                    jump_url = pending_info.get("shuffle_message_jump_url")
                    msg = f"{user.mention} ❌ You're in a pending match (Match #{pending_match_id})!"
                    if jump_url:
                        msg += f" [View your match]({jump_url}) and use `/record` to complete it first."
                    else:
                        msg += " Use `/record` to complete it first."
                    await channel.send(msg, delete_after=15)
                except Exception:
                    pass
            else:
                reason_messages = {
                    "lobby_full": "Lobby is full.",
                    "already_joined": "Already in lobby.",
                }
                msg = reason_messages.get(reason, "Could not join lobby.")
                try:
                    await channel.send(f"{user.mention} ❌ {msg}", delete_after=10)
                except Exception:
                    pass
            return

        await update_lobby_message(message, lobby, payload.guild_id)

        # Mention user in thread to subscribe them
        thread_id = bot.lobby_service.get_lobby_thread_id()
        thread = None
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

        # Check for rally notification (+2 or +1 needed)
        if not bot.lobby_service.is_ready(lobby):
            guild_id = payload.guild_id or 0
            await notify_lobby_rally(channel, thread, lobby, guild_id)
        else:
            await notify_lobby_ready(channel, lobby)
    except Exception as exc:
        logger.error(f"Error handling reaction add: {exc}", exc_info=True)


@bot.event
async def on_raw_reaction_remove(payload):
    """Handle reaction removes for lobby leaving and readycheck un-confirms."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    # Handle readycheck ✅ un-reaction
    if payload.emoji.name == "✅":
        _init_services()
        rc_msg_id = bot.lobby_service.get_readycheck_message_id()
        if rc_msg_id and payload.message_id == rc_msg_id:
            try:
                removed = bot.lobby_service.remove_readycheck_reaction(payload.user_id)
                if removed:
                    cog = bot.get_cog("LobbyCommands")
                    embed = cog.rebuild_readycheck_embed() if cog else None
                    if embed:
                        channel = bot.get_channel(payload.channel_id)
                        if not channel:
                            channel = await bot.fetch_channel(payload.channel_id)
                        message = await channel.fetch_message(payload.message_id)
                        await message.edit(embed=embed)
            except Exception as exc:
                logger.error(f"Error handling readycheck reaction remove: {exc}", exc_info=True)
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
        if message.id != bot.lobby_service.get_lobby_message_id():
            return

        lobby = bot.lobby_service.get_lobby()
        if not lobby or lobby.status != "open":
            return

        # Remove from appropriate set based on which emoji was removed
        if is_sword:
            left = await asyncio.to_thread(bot.lobby_service.leave_lobby, payload.user_id)
        else:
            left = await asyncio.to_thread(bot.lobby_service.leave_lobby_conditional, payload.user_id)

        if left:
            await update_lobby_message(message, lobby, payload.guild_id)

            # Post leave message in thread
            thread_id = bot.lobby_service.get_lobby_thread_id()
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
