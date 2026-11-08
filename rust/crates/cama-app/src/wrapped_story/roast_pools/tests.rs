use std::collections::BTreeSet;

use cama_domain::wrapped_activity::ServerRank;
use cama_domain::wrapped_distinct::{
    Direction, DistinctStory, METRIC_KEYS, PairStory, TRACKED_ITEMS, Theme, Tier, metric_directions,
};

use super::*;

const TIERS: [Tier; 3] = [Tier::Extreme, Tier::Strong, Tier::Notable];

/// Metrics in the core themes (Combat, Farm, Clutch, Chaos, Heroes), which
/// need the deepest pools.
const CORE: &[&str] = &[
    "deaths_per_game",
    "kills_per_game",
    "best_game",
    "kill_loss",
    "gpm",
    "last_hits",
    "hero_damage",
    "healing",
    "stuns",
    "win_rate",
    "upset_win",
    "upset_loss",
    "win_streak",
    "loss_streak",
    "rating_swing",
    "pings",
    "apm",
    "throw",
    "comeback",
    "rapiers",
    "hero_loyal",
    "hero_pool",
];

/// The widest value each placeholder takes on a real card.
const WORST_CASE: &[(&str, &str)] = &[
    ("n", "23,400"),
    ("hero", "Keeper of the Light"),
    ("name", "Alexandria"),
    ("rank", "187"),
    ("of", "187"),
    ("time", "12:59 AM"),
    ("item", "Black King Bar"),
];

/// The longest name a line may carry inline.
const LONG_NAME: &str = "Maximilianus";

fn worst_case(template: &str, name: &str) -> String {
    let mut filled = template.replace("{name}", name);
    for (key, value) in WORST_CASE {
        filled = filled.replace(&format!("{{{key}}}"), value);
    }
    assert!(!filled.contains('{'), "unknown placeholder in {template:?}");
    filled
}

fn story(metric: &'static str, theme: Theme, direction: Direction, tier: Tier) -> DistinctStory {
    DistinctStory {
        metric,
        theme,
        direction,
        tier,
        value: 9.4,
        display: "9.4".to_owned(),
        rank: None,
        hero_id: None,
        match_id: None,
        item: None,
        score: 1.0,
    }
}

fn vars() -> CopyVars {
    CopyVars::default()
}

fn copy_text(copy: &StoryCopy) -> String {
    [
        &copy.setup,
        &copy.value,
        &copy.label,
        &copy.punch,
        &copy.credit,
    ]
    .iter()
    .map(|part| part.as_str())
    .collect::<Vec<_>>()
    .join(" | ")
}

fn longest_digit_run(text: &str) -> usize {
    text.split(|character: char| !character.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0)
}

#[test]
fn every_catalog_metric_has_both_tiers_of_copy() {
    for key in METRIC_KEYS {
        let directions = metric_directions(key);
        assert!(!directions.is_empty(), "{key} has no directions");
        for direction in directions {
            let deep = if CORE.contains(key) { 6 } else { 3 };
            for (tier, minimum) in [
                (Tier::Extreme, deep),
                (Tier::Strong, deep),
                (Tier::Notable, 3),
            ] {
                let lines = pool(key, *direction, tier);
                assert!(
                    lines.len() >= minimum,
                    "{key} {direction:?} {tier:?}: {} lines, want {minimum}",
                    lines.len()
                );
            }
        }
    }
    for item in TRACKED_ITEMS {
        assert!(item_pool(item).len() >= 3, "{item} item lines");
    }
}

#[test]
fn meme_cards_vary_their_setups_and_keep_the_meme() {
    for key in METRIC_KEYS {
        for direction in metric_directions(key) {
            let meme = treatment(key, *direction);
            if meme == Punch::Plain {
                continue;
            }
            for tier in TIERS {
                let lines = pool(key, *direction, tier);
                let setups: BTreeSet<_> = lines.iter().map(|beat| beat.setup).collect();
                assert!(setups.len() >= 6, "{key} {tier:?}: {} setups", setups.len());
                assert_canonical(meme, lines, key);
            }
        }
    }
    for key in ["deaths_per_game", "gpm"] {
        assert_canonical(
            Punch::SkillIssue,
            pool(key, Direction::Roast, Tier::Extreme),
            key,
        );
    }
    for key in SIDE_GAME_KEYS {
        let meme = treatment(key, Direction::Roast);
        if meme == Punch::Plain {
            continue;
        }
        for tier in TIERS {
            assert_canonical(meme, side_pool(key, tier), key);
        }
    }
}

fn assert_canonical(meme: Punch, lines: &[Beat], key: &str) {
    let (phrase, drawn): (&str, &[&str]) = match meme {
        Punch::Cinema => ("cinema", &[]),
        Punch::GoldGraph => ("this is fine", &["gg"]),
        Punch::Stonks => ("stonks", &[]),
        Punch::NotStonks => ("not stonks", &[]),
        Punch::Calculated => ("calculated", &["win chance"]),
        Punch::TaskFailed => ("task failed successfully", &["defeat"]),
        Punch::LossTiles => ("go next", &[]),
        Punch::PressF => ("f", &[]),
        Punch::SkillIssue => ("skill issue", &["top", "bottom", "you"]),
        Punch::Clock => ("one more game", &[]),
        Punch::Plain => unreachable!(),
    };
    let punches: BTreeSet<_> = lines.iter().map(|beat| beat.punch).collect();
    assert!(
        punches.len() <= 3,
        "{key}: {punches:?} strays from the meme"
    );
    for punch in punches {
        let lower = punch.to_lowercase();
        let words: Vec<&str> = lower
            .split(|character: char| !character.is_alphanumeric())
            .collect();
        // Short phrases and labels match whole words: "f" is not in "off".
        let says = |text: &str| {
            if text.len() < 4 {
                words.contains(&text)
            } else {
                lower.contains(text)
            }
        };
        assert!(says(phrase), "{key}: {punch:?} is not the {meme:?} meme");
        for label in drawn {
            assert!(!says(label), "{key}: {punch:?} repeats the drawn {label:?}");
        }
    }
}

#[test]
fn lines_respect_beat_limits() {
    let beats = all_beats();
    assert!(beats.len() >= 500, "only {} beats", beats.len());
    let mut long = BTreeSet::new();
    for beat in beats {
        for name in ["Alexandria", LONG_NAME] {
            let setup = worst_case(beat.setup, name);
            let punch = worst_case(beat.punch, name);
            if setup.chars().count() > 28 {
                long.insert(format!("setup {}: {setup}", setup.chars().count()));
            }
            if punch.chars().count() > 30 {
                long.insert(format!("punch {}: {punch}", punch.chars().count()));
            }
            assert!(!setup.trim().is_empty() && !punch.trim().is_empty());
        }
    }
    assert!(long.is_empty(), "{long:#?}");
}

#[test]
fn every_story_pool_has_lines_that_need_no_extras() {
    let optional = ["{hero}", "{name}", "{rank}", "{of}", "{time}", "{item}"];
    for key in METRIC_KEYS {
        for direction in metric_directions(key) {
            for tier in TIERS {
                let plain = pool(key, *direction, tier)
                    .iter()
                    .filter(|beat| {
                        optional
                            .iter()
                            .all(|slot| !beat.setup.contains(slot) && !beat.punch.contains(slot))
                    })
                    .count();
                assert!(plain >= 3, "{key} {direction:?} {tier:?}: {plain} plain");
            }
        }
    }
}

#[test]
fn missing_name_skips_name_lines() {
    let pair = PairStory {
        discord_id: 123_456_789_012_345_678,
        games: 11,
        wins: 3,
    };
    for kind in PEOPLE_KINDS {
        for name in [
            None,
            Some("123456789012345678"),
            Some("<@123456789>"),
            Some("Unknown player"),
            Some("  "),
        ] {
            for seed in 0..64 {
                let copy = people_copy(kind, &pair, name, seed);
                let text = copy_text(&copy);
                assert!(!copy.setup.is_empty() && !copy.punch.is_empty(), "{kind}");
                assert!(!text.contains('{'), "{kind}: {text}");
                assert!(!text.contains("Unknown player"), "{kind}: {text}");
                assert!(!text.contains("<@"), "{kind}: {text}");
                assert!(longest_digit_run(&text) <= 4, "{kind}: {text}");
            }
        }
    }
}

#[test]
fn readable_names_reach_people_cards() {
    let pair = PairStory {
        discord_id: 42,
        games: 11,
        wins: 8,
    };
    for kind in PEOPLE_KINDS {
        let named = (0..64)
            .map(|seed| people_copy(kind, &pair, Some("Alexandria"), seed))
            .filter(|copy| copy.setup.contains("Alexandria") || copy.punch.contains("Alexandria"))
            .count();
        assert!(named > 0, "{kind} never says the name");
        let copy = people_copy(kind, &pair, Some("Alexandria"), 7);
        assert!(
            copy.credit.contains("Alexandria"),
            "{kind}: {}",
            copy.credit
        );
        let long = "A name far too long to fit a beat";
        for seed in 0..64 {
            let copy = people_copy(kind, &pair, Some(long), seed);
            assert!(!copy.setup.contains(long) && !copy.punch.contains(long));
        }
    }
}

#[test]
fn people_facts_are_the_pair_record() {
    let pair = PairStory {
        discord_id: 42,
        games: 11,
        wins: 3,
    };
    assert_eq!(people_copy("duo", &pair, None, 1).value, "3");
    assert_eq!(people_copy("cursed_duo", &pair, None, 1).value, "27%");
    assert_eq!(people_copy("nemesis", &pair, None, 1).value, "8");
    assert_eq!(people_copy("punching_bag", &pair, None, 1).value, "3");
    assert_eq!(
        people_copy("nemesis", &pair, Some("Alexandria"), 1).credit,
        "Won 3 of 11 against Alexandria"
    );
    assert_eq!(people_copy("mystery", &pair, None, 1), StoryCopy::default());
}

#[test]
fn people_pools_are_deep() {
    for kind in PEOPLE_KINDS {
        let lines = people_pool(kind);
        assert!(lines.len() >= 6, "{kind}: {}", lines.len());
        let nameless = lines
            .iter()
            .filter(|beat| !beat.setup.contains("{name}") && !beat.punch.contains("{name}"))
            .count();
        assert!(nameless >= 6, "{kind}: {nameless} lines without a name");
    }
}

#[test]
fn no_line_targets_personal_life() {
    const BANNED: [&str; 13] = [
        "hobbies",
        "social life",
        "girlfriend",
        "boyfriend",
        "wife",
        "husband",
        "your job",
        "ugly",
        "fat ",
        "stupid",
        "dumb",
        "mom",
        "dad",
    ];
    let mut lines: Vec<&str> = all_beats()
        .into_iter()
        .flat_map(|beat| [beat.setup, beat.punch])
        .collect();
    lines.extend(PERSONAS.iter().flat_map(|(_, _, titles)| {
        titles
            .iter()
            .flat_map(|(title, subtitle)| [*title, *subtitle])
    }));
    lines.extend(
        FALLBACK_PERSONAS
            .iter()
            .flat_map(|(title, subtitle)| [*title, *subtitle]),
    );
    lines.extend(
        super::super::FLAVOR_POOLS
            .iter()
            .flat_map(|(_, pool)| pool.iter().copied()),
    );
    for line in lines {
        let lower = line.to_lowercase();
        for banned in BANNED {
            assert!(!lower.contains(banned), "{line:?} mentions {banned:?}");
        }
    }
}

#[test]
fn copy_is_deterministic_and_varies_by_seed() {
    let deaths = story(
        "deaths_per_game",
        Theme::Combat,
        Direction::Roast,
        Tier::Strong,
    );
    assert_eq!(
        story_copy(&deaths, &vars(), 99),
        story_copy(&deaths, &vars(), 99)
    );
    let punches: BTreeSet<String> = (0..20)
        .map(|seed| story_copy(&deaths, &vars(), seed).punch)
        .collect();
    assert!(punches.len() >= 4, "{punches:?}");
    let real_seeds: BTreeSet<String> = (0..20)
        .map(|player| {
            let seed = super::super::chapters::story_seed(100_000_000_000_000_000 + player, 2026);
            story_copy(&deaths, &vars(), seed).punch
        })
        .collect();
    assert!(real_seeds.len() >= 4, "{real_seeds:?}");
}

#[test]
fn story_copy_fills_facts_and_never_leaks_placeholders() {
    for key in METRIC_KEYS {
        for direction in metric_directions(key) {
            for tier in TIERS {
                let mut bare = story(key, Theme::Clutch, *direction, tier);
                bare.item = (*key == "item").then(|| "magic_wand".to_owned());
                for seed in 0..24 {
                    let copy = story_copy(&bare, &vars(), seed);
                    let text = copy_text(&copy);
                    assert!(!text.contains('{'), "{key}: {text}");
                    assert!(!text.contains('#'), "{key} claims a rank: {text}");
                    assert_eq!(copy.value, "9.4");
                    assert!(!copy.setup.is_empty() && !copy.punch.is_empty());
                    assert!(!copy.label.is_empty() && !copy.credit.is_empty(), "{key}");
                }
            }
        }
    }
}

#[test]
fn ranks_and_heroes_show_up_when_known() {
    let mut best = story("gpm", Theme::Farm, Direction::Popoff, Tier::Extreme);
    best.rank = Some(ServerRank { rank: 1, of: 187 });
    let ranked = (0..64)
        .map(|seed| story_copy(&best, &vars(), seed))
        .filter(|copy| copy.setup.contains("#1") || copy.punch.contains("#1"))
        .count();
    assert!(ranked > 0, "rank lines never used");
    assert!(story_copy(&best, &vars(), 3).credit.ends_with("#1 of 187"));

    let mut worst = story(
        "deaths_per_game",
        Theme::Combat,
        Direction::Roast,
        Tier::Strong,
    );
    worst.rank = Some(ServerRank { rank: 3, of: 187 });
    assert!(
        story_copy(&worst, &vars(), 3)
            .credit
            .ends_with("3rd from last of 187")
    );
    worst.rank = Some(ServerRank { rank: 1, of: 187 });
    assert!(
        story_copy(&worst, &vars(), 3)
            .credit
            .ends_with("Dead last of 187")
    );

    let mut cinema = story("best_game", Theme::Combat, Direction::Popoff, Tier::Strong);
    cinema.display = "27 / 2 / 14".to_owned();
    let with_hero = CopyVars {
        hero: Some("Queen of Pain".to_owned()),
        ..CopyVars::default()
    };
    let copy = story_copy(&cinema, &with_hero, 5);
    assert_eq!(copy.credit, "27 / 2 / 14 · Queen of Pain");
    assert_eq!(copy.label, "Queen of Pain");
    assert_eq!(copy.treatment, Punch::Cinema);
}

#[test]
fn treatments_follow_the_story() {
    use Direction::{Popoff, Roast};
    assert_eq!(treatment("best_game", Popoff), Punch::Cinema);
    assert_eq!(treatment("throw", Roast), Punch::GoldGraph);
    assert_eq!(treatment("upset_win", Popoff), Punch::Calculated);
    assert_eq!(treatment("kill_loss", Roast), Punch::TaskFailed);
    assert_eq!(treatment("loss_streak", Roast), Punch::LossTiles);
    assert_eq!(treatment("night_owl", Roast), Punch::Clock);
    assert_eq!(treatment("gamba_up", Popoff), Punch::Stonks);
    assert_eq!(treatment("gamba_down", Roast), Punch::NotStonks);
    assert_eq!(treatment("pet_lost", Roast), Punch::PressF);
    assert_eq!(treatment("cave_in", Roast), Punch::PressF);
    assert_eq!(treatment("deaths_per_game", Roast), Punch::Plain);
    assert_eq!(treatment("comeback", Popoff), Punch::Plain);

    let deaths = |tier| story("deaths_per_game", Theme::Combat, Roast, tier);
    assert_eq!(
        story_copy(&deaths(Tier::Extreme), &vars(), 1).treatment,
        Punch::SkillIssue
    );
    assert_eq!(
        story_copy(&deaths(Tier::Strong), &vars(), 1).treatment,
        Punch::Plain
    );
    let farm = story("gpm", Theme::Farm, Roast, Tier::Extreme);
    assert_eq!(story_copy(&farm, &vars(), 1).treatment, Punch::SkillIssue);
    let kda = story("kill_loss", Theme::Combat, Roast, Tier::Extreme);
    assert_eq!(story_copy(&kda, &vars(), 1).treatment, Punch::TaskFailed);
    let late = story("night_owl", Theme::Time, Roast, Tier::Extreme);
    assert_eq!(story_copy(&late, &vars(), 1).treatment, Punch::Clock);
}

#[test]
fn pools_fall_back_to_lower_tiers() {
    for key in METRIC_KEYS {
        for direction in metric_directions(key) {
            assert!(!pool(key, *direction, Tier::Notable).is_empty());
        }
    }
    assert!(pool("not_a_metric", Direction::Roast, Tier::Extreme).is_empty());
}

#[test]
fn item_stories_use_the_items_own_lines() {
    let mut wand = story("item", Theme::Items, Direction::Roast, Tier::Strong);
    wand.item = Some("magic_wand".to_owned());
    wand.display = "1.4".to_owned();
    let lines = item_pool("magic_wand");
    let copy = story_copy(&wand, &vars(), 11);
    assert!(lines.iter().any(|beat| beat.punch == copy.punch));
    assert_eq!(copy.label, "Magic Wand buys a game");
}

#[test]
fn side_games_have_deep_pools() {
    for key in SIDE_GAME_KEYS {
        for tier in TIERS {
            let lines = side_pool(key, tier);
            assert!(lines.len() >= 6, "{key} {tier:?}: {}", lines.len());
            assert!(
                lines
                    .iter()
                    .all(|beat| !beat.setup.contains('{') && !beat.punch.contains('{'))
            );
            let picked = side_game_copy(key, tier, 5);
            assert!(lines.contains(&picked));
        }
    }
    assert_eq!(
        side_game_copy("unknown", Tier::Strong, 1),
        Beat {
            setup: "",
            punch: ""
        }
    );
}

#[test]
fn personas_are_plentiful_and_fit_the_card() {
    let titles: BTreeSet<&str> = PERSONAS
        .iter()
        .flat_map(|(_, _, titles)| titles.iter().map(|(title, _)| *title))
        .chain(FALLBACK_PERSONAS.iter().map(|(title, _)| *title))
        .collect();
    assert!(titles.len() >= 12, "{} persona titles", titles.len());
    for (title, subtitle) in PERSONAS
        .iter()
        .flat_map(|(_, _, titles)| titles.iter())
        .chain(FALLBACK_PERSONAS.iter())
    {
        assert!(title.chars().count() <= 20, "{title}");
        assert!(subtitle.chars().count() <= 48, "{subtitle}");
    }
    for key in METRIC_KEYS {
        for direction in metric_directions(key) {
            assert!(
                PERSONAS
                    .iter()
                    .any(|(metric, way, _)| metric == key && way == direction),
                "{key} {direction:?} has no persona"
            );
        }
    }
}

#[test]
fn persona_comes_from_the_strongest_trait() {
    let throw = story("throw", Theme::Chaos, Direction::Roast, Tier::Extreme);
    let owl = story("night_owl", Theme::Time, Direction::Roast, Tier::Strong);
    let thrower = persona_title(&[throw.clone(), owl.clone()], 4);
    assert_eq!(thrower, persona_title(&[throw.clone(), owl.clone()], 4));
    let throws = PERSONAS
        .iter()
        .find(|(metric, _, _)| *metric == "throw")
        .unwrap()
        .2;
    assert!(throws.contains(&thrower));
    assert_ne!(persona_title(&[owl, throw], 4), thrower);
    assert!(FALLBACK_PERSONAS.contains(&persona_title(&[], 4)));
}
