use super::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Default)]
struct MemorySource {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    reads: AtomicUsize,
}

impl MemorySource {
    fn insert(&self, path: impl Into<PathBuf>, bytes: Vec<u8>) {
        self.files
            .lock()
            .expect("memory source mutex is not poisoned")
            .insert(path.into(), bytes);
    }

    fn remove(&self, path: impl AsRef<Path>) {
        self.files
            .lock()
            .expect("memory source mutex is not poisoned")
            .remove(path.as_ref());
    }

    fn contains_fragment(&self, fragment: &str) -> bool {
        self.files
            .lock()
            .expect("memory source mutex is not poisoned")
            .keys()
            .any(|path| path.to_string_lossy().contains(fragment))
    }
}

impl AssetSource for MemorySource {
    fn len(&self, path: &Path) -> io::Result<u64> {
        self.files
            .lock()
            .expect("memory source mutex is not poisoned")
            .get(path)
            .map(|bytes| u64::try_from(bytes.len()).expect("test asset length fits u64"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing test asset"))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.files
            .lock()
            .expect("memory source mutex is not poisoned")
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing test asset"))
    }
}

#[derive(Default)]
struct RenderGate {
    blocked: bool,
    started: bool,
}

#[derive(Default)]
struct ContractRenderer {
    calls: Mutex<Vec<RenderRequest>>,
    mode: AtomicU8,
    gate: Mutex<RenderGate>,
    gate_ready: Condvar,
}

impl ContractRenderer {
    const FAILURE: u8 = 1;
    const OVERSIZE: u8 = 2;

    fn fail(&self) {
        self.mode.store(Self::FAILURE, Ordering::SeqCst);
    }

    fn oversize(&self) {
        self.mode.store(Self::OVERSIZE, Ordering::SeqCst);
    }

    fn block(&self) {
        self.gate
            .lock()
            .expect("render gate mutex is not poisoned")
            .blocked = true;
    }

    fn wait_started(&self) {
        let mut gate = self.gate.lock().expect("render gate mutex is not poisoned");
        while !gate.started {
            gate = self
                .gate_ready
                .wait(gate)
                .expect("render gate mutex is not poisoned while waiting");
        }
    }

    fn release(&self) {
        let mut gate = self.gate.lock().expect("render gate mutex is not poisoned");
        gate.blocked = false;
        self.gate_ready.notify_all();
    }

    fn call_count(&self, predicate: impl Fn(&RenderRequest) -> bool) -> usize {
        self.calls
            .lock()
            .expect("renderer call mutex is not poisoned")
            .iter()
            .filter(|request| predicate(request))
            .count()
    }
}

impl DigRenderPort for ContractRenderer {
    fn render(
        &self,
        request: &RenderRequest,
        source: Option<&[u8]>,
    ) -> Result<RenderedMedia, RenderFailure> {
        self.calls
            .lock()
            .expect("renderer call mutex is not poisoned")
            .push(request.clone());

        {
            let mut gate = self.gate.lock().expect("render gate mutex is not poisoned");
            gate.started = true;
            self.gate_ready.notify_all();
            while gate.blocked {
                gate = self
                    .gate_ready
                    .wait(gate)
                    .expect("render gate mutex is not poisoned while waiting");
            }
        }

        if self.mode.load(Ordering::SeqCst) == Self::FAILURE {
            return Err(RenderFailure("render failed".to_owned()));
        }

        let (format, size, mode, frames, durations, changed) = match request {
            RenderRequest::LayerThumbnail { .. } => (
                MediaFormat::Png,
                LAYER_THUMBNAIL_SIZE,
                PixelMode::Rgb,
                1,
                Vec::new(),
                Vec::new(),
            ),
            RenderRequest::BossScene { .. } | RenderRequest::EventScene { .. } => (
                MediaFormat::Png,
                SCENE_SIZE,
                PixelMode::Rgb,
                1,
                Vec::new(),
                Vec::new(),
            ),
            RenderRequest::PinnaclePhase { phase: 2, .. } => (
                MediaFormat::Png,
                PINNACLE_SIZE,
                PixelMode::Rgba,
                1,
                Vec::new(),
                vec![0.012],
            ),
            RenderRequest::PinnaclePhase { .. } => (
                MediaFormat::Gif,
                PINNACLE_SIZE,
                PixelMode::Indexed,
                8,
                vec![95, 95, 95, 95, 95, 95, 95, 1_500],
                vec![0.012; 8],
            ),
        };
        let mut bytes = format.signature().to_vec();
        bytes.extend_from_slice(format!("{request:?}").as_bytes());
        if let Some(source) = source {
            bytes.extend_from_slice(&source[..source.len().min(24)]);
        }
        if self.mode.load(Ordering::SeqCst) == Self::OVERSIZE {
            let limit = if matches!(request, RenderRequest::PinnaclePhase { phase: 3, .. }) {
                PHASE_THREE_LIMIT
            } else {
                PHASE_TWO_LIMIT
            };
            bytes.resize(limit + 1, b'x');
        }
        Ok(RenderedMedia {
            bytes,
            info: MediaInfo {
                format,
                width: size.0,
                height: size.1,
                mode,
                frame_count: frames,
                frame_durations_ms: durations,
                loop_count: None,
                strongly_changed_fractions: changed,
            },
        })
    }
}

fn fake_png(size: (u32, u32), mode: PixelMode) -> Vec<u8> {
    let color_type = match mode {
        PixelMode::Rgb => 2,
        PixelMode::Indexed => 3,
        PixelMode::Rgba => 6,
    };
    let mut bytes = vec![0; 29];
    bytes[..8].copy_from_slice(MediaFormat::Png.signature());
    bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
    bytes[12..16].copy_from_slice(b"IHDR");
    bytes[16..20].copy_from_slice(&size.0.to_be_bytes());
    bytes[20..24].copy_from_slice(&size.1.to_be_bytes());
    bytes[24] = 8;
    bytes[25] = color_type;
    bytes
}

fn service() -> (
    Arc<MemorySource>,
    Arc<ContractRenderer>,
    Arc<DigAssetService>,
) {
    let source = Arc::new(MemorySource::default());
    let renderer = Arc::new(ContractRenderer::default());
    let service = Arc::new(DigAssetService::new(source.clone(), renderer.clone()));
    (source, renderer, service)
}

fn service_with_encounter() -> (
    Arc<MemorySource>,
    Arc<ContractRenderer>,
    Arc<DigAssetService>,
) {
    let (source, renderer, service) = service();
    source.insert(
        "bosses/forgotten_king_encounter.png",
        fake_png(PINNACLE_SIZE, PixelMode::Rgba),
    );
    (source, renderer, service)
}

fn render_phase(boss_id: &str, phase: u8, secret: bool) -> RenderedMedia {
    ContractRenderer::default()
        .render(
            &RenderRequest::PinnaclePhase {
                boss_id: boss_id.to_owned(),
                phase,
                secret,
            },
            Some(&fake_png(PINNACLE_SIZE, PixelMode::Rgba)),
        )
        .expect("contract render succeeds")
}

fn repository_asset_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/dig")
}

fn repository_service() -> DigAssetService {
    DigAssetService::new(
        Arc::new(FilesystemAssetSource::new(repository_asset_root())),
        Arc::new(ContractRenderer::default()),
    )
}

fn assert_repository_png(relative: &Path, size: (u32, u32), mode: PixelMode) -> usize {
    let bytes = std::fs::read(repository_asset_root().join(relative))
        .expect("canonical repository asset exists");
    let info = inspect_media(&bytes).expect("canonical repository asset has a valid PNG header");
    assert_eq!(info.format, MediaFormat::Png);
    assert_eq!((info.width, info.height), size);
    assert_eq!(info.mode, mode);
    bytes.len()
}

#[test]
fn draw_layer_thumbnail_returns_bytes_with_valid_png_contract() {
    let (_, _, service) = service();
    let rendered = service
        .draw_layer_thumbnail("Dirt")
        .expect("thumbnail renderer succeeds");
    assert!(rendered.bytes.starts_with(MediaFormat::Png.signature()));
    assert_eq!((rendered.info.width, rendered.info.height), (128, 128));
}

#[test]
fn draw_layer_thumbnail_all_layers_produce_thumbnails() {
    let (_, _, service) = service();
    for palette in LAYER_PALETTES {
        let rendered = service
            .draw_layer_thumbnail(palette.name)
            .expect("registered layer renders");
        assert_eq!((rendered.info.width, rendered.info.height), (128, 128));
    }
}

#[test]
fn draw_layer_thumbnail_output_is_deterministic() {
    let (_, _, service) = service();
    let first = service.draw_layer_thumbnail("Stone").expect("first render");
    let second = service
        .draw_layer_thumbnail("Stone")
        .expect("second render");
    assert_eq!(first.bytes, second.bytes);
}

#[test]
fn draw_boss_scene_encounter_returns_valid_png() {
    let (_, _, service) = service();
    let rendered = service.draw_boss_scene("Dirt", None).expect("boss scene");
    assert_eq!(rendered.info.format, MediaFormat::Png);
    assert_eq!((rendered.info.width, rendered.info.height), SCENE_SIZE);
}

#[test]
fn draw_boss_result_scene_victory_returns_valid_png() {
    let (_, _, service) = service();
    assert!(service.draw_boss_scene("Crystal", Some(true)).is_ok());
}

#[test]
fn draw_boss_result_scene_defeat_returns_valid_png() {
    let (_, _, service) = service();
    assert!(service.draw_boss_scene("Magma", Some(false)).is_ok());
}

#[test]
fn pinnacle_phase_two_is_rgba_png_within_runtime_limit() {
    let media = render_phase("forgotten_king", 2, false);
    media
        .validate_contract(
            MediaFormat::Png,
            PINNACLE_SIZE,
            PixelMode::Rgba,
            PHASE_TWO_LIMIT,
        )
        .expect("phase two contract is valid");
}

#[test]
fn pinnacle_phase_three_is_eight_frame_play_once_gif_with_final_hold() {
    let media = render_phase("hollowforged", 3, false);
    media
        .validate_contract(
            MediaFormat::Gif,
            PINNACLE_SIZE,
            PixelMode::Indexed,
            PHASE_THREE_LIMIT,
        )
        .expect("phase three contract is valid");
    assert_eq!(media.info.frame_count, 8);
    assert_eq!(media.info.loop_count, None);
    assert!(media.info.frame_durations_ms[7] >= 1_000);
    assert!(
        media.info.frame_durations_ms[7]
            > media.info.frame_durations_ms[..7]
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
    );
}

#[test]
fn all_pinnacle_themes_and_secret_palette_are_distinct() {
    let ids: BTreeSet<_> = PINNACLE_THEMES.iter().map(|theme| theme.boss_id).collect();
    let catalog_ids: BTreeSet<_> = crate::dig_bosses::PINNACLE_BOSSES
        .iter()
        .map(|boss| boss.boss_id)
        .collect();
    let outputs: BTreeSet<_> = PINNACLE_THEMES
        .iter()
        .map(|theme| render_phase(theme.boss_id, 2, false).bytes)
        .collect();
    assert_eq!(ids, catalog_ids);
    assert_eq!(outputs.len(), PINNACLE_THEMES.len());
    assert_ne!(SECRET_ACCENT, PINNACLE_THEMES[0].accent);
    assert_ne!(SECRET_HIGHLIGHT, PINNACLE_THEMES[0].highlight);
}

macro_rules! secret_palette_test {
    ($name:ident, $boss_id:literal) => {
        #[test]
        fn $name() {
            let normal_two = render_phase($boss_id, 2, false);
            let secret_two = render_phase($boss_id, 2, true);
            let normal_three = render_phase($boss_id, 3, false);
            let secret_three = render_phase($boss_id, 3, true);
            assert_ne!(normal_two.bytes, secret_two.bytes);
            assert_ne!(normal_three.bytes, secret_three.bytes);
        }
    };
}

secret_palette_test!(
    secret_palette_changes_both_phases_forgotten_king,
    "forgotten_king"
);
secret_palette_test!(
    secret_palette_changes_both_phases_hollowforged,
    "hollowforged"
);
secret_palette_test!(
    secret_palette_changes_both_phases_first_digger,
    "first_digger"
);
secret_palette_test!(
    secret_palette_changes_both_phases_bastion_below,
    "bastion_below"
);
secret_palette_test!(
    secret_palette_changes_both_phases_pale_surveyor,
    "pale_surveyor"
);
secret_palette_test!(
    secret_palette_changes_both_phases_lantern_engine,
    "lantern_engine"
);

macro_rules! preservation_test {
    ($name:ident, $boss_id:literal, $secret:literal) => {
        #[test]
        fn $name() {
            let phase_two = render_phase($boss_id, 2, $secret);
            let phase_three = render_phase($boss_id, 3, $secret);
            assert!(phase_two.bytes.len() <= PHASE_TWO_LIMIT);
            assert!(phase_three.bytes.len() <= PHASE_THREE_LIMIT);
            assert_eq!(phase_three.info.frame_count, 8);
            assert!(
                phase_two
                    .info
                    .strongly_changed_fractions
                    .iter()
                    .all(|value| *value < 0.05)
            );
            assert!(
                phase_three
                    .info
                    .strongly_changed_fractions
                    .iter()
                    .all(|value| *value < 0.05)
            );
        }
    };
}

preservation_test!(
    pinnacle_effects_preserve_source_normal_forgotten_king,
    "forgotten_king",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_normal_hollowforged,
    "hollowforged",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_normal_first_digger,
    "first_digger",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_normal_bastion_below,
    "bastion_below",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_normal_pale_surveyor,
    "pale_surveyor",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_normal_lantern_engine,
    "lantern_engine",
    false
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_forgotten_king,
    "forgotten_king",
    true
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_hollowforged,
    "hollowforged",
    true
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_first_digger,
    "first_digger",
    true
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_bastion_below,
    "bastion_below",
    true
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_pale_surveyor,
    "pale_surveyor",
    true
);
preservation_test!(
    pinnacle_effects_preserve_source_secret_lantern_engine,
    "lantern_engine",
    true
);

#[test]
fn pinnacle_phase_three_effects_are_deterministic_normal() {
    assert_eq!(
        render_phase("forgotten_king", 3, false).bytes,
        render_phase("forgotten_king", 3, false).bytes
    );
}

#[test]
fn pinnacle_phase_three_effects_are_deterministic_secret() {
    assert_eq!(
        render_phase("lantern_engine", 3, true).bytes,
        render_phase("lantern_engine", 3, true).bytes
    );
}

#[test]
fn draw_event_scene_known_event_returns_valid_png() {
    let (_, _, service) = service();
    let rendered = service
        .draw_event_scene("Dirt", "pudge_fishing")
        .expect("event scene");
    assert_eq!(rendered.info.format, MediaFormat::Png);
    assert_eq!((rendered.info.width, rendered.info.height), SCENE_SIZE);
}

#[test]
fn draw_event_scene_unknown_event_still_renders_background_and_player() {
    let (_, renderer, service) = service();
    assert!(
        service
            .draw_event_scene("Stone", "nonexistent_event")
            .is_ok()
    );
    assert_eq!(renderer.call_count(|request| matches!(request, RenderRequest::EventScene { event_id, .. } if event_id == "nonexistent_event")), 1);
}

#[test]
fn has_event_scene_recognizes_known_event() {
    assert!(has_event_scene("pudge_fishing"));
}

#[test]
fn has_event_scene_rejects_unknown_event() {
    assert!(!has_event_scene("definitely_not_a_real_event"));
}

#[test]
fn find_asset_finds_png_file() {
    let (source, _, service) = service();
    source.insert("tmp/test.png", fake_png((1, 1), PixelMode::Rgba));
    let asset = service
        .find_asset(Path::new("tmp"), "test")
        .expect("PNG found");
    assert_eq!(asset.path, Path::new("tmp/test.png"));
}

#[test]
fn find_asset_returns_none_for_missing_file() {
    let (_, _, service) = service();
    assert_eq!(service.find_asset(Path::new("tmp"), "nonexistent"), None);
}

#[test]
fn find_asset_skips_oversized_file() {
    let (source, _, service) = service();
    source.insert("tmp/big.png", vec![0; MAX_FILE_SIZE + 1]);
    assert_eq!(service.find_asset(Path::new("tmp"), "big"), None);
}

#[test]
fn find_asset_prefers_gif_over_png() {
    let (source, _, service) = service();
    source.insert("tmp/test.gif", b"GIF89a payload".to_vec());
    source.insert("tmp/test.png", fake_png((1, 1), PixelMode::Rgba));
    let asset = service
        .find_asset(Path::new("tmp"), "test")
        .expect("asset found");
    assert_eq!(asset.format, MediaFormat::Gif);
}

#[test]
fn load_cached_bytes_loads_once_and_survives_source_removal() {
    let (source, _, service) = service();
    source.insert("tmp/data.bin", b"hello".to_vec());
    let first = service
        .load_cached_bytes(Path::new("tmp/data.bin"))
        .expect("loads");
    source.remove("tmp/data.bin");
    let second = service
        .load_cached_bytes(Path::new("tmp/data.bin"))
        .expect("cached");
    assert_eq!(&*first, b"hello");
    assert_eq!(&*second, b"hello");
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn load_cached_bytes_returns_none_for_missing_file() {
    let (_, _, service) = service();
    assert!(
        service
            .load_cached_bytes(Path::new("tmp/nope.bin"))
            .is_none()
    );
}

#[test]
fn get_boss_art_resolves_grandfathered_boss_by_id() {
    let (_, _, service) = service();
    let asset = service.get_boss_art(BossIdentity::Id("grothak"), BossScene::Encounter, "Dirt");
    assert_eq!(asset.expect("fallback art").filename, "boss_encounter.png");
}

#[test]
fn get_boss_art_resolves_dota_boss_by_id() {
    let (_, _, service) = service();
    assert!(
        service
            .get_boss_art(BossIdentity::Id("pudge"), BossScene::Encounter, "Dirt")
            .is_some()
    );
}

#[test]
fn get_boss_art_legacy_depth_still_resolves() {
    let (_, _, service) = service();
    assert!(
        service
            .get_boss_art(BossIdentity::LegacyDepth(25), BossScene::Encounter, "Dirt")
            .is_some()
    );
}

#[test]
fn get_boss_art_returns_none_for_unknown_boss() {
    let (_, _, service) = service();
    assert!(
        service
            .get_boss_art(BossIdentity::Id("not_a_boss"), BossScene::Encounter, "Dirt")
            .is_none()
    );
    assert!(
        service
            .get_boss_art(
                BossIdentity::LegacyDepth(9_999),
                BossScene::Encounter,
                "Dirt"
            )
            .is_none()
    );
}

#[test]
fn every_registered_boss_id_has_an_asset_slug() {
    assert!(boss_ids_missing_asset_slugs().is_empty());
}

#[test]
fn pinnacle_phase_cache_returns_fresh_attachments_from_generated_bytes() {
    let (_, renderer, service) = service_with_encounter();
    let first = service
        .get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
        .expect("first");
    let second = service
        .get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
        .expect("second");
    assert_eq!(first.bytes, second.bytes);
    assert_ne!(first.bytes.as_ptr(), second.bytes.as_ptr());
    assert_eq!(
        renderer.call_count(|request| matches!(request, RenderRequest::PinnaclePhase { .. })),
        1
    );
}

#[test]
fn pinnacle_phase_concurrent_misses_coalesce_and_return_fresh_attachments() {
    let (_, renderer, service) = service_with_encounter();
    renderer.block();
    let first_service = Arc::clone(&service);
    let first = thread::spawn(move || {
        first_service.get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
    });
    renderer.wait_started();
    let second_service = Arc::clone(&service);
    let second = thread::spawn(move || {
        second_service.get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
    });
    renderer.release();
    let first = first
        .join()
        .expect("first waiter does not panic")
        .expect("first art");
    let second = second
        .join()
        .expect("second waiter does not panic")
        .expect("second art");
    assert_eq!(first.bytes, second.bytes);
    assert_ne!(first.bytes.as_ptr(), second.bytes.as_ptr());
    assert_eq!(
        renderer.call_count(|request| matches!(request, RenderRequest::PinnaclePhase { .. })),
        1
    );
    assert_eq!(service.phase_cache_counts().2, 0);
}

#[test]
fn pinnacle_phase_dropped_waiter_does_not_cancel_shared_render() {
    let (_, renderer, service) = service_with_encounter();
    renderer.block();
    let rendering_service = Arc::clone(&service);
    let rendering = thread::spawn(move || {
        rendering_service.get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
    });
    renderer.wait_started();
    let cancelled_service = Arc::clone(&service);
    let cancelled = thread::spawn(move || {
        cancelled_service.get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
    });
    drop(cancelled);
    renderer.release();
    assert!(
        rendering
            .join()
            .expect("rendering waiter survives")
            .is_some()
    );
    assert_eq!(
        renderer.call_count(|request| matches!(request, RenderRequest::PinnaclePhase { .. })),
        1
    );
    assert_eq!(service.phase_cache_counts().2, 0);
}

#[test]
fn pinnacle_phase_missing_prebuilt_phase_files_are_not_required() {
    let (source, _, service) = service_with_encounter();
    let art = service
        .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
        .expect("runtime GIF");
    assert!(art.filename.ends_with(".gif"));
    assert!(!source.contains_fragment("phase"));
}

#[test]
fn pinnacle_phase_generation_failure_falls_back_to_static_encounter() {
    let (source, renderer, service) = service_with_encounter();
    renderer.fail();
    let art = service
        .get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
        .expect("static fallback");
    assert_eq!(art.filename, "boss_encounter.png");
    assert_eq!(
        art.bytes,
        source
            .read(Path::new("bosses/forgotten_king_encounter.png"))
            .expect("source art")
    );
}

#[test]
fn pinnacle_phase_missing_encounter_asset_falls_back_to_static_art() {
    let (_, _, service) = service();
    let art = service
        .get_pinnacle_phase_art("forgotten_king", 2, "Abyss", false)
        .expect("generated static fallback");
    assert_eq!(art.filename, "boss_encounter.png");
}

#[test]
fn pinnacle_phase_oversized_generation_falls_back_to_static_encounter() {
    let (source, renderer, service) = service_with_encounter();
    renderer.oversize();
    let art = service
        .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
        .expect("static fallback");
    assert_eq!(art.filename, "boss_encounter.png");
    assert_eq!(
        art.bytes,
        source
            .read(Path::new("bosses/forgotten_king_encounter.png"))
            .expect("source art")
    );
}

#[test]
fn pinnacle_phase_failed_generation_is_negatively_cached_failure() {
    let (_, renderer, service) = service_with_encounter();
    renderer.fail();
    assert!(
        service
            .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
            .is_some()
    );
    assert!(
        service
            .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
            .is_some()
    );
    assert_eq!(
        renderer.call_count(|request| matches!(request, RenderRequest::PinnaclePhase { .. })),
        1
    );
    assert_eq!(service.phase_cache_counts(), (0, 1, 0));
}

#[test]
fn pinnacle_phase_failed_generation_is_negatively_cached_oversize() {
    let (_, renderer, service) = service_with_encounter();
    renderer.oversize();
    assert!(
        service
            .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
            .is_some()
    );
    assert!(
        service
            .get_pinnacle_phase_art("forgotten_king", 3, "Abyss", false)
            .is_some()
    );
    assert_eq!(
        renderer.call_count(|request| matches!(request, RenderRequest::PinnaclePhase { .. })),
        1
    );
    assert_eq!(service.phase_cache_counts(), (0, 1, 0));
}

#[test]
fn prestige_two_boss_assets_are_consistently_sized_rgba_pngs() {
    for boss_id in NEW_PRESTIGE_TWO_BOSS_IDS {
        for scene in [BossScene::Encounter, BossScene::Victory, BossScene::Defeat] {
            assert_repository_png(
                &Path::new("bosses").join(format!("{boss_id}_{}.png", scene.as_str())),
                PINNACLE_SIZE,
                PixelMode::Rgba,
            );
        }
    }
}

#[test]
fn prestige_two_boss_loader_resolves_every_new_asset() {
    let service = repository_service();
    for boss_id in NEW_PRESTIGE_TWO_BOSS_IDS {
        for scene in [BossScene::Encounter, BossScene::Victory, BossScene::Defeat] {
            let path =
                repository_asset_root().join(format!("bosses/{boss_id}_{}.png", scene.as_str()));
            let asset = service
                .get_boss_art(BossIdentity::Id(boss_id), scene, "Stone")
                .expect("asset");
            assert_eq!(asset.filename, format!("boss_{}.png", scene.as_str()));
            assert_eq!(asset.bytes, std::fs::read(path).expect("source asset"));
        }
    }
}

#[test]
fn new_pinnacle_assets_are_compact_rgba_pngs() {
    let mut total = 0usize;
    for boss_id in NEW_PINNACLE_BOSS_IDS {
        for scene in [BossScene::Encounter, BossScene::Victory, BossScene::Defeat] {
            let size = assert_repository_png(
                &Path::new("bosses").join(format!("{boss_id}_{}.png", scene.as_str())),
                PINNACLE_SIZE,
                PixelMode::Rgba,
            );
            assert!(size <= PHASE_TWO_LIMIT);
            total += size;
        }
    }
    assert!(total <= 4 * 1024 * 1024);
}

#[test]
fn new_pinnacle_boss_loader_returns_each_source_asset() {
    let service = repository_service();
    for boss_id in NEW_PINNACLE_BOSS_IDS {
        for scene in [BossScene::Encounter, BossScene::Victory, BossScene::Defeat] {
            let asset = service
                .get_boss_art(BossIdentity::Id(boss_id), scene, "The Hollow")
                .expect("asset");
            assert_eq!(asset.filename, format!("boss_{}.png", scene.as_str()));
        }
    }
}

#[test]
fn get_layer_thumbnail_returns_attachment_for_known_layer() {
    let (_, _, service) = service();
    assert_eq!(
        service
            .get_layer_thumbnail("Dirt")
            .expect("thumbnail")
            .filename,
        "layer_dirt.png"
    );
}

#[test]
fn get_layer_thumbnail_returns_none_for_unknown_layer() {
    let (_, _, service) = service();
    assert!(service.get_layer_thumbnail("Nonexistent Layer").is_none());
}

#[test]
fn roguelike_event_assets_are_consistently_sized_rgb_pngs() {
    for event_id in ROGUELIKE_EVENT_ASSET_IDS {
        assert_repository_png(
            &Path::new("events").join(format!("{event_id}.png")),
            EVENT_ASSET_SIZE,
            PixelMode::Rgb,
        );
    }
}

#[test]
fn roguelike_event_loader_resolves_every_new_asset() {
    let service = repository_service();
    for event_id in ROGUELIKE_EVENT_ASSET_IDS {
        let asset = service
            .get_event_art(event_id, "Stone")
            .expect("event asset");
        assert_eq!(asset.filename, format!("event_{event_id}.png"));
    }
}

#[test]
fn pressure_event_assets_are_consistently_sized_rgb_pngs() {
    for event_id in PRESSURE_EVENT_ASSET_IDS {
        assert_repository_png(
            &Path::new("events").join(format!("{event_id}.png")),
            EVENT_ASSET_SIZE,
            PixelMode::Rgb,
        );
    }
}

#[test]
fn pressure_event_loader_resolves_every_new_asset() {
    let service = repository_service();
    for event_id in PRESSURE_EVENT_ASSET_IDS {
        let asset = service
            .get_event_art(event_id, "Stone")
            .expect("event asset");
        assert_eq!(asset.filename, format!("event_{event_id}.png"));
    }
}
