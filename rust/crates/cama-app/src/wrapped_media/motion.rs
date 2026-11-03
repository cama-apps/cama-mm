//! GIF encoding for card reveals.
//!
//! Cards are mostly flat color, so one shared palette fits every frame. Each
//! frame stores only the region that changed since the one before it, and
//! pixels that did not change inside that region are transparent.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::stage::Stage;
use super::*;

pub(super) const FRAMES: usize = 30;
/// Hundredths of a second. Twenty-five frames a second is near the fastest pace
/// that clients honor rather than slow down.
const FRAME_DELAY: u16 = 4;
/// No loop extension: one reveal, then the finished card. If a client replays it
/// anyway, a long hold keeps the card readable between plays.
const FINAL_HOLD: u16 = 600;
const WIDTH: usize = WRAPPED_SLIDE_WIDTH;
const HEIGHT: usize = WRAPPED_SLIDE_HEIGHT;
/// Colors available to the image; the last palette slot means "unchanged".
const COLORS: usize = 255;

pub(super) fn animate(slide: &WrappedSlideData) -> Result<Vec<u8>, gif::EncodingError> {
    let mut stage = Stage::new(slide);
    let colors = shared_palette(&mut stage);
    let transparent = (colors.len() / 3) as u8;
    let mut table = colors.clone();
    table.extend_from_slice(&[0, 0, 0]);
    let mut mapper = ColorMapper::new(&colors);
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, WIDTH as u16, HEIGHT as u16, &table)?;
        let mut previous: Option<Vec<u8>> = None;
        let mut pending: Option<gif::Frame<'static>> = None;
        for index in 0..FRAMES {
            let t = index as f32 / (FRAMES - 1) as f32;
            let indexed = mapper.map(&stage.paint(t).pixels);
            let frame = match &previous {
                None => Some(sub_frame(
                    None,
                    &indexed,
                    (0, 0, WIDTH, HEIGHT),
                    transparent,
                )),
                Some(before) => changed_region(before, &indexed)
                    .map(|region| sub_frame(Some(before), &indexed, region, transparent)),
            };
            if let Some(frame) = frame {
                if let Some(ready) = pending.replace(frame) {
                    encoder.write_frame(&ready)?;
                }
            } else if let Some(frame) = &mut pending {
                frame.delay = frame.delay.saturating_add(FRAME_DELAY);
            }
            previous = Some(indexed);
        }
        if let Some(mut frame) = pending {
            frame.delay = FINAL_HOLD;
            encoder.write_frame(&frame)?;
        }
    }
    Ok(bytes)
}

/// Exact colors when a card has few enough, otherwise a quantized palette drawn
/// from across the timeline so transient colors still have a home.
fn shared_palette(stage: &mut Stage<'_>) -> Vec<u8> {
    const SAMPLES: [f32; 4] = [1.0, 0.75, 0.5, 0.25];
    let mut stacked = Vec::with_capacity(WIDTH * HEIGHT * 4 * SAMPLES.len());
    let mut unique = HashSet::new();
    for t in SAMPLES {
        let pixels = &stage.paint(t).pixels;
        let mut last = [0_u8; 4];
        for pixel in pixels.chunks_exact(4) {
            if pixel != last {
                unique.insert([pixel[0], pixel[1], pixel[2]]);
                last.copy_from_slice(pixel);
            }
        }
        stacked.extend_from_slice(pixels);
    }
    if unique.len() <= COLORS {
        let mut colors = unique.into_iter().collect::<Vec<_>>();
        // Sorting keeps output reproducible.
        colors.sort_unstable();
        return colors.concat();
    }
    let frame = gif::Frame::from_rgba_speed(
        WIDTH as u16,
        (HEIGHT * SAMPLES.len()) as u16,
        &mut stacked,
        10,
    );
    let mut palette = frame.palette.unwrap_or_default();
    // Free one slot for transparency by retiring the least-used color.
    if palette.len() / 3 > COLORS {
        let mut uses = vec![0_usize; palette.len() / 3];
        for index in ColorMapper::new(&palette).map(&stacked[..WIDTH * HEIGHT * 4]) {
            uses[index as usize] += 1;
        }
        let rarest = (0..uses.len())
            .min_by_key(|index| uses[*index])
            .unwrap_or(0);
        palette.drain(rarest * 3..rarest * 3 + 3);
        palette.truncate(COLORS * 3);
    }
    palette
}

struct ColorMapper<'a> {
    palette: &'a [u8],
    known: HashMap<[u8; 3], u8>,
}

impl<'a> ColorMapper<'a> {
    fn new(palette: &'a [u8]) -> Self {
        Self {
            palette,
            known: HashMap::new(),
        }
    }

    fn map(&mut self, pixels: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(pixels.len() / 4);
        // Flat cards are long runs of one color; remembering the last pixel
        // skips almost every lookup.
        let mut last = [pixels[0], pixels[1], pixels[2]];
        let mut last_index = self.lookup(last);
        for pixel in pixels.chunks_exact(4) {
            let color = [pixel[0], pixel[1], pixel[2]];
            if color != last {
                last = color;
                last_index = self.lookup(color);
            }
            output.push(last_index);
        }
        output
    }

    fn lookup(&mut self, color: [u8; 3]) -> u8 {
        let palette = self.palette;
        *self.known.entry(color).or_insert_with(|| {
            palette
                .chunks_exact(3)
                .enumerate()
                .min_by_key(|(_, entry)| {
                    (0..3)
                        .map(|channel| {
                            (i32::from(entry[channel]) - i32::from(color[channel])).pow(2)
                        })
                        .sum::<i32>()
                })
                .map_or(0, |(index, _)| index as u8)
        })
    }
}

/// `(left, top, right, bottom)` around every differing pixel.
fn changed_region(before: &[u8], after: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let row = |y: usize| y * WIDTH..(y + 1) * WIDTH;
    let top = (0..HEIGHT).find(|y| before[row(*y)] != after[row(*y)])?;
    let bottom = (top..HEIGHT)
        .rev()
        .find(|y| before[row(*y)] != after[row(*y)])?
        + 1;
    let (mut left, mut right) = (WIDTH, 0);
    for y in top..bottom {
        let range = row(y);
        let (old, new) = (&before[range.clone()], &after[range]);
        if let Some(first) = (0..WIDTH).find(|x| old[*x] != new[*x]) {
            left = left.min(first);
            let last = (first..WIDTH)
                .rev()
                .find(|x| old[*x] != new[*x])
                .unwrap_or(first);
            right = right.max(last + 1);
        }
    }
    Some((left, top, right, bottom))
}

fn sub_frame(
    before: Option<&[u8]>,
    indexed: &[u8],
    region: (usize, usize, usize, usize),
    transparent: u8,
) -> gif::Frame<'static> {
    let (left, top, right, bottom) = region;
    let mut buffer = Vec::with_capacity((right - left) * (bottom - top));
    for y in top..bottom {
        let range = y * WIDTH + left..y * WIDTH + right;
        match before {
            Some(before) => buffer.extend(
                before[range.clone()]
                    .iter()
                    .zip(&indexed[range])
                    .map(|(old, new)| if old == new { transparent } else { *new }),
            ),
            None => buffer.extend_from_slice(&indexed[range]),
        }
    }
    gif::Frame {
        left: left as u16,
        top: top as u16,
        width: (right - left) as u16,
        height: (bottom - top) as u16,
        delay: FRAME_DELAY,
        dispose: gif::DisposalMethod::Keep,
        transparent: before.map(|_| transparent),
        buffer: Cow::Owned(buffer),
        ..gif::Frame::default()
    }
}
