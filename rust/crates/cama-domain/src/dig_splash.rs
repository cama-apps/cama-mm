//! Pure hostile Dig splash penalty policy.

pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;
pub const DIG_EVENT_DEFLATION_MULTIPLIER: f64 = 1.375;

#[must_use]
pub fn strengthen_dig_event_penalty(amount: i64) -> i64 {
    if amount == 0 {
        return 0;
    }
    let magnitude = ((amount.unsigned_abs() as f64) * DIG_EVENT_DEFLATION_MULTIPLIER).ceil() as i64;
    if amount > 0 { magnitude } else { -magnitude }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnSplashResult {
    pub victims: Vec<(i64, i64)>,
    pub total_burned: i64,
}
