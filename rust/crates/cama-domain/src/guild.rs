//! Guild and Discord identifiers shared by persistence and Discord-facing code.

use std::fmt;

/// Normalize a missing guild identifier to the sentinel used by the Python DB contract.
#[must_use]
pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

/// A Discord snowflake that does not fit SQLite's signed `INTEGER`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscordIdOutOfRange(pub u64);

impl fmt::Display for DiscordIdOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Discord ID {} exceeds SQLite INTEGER", self.0)
    }
}

impl std::error::Error for DiscordIdOutOfRange {}

/// Convert a Discord snowflake into the signed form SQLite stores, failing
/// closed instead of wrapping.
pub fn signed_discord_id(value: u64) -> Result<i64, DiscordIdOutOfRange> {
    i64::try_from(value).map_err(|_| DiscordIdOutOfRange(value))
}

/// A persisted guild identifier. Guild `0` is the direct-message/global
/// scope, matching [`normalize_guild_id`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuildId(pub i64);

impl GuildId {
    /// The direct-message/global scope persisted as guild `0`.
    pub const DIRECT_MESSAGE: Self = Self(0);

    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }

    /// Normalize an optional guild to the direct-message scope.
    #[must_use]
    pub const fn normalize(value: Option<Self>) -> Self {
        match value {
            Some(guild_id) => guild_id,
            None => Self::DIRECT_MESSAGE,
        }
    }

    /// Normalize an optional raw guild ID to the direct-message scope.
    #[must_use]
    pub const fn from_optional(value: Option<i64>) -> Self {
        Self(normalize_guild_id(value))
    }
}

impl TryFrom<u64> for GuildId {
    type Error = DiscordIdOutOfRange;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        signed_discord_id(value).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::{DiscordIdOutOfRange, GuildId, normalize_guild_id, signed_discord_id};

    #[test]
    fn test_signed_discord_id_fails_closed_above_i64_max() {
        assert_eq!(signed_discord_id(i64::MAX as u64), Ok(i64::MAX));
        let too_large = i64::MAX as u64 + 1;
        assert_eq!(
            signed_discord_id(too_large),
            Err(DiscordIdOutOfRange(too_large))
        );
        assert!(GuildId::try_from(u64::MAX).is_err());
        assert_eq!(GuildId::try_from(42_u64), Ok(GuildId(42)));
    }

    #[test]
    fn test_guild_id_normalizes_missing_guilds_to_direct_message_scope() {
        assert_eq!(GuildId::normalize(None), GuildId::DIRECT_MESSAGE);
        assert_eq!(GuildId::normalize(Some(GuildId(7))), GuildId(7));
        assert_eq!(GuildId::from_optional(None), GuildId(0));
        assert_eq!(GuildId::from_optional(Some(9)).value(), 9);
        assert_eq!(GuildId::default(), GuildId::DIRECT_MESSAGE);
    }

    #[test]
    fn test_returns_guild_id_unchanged() {
        assert_eq!(normalize_guild_id(Some(123_456_789)), 123_456_789);
        assert_eq!(normalize_guild_id(Some(1)), 1);
    }

    #[test]
    fn test_none_returns_zero() {
        assert_eq!(normalize_guild_id(None), 0);
    }

    #[test]
    fn test_guild_none_returns_zero() {
        assert_eq!(normalize_guild_id(None), 0);
    }

    #[test]
    fn test_zero_returns_zero() {
        assert_eq!(normalize_guild_id(Some(0)), 0);
    }

    #[test]
    fn test_guild_zero_returns_zero() {
        assert_eq!(normalize_guild_id(Some(0)), 0);
    }

    #[test]
    fn test_positive_id_unchanged() {
        assert_eq!(normalize_guild_id(Some(123_456_789)), 123_456_789);
    }

    #[test]
    fn test_large_discord_id() {
        assert_eq!(
            normalize_guild_id(Some(1_234_567_890_123_456_789)),
            1_234_567_890_123_456_789
        );
    }
}
