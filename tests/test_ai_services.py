"""
Tests for AI services: AIService, SQLQueryService, FlavorTextService, AIQueryRepository.
"""

import json
import sqlite3
import warnings
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from repositories.ai_query_repository import AIQueryRepository
from services.ai_service import (
    SQL_TOOL,
    AIService,
    _suppress_litellm_pydantic_warnings,
)
from services.flavor_text_service import FlavorEvent, FlavorTextService, PlayerContext
from services.sql_query_service import BLOCKED_COLUMNS, BLOCKED_TABLES, SQLQueryService


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

    def test_litellm_pydantic_serializer_warning_is_suppressed(self):
        """LiteLLM 1.80.x can emit noisy Pydantic serialization warnings."""
        message = (
            "Pydantic serializer warnings:\n"
            "  PydanticSerializationUnexpectedValue(Expected 10 fields but got 5: "
            "Expected `Message` - serialized value may not be as expected)"
        )

        with warnings.catch_warnings(record=True) as caught:
            _suppress_litellm_pydantic_warnings()
            warnings.warn_explicit(
                message,
                category=UserWarning,
                filename="pydantic/main.py",
                lineno=464,
                module="pydantic.main",
            )

        assert caught == []

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
    async def test_groq_tool_calls_disable_reasoning(self):
        """Groq/Qwen tool calls are brittle when reasoning emits tool JSON."""
        ai_service = AIService(
            model="groq/qwen/qwen3-32b",
            api_key="test-api-key",
            timeout=30.0,
            max_tokens=500,
        )
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "execute_sql_query"
        mock_tool_call.function.arguments = json.dumps({
            "sql": "SELECT discord_username FROM players LIMIT 1",
            "explanation": "Get one player",
        })
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.call_with_tools(
                messages=[{"role": "user", "content": "test"}],
                tools=[SQL_TOOL],
            )

        call_kwargs = mock_completion.call_args.kwargs
        assert result.tool_name == "execute_sql_query"
        assert call_kwargs["reasoning_format"] == "parsed"
        assert call_kwargs["reasoning_effort"] == "none"
        assert call_kwargs["parallel_tool_calls"] is False

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
    async def test_generate_flavor_match_win_injects_persona(self, ai_service):
        """match_win calls inject the persona's voice + examples into the system
        prompt and bump temperature for variety."""
        from services.flavor_personas import FlavorPersona

        persona = FlavorPersona(
            key="test_persona",
            name="Test Persona Voice",
            system_prompt="UNIQUE_VOICE_SENTINEL — speak only in haiku.",
            examples=[
                "UNIQUE_EXAMPLE_SENTINEL_ONE",
                "UNIQUE_EXAMPLE_SENTINEL_TWO",
            ],
        )

        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "generate_flavor_text"
        mock_tool_call.function.arguments = json.dumps({"comment": "ok"})
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            await ai_service.generate_flavor(
                event_type="match_win",
                player_context={"username": "TestPlayer"},
                event_details={
                    "is_big_gainer": True,
                    "rating_change": 90,
                    "expected_win_prob": 0.55,
                },
                examples=["unused fallback"],
                persona=persona,
            )

        call_kwargs = mock_completion.call_args.kwargs
        system_content = call_kwargs["messages"][0]["content"]
        assert "UNIQUE_VOICE_SENTINEL" in system_content
        assert "UNIQUE_EXAMPLE_SENTINEL_ONE" in system_content
        assert "UNIQUE_EXAMPLE_SENTINEL_TWO" in system_content
        assert "Test Persona Voice" in system_content
        # The fallback `examples` arg should be ignored when a persona is provided.
        assert "unused fallback" not in system_content
        # Persona calls bump temperature for variety.
        assert call_kwargs.get("temperature") == 0.95

    @pytest.mark.asyncio
    async def test_generate_flavor_gambling_event_skips_persona(self, ai_service):
        """Non-match events do not get persona injection or temperature override
        even if a persona is somehow passed."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "generate_flavor_text"
        mock_tool_call.function.arguments = json.dumps({"comment": "ok"})
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            await ai_service.generate_flavor(
                event_type="loan_taken",
                player_context={"username": "TestPlayer"},
                event_details={},
                examples=["Example A"],
            )

        call_kwargs = mock_completion.call_args.kwargs
        # No temperature override for gambling-event flavor.
        assert "temperature" not in call_kwargs

    @pytest.mark.asyncio
    async def test_generate_flavor_bet_warning_frames_minutes_not_final_call(self, ai_service):
        """The mid-window warning prompt frames urgency in minutes and roasts the
        under-bet side — it must NOT reuse the 1-minute 'FINAL CALL' wording."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "generate_flavor_text"
        mock_tool_call.function.arguments = json.dumps({"comment": "ok"})
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            await ai_service.generate_flavor(
                event_type="bet_warning",
                player_context={},
                event_details={
                    "angle": "roast_underdog",
                    "underdog_side": "dire",
                    "has_bettor": False,
                    "standings": "R 500 | D 100",
                    "seconds_left": 300,
                },
                examples=["example line"],
            )

        system_content = mock_completion.call_args.kwargs["messages"][0]["content"]
        assert "minute" in system_content.lower()
        assert "FINAL CALL" not in system_content.upper()
        # Underdog-aware roast targets the under-bet side by name.
        assert "Dire" in system_content
        assert mock_completion.call_args.kwargs.get("temperature") == 0.95

    @pytest.mark.asyncio
    async def test_generate_flavor_bet_last_call_still_says_final_call(self, ai_service):
        """Guard the 1-minute path: its prompt must still scream FINAL CALL."""
        mock_response = MagicMock()
        mock_tool_call = MagicMock()
        mock_tool_call.function.name = "generate_flavor_text"
        mock_tool_call.function.arguments = json.dumps({"comment": "ok"})
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[mock_tool_call]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            await ai_service.generate_flavor(
                event_type="bet_last_call",
                player_context={},
                event_details={
                    "angle": "taunt_crowd",
                    "has_bettor": False,
                    "standings": "x",
                    "seconds_left": 60,
                },
                examples=["example line"],
            )

        system_content = mock_completion.call_args.kwargs["messages"][0]["content"]
        assert "FINAL CALL" in system_content

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

    def test_validate_sql_rejects_blocked_columns_via_union(self):
        """A blocked column smuggled into a UNION branch must still be rejected.

        Regression for the column-blocklist bypass where validation only scanned
        the projection before the first FROM, so the second SELECT of a UNION
        leaked PII (discord_id / steam_id) into the visible results.
        """
        service = SQLQueryService.__new__(SQLQueryService)

        for query in [
            "SELECT discord_username FROM players UNION SELECT discord_id FROM players",
            "SELECT discord_username FROM players UNION ALL SELECT steam_id FROM player_steam_ids",
        ]:
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"UNION must not bypass the blocklist: {query}"
            assert "blocked column" in error.lower()

    def test_validate_sql_rejects_blocked_columns_via_subquery(self):
        """A blocked column after a projected subquery's FROM must be rejected.

        Regression for the bypass where a subquery before the target column moved
        the first FROM, hiding a projected discord_id from the prefix-only scan.
        Also covers a subquery whose own output is a blocked column under an alias.
        """
        service = SQLQueryService.__new__(SQLQueryService)

        for query in [
            "SELECT (SELECT 1 FROM matches LIMIT 1) AS a, discord_id FROM players",
            "SELECT (SELECT discord_id FROM players LIMIT 1) AS x FROM matches",
        ]:
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Subquery must not bypass the blocklist: {query}"
            assert "blocked column" in error.lower()

    def test_validate_sql_allows_blocked_column_only_in_subquery_where(self):
        """Blocked columns used only in a subquery WHERE (not projected) stay allowed.

        Guards against the per-SELECT projection scan over-blocking legitimate
        correlated subqueries — discord_id here is a join predicate, not output.
        """
        service = SQLQueryService.__new__(SQLQueryService)

        query = (
            "SELECT (SELECT COUNT(*) FROM bets WHERE discord_id = p.discord_id) AS bet_count, "
            "discord_username FROM players p"
        )
        is_valid, error = service._validate_sql(query)
        assert is_valid, f"Should allow blocked column used only in a subquery WHERE: {error}"

    def test_validate_sql_allows_semicolon_inside_string_literal(self):
        """A semicolon inside a string literal is data, not a statement separator.

        Regression for the multi-statement guard splitting on ';' naively and
        falsely rejecting a legitimate single SELECT whose value contains ';'.
        """
        service = SQLQueryService.__new__(SQLQueryService)

        is_valid, error = service._validate_sql(
            "SELECT discord_username FROM players WHERE discord_username = 'a;b'"
        )
        assert is_valid, f"Semicolon inside a literal should not be rejected: {error}"

    def test_validate_sql_literal_value_does_not_trip_keyword_guard(self):
        """A dangerous keyword appearing only as a string value is harmless data."""
        service = SQLQueryService.__new__(SQLQueryService)

        is_valid, error = service._validate_sql(
            "SELECT discord_username FROM players WHERE discord_username = 'DROP'"
        )
        assert is_valid, f"Keyword inside a literal should not be rejected: {error}"

    def test_validate_sql_rejects_schema_qualified_references(self):
        """Schema-qualified references (main./temp.) bypass the per-guild views.

        Guild isolation for AI queries is enforced by TEMP VIEWS that shadow the
        base tables; ``main.players`` would read past them into every guild's
        rows, so such references must be rejected. Alias-qualified refs (``p.``)
        and a literal value of 'main' must still be allowed.
        """
        service = SQLQueryService.__new__(SQLQueryService)

        for query in [
            "SELECT discord_username FROM main.players",
            "SELECT discord_username FROM temp.players",
            "SELECT p.discord_username FROM main.players p",
        ]:
            is_valid, _ = service._validate_sql(query)
            assert not is_valid, f"Schema-qualified ref must be rejected: {query}"

        # alias-qualified references and a 'main' literal value remain valid
        ok_alias, _ = service._validate_sql(
            "SELECT p.discord_username FROM players p "
            "JOIN loan_state l ON p.discord_id = l.discord_id"
        )
        assert ok_alias
        ok_literal, _ = service._validate_sql(
            "SELECT discord_username FROM players WHERE discord_username = 'main'"
        )
        assert ok_literal

    def test_blocked_tables_blocklist(self):
        """Test that sensitive tables are in the blocklist."""
        assert "sqlite_sequence" in BLOCKED_TABLES
        assert "schema_migrations" in BLOCKED_TABLES
        assert "pending_matches" in BLOCKED_TABLES
        assert "guild_config" in BLOCKED_TABLES

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
        with pytest.raises(sqlite3.Error):
            ai_query_repo.execute_readonly(
                "INSERT INTO players (discord_id, discord_username) VALUES (999, 'Hacker')"
            )

    def test_get_guild_scoped_tables(self, ai_query_repo):
        """Tables with a guild_id are scoped; the intentionally-global steam-id
        table is not (Steam accounts are unique across guilds by design)."""
        scoped = ai_query_repo.get_guild_scoped_tables()
        assert "players" in scoped
        assert "matches" in scoped
        assert "player_steam_ids" not in scoped

    def test_execute_readonly_guild_scoped_isolates_guilds(self, ai_query_repo):
        """Guild-isolation invariant: an AI query for one guild must never read
        another guild's rows. Enforced at the data layer (per-guild temp views),
        since the model never sees guild_id and cannot be trusted to scope."""
        conn = sqlite3.connect(ai_query_repo.db_path)
        conn.executemany(
            "INSERT INTO players (discord_id, guild_id, discord_username) VALUES (?, ?, ?)",
            [(1, 100, "alice"), (2, 100, "bob"), (3, 200, "eve")],
        )
        conn.commit()
        conn.close()

        names_100 = [
            r["discord_username"]
            for r in ai_query_repo.execute_readonly_guild_scoped(
                "SELECT discord_username FROM players ORDER BY discord_username", guild_id=100
            )
        ]
        assert names_100 == ["alice", "bob"]
        assert "eve" not in names_100  # guild 200's player must not leak into guild 100

        names_200 = [
            r["discord_username"]
            for r in ai_query_repo.execute_readonly_guild_scoped(
                "SELECT discord_username FROM players", guild_id=200
            )
        ]
        assert names_200 == ["eve"]

    def test_execute_readonly_guild_scoped_reads_global_tables_unscoped(self, ai_query_repo):
        """A table without a guild_id column (player_steam_ids) is global and must
        remain readable regardless of the asking guild."""
        conn = sqlite3.connect(ai_query_repo.db_path)
        conn.execute(
            "INSERT INTO player_steam_ids (discord_id, steam_id, is_primary, added_at) "
            "VALUES (1, 111, 1, '2026-01-01T00:00:00')"
        )
        conn.commit()
        conn.close()

        rows = ai_query_repo.execute_readonly_guild_scoped(
            "SELECT steam_id FROM player_steam_ids", guild_id=999999
        )
        assert len(rows) == 1
        assert rows[0]["steam_id"] == 111

    def test_execute_readonly_guild_scoped_rejects_writes(self, ai_query_repo):
        """Writes remain blocked on the guild-scoped connection too."""
        with pytest.raises(sqlite3.Error):
            ai_query_repo.execute_readonly_guild_scoped(
                "UPDATE players SET discord_username = 'x'", guild_id=100
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
            "MVP_CALLOUT",
        ]

        for event_name in expected_events:
            assert hasattr(FlavorEvent, event_name), f"Missing event: {event_name}"
