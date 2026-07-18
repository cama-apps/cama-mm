"""
Tests for bot initialization and basic setup.
These tests verify that the bot can be imported and configured without connecting to Discord.
"""

import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest


def test_bot_import():
    """Test that bot module can be imported (without running it)."""
    import bot

    # Verify bot object exists
    assert hasattr(bot, "bot")
    # Services are lazily initialized via ServiceContainer, not module-level globals
    assert hasattr(bot, "_init_services")


def test_bot_commands_registered(tmp_path):
    """Load every extension and inspect representative commands in an isolated process."""
    repo_root = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env["DB_PATH"] = str(tmp_path / "bot-smoke.db")
    script = textwrap.dedent(
        """
        import asyncio

        async def main():
            import bot

            try:
                await bot._load_extensions()
                missing_extensions = [
                    extension
                    for extension in bot.EXTENSIONS
                    if extension not in bot.bot.extensions
                ]
                assert not missing_extensions, (
                    f"Extensions failed to load: {missing_extensions}"
                )

                command_names = {
                    command.name for command in bot.bot.tree.get_commands()
                }
                expected_commands = (
                    "player",
                    "draft",
                    "predict",
                    "duel",
                    "admin",
                    "enrich",
                    "dota",
                    "lobby",
                    "shuffle",
                    "record",
                    "profile",
                    "leaderboard",
                    "help",
                )
                for command_name in expected_commands:
                    assert command_name in command_names, (
                        f"Command {command_name!r} not found in registered commands"
                    )
            finally:
                await bot.bot.close()

        asyncio.run(main())
        """
    )

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=repo_root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=180,
    )

    assert completed.returncode == 0, (
        "Fresh bot extension load failed.\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )


@pytest.mark.parametrize(
    "failure_stage",
    ("constructor", "initialize", "monitoring", "expose"),
)
def test_init_services_failure_is_retryable_and_success_is_idempotent(
    monkeypatch, failure_stage
):
    """A failed composition attempt must not poison the module singleton."""
    import bot as bot_module

    class ExpectedInitializationError(RuntimeError):
        pass

    events = []
    containers = {}
    monitors = {}
    factory_attempts = 0
    original_monitor = object()

    class FakeBot:
        def __init__(self):
            self._monitoring_service = original_monitor

        @property
        def monitoring_service(self):
            return self._monitoring_service

        @monitoring_service.setter
        def monitoring_service(self, value):
            events.append(("assign_monitoring", factory_attempts))
            assert bot_module._container is None
            self._monitoring_service = value

    fake_bot = FakeBot()

    class FakeContainer:
        def __init__(self, attempt):
            self.attempt = attempt
            self.expose_calls = 0

        def initialize(self):
            events.append(("initialize", self.attempt))
            if self.attempt == 1 and failure_stage == "initialize":
                raise ExpectedInitializationError("initialize")

        def expose_to_bot(self, target):
            events.append(("expose", self.attempt))
            self.expose_calls += 1
            assert target is fake_bot
            assert bot_module._container is None
            if self.attempt == 1 and failure_stage == "expose":
                raise ExpectedInitializationError("expose")

    def container_factory(**_kwargs):
        nonlocal factory_attempts
        factory_attempts += 1
        events.append(("constructor", factory_attempts))
        if factory_attempts == 1 and failure_stage == "constructor":
            raise ExpectedInitializationError("constructor")
        container = FakeContainer(factory_attempts)
        containers[factory_attempts] = container
        return container

    def monitoring_factory(db_path, *, usage_monitor):
        assert db_path == bot_module.DB_PATH
        assert usage_monitor is bot_module.usage_monitor
        events.append(("monitoring", factory_attempts))
        if factory_attempts == 1 and failure_stage == "monitoring":
            raise ExpectedInitializationError("monitoring")
        monitor = object()
        monitors[factory_attempts] = monitor
        return monitor

    monkeypatch.setattr(bot_module, "_container", None)
    monkeypatch.setattr(bot_module, "bot", fake_bot)
    monkeypatch.setattr(bot_module, "ServiceContainer", container_factory)
    monkeypatch.setattr(bot_module, "MonitoringService", monitoring_factory)

    with pytest.raises(ExpectedInitializationError, match=failure_stage):
        bot_module._init_services()

    assert bot_module._container is None
    assert fake_bot.monitoring_service is original_monitor
    assert factory_attempts == 1
    if failure_stage == "monitoring":
        assert containers[1].expose_calls == 0

    bot_module._init_services()

    successful_container = containers[2]
    assert factory_attempts == 2
    assert bot_module._container is successful_container
    assert successful_container.expose_calls == 1
    assert fake_bot.monitoring_service is monitors[2]
    success_start = events.index(("constructor", 2))
    assert events[success_start:] == [
        ("constructor", 2),
        ("initialize", 2),
        ("monitoring", 2),
        ("expose", 2),
        ("assign_monitoring", 2),
    ]

    completed_events = list(events)
    bot_module._init_services()

    assert bot_module._container is successful_container
    assert fake_bot.monitoring_service is monitors[2]
    assert events == completed_events
    assert factory_attempts == 2


def test_init_services_passes_only_the_unified_llm_api_key(monkeypatch):
    """The composition root forwards the resolved external key exactly once."""
    import bot as bot_module

    class FakeBot:
        pass

    fake_bot = FakeBot()
    monitor = object()
    instances = []

    class CapturingContainer:
        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self.initialized = False
            self.exposed = False
            instances.append(self)

        def initialize(self):
            self.initialized = True

        def expose_to_bot(self, target):
            assert self.initialized
            assert target is fake_bot
            self.exposed = True

    def monitoring_factory(db_path, *, usage_monitor):
        assert db_path == bot_module.DB_PATH
        assert usage_monitor is bot_module.usage_monitor
        return monitor

    monkeypatch.setattr(bot_module, "_container", None)
    monkeypatch.setattr(bot_module, "bot", fake_bot)
    monkeypatch.setattr(bot_module, "LLM_API_KEY", "sentinel-key")
    monkeypatch.setattr(bot_module, "ServiceContainer", CapturingContainer)
    monkeypatch.setattr(bot_module, "MonitoringService", monitoring_factory)

    bot_module._init_services()

    assert len(instances) == 1
    container = instances[0]
    assert container.kwargs == {
        "db_path": bot_module.DB_PATH,
        "admin_user_ids": bot_module.ADMIN_USER_IDS,
        "lobby_ready_threshold": bot_module.LOBBY_READY_THRESHOLD,
        "lobby_max_players": bot_module.LOBBY_MAX_PLAYERS,
        "use_glicko": bot_module.USE_GLICKO,
        "max_debt": bot_module.MAX_DEBT,
        "leverage_tiers": bot_module.LEVERAGE_TIERS,
        "garnishment_percentage": bot_module.GARNISHMENT_PERCENTAGE,
        "economy_events_enabled": bot_module.ECONOMY_EVENTS_ENABLED,
        "economy_recovery_mode": bot_module.ECONOMY_RECOVERY_MODE,
        "llm_api_key": bot_module.LLM_API_KEY,
        "ai_model": bot_module.AI_MODEL,
        "ai_timeout_seconds": bot_module.AI_TIMEOUT_SECONDS,
        "ai_max_tokens": bot_module.AI_MAX_TOKENS,
    }
    assert container.kwargs["llm_api_key"] == "sentinel-key"
    assert "cerebras_api_key" not in container.kwargs
    assert container.initialized
    assert container.exposed
    assert fake_bot.monitoring_service is monitor
    assert bot_module._container is container


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
