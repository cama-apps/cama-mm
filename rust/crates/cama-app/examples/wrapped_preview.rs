//! Export deterministic Wrapped artwork for visual review.
//! `cargo run -p cama-app --example wrapped_preview -- <output-directory> [--still]`
//!
//! Every name and number here is invented. `--still` skips the GIFs for a quick
//! layout pass. Hero portraits come from the Steam image cache that production
//! warms (`STEAM_IMAGE_CACHE_ROOT`, default `.cache`) when it is present.
use cama_app::drawing::{GambaInfo, GambaPoint, GambaStats};
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedGambaData, WrappedLayout, WrappedRow, WrappedSlideData, WrappedTheme,
    render_wrapped_media, render_wrapped_slide,
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

fn deck(assets: &str) -> Vec<WrappedSlideData> {
    let mut deck = Vec::new();

    let mut intro = card("story_intro", "This one is yours");
    intro.headline = "Your\n2026\nWrapped".to_owned();
    intro.stat_value = PLAYER.to_owned();
    intro.lines = lines(&["The wins. The bad picks. The people who queued anyway."]);
    deck.push(intro);

    let mut hours = styled(
        "story_moment",
        "Time well spent?",
        WrappedLayout::BigNumber,
        WrappedTheme::Queue,
    );
    hours.headline = "One more game. Several times.".to_owned();
    hours.stat_value = "185h".to_owned();
    hours.stat_label = "in the Ancient business".to_owned();
    hours.badge = "More than 89% of the server".to_owned();
    hours.lines = lines(&["That's 7.7 straight days. Queue time not included."]);
    hours.artwork = WrappedArtwork::Courier;
    deck.push(hours);

    let mut heroes = card("story_hero", "On repeat");
    heroes.headline = "Your top heroes".to_owned();
    heroes.badge = "#1 Queen of Pain on the server".to_owned();
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
        slide.badge = "Your personal superlative".to_owned();
        slide.lines = vec![award.detail, award.coverage];
        deck.push(slide);
    }

    let mut months = card("story_months", "Your year in queue");
    months.headline = "March had you in a chokehold.".to_owned();
    months.stat_value = "38".to_owned();
    months.stat_label = "games in March. July was suspiciously quiet.".to_owned();
    months.rows = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"]
        .iter()
        .zip([
            12.0, 21.0, 38.0, 25.0, 19.0, 14.0, 3.0, 17.0, 29.0, 31.0, 4.0, 0.0,
        ])
        .map(|(label, games)| WrappedRow::bar(*label, games))
        .collect();
    deck.push(months);

    let mut style = card("story_identity", "Your playstyle");
    style.stat_value = "The mapmaker".to_owned();
    style.headline = "You saw the bigger picture. The team saw nothing.".to_owned();
    style.lines = lines(&["14.2 wards a game, recorded in 180 of 247 games."]);
    deck.push(style);

    let mut wards = styled(
        "story_moment",
        "You had notes",
        WrappedLayout::BigNumber,
        WrappedTheme::QueueAlt,
    );
    wards.headline = "The minimap heard you.".to_owned();
    wards.stat_value = "9,412".to_owned();
    wards.stat_label = "recorded pings".to_owned();
    wards.lines = lines(&["52 a game. A very interactive presentation."]);
    wards.artwork = WrappedArtwork::Ward;
    deck.push(wards);

    let mut duo = card("story_teammates", "Your duo of the year");
    duo.headline = "Ride or die".to_owned();
    duo.rows = rows(&[
        ("Mirana Main", "41 games together · 68% wins"),
        ("Tusk Enjoyer", "33 games together · 58% wins"),
        ("definitely not smurf", "27 games together · 44% wins"),
    ]);
    deck.push(duo);

    let mut rival = card("story_rivals", "Your nemesis");
    rival.headline = "They live in your head.".to_owned();
    rival.rows = rows(&[
        ("Tusk Enjoyer", "Beat you 19 of 26 times"),
        ("Mirana Main", "You own them: 15 of 21"),
    ]);
    deck.push(rival);

    let mut rating = card("chart_rating", "Your rating arc");
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
        degen_title: "Certified Degen".to_owned(),
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
        "Degen report",
        WrappedLayout::Tiles,
        WrappedTheme::GambaUp,
    );
    degen.headline = "Certified Degen".to_owned();
    degen.rows = rows(&[
        ("73", "degen score"),
        ("+2,400", "best single bet"),
        ("-3,100", "worst single bet"),
        ("41", "wheel spins"),
        ("3", "bankruptcies"),
        ("11x", "loss chase streak"),
    ]);
    degen.lines = lines(&["You and the wheel are on a first-name basis."]);
    deck.push(degen);

    let mut dig = styled(
        "story_dig",
        "Dig",
        WrappedLayout::BigNumber,
        WrappedTheme::DigDeep,
    );
    dig.headline = "You went looking for trouble.".to_owned();
    dig.stat_value = "1,284".to_owned();
    dig.stat_label = "blocks down, deepest at 412".to_owned();
    dig.badge = "You out-dug 92% of the server".to_owned();
    dig.lines = lines(&["Caved in on yourself 37 times. Kept digging."]);
    deck.push(dig);

    let mut boss = styled(
        "story_dig_boss",
        "Your white whale",
        WrappedLayout::Spotlight,
        WrappedTheme::Dig,
    );
    boss.art_png = art(&format!("{assets}/dig/bosses/aegis_warden_victory.png"));
    boss.headline = "Aegis Warden".to_owned();
    boss.stat_value = "9 fights".to_owned();
    boss.stat_label = "It won 7. You won the last one.".to_owned();
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
    pet.lines = lines(&["Won 12 brawls. Still judges you."]);
    deck.push(pet);

    let mut grave = styled(
        "story_pet_grave",
        "In loving memory",
        WrappedLayout::BigNumber,
        WrappedTheme::PetGrave,
    );
    grave.headline = "Not everyone made it.".to_owned();
    grave.stat_value = "2".to_owned();
    grave.stat_label = "pets lost this year".to_owned();
    grave.lines = lines(&["One starved. One got eaten. You got a new one the next day."]);
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
        ("7", "got away with it"),
        ("3", "times MVP"),
    ]);
    mafia.lines = lines(&["Your friends voted you out 11 times. They were right 8."]);
    deck.push(mafia);

    let mut victim = styled(
        "story_mafia_victim",
        "Nothing personal",
        WrappedLayout::BigNumber,
        WrappedTheme::MafiaRed,
    );
    victim.headline = "Tusk Enjoyer should sleep with one eye open.".to_owned();
    victim.stat_value = "6".to_owned();
    victim.stat_label = "times you came for them".to_owned();
    deck.push(victim);

    let mut persona = styled(
        "story_persona",
        "Your Cama type",
        WrappedLayout::Persona,
        WrappedTheme::Persona,
    );
    persona.stat_value = "The Night Shift Mole".to_owned();
    persona.headline = "You treat sleep as a suggestion and the ground as a rival.".to_owned();
    persona.rows = rows(&[
        ("1,284", "blocks dug"),
        ("61%", "of games after midnight"),
        ("3", "bankruptcies, zero regrets"),
    ]);
    deck.push(persona);

    let mut finale = card("story_finale", "2026 Wrapped");
    finale.stat_value = PLAYER.to_owned();
    finale.headline = "The Night Shift Mole".to_owned();
    finale.badge = "Personal award: Escape Artist".to_owned();
    finale.rows = rows(&[
        ("Queen of Pain", ""),
        ("Crystal Maiden", ""),
        ("Dark Willow", ""),
        ("Pudge", ""),
        ("Keeper of the Light", ""),
    ]);
    finale.lines = lines(&[
        "185h|in queue",
        "247|games",
        "1,284|blocks dug",
        "-4,520|JC gamba",
        "34|mafia games",
    ]);
    finale.stat_label = "See you next queue.".to_owned();
    deck.push(finale);

    let mut records = card("records_combat", "Combat records");
    records.headline = "Your highest highs".to_owned();
    records.lines = lines(&[
        "Most kills: 24 as Queen of Pain",
        "Most deaths: 19 as Pudge (we saw)",
        "Best KDA: 18.0 as Crystal Maiden",
    ]);
    deck.push(records);

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
