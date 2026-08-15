//! Pure wheel catalog, payout, and result-copy contracts.
//!
//! WARNING — stale catalog with no production caller. The live,
//! Python-parity wheel tables (calibrated values included) are in
//! `cama_app::wheel`; this module retains pre-calibration constants only for
//! its own historical tests. Do not wire it to the runtime.

use crate::economy_scaling::scale_minigame_jc_delta;
use crate::formatting::JOPACOIN_EMOTE;

pub const WHEEL_EXPLOSION_REWARD: i64 = 67;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BaseValue {
    Numeric(i64),
    Special(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WedgeValue {
    Numeric(i64),
    Special(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WheelWedge {
    pub label: String,
    pub value: WedgeValue,
    pub color: &'static str,
}

const REGULAR_BASE: [(&str, BaseValue, &str); 24] = [
    ("BOMB", BaseValue::Special("BOMB_OMB"), "#0d0d0d"),
    ("BANKRUPT", BaseValue::Numeric(-100), "#1a1a1a"),
    ("BANKRUPT", BaseValue::Numeric(-100), "#1a1a1a"),
    ("GREEN", BaseValue::Special("GREEN_SHELL"), "#228b22"),
    ("30", BaseValue::Numeric(30), "#2980b9"),
    ("5", BaseValue::Numeric(5), "#2d5a27"),
    ("25", BaseValue::Numeric(25), "#3498db"),
    ("BLUE", BaseValue::Special("BLUE_SHELL"), "#3498db"),
    ("10", BaseValue::Numeric(10), "#3d7a37"),
    ("10", BaseValue::Numeric(10), "#3d7a37"),
    ("LOSE", BaseValue::Numeric(0), "#4a4a4a"),
    ("15", BaseValue::Numeric(15), "#4d9a47"),
    ("20", BaseValue::Numeric(20), "#5dba57"),
    ("50", BaseValue::Numeric(50), "#7d3c98"),
    ("50", BaseValue::Numeric(50), "#7d3c98"),
    ("40", BaseValue::Numeric(40), "#9b59b6"),
    ("80", BaseValue::Numeric(80), "#c0392b"),
    ("70", BaseValue::Numeric(70), "#d35400"),
    ("60", BaseValue::Numeric(60), "#e67e22"),
    ("RED", BaseValue::Special("RED_SHELL"), "#e74c3c"),
    ("100", BaseValue::Numeric(100), "#f1c40f"),
    ("100", BaseValue::Numeric(100), "#f1c40f"),
    ("BOLT", BaseValue::Special("LIGHTNING_BOLT"), "#f39c12"),
    ("BANANA", BaseValue::Special("BANANA_PEEL"), "#ffe14a"),
];

const GOLDEN_BASE: [(&str, BaseValue, &str); 24] = [
    ("BOMB", BaseValue::Special("BOMB_OMB"), "#0d0d0d"),
    ("GREEN", BaseValue::Special("GREEN_SHELL"), "#228b22"),
    ("RECESSION", BaseValue::Special("RECESSION"), "#3a0a0a"),
    ("BLUE", BaseValue::Special("BLUE_SHELL"), "#4080c0"),
    ("OVEREXTENDED", BaseValue::Numeric(-398), "#4a3000"),
    ("OVEREXTENDED", BaseValue::Numeric(-398), "#4a3000"),
    ("DIVIDEND", BaseValue::Special("DIVIDEND"), "#4a7000"),
    ("TRICKLE", BaseValue::Special("TRICKLE_DOWN"), "#5c7a00"),
    (
        "TAKEOVER",
        BaseValue::Special("HOSTILE_TAKEOVER"),
        "#6a2a80",
    ),
    (
        "COMPOUND",
        BaseValue::Special("COMPOUND_INTEREST"),
        "#6b8c00",
    ),
    ("HEIST", BaseValue::Special("HEIST"), "#7a5c00"),
    ("HEIST", BaseValue::Special("HEIST"), "#7a5c00"),
    ("CRASH", BaseValue::Special("MARKET_CRASH"), "#8a4000"),
    ("20", BaseValue::Numeric(20), "#b8860b"),
    ("30", BaseValue::Numeric(30), "#c8a000"),
    ("RED", BaseValue::Special("RED_SHELL"), "#cc6600"),
    ("50", BaseValue::Numeric(50), "#d4a000"),
    ("40", BaseValue::Numeric(40), "#daa520"),
    ("60", BaseValue::Numeric(60), "#e8b400"),
    ("150", BaseValue::Numeric(150), "#e8d080"),
    ("80", BaseValue::Numeric(80), "#f0c000"),
    ("100", BaseValue::Numeric(100), "#f5c500"),
    ("BANANA", BaseValue::Special("BANANA_PEEL"), "#ffe14a"),
    ("CROWN", BaseValue::Numeric(250), "#fffacd"),
];

fn scaled_catalog(
    base: &[(&'static str, BaseValue, &'static str)],
    minigame_jc_delta_scale: f64,
) -> Vec<WheelWedge> {
    base.iter()
        .map(|(label, value, color)| match value {
            BaseValue::Numeric(value) => {
                let value = scale_minigame_jc_delta(*value as f64, minigame_jc_delta_scale);
                WheelWedge {
                    label: if value >= 0 {
                        value.to_string()
                    } else {
                        (*label).to_owned()
                    },
                    value: WedgeValue::Numeric(value),
                    color,
                }
            }
            BaseValue::Special(value) => WheelWedge {
                label: (*label).to_owned(),
                value: WedgeValue::Special(value),
                color,
            },
        })
        .collect()
}

/// Regular wheel catalog with numeric display values scaled exactly once.
#[must_use]
pub fn wheel_wedges(minigame_jc_delta_scale: f64) -> Vec<WheelWedge> {
    scaled_catalog(&REGULAR_BASE, minigame_jc_delta_scale)
}

/// Golden wheel catalog with numeric display values scaled exactly once.
#[must_use]
pub fn golden_wheel_wedges(minigame_jc_delta_scale: f64) -> Vec<WheelWedge> {
    scaled_catalog(&GOLDEN_BASE, minigame_jc_delta_scale)
}

/// Eruption copies a settled numeric result without scaling it a second time.
#[must_use]
pub fn eruption_reward(last_settled_result: Option<i64>, minigame_jc_delta_scale: f64) -> i64 {
    match last_settled_result.filter(|value| *value != 0) {
        Some(value) => value.unsigned_abs().saturating_mul(2).min(i64::MAX as u64) as i64,
        None => scale_minigame_jc_delta(50.0, minigame_jc_delta_scale),
    }
}

#[must_use]
pub fn emergency_description(
    player_count: i64,
    total_drained: i64,
    minigame_jc_delta_scale: f64,
) -> String {
    let loss_cap = scale_minigame_jc_delta(20.0, minigame_jc_delta_scale);
    format!(
        "**SOS**\n\n🚨 Economic crisis triggered!\n\n**{player_count}** players each lost up to **{loss_cap}** {JOPACOIN_EMOTE}.\nTotal drained: **{total_drained}** {JOPACOIN_EMOTE} (vanished).\n\n*No one is safe. Not even you.*"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn test_wheel_numeric_wedges_are_scaled_for_display_and_payout() {
        let wedges = wheel_wedges(1.0);
        let values = wedges
            .iter()
            .filter_map(|wedge| match wedge.value {
                WedgeValue::Numeric(value) => Some(value),
                WedgeValue::Special(_) => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(values.contains(&5));
        assert!(values.contains(&100));
        assert!(
            wedges
                .iter()
                .any(|wedge| { wedge.label == "0" && wedge.value == WedgeValue::Numeric(0) })
        );
        for wedge in wedges {
            if let WedgeValue::Numeric(value) = wedge.value
                && value > 0
            {
                assert_eq!(wedge.label, value.to_string());
            }
        }
    }

    #[test]
    fn test_golden_wheel_numeric_wedges_are_scaled_for_display_and_payout() {
        let wedges = golden_wheel_wedges(1.0);
        let values = wedges
            .iter()
            .filter_map(|wedge| match wedge.value {
                WedgeValue::Numeric(value) => Some(value),
                WedgeValue::Special(_) => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(values.contains(&20));
        assert!(values.contains(&250));
        for wedge in wedges {
            if let WedgeValue::Numeric(value) = wedge.value
                && value > 0
            {
                assert_eq!(wedge.label, value.to_string());
            }
        }
    }

    #[test]
    fn test_wheel_explosion_keeps_legacy_sixty_seven_reward() {
        assert_eq!(WHEEL_EXPLOSION_REWARD, 67);
    }

    #[test]
    fn test_emergency_embed_displays_scaled_loss_cap() {
        let description = emergency_description(3, 48, 1.0);
        let expected_cap = scale_minigame_jc_delta(20.0, 1.0);
        assert!(description.contains(&format!("up to **{expected_cap}**")));
    }

    #[test]
    fn test_eruption_does_not_rescale_a_settled_spin() {
        assert_eq!(eruption_reward(Some(40), 1.0), 80);
        assert_eq!(eruption_reward(Some(-24), 1.0), 48);
        assert_eq!(
            eruption_reward(None, 1.0),
            scale_minigame_jc_delta(50.0, 1.0)
        );
    }
}
