//! Shared lobby identity types: Discord ID newtypes, the guild-scoped lobby
//! key, and the stored lobby message IDs.

use std::cmp::Ordering;

use crate::embeds::LobbyKind;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuildId(pub i64);

impl GuildId {
    pub const DEFAULT: Self = Self(0);

    #[must_use]
    pub const fn normalize(value: Option<Self>) -> Self {
        match value {
            Some(value) => value,
            None => Self::DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChannelId(pub i64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(pub i64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(pub i64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LobbyScope {
    pub guild_id: GuildId,
    pub kind: LobbyKind,
}

impl LobbyScope {
    #[must_use]
    pub const fn new(guild_id: GuildId, kind: LobbyKind) -> Self {
        Self { guild_id, kind }
    }

    #[must_use]
    pub const fn lobby_id(self) -> i64 {
        match self.kind {
            LobbyKind::Open => 1,
            LobbyKind::LowSkill => 2,
        }
    }

    const fn kind_rank(self) -> u8 {
        match self.kind {
            LobbyKind::Open => 0,
            LobbyKind::LowSkill => 1,
        }
    }
}

impl Ord for LobbyScope {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.guild_id, self.kind_rank()).cmp(&(other.guild_id, other.kind_rank()))
    }
}

impl PartialOrd for LobbyScope {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LobbyMessageIds {
    pub message_id: Option<MessageId>,
    pub channel_id: Option<ChannelId>,
    pub thread_id: Option<ChannelId>,
    pub embed_message_id: Option<MessageId>,
    pub origin_channel_id: Option<ChannelId>,
}
