//! Discord-neutral betting reminder and wager-copy policy.
//!
//! The live Python command remains the Discord/runtime authority. This module
//! keeps the reminder strings, lopsided-pool threshold, tightly scoped mention
//! policy, partial-message thread routing, and concurrent copy refresh behavior
//! executable without a gateway.

use std::collections::BTreeSet;
use std::fmt;

use cama_domain::formatting::JOPACOIN_EMOTE;

use crate::embeds::LobbyKind;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const BET_UNDERDOG_PING_RATIO: i64 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BettingMode {
    Pool,
    House,
}

impl BettingMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Pool => "Pool",
            Self::House => "House (1:1)",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReminderType {
    Warning,
    LastCall,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PotTotals {
    pub radiant: i64,
    pub dire: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBettingState {
    pub betting_mode: BettingMode,
    pub bet_lock_until: Option<i64>,
    pub pending_match_id: Option<i64>,
    pub lobby_kind: LobbyKind,
    pub bet_seed_radiant: i64,
    pub bet_seed_dire: i64,
    pub bet_seed_bonus: i64,
    pub shuffle_channel_id: Option<i64>,
    pub shuffle_message_id: Option<i64>,
    pub command_shuffle_channel_id: Option<i64>,
    pub command_shuffle_message_id: Option<i64>,
    pub thread_shuffle_thread_id: Option<i64>,
    pub thread_shuffle_message_id: Option<i64>,
    pub radiant_team_ids: Vec<DiscordId>,
    pub dire_team_ids: Vec<DiscordId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShuffleMessageInfo {
    pub channel_id: Option<i64>,
    pub thread_message_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub origin_channel_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WagerField {
    pub name: String,
    pub value: String,
}

/// Format the wager field using the same pool/house and seed semantics as the
/// Python presentation helper.
#[must_use]
pub fn format_betting_display(
    totals: PotTotals,
    mode: BettingMode,
    lock_until: Option<i64>,
    locked: bool,
    seed_radiant: i64,
    seed_dire: i64,
    seed_bonus: i64,
) -> WagerField {
    let lock_text = if locked {
        Some("🔒 Betting closed".to_owned())
    } else {
        lock_until.map(|timestamp| format!("Closes <t:{timestamp}:R>"))
    };

    let mut value = match mode {
        BettingMode::Pool => {
            let total_pool = totals.radiant.saturating_add(totals.dire);
            let radiant_odds = multiplier_text(
                total_pool.saturating_add(seed_dire),
                totals.radiant,
                total_pool,
            );
            let dire_odds = multiplier_text(
                total_pool.saturating_add(seed_radiant),
                totals.dire,
                total_pool,
            );
            let mut text = format!(
                "Radiant: {} {JOPACOIN_EMOTE} {radiant_odds} | Dire: {} {JOPACOIN_EMOTE} {dire_odds}",
                totals.radiant, totals.dire
            );
            if seed_radiant != 0 || seed_dire != 0 {
                text.push_str(&format!(
                    "\nSeed: Radiant {seed_radiant} {JOPACOIN_EMOTE} | Dire {seed_dire} {JOPACOIN_EMOTE}"
                ));
            }
            text
        }
        BettingMode::House => {
            let mut text = format!(
                "Radiant: {} {JOPACOIN_EMOTE} | Dire: {} {JOPACOIN_EMOTE}",
                totals.radiant, totals.dire
            );
            if seed_bonus != 0 {
                text.push_str(&format!("\nWinner bonus: {seed_bonus} {JOPACOIN_EMOTE}"));
            }
            text
        }
    };
    if let Some(lock_text) = lock_text {
        value.push('\n');
        value.push_str(&lock_text);
    }

    WagerField {
        name: match mode {
            BettingMode::Pool => "💰 Pool Betting",
            BettingMode::House => "💰 House Betting (1:1)",
        }
        .to_owned(),
        value,
    }
}

fn multiplier_text(numerator: i64, side_total: i64, total_pool: i64) -> String {
    if total_pool == 0 || side_total <= 0 {
        "(—)".to_owned()
    } else {
        format!("({:.2}x)", numerator as f64 / side_total as f64)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MessageTarget {
    pub channel_id: i64,
    pub message_id: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WagerRefreshReport {
    pub attempted: Vec<MessageTarget>,
    pub failed: Vec<MessageTarget>,
}

pub trait WagerCopyPort: Sync {
    fn load_pending_state(
        &self,
        guild_id: Option<GuildId>,
        pending_match_id: Option<i64>,
    ) -> Option<PendingBettingState>;

    fn pot_totals(
        &self,
        guild_id: Option<GuildId>,
        pending_state: &PendingBettingState,
    ) -> PotTotals;

    fn update_wager_field(&self, target: MessageTarget, field: &WagerField) -> Result<(), String>;
}

pub struct WagerRefreshRequest<'a> {
    pub guild_id: Option<GuildId>,
    pub pending_match_id: Option<i64>,
    pub locked: bool,
    pub pending_state: Option<&'a PendingBettingState>,
}

/// Refresh every distinct shuffle copy in parallel. A failed copy is reported
/// but never prevents another target from being attempted.
#[must_use]
pub fn refresh_wager_copies(
    port: &dyn WagerCopyPort,
    request: WagerRefreshRequest<'_>,
) -> Option<WagerRefreshReport> {
    let pending_state = request
        .pending_state
        .cloned()
        .or_else(|| port.load_pending_state(request.guild_id, request.pending_match_id))?;
    let totals = port.pot_totals(request.guild_id, &pending_state);
    let field = format_betting_display(
        totals,
        pending_state.betting_mode,
        pending_state.bet_lock_until,
        request.locked,
        pending_state.bet_seed_radiant,
        pending_state.bet_seed_dire,
        pending_state.bet_seed_bonus,
    );
    let attempted = wager_targets(&pending_state);
    let failures = std::thread::scope(|scope| {
        attempted
            .iter()
            .copied()
            .map(|target| {
                let field = &field;
                scope.spawn(move || port.update_wager_field(target, field).err().map(|_| target))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect::<Vec<_>>()
    });
    Some(WagerRefreshReport {
        attempted,
        failed: failures,
    })
}

#[must_use]
pub fn wager_targets(state: &PendingBettingState) -> Vec<MessageTarget> {
    let mut seen = BTreeSet::new();
    [
        (state.shuffle_channel_id, state.shuffle_message_id),
        (
            state.command_shuffle_channel_id,
            state.command_shuffle_message_id,
        ),
        (
            state.thread_shuffle_thread_id,
            state.thread_shuffle_message_id,
        ),
    ]
    .into_iter()
    .filter_map(|(channel_id, message_id)| {
        let target = MessageTarget {
            channel_id: channel_id.filter(|id| *id != 0)?,
            message_id: message_id.filter(|id| *id != 0)?,
        };
        seen.insert(target).then_some(target)
    })
    .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopBettor {
    pub discord_id: DiscordId,
    pub team_bet_on: Option<String>,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlavorKind {
    Warning,
    LastCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorRequest {
    pub kind: FlavorKind,
    pub guild_id: Option<GuildId>,
    pub standings: String,
    pub seconds_left: i64,
    pub leader_team: Option<String>,
    pub leader_amount: Option<i64>,
    pub leader_discord_id: Option<DiscordId>,
    pub underdog_side: Option<UnderdogSide>,
}

pub trait BettingFlavorPort: Send + Sync {
    fn generate(&self, request: &FlavorRequest) -> Result<Option<String>, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnderdogSide {
    Radiant,
    Dire,
}

impl UnderdogSide {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MentionUsers {
    Disabled,
    Scoped(Vec<DiscordId>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedMentions {
    pub everyone: bool,
    pub roles: bool,
    pub replied_user: bool,
    pub users: MentionUsers,
}

impl AllowedMentions {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            everyone: false,
            roles: false,
            replied_user: false,
            users: MentionUsers::Disabled,
        }
    }

    #[must_use]
    pub fn scoped(users: Vec<DiscordId>) -> Self {
        Self {
            everyone: false,
            roles: false,
            replied_user: false,
            users: MentionUsers::Scoped(users),
        }
    }

    /// Discord's broad parse list is deliberately empty for every reminder.
    #[must_use]
    pub const fn parse_everyone_or_roles(&self) -> bool {
        self.everyone || self.roles
    }
}

impl Default for AllowedMentions {
    fn default() -> Self {
        Self::none()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReminderDestination {
    Channel {
        channel_id: i64,
    },
    ThreadReply {
        thread_id: i64,
        parent_message_id: i64,
        use_partial_message: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReminderDelivery {
    pub destination: ReminderDestination,
    pub content: String,
    pub allowed_mentions: AllowedMentions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockedWagerRefresh {
    pub pending_match_id: Option<i64>,
    pub locked: bool,
    pub reuse_loaded_state: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReminderPlan {
    pub deliveries: Vec<ReminderDelivery>,
    pub locked_wager_refresh: Option<LockedWagerRefresh>,
    pub underdog_side: Option<UnderdogSide>,
}

pub struct ReminderRequest<'a> {
    pub guild_id: Option<GuildId>,
    pub reminder_type: ReminderType,
    pub lock_until: Option<i64>,
    pub pending_match_id: Option<i64>,
    pub is_final_warning: bool,
    pub now: i64,
    pub pending_state: &'a PendingBettingState,
    pub message_info: &'a ShuffleMessageInfo,
    pub totals: PotTotals,
    pub top_bettor: Option<&'a TopBettor>,
}

/// Build every channel/thread delivery and any closed-state refresh directive.
/// Missing or failed flavor is fail-soft and never suppresses an underdog ping.
#[must_use]
pub fn build_reminder(
    request: ReminderRequest<'_>,
    flavor_port: Option<&dyn BettingFlavorPort>,
) -> Option<ReminderPlan> {
    if matches!(
        request.reminder_type,
        ReminderType::Warning | ReminderType::LastCall
    ) && request.lock_until.is_none()
    {
        return None;
    }

    let totals_field = format_betting_display(
        request.totals,
        request.pending_state.betting_mode,
        None,
        false,
        request.pending_state.bet_seed_radiant,
        request.pending_state.bet_seed_dire,
        request.pending_state.bet_seed_bonus,
    );
    let mut underdog_side = None;
    let mut ping_ids = Vec::new();
    let mut content = match request.reminder_type {
        ReminderType::Warning => format!(
            "⏰ **Betting closes <t:{}:R>** — {}",
            request.lock_until?, totals_field.value
        ),
        ReminderType::LastCall => format!(
            "🎲 **Last call — betting closes <t:{}:R>** — {}",
            request.lock_until?, totals_field.value
        ),
        ReminderType::Closed => format!(
            "🔒 **Betting closed!** Final {} pool — {}",
            request.pending_state.betting_mode.label(),
            totals_field.value
        ),
    };

    let flavor_kind = match request.reminder_type {
        ReminderType::Warning if request.is_final_warning => {
            let status = underdog_status(request.pending_state, request.totals);
            underdog_side = status.0;
            ping_ids = status.1;
            Some(FlavorKind::Warning)
        }
        ReminderType::LastCall => Some(FlavorKind::LastCall),
        ReminderType::Warning | ReminderType::Closed => None,
    };
    if let (Some(kind), Some(port)) = (flavor_kind, flavor_port) {
        let top = request.top_bettor;
        let default_seconds = match kind {
            FlavorKind::Warning => 300,
            FlavorKind::LastCall => 60,
        };
        let flavor_request = FlavorRequest {
            kind,
            guild_id: request.guild_id,
            standings: totals_field.value.clone(),
            seconds_left: request.lock_until.map_or(default_seconds, |lock| {
                lock.saturating_sub(request.now).max(0)
            }),
            leader_team: top.and_then(|top| top.team_bet_on.clone()),
            leader_amount: top.map(|top| top.amount),
            leader_discord_id: top.map(|top| top.discord_id),
            underdog_side,
        };
        if let Ok(Some(flavor)) = port.generate(&flavor_request)
            && !flavor.is_empty()
        {
            content.push_str("\n\n💬 ");
            content.push_str(&flavor);
        }
    }
    if !ping_ids.is_empty() {
        content.push('\n');
        content.push_str(
            &ping_ids
                .iter()
                .map(|id| format!("<@{id}>"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }

    let resolved_match_id = request
        .pending_state
        .pending_match_id
        .or(request.pending_match_id);
    let mut identity = request.pending_state.lobby_kind.label().to_owned();
    if let Some(match_id) = resolved_match_id {
        identity.push_str(&format!(" / Match #{match_id}"));
    }
    content = format!("**{identity}**\n{content}");

    let allowed_mentions = if ping_ids.is_empty() {
        AllowedMentions::none()
    } else {
        AllowedMentions::scoped(ping_ids)
    };
    let mut deliveries = Vec::new();
    let channel_id = request
        .message_info
        .origin_channel_id
        .filter(|id| *id != 0)
        .or_else(|| request.message_info.channel_id.filter(|id| *id != 0));
    if let Some(channel_id) = channel_id {
        deliveries.push(ReminderDelivery {
            destination: ReminderDestination::Channel { channel_id },
            content: content.clone(),
            allowed_mentions: allowed_mentions.clone(),
        });
    }
    if let (Some(thread_id), Some(parent_message_id)) = (
        request.message_info.thread_id.filter(|id| *id != 0),
        request.message_info.thread_message_id.filter(|id| *id != 0),
    ) {
        deliveries.push(ReminderDelivery {
            destination: ReminderDestination::ThreadReply {
                thread_id,
                parent_message_id,
                use_partial_message: true,
            },
            content,
            allowed_mentions,
        });
    }

    Some(ReminderPlan {
        deliveries,
        locked_wager_refresh: (request.reminder_type == ReminderType::Closed).then_some(
            LockedWagerRefresh {
                pending_match_id: request.pending_match_id,
                locked: true,
                reuse_loaded_state: true,
            },
        ),
        underdog_side,
    })
}

#[must_use]
pub fn underdog_status(
    pending_state: &PendingBettingState,
    totals: PotTotals,
) -> (Option<UnderdogSide>, Vec<DiscordId>) {
    let radiant = totals.radiant.max(0);
    let dire = totals.dire.max(0);
    let high = radiant.max(dire);
    let low = radiant.min(dire);
    if high <= 0
        || (low > 0 && i128::from(high) < i128::from(BET_UNDERDOG_PING_RATIO) * i128::from(low))
    {
        return (None, Vec::new());
    }
    let side = if radiant <= dire {
        UnderdogSide::Radiant
    } else {
        UnderdogSide::Dire
    };
    let ids = match side {
        UnderdogSide::Radiant => &pending_state.radiant_team_ids,
        UnderdogSide::Dire => &pending_state.dire_team_ids,
    }
    .iter()
    .copied()
    .filter(|id| *id > 0)
    .collect();
    (Some(side), ids)
}

impl fmt::Display for UnderdogSide {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "betting_reminder_messaging/tests.rs"]
mod tests;
