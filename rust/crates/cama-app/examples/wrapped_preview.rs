//! Export deterministic Wrapped artwork for visual review.
//! `cargo run -p cama-app --example wrapped_preview -- <output-directory> [--still]`
//!
//! Every name and number here is invented. `--still` skips the GIFs for a quick
//! layout pass. Hero portraits come from the Steam image cache that production
//! warms (`STEAM_IMAGE_CACHE_ROOT`, default `.cache`) when it is present.
//! Two-beat cards show the full hero render at `WRAPPED_PREVIEW_HERO_RENDER`
//! when that path is set.
use cama_app::drawing::{GambaInfo, GambaPoint, GambaStats};
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedBeat, WrappedGambaData, WrappedLayout, WrappedPunch, WrappedRow,
    WrappedSlideData, WrappedTheme, render_wrapped_media, render_wrapped_slide,
};
use std::path::PathBuf;
use std::time::Instant;

const PLAYER: &str = "Alexandria";

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

fn art(path: &str) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
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

/// One two-beat card. `art` places the hero render on it.
fn two_beat(
    theme: WrappedTheme,
    treatment: WrappedPunch,
    copy: [&str; 5],
    art: bool,
) -> WrappedSlideData {
    let [setup, value, setup_label, punch, credit] = copy;
    let mut slide = styled("story_distinct", "", WrappedLayout::TwoBeat, theme);
    slide.beat = Some(WrappedBeat {
        setup: setup.to_owned(),
        value: value.to_owned(),
        setup_label: setup_label.to_owned(),
        punch: punch.to_owned(),
        credit: credit.to_owned(),
        treatment,
    });
    if art {
        slide.art_png = std::env::var_os("WRAPPED_PREVIEW_HERO_RENDER")
            .and_then(|path| std::fs::read(path).ok());
    }
    slide
}

/// One two-beat card per punchline treatment.
fn two_beats() -> Vec<WrappedSlideData> {
    use WrappedPunch::*;
    use WrappedTheme::{BeatPopoff, BeatRoast};
    let plain = two_beat(
        BeatRoast,
        Plain,
        [
            "You played",
            "41",
            "games of Pudge.",
            "Hooks are optional.",
            "41 Pudge games · 37% wins",
        ],
        false,
    );
    let plain_popoff = two_beat(
        BeatPopoff,
        Plain,
        [
            "You won",
            "68%",
            "of your games with Mirana Main.",
            "Certified duo.",
            "28 of 41 games won together",
        ],
        false,
    );
    let cinema = two_beat(
        BeatPopoff,
        Cinema,
        [
            "Your best game",
            "27/2/14",
            "Queen of Pain · Mar 14",
            "Absolute cinema.",
            "27 / 2 / 14 · Queen of Pain",
        ],
        true,
    );
    let mut gold = two_beat(
        BeatRoast,
        GoldGraph,
        [
            "You were up",
            "23,400",
            "gold. Your team, anyway.",
            "this is fine.",
            "Team gold lead +23,400",
        ],
        true,
    );
    gold.series = vec![
        0.0, 250.0, 900.0, 1_800.0, 2_600.0, 4_100.0, 6_000.0, 8_200.0, 11_000.0, 13_500.0,
        16_900.0, 19_800.0, 23_400.0, 21_000.0, 16_500.0, 11_200.0, 6_800.0, 1_900.0, -2_600.0,
        -5_900.0, -9_400.0, -12_800.0,
    ];
    let mut stonks = two_beat(
        BeatPopoff,
        Stonks,
        [
            "You cashed out",
            "+8,420",
            "JC this year.",
            "Stonks.",
            "+8,420 JC across 96 bets",
        ],
        false,
    );
    stonks.series = (0..40)
        .map(|index| {
            let x = f64::from(index);
            x * 210.0 + (x / 2.3).sin() * 700.0
        })
        .collect();
    let mut not_stonks = two_beat(
        BeatRoast,
        NotStonks,
        [
            "You bet",
            "3,100",
            "JC on one game.",
            "Not stonks.",
            "-3,100 JC on one bet",
        ],
        false,
    );
    not_stonks.series = vec![
        4_200.0, 3_900.0, 4_300.0, 3_100.0, 3_300.0, 2_000.0, 2_300.0, 400.0,
    ];
    let mut calculated = two_beat(
        BeatPopoff,
        Calculated,
        [
            "You won at",
            "18%",
            "win odds.",
            "Calculated.",
            "Won at 18% odds · Mar 2",
        ],
        false,
    );
    calculated.stat_value = "18%".to_owned();
    let task_failed = two_beat(
        BeatRoast,
        TaskFailed,
        [
            "You went",
            "31/4/9",
            "on Queen of Pain. Then lost.",
            "Task failed successfully.",
            "31 / 4 / 9 · Queen of Pain · Defeat",
        ],
        true,
    );
    let mut losses = two_beat(
        BeatRoast,
        LossTiles,
        [
            "You lost",
            "9",
            "games in a row.",
            "gg go next",
            "9 straight losses · Feb 11",
        ],
        false,
    );
    losses.series = vec![-1.0; 9];
    let press_f = two_beat(
        BeatRoast,
        PressF,
        [
            "Your pets lost",
            "2",
            "lives this year.",
            "Press F.",
            "2 pets lost in 2026",
        ],
        false,
    );
    let skill = two_beat(
        BeatRoast,
        SkillIssue,
        [
            "You averaged",
            "1.8",
            "kills a game.",
            "Skill issue.",
            "Fewest kills per game · 40th of 40",
        ],
        false,
    );
    let clock = two_beat(
        BeatRoast,
        Clock,
        [
            "Your latest queue",
            "3:47 AM",
            "on a Tuesday.",
            "One more game.",
            "Queued at 3:47 AM · Apr 8",
        ],
        false,
    );
    vec![
        plain,
        plain_popoff,
        cinema,
        gold,
        stonks,
        not_stonks,
        calculated,
        task_failed,
        losses,
        press_f,
        skill,
        clock,
    ]
}

fn deck(assets: &str) -> Vec<WrappedSlideData> {
    let mut deck = Vec::new();

    let mut intro = card("story_intro", "Your year");
    intro.headline = "Your\n2026\nWrapped".to_owned();
    intro.stat_value = PLAYER.to_owned();
    deck.push(intro);

    let mut hours = styled(
        "story_moment",
        "Time played",
        WrappedLayout::BigNumber,
        WrappedTheme::Queue,
    );
    hours.headline = "One more game. Several times.".to_owned();
    hours.stat_value = "185h".to_owned();
    hours.stat_label = "in recorded matches".to_owned();
    hours.badge = "More match time than 89% of players".to_owned();
    hours.lines = lines(&["Queue time not included."]);
    hours.artwork = WrappedArtwork::Courier;
    deck.push(hours);

    let mut heroes = card("story_hero", "Top heroes");
    heroes.headline = "Your top heroes".to_owned();
    heroes.badge = "Most Queen of Pain games on the server".to_owned();
    heroes.rows = rows(&[
        ("Queen of Pain", "81 games · 62% wins"),
        ("Crystal Maiden", "46 games · 54% wins"),
        ("Dark Willow", "28 games · 61% wins"),
        ("Pudge", "19 games · 37% wins"),
        ("Keeper of the Light", "12 games · 50% wins"),
    ]);
    let portraits = cama_app::trivia_image_cache::production_trivia_cache_path().join("heroes");
    for (row, slug) in heroes.rows.iter_mut().zip([
        "queenofpain",
        "crystal_maiden",
        "dark_willow",
        "pudge",
        "keeper_of_the_light",
    ]) {
        row.image_png = std::fs::read(portraits.join(format!("{slug}.png"))).ok();
    }
    let recap_heroes = heroes.rows.clone();
    deck.push(heroes);

    // Exercise the production award policy, including a wide scoreline and
    // coverage copy, rather than maintaining a second set of award wording.
    let award_matches = vec![
        cama_app::wrapped_story::PersonalMomentMatch {
            won: Some(true),
            deaths: Some(0),
            kills: Some(8),
            assists: Some(42),
            ..Default::default()
        },
        cama_app::wrapped_story::PersonalMomentMatch {
            won: Some(false),
            kills: Some(24),
            deaths: Some(3),
            assists: Some(18),
            ..Default::default()
        },
        cama_app::wrapped_story::PersonalMomentMatch::default(),
    ];
    for award in cama_app::wrapped_story::personal_superlatives(&award_matches) {
        let mut slide = card("story_superlative", award.title);
        slide.headline = award.headline.to_owned();
        slide.stat_value = award.value;
        slide.stat_label = award.label.to_owned();
        slide.lines = vec![award.detail, award.coverage];
        deck.push(slide);
    }

    let mut months = card("story_months", "Games by month");
    months.headline = "You played most in March.".to_owned();
    months.stat_value = "38".to_owned();
    months.stat_label = "games in March; none in July".to_owned();
    months.rows = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"]
        .iter()
        .zip([
            12.0, 21.0, 38.0, 25.0, 19.0, 14.0, 3.0, 17.0, 29.0, 31.0, 4.0, 0.0,
        ])
        .map(|(label, games)| WrappedRow::bar(*label, games))
        .collect();
    deck.push(months);

    let mut wards = styled(
        "story_moment",
        "Pings",
        WrappedLayout::BigNumber,
        WrappedTheme::QueueAlt,
    );
    wards.headline = "The minimap heard you.".to_owned();
    wards.stat_value = "9,412".to_owned();
    wards.stat_label = "recorded pings".to_owned();
    wards.lines = lines(&["52 pings per recorded game."]);
    wards.artwork = WrappedArtwork::Ward;
    deck.push(wards);

    let mut rating = card("chart_rating", "Rating");
    rating.headline = "Up, down, and sideways.".to_owned();
    rating.stat_value = "+214".to_owned();
    rating.stat_label = "rating this year".to_owned();
    rating.series = (0..60)
        .map(|index| {
            let x = f64::from(index);
            1400.0 + x * 3.5 + (x / 4.0).sin() * 60.0 + (x / 9.0).cos() * 40.0
        })
        .collect();
    rating.lines = lines(&["Peak 1,702 in October."]);
    deck.push(rating);

    let mut gamba = card("chart_gamba", "Gamba");
    gamba.headline = "The house sends its regards.".to_owned();
    gamba.stat_value = "-4,520".to_owned();
    gamba.stat_label = "JC across 312 wagers".to_owned();
    gamba.theme = WrappedTheme::GambaDown;
    gamba.lines = lines(&["Peaked at +1,890. You kept going."]);
    let mut total = 0_i64;
    gamba.gamba = Some(WrappedGambaData {
        degen_score: 73,
        degen_title: "Gambling stats".to_owned(),
        points: (0..140)
            .map(|index| {
                total += if index < 50 {
                    38
                } else {
                    -48 + (index % 5) * 9
                };
                GambaPoint {
                    event_number: index as i32 + 1,
                    cumulative: total,
                    info: GambaInfo {
                        source: "bet".to_owned(),
                        outcome: Some("won".to_owned()),
                        leverage: 1,
                        profit: 0,
                    },
                }
            })
            .collect(),
        stats: GambaStats {
            total_bets: 140,
            win_rate: 0.42,
            net_pnl: total,
            roi: -0.2,
        },
    });
    deck.push(gamba);

    let mut degen = styled(
        "story_gamba",
        "Gambling stats",
        WrappedLayout::Tiles,
        WrappedTheme::GambaUp,
    );
    degen.headline = "Gambling stats".to_owned();
    degen.rows = rows(&[
        ("73", "degen score"),
        ("+2,400", "best single bet"),
        ("-3,100", "worst single bet"),
        ("41", "wheel spins"),
        ("3", "bankruptcies"),
        ("11x", "losses in a row"),
    ]);
    deck.push(degen);

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
    dig.lines = lines(&["37 cave-ins."]);
    deck.push(dig);

    let mut boss = styled(
        "story_dig_boss",
        "Boss you fought most",
        WrappedLayout::Spotlight,
        WrappedTheme::Dig,
    );
    boss.art_png = art(&format!("{assets}/dig/bosses/aegis_warden_victory.png"));
    boss.headline = "Aegis Warden".to_owned();
    boss.stat_value = "9 fights".to_owned();
    boss.stat_label = "2 wins, 7 losses".to_owned();
    deck.push(boss);

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
    deck.push(pet);

    let mut grave = styled(
        "story_pet_grave",
        "Pets lost",
        WrappedLayout::BigNumber,
        WrappedTheme::PetGrave,
    );
    grave.headline = "Not everyone made it.".to_owned();
    grave.stat_value = "2".to_owned();
    grave.stat_label = "pets lost this year".to_owned();
    grave.lines = lines(&["One starved. One was eaten."]);
    deck.push(grave);

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
    deck.push(mafia);

    let mut victim = styled(
        "story_mafia_victim",
        "Night kills",
        WrappedLayout::BigNumber,
        WrappedTheme::MafiaRed,
    );
    victim.headline = "Tusk Enjoyer".to_owned();
    victim.stat_value = "6".to_owned();
    victim.stat_label = "night kills".to_owned();
    deck.push(victim);

    let mut persona = styled(
        "story_persona",
        "Your year",
        WrappedLayout::Persona,
        WrappedTheme::Persona,
    );
    persona.stat_value = "Dig regular".to_owned();
    persona.headline = "You also played Dota.".to_owned();
    persona.rows = rows(&[
        ("1,284", "blocks dug"),
        ("247", "Dota games"),
        ("3", "bankruptcies"),
    ]);
    deck.push(persona);

    let mut finale = card("story_finale", "2026 Wrapped");
    finale.stat_value = PLAYER.to_owned();
    finale.headline = "Dig regular".to_owned();
    finale.badge = "Can't touch this".to_owned();
    finale.rows = recap_heroes;
    finale.lines = lines(&[
        "185h|in matches",
        "247|games",
        "1,284|blocks dug",
        "-4,520|JC gamba",
        "34|mafia games",
    ]);
    deck.push(finale);

    let mut records = card("records_combat", "Combat records");
    records.headline = "Your records".to_owned();
    records.lines = lines(&[
        "Most kills: 24 as Queen of Pain",
        "Most deaths: 19 as Pudge",
        "Best KDA: 18.0 as Crystal Maiden",
    ]);
    deck.push(records);

    deck.extend(two_beats());

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
