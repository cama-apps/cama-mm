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
}

impl LaneStats {
    #[must_use]
    pub const fn new(lane_role: i64, last_hits_at_10: i64) -> Self {
        Self {
            lane_role: Some(lane_role),
            last_hits_at_10: Some(last_hits_at_10),
        }
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
    /// minutes, so core and support cannot be separated without guessing.
    TiedFarmPriority,
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
    for (index, stats) in team.iter().enumerate() {
        let (Some(lane), Some(player_last_hits)) = (stats.lane_role, stats.last_hits_at_10)
        else {
            return Err(DerivationFailure::Unparsed);
        };
        lanes[index] = lane;
        last_hits[index] = player_last_hits;
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
    let (carry, hard_support) = split_by_farm(&safe, &last_hits)?;
    positions[carry] = "1";
    positions[hard_support] = "5";

    match (off.len(), roaming.len()) {
        (2, 0) => {
            let (offlaner, soft_support) = split_by_farm(&off, &last_hits)?;
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

/// Split a two-player lane into (higher last hits, lower last hits).
fn split_by_farm(
    lane: &[usize],
    last_hits: &[i64; 5],
) -> Result<(usize, usize), DerivationFailure> {
    let (first, second) = (lane[0], lane[1]);
    match last_hits[first].cmp(&last_hits[second]) {
        std::cmp::Ordering::Greater => Ok((first, second)),
        std::cmp::Ordering::Less => Ok((second, first)),
        std::cmp::Ordering::Equal => Err(DerivationFailure::TiedFarmPriority),
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
