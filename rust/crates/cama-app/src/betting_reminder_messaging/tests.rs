use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use super::*;

const LOCK_TS: i64 = 1_700_000_000;

fn pending_state() -> PendingBettingState {
    PendingBettingState {
        betting_mode: BettingMode::Pool,
        bet_lock_until: Some(LOCK_TS),
        pending_match_id: Some(1),
        lobby_kind: LobbyKind::Open,
        bet_seed_radiant: 0,
        bet_seed_dire: 0,
        bet_seed_bonus: 0,
        shuffle_channel_id: Some(111),
        shuffle_message_id: Some(112),
        command_shuffle_channel_id: None,
        command_shuffle_message_id: None,
        thread_shuffle_thread_id: None,
        thread_shuffle_message_id: None,
        radiant_team_ids: vec![1001, 1002, 1003, 1004, 1005],
        dire_team_ids: vec![2001, 2002, 2003, 2004, 2005],
    }
}

fn message_info() -> ShuffleMessageInfo {
    ShuffleMessageInfo {
        channel_id: Some(111),
        origin_channel_id: Some(111),
        ..ShuffleMessageInfo::default()
    }
}

#[derive(Default)]
struct FakeFlavor {
    calls: Mutex<Vec<FlavorRequest>>,
    response: Mutex<Option<String>>,
}

impl FakeFlavor {
    fn responding(text: &str) -> Self {
        Self {
            calls: Mutex::default(),
            response: Mutex::new(Some(text.to_owned())),
        }
    }

    fn calls(&self) -> Vec<FlavorRequest> {
        self.calls.lock().expect("flavor calls").clone()
    }
}

impl BettingFlavorPort for FakeFlavor {
    fn generate(&self, request: &FlavorRequest) -> Result<Option<String>, String> {
        self.calls
            .lock()
            .expect("flavor calls")
            .push(request.clone());
        Ok(self.response.lock().expect("flavor response").clone())
    }
}

fn reminder_plan(
    reminder_type: ReminderType,
    final_warning: bool,
    state: &PendingBettingState,
    info: &ShuffleMessageInfo,
    totals: PotTotals,
    flavor: Option<&dyn BettingFlavorPort>,
) -> ReminderPlan {
    build_reminder(
        ReminderRequest {
            guild_id: Some(1),
            reminder_type,
            lock_until: Some(LOCK_TS),
            pending_match_id: Some(1),
            is_final_warning: final_warning,
            now: LOCK_TS - 300,
            pending_state: state,
            message_info: info,
            totals,
            top_bettor: None,
        },
        flavor,
    )
    .expect("reminder plan")
}

fn channel_delivery(plan: &ReminderPlan) -> &ReminderDelivery {
    plan.deliveries
        .iter()
        .find(|delivery| matches!(delivery.destination, ReminderDestination::Channel { .. }))
        .expect("channel delivery")
}

fn scoped_ids(delivery: &ReminderDelivery) -> BTreeSet<i64> {
    assert!(!delivery.allowed_mentions.everyone);
    assert!(!delivery.allowed_mentions.roles);
    match &delivery.allowed_mentions.users {
        MentionUsers::Scoped(ids) => ids.iter().copied().collect(),
        MentionUsers::Disabled => panic!("expected scoped users"),
    }
}

fn assert_identity(reminder_type: ReminderType) {
    let mut state = pending_state();
    state.pending_match_id = Some(42);
    state.lobby_kind = LobbyKind::LowSkill;
    let flavor = FakeFlavor::responding("ANNOUNCER LINE");
    let plan = reminder_plan(
        reminder_type,
        false,
        &state,
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 200,
        },
        Some(&flavor),
    );
    let content = &channel_delivery(&plan).content;
    assert!(content.contains(LobbyKind::LowSkill.display_name()));
    assert!(content.contains("Match #42"));
}

#[test]
fn test_reminder_identifies_lobby_and_pending_match_warning() {
    assert_identity(ReminderType::Warning);
}

#[test]
fn test_reminder_identifies_lobby_and_pending_match_last_call() {
    assert_identity(ReminderType::LastCall);
}

#[test]
fn test_reminder_identifies_lobby_and_pending_match_closed() {
    assert_identity(ReminderType::Closed);
}

struct CopyPort {
    state: PendingBettingState,
    totals: PotTotals,
    load_calls: AtomicUsize,
    total_calls: AtomicUsize,
    updates: Mutex<Vec<MessageTarget>>,
    failures: BTreeSet<MessageTarget>,
    barrier: Option<Arc<Barrier>>,
}

impl CopyPort {
    fn new(state: PendingBettingState) -> Self {
        Self {
            state,
            totals: PotTotals {
                radiant: 100,
                dire: 200,
            },
            load_calls: AtomicUsize::new(0),
            total_calls: AtomicUsize::new(0),
            updates: Mutex::default(),
            failures: BTreeSet::new(),
            barrier: None,
        }
    }
}

impl WagerCopyPort for CopyPort {
    fn load_pending_state(
        &self,
        _guild_id: Option<GuildId>,
        _pending_match_id: Option<i64>,
    ) -> Option<PendingBettingState> {
        self.load_calls.fetch_add(1, Ordering::SeqCst);
        Some(self.state.clone())
    }

    fn pot_totals(
        &self,
        _guild_id: Option<GuildId>,
        _pending_state: &PendingBettingState,
    ) -> PotTotals {
        self.total_calls.fetch_add(1, Ordering::SeqCst);
        self.totals
    }

    fn update_wager_field(&self, target: MessageTarget, _field: &WagerField) -> Result<(), String> {
        self.updates.lock().expect("updates").push(target);
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        if self.failures.contains(&target) {
            Err("copy unavailable".to_owned())
        } else {
            Ok(())
        }
    }
}

#[test]
fn test_wager_message_copies_update_concurrently_with_reused_state() {
    let mut state = pending_state();
    state.command_shuffle_channel_id = Some(221);
    state.command_shuffle_message_id = Some(222);
    state.thread_shuffle_thread_id = Some(331);
    state.thread_shuffle_message_id = Some(332);
    let mut port = CopyPort::new(state.clone());
    port.barrier = Some(Arc::new(Barrier::new(3)));

    let report = refresh_wager_copies(
        &port,
        WagerRefreshRequest {
            guild_id: Some(1),
            pending_match_id: Some(1),
            locked: false,
            pending_state: Some(&state),
        },
    )
    .expect("refresh report");

    assert_eq!(report.attempted.len(), 3);
    assert!(report.failed.is_empty());
    assert_eq!(port.load_calls.load(Ordering::SeqCst), 0);
    assert_eq!(port.total_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_wager_message_targets_are_deduplicated_and_failure_isolated() {
    let mut state = pending_state();
    state.command_shuffle_channel_id = state.shuffle_channel_id;
    state.command_shuffle_message_id = state.shuffle_message_id;
    state.thread_shuffle_thread_id = Some(331);
    state.thread_shuffle_message_id = Some(332);
    let primary = MessageTarget {
        channel_id: 111,
        message_id: 112,
    };
    let mut port = CopyPort::new(state.clone());
    port.failures.insert(primary);

    let report = refresh_wager_copies(
        &port,
        WagerRefreshRequest {
            guild_id: Some(1),
            pending_match_id: Some(1),
            locked: false,
            pending_state: Some(&state),
        },
    )
    .expect("refresh report");

    assert_eq!(report.attempted.len(), 2);
    assert_eq!(report.failed, vec![primary]);
    assert_eq!(port.updates.lock().expect("updates").len(), 2);
}

#[test]
fn test_warning_reminder_is_terse_and_skips_flavor() {
    let flavor = FakeFlavor::responding("ANNOUNCER LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        false,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 200,
        },
        Some(&flavor),
    );
    let content = &channel_delivery(&plan).content;
    assert!(content.contains(&format!("Betting closes <t:{LOCK_TS}:R>")));
    assert!(!content.contains("ANNOUNCER LINE"));
    assert!(flavor.calls().is_empty());
}

#[test]
fn test_warning_reminder_includes_pool_seed_text() {
    let mut state = pending_state();
    state.bet_seed_radiant = 25;
    state.bet_seed_dire = 25;
    let plan = reminder_plan(
        ReminderType::Warning,
        false,
        &state,
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 200,
        },
        None,
    );
    let content = &channel_delivery(&plan).content;
    assert!(content.contains("Seed: Radiant 25"));
    assert!(content.contains("Dire 25"));
}

#[test]
fn test_last_call_reminder_includes_flavor_line() {
    let flavor = FakeFlavor::responding("ANNOUNCER LINE");
    let plan = reminder_plan(
        ReminderType::LastCall,
        false,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 200,
        },
        Some(&flavor),
    );
    let content = &channel_delivery(&plan).content;
    assert!(content.contains("Last call"));
    assert!(content.contains("ANNOUNCER LINE"));
    assert_eq!(flavor.calls()[0].kind, FlavorKind::LastCall);
}

#[test]
fn test_closed_reminder_includes_house_bonus_text() {
    let mut state = pending_state();
    state.betting_mode = BettingMode::House;
    state.bet_seed_bonus = 50;
    let plan = reminder_plan(
        ReminderType::Closed,
        false,
        &state,
        &message_info(),
        PotTotals {
            radiant: 10,
            dire: 20,
        },
        None,
    );
    let content = &channel_delivery(&plan).content;
    assert!(content.contains("Final House (1:1) pool"));
    assert!(content.contains("Winner bonus: 50"));
}

#[test]
fn test_closed_reminder_flips_embed_to_locked() {
    let state = pending_state();
    let plan = reminder_plan(
        ReminderType::Closed,
        false,
        &state,
        &message_info(),
        PotTotals {
            radiant: 10,
            dire: 20,
        },
        None,
    );
    assert_eq!(
        plan.locked_wager_refresh,
        Some(LockedWagerRefresh {
            pending_match_id: Some(1),
            locked: true,
            reuse_loaded_state: true,
        })
    );
}

#[test]
fn test_non_final_warning_stays_terse() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        false,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert!(!delivery.content.contains("WARNING LINE"));
    assert!(!delivery.content.contains("<@"));
    assert_eq!(delivery.allowed_mentions.users, MentionUsers::Disabled);
    assert!(flavor.calls().is_empty());
}

#[test]
fn test_final_warning_balanced_pool_adds_flavor_no_ping() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 200,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert!(delivery.content.contains("WARNING LINE"));
    assert!(!delivery.content.contains("<@"));
    assert_eq!(delivery.allowed_mentions.users, MentionUsers::Disabled);
    assert_eq!(flavor.calls()[0].underdog_side, None);
}

#[test]
fn test_final_warning_lopsided_pings_underdog_team() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    for id in 1001..=1005 {
        assert!(delivery.content.contains(&format!("<@{id}>")));
    }
    assert!(!delivery.content.contains("<@2001>"));
    assert_eq!(scoped_ids(delivery), (1001..=1005).collect());
    assert_eq!(flavor.calls()[0].underdog_side, Some(UnderdogSide::Radiant));
}

#[test]
fn test_final_warning_just_below_threshold_no_ping() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 399,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert_eq!(plan.underdog_side, None);
    assert!(!delivery.content.contains("<@"));
    assert_eq!(delivery.allowed_mentions.users, MentionUsers::Disabled);
}

#[test]
fn test_final_warning_exactly_at_threshold_pings() {
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 400,
        },
        None,
    );
    assert_eq!(scoped_ids(channel_delivery(&plan)), (1001..=1005).collect());
}

#[test]
fn test_final_warning_one_sided_pool_pings_empty_side() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 0,
            dire: 300,
        },
        Some(&flavor),
    );
    assert_eq!(plan.underdog_side, Some(UnderdogSide::Radiant));
    assert!(channel_delivery(&plan).content.contains("<@1001>"));
    assert_eq!(scoped_ids(channel_delivery(&plan)), (1001..=1005).collect());
}

#[test]
fn test_final_warning_empty_pool_flavors_without_ping() {
    let flavor = FakeFlavor::responding("WARNING LINE");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals::default(),
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert!(delivery.content.contains("WARNING LINE"));
    assert!(!delivery.content.contains("<@"));
    assert_eq!(delivery.allowed_mentions.users, MentionUsers::Disabled);
    assert_eq!(flavor.calls()[0].underdog_side, None);
}

#[test]
fn test_final_warning_filters_non_real_player_ids() {
    let mut state = pending_state();
    state.radiant_team_ids = vec![3001, 0, -5, 3002];
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &state,
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        None,
    );
    let delivery = channel_delivery(&plan);
    assert!(delivery.content.contains("<@3001>"));
    assert!(delivery.content.contains("<@3002>"));
    assert!(!delivery.content.contains("<@0>"));
    assert!(!delivery.content.contains("<@-5>"));
    assert_eq!(scoped_ids(delivery), BTreeSet::from([3001, 3002]));
}

#[test]
fn test_last_call_unchanged_even_when_lopsided() {
    let flavor = FakeFlavor::responding("ANNOUNCER LINE");
    let plan = reminder_plan(
        ReminderType::LastCall,
        false,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert!(delivery.content.contains("Last call"));
    assert!(delivery.content.contains("ANNOUNCER LINE"));
    assert!(!delivery.content.contains("<@"));
    assert_eq!(delivery.allowed_mentions.users, MentionUsers::Disabled);
    assert_eq!(flavor.calls()[0].kind, FlavorKind::LastCall);
}

#[test]
fn test_final_warning_ping_cannot_mass_mention() {
    let flavor = FakeFlavor::responding("@everyone @here the longshots need backers!");
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        Some(&flavor),
    );
    let delivery = channel_delivery(&plan);
    assert!(delivery.content.contains("@everyone"));
    assert!(!delivery.allowed_mentions.parse_everyone_or_roles());
    assert_eq!(scoped_ids(delivery), (1001..=1005).collect());
}

#[test]
fn test_final_warning_thread_reply_also_pings_scoped() {
    let info = ShuffleMessageInfo {
        channel_id: Some(111),
        thread_message_id: Some(222),
        thread_id: Some(333),
        origin_channel_id: Some(111),
    };
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &info,
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        None,
    );
    let thread_delivery = plan
        .deliveries
        .iter()
        .find(|delivery| {
            matches!(
                delivery.destination,
                ReminderDestination::ThreadReply { .. }
            )
        })
        .expect("thread delivery");
    assert_eq!(
        thread_delivery.destination,
        ReminderDestination::ThreadReply {
            thread_id: 333,
            parent_message_id: 222,
            use_partial_message: true,
        }
    );
    assert_eq!(scoped_ids(thread_delivery), (1001..=1005).collect());
    assert!(thread_delivery.content.contains("<@1001>"));
}

#[test]
fn test_final_warning_pings_even_without_flavor_service() {
    let plan = reminder_plan(
        ReminderType::Warning,
        true,
        &pending_state(),
        &message_info(),
        PotTotals {
            radiant: 100,
            dire: 500,
        },
        None,
    );
    let delivery = channel_delivery(&plan);
    assert!(!delivery.content.contains("WARNING LINE"));
    assert!(delivery.content.contains("<@1001>"));
    assert_eq!(scoped_ids(delivery), (1001..=1005).collect());
}
