//! Shared building blocks for the personal patch-notes sections: known-value
//! filters, the early/late split of a year, and seeded copy choices.
//!
//! Unknown data is never zero: a missing, negative, or non-finite value drops
//! the game from whatever is being measured.

use cama_domain::wrapped_distinct::DistinctMatch;

pub(super) fn nonnegative(value: Option<i64>) -> Option<i64> {
    value.filter(|value| *value >= 0)
}

pub(super) fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

/// `(hits, known)` over the games where `condition` has an answer.
pub(super) fn counted(
    games: &[&DistinctMatch],
    condition: impl Fn(&DistinctMatch) -> Option<bool>,
) -> (usize, usize) {
    games
        .iter()
        .filter_map(|game| condition(game))
        .fold((0, 0), |(hits, known), hit| {
            (hits + usize::from(hit), known + 1)
        })
}

/// One player's rows in a stable order, whatever order they were loaded in.
pub(super) fn own_games(matches: &[DistinctMatch], player_id: i64) -> Vec<&DistinctMatch> {
    let mut games = matches
        .iter()
        .filter(|game| game.discord_id == player_id)
        .collect::<Vec<_>>();
    games.sort_by_key(|game| (game.finished_at, game.match_id));
    games
}

/// A real hero id; zero and negative ids are unknown.
pub(super) fn hero_of(game: &DistinctMatch) -> Option<i64> {
    game.hero_id.filter(|hero| *hero > 0)
}

/// Whether the game was played as a core (positions 1-3) rather than a
/// support (4-5); `None` when the position was not derived.
pub(super) fn core(game: &DistinctMatch) -> Option<bool> {
    match game.position? {
        1..=3 => Some(true),
        4..=5 => Some(false),
        _ => None,
    }
}

/// Game length in minutes, for games that report a positive duration.
pub(super) fn minutes(game: &DistinctMatch) -> Option<f64> {
    Some(game.duration_seconds.filter(|seconds| *seconds > 0)? as f64 / 60.0)
}

/// `hits` of `known` as a whole percentage, halves rounding up.
pub(super) fn percent(hits: usize, known: usize) -> usize {
    (200 * hits + known) / (2 * known.max(1))
}

/// Fewest dated games in each half of a year before the halves are compared.
const MIN_HALF_GAMES: usize = 5;

/// The first and last halves of a player's dated games.
pub(super) struct Timeline<'a> {
    pub early: Vec<&'a DistinctMatch>,
    pub late: Vec<&'a DistinctMatch>,
}

/// Split dated games into equal early and late halves on one timeline. For
/// odd counts the middle game is omitted; equal boundary timestamps cannot
/// establish an earlier period.
pub(super) fn timeline<'a>(games: &[&'a DistinctMatch]) -> Option<Timeline<'a>> {
    let mut dated = games
        .iter()
        .copied()
        .filter(|game| game.finished_at.is_some_and(|time| time > 0))
        .collect::<Vec<_>>();
    dated.sort_by_key(|game| (game.finished_at, game.match_id));
    let group_size = dated.len() / 2;
    if group_size < MIN_HALF_GAMES {
        return None;
    }
    let early = dated[..group_size].to_vec();
    let late = dated[dated.len() - group_size..].to_vec();
    if early[group_size - 1].finished_at >= late[0].finished_at {
        return None;
    }
    Some(Timeline { early, late })
}

/// A stable number for this player and `key`, for copy choices and
/// tie-breaks that must not change between replays.
pub(super) fn mix(seed: u64, key: &str) -> u64 {
    let salt = key.bytes().fold(0xCBF2_9CE4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
    });
    let mut mixed = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    mixed ^= mixed >> 33;
    mixed
}

/// One wording from `pool` for this player; the same every time.
pub(super) fn variant<'a>(seed: u64, key: &str, pool: &[&'a str]) -> &'a str {
    pool[(mix(seed, key) % pool.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rounds_halves_up_and_survives_an_empty_sample() {
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 67);
        assert_eq!(percent(1, 8), 13);
        assert_eq!(percent(5, 5), 100);
        assert_eq!(percent(0, 0), 0);
    }

    #[test]
    fn variants_are_stable_per_seed_and_differ_across_seeds_and_keys() {
        let pool = ["a", "b", "c"];
        assert_eq!(variant(9, "key", &pool), variant(9, "key", &pool));
        let across_seeds = (0..32)
            .map(|seed| variant(seed, "key", &pool))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(across_seeds.len(), 3);
        assert!((0..32).any(|seed| variant(seed, "key", &pool) != variant(seed, "other", &pool)));
    }

    #[test]
    fn timeline_needs_five_dated_games_a_side_and_a_real_boundary() {
        let rows = (1..=11)
            .map(|id| DistinctMatch {
                match_id: id,
                discord_id: 7,
                finished_at: Some(id * 86_400),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let games = rows.iter().collect::<Vec<_>>();
        let split = timeline(&games).expect("eleven dated games split");
        let ids =
            |half: &[&DistinctMatch]| half.iter().map(|game| game.match_id).collect::<Vec<_>>();
        assert_eq!(ids(&split.early), [1, 2, 3, 4, 5]);
        assert_eq!(ids(&split.late), [7, 8, 9, 10, 11]);
        assert!(timeline(&games[..9]).is_none());
        let same_day = rows
            .iter()
            .map(|row| DistinctMatch {
                finished_at: Some(1),
                ..row.clone()
            })
            .collect::<Vec<_>>();
        assert!(timeline(&same_day.iter().collect::<Vec<_>>()).is_none());
    }
}
