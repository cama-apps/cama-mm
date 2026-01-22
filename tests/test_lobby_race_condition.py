"""
Tests for race condition prevention in /lobby command.

Verifies that concurrent /lobby calls result in only one Discord message being created.
"""

import asyncio

import pytest

from services.lobby_manager_service import LobbyManagerService


class MockLobbyRepository:
    """Mock repository that does nothing for persistence."""

    def save_lobby_state(self, **kwargs):
        pass

    def clear_lobby_state(self, lobby_id):
        pass

    def load_lobby_state(self, lobby_id):
        return None


@pytest.fixture
def lobby_manager():
    """Create a LobbyManagerService with mock repository."""
    return LobbyManagerService(MockLobbyRepository())


@pytest.mark.asyncio
async def test_creation_lock_prevents_race_condition(lobby_manager):
    """Verify the lock prevents concurrent lobby creation."""
    results = []
    creation_count = 0

    async def simulate_lobby_creation(user_id: int):
        nonlocal creation_count
        async with lobby_manager.creation_lock:
            # Simulate checking if lobby exists
            existing = lobby_manager.get_lobby()
            if existing:
                results.append(("existing", user_id))
                return

            # Simulate delay during Discord message creation
            await asyncio.sleep(0.1)

            # Create lobby
            lobby_manager.get_or_create_lobby(creator_id=user_id)
            creation_count += 1
            results.append(("created", user_id))

    # Run two concurrent lobby creations
    await asyncio.gather(
        simulate_lobby_creation(1),
        simulate_lobby_creation(2),
    )

    # Only one should create, the other should see existing
    assert creation_count == 1
    assert len(results) == 2
    created_count = sum(1 for r in results if r[0] == "created")
    existing_count = sum(1 for r in results if r[0] == "existing")
    assert created_count == 1
    assert existing_count == 1


@pytest.mark.asyncio
async def test_without_lock_causes_race_condition():
    """Demonstrate that without the lock, race conditions occur."""
    mock_repo = MockLobbyRepository()
    manager = LobbyManagerService(mock_repo)
    creation_count = 0

    async def simulate_lobby_creation_no_lock(user_id: int):
        nonlocal creation_count
        # Check if lobby exists (without lock)
        existing = manager.get_lobby()
        if existing:
            return "existing"

        # Simulate delay during Discord message creation
        await asyncio.sleep(0.1)

        # Both coroutines will reach here before either creates
        manager.get_or_create_lobby(creator_id=user_id)
        creation_count += 1
        return "created"

    # Run two concurrent lobby creations WITHOUT lock
    results = await asyncio.gather(
        simulate_lobby_creation_no_lock(1),
        simulate_lobby_creation_no_lock(2),
    )

    # Without lock, both think they need to create (race condition)
    # Note: get_or_create_lobby is internally idempotent, so only one lobby object exists,
    # but in the real command handler, two Discord messages would be sent
    assert results.count("created") == 2  # Both think they created


@pytest.mark.asyncio
async def test_lock_serializes_access(lobby_manager):
    """Verify that the lock serializes access properly."""
    order = []

    async def access_with_lock(name: str, delay: float):
        async with lobby_manager.creation_lock:
            order.append(f"{name}_start")
            await asyncio.sleep(delay)
            order.append(f"{name}_end")

    # Start task1 first, then task2 while task1 is still running
    task1 = asyncio.create_task(access_with_lock("task1", 0.2))
    await asyncio.sleep(0.05)  # Let task1 acquire lock
    task2 = asyncio.create_task(access_with_lock("task2", 0.1))

    await asyncio.gather(task1, task2)

    # task1 should complete before task2 starts
    assert order == ["task1_start", "task1_end", "task2_start", "task2_end"]


@pytest.mark.asyncio
async def test_lock_property_returns_same_instance(lobby_manager):
    """Verify the creation_lock property returns the same lock instance."""
    lock1 = lobby_manager.creation_lock
    lock2 = lobby_manager.creation_lock

    assert lock1 is lock2
    assert isinstance(lock1, asyncio.Lock)


@pytest.mark.asyncio
async def test_multiple_concurrent_calls_only_one_creates(lobby_manager):
    """Test with more concurrent calls to stress the lock."""
    creation_count = 0

    async def simulate_lobby_creation(user_id: int):
        nonlocal creation_count
        async with lobby_manager.creation_lock:
            existing = lobby_manager.get_lobby()
            if existing:
                return "existing"

            await asyncio.sleep(0.05)  # Simulate Discord API latency
            lobby_manager.get_or_create_lobby(creator_id=user_id)
            creation_count += 1
            return "created"

    # Run 5 concurrent lobby creations
    results = await asyncio.gather(*[simulate_lobby_creation(i) for i in range(5)])

    # Only one should create
    assert creation_count == 1
    assert results.count("created") == 1
    assert results.count("existing") == 4
