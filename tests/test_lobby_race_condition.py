"""
Tests for race condition prevention in /lobby command.

Verifies that concurrent /lobby calls result in only one Discord message being created.
"""

import asyncio

import pytest

from services.lobby_manager_service import LobbyManagerService
from tests.fakes.lobby_repo import FakeLobbyRepo


@pytest.fixture
def lobby_manager():
    """Create a LobbyManagerService with an in-memory fake repository."""
    return LobbyManagerService(FakeLobbyRepo())


@pytest.mark.asyncio
async def test_creation_lock_prevents_race_condition(lobby_manager):
    """Verify the lock prevents concurrent lobby creation.

    Uses an asyncio.Event gate instead of real sleeps so ordering is deterministic
    and the test does not flake under -n 4 parallel workers.
    """
    results = []
    creation_count = 0
    # Gate: first task holds the lock here until the second task is waiting on it.
    inside_gate = asyncio.Event()
    proceed_gate = asyncio.Event()

    async def simulate_lobby_creation(user_id: int):
        nonlocal creation_count
        async with lobby_manager.get_creation_lock():
            existing = lobby_manager.get_lobby()
            if existing:
                results.append(("existing", user_id))
                return

            # Signal that we are inside the lock, then wait for the test harness.
            inside_gate.set()
            await proceed_gate.wait()

            lobby_manager.get_or_create_lobby(creator_id=user_id)
            creation_count += 1
            results.append(("created", user_id))

    task1 = asyncio.create_task(simulate_lobby_creation(1))
    # Wait until task1 is inside the lock before launching task2.
    await inside_gate.wait()
    task2 = asyncio.create_task(simulate_lobby_creation(2))
    # Let both tasks proceed now.
    proceed_gate.set()
    await asyncio.gather(task1, task2)

    assert creation_count == 1
    assert len(results) == 2
    created_count = sum(1 for r in results if r[0] == "created")
    existing_count = sum(1 for r in results if r[0] == "existing")
    assert created_count == 1
    assert existing_count == 1


@pytest.mark.asyncio
async def test_lock_serializes_access(lobby_manager):
    """Verify that the lock serializes access properly.

    Uses asyncio.Event gates instead of wall-clock sleeps so the test is
    deterministic under parallel pytest workers (-n 4).
    """
    order = []
    task1_inside = asyncio.Event()
    task1_may_finish = asyncio.Event()

    async def task1_work():
        async with lobby_manager.get_creation_lock():
            order.append("task1_start")
            task1_inside.set()       # tell the harness we are inside
            await task1_may_finish.wait()  # hold the lock until released
            order.append("task1_end")

    async def task2_work():
        async with lobby_manager.get_creation_lock():
            order.append("task2_start")
            order.append("task2_end")

    t1 = asyncio.create_task(task1_work())
    # Wait until task1 holds the lock, *then* enqueue task2.
    await task1_inside.wait()
    t2 = asyncio.create_task(task2_work())
    # Give t2 a chance to queue up before releasing t1.
    await asyncio.sleep(0)
    task1_may_finish.set()
    await asyncio.gather(t1, t2)

    # task1 must complete before task2 starts (lock is exclusive).
    assert order == ["task1_start", "task1_end", "task2_start", "task2_end"]


@pytest.mark.asyncio
async def test_lock_property_returns_same_instance(lobby_manager):
    """Verify get_creation_lock returns the same lock instance per guild."""
    lock1 = lobby_manager.get_creation_lock()
    lock2 = lobby_manager.get_creation_lock()

    assert lock1 is lock2
    assert isinstance(lock1, asyncio.Lock)


@pytest.mark.asyncio
async def test_creation_locks_are_per_guild(lobby_manager):
    """Distinct guilds must get distinct creation locks."""
    lock_a = lobby_manager.get_creation_lock(guild_id=1)
    lock_b = lobby_manager.get_creation_lock(guild_id=2)

    assert lock_a is not lock_b
    # And the lock for guild=1 should be stable across calls.
    assert lobby_manager.get_creation_lock(guild_id=1) is lock_a


@pytest.mark.asyncio
async def test_multiple_concurrent_calls_only_one_creates(lobby_manager):
    """Five concurrent /lobby calls under the creation lock: exactly one creates.

    No sleeps — the lock itself provides the serialization guarantee.
    """
    creation_count = 0

    async def simulate_lobby_creation(user_id: int):
        nonlocal creation_count
        async with lobby_manager.get_creation_lock():
            existing = lobby_manager.get_lobby()
            if existing:
                return "existing"
            # Yield to the event loop so other tasks can attempt to acquire
            # the lock (they will block, proving exclusivity).
            await asyncio.sleep(0)
            lobby_manager.get_or_create_lobby(creator_id=user_id)
            creation_count += 1
            return "created"

    results = await asyncio.gather(*[simulate_lobby_creation(i) for i in range(5)])

    assert creation_count == 1
    assert results.count("created") == 1
    assert results.count("existing") == 4
