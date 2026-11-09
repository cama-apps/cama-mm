//! Export deterministic Wrapped artwork for visual review.
//! `cargo run -p cama-app --example wrapped_preview -- <output-directory> [--still]`
//!
//! Every name and number here is invented. The deck follows the order
//! `/wrapped` sends it in and takes its jokes from the real copy pools, then
//! adds one card for each punchline treatment the deck did not show. `--still`
//! skips the GIFs for a quick layout pass. Hero portraits and full-body renders
//! come from the Steam image cache that production warms
//! (`STEAM_IMAGE_CACHE_ROOT`, default `.cache`) when it is present.
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedBeat, WrappedLayout, WrappedPunch, WrappedRow, WrappedSlideData,
    WrappedTheme, render_wrapped_media, render_wrapped_slide,
};
use cama_app::wrapped_story::roast_pools::{
    CopyVars, Punch, StoryCopy, hero_copy, people_copy, persona_title, record_copy, side_game_copy,
    story_copy,
};
use cama_domain::wrapped_activity::ServerRank;
use cama_domain::wrapped_distinct::{
    Direction, DistinctStory, HeroLine, PairStory, PersonalRecord, TableEnd, Theme, Tier,
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
    intro.headline = "Your\n2026\nWrapped".to_owned();
    intro.stat_value = PLAYER.to_owned();
    deck.push(intro);

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

    let (distinct, stories) = distinct_cards();
    deck.extend(distinct);
    deck.extend(personal_cards());
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

    let count = deck.len();
    for (index, slide) in deck.iter_mut().enumerate() {
        slide.chapter_index = index;
        slide.chapter_count = count;
    }
    deck
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("provide an output directory")?,
    );
    let still_only = std::env::args().any(|argument| argument == "--still");
    std::fs::create_dir_all(&output)?;
    let assets = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets");
    for (index, slide) in deck(assets).iter().enumerate() {
        let stem = format!("{index:02}-{}", slide.kind);
        std::fs::write(
            output.join(format!("{stem}.png")),
            render_wrapped_slide(slide)?,
        )?;
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
