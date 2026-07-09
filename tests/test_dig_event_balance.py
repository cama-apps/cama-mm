"""Catalog-wide balance invariants for /dig events after the threat re-tune.

Every two-branch event must be a genuine decision: the risky branch out-rewards
the safe sure thing (the incentive to gamble), and it is genuinely risky — it
can fail, or its outcome carries a real threat (cave-in, streak loss, curse, or
JC/block loss). Guards against an event regressing into a safe-dominates trap
or a no-downside freebie.

The expected-value calibration itself lives in ``scripts/dig_event_ev_audit.py``
(run manually); these tests assert the structural invariants that must always
hold regardless of fine tuning.
"""
from __future__ import annotations

import datetime
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_data.aliases import EVENT_POOL
from services.dig_service import DigService
from utils.economy_scaling import scale_minigame_jc_delta


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, discord_id: int, balance: int) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"User{discord_id}",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(discord_id, 12345, balance)
    # Stamp last_match_date so the splash victim pools pick them up.
    with player_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE players SET last_match_date = ? WHERE discord_id = ? AND guild_id = ?",
            (datetime.datetime.now(datetime.UTC).isoformat(), discord_id, 12345),
        )


def _seed_tunnel(dig_service, dig_repo, player_repository, depth: int = 30) -> None:
    _register(player_repository, 10001, balance=10000)
    random.seed(0)
    dig_service.dig(10001, 12345)
    dig_repo.update_tunnel(10001, 12345, depth=depth, luminosity=100)


def _two_branch_events():
    """Yield events that present a real safe-vs-risky choice (skip boon events)."""
    for e in EVENT_POOL:
        if e.get("complexity") == "boon" or e.get("boon_options"):
            continue
        safe = e.get("safe_option")
        risky = e.get("risky_option")
        if not safe or not risky:
            continue
        if not safe.get("success") or not risky.get("success"):
            continue
        yield e


def _carries_threat(outcome: dict | None) -> bool:
    """True if an outcome inflicts a real downside on the digger."""
    if not outcome:
        return False
    return bool(
        outcome.get("cave_in")
        or outcome.get("streak_loss", 0) > 0
        or outcome.get("curse") is not None
        or outcome.get("jc", 0) < 0
        or outcome.get("advance", 0) < 0
    )


def test_catalog_has_two_branch_events():
    """Sanity: the catalog actually yields a healthy set of two-branch events."""
    assert len(list(_two_branch_events())) >= 150


def test_every_risky_branch_outrewards_safe():
    """Risky success must pay strictly more JC than the safe sure thing —
    otherwise safe dominates and the fork is not a real decision."""
    offenders = [
        f"{e['id']}: risky success jc={e['risky_option']['success'].get('jc', 0)} "
        f"<= safe jc={e['safe_option']['success'].get('jc', 0)}"
        for e in _two_branch_events()
        if e["risky_option"]["success"].get("jc", 0)
        <= e["safe_option"]["success"].get("jc", 0)
    ]
    assert not offenders, "risky reward must beat safe:\n" + "\n".join(offenders)


def test_every_risky_branch_is_genuinely_risky():
    """A risky branch must genuinely be risky: it can fail (success_chance < 1),
    or its success/failure outcome carries a real threat. No no-downside picks."""
    offenders = []
    for e in _two_branch_events():
        risky = e["risky_option"]
        can_fail = risky.get("success_chance", 1.0) < 1.0
        threatening = _carries_threat(risky.get("failure")) or _carries_threat(
            risky.get("success")
        )
        if not (can_fail or threatening):
            offenders.append(f"{e['id']}: risky branch has no downside")
    assert not offenders, "risky branches with no downside:\n" + "\n".join(offenders)


def test_high_p2_event_rewards_are_modestly_trimmed(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    """The trimmed high-end P2 payouts must actually reach the digger's
    wallet — drive ``resolve_event`` on each event/branch and assert the
    credited ``jc_delta`` equals the trimmed catalog value. Jitter is pinned
    neutral (``random.uniform`` -> 1.0) and the roll forced to succeed, so the
    test fails if the trim ever drifts in either direction OR if the payout
    pipeline stops crediting the authored amount."""
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    _seed_tunnel(dig_service, dig_repo, player_repository)
    # Pin the ±50% JC jitter to its neutral point so the credited delta is the
    # exact authored value, and force every roll to succeed.
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    expected = {
        "necro_s3": {"risky": scale_minigame_jc_delta(13), "desperate": scale_minigame_jc_delta(20)},
        "necro_s4": {"risky": scale_minigame_jc_delta(13), "desperate": scale_minigame_jc_delta(20)},
        "necro_s5": {"risky": scale_minigame_jc_delta(17), "desperate": scale_minigame_jc_delta(27)},
    }
    for event_id, payouts in expected.items():
        for branch, want in payouts.items():
            dig_repo.update_tunnel(10001, 12345, depth=30)
            balance_before = player_repository.get_balance(10001, 12345)
            r = dig_service.resolve_event(10001, 12345, event_id, branch)
            assert r["success"] and r["succeeded"], (event_id, branch, r)
            assert r["jc_delta"] == want, (
                f"{event_id} {branch}: credited {r['jc_delta']} JC, "
                f"expected trimmed payout {want}"
            )
            balance_after = player_repository.get_balance(10001, 12345)
            assert balance_after - balance_before == want, (
                f"{event_id} {branch}: balance moved by "
                f"{balance_after - balance_before}, expected {want}"
            )


def test_social_burn_events_are_real_global_sinks(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    """The global deflation events must actually remove more JC from the
    economy than they mint. Drive ``resolve_event`` end-to-end on a
    representative burn event with funded victims, then assert that the JC
    burned off other players' real balances strictly exceeds the actor's
    credited payout — proving the event is net-deflationary in behavior, not
    just in its catalog literals."""
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    # The digger plus three rich victims (richest_n picks the top balances).
    _register(player_repository, 10001, balance=10000)
    for vid in (10002, 10003, 10004):
        _register(player_repository, vid, balance=1000)
    random.seed(0)
    dig_service.dig(10001, 12345)
    dig_repo.update_tunnel(10001, 12345, depth=120, luminosity=100)
    # Pin jitter neutral and force the risky success that triggers the burn.
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    victims = (10002, 10003, 10004)
    before = {v: player_repository.get_balance(v, 12345) for v in victims}
    r = dig_service.resolve_event(10001, 12345, "hungering_dark", "risky")
    assert r["success"] and r["succeeded"], r

    after = {v: player_repository.get_balance(v, 12345) for v in victims}
    burned_from_victims = sum(before[v] - after[v] for v in victims)
    actor_payout = r["jc_delta"]

    splash = r["splash"]
    assert splash is not None and splash["mode"] == "burn"
    # The reported burn matches the real balance movement (no phantom burn).
    assert splash["total_burned"] == burned_from_victims
    # Net deflation: more JC destroyed than the actor was credited.
    assert burned_from_victims > actor_payout > 0, (
        f"hungering_dark minted {actor_payout} JC but only burned "
        f"{burned_from_victims} from victims — not a real sink"
    )
