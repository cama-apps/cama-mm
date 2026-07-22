"""
Provider-neutral AI service wrapper built on LiteLLM.

Provides unified interface for LLM calls with tool calling support.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import sys
import time
import warnings
from dataclasses import dataclass
from functools import lru_cache
from typing import TYPE_CHECKING, Any

from services.monitoring_service import get_global_usage_monitor

if TYPE_CHECKING:
    from repositories.interfaces import ILLMRequestRepository
    from services.flavor_personas import FlavorPersona

logger = logging.getLogger("cama_bot.services.ai")

_LITELLM_PYDANTIC_SERIALIZER_WARNING = (
    r"Pydantic serializer warnings:\n\s+"
    r"PydanticSerializationUnexpectedValue\(Expected 10 fields but got 5: "
    r"Expected `Message`"
)


def _suppress_litellm_pydantic_warnings() -> None:
    warnings.filterwarnings(
        "ignore",
        message=_LITELLM_PYDANTIC_SERIALIZER_WARNING,
        category=UserWarning,
    )


_suppress_litellm_pydantic_warnings()

# Strip control chars + newlines + backticks from values that are interpolated
# into LLM prompts so a hostile display name can't end the prompt block early
# or smuggle in a fake instruction line.
_PROMPT_UNSAFE_CHARS = re.compile(r"[\x00-\x1f\x7f`]")


@lru_cache(maxsize=1)
def _get_litellm() -> Any:
    """Import and configure LiteLLM on the first actual provider call."""
    import litellm

    # Disable LiteLLM's automatic retries - callers in this service fail fast.
    litellm.num_retries = 0
    return litellm


def acompletion(**kwargs: Any) -> Any:
    """Return LiteLLM's completion coroutine while preserving the test seam.

    ``AIService._invoke`` prepares the lazy import in a worker thread before
    evaluating this wrapper, so first use neither blocks the event loop nor
    consumes the provider's hard timeout budget.
    """
    return _get_litellm().acompletion(**kwargs)


_DEFAULT_ACOMPLETION = acompletion


def _litellm_error_kind(exc: Exception) -> str | None:
    """Classify provider errors without importing LiteLLM just to inspect one."""
    litellm_module = sys.modules.get("litellm")
    if litellm_module is None:
        return None
    if isinstance(exc, litellm_module.RateLimitError):
        return "rate_limit"
    if isinstance(exc, litellm_module.Timeout):
        return "timeout"
    return None


def _sanitize_for_prompt(value: str | None, *, fallback: str = "Unknown", max_len: int = 64) -> str:
    if not value:
        return fallback
    cleaned = _PROMPT_UNSAFE_CHARS.sub("", value).strip()
    return (cleaned[:max_len] or fallback)


def _token_count(value: Any) -> int | None:
    """Return a provider token count without coercing mock/arbitrary objects."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    count = int(value)
    return count if count >= 0 else None


def _extract_usage_tokens(response: Any) -> tuple[int | None, int | None, int | None]:
    """Extract standard token metadata from LiteLLM object or mapping responses."""
    if response is None:
        return None, None, None
    if isinstance(response, dict):
        usage = response.get("usage")
    else:
        usage = getattr(response, "usage", None)
    if usage is None:
        return None, None, None

    def read(name: str) -> Any:
        if isinstance(usage, dict):
            return usage.get(name)
        return getattr(usage, name, None)

    return (
        _token_count(read("prompt_tokens")),
        _token_count(read("completion_tokens")),
        _token_count(read("total_tokens")),
    )


# Tool definitions for structured outputs
SQL_TOOL = {
    "type": "function",
    "function": {
        "name": "execute_sql_query",
        "description": "Execute a focused SQL query. Select ONLY 1-3 columns that directly answer the question. Always include the player name and the key metric.",
        "parameters": {
            "type": "object",
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "Minimal SELECT query with only essential columns. Example: SELECT discord_username, total_loans_taken FROM... (not SELECT *)",
                },
                "explanation": {
                    "type": "string",
                    "description": "One sentence explaining what this returns",
                },
            },
            "required": ["sql", "explanation"],
        },
    },
}

FLAVOR_TOOL = {
    "type": "function",
    "function": {
        "name": "generate_flavor_text",
        "description": "Generate a short, snarky comment about a player event",
        "parameters": {
            "type": "object",
            "properties": {
                "comment": {
                    "type": "string",
                    "description": "1-2 sentence roast/comment. Be funny and reference the player's history.",
                },
                "tone": {
                    "type": "string",
                    "enum": ["roast", "congratulations", "sympathy", "shock"],
                    "description": "The tone of the comment",
                },
            },
            "required": ["comment"],
        },
    },
}


@dataclass
class ToolCallResult:
    """Result from a tool-calling LLM invocation."""

    tool_name: str | None
    tool_args: dict[str, Any]
    content: str | None = None
    raw_response: Any = None


class AIService:
    """
    Provider-neutral LiteLLM wrapper for Groq and Cerebras models.

    Provides methods for:
    - General completions
    - Tool-calling completions (for structured outputs)
    - SQL query generation
    - Flavor text generation
    """

    def __init__(
        self,
        model: str,
        api_key: str,
        timeout: float = 3.0,
        max_tokens: int = 500,
        request_repo: ILLMRequestRepository | None = None,
    ):
        """
        Initialize AIService.

        Args:
            model: LiteLLM model identifier (e.g., "cerebras/gemma-4-31b")
            api_key: API key for the model provider
            timeout: Request timeout in seconds (default 3s to avoid Discord interaction timeout)
            max_tokens: Maximum tokens in response
            request_repo: Optional metadata-only LLM request telemetry repository
        """
        self.model = model
        self.api_key = api_key
        self.timeout = timeout
        self.max_tokens = max_tokens
        self.request_repo = request_repo
        self.provider, separator, provider_model = model.partition("/")
        if not separator:
            self.provider = "unknown"
            provider_model = model
        self.provider_model = provider_model
        self._is_groq = model.startswith("groq/")

        logger.info("AIService initialized with model: %s", model)

    def _apply_provider_options(
        self,
        kwargs: dict[str, Any],
        *,
        tool_call: bool,
    ) -> None:
        """Apply only options supported by the selected Groq model family."""
        if not self._is_groq:
            return

        if self.provider_model.startswith("openai/gpt-oss-"):
            # GPT-OSS accepts reasoning_effort but rejects Groq's
            # reasoning_format parameter.
            kwargs["reasoning_effort"] = "low"
        elif self.provider_model.startswith("qwen/"):
            # Keep chain-of-thought out of content. Current Qwen 3.6 supports
            # reasoning_effort=none at Groq even though the pinned LiteLLM
            # model metadata has not caught up yet.
            kwargs["reasoning_format"] = "parsed"
            if tool_call:
                kwargs["reasoning_effort"] = "none"
                if "qwen3.6" in self.provider_model:
                    kwargs["allowed_openai_params"] = ["reasoning_effort"]

        if tool_call:
            kwargs["parallel_tool_calls"] = False

    async def _record_attempt(
        self,
        *,
        feature: str,
        operation: str,
        success: bool,
        latency_ms: float,
        response: Any = None,
        error_type: str | None = None,
    ) -> None:
        """Persist request metadata without ever storing request/response bodies."""
        if self.request_repo is None:
            return

        prompt_tokens, completion_tokens, total_tokens = _extract_usage_tokens(response)
        try:
            await asyncio.to_thread(
                self.request_repo.record_attempt,
                feature=feature,
                operation=operation,
                provider=self.provider,
                model=self.provider_model,
                success=success,
                latency_ms=latency_ms,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                total_tokens=total_tokens,
                error_type=error_type,
            )
        except Exception:
            logger.warning("Failed to persist LLM request telemetry", exc_info=True)

    async def _invoke(
        self,
        kwargs: dict[str, Any],
        *,
        feature: str,
        operation: str,
    ) -> Any:
        """Make one provider attempt and record its operational metadata."""
        monitor = get_global_usage_monitor()
        if monitor is not None:
            monitor.record_api_request("ai")

        started = time.perf_counter()
        try:
            if (
                acompletion is _DEFAULT_ACOMPLETION
                and _get_litellm.cache_info().currsize == 0
            ):
                await asyncio.to_thread(_get_litellm)
            response = await asyncio.wait_for(
                acompletion(**kwargs),
                timeout=self.timeout,
            )
        except BaseException as exc:
            await self._record_attempt(
                feature=feature,
                operation=operation,
                success=False,
                latency_ms=(time.perf_counter() - started) * 1000,
                error_type=type(exc).__name__,
            )
            raise

        await self._record_attempt(
            feature=feature,
            operation=operation,
            success=True,
            latency_ms=(time.perf_counter() - started) * 1000,
            response=response,
        )
        return response

    async def complete(
        self,
        prompt: str,
        system_prompt: str | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        feature: str = "llm.complete",
    ) -> str | None:
        """
        Simple completion without tool calling.

        Args:
            prompt: User prompt
            system_prompt: Optional system message
            temperature: Sampling temperature (0.0-1.0)
            max_tokens: Override max tokens (default: use instance setting)
            feature: Stable workload label used by request telemetry

        Returns:
            Generated text or None on error
        """
        messages = []
        if system_prompt:
            messages.append({"role": "system", "content": system_prompt})
        messages.append({"role": "user", "content": prompt})

        try:
            kwargs: dict[str, Any] = {
                "model": self.model,
                "api_key": self.api_key,
                "messages": messages,
                "temperature": temperature,
                "timeout": self.timeout,
                "max_tokens": self.max_tokens if max_tokens is None else max_tokens,
                "num_retries": 0,  # No retries - fail fast
            }
            self._apply_provider_options(kwargs, tool_call=False)
            response = await self._invoke(
                kwargs,
                feature=feature,
                operation="completion",
            )
            message = response.choices[0].message
            # Only use content field - never use reasoning_content (thinking chain)
            return message.content
        except TimeoutError:
            logger.warning(f"AI hard timeout after {self.timeout}s (failing fast)")
            return None
        except Exception as e:
            error_kind = _litellm_error_kind(e)
            if error_kind == "rate_limit":
                logger.warning(f"AI rate limited (failing fast): {e}")
            elif error_kind == "timeout":
                logger.warning(f"AI timeout (failing fast): {e}")
            else:
                logger.error(f"AI completion failed: {e}")
            return None

    async def call_with_tools(
        self,
        messages: list[dict[str, str]],
        tools: list[dict[str, Any]],
        tool_choice: str | dict[str, Any] = "auto",
        max_tokens: int | None = None,
        temperature: float | None = None,
        feature: str = "llm.tool_call",
    ) -> ToolCallResult:
        """
        Call LLM with tool definitions and return tool call results.

        Args:
            messages: List of message dicts with role and content
            tools: List of tool definitions
            tool_choice: Tool selection mode ("auto", "none", or specific tool)
            temperature: Optional sampling temperature override
            feature: Stable workload label used by request telemetry

        Returns:
            ToolCallResult with tool name, args, and raw response
        """
        try:
            kwargs: dict[str, Any] = {
                "model": self.model,
                "api_key": self.api_key,
                "messages": messages,
                "tools": tools,
                "tool_choice": tool_choice,
                "timeout": self.timeout,
                "max_tokens": self.max_tokens if max_tokens is None else max_tokens,
                "num_retries": 0,  # No retries - fail fast
            }
            if temperature is not None:
                kwargs["temperature"] = temperature
            self._apply_provider_options(kwargs, tool_call=True)
            response = await self._invoke(
                kwargs,
                feature=feature,
                operation="tool_call",
            )

            message = response.choices[0].message

            # Extract tool call if present
            if hasattr(message, "tool_calls") and message.tool_calls:
                tool_call = message.tool_calls[0]
                try:
                    args = json.loads(tool_call.function.arguments)
                except json.JSONDecodeError:
                    args = {}

                return ToolCallResult(
                    tool_name=tool_call.function.name,
                    tool_args=args,
                    raw_response=response,
                )

            # Fallback if no tool call
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=message.content,
                raw_response=response,
            )

        except TimeoutError:
            logger.warning(f"AI tool call hard timeout after {self.timeout}s (failing fast)")
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=None,
            )
        except Exception as e:
            error_kind = _litellm_error_kind(e)
            if error_kind == "rate_limit":
                logger.warning(f"AI rate limited (failing fast): {e}")
            elif error_kind == "timeout":
                logger.warning(f"AI timeout (failing fast): {e}")
            else:
                logger.error(f"AI tool call failed: {e}")
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=None,
            )

    async def generate_sql(
        self,
        question: str,
        schema_context: str,
        asker_discord_id: int | None = None,
        asker_username: str | None = None,
    ) -> dict[str, Any]:
        """
        Generate SQL query for a natural language question.

        Args:
            question: User's question in natural language
            schema_context: Database schema description for context
            asker_discord_id: Discord ID of the person asking (for "my" queries)
            asker_username: Username of the person asking

        Returns:
            Dict with "sql" and "explanation" keys, or "error" on failure
        """
        # Build asker context for self-referential queries.
        # Username is user-controlled (stored at registration); sanitize before
        # interpolating so a display name can't smuggle prompt directives.
        asker_context = ""
        if asker_discord_id:
            safe_username = _sanitize_for_prompt(asker_username)
            asker_context = f"""
The person asking this question:
- discord_id: {asker_discord_id}
- discord_username: {safe_username}

When they say "me", "my", "I", or "myself", use their discord_id in WHERE clauses.
Example: "what's my win rate?" → WHERE discord_id = {asker_discord_id}
"""

        messages = [
            {
                "role": "system",
                "content": f"""You are a SQL query generator. Attempt to answer the intent of the user's question.

Database schema:
{schema_context}
{asker_context}
Rules:
- Select ONLY the columns needed to answer the question
- Always include discord_username for player queries
- Never include: discord_id, steam_id, dotabuff_url, timestamps
- LIMIT 10 unless asked for more for lists, LIMIT 1 for "who has the most" questions
- Use proper SQLite syntax

Good: SELECT discord_username, total_loans_taken FROM players p JOIN loan_state l ON p.discord_id = l.discord_id ORDER BY total_loans_taken DESC LIMIT 1
Bad: SELECT * FROM players JOIN loan_state... (too many columns)""",
            },
            {"role": "user", "content": question},
        ]

        result = await self.call_with_tools(
            messages=messages,
            tools=[SQL_TOOL],
            tool_choice={"type": "function", "function": {"name": "execute_sql_query"}},
            feature="ask.sql",
        )

        if result.tool_name == "execute_sql_query" and result.tool_args.get("sql"):
            return result.tool_args
        return {"error": "Failed to generate SQL query"}

    async def generate_flavor(
        self,
        event_type: str,
        player_context: dict[str, Any],
        event_details: dict[str, Any],
        examples: list[str],
        persona: FlavorPersona | None = None,
    ) -> str | None:
        """
        Generate flavor text for a player event.

        Args:
            event_type: Type of event (e.g., "loan_taken", "bankruptcy")
            player_context: Dict with player stats and history
            event_details: Dict with event-specific details
            examples: List of example comments for tone matching
            persona: Optional persona that overrides voice + few-shot examples
                for match_win and mvp_callout events

        Returns:
            Generated comment string or None on failure
        """
        examples_text = "\n".join(f"- {ex}" for ex in examples) if examples else "None"
        # Persona-driven calls bump temperature for variety; other events
        # keep the model's default sampling.
        call_temperature: float | None = None

        # Shop events need a FLEX prompt, not a roast prompt
        is_shop_event = event_type in ("shop_announce", "shop_announce_target")
        is_targeted_shop = event_type == "shop_announce_target"

        if is_shop_event:
            if is_targeted_shop:
                # Targeted flex: hype the buyer, roast the target
                system_prompt = f"""You are a hype man for a Dota 2 gambling Discord.
Generate a SHORT (1-2 sentences) FLEX message. The buyer paid money to flex on someone else.

IMPORTANT:
- HYPE UP the buyer - make them look good, powerful, wealthy
- ROAST the target - use the comparison stats to mock them
- Reference specific advantages the buyer has over the target
- Be cocky, arrogant, and petty on behalf of the buyer
- This is a FLEX, not a roast of the buyer!

Keep it PG-13. No slurs.

Example flex messages:
{examples_text}"""
                target_stats = event_details.get("target_stats", {})
                comparison = event_details.get("comparison", {})
                user_prompt = f"""Event: {event_type}
BUYER (the one flexing): {event_details.get('buyer_name', 'Unknown')}
TARGET (the one being flexed on): {event_details.get('target_name', 'Unknown')}

BUYER STATS:
- Balance: {event_details.get('buyer_balance', 0)} jopacoin
- Rating: {event_details.get('buyer_stats', {}).get('rating') or 'Unknown'}
- Win Rate: {event_details.get('buyer_stats', {}).get('win_rate') or 'Unknown'}%

TARGET STATS (for roasting):
- Balance: {target_stats.get('balance', 0)} jopacoin
- Rating: {target_stats.get('rating') or 'Unknown'}
- Win Rate: {target_stats.get('win_rate') or 'Unknown'}%
- Bankruptcies: {target_stats.get('bankruptcies', 0)}

BUYER'S ADVANTAGES: {comparison.get('buyer_wins', ['none'])}
TARGET'S ADVANTAGES: {comparison.get('target_wins', ['none'])}

Generate a cocky FLEX message that hypes the buyer and mocks the target."""
            else:
                # Self flex: just hype them up
                system_prompt = f"""You are a hype man for a Dota 2 gambling Discord.
Generate a SHORT (1-2 sentences) FLEX message. The player paid money to announce their wealth.

IMPORTANT:
- HYPE THEM UP - make them sound rich, powerful, important
- Be cocky and arrogant on their behalf
- Reference their balance as impressive
- This is a FLEX, they're showing off!

Keep it PG-13. No slurs.

Example flex messages:
{examples_text}"""
                user_prompt = f"""Event: {event_type}
Player: {player_context.get('username', 'Unknown')}
Balance: {event_details.get('buyer_balance', player_context.get('balance', 0))} jopacoin
Cost Paid: {event_details.get('cost_paid', 0)} jopacoin

Generate a cocky FLEX message hyping up their wealth."""
        elif event_type == "match_win":
            # MATCH_WIN: persona-driven hype with narrative-aware framing
            is_underdog = event_details.get("is_underdog")
            is_big_gainer = event_details.get("is_big_gainer")
            expected_prob = event_details.get("expected_win_prob")
            rating_change = event_details.get("rating_change")

            if is_underdog and expected_prob:
                narrative = f"UNDERDOG VICTORY - team only had {expected_prob:.0%} chance to win, they defied the odds"
            elif is_big_gainer and rating_change:
                narrative = (
                    f"BIG CLIMB - gained {rating_change:.0f} rating points this match"
                )
            else:
                narrative = "Solid win, nothing exceptional but still a W"

            persona_examples = persona.examples if persona else examples
            persona_examples_block = (
                "\n".join(f"- {ex}" for ex in persona_examples) if persona_examples else "None"
            )
            persona_voice = (
                persona.system_prompt
                if persona
                else "You are a hype commentator for a Dota 2 inhouse league."
            )
            persona_name = persona.name if persona else "the commentator"

            system_prompt = f"""{persona_voice}

You are commenting on a Dota 2 inhouse league match for this player's WIN.

RULES:
- Stay in character as {persona_name}.
- Reference Dota 2 lore (heroes, items, mechanics, memes) when natural.
- PG-13. No slurs.
- One short comment, soft-cap ~30 words.
- Match the narrative beat below.

Example lines from this persona:
{persona_examples_block}"""
            user_prompt = f"""Player: {player_context.get('username', 'Unknown')}
Narrative beat: {narrative}

Write a single comment in the persona's voice celebrating this win."""
            call_temperature = 0.95
        elif event_type == "mvp_callout":
            # MVP_CALLOUT: persona-driven commentary with rich enriched-match stats
            hero = event_details.get("hero", "Unknown Hero")
            kills = event_details.get("kills", 0)
            deaths = event_details.get("deaths", 0)
            assists = event_details.get("assists", 0)
            gpm = event_details.get("gpm", 0)
            xpm = event_details.get("xpm", 0)
            hero_damage = event_details.get("hero_damage", 0)
            tower_damage = event_details.get("tower_damage", 0)
            net_worth = event_details.get("net_worth", 0)
            fantasy = event_details.get("fantasy_points")
            fantasy_str = f"{fantasy:.1f}" if fantasy is not None else "N/A"

            persona_examples = persona.examples if persona else examples
            persona_examples_block = (
                "\n".join(f"- {ex}" for ex in persona_examples) if persona_examples else "None"
            )
            persona_voice = (
                persona.system_prompt
                if persona
                else "You are a snarky, backhanded commentator for a Dota 2 inhouse league."
            )
            persona_name = persona.name if persona else "the commentator"

            system_prompt = f"""{persona_voice}

You are commenting on a Dota 2 inhouse league match for this player's WIN, with detailed match stats available.

RULES:
- Stay in character as {persona_name}.
- Reference the player's specific match stats (KDA, hero, GPM, etc.) when they stand out.
- Reference Dota 2 lore (heroes, items, mechanics, memes) when natural.
- PG-13. No slurs.
- One short comment, soft-cap ~30 words.

Example lines from this persona:
{persona_examples_block}"""
            user_prompt = f"""Player: {player_context.get('username', 'Unknown')}
Hero: {hero} | KDA: {kills}/{deaths}/{assists} | GPM: {gpm} | XPM: {xpm}
Hero Damage: {hero_damage} | Tower Damage: {tower_damage} | Net Worth: {net_worth}
Fantasy Points: {fantasy_str}

GAMBLING HISTORY:
- Balance: {player_context.get('balance', 0)} jopacoin
- Degen Score: {player_context.get('degen_score') or 'Unknown'}/100
- Bankruptcies: {player_context.get('bankruptcy_count', 0)}
- Bet Win Rate: {player_context.get('bet_win_rate') or 'Unknown'}

Write a single comment in the persona's voice about this player's performance."""
            call_temperature = 0.95
        elif event_type == "bet_last_call":
            # BET_LAST_CALL: betting-announcer persona making the final-minute push
            angle = event_details.get("angle", "taunt_crowd")
            has_bettor = event_details.get("has_bettor", False)
            leader_name = event_details.get("leader_name")
            leader_team = event_details.get("leader_team")
            leader_amount = event_details.get("leader_amount")
            standings = event_details.get("standings", "")
            seconds_left = event_details.get("seconds_left", 60)

            persona_examples = persona.examples if persona else examples
            persona_examples_block = (
                "\n".join(f"- {ex}" for ex in persona_examples) if persona_examples else "None"
            )
            persona_voice = (
                persona.system_prompt
                if persona
                else "You are a hype announcer for a Dota 2 gambling Discord."
            )
            persona_name = persona.name if persona else "the announcer"

            if not has_bettor:
                angle_line = (
                    "Nobody has placed a real bet yet — the pool is empty or only has "
                    "auto-liquidity. Mock the empty pool and DARE someone to be the first in."
                )
            elif angle == "roast_leader":
                angle_line = (
                    f"ROAST the current biggest bettor ({leader_name}) using their gambling "
                    "history, and imply everyone else is too scared to challenge them."
                )
            elif angle == "hype_leader":
                angle_line = (
                    f"HYPE UP the current biggest bettor ({leader_name}) so everyone else fears "
                    "missing out — make them want to bet to deny the payout."
                )
            else:
                angle_line = (
                    "TAUNT everyone who hasn't bet yet. Dare them to step up before the window "
                    "slams shut."
                )

            system_prompt = f"""{persona_voice}

You are making the FINAL CALL for betting on a Dota 2 inhouse match — the window closes in about {seconds_left} seconds.

RULES:
- Stay in character as {persona_name}.
- GOAL: get more people to place a bet RIGHT NOW.
- {angle_line}
- You may reference the live standings/odds below.
- PG-13. No slurs.
- One short, punchy line, soft-cap ~30 words.

Example lines from this persona:
{persona_examples_block}"""

            leader_block = ""
            if has_bettor and leader_name:
                leader_block = (
                    f"Biggest bettor so far: {leader_name} "
                    f"({leader_amount} on {leader_team})\n"
                    f"Their bet win rate: {player_context.get('bet_win_rate') or 'Unknown'} | "
                    f"Degen score: {player_context.get('degen_score') or 'Unknown'}/100 | "
                    f"Bankruptcies: {player_context.get('bankruptcy_count', 0)}\n"
                )
            user_prompt = f"""Live standings: {standings or 'no bets yet'}
{leader_block}Seconds until betting closes: {seconds_left}

Write a single FINAL-CALL line in the persona's voice to drive last-second bets."""
            call_temperature = 0.95
        elif event_type == "bet_warning":
            # BET_WARNING: betting-announcer persona a few minutes out. Less
            # frantic than the final call; when the pool is lopsided, roast or
            # hype the under-bet ("underdog") side to drum up balancing action.
            angle = event_details.get("angle", "taunt_crowd")
            has_bettor = event_details.get("has_bettor", False)
            leader_name = event_details.get("leader_name")
            leader_team = event_details.get("leader_team")
            leader_amount = event_details.get("leader_amount")
            standings = event_details.get("standings", "")
            underdog_side = event_details.get("underdog_side")
            seconds_left = event_details.get("seconds_left", 300)
            minutes_left = max(1, round(seconds_left / 60))

            persona_examples = persona.examples if persona else examples
            persona_examples_block = (
                "\n".join(f"- {ex}" for ex in persona_examples) if persona_examples else "None"
            )
            persona_voice = (
                persona.system_prompt
                if persona
                else "You are a hype announcer for a Dota 2 gambling Discord."
            )
            persona_name = persona.name if persona else "the announcer"

            underdog_label = {"radiant": "Radiant", "dire": "Dire"}.get(underdog_side or "", "")
            if angle == "roast_underdog" and underdog_label:
                angle_line = (
                    f"The pool is badly lopsided AGAINST {underdog_label} — barely anyone is "
                    f"backing them. ROAST {underdog_label} and the cowards too scared to take "
                    "the long-shot payout. Keep it qualitative; don't quote exact numbers."
                )
            elif angle == "hype_underdog" and underdog_label:
                angle_line = (
                    f"The pool is badly lopsided AGAINST {underdog_label}, so a fat underdog "
                    f"payout is sitting there unclaimed. HYPE the value of backing {underdog_label} "
                    "and dare people to take the long shot. Keep it qualitative; don't quote exact numbers."
                )
            elif angle == "roast_leader":
                angle_line = (
                    f"ROAST the current biggest bettor ({leader_name}) using their gambling "
                    "history, and nudge everyone else to get in while there's still time."
                )
            elif angle == "hype_leader":
                angle_line = (
                    f"HYPE UP the current biggest bettor ({leader_name}) so everyone else fears "
                    "missing out — make them want to bet to deny the payout."
                )
            elif not has_bettor:
                angle_line = (
                    "Nobody has placed a real bet yet — the pool is empty or only has "
                    "auto-liquidity. Mock the empty pool and DARE someone to be the first in."
                )
            else:
                angle_line = (
                    "TAUNT everyone who hasn't bet yet. Nudge them to get their bets in while "
                    "there's still plenty of time."
                )

            system_prompt = f"""{persona_voice}

You are rallying bettors on a Dota 2 inhouse match — about {minutes_left} minute(s) left before betting closes. There's still time, so build momentum; do NOT scream a final warning.

RULES:
- Stay in character as {persona_name}.
- GOAL: get more people to place a bet.
- {angle_line}
- You may reference the live standings/odds below, but keep it qualitative.
- PG-13. No slurs.
- One short, punchy line, soft-cap ~30 words.

Example lines from this persona:
{persona_examples_block}"""

            leader_block = ""
            if has_bettor and leader_name:
                leader_block = (
                    f"Biggest bettor so far: {leader_name} "
                    f"({leader_amount} on {leader_team})\n"
                    f"Their bet win rate: {player_context.get('bet_win_rate') or 'Unknown'} | "
                    f"Degen score: {player_context.get('degen_score') or 'Unknown'}/100 | "
                    f"Bankruptcies: {player_context.get('bankruptcy_count', 0)}\n"
                )
            underdog_block = (
                f"Under-bet side (long-shot payout): {underdog_label}\n" if underdog_label else ""
            )
            user_prompt = f"""Live standings: {standings or 'no bets yet'}
{leader_block}{underdog_block}Minutes until betting closes: ~{minutes_left}

Write a single line in the persona's voice to drive bets, matching the angle above."""
            call_temperature = 0.95
        else:
            # Regular roast events
            system_prompt = f"""You are a snarky commentator for a Dota 2 gambling Discord.
Generate a SHORT (1-2 sentences) roast/comment. Be funny, sarcastic, and PERSONALIZED.

IMPORTANT: Reference the player's SPECIFIC history to make the burn personal:
- If they have many loans, mock their loan addiction
- If they have a low bet win rate, roast their gambling skills
- If they've hit rock bottom (lowest_balance), remind them
- If they have big wins but also big losses, call out the volatility
- If they've been in debt multiple times, mock the pattern
- Reference specific numbers when they're embarrassing (e.g., "your 12th loan")

Keep it PG-13. No slurs. Make it PERSONAL using their stats.

Example comments for similar events:
{examples_text}"""
            user_prompt = f"""Event: {event_type}
Player: {player_context.get('username', 'Unknown')}

CURRENT STATE:
- Balance: {player_context.get('balance', 0)} jopacoin
- Debt: {player_context.get('debt_amount') or 'None'}

GAMBLING HISTORY:
- Total Bets: {player_context.get('total_bets', 0)}
- Bet Win Rate: {player_context.get('bet_win_rate') or 'Unknown'}
- Biggest Win: {player_context.get('biggest_win') or 'None'}
- Biggest Loss: {player_context.get('biggest_loss') or 'None'}
- Degen Score: {player_context.get('degen_score') or 'Unknown'}/100

LOAN/DEBT HISTORY:
- Total Loans Taken: {player_context.get('total_loans', 0)}
- Loans While In Debt: {player_context.get('negative_loans', 0)}
- Total Fees Paid: {player_context.get('total_fees_paid', 0)}
- Bankruptcies: {player_context.get('bankruptcy_count', 0)}
- Lowest Balance Ever: {player_context.get('lowest_balance') or 'Unknown'}

MATCH HISTORY:
- Win Rate: {player_context.get('win_rate', 'Unknown')}

Event Details: {json.dumps(event_details)}

Generate a PERSONALIZED roast referencing their specific history."""

        messages = [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt},
        ]

        # Try tool calling first
        result = await self.call_with_tools(
            messages=messages,
            tools=[FLAVOR_TOOL],
            tool_choice="auto",  # Use auto instead of required - more compatible
            temperature=call_temperature,
            feature=f"flavor.{event_type}",
        )

        if result.tool_name == "generate_flavor_text":
            return result.tool_args.get("comment")

        # Fallback: if tool calling failed, try direct completion
        if result.content:
            return result.content

        # Last resort: try a simple completion without tools
        try:
            fallback_result = await self.complete(
                prompt=messages[1]["content"],
                system_prompt=messages[0]["content"]
                + "\n\nRespond with just the roast, nothing else.",
                temperature=call_temperature if call_temperature is not None else 0.9,
                feature=f"flavor.{event_type}",
            )
            return fallback_result
        except Exception:
            return None
