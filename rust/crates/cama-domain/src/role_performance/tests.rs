use super::*;

fn approx(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-9, "expected {right}, got {left}");
}

/// The factor for the first record of `records`, with the rest of the
/// player's roles making up their overall form.
fn factor_for_first(records: &[RoleRecord]) -> f64 {
    role_factor(records[0], RoleRecord::total(records))
}

#[test]
fn test_one_role_player_is_neutral_whatever_their_winrate() {
    // The only role is the overall record, so there is no gap to measure. A
    // lopsided record here is a rating climb, which the rating already holds.
    for record in [
        RoleRecord::new(57, 24),
        RoleRecord::new(10, 0),
        RoleRecord::new(0, 10),
        RoleRecord::new(400, 100),
    ] {
        approx(role_factor(record, record), 1.0);
    }
}

#[test]
fn test_role_won_as_often_as_overall_is_neutral() {
    // 70% in the role, 70% everywhere else: a strong player, not a specialist.
    approx(
        factor_for_first(&[RoleRecord::new(14, 6), RoleRecord::new(21, 9)]),
        1.0,
    );
    // Even records in every role, the role-neutral fixture other suites use.
    approx(factor_for_first(&[RoleRecord::new(5, 5); 5]), 1.0);
}

#[test]
fn test_role_above_own_form_is_scaled_up_and_below_is_scaled_down() {
    // 15-5 and 5-15 make an overall 50%, a gap of 25 points each way. Twenty
    // games carry half weight, so the factor moves 0.2 * 0.25 * 0.5.
    let best = RoleRecord::new(15, 5);
    let worst = RoleRecord::new(5, 15);
    approx(factor_for_first(&[best, worst]), 1.025);
    approx(factor_for_first(&[worst, best]), 0.975);
}

#[test]
fn test_same_role_record_is_judged_against_the_players_own_form() {
    let role = RoleRecord::new(15, 5);
    // For a player who wins 75% everywhere, 75% in this role says nothing.
    approx(role_factor(role, RoleRecord::new(75, 25)), 1.0);
    // For a 50% player it is their best role.
    assert!(role_factor(role, RoleRecord::new(50, 50)) > 1.0);
    // For a 90% player it is their worst.
    assert!(role_factor(role, RoleRecord::new(90, 10)) < 1.0);
}

#[test]
fn test_deeper_sample_moves_the_factor_further_for_the_same_gap() {
    // The same 25-point gap at 20, 60, and 180 games: weights 1/2, 3/4, 9/10.
    approx(
        factor_for_first(&[RoleRecord::new(15, 5), RoleRecord::new(5, 15)]),
        1.025,
    );
    approx(
        factor_for_first(&[RoleRecord::new(45, 15), RoleRecord::new(15, 45)]),
        1.0375,
    );
    approx(
        factor_for_first(&[RoleRecord::new(135, 45), RoleRecord::new(45, 135)]),
        1.045,
    );
}

#[test]
fn test_below_ten_games_leaves_the_rating_untouched_regardless_of_winrate() {
    let rest = RoleRecord::new(50, 50);
    for record in [
        RoleRecord::new(0, 0),
        RoleRecord::new(1, 0),
        RoleRecord::new(3, 0),
        RoleRecord::new(0, 3),
        RoleRecord::new(9, 0),
        RoleRecord::new(0, 9),
        RoleRecord::new(5, 4),
    ] {
        approx(factor_for_first(&[record, rest]), 1.0);
    }
}

#[test]
fn test_exactly_ten_games_is_enough_sample() {
    // Ten games is the documented threshold. 10-0 beside 0-10 is a 50-point
    // gap at a third of full weight: 0.2 * 0.5 * (10 / 30).
    let unbeaten = RoleRecord::new(10, 0);
    let winless = RoleRecord::new(0, 10);
    approx(factor_for_first(&[unbeaten, winless]), 1.0 + 0.1 / 3.0);
    approx(factor_for_first(&[winless, unbeaten]), 1.0 - 0.1 / 3.0);
}

#[test]
fn test_thin_roles_still_count_toward_overall_form() {
    // The nine-game role is too thin to be scaled itself, but its losses are
    // part of the form the deep role is measured against: 20-0 against an
    // overall 20-9 is a gap of 9/29 at half weight.
    let deep = RoleRecord::new(20, 0);
    let thin = RoleRecord::new(0, 9);
    approx(
        factor_for_first(&[deep, thin]),
        1.0 + 0.2 * (9.0 / 29.0) * 0.5,
    );
    approx(factor_for_first(&[thin, deep]), 1.0);
}

#[test]
fn test_mid_specialist_who_climbed_is_valued_near_their_rating() {
    // A long climb played mostly on mid: 70% there, 62% on carry, 45% on
    // offlane, 66% overall. Against 50% the mid record alone was worth 1.04;
    // against their own form it is the 4-point edge mid has over the rest.
    let mid = RoleRecord::new(57, 24);
    let carry = RoleRecord::new(15, 9);
    let offlane = RoleRecord::new(5, 6);
    let overall = RoleRecord::total(&[mid, carry, offlane]);
    assert_eq!(overall, RoleRecord::new(77, 39));

    let mid_factor = role_factor(mid, overall);
    assert!((mid_factor - 1.0064).abs() < 1e-4, "mid was {mid_factor}");
    let carry_factor = role_factor(carry, overall);
    assert!(
        (carry_factor - 0.9958).abs() < 1e-4,
        "carry was {carry_factor}"
    );
    let offlane_factor = role_factor(offlane, overall);
    assert!(
        (offlane_factor - 0.9852).abs() < 1e-4,
        "offlane was {offlane_factor}"
    );
}

#[test]
fn test_factor_always_stays_inside_the_documented_band() {
    for wins in 0..12_u32 {
        for losses in 0..12_u32 {
            let record = RoleRecord::new(wins, losses);
            for rest in [
                RoleRecord::new(0, 0),
                RoleRecord::new(0, 40),
                RoleRecord::new(40, 0),
                RoleRecord::new(20, 20),
            ] {
                let factor = factor_for_first(&[record, rest]);
                assert!(
                    (MIN_ROLE_FACTOR..=MAX_ROLE_FACTOR).contains(&factor),
                    "{wins}W/{losses}L beside {rest:?} produced {factor}"
                );
            }
        }
    }
}

#[test]
fn test_extreme_gap_is_held_at_the_band_edges() {
    // A deep role lost every time by a player who wins everything else, and
    // the reverse: the raw scale would pass 0.9 and 1.1, the band does not.
    let winless = RoleRecord::new(0, 1_000);
    let unbeaten = RoleRecord::new(1_000, 0);
    approx(
        factor_for_first(&[winless, RoleRecord::new(100_000, 0)]),
        MIN_ROLE_FACTOR,
    );
    approx(
        factor_for_first(&[unbeaten, RoleRecord::new(0, 100_000)]),
        MAX_ROLE_FACTOR,
    );
}

#[test]
fn test_no_overall_sample_is_neutral() {
    // An inconsistent caller - a deep role with an empty overall - has nothing
    // to measure against and must not divide by zero.
    approx(
        role_factor(RoleRecord::new(10, 0), RoleRecord::default()),
        1.0,
    );
}

#[test]
fn test_games_counts_both_columns() {
    assert_eq!(RoleRecord::new(4, 3).games(), 7);
    assert_eq!(RoleRecord::default().games(), 0);
    assert_eq!(RoleRecord::new(u32::MAX, 1).games(), u32::MAX);
}

#[test]
fn test_minimum_sample_starts_at_ten_games() {
    assert!(!RoleRecord::new(5, 4).has_minimum_sample());
    assert!(RoleRecord::new(5, 5).has_minimum_sample());
}

#[test]
fn test_total_sums_every_record() {
    assert_eq!(RoleRecord::total(&[]), RoleRecord::default());
    assert_eq!(
        RoleRecord::total(&[RoleRecord::new(4, 3), RoleRecord::new(1, 6)]),
        RoleRecord::new(5, 9)
    );
    assert_eq!(
        RoleRecord::total(&[RoleRecord::new(u32::MAX, 0), RoleRecord::new(1, 2)]),
        RoleRecord::new(u32::MAX, 2)
    );
}
