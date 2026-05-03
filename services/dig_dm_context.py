"""Pre-fetcher for the dig DM's pre-loaded context bundle.

Single-shot fetch of the data the flavor LLM call needs beyond what's already
serialized by ``services.dig_llm_prompts.build_*`` helpers — narrative memory
blob, NPC roster, last inhouse match, active loans, recent server activity.

Kept light by design: the user budget is ``Light: last 1 match, current
loans, last 24h server activity``. Each section collapses to a one-liner so
prompt growth is bounded.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import TYPE_CHECKING

from services.dig_npcs import roster_lines

if TYPE_CHECKING:
    from repositories.dig_repository import DigRepository
    from repositories.loan_repository import LoanRepository
    from repositories.match_repository import MatchRepository


@dataclass(frozen=True)
class DigDMContext:
    """Pre-loaded context the flavor LLM call sees as additional sections."""

    memory_blob: str
    last_match_summary: str
    economy_state: str
    server_activity: str
    npc_roster: list[str]


class DigDMContextBuilder:
    """Read-only fetcher for the dig flavor pre-loaded context."""

    def __init__(
        self,
        dig_repo: DigRepository,
        match_repo: MatchRepository,
        loan_repo: LoanRepository,
    ) -> None:
        self.dig_repo = dig_repo
        self.match_repo = match_repo
        self.loan_repo = loan_repo

    async def build(self, discord_id: int, guild_id: int) -> DigDMContext:
        memory_blob, last_match, loan_state, recent_diggers, recent_bosses = (
            await asyncio.gather(
                asyncio.to_thread(self.dig_repo.get_dm_memory, discord_id, guild_id),
                asyncio.to_thread(
                    self.match_repo.get_player_matches, discord_id, guild_id, 1
                ),
                asyncio.to_thread(self.loan_repo.get_state, discord_id, guild_id),
                asyncio.to_thread(
                    self.dig_repo.get_recent_diggers,
                    guild_id,
                    days=1,
                    exclude_id=discord_id,
                ),
                asyncio.to_thread(
                    self.dig_repo.count_recent_boss_kills, guild_id, 24
                ),
            )
        )

        return DigDMContext(
            memory_blob=memory_blob or "",
            last_match_summary=_summarize_last_match(last_match),
            economy_state=_summarize_loan_state(loan_state),
            server_activity=_summarize_server_activity(
                recent_diggers, recent_bosses
            ),
            npc_roster=roster_lines(),
        )


def _summarize_last_match(matches: list[dict] | None) -> str:
    if not matches:
        return "No recent inhouse matches."
    m = matches[0]
    won = bool(m.get("won"))
    side = m.get("side") or m.get("team_side")
    duration = m.get("duration_seconds") or m.get("duration") or 0
    mins = duration // 60 if isinstance(duration, int) else 0
    outcome = "won" if won else "lost"
    parts = [f"Most recent inhouse: {outcome}"]
    if mins:
        parts.append(f"{mins}m game")
    if side and not won:
        # only worth quoting when narratively useful
        parts.append(f"on {side}")
    return ", ".join(parts) + "."


def _summarize_loan_state(state: dict | None) -> str:
    if not state:
        return "No outstanding loan."
    principal = int(state.get("outstanding_principal") or 0)
    fee = int(state.get("outstanding_fee") or 0)
    total = principal + fee
    if total <= 0:
        return "No outstanding loan."
    return f"Outstanding loan: owes the bank a debt of {total}."


def _summarize_server_activity(recent_diggers: list[int], recent_boss_kills: int) -> str:
    digger_n = len(recent_diggers or [])
    if digger_n == 0 and recent_boss_kills == 0:
        return "Server is quiet — no other miners and no boss kills in the last day."
    parts: list[str] = []
    if digger_n == 1:
        parts.append("1 other miner has dug in the last day")
    elif digger_n > 1:
        parts.append(f"{digger_n} other miners have dug in the last day")
    if recent_boss_kills == 1:
        parts.append("1 boss has fallen recently")
    elif recent_boss_kills > 1:
        parts.append(f"{recent_boss_kills} bosses have fallen recently")
    return "; ".join(parts) + "."


def render_dm_context_section(ctx: DigDMContext) -> str:
    """Serialize the pre-loaded context into a prompt section block."""
    lines: list[str] = []
    if ctx.memory_blob.strip():
        lines.append(f"NARRATIVE MEMORY (your previous notes on this player):\n{ctx.memory_blob.strip()}")
    else:
        lines.append("NARRATIVE MEMORY: empty (no prior notes on this player).")
    lines.append(f"LAST INHOUSE MATCH: {ctx.last_match_summary}")
    lines.append(f"ECONOMY: {ctx.economy_state}")
    lines.append(f"SERVER ACTIVITY: {ctx.server_activity}")
    lines.append("NPC ROSTER (canon — pick from these or invent if none fit):")
    lines.extend(ctx.npc_roster)
    return "\n".join(lines)
