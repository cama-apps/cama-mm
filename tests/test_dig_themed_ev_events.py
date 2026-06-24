"""Intent tests for the themed -EV /dig events (subtle GW1 / MTG flavor).

These four events were added to give the catalog deliberately negative-value
encounters. Two are *digger taxes* (Sealed Reliquary, Grenth's Tithe) whose
every branch loses coin in expectation — the low-variance branch is a
guaranteed *toll*, not a free gain, so a smart player still bleeds. Two are
*whale taxes* (Rhystic Tollgate, The Underworld Reclaims) that reach the
guild's three richest balances: one steals the spoils to the digger, one burns
them out of the economy.

The catalog-wide invariants in ``test_dig_event_balance.py`` already guard the
structural rules (risky out-rewards safe, risky carries a downside). The tests
here guard the *purpose*: if someone later "fixes" a trap's safe branch back to
a positive payout, or unwires the splash so the rich stop paying, these fail.
"""
from __future__ import annotations

import datetime
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_data.aliases import EVENT_POOL
from services.dig_data.balance import NEGATIVE_EVENT_JC_MULTIPLIER
from services.dig_service import DigService

TRAP_EVENT_IDS = ("sealed_reliquary", "grenths_tithe")
WHALE_EVENT_IDS = ("rhystic_tollgate", "underworld_reclaims")


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _event(event_id: str) -> dict:
    return next(e for e in EVENT_POOL if e["id"] == event_id)


def _effective_jc(jc: int) -> float:
    """Mirror the runtime tuning: a flat JC *loss* bites 1.3x harder; the ±50%
    jitter is mean-neutral so it drops out of an expected-value calc."""
    return jc * NEGATIVE_EVENT_JC_MULTIPLIER if jc < 0 else float(jc)


def _branch_ev(choice: dict) -> float:
    """Expected JC of an event branch, accounting for the loss multiplier."""
    chance = choice.get("success_chance", 1.0)
    ev = chance * _effective_jc(choice["success"]["jc"])
    failure = choice.get("failure")
    if failure is not None:
        ev += (1.0 - chance) * _effective_jc(failure["jc"])
    return ev


def _register(player_repo, discord_id: int, balance: int, guild_id: int = 12345) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"User{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(discord_id, guild_id, balance)
    with player_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE players SET last_match_date = ? WHERE discord_id = ? AND guild_id = ?",
            (datetime.datetime.now(datetime.UTC).isoformat(), discord_id, guild_id),
        )


def _seed_tunnel(dig_service, dig_repo, depth: int) -> None:
    """Create the digger's tunnel, then force it to the target depth."""
    random.seed(0)
    dig_service.dig(10001, 12345)
    dig_repo.update_tunnel(10001, 12345, depth=depth, luminosity=100)


# ---------------------------------------------------------------------------
# Digger taxes: genuinely -EV no matter which branch is taken
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("event_id", TRAP_EVENT_IDS)
def test_trap_safe_branch_is_a_guaranteed_loss(event_id):
    """The low-variance ("safe") branch must NOT be a free positive payout —
    that was the whole point. It is a guaranteed toll, so engaging the event
    costs coin even on the cautious play."""
    e = _event(event_id)
    safe = e["safe_option"]
    assert safe["success_chance"] == 1.0, f"{event_id} safe branch should be guaranteed"
    assert safe["failure"] is None, f"{event_id} safe branch should have no failure roll"
    assert safe["success"]["jc"] < 0, (
        f"{event_id} safe branch pays {safe['success']['jc']} JC; a non-negative "
        f"'safe' branch lets the digger escape the tax and defeats the event"
    )


@pytest.mark.parametrize("event_id", TRAP_EVENT_IDS)
def test_trap_is_negative_ev_on_every_branch(event_id):
    """Both branches must have negative expected JC, so there is no positive-EV
    escape: the rational play (pay the small toll) still loses, and the greedy
    gamble loses more. This is what makes the event a real coin sink."""
    e = _event(event_id)
    safe_ev = _branch_ev(e["safe_option"])
    risky_ev = _branch_ev(e["risky_option"])
    assert safe_ev < 0, f"{event_id} safe EV {safe_ev:.2f} is not a loss"
    assert risky_ev < 0, f"{event_id} risky EV {risky_ev:.2f} is not a loss"
    # Sanity: the best available choice is still a loss.
    assert max(safe_ev, risky_ev) < 0


def test_trap_risky_still_outrewards_safe_headline():
    """Structural invariant the catalog test also enforces, asserted here for
    the new events directly: the risky *headline* reward beats the safe one, so
    the toll/gamble fork is a real decision (greed is tempted, then punished)."""
    for event_id in TRAP_EVENT_IDS:
        e = _event(event_id)
        assert e["risky_option"]["success"]["jc"] > e["safe_option"]["success"]["jc"]


# ---------------------------------------------------------------------------
# Whale taxes: the guild's richest actually lose coin, end-to-end
# ---------------------------------------------------------------------------

def test_rhystic_tollgate_steals_from_the_richest(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    """Driving the risky success end-to-end must transfer JC off the three
    richest players and into the digger (steal mode = Robin Hood)."""
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    _register(player_repository, 10001, balance=500)            # the digger
    for vid in (10002, 10003, 10004):
        _register(player_repository, vid, balance=1000)         # the rich
    _seed_tunnel(dig_service, dig_repo, depth=60)               # >= min_depth 51
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    victims = (10002, 10003, 10004)
    before = {v: player_repository.get_balance(v, 12345) for v in victims}
    digger_before = player_repository.get_balance(10001, 12345)

    r = dig_service.resolve_event(10001, 12345, "rhystic_tollgate", "risky")
    assert r["success"] and r["succeeded"], r

    splash = r["splash"]
    assert splash is not None and splash["mode"] == "steal"
    assert splash["total_burned"] == 3 * 12  # magnitude transferred

    after = {v: player_repository.get_balance(v, 12345) for v in victims}
    for v in victims:
        assert before[v] - after[v] == 12, f"victim {v} should lose 12 JC"

    # Zero-sum to the digger: they pocket the event payout *plus* the stolen pot.
    digger_after = player_repository.get_balance(10001, 12345)
    assert digger_after - digger_before == r["jc_delta"] + splash["total_burned"]


def test_underworld_reclaims_burns_the_richest(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    """Driving the risky success end-to-end must burn JC off the three richest
    players (destroyed, not transferred), and burn strictly more than the
    digger is paid — a real deflationary sink, not a coin printer."""
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    _register(player_repository, 10001, balance=500)            # the digger
    for vid in (10002, 10003, 10004):
        _register(player_repository, vid, balance=1000)         # the rich
    _seed_tunnel(dig_service, dig_repo, depth=80)               # >= min_depth 75
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    victims = (10002, 10003, 10004)
    before = {v: player_repository.get_balance(v, 12345) for v in victims}
    digger_before = player_repository.get_balance(10001, 12345)

    r = dig_service.resolve_event(10001, 12345, "underworld_reclaims", "risky")
    assert r["success"] and r["succeeded"], r

    splash = r["splash"]
    assert splash is not None and splash["mode"] == "burn"

    after = {v: player_repository.get_balance(v, 12345) for v in victims}
    burned = sum(before[v] - after[v] for v in victims)
    for v in victims:
        assert before[v] - after[v] == 22, f"victim {v} should lose 22 JC"

    assert splash["total_burned"] == burned == 66
    # The digger gains only their finder's share, and it is strictly less than
    # what was burned: net coin leaves the economy.
    digger_after = player_repository.get_balance(10001, 12345)
    assert digger_after - digger_before == r["jc_delta"]
    assert burned > r["jc_delta"] > 0
