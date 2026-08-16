//! Exact public policy counterparts for Python's post-pinnacle decay contract.

use cama_app::dig_bosses::PINNACLE_DEPTH;
use cama_app::dig_relic_rework::{RelicSet, post_pinnacle_decay_factor};

#[test]
fn test_post_pinnacle_factor_at_or_below_pinnacle_is_one() {
    let relics = RelicSet::default();

    assert_eq!(post_pinnacle_decay_factor(0, &relics), 1.0);
    assert_eq!(
        post_pinnacle_decay_factor(i64::from(PINNACLE_DEPTH - 1), &relics),
        1.0
    );
    assert_eq!(
        post_pinnacle_decay_factor(i64::from(PINNACLE_DEPTH), &relics),
        1.0
    );
}

#[test]
fn test_post_pinnacle_factor_steps_down_per_25_depth() {
    let relics = RelicSet::default();
    let pinnacle = i64::from(PINNACLE_DEPTH);

    assert_eq!(post_pinnacle_decay_factor(pinnacle + 25, &relics), 0.95);
    assert_eq!(post_pinnacle_decay_factor(pinnacle + 100, &relics), 0.80);
    assert_eq!(post_pinnacle_decay_factor(pinnacle + 200, &relics), 0.60);
}

#[test]
fn test_post_pinnacle_factor_clamps_at_zero() {
    let relics = RelicSet::default();

    assert_eq!(
        post_pinnacle_decay_factor(i64::from(PINNACLE_DEPTH) + 1_000, &relics),
        0.0
    );
}

#[test]
fn test_post_pinnacle_partial_steps_round_down() {
    let relics = RelicSet::default();
    let pinnacle = i64::from(PINNACLE_DEPTH);

    assert_eq!(post_pinnacle_decay_factor(pinnacle + 24, &relics), 1.0);
    assert_eq!(post_pinnacle_decay_factor(pinnacle + 26, &relics), 0.95);
}
