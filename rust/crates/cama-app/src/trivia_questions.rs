//! Transport-independent Dota trivia question generation.
//!
//! The live Python service currently loads these records from Dotabase and uses
//! Python's `random` module.  This additive Rust slice intentionally keeps both
//! concerns behind inputs: callers supply a [`TriviaCatalog`] and an injected
//! [`TriviaRandom`] implementation.  That makes the selection, answer-redaction,
//! image metadata, and difficulty contracts deterministic without creating a
//! second Dotabase or Discord runtime during the side-by-side migration.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
    Challenging,
}

impl Difficulty {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
            Self::Challenging => "challenging",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriviaQuestion {
    pub text: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub difficulty: Difficulty,
    pub image_url: Option<String>,
    pub category: &'static str,
    pub explanation: Option<String>,
}

impl TriviaQuestion {
    /// Structural invariant shared by every Python question test.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.options.len() == 4
            && self.correct_index < self.options.len()
            && !self.options[self.correct_index].is_empty()
            && !self.category.is_empty()
            && !self.text.is_empty()
            && self.options.iter().collect::<HashSet<_>>().len() == 4
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeroData {
    pub id: i64,
    pub localized_name: String,
    pub real_name: Option<String>,
    pub hype: Option<String>,
    pub bio: Option<String>,
    pub primary_attribute: Option<String>,
    pub is_melee: bool,
    pub base_movement: Option<i64>,
    pub attack_rate: Option<f64>,
    pub strength_gain: Option<f64>,
    pub agility_gain: Option<f64>,
    pub intelligence_gain: Option<f64>,
    pub image_url: Option<String>,
    pub armor_at_level_one: Option<f64>,
    pub night_vision: Option<i64>,
    pub turn_rate: Option<f64>,
    pub damage_min_at_level_one: Option<i64>,
    pub damage_max_at_level_one: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AbilityData {
    pub localized_name: String,
    pub hero_id: Option<i64>,
    pub hero_name: Option<String>,
    pub damage_type: Option<String>,
    pub damage: Option<String>,
    pub cooldown: Option<String>,
    pub lore: Option<String>,
    pub scepter_upgrades: bool,
    pub scepter_description: Option<String>,
    pub shard_upgrades: bool,
    pub shard_description: Option<String>,
    pub innate: bool,
    pub icon_url: Option<String>,
    pub mana_cost: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ItemSpecialValue {
    pub key: String,
    pub header: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ItemData {
    pub localized_name: String,
    pub cost: Option<i64>,
    pub lore: Option<String>,
    pub neutral_tier: Option<i64>,
    pub icon_url: Option<String>,
    pub special_values: Vec<ItemSpecialValue>,
    pub active_cooldown: Option<String>,
    pub is_leveled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VoicelineData {
    pub hero_id: i64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriviaCatalog {
    pub heroes: Vec<HeroData>,
    pub abilities: Vec<AbilityData>,
    pub items: Vec<ItemData>,
    pub voicelines: Vec<VoicelineData>,
}

/// Minimal injectable randomness boundary. `next_index` must return a value in
/// `0..upper`; all generator sampling and shuffling flows through this trait.
pub trait TriviaRandom {
    fn next_index(&mut self, upper: usize) -> usize;
}

fn pick<'a, T>(items: &[&'a T], random: &mut impl TriviaRandom) -> Option<&'a T> {
    (!items.is_empty()).then(|| items[random.next_index(items.len())].to_owned())
}

fn sample_refs<'a, T>(
    mut items: Vec<&'a T>,
    count: usize,
    random: &mut impl TriviaRandom,
) -> Option<Vec<&'a T>> {
    if items.len() < count {
        return None;
    }
    let mut selected = Vec::with_capacity(count);
    for _ in 0..count {
        selected.push(items.remove(random.next_index(items.len())));
    }
    Some(selected)
}

fn sample_strings(
    correct: &str,
    pool: impl IntoIterator<Item = String>,
    count: usize,
    random: &mut impl TriviaRandom,
) -> Option<Vec<String>> {
    let mut candidates = Vec::new();
    for candidate in pool {
        if candidate != correct && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    if candidates.len() < count {
        return None;
    }
    let mut selected = Vec::with_capacity(count);
    for _ in 0..count {
        selected.push(candidates.remove(random.next_index(candidates.len())));
    }
    Some(selected)
}

fn shuffle<T>(values: &mut [T], random: &mut impl TriviaRandom) {
    for upper in (2..=values.len()).rev() {
        values.swap(upper - 1, random.next_index(upper));
    }
}

fn options(
    correct: String,
    distractors: Vec<String>,
    random: &mut impl TriviaRandom,
) -> Option<(Vec<String>, usize)> {
    if distractors.len() != 3 {
        return None;
    }
    let mut result = Vec::with_capacity(4);
    result.push(correct.clone());
    result.extend(distractors);
    shuffle(&mut result, random);
    let index = result.iter().position(|value| value == &correct)?;
    Some((result, index))
}

#[must_use]
pub fn hero_name_leaks(entity_name: &str, hero_name: &str) -> bool {
    let entity = entity_name.to_lowercase();
    hero_name
        .split_whitespace()
        .any(|word| word.chars().count() > 2 && entity.contains(&word.to_lowercase()))
}

fn replace_case_insensitive(text: &str, needle: &str, whole_word: bool) -> String {
    if needle.is_empty() || !needle.is_ascii() {
        return text.to_owned();
    }
    let lower = text.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(&needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        let left_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let right_ok = end == lower.len() || !lower.as_bytes()[end].is_ascii_alphanumeric();
        if !whole_word || (left_ok && right_ok) {
            output.push_str(&text[cursor..start]);
            output.push_str("???");
            cursor = end;
        } else {
            output.push_str(&text[cursor..end]);
            cursor = end;
        }
    }
    output.push_str(&text[cursor..]);
    output
}

#[must_use]
pub fn redact_hero_name(text: &str, hero_name: &str) -> String {
    let mut redacted = replace_case_insensitive(text, hero_name, false);
    for word in hero_name.split_whitespace().filter(|word| word.len() > 2) {
        redacted = replace_case_insensitive(&redacted, word, true);
    }
    redacted
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn hero_pool(catalog: &TriviaCatalog) -> impl Iterator<Item = String> + '_ {
    catalog
        .heroes
        .iter()
        .map(|hero| hero.localized_name.clone())
}

#[expect(
    clippy::too_many_arguments,
    reason = "centralizes the seven Python TriviaQuestion fields plus injected randomness"
)]
fn question(
    text: String,
    answer: String,
    distractors: Vec<String>,
    difficulty: Difficulty,
    image_url: Option<String>,
    category: &'static str,
    explanation: Option<String>,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let (options, correct_index) = options(answer, distractors, random)?;
    let result = TriviaQuestion {
        text,
        options,
        correct_index,
        difficulty,
        image_url,
        category,
        explanation,
    };
    result.is_valid().then_some(result)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Generator {
    HeroByImage,
    PrimaryAttribute,
    AbilityToHero,
    AttackType,
    ItemCostCompare,
    NeutralItemTier,
    DamageType,
    AbilityByIcon,
    ItemByIcon,
    HeroRealName,
    HeroByHype,
    InnateAbility,
    VoicelineWithImage,
    ScepterUpgrade,
    ShardUpgrade,
    ItemCostExact,
    AbilityLore,
    ItemLore,
    HeroBio,
    MoveSpeed,
    AbilityCooldown,
    ArmorAtLevelOneCompare,
    AbilityManaCost,
    ItemActiveCooldown,
    DamageAtLevelOneCompare,
    DagonDamageByLevel,
    Voiceline,
    BaseAttackTime,
    AttributeGain,
    NightVisionCompare,
    TurnRate,
}

pub const EASY_GENERATORS: [Generator; 9] = [
    Generator::HeroByImage,
    Generator::PrimaryAttribute,
    Generator::AbilityToHero,
    Generator::AttackType,
    Generator::ItemCostCompare,
    Generator::NeutralItemTier,
    Generator::DamageType,
    Generator::AbilityByIcon,
    Generator::ItemByIcon,
];
pub const MEDIUM_GENERATORS: [Generator; 4] = [
    Generator::HeroRealName,
    Generator::HeroByHype,
    Generator::InnateAbility,
    Generator::VoicelineWithImage,
];
pub const HARD_GENERATORS: [Generator; 13] = [
    Generator::ScepterUpgrade,
    Generator::ShardUpgrade,
    Generator::ItemCostExact,
    Generator::AbilityLore,
    Generator::ItemLore,
    Generator::HeroBio,
    Generator::MoveSpeed,
    Generator::AbilityCooldown,
    Generator::ArmorAtLevelOneCompare,
    Generator::AbilityManaCost,
    Generator::ItemActiveCooldown,
    Generator::DamageAtLevelOneCompare,
    Generator::DagonDamageByLevel,
];
pub const CHALLENGING_GENERATORS: [Generator; 5] = [
    Generator::Voiceline,
    Generator::BaseAttackTime,
    Generator::AttributeGain,
    Generator::NightVisionCompare,
    Generator::TurnRate,
];
pub const IMAGE_GENERATORS: [Generator; 20] = [
    Generator::HeroByImage,
    Generator::PrimaryAttribute,
    Generator::AbilityToHero,
    Generator::NeutralItemTier,
    Generator::DamageType,
    Generator::AbilityByIcon,
    Generator::ItemByIcon,
    Generator::InnateAbility,
    Generator::ScepterUpgrade,
    Generator::ShardUpgrade,
    Generator::ItemCostExact,
    Generator::AbilityLore,
    Generator::ItemLore,
    Generator::AbilityCooldown,
    Generator::AbilityManaCost,
    Generator::ItemActiveCooldown,
    Generator::DagonDamageByLevel,
    Generator::TurnRate,
    Generator::VoicelineWithImage,
    Generator::NightVisionCompare,
];

impl Generator {
    #[must_use]
    pub const fn produces_image(self) -> bool {
        matches!(
            self,
            Self::HeroByImage
                | Self::PrimaryAttribute
                | Self::AbilityToHero
                | Self::NeutralItemTier
                | Self::DamageType
                | Self::AbilityByIcon
                | Self::ItemByIcon
                | Self::InnateAbility
                | Self::ScepterUpgrade
                | Self::ShardUpgrade
                | Self::ItemCostExact
                | Self::AbilityLore
                | Self::ItemLore
                | Self::AbilityCooldown
                | Self::AbilityManaCost
                | Self::ItemActiveCooldown
                | Self::DagonDamageByLevel
                | Self::TurnRate
                | Self::VoicelineWithImage
                | Self::NightVisionCompare
        )
    }

    pub fn generate(
        self,
        catalog: &TriviaCatalog,
        random: &mut impl TriviaRandom,
    ) -> Option<TriviaQuestion> {
        match self {
            Self::HeroByImage => gen_hero_by_image(catalog, random),
            Self::PrimaryAttribute => gen_primary_attribute(catalog, random),
            Self::AbilityToHero => gen_ability_to_hero(catalog, random),
            Self::AttackType => gen_attack_type(catalog, random),
            Self::ItemCostCompare => gen_item_cost_compare(catalog, random),
            Self::NeutralItemTier => gen_neutral_item_tier(catalog, random),
            Self::DamageType => gen_damage_type(catalog, random),
            Self::AbilityByIcon => gen_ability_by_icon(catalog, random),
            Self::ItemByIcon => gen_item_by_icon(catalog, random),
            Self::HeroRealName => gen_hero_real_name(catalog, random),
            Self::HeroByHype => gen_hero_by_hype(catalog, random),
            Self::InnateAbility => gen_innate_ability(catalog, random),
            Self::VoicelineWithImage => gen_voiceline_with_image(catalog, random),
            Self::ScepterUpgrade => gen_scepter_upgrade(catalog, random),
            Self::ShardUpgrade => gen_shard_upgrade(catalog, random),
            Self::ItemCostExact => gen_item_cost_exact(catalog, random),
            Self::AbilityLore => gen_ability_lore(catalog, random),
            Self::ItemLore => gen_item_lore(catalog, random),
            Self::HeroBio => gen_hero_bio(catalog, random),
            Self::MoveSpeed => gen_move_speed(catalog, random),
            Self::AbilityCooldown => gen_ability_cooldown(catalog, random),
            Self::ArmorAtLevelOneCompare => gen_armor_at_level_one_compare(catalog, random),
            Self::AbilityManaCost => gen_ability_mana_cost(catalog, random),
            Self::ItemActiveCooldown => gen_item_active_cooldown(catalog, random),
            Self::DamageAtLevelOneCompare => gen_damage_at_level_one_compare(catalog, random),
            Self::DagonDamageByLevel => gen_dagon_damage_by_level(catalog, random),
            Self::Voiceline => gen_voiceline(catalog, random),
            Self::BaseAttackTime => gen_base_attack_time(catalog, random),
            Self::AttributeGain => gen_attribute_gain(catalog, random),
            Self::NightVisionCompare => gen_night_vision_compare(catalog, random),
            Self::TurnRate => gen_turn_rate(catalog, random),
        }
    }
}

#[must_use]
pub const fn get_difficulty_tier(streak: i64) -> Difficulty {
    if streak <= 2 {
        Difficulty::Easy
    } else if streak <= 5 {
        Difficulty::Medium
    } else if streak <= 9 {
        Difficulty::Hard
    } else {
        Difficulty::Challenging
    }
}

pub fn gen_hero_by_image(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.image_url.is_some())
        .collect();
    if heroes.len() < 4 {
        return None;
    }
    let hero = pick(&heroes, random)?;
    let distractors = sample_strings(&hero.localized_name, hero_pool(catalog), 3, random)?;
    question(
        "Who is this hero?".to_owned(),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Easy,
        hero.image_url.clone(),
        "hero_by_image",
        None,
        random,
    )
}

pub fn gen_primary_attribute(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let display = [
        ("strength", "Strength"),
        ("agility", "Agility"),
        ("intelligence", "Intelligence"),
        ("universal", "Universal"),
    ];
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| {
            hero.image_url.is_some()
                && hero
                    .primary_attribute
                    .as_deref()
                    .is_some_and(|value| display.iter().any(|(key, _)| value == *key))
        })
        .collect();
    let hero = pick(&heroes, random)?;
    let primary = hero.primary_attribute.as_deref()?;
    let correct = display
        .iter()
        .find(|(key, _)| *key == primary)?
        .1
        .to_owned();
    let distractors = display
        .iter()
        .filter(|(_, value)| *value != correct)
        .map(|(_, value)| (*value).to_owned())
        .take(3)
        .collect();
    question(
        format!("What is {}'s primary attribute?", hero.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Easy,
        hero.image_url.clone(),
        "primary_attribute",
        Some(format!("{} is a {correct} hero.", hero.localized_name)),
        random,
    )
}

pub fn gen_ability_to_hero(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            !ability.innate
                && ability.icon_url.is_some()
                && ability
                    .hero_name
                    .as_deref()
                    .is_some_and(|hero| !hero_name_leaks(&ability.localized_name, hero))
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let hero = ability.hero_name.clone()?;
    let distractors = sample_strings(&hero, hero_pool(catalog), 3, random)?;
    question(
        format!("Which hero has the ability '{}'?", ability.localized_name),
        hero.clone(),
        distractors,
        Difficulty::Easy,
        ability.icon_url.clone(),
        "ability_to_hero",
        Some(format!("'{}' belongs to {hero}.", ability.localized_name)),
        random,
    )
}

pub fn gen_attack_type(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let is_melee = random.next_index(2) == 0;
    let matching: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.is_melee == is_melee)
        .collect();
    let other_names = catalog
        .heroes
        .iter()
        .filter(|hero| hero.is_melee != is_melee)
        .map(|hero| hero.localized_name.clone());
    let hero = pick(&matching, random)?;
    let distractors = sample_strings(&hero.localized_name, other_names, 3, random)?;
    let label = if is_melee { "melee" } else { "ranged" };
    question(
        format!("Which of these heroes is {label}?"),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Easy,
        None,
        "attack_type",
        Some(format!("{} is a {label} hero.", hero.localized_name)),
        random,
    )
}

pub fn gen_item_cost_compare(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let eligible: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| item.cost.is_some_and(|cost| cost > 0) && item.neutral_tier.is_none())
        .collect();
    let selected = sample_refs(eligible, 4, random)?;
    if selected
        .iter()
        .filter_map(|item| item.cost)
        .collect::<HashSet<_>>()
        .len()
        < 3
    {
        return None;
    }
    let winner = selected.iter().max_by_key(|item| item.cost)?.to_owned();
    let distractors = selected
        .iter()
        .filter(|item| item.localized_name != winner.localized_name)
        .map(|item| item.localized_name.clone())
        .take(3)
        .collect();
    question(
        "Which of these items costs the most?".to_owned(),
        winner.localized_name.clone(),
        distractors,
        Difficulty::Easy,
        None,
        "item_cost_compare",
        Some(format!(
            "{} costs {} gold.",
            winner.localized_name, winner.cost?
        )),
        random,
    )
}

pub fn gen_neutral_item_tier(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let neutrals: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| item.neutral_tier.is_some() && item.icon_url.is_some())
        .collect();
    if neutrals.len() < 4 {
        return None;
    }
    let item = pick(&neutrals, random)?;
    let tier = item.neutral_tier?;
    let correct = format!("Tier {tier}");
    let tiers = neutrals
        .iter()
        .filter_map(|candidate| candidate.neutral_tier)
        .map(|value| format!("Tier {value}"));
    let distractors = sample_strings(&correct, tiers, 3, random)?;
    question(
        format!("What tier neutral item is {}?", item.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Easy,
        item.icon_url.clone(),
        "neutral_item_tier",
        Some(format!(
            "{} is a {correct} neutral item.",
            item.localized_name
        )),
        random,
    )
}

pub fn gen_damage_type(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            ability.icon_url.is_some()
                && ability.damage.is_some()
                && ability.damage_type.as_deref().is_some_and(|kind| {
                    matches!(
                        kind.to_ascii_lowercase().as_str(),
                        "magical" | "physical" | "pure"
                    )
                })
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let correct = match ability
        .damage_type
        .as_deref()?
        .to_ascii_lowercase()
        .as_str()
    {
        "magical" => "Magical",
        "physical" => "Physical",
        "pure" => "Pure",
        _ => return None,
    }
    .to_owned();
    let mut distractors: Vec<_> = ["Magical", "Physical", "Pure"]
        .into_iter()
        .filter(|value| *value != correct)
        .map(str::to_owned)
        .collect();
    distractors.push("None".to_owned());
    question(
        format!("What damage type is {}?", ability.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Easy,
        ability.icon_url.clone(),
        "damage_type",
        Some(format!(
            "{} deals {correct} damage.",
            ability.localized_name
        )),
        random,
    )
}

pub fn gen_ability_by_icon(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            !ability.innate && ability.icon_url.is_some() && ability.hero_name.is_some()
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let distractors = sample_strings(
        &ability.localized_name,
        abilities.iter().map(|item| item.localized_name.clone()),
        3,
        random,
    )?;
    question(
        "Name this ability.".to_owned(),
        ability.localized_name.clone(),
        distractors,
        Difficulty::Easy,
        ability.icon_url.clone(),
        "ability_by_icon",
        Some(format!(
            "This is {} ({}).",
            ability.localized_name,
            ability.hero_name.as_deref()?
        )),
        random,
    )
}

pub fn gen_item_by_icon(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let items: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| {
            item.icon_url.is_some() && !item.localized_name.contains("Recipe") && !item.is_leveled
        })
        .collect();
    if items.len() < 4 {
        return None;
    }
    let item = pick(&items, random)?;
    let distractors = sample_strings(
        &item.localized_name,
        items.iter().map(|other| other.localized_name.clone()),
        3,
        random,
    )?;
    question(
        "What is this item?".to_owned(),
        item.localized_name.clone(),
        distractors,
        Difficulty::Easy,
        item.icon_url.clone(),
        "item_by_icon",
        None,
        random,
    )
}

pub fn gen_hero_real_name(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.real_name.is_some())
        .collect();
    if heroes.len() < 4 {
        return None;
    }
    let hero = pick(&heroes, random)?;
    let real_name = hero.real_name.as_deref()?;
    let distractors = sample_strings(&hero.localized_name, hero_pool(catalog), 3, random)?;
    question(
        format!("Which hero's real name is '{real_name}'?"),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Medium,
        None,
        "hero_real_name",
        Some(format!(
            "{}'s real name is {real_name}.",
            hero.localized_name
        )),
        random,
    )
}

pub fn gen_hero_by_hype(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.hype.as_ref().is_some_and(|hype| hype.len() > 30))
        .collect();
    if heroes.len() < 4 {
        return None;
    }
    let hero = pick(&heroes, random)?;
    let excerpt = redact_hero_name(&truncate(hero.hype.as_deref()?, 150), &hero.localized_name);
    if !excerpt.contains("???")
        && excerpt
            .to_lowercase()
            .contains(&hero.localized_name.to_lowercase())
    {
        return None;
    }
    let distractors = sample_strings(&hero.localized_name, hero_pool(catalog), 3, random)?;
    question(
        format!("Which hero: \"{excerpt}...\""),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Medium,
        None,
        "hero_by_hype",
        None,
        random,
    )
}

pub fn gen_innate_ability(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            ability.innate
                && ability
                    .hero_name
                    .as_deref()
                    .is_some_and(|hero| !hero_name_leaks(&ability.localized_name, hero))
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let hero = ability.hero_name.clone()?;
    let distractors = sample_strings(&hero, hero_pool(catalog), 3, random)?;
    question(
        format!(
            "Which hero has '{}' as their innate ability?",
            ability.localized_name
        ),
        hero.clone(),
        distractors,
        Difficulty::Medium,
        ability.icon_url.clone(),
        "innate_ability",
        Some(format!(
            "'{}' is {hero}'s innate ability.",
            ability.localized_name
        )),
        random,
    )
}

fn eligible_upgrades(catalog: &TriviaCatalog, scepter: bool) -> Vec<(&AbilityData, String)> {
    catalog
        .abilities
        .iter()
        .filter_map(|ability| {
            let enabled = if scepter {
                ability.scepter_upgrades
            } else {
                ability.shard_upgrades
            };
            let description = if scepter {
                ability.scepter_description.as_deref()
            } else {
                ability.shard_description.as_deref()
            }?;
            let hero = ability.hero_name.as_deref()?;
            if !enabled {
                return None;
            }
            let redacted = redact_hero_name(&truncate(description, 100), hero);
            (!hero_name_leaks(&redacted, hero)).then_some((ability, redacted))
        })
        .collect()
}

fn gen_upgrade(
    catalog: &TriviaCatalog,
    scepter: bool,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let eligible = eligible_upgrades(catalog, scepter);
    if eligible.len() < 4 {
        return None;
    }
    let chosen = &eligible[random.next_index(eligible.len())];
    let hero_name = chosen.0.hero_name.as_deref()?;
    let candidates = eligible
        .iter()
        .filter(|(ability, _)| ability.hero_name.as_deref() != Some(hero_name))
        .map(|(_, description)| description.clone());
    let distractors = sample_strings(&chosen.1, candidates, 3, random)?;
    let hero_image = chosen.0.hero_id.and_then(|id| {
        catalog
            .heroes
            .iter()
            .find(|hero| hero.id == id)
            .and_then(|hero| hero.image_url.clone())
    });
    let (artifact, category, raw) = if scepter {
        (
            "Scepter",
            "scepter_upgrade",
            chosen.0.scepter_description.as_deref()?,
        )
    } else {
        (
            "Shard",
            "shard_upgrade",
            chosen.0.shard_description.as_deref()?,
        )
    };
    question(
        format!("What does Aghanim's {artifact} do for {hero_name}?"),
        chosen.1.clone(),
        distractors,
        Difficulty::Hard,
        hero_image,
        category,
        Some(format!("{artifact}: {}", truncate(raw, 120))),
        random,
    )
}

pub fn gen_scepter_upgrade(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    gen_upgrade(catalog, true, random)
}

pub fn gen_shard_upgrade(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    gen_upgrade(catalog, false, random)
}

fn offset_options(
    correct_text: &str,
    correct_value: f64,
    offsets: &[f64],
    random: &mut impl TriviaRandom,
) -> Option<Vec<String>> {
    let mut shuffled = offsets.to_vec();
    shuffle(&mut shuffled, random);
    let mut wrong = Vec::new();
    for offset in shuffled {
        let value = correct_value + offset;
        if value <= 0.0 {
            continue;
        }
        let formatted = format_number(value);
        if formatted != correct_text && !wrong.contains(&formatted) {
            wrong.push(formatted);
        }
        if wrong.len() == 3 {
            return Some(wrong);
        }
    }
    None
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn last_numeric(raw: &str) -> Option<(String, f64)> {
    let text = raw.replace('/', " ");
    let value = text.split_whitespace().last()?;
    Some((value.to_owned(), value.parse().ok()?))
}

pub fn gen_item_cost_exact(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let items: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| {
            item.cost.is_some_and(|cost| cost > 0)
                && item.neutral_tier.is_none()
                && item.icon_url.is_some()
        })
        .collect();
    if items.len() < 4 {
        return None;
    }
    let item = pick(&items, random)?;
    let cost = item.cost?;
    let correct = cost.to_string();
    let distractors = offset_options(
        &correct,
        cost as f64,
        &[-500.0, -300.0, -200.0, -100.0, 100.0, 200.0, 300.0, 500.0],
        random,
    )?;
    question(
        format!("How much does {} cost?", item.localized_name),
        correct,
        distractors,
        Difficulty::Hard,
        item.icon_url.clone(),
        "item_cost_exact",
        Some(format!("{} costs {cost} gold.", item.localized_name)),
        random,
    )
}

pub fn gen_ability_lore(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            ability.lore.as_ref().is_some_and(|lore| lore.len() > 20)
                && ability.hero_name.is_some()
                && ability.icon_url.is_some()
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let hero = ability.hero_name.as_deref()?;
    let lore = redact_hero_name(&truncate(ability.lore.as_deref()?, 150), hero);
    let distractors = sample_strings(
        &ability.localized_name,
        abilities.iter().map(|item| item.localized_name.clone()),
        3,
        random,
    )?;
    question(
        format!("Which ability has this lore?\n\"{lore}\""),
        ability.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        ability.icon_url.clone(),
        "ability_lore",
        Some(format!(
            "This is the lore for {} ({hero}).",
            ability.localized_name
        )),
        random,
    )
}

pub fn gen_item_lore(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let items: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| {
            item.lore.as_ref().is_some_and(|lore| lore.len() > 20)
                && item.icon_url.is_some()
                && !item.is_leveled
        })
        .collect();
    if items.len() < 4 {
        return None;
    }
    let item = pick(&items, random)?;
    let distractors = sample_strings(
        &item.localized_name,
        items.iter().map(|other| other.localized_name.clone()),
        3,
        random,
    )?;
    question(
        format!(
            "Which item has this lore?\n\"{}\"",
            truncate(item.lore.as_deref()?, 150)
        ),
        item.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        item.icon_url.clone(),
        "item_lore",
        Some(format!("This is the lore for {}.", item.localized_name)),
        random,
    )
}

pub fn gen_hero_bio(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.bio.as_ref().is_some_and(|bio| bio.len() > 50))
        .collect();
    if heroes.len() < 4 {
        return None;
    }
    let hero = pick(&heroes, random)?;
    let bio = redact_hero_name(&truncate(hero.bio.as_deref()?, 180), &hero.localized_name);
    let distractors = sample_strings(&hero.localized_name, hero_pool(catalog), 3, random)?;
    question(
        format!("Which hero has this biography?\n\"{bio}...\""),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        None,
        "hero_bio",
        None,
        random,
    )
}

fn selected_hero_comparison<'a>(
    heroes: Vec<&'a HeroData>,
    score: impl Fn(&HeroData) -> Option<f64>,
    random: &mut impl TriviaRandom,
) -> Option<(&'a HeroData, Vec<String>)> {
    let selected = sample_refs(heroes, 4, random)?;
    let winner = selected.iter().max_by(|left, right| {
        score(left)
            .unwrap_or_default()
            .total_cmp(&score(right).unwrap_or_default())
    })?;
    let best = score(winner)?;
    if selected
        .iter()
        .filter(|hero| score(hero) == Some(best))
        .count()
        > 1
    {
        return None;
    }
    let distractors = selected
        .iter()
        .filter(|hero| hero.localized_name != winner.localized_name)
        .map(|hero| hero.localized_name.clone())
        .take(3)
        .collect();
    Some((winner, distractors))
}

pub fn gen_move_speed(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let eligible = catalog
        .heroes
        .iter()
        .filter(|hero| hero.base_movement.is_some_and(|value| value > 0))
        .collect();
    let (hero, distractors) = selected_hero_comparison(
        eligible,
        |item| item.base_movement.map(|value| value as f64),
        random,
    )?;
    question(
        "Which of these heroes has the highest base move speed?".to_owned(),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        None,
        "move_speed",
        Some(format!(
            "{} has {} base move speed.",
            hero.localized_name, hero.base_movement?
        )),
        random,
    )
}

pub fn gen_ability_cooldown(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            ability.cooldown.is_some() && ability.icon_url.is_some() && ability.hero_name.is_some()
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let (correct, value) = last_numeric(ability.cooldown.as_deref()?)?;
    if value > 300.0 {
        return None;
    }
    let distractors = offset_options(
        &correct,
        value,
        &[-10.0, -5.0, -3.0, 3.0, 5.0, 10.0, 15.0],
        random,
    )?;
    question(
        format!(
            "What is {}'s max-level cooldown (seconds)?",
            ability.localized_name
        ),
        correct.clone(),
        distractors,
        Difficulty::Hard,
        ability.icon_url.clone(),
        "ability_cooldown",
        Some(format!(
            "{} has a {correct}s cooldown at max level.",
            ability.localized_name
        )),
        random,
    )
}

pub fn gen_armor_at_level_one_compare(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let eligible = catalog
        .heroes
        .iter()
        .filter(|hero| hero.armor_at_level_one.is_some())
        .collect();
    let (hero, distractors) =
        selected_hero_comparison(eligible, |item| item.armor_at_level_one, random)?;
    question(
        "Which of these heroes has the highest armor at level 1?".to_owned(),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        None,
        "armor_at_level1_compare",
        Some(format!(
            "{} has {:.1} armor at level 1.",
            hero.localized_name, hero.armor_at_level_one?
        )),
        random,
    )
}

pub fn gen_ability_mana_cost(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let abilities: Vec<_> = catalog
        .abilities
        .iter()
        .filter(|ability| {
            ability.mana_cost.is_some() && ability.icon_url.is_some() && ability.hero_name.is_some()
        })
        .collect();
    if abilities.len() < 4 {
        return None;
    }
    let ability = pick(&abilities, random)?;
    let (correct, value) = last_numeric(ability.mana_cost.as_deref()?)?;
    if value > 600.0 {
        return None;
    }
    let distractors = offset_options(
        &correct,
        value,
        &[-50.0, -25.0, -15.0, 15.0, 25.0, 50.0, 75.0],
        random,
    )?;
    question(
        format!("What is {}'s max-level mana cost?", ability.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Hard,
        ability.icon_url.clone(),
        "ability_mana_cost",
        Some(format!(
            "{} costs {correct} mana at max level.",
            ability.localized_name
        )),
        random,
    )
}

pub fn gen_item_active_cooldown(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let items: Vec<_> = catalog
        .items
        .iter()
        .filter(|item| {
            item.active_cooldown.is_some()
                && item.icon_url.is_some()
                && item.neutral_tier.is_none()
                && !item.is_leveled
        })
        .collect();
    if items.len() < 4 {
        return None;
    }
    let item = pick(&items, random)?;
    let (correct, value) = last_numeric(item.active_cooldown.as_deref()?)?;
    if value > 300.0 {
        return None;
    }
    let distractors = offset_options(
        &correct,
        value,
        &[-20.0, -10.0, -5.0, 5.0, 10.0, 20.0, 30.0],
        random,
    )?;
    question(
        format!(
            "What is {}'s active cooldown (seconds)?",
            item.localized_name
        ),
        correct.clone(),
        distractors,
        Difficulty::Hard,
        item.icon_url.clone(),
        "item_active_cooldown",
        Some(format!(
            "{} has a {correct}s cooldown.",
            item.localized_name
        )),
        random,
    )
}

pub fn gen_damage_at_level_one_compare(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| {
            hero.damage_min_at_level_one.is_some_and(|value| value != 0)
                && hero.damage_max_at_level_one.is_some_and(|value| value != 0)
        })
        .collect();
    let selected = sample_refs(heroes, 4, random)?;
    let top = selected
        .iter()
        .max_by_key(|hero| hero.damage_max_at_level_one)?
        .to_owned();
    let top_min = top.damage_min_at_level_one?;
    let top_max = top.damage_max_at_level_one?;
    if selected.iter().any(|hero| {
        hero.localized_name != top.localized_name
            && (hero
                .damage_min_at_level_one
                .is_some_and(|value| value >= top_min)
                || hero
                    .damage_max_at_level_one
                    .is_some_and(|value| value >= top_max))
    }) {
        return None;
    }
    let distractors = selected
        .iter()
        .filter(|hero| hero.localized_name != top.localized_name)
        .map(|hero| hero.localized_name.clone())
        .take(3)
        .collect();
    question(
        "Which of these heroes has the highest attack damage at level 1?".to_owned(),
        top.localized_name.clone(),
        distractors,
        Difficulty::Hard,
        None,
        "damage_at_level1_compare",
        Some(format!(
            "{} has {top_min}–{top_max} attack damage at level 1.",
            top.localized_name
        )),
        random,
    )
}

pub fn gen_dagon_damage_by_level(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let dagon = catalog
        .items
        .iter()
        .find(|item| item.is_leveled && item.localized_name.starts_with("Dagon"))?;
    let raw = dagon.special_values.iter().find(|special| {
        format!("{} {}", special.key, special.header)
            .to_ascii_lowercase()
            .contains("damage")
    })?;
    let values: Vec<_> = raw
        .value
        .replace('/', " ")
        .split_whitespace()
        .filter(|value| value.chars().all(|character| character.is_ascii_digit()))
        .map(str::to_owned)
        .collect();
    if values.len() < 4 {
        return None;
    }
    let level = random.next_index(values.len()) + 1;
    let correct = values[level - 1].clone();
    let distractors = sample_strings(&correct, values, 3, random)?;
    question(
        format!("How much magic damage does Dagon deal at level {level}?"),
        correct.clone(),
        distractors,
        Difficulty::Hard,
        dagon.icon_url.clone(),
        "dagon_damage_by_level",
        Some(format!(
            "Dagon {level} deals {correct} magic damage on cast."
        )),
        random,
    )
}

fn pick_voiceline<'a>(
    catalog: &'a TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<(&'a HeroData, String)> {
    if catalog.voicelines.len() < 10 {
        return None;
    }
    let line = &catalog.voicelines[random.next_index(catalog.voicelines.len())];
    let hero = catalog.heroes.iter().find(|hero| hero.id == line.hero_id)?;
    let text = redact_hero_name(line.text.trim(), &hero.localized_name);
    (!hero_name_leaks(&text, &hero.localized_name)).then_some((hero, text))
}

fn gen_voiceline_common(
    catalog: &TriviaCatalog,
    with_image: bool,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let (hero, text) = pick_voiceline(catalog, random)?;
    if with_image && hero.image_url.is_none() {
        return None;
    }
    let distractors = sample_strings(&hero.localized_name, hero_pool(catalog), 3, random)?;
    question(
        format!("Which hero says: \"{text}\"?"),
        hero.localized_name.clone(),
        distractors,
        if with_image {
            Difficulty::Medium
        } else {
            Difficulty::Challenging
        },
        with_image.then(|| hero.image_url.clone()).flatten(),
        if with_image {
            "voiceline_with_image"
        } else {
            "voiceline"
        },
        Some(format!("This is a {} voiceline.", hero.localized_name)),
        random,
    )
}

pub fn gen_voiceline_with_image(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    gen_voiceline_common(catalog, true, random)
}

pub fn gen_voiceline(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    gen_voiceline_common(catalog, false, random)
}

pub fn gen_base_attack_time(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let mut eligible: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.attack_rate.is_some_and(|value| value > 0.0))
        .collect();
    if eligible.len() < 4 {
        return None;
    }
    let mut selected = sample_refs(eligible.clone(), 4, random)?;
    let min = selected
        .iter()
        .filter_map(|hero| hero.attack_rate)
        .min_by(f64::total_cmp)?;
    let max = selected
        .iter()
        .filter_map(|hero| hero.attack_rate)
        .max_by(f64::total_cmp)?;
    if max - min > 0.3 {
        eligible.sort_by(|left, right| {
            left.attack_rate
                .unwrap_or_default()
                .total_cmp(&right.attack_rate.unwrap_or_default())
        });
        selected = eligible
            .windows(4)
            .find(|window| {
                window[3].attack_rate.unwrap_or_default()
                    - window[0].attack_rate.unwrap_or_default()
                    <= 0.3
            })?
            .to_vec();
        shuffle(&mut selected, random);
    }
    let lowest = selected.iter().min_by(|left, right| {
        left.attack_rate
            .unwrap_or_default()
            .total_cmp(&right.attack_rate.unwrap_or_default())
    })?;
    let rate = lowest.attack_rate?;
    if selected
        .iter()
        .filter(|hero| hero.attack_rate == Some(rate))
        .count()
        > 1
    {
        return None;
    }
    let distractors = selected
        .iter()
        .filter(|hero| hero.localized_name != lowest.localized_name)
        .map(|hero| hero.localized_name.clone())
        .take(3)
        .collect();
    question(
        "Which of these heroes has the lowest base attack time (BAT)?".to_owned(),
        lowest.localized_name.clone(),
        distractors,
        Difficulty::Challenging,
        None,
        "base_attack_time",
        Some(format!("{} has a {rate:.2}s BAT.", lowest.localized_name)),
        random,
    )
}

pub fn gen_attribute_gain(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let (attribute, short) = match random.next_index(3) {
        0 => ("strength", "str"),
        1 => ("agility", "agi"),
        _ => ("intelligence", "int"),
    };
    let gain = |hero: &HeroData| match attribute {
        "strength" => hero.strength_gain,
        "agility" => hero.agility_gain,
        _ => hero.intelligence_gain,
    };
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| {
            hero.primary_attribute.as_deref() == Some(attribute)
                && gain(hero).is_some_and(|value| value > 0.0)
        })
        .collect();
    let (hero, distractors) = selected_hero_comparison(heroes, gain, random)?;
    let value = gain(hero)?;
    question(
        format!("Which {attribute} hero has the highest {attribute} gain per level?"),
        hero.localized_name.clone(),
        distractors,
        Difficulty::Challenging,
        None,
        "attribute_gain",
        Some(format!(
            "{} gains {value:.1} {short}/level.",
            hero.localized_name
        )),
        random,
    )
}

pub fn gen_night_vision_compare(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.night_vision.is_some_and(|value| value > 0))
        .collect();
    let mut values: Vec<_> = heroes.iter().filter_map(|hero| hero.night_vision).collect();
    values.sort_unstable();
    values.dedup();
    if values.len() < 4 {
        return None;
    }
    let default = values[0];
    let notable: Vec<_> = heroes
        .into_iter()
        .filter(|hero| hero.night_vision != Some(default))
        .collect();
    let hero = pick(&notable, random)?;
    let correct = hero.night_vision?.to_string();
    let distractors = sample_strings(
        &correct,
        values.into_iter().map(|value| value.to_string()),
        3,
        random,
    )?;
    question(
        format!("What is {}'s night vision range?", hero.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Challenging,
        hero.image_url.clone(),
        "night_vision_compare",
        Some(format!(
            "{} has {correct} night vision.",
            hero.localized_name
        )),
        random,
    )
}

pub fn gen_turn_rate(
    catalog: &TriviaCatalog,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let heroes: Vec<_> = catalog
        .heroes
        .iter()
        .filter(|hero| hero.turn_rate.is_some_and(|value| value > 0.0))
        .collect();
    let mut rates: Vec<_> = heroes.iter().filter_map(|hero| hero.turn_rate).collect();
    rates.sort_by(f64::total_cmp);
    rates.dedup();
    if rates.len() < 4 {
        return None;
    }
    let hero = pick(&heroes, random)?;
    let rate = hero.turn_rate?;
    let correct = format!("{rate:.2}");
    let distractors = sample_strings(
        &correct,
        rates.into_iter().map(|value| format!("{value:.2}")),
        3,
        random,
    )?;
    question(
        format!("What is {}'s turn rate?", hero.localized_name),
        correct.clone(),
        distractors,
        Difficulty::Challenging,
        hero.image_url.clone(),
        "turn_rate",
        Some(format!(
            "{} has a turn rate of {correct}.",
            hero.localized_name
        )),
        random,
    )
}

fn registry_for(difficulty: Difficulty) -> &'static [Generator] {
    match difficulty {
        Difficulty::Easy => &EASY_GENERATORS,
        Difficulty::Medium => &MEDIUM_GENERATORS,
        Difficulty::Hard => &HARD_GENERATORS,
        Difficulty::Challenging => &CHALLENGING_GENERATORS,
    }
}

fn weighted(generators: &[Generator]) -> Vec<Generator> {
    let mut result = Vec::new();
    for generator in generators {
        result.push(*generator);
        if generator.produces_image() {
            result.push(*generator);
        }
    }
    result
}

pub fn generate_question(
    catalog: &TriviaCatalog,
    streak: i64,
    recent_categories: &[&str],
    max_retries: usize,
    random: &mut impl TriviaRandom,
) -> Option<TriviaQuestion> {
    let generators = weighted(registry_for(get_difficulty_tier(streak)));
    let avoid: HashSet<_> = recent_categories.iter().rev().take(2).copied().collect();
    for _ in 0..max_retries {
        let generator = generators[random.next_index(generators.len())];
        if let Some(generated) = generator.generate(catalog, random)
            && !avoid.contains(generated.category)
        {
            return Some(generated);
        }
    }
    for _ in 0..5 {
        let generator = generators[random.next_index(generators.len())];
        if let Some(generated) = generator.generate(catalog, random) {
            return Some(generated);
        }
    }
    None
}

#[cfg(test)]
mod tests;
