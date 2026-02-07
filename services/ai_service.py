"""
AI Service wrapper for LiteLLM with Cerebras integration.

Provides unified interface for LLM calls with tool calling support.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from typing import Any

import litellm
from litellm import acompletion

logger = logging.getLogger("cama_bot.services.ai")


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
    Wrapper for LiteLLM to interact with Cerebras AI models.

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
    ):
        """
        Initialize AIService.

        Args:
            model: LiteLLM model identifier (e.g., "cerebras/zai-glm-4.7")
            api_key: API key for the model provider
            timeout: Request timeout in seconds (default 3s to avoid Discord interaction timeout)
            max_tokens: Maximum tokens in response
        """
        self.model = model
        self.api_key = api_key
        self.timeout = timeout
        self.max_tokens = max_tokens

        # Configure LiteLLM - set provider-specific API key
        # For Cerebras, we need to set CEREBRAS_API_KEY
        import os

        os.environ["CEREBRAS_API_KEY"] = api_key

        # Disable LiteLLM's automatic retries - we want to fail fast
        litellm.num_retries = 0

        logger.info(f"AIService initialized with model: {model}")

    async def complete(
        self,
        prompt: str,
        system_prompt: str | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
    ) -> str | None:
        """
        Simple completion without tool calling.

        Args:
            prompt: User prompt
            system_prompt: Optional system message
            temperature: Sampling temperature (0.0-1.0)
            max_tokens: Override max tokens (default: use instance setting)

        Returns:
            Generated text or None on error
        """
        messages = []
        if system_prompt:
            messages.append({"role": "system", "content": system_prompt})
        messages.append({"role": "user", "content": prompt})

        try:
            import asyncio

            response = await asyncio.wait_for(
                acompletion(
                    model=self.model,
                    messages=messages,
                    temperature=temperature,
                    timeout=self.timeout,
                    max_tokens=max_tokens or self.max_tokens,
                    num_retries=0,  # No retries - fail fast
                ),
                timeout=self.timeout,
            )
            message = response.choices[0].message
            # Only use content field - never use reasoning_content (thinking chain)
            return message.content
        except asyncio.TimeoutError:
            logger.warning(f"AI hard timeout after {self.timeout}s (failing fast)")
            return None
        except litellm.RateLimitError as e:
            logger.warning(f"AI rate limited (failing fast): {e}")
            return None
        except litellm.Timeout as e:
            logger.warning(f"AI timeout (failing fast): {e}")
            return None
        except Exception as e:
            logger.error(f"AI completion failed: {e}")
            return None

    async def call_with_tools(
        self,
        messages: list[dict[str, str]],
        tools: list[dict[str, Any]],
        tool_choice: str | dict[str, Any] = "auto",
    ) -> ToolCallResult:
        """
        Call LLM with tool definitions and return tool call results.

        Args:
            messages: List of message dicts with role and content
            tools: List of tool definitions
            tool_choice: Tool selection mode ("auto", "none", or specific tool)

        Returns:
            ToolCallResult with tool name, args, and raw response
        """
        try:
            import asyncio

            response = await asyncio.wait_for(
                acompletion(
                    model=self.model,
                    messages=messages,
                    tools=tools,
                    tool_choice=tool_choice,
                    timeout=self.timeout,
                    max_tokens=2000,  # Reasoning models need more tokens for thinking + tool call
                    num_retries=0,  # No retries - fail fast
                ),
                timeout=self.timeout,
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

        except asyncio.TimeoutError:
            logger.warning(f"AI tool call hard timeout after {self.timeout}s (failing fast)")
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=None,
            )
        except litellm.RateLimitError as e:
            logger.warning(f"AI rate limited (failing fast): {e}")
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=None,
            )
        except litellm.Timeout as e:
            logger.warning(f"AI timeout (failing fast): {e}")
            return ToolCallResult(
                tool_name=None,
                tool_args={},
                content=None,
            )
        except Exception as e:
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
        # Build asker context for self-referential queries
        asker_context = ""
        if asker_discord_id:
            asker_context = f"""
The person asking this question:
- discord_id: {asker_discord_id}
- discord_username: {asker_username or 'Unknown'}

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
    ) -> str | None:
        """
        Generate flavor text for a player event.

        Args:
            event_type: Type of event (e.g., "loan_taken", "bankruptcy")
            player_context: Dict with player stats and history
            event_details: Dict with event-specific details
            examples: List of example comments for tone matching

        Returns:
            Generated comment string or None on failure
        """
        examples_text = "\n".join(f"- {ex}" for ex in examples) if examples else "None"

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
            # MATCH_WIN: chill but specific to WHY they're notable
            is_underdog = event_details.get("is_underdog")
            is_big_gainer = event_details.get("is_big_gainer")
            expected_prob = event_details.get("expected_win_prob")
            rating_change = event_details.get("rating_change")

            if is_underdog and expected_prob:
                narrative = f"UNDERDOG VICTORY - team only had {expected_prob:.0%} chance to win, they defied the odds"
                tone_hint = "Acknowledge they proved doubters wrong, beat expectations"
            elif is_big_gainer and rating_change:
                narrative = (
                    f"BIG CLIMB - gained {rating_change:.0f} rating points this match"
                )
                tone_hint = "Acknowledge the rating boost, the grind paying off"
            else:
                narrative = "Solid win, nothing exceptional but still a W"
                tone_hint = "Just a simple acknowledgment"

            system_prompt = f"""You are a chill commentator for a Dota 2 inhouse league.
Generate ONE short sentence (max 15 words) acknowledging this player's win.

IMPORTANT: Your comment should reflect WHY this player is notable:
- {tone_hint}

RULES:
- Keep it brief and understated (no ALL CAPS, no excessive hype)
- Reference their specific accomplishment if there is one
- Be genuine, not generic

Example tone:
{examples_text}"""
            user_prompt = f"""Player: {player_context.get('username', 'Unknown')}
Narrative: {narrative}

Write one sentence that acknowledges their specific accomplishment."""
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
                temperature=0.9,
            )
            return fallback_result
        except Exception:
            return None
