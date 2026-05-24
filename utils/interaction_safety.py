"""
Utilities for safely interacting with Discord responses/followups.
"""

import logging

import discord

logger = logging.getLogger("cama_bot.utils.interaction_safety")


async def safe_defer(interaction: discord.Interaction, *, ephemeral: bool = False) -> bool:
    """
    Defer the interaction if it is still valid.

    Returns True when the defer succeeded (or the response already exists),
    False when the interaction is no longer valid.
    """
    try:
        await interaction.response.defer(ephemeral=ephemeral)
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
    bot, lobby_service, reason: str = "Lobby Closed", guild_id: int | None = None
) -> None:
    """Update the channel message embed to show lobby/match is closed.

    Shared between match and lobby commands to avoid duplication.
    """
    message_id = lobby_service.get_lobby_message_id(guild_id=guild_id)
    channel_id = lobby_service.get_lobby_channel_id(guild_id=guild_id)
    if not message_id or not channel_id:
        return

    try:
        channel = bot.get_channel(channel_id)
        if not channel:
            channel = await bot.fetch_channel(channel_id)
        message = await channel.fetch_message(message_id)

        import discord
        embed = discord.Embed(
            title=f"\U0001f6ab {reason}",
            description="This lobby has been closed.",
            color=discord.Color.dark_grey(),
        )
        await message.edit(embed=embed, view=None)
    except Exception as exc:
        logger.warning(f"Failed to update channel message as closed: {exc}")
