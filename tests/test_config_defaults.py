"""Tests for environment-independent configuration defaults."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path
from types import ModuleType
from unittest.mock import patch


def test_dota_betting_window_defaults_to_twenty_minutes():
    dotenv_stub = ModuleType("dotenv")

    def load_dotenv() -> None:
        return None

    dotenv_stub.load_dotenv = load_dotenv
    config_path = Path(__file__).parents[1] / "config.py"
    spec = importlib.util.spec_from_file_location("config_without_dotenv", config_path)
    assert spec is not None
    assert spec.loader is not None
    config_module = importlib.util.module_from_spec(spec)

    with (
        patch.dict(os.environ, {}, clear=False),
        patch.dict(sys.modules, {"dotenv": dotenv_stub}),
    ):
        os.environ.pop("BET_LOCK_SECONDS", None)
        spec.loader.exec_module(config_module)

    assert config_module.BET_LOCK_SECONDS == 20 * 60
