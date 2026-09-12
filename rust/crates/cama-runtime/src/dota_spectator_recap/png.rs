//! Lossless archival compression of the production renderer's RGBA PNGs.
use flate2::{Compression, Crc, read::ZlibDecoder, write::ZlibEncoder};
use std::io::{Read, Write};

const MAX_INPUT: usize = 4 * 1024 * 1024;
const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("truncated PNG")?
            .try_into()
            .map_err(|_| "truncated PNG")?,
    ))
}

pub(super) fn compress(png: &[u8]) -> Result<Vec<u8>, String> {
    if png.len() > MAX_INPUT || !png.starts_with(SIGNATURE) {
        return Err("invalid or oversized archival PNG".into());
    }
    let mut cursor = 8;
    let mut dimensions = None;
    let mut idat = Vec::new();
    let mut chunks = Vec::new();
    let mut ended = false;
    let mut idat_closed = false;
    while cursor < png.len() {
        let length = u32_at(png, cursor)? as usize;
        let start = cursor.checked_add(8).ok_or("invalid PNG chunk")?;
        let end = start.checked_add(length).ok_or("invalid PNG chunk")?;
        let chunk_end = end.checked_add(4).ok_or("invalid PNG chunk")?;
        let kind = png.get(cursor + 4..start).ok_or("truncated PNG chunk")?;
        let data = png.get(start..end).ok_or("truncated PNG chunk")?;
        let expected_crc = u32_at(png, end)?;
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        if crc.sum() != expected_crc {
            return Err("invalid PNG CRC".into());
        }
        if dimensions.is_none() && kind != b"IHDR" {
            return Err("PNG header must be first".into());
        }
        match kind {
            b"IHDR" => {
                if dimensions.is_some() || data.len() != 13 || data[8..] != [8, 6, 0, 0, 0] {
                    return Err("unsupported archival PNG format".into());
                }
                let width = u32_at(data, 0)? as usize;
                let height = u32_at(data, 4)? as usize;
                if width == 0 || height == 0 || width > 1240 || height > 704 {
                    return Err("archival PNG dimensions exceed 1240x704".into());
                }
                dimensions = Some((width, height));
            }
            b"IDAT" => {
                if idat_closed {
                    return Err("nonconsecutive PNG image data".into());
                }
                idat.extend_from_slice(data);
            }
            b"IEND" => {
                if !data.is_empty() || idat.is_empty() || chunk_end != png.len() {
                    return Err("invalid PNG end".into());
                }
                ended = true;
            }
            b"PLTE" => {
                if !idat.is_empty() {
                    return Err("PNG palette after image data".into());
                }
            }
            _ if kind[0].is_ascii_uppercase() => {
                return Err("unsupported critical PNG chunk".into());
            }
            _ => {
                if !idat.is_empty() {
                    idat_closed = true;
                }
            }
        }
        chunks.push((kind, &png[cursor..chunk_end]));
        cursor = chunk_end;
    }
    if !ended {
        return Err("PNG is missing its end".into());
    }
    let (width, height) = dimensions.ok_or("PNG is missing dimensions")?;
    let row = width * 4 + 1;
    let expected = row * height;
    let mut raw = Vec::with_capacity(expected + 1);
    let mut decoder = ZlibDecoder::new(idat.as_slice());
    (&mut decoder)
        .take((expected + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() != expected
        || decoder.total_in() != idat.len() as u64
        || raw.chunks_exact(row).any(|row| row[0] > 4)
    {
        return Err("invalid or oversized PNG decompressed data".into());
    }
    drop(decoder);
    drop(idat);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&raw).map_err(|error| error.to_string())?;
    drop(raw);
    let compressed = encoder.finish().map_err(|error| error.to_string())?;
    let mut output = Vec::new();
    output.extend_from_slice(SIGNATURE);
    let mut wrote_idat = false;
    for (kind, chunk) in chunks {
        if kind == b"IDAT" {
            if !wrote_idat {
                append_chunk(&mut output, b"IDAT", &compressed);
                wrote_idat = true;
            }
        } else {
            output.extend_from_slice(chunk);
        }
        if output.len() > MAX_INPUT {
            return Err("compressed archival PNG exceeds 4 MiB".into());
        }
    }
    Ok(output)
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
    use super::*;
    use cama_app::pet_assets::{RasterImage, Rgba, decode_png_raster};

    #[test]
    fn compresses_production_png_losslessly_at_full_map_dimensions() {
        let image = RasterImage::new(1240, 704, Rgba(30, 60, 90, 255));
        let original = image.encode_png();
        assert!(original.len() > 2 * 1024 * 1024);
        let compressed = compress(&original).unwrap();
        assert!(compressed.len() < original.len() / 10);
        assert_eq!(decode_png_raster(&compressed).unwrap(), image);
        let tiny = RasterImage::new(4, 4, Rgba(10, 20, 30, 255));
        assert_eq!(
            decode_png_raster(&compress(&tiny.encode_png()).unwrap()).unwrap(),
            tiny
        );
    }

    #[test]
    fn preserves_ancillary_chunks_and_joins_consecutive_idat_chunks() {
        let image = RasterImage::new(4, 4, Rgba(10, 20, 30, 255));
        let original = image.encode_png();
        let mut png = original[..33].to_vec();
        append_chunk(&mut png, b"tEXt", b"source\0rendered-map");
        let idat_size = u32_at(&original, 33).unwrap() as usize;
        let data = &original[41..41 + idat_size];
        append_chunk(&mut png, b"IDAT", &data[..data.len() / 2]);
        append_chunk(&mut png, b"IDAT", &data[data.len() / 2..]);
        append_chunk(&mut png, b"IEND", &[]);
        let compressed = compress(&png).unwrap();
        assert!(
            compressed
                .windows(b"source\0rendered-map".len())
                .any(|window| window == b"source\0rendered-map")
        );
        assert_eq!(decode_png_raster(&compressed).unwrap(), image);
    }

    #[test]
    fn rejects_malformed_input_dimensions_and_decompression_bombs() {
        assert!(compress(b"not PNG").is_err());
        assert!(compress(&vec![0; MAX_INPUT + 1]).is_err());
        let wide = RasterImage::new(1241, 1, Rgba(0, 0, 0, 255)).encode_png();
        assert!(compress(&wide).unwrap_err().contains("dimensions"));
        let valid = RasterImage::new(4, 4, Rgba(0, 0, 0, 255)).encode_png();
        let mut bad_crc = valid.clone();
        bad_crc[50] ^= 1;
        assert!(compress(&bad_crc).unwrap_err().contains("CRC"));
        assert!(compress(&valid[..valid.len() - 1]).is_err());
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&vec![0; 1024 * 1024]).unwrap();
        let bomb = encoder.finish().unwrap();
        let mut bad = valid[..33].to_vec();
        append_chunk(&mut bad, b"IDAT", &bomb);
        append_chunk(&mut bad, b"IEND", &[]);
        assert!(compress(&bad).unwrap_err().contains("decompressed"));
        let mut repeated_header = valid[..33].to_vec();
        repeated_header.extend_from_slice(&valid[8..]);
        assert!(compress(&repeated_header).is_err());
    }
}
