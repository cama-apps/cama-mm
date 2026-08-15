use std::collections::{BTreeSet, VecDeque};

use super::*;
use crate::interaction_safety::SentPayload;

#[derive(Default)]
struct RecordingTransport {
    responses: Vec<ResponseEvent>,
    lifecycle: Vec<LifecycleEvent>,
    modal_fails: bool,
    defer_fails: bool,
    callback_fails: bool,
}

impl DeferPort for RecordingTransport {
    fn defer(&mut self, ephemeral: bool, thinking: bool) -> Result<(), TransportError> {
        if self.defer_fails {
            return Err(TransportError::discord("interaction expired"));
        }
        self.responses.push(ResponseEvent::Deferred {
            ephemeral,
            thinking,
        });
        Ok(())
    }
}

impl BossResponsePort for RecordingTransport {
    fn send_initial(&mut self, content: &str, ephemeral: bool) {
        self.responses.push(ResponseEvent::Initial {
            content: content.to_owned(),
            ephemeral,
        });
    }

    fn send_modal(&mut self, modal: ModalKind) -> Result<(), TransportError> {
        if self.modal_fails {
            return Err(TransportError::discord("interaction expired"));
        }
        self.responses.push(ResponseEvent::Modal(modal));
        Ok(())
    }

    fn send_followup(&mut self, content: &str, ephemeral: bool) {
        self.responses.push(ResponseEvent::Followup {
            content: content.to_owned(),
            ephemeral,
        });
    }
}

impl ResolutionPort for RecordingTransport {
    fn notify_resolved(&mut self, user_id: i64, guild_id: Option<i64>) -> Result<(), String> {
        if self.callback_fails {
            return Err("reminder unavailable".to_owned());
        }
        self.lifecycle
            .push(LifecycleEvent::Callback { user_id, guild_id });
        Ok(())
    }

    fn render(&mut self, plan: RenderPlan) {
        self.lifecycle.push(LifecycleEvent::Render(plan));
    }

    fn warn(&mut self, message: String) {
        self.lifecycle.push(LifecycleEvent::Warning(message));
    }
}

#[derive(Default)]
struct FailingEditable {
    edit_attempts: Vec<SentPayload>,
    channel_sends: Vec<SentPayload>,
}

impl EditableMessage for FailingEditable {
    fn edit(&mut self, payload: SentPayload) -> Result<(), TransportError> {
        self.edit_attempts.push(payload);
        Err(TransportError::discord("message token expired"))
    }

    fn send_to_channel(&mut self, payload: SentPayload) -> Result<String, TransportError> {
        self.channel_sends.push(payload);
        Ok("replacement-message".to_owned())
    }
}

#[derive(Default)]
struct FakeService {
    carried: Option<Result<Option<CarriedWager>, ServiceError>>,
    starts: VecDeque<Result<BossResult, ServiceError>>,
    resumes: VecDeque<Result<BossResult, ServiceError>>,
    carried_calls: usize,
    start_calls: Vec<(i64, Option<i64>, RiskTier, i64)>,
    resume_calls: Vec<(i64, Option<i64>, u16)>,
}

impl FakeService {
    fn with_start(result: BossResult) -> Self {
        Self {
            starts: VecDeque::from([Ok(result)]),
            ..Self::default()
        }
    }

    fn with_resume(result: BossResult) -> Self {
        Self {
            resumes: VecDeque::from([Ok(result)]),
            ..Self::default()
        }
    }
}

impl BossFightPort for FakeService {
    fn carried_wager(
        &mut self,
        _user_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<Option<CarriedWager>, ServiceError> {
        self.carried_calls += 1;
        self.carried.take().unwrap_or(Ok(None))
    }

    fn start_boss_duel(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
        risk_tier: RiskTier,
        wager: i64,
    ) -> Result<BossResult, ServiceError> {
        self.start_calls.push((user_id, guild_id, risk_tier, wager));
        self.starts
            .pop_front()
            .unwrap_or_else(|| Ok(BossResult::default()))
    }

    fn resume_boss_duel(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
        option_index: u16,
    ) -> Result<BossResult, ServiceError> {
        self.resume_calls.push((user_id, guild_id, option_index));
        self.resumes
            .pop_front()
            .unwrap_or_else(|| Ok(BossResult::default()))
    }
}

fn view(info: BossInfo, callback_attached: bool) -> BossEncounterView {
    BossEncounterView::new(42, Some(7), info, true, callback_attached)
}

fn prompt() -> PendingPrompt {
    PendingPrompt {
        title: "Choose".to_owned(),
        description: "React.".to_owned(),
        safe_option_index: 2,
        options: vec![DuelOption {
            option_index: 1,
            label: "Strike".to_owned(),
        }],
    }
}

fn pet(species_id: &str, pet_name: &str, species_name: &str, bonus_pct: i32) -> PetAssist {
    PetAssist {
        pet_name: pet_name.to_owned(),
        species_id: species_id.to_owned(),
        species_name: species_name.to_owned(),
        bonus_pct,
    }
}

fn shaped_result(kind: &str) -> BossResult {
    let mut result = BossResult::default();
    match kind {
        "regular-loss" => {
            result.won = false;
            result.payout = 0;
            result.jc_delta = -12;
            result.knockback = 8;
        }
        "regular-phase2" => result.phase2_incoming = true,
        "regular-phase3" => result.phase3_incoming = true,
        "pinnacle-win" => result.is_pinnacle = true,
        "pinnacle-loss" => {
            result.is_pinnacle = true;
            result.won = false;
            result.payout = 0;
            result.jc_delta = -12;
        }
        "pinnacle-phase2" => {
            result.is_pinnacle = true;
            result.phase2_incoming = true;
        }
        "pinnacle-phase3" => {
            result.is_pinnacle = true;
            result.phase3_incoming = true;
        }
        _ => {}
    }
    result
}

fn assert_callback_before_render(origin: ResolutionOrigin, kind: &str) {
    let mut transport = RecordingTransport::default();
    route_resolution(
        origin,
        shaped_result(kind),
        42,
        Some(7),
        true,
        &mut transport,
    );
    assert_eq!(
        transport.lifecycle.first(),
        Some(&LifecycleEvent::Callback {
            user_id: 42,
            guild_id: Some(7)
        })
    );
    assert_eq!(
        transport
            .lifecycle
            .iter()
            .filter(|event| matches!(event, LifecycleEvent::Callback { .. }))
            .count(),
        1
    );
    assert!(matches!(
        transport.lifecycle.get(1),
        Some(LifecycleEvent::Render(_))
    ));
}

fn assert_resume_shape(kind: &str) {
    let prompt = prompt();
    let mut view = BossDuelView::new(42, Some(7), &prompt, true);
    let mut service = FakeService::with_resume(shaped_result(kind));
    let mut transport = RecordingTransport::default();
    assert_eq!(
        view.submit(1, &mut service, &mut transport),
        SubmitOutcome::Resolved
    );
    assert_eq!(
        view.submit(1, &mut service, &mut transport),
        SubmitOutcome::Duplicate
    );
    assert_eq!(service.resume_calls, vec![(42, Some(7), 1)]);
    assert!(matches!(
        transport.lifecycle.as_slice(),
        [LifecycleEvent::Callback { .. }, LifecycleEvent::Render(_)]
    ));
}

fn pinnacle_info() -> BossInfo {
    BossInfo {
        boss_id: "forgotten_king".to_owned(),
        name: "The Last Breath of Kings".to_owned(),
        dialogue: "Last breath.".to_owned(),
        is_pinnacle: true,
        phase: 3,
        ..BossInfo::default()
    }
}

fn secret_definition() -> SecretPhaseDefinition {
    SecretPhaseDefinition {
        title: "The King Behind the Crown".to_owned(),
        dialogue: vec![
            "The crown remembers.".to_owned(),
            "No court remains.".to_owned(),
        ],
    }
}

#[test]
fn test_fight_ignores_double_click() {
    let mut encounter = view(BossInfo::default(), false);
    let mut service = FakeService::default();
    let mut transport = RecordingTransport::default();
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::WagerModal
    );
    assert_eq!(encounter.state(), ViewState::ModalOpen);
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::Duplicate
    );
    assert_eq!(
        transport.responses,
        vec![
            ResponseEvent::Modal(ModalKind::Wager),
            ResponseEvent::Deferred {
                ephemeral: false,
                thinking: false
            }
        ]
    );
    transport.defer_fails = true;
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::Duplicate
    );
}

#[test]
fn test_fight_uses_risk_modal_when_wagers_are_disabled() {
    let mut encounter = view(
        BossInfo {
            wager_allowed: false,
            ..BossInfo::default()
        },
        false,
    );
    let mut service = FakeService::default();
    let mut transport = RecordingTransport::default();
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::RiskModal
    );
    assert_eq!(service.carried_calls, 0);
    assert_eq!(
        transport.responses,
        vec![ResponseEvent::Modal(ModalKind::RiskOnly)]
    );
    assert_eq!(encounter.state(), ViewState::ModalOpen);
}

#[test]
fn test_risk_modal_submit_reports_unexpected_resolution_failure() {
    let mut encounter = view(
        BossInfo {
            wager_allowed: false,
            ..BossInfo::default()
        },
        false,
    );
    encounter.state = ViewState::ModalOpen;
    let modal = BossRiskModal {
        callback_attached: false,
    };
    let mut service = FakeService {
        starts: VecDeque::from([Err(ServiceError("boom".to_owned()))]),
        ..FakeService::default()
    };
    let mut transport = RecordingTransport::default();
    assert_eq!(
        modal.submit(&mut encounter, "cautious", &mut service, &mut transport),
        Ok(())
    );
    assert_eq!(encounter.state(), ViewState::Stopped);
    assert!(transport.responses.contains(&ResponseEvent::Deferred {
        ephemeral: false,
        thinking: true
    }));
    assert!(transport.responses.contains(&ResponseEvent::Followup {
        content: "Boss fight failed. Try again.".to_owned(),
        ephemeral: true
    }));
}

#[test]
fn test_encounter_forwards_callback_to_every_fight_entry() {
    let mut wager = view(BossInfo::default(), true);
    let mut risk = view(
        BossInfo {
            wager_allowed: false,
            ..BossInfo::default()
        },
        true,
    );
    let mut carried = view(BossInfo::default(), true);
    let pending = BossResult {
        pending_prompt: Some(prompt()),
        ..BossResult::default()
    };
    let mut wager_service = FakeService::default();
    let mut risk_service = FakeService::default();
    let mut carried_service = FakeService {
        carried: Some(Ok(Some(CarriedWager {
            risk_tier: RiskTier::Bold,
            amount: 12,
        }))),
        starts: VecDeque::from([Ok(pending)]),
        ..FakeService::default()
    };
    let mut transport = RecordingTransport::default();
    assert_eq!(
        wager.fight(42, &mut wager_service, &mut transport),
        FightEntry::WagerModal
    );
    assert!(
        BossWagerModal {
            callback_attached: wager.callback_attached
        }
        .callback_attached
    );
    assert_eq!(
        risk.fight(42, &mut risk_service, &mut transport),
        FightEntry::RiskModal
    );
    assert!(
        BossRiskModal {
            callback_attached: risk.callback_attached
        }
        .callback_attached
    );
    assert_eq!(
        carried.fight(42, &mut carried_service, &mut transport),
        FightEntry::CarriedResolution
    );
    assert!(matches!(
        transport.lifecycle.last(),
        Some(LifecycleEvent::Render(RenderPlan::Prompt {
            callback_attached: true,
            ..
        }))
    ));
}

#[test]
fn test_phase_transition_forwards_callback_to_replacement_encounter() {
    let mut result = shaped_result("regular-phase2");
    result.phase2_name = Some("Next Form".to_owned());
    result.phase2_title = Some("Transformed".to_owned());
    result.dialogue = Some("Again.".to_owned());
    result.gear_broken = vec!["Stone Cuirass".to_owned()];
    result.round_log = vec![RoundEntry {
        pet_assist: Some(pet("common_cama", "Dane", "Common Cama", 10)),
        pet_assist_damage: 2,
    }];
    let next = BossInfo {
        name: "Next Form".to_owned(),
        dialogue: "Again.".to_owned(),
        ..BossInfo::default()
    };
    let plan = build_phase_transition(
        &result,
        next,
        EncounterPresentation {
            name: "Next Form".to_owned(),
            dialogue: "Again.".to_owned(),
            secret: false,
        },
        "Dirt",
        true,
    );
    assert!(plan.callback_attached);
    assert!(
        plan.transition
            .field("Gear Broken")
            .unwrap()
            .contains("Stone Cuirass")
    );
    assert!(
        plan.transition
            .field("Pet Assist")
            .unwrap()
            .contains("2 bonus damage")
    );
}

#[test]
fn test_phase_transition_encounter_surfaces_carried_wager() {
    let result = BossResult {
        phase2_incoming: true,
        phase2_name: Some("Next Form".to_owned()),
        phase2_title: Some("Transformed".to_owned()),
        dialogue: Some("Again.".to_owned()),
        wager: 1_500,
        ..BossResult::default()
    };
    let next = BossInfo {
        name: "Next Form".to_owned(),
        carried_wager: Some(1_500),
        ..BossInfo::default()
    };
    let plan = build_phase_transition(
        &result,
        next,
        EncounterPresentation {
            name: "Next Form".to_owned(),
            dialogue: "Again.".to_owned(),
            secret: false,
        },
        "Dirt",
        false,
    );
    assert!(
        plan.encounter
            .field("Carried Wager")
            .unwrap()
            .contains("**1,500**")
    );
}

#[test]
fn test_pinnacle_phase_transition_uses_next_phase_title() {
    let result = BossResult {
        phase3_incoming: true,
        next_phase_title: Some("The Digger Eternal".to_owned()),
        dialogue: Some("Eternal. The tunnel is me. I am the tunnel.".to_owned()),
        boss_name: "The Digger Unbound".to_owned(),
        ..BossResult::default()
    };
    let plan = build_phase_transition(
        &result,
        BossInfo::default(),
        EncounterPresentation {
            name: "The Digger Eternal".to_owned(),
            dialogue: "Last shift. Last dig. Last.".to_owned(),
            secret: false,
        },
        "Dirt",
        false,
    );
    assert_eq!(plan.transition.title, "The Digger Eternal Emerges!");
    assert!(
        plan.transition
            .description
            .contains("**The Digger Eternal**")
    );
    assert!(!plan.transition.description.contains("???"));
}

#[test]
fn test_pinnacle_transition_renders_event_text_and_attaches_phase_art_to_encounter() {
    let result = BossResult {
        phase2_incoming: true,
        is_pinnacle: true,
        next_phase_title: Some("The Crowned Hunger".to_owned()),
        dialogue: Some("The crown burns.".to_owned()),
        boss_name: "The Forgotten King".to_owned(),
        boss_id: Some("forgotten_king".to_owned()),
        phase_event_flavor: Some("The old crown catches fire.".to_owned()),
        phase_event_description: Some("Ash falls upward through the chamber.".to_owned()),
        ..BossResult::default()
    };
    let next = BossInfo {
        boss_id: "forgotten_king".to_owned(),
        name: "The Crowned Hunger".to_owned(),
        dialogue: "The crown burns.".to_owned(),
        is_pinnacle: true,
        phase: 2,
        ..BossInfo::default()
    };
    let plan = build_phase_transition(
        &result,
        next,
        EncounterPresentation {
            name: "The Crowned Hunger".to_owned(),
            dialogue: "The crown burns.".to_owned(),
            secret: false,
        },
        "The Hollow",
        false,
    );
    let event = plan.transition.field("Phase shift").unwrap();
    assert!(event.contains("The old crown catches fire."));
    assert!(event.contains("Ash falls upward through the chamber."));
    assert_eq!(
        plan.art,
        Some(ArtRequest {
            boss_id: "forgotten_king".to_owned(),
            phase: 2,
            layer_name: "The Hollow".to_owned(),
            secret: false
        })
    );
    assert_eq!(
        plan.encounter.image.as_deref(),
        Some("attachment://pinnacle_phase_2.png")
    );
}

#[test]
fn test_regular_transition_does_not_render_pinnacle_event_presentation() {
    let result = BossResult {
        phase2_incoming: true,
        phase2_name: Some("Regular Second Form".to_owned()),
        phase2_title: Some("Transformed".to_owned()),
        dialogue: Some("Again.".to_owned()),
        boss_name: "Regular Boss".to_owned(),
        phase_event_flavor: Some("Pinnacle-only flavor.".to_owned()),
        phase_event_description: Some("Pinnacle-only description.".to_owned()),
        ..BossResult::default()
    };
    let plan = build_phase_transition(
        &result,
        BossInfo::default(),
        EncounterPresentation {
            name: "Regular Second Form".to_owned(),
            dialogue: "Again.".to_owned(),
            secret: false,
        },
        "Dirt",
        false,
    );
    assert_eq!(plan.transition.title, "Transformed Emerges!");
    assert!(
        plan.transition
            .description
            .contains("**Regular Second Form**")
    );
    assert!(plan.transition.field("Phase shift").is_none());
    assert!(!plan.transition.description.contains("Pinnacle-only"));
}

#[test]
fn test_phase3_secret_roll_changes_only_presentation_secret() {
    let info = pinnacle_info();
    let display = resolve_encounter_presentation(
        &info,
        3,
        "42:forgotten_king:3:one",
        0.10,
        Some(&secret_definition()),
        Some(0.05),
    );
    assert_eq!(display.name, "The King Behind the Crown");
    assert!(secret_definition().dialogue.contains(&display.dialogue));
    assert!(display.secret);
    assert_eq!(info.name, "The Last Breath of Kings");
    assert_eq!(info.dialogue, "Last breath.");
}

#[test]
fn test_phase3_secret_roll_changes_only_presentation_normal() {
    let info = pinnacle_info();
    let display = resolve_encounter_presentation(
        &info,
        3,
        "42:forgotten_king:3:one",
        0.10,
        Some(&secret_definition()),
        Some(0.50),
    );
    assert_eq!(display.name, "The Last Breath of Kings");
    assert_eq!(display.dialogue, "Last breath.");
    assert!(!display.secret);
    assert_eq!(info.name, "The Last Breath of Kings");
}

#[test]
fn test_duel_prompt_surfaces_stale_cleanup_break_notification() {
    let result = BossResult {
        pending_prompt: Some(prompt()),
        gear_broken: vec!["Stone Cuirass".to_owned()],
        ..BossResult::default()
    };
    assert!(
        build_duel_prompt_embed(&result)
            .field("Gear Broken")
            .unwrap()
            .contains("Stone Cuirass")
    );
}

#[test]
fn test_duel_prompt_surfaces_active_pet_assist() {
    let result = BossResult {
        pending_prompt: Some(prompt()),
        pet_assist: Some(pet("common_cama", "Dane", "Common Cama", 12)),
        ..BossResult::default()
    };
    let embed = build_duel_prompt_embed(&result);
    let field = embed.field("Pet Assist").unwrap();
    assert!(field.contains("Dane"));
    assert!(field.contains("Common Cama"));
    assert!(field.contains("12%"));
    assert!(field.contains("barrels into the fray"));
}

#[test]
fn test_boss_result_reports_species_flavor_and_pet_damage() {
    let assist = pet("rama", "Rama", "Rama the First", 10);
    let result = BossResult {
        round_log: vec![
            RoundEntry {
                pet_assist: Some(assist.clone()),
                pet_assist_damage: 1,
            },
            RoundEntry {
                pet_assist: Some(assist),
                pet_assist_damage: 1,
            },
        ],
        ..BossResult::default()
    };
    let embed = build_boss_result_embed(&result);
    let field = embed.field("Pet Assist").unwrap();
    assert!(field.contains("Rama"));
    assert!(field.contains("historic contempt"));
    assert!(field.contains("2 bonus damage"));
}

#[test]
fn test_boss_result_reports_when_pet_found_no_opening() {
    let result = BossResult {
        round_log: vec![RoundEntry {
            pet_assist: Some(pet("common_cama", "Dane", "Common Cama", 12)),
            pet_assist_damage: 0,
        }],
        ..BossResult::default()
    };
    assert!(
        build_boss_result_embed(&result)
            .field("Pet Assist")
            .unwrap()
            .contains("No opening appeared this time")
    );
}

#[test]
fn test_no_wager_resolution_surfaces_break_notification() {
    let result = BossResult {
        gear_broken: vec!["Stone Cuirass".to_owned()],
        ..BossResult::default()
    };
    let mut transport = RecordingTransport::default();
    route_resolution(
        ResolutionOrigin::Carried,
        result,
        42,
        Some(7),
        false,
        &mut transport,
    );
    let LifecycleEvent::Render(RenderPlan::Final(embed)) = &transport.lifecycle[0] else {
        panic!("expected final render");
    };
    assert!(
        embed
            .field("Gear Broken")
            .unwrap()
            .contains("Stone Cuirass")
    );
}

#[test]
fn test_wager_modal_resolution_surfaces_break_notification() {
    let result = BossResult {
        gear_broken: vec!["Stone Cuirass".to_owned()],
        ..BossResult::default()
    };
    let mut encounter = view(BossInfo::default(), false);
    encounter.state = ViewState::ModalOpen;
    let modal = BossWagerModal {
        callback_attached: false,
    };
    let mut service = FakeService::with_start(result);
    let mut transport = RecordingTransport::default();
    modal
        .submit(&mut encounter, "bold", "12", &mut service, &mut transport)
        .unwrap();
    let LifecycleEvent::Render(RenderPlan::Final(embed)) = transport.lifecycle.last().unwrap()
    else {
        panic!("expected final render");
    };
    assert!(
        embed
            .field("Gear Broken")
            .unwrap()
            .contains("Stone Cuirass")
    );
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_regular_win() {
    assert_callback_before_render(ResolutionOrigin::Carried, "regular-win");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_regular_loss() {
    assert_callback_before_render(ResolutionOrigin::Carried, "regular-loss");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_regular_phase2() {
    assert_callback_before_render(ResolutionOrigin::Carried, "regular-phase2");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_regular_phase3() {
    assert_callback_before_render(ResolutionOrigin::Carried, "regular-phase3");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_pinnacle_win() {
    assert_callback_before_render(ResolutionOrigin::Carried, "pinnacle-win");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_pinnacle_loss() {
    assert_callback_before_render(ResolutionOrigin::Carried, "pinnacle-loss");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_pinnacle_phase2() {
    assert_callback_before_render(ResolutionOrigin::Carried, "pinnacle-phase2");
}

#[test]
fn test_carried_and_no_wager_start_shapes_notify_once_pinnacle_phase3() {
    assert_callback_before_render(ResolutionOrigin::Carried, "pinnacle-phase3");
}

#[test]
fn test_modal_start_shapes_notify_before_render_regular_win() {
    assert_callback_before_render(ResolutionOrigin::Modal, "regular-win");
}

#[test]
fn test_modal_start_shapes_notify_before_render_regular_loss() {
    assert_callback_before_render(ResolutionOrigin::Modal, "regular-loss");
}

#[test]
fn test_modal_start_shapes_notify_before_render_regular_phase2() {
    assert_callback_before_render(ResolutionOrigin::Modal, "regular-phase2");
}

#[test]
fn test_modal_start_shapes_notify_before_render_regular_phase3() {
    assert_callback_before_render(ResolutionOrigin::Modal, "regular-phase3");
}

#[test]
fn test_modal_start_shapes_notify_before_render_pinnacle_win() {
    assert_callback_before_render(ResolutionOrigin::Modal, "pinnacle-win");
}

#[test]
fn test_modal_start_shapes_notify_before_render_pinnacle_loss() {
    assert_callback_before_render(ResolutionOrigin::Modal, "pinnacle-loss");
}

#[test]
fn test_modal_start_shapes_notify_before_render_pinnacle_phase2() {
    assert_callback_before_render(ResolutionOrigin::Modal, "pinnacle-phase2");
}

#[test]
fn test_modal_start_shapes_notify_before_render_pinnacle_phase3() {
    assert_callback_before_render(ResolutionOrigin::Modal, "pinnacle-phase3");
}

#[test]
fn test_wager_pending_and_failed_results_do_not_notify() {
    let pending = BossResult {
        pending_prompt: Some(prompt()),
        ..BossResult::default()
    };
    let failed = BossResult {
        success: false,
        error: Some("No fight.".to_owned()),
        ..BossResult::default()
    };
    let mut transport = RecordingTransport::default();
    route_resolution(
        ResolutionOrigin::Modal,
        pending,
        42,
        Some(7),
        true,
        &mut transport,
    );
    route_resolution(
        ResolutionOrigin::Modal,
        failed,
        42,
        Some(7),
        true,
        &mut transport,
    );
    assert!(
        !transport
            .lifecycle
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Callback { .. }))
    );
    assert!(matches!(
        &transport.lifecycle[0],
        LifecycleEvent::Render(RenderPlan::Prompt {
            callback_attached: true,
            ..
        })
    ));
    assert_eq!(
        transport.lifecycle[1],
        LifecycleEvent::Render(RenderPlan::Error("No fight.".to_owned()))
    );
}

#[test]
fn test_wager_validation_errors_do_not_notify() {
    let cases = [
        ("invalid", "12", ModalValidationError::InvalidRisk),
        ("bold", "not-a-number", ModalValidationError::InvalidWager),
        ("bold", "-1", ModalValidationError::NegativeWager),
    ];
    let mut service = FakeService::default();
    for (risk, wager, expected) in cases {
        let mut encounter = view(BossInfo::default(), true);
        encounter.state = ViewState::ModalOpen;
        let modal = BossWagerModal {
            callback_attached: true,
        };
        let mut transport = RecordingTransport::default();
        assert_eq!(
            modal.submit(&mut encounter, risk, wager, &mut service, &mut transport),
            Err(expected)
        );
        assert_eq!(encounter.state(), ViewState::Ready);
        assert!(transport.lifecycle.is_empty());
    }
    assert!(service.start_calls.is_empty());
}

#[test]
fn test_wager_modal_surfaces_maximum() {
    assert_eq!(BOSS_WAGER_MAX_JC, 1_000);
    assert_eq!(BOSS_WAGER_LABEL, "Wager Amount (max 1,000 JC)");
    assert_eq!(BOSS_WAGER_PLACEHOLDER, "0-1000");
}

#[test]
fn test_risk_modal_forwards_callback_to_no_wager_resolution() {
    let mut encounter = view(
        BossInfo {
            wager_allowed: false,
            ..BossInfo::default()
        },
        true,
    );
    encounter.state = ViewState::ModalOpen;
    let modal = BossRiskModal {
        callback_attached: true,
    };
    let mut service = FakeService::with_start(BossResult::default());
    let mut transport = RecordingTransport::default();
    modal
        .submit(&mut encounter, "reckless", &mut service, &mut transport)
        .unwrap();
    assert_eq!(service.start_calls[0], (42, Some(7), RiskTier::Reckless, 0));
    assert!(matches!(
        transport.lifecycle.first(),
        Some(LifecycleEvent::Callback { .. })
    ));
    assert_eq!(encounter.state(), ViewState::Stopped);
}

#[test]
fn test_pending_carried_fight_propagates_without_notifying() {
    let mut encounter = view(BossInfo::default(), true);
    let mut service = FakeService {
        carried: Some(Ok(Some(CarriedWager {
            risk_tier: RiskTier::Bold,
            amount: 12,
        }))),
        starts: VecDeque::from([Ok(BossResult {
            pending_prompt: Some(prompt()),
            ..BossResult::default()
        })]),
        ..FakeService::default()
    };
    let mut transport = RecordingTransport::default();
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::CarriedResolution
    );
    assert!(
        !transport
            .lifecycle
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Callback { .. }))
    );
    assert!(matches!(
        transport.lifecycle.last(),
        Some(LifecycleEvent::Render(RenderPlan::Prompt {
            callback_attached: true,
            ..
        }))
    ));
}

#[test]
fn test_resume_shapes_notify_once_before_render_regular_win() {
    assert_resume_shape("regular-win");
}

#[test]
fn test_resume_shapes_notify_once_before_render_regular_loss() {
    assert_resume_shape("regular-loss");
}

#[test]
fn test_resume_shapes_notify_once_before_render_regular_phase2() {
    assert_resume_shape("regular-phase2");
}

#[test]
fn test_resume_shapes_notify_once_before_render_regular_phase3() {
    assert_resume_shape("regular-phase3");
}

#[test]
fn test_resume_shapes_notify_once_before_render_pinnacle_win() {
    assert_resume_shape("pinnacle-win");
}

#[test]
fn test_resume_shapes_notify_once_before_render_pinnacle_loss() {
    assert_resume_shape("pinnacle-loss");
}

#[test]
fn test_resume_shapes_notify_once_before_render_pinnacle_phase2() {
    assert_resume_shape("pinnacle-phase2");
}

#[test]
fn test_resume_shapes_notify_once_before_render_pinnacle_phase3() {
    assert_resume_shape("pinnacle-phase3");
}

#[test]
fn test_prompted_pinnacle_phase_clear_posts_next_encounter_phase2_incoming() {
    let mut result = shaped_result("pinnacle-phase2");
    result.boss_name = "The Digger Unbound".to_owned();
    let mut view = BossDuelView::new(42, Some(7), &prompt(), false);
    let mut service = FakeService::with_resume(result);
    let mut transport = RecordingTransport::default();
    view.submit(1, &mut service, &mut transport);
    let LifecycleEvent::Render(RenderPlan::PhaseCleared { embed, .. }) =
        transport.lifecycle.last().unwrap()
    else {
        panic!("expected phase-clear render");
    };
    assert_eq!(embed.title, "Phase Cleared");
}

#[test]
fn test_prompted_pinnacle_phase_clear_posts_next_encounter_phase3_incoming() {
    let mut result = shaped_result("pinnacle-phase3");
    result.boss_name = "The Digger Unbound".to_owned();
    let mut view = BossDuelView::new(42, Some(7), &prompt(), false);
    let mut service = FakeService::with_resume(result);
    let mut transport = RecordingTransport::default();
    view.submit(1, &mut service, &mut transport);
    let LifecycleEvent::Render(RenderPlan::PhaseCleared { embed, .. }) =
        transport.lifecycle.last().unwrap()
    else {
        panic!("expected phase-clear render");
    };
    assert_eq!(embed.title, "Phase Cleared");
    let render = transport.lifecycle.last().unwrap();
    let LifecycleEvent::Render(plan) = render else {
        panic!("expected render plan");
    };
    let mut message = FailingEditable::default();
    assert!(edit_render_with_fallback(Some(&mut message), None, plan).unwrap());
    assert_eq!(message.edit_attempts.len(), 1);
    assert_eq!(message.channel_sends.len(), 1);
    assert!(!message.channel_sends[0].view);
}

#[test]
fn test_resume_pending_replaces_view_without_notifying() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService::with_resume(BossResult {
        pending_prompt: Some(prompt()),
        ..BossResult::default()
    });
    let mut transport = RecordingTransport::default();
    view.submit(1, &mut service, &mut transport);
    assert_eq!(transport.lifecycle.len(), 1);
    assert!(matches!(
        &transport.lifecycle[0],
        LifecycleEvent::Render(RenderPlan::Prompt {
            callback_attached: true,
            replacement: true,
            ..
        })
    ));
}

#[test]
fn test_button_and_timeout_resume_notify_button() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService::with_resume(BossResult::default());
    let mut transport = RecordingTransport::default();
    assert_eq!(
        view.click(42, "duel_opt_1", &mut service, &mut transport),
        SubmitOutcome::Resolved
    );
    assert_eq!(
        DuelOptionId::parse("duel_opt_1").unwrap().to_string(),
        "duel_opt_1"
    );
    assert_eq!(service.resume_calls, vec![(42, Some(7), 1)]);
    assert!(matches!(
        transport.lifecycle.first(),
        Some(LifecycleEvent::Callback { .. })
    ));
}

#[test]
fn test_button_and_timeout_resume_notify_timeout() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService::with_resume(BossResult::default());
    let mut transport = RecordingTransport::default();
    assert_eq!(
        view.on_timeout(&mut service, &mut transport),
        SubmitOutcome::Resolved
    );
    assert_eq!(service.resume_calls, vec![(42, Some(7), 2)]);
    assert!(matches!(
        transport.lifecycle.first(),
        Some(LifecycleEvent::Callback { .. })
    ));
}

#[test]
fn test_callback_failure_is_logged_and_result_still_renders() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService::with_resume(BossResult::default());
    let mut transport = RecordingTransport {
        callback_fails: true,
        ..RecordingTransport::default()
    };
    view.submit(1, &mut service, &mut transport);
    assert!(transport.lifecycle.iter().any(|event| matches!(
        event,
        LifecycleEvent::Warning(message)
            if message.contains("Boss resolved callback failed for user 42 in guild 7")
    )));
    assert!(
        transport
            .lifecycle
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Render(RenderPlan::Final(_))))
    );
}

#[test]
fn test_resume_failure_does_not_notify_service_result() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService::with_resume(BossResult {
        success: false,
        error: Some("No.".to_owned()),
        ..BossResult::default()
    });
    let mut transport = RecordingTransport::default();
    view.submit(1, &mut service, &mut transport);
    assert_eq!(
        transport.lifecycle,
        vec![LifecycleEvent::Render(RenderPlan::Error("No.".to_owned()))]
    );
}

#[test]
fn test_resume_failure_does_not_notify_exception() {
    let mut view = BossDuelView::new(42, Some(7), &prompt(), true);
    let mut service = FakeService {
        resumes: VecDeque::from([Err(ServiceError("boom".to_owned()))]),
        ..FakeService::default()
    };
    let mut transport = RecordingTransport::default();
    assert_eq!(
        view.submit(1, &mut service, &mut transport),
        SubmitOutcome::ServiceFailed
    );
    assert!(
        !transport
            .lifecycle
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Callback { .. }))
    );
    assert!(
        transport
            .lifecycle
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Render(RenderPlan::Error(_))))
    );
}

#[test]
fn test_unauthorized_duplicate_and_nonfight_actions_do_not_notify() {
    let mut encounter = view(BossInfo::default(), true);
    let mut service = FakeService::default();
    let mut transport = RecordingTransport::default();
    assert_eq!(
        encounter.fight(99, &mut service, &mut transport),
        FightEntry::Unauthorized
    );
    assert_eq!(service.carried_calls, 0);
    encounter.state = ViewState::ModalOpen;
    assert_eq!(
        encounter.fight(42, &mut service, &mut transport),
        FightEntry::Duplicate
    );
    encounter.release_modal();
    assert!(encounter.nonfight(42, NonFightAction::Retreat, &mut transport));
    assert!(encounter.nonfight(42, NonFightAction::Scout, &mut transport));
    assert!(encounter.nonfight(99, NonFightAction::Cheer, &mut transport));
    assert!(transport.lifecycle.is_empty());

    let mut duel = BossDuelView::new(42, Some(7), &prompt(), true);
    assert_eq!(
        duel.click(99, "duel_opt_1", &mut service, &mut transport),
        SubmitOutcome::Unauthorized
    );
    assert_eq!(
        duel.click(42, "duel_opt_1:stale", &mut service, &mut transport),
        SubmitOutcome::Malformed
    );
    assert!(service.resume_calls.is_empty());
}

#[test]
fn test_pinnacle_secret_phase_is_stable_for_one_encounter() {
    let info = pinnacle_info();
    let secret = secret_definition();
    let renders = (0..12)
        .map(|_| {
            resolve_encounter_presentation(
                &info,
                3,
                "4242:forgotten_king:3:1000000",
                0.5,
                Some(&secret),
                None,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(renders.len(), 1);
    let per_player = (0..40)
        .map(|user| {
            resolve_encounter_presentation(
                &info,
                3,
                &format!("{user}:forgotten_king:3:1000000"),
                0.5,
                Some(&secret),
                None,
            )
            .secret
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(per_player, BTreeSet::from([false, true]));
    let per_encounter = (1_000_000..1_000_060)
        .map(|engaged_at| {
            resolve_encounter_presentation(
                &info,
                3,
                &format!("4242:forgotten_king:3:{engaged_at}"),
                0.5,
                Some(&secret),
                None,
            )
            .secret
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(per_encounter, BTreeSet::from([false, true]));
}

#[test]
fn test_every_modal_rejection_path_releases_the_encounter_view() {
    let wager_cases = [
        ("invalid", "12", ModalValidationError::InvalidRisk),
        ("bold", "not-a-number", ModalValidationError::InvalidWager),
        ("bold", "-1", ModalValidationError::NegativeWager),
    ];
    let mut service = FakeService::default();
    for (risk, wager, expected) in wager_cases {
        let mut encounter = view(BossInfo::default(), false);
        encounter.state = ViewState::ModalOpen;
        let mut transport = RecordingTransport::default();
        let result = BossWagerModal {
            callback_attached: false,
        }
        .submit(&mut encounter, risk, wager, &mut service, &mut transport);
        assert_eq!(result, Err(expected));
        assert_eq!(encounter.state(), ViewState::Ready);
    }

    let mut encounter = view(BossInfo::default(), false);
    encounter.state = ViewState::ModalOpen;
    let mut transport = RecordingTransport::default();
    assert_eq!(
        BossRiskModal {
            callback_attached: false
        }
        .submit(&mut encounter, "invalid", &mut service, &mut transport),
        Err(ModalValidationError::InvalidRisk)
    );
    assert_eq!(encounter.state(), ViewState::Ready);
    encounter.state = ViewState::ModalOpen;
    BossRiskModal::on_timeout(&mut encounter);
    assert_eq!(encounter.state(), ViewState::Ready);
    encounter.state = ViewState::ModalOpen;
    BossWagerModal::on_timeout(&mut encounter);
    assert_eq!(encounter.state(), ViewState::Ready);

    let mut rejected = view(BossInfo::default(), false);
    let mut transport = RecordingTransport {
        modal_fails: true,
        ..RecordingTransport::default()
    };
    assert_eq!(
        rejected.fight(42, &mut service, &mut transport),
        FightEntry::TransportRejected
    );
    assert_eq!(rejected.state(), ViewState::Ready);
}
