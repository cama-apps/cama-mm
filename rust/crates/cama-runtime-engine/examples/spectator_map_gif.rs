//! Offline, source-verified animation of the production spectator map.
//! Usage: spectator_map_gif CAPTURE_DIR OUTPUT.gif [SPEEDUP=15|--recap]
//! This does not connect to Steam, Discord, or any HTTP service.
#[path = "../../cama-runtime/src/dota_spectator_recap/encoder.rs"]
mod recap_encoder;
#[path = "../../cama-runtime/src/dota_spectator_recap/png.rs"]
mod recap_png;

use cama_app::pet_assets::{RasterImage, decode_png_raster};
use cama_runtime_engine::{
    dota_live::{LiveMapFrame, normalize_live_league_games, normalize_realtime_stats},
    dota_spectator_map::render_map,
};
use gif::{DisposalMethod, Encoder, Frame, Repeat};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::BTreeMap, fs, path::Path};

#[derive(Debug)]
struct Sample {
    map: LiveMapFrame,
    received: i64,
}

fn select_samples(samples: Vec<Sample>) -> Result<(Vec<Sample>, usize), String> {
    let match_id = samples.first().ok_or("no usable map samples")?.map.match_id;
    let mut selected: Vec<Sample> = Vec::new();
    let mut skipped = 0;
    for sample in samples {
        if sample.map.match_id != match_id {
            return Err("mixed-match map journal".into());
        }
        if let Some(previous) = selected.last() {
            if sample.received < previous.received {
                return Err("receive timestamps are out of order".into());
            }
            if sample.map.game_time <= previous.map.game_time {
                skipped += 1;
                continue;
            }
        }
        selected.push(sample);
    }
    Ok((selected, skipped))
}

fn load_samples(directory: &Path) -> Result<(Vec<Sample>, usize, usize), String> {
    let journal = fs::read_to_string(directory.join("frames.jsonl"))
        .map_err(|_| "cannot read frames.jsonl")?;
    let mut raw = BTreeMap::new();
    for entry in fs::read_dir(directory).map_err(|_| "cannot read capture directory")? {
        let path = entry.map_err(|_| "cannot read directory entry")?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("raw-") && name.ends_with(".json"))
        {
            let bytes = fs::read(path).map_err(|_| "cannot read raw capture")?;
            let hash = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            raw.insert(hash, bytes);
        }
    }
    let mut samples = Vec::new();
    let mut missing = 0;
    let mut expected_match = None;
    for line in journal.lines() {
        let record: Value = serde_json::from_str(line).map_err(|_| "invalid journal JSON")?;
        let match_id = record["frame"]["match_id"]
            .as_u64()
            .ok_or("missing match ID")?;
        if expected_match.is_some_and(|expected| expected != match_id) {
            return Err("mixed-match capture journal".into());
        }
        expected_match = Some(match_id);
        let received = chrono::DateTime::parse_from_rfc3339(
            record["received_at"]
                .as_str()
                .ok_or("missing received_at")?,
        )
        .map_err(|_| "invalid receive timestamp")?
        .timestamp_millis();
        let digest = record["raw_sha256"].as_str().ok_or("missing raw SHA-256")?;
        let bytes = raw
            .get(digest)
            .ok_or("raw capture does not match recorded SHA-256")?;
        let data: Value = serde_json::from_slice(bytes).map_err(|_| "invalid raw JSON")?;
        let snapshot = if data.get("result").unwrap_or(&data).get("games").is_some() {
            normalize_live_league_games(&data, 0, 0, match_id, received / 1000)
        } else {
            normalize_realtime_stats(&data, 0, 0, match_id, received / 1000)
        }
        .ok_or("raw response failed production normalization or match identity check")?;
        if let Some(map) = snapshot.map_frame {
            samples.push(Sample { map, received });
        } else {
            missing += 1;
        }
    }
    let (samples, skipped) = select_samples(samples)?;
    Ok((samples, skipped, missing))
}

struct Palette {
    colors: Vec<u8>,
    lookup: Vec<u8>,
}

impl Palette {
    fn new(image: &RasterImage) -> Result<Self, String> {
        let width = u16::try_from(image.width).map_err(|_| "image too wide")?;
        let height = u16::try_from(image.height).map_err(|_| "image too tall")?;
        let rgb: Vec<u8> = image
            .pixels
            .chunks_exact(4)
            .flat_map(|pixel| pixel[..3].iter().copied())
            .collect();
        let frame = Frame::from_rgb_speed(width, height, &rgb, 10);
        let mut colors = frame
            .palette
            .ok_or("GIF quantization returned no palette")?;
        colors.resize(768, 0);
        // Index 255 is reserved for unchanged pixels in subsequent delta frames.
        // Quantize a five-bit RGB lookup once instead of per pixel/per frame.
        let lookup = (0..32768)
            .map(|index| {
                let r = ((index >> 10) & 31) * 8 + 4;
                let g = ((index >> 5) & 31) * 8 + 4;
                let b = (index & 31) * 8 + 4;
                colors
                    .chunks_exact(3)
                    .take(255)
                    .enumerate()
                    .min_by_key(|(_, color)| {
                        let dr = r - i32::from(color[0]);
                        let dg = g - i32::from(color[1]);
                        let db = b - i32::from(color[2]);
                        dr * dr + dg * dg + db * db
                    })
                    .map_or(0, |(index, _)| index as u8)
            })
            .collect();
        Ok(Self { colors, lookup })
    }

    fn pixels(&self, image: &RasterImage) -> Vec<u8> {
        image
            .pixels
            .chunks_exact(4)
            .map(|pixel| {
                let index = (usize::from(pixel[0] >> 3) << 10)
                    | (usize::from(pixel[1] >> 3) << 5)
                    | usize::from(pixel[2] >> 3);
                self.lookup[index]
            })
            .collect()
    }
}

fn delta_frame(current: &[u8], previous: Option<&[u8]>, width: u16, height: u16) -> Frame<'static> {
    let stride = usize::from(width);
    let mut left = stride;
    let mut top = usize::from(height);
    let mut right = 0;
    let mut bottom = 0;
    for (index, pixel) in current.iter().enumerate() {
        if previous.is_none_or(|old| old[index] != *pixel) {
            left = left.min(index % stride);
            right = right.max(index % stride);
            top = top.min(index / stride);
            bottom = bottom.max(index / stride);
        }
    }
    if left == stride {
        return Frame {
            width: 1,
            height: 1,
            buffer: Cow::Owned(vec![255]),
            transparent: Some(255),
            dispose: DisposalMethod::Keep,
            ..Frame::default()
        };
    }
    let mut pixels = Vec::with_capacity((right - left + 1) * (bottom - top + 1));
    for y in top..=bottom {
        for x in left..=right {
            let index = y * stride + x;
            pixels.push(
                if previous.is_some_and(|old| old[index] == current[index]) {
                    255
                } else {
                    current[index]
                },
            );
        }
    }
    Frame {
        left: left as u16,
        top: top as u16,
        width: (right - left + 1) as u16,
        height: (bottom - top + 1) as u16,
        buffer: Cow::Owned(pixels),
        transparent: previous.map(|_| 255),
        dispose: DisposalMethod::Keep,
        ..Frame::default()
    }
}

fn delay(sample: &Sample, next: Option<&Sample>, speedup: f64) -> u64 {
    let milliseconds = next.map_or(15000, |next| (next.received - sample.received).max(0));
    // GIF delays are centiseconds. At least 20ms avoids browser zero-delay defaults.
    ((milliseconds as f64 / speedup / 10.0).round() as u64).max(2)
}

fn write_frame_with_delay<W: std::io::Write>(
    encoder: &mut Encoder<W>,
    mut frame: Frame<'_>,
    mut delay: u64,
) -> Result<usize, String> {
    let mut count = 0;
    while delay > 0 {
        frame.delay = delay.min(u64::from(u16::MAX)) as u16;
        encoder
            .write_frame(&frame)
            .map_err(|error| format!("cannot write GIF frame: {error}"))?;
        delay -= u64::from(frame.delay);
        count += 1;
    }
    Ok(count)
}

fn clock(time: i64) -> String {
    format!("{}:{:02}", time.div_euclid(60), time.rem_euclid(60))
}

fn generate(directory: &Path, output: &Path, speedup: f64) -> Result<(), String> {
    if !speedup.is_finite() || !(1.0..=100.0).contains(&speedup) {
        return Err("speedup must be between 1 and 100".into());
    }
    let (samples, skipped, missing) = load_samples(directory)?;
    let first = &samples[0];
    let last = samples.last().ok_or("no samples")?;
    let first_image =
        decode_png_raster(&render_map(&first.map)?).ok_or("cannot decode production map PNG")?;
    let palette = Palette::new(&first_image)?;
    let width = u16::try_from(first_image.width).map_err(|_| "image too wide")?;
    let height = u16::try_from(first_image.height).map_err(|_| "image too tall")?;
    let file = fs::File::create(output).map_err(|_| "cannot create GIF")?;
    let mut encoder =
        Encoder::new(file, width, height, &palette.colors).map_err(|error| error.to_string())?;
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|error| error.to_string())?;
    let mut previous = None;
    let mut gif_frames = 0;
    let mut playback_cs = 0;
    for (index, sample) in samples.iter().enumerate() {
        let image = if index == 0 {
            first_image.clone()
        } else {
            decode_png_raster(&render_map(&sample.map)?)
                .ok_or("cannot decode production map PNG")?
        };
        if image.width != usize::from(width) || image.height != usize::from(height) {
            return Err("production map changed dimensions within capture".into());
        }
        let pixels = palette.pixels(&image);
        let frame = delta_frame(&pixels, previous.as_deref(), width, height);
        let centiseconds = delay(sample, samples.get(index + 1), speedup);
        gif_frames += write_frame_with_delay(&mut encoder, frame, centiseconds)?;
        playback_cs += centiseconds;
        previous = Some(pixels);
    }
    drop(encoder);
    let mut summary = format!(
        "# Live spectator map animation — match {}\n\nSource: Valve api.steampowered.com. Every frame was rebuilt from a SHA-256-verified raw response using the production normalizer and map renderer. No Steam session, observer, or Discord connection was opened to generate this artifact.\n\n- Game-clock coverage: **{}–{}** ({} seconds of game clock).\n- Capture receipt window: {} through {}.\n- Distinct map samples: {}; GIF frames: {}.\n- Cached/regressed clocks skipped: {}; journal entries without a map: {}.\n- Native image: {} × {} pixels; GIF size: {} bytes.\n- Playback: {}× speed; {:.2} seconds, looping.\n\nThis is the observed live segment only. Earlier gameplay and the result are not reconstructed. Positions are discrete observations, never interpolated. Playback holds each map for its actual receive-time gap divided by the speedup; pauses, stale samples and missing polls therefore remain visible as longer holds. The final map is held for one nominal 15-second poll interval before the animation loops. The clock printed inside the map is the source game clock, not the capture clock. GIF palette compression can slightly change colors.\n\n## Sample timeline\n\n| Source game clock | Received UTC | Receive gap to next map |\n|---|---|---|\n",
        first.map.match_id,
        clock(first.map.game_time),
        clock(last.map.game_time),
        last.map.game_time - first.map.game_time,
        timestamp(first.received)?,
        timestamp(last.received)?,
        samples.len(),
        gif_frames,
        skipped,
        missing,
        width,
        height,
        fs::metadata(output).map_err(|_| "cannot stat GIF")?.len(),
        speedup,
        playback_cs as f64 / 100.0,
    );
    for (index, sample) in samples.iter().enumerate() {
        let gap = samples
            .get(index + 1)
            .map_or("final 15s hold".into(), |next| {
                format!("{:.3}s", (next.received - sample.received) as f64 / 1000.0)
            });
        summary.push_str(&format!(
            "| {} | {} | {} |\n",
            clock(sample.map.game_time),
            timestamp(sample.received)?,
            gap
        ));
    }
    let summary_path = output.with_extension("md");
    fs::write(&summary_path, summary).map_err(|_| "cannot write animation provenance")?;
    println!(
        "Rendered {} observed map samples to {} ({} bytes); provenance {}",
        samples.len(),
        output.display(),
        fs::metadata(output).map_err(|_| "cannot stat GIF")?.len(),
        summary_path.display()
    );
    Ok(())
}

/// Replay verified observations through the production archival PNG/GIF path.
fn generate_recap(directory: &Path, output: &Path) -> Result<(), String> {
    let (samples, skipped, missing) = load_samples(directory)?;
    let first = samples.first().ok_or("no usable map samples")?;
    let last = samples.last().ok_or("no usable map samples")?;
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut paths = Vec::with_capacity(samples.len());
    let mut archive_bytes = 0u64;
    for (index, sample) in samples.iter().enumerate() {
        // Only one rendered screenshot is resident at a time. Temporary files
        // disappear even if encoding fails; no source capture is modified.
        let png = recap_png::compress(&render_map(&sample.map)?)?;
        archive_bytes += png.len() as u64;
        let path = temporary.path().join(format!("frame-{index:06}.png"));
        fs::write(&path, png).map_err(|error| error.to_string())?;
        paths.push(path);
    }
    let info = recap_encoder::encode(&paths, output)?;
    let playback = f64::from(info.duration_cs) / 100.0;
    let game_seconds = last.map.game_time - first.map.game_time;
    let summary_path = output.with_extension("md");
    let summary = format!(
        "# Map recap preview — match {}\n\nBuilt offline from SHA-256-verified Valve responses, using the production map renderer, lossless archival PNG compression, and bounded recap GIF encoder.\n\n- Observed game-clock coverage: **{}–{}** ({} seconds).\n- Input observations: {}; selected GIF frames: {}.\n- Playback: **{:.2} seconds**, approximately **{:.1}×** game-clock speed, looping.\n- GIF: {} bytes; compressed screenshot archive: {} bytes.\n- Cached/regressed observations skipped: {}; records without map data: {}.\n\nOnly this captured live segment is shown; earlier play and the match result are not reconstructed. The encoder samples across the segment, holds each selected frame for at least half a second, and holds the final frame for one second. The completed GIF stays below 45 seconds.\n",
        first.map.match_id,
        clock(first.map.game_time),
        clock(last.map.game_time),
        game_seconds,
        samples.len(),
        info.frame_count,
        playback,
        game_seconds as f64 / playback,
        info.bytes,
        archive_bytes,
        skipped,
        missing,
    );
    fs::write(&summary_path, summary).map_err(|error| error.to_string())?;
    println!(
        "Recap: {} frames, {:.2}s, {} bytes; {} verified observations; {}. Provenance: {}",
        info.frame_count,
        playback,
        info.bytes,
        samples.len(),
        output.display(),
        summary_path.display()
    );
    Ok(())
}

fn timestamp(milliseconds: i64) -> Result<String, String> {
    chrono::DateTime::from_timestamp_millis(milliseconds)
        .map(|time| time.to_rfc3339())
        .ok_or("receive timestamp out of range".into())
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: spectator_map_gif CAPTURE_DIR OUTPUT.gif [SPEEDUP=15|--recap]".into());
    }
    if args.get(2).is_some_and(|value| value == "--recap") {
        return generate_recap(Path::new(&args[0]), Path::new(&args[1]));
    }
    let speedup = args.get(2).map_or(Ok(15.0), |value| {
        value.parse::<f64>().map_err(|_| "invalid speedup")
    })?;
    generate(Path::new(&args[0]), Path::new(&args[1]), speedup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cama_app::pet_assets::Rgba;
    fn sample(match_id: u64, clock: i64, received: i64) -> Sample {
        Sample {
            received,
            map: LiveMapFrame {
                match_id,
                game_time: clock,
                radiant_net_worth: None,
                dire_net_worth: None,
                heroes: vec![],
                buildings: vec![],
                roshan_respawn_seconds: None,
            },
        }
    }
    #[test]
    fn skips_cached_and_regressed_clocks_without_inventing_samples() {
        let (samples, skipped) = select_samples(vec![
            sample(1, 30, 0),
            sample(1, 30, 15000),
            sample(1, 29, 30000),
            sample(1, 60, 45000),
        ])
        .unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(skipped, 2);
        assert_eq!(delay(&samples[0], Some(&samples[1]), 15.0), 300);
    }
    #[test]
    fn rejects_mixed_matches_even_with_regressed_clock() {
        assert!(
            select_samples(vec![sample(1, 30, 0), sample(2, 0, 15000)])
                .unwrap_err()
                .contains("mixed-match")
        );
    }
    #[test]
    fn rejects_out_of_order_receipt_timestamps() {
        assert!(select_samples(vec![sample(1, 30, 15000), sample(1, 45, 0)]).is_err());
    }
    #[test]
    fn gif_global_palette_and_transparent_deltas_preserve_composited_pixels() {
        let mut image = RasterImage::new(8, 8, Rgba(20, 30, 40, 255));
        image.pixels[0..4].copy_from_slice(&[220, 20, 20, 255]);
        let palette = Palette::new(&image).unwrap();
        let first = palette.pixels(&image);
        image.pixels[4..8].copy_from_slice(&[220, 20, 20, 255]);
        let second = palette.pixels(&image);
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 8, 8, &palette.colors).unwrap();
            write_frame_with_delay(&mut encoder, delta_frame(&first, None, 8, 8), 100).unwrap();
            write_frame_with_delay(&mut encoder, delta_frame(&second, Some(&first), 8, 8), 200)
                .unwrap();
        }
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::Indexed);
        let mut reader = options.read_info(bytes.as_slice()).unwrap();
        assert_eq!(reader.global_palette(), Some(palette.colors.as_slice()));
        let mut composed = vec![0; 64];
        let mut count = 0;
        while let Some(frame) = reader.read_next_frame().unwrap() {
            assert!(frame.palette.is_none());
            assert_eq!(frame.delay, if count == 0 { 100 } else { 200 });
            for y in 0..usize::from(frame.height) {
                for x in 0..usize::from(frame.width) {
                    let pixel = frame.buffer[y * usize::from(frame.width) + x];
                    if Some(pixel) != frame.transparent {
                        composed[(usize::from(frame.top) + y) * 8 + usize::from(frame.left) + x] =
                            pixel;
                    }
                }
            }
            assert_eq!(composed, if count == 0 { &first } else { &second }.to_vec());
            count += 1;
        }
        assert_eq!(count, 2);
    }
    #[test]
    fn very_long_gap_splits_gif_delay_without_truncation() {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 1, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
            assert_eq!(
                write_frame_with_delay(&mut encoder, delta_frame(&[0], None, 1, 1), 70000).unwrap(),
                2
            );
        }
        let mut reader = gif::DecodeOptions::new()
            .read_info(bytes.as_slice())
            .unwrap();
        let mut delay = 0u64;
        while let Some(frame) = reader.read_next_frame().unwrap() {
            delay += u64::from(frame.delay);
        }
        assert_eq!(delay, 70000);
    }
}
