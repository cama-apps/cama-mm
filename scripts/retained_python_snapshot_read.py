#!/usr/bin/env python3
"""Read Rust snapshot sentinels through retained production Python APIs.

This helper intentionally does not open SQLite itself.  It is run only against
the disposable copy created by ``production_snapshot_replay.py`` and emits a
normalized JSON contract that omits timestamps and auto-incremented IDs.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

# Running a script by path places ``scripts/`` on sys.path, not the project
# root where the retained production packages live.
PROJECT_ROOT = str(Path(__file__).resolve().parents[1])
if PROJECT_ROOT not in sys.path:
    sys.path.insert(0, PROJECT_ROOT)

from repositories.dig_repository import DigRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.survey_repository import SurveyRepository

DIG_PLAYER_ID = -8_888_888_888_888_888_870
DIG_GUILD_ID = -8_888_888_888_888_888_888
SURVEY_GUILD_ID = 9_000_000_000_000_001
SURVEY_USER_ID = 9_000_000_000_000_002
SURVEY_TITLE = "Rust disposable Survey recovery smoke"
SURVEY_PROMPT = "Can the Rust recovery worker deliver this clone-only sentinel?"
DIG_ITEM_TYPE = "hard_hat"
DIG_ROUTE_STATE = '{"route_id":"shored_passage","status":"active"}'
SURVEY_CHANNEL_ID = 8_000_000_000_000_001
SURVEY_MESSAGE_ID = 8_000_000_000_000_002


def _required_mapping(row: dict[str, Any], key: str, context: str) -> Any:
    if key not in row:
        raise RuntimeError(
            f"Python repository API blocker: {context} does not expose {key!r}"
        )
    return row[key]


def _required_attribute(value: object, key: str, context: str) -> Any:
    if not hasattr(value, key):
        raise RuntimeError(
            f"Python repository API blocker: {context} does not expose {key!r}"
        )
    return getattr(value, key)


def _required_method(value: object, key: str, context: str):
    method = getattr(value, key, None)
    if not callable(method):
        raise RuntimeError(
            f"Python repository API blocker: {context} does not expose {key}()"
        )
    return method


def _enum_value(value: object) -> object:
    return getattr(value, "value", value)


def _canonical_json(value: object, context: str) -> str:
    if not isinstance(value, str):
        raise RuntimeError(
            f"Python repository API blocker: {context} route_state is not text"
        )
    try:
        decoded = json.loads(value)
    except (TypeError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"Python retained read mismatch: {context} route_state is invalid JSON"
        ) from error
    return json.dumps(decoded, sort_keys=True, separators=(",", ":"))


def read_snapshot(db_path: str) -> dict[str, Any]:
    """Read all Rust sentinels through the retained repository/domain APIs."""

    guild_repository = GuildConfigRepository(db_path)
    guild_config = _required_method(
        guild_repository, "get_config", "GuildConfigRepository"
    )(DIG_GUILD_ID)
    if guild_config is None:
        raise RuntimeError(
            "Python retained read mismatch: guild-config sentinel is missing"
        )
    guild_context = "GuildConfigRepository.get_config"

    dig_repository = DigRepository(db_path)
    inventory = _required_method(
        dig_repository, "get_inventory", "DigRepository"
    )(DIG_PLAYER_ID, DIG_GUILD_ID)
    if len(inventory) != 1:
        raise RuntimeError(
            "Python retained read mismatch: expected one Dig inventory sentinel, "
            f"found {len(inventory)}"
        )
    item = inventory[0]
    item_context = "DigRepository.get_inventory sentinel row"
    item_discord_id = _required_mapping(item, "discord_id", item_context)
    item_guild_id = _required_mapping(item, "guild_id", item_context)
    item_type = _required_mapping(item, "item_type", item_context)
    queued = _required_mapping(item, "queued", item_context)
    if (item_discord_id, item_guild_id, item_type, bool(queued)) != (
        DIG_PLAYER_ID,
        DIG_GUILD_ID,
        DIG_ITEM_TYPE,
        True,
    ):
        raise RuntimeError(
            f"Python retained read mismatch: Dig inventory sentinel is {item!r}"
        )

    tunnel = _required_method(
        dig_repository, "get_tunnel", "DigRepository"
    )(DIG_PLAYER_ID, DIG_GUILD_ID)
    if tunnel is None:
        raise RuntimeError(
            "Python retained read mismatch: Dig route tunnel sentinel is missing"
        )
    tunnel_context = "DigRepository.get_tunnel sentinel row"
    tunnel_discord_id = _required_mapping(tunnel, "discord_id", tunnel_context)
    tunnel_guild_id = _required_mapping(tunnel, "guild_id", tunnel_context)
    tunnel_depth = _required_mapping(tunnel, "depth", tunnel_context)
    normalized_route_state = _canonical_json(
        _required_mapping(tunnel, "route_state", tunnel_context), tunnel_context
    )
    if (tunnel_discord_id, tunnel_guild_id, tunnel_depth, normalized_route_state) != (
        DIG_PLAYER_ID,
        DIG_GUILD_ID,
        100,
        DIG_ROUTE_STATE,
    ):
        raise RuntimeError(
            f"Python retained read mismatch: Dig route sentinel is {tunnel!r}"
        )

    survey_repository = SurveyRepository(db_path)
    surveys = [
        survey
        for survey in _required_method(
            survey_repository, "list_surveys", "SurveyRepository"
        )(SURVEY_GUILD_ID)
        if _required_attribute(survey, "title", "SurveyRepository.list_surveys")
        == SURVEY_TITLE
    ]
    if len(surveys) != 1:
        raise RuntimeError(
            "Python retained read mismatch: expected one Survey sentinel, "
            f"found {len(surveys)}"
        )
    survey = surveys[0]
    survey_context = "SurveyRepository.list_surveys sentinel"
    survey_id = _required_attribute(survey, "survey_id", survey_context)
    survey_guild_id = _required_attribute(survey, "guild_id", survey_context)
    survey_title = _required_attribute(survey, "title", survey_context)
    survey_status = _enum_value(_required_attribute(survey, "status", survey_context))
    survey_target_type = _enum_value(
        _required_attribute(survey, "target_type", survey_context)
    )
    survey_registered_only = _required_attribute(
        survey, "registered_only", survey_context
    )
    if (
        survey_guild_id,
        survey_title,
        survey_status,
        survey_target_type,
        bool(survey_registered_only),
    ) != (SURVEY_GUILD_ID, SURVEY_TITLE, "open", "member", False):
        raise RuntimeError(f"Python retained read mismatch: Survey sentinel is {survey!r}")

    questions = _required_method(
        survey_repository, "get_questions", "SurveyRepository"
    )(SURVEY_GUILD_ID, survey_id)
    if len(questions) != 1:
        raise RuntimeError(
            "Python retained read mismatch: expected one Survey question, "
            f"found {len(questions)}"
        )
    question = questions[0]
    question_context = "SurveyRepository.get_questions sentinel"
    question_id = _required_attribute(question, "question_id", question_context)
    question_fields = (
        _required_attribute(question, "survey_id", question_context),
        _required_attribute(question, "position", question_context),
        _required_attribute(question, "prompt", question_context),
        _enum_value(_required_attribute(question, "question_type", question_context)),
        bool(_required_attribute(question, "required", question_context)),
    )
    if question_fields != (survey_id, 1, SURVEY_PROMPT, "nps", True):
        raise RuntimeError(
            f"Python retained read mismatch: Survey question is {question!r}"
        )

    session = _required_method(
        survey_repository, "get_response_session", "SurveyRepository"
    )(survey_id, SURVEY_USER_ID)
    if session is None:
        raise RuntimeError(
            "Python retained read mismatch: Survey response session sentinel is missing"
        )
    recipient = _required_attribute(
        session, "recipient", "SurveyRepository.get_response_session"
    )
    recipient_context = "SurveyRepository.get_response_session recipient"
    recipient_fields = (
        _required_attribute(recipient, "survey_id", recipient_context),
        _required_attribute(recipient, "guild_id", recipient_context),
        _required_attribute(recipient, "discord_id", recipient_context),
        _enum_value(_required_attribute(recipient, "delivery_status", recipient_context)),
        _required_attribute(recipient, "attempt_count", recipient_context),
        _required_attribute(recipient, "dm_channel_id", recipient_context),
        _required_attribute(recipient, "dm_message_id", recipient_context),
        _required_attribute(recipient, "ui_channel_id", recipient_context),
        _required_attribute(recipient, "ui_message_id", recipient_context),
        _required_attribute(recipient, "current_question_id", recipient_context),
        bool(_required_attribute(recipient, "in_review", recipient_context)),
        _required_attribute(recipient, "submitted_at", recipient_context),
    )
    if recipient_fields != (
        survey_id,
        SURVEY_GUILD_ID,
        SURVEY_USER_ID,
        "sent",
        1,
        SURVEY_CHANNEL_ID,
        SURVEY_MESSAGE_ID,
        SURVEY_CHANNEL_ID,
        SURVEY_MESSAGE_ID,
        question_id,
        False,
        None,
    ):
        raise RuntimeError(
            f"Python retained read mismatch: Survey recipient is {recipient!r}"
        )
    answers = _required_attribute(
        session, "answers", "SurveyRepository.get_response_session"
    )
    if len(answers) != 0:
        raise RuntimeError(
            f"Python retained read mismatch: Survey sentinel has {len(answers)} answers"
        )

    return {
        "guild_config": {
            "guild_id": _required_mapping(guild_config, "guild_id", guild_context),
            "league_id": _required_mapping(guild_config, "league_id", guild_context),
            "auto_enrich_matches": int(
                _required_mapping(guild_config, "auto_enrich_matches", guild_context)
            ),
            "ai_features_enabled": int(
                _required_mapping(guild_config, "ai_features_enabled", guild_context)
            ),
        },
        "dig": {
            "inventory": {
                "row_count": len(inventory),
                "discord_id": item_discord_id,
                "guild_id": item_guild_id,
                "item_type": item_type,
                "queued": bool(queued),
            },
            "tunnel": {
                "discord_id": tunnel_discord_id,
                "guild_id": tunnel_guild_id,
                "depth": tunnel_depth,
                "route_state": normalized_route_state,
            },
        },
        "survey": {
            "survey": {
                "guild_id": survey_guild_id,
                "title": survey_title,
                "status": survey_status,
                "target_type": survey_target_type,
                "registered_only": bool(survey_registered_only),
            },
            "question": {
                "position": _required_attribute(question, "position", question_context),
                "prompt": _required_attribute(question, "prompt", question_context),
                "question_type": _enum_value(
                    _required_attribute(question, "question_type", question_context)
                ),
                "required": bool(
                    _required_attribute(question, "required", question_context)
                ),
            },
            "recipient": {
                "guild_id": _required_attribute(recipient, "guild_id", recipient_context),
                "discord_id": _required_attribute(recipient, "discord_id", recipient_context),
                "delivery_status": _enum_value(
                    _required_attribute(recipient, "delivery_status", recipient_context)
                ),
                "attempt_count": _required_attribute(recipient, "attempt_count", recipient_context),
                "dm_channel_id": _required_attribute(recipient, "dm_channel_id", recipient_context),
                "dm_message_id": _required_attribute(recipient, "dm_message_id", recipient_context),
                "ui_channel_id": _required_attribute(recipient, "ui_channel_id", recipient_context),
                "ui_message_id": _required_attribute(recipient, "ui_message_id", recipient_context),
                "current_question_is_first": recipient.current_question_id == question_id,
                "in_review": bool(recipient.in_review),
                "submitted": recipient.submitted_at is not None,
                "answer_count": len(answers),
            },
        },
    }


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <disposable-db-path>", file=sys.stderr)
        return 2
    print(json.dumps(read_snapshot(sys.argv[1]), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
