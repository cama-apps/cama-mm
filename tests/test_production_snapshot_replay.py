"""Focused contracts for optional retained-Python snapshot diagnostics."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from scripts import production_snapshot_replay as replay
from scripts import retained_python_snapshot_read as reader


class _EnumValue:
    def __init__(self, value: str):
        self.value = value


def _fake_repositories(monkeypatch):
    question_id = 702
    survey_id = 701
    recipient = SimpleNamespace(
        survey_id=survey_id,
        guild_id=reader.SURVEY_GUILD_ID,
        discord_id=reader.SURVEY_USER_ID,
        delivery_status=_EnumValue("sent"),
        attempt_count=1,
        dm_channel_id=reader.SURVEY_CHANNEL_ID,
        dm_message_id=reader.SURVEY_MESSAGE_ID,
        ui_channel_id=reader.SURVEY_CHANNEL_ID,
        ui_message_id=reader.SURVEY_MESSAGE_ID,
        current_question_id=question_id,
        in_review=False,
        submitted_at=None,
    )
    survey = SimpleNamespace(
        survey_id=survey_id,
        guild_id=reader.SURVEY_GUILD_ID,
        title=reader.SURVEY_TITLE,
        status=_EnumValue("open"),
        target_type=_EnumValue("member"),
        registered_only=False,
    )
    question = SimpleNamespace(
        question_id=question_id,
        survey_id=survey_id,
        position=1,
        prompt=reader.SURVEY_PROMPT,
        question_type=_EnumValue("nps"),
        required=True,
    )

    class FakeGuildRepository:
        def __init__(self, _db_path):
            pass

        def get_config(self, _guild_id):
            return {
                "guild_id": reader.DIG_GUILD_ID,
                "league_id": 777,
                "auto_enrich_matches": 0,
                "ai_features_enabled": 1,
            }

    class FakeDigRepository:
        def __init__(self, _db_path):
            pass

        def get_inventory(self, _discord_id, _guild_id):
            return [
                {
                    "discord_id": reader.DIG_PLAYER_ID,
                    "guild_id": reader.DIG_GUILD_ID,
                    "item_type": reader.DIG_ITEM_TYPE,
                    "queued": 1,
                }
            ]

        def get_tunnel(self, _discord_id, _guild_id):
            return {
                "discord_id": reader.DIG_PLAYER_ID,
                "guild_id": reader.DIG_GUILD_ID,
                "depth": 100,
                # Deliberately reverse the authored key order; the retained
                # read must normalize the JSON route contract.
                "route_state": '{"status":"active","route_id":"shored_passage"}',
            }

    class FakeSurveyRepository:
        def __init__(self, _db_path):
            pass

        def list_surveys(self, _guild_id):
            return [survey]

        def get_questions(self, _guild_id, _survey_id):
            return [question]

        def get_response_session(self, _survey_id, _discord_id):
            return SimpleNamespace(recipient=recipient, answers=())

    monkeypatch.setattr(reader, "GuildConfigRepository", FakeGuildRepository)
    monkeypatch.setattr(reader, "DigRepository", FakeDigRepository)
    monkeypatch.setattr(reader, "SurveyRepository", FakeSurveyRepository)


def test_retained_read_normalizes_dig_and_survey_api_values(monkeypatch):
    _fake_repositories(monkeypatch)

    actual = reader.read_snapshot("disposable-copy.db")

    assert actual == replay.expected_retained_python_read()


def test_retained_read_reports_missing_repository_field(monkeypatch):
    _fake_repositories(monkeypatch)

    class MissingRouteState(reader.DigRepository):
        def get_tunnel(self, _discord_id, _guild_id):
            return {
                "discord_id": reader.DIG_PLAYER_ID,
                "guild_id": reader.DIG_GUILD_ID,
                "depth": 100,
            }

    monkeypatch.setattr(reader, "DigRepository", MissingRouteState)

    with pytest.raises(
        RuntimeError,
        match=r"Python repository API blocker: DigRepository\.get_tunnel sentinel row does not expose 'route_state'",
    ):
        reader.read_snapshot("disposable-copy.db")


def test_retained_reader_has_no_raw_sql_fallback():
    source = Path(reader.__file__).read_text(encoding="utf-8")

    assert "sqlite3" not in source
    assert "SELECT " not in source
    assert "DigRepository" in source
    assert "SurveyRepository" in source


def test_snapshot_parser_accepts_only_the_normalized_contract():
    expected = replay.expected_retained_python_read()

    assert replay._parse_retained_python_read(json.dumps(expected)) == expected

    replay_source = Path(replay.__file__).read_text(encoding="utf-8")
    assert "python_post_rust_retained_repository_read" not in replay_source
    assert "retained_read_script" not in replay_source
    assert "referral_snapshot_smoke" in replay_source
    assert replay.EXPECTED_REFERRAL_SMOKE_DELTAS == {
        "economy_ledger_entries": 8,
        "match_participants": 2,
        "match_predictions": 1,
        "matches": 1,
        "player_pairings": 1,
        "players": 6,
        "referrals": 1,
    }

    malformed = json.loads(json.dumps(expected))
    malformed["dig"]["tunnel"]["route_state"] = "{}"
    with pytest.raises(RuntimeError, match="retained Python repository read mismatch"):
        replay._parse_retained_python_read(json.dumps(malformed))


def test_referral_subprocess_failure_reports_attempt_and_partial_deltas(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "production.db"
    source.touch()
    phase = {"value": "before"}
    source_facts = {
        "sha256": "source",
        "size_bytes": 1,
        "migration_count": 1,
        "quick_check": "ok",
        "integrity_check": ["ok"],
        "integrity_ok": True,
    }

    def fake_inspect(path: Path) -> dict[str, object]:
        if path == source:
            return dict(source_facts)
        return {**source_facts, "sha256": f"copy-{phase['value']}", "phase": phase["value"]}

    repository_counts = dict(replay.EXPECTED_SMOKE_DELTAS)
    partial_referral_counts = dict(repository_counts)
    partial_referral_counts["matches"] = 1
    count_results = iter([{}, repository_counts, repository_counts, partial_referral_counts])

    def fake_run_step(
        name: str,
        _command: object,
        *,
        cwd: Path,
        timeout_seconds: float,
        report: dict[str, object],
    ) -> dict[str, object]:
        del cwd, timeout_seconds
        if name == "rust_db_check":
            return {"stdout": "schema_compatible=true"}
        if name == "rust_repository_smoke":
            return {"stdout": "shared_repository_roundtrip=ok"}
        if name == "rust_referral_settlement_smoke":
            phase["value"] = "referral_failed"
            step = {
                "name": name,
                "status": "failed",
                "returncode": 1,
                "stdout": "",
                "stderr": "injected failure after a partial write",
            }
            report["steps"].append(step)  # type: ignore[union-attr]
            raise replay.StepFailure(step)
        return {"stdout": ""}

    monkeypatch.setattr(replay, "inspect_database", fake_inspect)
    monkeypatch.setattr(replay, "table_row_counts", lambda _path: next(count_results))
    monkeypatch.setattr(
        replay,
        "sentinel_digests",
        lambda _path: {
            table: {"row_count": 1, "sha256": table} for table in replay.EXPECTED_SMOKE_DELTAS
        },
    )
    monkeypatch.setattr(replay, "run_step", fake_run_step)
    monkeypatch.setattr(replay, "_python_executable", lambda _argument, _root: "python")

    status, report = replay.run(
        SimpleNamespace(
            source=source,
            report=None,
            python=None,
            cargo="cargo",
            timeout_seconds=1,
        )
    )

    assert status == 1
    assert report["status"] == "failed"
    assert report["referral_settlement"] == {
        "status": "failed",
        "attempted": True,
        "settlement_marker": False,
        "expected_table_deltas": replay.EXPECTED_REFERRAL_SMOKE_DELTAS,
        "table_deltas": {
            "matches": {"before": 0, "after": 1, "delta": 1},
        },
    }
    assert report["disposable_copy"]["after_rust"]["phase"] == "referral_failed"
    assert report["source"]["unchanged"] is True
