"""Domain model for ready check functionality."""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Dict, Set


class ReadyStatus(Enum):
    """Player ready status."""

    UNCONFIRMED = "unconfirmed"  # Not ready yet
    AUTO_READY = "auto_ready"  # Auto-marked via voice channel
    CONFIRMED = "confirmed"  # Manually confirmed via button


class ReadyCheckStatus(Enum):
    """Overall ready check status."""

    ACTIVE = "active"
    COMPLETED = "completed"
    TIMEOUT = "timeout"
    CANCELLED = "cancelled"


@dataclass
class ReadyCheck:
    """Represents a ready check for lobby players."""

    guild_id: int | None
    started_at: datetime
    timeout_seconds: int
    player_ready_states: Dict[int, ReadyStatus] = field(default_factory=dict)
    status: ReadyCheckStatus = ReadyCheckStatus.ACTIVE
    voice_auto_ready_enabled: bool = True

    def get_ready_players(self) -> Set[int]:
        """Get set of ready player IDs (auto or confirmed)."""
        return {
            pid
            for pid, status in self.player_ready_states.items()
            if status in (ReadyStatus.AUTO_READY, ReadyStatus.CONFIRMED)
        }

    def get_unready_players(self) -> Set[int]:
        """Get set of unready player IDs."""
        return {
            pid
            for pid, status in self.player_ready_states.items()
            if status == ReadyStatus.UNCONFIRMED
        }

    def mark_ready(self, discord_id: int, auto: bool = False) -> bool:
        """
        Mark a player as ready.

        Args:
            discord_id: Player Discord ID
            auto: True if auto-marked via voice, False if button click

        Returns:
            True if state changed, False if already ready
        """
        if discord_id not in self.player_ready_states:
            return False

        current = self.player_ready_states[discord_id]
        if current in (ReadyStatus.AUTO_READY, ReadyStatus.CONFIRMED):
            return False  # Already ready

        new_status = ReadyStatus.AUTO_READY if auto else ReadyStatus.CONFIRMED
        self.player_ready_states[discord_id] = new_status
        return True

    def mark_unready(self, discord_id: int) -> bool:
        """
        Mark a player as unready (allows toggling ready status).

        Args:
            discord_id: Player Discord ID

        Returns:
            True if state changed, False if already unready
        """
        if discord_id not in self.player_ready_states:
            return False

        current = self.player_ready_states[discord_id]
        if current == ReadyStatus.UNCONFIRMED:
            return False  # Already unready

        self.player_ready_states[discord_id] = ReadyStatus.UNCONFIRMED
        return True

    def is_complete(self) -> bool:
        """Check if all players are ready."""
        return len(self.get_unready_players()) == 0

    def is_timed_out(self, current_time: datetime) -> bool:
        """Check if ready check has timed out."""
        elapsed = (current_time - self.started_at).total_seconds()
        return elapsed >= self.timeout_seconds

    def get_seconds_remaining(self, current_time: datetime) -> int:
        """Get seconds remaining before timeout."""
        elapsed = (current_time - self.started_at).total_seconds()
        remaining = max(0, self.timeout_seconds - elapsed)
        return int(remaining)

    def get_end_timestamp(self) -> int:
        """Get Unix timestamp when ready check ends (for Discord timestamp)."""
        return int(self.started_at.timestamp()) + self.timeout_seconds

    def to_dict(self) -> dict:
        """Serialize for storage."""
        return {
            "guild_id": self.guild_id,
            "started_at": self.started_at.isoformat(),
            "timeout_seconds": self.timeout_seconds,
            "player_ready_states": {
                str(pid): status.value
                for pid, status in self.player_ready_states.items()
            },
            "status": self.status.value,
            "voice_auto_ready_enabled": self.voice_auto_ready_enabled,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "ReadyCheck":
        """Deserialize from storage."""
        return cls(
            guild_id=data["guild_id"],
            started_at=datetime.fromisoformat(data["started_at"]),
            timeout_seconds=data["timeout_seconds"],
            player_ready_states={
                int(pid): ReadyStatus(status)
                for pid, status in data["player_ready_states"].items()
            },
            status=ReadyCheckStatus(data["status"]),
            voice_auto_ready_enabled=data.get("voice_auto_ready_enabled", True),
        )
