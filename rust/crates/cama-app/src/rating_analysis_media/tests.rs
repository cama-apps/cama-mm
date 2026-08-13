use std::collections::BTreeMap;

use crate::drawing::draw_rating_distribution;
use crate::rating_analysis_command::{CalibrationCurveData, CalibrationPoint};
use crate::rating_comparison_service::{
    CalibrationBucket, RatingComparisonMatchData, RatingComparisonResult, RatingSystemStats,
};

use super::*;

fn stats(name: &str, brier: f64, accuracy: f64, log_loss: f64) -> RatingSystemStats {
    RatingSystemStats {
        name: name.to_owned(),
        total_predictions: 25,
        brier_score: brier,
        accuracy,
        calibration_buckets: BTreeMap::<&'static str, CalibrationBucket>::new(),
        log_loss,
    }
}

fn comparison() -> RatingComparisonResult {
    RatingComparisonResult {
        glicko: stats("Glicko-2", 0.21, 0.64, 0.61),
        openskill: stats("OpenSkill", 0.18, 0.72, 0.56),
        matches_analyzed: 25,
        match_data: (0..25)
            .map(|index| RatingComparisonMatchData {
                match_id: index,
                match_date: index,
                radiant_won: index % 2 == 0,
                glicko_radiant_prob: 0.55,
                openskill_radiant_prob: 0.62,
                raw_openskill_radiant_prob: 0.65,
                glicko_correct: index % 3 != 0,
                openskill_correct: index % 4 != 0,
            })
            .collect(),
    }
}

fn calibration_curves() -> CalibrationCurveData {
    CalibrationCurveData {
        glicko: vec![
            CalibrationPoint {
                predicted: 0.2,
                actual_rate: 0.3,
                count: 8,
            },
            CalibrationPoint {
                predicted: 0.7,
                actual_rate: 0.6,
                count: 12,
            },
        ],
        openskill: vec![
            CalibrationPoint {
                predicted: 0.25,
                actual_rate: 0.2,
                count: 10,
            },
            CalibrationPoint {
                predicted: 0.8,
                actual_rate: 0.85,
                count: 15,
            },
        ],
        perfect_line: vec![(0.0, 0.0), (1.0, 1.0)],
    }
}

#[test]
fn test_import_drawing_does_not_load_scientific_stack() {
    let drawing_source = include_str!("../drawing.rs");
    let media_source = include_str!("../rating_analysis_media.rs");
    for forbidden in [
        "std::process::Command",
        "pyo3::",
        "Python::with_gil",
        "numpy::",
    ] {
        assert!(
            !drawing_source.contains(forbidden) && !media_source.contains(forbidden),
            "native drawing code unexpectedly depends on {forbidden}",
        );
    }
}

#[test]
fn test_lazy_rating_plotters_render_pngs() {
    let distribution =
        draw_rating_distribution(&[1_200.0, 1_350.0, 1_475.0, 1_600.0, 1_725.0, 1_900.0]);
    let charts = [
        distribution.into_inner(),
        draw_calibration_curve(&calibration_curves()),
        draw_rating_comparison_chart(&comparison()),
    ];

    for chart in charts {
        let decoded = decode(&chart);
        assert!(decoded.width > 0);
        assert!(decoded.height > 0);
    }
}

#[test]
fn comparison_png_decodes_to_three_semantic_metric_panels() {
    let decoded = decode(&draw_rating_comparison_chart(&comparison()));
    assert_eq!(
        (decoded.width, decoded.height),
        (COMPARISON_WIDTH, COMPARISON_HEIGHT)
    );
    assert!(decoded.count(DISCORD_ACCENT) > 5_000);
    assert!(decoded.count(DISCORD_GREEN) > 5_000);
    assert!(decoded.count(DISCORD_YELLOW) > 100);
}

#[test]
fn calibration_png_contains_reference_and_both_observed_curves() {
    let curves = calibration_curves();
    let decoded = decode(&draw_calibration_curve(&curves));
    assert_eq!(
        (decoded.width, decoded.height),
        (CALIBRATION_WIDTH, CALIBRATION_HEIGHT)
    );
    assert!(decoded.count(DISCORD_ACCENT) > 100);
    assert!(decoded.count(DISCORD_GREEN) > 100);
    assert!(decoded.count(DISCORD_GREY) > 100);
}

#[test]
fn trend_png_decodes_with_two_distinct_rolling_series() {
    let decoded = decode(&draw_prediction_over_time(&comparison(), 20).unwrap());
    assert_eq!((decoded.width, decoded.height), (TREND_WIDTH, TREND_HEIGHT));
    assert!(decoded.count(DISCORD_ACCENT) > 100);
    assert!(decoded.count(DISCORD_GREEN) > 100);
    assert!(draw_prediction_over_time(&comparison(), 26).is_err());
}

struct Decoded {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Decoded {
    fn count(&self, color: Rgba) -> usize {
        self.rgba
            .chunks_exact(4)
            .filter(|pixel| *pixel == color.0)
            .count()
    }
}

fn decode(bytes: &[u8]) -> Decoded {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut cursor = 8;
    let mut width = 0;
    let mut height = 0;
    let mut idat = Vec::new();
    while cursor < bytes.len() {
        let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let kind = &bytes[cursor + 4..cursor + 8];
        let payload = &bytes[cursor + 8..cursor + 8 + length];
        if kind == b"IHDR" {
            width = u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize;
            height = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
        } else if kind == b"IDAT" {
            idat.extend_from_slice(payload);
        }
        cursor += length + 12;
    }
    let mut compressed = 2;
    let mut filtered = Vec::new();
    loop {
        let final_block = idat[compressed] & 1 == 1;
        assert_eq!(idat[compressed] & 0b110, 0);
        compressed += 1;
        let length =
            u16::from_le_bytes(idat[compressed..compressed + 2].try_into().unwrap()) as usize;
        compressed += 4;
        filtered.extend_from_slice(&idat[compressed..compressed + length]);
        compressed += length;
        if final_block {
            break;
        }
    }
    let stride = width * 4 + 1;
    assert_eq!(filtered.len(), stride * height);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in filtered.chunks_exact(stride) {
        assert_eq!(row[0], 0);
        rgba.extend_from_slice(&row[1..]);
    }
    Decoded {
        width,
        height,
        rgba,
    }
}
