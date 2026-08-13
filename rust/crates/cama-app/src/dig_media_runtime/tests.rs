use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::dig_assets::{
    MAX_FILE_SIZE, MediaFormat, PixelMode, boss_ids_missing_asset_slugs, inspect_media,
};

use super::*;

#[derive(Default)]
struct MemorySource {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    reads: AtomicUsize,
}

impl MemorySource {
    fn insert(&self, path: impl Into<PathBuf>, bytes: Vec<u8>) {
        self.files
            .lock()
            .expect("memory source lock")
            .insert(path.into(), bytes);
    }
}

impl AssetSource for MemorySource {
    fn len(&self, path: &Path) -> io::Result<u64> {
        self.files
            .lock()
            .expect("memory source lock")
            .get(path)
            .map(|bytes| bytes.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing asset"))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.files
            .lock()
            .expect("memory source lock")
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing asset"))
    }
}

fn icon(color: [u8; 4]) -> Vec<u8> {
    encode_png(
        &RgbaImage::new(12, 10, color).expect("test icon"),
        PixelMode::Rgba,
    )
}

#[test]
fn production_constructor_uses_explicit_deployment_asset_root() {
    let config = DigRuntimeConfig::production();
    assert_eq!(config.asset_root, Path::new("/app/assets/dig"));
    let runtime = DigMediaRuntime::production(&config);
    assert!(runtime.pickaxe_art(0).is_none());
}

#[test]
fn authored_gif_precedes_png_and_cached_bytes_mint_fresh_attachments() {
    let source = Arc::new(MemorySource::default());
    source.insert("items/torch.gif", b"GIF89a-authored".to_vec());
    source.insert("items/torch.png", icon([1, 2, 3, 255]));
    let runtime = DigMediaRuntime::with_parts(source.clone(), Arc::new(NativeDigRenderer));

    let mut first = runtime.item_art("torch").expect("authored item");
    assert_eq!(first.filename, "item_torch.gif");
    assert_eq!(first.bytes, b"GIF89a-authored");
    first.bytes[0] = b'X';
    let second = runtime.item_art("torch").expect("cached authored item");
    assert_eq!(second.bytes, b"GIF89a-authored");
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn authored_item_pickaxe_strip_and_shop_grid_match_python_geometry() {
    let source = Arc::new(MemorySource::default());
    source.insert("pickaxes/wooden.png", icon([80, 60, 20, 255]));
    for (index, item_id) in SHOP_ITEM_IDS.iter().enumerate() {
        source.insert(
            format!("items/{item_id}.png"),
            icon([index as u8, 100, 200, 255]),
        );
    }
    let runtime = DigMediaRuntime::with_parts(source, Arc::new(NativeDigRenderer));

    assert_eq!(
        runtime.pickaxe_art(0).expect("pickaxe").filename,
        "pickaxe_wooden.png"
    );
    assert!(runtime.pickaxe_art(-1).is_none());
    assert!(runtime.pickaxe_art(7).is_none());

    let strip = runtime
        .compose_items_used(&["torch".to_owned(), "dynamite".to_owned()])
        .expect("used-item strip");
    assert_eq!(strip.filename, "items_used.png");
    let strip_info = inspect_media(&strip.bytes).expect("strip PNG");
    assert_eq!((strip_info.width, strip_info.height), (100, 48));
    assert_eq!(strip_info.mode, PixelMode::Rgba);

    let grid = runtime.compose_shop_grid().expect("shop grid");
    assert_eq!(grid.filename, "shop_grid.png");
    let grid_info = inspect_media(&grid.bytes).expect("grid PNG");
    assert_eq!((grid_info.width, grid_info.height), (252, 338));
    assert_eq!(grid_info.mode, PixelMode::Rgba);
}

#[test]
fn native_layer_boss_and_event_fallbacks_are_real_deterministic_media() {
    let runtime = DigMediaRuntime::with_parts(
        Arc::new(MemorySource::default()),
        Arc::new(NativeDigRenderer),
    );
    let first_layer = runtime.layer_thumbnail("Stone").expect("layer fallback");
    let second_layer = runtime
        .layer_thumbnail("Stone")
        .expect("layer fallback retry");
    assert_eq!(first_layer.bytes, second_layer.bytes);
    let layer = inspect_media(&first_layer.bytes).expect("layer PNG");
    assert_eq!((layer.width, layer.height), LAYER_THUMBNAIL_SIZE);
    assert_eq!(layer.mode, PixelMode::Rgb);

    let boss = runtime
        .boss_art(BossIdentity::Id("grothak"), BossScene::Encounter, "Dirt")
        .expect("boss fallback");
    let boss_info = inspect_media(&boss.bytes).expect("boss PNG");
    assert_eq!((boss_info.width, boss_info.height), SCENE_SIZE);
    assert_eq!(boss_info.mode, PixelMode::Rgb);

    let event = runtime
        .event_art("pudge_fishing", "Dirt")
        .expect("event fallback");
    let event_info = inspect_media(&event.bytes).expect("event PNG");
    assert_eq!((event_info.width, event_info.height), SCENE_SIZE);
    assert_eq!(event_info.mode, PixelMode::Rgb);
    assert!(runtime.event_art("not_an_authored_scene", "Dirt").is_none());
}

#[test]
fn native_pinnacle_phase_media_play_once_and_remain_within_runtime_limits() {
    let source = Arc::new(MemorySource::default());
    let encounter = encode_png(
        &RgbaImage::new(512, 288, [60, 40, 25, 255]).expect("encounter image"),
        PixelMode::Rgba,
    );
    source.insert("bosses/forgotten_king_encounter.png", encounter);
    let runtime = DigMediaRuntime::with_parts(source, Arc::new(NativeDigRenderer));

    let phase_two = runtime
        .pinnacle_phase_art("forgotten_king", 2, "The Hollow", false)
        .expect("phase two");
    assert!(phase_two.bytes.len() <= 512 * 1024);
    let info = inspect_media(&phase_two.bytes).expect("phase two PNG");
    assert_eq!(info.format, MediaFormat::Png);
    assert_eq!((info.width, info.height), PINNACLE_SIZE);
    assert_eq!(info.mode, PixelMode::Rgba);

    let phase_three = runtime
        .pinnacle_phase_art("forgotten_king", 3, "The Hollow", true)
        .expect("phase three");
    assert!(phase_three.bytes.len() <= 750 * 1024);
    let info = inspect_media(&phase_three.bytes).expect("phase three GIF");
    assert_eq!(info.format, MediaFormat::Gif);
    assert_eq!((info.width, info.height), PINNACLE_SIZE);
    assert_eq!(info.frame_count, 8);
    assert_eq!(info.loop_count, None);
    assert!(info.frame_durations_ms.last().copied().unwrap_or_default() >= 1_000);
}

#[test]
fn longest_dig_animation_uses_a_constant_bounded_global_palette() {
    const LONGEST_CANONICAL_FRAME_COUNT: usize = 34;
    assert_eq!(SCENE_SIZE, (320, 180));

    let frames = (0..LONGEST_CANONICAL_FRAME_COUNT)
        .map(|index| {
            RgbaImage::new(
                SCENE_SIZE.0 as usize,
                SCENE_SIZE.1 as usize,
                [
                    (index * 7) as u8,
                    (index * 11) as u8,
                    (index * 13) as u8,
                    255,
                ],
            )
            .expect("longest canonical animation frame")
        })
        .collect::<Vec<_>>();
    let durations = vec![100; LONGEST_CANONICAL_FRAME_COUNT];
    let bytes = encode_gif(frames, &durations).expect("bounded native GIF");

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::Indexed);
    let mut decoder = options
        .read_info(Cursor::new(&bytes))
        .expect("decode bounded native GIF");
    assert_eq!(decoder.global_palette().map(<[u8]>::len), Some(256 * 3));
    let mut decoded_frames = 0;
    while let Some(frame) = decoder.read_next_frame().expect("read native GIF frame") {
        assert!(
            frame.palette.is_none(),
            "all frames must reuse the fixed global palette"
        );
        decoded_frames += 1;
    }
    assert_eq!(decoded_frames, LONGEST_CANONICAL_FRAME_COUNT);
    assert_eq!(gif_palette().len(), 256 * 3);
}

#[test]
fn dig_gif_uses_one_shared_palette_and_preserves_exact_timing_and_release_boundary() {
    let frames = (0..4)
        .map(|index| {
            RgbaImage::new(32, 18, [index * 20, index * 10, index * 5, 255])
                .expect("timed native frame")
        })
        .collect::<Vec<_>>();
    let durations = [70, 80, 90, 60_000];

    // Moving the owned frames into `encode_gif` is the native equivalent of
    // Python closing each PIL image: all frame buffers are released at the
    // serialization boundary, including on an encoding error.
    let bytes = encode_gif(frames, &durations).expect("timed native GIF");
    let info = inspect_media(&bytes).expect("inspect timed native GIF");
    assert_eq!(info.frame_count, 4);
    assert_eq!(info.frame_durations_ms, durations);
    assert_eq!(info.loop_count, None);

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::Indexed);
    let mut decoder = options
        .read_info(Cursor::new(&bytes))
        .expect("decode timed native GIF");
    assert_eq!(decoder.global_palette().map(<[u8]>::len), Some(256 * 3));
    let mut local_palettes = 0;
    while let Some(frame) = decoder.read_next_frame().expect("read timed GIF frame") {
        local_palettes += usize::from(frame.palette.is_some());
    }
    assert_eq!(local_palettes, 0);
}

#[test]
fn canonical_production_tree_covers_every_boss_slug_and_file_size_contract() {
    assert!(boss_ids_missing_asset_slugs().is_empty());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|ancestor| ancestor.join("assets/dig"))
        .find(|candidate| candidate.is_dir())
        .expect("repository Dig asset tree");
    let runtime = DigMediaRuntime::production(&DigRuntimeConfig::with_asset_root(root));
    for layer in crate::dig_assets::LAYER_PALETTES {
        let attachment = runtime
            .layer_thumbnail(layer.name)
            .unwrap_or_else(|| panic!("missing layer media: {}", layer.name));
        assert!(attachment.bytes.len() <= MAX_FILE_SIZE);
    }
    for (tier, slug) in PICKAXE_SLUGS.iter().enumerate() {
        let attachment = runtime
            .pickaxe_art(tier as i64)
            .unwrap_or_else(|| panic!("missing pickaxe media: {slug}"));
        assert!(attachment.bytes.len() <= MAX_FILE_SIZE);
    }
}
