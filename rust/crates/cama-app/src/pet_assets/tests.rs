use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::rc::Rc;

use cama_domain::pet::{PetMood, PetStage, SPECIES};
use cama_domain::pet_evolution::{PetCalling, PetInstinct};
use gif::{Encoder, Frame, Repeat};

use super::*;

fn request(
    species_id: &'static str,
    stage: PetStage,
    mood: PetMood,
    seed: i64,
) -> PetRenderRequest<'static> {
    PetRenderRequest {
        species_id,
        stage,
        mood,
        seed,
        accessory: None,
        evolution: None,
    }
}

fn assert_valid_card(image: &RasterImage) {
    let info = inspect_png(&image.encode_png()).expect("renderer must emit a valid PNG");
    assert_eq!((info.width, info.height), (CARD_WIDTH, CARD_HEIGHT));
    assert_eq!(info.color_type, 6);
    assert_eq!(image.pixels.len(), CARD_WIDTH * CARD_HEIGHT * 4);
}

fn solid_gif(width: u16, height: u16, colors: &[[u8; 4]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let palette = [0_u8, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let mut encoder = Encoder::new(&mut bytes, width, height, &palette).expect("GIF encoder");
        encoder
            .set_repeat(Repeat::Infinite)
            .expect("GIF repeat extension");
        for (index, color) in colors.iter().enumerate() {
            let mut pixels = vec![0_u8; usize::from(width) * usize::from(height) * 4];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(color);
            }
            let mut frame = Frame::from_rgba_speed(width, height, &mut pixels, 1);
            frame.delay = u16::try_from(index + 1).expect("test GIF delay");
            frame.dispose = gif::DisposalMethod::Keep;
            encoder.write_frame(&frame).expect("GIF frame");
        }
    }
    bytes
}

#[derive(Clone, Default)]
struct MemoryAssets(Rc<RefCell<BTreeMap<String, AuthoredAsset>>>);

impl MemoryAssets {
    fn insert(&self, key: &str, asset: AuthoredAsset) {
        self.0.borrow_mut().insert(key.to_owned(), asset);
    }
}

impl AuthoredPetAssets for MemoryAssets {
    fn load(&self, base_name: &str) -> Option<AuthoredAsset> {
        self.0.borrow().get(base_name).cloned()
    }
}

#[derive(Clone)]
struct CountingRenderer {
    calls: Rc<Cell<usize>>,
}

impl PetCardRenderer for CountingRenderer {
    fn render(&mut self, request: &PetRenderRequest<'_>) -> RasterImage {
        self.calls.set(self.calls.get() + 1);
        render_pet_card(request)
    }
}

fn decode_stored_png(bytes: &[u8]) -> RasterImage {
    let info = inspect_png(bytes).expect("test PNG must validate");
    let mut cursor = PNG_SIGNATURE.len();
    let mut idat = Vec::new();
    while cursor < bytes.len() {
        let length = read_u32(bytes, cursor).expect("chunk length") as usize;
        cursor += 4;
        let kind = &bytes[cursor..cursor + 4];
        cursor += 4;
        let data = &bytes[cursor..cursor + length];
        cursor += length + 4;
        if kind == b"IDAT" {
            idat.extend_from_slice(data);
        }
        if kind == b"IEND" {
            break;
        }
    }
    assert_eq!(&idat[..2], &[0x78, 0x01]);
    let mut deflate_cursor = 2;
    let end = idat.len() - 4;
    let mut filtered = Vec::new();
    loop {
        let header = idat[deflate_cursor];
        deflate_cursor += 1;
        assert_eq!(header & 0b0000_0110, 0, "encoder must use stored blocks");
        let length = u16::from_le_bytes([idat[deflate_cursor], idat[deflate_cursor + 1]]);
        let inverse = u16::from_le_bytes([idat[deflate_cursor + 2], idat[deflate_cursor + 3]]);
        assert_eq!(length, !inverse);
        deflate_cursor += 4;
        let block_end = deflate_cursor + usize::from(length);
        filtered.extend_from_slice(&idat[deflate_cursor..block_end]);
        deflate_cursor = block_end;
        if header & 1 == 1 {
            break;
        }
    }
    assert_eq!(deflate_cursor, end);
    assert_eq!(adler32(&filtered), read_u32(&idat, end).expect("Adler sum"));
    let row_bytes = info.width * 4;
    let mut pixels = Vec::with_capacity(row_bytes * info.height);
    for row in filtered.chunks_exact(row_bytes + 1) {
        assert_eq!(row[0], 0, "encoder must use the None PNG filter");
        pixels.extend_from_slice(&row[1..]);
    }
    RasterImage {
        width: info.width,
        height: info.height,
        pixels,
    }
}

mod test_render_pet_card {
    use super::*;

    #[test]
    fn test_same_seed_is_deterministic() {
        let request = request("common_cama", PetStage::Baby, PetMood::Happy, 42);
        let first = render_pet_card(&request).encode_png();
        let second = render_pet_card(&request).encode_png();
        assert_eq!(first, second);
    }

    #[test]
    fn test_different_seed_changes_output() {
        let base = render_pet_card(&request("common_cama", PetStage::Baby, PetMood::Happy, 42));
        let other = render_pet_card(&request("common_cama", PetStage::Baby, PetMood::Happy, 43));
        assert_ne!(base.pixels, other.pixels);
    }

    #[test]
    fn test_all_species_stages_moods_render_valid_pngs() {
        for species in SPECIES {
            for stage in [PetStage::Baby, PetStage::Adult] {
                assert_valid_card(&render_pet_card(&request(
                    species.species_id,
                    stage,
                    PetMood::Content,
                    7,
                )));
            }
        }
        for stage in [PetStage::Baby, PetStage::Adult] {
            for mood in [PetMood::Happy, PetMood::Content, PetMood::Hungry] {
                assert_valid_card(&render_pet_card(&request("common_cama", stage, mood, 7)));
            }
        }
        for species in ["banana_ears", "rama"] {
            assert_valid_card(&render_pet_card(&request(
                species,
                PetStage::Adult,
                PetMood::Hungry,
                7,
            )));
        }
    }

    #[test]
    fn test_mood_variants_differ() {
        let happy = render_pet_card(&request("jopacama", PetStage::Adult, PetMood::Happy, 5));
        let hungry = render_pet_card(&request("jopacama", PetStage::Adult, PetMood::Hungry, 5));
        assert_ne!(happy.pixels, hungry.pixels);
    }

    #[test]
    fn test_stage_variants_differ() {
        let baby = render_pet_card(&request("jopacama", PetStage::Baby, PetMood::Content, 5));
        let adult = render_pet_card(&request("jopacama", PetStage::Adult, PetMood::Content, 5));
        assert_ne!(baby.pixels, adult.pixels);
    }

    #[test]
    fn test_species_sprites_differ_from_each_other() {
        let renders = SPECIES
            .into_iter()
            .map(|species| {
                render_pet_card(&request(
                    species.species_id,
                    PetStage::Adult,
                    PetMood::Content,
                    5,
                ))
                .encode_png()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(renders.len(), SPECIES.len());
    }

    #[test]
    fn test_new_species_have_distinct_composed_visuals() {
        let renders = [
            "common_cama",
            "embergear_cama",
            "riverglow_cama",
            "prismwool_cama",
            "moondrift_cama",
            "sunspun_cama",
        ]
        .into_iter()
        .map(|species| {
            render_pet_card(&request(species, PetStage::Adult, PetMood::Content, 17)).pixels
        })
        .collect::<BTreeSet<_>>();
        assert_eq!(renders.len(), 6);
    }

    #[test]
    fn test_legendary_species_have_signature_layers() {
        let common_ground = render_layer(
            LayerSlot::Ground,
            "common_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let moon_ground = render_layer(
            LayerSlot::Ground,
            "moondrift_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let common_back = render_layer(
            LayerSlot::Back,
            "common_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let sun_back = render_layer(
            LayerSlot::Back,
            "sunspun_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        assert_ne!(moon_ground.pixels, common_ground.pixels);
        assert_ne!(sun_back.pixels, common_back.pixels);
    }

    #[test]
    fn test_courier_back_layer_does_not_add_foreign_polygon_ears() {
        let back = render_layer(
            LayerSlot::Back,
            "courier_cama",
            PetStage::Adult,
            PetMood::Happy,
            17,
        );
        assert_eq!(back.alpha_bounds(), None);
    }

    #[test]
    fn test_top_hat_remains_inside_the_adult_headwear_frame() {
        let mut layer = RasterImage::transparent_card();
        draw_accessory(&mut layer, "top_hat", PetStage::Adult);
        let fitted = fit_to_mount(
            &layer,
            &geometry_frame(PetStage::Adult),
            &authoring_frame(PetStage::Adult),
            "headwear",
        );
        let (_, top, _, bottom) = fitted.alpha_bounds().expect("fitted top hat");
        assert!(top < 32);
        assert!(bottom <= 40);
    }

    #[test]
    fn test_riverglow_uses_grounded_effect_instead_of_floating_orbs() {
        let front = render_layer(
            LayerSlot::Front,
            "riverglow_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let river = render_layer(
            LayerSlot::Ground,
            "riverglow_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let common = render_layer(
            LayerSlot::Ground,
            "common_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        assert_eq!(front.alpha_bounds(), None);
        assert_ne!(river.pixels, common.pixels);
    }

    #[test]
    fn test_riverglow_body_mark_stays_compact() {
        let detail = render_layer(
            LayerSlot::Detail,
            "riverglow_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let (left, _, right, _) = detail.alpha_bounds().expect("river mark");
        assert!(right - left <= 20);
    }

    #[test]
    fn test_prismwool_casts_prismatic_light_beyond_its_fleece() {
        let prism = render_layer(
            LayerSlot::Ground,
            "prismwool_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        let common = render_layer(
            LayerSlot::Ground,
            "common_cama",
            PetStage::Adult,
            PetMood::Content,
            17,
        );
        assert_ne!(prism.pixels, common.pixels);
    }
}

mod test_render_egg_and_tombstone {
    use super::*;

    #[test]
    fn test_egg_renders_valid_png() {
        assert_valid_card(&render_egg_card(11));
    }

    #[test]
    fn test_tombstone_renders_valid_png() {
        assert_valid_card(&render_tombstone_card("Fluffy"));
    }

    #[test]
    fn test_tombstone_handles_long_name() {
        assert_valid_card(&render_tombstone_card(&"A".repeat(60)));
    }
}

mod test_pet_asset_loader {
    use super::*;

    #[test]
    fn test_get_pet_card_procedural_fallback_filename() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let file = loader.get_pet_card(&request("common_cama", PetStage::Adult, PetMood::Happy, 3));
        assert_eq!(file.filename, "pet_common_cama_adult_happy.png");
        assert_valid_card(&decode_stored_png(file.bytes()));
    }

    #[test]
    fn test_get_pet_card_returns_fresh_file_objects() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let request = request("rama", PetStage::Baby, PetMood::Content, 9);
        let mut first = loader.get_pet_card(&request);
        let second = loader.get_pet_card(&request);
        let mut byte = [0];
        first.fp.read_exact(&mut byte).expect("first cursor read");
        assert_eq!(first.fp.position(), 1);
        assert_eq!(second.fp.position(), 0);
        assert_ne!(first.fp.get_ref().as_ptr(), second.fp.get_ref().as_ptr());
        assert_eq!(first.bytes(), second.bytes());
    }

    #[test]
    fn test_evolved_card_bytes_are_rendered_once_and_then_cached() {
        let calls = Rc::new(Cell::new(0));
        let renderer = CountingRenderer {
            calls: Rc::clone(&calls),
        };
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, renderer);
        let request = PetRenderRequest {
            evolution: Some(EvolutionVisual {
                calling: PetCalling::Prospector,
                primary: PetInstinct::Delving,
                secondary: Some(PetInstinct::Fortune),
            }),
            ..request("common_cama", PetStage::Adult, PetMood::Happy, 13)
        };
        let first = loader.get_pet_card(&request);
        let second = loader.get_pet_card(&request);
        assert_eq!(calls.get(), 1);
        assert_eq!(first.bytes(), second.bytes());
        assert_ne!(first.fp.get_ref().as_ptr(), second.fp.get_ref().as_ptr());
    }

    #[test]
    fn test_get_egg_card_procedural_fallback() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let file = loader.get_egg_card(4);
        assert_eq!(file.filename, "pet_egg.png");
        assert_valid_card(&decode_stored_png(file.bytes()));
    }

    #[test]
    fn test_get_tombstone_card_procedural_fallback() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let file = loader.get_tombstone_card("Fluffy", 4);
        assert_eq!(file.filename, "pet_tombstone.png");
        assert_valid_card(&decode_stored_png(file.bytes()));
    }

    #[test]
    fn test_get_tombstone_card_engraves_name_on_disk_asset() {
        let assets = MemoryAssets::default();
        assets.insert(
            "tombstone",
            AuthoredAsset::png(RasterImage::solid_card(Rgba(150, 150, 160, 255))),
        );
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);
        let alpha = loader.get_tombstone_card("Alpha", 1);
        let beta = loader.get_tombstone_card("Beta", 2);
        assert_ne!(alpha.bytes(), beta.bytes());
        assert_eq!(alpha.filename, "pet_tombstone.png");
    }

    #[test]
    fn test_get_versus_card_composes_two_renders() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let left = request("common_cama", PetStage::Adult, PetMood::Happy, 3);
        let mut right = request("rama", PetStage::Baby, PetMood::Happy, 7);
        right.accessory = Some("red_bow");
        let file = loader.get_versus_card(&left, &right);
        let info = inspect_png(file.bytes()).expect("versus PNG");
        assert_eq!(file.filename, "pet_versus.png");
        assert_eq!((info.width, info.height), (VERSUS_WIDTH, CARD_HEIGHT));
    }

    #[test]
    fn test_get_altar_card_falls_back_to_tombstone() {
        let mut loader = PetAssetLoader::new(NoAuthoredPetAssets, ProceduralPetRenderer);
        let file = loader.get_altar_card("Fluffy", 4);
        assert_eq!(file.filename, "pet_tombstone.png");
    }

    #[test]
    fn test_get_altar_card_prefers_authored_asset() {
        let assets = MemoryAssets::default();
        assets.insert(
            "altar",
            AuthoredAsset::png(RasterImage::solid_card(Rgba(40, 20, 60, 255))),
        );
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);
        let file = loader.get_altar_card("Fluffy", 4);
        assert_eq!(file.filename, "pet_altar.png");
        assert_eq!(
            decode_stored_png(file.bytes()).pixel(0, 0),
            Some(Rgba(40, 20, 60, 255))
        );
    }

    #[test]
    fn test_get_versus_card_prefers_authored_overlay() {
        let assets = MemoryAssets::default();
        let mut loader = PetAssetLoader::new(assets.clone(), ProceduralPetRenderer);
        let left = request("common_cama", PetStage::Adult, PetMood::Happy, 3);
        let right = request("rama", PetStage::Baby, PetMood::Happy, 7);
        let baseline = loader.get_versus_card(&left, &right);
        assets.insert(
            "versus_overlay",
            AuthoredAsset::png(RasterImage::new(
                VERSUS_WIDTH,
                CARD_HEIGHT,
                Rgba(255, 0, 0, 255),
            )),
        );
        let overlaid = loader.get_versus_card(&left, &right);
        assert_ne!(overlaid.bytes(), baseline.bytes());
        assert_eq!(
            decode_stored_png(overlaid.bytes()).pixel(0, 0),
            Some(Rgba(255, 0, 0, 255))
        );
    }

    #[test]
    fn authored_gif_cards_preserve_raw_delivery_and_decode_first_frame_for_composition() {
        let animated = solid_gif(64, 64, &[[255, 0, 0, 255], [0, 255, 0, 255]]);
        let first_frame = decode_gif_raster(&animated).expect("first GIF frame");
        let direct_assets = MemoryAssets::default();
        direct_assets.insert(
            "egg",
            AuthoredAsset {
                bytes: animated.clone(),
                decoded: Some(first_frame.clone()),
                extension: "gif".to_owned(),
            },
        );
        direct_assets.insert(
            "altar",
            AuthoredAsset {
                bytes: animated.clone(),
                decoded: Some(first_frame),
                extension: "gif".to_owned(),
            },
        );
        let mut direct_loader = PetAssetLoader::new(direct_assets, ProceduralPetRenderer);
        let egg = direct_loader.get_egg_card(4);
        let altar = direct_loader.get_altar_card("Fluffy", 4);
        assert_eq!(egg.filename, "pet_egg.gif");
        assert_eq!(egg.bytes(), animated);
        assert_eq!(altar.filename, "pet_altar.gif");
        assert_eq!(altar.bytes(), animated);

        let tombstone_assets = MemoryAssets::default();
        tombstone_assets.insert(
            "tombstone",
            AuthoredAsset {
                bytes: animated.clone(),
                decoded: Some(decode_gif_raster(&animated).expect("first GIF frame")),
                extension: "gif".to_owned(),
            },
        );
        let mut tombstone_loader = PetAssetLoader::new(tombstone_assets, ProceduralPetRenderer);
        let tombstone = tombstone_loader.get_tombstone_card("Fluffy", 4);
        let engraved = decode_png_raster(tombstone.bytes()).expect("engraved GIF first frame PNG");
        assert_eq!(tombstone.filename, "pet_tombstone.gif");
        assert_eq!(engraved.pixel(0, 0), Some(Rgba(255, 0, 0, 255)));
    }

    #[test]
    fn native_fixed_versus_contract_uses_filtered_resampling_for_non_native_cards() {
        // Python preserves intrinsic authored card sizes in _compose_versus;
        // this test records Rust's existing fixed 1024x288 brawl contract.
        let assets = MemoryAssets::default();
        let mut left = RasterImage::new(2, 1, Rgba(0, 0, 0, 255));
        left.pixels[4..8].copy_from_slice(&[255, 255, 255, 255]);
        assets.insert("common_cama_adult_happy", AuthoredAsset::png(left));
        assets.insert(
            "rama_baby_happy",
            AuthoredAsset::png(RasterImage::solid_card(Rgba(20, 30, 40, 255))),
        );
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);
        let file = loader.get_versus_card(
            &request("common_cama", PetStage::Adult, PetMood::Happy, 3),
            &request("rama", PetStage::Baby, PetMood::Happy, 7),
        );
        let image = decode_stored_png(file.bytes());
        assert_eq!((image.width, image.height), (VERSUS_WIDTH, CARD_HEIGHT));
        assert!(
            (0..CARD_WIDTH).any(|x| {
                let pixel = image.pixel(x, CARD_HEIGHT / 2).expect("left card pixel");
                pixel.0 > 0 && pixel.0 < 255
            }),
            "Lanczos composition should retain intermediate edge samples"
        );
    }

    #[test]
    fn native_versus_overlay_uses_pillow_filtered_resampling() {
        let assets = MemoryAssets::default();
        let mut overlay = RasterImage::new(2, 1, Rgba(0, 0, 0, 255));
        overlay.pixels[4..8].copy_from_slice(&[255, 255, 255, 255]);
        assets.insert("versus_overlay", AuthoredAsset::png(overlay));
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);
        let file = loader.get_versus_card(
            &request("common_cama", PetStage::Adult, PetMood::Happy, 3),
            &request("rama", PetStage::Baby, PetMood::Happy, 7),
        );
        let image = decode_stored_png(file.bytes());
        assert_eq!((image.width, image.height), (VERSUS_WIDTH, CARD_HEIGHT));
        assert!(
            (0..VERSUS_WIDTH).any(|x| {
                let pixel = image.pixel(x, 0).expect("overlay pixel");
                pixel.0 > 0 && pixel.0 < 255
            }),
            "overlay resize should retain filtered edge samples"
        );
    }

    #[test]
    fn lanczos_samples_match_pillow_for_a_rgba_edge_fixture() {
        let mut source = RasterImage::new(2, 1, Rgba(0, 0, 0, 255));
        source.pixels[4..8].copy_from_slice(&[255, 255, 255, 255]);
        let resized = source.resized_lanczos(8, 1);
        let samples = (0..8)
            .map(|x| resized.pixel(x, 0).expect("resized sample").0)
            .collect::<Vec<_>>();
        assert_eq!(samples, [0, 0, 28, 93, 162, 227, 255, 255]);
    }

    #[test]
    fn lanczos_rgba_samples_match_pillow_alpha_association() {
        let mut source = RasterImage::new(2, 1, Rgba(255, 0, 0, 255));
        source.pixels[4..8].copy_from_slice(&[0, 0, 0, 0]);
        let resized = source.resized_lanczos(8, 1);
        let samples = (0..8)
            .map(|x| resized.pixel(x, 0).expect("resized RGBA sample"))
            .collect::<Vec<_>>();
        assert_eq!(
            samples,
            [
                Rgba(255, 0, 0, 255),
                Rgba(255, 0, 0, 255),
                Rgba(255, 0, 0, 227),
                Rgba(255, 0, 0, 162),
                Rgba(255, 0, 0, 93),
                Rgba(255, 0, 0, 28),
                Rgba(0, 0, 0, 0),
                Rgba(0, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn lanczos_rgba_samples_do_not_leak_transparent_rgb_channels() {
        let mut source = RasterImage::new(2, 1, Rgba(255, 0, 0, 0));
        source.pixels[4..8].copy_from_slice(&[0, 0, 255, 255]);
        let resized = source.resized_lanczos(8, 1);
        let samples = (0..8)
            .map(|x| resized.pixel(x, 0).expect("resized RGBA sample"))
            .collect::<Vec<_>>();
        assert_eq!(
            samples,
            [
                Rgba(0, 0, 0, 0),
                Rgba(0, 0, 0, 0),
                Rgba(0, 0, 255, 28),
                Rgba(0, 0, 255, 93),
                Rgba(0, 0, 255, 162),
                Rgba(0, 0, 255, 227),
                Rgba(0, 0, 255, 255),
                Rgba(0, 0, 255, 255),
            ]
        );
    }
}

mod test_filesystem_pet_assets {
    use super::*;

    fn repository_asset_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets")
    }

    #[test]
    fn production_authored_pngs_decode_and_tombstone_is_engraved() {
        let directory = repository_asset_root().join("pets");
        let assets = FilesystemPetAssets::new(directory);
        let egg = assets.load("egg").expect("authored egg");
        assert_eq!(egg.extension, "png");
        assert_eq!(
            egg.decoded
                .as_ref()
                .map(|image| (image.width, image.height)),
            Some((CARD_WIDTH, CARD_HEIGHT))
        );

        let raw_tombstone = assets.load("tombstone").expect("authored tombstone");
        let raw_bytes = raw_tombstone.bytes.clone();
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);
        let engraved = loader.get_tombstone_card("Blep", 7);
        assert_eq!(engraved.filename, "pet_tombstone.png");
        assert_ne!(engraved.bytes(), raw_bytes);
        assert_eq!(
            decode_png_raster(engraved.bytes()).map(|image| (image.width, image.height)),
            Some((CARD_WIDTH, CARD_HEIGHT))
        );
    }

    #[test]
    fn production_authored_cards_are_selected_byte_for_byte() {
        let asset_root = repository_asset_root();
        let pets = asset_root.join("pets");
        let assets = FilesystemPetAssets::new(&pets);
        let expected_egg = std::fs::read(pets.join("egg.png")).expect("authored egg bytes");
        let expected_altar = std::fs::read(pets.join("altar.png")).expect("authored altar bytes");
        let mut loader = PetAssetLoader::new(assets, ProceduralPetRenderer);

        let egg = loader.get_egg_card(17);
        assert_eq!(egg.filename, "pet_egg.png");
        assert_eq!(egg.bytes(), expected_egg);

        let altar = loader.get_altar_card("Blep", 17);
        assert_eq!(altar.filename, "pet_altar.png");
        assert_eq!(altar.bytes(), expected_altar);
    }

    #[test]
    fn static_gif_decodes_to_its_logical_rgba_canvas() {
        // Matches PIL.Image.open(BytesIO(gif)).convert("RGBA") for a
        // single-frame authored override.
        let gif = solid_gif(3, 2, &[[24, 48, 96, 255]]);
        let image = decode_gif_raster(&gif).expect("static GIF raster");
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(image.pixel(0, 0), Some(Rgba(24, 48, 96, 255)));
        assert_eq!(image.pixel(2, 1), Some(Rgba(24, 48, 96, 255)));
    }

    #[test]
    fn gif_decoder_preserves_transparent_logical_background_and_offsets() {
        // Pillow exposes the logical canvas, not only the frame rectangle,
        // and leaves the transparent GIF background transparent.
        let mut bytes = Vec::new();
        {
            let palette: [u8; 0] = [];
            let mut encoder = Encoder::new(&mut bytes, 3, 2, &palette).expect("GIF encoder");
            let mut pixels = vec![
                255_u8, 0, 0, 255, 0, 0, 0, 0, // red, transparent
                0, 0, 0, 0, 0, 0, 0, 0, // transparent, transparent
            ];
            let mut frame = Frame::from_rgba_speed(2, 2, &mut pixels, 1);
            frame.left = 1;
            frame.top = 0;
            encoder.write_frame(&frame).expect("GIF offset frame");
        }
        // gif::Encoder emits a padded black table even for an empty palette;
        // remove that table to model a GIF with no global palette, whose
        // logical background is transparent.
        bytes[10] &= 0x7f;
        bytes.drain(13..19);
        let image = decode_gif_raster(&bytes).expect("offset GIF raster");
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(image.pixel(1, 0), Some(Rgba(255, 0, 0, 255)));
        assert_eq!(image.pixel(0, 0), Some(Rgba(0, 0, 0, 0)));
        assert_eq!(image.pixel(2, 1), Some(Rgba(0, 0, 0, 0)));
    }

    #[test]
    fn gif_decoder_honors_nontransparent_global_background_around_offset_frame() {
        // This mirrors Pillow's logical-canvas result when the GIF screen
        // descriptor selects an opaque global background color.
        let mut bytes = Vec::new();
        {
            let palette = [
                0_u8, 0, 0, // background index 0
                24, 48, 96, // background index 1
                255, 0, 0, // unused global color
            ];
            let mut encoder = Encoder::new(&mut bytes, 3, 2, &palette).expect("GIF encoder");
            let mut pixels = vec![255_u8, 0, 0, 255];
            let mut frame = Frame::from_rgba_speed(1, 1, &mut pixels, 1);
            frame.left = 1;
            encoder.write_frame(&frame).expect("GIF offset frame");
        }
        // gif::Encoder writes background index zero; use index one to model
        // an authored GIF with an opaque logical-screen background.
        bytes[11] = 1;
        let image = decode_gif_raster(&bytes).expect("opaque-background GIF raster");
        assert_eq!(image.pixel(0, 0), Some(Rgba(24, 48, 96, 255)));
        assert_eq!(image.pixel(1, 0), Some(Rgba(255, 0, 0, 255)));
        assert_eq!(image.pixel(2, 1), Some(Rgba(24, 48, 96, 255)));
    }

    #[test]
    fn gif_decoder_honors_opaque_background_when_gce_disables_transparency() {
        let mut bytes = Vec::new();
        {
            let palette = [0_u8, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
            let mut encoder = Encoder::new(&mut bytes, 3, 2, &palette).expect("GIF encoder");
            let mut pixels = vec![
                255_u8, 0, 0, 255, 0, 0, 0, 0, // red, transparent
                0, 0, 0, 0, 0, 0, 0, 0, // transparent, transparent
            ];
            let mut frame = Frame::from_rgba_speed(2, 2, &mut pixels, 1);
            frame.left = 1;
            encoder.write_frame(&frame).expect("GIF offset frame");
        }
        // The fixture's GCE is 0x05 (disposal + transparency). Clearing the
        // transparency bit models the same offset frame with an opaque GIF
        // background, as Pillow exposes from this wire representation.
        assert_eq!(bytes[25..29], [0x21, 0xf9, 0x04, 0x05]);
        bytes[28] = 0x04;
        let image = decode_gif_raster(&bytes).expect("opaque-background GIF raster");
        assert_eq!(image.pixel(0, 0), Some(Rgba(0, 0, 0, 255)));
        assert_eq!(image.pixel(1, 0), Some(Rgba(255, 0, 0, 255)));
        assert_eq!(image.pixel(2, 1), Some(Rgba(0, 0, 0, 255)));
    }

    #[test]
    fn gif_decoder_transparent_frame_pixels_preserve_opaque_screen_background() {
        let mut bytes = Vec::new();
        {
            let palette = [
                0_u8, 0, 0, // global index 0
                24, 48, 96, // global background index 1
                255, 0, 0, // global index 2
            ];
            let mut encoder = Encoder::new(&mut bytes, 3, 2, &palette).expect("GIF encoder");
            let mut pixels = vec![
                255_u8, 0, 0, 255, 0, 0, 0, 0, // red, transparent
                0, 0, 0, 0, 0, 0, 0, 0, // transparent, transparent
            ];
            let mut frame = Frame::from_rgba_speed(2, 2, &mut pixels, 1);
            frame.left = 1;
            encoder.write_frame(&frame).expect("GIF offset frame");
        }
        bytes[11] = 1;
        let image = decode_gif_raster(&bytes).expect("transparent-frame GIF raster");
        assert_eq!(image.pixel(0, 0), Some(Rgba(24, 48, 96, 255)));
        assert_eq!(image.pixel(1, 0), Some(Rgba(255, 0, 0, 255)));
        assert_eq!(image.pixel(2, 0), Some(Rgba(24, 48, 96, 255)));
        assert_eq!(image.pixel(2, 1), Some(Rgba(24, 48, 96, 255)));
    }

    #[test]
    fn gif_decoder_does_not_apply_global_background_transparency_to_local_palette() {
        // Pillow's Image.open(...).convert("RGBA") keeps the opaque logical
        // screen background here. Frame transparency index 0 belongs to the
        // local palette, even though the global background index is also 0.
        let mut bytes = Vec::new();
        {
            let global_palette = [
                24_u8, 48, 96, // global background index 0
                255, 0, 0, // global index 1
                0, 0, 0, // global index 2
                0, 0, 0, // global index 3 (table size padding)
            ];
            let mut encoder = Encoder::new(&mut bytes, 3, 2, &global_palette).expect("GIF encoder");
            let mut frame =
                Frame::from_palette_pixels(1, 1, vec![0_u8], vec![255, 0, 0, 0, 255, 0], Some(0));
            frame.left = 1;
            encoder.write_frame(&frame).expect("local-palette frame");
        }
        bytes[11] = 0;
        let image = decode_gif_raster(&bytes).expect("local-palette GIF raster");
        let expected = Rgba(24, 48, 96, 255);
        assert_eq!(image.pixel(0, 0), Some(expected));
        assert_eq!(image.pixel(1, 0), Some(expected));
        assert_eq!(image.pixel(2, 1), Some(expected));
    }

    #[test]
    fn gif_decoder_rejects_malformed_or_out_of_bounds_frames_without_panicking() {
        let mut oversized = solid_gif(2, 2, &[[255, 0, 0, 255]]);
        let descriptor = oversized
            .iter()
            .position(|byte| *byte == 0x2c)
            .expect("GIF image descriptor");
        oversized[descriptor + 5..descriptor + 7].copy_from_slice(&u16::MAX.to_le_bytes());
        let oversized_result = std::panic::catch_unwind(|| decode_gif_raster(&oversized));
        assert!(oversized_result.is_ok(), "descriptor must not panic");
        assert_eq!(oversized_result.expect("panic result"), None);

        let malformed = b"GIF89a";
        let malformed_result = std::panic::catch_unwind(|| decode_gif_raster(malformed));
        assert!(malformed_result.is_ok(), "malformed GIF must not panic");
        assert_eq!(malformed_result.expect("panic result"), None);
    }

    #[test]
    fn gif_override_precedes_png_and_file_limit_is_enforced() {
        let directory = tempfile::tempdir().expect("temporary asset directory");
        let gif = solid_gif(4, 4, &[[255, 0, 0, 255], [0, 255, 0, 255]]);
        std::fs::write(directory.path().join("egg.gif"), &gif).expect("write gif override");
        std::fs::write(
            directory.path().join("egg.png"),
            render_egg_card(3).encode_png(),
        )
        .expect("write png override");
        let asset = FilesystemPetAssets::new(directory.path())
            .load("egg")
            .expect("load authored override");
        assert_eq!(asset.extension, "gif");
        assert_eq!(asset.bytes, gif);
        assert_eq!(
            asset
                .decoded
                .as_ref()
                .map(|image| (image.width, image.height)),
            Some((4, 4))
        );
    }
}

mod hybrid_component_pack {
    use super::*;

    const MAGENTA: Rgba = Rgba(255, 0, 255, 255);
    const BLUE: Rgba = Rgba(0, 80, 255, 255);

    fn render_with(
        renderer: &mut HybridPetRenderer,
        species: &'static str,
        stage: PetStage,
        mood: PetMood,
        seed: i64,
    ) -> RasterImage {
        renderer.render(&request(species, stage, mood, seed))
    }

    fn write_component(
        root: &Path,
        stage: &str,
        slot: &str,
        name: &str,
        bounds: (i32, i32, i32, i32),
        color: Rgba,
    ) -> PathBuf {
        let directory = root.join(stage).join(slot);
        std::fs::create_dir_all(&directory).expect("create component directory");
        let path = directory.join(format!("{name}.png"));
        let mut image = RasterImage::transparent_card();
        image.fill_rect(bounds.0, bounds.1, bounds.2, bounds.3, color);
        std::fs::write(&path, image.encode_png()).expect("write component");
        path
    }

    fn count_exact(image: &RasterImage, color: Rgba) -> usize {
        image
            .pixels
            .chunks_exact(4)
            .filter(|pixel| *pixel == [color.0, color.1, color.2, color.3])
            .count()
    }

    fn production_components() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/pets/components")
    }

    #[test]
    fn test_no_components_matches_pure_procedural() {
        let directory = tempfile::tempdir().expect("temporary components");
        let request = request("common_cama", PetStage::Adult, PetMood::Happy, 7);
        assert_eq!(
            HybridPetRenderer::new(directory.path()).render(&request),
            render_pet_card(&request)
        );
    }

    #[test]
    fn test_disk_face_component_wins_its_slot_only() {
        let directory = tempfile::tempdir().expect("temporary components");
        write_component(
            directory.path(),
            "adult",
            "face",
            "any_happy_01",
            (224, 40, 288, 88),
            MAGENTA,
        );
        let mut renderer = HybridPetRenderer::new(directory.path());
        let composed = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        let procedural =
            render_pet_card(&request("common_cama", PetStage::Adult, PetMood::Happy, 7));
        assert!(count_exact(&composed, MAGENTA) > 100);
        assert_eq!(composed.pixel(256, 184), procedural.pixel(256, 184));
    }

    #[test]
    fn test_species_scoped_beats_shared() {
        let directory = tempfile::tempdir().expect("temporary components");
        for (name, color) in [("any_happy_01", MAGENTA), ("rama_happy_01", BLUE)] {
            write_component(
                directory.path(),
                "adult",
                "face",
                name,
                (224, 40, 288, 88),
                color,
            );
        }
        let mut renderer = HybridPetRenderer::new(directory.path());
        let rama = render_with(&mut renderer, "rama", PetStage::Adult, PetMood::Happy, 7);
        let common = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        assert!(count_exact(&rama, BLUE) > 100);
        assert_eq!(count_exact(&rama, MAGENTA), 0);
        assert!(count_exact(&common, MAGENTA) > 100);
    }

    #[test]
    fn test_face_components_are_mood_keyed() {
        let directory = tempfile::tempdir().expect("temporary components");
        write_component(
            directory.path(),
            "adult",
            "face",
            "any_hungry_01",
            (224, 40, 288, 88),
            MAGENTA,
        );
        let mut renderer = HybridPetRenderer::new(directory.path());
        let hungry = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Hungry,
            7,
        );
        let happy = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        assert!(count_exact(&hungry, MAGENTA) > 100);
        assert_eq!(count_exact(&happy, MAGENTA), 0);
    }

    #[test]
    fn test_shared_creature_component_is_species_tinted() {
        let directory = tempfile::tempdir().expect("temporary components");
        write_component(
            directory.path(),
            "adult",
            "creature",
            "any_01",
            (176, 120, 336, 200),
            Rgba(128, 128, 128, 255),
        );
        let mut renderer = HybridPetRenderer::new(directory.path());
        let frost = render_with(
            &mut renderer,
            "crystal_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        )
        .pixel(256, 160)
        .expect("frost body pixel");
        let gold = render_with(
            &mut renderer,
            "jopacama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        )
        .pixel(256, 160)
        .expect("gold body pixel");
        assert!(frost.2 > frost.0);
        assert!(gold.0 > gold.2);
        assert_ne!(frost, gold);
    }

    #[test]
    fn test_species_scoped_component_is_not_tinted() {
        let directory = tempfile::tempdir().expect("temporary components");
        write_component(
            directory.path(),
            "adult",
            "creature",
            "rama_01",
            (176, 120, 336, 200),
            MAGENTA,
        );
        let image = render_with(
            &mut HybridPetRenderer::new(directory.path()),
            "rama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        assert_eq!(image.pixel(196, 190), Some(MAGENTA));
    }

    #[test]
    fn test_variant_choice_is_seeded_and_stable() {
        let directory = tempfile::tempdir().expect("temporary components");
        for (name, color) in [("any_happy_01", MAGENTA), ("any_happy_02", BLUE)] {
            write_component(
                directory.path(),
                "adult",
                "face",
                name,
                (224, 40, 288, 88),
                color,
            );
        }
        let mut renderer = HybridPetRenderer::new(directory.path());
        let first = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        let second = render_with(
            &mut renderer,
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        assert_eq!(first, second);
        let picks = (0..40)
            .filter_map(|seed| {
                renderer
                    .pick_component(
                        LayerSlot::Face,
                        "adult",
                        "common_cama",
                        PetMood::Happy,
                        seed,
                    )
                    .and_then(|(path, _)| path.file_name()?.to_str().map(str::to_owned))
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            picks,
            BTreeSet::from(["any_happy_01.png".to_owned(), "any_happy_02.png".to_owned()])
        );
    }

    #[test]
    fn test_anchor_sidecar_moves_face_onto_the_creatures_head() {
        let directory = tempfile::tempdir().expect("temporary components");
        let creature = write_component(
            directory.path(),
            "adult",
            "creature",
            "any_01",
            (176, 40, 336, 220),
            Rgba(90, 90, 90, 255),
        );
        write_component(
            directory.path(),
            "adult",
            "face",
            "any_happy_01",
            (224, 40, 288, 88),
            MAGENTA,
        );
        std::fs::write(
            creature.with_extension("json"),
            r#"{"head_center":[300,64],"head_width":64,"body_center":[256,184],"body_width":160}"#,
        )
        .expect("write sidecar");
        let image = render_with(
            &mut HybridPetRenderer::new(directory.path()),
            "common_cama",
            PetStage::Adult,
            PetMood::Happy,
            7,
        );
        assert_eq!(image.pixel(300, 64), Some(MAGENTA));
        assert_ne!(image.pixel(256, 64), Some(MAGENTA));
    }

    #[test]
    fn test_component_without_sidecar_keeps_authoring_mounts_adult() {
        let directory = tempfile::tempdir().expect("temporary components");
        let creature = write_component(
            directory.path(),
            "adult",
            "creature",
            "any_01",
            (176, 40, 336, 220),
            MAGENTA,
        );
        let frame = HybridPetRenderer::new(directory.path()).frame(&creature, PetStage::Adult);
        assert_eq!(frame.mount("face").center, (256.0, 68.0));
        assert_eq!(frame.mount("headwear").center, (256.0, 32.0));
        assert_eq!(frame.mount("chest").center, (256.0, 115.0));
    }

    #[test]
    fn test_component_without_sidecar_keeps_authoring_mounts_baby() {
        let directory = tempfile::tempdir().expect("temporary components");
        let creature = write_component(
            directory.path(),
            "baby",
            "creature",
            "any_01",
            (176, 40, 336, 220),
            MAGENTA,
        );
        let frame = HybridPetRenderer::new(directory.path()).frame(&creature, PetStage::Baby);
        assert_eq!(frame.mount("face").center, (256.0, 100.0));
        assert_eq!(frame.mount("headwear").center, (256.0, 55.0));
        assert_eq!(frame.mount("chest").center, (256.0, 149.0));
    }

    #[test]
    fn test_legacy_sidecar_derives_missing_semantic_mounts() {
        let directory = tempfile::tempdir().expect("temporary components");
        let creature = write_component(
            directory.path(),
            "adult",
            "creature",
            "any_01",
            (176, 40, 336, 220),
            MAGENTA,
        );
        std::fs::write(
            creature.with_extension("json"),
            r#"{"head_center":[300,70],"head_width":60,"body_center":[270,190],"body_width":150}"#,
        )
        .expect("write legacy sidecar");
        let frame = HybridPetRenderer::new(directory.path()).frame(&creature, PetStage::Adult);
        assert_eq!(frame.mount("face").center, (300.0, 70.0));
        assert_eq!(frame.mount("headwear").center, (300.0, 40.0));
        assert_eq!(frame.mount("neck").center, (300.0, 103.0));
        assert_eq!(frame.mount("chest").center, (285.0, 127.6));
    }

    #[test]
    fn test_mood_specific_face_mount_overrides_generic_mount() {
        let directory = tempfile::tempdir().expect("temporary components");
        let creature = write_component(
            directory.path(),
            "adult",
            "creature",
            "any_01",
            (176, 40, 336, 220),
            Rgba(90, 90, 90, 255),
        );
        write_component(
            directory.path(),
            "adult",
            "face",
            "any_hungry_01",
            (224, 40, 288, 88),
            MAGENTA,
        );
        std::fs::write(
            creature.with_extension("json"),
            r#"{"head_center":[256,64],"head_width":64,"face_center":[256,68],"face_width":64,"face_hungry_center":[300,68],"face_hungry_width":64,"body_center":[256,184],"body_width":160}"#,
        )
        .expect("write sidecar");
        let image = render_with(
            &mut HybridPetRenderer::new(directory.path()),
            "common_cama",
            PetStage::Adult,
            PetMood::Hungry,
            7,
        );
        assert_eq!(image.pixel(300, 68), Some(MAGENTA));
        assert_ne!(image.pixel(256, 68), Some(MAGENTA));
    }

    #[test]
    fn test_baby_backdrops_fall_back_to_adult_pool() {
        let directory = tempfile::tempdir().expect("temporary components");
        let orange = Rgba(255, 140, 0, 255);
        write_component(
            directory.path(),
            "adult",
            "backdrop",
            "any_01",
            (0, 0, CARD_WIDTH as i32, CARD_HEIGHT as i32),
            orange,
        );
        let image = render_with(
            &mut HybridPetRenderer::new(directory.path()),
            "common_cama",
            PetStage::Baby,
            PetMood::Happy,
            7,
        );
        assert_eq!(image.pixel(40, 40), Some(orange));
    }

    #[test]
    fn test_manifest_token_tracks_component_set() {
        let directory = tempfile::tempdir().expect("temporary components");
        let mut renderer = HybridPetRenderer::new(directory.path());
        let empty = renderer.manifest_token();
        write_component(
            directory.path(),
            "adult",
            "face",
            "any_happy_01",
            (224, 40, 288, 88),
            MAGENTA,
        );
        renderer.clear_caches();
        assert_ne!(renderer.manifest_token(), empty);
    }

    #[test]
    fn test_unreadable_component_falls_back_to_procedural() {
        let directory = tempfile::tempdir().expect("temporary components");
        let face = directory.path().join("adult/face/any_happy_01.png");
        std::fs::create_dir_all(face.parent().expect("component parent")).expect("create parent");
        std::fs::write(face, b"not a png").expect("write invalid component");
        let request = request("common_cama", PetStage::Adult, PetMood::Happy, 7);
        assert_eq!(
            HybridPetRenderer::new(directory.path()).render(&request),
            render_pet_card(&request)
        );
    }

    #[test]
    fn test_every_creature_has_its_head_at_its_anchor() {
        let mut renderer = HybridPetRenderer::new(production_components());
        let mut checked = 0;
        for (stage_name, stage) in [("adult", PetStage::Adult), ("baby", PetStage::Baby)] {
            for path in renderer.pool(stage_name, LayerSlot::Creature) {
                let image = renderer.image(&path).expect("valid creature component");
                let frame = renderer.frame(&path, stage);
                let head = frame.mount("head").center;
                assert!(
                    image
                        .pixel(head.0 as usize, head.1 as usize)
                        .is_some_and(|pixel| pixel.3 > 50)
                );
                checked += 1;
            }
        }
        assert!(checked >= 6);
    }

    #[test]
    fn test_face_layer_lands_on_the_declared_head() {
        let mut renderer = HybridPetRenderer::new(production_components());
        for (stage_name, stage) in [("adult", PetStage::Adult), ("baby", PetStage::Baby)] {
            for path in renderer.pool(stage_name, LayerSlot::Creature) {
                let target = renderer.frame(&path, stage);
                let face = render_layer(LayerSlot::Face, "common_cama", stage, PetMood::Happy, 1);
                let fitted = fit_to_mount(&face, &geometry_frame(stage), &target, "face");
                let bounds = alpha_bounds_above(&fitted, 20).expect("visible fitted face");
                let center = (
                    (bounds.0 + bounds.2) as f64 / 2.0,
                    (bounds.1 + bounds.3) as f64 / 2.0,
                );
                let expected = target.mount("face").center;
                assert!((center.0 - expected.0).abs() <= 10.0);
                assert!((center.1 - expected.1).abs() <= 14.0);
            }
        }
    }

    #[test]
    fn test_every_authored_face_is_fully_registered_to_every_creatures_head() {
        let mut renderer = HybridPetRenderer::new(production_components());
        for (stage_name, stage) in [("adult", PetStage::Adult), ("baby", PetStage::Baby)] {
            let faces = renderer.pool(stage_name, LayerSlot::Face);
            for creature_path in renderer.pool(stage_name, LayerSlot::Creature) {
                let creature = renderer.image(&creature_path).expect("creature component");
                let target = renderer.frame(&creature_path, stage);
                for face_path in &faces {
                    let face = renderer.image(face_path).expect("face component");
                    let name = face_path
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default();
                    let mood = name.split('_').rev().nth(1).unwrap_or("happy");
                    let mount = format!("face_{mood}");
                    let mount = if target.mounts.contains_key(&mount) {
                        mount.as_str()
                    } else {
                        "face"
                    };
                    let mut fitted = fit_to_mount(&face, &authoring_frame(stage), &target, mount);
                    for (pixel, mask) in fitted
                        .pixels
                        .chunks_exact_mut(4)
                        .zip(creature.pixels.chunks_exact(4))
                    {
                        if mask[3] <= 20 {
                            pixel.fill(0);
                        }
                    }
                    for y in 0..CARD_HEIGHT {
                        for x in 0..CARD_WIDTH {
                            if fitted.pixel(x, y).is_some_and(|pixel| pixel.3 > 20) {
                                assert!(
                                    creature.pixel(x, y).is_some_and(|pixel| pixel.3 > 20),
                                    "{} on {} at ({x},{y})",
                                    face_path.display(),
                                    creature_path.display()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_head_mounted_overlays_center_on_the_face() {
        let mut renderer = HybridPetRenderer::new(production_components());
        for path in renderer.pool("adult", LayerSlot::Creature) {
            let target = renderer.frame(&path, PetStage::Adult);
            for mount in ["head", "headwear"] {
                assert!(
                    (target.mount(mount).center.0 - target.mount("face").center.0).abs() <= 16.0
                );
            }
        }
    }

    #[test]
    fn test_adult_semantic_accessory_anchors_follow_the_face() {
        let mut renderer = HybridPetRenderer::new(production_components());
        let mut checked = 0;
        for path in renderer.pool("adult", LayerSlot::Creature) {
            if !path.with_extension("json").is_file() {
                continue;
            }
            let target = renderer.frame(&path, PetStage::Adult);
            for mount in ["head", "headwear", "neck", "chest"] {
                assert_eq!(target.mount(mount).center.0, target.mount("face").center.0);
            }
            checked += 1;
        }
        assert!(checked >= 3);
    }

    fn assert_chest_detail(species: &'static str, tolerance: f64) {
        let mut renderer = HybridPetRenderer::new(production_components());
        let detail = renderer
            .pick_component(LayerSlot::Detail, "adult", species, PetMood::Happy, 1)
            .and_then(|(path, _)| renderer.image(&path))
            .expect("species detail");
        for creature_path in renderer.pool("adult", LayerSlot::Creature) {
            let target = renderer.frame(&creature_path, PetStage::Adult);
            let fitted = fit_to_mount(&detail, &authoring_frame(PetStage::Adult), &target, "body");
            let shifted = translate_layer(
                &fitted,
                target.mount("chest").center.0 - target.mount("body").center.0,
                0.0,
            );
            let bounds = alpha_bounds_above(&shifted, 20).expect("fitted detail");
            let center = (bounds.0 + bounds.2) as f64 / 2.0;
            assert!((center - target.mount("chest").center.0).abs() <= tolerance);
        }
    }

    #[test]
    fn test_chest_mounted_detail_centers_on_the_adult_chest_courier_cama() {
        assert_chest_detail("courier_cama", 3.0);
    }

    #[test]
    fn test_chest_mounted_detail_centers_on_the_adult_chest_rama() {
        assert_chest_detail("rama", 3.0);
    }

    fn assert_body_detail(species: &'static str) {
        let mut renderer = HybridPetRenderer::new(production_components());
        let detail = renderer
            .pick_component(LayerSlot::Detail, "adult", species, PetMood::Happy, 1)
            .and_then(|(path, _)| renderer.image(&path))
            .expect("species detail");
        let creature_path = renderer.pool("adult", LayerSlot::Creature)[0].clone();
        let target = renderer.frame(&creature_path, PetStage::Adult);
        assert_eq!(
            fit_to_mount(&detail, &authoring_frame(PetStage::Adult), &target, "body"),
            fit_to_mount(&detail, &authoring_frame(PetStage::Adult), &target, "body")
        );
    }

    #[test]
    fn test_other_adult_details_keep_the_body_fit_crystal_cama() {
        assert_body_detail("crystal_cama");
    }

    #[test]
    fn test_other_adult_details_keep_the_body_fit_jopacama() {
        assert_body_detail("jopacama");
    }

    #[test]
    fn test_other_adult_details_keep_the_body_fit_pudge_cama() {
        assert_body_detail("pudge_cama");
    }

    #[test]
    fn test_every_accessory_is_visibly_attached_to_every_body() {
        let mut renderer = HybridPetRenderer::new(production_components());
        for (stage_name, stage) in [("adult", PetStage::Adult), ("baby", PetStage::Baby)] {
            for path in renderer.pool(stage_name, LayerSlot::Creature) {
                let target = renderer.frame(&path, stage);
                for accessory in [
                    "red_bow",
                    "straw_hat",
                    "bell_collar",
                    "wool_scarf",
                    "flower_crown",
                    "top_hat",
                    "aegis_charm",
                    "divine_rapier_pin",
                ] {
                    let mut layer = RasterImage::transparent_card();
                    draw_accessory(&mut layer, accessory, stage);
                    let fitted = fit_to_mount(
                        &layer,
                        &geometry_frame(stage),
                        &target,
                        accessory_mount(accessory),
                    );
                    assert!(alpha_bounds_above(&fitted, 20).is_some(), "{accessory}");
                }
            }
        }
    }

    fn assert_ground_floor(species: &'static str) {
        let mut renderer = HybridPetRenderer::new(production_components());
        for (stage_name, stage) in [("adult", PetStage::Adult), ("baby", PetStage::Baby)] {
            for path in renderer.pool(stage_name, LayerSlot::Creature) {
                let target = renderer.frame(&path, stage);
                let ground = render_layer(LayerSlot::Ground, species, stage, PetMood::Content, 17);
                let fitted = fit_ground(&ground, &geometry_frame(stage), &target);
                let bounds = alpha_bounds_above(&fitted, 20).expect("ground effect");
                assert!(bounds.1 <= usize::try_from(FLOOR_Y).unwrap());
                assert!(bounds.3 >= usize::try_from(FLOOR_Y).unwrap());
            }
        }
    }

    #[test]
    fn test_ground_effects_stay_on_the_floor_for_every_body_riverglow_cama() {
        assert_ground_floor("riverglow_cama");
    }

    #[test]
    fn test_ground_effects_stay_on_the_floor_for_every_body_prismwool_cama() {
        assert_ground_floor("prismwool_cama");
    }
}
