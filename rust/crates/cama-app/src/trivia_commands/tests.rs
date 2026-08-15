use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};

use crate::trivia_questions::Difficulty;

use super::*;

const GUILD: GuildId = 9_001;
const USER: DiscordId = 100_001;
const NOW: i64 = 1_800_000_000;

fn question() -> TriviaQuestion {
    TriviaQuestion {
        text: "Which hero says this?".to_owned(),
        options: vec![
            "Axe".to_owned(),
            "Crystal Maiden".to_owned(),
            "Pudge".to_owned(),
            "Invoker".to_owned(),
        ],
        correct_index: 1,
        difficulty: Difficulty::Easy,
        image_url: None,
        category: "test",
        explanation: None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventBehavior {
    Missing,
    Half,
    Fail,
}

#[derive(Debug)]
struct RewardState {
    balance: i64,
    scale_calls: Vec<i64>,
    event_calls: Vec<(GuildId, i64, ThreadId)>,
    credits: Vec<(DiscordId, GuildId, i64)>,
}

#[derive(Debug)]
struct RewardPort {
    event: EventBehavior,
    state: Mutex<RewardState>,
}

impl RewardPort {
    fn new(balance: i64, event: EventBehavior) -> Self {
        Self {
            event,
            state: Mutex::new(RewardState {
                balance,
                scale_calls: Vec::new(),
                event_calls: Vec::new(),
                credits: Vec::new(),
            }),
        }
    }

    fn balance(&self) -> i64 {
        self.state.lock().expect("reward state").balance
    }
}

impl TriviaRewardPort for RewardPort {
    fn scale_minigame_reward(&self, amount: i64) -> i64 {
        self.state
            .lock()
            .expect("reward state")
            .scale_calls
            .push(amount);
        amount
    }

    fn adjust_daily_event(&self, guild_id: GuildId, amount: i64) -> Result<Option<i64>, String> {
        self.state.lock().expect("reward state").event_calls.push((
            guild_id,
            amount,
            thread::current().id(),
        ));
        match self.event {
            EventBehavior::Missing => Ok(None),
            EventBehavior::Half => Ok(Some(amount / 2)),
            EventBehavior::Fail => Err("event database unavailable".to_owned()),
        }
    }

    fn credit_reward(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        amount: i64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("reward state");
        state.balance += amount;
        state.credits.push((discord_id, guild_id, amount));
        Ok(())
    }
}

#[derive(Default)]
struct ImmediateTransitionPort {
    edit_count: Mutex<usize>,
}

impl TriviaTransitionPort for ImmediateTransitionPort {
    fn delete_previous(&self, _message_id: Option<MessageId>) -> Result<(), String> {
        Ok(())
    }

    fn edit_correct(&self, _message_id: Option<MessageId>) -> Result<(), String> {
        *self.edit_count.lock().expect("edit count") += 1;
        Ok(())
    }

    fn edit_game_over(
        &self,
        _message_id: Option<MessageId>,
        _timed_out: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn generate_next(
        &self,
        _streak: i64,
        _recent_categories: &[&'static str],
    ) -> Result<Option<TriviaQuestion>, String> {
        Ok(None)
    }

    fn prepare_asset(&self, _question: &TriviaQuestion) -> Result<Option<PreparedAsset>, String> {
        Ok(None)
    }

    fn discard_asset(&self, _asset: PreparedAsset) {}

    fn send_next(&self, _prepared: PreparedQuestion) -> Result<MessageId, String> {
        Err("no next question".to_owned())
    }
}

#[test]
fn test_correct_answer_pays_base_plus_streak_bonuses() {
    assert_eq!(jc_for_streak(1), 1);
    assert_eq!(jc_for_streak(2), 1);
    assert_eq!(jc_for_streak(3), 2);
    assert_eq!(jc_for_streak(6), 2);
    assert_eq!(jc_for_streak(7), 1);
    assert_eq!(jc_for_streak(10), 3);
    assert_eq!(jc_for_streak(14), 2);
}

#[test]
fn test_correct_answer_awards_one_jc_before_streak_bonus() {
    let port = RewardPort::new(3, EventBehavior::Missing);
    let mut session = TriviaSession::new(USER, GUILD);
    let award = award_correct_answer(&port, &mut session, "test", 1);
    assert_eq!(award.awarded_jc, 1);
    assert_eq!((session.streak, session.total_jc), (1, 1));
    assert_eq!(port.balance(), 4);
}

#[test]
fn test_correct_answer_awards_streak_bonus_on_milestone() {
    let port = RewardPort::new(3, EventBehavior::Missing);
    let mut session = TriviaSession::new(USER, GUILD);
    session.streak = 2;
    let award = award_correct_answer(&port, &mut session, "test", 1);
    assert_eq!(
        (award.base_reward, award.streak_bonus, award.awarded_jc),
        (1, 1, 2)
    );
    assert_eq!((session.streak, session.total_jc), (3, 2));
    assert_eq!(port.balance(), 5);
}

#[test]
fn test_correct_answer_keeps_larger_payout_at_neutral_scale() {
    let port = RewardPort::new(3, EventBehavior::Missing);
    let mut session = TriviaSession::new(USER, GUILD);
    session.streak = 9;
    let award = award_correct_answer(&port, &mut session, "test", 1);
    assert_eq!(
        (award.streak, award.streak_bonus, award.awarded_jc),
        (10, 2, 3)
    );
    assert_eq!(session.total_jc, 3);
    assert_eq!(port.balance(), 6);
}

#[test]
fn test_daily_event_runs_after_single_central_scale() {
    let caller_thread = thread::current().id();
    let port = RewardPort::new(3, EventBehavior::Half);
    let mut session = TriviaSession::new(USER, GUILD);
    let award = award_correct_answer(&port, &mut session, "test", 10);
    let state = port.state.lock().expect("reward state");
    assert_eq!(state.scale_calls, [10]);
    assert_eq!(state.event_calls.len(), 1);
    assert_eq!(
        (state.event_calls[0].0, state.event_calls[0].1),
        (GUILD, 10)
    );
    assert_ne!(state.event_calls[0].2, caller_thread);
    assert_eq!(award.awarded_jc, 5);
    assert_eq!((session.total_jc, state.balance), (5, 8));
}

#[test]
fn test_daily_event_failure_does_not_strand_correct_answer() {
    let reward = RewardPort::new(3, EventBehavior::Fail);
    let mut session = TriviaSession::new(USER, GUILD);
    session.current_message_id = Some(111);
    let award = award_correct_answer(&reward, &mut session, "test", 1);
    assert_eq!(
        (award.awarded_jc, session.streak, session.total_jc),
        (1, 1, 1)
    );
    assert_eq!(reward.balance(), 4);

    let transition = ImmediateTransitionPort::default();
    assert_eq!(
        run_correct_transition(&transition, &mut session).expect("finish transition"),
        CorrectTransition::SessionEnded
    );
    assert_eq!(*transition.edit_count.lock().expect("edit count"), 1);
    assert!(!session.active);
}

#[test]
fn test_missing_bot_or_event_service_is_neutral() {
    let port = RewardPort::new(0, EventBehavior::Missing);
    let mut session = TriviaSession::new(USER, GUILD);
    let award = award_correct_answer(&port, &mut session, "test", 8);
    assert_eq!((award.scaled_reward, award.awarded_jc), (8, 8));
    assert_eq!(port.balance(), 8);
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredSession {
    discord_id: DiscordId,
    guild_id: GuildId,
    streak: i64,
    jc_earned: i64,
    played_at: i64,
}

#[derive(Clone, Debug, Default)]
struct MemorySessionPort {
    last_claims: BTreeMap<(DiscordId, GuildId), i64>,
    resets: usize,
    records: Vec<StoredSession>,
}

impl TriviaSessionPort for MemorySessionPort {
    fn try_claim(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<bool, String> {
        let key = (discord_id, guild_id);
        if self
            .last_claims
            .get(&key)
            .is_some_and(|last| *last >= now - cooldown_seconds)
        {
            return Ok(false);
        }
        self.last_claims.insert(key, now);
        Ok(true)
    }

    fn reset_cooldown(&mut self, discord_id: DiscordId, guild_id: GuildId) -> Result<bool, String> {
        self.resets += 1;
        Ok(self.last_claims.remove(&(discord_id, guild_id)).is_some())
    }

    fn record_session(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        streak: i64,
        jc_earned: i64,
        played_at: i64,
    ) -> Result<(), String> {
        self.records.push(StoredSession {
            discord_id,
            guild_id,
            streak,
            jc_earned,
            played_at,
        });
        Ok(())
    }
}

#[test]
fn test_end_session_awaits_leaderboard_write() {
    let mut service = TriviaCommandService::new(MemorySessionPort::default());
    service
        .start_session_with_defer(USER, GUILD, NOW, DEFAULT_COOLDOWN_SECONDS, true, || true)
        .expect("start admin session");
    let session = service.session_mut(USER, GUILD).expect("active session");
    session.streak = 5;
    session.total_jc = 5;
    let ended = service
        .end_session(USER, GUILD, NOW + 30)
        .expect("end and persist session");
    assert!(!ended.active);
    assert!(service.session(USER, GUILD).is_none());
    assert_eq!(
        service.persistence().records,
        [StoredSession {
            discord_id: USER,
            guild_id: GUILD,
            streak: 5,
            jc_earned: 5,
            played_at: NOW + 30,
        }]
    );
}

#[test]
fn test_failed_defer_releases_claimed_cooldown() {
    let mut service = TriviaCommandService::new(MemorySessionPort::default());
    assert_eq!(
        service
            .start_session_with_defer(USER, GUILD, NOW, DEFAULT_COOLDOWN_SECONDS, false, || false,),
        Err(TriviaCommandError::DeferFailed)
    );
    assert!(service.session(USER, GUILD).is_none());
    assert!(service.persistence().last_claims.is_empty());
    assert_eq!(service.persistence().resets, 1);
    service
        .start_session_with_defer(USER, GUILD, NOW, DEFAULT_COOLDOWN_SECONDS, false, || true)
        .expect("claim is immediately available again");
}

#[derive(Debug, Default)]
struct ProbeState {
    started: BTreeSet<&'static str>,
    released: BTreeSet<&'static str>,
    order: Vec<&'static str>,
    sent: bool,
    discarded_assets: usize,
}

#[derive(Debug)]
struct TransitionProbe {
    state: Mutex<ProbeState>,
    changed: Condvar,
    edit_error: bool,
    delete_error: bool,
}

impl TransitionProbe {
    fn new(edit_error: bool, delete_error: bool) -> Self {
        Self {
            state: Mutex::new(ProbeState::default()),
            changed: Condvar::new(),
            edit_error,
            delete_error,
        }
    }

    fn stage(&self, name: &'static str) {
        let mut state = self.state.lock().expect("probe state");
        state.started.insert(name);
        self.changed.notify_all();
        while !state.released.contains(name) {
            state = self.changed.wait(state).expect("probe wait");
        }
        state.order.push(match name {
            "delete" => "delete_done",
            "edit" => "edit_done",
            "generate" => "generate_done",
            "prepare" => "prepare_done",
            _ => "unknown_done",
        });
        self.changed.notify_all();
    }

    fn wait_started(&self, names: &[&'static str]) {
        let mut state = self.state.lock().expect("probe state");
        while names.iter().any(|name| !state.started.contains(name)) {
            state = self.changed.wait(state).expect("probe wait");
        }
    }

    fn release(&self, name: &'static str) {
        let mut state = self.state.lock().expect("probe state");
        state.released.insert(name);
        self.changed.notify_all();
    }

    fn sent(&self) -> bool {
        self.state.lock().expect("probe state").sent
    }
}

impl TriviaTransitionPort for TransitionProbe {
    fn delete_previous(&self, _message_id: Option<MessageId>) -> Result<(), String> {
        self.stage("delete");
        if self.delete_error {
            Err("best-effort delete failed".to_owned())
        } else {
            Ok(())
        }
    }

    fn edit_correct(&self, _message_id: Option<MessageId>) -> Result<(), String> {
        self.stage("edit");
        if self.edit_error {
            Err("edit failed".to_owned())
        } else {
            Ok(())
        }
    }

    fn edit_game_over(
        &self,
        _message_id: Option<MessageId>,
        _timed_out: bool,
    ) -> Result<(), String> {
        self.stage("edit");
        Ok(())
    }

    fn generate_next(
        &self,
        streak: i64,
        recent_categories: &[&'static str],
    ) -> Result<Option<TriviaQuestion>, String> {
        assert_eq!(streak, 1);
        assert_eq!(recent_categories, ["test"]);
        self.stage("generate");
        Ok(Some(question()))
    }

    fn prepare_asset(&self, _question: &TriviaQuestion) -> Result<Option<PreparedAsset>, String> {
        self.stage("prepare");
        Ok(Some(PreparedAsset { id: 77 }))
    }

    fn discard_asset(&self, _asset: PreparedAsset) {
        self.state.lock().expect("probe state").discarded_assets += 1;
    }

    fn send_next(&self, _prepared: PreparedQuestion) -> Result<MessageId, String> {
        let mut state = self.state.lock().expect("probe state");
        state.order.push("next_send");
        state.sent = true;
        self.changed.notify_all();
        Ok(222)
    }
}

fn transition_session() -> TriviaSession {
    let mut session = TriviaSession::new(USER, GUILD);
    session.streak = 1;
    session.recent_categories.push("test");
    session.current_message_id = Some(111);
    session.previous_message_id = Some(110);
    session
}

#[test]
fn test_correct_answer_overlaps_delete_edit_and_next_question_preparation() {
    let probe = Arc::new(TransitionProbe::new(false, false));
    let worker_probe = Arc::clone(&probe);
    let worker = thread::spawn(move || {
        let mut session = transition_session();
        let result = run_correct_transition(worker_probe.as_ref(), &mut session);
        (result, session)
    });
    probe.wait_started(&["delete", "edit", "generate"]);
    assert!(!probe.sent());
    assert!(!worker.is_finished());
    probe.release("generate");
    probe.wait_started(&["prepare"]);
    probe.release("prepare");
    probe.release("edit");
    assert!(!probe.sent());
    probe.release("delete");
    let (result, session) = worker.join().expect("transition worker");
    assert_eq!(
        result.expect("correct transition"),
        CorrectTransition::NextQuestion { message_id: 222 }
    );
    let state = probe.state.lock().expect("probe state");
    assert!(
        state.order.iter().position(|item| *item == "edit_done")
            < state.order.iter().position(|item| *item == "next_send")
    );
    assert!(
        state.order.iter().position(|item| *item == "prepare_done")
            < state.order.iter().position(|item| *item == "next_send")
    );
    assert_eq!(session.previous_message_id, Some(111));
    assert_eq!(session.current_message_id, Some(222));
    assert!(session.active);
}

#[test]
fn test_correct_edit_error_waits_for_cleanup_and_preparation_then_propagates() {
    let probe = Arc::new(TransitionProbe::new(true, false));
    let worker_probe = Arc::clone(&probe);
    let worker = thread::spawn(move || {
        let mut session = transition_session();
        let result = run_correct_transition(worker_probe.as_ref(), &mut session);
        (result, session)
    });
    probe.wait_started(&["delete", "edit", "generate"]);
    probe.release("edit");
    probe.release("generate");
    probe.wait_started(&["prepare"]);
    assert!(!worker.is_finished());
    probe.release("delete");
    probe.release("prepare");
    let (result, session) = worker.join().expect("transition worker");
    assert_eq!(
        result,
        Err(TriviaTransitionError::RequiredEdit(
            "edit failed".to_owned()
        ))
    );
    let state = probe.state.lock().expect("probe state");
    assert!(!state.sent);
    assert_eq!(state.discarded_assets, 1);
    assert_eq!(session.previous_message_id, Some(110));
    assert_eq!(session.current_message_id, Some(111));
    assert!(session.active);
}

#[test]
fn test_wrong_answer_overlaps_best_effort_delete_with_required_edit() {
    let probe = Arc::new(TransitionProbe::new(false, true));
    let worker_probe = Arc::clone(&probe);
    let worker = thread::spawn(move || {
        let mut session = transition_session();
        let result = run_wrong_transition(worker_probe.as_ref(), &mut session);
        (result, session)
    });
    probe.wait_started(&["delete", "edit"]);
    assert!(!worker.is_finished());
    probe.release("delete");
    probe.release("edit");
    let (result, session) = worker.join().expect("transition worker");
    result.expect("best-effort delete does not fail transition");
    assert!(!session.active);
}

#[test]
fn test_timeout_overlaps_delete_and_message_edit_before_session_end() {
    let probe = Arc::new(TransitionProbe::new(false, false));
    let worker_probe = Arc::clone(&probe);
    let worker = thread::spawn(move || {
        let mut session = transition_session();
        let result = run_timeout_transition(worker_probe.as_ref(), &mut session);
        (result, session)
    });
    probe.wait_started(&["delete", "edit"]);
    assert!(!worker.is_finished());
    probe.release("edit");
    assert!(!worker.is_finished());
    probe.release("delete");
    let (result, session) = worker.join().expect("transition worker");
    result.expect("timeout transition");
    assert!(session.answered);
    assert!(!session.active);
}
