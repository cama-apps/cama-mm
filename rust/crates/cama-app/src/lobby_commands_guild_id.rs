//! Guild-scoped lobby command and Discord-transport policy.
//!
//! This is an additive, gateway-neutral application model.  It normalizes an
//! interaction's guild before every player/lobby lookup, keeps lobby state
//! isolated by [`LobbyScope`], and makes command publication dependencies
//! explicit as a task graph.  Python remains the live Discord, repository,
//! schema, and runtime authority during the parity phase.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::dedicated_lobby_channel::{
    ChannelId, GuildId, LobbyMessageIds, LobbyScope, MessageId, UserId,
};
use crate::embeds::LobbyKind;

pub const REGISTRATION_REQUIRED: &str = "❌ You're not registered! Use `/player register` first.";
pub const ROLES_REQUIRED: &str = "❌ Set your preferred roles first! Use `/player roles`.";
pub const IN_FLIGHT_LEAVE_FRAGMENT: &str = "shuffle or draft is in progress";
pub const LOW_SKILL_RATING_CUTOFF: i32 = 1_400;
pub const LOBBY_CAPACITY: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InteractionContext {
    pub guild_id: Option<GuildId>,
    pub user_id: UserId,
    pub channel_id: ChannelId,
}

impl InteractionContext {
    #[must_use]
    pub const fn normalized_guild_id(self) -> GuildId {
        GuildId::normalize(self.guild_id)
    }

    #[must_use]
    pub const fn scope(self, kind: LobbyKind) -> LobbyScope {
        LobbyScope::new(self.normalized_guild_id(), kind)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredPlayer {
    pub display_name: String,
    pub preferred_roles: Vec<String>,
    pub glicko_rating: i32,
}

impl RegisteredPlayer {
    #[must_use]
    pub fn eligible(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            preferred_roles: vec!["1".to_owned(), "2".to_owned()],
            glicko_rating: 1_500,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandLobby {
    pub scope: Option<LobbyScope>,
    pub created_by: Option<UserId>,
    pub players: BTreeSet<UserId>,
    pub reserved_players: BTreeSet<UserId>,
    pub message_ids: LobbyMessageIds,
}

impl CommandLobby {
    #[must_use]
    pub fn contains(&self, user_id: UserId) -> bool {
        self.players.contains(&user_id)
    }

    #[must_use]
    pub fn player_count(&self) -> usize {
        self.players.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Followup {
    pub content: String,
    pub ephemeral: bool,
    pub allowed_mention_ids: Vec<UserId>,
}

impl Followup {
    fn ephemeral(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: true,
            allowed_mention_ids: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PolicyAction {
    MembershipCommitted,
    SendStarterMessage,
    PinStarter,
    AddLobbyReactions,
    CreateThread,
    PersistMessageIds,
    DeleteStarterMessage,
    AutoJoin,
    ConfirmationFollowup,
    DisplaySync,
    ReactionCleanup,
    JoinActivity,
    LeaveActivity,
    RallyOrReady,
    WatcherDirectMessage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskNode {
    pub action: PolicyAction,
    pub depends_on: Vec<PolicyAction>,
    pub best_effort: bool,
    pub detached: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicationGraph {
    nodes: BTreeMap<PolicyAction, TaskNode>,
}

impl PublicationGraph {
    #[must_use]
    pub fn from_nodes(nodes: impl IntoIterator<Item = TaskNode>) -> Self {
        Self {
            nodes: nodes.into_iter().map(|node| (node.action, node)).collect(),
        }
    }

    #[must_use]
    pub fn node(&self, action: PolicyAction) -> Option<&TaskNode> {
        self.nodes.get(&action)
    }

    #[must_use]
    pub fn contains(&self, action: PolicyAction) -> bool {
        self.nodes.contains_key(&action)
    }

    #[must_use]
    pub fn must_precede(&self, earlier: PolicyAction, later: PolicyAction) -> bool {
        fn visit(
            graph: &PublicationGraph,
            target: PolicyAction,
            current: PolicyAction,
            visited: &mut BTreeSet<PolicyAction>,
        ) -> bool {
            if !visited.insert(current) {
                return false;
            }
            let Some(node) = graph.node(current) else {
                return false;
            };
            node.depends_on.iter().any(|dependency| {
                *dependency == target || visit(graph, target, *dependency, visited)
            })
        }
        visit(self, earlier, later, &mut BTreeSet::new())
    }

    #[must_use]
    pub fn can_overlap(&self, left: PolicyAction, right: PolicyAction) -> bool {
        self.contains(left)
            && self.contains(right)
            && !self.must_precede(left, right)
            && !self.must_precede(right, left)
    }

    /// Return deterministic topological waves.  Actions in one inner vector
    /// may start together; every dependency is in an earlier wave.
    #[must_use]
    pub fn waves(&self) -> Vec<Vec<PolicyAction>> {
        let mut completed = BTreeSet::new();
        let mut remaining = self.nodes.keys().copied().collect::<BTreeSet<_>>();
        let mut waves = Vec::new();
        while !remaining.is_empty() {
            let ready = remaining
                .iter()
                .copied()
                .filter(|action| {
                    self.nodes[action]
                        .depends_on
                        .iter()
                        .all(|dependency| completed.contains(dependency))
                })
                .collect::<Vec<_>>();
            if ready.is_empty() {
                break;
            }
            for action in &ready {
                remaining.remove(action);
                completed.insert(*action);
            }
            waves.push(ready);
        }
        waves
    }
}

#[must_use]
pub fn new_lobby_publication_graph() -> PublicationGraph {
    PublicationGraph::from_nodes([
        TaskNode {
            action: PolicyAction::SendStarterMessage,
            depends_on: Vec::new(),
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::PinStarter,
            depends_on: vec![PolicyAction::SendStarterMessage],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::AddLobbyReactions,
            depends_on: vec![PolicyAction::SendStarterMessage],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::CreateThread,
            depends_on: vec![PolicyAction::SendStarterMessage],
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::PersistMessageIds,
            depends_on: vec![
                PolicyAction::PinStarter,
                PolicyAction::AddLobbyReactions,
                PolicyAction::CreateThread,
            ],
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::AutoJoin,
            depends_on: vec![PolicyAction::PersistMessageIds],
            best_effort: false,
            detached: false,
        },
    ])
}

#[must_use]
pub fn join_publication_graph(reminder_available: bool) -> PublicationGraph {
    let mut nodes = vec![
        TaskNode {
            action: PolicyAction::MembershipCommitted,
            depends_on: Vec::new(),
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::ConfirmationFollowup,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::DisplaySync,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::JoinActivity,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::RallyOrReady,
            depends_on: vec![PolicyAction::JoinActivity],
            best_effort: true,
            detached: false,
        },
    ];
    if reminder_available {
        nodes.push(TaskNode {
            action: PolicyAction::WatcherDirectMessage,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: true,
        });
    }
    PublicationGraph::from_nodes(nodes)
}

#[must_use]
pub fn leave_publication_graph() -> PublicationGraph {
    PublicationGraph::from_nodes([
        TaskNode {
            action: PolicyAction::MembershipCommitted,
            depends_on: Vec::new(),
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::ConfirmationFollowup,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: false,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::DisplaySync,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::ReactionCleanup,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: false,
        },
        TaskNode {
            action: PolicyAction::LeaveActivity,
            depends_on: vec![PolicyAction::MembershipCommitted],
            best_effort: true,
            detached: false,
        },
    ])
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatcherNotification {
    pub target_id: UserId,
    pub target_display_name: String,
    pub guild_id: GuildId,
    pub lobby_kind: LobbyKind,
    pub join_cutoff_ns: u64,
    pub detached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupTrace {
    pub operation: &'static str,
    pub guild_id: GuildId,
    pub user_id: Option<UserId>,
    pub lobby_kind: Option<LobbyKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageAccess {
    Fetch,
    Partial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageMutationKind {
    EditLobbyEmbed,
    EditThreadEmbed,
    RemoveSwordReaction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageMutation {
    pub channel_id: ChannelId,
    pub message_id: MessageId,
    pub access: MessageAccess,
    pub kind: MessageMutationKind,
    pub guild_id: GuildId,
    pub lobby_kind: LobbyKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckSuccess {
    pub kind: LobbyKind,
    pub message_jump_url: String,
    pub is_refresh: bool,
    pub mention_ids: Vec<UserId>,
    pub readycheck_channel_id: ChannelId,
}

#[must_use]
pub fn readycheck_followup(
    invocation_channel_id: ChannelId,
    success: &ReadycheckSuccess,
) -> Followup {
    if !success.mention_ids.is_empty() && invocation_channel_id != success.readycheck_channel_id {
        let mentions = success
            .mention_ids
            .iter()
            .map(|user_id| format!("<@{}>", user_id.0))
            .collect::<Vec<_>>()
            .join(" ");
        return Followup {
            content: format!(
                "⚔️ **{} ready check!** {mentions}\n[React in the lobby thread]({})",
                success.kind.label(),
                success.message_jump_url,
            ),
            ephemeral: false,
            allowed_mention_ids: success.mention_ids.clone(),
        };
    }
    Followup::ephemeral(format!(
        "✅ {} ready check {}! [View]({})",
        success.kind.label(),
        if success.is_refresh {
            "refreshed"
        } else {
            "posted"
        },
        success.message_jump_url,
    ))
}

#[must_use]
pub fn infer_lobby_kind(
    memberships: &[LobbyKind],
    explicit: Option<LobbyKind>,
) -> Option<LobbyKind> {
    explicit.or_else(|| (memberships.len() == 1).then(|| memberships[0]))
}

pub const LOBBY_REACTIONS: [&str; 4] = ["⚔️", "jopacoin", "📋", "🔔"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewLobbyBranchOutcomes {
    pub pin_error: Option<String>,
    /// Zero-based reaction that fails.  The ordered reaction branch stops at
    /// that reaction, exactly as the Python `try` around the whole sequence.
    pub reaction_error_at: Option<usize>,
    pub thread_result: Result<ChannelId, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewLobbyPublicationReport {
    pub graph: PublicationGraph,
    pub reactions_attempted: Vec<&'static str>,
    pub optional_warnings: Vec<String>,
    pub persisted_thread_id: Option<ChannelId>,
    pub starter_deleted: bool,
    pub auto_join_allowed: bool,
    pub followup: Followup,
}

#[must_use]
pub fn evaluate_new_lobby_publication(
    kind: LobbyKind,
    outcomes: &NewLobbyBranchOutcomes,
) -> NewLobbyPublicationReport {
    let attempted_count = outcomes
        .reaction_error_at
        .map_or(LOBBY_REACTIONS.len(), |failed| {
            failed.saturating_add(1).min(LOBBY_REACTIONS.len())
        });
    let reactions_attempted = LOBBY_REACTIONS[..attempted_count].to_vec();
    let mut optional_warnings = Vec::new();
    if let Some(error) = &outcomes.pin_error {
        optional_warnings.push(format!("Failed to pin lobby message: {error}"));
    }
    if outcomes.reaction_error_at.is_some() {
        optional_warnings.push("Failed to add lobby reactions: reaction failed".to_owned());
    }

    match &outcomes.thread_result {
        Ok(thread_id) => NewLobbyPublicationReport {
            graph: new_lobby_publication_graph(),
            reactions_attempted,
            optional_warnings,
            persisted_thread_id: Some(*thread_id),
            starter_deleted: false,
            auto_join_allowed: true,
            followup: Followup::ephemeral(format!("✅ {} created!", kind.label())),
        },
        Err(_) => NewLobbyPublicationReport {
            graph: new_lobby_publication_graph(),
            reactions_attempted,
            optional_warnings,
            persisted_thread_id: None,
            starter_deleted: true,
            auto_join_allowed: false,
            followup: Followup::ephemeral(
                "❌ Failed to create lobby thread. Please try again or contact an admin.",
            ),
        },
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WaveOutcomes {
    pub failed_actions: BTreeSet<PolicyAction>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WaveReport {
    pub confirmation_failed: bool,
    pub isolated_maintenance_failures: Vec<PolicyAction>,
    pub rally_or_ready_still_runs: bool,
}

#[must_use]
pub fn evaluate_publication_wave(graph: &PublicationGraph, outcomes: &WaveOutcomes) -> WaveReport {
    let confirmation_failed = outcomes
        .failed_actions
        .contains(&PolicyAction::ConfirmationFollowup);
    let isolated_maintenance_failures = outcomes
        .failed_actions
        .iter()
        .copied()
        .filter(|action| graph.node(*action).is_some_and(|node| node.best_effort))
        .collect::<Vec<_>>();
    WaveReport {
        confirmation_failed,
        rally_or_ready_still_runs: graph.contains(PolicyAction::RallyOrReady)
            && !outcomes
                .failed_actions
                .contains(&PolicyAction::JoinActivity),
        isolated_maintenance_failures,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JoinOptions {
    pub reminder_available: bool,
    pub joined_at_ns: Option<u64>,
    pub refreshed_lobby_available: bool,
}

impl JoinOptions {
    #[must_use]
    pub const fn normal() -> Self {
        Self {
            reminder_available: false,
            joined_at_ns: None,
            refreshed_lobby_available: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinFailure {
    Unregistered,
    MissingRoles,
    MissingLobby,
    LobbyFull,
    AlreadyJoined,
    RatingTooHigh,
    InFlight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinCommandResult {
    pub scope: LobbyScope,
    pub joined: bool,
    pub failure: Option<JoinFailure>,
    pub followup: Followup,
    pub notification: Option<WatcherNotification>,
    pub graph: PublicationGraph,
    pub lookup_trace: Vec<LookupTrace>,
    pub refreshed_lobby_available: bool,
    pub player_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoJoinResult {
    pub scope: LobbyScope,
    pub joined: bool,
    pub warning: Option<String>,
    pub notification: Option<WatcherNotification>,
    pub graph: PublicationGraph,
    pub lookup_trace: Vec<LookupTrace>,
    pub refreshed_lobby_available: bool,
    pub player_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyCommandResult {
    pub scope: LobbyScope,
    pub created: bool,
    pub joined: bool,
    pub followup: Followup,
    pub publication_graph: Option<PublicationGraph>,
    pub validation_mutation: Option<MessageMutation>,
    pub refresh_mutations: Vec<MessageMutation>,
    pub lookup_trace: Vec<LookupTrace>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaveCommandResult {
    pub guild_id: GuildId,
    pub left_kinds: Vec<LobbyKind>,
    pub pinned_kinds: Vec<LobbyKind>,
    pub followup: Followup,
    pub graph: PublicationGraph,
    pub lookup_trace: Vec<LookupTrace>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KickCommandResult {
    pub guild_id: GuildId,
    pub removed_from: Vec<LobbyKind>,
    pub followup: Followup,
    pub lookup_trace: Vec<LookupTrace>,
}

#[derive(Debug, Default)]
struct LobbyCommandState {
    players: BTreeMap<(GuildId, UserId), RegisteredPlayer>,
    lobbies: BTreeMap<LobbyScope, CommandLobby>,
}

#[derive(Clone, Debug)]
pub struct LobbyCommandService {
    state: Arc<Mutex<LobbyCommandState>>,
    join_clock: Arc<AtomicU64>,
}

impl Default for LobbyCommandService {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(LobbyCommandState::default())),
            join_clock: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl LobbyCommandService {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, LobbyCommandState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn register_player(
        &self,
        guild_id: Option<GuildId>,
        user_id: UserId,
        player: RegisteredPlayer,
    ) {
        self.lock_state()
            .players
            .insert((GuildId::normalize(guild_id), user_id), player);
    }

    pub fn seed_lobby(
        &self,
        guild_id: Option<GuildId>,
        kind: LobbyKind,
        creator: UserId,
        players: impl IntoIterator<Item = UserId>,
    ) {
        let scope = LobbyScope::new(GuildId::normalize(guild_id), kind);
        self.lock_state().lobbies.insert(
            scope,
            CommandLobby {
                scope: Some(scope),
                created_by: Some(creator),
                players: players.into_iter().collect(),
                ..CommandLobby::default()
            },
        );
    }

    pub fn set_message_ids(&self, scope: LobbyScope, ids: LobbyMessageIds) {
        if let Some(lobby) = self.lock_state().lobbies.get_mut(&scope) {
            lobby.message_ids = ids;
        }
    }

    pub fn reserve_player(&self, scope: LobbyScope, user_id: UserId) -> bool {
        self.lock_state()
            .lobbies
            .get_mut(&scope)
            .is_some_and(|lobby| lobby.reserved_players.insert(user_id))
    }

    #[must_use]
    pub fn lobby(&self, scope: LobbyScope) -> Option<CommandLobby> {
        self.lock_state().lobbies.get(&scope).cloned()
    }

    #[must_use]
    pub fn memberships(&self, guild_id: GuildId, user_id: UserId) -> Vec<LobbyKind> {
        [LobbyKind::Open, LobbyKind::LowSkill]
            .into_iter()
            .filter(|kind| {
                self.lock_state()
                    .lobbies
                    .get(&LobbyScope::new(guild_id, *kind))
                    .is_some_and(|lobby| lobby.players.contains(&user_id))
            })
            .collect()
    }

    fn lookup_trace(
        operation: &'static str,
        guild_id: GuildId,
        user_id: Option<UserId>,
        kind: Option<LobbyKind>,
    ) -> LookupTrace {
        LookupTrace {
            operation,
            guild_id,
            user_id,
            lobby_kind: kind,
        }
    }

    #[must_use]
    pub fn create_or_view_lobby(
        &self,
        interaction: InteractionContext,
        explicit_kind: Option<LobbyKind>,
    ) -> LobbyCommandResult {
        let guild_id = interaction.normalized_guild_id();
        let kind = explicit_kind.unwrap_or(LobbyKind::Open);
        let scope = LobbyScope::new(guild_id, kind);
        let lookup_trace = vec![Self::lookup_trace(
            "get_player",
            guild_id,
            Some(interaction.user_id),
            None,
        )];
        let mut state = self.lock_state();
        let Some(player) = state.players.get(&(guild_id, interaction.user_id)).cloned() else {
            return LobbyCommandResult {
                scope,
                created: false,
                joined: false,
                followup: Followup::ephemeral(REGISTRATION_REQUIRED),
                publication_graph: None,
                validation_mutation: None,
                refresh_mutations: Vec::new(),
                lookup_trace,
            };
        };
        if kind == LobbyKind::LowSkill && player.glicko_rating >= LOW_SKILL_RATING_CUTOFF {
            return LobbyCommandResult {
                scope,
                created: false,
                joined: false,
                followup: Followup::ephemeral(
                    "❌ Only players below 1400 Glicko can create Whine & Cheese.",
                ),
                publication_graph: None,
                validation_mutation: None,
                refresh_mutations: Vec::new(),
                lookup_trace,
            };
        }

        let created = !state.lobbies.contains_key(&scope);
        let lobby = state.lobbies.entry(scope).or_insert_with(|| CommandLobby {
            scope: Some(scope),
            created_by: Some(interaction.user_id),
            ..CommandLobby::default()
        });
        let joined = !lobby.players.contains(&interaction.user_id)
            && !player.preferred_roles.is_empty()
            && lobby.players.insert(interaction.user_id);
        let metadata = lobby.message_ids.clone();
        let validation_mutation = (!created)
            .then_some(metadata.message_id)
            .flatten()
            .zip(metadata.channel_id)
            .map(|(message_id, channel_id)| MessageMutation {
                channel_id,
                message_id,
                access: MessageAccess::Fetch,
                kind: MessageMutationKind::EditLobbyEmbed,
                guild_id,
                lobby_kind: kind,
            });
        let refresh_mutations = if !created {
            metadata
                .message_id
                .zip(metadata.channel_id)
                .map(|(message_id, channel_id)| {
                    vec![MessageMutation {
                        channel_id,
                        message_id,
                        access: MessageAccess::Partial,
                        kind: if metadata.embed_message_id == Some(message_id) {
                            MessageMutationKind::EditThreadEmbed
                        } else {
                            MessageMutationKind::EditLobbyEmbed
                        },
                        guild_id,
                        lobby_kind: kind,
                    }]
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let content = if created && joined {
            format!("✅ {} created and joined!", kind.label())
        } else if created {
            format!("✅ {} created!", kind.label())
        } else if joined {
            format!("✅ Joined {}!", kind.label())
        } else {
            format!("[View {}]", kind.label())
        };
        LobbyCommandResult {
            scope,
            created,
            joined,
            followup: Followup::ephemeral(content),
            publication_graph: created.then(new_lobby_publication_graph),
            validation_mutation,
            refresh_mutations,
            lookup_trace,
        }
    }

    fn failed_join(
        scope: LobbyScope,
        failure: JoinFailure,
        lookup_trace: Vec<LookupTrace>,
        content: impl Into<String>,
    ) -> JoinCommandResult {
        JoinCommandResult {
            scope,
            joined: false,
            failure: Some(failure),
            followup: Followup::ephemeral(content),
            notification: None,
            graph: PublicationGraph::default(),
            lookup_trace,
            refreshed_lobby_available: false,
            player_count: 0,
        }
    }

    #[must_use]
    pub fn join_lobby(
        &self,
        interaction: InteractionContext,
        explicit_kind: Option<LobbyKind>,
        options: JoinOptions,
    ) -> JoinCommandResult {
        let guild_id = interaction.normalized_guild_id();
        let kind = explicit_kind.unwrap_or(LobbyKind::Open);
        let scope = LobbyScope::new(guild_id, kind);
        let mut lookup_trace = vec![Self::lookup_trace(
            "get_player",
            guild_id,
            Some(interaction.user_id),
            None,
        )];
        let mut state = self.lock_state();
        let Some(player) = state.players.get(&(guild_id, interaction.user_id)).cloned() else {
            return Self::failed_join(
                scope,
                JoinFailure::Unregistered,
                lookup_trace,
                REGISTRATION_REQUIRED,
            );
        };
        if player.preferred_roles.is_empty() {
            return Self::failed_join(
                scope,
                JoinFailure::MissingRoles,
                lookup_trace,
                ROLES_REQUIRED,
            );
        }
        lookup_trace.push(Self::lookup_trace("get_lobby", guild_id, None, Some(kind)));
        let Some(lobby) = state.lobbies.get(&scope) else {
            return Self::failed_join(
                scope,
                JoinFailure::MissingLobby,
                lookup_trace,
                format!(
                    "⚠️ No active {} lobby. Use `/lobby` to create one.",
                    kind.label()
                ),
            );
        };
        if kind == LobbyKind::LowSkill && player.glicko_rating >= LOW_SKILL_RATING_CUTOFF {
            return Self::failed_join(
                scope,
                JoinFailure::RatingTooHigh,
                lookup_trace,
                format!(
                    "❌ {} is limited to players below 1400 Glicko.",
                    LobbyKind::LowSkill.label()
                ),
            );
        }
        if lobby.players.contains(&interaction.user_id) {
            return Self::failed_join(
                scope,
                JoinFailure::AlreadyJoined,
                lookup_trace,
                format!("❌ Already in {}, or that lobby is closed.", kind.label()),
            );
        }
        if lobby.player_count() >= LOBBY_CAPACITY {
            return Self::failed_join(
                scope,
                JoinFailure::LobbyFull,
                lookup_trace,
                format!("❌ {} is full.", kind.label()),
            );
        }
        let in_flight_elsewhere = state.lobbies.iter().any(|(candidate_scope, candidate)| {
            candidate_scope.guild_id == guild_id
                && *candidate_scope != scope
                && candidate.reserved_players.contains(&interaction.user_id)
        });
        if in_flight_elsewhere {
            return Self::failed_join(
                scope,
                JoinFailure::InFlight,
                lookup_trace,
                "❌ You can’t switch lobbies while your current shuffle or draft is in progress.",
            );
        }
        let lobby = state
            .lobbies
            .get_mut(&scope)
            .expect("lobby checked immediately above");
        lobby.players.insert(interaction.user_id);
        let player_count = lobby.player_count();
        drop(state);

        let cutoff = options
            .joined_at_ns
            .unwrap_or_else(|| self.join_clock.fetch_add(1, Ordering::Relaxed));
        let notification = options.reminder_available.then_some(WatcherNotification {
            target_id: interaction.user_id,
            target_display_name: player.display_name,
            guild_id,
            lobby_kind: kind,
            join_cutoff_ns: cutoff,
            detached: true,
        });
        JoinCommandResult {
            scope,
            joined: true,
            failure: None,
            followup: Followup::ephemeral(format!("✅ Joined {}!", kind.label())),
            notification,
            graph: join_publication_graph(options.reminder_available),
            lookup_trace,
            refreshed_lobby_available: options.refreshed_lobby_available,
            player_count,
        }
    }

    #[must_use]
    pub fn auto_join_lobby(
        &self,
        interaction: InteractionContext,
        kind: LobbyKind,
        options: JoinOptions,
    ) -> AutoJoinResult {
        let guild_id = interaction.normalized_guild_id();
        let scope = LobbyScope::new(guild_id, kind);
        let lookup_trace = vec![Self::lookup_trace(
            "get_player",
            guild_id,
            Some(interaction.user_id),
            None,
        )];
        let mut state = self.lock_state();
        let already_joined = state
            .lobbies
            .get(&scope)
            .is_some_and(|lobby| lobby.players.contains(&interaction.user_id));
        if already_joined {
            let count = state.lobbies[&scope].player_count();
            return AutoJoinResult {
                scope,
                joined: false,
                warning: None,
                notification: None,
                graph: PublicationGraph::default(),
                lookup_trace,
                refreshed_lobby_available: true,
                player_count: count,
            };
        }
        let Some(player) = state.players.get(&(guild_id, interaction.user_id)).cloned() else {
            return AutoJoinResult {
                scope,
                joined: false,
                warning: Some(
                    "⚠️ Set your preferred roles with `/player roles` to auto-join.".to_owned(),
                ),
                notification: None,
                graph: PublicationGraph::default(),
                lookup_trace,
                refreshed_lobby_available: false,
                player_count: 0,
            };
        };
        if player.preferred_roles.is_empty() {
            return AutoJoinResult {
                scope,
                joined: false,
                warning: Some(
                    "⚠️ Set your preferred roles with `/player roles` to auto-join.".to_owned(),
                ),
                notification: None,
                graph: PublicationGraph::default(),
                lookup_trace,
                refreshed_lobby_available: false,
                player_count: 0,
            };
        }
        let Some(lobby) = state.lobbies.get_mut(&scope) else {
            return AutoJoinResult {
                scope,
                joined: false,
                warning: None,
                notification: None,
                graph: PublicationGraph::default(),
                lookup_trace,
                refreshed_lobby_available: false,
                player_count: 0,
            };
        };
        if kind == LobbyKind::LowSkill && player.glicko_rating >= LOW_SKILL_RATING_CUTOFF {
            return AutoJoinResult {
                scope,
                joined: false,
                warning: Some(format!(
                    "ℹ️ You can view {}, but only players below 1400 Glicko can join.",
                    kind.label()
                )),
                notification: None,
                graph: PublicationGraph::default(),
                lookup_trace,
                refreshed_lobby_available: true,
                player_count: lobby.player_count(),
            };
        }
        lobby.players.insert(interaction.user_id);
        let player_count = lobby.player_count();
        drop(state);

        let cutoff = options
            .joined_at_ns
            .unwrap_or_else(|| self.join_clock.fetch_add(1, Ordering::Relaxed));
        AutoJoinResult {
            scope,
            joined: true,
            warning: None,
            notification: options.reminder_available.then_some(WatcherNotification {
                target_id: interaction.user_id,
                target_display_name: player.display_name,
                guild_id,
                lobby_kind: kind,
                join_cutoff_ns: cutoff,
                detached: true,
            }),
            graph: join_publication_graph(options.reminder_available),
            lookup_trace,
            refreshed_lobby_available: options.refreshed_lobby_available,
            player_count,
        }
    }

    #[must_use]
    pub fn leave_lobbies(&self, interaction: InteractionContext) -> LeaveCommandResult {
        let guild_id = interaction.normalized_guild_id();
        let lookup_trace = vec![Self::lookup_trace(
            "get_lobby_kinds_for_player",
            guild_id,
            Some(interaction.user_id),
            None,
        )];
        let mut state = self.lock_state();
        let mut left_kinds = Vec::new();
        let mut pinned_kinds = Vec::new();
        for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
            let scope = LobbyScope::new(guild_id, kind);
            let Some(lobby) = state.lobbies.get_mut(&scope) else {
                continue;
            };
            if !lobby.players.contains(&interaction.user_id) {
                continue;
            }
            if lobby.reserved_players.contains(&interaction.user_id) {
                pinned_kinds.push(kind);
            } else {
                lobby.players.remove(&interaction.user_id);
                left_kinds.push(kind);
            }
        }
        let content = if left_kinds.is_empty() && pinned_kinds.is_empty() {
            "⚠️ You're not in a lobby.".to_owned()
        } else if left_kinds.is_empty() {
            format!(
                "❌ You can’t leave {} while its shuffle or draft is in progress.",
                join_lobby_labels(&pinned_kinds)
            )
        } else {
            let mut content = format!("✅ Left {}.", join_lobby_labels(&left_kinds));
            if !pinned_kinds.is_empty() {
                content.push_str(&format!(
                    "\n⚠️ Still in {} — its shuffle or draft is in progress.",
                    join_lobby_labels(&pinned_kinds)
                ));
            }
            content
        };
        LeaveCommandResult {
            guild_id,
            left_kinds,
            pinned_kinds,
            followup: Followup::ephemeral(content),
            graph: leave_publication_graph(),
            lookup_trace,
        }
    }

    #[must_use]
    pub fn kick_player(
        &self,
        interaction: InteractionContext,
        target: UserId,
        is_admin: bool,
    ) -> KickCommandResult {
        let guild_id = interaction.normalized_guild_id();
        let lookup_trace = vec![Self::lookup_trace(
            "get_lobby_kinds_for_player",
            guild_id,
            Some(target),
            None,
        )];
        let mut removed_from = Vec::new();
        if is_admin {
            let mut state = self.lock_state();
            for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
                let scope = LobbyScope::new(guild_id, kind);
                if state
                    .lobbies
                    .get_mut(&scope)
                    .is_some_and(|lobby| lobby.players.remove(&target))
                {
                    removed_from.push(kind);
                }
            }
        }
        let content = if removed_from.is_empty() {
            format!("⚠️ <@{}> is not in a lobby.", target.0)
        } else {
            format!("✅ Removed <@{}> from the lobby.", target.0)
        };
        KickCommandResult {
            guild_id,
            removed_from,
            followup: Followup::ephemeral(content),
            lookup_trace,
        }
    }
}

fn join_lobby_labels(kinds: &[LobbyKind]) -> String {
    let labels = kinds.iter().map(|kind| kind.label()).collect::<Vec<_>>();
    match labels.as_slice() {
        [] => String::new(),
        [label] => (*label).to_owned(),
        _ => format!(
            "{} and {}",
            labels[..labels.len() - 1].join(", "),
            labels[labels.len() - 1]
        ),
    }
}

#[must_use]
pub fn plan_sync_lobby_display(
    scope: LobbyScope,
    ids: &LobbyMessageIds,
) -> Option<MessageMutation> {
    ids.channel_id
        .zip(ids.message_id)
        .map(|(channel_id, message_id)| MessageMutation {
            channel_id,
            message_id,
            access: MessageAccess::Partial,
            kind: MessageMutationKind::EditLobbyEmbed,
            guild_id: scope.guild_id,
            lobby_kind: scope.kind,
        })
}

#[must_use]
pub fn plan_remove_lobby_reaction(
    scope: LobbyScope,
    ids: &LobbyMessageIds,
) -> Option<MessageMutation> {
    ids.channel_id
        .zip(ids.message_id)
        .map(|(channel_id, message_id)| MessageMutation {
            channel_id,
            message_id,
            access: MessageAccess::Partial,
            kind: MessageMutationKind::RemoveSwordReaction,
            guild_id: scope.guild_id,
            lobby_kind: scope.kind,
        })
}

#[must_use]
pub fn plan_update_thread_embed(
    scope: LobbyScope,
    ids: &LobbyMessageIds,
) -> Option<MessageMutation> {
    ids.thread_id
        .zip(ids.embed_message_id)
        .map(|(thread_id, message_id)| MessageMutation {
            channel_id: thread_id,
            message_id,
            access: MessageAccess::Partial,
            kind: MessageMutationKind::EditThreadEmbed,
            guild_id: scope.guild_id,
            lobby_kind: scope.kind,
        })
}

/// Per-scope serialization shared by command display sync and pool refresh.
/// Different guild/kind scopes receive different mutexes and can overlap.
#[derive(Clone, Debug, Default)]
pub struct LobbyMessageSerializers {
    locks: Arc<Mutex<BTreeMap<LobbyScope, Arc<Mutex<()>>>>>,
}

impl LobbyMessageSerializers {
    fn scope_lock(&self, scope: LobbyScope) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(scope)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub fn serialize<R>(&self, scope: LobbyScope, operation: impl FnOnce() -> R) -> R {
        let lock = self.scope_lock(scope);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        operation()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    const GUILD: GuildId = GuildId(12_345);
    const OTHER_GUILD: GuildId = GuildId(67_890);

    fn interaction(user_id: i64, guild_id: Option<GuildId>) -> InteractionContext {
        InteractionContext {
            guild_id,
            user_id: UserId(user_id),
            channel_id: ChannelId(456),
        }
    }

    fn register(service: &LobbyCommandService, user_id: i64, guild_id: Option<GuildId>) {
        service.register_player(
            guild_id,
            UserId(user_id),
            RegisteredPlayer::eligible(format!("Player{user_id}")),
        );
    }

    fn metadata() -> LobbyMessageIds {
        LobbyMessageIds {
            message_id: Some(MessageId(789)),
            channel_id: Some(ChannelId(456)),
            thread_id: Some(ChannelId(999)),
            embed_message_id: Some(MessageId(789)),
            origin_channel_id: Some(ChannelId(456)),
        }
    }

    #[test]
    fn test_readycheck_command_mirrors_ping_to_invocation_channel() {
        let followup = readycheck_followup(
            ChannelId(456),
            &ReadycheckSuccess {
                kind: LobbyKind::Open,
                message_jump_url: "https://discord.com/channels/123/999/789".to_owned(),
                is_refresh: false,
                mention_ids: vec![UserId(2), UserId(3)],
                readycheck_channel_id: ChannelId(999),
            },
        );
        assert_eq!(
            followup.content,
            "⚔️ **🍽️ All You Can Feed ready check!** <@2> <@3>\n\
             [React in the lobby thread](https://discord.com/channels/123/999/789)"
        );
        assert!(!followup.ephemeral);
        assert_eq!(followup.allowed_mention_ids, [UserId(2), UserId(3)]);
    }

    #[test]
    fn test_readycheck_command_does_not_duplicate_ping_in_lobby_thread() {
        let followup = readycheck_followup(
            ChannelId(999),
            &ReadycheckSuccess {
                kind: LobbyKind::Open,
                message_jump_url: "https://discord.com/channels/123/999/789".to_owned(),
                is_refresh: true,
                mention_ids: vec![UserId(2), UserId(3)],
                readycheck_channel_id: ChannelId(999),
            },
        );
        assert_eq!(
            followup,
            Followup::ephemeral(
                "✅ 🍽️ All You Can Feed ready check refreshed! \
                 [View](https://discord.com/channels/123/999/789)"
            )
        );
    }

    #[test]
    fn test_readycheck_command_infers_lowskill_membership() {
        assert_eq!(
            infer_lobby_kind(&[LobbyKind::LowSkill], None),
            Some(LobbyKind::LowSkill)
        );
    }

    #[test]
    fn test_lobby_command_uses_guild_id() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        let result = service.create_or_view_lobby(interaction(1, Some(GUILD)), None);
        let lobby = service
            .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
            .unwrap();
        assert!(result.created && result.joined);
        assert_eq!(result.scope.guild_id, GUILD);
        assert_eq!(lobby.created_by, Some(UserId(1)));
        assert!(lobby.contains(UserId(1)));
        assert!(result.followup.content.contains("created and joined"));
        assert_eq!(LOBBY_REACTIONS[2], "📋");
        assert!(
            LOBBY_REACTIONS
                .iter()
                .all(|reaction| !reaction.contains("frogling"))
        );
    }

    #[test]
    fn test_new_lobby_publication_overlaps_decorations_with_thread_creation() {
        let graph = new_lobby_publication_graph();
        let waves = graph.waves();
        assert_eq!(waves[0], [PolicyAction::SendStarterMessage]);
        assert_eq!(
            waves[1].iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                PolicyAction::PinStarter,
                PolicyAction::AddLobbyReactions,
                PolicyAction::CreateThread,
            ])
        );
        assert!(graph.can_overlap(PolicyAction::PinStarter, PolicyAction::CreateThread));
        assert!(graph.can_overlap(PolicyAction::AddLobbyReactions, PolicyAction::CreateThread));
        assert!(graph.must_precede(PolicyAction::CreateThread, PolicyAction::PersistMessageIds));
        assert!(graph.must_precede(
            PolicyAction::AddLobbyReactions,
            PolicyAction::PersistMessageIds
        ));
        assert!(graph.must_precede(PolicyAction::PersistMessageIds, PolicyAction::AutoJoin));
    }

    #[test]
    fn test_new_lobby_optional_decoration_failures_remain_isolated() {
        let report = evaluate_new_lobby_publication(
            LobbyKind::Open,
            &NewLobbyBranchOutcomes {
                pin_error: Some("pin failed".to_owned()),
                reaction_error_at: Some(1),
                thread_result: Ok(ChannelId(999)),
            },
        );
        assert_eq!(report.reactions_attempted, ["⚔️", "jopacoin"]);
        assert_eq!(report.persisted_thread_id, Some(ChannelId(999)));
        assert!(report.auto_join_allowed);
        assert!(report.followup.content.contains("created!"));
        assert!(
            report
                .optional_warnings
                .iter()
                .any(|line| line.contains("pin failed"))
        );
        assert!(
            report
                .optional_warnings
                .iter()
                .any(|line| line.contains("reaction failed"))
        );
    }

    #[test]
    fn test_new_lobby_thread_failure_awaits_decorations_before_cleanup() {
        let report = evaluate_new_lobby_publication(
            LobbyKind::Open,
            &NewLobbyBranchOutcomes {
                pin_error: None,
                reaction_error_at: None,
                thread_result: Err("thread failed".to_owned()),
            },
        );
        assert_eq!(report.reactions_attempted, LOBBY_REACTIONS);
        assert!(
            report
                .graph
                .must_precede(PolicyAction::PinStarter, PolicyAction::PersistMessageIds)
        );
        assert!(report.starter_deleted);
        assert_eq!(report.persisted_thread_id, None);
        assert!(!report.auto_join_allowed);
        assert!(
            report
                .followup
                .content
                .contains("Failed to create lobby thread")
        );
    }

    #[test]
    fn test_join_command_uses_guild_id() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            Some(LobbyKind::Open),
            JoinOptions::normal(),
        );
        assert!(result.joined);
        assert_eq!(result.scope.guild_id, GUILD);
        assert!(service.lobby(result.scope).unwrap().contains(UserId(1)));
    }

    #[test]
    fn test_join_command_defaults_to_open_lobby() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(interaction(1, Some(GUILD)), None, JoinOptions::normal());
        assert!(result.joined);
        assert_eq!(result.scope.kind, LobbyKind::Open);
    }

    #[test]
    fn test_join_command_targets_explicit_lowskill_lobby() {
        let service = LobbyCommandService::default();
        let mut player = RegisteredPlayer::eligible("Player1");
        player.glicko_rating = 1_300;
        service.register_player(Some(GUILD), UserId(1), player);
        service.seed_lobby(Some(GUILD), LobbyKind::LowSkill, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            Some(LobbyKind::LowSkill),
            JoinOptions::normal(),
        );
        assert!(result.joined);
        assert!(
            service
                .lobby(LobbyScope::new(GUILD, LobbyKind::LowSkill))
                .unwrap()
                .contains(UserId(1))
        );
        assert!(
            service
                .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
                .is_none()
        );
    }

    #[test]
    fn test_leave_command_uses_guild_id() {
        let service = LobbyCommandService::default();
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), [UserId(1)]);
        let result = service.leave_lobbies(interaction(1, Some(GUILD)));
        assert_eq!(result.guild_id, GUILD);
        assert_eq!(result.left_kinds, [LobbyKind::Open]);
        assert!(
            !service
                .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
                .unwrap()
                .contains(UserId(1))
        );
    }

    #[test]
    fn test_leave_command_infers_lowskill_membership() {
        let service = LobbyCommandService::default();
        service.seed_lobby(Some(GUILD), LobbyKind::LowSkill, UserId(99), [UserId(1)]);
        let result = service.leave_lobbies(interaction(1, Some(GUILD)));
        assert_eq!(result.left_kinds, [LobbyKind::LowSkill]);
        assert!(
            !service
                .lobby(LobbyScope::new(GUILD, LobbyKind::LowSkill))
                .unwrap()
                .contains(UserId(1))
        );
    }

    #[test]
    fn test_leave_command_blocks_player_reserved_by_shuffle_or_draft() {
        let service = LobbyCommandService::default();
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), [UserId(1)]);
        assert!(service.reserve_player(scope, UserId(1)));
        let result = service.leave_lobbies(interaction(1, Some(GUILD)));
        assert!(result.left_kinds.is_empty());
        assert_eq!(result.pinned_kinds, [LobbyKind::Open]);
        assert!(service.lobby(scope).unwrap().contains(UserId(1)));
        assert!(result.followup.ephemeral);
        assert!(result.followup.content.contains(IN_FLIGHT_LEAVE_FRAGMENT));
    }

    #[test]
    fn test_kick_command_uses_guild_id() {
        let service = LobbyCommandService::default();
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(1), [UserId(42)]);
        service.seed_lobby(Some(OTHER_GUILD), LobbyKind::Open, UserId(1), [UserId(42)]);
        let result = service.kick_player(interaction(1, Some(GUILD)), UserId(42), true);
        assert_eq!(result.removed_from, [LobbyKind::Open]);
        assert!(
            !service
                .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
                .unwrap()
                .contains(UserId(42))
        );
        assert!(
            service
                .lobby(LobbyScope::new(OTHER_GUILD, LobbyKind::Open))
                .unwrap()
                .contains(UserId(42))
        );
    }

    #[test]
    fn test_lobby_command_unregistered_player() {
        let service = LobbyCommandService::default();
        let result = service.create_or_view_lobby(interaction(999, Some(GUILD)), None);
        assert!(result.followup.content.to_lowercase().contains("register"));
        assert_eq!(result.lookup_trace[0].guild_id, GUILD);
    }

    #[test]
    fn test_join_command_unregistered_player() {
        let service = LobbyCommandService::default();
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(999, Some(GUILD)),
            Some(LobbyKind::Open),
            JoinOptions::normal(),
        );
        assert_eq!(result.failure, Some(JoinFailure::Unregistered));
        assert!(result.followup.content.to_lowercase().contains("register"));
    }

    #[test]
    fn test_lobby_command_with_none_guild() {
        let service = LobbyCommandService::default();
        register(&service, 1, None);
        let result = service.create_or_view_lobby(interaction(1, None), None);
        assert_eq!(result.scope.guild_id, GuildId::DEFAULT);
        assert!(result.created && result.joined);
    }

    #[test]
    fn test_auto_join_lobby_uses_guild_id() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.auto_join_lobby(
            interaction(1, Some(GUILD)),
            LobbyKind::Open,
            JoinOptions::normal(),
        );
        assert!(result.joined);
        assert_eq!(result.scope.guild_id, GUILD);
    }

    #[test]
    fn test_join_notifies_target_watchers_without_channel_metadata() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            None,
            JoinOptions {
                reminder_available: true,
                ..JoinOptions::normal()
            },
        );
        let notification = result.notification.expect("successful join notification");
        assert_eq!(notification.target_id, UserId(1));
        assert_eq!(notification.target_display_name, "Player1");
        assert_eq!(notification.guild_id, GUILD);
        assert_eq!(notification.lobby_kind, LobbyKind::Open);
        assert!(notification.detached);
    }

    #[test]
    fn test_join_cutoff_uses_the_join_results_own_watermark() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            None,
            JoinOptions {
                reminder_available: true,
                joined_at_ns: Some(123_456_789),
                ..JoinOptions::normal()
            },
        );
        assert_eq!(result.notification.unwrap().join_cutoff_ns, 123_456_789);
    }

    #[test]
    fn test_auto_join_notifies_target_watchers_without_channel_metadata() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.auto_join_lobby(
            interaction(1, Some(GUILD)),
            LobbyKind::Open,
            JoinOptions {
                reminder_available: true,
                ..JoinOptions::normal()
            },
        );
        assert!(result.joined);
        assert!(result.warning.is_none());
        let notification = result.notification.unwrap();
        assert_eq!(notification.guild_id, GUILD);
        assert_eq!(notification.lobby_kind, LobbyKind::Open);
        assert!(notification.detached);
    }

    #[test]
    fn test_join_target_dm_does_not_delay_eight_player_rally() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(
            Some(GUILD),
            LobbyKind::Open,
            UserId(99),
            (10..17).map(UserId),
        );
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        service.set_message_ids(scope, metadata());
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            None,
            JoinOptions {
                reminder_available: true,
                ..JoinOptions::normal()
            },
        );
        assert_eq!(result.player_count, 8);
        assert!(
            result
                .graph
                .node(PolicyAction::WatcherDirectMessage)
                .unwrap()
                .detached
        );
        assert!(result.graph.can_overlap(
            PolicyAction::WatcherDirectMessage,
            PolicyAction::RallyOrReady
        ));
        assert!(
            result
                .graph
                .must_precede(PolicyAction::JoinActivity, PolicyAction::RallyOrReady)
        );
    }

    #[test]
    fn test_join_schedules_target_dm_when_post_join_refresh_disappears() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            None,
            JoinOptions {
                reminder_available: true,
                refreshed_lobby_available: false,
                ..JoinOptions::normal()
            },
        );
        assert!(result.joined);
        assert!(!result.refreshed_lobby_available);
        assert!(result.notification.is_some());
        assert!(result.graph.contains(PolicyAction::DisplaySync));
        assert!(result.graph.contains(PolicyAction::JoinActivity));
    }

    #[test]
    fn test_auto_join_schedules_target_dm_when_post_join_refresh_disappears() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.auto_join_lobby(
            interaction(1, Some(GUILD)),
            LobbyKind::Open,
            JoinOptions {
                reminder_available: true,
                refreshed_lobby_available: false,
                ..JoinOptions::normal()
            },
        );
        assert!(result.joined);
        assert!(result.warning.is_none());
        assert!(!result.refreshed_lobby_available);
        assert!(result.notification.is_some());
        assert!(result.graph.contains(PolicyAction::DisplaySync));
        assert!(result.graph.contains(PolicyAction::JoinActivity));
    }

    #[test]
    fn test_failed_join_does_not_notify_target_watchers() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(
            Some(GUILD),
            LobbyKind::Open,
            UserId(99),
            (10..20).map(UserId),
        );
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            None,
            JoinOptions {
                reminder_available: true,
                ..JoinOptions::normal()
            },
        );
        assert_eq!(result.failure, Some(JoinFailure::LobbyFull));
        assert!(result.notification.is_none());
        assert!(!result.graph.contains(PolicyAction::WatcherDirectMessage));
    }

    #[test]
    fn test_already_joined_auto_join_does_not_notify_target_watchers() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), [UserId(1)]);
        let result = service.auto_join_lobby(
            interaction(1, Some(GUILD)),
            LobbyKind::Open,
            JoinOptions {
                reminder_available: true,
                ..JoinOptions::normal()
            },
        );
        assert!(!result.joined);
        assert!(result.warning.is_none());
        assert!(result.notification.is_none());
    }

    #[test]
    fn test_join_fans_out_confirmation_and_isolates_maintenance_failure() {
        let graph = join_publication_graph(false);
        assert!(graph.can_overlap(
            PolicyAction::ConfirmationFollowup,
            PolicyAction::DisplaySync
        ));
        assert!(graph.can_overlap(
            PolicyAction::ConfirmationFollowup,
            PolicyAction::JoinActivity
        ));
        let report = evaluate_publication_wave(
            &graph,
            &WaveOutcomes {
                failed_actions: BTreeSet::from([PolicyAction::DisplaySync]),
            },
        );
        assert!(!report.confirmation_failed);
        assert_eq!(
            report.isolated_maintenance_failures,
            [PolicyAction::DisplaySync]
        );
        assert!(report.rally_or_ready_still_runs);
    }

    #[test]
    fn test_leave_fans_out_confirmation_and_isolates_maintenance_failure() {
        let graph = leave_publication_graph();
        for action in [
            PolicyAction::DisplaySync,
            PolicyAction::ReactionCleanup,
            PolicyAction::LeaveActivity,
        ] {
            assert!(graph.can_overlap(PolicyAction::ConfirmationFollowup, action));
        }
        let report = evaluate_publication_wave(
            &graph,
            &WaveOutcomes {
                failed_actions: BTreeSet::from([PolicyAction::ReactionCleanup]),
            },
        );
        assert!(!report.confirmation_failed);
        assert_eq!(
            report.isolated_maintenance_failures,
            [PolicyAction::ReactionCleanup]
        );
    }

    #[test]
    fn test_auto_join_runs_display_and_thread_publication_concurrently() {
        let graph = join_publication_graph(false);
        assert!(graph.can_overlap(PolicyAction::DisplaySync, PolicyAction::JoinActivity));
        let report = evaluate_publication_wave(
            &graph,
            &WaveOutcomes {
                failed_actions: BTreeSet::from([PolicyAction::DisplaySync]),
            },
        );
        assert!(report.rally_or_ready_still_runs);
    }

    #[test]
    fn test_auto_join_activity_finishes_before_rally_thread_send() {
        let graph = join_publication_graph(false);
        assert!(graph.must_precede(PolicyAction::JoinActivity, PolicyAction::RallyOrReady));
        assert!(!graph.can_overlap(PolicyAction::JoinActivity, PolicyAction::RallyOrReady));
    }

    #[test]
    fn test_sync_lobby_displays_uses_guild_id() {
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        let mutation = plan_sync_lobby_display(scope, &metadata()).unwrap();
        assert_eq!(mutation.guild_id, GUILD);
        assert_eq!(mutation.lobby_kind, LobbyKind::Open);
        assert_eq!(mutation.access, MessageAccess::Partial);
        assert_eq!(mutation.message_id, MessageId(789));
    }

    #[test]
    fn test_sync_lobby_display_serializes_with_pool_refresh() {
        let serializers = LobbyMessageSerializers::default();
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        let edits = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();

        let first_serializers = serializers.clone();
        let first_edits = Arc::clone(&edits);
        let first = thread::spawn(move || {
            first_serializers.serialize(scope, || {
                first_entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                first_edits.lock().unwrap().push("old");
            });
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let second_serializers = serializers.clone();
        let second_edits = Arc::clone(&edits);
        let second = thread::spawn(move || {
            second_serializers.serialize(scope, || {
                second_entered_tx.send(()).unwrap();
                second_edits.lock().unwrap().push("new");
            });
        });
        assert!(
            second_entered_rx
                .recv_timeout(Duration::from_millis(30))
                .is_err()
        );
        release_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(*edits.lock().unwrap(), ["old", "new"]);
    }

    #[test]
    fn test_remove_lobby_reaction_uses_partial_message_without_fetch() {
        let mutation =
            plan_remove_lobby_reaction(LobbyScope::new(GUILD, LobbyKind::Open), &metadata())
                .unwrap();
        assert_eq!(mutation.access, MessageAccess::Partial);
        assert_eq!(mutation.kind, MessageMutationKind::RemoveSwordReaction);
        assert_eq!(mutation.message_id, MessageId(789));
    }

    #[test]
    fn test_thread_embed_update_uses_partial_message_without_fetch() {
        let mutation =
            plan_update_thread_embed(LobbyScope::new(GUILD, LobbyKind::Open), &metadata()).unwrap();
        assert_eq!(mutation.access, MessageAccess::Partial);
        assert_eq!(mutation.kind, MessageMutationKind::EditThreadEmbed);
        assert_eq!(mutation.channel_id, ChannelId(999));
        assert_eq!(mutation.message_id, MessageId(789));
    }

    #[test]
    fn test_existing_lobby_auto_join_edits_starter_once() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        service.set_message_ids(scope, metadata());
        let result = service.create_or_view_lobby(interaction(1, Some(GUILD)), None);
        assert!(!result.created && result.joined);
        assert_eq!(
            result.validation_mutation.unwrap().access,
            MessageAccess::Fetch
        );
        assert_eq!(result.refresh_mutations.len(), 1);
        assert_eq!(result.refresh_mutations[0].access, MessageAccess::Partial);
        assert!(service.lobby(scope).unwrap().contains(UserId(1)));
    }

    #[test]
    fn test_existing_member_keeps_single_repair_refresh() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), [UserId(1)]);
        service.set_message_ids(scope, metadata());
        let result = service.create_or_view_lobby(interaction(1, Some(GUILD)), None);
        assert!(!result.created && !result.joined);
        assert_eq!(
            result.validation_mutation.unwrap().access,
            MessageAccess::Fetch
        );
        assert_eq!(result.refresh_mutations.len(), 1);
        assert_eq!(result.refresh_mutations[0].access, MessageAccess::Partial);
    }

    #[test]
    fn test_update_thread_embed_uses_guild_id() {
        let scope = LobbyScope::new(GUILD, LobbyKind::Open);
        let no_thread = LobbyMessageIds::default();
        assert_eq!(plan_update_thread_embed(scope, &no_thread), None);
        let mutation = plan_update_thread_embed(scope, &metadata()).unwrap();
        assert_eq!(mutation.guild_id, GUILD);
        assert_eq!(mutation.lobby_kind, LobbyKind::Open);
    }

    #[test]
    fn test_lobby_command_guild_id_order() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        let result = service.create_or_view_lobby(interaction(1, Some(GUILD)), None);
        assert_eq!(result.lookup_trace[0].operation, "get_player");
        assert_eq!(result.lookup_trace[0].guild_id, GUILD);
    }

    #[test]
    fn test_join_command_guild_id_order() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(99), []);
        let result = service.join_lobby(
            interaction(1, Some(GUILD)),
            Some(LobbyKind::Open),
            JoinOptions::normal(),
        );
        assert_eq!(result.lookup_trace[0].operation, "get_player");
        assert_eq!(result.lookup_trace[0].guild_id, GUILD);
        assert!(
            result
                .lookup_trace
                .iter()
                .all(|lookup| lookup.guild_id == GUILD)
        );
    }

    #[test]
    fn guild_state_isolated_for_same_signed_user_id() {
        let service = LobbyCommandService::default();
        let user = UserId(i64::MIN + 9);
        service.register_player(Some(GUILD), user, RegisteredPlayer::eligible("One"));
        service.register_player(Some(OTHER_GUILD), user, RegisteredPlayer::eligible("Two"));
        service.seed_lobby(Some(GUILD), LobbyKind::Open, UserId(1), []);
        service.seed_lobby(Some(OTHER_GUILD), LobbyKind::Open, UserId(2), []);
        assert!(
            service
                .join_lobby(
                    interaction(user.0, Some(GUILD)),
                    None,
                    JoinOptions::normal(),
                )
                .joined
        );
        assert!(
            service
                .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
                .unwrap()
                .contains(user)
        );
        assert!(
            !service
                .lobby(LobbyScope::new(OTHER_GUILD, LobbyKind::Open))
                .unwrap()
                .contains(user)
        );
    }

    #[test]
    fn concurrent_lobby_creation_is_atomic_per_guild_scope() {
        let service = LobbyCommandService::default();
        register(&service, 1, Some(GUILD));
        register(&service, 2, Some(GUILD));
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for user_id in [1, 2] {
            let service = service.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                service.create_or_view_lobby(interaction(user_id, Some(GUILD)), None)
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.created).count(), 1);
        let lobby = service
            .lobby(LobbyScope::new(GUILD, LobbyKind::Open))
            .unwrap();
        assert_eq!(lobby.players, BTreeSet::from([UserId(1), UserId(2)]));
    }

    #[test]
    fn message_serializers_are_independent_across_guild_scopes() {
        let serializers = LobbyMessageSerializers::default();
        let first_scope = LobbyScope::new(GUILD, LobbyKind::Open);
        let second_scope = LobbyScope::new(OTHER_GUILD, LobbyKind::Open);
        let barrier = Arc::new(Barrier::new(2));
        let first = {
            let serializers = serializers.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                serializers.serialize(first_scope, || barrier.wait());
            })
        };
        let second = {
            let serializers = serializers.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                serializers.serialize(second_scope, || barrier.wait());
            })
        };
        first.join().unwrap();
        second.join().unwrap();
    }
}
