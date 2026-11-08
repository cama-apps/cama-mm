use super::*;

fn gamba_slide() -> WrappedSlideData {
    let mut slide = WrappedSlideData::new("chart_gamba", "Gamba");
    slide.username = "Wrapped Gambler".to_owned();
    slide.stat_value = "+60".to_owned();
    slide.lines = vec!["+60 JC · 6 bets · Degen Score: 73".to_owned()];
    slide.gamba = Some(WrappedGambaData {
        degen_score: 73,
        degen_title: "House Favorite".to_owned(),
        points: vec![
            GambaPoint {
                event_number: 1,
                cumulative: -20,
                info: crate::drawing::GambaInfo {
                    source: "bet".to_owned(),
                    outcome: Some("lost".to_owned()),
                    leverage: 1,
                    profit: -20,
                },
            },
            GambaPoint {
                event_number: 2,
                cumulative: 60,
                info: crate::drawing::GambaInfo {
                    source: "wheel".to_owned(),
                    outcome: Some("won".to_owned()),
                    leverage: 1,
                    profit: 80,
                },
            },
        ],
        stats: GambaStats {
            total_bets: 1,
            win_rate: 1.0,
            net_pnl: 60,
            roi: 0.5,
        },
    });
    slide
}

const ALL_KINDS: [&str; 17] = [
    "story_intro",
    "story_identity",
    "story_superlative",
    "story_moment",
    "story_finale",
    "server_summary",
    "awards",
    "story_games",
    "story_summary",
    "records_combat",
    "story_hero",
    "story_lanes",
    "story_teammates",
    "story_rivals",
    "story_packages",
    "chart_rating",
    "chart_gamba",
];

const ALL_LAYOUTS: [WrappedLayout; 13] = [
    WrappedLayout::Cover,
    WrappedLayout::BigNumber,
    WrappedLayout::TopList,
    WrappedLayout::Bars,
    WrappedLayout::People,
    WrappedLayout::Curve,
    WrappedLayout::Tiles,
    WrappedLayout::Spotlight,
    WrappedLayout::Persona,
    WrappedLayout::Award,
    WrappedLayout::Recap,
    WrappedLayout::Details,
    WrappedLayout::TwoBeat,
];

fn busy(kind: &str) -> WrappedSlideData {
    let mut slide = WrappedSlideData::new(kind, "Wrapped");
    slide.year_label = "Cama Wrapped 2026".to_owned();
    slide.headline = "Production semantic page".to_owned();
    slide.stat_value = "42".to_owned();
    slide.stat_label = "Games".to_owned();
    slide.lines = vec![
        "Alice - 10 games - 60%".to_owned(),
        "Bob - 8 games".to_owned(),
    ];
    slide.series = vec![1.0, 4.0, 2.0, 8.0];
    slide
}

fn pixel(image: &DecodedPng, x: usize, y: usize) -> [u8; 4] {
    image.pixels[(y * image.width + x) * 4..][..4]
        .try_into()
        .unwrap()
}

/// Decode a reveal into fully composited frames.
fn play(bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<u16>) {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::Indexed);
    let mut decoder = options.read_info(bytes).unwrap();
    let (width, height) = (decoder.width() as usize, decoder.height() as usize);
    assert_eq!((width, height), (WRAPPED_SLIDE_WIDTH, WRAPPED_SLIDE_HEIGHT));
    let mut canvas = vec![0_u8; width * height];
    let (mut frames, mut delays) = (Vec::new(), Vec::new());
    while let Some(frame) = decoder.read_next_frame().unwrap() {
        let (left, top) = (frame.left as usize, frame.top as usize);
        for row in 0..frame.height as usize {
            for column in 0..frame.width as usize {
                let index = frame.buffer[row * frame.width as usize + column];
                if Some(index) != frame.transparent {
                    canvas[(top + row) * width + left + column] = index;
                }
            }
        }
        frames.push(canvas.clone());
        delays.push(frame.delay);
    }
    (frames, delays)
}

#[test]
fn every_kind_and_layout_renders_a_portrait_rgba_png() {
    let kinds = ALL_KINDS.iter().map(|kind| busy(kind));
    let layouts = ALL_LAYOUTS.iter().map(|layout| {
        let mut slide = busy("custom");
        slide.layout = *layout;
        slide.rows = vec![WrappedRow::new("7", "times"), WrappedRow::new("9", "wins")];
        slide
    });
    for slide in kinds.chain(layouts) {
        let png = render_wrapped_slide(&slide).unwrap();
        let decoded = decode_png(&png).expect("native PNG");
        assert_eq!(
            (decoded.width, decoded.height),
            (WRAPPED_SLIDE_WIDTH, WRAPPED_SLIDE_HEIGHT),
            "{} {:?}",
            slide.kind,
            slide.layout
        );
        assert!(
            decoded.pixels.chunks_exact(4).all(|p| p[3] == 255),
            "final Discord image is opaque"
        );
    }
}

#[test]
fn every_layout_survives_empty_and_oversized_copy() {
    for layout in ALL_LAYOUTS {
        let mut empty = WrappedSlideData::new("custom", "");
        empty.layout = layout;
        assert!(render_wrapped_slide(&empty).is_ok(), "{layout:?} empty");

        let long = "Unreasonably determined and extremely long copy ".repeat(10);
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.title.clone_from(&long);
        slide.headline.clone_from(&long);
        slide.stat_value = "9,999,999,999".to_owned();
        slide.stat_label.clone_from(&long);
        slide.badge.clone_from(&long);
        slide.username.clone_from(&long);
        slide.lines = vec![long.clone(); 9];
        slide.rows = vec![WrappedRow::new(long.as_str(), long.as_str()); 20];
        assert!(render_wrapped_slide(&slide).is_ok(), "{layout:?} long");
    }
}

#[test]
fn themes_change_the_whole_card() {
    let mut a = WrappedSlideData::new("story_moment", "Habits");
    a.theme = WrappedTheme::Queue;
    let mut b = a.clone();
    b.theme = WrappedTheme::Mafia;
    let (a, b) = (
        decode_png(&render_wrapped_slide(&a).unwrap()).unwrap(),
        decode_png(&render_wrapped_slide(&b).unwrap()).unwrap(),
    );
    assert_ne!(pixel(&a, 20, 80), pixel(&b, 20, 80));
}

#[test]
fn reveal_is_bounded_moves_and_ends_on_exactly_the_still_card() {
    for layout in [
        WrappedLayout::BigNumber,
        WrappedLayout::Tiles,
        WrappedLayout::Curve,
        WrappedLayout::Award,
    ] {
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.theme = if layout == WrappedLayout::Award {
            WrappedTheme::Awards
        } else {
            WrappedTheme::Queue
        };
        slide.rows = vec![WrappedRow::new("7", "times"), WrappedRow::new("9", "wins")];
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif");
        assert!(media.bytes.len() < MAX_ANIMATION_BYTES);
        let (frames, delays) = play(&media.bytes);
        assert!(frames.len() > 10, "{layout:?} actually moves");
        assert!(frames.len() <= motion::FRAMES);
        assert_ne!(frames[0], *frames.last().unwrap());
        assert!(*delays.last().unwrap() >= 300, "the finished card holds");
        if layout == WrappedLayout::Award {
            assert!(
                delays[..delays.len() - 1].iter().sum::<u16>() >= 220,
                "the award setup and reveal need a full reading beat"
            );
            assert!(delays[..delays.len() - 1].iter().all(|delay| *delay >= 8));
        }
        assert!(
            delays[..delays.len() - 1].iter().all(|delay| *delay >= 2),
            "no frame is too fast for clients to honor"
        );

        assert!(
            *frames.last().unwrap() == still_through_palette(&media.bytes, &slide),
            "{layout:?} final frame is not the still card"
        );
    }
}

/// The still card seen through a reveal's shared palette: every pixel holds
/// the nearest palette color to the still's pixel.
fn still_through_palette(gif_bytes: &[u8], slide: &WrappedSlideData) -> Vec<u8> {
    let decoder = gif::DecodeOptions::new().read_info(gif_bytes).unwrap();
    let palette = decoder.global_palette().unwrap().to_vec();
    // The final slot means "unchanged" and is never drawn.
    let colors = &palette[..palette.len() - 3];
    let still = stage::render_still(slide);
    let mut nearest = std::collections::HashMap::new();
    still
        .pixels
        .chunks_exact(4)
        .map(|pixel| {
            *nearest
                .entry([pixel[0], pixel[1], pixel[2]])
                .or_insert_with(|| {
                    colors
                        .chunks_exact(3)
                        .enumerate()
                        .min_by_key(|(_, entry)| {
                            (0..3)
                                .map(|c| (i32::from(entry[c]) - i32::from(pixel[c])).pow(2))
                                .sum::<i32>()
                        })
                        .map(|(index, _)| index as u8)
                        .unwrap()
                })
        })
        .collect()
}

fn two_beat(treatment: WrappedPunch) -> WrappedSlideData {
    let mut slide = WrappedSlideData::new("story_distinct", "Biggest throw");
    slide.layout = WrappedLayout::TwoBeat;
    slide.theme = WrappedTheme::BeatRoast;
    slide.username = "Alexandria".to_owned();
    slide.year_label = "Cama Wrapped 2026".to_owned();
    slide.chapter_index = 4;
    slide.chapter_count = 14;
    slide.stat_value = "18%".to_owned();
    slide.series = vec![
        0.0, 900.0, 2_400.0, 6_100.0, 11_800.0, 23_400.0, 15_000.0, 4_200.0, -3_900.0, -9_000.0,
    ];
    slide.beat = Some(WrappedBeat {
        setup: "You were up".to_owned(),
        value: "23,400".to_owned(),
        setup_label: "gold. Your team, anyway.".to_owned(),
        punch: "this is fine.".to_owned(),
        credit: "Team gold lead +23,400".to_owned(),
        treatment,
    });
    slide
}

/// A 1440x1440 render-sized RGBA image: a gradient figure on transparency.
fn full_render() -> Vec<u8> {
    const SIDE: usize = 1440;
    let mut image = Canvas::new(SIDE, SIDE, Rgba([0, 0, 0, 0]));
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (dx, dy) = (x as f32 - 720.0, y as f32 - 760.0);
            if (dx / 520.0).powi(2) + (dy / 640.0).powi(2) <= 1.0 {
                let offset = (y * SIDE + x) * 4;
                image.pixels[offset..offset + 4].copy_from_slice(&[
                    (x * 255 / SIDE) as u8,
                    (y * 255 / SIDE) as u8,
                    ((x + y) * 255 / (SIDE * 2)) as u8,
                    255,
                ]);
            }
        }
    }
    encode_png(&image)
}

#[test]
fn two_beat_still_is_the_punchline() {
    let slide = two_beat(WrappedPunch::Plain);
    let still = decode_png(&render_wrapped_slide(&slide).unwrap()).unwrap();
    let setup = stage::palette(WrappedTheme::BeatRoast).bg;
    let punch = beats::look(WrappedTheme::BeatRoast, WrappedPunch::Plain).bg;
    assert_ne!(setup, punch, "the punchline flips the card's color");
    for (x, y) in [(6, 300), (6, 620), (714, 880)] {
        assert_eq!(pixel(&still, x, y), punch.0, "({x}, {y})");
    }
    let mut uncredited = slide.clone();
    uncredited.beat.as_mut().unwrap().credit.clear();
    assert_ne!(
        render_wrapped_slide(&slide).unwrap(),
        render_wrapped_slide(&uncredited).unwrap(),
        "the punchline still carries the fact"
    );
    let mut other_setup = slide.clone();
    other_setup.beat.as_mut().unwrap().setup = "Different setup".to_owned();
    assert_eq!(
        render_wrapped_slide(&slide).unwrap(),
        render_wrapped_slide(&other_setup).unwrap(),
        "the setup has left the finished card"
    );
}

#[test]
fn two_beat_gif_has_more_frames_and_ends_on_still() {
    for treatment in [WrappedPunch::GoldGraph, WrappedPunch::Cinema] {
        let mut slide = two_beat(treatment);
        slide.art_png = Some(encode_png(&Canvas::new(256, 144, Rgba::rgb(160, 40, 60))));
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif", "{treatment:?}");
        let (frames, delays) = play(&media.bytes);
        assert!(
            frames.len() > motion::FRAMES,
            "{treatment:?} plays a longer two-beat timeline: {} frames",
            frames.len()
        );
        assert!(*delays.last().unwrap() >= 300, "the punchline holds");
        assert!(
            *frames.last().unwrap() == still_through_palette(&media.bytes, &slide),
            "{treatment:?} final frame is not the still card"
        );
        // The opening frames show the setup's color, not the punchline's.
        let mut stage = stage::Stage::new(&slide);
        let setup = stage::palette(slide.theme).bg;
        assert_eq!(
            &stage.paint(0.4).pixels[(300 * 720 + 6) * 4..][..4],
            setup.0
        );
    }
}

#[test]
fn every_treatment_renders_without_oversize_error() {
    // Worst-case copy at the authored limits: a 28-character setup, a
    // 30-character punchline, and the longest placeholder values.
    let setup = "Keeper of the Light, again.";
    let punch = "Black King Bar didn't save you";
    assert!(setup.chars().count() <= 28 && punch.chars().count() <= 30);
    for treatment in WrappedPunch::ALL {
        for theme in [WrappedTheme::BeatRoast, WrappedTheme::BeatPopoff] {
            let mut slide = two_beat(treatment);
            slide.theme = theme;
            let beat = slide.beat.as_mut().unwrap();
            beat.setup = setup.to_owned();
            beat.punch = punch.to_owned();
            beat.value = "12:59 AM".to_owned();
            beat.setup_label = "Keeper of the Light · Mar 14".to_owned();
            beat.credit = "27 / 2 / 14 · KEEPER OF THE LIGHT".to_owned();
            let png = render_wrapped_slide(&slide).unwrap();
            let decoded = decode_png(&png).unwrap();
            assert!(decoded.pixels.chunks_exact(4).all(|p| p[3] == 255));
            // Missing chart data and art never break a treatment.
            slide.series.clear();
            slide.stat_value.clear();
            slide.art_png = Some(b"not png".to_vec());
            assert!(render_wrapped_slide(&slide).is_ok(), "{treatment:?} bare");
        }
    }
    let mut oversized = two_beat(WrappedPunch::Plain);
    oversized.beat.as_mut().unwrap().credit = "a".repeat(501);
    assert!(matches!(
        render_wrapped_slide(&oversized),
        Err(WrappedMediaError::OversizedText)
    ));
}

#[test]
fn two_beat_with_full_render_stays_under_cap() {
    let mut slide = two_beat(WrappedPunch::Cinema);
    slide.art_png = Some(full_render());
    let stage = stage::Stage::new(&slide);
    assert!(stage.art.is_some(), "the full render decodes for the setup");
    assert!(stage.still_art.is_some(), "and for the letterboxed still");
    let media = render_wrapped_media(&slide, true).unwrap();
    assert_eq!(media.filename, "wrapped_slide.gif");
    assert!(
        media.bytes.len() <= MAX_ANIMATION_BYTES,
        "{} bytes",
        media.bytes.len()
    );
}

#[test]
fn two_beat_punchlines_settle_before_the_final_hold() {
    // Below the progress bar, which fills across the whole reveal, the card
    // is finished by 0.92 even with a two-line punchline.
    let below_bar = 40 * WRAPPED_SLIDE_WIDTH * 4;
    for treatment in WrappedPunch::ALL {
        let mut slide = two_beat(treatment);
        slide.beat.as_mut().unwrap().punch = "Black King Bar didn't save you".to_owned();
        let mut stage = stage::Stage::new(&slide);
        let held = stage.paint(0.925).pixels[below_bar..].to_vec();
        assert!(
            held == stage.paint(1.0).pixels[below_bar..],
            "{treatment:?} is still moving during the hold"
        );
    }
}

#[test]
fn every_punchline_keeps_its_text_readable() {
    fn luminance(color: Rgba) -> f32 {
        let channel = |value: u8| {
            let value = f32::from(value) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.0[0]) + 0.7152 * channel(color.0[1]) + 0.0722 * channel(color.0[2])
    }
    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    for treatment in WrappedPunch::ALL {
        for theme in [WrappedTheme::BeatRoast, WrappedTheme::BeatPopoff] {
            let look = beats::look(theme, treatment);
            assert!(
                contrast(look.ink, look.bg) >= 4.5,
                "{treatment:?} punchline"
            );
            assert!(contrast(look.quiet, look.bg) >= 4.5, "{treatment:?} credit");
            assert!(
                contrast(look.signal, look.bg) >= 3.0,
                "{treatment:?} graphic"
            );
        }
    }
}

#[test]
fn two_beat_copy_fits_rejects_overlong_lines() {
    assert!(two_beat_copy_fits("You were up", "this is fine."));
    assert!(two_beat_copy_fits("", "Absolute cinema."));
    assert!(two_beat_copy_fits(
        "Keeper of the Light, again.",
        "Black King Bar didn't save you"
    ));
    assert!(!two_beat_copy_fits(
        "You were up a frankly unreasonable amount of gold that game",
        "this is fine."
    ));
    assert!(!two_beat_copy_fits(
        "You were up",
        "this is fine, absolutely fine, totally and completely fine."
    ));
    assert!(
        !two_beat_copy_fits("You were up", "Supercalifragilisticexpialidoc"),
        "a word too wide for the line would break mid-word"
    );
}

#[test]
fn long_words_shrink_instead_of_breaking() {
    // Each is one word wider than the column at some treatment's design
    // size, yet fits whole above that treatment's minimum.
    for punch in ["Calculated.", "Calculated. Obviously.", "Respectfully."] {
        assert!(two_beat_copy_fits("You won at", punch), "{punch}");
    }
}

#[test]
fn every_pool_line_fits_its_beat() {
    // The widest value each placeholder takes on a real card; names longer
    // than the inline limit only reach the credit line.
    let worst = [
        ("{n}", "23,400"),
        ("{hero}", "Keeper of the Light"),
        ("{rank}", "187"),
        ("{of}", "187"),
        ("{time}", "12:59 AM"),
        ("{item}", "Black King Bar"),
    ];
    let beats = crate::wrapped_story::roast_pools::all_beats();
    assert!(!beats.is_empty());
    let mut misfits = Vec::new();
    for beat in beats {
        for name in ["Alexandria", "Maximilianus"] {
            let fill = |line: &str| {
                worst
                    .iter()
                    .fold(line.replace("{name}", name), |text, (slot, value)| {
                        text.replace(slot, value)
                    })
            };
            let (setup, punch) = (fill(beat.setup), fill(beat.punch));
            if !two_beat_copy_fits(&setup, &punch) {
                misfits.push(format!("{setup} / {punch}"));
            }
        }
    }
    misfits.dedup();
    assert!(
        misfits.is_empty(),
        "{} misfits: {misfits:#?}",
        misfits.len()
    );
}

#[test]
fn reveal_stays_small_over_a_detailed_image() {
    let mut slide = busy("custom");
    slide.layout = WrappedLayout::Spotlight;
    slide.theme = WrappedTheme::Dig;
    slide.art_png =
        Some(include_bytes!("../../../../../assets/dig/bosses/aegis_warden_victory.png").to_vec());
    let media = render_wrapped_media(&slide, true).unwrap();
    assert_eq!(media.filename, "wrapped_slide.gif");
    assert!(
        media.bytes.len() < MAX_ANIMATION_BYTES,
        "{} bytes",
        media.bytes.len()
    );
}

#[test]
fn still_mode_and_detail_pages_stay_png() {
    let slide = busy("story_games");
    assert_eq!(
        render_wrapped_media(&slide, false).unwrap().filename,
        "wrapped_slide.png"
    );
    assert_eq!(
        render_wrapped_media(&busy("awards"), true)
            .unwrap()
            .filename,
        "wrapped_slide.png"
    );
}

#[test]
fn gamba_card_plots_the_typed_profit_curve() {
    let slide = gamba_slide();
    let with_curve = render_wrapped_slide(&slide).unwrap();
    let mut flat = slide.clone();
    flat.gamba = None;
    assert_ne!(with_curve, render_wrapped_slide(&flat).unwrap());
    let mut uppercase = slide;
    uppercase.lines[0] = uppercase.lines[0].to_ascii_uppercase();
    assert_ne!(
        with_curve,
        render_wrapped_slide(&uppercase).unwrap(),
        "authored casing reaches the card"
    );
}

#[test]
fn curve_downsamples_huge_histories_but_keeps_both_ends() {
    let mut slide = WrappedSlideData::new("chart_rating", "Rating");
    slide.series = (0..5_000).map(f64::from).collect();
    slide.series[10] = f64::NAN;
    let points = cards_curve_points(&slide);
    assert!(points.len() <= 96);
    assert_eq!(points[0], 0.0);
    assert_eq!(*points.last().unwrap(), 4_999.0);
    assert!(points.iter().all(|value| value.is_finite()));
}

fn cards_curve_points(slide: &WrappedSlideData) -> Vec<f64> {
    cards::curve_points(slide)
}

#[test]
fn malformed_avatar_is_fail_soft() {
    let mut slide = WrappedSlideData::new("story_teammates", "Teammates");
    slide.lines.push("Alice - 3 games".to_owned());
    slide.avatars.push(WrappedAvatar {
        discord_id: 1,
        png: b"not png".to_vec(),
    });
    assert!(render_wrapped_slide(&slide).is_ok());
}

#[test]
fn transparent_avatar_is_alpha_blended_and_clipped_to_a_circle() {
    let mut avatar_canvas = Canvas::new(2, 2, Rgba([255, 0, 0, 128]));
    avatar_canvas.set(1, 1, Rgba([0, 255, 0, 255]));
    let mut slide = WrappedSlideData::new("story_teammates", "Teammates");
    slide.rows.push(WrappedRow::new("Alice", "3 games"));
    slide.avatars.push(WrappedAvatar {
        discord_id: 1,
        png: encode_png(&avatar_canvas),
    });
    let with_avatar = decode_png(&render_wrapped_slide(&slide).unwrap()).unwrap();
    slide.avatars.clear();
    slide.rows[0].primary = "Alice".to_owned();
    let without = decode_png(&render_wrapped_slide(&slide).unwrap()).unwrap();
    assert_ne!(with_avatar.pixels, without.pixels);
    assert!(with_avatar.pixels.chunks_exact(4).all(|p| p[3] == 255));
}

#[test]
fn authored_art_decodes_with_alpha_without_relaxing_avatar_limits() {
    assert!(decode_png(&encode_png(&Canvas::new(1025, 1, Rgba::rgb(0, 0, 0)))).is_none());
    for bytes in [
        include_bytes!("../../../../../assets/wrapped/ward.png").as_slice(),
        include_bytes!("../../../../../assets/wrapped/courier.png").as_slice(),
    ] {
        assert!(bytes.len() <= 450 * 1024);
        let art = decode_png(bytes).expect("bundled artwork");
        assert_eq!((art.width, art.height), (512, 512));
        assert!(art.pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(art.pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }
    let bytes = include_bytes!("../../../../../assets/wrapped/battlefield.png");
    assert!(bytes.len() <= 900 * 1024);
    let scene = decode_png(bytes).expect("bundled battlefield");
    assert_eq!((scene.width, scene.height), (800, 600));
}

#[test]
fn character_art_shows_on_big_reveals() {
    let mut moment = WrappedSlideData::new("story_moment", "Your vision budget");
    let plain = render_wrapped_slide(&moment).unwrap();
    moment.artwork = WrappedArtwork::Ward;
    let ward = render_wrapped_slide(&moment).unwrap();
    moment.artwork = WrappedArtwork::Courier;
    let courier = render_wrapped_slide(&moment).unwrap();
    assert_ne!(plain, ward);
    assert_ne!(ward, courier);
}

#[test]
fn player_art_shows_on_spotlight_cards() {
    let mut slide = WrappedSlideData::new("story_pet", "Pet");
    slide.layout = WrappedLayout::Spotlight;
    let plain = render_wrapped_slide(&slide).unwrap();
    slide.art_png = Some(encode_png(&Canvas::new(64, 36, Rgba::rgb(9, 200, 40))));
    assert_ne!(plain, render_wrapped_slide(&slide).unwrap());
}

#[test]
fn top_heroes_show_portraits_and_keep_slots_for_missing_ones() {
    let mut slide = WrappedSlideData::new("story_hero", "On repeat");
    slide.headline = "Your top heroes".to_owned();
    slide.rows = (1..=3)
        .map(|index| WrappedRow::new(format!("Hero {index}"), "10 games"))
        .collect();
    let green = [0, 200, 0];
    slide.rows[0].image_png = Some(encode_png(&Canvas::new(256, 144, Rgba::rgb(0, 200, 0))));
    slide.rows[2].image_png = Some(b"not png".to_vec());
    let count = |slide: &WrappedSlideData| {
        decode_png(&render_wrapped_slide(slide).unwrap())
            .unwrap()
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == green)
            .count()
    };
    // Half-size portrait with rounded corners.
    let shown = count(&slide);
    assert!(shown > 128 * 72 * 9 / 10 && shown <= 128 * 72, "{shown}");
    for row in &mut slide.rows {
        row.image_png = None;
    }
    assert_eq!(count(&slide), 0);
}

#[test]
fn recap_shows_cached_hero_portraits_with_missing_images() {
    let mut slide = busy("story_finale");
    slide.rows = (1..=5)
        .map(|index| WrappedRow::new(format!("Hero {index}"), ""))
        .collect();
    slide.rows[0].image_png = Some(encode_png(&Canvas::new(256, 144, Rgba::rgb(0, 200, 0))));
    slide.rows[2].image_png = Some(b"not png".to_vec());
    let rendered = stage::render_still(&slide);
    let shown = rendered
        .pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] == [0, 200, 0])
        .count();
    assert!(shown > 80 * 45 * 9 / 10 && shown <= 80 * 45, "{shown}");
    let mut prepared = stage::Stage::new(&slide);
    prepared.paint(0.5);
    assert!(rendered.pixels == prepared.paint(1.0).pixels);
}

#[test]
fn coverage_labels_stay_visible_on_lists_and_bars() {
    for kind in ["story_hero", "story_lanes"] {
        let mut slide = WrappedSlideData::new(kind, "Recorded picks");
        slide.stat_label = "Recorded in 7 of 20 games".into();
        let labeled = render_wrapped_slide(&slide).unwrap();
        slide.stat_label.clear();
        assert_ne!(labeled, render_wrapped_slide(&slide).unwrap());
    }
}

#[test]
fn still_and_animation_reject_oversized_input_before_rendering() {
    let mut slide = WrappedSlideData::new("story_intro", "Wrapped");
    slide.username = "a".repeat(501);
    assert!(matches!(
        render_wrapped_slide(&slide),
        Err(WrappedMediaError::OversizedText)
    ));
    assert!(matches!(
        render_wrapped_media(&slide, true),
        Err(WrappedMediaError::OversizedText)
    ));
    let mut rows = WrappedSlideData::new("story_hero", "Wrapped");
    rows.rows.push(WrappedRow::new("a".repeat(501), ""));
    assert!(matches!(
        render_wrapped_slide(&rows),
        Err(WrappedMediaError::OversizedText)
    ));
}

#[test]
fn personal_award_reveals_its_title_and_keeps_only_essential_copy() {
    let mut slide = busy("story_superlative");
    assert_eq!(slide.layout, WrappedLayout::Award);
    assert_eq!(slide.theme, WrappedTheme::Awards);
    slide.title = "Escape artist".into();
    slide.headline = "You knew exactly when to leave.".into();
    let mut changed = slide.clone();
    changed.title = "Different award".into();
    changed.stat_value = "199".into();
    changed.lines = vec!["Different evidence".into()];
    let mut first = stage::Stage::new(&slide);
    let mut second = stage::Stage::new(&changed);
    assert_eq!(first.paint(0.1).pixels, second.paint(0.1).pixels);
    assert_ne!(first.paint(1.0).pixels, second.paint(1.0).pixels);

    let mut extra_copy = slide.clone();
    extra_copy.headline = "An unnecessary second headline".into();
    extra_copy.badge = "Your personal award".into();
    extra_copy.lines[0] = "A repeat of the same achievement".into();
    assert_eq!(
        stage::render_still(&slide).pixels,
        stage::render_still(&extra_copy).pixels,
        "the award should read as title, result and coverage"
    );

    // Even adversarial copy cannot enter the player's footer or leave its
    // evidence band. The coverage line changes its own rendered pixels.
    let long = "Unreasonably determined and extremely long copy ".repeat(10);
    changed.title.clone_from(&long);
    changed.headline.clone_from(&long);
    changed.stat_label.clone_from(&long);
    changed.badge.clone_from(&long);
    changed.lines = vec![long; 5];
    let long_card = stage::render_still(&changed);
    let footer = 828 * WRAPPED_SLIDE_WIDTH * 4;
    assert_eq!(
        first.paint(1.0).pixels[footer..],
        long_card.pixels[footer..]
    );
    let before = stage::render_still(&slide);
    slide.lines[1] = "Stats available in 12 of 40 games".into();
    let after = stage::render_still(&slide);
    assert_ne!(before.pixels, after.pixels);
    assert_eq!(
        before.pixels[..728 * WRAPPED_SLIDE_WIDTH * 4],
        after.pixels[..728 * WRAPPED_SLIDE_WIDTH * 4]
    );
}

#[test]
fn share_card_keeps_the_personal_award_visible() {
    let mut slide = busy("story_finale");
    let plain = stage::render_still(&slide);
    slide.badge = "Escape artist".into();
    let awarded = stage::render_still(&slide);
    assert_ne!(plain.pixels, awarded.pixels);
    let footer = 828 * WRAPPED_SLIDE_WIDTH * 4;
    assert_eq!(plain.pixels[footer..], awarded.pixels[footer..]);
}

#[test]
fn cover_reveals_the_bottom_of_the_illustration_inside_its_reserved_band() {
    let slide = WrappedSlideData::new("story_intro", "Wrapped");
    let mut stage = stage::Stage::new(&slide);
    let sky = Rgba::rgb(4, 88, 151);
    let foreground = Rgba::rgb(17, 210, 63);
    let mut art = Canvas::new(WRAPPED_SLIDE_WIDTH, 540, sky);
    for y in 480..540 {
        for x in 0..WRAPPED_SLIDE_WIDTH {
            art.set(x as i32, y, foreground);
        }
    }
    stage.art = Some(stage::PreparedArt {
        width: art.width,
        height: art.height,
        pixels: art.pixels,
    });
    let at = |canvas: &Canvas, y: usize| {
        canvas.pixels[(y * WRAPPED_SLIDE_WIDTH + 20) * 4..][..4].to_vec()
    };
    assert_ne!(at(stage.paint(0.0), 810), foreground.0);
    let final_card = stage.paint(1.0);
    assert_eq!(
        at(final_card, 810),
        foreground.0,
        "the lower subject remains visible"
    );
    assert_eq!(at(final_card, 700), sky.0);
    assert_eq!(at(final_card, 870), stage::palette(slide.theme).bg.0);
    assert!(
        final_card.pixels[836 * WRAPPED_SLIDE_WIDTH * 4..]
            .chunks_exact(4)
            .all(|pixel| pixel == stage::palette(slide.theme).bg.0),
        "the empty cover footer is solid behind the player name"
    );
    assert_ne!(
        at(final_card, 599),
        sky.0,
        "the illustration cannot enter the text band"
    );
}

#[test]
fn story_cards_have_one_heading_without_losing_their_data() {
    for layout in [
        WrappedLayout::BigNumber,
        WrappedLayout::TopList,
        WrappedLayout::Bars,
        WrappedLayout::People,
        WrappedLayout::Curve,
        WrappedLayout::Tiles,
    ] {
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.title = "Your heroes".into();
        slide.rows = vec![WrappedRow::bar("Axe", 7.0)];
        let first = stage::render_still(&slide);
        slide.headline = "A redundant subtitle about your heroes".into();
        assert_eq!(
            first.pixels,
            stage::render_still(&slide).pixels,
            "{layout:?} adds no subtitle"
        );
        slide.title = "Your most played heroes".into();
        assert_ne!(
            first.pixels,
            stage::render_still(&slide).pixels,
            "{layout:?} keeps the heading"
        );
    }

    let mut slide = busy("story_identity");
    slide.stat_value = "Dota regular".into();
    slide.rows = vec![WrappedRow::new("42", "games")];
    let first = stage::render_still(&slide);
    slide.title = "Your activity profile".into();
    slide.headline = "You played quite a lot of Dota".into();
    assert_eq!(first.pixels, stage::render_still(&slide).pixels);
    slide.rows[0].primary = "64".into();
    assert_ne!(
        first.pixels,
        stage::render_still(&slide).pixels,
        "activity evidence remains visible"
    );
}

#[test]
fn cover_and_recap_remove_repeated_labels_but_keep_sparse_disclosure() {
    let mut cover = WrappedSlideData::new("story_intro", "Your year");
    cover.stat_value = "Alexandria".into();
    let first = stage::render_still(&cover);
    cover.username = "Alexandria".into();
    cover.title = "Another introductory label".into();
    assert_eq!(
        first.pixels,
        stage::render_still(&cover).pixels,
        "the cover names the player once"
    );
    cover.lines = vec!["No games recorded for you this year. Here are the server stats.".into()];
    assert_ne!(
        first.pixels,
        stage::render_still(&cover).pixels,
        "empty-history scope stays visible"
    );

    let mut recap = busy("story_finale");
    let first = stage::render_still(&recap);
    recap.title = "Another Wrapped title".into();
    recap.stat_label = "See you next queue!".into();
    assert_eq!(
        first.pixels,
        stage::render_still(&recap).pixels,
        "the recap needs neither repeated year nor closing subtitle"
    );

    let mut spotlight = busy("custom");
    spotlight.layout = WrappedLayout::Spotlight;
    spotlight.headline = "Mochi".into();
    let first = stage::render_still(&spotlight);
    spotlight.title = "Your pet".into();
    assert_eq!(
        first.pixels,
        stage::render_still(&spotlight).pixels,
        "the named spotlight needs no category subtitle"
    );
}
