//! Daily gamba-event multiplier policy shared by the wheel and betting runtime.

/// Coerce a daily-event multiplier to Python's operational range.
#[must_use]
pub fn bounded_economy_multiplier(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 10.0)
    } else {
        1.0
    }
}

/// Scale one numeric wheel wedge without turning it into the special zero
/// "lose a turn" wedge. Float-to-integer conversion truncates toward zero.
#[must_use]
pub fn apply_gamba_event_multiplier(value: i64, win_multiplier: f64, loss_multiplier: f64) -> i64 {
    if value == 0 {
        return 0;
    }
    let multiplier = if value > 0 {
        win_multiplier
    } else {
        loss_multiplier
    };
    let magnitude = ((value.unsigned_abs() as f64) * bounded_economy_multiplier(multiplier)) as u64;
    if value > 0 {
        magnitude.clamp(1, i64::MAX as u64) as i64
    } else {
        let magnitude = magnitude.clamp(1, 1_u64 << 63);
        if magnitude == 1_u64 << 63 {
            i64::MIN
        } else {
            -(magnitude as i64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gamba_multiplier_scales_exact_display_amounts() {
        assert_eq!(apply_gamba_event_multiplier(25, 0.6, 1.5), 15);
        assert_eq!(apply_gamba_event_multiplier(-20, 0.6, 1.5), -30);
        assert_eq!(apply_gamba_event_multiplier(0, 0.6, 1.5), 0);
    }

    #[test]
    fn test_gamba_multiplier_bounds_and_preserves_numeric_wedge_semantics() {
        assert_eq!(bounded_economy_multiplier(f64::INFINITY), 1.0);
        assert_eq!(bounded_economy_multiplier(-3.0), 0.0);
        assert_eq!(bounded_economy_multiplier(100.0), 10.0);
        assert_eq!(apply_gamba_event_multiplier(1, 0.0, 1.0), 1);
        assert_eq!(apply_gamba_event_multiplier(-1, 1.0, 0.0), -1);
    }
}
