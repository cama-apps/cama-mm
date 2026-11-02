//! Export deterministic Wrapped artwork for visual review.
//! `cargo run -p cama-app --example wrapped_preview -- <output-directory>`
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedGambaData, WrappedSlideData, render_wrapped_media, render_wrapped_slide,
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("provide an output directory")?,
    );
    std::fs::create_dir_all(&output)?;
    type Preview = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    );
    let cases: &[Preview] = &[
        (
            "story_intro",
            "This one is yours",
            "2026. You had to be there.",
            "",
            "",
            &["The wins, the questionable picks, the people who queued again."],
        ),
        (
            "story_games",
            "One more game?",
            "You had a recurring appointment.",
            "247",
            "games played",
            &[
                "More than 89% of this year's players.",
                "At this point, the queue should pay you rent.",
            ],
        ),
        (
            "story_identity",
            "Your playstyle",
            "You gave the team the bigger picture.",
            "The mapmaker",
            "your style in the recorded games",
            &[
                "312 observer and sentry wards. You lit up the map; the team supplied the plot twists.",
                "Based on ward data from 42 of 47 games.",
            ],
        ),
        (
            "story_moment",
            "The Vision Budget",
            "The map wasn't going to light itself.",
            "1,234",
            "recorded wards placed",
            &[
                "Observer and sentry wards. Someone had to fund the vision board. 14 per recorded game.",
                "Recorded in 100 of 120 games",
            ],
        ),
        (
            "story_hero",
            "Your heavy rotation",
            "Queen of Pain",
            "81 picks",
            "33% of 247 recorded hero picks",
            &[
                "Queen of Pain · 81 picks · 62% win · 4.2 KDA",
                "Crystal Maiden · 46 picks · 54% win · 2.8 KDA",
                "Dark Willow · 28 picks · 61% win · 3.9 KDA",
            ],
        ),
        (
            "awards",
            "The lobby superlatives",
            "A little recognition.",
            "",
            "",
            &[
                "Alexandria the Unreasonably Determined · The Visionary · 312 wards · You lit up the map. The team still found a way to wander into the dark.",
                "Morgan · Comeback Kid · 8 comeback wins · Apparently the first half was just the warm-up.",
            ],
        ),
        (
            "story_teammates",
            "Your regulars",
            "The party was the point.",
            "",
            "",
            &[
                "Alexandria the Unreasonably Determined · 41 games · 63% win",
                "Morgan · 28 games · 54% win",
                "Elliott · 19 games · 68% win",
            ],
        ),
        (
            "story_finale",
            "This was your year.",
            "The mapmaker",
            "Alexandria",
            "Save this card. See you next queue.",
            &[
                "247 games · 132 wins",
                "185h 42m recorded match time",
                "On repeat: Queen of Pain · 81 picks",
                "23 heroes in your recorded rotation",
                "312 observer and sentry wards. You lit up the map; the team supplied the plot twists.",
            ],
        ),
        (
            "story_intro",
            "A story is starting.",
            "Small sample.\nBig entrance.",
            "",
            "",
            &["3 games played. A little early to call it a personality."],
        ),
        (
            "story_moment",
            "Express delivery",
            "Somebody had to carry the parcels.",
            "27",
            "recorded courier takedowns",
            &[
                "You intercepted the delivery. Their shopping spree became your highlight reel.",
                "Recorded in 64 of 72 matches.",
            ],
        ),
    ];
    for (index, (kind, title, headline, value, label, lines)) in cases.iter().enumerate() {
        let mut slide = WrappedSlideData::new(*kind, *title);
        slide.artwork = match index {
            1 | 9 => WrappedArtwork::Courier,
            2 | 3 => WrappedArtwork::Ward,
            _ => WrappedArtwork::Rune,
        };
        slide.username = if index == 8 {
            "Éloïse-The-Extraordinarily-Long-Player-Name"
        } else {
            "Alexandria"
        }
        .into();
        slide.year_label = if *kind == "story_teammates" {
            "Your regulars · All-time"
        } else {
            "Cama Wrapped 2026"
        }
        .into();
        slide.headline = (*headline).into();
        slide.stat_value = (*value).into();
        slide.stat_label = (*label).into();
        slide.lines = lines.iter().map(|s| (*s).into()).collect();
        slide.chapter_index = index;
        slide.chapter_count = cases.len();
        std::fs::write(
            output.join(format!("{index:02}-{kind}.png")),
            render_wrapped_slide(&slide)?,
        )?;
        if index == 1 {
            let media = render_wrapped_media(&slide, true)?;
            std::fs::write(output.join(media.filename), media.bytes)?;
        }
    }
    let mut gamba = WrappedSlideData::new("chart_gamba", "The financial subplot · All-time");
    gamba.username = "Alexandria".into();
    gamba.year_label = "Cama Wrapped 2026".into();
    gamba.lines = vec!["+60 JC · 6 bets · Degen Score: 73".into()];
    gamba.gamba = Some(WrappedGambaData {
        degen_score: 73,
        degen_title: "House Favorite".into(),
        points: vec![
            cama_app::drawing::GambaPoint {
                event_number: 1,
                cumulative: -20,
                ..Default::default()
            },
            cama_app::drawing::GambaPoint {
                event_number: 2,
                cumulative: 60,
                ..Default::default()
            },
        ],
        stats: cama_app::drawing::GambaStats {
            total_bets: 6,
            win_rate: 0.5,
            net_pnl: 60,
            roi: 0.5,
        },
    });
    std::fs::write(
        output.join("09-chart_gamba.png"),
        render_wrapped_slide(&gamba)?,
    )?;
    let mut package = WrappedSlideData::new("story_packages", "Package deals");
    package.username = "Alexandria".into();
    package.year_label = "Cama Wrapped 2026".into();
    package.headline = "Friendship has a price".into();
    package.stat_value = "18 deals".into();
    package.stat_label = "Contract energy".into();
    package.lines = [
        "10 bought by you · 240 JC",
        "8 bought on you · 180 JC",
        "6 unique buyers",
        "90 games committed",
        "The group chat has a procurement department.",
    ]
    .iter()
    .map(|s| (*s).into())
    .collect();
    std::fs::write(
        output.join("10-story_packages.png"),
        render_wrapped_slide(&package)?,
    )?;
    Ok(())
}
