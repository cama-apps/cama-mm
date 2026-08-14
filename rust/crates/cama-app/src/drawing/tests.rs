use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek, SeekFrom};

use super::*;

#[derive(Debug)]
struct DecodedPng {
    width: usize,
    height: usize,
    color_type: u8,
    pixels: Vec<u8>,
}

impl DecodedPng {
    fn contains(&self, color: Rgba) -> bool {
        self.pixels.chunks_exact(4).any(|pixel| pixel == color.0)
    }

    fn contains_in(&self, bounds: (usize, usize, usize, usize), color: Rgba) -> bool {
        let (left, top, right, bottom) = bounds;
        (top..bottom.min(self.height)).any(|y| {
            (left..right.min(self.width)).any(|x| {
                let offset = (y * self.width + x) * 4;
                self.pixels[offset..offset + 4] == color.0
            })
        })
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn decode_png(cursor: &Cursor<Vec<u8>>) -> DecodedPng {
    let bytes = cursor.get_ref();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut offset = 8;
    let mut width = 0;
    let mut height = 0;
    let mut color_type = 0;
    let mut compressed = Vec::new();
    while offset < bytes.len() {
        let length = read_u32(bytes, offset) as usize;
        offset += 4;
        let kind: [u8; 4] = bytes[offset..offset + 4].try_into().expect("chunk kind");
        offset += 4;
        let payload = &bytes[offset..offset + length];
        offset += length;
        let expected_crc = read_u32(bytes, offset);
        offset += 4;
        let mut checksum = kind.to_vec();
        checksum.extend_from_slice(payload);
        assert_eq!(crc32(&checksum), expected_crc, "valid chunk CRC");
        match &kind {
            b"IHDR" => {
                width = read_u32(payload, 0) as usize;
                height = read_u32(payload, 4) as usize;
                assert_eq!(payload[8], 8, "eight-bit channels");
                color_type = payload[9];
            }
            b"IDAT" => compressed.extend_from_slice(payload),
            b"IEND" => break,
            _ => panic!("unexpected PNG chunk"),
        }
    }
    assert_eq!(&compressed[..2], &[0x78, 0x01], "zlib stored stream");
    let expected_adler = read_u32(&compressed, compressed.len() - 4);
    let mut filtered = Vec::new();
    let mut cursor = 2;
    loop {
        let flags = compressed[cursor];
        cursor += 1;
        assert_eq!(flags & 0b110, 0, "stored DEFLATE block");
        let final_block = flags & 1 == 1;
        let length = usize::from(u16::from_le_bytes(
            compressed[cursor..cursor + 2].try_into().expect("length"),
        ));
        cursor += 2;
        let inverse = u16::from_le_bytes(
            compressed[cursor..cursor + 2]
                .try_into()
                .expect("inverse length"),
        );
        cursor += 2;
        assert_eq!(
            u16::try_from(length).expect("block length") ^ inverse,
            u16::MAX
        );
        filtered.extend_from_slice(&compressed[cursor..cursor + length]);
        cursor += length;
        if final_block {
            break;
        }
    }
    assert_eq!(adler32(&filtered), expected_adler, "valid zlib Adler-32");
    let row_bytes = width * 4;
    assert_eq!(filtered.len(), (row_bytes + 1) * height);
    let mut pixels = Vec::with_capacity(row_bytes * height);
    for row in filtered.chunks_exact(row_bytes + 1) {
        assert_eq!(row[0], 0, "unfiltered scanline");
        pixels.extend_from_slice(&row[1..]);
    }
    DecodedPng {
        width,
        height,
        color_type,
        pixels,
    }
}

#[test]
fn prediction_market_chart_is_real_fixed_size_png_for_empty_and_multi_point_history() {
    let empty = draw_prediction_market_chart(7, None, &[], 1_000, 2_000);
    let decoded = decode_png(&empty);
    assert_eq!((decoded.width, decoded.height), (700, 280));
    assert!(decoded.contains(DISCORD_GREY));

    let history = draw_prediction_market_chart(
        7,
        Some("Will the market move?"),
        &[(1_000, 50), (1_500, 58), (2_000, 46)],
        1_000,
        2_000,
    );
    let decoded = decode_png(&history);
    assert_eq!((decoded.width, decoded.height), (700, 280));
    assert!(decoded.contains(DISCORD_ACCENT));
    assert!(decoded.contains(DISCORD_WHITE));
}

#[test]
fn prediction_chart_wraps_titles_and_formats_utc_ticks_like_python() {
    assert_eq!(
        wrap_prediction_title("one two three four", 54, 1),
        ["one two", "three", "four"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        wrap_prediction_title("supercalifragilistic", 12, 1),
        vec!["supercalifragilistic"]
    );
    assert_eq!(round_div_4_bankers(6), 2, "1.5 rounds up");
    assert_eq!(round_div_4_bankers(18), 4, "4.5 rounds to even");
    assert_eq!(
        prediction_x_for(-100, 0, 100, 50, 620),
        -570,
        "Python's int projection keeps out-of-range points out of the chart"
    );
    assert_eq!(prediction_x_for(150, 0, 100, 50, 620), 980);
    assert_eq!(prediction_utc_label(0, 23 * 60 * 60), "00:00");
    assert_eq!(prediction_utc_label(0, 24 * 60 * 60), "01-01");
}

#[test]
fn prediction_chart_uses_python_empty_axis_and_marker_branches() {
    let empty = decode_png(&draw_prediction_market_chart(1, None, &[], 0, 1));
    assert!(empty.contains_in((50, 40, 51, 41), DISCORD_GREY));
    assert!(
        !empty.contains_in((50, 40, 671, 241), DISCORD_ACCENT),
        "an empty history has no series pixels"
    );

    let one = decode_png(&draw_prediction_market_chart(
        1,
        Some("one line"),
        &[(0, 50)],
        0,
        1,
    ));
    assert!(one.contains_in((50, 137, 55, 145), DISCORD_WHITE));
    assert!(one.contains_in((50, 140, 671, 141), DISCORD_ACCENT));

    let many = decode_png(&draw_prediction_market_chart(
        1,
        Some("one line"),
        &[(0, 50), (3_600, 51)],
        0,
        3_600,
    ));
    assert!(many.contains_in((50, 40, 51, 41), DISCORD_GREY));
    assert!(many.contains(DISCORD_ACCENT));
    assert!(many.contains(DISCORD_WHITE));
}

#[test]
fn prediction_chart_native_output_is_deterministic_for_fixed_clock_input() {
    let input = [
        (1_700_000_000, 17),
        (1_700_003_600, 19),
        (1_700_007_200, 22),
        (1_700_010_800, 18),
    ];
    let first = draw_prediction_market_chart(
        10,
        Some("A deterministic question"),
        &input,
        1_700_000_000,
        1_700_014_400,
    )
    .into_inner();
    let second = draw_prediction_market_chart(
        10,
        Some("A deterministic question"),
        &input,
        1_700_000_000,
        1_700_014_400,
    )
    .into_inner();
    assert_eq!(first, second);
}

fn assert_rgba_png(cursor: &Cursor<Vec<u8>>, size: Option<(usize, usize)>) -> DecodedPng {
    let decoded = decode_png(cursor);
    assert_eq!(decoded.color_type, 6, "PNG color type must be RGBA");
    assert!(decoded.width > 0 && decoded.height > 0);
    if let Some(expected) = size {
        assert_eq!((decoded.width, decoded.height), expected);
    }
    decoded
}

fn match_row(name: &str, won: bool, duration_seconds: Option<u64>) -> MatchRow {
    MatchRow {
        hero_name: Some(name.to_owned()),
        kills: 5,
        deaths: 2,
        assists: 9,
        won: Some(won),
        duration_seconds,
        ..MatchRow::default()
    }
}

fn history(
    rating: Option<f64>,
    won: Option<bool>,
    openskill_mu: Option<f64>,
) -> RatingHistoryEntry {
    RatingHistoryEntry {
        rating,
        openskill_mu,
        won,
    }
}

fn info(source: &str, leverage: i64, profit: i64) -> GambaInfo {
    GambaInfo {
        source: source.to_owned(),
        outcome: Some(if profit >= 0 { "won" } else { "lost" }.to_owned()),
        leverage,
        profit,
    }
}

#[test]
fn test_empty_matches_returns_image() {
    let image = draw_matches_table(&[], &BTreeMap::new());
    assert_rgba_png(&image, Some((400, 100)));
}

#[test]
fn test_single_match() {
    let image = draw_matches_table(
        &[match_row("Anti-Mage", true, Some(2_400))],
        &BTreeMap::new(),
    );
    let decoded = assert_rgba_png(&image, None);
    assert!(decoded.width >= 300 && decoded.height >= 50);
    assert!(decoded.contains(DISCORD_GREEN));
}

#[test]
fn test_multiple_matches() {
    let image = draw_matches_table(
        &[
            match_row("Pudge", false, Some(1_800)),
            match_row("Crystal Maiden", true, Some(3_000)),
            match_row("Axe", true, Some(2_100)),
        ],
        &BTreeMap::new(),
    );
    let decoded = assert_rgba_png(&image, None);
    assert!(decoded.height >= 100);
    assert!(decoded.contains(DISCORD_DARKER));
    assert!(decoded.contains(DISCORD_RED));
}

#[test]
fn test_hero_id_with_names_dict() {
    let row = MatchRow {
        hero_id: Some(1),
        kills: 10,
        deaths: 2,
        assists: 5,
        won: Some(true),
        duration_seconds: Some(2_000),
        ..MatchRow::default()
    };
    let names = BTreeMap::from([(1, "Anti-Mage".to_owned()), (2, "Axe".to_owned())]);
    let named = draw_matches_table(std::slice::from_ref(&row), &names);
    let unknown = draw_matches_table(&[row], &BTreeMap::new());
    assert_rgba_png(&named, None);
    assert_ne!(
        named.get_ref(),
        unknown.get_ref(),
        "resolved hero name is rasterized"
    );
}

#[test]
fn test_missing_duration() {
    let image = draw_matches_table(&[match_row("Pudge", false, None)], &BTreeMap::new());
    assert_rgba_png(&image, None);
}

#[test]
fn test_zero_duration_matches_python_missing_duration_fallback() {
    let missing = draw_matches_table(&[match_row("Pudge", false, None)], &BTreeMap::new());
    let zero = draw_matches_table(
        &[MatchRow {
            hero_name: Some("Pudge".to_owned()),
            won: Some(false),
            duration_seconds: Some(0),
            ..match_row("Pudge", false, None)
        }],
        &BTreeMap::new(),
    );
    assert_eq!(missing.get_ref(), zero.get_ref());
}

#[test]
fn test_duration_changes_recent_match_raster() {
    let first = draw_matches_table(
        &[match_row("Outworld Destroyer", true, Some(2_400))],
        &BTreeMap::new(),
    );
    let second = draw_matches_table(
        &[match_row("Outworld Destroyer", true, Some(2_401))],
        &BTreeMap::new(),
    );
    assert_ne!(first.get_ref(), second.get_ref());
}

#[test]
fn test_basic_role_graph() {
    let roles = BTreeMap::from([
        ("Carry".to_owned(), 30.0),
        ("Support".to_owned(), 25.0),
        ("Nuker".to_owned(), 20.0),
        ("Disabler".to_owned(), 15.0),
        ("Initiator".to_owned(), 10.0),
    ]);
    let image = draw_role_graph(&roles, "Role Distribution");
    let decoded = assert_rgba_png(&image, Some((400, 400)));
    assert!(decoded.contains(DISCORD_ACCENT));
}

#[test]
fn test_role_graph_with_title() {
    let roles = BTreeMap::from([("Carry".to_owned(), 50.0), ("Support".to_owned(), 50.0)]);
    let custom = draw_role_graph(&roles, "My Roles");
    let standard = draw_role_graph(&roles, "Role Distribution");
    assert_rgba_png(&custom, Some((400, 400)));
    assert_ne!(
        custom.get_ref(),
        standard.get_ref(),
        "title text changes pixels"
    );
}

#[test]
fn test_role_graph_few_roles() {
    let roles = BTreeMap::from([("Carry".to_owned(), 60.0), ("Support".to_owned(), 40.0)]);
    let image = draw_role_graph(&roles, "Role Distribution");
    assert_rgba_png(&image, Some((400, 400)));
}

#[test]
fn test_role_graph_many_roles() {
    let roles = ROLE_ORDER
        .iter()
        .enumerate()
        .map(|(index, role)| ((*role).to_owned(), 20.0 - index as f64))
        .collect::<BTreeMap<_, _>>();
    let image = draw_role_graph(&roles, "Role Distribution");
    let decoded = assert_rgba_png(&image, Some((400, 400)));
    assert!(decoded.contains(DISCORD_ACCENT));
}

#[test]
fn test_basic_lane_distribution() {
    let lanes = [
        ("Safe Lane".to_owned(), 40.0),
        ("Mid".to_owned(), 25.0),
        ("Off Lane".to_owned(), 30.0),
        ("Jungle".to_owned(), 5.0),
    ];
    let image = draw_lane_distribution(&lanes);
    let decoded = assert_rgba_png(&image, None);
    assert_eq!(decoded.width, 350);
    assert!(decoded.contains(Rgba::rgb(0x4c, 0xaf, 0x50)));
}

#[test]
fn test_lane_distribution_all_zeros() {
    let lanes = [
        ("Safe Lane".to_owned(), 0.0),
        ("Mid".to_owned(), 0.0),
        ("Off Lane".to_owned(), 0.0),
        ("Jungle".to_owned(), 0.0),
    ];
    let image = draw_lane_distribution(&lanes);
    let decoded = assert_rgba_png(&image, None);
    assert!(decoded.contains(DISCORD_DARKER));
}

#[test]
fn test_lane_distribution_single_lane() {
    let image = draw_lane_distribution(&[("Mid".to_owned(), 100.0)]);
    let decoded = assert_rgba_png(&image, Some((350, 100)));
    assert!(decoded.contains(Rgba::rgb(0x21, 0x96, 0xf3)));
}

#[test]
fn test_hero_performance_chart_matches_python_dimensions_and_thresholds() {
    let image = draw_hero_performance_chart(
        &[
            HeroPerformanceEntry {
                hero_name: "Anti-Mage".to_owned(),
                games: 5,
                wins: 4,
            },
            HeroPerformanceEntry {
                hero_name: "Invoker".to_owned(),
                games: 3,
                wins: 1,
            },
        ],
        "TestUser",
    );
    let decoded = assert_rgba_png(&image, Some((450, 141)));
    assert!(decoded.contains(DISCORD_GREEN));
    assert!(decoded.contains(DISCORD_RED));
}

#[test]
fn test_hero_performance_chart_empty_history_matches_python_fallback() {
    let image = draw_hero_performance_chart(&[], "TestUser");
    assert_rgba_png(&image, Some((450, 100)));
}

#[test]
fn test_basic_attribute_distribution() {
    let image = draw_attribute_distribution(AttributeValues {
        strength: 25.0,
        agility: 35.0,
        intelligence: 30.0,
        universal: 10.0,
    });
    let decoded = assert_rgba_png(&image, Some((300, 300)));
    assert!(decoded.contains(Rgba::rgb(0xe5, 0x39, 0x35)));
    assert!(decoded.contains(Rgba::rgb(0x43, 0xa0, 0x47)));
}

#[test]
fn test_attribute_distribution_missing_attrs() {
    let image = draw_attribute_distribution(AttributeValues {
        agility: 60.0,
        intelligence: 40.0,
        ..AttributeValues::default()
    });
    let decoded = assert_rgba_png(&image, Some((300, 300)));
    assert!(!decoded.contains(Rgba::rgb(0xe5, 0x39, 0x35)));
}

#[test]
fn test_attribute_distribution_single_attr() {
    let image = draw_attribute_distribution(AttributeValues {
        strength: 100.0,
        ..AttributeValues::default()
    });
    let decoded = assert_rgba_png(&image, Some((300, 300)));
    assert!(decoded.contains(Rgba::rgb(0xe5, 0x39, 0x35)));
}

#[test]
fn test_all_functions_return_seekable_bytesio() {
    let roles = BTreeMap::from([
        ("Carry".to_owned(), 50.0),
        ("Support".to_owned(), 30.0),
        ("Nuker".to_owned(), 20.0),
    ]);
    let mut images = vec![
        draw_matches_table(&[match_row("Pudge", true, Some(1_000))], &BTreeMap::new()),
        draw_role_graph(&roles, "Role Distribution"),
        draw_lane_distribution(&[("Safe Lane".to_owned(), 50.0), ("Mid".to_owned(), 50.0)]),
        draw_attribute_distribution(AttributeValues {
            strength: 50.0,
            agility: 50.0,
            ..AttributeValues::default()
        }),
    ];
    for image in &mut images {
        assert_eq!(image.position(), 0);
        image.seek(SeekFrom::Start(10)).expect("seek into PNG");
        image.seek(SeekFrom::Start(0)).expect("rewind PNG");
        let mut signature = [0_u8; 8];
        image.read_exact(&mut signature).expect("read signature");
        assert_eq!(&signature, b"\x89PNG\r\n\x1a\n");
    }
}

#[test]
fn test_all_images_are_rgba() {
    let roles = BTreeMap::from([
        ("A".to_owned(), 33.0),
        ("B".to_owned(), 33.0),
        ("C".to_owned(), 34.0),
    ]);
    let images = [
        draw_matches_table(&[match_row("Test", true, Some(100))], &BTreeMap::new()),
        draw_role_graph(&roles, "Role Distribution"),
        draw_lane_distribution(&[("Safe Lane".to_owned(), 100.0)]),
        draw_attribute_distribution(AttributeValues {
            strength: 100.0,
            ..AttributeValues::default()
        }),
    ];
    for image in images {
        assert_eq!(decode_png(&image).color_type, 6);
    }
}

#[test]
fn test_empty_history_returns_image() {
    let image = draw_rating_history_chart("TestUser", &[]);
    assert_rgba_png(&image, Some((700, 400)));
}

#[test]
fn test_basic_chart_with_both_ratings() {
    let entries = [
        history(Some(1_600.0), Some(true), Some(40.0)),
        history(Some(1_550.0), Some(false), Some(38.0)),
        history(Some(1_500.0), Some(true), Some(35.0)),
    ];
    let image = draw_rating_history_chart("TestUser", &entries);
    let decoded = assert_rgba_png(&image, Some((700, 400)));
    assert!(decoded.contains(DISCORD_ACCENT));
    assert!(decoded.contains(DISCORD_YELLOW));
}

#[test]
fn test_chart_without_openskill() {
    let entries = [
        history(Some(1_600.0), Some(true), None),
        history(Some(1_550.0), Some(false), None),
        history(Some(1_500.0), Some(true), None),
    ];
    let decoded = assert_rgba_png(
        &draw_rating_history_chart("TestUser", &entries),
        Some((700, 400)),
    );
    assert!(decoded.contains(DISCORD_ACCENT));
    assert!(!decoded.contains(DISCORD_YELLOW));
}

#[test]
fn test_two_matches_minimum() {
    let entries = [
        history(Some(1_550.0), Some(true), Some(40.0)),
        history(Some(1_500.0), Some(false), Some(35.0)),
    ];
    assert_rgba_png(
        &draw_rating_history_chart("TestUser", &entries),
        Some((700, 400)),
    );
}

#[test]
fn test_large_history() {
    let entries = (1..=60)
        .rev()
        .map(|index| {
            history(
                Some(1_500.0 + f64::from(index * 10)),
                Some(index % 3 != 0),
                Some(30.0 + f64::from(index) * 0.5),
            )
        })
        .collect::<Vec<_>>();
    let decoded = assert_rgba_png(
        &draw_rating_history_chart("TestUser", &entries),
        Some((700, 400)),
    );
    assert!(decoded.contains(DISCORD_GREEN));
    assert!(decoded.contains(DISCORD_RED));
}

#[test]
fn test_partial_openskill_data() {
    let entries = [
        history(Some(1_600.0), Some(true), Some(40.0)),
        history(Some(1_550.0), Some(false), None),
        history(Some(1_500.0), Some(true), Some(35.0)),
    ];
    let decoded = assert_rgba_png(&draw_rating_history_chart("TestUser", &entries), None);
    assert!(
        decoded.contains(DISCORD_YELLOW),
        "legend exposes available OpenSkill data"
    );
}

#[test]
fn test_partial_glicko_data() {
    let entries = [
        history(Some(1_600.0), Some(true), Some(40.0)),
        history(None, Some(false), Some(38.0)),
        history(Some(1_500.0), Some(true), Some(35.0)),
    ];
    assert_rgba_png(
        &draw_rating_history_chart("TestUser", &entries),
        Some((700, 400)),
    );
}

#[test]
fn test_openskill_only_rows_keep_outcome_markers() {
    let entries = [
        history(None, Some(true), Some(40.0)),
        history(None, Some(false), Some(35.0)),
    ];
    let decoded = assert_rgba_png(&draw_rating_history_chart("TestUser", &entries), None);
    assert!(decoded.contains_in((60, 70, 641, 321), DISCORD_GREEN));
    assert!(decoded.contains_in((60, 70, 641, 321), DISCORD_RED));
}

#[test]
fn test_openskill_only_history_hides_glicko_legend() {
    let entries = [
        history(None, Some(true), Some(40.0)),
        history(None, Some(false), Some(35.0)),
    ];
    let decoded = assert_rgba_png(&draw_rating_history_chart("TestUser", &entries), None);
    assert!(!decoded.contains_in((60, 321, 641, 390), DISCORD_ACCENT));
}

#[test]
fn test_flat_ratings() {
    let entries = [
        history(Some(1_500.0), Some(true), Some(35.0)),
        history(Some(1_500.0), Some(false), Some(35.0)),
        history(Some(1_500.0), Some(true), Some(35.0)),
    ];
    assert_rgba_png(
        &draw_rating_history_chart("TestUser", &entries),
        Some((700, 400)),
    );
}

#[test]
fn test_won_none_renders_grey() {
    let entries = [
        history(Some(1_600.0), Some(true), None),
        history(Some(1_550.0), None, None),
        history(Some(1_500.0), Some(false), None),
    ];
    let decoded = assert_rgba_png(&draw_rating_history_chart("TestUser", &entries), None);
    assert!(decoded.contains_in((60, 70, 641, 321), DISCORD_GREY));
}

#[test]
fn test_returns_seekable_bytesio() {
    let entries = [
        history(Some(1_600.0), Some(true), None),
        history(Some(1_500.0), Some(false), None),
    ];
    let mut image = draw_rating_history_chart("TestUser", &entries);
    assert_eq!(image.position(), 0);
    let mut signature = [0_u8; 8];
    image
        .read_exact(&mut signature)
        .expect("read PNG without seeking");
    assert_eq!(&signature, b"\x89PNG\r\n\x1a\n");
}

#[test]
fn test_returns_valid_png() {
    let data = AdvantageData {
        radiant_gold: vec![0.0, 500.0, 1_200.0, -300.0, 2_000.0, 5_000.0, 3_000.0],
        radiant_xp: vec![0.0, 200.0, 800.0, 100.0, 1_500.0, 3_000.0, 2_500.0],
    };
    let image = draw_advantage_graph(&data, Some(42)).expect("advantage attachment");
    let decoded = assert_rgba_png(&image, Some((790, 340)));
    assert!(decoded.contains(DISCORD_YELLOW));
    assert!(decoded.contains(DISCORD_ACCENT));
    assert!(decoded.pixels.chunks_exact(4).any(|pixel| {
        u16::from(pixel[1]) > u16::from(pixel[0]) + 10
            && u16::from(pixel[1]) > u16::from(pixel[2]) + 5
    }));
    assert!(
        decoded
            .pixels
            .chunks_exact(4)
            .any(|pixel| u16::from(pixel[0]) > u16::from(pixel[1]) + 10)
    );
}

#[test]
fn long_mixed_sign_advantage_uses_full_data_span_and_minute_ticks() {
    let gold = (0..91)
        .map(|index| {
            let minute = index as f64;
            if index % 12 < 4 {
                -350.0 - minute * 4.0
            } else {
                minute * 95.0 - 2_600.0
            }
        })
        .collect::<Vec<_>>();
    let xp = (0..73)
        .map(|index| index as f64 * 80.0 - 1_800.0)
        .collect::<Vec<_>>();
    let data = AdvantageData {
        radiant_gold: gold,
        radiant_xp: xp,
    };
    let image = draw_advantage_graph(&data, Some(90)).expect("long advantage attachment");
    let decoded = assert_rgba_png(&image, Some((790, 340)));
    assert!(decoded.contains(DISCORD_YELLOW));
    assert!(decoded.contains(DISCORD_ACCENT));

    let projection = AdvantageProjection {
        left: 56,
        right: 780,
        top: 35,
        bottom: 288,
        minimum: -2_600.0,
        maximum: 6_000.0,
    };
    let ticks = advantage_ticks(projection);
    assert_eq!(ticks.len(), 6);
    assert_eq!(ticks.first().copied(), Some(projection.minimum));
    assert_eq!(ticks.last().copied(), Some(projection.maximum));
    assert!(ticks.iter().any(|value| *value < 0.0));
    assert!(ticks.iter().any(|value| *value > 0.0));
    assert_eq!(advantage_x_tick_values(90), vec![0, 15, 30, 45, 60, 75, 90]);
    assert_eq!(advantage_x_tick_values(4), vec![0, 1, 2, 3, 4]);
}

#[test]
fn test_returns_none_without_advantage_data() {
    assert!(draw_advantage_graph(&AdvantageData::default(), None).is_none());
}

#[test]
fn test_returns_none_with_empty_dict() {
    let empty = AdvantageData {
        radiant_gold: Vec::new(),
        radiant_xp: Vec::new(),
    };
    assert!(draw_advantage_graph(&empty, None).is_none());
}

#[test]
fn test_gold_only() {
    let data = AdvantageData {
        radiant_gold: vec![0.0, 1_000.0, 2_000.0, -500.0, 3_000.0],
        ..AdvantageData::default()
    };
    let decoded = assert_rgba_png(
        &draw_advantage_graph(&data, None).expect("gold chart"),
        Some((790, 340)),
    );
    assert!(decoded.contains(DISCORD_YELLOW));
}

#[test]
fn test_xp_only() {
    let data = AdvantageData {
        radiant_xp: vec![0.0, 300.0, 600.0, 1_200.0, 900.0],
        ..AdvantageData::default()
    };
    let decoded = assert_rgba_png(
        &draw_advantage_graph(&data, None).expect("XP chart"),
        Some((790, 340)),
    );
    assert!(decoded.contains(DISCORD_ACCENT));
}

#[test]
fn test_dense_series_culls_overlapping_markers_but_keeps_key_events() {
    let points = (0..386).map(|index| (index * 2, 100)).collect::<Vec<_>>();
    let infos = (0..386)
        .map(|index| {
            let source = if index % 47 == 0 {
                "double_or_nothing"
            } else if index % 19 == 0 {
                "wheel"
            } else {
                "bet"
            };
            info(
                source,
                if source == "bet" && index % 13 == 0 {
                    3
                } else {
                    1
                },
                index % 31,
            )
        })
        .collect::<Vec<_>>();
    let radii = infos.iter().map(marker_radius).collect::<Vec<_>>();
    let required = [0, 100, 200, 385];
    let selected =
        select_marker_indices(&points, &infos, &radii, &required).expect("equal vectors");
    assert!(required.iter().all(|required| selected.contains(required)));
    assert!(selected.len() < 100);
    assert!(
        selected
            .iter()
            .any(|index| infos[*index].source == "double_or_nothing")
    );
    assert!(selected.iter().any(|index| infos[*index].source == "wheel"));
    assert!(
        selected
            .iter()
            .any(|index| infos[*index].normalized_leverage() > 1)
    );
    for (position, index) in selected.iter().enumerate() {
        for other in &selected[position + 1..] {
            let dx = points[*index].0 - points[*other].0;
            let dy = points[*index].1 - points[*other].1;
            let distance = radii[*index] + radii[*other] + 3;
            assert!(dx * dx + dy * dy >= distance * distance);
        }
    }
}

#[test]
fn test_sparse_series_keeps_every_marker() {
    let points = (0..8).map(|index| (index * 20, 100)).collect::<Vec<_>>();
    let infos = points
        .iter()
        .map(|_| info("bet", 1, 10))
        .collect::<Vec<_>>();
    let radii = infos.iter().map(marker_radius).collect::<Vec<_>>();
    assert_eq!(
        select_marker_indices(&points, &infos, &radii, &[]).expect("equal vectors"),
        (0..points.len()).collect::<Vec<_>>()
    );
}

#[test]
fn test_axis_ticks_are_bounded_and_collision_free() {
    assert_eq!(
        select_x_axis_ticks(&(1..=386).collect::<Vec<_>>(), 5),
        vec![1, 97, 193, 290, 386]
    );
    let projection = make_projection(&[-4_430.0, 3_616.0], 386, (60, 88, 614, 222));
    let ticks = select_y_axis_ticks(projection, -4_430.0, 3_616.0);
    let mut positions = ticks.iter().map(|tick| tick.1).collect::<Vec<_>>();
    positions.push(projection.to_pixel(1, 0.0).1);
    positions.sort_unstable();
    assert!(positions.len() <= 8);
    assert!(positions.windows(2).all(|pair| pair[1] - pair[0] >= 20));
}

#[test]
fn test_dense_mixed_history_returns_fixed_size_png() {
    let mut cumulative = 0_i64;
    let series = (1..=386)
        .map(|event_number| {
            let profit = if event_number % 3 == 0 { -150 } else { 80 };
            cumulative += profit;
            let source = if event_number % 47 == 0 {
                "double_or_nothing"
            } else if event_number % 19 == 0 {
                "wheel"
            } else {
                "bet"
            };
            GambaPoint {
                event_number,
                cumulative,
                info: info(
                    source,
                    if source == "bet" && event_number % 13 == 0 {
                        3
                    } else {
                        1
                    },
                    profit,
                ),
            }
        })
        .collect::<Vec<_>>();
    let image = draw_gamba_chart(
        "DenseUser",
        65,
        "Degenerate",
        &series,
        GambaStats {
            total_bets: 288,
            win_rate: 0.53,
            net_pnl: -5_489,
            roi: -0.188,
        },
    );
    let decoded = assert_rgba_png(&image, Some((700, 400)));
    assert!(decoded.contains(DISCORD_GREEN));
    assert!(decoded.contains(DISCORD_RED));
}

#[test]
fn test_wheel_source_info_missing_keys_does_not_raise() {
    let series = [
        GambaPoint {
            event_number: 1,
            cumulative: 100,
            info: GambaInfo {
                source: "wheel".to_owned(),
                ..GambaInfo::default()
            },
        },
        GambaPoint {
            event_number: 2,
            cumulative: -50,
            info: GambaInfo {
                source: "wheel".to_owned(),
                ..GambaInfo::default()
            },
        },
    ];
    let image = draw_gamba_chart("TestUser", 50, "Degen", &series, GambaStats::default());
    assert_rgba_png(&image, Some((700, 400)));
}

#[test]
fn test_shared_axis_changes_keep_balance_chart_rendering() {
    let series = [
        BalancePoint {
            event_number: 1,
            cumulative: 100,
            source: "bets".to_owned(),
        },
        BalancePoint {
            event_number: 2,
            cumulative: -50,
            source: "wheel".to_owned(),
        },
        BalancePoint {
            event_number: 3,
            cumulative: 500,
            source: "bonus".to_owned(),
        },
    ];
    let totals = BTreeMap::from([
        ("bets".to_owned(), 100),
        ("wheel".to_owned(), -50),
        ("bonus".to_owned(), 450),
    ]);
    let decoded = assert_rgba_png(
        &draw_balance_chart("TestUser", &series, &totals),
        Some((700, 400)),
    );
    assert!(decoded.contains(source_color("bets")));
    assert!(decoded.contains(source_color("wheel")));
    assert!(decoded.contains(DISCORD_GRID));
    assert!(decoded.contains(DISCORD_GREEN));
    assert!(decoded.contains(DISCORD_RED));
    assert!(decoded.contains(DISCORD_WHITE));
}

#[test]
fn test_successful_and_failed_renders_register_no_figure() {
    let ratings = [1_400.0, 1_500.0, 1_520.0, 1_600.0, 1_700.0, 1_450.0];
    let varied = draw_rating_distribution(&ratings);
    let decoded = assert_rgba_png(&varied, Some((640, 390)));
    assert!(
        decoded.contains(DISCORD_GREEN),
        "spread distribution draws normal fit"
    );
    assert!(
        decoded.contains(DISCORD_YELLOW),
        "five-plus samples draw the KDE overlay"
    );
    assert!(decoded.contains(DISCORD_RED), "mean marker is visible");
    assert!(
        decoded.contains(Rgba::rgb(0xf4, 0x7b, 0x67)),
        "median marker is visible"
    );
    let mut ordered_ratings = ratings;
    ordered_ratings.sort_by(f64::total_cmp);
    let normality =
        distribution_normality_p_value(&ordered_ratings).expect("normality probability");
    assert!(
        (normality - 0.858_128_646_411_875_1).abs() < 1e-6,
        "native Royston probability {normality} matches scipy.stats.shapiro"
    );

    let mut state = 1_u32;
    let mut large_sample = Vec::with_capacity(5_001);
    while large_sample.len() < 5_001 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let first = (f64::from(state) + 1.0) / (f64::from(u32::MAX) + 2.0);
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let second = (f64::from(state) + 1.0) / (f64::from(u32::MAX) + 2.0);
        let radius = (-2.0 * first.ln()).sqrt();
        let angle = 2.0 * std::f64::consts::PI * second;
        large_sample.push(radius * angle.cos());
        large_sample.push(radius * angle.sin());
    }
    large_sample.truncate(5_001);
    large_sample.sort_by(f64::total_cmp);
    let large_normality =
        distribution_normality_p_value(&large_sample).expect("large-sample normality probability");
    assert!(
        (large_normality - 0.750_632_579_300_718_9).abs() < 1e-6,
        "native D'Agostino-Pearson probability {large_normality} matches scipy.stats.normaltest"
    );

    let mut outputs = BTreeSet::new();
    for _ in 0..3 {
        let flat = draw_rating_distribution(&[1_500.0; 8]);
        assert_rgba_png(&flat, Some((640, 390)));
        outputs.insert(flat.into_inner());
    }
    assert_eq!(
        outputs.len(),
        1,
        "degenerate rendering is deterministic and stateless"
    );
}

#[test]
fn rating_distribution_font_supports_authored_stats_punctuation() {
    assert_eq!(
        glyph('a'),
        glyph('A'),
        "native bitmap letters are uppercase"
    );
    assert_ne!(glyph('μ'), glyph('?'));
    assert_ne!(glyph('σ'), glyph('?'));
    assert_ne!(glyph(','), glyph('?'));
}

#[test]
fn rating_distribution_explicit_median_controls_the_live_marker() {
    let ratings = [1_400.0, 1_450.0, 1_500.0, 1_520.0, 1_600.0, 1_700.0];
    let median_color = Rgba::rgb(0xf4, 0x7b, 0x67);

    let low = decode_png(&draw_rating_distribution_with_median(
        &ratings,
        Some(1_425.0),
    ));
    assert_eq!((low.width, low.height), (640, 390));
    assert!(
        low.contains_in((135, 130, 145, 330), median_color),
        "the explicit service median should control the plot marker"
    );

    let high = decode_png(&draw_rating_distribution_with_median(
        &ratings,
        Some(1_675.0),
    ));
    assert!(
        high.contains_in((445, 130, 456, 330), median_color),
        "changing the service median should move the plot marker"
    );
    assert_ne!(low.pixels, high.pixels);

    let without_median = decode_png(&draw_rating_distribution_with_median(&ratings, None));
    assert!(
        !without_median.contains(median_color),
        "None must suppress both the median marker and its legend entry"
    );
}
