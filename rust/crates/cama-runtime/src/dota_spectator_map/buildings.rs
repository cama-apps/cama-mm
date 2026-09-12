//! Static building UI anchors for the bundled OpenDota 7.40 terrain image.
//!
//! Source (MIT; notice already bundled in assets/dota_spectator):
//! https://github.com/odota/web/blob/1b7ce1ca467403ed6d0ca871e7802924d91a31f5/src/components/Match/BuildingMap/buildingData733.ts
//! At that exact revision BuildingMap.tsx selects this layout for 7.33 onward,
//! including 7.40, while DotaMap.tsx selects detailed_740.jpg for 7.40 matches.
//! Lane structures use the upstream UI's approximate percentage anchors, not positions
//! measured from a live server. In particular, the upstream layout predates
//! minor subsequent tower moves. Prefer explicit source coordinates whenever
//! available; these anchors communicate lane/tier, not exact attack range.
//!
//! Core landmarks (Ancient and tier 4 pair) use reviewed image-space centers
//! inside each base instead of the old upstream core layout, which placed the
//! tier 4 pair near the outer wall. These six centers are manually aligned UI
//! approximations for this asset, not measured game coordinates.
//!
//! Only building identity is mapped here. Whether a structure is standing must
//! come from the current feed; callers must not draw unknown/destroyed entries.
//! Never use these static anchors to infer visibility, health, or a death time.
//! The Ancient has no league mask bit: it may be shown as a static landmark,
//! but this helper does not establish that it is alive or provide its health.

/// Return the upstream marker center expressed in the renderer's world frame.
/// This inverse transform preserves the source percentage position on any map
/// size and avoids a second, slightly different projection in the renderer.
pub(super) fn position(name: &str, radiant: bool) -> Option<(f64, f64)> {
    let core = match (name, radiant) {
        ("Ancient", true) => Some((82.0, 526.0)),
        ("upper Ancient tier 4 tower", true) => Some((99.0, 501.0)),
        ("lower Ancient tier 4 tower", true) => Some((124.0, 525.0)),
        ("Ancient", false) => Some((546.0, 112.0)),
        ("upper Ancient tier 4 tower", false) => Some((514.0, 117.0)),
        ("lower Ancient tier 4 tower", false) => Some((538.0, 141.0)),
        _ => None,
    };
    if let Some((x, y)) = core {
        // Pixel centers in the unchanged 640x640 bundled terrain reference.
        // Projection spans pixel 0 through 639, not a padded 640px extent.
        return Some((
            (x / 639.0 * 127.0 - 64.0) * 128.0,
            (63.0 - y / 639.0 * 127.0) * 128.0,
        ));
    }
    let index = NAMES.iter().position(|candidate| *candidate == name)?;
    let (left, top) = if radiant { RADIANT[index] } else { DIRE[index] };
    // BuildingMap.tsx places square sprites by their CSS top-left at a 300px
    // map size: towers are 16px, barracks 12px. Our icons are center-anchored,
    // so retain the upstream visual center rather than shifting it up/left.
    // Original goodguys_tower.png and goodguys_rax.png are both 64px squares.
    let half_size_uv = if index < 11 { 8.0 / 300.0 } else { 6.0 / 300.0 };
    Some((
        ((left / 100.0 + half_size_uv) * 127.0 - 64.0) * 128.0,
        (63.0 - (top / 100.0 + half_size_uv) * 127.0) * 128.0,
    ))
}

const NAMES: [&str; 17] = [
    "top tier 1 tower",
    "top tier 2 tower",
    "top tier 3 tower",
    "mid tier 1 tower",
    "mid tier 2 tower",
    "mid tier 3 tower",
    "bottom tier 1 tower",
    "bottom tier 2 tower",
    "bottom tier 3 tower",
    "upper Ancient tier 4 tower",
    "lower Ancient tier 4 tower",
    "top melee barracks",
    "top ranged barracks",
    "mid melee barracks",
    "mid ranged barracks",
    "bottom melee barracks",
    "bottom ranged barracks",
];

// Source values copied as (left %, top %), rather than the TypeScript's
// (top, left) property order. Barracks "bm" is melee and "br" is ranged.
const RADIANT: [(f64, f64); 17] = [
    (9.5, 36.0),
    (9.0, 53.0),
    (8.0, 68.0),
    (38.0, 54.0),
    (27.5, 63.0),
    (18.5, 71.5),
    (75.0, 82.0),
    (46.0, 85.0),
    (23.0, 83.5),
    (8.0, 79.0),
    (12.0, 82.0),
    (10.5, 70.5),
    (6.5, 70.5),
    (18.0, 74.5),
    (15.0, 72.0),
    (20.0, 84.5),
    (20.0, 80.5),
];
const DIRE: [(f64, f64); 17] = [
    (18.0, 12.0),
    (49.0, 11.0),
    (70.0, 12.5),
    (51.0, 43.5),
    (64.0, 33.0),
    (73.0, 24.0),
    (84.0, 60.0),
    (85.0, 45.0),
    (85.5, 28.0),
    (81.0, 13.0),
    (84.0, 16.0),
    (74.0, 14.0),
    (74.0, 10.0),
    (77.5, 22.0),
    (74.5, 19.5),
    (88.0, 24.0),
    (84.0, 24.0),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mask_identities_project_to_distinct_in_crop_anchors() {
        let mut anchors = std::collections::BTreeSet::new();
        for radiant in [true, false] {
            for name in NAMES {
                let (x, y) = position(name, radiant).unwrap();
                assert!((-8192.0..=8064.0).contains(&x));
                assert!((-8192.0..=8064.0).contains(&y));
                assert!(anchors.insert((x.to_bits(), y.to_bits())));
            }
        }
        assert_eq!(anchors.len(), 34);
    }

    #[test]
    fn preserves_upstream_lane_tier_and_barracks_identity() {
        let uv = |name, radiant| {
            let (x, y) = position(name, radiant).unwrap();
            (
                ((x / 128.0 + 64.0) / 127.0 * 100.0).round() as i32,
                ((63.0 - y / 128.0) / 127.0 * 100.0).round() as i32,
            )
        };
        assert_eq!(uv("top tier 2 tower", true), (12, 56));
        assert_eq!(uv("mid tier 2 tower", false), (67, 36));
        assert_eq!(uv("top melee barracks", false), (76, 16));
        assert_eq!(uv("top ranged barracks", false), (76, 12));
        assert_eq!(position("Roshan", true), None);
        assert_eq!(position("top tower", true), None);
    }

    #[test]
    fn core_landmarks_place_tier_fours_between_ancient_and_mid_barracks() {
        for radiant in [true, false] {
            let ancient = position("Ancient", radiant).unwrap();
            let upper = position("upper Ancient tier 4 tower", radiant).unwrap();
            let lower = position("lower Ancient tier 4 tower", radiant).unwrap();
            let mid = position("mid melee barracks", radiant).unwrap();
            let facing = if radiant { 1.0 } else { -1.0 };
            for tower in [upper, lower] {
                // Both towers are on the mid-facing side of their Ancient.
                assert!((tower.0 - ancient.0) * facing > 0.0);
                assert!((tower.1 - ancient.1) * facing > 0.0);
                let ancient_distance = (tower.0 - ancient.0).hypot(tower.1 - ancient.1);
                assert!(ancient_distance < (mid.0 - ancient.0).hypot(mid.1 - ancient.1));
            }
            assert!(upper.1 > lower.1);
            let pair_gap_pixels =
                (upper.0 - lower.0).hypot(upper.1 - lower.1) / (127.0 * 128.0) * 639.0;
            assert!(
                pair_gap_pixels >= 24.0,
                "tier 4 marker glyphs must remain separate"
            );
            assert_ne!(ancient, upper);
            assert_ne!(ancient, lower);
        }
    }
}
