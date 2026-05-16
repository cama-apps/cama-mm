"""Tunnel name generation: the three-pool RNG picker and its
40/35/25 pool-weighting branches."""

import random

from services.dig_constants import (
    TUNNEL_NAME_ADJECTIVES,
    TUNNEL_NAME_NOUNS,
    TUNNEL_NAME_SILLY,
    TUNNEL_NAME_TITLES,
)
from services.dig_tunnel_naming_service import DigTunnelNamingService


def _expected_adj_noun_names() -> set[str]:
    """Every name the adjective+noun pool can produce."""
    return {
        f"The {adj} {noun}"
        for adj in TUNNEL_NAME_ADJECTIVES
        for noun in TUNNEL_NAME_NOUNS
    }


class TestBranchSelection:
    """generate_tunnel_name routes on random.random(): <0.40 adj+noun,
    <0.75 title, else silly. Each branch must draw from its own pool."""

    def test_low_roll_yields_adjective_noun_name(self, monkeypatch):
        # roll 0.0 is below the 0.40 cutoff -> "The <Adjective> <Noun>".
        rolls = iter([0.0, 0.0, 0.0])
        monkeypatch.setattr(random, "random", lambda: next(rolls))
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name == f"The {TUNNEL_NAME_ADJECTIVES[0]} {TUNNEL_NAME_NOUNS[0]}"

    def test_boundary_below_040_is_adjective_noun(self, monkeypatch):
        # 0.399... must still take the adj+noun branch (strict <0.40).
        monkeypatch.setattr(random, "random", lambda: 0.3999)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name in _expected_adj_noun_names()

    def test_mid_roll_yields_title_name(self, monkeypatch):
        # 0.40 <= roll < 0.75 -> a name straight from the title pool.
        monkeypatch.setattr(random, "random", lambda: 0.50)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name == TUNNEL_NAME_TITLES[0]

    def test_boundary_at_040_is_title(self, monkeypatch):
        # Exactly 0.40 falls out of the adj+noun branch into title.
        monkeypatch.setattr(random, "random", lambda: 0.40)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name in TUNNEL_NAME_TITLES

    def test_boundary_below_075_is_title(self, monkeypatch):
        # 0.749... is still the title branch (strict <0.75).
        monkeypatch.setattr(random, "random", lambda: 0.7499)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name in TUNNEL_NAME_TITLES

    def test_high_roll_yields_silly_name(self, monkeypatch):
        # roll >= 0.75 -> the silly pool, returned verbatim.
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name == TUNNEL_NAME_SILLY[0]

    def test_boundary_at_075_is_silly(self, monkeypatch):
        # Exactly 0.75 falls into the silly branch (title is strict <0.75).
        monkeypatch.setattr(random, "random", lambda: 0.75)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])
        name = DigTunnelNamingService().generate_tunnel_name()
        assert name in TUNNEL_NAME_SILLY


class TestOutputContract:
    """Whatever the branch, the returned name must be a usable label."""

    def test_always_returns_a_known_pool_name(self):
        # Every generated name must trace back to one of the three pools;
        # a name from nowhere would mean a logic bug in the picker.
        valid = (
            _expected_adj_noun_names()
            | set(TUNNEL_NAME_TITLES)
            | set(TUNNEL_NAME_SILLY)
        )
        random.seed(2024)
        svc = DigTunnelNamingService()
        for _ in range(500):
            assert svc.generate_tunnel_name() in valid

    def test_names_are_non_blank_strings(self):
        # Tunnel names are persisted and shown to players; a blank name
        # would render as an empty label and break clue generation.
        random.seed(11)
        svc = DigTunnelNamingService()
        for _ in range(200):
            name = svc.generate_tunnel_name()
            assert isinstance(name, str)
            assert name.strip()

    def test_all_three_pools_are_reachable(self):
        # Over many seeded draws each branch must fire at least once,
        # proving no pool is dead code under the 40/35/25 weighting.
        random.seed(777)
        svc = DigTunnelNamingService()
        silly = set(TUNNEL_NAME_SILLY)
        titles = set(TUNNEL_NAME_TITLES)
        adj_noun = _expected_adj_noun_names()
        saw_silly = saw_title = saw_adj_noun = False
        for _ in range(2000):
            name = svc.generate_tunnel_name()
            if name in silly:
                saw_silly = True
            elif name in titles:
                saw_title = True
            elif name in adj_noun:
                saw_adj_noun = True
        assert saw_silly and saw_title and saw_adj_noun

    def test_seeded_generation_is_deterministic(self):
        # Two services seeded identically must emit the same sequence --
        # the picker holds no state, so RNG is the only source of variance.
        random.seed(555)
        seq_a = [DigTunnelNamingService().generate_tunnel_name() for _ in range(20)]
        random.seed(555)
        seq_b = [DigTunnelNamingService().generate_tunnel_name() for _ in range(20)]
        assert seq_a == seq_b

    def test_weighting_roughly_matches_40_35_25(self):
        # The docstring promises ~40% adj+noun / ~35% title / ~25% silly;
        # a badly ordered branch chain would skew these shares.
        random.seed(20260516)
        svc = DigTunnelNamingService()
        silly = set(TUNNEL_NAME_SILLY)
        titles = set(TUNNEL_NAME_TITLES)
        counts = {"adj_noun": 0, "title": 0, "silly": 0}
        total = 6000
        for _ in range(total):
            name = svc.generate_tunnel_name()
            if name in silly:
                counts["silly"] += 1
            elif name in titles:
                counts["title"] += 1
            else:
                counts["adj_noun"] += 1
        # Generous tolerance bands so the test is not flaky on the seed.
        assert 0.33 < counts["adj_noun"] / total < 0.47
        assert 0.28 < counts["title"] / total < 0.42
        assert 0.18 < counts["silly"] / total < 0.32
