"""Contracts for the bounded Python/Rust snapshot A/B evidence runner."""

from __future__ import annotations

import json
from pathlib import Path

from scripts import production_snapshot_ab_delta as ab


def test_route_state_projection_is_json_order_independent() -> None:
    rows = [
        {
            "discord_id": ab.DIG_PLAYER_ID,
            "guild_id": ab.SMOKE_GUILD_ID,
            "depth": 100,
            "route_state": '{"status":"active","route_id":"shored_passage"}',
        }
    ]

    assert ab._project_rows("tunnels", rows) == ab.expected_scope_after()["tunnels"]["rows"]


def test_scope_delta_reports_schema_aware_rows_and_counts() -> None:
    empty = {
        table: {"row_count": 0, "rows": []} for table in ab.SCOPE_COLUMNS
    }
    after = ab.expected_scope_after()

    delta = ab.scope_delta(empty, after)

    assert all(details["before_count"] == 0 for details in delta.values())
    assert delta["guild_config"]["delta"] == 1
    assert delta["survey_answers"]["delta"] == 0
    assert delta["survey_recipients"]["after_rows"][0]["current_question_id"] == (
        "<question>"
    )


def test_expected_contract_is_stable_json() -> None:
    encoded = json.dumps(ab.expected_scope_after(), sort_keys=True)

    assert "created_at" not in encoded
    assert "updated_at" not in encoded
    assert "route_id" in encoded
    assert "delivery_status" in encoded


def test_python_write_helper_uses_retained_repositories_for_transitions() -> None:
    source = Path(ab.PROJECT_ROOT, "scripts", "python_snapshot_ab_write.py").read_text(
        encoding="utf-8"
    )

    assert "GuildConfigRepository" in source
    assert "DigRepository" in source
    assert "SurveyRepository" in source
    assert "set_league_id" in source
    assert "atomic_auto_buy_items" in source
    assert "mark_delivery_sent" in source
