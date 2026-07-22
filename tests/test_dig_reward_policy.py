import json
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

import services.dig_service as dig_service_module
from repositories.dig_repository import DigRepository
from services.dig_constants import BOSS_BOUNDARIES, PINNACLE_DEPTH
from services.dig_data.balance import scale_positive_dig_jc
from services.dig_service import DigService


@pytest.mark.parametrize(
    ("gross", "expected"),
    [
        (1, 1),
        (2, 1),
        (3, 2),
        (5, 3),
        (15, 10),
        (100, 65),
        (1000, 650),
    ],
)
def test_positive_dig_jc_uses_half_up_rounding_with_minimum_one(
    gross: int,
    expected: int,
) -> None:
    assert scale_positive_dig_jc(gross) == expected


@pytest.mark.parametrize("amount", [0, -1, -5, -100])
def test_dig_jc_policy_does_not_scale_non_positive_amounts(amount: int) -> None:
    assert scale_positive_dig_jc(amount) == amount


def test_first_dig_credits_and_logs_scaled_reward(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81001
    player_repository.add(
        discord_id=player_id,
        discord_username="reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    monkeypatch.setattr("services.dig.dig_core_mixin.random.randint", lambda _a, b: b)

    result = service.dig(player_id, guild_id)

    assert result["jc_earned"] == 3
    assert player_repository.get_balance(player_id, guild_id) == 103
    action = service.dig_repo.get_recent_actions(player_id, guild_id, limit=1)[0]
    assert action["jc_delta"] == 3
    assert '"gross_jc": 5' in action["detail"]
    assert '"reward_multiplier": 0.65' in action["detail"]


def test_positive_event_reward_is_scaled_once_and_audited(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81002
    player_repository.add(
        discord_id=player_id,
        discord_username="event_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    event = {
        "id": "reward_policy_event",
        "name": "Reward Policy Event",
        "rarity": "common",
        "safe_option": {
            "success_chance": 1.0,
            "success": {
                "description": "A compact reward.",
                "advance": 0,
                "jc": 15,
                "cave_in": False,
            },
            "failure": None,
        },
    }
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda _a, _b: 1.0)

    result = service.resolve_event(player_id, guild_id, event["id"], "safe")

    assert result["jc_delta"] == 10
    action = service.dig_repo.get_recent_actions(
        player_id,
        guild_id,
        limit=1,
        action_type="event",
    )[0]
    assert action["jc_delta"] == 10
    assert '"gross_jc": 15' in action["detail"]
    assert '"reward_multiplier": 0.65' in action["detail"]


def test_legacy_event_reward_is_scaled_once_and_audited(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81010
    player_repository.add(
        discord_id=player_id,
        discord_username="legacy_event_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    event = {
        "id": "legacy_reward_policy_event",
        "name": "Legacy Reward Policy Event",
        "outcomes": {
            "inspect": {
                "message": "A compact reward.",
                "jc": 15,
            },
        },
    }
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])

    result = service.resolve_event(
        player_id, guild_id, event["id"], "inspect"
    )

    assert result["jc_delta"] == 10
    action = service.dig_repo.get_recent_actions(
        player_id,
        guild_id,
        limit=1,
        action_type="event",
    )[0]
    assert action["jc_delta"] == 10
    assert '"gross_jc": 15' in action["detail"]
    assert '"reward_multiplier": 0.65' in action["detail"]


def test_slow_drip_tracks_gross_cap_but_credits_scaled_reward(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81011
    player_repository.add(
        discord_id=player_id,
        discord_username="slow_drip_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    slow_drip_repo = MagicMock()
    slow_drip_repo.get_today.return_value = {
        "claimed_today": 0,
        "last_claim_at": 1_000,
    }
    service = DigService(
        DigRepository(repo_db_path),
        player_repository,
        slow_drip_repo=slow_drip_repo,
    )
    monkeypatch.setattr(service, "_has_relic", lambda *_args: True)
    monkeypatch.setattr("services.dig.gear_mixin.time.time", lambda: 2_200)

    credited = service._claim_slow_drip(
        player_id,
        guild_id,
        last_dig_at=None,
    )

    assert credited == 7
    slow_drip_repo.add_claim.assert_called_once_with(
        player_id,
        guild_id,
        service._get_game_date(),
        10,
    )
    assert player_repository.get_balance(player_id, guild_id) == 107


def test_deterministic_dig_outcome_scales_full_positive_payout_before_sinks(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81003
    player_repository.add(
        discord_id=player_id,
        discord_username="deterministic_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    monkeypatch.setattr(service, "_get_weather_effects", lambda *_args: {})
    service.dig(player_id, guild_id)
    service.dig_repo.update_tunnel(
        player_id,
        guild_id,
        depth=10,
        max_depth=10,
        last_dig_at=0,
        streak_days=0,
        streak_last_date=None,
    )
    terminal, preconditions = service.dig_with_preconditions(player_id, guild_id)
    assert terminal is None
    balance_before = player_repository.get_balance(player_id, guild_id)

    result = service.apply_dig_outcome(
        preconditions,
        {
            "advance": 1,
            "jc_earned": 15,
            "cave_in": False,
            "event_id": "",
        },
    )

    assert result["jc_earned"] == 10
    assert result["gross_jc"] == 15
    assert player_repository.get_balance(player_id, guild_id) == balance_before + 10


def test_live_dig_scales_full_positive_payout_before_sinks(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81004
    player_repository.add(
        discord_id=player_id,
        discord_username="live_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    balance_before = player_repository.get_balance(player_id, guild_id)
    service.dig_repo.update_tunnel(
        player_id,
        guild_id,
        depth=10,
        max_depth=10,
        last_dig_at=0,
        streak_days=0,
        streak_last_date=None,
    )
    layer = {
        "name": "Dirt",
        "min_depth": 0,
        "max_depth": 24,
        "cave_in_pct": 0.0,
        "jc_min": 15,
        "jc_max": 15,
        "advance_min": 1,
        "advance_max": 1,
        "emoji": "",
    }
    monkeypatch.setattr(service, "_get_layer", lambda _depth: layer)
    monkeypatch.setattr(service, "_get_weather_effects", lambda *_args: {})
    monkeypatch.setattr(service, "roll_artifact", lambda *_args, **_kwargs: None)
    monkeypatch.setattr("services.dig_service.random.random", lambda: 0.99)

    result = service.dig(player_id, guild_id)

    assert result["gross_jc"] == 15
    assert result["jc_earned"] == 10
    assert player_repository.get_balance(player_id, guild_id) == balance_before + 10


def test_cave_in_scales_combined_positive_reward_once(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81012
    player_repository.add(
        discord_id=player_id,
        discord_username="cave_in_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    # Weather is unrelated to reward scaling; a random Mudslide would cap the
    # block loss and therefore change Gambler's Charm's authored gross bonus.
    monkeypatch.setattr(service, "_get_weather_effects", lambda *_args: {})
    service.dig(player_id, guild_id)
    service.dig_repo.update_tunnel(
        player_id,
        guild_id,
        depth=10,
        max_depth=10,
        last_dig_at=0,
        mutations=json.dumps([{"id": "cave_in_loot"}]),
    )
    balance_before = player_repository.get_balance(player_id, guild_id)
    monkeypatch.setattr(
        service,
        "_has_relic",
        lambda _pid, _gid, relic_id: relic_id == "gamblers_charm",
    )
    monkeypatch.setattr("services.dig_service.random.random", lambda: 0.0)

    def rigged_randint(low, high):
        if (low, high) == (6, 14):
            return 8
        if (low, high) == (1, 3):
            return 1
        return low

    monkeypatch.setattr("services.dig_service.random.randint", rigged_randint)

    result = service.dig(player_id, guild_id)

    assert result["cave_in"] is True
    # Lucky Rubble (1) + Gambler's Charm (4) is one 5-JC cave-in mint.
    assert player_repository.get_balance(player_id, guild_id) == (
        balance_before + scale_positive_dig_jc(5)
    )


def test_prestige_grant_is_scaled_and_audited(
    repo_db_path,
    player_repository,
    guild_id: int,
) -> None:
    player_id = 81005
    player_repository.add(
        discord_id=player_id,
        discord_username="prestige_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    balance_before = player_repository.get_balance(player_id, guild_id)
    defeated = {
        str(boundary): {"boss_id": f"boss_{boundary}", "status": "defeated"}
        for boundary in (*BOSS_BOUNDARIES, PINNACLE_DEPTH)
    }
    service.dig_repo.update_tunnel(
        player_id,
        guild_id,
        depth=PINNACLE_DEPTH,
        max_depth=PINNACLE_DEPTH,
        boss_progress=json.dumps(defeated),
    )

    result = service.prestige(player_id, guild_id, "advance_boost")

    assert result["prestige_grant"]["jc"] == 650
    assert player_repository.get_balance(player_id, guild_id) == balance_before + 650
    action = service.dig_repo.get_recent_actions(
        player_id,
        guild_id,
        limit=1,
        action_type="prestige",
    )[0]
    assert action["jc_delta"] == 650
    assert '"gross_jc": 1000' in action["detail"]
    assert '"reward_multiplier": 0.65' in action["detail"]


def test_abandon_preview_and_refund_use_scaled_positive_reward(
    repo_db_path,
    player_repository,
    guild_id: int,
) -> None:
    player_id = 81006
    player_repository.add(
        discord_id=player_id,
        discord_username="abandon_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    service.dig_repo.update_tunnel(player_id, guild_id, depth=100, max_depth=100)
    balance_before = player_repository.get_balance(player_id, guild_id)

    preview = service.preview_abandon(player_id, guild_id)
    result = service.abandon_tunnel(player_id, guild_id)

    assert preview["refund"] == 7
    assert preview["gross_refund"] == 10
    assert result["refund"] == 7
    assert player_repository.get_balance(player_id, guild_id) == balance_before + 7
    action = service.dig_repo.get_recent_actions(
        player_id,
        guild_id,
        limit=1,
        action_type="abandon",
    )[0]
    assert action["jc_delta"] == 7
    assert '"gross_jc": 10' in action["detail"]


def test_pinnacle_reward_scales_base_but_not_wager_profit(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    player_id = 81007
    player_repository.add(
        discord_id=player_id,
        discord_username="pinnacle_reward_policy_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(player_id, guild_id)
    balance_before = player_repository.get_balance(player_id, guild_id)
    boss_progress = {
        str(PINNACLE_DEPTH): {
            "boss_id": "forgotten_king",
            "status": "phase2_defeated",
        },
        "pinnacle:3": {"boss_id": "forgotten_king", "status": "active"},
    }
    service.dig_repo.update_tunnel(
        player_id,
        guild_id,
        depth=PINNACLE_DEPTH,
        max_depth=PINNACLE_DEPTH,
        boss_progress=json.dumps(boss_progress),
        pinnacle_phase=3,
    )
    tunnel = dict(service.dig_repo.get_tunnel(player_id, guild_id))
    monkeypatch.setattr(
        service,
        "_roll_pinnacle_relic",
        lambda *_args: {
            "artifact_id": "mole_claws",
            "name": "Mole Claws",
            "rarity": "Uncommon",
        },
    )

    result = service._finalize_pinnacle_outcome(
        discord_id=player_id,
        guild_id=guild_id,
        tunnel=tunnel,
        pinnacle_id="forgotten_king",
        pinnacle=SimpleNamespace(name="Forgotten King"),
        phase_def=SimpleNamespace(title="Forgotten King"),
        phase_idx=3,
        phase_key="pinnacle:3",
        boss_progress=boss_progress,
        won=True,
        boss_hp=0,
        boss_hp_max=10,
        risk_tier="cautious",
        wager=200,
        win_chance=0.5,
        attempts=1,
        round_log=[],
        gear_broken_names=[],
        prestige_level=0,
        depth=PINNACLE_DEPTH,
        now=1_000_000,
    )

    expected = scale_positive_dig_jc(500) + result["wager_payout"]
    assert result["gross_payout"] == 500 + result["wager_payout"]
    assert result["payout"] == expected
    assert player_repository.get_balance(player_id, guild_id) == balance_before + expected
    action = service.dig_repo.get_recent_actions(
        player_id,
        guild_id,
        limit=1,
        action_type="pinnacle_fight",
    )[0]
    detail = json.loads(action["detail"])
    assert detail["gross_jc"] == 500
    assert detail["gross_payout"] == 500 + result["wager_payout"]
    assert detail["scaled_base_jc"] == scale_positive_dig_jc(500)
    assert detail["wager_payout"] == result["wager_payout"]
    assert detail["reward_multiplier"] == 0.65


def test_help_rewards_scale_minted_bonuses_for_both_players(
    repo_db_path,
    player_repository,
    guild_id: int,
    monkeypatch,
) -> None:
    helper_id = 81008
    target_id = 81009
    for player_id in (helper_id, target_id):
        player_repository.add(
            discord_id=player_id,
            discord_username=f"help_reward_policy_{player_id}",
            guild_id=guild_id,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repository.update_balance(player_id, guild_id, 100)
    service = DigService(DigRepository(repo_db_path), player_repository)
    service.dig(helper_id, guild_id)
    service.dig(target_id, guild_id)
    service.dig_repo.update_tunnel(helper_id, guild_id, last_dig_at=0)
    helper_before = player_repository.get_balance(helper_id, guild_id)
    target_before = player_repository.get_balance(target_id, guild_id)
    monkeypatch.setattr(
        service,
        "_has_relic",
        lambda player_id, _guild_id, relic_id: (
            player_id == helper_id and relic_id == "mentors_lantern"
        ),
    )

    result = service.help_tunnel(helper_id, target_id, guild_id)

    assert result["mentor_helper_bonus"] == 7
    assert result["mentor_target_bonus"] == 7
    assert player_repository.get_balance(helper_id, guild_id) == helper_before + 7
    assert player_repository.get_balance(target_id, guild_id) == target_before + 7
