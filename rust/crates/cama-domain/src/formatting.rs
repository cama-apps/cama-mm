//! Shared display formatting and Discord-safe text helpers.

/// Role identifiers paired with their display emoji.
pub const ROLE_EMOJIS: [(&str, &str); 5] = [
    ("1", "⚔️"),
    ("2", "🏹"),
    ("3", "🛡️"),
    ("4", "🔥"),
    ("5", "⚕️"),
];

/// Role identifiers paired with their friendly names.
pub const ROLE_NAMES: [(&str, &str); 5] = [
    ("1", "Carry"),
    ("2", "Mid"),
    ("3", "Offlane"),
    ("4", "Soft Support"),
    ("5", "Hard Support"),
];

/// Custom Jopacoin emote used across embeds and messages.
pub const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";

/// Discord identifier for [`JOPACOIN_EMOTE`].
pub const JOPACOIN_EMOJI_ID: u64 = 954_159_801_049_440_297;

/// Tombstone emoji used for players with an active bankruptcy penalty.
pub const TOMBSTONE_EMOJI: &str = "🪦";

/// Seed amounts supplied by the house to a pool bet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PoolOddsSeeds {
    /// Seed applied to the Radiant side of the pool.
    pub seed_radiant: i64,
    /// Seed applied to the Dire side of the pool.
    pub seed_dire: i64,
}

/// Optional state used while formatting the betting field of an embed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BettingDisplayOptions {
    /// Unix timestamp at which betting closes.
    pub lock_until: Option<i64>,
    /// Whether betting has already closed.
    pub locked: bool,
    /// Seed applied to the Radiant side in pool mode.
    pub seed_radiant: i64,
    /// Seed applied to the Dire side in pool mode.
    pub seed_dire: i64,
    /// Winner bonus advertised in house mode.
    pub seed_bonus: i64,
}

/// Rank-band information for the two-tier rich-spectator wager policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectatorAutobetTiers {
    /// Number of eligible rich spectators.
    pub total_count: i64,
    /// Number of top-ranked spectators using `top_percentage`.
    pub top_count: i64,
    /// Net-worth fraction wagered by top-ranked spectators.
    pub top_percentage: f64,
}

/// Typed input for a rich-spectator auto-wager summary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectatorAutobetSummaryInput {
    /// Number of spectator wagers created.
    pub created: i64,
    /// Net-worth fraction wagered by the secondary or legacy tier.
    pub percentage: f64,
    /// Total Jopacoin wagered on Radiant.
    pub total_radiant: i64,
    /// Total Jopacoin wagered on Dire.
    pub total_dire: i64,
    /// Two-tier rank bands, or `None` for the legacy single-tier copy.
    pub tiers: Option<SpectatorAutobetTiers>,
}

/// Escape Discord markdown characters and neutralize user-controlled mentions.
#[must_use]
pub fn escape_discord_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '@' {
            escaped.push_str("@\u{200b}");
        } else if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '>'
                | '~'
        ) {
            escaped.push('\\');
            escaped.push(character);
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// Return a role with its emoji and friendly name, such as `⚔️ Carry`.
#[must_use]
pub fn format_role_display(role: &str) -> String {
    let emoji = role_value(&ROLE_EMOJIS, role).unwrap_or("");
    let name = role_value(&ROLE_NAMES, role).unwrap_or(role);
    format!("{emoji} {name}").trim().to_owned()
}

/// Calculate pool-betting multipliers for Radiant and Dire.
///
/// The returned multiplier is the total amount returned for one unit wagered.
/// A side with no wagers has no defined multiplier. Seeds intentionally enter
/// the opposite side's numerator without changing the player-funded pool total,
/// matching the Python payout contract.
#[must_use]
pub fn calculate_pool_odds(
    radiant_total: i64,
    dire_total: i64,
    seeds: PoolOddsSeeds,
) -> (Option<f64>, Option<f64>) {
    let total_pool = i128::from(radiant_total) + i128::from(dire_total);
    if total_pool == 0 {
        return (None, None);
    }

    let radiant_multiplier = (radiant_total > 0)
        .then(|| (total_pool + i128::from(seeds.seed_dire)) as f64 / radiant_total as f64);
    let dire_multiplier = (dire_total > 0)
        .then(|| (total_pool + i128::from(seeds.seed_radiant)) as f64 / dire_total as f64);
    (radiant_multiplier, dire_multiplier)
}

/// Format the betting field name and value for an embed.
///
/// `betting_mode == "pool"` selects pool behavior; every other value retains
/// the legacy house-mode fallback. Once `options.locked` is true, the closed
/// label replaces any relative countdown.
#[must_use]
pub fn format_betting_display(
    radiant_total: i64,
    dire_total: i64,
    betting_mode: &str,
    options: BettingDisplayOptions,
) -> (String, String) {
    let lock_text = if options.locked {
        Some("🔒 Betting closed".to_owned())
    } else {
        options
            .lock_until
            .filter(|timestamp| *timestamp != 0)
            .map(|timestamp| format!("Closes <t:{timestamp}:R>"))
    };

    if betting_mode == "pool" {
        let (radiant_multiplier, dire_multiplier) = calculate_pool_odds(
            radiant_total,
            dire_total,
            PoolOddsSeeds {
                seed_radiant: options.seed_radiant,
                seed_dire: options.seed_dire,
            },
        );
        let radiant_odds = format_pool_multiplier(radiant_multiplier);
        let dire_odds = format_pool_multiplier(dire_multiplier);
        let mut totals_text = format!(
            "Radiant: {radiant_total} {JOPACOIN_EMOTE} {radiant_odds} | Dire: {dire_total} {JOPACOIN_EMOTE} {dire_odds}"
        );

        if options.seed_radiant != 0 || options.seed_dire != 0 {
            totals_text.push_str(&format!(
                "\nSeed: Radiant {} {JOPACOIN_EMOTE} | Dire {} {JOPACOIN_EMOTE}",
                options.seed_radiant, options.seed_dire
            ));
        }
        if let Some(lock_text) = lock_text {
            totals_text.push('\n');
            totals_text.push_str(&lock_text);
        }

        ("💰 Pool Betting".to_owned(), totals_text)
    } else {
        let mut totals_text = format!(
            "Radiant: {radiant_total} {JOPACOIN_EMOTE} | Dire: {dire_total} {JOPACOIN_EMOTE}"
        );
        if options.seed_bonus != 0 {
            totals_text.push_str(&format!(
                "\nWinner bonus: {} {JOPACOIN_EMOTE}",
                options.seed_bonus
            ));
        }
        if let Some(lock_text) = lock_text {
            totals_text.push('\n');
            totals_text.push_str(&lock_text);
        }

        ("💰 House Betting (1:1)".to_owned(), totals_text)
    }
}

/// Format the compact shuffle or draft summary for rich spectator auto-wagers.
#[must_use]
pub fn format_spectator_autobet_summary(input: &SpectatorAutobetSummaryInput) -> String {
    let secondary_percentage = format_python_general(input.percentage * 100.0);
    let headline = if let Some(tiers) = input.tiers {
        let top_percentage = format_python_general(tiers.top_percentage * 100.0);
        let mut rank_bands = Vec::with_capacity(2);
        if tiers.top_count > 0 {
            rank_bands.push(format!("ranks 1–{} at {top_percentage}%", tiers.top_count));
        }
        if tiers.total_count > tiers.top_count {
            rank_bands.push(format!(
                "ranks {}–{} at {secondary_percentage}%",
                tiers.top_count + 1,
                tiers.total_count
            ));
        }
        format!(
            "**Rich spectator auto-wagers:** {} spectators ({})",
            input.created,
            rank_bands.join("; ")
        )
    } else {
        format!(
            "**Rich spectator auto-wagers:** {} spectators wagered {secondary_percentage}% of net worth",
            input.created
        )
    };

    format!(
        "{headline}\n🟢 Radiant: {} {JOPACOIN_EMOTE} | 🔴 Dire: {} {JOPACOIN_EMOTE}",
        input.total_radiant, input.total_dire
    )
}

/// Format a duration as a concise string such as `1m`, `2h`, or `1d`.
#[must_use]
pub fn format_duration_short(seconds: f64) -> String {
    let seconds = seconds as i64;
    if seconds < 60 {
        format!("{}s", seconds.max(1))
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn role_value(configuration: &[(&'static str, &'static str)], role: &str) -> Option<&'static str> {
    configuration
        .iter()
        .find_map(|(identifier, value)| (*identifier == role).then_some(*value))
}

fn format_pool_multiplier(multiplier: Option<f64>) -> String {
    multiplier
        .filter(|value| *value != 0.0)
        .map_or_else(|| "(—)".to_owned(), |value| format!("({value:.2}x)"))
}

/// Match Python's default `:g` formatting for spectator percentages: six
/// significant digits, trailing fractional zeroes removed, and scientific
/// notation outside the `-4..6` exponent range.
fn format_python_general(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value == f64::INFINITY {
        return "inf".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    const SIGNIFICANT_DIGITS: usize = 6;
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust scientific float formatting includes an exponent");
    let exponent = exponent
        .parse::<i32>()
        .expect("Rust emits a numeric scientific exponent");

    if !(-4..SIGNIFICANT_DIGITS as i32).contains(&exponent) {
        let mantissa = trim_fractional_zeroes(mantissa.to_owned());
        return format!("{mantissa}e{exponent:+03}");
    }

    let fractional_digits = (SIGNIFICANT_DIGITS as i32 - exponent - 1).max(0) as usize;
    trim_fractional_zeroes(format!("{value:.fractional_digits$}"))
}

fn trim_fractional_zeroes(mut value: String) -> String {
    if value.contains('.') {
        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.pop();
        }
    }
    value
}

/// CPython `round(value, ndigits)`: correctly-rounded decimal rounding of the
/// exact binary value (ties to even). Multiplying by `10^n` and calling
/// `round_ties_even` introduces an intermediate binary rounding that
/// manufactures false ties; fixed-precision formatting performs the same
/// correctly-rounded conversion as CPython's dtoa-based `round`.
#[must_use]
pub fn python_round_decimals(value: f64, decimals: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{value:.decimals$}").parse().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::{
        BettingDisplayOptions, JOPACOIN_EMOTE, PoolOddsSeeds, ROLE_EMOJIS, ROLE_NAMES,
        SpectatorAutobetSummaryInput, SpectatorAutobetTiers, calculate_pool_odds,
        format_betting_display, format_role_display, format_spectator_autobet_summary,
        python_round_decimals,
    };

    #[test]
    fn test_format_role_display_uses_role_configuration() {
        for role in ["1", "2", "3", "4", "5"] {
            let emoji = ROLE_EMOJIS
                .iter()
                .find_map(|(identifier, emoji)| (*identifier == role).then_some(*emoji))
                .expect("configured role has an emoji");
            let name = ROLE_NAMES
                .iter()
                .find_map(|(identifier, name)| (*identifier == role).then_some(*name))
                .expect("configured role has a name");
            assert_eq!(format_role_display(role), format!("{emoji} {name}"));
        }
    }

    #[test]
    fn test_calculate_pool_odds_even_split() {
        let (radiant_multiplier, dire_multiplier) =
            calculate_pool_odds(100, 100, PoolOddsSeeds::default());

        assert_eq!(radiant_multiplier, Some(2.0));
        assert_eq!(dire_multiplier, Some(2.0));
    }

    #[test]
    fn test_calculate_pool_odds_underdog() {
        let (radiant_multiplier, dire_multiplier) =
            calculate_pool_odds(100, 300, PoolOddsSeeds::default());

        assert_eq!(radiant_multiplier, Some(4.0));
        assert!((dire_multiplier.expect("Dire has defined odds") - 1.33).abs() < 0.01);

        let (seeded_radiant, seeded_dire) = calculate_pool_odds(
            10,
            0,
            PoolOddsSeeds {
                seed_radiant: 25,
                seed_dire: 25,
            },
        );
        assert_eq!(seeded_radiant, Some(3.5));
        assert_eq!(seeded_dire, None);
    }

    #[test]
    fn test_calculate_pool_odds_empty_side() {
        let (radiant_multiplier, dire_multiplier) =
            calculate_pool_odds(100, 0, PoolOddsSeeds::default());
        assert_eq!(radiant_multiplier, Some(1.0));
        assert_eq!(dire_multiplier, None);

        let (radiant_multiplier, dire_multiplier) =
            calculate_pool_odds(0, 100, PoolOddsSeeds::default());
        assert_eq!(radiant_multiplier, None);
        assert_eq!(dire_multiplier, Some(1.0));
    }

    #[test]
    fn test_calculate_pool_odds_both_empty() {
        let (radiant_multiplier, dire_multiplier) =
            calculate_pool_odds(0, 0, PoolOddsSeeds::default());

        assert_eq!(radiant_multiplier, None);
        assert_eq!(dire_multiplier, None);
    }

    #[test]
    fn test_format_house_mode() {
        let (field_name, field_value) =
            format_betting_display(100, 200, "house", BettingDisplayOptions::default());

        assert_eq!(field_name, "💰 House Betting (1:1)");
        assert_eq!(
            field_value,
            format!("Radiant: 100 {JOPACOIN_EMOTE} | Dire: 200 {JOPACOIN_EMOTE}")
        );

        let (_, seeded_value) = format_betting_display(
            10,
            0,
            "house",
            BettingDisplayOptions {
                seed_bonus: 50,
                ..BettingDisplayOptions::default()
            },
        );
        assert!(seeded_value.contains(&format!("Winner bonus: 50 {JOPACOIN_EMOTE}")));
    }

    #[test]
    fn test_format_pool_mode() {
        let (field_name, field_value) =
            format_betting_display(100, 200, "pool", BettingDisplayOptions::default());

        assert_eq!(field_name, "💰 Pool Betting");
        assert_eq!(
            field_value,
            format!("Radiant: 100 {JOPACOIN_EMOTE} (3.00x) | Dire: 200 {JOPACOIN_EMOTE} (1.50x)")
        );

        let (_, seeded_value) = format_betting_display(
            10,
            0,
            "pool",
            BettingDisplayOptions {
                seed_radiant: 25,
                seed_dire: 25,
                ..BettingDisplayOptions::default()
            },
        );
        assert_eq!(
            seeded_value,
            format!(
                "Radiant: 10 {JOPACOIN_EMOTE} (3.50x) | Dire: 0 {JOPACOIN_EMOTE} (—)\nSeed: Radiant 25 {JOPACOIN_EMOTE} | Dire 25 {JOPACOIN_EMOTE}"
            )
        );
    }

    #[test]
    fn test_format_pool_mode_empty_side() {
        let (_, field_value) =
            format_betting_display(0, 100, "pool", BettingDisplayOptions::default());

        assert_eq!(
            field_value,
            format!("Radiant: 0 {JOPACOIN_EMOTE} (—) | Dire: 100 {JOPACOIN_EMOTE} (1.00x)")
        );
    }

    #[test]
    fn test_format_with_lock_time() {
        let lock_timestamp = 1_704_067_200;
        let (_, field_value) = format_betting_display(
            100,
            100,
            "house",
            BettingDisplayOptions {
                lock_until: Some(lock_timestamp),
                ..BettingDisplayOptions::default()
            },
        );

        assert_eq!(
            field_value,
            format!(
                "Radiant: 100 {JOPACOIN_EMOTE} | Dire: 100 {JOPACOIN_EMOTE}\nCloses <t:{lock_timestamp}:R>"
            )
        );
    }

    #[test]
    fn test_format_pool_mode_equal_bets() {
        let (_, field_value) =
            format_betting_display(100, 100, "pool", BettingDisplayOptions::default());

        assert_eq!(
            field_value,
            format!("Radiant: 100 {JOPACOIN_EMOTE} (2.00x) | Dire: 100 {JOPACOIN_EMOTE} (2.00x)")
        );
    }

    #[test]
    fn test_format_locked_pool_shows_closed_not_countdown() {
        let (field_name, field_value) = format_betting_display(
            100,
            200,
            "pool",
            BettingDisplayOptions {
                lock_until: Some(1_704_067_200),
                locked: true,
                ..BettingDisplayOptions::default()
            },
        );

        assert_eq!(field_name, "💰 Pool Betting");
        assert_eq!(
            field_value,
            format!(
                "Radiant: 100 {JOPACOIN_EMOTE} (3.00x) | Dire: 200 {JOPACOIN_EMOTE} (1.50x)\n🔒 Betting closed"
            )
        );
        assert!(!field_value.contains("<t:"));
    }

    #[test]
    fn test_format_locked_house_shows_closed_not_countdown() {
        let (_, field_value) = format_betting_display(
            100,
            200,
            "house",
            BettingDisplayOptions {
                lock_until: Some(1_704_067_200),
                locked: true,
                ..BettingDisplayOptions::default()
            },
        );

        assert_eq!(
            field_value,
            format!(
                "Radiant: 100 {JOPACOIN_EMOTE} | Dire: 200 {JOPACOIN_EMOTE}\n🔒 Betting closed"
            )
        );
        assert!(!field_value.contains("<t:"));
    }

    #[test]
    fn test_format_spectator_autobet_summary_shows_both_compact_tiers() {
        let summary = format_spectator_autobet_summary(&SpectatorAutobetSummaryInput {
            created: 10,
            percentage: 0.01,
            total_radiant: 48,
            total_dire: 47,
            tiers: Some(SpectatorAutobetTiers {
                total_count: 10,
                top_count: 5,
                top_percentage: 0.02,
            }),
        });

        assert_eq!(
            summary,
            format!(
                "**Rich spectator auto-wagers:** 10 spectators (ranks 1–5 at 2%; ranks 6–10 at 1%)\n🟢 Radiant: 48 {JOPACOIN_EMOTE} | 🔴 Dire: 47 {JOPACOIN_EMOTE}"
            )
        );
    }

    #[test]
    fn test_format_spectator_autobet_summary_preserves_legacy_pending_draft_copy() {
        let summary = format_spectator_autobet_summary(&SpectatorAutobetSummaryInput {
            created: 5,
            percentage: 0.01,
            total_radiant: 8,
            total_dire: 7,
            tiers: None,
        });

        assert_eq!(
            summary,
            format!(
                "**Rich spectator auto-wagers:** 5 spectators wagered 1% of net worth\n🟢 Radiant: 8 {JOPACOIN_EMOTE} | 🔴 Dire: 7 {JOPACOIN_EMOTE}"
            )
        );
        assert!(!summary.contains("top 0"));
    }

    #[test]
    fn round_decimals_handles_binary_ties_and_precision() {
        let cases4 = [
            (0.96625_f64, 0.9663_f64),
            (0.94375, 0.9437),
            (0.966_349_999_9, 0.9663),
            (1.00005, 1.0001),
            (0.12345, 0.1235),
            (2.675, 2.675),
        ];
        for (value, expected) in cases4 {
            assert_eq!(
                python_round_decimals(value, 4),
                expected,
                "round({value}, 4)"
            );
        }
        // Rating uncertainty vectors: round(min(rd / 250 * 100, 100), 1).
        let cases1 = [
            (31.325_f64, 12.5_f64),
            (45.675, 18.3),
            (250.0, 100.0),
            (123.456, 49.4),
        ];
        for (rd, expected) in cases1 {
            let raw = (rd / 250.0 * 100.0_f64).min(100.0);
            assert_eq!(python_round_decimals(raw, 1), expected, "uncertainty({rd})");
        }
        assert_eq!(python_round_decimals(0.123_456_5, 6), 0.123_456);
        assert_eq!(python_round_decimals(0.123_457_5, 6), 0.123_457);
    }
}
