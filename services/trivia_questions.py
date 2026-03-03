"""
Trivia question generators for Dota 2 trivia.

Each generator returns a TriviaQuestion or None (if insufficient data).
"""

from __future__ import annotations

import random
from dataclasses import dataclass

from services.trivia_data import (
    AbilityData,
    FacetData,
    HeroData,
    ItemData,
    get_hero_by_id,
    load_abilities,
    load_facets,
    load_heroes,
    load_items,
    load_voicelines,
    redact_hero_name,
)


@dataclass
class TriviaQuestion:
    text: str
    options: list[str]       # 4 options
    correct_index: int       # 0-3
    difficulty: str           # "easy" / "medium" / "hard" / "challenging"
    image_url: str | None     # Steam CDN thumbnail
    category: str             # e.g. "hero_by_image"
    explanation: str | None   # Shown on wrong answer


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _hero_name_leaks(entity_name: str, hero_name: str) -> bool:
    """Return True if any word (>2 chars) from the hero name appears in the entity name."""
    for word in hero_name.split():
        if len(word) > 2 and word.lower() in entity_name.lower():
            return True
    return False


def _pick_distractors(correct: str, pool: list[str], n: int = 3) -> list[str] | None:
    """Pick n unique distractors from pool, excluding correct answer."""
    candidates = [x for x in pool if x != correct]
    if len(candidates) < n:
        return None
    return random.sample(candidates, n)


def _shuffle_options(correct: str, distractors: list[str]) -> tuple[list[str], int]:
    """Shuffle correct answer among distractors, return (options, correct_index)."""
    options = [correct] + distractors
    random.shuffle(options)
    return options, options.index(correct)


def _hero_pool() -> list[str]:
    return [h.localized_name for h in load_heroes()]


def _heroes_by_attr(attr: str) -> list[HeroData]:
    return [h for h in load_heroes() if h.attr_primary == attr]


# ---------------------------------------------------------------------------
# EASY generators (streak 0-2)
# ---------------------------------------------------------------------------

def gen_hero_by_image() -> TriviaQuestion | None:
    """E1: Show hero portrait, guess the hero."""
    heroes = [h for h in load_heroes() if h.image_url]
    if len(heroes) < 4:
        return None
    hero = random.choice(heroes)
    distractors = _pick_distractors(hero.localized_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text="Who is this hero?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=hero.image_url,
        category="hero_by_image",
        explanation=None,
    )


def gen_primary_attribute() -> TriviaQuestion | None:
    """E2: What is this hero's primary attribute?"""
    attr_display = {"strength": "Strength", "agility": "Agility", "intelligence": "Intelligence", "universal": "Universal"}
    heroes = [h for h in load_heroes() if h.attr_primary in attr_display and h.image_url]
    if not heroes:
        return None
    hero = random.choice(heroes)
    correct = attr_display[hero.attr_primary]
    distractors = [v for v in attr_display.values() if v != correct]
    if len(distractors) < 3:
        return None
    options, idx = _shuffle_options(correct, distractors[:3])
    return TriviaQuestion(
        text=f"What is {hero.localized_name}'s primary attribute?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=hero.image_url,
        category="primary_attribute",
        explanation=f"{hero.localized_name} is a {correct} hero.",
    )


def gen_ability_to_hero() -> TriviaQuestion | None:
    """E3: Which hero has this ability?"""
    abilities = [
        a for a in load_abilities()
        if a.hero_name and a.icon_url and not a.innate
        and not _hero_name_leaks(a.localized_name, a.hero_name)
    ]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    distractors = _pick_distractors(ability.hero_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(ability.hero_name, distractors)
    return TriviaQuestion(
        text=f"Which hero has the ability '{ability.localized_name}'?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=ability.icon_url,
        category="ability_to_hero",
        explanation=f"'{ability.localized_name}' belongs to {ability.hero_name}.",
    )


def gen_attack_type() -> TriviaQuestion | None:
    """E4: Which of these heroes is melee/ranged?"""
    is_melee = random.choice([True, False])
    label = "melee" if is_melee else "ranged"
    matching = [h for h in load_heroes() if h.is_melee == is_melee]
    non_matching = [h for h in load_heroes() if h.is_melee != is_melee]
    if not matching or len(non_matching) < 3:
        return None
    hero = random.choice(matching)
    distractors = _pick_distractors(hero.localized_name, [h.localized_name for h in non_matching])
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text=f"Which of these heroes is {label}?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=None,
        category="attack_type",
        explanation=f"{hero.localized_name} is a {label} hero.",
    )


def gen_item_cost_compare() -> TriviaQuestion | None:
    """E5: Which item costs the most?"""
    items = [i for i in load_items() if i.cost and i.cost > 0 and i.neutral_tier is None]
    if len(items) < 4:
        return None
    chosen = random.sample(items, 4)
    # Ensure costs are different enough to be interesting
    costs = [i.cost for i in chosen]
    if len(set(costs)) < 3:
        return None
    most_expensive = max(chosen, key=lambda i: i.cost)
    options, idx = _shuffle_options(
        most_expensive.localized_name,
        [i.localized_name for i in chosen if i != most_expensive][:3],
    )
    return TriviaQuestion(
        text="Which of these items costs the most?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=None,
        category="item_cost_compare",
        explanation=f"{most_expensive.localized_name} costs {most_expensive.cost} gold.",
    )


def gen_hero_by_hype() -> TriviaQuestion | None:
    """E6: Identify hero from hype text excerpt."""
    heroes = [h for h in load_heroes() if h.hype and len(h.hype) > 30]
    if len(heroes) < 4:
        return None
    hero = random.choice(heroes)
    excerpt = redact_hero_name(hero.hype[:150], hero.localized_name)
    if "???" not in excerpt and hero.localized_name.lower() in excerpt.lower():
        return None  # Couldn't redact
    distractors = _pick_distractors(hero.localized_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text=f'Which hero: "{excerpt}..."',
        options=options,
        correct_index=idx,
        difficulty="medium",
        image_url=None,
        category="hero_by_hype",
        explanation=None,
    )


def gen_neutral_item_tier() -> TriviaQuestion | None:
    """E7: What tier neutral item is this?"""
    neutrals = [i for i in load_items() if i.neutral_tier is not None and i.icon_url]
    if len(neutrals) < 4:
        return None
    item = random.choice(neutrals)
    correct = f"Tier {item.neutral_tier}"
    tiers = [f"Tier {t}" for t in sorted({n.neutral_tier for n in neutrals})]
    distractors = [t for t in tiers if t != correct]
    if len(distractors) < 3:
        return None
    distractors = random.sample(distractors, 3)
    options, idx = _shuffle_options(correct, distractors)
    return TriviaQuestion(
        text=f"What tier neutral item is {item.localized_name}?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=item.icon_url,
        category="neutral_item_tier",
        explanation=f"{item.localized_name} is a {correct} neutral item.",
    )


# ---------------------------------------------------------------------------
# MEDIUM generators (streak 3-5)
# ---------------------------------------------------------------------------

def gen_ability_by_icon() -> TriviaQuestion | None:
    """M1: Name this ability from its icon."""
    abilities = [a for a in load_abilities() if a.icon_url and a.hero_name and not a.innate]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    pool = [a.localized_name for a in abilities if a.localized_name != ability.localized_name]
    distractors = _pick_distractors(ability.localized_name, pool)
    if not distractors:
        return None
    options, idx = _shuffle_options(ability.localized_name, distractors)
    return TriviaQuestion(
        text="Name this ability.",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=ability.icon_url,
        category="ability_by_icon",
        explanation=f"This is {ability.localized_name} ({ability.hero_name}).",
    )


def gen_hero_real_name() -> TriviaQuestion | None:
    """M2: Which hero's real name is X?"""
    heroes = [h for h in load_heroes() if h.real_name]
    if len(heroes) < 4:
        return None
    hero = random.choice(heroes)
    distractors = _pick_distractors(hero.localized_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text=f"Which hero's real name is '{hero.real_name}'?",
        options=options,
        correct_index=idx,
        difficulty="medium",
        image_url=None,
        category="hero_real_name",
        explanation=f"{hero.localized_name}'s real name is {hero.real_name}.",
    )


def gen_scepter_upgrade() -> TriviaQuestion | None:
    """M3→H: What does Aghanim's Scepter do for this hero?"""
    abilities = [a for a in load_abilities() if a.scepter_upgrades and a.scepter_description and a.hero_name]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    # Use other scepter descriptions as distractors
    other = [a for a in abilities if a.hero_name != ability.hero_name and a.scepter_description]
    if len(other) < 3:
        return None
    dist_abilities = random.sample(other, 3)
    # Redact hero names from ALL option descriptions to prevent leaks
    correct_desc = redact_hero_name(ability.scepter_description[:100], ability.hero_name)
    # Skip if hero name still leaks as a substring (e.g. "Demon" in "Demonic")
    if _hero_name_leaks(correct_desc, ability.hero_name):
        return None
    distractors = [
        redact_hero_name(a.scepter_description[:100], a.hero_name)
        for a in dist_abilities
    ]
    options, idx = _shuffle_options(correct_desc, distractors)
    hero = get_hero_by_id(ability.hero_id) if ability.hero_id else None
    return TriviaQuestion(
        text=f"What does Aghanim's Scepter do for {ability.hero_name}?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=hero.image_url if hero else None,
        category="scepter_upgrade",
        explanation=f"Scepter: {ability.scepter_description[:120]}",
    )


def gen_item_cost_exact() -> TriviaQuestion | None:
    """M4: How much does this item cost?"""
    items = [i for i in load_items() if i.cost and i.cost > 0 and i.neutral_tier is None and i.icon_url]
    if len(items) < 4:
        return None
    item = random.choice(items)
    correct = str(item.cost)
    # Generate plausible wrong costs
    offsets = [-500, -300, -200, -100, 100, 200, 300, 500]
    random.shuffle(offsets)
    wrong = []
    for off in offsets:
        val = item.cost + off
        if val > 0 and str(val) != correct and str(val) not in wrong:
            wrong.append(str(val))
        if len(wrong) >= 3:
            break
    if len(wrong) < 3:
        return None
    options, idx = _shuffle_options(correct, wrong)
    return TriviaQuestion(
        text=f"How much does {item.localized_name} cost?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=item.icon_url,
        category="item_cost_exact",
        explanation=f"{item.localized_name} costs {item.cost} gold.",
    )


def gen_move_speed() -> TriviaQuestion | None:
    """M5: Which hero has the highest base move speed?"""
    heroes = [h for h in load_heroes() if h.base_movement and h.base_movement > 0]
    if len(heroes) < 4:
        return None
    chosen = random.sample(heroes, 4)
    speeds = [h.base_movement for h in chosen]
    if len(set(speeds)) < 3:
        return None
    fastest = max(chosen, key=lambda h: h.base_movement)
    distractors = [h.localized_name for h in chosen if h != fastest][:3]
    options, idx = _shuffle_options(fastest.localized_name, distractors)
    return TriviaQuestion(
        text="Which of these heroes has the highest base move speed?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=None,
        category="move_speed",
        explanation=f"{fastest.localized_name} has {fastest.base_movement} base move speed.",
    )


def gen_facet_to_hero() -> TriviaQuestion | None:
    """M6: This facet description belongs to which hero?"""
    facets = [
        f for f in load_facets()
        if f.description and f.hero_name
        and not _hero_name_leaks(f.localized_name, f.hero_name)
    ]
    if len(facets) < 4:
        return None
    facet = random.choice(facets)
    desc = redact_hero_name(facet.description[:120], facet.hero_name)
    distractors = _pick_distractors(facet.hero_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(facet.hero_name, distractors)
    return TriviaQuestion(
        text=f"'{facet.localized_name}: {desc}' — which hero's facet is this?",
        options=options,
        correct_index=idx,
        difficulty="medium",
        image_url=None,
        category="facet_to_hero",
        explanation=f"'{facet.localized_name}' is a facet for {facet.hero_name}.",
    )


def gen_damage_type() -> TriviaQuestion | None:
    """M7: What damage type is this ability?"""
    dmg_types = {"magical": "Magical", "physical": "Physical", "pure": "Pure"}
    abilities = [a for a in load_abilities() if a.damage_type and a.damage_type.lower() in dmg_types and a.icon_url]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    correct = dmg_types.get(ability.damage_type.lower(), ability.damage_type.title())
    distractors = [v for v in dmg_types.values() if v != correct]
    if len(distractors) < 2:
        return None
    # Pad to 3 if needed
    while len(distractors) < 3:
        distractors.append("None")
    options, idx = _shuffle_options(correct, distractors[:3])
    return TriviaQuestion(
        text=f"What damage type is {ability.localized_name}?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=ability.icon_url,
        category="damage_type",
        explanation=f"{ability.localized_name} deals {correct} damage.",
    )


def gen_shard_upgrade() -> TriviaQuestion | None:
    """M8→H: What does Aghanim's Shard do for this hero?"""
    abilities = [a for a in load_abilities() if a.shard_upgrades and a.shard_description and a.hero_name]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    other = [a for a in abilities if a.hero_name != ability.hero_name and a.shard_description]
    if len(other) < 3:
        return None
    dist_abilities = random.sample(other, 3)
    # Redact hero names from ALL option descriptions to prevent leaks
    correct_desc = redact_hero_name(ability.shard_description[:100], ability.hero_name)
    # Skip if hero name still leaks as a substring (e.g. "Demon" in "Demonic")
    if _hero_name_leaks(correct_desc, ability.hero_name):
        return None
    distractors = [
        redact_hero_name(a.shard_description[:100], a.hero_name)
        for a in dist_abilities
    ]
    options, idx = _shuffle_options(correct_desc, distractors)
    hero = get_hero_by_id(ability.hero_id) if ability.hero_id else None
    return TriviaQuestion(
        text=f"What does Aghanim's Shard do for {ability.hero_name}?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=hero.image_url if hero else None,
        category="shard_upgrade",
        explanation=f"Shard: {ability.shard_description[:120]}",
    )


# ---------------------------------------------------------------------------
# HARD generators (streak 6+)
# ---------------------------------------------------------------------------

def gen_ability_lore() -> TriviaQuestion | None:
    """H1: Which ability has this lore?"""
    abilities = [a for a in load_abilities() if a.lore and len(a.lore) > 20 and a.hero_name and a.icon_url]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    lore = redact_hero_name(ability.lore[:150], ability.hero_name)
    pool = [a.localized_name for a in abilities]
    distractors = _pick_distractors(ability.localized_name, pool)
    if not distractors:
        return None
    options, idx = _shuffle_options(ability.localized_name, distractors)
    return TriviaQuestion(
        text=f'Which ability has this lore?\n"{lore}"',
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=ability.icon_url,
        category="ability_lore",
        explanation=f"This is the lore for {ability.localized_name} ({ability.hero_name}).",
    )


def gen_item_lore() -> TriviaQuestion | None:
    """H2: Which item has this lore?"""
    items = [i for i in load_items() if i.lore and len(i.lore) > 20 and i.icon_url]
    if len(items) < 4:
        return None
    item = random.choice(items)
    pool = [i.localized_name for i in items]
    distractors = _pick_distractors(item.localized_name, pool)
    if not distractors:
        return None
    options, idx = _shuffle_options(item.localized_name, distractors)
    return TriviaQuestion(
        text=f'Which item has this lore?\n"{item.lore[:150]}"',
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=item.icon_url,
        category="item_lore",
        explanation=f"This is the lore for {item.localized_name}.",
    )


def gen_hero_bio() -> TriviaQuestion | None:
    """H3: Which hero has this biography?"""
    heroes = [h for h in load_heroes() if h.bio and len(h.bio) > 50]
    if len(heroes) < 4:
        return None
    hero = random.choice(heroes)
    bio = redact_hero_name(hero.bio[:180], hero.localized_name)
    distractors = _pick_distractors(hero.localized_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text=f'Which hero has this biography?\n"{bio}..."',
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=None,
        category="hero_bio",
        explanation=None,
    )


def gen_voiceline() -> TriviaQuestion | None:
    """H4: Which hero says this quote?"""
    voicelines = load_voicelines()
    if len(voicelines) < 10:
        return None
    vl = random.choice(voicelines)
    hero = get_hero_by_id(vl.hero_id)
    if not hero:
        return None
    # Make sure the voiceline doesn't contain the hero's name
    text = vl.text.strip()
    text = redact_hero_name(text, hero.localized_name)
    # Skip if hero name still leaks after redaction
    if _hero_name_leaks(text, hero.localized_name):
        return None
    distractors = _pick_distractors(hero.localized_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(hero.localized_name, distractors)
    return TriviaQuestion(
        text=f'Which hero says: "{text}"?',
        options=options,
        correct_index=idx,
        difficulty="challenging",
        image_url=None,
        category="voiceline",
        explanation=f"This is a {hero.localized_name} voiceline.",
    )


def gen_base_attack_time() -> TriviaQuestion | None:
    """C1: Which hero has the lowest base attack time?"""
    heroes = [h for h in load_heroes() if h.attack_rate and h.attack_rate > 0]
    if len(heroes) < 4:
        return None
    chosen = random.sample(heroes, 4)
    # Ensure all 4 are within ±0.3 BAT of each other for difficulty
    bats = [h.attack_rate for h in chosen]
    if max(bats) - min(bats) > 0.3:
        # Try to find a tighter cluster
        heroes_sorted = sorted(heroes, key=lambda h: h.attack_rate)
        found = False
        for start in range(len(heroes_sorted) - 3):
            window = heroes_sorted[start:start + 4]
            if window[-1].attack_rate - window[0].attack_rate <= 0.3:
                chosen = list(window)
                random.shuffle(chosen)
                found = True
                break
        if not found:
            return None
    # Need unique BATs for a clear answer
    bats = [h.attack_rate for h in chosen]
    if len(set(bats)) < 2:
        return None
    lowest = min(chosen, key=lambda h: h.attack_rate)
    distractors = [h.localized_name for h in chosen if h != lowest][:3]
    options, idx = _shuffle_options(lowest.localized_name, distractors)
    return TriviaQuestion(
        text="Which of these heroes has the lowest base attack time (BAT)?",
        options=options,
        correct_index=idx,
        difficulty="challenging",
        image_url=None,
        category="base_attack_time",
        explanation=f"{lowest.localized_name} has a {lowest.attack_rate:.2f}s BAT.",
    )


def gen_attribute_gain() -> TriviaQuestion | None:
    """C2: Which [attr] hero has the highest [attr] gain?"""
    attr_map = {
        "strength": ("str", "attr_str_gain"),
        "agility": ("agi", "attr_agi_gain"),
        "intelligence": ("int", "attr_int_gain"),
    }
    attr_key = random.choice(list(attr_map.keys()))
    short_name, gain_field = attr_map[attr_key]
    heroes = [h for h in _heroes_by_attr(attr_key) if getattr(h, gain_field) and getattr(h, gain_field) > 0]
    if len(heroes) < 4:
        return None
    chosen = random.sample(heroes, 4)
    gains = [getattr(h, gain_field) for h in chosen]
    if len(set(gains)) < 2:
        return None
    highest = max(chosen, key=lambda h: getattr(h, gain_field))
    gain_val = getattr(highest, gain_field)
    distractors = [h.localized_name for h in chosen if h != highest][:3]
    options, idx = _shuffle_options(highest.localized_name, distractors)
    return TriviaQuestion(
        text=f"Which {attr_key} hero has the highest {attr_key} gain per level?",
        options=options,
        correct_index=idx,
        difficulty="challenging",
        image_url=None,
        category="attribute_gain",
        explanation=f"{highest.localized_name} gains {gain_val:.1f} {short_name}/level.",
    )


def gen_ability_cooldown() -> TriviaQuestion | None:
    """H5: What is this ability's max-level cooldown?"""
    abilities = [a for a in load_abilities() if a.cooldown and a.icon_url and a.hero_name]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    # Cooldown can be "22 / 18 / 14 / 10" or "20 19 18 17" — take the last value
    cd_str = ability.cooldown.replace("/", " ")
    cd_parts = cd_str.split()
    correct = cd_parts[-1].strip() if cd_parts else ""
    if not correct:
        return None
    # Generate plausible wrong cooldowns
    try:
        cd_val = float(correct)
    except ValueError:
        return None
    if cd_val > 300:
        return None  # sanity check — reject absurd values
    offsets = [-10, -5, -3, 3, 5, 10, 15]
    random.shuffle(offsets)
    wrong = []
    for off in offsets:
        val = cd_val + off
        if val > 0:
            formatted = str(int(val)) if val == int(val) else str(val)
            if formatted != correct and formatted not in wrong:
                wrong.append(formatted)
        if len(wrong) >= 3:
            break
    if len(wrong) < 3:
        return None
    options, idx = _shuffle_options(correct, wrong)
    return TriviaQuestion(
        text=f"What is {ability.localized_name}'s max-level cooldown (seconds)?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=ability.icon_url,
        category="ability_cooldown",
        explanation=f"{ability.localized_name} has a {correct}s cooldown at max level.",
    )


def gen_facet_name() -> TriviaQuestion | None:
    """H6: Which of these is a facet for this hero?"""
    facets = load_facets()
    if len(facets) < 4:
        return None
    facet = random.choice(facets)
    hero = get_hero_by_id(facet.hero_id)
    if not hero:
        return None
    # Use other heroes' facet names as distractors — filter out names that leak their hero
    other_facets = [
        f for f in facets
        if f.hero_id != facet.hero_id
        and not _hero_name_leaks(f.localized_name, f.hero_name or "")
    ]
    if len(other_facets) < 3:
        return None
    dist = random.sample(other_facets, 3)
    distractors = [f.localized_name for f in dist]
    options, idx = _shuffle_options(facet.localized_name, distractors)
    return TriviaQuestion(
        text=f"Which of these is a facet for {hero.localized_name}?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=hero.image_url,
        category="facet_name",
        explanation=f"'{facet.localized_name}' is a facet for {hero.localized_name}.",
    )


def gen_base_armor_compare() -> TriviaQuestion | None:
    """H7: Which hero has the highest base armor?"""
    heroes = [h for h in load_heroes() if h.base_armor is not None]
    if len(heroes) < 4:
        return None
    chosen = random.sample(heroes, 4)
    armors = [h.base_armor for h in chosen]
    if len(set(armors)) < 3:
        return None
    highest = max(chosen, key=lambda h: h.base_armor)
    distractors = [h.localized_name for h in chosen if h != highest][:3]
    options, idx = _shuffle_options(highest.localized_name, distractors)
    return TriviaQuestion(
        text="Which of these heroes has the highest base armor?",
        options=options,
        correct_index=idx,
        difficulty="hard",
        image_url=None,
        category="base_armor_compare",
        explanation=f"{highest.localized_name} has {highest.base_armor} base armor.",
    )


def gen_innate_ability() -> TriviaQuestion | None:
    """H8→M: Which hero has this innate ability?"""
    abilities = [
        a for a in load_abilities()
        if a.innate and a.hero_name
        and not _hero_name_leaks(a.localized_name, a.hero_name)
    ]
    if len(abilities) < 4:
        return None
    ability = random.choice(abilities)
    distractors = _pick_distractors(ability.hero_name, _hero_pool())
    if not distractors:
        return None
    options, idx = _shuffle_options(ability.hero_name, distractors)
    return TriviaQuestion(
        text=f"Which hero has '{ability.localized_name}' as their innate ability?",
        options=options,
        correct_index=idx,
        difficulty="medium",
        image_url=ability.icon_url,
        category="innate_ability",
        explanation=f"'{ability.localized_name}' is {ability.hero_name}'s innate ability.",
    )


def gen_item_by_icon() -> TriviaQuestion | None:
    """E8: Name this item from its icon."""
    # Exclude recipes — they all share the same scroll icon
    items = [i for i in load_items() if i.icon_url and i.localized_name and "Recipe" not in i.localized_name]
    if len(items) < 4:
        return None
    item = random.choice(items)
    pool = [i.localized_name for i in items if i.localized_name != item.localized_name]
    distractors = _pick_distractors(item.localized_name, pool)
    if not distractors:
        return None
    options, idx = _shuffle_options(item.localized_name, distractors)
    return TriviaQuestion(
        text="What is this item?",
        options=options,
        correct_index=idx,
        difficulty="easy",
        image_url=item.icon_url,
        category="item_by_icon",
        explanation=None,
    )


# ---------------------------------------------------------------------------
# Generator registry + selection
# ---------------------------------------------------------------------------

EASY_GENERATORS = [
    gen_hero_by_image,
    gen_primary_attribute,
    gen_ability_to_hero,
    gen_attack_type,
    gen_item_cost_compare,
    gen_neutral_item_tier,
    gen_damage_type,
    gen_ability_by_icon,
    gen_item_by_icon,
]

MEDIUM_GENERATORS = [
    gen_hero_real_name,
    gen_facet_to_hero,
    gen_hero_by_hype,
    gen_innate_ability,
]

HARD_GENERATORS = [
    gen_scepter_upgrade,
    gen_shard_upgrade,
    gen_item_cost_exact,
    gen_ability_lore,
    gen_item_lore,
    gen_hero_bio,
    gen_move_speed,
    gen_ability_cooldown,
    gen_facet_name,
    gen_base_armor_compare,
]

CHALLENGING_GENERATORS = [
    gen_voiceline,
    gen_base_attack_time,
    gen_attribute_gain,
]

# Generators known to produce images — given 2x weight in selection
IMAGE_GENERATORS = {
    gen_hero_by_image,
    gen_primary_attribute,
    gen_ability_to_hero,
    gen_neutral_item_tier,
    gen_damage_type,
    gen_ability_by_icon,
    gen_item_by_icon,
    gen_innate_ability,
    gen_scepter_upgrade,
    gen_shard_upgrade,
    gen_item_cost_exact,
    gen_ability_lore,
    gen_item_lore,
    gen_ability_cooldown,
    gen_facet_name,
}


def _build_weighted(generators: list) -> list:
    """Build a weighted list — image-producing generators get 2x weight."""
    weighted = []
    for gen in generators:
        weighted.append(gen)
        if gen in IMAGE_GENERATORS:
            weighted.append(gen)
    return weighted


def get_difficulty_tier(streak: int) -> str:
    """Return difficulty tier based on streak count."""
    if streak <= 2:
        return "easy"
    elif streak <= 5:
        return "medium"
    elif streak <= 9:
        return "hard"
    else:
        return "challenging"


def generate_question(streak: int, recent_categories: list[str] | None = None, max_retries: int = 15) -> TriviaQuestion | None:
    """
    Generate a trivia question for the given streak level.

    Avoids repeating the last 2 categories.
    Image-producing generators get 2x selection weight.
    """
    tier = get_difficulty_tier(streak)
    if tier == "easy":
        generators = EASY_GENERATORS
    elif tier == "medium":
        generators = MEDIUM_GENERATORS
    elif tier == "hard":
        generators = HARD_GENERATORS
    else:
        generators = CHALLENGING_GENERATORS

    weighted = _build_weighted(generators)
    avoid = set(recent_categories[-2:]) if recent_categories else set()

    for _ in range(max_retries):
        gen = random.choice(weighted)
        q = gen()
        if q is None:
            continue
        if q.category in avoid:
            continue
        return q

    # Fallback: try any generator in the tier without category restriction
    for _ in range(5):
        gen = random.choice(weighted)
        q = gen()
        if q is not None:
            return q

    return None
