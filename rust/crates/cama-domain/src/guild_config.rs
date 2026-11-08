//! Per-guild configuration storage contract.

/// One row from Python's `guild_config` table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildConfig {
    pub guild_id: i64,
    pub league_id: Option<i64>,
    pub auto_enrich_matches: bool,
    pub ai_features_enabled: Option<bool>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Storage operations for per-guild configuration.
pub trait GuildConfigStore {
    type Error;

    fn get_config(&self, guild_id: i64) -> Result<Option<GuildConfig>, Self::Error>;
    fn set_league_id(&self, guild_id: i64, league_id: i64) -> Result<(), Self::Error>;
    fn get_league_id(&self, guild_id: i64) -> Result<Option<i64>, Self::Error>;
    fn set_auto_enrich(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error>;
    fn get_auto_enrich(&self, guild_id: i64) -> Result<bool, Self::Error>;
    fn set_ai_enabled(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error>;
    fn get_ai_enabled(&self, guild_id: i64) -> Result<bool, Self::Error>;
}
