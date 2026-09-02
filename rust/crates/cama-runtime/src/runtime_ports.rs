//! Shared cross-provider port definitions.
//!
//! These traits and their parameter/result types are the narrow seams the
//! runtime providers expose to one another. Hoisting the definitions into the
//! Serenity-independent core lets each provider implement or consume a seam
//! without importing another provider's module; the concrete implementations
//! stay with the providers that own the live state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::embeds::LobbyKind;
use cama_app::match_discovery::DiscoveryResult;
use cama_app::match_recording::{GamesMilestone, RivalryRecord, WinStreakRecord};

use crate::registration::{InteractionResponder, InteractionResponse};

/// A request to extend one explicitly selected pending match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminExtendBettingRequest {
    pub guild_id: i64,
    pub actor_id: i64,
    pub minutes: i64,
    pub pending_match_id: i64,
}

/// The live match runtime owns persistence, task replacement, and all rendered
/// copies; this result contains only the copy needed by `/admin extendbetting`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminExtendBettingResult {
    pub pending_match_id: i64,
    pub old_bet_lock_until: i64,
    pub new_bet_lock_until: i64,
    pub lobby_label: String,
    pub jump_url: Option<String>,
    pub refreshed_routes: usize,
    pub refresh_failures: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminSeedHeroGridRequest {
    pub guild_id: i64,
    pub actor_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminSeedHeroGridResult {
    pub players_seeded: usize,
    pub matches_created: usize,
}

/// Additive control surface implemented by the production match provider.
#[async_trait]
pub trait AdminMatchControl: Send + Sync {
    async fn extend_betting(
        &self,
        request: AdminExtendBettingRequest,
    ) -> Result<AdminExtendBettingResult, String>;

    async fn seed_hero_grid(
        &self,
        request: AdminSeedHeroGridRequest,
    ) -> Result<AdminSeedHeroGridResult, String>;

    async fn backfill_derived_roles(
        &self,
        guild_id: i64,
    ) -> Result<AdminRoleBackfillResult, String>;
}

/// Result of a derived-role backfill, paired with the coverage that exists
/// afterwards so the run can be confirmed rather than taken on trust.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AdminRoleBackfillResult {
    pub matches_scanned: u64,
    pub teams_derived: u64,
    pub gold_samples_written: u64,
    pub last_hits_samples_written: u64,
    pub unreadable_payloads: u64,
    pub unparsed_teams: u64,
    pub ambiguous_lanes: u64,
    pub incomplete_teams: u64,
    pub tied_farm_priority: u64,
    pub tied_farm_and_ward_priority: u64,
    pub participants: i64,
    pub with_derived_role: i64,
    pub with_gold_at_10: i64,
    pub with_last_hits_at_10: i64,
    pub player_roles_above_minimum_sample: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrectionWinRewardRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub discord_id: i64,
    pub gross_reward: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrectionWinRewardResult {
    /// The exact participant snapshot used for a later result reversal:
    /// service `net + garnished`, matching Python's durable correction input.
    pub snapshot_balance_delta: i64,
}

/// Production reward policy invoked one corrected winner at a time. The
/// implementation atomically credits and writes the match-win ledger marker,
/// then applies the same garnishment, bankruptcy, vanity-tax, Sanctuary,
/// Communion, and Blood Pact ordering as ordinary match recording.
pub trait CorrectionWinRewardControl: Send + Sync {
    fn award_match_win_bonus(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<CorrectionWinRewardResult, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminFakeLobbyRequest {
    pub interaction_id: u64,
    pub guild_id: i64,
    pub channel_id: u64,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminFakeLobbyResult {
    pub users_added: usize,
    pub user_names: Vec<String>,
    pub already_at_threshold: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AdminLobbyEjectionResult {
    pub applicable_lobbies: Vec<String>,
    pub evicted_lobbies: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminLobbyScope {
    All,
    Open,
    Lowskill,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminLobbyEjectionRequest {
    pub guild_id: i64,
    pub player_id: i64,
    pub player_display_name: String,
    pub scope: AdminLobbyScope,
}

/// Additive control surface implemented by the production lobby provider.
#[async_trait]
pub trait AdminLobbyControl: Send + Sync {
    async fn add_fake_users(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String>;

    async fn fill_lobby(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String>;

    /// Eject the player only from queued lobbies covered by `scope`. A lobby
    /// already starting a match must be left untouched.
    async fn eject_suspended_player(
        &self,
        request: AdminLobbyEjectionRequest,
    ) -> Result<AdminLobbyEjectionResult, String>;
}

/// Durable-join projection delivered after the lobby display and ordered
/// thread activity have been published.  Consumers can claim one-shot watches
/// against `joined_at_ns` without reaching into lobby-runtime internals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmedLobbyJoin {
    pub guild_id: u64,
    pub player_id: u64,
    pub player_name: String,
    pub player_display_name: String,
    pub lobby_kind: LobbyKind,
    pub joined_at_ns: i64,
    pub player_ids: BTreeSet<u64>,
    pub ready_threshold: usize,
    pub lobby_channel_id: Option<u64>,
    pub lobby_message_id: Option<u64>,
    pub origin_channel_id: Option<u64>,
    pub thread_id: Option<u64>,
}

/// Projection of the auxiliary jopacoin reaction. The registration provider
/// owns the shared Neon service, so this keeps the lobby provider from
/// constructing a second cooldown/event ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyGambaSpectator {
    pub guild_id: u64,
    pub player_id: u64,
    pub player_display_name: String,
    pub channel_id: u64,
}

#[async_trait]
pub trait LobbyJoinObserver: Send + Sync {
    async fn confirmed_lobby_join(&self, event: ConfirmedLobbyJoin) -> Result<(), String>;

    /// Python invokes this only after the successful `/join` follow-up. Lobby
    /// creation auto-join and raw sword joins deliberately never call it.
    async fn explicit_lobby_join_neon(&self, _event: ConfirmedLobbyJoin) -> Result<(), String> {
        Ok(())
    }

    async fn gamba_spectator(&self, _event: LobbyGambaSpectator) -> Result<(), String> {
        Ok(())
    }

    /// Reset any generation-scoped join side effects after `/resetlobby` has
    /// durably cleared the lobby. Python clears both rally and ready cooldowns
    /// at this boundary so a newly-created generation is never suppressed by
    /// the lobby it replaced.
    async fn lobby_reset(&self, _guild_id: u64, _lobby_kind: LobbyKind) -> Result<(), String> {
        Ok(())
    }
}

/// Stable snapshot consumed while one live lobby operation lock is held.
///
/// `confirmed_player_ids` comes from `ReadycheckService`, not the legacy
/// readycheck fields retained in `LobbyService`. This distinction matters for
/// the ten-confirmation conditional-roster behavior of Python `/shuffle`.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchLobbySnapshot {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub created_by: Option<i64>,
    pub player_ids: Vec<i64>,
    pub player_join_times: BTreeMap<i64, f64>,
    pub confirmed_player_ids: Option<BTreeSet<i64>>,
    pub ready_threshold: usize,
    pub lobby_channel_id: Option<u64>,
    pub lobby_message_id: Option<u64>,
    pub origin_channel_id: Option<u64>,
    pub thread_id: Option<u64>,
}

/// Typed Neon seam for the three Draft events owned by the Python command.
/// The live composition layer can adapt the existing Neon service without
/// making this provider depend on Neon implementation details.  Returning
/// `None` is a normal no-event result; an error is logged and never turns a
/// durable draft failure-prone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftNeonResult {
    pub text_block: String,
    pub attachment: Option<crate::registration::InteractionAttachment>,
}

#[async_trait]
pub trait DraftNeonObserver: Send + Sync {
    async fn on_draft_coinflip(
        &self,
        guild_id: i64,
        winner_id: i64,
        loser_id: i64,
    ) -> Result<Option<DraftNeonResult>, String>;

    async fn on_captain_symmetry(
        &self,
        guild_id: i64,
        radiant_captain_id: i64,
        dire_captain_id: i64,
        rating_diff: i64,
    ) -> Result<Option<DraftNeonResult>, String>;

    async fn on_bomb_pot(
        &self,
        guild_id: i64,
        pool_amount: i64,
        contributor_count: i64,
    ) -> Result<Option<DraftNeonResult>, String>;
}

/// Durable facts selected after Match commits its rating history. The
/// composition root adapts this request to the live JOPA-T/Neon delivery
/// service; Match owns the database read and Python-compatible winner policy.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchPostMatchDebriefRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub winner_id: Option<i64>,
    pub loser_id: Option<i64>,
    pub payout: i64,
    pub loss: i64,
    pub leverage: i64,
    pub rating_change: Option<f64>,
    pub expected_win_prob: Option<f64>,
}

/// Committed Match-side Neon candidates. Settlement owns the degen/balance
/// hooks; this adapter receives the remaining candidates so it can preserve
/// Python's games-milestone, streak-record, rivalry, then fallback ordering
/// and claim/replay the whole event atomically from the delivery boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchEasterEggRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub games_milestones: Vec<GamesMilestone>,
    pub win_streak_records: Vec<WinStreakRecord>,
    pub rivalries_detected: Vec<RivalryRecord>,
}

/// Settlement facts handed to Betting's Neon observer after Match has
/// committed the immutable bet settlement rows.  Match owns the source of
/// truth and this DTO deliberately contains no provider-local state, making
/// it safe to replay from READY recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchBetSettlementParticipant {
    pub discord_id: i64,
    pub amount: i64,
    pub leverage: i64,
    pub balance_after: i64,
    pub payout: i64,
    pub won: bool,
    pub refunded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchBetSettlementRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub participants: Vec<MatchBetSettlementParticipant>,
}

/// Narrow live seam for post-match JOPA-T/Neon presentation. Implementors
/// must make delivery idempotent by `match_id`; READY recovery may replay a
/// committed Match whose Discord presentation was interrupted.
#[async_trait]
pub trait MatchPostMatchDebriefPort: Send + Sync {
    async fn on_bet_settlement(&self, _request: MatchBetSettlementRequest) -> Result<(), String> {
        Ok(())
    }

    async fn on_match_easter_eggs(&self, _request: MatchEasterEggRequest) -> Result<(), String> {
        Ok(())
    }

    async fn on_post_match_debrief(
        &self,
        request: MatchPostMatchDebriefRequest,
    ) -> Result<(), String>;
}

/// Result of Python-compatible post-record OpenDota discovery.
#[derive(Clone, Debug, PartialEq)]
pub enum RecordedMatchDiscoveryOutcome {
    Disabled,
    Discovered {
        result: DiscoveryResult,
        response: InteractionResponse,
    },
    Exhausted {
        last_result: Option<DiscoveryResult>,
    },
    Stopped(DiscoveryResult),
}

/// Narrow handle shared with the match runtime. It deliberately owns no
/// service state: the provider's existing handler supplies the one live
/// discovery cache, enrichment pipeline, and OpenSkill replay policy.
#[async_trait]
pub trait RecordedMatchDiscovery: Send + Sync {
    async fn discover_recorded_match(
        &self,
        guild_id: i64,
        match_id: i64,
    ) -> Result<RecordedMatchDiscoveryOutcome, String>;
}

/// Runtime adapter for the three rare bonus surfaces owned by the migrated
/// application layer (wheel, package deal, and trivia). The provider owns the
/// post-UI trigger and durable action claim; the adapter owns the existing
/// interactive Discord/session implementations.
#[async_trait]
pub trait DigBonusDispatchPort: Send + Sync {
    async fn dispatch_bonus(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        bonus: cama_app::dig_bonus_events::DigBonus,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String>;

    async fn report_partial_failure(
        &self,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .followup(
                InteractionResponse::message(cama_app::dig_bonus_events::PARTIAL_BONUS_FAILURE)
                    .ephemeral(),
            )
            .await
            .map_err(|error| error.to_string())
    }
}
