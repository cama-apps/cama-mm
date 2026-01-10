"""
Tests for AI services: AIService, SQLQueryService, FlavorTextService, AIQueryRepository.
"""

import json
import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from services.ai_service import AIService, SQL_TOOL, FLAVOR_TOOL
from services.sql_query_service import SQLQueryService, ALLOWED_TABLES, BLOCKED_COLUMNS
from services.flavor_text_service import FlavorTextService, FlavorEvent, PlayerContext
from repositories.ai_query_repository import AIQueryRepository


class TestAIService:
    """Tests for AIService LiteLLM wrapper."""

    @pytest.fixture
    def ai_service(self):
        return AIService(
            model="cerebras/zai-glm-4.7",
            api_key="test-api-key",
            timeout=30.0,
            max_tokens=500,
        )

    @pytest.mark.asyncio
    async def test_call_with_tools_returns_tool_args(self, ai_service):
        """Test that call_with_tools extracts tool call arguments."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "execute_sql_query"
        mock_tool_call.function.arguments = json.dumps({
            "sql": "SELECT * FROM players",
            "explanation": "Get all players"
        })
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.call_with_tools(
                messages=[{"role": "user", "content": "test"}],
                tools=[SQL_TOOL],
            )

        assert result.tool_name == "execute_sql_query"
        assert result.tool_args["sql"] == "SELECT * FROM players"
        assert result.tool_args["explanation"] == "Get all players"

    @pytest.mark.asyncio
    async def test_generate_sql_returns_sql_and_explanation(self, ai_service):
        """Test that generate_sql returns structured SQL output."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "execute_sql_query"
        mock_tool_call.function.arguments = json.dumps({
            "sql": "SELECT discord_username, wins FROM players ORDER BY wins DESC LIMIT 5",
            "explanation": "Get top 5 players by wins"
        })
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.generate_sql("who has the most wins?", "schema context")

        assert "sql" in result
        assert "SELECT discord_username" in result["sql"]
        assert result["explanation"] == "Get top 5 players by wins"

    @pytest.mark.asyncio
    async def test_generate_flavor_returns_comment(self, ai_service):
        """Test that generate_flavor returns a comment string."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "generate_flavor_text"
        mock_tool_call.function.arguments = json.dumps({
            "comment": "The house always wins, but at least you tried!",
            "tone": "roast"
        })
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.generate_flavor(
                event_type="bankruptcy_declared",
                player_context={"username": "TestPlayer", "balance": -100},
                event_details={"debt_cleared": 100},
                examples=["Example roast 1", "Example roast 2"],
            )

        assert result == "The house always wins, but at least you tried!"

    @pytest.mark.asyncio
    async def test_complete_returns_text_content(self, ai_service):
        """Test that complete returns the text content from the response."""
        mock_response = MagicMock()
        mock_response.choices = [MagicMock(message=MagicMock(content="This is the AI response."))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.complete("Test prompt", system_prompt="Be helpful")

        assert result == "This is the AI response."


class TestSQLQueryService:
    """Tests for SQLQueryService validation and query execution."""

    def test_validate_sql_rejects_non_select(self):
        """Test that non-SELECT queries are rejected."""
        service = SQLQueryService.__new__(SQLQueryService)

        is_valid, error = service._validate_sql("INSERT INTO players VALUES (1, 'test')")
        assert not is_valid
        assert "SELECT" in error

        is_valid, error = service._validate_sql("UPDATE players SET name='test'")
        assert not is_valid
        assert "SELECT" in error

        is_valid, error = service._validate_sql("DELETE FROM players")
        assert not is_valid
        assert "SELECT" in error

    def test_validate_sql_rejects_dangerous_keywords(self):
        """Test that dangerous SQL keywords are blocked."""
        service = SQLQueryService.__new__(SQLQueryService)

        # Test each dangerous keyword
        dangerous_queries = [
            "SELECT * FROM players; DROP TABLE players",
            "SELECT * FROM players WHERE 1=1; TRUNCATE players",
            "SELECT * FROM (SELECT 1) AS t; ALTER TABLE players ADD x INT",
        ]

        for query in dangerous_queries:
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Should reject: {query}"

    def test_validate_sql_rejects_blocked_columns(self):
        """Test that blocked columns are not allowed."""
        service = SQLQueryService.__new__(SQLQueryService)

        for column in BLOCKED_COLUMNS:
            query = f"SELECT {column} FROM players"
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Should block column: {column}"
            assert column in error.lower()

    def test_validate_sql_allows_valid_select(self):
        """Test that valid SELECT queries pass validation."""
        service = SQLQueryService.__new__(SQLQueryService)

        valid_queries = [
            "SELECT discord_username, wins, losses FROM players",
            "SELECT COUNT(*) FROM matches",
            # Note: aliases like 'p' are treated as table references and validated
            "SELECT discord_username FROM players ORDER BY wins DESC LIMIT 10",
        ]

        for query in valid_queries:
            is_valid, error = service._validate_sql(query)
            assert is_valid, f"Should allow: {query}, got error: {error}"

    def test_allowed_tables_whitelist(self):
        """Test that the allowed tables whitelist is properly defined."""
        assert "players" in ALLOWED_TABLES
        assert "matches" in ALLOWED_TABLES
        assert "bets" in ALLOWED_TABLES
        assert "rating_history" in ALLOWED_TABLES

    def test_blocked_columns_blocklist(self):
        """Test that sensitive columns are in the blocklist."""
        assert "discord_id" in BLOCKED_COLUMNS
        assert "steam_id" in BLOCKED_COLUMNS
        assert "dotabuff_url" in BLOCKED_COLUMNS


class TestFlavorTextService:
    """Tests for FlavorTextService event and data insights."""

    @pytest.fixture
    def mock_player_repo(self):
        repo = MagicMock()
        player = MagicMock()
        player.name = "TestPlayer"
        player.jopacoin_balance = 100
        player.wins = 10
        player.losses = 5
        repo.get_by_id.return_value = player
        return repo

    @pytest.fixture
    def mock_ai_service(self):
        service = MagicMock()
        service.generate_flavor = AsyncMock(return_value="Snarky AI comment")
        service.complete = AsyncMock(return_value="AI insight about data")
        return service

    @pytest.fixture
    def mock_guild_config_repo(self):
        repo = MagicMock()
        repo.get_ai_enabled.return_value = True
        return repo

    @pytest.fixture
    def flavor_service(self, mock_ai_service, mock_player_repo, mock_guild_config_repo):
        return FlavorTextService(
            ai_service=mock_ai_service,
            player_repo=mock_player_repo,
            guild_config_repo=mock_guild_config_repo,
        )

    @pytest.mark.asyncio
    async def test_generate_event_flavor_returns_comment(self, flavor_service, mock_ai_service):
        """Test that generate_event_flavor returns AI comment."""
        result = await flavor_service.generate_event_flavor(
            guild_id=123,
            event=FlavorEvent.LOAN_TAKEN,
            discord_id=456,
            event_details={"amount": 50, "fee": 10},
        )

        assert result == "Snarky AI comment"
        mock_ai_service.generate_flavor.assert_called_once()

    @pytest.mark.asyncio
    async def test_generate_event_flavor_respects_ai_disabled(
        self, mock_ai_service, mock_player_repo
    ):
        """Test that AI is not called when disabled, but fallback is returned."""
        guild_config_repo = MagicMock()
        guild_config_repo.get_ai_enabled.return_value = False

        service = FlavorTextService(
            ai_service=mock_ai_service,
            player_repo=mock_player_repo,
            guild_config_repo=guild_config_repo,
        )

        result = await service.generate_event_flavor(
            guild_id=123,
            event=FlavorEvent.LOAN_TAKEN,
            discord_id=456,
            event_details={},
        )

        # AI should not be called when disabled
        mock_ai_service.generate_flavor.assert_not_called()
        # But we should get a fallback from examples
        from services.flavor_text_service import EVENT_EXAMPLES

        assert result in EVENT_EXAMPLES[FlavorEvent.LOAN_TAKEN]

    @pytest.mark.asyncio
    async def test_generate_data_insight_returns_insight(self, flavor_service, mock_ai_service):
        """Test that generate_data_insight returns AI insight."""
        result = await flavor_service.generate_data_insight(
            guild_id=123,
            data_type="leaderboard",
            data={"top_players": [{"name": "Player1", "balance": 500}]},
        )

        assert result == "AI insight about data"
        mock_ai_service.complete.assert_called_once()

    @pytest.mark.asyncio
    async def test_generate_data_insight_respects_ai_disabled(
        self, mock_ai_service, mock_player_repo
    ):
        """Test that data insight is not generated when AI is disabled."""
        guild_config_repo = MagicMock()
        guild_config_repo.get_ai_enabled.return_value = False

        service = FlavorTextService(
            ai_service=mock_ai_service,
            player_repo=mock_player_repo,
            guild_config_repo=guild_config_repo,
        )

        result = await service.generate_data_insight(
            guild_id=123,
            data_type="leaderboard",
            data={},
        )

        assert result is None
        mock_ai_service.complete.assert_not_called()

    def test_player_context_from_services(self, mock_player_repo):
        """Test PlayerContext.from_services builds context correctly."""
        context = PlayerContext.from_services(
            discord_id=123,
            player_repo=mock_player_repo,
        )

        assert context is not None
        assert context.username == "TestPlayer"
        assert context.balance == 100

    def test_player_context_returns_none_for_unknown_player(self):
        """Test that PlayerContext returns None for unknown players."""
        repo = MagicMock()
        repo.get_by_id.return_value = None

        context = PlayerContext.from_services(discord_id=999, player_repo=repo)
        assert context is None


class TestAIQueryRepository:
    """Tests for AIQueryRepository read-only SQL execution."""

    @pytest.fixture
    def ai_query_repo(self, repo_db_path):
        return AIQueryRepository(repo_db_path)

    def test_execute_readonly_returns_results(self, ai_query_repo):
        """Test that execute_readonly returns query results."""
        # First insert a test player
        import sqlite3
        conn = sqlite3.connect(ai_query_repo.db_path)
        conn.execute(
            "INSERT INTO players (discord_id, discord_username) VALUES (?, ?)",
            (123, "TestPlayer"),
        )
        conn.commit()
        conn.close()

        results = ai_query_repo.execute_readonly("SELECT discord_username FROM players WHERE discord_id = 123")

        assert len(results) == 1
        assert results[0]["discord_username"] == "TestPlayer"

    def test_execute_readonly_respects_max_rows(self, ai_query_repo):
        """Test that execute_readonly limits results."""
        # Insert multiple test players
        import sqlite3
        conn = sqlite3.connect(ai_query_repo.db_path)
        for i in range(10):
            conn.execute(
                "INSERT INTO players (discord_id, discord_username) VALUES (?, ?)",
                (1000 + i, f"Player{i}"),
            )
        conn.commit()
        conn.close()

        results = ai_query_repo.execute_readonly(
            "SELECT discord_username FROM players WHERE discord_id >= 1000",
            max_rows=5
        )

        assert len(results) == 5

    def test_execute_readonly_rejects_writes(self, ai_query_repo):
        """Test that write operations are blocked."""
        with pytest.raises(Exception):
            ai_query_repo.execute_readonly(
                "INSERT INTO players (discord_id, discord_username) VALUES (999, 'Hacker')"
            )


class TestFlavorEvents:
    """Tests for FlavorEvent enum and examples."""

    def test_all_events_have_examples(self):
        """Test that all FlavorEvent values have example messages."""
        from services.flavor_text_service import EVENT_EXAMPLES

        for event in FlavorEvent:
            assert event in EVENT_EXAMPLES, f"Missing examples for {event}"
            assert len(EVENT_EXAMPLES[event]) > 0, f"Empty examples for {event}"

    def test_flavor_events_match_expected(self):
        """Test that expected FlavorEvents are defined."""
        expected_events = [
            "LOAN_TAKEN",
            "NEGATIVE_LOAN",
            "BANKRUPTCY_DECLARED",
            "DEBT_PAID",
            "BET_WON",
            "BET_LOST",
            "LEVERAGE_LOSS",
            "MATCH_WIN",
            "MATCH_LOSS",
        ]

        for event_name in expected_events:
            assert hasattr(FlavorEvent, event_name), f"Missing event: {event_name}"
