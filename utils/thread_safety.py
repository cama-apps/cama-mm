"""Helpers for writing into bot-managed threads safely."""

# Max auto-archive window (minutes). Bot-managed threads get little organic
# activity (trade confirmations are ephemeral), so use the widest window and
# re-apply it whenever a thread is revived.
THREAD_AUTO_ARCHIVE_MINUTES = 10080


async def ensure_thread_writable(channel) -> None:
    """Revive an archived thread so message edits (and sends) succeed.

    Message edits in an archived thread are rejected by Discord (error 50083).
    Plain sends auto-unarchive an *unlocked* thread, but reviving explicitly
    also covers locked threads (the bot created them, and a thread's creator
    may unarchive it even when locked) and re-widens the auto-archive window
    in the same API call so quiet threads don't re-archive daily.

    No-op for non-thread channels (they have no ``archived`` attribute).
    """
    if getattr(channel, "archived", False):
        await channel.edit(
            archived=False, auto_archive_duration=THREAD_AUTO_ARCHIVE_MINUTES
        )
