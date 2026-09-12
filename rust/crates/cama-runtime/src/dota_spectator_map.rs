//! Offline spectator minimap rendering. Run on a blocking thread.
//!
//! The bundled 7.40 map and projection provenance are documented in
//! `assets/dota_spectator/README.md`. No image or network request is made here.

use std::path::Path;
use std::sync::OnceLock;

use cama_app::font_assets::{DejaVuFace, load_dejavu_font};
use cama_app::hero_lookup::{HeroImageSize, HeroLookup, hero_name, hero_short_name};
use cama_app::pet_assets::{RasterImage, Rgba, decode_png_raster};
use cama_app::trivia_image_cache::{TriviaImageCache, production_steam_image_cache_root};
use fontdue::{Font, FontSettings};

use crate::dota_live::{LiveMapFrame, LiveMapHero};

const MAP_SIZE: i32 = 640;
const MAP_X: i32 = 16;
const MAP_Y: i32 = 68;
const WIDTH: usize = 920;
const HEIGHT: usize = 758;
const BG: Rgba = Rgba(15, 21, 29, 255);
const PANEL: Rgba = Rgba(23, 32, 42, 255);
const WHITE: Rgba = Rgba(235, 241, 245, 255);
const MUTED: Rgba = Rgba(158, 174, 187, 255);
const RADIANT: Rgba = Rgba(92, 231, 165, 255);
const DIRE: Rgba = Rgba(255, 111, 126, 255);
const MAP_BYTES: &[u8] = include_bytes!("../../../../assets/dota_spectator/map-7.40.png");

/// Render source-backed hero locations and objective status into one PNG.
/// Missing fields stay unknown; wards, runes, and Roshan position are never
/// inferred from game time. Dead heroes appear in the roster, not on the map.
pub fn render_map(frame: &LiveMapFrame) -> Result<Vec<u8>, String> {
    render_with_cache(frame, &production_steam_image_cache_root())
}

fn render_with_cache(frame: &LiveMapFrame, cache: &Path) -> Result<Vec<u8>, String> {
    static BACKGROUND: OnceLock<Option<RasterImage>> = OnceLock::new();
    let map = BACKGROUND
        .get_or_init(|| decode_png_raster(MAP_BYTES))
        .as_ref()
        .ok_or_else(|| "bundled spectator map could not be decoded".to_owned())?;
    let mut canvas = Canvas(RasterImage::new(WIDTH, HEIGHT, BG));
    canvas.text(16, 17, "SPECTATOR MAP", 25.0, WHITE);
    canvas.text(285, 22, &format!("MATCH {}", frame.match_id), 15.0, MUTED);
    canvas.text(797, 15, &clock(frame.game_time), 28.0, WHITE);
    canvas.blit(map, MAP_X, MAP_Y, MAP_SIZE, MAP_SIZE, false);
    canvas.rect(672, MAP_Y, 232, MAP_SIZE, PANEL);
    canvas.text(16, 43, "15-second snapshots", 13.0, MUTED);
    canvas.text(678, 46, "GREEN Radiant / RED Dire", 13.0, MUTED);

    for building in &frame.buildings {
        if let Some((x, y)) = building.x.zip(building.y).and_then(|(x, y)| project(x, y)) {
            let color = if building.destroyed {
                MUTED
            } else {
                team_color(building.radiant)
            };
            canvas.rect(x - 5, y - 5, 11, 11, BG);
            canvas.rect(x - 3, y - 3, 7, 7, color);
            if building.destroyed {
                canvas.line(x - 5, y - 5, x + 5, y + 5, DIRE);
                canvas.line(x - 5, y + 5, x + 5, y - 5, DIRE);
            }
        }
    }

    let portraits = frame
        .heroes
        .iter()
        .take(10)
        .map(|hero| (hero.hero_id, cached_portrait(cache, hero.hero_id)))
        .collect::<Vec<_>>();
    for hero in frame.heroes.iter().take(10) {
        if hero.respawn_seconds.is_some_and(|seconds| seconds > 0) {
            continue;
        }
        let Some((x, y)) = hero.x.zip(hero.y).and_then(|(x, y)| project(x, y)) else {
            continue;
        };
        let portrait = portraits
            .iter()
            .find(|(id, _)| *id == hero.hero_id)
            .and_then(|(_, image)| image.as_ref());
        canvas.hero_icon(hero, portrait, x, y, 17, false);
    }

    for (side, radiant) in [true, false].into_iter().enumerate() {
        let top = 82 + side as i32 * 191;
        canvas.text(
            686,
            top,
            if radiant { "RADIANT" } else { "DIRE" },
            17.0,
            team_color(radiant),
        );
        for (index, hero) in frame
            .heroes
            .iter()
            .filter(|hero| hero.radiant == radiant)
            .take(5)
            .enumerate()
        {
            let y = top + 39 + index as i32 * 29;
            let portrait = portraits
                .iter()
                .find(|(id, _)| *id == hero.hero_id)
                .and_then(|(_, image)| image.as_ref());
            let dead = hero.respawn_seconds.is_some_and(|seconds| seconds > 0);
            canvas.hero_icon(hero, portrait, 697, y, 11, dead);
            canvas.text(
                715,
                y - 10,
                &truncate(&hero_name(i64::from(hero.hero_id)), 21),
                13.0,
                if dead { MUTED } else { WHITE },
            );
            if let Some(seconds) = hero.respawn_seconds.filter(|seconds| *seconds > 0) {
                canvas.text(715, y + 3, &format!("respawn {seconds}s"), 10.0, DIRE);
            } else if let Some(label) = position_status(hero) {
                canvas.text(715, y + 3, label, 10.0, MUTED);
            }
        }
    }

    canvas.text(686, 466, "OBJECTIVES", 17.0, WHITE);
    // League masks contain reliable identity/status but no coordinates.
    // Keep those in a lane/tier ledger instead of guessing map positions.
    canvas.text(778, 493, "RAD", 12.0, RADIANT);
    canvas.text(842, 493, "DIRE", 12.0, DIRE);
    for (row, lane) in ["top", "mid", "bottom"].into_iter().enumerate() {
        let y = 514 + row as i32 * 29;
        canvas.text(686, y, &lane.to_uppercase(), 12.0, MUTED);
        for (side, radiant) in [true, false].into_iter().enumerate() {
            for tier in 1..=3 {
                let expected = format!("{lane} tier {tier} tower");
                let state = frame
                    .buildings
                    .iter()
                    .find(|b| b.radiant == radiant && b.name.eq_ignore_ascii_case(&expected))
                    .map(|b| b.destroyed);
                let x = 779 + side as i32 * 64 + (tier - 1) * 17;
                canvas.status(x, y, &tier.to_string(), state, team_color(radiant));
            }
            for (index, (kind, label)) in [("melee", "M"), ("ranged", "R")].into_iter().enumerate()
            {
                let expected = format!("{lane} {kind} barracks");
                let state = frame
                    .buildings
                    .iter()
                    .find(|b| b.radiant == radiant && b.name.eq_ignore_ascii_case(&expected))
                    .map(|b| b.destroyed);
                canvas.status(
                    779 + side as i32 * 64 + index as i32 * 17,
                    y + 13,
                    label,
                    state,
                    team_color(radiant),
                );
            }
        }
    }
    canvas.text(686, 643, "M/R = melee/ranged barracks", 10.0, MUTED);
    for (row, radiant) in [true, false].into_iter().enumerate() {
        let y = 609 + row as i32 * 19;
        let towers = frame
            .buildings
            .iter()
            .filter(|b| b.radiant == radiant && b.name.contains("tower"))
            .collect::<Vec<_>>();
        let racks = frame
            .buildings
            .iter()
            .filter(|b| b.radiant == radiant && b.name.contains("barracks"))
            .collect::<Vec<_>>();
        let count = |items: &[&crate::dota_live::LiveMapBuilding]| {
            if items.is_empty() {
                "?".to_owned()
            } else {
                format!(
                    "{}/{}",
                    items.iter().filter(|b| !b.destroyed).count(),
                    items.len()
                )
            }
        };
        canvas.text(
            686,
            y,
            &format!(
                "{} towers {}  racks {}",
                if radiant { "R" } else { "D" },
                count(&towers),
                count(&racks)
            ),
            12.0,
            team_color(radiant),
        );
    }
    let roshan = match frame.roshan_respawn_seconds {
        Some(seconds) if seconds > 0 => format!("Roshan respawn {}", clock(seconds)),
        Some(0) => "Roshan timer 0s".to_owned(),
        _ => "Roshan status unavailable".to_owned(),
    };
    canvas.text(686, 659, &roshan, 12.0, MUTED);
    canvas.text(686, 685, "Crossed = down / ? = unknown", 11.0, MUTED);
    canvas.text(
        16,
        722,
        "Wards and vision are unavailable in this feed.",
        13.0,
        MUTED,
    );
    canvas.text(
        16,
        741,
        "7.40 static terrain: Valve / OpenDota. Objective icons in terrain are not live status.",
        11.0,
        MUTED,
    );
    Ok(canvas.0.encode_png())
}

/// OpenDota's matching map transform: world units become 128-unit cells,
/// offset by 128, then gameCoordToUV subtracts 64 and flips Y around 127.
/// The resulting image bounds are -8192..8064 on each axis. Coordinates
/// outside the asset crop are omitted, never clamped to a false location.
fn project(x: f64, y: f64) -> Option<(i32, i32)> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let u = (x / 128.0 + 64.0) / 127.0;
    let v = (63.0 - y / 128.0) / 127.0;
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    Some((
        MAP_X + (u * f64::from(MAP_SIZE - 1)).round() as i32,
        MAP_Y + (v * f64::from(MAP_SIZE - 1)).round() as i32,
    ))
}

fn position_status(hero: &LiveMapHero) -> Option<&'static str> {
    match hero.x.zip(hero.y) {
        Some((x, y)) if x.is_finite() && y.is_finite() => {
            project(x, y).is_none().then_some("outside terrain crop")
        }
        _ => Some("position unavailable"),
    }
}

fn cached_portrait(cache: &Path, hero_id: u32) -> Option<RasterImage> {
    let mut paths = Vec::new();
    static LOOKUP: OnceLock<HeroLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(HeroLookup::production);
    if let Ok(trivia) = TriviaImageCache::new(cache.join("trivia")) {
        for size in [HeroImageSize::Icon, HeroImageSize::Full] {
            if let Some(url) = lookup.image_url(i64::from(hero_id), size) {
                paths.push(trivia.path_for_url(&url));
            }
        }
    }
    paths.push(cache.join("scout/heroes").join(format!("{hero_id}.png")));
    paths.into_iter().find_map(|path| {
        let metadata = std::fs::metadata(&path).ok()?;
        if metadata.len() > 4 * 1024 * 1024 {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        // Refuse decompression bombs before handing data to the shared decoder.
        if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return None;
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        if width == 0 || height == 0 || width > 1024 || height > 1024 {
            return None;
        }
        decode_png_raster(&bytes)
    })
}

fn team_color(radiant: bool) -> Rgba {
    if radiant { RADIANT } else { DIRE }
}
fn truncate(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}
fn clock(seconds: i64) -> String {
    let value = seconds.unsigned_abs();
    format!(
        "{}{}:{:02}",
        if seconds < 0 { "-" } else { "" },
        value / 60,
        value % 60
    )
}

struct Canvas(RasterImage);
impl Canvas {
    fn pixel(&mut self, x: i32, y: i32, color: Rgba) {
        if x < 0 || y < 0 || x as usize >= self.0.width || y as usize >= self.0.height {
            return;
        }
        let offset = (y as usize * self.0.width + x as usize) * 4;
        let alpha = u32::from(color.3);
        for (channel, value) in [color.0, color.1, color.2].into_iter().enumerate() {
            self.0.pixels[offset + channel] = ((u32::from(value) * alpha
                + u32::from(self.0.pixels[offset + channel]) * (255 - alpha))
                / 255) as u8;
        }
        self.0.pixels[offset + 3] = 255;
    }
    fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Rgba) {
        for row in y..y + height {
            for col in x..x + width {
                self.pixel(col, row, color);
            }
        }
    }
    fn line(&mut self, x: i32, y: i32, end_x: i32, end_y: i32, color: Rgba) {
        let steps = (end_x - x).abs().max((end_y - y).abs()).max(1);
        for step in 0..=steps {
            self.pixel(
                x + (end_x - x) * step / steps,
                y + (end_y - y) * step / steps,
                color,
            );
        }
    }
    fn circle(&mut self, x: i32, y: i32, radius: i32, color: Rgba) {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= radius * radius {
                    self.pixel(x + dx, y + dy, color);
                }
            }
        }
    }
    fn blit(&mut self, image: &RasterImage, x: i32, y: i32, width: i32, height: i32, circle: bool) {
        for dy in 0..height {
            for dx in 0..width {
                if circle && (dx - width / 2).pow(2) + (dy - height / 2).pow(2) > (width / 2).pow(2)
                {
                    continue;
                }
                let (source_x, source_y) = if circle {
                    let crop = image.width.min(image.height);
                    (
                        (image.width - crop) / 2 + dx as usize * crop / width as usize,
                        (image.height - crop) / 2 + dy as usize * crop / height as usize,
                    )
                } else {
                    (
                        dx as usize * image.width / width as usize,
                        dy as usize * image.height / height as usize,
                    )
                };
                if let Some(pixel) = image.pixel(source_x, source_y) {
                    self.pixel(x + dx, y + dy, pixel);
                }
            }
        }
    }
    fn hero_icon(
        &mut self,
        hero: &LiveMapHero,
        portrait: Option<&RasterImage>,
        x: i32,
        y: i32,
        radius: i32,
        dead: bool,
    ) {
        self.circle(x, y, radius + 3, BG);
        self.circle(
            x,
            y,
            radius + 1,
            if dead {
                MUTED
            } else {
                team_color(hero.radiant)
            },
        );
        self.circle(x, y, radius - 1, PANEL);
        if let Some(image) = portrait {
            self.blit(
                image,
                x - radius + 1,
                y - radius + 1,
                (radius - 1) * 2,
                (radius - 1) * 2,
                true,
            );
        } else {
            let short = hero_short_name(i64::from(hero.hero_id))
                .chars()
                .take(3)
                .collect::<String>();
            self.text(
                x - radius + 3,
                y - 5,
                &short,
                if radius > 12 { 11.0 } else { 8.0 },
                WHITE,
            );
        }
        if dead {
            self.line(x - radius, y - radius, x + radius, y + radius, DIRE);
        }
    }
    fn status(&mut self, x: i32, y: i32, label: &str, down: Option<bool>, color: Rgba) {
        self.text(
            x,
            y,
            if down.is_none() { "?" } else { label },
            12.0,
            if down == Some(false) { color } else { MUTED },
        );
        if down == Some(true) {
            self.line(x - 1, y + 11, x + 9, y, DIRE);
        }
    }
    fn text(&mut self, x: i32, y: i32, text: &str, size: f32, color: Rgba) {
        static FONT: OnceLock<Option<Font>> = OnceLock::new();
        let font = FONT.get_or_init(|| {
            load_dejavu_font(DejaVuFace::Sans)
                .ok()
                .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
        });
        if let Some(font) = font {
            let mut cursor = x as f32;
            for character in text.chars() {
                let (metrics, bitmap) = font.rasterize(character, size);
                let top = y + size.ceil() as i32 - metrics.height as i32 - metrics.ymin;
                for dy in 0..metrics.height {
                    for dx in 0..metrics.width {
                        self.pixel(
                            cursor as i32 + metrics.xmin + dx as i32,
                            top + dy as i32,
                            Rgba(color.0, color.1, color.2, bitmap[dy * metrics.width + dx]),
                        );
                    }
                }
                cursor += metrics.advance_width;
            }
        } else {
            // Deterministic readable fallback when the fixed font is absent.
            let scale = if size >= 19.0 { 2 } else { 1 };
            for (index, character) in text.to_ascii_uppercase().chars().enumerate() {
                for (row, bits) in glyph(character).into_iter().enumerate() {
                    for col in 0..5 {
                        if bits & (1 << (4 - col)) != 0 {
                            self.rect(
                                x + index as i32 * 6 * scale + col * scale,
                                y + row as i32 * scale,
                                scale,
                                scale,
                                color,
                            );
                        }
                    }
                }
            }
        }
    }
}

fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        ':' => [0, 4, 4, 0, 4, 4, 0],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '.' => [0, 0, 0, 0, 0, 4, 4],
        ' ' => [0; 7],
        _ => [0, 0, 0, 0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dota_live::LiveMapBuilding;

    fn frame() -> LiveMapFrame {
        LiveMapFrame {
            match_id: 42,
            game_time: 91,
            heroes: vec![LiveMapHero {
                hero_id: 1,
                radiant: true,
                x: Some(0.0),
                y: Some(0.0),
                respawn_seconds: None,
            }],
            buildings: vec![],
            roshan_respawn_seconds: None,
        }
    }

    #[test]
    fn projection_matches_opendota_crop_and_axis_direction() {
        assert_eq!(project(-8192.0, 8064.0), Some((MAP_X, MAP_Y)));
        assert_eq!(project(8064.0, -8192.0), Some((MAP_X + 639, MAP_Y + 639)));
        let (x, y) = project(0.0, 0.0).unwrap();
        assert_eq!((x, y), (338, 385));
        assert!(project(1.0, 1.0).unwrap().0 >= x);
        assert!(project(f64::NAN, 0.0).is_none());
        assert!(project(0.0, f64::INFINITY).is_none());
        assert!(project(9000.0, 0.0).is_none());
    }
    #[test]
    fn png_renders_without_cache_or_live_services() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = render_with_cache(&frame(), cache.path()).unwrap();
        let image = decode_png_raster(&bytes).unwrap();
        assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
        assert!(bytes.len() < 3 * 1024 * 1024);
    }
    #[test]
    fn dead_heroes_do_not_leave_false_map_positions() {
        let cache = tempfile::tempdir().unwrap();
        let mut dead = frame();
        dead.heroes[0].respawn_seconds = Some(20);
        let mut absent = dead.clone();
        absent.heroes.clear();
        let a = decode_png_raster(&render_with_cache(&dead, cache.path()).unwrap()).unwrap();
        let b = decode_png_raster(&render_with_cache(&absent, cache.path()).unwrap()).unwrap();
        for y in MAP_Y..MAP_Y + MAP_SIZE {
            for x in MAP_X..MAP_X + MAP_SIZE {
                assert_eq!(
                    a.pixel(x as usize, y as usize),
                    b.pixel(x as usize, y as usize)
                );
            }
        }
    }
    #[test]
    fn partial_coordinates_keep_roster_without_painting_a_false_position() {
        let cache = tempfile::tempdir().unwrap();
        let mut missing = frame();
        missing.heroes[0].x = None;
        assert_eq!(
            position_status(&missing.heroes[0]),
            Some("position unavailable")
        );
        let mut outside = missing.clone();
        outside.heroes[0].x = Some(9000.0);
        assert_eq!(
            position_status(&outside.heroes[0]),
            Some("outside terrain crop")
        );
        let mut absent = missing.clone();
        absent.heroes.clear();
        let image = decode_png_raster(&render_with_cache(&missing, cache.path()).unwrap()).unwrap();
        let blank = decode_png_raster(&render_with_cache(&absent, cache.path()).unwrap()).unwrap();
        assert_ne!(image, blank, "known hero remains in the roster");
        for y in MAP_Y..MAP_Y + MAP_SIZE {
            for x in MAP_X..MAP_X + MAP_SIZE {
                assert_eq!(
                    image.pixel(x as usize, y as usize),
                    blank.pixel(x as usize, y as usize)
                );
            }
        }
    }

    #[test]
    fn known_building_coordinates_change_map_but_masks_only_change_ledger() {
        let cache = tempfile::tempdir().unwrap();
        let base = frame();
        let mut masks = base.clone();
        masks.buildings.push(LiveMapBuilding {
            radiant: true,
            name: "top tier 2 tower".into(),
            destroyed: true,
            x: None,
            y: None,
        });
        let a = decode_png_raster(&render_with_cache(&base, cache.path()).unwrap()).unwrap();
        let b = decode_png_raster(&render_with_cache(&masks, cache.path()).unwrap()).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.pixel(100, 100), b.pixel(100, 100));
        masks.buildings[0].x = Some(-5000.0);
        masks.buildings[0].y = Some(5000.0);
        let c = decode_png_raster(&render_with_cache(&masks, cache.path()).unwrap()).unwrap();
        let (x, y) = project(-5000.0, 5000.0).unwrap();
        assert_ne!(
            b.pixel(x as usize, y as usize),
            c.pixel(x as usize, y as usize)
        );
    }
    #[test]
    fn cached_portraits_are_used_and_invalid_sizes_are_rejected() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("scout/heroes");
        std::fs::create_dir_all(&directory).unwrap();
        let portrait = RasterImage::new(32, 32, Rgba(250, 10, 20, 255));
        std::fs::write(directory.join("1.png"), portrait.encode_png()).unwrap();
        assert_eq!(cached_portrait(cache.path(), 1), Some(portrait));
        let mut malformed = vec![0; 24];
        malformed[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        malformed[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        std::fs::write(directory.join("2.png"), malformed).unwrap();
        assert!(cached_portrait(cache.path(), 2).is_none());
    }
}
