//! Adapter-neutral command policy for Jopacoin Reserve disbursement actions.
//!
//! The live Python command remains authoritative. This module makes the
//! interaction, pagination, and partial-message mutation contract executable
//! without a Discord gateway. A future transport adapter can execute these
//! plans while preserving the important rule that message mutations never
//! issue a REST fetch first.

use std::collections::BTreeMap;

pub const MONETARY_RECOVERY_CODE: &str = "monetary_recovery";
pub const ECONOMY_EDICT_CODE: &str = "economy_edict";
pub const MONETARY_RECOVERY_REASON: &str = "Jopacoin Reserve voting is temporarily disabled while the economy is in monetary recovery mode.";
pub const DISBURSE_VOTES_PAGE_SIZE: usize = 15;
pub const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";

pub const METHODS: [(&str, &str); 10] = [
    ("even", "Even Split"),
    ("proportional", "Proportional"),
    ("neediest", "Neediest First"),
    ("stimulus", "Stimulus"),
    ("lottery", "Lottery"),
    ("social_security", "Social Security"),
    ("richest", "Richest"),
    ("burn", "Burn"),
    ("next_match_pot", "Next Match Pot"),
    ("cancel", "Cancel"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub guild_id: i64,
    pub proposal_id: i64,
    pub message_id: Option<u64>,
    pub channel_id: Option<u64>,
    pub fund_amount: i64,
    pub quorum_required: u32,
    pub status: String,
    pub votes: BTreeMap<String, u32>,
}

impl Proposal {
    #[must_use]
    pub fn total_votes(&self) -> u32 {
        self.votes.values().sum()
    }

    #[must_use]
    pub fn quorum_reached(&self) -> bool {
        self.total_votes() >= self.quorum_required
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteRow {
    pub discord_id: i64,
    pub vote_method: String,
    pub voted_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: u32,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
}

impl Embed {
    #[must_use]
    pub fn field_containing(&self, needle: &str) -> Option<&EmbedField> {
        self.fields.iter().find(|field| field.name.contains(needle))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restriction {
    pub code: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewPlan {
    None,
    Vote,
    VoteAudit {
        total_pages: usize,
        requester_id: i64,
    },
    DisabledVote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub content: Option<String>,
    pub embed: Option<Embed>,
    pub view: ViewPlan,
    pub ephemeral: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartialMessageMutationKind {
    Delete,
    EditEmbed(Embed),
    DisableVoteButtons,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialMessageMutation {
    pub channel_id: u64,
    pub message_id: u64,
    pub kind: PartialMessageMutationKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessageRef {
    pub guild_id: i64,
    pub message_id: u64,
    pub channel_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionPlan {
    pub delivery: DeliveryPlan,
    pub mutations: Vec<PartialMessageMutation>,
    pub store_message: Option<StoredMessageRef>,
    pub invoke_reset: bool,
    pub invoke_force_execute: bool,
}

impl ActionPlan {
    fn delivery(delivery: DeliveryPlan) -> Self {
        Self {
            delivery,
            mutations: Vec::new(),
            store_message: None,
            invoke_reset: false,
            invoke_force_execute: false,
        }
    }
}

fn vote_count(proposal: &Proposal, method: &str) -> u32 {
    proposal.votes.get(method).copied().unwrap_or(0)
}

fn field(name: impl Into<String>, value: impl Into<String>, inline: bool) -> EmbedField {
    EmbedField {
        name: name.into(),
        value: value.into(),
        inline,
    }
}

#[must_use]
pub fn build_disburse_embed(proposal: &Proposal) -> Embed {
    let total = proposal.total_votes();
    let progress = if proposal.quorum_required == 0 {
        0.0
    } else {
        f64::from(total) / f64::from(proposal.quorum_required)
    };
    let filled = (progress * 20.0) as usize;
    let mut fields = vec![
        field(
            "📊 Even Split",
            format!(
                "Split equally to debtors\n**{}** votes",
                vote_count(proposal, "even")
            ),
            true,
        ),
        field(
            "📈 Proportional",
            format!(
                "More debt = more funds\n**{}** votes",
                vote_count(proposal, "proportional")
            ),
            true,
        ),
        field(
            "🎯 Neediest First",
            format!(
                "All to most indebted\n**{}** votes",
                vote_count(proposal, "neediest")
            ),
            true,
        ),
        field(
            "💸 Stimulus",
            format!(
                "Even split to active non-top-3\n**{}** votes",
                vote_count(proposal, "stimulus")
            ),
            true,
        ),
        field(
            "🎲 Lottery",
            format!(
                "Random active player wins all\n**{}** votes",
                vote_count(proposal, "lottery")
            ),
            true,
        ),
        field(
            "👴 Social Security",
            format!(
                "By games played (excl. top 3)\n**{}** votes",
                vote_count(proposal, "social_security")
            ),
            true,
        ),
        field(
            "💎 Richest",
            format!(
                "All to the richest player\n**{}** votes",
                vote_count(proposal, "richest")
            ),
            true,
        ),
        field(
            "🔥 Burn",
            format!(
                "Remove reserve funds\n**{}** votes",
                vote_count(proposal, "burn")
            ),
            true,
        ),
        field(
            "🎰 Next Match Pot",
            format!(
                "Split all funds evenly into the next betting pot\n**{}** votes",
                vote_count(proposal, "next_match_pot")
            ),
            true,
        ),
        field(
            "❌ Cancel",
            format!(
                "Keep budget in reserve\n**{}** votes",
                vote_count(proposal, "cancel")
            ),
            true,
        ),
        field(
            "Quorum Progress",
            format!(
                "`{}{}` {}/{} ({}%)",
                "█".repeat(filled),
                "░".repeat(20_usize.saturating_sub(filled)),
                total,
                proposal.quorum_required,
                (progress * 100.0) as u32
            ),
            false,
        ),
    ];
    if proposal.quorum_reached() {
        fields.push(field(
            "✅ Quorum Reached!",
            "The next vote will trigger automatic disbursement.",
            false,
        ));
    }
    Embed {
        title: "🏛️ Jopacoin Reserve Allocation Vote".to_owned(),
        description: format!(
            "Vote on how to allocate **{}** {} from the server operations budget.\n\nClick a button below to vote!",
            proposal.fund_amount, JOPACOIN_EMOTE
        ),
        color: 0x00e9_1e63,
        fields,
        footer: Some("Ties are broken in favor of Even Split".to_owned()),
    }
}

#[must_use]
pub fn build_disburse_votes_embed(
    proposal: &Proposal,
    individual_votes: &[VoteRow],
    page: usize,
    page_size: usize,
) -> Embed {
    let page_size = page_size.max(1);
    let total_pages = individual_votes.len().div_ceil(page_size).max(1);
    let page = page.min(total_pages - 1);
    let start = page * page_size;
    let end = (start + page_size).min(individual_votes.len());
    let total = proposal.total_votes();
    let progress = if proposal.quorum_required == 0 {
        0
    } else {
        total.saturating_mul(100) / proposal.quorum_required
    };
    let breakdown = METHODS
        .iter()
        .map(|(method, label)| {
            let count = vote_count(proposal, method);
            let pct = if total == 0 {
                0.0
            } else {
                f64::from(count) / f64::from(total) * 100.0
            };
            format!("**{label}:** {count} ({pct:.0}%)")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let voters = individual_votes[start..end]
        .iter()
        .map(|vote| {
            let label = METHODS
                .iter()
                .find(|(method, _)| *method == vote.vote_method)
                .map_or(vote.vote_method.as_str(), |(_, label)| *label);
            format!("• <@{}> → {label}", vote.discord_id)
        })
        .collect::<Vec<_>>();
    let voters_text = if voters.is_empty() {
        "*No votes yet*".to_owned()
    } else {
        voters.join("\n")
    };
    let voter_field_name = if individual_votes.is_empty() {
        "👥 Individual Votes".to_owned()
    } else {
        format!(
            "👥 Individual Votes ({}-{} of {})",
            start + 1,
            end,
            individual_votes.len()
        )
    };
    Embed {
        title: "🔍 Disbursement Vote Details (Tax Man)".to_owned(),
        description: format!(
            "Reserve Budget: **{}** {}",
            proposal.fund_amount, JOPACOIN_EMOTE
        ),
        color: 0x009c_27b0,
        fields: vec![
            field(
                "📋 Proposal Status",
                format!(
                    "**Quorum:** {}/{} ({}%)\n**Status:** {}",
                    total,
                    proposal.quorum_required,
                    progress,
                    if proposal.quorum_reached() {
                        "✅ Ready"
                    } else {
                        "⏳ Voting"
                    }
                ),
                false,
            ),
            field("📊 Vote Breakdown", breakdown, false),
            field(voter_field_name, voters_text, false),
        ],
        footer: Some(format!(
            "Tax Man audit only | Page {}/{}",
            page + 1,
            total_pages
        )),
    }
}

fn text_delivery(content: impl Into<String>, ephemeral: bool) -> DeliveryPlan {
    DeliveryPlan {
        content: Some(content.into()),
        embed: None,
        view: ViewPlan::None,
        ephemeral,
    }
}

#[must_use]
pub fn disburse_votes_plan(
    is_tax_man: bool,
    proposal: Option<&Proposal>,
    individual_votes: &[VoteRow],
    requester_id: i64,
) -> ActionPlan {
    if !is_tax_man {
        return ActionPlan::delivery(text_delivery(
            "Only Tax Men can view detailed voting information.",
            true,
        ));
    }
    let Some(proposal) = proposal else {
        return ActionPlan::delivery(text_delivery(
            "No active disbursement proposal. Use `/economy disburse` and choose status to check.",
            true,
        ));
    };
    let total_pages = individual_votes
        .len()
        .div_ceil(DISBURSE_VOTES_PAGE_SIZE)
        .max(1);
    ActionPlan::delivery(DeliveryPlan {
        content: None,
        embed: Some(build_disburse_votes_embed(
            proposal,
            individual_votes,
            0,
            DISBURSE_VOTES_PAGE_SIZE,
        )),
        view: if total_pages > 1 {
            ViewPlan::VoteAudit {
                total_pages,
                requester_id,
            }
        } else {
            ViewPlan::None
        },
        ephemeral: true,
    })
}

#[must_use]
pub fn disburse_reset_plan(is_tax_man: bool, reset_succeeded: bool) -> ActionPlan {
    if !is_tax_man {
        return ActionPlan::delivery(text_delivery(
            "Only Tax Men can reset disbursement proposals.",
            true,
        ));
    }
    let mut plan = ActionPlan::delivery(text_delivery(
        if reset_succeeded {
            "Disbursement proposal has been reset."
        } else {
            "No active proposal to reset."
        },
        !reset_succeeded,
    ));
    plan.invoke_reset = true;
    plan
}

#[must_use]
pub fn disburse_status_plan(
    proposal: Option<&Proposal>,
    restriction: Option<&Restriction>,
    new_message_id: u64,
    interaction_channel_id: u64,
) -> ActionPlan {
    let Some(proposal) = proposal else {
        let content = restriction.map_or_else(
            || {
                "No active disbursement proposal. Use `/economy disburse` and choose propose to create one."
                    .to_owned()
            },
            |restriction| {
                format!(
                    "{} There is no active allocation ballot.",
                    restriction.reason
                )
            },
        );
        return ActionPlan::delivery(text_delivery(content, true));
    };
    let mut plan = ActionPlan::delivery(DeliveryPlan {
        content: restriction.map(|restriction| restriction.reason.clone()),
        embed: Some(build_disburse_embed(proposal)),
        view: ViewPlan::Vote,
        ephemeral: false,
    });
    if let (Some(channel_id), Some(message_id)) = (proposal.channel_id, proposal.message_id) {
        plan.mutations.push(PartialMessageMutation {
            channel_id,
            message_id,
            kind: PartialMessageMutationKind::Delete,
        });
    }
    plan.store_message = Some(StoredMessageRef {
        guild_id: proposal.guild_id,
        message_id: new_message_id,
        channel_id: interaction_channel_id,
    });
    plan
}

#[must_use]
pub fn update_disburse_message_plan(proposal: Option<&Proposal>) -> Option<PartialMessageMutation> {
    let proposal = proposal?;
    Some(PartialMessageMutation {
        channel_id: proposal.channel_id?,
        message_id: proposal.message_id?,
        kind: PartialMessageMutationKind::EditEmbed(build_disburse_embed(proposal)),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub cancelled: bool,
    pub method_label: String,
    pub total_disbursed: i64,
    pub recipient_count: usize,
    pub distributions: Vec<(i64, i64)>,
    pub message: Option<String>,
}

fn settlement_embed(settlement: &Settlement, forced: bool) -> Embed {
    let (title, description, color) = if settlement.cancelled {
        (
            if forced {
                "❌ Proposal Cancelled (Tax Man)".to_owned()
            } else {
                "❌ Proposal Cancelled".to_owned()
            },
            settlement
                .message
                .clone()
                .unwrap_or_else(|| "Proposal cancelled by vote.".to_owned()),
            0x00ff_6b6b,
        )
    } else if settlement.total_disbursed == 0 || settlement.message.is_some() {
        (
            if forced {
                "💝 Disbursement Complete (Tax Man)".to_owned()
            } else {
                "💝 Disbursement Complete!".to_owned()
            },
            settlement
                .message
                .clone()
                .unwrap_or_else(|| "No funds were distributed.".to_owned()),
            0x0000_ff00,
        )
    } else {
        let mut recipients = settlement
            .distributions
            .iter()
            .take(10)
            .map(|(discord_id, amount)| format!("<@{discord_id}>: +{amount}"))
            .collect::<Vec<_>>();
        if settlement.distributions.len() > 10 {
            recipients.push(format!(
                "...and {} more",
                settlement.distributions.len() - 10
            ));
        }
        (
            if forced {
                "💝 Disbursement Complete (Tax Man)".to_owned()
            } else {
                "💝 Disbursement Complete!".to_owned()
            },
            format!(
                "**{}** {} distributed via **{}** to {} player(s):\n{}",
                settlement.total_disbursed,
                JOPACOIN_EMOTE,
                settlement.method_label,
                settlement.recipient_count,
                recipients.join("\n")
            ),
            0x0000_ff00,
        )
    };
    Embed {
        title,
        description,
        color,
        fields: Vec::new(),
        footer: None,
    }
}

#[must_use]
pub fn disburse_execute_plan(
    is_tax_man: bool,
    restriction: Option<&Restriction>,
    proposal: Option<&Proposal>,
    settlement: Option<&Settlement>,
) -> ActionPlan {
    if !is_tax_man {
        return ActionPlan::delivery(text_delivery(
            "Only Tax Men can force-execute disbursement proposals.",
            true,
        ));
    }
    if let Some(restriction) = restriction {
        return ActionPlan::delivery(text_delivery(&restriction.reason, true));
    }
    let Some(proposal) = proposal else {
        return ActionPlan::delivery(text_delivery("No active disbursement proposal.", true));
    };
    let Some(settlement) = settlement else {
        return ActionPlan::delivery(text_delivery(
            "Cannot execute: missing settlement result",
            true,
        ));
    };
    let mut plan = ActionPlan::delivery(DeliveryPlan {
        content: None,
        embed: Some(settlement_embed(settlement, true)),
        view: ViewPlan::None,
        ephemeral: false,
    });
    plan.invoke_force_execute = true;
    if let (Some(channel_id), Some(message_id)) = (proposal.channel_id, proposal.message_id) {
        plan.mutations.push(PartialMessageMutation {
            channel_id,
            message_id,
            kind: PartialMessageMutationKind::DisableVoteButtons,
        });
    }
    plan
}

#[must_use]
pub fn disburse_propose_restriction_plan(restriction: &Restriction) -> ActionPlan {
    ActionPlan::delivery(text_delivery(&restriction.reason, true))
}

#[must_use]
pub fn vote_restriction_plan(restriction: &Restriction) -> ActionPlan {
    ActionPlan::delivery(text_delivery(&restriction.reason, true))
}

#[must_use]
pub fn quorum_vote_plan(proposal: &Proposal, settlement: &Settlement) -> ActionPlan {
    let mut plan = ActionPlan::delivery(DeliveryPlan {
        content: None,
        embed: Some(settlement_embed(settlement, false)),
        view: ViewPlan::None,
        ephemeral: false,
    });
    if let (Some(channel_id), Some(message_id)) = (proposal.channel_id, proposal.message_id) {
        plan.mutations.push(PartialMessageMutation {
            channel_id,
            message_id,
            kind: PartialMessageMutationKind::DisableVoteButtons,
        });
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal() -> Proposal {
        Proposal {
            guild_id: 987_654_321,
            proposal_id: 123,
            message_id: None,
            channel_id: None,
            fund_amount: 500,
            quorum_required: 10,
            status: "active".to_owned(),
            votes: METHODS
                .iter()
                .map(|(method, _)| ((*method).to_owned(), 0))
                .collect(),
        }
    }

    fn vote_rows(count: usize) -> Vec<VoteRow> {
        (0..count)
            .map(|index| VoteRow {
                discord_id: 10_000 + index as i64,
                vote_method: if index % 2 == 0 {
                    "even".to_owned()
                } else {
                    "proportional".to_owned()
                },
                voted_at: 1_700_000_000 + index as i64,
            })
            .collect()
    }

    fn monetary_recovery() -> Restriction {
        Restriction {
            code: MONETARY_RECOVERY_CODE.to_owned(),
            reason: MONETARY_RECOVERY_REASON.to_owned(),
        }
    }

    fn edict() -> Restriction {
        Restriction {
            code: ECONOMY_EDICT_CODE.to_owned(),
            reason: "Jopacoin Reserve allocation voting is suspended by Ravage — Level III."
                .to_owned(),
        }
    }

    #[test]
    fn test_disburse_embed_uses_jopacoin_reserve_language() {
        let embed = build_disburse_embed(&proposal());
        let text = std::iter::once(embed.title.as_str())
            .chain(std::iter::once(embed.description.as_str()))
            .chain(
                embed
                    .fields
                    .iter()
                    .flat_map(|field| [field.name.as_str(), field.value.as_str()]),
            )
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Jopacoin Reserve"));
        assert!(text.contains("server operations budget"));
        assert!(text.contains("Keep budget in reserve"));
        assert!(text.contains("Remove reserve funds"));
        assert!(!text.contains("Remove reserve funds permanently"));
        assert!(text.contains("Split all funds evenly into the next betting pot"));
        assert!(!text.contains("Nonprofit"));
    }

    #[test]
    fn test_disburse_votes_embed_pages_show_every_voter() {
        let mut proposal = proposal();
        proposal.votes.insert("even".to_owned(), 16);
        proposal.votes.insert("proportional".to_owned(), 15);
        let votes = vote_rows(31);
        let pages = (0..3)
            .map(|page| build_disburse_votes_embed(&proposal, &votes, page, 15))
            .collect::<Vec<_>>();
        let all_vote_text = pages
            .iter()
            .map(|page| {
                &page
                    .field_containing("Individual Votes")
                    .expect("voter field")
                    .value
            })
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        for vote in votes {
            assert!(all_vote_text.contains(&format!("<@{}>", vote.discord_id)));
        }
        assert!(!all_vote_text.contains("..."));
        assert!(
            pages[0]
                .footer
                .as_deref()
                .expect("footer")
                .ends_with("Page 1/3")
        );
        assert!(
            pages[2]
                .footer
                .as_deref()
                .expect("footer")
                .ends_with("Page 3/3")
        );
    }

    #[test]
    fn test_disburse_votes_tax_man_gets_paginated_view() {
        let votes = vote_rows(16);
        let plan = disburse_votes_plan(true, Some(&proposal()), &votes, 999);
        assert!(plan.delivery.ephemeral);
        assert_eq!(
            plan.delivery.view,
            ViewPlan::VoteAudit {
                total_pages: 2,
                requester_id: 999,
            }
        );
        let embed = plan.delivery.embed.expect("audit embed");
        let field = embed
            .field_containing("Individual Votes")
            .expect("voter field");
        assert!(field.value.contains("<@10000>"));
        assert!(!field.value.contains("<@10015>"));
    }

    #[test]
    fn test_disburse_votes_blocks_non_tax_man() {
        let plan = disburse_votes_plan(false, Some(&proposal()), &[], 999);
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .contains("Only Tax Men")
        );
    }

    #[test]
    fn test_disburse_reset_tax_man_can_reset() {
        let plan = disburse_reset_plan(true, true);
        assert!(plan.invoke_reset);
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .to_lowercase()
                .contains("reset")
        );
        assert!(!plan.delivery.ephemeral);
    }

    #[test]
    fn test_disburse_status_replaces_old_message_without_fetch() {
        let mut proposal = proposal();
        proposal.message_id = Some(321);
        proposal.channel_id = Some(456);
        let plan = disburse_status_plan(Some(&proposal), None, 789, 456);
        assert_eq!(
            plan.mutations,
            vec![PartialMessageMutation {
                channel_id: 456,
                message_id: 321,
                kind: PartialMessageMutationKind::Delete,
            }]
        );
        assert_eq!(
            plan.store_message,
            Some(StoredMessageRef {
                guild_id: proposal.guild_id,
                message_id: 789,
                channel_id: 456,
            })
        );
    }

    #[test]
    fn test_disburse_status_keeps_resumable_vote_view_during_edict() {
        let restriction = edict();
        let plan = disburse_status_plan(Some(&proposal()), Some(&restriction), 789, 456);
        assert_eq!(
            plan.delivery.content.as_deref(),
            Some(restriction.reason.as_str())
        );
        assert_eq!(plan.delivery.view, ViewPlan::Vote);
    }

    #[test]
    fn test_update_disburse_message_edits_without_fetch() {
        let mut proposal = proposal();
        proposal.message_id = Some(321);
        proposal.channel_id = Some(456);
        let mutation = update_disburse_message_plan(Some(&proposal)).expect("mutation");
        assert_eq!(mutation.channel_id, 456);
        assert_eq!(mutation.message_id, 321);
        let PartialMessageMutationKind::EditEmbed(embed) = mutation.kind else {
            panic!("expected embed edit")
        };
        assert_eq!(embed.title, "🏛️ Jopacoin Reserve Allocation Vote");
    }

    #[test]
    fn test_disburse_execute_tax_man_can_force_execute() {
        let mut proposal = proposal();
        proposal.message_id = Some(321);
        proposal.channel_id = Some(456);
        let settlement = Settlement {
            cancelled: false,
            method_label: "Even Split".to_owned(),
            total_disbursed: 0,
            recipient_count: 0,
            distributions: Vec::new(),
            message: Some("No funds were distributed.".to_owned()),
        };
        let plan = disburse_execute_plan(true, None, Some(&proposal), Some(&settlement));
        assert!(plan.invoke_force_execute);
        assert_eq!(
            plan.delivery.embed.expect("embed").title,
            "💝 Disbursement Complete (Tax Man)"
        );
        assert_eq!(
            plan.mutations[0].kind,
            PartialMessageMutationKind::DisableVoteButtons
        );
    }

    #[test]
    fn test_disburse_execute_rejects_during_monetary_recovery() {
        let restriction = monetary_recovery();
        let plan = disburse_execute_plan(true, Some(&restriction), Some(&proposal()), None);
        assert!(!plan.invoke_force_execute);
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .contains("monetary recovery mode")
        );
        assert!(plan.delivery.ephemeral);
    }

    #[test]
    fn test_disburse_propose_names_active_economy_edict() {
        let plan = disburse_propose_restriction_plan(&edict());
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .ends_with("Ravage — Level III.")
        );
        assert!(plan.delivery.ephemeral);
    }

    #[test]
    fn test_vote_button_rejects_clearly_during_monetary_recovery() {
        let plan = vote_restriction_plan(&monetary_recovery());
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .contains("monetary recovery mode")
        );
        assert!(plan.delivery.ephemeral);
    }

    #[test]
    fn test_vote_button_names_active_economy_edict_before_registration() {
        let plan = vote_restriction_plan(&edict());
        assert!(
            plan.delivery
                .content
                .as_deref()
                .expect("content")
                .ends_with("Ravage — Level III.")
        );
        assert!(plan.delivery.ephemeral);
    }

    #[test]
    fn test_quorum_vote_disables_original_message_without_fetch() {
        let mut proposal = proposal();
        proposal.message_id = Some(321);
        proposal.channel_id = Some(456);
        let settlement = Settlement {
            cancelled: true,
            method_label: "Cancel".to_owned(),
            total_disbursed: 0,
            recipient_count: 0,
            distributions: Vec::new(),
            message: Some("Cancelled by vote.".to_owned()),
        };
        let plan = quorum_vote_plan(&proposal, &settlement);
        assert_eq!(
            plan.mutations,
            vec![PartialMessageMutation {
                channel_id: 456,
                message_id: 321,
                kind: PartialMessageMutationKind::DisableVoteButtons,
            }]
        );
    }
}
