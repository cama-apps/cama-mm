"""
Behavioural tests for the ready-check repost-when-stale + prune flow.

Covers `LobbyCommands._execute_readycheck`:
- ready checks are allowed below 10 players (with a "need N more" note),
- the trigger-er is auto-counted as ready,
- a sub-30-min refresh edits the existing message in place and preserves ✅,
- a 30+ min-old check is deleted and reposted fresh with confirmations reset,
- the repost prunes current AFK non-responders (regular and conditional,
  keeping the trigger-er, active players, and anyone who reacted),
- pruning that drops the roster below 10 still posts, with the shortfall note.

Time is injected (the stored post-time is overwritten), never frozen. Discord is
faked; services are real over the in-memory FakeLobbyRepo.
"""

import itertools
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

import discord
import pytest

from commands.lobby import LobbyCommands
from domain.models.player import Player
from services.lobby_manager_service import LobbyManagerService
from services.lobby_service import LobbyService
from tests.conftest import TEST_GUILD_ID
from tests.fakes.lobby_repo import FakeLobbyRepo

THREAD_ID = 9999
_msg_ids = itertools.count(1000)


class FakeMember:
    def __init__(self, pid, name, status, voice=None, activities=None):
        self.id = pid
        self.display_name = name
        self.status = status
        self.voice = voice
        self.activities = activities or []


class FakeGuild:
    def __init__(self, guild_id=TEST_GUILD_ID):
        self.id = guild_id
        self._members = {}

    def add_member(self, pid, status, name):
        self._members[pid] = FakeMember(pid, name, status)

    def get_member(self, pid):
        return self._members.get(pid)

    async def fetch_member(self, pid):
        member = self._members.get(pid)
        if member is None:
            raise Exception("member not found")  # mirrors a left-the-server user
        return member


class FakeMessage:
    def __init__(self, channel, embed=None):
        self.id = next(_msg_ids)
        self.channel = channel
        self.embed = embed
        self.jump_url = f"https://discord.com/channels/1/2/{self.id}"
        self.edited = False
        self.deleted = False

    async def edit(self, embed=None, content=None, allowed_mentions=None):
        self.edited = True
        if embed is not None:
            self.embed = embed

    async def add_reaction(self, emoji):
        pass

    async def delete(self):
        self.deleted = True

    async def remove_reaction(self, emoji, user):
        pass


class FakeThread:
    def __init__(self, thread_id=THREAD_ID):
        self.id = thread_id
        self.sent = []  # SimpleNamespace(content, embed, message)
        self.by_id = {}

    async def send(self, content=None, embed=None, allowed_mentions=None):
        msg = FakeMessage(self, embed=embed)
        self.by_id[msg.id] = msg
        self.sent.append(SimpleNamespace(content=content, embed=embed, message=msg))
        return msg

    async def fetch_message(self, message_id):
        return self.by_id[message_id]


class FakeBot:
    def __init__(self, thread):
        self._thread = thread

    def get_channel(self, channel_id):
        return self._thread

    async def fetch_channel(self, channel_id):
        return self._thread


class FakePlayerRepo:
    def __init__(self):
        self.players = {}

    def add_player(self, discord_id, guild_id=TEST_GUILD_ID):
        player = Player(
            name=f"P{discord_id}",
            mmr=3000,
            initial_mmr=3000,
            preferred_roles=["1"],
            main_role="1",
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            discord_id=discord_id,
        )
        self.players[(discord_id, guild_id)] = player
        return player

    def get_by_ids(self, ids, guild_id=None):
        return [self.players[(i, guild_id)] for i in ids if (i, guild_id) in self.players]


class FakePlayerService:
    def __init__(self, player_repo):
        self.player_repo = player_repo

    def get_player(self, discord_id, guild_id=None):
        return self.player_repo.players.get((discord_id, guild_id))


def _setup(monkeypatch, regular, conditional=None, present=None, guild_id=TEST_GUILD_ID):
    """Build a lobby + cog. ``regular``/``conditional`` map pid -> discord.Status.

    ``present`` lists pids that appear as guild members; pids omitted from it are
    treated as having left the server (get_member None, fetch_member raises). All
    join times are backdated 10 min so offline/idle players classify as AFK.
    """
    # No cooldown in these tests.
    monkeypatch.setattr(
        "commands.lobby.GLOBAL_RATE_LIMITER",
        SimpleNamespace(check=lambda **kw: SimpleNamespace(allowed=True, retry_after_seconds=0)),
    )
    conditional = conditional or {}

    mgr = LobbyManagerService(FakeLobbyRepo())
    player_repo = FakePlayerRepo()
    lobby_service = LobbyService(mgr, player_repo)
    player_service = FakePlayerService(player_repo)

    lobby = mgr.get_or_create_lobby(creator_id=0, guild_id=guild_id)
    guild = FakeGuild(guild_id)
    old = time.time() - 600
    all_status = {**regular, **conditional}
    present = set(all_status) if present is None else set(present)
    for pid, status in all_status.items():
        player_repo.add_player(pid, guild_id)
        if pid in conditional:
            lobby.add_conditional_player(pid)
        else:
            lobby.add_player(pid)
        lobby.player_join_times[pid] = old
        if pid in present:
            guild.add_member(pid, status=status, name=f"P{pid}")

    thread = FakeThread()
    bot = FakeBot(thread)
    mgr.set_lobby_message(message_id=None, channel_id=None, thread_id=THREAD_ID, guild_id=guild_id)
    cog = LobbyCommands(bot, lobby_service, player_service)
    cog._sync_lobby_displays = AsyncMock()
    return SimpleNamespace(
        mgr=mgr, lobby_service=lobby_service, lobby=lobby, guild=guild,
        bot=bot, thread=thread, cog=cog, guild_id=guild_id,
    )


def _make_stale(env):
    env.mgr.readycheck_created_ats[env.guild_id] = time.time() - 31 * 60


def _posted_embed(env):
    """The most recently posted ready-check embed (a send carrying an embed)."""
    embeds = [s.embed for s in env.thread.sent if s.embed is not None]
    return embeds[-1]


def _ping_texts(env):
    return [s.content for s in env.thread.sent if s.content]


ONLINE = discord.Status.online
OFFLINE = discord.Status.offline


@pytest.mark.asyncio
async def test_readycheck_allowed_under_10_players(monkeypatch):
    """No 10-player gate: a 4-person lobby posts, with a 'need 6 more' note."""
    env = _setup(monkeypatch, regular={1: ONLINE, 2: ONLINE, 3: ONLINE, 4: ONLINE})

    status, _ = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok"
    assert "need 6 more for a full game" in _posted_embed(env).description


@pytest.mark.asyncio
async def test_trigger_er_auto_counted_ready(monkeypatch):
    """Running the check confirms the trigger-er; they aren't pinged."""
    env = _setup(monkeypatch, regular={1: ONLINE, 2: ONLINE})

    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    reacted = env.lobby_service.get_readycheck_reacted(guild_id=env.guild_id)
    assert 1 in reacted  # invoker auto-readied
    ping = " ".join(_ping_texts(env))
    assert "<@2>" in ping and "<@1>" not in ping  # invoker excluded from the ping


@pytest.mark.asyncio
async def test_fresh_refresh_edits_in_place_and_preserves_reacted(monkeypatch):
    """Within 30 min: edit the same message, keep confirmations, prune nobody."""
    env = _setup(monkeypatch, regular=dict.fromkeys(range(1, 11), ONLINE))

    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)
    first_id = env.lobby_service.get_readycheck_message_id(guild_id=env.guild_id)
    # A different player confirms.
    env.lobby_service.add_readycheck_reaction(2, "<@2>", guild_id=env.guild_id)

    status, info = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok" and info["is_refresh"] is True and info["pruned_count"] == 0
    # Same message edited, not replaced or deleted.
    assert env.lobby_service.get_readycheck_message_id(guild_id=env.guild_id) == first_id
    assert env.thread.by_id[first_id].edited is True
    assert env.thread.by_id[first_id].deleted is False
    # Prior confirmation preserved; roster untouched.
    reacted = env.lobby_service.get_readycheck_reacted(guild_id=env.guild_id)
    assert 2 in reacted
    assert env.lobby.get_total_count() == 10


@pytest.mark.asyncio
async def test_stale_repost_deletes_old_and_resets_confirmations(monkeypatch):
    """30+ min old: delete the buried message, post a fresh one, reset ✅."""
    env = _setup(monkeypatch, regular=dict.fromkeys(range(1, 11), ONLINE))

    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)
    first_id = env.lobby_service.get_readycheck_message_id(guild_id=env.guild_id)
    env.lobby_service.add_readycheck_reaction(2, "<@2>", guild_id=env.guild_id)
    _make_stale(env)

    status, info = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok" and info["is_refresh"] is False
    new_id = env.lobby_service.get_readycheck_message_id(guild_id=env.guild_id)
    assert new_id != first_id  # a brand-new message
    assert env.thread.by_id[first_id].deleted is True  # old one removed
    # Confirmations reset to just the trigger-er (player 2's old ✅ is gone).
    assert env.lobby_service.get_readycheck_reacted(guild_id=env.guild_id) == {1: "<@1>"}


@pytest.mark.asyncio
async def test_stale_repost_keeps_afk_player_who_confirmed_old_check(monkeypatch):
    """A stale-check confirmation protects an AFK player from that check's sweep."""
    env = _setup(
        monkeypatch,
        regular={1: ONLINE, 2: OFFLINE, 3: OFFLINE, 4: ONLINE},
    )

    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)
    env.lobby_service.add_readycheck_reaction(2, "<@2>", guild_id=env.guild_id)
    _make_stale(env)

    status, info = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok"
    assert info["pruned_count"] == 1
    assert 2 in env.lobby.players
    assert 3 not in env.lobby.players
    assert env.lobby_service.get_readycheck_reacted(guild_id=env.guild_id) == {1: "<@1>"}
    note = next(text for text in _ping_texts(env) if "Removed (away during ready check)" in text)
    assert "<@3>" in note
    assert "<@2>" not in note


@pytest.mark.asyncio
async def test_stale_repost_prunes_afk_no_shows(monkeypatch):
    """Prune AFK non-responders; keep the trigger-er, active, and reacted."""
    env = _setup(
        monkeypatch,
        regular={1: OFFLINE, 2: OFFLINE, 4: OFFLINE, 5: ONLINE, 6: OFFLINE},
        conditional={3: OFFLINE},
        # Player 6 has left the server (not a guild member anymore).
        present={1, 2, 3, 4, 5},
    )
    # First check; player 4 confirms and is protected from this check's sweep.
    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)
    env.lobby_service.add_readycheck_reaction(4, "<@4>", guild_id=env.guild_id)
    _make_stale(env)

    status, info = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok"
    # Pruned: 2 (offline), 3 (conditional, offline), 6 (left server).
    assert info["pruned_count"] == 3
    assert 2 not in env.lobby.players
    assert 3 not in env.lobby.conditional_players
    assert 6 not in env.lobby.players
    # Kept: 1 (trigger-er), 4 (reacted), 5 (active).
    assert {1, 4, 5}.issubset(env.lobby.players)
    # Confirmations still reset to just the trigger-er for the new check.
    assert env.lobby_service.get_readycheck_reacted(guild_id=env.guild_id) == {1: "<@1>"}
    env.cog._sync_lobby_displays.assert_awaited()  # lobby display refreshed
    note = next(text for text in _ping_texts(env) if "Removed (away during ready check)" in text)
    assert "<@2>" in note and "<@3>" in note and "<@6>" in note
    assert "<@4>" not in note


@pytest.mark.asyncio
async def test_stale_prune_below_10_shows_shortfall_note(monkeypatch):
    """Pruning under 10 still posts, with the 'need N more' note."""
    regular = dict.fromkeys(range(1, 9), ONLINE)  # 1..8 active
    regular.update({9: OFFLINE, 10: OFFLINE, 11: OFFLINE, 12: OFFLINE})  # 4 AFK no-shows
    env = _setup(monkeypatch, regular=regular)

    await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)
    _make_stale(env)

    status, info = await env.cog._execute_readycheck(env.guild, env.guild_id, invoker_id=1)

    assert status == "ok" and info["pruned_count"] == 4
    assert env.lobby.get_total_count() == 8
    assert "need 2 more for a full game" in _posted_embed(env).description
