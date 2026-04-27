"""Tests for the post-match flavor persona roster."""

import random

from services.flavor_personas import PERSONAS, FlavorPersona, pick_persona


class TestFlavorPersonas:
    def test_roster_has_eight_distinct_personas(self):
        assert len(PERSONAS) == 8
        keys = list(PERSONAS.keys())
        assert len(set(keys)) == 8

    def test_each_persona_is_well_formed(self):
        for key, persona in PERSONAS.items():
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

    def test_pick_persona_returns_a_persona_from_the_roster(self):
        persona = pick_persona()
        assert isinstance(persona, FlavorPersona)
        assert persona.key in PERSONAS

    def test_pick_persona_is_deterministic_with_seeded_rng(self):
        rng_a = random.Random(42)
        rng_b = random.Random(42)
        sequence_a = [pick_persona(rng_a).key for _ in range(20)]
        sequence_b = [pick_persona(rng_b).key for _ in range(20)]
        assert sequence_a == sequence_b

    def test_pick_persona_can_reach_every_roster_entry(self):
        rng = random.Random(0)
        seen = {pick_persona(rng).key for _ in range(500)}
        assert seen == set(PERSONAS.keys())
