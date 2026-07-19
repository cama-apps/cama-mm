"""
Tests for registration command logic.
"""


from unittest.mock import AsyncMock, Mock

import pytest

from commands.registration import RegistrationCommands
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID
from tests.repository_harness import RepositoryTestDatabase as Database


class TestPlayerServiceSetRoles:
    """Service layer tests for PlayerService.set_roles()."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def player_service(self, test_db):
        """Create a PlayerService with test database."""
        return PlayerService(PlayerRepository(test_db.db_path))

    def test_set_roles_persists_to_database(self, test_db, player_service):
        """Test that set_roles correctly persists roles to the database."""
        user_id = 12346  # Different ID to avoid collision with TEST_GUILD_ID
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Set roles through the service
        player_service.set_roles(user_id, TEST_GUILD_ID, ["1", "2", "3"])

        # Verify persisted in database
        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1", "2", "3"]

    def test_set_roles_updates_existing_roles(self, test_db, player_service):
        """Test that set_roles updates existing roles."""
        user_id = 12347
        player_repo = PlayerRepository(test_db.db_path)
        player_repo.add(
            discord_id=user_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            preferred_roles=["1", "2"],
        )

        # Update roles
        player_service.set_roles(user_id, TEST_GUILD_ID, ["4", "5"])

        # Verify updated
        player = player_repo.get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["4", "5"]

    def test_set_roles_unregistered_player_raises(self, player_service):
        """Test that set_roles raises for unregistered player."""
        with pytest.raises(ValueError, match="Player not registered"):
            player_service.set_roles(99999, TEST_GUILD_ID, ["1", "2"])


class TestRegistrationCommandsConstructor:
    """Tests for the supported registration-cog dependency surface."""

    def test_constructor_signature_is_bot_and_player_service_only(self):
        from inspect import Parameter, signature

        from discord.ext import commands

        from commands.registration import RegistrationCommands

        parameters = signature(RegistrationCommands.__init__).parameters

        assert list(parameters) == ["self", "bot", "player_service"]
        assert parameters["bot"].annotation is commands.Bot
        assert all(
            parameter.kind is Parameter.POSITIONAL_OR_KEYWORD
            for parameter in parameters.values()
        )


class TestMMRPromptViewSignature:
    """Tests for the MMRPromptView button callback signature.

    The button callback must have (self, interaction, button) parameter order.
    Discord.py passes interaction first, then button. A previous bug had
    these reversed, causing "This interaction failed" errors.
    """

    def test_enter_mmr_button_callback_has_correct_signature(self):
        """Test that discord.ui.button callbacks have (interaction, button) order.

        The correct signature is:
            async def callback(self, interaction: discord.Interaction, button: discord.ui.Button)

        If the parameters are reversed (button, interaction), discord.py will pass
        the wrong types and cause 'This interaction failed' errors when users
        click buttons.

        This test verifies that all button callbacks in the codebase follow the
        correct pattern by checking against a known working example.
        """
        import re
        from pathlib import Path

        # Read the registration.py file and find the enter_mmr callback
        registration_path = Path(__file__).parent.parent / "commands" / "registration.py"
        # Be explicit about encoding for Windows compatibility (Path.read_text() defaults
        # to the locale encoding, which may choke on UTF-8 byte sequences).
        source = registration_path.read_text(encoding="utf-8")

        # Find the enter_mmr function definition
        # Pattern: async def enter_mmr(self, <param1>: <type1>, <param2>: <type2>)
        pattern = r"async\s+def\s+enter_mmr\s*\(\s*self\s*,\s*(\w+)\s*:\s*([\w\.]+)\s*,\s*(\w+)\s*:\s*([\w\.]+)"
        match = re.search(pattern, source)

        assert match is not None, "Could not find enter_mmr callback in registration.py"

        param1_name = match.group(1)
        param1_type = match.group(2)
        param2_name = match.group(3)
        param2_type = match.group(4)

        # First parameter should be the interaction
        assert "Interaction" in param1_type or param1_name.lower().startswith("interaction"), (
            f"First parameter should be Interaction type, "
            f"but found '{param1_name}: {param1_type}'. "
            f"Parameters may be in wrong order (should be interaction, then button)."
        )

        # Second parameter should be the button
        assert "Button" in param2_type or param2_name.lower() == "button", (
            f"Second parameter should be Button type, "
            f"but found '{param2_name}: {param2_type}'. "
            f"Parameters may be in wrong order (should be interaction, then button)."
        )


class TestRegisterSteamIdUniqueness:
    """register_player must enforce the global Steam-ID uniqueness guarantee
    and populate the junction table, not just the legacy column.

    Before the fix, register_player wrote only players.steam_id (no UNIQUE
    constraint, no cross-player check), so two Discord users could claim the
    same Steam ID and corrupt the global steam->discord mapping.
    """

    @pytest.fixture
    def test_db(self, repo_db_path):
        return Database(repo_db_path)

    @pytest.fixture
    def player_service(self, test_db):
        return PlayerService(PlayerRepository(test_db.db_path))

    def test_duplicate_steam_id_rejected_across_players(self, test_db, player_service):
        """A second player registering the same Steam ID is rejected."""
        steam_id = 123456
        player_service.register_player(
            discord_id=1, discord_username="A", guild_id=TEST_GUILD_ID,
            steam_id=steam_id, mmr_override=3000,
        )

        with pytest.raises(ValueError, match="already linked to another player"):
            player_service.register_player(
                discord_id=2, discord_username="B", guild_id=TEST_GUILD_ID,
                steam_id=steam_id, mmr_override=3000,
            )

        # The first owner is unchanged; the second user was not created.
        repo = PlayerRepository(test_db.db_path)
        assert repo.get_steam_id_owner(steam_id) == 1
        assert repo.get_by_id(2, TEST_GUILD_ID) is None

    def test_register_rolls_back_player_on_steam_link_failure(
        self, test_db, player_service, monkeypatch
    ):
        """If add_steam_id raises after the player row is created (a concurrent
        registration won the race for the same Steam ID between the pre-check and
        the junction insert), register_player must roll back the just-created
        player row — no half-registered player left with no Steam mapping."""

        def _boom(*args, **kwargs):
            raise ValueError("Steam ID 98765 is already linked to another player")

        monkeypatch.setattr(player_service.player_repo, "add_steam_id", _boom)

        with pytest.raises(ValueError, match="already linked to another player"):
            player_service.register_player(
                discord_id=42, discord_username="racer", guild_id=TEST_GUILD_ID,
                steam_id=98765, mmr_override=3000,
            )

        repo = PlayerRepository(test_db.db_path)
        assert repo.get_by_id(42, TEST_GUILD_ID) is None

    def test_register_populates_junction_table(self, test_db, player_service):
        """The primary Steam ID is recorded in the junction table (source of
        truth), not only the legacy column."""
        repo = PlayerRepository(test_db.db_path)
        player_service.register_player(
            discord_id=7, discord_username="C", guild_id=TEST_GUILD_ID,
            steam_id=222333, mmr_override=2500,
        )

        assert repo.get_steam_ids(7) == [222333]
        assert repo.get_primary_steam_id(7) == 222333


class TestRegisterSteamIdValidation:
    """_validate_steam_id must accept the full unsigned 32-bit account-id range."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        return Database(repo_db_path)

    @pytest.fixture
    def player_service(self, test_db):
        return PlayerService(PlayerRepository(test_db.db_path))

    def test_high_uint32_account_id_accepted(self, test_db, player_service):
        """Regression: a valid high uint32 account ID (above int32 max) must
        register, not be rejected as 'Invalid Steam ID'."""
        high_id = 3_000_000_000  # > 2**31 - 1, still < 2**32
        result = player_service.register_player(
            discord_id=9, discord_username="D", guild_id=TEST_GUILD_ID,
            steam_id=high_id, mmr_override=4000,
        )
        assert result["mmr"] == 4000

        repo = PlayerRepository(test_db.db_path)
        assert repo.get_primary_steam_id(9) == high_id

    def test_steam_id_at_uint32_ceiling_rejected(self, player_service):
        """2**32 (and above) is out of range and must still be rejected."""
        with pytest.raises(ValueError, match="Invalid Steam ID"):
            player_service.register_player(
                discord_id=10, discord_username="E", guild_id=TEST_GUILD_ID,
                steam_id=2**32, mmr_override=4000,
            )


class TestSetRolesCommand:
    """End-to-end tests for the /player roles command.

    These drive the real command callback in commands/registration.py
    (parse -> validate -> dedup -> PlayerService.set_roles -> DB) with a
    mocked discord Interaction, asserting on the persisted roles or the
    error message sent back — no re-implementation of the parsing logic.
    """

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def player_service(self, test_db):
        """Create a PlayerService with test database."""
        return PlayerService(PlayerRepository(test_db.db_path))

    @pytest.fixture
    def cog(self, player_service):
        """RegistrationCommands cog wired to the real player service."""
        return RegistrationCommands(bot=Mock(), player_service=player_service)

    def _make_interaction(self, user_id: int):
        interaction = Mock()
        interaction.user.id = user_id
        interaction.guild.id = TEST_GUILD_ID
        interaction.response = AsyncMock()
        interaction.followup = AsyncMock()
        return interaction

    @staticmethod
    def _sent_messages(interaction) -> list[str]:
        """Collect message contents sent via followup (positional or content=)."""
        messages = []
        for call in interaction.followup.send.call_args_list:
            if call.args and isinstance(call.args[0], str):
                messages.append(call.args[0])
            content = call.kwargs.get("content")
            if isinstance(content, str):
                messages.append(content)
        return messages

    def _add_player(self, test_db, user_id: int, roles=None):
        PlayerRepository(test_db.db_path).add(
            discord_id=user_id,
            discord_username=f"E2EPlayer{user_id}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2500,
            preferred_roles=roles,
        )

    @pytest.mark.asyncio
    async def test_duplicate_roles_deduplicated_and_persisted(self, test_db, cog):
        """The bug case: '1111111111' (10 carries) collapses to a single role."""
        user_id = 54321
        self._add_player(test_db, user_id)
        interaction = self._make_interaction(user_id)

        await cog.set_roles.callback(cog, interaction, "1111111111")

        player = PlayerRepository(test_db.db_path).get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1"]
        assert any("Set your preferred roles" in m for m in self._sent_messages(interaction))

    @pytest.mark.asyncio
    async def test_mixed_duplicates_preserve_order(self, test_db, cog):
        """Mixed duplicates keep first-occurrence order in the persisted roles."""
        user_id = 54322
        self._add_player(test_db, user_id)
        interaction = self._make_interaction(user_id)

        await cog.set_roles.callback(cog, interaction, "54321123")

        player = PlayerRepository(test_db.db_path).get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["5", "4", "3", "2", "1"]

    @pytest.mark.asyncio
    async def test_comma_separated_with_duplicates(self, test_db, cog):
        """Comma/space-separated input with duplicates is parsed and deduped."""
        user_id = 54323
        self._add_player(test_db, user_id)
        interaction = self._make_interaction(user_id)

        await cog.set_roles.callback(cog, interaction, "1, 2, 1, 3, 2")

        player = PlayerRepository(test_db.db_path).get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["1", "2", "3"]

    @pytest.mark.asyncio
    async def test_invalid_role_rejected_without_persisting(self, test_db, cog):
        """An out-of-range role is rejected and existing roles stay untouched."""
        user_id = 54324
        self._add_player(test_db, user_id, roles=["2"])
        interaction = self._make_interaction(user_id)

        await cog.set_roles.callback(cog, interaction, "126")  # 6 is invalid

        assert any("Invalid role: 6" in m for m in self._sent_messages(interaction))
        player = PlayerRepository(test_db.db_path).get_by_id(user_id, TEST_GUILD_ID)
        assert player.preferred_roles == ["2"]

    @pytest.mark.asyncio
    async def test_empty_roles_rejected(self, test_db, cog):
        """Input that parses to no roles yields the 'at least one role' error."""
        user_id = 54325
        self._add_player(test_db, user_id)
        interaction = self._make_interaction(user_id)

        await cog.set_roles.callback(cog, interaction, ", ")

        assert any("at least one role" in m for m in self._sent_messages(interaction))

    @pytest.mark.asyncio
    async def test_unregistered_player_gets_error(self, cog):
        """The service's 'not registered' ValueError surfaces as a user error."""
        interaction = self._make_interaction(98765)

        await cog.set_roles.callback(cog, interaction, "123")

        assert any("not registered" in m.lower() for m in self._sent_messages(interaction))
