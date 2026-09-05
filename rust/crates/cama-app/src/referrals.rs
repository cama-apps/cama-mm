//! Discord-neutral referral service, reward-summary formatting, and `/refer` transport policy.
//!
//! The live Python command and match service remain authoritative. This module makes the bounded
//! application decisions explicit: repository error classification, referral component merging,
//! deterministic JC summaries, private acknowledgement before public onboarding, and restrictive
//! mention policy.

use std::collections::{BTreeMap, BTreeSet};

use cama_db::referrals::{ReferralRepository, ReferralRepositoryError, ReferralReward};
use thiserror::Error;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";
pub const REFERRAL_CREATED_MESSAGE: &str = "✅ Referral created.";

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReferralCreateFailure {
    #[error("Self-referrals are not allowed.")]
    SelfReferral,
    #[error("Referrer must be registered in this guild.")]
    ReferrerNotRegistered,
    #[error("That player has already played a game in this guild.")]
    ReferredAlreadyPlayed,
    #[error("That player already has a referral.")]
    AlreadyReferred,
    #[error("referral persistence failed: {0}")]
    Backend(String),
}

impl From<ReferralRepositoryError> for ReferralCreateFailure {
    fn from(error: ReferralRepositoryError) -> Self {
        match error {
            ReferralRepositoryError::SelfReferral => Self::SelfReferral,
            ReferralRepositoryError::ReferrerNotRegistered => Self::ReferrerNotRegistered,
            ReferralRepositoryError::ReferredAlreadyPlayed => Self::ReferredAlreadyPlayed,
            ReferralRepositoryError::AlreadyReferred => Self::AlreadyReferred,
            other => Self::Backend(other.to_string()),
        }
    }
}

pub trait ReferralCreatePort {
    fn create_referral(
        &self,
        referrer_id: DiscordId,
        referred_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<(), ReferralCreateFailure>;
}

impl ReferralCreatePort for ReferralRepository {
    fn create_referral(
        &self,
        referrer_id: DiscordId,
        referred_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<(), ReferralCreateFailure> {
        self.create_referral(referrer_id, referred_id, Some(guild_id))
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug)]
pub struct PlayerReferralService<P> {
    repository: P,
}

impl<P> PlayerReferralService<P> {
    #[must_use]
    pub const fn new(repository: P) -> Self {
        Self { repository }
    }

    #[must_use]
    pub const fn repository(&self) -> &P {
        &self.repository
    }
}

impl<P> ReferralCreatePort for PlayerReferralService<P>
where
    P: ReferralCreatePort,
{
    fn create_referral(
        &self,
        referrer_id: DiscordId,
        referred_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<(), ReferralCreateFailure> {
        self.repository
            .create_referral(referrer_id, referred_id, guild_id)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JcComponents {
    pub payout: Option<i64>,
    pub bet: Option<i64>,
    pub streak: Option<i64>,
    pub referral: Option<i64>,
}

impl JcComponents {
    #[must_use]
    pub fn total(self) -> i64 {
        [self.payout, self.bet, self.streak, self.referral]
            .into_iter()
            .flatten()
            .sum()
    }
}

/// Merge referral components into a post-match JC map without replacing payout/bet/streak data.
pub fn merge_referral_rewards(
    changes: &mut BTreeMap<DiscordId, JcComponents>,
    rewards: &[ReferralReward],
) {
    for reward in rewards {
        for (beneficiary_id, net) in [
            (reward.referred_id, reward.referred_net),
            (reward.referrer_id, reward.referrer_net),
        ] {
            let components = changes.entry(beneficiary_id).or_default();
            components.referral = Some(components.referral.unwrap_or_default().saturating_add(net));
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JcSummaryInput {
    pub winning_player_ids: Vec<DiscordId>,
    pub losing_player_ids: Vec<DiscordId>,
    pub excluded_player_ids: Vec<DiscordId>,
    /// Ordered like Python's insertion-ordered `dict` for stable equal-total output.
    pub changes: Vec<(DiscordId, JcComponents)>,
}

#[must_use]
pub fn format_jc_changes(input: &JcSummaryInput) -> String {
    if input.changes.is_empty() {
        return String::new();
    }

    let winning = input
        .winning_player_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let losing = input
        .losing_player_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let excluded = input
        .excluded_player_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let participants = winning
        .union(&losing)
        .copied()
        .chain(excluded.iter().copied())
        .collect::<BTreeSet<_>>();

    let mut seen = BTreeSet::new();
    let mut ordered_ids = Vec::new();
    for discord_id in input
        .winning_player_ids
        .iter()
        .chain(&input.losing_player_ids)
        .chain(&input.excluded_player_ids)
        .copied()
        .chain(input.changes.iter().map(|(discord_id, _)| *discord_id))
    {
        if seen.insert(discord_id) {
            ordered_ids.push(discord_id);
        }
    }

    let components_by_id = input.changes.iter().copied().collect::<BTreeMap<_, _>>();
    let mut entries = ordered_ids
        .into_iter()
        .filter_map(|discord_id| {
            components_by_id
                .get(&discord_id)
                .copied()
                .map(|components| {
                    let mut parts = Vec::new();
                    if let Some(payout) = components.payout {
                        let label = if winning.contains(&discord_id) {
                            "win"
                        } else if losing.contains(&discord_id) {
                            "play"
                        } else if excluded.contains(&discord_id) {
                            "excluded"
                        } else {
                            "payout"
                        };
                        parts.push(format!("{label} {}", signed(payout)));
                    }
                    if let Some(bet) = components.bet {
                        parts.push(format!("bet {}", signed(bet)));
                    }
                    if let Some(streak) = components.streak {
                        parts.push(format!("streak {}", signed(streak)));
                    }
                    if let Some(referral) = components.referral {
                        parts.push(format!("referral {}", signed(referral)));
                    }
                    let total = components.total();
                    let detail = if parts.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", parts.join(", "))
                    };
                    SummaryEntry {
                        discord_id,
                        total,
                        is_participant: participants.contains(&discord_id),
                        is_bettor: components.bet.is_some(),
                        line: format!(
                            "<@{discord_id}>: **{}** {JOPACOIN_EMOTE}{detail}",
                            signed(total)
                        ),
                    }
                })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| right.total.cmp(&left.total));

    let mut sections = Vec::new();
    append_summary_section(
        &mut sections,
        "Match Players",
        entries.iter().filter(|entry| entry.is_participant),
    );
    append_summary_section(
        &mut sections,
        "Bettors",
        entries
            .iter()
            .filter(|entry| !entry.is_participant && entry.is_bettor),
    );
    append_summary_section(
        &mut sections,
        "Other Rewards",
        entries
            .iter()
            .filter(|entry| !entry.is_participant && !entry.is_bettor),
    );
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n🪙 **JC Changes:**\n{}", sections.join("\n"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SummaryEntry {
    discord_id: DiscordId,
    total: i64,
    is_participant: bool,
    is_bettor: bool,
    line: String,
}

fn append_summary_section<'a>(
    sections: &mut Vec<String>,
    label: &str,
    entries: impl Iterator<Item = &'a SummaryEntry>,
) {
    let lines = entries.map(|entry| entry.line.as_str()).collect::<Vec<_>>();
    if !lines.is_empty() {
        sections.push(format!("**{label}:**\n{}", lines.join("\n")));
    }
}

fn signed(amount: i64) -> String {
    match amount.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{amount}"),
        std::cmp::Ordering::Less => format!("−{}", amount.unsigned_abs()),
        std::cmp::Ordering::Equal => "0".to_owned(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferTarget {
    pub id: DiscordId,
    pub is_bot: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferCommandRequest {
    pub referrer_id: DiscordId,
    pub guild_id: GuildId,
    pub target: ReferTarget,
    pub lobby_channel_id: Option<DiscordId>,
    pub low_skill_lobby_channel_id: Option<DiscordId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferPlan {
    pub ephemeral: bool,
    pub thinking: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AllowedMentions {
    pub users: Vec<DiscordId>,
    pub roles: bool,
    pub everyone: bool,
    pub replied_user: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FollowupPlan {
    pub content: String,
    pub ephemeral: bool,
    pub allowed_mentions: AllowedMentions,
}

impl FollowupPlan {
    fn ephemeral(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: true,
            allowed_mentions: AllowedMentions::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferCommandPlan {
    pub defer: DeferPlan,
    pub followups: Vec<FollowupPlan>,
}

#[must_use]
pub fn execute_refer(
    service: &impl ReferralCreatePort,
    request: ReferCommandRequest,
) -> ReferCommandPlan {
    let defer = DeferPlan {
        ephemeral: true,
        thinking: false,
    };
    let reject = |content: &str| ReferCommandPlan {
        defer,
        followups: vec![FollowupPlan::ephemeral(content)],
    };

    if request.target.is_bot {
        return reject("❌ You can only refer human players.");
    }
    if request.target.id == request.referrer_id {
        return reject("❌ You can't refer yourself.");
    }

    match service.create_referral(request.referrer_id, request.target.id, request.guild_id) {
        Ok(()) => ReferCommandPlan {
            defer,
            followups: vec![
                FollowupPlan::ephemeral(REFERRAL_CREATED_MESSAGE),
                FollowupPlan {
                    content: onboarding_message_with_channels(
                        request.target.id,
                        request.lobby_channel_id,
                        request.low_skill_lobby_channel_id,
                    ),
                    ephemeral: false,
                    allowed_mentions: AllowedMentions {
                        users: vec![request.target.id],
                        roles: false,
                        everyone: false,
                        replied_user: false,
                    },
                },
            ],
        },
        Err(ReferralCreateFailure::Backend(_)) => {
            reject("❌ I couldn't create that referral. Try again later.")
        }
        Err(error) => reject(&format!("❌ {error}")),
    }
}

#[must_use]
pub fn onboarding_message(target_id: DiscordId) -> String {
    onboarding_message_with_channels(target_id, None, None)
}

#[must_use]
pub fn onboarding_message_with_channels(
    target_id: DiscordId,
    lobby_channel_id: Option<DiscordId>,
    low_skill_lobby_channel_id: Option<DiscordId>,
) -> String {
    let mut channels = Vec::new();
    for channel_id in [lobby_channel_id, low_skill_lobby_channel_id]
        .into_iter()
        .flatten()
    {
        if !channels.contains(&channel_id) {
            channels.push(channel_id);
        }
    }
    let destination = if channels.is_empty() {
        "a lobby channel".to_owned()
    } else {
        channels
            .into_iter()
            .map(|channel_id| format!("<#{}>", channel_id))
            .collect::<Vec<_>>()
            .join(" or ")
    };
    format!(
        "**Referral: <@{target_id}>**\n\
         1. If needed, run `/player register` with your Steam32 ID \
         (the number in your Dotabuff URL).\n\
         2. Run `/player roles` with your Dota positions (1-5).\n\
         3. React in {destination} to join an active lobby, \
         or run `/lobby` to start one."
    )
}

#[cfg(test)]
#[path = "referrals/tests.rs"]
mod tests;
