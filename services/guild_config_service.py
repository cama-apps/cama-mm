"""
Service for managing per-guild configuration.

This service wraps GuildConfigRepository to provide a clean interface
for commands, keeping repository access out of the command layer.
"""

from repositories.interfaces import IGuildConfigRepository


class GuildConfigService:
    """
    Manages guild-specific configuration settings.

    Provides methods for:
    - League ID configuration (for match enrichment)
    - Auto-enrich toggle
    - AI features toggle
    """

    def __init__(self, guild_config_repo: IGuildConfigRepository):
        self.guild_config_repo = guild_config_repo

    def get_config(self, guild_id: int | None) -> dict | None:
        """Get full configuration for a guild."""
        normalized = guild_id if guild_id is not None else 0
        return self.guild_config_repo.get_config(normalized)

    def get_league_id(self, guild_id: int | None) -> int | None:
        """Get the Valve league ID for a guild."""
        normalized = guild_id if guild_id is not None else 0
        return self.guild_config_repo.get_league_id(normalized)

    def set_league_id(self, guild_id: int | None, league_id: int) -> None:
        """Set the Valve league ID for a guild."""
        normalized = guild_id if guild_id is not None else 0
        self.guild_config_repo.set_league_id(normalized, league_id)

    def is_auto_enrich_enabled(self, guild_id: int | None) -> bool:
        """Check if auto-enrichment is enabled for a guild. Defaults to True."""
        normalized = guild_id if guild_id is not None else 0
        return self.guild_config_repo.get_auto_enrich(normalized)

    def set_auto_enrich(self, guild_id: int | None, enabled: bool) -> None:
        """Enable or disable auto-enrichment for a guild."""
        normalized = guild_id if guild_id is not None else 0
        self.guild_config_repo.set_auto_enrich(normalized, enabled)

    def is_ai_enabled(self, guild_id: int | None) -> bool:
        """Check if AI features are enabled for a guild."""
        normalized = guild_id if guild_id is not None else 0
        return self.guild_config_repo.get_ai_enabled(normalized)

    def set_ai_enabled(self, guild_id: int | None, enabled: bool) -> None:
        """Enable or disable AI features for a guild."""
        normalized = guild_id if guild_id is not None else 0
        self.guild_config_repo.set_ai_enabled(normalized, enabled)
