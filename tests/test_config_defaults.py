"""Tests for environment-independent configuration defaults."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path
from types import ModuleType
from unittest.mock import patch


def _load_config_without_env(*env_vars: str):
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
        for env_var in env_vars:
            os.environ.pop(env_var, None)
        spec.loader.exec_module(config_module)

    return config_module


def test_dota_betting_window_defaults_to_twenty_minutes():
    config_module = _load_config_without_env("BET_LOCK_SECONDS")

    assert config_module.BET_LOCK_SECONDS == 20 * 60


def test_openskill_shuffle_chance_defaults_to_two_percent():
    config_module = _load_config_without_env("OPENSKILL_SHUFFLE_CHANCE")

    assert config_module.OPENSKILL_SHUFFLE_CHANCE == 0.02
