//! Opt-in configuration for the dedicated Dota host and its live feed.

use std::{net::SocketAddr, path::PathBuf};

use crate::{ConfigError, Secret};

/// Dota's USSouthCentral/dfw lobby region (not its matchmaking group 1).
/// Verified in scripts/regions.txt from client build 25219194 (2026-09-10).
pub const US_SOUTH_CENTRAL_REGION: u32 = 31;

pub const ENV_KEYS: &[&str] = &[
    "DOTA_HOST_ENABLED",
    "DOTA_HOST_GUILD_IDS",
    "DOTA_STEAM_USERNAME",
    "DOTA_BOT_ACCOUNT_ID",
    "DOTA_STEAM_SESSION_PATH",
    "DOTA_HOST_START_AFTER",
    "DOTA_SERVER_REGION",
    "DOTA_GAME_MODE",
    "DOTA_TV_DELAY",
    "DOTA_LOBBY_TIMEOUT_SECONDS",
    "DOTA_STEAM_WEB_API_KEY",
    "DOTA_LIVE_BIND",
    "DOTA_LIVE_TOKEN",
    "DOTA_GSI_TOKEN",
    "DOTA_REPLAY_DIRECTORY",
    "DOTA_REPLAY_MAX_BYTES",
    "DOTA_REPLAY_RETENTION_DAYS",
];

#[derive(Clone, Debug, PartialEq)]
pub struct DotaHostConfig {
    pub guild_ids: Vec<i64>,
    pub username: Secret,
    pub account_id: u32,
    pub session_path: PathBuf,
    /// Do not adopt pre-activation manual matches. Persist this deployment value
    /// across restarts so queued matches remain eligible.
    pub start_after: i64,
    pub server_region: u32,
    pub game_mode: u32,
    pub tv_delay: u32,
    pub lobby_timeout_seconds: u64,
    pub web_api_key: Option<Secret>,
    pub live_bind: Option<SocketAddr>,
    pub live_token: Option<Secret>,
    pub gsi_token: Option<Secret>,
    pub replay_directory: PathBuf,
    pub replay_max_bytes: u64,
    pub replay_retention_days: u64,
}

impl DotaHostConfig {
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let invalid = |name, reason| ConfigError::InvalidDotaSetting { name, reason };
        match lookup("DOTA_HOST_ENABLED").as_deref().unwrap_or("false") {
            "false" | "0" | "" => return Ok(None),
            "true" | "1" => {}
            _ => return Err(invalid("DOTA_HOST_ENABLED", "expected true or false")),
        }
        let username = lookup("DOTA_STEAM_USERNAME")
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid("DOTA_STEAM_USERNAME", "required when hosting is enabled"))?;
        let guild_ids = lookup("DOTA_HOST_GUILD_IDS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().parse::<i64>())
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .filter(|ids| !ids.is_empty() && ids.iter().all(|id| *id > 0))
            .ok_or_else(|| {
                invalid(
                    "DOTA_HOST_GUILD_IDS",
                    "requires positive comma-separated guild IDs",
                )
            })?;
        let number = |lookup: &mut dyn FnMut(&str) -> Option<String>,
                      name,
                      default|
         -> Result<u64, ConfigError> {
            lookup(name).map_or(Ok(default), |s| {
                s.parse()
                    .map_err(|_| invalid(name, "expected a non-negative integer"))
            })
        };
        let account_id = number(&mut lookup, "DOTA_BOT_ACCOUNT_ID", 0)?;
        let account_id = u32::try_from(account_id)
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                invalid(
                    "DOTA_BOT_ACCOUNT_ID",
                    "requires the bot's positive 32-bit Dota account ID",
                )
            })?;
        let start_after = number(&mut lookup, "DOTA_HOST_START_AFTER", 0)?;
        let start_after = i64::try_from(start_after)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                invalid(
                    "DOTA_HOST_START_AFTER",
                    "requires the initial activation Unix timestamp; keep it across restarts",
                )
            })?;
        let server_region = number(
            &mut lookup,
            "DOTA_SERVER_REGION",
            u64::from(US_SOUTH_CENTRAL_REGION),
        )?;
        let server_region = u32::try_from(server_region)
            .ok()
            .filter(|id| *id > 0 && *id <= 100)
            .ok_or_else(|| invalid("DOTA_SERVER_REGION", "requires a Dota server-region ID"))?;
        let game_mode = number(&mut lookup, "DOTA_GAME_MODE", 2)?;
        let game_mode = u32::try_from(game_mode)
            .ok()
            .filter(|id| matches!(*id, 1 | 2 | 22))
            .ok_or_else(|| {
                invalid(
                    "DOTA_GAME_MODE",
                    "supported modes: 1 All Pick, 2 Captains Mode, 22 Ranked All Pick",
                )
            })?;
        let tv_delay = number(&mut lookup, "DOTA_TV_DELAY", 3)?;
        let tv_delay = u32::try_from(tv_delay)
            .ok()
            .filter(|id| *id <= 4)
            .ok_or_else(|| invalid("DOTA_TV_DELAY", "expected Dota delay enum 0, 1, 2, 3, or 4"))?;
        let lobby_timeout_seconds = number(&mut lookup, "DOTA_LOBBY_TIMEOUT_SECONDS", 1800)?;
        if !(60..=86400).contains(&lobby_timeout_seconds) {
            return Err(invalid(
                "DOTA_LOBBY_TIMEOUT_SECONDS",
                "expected 60 through 86400 seconds",
            ));
        }
        let live_bind = lookup("DOTA_LIVE_BIND")
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<SocketAddr>()
                    .map_err(|_| invalid("DOTA_LIVE_BIND", "expected an IP address and port"))
            })
            .transpose()?;
        let live_token = lookup("DOTA_LIVE_TOKEN").filter(|s| !s.is_empty());
        let gsi_token = lookup("DOTA_GSI_TOKEN").filter(|s| !s.is_empty());
        if live_bind.is_some() && live_token.as_ref().is_none_or(|s| s.len() < 32) {
            return Err(invalid(
                "DOTA_LIVE_TOKEN",
                "requires at least 32 characters when live HTTP is enabled",
            ));
        }
        if gsi_token.as_ref().is_some_and(|s| s.len() < 32) {
            return Err(invalid("DOTA_GSI_TOKEN", "requires at least 32 characters"));
        }
        let replay_max_bytes = number(&mut lookup, "DOTA_REPLAY_MAX_BYTES", 512 * 1024 * 1024)?;
        if !(1024..=2 * 1024 * 1024 * 1024).contains(&replay_max_bytes) {
            return Err(invalid(
                "DOTA_REPLAY_MAX_BYTES",
                "expected 1 KiB through 2 GiB",
            ));
        }
        Ok(Some(Self {
            guild_ids,
            username: Secret::new(username),
            account_id,
            session_path: lookup("DOTA_STEAM_SESSION_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "data/steam/session.json".into()),
            start_after,
            server_region,
            game_mode,
            tv_delay,
            lobby_timeout_seconds,
            web_api_key: lookup("DOTA_STEAM_WEB_API_KEY")
                .filter(|s| !s.is_empty())
                .map(Secret::new),
            live_bind,
            live_token: live_token.map(Secret::new),
            gsi_token: gsi_token.map(Secret::new),
            replay_directory: lookup("DOTA_REPLAY_DIRECTORY")
                .map(PathBuf::from)
                .unwrap_or_else(|| "data/replays".into()),
            replay_max_bytes,
            replay_retention_days: number(&mut lookup, "DOTA_REPLAY_RETENTION_DAYS", 30)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_and_activation_requires_explicit_scope() {
        assert!(DotaHostConfig::from_lookup(|_| None).unwrap().is_none());
        assert!(
            DotaHostConfig::from_lookup(|key| (key == "DOTA_HOST_ENABLED").then(|| "true".into()))
                .is_err()
        );
        let config = DotaHostConfig::from_lookup(|key| {
            match key {
                "DOTA_HOST_ENABLED" => Some("true"),
                "DOTA_HOST_GUILD_IDS" => Some("123"),
                "DOTA_STEAM_USERNAME" => Some("private-login"),
                "DOTA_BOT_ACCOUNT_ID" => Some("345"),
                "DOTA_HOST_START_AFTER" => Some("1700000000"),
                _ => None,
            }
            .map(str::to_owned)
        })
        .unwrap()
        .unwrap();
        assert!(!format!("{config:?}").contains("private-login"));
        assert_eq!(config.game_mode, 2);
        assert_eq!(config.server_region, 31);
        assert_eq!(config.tv_delay, 3);
    }

    #[test]
    fn tv_delay_accepts_current_wire_values_and_rejects_unknown_values() {
        for value in ["0", "1", "2", "3", "4", "5", "-1"] {
            let parsed = DotaHostConfig::from_lookup(|key| {
                match key {
                    "DOTA_HOST_ENABLED" => Some("true"),
                    "DOTA_HOST_GUILD_IDS" => Some("123"),
                    "DOTA_STEAM_USERNAME" => Some("bot"),
                    "DOTA_BOT_ACCOUNT_ID" => Some("345"),
                    "DOTA_HOST_START_AFTER" => Some("1700000000"),
                    "DOTA_TV_DELAY" => Some(value),
                    _ => None,
                }
                .map(str::to_owned)
            });
            if let Ok(expected @ 0..=4) = value.parse::<u32>() {
                assert_eq!(parsed.unwrap().unwrap().tv_delay, expected);
            } else {
                assert!(parsed.is_err(), "unknown delay {value} was accepted");
            }
        }
    }
}
