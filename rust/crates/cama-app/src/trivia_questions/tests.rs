use super::*;

#[derive(Default)]
struct SequenceRandom {
    next: usize,
}

impl TriviaRandom for SequenceRandom {
    fn next_index(&mut self, upper: usize) -> usize {
        assert!(
            upper > 0,
            "generator cannot select from an empty collection"
        );
        let selected = self.next % upper;
        self.next += 1;
        selected
    }
}

fn random() -> SequenceRandom {
    SequenceRandom::default()
}

fn hero(index: usize) -> HeroData {
    const NAMES: [&str; 12] = [
        "Axe",
        "Crystal Maiden",
        "Juggernaut",
        "Lich",
        "Puck",
        "Invoker",
        "Sven",
        "Lina",
        "Drow Ranger",
        "Earthshaker",
        "Windranger",
        "Zeus",
    ];
    let name = NAMES[index];
    HeroData {
        id: i64::try_from(index + 1).expect("fixture hero id fits i64"),
        localized_name: name.to_owned(),
        real_name: Some(format!("Real Name {index}")),
        hype: Some(format!(
            "{name} is a legendary combatant whose extraordinary story echoes across every lane."
        )),
        bio: Some(format!(
            "Long before this battle, {name} crossed distant lands and mastered an ancient art that changed the course of history forever."
        )),
        primary_attribute: Some("strength".to_owned()),
        is_melee: index < 6,
        base_movement: Some(280 + i64::try_from(index).expect("fixture index fits i64") * 5),
        attack_rate: Some(1.40 + index as f64 * 0.02),
        strength_gain: Some(2.0 + index as f64 * 0.1),
        agility_gain: Some(1.5 + index as f64 * 0.1),
        intelligence_gain: Some(1.0 + index as f64 * 0.1),
        image_url: Some(format!("https://steam.invalid/heroes/{index}.png")),
        armor_at_level_one: Some(2.0 + index as f64),
        night_vision: Some(800 + i64::try_from(index % 6).expect("fixture index fits i64") * 100),
        turn_rate: Some(0.40 + index as f64 * 0.01),
        damage_min_at_level_one: Some(
            30 + i64::try_from(index).expect("fixture index fits i64") * 3,
        ),
        damage_max_at_level_one: Some(
            35 + i64::try_from(index).expect("fixture index fits i64") * 3,
        ),
    }
}

fn ability(index: usize, innate: bool) -> AbilityData {
    const ABILITIES: [&str; 12] = [
        "Berserker's Call",
        "Frostbite",
        "Blade Fury",
        "Frost Shield",
        "Phase Shift",
        "Sun Strike",
        "Storm Hammer",
        "Dragon Slave",
        "Multishot",
        "Fissure",
        "Powershot",
        "Arc Lightning",
    ];
    const HEROES: [&str; 12] = [
        "Axe",
        "Crystal Maiden",
        "Juggernaut",
        "Lich",
        "Puck",
        "Invoker",
        "Sven",
        "Lina",
        "Drow Ranger",
        "Earthshaker",
        "Windranger",
        "Zeus",
    ];
    let n = index % ABILITIES.len();
    AbilityData {
        localized_name: if innate {
            format!("Ancient Gift {n}")
        } else {
            ABILITIES[n].to_owned()
        },
        hero_id: Some(i64::try_from(n + 1).expect("fixture hero id fits i64")),
        hero_name: Some(HEROES[n].to_owned()),
        damage_type: Some(
            match n % 3 {
                0 => "magical",
                1 => "physical",
                _ => "pure",
            }
            .to_owned(),
        ),
        damage: Some(format!("{}", 100 + n * 10)),
        cooldown: Some(format!("20 / 15 / 10 / {}", 5 + n)),
        lore: Some(format!(
            "An old and carefully guarded spell whose hidden origin has been lost to the ages number {n}."
        )),
        scepter_upgrades: true,
        scepter_description: Some(format!(
            "Adds a unique empowered effect number {n} and improves its duration."
        )),
        shard_upgrades: true,
        shard_description: Some(format!(
            "Creates a unique secondary effect number {n} whenever the spell lands."
        )),
        innate,
        icon_url: Some(format!("https://steam.invalid/abilities/{n}.png")),
        mana_cost: Some(format!("50 / 75 / 100 / {}", 125 + n * 10)),
    }
}

fn item(index: usize) -> ItemData {
    ItemData {
        localized_name: format!("Shop Item {index}"),
        cost: Some(500 + i64::try_from(index).expect("fixture index fits i64") * 300),
        lore: Some(format!(
            "A remarkable artifact carried by many heroes throughout a long forgotten age number {index}."
        )),
        neutral_tier: None,
        icon_url: Some(format!("https://steam.invalid/items/{index}.png")),
        special_values: Vec::new(),
        active_cooldown: Some(format!("{}", 10 + index * 5)),
        is_leveled: false,
    }
}

fn catalog() -> TriviaCatalog {
    let mut items: Vec<_> = (0..8).map(item).collect();
    items.extend((1..=5).map(|tier| ItemData {
        localized_name: format!("Neutral Relic {tier}"),
        neutral_tier: Some(tier),
        icon_url: Some(format!("https://steam.invalid/items/neutral-{tier}.png")),
        lore: Some(format!(
            "A strange neutral artifact with a sufficiently detailed history for tier {tier}."
        )),
        ..ItemData::default()
    }));
    items.push(ItemData {
        localized_name: "Dagon 1".to_owned(),
        icon_url: Some("https://steam.invalid/items/dagon.png".to_owned()),
        special_values: vec![ItemSpecialValue {
            key: "damage".to_owned(),
            header: "Damage".to_owned(),
            value: "400 / 500 / 600 / 700 / 800".to_owned(),
        }],
        is_leveled: true,
        ..ItemData::default()
    });

    let heroes: Vec<_> = (0..12).map(hero).collect();
    let mut abilities: Vec<_> = (0..12).map(|index| ability(index, false)).collect();
    abilities.extend((0..4).map(|index| ability(index, true)));
    let voicelines = (0..12)
        .map(|index| VoicelineData {
            hero_id: i64::from(index + 1),
            text: format!("The battle begins and our enemies will remember this moment {index}."),
        })
        .collect();
    TriviaCatalog {
        heroes,
        abilities,
        items,
        voicelines,
    }
}

fn validate(question: &TriviaQuestion) {
    assert!(question.is_valid());
    assert!(matches!(
        question.difficulty,
        Difficulty::Easy | Difficulty::Medium | Difficulty::Hard | Difficulty::Challenging
    ));
}

fn generated(generator: Generator) -> TriviaQuestion {
    let mut random = random();
    let question = generator
        .generate(&catalog(), &mut random)
        .expect("complete fixture supports every registered generator");
    validate(&question);
    question
}

#[test]
fn test_easy() {
    assert_eq!(get_difficulty_tier(0), Difficulty::Easy);
    assert_eq!(get_difficulty_tier(1), Difficulty::Easy);
    assert_eq!(get_difficulty_tier(2), Difficulty::Easy);
}

#[test]
fn test_medium() {
    assert_eq!(get_difficulty_tier(3), Difficulty::Medium);
    assert_eq!(get_difficulty_tier(4), Difficulty::Medium);
    assert_eq!(get_difficulty_tier(5), Difficulty::Medium);
}

#[test]
fn test_hard() {
    assert_eq!(get_difficulty_tier(6), Difficulty::Hard);
    assert_eq!(get_difficulty_tier(9), Difficulty::Hard);
}

#[test]
fn test_challenging() {
    assert_eq!(get_difficulty_tier(10), Difficulty::Challenging);
    assert_eq!(get_difficulty_tier(100), Difficulty::Challenging);
}

#[test]
fn test_obvious_leak() {
    assert!(hero_name_leaks("Phantom Rush", "Phantom Lancer"));
}

#[test]
fn test_no_leak() {
    assert!(!hero_name_leaks("Blink Strike", "Phantom Lancer"));
}

#[test]
fn test_short_words_ignored() {
    assert!(!hero_name_leaks("Cup of Tea", "Keeper of the Light"));
}

#[test]
fn test_case_insensitive() {
    assert!(hero_name_leaks("puck shot", "Puck"));
    assert_eq!(redact_hero_name("⚔ PUCK returns", "Puck"), "⚔ ??? returns");
}

#[test]
fn test_single_word_hero() {
    assert!(hero_name_leaks("Puckish", "Puck"));
}

#[test]
fn test_hero_by_image() {
    let question = generated(Generator::HeroByImage);
    assert_eq!(question.difficulty, Difficulty::Easy);
    assert!(question.image_url.is_some());
}

#[test]
fn test_primary_attribute() {
    assert_eq!(
        generated(Generator::PrimaryAttribute).difficulty,
        Difficulty::Easy
    );
}

#[test]
fn test_ability_to_hero() {
    assert_eq!(
        generated(Generator::AbilityToHero).difficulty,
        Difficulty::Easy
    );
}

#[test]
fn test_attack_type() {
    let question = generated(Generator::AttackType);
    assert_eq!(question.difficulty, Difficulty::Easy);
    assert!(question.text.contains("melee") || question.text.contains("ranged"));
}

#[test]
fn test_item_cost_compare() {
    assert_eq!(
        generated(Generator::ItemCostCompare).difficulty,
        Difficulty::Easy
    );
}

#[test]
fn test_neutral_item_tier() {
    assert_eq!(
        generated(Generator::NeutralItemTier).difficulty,
        Difficulty::Easy
    );
}

#[test]
fn test_damage_type() {
    assert_eq!(
        generated(Generator::DamageType).difficulty,
        Difficulty::Easy
    );
}

#[test]
fn test_ability_by_icon() {
    let question = generated(Generator::AbilityByIcon);
    assert_eq!(question.difficulty, Difficulty::Easy);
    assert!(question.image_url.is_some());
}

#[test]
fn test_item_by_icon() {
    let question = generated(Generator::ItemByIcon);
    assert_eq!(question.difficulty, Difficulty::Easy);
    assert!(question.image_url.is_some());
    assert_eq!(question.category, "item_by_icon");
}

#[test]
fn test_hero_real_name() {
    assert_eq!(
        generated(Generator::HeroRealName).difficulty,
        Difficulty::Medium
    );
}

#[test]
fn test_hero_by_hype() {
    assert_eq!(
        generated(Generator::HeroByHype).difficulty,
        Difficulty::Medium
    );
}

#[test]
fn test_innate_ability() {
    let question = generated(Generator::InnateAbility);
    assert_eq!(question.difficulty, Difficulty::Medium);
    assert!(question.image_url.is_some());
}

#[test]
fn test_voiceline_with_image() {
    let question = generated(Generator::VoicelineWithImage);
    assert_eq!(question.difficulty, Difficulty::Medium);
    assert_eq!(question.category, "voiceline_with_image");
    assert!(question.image_url.is_some());
}

#[test]
fn test_scepter_upgrade() {
    assert_eq!(
        generated(Generator::ScepterUpgrade).difficulty,
        Difficulty::Hard
    );
}

#[test]
fn test_scepter_upgrade_skips_leaking_descriptions() {
    let entries = [
        (
            "Shadow Demon",
            "Demonic Purge",
            "Causes Demonic Purge to break and gain charges.",
        ),
        (
            "Axe",
            "Berserker's Call",
            "Applies Battle Hunger to taunted enemies.",
        ),
        (
            "Crystal Maiden",
            "Frostbite",
            "Creates a freezing explosion around the target.",
        ),
        (
            "Juggernaut",
            "Blade Fury",
            "Allows movement and increases the slash rate.",
        ),
        (
            "Lich",
            "Frost Shield",
            "Adds a damaging nova when the shield expires.",
        ),
    ];
    let abilities = entries
        .into_iter()
        .map(|(hero, name, description)| AbilityData {
            localized_name: name.to_owned(),
            hero_name: Some(hero.to_owned()),
            scepter_upgrades: true,
            scepter_description: Some(description.to_owned()),
            ..AbilityData::default()
        })
        .collect();
    let fixture = TriviaCatalog {
        abilities,
        ..TriviaCatalog::default()
    };
    let question = gen_scepter_upgrade(&fixture, &mut random()).expect("four safe upgrades remain");
    validate(&question);
    assert_eq!(question.text, "What does Aghanim's Scepter do for Axe?");
    assert!(!question.text.contains("Shadow Demon"));
}

#[test]
fn test_shard_upgrade() {
    assert_eq!(
        generated(Generator::ShardUpgrade).difficulty,
        Difficulty::Hard
    );
}

#[test]
fn test_item_cost_exact() {
    assert_eq!(
        generated(Generator::ItemCostExact).difficulty,
        Difficulty::Hard
    );
}

#[test]
fn test_ability_lore() {
    let question = generated(Generator::AbilityLore);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert!(question.image_url.is_some());
}

#[test]
fn test_item_lore() {
    let question = generated(Generator::ItemLore);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert!(question.image_url.is_some());
}

#[test]
fn test_hero_bio() {
    assert_eq!(generated(Generator::HeroBio).difficulty, Difficulty::Hard);
}

#[test]
fn test_move_speed() {
    assert_eq!(generated(Generator::MoveSpeed).difficulty, Difficulty::Hard);
}

#[test]
fn test_ability_cooldown() {
    assert_eq!(
        generated(Generator::AbilityCooldown).difficulty,
        Difficulty::Hard
    );
}

#[test]
fn test_armor_at_level1_compare() {
    let question = generated(Generator::ArmorAtLevelOneCompare);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert_eq!(question.category, "armor_at_level1_compare");
    assert!(question.text.contains("level 1"));
}

#[test]
fn test_ability_mana_cost() {
    let question = generated(Generator::AbilityManaCost);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert_eq!(question.category, "ability_mana_cost");
    assert!(question.image_url.is_some());
}

#[test]
fn test_item_active_cooldown() {
    let question = generated(Generator::ItemActiveCooldown);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert_eq!(question.category, "item_active_cooldown");
    assert!(question.image_url.is_some());
}

#[test]
fn test_damage_at_level1_compare() {
    let fixture = catalog();
    let mut random = random();
    let question =
        gen_damage_at_level_one_compare(&fixture, &mut random).expect("strict winner exists");
    validate(&question);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert_eq!(question.category, "damage_at_level1_compare");
    let top_name = &question.options[question.correct_index];
    let top = fixture
        .heroes
        .iter()
        .find(|hero| &hero.localized_name == top_name)
        .expect("answer names a fixture hero");
    for option in question.options.iter().filter(|option| *option != top_name) {
        let other = fixture
            .heroes
            .iter()
            .find(|hero| &hero.localized_name == option)
            .expect("distractor names a fixture hero");
        assert!(top.damage_min_at_level_one > other.damage_min_at_level_one);
        assert!(top.damage_max_at_level_one > other.damage_max_at_level_one);
    }
}

#[test]
fn test_dagon_damage_by_level() {
    let question = generated(Generator::DagonDamageByLevel);
    assert_eq!(question.difficulty, Difficulty::Hard);
    assert_eq!(question.category, "dagon_damage_by_level");
    assert!(question.image_url.is_some());
    let level: i64 = question
        .text
        .trim_end_matches('?')
        .split_whitespace()
        .last()
        .expect("prompt includes level")
        .parse()
        .expect("level is numeric");
    assert_eq!(
        question.options[question.correct_index],
        (400 + 100 * (level - 1)).to_string()
    );
}

#[test]
fn test_voiceline() {
    assert_eq!(
        generated(Generator::Voiceline).difficulty,
        Difficulty::Challenging
    );
}

#[test]
fn test_base_attack_time() {
    let question = generated(Generator::BaseAttackTime);
    assert_eq!(question.difficulty, Difficulty::Challenging);
    assert_eq!(question.category, "base_attack_time");
    assert!(question.text.contains("BAT"));
}

#[test]
fn test_attribute_gain() {
    let question = generated(Generator::AttributeGain);
    assert_eq!(question.difficulty, Difficulty::Challenging);
    assert_eq!(question.category, "attribute_gain");
    assert!(question.text.contains("gain per level"));
}

#[test]
fn test_night_vision_compare() {
    let question = generated(Generator::NightVisionCompare);
    assert_eq!(question.difficulty, Difficulty::Challenging);
    assert_eq!(question.category, "night_vision_compare");
    assert!(question.image_url.is_some());
    for option in question.options {
        option.parse::<i64>().expect("vision choices are integers");
    }
}

#[test]
fn test_turn_rate() {
    let question = generated(Generator::TurnRate);
    assert_eq!(question.difficulty, Difficulty::Challenging);
    assert_eq!(question.category, "turn_rate");
    assert!(question.image_url.is_some());
    for option in question.options {
        option
            .parse::<f64>()
            .expect("turn-rate choices are numeric");
    }
}

#[test]
fn test_ability_to_hero_no_hero_name_leak() {
    let fixture = catalog();
    let mut random = random();
    for _ in 0..50 {
        let question =
            gen_ability_to_hero(&fixture, &mut random).expect("fixture has safe abilities");
        let hero = &question.options[question.correct_index];
        let ability = question
            .text
            .split('\'')
            .nth(1)
            .expect("ability prompt uses quotes");
        for word in hero.split_whitespace().filter(|word| word.len() > 2) {
            assert!(!ability.to_lowercase().contains(&word.to_lowercase()));
        }
    }
}

#[test]
fn test_scepter_descriptions_redacted() {
    let fixture = catalog();
    let mut random = random();
    for _ in 0..30 {
        let question = gen_scepter_upgrade(&fixture, &mut random).expect("fixture has upgrades");
        let hero = question
            .text
            .split(" for ")
            .last()
            .expect("upgrade prompt names hero")
            .trim_end_matches('?');
        let answer = &question.options[question.correct_index];
        for word in hero.split_whitespace().filter(|word| word.len() > 2) {
            assert!(!answer.to_lowercase().contains(&word.to_lowercase()));
        }
    }
}

#[test]
fn test_shard_descriptions_redacted() {
    let fixture = catalog();
    let mut random = random();
    for _ in 0..30 {
        let question = gen_shard_upgrade(&fixture, &mut random).expect("fixture has upgrades");
        let hero = question
            .text
            .split(" for ")
            .last()
            .expect("upgrade prompt names hero")
            .trim_end_matches('?');
        let answer = &question.options[question.correct_index];
        for word in hero.split_whitespace().filter(|word| word.len() > 2) {
            assert!(!answer.to_lowercase().contains(&word.to_lowercase()));
        }
    }
}

#[test]
fn test_image_generators_set_accurate() {
    let all = EASY_GENERATORS
        .into_iter()
        .chain(MEDIUM_GENERATORS)
        .chain(HARD_GENERATORS)
        .chain(CHALLENGING_GENERATORS);
    for generator in all {
        let question = generated(generator);
        assert_eq!(
            generator.produces_image(),
            IMAGE_GENERATORS.contains(&generator)
        );
        if generator.produces_image() {
            assert!(
                question.image_url.is_some(),
                "{generator:?} is image-weighted and must carry image metadata"
            );
        }
    }
}

fn generate_for(streak: i64) -> TriviaQuestion {
    generate_question(&catalog(), streak, &[], 15, &mut random())
        .expect("complete fixture supports tier selection")
}

#[test]
fn test_easy_streak() {
    assert_eq!(generate_for(0).difficulty, Difficulty::Easy);
}

#[test]
fn test_medium_streak() {
    assert_eq!(generate_for(4).difficulty, Difficulty::Medium);
}

#[test]
fn test_hard_streak() {
    assert_eq!(generate_for(7).difficulty, Difficulty::Hard);
}

#[test]
fn test_challenging_streak() {
    assert_eq!(generate_for(10).difficulty, Difficulty::Challenging);
}

#[test]
fn test_avoids_recent_categories() {
    let fixture = catalog();
    let mut random = random();
    let recent = ["older_category", "hero_by_image"];
    let question = generate_question(&fixture, 0, &recent, 15, &mut random)
        .expect("another easy generator is available during bounded retries");
    assert!(!recent.contains(&question.category));
}

#[test]
fn test_all_generators_registered() {
    assert_eq!(EASY_GENERATORS.len(), 9);
    assert_eq!(MEDIUM_GENERATORS.len(), 4);
    assert_eq!(HARD_GENERATORS.len(), 13);
    assert_eq!(CHALLENGING_GENERATORS.len(), 5);
    assert_eq!(
        EASY_GENERATORS
            .into_iter()
            .chain(MEDIUM_GENERATORS)
            .chain(HARD_GENERATORS)
            .chain(CHALLENGING_GENERATORS)
            .collect::<HashSet<_>>()
            .len(),
        31
    );
}
