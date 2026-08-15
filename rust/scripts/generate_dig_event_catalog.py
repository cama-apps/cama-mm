#!/usr/bin/env python3
"""Generate/check the Rust Dig event snapshot from the Python source catalog.

The Rust binary never imports Python.  This script is the development/parity
gate that makes regeneration explicit whenever
``services/dig_data/event_definitions.py`` changes.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, is_dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

SOURCE = ROOT / "services" / "dig_data" / "event_definitions.py"
QUEST_SOURCE = ROOT / "services" / "dig_data" / "quests.py"
TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_event_catalog.json"
QUEST_TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_quest_catalog.json"


def _normalize(value: object) -> object:
    if is_dataclass(value):
        return {key: _normalize(item) for key, item in asdict(value).items()}
    if isinstance(value, tuple):
        return [_normalize(item) for item in value]
    if isinstance(value, list):
        return [_normalize(item) for item in value]
    if isinstance(value, dict):
        return {str(key): _normalize(item) for key, item in value.items()}
    return value


def render_catalog() -> bytes:
    # Import only the authored data module.  Do not import DigService or any
    # Discord/SQLite runtime module from this generator.
    from services.dig_data.event_art import EVENT_ASCII_ART
    from services.dig_data.event_definitions import RANDOM_EVENTS

    events = []
    for event in RANDOM_EVENTS:
        normalized = _normalize(event)
        assert isinstance(normalized, dict)
        # Python's live EVENT_POOL overlays the separately authored art table
        # whenever the dataclass itself has no inline scene.  The Rust
        # snapshot must capture that application-facing projection too.
        normalized["ascii_art"] = normalized.get("ascii_art") or EVENT_ASCII_ART.get(event.id)
        events.append(normalized)
    lines = [
        json.dumps(event, ensure_ascii=False, separators=(",", ":"))
        + ("," if index + 1 < len(events) else "")
        for index, event in enumerate(events)
    ]
    return ("[\n" + "\n".join(lines) + "\n]\n").encode("utf-8")


def render_quest_catalog() -> bytes:
    # Quest definitions are authored separately from event definitions, but
    # use the same immutable dataclass normalization and generated-artifact
    # policy.  Import only the data module so production remains Python-free.
    from services.dig_data.quests import QUESTS

    quests = [_normalize(quest) for quest in QUESTS]
    lines = [
        json.dumps(quest, ensure_ascii=False, separators=(",", ":"))
        + ("," if index + 1 < len(quests) else "")
        for index, quest in enumerate(quests)
    ]
    return ("[\n" + "\n".join(lines) + "\n]\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail when the snapshot is stale")
    mode.add_argument("--write", action="store_true", help="write the regenerated snapshot")
    parser.add_argument(
        "--quests",
        action="store_true",
        help="operate on the generated quest/finale snapshot instead of events",
    )
    args = parser.parse_args(argv)

    rendered = render_quest_catalog() if args.quests else render_catalog()
    source = QUEST_SOURCE if args.quests else SOURCE
    target = QUEST_TARGET if args.quests else TARGET
    if args.write:
        target.write_bytes(rendered)
        kind = "quests" if args.quests else "events"
        print(f"wrote {len(json.loads(rendered))} {kind} to {target}")
        return 0
    if args.check:
        actual = target.read_bytes()
        if actual != rendered:
            print(
                f"stale Dig snapshot: regenerate {target} from {source}",
                file=sys.stderr,
            )
            return 1
        kind = "quest" if args.quests else "event"
        print(f"Dig {kind} snapshot is current ({len(json.loads(rendered))} entries)")
        return 0
    sys.stdout.buffer.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
