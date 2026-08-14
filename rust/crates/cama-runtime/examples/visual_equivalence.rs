//! Emit the betting provider's production wheel media for the visual gate.
//!
//! This example is intentionally small and side-effect free: it calls the
//! same attachment renderers used by the live provider and writes only the
//! requested GIF bytes to the supplied output directory.  The companion
//! Python runner owns decoding and perceptual comparison.

use std::env;
use std::fs;
use std::path::Path;

use cama_app::wheel::{WheelEconomyPolicy, get_wheel_wedges};
use cama_runtime::betting_provider::{
    render_explosion_attachment_for_visual_equivalence,
    render_wheel_attachment_for_visual_equivalence,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    wheel: WheelFixture,
}

#[derive(Debug, Deserialize)]
struct WheelFixture {
    target_index: usize,
    is_bankrupt: bool,
    is_golden: bool,
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
    let policy = WheelEconomyPolicy::default();
    let wedges = get_wheel_wedges(fixture.wheel.is_bankrupt, fixture.wheel.is_golden, policy);
    let wheel = render_wheel_attachment_for_visual_equivalence(
        &wedges,
        fixture.wheel.target_index,
        fixture.wheel.is_golden,
    )?;
    if wheel.filename != "wheel.gif" {
        return Err(format!("unexpected wheel attachment name: {}", wheel.filename).into());
    }
    fs::write(Path::new(&output_dir).join("rust_wheel.gif"), wheel.bytes)?;

    let explosion = render_explosion_attachment_for_visual_equivalence()?;
    if explosion.filename != "explosion.gif" {
        return Err(format!(
            "unexpected explosion attachment name: {}",
            explosion.filename
        )
        .into());
    }
    fs::write(
        Path::new(&output_dir).join("rust_explosion.gif"),
        explosion.bytes,
    )?;
    Ok(())
}
