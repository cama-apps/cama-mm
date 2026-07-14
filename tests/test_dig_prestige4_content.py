"""Tests for the prestige-4 content drop: twisted-homage bosses, reskins,
per-boss phases, signature trophy relics (carve + flashy mid-fight effects),
the general relics, and the repaired dig-time artifact roll.
"""

import json
import random

import pytest

from domain.models.boss_mechanics import get_mechanic
from domain.models.boss_stingers import get_stinger
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    ALL_ARTIFACTS,
    ARTIFACT_BY_ID,
    BOSSES_BY_ID,
    TROPHY_RELIC_IDS,
    get_boss_pool_for_tier,
    get_phase2_for,
    get_phase3_for,
)
from services.dig_data.artifacts import is_ordinary_relic
from services.dig_service import DigService

NEW_BOSSES = {
    "blightcoil": (150, "The Blightcoil", "bruiser", "weeping_fang"),
    "rimebound_king": (200, "The Rimebound King", "tank", "runebitten_shard"),
    "spineback": (275, "The Spineback", "bruiser", "aching_spine"),
}
NEW_MECHANICS = (
    "blightcoil_wards", "blightcoil_nova",
    "rimebound_harvest", "rimebound_raise",
    "spineback_regrowth", "spineback_divebomb",
)
NEW_STINGERS = ("blightcoil_venom", "rimebound_soulchill", "spineback_rend")
TROPHY_RELICS = (
    "weeping_fang", "runebitten_shard", "aching_spine",
    "listening_shard", "hateborn_ember",
)
GENERAL_RELICS = ("deepveined_coal", "diviners_knot", "pathfinders_spur")


@pytest.fixture
def svc(repo_db_path):
    drepo = DigRepository(repo_db_path)
    prepo = PlayerRepository(repo_db_path)
    s = DigService(drepo, prepo)
    s.player_repo.add(discord_id=111, discord_username="pf", guild_id=0)
    s.player_repo.add_balance(111, 0, 5000)
    s.dig_repo.create_tunnel(111, 0, "Test Tunnel")
    s.dig_repo.update_tunnel(111, 0, depth=300, prestige_level=5)
    return s


def _equip(svc, relic_id, did=111, gid=0):
    db_id = svc.dig_repo.add_artifact(did, gid, relic_id, is_relic=True)
    svc.equip_relic_for_player(did, gid, db_id)
    svc._invalidate_relic_cache(did, gid)
    return db_id


# --------------------------------------------------------------------------
# Boss gating, reskins, archetypes, trophies
# --------------------------------------------------------------------------

def test_new_bosses_gated_to_prestige_4():
    for bid, (tier, _name, _arch, _trophy) in NEW_BOSSES.items():
        at_p4 = {b.boss_id for b in get_boss_pool_for_tier(tier, prestige_level=4)}
        at_p3 = {b.boss_id for b in get_boss_pool_for_tier(tier, prestige_level=3)}
        assert bid in at_p4, f"{bid} should appear at P4 in tier {tier}"
        assert bid not in at_p3, f"{bid} must be hidden at P3 in tier {tier}"


def test_new_bosses_fields():
    from services.dig_constants import BOSS_ARCHETYPE_BY_ID
    for bid, (tier, name, arch, trophy) in NEW_BOSSES.items():
        boss = BOSSES_BY_ID[bid]
        assert boss.name == name
        assert boss.depth == tier
        assert boss.prestige_required == 4
        assert boss.trophy_relic_id == trophy
        assert BOSS_ARCHETYPE_BY_ID[bid] == arch


def test_reskins_keep_ids_change_names_and_add_trophies():
    # boss_id stays stable (no save-data break); display name reskinned.
    whisper = BOSSES_BY_ID["xalatath"]
    red = BOSSES_BY_ID["lilith"]
    assert whisper.name == "The Whispering Edge"
    assert whisper.prestige_required == 3
    assert whisper.trophy_relic_id == "listening_shard"
    assert red.name == "The Red Mother"
    assert red.prestige_required == 3
    assert red.trophy_relic_id == "hateborn_ember"


# --------------------------------------------------------------------------
# Per-boss phases
# --------------------------------------------------------------------------

def test_bespoke_phase_overrides_present():
    # Each new boss + reskin has its own phase-2 and phase-3 text.
    for bid in ("blightcoil", "rimebound_king", "spineback", "xalatath", "lilith"):
        boss = BOSSES_BY_ID[bid]
        p2 = get_phase2_for(bid, boss.depth)
        p3 = get_phase3_for(bid, boss.depth)
        assert p2 is not None and p3 is not None
        # Must not be the grandfathered depth-default text.
        assert p2.name not in ("The Sporeling Collective", "Chronofrost Paradox", "The Name Reclaimed")


def test_phase_resolver_falls_back_to_depth_default():
    # A boss without an override resolves to the depth-keyed default.
    assert get_phase2_for("sporeling_sovereign", 150).name == "The Sporeling Collective"
    # An unknown id at a depth with no phase-3 entry returns None.
    assert get_phase3_for("grothak", 25) is None


# --------------------------------------------------------------------------
# Mechanics + stingers
# --------------------------------------------------------------------------

def test_new_mechanics_well_formed():
    for mid in NEW_MECHANICS:
        m = get_mechanic(mid)
        assert m is not None, f"missing mechanic {mid}"
        assert len(m.options) == 3
        for opt in m.options:
            assert round(sum(r.probability for r in opt.outcome_rolls), 6) == 1.0
        assert 0 <= m.safe_option_idx < 3


def test_new_stingers_resolve():
    for sid in NEW_STINGERS:
        assert get_stinger(sid) is not None


# --------------------------------------------------------------------------
# Relic catalog
# --------------------------------------------------------------------------

def test_new_relics_in_catalog_with_lore_and_effect():
    for rid in TROPHY_RELICS + GENERAL_RELICS:
        relic = ARTIFACT_BY_ID[rid]
        assert relic.is_relic is True
        assert relic.effect, f"{rid} missing effect text"
        assert relic.lore_text, f"{rid} missing lore"
    # The prestige-4 trophies are a subset; later trophies (Death's Door) also exist.
    assert set(TROPHY_RELICS).issubset(set(TROPHY_RELIC_IDS))
    # Trophies gate to their boss's prestige; generals are P4.
    assert ARTIFACT_BY_ID["weeping_fang"].min_prestige == 4
    assert ARTIFACT_BY_ID["listening_shard"].min_prestige == 3
    assert all(ARTIFACT_BY_ID[r].min_prestige == 4 for r in GENERAL_RELICS)


def test_total_artifact_count():
    # 37 existing relics plus the 8 ordinary relics added with rarity progression.
    assert len(ALL_ARTIFACTS) == 45
    assert all(a.is_relic for a in ALL_ARTIFACTS)


# --------------------------------------------------------------------------
# Flashy trophy effects (pure — _run_one_round drives off status_effects flags)
# --------------------------------------------------------------------------

def _round(**kw):
    base = {
        "round_num": 2, "player_hp": 5, "boss_hp": 10, "player_hit": 0.0,
        "player_dmg": 1, "boss_hit": 0.0, "boss_dmg": 1, "status_effects": {},
    }
    base.update(kw)
    return DigService._run_one_round(None, **base)


def test_trophy_weeping_fang_venom_chips_boss():
    e, _php, bhp, _t = _round(status_effects={"trophy_venom": 4})
    assert bhp == 9 and e.get("venom")


def test_trophy_listening_shard_forewarned_only_round_1():
    se = {"trophy_forewarned": True}
    _e, php1, _b, _t = _round(round_num=1, boss_hit=1.0, status_effects=se)
    assert php1 == 5  # boss could not land round 1
    _e, php2, _b, _t = _round(round_num=2, boss_hit=1.0, status_effects={})
    assert php2 == 4  # round 2 lands normally


def test_trophy_runebitten_lifesteal_first_hit_only():
    se = {"trophy_lifesteal": True, "trophy_start_hp": 3}
    e, php, _b, _t = _round(round_num=1, player_hp=2, player_hit=1.0, status_effects=se)
    assert php == 3 and e.get("lifesteal") and se["trophy_lifesteal"] is False


def test_trophy_aching_spine_regrowth_capped():
    e, php, _b, _t = _round(player_hp=3, status_effects={"trophy_regrowth": True, "trophy_start_hp": 5})
    assert php == 4 and e.get("regrowth")
    # Already at start HP — no regrowth.
    e, php, _b, _t = _round(player_hp=3, status_effects={"trophy_regrowth": True, "trophy_start_hp": 3})
    assert php == 3 and not e.get("regrowth")


def test_trophy_hateborn_last_stand_extra_damage_at_1hp():
    e, _php, bhp, _t = _round(player_hp=1, player_hit=1.0, status_effects={"trophy_laststand": True})
    assert bhp == 8 and e.get("laststand")  # 2 damage instead of 1


# --------------------------------------------------------------------------
# Carve (signature drop)
# --------------------------------------------------------------------------

def test_carve_grants_then_dedups(svc, monkeypatch):
    boss = BOSSES_BY_ID["blightcoil"]
    monkeypatch.setattr(random, "random", lambda: 0.0)  # always under carve rate
    drop = svc._maybe_carve_trophy_relic(111, 0, boss)
    assert drop and drop["id"] == "weeping_fang"
    owned = {a.get("artifact_id") for a in svc.dig_repo.get_artifacts(111, 0)}
    assert "weeping_fang" in owned
    # Already owned — never re-drops (no dupe sink).
    assert svc._maybe_carve_trophy_relic(111, 0, boss) is None


def test_carve_respects_rate(svc, monkeypatch):
    boss = BOSSES_BY_ID["spineback"]
    monkeypatch.setattr(random, "random", lambda: 0.99)  # above carve rate
    assert svc._maybe_carve_trophy_relic(111, 0, boss) is None


def test_carve_drops_through_live_victory(svc, monkeypatch):
    # End-to-end: a full P4 victory through the live duel path carves the boss's
    # trophy and threads it into the result payload. The boss sits in its phase-2
    # fight (status phase1_defeated) at P4 — winning that is a full victory (phase
    # 3 needs P5), so resolution reaches the carve hook. Seed a paused duel with
    # the boss at 1 HP and resume to the kill (deterministic, no trigger-RNG).
    from domain.models.boss_mechanics import MECHANIC_REGISTRY
    svc.dig_repo.update_tunnel(
        111, 0, depth=150, prestige_level=4,
        boss_progress=json.dumps({
            "25": "defeated", "50": "defeated", "75": "defeated", "100": "defeated",
            "150": {"boss_id": "blightcoil", "status": "phase1_defeated"},
        }),
    )
    mech = MECHANIC_REGISTRY["blightcoil_wards"]
    svc.dig_repo.save_active_duel(111, 0, {
        "boss_id": "blightcoil", "tier": 150, "mechanic_id": "blightcoil_wards",
        "risk_tier": "cautious", "wager": 0,
        "player_hp": 5, "boss_hp": 1, "round_num": 3,
        "round_log": json.dumps([]),
        "pending_prompt": json.dumps({
            "mechanic_id": "blightcoil_wards",
            "prompt_title": mech.prompt_title,
            "prompt_description": mech.prompt_description,
            "options": [{"option_idx": i, "label": o.label} for i, o in enumerate(mech.options)],
            "safe_option_idx": mech.safe_option_idx,
        }),
        "rng_state": "",
        "status_effects": json.dumps({"attempts_this_fight": 1, "initial_win_chance": 0.5, "multiplier": 2.0}),
        "echo_applied": 0, "echo_killer_id": None,
        "player_hit": 0.9, "player_dmg": 1, "boss_hit": 0.0, "boss_dmg": 1,
    })
    monkeypatch.setattr(random, "random", lambda: 0.0)  # option's 1st branch; player hits; carve passes
    result = svc.resume_boss_duel(111, 0, option_idx=0)
    assert result.get("won") is True, result
    drop = result.get("trophy_relic_drop")
    assert drop and drop["id"] == "weeping_fang", drop


# --------------------------------------------------------------------------
# Pool / dig-roll exclusion + the repaired roll_artifact
# --------------------------------------------------------------------------

def test_trophies_excluded_from_prestige_relic_pool(svc, monkeypatch):
    monkeypatch.setattr(random, "random", lambda: 0.0)  # force a drop each call
    dropped = set()
    for _ in range(300):
        d = svc._maybe_drop_prestige_relic(111, 0, 5)
        if d:
            dropped.add(d["id"])
    assert dropped.isdisjoint(TROPHY_RELIC_IDS), f"trophy leaked into pool: {dropped & set(TROPHY_RELICS)}"
    # General P4 relics still drop from this pool.
    assert dropped & set(GENERAL_RELICS)


def test_roll_artifact_finds_only_ordinary_relics(svc, monkeypatch):
    # Raw digs use the ordinary rarity progression; prestige-gated and trophy
    # relics remain exclusive to their original sources.
    monkeypatch.setattr(random, "random", lambda: 0.0)  # always pass the find roll
    random.seed(7)
    found = set()
    for _ in range(80):
        r = svc.roll_artifact(111, 0, 160)  # Fungal Depths
        if r:
            found.add(r["id"])
    assert found, "roll_artifact should find ordinary relics"
    ordinary = {a.id for a in ALL_ARTIFACTS if is_ordinary_relic(a)}
    assert found <= ordinary, f"exclusive relic leaked into dig finds: {found - ordinary}"
    for excluded in ("deepveined_coal", "aching_spine", "hollow_fang"):
        assert excluded not in found  # general / trophy / boss-only P5


def test_roll_artifact_relics_are_unique(svc, monkeypatch):
    # A relic is never found twice — acquisition skips relics already owned.
    monkeypatch.setattr(random, "random", lambda: 0.0)
    random.seed(3)
    seen = []
    for _ in range(80):
        r = svc.roll_artifact(111, 0, 160)
        if r:
            seen.append(r["id"])
    assert seen and len(seen) == len(set(seen)), f"duplicate dig find: {seen}"


def test_equip_rejects_duplicate_relic(svc):
    # Two rows of the same relic (e.g. legacy dups) — equipping the second is refused.
    db1 = svc.dig_repo.add_artifact(111, 0, "mole_claws", is_relic=True)
    db2 = svc.dig_repo.add_artifact(111, 0, "mole_claws", is_relic=True)
    assert svc.equip_relic_for_player(111, 0, db1).get("success")
    res = svc.equip_relic_for_player(111, 0, db2)
    assert not res.get("success")
    assert "already" in (res.get("error") or "").lower()


def test_prestige_relic_drop_skips_owned(svc, monkeypatch):
    # Owned relics aren't dropped again from the boss pool (relics are unique).
    monkeypatch.setattr(random, "random", lambda: 0.0)  # force a drop each call
    svc.dig_repo.add_artifact(111, 0, "deepveined_coal", is_relic=True)
    random.seed(5)
    for _ in range(100):
        d = svc._maybe_drop_prestige_relic(111, 0, 5)
        if d:
            assert d["id"] != "deepveined_coal"


# --------------------------------------------------------------------------
# General relic effects
# --------------------------------------------------------------------------

def test_deepveined_coal_boosts_yield_only_in_the_dark(svc):
    _equip(svc, "deepveined_coal")
    bright = svc._relic_jc_yield_multiplier(111, 0, luminosity=100, include_random=False)
    dark = svc._relic_jc_yield_multiplier(111, 0, luminosity=5, include_random=False)
    assert bright == pytest.approx(1.0)
    assert dark == pytest.approx(1.20)


def _deep_tunnel(svc, depth=160):
    # A realistic deep tunnel: lower boss boundaries cleared (so the next
    # boundary is above us and the advance isn't clamped), past the first dig.
    cleared = {str(b): "defeated" for b in (25, 50, 75, 100, 150)}
    svc.dig_repo.update_tunnel(
        111, 0, depth=depth, prestige_level=1, last_dig_at=0,
        total_digs=10, boss_progress=json.dumps(cleared),
    )


def test_pathfinders_spur_adds_advance_in_deep_layers(svc, monkeypatch):
    # Deterministic dig: no cave-in/event/artifact (random()=0.99), top of the
    # advance range (randint->hi) so the base advance is positive and the
    # relic's +1 is visible.
    monkeypatch.setattr(random, "random", lambda: 0.99)
    monkeypatch.setattr(random, "randint", lambda lo, hi: hi)
    _deep_tunnel(svc, 160)
    without = svc.dig(111, 0).get("advance")
    assert without and without > 0
    _equip(svc, "pathfinders_spur")
    _deep_tunnel(svc, 160)
    with_relic = svc.dig(111, 0).get("advance")
    assert with_relic == without + 1


def test_diviners_knot_boosts_risky_success(svc, monkeypatch):
    from services.dig_constants import EVENT_POOL
    evt = next(
        e for e in EVENT_POOL
        if e.get("risky_option")
        and 0.0 < e["risky_option"].get("success_chance", 1.0) <= 0.8
        and e["risky_option"].get("failure") is not None
        and e["risky_option"].get("success") is not None
    )
    sc = evt["risky_option"]["success_chance"]
    # Roll lands in the +10% band: fails at base, succeeds with the relic.
    monkeypatch.setattr(random, "random", lambda: sc + 0.05)
    svc.dig_repo.update_tunnel(111, 0, depth=160, prestige_level=5)
    without = svc.resolve_event(111, 0, evt["id"], "risky")
    _equip(svc, "diviners_knot")
    svc.dig_repo.update_tunnel(111, 0, depth=160)
    with_relic = svc.resolve_event(111, 0, evt["id"], "risky")
    success_desc = evt["risky_option"]["success"]["description"]
    failure_desc = evt["risky_option"]["failure"]["description"]
    assert without.get("message") == failure_desc
    assert with_relic.get("message") == success_desc
