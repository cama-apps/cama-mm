"""BossEncounterView.fight must swallow a double-click.

Without the ``_engaged`` guard a fast second click re-enters resolution before
the first click's await completes and stops the view — on the carried-wager
path that resolves the same phase (and settles the wager) twice.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import commands.dig_helpers.boss_views as bv


def test_fight_ignores_double_click(monkeypatch):
    deferred: list[int] = []

    async def fake_defer(interaction):
        deferred.append(1)

    monkeypatch.setattr(bv, "safe_defer", fake_defer)
    monkeypatch.setattr(bv, "BossWagerModal", lambda *a, **k: MagicMock())

    async def scenario():
        dig_service = MagicMock()
        dig_service.get_carried_wager.return_value = None  # no carry → modal path
        view = bv.BossEncounterView(
            dig_service, user_id=42, guild_id=7, boss_info=MagicMock(),
        )

        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        # ``view.fight`` is a discord.py Button; invoke its bound callback.
        fight = view.fight.callback

        # First click opens the wager modal and marks the view engaged.
        await fight(interaction)
        assert view._engaged is True
        interaction.response.send_modal.assert_awaited_once()

        # Second click is swallowed by the guard — no second modal, just a defer.
        interaction.response.send_modal.reset_mock()
        await fight(interaction)
        interaction.response.send_modal.assert_not_called()
        assert deferred == [1]

    asyncio.run(scenario())


def test_fight_uses_risk_modal_when_wagers_are_disabled(monkeypatch):
    risk_modal = MagicMock()
    risk_modal_factory = MagicMock(return_value=risk_modal)
    wager_modal_factory = MagicMock()

    monkeypatch.setattr(bv, "BossRiskModal", risk_modal_factory)
    monkeypatch.setattr(bv, "BossWagerModal", wager_modal_factory)

    async def scenario():
        dig_service = MagicMock()
        dig_service.get_carried_wager.return_value = None
        boss_info = MagicMock()
        boss_info.wager_allowed = False
        view = bv.BossEncounterView(
            dig_service, user_id=42, guild_id=7, boss_info=boss_info,
        )

        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        await view.fight.callback(interaction)

        interaction.response.send_modal.assert_awaited_once_with(risk_modal)
        assert view._engaged is True
        assert not dig_service.get_carried_wager.called
        wager_modal_factory.assert_not_called()

    asyncio.run(scenario())


def test_risk_modal_submit_reports_unexpected_resolution_failure(monkeypatch):
    deferred = []
    followups = []

    async def fake_defer(interaction, **kwargs):
        deferred.append(kwargs)
        return True

    async def fake_followup(interaction, **kwargs):
        followups.append(kwargs)

    async def fail_resolution(*args, **kwargs):
        raise RuntimeError("boom")

    monkeypatch.setattr(bv, "safe_defer", fake_defer)
    monkeypatch.setattr(bv, "safe_followup", fake_followup)
    monkeypatch.setattr(bv, "_resolve_phase_fight_without_modal", fail_resolution)

    async def scenario():
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="cautious"),
            dig_service=MagicMock(),
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            stop=MagicMock(),
        )
        interaction = MagicMock()

        await bv.BossRiskModal.on_submit(modal, interaction)

        assert deferred == [{"thinking": True}]
        assert followups == [{
            "content": "Boss fight failed. Try again.",
            "ephemeral": True,
        }]
        modal.stop.assert_called_once()

    asyncio.run(scenario())


def _result(**overrides):
    result = {
        "success": True,
        "won": True,
        "boss_name": "Test Boss",
        "payout": 25,
        "jc_delta": 25,
        "knockback": 0,
        "win_chance": 0.5,
        "new_depth": 100,
    }
    result.update(overrides)
    return result


def _interaction(user_id=42):
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=user_id, display_name="Tester"),
        response=SimpleNamespace(
            send_message=AsyncMock(),
            send_modal=AsyncMock(),
            defer=AsyncMock(),
        ),
        followup=SimpleNamespace(send=AsyncMock(return_value=MagicMock())),
        channel=SimpleNamespace(send=AsyncMock(return_value=MagicMock())),
        client=MagicMock(),
    )
    return interaction


def test_encounter_forwards_callback_to_every_fight_entry(monkeypatch):
    wager_factory = MagicMock(return_value=MagicMock())
    risk_factory = MagicMock(return_value=MagicMock())
    resolve = AsyncMock()
    monkeypatch.setattr(bv, "BossWagerModal", wager_factory)
    monkeypatch.setattr(bv, "BossRiskModal", risk_factory)
    monkeypatch.setattr(bv, "_resolve_phase_fight_without_modal", resolve)
    monkeypatch.setattr(bv, "safe_defer", AsyncMock())

    async def scenario():
        callback = AsyncMock()

        wager_service = MagicMock()
        wager_service.get_carried_wager.return_value = None
        wager_view = bv.BossEncounterView(
            wager_service, 42, 7, SimpleNamespace(wager_allowed=True),
            on_boss_resolved=callback,
        )
        await wager_view.fight.callback(_interaction())
        assert wager_factory.call_args.kwargs["on_boss_resolved"] is callback

        risk_view = bv.BossEncounterView(
            MagicMock(), 42, 7, SimpleNamespace(wager_allowed=False),
            on_boss_resolved=callback,
        )
        await risk_view.fight.callback(_interaction())
        assert risk_factory.call_args.kwargs["on_boss_resolved"] is callback

        carried_service = MagicMock()
        carried_service.get_carried_wager.return_value = {
            "risk_tier": "bold",
            "wager": 12,
        }
        carried_view = bv.BossEncounterView(
            carried_service, 42, 7, SimpleNamespace(wager_allowed=True),
            on_boss_resolved=callback,
        )
        await carried_view.fight.callback(_interaction())
        assert resolve.call_args.kwargs["on_boss_resolved"] is callback

    asyncio.run(scenario())


def test_phase_transition_forwards_callback_to_replacement_encounter(monkeypatch):
    encounter = MagicMock()
    encounter_factory = MagicMock(return_value=encounter)
    monkeypatch.setattr(bv, "BossEncounterView", encounter_factory)

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.build_next_boss_encounter.return_value = {
            "name": "Next Form",
            "dialogue": "Again.",
        }
        service.has_scout_lantern.return_value = True
        channel = SimpleNamespace(send=AsyncMock(return_value=MagicMock()))

        await bv._post_phase_transition_followup(
            channel,
            dig_service=service,
            user_id=42,
            guild_id=7,
            result=SimpleNamespace(
                phase2_name="Next Form",
                phase2_title="Transformed",
                dialogue="Again.",
                boss_name="Test Boss",
                wager=12,
                gear_broken=["Stone Cuirass"],
            ),
            on_boss_resolved=callback,
        )

        transition_embed = channel.send.await_args_list[0].kwargs["embed"]
        broken_field = next(
            (field for field in transition_embed.fields if field.name == "Gear Broken"),
            None,
        )
        assert broken_field is not None
        assert "Stone Cuirass" in broken_field.value
        assert encounter_factory.call_args.kwargs["on_boss_resolved"] is callback
        assert encounter.message is channel.send.return_value

    asyncio.run(scenario())


def test_phase_transition_encounter_surfaces_carried_wager(monkeypatch):
    monkeypatch.setattr(bv, "BossEncounterView", MagicMock(return_value=MagicMock()))

    async def scenario():
        service = MagicMock()
        service.build_next_boss_encounter.return_value = {
            "name": "Next Form",
            "dialogue": "Again.",
            "carried_wager": 1_500,
        }
        service.has_scout_lantern.return_value = False
        channel = SimpleNamespace(send=AsyncMock(return_value=MagicMock()))

        await bv._post_phase_transition_followup(
            channel,
            dig_service=service,
            user_id=42,
            guild_id=7,
            result=SimpleNamespace(
                phase2_name="Next Form",
                phase2_title="Transformed",
                dialogue="Again.",
                boss_name="Test Boss",
                wager=1_500,
            ),
        )

        encounter_embed = channel.send.await_args_list[1].kwargs["embed"]
        carried_field = next(
            (field for field in encounter_embed.fields if field.name == "Carried Wager"),
            None,
        )
        assert carried_field is not None
        assert "**1,500**" in carried_field.value

    asyncio.run(scenario())


def test_pinnacle_phase_transition_uses_next_phase_title(monkeypatch):
    monkeypatch.setattr(bv, "BossEncounterView", MagicMock(return_value=MagicMock()))

    async def scenario():
        service = MagicMock()
        service.build_next_boss_encounter.return_value = {
            "name": "The Digger Eternal",
            "dialogue": "Last shift. Last dig. Last.",
        }
        service.has_scout_lantern.return_value = False
        channel = SimpleNamespace(send=AsyncMock(return_value=MagicMock()))

        await bv._post_phase_transition_followup(
            channel,
            dig_service=service,
            user_id=42,
            guild_id=7,
            result=SimpleNamespace(
                phase3_incoming=True,
                next_phase_title="The Digger Eternal",
                dialogue="Eternal. The tunnel is me. I am the tunnel.",
                boss_name="The Digger Unbound",
                wager=0,
            ),
        )

        transition_embed = channel.send.await_args_list[0].kwargs["embed"]
        assert transition_embed.title == "The Digger Eternal Emerges!"
        assert "**The Digger Eternal**" in transition_embed.description
        assert "???" not in transition_embed.description

    asyncio.run(scenario())


def test_duel_prompt_surfaces_stale_cleanup_break_notification():
    result = _result(
        pending_prompt={
            "prompt_title": "Choose",
            "prompt_description": "React.",
            "options": [],
        },
        round_num=2,
        player_hp=4,
        player_hp_max=5,
        boss_hp=5,
        boss_hp_max=6,
        gear_broken=["Stone Cuirass"],
    )

    embed = bv._build_duel_prompt_embed(result)

    broken_field = next(
        (field for field in embed.fields if field.name == "Gear Broken"), None,
    )
    assert broken_field is not None
    assert "Stone Cuirass" in broken_field.value


def test_no_wager_resolution_surfaces_break_notification(monkeypatch):
    monkeypatch.setattr(bv, "_send_boss_victory_neon", AsyncMock())

    async def scenario():
        service = MagicMock()
        service.start_boss_duel.return_value = _result(
            gear_broken=["Stone Cuirass"],
        )
        interaction = _interaction()

        await bv._resolve_phase_fight_without_modal(
            interaction,
            dig_service=service,
            user_id=42,
            guild_id=7,
            risk_tier="bold",
            wager=0,
        )

        embed = interaction.followup.send.await_args.kwargs["embed"]
        broken_field = next(
            (field for field in embed.fields if field.name == "Gear Broken"), None,
        )
        assert broken_field is not None
        assert "Stone Cuirass" in broken_field.value

    asyncio.run(scenario())


def test_wager_modal_resolution_surfaces_break_notification(monkeypatch):
    monkeypatch.setattr(bv, "safe_defer", AsyncMock())
    monkeypatch.setattr(bv, "_send_boss_victory_neon", AsyncMock())

    async def scenario():
        service = MagicMock()
        service.start_boss_duel.return_value = _result(
            gear_broken=["Stone Cuirass"],
        )
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="bold"),
            wager=SimpleNamespace(value="12"),
            dig_service=service,
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            on_boss_resolved=None,
            result=None,
            stop=MagicMock(),
        )
        interaction = _interaction()

        await bv.BossWagerModal.on_submit(modal, interaction)

        embed = interaction.followup.send.await_args.kwargs["embed"]
        broken_field = next(
            (field for field in embed.fields if field.name == "Gear Broken"), None,
        )
        assert broken_field is not None
        assert "Stone Cuirass" in broken_field.value

    asyncio.run(scenario())


@pytest.mark.parametrize(
    "shape",
    [
        {},
        {"won": False, "payout": 0, "jc_delta": -12, "knockback": 8},
        {"phase2_incoming": True},
        {"phase3_incoming": True},
        {"is_pinnacle": True},
        {"is_pinnacle": True, "won": False, "payout": 0, "jc_delta": -12},
        {"is_pinnacle": True, "phase2_incoming": True},
        {"is_pinnacle": True, "phase3_incoming": True},
    ],
    ids=[
        "regular-win",
        "regular-loss",
        "regular-phase2",
        "regular-phase3",
        "pinnacle-win",
        "pinnacle-loss",
        "pinnacle-phase2",
        "pinnacle-phase3",
    ],
)
def test_carried_and_no_wager_start_shapes_notify_once(monkeypatch, shape):
    transition = AsyncMock()
    monkeypatch.setattr(bv, "_post_phase_transition_followup", transition)
    monkeypatch.setattr(bv, "_send_boss_victory_neon", AsyncMock())

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.start_boss_duel.return_value = _result(**shape)
        interaction = _interaction()

        await bv._resolve_phase_fight_without_modal(
            interaction,
            dig_service=service,
            user_id=42,
            guild_id=7,
            risk_tier="bold",
            wager=0,
            on_boss_resolved=callback,
        )

        callback.assert_awaited_once_with(42, 7)
        if shape.get("phase2_incoming") or shape.get("phase3_incoming"):
            assert transition.call_args.kwargs["on_boss_resolved"] is callback
        else:
            interaction.followup.send.assert_awaited()

    asyncio.run(scenario())


@pytest.mark.parametrize(
    "shape",
    [
        {},
        {"won": False, "payout": 0, "jc_delta": -12, "knockback": 8},
        {"phase2_incoming": True},
        {"phase3_incoming": True},
        {"is_pinnacle": True},
        {"is_pinnacle": True, "won": False, "payout": 0, "jc_delta": -12},
        {"is_pinnacle": True, "phase2_incoming": True},
        {"is_pinnacle": True, "phase3_incoming": True},
    ],
    ids=[
        "regular-win",
        "regular-loss",
        "regular-phase2",
        "regular-phase3",
        "pinnacle-win",
        "pinnacle-loss",
        "pinnacle-phase2",
        "pinnacle-phase3",
    ],
)
def test_modal_start_shapes_notify_before_render(monkeypatch, shape):
    events = []
    transition = AsyncMock()

    async def callback(user_id, guild_id):
        events.append(("callback", user_id, guild_id))

    async def send(*args, **kwargs):
        events.append(("render", kwargs))
        return MagicMock()

    monkeypatch.setattr(bv.asyncio, "sleep", AsyncMock())
    monkeypatch.setattr(bv, "_post_phase_transition_followup", transition)
    monkeypatch.setattr(bv, "_send_boss_victory_neon", AsyncMock())

    async def scenario():
        service = MagicMock()
        service.start_boss_duel.return_value = _result(**shape)
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="bold"),
            wager=SimpleNamespace(value="12"),
            dig_service=service,
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            on_boss_resolved=callback,
            result=None,
            stop=MagicMock(),
        )
        interaction = _interaction()
        interaction.followup.send.side_effect = send

        await bv.BossWagerModal.on_submit(modal, interaction)

        assert events[0] == ("callback", 42, 7)
        assert sum(event[0] == "callback" for event in events) == 1
        assert any(event[0] == "render" for event in events)
        if shape.get("phase2_incoming") or shape.get("phase3_incoming"):
            transition.assert_awaited_once()
            assert transition.call_args.kwargs["on_boss_resolved"] is callback

    asyncio.run(scenario())


def test_wager_pending_and_failed_results_do_not_notify(monkeypatch):
    duel = MagicMock()
    duel_factory = MagicMock(return_value=duel)
    monkeypatch.setattr(bv, "BossDuelView", duel_factory)

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="bold"),
            wager=SimpleNamespace(value="12"),
            dig_service=service,
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            on_boss_resolved=callback,
            result=None,
            stop=MagicMock(),
        )

        service.start_boss_duel.return_value = _result(
            pending_prompt={"safe_option_idx": 0, "options": []},
        )
        await bv.BossWagerModal.on_submit(modal, _interaction())
        callback.assert_not_awaited()
        assert duel_factory.call_args.kwargs["on_boss_resolved"] is callback

        duel_factory.reset_mock()
        service.start_boss_duel.return_value = {
            "success": False,
            "error": "No fight.",
        }
        await bv.BossWagerModal.on_submit(modal, _interaction())
        callback.assert_not_awaited()
        duel_factory.assert_not_called()

    asyncio.run(scenario())


def test_wager_validation_errors_do_not_notify():
    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="invalid"),
            wager=SimpleNamespace(value="12"),
            dig_service=service,
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            on_boss_resolved=callback,
        )
        await bv.BossWagerModal.on_submit(modal, _interaction())

        modal.risk_tier.value = "bold"
        modal.wager.value = "not-a-number"
        await bv.BossWagerModal.on_submit(modal, _interaction())

        modal.wager.value = "-1"
        await bv.BossWagerModal.on_submit(modal, _interaction())

        callback.assert_not_awaited()
        service.start_boss_duel.assert_not_called()

    asyncio.run(scenario())


def test_wager_modal_surfaces_maximum():
    wager_input = bv.BossWagerModal.wager._underlying
    assert wager_input.label == "Wager Amount (max 1,000 JC)"
    assert wager_input.placeholder == "0-1000"


def test_risk_modal_forwards_callback_to_no_wager_resolution(monkeypatch):
    resolve = AsyncMock()
    monkeypatch.setattr(bv, "_resolve_phase_fight_without_modal", resolve)
    monkeypatch.setattr(bv, "safe_defer", AsyncMock())

    async def scenario():
        callback = AsyncMock()
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="reckless"),
            dig_service=MagicMock(),
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            on_boss_resolved=callback,
            stop=MagicMock(),
        )
        await bv.BossRiskModal.on_submit(modal, _interaction())

        assert resolve.call_args.kwargs["on_boss_resolved"] is callback
        modal.stop.assert_called_once()

    asyncio.run(scenario())


def test_pending_carried_fight_propagates_without_notifying(monkeypatch):
    duel = MagicMock()
    duel_factory = MagicMock(return_value=duel)
    monkeypatch.setattr(bv, "BossDuelView", duel_factory)

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.start_boss_duel.return_value = _result(
            pending_prompt={"safe_option_idx": 0, "options": []},
        )

        await bv._resolve_phase_fight_without_modal(
            _interaction(),
            dig_service=service,
            user_id=42,
            guild_id=7,
            risk_tier="bold",
            wager=12,
            on_boss_resolved=callback,
        )

        callback.assert_not_awaited()
        assert duel_factory.call_args.kwargs["on_boss_resolved"] is callback

    asyncio.run(scenario())


@pytest.mark.parametrize(
    "shape",
    [
        {},
        {"won": False},
        {"phase2_incoming": True},
        {"phase3_incoming": True},
        {"is_pinnacle": True},
        {"is_pinnacle": True, "won": False},
        {"is_pinnacle": True, "phase2_incoming": True},
        {"is_pinnacle": True, "phase3_incoming": True},
    ],
    ids=[
        "regular-win",
        "regular-loss",
        "regular-phase2",
        "regular-phase3",
        "pinnacle-win",
        "pinnacle-loss",
        "pinnacle-phase2",
        "pinnacle-phase3",
    ],
)
def test_resume_shapes_notify_once_before_render(shape):
    async def scenario():
        events = []

        async def callback(user_id, guild_id):
            events.append(("callback", user_id, guild_id))

        service = MagicMock()
        service.resume_boss_duel.return_value = _result(**shape)
        view = bv.BossDuelView(
            dig_service=service,
            user_id=42,
            guild_id=7,
            initial_result={"pending_prompt": {"options": []}},
            risk_tier="bold",
            wager=12,
            on_boss_resolved=callback,
        )

        async def render(result):
            events.append(("render", result))

        view._render_resolution = AsyncMock(side_effect=render)
        await view._submit(1)
        await view._submit(1)

        assert events[0] == ("callback", 42, 7)
        assert sum(event[0] == "callback" for event in events) == 1
        assert sum(event[0] == "render" for event in events) == 1
        service.resume_boss_duel.assert_called_once_with(42, 7, 1)

    asyncio.run(scenario())


@pytest.mark.parametrize("phase_flag", ["phase2_incoming", "phase3_incoming"])
def test_prompted_pinnacle_phase_clear_posts_next_encounter(monkeypatch, phase_flag):
    transition = AsyncMock()
    monkeypatch.setattr(bv, "_post_phase_transition_followup", transition)

    async def scenario():
        view = bv.BossDuelView(
            dig_service=MagicMock(),
            user_id=42,
            guild_id=7,
            initial_result={"pending_prompt": {"options": []}},
            risk_tier="bold",
            wager=12,
        )
        view.message = SimpleNamespace(edit=AsyncMock(), channel=SimpleNamespace())
        result = SimpleNamespace(**_result(
            **{phase_flag: True},
            is_pinnacle=True,
            boss_name="The Digger Unbound",
            payout=0,
            jc_delta=0,
        ))

        await view._render_resolution(result)

        transition.assert_awaited_once()
        cleared_embed = view.message.edit.await_args.kwargs["embed"]
        assert cleared_embed.title == "Phase Cleared"

    asyncio.run(scenario())


def test_resume_pending_replaces_view_without_notifying(monkeypatch):
    replacement = MagicMock()
    duel_factory = MagicMock(return_value=replacement)

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.resume_boss_duel.return_value = _result(
            pending_prompt={"safe_option_idx": 0, "options": []},
        )
        view = bv.BossDuelView(
            dig_service=service,
            user_id=42,
            guild_id=7,
            initial_result={"pending_prompt": {"options": []}},
            risk_tier="bold",
            wager=12,
            on_boss_resolved=callback,
        )
        monkeypatch.setattr(bv, "BossDuelView", duel_factory)
        view.message = SimpleNamespace(edit=AsyncMock(), channel=None)

        await view._submit(1)

        callback.assert_not_awaited()
        assert duel_factory.call_args.kwargs["on_boss_resolved"] is callback

    asyncio.run(scenario())


@pytest.mark.parametrize("trigger", ["button", "timeout"])
def test_button_and_timeout_resume_notify(monkeypatch, trigger):
    monkeypatch.setattr(bv, "safe_defer", AsyncMock())

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.resume_boss_duel.return_value = _result()
        view = bv.BossDuelView(
            dig_service=service,
            user_id=42,
            guild_id=7,
            initial_result={
                "pending_prompt": {
                    "safe_option_idx": 2,
                    "options": [{"option_idx": 1, "label": "Strike"}],
                },
            },
            risk_tier="bold",
            wager=12,
            on_boss_resolved=callback,
        )
        view._render_resolution = AsyncMock()

        if trigger == "button":
            await view.children[0].callback(_interaction())
            expected_option = 1
        else:
            await view.on_timeout()
            expected_option = 2

        callback.assert_awaited_once_with(42, 7)
        service.resume_boss_duel.assert_called_once_with(42, 7, expected_option)

    asyncio.run(scenario())


def test_callback_failure_is_logged_and_result_still_renders(caplog):
    async def scenario():
        async def fail_callback(user_id, guild_id):
            raise RuntimeError("reminder unavailable")

        service = MagicMock()
        service.resume_boss_duel.return_value = _result()
        view = bv.BossDuelView(
            dig_service=service,
            user_id=42,
            guild_id=7,
            initial_result={"pending_prompt": {"options": []}},
            risk_tier="bold",
            wager=12,
            on_boss_resolved=fail_callback,
        )
        view._render_resolution = AsyncMock()

        await view._submit(0)

        view._render_resolution.assert_awaited_once()

    with caplog.at_level("WARNING", logger="cama_bot.commands.dig"):
        asyncio.run(scenario())
    assert "Boss resolved callback failed for user 42 in guild 7" in caplog.text


@pytest.mark.parametrize("service_result", [{"success": False, "error": "No."}, RuntimeError("boom")])
def test_resume_failure_does_not_notify(service_result):
    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        if isinstance(service_result, Exception):
            service.resume_boss_duel.side_effect = service_result
        else:
            service.resume_boss_duel.return_value = service_result
        view = bv.BossDuelView(
            dig_service=service,
            user_id=42,
            guild_id=7,
            initial_result={"pending_prompt": {"options": []}},
            risk_tier="bold",
            wager=12,
            on_boss_resolved=callback,
        )
        view._edit_message = AsyncMock()

        await view._submit(0)

        callback.assert_not_awaited()

    asyncio.run(scenario())


def test_unauthorized_duplicate_and_nonfight_actions_do_not_notify(monkeypatch):
    monkeypatch.setattr(bv, "safe_defer", AsyncMock())
    monkeypatch.setattr(bv, "safe_followup", AsyncMock())

    async def scenario():
        callback = AsyncMock()
        service = MagicMock()
        service.get_carried_wager.return_value = None
        service.retreat_boss.return_value = {
            "success": True,
            "loss": 2,
            "new_depth": 98,
        }
        service.scout_boss.return_value = {
            "success": True,
            "boss_name": "Test Boss",
            "odds": {},
        }
        service.cheer_boss.return_value = {
            "success": True,
            "total_boost": 0.1,
            "cheer_count": 1,
        }
        view = bv.BossEncounterView(
            service,
            42,
            7,
            SimpleNamespace(wager_allowed=True),
            has_lantern=True,
            on_boss_resolved=callback,
        )

        await view.fight.callback(_interaction(user_id=99))
        service.get_carried_wager.assert_not_called()

        view._engaged = True
        await view.fight.callback(_interaction())
        service.get_carried_wager.assert_not_called()

        await view.retreat.callback(_interaction())
        await view.scout.callback(_interaction())
        await view.cheer.callback(_interaction(user_id=99))

        callback.assert_not_awaited()

    asyncio.run(scenario())
