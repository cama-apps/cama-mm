//! Tip DTOs and the `/economy tip` fee policy.
//!
//! Tip history and the atomic money path live in the tip repository; this module
//! keeps the storage-independent leaderboard aggregates and the fee/tithe
//! calculation applied before the transfer is attempted.

use crate::mana::ManaEffects;

/// One sender or receiver leaderboard aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipLeaderboardEntry {
    pub discord_id: i64,
    pub total_amount: i64,
    pub tip_count: i64,
}

/// Guild-scoped tip volume aggregates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TipVolume {
    pub total_amount: i64,
    pub total_fees: i64,
    pub total_transactions: i64,
}

/// Fee and tithe charged to the sender of a tip on top of `amount`.
///
/// White Plains mana waives the reserve fee; otherwise the fee rounds up and
/// is at least one coin. A positive Plains tithe rate truncates and is also
/// at least one coin.
#[must_use]
pub fn tip_fee_and_tithe(amount: i64, fee_rate: f64, effects: &ManaEffects) -> (i64, i64) {
    let fee = if effects.color.as_deref() == Some("White") && effects.plains_tip_fee_rate.is_some()
    {
        0
    } else {
        ((amount as f64 * fee_rate).ceil() as i64).max(1)
    };
    let tithe = if effects.plains_tithe_rate > 0.0 {
        ((amount as f64 * effects.plains_tithe_rate) as i64).max(1)
    } else {
        0
    };
    (fee, tithe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_fee_rounds_up_with_a_one_coin_floor_and_no_tithe_by_default() {
        let effects = ManaEffects::default();
        assert_eq!(tip_fee_and_tithe(100, 0.01, &effects), (1, 0));
        assert_eq!(tip_fee_and_tithe(101, 0.01, &effects), (2, 0));
        assert_eq!(tip_fee_and_tithe(1, 0.0, &effects), (1, 0));
    }

    #[test]
    fn white_plains_waives_the_fee_and_tithe_truncates_with_a_floor() {
        let effects = ManaEffects {
            color: Some("White".to_owned()),
            plains_tip_fee_rate: Some(0.0),
            plains_tithe_rate: 0.05,
            ..ManaEffects::default()
        };
        assert_eq!(tip_fee_and_tithe(99, 0.01, &effects), (0, 4));
        assert_eq!(tip_fee_and_tithe(10, 0.01, &effects), (0, 1));
    }

    #[test]
    fn plains_fee_rate_without_white_color_still_charges_the_fee() {
        let effects = ManaEffects {
            color: Some("Green".to_owned()),
            plains_tip_fee_rate: Some(0.0),
            ..ManaEffects::default()
        };
        assert_eq!(tip_fee_and_tithe(100, 0.05, &effects), (5, 0));
    }
}
