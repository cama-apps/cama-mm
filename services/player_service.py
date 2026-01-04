"""
Player-facing business logic (registration, roles, stats).
"""

from domain.models.player import Player
from opendota_integration import OpenDotaAPI
from rating_system import CamaRatingSystem
from repositories.interfaces import IPlayerRepository

STEAM_ID64_OFFSET = 76561197960265728


class PlayerService:
    """Encapsulates registration, role updates, and player stats."""

    def __init__(self, player_repo: IPlayerRepository):
        self.player_repo = player_repo
        self.rating_system = CamaRatingSystem()

    @staticmethod
    def _validate_steam_id(steam_id: int):
        if steam_id <= 0 or steam_id > 2147483647:
            raise ValueError("Invalid Steam ID. Must be Steam32 (positive, 32-bit).")

    def register_player(self, discord_id: int, discord_username: str, steam_id: int) -> dict:
        """
        Register a new player and seed their rating.

        Returns a dict with display-friendly values (cama_rating, uncertainty, dotabuff_url).
        """
        self._validate_steam_id(steam_id)

        existing = self.player_repo.get_by_id(discord_id)
        if existing:
            raise ValueError("Player already registered.")

        api = OpenDotaAPI()
        player_data = api.get_player_data(steam_id)
        if not player_data:
            raise ValueError("Could not fetch player data from OpenDota.")

        mmr = api.get_player_mmr(steam_id)
        glicko_player = self.rating_system.create_player_from_mmr(mmr)

        steam_id64 = steam_id + STEAM_ID64_OFFSET
        dotabuff_url = f"https://www.dotabuff.com/players/{steam_id64}"

        self.player_repo.add(
            discord_id=discord_id,
            discord_username=discord_username,
            dotabuff_url=dotabuff_url,
            initial_mmr=mmr,
            glicko_rating=glicko_player.rating,
            glicko_rd=glicko_player.rd,
            glicko_volatility=glicko_player.vol,
        )

        cama_rating = self.rating_system.rating_to_display(glicko_player.rating)
        uncertainty = self.rating_system.get_rating_uncertainty_percentage(glicko_player.rd)

        return {
            "cama_rating": cama_rating,
            "uncertainty": uncertainty,
            "dotabuff_url": dotabuff_url,
            "mmr": mmr,
        }

    def set_roles(self, discord_id: int, roles: list[str]):
        """Persist preferred roles for a player."""
        player = self.player_repo.get_by_id(discord_id)
        if not player:
            raise ValueError("Player not registered.")
        self.player_repo.update_roles(discord_id, roles)

    def get_player(self, discord_id: int) -> Player | None:
        """Fetch a Player model by Discord ID."""
        return self.player_repo.get_by_id(discord_id)

    def get_balance(self, discord_id: int) -> int:
        """Return the player's current jopacoin balance."""
        return self.player_repo.get_balance(discord_id)

    def get_stats(self, discord_id: int) -> dict:
        """Return stats payload for a player."""
        player = self.player_repo.get_by_id(discord_id)
        if not player:
            raise ValueError("Player not registered.")

        cama_rating = None
        uncertainty = None
        if player.glicko_rating is not None:
            cama_rating = self.rating_system.rating_to_display(player.glicko_rating)
            uncertainty = self.rating_system.get_rating_uncertainty_percentage(
                player.glicko_rd or 350
            )

        total_games = player.wins + player.losses
        win_rate = (player.wins / total_games * 100) if total_games > 0 else None

        return {
            "player": player,
            "cama_rating": cama_rating,
            "uncertainty": uncertainty,
            "win_rate": win_rate,
            "jopacoin_balance": self.player_repo.get_balance(discord_id),
        }
