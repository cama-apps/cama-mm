//! Dedicated-lobby routing, persistence, and publication contracts.
//!
//! The Python application remains the only production Discord/SQLite owner
//! during the side-by-side migration.  This module therefore keeps the
//! orchestration executable behind narrow ports: a `LobbyStateStore` can be
//! backed by the existing `lobby_state` table, while channel/publication ports
//! can be backed by the Discord runtime without coupling the policy to an SDK.

use std::cmp::Ordering;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use crate::embeds::LobbyKind;
use crate::interaction_safety::{ClosedLobbyEmbed, TransportError};

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PersistedIdPatch<T> {
    #[default]
    Preserve,
    Set(T),
    Clear,
}

impl<T: Copy> PersistedIdPatch<T> {
    fn apply(self, current: &mut Option<T>) {
        match self {
            Self::Preserve => {}
            Self::Set(value) => *current = Some(value),
            Self::Clear => *current = None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LobbyMessageUpdate {
    pub message_id: Option<MessageId>,
    pub channel_id: Option<ChannelId>,
    pub thread_id: PersistedIdPatch<ChannelId>,
    pub embed_message_id: PersistedIdPatch<MessageId>,
    pub origin_channel_id: PersistedIdPatch<ChannelId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyRecord {
    pub scope: LobbyScope,
    pub players: BTreeSet<UserId>,
    pub status: String,
    pub created_by: UserId,
    pub created_at: String,
    pub message_ids: LobbyMessageIds,
}

impl LobbyRecord {
    #[must_use]
    pub fn open(scope: LobbyScope, creator: UserId) -> Self {
        Self {
            scope,
            players: BTreeSet::from([creator]),
            status: "open".to_owned(),
            created_by: creator,
            created_at: "1970-01-01T00:00:00+00:00".to_owned(),
            message_ids: LobbyMessageIds::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LobbyStoreError {
    #[error("lobby state store failed: {0}")]
    Backend(String),
    #[error("no active lobby for guild {0:?}")]
    MissingLobby(LobbyScope),
}

pub trait LobbyStateStore {
    fn load_all(&self) -> Result<Vec<LobbyRecord>, LobbyStoreError>;
    fn save(&mut self, record: &LobbyRecord) -> Result<(), LobbyStoreError>;
    fn delete(&mut self, scope: LobbyScope) -> Result<(), LobbyStoreError>;
}

/// Durable lobby state with write-before-cache semantics.  A failed save or
/// delete leaves the in-memory snapshot unchanged, preventing a stale row from
/// reappearing as a ghost lobby after restart.
pub struct LobbyManager<S> {
    store: S,
    records: BTreeMap<LobbyScope, LobbyRecord>,
}

impl<S: LobbyStateStore> LobbyManager<S> {
    pub fn new(store: S) -> Result<Self, LobbyStoreError> {
        let records = store
            .load_all()?
            .into_iter()
            .map(|record| (record.scope, record))
            .collect();
        Ok(Self { store, records })
    }

    pub fn get_or_create_lobby(
        &mut self,
        scope: LobbyScope,
        creator: UserId,
    ) -> Result<&LobbyRecord, LobbyStoreError> {
        match self.records.entry(scope) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let record = LobbyRecord::open(scope, creator);
                self.store.save(&record)?;
                Ok(entry.insert(record))
            }
        }
    }

    pub fn set_lobby_message(
        &mut self,
        scope: LobbyScope,
        update: LobbyMessageUpdate,
    ) -> Result<(), LobbyStoreError> {
        let Some(current) = self.records.get(&scope) else {
            return Err(LobbyStoreError::MissingLobby(scope));
        };
        let mut replacement = current.clone();
        replacement.message_ids.message_id = update.message_id;
        replacement.message_ids.channel_id = update.channel_id;
        update
            .thread_id
            .apply(&mut replacement.message_ids.thread_id);
        update
            .embed_message_id
            .apply(&mut replacement.message_ids.embed_message_id);
        update
            .origin_channel_id
            .apply(&mut replacement.message_ids.origin_channel_id);
        self.store.save(&replacement)?;
        self.records.insert(scope, replacement);
        Ok(())
    }

    #[must_use]
    pub fn get_lobby(&self, scope: LobbyScope) -> Option<&LobbyRecord> {
        self.records.get(&scope)
    }

    #[must_use]
    pub fn message_ids(&self, scope: LobbyScope) -> Option<&LobbyMessageIds> {
        self.records.get(&scope).map(|record| &record.message_ids)
    }

    #[must_use]
    pub fn origin_channel_id(&self, scope: LobbyScope) -> Option<ChannelId> {
        self.message_ids(scope)?.origin_channel_id
    }

    pub fn reset_lobby(&mut self, scope: LobbyScope) -> Result<(), LobbyStoreError> {
        self.store.delete(scope)?;
        self.records.remove(&scope);
        Ok(())
    }

    #[must_use]
    pub fn into_store(self) -> S {
        self.store
    }
}

pub struct LobbyService<S> {
    manager: LobbyManager<S>,
}

impl<S: LobbyStateStore> LobbyService<S> {
    pub const fn new(manager: LobbyManager<S>) -> Self {
        Self { manager }
    }

    pub fn get_or_create_lobby(
        &mut self,
        scope: LobbyScope,
        creator: UserId,
    ) -> Result<&LobbyRecord, LobbyStoreError> {
        self.manager.get_or_create_lobby(scope, creator)
    }

    pub fn set_lobby_message_id(
        &mut self,
        scope: LobbyScope,
        update: LobbyMessageUpdate,
    ) -> Result<(), LobbyStoreError> {
        self.manager.set_lobby_message(scope, update)
    }

    #[must_use]
    pub fn get_lobby(&self, scope: LobbyScope) -> Option<&LobbyRecord> {
        self.manager.get_lobby(scope)
    }

    #[must_use]
    pub fn get_lobby_message_ids(&self, scope: LobbyScope) -> Option<&LobbyMessageIds> {
        self.manager.message_ids(scope)
    }

    #[must_use]
    pub fn get_origin_channel_id(&self, scope: LobbyScope) -> Option<ChannelId> {
        self.manager.origin_channel_id(scope)
    }

    pub fn reset_lobby(&mut self, scope: LobbyScope) -> Result<(), LobbyStoreError> {
        self.manager.reset_lobby(scope)
    }

    #[must_use]
    pub fn into_manager(self) -> LobbyManager<S> {
        self.manager
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscordChannelKind {
    Text,
    Thread,
    OtherMessageable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelPermissions {
    pub send_messages: bool,
    pub create_public_threads: bool,
}

impl ChannelPermissions {
    pub const REQUIRED_FOR_LOBBY: Self = Self {
        send_messages: true,
        create_public_threads: true,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscordChannel {
    pub id: ChannelId,
    pub guild_id: Option<GuildId>,
    pub kind: DiscordChannelKind,
    pub permissions: ChannelPermissions,
}

impl DiscordChannel {
    #[must_use]
    pub const fn text(id: ChannelId, guild_id: GuildId) -> Self {
        Self {
            id,
            guild_id: Some(guild_id),
            kind: DiscordChannelKind::Text,
            permissions: ChannelPermissions::REQUIRED_FOR_LOBBY,
        }
    }
}

pub trait ChannelDirectory {
    fn cached_channel(&self, channel_id: ChannelId) -> Option<DiscordChannel>;
    fn fetch_channel(
        &mut self,
        channel_id: ChannelId,
    ) -> Result<Option<DiscordChannel>, TransportError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LobbyChannelConfig {
    pub regular: Option<ChannelId>,
    pub low_skill: Option<ChannelId>,
}

impl LobbyChannelConfig {
    #[must_use]
    pub fn from_env_values(regular: Option<&str>, low_skill: Option<&str>) -> Self {
        Self {
            regular: parse_configured_channel_id(regular),
            low_skill: parse_configured_channel_id(low_skill),
        }
    }

    #[must_use]
    pub const fn configured_for(self, kind: LobbyKind) -> Option<ChannelId> {
        match kind {
            LobbyKind::LowSkill => match self.low_skill {
                Some(channel_id) => Some(channel_id),
                None => self.regular,
            },
            LobbyKind::Open => self.regular,
        }
    }
}

#[must_use]
pub fn parse_configured_channel_id(raw: Option<&str>) -> Option<ChannelId> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<i64>().ok().map(ChannelId)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionChannelContext {
    pub channel: DiscordChannel,
    pub guild_id: Option<GuildId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelResolutionSource {
    Interaction,
    DedicatedCache,
    DedicatedFetch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelFallbackReason {
    NotConfigured,
    NotFound,
    TransportFailure,
    DifferentGuild,
    MissingSendPermission,
    MissingThreadPermission,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLobbyChannel {
    pub channel: DiscordChannel,
    pub source: ChannelResolutionSource,
    pub fallback_reason: Option<ChannelFallbackReason>,
}

fn channel_is_usable(
    channel: &DiscordChannel,
    expected_guild: Option<GuildId>,
) -> Result<(), ChannelFallbackReason> {
    if channel.kind != DiscordChannelKind::Text {
        return Ok(());
    }
    if expected_guild.is_some() && channel.guild_id != expected_guild {
        return Err(ChannelFallbackReason::DifferentGuild);
    }
    if !channel.permissions.send_messages {
        return Err(ChannelFallbackReason::MissingSendPermission);
    }
    if !channel.permissions.create_public_threads {
        return Err(ChannelFallbackReason::MissingThreadPermission);
    }
    Ok(())
}

#[must_use]
pub fn resolve_lobby_target_channel(
    directory: &mut dyn ChannelDirectory,
    context: &InteractionChannelContext,
    kind: LobbyKind,
    config: LobbyChannelConfig,
) -> ResolvedLobbyChannel {
    let Some(configured) = config.configured_for(kind).filter(|id| id.0 != 0) else {
        return ResolvedLobbyChannel {
            channel: context.channel.clone(),
            source: ChannelResolutionSource::Interaction,
            fallback_reason: Some(ChannelFallbackReason::NotConfigured),
        };
    };

    let (resolved, source, resolution_error) = match directory.cached_channel(configured) {
        Some(channel) => (Some(channel), ChannelResolutionSource::DedicatedCache, None),
        None => match directory.fetch_channel(configured) {
            Ok(channel) => (channel, ChannelResolutionSource::DedicatedFetch, None),
            Err(_) => (
                None,
                ChannelResolutionSource::Interaction,
                Some(ChannelFallbackReason::TransportFailure),
            ),
        },
    };

    let Some(channel) = resolved else {
        return ResolvedLobbyChannel {
            channel: context.channel.clone(),
            source: ChannelResolutionSource::Interaction,
            fallback_reason: resolution_error.or(Some(ChannelFallbackReason::NotFound)),
        };
    };
    match channel_is_usable(&channel, context.guild_id) {
        Ok(()) => ResolvedLobbyChannel {
            channel,
            source,
            fallback_reason: None,
        },
        Err(reason) => ResolvedLobbyChannel {
            channel: context.channel.clone(),
            source: ChannelResolutionSource::Interaction,
            fallback_reason: Some(reason),
        },
    }
}

pub trait LobbyPublicationPort: ChannelDirectory {
    fn publish_lobby_embed(
        &mut self,
        channel_id: ChannelId,
        kind: LobbyKind,
    ) -> Result<MessageId, TransportError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyPublication {
    pub message_id: MessageId,
    pub lobby_channel_id: ChannelId,
    pub origin_channel_id: ChannelId,
    pub resolution: ChannelResolutionSource,
}

pub fn publish_new_lobby<S: LobbyStateStore, P: LobbyPublicationPort>(
    service: &mut LobbyService<S>,
    port: &mut P,
    scope: LobbyScope,
    creator: UserId,
    context: &InteractionChannelContext,
    config: LobbyChannelConfig,
) -> Result<LobbyPublication, String> {
    service
        .get_or_create_lobby(scope, creator)
        .map_err(|error| error.to_string())?;
    let target = resolve_lobby_target_channel(port, context, scope.kind, config);
    let message_id = port
        .publish_lobby_embed(target.channel.id, scope.kind)
        .map_err(|error| error.to_string())?;
    service
        .set_lobby_message_id(
            scope,
            LobbyMessageUpdate {
                message_id: Some(message_id),
                channel_id: Some(target.channel.id),
                embed_message_id: PersistedIdPatch::Set(message_id),
                origin_channel_id: PersistedIdPatch::Set(context.channel.id),
                ..LobbyMessageUpdate::default()
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(LobbyPublication {
        message_id,
        lobby_channel_id: target.channel.id,
        origin_channel_id: context.channel.id,
        resolution: target.source,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscriber {
    pub user_id: UserId,
    pub mention: String,
    pub is_bot: bool,
    pub glicko_rating: Option<i32>,
}

impl Subscriber {
    #[must_use]
    pub fn persistent(user_id: UserId, glicko_rating: Option<i32>) -> Self {
        Self {
            user_id,
            mention: format!("<@{}>", user_id.0),
            is_bot: false,
            glicko_rating,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RallyPayload {
    pub title: String,
    pub description: String,
    pub jump_url: Option<String>,
    pub content: Option<String>,
    pub allowed_mention_ids: Vec<UserId>,
    pub thread_content: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParallelSendResult {
    pub target: Result<(), TransportError>,
    pub thread: Option<Result<(), TransportError>>,
}

pub trait RallyPublicationPort: ChannelDirectory {
    fn reaction_subscribers(
        &mut self,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> Result<Vec<Subscriber>, TransportError>;

    fn persistent_subscribers(
        &mut self,
        scope: LobbyScope,
    ) -> Result<Vec<Subscriber>, TransportError>;

    /// The runtime adapter must start both sends before awaiting either one.
    fn publish_concurrently(
        &mut self,
        target_channel_id: ChannelId,
        thread_channel_id: Option<ChannelId>,
        payload: &RallyPayload,
    ) -> ParallelSendResult;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RallyCooldownKey {
    scope: LobbyScope,
    needed: usize,
}

#[derive(Debug, Default)]
pub struct RallyCooldowns {
    claims: Vec<(RallyCooldownKey, i64)>,
}

impl RallyCooldowns {
    fn try_claim(&mut self, key: RallyCooldownKey, now: i64, cooldown_seconds: i64) -> bool {
        if let Some((_, last_sent)) = self
            .claims
            .iter_mut()
            .find(|(existing, _)| *existing == key)
        {
            if now - *last_sent < cooldown_seconds {
                return false;
            }
            *last_sent = now;
            return true;
        }
        self.claims.push((key, now));
        true
    }

    fn release(&mut self, key: RallyCooldownKey) {
        self.claims.retain(|(existing, _)| *existing != key);
    }

    #[must_use]
    pub fn contains(&self, scope: LobbyScope, needed: usize) -> bool {
        self.claims
            .iter()
            .any(|(key, _)| key.scope == scope && key.needed == needed)
    }

    pub fn clear_scope(&mut self, scope: LobbyScope) {
        self.claims.retain(|(key, _)| key.scope != scope);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RallyRequest {
    pub scope: LobbyScope,
    pub reaction_channel: DiscordChannel,
    pub thread_channel_id: Option<ChannelId>,
    pub lobby_players: BTreeSet<UserId>,
    pub total_players: usize,
    pub ready_threshold: usize,
    pub message_ids: LobbyMessageIds,
    pub bot_user_id: Option<UserId>,
    pub now: i64,
    pub cooldown_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RallyReport {
    pub sent: bool,
    pub target_channel_id: ChannelId,
    pub used_origin: bool,
    pub allowed_mention_ids: Vec<UserId>,
    pub thread_attempted: bool,
    pub thread_failed: bool,
}

fn resolve_optional_channel(
    port: &mut dyn ChannelDirectory,
    channel_id: ChannelId,
    guild_id: GuildId,
) -> Option<DiscordChannel> {
    let channel = port
        .cached_channel(channel_id)
        .or_else(|| port.fetch_channel(channel_id).ok().flatten())?;
    (channel.guild_id.is_none() || channel.guild_id == Some(guild_id)).then_some(channel)
}

fn rally_subscribers(
    reaction: Vec<Subscriber>,
    mut persistent: Vec<Subscriber>,
    request: &RallyRequest,
) -> (Option<String>, Vec<UserId>) {
    persistent.sort_by_key(|subscriber| subscriber.user_id);
    let mut seen = request.lobby_players.clone();
    if let Some(bot_user_id) = request.bot_user_id {
        seen.insert(bot_user_id);
    }
    let mut selected_mentions = Vec::new();
    let mut selected_ids = Vec::new();
    let mut content_length = 0;
    for (subscriber, is_persistent) in reaction
        .into_iter()
        .map(|subscriber| (subscriber, false))
        .chain(persistent.into_iter().map(|subscriber| (subscriber, true)))
    {
        let low_skill_eligible = !is_persistent
            || request.scope.kind != LobbyKind::LowSkill
            || subscriber
                .glicko_rating
                .is_some_and(|rating| rating < 1_400);
        if subscriber.user_id.0 <= 0
            || subscriber.is_bot
            || !low_skill_eligible
            || !seen.insert(subscriber.user_id)
        {
            continue;
        }
        let mention = if subscriber.mention.is_empty() {
            format!("<@{}>", subscriber.user_id.0)
        } else {
            subscriber.mention
        };
        let added_length = mention.len() + usize::from(!selected_mentions.is_empty());
        if selected_ids.len() >= 100 || content_length + added_length > 2_000 {
            break;
        }
        content_length += added_length;
        selected_ids.push(subscriber.user_id);
        selected_mentions.push(mention);
    }
    let content = (!selected_mentions.is_empty()).then(|| selected_mentions.join(" "));
    (content, selected_ids)
}

pub fn notify_lobby_rally(
    port: &mut dyn RallyPublicationPort,
    cooldowns: &mut RallyCooldowns,
    request: &RallyRequest,
) -> RallyReport {
    let needed = request
        .ready_threshold
        .saturating_sub(request.total_players);
    let fallback = || RallyReport {
        sent: false,
        target_channel_id: request.reaction_channel.id,
        used_origin: false,
        allowed_mention_ids: Vec::new(),
        thread_attempted: false,
        thread_failed: false,
    };
    if !(1..=2).contains(&needed) {
        return fallback();
    }
    let key = RallyCooldownKey {
        scope: request.scope,
        needed,
    };
    if !cooldowns.try_claim(key, request.now, request.cooldown_seconds) {
        return fallback();
    }

    let reaction_subscribers = match (
        request.message_ids.channel_id,
        request.message_ids.message_id,
    ) {
        (Some(channel_id), Some(message_id)) => {
            let channel_available = channel_id == request.reaction_channel.id
                || resolve_optional_channel(port, channel_id, request.scope.guild_id).is_some();
            if channel_available {
                port.reaction_subscribers(channel_id, message_id)
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };
    let persistent_subscribers = port
        .persistent_subscribers(request.scope)
        .unwrap_or_default();
    let (content, allowed_mention_ids) =
        rally_subscribers(reaction_subscribers, persistent_subscribers, request);

    let origin = request
        .message_ids
        .origin_channel_id
        .filter(|channel_id| *channel_id != request.reaction_channel.id)
        .and_then(|channel_id| resolve_optional_channel(port, channel_id, request.scope.guild_id));
    let target = origin.as_ref().unwrap_or(&request.reaction_channel).clone();
    let jump_url = match (
        request.message_ids.channel_id,
        request.message_ids.message_id,
    ) {
        (Some(channel_id), Some(message_id)) => Some(format!(
            "https://discord.com/channels/{}/{}/{}",
            request.scope.guild_id.0, channel_id.0, message_id.0
        )),
        _ => None,
    };
    let payload = RallyPayload {
        title: format!("📢 {} Almost Ready!", request.scope.kind.label()),
        description: format!(
            "{} has **{}** players — just **+{}** more needed!",
            request.scope.kind.display_name(),
            request.total_players,
            needed
        ),
        jump_url,
        content,
        allowed_mention_ids: allowed_mention_ids.clone(),
        thread_content: request.thread_channel_id.map(|_| {
            format!(
                "📢 {}: **+{}** more player{} needed!",
                request.scope.kind.label(),
                needed,
                if needed > 1 { "s" } else { "" }
            )
        }),
    };
    let result = port.publish_concurrently(target.id, request.thread_channel_id, &payload);
    if result.target.is_err() {
        cooldowns.release(key);
        return RallyReport {
            target_channel_id: target.id,
            used_origin: origin.is_some(),
            thread_attempted: request.thread_channel_id.is_some(),
            thread_failed: result.thread.is_some_and(|send| send.is_err()),
            ..fallback()
        };
    }
    RallyReport {
        sent: true,
        target_channel_id: target.id,
        used_origin: origin.is_some(),
        allowed_mention_ids,
        thread_attempted: request.thread_channel_id.is_some(),
        thread_failed: result.thread.is_some_and(|send| send.is_err()),
    }
}

pub trait ShufflePublicationPort: ChannelDirectory {
    fn publish_shuffle(&mut self, channel_id: ChannelId) -> Result<MessageId, TransportError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShufflePublicationReport {
    pub lobby_message_id: Option<MessageId>,
    pub command_message_id: Option<MessageId>,
    pub attempted_channels: Vec<ChannelId>,
}

pub fn publish_shuffle_result(
    port: &mut dyn ShufflePublicationPort,
    scope: LobbyScope,
    lobby_channel_id: Option<ChannelId>,
    command_channel: Option<&DiscordChannel>,
) -> ShufflePublicationReport {
    let mut report = ShufflePublicationReport::default();
    if let Some(channel_id) = lobby_channel_id {
        let usable = command_channel.is_some_and(|channel| channel.id == channel_id)
            || resolve_optional_channel(port, channel_id, scope.guild_id).is_some();
        if usable {
            report.attempted_channels.push(channel_id);
            report.lobby_message_id = port.publish_shuffle(channel_id).ok();
        }
    }
    if let Some(command_channel) = command_channel
        && Some(command_channel.id) != lobby_channel_id
    {
        report.attempted_channels.push(command_channel.id);
        report.command_message_id = port.publish_shuffle(command_channel.id).ok();
    }
    report
}

pub const AMBIGUOUS_LOBBY_MESSAGE: &str =
    "❌ You're in multiple lobbies. Please specify which lobby.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LobbySelectionError {
    NoLobby,
    Ambiguous { message: &'static str },
}

pub fn select_lobby_kind(
    memberships: &[LobbyKind],
    explicit: Option<LobbyKind>,
) -> Result<LobbyKind, LobbySelectionError> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    match memberships {
        [kind] => Ok(*kind),
        [] => Err(LobbySelectionError::NoLobby),
        _ => Err(LobbySelectionError::Ambiguous {
            message: AMBIGUOUS_LOBBY_MESSAGE,
        }),
    }
}

pub trait ResetLobbyPort: ChannelDirectory {
    fn edit_partial_message(
        &mut self,
        channel_id: ChannelId,
        message_id: MessageId,
        embed: ClosedLobbyEmbed,
        clear_view: bool,
    ) -> Result<(), TransportError>;
    fn archive_thread(&mut self, thread_id: ChannelId, reason: &str) -> Result<(), TransportError>;
    fn unpin_message(
        &mut self,
        channel_id: ChannelId,
        message_id: Option<MessageId>,
    ) -> Result<(), TransportError>;
    fn respond_ephemerally(&mut self, content: &str) -> Result<(), TransportError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResetLobbyOutcome {
    Reset {
        scope: LobbyScope,
        unpin_channel_id: ChannelId,
    },
    Refused(LobbySelectionError),
    StoreFailure(LobbyStoreError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetLobbyRequest {
    pub guild_id: GuildId,
    pub memberships: Vec<LobbyKind>,
    pub explicit_kind: Option<LobbyKind>,
    pub interaction_channel: DiscordChannel,
}

pub fn reset_lobby_command<S: LobbyStateStore>(
    service: &mut LobbyService<S>,
    port: &mut dyn ResetLobbyPort,
    cooldowns: &mut RallyCooldowns,
    request: &ResetLobbyRequest,
) -> ResetLobbyOutcome {
    let kind = match select_lobby_kind(&request.memberships, request.explicit_kind) {
        Ok(kind) => kind,
        Err(error) => {
            if let LobbySelectionError::Ambiguous { message } = error {
                let _ = port.respond_ephemerally(message);
            }
            return ResetLobbyOutcome::Refused(error);
        }
    };
    let scope = LobbyScope::new(request.guild_id, kind);
    let Some(ids) = service.get_lobby_message_ids(scope).cloned() else {
        return ResetLobbyOutcome::StoreFailure(LobbyStoreError::MissingLobby(scope));
    };

    if let (Some(channel_id), Some(message_id)) = (ids.channel_id, ids.message_id) {
        let _ = port.edit_partial_message(
            channel_id,
            message_id,
            ClosedLobbyEmbed {
                title: format!("🚫 {} — Lobby Reset", kind.label()),
                description: format!("{} has been closed.", kind.display_name()),
            },
            true,
        );
    }
    if let Some(thread_id) = ids.thread_id {
        let _ = port.archive_thread(thread_id, "Lobby Reset");
    }

    let unpin_channel = ids
        .channel_id
        .and_then(|channel_id| resolve_optional_channel(port, channel_id, request.guild_id))
        .unwrap_or_else(|| request.interaction_channel.clone());
    let _ = port.unpin_message(unpin_channel.id, ids.message_id);
    if let Err(error) = service.reset_lobby(scope) {
        return ResetLobbyOutcome::StoreFailure(error);
    }
    cooldowns.clear_scope(scope);
    ResetLobbyOutcome::Reset {
        scope,
        unpin_channel_id: unpin_channel.id,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaColumn {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
}

pub trait LobbySchemaPort {
    fn lobby_state_columns(&mut self) -> Result<Vec<SchemaColumn>, LobbyStoreError>;
    fn add_nullable_integer_column(
        &mut self,
        table: &str,
        column: &str,
    ) -> Result<(), LobbyStoreError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OriginChannelMigration {
    AlreadyPresent,
    Added,
}

pub fn migrate_origin_channel_id(
    port: &mut dyn LobbySchemaPort,
) -> Result<OriginChannelMigration, LobbyStoreError> {
    if port
        .lobby_state_columns()?
        .iter()
        .any(|column| column.name == "origin_channel_id")
    {
        return Ok(OriginChannelMigration::AlreadyPresent);
    }
    port.add_nullable_integer_column("lobby_state", "origin_channel_id")?;
    Ok(OriginChannelMigration::Added)
}

#[cfg(test)]
#[path = "dedicated_lobby_channel/tests.rs"]
mod tests;
