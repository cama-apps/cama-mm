use std::collections::BTreeSet;
use std::io::Cursor;

use cama_app::drawing::{BalancePoint, draw_balance_chart, draw_prediction_market_chart};
use cama_app::post_match_gif_media::render_post_match_gif;
use gif::{ColorOutput, DecodeOptions};
use serde::Deserialize;

const FIXTURE_JSON: &str = include_str!("../../../../scripts/visual_equivalence_fixture.json");
const FOREGROUND_THRESHOLD: u8 = 80;

#[derive(Debug, Deserialize)]
struct Fixture {
    chart: ChartFixture,
    animation: AnimationFixture,
    balance: BalanceFixture,
}

#[derive(Debug, Deserialize)]
struct BalanceFixture {
    username: String,
    series: Vec<(i32, i64, String)>,
    source_totals: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize)]
struct ChartFixture {
    market_id: i64,
    title: Option<String>,
    snapshots: Vec<(i64, i64)>,
    created_at: i64,
    now: i64,
}

#[derive(Debug, Deserialize)]
struct AnimationFixture {
    name: String,
    value: i64,
    theme: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE_JSON).expect("visual fixture is typed Rust input")
}

fn pixel_metrics(left: &[u8], right: &[u8]) -> (f64, f64, f64) {
    assert_eq!(left.len(), right.len());
    let mut absolute = 0_u64;
    let mut squared = 0_u64;
    let mut equal = 0_u64;
    for (&first, &second) in left.iter().zip(right) {
        let difference = first.abs_diff(second);
        absolute += u64::from(difference);
        squared += u64::from(difference) * u64::from(difference);
        equal += u64::from(first == second);
    }
    let length = left.len() as f64;
    (
        absolute as f64 / (255.0 * length),
        (squared as f64 / length).sqrt() / 255.0,
        equal as f64 / length,
    )
}

fn foreground_signature(
    rgba: &[u8],
    width: usize,
    height: usize,
) -> (usize, BTreeSet<(usize, usize)>) {
    assert_eq!(rgba.len(), width * height * 4);
    let mut interior = 0;
    let mut occupied = BTreeSet::new();
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        if pixel[..3].iter().copied().max().unwrap_or(0) <= FOREGROUND_THRESHOLD {
            continue;
        }
        let x = index % width;
        let y = index / width;
        occupied.insert((x * 10 / width, y * 10 / height));
        if (24..width - 24).contains(&x) && (24..height - 24).contains(&y) {
            interior += 1;
        }
    }
    (interior, occupied)
}

fn foreground_gate(reference: &[u8], candidate: &[u8], width: usize, height: usize) -> bool {
    let (reference_count, reference_cells) = foreground_signature(reference, width, height);
    let (candidate_count, candidate_cells) = foreground_signature(candidate, width, height);
    if reference_count == 0
        || candidate_count < 200.max(reference_count / 2)
        || reference_cells.is_empty()
    {
        return false;
    }
    let intersection = reference_cells.intersection(&candidate_cells).count();
    let union = reference_cells.union(&candidate_cells).count();
    union > 0 && intersection as f64 / union as f64 >= 0.65
}

#[test]
fn visual_fixture_has_typed_inputs() {
    let fixture = fixture();
    assert_eq!(fixture.chart.market_id, 42);
    assert_eq!(fixture.chart.snapshots.len(), 4);
    assert!(fixture.chart.created_at < fixture.chart.now);
    assert_eq!(fixture.animation.name, "Client 47");
    assert_eq!(fixture.animation.value, 1_337);
    assert_eq!(fixture.animation.theme, "odds_anomaly");
    assert_eq!(fixture.balance.username, "Visual Balance");
    assert_eq!(fixture.balance.series.len(), 7);
    assert_eq!(fixture.balance.source_totals.len(), 7);
}

#[test]
fn native_fixture_render_is_deterministic_and_seekable() {
    let fixture = fixture();
    let chart = &fixture.chart;
    let first_chart = draw_prediction_market_chart(
        chart.market_id,
        chart.title.as_deref(),
        &chart.snapshots,
        chart.created_at,
        chart.now,
    )
    .into_inner();
    let second_chart = draw_prediction_market_chart(
        chart.market_id,
        chart.title.as_deref(),
        &chart.snapshots,
        chart.created_at,
        chart.now,
    )
    .into_inner();
    assert_eq!(first_chart, second_chart);

    let balance = &fixture.balance;
    let series = balance
        .series
        .iter()
        .map(|(event_number, cumulative, source)| BalancePoint {
            event_number: *event_number,
            cumulative: *cumulative,
            source: source.clone(),
        })
        .collect::<Vec<_>>();
    let first_balance =
        draw_balance_chart(&balance.username, &series, &balance.source_totals).into_inner();
    let second_balance =
        draw_balance_chart(&balance.username, &series, &balance.source_totals).into_inner();
    assert_eq!(first_balance, second_balance);
    assert_eq!(&first_balance[..8], b"\x89PNG\r\n\x1a\n");

    let animation = &fixture.animation;
    let first = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render first fixture animation");
    let second = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render second fixture animation");
    assert_eq!(first.bytes, second.bytes);

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(first.bytes))
        .expect("decode fixture animation");
    let mut durations = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("read fixture frame") {
        durations.push(u32::from(frame.delay) * 10);
    }
    assert_eq!(durations, [vec![80; 17], vec![60_000]].concat());
}

#[test]
fn pixel_metrics_are_normalized_and_exact_for_identical_rgba() {
    assert_eq!(
        pixel_metrics(&[0, 10, 20, 255], &[0, 10, 20, 255]),
        (0.0, 0.0, 1.0)
    );
    assert_eq!(
        pixel_metrics(&[0, 0, 0, 255], &[255, 0, 0, 255]),
        (0.25, 0.5, 0.75)
    );
}

#[test]
fn foreground_gate_rejects_contentless_animation() {
    let animation = fixture().animation;
    let asset = render_post_match_gif(&animation.name, animation.value, &animation.theme)
        .expect("render fixture animation");
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(asset.bytes))
        .expect("decode fixture animation");
    let frame = decoder
        .read_next_frame()
        .expect("read fixture frame")
        .expect("fixture has a first frame");
    let reference = frame.buffer.to_vec();
    let blank = [5, 5, 8, 255].repeat(400 * 300);

    assert!(foreground_gate(&reference, &reference, 400, 300));
    assert!(!foreground_gate(&reference, &blank, 400, 300));
}
