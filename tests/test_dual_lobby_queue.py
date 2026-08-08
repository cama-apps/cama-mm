"""
Tests for queueing in both lobby kinds at once.

A player may sit in All You Can Feed and Whine & Cheese simultaneously. When
one of them shuffles or drafts, the players it committed to a match lose their
seat in the other. Commands that used to read "the lobby you're in" have to ask
which one when the answer is no longer singular.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from discord import app_commands

from commands.lobby import LobbyCommands
from domain.models.lobby import LobbyKind
from domain.models.player import Player
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.conftest import TEST_GUILD_ID
from tests.fakes.lobby_repo import FakeLobbyRepo
from utils.lobby_selection import AMBIGUOUS_LOBBY_MESSAGE

LOWSKILL_RATING = 1200.0


class FakeFollowup:
    def __init__(self):
        self.messages = []

    async def send(self, content=None, ephemeral=None, embed=None, allowed_mentions=None):
        self.messages.append({"content": content, "ephemeral": ephemeral})


class FakeResponse:
    async def defer(self, ephemeral=False):
        return None

    async def send_message(self, content=None, embed=None, ephemeral=False):
        return None


class FakeInteraction:
    def __init__(self, user_id=1, guild_id=TEST_GUILD_ID):
        self.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = SimpleNamespace(id=guild_id)
        self.channel = SimpleNamespace(id=456)
        self.followup = FakeFollowup()
        self.response = FakeResponse()


class FakeThread:
    def __init__(self):
        self.id = 999
        self.sent = []

    async def send(self, content=None, embed=None, allowed_mentions=None):
        self.sent.append(content)
        return SimpleNamespace(id=1, jump_url="https://discord.test/1")


class FakeBot:
    """Bot whose channel lookups always resolve to one observable thread."""

    def __init__(self):
        self.thread = FakeThread()

    def get_channel(self, channel_id):
        return self.thread

    async def fetch_channel(self, channel_id):
        return self.thread

    def get_guild(self, guild_id):
        return None

    def get_cog(self, name):
        return None


class FakePlayerRepo:
    def __init__(self):
        self.players = {}

    def add_player(self, discord_id, guild_id=TEST_GUILD_ID, glicko_rating=LOWSKILL_RATING):
        player = Player(
            name=f"Player{discord_id}",
            mmr=3000,
            initial_mmr=3000,
            preferred_roles=["1", "2"],
            main_role="1",
            glicko_rating=glicko_rating,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            discord_id=discord_id,
        )
        self.players[(discord_id, guild_id)] = player
        return player

    def get_by_id(self, discord_id, guild_id=None):
        return self.players.get((discord_id, guild_id))

    def get_by_ids(self, ids, guild_id=None):
        return [
            self.players[(pid, guild_id)] for pid in ids if (pid, guild_id) in self.players
        ]


class FakePlayerService:
    def __init__(self, player_repo):
        self.player_repo = player_repo

    def get_player(self, discord_id, guild_id=None):
        return self.player_repo.players.get((discord_id, guild_id))


def make_services(player_ids=(), glicko_rating=LOWSKILL_RATING):
    """Real lobby manager/service over fakes, with both lobbies already open."""
    manager = LobbyManager(FakeLobbyRepo())
    player_repo = FakePlayerRepo()
    for discord_id in player_ids:
        player_repo.add_player(discord_id, glicko_rating=glicko_rating)
    service = LobbyService(manager, player_repo)
    for kind in LobbyKind:
        manager.get_or_create_lobby(guild_id=TEST_GUILD_ID, lobby_kind=kind)
    return manager, service, player_repo


def seat(service, discord_id, *kinds):
    """Join one player to each named lobby, asserting each join lands."""
    for kind in kinds:
        success, reason, _ = service.join_lobby(
            discord_id, guild_id=TEST_GUILD_ID, lobby_kind=kind
        )
        assert success, f"join {kind.value} rejected: {reason}"


@pytest.fixture
def monkeypatch_safe_defer(monkeypatch):
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))


# ---------------------------------------------------------------- membership


def test_player_holds_a_seat_in_both_lobbies():
    _, service, _ = make_services(player_ids=[1])

    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)

    for kind in LobbyKind:
        assert 1 in service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=kind).players


def test_lobby_kinds_for_player_lists_every_seat_in_declaration_order():
    _, service, _ = make_services(player_ids=[1, 2])

    seat(service, 1, LobbyKind.LOWSKILL, LobbyKind.OPEN)
    seat(service, 2, LobbyKind.LOWSKILL)

    assert service.get_lobby_kinds_for_player(1, TEST_GUILD_ID) == [
        LobbyKind.OPEN,
        LobbyKind.LOWSKILL,
    ]
    assert service.get_lobby_kinds_for_player(2, TEST_GUILD_ID) == [LobbyKind.LOWSKILL]
    assert service.get_lobby_kinds_for_player(3, TEST_GUILD_ID) == []


def test_rating_gate_still_bars_the_second_queue():
    """Dual queueing does not smuggle a high-rated player into Whine & Cheese."""
    _, service, _ = make_services(player_ids=[1], glicko_rating=1600.0)

    seat(service, 1, LobbyKind.OPEN)

    assert service.join_lobby(
        1, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL
    ) == (False, "rating_too_high", None)


# ------------------------------------------------------------ the pop's reach


@pytest.mark.asyncio
async def test_pop_evicts_only_the_players_who_got_a_match():
    """A benched player keeps their other seat — they did not get a game."""
    manager, service, _ = make_services(player_ids=[1, 2, 3])
    for discord_id in (1, 2, 3):
        seat(service, discord_id, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))

    evicted = await cog.evict_matched_players_from_other_lobbies(
        [1, 2],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert evicted == {LobbyKind.LOWSKILL: {1, 2}}
    lowskill = service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL)
    assert lowskill.players == {3}
    # The popped lobby is left for its own reset path to tear down.
    assert manager.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players == {1, 2, 3}


@pytest.mark.asyncio
async def test_pop_announces_the_departure_in_the_drained_lobbys_thread():
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    service.set_lobby_message_id(
        None, thread_id=999, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL
    )
    bot = FakeBot()
    cog = LobbyCommands(bot, service, FakePlayerService(FakePlayerRepo()))

    await cog.evict_matched_players_from_other_lobbies(
        [1],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert len(bot.thread.sent) == 1
    assert "<@1>" in bot.thread.sent[0]
    assert LobbyKind.OPEN.label in bot.thread.sent[0]


@pytest.mark.asyncio
async def test_pop_reopens_the_ready_check_notice_when_quorum_breaks():
    """Dropping below the threshold must let the notice fire again on refill."""
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    cog._readycheck_shuffle_notified[(TEST_GUILD_ID, LobbyKind.LOWSKILL)] = 555
    cog._readycheck_shuffle_notified[(TEST_GUILD_ID, LobbyKind.OPEN)] = 777

    await cog.evict_matched_players_from_other_lobbies(
        [1],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert (TEST_GUILD_ID, LobbyKind.LOWSKILL) not in cog._readycheck_shuffle_notified
    # The popped lobby's memo is irrelevant — reset_lobby clears it wholesale.
    assert cog._readycheck_shuffle_notified[(TEST_GUILD_ID, LobbyKind.OPEN)] == 777


@pytest.mark.asyncio
async def test_pop_keeps_the_memo_when_the_lobby_is_still_ready():
    """A lobby that never stopped being ready must not re-announce.

    Clearing the memo unconditionally let the eviction's own display resync
    immediately re-send "N players confirmed ready" for the same generation.
    """
    manager, service, _ = make_services(player_ids=list(range(1, 13)))
    for discord_id in range(1, 13):
        seat(service, discord_id, LobbyKind.LOWSKILL)
    seat(service, 1, LobbyKind.OPEN)
    service.set_readycheck_state(
        555, 456, set(range(1, 13)), {}, guild_id=TEST_GUILD_ID,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    for discord_id in range(1, 12):
        manager.add_readycheck_reaction(
            discord_id, f"<@{discord_id}>", guild_id=TEST_GUILD_ID,
            lobby_kind=LobbyKind.LOWSKILL,
        )
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    cog._readycheck_shuffle_notified[(TEST_GUILD_ID, LobbyKind.LOWSKILL)] = 555

    # Evicting player 1 drops confirmations 11 -> 10, still at the threshold.
    await cog.evict_matched_players_from_other_lobbies(
        [1],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert cog._readycheck_shuffle_notified[(TEST_GUILD_ID, LobbyKind.LOWSKILL)] == 555


@pytest.mark.asyncio
async def test_pop_is_a_no_op_when_nobody_was_double_queued():
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN)
    bot = FakeBot()
    cog = LobbyCommands(bot, service, FakePlayerService(FakePlayerRepo()))

    evicted = await cog.evict_matched_players_from_other_lobbies(
        [1],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert evicted == {}
    assert bot.thread.sent == []


@pytest.mark.asyncio
async def test_pop_survives_a_broken_thread_post():
    """Lobby bookkeeping must never unwind a match that already started."""
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    service.set_lobby_message_id(
        None, thread_id=999, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL
    )
    bot = FakeBot()
    bot.thread.send = AsyncMock(side_effect=RuntimeError("discord is down"))
    cog = LobbyCommands(bot, service, FakePlayerService(FakePlayerRepo()))

    evicted = await cog.evict_matched_players_from_other_lobbies(
        [1],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )

    assert evicted == {LobbyKind.LOWSKILL: {1}}
    assert service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL
    ).players == set()


# ------------------------------------------------------- two pops at once


def test_a_reserved_player_blocks_the_other_lobbys_shuffle():
    """The guild-wide reservation is what serialises two simultaneous pops."""
    _, service, _ = make_services(player_ids=[1, 2])
    for discord_id in (1, 2):
        seat(service, discord_id, LobbyKind.OPEN, LobbyKind.LOWSKILL)

    assert service.reserve_lobby_players([1, 2], TEST_GUILD_ID, LobbyKind.OPEN) is True
    assert (
        service.reserve_lobby_players([1, 2], TEST_GUILD_ID, LobbyKind.LOWSKILL) is False
    )


@pytest.mark.asyncio
async def test_eviction_keeps_blocking_the_other_shuffle_after_the_release():
    """The window a naive hook would leave: reservation dropped, seat still held.

    Evicting inside the reservation window closes it — once the players are out
    of the other lobby's roster, its reservation fails the subset check no
    matter when it is retried.
    """
    _, service, _ = make_services(player_ids=[1, 2])
    for discord_id in (1, 2):
        seat(service, discord_id, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))

    assert service.reserve_lobby_players([1, 2], TEST_GUILD_ID, LobbyKind.OPEN) is True
    await cog.evict_matched_players_from_other_lobbies(
        [1, 2],
        guild_id=TEST_GUILD_ID,
        popped_kind=LobbyKind.OPEN,
    )
    service.release_lobby_players([1, 2], TEST_GUILD_ID, LobbyKind.OPEN)

    assert (
        service.reserve_lobby_players([1, 2], TEST_GUILD_ID, LobbyKind.LOWSKILL) is False
    )


def test_the_other_lobbys_reservation_does_not_pin_this_seat():
    """Kind-scoped in-flight guard: an OPEN shuffle cannot freeze a LOWSKILL seat."""
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    assert service.reserve_lobby_players([1], TEST_GUILD_ID, LobbyKind.OPEN) is True

    assert service.leave_lobby(1, TEST_GUILD_ID, LobbyKind.LOWSKILL) is True
    assert service.leave_lobby(1, TEST_GUILD_ID, LobbyKind.OPEN) is False


# ------------------------------------------------------------------ commands


@pytest.mark.asyncio
async def test_leave_clears_every_lobby_at_once(monkeypatch_safe_defer):
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    interaction = FakeInteraction(user_id=1)

    await cog.leave.callback(cog, interaction)

    for kind in LobbyKind:
        assert 1 not in service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=kind).players
    content = interaction.followup.messages[-1]["content"]
    assert LobbyKind.OPEN.label in content
    assert LobbyKind.LOWSKILL.label in content


@pytest.mark.asyncio
async def test_leave_keeps_the_seat_its_own_shuffle_is_holding(monkeypatch_safe_defer):
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    assert service.reserve_lobby_players([1], TEST_GUILD_ID, LobbyKind.OPEN) is True
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    interaction = FakeInteraction(user_id=1)

    await cog.leave.callback(cog, interaction)

    assert 1 in service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players
    assert (
        1 not in service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.LOWSKILL).players
    )
    content = interaction.followup.messages[-1]["content"]
    assert f"Left {LobbyKind.LOWSKILL.label}" in content
    assert f"Still in {LobbyKind.OPEN.label}" in content


@pytest.mark.asyncio
async def test_readycheck_asks_which_lobby_when_the_caller_is_in_both(
    monkeypatch_safe_defer,
):
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    cog._execute_readycheck = AsyncMock(return_value=("ok", {}))
    interaction = FakeInteraction(user_id=1)

    await cog.readycheck.callback(cog, interaction, None)

    assert interaction.followup.messages[-1]["content"] == AMBIGUOUS_LOBBY_MESSAGE
    cog._execute_readycheck.assert_not_awaited()


@pytest.mark.asyncio
async def test_readycheck_accepts_an_explicit_lobby_from_a_dual_queued_caller(
    monkeypatch_safe_defer,
):
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.OPEN, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    cog._execute_readycheck = AsyncMock(return_value=("no_lobby", {}))
    interaction = FakeInteraction(user_id=1)

    await cog.readycheck.callback(
        cog,
        interaction,
        app_commands.Choice(name="Whine & Cheese", value="lowskill"),
    )

    cog._execute_readycheck.assert_awaited_once()
    assert (
        cog._execute_readycheck.await_args.kwargs["lobby_kind"] is LobbyKind.LOWSKILL
    )


@pytest.mark.asyncio
async def test_readycheck_still_infers_a_single_seat(monkeypatch_safe_defer):
    _, service, _ = make_services(player_ids=[1])
    seat(service, 1, LobbyKind.LOWSKILL)
    cog = LobbyCommands(FakeBot(), service, FakePlayerService(FakePlayerRepo()))
    cog._execute_readycheck = AsyncMock(return_value=("no_lobby", {}))
    interaction = FakeInteraction(user_id=1)

    await cog.readycheck.callback(cog, interaction, None)

    assert (
        cog._execute_readycheck.await_args.kwargs["lobby_kind"] is LobbyKind.LOWSKILL
    )
