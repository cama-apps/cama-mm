//! Classification of informational draft win probabilities.

/// Return the favored team (1 Radiant, 2 Dire), or 0 for an inclusive 48–52% split.
/// Invalid probabilities are unavailable, never a split.
#[must_use]
pub const fn draft_winner(radiant_bps: i64) -> Option<i64> {
    match radiant_bps {
        0..=4_799 => Some(2),
        4_800..=5_200 => Some(0),
        5_201..=10_000 => Some(1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::draft_winner;

    #[test]
    fn draft_split_includes_both_boundaries() {
        for probability in [4_800, 4_896, 5_000, 5_200] {
            assert_eq!(draft_winner(probability), Some(0));
        }
        assert_eq!(draft_winner(4_799), Some(2));
        assert_eq!(draft_winner(5_201), Some(1));
    }

    #[test]
    fn favored_teams_and_invalid_probabilities_are_distinct() {
        for probability in [0, 3_834] {
            assert_eq!(draft_winner(probability), Some(2));
        }
        for probability in [6_166, 10_000] {
            assert_eq!(draft_winner(probability), Some(1));
        }
        for probability in [i64::MIN, -1, 10_001, i64::MAX] {
            assert_eq!(draft_winner(probability), None);
        }
    }
}
