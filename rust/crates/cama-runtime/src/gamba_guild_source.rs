//! Live Discord destinations for guild-wide gamba announcements.
//!
//! Python discovers these channels from the connected guild cache by taking
//! the first text channel whose lower-cased name contains `gamba`. Workers use
//! this narrow snapshot instead of retaining Serenity guild/channel objects.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GambaDestination {
    pub guild_id: i64,
    pub channel_id: i64,
}

pub trait GambaGuildSource: Send + Sync {
    fn live_gamba_destinations(&self) -> Result<Vec<GambaDestination>, String>;
}
