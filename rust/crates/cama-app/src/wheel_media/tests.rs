use std::io::Cursor;

use super::*;
use crate::wheel::{WheelEconomyPolicy, get_wheel_wedges};

#[test]
fn wheel_and_explosion_media_are_valid_multi_frame_gifs() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let wheel = render_wheel_gif(&wedges, 3, false, None)
        .expect("wheel gif")
        .bytes;
    let explosion = render_explosion_gif().expect("explosion gif");
    let expected_wheel_delays = (0..WHEEL_MEDIA_FRAME_COUNT)
        .map(wheel_frame_delay_ms)
        .collect::<Vec<_>>();
    let expected_explosion_delays = (0..EXPLOSION_MEDIA_FRAME_COUNT)
        .map(explosion_frame_delay_ms)
        .collect::<Vec<_>>();
    for (bytes, expected_frames, expected_delays) in [
        (
            wheel,
            WHEEL_MEDIA_FRAME_COUNT,
            expected_wheel_delays.as_slice(),
        ),
        (
            explosion,
            EXPLOSION_MEDIA_FRAME_COUNT,
            expected_explosion_delays.as_slice(),
        ),
    ] {
        assert!(bytes.starts_with(b"GIF89a"));
        assert!(bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
        let mut decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(&bytes))
            .expect("decode gif header");
        assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
        assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
        let mut frames = 0;
        let mut delays = Vec::with_capacity(expected_frames);
        while decoder
            .read_next_frame()
            .expect("decode gif frame")
            .is_some_and(|frame| {
                delays.push(u64::from(frame.delay) * 10);
                frames += 1;
                true
            })
        {}
        let encoded_delays = expected_delays
            .iter()
            .map(|milliseconds| milliseconds / 10 * 10)
            .collect::<Vec<_>>();
        assert_eq!(delays, encoded_delays);
        assert_eq!(delays.first().copied(), Some(expected_delays[0]));
        assert_eq!(delays.last().copied(), Some(60_000));
        assert_eq!(frames, expected_frames);
    }
    assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
    let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
    assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
}

#[test]
fn wheel_final_pointer_is_centered_on_the_settled_wedge() {
    for wedge_count in [2, 6, 12, 24, 32] {
        let slice = std::f64::consts::TAU / wedge_count as f64;
        for target_index in 0..wedge_count {
            let rotation =
                wheel_frame_rotation(WHEEL_MEDIA_FRAME_COUNT - 1, target_index, wedge_count);
            assert_eq!(
                wheel_index_at_pointer(rotation, wedge_count),
                Some(target_index),
                "pointer must resolve the same wedge as settlement"
            );
            let target_label_angle = (target_index as f64 * slice - rotation + slice / 2.0)
                .rem_euclid(std::f64::consts::TAU);
            assert!(
                target_label_angle.min(std::f64::consts::TAU - target_label_angle) < 1e-9,
                "settled wedge label must be centered under the pointer"
            );
        }
    }
}

#[test]
fn wheel_media_labels_cover_python_themes_and_fractional_phases() {
    let policies = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let cases = [
        (get_wheel_wedges(false, false, policies), false, 0.0_f64),
        (get_wheel_wedges(true, false, policies), false, 47.25_f64),
        (get_wheel_wedges(false, true, policies), true, 271.875_f64),
        (get_wheel_wedges(false, false, policies), false, 13.37_f64),
        (get_wheel_wedges(true, false, policies), false, 222.615_f64),
        (get_wheel_wedges(true, false, policies), false, 358.905_f64),
    ];
    for (wedges, golden, rotation) in cases {
        let colors = wedges
            .iter()
            .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
            .collect::<Vec<_>>();
        let mut palette = vec![
            [30, 30, 35],
            [255, 215, 0],
            [231, 76, 60],
            if golden { [58, 40, 0] } else { [44, 62, 80] },
            [0, 0, 0],
            [255, 255, 255],
            [55, 190, 55],
            [55, 90, 55],
            [255, 255, 0],
            [160, 160, 160],
            [220, 220, 220],
            [150, 127, 0],
            [30, 110, 30],
        ];
        let indices = colors
            .iter()
            .map(|color| {
                palette
                    .iter()
                    .position(|candidate| candidate == color)
                    .unwrap_or_else(|| {
                        palette.push(*color);
                        palette.len() - 1
                    }) as u8
            })
            .collect::<Vec<_>>();
        let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
        let mut sprite_cache = WheelLabelSpriteCache::default();
        let label_atlas = build_wheel_label_atlas(&wedges, &mut sprite_cache);
        draw_wheel_frame(
            &mut pixels,
            &indices,
            &indices,
            &label_atlas,
            0,
            ((rotation / 11.75).round() as usize).min(WHEEL_MEDIA_FRAME_COUNT - 1),
            golden,
            None,
        );
        assert!(pixels.contains(&4));
        assert!(pixels.contains(&5));
        assert!(
            pixels
                .iter()
                .all(|pixel| usize::from(*pixel) < palette.len())
        );
    }
}

fn assert_cached_label_visual_case(wedges: &[WheelWedge], golden: bool, frame_index: usize) {
    let colors = wedges
        .iter()
        .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
        .collect::<Vec<_>>();
    let palette = [
        [30, 30, 35],
        [255, 215, 0],
        [231, 76, 60],
        if golden { [58, 40, 0] } else { [44, 62, 80] },
        [0, 0, 0],
        [255, 255, 255],
        [55, 190, 55],
        [55, 90, 55],
        [255, 255, 0],
        [160, 160, 160],
        [220, 220, 220],
        [150, 127, 0],
        [30, 110, 30],
    ];
    let indices = colors
        .iter()
        .map(|color| {
            palette
                .iter()
                .position(|candidate| candidate == color)
                .unwrap_or(0) as u8
        })
        .collect::<Vec<_>>();
    let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
    let mut sprite_cache = WheelLabelSpriteCache::default();
    let atlas = build_wheel_label_atlas(wedges, &mut sprite_cache);
    draw_wheel_frame(
        &mut pixels,
        &indices,
        &indices,
        &atlas,
        0,
        frame_index.min(WHEEL_MEDIA_FRAME_COUNT - 1),
        golden,
        None,
    );
    assert!(pixels.contains(&4));
    assert!(pixels.contains(&5));
    assert!(pixels.iter().all(|pixel| usize::from(*pixel) < 256));
}

#[test]
fn cached_wedge_label_sprites_match_normal_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 0);
}

#[test]
fn cached_wedge_label_sprites_match_bankrupt_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 4);
}

#[test]
fn cached_wedge_label_sprites_match_golden_visual_bound() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, true, policy), true, 23);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_a() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 1);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_b() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 19);
}

#[test]
fn cached_wedge_label_sprites_match_fractional_phase_c() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 30);
}

#[test]
fn wheel_label_layout_is_measured_once_and_reused_across_frames() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let mut cache = WheelLabelSpriteCache::default();
    let first = build_wheel_label_atlas(&wedges, &mut cache);
    let misses_after_first = cache.misses;
    let hits_after_first = cache.hits;
    let second = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(first.wedge_sprite_indices, second.wedge_sprite_indices);
    assert_eq!(cache.misses, misses_after_first);
    assert_eq!(cache.hits - hits_after_first, first.sprites.len());
    assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
}

#[test]
fn wheel_label_sprite_cache_is_custom_safe_and_bounded() {
    let wedges = (0..(MAX_WHEEL_LABEL_SPRITES + 24))
        .map(|index| WheelWedge {
            label: format!("CUSTOM_{index}"),
            value: WheelValue::Numeric(0),
            color: "#123456",
        })
        .collect::<Vec<_>>();
    let mut cache = WheelLabelSpriteCache::default();
    let atlas = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(atlas.wedge_sprite_indices.len(), wedges.len());
    assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
    assert_eq!(cache.misses, wedges.len());
    assert!(atlas.sprites.len() > MAX_WHEEL_LABEL_SPRITES);
}

#[test]
fn completed_wheel_attachment_cache_is_lru_bounded() {
    let wedges = vec![WheelWedge {
        label: "CACHE".to_owned(),
        value: WheelValue::Numeric(5),
        color: "#123456",
    }];
    let keys = (0..=MAX_CACHED_WHEEL_ATTACHMENTS)
        .map(|target_index| WheelAttachmentCacheKey::new(&wedges, target_index, false, None))
        .collect::<Vec<_>>();
    let mut cache = WheelAttachmentCache::default();
    for (index, key) in keys.iter().cloned().enumerate() {
        cache.insert(key, vec![index as u8]);
    }
    assert_eq!(cache.entries.len(), MAX_CACHED_WHEEL_ATTACHMENTS);
    assert!(!cache.entries.contains_key(&keys[0]));
    assert_eq!(
        cache.get(keys.last().expect("latest cache key")),
        Some(vec![MAX_CACHED_WHEEL_ATTACHMENTS as u8])
    );
}

#[test]
fn wheel_label_sprite_cache_distinguishes_subpixel_geometry_keys() {
    let wedges = vec![WheelWedge {
        label: "PHASE".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#123456",
    }];
    let mut cache = WheelLabelSpriteCache::default();
    let _ = cache.get_or_insert("PHASE", 1);
    let _ = cache.get_or_insert("PHASE", 2);
    let _ = cache.get_or_insert("PHASE", 1);
    assert_eq!(cache.misses, 2);
    assert_eq!(cache.hits, 1);
    let atlas = build_wheel_label_atlas(&wedges, &mut cache);
    assert_eq!(atlas.sprites.len(), 1);
    let expected_scale = label_scale("PHASE", std::f64::consts::TAU, 220);
    let expected = cache
        .entries
        .get(&("PHASE".to_owned(), expected_scale))
        .expect("atlas geometry is cached under its measured scale");
    assert_eq!(
        atlas.sprites[0].width, expected.width,
        "atlas reuses the font-backed sprite for the measured geometry"
    );
}

#[test]
fn wheel_production_media_is_500px_and_under_discord_limit() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let wedges = get_wheel_wedges(false, false, policy);
    let bytes = render_wheel_gif(&wedges, 7, false, None)
        .expect("production wheel")
        .bytes;
    assert!(bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
    let decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(bytes))
        .expect("wheel header");
    assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
    assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
}

#[test]
fn explosion_production_media_uses_shared_palette_budget_and_upload_bound() {
    let bytes = render_explosion_gif().expect("production explosion");
    assert!(bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
    assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
    let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
    assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
}

#[test]
fn wheel_gif_reuses_palette_seed_without_changing_timing() {
    let policy = WheelEconomyPolicy {
        minigame_scale: 1.0,
        regular_target_ev: 10.0,
        bankrupt_target_ev: 10.0,
        golden_target_ev: 10.0,
    };
    let bytes = render_wheel_gif(&get_wheel_wedges(false, false, policy), 0, false, None)
        .expect("seeded wheel GIF")
        .bytes;
    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(bytes))
        .expect("wheel GIF header");
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("wheel frame") {
        delays.push(u64::from(frame.delay) * 10);
    }
    assert_eq!(delays.len(), WHEEL_MEDIA_FRAME_COUNT);
    assert_eq!(delays[..14], [30_u64; 14]);
    assert_eq!(delays.last().copied(), Some(60_000));
}

#[test]
fn explosion_gif_builds_one_shared_palette_without_changing_timing() {
    let bytes = render_explosion_gif().expect("shared-palette explosion GIF");
    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(bytes))
        .expect("explosion GIF header");
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("explosion frame") {
        assert!(frame.buffer.iter().all(|index| *index < 6));
        delays.push(u64::from(frame.delay) * 10);
    }
    assert_eq!(delays.len(), EXPLOSION_MEDIA_FRAME_COUNT);
    assert_eq!(delays[0..14], [50_u64; 14]);
    assert_eq!(delays.last().copied(), Some(60_000));
}

#[test]
fn media_delay_contract_matches_python_frame_tables() {
    assert_eq!(
        (0..WHEEL_MEDIA_FRAME_COUNT)
            .map(wheel_frame_delay_ms)
            .collect::<Vec<_>>(),
        [
            [30; 14].as_slice(),
            [45; 14].as_slice(),
            [70; 14].as_slice(),
            [110; 16].as_slice(),
            [180].as_slice(),
            &[195, 210, 225, 240, 255, 270, 285, 300, 315],
            &[60_000, 60_000],
        ]
        .concat()
    );
    assert_eq!(
        (0..EXPLOSION_MEDIA_FRAME_COUNT)
            .map(explosion_frame_delay_ms)
            .collect::<Vec<_>>(),
        [
            [50; 14].as_slice(),
            &[60, 70, 80, 90, 100, 110, 120, 130, 140, 150],
            [60; 4].as_slice(),
            [80; 14].as_slice(),
            [100; 13].as_slice(),
            &[60_000],
        ]
        .concat()
    );
    assert_eq!(gif_delay_centiseconds(60_000), 6_000);
}
