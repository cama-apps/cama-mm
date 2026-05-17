"""Curated dig DM NPC roster: data-integrity checks on the canon NPC set
and the prompt-injection line formatter."""

from services.dig_npcs import NPCS, DigNPC, roster_lines

# The three tone profiles the DM rolls; every NPC must be voiced on-tone
# so the chosen voice always has someone to summon (see module docstring).
VALID_VOICES = {"cosmic_dread", "industrial_grim", "cryptic_folkloric"}


class TestRosterIntegrity:
    """The roster is canon data injected into the DM prompt; malformed
    entries would corrupt narration or break prompt assembly."""

    def test_roster_is_non_empty(self):
        # An empty roster leaves the DM with no on-tone NPCs to summon.
        assert len(NPCS) > 0

    def test_dict_key_matches_npc_id(self):
        # roster_lines() and any lookups assume key == entry.npc_id; a
        # mismatch would make an NPC referenceable under the wrong handle.
        for key, npc in NPCS.items():
            assert key == npc.npc_id

    def test_npc_ids_are_unique(self):
        # IDs double as the memory-blob handle for persistence; collisions
        # would let one NPC overwrite another.
        ids = [npc.npc_id for npc in NPCS.values()]
        assert len(set(ids)) == len(ids)

    def test_npc_ids_are_snake_case_tokens(self):
        # IDs are injected verbatim into the prompt as stable handles;
        # whitespace or casing drift would break recognition.
        for npc in NPCS.values():
            assert npc.npc_id
            assert npc.npc_id == npc.npc_id.lower()
            assert " " not in npc.npc_id

    def test_every_npc_uses_a_valid_voice(self):
        # The DM picks a tone profile then summons on-tone NPCs; an unknown
        # voice means that NPC can never be matched to the rolled tone.
        for npc in NPCS.values():
            assert npc.voice in VALID_VOICES, f"{npc.npc_id} has voice {npc.voice!r}"

    def test_all_three_tone_profiles_are_represented(self):
        # The roster must cover every rolled tone so no voice is left
        # without an NPC -- this is the roster's stated design goal.
        present = {npc.voice for npc in NPCS.values()}
        assert present == VALID_VOICES

    def test_titles_are_non_blank_and_carry_no_proper_names(self):
        # The contract is "titled (no proper-name) figure": titles should
        # read as roles, conventionally lowercase-articled ("the X").
        for npc in NPCS.values():
            assert npc.title.strip()
            assert npc.title.startswith("the ")

    def test_triggers_are_non_blank(self):
        # Triggers tell the DM when to summon the NPC; a blank trigger
        # makes the entry effectively unusable.
        for npc in NPCS.values():
            assert isinstance(npc.triggers, str)
            assert npc.triggers.strip()

    def test_sample_lines_present_and_well_formed(self):
        # Sample lines anchor the NPC's voice for the LLM; an empty list or
        # blank line gives the DM nothing to imitate.
        for npc in NPCS.values():
            assert isinstance(npc.sample_lines, list)
            assert len(npc.sample_lines) > 0
            for line in npc.sample_lines:
                assert isinstance(line, str)
                assert line.strip()

    def test_npc_is_frozen_dataclass(self):
        # Frozen guards the canon set from accidental mutation at runtime;
        # the DM is meant to invent *new* NPCs, not edit canon ones.
        npc = next(iter(NPCS.values()))
        assert isinstance(npc, DigNPC)
        try:
            npc.title = "mutated"  # type: ignore[misc]
        except Exception as exc:
            assert isinstance(exc, (AttributeError, TypeError))
        else:
            raise AssertionError("DigNPC should be immutable")


class TestRosterLines:
    """roster_lines() flattens the roster into prompt-injection bullets."""

    def test_one_line_per_npc(self):
        # Every canon NPC must reach the prompt, exactly once.
        assert len(roster_lines()) == len(NPCS)

    def test_line_format_includes_id_title_voice_and_triggers(self):
        # Format contract: "- <id> (<title>, <voice>): <triggers>".
        lines = roster_lines()
        for npc in NPCS.values():
            expected = (
                f"- {npc.npc_id} ({npc.title}, {npc.voice}): {npc.triggers}"
            )
            assert expected in lines

    def test_lines_are_bullet_prefixed(self):
        # The DM prompt expects bullet list items.
        for line in roster_lines():
            assert line.startswith("- ")

    def test_every_npc_id_is_recoverable_from_output(self):
        # The DM references NPCs by id; each id must appear in the joined
        # roster text so the model can cite them.
        joined = "\n".join(roster_lines())
        for npc in NPCS.values():
            assert npc.npc_id in joined
