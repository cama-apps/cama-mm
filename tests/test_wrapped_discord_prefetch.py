"""Tests for concurrent Discord profile prefetching in Wrapped."""

import asyncio
from types import SimpleNamespace

import pytest

from commands.wrapped import _prefetch_discord_profiles


class _Avatar:
    def __init__(self, discord_id, tracker=None, *, fails=False):
        self.discord_id = discord_id
        self.tracker = tracker
        self.fails = fails

    async def read(self):
        if self.tracker:
            await self.tracker.run()
        if self.fails:
            raise RuntimeError("avatar unavailable")
        return f"avatar-{self.discord_id}".encode()


class _ConcurrencyTracker:
    def __init__(self):
        self.active = 0
        self.peak = 0

    async def run(self):
        self.active += 1
        self.peak = max(self.peak, self.active)
        await asyncio.sleep(0.005)
        self.active -= 1


class _Guild:
    def __init__(self, members, *, cached_ids=(), tracker=None, failing_ids=()):
        self.members = members
        self.cached_ids = set(cached_ids)
        self.tracker = tracker
        self.failing_ids = set(failing_ids)
        self.fetch_calls = []

    def get_member(self, discord_id):
        if discord_id in self.cached_ids:
            return self.members[discord_id]
        return None

    async def fetch_member(self, discord_id):
        self.fetch_calls.append(discord_id)
        if self.tracker:
            await self.tracker.run()
        if discord_id in self.failing_ids:
            raise RuntimeError("member unavailable")
        return self.members[discord_id]


@pytest.mark.asyncio
async def test_prefetch_profiles_reuses_members_for_names_and_avatars():
    members = {
        1: SimpleNamespace(display_name="One", avatar=_Avatar(1)),
        2: SimpleNamespace(display_name="Two", avatar=_Avatar(2)),
        3: SimpleNamespace(display_name="Three", avatar=None),
    }
    guild = _Guild(members, cached_ids={1})

    display_names, avatars = await _prefetch_discord_profiles(
        guild,
        {1, 2, 3},
        {1, 2},
    )

    assert display_names == {1: "One", 2: "Two", 3: "Three"}
    assert avatars == {1: b"avatar-1", 2: b"avatar-2"}
    assert sorted(guild.fetch_calls) == [2, 3]


@pytest.mark.asyncio
async def test_prefetch_profiles_bounds_all_network_concurrency():
    tracker = _ConcurrencyTracker()
    members = {
        discord_id: SimpleNamespace(
            display_name=str(discord_id),
            avatar=_Avatar(discord_id, tracker),
        )
        for discord_id in range(12)
    }
    guild = _Guild(members, tracker=tracker)

    display_names, avatars = await _prefetch_discord_profiles(
        guild,
        set(members),
        set(members),
        max_concurrency=3,
    )

    assert len(display_names) == len(members)
    assert len(avatars) == len(members)
    assert tracker.peak == 3


@pytest.mark.asyncio
async def test_prefetch_profiles_isolates_member_and_avatar_failures():
    members = {
        1: SimpleNamespace(display_name="One", avatar=_Avatar(1, fails=True)),
        2: SimpleNamespace(display_name="Two", avatar=_Avatar(2)),
    }
    guild = _Guild(members, cached_ids={1}, failing_ids={2})

    display_names, avatars = await _prefetch_discord_profiles(
        guild,
        {1, 2},
        {1, 2},
    )

    assert display_names == {1: "One"}
    assert avatars == {}
