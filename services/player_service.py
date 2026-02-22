"""
Player-facing business logic (registration, roles, stats).
"""

from domain.models.player import Player
from opendota_integration import OpenDotaAPI
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from repositories.interfaces import IPlayerRepository

STEAM_ID64_OFFSET = 76561197960265728


class PlayerService:
    """Encapsulates registration, role updates, and player stats."""

    def __init__(self, player_repo: IPlayerRepository):
        self.player_repo = player_repo
        self.rating_system = CamaRatingSystem()
        self.openskill_system = CamaOpenSkillSystem()

    @staticmethod
    def _validate_steam_id(steam_id: int):
        if steam_id <= 0 or steam_id > 2147483647:
            raise ValueError("Invalid Steam ID. Must be Steam32 (positive, 32-bit).")

    def register_player(
        self,
        discord_id: int,
        discord_username: str,
        guild_id: int,
        steam_id: int,
        *,
        mmr_override: int | None = None,
    ) -> dict:
        """
        Register a new player and seed their rating.

        Returns a dict with display-friendly values (cama_rating, uncertainty, dotabuff_url).
        """
        self._validate_steam_id(steam_id)

        existing = self.player_repo.get_by_id(discord_id, guild_id)
        if existing:
            raise ValueError("Player already registered.")

        mmr: int | None = None
        if mmr_override is not None:
            mmr = mmr_override
        else:
            api = OpenDotaAPI()
            player_data = api.get_player_data(steam_id)
            if not player_data:
                raise ValueError("Could not fetch player data from OpenDota.")

            # Primary MMR
            mmr = api.get_player_mmr(steam_id)

            # Fallback: use current_mmr estimate if present and mmr was not returned.
            # NOTE: CamaRatingSystem.mmr_to_rating() already maps 0-12000 -> 0-3000 (effectively /4),
            # so we must NOT divide by 4 here.
            if mmr is None:
                current_mmr = player_data.get("mmr_estimate", {}).get("estimate")
                if current_mmr is None:
                    current_mmr = player_data.get("solo_competitive_rank")
                try:
                    current_mmr_int = int(current_mmr) if current_mmr is not None else None
                except (TypeError, ValueError):
                    current_mmr_int = None
                if current_mmr_int and current_mmr_int > 0:
                    mmr = current_mmr_int

        if mmr is None or mmr <= 0:
            raise ValueError("MMR not available; please provide your MMR.")
        glicko_player = self.rating_system.create_player_from_mmr(mmr)

        # Initialize OpenSkill from MMR (same scale alignment as Glicko)
        os_mu = self.openskill_system.mmr_to_os_mu(mmr)
        os_sigma = CamaOpenSkillSystem.DEFAULT_SIGMA  # High uncertainty for new players

        steam_id64 = steam_id + STEAM_ID64_OFFSET
        dotabuff_url = f"https://www.dotabuff.com/players/{steam_id64}"

        self.player_repo.add(
            discord_id=discord_id,
            discord_username=discord_username,
            guild_id=guild_id,
            dotabuff_url=dotabuff_url,
            steam_id=steam_id,
            initial_mmr=mmr,
            glicko_rating=glicko_player.rating,
            glicko_rd=glicko_player.rd,
            glicko_volatility=glicko_player.vol,
            os_mu=os_mu,
            os_sigma=os_sigma,
        )

        cama_rating = self.rating_system.rating_to_display(glicko_player.rating)
        uncertainty = self.rating_system.get_rating_uncertainty_percentage(glicko_player.rd)

        return {
            "cama_rating": cama_rating,
            "uncertainty": uncertainty,
            "dotabuff_url": dotabuff_url,
            "mmr": mmr,
        }

    def set_roles(self, discord_id: int, guild_id: int, roles: list[str]):
        """Persist preferred roles for a player."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            raise ValueError("Player not registered.")
        self.player_repo.update_roles(discord_id, guild_id, roles)

    def get_player(self, discord_id: int, guild_id: int) -> Player | None:
        """Fetch a Player model by Discord ID and Guild ID."""
        return self.player_repo.get_by_id(discord_id, guild_id)

    def get_balance(self, discord_id: int, guild_id: int) -> int:
        """Return the player's current jopacoin balance."""
        return self.player_repo.get_balance(discord_id, guild_id)

    def get_stats(self, discord_id: int, guild_id: int) -> dict:
        """Return stats payload for a player."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
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
            "jopacoin_balance": self.player_repo.get_balance(discord_id, guild_id),
        }

    # --- Balance operations ---

    def adjust_balance(self, discord_id: int, guild_id: int, delta: int) -> int:
        """
        Add or subtract from a player's jopacoin balance.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            delta: Amount to add (positive) or subtract (negative)

        Returns:
            New balance after adjustment
        """
        self.player_repo.add_balance(discord_id, guild_id, delta)
        return self.player_repo.get_balance(discord_id, guild_id)

    def set_balance(self, discord_id: int, guild_id: int, amount: int) -> None:
        """
        Set a player's jopacoin balance to a specific amount.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            amount: New balance amount
        """
        self.player_repo.update_balance(discord_id, guild_id, amount)

    # --- Exclusion count operations ---

    def increment_exclusion_count_half(self, discord_id: int, guild_id: int) -> None:
        """
        Increment player's exclusion count by 2 (half the normal bonus).

        Used for conditional players who weren't picked.
        """
        self.player_repo.increment_exclusion_count_half(discord_id, guild_id)

    # --- OpenSkill rating operations ---

    def get_openskill_rating(self, discord_id: int, guild_id: int) -> tuple[float, float] | None:
        """
        Get player's OpenSkill rating (mu, sigma).

        Returns:
            Tuple of (mu, sigma) or None if not found or not set
        """
        return self.player_repo.get_openskill_rating(discord_id, guild_id)

    # --- Double or Nothing operations ---

    def get_last_double_or_nothing(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the timestamp of a player's last Double or Nothing spin.

        Returns:
            Unix timestamp of last spin, or None if never played
        """
        return self.player_repo.get_last_double_or_nothing(discord_id, guild_id)

    def log_double_or_nothing(
        self,
        discord_id: int,
        guild_id: int,
        cost: int,
        balance_before: int,
        balance_after: int,
        won: bool,
        spin_time: int,
    ) -> None:
        """
        Log a Double or Nothing spin and update cooldown.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            cost: Cost paid to play
            balance_before: Balance before the gamble (after cost deducted)
            balance_after: Balance after the gamble
            won: Whether the player won
            spin_time: Unix timestamp of the spin
        """
        self.player_repo.log_double_or_nothing(
            discord_id=discord_id,
            guild_id=guild_id,
            cost=cost,
            balance_before=balance_before,
            balance_after=balance_after,
            won=won,
            spin_time=spin_time,
        )

    # --- Wheel of Fortune operations ---

    def get_last_wheel_spin(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the timestamp of a player's last wheel spin.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            Unix timestamp of last spin, or None if never played
        """
        return self.player_repo.get_last_wheel_spin(discord_id, guild_id)

    def set_last_wheel_spin(self, discord_id: int, guild_id: int, timestamp: int) -> None:
        """
        Set the timestamp of a player's last wheel spin.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            timestamp: Unix timestamp of the spin
        """
        self.player_repo.set_last_wheel_spin(discord_id, guild_id, timestamp)

    def try_claim_wheel_spin(
        self, discord_id: int, guild_id: int, now: int, cooldown_seconds: int
    ) -> bool:
        """
        Atomically check cooldown and claim a wheel spin.

        Prevents race conditions where concurrent requests could both pass
        the cooldown check before either sets the new timestamp.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            now: Current Unix timestamp
            cooldown_seconds: Required cooldown between spins

        Returns:
            True if spin was successfully claimed, False if still on cooldown
        """
        return self.player_repo.try_claim_wheel_spin(discord_id, guild_id, now, cooldown_seconds)

    def log_wheel_spin(
        self, discord_id: int, guild_id: int | None, result: int, spin_time: int,
        is_bankrupt: bool = False,
    ) -> int:
        """
        Log a wheel spin result for gambling history tracking.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID (None for DMs)
            result: The spin result value (positive for win, negative for loss)
            spin_time: Unix timestamp of the spin
            is_bankrupt: True if this was a bankrupt wheel spin

        Returns:
            The spin log ID
        """
        return self.player_repo.log_wheel_spin(discord_id, guild_id, result, spin_time, is_bankrupt)

    def get_last_normal_wheel_spin(self, guild_id: int | None) -> dict | None:
        """
        Get the most recent normal-wheel (non-bankrupt) spin in this guild.

        Used by CHAIN_REACTION bankrupt wheel mechanic.

        Returns:
            Dict with 'result' (int) and 'discord_id' (int), or None
        """
        return self.player_repo.get_last_normal_wheel_spin(guild_id)

    # --- Leaderboard and ranking operations ---

    def get_leaderboard(self, guild_id: int, limit: int = 20, offset: int = 0):
        """
        Get players for leaderboard, sorted by jopacoin balance descending.

        Args:
            guild_id: Guild ID to filter by
            limit: Maximum number of players to return
            offset: Number of players to skip (for pagination)

        Returns:
            List of Player objects sorted by balance
        """
        return self.player_repo.get_leaderboard(guild_id, limit, offset)

    def get_player_above(self, discord_id: int, guild_id: int):
        """
        Get the player ranked one position higher on the balance leaderboard.

        Used for Red Shell wheel mechanic - steals from the player ahead.

        Args:
            discord_id: The player's Discord ID
            guild_id: Guild ID

        Returns:
            Player object of the player ranked above, or None if user is #1
        """
        return self.player_repo.get_player_above(discord_id, guild_id)

    # --- Atomic transfer operations ---

    def steal_atomic(
        self,
        thief_discord_id: int,
        victim_discord_id: int,
        guild_id: int,
        amount: int,
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from victim to thief (shell mechanic).

        Unlike tips, this transfer has no fee and can push victim below MAX_DEBT.
        Used for Red Shell and Blue Shell wheel outcomes.

        Args:
            thief_discord_id: Discord ID of player receiving coins
            victim_discord_id: Discord ID of player losing coins
            guild_id: Guild ID
            amount: Amount to transfer

        Returns:
            Dict with 'amount', 'thief_new_balance', 'victim_new_balance'
        """
        return self.player_repo.steal_atomic(
            thief_discord_id, victim_discord_id, guild_id, amount
        )

    def tip_atomic(
        self,
        from_discord_id: int,
        to_discord_id: int,
        guild_id: int,
        amount: int,
        fee: int,
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from one player to another with a fee.

        The fee is sent to the nonprofit fund. Sender pays amount + fee,
        recipient receives only amount.

        Args:
            from_discord_id: Player sending the tip
            to_discord_id: Player receiving the tip
            guild_id: Guild ID
            amount: Amount to transfer (recipient receives this)
            fee: Fee to charge (goes to nonprofit fund)

        Returns:
            Dict with transfer details
        """
        return self.player_repo.tip_atomic(
            from_discord_id, to_discord_id, guild_id, amount, fee
        )

    def pay_debt_atomic(
        self, from_discord_id: int, to_discord_id: int, guild_id: int, amount: int
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from one player to pay down another's debt.

        Args:
            from_discord_id: Player paying (must have positive balance)
            to_discord_id: Player receiving (can be same for self-payment)
            guild_id: Guild ID
            amount: Amount to transfer

        Returns:
            Dict with 'amount_paid', 'from_new_balance', 'to_new_balance'
        """
        return self.player_repo.pay_debt_atomic(
            from_discord_id, to_discord_id, guild_id, amount
        )

    # --- Admin operations ---

    def delete_player(self, discord_id: int, guild_id: int) -> bool:
        """
        Delete a player's account.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            True if player was deleted, False if not found
        """
        return self.player_repo.delete(discord_id, guild_id)

    def add_fake_player(
        self,
        discord_id: int,
        discord_username: str,
        guild_id: int | None,
        glicko_rating: float,
        glicko_rd: float,
        glicko_volatility: float,
        preferred_roles: list[str],
    ) -> None:
        """
        Add a fake player for testing purposes.

        Unlike register_player, this does not require a Steam ID or OpenDota lookup.

        Args:
            discord_id: Fake Discord ID (usually negative)
            discord_username: Display name
            guild_id: Guild ID
            glicko_rating: Initial rating
            glicko_rd: Initial RD
            glicko_volatility: Initial volatility
            preferred_roles: List of role preferences
        """
        self.player_repo.add(
            discord_id=discord_id,
            discord_username=discord_username,
            guild_id=guild_id,
            initial_mmr=None,
            glicko_rating=glicko_rating,
            glicko_rd=glicko_rd,
            glicko_volatility=glicko_volatility,
            preferred_roles=preferred_roles,
        )

    def set_captain_eligible(self, discord_id: int, guild_id: int | None, eligible: bool) -> None:
        """
        Set whether a player is eligible to be a captain.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            eligible: Whether the player can be a captain
        """
        self.player_repo.set_captain_eligible(discord_id, guild_id, eligible)

    def get_game_count(self, discord_id: int, guild_id: int) -> int:
        """
        Get the number of games a player has played.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            Total games played (wins + losses)
        """
        if hasattr(self.player_repo, "get_game_count"):
            return self.player_repo.get_game_count(discord_id, guild_id)
        # Fallback: get player and compute
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if player:
            return player.wins + player.losses
        return 0

    def get_glicko_rating(self, discord_id: int, guild_id: int) -> tuple[float, float, float] | None:
        """
        Get a player's Glicko-2 rating components.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            Tuple of (rating, rd, volatility) or None if not found
        """
        return self.player_repo.get_glicko_rating(discord_id, guild_id)

    def update_glicko_rating(
        self,
        discord_id: int,
        guild_id: int,
        rating: float,
        rd: float,
        volatility: float,
    ) -> None:
        """
        Update a player's Glicko-2 rating.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            rating: New rating value
            rd: New rating deviation
            volatility: New volatility
        """
        self.player_repo.update_glicko_rating(discord_id, guild_id, rating, rd, volatility)

    # --- Steam ID operations ---

    def get_steam_ids(self, discord_id: int) -> list[int]:
        """
        Get all Steam IDs linked to a player.

        Args:
            discord_id: Player's Discord ID

        Returns:
            List of Steam IDs, primary first
        """
        return self.player_repo.get_steam_ids(discord_id)

    def add_steam_id(self, discord_id: int, steam_id: int, is_primary: bool = False) -> None:
        """
        Link a Steam ID to a player's account.

        Args:
            discord_id: Player's Discord ID
            steam_id: Steam32 ID to add
            is_primary: Whether to set as primary account

        Raises:
            ValueError: If Steam ID is already linked to another player
        """
        self.player_repo.add_steam_id(discord_id, steam_id, is_primary=is_primary)

    def remove_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """
        Remove a Steam ID from a player's account.

        Args:
            discord_id: Player's Discord ID
            steam_id: Steam32 ID to remove

        Returns:
            True if removed, False if not found
        """
        return self.player_repo.remove_steam_id(discord_id, steam_id)

    def set_primary_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """
        Set a Steam ID as the player's primary account.

        Args:
            discord_id: Player's Discord ID
            steam_id: Steam32 ID to set as primary (must already be linked)

        Returns:
            True if set successfully, False if not found
        """
        return self.player_repo.set_primary_steam_id(discord_id, steam_id)

    # --- Leaderboard and statistics operations ---

    def get_player_count(self, guild_id: int) -> int:
        """Get total number of registered players in a guild."""
        return self.player_repo.get_player_count(guild_id)

    def get_players_with_negative_balance(self, guild_id: int) -> list[dict]:
        """Get players with negative jopacoin balance (debtors)."""
        return self.player_repo.get_players_with_negative_balance(guild_id)

    def get_leaderboard_by_glicko(self, guild_id: int, limit: int = 100) -> list:
        """Get players sorted by Glicko-2 rating descending."""
        return self.player_repo.get_leaderboard_by_glicko(guild_id, limit)

    def get_leaderboard_by_openskill(self, guild_id: int, limit: int = 100) -> list:
        """Get players sorted by OpenSkill mu descending."""
        return self.player_repo.get_leaderboard_by_openskill(guild_id, limit)

    def get_rated_player_count(self, guild_id: int, rating_type: str = "glicko") -> int:
        """Get count of players with ratings of a specific type."""
        return self.player_repo.get_rated_player_count(guild_id, rating_type)

    def get_all(self, guild_id: int) -> list:
        """Get all players in a guild."""
        return self.player_repo.get_all(guild_id)

    def get_by_ids(self, discord_ids: list[int], guild_id: int | None) -> list:
        """
        Get multiple players by their Discord IDs.

        Args:
            discord_ids: List of Discord IDs to fetch
            guild_id: Guild ID for multi-server isolation

        Returns:
            List of Player objects found
        """
        return self.player_repo.get_by_ids(discord_ids, guild_id)
