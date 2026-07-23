import json
import random
import sqlite3
import time

import pytest

from repositories.dig_repository import DigRepository, TunnelStateConflictError
from services.dig_constants import BOSS_BOUNDARIES, PINNACLE_DEPTH
from services.dig_data.routes import (
    LAYER_ROUTE_POOLS,
    ROUTE_BY_ID,
    UNIVERSAL_ROUTES,
    generate_route_offer,
)
from services.dig_service import DigService


@pytest.fixture
def route_service(repo_db_path, player_repository):
    return DigService(DigRepository(repo_db_path), player_repository)


POST_DIRT_LAYERS = (
    "Stone",
    "Crystal",
    "Magma",
    "Abyss",
    "Fungal Depths",
    "Frozen Core",
    "The Hollow",
)


def test_route_catalog_has_four_universal_and_three_per_post_dirt_layer():
    assert len(UNIVERSAL_ROUTES) == 4
    assert set(LAYER_ROUTE_POOLS) == set(POST_DIRT_LAYERS)
    assert all(len(LAYER_ROUTE_POOLS[layer]) == 3 for layer in POST_DIRT_LAYERS)
    assert len(ROUTE_BY_ID) == 25


def test_route_catalog_ids_are_unique_and_never_modify_jc_directly():
    routes = [*UNIVERSAL_ROUTES]
    for layer in POST_DIRT_LAYERS:
        routes.extend(LAYER_ROUTE_POOLS[layer])

    assert len({route.id for route in routes}) == len(routes)
    assert set(ROUTE_BY_ID) == {route.id for route in routes}
    assert all(
        not any("jc" in effect_key for effect_key in route.effects)
        for route in routes
    )


@pytest.mark.parametrize("layer", POST_DIRT_LAYERS)
def test_route_offer_contains_one_universal_and_two_routes_for_layer(layer):
    offered = generate_route_offer(layer, rng=random.Random(17))

    universal_ids = {route.id for route in UNIVERSAL_ROUTES}
    themed_ids = {route.id for route in LAYER_ROUTE_POOLS[layer]}
    assert len(offered) == 3
    assert len(set(offered)) == 3
    assert len(universal_ids.intersection(offered)) == 1
    assert len(themed_ids.intersection(offered)) == 2


def test_route_offer_excludes_previous_universal_route_when_possible():
    previous = UNIVERSAL_ROUTES[0].id

    for seed in range(30):
        offered = generate_route_offer(
            "Stone",
            previous_route_id=previous,
            rng=random.Random(seed),
        )
        assert previous not in offered


def test_route_offer_rejects_layer_without_a_route_pool():
    with pytest.raises(ValueError, match="No route pool for layer"):
        generate_route_offer("Dirt", rng=random.Random(1))


def test_route_state_migration_defaults_to_null_and_is_repository_updatable(repo_db_path):
    repo = DigRepository(repo_db_path)
    created = repo.create_tunnel(10001, 12345, "Route Test")

    assert created["route_state"] is None

    pending = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }, sort_keys=True)
    repo.update_tunnel(10001, 12345, route_state=pending)

    assert repo.get_tunnel(10001, 12345)["route_state"] == pending


def test_route_state_transition_is_guarded_and_audited_atomically(repo_db_path):
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(10001, 12345, "Route Test")
    pending = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }, sort_keys=True)
    selected = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": "old_supports",
    }, sort_keys=True)
    repo.update_tunnel(10001, 12345, route_state=pending)

    repo.atomic_tunnel_balance_update(
        10001,
        12345,
        tunnel_updates={"route_state": selected},
        require_tunnel_state={"route_state": pending},
        log_detail={"route_id": "old_supports", "layer": "Stone"},
        log_action_type="route_choice",
    )

    with pytest.raises(TunnelStateConflictError):
        repo.atomic_tunnel_balance_update(
            10001,
            12345,
            tunnel_updates={"route_state": pending},
            require_tunnel_state={"route_state": pending},
            log_detail={"route_id": "shored_passage", "layer": "Stone"},
            log_action_type="route_choice",
        )

    assert repo.get_tunnel(10001, 12345)["route_state"] == selected
    with sqlite3.connect(repo_db_path) as conn:
        rows = conn.execute(
            "SELECT action_type, detail FROM dig_actions WHERE actor_id = ?",
            (10001,),
        ).fetchall()
    assert rows == [
        ("route_choice", json.dumps({"route_id": "old_supports", "layer": "Stone"}))
    ]


def test_full_boss_victory_claim_prevents_duplicate_payout_and_offer_overwrite(
    repo_db_path,
    player_repository,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(10001, 12345, "Route Test")
    active_progress = json.dumps({
        "25": {"boss_id": "grothak", "status": "active"},
    })
    defeated_progress = json.dumps({
        "25": {"boss_id": "grothak", "status": "defeated"},
    })
    winning_offer = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }, sort_keys=True)
    losing_offer = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["measured_descent", "old_supports", "fossil_seam"],
        "selected": None,
    }, sort_keys=True)
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        boss_progress=active_progress,
    )
    guard = {"depth": 24, "boss_progress": active_progress}
    common = {
        "discord_id": 10001,
        "guild_id": 12345,
        "jc_delta": 11,
        "boss_echo_boss_id": "grothak",
        "boss_echo_depth": 25,
        "boss_echo_window_seconds": 24 * 3600,
        "log_detail": {"boundary": 25, "won": True, "jc_delta": 11},
        "require_tunnel_state": guard,
    }
    balance_before = player_repository.get_balance(10001, 12345)

    repo.atomic_boss_full_victory(
        **common,
        tunnel_updates={
            "depth": 25,
            "boss_progress": defeated_progress,
            "route_state": winning_offer,
        },
    )
    with pytest.raises(TunnelStateConflictError):
        repo.atomic_boss_full_victory(
            **common,
            tunnel_updates={
                "depth": 25,
                "boss_progress": defeated_progress,
                "route_state": losing_offer,
            },
        )

    tunnel = repo.get_tunnel(10001, 12345)
    assert tunnel["route_state"] == winning_offer
    assert player_repository.get_balance(10001, 12345) == balance_before + 11
    with sqlite3.connect(repo_db_path) as conn:
        boss_logs = conn.execute(
            "SELECT COUNT(*) FROM dig_actions "
            "WHERE actor_id = ? AND action_type = 'boss_fight'",
            (10001,),
        ).fetchone()[0]
    assert boss_logs == 1


@pytest.mark.parametrize("first_outcome", ["victory", "loss"])
def test_boss_outcome_claim_allows_only_the_first_win_or_loss(
    repo_db_path,
    player_repository,
    first_outcome,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(10001, 12345, "Route Test")
    active_progress = json.dumps({
        "25": {"boss_id": "grothak", "status": "active"},
    })
    defeated_progress = json.dumps({
        "25": {"boss_id": "grothak", "status": "defeated"},
    })
    damaged_progress = json.dumps({
        "25": {
            "boss_id": "grothak",
            "status": "active",
            "hp_remaining": 3,
            "hp_max": 5,
        },
    })
    route_offer = json.dumps({
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }, sort_keys=True)
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        boss_progress=active_progress,
    )
    guard = {
        "depth": 24,
        "boss_progress": active_progress,
        "stinger_curse": None,
    }

    def commit_victory():
        repo.atomic_boss_full_victory(
            discord_id=10001,
            guild_id=12345,
            jc_delta=11,
            tunnel_updates={
                "depth": 25,
                "boss_progress": defeated_progress,
                "route_state": route_offer,
            },
            require_tunnel_state=guard,
            boss_echo_boss_id="grothak",
            boss_echo_depth=25,
            boss_echo_window_seconds=24 * 3600,
            log_detail={"boundary": 25, "won": True, "jc_delta": 11},
        )

    def commit_loss():
        repo.atomic_tunnel_balance_update(
            10001,
            12345,
            tunnel_updates={
                "depth": 20,
                "boss_progress": damaged_progress,
            },
            require_tunnel_state=guard,
            log_detail={"boundary": 25, "won": False},
            log_action_type="boss_fight",
        )

    first = commit_victory if first_outcome == "victory" else commit_loss
    second = commit_loss if first_outcome == "victory" else commit_victory
    first()
    with pytest.raises(TunnelStateConflictError):
        second()

    tunnel = repo.get_tunnel(10001, 12345)
    if first_outcome == "victory":
        assert tunnel["depth"] == 25
        assert tunnel["boss_progress"] == defeated_progress
        assert tunnel["route_state"] == route_offer
    else:
        assert tunnel["depth"] == 20
        assert tunnel["boss_progress"] == damaged_progress
        assert tunnel["route_state"] is None


def test_service_excludes_previous_offered_universal_when_themed_route_was_selected(
    route_service,
):
    previous = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": "old_supports",
    }

    state = route_service._build_route_offer_state(
        {"route_state": json.dumps(previous)},
        50,
        rng=random.Random(42),
    )

    assert state["layer"] == "Crystal"
    assert state["start_depth"] == 50
    assert state["end_depth"] == 75
    assert state["selected"] is None
    assert "shored_passage" not in state["offered"]
    offered = [ROUTE_BY_ID[route_id] for route_id in state["offered"]]
    assert sum(route.layer is None for route in offered) == 1
    assert sum(route.layer == "Crystal" for route in offered) == 2


def test_service_does_not_build_routes_for_non_boss_or_pinnacle(route_service):
    assert route_service._build_route_offer_state({}, 24) is None
    assert route_service._build_route_offer_state({}, 350) is None


def test_concurrent_first_encounters_keep_the_first_locked_boss(
    route_service,
    monkeypatch,
):
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        boss_progress=json.dumps({"25": "active"}),
    )
    first_view = dict(repo.get_tunnel(10001, 12345))
    second_view = dict(repo.get_tunnel(10001, 12345))
    choices = iter((0, 1))

    def choose_different_bosses(_rng, pool):
        return pool[next(choices)]

    monkeypatch.setattr(random.Random, "choice", choose_different_bosses)

    first_boss = route_service._ensure_boss_locked(
        10001,
        12345,
        first_view,
        25,
    )
    second_boss = route_service._ensure_boss_locked(
        10001,
        12345,
        second_view,
        25,
    )

    persisted = json.loads(repo.get_tunnel(10001, 12345)["boss_progress"])
    assert second_boss.boss_id == first_boss.boss_id
    assert persisted["25"]["boss_id"] == first_boss.boss_id


def test_service_selects_only_offered_route_once_and_returns_active_effects(route_service):
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    pending = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }
    repo.update_tunnel(10001, 12345, route_state=json.dumps(pending, sort_keys=True))

    invalid = route_service.choose_route(10001, 12345, "lava_tube")
    assert invalid["success"] is False
    assert "offered" in invalid["error"].lower()

    chosen = route_service.choose_route(10001, 12345, "old_supports")
    assert chosen["success"] is True
    assert chosen["route"]["id"] == "old_supports"
    assert chosen["already_selected"] is False

    tunnel = repo.get_tunnel(10001, 12345)
    assert json.loads(tunnel["route_state"])["selected"] == "old_supports"
    assert route_service._get_route_effects(tunnel) == ROUTE_BY_ID["old_supports"].effects

    repeated = route_service.choose_route(10001, 12345, "old_supports")
    assert repeated["success"] is True
    assert repeated["already_selected"] is True
    actions = repo.get_recent_actions(10001, 12345, action_type="route_choice", hours=1)
    assert len(actions) == 1


def test_route_choice_status_distinguishes_pending_and_active(route_service):
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    pending = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }
    repo.update_tunnel(10001, 12345, route_state=json.dumps(pending, sort_keys=True))

    status = route_service.get_route_status(10001, 12345)
    assert status["success"] is True
    assert status["choice_required"] is True
    assert [route["id"] for route in status["offered_routes"]] == pending["offered"]
    assert status["active_route"] is None

    route_service.choose_route(10001, 12345, "fossil_seam")
    status = route_service.get_route_status(10001, 12345)
    assert status["choice_required"] is False
    assert status["active_route"]["id"] == "fossil_seam"
    tunnel_info = route_service.get_tunnel_info(10001, 12345)
    assert tunnel_info["route"]["active_route"]["id"] == "fossil_seam"


@pytest.mark.parametrize("entrypoint", ["dig", "_compute_preconditions"])
def test_pending_route_blocks_dig_before_cost_or_consumable_use(
    route_service,
    player_repository,
    entrypoint,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    pending = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=25,
        total_digs=5,
        last_dig_at=2_000_000_000,
        route_state=json.dumps(pending, sort_keys=True),
    )
    item_id = repo.add_inventory_item(10001, 12345, "dynamite")
    repo.queue_item(item_id)
    balance_before = player_repository.get_balance(10001, 12345)

    if entrypoint == "dig":
        result = route_service.dig(10001, 12345, paid=True)
    else:
        result, preconditions = route_service._compute_preconditions(
            10001,
            12345,
            paid=True,
        )
        assert preconditions is None

    assert result["route_choice_required"] is True
    assert player_repository.get_balance(10001, 12345) == balance_before
    assert [item["id"] for item in repo.get_queued_items(10001, 12345)] == [item_id]


def test_abandon_clears_route_state(route_service, player_repository):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    active = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": "old_supports",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=20,
        route_state=json.dumps(active, sort_keys=True),
    )

    result = route_service.abandon_tunnel(10001, 12345)

    assert result["success"] is True
    assert repo.get_tunnel(10001, 12345)["route_state"] is None


def test_prestige_clears_route_state(route_service, player_repository):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    active = {
        "layer": "The Hollow",
        "start_depth": 275,
        "end_depth": PINNACLE_DEPTH,
        "offered": ["silent_road", "void_harvest", "echoing_gallery"],
        "selected": "silent_road",
    }
    boss_progress = {str(depth): "defeated" for depth in BOSS_BOUNDARIES}
    boss_progress[str(PINNACLE_DEPTH)] = "defeated"
    repo.update_tunnel(
        10001,
        12345,
        depth=PINNACLE_DEPTH,
        boss_progress=json.dumps(boss_progress),
        route_state=json.dumps(active, sort_keys=True),
    )

    result = route_service.prestige(10001, 12345, "advance_boost")

    assert result["success"] is True
    assert repo.get_tunnel(10001, 12345)["route_state"] is None


def test_route_loss_modifiers_add_before_applying_the_route_cap(route_service):
    assert route_service._apply_route_cave_in_loss(
        8,
        {"cave_in_loss_bonus": 3, "cave_in_loss_cap": 7},
    ) == 7
    assert route_service._apply_route_cave_in_loss(
        8,
        {"cave_in_loss_bonus": 2},
    ) == 10


@pytest.mark.parametrize("engine", ["live", "deterministic", "applied"])
def test_route_loss_bonus_does_not_exceed_weather_cap(
    route_service,
    player_repository,
    monkeypatch,
    engine,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["fractured_shortcut", "old_supports", "fossil_seam"],
        "selected": "fractured_shortcut",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=20,
        total_digs=1,
        last_dig_at=0,
        boss_progress=json.dumps({"25": "defeated"}),
        route_state=json.dumps(state, sort_keys=True),
    )
    monkeypatch.setattr(
        route_service,
        "_get_weather_effects",
        lambda *_: {"cave_in_loss_cap": 3},
    )
    monkeypatch.setattr(random, "random", lambda: 0.0)
    monkeypatch.setattr(random, "randint", lambda low, high: high)

    if engine == "live":
        result = route_service.dig(10001, 12345)
    else:
        terminal, preconditions = route_service._compute_preconditions(10001, 12345)
        assert terminal is None
        if engine == "deterministic":
            result = route_service._execute_deterministic_outcome(preconditions)
        else:
            result = route_service.apply_dig_outcome(
                preconditions,
                {
                    "cave_in": True,
                    "cave_in_block_loss": 10,
                    "cave_in_type": "stun",
                },
            )

    assert result["success"] is True
    assert result["cave_in"] is True
    assert result["cave_in_detail"]["block_loss"] == 3


@pytest.mark.parametrize("engine", ["live", "deterministic"])
def test_route_loss_keeps_reinforcement_after_steady_hands(
    route_service,
    player_repository,
    monkeypatch,
    engine,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["fractured_shortcut", "old_supports", "fossil_seam"],
        "selected": "fractured_shortcut",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=40,
        total_digs=1,
        last_dig_at=0,
        boss_progress=json.dumps({"25": "defeated"}),
        prestige_perks=json.dumps(["steady_hands"]),
        reinforced_until=1_000_000 + 48 * 3600,
        route_state=json.dumps(state, sort_keys=True),
    )
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(route_service, "_get_weather_effects", lambda *_: {})
    monkeypatch.setattr(random, "random", lambda: 0.0)
    monkeypatch.setattr(random, "randint", lambda low, high: high)

    if engine == "live":
        result = route_service.dig(10001, 12345)
    else:
        terminal, preconditions = route_service._compute_preconditions(10001, 12345)
        assert terminal is None
        result = route_service._execute_deterministic_outcome(preconditions)

    assert result["success"] is True
    assert result["cave_in_detail"]["block_loss"] == 8


@pytest.mark.parametrize("engine", ["live", "deterministic"])
def test_catastrophic_cave_in_respects_route_loss_cap(
    route_service,
    player_repository,
    monkeypatch,
    engine,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": "old_supports",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=148,
        total_digs=1,
        last_dig_at=0,
        boss_progress=json.dumps({
            str(boundary): "defeated"
            for boundary in BOSS_BOUNDARIES
            if boundary < 148
        }),
        route_state=json.dumps(state, sort_keys=True),
    )
    monkeypatch.setattr(route_service, "_get_weather_effects", lambda *_: {})
    monkeypatch.setattr(random, "random", lambda: 0.0)
    monkeypatch.setattr(random, "randint", lambda low, high: high)
    monkeypatch.setattr(
        "services.dig_service.roll_catastrophic_cave_in",
        lambda _band: True,
    )
    monkeypatch.setattr(
        "services.dig.dig_core_mixin.roll_catastrophic_cave_in",
        lambda _band: True,
    )

    if engine == "live":
        result = route_service.dig(10001, 12345)
    else:
        terminal, preconditions = route_service._compute_preconditions(10001, 12345)
        assert terminal is None
        result = route_service._execute_deterministic_outcome(preconditions)

    assert result["success"] is True
    assert result["cave_in_detail"]["type"] == "catastrophic"
    assert result["cave_in_detail"]["block_loss"] == 6
    assert result["depth_after"] == 142


@pytest.mark.parametrize(
    ("route_id", "layer", "depth", "expected"),
    [
        (
            "old_supports",
            "Stone",
            30,
            {
                "cave_in_chance": 0.045,
                "advance_min": 1,
                "advance_max": 2,
                "luminosity": 100,
                "event_chance": 0.22,
                "artifact_multiplier": 1.0,
            },
        ),
        (
            "sporelit_garden",
            "Fungal Depths",
            160,
            {
                "cave_in_chance": 0.405,
                "advance_min": 1,
                "advance_max": 2,
                "luminosity": 99,
                "event_chance": 0.285,
                "artifact_multiplier": 1.25,
            },
        ),
    ],
)
def test_preconditions_include_active_route_modifiers_in_both_dig_engines(
    route_service,
    player_repository,
    monkeypatch,
    route_id,
    layer,
    depth,
    expected,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": layer,
        "start_depth": 25,
        "end_depth": PINNACLE_DEPTH,
        "offered": [route_id, "shored_passage", "fractured_shortcut"],
        "selected": route_id,
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=depth,
        total_digs=1,
        last_dig_at=0,
        luminosity=100,
        boss_progress=json.dumps({
            str(boundary): "defeated"
            for boundary in BOSS_BOUNDARIES
            if boundary < depth
        }),
        route_state=json.dumps(state, sort_keys=True),
    )
    monkeypatch.setattr(route_service, "_get_weather_effects", lambda *_: {})

    terminal, preconditions = route_service._compute_preconditions(10001, 12345)

    assert terminal is None
    assert preconditions is not None
    assert preconditions["cave_in_chance"] == pytest.approx(expected["cave_in_chance"])
    assert preconditions["advance_min"] == expected["advance_min"]
    assert preconditions["advance_max"] == expected["advance_max"]
    assert preconditions["luminosity"] == expected["luminosity"]
    assert preconditions["event_chance"] == pytest.approx(expected["event_chance"])
    assert preconditions["route_effects"].get("artifact_multiplier", 1.0) == expected[
        "artifact_multiplier"
    ]


def test_full_regular_boss_victory_persists_and_returns_route_offer_atomically(
    route_service,
    player_repository,
    monkeypatch,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        total_digs=1,
        boss_progress=json.dumps({
            "25": {"boss_id": "grothak", "status": "active"},
        }),
    )
    monkeypatch.setattr(
        route_service,
        "_scale_boss_stats",
        lambda stats, *args, **kwargs: {
            **stats,
            "boss_hp": 1,
            "boss_hit": 0.0,
            "boss_dmg": 1,
        },
    )
    monkeypatch.setattr(random, "random", lambda: 0.01)
    monkeypatch.setattr(
        "domain.models.boss_mechanics.get_mechanic",
        lambda mechanic_id: None,
    )

    result = route_service.start_boss_duel(10001, 12345, "cautious", wager=0)

    assert result["success"] is True
    assert result["won"] is True
    assert result["route_choice"]["choice_required"] is True
    persisted = json.loads(repo.get_tunnel(10001, 12345)["route_state"])
    assert persisted["layer"] == "Stone"
    assert persisted["selected"] is None
    assert [route["id"] for route in result["route_choice"]["offered_routes"]] == persisted[
        "offered"
    ]


def test_boss_victory_conflict_returns_the_persisted_route(
    route_service,
    player_repository,
    monkeypatch,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        total_digs=1,
        stinger_curse=json.dumps({"drain_next_reward": True}),
        boss_progress=json.dumps({
            "25": {"boss_id": "grothak", "status": "active"},
        }),
    )
    gear_id = repo.add_gear(10001, 12345, "armor", 1)
    repo.equip_gear(gear_id, 10001, 12345, "armor")
    durability_before = repo.get_gear_by_id(gear_id)["durability"]
    persisted_offer = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }

    def resolve_elsewhere(**_kwargs):
        assert _kwargs["tunnel_updates"]["stinger_curse"] is None
        assert "stinger_curse" in _kwargs["require_tunnel_state"]
        repo.update_tunnel(
            10001,
            12345,
            depth=25,
            boss_progress=json.dumps({
                "25": {"boss_id": "grothak", "status": "defeated"},
            }),
            route_state=json.dumps(persisted_offer, sort_keys=True),
        )
        raise TunnelStateConflictError("already resolved")

    monkeypatch.setattr(repo, "atomic_boss_full_victory", resolve_elsewhere)
    monkeypatch.setattr(
        route_service,
        "_scale_boss_stats",
        lambda stats, *args, **kwargs: {
            **stats,
            "boss_hp": 1,
            "boss_hit": 0.0,
            "boss_dmg": 1,
        },
    )
    monkeypatch.setattr(random, "random", lambda: 0.01)
    monkeypatch.setattr(
        "domain.models.boss_mechanics.get_mechanic",
        lambda mechanic_id: None,
    )
    balance_before = player_repository.get_balance(10001, 12345)

    result = route_service.start_boss_duel(10001, 12345, "cautious", wager=0)

    assert result["success"] is False
    assert result["route_choice"]["choice_required"] is True
    assert [
        route["id"] for route in result["route_choice"]["offered_routes"]
    ] == persisted_offer["offered"]
    assert player_repository.get_balance(10001, 12345) == balance_before
    assert repo.get_gear_by_id(gear_id)["durability"] == durability_before
    assert json.loads(repo.get_tunnel(10001, 12345)["stinger_curse"]) == {
        "drain_next_reward": True,
    }


def test_boss_loss_conflict_cannot_overwrite_victory_or_damage_gear(
    route_service,
    player_repository,
    monkeypatch,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    repo.update_tunnel(
        10001,
        12345,
        depth=24,
        total_digs=1,
        boss_progress=json.dumps({
            "25": {"boss_id": "grothak", "status": "active"},
        }),
    )
    gear_id = repo.add_gear(10001, 12345, "armor", 1)
    repo.equip_gear(gear_id, 10001, 12345, "armor")
    durability_before = repo.get_gear_by_id(gear_id)["durability"]
    persisted_offer = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }

    def victory_commits_before_loss(_discord_id, _guild_id, **kwargs):
        assert kwargs["require_tunnel_state"]["depth"] == 24
        repo.update_tunnel(
            10001,
            12345,
            depth=25,
            boss_progress=json.dumps({
                "25": {"boss_id": "grothak", "status": "defeated"},
            }),
            route_state=json.dumps(persisted_offer, sort_keys=True),
        )
        raise TunnelStateConflictError("victory committed first")

    monkeypatch.setattr(
        repo,
        "atomic_tunnel_balance_update",
        victory_commits_before_loss,
    )
    monkeypatch.setattr(
        route_service,
        "_scale_boss_stats",
        lambda stats, *args, **kwargs: {
            **stats,
            "boss_hp": 50,
            "boss_hit": 1.0,
            "boss_dmg": 100,
        },
    )
    monkeypatch.setattr(random, "random", lambda: 0.99)
    monkeypatch.setattr(
        "domain.models.boss_mechanics.get_mechanic",
        lambda mechanic_id: None,
    )

    result = route_service.start_boss_duel(10001, 12345, "cautious", wager=0)

    assert result["success"] is False
    assert result["route_choice"]["choice_required"] is True
    tunnel = repo.get_tunnel(10001, 12345)
    assert tunnel["depth"] == 25
    assert json.loads(tunnel["boss_progress"])["25"]["status"] == "defeated"
    assert repo.get_gear_by_id(gear_id)["durability"] == durability_before


def test_boss_conflict_copy_mentions_route_only_when_one_is_pending(route_service):
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    repo.update_tunnel(
        10001,
        12345,
        depth=20,
        boss_progress=json.dumps({
            "25": {"boss_id": "grothak", "status": "active"},
        }),
    )

    without_route = route_service._boss_resolution_conflict_result(10001, 12345)

    assert without_route["success"] is False
    assert "saved route" not in without_route["error"].lower()

    pending_offer = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["shored_passage", "old_supports", "fossil_seam"],
        "selected": None,
    }
    repo.update_tunnel(
        10001,
        12345,
        route_state=json.dumps(pending_offer, sort_keys=True),
    )

    with_route = route_service._boss_resolution_conflict_result(10001, 12345)

    assert "saved route" in with_route["error"].lower()


def test_thick_skin_still_forces_zero_cave_in_chance_with_risky_route(
    route_service,
    player_repository,
    monkeypatch,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": "The Hollow",
        "start_depth": 275,
        "end_depth": PINNACLE_DEPTH,
        "offered": ["maws_shortcut", "silent_road", "echoing_gallery"],
        "selected": "maws_shortcut",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=280,
        total_digs=1,
        last_dig_at=0,
        boss_progress=json.dumps({str(boundary): "defeated" for boundary in BOSS_BOUNDARIES}),
        mutations=json.dumps([{"id": "thick_skin"}]),
        route_state=json.dumps(state, sort_keys=True),
    )
    repo.log_action(
        guild_id=12345,
        actor_id=10001,
        target_id=20002,
        action_type="sabotage",
    )
    monkeypatch.setattr(route_service, "_get_weather_effects", lambda *_: {})

    terminal, preconditions = route_service._compute_preconditions(10001, 12345)

    assert terminal is None
    assert preconditions["thick_skin_saved"] is True
    assert preconditions["cave_in_chance"] == 0.0


@pytest.mark.parametrize("engine", ["live", "deterministic"])
def test_live_and_deterministic_resolvers_apply_route_artifact_multiplier(
    route_service,
    player_repository,
    monkeypatch,
    engine,
):
    player_repository.add(
        discord_id=10001,
        discord_username="RoutePlayer",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = route_service.dig_repo
    repo.create_tunnel(10001, 12345, "Route Test")
    state = {
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered": ["fossil_seam", "old_supports", "shored_passage"],
        "selected": "fossil_seam",
    }
    repo.update_tunnel(
        10001,
        12345,
        depth=30,
        total_digs=1,
        last_dig_at=0,
        boss_progress=json.dumps({"25": "defeated"}),
        route_state=json.dumps(state, sort_keys=True),
    )
    monkeypatch.setattr(route_service, "_get_weather_effects", lambda *_: {})
    monkeypatch.setattr(random, "random", lambda: 0.99)
    monkeypatch.setattr(random, "randint", lambda low, high: high)
    artifact_multipliers = []

    def capture_artifact(*args, **kwargs):
        artifact_multipliers.append(kwargs["extra_rate_mod"])
        return None

    monkeypatch.setattr(route_service, "roll_artifact", capture_artifact)

    if engine == "live":
        result = route_service.dig(10001, 12345)
    else:
        terminal, preconditions = route_service._compute_preconditions(10001, 12345)
        assert terminal is None
        result = route_service._execute_deterministic_outcome(preconditions)

    assert result["success"] is True
    assert artifact_multipliers == [1.75]
