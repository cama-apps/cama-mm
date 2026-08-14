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

    malformed = json.loads(json.dumps(expected))
    malformed["dig"]["tunnel"]["route_state"] = "{}"
    with pytest.raises(RuntimeError, match="retained Python repository read mismatch"):
        replay._parse_retained_python_read(json.dumps(malformed))
