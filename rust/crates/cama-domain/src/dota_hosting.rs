//! Validated lobby preferences shared by command, storage, and hosting adapters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum age of an authoritative owned-lobby observation for hosted wagers.
pub const BETTING_OBSERVATION_TTL_SECONDS: i64 = 90;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostingMode {
    #[default]
    Bot,
    Manual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstPick {
    Radiant,
    Dire,
    Random,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    #[default]
    Automatic,
    Manual,
}

/// `None` inherits the next settings layer; an empty value changes no settings.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DotaHostingOptions {
    pub hosting: Option<HostingMode>,
    pub region: Option<u32>,
    pub game_mode: Option<u32>,
    pub tv_delay: Option<u32>,
    pub league_id: Option<u32>,
    pub first_pick: Option<FirstPick>,
    pub start: Option<StartMode>,
    pub visibility: Option<u32>,
}

impl DotaHostingOptions {
    /// Explain the frozen hosting choice on shuffle and captain-draft results.
    #[must_use]
    pub fn lobby_instructions(&self, bot_busy: bool) -> Option<String> {
        match self.hosting {
            Some(HostingMode::Manual) => Some(format!(
                "{}Manual hosting: create the Dota lobby yourself using the teams above, then use `/record` to submit the result.\n📻 Live map and commentary are unavailable for this manually hosted match.",
                if bot_busy {
                    "The bot is hosting another match. "
                } else {
                    ""
                }
            )),
            Some(HostingMode::Bot) => Some(format!(
                "Bot hosted. {}",
                if self.start == Some(StartMode::Manual) {
                    "An admin starts the game when everyone is ready."
                } else {
                    "The bot starts the game when everyone is ready."
                }
            )),
            None => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.region.is_some_and(|value| !(1..=100).contains(&value)) {
            return Err("Dota server region must be between 1 and 100.".into());
        }
        if self
            .game_mode
            .is_some_and(|value| !matches!(value, 1 | 2 | 3 | 4 | 5 | 12 | 16 | 18 | 20 | 22 | 23))
        {
            return Err("Choose a supported ten-player Dota game mode.".into());
        }
        if self.tv_delay.is_some_and(|value| value > 4) {
            return Err("Dota TV delay must be between 0 and 4.".into());
        }
        if self.league_id == Some(0) {
            return Err("Dota league ID must be positive.".into());
        }
        if self.visibility.is_some_and(|value| !matches!(value, 0 | 2)) {
            return Err("Dota lobby visibility must be public (0) or unlisted (2).".into());
        }
        Ok(())
    }

    #[must_use]
    pub fn merged(&self, overrides: &Self) -> Self {
        Self {
            hosting: overrides.hosting.or(self.hosting),
            region: overrides.region.or(self.region),
            game_mode: overrides.game_mode.or(self.game_mode),
            tv_delay: overrides.tv_delay.or(self.tv_delay),
            league_id: overrides.league_id.or(self.league_id),
            first_pick: overrides.first_pick.or(self.first_pick),
            start: overrides.start.or(self.start),
            visibility: overrides.visibility.or(self.visibility),
        }
    }

    /// Read the durable per-match snapshot without depending on a database type.
    pub fn from_extra(extra: &BTreeMap<String, Value>) -> Result<Self, String> {
        let options: Self = extra.get("dota_hosting").map_or_else(
            || Ok(Self::default()),
            |value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()),
        )?;
        options.validate()?;
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_preserves_defaults_and_applies_explicit_zero_values() {
        let base = DotaHostingOptions {
            hosting: Some(HostingMode::Bot),
            region: Some(31),
            game_mode: Some(2),
            tv_delay: Some(3),
            league_id: Some(19144),
            first_pick: Some(FirstPick::Radiant),
            start: Some(StartMode::Automatic),
            visibility: Some(2),
        };
        let patch = DotaHostingOptions {
            hosting: Some(HostingMode::Manual),
            tv_delay: Some(0),
            first_pick: Some(FirstPick::Random),
            start: Some(StartMode::Manual),
            visibility: Some(0),
            ..Default::default()
        };
        assert_eq!(base.merged(&Default::default()), base);
        assert_eq!(
            base.merged(&patch),
            DotaHostingOptions {
                region: Some(31),
                game_mode: Some(2),
                league_id: Some(19144),
                ..patch
            }
        );
    }

    #[test]
    fn validates_supported_modes_and_rejects_incompatible_or_out_of_range_options() {
        for mode in [1, 2, 3, 4, 5, 12, 16, 18, 20, 22, 23] {
            assert!(
                DotaHostingOptions {
                    game_mode: Some(mode),
                    ..Default::default()
                }
                .validate()
                .is_ok()
            );
        }
        for value in [
            json!({"region":0}),
            json!({"region":101}),
            json!({"game_mode":21}),
            json!({"tv_delay":5}),
            json!({"league_id":0}),
            json!({"visibility":1}),
        ] {
            assert!(
                serde_json::from_value::<DotaHostingOptions>(value)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }

    #[test]
    fn manual_hosting_explains_spectator_unavailability_without_changing_recording() {
        let options = DotaHostingOptions {
            hosting: Some(HostingMode::Manual),
            ..Default::default()
        };
        for busy in [false, true] {
            let instructions = options.lobby_instructions(busy).unwrap();
            assert!(instructions.contains("create the Dota lobby yourself"));
            assert!(instructions.contains("`/record`"));
            assert!(instructions.contains("Live map and commentary are unavailable"));
            assert_eq!(instructions.contains("hosting another match"), busy);
        }
    }

    #[test]
    fn bot_hosting_with_manual_start_does_not_claim_spectators_are_unavailable() {
        let options = DotaHostingOptions {
            hosting: Some(HostingMode::Bot),
            start: Some(StartMode::Manual),
            ..Default::default()
        };
        let instructions = options.lobby_instructions(false).unwrap();
        assert!(instructions.contains("An admin starts the game"));
        assert!(!instructions.contains("unavailable"));
        assert!(
            DotaHostingOptions::default()
                .lobby_instructions(false)
                .is_none()
        );
    }

    #[test]
    fn snapshots_are_backward_compatible_but_malformed_options_fail_closed() {
        assert_eq!(
            DotaHostingOptions::from_extra(&BTreeMap::new()).unwrap(),
            DotaHostingOptions::default()
        );
        let extra = BTreeMap::from([(
            "dota_hosting".into(),
            json!({"hosting":"manual","first_pick":"dire","start":"manual"}),
        )]);
        let options = DotaHostingOptions::from_extra(&extra).unwrap();
        assert_eq!(options.hosting, Some(HostingMode::Manual));
        assert_eq!(options.first_pick, Some(FirstPick::Dire));
        assert_eq!(options.start, Some(StartMode::Manual));
        for value in [
            json!({"region":-1}),
            json!({"region":0}),
            json!({"hosting":"unknown"}),
            json!({"regoin":31}),
            Value::Null,
        ] {
            assert!(
                DotaHostingOptions::from_extra(&BTreeMap::from([("dota_hosting".into(), value)]))
                    .is_err()
            );
        }
    }
}
