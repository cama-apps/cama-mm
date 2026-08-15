//! Dota server-region preferences, inference, and aggregation.
//!
//! Players may choose a preferred US server explicitly. When that is unset,
//! OpenDota region counts can supply an inferred preference. This module keeps
//! the policy independent of storage and JSON parsing while retaining the
//! distinctions present in the Python implementation.

use std::cmp::Ordering;
use std::collections::HashMap;

/// The stored code for US East.
pub const USE_REGION_CODE: &str = "USE";
/// The stored code for US West.
pub const USW_REGION_CODE: &str = "USW";
/// The display name for US East.
pub const USE_REGION_NAME: &str = "US East";
/// The display name for US West.
pub const USW_REGION_NAME: &str = "US West";

/// Region-code/display-name pairs accepted by the region policy.
pub const REGION_NAMES: [(&str, &str); 2] = [
    (USE_REGION_CODE, USE_REGION_NAME),
    (USW_REGION_CODE, USW_REGION_NAME),
];

/// The fallback region code for ties and groups with no votes.
pub const DEFAULT_REGION_CODE: &str = USW_REGION_CODE;
/// The fallback display name for ties and groups with no votes.
pub const DEFAULT_REGION_NAME: &str = USW_REGION_NAME;

/// Stored as an inferred region after a successful lookup finds no US play.
///
/// This is distinct from an absent inferred region, which means that inference
/// has not completed successfully and should be retried later.
pub const SENTINEL_NONE: &str = "NONE";

/// An OpenDota `/counts` payload reduced to the data needed by inference.
///
/// `None` passed to [`infer_region_from_counts`] represents a failed or missing
/// API response. `Some(CountsPayload::default())`, by contrast, is a real empty
/// response and therefore produces [`SENTINEL_NONE`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CountsPayload {
    /// Counts keyed by OpenDota region number (`"1"` is US West and `"2"` is
    /// US East). A missing region object behaves like an empty object.
    pub region: Option<RegionCounts>,
}

/// OpenDota region entries keyed by their numeric string identifiers.
pub type RegionCounts = HashMap<String, RegionCountEntry>;

/// The two shapes accepted for one OpenDota region count.
///
/// Normal API responses use `{"games": value}`, but the Python implementation
/// also accepts a bare scalar defensively. A mapping without a `games` key is
/// represented separately because it defaults to zero.
#[derive(Clone, Debug, PartialEq)]
pub enum RegionCountEntry {
    /// A mapping containing a `games` value.
    Games(GameCountValue),
    /// A mapping without a `games` value.
    GamesMissing,
    /// A defensive bare scalar entry.
    Bare(GameCountValue),
}

impl RegionCountEntry {
    /// Construct a normal `{"games": value}` region entry.
    pub fn games(value: impl Into<GameCountValue>) -> Self {
        Self::Games(value.into())
    }

    /// Construct a defensive bare-scalar region entry.
    pub fn bare(value: impl Into<GameCountValue>) -> Self {
        Self::Bare(value.into())
    }

    fn value(&self) -> Option<&GameCountValue> {
        match self {
            Self::Games(value) | Self::Bare(value) => Some(value),
            Self::GamesMissing => None,
        }
    }
}

/// Scalar values that may appear in an OpenDota region entry.
///
/// The variants deliberately model Python's permissive `int(value or 0)`
/// conversion. Null and malformed values become zero; finite floats truncate
/// toward zero; booleans become zero or one; and decimal strings may contain a
/// sign, surrounding whitespace, and valid digit-separating underscores.
#[derive(Clone, Debug, PartialEq)]
pub enum GameCountValue {
    /// A signed integer.
    Integer(i64),
    /// An unsigned integer.
    UnsignedInteger(u64),
    /// A floating-point number.
    Float(f64),
    /// A decimal integer string.
    String(String),
    /// A boolean (`false` = 0, `true` = 1).
    Boolean(bool),
    /// A JSON null or equivalent missing scalar.
    Null,
    /// A value Python's `int` conversion would reject.
    Malformed,
}

macro_rules! impl_signed_game_count {
    ($($integer:ty),+ $(,)?) => {
        $(
            impl From<$integer> for GameCountValue {
                fn from(value: $integer) -> Self {
                    Self::Integer(value as i64)
                }
            }
        )+
    };
}

macro_rules! impl_unsigned_game_count {
    ($($integer:ty),+ $(,)?) => {
        $(
            impl From<$integer> for GameCountValue {
                fn from(value: $integer) -> Self {
                    Self::UnsignedInteger(value as u64)
                }
            }
        )+
    };
}

impl_signed_game_count!(i8, i16, i32, i64, isize);
impl_unsigned_game_count!(u8, u16, u32, u64, usize);

impl From<f32> for GameCountValue {
    fn from(value: f32) -> Self {
        Self::Float(f64::from(value))
    }
}

impl From<f64> for GameCountValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<bool> for GameCountValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<&str> for GameCountValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for GameCountValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl<T> From<Option<T>> for GameCountValue
where
    T: Into<Self>,
{
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

/// The two region fields needed to resolve one player's vote.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlayerRegionInput<'a> {
    /// The player's explicit selection, if any.
    pub preferred_region: Option<&'a str>,
    /// The player's OpenDota-derived selection or sentinel, if any.
    pub inferred_region: Option<&'a str>,
}

/// Read-only access to the region fields used by this policy.
///
/// Runtime player models can implement this trait without coupling the pure
/// domain crate to a database representation.
pub trait RegionPreference {
    /// Return the explicit region code, if set.
    fn preferred_region(&self) -> Option<&str>;

    /// Return the inferred region code or sentinel, if set.
    fn inferred_region(&self) -> Option<&str>;
}

impl RegionPreference for PlayerRegionInput<'_> {
    fn preferred_region(&self) -> Option<&str> {
        self.preferred_region
    }

    fn inferred_region(&self) -> Option<&str> {
        self.inferred_region
    }
}

impl<T> RegionPreference for &T
where
    T: RegionPreference + ?Sized,
{
    fn preferred_region(&self) -> Option<&str> {
        <T as RegionPreference>::preferred_region(*self)
    }

    fn inferred_region(&self) -> Option<&str> {
        <T as RegionPreference>::inferred_region(*self)
    }
}

/// Infer `"USE"` or `"USW"` from an OpenDota `/counts` payload.
///
/// US East is OpenDota region `"2"`; US West is region `"1"`. The greater
/// count wins and an exact tie leans US West. A successfully received payload
/// with no US games returns [`SENTINEL_NONE`], while a missing payload returns
/// `None` so the caller can retry later.
#[must_use]
pub fn infer_region_from_counts(counts: Option<&CountsPayload>) -> Option<&'static str> {
    let counts = counts?;
    let usw_games = games_for_region(counts, "1");
    let use_games = games_for_region(counts, "2");

    if use_games.is_zero() && usw_games.is_zero() {
        return Some(SENTINEL_NONE);
    }

    Some(if use_games > usw_games {
        USE_REGION_CODE
    } else {
        USW_REGION_CODE
    })
}

/// Resolve the player's effective region code, if the player has a valid vote.
///
/// An explicit valid selection wins over inference. Unset values, unknown
/// strings, and [`SENTINEL_NONE`] do not vote.
#[must_use]
pub fn resolve_region<P>(player: &P) -> Option<&'static str>
where
    P: RegionPreference + ?Sized,
{
    canonical_region(player.preferred_region())
        .or_else(|| canonical_region(player.inferred_region()))
}

/// Recommend a server display name for a group of player votes.
///
/// The majority wins; a tie or an entirely unconfigured group defaults to US
/// West, matching inference's tie-break.
#[must_use]
pub fn summarize_region<P>(players: &[P]) -> &'static str
where
    P: RegionPreference,
{
    let (use_votes, usw_votes) = tally_regions(players);
    if use_votes > usw_votes {
        USE_REGION_NAME
    } else {
        DEFAULT_REGION_NAME
    }
}

/// Count resolved players that prevent a clean USE-vs-USW team split.
///
/// Team labels are arbitrary, so both orientations are scored and the smaller
/// mismatch count is returned. Players without a resolved region do not count.
#[must_use]
pub fn region_split_mismatches<Team1Player, Team2Player>(
    team1_players: &[Team1Player],
    team2_players: &[Team2Player],
) -> usize
where
    Team1Player: RegionPreference,
    Team2Player: RegionPreference,
{
    let (team1_use, team1_usw) = tally_regions(team1_players);
    let (team2_use, team2_usw) = tally_regions(team2_players);

    let team1_usw_orientation = team1_use + team2_usw;
    let team1_use_orientation = team1_usw + team2_use;
    team1_usw_orientation.min(team1_use_orientation)
}

fn canonical_region(value: Option<&str>) -> Option<&'static str> {
    match value {
        Some(USE_REGION_CODE) => Some(USE_REGION_CODE),
        Some(USW_REGION_CODE) => Some(USW_REGION_CODE),
        _ => None,
    }
}

fn tally_regions<P>(players: &[P]) -> (usize, usize)
where
    P: RegionPreference,
{
    players.iter().fold(
        (0, 0),
        |(use_votes, usw_votes), player| match resolve_region(player) {
            Some(USE_REGION_CODE) => (use_votes + 1, usw_votes),
            Some(USW_REGION_CODE) => (use_votes, usw_votes + 1),
            _ => (use_votes, usw_votes),
        },
    )
}

fn games_for_region(counts: &CountsPayload, region_key: &str) -> NormalizedInteger {
    counts
        .region
        .as_ref()
        .and_then(|regions| regions.get(region_key))
        .and_then(RegionCountEntry::value)
        .and_then(GameCountValue::to_python_integer)
        .unwrap_or_else(NormalizedInteger::zero)
}

impl GameCountValue {
    fn to_python_integer(&self) -> Option<NormalizedInteger> {
        match self {
            Self::Integer(value) => Some(NormalizedInteger::from_i64(*value)),
            Self::UnsignedInteger(value) => Some(NormalizedInteger::from_u64(*value)),
            Self::Float(value) => NormalizedInteger::from_f64(*value),
            Self::String(value) => NormalizedInteger::from_decimal_string(value),
            Self::Boolean(value) => Some(NormalizedInteger::from_u64(u64::from(*value))),
            Self::Null => Some(NormalizedInteger::zero()),
            Self::Malformed => None,
        }
    }
}

/// A minimal arbitrary-precision integer used to match Python's count coercion.
/// Digits are decimal and little-endian so large finite floats can be expanded
/// exactly without adding a big-integer dependency to the domain crate.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NormalizedInteger {
    sign: i8,
    digits: Vec<u8>,
}

impl NormalizedInteger {
    fn zero() -> Self {
        Self {
            sign: 0,
            digits: vec![0],
        }
    }

    fn from_i64(value: i64) -> Self {
        if value == 0 {
            return Self::zero();
        }

        let mut result = Self::from_u64(value.unsigned_abs());
        result.sign = if value.is_negative() { -1 } else { 1 };
        result
    }

    fn from_u64(mut value: u64) -> Self {
        if value == 0 {
            return Self::zero();
        }

        let mut digits = Vec::new();
        while value > 0 {
            digits.push((value % 10) as u8);
            value /= 10;
        }
        Self { sign: 1, digits }
    }

    fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }

        let bits = value.to_bits();
        let is_negative = bits >> 63 != 0;
        let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1_u64 << 52) - 1);

        let (mantissa, binary_shift) = if exponent_bits == 0 {
            (fraction, -1_074)
        } else {
            ((1_u64 << 52) | fraction, exponent_bits - 1_023 - 52)
        };

        let mut result = if binary_shift < 0 {
            let right_shift = binary_shift.unsigned_abs();
            if right_shift >= u64::BITS {
                Self::zero()
            } else {
                Self::from_u64(mantissa >> right_shift)
            }
        } else {
            let mut integer = Self::from_u64(mantissa);
            for _ in 0..binary_shift {
                integer.multiply_by_two();
            }
            integer
        };

        if is_negative && !result.is_zero() {
            result.sign = -1;
        }
        Some(result)
    }

    fn from_decimal_string(value: &str) -> Option<Self> {
        let value = value.trim();
        let (sign, unsigned) = match value.as_bytes().first() {
            Some(b'+') => (1, &value[1..]),
            Some(b'-') => (-1, &value[1..]),
            _ => (1, value),
        };

        if unsigned.is_empty() {
            return None;
        }

        let bytes = unsigned.as_bytes();
        let mut forward_digits = Vec::with_capacity(bytes.len());
        for (index, byte) in bytes.iter().copied().enumerate() {
            if byte.is_ascii_digit() {
                forward_digits.push(byte - b'0');
            } else if byte == b'_'
                && index > 0
                && index + 1 < bytes.len()
                && bytes[index - 1].is_ascii_digit()
                && bytes[index + 1].is_ascii_digit()
            {
                continue;
            } else {
                return None;
            }
        }

        let first_nonzero = forward_digits
            .iter()
            .position(|digit| *digit != 0)
            .unwrap_or(forward_digits.len());
        if first_nonzero == forward_digits.len() {
            return Some(Self::zero());
        }

        let digits = forward_digits[first_nonzero..]
            .iter()
            .rev()
            .copied()
            .collect();
        Some(Self { sign, digits })
    }

    fn multiply_by_two(&mut self) {
        let mut carry = 0;
        for digit in &mut self.digits {
            let doubled = *digit * 2 + carry;
            *digit = doubled % 10;
            carry = doubled / 10;
        }
        if carry != 0 {
            self.digits.push(carry);
        }
    }

    fn is_zero(&self) -> bool {
        self.sign == 0
    }

    fn compare_magnitude(&self, other: &Self) -> Ordering {
        self.digits
            .len()
            .cmp(&other.digits.len())
            .then_with(|| self.digits.iter().rev().cmp(other.digits.iter().rev()))
    }
}

impl Ord for NormalizedInteger {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sign.cmp(&other.sign).then_with(|| match self.sign {
            -1 => other.compare_magnitude(self),
            1 => self.compare_magnitude(other),
            _ => Ordering::Equal,
        })
    }
}

impl PartialOrd for NormalizedInteger {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        CountsPayload, DEFAULT_REGION_NAME, GameCountValue, PlayerRegionInput, RegionCountEntry,
        SENTINEL_NONE, infer_region_from_counts, region_split_mismatches, resolve_region,
        summarize_region,
    };

    fn player<'a>(
        preferred_region: Option<&'a str>,
        inferred_region: Option<&'a str>,
    ) -> PlayerRegionInput<'a> {
        PlayerRegionInput {
            preferred_region,
            inferred_region,
        }
    }

    fn counts(
        entries: impl IntoIterator<Item = (&'static str, RegionCountEntry)>,
    ) -> CountsPayload {
        CountsPayload {
            region: Some(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value))
                    .collect::<HashMap<_, _>>(),
            ),
        }
    }

    #[test]
    fn test_more_us_east_games_infers_use() {
        let payload = counts([
            ("3", RegionCountEntry::games(1_000)),
            ("2", RegionCountEntry::games(30)),
            ("1", RegionCountEntry::games(20)),
        ]);
        assert_eq!(infer_region_from_counts(Some(&payload)), Some("USE"));
    }

    #[test]
    fn test_more_us_west_games_infers_usw() {
        let payload = counts([
            ("1", RegionCountEntry::games(50)),
            ("2", RegionCountEntry::games(12)),
        ]);
        assert_eq!(infer_region_from_counts(Some(&payload)), Some("USW"));
    }

    #[test]
    fn test_equal_us_games_defaults_west() {
        let payload = counts([
            ("1", RegionCountEntry::games(40)),
            ("2", RegionCountEntry::games(40)),
        ]);
        assert_eq!(infer_region_from_counts(Some(&payload)), Some("USW"));
    }

    #[test]
    fn test_no_us_games_returns_sentinel() {
        let payload = counts([("3", RegionCountEntry::games(500))]);
        assert_eq!(
            infer_region_from_counts(Some(&payload)),
            Some(SENTINEL_NONE)
        );
    }

    #[test]
    fn test_empty_payload_returns_sentinel() {
        let empty_region = CountsPayload {
            region: Some(HashMap::new()),
        };
        let missing_region = CountsPayload::default();

        assert_eq!(
            infer_region_from_counts(Some(&empty_region)),
            Some(SENTINEL_NONE)
        );
        assert_eq!(
            infer_region_from_counts(Some(&missing_region)),
            Some(SENTINEL_NONE)
        );
    }

    #[test]
    fn test_missing_payload_returns_none_for_retry() {
        assert_eq!(infer_region_from_counts(None), None);
    }

    #[test]
    fn test_handles_non_dict_region_values() {
        let payload = counts([
            ("2", RegionCountEntry::bare(5)),
            ("1", RegionCountEntry::bare(3)),
        ]);
        assert_eq!(infer_region_from_counts(Some(&payload)), Some("USE"));
    }

    #[test]
    fn test_handles_string_and_malformed_game_values() {
        let numeric_strings = counts([
            ("2", RegionCountEntry::games("  +1_000 ")),
            ("1", RegionCountEntry::bare("999")),
        ]);
        assert_eq!(
            infer_region_from_counts(Some(&numeric_strings)),
            Some("USE")
        );

        let malformed = counts([
            ("2", RegionCountEntry::games(GameCountValue::Malformed)),
            ("1", RegionCountEntry::games("not-a-number")),
        ]);
        assert_eq!(
            infer_region_from_counts(Some(&malformed)),
            Some(SENTINEL_NONE)
        );
    }

    #[test]
    fn test_explicit_pick_wins_over_inferred() {
        assert_eq!(
            resolve_region(&player(Some("USW"), Some("USE"))),
            Some("USW")
        );
    }

    #[test]
    fn test_inferred_used_when_no_explicit() {
        assert_eq!(resolve_region(&player(None, Some("USE"))), Some("USE"));
    }

    #[test]
    fn test_sentinel_and_unset_resolve_to_none() {
        assert_eq!(resolve_region(&player(None, Some(SENTINEL_NONE))), None);
        assert_eq!(resolve_region(&player(None, None)), None);
    }

    #[test]
    fn test_majority_wins() {
        let mut players = vec![player(Some("USE"), None); 6];
        players.extend(vec![player(Some("USW"), None); 4]);
        assert_eq!(summarize_region(&players), "US East");
    }

    #[test]
    fn test_tie_defaults_to_west() {
        let mut players = vec![player(Some("USE"), None); 5];
        players.extend(vec![player(Some("USW"), None); 5]);
        assert_eq!(summarize_region(&players), "US West");
    }

    #[test]
    fn test_no_votes_defaults_to_west() {
        let players = [player(None, None), player(None, Some(SENTINEL_NONE))];
        assert_eq!(summarize_region(&players), DEFAULT_REGION_NAME);
    }

    #[test]
    fn test_inferred_votes_count() {
        let players = [
            player(None, Some("USE")),
            player(None, Some("USE")),
            player(Some("USW"), None),
        ];
        assert_eq!(summarize_region(&players), "US East");
    }

    #[test]
    fn test_clean_split_has_no_mismatches() {
        let team1 = [player(Some("USW"), None); 5];
        let team2 = [player(Some("USE"), None); 5];
        assert_eq!(region_split_mismatches(&team1, &team2), 0);
        assert_eq!(region_split_mismatches(&team2, &team1), 0);
    }

    #[test]
    fn test_mixed_split_counts_best_orientation() {
        let mut team1 = vec![player(Some("USW"), None); 4];
        team1.push(player(Some("USE"), None));
        let mut team2 = vec![player(Some("USE"), None); 4];
        team2.push(player(Some("USW"), None));
        assert_eq!(region_split_mismatches(&team1, &team2), 2);
    }

    #[test]
    fn test_unset_players_do_not_count() {
        let team1 = [player(Some("USW"), None), player(None, None)];
        let team2 = [player(Some("USE"), None), player(None, Some(SENTINEL_NONE))];
        assert_eq!(region_split_mismatches(&team1, &team2), 0);
    }
}
