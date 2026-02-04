"""Tests for admin /setinitialrating command."""

import asyncio
import types

import pytest

from commands.admin import AdminCommands


class FakeRepo:
    def __init__(self, *, exists=True, game_count=0, rating_data=None):
        self.exists = exists
        self.game_count = game_count
        self.rating_data = rating_data
        self.updates = []

    def get_by_id(self, _id, guild_id=None):
        return object() if self.exists else None

    def get_game_count(self, _id):
        return self.game_count

    def get_glicko_rating(self, _id):
        return self.rating_data

    def update_glicko_rating(self, discord_id, rating, rd, vol):
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
    repo = FakeRepo(game_count=2, rating_data=(1500.0, 100.0, 0.07))
    admin_cmd = AdminCommands(bot=None, lobby_service=None, player_repo=repo, loan_service=None, bankruptcy_service=None)

    # Monkeypatch permission check to allow
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction(user_id=999)
    target_user = types.SimpleNamespace(id=42, mention="<@42>")

    await admin_cmd.setinitialrating.callback(admin_cmd, interaction, target_user, 2500.0)

    assert repo.updates, "update_glicko_rating should be called"
    _pid, rating, rd, vol = repo.updates[0]
    assert rating == 2500.0
    assert rd == 100.0  # RD should be preserved from existing rating_data
    assert vol == 0.07
    assert any("Set initial rating" in msg for msg, _ep in interaction.response_messages)
    assert any("RD kept at 100.0" in msg for msg, _ep in interaction.response_messages)


@pytest.mark.asyncio
async def test_setinitialrating_rejects_too_many_games(monkeypatch):
    repo = FakeRepo(game_count=1000, rating_data=None)
    admin_cmd = AdminCommands(bot=None, lobby_service=None, player_repo=repo, loan_service=None, bankruptcy_service=None)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _i: True)

    interaction = DummyInteraction()
    target_user = types.SimpleNamespace(id=7, mention="<@7>")

    await admin_cmd.setinitialrating.callback(admin_cmd, interaction, target_user, 1200.0)

    assert not repo.updates, "Should not update rating when too many games"
    assert any("too many games" in msg.lower() for msg, _ep in interaction.response_messages)

