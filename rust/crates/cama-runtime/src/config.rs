use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

const DEFAULT_DB_PATH: &str = "cama_shuffle.db";
const DEFAULT_RECONNECT_INITIAL_SECONDS: u64 = 5;
const DEFAULT_RECONNECT_MAX_SECONDS: u64 = 300;

/// Discord credential with deliberately redacted formatting.
#[derive(Clone, Eq, PartialEq)]
pub struct DiscordToken(String);

impl DiscordToken {
    pub fn parse(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::MissingToken);
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(ConfigError::InvalidToken);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Access is crate-private so credentials cannot accidentally escape
    /// through application adapters.
    #[doc(hidden)]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DiscordToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DiscordToken([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub token: DiscordToken,
    pub db_path: PathBuf,
    pub reconnect_initial: Duration,
    pub reconnect_max: Duration,
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let token = DiscordToken::parse(lookup("DISCORD_BOT_TOKEN").unwrap_or_default())?;
        // Python's `os.getenv("DB_PATH", default)` only applies the default
        // when the variable is absent; an explicitly empty path is retained.
        let db_path =
            lookup("DB_PATH").map_or_else(|| PathBuf::from(DEFAULT_DB_PATH), PathBuf::from);
        let reconnect_initial = parse_seconds(
            &mut lookup,
            "CAMA_GATEWAY_RECONNECT_INITIAL_SECONDS",
            DEFAULT_RECONNECT_INITIAL_SECONDS,
        )?;
        let reconnect_max = parse_seconds(
            &mut lookup,
            "CAMA_GATEWAY_RECONNECT_MAX_SECONDS",
            DEFAULT_RECONNECT_MAX_SECONDS,
        )?;
        if reconnect_initial > reconnect_max {
            return Err(ConfigError::ReconnectRange {
                initial: reconnect_initial,
                maximum: reconnect_max,
            });
        }

        Ok(Self {
            token,
            db_path,
            reconnect_initial: Duration::from_secs(reconnect_initial),
            reconnect_max: Duration::from_secs(reconnect_max),
        })
    }
}

fn parse_seconds(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
    default: u64,
) -> Result<u64, ConfigError> {
    match lookup(name) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| ConfigError::InvalidInteger { name, value }),
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConfigError {
    #[error("DISCORD_BOT_TOKEN is required")]
    MissingToken,
    #[error("DISCORD_BOT_TOKEN contains whitespace and is not a valid bot token")]
    InvalidToken,
    #[error("{name} must be a non-negative integer, got {value:?}")]
    InvalidInteger { name: &'static str, value: String },
    #[error("gateway reconnect initial delay ({initial}s) exceeds maximum ({maximum}s)")]
    ReconnectRange { initial: u64, maximum: u64 },
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn config(values: &[(&str, &str)]) -> Result<RuntimeConfig, ConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        RuntimeConfig::from_lookup(|key| values.get(key).cloned())
    }

    #[test]
    fn token_is_required_and_never_debugged() {
        assert_eq!(config(&[]), Err(ConfigError::MissingToken));
        let parsed = config(&[("DISCORD_BOT_TOKEN", "secret-token")]).expect("valid config");
        assert_eq!(format!("{:?}", parsed.token), "DiscordToken([REDACTED])");
        assert!(!format!("{parsed:?}").contains("secret-token"));
    }

    #[test]
    fn defaults_match_the_python_runtime_and_supervisor() {
        let parsed = config(&[("DISCORD_BOT_TOKEN", "token")]).expect("valid config");
        assert_eq!(parsed.db_path, PathBuf::from("cama_shuffle.db"));
        assert_eq!(parsed.reconnect_initial, Duration::from_secs(5));
        assert_eq!(parsed.reconnect_max, Duration::from_secs(300));
    }

    #[test]
    fn reconnect_range_is_validated() {
        let error = config(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("CAMA_GATEWAY_RECONNECT_INITIAL_SECONDS", "10"),
            ("CAMA_GATEWAY_RECONNECT_MAX_SECONDS", "5"),
        ])
        .expect_err("invalid range");
        assert!(matches!(error, ConfigError::ReconnectRange { .. }));
    }

    #[test]
    fn explicitly_empty_database_path_matches_python_getenv() {
        let parsed = config(&[("DISCORD_BOT_TOKEN", "token"), ("DB_PATH", "")])
            .expect("configuration parses before database admission");
        assert_eq!(parsed.db_path, PathBuf::from(""));
    }
}
