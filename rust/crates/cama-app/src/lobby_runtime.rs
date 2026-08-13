//! Dual-lobby command policy for the Rust lift-and-shift runtime.
//!
//! The existing [`crate::lobby_service::LobbyService`] remains the single
//! membership, reservation, and ready-check policy.  This module adds the
//! command-level orchestration that Python's `commands/lobby.py` currently
//! performs around it: explicit lobby selection, cross-lobby match eviction,
//! leave-all behavior, and reset serialization with fail-soft Discord cleanup.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;

use crate::dedicated_lobby_channel::{
    ChannelId, GuildId, LobbyMessageIds, LobbyScope, MessageId, UserId,
};
use crate::draft::{DraftStateManager, LobbyKind as DraftLobbyKind};
use crate::embeds::LobbyKind;
use crate::lobby_service::{LobbyClock, LobbyPlayerPort, LobbyService, PendingMatchPort};

pub const AMBIGUOUS_LOBBY_MESSAGE: &str = concat!(
    "❓ You're queued in both 🍽️ All You Can Feed and ",
    "🧀 Whine & Cheese — add the `lobby` option to pick one."
);

pub const RESET_NO_LOBBY_MESSAGE: &str =
    "❌ Join a lobby, or pass the `lobby` option to pick which lobby to reset.";
pub const READYCHECK_NO_LOBBY_MESSAGE: &str = "❌ Join a lobby before running a ready check.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LobbySelectionError {
    NoLobby { message: String },
    Ambiguous,
}

pub fn resolve_lobby_kind(
    memberships: &[LobbyKind],
    explicit: Option<LobbyKind>,
    no_lobby_message: &str,
) -> Result<LobbyKind, LobbySelectionError> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    match memberships {
        [kind] => Ok(*kind),
        [] => Err(LobbySelectionError::NoLobby {
            message: no_lobby_message.to_owned(),
        }),
        _ => Err(LobbySelectionError::Ambiguous),
    }
}

#[must_use]
pub fn format_lobby_labels(kinds: &[LobbyKind]) -> String {
    match kinds {
        [] => String::new(),
        [kind] => kind.label().to_owned(),
        _ => {
            let (last, leading) = kinds.split_last().expect("non-empty lobby kinds");
            format!(
                "{} and {}",
                leading
                    .iter()
                    .map(|kind| kind.label())
                    .collect::<Vec<_>>()
                    .join(", "),
                last.label()
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllowedMentions {
    Default,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationStage {
    Display,
    Reaction,
    Thread,
    Close,
    Archive,
    Unpin,
    Response,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFailure {
    pub scope: LobbyScope,
    pub stage: PublicationStage,
    pub message: String,
}

/// Discord-facing effects.  Every operation is best-effort: committed lobby
/// state must never be rolled back because a message is missing or Discord is
/// unavailable.
#[async_trait]
pub trait LobbyRuntimeTransport: Send + Sync {
    async fn sync_lobby_display(&self, _scope: LobbyScope) -> Result<(), String> {
        Ok(())
    }

    async fn remove_lobby_reaction(
        &self,
        _scope: LobbyScope,
        _player_id: UserId,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn send_thread_message(
        &self,
        _thread_id: ChannelId,
        _content: &str,
        _allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn close_lobby_message(
        &self,
        _scope: LobbyScope,
        _message_ids: &LobbyMessageIds,
        _reason: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn archive_lobby_thread(
        &self,
        _scope: LobbyScope,
        _thread_id: ChannelId,
        _reason: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_lobby_message(
        &self,
        _scope: LobbyScope,
        _channel_id: Option<ChannelId>,
        _message_id: Option<MessageId>,
        _fallback_channel_id: ChannelId,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn respond_ephemerally(
        &self,
        _content: &str,
        _allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CrossLobbyEvictionReport {
    pub evicted: BTreeMap<LobbyKind, BTreeSet<UserId>>,
    pub publication_failures: Vec<PublicationFailure>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeaveAllOutcome {
    pub left: Vec<LobbyKind>,
    pub pinned: Vec<LobbyKind>,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedPendingMatch {
    /// `None` is a legacy pending match and therefore guards both lobby kinds.
    pub lobby_kind: Option<LobbyKind>,
    pub shuffle_message_jump_url: Option<String>,
}

pub trait ResetPendingMatchPort: Send + Sync {
    fn pending_matches(&self, guild_id: GuildId) -> Result<Vec<ScopedPendingMatch>, String>;
}

impl ResetPendingMatchPort for () {
    fn pending_matches(&self, _guild_id: GuildId) -> Result<Vec<ScopedPendingMatch>, String> {
        Ok(Vec::new())
    }
}

pub trait ActiveDraftPort: Send + Sync {
    fn active_draft_kind(&self, guild_id: GuildId) -> Option<LobbyKind>;
}

impl ActiveDraftPort for () {
    fn active_draft_kind(&self, _guild_id: GuildId) -> Option<LobbyKind> {
        None
    }
}

impl ActiveDraftPort for DraftStateManager {
    fn active_draft_kind(&self, guild_id: GuildId) -> Option<LobbyKind> {
        let state = self.get_state(Some(guild_id.0))?;
        let kind = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lobby_kind;
        Some(match kind {
            DraftLobbyKind::Open => LobbyKind::Open,
            DraftLobbyKind::LowSkill => LobbyKind::LowSkill,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetLobbyRequest {
    pub guild_id: GuildId,
    pub user_id: UserId,
    pub is_admin: bool,
    pub explicit_kind: Option<LobbyKind>,
    pub interaction_channel_id: ChannelId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResetRefusal {
    NoLobbySelection,
    Ambiguous,
    PendingMatch,
    NoActiveLobby,
    PermissionDenied,
    ActiveDraft,
    InFlight,
    Busy,
    GuardFailure,
    Persistence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResetLobbyOutcome {
    Reset {
        scope: LobbyScope,
        content: String,
        publication_failures: Vec<PublicationFailure>,
    },
    Refused {
        scope: Option<LobbyScope>,
        reason: ResetRefusal,
        content: String,
    },
}

/// Shared command policy over the real lobby service.  Operation locks are
/// keyed by `(guild, kind)`, so cleanup for one lobby neither blocks nor guards
/// its sibling.
pub struct LobbyCommandRuntime<P, M, C> {
    service: Arc<LobbyService<P, M, C>>,
    ready_notifications: Mutex<BTreeMap<LobbyScope, MessageId>>,
    operation_locks: Mutex<BTreeMap<LobbyScope, Arc<tokio::sync::Mutex<()>>>>,
}

impl<P, M, C> LobbyCommandRuntime<P, M, C>
where
    P: LobbyPlayerPort,
    M: PendingMatchPort,
    C: LobbyClock,
{
    #[must_use]
    pub fn new(service: Arc<LobbyService<P, M, C>>) -> Self {
        Self {
            service,
            ready_notifications: Mutex::new(BTreeMap::new()),
            operation_locks: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub const fn service(&self) -> &Arc<LobbyService<P, M, C>> {
        &self.service
    }

    fn ready_notifications(&self) -> MutexGuard<'_, BTreeMap<LobbyScope, MessageId>> {
        self.ready_notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn operation_locks(&self) -> MutexGuard<'_, BTreeMap<LobbyScope, Arc<tokio::sync::Mutex<()>>>> {
        self.operation_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn operation_lock(&self, scope: LobbyScope) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.operation_locks()
                .entry(scope)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// The single mutation boundary shared by command creation/join/leave,
    /// shuffle reservation, and reset for this `(guild, lobby kind)`.
    #[must_use]
    pub fn scope_operation_lock(&self, scope: LobbyScope) -> Arc<tokio::sync::Mutex<()>> {
        self.operation_lock(scope)
    }

    pub fn mark_ready_notification(&self, scope: LobbyScope, message_id: MessageId) {
        self.ready_notifications().insert(scope, message_id);
    }

    #[must_use]
    pub fn ready_notification(&self, scope: LobbyScope) -> Option<MessageId> {
        self.ready_notifications().get(&scope).copied()
    }

    /// Reserve while holding the same per-scope operation lock used by reset.
    /// This closes the cleanup/start window: after reset wins the lock, a
    /// waiting match start sees that the roster no longer exists.
    pub async fn reserve_for_match(
        &self,
        player_ids: &BTreeSet<UserId>,
        scope: LobbyScope,
    ) -> bool {
        let lock = self.operation_lock(scope);
        let _guard = lock.lock().await;
        self.service.reserve_lobby_players(player_ids, scope)
    }

    pub async fn evict_matched_players_from_other_lobbies(
        &self,
        player_ids: &BTreeSet<UserId>,
        popped_scope: LobbyScope,
        transport: &dyn LobbyRuntimeTransport,
    ) -> CrossLobbyEvictionReport {
        let mut report = CrossLobbyEvictionReport::default();
        if player_ids.is_empty() {
            return report;
        }

        for other_kind in [LobbyKind::Open, LobbyKind::LowSkill] {
            if other_kind == popped_scope.kind {
                continue;
            }
            let other_scope = LobbyScope::new(popped_scope.guild_id, other_kind);
            let removed = self
                .service
                .remove_players_from_lobby(player_ids, other_scope);
            if removed.is_empty() {
                continue;
            }
            report.evicted.insert(other_kind, removed.clone());

            // The state reconciliation is authoritative and happens even when
            // every following Discord operation fails.
            let confirmed = self.service.prune_readycheck_to_lobby_members(other_scope);
            if let Err(message) = transport.sync_lobby_display(other_scope).await {
                report.publication_failures.push(PublicationFailure {
                    scope: other_scope,
                    stage: PublicationStage::Display,
                    message,
                });
            }
            if confirmed < self.service.ready_threshold() {
                self.ready_notifications().remove(&other_scope);
            }

            for player_id in &removed {
                if let Err(message) = transport
                    .remove_lobby_reaction(other_scope, *player_id)
                    .await
                {
                    report.publication_failures.push(PublicationFailure {
                        scope: other_scope,
                        stage: PublicationStage::Reaction,
                        message,
                    });
                }
            }

            let Some(thread_id) = self
                .service
                .get_lobby(other_scope)
                .and_then(|lobby| lobby.message_ids.thread_id)
            else {
                continue;
            };
            let mentions = removed
                .iter()
                .map(|player_id| format!("<@{}>", player_id.0))
                .collect::<Vec<_>>()
                .join(", ");
            let content = format!(
                "🔀 {mentions} left for the {} match.",
                popped_scope.kind.label()
            );
            if let Err(message) = transport
                .send_thread_message(thread_id, &content, AllowedMentions::None)
                .await
            {
                report.publication_failures.push(PublicationFailure {
                    scope: other_scope,
                    stage: PublicationStage::Thread,
                    message,
                });
            }
        }
        report
    }

    #[must_use]
    pub fn leave_every_lobby(&self, player_id: UserId, guild_id: GuildId) -> LeaveAllOutcome {
        let memberships = self.service.get_lobby_kinds_for_player(player_id, guild_id);
        if memberships.is_empty() {
            return LeaveAllOutcome {
                content: "⚠️ You're not in a lobby.".to_owned(),
                ..LeaveAllOutcome::default()
            };
        }

        let mut left = Vec::new();
        let mut pinned = Vec::new();
        for kind in memberships {
            let scope = LobbyScope::new(guild_id, kind);
            if self.service.leave_lobby(player_id, scope) {
                left.push(kind);
            } else if self
                .service
                .get_in_flight_lobby_kind_for_player(player_id, guild_id)
                == Some(kind)
            {
                pinned.push(kind);
            }
        }

        let content = if left.is_empty() {
            if pinned.is_empty() {
                "⚠️ You’re no longer in the lobby.".to_owned()
            } else {
                format!(
                    "❌ You can’t leave {} while its shuffle or draft is in progress.",
                    format_lobby_labels(&pinned)
                )
            }
        } else {
            let mut content = format!("✅ Left {}.", format_lobby_labels(&left));
            if !pinned.is_empty() {
                content.push_str(&format!(
                    "\n⚠️ Still in {} — its shuffle or draft is in progress.",
                    format_lobby_labels(&pinned)
                ));
            }
            content
        };
        LeaveAllOutcome {
            left,
            pinned,
            content,
        }
    }

    pub fn select_readycheck_lobby(
        &self,
        player_id: UserId,
        guild_id: GuildId,
        explicit: Option<LobbyKind>,
    ) -> Result<LobbyKind, LobbySelectionError> {
        resolve_lobby_kind(
            &self.service.get_lobby_kinds_for_player(player_id, guild_id),
            explicit,
            READYCHECK_NO_LOBBY_MESSAGE,
        )
    }

    pub async fn reset_lobby(
        &self,
        request: &ResetLobbyRequest,
        pending_matches: &dyn ResetPendingMatchPort,
        drafts: &dyn ActiveDraftPort,
        transport: &dyn LobbyRuntimeTransport,
    ) -> ResetLobbyOutcome {
        let memberships = self
            .service
            .get_lobby_kinds_for_player(request.user_id, request.guild_id);
        let kind =
            match resolve_lobby_kind(&memberships, request.explicit_kind, RESET_NO_LOBBY_MESSAGE) {
                Ok(kind) => kind,
                Err(LobbySelectionError::NoLobby { message }) => {
                    return self
                        .refuse(transport, None, ResetRefusal::NoLobbySelection, message)
                        .await;
                }
                Err(LobbySelectionError::Ambiguous) => {
                    return self
                        .refuse(
                            transport,
                            None,
                            ResetRefusal::Ambiguous,
                            AMBIGUOUS_LOBBY_MESSAGE.to_owned(),
                        )
                        .await;
                }
            };
        let scope = LobbyScope::new(request.guild_id, kind);

        match pending_refusal(pending_matches, scope) {
            Ok(Some(content)) => {
                return self
                    .refuse(transport, Some(scope), ResetRefusal::PendingMatch, content)
                    .await;
            }
            Ok(None) => {}
            Err(()) => {
                return self
                    .refuse(
                        transport,
                        Some(scope),
                        ResetRefusal::GuardFailure,
                        pending_guard_failure_message(),
                    )
                    .await;
            }
        }

        let Some(lobby) = self.service.get_lobby(scope) else {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::NoActiveLobby,
                    format!("⚠️ No active {} lobby.", kind.label()),
                )
                .await;
        };
        if !request.is_admin && lobby.created_by != Some(request.user_id) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::PermissionDenied,
                    "❌ Permission denied. Admin or lobby creator only.".to_owned(),
                )
                .await;
        }
        if drafts.active_draft_kind(request.guild_id) == Some(kind) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::ActiveDraft,
                    active_draft_message(),
                )
                .await;
        }
        if self.service.has_in_flight_lobby_players(scope) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::InFlight,
                    in_flight_message(),
                )
                .await;
        }

        let lock = self.operation_lock(scope);
        let Ok(_guard) = tokio::time::timeout(Duration::from_millis(500), lock.lock()).await else {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::Busy,
                    format!(
                        "❌ {} is currently starting a shuffle or draft. Please try again in a moment.",
                        kind.label()
                    ),
                )
                .await;
        };

        // Repeat every volatile guard under the shared operation lock.
        match pending_refusal(pending_matches, scope) {
            Ok(Some(content)) => {
                return self
                    .refuse(transport, Some(scope), ResetRefusal::PendingMatch, content)
                    .await;
            }
            Ok(None) => {}
            Err(()) => {
                return self
                    .refuse(
                        transport,
                        Some(scope),
                        ResetRefusal::GuardFailure,
                        pending_guard_failure_message(),
                    )
                    .await;
            }
        }
        if drafts.active_draft_kind(request.guild_id) == Some(kind) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::ActiveDraft,
                    active_draft_message(),
                )
                .await;
        }
        if self.service.has_in_flight_lobby_players(scope) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::InFlight,
                    in_flight_message(),
                )
                .await;
        }
        let Some(lobby) = self.service.get_lobby(scope) else {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::NoActiveLobby,
                    format!("⚠️ No active {} lobby.", kind.label()),
                )
                .await;
        };

        let ids = lobby.message_ids;
        let mut publication_failures = Vec::new();
        collect_publication_failure(
            &mut publication_failures,
            scope,
            PublicationStage::Close,
            transport
                .close_lobby_message(scope, &ids, "Lobby Reset")
                .await,
        );
        if let Some(thread_id) = ids.thread_id {
            collect_publication_failure(
                &mut publication_failures,
                scope,
                PublicationStage::Archive,
                transport
                    .archive_lobby_thread(scope, thread_id, "Lobby Reset")
                    .await,
            );
        }
        collect_publication_failure(
            &mut publication_failures,
            scope,
            PublicationStage::Unpin,
            transport
                .unpin_lobby_message(
                    scope,
                    ids.channel_id,
                    ids.message_id,
                    request.interaction_channel_id,
                )
                .await,
        );

        if let Err(_error) = self.service.try_reset_lobby(scope) {
            return self
                .refuse(
                    transport,
                    Some(scope),
                    ResetRefusal::Persistence,
                    format!(
                        "❌ Could not persist the {} reset. Please try again.",
                        kind.label()
                    ),
                )
                .await;
        }
        self.ready_notifications().remove(&scope);
        let content = format!(
            "✅ {} reset. You can create a new lobby with `/lobby`.",
            kind.label()
        );
        collect_publication_failure(
            &mut publication_failures,
            scope,
            PublicationStage::Response,
            transport
                .respond_ephemerally(&content, AllowedMentions::Default)
                .await,
        );
        ResetLobbyOutcome::Reset {
            scope,
            content,
            publication_failures,
        }
    }

    async fn refuse(
        &self,
        transport: &dyn LobbyRuntimeTransport,
        scope: Option<LobbyScope>,
        reason: ResetRefusal,
        content: String,
    ) -> ResetLobbyOutcome {
        // Python's safe follow-up is fail-soft: failure to explain a refusal
        // must not convert the refusal into a state mutation.
        let _ = transport
            .respond_ephemerally(&content, AllowedMentions::Default)
            .await;
        ResetLobbyOutcome::Refused {
            scope,
            reason,
            content,
        }
    }
}

fn collect_publication_failure(
    failures: &mut Vec<PublicationFailure>,
    scope: LobbyScope,
    stage: PublicationStage,
    result: Result<(), String>,
) {
    if let Err(message) = result {
        failures.push(PublicationFailure {
            scope,
            stage,
            message,
        });
    }
}

fn pending_refusal(
    port: &dyn ResetPendingMatchPort,
    scope: LobbyScope,
) -> Result<Option<String>, ()> {
    let pending = port.pending_matches(scope.guild_id).map_err(|_| ())?;
    let pending = pending
        .into_iter()
        .find(|pending| pending.lobby_kind.is_none() || pending.lobby_kind == Some(scope.kind));
    let Some(pending) = pending else {
        return Ok(None);
    };
    let mut message = "❌ There's a pending match that needs to be recorded!".to_owned();
    if let Some(jump_url) = pending.shuffle_message_jump_url {
        message.push_str(&format!(
            " [View pending match]({jump_url}) then use `/record` first."
        ));
    } else {
        message.push_str(" Use `/record` first.");
    }
    Ok(Some(message))
}

fn pending_guard_failure_message() -> String {
    "❌ Could not verify pending matches, so the lobby was not reset. Please try again.".to_owned()
}

fn active_draft_message() -> String {
    "❌ There's an active draft in progress. Use `/draft restart` first to clear the draft."
        .to_owned()
}

fn in_flight_message() -> String {
    "❌ This lobby can’t be reset while its shuffle or draft is starting. Please try again in a moment."
        .to_owned()
}

#[cfg(test)]
#[path = "lobby_runtime/tests.rs"]
mod tests;
