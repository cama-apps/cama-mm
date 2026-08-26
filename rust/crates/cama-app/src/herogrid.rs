//! Discord-neutral Hero Grid source selection, command policy, and PNG renderer.
//!
//! The renderer intentionally has no image dependency.  It rasterizes an RGBA
//! image and emits a standards-compliant PNG with stored DEFLATE blocks, CRC-32
//! chunk checksums, and an Adler-32 zlib checksum.  Python/Pillow and the Python
//! hero catalog remain the pixel-reference authority during the parity phase;
//! this implementation pins the tested layout and policy without starting a
//! Discord client.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Cursor;

use cama_db::herogrid_repository::{
    EnrichedPlayer, HeroGridDataPort, HeroGridPlayer, HeroGridStat,
};

use crate::embeds::LobbyKind;
use crate::hero_lookup::hero_short_name;

pub const AMBIGUOUS_LOBBY_MESSAGE: &str = "❓ You're queued in both 🍽️ All You Can Feed and 🧀 Whine & Cheese — add the `lobby` option to pick one.";
pub const NO_LOBBY_SOURCE_MESSAGE: &str = "No active lobby, pending match, draft, or recent match found. Use `source: All Players` to show all players.";
pub const NO_ENRICHED_PLAYERS_MESSAGE: &str = "No players with enriched match data found.";
pub const NO_SELECTED_HERO_DATA_MESSAGE: &str =
    "No enriched hero data found for the selected players.";

const GRID_MAX_HEROES: usize = 60;
const GRID_CELL_SIZE: usize = 44;
const GRID_PLAYER_LABEL_WIDTH: usize = 120;
const GRID_HERO_LABEL_HEIGHT: usize = 90;
const GRID_PADDING: usize = 15;
const GRID_LEGEND_HEIGHT: usize = 80;
const GRID_TITLE_HEIGHT: usize = 30;
const GRID_MIN_CIRCLE_RADIUS: i32 = 4;
const GRID_MAX_CIRCLE_RADIUS: i32 = 18;
const GRID_LABEL_REPEAT_INTERVAL: usize = 10;
const GRID_MAX_WIDTH: usize = 3_900;
const MAX_RASTER_BYTES: usize = 512 * 1024 * 1024;

const DISCORD_BG: Rgba = Rgba::rgb(0x36, 0x39, 0x3f);
const DISCORD_DARKER: Rgba = Rgba::rgb(0x2f, 0x31, 0x36);
const DISCORD_GREEN: Rgba = Rgba::rgb(0x57, 0xf2, 0x87);
const DISCORD_LIGHT_GREEN: Rgba = Rgba::rgb(0x7c, 0xb3, 0x42);
const DISCORD_RED: Rgba = Rgba::rgb(0xed, 0x42, 0x45);
const DISCORD_YELLOW: Rgba = Rgba::rgb(0xfe, 0xe7, 0x5c);
const DISCORD_WHITE: Rgba = Rgba::rgb(0xff, 0xff, 0xff);
const DISCORD_GREY: Rgba = Rgba::rgb(0xb9, 0xbb, 0xbe);
const GRID_COLOR: Rgba = Rgba::rgb(0x2a, 0x2d, 0x33);
const SEPARATOR_COLOR: Rgba = Rgba::rgb(0x4e, 0x50, 0x58);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Teams {
    pub radiant: Vec<i64>,
    pub dire: Vec<i64>,
}

impl Teams {
    #[must_use]
    pub fn new(radiant: &[i64], dire: &[i64]) -> Self {
        Self {
            radiant: radiant.to_vec(),
            dire: dire.to_vec(),
        }
    }

    fn player_ids(&self) -> Vec<i64> {
        self.radiant
            .iter()
            .chain(self.dire.iter())
            .copied()
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.radiant.is_empty() && self.dire.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeroGridSource {
    Auto,
    Lobby,
    All,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HeroGridSources {
    pub open_lobby: Vec<i64>,
    pub low_skill_lobby: Vec<i64>,
    /// Retained so adapters can pass the legacy field explicitly.  Resolution
    /// never includes it, matching Python's `lobby.players`-only behavior.
    pub legacy_conditional_players: Vec<i64>,
    pub invoking_user_lobbies: Vec<LobbyKind>,
    pub invoking_pending_match: Option<Teams>,
    pub guild_pending_match: Option<Teams>,
    /// Python uses the per-player state-service lookup when that method exists,
    /// even when it returns `None`, instead of falling back to last-shuffle.
    pub invoking_pending_lookup_available: bool,
    pub draft_pool_ids: Option<Vec<i64>>,
    pub last_match_ids: Vec<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedPlayers {
    pub player_ids: Vec<i64>,
    pub source_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerResolutionError {
    AmbiguousLobby,
}

enum ResolutionStage {
    Resolved(ResolvedPlayers),
    NeedEnrichedPlayers,
}

fn resolve_before_enriched_fallback(
    source: HeroGridSource,
    sources: &HeroGridSources,
    invoking_user_id: Option<i64>,
    explicit_lobby: Option<LobbyKind>,
) -> Result<ResolutionStage, PlayerResolutionError> {
    if source == HeroGridSource::All {
        return Ok(ResolutionStage::NeedEnrichedPlayers);
    }

    let lobby_kind = if let Some(kind) = explicit_lobby {
        kind
    } else if invoking_user_id.is_some() {
        if sources.invoking_user_lobbies.len() > 1 {
            return Err(PlayerResolutionError::AmbiguousLobby);
        }
        sources
            .invoking_user_lobbies
            .first()
            .copied()
            .unwrap_or(LobbyKind::Open)
    } else {
        LobbyKind::Open
    };

    let lobby_players = match lobby_kind {
        LobbyKind::Open => &sources.open_lobby,
        LobbyKind::LowSkill => &sources.low_skill_lobby,
    };
    if !lobby_players.is_empty() {
        return Ok(ResolutionStage::Resolved(ResolvedPlayers {
            player_ids: lobby_players.clone(),
            source_label: Some(lobby_kind.label().to_owned()),
        }));
    }

    let pending_match = if invoking_user_id.is_some() && sources.invoking_pending_lookup_available {
        sources.invoking_pending_match.as_ref()
    } else {
        sources.guild_pending_match.as_ref()
    };
    if let Some(teams) = pending_match.filter(|teams| !teams.is_empty()) {
        return Ok(ResolutionStage::Resolved(ResolvedPlayers {
            player_ids: teams.player_ids(),
            source_label: Some("Active Match".to_owned()),
        }));
    }

    if let Some(player_ids) = sources
        .draft_pool_ids
        .as_ref()
        .filter(|player_ids| !player_ids.is_empty())
    {
        return Ok(ResolutionStage::Resolved(ResolvedPlayers {
            player_ids: player_ids.clone(),
            source_label: Some("Draft".to_owned()),
        }));
    }

    if !sources.last_match_ids.is_empty() {
        return Ok(ResolutionStage::Resolved(ResolvedPlayers {
            player_ids: sources.last_match_ids.clone(),
            source_label: Some("Last Match".to_owned()),
        }));
    }

    if source == HeroGridSource::Auto {
        Ok(ResolutionStage::NeedEnrichedPlayers)
    } else {
        Ok(ResolutionStage::Resolved(ResolvedPlayers::default()))
    }
}

/// Resolve the Python source-priority chain from an already-read snapshot.
pub fn resolve_player_ids(
    source: HeroGridSource,
    sources: &HeroGridSources,
    invoking_user_id: Option<i64>,
    explicit_lobby: Option<LobbyKind>,
    enriched_players: &[EnrichedPlayer],
) -> Result<ResolvedPlayers, PlayerResolutionError> {
    match resolve_before_enriched_fallback(source, sources, invoking_user_id, explicit_lobby)? {
        ResolutionStage::Resolved(resolved) => Ok(resolved),
        ResolutionStage::NeedEnrichedPlayers => Ok(ResolvedPlayers {
            player_ids: enriched_players
                .iter()
                .map(|player| player.discord_id)
                .collect(),
            source_label: None,
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rgba([u8; 4]);

impl Rgba {
    const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue, 0xff])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeroGridRenderError {
    DimensionOverflow,
    RasterTooLarge { width: usize, height: usize },
}

impl fmt::Display for HeroGridRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionOverflow => formatter.write_str("hero-grid dimensions overflowed"),
            Self::RasterTooLarge { width, height } => {
                write!(formatter, "hero-grid raster is too large: {width}x{height}")
            }
        }
    }
}

impl std::error::Error for HeroGridRenderError {}

#[derive(Clone, Debug)]
struct HeroGridLayout {
    hero_ids: Vec<i64>,
    num_players: usize,
    num_heroes: usize,
    extra_bands: usize,
    extra_columns: usize,
    width: usize,
    height: usize,
    grid_top: usize,
}

impl HeroGridLayout {
    fn player_row_y(&self, player_index: usize) -> usize {
        self.grid_top
            + player_index * GRID_CELL_SIZE
            + (player_index / GRID_LABEL_REPEAT_INTERVAL) * GRID_HERO_LABEL_HEIGHT
    }

    fn hero_column_x(&self, hero_index: usize) -> usize {
        GRID_PADDING
            + GRID_PLAYER_LABEL_WIDTH
            + hero_index * GRID_CELL_SIZE
            + (hero_index / GRID_LABEL_REPEAT_INTERVAL) * GRID_PLAYER_LABEL_WIDTH
    }
}

fn compute_layout(
    player_ids: &[i64],
    raw_hero_ids: &[i64],
) -> Result<HeroGridLayout, HeroGridRenderError> {
    let num_players = player_ids.len();
    let raw_extra_columns = if raw_hero_ids.len() > GRID_LABEL_REPEAT_INTERVAL {
        (raw_hero_ids.len() - 1) / GRID_LABEL_REPEAT_INTERVAL
    } else {
        0
    };
    let fixed_width = GRID_PADDING
        .checked_mul(2)
        .and_then(|value| value.checked_add(GRID_PLAYER_LABEL_WIDTH))
        .and_then(|value| {
            value.checked_add(raw_extra_columns.saturating_mul(GRID_PLAYER_LABEL_WIDTH))
        })
        .ok_or(HeroGridRenderError::DimensionOverflow)?;
    let max_heroes_by_width = GRID_MAX_WIDTH
        .saturating_sub(fixed_width)
        .checked_div(GRID_CELL_SIZE)
        .ok_or(HeroGridRenderError::DimensionOverflow)?;
    let num_heroes = raw_hero_ids
        .len()
        .min(GRID_MAX_HEROES)
        .min(max_heroes_by_width);
    let hero_ids = raw_hero_ids[..num_heroes].to_vec();
    let extra_columns = if num_heroes > GRID_LABEL_REPEAT_INTERVAL {
        (num_heroes - 1) / GRID_LABEL_REPEAT_INTERVAL
    } else {
        0
    };
    let extra_bands = if num_players > GRID_LABEL_REPEAT_INTERVAL {
        (num_players - 1) / GRID_LABEL_REPEAT_INTERVAL
    } else {
        0
    };

    let width = GRID_PADDING
        .checked_add(GRID_PLAYER_LABEL_WIDTH)
        .and_then(|value| value.checked_add(num_heroes.checked_mul(GRID_CELL_SIZE)?))
        .and_then(|value| value.checked_add(extra_columns.checked_mul(GRID_PLAYER_LABEL_WIDTH)?))
        .and_then(|value| value.checked_add(GRID_PADDING))
        .ok_or(HeroGridRenderError::DimensionOverflow)?;
    let height = GRID_PADDING
        .checked_add(GRID_TITLE_HEIGHT)
        .and_then(|value| value.checked_add(GRID_HERO_LABEL_HEIGHT))
        .and_then(|value| value.checked_add(num_players.checked_mul(GRID_CELL_SIZE)?))
        .and_then(|value| value.checked_add(extra_bands.checked_mul(GRID_HERO_LABEL_HEIGHT)?))
        .and_then(|value| value.checked_add(GRID_LEGEND_HEIGHT))
        .and_then(|value| value.checked_add(GRID_PADDING))
        .ok_or(HeroGridRenderError::DimensionOverflow)?;

    Ok(HeroGridLayout {
        hero_ids,
        num_players,
        num_heroes,
        extra_bands,
        extra_columns,
        width,
        height,
        grid_top: GRID_PADDING + GRID_TITLE_HEIGHT + GRID_HERO_LABEL_HEIGHT,
    })
}

#[derive(Clone, Debug)]
struct Raster {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Raster {
    fn new(width: usize, height: usize, color: Rgba) -> Result<Self, HeroGridRenderError> {
        let byte_count = width
            .checked_mul(height)
            .and_then(|value| value.checked_mul(4))
            .ok_or(HeroGridRenderError::DimensionOverflow)?;
        if byte_count > MAX_RASTER_BYTES {
            return Err(HeroGridRenderError::RasterTooLarge { width, height });
        }
        let mut pixels = vec![0_u8; byte_count];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) * 4;
        self.pixels[offset..offset + 4].copy_from_slice(&color.0);
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        let left = left.max(0) as usize;
        let top = top.max(0) as usize;
        let right = usize::try_from(right.max(0))
            .unwrap_or(usize::MAX)
            .min(self.width);
        let bottom = usize::try_from(bottom.max(0))
            .unwrap_or(usize::MAX)
            .min(self.height);
        for y in top.min(bottom)..bottom {
            for x in left.min(right)..right {
                let offset = (y * self.width + x) * 4;
                self.pixels[offset..offset + 4].copy_from_slice(&color.0);
            }
        }
    }

    fn horizontal_line(&mut self, left: i32, right: i32, y: i32, color: Rgba) {
        for x in left.min(right)..=left.max(right) {
            self.set_pixel(x, y, color);
        }
    }

    fn vertical_line(&mut self, x: i32, top: i32, bottom: i32, color: Rgba, width: i32) {
        for offset in 0..width.max(1) {
            for y in top.min(bottom)..=top.max(bottom) {
                self.set_pixel(x + offset, y, color);
            }
        }
    }

    fn circle(&mut self, center_x: i32, center_y: i32, radius: i32, color: Rgba) {
        let radius_squared = radius * radius;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= radius_squared {
                    self.set_pixel(center_x + dx, center_y + dy, color);
                }
            }
        }
    }

    fn draw_glyph(&mut self, x: i32, y: i32, character: char, color: Rgba, scale: i32) {
        let glyph = glyph(character);
        for (row, bits) in glyph.into_iter().enumerate() {
            for column in 0..5_u8 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let pixel_x = x + i32::from(column) * scale;
                let pixel_y = y + i32::try_from(row).unwrap_or(0) * scale;
                self.fill_rect(pixel_x, pixel_y, pixel_x + scale, pixel_y + scale, color);
            }
        }
    }

    fn draw_text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        let mut cursor_x = x;
        for character in text.chars() {
            self.draw_glyph(cursor_x, y, character, color, scale);
            cursor_x += 6 * scale;
        }
    }

    fn draw_vertical_text(&mut self, center_x: i32, bottom: i32, text: &str, color: Rgba) {
        let characters = text.chars().collect::<Vec<_>>();
        let height = i32::try_from(characters.len().saturating_mul(8)).unwrap_or(i32::MAX);
        let mut y = bottom - height - 2;
        for character in characters {
            self.draw_glyph(center_x - 2, y, character, color, 1);
            y += 8;
        }
    }
}

fn glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
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
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

/// Python truncates names longer than 14 code points to 12 plus two dots.
#[must_use]
pub fn truncate_player_name(name: &str) -> String {
    if name.chars().count() <= 14 {
        return name.to_owned();
    }
    format!("{}..", name.chars().take(12).collect::<String>())
}

fn win_rate_color(games: i64, wins: i64) -> Rgba {
    if games <= 0 {
        return DISCORD_RED;
    }
    let win_rate = wins as f64 / games as f64;
    if win_rate >= 0.60 {
        DISCORD_GREEN
    } else if win_rate >= 0.50 {
        DISCORD_LIGHT_GREEN
    } else if win_rate >= 0.40 {
        DISCORD_YELLOW
    } else {
        DISCORD_RED
    }
}

fn render_empty(message: &str) -> Result<Cursor<Vec<u8>>, HeroGridRenderError> {
    let mut raster = Raster::new(450, 100, DISCORD_BG)?;
    raster.draw_text(20, 40, message, DISCORD_GREY, 1);
    Ok(Cursor::new(encode_png(&raster)))
}

/// Render a deterministic, seekable RGBA PNG using Python's tested geometry.
pub fn draw_hero_grid(
    grid_data: &[HeroGridStat],
    player_names: &[HeroGridPlayer],
    min_games: i64,
    title: &str,
) -> Result<Cursor<Vec<u8>>, HeroGridRenderError> {
    if grid_data.is_empty() || player_names.is_empty() {
        return render_empty("No hero data available");
    }

    // Python dict assignment keeps the first insertion position and the last
    // value for duplicate `(player, hero)` keys.
    let mut pivot = Vec::<((i64, i64), (i64, i64))>::new();
    let mut pivot_indices = BTreeMap::<(i64, i64), usize>::new();
    for row in grid_data {
        let key = (row.discord_id, row.hero_id);
        if let Some(index) = pivot_indices.get(&key).copied() {
            pivot[index].1 = (row.games, row.wins);
        } else {
            pivot_indices.insert(key, pivot.len());
            pivot.push((key, (row.games, row.wins)));
        }
    }

    let mut seen_players = BTreeSet::new();
    let player_ids = player_names
        .iter()
        .map(|player| player.discord_id)
        .filter(|player_id| seen_players.insert(*player_id))
        .filter(|player_id| pivot.iter().any(|(key, _)| key.0 == *player_id))
        .collect::<Vec<_>>();
    if player_ids.is_empty() {
        return render_empty("No hero data available");
    }
    let player_set = player_ids.iter().copied().collect::<BTreeSet<_>>();

    #[derive(Clone, Copy)]
    struct HeroAggregate {
        hero_id: i64,
        max_games: i64,
        total_games: i64,
        first_index: usize,
    }
    let mut hero_aggregates = Vec::<HeroAggregate>::new();
    let mut hero_indices = BTreeMap::<i64, usize>::new();
    for (index, ((player_id, hero_id), (games, _))) in pivot.iter().enumerate() {
        if !player_set.contains(player_id) {
            continue;
        }
        if let Some(hero_index) = hero_indices.get(hero_id).copied() {
            let aggregate = &mut hero_aggregates[hero_index];
            aggregate.max_games = aggregate.max_games.max(*games);
            aggregate.total_games += *games;
        } else {
            hero_indices.insert(*hero_id, hero_aggregates.len());
            hero_aggregates.push(HeroAggregate {
                hero_id: *hero_id,
                max_games: *games,
                total_games: *games,
                first_index: index,
            });
        }
    }
    hero_aggregates.retain(|aggregate| aggregate.max_games >= min_games);
    if hero_aggregates.is_empty() {
        return render_empty("No heroes meet the minimum games threshold");
    }
    hero_aggregates.sort_by(|left, right| {
        right
            .total_games
            .cmp(&left.total_games)
            .then_with(|| left.first_index.cmp(&right.first_index))
    });
    let raw_hero_ids = hero_aggregates
        .iter()
        .map(|aggregate| aggregate.hero_id)
        .collect::<Vec<_>>();
    let layout = compute_layout(&player_ids, &raw_hero_ids)?;
    let mut raster = Raster::new(layout.width, layout.height, DISCORD_BG)?;

    let data = pivot.into_iter().collect::<BTreeMap<_, _>>();
    let names = player_names
        .iter()
        .map(|player| (player.discord_id, player.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let maximum_games = player_ids
        .iter()
        .flat_map(|player_id| {
            layout.hero_ids.iter().map(|hero_id| {
                data.get(&(*player_id, *hero_id))
                    .map_or(0, |(games, _)| *games)
            })
        })
        .max()
        .unwrap_or(1)
        .max(1);

    raster.draw_text(
        i32::try_from(GRID_PADDING).unwrap_or(0),
        i32::try_from(GRID_PADDING).unwrap_or(0),
        title,
        DISCORD_WHITE,
        2,
    );

    let draw_headers = |raster: &mut Raster, bottom: usize| {
        for (hero_index, hero_id) in layout.hero_ids.iter().enumerate() {
            let center_x = layout.hero_column_x(hero_index) + GRID_CELL_SIZE / 2;
            raster.draw_vertical_text(
                i32::try_from(center_x).unwrap_or(i32::MAX),
                i32::try_from(bottom).unwrap_or(i32::MAX),
                &hero_short_name(*hero_id),
                DISCORD_WHITE,
            );
        }
    };
    let draw_player_labels = |raster: &mut Raster, left: usize| {
        for (player_index, player_id) in player_ids.iter().enumerate() {
            let row_y = layout.player_row_y(player_index);
            let fallback = format!("Player {player_id}");
            let name = names.get(player_id).copied().unwrap_or(&fallback);
            raster.draw_text(
                i32::try_from(left).unwrap_or(i32::MAX),
                i32::try_from(row_y + 18).unwrap_or(i32::MAX),
                &truncate_player_name(name),
                DISCORD_WHITE,
                1,
            );
        }
    };

    let grid_right = if layout.num_heroes == 0 {
        GRID_PADDING + GRID_PLAYER_LABEL_WIDTH
    } else {
        layout.hero_column_x(layout.num_heroes - 1) + GRID_CELL_SIZE
    };
    for player_index in 0..layout.num_players {
        if player_index % 2 == 1 {
            let row_y = layout.player_row_y(player_index);
            raster.fill_rect(
                i32::try_from(GRID_PADDING + GRID_PLAYER_LABEL_WIDTH).unwrap_or(i32::MAX),
                i32::try_from(row_y).unwrap_or(i32::MAX),
                i32::try_from(grid_right).unwrap_or(i32::MAX),
                i32::try_from(row_y + GRID_CELL_SIZE).unwrap_or(i32::MAX),
                DISCORD_DARKER,
            );
        }
    }

    draw_headers(&mut raster, layout.grid_top);
    for band_index in 1..=layout.extra_bands {
        draw_headers(
            &mut raster,
            layout.player_row_y(band_index * GRID_LABEL_REPEAT_INTERVAL),
        );
    }
    draw_player_labels(&mut raster, GRID_PADDING);

    let grid_bottom = layout.player_row_y(layout.num_players - 1) + GRID_CELL_SIZE;
    for hero_index in 0..=layout.num_heroes {
        let x = if hero_index < layout.num_heroes {
            layout.hero_column_x(hero_index)
        } else {
            layout.hero_column_x(layout.num_heroes - 1) + GRID_CELL_SIZE
        };
        raster.vertical_line(
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(layout.grid_top).unwrap_or(i32::MAX),
            i32::try_from(grid_bottom).unwrap_or(i32::MAX),
            GRID_COLOR,
            1,
        );
    }
    for player_index in 0..=layout.num_players {
        let y = if player_index < layout.num_players {
            layout.player_row_y(player_index)
        } else {
            grid_bottom
        };
        raster.horizontal_line(
            i32::try_from(GRID_PADDING + GRID_PLAYER_LABEL_WIDTH).unwrap_or(i32::MAX),
            i32::try_from(grid_right).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            GRID_COLOR,
        );
    }

    for (player_index, player_id) in player_ids.iter().enumerate() {
        for (hero_index, hero_id) in layout.hero_ids.iter().enumerate() {
            let Some((games, wins)) = data.get(&(*player_id, *hero_id)).copied() else {
                continue;
            };
            if games <= 0 {
                continue;
            }
            let ratio = (games as f64 / maximum_games as f64).min(1.0);
            let radius = f64::from(GRID_MIN_CIRCLE_RADIUS)
                + f64::from(GRID_MAX_CIRCLE_RADIUS - GRID_MIN_CIRCLE_RADIUS) * ratio.sqrt();
            let radius = radius.round() as i32;
            let center_x = layout.hero_column_x(hero_index) + GRID_CELL_SIZE / 2;
            let center_y = layout.player_row_y(player_index) + GRID_CELL_SIZE / 2;
            raster.circle(
                i32::try_from(center_x).unwrap_or(i32::MAX),
                i32::try_from(center_y).unwrap_or(i32::MAX),
                radius,
                win_rate_color(games, wins),
            );
            if radius >= 10 {
                let count = games.to_string();
                let width = i32::try_from(count.chars().count().saturating_mul(6)).unwrap_or(0);
                raster.draw_text(
                    i32::try_from(center_x).unwrap_or(i32::MAX) - width / 2,
                    i32::try_from(center_y).unwrap_or(i32::MAX) - 3,
                    &count,
                    DISCORD_BG,
                    1,
                );
            }
        }
    }

    for column_index in 1..=layout.extra_columns {
        let hero_index = column_index * GRID_LABEL_REPEAT_INTERVAL;
        let left = layout.hero_column_x(hero_index) - GRID_PLAYER_LABEL_WIDTH;
        raster.fill_rect(
            i32::try_from(left).unwrap_or(i32::MAX),
            i32::try_from(layout.grid_top).unwrap_or(i32::MAX),
            i32::try_from(left + GRID_PLAYER_LABEL_WIDTH).unwrap_or(i32::MAX),
            i32::try_from(grid_bottom + 1).unwrap_or(i32::MAX),
            DISCORD_BG,
        );
        draw_player_labels(&mut raster, left);
        raster.vertical_line(
            i32::try_from(left).unwrap_or(i32::MAX) - 1,
            i32::try_from(layout.grid_top).unwrap_or(i32::MAX),
            i32::try_from(grid_bottom).unwrap_or(i32::MAX),
            SEPARATOR_COLOR,
            2,
        );
        raster.vertical_line(
            i32::try_from(left + GRID_PLAYER_LABEL_WIDTH).unwrap_or(i32::MAX),
            i32::try_from(layout.grid_top).unwrap_or(i32::MAX),
            i32::try_from(grid_bottom).unwrap_or(i32::MAX),
            SEPARATOR_COLOR,
            2,
        );
    }

    let legend_y = grid_bottom + 25;
    raster.draw_text(
        i32::try_from(GRID_PADDING).unwrap_or(0),
        i32::try_from(legend_y).unwrap_or(i32::MAX),
        "Size = games:  few   some   many",
        DISCORD_GREY,
        1,
    );
    raster.draw_text(
        i32::try_from(GRID_PADDING).unwrap_or(0),
        i32::try_from(legend_y + 45).unwrap_or(i32::MAX),
        "Color = WR:",
        DISCORD_GREY,
        1,
    );
    for (index, color) in [
        DISCORD_GREEN,
        DISCORD_LIGHT_GREEN,
        DISCORD_YELLOW,
        DISCORD_RED,
    ]
    .into_iter()
    .enumerate()
    {
        raster.circle(
            i32::try_from(GRID_PADDING + 90 + index * 58).unwrap_or(i32::MAX),
            i32::try_from(legend_y + 52).unwrap_or(i32::MAX),
            6,
            color,
        );
    }

    Ok(Cursor::new(encode_png(&raster)))
}

fn encode_png(raster: &Raster) -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(raster.width as u32).to_be_bytes());
    header.extend_from_slice(&(raster.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_png_chunk(&mut png, b"IHDR", &header);

    let row_bytes = raster.width * 4;
    let mut filtered = Vec::with_capacity((row_bytes + 1) * raster.height);
    for row in raster.pixels.chunks_exact(row_bytes) {
        filtered.push(0); // PNG filter type: None
        filtered.extend_from_slice(row);
    }

    let mut zlib = Vec::with_capacity(filtered.len() + filtered.len() / 65_535 * 5 + 16);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let chunks = filtered.chunks(65_535).collect::<Vec<_>>();
    for (index, chunk) in chunks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == chunks.len()));
        let length = u16::try_from(chunk.len()).expect("stored DEFLATE block length");
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(chunk);
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    append_png_chunk(&mut png, b"IDAT", &zlib);
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut checksum_input = Vec::with_capacity(4 + data.len());
    checksum_input.extend_from_slice(kind);
    checksum_input.extend_from_slice(data);
    output.extend_from_slice(&crc32(&checksum_input).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

#[derive(Clone, Debug)]
pub struct HeroGridCommandRequest<'a> {
    pub source: HeroGridSource,
    pub guild_id: Option<i64>,
    pub invoking_user_id: Option<i64>,
    pub explicit_lobby: Option<LobbyKind>,
    pub min_games: i64,
    pub limit: Option<i64>,
    pub sources: &'a HeroGridSources,
    pub player_name_overrides: Option<&'a BTreeMap<i64, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroGridAttachment {
    pub title: String,
    pub description: String,
    pub footer: String,
    pub filename: String,
    pub png: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeroGridCommandOutcome {
    Message(String),
    Attachment(HeroGridAttachment),
}

#[derive(Debug)]
pub enum HeroGridCommandError<E> {
    Data(E),
    Render(HeroGridRenderError),
}

impl<E: fmt::Display> fmt::Display for HeroGridCommandError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(error) => write!(formatter, "hero-grid data read failed: {error}"),
            Self::Render(error) => write!(formatter, "hero-grid render failed: {error}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for HeroGridCommandError<E> {}

#[derive(Clone, Debug)]
pub struct HeroGridCommandService<P> {
    port: P,
}

impl<P> HeroGridCommandService<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P: HeroGridDataPort> HeroGridCommandService<P> {
    /// Execute every synchronous policy step behind `/herogrid` without a
    /// Discord type, defer/follow-up call, gateway, or runtime dependency.
    pub fn execute(
        &self,
        request: &HeroGridCommandRequest<'_>,
    ) -> Result<HeroGridCommandOutcome, HeroGridCommandError<P::Error>> {
        let stage = match resolve_before_enriched_fallback(
            request.source,
            request.sources,
            request.invoking_user_id,
            request.explicit_lobby,
        ) {
            Ok(stage) => stage,
            Err(PlayerResolutionError::AmbiguousLobby) => {
                return Ok(HeroGridCommandOutcome::Message(
                    AMBIGUOUS_LOBBY_MESSAGE.to_owned(),
                ));
            }
        };
        let resolved = match stage {
            ResolutionStage::Resolved(resolved) => resolved,
            ResolutionStage::NeedEnrichedPlayers => {
                let players = self
                    .port
                    .players_with_enriched_data(request.guild_id)
                    .map_err(HeroGridCommandError::Data)?;
                ResolvedPlayers {
                    player_ids: players
                        .into_iter()
                        .map(|player| player.discord_id)
                        .collect(),
                    source_label: None,
                }
            }
        };

        if resolved.player_ids.is_empty() {
            let message = if request.source == HeroGridSource::Lobby {
                NO_LOBBY_SOURCE_MESSAGE
            } else {
                NO_ENRICHED_PLAYERS_MESSAGE
            };
            return Ok(HeroGridCommandOutcome::Message(message.to_owned()));
        }

        let mut player_ids = resolved.player_ids;
        if let Some(limit) = request.limit.filter(|limit| *limit > 0) {
            player_ids.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        let min_games = request.min_games.clamp(1, 10);
        let grid_data = self
            .port
            .multi_player_hero_stats(&player_ids, request.guild_id)
            .map_err(HeroGridCommandError::Data)?;
        if grid_data.is_empty() {
            return Ok(HeroGridCommandOutcome::Message(
                NO_SELECTED_HERO_DATA_MESSAGE.to_owned(),
            ));
        }

        let stored_names = self
            .port
            .player_names(&player_ids, request.guild_id)
            .map_err(HeroGridCommandError::Data)?;
        let stored_names = stored_names
            .into_iter()
            .map(|player| (player.discord_id, player.name))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let player_names = player_ids
            .iter()
            .copied()
            .filter(|player_id| seen.insert(*player_id))
            .map(|player_id| HeroGridPlayer {
                discord_id: player_id,
                name: request.player_name_overrides.map_or_else(
                    || {
                        stored_names
                            .get(&player_id)
                            .cloned()
                            .unwrap_or_else(|| format!("User {player_id}"))
                    },
                    |names| {
                        names
                            .get(&player_id)
                            .cloned()
                            .unwrap_or_else(|| player_id.to_string())
                    },
                ),
            })
            .collect::<Vec<_>>();

        let title = resolved.source_label.map_or_else(
            || "Hero Grid".to_owned(),
            |label| format!("Hero Grid: {label}"),
        );
        let png = draw_hero_grid(&grid_data, &player_names, min_games, &title)
            .map_err(HeroGridCommandError::Render)?
            .into_inner();
        Ok(HeroGridCommandOutcome::Attachment(HeroGridAttachment {
            title,
            description: format!(
                "{} players | min {min_games} games per hero",
                player_ids.len()
            ),
            footer:
                "Circle size = games played | Color = win rate (green ≥60%, yellow ≥40%, red <40%)"
                    .to_owned(),
            filename: "hero_grid.png".to_owned(),
            png,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::convert::Infallible;

    use super::*;

    const GUILD: i64 = 99;

    #[derive(Debug, Default)]
    struct MemoryPort {
        stats: Vec<HeroGridStat>,
        enriched: Vec<EnrichedPlayer>,
        names: Vec<HeroGridPlayer>,
        stats_calls: Cell<usize>,
    }

    impl HeroGridDataPort for MemoryPort {
        type Error = Infallible;

        fn multi_player_hero_stats(
            &self,
            discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<Vec<HeroGridStat>, Self::Error> {
            self.stats_calls.set(self.stats_calls.get() + 1);
            Ok(self
                .stats
                .iter()
                .filter(|row| discord_ids.contains(&row.discord_id))
                .cloned()
                .collect())
        }

        fn players_with_enriched_data(
            &self,
            _guild_id: Option<i64>,
        ) -> Result<Vec<EnrichedPlayer>, Self::Error> {
            Ok(self.enriched.clone())
        }

        fn player_names(
            &self,
            discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<Vec<HeroGridPlayer>, Self::Error> {
            let by_id = self
                .names
                .iter()
                .map(|player| (player.discord_id, player.name.clone()))
                .collect::<BTreeMap<_, _>>();
            Ok(discord_ids
                .iter()
                .filter_map(|discord_id| {
                    by_id.get(discord_id).map(|name| HeroGridPlayer {
                        discord_id: *discord_id,
                        name: name.clone(),
                    })
                })
                .collect())
        }
    }

    fn stat(discord_id: i64, hero_id: i64, games: i64, wins: i64) -> HeroGridStat {
        HeroGridStat {
            discord_id,
            hero_id,
            games,
            wins,
        }
    }

    fn player(discord_id: i64, name: &str) -> HeroGridPlayer {
        HeroGridPlayer {
            discord_id,
            name: name.to_owned(),
        }
    }

    #[derive(Debug)]
    struct DecodedPng {
        raster: Raster,
        color_type: u8,
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("four-byte integer"),
        )
    }

    /// Decode the exact PNG subset emitted above while checking every chunk
    /// CRC, every stored-DEFLATE length complement, and the zlib Adler sum.
    fn decode_png(bytes: &[u8]) -> DecodedPng {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let mut offset = 8;
        let mut width = None;
        let mut height = None;
        let mut color_type = None;
        let mut idat = Vec::new();
        let mut saw_end = false;
        while offset < bytes.len() {
            let length = read_u32(bytes, offset) as usize;
            offset += 4;
            let kind = &bytes[offset..offset + 4];
            offset += 4;
            let data = &bytes[offset..offset + length];
            offset += length;
            let expected_crc = read_u32(bytes, offset);
            offset += 4;
            let mut checksum = Vec::with_capacity(4 + data.len());
            checksum.extend_from_slice(kind);
            checksum.extend_from_slice(data);
            assert_eq!(crc32(&checksum), expected_crc, "invalid PNG chunk CRC");
            match kind {
                b"IHDR" => {
                    assert_eq!(data.len(), 13);
                    width = Some(read_u32(data, 0) as usize);
                    height = Some(read_u32(data, 4) as usize);
                    assert_eq!(data[8], 8);
                    color_type = Some(data[9]);
                    assert_eq!(&data[10..], &[0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => {
                    assert!(data.is_empty());
                    saw_end = true;
                }
                _ => panic!("unexpected PNG chunk"),
            }
        }
        assert!(saw_end);
        assert_eq!(offset, bytes.len());
        assert_eq!(&idat[..2], &[0x78, 0x01]);

        let mut compressed_offset = 2;
        let mut filtered = Vec::new();
        loop {
            let header = idat[compressed_offset];
            compressed_offset += 1;
            assert!(header == 0 || header == 1, "expected stored DEFLATE block");
            let length = u16::from_le_bytes(
                idat[compressed_offset..compressed_offset + 2]
                    .try_into()
                    .expect("DEFLATE length"),
            );
            compressed_offset += 2;
            let complement = u16::from_le_bytes(
                idat[compressed_offset..compressed_offset + 2]
                    .try_into()
                    .expect("DEFLATE complement"),
            );
            compressed_offset += 2;
            assert_eq!(length, !complement);
            let end = compressed_offset + usize::from(length);
            filtered.extend_from_slice(&idat[compressed_offset..end]);
            compressed_offset = end;
            if header == 1 {
                break;
            }
        }
        let expected_adler = read_u32(&idat, compressed_offset);
        compressed_offset += 4;
        assert_eq!(compressed_offset, idat.len());
        assert_eq!(adler32(&filtered), expected_adler, "invalid zlib Adler-32");

        let width = width.expect("IHDR width");
        let height = height.expect("IHDR height");
        let row_bytes = width * 4;
        assert_eq!(filtered.len(), height * (row_bytes + 1));
        let mut pixels = Vec::with_capacity(width * height * 4);
        for row in filtered.chunks_exact(row_bytes + 1) {
            assert_eq!(row[0], 0, "unexpected PNG row filter");
            pixels.extend_from_slice(&row[1..]);
        }
        DecodedPng {
            raster: Raster {
                width,
                height,
                pixels,
            },
            color_type: color_type.expect("IHDR color type"),
        }
    }

    fn render(data: &[HeroGridStat], names: &[HeroGridPlayer], min_games: i64) -> Cursor<Vec<u8>> {
        draw_hero_grid(data, names, min_games, "Hero Grid").expect("render hero grid")
    }

    fn pixel(raster: &Raster, x: usize, y: usize) -> Rgba {
        let offset = (y * raster.width + x) * 4;
        Rgba(
            raster.pixels[offset..offset + 4]
                .try_into()
                .expect("RGBA pixel"),
        )
    }

    fn region_contains(
        raster: &Raster,
        left: usize,
        top: usize,
        right: usize,
        bottom: usize,
        color: Rgba,
    ) -> bool {
        (top..bottom).any(|y| (left..right).any(|x| pixel(raster, x, y) == color))
    }

    fn dense_grid(num_players: i64, num_heroes: i64) -> (Vec<HeroGridStat>, Vec<HeroGridPlayer>) {
        let mut data = Vec::new();
        let mut names = Vec::new();
        for player_id in 1..=num_players {
            names.push(player(player_id, &format!("P{player_id}")));
            for hero_id in 1..=num_heroes {
                data.push(stat(player_id, hero_id, 5, 3));
            }
        }
        (data, names)
    }

    #[test]
    fn test_empty_data_returns_image() {
        let image = render(&[], &[], 2);
        assert_eq!(image.position(), 0);
        let decoded = decode_png(image.get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (450, 100));
    }

    #[test]
    fn test_empty_player_names_returns_image() {
        let image = render(&[stat(1, 1, 5, 3)], &[], 2);
        let decoded = decode_png(image.get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (450, 100));
    }

    #[test]
    fn test_single_player_single_hero() {
        let image = render(&[stat(1, 1, 5, 3)], &[player(1, "TestPlayer")], 1);
        let decoded = decode_png(image.get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (194, 274));
    }

    #[test]
    fn test_multiple_players_multiple_heroes() {
        let data = [
            stat(1, 1, 10, 7),
            stat(1, 2, 5, 1),
            stat(2, 1, 3, 3),
            stat(2, 3, 8, 4),
        ];
        let image = render(&data, &[player(1, "Alice"), player(2, "Bob")], 1);
        let decoded = decode_png(image.get_ref());
        assert_eq!(decoded.color_type, 6, "PNG must be RGBA");
        assert_eq!((decoded.raster.width, decoded.raster.height), (282, 318));
    }

    #[test]
    fn test_populated_input_returns_valid_png() {
        let data = [
            stat(1, 1, 12, 9),
            stat(1, 5, 6, 2),
            stat(2, 1, 8, 4),
            stat(2, 11, 4, 1),
            stat(3, 5, 10, 7),
        ];
        let names = [player(1, "Alice"), player(2, "Bob"), player(3, "Carol")];
        let image = draw_hero_grid(&data, &names, 2, "Smoke").expect("render populated grid");
        assert_eq!(image.position(), 0);
        assert!(image.get_ref().len() > 8);
        let decoded = decode_png(image.get_ref());
        assert!(decoded.raster.width > 0 && decoded.raster.height > 0);
    }

    #[test]
    fn test_empty_input_returns_valid_png() {
        let image = draw_hero_grid(&[], &[], 2, "Hero Grid").expect("render empty grid");
        assert_eq!(image.position(), 0);
        assert!(image.get_ref().len() > 8);
        let decoded = decode_png(image.get_ref());
        assert!(decoded.raster.width > 0 && decoded.raster.height > 0);
    }

    #[test]
    fn test_minimal_input_returns_valid_png() {
        let image = draw_hero_grid(&[stat(1, 1, 3, 2)], &[player(1, "Solo")], 1, "Hero Grid")
            .expect("render minimal grid");
        assert_eq!(image.position(), 0);
        assert!(image.get_ref().len() > 8);
        let decoded = decode_png(image.get_ref());
        assert!(decoded.raster.width > 0 && decoded.raster.height > 0);
    }

    #[test]
    fn test_min_games_filters_heroes() {
        let data = [stat(1, 1, 5, 3), stat(1, 2, 1, 0)];
        let names = [player(1, "TestPlayer")];
        let filtered = decode_png(render(&data, &names, 2).get_ref());
        let all = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!(filtered.raster.width, 194);
        assert_eq!(all.raster.width, 238);
        assert!(filtered.raster.width < all.raster.width);
    }

    #[test]
    fn test_no_heroes_meet_threshold() {
        let image = render(&[stat(1, 1, 1, 0)], &[player(1, "TestPlayer")], 5);
        let decoded = decode_png(image.get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (450, 100));
    }

    #[test]
    fn test_hero_cap_width() {
        let data = (1..=80).map(|hero| stat(1, hero, 3, 1)).collect::<Vec<_>>();
        let image = render(&data, &[player(1, "TestPlayer")], 1);
        let decoded = decode_png(image.get_ref());
        assert_eq!(decoded.raster.width, 3_390, "60-hero cap geometry");
        assert!(decoded.raster.width <= 4_000);
    }

    #[test]
    fn test_winrate_colors_all_brackets() {
        let data = [
            stat(1, 1, 10, 8),
            stat(1, 2, 10, 5),
            stat(1, 3, 10, 4),
            stat(1, 4, 10, 2),
        ];
        let image = render(&data, &[player(1, "TestPlayer")], 1);
        let decoded = decode_png(image.get_ref());
        let center_y = 135 + 22;
        for (index, expected) in [
            DISCORD_GREEN,
            DISCORD_LIGHT_GREEN,
            DISCORD_YELLOW,
            DISCORD_RED,
        ]
        .into_iter()
        .enumerate()
        {
            let center_x = 135 + index * 44 + 22;
            assert_eq!(pixel(&decoded.raster, center_x + 17, center_y), expected);
        }
    }

    #[test]
    fn test_returns_seekable_bytesio() {
        let mut image = render(&[stat(1, 1, 5, 3)], &[player(1, "Test")], 1);
        assert_eq!(image.position(), 0);
        image.set_position(17);
        assert_eq!(image.position(), 17);
        image.set_position(0);
        decode_png(image.get_ref());
    }

    #[test]
    fn test_long_player_name_truncated() {
        let data = [stat(1, 1, 5, 3)];
        let long = render(&data, &[player(1, "ThisIsAVeryLongPlayerName")], 1);
        let explicit = render(&data, &[player(1, "ThisIsAVeryL..")], 1);
        assert_eq!(
            truncate_player_name("ThisIsAVeryLongPlayerName"),
            "ThisIsAVeryL.."
        );
        assert_eq!(long.into_inner(), explicit.into_inner());
    }

    #[test]
    fn test_many_players() {
        let data = (1..=20)
            .map(|player_id| stat(player_id, 1, 5, 3))
            .collect::<Vec<_>>();
        let names = (1..=20)
            .map(|player_id| player(player_id, &format!("Player{player_id}")))
            .collect::<Vec<_>>();
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!(decoded.raster.height, 1_200);
    }

    #[test]
    fn test_player_order_preserved() {
        let data = [stat(1, 1, 10, 8), stat(2, 1, 10, 2)];
        let decoded =
            decode_png(render(&data, &[player(2, "Bob"), player(1, "Alice")], 1).get_ref());
        let center_x = 135 + 22;
        assert_eq!(pixel(&decoded.raster, center_x + 17, 135 + 22), DISCORD_RED);
        assert_eq!(
            pixel(&decoded.raster, center_x + 17, 135 + 44 + 22),
            DISCORD_GREEN
        );
    }

    #[test]
    fn test_full_pipeline() {
        let sources = HeroGridSources {
            open_lobby: vec![100, 200],
            ..HeroGridSources::default()
        };
        let service = HeroGridCommandService::new(MemoryPort {
            stats: vec![stat(100, 1, 1, 1), stat(200, 2, 1, 0)],
            names: vec![player(100, "Alice"), player(200, "Bob")],
            ..MemoryPort::default()
        });
        let name_overrides = BTreeMap::from([(100, "Server Alice".to_owned())]);
        let outcome = service
            .execute(&HeroGridCommandRequest {
                source: HeroGridSource::Auto,
                guild_id: Some(GUILD),
                invoking_user_id: None,
                explicit_lobby: None,
                min_games: 1,
                limit: None,
                sources: &sources,
                player_name_overrides: Some(&name_overrides),
            })
            .expect("full command pipeline");
        let HeroGridCommandOutcome::Attachment(attachment) = outcome else {
            panic!("expected rendered attachment");
        };
        assert_eq!(
            attachment.title,
            format!("Hero Grid: {}", LobbyKind::Open.label())
        );
        assert_eq!(attachment.filename, "hero_grid.png");
        let decoded = decode_png(&attachment.png);
        assert_eq!((decoded.raster.width, decoded.raster.height), (238, 318));
        let expected = draw_hero_grid(
            &[stat(100, 1, 1, 1), stat(200, 2, 1, 0)],
            &[player(100, "Server Alice"), player(200, "200")],
            1,
            &format!("Hero Grid: {}", LobbyKind::Open.label()),
        )
        .expect("render expected nickname override")
        .into_inner();
        assert_eq!(attachment.png, expected);
    }

    fn enriched(ids: &[i64]) -> Vec<EnrichedPlayer> {
        ids.iter()
            .enumerate()
            .map(|(index, discord_id)| EnrichedPlayer {
                discord_id: *discord_id,
                total_games: i64::try_from(ids.len() - index).expect("small fixture"),
            })
            .collect()
    }

    #[test]
    fn test_resolve_lobby_priority() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2, 3],
            guild_pending_match: Some(Teams::new(&[10, 11], &[20, 21])),
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve lobby");
        assert_eq!(
            resolved.player_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([1, 2, 3])
        );
        assert_eq!(
            resolved.source_label.as_deref(),
            Some(LobbyKind::Open.label())
        );
    }

    #[test]
    fn test_resolve_lobby_ignores_legacy_conditional_players() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            legacy_conditional_players: vec![99],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve lobby");
        assert_eq!(resolved.player_ids, [1, 2]);
        assert!(!resolved.player_ids.contains(&99));
    }

    #[test]
    fn test_resolve_pending_match_fallback() {
        let sources = HeroGridSources {
            guild_pending_match: Some(Teams::new(&[1, 2, 3, 4, 5], &[6, 7, 8, 9, 10])),
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve pending match");
        assert_eq!(resolved.player_ids, (1..=10).collect::<Vec<_>>());
        assert_eq!(resolved.source_label.as_deref(), Some("Active Match"));
    }

    #[test]
    fn test_resolve_invokers_pending_match_when_two_are_active() {
        let sources = HeroGridSources {
            invoking_pending_match: Some(Teams::new(&[7, 9], &[8, 10])),
            guild_pending_match: Some(Teams::new(&[101], &[102])),
            invoking_pending_lookup_available: true,
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, Some(7), None, &[])
            .expect("resolve invoking player's pending match");
        assert_eq!(resolved.player_ids, [7, 9, 8, 10]);
        assert_eq!(resolved.source_label.as_deref(), Some("Active Match"));
    }

    #[test]
    fn test_resolve_draft_fallback() {
        let sources = HeroGridSources {
            draft_pool_ids: Some(vec![100, 200, 300]),
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve draft");
        assert_eq!(resolved.player_ids, [100, 200, 300]);
        assert_eq!(resolved.source_label.as_deref(), Some("Draft"));
    }

    #[test]
    fn test_resolve_last_match_fallback() {
        let sources = HeroGridSources {
            last_match_ids: vec![50, 51, 52, 53, 54],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve last match");
        assert_eq!(resolved.player_ids, [50, 51, 52, 53, 54]);
        assert_eq!(resolved.source_label.as_deref(), Some("Last Match"));
    }

    #[test]
    fn test_resolve_all_fallback() {
        let resolved = resolve_player_ids(
            HeroGridSource::Auto,
            &HeroGridSources::default(),
            None,
            None,
            &enriched(&[1_000, 2_000, 3_000]),
        )
        .expect("resolve all fallback");
        assert_eq!(resolved.player_ids, [1_000, 2_000, 3_000]);
        assert_eq!(resolved.source_label, None);
    }

    #[test]
    fn test_resolve_priority_order() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            guild_pending_match: Some(Teams::new(&[10], &[20])),
            last_match_ids: vec![50, 51],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("resolve priority");
        assert_eq!(resolved.player_ids, [1, 2]);
        assert_eq!(
            resolved.source_label.as_deref(),
            Some(LobbyKind::Open.label())
        );
    }

    #[test]
    fn test_draft_state_manager_none() {
        let sources = HeroGridSources {
            draft_pool_ids: None,
            last_match_ids: vec![70, 71],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("skip absent draft manager");
        assert_eq!(resolved.player_ids, [70, 71]);
        assert_eq!(resolved.source_label.as_deref(), Some("Last Match"));
    }

    #[test]
    fn test_match_service_no_shuffle() {
        let sources = HeroGridSources {
            guild_pending_match: None,
            last_match_ids: vec![80, 81],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(HeroGridSource::Auto, &sources, None, None, &[])
            .expect("skip absent shuffle");
        assert_eq!(resolved.player_ids, [80, 81]);
        assert_eq!(resolved.source_label.as_deref(), Some("Last Match"));
    }

    #[test]
    fn test_source_all_skips_chain() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2, 3],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(
            HeroGridSource::All,
            &sources,
            None,
            None,
            &enriched(&[100, 200]),
        )
        .expect("all skips source chain");
        assert_eq!(resolved.player_ids, [100, 200]);
        assert_eq!(resolved.source_label, None);
    }

    #[test]
    fn test_source_lobby_fails_gracefully() {
        let resolved = resolve_player_ids(
            HeroGridSource::Lobby,
            &HeroGridSources::default(),
            None,
            None,
            &enriched(&[100, 200]),
        )
        .expect("empty lobby resolution");
        assert!(resolved.player_ids.is_empty());
    }

    #[test]
    fn test_current_lobby_uses_invokers_whine_and_cheese_roster_and_label() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::LowSkill],
            ..HeroGridSources::default()
        };
        let service = HeroGridCommandService::new(MemoryPort {
            stats: vec![stat(7, 1, 3, 2)],
            names: vec![player(7, "P7"), player(8, "P8")],
            ..MemoryPort::default()
        });
        let outcome = service
            .execute(&HeroGridCommandRequest {
                source: HeroGridSource::Lobby,
                guild_id: Some(GUILD),
                invoking_user_id: Some(7),
                explicit_lobby: None,
                min_games: 1,
                limit: None,
                sources: &sources,
                player_name_overrides: None,
            })
            .expect("execute current-lobby command");
        let HeroGridCommandOutcome::Attachment(attachment) = outcome else {
            panic!("expected attachment");
        };
        assert_eq!(
            attachment.title,
            format!("Hero Grid: {}", LobbyKind::LowSkill.label())
        );
        assert_eq!(attachment.description, "2 players | min 1 games per hero");
    }

    #[test]
    fn test_dual_membership_without_explicit_lobby_is_ambiguous() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..HeroGridSources::default()
        };
        assert_eq!(
            resolve_player_ids(HeroGridSource::Auto, &sources, Some(7), None, &[],),
            Err(PlayerResolutionError::AmbiguousLobby)
        );
    }

    #[test]
    fn test_explicit_lobby_kind_beats_dual_membership() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(
            HeroGridSource::Auto,
            &sources,
            Some(7),
            Some(LobbyKind::LowSkill),
            &[],
        )
        .expect("explicit kind resolves ambiguity");
        assert_eq!(resolved.player_ids, [7, 8]);
        assert_eq!(
            resolved.source_label.as_deref(),
            Some(LobbyKind::LowSkill.label())
        );
    }

    #[test]
    fn test_source_all_short_circuits_before_the_lobby_branch() {
        let sources = HeroGridSources {
            open_lobby: vec![1, 2],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..HeroGridSources::default()
        };
        let resolved = resolve_player_ids(
            HeroGridSource::All,
            &sources,
            Some(7),
            None,
            &enriched(&[100, 200]),
        )
        .expect("all source cannot be ambiguous");
        assert_eq!(resolved.player_ids, [100, 200]);
    }

    #[test]
    fn test_command_reports_ambiguity_instead_of_raising() {
        let sources = HeroGridSources {
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..HeroGridSources::default()
        };
        let port = MemoryPort::default();
        let service = HeroGridCommandService::new(port);
        let outcome = service
            .execute(&HeroGridCommandRequest {
                source: HeroGridSource::Lobby,
                guild_id: Some(GUILD),
                invoking_user_id: Some(7),
                explicit_lobby: None,
                min_games: 2,
                limit: None,
                sources: &sources,
                player_name_overrides: None,
            })
            .expect("ambiguity is a command result");
        assert_eq!(
            outcome,
            HeroGridCommandOutcome::Message(AMBIGUOUS_LOBBY_MESSAGE.to_owned())
        );
        assert_eq!(service.port.stats_calls.get(), 0);
    }

    #[test]
    fn test_no_repeat_small_grid() {
        let (data, names) = dense_grid(5, 5);
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (370, 450));
    }

    #[test]
    fn test_repeat_bands_many_players() {
        let (data, names) = dense_grid(25, 5);
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!(decoded.raster.height, 1_510);
        assert!(region_contains(
            &decoded.raster,
            150,
            575,
            165,
            665,
            DISCORD_WHITE,
        ));
        assert!(region_contains(
            &decoded.raster,
            150,
            1_105,
            165,
            1_195,
            DISCORD_WHITE,
        ));
    }

    #[test]
    fn test_repeat_cols_many_heroes() {
        let (data, names) = dense_grid(5, 25);
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!(decoded.raster.width, 1_490);
        assert!(region_contains(
            &decoded.raster,
            575,
            150,
            695,
            180,
            DISCORD_WHITE,
        ));
        assert_eq!(pixel(&decoded.raster, 574, 200), SEPARATOR_COLOR);
    }

    #[test]
    fn test_exactly_10_no_repeat() {
        let (data, names) = dense_grid(10, 10);
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!((decoded.raster.width, decoded.raster.height), (590, 670));
    }

    #[test]
    fn test_11_players_one_repeat() {
        let (data, names) = dense_grid(11, 5);
        let decoded = decode_png(render(&data, &names, 1).get_ref());
        assert_eq!(decoded.raster.height, 804);
        assert!(region_contains(
            &decoded.raster,
            150,
            575,
            165,
            665,
            DISCORD_WHITE,
        ));
    }

    #[test]
    fn png_crc_adler_dimensions_and_bytes_are_deterministic() {
        let data = [stat(1, 1, 5, 3)];
        let names = [player(1, "Test")];
        let first = render(&data, &names, 1).into_inner();
        let second = render(&data, &names, 1).into_inner();
        assert_eq!(first, second);
        let decoded = decode_png(&first);
        assert_eq!((decoded.raster.width, decoded.raster.height), (194, 274));
    }

    #[test]
    fn signed_ids_survive_resolution_fallback_names_and_rendering() {
        let signed_id = i64::MIN + 17;
        let sources = HeroGridSources {
            open_lobby: vec![signed_id],
            ..HeroGridSources::default()
        };
        let service = HeroGridCommandService::new(MemoryPort {
            stats: vec![stat(signed_id, 1, 2, 1)],
            ..MemoryPort::default()
        });
        let outcome = service
            .execute(&HeroGridCommandRequest {
                source: HeroGridSource::Auto,
                guild_id: Some(GUILD),
                invoking_user_id: None,
                explicit_lobby: None,
                min_games: 1,
                limit: None,
                sources: &sources,
                player_name_overrides: None,
            })
            .expect("signed-id pipeline");
        let HeroGridCommandOutcome::Attachment(attachment) = outcome else {
            panic!("expected signed-id attachment");
        };
        decode_png(&attachment.png);
    }
}
