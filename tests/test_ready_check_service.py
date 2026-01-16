"""Unit tests for ready check functionality."""

import time
from datetime import datetime, timedelta
from unittest.mock import Mock

import pytest

from domain.models.ready_check import ReadyCheck, ReadyCheckStatus, ReadyStatus


class TestReadyCheckModel:
    """Test ReadyCheck domain model."""

    def test_init_with_players(self):
        """Test initializing ready check with players."""
        player_ids = [1, 2, 3]
        player_ready_states = {pid: ReadyStatus.UNCONFIRMED for pid in player_ids}

        check = ReadyCheck(
            guild_id=123,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states=player_ready_states,
        )

        assert check.guild_id == 123
        assert check.timeout_seconds == 60
        assert len(check.player_ready_states) == 3
        assert all(s == ReadyStatus.UNCONFIRMED for s in check.player_ready_states.values())
        assert check.status == ReadyCheckStatus.ACTIVE

    def test_get_ready_players_empty(self):
        """Test getting ready players when none are ready."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.UNCONFIRMED,
                2: ReadyStatus.UNCONFIRMED,
            },
        )

        ready = check.get_ready_players()
        assert len(ready) == 0

    def test_get_ready_players_mixed(self):
        """Test getting ready players with mixed states."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.AUTO_READY,
                2: ReadyStatus.CONFIRMED,
                3: ReadyStatus.UNCONFIRMED,
            },
        )

        ready = check.get_ready_players()
        assert ready == {1, 2}

    def test_get_unready_players(self):
        """Test getting unready players."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.AUTO_READY,
                2: ReadyStatus.UNCONFIRMED,
                3: ReadyStatus.UNCONFIRMED,
            },
        )

        unready = check.get_unready_players()
        assert unready == {2, 3}

    def test_mark_ready_changes_state(self):
        """Test marking player as ready changes state."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        changed = check.mark_ready(1, auto=False)

        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.CONFIRMED
        assert 1 in check.get_ready_players()

    def test_mark_ready_auto_vs_manual(self):
        """Test marking ready with auto vs manual."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.UNCONFIRMED,
                2: ReadyStatus.UNCONFIRMED,
            },
        )

        check.mark_ready(1, auto=True)
        check.mark_ready(2, auto=False)

        assert check.player_ready_states[1] == ReadyStatus.AUTO_READY
        assert check.player_ready_states[2] == ReadyStatus.CONFIRMED

    def test_mark_ready_already_ready(self):
        """Test marking ready when already ready returns False."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.CONFIRMED},
        )

        changed = check.mark_ready(1, auto=False)

        assert changed is False
        assert check.player_ready_states[1] == ReadyStatus.CONFIRMED

    def test_mark_ready_invalid_player(self):
        """Test marking ready for non-existent player."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        changed = check.mark_ready(999, auto=False)

        assert changed is False

    def test_is_complete_all_ready(self):
        """Test completion detection when all ready."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.AUTO_READY,
                2: ReadyStatus.CONFIRMED,
            },
        )

        assert check.is_complete() is True

    def test_is_complete_partial(self):
        """Test completion detection when partially ready."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.CONFIRMED,
                2: ReadyStatus.UNCONFIRMED,
            },
        )

        assert check.is_complete() is False

    def test_is_timed_out_not_expired(self):
        """Test timeout detection before timeout."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={},
        )

        assert check.is_timed_out(datetime.now()) is False

    def test_is_timed_out_expired(self):
        """Test timeout detection after timeout."""
        start = datetime.now() - timedelta(seconds=61)
        check = ReadyCheck(
            guild_id=None,
            started_at=start,
            timeout_seconds=60,
            player_ready_states={},
        )

        assert check.is_timed_out(datetime.now()) is True

    def test_get_seconds_remaining_start(self):
        """Test seconds remaining at start."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={},
        )

        remaining = check.get_seconds_remaining(datetime.now())
        assert 59 <= remaining <= 60

    def test_get_seconds_remaining_half(self):
        """Test seconds remaining at halfway point."""
        start = datetime.now() - timedelta(seconds=30)
        check = ReadyCheck(
            guild_id=None,
            started_at=start,
            timeout_seconds=60,
            player_ready_states={},
        )

        remaining = check.get_seconds_remaining(datetime.now())
        assert 29 <= remaining <= 30

    def test_get_seconds_remaining_expired(self):
        """Test seconds remaining returns 0 after timeout."""
        start = datetime.now() - timedelta(seconds=70)
        check = ReadyCheck(
            guild_id=None,
            started_at=start,
            timeout_seconds=60,
            player_ready_states={},
        )

        remaining = check.get_seconds_remaining(datetime.now())
        assert remaining == 0

    def test_to_dict_serialization(self):
        """Test serialization to dict."""
        start = datetime(2024, 1, 1, 12, 0, 0)
        check = ReadyCheck(
            guild_id=123,
            started_at=start,
            timeout_seconds=60,
            player_ready_states={
                1: ReadyStatus.AUTO_READY,
                2: ReadyStatus.UNCONFIRMED,
            },
            status=ReadyCheckStatus.ACTIVE,
        )

        data = check.to_dict()

        assert data["guild_id"] == 123
        assert data["started_at"] == start.isoformat()
        assert data["timeout_seconds"] == 60
        assert data["player_ready_states"]["1"] == "auto_ready"
        assert data["player_ready_states"]["2"] == "unconfirmed"
        assert data["status"] == "active"

    def test_from_dict_deserialization(self):
        """Test deserialization from dict."""
        data = {
            "guild_id": 123,
            "started_at": "2024-01-01T12:00:00",
            "timeout_seconds": 60,
            "player_ready_states": {
                "1": "auto_ready",
                "2": "unconfirmed",
            },
            "status": "active",
            "voice_auto_ready_enabled": True,
        }

        check = ReadyCheck.from_dict(data)

        assert check.guild_id == 123
        assert check.started_at == datetime(2024, 1, 1, 12, 0, 0)
        assert check.timeout_seconds == 60
        assert check.player_ready_states[1] == ReadyStatus.AUTO_READY
        assert check.player_ready_states[2] == ReadyStatus.UNCONFIRMED
        assert check.status == ReadyCheckStatus.ACTIVE

    def test_round_trip_serialization(self):
        """Test serialization round trip."""
        original = ReadyCheck(
            guild_id=456,
            started_at=datetime.now(),
            timeout_seconds=120,
            player_ready_states={
                10: ReadyStatus.CONFIRMED,
                20: ReadyStatus.AUTO_READY,
                30: ReadyStatus.UNCONFIRMED,
            },
        )

        data = original.to_dict()
        restored = ReadyCheck.from_dict(data)

        assert restored.guild_id == original.guild_id
        assert restored.timeout_seconds == original.timeout_seconds
        assert restored.player_ready_states == original.player_ready_states
        assert restored.status == original.status


class TestReadyCheckService:
    """Test ReadyCheckService."""

    @pytest.fixture
    def lobby_service(self):
        """Mock lobby service."""
        service = Mock()
        service.leave_lobby = Mock()
        return service

    @pytest.fixture
    def ready_check_service(self, lobby_service):
        """Create ReadyCheckService instance."""
        from services.ready_check_service import ReadyCheckService

        return ReadyCheckService(
            lobby_service=lobby_service,
            timeout_seconds=60,
            voice_auto_ready_enabled=True,
        )

    def test_start_check_all_unconfirmed(self, ready_check_service):
        """Test starting ready check with no voice detection."""
        player_ids = [1, 2, 3]
        check = ready_check_service.start_check(
            guild_id=None, player_ids=player_ids, guild=None
        )

        assert len(check.player_ready_states) == 3
        assert all(
            s == ReadyStatus.UNCONFIRMED for s in check.player_ready_states.values()
        )
        assert check.status == ReadyCheckStatus.ACTIVE

    def test_start_check_with_voice_members(self, ready_check_service):
        """Test starting ready check with voice detection."""
        # Mock guild and members
        mock_guild = Mock()

        # Member 1: in voice, not deafened (should be auto-ready)
        mock_member1 = Mock()
        mock_member1.voice = Mock()
        mock_member1.voice.self_deaf = False
        mock_member1.voice.deaf = False

        # Member 2: not in voice (should be unconfirmed)
        mock_member2 = Mock()
        mock_member2.voice = None

        # Member 3: in voice but deafened (should be unconfirmed)
        mock_member3 = Mock()
        mock_member3.voice = Mock()
        mock_member3.voice.self_deaf = True
        mock_member3.voice.deaf = False

        def get_member(discord_id):
            if discord_id == 1:
                return mock_member1
            elif discord_id == 2:
                return mock_member2
            elif discord_id == 3:
                return mock_member3
            return None

        mock_guild.get_member = get_member

        player_ids = [1, 2, 3]
        check = ready_check_service.start_check(
            guild_id=123, player_ids=player_ids, guild=mock_guild
        )

        assert check.player_ready_states[1] == ReadyStatus.AUTO_READY
        assert check.player_ready_states[2] == ReadyStatus.UNCONFIRMED
        assert check.player_ready_states[3] == ReadyStatus.UNCONFIRMED

    def test_mark_ready_changes_state(self, ready_check_service):
        """Test marking player as ready."""
        player_ids = [1, 2, 3]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        success, updated_check = ready_check_service.mark_ready(
            guild_id=None, discord_id=1
        )

        assert success is True
        assert updated_check.player_ready_states[1] == ReadyStatus.CONFIRMED
        assert 1 in updated_check.get_ready_players()

    def test_mark_ready_already_ready(self, ready_check_service):
        """Test marking ready when already ready."""
        player_ids = [1]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)
        ready_check_service.mark_ready(guild_id=None, discord_id=1)

        # Try to mark ready again
        success, _ = ready_check_service.mark_ready(guild_id=None, discord_id=1)

        assert success is False

    def test_mark_ready_no_active_check(self, ready_check_service):
        """Test marking ready when no active check."""
        success, check = ready_check_service.mark_ready(guild_id=None, discord_id=1)

        assert success is False
        assert check is None

    def test_is_complete_all_ready(self, ready_check_service):
        """Test completion detection."""
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        ready_check_service.mark_ready(None, 1)
        ready_check_service.mark_ready(None, 2)

        check = ready_check_service.get_check(None)
        assert check.is_complete() is True

    def test_timeout_detection(self, ready_check_service):
        """Test timeout detection."""
        # Start with 1-second timeout
        ready_check_service.timeout_seconds = 1
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        # Wait for timeout
        time.sleep(1.1)

        is_timeout, check = ready_check_service.check_timeout(None)
        assert is_timeout is True
        assert check.status == ReadyCheckStatus.TIMEOUT

    def test_complete_check_cleanup(self, ready_check_service):
        """Test completing check cleans up state."""
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=123, player_ids=player_ids)

        check = ready_check_service.complete_check(123)

        assert check is not None
        assert check.status == ReadyCheckStatus.COMPLETED
        assert ready_check_service.get_check(123) is None

    def test_cancel_check_cleanup(self, ready_check_service):
        """Test cancelling check cleans up state."""
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=123, player_ids=player_ids)

        ready_check_service.cancel_check(123)

        assert ready_check_service.get_check(123) is None

    def test_kick_unready_players(self, ready_check_service, lobby_service):
        """Test kicking unready players from lobby."""
        player_ids = [1, 2, 3]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        # Mark player 1 as ready
        ready_check_service.mark_ready(None, 1)

        # Kick unready players
        kicked = ready_check_service.kick_unready_players(None)

        assert set(kicked) == {2, 3}
        assert lobby_service.leave_lobby.call_count == 2

    def test_start_check_cancels_existing(self, ready_check_service):
        """Test starting new check cancels existing."""
        player_ids = [1, 2]
        check1 = ready_check_service.start_check(guild_id=123, player_ids=player_ids)
        check2 = ready_check_service.start_check(guild_id=123, player_ids=player_ids)

        assert check1.status == ReadyCheckStatus.CANCELLED
        assert check2.status == ReadyCheckStatus.ACTIVE

    def test_guild_isolation(self, ready_check_service):
        """Test guild isolation of checks."""
        player_ids1 = [1, 2]
        player_ids2 = [3, 4]

        check1 = ready_check_service.start_check(guild_id=111, player_ids=player_ids1)
        check2 = ready_check_service.start_check(guild_id=222, player_ids=player_ids2)

        assert ready_check_service.get_check(111) == check1
        assert ready_check_service.get_check(222) == check2
        assert check1 != check2

    def test_message_id_tracking(self, ready_check_service):
        """Test message ID storage and retrieval."""
        guild_id = 123
        message_id = 456
        channel_id = 789

        ready_check_service.set_message_id(guild_id, message_id, channel_id)
        result = ready_check_service.get_message_id(guild_id)

        assert result == (message_id, channel_id)

    def test_clear_message_id(self, ready_check_service):
        """Test clearing message ID."""
        guild_id = 123
        ready_check_service.set_message_id(guild_id, 456, 789)
        ready_check_service.clear_message_id(guild_id)

        assert ready_check_service.get_message_id(guild_id) is None

    def test_get_end_timestamp_accuracy(self):
        """Test get_end_timestamp returns correct Unix timestamp."""
        start_time = datetime(2024, 1, 1, 12, 0, 0)
        timeout_seconds = 60

        check = ReadyCheck(
            guild_id=None,
            started_at=start_time,
            timeout_seconds=timeout_seconds,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        end_timestamp = check.get_end_timestamp()
        expected_timestamp = int(start_time.timestamp()) + timeout_seconds

        assert end_timestamp == expected_timestamp

    def test_mark_unready_changes_state(self):
        """Test marking player as unready changes state."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.CONFIRMED},
        )

        changed = check.mark_unready(1)

        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.UNCONFIRMED
        assert 1 in check.get_unready_players()
        assert 1 not in check.get_ready_players()

    def test_mark_unready_from_auto_ready(self):
        """Test marking auto-ready player as unready."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.AUTO_READY},
        )

        changed = check.mark_unready(1)

        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.UNCONFIRMED

    def test_mark_unready_already_unready(self):
        """Test marking unready when already unready returns False."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        changed = check.mark_unready(1)

        assert changed is False
        assert check.player_ready_states[1] == ReadyStatus.UNCONFIRMED

    def test_mark_unready_invalid_player(self):
        """Test marking unready for non-existent player."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        changed = check.mark_unready(999)

        assert changed is False

    def test_toggle_ready_unready(self):
        """Test toggling between ready and unready states."""
        check = ReadyCheck(
            guild_id=None,
            started_at=datetime.now(),
            timeout_seconds=60,
            player_ready_states={1: ReadyStatus.UNCONFIRMED},
        )

        # Mark ready
        changed = check.mark_ready(1, auto=False)
        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.CONFIRMED

        # Mark unready
        changed = check.mark_unready(1)
        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.UNCONFIRMED

        # Mark ready again
        changed = check.mark_ready(1, auto=False)
        assert changed is True
        assert check.player_ready_states[1] == ReadyStatus.CONFIRMED

    def test_mark_unready_service_integration(self, ready_check_service):
        """Test mark_unready through service layer."""
        player_ids = [1, 2, 3]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        # Mark player 1 as ready first
        ready_check_service.mark_ready(guild_id=None, discord_id=1)

        # Then mark unready
        success, updated_check = ready_check_service.mark_unready(
            guild_id=None, discord_id=1
        )

        assert success is True
        assert updated_check.player_ready_states[1] == ReadyStatus.UNCONFIRMED
        assert 1 not in updated_check.get_ready_players()

    def test_mark_unready_no_active_check(self, ready_check_service):
        """Test marking unready when no active check."""
        success, check = ready_check_service.mark_unready(guild_id=None, discord_id=1)

        assert success is False
        assert check is None

    def test_mark_ready_auto_parameter(self, ready_check_service):
        """Test marking ready with auto=True sets AUTO_READY status."""
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        success, updated_check = ready_check_service.mark_ready(
            guild_id=None, discord_id=1, auto=True
        )

        assert success is True
        assert updated_check.player_ready_states[1] == ReadyStatus.AUTO_READY
        assert 1 in updated_check.get_ready_players()

    def test_mark_ready_manual_parameter(self, ready_check_service):
        """Test marking ready with auto=False sets CONFIRMED status."""
        player_ids = [1, 2]
        ready_check_service.start_check(guild_id=None, player_ids=player_ids)

        success, updated_check = ready_check_service.mark_ready(
            guild_id=None, discord_id=1, auto=False
        )

        assert success is True
        assert updated_check.player_ready_states[1] == ReadyStatus.CONFIRMED
        assert 1 in updated_check.get_ready_players()
