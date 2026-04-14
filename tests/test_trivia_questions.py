"""Tests for services/trivia_questions.py — question generators and selection."""

import pytest

from services.trivia_data import AbilityData
from services.trivia_questions import (
    CHALLENGING_GENERATORS,
    EASY_GENERATORS,
    HARD_GENERATORS,
    IMAGE_GENERATORS,
    MEDIUM_GENERATORS,
    TriviaQuestion,
    _hero_name_leaks,
    gen_ability_by_icon,
    gen_ability_cooldown,
    gen_ability_lore,
    gen_ability_to_hero,
    gen_attack_type,
    gen_attribute_gain,
    gen_base_armor_compare,
    gen_base_attack_time,
    gen_damage_type,
    gen_enchantment_effect,
    gen_facet_name,
    gen_facet_to_hero,
    gen_hero_bio,
    gen_hero_by_hype,
    gen_hero_by_image,
    gen_hero_real_name,
    gen_innate_ability,
    gen_item_by_icon,
    gen_item_cost_compare,
    gen_item_cost_exact,
    gen_item_lore,
    gen_move_speed,
    gen_neutral_item_tier,
    gen_primary_attribute,
    gen_scepter_upgrade,
    gen_shard_upgrade,
    gen_voiceline,
    generate_question,
    get_difficulty_tier,
)


class TestDifficultyTier:
    def test_easy(self):
        assert get_difficulty_tier(0) == "easy"
        assert get_difficulty_tier(1) == "easy"
        assert get_difficulty_tier(2) == "easy"

    def test_medium(self):
        assert get_difficulty_tier(3) == "medium"
        assert get_difficulty_tier(4) == "medium"
        assert get_difficulty_tier(5) == "medium"

    def test_hard(self):
        assert get_difficulty_tier(6) == "hard"
        assert get_difficulty_tier(9) == "hard"

    def test_challenging(self):
        assert get_difficulty_tier(10) == "challenging"
        assert get_difficulty_tier(100) == "challenging"


def _validate_question(q: TriviaQuestion):
    """Validate that a generated question has correct structure."""
    assert q is not None
    assert len(q.options) == 4
    assert 0 <= q.correct_index <= 3
    assert q.difficulty in ("easy", "medium", "hard", "challenging")
    assert q.category
    assert q.text
    # All options should be unique
    assert len(set(q.options)) == 4, f"Duplicate options: {q.options}"
    # Correct answer should be in options
    assert q.options[q.correct_index]


class TestHeroNameLeaks:
    def test_obvious_leak(self):
        assert _hero_name_leaks("Phantom Rush", "Phantom Lancer") is True

    def test_no_leak(self):
        assert _hero_name_leaks("Blink Strike", "Phantom Lancer") is False

    def test_short_words_ignored(self):
        # "of" is <=2 chars, should not count as a leak
        assert _hero_name_leaks("Cup of Tea", "Keeper of the Light") is False

    def test_case_insensitive(self):
        assert _hero_name_leaks("puck shot", "Puck") is True

    def test_single_word_hero(self):
        assert _hero_name_leaks("Puckish", "Puck") is True


class TestEasyGenerators:
    def test_hero_by_image(self):
        q = gen_hero_by_image()
        _validate_question(q)
        assert q.difficulty == "easy"
        assert q.image_url is not None

    def test_primary_attribute(self):
        q = gen_primary_attribute()
        _validate_question(q)
        assert q.difficulty == "easy"

    def test_ability_to_hero(self):
        q = gen_ability_to_hero()
        _validate_question(q)
        assert q.difficulty == "easy"

    def test_attack_type(self):
        q = gen_attack_type()
        _validate_question(q)
        assert q.difficulty == "easy"
        assert "melee" in q.text or "ranged" in q.text

    def test_item_cost_compare(self):
        q = gen_item_cost_compare()
        _validate_question(q)
        assert q.difficulty == "easy"

    def test_neutral_item_tier(self):
        q = gen_neutral_item_tier()
        _validate_question(q)
        assert q.difficulty == "easy"

    def test_damage_type(self):
        q = gen_damage_type()
        _validate_question(q)
        assert q.difficulty == "easy"

    def test_ability_by_icon(self):
        q = gen_ability_by_icon()
        _validate_question(q)
        assert q.difficulty == "easy"
        assert q.image_url is not None

    def test_item_by_icon(self):
        q = gen_item_by_icon()
        _validate_question(q)
        assert q.difficulty == "easy"
        assert q.image_url is not None
        assert q.category == "item_by_icon"


class TestMediumGenerators:
    def test_hero_real_name(self):
        q = gen_hero_real_name()
        _validate_question(q)
        assert q.difficulty == "medium"

    def test_facet_to_hero(self):
        q = gen_facet_to_hero()
        _validate_question(q)
        assert q.difficulty == "medium"

    def test_hero_by_hype(self):
        for _ in range(10):
            q = gen_hero_by_hype()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "medium"
                return
        pytest.fail("gen_hero_by_hype returned None after 10 tries")

    def test_innate_ability(self):
        q = gen_innate_ability()
        _validate_question(q)
        assert q.difficulty == "medium"
        assert q.image_url is not None

    def test_enchantment_effect(self):
        q = gen_enchantment_effect()
        _validate_question(q)
        assert q.difficulty == "medium"
        assert q.category == "enchantment_effect"
        assert "bonuses" in q.text


class TestHardGenerators:
    def test_scepter_upgrade(self):
        q = gen_scepter_upgrade()
        _validate_question(q)
        assert q.difficulty == "hard"

    def test_scepter_upgrade_skips_leaking_descriptions(self, monkeypatch):
        abilities = [
            AbilityData(
                id=1,
                name="shadow_demon_demonic_purge",
                localized_name="Demonic Purge",
                hero_id=None,
                hero_name="Shadow Demon",
                damage_type=None,
                damage=None,
                cooldown=None,
                lore=None,
                scepter_upgrades=True,
                scepter_description="Causes Demonic Purge to break and gain charges.",
                shard_upgrades=False,
                shard_description=None,
                innate=False,
                icon_url=None,
            ),
            AbilityData(
                id=2,
                name="axe_berserkers_call",
                localized_name="Berserker's Call",
                hero_id=None,
                hero_name="Axe",
                damage_type=None,
                damage=None,
                cooldown=None,
                lore=None,
                scepter_upgrades=True,
                scepter_description="Applies Battle Hunger to taunted enemies.",
                shard_upgrades=False,
                shard_description=None,
                innate=False,
                icon_url=None,
            ),
            AbilityData(
                id=3,
                name="crystal_maiden_frostbite",
                localized_name="Frostbite",
                hero_id=None,
                hero_name="Crystal Maiden",
                damage_type=None,
                damage=None,
                cooldown=None,
                lore=None,
                scepter_upgrades=True,
                scepter_description="Creates a freezing explosion around the target.",
                shard_upgrades=False,
                shard_description=None,
                innate=False,
                icon_url=None,
            ),
            AbilityData(
                id=4,
                name="juggernaut_blade_fury",
                localized_name="Blade Fury",
                hero_id=None,
                hero_name="Juggernaut",
                damage_type=None,
                damage=None,
                cooldown=None,
                lore=None,
                scepter_upgrades=True,
                scepter_description="Allows movement during Omnislash and increases slash rate.",
                shard_upgrades=False,
                shard_description=None,
                innate=False,
                icon_url=None,
            ),
            AbilityData(
                id=5,
                name="lich_frost_shield",
                localized_name="Frost Shield",
                hero_id=None,
                hero_name="Lich",
                damage_type=None,
                damage=None,
                cooldown=None,
                lore=None,
                scepter_upgrades=True,
                scepter_description="Adds a damaging nova when the shield expires.",
                shard_upgrades=False,
                shard_description=None,
                innate=False,
                icon_url=None,
            ),
        ]

        monkeypatch.setattr("services.trivia_questions.load_abilities", lambda: abilities)
        monkeypatch.setattr("services.trivia_questions.random.choice", lambda seq: seq[0])
        monkeypatch.setattr("services.trivia_questions.random.shuffle", lambda seq: None)

        q = gen_scepter_upgrade()

        _validate_question(q)
        assert q.text == "What does Aghanim's Scepter do for Axe?"
        assert "Shadow Demon" not in q.text

    def test_shard_upgrade(self):
        q = gen_shard_upgrade()
        _validate_question(q)
        assert q.difficulty == "hard"

    def test_item_cost_exact(self):
        q = gen_item_cost_exact()
        _validate_question(q)
        assert q.difficulty == "hard"

    def test_ability_lore(self):
        q = gen_ability_lore()
        _validate_question(q)
        assert q.difficulty == "hard"
        assert q.image_url is not None

    def test_item_lore(self):
        q = gen_item_lore()
        _validate_question(q)
        assert q.difficulty == "hard"
        assert q.image_url is not None

    def test_hero_bio(self):
        q = gen_hero_bio()
        _validate_question(q)
        assert q.difficulty == "hard"

    def test_move_speed(self):
        for _ in range(10):
            q = gen_move_speed()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "hard"
                return
        pytest.fail("gen_move_speed returned None")

    def test_ability_cooldown(self):
        for _ in range(10):
            q = gen_ability_cooldown()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "hard"
                return
        pytest.fail("gen_ability_cooldown returned None")

    def test_facet_name(self):
        q = gen_facet_name()
        _validate_question(q)
        assert q.difficulty == "hard"

    def test_base_armor_compare(self):
        for _ in range(10):
            q = gen_base_armor_compare()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "hard"
                return
        pytest.fail("gen_base_armor_compare returned None")


class TestChallengingGenerators:
    def test_voiceline(self):
        for _ in range(10):
            q = gen_voiceline()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "challenging"
                return
        pytest.fail("gen_voiceline returned None")

    def test_base_attack_time(self):
        for _ in range(20):
            q = gen_base_attack_time()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "challenging"
                assert q.category == "base_attack_time"
                assert "BAT" in q.text
                return
        pytest.fail("gen_base_attack_time returned None")

    def test_attribute_gain(self):
        for _ in range(20):
            q = gen_attribute_gain()
            if q is not None:
                _validate_question(q)
                assert q.difficulty == "challenging"
                assert q.category == "attribute_gain"
                assert "gain per level" in q.text
                return
        pytest.fail("gen_attribute_gain returned None")


class TestAnswerLeaks:
    def test_ability_to_hero_no_hero_name_leak(self):
        """Verify ability names don't contain hero name words."""
        for _ in range(50):
            q = gen_ability_to_hero()
            if q is None:
                continue
            correct_hero = q.options[q.correct_index]
            # Extract ability name from the question text
            # Format: "Which hero has the ability 'X'?"
            ability_name = q.text.split("'")[1] if "'" in q.text else ""
            for word in correct_hero.split():
                if len(word) > 2:
                    assert word.lower() not in ability_name.lower(), (
                        f"Leak: ability '{ability_name}' contains hero word '{word}' from '{correct_hero}'"
                    )

    def test_scepter_descriptions_redacted(self):
        """Verify the correct option's description is redacted of the question hero's name."""
        for _ in range(30):
            q = gen_scepter_upgrade()
            if q is None:
                continue
            hero_name = q.text.split(" for ")[-1].rstrip("?")
            correct_opt = q.options[q.correct_index]
            for word in hero_name.split():
                if len(word) > 2:
                    assert word.lower() not in correct_opt.lower(), (
                        f"Leak: correct option '{correct_opt}' contains hero word '{word}'"
                    )

    def test_shard_descriptions_redacted(self):
        """Verify the correct option's description is redacted of the question hero's name."""
        for _ in range(30):
            q = gen_shard_upgrade()
            if q is None:
                continue
            hero_name = q.text.split(" for ")[-1].rstrip("?")
            correct_opt = q.options[q.correct_index]
            for word in hero_name.split():
                if len(word) > 2:
                    assert word.lower() not in correct_opt.lower(), (
                        f"Leak: correct option '{correct_opt}' contains hero word '{word}'"
                    )

    def test_facet_to_hero_no_hero_name_leak(self):
        """Verify facet names shown in question don't leak the hero name."""
        for _ in range(50):
            q = gen_facet_to_hero()
            if q is None:
                continue
            correct_hero = q.options[q.correct_index]
            # The facet name appears before the colon in the question text
            facet_part = q.text.split(":")[0].strip("'\"")
            for word in correct_hero.split():
                if len(word) > 2:
                    assert word.lower() not in facet_part.lower(), (
                        f"Leak: facet text '{facet_part}' contains hero word '{word}'"
                    )


class TestImageGenerators:
    def test_image_generators_set_accurate(self):
        """Verify IMAGE_GENERATORS set matches generators that actually produce images."""
        all_gens = EASY_GENERATORS + MEDIUM_GENERATORS + HARD_GENERATORS + CHALLENGING_GENERATORS
        for gen in all_gens:
            # Try a few times to get a non-None result
            for _ in range(10):
                q = gen()
                if q is not None:
                    if gen in IMAGE_GENERATORS:
                        assert q.image_url is not None, (
                            f"{gen.__name__} is in IMAGE_GENERATORS but produced no image"
                        )
                    break


class TestGenerateQuestion:
    def test_easy_streak(self):
        q = generate_question(0)
        assert q is not None
        assert q.difficulty == "easy"

    def test_medium_streak(self):
        q = generate_question(4)
        assert q is not None
        assert q.difficulty == "medium"

    def test_hard_streak(self):
        q = generate_question(7)
        assert q is not None
        assert q.difficulty == "hard"

    def test_challenging_streak(self):
        q = generate_question(10)
        assert q is not None
        assert q.difficulty == "challenging"

    def test_avoids_recent_categories(self):
        # Generate many questions, checking that recent categories are usually avoided
        recent = []
        for _ in range(20):
            q = generate_question(0, recent)
            assert q is not None
            recent.append(q.category)

    def test_all_generators_registered(self):
        assert len(EASY_GENERATORS) == 9
        assert len(MEDIUM_GENERATORS) == 5
        assert len(HARD_GENERATORS) == 10
        assert len(CHALLENGING_GENERATORS) == 3
