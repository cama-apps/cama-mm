"""
SQL Query Service for natural language to SQL translation.

Provides safe, validated SQL generation from natural language questions
using AI with strict whitelist enforcement.
"""

from __future__ import annotations

import asyncio
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
    # Sensitive social information
    "soft_avoids",
    "package_deals",
    # Internal player-trivia state: question_text embeds <@discord_id> mentions
    # and correct_index is a live answer key for in-progress sessions.
    "player_trivia_sessions",
    "player_trivia_questions",
    # Tables with no guild_id column: the per-guild TEMP VIEW isolation cannot
    # shadow them, so an AI query would read every guild's rows. Listed here for
    # documentation; the enforcement is structural — _validate_sql rejects any
    # table lacking a guild_id column unless it is in UNSCOPED_TABLE_ALLOWLIST.
    "player_steam_ids",
    "prediction_positions",
    "prediction_trades",
    "prediction_levels",
    "match_predictions",
    "match_corrections",
    "economy_ledger_context",
}

# Tables with no guild_id column that are deliberately queryable anyway.
# Every other table lacking guild_id is blocked structurally (fail closed) so a
# new global table is safe by default instead of relying on someone remembering
# to extend BLOCKED_TABLES above.
UNSCOPED_TABLE_ALLOWLIST: set[str] = set()

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
        self._blocked_tables_cache: set[str] | None = None

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
        if (
            guild_id is not None
            and self.guild_config_repo
            and not await asyncio.to_thread(self.guild_config_repo.get_ai_enabled, guild_id)
        ):
            return QueryResult(
                success=False,
                error="AI features are not enabled for this server. An admin can enable them.",
            )

        # 2. Build schema context for the AI
        schema_ctx = await asyncio.to_thread(self._build_schema_context)

        # 3. Look up asker's username for self-referential queries
        asker_username = None
        if asker_discord_id:
            try:
                normalized_guild = guild_id if guild_id is not None else 0
                row = await asyncio.to_thread(
                    self.ai_query_repo.execute_readonly,
                    "SELECT discord_username FROM players WHERE discord_id = ? AND guild_id = ?",
                    params=(asker_discord_id, normalized_guild),
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

        # 5. Execute the query, scoped to the asking guild at the data layer.
        # The model never sees guild_id (it is a blocked column), so isolation is
        # enforced here via per-guild views rather than by trusting the SQL.
        try:
            rows = await asyncio.to_thread(
                self.ai_query_repo.execute_readonly_guild_scoped, sql, guild_id, max_rows=25
            )
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

        # Get all tables from DB, filter out blocked ones (including tables
        # blocked structurally for lacking a guild_id column)
        try:
            all_tables = self.ai_query_repo.get_all_tables()
            blocked_lower = self._blocked_table_names()
        except Exception as e:
            logger.error(f"Failed to get tables: {e}")
            all_tables = []
            blocked_lower = {b.lower() for b in BLOCKED_TABLES}

        allowed_tables = [t for t in all_tables if t.lower() not in blocked_lower]

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
                    if ref_table.lower() not in blocked_lower:
                        fk_relationships.add(f"- {table_name}.{from_col} = {ref_table}.{to_col}")
            except Exception as e:
                logger.debug(f"Failed to get FKs for {table_name}: {e}")

        if fk_relationships:
            lines.extend(sorted(fk_relationships))

        self._schema_cache = "\n".join(lines)
        return self._schema_cache

    def _blocked_table_names(self) -> set[str]:
        """Lowercased names of every table AI queries must not touch.

        Combines BLOCKED_TABLES with schema ground truth: any base table that
        lacks a guild_id column cannot be shadowed by the per-guild TEMP views,
        so querying it would read every guild's rows. Such tables are blocked
        unless explicitly named in UNSCOPED_TABLE_ALLOWLIST (fail closed — the
        hand-written no-guild-id entries in BLOCKED_TABLES are documentation;
        this check is the enforcement). Cached: the schema is static at runtime.
        """
        if self._blocked_tables_cache is None:
            all_tables = {t.lower() for t in self.ai_query_repo.get_all_tables()}
            scoped = {t.lower() for t in self.ai_query_repo.get_guild_scoped_tables()}
            allowed = {t.lower() for t in UNSCOPED_TABLE_ALLOWLIST}
            blocked = {t.lower() for t in BLOCKED_TABLES}
            self._blocked_tables_cache = blocked | (all_tables - scoped - allowed)
        return self._blocked_tables_cache

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

        # Mask the *contents* of string literals before any structural check, so
        # the keyword/statement/table/column scans operate on SQL structure, not
        # on data that merely looks like SQL. This both prevents false rejections
        # (a name like 'a;b' or a value of 'DROP') and stops a literal from
        # hiding a real second statement.
        masked = self._mask_string_literals(sql)
        masked_upper = masked.upper().strip()

        # 0. Reject SQL comments outright. SQLite treats a comment as
        # whitespace, so `main/**/.players` or `FROM/**/soft_avoids` would slip
        # past the regex-based checks below, which do not model comment tokens.
        # Rejecting (rather than stripping) keeps the executed SQL identical to
        # what was validated. String-literal contents are masked above, so '--'
        # inside a quoted value does not trigger this.
        if "--" in masked or "/*" in masked:
            return False, "SQL comments are not allowed"

        # 1. Must start with SELECT
        if not masked_upper.startswith("SELECT"):
            return False, "Only SELECT queries are allowed"

        # 2. Check for dangerous keywords
        for keyword in DANGEROUS_KEYWORDS:
            # Use word boundary matching to avoid false positives
            if re.search(rf"\b{keyword}\b", masked_upper):
                return False, f"Forbidden keyword: {keyword}"

        # 3. Check for multiple statements (semicolon followed by more SQL).
        # Operates on the masked SQL so a semicolon inside a string literal does
        # not count as a statement separator. Allow a trailing semicolon.
        statements = [s.strip() for s in masked.split(";") if s.strip()]
        if len(statements) > 1:
            return False, "Multiple statements not allowed"

        # 4. Reject SELECT * — wildcard bypasses the column blocklist and
        # would expose PII columns like discord_id and steam_id.
        # Matches `SELECT *` and `SELECT table.*` (including DISTINCT/ALL).
        if re.search(r"\bSELECT\s+(?:DISTINCT\s+|ALL\s+)?(?:\w+\s*\.\s*)?\*", masked_upper):
            return False, "SELECT * is not allowed; list explicit columns"

        # 4b. Reject schema-qualified references (main./temp.). Guild isolation is
        # enforced by per-guild TEMP VIEWS that shadow the base tables; a query
        # that reaches past them via `main.players` would read every guild's rows.
        if re.search(r"\b(?:main|temp)\s*\.\s*\w", masked, re.IGNORECASE):
            return False, "Schema-qualified table references are not allowed"

        # 5. Extract and validate table names against the blocklist, which
        # includes (fail closed) every base table without a guild_id column
        # that is not explicitly allowlisted — see _blocked_table_names.
        try:
            blocked_tables_lower = self._blocked_table_names()
        except Exception as e:
            logger.error(f"Could not resolve blocked tables: {e}")
            return False, "Could not verify table guild scoping"
        tables = self._extract_tables(masked)
        for table in tables:
            if table.lower() in blocked_tables_lower:
                return False, f"Table not allowed: {table}"

        # 6. Extract and check for blocked columns in every SELECT projection
        # (top-level, UNION branches, and projected subqueries).
        blocked = self._check_blocked_columns(masked)
        if blocked:
            return False, f"Blocked column(s): {', '.join(blocked)}"

        return True, ""

    @staticmethod
    def _mask_string_literals(sql: str) -> str:
        """Normalise a query for structural validation.

        Two quote kinds are handled differently, matching SQLite semantics:

        * Single-quoted ``'...'`` is *string data* — its contents are masked to a
          neutral placeholder so a value like ``'a;b'`` or ``'DROP'`` can neither
          trigger a false rejection nor hide a second statement.
        * Double-quoted ``"..."``, bracketed ``[...]`` and backtick ``` `...` ```
          are *identifiers* (column/table names). SQLite resolves ``"discord_id"``
          to the real column, so these must stay visible to the column/table
          blocklist. The delimiters are replaced with spaces and the identifier
          text is kept, turning ``"discord_id"`` into a bare word the structural
          scans can see. (Previously their contents were masked like a literal,
          which let a quoted identifier smuggle a blocked column/table past every
          check while SQLite still read the real data.)

        Handles doubled-delimiter escaping for both kinds (e.g. ``'it''s'``).
        """
        out: list[str] = []
        i = 0
        n = len(sql)
        # opening delimiter -> closing delimiter for identifier quoting
        ident_close = {'"': '"', "`": "`", "[": "]"}
        while i < n:
            ch = sql[i]
            if ch == "'":
                out.append("'")
                i += 1
                while i < n:
                    if sql[i] == "'":
                        # Doubled quote = an escaped quote inside the literal.
                        if i + 1 < n and sql[i + 1] == "'":
                            out.append("xx")
                            i += 2
                            continue
                        out.append("'")
                        i += 1
                        break
                    out.append("x")
                    i += 1
            elif ch in ident_close:
                close = ident_close[ch]
                out.append(" ")
                i += 1
                while i < n:
                    if sql[i] == close:
                        # Doubled delimiter = an escaped delimiter in the name
                        # (only for "" and ``, not []). Keep it as a plain char.
                        if close != "]" and i + 1 < n and sql[i + 1] == close:
                            out.append(sql[i])
                            i += 2
                            continue
                        out.append(" ")
                        i += 1
                        break
                    out.append(sql[i])
                    i += 1
            else:
                out.append(ch)
                i += 1
        return "".join(out)

    @staticmethod
    def _projection_lists(masked_lower: str) -> list[str]:
        """Return the output-column text of every SELECT, scoped to that SELECT's
        own nesting level.

        For each ``SELECT`` the projection runs to the ``FROM`` that closes it at
        the same parenthesis depth (or to end / an enclosing ``)`` for a
        FROM-less SELECT). Text inside nested subqueries is skipped, so a blocked
        column is detected when it is actually projected — at top level, in a
        UNION branch, or as a projected subquery's own output — but not when it
        appears only inside a subquery's WHERE/JOIN.
        """
        n = len(masked_lower)
        projections: list[str] = []
        for m in re.finditer(r"\bselect\b", masked_lower):
            depth = 0
            buf: list[str] = []
            j = m.end()
            while j < n:
                c = masked_lower[j]
                if c == "(":
                    depth += 1
                    j += 1
                    continue
                if c == ")":
                    if depth == 0:
                        break  # closing paren of an enclosing subquery
                    depth -= 1
                    j += 1
                    continue
                if (
                    depth == 0
                    and masked_lower.startswith("from", j)
                    and (j == 0 or not (masked_lower[j - 1].isalnum() or masked_lower[j - 1] == "_"))
                    and (j + 4 >= n or not (masked_lower[j + 4].isalnum() or masked_lower[j + 4] == "_"))
                ):
                    break  # FROM closing this SELECT's column list
                if depth == 0:
                    buf.append(c)
                j += 1
            projections.append("".join(buf))
        return projections

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

    def _check_blocked_columns(self, masked_sql: str) -> list[str]:
        """
        Return blocked columns that appear in any SELECT projection.

        ``masked_sql`` must already have string-literal contents masked. Every
        projection is scanned — top-level, UNION/INTERSECT/EXCEPT branches, and
        projected subqueries — so the blocklist cannot be bypassed with a UNION
        or a subquery placed after the first FROM. Blocked columns used only in a
        JOIN/WHERE (i.e. not projected) are still allowed.
        """
        found_blocked: set[str] = set()
        for proj in self._projection_lists(masked_sql.lower()):
            for col in BLOCKED_COLUMNS:
                col_l = col.lower()
                patterns = [
                    rf"\b{re.escape(col_l)}\b",  # Simple reference
                    rf"\.\s*{re.escape(col_l)}\b",  # table.column
                ]
                if any(re.search(p, proj) for p in patterns):
                    found_blocked.add(col)

        return sorted(found_blocked)
