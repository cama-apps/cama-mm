//! Parity-harness mirror of wheel outcome math — NOT the live wheel.
//!
//! WARNING: the production wheel catalog, payouts, and presentation live in
//! the application crate's `wheel` module (the calibrated, Python-parity
//! tables) and in the runtime's betting provider. Never wire this module into
//! the runtime. It survives only because the xtask domain-vector harness
//! needs a pure, callable copy of the Eruption math: the live implementation
//! is inlined in the runtime's wheel resolver (`cama-runtime`
//! `betting_provider`) and is coupled to the database. Keep
//! [`eruption_reward`] in sync with that resolver.
#![doc(hidden)]

use crate::economy_scaling::scale_minigame_jc_delta;

/// Pure mirror of the runtime's Eruption resolution: double the magnitude of
/// the last settled spin without rescaling it, or fall back to a scaled 50.
#[doc(hidden)]
#[must_use]
pub fn eruption_reward(last_settled_result: Option<i64>, minigame_jc_delta_scale: f64) -> i64 {
    match last_settled_result.filter(|value| *value != 0) {
        Some(value) => value.unsigned_abs().saturating_mul(2).min(i64::MAX as u64) as i64,
        None => scale_minigame_jc_delta(50.0, minigame_jc_delta_scale),
    }
}

/// Character-exact mirror of Python's EMERGENCY embed description
/// (`commands/betting_helpers/wheel_embeds.py`). The live runtime surfaces
/// the same scaled loss cap through its hostile-result note instead, so this
/// exists only for its parity test below.
#[cfg(test)]
fn emergency_description(
    player_count: i64,
    total_drained: i64,
    minigame_jc_delta_scale: f64,
) -> String {
    use crate::formatting::JOPACOIN_EMOTE;

    let loss_cap = scale_minigame_jc_delta(20.0, minigame_jc_delta_scale);
    format!(
        "**SOS**\n\n🚨 Economic crisis triggered!\n\n**{player_count}** players each lost up to **{loss_cap}** {JOPACOIN_EMOTE}.\nTotal drained: **{total_drained}** {JOPACOIN_EMOTE} (vanished).\n\n*No one is safe. Not even you.*"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
