//! Shared PNG encoder for the native image renderers.
//!
//! Every renderer keeps row-major 8-bit pixels; this writes them as one
//! zlib-compressed `IDAT` (filter type None) with table-driven CRC-32s.

use std::io::Write;

use flate2::{Compression, Crc, write::ZlibEncoder};

pub const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Encode `rgba` (`width * height` straight RGBA8 pixels) as a PNG.
#[must_use]
pub fn encode_rgba_png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    encode(width, height, 6, 4, rgba)
}

/// Encode `rgb` (`width * height` RGB8 pixels) as an opaque PNG.
#[must_use]
pub fn encode_rgb_png(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    encode(width, height, 2, 3, rgb)
}

/// The PNG chunk CRC-32 of `bytes`.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc::new();
    crc.update(bytes);
    crc.sum()
}

fn encode(width: usize, height: usize, color_type: u8, channels: usize, pixels: &[u8]) -> Vec<u8> {
    let row_bytes = width * channels;
    debug_assert_eq!(pixels.len(), row_bytes * height, "PNG pixel buffer size");
    let mut filtered = Vec::with_capacity((row_bytes + 1) * height);
    if row_bytes > 0 {
        for row in pixels.chunks_exact(row_bytes) {
            filtered.push(0);
            filtered.extend_from_slice(row);
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&filtered)
        .expect("in-memory PNG compression cannot fail");
    let zlib = encoder
        .finish()
        .expect("in-memory PNG compression cannot fail");

    let mut png = Vec::with_capacity(PNG_SIGNATURE.len() + 25 + zlib.len() + 24);
    png.extend_from_slice(PNG_SIGNATURE);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header);
    append_chunk(&mut png, b"IDAT", &zlib);
    append_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(data);
    output.extend_from_slice(&crc.sum().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::ZlibDecoder;

    use super::*;

    /// Minimal decoder for the encoder's own output: checks every chunk CRC
    /// and returns the dimensions, colour type, and unfiltered pixels.
    fn decode(png: &[u8]) -> (u32, u32, u8, Vec<u8>) {
        assert!(png.starts_with(PNG_SIGNATURE));
        let mut cursor = PNG_SIGNATURE.len();
        let (mut width, mut height, mut color_type) = (0, 0, 0);
        let mut idat = Vec::new();
        while cursor < png.len() {
            let length = u32::from_be_bytes(png[cursor..cursor + 4].try_into().unwrap()) as usize;
            let kind = &png[cursor + 4..cursor + 8];
            let data = &png[cursor + 8..cursor + 8 + length];
            let expected = u32::from_be_bytes(
                png[cursor + 8 + length..cursor + 12 + length]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(crc32(&png[cursor + 4..cursor + 8 + length]), expected);
            match kind {
                b"IHDR" => {
                    width = u32::from_be_bytes(data[..4].try_into().unwrap());
                    height = u32::from_be_bytes(data[4..8].try_into().unwrap());
                    color_type = data[9];
                }
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            cursor += length + 12;
        }
        let mut filtered = Vec::new();
        ZlibDecoder::new(idat.as_slice())
            .read_to_end(&mut filtered)
            .unwrap();
        let channels = if color_type == 6 { 4 } else { 3 };
        let row_bytes = width as usize * channels;
        let pixels = filtered
            .chunks_exact(row_bytes + 1)
            .flat_map(|row| {
                assert_eq!(row[0], 0, "filter type None");
                row[1..].to_vec()
            })
            .collect();
        (width, height, color_type, pixels)
    }

    #[test]
    fn rgba_round_trips_losslessly() {
        let (width, height) = (5, 3);
        let pixels: Vec<u8> = (0..width * height * 4)
            .map(|value| (value * 37) as u8)
            .collect();
        let png = encode_rgba_png(width, height, &pixels);
        assert_eq!(decode(&png), (5, 3, 6, pixels));
    }

    #[test]
    fn rgb_round_trips_losslessly() {
        let pixels: Vec<u8> = (0..4 * 2 * 3).map(|value| (value * 11) as u8).collect();
        let png = encode_rgb_png(4, 2, &pixels);
        assert_eq!(decode(&png), (4, 2, 2, pixels));
    }

    #[test]
    fn flat_images_compress_far_below_raw_size() {
        let pixels = [24_u8, 26, 33, 255].repeat(700 * 400);
        let png = encode_rgba_png(700, 400, &pixels);
        assert!(
            png.len() < 100 * 1024,
            "flat 700x400 PNG is {} bytes",
            png.len()
        );
        assert_eq!(decode(&png).3, pixels);
    }

    #[test]
    fn crc32_matches_the_png_reference_value() {
        // CRC of the bare IEND chunk type, fixed by the PNG specification.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }
}
