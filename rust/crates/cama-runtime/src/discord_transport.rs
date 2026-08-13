//! Discord HTTP/cache operations needed outside an interaction response.
//!
//! Command policy depends on this narrow boundary rather than Serenity. The
//! production adapter is attached to the live Serenity context before ready
//! recovery or interaction dispatch; tests use a deterministic recording port.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::registration::InteractionResponse;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscordAllowedMentions {
    /// Preserve Discord's normal parse policy for deliberately public copy.
    Default,
    None,
    Users(BTreeSet<u64>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordMessage {
    pub response: InteractionResponse,
    pub allowed_mentions: DiscordAllowedMentions,
    pub content_mode: DiscordMessageContentMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiscordMessageContentMode {
    #[default]
    Replace,
    Preserve,
    /// Replace components while preserving the existing content, embeds, and
    /// attachments. This is used by restart recovery that only needs to
    /// restore a persistent Discord view.
    PreserveBody,
}

impl DiscordMessage {
    #[must_use]
    pub fn default_mentions(response: InteractionResponse) -> Self {
        Self {
            response,
            allowed_mentions: DiscordAllowedMentions::Default,
            content_mode: DiscordMessageContentMode::Replace,
        }
    }

    #[must_use]
    pub fn silent(response: InteractionResponse) -> Self {
        Self {
            response,
            allowed_mentions: DiscordAllowedMentions::None,
            content_mode: DiscordMessageContentMode::Replace,
        }
    }

    #[must_use]
    pub fn mentioning(response: InteractionResponse, users: BTreeSet<u64>) -> Self {
        Self {
            response,
            allowed_mentions: DiscordAllowedMentions::Users(users),
            content_mode: DiscordMessageContentMode::Replace,
        }
    }

    /// Edit embeds/components without changing the message's existing text.
    /// Discord create/send calls ignore this flag; it only affects edits.
    #[must_use]
    pub const fn preserving_content(mut self) -> Self {
        self.content_mode = DiscordMessageContentMode::Preserve;
        self
    }

    /// Edit only the component rows, leaving the rendered message body intact.
    #[must_use]
    pub const fn preserving_body(mut self) -> Self {
        self.content_mode = DiscordMessageContentMode::PreserveBody;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordMessageReceipt {
    pub channel_id: u64,
    pub message_id: u64,
    pub jump_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordEmoji {
    pub name: String,
    pub id: Option<u64>,
}

impl DiscordEmoji {
    #[must_use]
    pub fn unicode(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordMessageSnapshot {
    pub receipt: DiscordMessageReceipt,
    pub reactions: Vec<DiscordEmoji>,
}

/// Identity needed to clean up only messages authored by this bot without
/// disturbing pins owned by guild members or sibling applications.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscordPinnedMessage {
    pub message_id: u64,
    pub author_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscordPresence {
    Online,
    Idle,
    DoNotDisturb,
    Offline,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordGuildMemberSnapshot {
    pub user_id: u64,
    pub display_name: String,
    pub presence: DiscordPresence,
    pub in_voice: bool,
    pub deafened: bool,
    pub activities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordUserSnapshot {
    pub user_id: u64,
    pub display_name: String,
    pub is_bot: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscordDirectMessageErrorKind {
    Forbidden,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordDirectMessageError {
    pub kind: DiscordDirectMessageErrorKind,
    pub message: String,
}

impl DiscordDirectMessageError {
    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let lower = message.to_ascii_lowercase();
        let kind = if lower.contains("forbidden")
            || lower.contains("cannot send messages to this user")
            || lower.contains("status code: 403")
        {
            DiscordDirectMessageErrorKind::Forbidden
        } else {
            DiscordDirectMessageErrorKind::Unavailable
        };
        Self { kind, message }
    }
}

#[async_trait]
pub trait DiscordTransport: Send + Sync {
    async fn fetch_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String>;

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String>;

    /// Send a message as an inline reply to an existing message in a thread.
    /// The default keeps older transports source-compatible while preserving
    /// delivery if they cannot express Discord's reply reference.
    async fn send_thread_reply(
        &self,
        thread_id: u64,
        parent_message_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let _ = parent_message_id;
        self.send_message(thread_id, message).await
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String>;

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String>;

    async fn create_public_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, String>;

    /// Create a private thread and add the requested users to it.
    ///
    /// The default is an explicit failure.  Private Mafia conversations must
    /// never silently degrade into public threads when an older transport has
    /// not implemented this operation yet.
    async fn create_private_thread(
        &self,
        _channel_id: u64,
        _name: &str,
        _member_ids: &[u64],
    ) -> Result<u64, String> {
        Err("Discord transport does not support private threads".to_owned())
    }

    /// Add users to an existing private thread, with the implementation's
    /// bounded REST policy.  The default is an explicit failure so callers do
    /// not mistake an unimplemented adapter for a synchronized graveyard.
    async fn add_private_thread_members(
        &self,
        _thread_id: u64,
        _member_ids: &[u64],
    ) -> Result<(), String> {
        Err("Discord transport does not support private-thread membership".to_owned())
    }

    /// Whether a guild channel is present and can receive a Mafia post.
    /// Implementations should include their cache/permission policy here so
    /// the command gate and the announcement worker select the same channel.
    async fn mafia_channel_available(
        &self,
        _guild_id: u64,
        _channel_id: u64,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// First sendable text channel whose name contains `gamba`, matching the
    /// Python fallback used when the configured Mafia channel is absent.
    async fn mafia_gamba_channel_id(&self, _guild_id: u64) -> Result<Option<u64>, String> {
        Ok(None)
    }

    /// Whether a channel is present in the guild's resolved channel cache.
    ///
    /// This deliberately answers only the same lookup question as Discord.py
    /// `Guild.get_channel`: callers decide their own permission policy and
    /// fall back when a configured ID is stale or unavailable.
    async fn guild_channel_exists(&self, _guild_id: u64, _channel_id: u64) -> Result<bool, String> {
        Ok(false)
    }

    async fn pin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String>;

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String>;

    /// Edit a thread's live lifecycle state without forcing archival.
    ///
    /// The legacy boundary only exposed `archive_thread`, so alternate/test
    /// transports remain source compatible. Production overrides this method
    /// to support the match shuffle's rename-and-lock operation while leaving
    /// the thread unarchived.
    async fn edit_thread(
        &self,
        thread_id: u64,
        name: &str,
        archived: bool,
        locked: bool,
    ) -> Result<(), String> {
        if archived {
            self.archive_thread(thread_id, name, locked).await
        } else {
            Err("Discord transport does not support editing an active thread".to_owned())
        }
    }

    async fn add_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String>;

    async fn remove_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
        user_id: u64,
    ) -> Result<(), String>;

    async fn clear_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String>;

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String>;

    /// List the messages currently pinned in a channel. Older transports that
    /// do not expose pin enumeration remain source-compatible and behave like
    /// a channel without a `pins` operation in the Python helper.
    async fn pinned_messages(&self, _channel_id: u64) -> Result<Vec<DiscordPinnedMessage>, String> {
        Ok(Vec::new())
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String>;

    /// Send a DM and retain the receipt so a preflight can be rewritten to its
    /// final durable outcome. Older transports remain source compatible.
    async fn send_direct_message_with_receipt(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, DiscordDirectMessageError> {
        self.send_direct_message(user_id, message)
            .await
            .map_err(DiscordDirectMessageError::from_message)?;
        Err(DiscordDirectMessageError {
            kind: DiscordDirectMessageErrorKind::Unavailable,
            message: "Discord transport did not return a DM receipt".to_owned(),
        })
    }

    async fn edit_direct_message(
        &self,
        _receipt: &DiscordMessageReceipt,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Err("Discord transport does not support DM edits".to_owned())
    }

    async fn reaction_users(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<Vec<DiscordUserSnapshot>, String> {
        Ok(Vec::new())
    }

    async fn bot_user_id(&self) -> Result<Option<u64>, String> {
        Ok(None)
    }

    /// Resolve a raw-event actor outside a guild member payload. Production
    /// uses Serenity's user cache before HTTP, matching Python's
    /// `get_user`/`fetch_user` fallback; existing test transports remain
    /// source-compatible.
    async fn user(&self, _user_id: u64) -> Result<Option<DiscordUserSnapshot>, String> {
        Ok(None)
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String>;

    /// Resolve a channel's thread parent when the interaction happened in a
    /// thread. Older transports return `None`, preserving source compatibility.
    async fn channel_parent_id(
        &self,
        _guild_id: u64,
        _channel_id: u64,
    ) -> Result<Option<u64>, String> {
        Ok(None)
    }

    /// Return the requested members whose Discord presence contains a Go Live
    /// streaming activity. Activity names alone cannot distinguish this from
    /// Playing/Listening, so production inspects Discord's typed activity and
    /// older transports deliberately return an empty set.
    async fn streaming_member_ids(
        &self,
        _guild_id: u64,
        _user_ids: &[u64],
    ) -> Result<BTreeSet<u64>, String> {
        Ok(BTreeSet::new())
    }
}
