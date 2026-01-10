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


# Blocklist approach - block sensitive tables/columns, allow everything else

# Tables that should never be queried (internal/transient/sensitive)
BLOCKED_TABLES: set[str] = {
    # SQLite internals
    "sqlite_sequence",
    "sqlite_master",
    # Schema management
    "schema_migrations",
    # Transient state
    "pending_matches",
    "lobby_state",
    # Server config
    "guild_config",
    # Internal voting/proposals
    "nonprofit_fund",
    "disburse_proposals",
    "disburse_votes",
}

# Columns that should never appear in SELECT results (PII/internal)
BLOCKED_COLUMNS: set[str] = {
    # PII - can identify real people
    "discord_id",
    "steam_id",
    "dotabuff_url",
    # Internal routing/references
    "guild_id",
    "creator_id",
    "resolved_by",
    # Discord internal IDs
    "channel_id",
    "thread_id",
    "message_id",
    "embed_message_id",
    "channel_message_id",
    "close_message_id",
    # Internal timestamps
    "created_at",
    "updated_at",
    "timestamp",
    "applied_at",
    # Internal data blobs
    "enrichment_data",
    "payload",
    "resolution_votes",
    "recipients",
    "notes",
    # Internal metadata
    "enrichment_source",
    "enrichment_confidence",
    # Redundant/internal IDs
    "dotabuff_match_id",
    "id",  # Generic auto-increment IDs (use specific IDs like match_id instead)
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
        self._schema_cache: str | None = None

    async def query(
        self,
        guild_id: int | None,
        question: str,
        asker_discord_id: int | None = None,
    ) -> QueryResult:
        """
        Translate a natural language question to SQL and execute it.

        Args:
            guild_id: Guild ID to check AI enabled (None = always enabled)
            question: User's question in natural language
            asker_discord_id: Discord ID of the user asking (for "my stats" context)

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

        # 3. Look up asker's username for self-referential queries
        asker_username = None
        if asker_discord_id:
            try:
                row = self.ai_query_repo.execute_readonly(
                    "SELECT discord_username FROM players WHERE discord_id = ?",
                    params=(asker_discord_id,),
                    max_rows=1,
                )
                if row:
                    asker_username = row[0].get("discord_username")
            except Exception as e:
                logger.debug(f"Could not look up asker username: {e}")

        # 4. Generate SQL via AI
        logger.info(f"Generating SQL for question: {question[:100]}...")
        result = await self.ai_service.generate_sql(
            question, schema_ctx, asker_discord_id=asker_discord_id, asker_username=asker_username
        )

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
        Build schema description from actual database structure.

        Uses blocklist approach - includes all tables/columns except blocked ones.
        Caches the result for performance.

        Returns:
            String describing available tables and columns with types
        """
        if self._schema_cache is not None:
            return self._schema_cache

        lines = ["## Available Tables\n"]

        # Get all tables from DB, filter out blocked ones
        try:
            all_tables = self.ai_query_repo.get_all_tables()
        except Exception as e:
            logger.error(f"Failed to get tables: {e}")
            all_tables = []

        allowed_tables = [t for t in all_tables if t.lower() not in {b.lower() for b in BLOCKED_TABLES}]

        for table_name in sorted(allowed_tables):
            try:
                schema_info = self.ai_query_repo.get_table_schema(table_name)
                if not schema_info:
                    continue

                lines.append(f"### {table_name}")

                col_lines = []
                for col in schema_info:
                    col_name = col["name"]
                    # Skip blocked columns
                    if col_name.lower() in {b.lower() for b in BLOCKED_COLUMNS}:
                        continue

                    col_type = col["type"] or "ANY"
                    nullable = "" if col["notnull"] else " (nullable)"
                    pk = " PK" if col["pk"] else ""
                    col_lines.append(f"  - {col_name}: {col_type}{pk}{nullable}")

                if col_lines:
                    lines.extend(col_lines)
                    lines.append("")

            except Exception as e:
                logger.warning(f"Failed to get schema for {table_name}: {e}")

        # Introspect foreign key relationships
        lines.append("## Relationships (use for JOINs, don't SELECT these ID columns)")
        fk_relationships = set()
        for table_name in allowed_tables:
            try:
                fks = self.ai_query_repo.get_foreign_keys(table_name)
                for fk in fks:
                    ref_table = fk["table"]
                    from_col = fk["from"]
                    to_col = fk["to"]
                    # Only include if referenced table is also allowed
                    if ref_table.lower() not in {b.lower() for b in BLOCKED_TABLES}:
                        fk_relationships.add(f"- {table_name}.{from_col} = {ref_table}.{to_col}")
            except Exception as e:
                logger.debug(f"Failed to get FKs for {table_name}: {e}")

        if fk_relationships:
            lines.extend(sorted(fk_relationships))

        self._schema_cache = "\n".join(lines)
        return self._schema_cache

    def _validate_sql(self, sql: str) -> tuple[bool, str]:
        """
        Multi-layer SQL validation.

        Validates:
        1. Query starts with SELECT
        2. No dangerous keywords (INSERT, UPDATE, etc.)
        3. No blocked tables are referenced
        4. No blocked columns in SELECT clause

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

        # 4. Extract and validate table names against blocklist
        tables = self._extract_tables(sql)
        blocked_tables_lower = {t.lower() for t in BLOCKED_TABLES}
        for table in tables:
            if table.lower() in blocked_tables_lower:
                return False, f"Table not allowed: {table}"

        # 5. Extract and check for blocked columns in SELECT
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
