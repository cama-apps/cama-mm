"""Private notifications for moderation terms completed by a recorded match."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.match import MatchCommands


@pytest.mark.asyncio
async def test_match_completion_events_skip_lowprio_dms_but_notify_suspensions():
    lowprio_target = SimpleNamespace(send=AsyncMock())
    suspension_target = SimpleNamespace(send=AsyncMock())
    members = {101: lowprio_target, 202: suspension_target}
    service = MagicMock()
    service.get_completion_events_for_match.return_value = [
        SimpleNamespace(
            discord_id=101,
            event_type=SimpleNamespace(value="lowprio_complete"),
        ),
        SimpleNamespace(
            discord_id=202,
            event_type=SimpleNamespace(value="complete"),
        ),
    ]
    bot = SimpleNamespace(
        moderation_service=service,
        get_user=lambda _discord_id: None,
    )
    guild = SimpleNamespace(get_member=MagicMock(side_effect=members.get))
    cog = MatchCommands(
        bot,
        lobby_service=None,
        match_service=None,
        player_service=None,
    )

    await cog._notify_moderation_completions(guild, 77, 314)

    service.get_completion_events_for_match.assert_called_once_with(77, 314)
    lowprio_target.send.assert_not_awaited()
    suspension_target.send.assert_awaited_once()
    guild.get_member.assert_called_once_with(202)
    assert "lobby suspension" in suspension_target.send.await_args.args[0]
    assert "#314" in suspension_target.send.await_args.args[0]
