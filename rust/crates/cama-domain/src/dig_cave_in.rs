//! Pure cave-in escalation policy for the `/dig` minigame.
//!
//! Persistence and consequence application remain service concerns. This
//! module owns the depth bands, balance tables, applicability filtering, and
//! weighted/catastrophic rolls. Randomness is supplied by the caller so the
//! domain crate stays dependency-free and tests can be deterministic.

use std::ops::Index;

/// Legacy minimum depth loss for a shallow cave-in.
pub const CAVE_IN_BLOCK_LOSS_MIN: u32 = 6;
/// Legacy maximum depth loss for a shallow cave-in.
pub const CAVE_IN_BLOCK_LOSS_MAX: u32 = 14;

/// First depth in the mid cave-in band.
pub const CAVE_IN_DEPTH_BAND_MID: i64 = 50;
/// First depth in the deep cave-in band.
pub const CAVE_IN_DEPTH_BAND_DEEP: i64 = 150;
/// First depth in the endgame cave-in band.
pub const CAVE_IN_DEPTH_BAND_ENDGAME: i64 = 250;

/// The four cave-in severity bands, ordered from least to most severe.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CaveInBand {
    Shallow,
    Mid,
    Deep,
    Endgame,
}

/// Stable shallow-band constant matching the Python facade.
pub const CAVE_IN_BAND_SHALLOW: CaveInBand = CaveInBand::Shallow;
/// Stable mid-band constant matching the Python facade.
pub const CAVE_IN_BAND_MID: CaveInBand = CaveInBand::Mid;
/// Stable deep-band constant matching the Python facade.
pub const CAVE_IN_BAND_DEEP: CaveInBand = CaveInBand::Deep;
/// Stable endgame-band constant matching the Python facade.
pub const CAVE_IN_BAND_ENDGAME: CaveInBand = CaveInBand::Endgame;

impl CaveInBand {
    /// Return the stable identifier persisted and exposed by the Python app.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::Mid => "mid",
            Self::Deep => "deep",
            Self::Endgame => "endgame",
        }
    }
}

/// A value table with one entry for every cave-in band.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaveInBandTable<T> {
    pub shallow: T,
    pub mid: T,
    pub deep: T,
    pub endgame: T,
}

impl<T> Index<CaveInBand> for CaveInBandTable<T> {
    type Output = T;

    fn index(&self, band: CaveInBand) -> &Self::Output {
        match band {
            CaveInBand::Shallow => &self.shallow,
            CaveInBand::Mid => &self.mid,
            CaveInBand::Deep => &self.deep,
            CaveInBand::Endgame => &self.endgame,
        }
    }
}

/// Per-band inclusive block-loss ranges.
pub const CAVE_IN_BLOCK_LOSS_RANGES: CaveInBandTable<(u32, u32)> = CaveInBandTable {
    shallow: (CAVE_IN_BLOCK_LOSS_MIN, CAVE_IN_BLOCK_LOSS_MAX),
    mid: (8, 18),
    deep: (12, 25),
    endgame: (16, 32),
};

/// Per-band inclusive medical-bill ranges in Jopacoin.
pub const CAVE_IN_MEDICAL_BILL_RANGES: CaveInBandTable<(u32, u32)> = CaveInBandTable {
    shallow: (3, 9),
    mid: (6, 15),
    deep: (12, 25),
    endgame: (18, 40),
};

/// Number of slower-cooldown digs caused by a stun.
pub const CAVE_IN_STUN_DIGS_BY_BAND: CaveInBandTable<u32> = CaveInBandTable {
    shallow: 2,
    mid: 3,
    deep: 4,
    endgame: 5,
};

/// Number of reduced-advance digs caused by an injury.
pub const CAVE_IN_INJURY_DIGS_BY_BAND: CaveInBandTable<u32> = CaveInBandTable {
    shallow: 3,
    mid: 4,
    deep: 5,
    endgame: 6,
};

/// A cave-in consequence identifier shared with the Python service layer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CaveInConsequence {
    Stun,
    Injury,
    MedicalBill,
    GearNick,
    SpilledSatchel,
    SnuffedLight,
    CrackedHat,
}

impl CaveInConsequence {
    /// Return the stable identifier used in cave-in detail payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stun => "stun",
            Self::Injury => "injury",
            Self::MedicalBill => "medical_bill",
            Self::GearNick => "gear_nick",
            Self::SpilledSatchel => "spilled_satchel",
            Self::SnuffedLight => "snuffed_light",
            Self::CrackedHat => "cracked_hat",
        }
    }
}

/// One authored consequence weight in percentage points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeightedConsequence {
    pub consequence: CaveInConsequence,
    pub weight: u32,
}

impl WeightedConsequence {
    const fn new(consequence: CaveInConsequence, weight: u32) -> Self {
        Self {
            consequence,
            weight,
        }
    }
}

const SHALLOW_CONSEQUENCES: &[WeightedConsequence] = &[
    WeightedConsequence::new(CaveInConsequence::Stun, 30),
    WeightedConsequence::new(CaveInConsequence::Injury, 30),
    WeightedConsequence::new(CaveInConsequence::MedicalBill, 40),
];

const MID_CONSEQUENCES: &[WeightedConsequence] = &[
    WeightedConsequence::new(CaveInConsequence::Stun, 25),
    WeightedConsequence::new(CaveInConsequence::Injury, 25),
    WeightedConsequence::new(CaveInConsequence::MedicalBill, 30),
    WeightedConsequence::new(CaveInConsequence::GearNick, 10),
    WeightedConsequence::new(CaveInConsequence::SpilledSatchel, 5),
    WeightedConsequence::new(CaveInConsequence::SnuffedLight, 4),
    WeightedConsequence::new(CaveInConsequence::CrackedHat, 1),
];

const DEEP_CONSEQUENCES: &[WeightedConsequence] = &[
    WeightedConsequence::new(CaveInConsequence::Stun, 20),
    WeightedConsequence::new(CaveInConsequence::Injury, 20),
    WeightedConsequence::new(CaveInConsequence::MedicalBill, 25),
    WeightedConsequence::new(CaveInConsequence::GearNick, 15),
    WeightedConsequence::new(CaveInConsequence::SpilledSatchel, 8),
    WeightedConsequence::new(CaveInConsequence::SnuffedLight, 7),
    WeightedConsequence::new(CaveInConsequence::CrackedHat, 5),
];

const ENDGAME_CONSEQUENCES: &[WeightedConsequence] = &[
    WeightedConsequence::new(CaveInConsequence::Stun, 18),
    WeightedConsequence::new(CaveInConsequence::Injury, 18),
    WeightedConsequence::new(CaveInConsequence::MedicalBill, 22),
    WeightedConsequence::new(CaveInConsequence::GearNick, 18),
    WeightedConsequence::new(CaveInConsequence::SpilledSatchel, 10),
    WeightedConsequence::new(CaveInConsequence::SnuffedLight, 9),
    WeightedConsequence::new(CaveInConsequence::CrackedHat, 5),
];

/// Authored consequence weights. Every unfiltered row totals exactly 100.
pub const CAVE_IN_CONSEQUENCE_WEIGHTS: CaveInBandTable<&[WeightedConsequence]> = CaveInBandTable {
    shallow: SHALLOW_CONSEQUENCES,
    mid: MID_CONSEQUENCES,
    deep: DEEP_CONSEQUENCES,
    endgame: ENDGAME_CONSEQUENCES,
};

/// Probability that a resolved cave-in becomes catastrophic.
pub const CAVE_IN_CATASTROPHIC_PCT_BY_BAND: CaveInBandTable<f64> = CaveInBandTable {
    shallow: 0.0,
    mid: 0.01,
    deep: 0.03,
    endgame: 0.05,
};

/// Inclusive catastrophic medical-bill range in Jopacoin.
pub const CAVE_IN_CATASTROPHIC_MEDICAL_BILL: (u32, u32) = (50, 200);
/// Inclusive number of stun digs applied by a catastrophic cave-in.
pub const CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE: (u32, u32) = (3, 5);
/// Durability damage applied to each selected piece of gear.
pub const CAVE_IN_CATASTROPHIC_GEAR_TICKS: u32 = 3;
/// Milestone size used when catastrophic cave-ins roll depth backward.
pub const CAVE_IN_CATASTROPHIC_MILESTONE_STEP: u32 = 25;

/// Classify a tunnel depth into its cave-in escalation band.
#[must_use]
pub const fn cave_in_band(depth: i64) -> CaveInBand {
    if depth >= CAVE_IN_DEPTH_BAND_ENDGAME {
        CaveInBand::Endgame
    } else if depth >= CAVE_IN_DEPTH_BAND_DEEP {
        CaveInBand::Deep
    } else if depth >= CAVE_IN_DEPTH_BAND_MID {
        CaveInBand::Mid
    } else {
        CaveInBand::Shallow
    }
}

/// State-dependent eligibility for authored consequence types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaveInApplicability {
    pub has_consumables: bool,
    pub has_equipped_gear: bool,
    pub can_lower_luminosity: bool,
    pub has_hard_hat_charges: bool,
}

impl CaveInApplicability {
    /// Construct an applicability snapshot from the current tunnel state.
    #[must_use]
    pub const fn new(
        has_consumables: bool,
        has_equipped_gear: bool,
        can_lower_luminosity: bool,
        has_hard_hat_charges: bool,
    ) -> Self {
        Self {
            has_consumables,
            has_equipped_gear,
            can_lower_luminosity,
            has_hard_hat_charges,
        }
    }

    const fn allows(self, consequence: CaveInConsequence) -> bool {
        match consequence {
            CaveInConsequence::Stun
            | CaveInConsequence::Injury
            | CaveInConsequence::MedicalBill => true,
            CaveInConsequence::GearNick => self.has_equipped_gear,
            CaveInConsequence::SpilledSatchel => self.has_consumables,
            CaveInConsequence::SnuffedLight => self.can_lower_luminosity,
            CaveInConsequence::CrackedHat => self.has_hard_hat_charges,
        }
    }
}

/// Random values required by cave-in policy.
///
/// Runtime adapters must return a value in `[0, 1)` from [`Self::random_unit`]
/// and an integer in the inclusive requested range from
/// [`Self::random_inclusive`]. Keeping this boundary in the caller avoids a
/// random-number-generator dependency in the pure domain crate.
pub trait CaveInRng {
    fn random_unit(&mut self) -> f64;
    fn random_inclusive(&mut self, lower: u32, upper: u32) -> u32;
}

/// Pick a consequence after removing entries that cannot affect current state.
///
/// Remaining weights are renormalized exactly as in Python by rolling over
/// their filtered sum. The medical bill fallback is defensive: the three
/// legacy consequences are always applicable in every currently authored row.
#[must_use]
pub fn pick_cave_in_consequence<R: CaveInRng + ?Sized>(
    band: CaveInBand,
    applicability: CaveInApplicability,
    rng: &mut R,
) -> CaveInConsequence {
    let weights = CAVE_IN_CONSEQUENCE_WEIGHTS[band];
    let total = weights
        .iter()
        .filter(|entry| entry.weight > 0 && applicability.allows(entry.consequence))
        .map(|entry| entry.weight)
        .sum();

    if total == 0 {
        return CaveInConsequence::MedicalBill;
    }

    let roll = rng.random_inclusive(1, total);
    debug_assert!((1..=total).contains(&roll));
    let mut cumulative = 0;
    let mut last_applicable = CaveInConsequence::MedicalBill;

    for entry in weights
        .iter()
        .filter(|entry| entry.weight > 0 && applicability.allows(entry.consequence))
    {
        last_applicable = entry.consequence;
        cumulative += entry.weight;
        if roll <= cumulative {
            return entry.consequence;
        }
    }

    last_applicable
}

/// Roll whether a resolved cave-in escalates to catastrophic.
///
/// A zero-probability band returns without consuming randomness, matching the
/// Python helper's early return.
#[must_use]
pub fn roll_catastrophic_cave_in<R: CaveInRng + ?Sized>(band: CaveInBand, rng: &mut R) -> bool {
    let probability = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[band];
    if probability <= 0.0 {
        return false;
    }

    let roll = rng.random_unit();
    debug_assert!((0.0..1.0).contains(&roll));
    roll < probability
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        CAVE_IN_BLOCK_LOSS_RANGES, CAVE_IN_CATASTROPHIC_PCT_BY_BAND, CAVE_IN_CONSEQUENCE_WEIGHTS,
        CAVE_IN_INJURY_DIGS_BY_BAND, CAVE_IN_MEDICAL_BILL_RANGES, CAVE_IN_STUN_DIGS_BY_BAND,
        CaveInApplicability, CaveInBand, CaveInConsequence, CaveInRng, cave_in_band,
        pick_cave_in_consequence, roll_catastrophic_cave_in,
    };

    #[derive(Debug)]
    struct SeededRng {
        state: u64,
        unit_calls: usize,
        integer_calls: usize,
    }

    impl SeededRng {
        fn new(seed: u64) -> Self {
            Self {
                state: seed.max(1),
                unit_calls: 0,
                integer_calls: 0,
            }
        }

        fn next_u64(&mut self) -> u64 {
            let mut value = self.state;
            value ^= value >> 12;
            value ^= value << 25;
            value ^= value >> 27;
            self.state = value;
            value.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
    }

    impl CaveInRng for SeededRng {
        fn random_unit(&mut self) -> f64 {
            self.unit_calls += 1;
            ((self.next_u64() >> 11) as f64) / ((1_u64 << 53) as f64)
        }

        fn random_inclusive(&mut self, lower: u32, upper: u32) -> u32 {
            self.integer_calls += 1;
            assert!(lower <= upper);
            let width = u64::from(upper - lower) + 1;
            lower + (self.next_u64() % width) as u32
        }
    }

    const ALL_APPLICABLE: CaveInApplicability = CaveInApplicability::new(true, true, true, true);
    const ONLY_LEGACY_APPLICABLE: CaveInApplicability =
        CaveInApplicability::new(false, false, false, false);

    #[test]
    fn test_shallow() {
        assert_eq!(cave_in_band(0), CaveInBand::Shallow);
        assert_eq!(cave_in_band(49), CaveInBand::Shallow);
    }

    #[test]
    fn test_mid() {
        assert_eq!(cave_in_band(50), CaveInBand::Mid);
        assert_eq!(cave_in_band(149), CaveInBand::Mid);
    }

    #[test]
    fn test_deep() {
        assert_eq!(cave_in_band(150), CaveInBand::Deep);
        assert_eq!(cave_in_band(249), CaveInBand::Deep);
    }

    #[test]
    fn test_endgame() {
        assert_eq!(cave_in_band(250), CaveInBand::Endgame);
        assert_eq!(cave_in_band(10_000), CaveInBand::Endgame);
    }

    #[test]
    fn test_block_loss_increases_with_depth() {
        let shallow = CAVE_IN_BLOCK_LOSS_RANGES[CaveInBand::Shallow];
        let mid = CAVE_IN_BLOCK_LOSS_RANGES[CaveInBand::Mid];
        let deep = CAVE_IN_BLOCK_LOSS_RANGES[CaveInBand::Deep];
        let endgame = CAVE_IN_BLOCK_LOSS_RANGES[CaveInBand::Endgame];

        assert!(shallow.1 < mid.1 && mid.1 < deep.1 && deep.1 < endgame.1);
        assert!(shallow.0 <= mid.0 && mid.0 <= deep.0 && deep.0 <= endgame.0);
    }

    #[test]
    fn test_medical_bill_increases_with_depth() {
        let shallow = CAVE_IN_MEDICAL_BILL_RANGES[CaveInBand::Shallow];
        let endgame = CAVE_IN_MEDICAL_BILL_RANGES[CaveInBand::Endgame];
        assert!(endgame.1 >= shallow.1 * 4);
    }

    #[test]
    fn test_stun_and_injury_durations_grow() {
        assert!(
            CAVE_IN_STUN_DIGS_BY_BAND[CaveInBand::Shallow]
                < CAVE_IN_STUN_DIGS_BY_BAND[CaveInBand::Endgame]
        );
        assert!(
            CAVE_IN_INJURY_DIGS_BY_BAND[CaveInBand::Shallow]
                < CAVE_IN_INJURY_DIGS_BY_BAND[CaveInBand::Endgame]
        );
    }

    #[test]
    fn test_consequence_weights_sum_to_100() {
        for band in [
            CaveInBand::Shallow,
            CaveInBand::Mid,
            CaveInBand::Deep,
            CaveInBand::Endgame,
        ] {
            let total: u32 = CAVE_IN_CONSEQUENCE_WEIGHTS[band]
                .iter()
                .map(|entry| entry.weight)
                .sum();
            assert_eq!(total, 100, "band {:?} weights sum to {total}", band);
        }
    }

    #[test]
    fn test_new_consequences_only_at_deeper_bands() {
        let shallow_ids: BTreeSet<_> = CAVE_IN_CONSEQUENCE_WEIGHTS[CaveInBand::Shallow]
            .iter()
            .map(|entry| entry.consequence)
            .collect();
        assert!(!shallow_ids.contains(&CaveInConsequence::GearNick));
        assert!(!shallow_ids.contains(&CaveInConsequence::SpilledSatchel));
        assert!(!shallow_ids.contains(&CaveInConsequence::SnuffedLight));
        assert!(!shallow_ids.contains(&CaveInConsequence::CrackedHat));

        let deep_ids: BTreeSet<_> = CAVE_IN_CONSEQUENCE_WEIGHTS[CaveInBand::Deep]
            .iter()
            .map(|entry| entry.consequence)
            .collect();
        assert!(deep_ids.contains(&CaveInConsequence::GearNick));
        assert!(deep_ids.contains(&CaveInConsequence::SpilledSatchel));
    }

    #[test]
    fn test_shallow_never_catastrophic() {
        let mut rng = SeededRng::new(1);
        for _ in 0..500 {
            assert!(!roll_catastrophic_cave_in(CaveInBand::Shallow, &mut rng));
        }
        assert_eq!(rng.unit_calls, 0, "zero probability must not consume RNG");
    }

    #[test]
    fn test_endgame_can_be_catastrophic() {
        let mut rng = SeededRng::new(1);
        let hits = (0..2_000)
            .filter(|_| roll_catastrophic_cave_in(CaveInBand::Endgame, &mut rng))
            .count();
        assert!(
            (31..200).contains(&hits),
            "got {hits} catastrophic hits in 2000 rolls"
        );
    }

    #[test]
    fn test_pct_table_grows_monotonically() {
        let shallow = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CaveInBand::Shallow];
        let mid = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CaveInBand::Mid];
        let deep = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CaveInBand::Deep];
        let endgame = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CaveInBand::Endgame];
        assert!(shallow < mid && mid < deep && deep < endgame);
    }

    #[test]
    fn test_shallow_only_picks_legacy_consequences() {
        let mut rng = SeededRng::new(42);
        let seen: BTreeSet<_> = (0..500)
            .map(|_| pick_cave_in_consequence(CaveInBand::Shallow, ALL_APPLICABLE, &mut rng))
            .collect();
        let legacy = BTreeSet::from([
            CaveInConsequence::Stun,
            CaveInConsequence::Injury,
            CaveInConsequence::MedicalBill,
        ]);
        assert!(seen.is_subset(&legacy));
    }

    #[test]
    fn test_deep_can_pick_new_consequence_types() {
        let mut rng = SeededRng::new(42);
        let seen: BTreeSet<_> = (0..2_000)
            .map(|_| pick_cave_in_consequence(CaveInBand::Deep, ALL_APPLICABLE, &mut rng))
            .collect();
        for expected in [
            CaveInConsequence::GearNick,
            CaveInConsequence::SpilledSatchel,
            CaveInConsequence::SnuffedLight,
        ] {
            assert!(seen.contains(&expected), "never picked {expected:?}");
        }
    }

    #[test]
    fn test_inapplicable_consequences_are_skipped() {
        let mut rng = SeededRng::new(7);
        let legacy = BTreeSet::from([
            CaveInConsequence::Stun,
            CaveInConsequence::Injury,
            CaveInConsequence::MedicalBill,
        ]);
        for _ in 0..500 {
            let consequence =
                pick_cave_in_consequence(CaveInBand::Deep, ONLY_LEGACY_APPLICABLE, &mut rng);
            assert!(legacy.contains(&consequence));
        }
    }
}
