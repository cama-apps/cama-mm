"""
Main Discord bot entry for Cama Balanced Shuffle.
"""

import asyncio
import datetime as _dt
import logging
import os
import time

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
    DB_PATH,
    GARNISHMENT_PERCENTAGE,
    LEVERAGE_TIERS,
    LLM_API_KEY,
    LOBBY_MAX_PLAYERS,
    LOBBY_RALLY_COOLDOWN_SECONDS,
    LOBBY_READY_COOLDOWN_SECONDS,
    LOBBY_READY_THRESHOLD,
    MAX_DEBT,
    USE_GLICKO,
)
from infrastructure.service_container import ServiceContainer
from services.monitoring_service import MonitoringService, UsageMonitor, set_global_usage_monitor
from utils.formatting import FROGLING_EMOJI_ID, FROGLING_EMOTE, JOPACOIN_EMOJI_ID, JOPACOIN_EMOTE
from utils.thread_safety import ensure_thread_writable

# Bot setup

intents = discord.Intents.default()
intents.message_content = True
intents.members = True
intents.presences = True

bot = commands.Bot(command_prefix="!", intents=intents)
usage_monitor = UsageMonitor()
set_global_usage_monitor(usage_monitor)

# Lazy-initialized service container
_container: ServiceContainer | None = None

# Lobby rally notification cooldowns
# Key: (guild_id, needed_count) -> timestamp
# Allows independent cooldowns for +2 and +1 notifications
_lobby_rally_cooldowns: dict[tuple[int, int], float] = {}

# Lobby ready notification cooldowns
# Key: guild_id -> timestamp. Guarded by ``_lobby_ready_lock`` so that
# concurrent reaction handlers cannot both pass the cooldown check and
# double-post the "lobby ready" message.
_lobby_ready_cooldowns: dict[int, float] = {}
_lobby_ready_lock = asyncio.Lock()

# Retained startup-recovery task handles.  These prevent overlapping sweeps
# when Discord dispatches on_ready repeatedly during a reconnect.
_reminder_recovery_task: asyncio.Task | None = None
_rebellion_recovery_task: asyncio.Task | None = None
_rebellion_recovery_complete = False

# Prediction market background task handles
_prediction_refresh_task: asyncio.Task | None = None
_prediction_digest_task: asyncio.Task | None = None
_manashop_debt_task: asyncio.Task | None = None


def _log_task_exit(name: str):
    """Done-callback factory: surface any unexpected task exit to the log.

    A graceful shutdown raises ``CancelledError`` and is silent. Anything
    else gets a traceback so we can never lose a task to silent failure.
    """

    def _cb(task: asyncio.Task) -> None:
        try:
            task.result()
        except asyncio.CancelledError:
            return
        except Exception:  # noqa: BLE001
            logger.exception("background task %s exited unexpectedly", name)

    return _cb

def _reminder_recovery_done(task: asyncio.Task) -> None:
    """Observe a reminder sweep and release its retained handle."""
    global _reminder_recovery_task

    try:
        task.result()
    except asyncio.CancelledError:
        pass
    except Exception as exc:  # noqa: BLE001
        logger.warning("Reminder recovery sweep failed: %s", exc, exc_info=True)
    finally:
        if _reminder_recovery_task is task:
            _reminder_recovery_task = None


def _start_reminder_recovery(reminder_svc, guild_ids: list[int]) -> asyncio.Task:
    """Start one reminder sweep, or return the currently retained sweep."""
    global _reminder_recovery_task

    if _reminder_recovery_task is not None:
        return _reminder_recovery_task

    guild_snapshot = list(guild_ids)
    task = asyncio.create_task(reminder_svc.reschedule_all(bot, guild_snapshot))
    _reminder_recovery_task = task
    task.add_done_callback(_reminder_recovery_done)
    return task


def _rebellion_recovery_done(task: asyncio.Task) -> None:
    """Observe a rebellion sweep, retaining permanent completion on success."""
    global _rebellion_recovery_complete, _rebellion_recovery_task

    try:
        recovered = task.result()
    except asyncio.CancelledError:
        pass
    except Exception as exc:  # noqa: BLE001
        logger.warning("Wheel war recovery sweep failed: %s", exc, exc_info=True)
    else:
        if _rebellion_recovery_task is task:
            _rebellion_recovery_complete = True
        if recovered:
            logger.info(
                "Recovered %d abandoned wheel war(s): %s",
                len(recovered),
                [result["war_id"] for result in recovered],
            )
    finally:
        if _rebellion_recovery_task is task:
            _rebellion_recovery_task = None


def _start_rebellion_recovery(rebellion_svc) -> asyncio.Task | None:
    """Start the all-guild rebellion sweep unless running or already successful."""
    global _rebellion_recovery_task

    if _rebellion_recovery_complete:
        return None
    if _rebellion_recovery_task is not None:
        return _rebellion_recovery_task

    task = asyncio.create_task(asyncio.to_thread(rebellion_svc.recover_stale_wars))
    _rebellion_recovery_task = task
    task.add_done_callback(_rebellion_recovery_done)
    return task


async def _supervised_loop(name: str, body) -> None:
    """Run a long-lived background coroutine; restart it with backoff on crash.

    ``body`` is a no-arg coroutine function. Each call should be the loop
    itself (so a single ``await body()`` lasts for the lifetime of the bot).
    A clean return ends the supervisor; an exception is logged and ``body``
    is invoked again after a backoff (5s, doubling, capped at 300s).
    Cancellation propagates so shutdown is clean.
    """
    backoff = 5
    while not bot.is_closed():
        try:
            await body()
            return
        except asyncio.CancelledError:
            raise
        except Exception:  # noqa: BLE001
            logger.exception(
                "background task %s crashed; restarting in %ds", name, backoff
            )
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 300)


async def _prediction_refresh_loop() -> None:
    """Per-market refresh worker.

    Wakes every ``PREDICTION_REFRESH_WAKE_SECONDS`` and processes any open
    market whose ``last_refresh_at`` is older than ``PREDICTION_REFRESH_SECONDS``.
    Refresh = drift price + repost ladder. If there were trades since the last
    refresh, also posts a daily-summary message in the market thread.
    """
    from config import PREDICTION_REFRESH_WAKE_SECONDS

    await bot.wait_until_ready()
    logger.info("prediction refresh loop started (wake=%ss)", PREDICTION_REFRESH_WAKE_SECONDS)
    while not bot.is_closed():
        try:
            now_ts = int(time.time())
            due = await asyncio.to_thread(
                bot.prediction_service.get_markets_due_for_refresh, now_ts
            )
            logger.info("refresh wake: %d markets due", len(due))
            for market in due:
                try:
                    await _process_one_refresh(market)
                except Exception as ex:
                    logger.exception("refresh failed for market %s: %s", market.get("prediction_id"), ex)
        except Exception as ex:
            logger.exception("prediction refresh outer loop error: %s", ex)
        await asyncio.sleep(PREDICTION_REFRESH_WAKE_SECONDS)


async def _process_one_refresh(market: dict) -> None:
    pid = market["prediction_id"]
    summary = await asyncio.to_thread(bot.prediction_service.refresh_market, pid)
    if summary.get("skipped"):
        logger.info("refresh skipped pid=%s reason=%s", pid, summary.get("reason"))
        return

    trade_summary = summary.get("trade_summary") or {}
    trade_count = int(trade_summary.get("trade_count") or 0)
    logger.info(
        "refresh done pid=%s %s->%s trades=%d",
        pid,
        summary.get("old_price"),
        summary.get("new_price"),
        trade_count,
    )

    cog = bot.get_cog("PredictionCommands")
    if cog is not None:
        await cog.refresh_market_embed(pid)

    if trade_count <= 0:
        return  # quiet day; no thread spam

    thread_id = market.get("thread_id")
    if not thread_id:
        return
    # A concurrent /predict resolve or cancel may have settled the market
    # between refresh_market and here — don't post into (or revive) the
    # thread of a market that is no longer open.
    pred = await asyncio.to_thread(
        bot.prediction_service.prediction_repo.get_prediction, pid
    )
    if not pred or pred.get("status") != "open":
        return
    try:
        thread = bot.get_channel(thread_id) or await bot.fetch_channel(thread_id)
        # Sends auto-unarchive an unlocked thread, but reviving explicitly
        # also covers locked threads and re-widens the archive window.
        await ensure_thread_writable(thread)
        biggest = trade_summary.get("biggest_trade")
        biggest_str = ""
        if biggest:
            verb = "bought" if biggest["action"].startswith("buy") else "sold"
            side = "YES" if biggest["action"].endswith("yes") else "NO"
            avg = (int(biggest["vwap_x100"]) + 50) // 100
            biggest_str = (
                f"\n  biggest: <@{biggest['discord_id']}> {verb} {biggest['contracts']} {side} @ {avg}"
            )
        msg = (
            f"📈 **Daily refresh** — volume {trade_summary.get('total_volume', 0)} contracts "
            f"({trade_summary.get('yes_volume', 0)} YES / {trade_summary.get('no_volume', 0)} NO)"
            f", YES {summary['old_price']}% → {summary['new_price']}%{biggest_str}"
        )
        await thread.send(msg)
    except Exception as ex:
        logger.warning("failed to post daily summary for market %s: %s", pid, ex)


def _next_digest_run(now, anchor_hours):
    """Return the next UTC datetime at one of ``anchor_hours`` strictly after ``now``.

    ``anchor_hours`` is a list of UTC hours (0-23); the digest fires at each one
    every day. Picks the soonest upcoming anchor, rolling into tomorrow when all
    of today's anchors have passed.
    """
    candidates = []
    for day_offset in (0, 1):
        base = (now + _dt.timedelta(days=day_offset)).replace(
            minute=0, second=0, microsecond=0
        )
        for hour in anchor_hours:
            candidates.append(base.replace(hour=hour))
    return min(c for c in candidates if c > now)


async def _prediction_digest_loop() -> None:
    """Twice-a-day digest of open markets, posted to each guild's gamba channel.

    Fires at ``PREDICTION_DIGEST_HOUR_UTC`` and 12h opposite it (every 12 hours).
    """
    from config import PREDICTION_DIGEST_HOUR_UTC

    anchor_hours = sorted(
        {PREDICTION_DIGEST_HOUR_UTC % 24, (PREDICTION_DIGEST_HOUR_UTC + 12) % 24}
    )
    await bot.wait_until_ready()
    logger.info("prediction digest loop started (anchor_hours_utc=%s)", anchor_hours)
    while not bot.is_closed():
        try:
            now = _dt.datetime.now(_dt.UTC)
            target = _next_digest_run(now, anchor_hours)
            wait_s = max(60.0, (target - now).total_seconds())
            await asyncio.sleep(wait_s)
            logger.info("digest firing for %d guilds", len(bot.guilds))
            await _post_daily_digest_all_guilds()
        except Exception as ex:
            logger.exception("digest outer loop error: %s", ex)
            await asyncio.sleep(60)


async def _manashop_debt_loop() -> None:
    await bot.wait_until_ready()
    logger.info("manashop debt loop started")
    while not bot.is_closed():
        try:
            buff_service = getattr(bot, "buff_service", None)
            player_repo = getattr(bot, "player_repo", None)
            bankruptcy_repo = getattr(bot, "bankruptcy_repo", None)
            if buff_service is not None and player_repo is not None and bankruptcy_repo is not None:
                settled = await asyncio.to_thread(
                    buff_service.settle_due_dark_bargains,
                    player_repo=player_repo,
                    bankruptcy_repo=bankruptcy_repo,
                )
                if settled:
                    logger.info("settled %d due Dark Bargain debt(s): %s", len(settled), settled)
        except Exception as ex:
            logger.exception("manashop debt loop error: %s", ex)
        await asyncio.sleep(3600)


async def _post_daily_digest_all_guilds() -> None:
    from commands.predictions import (
        _delta_phrase,
        _format_market_field,
        _pick_biggest_mover,
    )

    cog = bot.get_cog("PredictionCommands")
    if cog is None:
        logger.warning("digest skipped: PredictionCommands cog not loaded")
        return
    for guild in bot.guilds:
        try:
            opens = await asyncio.to_thread(
                bot.prediction_service.list_open_orderbook_markets, guild.id
            )
            if not opens:
                logger.info("digest guild=%s skipped: no open markets", guild.id)
                continue
            opens.sort(key=lambda p: p.get("volume_recent", 0) or 0, reverse=True)

            embed = discord.Embed(
                title="📈 Today in prediction markets",
                color=0x3498DB,
            )
            banner_lines = []
            split_banner = await asyncio.to_thread(
                bot.prediction_service.prediction_repo.pop_one_shot_flag,
                guild.id,
                "split_announced",
            )
            if split_banner:
                banner_lines.append(
                    "**Markets stock-split 10:1** — quantities have been restated; "
                    "jopa balances unchanged."
                )

            # Spotlight the market with the biggest price-point swing since its
            # last refresh (~1 day) with its fair-history chart.
            chart_file = None
            biggest = _pick_biggest_mover(opens)
            if biggest is not None:
                chart_file = await cog.render_market_chart_file(biggest)
                if chart_file is not None:
                    cur = biggest["current_price"]
                    prev = biggest["prev_price"]
                    banner_lines.append(
                        f"📊 **Biggest mover:** #{biggest['prediction_id']} "
                        f"{_delta_phrase(cur, prev)} ({prev}% → {cur}%)"
                    )
                    embed.set_image(url=f"attachment://{chart_file.filename}")

            if banner_lines:
                embed.description = "\n\n".join(banner_lines)

            FIELD_CAP = 25
            for added, p in enumerate(opens):
                if added >= FIELD_CAP:
                    embed.set_footer(
                        text=f"+{len(opens) - added} more — use /predict list"
                    )
                    break
                name, value = _format_market_field(p, with_delta=True)
                embed.add_field(name=name, value=value, inline=False)
            await cog.announce_to_gamba(guild, embed=embed, file=chart_file)
            logger.info("digest guild=%s posted %d markets", guild.id, min(len(opens), FIELD_CAP))
        except Exception as ex:
            logger.warning("digest failed for guild %s: %s", guild.id, ex)


def _init_services():
    """Initialize all services via ServiceContainer (lazy, idempotent)."""
    global _container

    if _container is not None:
        return

    container = ServiceContainer(
        db_path=DB_PATH,
        admin_user_ids=ADMIN_USER_IDS,
        lobby_ready_threshold=LOBBY_READY_THRESHOLD,
        lobby_max_players=LOBBY_MAX_PLAYERS,
        use_glicko=USE_GLICKO,
        max_debt=MAX_DEBT,
        leverage_tiers=LEVERAGE_TIERS,
        garnishment_percentage=GARNISHMENT_PERCENTAGE,
        llm_api_key=LLM_API_KEY,
        ai_model=AI_MODEL,
        ai_timeout_seconds=AI_TIMEOUT_SECONDS,
        ai_max_tokens=AI_MAX_TOKENS,
    )
    container.initialize()
    monitoring_service = MonitoringService(DB_PATH, usage_monitor=usage_monitor)
    container.expose_to_bot(bot)
    bot.monitoring_service = monitoring_service
    _container = container



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
    "commands.tax",
    "commands.ask",
    "commands.profile",
    "commands.draft",
    "commands.rating_analysis",
    "commands.herogrid",
    "commands.scout",
    "commands.wrapped",
    "commands.trivia",
    "commands.mana",
    "commands.dig",
    "commands.mafia",
    "commands.reminders",
]


async def _load_extensions():
    """Load command extensions if not already loaded."""
    # Ensure services are initialized before loading extensions
    _init_services()

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






async def update_lobby_message(message, lobby, guild_id=None):
    """Refresh lobby embed on the pinned lobby message (also updates thread since msg is thread starter)."""
    _init_services()  # Ensure services are initialized
    try:
        embed = await asyncio.to_thread(bot.lobby_service.build_lobby_embed, lobby, guild_id)
        if embed:
            await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
            logger.info(f"Updated lobby embed: {lobby.get_player_count()} players")
    except Exception as exc:
        logger.error(f"Error updating lobby message: {exc}", exc_info=True)


async def notify_lobby_ready(channel, guild_id: int = 0):
    """Notify that lobby is ready to shuffle."""
    async with _lobby_ready_lock:
        now = time.time()
        last_sent = _lobby_ready_cooldowns.get(guild_id, 0)
        if now - last_sent < LOBBY_READY_COOLDOWN_SECONDS:
            return
        # Claim the cooldown slot immediately so a concurrent handler firing
        # during the await-chain below can't also pass the check.
        _lobby_ready_cooldowns[guild_id] = now
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
        lobby_message_id = None
        lobby_channel_id = None
        if bot.lobby_service:
            lobby_message_id, lobby_channel_id = await asyncio.gather(
                asyncio.to_thread(bot.lobby_service.get_lobby_message_id, guild_id=guild_id),
                asyncio.to_thread(bot.lobby_service.get_lobby_channel_id, guild_id=guild_id),
            )
        if lobby_message_id and lobby_channel_id:
            jump_guild_id = channel.guild.id if channel.guild else guild_id
            jump_url = f"https://discord.com/channels/{jump_guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Use origin channel if available (where /lobby was run), otherwise fallback to reaction channel
        origin_channel_id = (
            await asyncio.to_thread(bot.lobby_service.get_origin_channel_id, guild_id=guild_id)
            if bot.lobby_service
            else None
        )
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
        # Send failed — release the cooldown slot we claimed so a retry can fire.
        _lobby_ready_cooldowns.pop(guild_id, None)
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
        lobby_message_id = None
        lobby_channel_id = None
        if bot.lobby_service:
            lobby_message_id, lobby_channel_id = await asyncio.gather(
                asyncio.to_thread(bot.lobby_service.get_lobby_message_id, guild_id=guild_id),
                asyncio.to_thread(bot.lobby_service.get_lobby_channel_id, guild_id=guild_id),
            )
        if lobby_message_id and lobby_channel_id:
            jump_url = f"https://discord.com/channels/{guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Use origin channel if available (where /lobby was run), otherwise fallback to reaction channel
        origin_channel_id = (
            await asyncio.to_thread(bot.lobby_service.get_origin_channel_id, guild_id=guild_id)
            if bot.lobby_service
            else None
        )
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
    """Clear lobby rally and ready cooldowns for a guild. Called on /resetlobby and shuffle."""
    keys_to_remove = [k for k in _lobby_rally_cooldowns if k[0] == guild_id]
    for key in keys_to_remove:
        del _lobby_rally_cooldowns[key]
    _lobby_ready_cooldowns.pop(guild_id, None)


@bot.event
async def setup_hook():
    """Load command cogs."""
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

    # Warm trivia image cache in background. Use bot.loop.create_task with a
    # done-callback so a failure inside warm_cache surfaces in logs instead of
    # being silently swallowed (consistent with the prediction tasks below).
    try:
        from services.trivia_image_cache import warm_cache

        def _log_warm_cache_failure(t: asyncio.Task) -> None:
            try:
                t.result()
            except Exception as exc:  # noqa: BLE001
                logger.warning("Trivia image cache warm failed: %s", exc, exc_info=True)

        warm_task = bot.loop.create_task(asyncio.to_thread(warm_cache))
        warm_task.add_done_callback(_log_warm_cache_failure)
    except Exception as exc:
        logger.debug(f"Trivia image cache warm failed to schedule: {exc}")

    # Backfill inferred server regions for players not yet checked. One-shot, off
    # the event loop, and rate-limiter-bounded; only NULL rows are processed so it
    # converges to a no-op and self-heals future gaps (e.g. manual-MMR signups).
    region_service = getattr(bot, "player_service", None)
    if region_service:
        def _log_region_backfill(t: asyncio.Task) -> None:
            try:
                count = t.result()
                if count:
                    logger.info("Inferred-region backfill updated %d player(s)", count)
            except Exception as exc:  # noqa: BLE001
                logger.warning("Inferred-region backfill failed: %s", exc, exc_info=True)

        region_task = bot.loop.create_task(
            asyncio.to_thread(region_service.backfill_inferred_regions)
        )
        region_task.add_done_callback(_log_region_backfill)

    # Start prediction-market background tasks (refresh worker + daily digest).
    # Both are wrapped in a supervisor that auto-restarts the body on a
    # crash, and a done-callback that surfaces an unexpected exit to the log
    # so we can never lose a feature to silent failure.
    global _prediction_refresh_task, _prediction_digest_task, _manashop_debt_task
    if _prediction_refresh_task is None or _prediction_refresh_task.done():
        _prediction_refresh_task = bot.loop.create_task(
            _supervised_loop("prediction_refresh", _prediction_refresh_loop)
        )
        _prediction_refresh_task.add_done_callback(_log_task_exit("prediction_refresh"))
    if _prediction_digest_task is None or _prediction_digest_task.done():
        _prediction_digest_task = bot.loop.create_task(
            _supervised_loop("prediction_digest", _prediction_digest_loop)
        )
        _prediction_digest_task.add_done_callback(_log_task_exit("prediction_digest"))
    if _manashop_debt_task is None or _manashop_debt_task.done():
        _manashop_debt_task = bot.loop.create_task(
            _supervised_loop("manashop_debt", _manashop_debt_loop)
        )
        _manashop_debt_task.add_done_callback(_log_task_exit("manashop_debt"))

    reminder_svc = getattr(bot, "reminder_service", None)
    if reminder_svc:
        guild_ids = [guild.id for guild in bot.guilds]
        _start_reminder_recovery(reminder_svc, guild_ids)

    # Recover wheel wars abandoned by a crash/restart mid-window. The whole
    # /incite lifecycle runs in an in-memory task, so a restart would leave the
    # war active forever — burning defender + meta-bet stakes and blocking every
    # future /incite in that guild. This one-shot, idempotent sweep refunds the
    # stakes and fizzles each stale war.
    #
    # on_ready fires on every gateway reconnect. Retain the running task so
    # reconnects cannot overlap the all-guild sweep; only a successful sweep is
    # permanent for this process, while a failed sweep is retried on a later
    # ready event.
    rebellion_svc = getattr(bot, "rebellion_service", None)
    if rebellion_svc:
        _start_rebellion_recovery(rebellion_svc)


@bot.tree.error
async def on_app_command_error(interaction: discord.Interaction, error: discord.app_commands.AppCommandError):
    """Global error handler for app commands - prevents infinite 'thinking...' state."""
    usage_monitor.record_command_failure()
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


@bot.event
async def on_interaction(interaction: discord.Interaction):
    """Track slash command usage for health reporting."""
    if interaction.type != discord.InteractionType.application_command:
        return
    command = interaction.command
    usage_monitor.record_command(getattr(command, "qualified_name", None) or getattr(command, "name", None))


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


def _should_force_regular_join_for_conditional_click(
    lobby,
    user_id: int,
    ready_threshold: int = LOBBY_READY_THRESHOLD,
) -> bool:
    """Return True when a frogling click should fill a regular lobby slot."""
    if not lobby:
        return False

    players = getattr(lobby, "players", set())
    conditional_players = getattr(lobby, "conditional_players", set())
    already_in_lobby = user_id in players or user_id in conditional_players
    projected_total = lobby.get_total_count() if already_in_lobby else lobby.get_total_count() + 1
    return projected_total <= ready_threshold


@bot.event
async def on_raw_reaction_add(payload):
    """Handle reaction adds for lobby joining, readycheck confirmations, and gamba notifications."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    # Handle readycheck ✅ reactions
    if payload.emoji.name == "✅":
        _init_services()
        payload_guild_id = payload.guild_id
        rc_msg_id = await asyncio.to_thread(
            bot.lobby_service.get_readycheck_message_id, guild_id=payload_guild_id
        )
        if rc_msg_id and payload.message_id == rc_msg_id:
            try:
                added = await asyncio.to_thread(
                    bot.lobby_service.add_readycheck_reaction,
                    payload.user_id,
                    f"<@{payload.user_id}>",
                    guild_id=payload_guild_id,
                )
                if added:
                    cog = bot.get_cog("LobbyCommands")
                    embed = (
                        await asyncio.to_thread(cog.rebuild_readycheck_embed, guild_id=payload_guild_id)
                        if cog
                        else None
                    )
                    if embed:
                        channel = bot.get_channel(payload.channel_id)
                        if not channel:
                            channel = await bot.fetch_channel(payload.channel_id)
                        message = await channel.fetch_message(payload.message_id)
                        await message.edit(embed=embed)
            except Exception as exc:
                logger.error(f"Error handling readycheck reaction: {exc}", exc_info=True)
        return

    # Handle 🔔 readycheck-shortcut reactions on the lobby embed
    if payload.emoji.name == "🔔":
        _init_services()
        lobby_message_id = await asyncio.to_thread(
            bot.lobby_service.get_lobby_message_id, guild_id=payload.guild_id
        )
        if payload.message_id != lobby_message_id:
            return
        cog = bot.get_cog("LobbyCommands")
        if not cog:
            return
        guild = bot.get_guild(payload.guild_id) if payload.guild_id else None
        if not guild:
            return
        try:
            status, _info = await cog._execute_readycheck(guild, payload.guild_id, payload.user_id)
        except Exception as exc:
            logger.error(f"Error running 🔔 readycheck shortcut: {exc}", exc_info=True)
            status = "error"
        if status != "ok":
            # Visible feedback that nothing happened — remove only this user's reaction
            try:
                channel = bot.get_channel(payload.channel_id)
                if not channel:
                    channel = await bot.fetch_channel(payload.channel_id)
                message = await channel.fetch_message(payload.message_id)
                user = await bot.fetch_user(payload.user_id)
                await message.remove_reaction("🔔", user)
            except Exception:
                pass
        return

    is_sword = _is_sword_emoji(payload.emoji)
    is_frogling = _is_frogling_emoji(payload.emoji)
    is_jopacoin = _is_jopacoin_emoji(payload.emoji)

    if not is_sword and not is_frogling and not is_jopacoin:
        return

    _init_services()  # Ensure services are initialized
    try:
        payload_guild_id = payload.guild_id
        channel = bot.get_channel(payload.channel_id)
        if not channel:
            channel = await bot.fetch_channel(payload.channel_id)

        message = await channel.fetch_message(payload.message_id)
        lobby_message_id = await asyncio.to_thread(
            bot.lobby_service.get_lobby_message_id, guild_id=payload_guild_id
        )
        if message.id != lobby_message_id:
            return

        lobby = await asyncio.to_thread(bot.lobby_service.get_lobby, guild_id=payload_guild_id)
        if not lobby or lobby.status != "open":
            return

        user = await bot.fetch_user(payload.user_id)

        # Handle jopacoin reaction for gamba notifications
        if is_jopacoin:
            # Only ping if user is NOT already in the lobby (regular or conditional)
            already_in_lobby = payload.user_id in lobby.players or payload.user_id in lobby.conditional_players
            if not already_in_lobby:
                thread_id = await asyncio.to_thread(
                    bot.lobby_service.get_lobby_thread_id, guild_id=payload_guild_id
                )
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
                    f"{user.mention} ❌ You're not registered! Use `/player register` first to join the lobby.",
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
                    f"{user.mention} ❌ Set your preferred roles first! Use `/player roles` (e.g., `/player roles 123`).",
                    delete_after=10,
                )
            except Exception:
                pass
            return

        force_regular_from_frogling = is_frogling and _should_force_regular_join_for_conditional_click(
            lobby,
            payload.user_id,
            getattr(bot.lobby_service, "ready_threshold", LOBBY_READY_THRESHOLD),
        )

        # Handle mutual exclusivity: join first (atomically moves between sets),
        # then remove the old reaction. This order prevents on_raw_reaction_remove
        # from seeing the player still in the old set and posting a spurious leave.
        post_join_activity = True
        if is_sword or force_regular_from_frogling:
            success, reason, pending_info = await asyncio.to_thread(
                bot.lobby_service.join_lobby, payload.user_id, guild_id
            )
            join_type = "regular"
            if (
                not success
                and force_regular_from_frogling
                and reason == "already_joined"
                and payload.user_id in lobby.players
            ):
                success = True
                reason = ""
                post_join_activity = False
            if success:
                # Remove frogling after join so the reaction_remove handler finds nothing to leave
                try:
                    frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                    await message.remove_reaction(frogling_emoji, user)
                except Exception:
                    pass
        else:
            success, reason, pending_info = await asyncio.to_thread(
                bot.lobby_service.join_lobby_conditional, payload.user_id, guild_id
            )
            join_type = "conditional"
            if success:
                # Remove sword after join so the reaction_remove handler finds nothing to leave
                try:
                    await message.remove_reaction("⚔️", user)
                except Exception:
                    pass

        if not success:
            try:
                await message.remove_reaction(payload.emoji, user)
            except Exception:
                pass
            if reason == "in_pending_match" and pending_info:
                try:
                    pending_match_id = pending_info.pending_match_id
                    jump_url = pending_info.shuffle_message_jump_url
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

        # Re-fetch the lobby after the join lands so the embed update and
        # readycheck see the post-join roster. The earlier fetch (line ~801)
        # is from before the join, so under concurrent reactions it can lag.
        lobby = await asyncio.to_thread(bot.lobby_service.get_lobby, guild_id=payload_guild_id)
        if not lobby:
            return

        if not post_join_activity:
            return

        await update_lobby_message(message, lobby, payload.guild_id)

        # Mention user in thread to subscribe them
        thread_id = await asyncio.to_thread(
            bot.lobby_service.get_lobby_thread_id, guild_id=payload_guild_id
        )
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
        if not await asyncio.to_thread(bot.lobby_service.is_ready, lobby):
            guild_id = payload.guild_id or 0
            await notify_lobby_rally(channel, thread, lobby, guild_id)
        else:
            await notify_lobby_ready(channel, guild_id=payload.guild_id or 0)
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
        payload_guild_id = payload.guild_id
        rc_msg_id = await asyncio.to_thread(
            bot.lobby_service.get_readycheck_message_id, guild_id=payload_guild_id
        )
        if rc_msg_id and payload.message_id == rc_msg_id:
            try:
                removed = await asyncio.to_thread(
                    bot.lobby_service.remove_readycheck_reaction,
                    payload.user_id,
                    guild_id=payload_guild_id,
                )
                if removed:
                    cog = bot.get_cog("LobbyCommands")
                    embed = (
                        await asyncio.to_thread(cog.rebuild_readycheck_embed, guild_id=payload_guild_id)
                        if cog
                        else None
                    )
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
        payload_guild_id = payload.guild_id
        channel = bot.get_channel(payload.channel_id)
        if not channel:
            channel = await bot.fetch_channel(payload.channel_id)
        message = await channel.fetch_message(payload.message_id)
        lobby_message_id = await asyncio.to_thread(
            bot.lobby_service.get_lobby_message_id, guild_id=payload_guild_id
        )
        if message.id != lobby_message_id:
            return

        lobby = await asyncio.to_thread(bot.lobby_service.get_lobby, guild_id=payload_guild_id)
        if not lobby or lobby.status != "open":
            return

        # Remove from appropriate set based on which emoji was removed
        if is_sword:
            left = await asyncio.to_thread(
                bot.lobby_service.leave_lobby, payload.user_id, payload_guild_id
            )
        else:
            left = await asyncio.to_thread(
                bot.lobby_service.leave_lobby_conditional, payload.user_id, payload_guild_id
            )

        if left:
            await update_lobby_message(message, lobby, payload.guild_id)

            # Post leave message in thread
            thread_id = await asyncio.to_thread(
                bot.lobby_service.get_lobby_thread_id, guild_id=payload_guild_id
            )
            if thread_id:
                try:
                    thread = bot.get_channel(thread_id)
                    if not thread:
                        thread = await bot.fetch_channel(thread_id)
                    guild = bot.get_guild(payload.guild_id)
                    member = guild.get_member(payload.user_id) if guild else None
                    if not member and guild:
                        try:
                            member = await guild.fetch_member(payload.user_id)
                        except discord.NotFound:
                            member = None
                    if member:
                        display = member.display_name
                    else:
                        user = bot.get_user(payload.user_id)
                        if not user:
                            user = await bot.fetch_user(payload.user_id)
                        display = user.display_name
                    await thread.send(f"🚪 {display} left the lobby.")
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
