//! Derive which position (1-5) each player actually played, from OpenDota's
//! parsed lane data plus farm priority at ten minutes.
//!
//! Valve's API does not report position, so every site that shows one derives
//! it. OpenDota gives us the *lane* (itself inferred from replay positioning);
//! splitting each lane into core and support is ours to do. Creep score at
//! ten minutes is the signal for it — by the end of a game a support may have
//! farmed up and a shut-down carry may be poor, so a fixed-time snapshot is
//! needed either way. Last hits are used rather than gold at the same minute
//! because last hits move only from creeps and denies: an early kill, bounty
//! rune, or tower assist can inflate a support's ten-minute gold without
//! reflecting who actually had lane priority, and creep score has no such
//! failure mode.
//!
//! The derivation is deliberately strict: anything that does not resolve to a
//! clean permutation of 1-5 returns [`None`]. A missing role reads as "no
//! sample", which is recoverable; a wrong role silently corrupts a win rate
//! forever.
//!
//! When two lane-mates have identical creep score at ten minutes, ward count
//! (`obs_placed + sen_placed`) breaks the tie: it is a role trait that holds
//! across the whole game rather than a farm reading with a right minute to
//! take, so the full-game total is used directly, no time-slicing needed.
//! This is a fallback, not a blend — wards are consulted only when farm
//! carries no information at all, never averaged against it, since a
//! weighted score would need an arbitrary weight and could override a clean
//! farm read for no principled reason.

/// OpenDota `lane_role` values, as rendered by the match embeds.
pub const LANE_ROAM: i64 = 0;
pub const LANE_SAFE: i64 = 1;
pub const LANE_MID: i64 = 2;
pub const LANE_OFF: i64 = 3;
pub const LANE_JUNGLE: i64 = 4;

/// Minute at which farm priority is read.
pub const FARM_PRIORITY_MINUTE: usize = 10;

/// One player's parsed lane data for a single match.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LaneStats {
    /// OpenDota's inferred lane. `None` when the replay was never parsed.
    pub lane_role: Option<i64>,
    /// Creep score at [`FARM_PRIORITY_MINUTE`], from OpenDota's `lh_t` series.
    pub last_hits_at_10: Option<i64>,
    /// Full-game `obs_placed + sen_placed`, consulted only to break a tie on
    /// `last_hits_at_10` within a lane. `None` when not (yet) captured —
    /// this does not make the whole player `Unparsed`, since it is only
    /// needed in the rare case a tie is actually hit.
    pub wards: Option<i64>,
}

impl LaneStats {
    #[must_use]
    pub const fn new(lane_role: i64, last_hits_at_10: i64) -> Self {
        Self {
            lane_role: Some(lane_role),
            last_hits_at_10: Some(last_hits_at_10),
            wards: None,
        }
    }

    #[must_use]
    pub const fn with_wards(mut self, wards: i64) -> Self {
        self.wards = Some(wards);
        self
    }
}

/// Why a team's lane data could not be resolved into five distinct positions.
///
/// Carried so a backfill can report *why* it skipped rows instead of only how
/// many, which is what makes the accept rate actionable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivationFailure {
    /// The replay was not parsed, so lane or last-hits data is missing.
    Unparsed,
    /// Lane occupancy did not match a shape we can split confidently — a
    /// tri-lane, a lane swap, two mids, and so on.
    AmbiguousLanes,
    /// Two players in the same lane had identical creep score at ten
    /// minutes, and at least one of them has no ward count to fall back on,
    /// so core and support cannot be separated without guessing.
    TiedFarmPriority,
    /// Two players in the same lane had identical creep score at ten
    /// minutes *and* identical ward counts, so the fallback signal did not
    /// resolve it either.
    TiedFarmAndWardPriority,
}

/// Derive the five positions for one team, in the same order as `team`.
///
/// Two lane shapes are accepted, covering the standard modern configurations:
///
/// - `2` safe, `1` mid, `2` off — the safe and off lanes each split into a
///   core and a support by creep score at ten minutes.
/// - `2` safe, `1` mid, `1` off, `1` roaming or jungling — the solo offlaner
///   is position 3 and the roamer is position 4.
///
/// Anything else returns the reason it was rejected.
pub fn derive_positions(team: &[LaneStats; 5]) -> Result<[&'static str; 5], DerivationFailure> {
    let mut lanes = [0_i64; 5];
    let mut last_hits = [0_i64; 5];
    let mut wards = [None; 5];
    for (index, stats) in team.iter().enumerate() {
        let (Some(lane), Some(player_last_hits)) = (stats.lane_role, stats.last_hits_at_10) else {
            return Err(DerivationFailure::Unparsed);
        };
        lanes[index] = lane;
        last_hits[index] = player_last_hits;
        wards[index] = stats.wards;
    }

    let indices_in =
        |lane: i64| -> Vec<usize> { (0..5).filter(|index| lanes[*index] == lane).collect() };
    let safe = indices_in(LANE_SAFE);
    let mid = indices_in(LANE_MID);
    let off = indices_in(LANE_OFF);
    let roaming: Vec<usize> = (0..5)
        .filter(|index| lanes[*index] == LANE_ROAM || lanes[*index] == LANE_JUNGLE)
        .collect();

    if safe.len() != 2 || mid.len() != 1 {
        return Err(DerivationFailure::AmbiguousLanes);
    }

    let mut positions = [""; 5];
    positions[mid[0]] = "2";

    // Safe lane: the farmed player is the carry, their partner the hard support.
    let (carry, hard_support) = split_by_farm(&safe, &last_hits, &wards)?;
    positions[carry] = "1";
    positions[hard_support] = "5";

    match (off.len(), roaming.len()) {
        (2, 0) => {
            let (offlaner, soft_support) = split_by_farm(&off, &last_hits, &wards)?;
            positions[offlaner] = "3";
            positions[soft_support] = "4";
        }
        (1, 1) => {
            positions[off[0]] = "3";
            positions[roaming[0]] = "4";
        }
        _ => return Err(DerivationFailure::AmbiguousLanes),
    }

    if positions.iter().any(|position| position.is_empty()) {
        return Err(DerivationFailure::AmbiguousLanes);
    }
    Ok(positions)
}

/// Split a two-player lane into (core, support): the higher-last-hits player
/// first. A tie falls back to ward count — the higher-ward player is the
/// support — since ward-buying is a role trait that holds all game, unlike
/// farm, which needs a specific minute to be read at. A further tie (or
/// missing ward data to break the first one) still fails rather than guess.
fn split_by_farm(
    lane: &[usize],
    last_hits: &[i64; 5],
    wards: &[Option<i64>; 5],
) -> Result<(usize, usize), DerivationFailure> {
    let (first, second) = (lane[0], lane[1]);
    match last_hits[first].cmp(&last_hits[second]) {
        std::cmp::Ordering::Greater => return Ok((first, second)),
        std::cmp::Ordering::Less => return Ok((second, first)),
        std::cmp::Ordering::Equal => {}
    }
    let (Some(first_wards), Some(second_wards)) = (wards[first], wards[second]) else {
        return Err(DerivationFailure::TiedFarmPriority);
    };
    match first_wards.cmp(&second_wards) {
        std::cmp::Ordering::Greater => Ok((second, first)),
        std::cmp::Ordering::Less => Ok((first, second)),
        std::cmp::Ordering::Equal => Err(DerivationFailure::TiedFarmAndWardPriority),
    }
}

/// Pull creep score at [`FARM_PRIORITY_MINUTE`] out of OpenDota's `lh_t`
/// series.
///
/// A series that ends before ten minutes means the game was shorter than the
/// sample point, so there is no farm-priority reading to take.
#[must_use]
pub fn last_hits_at_ten(last_hits_series: &[i64]) -> Option<i64> {
    last_hits_series.get(FARM_PRIORITY_MINUTE).copied()
}

#[cfg(test)]
#[path = "role_derivation/tests.rs"]
mod tests;
