"""Tests for rare cross-system rewards triggered by completed digs."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from services.trivia_questions import TriviaQuestion


def test_bonus_roll_uses_mutually_exclusive_one_percent_bands():
    from commands.dig_helpers.bonus_events import pick_dig_bonus

    assert pick_dig_bonus(0.0) == "wheel"
    assert pick_dig_bonus(0.009999) == "wheel"
    assert pick_dig_bonus(0.01) == "package_deal"
    assert pick_dig_bonus(0.019999) == "package_deal"
    assert pick_dig_bonus(0.02) == "trivia"
    assert pick_dig_bonus(0.029999) == "trivia"
    assert pick_dig_bonus(0.03) is None
    assert pick_dig_bonus(0.999999) is None


def test_player_service_exposes_recently_active_package_pool():
    from services.player_service import PlayerService

    repo = MagicMock()
    repo.get_all_registered_players_for_lottery.return_value = [
        {"discord_id": 2},
        {"discord_id": 3},
    ]
    service = PlayerService(repo)

    assert service.get_all_registered_players_for_lottery(99) == [
        {"discord_id": 2},
        {"discord_id": 3},
    ]
    repo.get_all_registered_players_for_lottery.assert_called_once_with(99, 14)


def test_package_candidates_exclude_digger_and_require_four_choices():
    from commands.dig_helpers.bonus_events import choose_package_candidates

    players = [
        SimpleNamespace(id=player_id, display_name=f"Player {player_id}")
        for player_id in range(1, 7)
    ]
    sample = MagicMock(side_effect=lambda candidates, count: candidates[:count])

    choices = choose_package_candidates(players, digger_id=1, sample=sample)

    assert [choice.id for choice in choices] == [2, 3, 4, 5]
    sample.assert_called_once()
    assert all(candidate.id != 1 for candidate in sample.call_args.args[0])
    assert choose_package_candidates(players[:4], digger_id=1, sample=sample) == []


@pytest.mark.asyncio
async def test_package_deal_view_creates_free_three_game_deal():
    from commands.dig_helpers.bonus_events import PackageDealView

    candidates = [
        SimpleNamespace(id=player_id, display_name=f"Player {player_id}")
        for player_id in range(2, 6)
    ]
    package_service = SimpleNamespace(
        create_or_extend_deal=MagicMock(
            return_value=SimpleNamespace(games_remaining=3),
        ),
    )
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=candidates,
        package_deal_service=package_service,
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.select_partner(interaction, candidates[0])

    package_service.create_or_extend_deal.assert_called_once_with(
        guild_id=99,
        buyer_id=1,
        partner_id=2,
        games=3,
        cost=0,
    )
    interaction.response.edit_message.assert_awaited_once()
    content = interaction.response.edit_message.call_args.kwargs["content"]
    assert "unearthed foreman's contract" in content
    assert "signed with **Player 2**" in content
    assert interaction.response.edit_message.call_args.kwargs["view"] is None


@pytest.mark.asyncio
async def test_package_deal_activation_failure_keeps_mine_framing():
    from commands.dig_helpers.bonus_events import PackageDealView

    candidate = SimpleNamespace(id=2, display_name="Player 2")
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=[candidate],
        package_deal_service=SimpleNamespace(
            create_or_extend_deal=MagicMock(
                side_effect=RuntimeError("package service unavailable"),
            ),
        ),
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.select_partner(interaction, candidate)

    content = interaction.response.edit_message.call_args.kwargs["content"]
    assert "unearthed foreman's contract" in content
    assert "could not be activated" in content
    assert interaction.response.edit_message.call_args.kwargs["view"] is None


@pytest.mark.asyncio
async def test_package_deal_confirmation_reports_extension_total():
    from commands.dig_helpers.bonus_events import PackageDealView

    candidate = SimpleNamespace(id=2, display_name="Player 2")
    package_service = SimpleNamespace(
        create_or_extend_deal=MagicMock(
            return_value=SimpleNamespace(games_remaining=8),
        ),
    )
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=[candidate],
        package_deal_service=package_service,
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.select_partner(interaction, candidate)

    content = interaction.response.edit_message.call_args.kwargs["content"]
    assert "3 games added" in content
    assert "8 games remaining" in content


@pytest.mark.asyncio
async def test_package_deal_timeout_disables_expired_choices():
    from commands.dig_helpers.bonus_events import PackageDealView

    candidates = [
        SimpleNamespace(id=player_id, display_name=f"Player {player_id}")
        for player_id in range(2, 6)
    ]
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=candidates,
        package_deal_service=SimpleNamespace(),
    )
    view.message = SimpleNamespace(edit=AsyncMock())

    await view.on_timeout()

    assert all(child.disabled for child in view.children)
    view.message.edit.assert_awaited_once_with(
        content=(
            "*The unearthed foreman's contract crumbled to dust in the mine.*"
        ),
        embed=None,
        view=view,
    )


@pytest.mark.asyncio
async def test_package_deal_ignores_a_deleted_confirmation_message():
    import discord

    from commands.dig_helpers.bonus_events import PackageDealView

    response = MagicMock(status=404, reason="Not Found")
    missing_message = discord.NotFound(
        response,
        {"code": 10008, "message": "Unknown Message"},
    )
    candidate = SimpleNamespace(id=2, display_name="Player 2")
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=[candidate],
        package_deal_service=SimpleNamespace(
            create_or_extend_deal=MagicMock(
                return_value=SimpleNamespace(games_remaining=3),
            ),
        ),
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(
            edit_message=AsyncMock(side_effect=missing_message),
        ),
    )

    await view.select_partner(interaction, candidate)


@pytest.mark.asyncio
async def test_package_deal_view_only_accepts_the_digger():
    from commands.dig_helpers.bonus_events import PackageDealView

    candidates = [
        SimpleNamespace(id=player_id, display_name=f"Player {player_id}")
        for player_id in range(2, 6)
    ]
    view = PackageDealView(
        buyer_id=1,
        guild_id=99,
        candidates=candidates,
        package_deal_service=SimpleNamespace(),
    )

    assert len(view.children) == 4
    assert await view.interaction_check(SimpleNamespace(user=SimpleNamespace(id=1))) is True
    assert await view.interaction_check(SimpleNamespace(user=SimpleNamespace(id=9))) is False


def _trivia_question() -> TriviaQuestion:
    return TriviaQuestion(
        text="Which hero says this?",
        options=["Axe", "Bane", "Chen", "Drow Ranger"],
        correct_index=1,
        difficulty="easy",
        image_url=None,
        category="hero_quote",
        explanation="It is Bane.",
    )


@pytest.mark.asyncio
async def test_dig_trivia_correct_answer_applies_dig_reward_policy_with_audit_context():
    from commands.dig_helpers.bonus_events import DigTriviaView

    player_service = SimpleNamespace(adjust_balance=MagicMock(return_value=115))
    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=player_service,
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.answer(interaction, 1)

    player_service.adjust_balance.assert_called_once_with(
        1,
        99,
        10,
        source="dig",
        actor_id=1,
        related_type="dig_bonus_trivia",
        related_id="hero_quote",
        reason="dig bonus trivia correct answer",
        metadata={
            "correct": True,
            "timed_out": False,
            "gross_jc": 15,
            "reward_multiplier": 0.65,
        },
    )
    result_embed = interaction.response.edit_message.call_args.kwargs["embed"]
    assert result_embed.title == "⛏️ Unearthed Rune Tablet — Correct! +10 JC"
    assert "rune tablet reveals the correct answer" in result_embed.description


@pytest.mark.asyncio
async def test_dig_trivia_persists_its_economy_ledger_context(repo_db_path):
    import json
    import sqlite3

    from commands.dig_helpers.bonus_events import DigTriviaView
    from repositories.player_repository import PlayerRepository
    from services.player_service import PlayerService

    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(1, "Digger", 99)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute("DELETE FROM economy_ledger_entries")

    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=PlayerService(player_repo),
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.answer(interaction, 1)

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute(
            """
            SELECT delta, source, actor_id, related_type, related_id,
                   reason, metadata
            FROM economy_ledger_entries
            ORDER BY ledger_id DESC
            LIMIT 1
            """
        ).fetchone()

    assert dict(row) | {"metadata": json.loads(row["metadata"])} == {
        "delta": 10,
        "source": "dig",
        "actor_id": 1,
        "related_type": "dig_bonus_trivia",
        "related_id": "hero_quote",
        "reason": "dig bonus trivia correct answer",
        "metadata": {
            "correct": True,
            "timed_out": False,
            "gross_jc": 15,
            "reward_multiplier": 0.65,
        },
    }


@pytest.mark.asyncio
async def test_dig_trivia_answer_surfaces_settlement_failure():
    from commands.dig_helpers.bonus_events import DigTriviaView

    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=SimpleNamespace(
            adjust_balance=MagicMock(side_effect=RuntimeError("db unavailable")),
        ),
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.answer(interaction, 1)

    error_embed = interaction.response.edit_message.call_args.kwargs["embed"]
    assert error_embed.title == "⛏️ Unearthed Rune Tablet — Settlement Error"
    assert "mine's rune tablet" in error_embed.description
    assert interaction.response.edit_message.call_args.kwargs["view"] is None


@pytest.mark.asyncio
async def test_dig_trivia_answer_ignores_a_deleted_message():
    import discord

    from commands.dig_helpers.bonus_events import DigTriviaView

    response = MagicMock(status=404, reason="Not Found")
    missing_message = discord.NotFound(
        response,
        {"code": 10008, "message": "Unknown Message"},
    )
    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=SimpleNamespace(adjust_balance=MagicMock(return_value=115)),
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(
            edit_message=AsyncMock(side_effect=missing_message),
        ),
    )

    await view.answer(interaction, 1)


@pytest.mark.asyncio
async def test_dig_trivia_wrong_answer_loses_five_jc():
    from commands.dig_helpers.bonus_events import DigTriviaView

    player_service = SimpleNamespace(adjust_balance=MagicMock(return_value=95))
    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=player_service,
    )
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1),
        response=SimpleNamespace(edit_message=AsyncMock()),
    )

    await view.answer(interaction, 0)

    assert player_service.adjust_balance.call_args.args[:3] == (1, 99, -5)
    assert player_service.adjust_balance.call_args.kwargs["metadata"] == {
        "correct": False,
        "timed_out": False,
    }
    result_embed = interaction.response.edit_message.call_args.kwargs["embed"]
    assert result_embed.title == "⛏️ Unearthed Rune Tablet — Wrong! -5 JC"
    assert "rune tablet reveals the correct answer" in result_embed.description
    assert "Bane" in result_embed.description


@pytest.mark.asyncio
async def test_dig_trivia_timeout_loses_five_jc():
    from commands.dig_helpers.bonus_events import DigTriviaView

    player_service = SimpleNamespace(adjust_balance=MagicMock(return_value=95))
    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=player_service,
    )
    view.message = SimpleNamespace(edit=AsyncMock())

    await view.on_timeout()

    assert player_service.adjust_balance.call_args.args[:3] == (1, 99, -5)
    assert player_service.adjust_balance.call_args.kwargs["metadata"] == {
        "correct": False,
        "timed_out": True,
    }
    timeout_embed = view.message.edit.call_args.kwargs["embed"]
    assert timeout_embed.title == "⛏️ Unearthed Rune Tablet — Time's up! -5 JC"
    assert "rune tablet reveals the correct answer" in timeout_embed.description


@pytest.mark.asyncio
async def test_dig_trivia_timeout_surfaces_settlement_failure():
    from commands.dig_helpers.bonus_events import DigTriviaView

    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=SimpleNamespace(
            adjust_balance=MagicMock(side_effect=RuntimeError("db unavailable")),
        ),
    )
    view.message = SimpleNamespace(edit=AsyncMock())

    await view.on_timeout()

    error_embed = view.message.edit.call_args.kwargs["embed"]
    assert error_embed.title == "⛏️ Unearthed Rune Tablet — Settlement Error"
    assert "mine's rune tablet" in error_embed.description
    assert view.message.edit.call_args.kwargs["view"] is None


@pytest.mark.asyncio
async def test_dig_trivia_timeout_ignores_a_deleted_message():
    import discord

    from commands.dig_helpers.bonus_events import DigTriviaView

    response = MagicMock(status=404, reason="Not Found")
    missing_message = discord.NotFound(
        response,
        {"code": 10008, "message": "Unknown Message"},
    )
    view = DigTriviaView(
        user_id=1,
        guild_id=99,
        question=_trivia_question(),
        player_service=SimpleNamespace(adjust_balance=MagicMock(return_value=95)),
    )
    view.message = SimpleNamespace(
        edit=AsyncMock(side_effect=missing_message),
    )

    await view.on_timeout()


@pytest.mark.asyncio
async def test_maybe_send_dig_bonus_only_rolls_for_consumed_digs():
    from commands.dig_helpers.bonus_events import maybe_send_dig_bonus

    with patch(
        "commands.dig_helpers.bonus_events.send_dig_bonus",
        new_callable=AsyncMock,
        create=True,
    ) as send_bonus:
        await maybe_send_dig_bonus(
            MagicMock(), MagicMock(), SimpleNamespace(dig_consumed=False), roll=0.0,
        )
        send_bonus.assert_not_awaited()

        bot = MagicMock()
        interaction = MagicMock()
        await maybe_send_dig_bonus(
            bot, interaction, SimpleNamespace(dig_consumed=True), roll=0.015,
        )
        send_bonus.assert_awaited_once_with(bot, interaction, "package_deal")


@pytest.mark.asyncio
async def test_bonus_dispatch_failure_never_masks_a_successful_dig():
    from commands.dig_helpers.bonus_events import maybe_send_dig_bonus

    with patch(
        "commands.dig_helpers.bonus_events.send_dig_bonus",
        new_callable=AsyncMock,
        side_effect=RuntimeError("bonus failed"),
    ):
        with patch(
            "commands.dig_helpers.bonus_events.safe_followup",
            new_callable=AsyncMock,
            side_effect=RuntimeError("followup failed"),
        ):
            await maybe_send_dig_bonus(
                MagicMock(),
                MagicMock(),
                SimpleNamespace(dig_consumed=True),
                roll=0.0,
            )


@pytest.mark.asyncio
async def test_bonus_dispatch_failure_does_not_claim_reward_was_rolled_back():
    from commands.dig_helpers.bonus_events import maybe_send_dig_bonus

    interaction = MagicMock()
    with patch(
        "commands.dig_helpers.bonus_events.send_dig_bonus",
        new_callable=AsyncMock,
        side_effect=RuntimeError("bonus failed after settlement"),
    ):
        with patch(
            "commands.dig_helpers.bonus_events.safe_followup",
            new_callable=AsyncMock,
        ) as report_failure:
            await maybe_send_dig_bonus(
                MagicMock(),
                interaction,
                SimpleNamespace(dig_consumed=True),
                roll=0.0,
            )

    content = report_failure.call_args.kwargs["content"]
    assert "may already be recorded" in content


@pytest.mark.asyncio
async def test_send_dig_bonus_routes_wheel_to_bonus_mode():
    from commands.dig_helpers.bonus_events import send_dig_bonus

    betting_cog = SimpleNamespace(_gamba_action=AsyncMock())
    bot = SimpleNamespace(get_cog=MagicMock(return_value=betting_cog))
    interaction = MagicMock()

    await send_dig_bonus(bot, interaction, "wheel")

    bot.get_cog.assert_called_once_with("BettingCommands")
    betting_cog._gamba_action.assert_awaited_once_with(
        interaction, bonus_spin=True,
    )


@pytest.mark.asyncio
async def test_send_dig_bonus_offers_four_active_package_players():
    from commands.dig_helpers.bonus_events import PackageDealView, send_dig_bonus

    members = {
        player_id: SimpleNamespace(
            id=player_id,
            display_name=f"Player {player_id}",
            bot=False,
        )
        for player_id in range(1, 7)
    }
    player_service = SimpleNamespace(
        get_all_registered_players_for_lottery=MagicMock(
            return_value=[{"discord_id": player_id} for player_id in members],
        ),
    )
    bot = SimpleNamespace(
        package_deal_service=MagicMock(),
        player_service=player_service,
    )
    sent_message = MagicMock()
    interaction = SimpleNamespace(
        user=members[1],
        guild=SimpleNamespace(id=99, get_member=lambda player_id: members.get(player_id)),
        followup=SimpleNamespace(send=AsyncMock(return_value=sent_message)),
        channel=MagicMock(),
    )

    await send_dig_bonus(bot, interaction, "package_deal")

    player_service.get_all_registered_players_for_lottery.assert_called_once_with(99)
    sent_view = interaction.followup.send.call_args.kwargs["view"]
    assert isinstance(sent_view, PackageDealView)
    assert len(sent_view.children) == 4
    assert sent_view.buyer_id == 1
    assert sent_view.message is sent_message
    sent_embed = interaction.followup.send.call_args.kwargs["embed"]
    assert sent_embed.title == "⛏️ Unearthed in the Mine — Package Deal"
    assert "pickaxe" in sent_embed.description
    assert "buried" in sent_embed.description
    assert "free Package Deal for **3 games**" in sent_embed.description


@pytest.mark.asyncio
async def test_send_dig_bonus_insufficient_package_candidates_keeps_mine_framing():
    from commands.dig_helpers.bonus_events import send_dig_bonus

    members = {
        player_id: SimpleNamespace(
            id=player_id,
            display_name=f"Player {player_id}",
            bot=False,
        )
        for player_id in range(1, 5)
    }
    player_service = SimpleNamespace(
        get_all_registered_players_for_lottery=MagicMock(
            return_value=[{"discord_id": player_id} for player_id in members],
        ),
    )
    interaction = SimpleNamespace(
        user=members[1],
        guild=SimpleNamespace(id=99, get_member=lambda player_id: members.get(player_id)),
        followup=SimpleNamespace(send=AsyncMock()),
        channel=MagicMock(),
    )
    bot = SimpleNamespace(
        package_deal_service=MagicMock(),
        player_service=player_service,
    )

    await send_dig_bonus(bot, interaction, "package_deal")

    content = interaction.followup.send.call_args.kwargs["content"]
    assert "pickaxe unearthed a foreman's Package Deal contract" in content
    assert "not four eligible active players to sign it" in content
    assert interaction.followup.send.call_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_send_dig_bonus_posts_one_standalone_trivia_question():
    from commands.dig_helpers.bonus_events import DigTriviaView, send_dig_bonus

    question = _trivia_question()
    sent_message = MagicMock()
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=1, display_name="Digger"),
        guild=SimpleNamespace(id=99),
        followup=SimpleNamespace(send=AsyncMock(return_value=sent_message)),
        channel=MagicMock(),
    )
    bot = SimpleNamespace(player_service=MagicMock())

    with patch(
        "commands.dig_helpers.bonus_events.generate_question",
        return_value=question,
        create=True,
    ):
        await send_dig_bonus(bot, interaction, "trivia")

    sent_embed = interaction.followup.send.call_args.kwargs["embed"]
    sent_view = interaction.followup.send.call_args.kwargs["view"]
    assert isinstance(sent_view, DigTriviaView)
    assert sent_view.message is sent_message
    assert sent_embed.title == "⛏️ Unearthed in the Mine — Dota 2 Trivia"
    assert "stone tablet" in sent_embed.description
    assert "mine wall" in sent_embed.description
    assert sent_embed.description.endswith(question.text)
    assert "+10 JC" in sent_embed.footer.text
    assert "-5 JC" in sent_embed.footer.text


@pytest.mark.asyncio
@pytest.mark.parametrize("result_kind", ["normal", "boss", "boon", "choice"])
async def test_dig_result_dispatch_runs_bonus_after_existing_ui(result_kind):
    from commands.dig import DigCommands

    bot = MagicMock()
    cog = DigCommands(bot, MagicMock())
    interaction = MagicMock()
    result = SimpleNamespace(
        is_first_dig=False,
        boss_encounter=result_kind == "boss",
        paid_dig_available=False,
        event=None,
        dig_consumed=True,
    )
    if result_kind == "boon":
        result.event = {"complexity": "boon", "boon_options": ["one"]}
    elif result_kind == "choice":
        result.event = {"safe_option": {"id": "safe"}}

    cog._send_normal_dig_result = AsyncMock()
    cog._handle_boss_encounter = AsyncMock()
    cog._handle_boon_encounter = AsyncMock()
    cog._handle_choice_encounter = AsyncMock()

    with patch(
        "commands.dig.maybe_send_dig_bonus",
        new_callable=AsyncMock,
        create=True,
    ) as maybe_bonus:
        await cog._dispatch_dig_result(interaction, 99, result)

    maybe_bonus.assert_awaited_once_with(bot, interaction, result)
    if result_kind == "normal":
        cog._send_normal_dig_result.assert_awaited_once_with(interaction, result)
    elif result_kind == "boss":
        cog._handle_boss_encounter.assert_awaited_once_with(interaction, 99, result)
    elif result_kind == "boon":
        cog._handle_boon_encounter.assert_awaited_once()
    else:
        cog._handle_choice_encounter.assert_awaited_once()


@pytest.mark.asyncio
async def test_paid_dig_dispatches_bonus_for_the_consumed_paid_result():
    from commands.dig import DigCommands

    bot = MagicMock()
    cog = DigCommands(bot, MagicMock())
    interaction = MagicMock()
    interaction.user.id = 1
    prompt_result = SimpleNamespace(paid_dig_cost=5, cooldown_remaining=60)
    paid_result = SimpleNamespace(
        success=True,
        event=None,
        dig_consumed=True,
    )
    cog._run_dig = AsyncMock(return_value=paid_result)
    cog._schedule_dig_reminder = AsyncMock()
    message = SimpleNamespace(edit=AsyncMock())
    paid_view = SimpleNamespace(wait=AsyncMock(), value=True)

    with patch("commands.dig.PaidDigView", return_value=paid_view):
        with patch("commands.dig.safe_followup", AsyncMock(return_value=message)):
            with patch(
                "commands.dig._build_dig_embed",
                return_value=(MagicMock(), None, 0, []),
            ):
                with patch("commands.dig._attach_layer_thumbnail", AsyncMock(return_value=None)):
                    with patch("commands.dig._attach_pickaxe_footer", AsyncMock(return_value=None)):
                        with patch("commands.dig._attach_items_strip", AsyncMock(return_value=None)):
                            with patch(
                                "commands.dig.maybe_send_dig_bonus",
                                new_callable=AsyncMock,
                                create=True,
                            ) as maybe_bonus:
                                await cog._handle_paid_dig_confirmation(
                                    interaction, 99, prompt_result,
                                )

    maybe_bonus.assert_awaited_once_with(bot, interaction, paid_result)
