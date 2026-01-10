"""
SQL Query Service for natural language to SQL translation.

Provides safe, validated SQL generation from natural language questions
using AI with strict whitelist enforcement.
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from repositories.ai_query_repository import AIQueryRepository
    from repositories.interfaces import IGuildConfigRepository
    from services.ai_service import AIService

logger = logging.getLogger("cama_bot.services.sql_query")


# Schema whitelist - only these tables and columns can be queried
ALLOWED_TABLES: dict[str, list[str]] = {
    "players": [
        "discord_username",
        "glicko_rating",
        "glicko_rd",
        "glicko_volatility",
        "wins",
        "losses",
        "jopacoin_balance",
        "exclusion_count",
        "preferred_roles",
        "main_role",
        "initial_mmr",
        "current_mmr",
        "lowest_balance_ever",
    ],
    "matches": [
        "match_id",
        "team1_players",
        "team2_players",
        "winning_team",
        "match_date",
        "duration_seconds",
        "radiant_score",
        "dire_score",
        "game_mode",
        "valve_match_id",
    ],
    "match_participants": [
        "match_id",
        "team_number",
        "won",
        "side",
        "hero_id",
        "kills",
        "deaths",
        "assists",
        "last_hits",
        "denies",
        "gpm",
        "xpm",
        "hero_damage",
        "tower_damage",
        "net_worth",
        "hero_healing",
        "lane_role",
        "lane_efficiency",
    ],
    "rating_history": [
        "match_id",
        "rating",
        "rating_before",
        "rd_before",
        "rd_after",
        "expected_team_win_prob",
        "team_number",
        "won",
    ],
    "bets": [
        "match_id",
        "team_bet_on",
        "amount",
        "leverage",
        "payout",
        "is_blind",
        "odds_at_placement",
    ],
    "player_pairings": [
        "player1_id",
        "player2_id",
        "games_together",
        "wins_together",
        "games_against",
        "player1_wins_against",
    ],
    "loan_state": [
        "total_loans_taken",
        "total_fees_paid",
        "outstanding_principal",
        "outstanding_fee",
        "negative_loans_taken",
    ],
    "bankruptcy_state": [
        "penalty_games_remaining",
        "last_bankruptcy_at",
    ],
    "predictions": [
        "prediction_id",
        "question",
        "status",
        "outcome",
        "closes_at",
        "resolved_at",
    ],
    "prediction_bets": [
        "prediction_id",
        "position",
        "amount",
        "payout",
        "bet_time",
    ],
    "match_predictions": [
        "match_id",
        "radiant_rating",
        "dire_rating",
        "expected_radiant_win_prob",
    ],
}

# Columns that should never be exposed
BLOCKED_COLUMNS: set[str] = {
    "discord_id",
    "steam_id",
    "dotabuff_url",
    "created_at",
    "updated_at",
    "creator_id",
    "resolved_by",
    "channel_id",
    "thread_id",
    "message_id",
    "embed_message_id",
    "channel_message_id",
    "close_message_id",
    "resolution_votes",
    "enrichment_data",
    "payload",
    "api_key",
}

# SQL keywords that indicate write operations
DANGEROUS_KEYWORDS: set[str] = {
    "INSERT",
    "UPDATE",
    "DELETE",
    "DROP",
    "ALTER",
    "CREATE",
    "TRUNCATE",
    "REPLACE",
    "GRANT",
    "REVOKE",
    "ATTACH",
    "DETACH",
    "PRAGMA",
    "VACUUM",
    "REINDEX",
}


@dataclass
class QueryResult:
    """Result of a SQL query execution."""

    success: bool
    sql: str | None = None
    explanation: str | None = None
    results: list[dict[str, Any]] = field(default_factory=list)
    row_count: int = 0
    error: str | None = None

    def format_for_discord(self, max_length: int = 1024) -> str:
        """
        Format query results for Discord embed display.

        Args:
            max_length: Maximum character length for output

        Returns:
            Formatted string suitable for Discord embed
        """
        if not self.success:
            return f"Error: {self.error}"

        if not self.results:
            return "No results found."

        lines = []
        show_numbers = len(self.results) > 1  # Only number multi-row results

        # Format results in a clean, readable way
        for i, row in enumerate(self.results[:10], 1):  # Limit to 10 rows for display
            # Build a natural-looking row
            parts = []
            name_field = None

            # Find the "name" field first (discord_username, question, etc.)
            for key in ["discord_username", "username", "name", "question"]:
                if key in row and row[key]:
                    name_field = row[key]
                    break

            if name_field:
                parts.append(f"**{name_field}**")

            # Add other fields with cleaner formatting
            for key, value in row.items():
                if key in ["discord_username", "username", "name", "question"]:
                    continue  # Already handled
                if value is None:
                    continue

                # Format based on key name for readability
                if isinstance(value, float):
                    if "rate" in key.lower() or "prob" in key.lower():
                        if value > 1:
                            parts.append(f"{value:.1f}% {_humanize_key(key)}")
                        else:
                            parts.append(f"{value:.0%} {_humanize_key(key)}")
                    elif "rating" in key.lower():
                        parts.append(f"{value:.0f} rating")
                    else:
                        parts.append(f"{value:.1f} {_humanize_key(key)}")
                elif isinstance(value, int):
                    parts.append(f"{value} {_humanize_key(key)}")
                else:
                    parts.append(f"{_humanize_key(key)}: {value}")

            if show_numbers:
                lines.append(f"{i}. " + " • ".join(parts))
            else:
                lines.append(" • ".join(parts))

        if self.row_count > 10:
            lines.append(f"\n*...and {self.row_count - 10} more*")

        output = "\n".join(lines)

        # Truncate if too long
        if len(output) > max_length:
            output = output[: max_length - 20] + "\n*...truncated*"

        return output


def _humanize_key(key: str) -> str:
    """Convert snake_case key to readable label."""
    # Common abbreviations and terms
    replacements = {
        "glicko_rating": "rating",
        "glicko_rd": "uncertainty",
        "discord_username": "player",
        "jopacoin_balance": "jopacoin",
        "win_rate": "win rate",
        "games_together": "games together",
        "games_against": "games against",
        "wins_together": "wins together",
        "total_loans_taken": "loans",
        "total_fees_paid": "fees paid",
        "outstanding_principal": "debt",
    }
    if key in replacements:
        return replacements[key]
    # Default: replace underscores with spaces
    return key.replace("_", " ")


class SQLQueryService:
    """
    Service for translating natural language questions to safe SQL queries.

    Features:
    - AI-powered NL-to-SQL translation
    - Multi-layer SQL validation
    - Schema whitelist enforcement
    - Read-only execution
    """

    def __init__(
        self,
        ai_service: AIService,
        ai_query_repo: AIQueryRepository,
        guild_config_repo: IGuildConfigRepository | None = None,
    ):
        """
        Initialize SQLQueryService.

        Args:
            ai_service: AI service for query generation
            ai_query_repo: Repository for safe query execution
            guild_config_repo: Optional guild config for AI toggle
        """
        self.ai_service = ai_service
        self.ai_query_repo = ai_query_repo
        self.guild_config_repo = guild_config_repo

    async def query(
        self,
        guild_id: int | None,
        question: str,
    ) -> QueryResult:
        """
        Translate a natural language question to SQL and execute it.

        Args:
            guild_id: Guild ID to check AI enabled (None = always enabled)
            question: User's question in natural language

        Returns:
            QueryResult with success status, SQL, and results
        """
        # 1. Check if AI features are enabled for this guild
        if guild_id is not None and self.guild_config_repo:
            if not self.guild_config_repo.get_ai_enabled(guild_id):
                return QueryResult(
                    success=False,
                    error="AI features are not enabled for this server. An admin can enable them.",
                )

        # 2. Build schema context for the AI
        schema_ctx = self._build_schema_context()

        # 3. Generate SQL via AI
        logger.info(f"Generating SQL for question: {question[:100]}...")
        result = await self.ai_service.generate_sql(question, schema_ctx)

        if "error" in result:
            return QueryResult(success=False, error=result["error"])

        sql = result.get("sql", "")
        explanation = result.get("explanation", "")

        # 4. Validate the generated SQL
        is_valid, validation_error = self._validate_sql(sql)
        if not is_valid:
            logger.warning(f"SQL validation failed: {validation_error}\nSQL: {sql}")
            return QueryResult(
                success=False,
                error=f"Query validation failed: {validation_error}",
                sql=sql,
            )

        # 5. Execute the query
        try:
            rows = self.ai_query_repo.execute_readonly(sql, max_rows=25)
            logger.info(f"Query executed successfully, {len(rows)} rows returned")
            return QueryResult(
                success=True,
                sql=sql,
                explanation=explanation,
                results=rows,
                row_count=len(rows),
            )
        except Exception as e:
            logger.error(f"Query execution failed: {e}\nSQL: {sql}")
            return QueryResult(
                success=False,
                error=f"Query execution failed: {str(e)}",
                sql=sql,
            )

    def _build_schema_context(self) -> str:
        """
        Build schema description for AI context.

        Returns:
            String describing available tables and columns
        """
        lines = []
        for table, columns in ALLOWED_TABLES.items():
            col_list = ", ".join(columns)
            lines.append(f"- {table}: {col_list}")

        # Add join hints
        lines.append("")
        lines.append("Table relationships (use discord_id for joins, but don't SELECT it):")
        lines.append("- players.discord_id = loan_state.discord_id")
        lines.append("- players.discord_id = bankruptcy_state.discord_id")
        lines.append("- players.discord_id = bets.discord_id")
        lines.append("- players.discord_id = prediction_bets.discord_id")
        lines.append("- matches.match_id = match_participants.match_id")
        lines.append("- matches.match_id = bets.match_id")
        lines.append("- predictions.prediction_id = prediction_bets.prediction_id")

        return "\n".join(lines)

    def _validate_sql(self, sql: str) -> tuple[bool, str]:
        """
        Multi-layer SQL validation.

        Validates:
        1. Query starts with SELECT
        2. No dangerous keywords (INSERT, UPDATE, etc.)
        3. All tables are in whitelist
        4. No blocked columns are referenced

        Args:
            sql: SQL query to validate

        Returns:
            Tuple of (is_valid, error_message)
        """
        if not sql or not sql.strip():
            return False, "Empty query"

        sql_upper = sql.upper().strip()

        # 1. Must start with SELECT
        if not sql_upper.startswith("SELECT"):
            return False, "Only SELECT queries are allowed"

        # 2. Check for dangerous keywords
        for keyword in DANGEROUS_KEYWORDS:
            # Use word boundary matching to avoid false positives
            if re.search(rf"\b{keyword}\b", sql_upper):
                return False, f"Forbidden keyword: {keyword}"

        # 3. Check for multiple statements (semicolon followed by more SQL)
        # Allow trailing semicolon but not multiple statements
        statements = [s.strip() for s in sql.split(";") if s.strip()]
        if len(statements) > 1:
            return False, "Multiple statements not allowed"

        # 4. Extract and validate table names
        tables = self._extract_tables(sql)
        for table in tables:
            if table.lower() not in {t.lower() for t in ALLOWED_TABLES}:
                return False, f"Table not allowed: {table}"

        # 5. Extract and check for blocked columns
        # This is a best-effort check - complex queries may bypass it
        blocked = self._check_blocked_columns(sql)
        if blocked:
            return False, f"Blocked column(s): {', '.join(blocked)}"

        return True, ""

    def _extract_tables(self, sql: str) -> list[str]:
        """
        Extract table names from SQL query.

        Only extracts from FROM and JOIN clauses - table aliases in
        column references (e.g., p.column) are not treated as tables.
        """
        tables = []

        # Pattern for FROM and JOIN clauses
        # Handles: FROM table, FROM table AS alias, JOIN table, etc.
        patterns = [
            r"\bFROM\s+(\w+)",
            r"\bJOIN\s+(\w+)",
        ]

        sql_upper = sql.upper()

        for pattern in patterns:
            matches = re.findall(pattern, sql_upper)
            tables.extend(matches)

        return list(set(tables))

    def _check_blocked_columns(self, sql: str) -> list[str]:
        """
        Check if any blocked columns are in the SELECT clause.

        Only checks SELECT clause - allows blocked columns in JOIN/WHERE.
        Returns list of blocked columns found in SELECT.
        """
        found_blocked = []
        sql_lower = sql.lower()

        # Extract just the SELECT clause (before FROM)
        from_match = re.search(r'\bfrom\b', sql_lower)
        if from_match:
            select_clause = sql_lower[:from_match.start()]
        else:
            select_clause = sql_lower

        for col in BLOCKED_COLUMNS:
            # Check for column in SELECT clause only
            patterns = [
                rf"\b{col.lower()}\b",  # Simple reference
                rf"\.\s*{col.lower()}\b",  # table.column
            ]
            for pattern in patterns:
                if re.search(pattern, select_clause):
                    found_blocked.append(col)
                    break

        return found_blocked
