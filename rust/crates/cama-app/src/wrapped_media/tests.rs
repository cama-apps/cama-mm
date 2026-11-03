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

const ALL_KINDS: [&str; 16] = [
    "story_intro",
    "story_identity",
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

const ALL_LAYOUTS: [WrappedLayout; 11] = [
    WrappedLayout::Cover,
    WrappedLayout::BigNumber,
    WrappedLayout::TopList,
    WrappedLayout::Bars,
    WrappedLayout::People,
    WrappedLayout::Curve,
    WrappedLayout::Tiles,
    WrappedLayout::Spotlight,
    WrappedLayout::Persona,
    WrappedLayout::Recap,
    WrappedLayout::Details,
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
    ] {
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.theme = WrappedTheme::Queue;
        slide.rows = vec![WrappedRow::new("7", "times"), WrappedRow::new("9", "wins")];
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif");
        assert!(media.bytes.len() < MAX_ANIMATION_BYTES);
        let (frames, delays) = play(&media.bytes);
        assert!(frames.len() > 10, "{layout:?} actually moves");
        assert!(frames.len() <= motion::FRAMES);
        assert_ne!(frames[0], *frames.last().unwrap());
        assert!(*delays.last().unwrap() >= 300, "the finished card holds");
        assert!(
            delays[..delays.len() - 1].iter().all(|delay| *delay >= 2),
            "no frame is too fast for clients to honor"
        );

        // The last frame is exactly the still card seen through the shared
        // palette: every pixel holds the nearest color to the still's pixel.
        let decoder = gif::DecodeOptions::new()
            .read_info(media.bytes.as_slice())
            .unwrap();
        let palette = decoder.global_palette().unwrap().to_vec();
        // The final slot means "unchanged" and is never drawn.
        let colors = &palette[..palette.len() - 3];
        let still = stage::render_still(&slide);
        let mut nearest = std::collections::HashMap::new();
        let expected = still
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
            .collect::<Vec<_>>();
        assert!(
            *frames.last().unwrap() == expected,
            "{layout:?} final frame is not the still card"
        );
    }
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
