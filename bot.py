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
    DIG_LLM_ENABLED,
    ECONOMY_EVENT_TRIGGER_HOUR_LOCAL,
    ECONOMY_EVENT_WAKE_SECONDS,
    ECONOMY_EVENTS_ENABLED,
    FIRST_GAME_POOL_DAILY_AMOUNT,
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
from domain.models.lobby import LobbyKind
from infrastructure.service_container import ServiceContainer
from opendota_integration import run_opendota_io
from services import trivia_data
from services.monitoring_service import MonitoringService, UsageMonitor, set_global_usage_monitor
from utils.command_registry import (
    CHAT_INPUT_COMMAND_LIMIT,
    COMMAND_OPTION_LIMIT,
    summarize_command_tree,
)
from utils.economy_event_display import build_public_economy_event_embed
from utils.formatting import JOPACOIN_EMOJI_ID, JOPACOIN_EMOTE
from utils.game_date import get_game_date
from utils.thread_safety import ensure_thread_writable

# Bot setup

intents = discord.Intents.default()
intents.message_content = True
intents.members = True
intents.presences = True

bot = commands.Bot(command_prefix="!", intents=intents)
usage_monitor = UsageMonitor()
set_global_usage_monitor(usage_monitor)

# Kept locally for one-way cleanup of lobby messages created before the
# conditional queue was retired. It is not a supported lobby reaction.
_LEGACY_FROGLING_EMOJI_ID = 1463270458848842003
_LOBBY_RECONCILE_MAX_ATTEMPTS = 3
_LOBBY_RECONCILE_RETRY_SECONDS = 1

# Lazy-initialized service container
_container: ServiceContainer | None = None

# Lobby rally notification cooldowns
# Key: (guild_id, lobby_kind, needed_count) -> timestamp
# Allows independent cooldowns for +2 and +1 notifications
_lobby_rally_cooldowns: dict[tuple[int, LobbyKind, int], float] = {}
_lobby_rally_lock = asyncio.Lock()

# Lobby ready notification cooldowns
# Key: (guild_id, lobby_kind) -> timestamp. Guarded by ``_lobby_ready_lock`` so that
# concurrent reaction handlers cannot both pass the cooldown check and
# double-post the "lobby ready" message.
_lobby_ready_cooldowns: dict[tuple[int, LobbyKind], float] = {}
_lobby_ready_lock = asyncio.Lock()

# Strong references to fire-and-forget tasks. The event loop only holds
# tasks weakly, so an unreferenced task can be garbage-collected mid-run.
_background_tasks: set[asyncio.Task] = set()


def _normalize_lobby_kind_or_open(value) -> LobbyKind:
    """Normalize real lobby state while tolerating legacy test/service doubles."""
    try:
        return LobbyKind.normalize(value)
    except (TypeError, ValueError):
        return LobbyKind.OPEN


def _retain_background_task(task: asyncio.Task) -> asyncio.Task:
    """Hold a strong reference to a fire-and-forget task until it finishes."""
    _background_tasks.add(task)
    task.add_done_callback(_background_tasks.discard)
    return task

# Retained startup-recovery task handles.  These prevent overlapping sweeps
# when Discord dispatches on_ready repeatedly during a reconnect.
_reminder_recovery_task: asyncio.Task | None = None

# Prediction market background task handles
_prediction_refresh_task: asyncio.Task | None = None
_prediction_digest_task: asyncio.Task | None = None
_manashop_debt_task: asyncio.Task | None = None
_duel_challenge_task: asyncio.Task | None = None
_economy_event_task: asyncio.Task | None = None
_first_game_pool_task: asyncio.Task | None = None

DUEL_WORKER_WAKE_SECONDS = 60
FIRST_GAME_POOL_WAKE_SECONDS = 900


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


async def _duel_challenge_loop() -> None:
    """Claim and deliver persisted duel reminders and expirations."""
    await bot.wait_until_ready()
    logger.info("duel challenge loop started (wake=%ss)", DUEL_WORKER_WAKE_SECONDS)
    while not bot.is_closed():
        now = int(time.time())
        try:
            service = getattr(bot, "duel_service", None)
            cog = bot.get_cog("DuelCommands")
            if service is not None and cog is not None:
                due = await asyncio.to_thread(service.get_due_challenge_ids, now)
                for challenge_id, guild_id in due:
                    try:
                        await cog.process_due_challenge(challenge_id, guild_id, now)
                    except Exception:  # noqa: BLE001
                        logger.exception(
                            "duel due delivery failed challenge=%s guild=%s",
                            challenge_id,
                            guild_id,
                        )
        except Exception:  # noqa: BLE001
            logger.exception("duel challenge loop wake failed")
        await asyncio.sleep(DUEL_WORKER_WAKE_SECONDS)


async def _announce_economy_event(guild: discord.Guild, event: dict) -> bool:
    """Post a newly activated monetary event in the guild's gamba channel.

    Returns True when the announcement was handed to the gamba channel, so
    the caller can stamp the event as announced. A missing cog returns False
    and leaves the event unannounced for the next wake to retry.
    """
    cog = bot.get_cog("PredictionCommands")
    if cog is None:
        logger.warning(
            "economy event announcement skipped for guild=%s: "
            "PredictionCommands cog not loaded",
            guild.id,
        )
        return False
    icon_url = None
    event_name = event.get("name")
    if event_name:
        try:
            icon_url = await asyncio.to_thread(
                trivia_data.get_ability_icon_url_by_name,
                event_name,
            )
        except Exception:  # noqa: BLE001
            logger.warning(
                "economy event icon lookup failed for event=%s guild=%s",
                event_name,
                guild.id,
                exc_info=True,
            )
    embed = build_public_economy_event_embed(
        event,
        icon_url=icon_url,
    )
    await cog.announce_to_gamba(guild, embed=embed)
    return True


async def _economy_event_loop() -> None:
    """Activate one rolling-band economy event per day."""
    await bot.wait_until_ready()
    logger.info(
        "economy event loop started (wake=%ss trigger=%02d:00 Pacific events=%s)",
        ECONOMY_EVENT_WAKE_SECONDS,
        ECONOMY_EVENT_TRIGGER_HOUR_LOCAL,
        ECONOMY_EVENTS_ENABLED,
    )
    while not bot.is_closed():
        for guild in list(bot.guilds):
            try:
                event, _created = await asyncio.to_thread(
                    bot.economy_event_service.ensure_daily_event,
                    guild.id,
                )
                # Durable stamps make the initial post and its 12-hour
                # reminder independently retryable after delivery failures.
                announcement_slot = (
                    bot.economy_event_service.pending_announcement_slot(event)
                    if event
                    else None
                )
                if announcement_slot:
                    announced = await _announce_economy_event(guild, event)
                    if announced:
                        marker = (
                            bot.economy_event_service.mark_event_announced
                            if announcement_slot == "initial"
                            else bot.economy_event_service.mark_event_reminder_announced
                        )
                        await asyncio.to_thread(marker, guild.id, event["event_id"])
            except Exception:  # noqa: BLE001
                logger.exception("economy event wake failed for guild=%s", guild.id)
        sleep_seconds = ECONOMY_EVENT_WAKE_SECONDS
        try:
            seconds_until_trigger = await asyncio.to_thread(
                bot.economy_event_service.seconds_until_next_trigger
            )
            sleep_seconds = min(
                ECONOMY_EVENT_WAKE_SECONDS,
                max(0.0, float(seconds_until_trigger)),
            )
        except Exception:  # noqa: BLE001
            logger.exception(
                "economy event trigger scheduling failed; using configured wake interval"
            )
        await asyncio.sleep(sleep_seconds)


async def _first_game_pool_loop() -> None:
    """Fund both stacked lobby pools once per fixed-PST game-date."""
    await bot.wait_until_ready()
    logger.info(
        "first-game pool loop started (wake=%ss daily=%s JC per lobby)",
        FIRST_GAME_POOL_WAKE_SECONDS,
        FIRST_GAME_POOL_DAILY_AMOUNT,
    )
    while not bot.is_closed():
        game_date = get_game_date()
        for guild in list(bot.guilds):
            try:
                await asyncio.to_thread(
                    bot.loan_service.fund_first_game_pools,
                    guild.id,
                    game_date,
                    FIRST_GAME_POOL_DAILY_AMOUNT,
                )
            except Exception:  # noqa: BLE001
                logger.exception(
                    "first-game pool funding failed for guild=%s date=%s",
                    guild.id,
                    game_date,
                )
        await asyncio.sleep(FIRST_GAME_POOL_WAKE_SECONDS)


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
        economy_events_enabled=ECONOMY_EVENTS_ENABLED,
        llm_api_key=LLM_API_KEY,
        ai_model=AI_MODEL,
        ai_timeout_seconds=AI_TIMEOUT_SECONDS,
        ai_max_tokens=AI_MAX_TOKENS,
        dig_llm_enabled=DIG_LLM_ENABLED,
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
    "commands.duel",
    "commands.tax",
    "commands.blame_luke",
    "commands.ask",
    "commands.profile",
    "commands.draft",
    "commands.rating_analysis",
    "commands.herogrid",
    "commands.scout",
    "commands.wrapped",
    "commands.trivia",
    "commands.player_trivia",
    "commands.mana",
    "commands.dig",
    "commands.mafia",
    "commands.pet",
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

    _log_command_registration("Extension load")






async def update_lobby_message(message, lobby, guild_id=None) -> bool:
    """Refresh lobby embed on the pinned lobby message (also updates thread since msg is thread starter)."""
    _init_services()  # Ensure services are initialized
    try:
        embed = await asyncio.to_thread(bot.lobby_service.build_lobby_embed, lobby, guild_id)
        if embed:
            await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
            logger.info(f"Updated lobby embed: {lobby.get_player_count()} players")
            return True
    except Exception as exc:
        logger.error(f"Error updating lobby message: {exc}", exc_info=True)
    return False


def _snapshot_restored_lobbies(lobby_manager) -> list[tuple[tuple[int, LobbyKind], object]]:
    """Copy restored lobby entries outside the async command path."""
    return list(lobby_manager.lobbies.items())


async def _resolve_lobby_kind_for_message(
    lobby_service,
    message_id: int,
    guild_id: int | None,
    *,
    readycheck: bool = False,
) -> LobbyKind | None:
    """Resolve message ownership, with an open-lobby fallback for old service doubles."""
    resolver_name = (
        "get_lobby_kind_for_readycheck_message"
        if readycheck
        else "get_lobby_kind_for_message"
    )
    resolver = getattr(lobby_service, resolver_name, None)
    if callable(resolver):
        candidate = await asyncio.to_thread(
            resolver,
            message_id,
            guild_id=guild_id,
        )
        try:
            if candidate is not None:
                return LobbyKind.normalize(candidate)
        except (TypeError, ValueError):
            pass

    legacy_getter_name = (
        "get_readycheck_message_id" if readycheck else "get_lobby_message_id"
    )
    legacy_getter = getattr(lobby_service, legacy_getter_name, None)
    if callable(legacy_getter):
        legacy_message_id = await asyncio.to_thread(
            legacy_getter,
            guild_id=guild_id,
        )
        if legacy_message_id == message_id:
            return LobbyKind.OPEN
    return None


async def _reconcile_persisted_lobby_messages() -> None:
    """Refresh restored lobby UI and remove the retired Frogling reaction."""
    lobby_service = getattr(bot, "lobby_service", None)
    lobby_manager = getattr(lobby_service, "lobby_manager", None)
    if not lobby_service or not lobby_manager:
        return

    restored_lobbies = await asyncio.to_thread(
        _snapshot_restored_lobbies,
        lobby_manager,
    )
    pending_lobbies = []
    for key, lobby in restored_lobbies:
        if lobby.status != "open":
            continue
        if isinstance(key, tuple):
            guild_id, kind = key
        else:
            guild_id, kind = key, LobbyKind.OPEN
        pending_lobbies.append((guild_id, LobbyKind.normalize(kind), lobby))
    for attempt in range(_LOBBY_RECONCILE_MAX_ATTEMPTS):
        failed_lobbies = []
        for guild_id, kind, lobby in pending_lobbies:
            try:
                message_id, channel_id = await asyncio.gather(
                    asyncio.to_thread(
                        lobby_service.get_lobby_message_id,
                        guild_id=guild_id,
                        lobby_kind=kind,
                    ),
                    asyncio.to_thread(
                        lobby_service.get_lobby_channel_id,
                        guild_id=guild_id,
                        lobby_kind=kind,
                    ),
                )
                if not message_id or not channel_id:
                    continue

                channel = bot.get_channel(channel_id)
                if not channel:
                    channel = await bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
            except Exception as exc:
                logger.warning(
                    "Could not reconcile restored lobby message for guild %s: %s",
                    guild_id,
                    exc,
                )
                failed_lobbies.append((guild_id, kind, lobby))
                continue

            updated = await update_lobby_message(message, lobby, guild_id)
            reaction_removed = True
            legacy_reaction = next(
                (
                    reaction
                    for reaction in message.reactions
                    if getattr(reaction.emoji, "id", None)
                    == _LEGACY_FROGLING_EMOJI_ID
                ),
                None,
            )
            if legacy_reaction:
                try:
                    await message.clear_reaction(legacy_reaction.emoji)
                except Exception as exc:
                    reaction_removed = False
                    logger.warning(
                        "Could not remove retired Frogling reaction for guild %s: %s",
                        guild_id,
                        exc,
                    )

            if not updated or not reaction_removed:
                failed_lobbies.append((guild_id, kind, lobby))

        if not failed_lobbies:
            return
        pending_lobbies = failed_lobbies
        if attempt + 1 < _LOBBY_RECONCILE_MAX_ATTEMPTS:
            await asyncio.sleep(_LOBBY_RECONCILE_RETRY_SECONDS * (2**attempt))

    logger.warning(
        "Lobby message reconciliation still incomplete for %d guild(s)",
        len(pending_lobbies),
    )


async def notify_lobby_ready(
    channel,
    guild_id: int = 0,
    lobby_kind: LobbyKind | str | None = None,
):
    """Notify that lobby is ready to shuffle."""
    lobby_service = bot.lobby_service
    if not lobby_service:
        return
    kind = LobbyKind.normalize(lobby_kind)
    cooldown_key = (guild_id, kind)
    cooldown_claimed = False

    lobby, lobby_message_id, lobby_channel_id = await asyncio.gather(
        asyncio.to_thread(
            lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=kind,
        ),
        asyncio.to_thread(
            lobby_service.get_lobby_message_id,
            guild_id=guild_id,
            lobby_kind=kind,
        ),
        asyncio.to_thread(
            lobby_service.get_lobby_channel_id,
            guild_id=guild_id,
            lobby_kind=kind,
        ),
    )
    if (
        not lobby
        or lobby.status != "open"
        or lobby.get_player_count() != lobby_service.ready_threshold
        or not lobby_message_id
    ):
        return

    try:
        embed = discord.Embed(
            title=f"🎮 {kind.label} Ready!",
            description=f"{kind.display_name} now has 10 players!",
            color=discord.Color.green(),
        )
        embed.add_field(
            name="Next Step",
            value="Run `/readycheck` before `/shuffle` to confirm everyone is ready.",
            inline=False,
        )

        # Add jump link to lobby embed
        if lobby_message_id and lobby_channel_id:
            jump_guild_id = channel.guild.id if channel.guild else guild_id
            jump_url = f"https://discord.com/channels/{jump_guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Use origin channel if available (where /lobby was run), otherwise fallback to reaction channel
        origin_channel_id = (
            await asyncio.to_thread(
                lobby_service.get_origin_channel_id,
                guild_id=guild_id,
                lobby_kind=kind,
            )
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

        async with _lobby_ready_lock:
            now = time.time()
            last_sent = _lobby_ready_cooldowns.get(cooldown_key, 0)
            if now - last_sent < LOBBY_READY_COOLDOWN_SECONDS:
                return

            current_lobby, current_message_id = await asyncio.gather(
                asyncio.to_thread(
                    lobby_service.get_lobby,
                    guild_id=guild_id,
                    lobby_kind=kind,
                ),
                asyncio.to_thread(
                    lobby_service.get_lobby_message_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                ),
            )
            if (
                not current_lobby
                or current_lobby.status != "open"
                or current_lobby.get_player_count() != lobby_service.ready_threshold
                or current_message_id != lobby_message_id
            ):
                return

            # Claim the cooldown slot before sending so concurrent handlers for
            # the same lobby generation cannot both announce it.
            _lobby_ready_cooldowns[cooldown_key] = now
            cooldown_claimed = True

        await target_channel.send(embed=embed)
    except Exception as exc:
        # Send failed — release the cooldown slot we claimed so a retry can fire.
        if cooldown_claimed:
            _lobby_ready_cooldowns.pop(cooldown_key, None)
        logger.error(f"Error notifying lobby ready: {exc}", exc_info=True)


async def notify_lobby_rally(
    channel,
    thread,
    lobby,
    guild_id: int,
    lobby_kind: LobbyKind | str | None = None,
) -> bool:
    """
    Notify that lobby is almost ready. Returns True if notification was sent.
    Each threshold (+2, +1) has an independent cooldown.

    If a dedicated lobby channel is configured, rally notifications go to the
    origin channel (where /lobby was run) instead of the reaction channel.
    """
    total = lobby.get_total_count()
    needed = LOBBY_READY_THRESHOLD - total
    kind = _normalize_lobby_kind_or_open(
        lobby_kind or getattr(lobby, "kind", None)
    )

    if needed < 1 or needed > 2:
        return False  # Only notify for +1 or +2

    cooldown_key = (guild_id, kind, needed)

    # The lock covers only the check-and-claim so one slow guild's network
    # I/O cannot block rally notifications for every other guild.
    async with _lobby_rally_lock:
        now = time.time()
        last_sent = _lobby_rally_cooldowns.get(cooldown_key, 0)

        if now - last_sent < LOBBY_RALLY_COOLDOWN_SECONDS:
            return False  # Still in cooldown for this threshold

        # Claim before any Discord awaits so simultaneous joins cannot both
        # send. A failed send releases the claim below.
        _lobby_rally_cooldowns[cooldown_key] = now

    try:
        sent = await _send_lobby_rally(
            channel,
            thread,
            lobby,
            guild_id,
            total,
            needed,
            lobby_kind=kind,
        )
        if not sent:
            _lobby_rally_cooldowns.pop(cooldown_key, None)
        return sent
    except Exception as exc:
        _lobby_rally_cooldowns.pop(cooldown_key, None)
        logger.error(f"Error sending rally notification: {exc}", exc_info=True)
        return False


async def _send_lobby_rally(
    channel,
    thread,
    lobby,
    guild_id: int,
    total: int,
    needed: int,
    lobby_kind: LobbyKind | str | None = None,
) -> bool:
    """Send one claimed near-full notification and ping eligible subscribers."""
    kind = _normalize_lobby_kind_or_open(
        lobby_kind or getattr(lobby, "kind", None)
    )
    try:
        async def load_reaction_subscribers(
            lobby_message_id: int | None,
            lobby_channel_id: int | None,
        ) -> list[tuple[int, str]]:
            if not lobby_message_id or not lobby_channel_id:
                return []

            subscribers: list[tuple[int, str]] = []
            try:
                lobby_channel = (
                    channel
                    if channel.id == lobby_channel_id
                    else bot.get_channel(lobby_channel_id)
                )
                if not lobby_channel:
                    lobby_channel = await bot.fetch_channel(lobby_channel_id)
                lobby_message = await lobby_channel.fetch_message(lobby_message_id)
                excluded_ids = set(lobby.players)
                for reaction in lobby_message.reactions:
                    if str(reaction.emoji) != "📋":
                        continue
                    async for subscriber in reaction.users():
                        if not subscriber.bot and subscriber.id not in excluded_ids:
                            subscribers.append((subscriber.id, subscriber.mention))
                    break
            except Exception as exc:
                logger.warning("Could not load clipboard lobby subscribers: %s", exc)
            return subscribers

        async def load_persistent_subscriber_ids() -> list[int]:
            reminder_service = getattr(bot, "reminder_service", None)
            if reminder_service is None:
                return []
            try:
                subscriber_ids = await asyncio.to_thread(
                    reminder_service.get_lobby_subscriber_ids,
                    guild_id,
                )
                return list(subscriber_ids)
            except Exception as exc:
                logger.warning("Could not load persistent lobby subscribers: %s", exc)
                return []

        async def resolve_target_channel(origin_channel_id: int | None):
            target_channel = channel
            if origin_channel_id and origin_channel_id != channel.id:
                try:
                    target_channel = bot.get_channel(origin_channel_id)
                    if not target_channel:
                        target_channel = await bot.fetch_channel(origin_channel_id)
                except Exception as exc:
                    logger.warning(
                        "Could not fetch origin channel %s: %s",
                        origin_channel_id,
                        exc,
                    )
                    target_channel = channel
            return target_channel

        embed = discord.Embed(
            title=f"📢 {kind.label} Almost Ready!",
            description=(
                f"{kind.display_name} has **{total}** players — "
                f"just **+{needed}** more needed!"
            ),
            color=discord.Color.orange(),
        )

        lobby_message_id = None
        lobby_channel_id = None
        origin_channel_id = None
        if bot.lobby_service:
            (
                lobby_message_id,
                lobby_channel_id,
                origin_channel_id,
            ) = await asyncio.gather(
                asyncio.to_thread(
                    bot.lobby_service.get_lobby_message_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                ),
                asyncio.to_thread(
                    bot.lobby_service.get_lobby_channel_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                ),
                asyncio.to_thread(
                    bot.lobby_service.get_origin_channel_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                ),
            )

        # Add jump link to lobby embed.
        if lobby_message_id and lobby_channel_id:
            jump_url = f"https://discord.com/channels/{guild_id}/{lobby_channel_id}/{lobby_message_id}"
            embed.add_field(name="", value=f"[Jump to Lobby]({jump_url})", inline=False)

        # Per-lobby reaction subscribers, persistent subscribers, and the
        # destination channel can all be resolved independently.
        reaction_subscribers, persistent_subscriber_ids, target_channel = await asyncio.gather(
            load_reaction_subscribers(lobby_message_id, lobby_channel_id),
            load_persistent_subscriber_ids(),
            resolve_target_channel(origin_channel_id),
        )

        excluded_ids = set(lobby.players)
        bot_user_id = getattr(getattr(bot, "user", None), "id", None)
        if isinstance(bot_user_id, int):
            excluded_ids.add(bot_user_id)
        subscriber_mentions: list[str] = []
        subscriber_ids: list[int] = []
        seen_ids = set(excluded_ids)
        for subscriber_id, mention in reaction_subscribers:
            if subscriber_id not in seen_ids:
                seen_ids.add(subscriber_id)
                subscriber_ids.append(subscriber_id)
                subscriber_mentions.append(mention)
        valid_persistent_ids = {
            subscriber_id
            for subscriber_id in persistent_subscriber_ids
            if isinstance(subscriber_id, int) and subscriber_id > 0
        }
        for subscriber_id in sorted(valid_persistent_ids):
            if subscriber_id not in seen_ids:
                seen_ids.add(subscriber_id)
                subscriber_ids.append(subscriber_id)
                subscriber_mentions.append(f"<@{subscriber_id}>")

        # Discord caps explicit allowed-mention users at 100 and message content
        # at 2,000 characters. Prefer the current lobby's 📋 reactors, which
        # were added first, then fill the remaining capacity with auto-subscribers.
        selected_mentions: list[str] = []
        selected_ids: list[int] = []
        content_length = 0
        for subscriber_id, mention in zip(subscriber_ids, subscriber_mentions, strict=True):
            added_length = len(mention) + int(bool(selected_mentions))
            if len(selected_ids) >= 100 or content_length + added_length > 2000:
                break
            selected_ids.append(subscriber_id)
            selected_mentions.append(mention)
            content_length += added_length
        if len(selected_ids) < len(subscriber_ids):
            logger.warning(
                "Lobby rally subscriber list truncated from %d to %d for guild %s",
                len(subscriber_ids),
                len(selected_ids),
                guild_id,
            )

        # Send to origin channel (or reaction channel as fallback)
        content = " ".join(selected_mentions) or None
        target_send = target_channel.send(
            content=content,
            embed=embed,
            allowed_mentions=discord.AllowedMentions(
                everyone=False,
                roles=False,
                users=[discord.Object(id=subscriber_id) for subscriber_id in selected_ids],
                replied_user=False,
            ),
        )
        if thread:
            target_result, thread_result = await asyncio.gather(
                target_send,
                thread.send(
                    f"📢 {kind.label}: **+{needed}** more "
                    f"player{'s' if needed > 1 else ''} needed!"
                ),
                return_exceptions=True,
            )
            if isinstance(target_result, BaseException):
                raise target_result
            if isinstance(thread_result, BaseException):
                if not isinstance(thread_result, Exception):
                    raise thread_result
                logger.warning("Could not send lobby rally to thread: %s", thread_result)
        else:
            await target_send

        return True
    except Exception as exc:
        logger.error(f"Error sending rally notification: {exc}", exc_info=True)
        return False


def clear_lobby_rally_cooldowns(
    guild_id: int,
    lobby_kind: LobbyKind | str | None = None,
) -> None:
    """Clear rally and ready cooldowns for one lobby, or all lobbies in a guild."""
    kind = LobbyKind.normalize(lobby_kind) if lobby_kind is not None else None
    keys_to_remove = [
        key
        for key in _lobby_rally_cooldowns
        if key[0] == guild_id and (kind is None or key[1] == kind)
    ]
    for key in keys_to_remove:
        del _lobby_rally_cooldowns[key]
    ready_keys_to_remove = [
        key
        for key in _lobby_ready_cooldowns
        if key[0] == guild_id and (kind is None or key[1] == kind)
    ]
    for key in ready_keys_to_remove:
        del _lobby_ready_cooldowns[key]


@bot.event
async def setup_hook():
    """Load command cogs."""
    await _load_extensions()


def _log_command_registration(stage: str):
    """Log top-level command capacity separately from nested command nodes."""
    summary = summarize_command_tree(bot.tree)
    logger.info(
        "%s: %d/%d top-level commands; %d total command/group nodes",
        stage,
        summary.top_level_count,
        CHAT_INPUT_COMMAND_LIMIT,
        summary.node_count,
    )
    if summary.near_option_limit:
        logger.warning(
            "%s groups nearing Discord's %d-option limit: %s",
            stage,
            COMMAND_OPTION_LIMIT,
            summary.near_option_limit,
        )
    if summary.duplicate_qualified_names:
        logger.warning(
            "%s duplicate qualified command registrations: %s",
            stage,
            summary.duplicate_qualified_names,
        )
    return summary


def _refresh_vanity_tax_memberships() -> None:
    """Rebuild nickname eligibility from Discord's current member cache."""
    service = getattr(bot, "vanity_tax_service", None)
    if service is None:
        return
    for guild in bot.guilds:
        service.refresh_guild(guild.id, guild.members)


@bot.event
async def on_member_join(member: discord.Member) -> None:
    service = getattr(bot, "vanity_tax_service", None)
    if service is not None:
        service.update_member(member.guild.id, member.id, member.nick)


@bot.event
async def on_member_update(
    before: discord.Member,
    after: discord.Member,
) -> None:
    service = getattr(bot, "vanity_tax_service", None)
    if service is not None:
        service.update_member(after.guild.id, after.id, after.nick)


@bot.event
async def on_member_remove(member: discord.Member) -> None:
    service = getattr(bot, "vanity_tax_service", None)
    if service is not None:
        service.remove_member(member.guild.id, member.id)


@bot.event
async def on_ready():
    """Called when bot is ready."""
    logger.info(f"{bot.user} connected. Guilds: {len(bot.guilds)}")
    _refresh_vanity_tax_memberships()

    _log_command_registration("Pre-sync")
    logger.info(f"Loaded cogs: {list(bot.cogs.keys())}")

    try:
        await bot.tree.sync()
        logger.info("Slash commands synced globally.")

        _log_command_registration("Post-sync")
    except Exception as exc:
        logger.error(f"Failed to sync commands: {exc}", exc_info=True)

    await _reconcile_persisted_lobby_messages()

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

        warm_task = _retain_background_task(bot.loop.create_task(asyncio.to_thread(warm_cache)))
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

        region_task = _retain_background_task(
            bot.loop.create_task(
                run_opendota_io(region_service.backfill_inferred_regions)
            )
        )
        region_task.add_done_callback(_log_region_backfill)

    # Start prediction-market background tasks (refresh worker + daily digest).
    # Both are wrapped in a supervisor that auto-restarts the body on a
    # crash, and a done-callback that surfaces an unexpected exit to the log
    # so we can never lose a feature to silent failure.
    global _prediction_refresh_task, _prediction_digest_task, _manashop_debt_task
    global _duel_challenge_task, _economy_event_task, _first_game_pool_task
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
    if _duel_challenge_task is None or _duel_challenge_task.done():
        _duel_challenge_task = bot.loop.create_task(
            _supervised_loop("duel_challenges", _duel_challenge_loop)
        )
        _duel_challenge_task.add_done_callback(
            _log_task_exit("duel_challenges")
        )
    if ECONOMY_EVENTS_ENABLED and (
        _economy_event_task is None or _economy_event_task.done()
    ):
        _economy_event_task = bot.loop.create_task(
            _supervised_loop("economy_events", _economy_event_loop)
        )
        _economy_event_task.add_done_callback(_log_task_exit("economy_events"))
    if FIRST_GAME_POOL_DAILY_AMOUNT > 0 and (
        _first_game_pool_task is None or _first_game_pool_task.done()
    ):
        _first_game_pool_task = bot.loop.create_task(
            _supervised_loop("first_game_pool", _first_game_pool_loop)
        )
        _first_game_pool_task.add_done_callback(_log_task_exit("first_game_pool"))

    reminder_svc = getattr(bot, "reminder_service", None)
    if reminder_svc:
        guild_ids = [guild.id for guild in bot.guilds]
        _start_reminder_recovery(reminder_svc, guild_ids)


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


def _is_legacy_frogling_emoji(emoji) -> bool:
    """Identify the retired lobby reaction so it can be removed."""
    return getattr(emoji, "id", None) == _LEGACY_FROGLING_EMOJI_ID


def _is_jopacoin_emoji(emoji) -> bool:
    """Check if the emoji is the jopacoin emoji for gamba notifications."""
    # Custom emoji: check by ID or name
    return emoji.id == JOPACOIN_EMOJI_ID or emoji.name == "jopacoin"


async def _resolve_raw_reaction_user(
    payload,
) -> discord.User | discord.Member:
    """Resolve a reaction actor without REST when gateway/cache data exists."""
    member = getattr(payload, "member", None)
    if member is not None:
        return member

    guild_id = getattr(payload, "guild_id", None)
    guild = bot.get_guild(guild_id) if guild_id is not None else None
    if guild is not None:
        member = guild.get_member(payload.user_id)
        if member is not None:
            return member

    user = bot.get_user(payload.user_id)
    if user is not None:
        return user

    return await bot.fetch_user(payload.user_id)


@bot.event
async def on_raw_reaction_add(payload):
    """Handle reaction adds for lobby joining, readycheck confirmations, and gamba notifications."""
    if not bot.user or payload.user_id == bot.user.id:
        return

    # Handle readycheck ✅ reactions
    if payload.emoji.name == "✅":
        _init_services()
        payload_guild_id = payload.guild_id
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload_guild_id,
            readycheck=True,
        )
        if lobby_kind is not None:
            rc_msg_id = payload.message_id
            try:
                added = await asyncio.to_thread(
                    bot.lobby_service.add_readycheck_reaction,
                    payload.user_id,
                    f"<@{payload.user_id}>",
                    guild_id=payload_guild_id,
                    lobby_kind=lobby_kind,
                    expected_message_id=rc_msg_id,
                )
                if added:
                    cog = bot.get_cog("LobbyCommands")
                    if cog:
                        await cog.notify_readycheck_completion_if_ready(
                            payload_guild_id or 0,
                            rc_msg_id,
                            fallback_channel_id=payload.channel_id,
                            lobby_kind=lobby_kind,
                        )
                    try:
                        channel = bot.get_channel(payload.channel_id)
                        if not channel:
                            channel = await bot.fetch_channel(payload.channel_id)
                        embed = (
                            await asyncio.to_thread(
                                cog.rebuild_readycheck_embed,
                                guild_id=payload_guild_id,
                                lobby_kind=lobby_kind,
                                expected_message_id=rc_msg_id,
                            )
                            if cog
                            else None
                        )
                        if embed:
                            message = channel.get_partial_message(payload.message_id)
                            await message.edit(embed=embed)
                    except Exception as exc:
                        logger.error(
                            f"Error refreshing readycheck embed: {exc}",
                            exc_info=True,
                        )
            except Exception as exc:
                logger.error(f"Error handling readycheck reaction: {exc}", exc_info=True)
        return

    # Handle 🔔 readycheck-shortcut reactions on the lobby embed
    if payload.emoji.name == "🔔":
        _init_services()
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload.guild_id,
        )
        if lobby_kind is None:
            return
        cog = bot.get_cog("LobbyCommands")
        if not cog:
            return
        guild = bot.get_guild(payload.guild_id) if payload.guild_id else None
        if not guild:
            return
        try:
            status, _info = await cog._execute_readycheck(
                guild,
                payload.guild_id,
                payload.user_id,
                lobby_kind=lobby_kind,
            )
        except Exception as exc:
            logger.error(f"Error running 🔔 readycheck shortcut: {exc}", exc_info=True)
            status = "error"
        if status == "ok" and _info.get("mention_ids"):
            try:
                origin_channel_id = await asyncio.to_thread(
                    bot.lobby_service.get_origin_channel_id,
                    guild_id=payload.guild_id,
                    lobby_kind=lobby_kind,
                )
                if (
                    origin_channel_id
                    and origin_channel_id != _info.get("readycheck_channel_id")
                ):
                    origin_channel = bot.get_channel(origin_channel_id)
                    if not origin_channel:
                        origin_channel = await bot.fetch_channel(origin_channel_id)
                    mention_ids = _info["mention_ids"]
                    tags = " ".join(f"<@{user_id}>" for user_id in mention_ids)
                    current_readycheck_message_id = await asyncio.to_thread(
                        bot.lobby_service.get_readycheck_message_id,
                        guild_id=payload.guild_id,
                        lobby_kind=lobby_kind,
                    )
                    if current_readycheck_message_id != _info.get(
                        "readycheck_message_id"
                    ):
                        return
                    await origin_channel.send(
                        (
                            f"⚔️ **{lobby_kind.label} ready check!** {tags}\n"
                            f"[React in the lobby thread]({_info['message_jump_url']})"
                        ),
                        allowed_mentions=discord.AllowedMentions(
                            users=[
                                discord.Object(id=user_id) for user_id in mention_ids
                            ]
                        ),
                    )
            except Exception as exc:
                logger.error(
                    "Error advertising 🔔 readycheck in origin channel: %s",
                    exc,
                    exc_info=True,
                )
        if status != "ok":
            # Visible feedback that nothing happened — remove only this user's reaction
            try:
                channel = bot.get_channel(payload.channel_id)
                if not channel:
                    channel = await bot.fetch_channel(payload.channel_id)
                message = channel.get_partial_message(payload.message_id)
                user = await _resolve_raw_reaction_user(payload)
                await message.remove_reaction("🔔", user)
            except Exception:
                pass
        return

    is_sword = _is_sword_emoji(payload.emoji)
    is_legacy_frogling = _is_legacy_frogling_emoji(payload.emoji)
    is_jopacoin = _is_jopacoin_emoji(payload.emoji)

    if is_legacy_frogling:
        _init_services()
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload.guild_id,
        )
        if lobby_kind is None:
            return
        try:
            channel = bot.get_channel(payload.channel_id)
            if not channel:
                channel = await bot.fetch_channel(payload.channel_id)
            message = channel.get_partial_message(payload.message_id)
            await message.remove_reaction(
                payload.emoji,
                discord.Object(id=payload.user_id),
            )
        except Exception as exc:
            logger.warning(
                "Could not remove re-added retired Frogling reaction: %s",
                exc,
            )
        return

    if not is_sword and not is_jopacoin:
        return

    _init_services()  # Ensure services are initialized
    try:
        payload_guild_id = payload.guild_id
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload_guild_id,
        )
        if lobby_kind is None:
            return

        lobby = await asyncio.to_thread(
            bot.lobby_service.get_lobby,
            guild_id=payload_guild_id,
            lobby_kind=lobby_kind,
        )
        if not lobby or lobby.status != "open":
            return

        # Handle jopacoin reaction for gamba notifications
        if is_jopacoin:
            already_in_lobby = payload.user_id in lobby.players
            if already_in_lobby:
                return

        channel = bot.get_channel(payload.channel_id)
        if not channel:
            channel = await bot.fetch_channel(payload.channel_id)
        user = await _resolve_raw_reaction_user(payload)

        if is_jopacoin:
            thread_id = await asyncio.to_thread(
                bot.lobby_service.get_lobby_thread_id,
                guild_id=payload_guild_id,
                lobby_kind=lobby_kind,
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

                        _retain_background_task(
                            asyncio.create_task(_delete_after(neon_msg, 60))
                        )
            except Exception as exc:
                logger.debug(f"Neon gamba spectator hook failed: {exc}")
            return

        # The rest of the handler is for sword-based lobby joining.
        message = channel.get_partial_message(payload.message_id)
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

        join_cutoff_ns = time.time_ns()
        success, reason, pending_info = await asyncio.to_thread(
            bot.lobby_service.join_lobby,
            payload.user_id,
            guild_id,
            lobby_kind,
        )

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
            elif reason == "lobby_suspended":
                try:
                    await channel.send(
                        f"{user.mention} ❌ You are temporarily restricted from this "
                        "matchmaking lobby. Check your DMs or use `/player lobby status`.",
                        delete_after=15,
                    )
                except Exception:
                    pass
                try:
                    private_reason = getattr(pending_info, "reason", "Not provided")
                    await user.send(
                        "You are temporarily suspended from this matchmaking lobby.\n"
                        f"Reason: {private_reason}\n"
                        "Use `/player lobby status` in the server for the exact remaining term."
                    )
                except Exception:
                    pass
            else:
                reason_messages = {
                    "lobby_full": "Lobby is full.",
                    "already_joined": "Already in lobby.",
                    "rating_too_high": (
                        f"{LobbyKind.LOWSKILL.label} is limited to Glicko below 1400."
                    ),
                    "in_other_lobby": "Leave your current lobby before joining another.",
                    "in_flight": (
                        "You can't switch lobbies while your current shuffle or draft "
                        "is in progress."
                    ),
                }
                msg = reason_messages.get(reason, "Could not join lobby.")
                try:
                    await channel.send(f"{user.mention} ❌ {msg}", delete_after=10)
                except Exception:
                    pass
            return

        # Claim and deliver target watches from the confirmed join result, but
        # keep that work independent of embed updates and public 8/9 alerts.
        reminder_service = getattr(bot, "reminder_service", None)
        if reminder_service is not None:
            notification_task = _retain_background_task(
                asyncio.create_task(
                    reminder_service.notify_lobby_player_subscribers(
                        bot,
                        user.id,
                        user.display_name,
                        payload.guild_id or 0,
                        lobby_kind=lobby_kind,
                        join_cutoff_ns=join_cutoff_ns,
                    )
                )
            )
            notification_task.add_done_callback(
                _log_task_exit(f"lobby target notification for {user.id}")
            )

        # Re-fetch the lobby after the join lands so the embed update and
        # readycheck see the post-join roster. The earlier fetch (line ~801)
        # is from before the join, so under concurrent reactions it can lag.
        lobby = await asyncio.to_thread(
            bot.lobby_service.get_lobby,
            guild_id=payload_guild_id,
            lobby_kind=lobby_kind,
        )
        if not lobby:
            return

        await update_lobby_message(message, lobby, payload.guild_id)
        lobby_cog = bot.get_cog("LobbyCommands")
        if lobby_cog:
            await lobby_cog.sync_readycheck_with_lobby(
                payload.guild_id,
                lobby_kind=lobby_kind,
            )

        # Mention user in thread to subscribe them
        thread_id = await asyncio.to_thread(
            bot.lobby_service.get_lobby_thread_id,
            guild_id=payload_guild_id,
            lobby_kind=lobby_kind,
        )
        thread = None
        if thread_id:
            try:
                thread = bot.get_channel(thread_id)
                if not thread:
                    thread = await bot.fetch_channel(thread_id)
                await thread.send(f"✅ {user.mention} joined {lobby_kind.label}!")
            except Exception as exc:
                logger.warning(f"Failed to post join activity in thread: {exc}")

        # Check for rally notification (+2 or +1 needed)
        if not await asyncio.to_thread(bot.lobby_service.is_ready, lobby):
            guild_id = payload.guild_id or 0
            await notify_lobby_rally(
                channel,
                thread,
                lobby,
                guild_id,
                lobby_kind=lobby_kind,
            )
        else:
            await notify_lobby_ready(
                channel,
                guild_id=payload.guild_id or 0,
                lobby_kind=lobby_kind,
            )
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
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload_guild_id,
            readycheck=True,
        )
        if lobby_kind is not None:
            rc_msg_id = payload.message_id
            try:
                removed = await asyncio.to_thread(
                    bot.lobby_service.remove_readycheck_reaction,
                    payload.user_id,
                    guild_id=payload_guild_id,
                    lobby_kind=lobby_kind,
                    expected_message_id=rc_msg_id,
                )
                if removed:
                    cog = bot.get_cog("LobbyCommands")
                    embed = (
                        await asyncio.to_thread(
                            cog.rebuild_readycheck_embed,
                            guild_id=payload_guild_id,
                            lobby_kind=lobby_kind,
                            expected_message_id=rc_msg_id,
                        )
                        if cog
                        else None
                    )
                    if embed:
                        channel = bot.get_channel(payload.channel_id)
                        if not channel:
                            channel = await bot.fetch_channel(payload.channel_id)
                        message = channel.get_partial_message(payload.message_id)
                        await message.edit(embed=embed)
            except Exception as exc:
                logger.error(f"Error handling readycheck reaction remove: {exc}", exc_info=True)
        return

    is_sword = _is_sword_emoji(payload.emoji)

    if not is_sword:
        return

    _init_services()  # Ensure services are initialized
    try:
        payload_guild_id = payload.guild_id
        lobby_kind = await _resolve_lobby_kind_for_message(
            bot.lobby_service,
            payload.message_id,
            payload_guild_id,
        )
        if lobby_kind is None:
            return

        lobby = await asyncio.to_thread(
            bot.lobby_service.get_lobby,
            guild_id=payload_guild_id,
            lobby_kind=lobby_kind,
        )
        if not lobby or lobby.status != "open":
            return

        channel = bot.get_channel(payload.channel_id)
        if not channel:
            channel = await bot.fetch_channel(payload.channel_id)
        message = channel.get_partial_message(payload.message_id)

        left = await asyncio.to_thread(
            bot.lobby_service.leave_lobby,
            payload.user_id,
            payload_guild_id,
            lobby_kind,
        )

        if left:
            await update_lobby_message(message, lobby, payload.guild_id)
            lobby_cog = bot.get_cog("LobbyCommands")
            if lobby_cog:
                await lobby_cog.sync_readycheck_with_lobby(
                    payload.guild_id,
                    lobby_kind=lobby_kind,
                )

            # Post leave message in thread
            thread_id = await asyncio.to_thread(
                bot.lobby_service.get_lobby_thread_id,
                guild_id=payload_guild_id,
                lobby_kind=lobby_kind,
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
                    await thread.send(f"🚪 {display} left {lobby_kind.label}.")
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
