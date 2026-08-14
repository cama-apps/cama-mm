//! Emit deterministic artifacts for the cross-language visual-equivalence gate.
//!
//! This is deliberately an example rather than a production path.  The
//! companion `scripts/visual_equivalence.py` owns the Python invocation and
//! image decoding; this binary only exercises the same public Rust renderers
//! that the runtime calls.

use std::env;
use std::fs;
use std::path::Path;

use cama_app::drawing::{BalancePoint, draw_balance_chart, draw_prediction_market_chart};
use cama_app::post_match_gif_media::render_post_match_gif;
use serde::Deserialize;

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
    snapshots: Vec<[i64; 2]>,
    created_at: i64,
    now: i64,
}

#[derive(Debug, Deserialize)]
struct AnimationFixture {
    name: String,
    value: i64,
    theme: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let fixture_path = arguments.next().ok_or("missing fixture path")?;
    let output_dir = arguments.next().ok_or("missing output directory")?;
    if arguments.next().is_some() {
        return Err("usage: visual_equivalence <fixture.json> <output-dir>".into());
    }

    let fixture: Fixture = serde_json::from_slice(&fs::read(&fixture_path)?)?;
    fs::create_dir_all(Path::new(&output_dir))?;

    let snapshots = fixture
        .chart
        .snapshots
        .iter()
        .map(|snapshot| (snapshot[0], snapshot[1]))
        .collect::<Vec<_>>();
    let chart = draw_prediction_market_chart(
        fixture.chart.market_id,
        fixture.chart.title.as_deref(),
        &snapshots,
        fixture.chart.created_at,
        fixture.chart.now,
    )
    .into_inner();
    fs::write(Path::new(&output_dir).join("rust_chart.png"), chart)?;

    let balance = fixture
        .balance
        .series
        .iter()
        .map(|(event_number, cumulative, source)| BalancePoint {
            event_number: *event_number,
            cumulative: *cumulative,
            source: source.clone(),
        })
        .collect::<Vec<_>>();
    let balance_chart = draw_balance_chart(
        &fixture.balance.username,
        &balance,
        &fixture.balance.source_totals,
    )
    .into_inner();
    fs::write(
        Path::new(&output_dir).join("rust_balance.png"),
        balance_chart,
    )?;

    let animation = render_post_match_gif(
        &fixture.animation.name,
        fixture.animation.value,
        &fixture.animation.theme,
    )?;
    fs::write(
        Path::new(&output_dir).join("rust_animation.gif"),
        animation.bytes,
    )?;
    Ok(())
}
