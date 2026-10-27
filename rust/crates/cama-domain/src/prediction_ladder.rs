//! House liquidity ladder for prediction markets.
//!
//! Market creation, admin refreshes, and the scheduled refresh worker all
//! quote the same symmetric ladder around a fair price; this is its single
//! definition.

/// Which side of the YES order book a house level rests on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LadderSide {
    YesAsk,
    YesBid,
}

/// One house quote: `size` contracts resting at `price` cents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LadderLevel {
    pub side: LadderSide,
    pub price: i64,
    pub size: i64,
}

/// Apply an economy-event spread delta, keeping the spread at least one tick
/// and never wider than the 1..=99 price band allows.
#[must_use]
pub fn adjusted_spread(base: i64, delta: i64, tick_size: i64) -> i64 {
    let maximum = (98 / tick_size.max(1)).max(1);
    base.saturating_add(delta).clamp(1, maximum)
}

/// Build the house ladder around `fair`.
///
/// Level `i` (zero-based) is quoted `(spread_ticks + i) * tick_size` cents
/// from fair on both sides. The first `normal_levels` levels carry
/// `size_per_level`; the outer levels follow with `outer_level_sizes`.
/// A non-positive `size_per_level` quotes nothing at all, zero-size outer
/// levels are skipped while still consuming their offset, and quotes outside
/// 1..=99 are dropped. Each level emits its ask before its bid.
#[must_use]
pub fn build_levels(
    fair: i64,
    spread_ticks: i64,
    tick_size: i64,
    normal_levels: usize,
    size_per_level: i64,
    outer_level_sizes: &[i64],
) -> Vec<LadderLevel> {
    if size_per_level <= 0 {
        return Vec::new();
    }
    let sizes = std::iter::repeat_n(size_per_level, normal_levels)
        .chain(outer_level_sizes.iter().map(|size| (*size).max(0)));
    let mut levels = Vec::new();
    for (index, size) in sizes.enumerate() {
        if size == 0 {
            continue;
        }
        let Ok(index) = i64::try_from(index) else {
            break;
        };
        let offset = spread_ticks.saturating_add(index).saturating_mul(tick_size);
        let ask = fair.saturating_add(offset);
        let bid = fair.saturating_sub(offset);
        if (1..=99).contains(&ask) {
            levels.push(LadderLevel {
                side: LadderSide::YesAsk,
                price: ask,
                size,
            });
        }
        if (1..=99).contains(&bid) {
            levels.push(LadderLevel {
                side: LadderSide::YesBid,
                price: bid,
                size,
            });
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quotes(levels: &[LadderLevel]) -> Vec<(char, i64, i64)> {
        levels
            .iter()
            .map(|level| {
                let side = match level.side {
                    LadderSide::YesAsk => 'a',
                    LadderSide::YesBid => 'b',
                };
                (side, level.price, level.size)
            })
            .collect()
    }

    #[test]
    fn default_creation_ladder_is_pinned() {
        let levels = build_levels(50, 2, 1, 3, 50, &[20, 15, 10, 5]);
        assert_eq!(
            quotes(&levels),
            vec![
                ('a', 52, 50),
                ('b', 48, 50),
                ('a', 53, 50),
                ('b', 47, 50),
                ('a', 54, 50),
                ('b', 46, 50),
                ('a', 55, 20),
                ('b', 45, 20),
                ('a', 56, 15),
                ('b', 44, 15),
                ('a', 57, 10),
                ('b', 43, 10),
                ('a', 58, 5),
                ('b', 42, 5),
            ]
        );
    }

    #[test]
    fn tick_size_scales_offsets_and_out_of_band_quotes_drop() {
        let levels = build_levels(90, 2, 3, 2, 10, &[0, 4]);
        assert_eq!(
            quotes(&levels),
            vec![
                ('a', 96, 10),
                ('b', 84, 10),
                ('a', 99, 10),
                ('b', 81, 10),
                ('b', 75, 4),
            ]
        );
    }

    #[test]
    fn nonpositive_size_per_level_quotes_nothing() {
        assert!(build_levels(50, 4, 1, 3, 0, &[8, 6, 4, 2]).is_empty());
        assert!(build_levels(50, 4, 1, 3, -5, &[8, 6, 4, 2]).is_empty());
    }

    #[test]
    fn adjusted_spread_stays_within_the_price_band() {
        assert_eq!(adjusted_spread(4, 2, 1), 6);
        assert_eq!(adjusted_spread(2, -999, 1), 1);
        assert_eq!(adjusted_spread(2, 999, 1), 98);
        assert_eq!(adjusted_spread(2, 999, 10), 9);
        assert_eq!(adjusted_spread(2, 0, 0), 2);
    }
}
