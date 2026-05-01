"""Tests for admin /setinitialrating command."""

import types

import pytest

from commands.admin import AdminCommands


class FakePlayerService:
    """Fake player service for admin command tests."""

    def __init__(self, *, exists=True, game_count=0, rating_data=None):
        self.exists = exists
        self._game_count = game_count
        self.rating_data = rating_data
        self.updates = []
        self.game_count_calls = 0

    def get_player(self, _id, guild_id=None):
        return object() if self.exists else None

    def get_game_count(self, _id, guild_id=None):
        self.game_count_calls += 1
        return self._game_count

    def get_glicko_rating(self, _id, guild_id=None):
        return self.rating_data

    def update_glicko_rating(self, discord_id, guild_id, rating, rd, vol):
        self.updates.append((discord_id, rating, rd, vol))


class DummyInteraction:
    def __init__(self, user_id=1, guild_id=123):
        self.user = types.SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = types.SimpleNamespace(id=guild_id) if guild_id else None
        self.response_messages = []

        class Resp:
            def __init__(self, outer):
                self.outer = outer

            async def send_message(self, content, ephemeral=False):
                self.outer.response_messages.append((content, ephemeral))

        self.response = Resp(self)


@pytest.mark.asyncio
async def test_setinitialrating_happy_path(monkeypatch):
    service = FakePlayerService(game_count=2, rating_data=(1500.0, 100.0, 0.07))
    admin_cmd = AdminCommands(bot=None, lobby_service=None, player_service=service, loan_service=None, bankruptcy_service=None)

    # Monkeypatch permission check to allow
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction(user_id=999)
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.setinitialrating.callback(admin_cmd, interaction, target_user, 2500.0)

    assert service.updates, "update_glicko_rating should be called"
    _pid, rating, rd, vol = service.updates[0]
    assert rating == 2500.0
    assert rd == 100.0  # RD should be preserved from existing rating_data
    assert vol == 0.07
    assert any("Set initial rating" in msg for msg, _ep in interaction.response_messages)
    assert any("RD kept at 100.0" in msg for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_setinitialrating_rejects_too_many_games(monkeypatch):
    service = FakePlayerService(game_count=1000, rating_data=None)
    admin_cmd = AdminCommands(bot=None, lobby_service=None, player_service=service, loan_service=None, bankruptcy_service=None)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=7, mention="<@7>")

    await admin_cmd.setinitialrating.callback(admin_cmd, interaction, target_user, 1200.0)

    assert not service.updates, "Should not update rating when too many games"
    assert any("too many games" in msg.lower() for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_adjust_rating_allows_many_games_and_preserves_rd(monkeypatch):
    service = FakePlayerService(game_count=1000, rating_data=(1500.0, 110.0, 0.08))
    admin_cmd = AdminCommands(
        bot=None,
        lobby_service=None,
        player_service=service,
        loan_service=None,
        bankruptcy_service=None,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.adjust_rating.callback(admin_cmd, interaction, target_user, 2400.0)

    assert service.game_count_calls == 0
    assert service.updates == [(42, 2400.0, 110.0, 0.08)]
    assert any("RD kept at 110.0" in msg for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_adjust_rd_preserves_rating_and_volatility(monkeypatch):
    service = FakePlayerService(game_count=1000, rating_data=(1512.0, 95.0, 0.07))
    admin_cmd = AdminCommands(
        bot=None,
        lobby_service=None,
        player_service=service,
        loan_service=None,
        bankruptcy_service=None,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.adjust_rd.callback(admin_cmd, interaction, target_user, 180.0)

    assert service.game_count_calls == 0
    assert service.updates == [(42, 1512.0, 180.0, 0.07)]
    assert any("rating kept at 1512" in msg for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_adjust_rating_rejects_non_admin(monkeypatch):
    service = FakePlayerService(rating_data=(1500.0, 100.0, 0.07))
    admin_cmd = AdminCommands(
        bot=None,
        lobby_service=None,
        player_service=service,
        loan_service=None,
        bankruptcy_service=None,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: False)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.adjust_rating.callback(admin_cmd, interaction, target_user, 2400.0)

    assert not service.updates
    assert any("Admin only" in msg for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_adjust_rd_rejects_invalid_rd(monkeypatch):
    service = FakePlayerService(rating_data=(1500.0, 100.0, 0.07))
    admin_cmd = AdminCommands(
        bot=None,
        lobby_service=None,
        player_service=service,
        loan_service=None,
        bankruptcy_service=None,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.adjust_rd.callback(admin_cmd, interaction, target_user, 351.0)

    assert not service.updates
    assert any("RD must be between 0 and 350" in msg for msg, _ep in interaction.response_messages)
