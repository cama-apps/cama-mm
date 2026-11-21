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

fn patch_slide() -> WrappedSlideData {
    let mut slide = WrappedSlideData::new("story_patch", "Balance changes");
    slide.headline = "You got\npatched.".to_owned();
    slide.username = "The Last Support".to_owned();
    slide.rows = vec![
        WrappedRow::new("More wards", "12.4 to 18.7 a game"),
        WrappedRow::new("More deaths", "7.1 to 9.2 a game"),
        WrappedRow::new("Same hero pool", "24 to 24 heroes"),
    ];
    slide.rows[0].value = 1.0;
    slide.rows[1].value = -1.0;
    slide.lines = vec!["First 24 games vs last 24.".to_owned()];
    slide
}

#[test]
fn patch_notes_show_each_fact_and_distinguish_buff_nerf_and_rework() {
    let slide = patch_slide();
    let full = stage::render_still(&slide).pixels;
    for index in 0..3 {
        let mut changed = slide.clone();
        changed.rows[index].secondary = "A different measured result".to_owned();
        assert!(
            full != stage::render_still(&changed).pixels,
            "fact {index} is visible"
        );
        changed = slide.clone();
        changed.rows[index].value = if index == 0 { -1.0 } else { 1.0 };
        assert!(
            full != stage::render_still(&changed).pixels,
            "sign {index} changes its glyph"
        );
    }
    let mut overflow = slide.clone();
    overflow
        .rows
        .push(WrappedRow::new("Off-card", "Must not overlap the footer"));
    assert!(full == stage::render_still(&overflow).pixels);
}

#[test]
fn patch_notes_reveal_rows_in_order_before_the_final_hold() {
    let slide = patch_slide();
    let mut stage = stage::Stage::new(&slide);
    let early = stage.paint(0.35).pixels.clone();
    let late = stage.paint(0.9).pixels.clone();
    let row_band = |pixels: &[u8], top: usize, bottom: usize| {
        pixels[top * WRAPPED_SLIDE_WIDTH * 4..bottom * WRAPPED_SLIDE_WIDTH * 4].to_vec()
    };
    let mut changed = slide.clone();
    changed.rows[2].primary = "A third change".to_owned();
    let mut other = stage::Stage::new(&changed);
    assert!(row_band(&early, 600, 740) == row_band(&other.paint(0.35).pixels, 600, 740));
    assert!(row_band(&late, 600, 740) != row_band(&other.paint(0.9).pixels, 600, 740));
    assert!(
        row_band(&late, 80, 840) == row_band(&stage.paint(1.0).pixels, 80, 840),
        "the complete card holds still"
    );
}

#[test]
fn patch_cover_keeps_the_courier_colors_and_player_identity() {
    let mut slide = WrappedSlideData::new("story_intro", "Your gameplay update");
    slide.theme = WrappedTheme::Patch;
    slide.username = "Alexandria".to_owned();
    let courier = decode_png(include_bytes!(
        "../../../../../assets/wrapped/patch-courier.png"
    ))
    .unwrap();
    let art = artwork::prepare(&slide).expect("bundled patch courier");
    assert_eq!((art.width, art.height), (720, 480));
    assert!(
        art.pixels == courier.pixels,
        "the art retains its ink and flat colors"
    );
    let before = stage::render_still(&slide).pixels;
    slide.username = "A different player".to_owned();
    assert!(before != stage::render_still(&slide).pixels);
}

#[test]
fn patch_notes_and_medal_evidence_fit_at_readable_sizes() {
    let (title_width, title_height, title_size) = patches::CHANGE_TITLE;
    let (value_width, value_height, value_size) = patches::CHANGE_VALUE;
    for (text, face, design, smallest, width, height) in [
        // The longest title and the widest values a balance change shows.
        (
            "Fewer last hits early",
            stage::Face::Bold,
            title_size,
            title_size,
            title_width,
            title_height,
        ),
        (
            "128,000 to 128,000 a game",
            stage::Face::Medium,
            value_size,
            value_size,
            value_width,
            value_height,
        ),
        (
            "400 to 650 gold a minute",
            stage::Face::Medium,
            value_size,
            value_size,
            value_width,
            value_height,
        ),
        (
            "60+ stun seconds and 20+ assists in 17 of 48 games.",
            stage::Face::Medium,
            27.0,
            25.0,
            600,
            108,
        ),
    ] {
        let (size, lines) = stage::fit_text(text, face, design, width, height);
        assert!(
            size >= smallest,
            "{text:?} must be legible on a phone: {size}"
        );
        assert!(lines.iter().all(|line| !line.ends_with('…')));
        assert!(stage::block_height(lines.len(), size, face) <= height as f32);
    }
}

#[test]
fn patch_cover_and_awards_finish_on_the_same_still_and_fit_discord() {
    let mut cover = WrappedSlideData::new("story_intro", "Your gameplay update");
    cover.theme = WrappedTheme::Patch;
    cover.stat_value = "20.26".to_owned();
    cover.headline = "The Bloodbath\nUpdate".to_owned();
    cover.stat_label = "Alexandria".to_owned();
    let mut award = WrappedSlideData::new("story_patch_award", "Tower breaker");
    award.layout = WrappedLayout::Award;
    award.theme = WrappedTheme::Awards;
    award.stat_value = "12 games".to_owned();
    award.stat_label = "8,000+ tower damage in 12 of 48 games.".to_owned();
    award.art_png = Some(textured(88, 64));
    for slide in [cover, award] {
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif");
        assert!(media.bytes.len() < MAX_ANIMATION_BYTES);
        let (frames, delays) = play(&media.bytes);
        // The award keeps the short reveal; the cover takes its own pace.
        let most = if slide.layout == WrappedLayout::Award {
            motion::FRAMES
        } else {
            motion::pace(&slide).0
        };
        assert!(frames.len() > 10 && frames.len() <= most);
        assert!(*frames.last().unwrap() == still_through_palette(&media.bytes, &slide));
        assert_eq!(*delays.last().unwrap(), 600);
        assert_eq!(
            render_wrapped_media(&slide, false).unwrap().filename,
            "wrapped_slide.png"
        );
    }
}

#[test]
fn measured_patch_changes_keep_each_line_whole_at_its_design_size() {
    let games = (1..=12)
        .map(|id| cama_domain::wrapped_distinct::DistinctMatch {
            match_id: id,
            discord_id: 7,
            finished_at: Some(id * 86_400),
            gpm: (!matches!(id, 1 | 12)).then_some(if id <= 6 { 400 } else { 650 }),
            deaths: Some(if id <= 6 { 8 } else { 4 }),
            wards: Some(if id <= 6 { 3 } else { 8 }),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let report = crate::wrapped_story::personal_patch::personal_patch(&games, 7, None);
    assert_eq!(report.changes.len(), 3);
    for change in report.changes {
        for (copy, face, (width, height, design)) in [
            (&change.title, stage::Face::Bold, patches::CHANGE_TITLE),
            (&change.value, stage::Face::Medium, patches::CHANGE_VALUE),
        ] {
            let (size, lines) = stage::fit_text(copy, face, design, width, height);
            assert_eq!(size, design, "{}: {copy}", change.key);
            assert_eq!(lines, [copy.as_str()], "{} wraps", change.key);
        }
    }
}

const ALL_KINDS: [&str; 17] = [
    "story_hero_patch",
    "story_items",
    "story_talents",
    "story_bugfixes",
    "story_intro",
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
    "chart_rating",
    "chart_gamba",
];

const ALL_LAYOUTS: [WrappedLayout; 17] = [
    WrappedLayout::HeroPatch,
    WrappedLayout::ItemPatch,
    WrappedLayout::Talents,
    WrappedLayout::BugFixes,
    WrappedLayout::Cover,
    WrappedLayout::BigNumber,
    WrappedLayout::TopList,
    WrappedLayout::Bars,
    WrappedLayout::Curve,
    WrappedLayout::Tiles,
    WrappedLayout::Spotlight,
    WrappedLayout::Persona,
    WrappedLayout::Award,
    WrappedLayout::Recap,
    WrappedLayout::Details,
    WrappedLayout::TwoBeat,
    WrappedLayout::PatchNotes,
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
        WrappedLayout::PatchNotes,
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
        for theme in ROAST_PALETTES.into_iter().chain(POPOFF_PALETTES) {
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
    // A plain punchline keeps the render beside its type, still animated
    // and still ending on exactly the finished card.
    let mut plain = two_beat(WrappedPunch::Plain);
    plain.art_png = Some(full_render());
    let media = render_wrapped_media(&plain, true).unwrap();
    assert_eq!(media.filename, "wrapped_slide.gif");
    assert!(media.bytes.len() <= MAX_ANIMATION_BYTES);
    let (frames, _) = play(&media.bytes);
    assert!(*frames.last().unwrap() == still_through_palette(&media.bytes, &plain));
}

/// A 256x144 opaque portrait, the art a card wears before a render is cached.
fn portrait() -> Vec<u8> {
    encode_png(&Canvas::new(256, 144, Rgba::rgb(160, 40, 60)))
}

#[test]
fn plain_punchline_keeps_the_hero_clear_of_its_type() {
    for punch in [
        "Stop picking it.",
        "The fountain knew you by name.",
        "Black King Bar didn't save you",
    ] {
        for (name, art) in [("render", full_render()), ("portrait", portrait())] {
            let mut slide = two_beat(WrappedPunch::Plain);
            slide.beat.as_mut().unwrap().punch = punch.to_owned();
            slide.art_png = Some(art);
            let mut stage = stage::Stage::new(&slide);
            let placed = stage
                .punch_art
                .take()
                .unwrap_or_else(|| panic!("no room for the {name} beside {punch:?}"));
            // The finished still carries the hero: the middle of its visible
            // part lands on the card exactly.
            let still = decode_png(&render_wrapped_slide(&slide).unwrap()).unwrap();
            let (width, height) = (placed.art.width, placed.art.height);
            let visible = height.min((stage::HEIGHT - placed.top) as usize);
            let (column, row) = (width / 2, visible / 2);
            let solid = &placed.art.pixels[(row * width + column) * 4..][..4];
            assert_eq!(solid[3], 255, "{name}: the middle of the hero is solid");
            let (x, y) = (placed.left as usize + column, placed.top as usize + row);
            assert_eq!(pixel(&still, x, y)[..3], solid[..3], "{name} on the still");

            // The same layout with an invisible hero shows where the type,
            // its chip and the footer name actually ink the card.
            stage.punch_art = Some(stage::PlacedArt {
                art: stage::PreparedArt {
                    width: 1,
                    height: 1,
                    pixels: vec![0; 4],
                },
                left: placed.left,
                top: placed.top,
                radius: 0.0,
            });
            let typed = stage.paint(1.0).pixels.clone();
            let background = beats::look(slide.theme, WrappedPunch::Plain).bg.0;
            let (card_width, card_height) = (WRAPPED_SLIDE_WIDTH, WRAPPED_SLIDE_HEIGHT);
            // Below the progress bar and year label.
            let inked = |x: usize, y: usize| {
                y >= 90 && typed[(y * card_width + x) * 4..][..3] != background[..3]
            };
            // Every inked pixel sits inside the boxes placement keeps clear.
            let taken = beats::punch_art_clearance(&slide).unwrap();
            for y in 0..card_height {
                for x in (0..card_width).filter(|x| inked(*x, y)) {
                    let (x, y) = (x as i32, y as i32);
                    let covered = taken.iter().any(|(left, top, right, bottom)| {
                        x >= left - 3 && x < right + 3 && y >= top - 3 && y < bottom + 3
                    });
                    assert!(
                        covered,
                        "{punch:?}: ink at ({x}, {y}) outside the clearance"
                    );
                }
            }
            // No visible pixel of the hero lands on or beside any ink.
            const NEAR: usize = 3;
            let mut near_ink = vec![false; card_width * card_height];
            for y in 0..card_height {
                for x in (0..card_width).filter(|x| inked(*x, y)) {
                    for ny in y.saturating_sub(NEAR)..(y + NEAR + 1).min(card_height) {
                        for nx in x.saturating_sub(NEAR)..(x + NEAR + 1).min(card_width) {
                            near_ink[ny * card_width + nx] = true;
                        }
                    }
                }
            }
            for (index, hero) in placed.art.pixels.chunks_exact(4).enumerate() {
                let x = placed.left + (index % width) as i32;
                let y = placed.top + (index / width) as i32;
                if hero[3] < 32 || !(0..card_width as i32).contains(&x) || y >= card_height as i32 {
                    continue;
                }
                assert!(
                    !near_ink[y as usize * card_width + x as usize],
                    "{punch:?}: the {name} touches the type at ({x}, {y})"
                );
            }
        }
    }
    // Meme punchlines keep their graphics to themselves.
    let mut graph = two_beat(WrappedPunch::GoldGraph);
    graph.art_png = Some(full_render());
    assert!(stage::Stage::new(&graph).punch_art.is_none());
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
    use stage::contrast;
    for treatment in WrappedPunch::ALL {
        for theme in ROAST_PALETTES.into_iter().chain(POPOFF_PALETTES) {
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
fn every_label_and_credit_fits_its_beat() {
    use crate::wrapped_story::roast_pools::{
        CopyVars, HERO_STORY_KINDS, PEOPLE_KINDS, hero_copy, people_copy, record_copy,
        side_split_copy, story_copy,
    };
    use cama_domain::wrapped_activity::ServerRank;
    use cama_domain::wrapped_distinct::{
        DistinctStory, HeroLine, HeroYear, METRIC_KEYS, PairStory, PersonalRecord, RECORD_KEYS,
        TRACKED_ITEMS, TableEnd, Theme, Tier, metric_directions,
    };
    // The longest hero, and long readable names with and without a break.
    const HERO: &str = "Keeper of the Light";
    const NAMES: [&str; 2] = ["Bartholomew Featherstone", "Bartholomew_Featherstone"];
    let vars = CopyVars {
        hero: Some(HERO.to_owned()),
        ..CopyVars::default()
    };
    let mut copies = Vec::new();
    for metric in METRIC_KEYS.iter().copied() {
        let items: Vec<Option<&str>> = if metric == "item" {
            TRACKED_ITEMS.iter().copied().map(Some).collect()
        } else {
            vec![None]
        };
        let display = match metric {
            "best_game" => "27 / 22 / 34",
            "night_owl" => "12:59 AM",
            "rating_swing" => "-23,400",
            "hero_pool" => "187 heroes",
            _ => "23,400",
        };
        for item in items {
            for direction in metric_directions(metric) {
                for tier in [Tier::Extreme, Tier::Strong, Tier::Notable] {
                    for end in [TableEnd::Top, TableEnd::Bottom] {
                        let story = DistinctStory {
                            metric,
                            theme: Theme::Chaos,
                            direction: *direction,
                            tier,
                            value: 2.0,
                            display: display.to_owned(),
                            rank: Some(ServerRank { rank: 187, of: 187 }),
                            end,
                            hero_id: Some(1),
                            match_id: Some(1),
                            item: item.map(str::to_owned),
                            score: 1.0,
                        };
                        copies.push(story_copy(&story, &vars, 7));
                    }
                }
            }
        }
    }
    let pair = PairStory {
        discord_id: 1,
        games: 187,
        wins: 99,
    };
    for kind in PEOPLE_KINDS {
        for name in NAMES {
            copies.push(people_copy(kind, &pair, Some(name), 7));
        }
    }
    for (key, direction) in RECORD_KEYS {
        let display = match key {
            "most_kills" => "99 kills",
            "most_deaths" => "99 deaths",
            "most_assists" => "99 assists",
            "best_gpm" => "1,400 GPM",
            "most_damage" => "123,400 damage",
            "most_wards" => "99 wards",
            "longest_game" | "fastest_win" => "123:59",
            "first_bloods" => "99 first bloods",
            _ => "99 Roshans",
        };
        let record = PersonalRecord {
            key,
            direction,
            value: 99,
            display: display.to_owned(),
            hero_id: Some(1),
            match_id: 1,
            finished_at: Some(0),
            won: Some(false),
            score: 1.0,
        };
        copies.push(record_copy(&record, Some(HERO), Some("Sep 30"), 7));
    }
    let line = HeroLine {
        hero_id: 1,
        games: 187,
        wins: 99,
    };
    for kind in &HERO_STORY_KINDS[..2] {
        copies.push(hero_copy(kind, &line, Some(HERO), 7));
    }
    let split = HeroYear {
        radiant: (187, 150),
        dire: (187, 20),
        ..HeroYear::default()
    };
    copies.extend(side_split_copy(&split, 7));
    assert!(copies.len() > 100, "{} copies", copies.len());
    // `Punch` mirrors `WrappedPunch` variant for variant.
    let media = |copy: &crate::wrapped_story::roast_pools::StoryCopy| {
        let name = format!("{:?}", copy.treatment);
        WrappedPunch::ALL
            .into_iter()
            .find(|punch| format!("{punch:?}") == name)
            .expect("a media twin")
    };
    let misfits = copies
        .iter()
        .filter(|copy| !two_beat_facts_fit(&copy.label, &copy.credit, media(copy)))
        .map(|copy| format!("{:?} | {} | {}", copy.treatment, copy.label, copy.credit))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(misfits.is_empty(), "{misfits:#?}");
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
fn authored_art_decodes_with_alpha_within_the_art_size_limit() {
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
fn top_heroes_stand_the_main_hero_render_beside_the_heading() {
    let mut slide = WrappedSlideData::new("story_hero", "Your heroes");
    slide.stat_label = "12 heroes across 120 recorded picks".to_owned();
    slide.badge = "#1 by Anti-Mage games (40 players)".to_owned();
    slide.rows = (1..=5)
        .map(|index| WrappedRow::new(format!("Hero {index}"), "10 games · 60% wins"))
        .collect();
    let green = encode_png(&Canvas::new(256, 144, Rgba::rgb(0, 200, 0)));
    slide.rows[0].image_png = Some(green);
    let plain = stage::render_still(&slide);
    slide.art_png = Some(full_render());
    let prepared = stage::Stage::new(&slide);
    let art = prepared.art.as_ref().expect("the full render decodes");
    assert_eq!(
        (art.width, art.height),
        super::artwork::TOP_HERO_FRAME,
        "one head-and-shoulders tile"
    );
    let framed = stage::render_still(&slide);
    // The tile sits in the top-right corner, clear of the left text column.
    let changed = |left: usize, top: usize, right: usize, bottom: usize| {
        (top..bottom)
            .flat_map(|y| (left..right).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let offset = (y * framed.width + x) * 4;
                framed.pixels[offset..offset + 4] != plain.pixels[offset..offset + 4]
            })
            .count()
    };
    assert!(changed(470, 90, 670, 280) > 200 * 190 / 2);
    // The portrait rows still have room for their pictures.
    let portrait = framed
        .pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] == [0, 200, 0])
        .count();
    assert!(portrait > 128 * 72 * 9 / 10, "{portrait}");
    assert!(render_wrapped_media(&slide, true).is_ok());
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

    let mut slide = busy("story_persona");
    slide.layout = WrappedLayout::Persona;
    slide.theme = WrappedTheme::Persona;
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

#[test]
fn neighboring_cards_never_share_a_background() {
    // Four popoffs in a row, a fixed card that matches the default popoff
    // punchline, and roasts on both sides of a cobalt card.
    let mut deck = vec![WrappedSlideData::new("story_hero", "Your heroes")];
    for punch in [WrappedPunch::Plain; 4] {
        deck.push(two_beat(punch));
        deck.last_mut().unwrap().theme = WrappedTheme::BeatPopoff;
    }
    let mut yellow = WrappedSlideData::new("story_persona", "Your Cama type");
    yellow.layout = WrappedLayout::Persona;
    yellow.theme = WrappedTheme::Persona;
    deck.push(yellow);
    for punch in [
        WrappedPunch::Plain,
        WrappedPunch::Cinema,
        WrappedPunch::Plain,
    ] {
        deck.push(two_beat(punch));
        deck.last_mut().unwrap().theme = WrappedTheme::BeatRoast;
    }
    deck.push(WrappedSlideData::new("story_games", "Your Year"));
    deck.push(two_beat(WrappedPunch::Plain));
    deck.last_mut().unwrap().theme = WrappedTheme::BeatRoast;
    // A late-night clock and a gold graph end near-black, so the plain
    // popoff between them must not end on ink.
    for (punch, theme) in [
        (WrappedPunch::Clock, WrappedTheme::BeatRoastRed),
        (WrappedPunch::Plain, WrappedTheme::BeatPopoffLime),
        (WrappedPunch::GoldGraph, WrappedTheme::BeatRoastViolet),
    ] {
        deck.push(two_beat(punch));
        deck.last_mut().unwrap().theme = theme;
    }
    let kinds = deck
        .iter()
        .map(|slide| slide.theme.is_roast_beat())
        .collect::<Vec<_>>();

    vary_beat_palettes(&mut deck);

    for pair in deck.windows(2) {
        let (before, after) = (slide_backgrounds(&pair[0]), slide_backgrounds(&pair[1]));
        assert!(
            before
                .iter()
                .all(|color| after.iter().all(|other| !backgrounds_alike(*color, *other))),
            "{:?} then {:?} look alike: {before:?} {after:?}",
            pair[0].theme,
            pair[1].theme
        );
    }
    // Roasts stay roasts and popoffs stay popoffs, and a run of popoffs
    // rotates colors.
    assert_eq!(
        deck.iter()
            .map(|slide| slide.theme.is_roast_beat())
            .collect::<Vec<_>>(),
        kinds
    );
    let popoffs = deck[1..5]
        .iter()
        .map(|slide| slide.theme)
        .collect::<Vec<_>>();
    for (index, theme) in popoffs.iter().enumerate() {
        assert!(!popoffs[..index].contains(theme), "{popoffs:?}");
    }
    // The same deck always gets the same colors.
    let mut again = deck.clone();
    vary_beat_palettes(&mut again);
    assert_eq!(
        again.iter().map(|slide| slide.theme).collect::<Vec<_>>(),
        deck.iter().map(|slide| slide.theme).collect::<Vec<_>>()
    );
}

#[test]
fn near_black_backgrounds_count_as_alike() {
    let black = [0, 0, 0];
    let clock = beats::look(WrappedTheme::BeatRoast, WrappedPunch::Clock).bg;
    let graph = beats::look(WrappedTheme::BeatRoast, WrappedPunch::GoldGraph).bg;
    let ink = beats::look(WrappedTheme::BeatRoast, WrappedPunch::Plain).bg;
    // Black, a near-black clock face, a navy graph and ink all read as black.
    for pair in [
        (black, rgb(clock)),
        (rgb(clock), rgb(graph)),
        (rgb(graph), rgb(ink)),
    ] {
        assert!(backgrounds_alike(pair.0, pair.1), "{pair:?}");
    }
    // Small RGB steps between brighter colors look alike too; real contrast
    // does not.
    assert!(backgrounds_alike([200, 255, 60], [198, 244, 50]));
    assert!(!backgrounds_alike([255, 90, 54], [45, 70, 185]));
    assert!(!backgrounds_alike(black, [92, 33, 240]));
    assert!(!backgrounds_alike(rgb(graph), [62, 34, 18]));
    // Dark colors that are not quite black still read alike when their hue
    // is close: an ink ending beside a plum one. A dark green and a dark
    // brown stay apart.
    assert!(
        backgrounds_alike([20, 10, 46], [60, 12, 80]),
        "ink and plum"
    );
    assert!(
        !backgrounds_alike([11, 43, 29], [62, 34, 18]),
        "green and brown"
    );
    // A calculated bar's indigo beside a gold graph's navy read alike too:
    // the dark-meme spacing in the deck relies on it.
    assert!(
        backgrounds_alike([27, 17, 64], [15, 20, 32]),
        "indigo and navy"
    );
    // Two yellows read as one: the persona card and a yellow punchline.
    assert!(backgrounds_alike([255, 210, 30], [255, 225, 77]));
}

/// Whether a background reads as yellow: a bright, saturated hue between
/// orange and lime.
fn looks_yellow(color: [u8; 3]) -> bool {
    let [red, green, blue] = color.map(f32::from);
    let (high, low) = (red.max(green).max(blue), red.min(green).min(blue));
    if high < 200.0 || (high - low) / high < 0.5 {
        return false;
    }
    let hue = if high == red {
        60.0 * ((green - blue) / (high - low))
    } else if high == green {
        60.0 * ((blue - red) / (high - low) + 2.0)
    } else {
        60.0 * ((red - green) / (high - low) + 4.0)
    };
    (42.0..=64.0).contains(&hue)
}

#[test]
fn no_more_than_two_cards_end_on_yellow() {
    // A popoff-heavy deck with the yellow persona card in the middle: the
    // default popoff and the plum roast both end on yellow.
    let mut deck = vec![WrappedSlideData::new("story_hero", "Your heroes")];
    for index in 0..9 {
        deck.push(two_beat(WrappedPunch::Plain));
        deck.last_mut().unwrap().theme = if index % 3 == 2 {
            WrappedTheme::BeatRoastPlum
        } else {
            WrappedTheme::BeatPopoff
        };
        if index == 4 {
            let mut persona = WrappedSlideData::new("story_persona", "Your Cama type");
            persona.layout = WrappedLayout::Persona;
            persona.theme = WrappedTheme::Persona;
            deck.push(persona);
        }
    }
    vary_beat_palettes(&mut deck);
    let yellow = deck
        .iter()
        .filter(|slide| {
            slide_backgrounds(slide)
                .last()
                .copied()
                .is_some_and(looks_yellow)
        })
        .count();
    assert!(yellow <= 2, "{yellow} cards end on yellow");
    for pair in deck.windows(2) {
        let (before, after) = (slide_backgrounds(&pair[0]), slide_backgrounds(&pair[1]));
        assert!(
            !any_backgrounds_alike(&before, &after),
            "{before:?} {after:?}"
        );
    }
    assert!(looks_yellow([255, 225, 77]) && looks_yellow([255, 210, 30]));
    assert!(!looks_yellow([200, 255, 60]) && !looks_yellow([255, 245, 225]));
    assert!(!looks_yellow([255, 90, 54]) && !looks_yellow([255, 157, 230]));
}

#[test]
fn missing_portraits_leave_no_empty_box() {
    // A row without a cached portrait keeps its place in line but draws
    // nothing there, exactly as an invisible portrait would.
    let invisible = encode_png(&Canvas::new(256, 144, Rgba([0, 0, 0, 0])));
    for (kind, layout) in [
        ("story_finale", WrappedLayout::Recap),
        ("story_hero", WrappedLayout::TopList),
    ] {
        let mut slide = busy(kind);
        slide.layout = layout;
        slide.rows = (1..=3)
            .map(|index| WrappedRow::new(format!("Hero {index}"), "3 games"))
            .collect();
        slide.rows[0].image_png = Some(encode_png(&Canvas::new(256, 144, Rgba::rgb(0, 200, 0))));
        let missing = render_wrapped_slide(&slide).unwrap();
        slide.rows[1].image_png = Some(invisible.clone());
        slide.rows[2].image_png = Some(invisible.clone());
        assert!(
            missing == render_wrapped_slide(&slide).unwrap(),
            "{kind}: a missing portrait drew something"
        );
    }
}

#[test]
fn two_beat_cards_hold_each_beat_long_enough_to_read() {
    let frames = motion::frame_count(WrappedLayout::TwoBeat);
    let at = |seconds: f32| {
        let index = (seconds * 25.0).round() as usize;
        motion::frame_time(WrappedLayout::TwoBeat, index, frames)
    };
    // Setup fully in by 1 s and still showing at 5.5 s: four and a half
    // seconds to read it.
    assert!((at(1.0) - 0.30).abs() < 1e-4);
    assert!(at(5.5) < 0.48);
    // The wipe runs 5.6 s to 6.08 s, and the punchline settles by 7.2 s.
    assert!((at(5.6) - 0.48).abs() < 1e-4);
    assert!((at(6.08) - 0.56).abs() < 1e-4);
    assert!((at(7.2) - 0.92).abs() < 1e-4);
    assert_eq!(
        motion::frame_time(WrappedLayout::TwoBeat, frames - 1, frames),
        1.0
    );
    // Other cards keep their even pace.
    assert_eq!(motion::frame_time(WrappedLayout::BigNumber, 15, 31), 0.5);
    // The GIF plays about 7.5 s of reveal before its final hold.
    let media = render_wrapped_media(&two_beat(WrappedPunch::Plain), true).unwrap();
    let (_, delays) = play(&media.bytes);
    let reveal = delays[..delays.len() - 1]
        .iter()
        .map(|delay| u32::from(*delay))
        .sum::<u32>();
    assert!((700..=760).contains(&reveal), "{reveal} hundredths");
}

/// Art with busy detail: the worst case for a reveal's size.
fn textured(width: usize, height: usize) -> Vec<u8> {
    let mut image = Canvas::new(width, height, Rgba::rgb(0, 0, 0));
    let mut state = 0x2545_F491_u32;
    for pixel in image.pixels.chunks_exact_mut(4) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        pixel[..3].copy_from_slice(&state.to_le_bytes()[..3]);
    }
    encode_png(&image)
}

fn flat(width: usize, height: usize, color: [u8; 3]) -> Vec<u8> {
    let [red, green, blue] = color;
    encode_png(&Canvas::new(width, height, Rgba::rgb(red, green, blue)))
}

fn count_of(canvas: &Canvas, color: [u8; 3]) -> usize {
    canvas
        .pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] == color)
        .count()
}

fn tagged(primary: &str, secondary: &str, tag: &str, value: f64) -> WrappedRow {
    let mut row = WrappedRow::new(primary, secondary);
    row.tag = tag.to_owned();
    row.value = value;
    row
}

/// The four patch chapters at their fullest: every row, the longest hero and
/// item lines the story functions write, 80-character fixes, and detailed
/// art in every slot.
fn patch_chapters() -> Vec<WrappedSlideData> {
    use crate::wrapped_story::{hero_patch, item_patch};
    let mut changes = hero_patch::widest_lines();
    changes.sort_by_key(|line| std::cmp::Reverse(line.len()));
    let mut heroes = WrappedSlideData::new("story_hero_patch", "Hero updates");
    heroes.rows = [
        ("Keeper of the Light", "REMOVED", -1.0),
        ("Queen of Pain", "BUFFED", 1.0),
        ("Crystal Maiden", "NEW", 0.0),
    ]
    .into_iter()
    .zip(&changes)
    .map(|((hero, tag, value), line)| tagged(hero, line, tag, value))
    .collect();
    for row in &mut heroes.rows {
        row.image_png = Some(textured(256, 144));
        row.icons = vec![textured(128, 128); 4];
    }

    let mut lines = item_patch::widest_lines();
    lines.sort_by_key(|line| std::cmp::Reverse(line.len()));
    let mut items = WrappedSlideData::new("story_items", "Item updates");
    items.rows = [
        ("Black King Bar", "MOST BUILT", 0.0),
        ("Aghanim's Scepter", "LOSES WITH", -1.0),
        ("Observer Ward", "MOST BOUGHT", 0.0),
        ("Divine Rapier", "", 0.0),
    ]
    .into_iter()
    .zip(&lines)
    .map(|((item, tag, value), line)| tagged(item, line, tag, value))
    .collect();
    for row in &mut items.rows {
        row.image_png = Some(textured(88, 64));
    }

    let mut talents = WrappedSlideData::new("story_talents", "Talents");
    talents.rows = vec![
        tagged("100% under 35 min", "100% over 45 min", "25", -1.0),
        tagged("100% as favorite", "100% as underdog", "20", 1.0),
        tagged("100% as core", "100% as support", "15", 1.0),
        tagged("100% Radiant", "100% Dire", "10", -1.0),
    ];
    talents.lines = vec!["Your win rate each way.".to_owned()];

    let mut fixes = WrappedSlideData::new("story_bugfixes", "Bug fixes");
    fixes.rows = [
        "Fixed an issue where the player would buy back and immediately die again anyway.",
        "Fixed Black King Bar being purchased only after the team fight had already ended",
        "Fixed an issue where wards were placed exclusively on the high ground cliff. OK.",
        "Sonic Wave still occasionally targets the single enemy hero who is already dead.",
    ]
    .into_iter()
    .map(|line| tagged(line, "", "", 0.0))
    .collect();
    fixes.rows[3].tag = "KNOWN ISSUE".to_owned();
    for row in &mut fixes.rows {
        row.image_png = Some(textured(256, 144));
    }

    let mut cards = vec![heroes, items, talents, fixes];
    for card in &mut cards {
        card.username = "Alexandria".to_owned();
        card.year_label = "Cama Wrapped 2026".to_owned();
        card.section = card.title.clone();
        card.seed = 7;
    }
    cards
}

#[test]
fn patch_chapters_reveal_slowly_end_on_the_still_and_fit_discord() {
    for slide in patch_chapters() {
        let layout = slide.layout;
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif", "{layout:?}");
        assert!(
            media.bytes.len() < MAX_ANIMATION_BYTES,
            "{layout:?} is {} bytes",
            media.bytes.len()
        );
        let (frames, delays) = play(&media.bytes);
        let (most, delay) = motion::pace(&slide);
        assert_eq!(delay, 4, "{layout:?} plays at full frame rate");
        assert!(
            frames.len() > motion::FRAMES && frames.len() <= most,
            "{layout:?} plays {} frames",
            frames.len()
        );
        assert_ne!(frames[0], *frames.last().unwrap());
        assert!(
            *frames.last().unwrap() == still_through_palette(&media.bytes, &slide),
            "{layout:?} final frame is not the still card"
        );
        assert_eq!(*delays.last().unwrap(), 600, "the finished card holds");
        assert!(delays[..delays.len() - 1].iter().all(|delay| *delay >= 2));
        // Rows arrive one at a time over about four and a half seconds.
        let reveal = delays[..delays.len() - 1]
            .iter()
            .map(|delay| u32::from(*delay))
            .sum::<u32>();
        assert!(
            (380..=500).contains(&reveal),
            "{layout:?}: {reveal} hundredths"
        );
        assert_eq!(
            render_wrapped_media(&slide, false).unwrap().filename,
            "wrapped_slide.png"
        );
    }
}

#[test]
fn patch_chapters_settle_before_the_final_hold() {
    // Below the progress bar, which fills across the whole reveal, every
    // chapter is finished a moment before its last frame.
    let below_bar = 40 * WRAPPED_SLIDE_WIDTH * 4;
    for slide in patch_chapters() {
        let mut stage = stage::Stage::new(&slide);
        let early = stage.paint(0.5).pixels[below_bar..].to_vec();
        let held = stage.paint(0.985).pixels[below_bar..].to_vec();
        let done = &stage.paint(1.0).pixels[below_bar..];
        assert!(early != done, "{:?} reveals row by row", slide.layout);
        assert!(held == done, "{:?} is still moving", slide.layout);
    }
}

#[test]
fn section_tag_sits_on_the_year_line_and_nowhere_else() {
    let band = 44 * WRAPPED_SLIDE_WIDTH * 4..72 * WRAPPED_SLIDE_WIDTH * 4;
    for layout in ALL_LAYOUTS {
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.rows = vec![WrappedRow::new("7", "times"), WrappedRow::new("9", "wins")];
        let plain = stage::render_still(&slide);
        slide.section = "Hero updates".to_owned();
        let tagged = stage::render_still(&slide);
        assert!(
            plain.pixels[band.clone()] != tagged.pixels[band.clone()],
            "{layout:?} shows no section"
        );
        assert!(plain.pixels[..band.start] == tagged.pixels[..band.start]);
        assert!(plain.pixels[band.end..] == tagged.pixels[band.end..]);
        // The year label keeps the left of the line to itself.
        for y in 44..72 {
            let row = y * WRAPPED_SLIDE_WIDTH * 4;
            let left = row..row + 360 * 4;
            assert!(plain.pixels[left.clone()] == tagged.pixels[left]);
        }
        // A tag far too long for the line shrinks instead of covering it.
        slide.section = "An unreasonably long chapter name that keeps going".to_owned();
        let long = stage::render_still(&slide);
        for y in 44..72 {
            let row = y * WRAPPED_SLIDE_WIDTH * 4;
            let label = row..row + 280 * 4;
            assert!(plain.pixels[label.clone()] == long.pixels[label]);
        }
    }
}

/// The lowest contrast any letter of a section tag reaches against what is
/// behind it at `t`. A letter's solid strokes carry its ink, so each run of
/// tagged columns is read at its strongest pixel.
fn section_tag_contrast(slide: &WrappedSlideData, t: f32) -> f32 {
    let mut tagged = slide.clone();
    tagged.section = "Gameplay".to_owned();
    assert!(slide.section.is_empty());
    let behind = stage::Stage::new(slide).paint(t).pixels.clone();
    let shown = stage::Stage::new(&tagged).paint(t).pixels.clone();
    let color = |pixels: &[u8], x: usize, y: usize| {
        let offset = (y * WRAPPED_SLIDE_WIDTH + x) * 4;
        Rgba::rgb(pixels[offset], pixels[offset + 1], pixels[offset + 2])
    };
    let (mut letters, mut lowest, mut letter) = (0, f32::MAX, 0.0_f32);
    // The tag ends at the margin, so a clear column follows its last letter.
    for x in 0..WRAPPED_SLIDE_WIDTH {
        let strongest = (44..72)
            .map(|y| (color(&shown, x, y), color(&behind, x, y)))
            .filter(|(ink, surface)| ink != surface)
            .map(|(ink, surface)| stage::contrast(ink, surface))
            .reduce(f32::max);
        match strongest {
            Some(strongest) => letter = letter.max(strongest),
            // A clear column closes the letter before it.
            None if letter > 0.0 => {
                letters += 1;
                lowest = lowest.min(letter);
                letter = 0.0;
            }
            None => {}
        }
    }
    assert!(letters >= 6, "{:?}: {letters} letters at {t}", slide.theme);
    lowest
}

#[test]
fn section_tag_reads_on_every_two_beat_palette_through_both_beats() {
    let palettes = ROAST_PALETTES.into_iter().chain(POPOFF_PALETTES);
    let mut faint = Vec::new();
    let mut sample = |theme: WrappedTheme, treatment: WrappedPunch, moments: &[f32]| {
        let mut slide = two_beat(treatment);
        slide.theme = theme;
        for t in moments {
            let contrast = section_tag_contrast(&slide, *t);
            if contrast < 4.5 {
                faint.push(format!("{theme:?} {treatment:?} at {t}: {contrast:.1}"));
            }
        }
    };
    // Each palette's setup with its corner shape growing in and then held,
    // the wipe as its stripe crosses the tag, and the punchline.
    for theme in palettes.clone() {
        sample(theme, WrappedPunch::Plain, &[0.1, 0.4, 0.52, 1.0]);
    }
    // Each meme's own colors arriving over a setup and then finished; the
    // setup under them takes each palette in turn.
    for (treatment, theme) in WrappedPunch::ALL.into_iter().zip(palettes.clone().cycle()) {
        sample(theme, treatment, &[0.52, 1.0]);
    }
    assert!(faint.is_empty(), "{faint:#?}");

    // Whatever else ends up behind the tag, the ink it takes reads there.
    for theme in palettes {
        let p = stage::palette(theme);
        for treatment in WrappedPunch::ALL {
            let inks = [p.fg, beats::look(theme, treatment).quiet];
            for step in 0..=u8::MAX {
                for surface in [
                    Rgba::rgb(step, step, step),
                    Rgba::rgb(step, 0, u8::MAX - step),
                    Rgba::rgb(u8::MAX, step, 0),
                    Rgba::rgb(0, u8::MAX - step, step),
                ] {
                    for preferred in inks {
                        let ink = cards::readable_on(surface, &[preferred, p.on_accent]);
                        assert!(
                            stage::contrast(ink, surface) >= 4.5,
                            "{theme:?} {treatment:?}: {ink:?} on {surface:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_seed_moves_the_background_shapes_and_never_the_type() {
    use WrappedLayout::{
        Bars, BigNumber, BugFixes, Cover, Curve, HeroPatch, ItemPatch, Persona, Recap, Spotlight,
        Talents, Tiles, TopList,
    };
    for (layout, theme) in [
        (Cover, WrappedTheme::Cover),
        (BigNumber, WrappedTheme::Queue),
        (BigNumber, WrappedTheme::QueueAlt),
        (TopList, WrappedTheme::Heroes),
        (Bars, WrappedTheme::Months),
        (BigNumber, WrappedTheme::Squad),
        (BigNumber, WrappedTheme::Rivals),
        (Curve, WrappedTheme::Rating),
        (Curve, WrappedTheme::GambaUp),
        (BigNumber, WrappedTheme::Dig),
        (Spotlight, WrappedTheme::Pet),
        (BigNumber, WrappedTheme::PetGrave),
        (Tiles, WrappedTheme::Mafia),
        (Tiles, WrappedTheme::MafiaRed),
        (Persona, WrappedTheme::Persona),
        (Recap, WrappedTheme::Recap),
        (HeroPatch, WrappedTheme::PatchHeroes),
        (ItemPatch, WrappedTheme::PatchItems),
        (Talents, WrappedTheme::PatchTalents),
        (BugFixes, WrappedTheme::PatchFixes),
    ] {
        let mut slide = busy("custom");
        slide.layout = layout;
        slide.theme = theme;
        slide.rows = vec![WrappedRow::new("7", "times"), WrappedRow::new("9", "wins")];
        let plain = stage::render_still(&slide);
        slide.seed = 41;
        let first = stage::render_still(&slide);
        slide.seed = 9_000_000_007;
        let second = stage::render_still(&slide);
        assert!(plain.pixels != first.pixels, "{theme:?} ignores its seed");
        assert!(
            first.pixels != second.pixels,
            "{theme:?} looks the same for two players"
        );
        // Type is drawn over the shapes: every pixel of solid ink stays ink.
        let ink = stage::palette(theme).fg.0;
        for seeded in [&first, &second] {
            let kept = plain
                .pixels
                .chunks_exact(4)
                .zip(seeded.pixels.chunks_exact(4))
                .all(|(before, after)| before != ink || after == ink);
            assert!(kept, "{theme:?}: a seed moved the type");
        }
        // The same player always gets the same card.
        assert!(second.pixels == stage::render_still(&slide).pixels);
    }
    // A two-beat setup's corner shape moves too; its finished punchline,
    // which has no shape, does not.
    let mut beat = two_beat(WrappedPunch::Plain);
    let still = stage::render_still(&beat);
    let setup = stage::Stage::new(&beat).paint(0.4).pixels.clone();
    beat.seed = 41;
    assert!(still.pixels == stage::render_still(&beat).pixels);
    assert!(setup != stage::Stage::new(&beat).paint(0.4).pixels);
}

#[test]
fn patch_chapter_palettes_stand_apart_from_each_other_and_their_neighbors() {
    let chapters = [
        WrappedTheme::PatchHeroes,
        WrappedTheme::PatchItems,
        WrappedTheme::PatchTalents,
        WrappedTheme::PatchFixes,
    ];
    let all = [WrappedTheme::Patch].into_iter().chain(chapters);
    let colors = all.map(theme_background).collect::<Vec<_>>();
    for (index, first) in colors.iter().enumerate() {
        for second in &colors[index + 1..] {
            assert!(!backgrounds_alike(*first, *second), "{first:?} {second:?}");
        }
    }
    // Hero updates follow the top heroes card.
    assert!(!backgrounds_alike(
        theme_background(WrappedTheme::PatchHeroes),
        theme_background(WrappedTheme::Heroes)
    ));
    for theme in chapters {
        let fixed = [theme_background(theme)];
        // The chapters sit among two-beat cards. A meme punchline cannot
        // change color, so no chapter may look like one.
        for treatment in WrappedPunch::ALL {
            if treatment != WrappedPunch::Plain {
                let punch = rgb(beats::look(WrappedTheme::BeatRoast, treatment).bg);
                assert!(
                    !backgrounds_alike(fixed[0], punch),
                    "{theme:?} beside {treatment:?}"
                );
            }
        }
        // A plain two-beat neighbor still has palettes of its kind to pick.
        let free = |family: &[WrappedTheme]| {
            family
                .iter()
                .filter(|palette| {
                    let colors = beat_backgrounds(**palette, WrappedPunch::Plain);
                    !any_backgrounds_alike(&colors, &fixed)
                })
                .count()
        };
        assert!(free(&ROAST_PALETTES) >= 2, "{theme:?} roasts");
        assert!(free(&POPOFF_PALETTES) >= 2, "{theme:?} popoffs");
        // A chapter's card is a fixed card: nothing recolors it.
        let mut slide = WrappedSlideData::new("custom", "");
        slide.theme = theme;
        slide.layout = WrappedLayout::HeroPatch;
        assert_eq!(fixed_backgrounds(&slide), fixed);
        assert_eq!(slide_backgrounds(&slide), fixed);
    }
}

#[test]
fn patch_award_medal_wears_its_item_icon() {
    let green = [0, 200, 0];
    for kind in ["story_patch_award", "more_story_patch_award"] {
        for theme in [WrappedTheme::Patch, WrappedTheme::Awards] {
            let mut slide = WrappedSlideData::new(kind, "Fully committed");
            slide.layout = WrappedLayout::Award;
            slide.theme = theme;
            slide.stat_value = "3 Rapiers".to_owned();
            slide.stat_label = "Bought in 3 of 48 games. Lost all three.".to_owned();
            let crest = stage::render_still(&slide);
            assert_eq!(count_of(&crest, green), 0);
            slide.art_png = Some(flat(88, 64, green));
            let medal = stage::render_still(&slide);
            // The whole icon, enlarged, less its rounded corners.
            let (width, height) = patches::MEDAL_ICON;
            let shown = count_of(&medal, green);
            assert!(
                shown > width * height * 9 / 10 && shown <= width * height,
                "{shown}"
            );
            // Only the medal changes; the award's copy stays where it was.
            let copy = 350 * WRAPPED_SLIDE_WIDTH * 4;
            assert!(crest.pixels[copy..] == medal.pixels[copy..]);
            // The glint has crossed and gone by the end of the reveal.
            let mut stage = stage::Stage::new(&slide);
            assert!(count_of(stage.paint(0.67), green) < shown);
            assert_eq!(count_of(stage.paint(0.9), green), shown);
            // Art that does not decode leaves the crest.
            slide.art_png = Some(b"not png".to_vec());
            assert!(stage::render_still(&slide).pixels == crest.pixels);
        }
    }
    // Other awards keep their trophy and never read item art.
    let mut other = busy("story_superlative");
    let plain = stage::render_still(&other);
    other.art_png = Some(flat(88, 64, green));
    assert!(plain.pixels == stage::render_still(&other).pixels);
}

#[test]
fn patch_cover_downloads_the_update_then_resolves_into_its_title() {
    let mut slide = WrappedSlideData::new("story_intro", "Your gameplay update");
    slide.theme = WrappedTheme::Patch;
    slide.stat_value = "20.26".to_owned();
    slide.headline = "The Bloodbath\nUpdate".to_owned();
    slide.stat_label = "Alexandria".to_owned();
    let p = stage::palette(slide.theme);
    let at = |canvas: &Canvas, x: usize, y: usize| {
        canvas.pixels[(y * WRAPPED_SLIDE_WIDTH + x) * 4..][..4].to_vec()
    };
    let rows = |canvas: &Canvas, top: usize, bottom: usize| {
        canvas.pixels[top * WRAPPED_SLIDE_WIDTH * 4..bottom * WRAPPED_SLIDE_WIDTH * 4].to_vec()
    };
    let mut stage = stage::Stage::new(&slide);
    // Mid-download the bar is part full and the title has not arrived.
    let downloading = stage.paint(0.2).clone();
    assert_eq!(at(&downloading, 80, 610), p.accent.0, "the filled end");
    assert_eq!(at(&downloading, 640, 610), p.soft.0, "the empty end");
    assert!(
        rows(&downloading, 90, 300)
            .chunks_exact(4)
            .all(|pixel| pixel == p.bg.0),
        "the title waits for the download"
    );
    // The percentage keeps counting while it downloads.
    assert!(rows(&downloading, 340, 580) != rows(stage.paint(0.3), 340, 580));
    // The finished cover has no bar, and names the version, patch and player.
    let done = stage.paint(1.0).clone();
    assert!(done.pixels == stage::render_still(&slide).pixels);
    assert!(
        rows(&done, 340, 420)
            .chunks_exact(4)
            .all(|pixel| pixel != p.soft.0)
    );
    let changes: [fn(&mut WrappedSlideData); 3] = [
        |slide| slide.stat_value = "20.27".to_owned(),
        |slide| slide.headline = "The Quiet\nUpdate".to_owned(),
        |slide| slide.stat_label = "Someone else".to_owned(),
    ];
    for change in changes {
        let mut changed = slide.clone();
        change(&mut changed);
        let other = stage::render_still(&changed);
        assert!(rows(&done, 90, 540) != rows(&other, 90, 540));
        assert!(rows(&done, 560, 900) == rows(&other, 560, 900));
    }
}
