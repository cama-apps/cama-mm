"""Tests for the betting announcer persona roster."""

import random

from services.betting_personas import BETTING_PERSONAS, pick_betting_persona
from services.flavor_personas import FlavorPersona


class TestBettingPersonas:
    def test_roster_has_eight_distinct_personas(self):
        assert len(BETTING_PERSONAS) == 8
        assert len(set(BETTING_PERSONAS.keys())) == 8

    def test_each_persona_is_well_formed(self):
        for key, persona in BETTING_PERSONAS.items():
            assert isinstance(persona, FlavorPersona)
            assert persona.key == key
            assert persona.name
            assert persona.system_prompt
            assert isinstance(persona.examples, list)
            assert 4 <= len(persona.examples) <= 6
            for example in persona.examples:
                assert isinstance(example, str)
                assert example.strip()
                assert len(example) <= 200

    def test_pick_returns_a_roster_member(self):
        persona = pick_betting_persona()
        assert isinstance(persona, FlavorPersona)
        assert persona.key in BETTING_PERSONAS

    def test_pick_is_deterministic_with_seeded_rng(self):
        rng_a = random.Random(7)
        rng_b = random.Random(7)
        seq_a = [pick_betting_persona(rng_a).key for _ in range(20)]
        seq_b = [pick_betting_persona(rng_b).key for _ in range(20)]
        assert seq_a == seq_b

    def test_pick_can_reach_every_roster_entry(self):
        rng = random.Random(0)
        seen = {pick_betting_persona(rng).key for _ in range(500)}
        assert seen == set(BETTING_PERSONAS.keys())
