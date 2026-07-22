"""
Tests for AI services: AIService, SQLQueryService, FlavorTextService, AIQueryRepository.
"""

import json
import sqlite3
import subprocess
import sys
import warnings
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from repositories.ai_query_repository import AIQueryRepository
from services.ai_service import (
    SQL_TOOL,
    AIService,
    _litellm_error_kind,
    _suppress_litellm_pydantic_warnings,
)
from services.flavor_text_service import FlavorEvent, FlavorTextService, PlayerContext
from services.sql_query_service import (
    BLOCKED_COLUMNS,
    BLOCKED_TABLES,
    UNSCOPED_TABLE_ALLOWLIST,
    SQLQueryService,
)


def test_ai_service_import_and_construction_do_not_load_litellm():
    """Importing and configuring AI support must not load LiteLLM before first use."""
    repo_root = Path(__file__).resolve().parents[1]
    script = """
import sys

import services.ai_service as ai_module

ai_module.AIService(
    model="cerebras/zai-glm-4.7",
    api_key="test-api-key",
    timeout=30.0,
    max_tokens=500,
)

loaded = sorted(
    name
    for name in sys.modules
    if name == "litellm" or name.startswith("litellm.")
)
assert not loaded, loaded
"""

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )

    assert completed.returncode == 0, (
        "Fresh AIService import or construction loaded LiteLLM:\n"
        f"{completed.stderr or completed.stdout}"
    )


def test_acompletion_first_use_configures_and_delegates_to_litellm():
    """The stable completion seam must configure and call LiteLLM on first use."""
    repo_root = Path(__file__).resolve().parents[1]
    script = """
import asyncio
import sys
import types

import services.ai_service as ai_module

calls = []

async def fake_acompletion(**kwargs):
    calls.append(kwargs)
    return "fake-response"

fake_litellm = types.ModuleType("litellm")
fake_litellm.acompletion = fake_acompletion
fake_litellm.num_retries = 17
sys.modules["litellm"] = fake_litellm

kwargs = {
    "model": "cerebras/zai-glm-4.7",
    "messages": [{"role": "user", "content": "test"}],
}
result = asyncio.run(ai_module.acompletion(**kwargs))

assert result == "fake-response"
assert calls == [kwargs]
assert fake_litellm.num_retries == 0
"""

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )

    assert completed.returncode == 0, (
        "AI completion lazy-import delegation failed:\n"
        f"{completed.stderr or completed.stdout}"
    )


def test_litellm_error_classification_uses_only_loaded_module(monkeypatch):
    """Provider errors are classified without importing LiteLLM for inspection."""

    class FakeRateLimitError(Exception):
        pass

    class FakeTimeout(Exception):
        pass

    fake_litellm = MagicMock(
        RateLimitError=FakeRateLimitError,
        Timeout=FakeTimeout,
    )
    monkeypatch.setitem(sys.modules, "litellm", fake_litellm)

    assert _litellm_error_kind(FakeRateLimitError()) == "rate_limit"
    assert _litellm_error_kind(FakeTimeout()) == "timeout"
    assert _litellm_error_kind(RuntimeError()) is None

    monkeypatch.delitem(sys.modules, "litellm")
    assert _litellm_error_kind(RuntimeError()) is None


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
    async def test_groq_gpt_oss_tool_calls_use_supported_reasoning_params(self):
        """GPT-OSS must not receive Groq's incompatible reasoning_format."""
        ai_service = AIService(
            model="groq/openai/gpt-oss-120b",
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
        assert "reasoning_format" not in call_kwargs
        assert call_kwargs["reasoning_effort"] == "low"
        assert call_kwargs["parallel_tool_calls"] is False

    @pytest.mark.asyncio
    async def test_default_groq_qwen_tool_calls_use_litellm_param_allowlist(self):
        """The Qwen 3.6 default disables reasoning safely for tool calls."""
        ai_service = AIService(
            model="groq/qwen/qwen3.6-27b",
            api_key="test-api-key",
            timeout=30.0,
            max_tokens=500,
        )
        mock_response = MagicMock()
        mock_response.choices = [MagicMock(message=MagicMock(tool_calls=[]))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            await ai_service.call_with_tools(
                messages=[{"role": "user", "content": "test"}],
                tools=[SQL_TOOL],
            )

        call_kwargs = mock_completion.call_args.kwargs
        assert call_kwargs["reasoning_format"] == "parsed"
        assert call_kwargs["reasoning_effort"] == "none"
        assert call_kwargs["allowed_openai_params"] == ["reasoning_effort"]
        assert call_kwargs["parallel_tool_calls"] is False

    @pytest.mark.asyncio
    async def test_default_groq_qwen_completion_uses_parsed_reasoning_only(self):
        """Plain Qwen calls parse reasoning without tool-only parameters."""
        ai_service = AIService(
            model="groq/qwen/qwen3.6-27b",
            api_key="test-api-key",
            timeout=30.0,
            max_tokens=500,
        )
        mock_response = MagicMock()
        mock_response.choices = [MagicMock(message=MagicMock(content="done"))]

        with patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion:
            mock_completion.return_value = mock_response
            result = await ai_service.complete("test")

        call_kwargs = mock_completion.call_args.kwargs
        assert result == "done"
        assert call_kwargs["reasoning_format"] == "parsed"
        assert "reasoning_effort" not in call_kwargs
        assert "allowed_openai_params" not in call_kwargs
        assert "parallel_tool_calls" not in call_kwargs

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

    @pytest.mark.asyncio
    async def test_request_telemetry_records_metadata_and_token_usage(self):
        """Each provider attempt is attributed without storing prompt content."""
        request_repo = MagicMock()
        ai_service = AIService(
            model="groq/openai/gpt-oss-20b",
            api_key="test-api-key",
            timeout=30.0,
            max_tokens=500,
            request_repo=request_repo,
        )
        mock_response = MagicMock()
        mock_response.choices = [MagicMock(message=MagicMock(content="done"))]
        mock_response.usage.prompt_tokens = 41
        mock_response.usage.completion_tokens = 7
        mock_response.usage.total_tokens = 48

        with (
            patch("services.ai_service.acompletion", new_callable=AsyncMock) as mock_completion,
            patch("services.ai_service.asyncio.to_thread", new_callable=AsyncMock) as mock_to_thread,
        ):
            mock_completion.return_value = mock_response
            mock_to_thread.side_effect = lambda func, *args, **kwargs: func(*args, **kwargs)
            result = await ai_service.complete(
                "secret prompt body",
                feature="dig.flavor",
            )

        assert result == "done"
        telemetry = request_repo.record_attempt.call_args.kwargs
        assert telemetry["feature"] == "dig.flavor"
        assert telemetry["operation"] == "completion"
        assert telemetry["provider"] == "groq"
        assert telemetry["model"] == "openai/gpt-oss-20b"
        assert telemetry["success"] is True
        assert telemetry["prompt_tokens"] == 41
        assert telemetry["completion_tokens"] == 7
        assert telemetry["total_tokens"] == 48
        assert "prompt" not in telemetry
        assert "response" not in telemetry
        assert "api_key" not in telemetry

    @pytest.mark.asyncio
    async def test_request_telemetry_records_failures(self):
        request_repo = MagicMock()
        ai_service = AIService(
            model="cerebras/zai-glm-4.7",
            api_key="test-api-key",
            timeout=30.0,
            request_repo=request_repo,
        )

        with (
            patch(
                "services.ai_service.acompletion",
                new_callable=AsyncMock,
                side_effect=RuntimeError("provider unavailable"),
            ),
            patch("services.ai_service.asyncio.to_thread", new_callable=AsyncMock) as mock_to_thread,
        ):
            mock_to_thread.side_effect = lambda func, *args, **kwargs: func(*args, **kwargs)
            result = await ai_service.complete("prompt", feature="ask.sql")

        assert result is None
        telemetry = request_repo.record_attempt.call_args.kwargs
        assert telemetry["feature"] == "ask.sql"
        assert telemetry["success"] is False
        assert telemetry["error_type"] == "RuntimeError"
        assert telemetry["prompt_tokens"] is None


def _validator_service(ai_query_repo=None):
    """Build a SQLQueryService for validator tests.

    The structural guild-scoping check needs an ai_query_repo for schema ground
    truth; the default stub reports no unscoped tables so tests of the static
    rules are unaffected by it.
    """
    if ai_query_repo is None:
        ai_query_repo = MagicMock()
        ai_query_repo.get_all_tables.return_value = []
        ai_query_repo.get_guild_scoped_tables.return_value = set()
    return SQLQueryService(ai_service=MagicMock(), ai_query_repo=ai_query_repo)


def _build_schema_context_via_public_metadata_methods(ai_query_repo):
    """Reproduce the pre-bulk schema builder for output-parity regression."""
    service = SQLQueryService(ai_service=MagicMock(), ai_query_repo=ai_query_repo)
    lines = ["## Available Tables\n"]
    all_tables = ai_query_repo.get_all_tables()
    blocked_lower = service._blocked_table_names()
    allowed_tables = [
        table for table in all_tables if table.lower() not in blocked_lower
    ]
    blocked_columns = {column.lower() for column in BLOCKED_COLUMNS}

    for table_name in sorted(allowed_tables):
        schema_info = ai_query_repo.get_table_schema(table_name)
        if not schema_info:
            continue
        lines.append(f"### {table_name}")
        col_lines = []
        for column in schema_info:
            if column["name"].lower() in blocked_columns:
                continue
            column_type = column["type"] or "ANY"
            nullable = "" if column["notnull"] else " (nullable)"
            primary_key = " PK" if column["pk"] else ""
            col_lines.append(
                f"  - {column['name']}: {column_type}{primary_key}{nullable}"
            )
        if col_lines:
            lines.extend(col_lines)
            lines.append("")

    lines.append("## Relationships (use for JOINs, don't SELECT these ID columns)")
    relationships = set()
    for table_name in allowed_tables:
        for foreign_key in ai_query_repo.get_foreign_keys(table_name):
            referenced_table = foreign_key["table"]
            if referenced_table.lower() not in blocked_lower:
                relationships.add(
                    f"- {table_name}.{foreign_key['from']} = "
                    f"{referenced_table}.{foreign_key['to']}"
                )
    if relationships:
        lines.extend(sorted(relationships))
    return "\n".join(lines)


class TestSQLQueryService:
    """Tests for SQLQueryService validation and query execution."""

    def test_validate_sql_rejects_non_select(self):
        """Test that non-SELECT queries are rejected."""
        service = _validator_service()

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
        service = _validator_service()

        # Test each dangerous keyword
        dangerous_queries = [
            "SELECT * FROM players; DROP TABLE players",
            "SELECT * FROM players WHERE 1=1; TRUNCATE players",
            "SELECT * FROM (SELECT 1) AS t; ALTER TABLE players ADD x INT",
        ]

        for query in dangerous_queries:
            is_valid, _ = service._validate_sql(query)
            assert not is_valid, f"Should reject: {query}"

    def test_validate_sql_rejects_blocked_columns(self):
        """Test that blocked columns are not allowed."""
        service = _validator_service()

        for column in BLOCKED_COLUMNS:
            query = f"SELECT {column} FROM players"
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Should block column: {column}"
            assert column in error.lower()

    def test_validate_sql_allows_valid_select(self):
        """Test that valid SELECT queries pass validation."""
        service = _validator_service()

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
        service = _validator_service()

        # A blocked column in a UNION branch (over a non-blocked table) must be
        # caught by the projection scan of every branch.
        is_valid, error = service._validate_sql(
            "SELECT discord_username FROM players UNION SELECT discord_id FROM players"
        )
        assert not is_valid
        assert "blocked column" in error.lower()

        # steam_id only lives in player_steam_ids, which is now a blocked table
        # (no guild_id column -> cannot be guild-scoped), so the UNION is rejected
        # at the table layer before the column scan. Either way it must not pass.
        is_valid, error = service._validate_sql(
            "SELECT discord_username FROM players UNION ALL SELECT steam_id FROM player_steam_ids"
        )
        assert not is_valid, "UNION must not bypass the blocklist"

    def test_validate_sql_rejects_quoted_identifier_bypass(self):
        """Double-quoted / bracketed identifiers must not smuggle blocked
        columns or tables past the validator.

        Regression for the bypass where the literal-masker treated a quoted
        identifier like ``"discord_id"`` as opaque string data and masked its
        contents, so the column/table/`*` scans were blind to it while SQLite
        still resolved it to the real column.
        """
        service = _validator_service()

        for query in [
            'SELECT "discord_id" FROM players',
            'SELECT p."discord_id" FROM players p',
            "SELECT [discord_id] FROM players",
            'SELECT avoider_discord_id FROM "soft_avoids"',
            'SELECT "t".* FROM players "t"',
            'SELECT * FROM "players"',
        ]:
            is_valid, _ = service._validate_sql(query)
            assert not is_valid, f"Quoted identifier must not bypass validation: {query}"

    def test_validate_sql_rejects_blocked_columns_via_subquery(self):
        """A blocked column after a projected subquery's FROM must be rejected.

        Regression for the bypass where a subquery before the target column moved
        the first FROM, hiding a projected discord_id from the prefix-only scan.
        Also covers a subquery whose own output is a blocked column under an alias.
        """
        service = _validator_service()

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
        service = _validator_service()

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
        service = _validator_service()

        is_valid, error = service._validate_sql(
            "SELECT discord_username FROM players WHERE discord_username = 'a;b'"
        )
        assert is_valid, f"Semicolon inside a literal should not be rejected: {error}"

    def test_validate_sql_literal_value_does_not_trip_keyword_guard(self):
        """A dangerous keyword appearing only as a string value is harmless data."""
        service = _validator_service()

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
        service = _validator_service()

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

    def test_validate_sql_rejects_comment_tokens(self):
        """SQL comments must be rejected outright.

        SQLite treats a comment as whitespace, so ``main/**/.players`` reads
        past the per-guild TEMP views, ``FROM/**/soft_avoids`` hides a table
        from the blocklist scan, and ``SELECT/**/*`` hides the wildcard — the
        regex-based structural checks do not model comment tokens. Regression
        for that whole bypass class.
        """
        service = _validator_service()

        for query in [
            "SELECT discord_username FROM main/**/.players",
            "SELECT reason FROM/**/soft_avoids",
            "SELECT/**/* FROM players",
            "SELECT reason FROM--\nsoft_avoids",
        ]:
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Comment token must be rejected: {query!r}"
            assert "comment" in error.lower()

    def test_validate_sql_allows_comment_chars_inside_string_literal(self):
        """'--' or '/*' appearing inside a string literal is data, not a comment."""
        service = _validator_service()

        for query in [
            "SELECT discord_username FROM players WHERE discord_username = '--'",
            "SELECT discord_username FROM players WHERE discord_username = 'a/*b'",
        ]:
            is_valid, error = service._validate_sql(query)
            assert is_valid, f"Comment chars inside a literal should pass: {query!r}, got: {error}"

    def test_validate_sql_rejects_player_trivia_tables(self):
        """player_trivia_questions embeds <@discord_id> mentions and live answer
        keys, and has no guild_id column; sessions is internal per-player state.
        Both must be blocked."""
        service = _validator_service()

        for query in [
            "SELECT question_text FROM player_trivia_questions",
            "SELECT score FROM player_trivia_sessions",
        ]:
            is_valid, error = service._validate_sql(query)
            assert not is_valid, f"Trivia table must be blocked: {query}"
            assert "not allowed" in error.lower()

    def test_validate_sql_blocks_unscoped_table_not_in_blocklist(self):
        """A table with no guild_id column is blocked even when BLOCKED_TABLES
        does not name it (fail closed): the per-guild TEMP views cannot shadow
        it, so it would expose every guild's rows."""
        repo = MagicMock()
        repo.get_all_tables.return_value = ["players", "new_global_table"]
        repo.get_guild_scoped_tables.return_value = {"players"}
        service = _validator_service(repo)

        is_valid, _ = service._validate_sql("SELECT label FROM new_global_table")
        assert not is_valid, "Unscoped table must be blocked without a BLOCKED_TABLES entry"

        is_valid, error = service._validate_sql("SELECT discord_username FROM players")
        assert is_valid, f"Guild-scoped table must stay allowed: {error}"

    def test_schema_context_bulk_metadata_preserves_output_and_cache(
        self, repo_db_path
    ):
        repo = AIQueryRepository(repo_db_path)
        expected = _build_schema_context_via_public_metadata_methods(repo)
        service = SQLQueryService(ai_service=MagicMock(), ai_query_repo=repo)

        with patch.object(
            repo, "readonly_connection", wraps=repo.readonly_connection
        ) as connection:
            first = service._build_schema_context()
            second = service._build_schema_context()

        assert first == expected
        assert second == first
        assert connection.call_count == 1

    def test_schema_context_bulk_metadata_blocks_unscoped_tables_fail_closed(self):
        repo = MagicMock()
        repo.get_schema_metadata.return_value = {
            "players": {
                "columns": [
                    {"name": "guild_id", "type": "INTEGER", "notnull": 1, "pk": 1},
                    {
                        "name": "discord_username",
                        "type": "TEXT",
                        "notnull": 1,
                        "pk": 0,
                    },
                ],
                "foreign_keys": [],
            },
            "new_global_table": {
                "columns": [
                    {"name": "label", "type": "TEXT", "notnull": 0, "pk": 0}
                ],
                "foreign_keys": [],
            },
        }
        service = SQLQueryService(ai_service=MagicMock(), ai_query_repo=repo)

        context = service._build_schema_context()

        assert "### players" in context
        assert "new_global_table" not in context
        assert "new_global_table" in service._blocked_table_names()
        repo.get_schema_metadata.assert_called_once_with()
        repo.get_table_schema.assert_not_called()
        repo.get_foreign_keys.assert_not_called()

    def test_no_guild_id_tables_are_blocked_or_allowlisted(self, repo_db_path):
        """Every base table in the live schema without a guild_id column must be
        named in BLOCKED_TABLES or UNSCOPED_TABLE_ALLOWLIST.

        The structural check in _validate_sql enforces the blocking at runtime;
        this pins the documented lists against schema drift so a new global
        table must be consciously classified."""
        repo = AIQueryRepository(repo_db_path)
        unscoped = set(repo.get_all_tables()) - repo.get_guild_scoped_tables()
        unclassified = {
            t for t in unscoped if t not in BLOCKED_TABLES and t not in UNSCOPED_TABLE_ALLOWLIST
        }
        assert not unclassified, (
            f"Tables without guild_id must be blocked or explicitly allowlisted: {unclassified}"
        )

    def test_blocked_tables_blocklist(self):
        """Test that sensitive tables are in the blocklist."""
        assert "sqlite_sequence" in BLOCKED_TABLES
        assert "schema_migrations" in BLOCKED_TABLES
        assert "pending_matches" in BLOCKED_TABLES
        assert "guild_config" in BLOCKED_TABLES
        assert "llm_request_attempts" in BLOCKED_TABLES
        assert "player_trivia_questions" in BLOCKED_TABLES
        assert "player_trivia_sessions" in BLOCKED_TABLES

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

    def test_get_schema_metadata_uses_one_connection_and_preserves_pragma_order(
        self, ai_query_repo
    ):
        with patch.object(
            ai_query_repo,
            "readonly_connection",
            wraps=ai_query_repo.readonly_connection,
        ) as connection:
            metadata = ai_query_repo.get_schema_metadata()

        assert connection.call_count == 1
        assert list(metadata) == ai_query_repo.get_all_tables()
        for table_name in ("players", "match_participants"):
            assert metadata[table_name]["columns"] == ai_query_repo.get_table_schema(
                table_name
            )
            assert metadata[table_name][
                "foreign_keys"
            ] == ai_query_repo.get_foreign_keys(table_name)

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
