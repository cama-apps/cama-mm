//! Opt-in Deadlock rollout and rating-provider configuration.

use std::collections::{BTreeMap, BTreeSet};

use crate::application_config::Secret;

#[derive(Clone, Debug)]
pub struct DeadlockConfig {
    pub enabled: bool,
    /// Guilds explicitly enabled for the dedicated matchmaking channel.
    pub guild_ids: BTreeSet<i64>,
    /// Dedicated #deadlock-mm text channel per guild.
    pub channels: BTreeMap<i64, u64>,
    pub betting_window_seconds: i64,
    pub seed_amount: i64,
    pub api_key: Option<Secret>,
}

impl DeadlockConfig {
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Self, String> {
        fn flag(value: Option<String>, key: &str) -> Result<bool, String> {
            match value.as_deref().map(str::trim) {
                None | Some("") | Some("0" | "false" | "no") => Ok(false),
                Some("1" | "true" | "yes") => Ok(true),
                _ => Err(format!("{key} must be true or false")),
            }
        }
        let enabled = flag(lookup("DEADLOCK_ENABLED"), "DEADLOCK_ENABLED")?;
        let mut guild_ids: BTreeSet<i64> = lookup("DEADLOCK_GUILD_IDS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| "DEADLOCK_GUILD_IDS must contain positive guild IDs".to_owned())
            })
            .collect::<Result<_, _>>()?;
        let mut channels = BTreeMap::new();
        for pair in lookup("DEADLOCK_CHANNELS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let (guild, channel) = pair
                .split_once(':')
                .ok_or("DEADLOCK_CHANNELS expects guild_id:channel_id pairs")?;
            let guild = guild
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or("Invalid guild in DEADLOCK_CHANNELS")?;
            let channel = channel
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0 && i64::try_from(*id).is_ok())
                .ok_or("Invalid channel in DEADLOCK_CHANNELS")?;
            if channels.insert(guild, channel).is_some() {
                return Err("Duplicate guild in DEADLOCK_CHANNELS".into());
            }
        }
        if let Some(channel) = lookup("DEADLOCK_CHANNEL_ID").filter(|v| !v.trim().is_empty()) {
            if guild_ids.len() != 1 || !channels.is_empty() {
                return Err("DEADLOCK_CHANNEL_ID requires exactly one DEADLOCK_GUILD_IDS entry; use DEADLOCK_CHANNELS for multiple guilds".into());
            }
            let channel = channel
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0 && i64::try_from(*id).is_ok())
                .ok_or("Invalid DEADLOCK_CHANNEL_ID")?;
            channels.insert(*guild_ids.first().expect("one guild checked"), channel);
        }
        if guild_ids.is_empty() {
            guild_ids.extend(channels.keys().copied());
        }
        if enabled && (channels.is_empty() || guild_ids.iter().any(|id| !channels.contains_key(id)))
        {
            return Err("Configure a dedicated #deadlock-mm channel with DEADLOCK_CHANNELS=guild_id:channel_id (or DEADLOCK_GUILD_IDS plus DEADLOCK_CHANNEL_ID)".into());
        }
        let betting_window_seconds = lookup("DEADLOCK_BET_WINDOW_SECONDS")
            .map(|s| {
                s.parse::<i64>()
                    .map_err(|_| "DEADLOCK_BET_WINDOW_SECONDS must be an integer".to_owned())
            })
            .transpose()?
            .unwrap_or(180);
        if !(30..=3600).contains(&betting_window_seconds) {
            return Err("DEADLOCK_BET_WINDOW_SECONDS must be between 30 and 3600".to_owned());
        }
        let seed_amount = lookup("DEADLOCK_BET_SEED_AMOUNT")
            .map(|s| {
                s.parse::<i64>().map_err(|_| {
                    "DEADLOCK_BET_SEED_AMOUNT must be a nonnegative integer".to_owned()
                })
            })
            .transpose()?
            .unwrap_or(0);
        if seed_amount < 0 {
            return Err("DEADLOCK_BET_SEED_AMOUNT must be nonnegative".into());
        }
        let api_key = lookup("DEADLOCK_API_KEY")
            .filter(|v| !v.trim().is_empty())
            .map(Secret::new);
        Ok(Self {
            enabled,
            guild_ids,
            channels,
            betting_window_seconds,
            seed_amount,
            api_key,
        })
    }

    pub fn allows(&self, guild_id: i64) -> bool {
        self.enabled
            && guild_id > 0
            && self.guild_ids.contains(&guild_id)
            && self.channels.contains_key(&guild_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rating_import_does_not_require_or_read_statlocker_configuration() {
        let config = DeadlockConfig::from_lookup(|key| {
            assert_ne!(key, "STATLOCKER_API_KEY");
            assert_ne!(key, "DEADLOCK_STATLOCKER_STANDARD_CONFIRMED");
            None
        })
        .unwrap();
        assert!(config.api_key.is_none());
    }

    #[test]
    fn rollout_is_explicit_and_guild_scoped() {
        let disabled = DeadlockConfig::from_lookup(|_| None).unwrap();
        assert!(!disabled.allows(1));
        let enabled = DeadlockConfig::from_lookup(|k| match k {
            "DEADLOCK_ENABLED" => Some("true".into()),
            "DEADLOCK_CHANNELS" => Some("1:10,2:20".into()),
            _ => None,
        })
        .unwrap();
        assert!(enabled.allows(1));
        assert!(!enabled.allows(3));
        assert!(!enabled.allows(0));
    }
    #[test]
    fn malformed_security_and_window_configuration_fails_closed() {
        for (key, value) in [
            ("DEADLOCK_ENABLED", "maybe"),
            ("DEADLOCK_GUILD_IDS", "-1"),
            ("DEADLOCK_BET_WINDOW_SECONDS", "-1"),
        ] {
            assert!(DeadlockConfig::from_lookup(|k| (k == key).then(|| value.to_owned())).is_err());
        }
        let config = DeadlockConfig::from_lookup(|k| {
            (k == "DEADLOCK_API_KEY").then(|| "private-key".to_owned())
        })
        .unwrap();
        assert!(!format!("{config:?}").contains("private-key"));
    }
}
