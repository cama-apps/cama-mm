"""
Tests for shuffle race condition prevention.

These tests verify that the per-guild asyncio lock prevents concurrent
shuffles from creating duplicate matches.
"""

import asyncio
import time

import pytest

from services.lobby_manager_service import LobbyManagerService


class MockLobbyRepo:
    """Mock lobby repository for testing."""

    def save_lobby_state(self, **kwargs):
        pass

    def clear_lobby_state(self, lobby_id):
        pass

    def load_lobby_state(self, lobby_id):
        return None


@pytest.fixture
def lobby_manager():
    """Create a LobbyManagerService with a mock repository."""
    return LobbyManagerService(MockLobbyRepo())


class TestShuffleLockPreventsRace:
    """Test that shuffle locks prevent concurrent operations."""

    @pytest.mark.asyncio
    async def test_shuffle_lock_prevents_concurrent_shuffles(self, lobby_manager):
        """Verify that two concurrent shuffles are serialized - one succeeds, one is blocked."""
        guild_id = 12345
        lock = lobby_manager.get_shuffle_lock(guild_id)

        # Simulate first shuffle acquiring lock
        await lock.acquire()
        lobby_manager.record_lock_acquired(guild_id)

        # Second shuffle should see lock is already held
        assert lock.locked() is True

        # Trying to acquire with timeout should fail
        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(lock.acquire(), timeout=0.1)

        # Release first lock
        lobby_manager.clear_lock_time(guild_id)
        lock.release()

        # Now second acquisition should succeed
        await asyncio.wait_for(lock.acquire(), timeout=0.1)
        assert lock.locked() is True
        lock.release()

    @pytest.mark.asyncio
    async def test_shuffle_locks_are_per_guild(self, lobby_manager):
        """Verify that different guilds have independent locks."""
        guild_1 = 11111
        guild_2 = 22222

        lock_1 = lobby_manager.get_shuffle_lock(guild_1)
        lock_2 = lobby_manager.get_shuffle_lock(guild_2)

        # They should be different lock objects
        assert lock_1 is not lock_2

        # Acquire lock for guild 1
        await lock_1.acquire()
        lobby_manager.record_lock_acquired(guild_1)

        # Guild 2 should still be able to acquire its lock
        acquired = False
        try:
            await asyncio.wait_for(lock_2.acquire(), timeout=0.1)
            acquired = True
            lock_2.release()
        except asyncio.TimeoutError:
            pass

        assert acquired is True, "Guild 2 should acquire lock while guild 1 holds its lock"

        # Cleanup
        lobby_manager.clear_lock_time(guild_1)
        lock_1.release()

    @pytest.mark.asyncio
    async def test_lock_released_on_exception(self, lobby_manager):
        """Verify that lock is released even when an exception occurs."""
        guild_id = 33333
        lock = lobby_manager.get_shuffle_lock(guild_id)

        # Simulate acquiring lock in a try/finally block like the real code
        await lock.acquire()
        lobby_manager.record_lock_acquired(guild_id)

        try:
            # Simulate exception during shuffle
            raise ValueError("Simulated shuffle error")
        except ValueError:
            pass
        finally:
            lobby_manager.clear_lock_time(guild_id)
            lock.release()

        # Lock should now be available
        assert lock.locked() is False

        # Should be able to acquire again
        await asyncio.wait_for(lock.acquire(), timeout=0.1)
        lock.release()

    @pytest.mark.asyncio
    async def test_stale_lock_auto_releases(self, lobby_manager):
        """Verify that stale locks (>60s) are automatically released."""
        guild_id = 44444
        lock = lobby_manager.get_shuffle_lock(guild_id)

        # Acquire lock
        await lock.acquire()

        # Set lock time to >60 seconds ago (simulate stale lock)
        normalized = guild_id if guild_id is not None else 0
        lobby_manager._shuffle_lock_times[normalized] = time.time() - 70  # 70 seconds ago

        # Check for stale lock should release it
        was_stale = lobby_manager._check_stale_lock(guild_id)
        assert was_stale is True
        assert lock.locked() is False

        # Should be able to acquire again
        await asyncio.wait_for(lock.acquire(), timeout=0.1)
        lock.release()

    @pytest.mark.asyncio
    async def test_non_stale_lock_not_released(self, lobby_manager):
        """Verify that recent locks are not released by stale check."""
        guild_id = 55555
        lock = lobby_manager.get_shuffle_lock(guild_id)

        # Acquire lock with current time
        await lock.acquire()
        lobby_manager.record_lock_acquired(guild_id)

        # Check for stale lock should NOT release it
        was_stale = lobby_manager._check_stale_lock(guild_id)
        assert was_stale is False
        assert lock.locked() is True

        # Cleanup
        lobby_manager.clear_lock_time(guild_id)
        lock.release()

    @pytest.mark.asyncio
    async def test_null_guild_id_normalized_to_zero(self, lobby_manager):
        """Verify that None guild_id is normalized to 0."""
        lock_none = lobby_manager.get_shuffle_lock(None)
        lock_zero = lobby_manager.get_shuffle_lock(0)

        # They should be the same lock object
        assert lock_none is lock_zero

    @pytest.mark.asyncio
    async def test_get_shuffle_lock_creates_new_lock(self, lobby_manager):
        """Verify that get_shuffle_lock creates a new lock if none exists."""
        guild_id = 99999

        # Should not exist yet
        assert guild_id not in lobby_manager._shuffle_locks

        # Get lock should create it
        lock = lobby_manager.get_shuffle_lock(guild_id)
        assert lock is not None
        assert isinstance(lock, asyncio.Lock)

        # Should be the same lock on second call
        lock2 = lobby_manager.get_shuffle_lock(guild_id)
        assert lock is lock2

    @pytest.mark.asyncio
    async def test_clear_lock_time_removes_entry(self, lobby_manager):
        """Verify that clear_lock_time removes the time entry."""
        guild_id = 66666
        normalized = guild_id

        # Record a lock time
        lobby_manager.record_lock_acquired(guild_id)
        assert normalized in lobby_manager._shuffle_lock_times

        # Clear it
        lobby_manager.clear_lock_time(guild_id)
        assert normalized not in lobby_manager._shuffle_lock_times

    @pytest.mark.asyncio
    async def test_clear_lock_time_handles_missing_entry(self, lobby_manager):
        """Verify that clear_lock_time handles missing entries gracefully."""
        guild_id = 77777

        # Should not raise even if entry doesn't exist
        lobby_manager.clear_lock_time(guild_id)
        # No assertion needed - just verify no exception


class TestConcurrentShuffleScenario:
    """Integration-style tests for concurrent shuffle scenarios."""

    @pytest.mark.asyncio
    async def test_concurrent_shuffle_simulation(self, lobby_manager):
        """Simulate two shuffles happening concurrently."""
        guild_id = 88888
        results = []

        async def simulate_shuffle(shuffle_id: int):
            """Simulate a shuffle operation."""
            lobby_manager._check_stale_lock(guild_id)
            lock = lobby_manager.get_shuffle_lock(guild_id)

            if lock.locked():
                results.append(f"shuffle_{shuffle_id}_rejected")
                return

            try:
                await asyncio.wait_for(lock.acquire(), timeout=0.5)
            except asyncio.TimeoutError:
                results.append(f"shuffle_{shuffle_id}_timeout")
                return

            lobby_manager.record_lock_acquired(guild_id)
            try:
                # Simulate shuffle work
                await asyncio.sleep(0.1)
                results.append(f"shuffle_{shuffle_id}_success")
            finally:
                lobby_manager.clear_lock_time(guild_id)
                lock.release()

        # Run two shuffles concurrently
        await asyncio.gather(
            simulate_shuffle(1),
            simulate_shuffle(2),
        )

        # One should succeed, one should be rejected/timeout
        success_count = sum(1 for r in results if "success" in r)
        rejected_count = sum(1 for r in results if "rejected" in r or "timeout" in r)

        assert success_count == 1, f"Expected exactly 1 success, got: {results}"
        assert rejected_count == 1, f"Expected exactly 1 rejection, got: {results}"
