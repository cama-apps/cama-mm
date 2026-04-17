"""Tests for the /russianroulette command."""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest


@pytest.fixture(scope="module")
def loaded_bot():
    """Load bot extensions once so commands are registered on the tree."""
    import bot

    asyncio.run(bot._load_extensions())
    return bot.bot


class TestRussianRouletteRegistration:
    def test_command_is_registered(self, loaded_bot):
        names = {cmd.name for cmd in loaded_bot.tree.get_commands()}
        assert "russianroulette" in names

    def test_every_whitelisted_command_resolves(self, loaded_bot):
        """Every entry in ROULETTE_ENTRIES must map to a real registered command.

        Guards against drift when commands are renamed or removed.
        """
        from commands.russianroulette import ROULETTE_ENTRIES, _resolve_command

        missing = [
            entry.display
            for entry in ROULETTE_ENTRIES
            if _resolve_command(loaded_bot, entry.command) is None
        ]
        assert not missing, f"Whitelisted commands not found in tree: {missing}"

    def test_whitelist_excludes_admin_commands(self):
        """The whitelist must not include any admin-gated commands."""
        from commands.russianroulette import ROULETTE_ENTRIES

        forbidden_prefixes = ("admin ", "enrich ")
        forbidden_exact = {
            "recalibrate",
            "resetuser",
            "registeruser",
            "resetloancooldown",
            "resetbankruptcycooldown",
            "extendbetting",
            "correctmatch",
            "adminaddsteamid",
            "adminremovesteamid",
            "adminsetprimarysteam",
            "seedherogrid",
            "filllobbytest",
            "ratinganalysis",
            "trivia-reset-cooldown",
            "predict cancel",
            "predict close",
            "draft restart",
            "draft sampleinprogress",
            "draft samplecomplete",
            "dig resetcooldown",
            "dig forceevent",
            "dig setdepth",
        }
        for entry in ROULETTE_ENTRIES:
            assert not any(entry.command.startswith(p) for p in forbidden_prefixes), (
                f"Admin-grouped command in whitelist: {entry.command}"
            )
            assert entry.command not in forbidden_exact, (
                f"Admin command in whitelist: {entry.command}"
            )

    def test_shop_entries_prefill_safe_items(self):
        """Every /shop entry must prefill an item that doesn't need target/hero."""
        from commands.russianroulette import ROULETTE_ENTRIES

        safe_items = {
            "announce",
            "mystery_gift",
            "double_or_nothing",
            "recalibrate",
            "dig_dynamite",
            "dig_hard_hat",
            "dig_lantern",
            "dig_reinforcement",
            "dig_upgrade",
        }
        shop_entries = [e for e in ROULETTE_ENTRIES if e.command == "shop"]
        assert shop_entries, "expected at least one /shop entry"
        for e in shop_entries:
            assert "item" in e.kwargs, f"shop entry missing prefilled item: {e.display}"
            assert e.kwargs["item"] in safe_items, (
                f"shop entry {e.display!r} prefills unsafe item {e.kwargs['item']!r} "
                f"(requires target/hero)"
            )
            # Disallow target/hero prefill — roulette shouldn't pick a random victim
            assert "target" not in e.kwargs
            assert "hero" not in e.kwargs


@pytest.fixture(autouse=True)
def _clear_spin_cooldowns():
    """Reset the in-memory cooldown dict before each test."""
    from commands.russianroulette import _spin_cooldowns

    _spin_cooldowns.clear()
    yield
    _spin_cooldowns.clear()


class TestRussianRouletteDelegation:
    def _mock_interaction(self) -> MagicMock:
        interaction = MagicMock(spec=discord.Interaction)
        interaction.user = MagicMock()
        interaction.user.id = 42
        interaction.user.mention = "<@42>"
        # Set guild=None so the cooldown key is deterministic across mocks
        interaction.guild = None
        interaction.channel = MagicMock()
        interaction.channel.send = AsyncMock()
        interaction.response = MagicMock()
        interaction.response.is_done = MagicMock(return_value=False)
        interaction.response.send_message = AsyncMock()
        interaction.followup = MagicMock()
        interaction.followup.send = AsyncMock()
        return interaction

    @pytest.mark.asyncio
    async def test_delegates_to_randomly_chosen_command(self, loaded_bot):
        """Running /russianroulette should invoke the callback of the rolled command."""
        from commands.russianroulette import (
            RouletteEntry,
            RussianRouletteCommands,
            _resolve_command,
        )

        cog = RussianRouletteCommands(loaded_bot)
        interaction = self._mock_interaction()

        target_cmd = _resolve_command(loaded_bot, "balance")
        assert target_cmd is not None
        entry = RouletteEntry("balance")
        stub = AsyncMock()

        with patch("commands.russianroulette.random.choice") as mock_choice:
            mock_choice.return_value = (entry, target_cmd)
            with patch.object(target_cmd, "_callback", new=stub):
                await cog.russianroulette.callback(cog, interaction)

        interaction.channel.send.assert_awaited_once()
        announce_text = interaction.channel.send.await_args.args[0]
        assert "/balance" in announce_text
        assert stub.await_count == 1
        args = stub.await_args.args
        assert interaction in args
        # No kwargs prefilled for a plain read-only entry
        assert stub.await_args.kwargs == {}

    @pytest.mark.asyncio
    async def test_delegates_with_prefilled_kwargs(self, loaded_bot):
        """Shop entries should pass their prefilled item kwarg to the callback."""
        from commands.russianroulette import (
            RouletteEntry,
            RussianRouletteCommands,
            _resolve_command,
        )

        cog = RussianRouletteCommands(loaded_bot)
        interaction = self._mock_interaction()

        target_cmd = _resolve_command(loaded_bot, "shop")
        assert target_cmd is not None
        entry = RouletteEntry("shop", {"item": "recalibrate"}, label="shop recalibrate")
        stub = AsyncMock()

        with patch("commands.russianroulette.random.choice") as mock_choice:
            mock_choice.return_value = (entry, target_cmd)
            with patch.object(target_cmd, "_callback", new=stub):
                await cog.russianroulette.callback(cog, interaction)

        interaction.channel.send.assert_awaited_once()
        announce_text = interaction.channel.send.await_args.args[0]
        assert "/shop recalibrate" in announce_text
        assert stub.await_count == 1
        assert stub.await_args.kwargs == {"item": "recalibrate"}

    @pytest.mark.asyncio
    async def test_cooldown_blocks_second_spin_within_window(self, loaded_bot):
        """A second spin within the cooldown window is rejected without delegating."""
        from commands.russianroulette import (
            RouletteEntry,
            RussianRouletteCommands,
            _resolve_command,
            _spin_cooldowns,
        )

        cog = RussianRouletteCommands(loaded_bot)
        _spin_cooldowns.clear()

        target_cmd = _resolve_command(loaded_bot, "balance")
        assert target_cmd is not None
        entry = RouletteEntry("balance")
        stub = AsyncMock()

        with patch("commands.russianroulette.random.choice") as mock_choice:
            mock_choice.return_value = (entry, target_cmd)
            with patch.object(target_cmd, "_callback", new=stub):
                first = self._mock_interaction()
                await cog.russianroulette.callback(cog, first)
                assert stub.await_count == 1

                second = self._mock_interaction()
                await cog.russianroulette.callback(cog, second)

        assert stub.await_count == 1, "second spin must not delegate"
        second.response.send_message.assert_awaited_once()
        msg = second.response.send_message.await_args.args[0]
        assert "barrel" in msg.lower()

    @pytest.mark.asyncio
    async def test_cooldown_expires_after_window(self, loaded_bot):
        """A spin after the cooldown window is allowed."""
        from commands.russianroulette import (
            SPIN_COOLDOWN_SECONDS,
            RouletteEntry,
            RussianRouletteCommands,
            _resolve_command,
            _spin_cooldowns,
        )

        cog = RussianRouletteCommands(loaded_bot)
        _spin_cooldowns.clear()

        target_cmd = _resolve_command(loaded_bot, "balance")
        entry = RouletteEntry("balance")
        stub = AsyncMock()

        with patch("commands.russianroulette.random.choice") as mock_choice:
            mock_choice.return_value = (entry, target_cmd)
            with patch.object(target_cmd, "_callback", new=stub):
                with patch("commands.russianroulette.time.time", return_value=1000.0):
                    await cog.russianroulette.callback(cog, self._mock_interaction())
                future = 1000.0 + SPIN_COOLDOWN_SECONDS + 1
                with patch("commands.russianroulette.time.time", return_value=future):
                    await cog.russianroulette.callback(cog, self._mock_interaction())

        assert stub.await_count == 2

    @pytest.mark.asyncio
    async def test_misfire_on_delegate_exception(self, loaded_bot):
        """If the delegated callback raises, the user sees a misfire message."""
        from commands.russianroulette import (
            RouletteEntry,
            RussianRouletteCommands,
            _resolve_command,
        )

        cog = RussianRouletteCommands(loaded_bot)
        interaction = self._mock_interaction()

        target_cmd = _resolve_command(loaded_bot, "help")
        assert target_cmd is not None
        entry = RouletteEntry("help")
        boom = AsyncMock(side_effect=RuntimeError("chamber jammed"))

        with patch("commands.russianroulette.random.choice") as mock_choice:
            mock_choice.return_value = (entry, target_cmd)
            with patch.object(target_cmd, "_callback", new=boom):
                await cog.russianroulette.callback(cog, interaction)

        interaction.response.send_message.assert_awaited_once()
        msg = interaction.response.send_message.await_args.args[0]
        assert "misfired" in msg.lower()
        assert "/help" in msg
