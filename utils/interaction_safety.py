"""
Utilities for safely interacting with Discord responses/followups.
"""

import logging
from collections.abc import Awaitable, Callable

import discord

logger = logging.getLogger("cama_bot.utils.interaction_safety")


def friendly_error(action: str) -> str:
    """User-facing error text with a consistent voice and no leaked internals.

    ``action`` is a short verb phrase for what failed, e.g.
    ``friendly_error("load the leaderboard")`` ->
    "❌ Couldn't load the leaderboard right now — try again in a moment."
    Always pair this with a ``logger`` call that records the real exception.
    """
    return f"❌ Couldn't {action} right now — try again in a moment."


async def safe_defer(
    interaction: discord.Interaction,
    *,
    ephemeral: bool = False,
    thinking: bool = False,
) -> bool:
    """
    Defer the interaction if it is still valid.

    Returns True when the defer succeeded (or the response already exists),
    False when the interaction is no longer valid.
    """
    try:
        await interaction.response.defer(ephemeral=ephemeral, thinking=thinking)
        return True
    except (discord.NotFound, discord.InteractionResponded, discord.HTTPException) as exc:
        logger.warning("Unable to defer interaction: %s", exc)
        return False


async def safe_followup(
    interaction: discord.Interaction,
    *,
    content: str | None = None,
    embed: discord.Embed | None = None,
    file: discord.File | None = None,
    files: list[discord.File] | None = None,
    view: discord.ui.View | None = None,
    ephemeral: bool = False,
    allowed_mentions: discord.AllowedMentions | None = None,
) -> discord.Message | None:
    """
    Send a followup via the interaction; on failure, recover instead of failing silently.

    A non-ephemeral message that can't be sent as a followup falls back to a direct
    channel send, so a deferred interaction is never left on a perpetual "thinking…"
    spinner. An ephemeral message re-raises on failure rather than leaking private
    content into the public channel. If no recovery is possible the error propagates
    so the caller / global error handler can surface and log it.

    Supports either file (single) or files (multiple) - not both.
    """
    # Build kwargs, only including file/files/view when provided (Discord errors on None).
    send_kwargs: dict = {
        "content": content,
        "embed": embed,
        "ephemeral": ephemeral,
        "allowed_mentions": allowed_mentions,
    }
    if files is not None and len(files) > 0:
        send_kwargs["files"] = files
    elif file is not None:
        send_kwargs["file"] = file
    if view is not None:
        send_kwargs["view"] = view

    try:
        return await interaction.followup.send(**send_kwargs)
    except (discord.NotFound, discord.InteractionResponded, discord.HTTPException) as exc:
        # Ephemeral content must not spill into the public channel: surface the
        # failure to the caller / global handler instead of falling back publicly.
        if ephemeral:
            logger.warning("Ephemeral followup failed: %s", exc)
            raise

        channel = interaction.channel
        if channel is None:
            logger.warning("Followup failed and no channel to fall back to: %s", exc)
            raise

        # Fall back to a direct channel send so the user isn't left hanging. A bare
        # defer always makes interaction.response.is_done() True, so we deliberately
        # do NOT gate on it here — that check used to swallow every deferred-command
        # followup failure as a "duplicate handler" and leave a perpetual spinner.
        logger.warning("Followup failed, sending to channel instead: %s", exc)
        fallback_kwargs: dict = {"content": content, "embed": embed, "allowed_mentions": allowed_mentions}
        if files is not None and len(files) > 0:
            fallback_kwargs["files"] = files
        elif file is not None:
            fallback_kwargs["file"] = file
        if view is not None:
            fallback_kwargs["view"] = view
        # The failed followup may have partially consumed the attachment(s); rewind
        # so the channel send re-reads them from the start.
        attachments = list(files) if files else ([file] if file is not None else [])
        for attachment in attachments:
            try:
                attachment.reset()
            except Exception as reset_exc:
                logger.debug("Could not rewind attachment for channel fallback: %s", reset_exc)
        return await channel.send(**fallback_kwargs)


async def edit_message_with_fallback(
    message: discord.Message | None,
    *,
    send_fallback: Callable[..., Awaitable[object]] | None = None,
    log_label: str = "Message",
    edit_exceptions: tuple[type[BaseException], ...] = (Exception,),
    **edit_kwargs,
) -> bool:
    """Edit ``message``; on failure, rewind attachments and re-send fresh.

    ``message.edit`` fails once the message is deleted or (on view timeout
    paths) after Discord's 15-minute interaction token has expired; without a
    fallback the user is left staring at stale buttons forever. On edit
    failure any ``discord.File`` attachments are rewound (files are
    single-use — the failed edit consumed their buffers) and the embed (plus
    files) or content is re-sent: via ``send_fallback(embed=..., files=...,
    content=...)`` when provided (e.g. a ``safe_followup`` partial), otherwise
    via ``message.channel.send``. The ``view`` kwarg only applies to the edit;
    the fallback send carries no view. Failures are logged at WARNING with
    ``log_label`` so recurring problems stay visible.

    ``edit_exceptions`` scopes which edit failures trigger the fallback;
    pass ``(discord.HTTPException,)`` to let local bugs propagate instead of
    being masked by a duplicate post.

    Returns True when either the edit or the fallback delivered the content.
    """
    if message is None:
        return False
    try:
        await message.edit(**edit_kwargs)
        return True
    except edit_exceptions as exc:
        logger.warning("%s edit failed: %s", log_label, exc)

    embed = edit_kwargs.get("embed")
    content = edit_kwargs.get("content")
    files = [
        a for a in (edit_kwargs.get("attachments") or []) if isinstance(a, discord.File)
    ]
    # discord.File is single-use — the failed edit consumed the buffers, so
    # rewind each before the fallback send.
    for attachment in files:
        try:
            attachment.reset()
        except Exception as reset_exc:
            logger.debug("Could not rewind attachment for fallback send: %s", reset_exc)

    send_kwargs: dict = {}
    if embed is not None:
        send_kwargs["embed"] = embed
        if files:
            send_kwargs["files"] = files
    elif content:
        send_kwargs["content"] = content
    else:
        return False

    if send_fallback is None:
        channel = getattr(message, "channel", None)
        if channel is None:
            return False
        send_fallback = channel.send
    try:
        await send_fallback(**send_kwargs)
        return True
    except Exception as exc:
        logger.warning("%s fallback send also failed: %s", log_label, exc)
        return False


async def send_public_or_ephemeral(
    interaction: discord.Interaction,
    *,
    content: str | None = None,
    embed: discord.Embed | None = None,
    file: discord.File | None = None,
    files: list[discord.File] | None = None,
    view: discord.ui.View | None = None,
    allowed_mentions: discord.AllowedMentions | None = None,
) -> discord.Message | None:
    """Send a public result, guaranteeing the user sees it.

    Tries a public followup first. If that fails (e.g. the channel rejects the
    embed/attachment), retries privately as an ephemeral message *without the
    attachment* — attachments are decorative and the likeliest rejection cause,
    and an ephemeral retry avoids re-using an already-consumed file. As a last
    resort, sends an ephemeral note naming the failure so it is diagnosable
    without server logs.
    """
    try:
        return await safe_followup(
            interaction,
            content=content,
            embed=embed,
            file=file,
            files=files,
            view=view,
            allowed_mentions=allowed_mentions,
        )
    except Exception as exc:
        logger.warning("Public send failed (%s); retrying ephemerally without attachments", exc)
        try:
            return await safe_followup(
                interaction,
                content=content,
                embed=embed,
                view=view,
                allowed_mentions=allowed_mentions,
                ephemeral=True,
            )
        except Exception:
            logger.exception("Ephemeral fallback also failed")
            try:
                return await interaction.followup.send(
                    content=f"⚠️ Couldn't display this here ({type(exc).__name__}: {exc}).",
                    ephemeral=True,
                )
            except Exception:
                return None


async def update_lobby_message_closed(
    bot,
    lobby_service,
    reason: str = "Lobby Closed",
    guild_id: int | None = None,
    lobby_kind=None,
) -> None:
    """Update the channel message embed to show lobby/match is closed.

    Shared between match and lobby commands to avoid duplication.
    """
    from domain.models.lobby import LobbyKind

    kind = LobbyKind.normalize(lobby_kind)
    message_id = lobby_service.get_lobby_message_id(
        guild_id=guild_id,
        lobby_kind=kind,
    )
    channel_id = lobby_service.get_lobby_channel_id(
        guild_id=guild_id,
        lobby_kind=kind,
    )
    if not message_id or not channel_id:
        return

    try:
        channel = bot.get_channel(channel_id)
        if not channel:
            channel = await bot.fetch_channel(channel_id)
        message = channel.get_partial_message(message_id)

        import discord
        embed = discord.Embed(
            title=f"\U0001f6ab {kind.label} — {reason}",
            description=f"{kind.display_name} has been closed.",
            color=discord.Color.dark_grey(),
        )
        await message.edit(embed=embed, view=None)
    except Exception as exc:
        logger.warning(f"Failed to update channel message as closed: {exc}")
