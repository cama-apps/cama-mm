use super::*;

/// A standard modern lineup: 2 safe, 1 mid, 2 off, in team order
/// carry / mid / offlaner / soft support / hard support.
fn standard_team() -> [LaneStats; 5] {
    [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_OFF, 3_200),
        LaneStats::new(LANE_OFF, 1_800),
        LaneStats::new(LANE_SAFE, 1_500),
    ]
}

#[test]
fn test_standard_lanes_resolve_to_all_five_positions() {
    assert_eq!(
        derive_positions(&standard_team()),
        Ok(["1", "2", "3", "4", "5"])
    );
}

#[test]
fn test_farm_priority_decides_core_versus_support_not_input_order() {
    // Same lanes, but the supports are listed first and out-farm nobody.
    let team = [
        LaneStats::new(LANE_SAFE, 1_500),
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_OFF, 1_800),
        LaneStats::new(LANE_OFF, 3_200),
    ];
    assert_eq!(derive_positions(&team), Ok(["5", "1", "2", "4", "3"]));
}

#[test]
fn test_solo_offlane_with_a_roamer_puts_the_roamer_at_position_four() {
    let team = [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_OFF, 3_000),
        LaneStats::new(LANE_ROAM, 1_600),
        LaneStats::new(LANE_SAFE, 1_500),
    ];
    assert_eq!(derive_positions(&team), Ok(["1", "2", "3", "4", "5"]));
}

#[test]
fn test_jungler_is_treated_as_the_soft_support() {
    let team = [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_OFF, 3_000),
        LaneStats::new(LANE_JUNGLE, 1_600),
        LaneStats::new(LANE_SAFE, 1_500),
    ];
    assert_eq!(derive_positions(&team), Ok(["1", "2", "3", "4", "5"]));
}

#[test]
fn test_unparsed_replay_is_rejected_rather_than_guessed() {
    let mut team = standard_team();
    team[2].lane_role = None;
    assert_eq!(derive_positions(&team), Err(DerivationFailure::Unparsed));

    let mut team = standard_team();
    team[2].gold_at_10 = None;
    assert_eq!(derive_positions(&team), Err(DerivationFailure::Unparsed));
}

#[test]
fn test_trilane_is_rejected() {
    let team = [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_SAFE, 1_800),
        LaneStats::new(LANE_SAFE, 1_500),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_OFF, 3_000),
    ];
    assert_eq!(
        derive_positions(&team),
        Err(DerivationFailure::AmbiguousLanes)
    );
}

#[test]
fn test_two_mids_is_rejected() {
    let team = [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_SAFE, 1_500),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_MID, 3_000),
        LaneStats::new(LANE_OFF, 2_000),
    ];
    assert_eq!(
        derive_positions(&team),
        Err(DerivationFailure::AmbiguousLanes)
    );
}

#[test]
fn test_two_roamers_is_rejected() {
    let team = [
        LaneStats::new(LANE_SAFE, 4_000),
        LaneStats::new(LANE_SAFE, 1_500),
        LaneStats::new(LANE_MID, 3_800),
        LaneStats::new(LANE_ROAM, 1_600),
        LaneStats::new(LANE_ROAM, 1_400),
    ];
    assert_eq!(
        derive_positions(&team),
        Err(DerivationFailure::AmbiguousLanes)
    );
}

#[test]
fn test_identical_farm_in_a_lane_is_rejected_rather_than_coin_flipped() {
    let mut team = standard_team();
    team[0].gold_at_10 = Some(2_500);
    team[4].gold_at_10 = Some(2_500);
    assert_eq!(
        derive_positions(&team),
        Err(DerivationFailure::TiedFarmPriority)
    );
}

#[test]
fn test_every_accepted_result_is_a_permutation_of_one_through_five() {
    let accepted = [
        standard_team(),
        [
            LaneStats::new(LANE_SAFE, 4_000),
            LaneStats::new(LANE_MID, 3_800),
            LaneStats::new(LANE_OFF, 3_000),
            LaneStats::new(LANE_ROAM, 1_600),
            LaneStats::new(LANE_SAFE, 1_500),
        ],
    ];
    for team in accepted {
        let mut positions = derive_positions(&team).expect("accepted shape");
        positions.sort_unstable();
        assert_eq!(positions, ["1", "2", "3", "4", "5"]);
    }
}

#[test]
fn test_gold_at_ten_reads_the_tenth_minute_sample() {
    let series: Vec<i64> = (0..=15).map(|minute| minute * 100).collect();
    assert_eq!(gold_at_ten(&series), Some(1_000));
}

#[test]
fn test_gold_at_ten_is_absent_for_a_game_shorter_than_the_sample() {
    let series: Vec<i64> = (0..8).map(|minute| minute * 100).collect();
    assert_eq!(gold_at_ten(&series), None);
    assert_eq!(gold_at_ten(&[]), None);
}
