//! Dig visual-asset selection and rendering contracts.
//!
//! Python remains the live Discord/Pillow implementation. This additive module
//! owns the transport-independent part of the Rust cutover: catalog identity,
//! bounded file discovery, byte caching, media validation, deterministic render
//! requests, fallback policy, and single-flight pinnacle rendering. A future
//! runtime adapter can implement [`DigRenderPort`] with an image library without
//! coupling Discord or filesystem concerns to the application service.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

#[path = "dig_assets/authored_asset_manifest.rs"]
pub mod authored_asset_manifest;

/// Discord's per-file limit used by the Python dig asset loader.
pub const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;
pub const PHASE_TWO_LIMIT: usize = 512 * 1024;
pub const PHASE_THREE_LIMIT: usize = 750 * 1024;
pub const SCENE_SIZE: (u32, u32) = (320, 180);
pub const LAYER_THUMBNAIL_SIZE: (u32, u32) = (128, 128);
pub const PINNACLE_SIZE: (u32, u32) = (512, 288);
pub const EVENT_ASSET_SIZE: (u32, u32) = (1024, 576);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaFormat {
    Png,
    Gif,
}

impl MediaFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Gif => "gif",
        }
    }

    #[must_use]
    pub const fn signature(self) -> &'static [u8] {
        match self {
            Self::Png => b"\x89PNG\r\n\x1a\n",
            Self::Gif => b"GIF8",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelMode {
    Rgb,
    Rgba,
    Indexed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub format: MediaFormat,
    pub width: u32,
    pub height: u32,
    pub mode: PixelMode,
    pub frame_count: usize,
    pub frame_durations_ms: Vec<u32>,
    pub loop_count: Option<u16>,
    /// Per-frame upper bounds reported by the renderer's preservation metric.
    pub strongly_changed_fractions: Vec<f64>,
}

impl MediaInfo {
    #[must_use]
    pub fn still(format: MediaFormat, size: (u32, u32), mode: PixelMode) -> Self {
        Self {
            format,
            width: size.0,
            height: size.1,
            mode,
            frame_count: 1,
            frame_durations_ms: Vec::new(),
            loop_count: None,
            strongly_changed_fractions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaError {
    InvalidSignature,
    InvalidHeader,
    EmptyDimensions,
    InvalidFrameContract,
    Oversized { actual: usize, limit: usize },
    UnexpectedFormat,
    UnexpectedDimensions,
    UnexpectedMode,
    SourceArtChanged,
}

impl Display for MediaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature => formatter.write_str("invalid media signature"),
            Self::InvalidHeader => formatter.write_str("invalid media header"),
            Self::EmptyDimensions => formatter.write_str("media dimensions must be non-zero"),
            Self::InvalidFrameContract => formatter.write_str("invalid animated frame contract"),
            Self::Oversized { actual, limit } => {
                write!(formatter, "media has {actual} bytes, exceeding {limit}")
            }
            Self::UnexpectedFormat => formatter.write_str("unexpected media format"),
            Self::UnexpectedDimensions => formatter.write_str("unexpected media dimensions"),
            Self::UnexpectedMode => formatter.write_str("unexpected media pixel mode"),
            Self::SourceArtChanged => formatter.write_str("render changed too much source art"),
        }
    }
}

impl std::error::Error for MediaError {}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedMedia {
    pub bytes: Vec<u8>,
    pub info: MediaInfo,
}

impl RenderedMedia {
    pub fn validate(&self, limit: usize) -> Result<(), MediaError> {
        if self.bytes.len() > limit {
            return Err(MediaError::Oversized {
                actual: self.bytes.len(),
                limit,
            });
        }
        if !self.bytes.starts_with(self.info.format.signature()) {
            return Err(MediaError::InvalidSignature);
        }
        if self.info.width == 0 || self.info.height == 0 {
            return Err(MediaError::EmptyDimensions);
        }
        match self.info.format {
            MediaFormat::Png if self.info.frame_count != 1 => {
                return Err(MediaError::InvalidFrameContract);
            }
            MediaFormat::Gif
                if self.info.frame_count == 0
                    || self.info.frame_durations_ms.len() != self.info.frame_count =>
            {
                return Err(MediaError::InvalidFrameContract);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn validate_contract(
        &self,
        format: MediaFormat,
        size: (u32, u32),
        mode: PixelMode,
        limit: usize,
    ) -> Result<(), MediaError> {
        self.validate(limit)?;
        if self.info.format != format {
            return Err(MediaError::UnexpectedFormat);
        }
        if (self.info.width, self.info.height) != size {
            return Err(MediaError::UnexpectedDimensions);
        }
        if self.info.mode != mode {
            return Err(MediaError::UnexpectedMode);
        }
        Ok(())
    }
}

/// Inspect the header-level media contract used for repository asset audits.
pub fn inspect_media(bytes: &[u8]) -> Result<MediaInfo, MediaError> {
    if bytes.starts_with(MediaFormat::Png.signature()) {
        inspect_png(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        inspect_gif(bytes)
    } else {
        Err(MediaError::InvalidSignature)
    }
}

fn inspect_png(bytes: &[u8]) -> Result<MediaInfo, MediaError> {
    if bytes.len() < 29 || &bytes[12..16] != b"IHDR" {
        return Err(MediaError::InvalidHeader);
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("four-byte PNG width"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("four-byte PNG height"));
    let mode = match bytes[25] {
        2 => PixelMode::Rgb,
        3 => PixelMode::Indexed,
        6 => PixelMode::Rgba,
        _ => return Err(MediaError::InvalidHeader),
    };
    if width == 0 || height == 0 {
        return Err(MediaError::EmptyDimensions);
    }
    Ok(MediaInfo::still(MediaFormat::Png, (width, height), mode))
}

fn inspect_gif(bytes: &[u8]) -> Result<MediaInfo, MediaError> {
    if bytes.len() < 13 {
        return Err(MediaError::InvalidHeader);
    }
    let width = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    let height = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    if width == 0 || height == 0 {
        return Err(MediaError::EmptyDimensions);
    }

    let mut frames = 0usize;
    let mut delays = Vec::new();
    let mut pending_delay = None;
    let packed = bytes[10];
    let global_palette_bytes = if packed & 0x80 != 0 {
        3usize
            .checked_mul(1usize << (usize::from(packed & 0x07) + 1))
            .ok_or(MediaError::InvalidHeader)?
    } else {
        0
    };
    let mut index = 13usize
        .checked_add(global_palette_bytes)
        .filter(|index| *index <= bytes.len())
        .ok_or(MediaError::InvalidHeader)?;
    while index < bytes.len() {
        match bytes[index] {
            0x21 => {
                let label = *bytes.get(index + 1).ok_or(MediaError::InvalidHeader)?;
                if label == 0xf9 {
                    let block_size =
                        usize::from(*bytes.get(index + 2).ok_or(MediaError::InvalidHeader)?);
                    if block_size != 4 || index + 7 >= bytes.len() {
                        return Err(MediaError::InvalidFrameContract);
                    }
                    pending_delay = Some(
                        u32::from(u16::from_le_bytes([bytes[index + 4], bytes[index + 5]])) * 10,
                    );
                    if bytes[index + 7] != 0 {
                        return Err(MediaError::InvalidFrameContract);
                    }
                    index += 8;
                } else {
                    index = skip_gif_data_sub_blocks(bytes, index + 2)?;
                }
            }
            0x2c => {
                if index + 9 >= bytes.len() {
                    return Err(MediaError::InvalidFrameContract);
                }
                let image_packed = bytes[index + 9];
                index += 10;
                if image_packed & 0x80 != 0 {
                    let local_palette_bytes = 3usize
                        .checked_mul(1usize << (usize::from(image_packed & 0x07) + 1))
                        .ok_or(MediaError::InvalidFrameContract)?;
                    index = index
                        .checked_add(local_palette_bytes)
                        .filter(|index| *index <= bytes.len())
                        .ok_or(MediaError::InvalidFrameContract)?;
                }
                // LZW minimum code size precedes the image-data sub-blocks.
                index = index
                    .checked_add(1)
                    .filter(|index| *index <= bytes.len())
                    .ok_or(MediaError::InvalidFrameContract)?;
                index = skip_gif_data_sub_blocks(bytes, index)?;
                frames += 1;
                delays.push(pending_delay.take().unwrap_or_default());
            }
            0x3b => break,
            _ => return Err(MediaError::InvalidFrameContract),
        }
    }
    if frames == 0 {
        return Err(MediaError::InvalidFrameContract);
    }
    if pending_delay.is_some() {
        return Err(MediaError::InvalidFrameContract);
    }
    Ok(MediaInfo {
        format: MediaFormat::Gif,
        width,
        height,
        mode: PixelMode::Indexed,
        frame_count: frames,
        frame_durations_ms: delays,
        loop_count: None,
        strongly_changed_fractions: Vec::new(),
    })
}

fn skip_gif_data_sub_blocks(bytes: &[u8], mut index: usize) -> Result<usize, MediaError> {
    loop {
        let size = usize::from(*bytes.get(index).ok_or(MediaError::InvalidFrameContract)?);
        index += 1;
        if size == 0 {
            return Ok(index);
        }
        index = index
            .checked_add(size)
            .filter(|index| *index <= bytes.len())
            .ok_or(MediaError::InvalidFrameContract)?;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerPalette {
    pub name: &'static str,
    pub colors: [(u8, u8, u8); 4],
}

pub const LAYER_PALETTES: [LayerPalette; 8] = [
    LayerPalette {
        name: "Dirt",
        colors: [(59, 31, 10), (139, 69, 19), (160, 82, 45), (210, 180, 140)],
    },
    LayerPalette {
        name: "Stone",
        colors: [
            (50, 50, 50),
            (105, 105, 105),
            (128, 128, 128),
            (169, 169, 169),
        ],
    },
    LayerPalette {
        name: "Crystal",
        colors: [(0, 80, 80), (0, 139, 139), (0, 206, 209), (127, 255, 212)],
    },
    LayerPalette {
        name: "Magma",
        colors: [(80, 0, 0), (139, 0, 0), (255, 69, 0), (255, 215, 0)],
    },
    LayerPalette {
        name: "Abyss",
        colors: [(10, 0, 25), (26, 0, 51), (47, 0, 71), (75, 0, 130)],
    },
    LayerPalette {
        name: "Fungal Depths",
        colors: [(0, 40, 0), (0, 100, 0), (34, 139, 34), (124, 252, 0)],
    },
    LayerPalette {
        name: "Frozen Core",
        colors: [(0, 0, 60), (0, 0, 128), (65, 105, 225), (135, 206, 235)],
    },
    LayerPalette {
        name: "The Hollow",
        colors: [(3, 3, 3), (10, 10, 10), (20, 20, 20), (40, 40, 40)],
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnacleTheme {
    pub boss_id: &'static str,
    pub effect: &'static str,
    pub accent: (u8, u8, u8),
    pub highlight: (u8, u8, u8),
}

pub const PINNACLE_THEMES: [PinnacleTheme; 6] = [
    PinnacleTheme {
        boss_id: "forgotten_king",
        effect: "crown_fire",
        accent: (210, 54, 28),
        highlight: (255, 208, 70),
    },
    PinnacleTheme {
        boss_id: "hollowforged",
        effect: "crystal_choir",
        accent: (48, 180, 202),
        highlight: (185, 255, 245),
    },
    PinnacleTheme {
        boss_id: "first_digger",
        effect: "ghost_tunnel",
        accent: (105, 112, 150),
        highlight: (225, 236, 255),
    },
    PinnacleTheme {
        boss_id: "bastion_below",
        effect: "fortress_siege",
        accent: (150, 66, 42),
        highlight: (255, 174, 80),
    },
    PinnacleTheme {
        boss_id: "pale_surveyor",
        effect: "map_fold",
        accent: (190, 172, 118),
        highlight: (255, 242, 190),
    },
    PinnacleTheme {
        boss_id: "lantern_engine",
        effect: "heat_gear",
        accent: (218, 92, 26),
        highlight: (255, 225, 92),
    },
];

pub const SECRET_ACCENT: (u8, u8, u8) = (117, 33, 170);
pub const SECRET_HIGHLIGHT: (u8, u8, u8) = (70, 255, 226);

pub const EVENT_SCENE_IDS: [&str; 27] = [
    "pudge_fishing",
    "tinker_workshop",
    "roshan_lair",
    "arcanist_library",
    "the_dark_rift",
    "the_burrow",
    "toll_keeper",
    "mirror_tunnel",
    "void_market",
    "time_eddy",
    "paradox_loop",
    "the_cartographer",
    "the_final_merchant",
    "spore_storm",
    "mycelium_network",
    "bioluminescent_cathedral",
    "frozen_ancient",
    "the_lightless_path",
    "whispering_walls_extended",
    "wisps_tether",
    "tunnel_echoes",
    "stalled_caravan",
    "volatile_affix",
    "rivals_cache",
    "forsaken_pact",
    "mapworks_drift",
    "the_eye_opens",
];

pub const ROGUELIKE_EVENT_ASSET_IDS: [&str; 18] = [
    "collapsed_armory",
    "dead_prospectors_pack",
    "spore_debt",
    "clockwork_toll",
    "collectors_roots",
    "hungry_beacon",
    "abandoned_forge",
    "salted_threshold",
    "frayed_lifeline",
    "quartermaster_s_niche",
    "collapsed_armory_cache",
    "relic_bearing_strata",
    "prospector_s_last_pack",
    "song_below_stone",
    "first_descent_cartography",
    "surveyors_cairn",
    "ruinwager_vault",
    "red_thread_shrine",
];

pub const PRESSURE_EVENT_ASSET_IDS: [&str; 3] = [
    "second_life_ledger",
    "twin_scar_altar",
    "ember_clearinghouse",
];

pub const NEW_PRESTIGE_TWO_BOSS_IDS: [&str; 3] =
    ["cairn_general", "brass_croupier", "saltveil_commodore"];
pub const NEW_PINNACLE_BOSS_IDS: [&str; 3] = ["bastion_below", "pale_surveyor", "lantern_engine"];

#[must_use]
pub fn has_event_scene(event_id: &str) -> bool {
    EVENT_SCENE_IDS.contains(&event_id)
}

#[must_use]
pub fn layer_slug(layer_name: &str) -> Option<&'static str> {
    match layer_name {
        "Dirt" => Some("dirt"),
        "Stone" => Some("stone"),
        "Crystal" => Some("crystal"),
        "Magma" => Some("magma"),
        "Abyss" => Some("abyss"),
        "Fungal Depths" => Some("fungal_depths"),
        "Frozen Core" => Some("frozen_core"),
        "The Hollow" => Some("the_hollow"),
        _ => None,
    }
}

#[must_use]
pub fn boss_slug(boss_id: &str) -> Option<&'static str> {
    match boss_id {
        "grothak" => Some("grothak"),
        "crystalia" => Some("crystalia"),
        "magmus_rex" => Some("magmus"),
        "void_warden" => Some("void_warden"),
        "sporeling_sovereign" => Some("sporeling"),
        "chronofrost" => Some("chronofrost"),
        "nameless_depth" => Some("nameless"),
        "pudge" | "ogre_magi" | "crystal_maiden" | "tusk" | "lina" | "doom" | "spectre"
        | "void_spirit" | "treant_protector" | "broodmother" | "faceless_void" | "weaver"
        | "oracle" | "terrorblade" | "aegis_warden" | "cairn_general" | "heartspire"
        | "brass_croupier" | "emberwright" | "saltveil_commodore" | "xalatath" | "lilith"
        | "underlord" | "blightcoil" | "rimebound_king" | "spineback" | "forgotten_king"
        | "hollowforged" | "first_digger" | "bastion_below" | "pale_surveyor"
        | "lantern_engine" => Some(match boss_id {
            "pudge" => "pudge",
            "ogre_magi" => "ogre_magi",
            "crystal_maiden" => "crystal_maiden",
            "tusk" => "tusk",
            "lina" => "lina",
            "doom" => "doom",
            "spectre" => "spectre",
            "void_spirit" => "void_spirit",
            "treant_protector" => "treant_protector",
            "broodmother" => "broodmother",
            "faceless_void" => "faceless_void",
            "weaver" => "weaver",
            "oracle" => "oracle",
            "terrorblade" => "terrorblade",
            "aegis_warden" => "aegis_warden",
            "cairn_general" => "cairn_general",
            "heartspire" => "heartspire",
            "brass_croupier" => "brass_croupier",
            "emberwright" => "emberwright",
            "saltveil_commodore" => "saltveil_commodore",
            "xalatath" => "xalatath",
            "lilith" => "lilith",
            "underlord" => "underlord",
            "blightcoil" => "blightcoil",
            "rimebound_king" => "rimebound_king",
            "spineback" => "spineback",
            "forgotten_king" => "forgotten_king",
            "hollowforged" => "hollowforged",
            "first_digger" => "first_digger",
            "bastion_below" => "bastion_below",
            "pale_surveyor" => "pale_surveyor",
            "lantern_engine" => "lantern_engine",
            _ => unreachable!("outer boss-id match guarantees this arm"),
        }),
        _ => None,
    }
}

#[must_use]
pub const fn legacy_boss_id(depth: i32) -> Option<&'static str> {
    match depth {
        25 => Some("grothak"),
        50 => Some("crystalia"),
        75 => Some("magmus_rex"),
        100 => Some("void_warden"),
        150 => Some("sporeling_sovereign"),
        200 => Some("chronofrost"),
        275 => Some("nameless_depth"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BossScene {
    Encounter,
    Victory,
    Defeat,
}

impl BossScene {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Encounter => "encounter",
            Self::Victory => "victory",
            Self::Defeat => "defeat",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BossIdentity<'a> {
    Id(&'a str),
    LegacyDepth(i32),
}

impl<'a> BossIdentity<'a> {
    fn boss_id(self) -> Option<&'a str> {
        match self {
            Self::Id(value) => Some(value),
            Self::LegacyDepth(depth) => legacy_boss_id(depth),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RenderRequest {
    LayerThumbnail {
        layer_name: String,
    },
    BossScene {
        layer_name: String,
        won: Option<bool>,
    },
    EventScene {
        layer_name: String,
        event_id: String,
    },
    PinnaclePhase {
        boss_id: String,
        phase: u8,
        secret: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderFailure(pub String);

impl Display for RenderFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RenderFailure {}

/// Runtime boundary for Pillow-equivalent image generation.
pub trait DigRenderPort: Send + Sync {
    fn render(
        &self,
        request: &RenderRequest,
        source: Option<&[u8]>,
    ) -> Result<RenderedMedia, RenderFailure>;
}

/// Read-only boundary for asset storage; production uses [`FilesystemAssetSource`].
pub trait AssetSource: Send + Sync {
    fn len(&self, path: &Path) -> io::Result<u64>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

#[derive(Clone, Debug)]
pub struct FilesystemAssetSource {
    root: PathBuf,
}

impl FilesystemAssetSource {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        self.root.join(path)
    }
}

impl AssetSource for FilesystemAssetSource {
    fn len(&self, path: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(self.absolute(path))?.len())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(self.absolute(path))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetCandidate {
    pub path: PathBuf,
    pub format: MediaFormat,
    pub size: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PhaseKey {
    boss_id: String,
    phase: u8,
    secret: bool,
}

#[derive(Default)]
struct PhaseState {
    cache: HashMap<PhaseKey, Arc<[u8]>>,
    failures: HashSet<PhaseKey>,
    inflight: HashSet<PhaseKey>,
}

pub struct DigAssetService {
    source: Arc<dyn AssetSource>,
    renderer: Arc<dyn DigRenderPort>,
    bytes_cache: Mutex<HashMap<PathBuf, Arc<[u8]>>>,
    phase_state: Mutex<PhaseState>,
    phase_ready: Condvar,
}

impl DigAssetService {
    #[must_use]
    pub fn new(source: Arc<dyn AssetSource>, renderer: Arc<dyn DigRenderPort>) -> Self {
        Self {
            source,
            renderer,
            bytes_cache: Mutex::new(HashMap::new()),
            phase_state: Mutex::new(PhaseState::default()),
            phase_ready: Condvar::new(),
        }
    }

    #[must_use]
    pub fn find_asset(&self, directory: &Path, base_name: &str) -> Option<AssetCandidate> {
        for format in [MediaFormat::Gif, MediaFormat::Png] {
            let path = directory.join(format!("{base_name}.{}", format.extension()));
            let Ok(size) = self.source.len(&path) else {
                continue;
            };
            let Ok(size) = usize::try_from(size) else {
                continue;
            };
            if size <= MAX_FILE_SIZE {
                return Some(AssetCandidate { path, format, size });
            }
        }
        None
    }

    #[must_use]
    pub fn load_cached_bytes(&self, path: &Path) -> Option<Arc<[u8]>> {
        if let Some(bytes) = self
            .bytes_cache
            .lock()
            .expect("dig asset byte cache mutex is not poisoned")
            .get(path)
            .cloned()
        {
            return Some(bytes);
        }
        let bytes = self.source.read(path).ok()?;
        if bytes.len() > MAX_FILE_SIZE {
            return None;
        }
        let bytes: Arc<[u8]> = bytes.into();
        self.bytes_cache
            .lock()
            .expect("dig asset byte cache mutex is not poisoned")
            .insert(path.to_path_buf(), Arc::clone(&bytes));
        Some(bytes)
    }

    fn attachment(bytes: &[u8], filename: String) -> Attachment {
        Attachment {
            filename,
            bytes: bytes.to_vec(),
        }
    }

    pub fn draw_layer_thumbnail(&self, layer_name: &str) -> Result<RenderedMedia, RenderFailure> {
        let media = self.renderer.render(
            &RenderRequest::LayerThumbnail {
                layer_name: layer_name.to_owned(),
            },
            None,
        )?;
        media
            .validate_contract(
                MediaFormat::Png,
                LAYER_THUMBNAIL_SIZE,
                PixelMode::Rgb,
                MAX_FILE_SIZE,
            )
            .map_err(|error| RenderFailure(error.to_string()))?;
        Ok(media)
    }

    pub fn draw_boss_scene(
        &self,
        layer_name: &str,
        won: Option<bool>,
    ) -> Result<RenderedMedia, RenderFailure> {
        let media = self.renderer.render(
            &RenderRequest::BossScene {
                layer_name: layer_name.to_owned(),
                won,
            },
            None,
        )?;
        media
            .validate_contract(MediaFormat::Png, SCENE_SIZE, PixelMode::Rgb, MAX_FILE_SIZE)
            .map_err(|error| RenderFailure(error.to_string()))?;
        Ok(media)
    }

    pub fn draw_event_scene(
        &self,
        layer_name: &str,
        event_id: &str,
    ) -> Result<RenderedMedia, RenderFailure> {
        let media = self.renderer.render(
            &RenderRequest::EventScene {
                layer_name: layer_name.to_owned(),
                event_id: event_id.to_owned(),
            },
            None,
        )?;
        media
            .validate_contract(MediaFormat::Png, SCENE_SIZE, PixelMode::Rgb, MAX_FILE_SIZE)
            .map_err(|error| RenderFailure(error.to_string()))?;
        Ok(media)
    }

    #[must_use]
    pub fn get_boss_art(
        &self,
        identity: BossIdentity<'_>,
        scene: BossScene,
        layer_name: &str,
    ) -> Option<Attachment> {
        let boss_id = identity.boss_id()?;
        let slug = boss_slug(boss_id)?;
        let base_name = format!("{slug}_{}", scene.as_str());
        if let Some(asset) = self.find_asset(Path::new("bosses"), &base_name)
            && let Some(bytes) = self.load_cached_bytes(&asset.path)
        {
            return Some(Self::attachment(
                &bytes,
                format!("boss_{}.{}", scene.as_str(), asset.format.extension()),
            ));
        }

        let won = match scene {
            BossScene::Encounter => None,
            BossScene::Victory => Some(true),
            BossScene::Defeat => Some(false),
        };
        let rendered = self.draw_boss_scene(layer_name, won).ok()?;
        Some(Self::attachment(
            &rendered.bytes,
            format!("boss_{}.png", scene.as_str()),
        ))
    }

    #[must_use]
    pub fn get_layer_thumbnail(&self, layer_name: &str) -> Option<Attachment> {
        let slug = layer_slug(layer_name)?;
        if let Some(asset) = self.find_asset(Path::new("layers"), slug)
            && let Some(bytes) = self.load_cached_bytes(&asset.path)
        {
            return Some(Self::attachment(
                &bytes,
                format!("layer_{slug}.{}", asset.format.extension()),
            ));
        }
        let rendered = self.draw_layer_thumbnail(layer_name).ok()?;
        Some(Self::attachment(
            &rendered.bytes,
            format!("layer_{slug}.png"),
        ))
    }

    #[must_use]
    pub fn get_event_art(&self, event_id: &str, layer_name: &str) -> Option<Attachment> {
        if let Some(asset) = self.find_asset(Path::new("events"), event_id)
            && let Some(bytes) = self.load_cached_bytes(&asset.path)
        {
            return Some(Self::attachment(
                &bytes,
                format!("event_{event_id}.{}", asset.format.extension()),
            ));
        }
        if !has_event_scene(event_id) {
            return None;
        }
        let rendered = self.draw_event_scene(layer_name, event_id).ok()?;
        Some(Self::attachment(
            &rendered.bytes,
            format!("event_{event_id}.png"),
        ))
    }

    fn static_encounter(&self, boss_id: &str, layer_name: &str) -> Option<Attachment> {
        self.get_boss_art(BossIdentity::Id(boss_id), BossScene::Encounter, layer_name)
    }

    #[must_use]
    pub fn get_pinnacle_phase_art(
        &self,
        boss_id: &str,
        phase: u8,
        layer_name: &str,
        secret: bool,
    ) -> Option<Attachment> {
        let limit = match phase {
            2 => PHASE_TWO_LIMIT,
            3 => PHASE_THREE_LIMIT,
            _ => return self.static_encounter(boss_id, layer_name),
        };
        let key = PhaseKey {
            boss_id: boss_id.to_owned(),
            phase,
            secret,
        };

        let mut state = self
            .phase_state
            .lock()
            .expect("pinnacle phase state mutex is not poisoned");
        loop {
            if let Some(bytes) = state.cache.get(&key).cloned() {
                let extension = if phase == 2 { "png" } else { "gif" };
                return Some(Self::attachment(
                    &bytes,
                    format!("pinnacle_phase_{phase}.{extension}"),
                ));
            }
            if state.failures.contains(&key) {
                drop(state);
                return self.static_encounter(boss_id, layer_name);
            }
            if !state.inflight.contains(&key) {
                state.inflight.insert(key.clone());
                break;
            }
            state = self
                .phase_ready
                .wait(state)
                .expect("pinnacle phase state mutex is not poisoned while waiting");
        }
        drop(state);

        let source = boss_slug(boss_id).and_then(|slug| {
            self.load_cached_bytes(&Path::new("bosses").join(format!("{slug}_encounter.png")))
        });
        let request = RenderRequest::PinnaclePhase {
            boss_id: boss_id.to_owned(),
            phase,
            secret,
        };
        let rendered = source
            .as_deref()
            .ok_or_else(|| RenderFailure("missing encounter art".to_owned()))
            .and_then(|source_bytes| self.renderer.render(&request, Some(source_bytes)));
        let rendered = rendered.and_then(|media| {
            let (format, mode) = if phase == 2 {
                (MediaFormat::Png, PixelMode::Rgba)
            } else {
                (MediaFormat::Gif, PixelMode::Indexed)
            };
            media
                .validate_contract(format, PINNACLE_SIZE, mode, limit)
                .map_err(|error| RenderFailure(error.to_string()))?;
            if media
                .info
                .strongly_changed_fractions
                .iter()
                .any(|fraction| *fraction >= 0.05)
            {
                return Err(RenderFailure(MediaError::SourceArtChanged.to_string()));
            }
            Ok(media)
        });

        let mut state = self
            .phase_state
            .lock()
            .expect("pinnacle phase state mutex is not poisoned");
        state.inflight.remove(&key);
        match rendered {
            Ok(media) => {
                let bytes: Arc<[u8]> = media.bytes.into();
                state.cache.insert(key, Arc::clone(&bytes));
                self.phase_ready.notify_all();
                drop(state);
                let extension = if phase == 2 { "png" } else { "gif" };
                Some(Self::attachment(
                    &bytes,
                    format!("pinnacle_phase_{phase}.{extension}"),
                ))
            }
            Err(_) => {
                state.failures.insert(key);
                self.phase_ready.notify_all();
                drop(state);
                self.static_encounter(boss_id, layer_name)
            }
        }
    }

    #[must_use]
    pub fn phase_cache_counts(&self) -> (usize, usize, usize) {
        let state = self
            .phase_state
            .lock()
            .expect("pinnacle phase state mutex is not poisoned");
        (
            state.cache.len(),
            state.failures.len(),
            state.inflight.len(),
        )
    }
}

/// Audit that every application boss definition has an asset slug.
#[must_use]
pub fn boss_ids_missing_asset_slugs() -> BTreeSet<&'static str> {
    crate::dig_bosses::BOSSES
        .iter()
        .map(|boss| boss.boss_id)
        .chain(
            crate::dig_bosses::PINNACLE_BOSSES
                .iter()
                .map(|boss| boss.boss_id),
        )
        .filter(|boss_id| boss_slug(boss_id).is_none())
        .collect()
}

#[cfg(test)]
#[path = "dig_assets/tests.rs"]
mod tests;
