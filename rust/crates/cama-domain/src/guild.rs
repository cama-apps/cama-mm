//! Guild identifier normalization shared by persistence and Discord-facing code.

/// Normalize a missing guild identifier to the sentinel used by the Python DB contract.
#[must_use]
pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_guild_id;

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
