"""
Tests for the /ask command — natural-language SQL via Cerebras LLM.

These tests exercise the AskCommands cog with a mocked SQLQueryService,
focusing on:
- Length / format validation gates (5–500 chars).
- Rate limiting branch.
- Embed assembly for success and failure paths.
- That QueryResult objects are formatted via format_for_discord() before display.
"""

import types
from unittest.mock import AsyncMock

import pytest

import commands.ask as ask_module
from commands.ask import AskCommands
from services.sql_query_service import QueryResult

# ---------------------------------------------------------------------------
# Discord interaction shims
# ---------------------------------------------------------------------------


class FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(
        self,
        content=None,
        embed=None,
        ephemeral=None,
        file=None,
        files=None,
        view=None,
        allowed_mentions=None,
    ):
        self.messages.append(
            {"content": content, "embed": embed, "ephemeral": ephemeral}
        )


class FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self._done = False

    async def send_message(self, content=None, ephemeral=None, embed=None):
        self._done = True
        self.messages.append({"content": content, "ephemeral": ephemeral, "embed": embed})

    async def defer(self, ephemeral=False):
        self._done = True

    def is_done(self):
        return self._done


class FakeInteraction:
    _next_id = 5000

    def __init__(self, *, user_id: int = 42, guild_id: int | None = 12345):
        FakeInteraction._next_id += 1
        self.id = FakeInteraction._next_id
        self.user = types.SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = types.SimpleNamespace(id=guild_id) if guild_id is not None else None
        self.response = FakeResponse()
        self.followup = FakeFollowup()
        self.channel = None


@pytest.fixture(autouse=True)
def patch_safe_io(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kw):
        await interaction.followup.send(**kw)

    monkeypatch.setattr("commands.ask.safe_defer", _safe_defer)
    monkeypatch.setattr("commands.ask.safe_followup", _safe_followup)


@pytest.fixture(autouse=True)
def reset_rate_limiter():
    """Avoid bleed-over state in the module-level RateLimiter across tests."""
    ask_module.AI_RATE_LIMITER._hits.clear()
    ask_module.AI_RATE_LIMITER._next_purge_at = 0.0
    yield
    ask_module.AI_RATE_LIMITER._hits.clear()


def make_cog(query_result: QueryResult | None = None):
    """Construct an AskCommands cog with an AsyncMocked SQLQueryService.query."""
    sql_service = types.SimpleNamespace()
    sql_service.query = AsyncMock(return_value=query_result)
    bot = types.SimpleNamespace()
    return AskCommands(bot, sql_service), sql_service


# ---------------------------------------------------------------------------
# Validation gates
# ---------------------------------------------------------------------------


class TestAskValidation:
    @pytest.mark.asyncio
    async def test_too_short_question_rejected(self):
        cog, sql = make_cog(QueryResult(success=True, results=[], row_count=0))
        interaction = FakeInteraction()

        await cog.ask.callback(cog, interaction, "hi")

        # Should hit the validation branch and never call the SQL service
        sql.query.assert_not_called()
        # Expect ephemeral followup explaining detail required
        assert interaction.followup.messages
        msg = interaction.followup.messages[-1]
        assert msg["ephemeral"] is True
        assert "more detailed" in (msg["content"] or "").lower()

    @pytest.mark.asyncio
    async def test_too_long_question_rejected(self):
        cog, sql = make_cog(QueryResult(success=True, results=[], row_count=0))
        interaction = FakeInteraction()

        await cog.ask.callback(cog, interaction, "x" * 501)

        sql.query.assert_not_called()
        assert interaction.followup.messages
        msg = interaction.followup.messages[-1]
        assert msg["ephemeral"] is True
        assert "too long" in (msg["content"] or "").lower()


# ---------------------------------------------------------------------------
# Rate limiting
# ---------------------------------------------------------------------------


class TestAskRateLimit:
    @pytest.mark.asyncio
    async def test_rate_limit_short_circuits(self, monkeypatch):
        cog, sql = make_cog(QueryResult(success=True, results=[], row_count=0))
        interaction = FakeInteraction()

        # Force the rate limiter to deny.
        rl_result = types.SimpleNamespace(allowed=False, retry_after_seconds=42)
        monkeypatch.setattr(ask_module.AI_RATE_LIMITER, "check", lambda **kw: rl_result)

        await cog.ask.callback(cog, interaction, "what is the average rating?")

        sql.query.assert_not_called()
        # Rate limit responds via interaction.response.send_message (not followup)
        assert interaction.response.messages
        msg = interaction.response.messages[-1]
        assert msg["ephemeral"] is True
        assert "Rate limited" in msg["content"]
        assert "42" in msg["content"]


# ---------------------------------------------------------------------------
# Happy path
# ---------------------------------------------------------------------------


class TestAskHappyPath:
    @pytest.mark.asyncio
    async def test_success_renders_blue_embed_with_formatted_answer(self):
        result = QueryResult(
            success=True,
            sql="SELECT discord_username, wins FROM players ORDER BY wins DESC LIMIT 3",
            explanation="Top winners",
            results=[
                {"discord_username": "Alice", "wins": 12},
                {"discord_username": "Bob", "wins": 7},
            ],
            row_count=2,
        )
        cog, sql = make_cog(result)
        interaction = FakeInteraction()

        await cog.ask.callback(cog, interaction, "who wins the most?")

        sql.query.assert_awaited_once()
        kwargs = sql.query.call_args.kwargs
        # asker_discord_id forwarded
        assert kwargs.get("asker_discord_id") == 42

        # Embed posted via followup
        assert interaction.followup.messages
        embed = interaction.followup.messages[-1]["embed"]
        assert embed is not None
        # Blue (success) per the command code
        import discord
        assert embed.color == discord.Color.blue()
        # Embed contains Question and Answer fields
        names = [f.name for f in embed.fields]
        assert "Question" in names
        assert "Answer" in names
        # The formatted Answer field includes the player names
        answer_text = next(f.value for f in embed.fields if f.name == "Answer")
        assert "Alice" in answer_text
        assert "Bob" in answer_text

    @pytest.mark.asyncio
    async def test_question_truncated_to_256_in_embed(self):
        long_question = "Q" * 400
        result = QueryResult(success=True, results=[], row_count=0)
        cog, _ = make_cog(result)
        interaction = FakeInteraction()

        await cog.ask.callback(cog, interaction, long_question)

        embed = interaction.followup.messages[-1]["embed"]
        question_text = next(f.value for f in embed.fields if f.name == "Question")
        # Embed Question field is truncated at 256
        assert len(question_text) == 256
        assert question_text == "Q" * 256


# ---------------------------------------------------------------------------
# Failure path: backend returns success=False
# ---------------------------------------------------------------------------


class TestAskFailurePath:
    @pytest.mark.asyncio
    async def test_failure_renders_red_embed_without_leaking_error(self):
        # Use an internal error string with a sentinel that does NOT appear
        # in the question, so we can prove the embed body never echoes it.
        result = QueryResult(
            success=False,
            error="ZZZINTERNALSENTINELZZZ near keyword",
        )
        cog, sql = make_cog(result)
        interaction = FakeInteraction()

        await cog.ask.callback(cog, interaction, "what is the average rating?")

        sql.query.assert_awaited_once()

        embed = interaction.followup.messages[-1]["embed"]
        assert embed is not None
        import discord
        assert embed.color == discord.Color.red()
        # The user-facing message should NOT contain the raw error string
        all_text = "\n".join((f.value or "") for f in embed.fields)
        assert "ZZZINTERNALSENTINELZZZ" not in all_text
        # But should contain the safe fallback hint
        assert "I couldn't answer that" in all_text


# ---------------------------------------------------------------------------
# QueryResult.format_for_discord — the only piece of real logic in ask path
# ---------------------------------------------------------------------------


class TestQueryResultFormatting:
    def test_format_no_results(self):
        r = QueryResult(success=True, results=[], row_count=0)
        assert r.format_for_discord() == "No results found."

    def test_format_failure_returns_error(self):
        r = QueryResult(success=False, error="boom")
        assert "boom" in r.format_for_discord()

    def test_format_single_row_does_not_number(self):
        r = QueryResult(
            success=True,
            results=[{"discord_username": "Solo", "wins": 5}],
            row_count=1,
        )
        out = r.format_for_discord()
        # No "1." prefix when there's just one row
        assert not out.startswith("1.")
        assert "Solo" in out
        assert "5" in out

    def test_format_multi_row_numbers_each_line(self):
        r = QueryResult(
            success=True,
            results=[
                {"discord_username": "A", "wins": 10},
                {"discord_username": "B", "wins": 7},
            ],
            row_count=2,
        )
        out = r.format_for_discord()
        assert out.startswith("1.")
        assert "\n2." in out

    def test_format_caps_at_ten_visible_rows(self):
        rows = [{"discord_username": f"P{i}", "wins": i} for i in range(12)]
        r = QueryResult(success=True, results=rows, row_count=12)
        out = r.format_for_discord()
        # Only 10 rows are listed, plus the more-suffix
        # Count "1.", "2."… and confirm "11." is absent
        assert "1. " in out
        assert "10. " in out
        assert "11. " not in out
        assert "and 2 more" in out
