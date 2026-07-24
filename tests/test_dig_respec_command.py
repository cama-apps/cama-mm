from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import commands.dig as dig_commands


@pytest.mark.asyncio
async def test_dig_respec_debits_and_surfaces_returned_points(monkeypatch):
    monkeypatch.setattr(
        dig_commands,
        "require_dig_channel",
        AsyncMock(return_value=True),
    )
    monkeypatch.setattr(
        dig_commands,
        "_check_registered",
        AsyncMock(return_value=object()),
    )
    monkeypatch.setattr(
        dig_commands,
        "safe_defer",
        AsyncMock(return_value=True),
    )
    safe_followup = AsyncMock()
    monkeypatch.setattr(dig_commands, "safe_followup", safe_followup)

    respec = Mock(return_value={
        "success": True,
        "cost": 50,
        "returned_points": 5,
        "stats": {
            "strength": 0,
            "smarts": 0,
            "stamina": 0,
            "stat_points": 12,
            "spent_points": 0,
            "unspent_points": 12,
        },
        "effects": {},
    })
    cog = dig_commands.DigCommands(
        SimpleNamespace(),
        SimpleNamespace(respec_miner_stats=respec),
    )
    interaction = SimpleNamespace(
        guild=SimpleNamespace(id=12345),
        user=SimpleNamespace(id=10001),
    )

    await cog.dig_respec.callback(cog, interaction)

    respec.assert_called_once_with(10001, 12345)
    embed = safe_followup.await_args.kwargs["embed"]
    assert embed.title == "S Points Reset"
    assert "5" in embed.description
    assert "12" in embed.description
    assert "50 JC" in embed.footer.text
    assert safe_followup.await_args.kwargs["ephemeral"] is True
