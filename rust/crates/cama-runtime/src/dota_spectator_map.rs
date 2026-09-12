//! Offline spectator minimap rendering. Run on a blocking thread.
//!
//! The bundled 7.40 map and projection provenance are documented in
//! `assets/dota_spectator/README.md`. No image or network request is made here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cama_app::dotabase_sqlite::DotabaseSqliteSource;
use cama_app::font_assets::{DejaVuFace, load_dejavu_font};
use cama_app::hero_lookup::{HeroImageSize, HeroLookup, hero_name, hero_short_name};
use cama_app::pet_assets::{RasterImage, Rgba, decode_png_raster};
use cama_app::trivia_data::{TriviaDataSource, item_icon_url};
use cama_app::trivia_image_cache::{TriviaImageCache, production_steam_image_cache_root};
use fontdue::{Font, FontSettings};

use crate::dota_live::{LiveMapFrame, LiveMapHero};

const MAP_SIZE: i32 = 640;
const MAP_X: i32 = 16;
#[path = "dota_spectator_map/buildings.rs"]
mod buildings;

const MAP_Y: i32 = 48;
const WIDTH: usize = 1240;
const HEIGHT: usize = 704;
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
    canvas.text(16, 14, &format!("MATCH {}", frame.match_id), 14.0, MUTED);
    canvas.day_night(532, 23, is_day(frame.game_time));
    canvas.text(561, 6, &clock(frame.game_time), 28.0, WHITE);
    let (text, color) = gold_lead_label(frame.radiant_net_worth, frame.dire_net_worth);
    canvas.text(686, 17, &text, 15.0, color);
    canvas.blit(map, MAP_X, MAP_Y, MAP_SIZE, MAP_SIZE, false);
    canvas.rect(672, MAP_Y, 552, MAP_SIZE, PANEL);

    // The league feed has no Ancient health/status bit. Keep each Ancient as
    // a static base landmark, but honor an explicit destruction record when
    // another qualified source supplies one. Never imply live health here.
    for radiant in [true, false] {
        let ancient = frame
            .buildings
            .iter()
            .find(|building| building.radiant == radiant && building.name == "Ancient");
        if ancient.is_some_and(|building| building.destroyed) {
            continue;
        }
        let position = ancient
            .and_then(|building| building.x.zip(building.y))
            .or_else(|| buildings::position("Ancient", radiant));
        if let Some((x, y)) = position.and_then(|(x, y)| project(x, y)) {
            canvas.building_icon(x, y, "Ancient", team_color(radiant));
        }
    }

    for building in frame
        .buildings
        .iter()
        .filter(|building| !building.destroyed && building.name != "Ancient")
    {
        let location = building
            .x
            .zip(building.y)
            .or_else(|| buildings::position(&building.name, building.radiant));
        if let Some((x, y)) = location.and_then(|(x, y)| project(x, y)) {
            canvas.building_icon(x, y, &building.name, team_color(building.radiant));
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

    let items = frame
        .heroes
        .iter()
        .take(10)
        .flat_map(|hero| hero.items.iter().take(6).flatten().copied())
        .map(|id| (id, cached_item(cache, id)))
        .collect::<BTreeMap<_, _>>();
    for (side, radiant) in [true, false].into_iter().enumerate() {
        let top = 58 + side as i32 * 320;
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
            let y = top + 28 + index as i32 * 56;
            let portrait = portraits
                .iter()
                .find(|(id, _)| *id == hero.hero_id)
                .and_then(|(_, image)| image.as_ref());
            let dead = hero.respawn_seconds.is_some_and(|seconds| seconds > 0);
            canvas.hero_icon(hero, portrait, 700, y + 20, 17, dead);
            let name = hero
                .player_name
                .as_deref()
                .map(player_label)
                .filter(|name| !name.is_empty());
            if let Some(name) = name {
                canvas.fitted_text(725, y, &name, 204.0, 13.0, if dead { MUTED } else { WHITE });
                canvas.text(
                    725,
                    y + 16,
                    &truncate(&hero_name(i64::from(hero.hero_id)), 24),
                    11.0,
                    MUTED,
                );
                canvas.text(725, y + 33, &hero_stats(hero), 11.0, MUTED);
            } else {
                canvas.text(
                    725,
                    y,
                    &truncate(&hero_name(i64::from(hero.hero_id)), 24),
                    14.0,
                    if dead { MUTED } else { WHITE },
                );
                canvas.text(725, y + 20, &hero_stats(hero), 11.0, MUTED);
            }
            let (ultimate, color) = ultimate_status(hero);
            canvas.text(680, y + 40, &ultimate, 9.0, color);
            if let Some(seconds) = hero.respawn_seconds.filter(|seconds| *seconds > 0) {
                canvas.rect(684, y + 21, 33, 14, BG);
                canvas.text(687, y + 21, &format!("{seconds}s"), 10.0, DIRE);
            }
            for slot in 0..6 {
                let x = 934 + slot as i32 * 46;
                canvas.rect(x, y + 7, 43, 33, BG);
                match hero.items.get(slot) {
                    Some(Some(id)) => {
                        if let Some(image) = items.get(id).and_then(Option::as_ref) {
                            canvas.blit(image, x + 1, y + 8, 41, 31, false);
                        } else {
                            canvas.text(x + 17, y + 17, "?", 10.0, MUTED);
                        }
                    }
                    Some(None) => {}
                    None => canvas.text(x + 18, y + 15, "?", 12.0, MUTED),
                }
            }
            canvas.text(935, y + 42, &net_worth_label(hero.net_worth), 10.0, MUTED);
        }
    }
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

fn player_label(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'' | '.'))
        .take(24)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn is_day(game_time: i64) -> bool {
    game_time.max(0) % 600 < 300
}

fn ultimate_status(hero: &LiveMapHero) -> (String, Rgba) {
    if let Some(seconds) = hero.ultimate_cooldown.filter(|seconds| *seconds > 0) {
        return (format!("R {seconds}s"), MUTED);
    }
    // Valve DOTAUltimateState in dota_gcmessages_server.proto:
    // https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_server.proto
    match hero.ultimate_state {
        Some(0) => ("R -".into(), MUTED),
        Some(1) => ("R CD".into(), MUTED),
        Some(2) => ("R MP".into(), Rgba(115, 174, 250, 255)),
        Some(3) => ("R UP".into(), RADIANT),
        _ => ("R ?".into(), MUTED),
    }
}

fn compact_number(value: i64) -> String {
    if value >= 1000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

fn net_worth_label(value: Option<i64>) -> String {
    let value = value
        .filter(|value| *value >= 0)
        .map(compact_number)
        .unwrap_or_else(|| "?".into());
    format!("NW {value}")
}

fn gold_lead_label(radiant: Option<i64>, dire: Option<i64>) -> (String, Rgba) {
    let Some((radiant, dire)) = radiant.zip(dire).filter(|(r, d)| *r >= 0 && *d >= 0) else {
        return ("GOLD  ?".into(), MUTED);
    };
    if radiant == dire {
        return ("GOLD  EVEN".into(), MUTED);
    }
    let difference = radiant.abs_diff(dire);
    let amount = compact_number(difference.min(i64::MAX as u64) as i64);
    let side = if radiant > dire { "RADIANT" } else { "DIRE" };
    (
        format!("GOLD  {side} +{amount}"),
        team_color(radiant > dire),
    )
}

fn hero_stats(hero: &LiveMapHero) -> String {
    let value = |stat: Option<i64>| {
        stat.map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_owned())
    };
    format!(
        "{}/{}/{}  LV {}  GPM {}",
        value(hero.kills),
        value(hero.deaths),
        value(hero.assists),
        value(hero.level),
        value(hero.gold_per_min)
    )
}

fn cached_item(cache: &Path, item_id: u32) -> Option<RasterImage> {
    static ITEM_URLS: OnceLock<BTreeMap<u32, String>> = OnceLock::new();
    let urls = ITEM_URLS.get_or_init(|| {
        DotabaseSqliteSource::production()
            .items()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| {
                Some((
                    u32::try_from(item.id).ok()?,
                    item_icon_url(item.icon_path.as_deref())?,
                ))
            })
            .collect()
    });
    let mut paths = Vec::new();
    if let Some(url) = urls.get(&item_id)
        && let Ok(trivia) = TriviaImageCache::new(cache.join("trivia"))
    {
        paths.push(trivia.path_for_url(url));
    }
    paths.push(cache.join("scout/items").join(format!("{item_id}.png")));
    cached_png(paths)
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
    cached_png(paths)
}

fn cached_png(paths: Vec<PathBuf>) -> Option<RasterImage> {
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
    fn day_night(&mut self, x: i32, y: i32, day: bool) {
        if day {
            let sun = Rgba(255, 209, 87, 255);
            self.circle(x, y, 6, sun);
            for (dx, dy) in [
                (0, -1),
                (1, -1),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
            ] {
                self.line(x + dx * 9, y + dy * 9, x + dx * 12, y + dy * 12, sun);
            }
        } else {
            self.circle(x, y, 10, Rgba(186, 206, 245, 255));
            self.circle(x + 5, y - 4, 9, BG);
        }
    }
    fn building_icon(&mut self, x: i32, y: i32, name: &str, color: Rgba) {
        // Draw only the silhouette. Spaces (including doors and windows)
        // preserve the terrain underneath instead of painting a black tile.
        let rows: &[&str] = if name == "Ancient" {
            &[
                "          +          ",
                "         ++#         ",
                "         ++#         ",
                "        +++##        ",
                "   +    +++##    +   ",
                "  ++#   +++##   ++#  ",
                "  ++#  ++++###  ++#  ",
                " +++## ++++### +++## ",
                " +++## ++++### +++## ",
                "  ++## ++++### ++##  ",
                "  ++##  +++##  ++##  ",
                "   ###  +++##  ###   ",
                "    ##   ++#   ##    ",
                "    ###  ++#  ###    ",
                "     ####+######     ",
                "   +++++++++++++++   ",
                "  #################  ",
                "   ###############   ",
            ]
        } else if name.contains("tower") {
            &[
                " +++  +++  +++ ",
                " ###  ###  ### ",
                " ###  ###  ### ",
                " ############# ",
                " +++++++++++++ ",
                "  ###########  ",
                "  ###########  ",
                "  ###########  ",
                "  ####   ####  ",
                "  ####   ####  ",
                "  ####   ####  ",
                "  ####   ####  ",
                "  ####   ####  ",
                "++++++   ++++++",
                "###############",
            ]
        } else if name.contains("ranged barracks") {
            &[
                "      +++++      ",
                "     +++####     ",
                "    +++######    ",
                "   +++########   ",
                "  +++##########  ",
                " +++++++++++++++ ",
                "  #############  ",
                "  ##  #####  ##  ",
                "  ##  #####  ##  ",
                "  ##  #####  ##  ",
                "  #############  ",
                "  #############  ",
            ]
        } else if name.contains("barracks") {
            &[
                "      +++++      ",
                "     +++####     ",
                "    +++######    ",
                "   +++########   ",
                "  +++##########  ",
                " +++++++++++++++ ",
                "  #############  ",
                "  #####   #####  ",
                "  #####   #####  ",
                "  #####   #####  ",
                "  #####   #####  ",
                "  #####   #####  ",
            ]
        } else {
            return;
        };
        let highlight = Rgba(
            color.0.saturating_add(45),
            color.1.saturating_add(45),
            color.2.saturating_add(45),
            255,
        );
        let left = x - rows[0].len() as i32 / 2;
        let top = y - rows.len() as i32 / 2;
        for (dy, row) in rows.iter().enumerate() {
            for (dx, pixel) in row.bytes().enumerate() {
                let fill = match pixel {
                    b'#' => color,
                    b'+' => highlight,
                    _ => continue,
                };
                self.pixel(left + dx as i32, top + dy as i32, fill);
            }
        }
    }
    fn fitted_text(&mut self, x: i32, y: i32, text: &str, width: f32, size: f32, color: Rgba) {
        let mut used = 0.0;
        let mut fitted = String::new();
        for character in text.chars() {
            let advance = font()
                .map(|font| font.metrics(character, size).advance_width)
                .unwrap_or(6.0);
            if used + advance > width {
                break;
            }
            used += advance;
            fitted.push(character);
        }
        self.text(x, y, &fitted, size, color);
    }
    fn text(&mut self, x: i32, y: i32, text: &str, size: f32, color: Rgba) {
        let font = font();
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

fn font() -> Option<&'static Font> {
    static FONT: OnceLock<Option<Font>> = OnceLock::new();
    FONT.get_or_init(|| {
        load_dejavu_font(DejaVuFace::Sans)
            .ok()
            .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
    })
    .as_ref()
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
            radiant_net_worth: None,
            dire_net_worth: None,
            heroes: vec![LiveMapHero {
                hero_id: 1,
                radiant: true,
                x: Some(0.0),
                y: Some(0.0),
                respawn_seconds: None,
                kills: None,
                deaths: None,
                assists: None,
                level: None,
                gold_per_min: None,
                net_worth: None,
                items: vec![],
                player_name: None,
                ultimate_state: None,
                ultimate_cooldown: None,
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
        assert_eq!((x, y), (338, 365));
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
        // The expanded roster is 1240x704; the shared encoder uses stored RGBA.
        assert!(bytes.len() < 4 * 1024 * 1024);
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
    fn standing_buildings_use_fixed_positions_and_destroyed_buildings_disappear() {
        let cache = tempfile::tempdir().unwrap();
        let base = frame();
        let blank = decode_png_raster(&render_with_cache(&base, cache.path()).unwrap()).unwrap();
        let mut state = base.clone();
        state.buildings.push(LiveMapBuilding {
            radiant: true,
            name: "top tier 2 tower".into(),
            destroyed: false,
            x: None,
            y: None,
        });
        let standing =
            decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        let (wx, wy) = buildings::position("top tier 2 tower", true).unwrap();
        let (x, y) = project(wx, wy).unwrap();
        assert_ne!(
            standing.pixel(x as usize, y as usize),
            blank.pixel(x as usize, y as usize)
        );
        state.buildings[0].destroyed = true;
        assert_eq!(
            decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap(),
            blank
        );
        state.buildings[0].destroyed = false;
        state.buildings[0].x = Some(-5000.0);
        state.buildings[0].y = Some(5000.0);
        let explicit =
            decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        let (x, y) = project(-5000.0, 5000.0).unwrap();
        assert_ne!(
            explicit.pixel(x as usize, y as usize),
            blank.pixel(x as usize, y as usize)
        );
    }
    #[test]
    fn gold_lead_labels_distinguish_sides_ties_and_missing_data() {
        assert_eq!(
            gold_lead_label(Some(50_400), Some(44_900)),
            ("GOLD  RADIANT +5.5k".into(), RADIANT)
        );
        assert_eq!(
            gold_lead_label(Some(2000), Some(2300)),
            ("GOLD  DIRE +300".into(), DIRE)
        );
        assert_eq!(net_worth_label(Some(13531)), "NW 13.5k");
        assert_eq!(net_worth_label(None), "NW ?");
        assert_eq!(gold_lead_label(Some(1000), Some(1000)).0, "GOLD  EVEN");
        assert_eq!(gold_lead_label(None, Some(2300)).0, "GOLD  ?");
        assert_eq!(gold_lead_label(Some(-1), Some(1)).0, "GOLD  ?");
    }

    #[test]
    fn building_silhouettes_preserve_terrain_in_background_and_openings() {
        let terrain = Rgba(80, 130, 90, 255);
        for name in [
            "mid tier 3 tower",
            "mid melee barracks",
            "mid ranged barracks",
            "Ancient",
        ] {
            let mut canvas = Canvas(RasterImage::new(48, 48, terrain));
            canvas.building_icon(24, 24, name, RADIANT);
            assert_eq!(canvas.0.pixel(14, 14), Some(terrain));
            assert!(
                canvas
                    .0
                    .pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel != [BG.0, BG.1, BG.2, BG.3])
            );
            assert!(
                canvas
                    .0
                    .pixels
                    .chunks_exact(4)
                    .any(|pixel| pixel == [RADIANT.0, RADIANT.1, RADIANT.2, 255])
            );
            if name.contains("tower") || name.contains("melee barracks") {
                assert_eq!(
                    canvas.0.pixel(24, 27),
                    Some(terrain),
                    "door opening preserves terrain"
                );
            }
        }
    }

    #[test]
    fn ancient_landmarks_honor_explicit_destruction_and_source_position() {
        let cache = tempfile::tempdir().unwrap();
        let mut state = frame();
        state.heroes.clear();
        let both = decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        let (wx, wy) = buildings::position("Ancient", true).unwrap();
        let (x, y) = project(wx, wy).unwrap();
        state.buildings.push(LiveMapBuilding {
            radiant: true,
            name: "Ancient".into(),
            destroyed: true,
            x: None,
            y: None,
        });
        let destroyed =
            decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        assert_ne!(
            both.pixel(x as usize, y as usize),
            destroyed.pixel(x as usize, y as usize)
        );
        state.buildings[0].destroyed = false;
        state.buildings[0].x = Some(0.0);
        state.buildings[0].y = Some(0.0);
        let moved = decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        assert_eq!(
            moved.pixel(x as usize, y as usize),
            destroyed.pixel(x as usize, y as usize)
        );
        let (x, y) = project(0.0, 0.0).unwrap();
        assert_ne!(
            moved.pixel(x as usize, y as usize),
            destroyed.pixel(x as usize, y as usize)
        );
    }

    #[test]
    fn cached_items_and_unknown_inventory_are_distinct_from_empty_slots() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("scout/items");
        std::fs::create_dir_all(&directory).unwrap();
        let item = RasterImage::new(32, 32, Rgba(20, 40, 250, 255));
        std::fs::write(directory.join("1.png"), item.encode_png()).unwrap();
        assert_eq!(cached_item(cache.path(), 1), Some(item));
        let mut state = frame();
        let unknown = render_with_cache(&state, cache.path()).unwrap();
        state.heroes[0].items = vec![None; 6];
        let empty = render_with_cache(&state, cache.path()).unwrap();
        assert_ne!(unknown, empty);
        state.heroes[0].items[0] = Some(1);
        let equipped =
            decode_png_raster(&render_with_cache(&state, cache.path()).unwrap()).unwrap();
        assert_eq!(equipped.pixel(940, 100), Some(Rgba(20, 40, 250, 255)));
        state.heroes[0].items[0] = Some(99999);
        assert_ne!(render_with_cache(&state, cache.path()).unwrap(), empty);
    }
    #[test]
    fn ultimate_requires_confirmed_ready_and_respects_cooldown() {
        let mut state = frame();
        let hero = &mut state.heroes[0];
        assert_eq!(ultimate_status(hero).0, "R ?");
        for (value, label) in [
            (0, "R -"),
            (1, "R CD"),
            (2, "R MP"),
            (3, "R UP"),
            (4, "R ?"),
        ] {
            hero.ultimate_state = Some(value);
            assert_eq!(ultimate_status(hero).0, label);
        }
        hero.ultimate_state = Some(3);
        hero.ultimate_cooldown = Some(42);
        assert_eq!(ultimate_status(hero).0, "R 42s");
        hero.ultimate_cooldown = Some(0);
        assert_eq!(ultimate_status(hero), ("R UP".into(), RADIANT));
    }
    #[test]
    fn clock_cycle_and_names_are_bounded() {
        assert!(is_day(-1));
        assert!(is_day(0));
        assert!(is_day(299));
        assert!(!is_day(300));
        assert!(!is_day(599));
        assert!(is_day(600));
        assert_eq!(player_label(" @everyone **jaso7** "), "everyone jaso7");
        assert_eq!(player_label(&"x".repeat(100)).len(), 24);
    }
    #[test]
    fn missing_stats_are_unknown_and_observed_zero_is_zero() {
        let mut state = frame();
        assert_eq!(hero_stats(&state.heroes[0]), "?/?/?  LV ?  GPM ?");
        let hero = &mut state.heroes[0];
        hero.kills = Some(0);
        hero.deaths = Some(1);
        hero.assists = Some(2);
        hero.level = Some(3);
        hero.gold_per_min = Some(234);
        assert_eq!(hero_stats(hero), "0/1/2  LV 3  GPM 234");
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
