use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use super::*;

const GUILD: i64 = 7_001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CreateCall {
    referrer_id: i64,
    referred_id: i64,
    guild_id: i64,
}

#[derive(Debug, Default)]
struct FakeReferralPort {
    calls: Mutex<Vec<CreateCall>>,
    outcomes: Mutex<VecDeque<Result<(), ReferralCreateFailure>>>,
}

impl FakeReferralPort {
    fn with_outcome(outcome: Result<(), ReferralCreateFailure>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(VecDeque::from([outcome])),
        }
    }

    fn calls(&self) -> Vec<CreateCall> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl ReferralCreatePort for FakeReferralPort {
    fn create_referral(
        &self,
        referrer_id: DiscordId,
        referred_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<(), ReferralCreateFailure> {
        self.calls.lock().expect("calls lock").push(CreateCall {
            referrer_id,
            referred_id,
            guild_id,
        });
        self.outcomes
            .lock()
            .expect("outcomes lock")
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

fn request(target_id: i64, is_bot: bool) -> ReferCommandRequest {
    ReferCommandRequest {
        referrer_id: 10,
        guild_id: GUILD,
        target: ReferTarget {
            id: target_id,
            is_bot,
        },
    }
}

#[test]
fn test_player_service_creates_referral_through_repository() {
    let repository = FakeReferralPort::with_outcome(Ok(()));
    let service = PlayerReferralService::new(repository);
    service
        .create_referral(10, 20, GUILD)
        .expect("service enrollment");
    assert_eq!(
        service.repository().calls(),
        [CreateCall {
            referrer_id: 10,
            referred_id: 20,
            guild_id: GUILD,
        }]
    );
}

#[test]
fn test_referral_jc_component_survives_post_match_bonus_accounting() {
    let mut changes = BTreeMap::from([(
        111,
        JcComponents {
            payout: Some(10),
            ..JcComponents::default()
        },
    )]);
    merge_referral_rewards(
        &mut changes,
        &[ReferralReward {
            referred_id: 111,
            referrer_id: 110,
            reward_amount: 100,
        }],
    );
    assert_eq!(
        changes.get(&111),
        Some(&JcComponents {
            payout: Some(10),
            referral: Some(100),
            ..JcComponents::default()
        })
    );
    assert_eq!(
        changes.get(&110),
        Some(&JcComponents {
            referral: Some(100),
            ..JcComponents::default()
        })
    );
}

#[test]
fn test_referral_rewards_join_the_netted_jc_summary() {
    let summary = format_jc_changes(&JcSummaryInput {
        winning_player_ids: vec![20],
        changes: vec![
            (
                20,
                JcComponents {
                    payout: Some(10),
                    referral: Some(100),
                    ..JcComponents::default()
                },
            ),
            (
                10,
                JcComponents {
                    referral: Some(100),
                    ..JcComponents::default()
                },
            ),
        ],
        ..JcSummaryInput::default()
    });
    assert_eq!(
        summary,
        concat!(
            "\n\n🪙 **JC Changes:**\n",
            "**Match Players:**\n",
            "<@20>: **+110** <:jopacoin:954159801049440297> (win +10, referral +100)\n",
            "**Other Rewards:**\n",
            "<@10>: **+100** <:jopacoin:954159801049440297> (referral +100)"
        )
    );
}

#[test]
fn test_referral_uses_private_ack_before_public_onboarding() {
    let port = FakeReferralPort::with_outcome(Ok(()));
    let plan = execute_refer(&port, request(20, false));
    assert_eq!(
        port.calls(),
        [CreateCall {
            referrer_id: 10,
            referred_id: 20,
            guild_id: GUILD,
        }]
    );
    assert_eq!(
        plan.defer,
        DeferPlan {
            ephemeral: true,
            thinking: false,
        }
    );
    assert_eq!(plan.followups.len(), 2);
    assert_eq!(plan.followups[0].content, REFERRAL_CREATED_MESSAGE);
    assert!(plan.followups[0].ephemeral);
    assert_eq!(plan.followups[1].content, onboarding_message(20));
    assert!(!plan.followups[1].ephemeral);
    assert_eq!(
        plan.followups[1].allowed_mentions,
        AllowedMentions {
            users: vec![20],
            roles: false,
            everyone: false,
            replied_user: false,
        }
    );
}

#[test]
fn test_bot_target_is_rejected_ephemerally() {
    let port = FakeReferralPort::default();
    let plan = execute_refer(&port, request(20, true));
    assert!(port.calls().is_empty());
    assert_eq!(plan.followups.len(), 1);
    assert!(plan.followups[0].ephemeral);
    assert!(plan.followups[0].content.contains("human players"));
}

#[test]
fn test_self_target_is_rejected_ephemerally() {
    let port = FakeReferralPort::default();
    let plan = execute_refer(&port, request(10, false));
    assert!(port.calls().is_empty());
    assert_eq!(plan.followups.len(), 1);
    assert!(plan.followups[0].ephemeral);
    assert!(plan.followups[0].content.contains("yourself"));
}

#[test]
fn test_unregistered_referrer_error_is_ephemeral() {
    let port = FakeReferralPort::with_outcome(Err(ReferralCreateFailure::ReferrerNotRegistered));
    let plan = execute_refer(&port, request(20, false));
    assert_eq!(port.calls().len(), 1);
    assert_eq!(plan.followups.len(), 1);
    assert!(plan.followups[0].ephemeral);
    assert!(
        plan.followups[0]
            .content
            .contains("Referrer must be registered")
    );
}

#[test]
fn test_registered_target_error_is_ephemeral() {
    let port =
        FakeReferralPort::with_outcome(Err(ReferralCreateFailure::ReferredAlreadyRegistered));
    let plan = execute_refer(&port, request(20, false));
    assert_eq!(port.calls().len(), 1);
    assert_eq!(plan.followups.len(), 1);
    assert!(plan.followups[0].ephemeral);
    assert!(plan.followups[0].content.contains("already registered"));
}

#[test]
fn test_duplicate_referral_error_does_not_expose_original_referrer() {
    let port = FakeReferralPort::with_outcome(Err(ReferralCreateFailure::AlreadyReferred));
    let plan = execute_refer(&port, request(20, false));
    assert_eq!(port.calls().len(), 1);
    assert_eq!(plan.followups.len(), 1);
    assert!(plan.followups[0].ephemeral);
    assert_eq!(
        plan.followups[0].content,
        "❌ That player already has a referral."
    );
    assert!(!plan.followups[0].content.contains("<@"));
}

#[test]
fn backend_failures_are_ephemeral_and_do_not_leak_sqlite_details() {
    let port = FakeReferralPort::with_outcome(Err(ReferralCreateFailure::Backend(
        "database disk image is malformed".to_owned(),
    )));
    let plan = execute_refer(&port, request(20, false));
    assert_eq!(
        plan.followups[0].content,
        "❌ I couldn't create that referral. Try again later."
    );
    assert!(plan.followups[0].ephemeral);
}

#[test]
fn signed_ids_and_guild_are_forwarded_without_lossy_normalization() {
    let port = FakeReferralPort::with_outcome(Ok(()));
    let plan = execute_refer(
        &port,
        ReferCommandRequest {
            referrer_id: -10,
            guild_id: -30,
            target: ReferTarget {
                id: -20,
                is_bot: false,
            },
        },
    );
    assert_eq!(
        port.calls(),
        [CreateCall {
            referrer_id: -10,
            referred_id: -20,
            guild_id: -30,
        }]
    );
    assert_eq!(plan.followups[1].allowed_mentions.users, [-20]);
}
