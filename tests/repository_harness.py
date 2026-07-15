"""Repository-backed compatibility harness for older end-to-end tests."""

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository


class RepositoryTestDatabase(Database):
    """Keep legacy test setup concise while exercising canonical repositories."""

    def __init__(self, db_path: str | None = None):
        super().__init__(db_path)
        self.player_repo = PlayerRepository(self.db_path)
        self.match_repo = MatchRepository(self.db_path)

    @staticmethod
    def _guild_id(guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def add_player(
        self,
        discord_id: int,
        discord_username: str,
        dotabuff_url: str | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
        guild_id: int | None = None,
    ) -> None:
        self.player_repo.add(
            discord_id=discord_id,
            discord_username=discord_username,
            guild_id=self._guild_id(guild_id),
            dotabuff_url=dotabuff_url,
            initial_mmr=initial_mmr,
            preferred_roles=preferred_roles,
            main_role=main_role,
            glicko_rating=glicko_rating,
            glicko_rd=glicko_rd,
            glicko_volatility=glicko_volatility,
        )

    def update_player_glicko_rating(
        self,
        discord_id: int,
        rating: float,
        rd: float,
        volatility: float,
        guild_id: int | None = None,
    ) -> None:
        self.player_repo.update_glicko_rating(
            discord_id,
            self._guild_id(guild_id),
            rating,
            rd,
            volatility,
        )

    def get_player_glicko_rating(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> tuple[float, float, float] | None:
        return self.player_repo.get_glicko_rating(discord_id, self._guild_id(guild_id))

    def get_player(self, discord_id: int, guild_id: int | None = None):
        return self.player_repo.get_by_id(discord_id, self._guild_id(guild_id))

    def get_player_balance(self, discord_id: int, guild_id: int | None = None) -> int:
        return self.player_repo.get_balance(discord_id, self._guild_id(guild_id))

    def get_all_players(self, guild_id: int | None = None):
        return self.player_repo.get_all(self._guild_id(guild_id))

    def get_players_by_ids(
        self,
        discord_ids: list[int],
        guild_id: int | None = None,
    ):
        return self.player_repo.get_by_ids(discord_ids, self._guild_id(guild_id))

    def record_match(
        self,
        radiant_team_ids: list[int] | None = None,
        dire_team_ids: list[int] | None = None,
        winning_team: str | int | None = None,
        team1_ids: list[int] | None = None,
        team2_ids: list[int] | None = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
        guild_id: int | None = None,
    ) -> int:
        if isinstance(winning_team, int):
            if team1_ids is None or team2_ids is None:
                if radiant_team_ids is None or dire_team_ids is None:
                    raise ValueError("Old API requires team1_ids and team2_ids")
                team1_ids, team2_ids = radiant_team_ids, dire_team_ids
            winning_team_number = winning_team
        elif isinstance(winning_team, str):
            if winning_team not in {"radiant", "dire"}:
                raise ValueError(
                    f"winning_team must be 'radiant' or 'dire', got '{winning_team}'"
                )
            if radiant_team_ids is None or dire_team_ids is None:
                raise ValueError("New API requires radiant_team_ids and dire_team_ids")
            team1_ids, team2_ids = radiant_team_ids, dire_team_ids
            winning_team_number = 1 if winning_team == "radiant" else 2
        else:
            raise ValueError("winning_team must be 'radiant'/'dire' (str) or 1/2 (int)")

        normalized_guild = self._guild_id(guild_id)
        match_id = self.match_repo.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=winning_team_number,
            guild_id=normalized_guild,
            dotabuff_match_id=dotabuff_match_id,
            notes=notes,
        )
        winning_ids = team1_ids if winning_team_number == 1 else team2_ids
        losing_ids = team2_ids if winning_team_number == 1 else team1_ids
        self.player_repo.apply_match_outcome(winning_ids, losing_ids, normalized_guild)
        return match_id

    def get_exclusion_counts(
        self,
        discord_ids: list[int],
        guild_id: int | None = None,
    ) -> dict[int, int]:
        return self.player_repo.get_exclusion_counts(
            discord_ids,
            self._guild_id(guild_id),
        )

    def increment_exclusion_count(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> None:
        self.player_repo.increment_exclusion_count(discord_id, self._guild_id(guild_id))

    def increment_exclusion_count_half(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> None:
        self.player_repo.increment_exclusion_count_half(
            discord_id,
            self._guild_id(guild_id),
        )

    def decay_exclusion_count(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> None:
        self.player_repo.decay_exclusion_count(discord_id, self._guild_id(guild_id))

    def delete_player(self, discord_id: int, guild_id: int | None = None) -> bool:
        return self.player_repo.delete(discord_id, self._guild_id(guild_id))

    def delete_fake_users(self) -> int:
        with self.connection() as conn:
            guild_ids = [
                row["guild_id"]
                for row in conn.execute(
                    "SELECT DISTINCT guild_id FROM players WHERE discord_id < 0"
                )
            ]
        return sum(self.player_repo.delete_fake_users(guild_id) for guild_id in guild_ids)

    def clear_all_players(self) -> int:
        with self.connection() as conn:
            count = conn.execute("SELECT COUNT(*) FROM players").fetchone()[0]
            conn.execute("DELETE FROM players")
            conn.execute("DELETE FROM match_participants")
            conn.execute("DELETE FROM rating_history")
            conn.execute("DELETE FROM match_predictions")
            conn.execute("DELETE FROM matches")
        return count

    def save_pending_match(self, guild_id: int | None, payload: dict) -> int:
        return self.match_repo.save_pending_match(guild_id, payload)

    def get_pending_match(self, guild_id: int | None) -> dict | None:
        return self.match_repo.get_pending_match(guild_id)

    def clear_pending_match(
        self,
        guild_id: int | None,
        pending_match_id: int | None = None,
    ) -> None:
        self.match_repo.clear_pending_match(guild_id, pending_match_id)

    def consume_pending_match(
        self,
        guild_id: int | None,
        pending_match_id: int | None = None,
    ) -> dict | None:
        return self.match_repo.consume_pending_match(guild_id, pending_match_id)
