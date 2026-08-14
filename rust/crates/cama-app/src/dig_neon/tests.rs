use std::sync::atomic::{AtomicUsize, Ordering};

use crate::dig_assets::{MediaFormat, inspect_media};

use super::*;

fn assert_valid_gif(gif: &GifAsset) {
    assert!(!gif.bytes.is_empty());
    assert!(gif.bytes.len() < DISCORD_GIF_LIMIT);
    assert!(gif.upload_safe());
    let info = inspect_media(&gif.bytes).expect("valid GIF header and frame stream");
    assert_eq!(info.format, MediaFormat::Gif);
    assert_eq!((info.width, info.height), (400, 300));
    assert!(info.frame_count >= 2);
    assert_eq!(info.frame_count, gif.frame_durations_ms.len());
    assert_eq!(info.frame_durations_ms.last(), Some(&60_000));
    assert_eq!(gif.frame_durations_ms.last(), Some(&60_000));
    assert!(gif.shared_palette);
}

fn boss_event() -> BossVictory<'static> {
    BossVictory {
        boss_name: "Burrow-King",
        boundary: 100,
        layer_name: "Magma",
        jc_delta: 240,
        gear_drop: false,
        trophy_relic_drop: false,
    }
}

fn service(roll: bool) -> DigNeonService<ScriptedDigNeonRandom, MemoryDigNeonCooldown> {
    DigNeonService::new(
        ScriptedDigNeonRandom::always(roll),
        MemoryDigNeonCooldown::default(),
    )
}

#[test]
fn test_reveal_victory() {
    assert_valid_gif(&animate_dig_reveal(
        "Magma",
        DigRevealMotion::Victory,
        "THE KING FALLS",
        &["+240 jc"],
        None,
    ));
}

#[test]
fn test_reveal_unearth() {
    assert_valid_gif(&animate_dig_reveal(
        "Crystal",
        DigRevealMotion::Unearth,
        "Crystal Compass",
        &[],
        Some("crystal"),
    ));
}

#[test]
fn test_legendary_relic() {
    assert_valid_gif(&animate_legendary_relic("Ethereal Crown"));
}

#[test]
fn test_cave_in() {
    assert_valid_gif(&animate_cave_in("Stone", 112, 100));
}

#[test]
fn test_pinnacle() {
    assert_valid_gif(&animate_pinnacle(false));
}

#[test]
fn test_prestige() {
    assert_valid_gif(&animate_pinnacle(true));
}

#[test]
fn test_unknown_layer_does_not_crash() {
    let gif = animate_dig_reveal(
        "Nonexistent Layer",
        DigRevealMotion::Unearth,
        "X",
        &[],
        None,
    );
    assert_eq!(layer_palette("Nonexistent Layer").name, "Dirt");
    assert_valid_gif(&gif);
}

#[test]
fn test_dig_animation_parameters_change_native_seekable_frames() {
    let reveal = animate_dig_reveal(
        "Magma",
        DigRevealMotion::Victory,
        "THE KING FALLS",
        &["+240 jc"],
        None,
    );
    let different_reveal = animate_dig_reveal(
        "Crystal",
        DigRevealMotion::Unearth,
        "Crystal Compass",
        &[],
        Some("crystal"),
    );
    let cave = animate_cave_in("Stone", 112, 100);
    let different_cave = animate_cave_in("Stone", 212, 180);
    let relic = animate_legendary_relic("Ethereal Crown");
    let different_relic = animate_legendary_relic("Forgotten Map");

    assert_valid_gif(&reveal);
    assert_valid_gif(&different_reveal);
    assert_valid_gif(&cave);
    assert_valid_gif(&different_cave);
    assert_valid_gif(&relic);
    assert_valid_gif(&different_relic);
    assert_ne!(reveal.bytes, different_reveal.bytes);
    assert_ne!(cave.bytes, different_cave.bytes);
    assert_ne!(relic.bytes, different_relic.bytes);
    assert!(reveal.bytes.len() > 1_000);
    assert!(cave.bytes.len() > 1_000);
    assert!(relic.bytes.len() > 1_000);
}

#[test]
fn test_bigwin_gif_match_bigwin() {
    assert_valid_gif(&create_bigwin_gif(
        "pf",
        3_200,
        BigWinSource::Match,
        BigWinFlavor::BigWin,
    ));
}

#[test]
fn test_bigwin_gif_prediction_top_dog() {
    assert_valid_gif(&create_bigwin_gif(
        "pf",
        3_200,
        BigWinSource::Prediction,
        BigWinFlavor::TopDog,
    ));
}

#[test]
fn test_bigwin_gif_gamba_underdog() {
    assert_valid_gif(&create_bigwin_gif(
        "pf",
        3_200,
        BigWinSource::Gamba,
        BigWinFlavor::Underdog,
    ));
}

#[test]
fn test_affinity_bias() {
    let mut random = SeededDigNeonRandom::new(0);
    let affinity = ["the_damp", "a_drowned_map"];
    let matching = (0..200)
        .map(|_| pick_dig_voice(Some("cave_in"), &mut random).key)
        .filter(|key| affinity.contains(key))
        .count();
    assert!(matching > 100);
}

#[test]
fn test_no_event_returns_valid_voice() {
    let mut random = SeededDigNeonRandom::new(1);
    let selected = pick_dig_voice(None, &mut random);
    assert!(DIG_VOICES.iter().any(|voice| voice.key == selected.key));
}

#[test]
fn test_fallback_line_deterministic_and_nonempty() {
    let mut first = SeededDigNeonRandom::new(5);
    let mut second = SeededDigNeonRandom::new(5);
    let first = fallback_line("legendary_relic", &mut first);
    let second = fallback_line("legendary_relic", &mut second);
    assert_eq!(first, second);
    assert!(!first.is_empty());
}

#[test]
fn test_fallback_line_unknown_event_uses_default() {
    let mut random = SeededDigNeonRandom::new(2);
    assert!(!fallback_line("does_not_exist", &mut random).is_empty());
}

#[derive(Debug, Default)]
struct CountingCaptionPort {
    calls: AtomicUsize,
}

impl DigCaptionPort for CountingCaptionPort {
    fn complete(&self, _request: DigCaptionRequest) -> Result<Option<String>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some("the stone remembers".into()))
    }
}

#[test]
fn test_dig_llm_env_switch_uses_static_caption() {
    let caption_port = Arc::new(CountingCaptionPort::default());
    let mut service = service(true).with_caption(caption_port.clone());
    service.set_dig_llm_enabled(false);

    let caption = service.dig_caption("boss_victory", "defeated a guardian", Some(0));

    assert!(!caption.is_empty());
    assert_eq!(caption_port.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn test_boss_victory_fires_with_attribution() {
    let mut service = service(true);
    let result = service
        .on_dig_boss_victory(1, Some(0), boss_event())
        .expect("forced roll fires");
    assert_eq!(result.layer, 3);
    assert_valid_gif(result.gif_file.as_ref().expect("GIF attached"));
    assert!(
        result
            .text_block
            .as_deref()
            .is_some_and(|text| text.contains('—'))
    );
}

#[test]
fn test_boss_victory_skipped_on_roll_miss() {
    let mut service = service(false);
    assert!(
        service
            .on_dig_boss_victory(1, Some(0), boss_event())
            .is_none()
    );
}

#[test]
fn test_disabled_returns_none() {
    let mut service = service(true);
    service.set_enabled(false);
    assert!(
        service
            .on_dig_boss_victory(1, Some(0), boss_event())
            .is_none()
    );
}

#[test]
fn test_legendary_relic_fires() {
    let mut service = service(true);
    let result = service.on_dig_relic_found(
        1,
        Some(0),
        RelicFound {
            relic_name: "Ethereal Crown",
            rarity: "legendary",
            layer_name: "Abyss",
        },
    );
    assert_valid_gif(
        result
            .and_then(|result| result.gif_file)
            .as_ref()
            .expect("legendary GIF"),
    );
}

#[test]
fn test_common_relic_never_animates() {
    let mut service = service(true);
    assert!(
        service
            .on_dig_relic_found(
                1,
                Some(0),
                RelicFound {
                    relic_name: "Pebble",
                    rarity: "common",
                    layer_name: "Dirt",
                },
            )
            .is_none()
    );
}

#[test]
fn test_cave_in_requires_rollback() {
    let mut service = service(true);
    assert!(
        service
            .on_dig_cave_in(1, Some(0), 100, 100, "Stone")
            .is_none()
    );
    let result = service
        .on_dig_cave_in(1, Some(0), 120, 100, "Stone")
        .expect("rollback fires");
    assert_valid_gif(result.gif_file.as_ref().expect("cave-in GIF"));
}

#[test]
fn test_prestige_fires() {
    let mut service = service(true);
    let result = service
        .on_dig_prestige(1, Some(0))
        .expect("forced roll fires");
    assert_valid_gif(result.gif_file.as_ref().expect("prestige GIF"));
}

#[test]
fn test_big_win_below_floor_returns_none() {
    let mut service = service(true);
    assert!(
        service
            .on_big_win(1, Some(0), BigWinSource::Match, 100, BigWinFlavor::BigWin,)
            .is_none()
    );
}

#[test]
fn test_big_win_fires_above_floor() {
    let mut service = service(true);
    let result = service
        .on_big_win(1, Some(0), BigWinSource::Match, 3_000, BigWinFlavor::TopDog)
        .expect("forced roll fires");
    assert_valid_gif(result.gif_file.as_ref().expect("big-win GIF"));
}

#[test]
fn test_big_win_chance_scales_with_payout() {
    let mut service = service(false);
    assert!(
        service
            .on_big_win(
                1,
                Some(0),
                BigWinSource::Gamba,
                NEON_BIGWIN_MIN_PAYOUT,
                BigWinFlavor::BigWin,
            )
            .is_none()
    );
    assert!(
        service
            .on_big_win(
                1,
                Some(0),
                BigWinSource::Gamba,
                NEON_BIGWIN_FULL_PAYOUT * 3,
                BigWinFlavor::BigWin,
            )
            .is_none()
    );
    let chances = service.random().observed_chances();
    assert!(chances[1] > chances[0]);
    assert!(chances[1] <= 0.95 + f64::EPSILON);
}

#[test]
fn test_wheel_win_routes_to_big_win() {
    let mut service = service(true);
    let result = service
        .on_wheel_result(1, Some(0), 4_000, 5_000)
        .expect("wheel routes to big win");
    assert_valid_gif(result.gif_file.as_ref().expect("wheel big-win GIF"));
}

#[test]
fn test_don_big_win_routes_to_big_win() {
    let mut service = service(true);
    let result = service
        .on_double_or_nothing(1, Some(0), true, 3_000, 6_000)
        .expect("DoN routes to big win");
    assert_valid_gif(result.gif_file.as_ref().expect("DoN big-win GIF"));
}

#[test]
fn test_cooldown_blocks_second_fire() {
    let mut service = service(true);
    assert!(
        service
            .on_dig_boss_victory(1, Some(0), boss_event())
            .is_some()
    );
    assert!(
        service
            .on_dig_relic_found(
                1,
                Some(0),
                RelicFound {
                    relic_name: "Crown",
                    rarity: "legendary",
                    layer_name: "Abyss",
                },
            )
            .is_none()
    );
}
