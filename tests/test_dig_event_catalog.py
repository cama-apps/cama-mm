"""Drift gate for the generated Rust Dig event snapshot."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / "rust" / "scripts" / "generate_dig_event_catalog.py"
TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_event_catalog.json"
QUEST_TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_quest_catalog.json"


def _generator_module():
    spec = importlib.util.spec_from_file_location("generate_dig_event_catalog", GENERATOR)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_rust_dig_event_snapshot_is_regenerated_from_python_catalog():
    generator = _generator_module()
    rendered = generator.render_catalog()
    assert TARGET.read_bytes() == rendered
    payload = json.loads(rendered)
    assert len(payload) == 203
    assert payload[0]["id"] == "underground_stream"
    assert payload[-1]["id"] == "first_descent_cartography"
    assert sum(bool(event["ascii_art"]) for event in payload) == 71
    assert "~~~~~~~~~~~~" in payload[0]["ascii_art"]


def test_rust_dig_quest_snapshot_is_regenerated_from_python_catalog():
    generator = _generator_module()
    rendered = generator.render_quest_catalog()
    assert QUEST_TARGET.read_bytes() == rendered
    payload = json.loads(rendered)
    assert len(payload) == 3
    assert [quest["quest_id"] for quest in payload] == [
        "agh_lost_trial",
        "necropolis_below",
        "bolas_hidden_vault",
    ]
