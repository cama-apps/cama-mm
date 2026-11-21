//! Export deterministic Wrapped artwork for visual review.
//! `cargo run -p cama-app --example wrapped_preview -- <output-directory> [--still] [--patch-only]
//! [--only <kind-substring>] [--seed <number>] [--frames <progress,progress,...>]`
//!
//! Every name and number here is invented. The deck follows the order
//! `/wrapped` sends it in and takes its jokes from the real copy pools, then
//! adds one card for each punchline treatment the deck did not show. `--still`
//! skips the GIFs for a quick layout pass, `--only` keeps the cards whose kind
//! contains a string, `--seed` gives every card a player's background
//! variation, and `--frames` also writes the reveal at each progress from 0
//! to 1. Hero portraits, full-body renders, item icons and ability icons come
//! from the Steam image cache that production warms (`STEAM_IMAGE_CACHE_ROOT`,
//! default `.cache`) when it is present.
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedBeat, WrappedLayout, WrappedPunch, WrappedRow, WrappedSlideData,
    WrappedTheme, render_wrapped_frame, render_wrapped_media, render_wrapped_slide,
};
use cama_app::wrapped_story::personal_patch::{BalanceChange, BalanceDirection, personal_patch};
use cama_app::wrapped_story::roast_pools::{
    CopyVars, Punch, StoryCopy, hero_copy, people_copy, persona_title, record_copy, side_game_copy,
    story_copy,
};
use cama_domain::wrapped_activity::ServerRank;
use cama_domain::wrapped_distinct::{
    Direction, DistinctMatch, DistinctStory, HeroLine, PairStory, PersonalRecord, TableEnd, Theme,
    Tier,
};
use std::path::PathBuf;
use std::time::Instant;

const PLAYER: &str = "Alexandria";
const SEED: u64 = 2026;

fn card(kind: &str, title: &str) -> WrappedSlideData {
    let mut slide = WrappedSlideData::new(kind, title);
    slide.username = PLAYER.to_owned();
    slide.year_label = "Cama Wrapped 2026".to_owned();
    slide
}

fn styled(kind: &str, title: &str, layout: WrappedLayout, theme: WrappedTheme) -> WrappedSlideData {
    let mut slide = card(kind, title);
    slide.layout = layout;
    slide.theme = theme;
    slide
}

fn rows(values: &[(&str, &str)]) -> Vec<WrappedRow> {
    values
        .iter()
        .map(|(primary, secondary)| WrappedRow::new(*primary, *secondary))
        .collect()
}

fn lines(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// A cached hero image: the portrait, or the full-body render.
fn hero_image(slug: &str, render: bool) -> Option<Vec<u8>> {
    let mut path = cama_app::trivia_image_cache::production_trivia_cache_path().join("heroes");
    if render {
        path.push("renders");
    }
    std::fs::read(path.join(format!("{slug}.png"))).ok()
}

/// A cached game icon from `folder` of the trivia cache: `items` by
/// purchase-log key, `abilities` by slug.
fn icon_image(folder: &str, name: &str) -> Option<Vec<u8>> {
    let path = cama_app::trivia_image_cache::production_trivia_cache_path().join(folder);
    std::fs::read(path.join(format!("{name}.png"))).ok()
}

/// One row of a patch chapter: its name, line, stamp and buff-or-nerf sign.
fn tagged(primary: &str, secondary: &str, tag: &str, value: f64) -> WrappedRow {
    let mut row = WrappedRow::new(primary, secondary);
    row.tag = tag.to_owned();
    row.value = value;
    row
}

/// A real Camagotchi card, drawn the way `/pet` draws one.
fn pet_card(assets: &str) -> Vec<u8> {
    use cama_app::pet_assets::{
        FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
    };
    use cama_domain::pet::{PetMood, PetStage};
    let mut loader = PetAssetLoader::new(
        FilesystemPetAssets::new(format!("{assets}/pets")),
        HybridPetRenderer::new(format!("{assets}/pets/components")),
    );
    loader
        .get_pet_card(&PetRenderRequest {
            species_id: "common_cama",
            stage: PetStage::Adult,
            mood: PetMood::Happy,
            seed: 7,
            accessory: Some("red_bow"),
            evolution: None,
        })
        .bytes()
        .to_vec()
}

/// `Punch` and `WrappedPunch` live in crates that cannot see each other.
fn media_punch(punch: Punch) -> WrappedPunch {
    match punch {
        Punch::Plain => WrappedPunch::Plain,
        Punch::Cinema => WrappedPunch::Cinema,
        Punch::GoldGraph => WrappedPunch::GoldGraph,
        Punch::Stonks => WrappedPunch::Stonks,
        Punch::NotStonks => WrappedPunch::NotStonks,
        Punch::Calculated => WrappedPunch::Calculated,
        Punch::TaskFailed => WrappedPunch::TaskFailed,
        Punch::LossTiles => WrappedPunch::LossTiles,
        Punch::PressF => WrappedPunch::PressF,
        Punch::SkillIssue => WrappedPunch::SkillIssue,
        Punch::Clock => WrappedPunch::Clock,
    }
}

/// One two-beat card from real copy, with `hero`'s full-body render on it.
fn two_beat(
    kind: &str,
    title: &str,
    theme: WrappedTheme,
    copy: StoryCopy,
    hero: Option<&str>,
) -> WrappedSlideData {
    let mut slide = styled(kind, title, WrappedLayout::TwoBeat, theme);
    slide.stat_value = copy.value.clone();
    slide.beat = Some(WrappedBeat {
        setup: copy.setup,
        value: copy.value,
        setup_label: copy.label,
        punch: copy.punch,
        credit: copy.credit,
        treatment: media_punch(copy.treatment),
    });
    slide.art_png = hero.and_then(|slug| hero_image(slug, true));
    slide
}

/// A story that stands out on the server, as the ranking engine reports it.
fn story(
    metric: &'static str,
    theme: Theme,
    direction: Direction,
    tier: Tier,
    display: &str,
    rank: Option<(i64, i64)>,
) -> DistinctStory {
    DistinctStory {
        metric,
        theme,
        direction,
        tier,
        value: 0.0,
        display: display.to_owned(),
        rank: rank.map(|(rank, of)| ServerRank { rank, of }),
        // The preview's only ranked roast is a bottom-of-the-table one.
        end: match direction {
            Direction::Popoff => TableEnd::Top,
            Direction::Roast => TableEnd::Bottom,
        },
        hero_id: None,
        match_id: None,
        item: None,
        score: 0.0,
    }
}

fn vars(hero: Option<&str>) -> CopyVars {
    CopyVars {
        hero: hero.map(str::to_owned),
        ..CopyVars::default()
    }
}

fn record(key: &'static str, direction: Direction, display: &str, won: bool) -> PersonalRecord {
    PersonalRecord {
        key,
        direction,
        value: 0,
        display: display.to_owned(),
        hero_id: None,
        match_id: 0,
        finished_at: None,
        won: Some(won),
        score: 0.0,
    }
}

/// The server-relative stories: the deck's Dota heart.
fn distinct_cards() -> (Vec<WrappedSlideData>, Vec<DistinctStory>) {
    use Direction::{Popoff, Roast};
    use WrappedTheme::{BeatPopoff, BeatPopoffAqua, BeatPopoffPink, BeatRoast, BeatRoastRed};
    let best = story(
        "best_game",
        Theme::Combat,
        Popoff,
        Tier::Extreme,
        "27 / 2 / 14",
        Some((1, 40)),
    );
    let gpm = story(
        "gpm",
        Theme::Farm,
        Popoff,
        Tier::Strong,
        "712",
        Some((2, 40)),
    );
    let mut night = story(
        "night_owl",
        Theme::Time,
        Roast,
        Tier::Extreme,
        "4:12 AM",
        None,
    );
    night.value = 23.0;
    let win_rate = story(
        "win_rate",
        Theme::Clutch,
        Popoff,
        Tier::Notable,
        "58%",
        Some((5, 40)),
    );
    let throw = story("throw", Theme::Chaos, Roast, Tier::Extreme, "23,400", None);
    let mut cards = vec![
        two_beat(
            "story_distinct",
            "Combat",
            BeatPopoff,
            story_copy(&best, &vars(Some("Queen of Pain")), SEED),
            Some("queenofpain"),
        ),
        two_beat(
            "story_distinct",
            "Farm",
            BeatPopoffPink,
            story_copy(&gpm, &vars(None), SEED + 1),
            None,
        ),
        two_beat(
            "story_distinct",
            "Late nights",
            BeatRoast,
            story_copy(&night, &vars(None), SEED + 2),
            None,
        ),
        two_beat(
            "story_distinct",
            "Clutch",
            BeatPopoffAqua,
            story_copy(&win_rate, &vars(None), SEED + 3),
            None,
        ),
        two_beat(
            "story_distinct",
            "Chaos",
            BeatRoastRed,
            story_copy(&throw, &vars(None), SEED + 4),
            None,
        ),
    ];
    cards[4].series = vec![
        0.0, 250.0, 900.0, 1_800.0, 2_600.0, 4_100.0, 6_000.0, 8_200.0, 11_000.0, 13_500.0,
        16_900.0, 19_800.0, 23_400.0, 21_000.0, 16_500.0, 11_200.0, 6_800.0, 1_900.0, -2_600.0,
        -5_900.0, -9_400.0, -12_800.0,
    ];
    (cards, vec![best, gpm, night, win_rate, throw])
}

/// Personal records, the hero cards, and the people cards.
fn personal_cards() -> Vec<WrappedSlideData> {
    use WrappedTheme::{BeatPopoffLime, BeatPopoffYellow, BeatRoastPlum, BeatRoastViolet};
    let deaths = record("most_deaths", Direction::Roast, "19 deaths", false);
    let kills = record("most_kills", Direction::Popoff, "31 kills", true);
    let best = HeroLine {
        hero_id: 1,
        games: 12,
        wins: 10,
    };
    let nemesis = HeroLine {
        hero_id: 14,
        games: 9,
        wins: 2,
    };
    let duo = PairStory {
        discord_id: 2,
        games: 41,
        wins: 28,
    };
    let bag = PairStory {
        discord_id: 3,
        games: 14,
        wins: 11,
    };
    vec![
        two_beat(
            "story_record",
            "Personal records",
            BeatRoastViolet,
            record_copy(&deaths, Some("Pudge"), Some("Mar 2"), SEED + 5),
            Some("pudge"),
        ),
        two_beat(
            "story_record",
            "Personal records",
            BeatPopoffYellow,
            record_copy(&kills, Some("Queen of Pain"), Some("Mar 14"), SEED + 6),
            Some("queenofpain"),
        ),
        two_beat(
            "story_hero_year",
            "Your best hero",
            BeatPopoffLime,
            hero_copy("hero_best", &best, Some("Anti-Mage"), SEED + 7),
            Some("antimage"),
        ),
        two_beat(
            "story_hero_year",
            "The hero that hates you",
            BeatRoastPlum,
            hero_copy("hero_nemesis", &nemesis, Some("Pudge"), SEED + 8),
            Some("pudge"),
        ),
        two_beat(
            "story_people",
            "Your duo",
            WrappedTheme::BeatPopoffAqua,
            people_copy("duo", &duo, Some("Mirana Main"), SEED + 9),
            None,
        ),
        two_beat(
            "story_people",
            "Punching bag",
            WrappedTheme::BeatPopoffPink,
            people_copy("punching_bag", &bag, Some("Tusk Enjoyer"), SEED + 10),
            None,
        ),
    ]
}

/// A side-game beat as a two-beat card, the way the deck builds one.
fn side_beat(
    kind: &str,
    key: &str,
    theme: WrappedTheme,
    value: &str,
    label: &str,
    treatment: Punch,
) -> WrappedSlideData {
    let beat = side_game_copy(key, Tier::Strong, SEED + 11);
    let copy = StoryCopy {
        setup: beat.setup.to_owned(),
        value: value.to_owned(),
        label: label.to_owned(),
        punch: beat.punch.to_owned(),
        credit: format!("{value} {label}"),
        treatment,
    };
    two_beat(kind, "", theme, copy, None)
}

fn side_cards(assets: &str) -> Vec<WrappedSlideData> {
    let mut stonks = side_beat(
        "chart_gamba",
        "gamba_up",
        WrappedTheme::BeatPopoffLime,
        "+8,420 JC",
        "across 96 gambles",
        Punch::Stonks,
    );
    stonks.series = (0..40)
        .map(|index| {
            let x = f64::from(index);
            x * 210.0 + (x / 2.3).sin() * 700.0
        })
        .collect();

    let mut dig = styled(
        "story_dig",
        "Dig",
        WrappedLayout::BigNumber,
        WrappedTheme::DigDeep,
    );
    dig.headline = "You went looking for trouble.".to_owned();
    dig.stat_value = "1,284".to_owned();
    dig.stat_label = "blocks dug; deepest level 412".to_owned();
    dig.badge = "Dug more than 92% of players".to_owned();
    dig.lines = lines(&["Busiest day: 41 digs on 2026-03-01."]);

    let mut pet = styled(
        "story_pet",
        "Camagotchi",
        WrappedLayout::Spotlight,
        WrappedTheme::Pet,
    );
    pet.art_png = Some(pet_card(assets));
    pet.headline = "Sir Fluffington".to_owned();
    pet.stat_value = "412 meals".to_owned();
    pet.stat_label = "served across 3 pets".to_owned();
    pet.badge = "4,120 JC on snacks".to_owned();
    pet.lines = lines(&["12 brawl wins."]);

    let mut mafia = styled(
        "story_mafia",
        "Mafia",
        WrappedLayout::Tiles,
        WrappedTheme::Mafia,
    );
    mafia.headline = "You lied to your friends.".to_owned();
    mafia.rows = rows(&[
        ("34", "games played"),
        ("9", "times Mafia"),
        ("7", "wins as Mafia"),
        ("3", "times MVP"),
    ]);
    mafia.lines = lines(&["Your friends voted you out 11 times. They were right 8."]);

    vec![stonks, dig, pet, mafia]
}

/// One card for each punchline treatment the deck above leaves out.
fn treatment_gallery() -> Vec<WrappedSlideData> {
    use Direction::Roast;
    use WrappedTheme::{BeatPopoffPink, BeatRoast, BeatRoastRed, BeatRoastViolet};
    let upset = story(
        "upset_win",
        Theme::Clutch,
        Direction::Popoff,
        Tier::Strong,
        "18%",
        None,
    );
    let kill_loss = story("kill_loss", Theme::Combat, Roast, Tier::Strong, "31", None);
    let streak = story("loss_streak", Theme::Chaos, Roast, Tier::Strong, "9", None);
    let kills = story(
        "kills_per_game",
        Theme::Combat,
        Roast,
        Tier::Extreme,
        "1.8",
        Some((1, 40)),
    );
    let mut calculated = two_beat(
        "story_distinct",
        "Clutch",
        BeatPopoffPink,
        story_copy(&upset, &vars(None), SEED + 12),
        None,
    );
    calculated.stat_value = "18%".to_owned();
    let task_failed = two_beat(
        "story_distinct",
        "Combat",
        BeatRoastViolet,
        story_copy(&kill_loss, &vars(Some("Queen of Pain")), SEED + 13),
        Some("queenofpain"),
    );
    let mut losses = two_beat(
        "story_distinct",
        "Chaos",
        BeatRoastRed,
        story_copy(&streak, &vars(None), SEED + 14),
        None,
    );
    losses.series = vec![-1.0; 9];
    let skill = two_beat(
        "story_distinct",
        "Combat",
        BeatRoast,
        story_copy(&kills, &vars(None), SEED + 15),
        None,
    );
    let mut not_stonks = side_beat(
        "chart_gamba",
        "gamba_down",
        WrappedTheme::BeatRoastPlum,
        "-3,100 JC",
        "across 12 gambles",
        Punch::NotStonks,
    );
    not_stonks.series = vec![
        0.0, -300.0, 100.0, -1_100.0, -900.0, -2_200.0, -1_900.0, -3_100.0,
    ];
    let press_f = side_beat(
        "story_dig_cave_in",
        "cave_in",
        BeatRoast,
        "37",
        "cave-ins",
        Punch::PressF,
    );
    vec![calculated, task_failed, losses, skill, not_stonks, press_f]
}

fn deck(assets: &str) -> Vec<WrappedSlideData> {
    let mut deck = Vec::new();

    let mut intro = card("story_intro", "Your year");
    intro.theme = WrappedTheme::Patch;
    intro.stat_value = "20.26".to_owned();
    intro.headline = "The Bloodbath\nUpdate".to_owned();
    intro.stat_label = PLAYER.to_owned();
    deck.push(intro);
    deck.extend(patch_gallery());

    let mut games = styled(
        "story_games",
        "Your Year",
        WrappedLayout::BigNumber,
        WrappedTheme::Queue,
    );
    games.headline = "Your games this year".to_owned();
    games.stat_value = "247".to_owned();
    games.stat_label = "games played".to_owned();
    games.badge = "More games than 90% of the server".to_owned();
    games.artwork = WrappedArtwork::Courier;
    deck.push(games);

    let mut hours = styled(
        "story_playtime",
        "Time played",
        WrappedLayout::BigNumber,
        WrappedTheme::QueueAlt,
    );
    hours.headline = "Your time in matches.".to_owned();
    hours.stat_value = "185h".to_owned();
    hours.stat_label = "recorded match time".to_owned();
    hours.lines = lines(&["Queue time is not included."]);
    hours.artwork = WrappedArtwork::Courier;
    deck.push(hours);

    let mut heroes = card("story_hero", "Your heroes");
    heroes.headline = "Your top heroes".to_owned();
    heroes.stat_label = "5 heroes across 186 recorded picks".to_owned();
    heroes.rows = rows(&[
        ("Queen of Pain", "81 games · 62% wins"),
        ("Crystal Maiden", "46 games · 54% wins"),
        ("Dark Willow", "28 games · 61% wins"),
        ("Pudge", "19 games · 37% wins"),
        ("Keeper of the Light", "1 game · 100% wins"),
    ]);
    for (row, slug) in heroes.rows.iter_mut().zip([
        "queenofpain",
        "crystal_maiden",
        "dark_willow",
        "pudge",
        "keeper_of_the_light",
    ]) {
        row.image_png = hero_image(slug, false);
    }
    heroes.art_png = hero_image("queenofpain", true);
    let recap_heroes = heroes.rows.clone();
    deck.push(heroes);
    deck.extend(hero_update_cards());
    deck.extend(item_update_cards());
    deck.extend(talent_cards());

    let (distinct, stories) = distinct_cards();
    deck.extend(distinct);
    deck.extend(personal_cards());
    deck.extend(bug_fix_cards());
    deck.extend(side_cards(assets));

    let (title, subtitle) = persona_title(&stories, SEED);
    let mut persona = styled(
        "story_persona",
        "Your Cama type",
        WrappedLayout::Persona,
        WrappedTheme::Persona,
    );
    persona.stat_value = title.to_owned();
    persona.headline = subtitle.to_owned();
    persona.rows = rows(&[
        ("27 / 2 / 14", "Queen of Pain"),
        ("712", "gold a minute"),
        ("58%", "win rate"),
    ]);
    deck.push(persona);

    let mut finale = card("story_finale", "2026 Wrapped");
    finale.stat_value = PLAYER.to_owned();
    finale.headline = title.to_owned();
    finale.badge = "27 / 2 / 14 · Queen of Pain · #1 of 40".to_owned();
    finale.rows = recap_heroes;
    finale.lines = lines(&[
        "247|games played",
        "185h|in recorded matches",
        "27 / 2 / 14|Queen of Pain",
        "4:12 AM|latest game finish",
        "1,284|blocks dug",
    ]);
    deck.push(finale);

    deck.extend(treatment_gallery());
    deck.extend(measured_chapter_cards());

    let count = deck.len();
    for (index, slide) in deck.iter_mut().enumerate() {
        slide.chapter_index = index;
        slide.chapter_count = count;
        if slide.section.is_empty() {
            slide.section = section(&slide.kind).to_owned();
        }
    }
    deck
}

/// The chapter a card's kind belongs to, as the deck tags it.
fn section(kind: &str) -> &'static str {
    match kind {
        "story_games" | "story_playtime" => "General",
        kind if kind.contains("story_patch_award") => "Superlatives",
        kind if kind.starts_with("story_patch") => "General",
        kind if kind.starts_with("story_hero") => "Hero updates",
        kind if kind.starts_with("story_items") => "Item updates",
        kind if kind.starts_with("story_talents") => "Talents",
        "story_distinct" | "story_record" => "Gameplay",
        kind if kind.starts_with("story_bugfixes") => "Bug fixes",
        _ => "",
    }
}

/// An invented league year: forty-eight games for the player and nine
/// other players in each, so the measured chapter has medians to stand on.
fn invented_year() -> Vec<DistinctMatch> {
    const QUEEN_OF_PAIN: i64 = 39;
    const PUDGE: i64 = 14;
    const KEEPER: i64 = 90;
    let mut games = Vec::new();
    for id in 1..=48_i64 {
        let late = id > 24;
        let hero = match (id % 2 == 0, late) {
            (true, _) => QUEEN_OF_PAIN,
            (false, false) => PUDGE,
            (false, true) => KEEPER,
        };
        let won = match hero {
            QUEEN_OF_PAIN if late => id % 8 != 2,
            QUEEN_OF_PAIN => id % 8 == 0,
            PUDGE => matches!(id, 1 | 13),
            _ => id % 4 == 1,
        };
        let (kills, deaths, assists, position) = match hero {
            QUEEN_OF_PAIN => (11, if late { 4 } else { 7 }, 8, 2),
            PUDGE => (4, if id == 9 { 19 } else { 9 }, 12, 4),
            _ => (2, 5, 18, 5),
        };
        let mut items = match hero {
            QUEEN_OF_PAIN => vec![("blink", 1), ("tpscroll", 3), ("tango", 2), ("bottle", 1)],
            PUDGE => vec![
                ("blink", 1),
                ("tango", 4),
                ("ward_observer", 5),
                ("smoke_of_deceit", 1),
                ("tpscroll", 3),
            ],
            _ => vec![
                ("ward_observer", 8),
                ("ward_sentry", 6),
                ("tango", 3),
                ("clarity", 4),
                ("tpscroll", 3),
                ("dust", 2),
            ],
        };
        if hero == QUEEN_OF_PAIN && id % 4 == 0 {
            items.push(("black_king_bar", 1));
        }
        if matches!(id, 16 | 40) {
            items.push(("rapier", 1));
        }
        games.push(DistinctMatch {
            match_id: id,
            discord_id: 7,
            side: Some(if (id / 2) % 2 == 0 { "radiant" } else { "dire" }.to_owned()),
            won: Some(won),
            // Weekly from early January 2026, a little after one in the morning UTC.
            finished_at: Some(1_767_661_200 + id * 7 * 86_400 + (id % 3) * 86_400),
            duration_seconds: Some(if id == 30 {
                4_260
            } else {
                1_500 + (id % 5) * 480
            }),
            hero_id: Some(hero),
            kills: Some(kills),
            deaths: Some(deaths),
            assists: Some(assists),
            last_hits: Some(if hero == QUEEN_OF_PAIN { 260 } else { 40 }),
            gpm: Some(match hero {
                QUEEN_OF_PAIN if late => 640,
                QUEEN_OF_PAIN => 520,
                _ => 300,
            }),
            hero_damage: Some(if hero == QUEEN_OF_PAIN { 28_000 } else { 9_000 }),
            hero_healing: Some(if hero == KEEPER { 6_400 } else { 0 }),
            wards: Some(if hero == QUEEN_OF_PAIN { 1 } else { 13 }),
            camps_stacked: Some(if hero == QUEEN_OF_PAIN { 0 } else { 4 }),
            rune_pickups: Some(7),
            stuns: Some(if hero == PUDGE { 70.0 } else { 12.0 }),
            enriched: true,
            courier_kills: Some(0),
            pings: Some(if hero == PUDGE { 61 } else { 24 }),
            actions_per_min: Some(180.0),
            rapiers: Some(i64::from(matches!(id, 16 | 40))),
            buybacks: Some(i64::from(id % 6 == 0)),
            items: Some(
                items
                    .into_iter()
                    .map(|(key, count)| (key.to_owned(), count))
                    .collect(),
            ),
            comeback: (won && id % 5 == 0).then_some(9_400.0),
            throw: (!won && id == 22).then_some(14_300.0),
            expected_win: Some(if id % 3 == 0 { 0.36 } else { 0.62 }),
            rating_delta: Some(if won { 18.0 } else { -17.0 }),
            first_blood: Some(hero == QUEEN_OF_PAIN && id % 4 == 0),
            roshans: Some(i64::from(hero == QUEEN_OF_PAIN && won)),
            position: Some(position),
            last_hits_at_10: Some(if hero == QUEEN_OF_PAIN { 62 } else { 9 }),
            teamfight_participation: Some(if hero == QUEEN_OF_PAIN { 0.84 } else { 0.58 }),
            net_worth: Some(if hero == QUEEN_OF_PAIN { 24_000 } else { 9_000 }),
            denies: Some(if hero == QUEEN_OF_PAIN { 13 } else { 2 }),
            tower_damage: Some(if hero == QUEEN_OF_PAIN { 2_400 } else { 300 }),
        });
        for other in 1..=9_i64 {
            games.push(DistinctMatch {
                match_id: id,
                discord_id: 100 + other,
                side: Some(if other <= 4 { "radiant" } else { "dire" }.to_owned()),
                won: Some((id + other) % 2 == 0),
                finished_at: Some(1_767_661_200 + id * 7 * 86_400),
                duration_seconds: Some(1_500 + (id % 5) * 480),
                hero_id: Some(other),
                kills: Some(6),
                deaths: Some(6),
                assists: Some(10),
                gpm: Some(380 + other * 20),
                teamfight_participation: Some(0.6),
                net_worth: Some(11_000 + other * 900),
                items: Some(vec![("tango".to_owned(), 1), ("tpscroll".to_owned(), 2)]),
                position: Some((other - 1) % 5 + 1),
                ..Default::default()
            });
        }
    }
    games
}

/// The patch chapter as `/wrapped` writes it: every number and line below
/// comes from the story functions run over the invented year.
fn measured_chapter_cards() -> Vec<WrappedSlideData> {
    use cama_app::wrapped_story::bug_fixes::bug_fixes;
    use cama_app::wrapped_story::hero_patch::{HeroChangeTag, hero_updates};
    use cama_app::wrapped_story::item_patch::{ItemInfo, ItemNote, item_updates};
    use cama_app::wrapped_story::talents::{TalentSide, talent_tree};
    use cama_domain::hero_catalog::bundled_hero_name;

    let year = invented_year();
    let slug = |hero_id: i64| match hero_id {
        39 => (
            "queenofpain",
            &["shadow_strike", "blink", "scream_of_pain", "sonic_wave"][..],
        ),
        14 => ("pudge", &["meat_hook", "rot", "dismember"][..]),
        90 => ("keeper_of_the_light", &["illuminate"][..]),
        _ => ("", &[][..]),
    };
    let catalog = [
        ("blink", "Blink Dagger", 2_250),
        ("black_king_bar", "Black King Bar", 4_050),
        ("rapier", "Divine Rapier", 5_600),
        ("tango", "Tango", 90),
        ("tpscroll", "Town Portal Scroll", 100),
        ("ward_observer", "Observer Ward", 0),
        ("ward_sentry", "Sentry Ward", 50),
        ("smoke_of_deceit", "Smoke of Deceit", 50),
        ("clarity", "Clarity", 50),
        ("dust", "Dust of Appearance", 80),
        ("bottle", "Bottle", 675),
    ]
    .map(|(key, name, cost)| ItemInfo {
        key: key.to_owned(),
        name: name.to_owned(),
        cost,
    });
    let mut cards = Vec::new();

    let mut heroes = styled(
        "story_hero_patch_measured",
        "Hero updates",
        WrappedLayout::HeroPatch,
        WrappedTheme::PatchHeroes,
    );
    heroes.rows = hero_updates(&year, 7, &[(39, 9)])
        .iter()
        .map(|update| {
            let (tag, value) = match update.tag {
                HeroChangeTag::Buffed => ("Buffed", 1.0),
                HeroChangeTag::Nerfed => ("Nerfed", -1.0),
                HeroChangeTag::New => ("New", 0.0),
                HeroChangeTag::Removed => ("Removed", -1.0),
                HeroChangeTag::Banned => ("Banned", 0.0),
            };
            let mut row = tagged(
                bundled_hero_name(update.hero_id).unwrap_or("Unknown hero"),
                &update.line,
                tag,
                value,
            );
            let (hero, abilities) = slug(update.hero_id);
            row.image_png = hero_image(hero, false);
            row.icons = abilities
                .iter()
                .filter_map(|ability| icon_image("abilities", &format!("{hero}_{ability}")))
                .collect();
            row
        })
        .collect();
    cards.push(heroes);

    let mut items = styled(
        "story_items_measured",
        "Item updates",
        WrappedLayout::ItemPatch,
        WrappedTheme::PatchItems,
    );
    items.rows = item_updates(&year, 7, &catalog)
        .iter()
        .map(|update| {
            let (tag, value) = match (update.note, update.direction) {
                (ItemNote::Signature, _) => ("Most built", 0.0),
                (ItemNote::WinRate, Some(BalanceDirection::Nerf)) => ("Loses with", -1.0),
                (ItemNote::WinRate, _) => ("Wins with", 1.0),
                (ItemNote::Consumable, _) => ("Most bought", 0.0),
                (ItemNote::NeverBuilt, _) => ("Never built", 0.0),
                (ItemNote::Oddity, _) => ("", 0.0),
            };
            let mut row = tagged(&update.name, &update.line, tag, value);
            row.image_png = icon_image("items", &update.key);
            row
        })
        .collect();
    cards.push(items);

    let mut talents = styled(
        "story_talents_measured",
        "Talents",
        WrappedLayout::Talents,
        WrappedTheme::PatchTalents,
    );
    talents.rows = talent_tree(&year, 7, None, SEED)
        .iter()
        .map(|tier| {
            tagged(
                &tier.left,
                &tier.right,
                &tier.level.to_string(),
                match tier.picked {
                    TalentSide::Left => -1.0,
                    TalentSide::Right => 1.0,
                },
            )
        })
        .collect();
    talents.lines = lines(&["Your win rate each way."]);
    cards.push(talents);

    let mut fixes = styled(
        "story_bugfixes_measured",
        "Bug fixes",
        WrappedLayout::BugFixes,
        WrappedTheme::PatchFixes,
    );
    fixes.rows = bug_fixes(&year, 7, None, SEED)
        .iter()
        .map(|fix| {
            let hero = fix.hero_id.and_then(bundled_hero_name).unwrap_or_default();
            let mut row = tagged(
                &fix.line.replace("{hero}", hero),
                "",
                if fix.known_issue { "Known issue" } else { "" },
                0.0,
            );
            row.image_png = fix
                .hero_id
                .and_then(|hero_id| hero_image(slug(hero_id).0, false));
            row
        })
        .collect();
    cards.push(fixes);

    let patch = personal_patch(&year, 7, None);
    for (index, award) in patch.awards.iter().enumerate() {
        let mut medal = styled(
            "story_patch_award_measured",
            &award.title,
            WrappedLayout::Award,
            if index == 1 {
                WrappedTheme::Awards
            } else {
                WrappedTheme::Patch
            },
        );
        medal.stat_value = award.value.clone();
        medal.stat_label = award.evidence.clone();
        medal.art_png = icon_image("items", award.icon);
        cards.push(medal);
    }
    let mut changes = styled(
        "story_patch_measured",
        "Balance changes",
        WrappedLayout::PatchNotes,
        WrappedTheme::Patch,
    );
    changes.headline = "You got\npatched.".to_owned();
    changes.rows = patch.changes.iter().map(change_row).collect();
    changes.lines = patch.changes.first().map(compared).into_iter().collect();
    cards.push(changes);
    cards
}

/// A balance change as its card shows it: the title, then the value.
fn change_row(change: &BalanceChange) -> WrappedRow {
    let sign = match change.direction {
        BalanceDirection::Buff => 1.0,
        BalanceDirection::Nerf => -1.0,
    };
    tagged(&change.title, &change.value, "", sign)
}

/// The balance card's footnote: which games every change compares.
fn compared(change: &BalanceChange) -> String {
    let games = change.compared_games;
    format!("Your first {games} games vs your last {games}")
}

/// Hero updates: a full card with real art, the same card with nothing
/// cached and the longest copy, one and two heroes, and long one-word names
/// under the widest lines the deck writes.
fn hero_update_cards() -> Vec<WrappedSlideData> {
    let hero = |name: &str, slug: &str, tag: &str, value: f64, changes: &str, icons: &[&str]| {
        let mut row = tagged(name, changes, tag, value);
        row.image_png = hero_image(slug, false);
        row.icons = icons
            .iter()
            .filter_map(|ability| icon_image("abilities", &format!("{slug}_{ability}")))
            .collect();
        row
    };
    let queen = hero(
        "Queen of Pain",
        "queenofpain",
        "BUFFED",
        1.0,
        "Win rate 48% to 62%",
        &["shadow_strike", "blink", "scream_of_pain", "sonic_wave"],
    );
    let pudge = hero(
        "Pudge",
        "pudge",
        "NERFED",
        -1.0,
        "Deaths 6.2 to 9.4 a game",
        &["meat_hook", "rot", "dismember"],
    );
    let keeper = hero(
        "Keeper of the Light",
        "keeper_of_the_light",
        "NEW",
        0.0,
        "12 games, 67% won",
        &["illuminate"],
    );
    let maiden = hero(
        "Crystal Maiden",
        "crystal_maiden",
        "BANNED",
        -1.0,
        "Banned against you 14 times",
        &["crystal_nova", "frostbite", "freezing_field"],
    );
    let willow = hero(
        "Dark Willow",
        "dark_willow",
        "REMOVED",
        -1.0,
        "Dropped after 9 games, 33% won",
        &["bramble_maze", "shadow_realm", "cursed_crown"],
    );

    let mut full = card("story_hero_patch", "Hero updates");
    full.rows = vec![queen.clone(), pudge.clone(), keeper.clone()];

    let mut bare = styled(
        "story_hero_patch_bare",
        "Hero updates",
        WrappedLayout::HeroPatch,
        WrappedTheme::PatchHeroes,
    );
    bare.rows = vec![keeper, maiden.clone(), willow.clone()];
    for row in &mut bare.rows {
        row.image_png = None;
        row.icons.clear();
    }

    let mut duo = bare.clone();
    duo.kind = "story_hero_patch_duo".to_owned();
    duo.rows = vec![maiden, pudge];

    let mut solo = bare.clone();
    solo.kind = "story_hero_patch_solo".to_owned();
    solo.rows = vec![queen];

    let mut solo_bare = solo.clone();
    solo_bare.kind = "story_hero_patch_solo_bare".to_owned();
    solo_bare.rows = vec![willow];
    solo_bare.rows[0].image_png = None;
    solo_bare.rows[0].icons.clear();

    // One-word names wider than their box, each under the widest line the
    // story functions write for its verdict.
    let lines = cama_app::wrapped_story::hero_patch::widest_lines();
    let longest = |opening: &str| {
        lines
            .iter()
            .find(|line| line.starts_with(opening))
            .cloned()
            .unwrap_or_default()
    };
    let shaker = hero(
        "Earthshaker",
        "earthshaker",
        "BANNED",
        0.0,
        &longest("Banned"),
        &["fissure", "enchant_totem", "aftershock", "echo_slam"],
    );
    let dawn = hero(
        "Dawnbreaker",
        "dawnbreaker",
        "NERFED",
        -1.0,
        &longest("Deaths"),
        &[
            "fire_wreath",
            "celestial_hammer",
            "luminosity",
            "solar_guardian",
        ],
    );
    let legion = hero(
        "Legion Commander",
        "legion_commander",
        "REMOVED",
        -1.0,
        &longest("Dropped"),
        &[],
    );
    let mut duo_long = bare.clone();
    duo_long.kind = "story_hero_patch_duo_long".to_owned();
    duo_long.rows = vec![shaker.clone(), legion.clone()];
    let mut trio_long = bare.clone();
    trio_long.kind = "story_hero_patch_trio_long".to_owned();
    trio_long.rows = vec![dawn.clone(), shaker.clone(), legion];
    let mut solo_long = solo.clone();
    solo_long.kind = "story_hero_patch_solo_long".to_owned();
    solo_long.rows = vec![shaker];
    let mut solo_stats = solo.clone();
    solo_stats.kind = "story_hero_patch_solo_stats".to_owned();
    solo_stats.rows = vec![dawn];
    vec![
        full, bare, duo, solo, solo_bare, duo_long, trio_long, solo_long, solo_stats,
    ]
}

/// Item updates: four icons, the same card with nothing cached and long
/// names, one and two items, and the widest figures beside the longest lines
/// the deck writes.
fn item_update_cards() -> Vec<WrappedSlideData> {
    let item = |name: &str, key: &str, tag: &str, value: f64, line: &str| {
        let mut row = tagged(name, line, tag, value);
        row.image_png = icon_image("items", key);
        row
    };
    let blink = item("Blink Dagger", "blink", "MOST BUILT", 0.0, "41 of 48 games");
    let bar = item(
        "Black King Bar",
        "black_king_bar",
        "LOSES WITH",
        -1.0,
        "31% won in 13 games",
    );
    let ward = item(
        "Observer Ward",
        "ward_observer",
        "MOST BOUGHT",
        0.0,
        "212 bought",
    );
    let rapier = item("Divine Rapier", "rapier", "", 0.0, "3 bought, 0 won");

    let mut full = card("story_items", "Item updates");
    full.rows = vec![blink.clone(), bar.clone(), ward.clone(), rapier.clone()];

    let mut bare = styled(
        "story_items_bare",
        "Item updates",
        WrappedLayout::ItemPatch,
        WrappedTheme::PatchItems,
    );
    bare.rows = vec![
        tagged("Aghanim's Scepter", "22 of 48 games", "MOST BUILT", 0.0),
        tagged("Desolator", "68% won in 19 games", "WINS WITH", 1.0),
        tagged("Town Portal Scroll", "1,204 bought", "MOST BOUGHT", 0.0),
        tagged("Black King Bar", "0 of 31 games", "NEVER BUILT", 0.0),
    ];

    let mut trio = bare.clone();
    trio.kind = "story_items_trio".to_owned();
    trio.rows = vec![ward, rapier, blink.clone()];

    let mut duo = bare.clone();
    duo.kind = "story_items_duo".to_owned();
    duo.rows = vec![bar, blink.clone()];

    let mut solo = bare.clone();
    solo.kind = "story_items_solo".to_owned();
    solo.rows = vec![blink];

    // The widest figures with the longest lines the story functions write.
    let lines = cama_app::wrapped_story::item_patch::widest_lines();
    let longest = |opening: &str| {
        lines
            .iter()
            .filter(|line| line.starts_with(opening))
            .max_by_key(|line| line.len())
            .map_or("", String::as_str)
    };
    let wide_rows = vec![
        item("Divine Rapier", "rapier", "", 0.0, longest("1,204 bought")),
        item("Desolator", "desolator", "WINS WITH", 1.0, longest("100%")),
        item(
            "Blink Dagger",
            "blink",
            "MOST BUILT",
            0.0,
            longest("888 of"),
        ),
        item(
            "Black King Bar",
            "black_king_bar",
            "NEVER BUILT",
            0.0,
            longest("0 of"),
        ),
    ];
    let mut wide = bare.clone();
    wide.kind = "story_items_wide".to_owned();
    wide.rows = wide_rows.clone();
    let mut wide_trio = bare.clone();
    wide_trio.kind = "story_items_wide_trio".to_owned();
    wide_trio.rows = wide_rows[..3].to_vec();
    let mut wide_solo = bare.clone();
    wide_solo.kind = "story_items_wide_solo".to_owned();
    wide_solo.rows = wide_rows[..1].to_vec();
    vec![full, bare, trio, duo, solo, wide, wide_trio, wide_solo]
}

/// Talents: a full tree with the longest options, and a tree that stopped
/// growing at level 15.
fn talent_cards() -> Vec<WrappedSlideData> {
    let level = |tag: &str, left: &str, right: &str, value: f64| tagged(left, right, tag, value);
    let mut full = card("story_talents", "Talents");
    full.rows = vec![
        level("25", "71% under 35m", "38% over 45m", -1.0),
        level("20", "44% as favorite", "62% as underdog", 1.0),
        level("15", "48% Jan\u{2013}Jun", "63% Jul\u{2013}Dec", 1.0),
        level("10", "58% Radiant", "51% Dire", -1.0),
    ];
    full.lines = lines(&["Your win rate each way."]);

    let mut sparse = styled(
        "story_talents_sparse",
        "Talents",
        WrappedLayout::Talents,
        WrappedTheme::PatchTalents,
    );
    sparse.rows = vec![
        level("20", "50% weekdays", "50% weekends", 0.0),
        level("15", "64% as core", "41% as support", -1.0),
        level("10", "47% Radiant", "55% Dire", 1.0),
    ];
    sparse.lines = lines(&["Your win rate each way."]);

    let mut pair = sparse.clone();
    pair.kind = "story_talents_pair".to_owned();
    pair.rows.remove(0);
    pair.lines.clear();
    vec![full, sparse, pair]
}

/// Bug fixes: four lines with portraits and a known issue, and two short
/// ones with nothing cached.
fn bug_fix_cards() -> Vec<WrappedSlideData> {
    let fix = |line: &str, hero: Option<&str>, tag: &str| {
        let mut row = tagged(line, "", tag, 0.0);
        row.image_png = hero.and_then(|slug| hero_image(slug, false));
        row
    };
    let mut full = card("story_bugfixes", "Bug fixes");
    full.rows = vec![
        fix(
            "Fixed Pudge dying 19 times in a single game.",
            Some("pudge"),
            "",
        ),
        fix("A 14k gold lead no longer counts as a win.", None, ""),
        fix(
            "Fixed Crystal Maiden losing the game in 17 minutes.",
            Some("crystal_maiden"),
            "",
        ),
        fix(
            "Queen of Pain win rate remains 31% after 13 games.",
            Some("queenofpain"),
            "KNOWN ISSUE",
        ),
    ];

    let mut sparse = styled(
        "story_bugfixes_sparse",
        "Bug fixes",
        WrappedLayout::BugFixes,
        WrappedTheme::PatchFixes,
    );
    sparse.rows = vec![
        fix("Fixed a 6-game losing streak.", None, ""),
        fix("Map pings still average 61 per game.", None, "KNOWN ISSUE"),
    ];
    vec![full, sparse]
}

/// Signed changes, a long profile, a sparse history, and an earned superlative.
fn patch_gallery() -> Vec<WrappedSlideData> {
    let mut changes = card("story_patch", "Balance changes");
    changes.headline = "You got\npatched.".to_owned();
    changes.rows = rows(&[
        ("More gold", "400 to 650 gold a minute"),
        ("More deaths", "7.1 to 9.2 a game"),
        ("More wins", "50% to 83% of games"),
    ]);
    changes.rows[0].value = 1.0;
    changes.rows[1].value = -1.0;
    changes.rows[2].value = 1.0;
    changes.lines = lines(&["Your first 6 games vs your last 6"]);

    let mut long = changes.clone();
    long.kind = "story_patch_long".to_owned();
    long.username = "Alexandria the Unreasonably Determined".to_owned();
    let matches = (1..=12)
        .map(|id| DistinctMatch {
            match_id: id,
            discord_id: 7,
            finished_at: Some(id * 86_400),
            gpm: (!matches!(id, 1 | 12)).then_some(if id <= 6 { 400 } else { 650 }),
            deaths: Some(if id <= 6 { 8 } else { 4 }),
            wards: Some(if id <= 6 { 3 } else { 8 }),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let measured = personal_patch(&matches, 7, None).changes;
    long.rows = measured.iter().map(change_row).collect();
    long.lines = measured.first().map(compared).into_iter().collect();

    let mut sparse = card("story_patch_sparse", "Balance changes");
    sparse.layout = WrappedLayout::PatchNotes;
    sparse.theme = WrappedTheme::Patch;
    sparse.headline = "You got\npatched.".to_owned();
    sparse.username = "New arrival".to_owned();
    sparse.rows = rows(&[("More gold", "400 to 450 gold a minute")]);
    sparse.rows[0].value = 1.0;
    sparse.lines = lines(&["Your first 5 games vs your last 5"]);

    let mut medal = styled(
        "story_patch_award",
        "Stuns and assists",
        WrappedLayout::Award,
        WrappedTheme::Patch,
    );
    medal.stat_value = "17 games".to_owned();
    medal.stat_label = "60+ stun seconds and 20+ assists in 17 of 48 games.".to_owned();
    let mut alternate = medal.clone();
    alternate.theme = WrappedTheme::Awards;
    alternate.title = "Wins on both sides".to_owned();
    alternate.stat_value = "67% / 80%".to_owned();
    alternate.stat_label = "Won 67% on Radiant and 80% on Dire.".to_owned();
    // The medal wears the item an award is named for; without one it is a crest.
    medal.art_png = icon_image("items", "basher");
    let mut richest = alternate.clone();
    richest.kind = "more_story_patch_award".to_owned();
    richest.title = "Richest in the game".to_owned();
    richest.stat_value = "9 games".to_owned();
    richest.stat_label = "Highest net worth in 9 of 20 games.".to_owned();
    richest.art_png = icon_image("items", "hand_of_midas");
    vec![changes, long, sparse, medal, alternate, richest]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("provide an output directory")?,
    );
    let arguments = std::env::args().collect::<Vec<_>>();
    let flag = |name: &str| arguments.iter().any(|argument| argument == name);
    let value = |name: &str| {
        let position = arguments.iter().position(|argument| argument == name)?;
        arguments.get(position + 1)
    };
    let (still_only, patch_only) = (flag("--still"), flag("--patch-only"));
    let only = value("--only");
    let seed = value("--seed")
        .map(|seed| seed.parse::<u64>())
        .transpose()?;
    let frames = value("--frames")
        .map(|list| {
            list.split(',')
                .map(str::parse::<f32>)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    std::fs::create_dir_all(&output)?;
    let assets = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets");
    for (index, slide) in deck(assets).iter_mut().enumerate() {
        let patch_chapter = slide.theme == WrappedTheme::Patch
            || slide.kind.ends_with("story_patch_award")
            || matches!(
                slide.layout,
                WrappedLayout::HeroPatch
                    | WrappedLayout::ItemPatch
                    | WrappedLayout::Talents
                    | WrappedLayout::BugFixes
            );
        if (patch_only && !patch_chapter) || only.is_some_and(|only| !slide.kind.contains(only)) {
            continue;
        }
        slide.seed = seed.unwrap_or(0);
        let slide = &*slide;
        let stem = format!("{index:02}-{}", slide.kind);
        std::fs::write(
            output.join(format!("{stem}.png")),
            render_wrapped_slide(slide)?,
        )?;
        for progress in &frames {
            let name = format!("{stem}-at-{:03}.png", (progress * 100.0).round() as i32);
            std::fs::write(output.join(name), render_wrapped_frame(slide, *progress)?)?;
        }
        if still_only {
            continue;
        }
        let started = Instant::now();
        let media = render_wrapped_media(slide, true)?;
        println!(
            "{stem}: {} {} KiB in {} ms",
            media.filename,
            media.bytes.len() / 1024,
            started.elapsed().as_millis()
        );
        if media.filename.ends_with(".gif") {
            std::fs::write(output.join(format!("{stem}.gif")), &media.bytes)?;
        }
    }
    Ok(())
}
