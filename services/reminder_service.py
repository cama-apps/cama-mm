import asyncio
import logging
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from discord.ext import commands

logger = logging.getLogger("cama_bot.reminder_service")
_READY_AT_UNSET = object()


class ReminderService:
    def __init__(self, notification_repo, player_repo, dig_service=None):
        self._notification_repo = notification_repo
        self._player_repo = player_repo
        self._dig_service = dig_service
        self._tasks: dict[tuple[int, int, str], asyncio.Task] = {}
        self._dig_locks: dict[tuple[int, int, str], asyncio.Lock] = {}
        self._dig_reconcile_versions: dict[tuple[int, int, str], int] = {}
        self._dig_reconcile_reference_times: dict[tuple[int, int, str], float] = {}
        self._next_dig_reconcile_version = 0

    # ------------------------------------------------------------------
    # Preference management
    # ------------------------------------------------------------------

    def get_preferences(self, discord_id: int, guild_id: int) -> dict:
        return self._notification_repo.get_preferences(discord_id, guild_id)

    def toggle_preference(self, discord_id: int, guild_id: int, reminder_type: str) -> bool:
        prefs = self._notification_repo.get_preferences(discord_id, guild_id)
        new_state = not prefs.get(f"{reminder_type}_enabled", False)
        self._notification_repo.set_preference(discord_id, guild_id, reminder_type, new_state)
        if not new_state:
            self._cancel_task(discord_id, guild_id, reminder_type)
        return new_state

    # ------------------------------------------------------------------
    # Scheduling
    # ------------------------------------------------------------------

    def schedule_wheel_reminder(
        self,
        bot: "commands.Bot",
        discord_id: int,
        guild_id: int,
        next_spin_time: int,
        *,
        preference_enabled: bool | None = None,
    ) -> None:
        if preference_enabled is None:
            prefs = self._notification_repo.get_preferences(discord_id, guild_id)
            preference_enabled = bool(prefs.get("wheel_enabled"))
        if not preference_enabled:
            return
        delay = max(0.0, next_spin_time - time.time())
        self._cancel_task(discord_id, guild_id, "wheel")
        task = asyncio.create_task(
            self._send_dm_after_delay(
                delay=delay,
                bot=bot,
                discord_id=discord_id,
                message="Your wheel cooldown has expired! You can `/gamba` again now.",
            )
        )
        self._register_task((discord_id, guild_id, "wheel"), task)

    def schedule_trivia_reminder(
        self,
        bot: "commands.Bot",
        discord_id: int,
        guild_id: int,
        next_trivia_time: int,
        *,
        preference_enabled: bool | None = None,
    ) -> None:
        if preference_enabled is None:
            prefs = self._notification_repo.get_preferences(discord_id, guild_id)
            preference_enabled = bool(prefs.get("trivia_enabled"))
        if not preference_enabled:
            return
        delay = max(0.0, next_trivia_time - time.time())
        self._cancel_task(discord_id, guild_id, "trivia")
        task = asyncio.create_task(
            self._send_dm_after_delay(
                delay=delay,
                bot=bot,
                discord_id=discord_id,
                message="Your trivia cooldown has expired! You can `/trivia` again now.",
            )
        )
        self._register_task((discord_id, guild_id, "trivia"), task)

    def schedule_dig_reminder(
        self,
        bot: "commands.Bot",
        discord_id: int,
        guild_id: int,
        next_dig_time: int,
        *,
        preference_enabled: bool | None = None,
    ) -> None:
        guild_id = 0 if guild_id is None else guild_id
        if preference_enabled is None:
            prefs = self._notification_repo.get_preferences(discord_id, guild_id)
            preference_enabled = bool(prefs.get("dig_enabled"))
        if not preference_enabled:
            self._cancel_task(discord_id, guild_id, "dig")
            return
        delay = max(0.0, next_dig_time - time.time())
        self._cancel_task(discord_id, guild_id, "dig")
        task = asyncio.create_task(
            self._send_dm_after_delay(
                delay=delay,
                bot=bot,
                discord_id=discord_id,
                message="Your free dig cooldown has expired! You can `/dig go` again now.",
            )
        )
        self._register_task((discord_id, guild_id, "dig"), task)

    def cancel_dig_reminder(self, discord_id: int, guild_id: int) -> None:
        guild_id = 0 if guild_id is None else guild_id
        self._cancel_task(discord_id, guild_id, "dig")

    async def reconcile_dig_reminder(
        self,
        bot: "commands.Bot",
        discord_id: int,
        guild_id: int,
        *,
        now: int | None = None,
        ready_at: int | None | object = _READY_AT_UNSET,
        preference_enabled: bool | None = None,
    ) -> None:
        """Reconcile one dig reminder with the authoritative eligibility time."""
        guild_id = 0 if guild_id is None else guild_id
        key = (discord_id, guild_id, "dig")
        reference_now = int(time.time()) if now is None else now

        # A restart recovery carries an older reference time than a subsequent
        # live reconciliation. Do not let that stale request run after the live
        # result merely because its coroutine happened to be scheduled later.
        latest_reference = self._dig_reconcile_reference_times.get(key)
        if latest_reference is not None and reference_now < latest_reference:
            return

        self._next_dig_reconcile_version += 1
        version = self._next_dig_reconcile_version
        self._dig_reconcile_versions[key] = version
        self._dig_reconcile_reference_times[key] = reference_now

        lock = self._dig_locks.setdefault(key, asyncio.Lock())
        async with lock:
            if ready_at is _READY_AT_UNSET:
                ready_at = await asyncio.to_thread(
                    self._dig_service.get_free_dig_ready_at,
                    discord_id,
                    guild_id,
                    now=reference_now,
                )

            # Another reconcile may have arrived while the authoritative read
            # was in flight. It owns the final task mutation.
            if self._dig_reconcile_versions.get(key) != version:
                return

            if ready_at is None or ready_at <= reference_now:
                self._cancel_task(discord_id, guild_id, "dig")
                return

            if preference_enabled is None:
                prefs = await asyncio.to_thread(
                    self._notification_repo.get_preferences,
                    discord_id,
                    guild_id,
                )
                preference_enabled = bool(prefs.get("dig_enabled"))
            self.schedule_dig_reminder(
                bot,
                discord_id,
                guild_id,
                ready_at,
                preference_enabled=preference_enabled,
            )

    async def _cancel_stale_dig_reminder(
        self,
        discord_id: int,
        guild_id: int,
        *,
        now: int,
    ) -> None:
        """Cancel a recovery-only stale task without superseding newer live work."""
        key = (discord_id, guild_id, "dig")
        latest_reference = self._dig_reconcile_reference_times.get(key)
        if latest_reference is not None and now < latest_reference:
            return

        self._next_dig_reconcile_version += 1
        version = self._next_dig_reconcile_version
        self._dig_reconcile_versions[key] = version
        self._dig_reconcile_reference_times[key] = now

        lock = self._dig_locks.setdefault(key, asyncio.Lock())
        async with lock:
            if self._dig_reconcile_versions.get(key) == version:
                self._cancel_task(discord_id, guild_id, "dig")

    async def notify_betting_subscribers(
        self, bot: "commands.Bot", guild_id: int, bet_lock_until: int
    ) -> None:
        subscribers = await asyncio.to_thread(
            self._notification_repo.get_enabled_users_for_type, guild_id, "betting"
        )
        if not subscribers:
            return
        remaining = max(0, bet_lock_until - int(time.time()))
        minutes = remaining // 60
        message = (
            f"A new match has been shuffled! Betting is open for ~{minutes} more minutes. "
            "Use `/bet` now!"
        )
        for discord_id in subscribers:
            asyncio.create_task(self._dm_user(bot, discord_id, message))

    # ------------------------------------------------------------------
    # Restart recovery
    # ------------------------------------------------------------------

    async def reschedule_all(self, bot: "commands.Bot", guild_ids: list[int]) -> None:
        from config import TRIVIA_COOLDOWN_SECONDS, WHEEL_COOLDOWN_SECONDS

        now = int(time.time())
        standard_reminders = (
            (
                "wheel",
                "last_wheel_spin",
                WHEEL_COOLDOWN_SECONDS,
                self.schedule_wheel_reminder,
            ),
            (
                "trivia",
                "last_trivia_session",
                TRIVIA_COOLDOWN_SECONDS,
                self.schedule_trivia_reminder,
            ),
        )

        for guild_id in guild_ids:
            guild_id = 0 if guild_id is None else guild_id
            reminder_types = ("wheel", "trivia", "dig")
            try:
                subscribers_by_type = await asyncio.to_thread(
                    self._notification_repo.get_enabled_users_by_type_bulk,
                    guild_id,
                    reminder_types,
                )
            except Exception:
                logger.exception(
                    "Failed to load reminder subscribers for guild_id=%d",
                    guild_id,
                )
                continue

            standard_subscriber_ids = list(
                dict.fromkeys(
                    discord_id
                    for reminder_type, *_ in standard_reminders
                    for discord_id in subscribers_by_type.get(reminder_type, ())
                )
            )
            try:
                timestamps = await asyncio.to_thread(
                    self._player_repo.get_reminder_timestamps_bulk,
                    standard_subscriber_ids,
                    guild_id,
                )
            except Exception:
                logger.exception(
                    "Failed to load reminder timestamps for guild_id=%d",
                    guild_id,
                )
                timestamps = {}

            for reminder_type, timestamp_field, cooldown, schedule in standard_reminders:
                for discord_id in subscribers_by_type.get(reminder_type, ()):
                    try:
                        last_at = timestamps.get(discord_id, {}).get(timestamp_field)
                        if last_at is None:
                            continue
                        ready_at = last_at + cooldown
                        if ready_at > now:
                            schedule(
                                bot,
                                discord_id,
                                guild_id,
                                ready_at,
                                preference_enabled=True,
                            )
                    except Exception:
                        logger.exception(
                            "Failed to recover %s reminder for discord_id=%d guild_id=%d",
                            reminder_type,
                            discord_id,
                            guild_id,
                        )

            if self._dig_service is None:
                continue

            dig_subscribers = subscribers_by_type.get("dig", [])

            subscriber_ids = set(dig_subscribers)
            stale_dig_keys = [
                key
                for key in self._tasks
                if key[1] == guild_id and key[2] == "dig" and key[0] not in subscriber_ids
            ]
            for discord_id, _, _ in stale_dig_keys:
                try:
                    await self._cancel_stale_dig_reminder(
                        discord_id,
                        guild_id,
                        now=now,
                    )
                except Exception:
                    logger.exception(
                        "Failed to remove stale dig reminder for discord_id=%d guild_id=%d",
                        discord_id,
                        guild_id,
                    )

            try:
                dig_ready_times = await asyncio.to_thread(
                    self._dig_service.get_free_dig_ready_times_bulk,
                    dig_subscribers,
                    guild_id,
                    now=now,
                )
            except Exception:
                logger.exception(
                    "Failed to load dig reminder times for guild_id=%d",
                    guild_id,
                )
                continue

            for discord_id in dig_subscribers:
                if discord_id not in dig_ready_times:
                    continue
                try:
                    await self.reconcile_dig_reminder(
                        bot,
                        discord_id,
                        guild_id,
                        now=now,
                        ready_at=dig_ready_times[discord_id],
                        preference_enabled=True,
                    )
                except Exception:
                    logger.exception(
                        "Failed to recover dig reminder for discord_id=%d guild_id=%d",
                        discord_id,
                        guild_id,
                    )

        logger.info("Reminder service rescheduled %d tasks after restart", len(self._tasks))

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _register_task(self, key: tuple, task: asyncio.Task) -> None:
        """Track a task and ensure it's removed from ``_tasks`` once it finishes.

        Without the done-callback, naturally-completed tasks would linger in
        ``_tasks`` forever (only ``_cancel_task`` pops).
        """
        self._tasks[key] = task
        task.add_done_callback(lambda t, k=key: self._tasks.pop(k, None) if self._tasks.get(k) is t else None)

    def _cancel_task(self, discord_id: int, guild_id: int, reminder_type: str) -> None:
        task = self._tasks.pop((discord_id, guild_id, reminder_type), None)
        if task and not task.done():
            task.cancel()

    async def _send_dm_after_delay(
        self, *, delay: float, bot: "commands.Bot", discord_id: int, message: str
    ) -> None:
        try:
            await asyncio.sleep(delay)
            await self._dm_user(bot, discord_id, message)
        except asyncio.CancelledError:
            pass
        except Exception as exc:
            logger.debug("Reminder task error for %d: %s", discord_id, exc)

    async def _dm_user(self, bot: "commands.Bot", discord_id: int, message: str) -> None:
        try:
            user = bot.get_user(discord_id) or await bot.fetch_user(discord_id)
            await user.send(message)
        except Exception as exc:
            logger.debug("Failed to DM user %d: %s", discord_id, exc)
