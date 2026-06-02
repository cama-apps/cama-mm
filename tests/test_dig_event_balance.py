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

from services.dig_data.aliases import EVENT_POOL


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


def test_high_p2_event_rewards_are_modestly_trimmed():
    """Only the high-end P2 quest payouts get a small haircut."""
    events_by_id = {e["id"]: e for e in EVENT_POOL}
    expected = {
        "necro_s3": {"risky": 13, "desperate": 20},
        "necro_s4": {"risky": 13, "desperate": 20},
        "necro_s5": {"risky": 17, "desperate": 27},
    }
    for event_id, payouts in expected.items():
        event = events_by_id[event_id]
        assert event["risky_option"]["success"]["jc"] == payouts["risky"]
        assert event["desperate_option"]["success"]["jc"] == payouts["desperate"]


def test_social_burn_events_are_real_global_sinks():
    """The global deflation events must burn more JC than they grant the actor."""
    expected_burns = {
        "hungering_dark": 5,
        "deny_the_seam": 6,
        "turf_war": 6,
        "smoke_ambush": 7,
        "the_tear": 8,
        "the_deep_hunter": 12,
    }
    events_by_id = {e["id"]: e for e in EVENT_POOL}
    for event_id, penalty in expected_burns.items():
        event = events_by_id[event_id]
        splash = event["splash"]
        assert splash["mode"] == "burn"
        assert splash["penalty_jc"] == penalty
        total_burn = splash["victim_count"] * splash["penalty_jc"]
        actor_jc = event["risky_option"]["success"].get("jc", 0)
        assert total_burn > actor_jc
