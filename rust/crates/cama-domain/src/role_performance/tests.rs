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
    approx(role_factor(RoleRecord::new(0, 10)), 0.9);
}

#[test]
fn test_hundred_percent_winrate_takes_the_ceiling() {
    approx(role_factor(RoleRecord::new(10, 0)), 1.1);
}

#[test]
fn test_exactly_ten_games_is_enough_sample() {
    // 10 games is the documented threshold, so a 10-0 record reaches the top
    // of the scale rather than falling back to the unproven factor.
    approx(role_factor(RoleRecord::new(10, 0)), 1.1);
    approx(role_factor(RoleRecord::new(0, 10)), 0.9);
}

#[test]
fn test_below_ten_games_leaves_the_rating_untouched_regardless_of_winrate() {
    for record in [
        RoleRecord::new(0, 0),
        RoleRecord::new(1, 0),
        RoleRecord::new(3, 0),
        RoleRecord::new(0, 3),
        RoleRecord::new(9, 0),
        RoleRecord::new(0, 9),
        RoleRecord::new(5, 4),
    ] {
        approx(role_factor(record), 1.0);
    }
}

#[test]
fn test_scale_is_linear_in_winrate() {
    // 25% and 75% sit exactly a quarter in from each end.
    approx(role_factor(RoleRecord::new(5, 15)), 0.95);
    approx(role_factor(RoleRecord::new(15, 5)), 1.05);
    // Midpoints between those and the ends stay on the same line.
    approx(role_factor(RoleRecord::new(2, 14)), 0.925);
    approx(role_factor(RoleRecord::new(14, 2)), 1.075);
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
fn test_games_counts_both_columns() {
    assert_eq!(RoleRecord::new(4, 3).games(), 7);
    assert_eq!(RoleRecord::default().games(), 0);
}
