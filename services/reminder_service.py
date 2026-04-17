import asyncio
import logging
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from discord.ext import commands

logger = logging.getLogger("cama_bot.reminder_service")


class ReminderService:
    def __init__(self, notification_repo, player_repo):
        self._notification_repo = notification_repo
        self._player_repo = player_repo
        self._tasks: dict[tuple, asyncio.Task] = {}

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
        self, bot: "commands.Bot", discord_id: int, guild_id: int, next_spin_time: int
    ) -> None:
        prefs = self._notification_repo.get_preferences(discord_id, guild_id)
        if not prefs.get("wheel_enabled"):
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
        self._tasks[(discord_id, guild_id, "wheel")] = task

    def schedule_trivia_reminder(
        self, bot: "commands.Bot", discord_id: int, guild_id: int, next_trivia_time: int
    ) -> None:
        prefs = self._notification_repo.get_preferences(discord_id, guild_id)
        if not prefs.get("trivia_enabled"):
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
        self._tasks[(discord_id, guild_id, "trivia")] = task

    async def notify_betting_subscribers(
        self, bot: "commands.Bot", guild_id: int, bet_lock_until: int
    ) -> None:
        subscribers = self._notification_repo.get_enabled_users_for_type(guild_id, "betting")
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
        for guild_id in guild_ids:
            for discord_id in self._notification_repo.get_enabled_users_for_type(guild_id, "wheel"):
                last_spin = self._player_repo.get_last_wheel_spin(discord_id, guild_id)
                if last_spin is None:
                    continue
                next_spin = last_spin + WHEEL_COOLDOWN_SECONDS
                if next_spin > now:
                    self.schedule_wheel_reminder(bot, discord_id, guild_id, next_spin)

            for discord_id in self._notification_repo.get_enabled_users_for_type(guild_id, "trivia"):
                last_trivia = self._player_repo.get_last_trivia_session(discord_id, guild_id)
                if last_trivia is None:
                    continue
                next_trivia = last_trivia + TRIVIA_COOLDOWN_SECONDS
                if next_trivia > now:
                    self.schedule_trivia_reminder(bot, discord_id, guild_id, next_trivia)

        logger.info("Reminder service rescheduled %d tasks after restart", len(self._tasks))

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

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
