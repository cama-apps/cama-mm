import io
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import commands.blame_luke as blame_commands
from services.player_service import PlayerService


class _FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self.deferred: dict | None = None

    async def send_message(self, content=None, **kwargs):
        self.messages.append({"content": content, **kwargs})

    async def defer(self, **kwargs):
        self.deferred = kwargs


class _FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(self, content=None, **kwargs):
        self.messages.append({"content": content, **kwargs})


class _FakeInteraction:
    def __init__(self, *, user_id=42, display_name="Accuser", guild_id=123):
        self.user = SimpleNamespace(id=user_id, display_name=display_name)
        self.guild = SimpleNamespace(id=guild_id)
        self.response = _FakeResponse()
        self.followup = _FakeFollowup()


def _player_service(*, charged=True, player=object(), balance=100):
    return SimpleNamespace(
        try_spend=MagicMock(return_value=charged),
        get_player=MagicMock(return_value=player),
        get_balance=MagicMock(return_value=balance),
        adjust_balance=MagicMock(return_value=balance),
    )


def test_player_service_try_spend_uses_atomic_repository_debit():
    repository = SimpleNamespace(try_debit=MagicMock(return_value=True))
    service = PlayerService.__new__(PlayerService)
    service.player_repo = repository

    result = service.try_spend(
        99,
        123,
        10,
        source="blame_luke",
        actor_id=99,
    )

    assert result is True
    repository.try_debit.assert_called_once_with(
        99,
        123,
        10,
        source="blame_luke",
        actor_id=99,
        related_type=None,
        related_id=None,
        reason=None,
        metadata=None,
    )


def test_player_service_try_spend_rejects_negative_amount():
    repository = SimpleNamespace(try_debit=MagicMock())
    service = PlayerService.__new__(PlayerService)
    service.player_repo = repository

    with pytest.raises(ValueError, match="cannot be negative"):
        service.try_spend(99, 123, -1)

    repository.try_debit.assert_not_called()


@pytest.mark.asyncio
async def test_command_posts_public_button_anyone_can_use():
    service = _player_service()
    cog = blame_commands.BlameLukeCommands(SimpleNamespace(), service)
    interaction = _FakeInteraction(user_id=1)

    await cog.blameluke.callback(cog, interaction)

    message = interaction.response.messages[-1]
    assert message.get("ephemeral") is not True
    assert message["view"].player_service is service
    assert message["view"].blame_luke.label == "Blame Luke"
    assert message["view"].timeout is None
    assert message["view"].is_persistent()
    assert message["view"].blame_luke.custom_id == "blame_luke:investigate"
    assert "10" not in message["embed"].description
    assert "JC" not in message["embed"].description
    assert message["embed"].title == "Ministry of Accountability"
    assert "Anyone can press it" in message["embed"].footer.text


@pytest.mark.asyncio
async def test_button_charges_clicker_and_posts_public_result(monkeypatch):
    reason = blame_commands.BLAME_LUKE_REASONS[3]
    service = _player_service()
    view = blame_commands.BlameLukeView(player_service=service)
    interaction = _FakeInteraction(user_id=99, display_name="Not the launcher")
    monkeypatch.setattr(blame_commands.random, "randrange", lambda _: 3)
    monkeypatch.setattr(
        blame_commands,
        "create_blame_luke_gif",
        lambda selected: io.BytesIO(b"GIF89a blame"),
    )
    monkeypatch.setattr(blame_commands, "safe_defer", AsyncMock(return_value=True))

    async def _safe_followup(target, **kwargs):
        await target.followup.send(**kwargs)

    monkeypatch.setattr(blame_commands, "safe_followup", _safe_followup)

    await view.blame_luke.callback(interaction)

    service.try_spend.assert_called_once_with(
        99,
        123,
        10,
        source="blame_luke",
        actor_id=99,
        related_type="blame_luke",
        related_id=3,
        reason="Blame Luke animation",
    )
    result = interaction.followup.messages[-1]
    assert result["ephemeral"] is False
    assert reason in result["embed"].description
    assert result["embed"].title == "📌 Finding of the Ministry of Accountability"
    assert "Not the launcher" in result["embed"].footer.text
    assert result["file"].filename == "blame_luke.gif"


@pytest.mark.asyncio
async def test_button_rejects_insufficient_funds_without_rendering(monkeypatch):
    service = _player_service(charged=False, balance=7)
    view = blame_commands.BlameLukeView(player_service=service)
    interaction = _FakeInteraction(user_id=99)
    render = MagicMock()
    monkeypatch.setattr(blame_commands, "create_blame_luke_gif", render)

    await view.blame_luke.callback(interaction)

    message = interaction.response.messages[-1]
    assert message["ephemeral"] is True
    assert message["content"] == "The Ministry rejected your funding request."
    assert "10" not in message["content"]
    assert "JC" not in message["content"]
    render.assert_not_called()
    service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_button_tells_unregistered_clicker_to_register():
    service = _player_service(charged=False, player=None, balance=0)
    view = blame_commands.BlameLukeView(player_service=service)
    interaction = _FakeInteraction(user_id=99)

    await view.blame_luke.callback(interaction)

    message = interaction.response.messages[-1]
    assert message["ephemeral"] is True
    assert "/player register" in message["content"]
    service.get_balance.assert_not_called()


@pytest.mark.asyncio
async def test_render_failure_refunds_clicker(monkeypatch):
    service = _player_service()
    view = blame_commands.BlameLukeView(player_service=service)
    interaction = _FakeInteraction(user_id=99)
    monkeypatch.setattr(
        blame_commands,
        "create_blame_luke_gif",
        MagicMock(side_effect=RuntimeError("render broke")),
    )
    monkeypatch.setattr(blame_commands, "safe_defer", AsyncMock(return_value=True))

    async def _safe_followup(target, **kwargs):
        await target.followup.send(**kwargs)

    monkeypatch.setattr(blame_commands, "safe_followup", _safe_followup)

    await view.blame_luke.callback(interaction)

    service.adjust_balance.assert_called_once_with(
        99,
        123,
        10,
        source="blame_luke_refund",
        actor_id=99,
        related_type="blame_luke",
        reason="Blame Luke delivery refund",
    )
    assert "refunded" in interaction.followup.messages[-1]["content"]
    assert "10" not in interaction.followup.messages[-1]["content"]
    assert "JC" not in interaction.followup.messages[-1]["content"]


@pytest.mark.asyncio
async def test_setup_registers_restart_safe_persistent_view():
    service = _player_service()
    bot = SimpleNamespace(
        player_service=service,
        add_cog=AsyncMock(),
        add_view=MagicMock(),
    )

    await blame_commands.setup(bot)

    bot.add_cog.assert_awaited_once()
    persistent_view = bot.add_view.call_args.args[0]
    assert isinstance(persistent_view, blame_commands.BlameLukeView)
    assert persistent_view.timeout is None
    assert persistent_view.is_persistent()
