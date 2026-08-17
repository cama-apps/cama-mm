use super::*;

fn approx(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-9, "expected {right}, got {left}");
}

#[test]
fn test_fifty_percent_winrate_leaves_the_rating_untouched() {
    approx(role_factor(RoleRecord::new(5, 5)), 1.0);
}

#[test]
fn test_zero_percent_winrate_takes_the_floor() {
    approx(role_factor(RoleRecord::new(0, 4)), 0.95);
}

#[test]
fn test_hundred_percent_winrate_takes_the_ceiling() {
    approx(role_factor(RoleRecord::new(4, 0)), 1.05);
}

#[test]
fn test_exactly_three_games_is_enough_sample() {
    // 3 games is the documented threshold, so a 3-0 record reaches the top of
    // the scale rather than falling back to the floor.
    approx(role_factor(RoleRecord::new(3, 0)), 1.05);
    approx(role_factor(RoleRecord::new(0, 3)), 0.95);
}

#[test]
fn test_below_three_games_takes_the_floor_regardless_of_winrate() {
    for record in [
        RoleRecord::new(0, 0),
        RoleRecord::new(1, 0),
        RoleRecord::new(2, 0),
        RoleRecord::new(0, 1),
        RoleRecord::new(1, 1),
    ] {
        approx(role_factor(record), 0.95);
    }
}

#[test]
fn test_scale_is_linear_in_winrate() {
    // 25% and 75% sit exactly a quarter in from each end.
    approx(role_factor(RoleRecord::new(1, 3)), 0.975);
    approx(role_factor(RoleRecord::new(3, 1)), 1.025);
    // Midpoints between those and the ends stay on the same line.
    approx(role_factor(RoleRecord::new(1, 7)), 0.9625);
    approx(role_factor(RoleRecord::new(7, 1)), 1.0375);
}

#[test]
fn test_factor_always_stays_inside_the_documented_band() {
    for wins in 0..12_u32 {
        for losses in 0..12_u32 {
            let factor = role_factor(RoleRecord::new(wins, losses));
            assert!(
                (MIN_ROLE_FACTOR..=MAX_ROLE_FACTOR).contains(&factor),
                "{wins}W/{losses}L produced {factor}"
            );
        }
    }
}

#[test]
fn test_adjusted_value_scales_the_base_rating() {
    approx(adjusted_value(2_000.0, RoleRecord::new(5, 5)), 2_000.0);
    approx(adjusted_value(2_000.0, RoleRecord::new(0, 5)), 1_900.0);
    approx(adjusted_value(2_000.0, RoleRecord::new(5, 0)), 2_100.0);
}

#[test]
fn test_adjusted_value_clamps_negative_ratings_at_zero() {
    approx(adjusted_value(-100.0, RoleRecord::new(5, 5)), 0.0);
}

#[test]
fn test_games_counts_both_columns() {
    assert_eq!(RoleRecord::new(4, 3).games(), 7);
    assert_eq!(RoleRecord::default().games(), 0);
}
