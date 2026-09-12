//! Bounded, offline animation of already rendered spectator screenshots.
use cama_app::pet_assets::{RasterImage, decode_png_raster, inspect_png};
use gif::{DisposalMethod, Encoder, Frame, Repeat};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_FRAMES: usize = 88;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const FRAME_DELAY_CS: u16 = 50;
const FINAL_DELAY_CS: u16 = 100;

#[derive(Debug)]
pub(super) struct RecapInfo {
    pub(super) duration_cs: u32,
    pub(super) frame_count: usize,
    pub(super) bytes: u64,
}

/// Keep the beginning and end, and spread the remaining samples uniformly.
fn selected_indices(count: usize) -> Vec<usize> {
    let selected = count.min(MAX_FRAMES);
    match selected {
        0 => Vec::new(),
        1 => vec![0],
        _ => (0..selected)
            .map(|index| index * (count - 1) / (selected - 1))
            .collect(),
    }
}

fn read_png(path: &Path) -> Result<Vec<u8>, String> {
    let file =
        fs::File::open(path).map_err(|error| format!("cannot read recap screenshot: {error}"))?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    if length > MAX_FILE_BYTES {
        return Err("recap screenshot exceeds 8 MiB".into());
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("recap screenshot exceeds 8 MiB".into());
    }
    Ok(bytes)
}

fn decode_screenshot(bytes: &[u8]) -> Result<RasterImage, String> {
    // Inspect dimensions before decoding to bound decompression allocations.
    let info = inspect_png(bytes).map_err(|error| format!("invalid recap screenshot: {error}"))?;
    if info.width == 0 || info.height == 0 || info.width > 1240 || info.height > 704 {
        return Err("recap screenshot dimensions exceed 1240x704".into());
    }
    decode_png_raster(bytes).ok_or_else(|| "cannot decode recap screenshot".into())
}

struct LimitedWriter<W> {
    inner: W,
    bytes: u64,
    limit: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("recap GIF exceeds upload budget"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn encode(paths: &[PathBuf], output: &Path) -> Result<RecapInfo, String> {
    encode_with_limit(paths, output, MAX_FILE_BYTES)
}

fn encode_with_limit(paths: &[PathBuf], output: &Path, limit: u64) -> Result<RecapInfo, String> {
    // A read-only pass keeps only digests/path references, never the full movie.
    let mut previous_digest = None;
    let mut unique = Vec::new();
    for path in paths {
        let bytes = read_png(path)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if previous_digest != Some(digest) {
            unique.push(path);
            previous_digest = Some(digest);
        }
    }
    if unique.is_empty() {
        return Err("no recap screenshots available".into());
    }
    let indices = selected_indices(unique.len());
    let first = decode_screenshot(&read_png(unique[indices[0]])?)?;
    let dimensions = (first.width, first.height);
    let width = first.width as u16;
    let height = first.height as u16;
    let palette = Palette::new(&first)?;
    let first_pixels = palette.pixels(&first);
    drop(first);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("cannot create recap GIF: {error}"))?;
    let result = (|| {
        let writer = LimitedWriter {
            inner: file,
            bytes: 0,
            limit,
        };
        let mut encoder = Encoder::new(writer, width, height, &palette.colors)
            .map_err(|error| error.to_string())?;
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|error| error.to_string())?;
        let mut previous = None;
        let mut first_pixels = Some(first_pixels);
        let mut duration_cs = 0;
        for (position, index) in indices.iter().enumerate() {
            let pixels = if let Some(first) = first_pixels.take() {
                first
            } else {
                let image = decode_screenshot(&read_png(unique[*index])?)?;
                if (image.width, image.height) != dimensions {
                    return Err("recap screenshots have different dimensions".into());
                }
                palette.pixels(&image)
            };
            let mut frame = delta_frame(&pixels, previous.as_deref(), width, height);
            frame.delay = if position + 1 == indices.len() {
                FINAL_DELAY_CS
            } else {
                FRAME_DELAY_CS
            };
            duration_cs += u32::from(frame.delay);
            encoder
                .write_frame(&frame)
                .map_err(|error| error.to_string())?;
            previous = Some(pixels);
        }
        // Explicitly finalize so a trailer-write failure cannot report success.
        let mut writer = encoder.into_inner().map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
        Ok(RecapInfo {
            duration_cs,
            frame_count: indices.len(),
            bytes: writer.bytes,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use cama_app::pet_assets::Rgba;

    fn screenshot(directory: &Path, name: &str, red: u8, width: usize) -> PathBuf {
        let path = directory.join(name);
        fs::write(
            &path,
            RasterImage::new(width, 8, Rgba(red, 30, 40, 255)).encode_png(),
        )
        .unwrap();
        path
    }

    #[test]
    fn recap_timing_is_legible_and_under_45_seconds_for_30_60_minute_and_long_matches() {
        for count in [1, 2, 30, 120, 240, 1000] {
            let selected = selected_indices(count);
            assert_eq!(selected.first(), Some(&0));
            assert_eq!(selected.last(), Some(&(count - 1)));
            assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
            let duration =
                (selected.len() - 1) * usize::from(FRAME_DELAY_CS) + usize::from(FINAL_DELAY_CS);
            assert!(duration <= 4500);
            assert!(selected.len() <= MAX_FRAMES);
            if count <= MAX_FRAMES {
                assert_eq!(selected.len(), count);
            }
            let gaps: Vec<_> = selected.windows(2).map(|pair| pair[1] - pair[0]).collect();
            if let (Some(min), Some(max)) = (gaps.iter().min(), gaps.iter().max()) {
                assert!(max - min <= 1);
            }
        }
        assert!(selected_indices(0).is_empty());
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
            for (pixels, previous) in [
                (&first, None),
                (&second, Some(first.as_slice())),
                (&second, Some(second.as_slice())),
            ] {
                let mut frame = delta_frame(pixels, previous, 8, 8);
                frame.delay = FRAME_DELAY_CS;
                encoder.write_frame(&frame).unwrap();
            }
        }
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::Indexed);
        let mut reader = options.read_info(bytes.as_slice()).unwrap();
        assert_eq!(reader.global_palette(), Some(palette.colors.as_slice()));
        let mut composed = vec![0; 64];
        let mut count = 0;
        while let Some(frame) = reader.read_next_frame().unwrap() {
            assert!(frame.palette.is_none());
            assert_eq!(frame.delay, FRAME_DELAY_CS);
            for y in 0..usize::from(frame.height) {
                for x in 0..usize::from(frame.width) {
                    let pixel = frame.buffer[y * usize::from(frame.width) + x];
                    if Some(pixel) != frame.transparent {
                        composed[(usize::from(frame.top) + y) * 8 + usize::from(frame.left) + x] =
                            pixel;
                    }
                }
            }
            assert_eq!(&composed, if count == 0 { &first } else { &second });
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn reads_saved_frames_skips_repeats_and_reports_actual_file_duration_and_size() {
        let directory = tempfile::tempdir().unwrap();
        let first = screenshot(directory.path(), "first.png", 20, 8);
        let second = screenshot(directory.path(), "second.png", 220, 8);
        let output = directory.path().join("recap.gif");
        // A later return to the first image must remain the final visual state.
        let info = encode(&[first.clone(), first.clone(), second, first], &output).unwrap();
        assert_eq!(info.frame_count, 3);
        assert_eq!(info.duration_cs, 200);
        assert_eq!(info.bytes, fs::metadata(&output).unwrap().len());
        let bytes = fs::read(&output).unwrap();
        let mut reader = gif::DecodeOptions::new()
            .read_info(bytes.as_slice())
            .unwrap();
        let mut delays = Vec::new();
        while let Some(frame) = reader.read_next_frame().unwrap() {
            delays.push(frame.delay);
        }
        assert_eq!(delays, vec![50, 50, 100]);
        assert!(encode(&[], &directory.path().join("empty.gif")).is_err());
    }

    #[test]
    fn rejects_malformed_oversized_and_mixed_dimensions_and_removes_partial_output() {
        let directory = tempfile::tempdir().unwrap();
        let first = screenshot(directory.path(), "first.png", 20, 8);
        let other = screenshot(directory.path(), "other.png", 30, 9);
        let output = directory.path().join("mixed.gif");
        assert!(
            encode(&[first.clone(), other], &output)
                .unwrap_err()
                .contains("different dimensions")
        );
        assert!(!output.exists());
        let malformed = directory.path().join("broken.png");
        fs::write(&malformed, b"not a PNG").unwrap();
        assert!(encode(&[malformed], &output).is_err());
        let oversized = screenshot(directory.path(), "wide.png", 20, 1241);
        assert!(
            encode(&[oversized], &output)
                .unwrap_err()
                .contains("dimensions")
        );
        let huge = directory.path().join("huge.png");
        fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(
            encode(&[huge], &output)
                .unwrap_err()
                .contains("exceeds 8 MiB")
        );
        assert!(!output.exists());
        // Existing output is never overwritten or deleted on creation failure.
        fs::write(&output, b"existing").unwrap();
        assert!(encode(&[first], &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"existing");
    }

    #[test]
    fn output_budget_is_enforced_while_writing() {
        let directory = tempfile::tempdir().unwrap();
        let first = screenshot(directory.path(), "first.png", 20, 8);
        let output = directory.path().join("bounded.gif");
        assert!(encode_with_limit(&[first], &output, 64).is_err());
        assert!(!output.exists());
        let mut writer = LimitedWriter {
            inner: Vec::new(),
            bytes: 0,
            limit: 5,
        };
        writer.write_all(b"1234").unwrap();
        assert!(writer.write_all(b"56").is_err());
        assert_eq!(writer.bytes, 4);
        assert_eq!(writer.inner, b"1234");
    }
}
