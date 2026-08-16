use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::convert::Infallible;

use crate::trivia_questions::Difficulty;

use super::*;

#[derive(Debug, Default)]
struct SequenceEntropy {
    units: VecDeque<f64>,
}

impl SequenceEntropy {
    fn first() -> Self {
        Self {
            units: VecDeque::from([0.0; 8]),
        }
    }
}

impl LootEntropy for SequenceEntropy {
    fn unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(0.0)
    }

    fn advance(&mut self, minimum: i64, _maximum: i64) -> i64 {
        minimum
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedSettlement {
    user_id: i64,
    guild_id: i64,
    delta: i64,
    category: String,
    correct: bool,
    timed_out: bool,
    metadata_json: String,
    reason: &'static str,
}

#[derive(Debug, Default)]
struct FakeEconomy {
    active_ids: RefCell<Vec<i64>>,
    active_request: RefCell<Option<(i64, i64)>>,
    settlements: RefCell<Vec<RecordedSettlement>>,
    fail_settlement: Cell<bool>,
}

impl FakeEconomy {
    fn with_active(ids: impl IntoIterator<Item = i64>) -> Self {
        Self {
            active_ids: RefCell::new(ids.into_iter().collect()),
            ..Self::default()
        }
    }
}

impl DigBonusEconomyPort for FakeEconomy {
    fn recently_active_player_ids(
        &self,
        guild_id: i64,
        activity_days: i64,
    ) -> Result<Vec<i64>, DigBonusPortError> {
        *self.active_request.borrow_mut() = Some((guild_id, activity_days));
        Ok(self.active_ids.borrow().clone())
    }

    fn settle_trivia(
        &self,
        request: TriviaSettlementRequest<'_>,
    ) -> Result<i64, DigBonusPortError> {
        if self.fail_settlement.get() {
            return Err(DigBonusPortError::Backend("db unavailable".to_owned()));
        }
        self.settlements.borrow_mut().push(RecordedSettlement {
            user_id: request.user_id,
            guild_id: request.guild_id,
            delta: request.delta,
            category: request.category.to_owned(),
            correct: request.correct,
            timed_out: request.timed_out,
            metadata_json: request.metadata_json(),
            reason: request.reason(),
        });
        Ok(105 + request.delta)
    }
}

#[derive(Debug)]
struct FakePackagePort {
    request: RefCell<Option<FreePackageRequest>>,
    result: RefCell<Result<PackageActivation, DigBonusPortError>>,
}

impl FakePackagePort {
    fn success(games_added: i64, games_remaining: i64) -> Self {
        Self {
            request: RefCell::new(None),
            result: RefCell::new(Ok(PackageActivation {
                games_added,
                games_remaining,
            })),
        }
    }

    fn failure() -> Self {
        Self {
            request: RefCell::new(None),
            result: RefCell::new(Err(DigBonusPortError::Backend(
                "package service unavailable".to_owned(),
            ))),
        }
    }
}

impl PackageDealBonusPort for FakePackagePort {
    fn create_free_package(
        &self,
        request: FreePackageRequest,
    ) -> Result<PackageActivation, DigBonusPortError> {
        *self.request.borrow_mut() = Some(request);
        self.result.borrow().clone()
    }
}

#[derive(Debug, Default)]
struct FakeEditor {
    updates: Vec<MessageUpdate>,
    error: Option<MessageEditError>,
}

impl MessageEditor for FakeEditor {
    fn edit(&mut self, update: &MessageUpdate) -> Result<(), MessageEditError> {
        self.updates.push(update.clone());
        match self.error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn question() -> TriviaQuestion {
    TriviaQuestion {
        text: "Which hero says this?".to_owned(),
        options: ["Axe", "Bane", "Chen", "Drow Ranger"]
            .map(str::to_owned)
            .to_vec(),
        correct_index: 1,
        difficulty: Difficulty::Easy,
        image_url: None,
        category: "hero_quote",
        explanation: Some("It is Bane.".to_owned()),
    }
}

fn candidates() -> Vec<GuildMember> {
    (2..=5)
        .map(|id| GuildMember {
            id,
            display_name: format!("Player {id}"),
            bot: false,
        })
        .collect()
}

fn trivia_session(correct_jc: i64) -> DigTriviaSession {
    DigTriviaSession::new(1, 99, question(), correct_jc)
}

#[test]
fn test_bonus_roll_uses_mutually_exclusive_one_percent_bands() {
    assert_eq!(pick_dig_bonus(0.0), Some(DigBonus::Wheel));
    assert_eq!(pick_dig_bonus(0.009_999), Some(DigBonus::Wheel));
    assert_eq!(pick_dig_bonus(0.01), Some(DigBonus::PackageDeal));
    assert_eq!(pick_dig_bonus(0.019_999), Some(DigBonus::PackageDeal));
    assert_eq!(pick_dig_bonus(0.02), Some(DigBonus::Trivia));
    assert_eq!(pick_dig_bonus(0.029_999), Some(DigBonus::Trivia));
    assert_eq!(pick_dig_bonus(0.03), None);
    assert_eq!(pick_dig_bonus(0.999_999), None);
}

#[test]
fn test_player_service_exposes_recently_active_package_pool() {
    let economy = FakeEconomy::with_active([2, 3]);
    assert_eq!(recently_active_package_pool(&economy, 99).unwrap(), [2, 3]);
    assert_eq!(*economy.active_request.borrow(), Some((99, 14)));
}

#[test]
fn test_package_candidates_exclude_digger_and_require_four_choices() {
    let players = (1..=6)
        .map(|id| GuildMember {
            id,
            display_name: format!("Player {id}"),
            bot: false,
        })
        .collect::<Vec<_>>();
    let selected = choose_package_candidates(&players, 1, &mut SequenceEntropy::first());
    assert_eq!(
        selected.iter().map(|member| member.id).collect::<Vec<_>>(),
        [2, 3, 4, 5]
    );
    assert!(selected.iter().all(|member| member.id != 1));
    assert!(choose_package_candidates(&players[..4], 1, &mut SequenceEntropy::first()).is_empty());
}

#[test]
fn test_package_deal_view_creates_free_three_game_deal() {
    let port = FakePackagePort::success(3, 3);
    let mut session = PackageDealSession::new(1, 99, candidates());
    let update = session.select_partner(2, &port);

    assert_eq!(
        *port.request.borrow(),
        Some(FreePackageRequest {
            guild_id: 99,
            buyer_id: 1,
            partner_id: 2,
        })
    );
    let content = update.content.unwrap();
    assert!(content.contains("unearthed foreman's contract"));
    assert!(content.contains("signed with **Player 2**"));
    assert!(update.clear_view);
}

#[test]
fn test_package_deal_activation_failure_keeps_mine_framing() {
    let mut session = PackageDealSession::new(1, 99, candidates());
    let update = session.select_partner(2, &FakePackagePort::failure());
    let content = update.content.unwrap();
    assert!(content.contains("unearthed foreman's contract"));
    assert!(content.contains("could not be activated"));
    assert!(update.clear_view);
}

#[test]
fn test_package_deal_confirmation_reports_extension_total() {
    let mut session = PackageDealSession::new(1, 99, candidates());
    let update = session.select_partner(2, &FakePackagePort::success(2, 10));
    let content = update.content.unwrap();
    assert!(content.contains("2 games added"));
    assert!(content.contains("10 games remaining"));
}

#[test]
fn test_package_deal_timeout_disables_expired_choices() {
    let mut session = PackageDealSession::new(1, 99, candidates());
    let update = session.on_timeout().unwrap();
    assert!(session.choices_disabled);
    assert_eq!(
        update.content.as_deref(),
        Some("*The unearthed foreman's contract crumbled to dust in the mine.*")
    );
    assert!(update.embed.is_none());
    assert!(!update.clear_view);
}

#[test]
fn test_package_deal_ignores_a_deleted_confirmation_message() {
    let mut session = PackageDealSession::new(1, 99, candidates());
    let update = session.select_partner(2, &FakePackagePort::success(3, 3));
    let mut editor = FakeEditor {
        error: Some(MessageEditError::NotFound),
        ..FakeEditor::default()
    };
    assert_eq!(edit_message_best_effort(&mut editor, &update), Ok(()));
    assert_eq!(editor.updates.len(), 1);
}

#[test]
fn test_package_deal_view_only_accepts_the_digger() {
    let session = PackageDealSession::new(1, 99, candidates());
    assert_eq!(session.candidates.len(), 4);
    assert!(session.interaction_allowed(1));
    assert!(!session.interaction_allowed(9));
}

#[test]
fn test_dig_trivia_correct_answer_applies_dig_reward_policy_with_audit_context() {
    let economy = FakeEconomy::default();
    let mut session = trivia_session(scale_positive_dig_jc(15));
    let update = session.answer(1, &economy);

    assert_eq!(economy.settlements.borrow().len(), 1);
    let settlement = &economy.settlements.borrow()[0];
    assert_eq!(settlement.user_id, 1);
    assert_eq!(settlement.guild_id, 99);
    assert_eq!(settlement.delta, 10);
    assert_eq!(settlement.category, "hero_quote");
    assert_eq!(settlement.reason, "dig bonus trivia correct answer");
    assert_eq!(
        settlement.metadata_json,
        r#"{"correct":true,"timed_out":false,"gross_jc":15,"reward_multiplier":0.65}"#
    );
    let embed = update.embed.unwrap();
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Correct! +10 JC")
    );
    assert!(
        embed
            .description
            .as_deref()
            .unwrap()
            .contains("rune tablet reveals the correct answer")
    );
}

#[test]
fn test_dig_trivia_answer_surfaces_settlement_failure() {
    let economy = FakeEconomy::default();
    economy.fail_settlement.set(true);
    let update = trivia_session(10).answer(1, &economy);
    let embed = update.embed.unwrap();
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Settlement Error")
    );
    assert!(embed.description.unwrap().contains("mine's rune tablet"));
    assert!(update.clear_view);
}

#[test]
fn test_dig_trivia_answer_ignores_a_deleted_message() {
    let economy = FakeEconomy::default();
    let update = trivia_session(10).answer(1, &economy);
    let mut editor = FakeEditor {
        error: Some(MessageEditError::NotFound),
        ..FakeEditor::default()
    };
    assert_eq!(edit_message_best_effort(&mut editor, &update), Ok(()));
}

#[test]
fn test_dig_trivia_wrong_answer_loses_five_jc() {
    let economy = FakeEconomy::default();
    let update = trivia_session(10).answer(0, &economy);
    let settlement = &economy.settlements.borrow()[0];
    assert_eq!(settlement.delta, -5);
    assert_eq!(
        settlement.metadata_json,
        r#"{"correct":false,"timed_out":false}"#
    );
    let embed = update.embed.unwrap();
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Wrong! -5 JC")
    );
    let description = embed.description.unwrap();
    assert!(description.contains("rune tablet reveals the correct answer"));
    assert!(description.contains("Bane"));
}

#[test]
fn test_dig_trivia_timeout_loses_five_jc() {
    let economy = FakeEconomy::default();
    let update = trivia_session(10).on_timeout(&economy).unwrap();
    let settlement = &economy.settlements.borrow()[0];
    assert_eq!(settlement.delta, -5);
    assert_eq!(
        settlement.metadata_json,
        r#"{"correct":false,"timed_out":true}"#
    );
    let embed = update.embed.unwrap();
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Time's up! -5 JC")
    );
    assert!(
        embed
            .description
            .unwrap()
            .contains("rune tablet reveals the correct answer")
    );
}

#[test]
fn test_dig_trivia_timeout_surfaces_settlement_failure() {
    let economy = FakeEconomy::default();
    economy.fail_settlement.set(true);
    let update = trivia_session(10).on_timeout(&economy).unwrap();
    let embed = update.embed.unwrap();
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Settlement Error")
    );
    assert!(embed.description.unwrap().contains("mine's rune tablet"));
    assert!(update.clear_view);
}

#[test]
fn test_dig_trivia_timeout_ignores_a_deleted_message() {
    let economy = FakeEconomy::default();
    let update = trivia_session(10).on_timeout(&economy).unwrap();
    let mut editor = FakeEditor {
        error: Some(MessageEditError::NotFound),
        ..FakeEditor::default()
    };
    assert_eq!(edit_message_best_effort(&mut editor, &update), Ok(()));
}

#[derive(Debug, Default)]
struct FakeBonusDispatch {
    sent: Vec<DigBonus>,
    reports: Vec<String>,
    fail_send: bool,
    fail_report: bool,
}

impl BonusDispatchPort for FakeBonusDispatch {
    fn send_bonus(&mut self, bonus: DigBonus) -> Result<(), DigBonusPortError> {
        self.sent.push(bonus);
        if self.fail_send {
            Err(DigBonusPortError::Backend("bonus failed".to_owned()))
        } else {
            Ok(())
        }
    }

    fn report_partial_failure(&mut self, content: &str) -> Result<(), DigBonusPortError> {
        self.reports.push(content.to_owned());
        if self.fail_report {
            Err(DigBonusPortError::Backend("followup failed".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn test_maybe_send_dig_bonus_only_rolls_for_consumed_digs() {
    let mut port = FakeBonusDispatch::default();
    assert_eq!(
        maybe_send_dig_bonus(false, 0.0, &mut port),
        BonusDispatchOutcome::NotConsumed
    );
    assert!(port.sent.is_empty());
    assert_eq!(
        maybe_send_dig_bonus(true, 0.015, &mut port),
        BonusDispatchOutcome::Sent(DigBonus::PackageDeal)
    );
    assert_eq!(port.sent, [DigBonus::PackageDeal]);
}

#[test]
fn test_bonus_dispatch_failure_never_masks_a_successful_dig() {
    let mut port = FakeBonusDispatch {
        fail_send: true,
        fail_report: true,
        ..FakeBonusDispatch::default()
    };
    assert_eq!(
        maybe_send_dig_bonus(true, 0.0, &mut port),
        BonusDispatchOutcome::FailedButDigPreserved(DigBonus::Wheel)
    );
}

#[test]
fn test_bonus_dispatch_failure_does_not_claim_reward_was_rolled_back() {
    let mut port = FakeBonusDispatch {
        fail_send: true,
        ..FakeBonusDispatch::default()
    };
    maybe_send_dig_bonus(true, 0.0, &mut port);
    assert_eq!(port.reports.len(), 1);
    assert!(port.reports[0].contains("may already be recorded"));
}

#[derive(Debug, Default)]
struct FakeWheel {
    bonus_spins: usize,
}

impl WheelBonusPort for FakeWheel {
    fn trigger_bonus_spin(&mut self) -> Result<(), DigBonusPortError> {
        self.bonus_spins += 1;
        Ok(())
    }
}

#[test]
fn test_send_dig_bonus_routes_wheel_to_bonus_mode() {
    let mut wheel = FakeWheel::default();
    send_wheel_bonus(&mut wheel).unwrap();
    assert_eq!(wheel.bonus_spins, 1);
}

fn guild_members(range: std::ops::RangeInclusive<i64>) -> Vec<GuildMember> {
    range
        .map(|id| GuildMember {
            id,
            display_name: format!("Player {id}"),
            bot: false,
        })
        .collect()
}

#[test]
fn test_send_dig_bonus_offers_four_active_package_players() {
    let economy = FakeEconomy::with_active(1..=6);
    let presentation = prepare_package_bonus(
        &economy,
        99,
        1,
        &guild_members(1..=6),
        &mut SequenceEntropy::first(),
    )
    .unwrap();
    let PackageBonusPresentation::Offer { embed, session } = presentation else {
        panic!("expected package offer");
    };
    assert_eq!(session.candidates.len(), 4);
    assert_eq!(session.buyer_id, 1);
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed in the Mine — Package Deal")
    );
    let description = embed.description.unwrap();
    assert!(description.contains("pickaxe"));
    assert!(description.contains("buried"));
    assert!(description.contains("free Package Deal for **3 games**"));
}

#[test]
fn test_send_dig_bonus_insufficient_package_candidates_keeps_mine_framing() {
    let economy = FakeEconomy::with_active(1..=4);
    let presentation = prepare_package_bonus(
        &economy,
        99,
        1,
        &guild_members(1..=4),
        &mut SequenceEntropy::first(),
    )
    .unwrap();
    let PackageBonusPresentation::InsufficientCandidates(update) = presentation else {
        panic!("expected insufficient-candidate notice");
    };
    let content = update.content.unwrap();
    assert!(content.contains("pickaxe unearthed a foreman's Package Deal contract"));
    assert!(content.contains("not four eligible active players to sign it"));
    assert!(update.ephemeral);
}

#[test]
fn test_send_dig_bonus_posts_one_standalone_trivia_question() {
    let question = question();
    let (embed, session) = prepare_trivia_bonus(1, 99, question.clone(), 15);
    assert_eq!(session.question, question);
    assert_eq!(
        embed.title.as_deref(),
        Some("⛏️ Unearthed in the Mine — Dota 2 Trivia")
    );
    let description = embed.description.unwrap();
    assert!(description.contains("stone tablet"));
    assert!(description.contains("mine wall"));
    assert!(description.ends_with(&question.text));
    let footer = embed.footer.unwrap();
    assert!(footer.contains("+10 JC"));
    assert!(footer.contains("-5 JC"));
}

#[test]
fn test_send_dig_trivia_freezes_active_edict_reward_for_display_and_settlement() {
    let economy = FakeEconomy::default();
    let (embed, mut session) = prepare_trivia_bonus(1, 99, question(), 30);
    assert!(embed.footer.unwrap().contains("+20 JC"));
    let result = session.answer(1, &economy);
    assert_eq!(
        result.embed.unwrap().title.as_deref(),
        Some("⛏️ Unearthed Rune Tablet — Correct! +20 JC")
    );
    assert_eq!(economy.settlements.borrow()[0].delta, 20);
}

#[derive(Debug, Default)]
struct FakeResultDispatch {
    calls: Vec<&'static str>,
}

impl DigResultDispatchPort for FakeResultDispatch {
    type Error = Infallible;

    fn send_normal(&mut self) -> Result<(), Self::Error> {
        self.calls.push("normal");
        Ok(())
    }

    fn send_boss(&mut self) -> Result<(), Self::Error> {
        self.calls.push("boss");
        Ok(())
    }

    fn send_boon(&mut self) -> Result<(), Self::Error> {
        self.calls.push("boon");
        Ok(())
    }

    fn send_choice(&mut self) -> Result<(), Self::Error> {
        self.calls.push("choice");
        Ok(())
    }

    fn send_bonus_after_ui(&mut self, dig_consumed: bool) -> Result<(), Self::Error> {
        assert!(dig_consumed);
        self.calls.push("bonus");
        Ok(())
    }
}

fn dispatch_shape(
    boss_encounter: bool,
    event_complexity: Option<EventComplexity>,
) -> FakeResultDispatch {
    let mut port = FakeResultDispatch::default();
    dispatch_dig_result(
        DigDispatchResult {
            boss_encounter,
            event_complexity,
            dig_consumed: true,
        },
        &mut port,
    )
    .unwrap();
    port
}

#[test]
fn test_dig_result_dispatch_runs_bonus_after_existing_ui_normal() {
    assert_eq!(dispatch_shape(false, None).calls, ["normal", "bonus"]);
}

#[test]
fn test_dig_result_dispatch_runs_bonus_after_existing_ui_boss() {
    assert_eq!(dispatch_shape(true, None).calls, ["boss", "bonus"]);
}

#[test]
fn test_dig_result_dispatch_runs_bonus_after_existing_ui_boon() {
    assert_eq!(
        dispatch_shape(false, Some(EventComplexity::Boon)).calls,
        ["boon", "bonus"]
    );
}

#[test]
fn test_dig_result_dispatch_runs_bonus_after_existing_ui_choice() {
    assert_eq!(
        dispatch_shape(false, Some(EventComplexity::Choice)).calls,
        ["choice", "bonus"]
    );
}

#[test]
fn test_paid_dig_dispatches_bonus_for_the_consumed_paid_result() {
    let mut port = FakeResultDispatch::default();
    dispatch_paid_dig_bonus(true, &mut port).unwrap();
    assert_eq!(port.calls, ["bonus"]);
}
